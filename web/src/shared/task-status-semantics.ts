/**
 * 前端任务状态语义的最小自包含定义。
 * 这里保留的是任务运行状态解析，不再把旧 todo 语义作为正式主语义。
 */

export type TaskRuntimeStatus =
  | 'pending'
  | 'running'
  | 'completed'
  | 'failed'
  | 'skipped'
  | 'cancelled';

export type TaskExecutionBlocker = 'dependencies' | 'contracts' | 'approval';
export type ApprovalStatus = 'pending' | 'approved' | 'rejected';

export interface TaskStatusSnapshot {
  status: TaskRuntimeStatus;
  approvalStatus?: ApprovalStatus;
  reviewStatus?: 'approved' | 'needs_revision' | 'rejected';
  executionBlocker?: TaskExecutionBlocker;
  blockedReason?: string;
}

export type TaskSemanticStatus =
  | 'pending'
  | 'review_required'
  | 'awaiting_approval'
  | 'running'
  | 'completed'
  | 'failed'
  | 'skipped'
  | 'blocked'
  | 'cancelled';

export type TaskAggregateViewStatus =
  | 'pending'
  | 'running'
  | 'paused'
  | 'completed'
  | 'failed'
  | 'cancelled';

export type TaskSemanticDisplayStatus = Exclude<TaskSemanticStatus, 'skipped'>;

export interface TaskStatusLike {
  status?: unknown;
  approvalStatus?: unknown;
  reviewStatus?: unknown;
  executionBlocker?: unknown;
  blockedReason?: unknown;
}

export interface TaskSemanticStatusSummary {
  total: number;
  dominantStatus: TaskSemanticStatus | null;
  assignmentStatus: TaskSemanticStatus | 'paused';
  awaitingApprovalCount: number;
  reviewRequiredCount: number;
  blockedCount: number;
  completedCount: number;
  failedCount: number;
  runningCount: number;
  settledCount: number;
  cancelledCount: number;
}

const TASK_STATUS_PRIORITY: readonly TaskSemanticStatus[] = [
  'running',
  'awaiting_approval',
  'review_required',
  'blocked',
  'failed',
  'pending',
  'cancelled',
  'completed',
  'skipped',
];

function normalizeStatus(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

export function resolveTaskSemanticStatus(input: TaskStatusLike): TaskSemanticStatus | null {
  const reviewStatus = normalizeStatus(input.reviewStatus);
  if (reviewStatus === 'needs_revision') {
    return 'review_required';
  }

  const status = normalizeStatus(input.status);
  const approvalStatus = normalizeStatus(input.approvalStatus);
  const executionBlocker = normalizeStatus(input.executionBlocker);
  if (
    approvalStatus === 'pending'
    && (status === 'blocked' || status === 'pending' || status === 'awaiting_approval' || executionBlocker === 'approval')
  ) {
    return 'awaiting_approval';
  }

  if (
    status === 'pending'
    && (executionBlocker === 'dependencies' || executionBlocker === 'contracts')
  ) {
    return 'blocked';
  }

  switch (status) {
    case 'awaiting_approval':
      return 'awaiting_approval';
    case 'review_required':
      return 'review_required';
    case 'running':
    case 'in_progress':
    case 'executing':
      return 'running';
    case 'blocked':
      return 'blocked';
    case 'failed':
      return 'failed';
    case 'pending':
      return 'pending';
    case 'ready':
    case 'paused':
    case 'planning':
    case 'waiting_deps':
      return 'pending';
    case 'cancelled':
      return 'cancelled';
    case 'completed':
      return 'completed';
    case 'skipped':
      return 'skipped';
    default:
      return null;
  }
}

export function pickHighestPriorityTaskStatus(
  statuses: Iterable<TaskSemanticStatus | null | undefined>,
): TaskSemanticStatus | null {
  const normalized = new Set<TaskSemanticStatus>();
  for (const status of statuses) {
    if (status) {
      normalized.add(status);
    }
  }
  for (const candidate of TASK_STATUS_PRIORITY) {
    if (normalized.has(candidate)) {
      return candidate;
    }
  }
  return null;
}

export function deriveDominantTaskSemanticStatus(
  statuses: Iterable<TaskSemanticStatus | null | undefined>,
): TaskSemanticStatus | null {
  return pickHighestPriorityTaskStatus(statuses);
}

export function mapTaskSemanticStatusToDisplayStatus(
  status: TaskSemanticStatus | null | undefined,
): TaskSemanticDisplayStatus | null {
  if (!status) {
    return null;
  }
  return status === 'skipped' ? 'completed' : status;
}

export function deriveDisplayTaskStatusFromSemanticStatuses(
  statuses: Iterable<TaskSemanticStatus | null | undefined>,
): TaskSemanticDisplayStatus | null {
  return mapTaskSemanticStatusToDisplayStatus(deriveDominantTaskSemanticStatus(statuses));
}

export function isTaskSemanticRunningStatus(status: TaskSemanticStatus | null | undefined): boolean {
  return status === 'running' || status === 'pending';
}

export function isTaskSemanticPausedStatus(status: TaskSemanticStatus | null | undefined): boolean {
  return status === 'awaiting_approval' || status === 'review_required' || status === 'blocked';
}

export function isTaskSemanticCompletedStatus(status: TaskSemanticStatus | null | undefined): boolean {
  return status === 'completed' || status === 'skipped';
}

export function isTaskSemanticFailedStatus(status: TaskSemanticStatus | null | undefined): boolean {
  return status === 'failed' || status === 'cancelled';
}

export function isTaskSemanticProgressSettledStatus(status: TaskSemanticStatus | null | undefined): boolean {
  return isTaskSemanticCompletedStatus(status) || status === 'cancelled';
}

export function deriveTaskViewStatusFromSemanticStatuses(
  statuses: Iterable<TaskSemanticStatus | null | undefined>,
  fallbackMissionStatus: TaskAggregateViewStatus,
): TaskAggregateViewStatus {
  const normalizedStatuses = Array.from(statuses).filter((status): status is TaskSemanticStatus => Boolean(status));
  if (normalizedStatuses.length === 0) {
    return fallbackMissionStatus;
  }

  const semanticStatus = pickHighestPriorityTaskStatus(normalizedStatuses);
  if (isTaskSemanticRunningStatus(semanticStatus)) {
    return 'running';
  }
  if (isTaskSemanticPausedStatus(semanticStatus)) {
    return 'paused';
  }
  if (semanticStatus === 'failed') {
    return 'failed';
  }
  if (normalizedStatuses.every((status) => status === 'cancelled')) {
    return 'cancelled';
  }
  if (normalizedStatuses.every((status) => isTaskSemanticProgressSettledStatus(status))) {
    return 'completed';
  }

  return fallbackMissionStatus;
}

export function deriveAssignmentStatusFromSemanticStatuses(
  statuses: Iterable<TaskSemanticStatus | null | undefined>,
  options: { fallbackTaskStatus?: TaskAggregateViewStatus | null | undefined } = {},
): TaskSemanticStatus | 'paused' {
  const normalizedStatuses = Array.from(statuses).filter((status): status is TaskSemanticStatus => Boolean(status));
  if (normalizedStatuses.some((status) => status === 'review_required')) {
    return 'review_required';
  }
  if (normalizedStatuses.some((status) => status === 'awaiting_approval')) {
    return 'awaiting_approval';
  }
  if (normalizedStatuses.some((status) => isTaskSemanticRunningStatus(status))) {
    return 'running';
  }
  if (normalizedStatuses.some((status) => status === 'blocked')) {
    return 'blocked';
  }
  if (normalizedStatuses.some((status) => status === 'failed')) {
    return 'failed';
  }
  if (normalizedStatuses.length > 0 && normalizedStatuses.every((status) => status === 'cancelled')) {
    return 'cancelled';
  }
  if (normalizedStatuses.length > 0 && normalizedStatuses.every((status) => isTaskSemanticProgressSettledStatus(status))) {
    return 'completed';
  }
  if (options.fallbackTaskStatus === 'paused') {
    return 'paused';
  }
  if (options.fallbackTaskStatus === 'cancelled') {
    return 'cancelled';
  }
  return 'pending';
}

export function summarizeTaskSemanticStatuses(
  statuses: Iterable<TaskSemanticStatus | null | undefined>,
  options: { fallbackTaskStatus?: TaskAggregateViewStatus | null | undefined } = {},
): TaskSemanticStatusSummary {
  const normalizedStatuses = Array.from(statuses).filter((status): status is TaskSemanticStatus => Boolean(status));
  let awaitingApprovalCount = 0;
  let reviewRequiredCount = 0;
  let blockedCount = 0;
  let completedCount = 0;
  let failedCount = 0;
  let runningCount = 0;
  let settledCount = 0;
  let cancelledCount = 0;

  for (const status of normalizedStatuses) {
    if (status === 'awaiting_approval') {
      awaitingApprovalCount++;
    }
    if (status === 'review_required') {
      reviewRequiredCount++;
    }
    if (status === 'blocked') {
      blockedCount++;
    }
    if (isTaskSemanticCompletedStatus(status)) {
      completedCount++;
    }
    if (isTaskSemanticFailedStatus(status)) {
      failedCount++;
    }
    if (isTaskSemanticRunningStatus(status)) {
      runningCount++;
    }
    if (isTaskSemanticProgressSettledStatus(status)) {
      settledCount++;
    }
    if (status === 'cancelled') {
      cancelledCount++;
    }
  }

  return {
    total: normalizedStatuses.length,
    dominantStatus: pickHighestPriorityTaskStatus(normalizedStatuses),
    assignmentStatus: deriveAssignmentStatusFromSemanticStatuses(normalizedStatuses, options),
    awaitingApprovalCount,
    reviewRequiredCount,
    blockedCount,
    completedCount,
    failedCount,
    runningCount,
    settledCount,
    cancelledCount,
  };
}
