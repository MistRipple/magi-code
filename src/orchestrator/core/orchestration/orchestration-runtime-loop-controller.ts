import { t } from '../../../i18n';
import { logger, LogCategory } from '../../../logging';
import { resolveTimelineAnchorTimestampFromMetadata } from '../../../shared/timeline-ordering';
import type { WorkerSlot } from '../../../types';
import type { TokenUsage } from '../../../types/agent-types';
import type { MessageHub } from '../message/message-hub';
import type { DispatchManager } from '../dispatch/dispatch-manager';
import type { EffectiveModeResolution } from '../effective-mode-resolver';
import type {
  ResolvedOrchestratorTerminationReason,
  RuntimeTerminationDecisionTraceEntry,
  RuntimeTerminationShadow,
  RuntimeTerminationSnapshot,
} from './orchestration-control-plane-types';
import type { RequirementAnalysis } from '../../protocols/types';
import type { VerificationRunner } from '../../verification-runner';
import type {
  AcceptanceCriterion,
  AcceptanceExecutionReport,
  MissionContinuationPolicy,
  MissionDeliveryStatus,
  MissionStorageManager,
} from '../../mission';
import { PlanLedgerService, type PlanRuntimePhaseState } from '../../plan-ledger';
import type { IAdapterFactory } from '../../../adapters/adapter-factory-interface';
import type { TerminationCandidate } from '../../../llm/adapters/orchestrator-termination';
import {
  OrchestrationDeliveryController,
  type DeliveryRoundState,
  type OrchestrationDeliveryControllerDependencies,
} from './orchestration-delivery-controller';
import {
  OrchestrationRecoveryCoordinator,
  type OrchestrationRecoveryCoordinatorDependencies,
  type RecoveryLoopState,
} from './orchestration-recovery-coordinator';
import { publishInternalControlNotice } from '../internal-control-notice';

export interface RuntimeLoopInput {
  sessionId: string;
  prompt: string;
  imagePaths?: string[];
  rootRequestId: string;
  systemPrompt: string;
  requirementAnalysis: RequirementAnalysis;
  effectiveMode: EffectiveModeResolution;
}

export interface RuntimeLoopResult {
  finalContent: string;
  runtimeReason: ResolvedOrchestratorTerminationReason;
  runtimeRounds: number;
  runtimeSnapshot?: RuntimeTerminationSnapshot;
  runtimeShadow?: RuntimeTerminationShadow;
  runtimeDecisionTrace?: RuntimeTerminationDecisionTraceEntry[];
  runtimeTokenUsage?: TokenUsage;
  deliveryStatusForMission: MissionDeliveryStatus | null;
  deliverySummaryForMission?: string;
  deliveryDetailsForMission?: string;
  deliveryWarningsForMission?: string[];
  acceptanceReportForMission?: AcceptanceExecutionReport;
  continuationPolicyForMission?: MissionContinuationPolicy;
  continuationReasonForMission?: string;
  finalExecutionStatus: 'completed' | 'failed' | 'cancelled' | 'paused';
  executionErrors: string[];
}

interface RuntimeLoopHelperBag {
  getCurrentPlanId: () => string | null;
  getLastMissionId: () => string | null;
  setActiveRoundRequestId: (requestId: string) => void;
  setActiveUserPrompt: (prompt: string) => void;
  recordOrchestratorTokens: (usage?: TokenUsage) => void;
  normalizeOrchestratorRuntimeReason: (
    runtimeReason?: string,
  ) => ResolvedOrchestratorTerminationReason | undefined;
  resolveOrchestratorRuntimeReason: (input: {
    runtimeReason?: string;
    runtimeSnapshot?: RuntimeTerminationSnapshot;
    additionalCandidates?: TerminationCandidate[];
    fallback?: ResolvedOrchestratorTerminationReason;
  }) => {
    reason: ResolvedOrchestratorTerminationReason;
    runtimeSnapshot?: RuntimeTerminationSnapshot;
  };
  resolveExecutionFinalStatus: (
    runtimeReason?: ResolvedOrchestratorTerminationReason,
    runtimeSnapshot?: RuntimeTerminationSnapshot,
  ) => 'completed' | 'failed' | 'cancelled' | 'paused';
  isGovernancePauseReason: (reason: ResolvedOrchestratorTerminationReason) => boolean;
  resolveRequiredTotal: (snapshot?: RuntimeTerminationSnapshot) => number | undefined;
  resolveTerminalRequired: (snapshot?: RuntimeTerminationSnapshot) => number | undefined;
  extractPendingRequiredCount: (snapshot?: RuntimeTerminationSnapshot) => number;
  buildFollowUpProgressSignature: (snapshot?: RuntimeTerminationSnapshot) => string;
  mergeAcceptanceCriteriaWithExecutionReport: (input: {
    criteria?: AcceptanceCriterion[] | null;
    report?: AcceptanceExecutionReport;
    reviewRound: number;
    batchId?: string;
    workers?: WorkerSlot[];
  }) => AcceptanceCriterion[];
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

export interface OrchestrationRuntimeLoopControllerDependencies {
  adapterFactory: IAdapterFactory;
  dispatchManager: DispatchManager;
  messageHub: MessageHub;
  missionStorage: MissionStorageManager;
  planLedger: PlanLedgerService;
  workspaceRoot: string;
  getVerificationRunner: () => VerificationRunner | undefined;
  getAutoRepairMaxRounds: () => number | undefined;
  onVerificationCompleted?: OrchestrationDeliveryControllerDependencies['onVerificationCompleted'];
  helpers: RuntimeLoopHelperBag;
}

export class OrchestrationRuntimeLoopController {
  private readonly deliveryController: OrchestrationDeliveryController;
  private readonly recoveryCoordinator: OrchestrationRecoveryCoordinator;

  constructor(
    private readonly deps: OrchestrationRuntimeLoopControllerDependencies,
  ) {
    const deliveryDeps: OrchestrationDeliveryControllerDependencies = {
      dispatchManager: deps.dispatchManager,
      messageHub: deps.messageHub,
      missionStorage: deps.missionStorage,
      planLedger: deps.planLedger,
      workspaceRoot: deps.workspaceRoot,
      getVerificationRunner: deps.getVerificationRunner,
      onVerificationCompleted: deps.onVerificationCompleted,
      helpers: {
        getCurrentPlanId: deps.helpers.getCurrentPlanId,
        getLastMissionId: deps.helpers.getLastMissionId,
        mergeAcceptanceCriteriaWithExecutionReport: deps.helpers.mergeAcceptanceCriteriaWithExecutionReport,
      },
    };
    this.deliveryController = new OrchestrationDeliveryController(deliveryDeps);

    const recoveryDeps: OrchestrationRecoveryCoordinatorDependencies = {
      messageHub: deps.messageHub,
      missionStorage: deps.missionStorage,
      planLedger: deps.planLedger,
      getAutoRepairMaxRounds: deps.getAutoRepairMaxRounds,
      helpers: {
        getCurrentPlanId: deps.helpers.getCurrentPlanId,
        getLastMissionId: deps.helpers.getLastMissionId,
        setActiveRoundRequestId: deps.helpers.setActiveRoundRequestId,
        normalizeOrchestratorRuntimeReason: deps.helpers.normalizeOrchestratorRuntimeReason,
        resolveExecutionFinalStatus: deps.helpers.resolveExecutionFinalStatus,
        isGovernancePauseReason: deps.helpers.isGovernancePauseReason,
        resolveRequiredTotal: deps.helpers.resolveRequiredTotal,
        resolveTerminalRequired: deps.helpers.resolveTerminalRequired,
        extractPendingRequiredCount: deps.helpers.extractPendingRequiredCount,
        buildFollowUpProgressSignature: deps.helpers.buildFollowUpProgressSignature,
        buildAutoRepairPrompt: deps.helpers.buildAutoRepairPrompt,
        buildGovernanceRecoveryPrompt: deps.helpers.buildGovernanceRecoveryPrompt,
        resolveFollowUpSteps: deps.helpers.resolveFollowUpSteps,
        extractStructuredContinuationStepsFromContent: deps.helpers.extractStructuredContinuationStepsFromContent,
        classifyFollowUpSteps: deps.helpers.classifyFollowUpSteps,
        buildFollowUpBlockedNotice: deps.helpers.buildFollowUpBlockedNotice,
        buildPhaseRuntimePatch: deps.helpers.buildPhaseRuntimePatch,
        resolvePhaseRuntimeForDecision: deps.helpers.resolvePhaseRuntimeForDecision,
        stripNonActionableFollowUpSection: deps.helpers.stripNonActionableFollowUpSection,
        markPhaseRuntimeRunning: deps.helpers.markPhaseRuntimeRunning,
        beginSyntheticExecutionRound: deps.helpers.beginSyntheticExecutionRound,
        buildAutoFollowUpPrompt: deps.helpers.buildAutoFollowUpPrompt,
        buildGovernancePauseReport: deps.helpers.buildGovernancePauseReport,
        formatGovernanceReason: deps.helpers.formatGovernanceReason,
        buildExecutionFailureMessages: deps.helpers.buildExecutionFailureMessages,
      },
    };
    this.recoveryCoordinator = new OrchestrationRecoveryCoordinator(recoveryDeps);
  }

  async run(input: RuntimeLoopInput): Promise<RuntimeLoopResult> {
    const requiresOrchestrationArtifacts = input.requirementAnalysis.entryPath === 'task_execution'
      && Array.isArray(input.requirementAnalysis.decisionFactors)
      && input.requirementAnalysis.decisionFactors.includes('signal:explicit_worker_dispatch_intent');
    let recoveryState: RecoveryLoopState = {
      autoRepairAttempt: 0,
      autoContinuationAttempt: 0,
      lastAutoRepairSignature: '',
      autoRepairStallStreak: 0,
      governanceRecoveryAttempt: 0,
      totalRecoveryRounds: 0,
    };
    let promptForRound = input.prompt;
    let currentRoundRequestId = input.rootRequestId;

    let orchestratorRuntimeReason: ResolvedOrchestratorTerminationReason = 'completed';
    let orchestratorRuntimeRounds = 0;
    let orchestratorRuntimeSnapshot: RuntimeTerminationSnapshot | undefined;
    let orchestratorRuntimeShadow: RuntimeTerminationShadow | undefined;
    let orchestratorRuntimeDecisionTrace: RuntimeTerminationDecisionTraceEntry[] | undefined;
    let runtimeTokenUsage: TokenUsage | undefined;
    let deliveryStatusForMission: MissionDeliveryStatus | null = null;
    let deliverySummaryForMission: string | undefined;
    let deliveryDetailsForMission: string | undefined;
    let deliveryWarningsForMission: string[] | undefined;
    let acceptanceReportForMission: AcceptanceExecutionReport | undefined;
    let continuationPolicyForMission: MissionContinuationPolicy | undefined;
    let continuationReasonForMission: string | undefined;

    for (;;) {
      this.deps.helpers.setActiveRoundRequestId(currentRoundRequestId);
      if (currentRoundRequestId !== input.rootRequestId) {
        this.deps.dispatchManager.resetForNewExecutionCycle();
      }

      deliveryStatusForMission = null;
      deliverySummaryForMission = undefined;
      deliveryDetailsForMission = undefined;
      deliveryWarningsForMission = undefined;
      acceptanceReportForMission = undefined;
      continuationPolicyForMission = undefined;
      continuationReasonForMission = undefined;

      this.deps.helpers.setActiveUserPrompt(promptForRound);
      orchestratorRuntimeReason = 'completed';
      orchestratorRuntimeSnapshot = undefined;
      orchestratorRuntimeShadow = undefined;
      orchestratorRuntimeDecisionTrace = undefined;

      // 解析当前轮次请求消息的锚点时间，确保编排器流式消息与 SubTaskCard 共享同一排序时间。
      // 如果不对齐，SubTaskCard 会因 batch 锚定到请求消息时间(T0)而排到编排器思考过程(T1>T0)之前。
      const roundAnchorTimestamp = (() => {
        const requestMessageId = this.deps.messageHub.getRequestMessageId(currentRoundRequestId);
        if (!requestMessageId) return undefined;
        const requestMessage = this.deps.messageHub.getMessageSnapshot(requestMessageId);
        const metadata = requestMessage?.metadata as Record<string, unknown> | undefined;
        const metadataAnchor = resolveTimelineAnchorTimestampFromMetadata(metadata);
        if (metadataAnchor !== null) return metadataAnchor;
        return typeof requestMessage?.timestamp === 'number'
          && Number.isFinite(requestMessage.timestamp)
          && requestMessage.timestamp > 0
          ? Math.floor(requestMessage.timestamp)
          : undefined;
      })();

      const response = await this.deps.adapterFactory.sendMessage(
        'orchestrator',
        promptForRound,
        input.imagePaths,
        {
          planningMode: input.effectiveMode.planningMode,
          source: 'orchestrator',
          adapterRole: 'orchestrator',
          systemPrompt: input.systemPrompt,
          includeThinking: input.requirementAnalysis.includeThinking ?? false,
          includeToolCalls: input.requirementAnalysis.includeToolCalls ?? false,
          toolPolicy: input.requirementAnalysis.toolPolicy,
          historyMode: input.requirementAnalysis.historyMode ?? 'isolated',
          requestId: currentRoundRequestId,
          messageMetadata: {
            sessionId: input.sessionId,
            ...(roundAnchorTimestamp ? { timelineAnchorTimestamp: roundAnchorTimestamp } : {}),
          },
        },
      );

      orchestratorRuntimeReason = this.deps.helpers.normalizeOrchestratorRuntimeReason(
        response.orchestratorRuntime?.reason,
      ) || 'failed';
      orchestratorRuntimeRounds = response.orchestratorRuntime?.rounds || 0;
      orchestratorRuntimeSnapshot = response.orchestratorRuntime?.snapshot as RuntimeTerminationSnapshot | undefined;
      orchestratorRuntimeShadow = response.orchestratorRuntime?.shadow as RuntimeTerminationShadow | undefined;
      orchestratorRuntimeDecisionTrace =
        response.orchestratorRuntime?.decisionTrace as RuntimeTerminationDecisionTraceEntry[] | undefined;
      runtimeTokenUsage = response.tokenUsage;
      this.deps.helpers.recordOrchestratorTokens(response.tokenUsage);

      const currentBatch = this.deps.dispatchManager.getActiveBatch();
      if (currentBatch && currentBatch.status !== 'archived') {
        await currentBatch.waitForArchive(this.deps.dispatchManager.getIdleTimeoutMs());
      }
      const hasDispatchEntries = Boolean(currentBatch && currentBatch.size > 0);

      const executionWarnings: string[] = [];
      const executionErrors: string[] = [];
      const terminationCandidates: TerminationCandidate[] = [];
      let orchestrationArtifactFailureMessage: string | undefined;

      if (response.error) {
        const modelError = response.error.trim();
        executionWarnings.push(modelError ? `上游模型异常：${modelError}` : '上游模型异常：未知错误');
        terminationCandidates.push({
          reason: 'upstream_model_error',
          eventId: 'engine:upstream-model-error',
          triggeredAt: Date.now(),
        });
        logger.warn('编排器.统一执行.上游模型异常_已降级', {
          error: response.error,
        }, LogCategory.ORCHESTRATOR);
      }

      if (requiresOrchestrationArtifacts && !hasDispatchEntries) {
        orchestrationArtifactFailureMessage = t('engine.errors.orchestrationRequiredButNoAssignment');
        executionErrors.push(orchestrationArtifactFailureMessage);
        terminationCandidates.push({
          reason: 'failed',
          eventId: 'engine:orchestration-required-no-assignment',
          triggeredAt: Date.now(),
        });
        logger.warn('编排器.统一执行.编排请求缺少Assignment', {
          requestId: currentRoundRequestId,
          prompt: input.prompt,
        }, LogCategory.ORCHESTRATOR);
      }

      const deliveryRound = await this.deliveryController.processRound({
        sessionId: input.sessionId,
        batch: currentBatch,
        responseContent: response.content || '',
        effectiveMode: input.effectiveMode,
        state: {
          deliveryStatusForMission,
          deliverySummaryForMission,
          deliveryDetailsForMission,
          deliveryWarningsForMission,
          acceptanceReportForMission,
          continuationPolicyForMission,
          continuationReasonForMission,
        },
      });
      deliveryStatusForMission = deliveryRound.deliveryStatusForMission;
      deliverySummaryForMission = deliveryRound.deliverySummaryForMission;
      deliveryDetailsForMission = deliveryRound.deliveryDetailsForMission;
      deliveryWarningsForMission = deliveryRound.deliveryWarningsForMission;
      acceptanceReportForMission = deliveryRound.acceptanceReportForMission;
      continuationPolicyForMission = deliveryRound.continuationPolicyForMission;
      continuationReasonForMission = deliveryRound.continuationReasonForMission;
      const deliveryNotes = deliveryRound.deliveryNotes;
      let finalContent = deliveryRound.finalContent;

      if (orchestrationArtifactFailureMessage) {
        publishInternalControlNotice(this.deps.messageHub, orchestrationArtifactFailureMessage, {
          title: '编排契约失败',
          level: 'error',
          category: 'incident',
          actionRequired: true,
          countUnread: true,
        });
      }

      const resolvedRuntimeTermination = this.deps.helpers.resolveOrchestratorRuntimeReason({
        runtimeReason: orchestratorRuntimeReason,
        runtimeSnapshot: orchestratorRuntimeSnapshot,
        additionalCandidates: terminationCandidates,
        fallback: terminationCandidates.length > 0 ? 'failed' : 'completed',
      });
      orchestratorRuntimeReason = resolvedRuntimeTermination.reason;
      orchestratorRuntimeSnapshot = resolvedRuntimeTermination.runtimeSnapshot;

      const finalExecutionStatus = this.deps.helpers.resolveExecutionFinalStatus(
        orchestratorRuntimeReason,
        orchestratorRuntimeSnapshot,
      );
      const normalizedRuntimeReason = this.deps.helpers.normalizeOrchestratorRuntimeReason(orchestratorRuntimeReason);

      if (finalExecutionStatus === 'completed' && normalizedRuntimeReason && normalizedRuntimeReason !== 'completed') {
        executionWarnings.push(`终止门禁判定为 ${normalizedRuntimeReason}，但必需 Todo 已完成，按执行完成处理。`);
      }

      if (executionWarnings.length > 0) {
        publishInternalControlNotice(this.deps.messageHub, executionWarnings.map(item => `- ${item}`).join('\n'), {
          title: '运行门禁降级',
          level: 'warning',
          category: 'audit',
        });
      }

      if (deliveryNotes.length > 0) {
        publishInternalControlNotice(this.deps.messageHub, deliveryNotes.map(item => `- ${item}`).join('\n'), {
          title: '交付验收结果',
          level: 'warning',
          category: 'audit',
          source: 'delivery-verification',
        });
      }

      const recoveryResult = await this.recoveryCoordinator.coordinate({
        sessionId: input.sessionId,
        prompt: input.prompt,
        finalContent,
        executionWarnings,
        executionErrors,
        runtimeReason: orchestratorRuntimeReason,
        runtimeSnapshot: orchestratorRuntimeSnapshot,
        runtimeNextSteps: response.orchestratorRuntime?.nextSteps,
        effectiveMode: input.effectiveMode,
        requirementAnalysis: input.requirementAnalysis,
        deliveryState: {
          deliveryStatusForMission,
          deliverySummaryForMission,
          deliveryDetailsForMission,
          deliveryWarningsForMission,
          acceptanceReportForMission,
          continuationPolicyForMission,
          continuationReasonForMission,
        } satisfies DeliveryRoundState,
        state: recoveryState,
      });

      recoveryState = recoveryResult.state;
      if (recoveryResult.action === 'continue') {
        currentRoundRequestId = recoveryResult.nextRequestId;
        promptForRound = recoveryResult.nextPrompt;
        continue;
      }

      return {
        finalContent: recoveryResult.finalContent,
        runtimeReason: orchestratorRuntimeReason,
        runtimeRounds: orchestratorRuntimeRounds,
        runtimeSnapshot: orchestratorRuntimeSnapshot,
        runtimeShadow: orchestratorRuntimeShadow,
        runtimeDecisionTrace: orchestratorRuntimeDecisionTrace,
        runtimeTokenUsage: runtimeTokenUsage,
        deliveryStatusForMission,
        deliverySummaryForMission,
        deliveryDetailsForMission,
        deliveryWarningsForMission,
        acceptanceReportForMission,
        continuationPolicyForMission,
        continuationReasonForMission,
        finalExecutionStatus: recoveryResult.finalExecutionStatus,
        executionErrors: recoveryResult.executionErrors,
      };
    }
  }
}
