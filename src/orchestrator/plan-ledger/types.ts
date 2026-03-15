import type { WorkerSlot } from '../../types';
import type { AcceptanceCriterion } from '../mission/types';

export type PlanMode = 'standard' | 'deep';

export type PlanStatus =
  | 'draft'
  | 'awaiting_confirmation'
  | 'approved'
  | 'rejected'
  | 'executing'
  | 'partially_completed'
  | 'completed'
  | 'failed'
  | 'cancelled'
  | 'superseded';

export type PlanItemOwner = 'orchestrator' | WorkerSlot;

export type PlanItemStatus =
  | 'pending'
  | 'running'
  | 'completed'
  | 'failed'
  | 'skipped'
  | 'cancelled';

export type PlanTodoStatus =
  | 'pending'
  | 'in_progress'
  | 'running'
  | 'completed'
  | 'failed'
  | 'skipped'
  | 'blocked'
  | 'cancelled';

export type PlanAttemptScope = 'orchestrator' | 'assignment' | 'todo';

export type PlanAttemptStatus =
  | 'created'
  | 'inflight'
  | 'succeeded'
  | 'failed'
  | 'timeout'
  | 'cancelled';

export type PlanAttemptTerminalStatus = Extract<
  PlanAttemptStatus,
  'succeeded' | 'failed' | 'timeout' | 'cancelled'
>;

export type PlanAttemptMetadataValue = string | number | boolean | null;

export interface PlanAttemptRecord {
  attemptId: string;
  scope: PlanAttemptScope;
  targetId: string;
  assignmentId?: string;
  todoId?: string;
  sequence: number;
  status: PlanAttemptStatus;
  reason?: string;
  error?: string;
  evidenceIds: string[];
  metadata?: Record<string, PlanAttemptMetadataValue>;
  createdAt: number;
  startedAt?: number;
  endedAt?: number;
  updatedAt: number;
}

export interface PlanReview {
  status: 'approved' | 'rejected' | 'skipped';
  reviewer?: string;
  reason?: string;
  reviewedAt: number;
}

export type PlanRuntimeVersion = 'classic' | 'deep_v1';

export type PlanAcceptanceSummary = 'pending' | 'partial' | 'passed' | 'failed';

export interface PlanRuntimeAcceptance {
  criteria: AcceptanceCriterion[];
  summary: PlanAcceptanceSummary;
  updatedAt: number;
}

export interface PlanRuntimeReviewState {
  round: number;
  state: 'idle' | 'running' | 'accepted' | 'rejected';
  lastReviewedAt?: number;
}

export interface PlanRuntimeReplanState {
  state: 'none' | 'required' | 'awaiting_confirmation' | 'applied';
  reason?: string;
  updatedAt?: number;
}

export interface PlanRuntimeWaitState {
  state: 'none' | 'external_waiting';
  reasonCode?: string;
  updatedAt?: number;
}

export interface PlanRuntimePhaseState {
  state: 'idle' | 'running' | 'awaiting_next_phase' | 'completed';
  currentIndex?: number;
  currentTitle?: string;
  nextIndex?: number;
  nextTitle?: string;
  remainingPhases: string[];
  continuationIntent: 'continue' | 'stop';
  updatedAt?: number;
}

export interface PlanRuntimeTerminationState {
  snapshotId?: string;
  reason?: string;
  updatedAt?: number;
}

/**
 * 执行计划的细粒度运行态（三层状态架构的第 2 层）
 *
 * 与 Mission.status（宏观阶段）互补，记录执行过程中的微观状态。
 * 由 PlanLedgerService.updateRuntimeState() 在关键链路推进。
 *
 * @see mission/types.ts 中的「三层状态架构说明」
 */
export interface PlanRuntimeState {
  acceptance: PlanRuntimeAcceptance;
  review: PlanRuntimeReviewState;
  replan: PlanRuntimeReplanState;
  wait: PlanRuntimeWaitState;
  phase: PlanRuntimePhaseState;
  termination: PlanRuntimeTerminationState;
}

export interface PlanLinks {
  assignmentIds: string[];
  todoIds: string[];
}

export interface PlanItem {
  itemId: string;
  title: string;
  owner: PlanItemOwner;
  category?: string;
  dependsOn: string[];
  scopeHints?: string[];
  targetFiles?: string[];
  requiresModification?: boolean;
  status: PlanItemStatus;
  progress: number;
  assignmentId?: string;
  todoIds: string[];
  todoStatuses: Record<string, PlanTodoStatus>;
  createdAt: number;
  updatedAt: number;
}

export interface PlanRecord {
  planId: string;
  sessionId: string;
  missionId?: string;
  turnId: string;
  schemaVersion: number;
  runtimeVersion: PlanRuntimeVersion;
  revision: number;
  version: number;
  parentPlanId?: string;
  mode: PlanMode;
  status: PlanStatus;
  source: 'orchestrator';
  promptDigest: string;
  summary: string;
  analysis?: string;
  constraints: string[];
  riskLevel?: 'low' | 'medium' | 'high' | 'critical';
  review?: PlanReview;
  runtime: PlanRuntimeState;
  formattedPlan?: string;
  items: PlanItem[];
  attempts: PlanAttemptRecord[];
  links: PlanLinks;
  createdAt: number;
  updatedAt: number;
}

export interface PlanIndexEntry {
  planId: string;
  sessionId: string;
  missionId?: string;
  turnId: string;
  schemaVersion: number;
  runtimeVersion: PlanRuntimeVersion;
  revision: number;
  version: number;
  status: PlanStatus;
  mode: PlanMode;
  summary: string;
  createdAt: number;
  updatedAt: number;
}

export interface CreatePlanDraftInput {
  sessionId: string;
  turnId: string;
  missionId?: string;
  mode: PlanMode;
  prompt: string;
  summary?: string;
  analysis?: string;
  acceptanceCriteria?: string[];
  constraints?: string[];
  riskLevel?: 'low' | 'medium' | 'high' | 'critical';
  formattedPlan?: string;
}

export interface DispatchPlanItemInput {
  itemId: string;
  title: string;
  worker: WorkerSlot;
  category?: string;
  dependsOn?: string[];
  scopeHints?: string[];
  targetFiles?: string[];
  requiresModification?: boolean;
}

export interface PlanAttemptStartInput {
  scope: PlanAttemptScope;
  targetId?: string;
  assignmentId?: string;
  todoId?: string;
  reason?: string;
  metadata?: Record<string, PlanAttemptMetadataValue>;
}

export interface PlanAttemptCompleteInput extends PlanAttemptStartInput {
  status: PlanAttemptTerminalStatus;
  error?: string;
  evidenceIds?: string[];
}

export interface PlanMutationOptions {
  /**
   * 可选的 CAS 语义：仅当当前 revision 与 expectedRevision 一致时才允许写入。
   * 用于避免跨调用方并发写覆盖。
   */
  expectedRevision?: number;
  /**
   * 审计上下文：记录该次写入的调用来源，便于定位冲突或非法迁移。
   */
  auditReason?: string;
}

export interface PlanLedgerSnapshot {
  activePlan: PlanRecord | null;
  plans: PlanRecord[];
}
