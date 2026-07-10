export interface RuntimePanelVisibilityInput {
  status?: string | null;
  isProcessing: boolean;
  assignmentCount: number;
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
  if (input.isProcessing || input.assignmentCount > 0) {
    return true;
  }
  const status = input.status?.trim();
  return Boolean(status && status !== 'idle');
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
