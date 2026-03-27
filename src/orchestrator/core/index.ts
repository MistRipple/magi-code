/**
 * Core Module - Mission-Driven Architecture 核心
 *
 * 提供任务编排和执行的核心组件：
 * - MissionOrchestrator: 任务编排核心
 * - MissionDrivenEngine: 编排引擎
 */

export {
  MissionOrchestrator,
} from './mission-orchestrator';

// 编排引擎
export {
  MissionDrivenEngine,
  MissionDrivenEngineConfig,
} from './mission-driven-engine';

export {
  OrchestrationPlanController,
  type OrchestrationPlanControllerDependencies,
  type ResolveExecutionPlanInput,
  type ResolveExecutionPlanResult,
} from './orchestration/orchestration-plan-controller';

export {
  OrchestrationRuntimeLoopController,
  type OrchestrationRuntimeLoopControllerDependencies,
  type RuntimeLoopInput,
  type RuntimeLoopResult,
} from './orchestration/orchestration-runtime-loop-controller';

export {
  OrchestrationDeliveryController,
  type DeliveryRoundInput,
  type DeliveryRoundResult,
  type DeliveryRoundState,
  type OrchestrationDeliveryControllerDependencies,
} from './orchestration/orchestration-delivery-controller';

export {
  OrchestrationRecoveryCoordinator,
  type OrchestrationRecoveryCoordinatorDependencies,
  type RecoveryCoordinationInput,
  type RecoveryCoordinationResult,
  type RecoveryLoopState,
} from './orchestration/orchestration-recovery-coordinator';

export {
  DispatchProtocolManager,
  type DispatchAckState,
  type DispatchExecutionProtocolState,
  type DispatchProtocolManagerDeps,
  type DispatchProtocolTimeoutPayload,
} from './dispatch/dispatch-protocol-manager';

export {
  DispatchScheduler,
  type DispatchExecutionWorkerResolution,
  type DispatchSchedulerDeps,
} from './dispatch/dispatch-scheduler';

export {
  DispatchBatchCoordinator,
  type DispatchBatchCoordinatorDeps,
} from './dispatch/dispatch-batch-coordinator';

export {
  DispatchReactiveWaitCoordinator,
  type DispatchReactiveWaitCoordinatorDeps,
} from './dispatch/dispatch-reactive-wait-coordinator';

export {
  DispatchPresentationAdapter,
  type DispatchPresentationAdapterDeps,
} from './dispatch/dispatch-presentation-adapter';

export {
  FileRequestClassificationCalibrationStore,
  buildRequestClassificationDecisionRecord,
  replayRequestClassificationDecisions,
  type RequestClassificationCalibrationEvent,
  type RequestClassificationCalibrationStore,
  type RequestClassificationDecisionRecord,
  type RequestClassificationFeedbackRecord,
  type RequestClassificationReplayItem,
  type RequestClassificationReplayReport,
} from './request-classification-calibration';

export {
  ValidatorRegistry,
  createDefaultValidatorRegistry,
  createProcessVerificationCommandRunner,
  type VerificationCommandOptions,
  type VerificationCommandRunner,
  type VerificationCustomValidator,
  type VerificationCustomValidatorResult,
  type VerificationSpecExecutionContext,
  type VerificationSpecExecutionResult,
  type VerificationSpecExecutor,
} from './validator-registry';

export {
  type PlanGovernanceAssessment,
  type ResolvedOrchestratorTerminationReason,
  type RuntimeTerminationDecisionTraceEntry,
  type RuntimeTerminationShadow,
  type RuntimeTerminationSnapshot,
} from './orchestration/orchestration-control-plane-types';

// 统一消息出口
export {
  MessageHub,
  globalMessageHub,
  type SubTaskCardPayload,
  type MessageHubEvents,
} from './message/message-hub';
