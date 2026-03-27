import type { MissionContinuationPolicy, MissionDeliveryStatus, MissionStatus } from '../mission/types';
import type { PlanAcceptanceSummary, PlanMode, PlanStatus } from '../plan-ledger/types';
import type {
  ResolvedOrchestratorTerminationReason,
  RuntimeTerminationDecisionTraceEntry,
  RuntimeTerminationSnapshot,
} from '../core/orchestration/orchestration-control-plane-types';
import type { OrchestrationTraceLinks } from '../trace/types';
import type { ExecutionChainStatus, InterruptedReason } from './execution-chain-types';

export type OrchestrationRuntimeStateStatus =
  | 'idle'
  | 'running'
  | 'waiting'
  | 'paused'
  | 'completed'
  | 'failed'
  | 'cancelled';

export interface OrchestrationRuntimeStateQuery {
  sessionId: string;
  requestId?: string;
  missionId?: string;
  planId?: string;
  batchId?: string;
  chainId?: string;
  maxTimelineEvents?: number;
  maxStateDiffs?: number;
  maxKnowledgeAuditEntries?: number;
  liveRuntimeReason?: ResolvedOrchestratorTerminationReason;
  liveFailureReason?: string;
  liveErrors?: string[];
  liveRuntimeSnapshot?: RuntimeTerminationSnapshot | null;
  liveRuntimeDecisionTrace?: RuntimeTerminationDecisionTraceEntry[];
  livePhase?: string;
  liveProcessingState?: {
    isProcessing: boolean;
    startedAt?: number | null;
    source?: string | null;
    agent?: string | null;
  };
  updatedAt?: number;
}

export interface OrchestrationRuntimeScopeSummary {
  sessionId: string;
  requestId?: string;
  missionId?: string;
  planId?: string;
  batchId?: string;
}

export interface OrchestrationRuntimeMissionSummary {
  missionId: string;
  title: string;
  status: MissionStatus;
  deliveryStatus: MissionDeliveryStatus;
  updatedAt: number;
  failureReason?: string;
}

export interface OrchestrationRuntimePlanSummary {
  planId: string;
  status: PlanStatus;
  mode: PlanMode;
  revision: number;
  version: number;
  updatedAt: number;
  acceptanceSummary: PlanAcceptanceSummary;
  waitState: string;
  replanState: string;
  terminationReason?: string;
}

export interface OrchestrationRuntimeTimelineEntry {
  eventId: string;
  seq: number;
  timestamp: number;
  type: string;
  summary: string;
  diffCount: number;
  trace?: OrchestrationTraceLinks;
}

export interface OrchestrationRuntimeStateDiffEntry {
  eventId: string;
  timestamp: number;
  entityType: string;
  entityId: string;
  changedKeys: string[];
  beforeSummary?: string;
  afterSummary?: string;
}

export interface OrchestrationRuntimeAssignmentSummary {
  assignmentId: string;
  workerId?: string;
  title: string;
  status: string;
  progress: number;
  todoTotal: number;
  completedTodos: number;
  failedTodos: number;
  runningTodos: number;
  trace?: OrchestrationTraceLinks;
}

export interface OrchestrationRuntimeChainSummary {
  chainId: string;
  status: ExecutionChainStatus;
  recoverable: boolean;
  attempt: number;
  createdAt: number;
  updatedAt: number;
  interruptedReason?: InterruptedReason;
}

export interface OrchestrationRuntimeFailureRootCause {
  summary: string;
  eventType?: string;
  eventId?: string;
  occurredAt: number;
  assignmentId?: string;
  todoId?: string;
  verificationId?: string;
  error?: string;
}

export interface OrchestrationRuntimeRecoverySummary {
  continuationPolicy?: MissionContinuationPolicy;
  continuationReason?: string;
  waitState?: string;
  waitReasonCode?: string;
  replanState?: string;
  replanReason?: string;
  terminationReason?: string;
  acceptanceSummary?: PlanAcceptanceSummary;
  reviewState?: string;
}

export interface OrchestrationRuntimeKnowledgeAuditEntry {
  eventId: string;
  timestamp: number;
  purpose: string;
  consumer?: string;
  resultKind: string;
  referenceCount: number;
  sessionId?: string;
  requestId?: string;
  missionId?: string;
  assignmentId?: string;
  todoId?: string;
  workerId?: string;
}

export interface OrchestrationRuntimeKnowledgeAuditView {
  auditPath: string;
  eventCount: number;
  recentEntries: OrchestrationRuntimeKnowledgeAuditEntry[];
}

export interface OrchestrationRuntimeOpsView {
  scope: OrchestrationRuntimeScopeSummary;
  timelinePath: string;
  eventCount: number;
  diffCount: number;
  mission?: OrchestrationRuntimeMissionSummary;
  plan?: OrchestrationRuntimePlanSummary;
  recentTimeline: OrchestrationRuntimeTimelineEntry[];
  recentStateDiffs: OrchestrationRuntimeStateDiffEntry[];
  failureRootCause?: OrchestrationRuntimeFailureRootCause;
  recovery?: OrchestrationRuntimeRecoverySummary;
  knowledgeAudit?: OrchestrationRuntimeKnowledgeAuditView;
}

export interface OrchestrationRuntimeStateSnapshot {
  sessionId: string;
  requestId?: string;
  chain?: OrchestrationRuntimeChainSummary;
  status: OrchestrationRuntimeStateStatus;
  phase: string;
  statusReason?: string;
  canResume?: boolean;
  runtimeReason?: string;
  failureReason?: string;
  errors: string[];
  startedAt?: number;
  statusChangedAt: number;
  lastEventAt: number;
  endedAt?: number;
  runtimeSnapshot?: RuntimeTerminationSnapshot | null;
  runtimeDecisionTrace?: RuntimeTerminationDecisionTraceEntry[];
  assignments: OrchestrationRuntimeAssignmentSummary[];
  opsView?: OrchestrationRuntimeOpsView | null;
}
