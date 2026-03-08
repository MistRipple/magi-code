import type { WorkerSlot } from '../../types';

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
  version: number;
  parentPlanId?: string;
  mode: PlanMode;
  status: PlanStatus;
  source: 'orchestrator';
  promptDigest: string;
  summary: string;
  analysis?: string;
  acceptanceCriteria: string[];
  constraints: string[];
  riskLevel?: 'low' | 'medium' | 'high' | 'critical';
  review?: PlanReview;
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

export interface PlanLedgerSnapshot {
  activePlan: PlanRecord | null;
  plans: PlanRecord[];
}
