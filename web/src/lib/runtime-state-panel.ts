export interface RuntimePanelVisibilityInput {
  status?: string | null;
  isProcessing: boolean;
  activeAssignmentCount: number;
}

export interface RuntimePanelStatusInput {
  status?: string | null;
  isProcessing: boolean;
}

export interface RuntimeTaskProgressSnapshot {
  requiredTotal?: number;
  failedRequired?: number;
  runningOrPendingRequired?: number;
}

export interface RuntimeTaskProgress {
  completed: number;
  failed: number;
  running: number;
  total: number;
  percent: number;
}

export function shouldShowRuntimePanel(input: RuntimePanelVisibilityInput): boolean {
  const status = input.status?.trim();
  if (status === 'completed' || status === 'cancelled') {
    return false;
  }
  if (input.isProcessing || input.activeAssignmentCount > 0) {
    return true;
  }
  return status === 'running'
    || status === 'waiting'
    || status === 'paused'
    || status === 'blocked'
    || status === 'failed';
}

export function resolveRuntimePanelStatus(input: RuntimePanelStatusInput): string | undefined {
  const status = input.status?.trim() || undefined;
  if (
    input.isProcessing
    && (!status || status === 'idle' || status === 'completed' || status === 'failed' || status === 'cancelled')
  ) {
    return 'running';
  }
  return status;
}

export function runtimeAssignmentIsActive(status: string | undefined): boolean {
  switch (status?.trim().toLowerCase()) {
    case 'running':
    case 'in_progress':
    case 'pending':
    case 'waiting':
    case 'waiting_deps':
    case 'awaiting_approval':
    case 'review_required':
    case 'paused':
    case 'blocked':
      return true;
    default:
      return false;
  }
}

export function shouldShowRuntimePhase(status: string | undefined, phase: string | undefined): boolean {
  const normalizedStatus = status?.trim();
  const normalizedPhase = phase?.trim();
  return Boolean(
    normalizedPhase
    && normalizedPhase !== 'idle'
    && normalizedPhase !== normalizedStatus,
  );
}

export function shouldShowRuntimeBudget(warningLevel: string | undefined): boolean {
  return warningLevel === 'notice' || warningLevel === 'warning' || warningLevel === 'danger';
}

export function shouldShowRuntimeCache(health: string | undefined): boolean {
  return health === 'degraded';
}

export function resolveRuntimeTaskProgress(
  snapshot: RuntimeTaskProgressSnapshot | null | undefined,
): RuntimeTaskProgress | null {
  if (!snapshot) return null;
  const total = Math.max(0, snapshot.requiredTotal ?? 0);
  const failed = Math.max(0, snapshot.failedRequired ?? 0);
  const running = Math.max(0, snapshot.runningOrPendingRequired ?? 0);
  const completed = Math.max(0, total - failed - running);
  const percent = total > 0 ? Math.round((completed / total) * 100) : 0;
  return { completed, failed, running, total, percent };
}
