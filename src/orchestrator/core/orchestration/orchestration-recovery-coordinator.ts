import { t } from '../../../i18n';
import { logger, LogCategory } from '../../../logging';
import { hasPhaseContinuationPending } from '../phase-continuation';
import { decideDeliveryRecovery, decideGovernanceRecovery } from '../recovery-decision-kernel';
import { decideContinuationAction } from '../continuation-decision-kernel';
import type { MessageHub } from '../message/message-hub';
import type { EffectiveModeResolution } from '../effective-mode-resolver';
import { publishInternalControlNotice } from '../internal-control-notice';
import type {
  ResolvedOrchestratorTerminationReason,
  RuntimeTerminationSnapshot,
} from './orchestration-control-plane-types';
import type { RequirementAnalysis } from '../../protocols/types';
import type { MissionStorageManager } from '../../mission';
import { PlanLedgerService, type PlanRuntimePhaseState } from '../../plan-ledger';
import type { DeliveryRoundState } from './orchestration-delivery-controller';

export interface RecoveryLoopState {
  autoRepairAttempt: number;
  autoContinuationAttempt: number;
  lastAutoRepairSignature: string;
  autoRepairStallStreak: number;
  governanceRecoveryAttempt: number;
}

export interface RecoveryCoordinationInput {
  sessionId: string;
  prompt: string;
  finalContent: string;
  executionWarnings: string[];
  executionErrors?: string[];
  runtimeReason: ResolvedOrchestratorTerminationReason;
  runtimeSnapshot?: RuntimeTerminationSnapshot;
  runtimeNextSteps?: string[];
  effectiveMode: EffectiveModeResolution;
  requirementAnalysis: RequirementAnalysis;
  deliveryState: DeliveryRoundState;
  state: RecoveryLoopState;
}

export type RecoveryCoordinationResult =
  | {
    action: 'continue';
    state: RecoveryLoopState;
    nextPrompt: string;
    nextRequestId: string;
  }
  | {
    action: 'finalize';
    state: RecoveryLoopState;
    finalContent: string;
    finalExecutionStatus: 'completed' | 'failed' | 'cancelled' | 'paused';
    executionErrors: string[];
  };

interface RecoveryCoordinatorHelperBag {
  getCurrentPlanId: () => string | null;
  getLastMissionId: () => string | null;
  setActiveRoundRequestId: (requestId: string) => void;
  normalizeOrchestratorRuntimeReason: (
    runtimeReason?: string,
  ) => ResolvedOrchestratorTerminationReason | undefined;
  resolveExecutionFinalStatus: (
    runtimeReason?: ResolvedOrchestratorTerminationReason,
    runtimeSnapshot?: RuntimeTerminationSnapshot,
  ) => 'completed' | 'failed' | 'cancelled' | 'paused';
  isGovernancePauseReason: (reason: ResolvedOrchestratorTerminationReason) => boolean;
  resolveRequiredTotal: (snapshot?: RuntimeTerminationSnapshot) => number | undefined;
  resolveTerminalRequired: (snapshot?: RuntimeTerminationSnapshot) => number | undefined;
  extractPendingRequiredCount: (snapshot?: RuntimeTerminationSnapshot) => number;
  buildFollowUpProgressSignature: (snapshot?: RuntimeTerminationSnapshot) => string;
  buildAutoRepairPrompt: (input: {
    originalPrompt: string;
    goal: string;
    constraints: string[];
    acceptanceCriteria: string[];
    deliverySummary?: string;
    deliveryDetails?: string;
    round: number;
    maxRounds: number;
  }) => string;
  buildGovernanceRecoveryPrompt: (input: {
    originalPrompt: string;
    goal: string;
    constraints: string[];
    acceptanceCriteria: string[];
    reason?: ResolvedOrchestratorTerminationReason;
    round: number;
    maxRounds: number;
  }) => string;
  resolveFollowUpSteps: (runtimeSteps?: string[]) => string[];
  extractStructuredContinuationStepsFromContent: (content: string) => string[];
  classifyFollowUpSteps: (steps: string[]) => {
    actionable: string[];
    blocked: string[];
    nonActionable: string[];
  };
  buildFollowUpBlockedNotice: (steps: string[]) => string;
  buildPhaseRuntimePatch: (input: {
    current?: PlanRuntimePhaseState | null;
    runtimeReason?: ResolvedOrchestratorTerminationReason;
    pendingRequiredTodos: number;
    followUpSteps: string[];
  }) => Partial<PlanRuntimePhaseState> | null;
  resolvePhaseRuntimeForDecision: (
    current?: PlanRuntimePhaseState | null,
    patch?: Partial<PlanRuntimePhaseState> | null,
  ) => PlanRuntimePhaseState | null;
  stripNonActionableFollowUpSection: (content: string) => string;
  markPhaseRuntimeRunning: (input: {
    sessionId: string;
    followUpSteps: string[];
  }) => Promise<void>;
  beginSyntheticExecutionRound: (input: {
    kind: 'auto_continuation' | 'auto_repair' | 'auto_governance_resume';
    round: number;
    message: string;
  }) => string;
  buildAutoFollowUpPrompt: (input: {
    originalPrompt: string;
    goal: string;
    constraints: string[];
    acceptanceCriteria: string[];
    steps: string[];
    round: number;
    requiredTotal?: number;
    terminalRequired?: number;
    pendingRequired?: number;
  }) => string;
  buildGovernancePauseReport: (input: {
    reason?: ResolvedOrchestratorTerminationReason;
    snapshot?: RuntimeTerminationSnapshot;
    recoveryAttempted: number;
    recoveryMaxRounds: number;
  }) => string;
  formatGovernanceReason: (reason?: ResolvedOrchestratorTerminationReason) => string;
  buildExecutionFailureMessages: (
    runtimeReason: ResolvedOrchestratorTerminationReason,
    executionErrors: string[],
  ) => string[];
}

export interface OrchestrationRecoveryCoordinatorDependencies {
  messageHub: MessageHub;
  missionStorage: MissionStorageManager;
  planLedger: PlanLedgerService;
  getAutoRepairMaxRounds: () => number | undefined;
  helpers: RecoveryCoordinatorHelperBag;
}

export class OrchestrationRecoveryCoordinator {
  private static readonly AUTO_REPAIR_STALL_THRESHOLD = 3;
  private static readonly GOVERNANCE_RECOVERY_DELAYS = [10000, 20000, 30000, 40000, 50000];

  constructor(
    private readonly deps: OrchestrationRecoveryCoordinatorDependencies,
  ) {}

  async coordinate(input: RecoveryCoordinationInput): Promise<RecoveryCoordinationResult> {
    const autoRepairMaxRounds = this.resolveAutoRepairMaxRounds();
    const autoRepairMaxRoundsLabel = autoRepairMaxRounds > 0
      ? autoRepairMaxRounds
      : t('common.unlimited');
    const governanceRecoveryMaxRounds = OrchestrationRecoveryCoordinator.GOVERNANCE_RECOVERY_DELAYS.length;

    const state: RecoveryLoopState = { ...input.state };
    let finalContent = input.finalContent;
    const finalExecutionStatus = this.deps.helpers.resolveExecutionFinalStatus(
      input.runtimeReason,
      input.runtimeSnapshot,
    );
    const normalizedRuntimeReason = this.deps.helpers.normalizeOrchestratorRuntimeReason(input.runtimeReason);
    const isGovernancePaused = finalExecutionStatus === 'paused'
      && normalizedRuntimeReason
      && this.deps.helpers.isGovernancePauseReason(normalizedRuntimeReason);

    const autoRepairProgressSignature = [
      input.deliveryState.deliverySummaryForMission?.trim() ?? '',
      input.deliveryState.deliveryDetailsForMission?.trim() ?? '',
      this.deps.helpers.buildFollowUpProgressSignature(input.runtimeSnapshot),
    ].filter(Boolean).join('|');
    let autoRepairStalled = false;
    if (input.deliveryState.deliveryStatusForMission === 'failed') {
      if (autoRepairProgressSignature && autoRepairProgressSignature === state.lastAutoRepairSignature) {
        state.autoRepairStallStreak += 1;
      } else if (autoRepairProgressSignature) {
        state.lastAutoRepairSignature = autoRepairProgressSignature;
        state.autoRepairStallStreak = 0;
      } else {
        state.lastAutoRepairSignature = '';
        state.autoRepairStallStreak = 0;
      }
      autoRepairStalled = state.autoRepairStallStreak >= OrchestrationRecoveryCoordinator.AUTO_REPAIR_STALL_THRESHOLD;
    } else {
      state.lastAutoRepairSignature = '';
      state.autoRepairStallStreak = 0;
    }

    const canAutoRepairByRounds = autoRepairMaxRounds === 0 || state.autoRepairAttempt < autoRepairMaxRounds;
    const structuredRequiredTotal = this.deps.helpers.resolveRequiredTotal(input.runtimeSnapshot) ?? 0;
    const hasStructuredExecutionContext = structuredRequiredTotal > 0;
    const shouldFinalizeOrchestrationContractFailure =
      this.isOrchestrationContractFailureWithoutAssignments({
        requirementAnalysis: input.requirementAnalysis,
        finalExecutionStatus,
        hasStructuredExecutionContext,
      });

    const deliveryRecoveryDecision = decideDeliveryRecovery({
      allowAutoGovernanceResume: input.effectiveMode.allowAutoGovernanceResume,
      isGovernancePaused: isGovernancePaused === true,
      governanceReason: normalizedRuntimeReason,
      governanceRecoveryAttempt: state.governanceRecoveryAttempt,
      governanceRecoveryMaxRounds,
      deliveryFailed: input.deliveryState.deliveryStatusForMission === 'failed',
      continuationPolicy: input.deliveryState.continuationPolicyForMission,
      canAutoRepairByRounds: canAutoRepairByRounds && hasStructuredExecutionContext,
      autoRepairStalled,
    });

    if (deliveryRecoveryDecision.action === 'auto_repair') {
      const lastMissionId = this.deps.helpers.getLastMissionId();
      if (lastMissionId) {
        try {
          await this.deps.missionStorage.transitionStatus(lastMissionId, 'executing');
        } catch (error) {
          logger.warn('编排器.Mission.自动修复前状态恢复失败', {
            missionId: lastMissionId,
            to: 'executing',
            error: error instanceof Error ? error.message : String(error),
          }, LogCategory.ORCHESTRATOR);
        }
      }

      const currentPlanId = this.deps.helpers.getCurrentPlanId();
      if (currentPlanId) {
        try {
          await this.deps.planLedger.updateRuntimeState(input.sessionId, currentPlanId, {
            review: { state: 'idle' },
            replan: { state: 'applied', reason: 'auto_repair_triggered' },
          });
        } catch (error) {
          logger.warn('编排器.PlanRuntime.自动修复状态回写失败', {
            sessionId: input.sessionId,
            planId: currentPlanId,
            error: error instanceof Error ? error.message : String(error),
          }, LogCategory.ORCHESTRATOR);
        }
      }

      state.autoRepairAttempt += 1;
      const nextPrompt = this.deps.helpers.buildAutoRepairPrompt({
        originalPrompt: input.prompt,
        goal: input.requirementAnalysis.goal,
        constraints: input.requirementAnalysis.constraints ?? [],
        acceptanceCriteria: input.requirementAnalysis.acceptanceCriteria ?? [],
        deliverySummary: input.deliveryState.deliverySummaryForMission,
        deliveryDetails: input.deliveryState.deliveryDetailsForMission,
        round: state.autoRepairAttempt,
        maxRounds: autoRepairMaxRounds,
      });
      this.deps.messageHub.notify(
        t('engine.delivery.autoRepairScheduled', { round: state.autoRepairAttempt, maxRounds: autoRepairMaxRoundsLabel }),
        'warning',
      );
      const nextRequestId = this.deps.helpers.beginSyntheticExecutionRound({
        kind: 'auto_repair',
        round: state.autoRepairAttempt,
        message: t('engine.delivery.autoRepairProgressMessage', {
          round: state.autoRepairAttempt,
          maxRounds: autoRepairMaxRoundsLabel,
        }),
      });
      this.deps.helpers.setActiveRoundRequestId(nextRequestId);
      return {
        action: 'continue',
        state,
        nextPrompt,
        nextRequestId,
      };
    }

    if (deliveryRecoveryDecision.action === 'auto_repair_stalled_notice') {
      const stalledMessage = t('engine.delivery.autoRepairStalled', {
        streak: state.autoRepairStallStreak,
        threshold: OrchestrationRecoveryCoordinator.AUTO_REPAIR_STALL_THRESHOLD,
      });
      publishInternalControlNotice(this.deps.messageHub, stalledMessage, {
        title: '自动修复停滞',
        level: 'warning',
        category: 'audit',
      });
    }

    if (shouldFinalizeOrchestrationContractFailure) {
      await this.resetPhaseContinuation(input.sessionId);
      logger.warn('编排器.恢复协调.编排契约失败_禁止自动续跑', {
        sessionId: input.sessionId,
        runtimeReason: input.runtimeReason,
      }, LogCategory.ORCHESTRATOR);
      return {
        action: 'finalize',
        state,
        finalContent,
        finalExecutionStatus,
        executionErrors: this.buildFinalExecutionErrors(input),
      };
    }

    const governanceRecoveryDecision = decideGovernanceRecovery({
      allowAutoGovernanceResume: input.effectiveMode.allowAutoGovernanceResume,
      isGovernancePaused: isGovernancePaused === true,
      governanceReason: normalizedRuntimeReason,
      governanceRecoveryAttempt: state.governanceRecoveryAttempt,
      governanceRecoveryMaxRounds,
      continuationPolicy: input.deliveryState.continuationPolicyForMission,
      canAutoRepairByRounds,
      autoRepairStalled,
    });
    if (governanceRecoveryDecision.action === 'auto_governance_resume') {
      state.governanceRecoveryAttempt += 1;
      const delayMs = OrchestrationRecoveryCoordinator.GOVERNANCE_RECOVERY_DELAYS[state.governanceRecoveryAttempt - 1] ?? 0;
      const reasonLabel = this.deps.helpers.formatGovernanceReason(normalizedRuntimeReason);
      const waitSeconds = Math.max(1, Math.round(delayMs / 1000));
      this.deps.messageHub.notify(
        t('engine.governance.autoResumeScheduled', {
          round: state.governanceRecoveryAttempt,
          maxRounds: governanceRecoveryMaxRounds,
          seconds: waitSeconds,
          reason: reasonLabel,
        }),
        'warning',
      );
      if (delayMs > 0) {
        await new Promise((resolve) => setTimeout(resolve, delayMs));
      }
      const nextRequestId = this.deps.helpers.beginSyntheticExecutionRound({
        kind: 'auto_governance_resume',
        round: state.governanceRecoveryAttempt,
        message: t('engine.governance.autoResumeScheduled', {
          round: state.governanceRecoveryAttempt,
          maxRounds: governanceRecoveryMaxRounds,
          seconds: waitSeconds,
          reason: reasonLabel,
        }),
      });
      this.deps.helpers.setActiveRoundRequestId(nextRequestId);
      return {
        action: 'continue',
        state,
        nextRequestId,
        nextPrompt: this.deps.helpers.buildGovernanceRecoveryPrompt({
          originalPrompt: input.prompt,
          goal: input.requirementAnalysis.goal,
          constraints: input.requirementAnalysis.constraints ?? [],
          acceptanceCriteria: input.requirementAnalysis.acceptanceCriteria ?? [],
          reason: normalizedRuntimeReason,
          round: state.governanceRecoveryAttempt,
          maxRounds: governanceRecoveryMaxRounds,
        }),
      };
    }

    // ── 续航信号解析 ──
    // runtimeNextSteps 来自 MISSION_OUTCOME.next_steps（唯一权威续航信号）。
    // undefined = 协议块缺失（内层未输出 MISSION_OUTCOME），此时回退到正则提取。
    // []        = 协议块存在但无后续步骤，直接按"无续航"处理。
    const resolvedFollowUpSteps = this.deps.helpers.resolveFollowUpSteps(input.runtimeNextSteps);
    const hasAuthoritativeOutcome = input.runtimeNextSteps !== undefined;
    // 仅当协议块完全缺失时才回退到正则提取
    const renderedFollowUpSteps = hasAuthoritativeOutcome
      ? []
      : this.deps.helpers.extractStructuredContinuationStepsFromContent(finalContent);
    const structuredFollowUpSteps = resolvedFollowUpSteps.length > 0
      ? resolvedFollowUpSteps
      : renderedFollowUpSteps;
    const {
      actionable: followUpSteps,
      blocked: blockedFollowUpSteps,
      nonActionable: nonActionableFollowUpSteps,
    } = this.deps.helpers.classifyFollowUpSteps(structuredFollowUpSteps);
    const currentPlanIdForFollowUp = this.deps.helpers.getCurrentPlanId();
    const currentPlanForFollowUp = currentPlanIdForFollowUp
      ? this.deps.planLedger.getPlan(input.sessionId, currentPlanIdForFollowUp)
      : null;
    const effectiveFollowUpSteps = followUpSteps;
    const blockedFollowUpOnly = effectiveFollowUpSteps.length === 0 && blockedFollowUpSteps.length > 0;
    const pendingRequiredTodos = this.deps.helpers.extractPendingRequiredCount(input.runtimeSnapshot);

    if (blockedFollowUpOnly) {
      const blockedNotice = this.deps.helpers.buildFollowUpBlockedNotice(blockedFollowUpSteps);
      publishInternalControlNotice(this.deps.messageHub, blockedNotice, {
        title: '后续步骤已阻断',
        level: 'warning',
        category: 'audit',
      });
    }

    const phasePatch = this.deps.helpers.buildPhaseRuntimePatch({
      current: currentPlanForFollowUp?.runtime.phase,
      runtimeReason: normalizedRuntimeReason,
      pendingRequiredTodos,
      followUpSteps: effectiveFollowUpSteps,
    });
    const phaseRuntimeForDecision = this.deps.helpers.resolvePhaseRuntimeForDecision(
      currentPlanForFollowUp?.runtime.phase,
      phasePatch,
    );
    const hasFollowUpPending = hasPhaseContinuationPending(phaseRuntimeForDecision);
    const {
      actionable: renderedActionableFollowUpSteps,
      blocked: renderedBlockedFollowUpSteps,
      nonActionable: renderedNonActionableFollowUpSteps,
    } = this.deps.helpers.classifyFollowUpSteps(renderedFollowUpSteps);

    if (nonActionableFollowUpSteps.length > 0 || renderedNonActionableFollowUpSteps.length > 0) {
      logger.info('编排器.FollowUp.忽略非任务建议', {
        count: Math.max(nonActionableFollowUpSteps.length, renderedNonActionableFollowUpSteps.length),
        examples: (
          nonActionableFollowUpSteps.length > 0
            ? nonActionableFollowUpSteps
            : renderedNonActionableFollowUpSteps
        ).slice(0, 3),
      }, LogCategory.ORCHESTRATOR);
      const hasOnlyRenderedNonActionableFollowUp = renderedNonActionableFollowUpSteps.length > 0
        && renderedActionableFollowUpSteps.length === 0
        && renderedBlockedFollowUpSteps.length === 0;
      if (!hasFollowUpPending && effectiveFollowUpSteps.length === 0 && blockedFollowUpSteps.length === 0 && hasOnlyRenderedNonActionableFollowUp) {
        finalContent = this.deps.helpers.stripNonActionableFollowUpSection(finalContent);
      }
    }

    if (currentPlanIdForFollowUp && phasePatch) {
      try {
        await this.deps.planLedger.updateRuntimeState(input.sessionId, currentPlanIdForFollowUp, {
          phase: phasePatch,
        }, {
          auditReason: 'follow-up-phase-runtime-sync',
        });
      } catch (error) {
        logger.warn('编排器.PlanRuntime.phase回写失败', {
          sessionId: input.sessionId,
          planId: currentPlanIdForFollowUp,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
      }
    }

    const continuationDecision = decideContinuationAction({
      allowDeepContinuation: input.effectiveMode.allowDeepContinuation,
      isGovernancePaused: isGovernancePaused === true,
      phaseRuntime: phaseRuntimeForDecision,
    });
    if (continuationDecision.decision === 'run') {
      const lastMissionId = this.deps.helpers.getLastMissionId();
      if (lastMissionId) {
        try {
          await this.deps.missionStorage.transitionStatus(lastMissionId, 'executing');
        } catch (error) {
          logger.warn('编排器.Mission.自动续跑前状态恢复失败', {
            missionId: lastMissionId,
            to: 'executing',
            error: error instanceof Error ? error.message : String(error),
          }, LogCategory.ORCHESTRATOR);
        }
      }

      state.autoContinuationAttempt += 1;
      await this.deps.helpers.markPhaseRuntimeRunning({
        sessionId: input.sessionId,
        followUpSteps: effectiveFollowUpSteps,
      });
      const nextRequestId = this.deps.helpers.beginSyntheticExecutionRound({
        kind: 'auto_continuation',
        round: state.autoContinuationAttempt,
        message: t('engine.followUp.autoContinueMessage', { round: state.autoContinuationAttempt }),
      });
      this.deps.helpers.setActiveRoundRequestId(nextRequestId);

      if (currentPlanIdForFollowUp) {
        try {
          await this.deps.planLedger.updateRuntimeState(input.sessionId, currentPlanIdForFollowUp, {
            replan: { state: 'applied', reason: 'auto_continuation_triggered' },
          }, {
            auditReason: 'continuation-gate:auto-continuation-applied',
          });
        } catch (error) {
          logger.warn('编排器.PlanRuntime.自动续跑状态回写失败', {
            sessionId: input.sessionId,
            planId: currentPlanIdForFollowUp,
            error: error instanceof Error ? error.message : String(error),
          }, LogCategory.ORCHESTRATOR);
        }
      }

      return {
        action: 'continue',
        state,
        nextRequestId,
        nextPrompt: this.deps.helpers.buildAutoFollowUpPrompt({
          originalPrompt: input.prompt,
          goal: input.requirementAnalysis.goal,
          constraints: input.requirementAnalysis.constraints ?? [],
          acceptanceCriteria: input.requirementAnalysis.acceptanceCriteria ?? [],
          steps: effectiveFollowUpSteps,
          round: state.autoContinuationAttempt,
          requiredTotal: this.deps.helpers.resolveRequiredTotal(input.runtimeSnapshot),
          terminalRequired: this.deps.helpers.resolveTerminalRequired(input.runtimeSnapshot),
          pendingRequired: pendingRequiredTodos,
        }),
      };
    }

    if (continuationDecision.decision === 'pause_for_system') {
      const pauseReport = this.deps.helpers.buildGovernancePauseReport({
        reason: normalizedRuntimeReason,
        snapshot: input.runtimeSnapshot,
        recoveryAttempted: state.governanceRecoveryAttempt,
        recoveryMaxRounds: governanceRecoveryMaxRounds,
      });
      publishInternalControlNotice(this.deps.messageHub, pauseReport, {
        title: '治理暂停',
        level: 'warning',
        category: 'incident',
        actionRequired: true,
        countUnread: true,
      });
    }

    if (state.autoRepairAttempt > 0) {
      publishInternalControlNotice(this.deps.messageHub, `已自动修复 ${state.autoRepairAttempt} 轮（详细记录见运行态诊断）。`, {
        title: '自动修复记录',
        level: 'info',
        category: 'audit',
        displayMode: 'silent',
      });
    }

    if (state.autoContinuationAttempt > 0) {
      publishInternalControlNotice(this.deps.messageHub, `已自动续跑 ${state.autoContinuationAttempt} 轮（详细记录见运行态诊断）。`, {
        title: '自动续跑记录',
        level: 'info',
        category: 'audit',
        displayMode: 'silent',
      });
    }

    return {
      action: 'finalize',
      state,
      finalContent,
      finalExecutionStatus,
      executionErrors: this.buildFinalExecutionErrors(input, finalExecutionStatus),
    };
  }

  private resolveAutoRepairMaxRounds(): number {
    const raw = this.deps.getAutoRepairMaxRounds();
    const value = Number(raw);
    if (!Number.isFinite(value)) {
      return 0;
    }
    return Math.max(0, value);
  }

  private buildFinalExecutionErrors(
    input: RecoveryCoordinationInput,
    finalExecutionStatusOverride?: 'completed' | 'failed' | 'cancelled' | 'paused',
  ): string[] {
    const finalExecutionStatus = finalExecutionStatusOverride
      ?? this.deps.helpers.resolveExecutionFinalStatus(input.runtimeReason, input.runtimeSnapshot);
    if (finalExecutionStatus !== 'failed') {
      return [];
    }
    const combinedErrors = [
      ...(Array.isArray(input.executionErrors) ? input.executionErrors : []),
      ...input.executionWarnings,
    ];
    return this.deps.helpers.buildExecutionFailureMessages(input.runtimeReason, combinedErrors);
  }

  private isOrchestrationContractFailureWithoutAssignments(input: {
    requirementAnalysis: RequirementAnalysis;
    finalExecutionStatus: 'completed' | 'failed' | 'cancelled' | 'paused';
    hasStructuredExecutionContext: boolean;
  }): boolean {
    const decisionFactors = Array.isArray(input.requirementAnalysis.decisionFactors)
      ? input.requirementAnalysis.decisionFactors
      : [];
    return input.finalExecutionStatus === 'failed'
      && input.hasStructuredExecutionContext === false
      && input.requirementAnalysis.entryPath === 'task_execution'
      && decisionFactors.includes('signal:explicit_worker_dispatch_intent');
  }

  private async resetPhaseContinuation(sessionId: string): Promise<void> {
    const planId = this.deps.helpers.getCurrentPlanId();
    if (!planId) {
      return;
    }
    try {
      await this.deps.planLedger.updateRuntimeState(sessionId, planId, {
        phase: {
          state: 'idle',
          continuationIntent: 'stop',
          remainingPhases: [],
          nextIndex: undefined,
          nextTitle: undefined,
        },
      }, {
        auditReason: 'orchestration-contract-failure:clear-phase-continuation',
      });
    } catch (error) {
      logger.warn('编排器.PlanRuntime.phase清理失败', {
        sessionId,
        planId,
        error: error instanceof Error ? error.message : String(error),
      }, LogCategory.ORCHESTRATOR);
    }
  }
}
