import { t } from '../../i18n';
import { logger, LogCategory } from '../../logging';
import type { DispatchBatch } from './dispatch-batch';
import { runPostDispatchVerification, type DeliveryVerificationOutcome } from './post-dispatch-verifier';
import type { MessageHub } from './message-hub';
import type { DispatchManager } from './dispatch-manager';
import type { EffectiveModeResolution } from './effective-mode-resolver';
import type { WorkerSlot } from '../../types';
import type { VerificationRunner } from '../verification-runner';
import type {
  AcceptanceCriterion,
  AcceptanceExecutionReport,
  MissionContinuationPolicy,
  MissionDeliveryStatus,
  MissionStorageManager,
} from '../mission';
import { PlanLedgerService } from '../plan-ledger';
import type { OrchestrationTraceLinks } from '../trace/types';

export interface DeliveryRoundState {
  deliveryStatusForMission: MissionDeliveryStatus | null;
  deliverySummaryForMission?: string;
  deliveryDetailsForMission?: string;
  deliveryWarningsForMission?: string[];
  acceptanceReportForMission?: AcceptanceExecutionReport;
  continuationPolicyForMission?: MissionContinuationPolicy;
  continuationReasonForMission?: string;
}

export interface DeliveryRoundInput {
  sessionId: string;
  batch?: DispatchBatch | null;
  responseContent: string;
  effectiveMode: EffectiveModeResolution;
  state: DeliveryRoundState;
}

export interface DeliveryRoundResult extends DeliveryRoundState {
  finalContent: string;
  deliveryNotes: string[];
}

interface DeliveryControllerHelperBag {
  getCurrentPlanId: () => string | null;
  getLastMissionId: () => string | null;
  mergeAcceptanceCriteriaWithExecutionReport: (input: {
    criteria?: AcceptanceCriterion[] | null;
    report?: AcceptanceExecutionReport;
    reviewRound: number;
    batchId?: string;
    workers?: WorkerSlot[];
  }) => AcceptanceCriterion[];
}

export interface OrchestrationDeliveryControllerDependencies {
  dispatchManager: DispatchManager;
  messageHub: MessageHub;
  missionStorage: MissionStorageManager;
  planLedger: PlanLedgerService;
  workspaceRoot: string;
  getVerificationRunner: () => VerificationRunner | undefined;
  onVerificationCompleted?: (payload: {
    sessionId: string;
    planId?: string;
    batchId: string;
    trace?: OrchestrationTraceLinks;
    outcome: DeliveryVerificationOutcome;
  }) => Promise<void> | void;
  helpers: DeliveryControllerHelperBag;
}

export class OrchestrationDeliveryController {
  constructor(
    private readonly deps: OrchestrationDeliveryControllerDependencies,
  ) {}

  async processRound(input: DeliveryRoundInput): Promise<DeliveryRoundResult> {
    let state: DeliveryRoundState = { ...input.state };
    let finalContent = input.responseContent || '';
    const deliveryNotes: string[] = [];
    const currentBatch = input.batch;
    const auditOutcome = currentBatch?.getAuditOutcome();

    if (auditOutcome?.level === 'intervention') {
      const blockedMessage = t('engine.phaseC.interventionBlocked');
      deliveryNotes.push(blockedMessage);
      state = {
        ...state,
        deliveryStatusForMission: 'blocked',
        deliverySummaryForMission: blockedMessage,
        continuationPolicyForMission: 'stop',
        continuationReasonForMission: blockedMessage,
      };
      logger.warn('编排器.PhaseC.审计阻断_已降级', {
        batchId: currentBatch?.id,
        level: auditOutcome.level,
      }, LogCategory.ORCHESTRATOR);
    }

    if (currentBatch) {
      state = await this.applyBatchDelivery({
        sessionId: input.sessionId,
        batch: currentBatch,
        effectiveMode: input.effectiveMode,
        state,
        deliveryNotes,
      });
    }

    if (currentBatch && this.deps.dispatchManager.isReactiveBatchAwaitingSummary(currentBatch.id)) {
      if (finalContent.trim()) {
        this.deps.dispatchManager.markReactiveBatchSummarized(currentBatch.id);
      } else {
        finalContent = this.deps.dispatchManager.buildReactiveBatchFallbackSummary(currentBatch);
        this.deps.messageHub.result(finalContent, {
          metadata: {
            phase: 'reactive_fallback_summary',
            extra: {
              batchId: currentBatch.id,
            },
          },
        });
        this.deps.dispatchManager.markReactiveBatchSummarized(currentBatch.id);
      }
    }

    return {
      ...state,
      finalContent,
      deliveryNotes,
    };
  }

  private async applyBatchDelivery(input: {
    sessionId: string;
    batch: DispatchBatch;
    effectiveMode: EffectiveModeResolution;
    state: DeliveryRoundState;
    deliveryNotes: string[];
  }): Promise<DeliveryRoundState> {
    const planIdForRuntime = this.deps.helpers.getCurrentPlanId();
    const planForRuntime = planIdForRuntime
      ? this.deps.planLedger.getPlan(input.sessionId, planIdForRuntime)
      : null;
    const reviewRoundForRuntime = Math.max(1, (planForRuntime?.runtime.review.round || 0) + 1);
    const acceptanceCriteriaForRuntime = planForRuntime?.runtime.acceptance.criteria;
    const lastMissionId = this.deps.helpers.getLastMissionId();

    if (lastMissionId) {
      try {
        await this.deps.missionStorage.transitionStatus(lastMissionId, 'reviewing');
      } catch (error) {
        logger.warn('编排器.Mission.状态迁移失败', {
          missionId: lastMissionId,
          from: 'executing',
          to: 'reviewing',
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
      }
    }

    if (planIdForRuntime) {
      try {
        await this.deps.planLedger.updateRuntimeState(input.sessionId, planIdForRuntime, {
          review: { state: 'running', round: reviewRoundForRuntime },
        });
      } catch (error) {
        logger.warn('编排器.PlanRuntime.评审状态推进失败', {
          sessionId: input.sessionId,
          planId: planIdForRuntime,
          state: 'running',
          round: reviewRoundForRuntime,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
      }
    }

    let outcome: DeliveryVerificationOutcome;
    try {
      outcome = await runPostDispatchVerification(
        input.batch,
        this.deps.getVerificationRunner(),
        this.deps.messageHub,
        {
          workspaceRoot: this.deps.workspaceRoot,
          acceptanceCriteria: acceptanceCriteriaForRuntime,
        },
      );
    } catch (verificationError) {
      const verificationMessage = verificationError instanceof Error
        ? verificationError.message
        : String(verificationError);
      logger.warn('编排器.PhaseC.校验异常_已降级', {
        batchId: input.batch.id,
        error: verificationMessage,
      }, LogCategory.ORCHESTRATOR);
      outcome = {
        status: 'failed',
        summary: `验收异常：${verificationMessage}`,
      };
    }

    const acceptanceCriteriaWithSpec = this.deps.helpers.mergeAcceptanceCriteriaWithExecutionReport({
      criteria: acceptanceCriteriaForRuntime,
      report: outcome,
      reviewRound: reviewRoundForRuntime,
      batchId: input.batch.id,
      workers: input.batch.getEntries().map((entry) => entry.worker),
    });
    const shouldUpdateAcceptanceCriteria = (outcome.criteriaSummary?.total || 0) > 0
      && acceptanceCriteriaWithSpec.length > 0;

    if (planIdForRuntime) {
      try {
        if (outcome.status === 'failed') {
          await this.deps.planLedger.updateRuntimeState(input.sessionId, planIdForRuntime, {
            review: { state: 'rejected', round: reviewRoundForRuntime },
            acceptance: {
              summary: 'failed',
              criteria: shouldUpdateAcceptanceCriteria ? acceptanceCriteriaWithSpec : undefined,
            },
            replan: { state: 'required', reason: outcome.summary },
          });
        } else if (outcome.status === 'passed') {
          await this.deps.planLedger.updateRuntimeState(input.sessionId, planIdForRuntime, {
            review: { state: 'accepted', round: reviewRoundForRuntime },
            acceptance: {
              summary: 'passed',
              criteria: shouldUpdateAcceptanceCriteria ? acceptanceCriteriaWithSpec : undefined,
            },
            replan: { state: 'none' },
          });
        } else if (outcome.skippedReason === 'execution_failed') {
          await this.deps.planLedger.updateRuntimeState(input.sessionId, planIdForRuntime, {
            review: { state: 'rejected', round: reviewRoundForRuntime },
            acceptance: {
              summary: 'failed',
              criteria: shouldUpdateAcceptanceCriteria ? acceptanceCriteriaWithSpec : undefined,
            },
            replan: { state: 'required', reason: outcome.summary },
          });
        } else {
          await this.deps.planLedger.updateRuntimeState(input.sessionId, planIdForRuntime, {
            review: { state: 'idle', round: reviewRoundForRuntime },
            acceptance: {
              criteria: shouldUpdateAcceptanceCriteria ? acceptanceCriteriaWithSpec : undefined,
            },
            replan: { state: 'none' },
          });
        }
      } catch (error) {
        logger.warn('编排器.PlanRuntime.评审结果回写失败', {
          sessionId: input.sessionId,
          planId: planIdForRuntime,
          status: outcome.status,
          skippedReason: outcome.skippedReason,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
      }
    }

    if (this.deps.onVerificationCompleted) {
      try {
        await this.deps.onVerificationCompleted({
          sessionId: input.sessionId,
          planId: planIdForRuntime || undefined,
          batchId: input.batch.id,
          trace: outcome.trace,
          outcome,
        });
      } catch (error) {
        logger.warn('编排器.PhaseC.timeline回写失败', {
          sessionId: input.sessionId,
          planId: planIdForRuntime,
          batchId: input.batch.id,
          error: error instanceof Error ? error.message : String(error),
        }, LogCategory.ORCHESTRATOR);
      }
    }

    return this.mergeDeliveryOutcome({
      state: input.state,
      outcome,
      allowDeepContinuation: input.effectiveMode.allowDeepContinuation,
      deliveryNotes: input.deliveryNotes,
    });
  }

  private mergeDeliveryOutcome(input: {
    state: DeliveryRoundState;
    outcome: DeliveryVerificationOutcome;
    allowDeepContinuation: boolean;
    deliveryNotes: string[];
  }): DeliveryRoundState {
    const state = { ...input.state };
    const outcome = input.outcome;

    if (outcome.status === 'failed') {
      input.deliveryNotes.push(outcome.summary);
      if (state.deliveryStatusForMission === 'blocked') {
        return {
          ...state,
          deliverySummaryForMission: state.deliverySummaryForMission || outcome.summary,
          deliveryDetailsForMission: state.deliveryDetailsForMission || outcome.details,
          deliveryWarningsForMission: state.deliveryWarningsForMission || (outcome.warnings && outcome.warnings.length > 0 ? outcome.warnings : undefined),
          acceptanceReportForMission: state.acceptanceReportForMission || outcome,
          continuationPolicyForMission: state.continuationPolicyForMission || (input.allowDeepContinuation ? 'auto' : 'stop'),
          continuationReasonForMission: state.continuationReasonForMission || outcome.summary,
        };
      }
      return {
        ...state,
        deliveryStatusForMission: 'failed',
        deliverySummaryForMission: outcome.summary,
        deliveryDetailsForMission: outcome.details,
        deliveryWarningsForMission: outcome.warnings && outcome.warnings.length > 0 ? outcome.warnings : undefined,
        acceptanceReportForMission: outcome,
        continuationPolicyForMission: input.allowDeepContinuation ? 'auto' : 'stop',
        continuationReasonForMission: outcome.summary,
      };
    }

    if (outcome.status === 'passed') {
      if (state.deliveryStatusForMission === 'blocked') {
        return {
          ...state,
          deliverySummaryForMission: state.deliverySummaryForMission || outcome.summary,
          deliveryDetailsForMission: state.deliveryDetailsForMission || outcome.details,
          deliveryWarningsForMission: state.deliveryWarningsForMission || (outcome.warnings && outcome.warnings.length > 0 ? outcome.warnings : undefined),
          acceptanceReportForMission: state.acceptanceReportForMission || outcome,
          continuationPolicyForMission: state.continuationPolicyForMission || 'stop',
          continuationReasonForMission: state.continuationReasonForMission || outcome.summary,
        };
      }
      return {
        ...state,
        deliveryStatusForMission: 'passed',
        deliverySummaryForMission: outcome.summary,
        deliveryDetailsForMission: outcome.details,
        deliveryWarningsForMission: outcome.warnings && outcome.warnings.length > 0 ? outcome.warnings : undefined,
        acceptanceReportForMission: outcome,
        continuationPolicyForMission: 'stop',
        continuationReasonForMission: outcome.summary,
      };
    }

    const skippedStatus: MissionDeliveryStatus = outcome.skippedReason === 'execution_failed'
      ? 'blocked'
      : 'skipped';
    if (outcome.skippedReason === 'execution_failed') {
      input.deliveryNotes.push(outcome.summary);
    }
    if (state.deliveryStatusForMission === 'blocked') {
      return {
        ...state,
        deliverySummaryForMission: state.deliverySummaryForMission || outcome.summary,
        deliveryDetailsForMission: state.deliveryDetailsForMission || outcome.details,
        deliveryWarningsForMission: state.deliveryWarningsForMission || (outcome.warnings && outcome.warnings.length > 0 ? outcome.warnings : undefined),
        acceptanceReportForMission: state.acceptanceReportForMission || outcome,
        continuationPolicyForMission: state.continuationPolicyForMission || (
          skippedStatus === 'blocked' && input.allowDeepContinuation ? 'auto' : 'stop'
        ),
        continuationReasonForMission: state.continuationReasonForMission || outcome.summary,
      };
    }
    return {
      ...state,
      deliveryStatusForMission: skippedStatus,
      deliverySummaryForMission: outcome.summary,
      deliveryDetailsForMission: outcome.details,
      deliveryWarningsForMission: outcome.warnings && outcome.warnings.length > 0 ? outcome.warnings : undefined,
      acceptanceReportForMission: outcome,
      continuationPolicyForMission: skippedStatus === 'blocked' && input.allowDeepContinuation ? 'auto' : 'stop',
      continuationReasonForMission: outcome.summary,
    };
  }
}
