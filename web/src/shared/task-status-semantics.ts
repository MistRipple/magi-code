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

export type TaskSemanticDisplayStatus = Exclude<TaskSemanticStatus, 'skipped'>;

export interface TaskStatusLike {
  status?: unknown;
  approvalStatus?: unknown;
  reviewStatus?: unknown;
  executionBlocker?: unknown;
  blockedReason?: unknown;
}

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

export function mapTaskSemanticStatusToDisplayStatus(
  status: TaskSemanticStatus | null | undefined,
): TaskSemanticDisplayStatus | null {
  if (!status) {
    return null;
  }
  return status === 'skipped' ? 'completed' : status;
}
