const WORKER_SLOT_SET = new Set(['claude', 'codex', 'gemini']);

function normalizeNonEmptyString(value: unknown): string {
  if (typeof value !== 'string') {
    return '';
  }
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : '';
}

function normalizeWorkerSlot(value: unknown): string {
  const normalized = normalizeNonEmptyString(value).toLowerCase();
  return WORKER_SLOT_SET.has(normalized) ? normalized : '';
}

function resolveSubTaskCardMetadata(
  metadata: Record<string, unknown> | undefined,
): Record<string, unknown> | undefined {
  const subTaskCard = metadata?.subTaskCard;
  return subTaskCard && typeof subTaskCard === 'object' && !Array.isArray(subTaskCard)
    ? subTaskCard as Record<string, unknown>
    : undefined;
}

export function resolveTimelineDispatchWaveId(
  metadata: Record<string, unknown> | undefined,
): string {
  const explicitDispatchWaveId = normalizeNonEmptyString(metadata?.dispatchWaveId);
  if (explicitDispatchWaveId) {
    return explicitDispatchWaveId;
  }
  const subTaskCard = resolveSubTaskCardMetadata(metadata);
  const nestedDispatchWaveId = normalizeNonEmptyString(subTaskCard?.dispatchWaveId);
  if (nestedDispatchWaveId) {
    return nestedDispatchWaveId;
  }
  return normalizeNonEmptyString(metadata?.missionId) || normalizeNonEmptyString(subTaskCard?.missionId);
}

export function resolveTimelineWorkerCardId(
  metadata: Record<string, unknown> | undefined,
): string {
  const explicitWorkerCardId = normalizeNonEmptyString(metadata?.workerCardId);
  if (explicitWorkerCardId) {
    return explicitWorkerCardId;
  }
  const subTaskCard = resolveSubTaskCardMetadata(metadata);
  return normalizeNonEmptyString(subTaskCard?.workerCardId);
}

export function resolveTimelineTaskCardScopeId(
  metadata: Record<string, unknown> | undefined,
): string {
  const requestId = normalizeNonEmptyString(metadata?.requestId);
  if (requestId) {
    return requestId;
  }
  const subTaskCard = resolveSubTaskCardMetadata(metadata);
  const subTaskRequestId = normalizeNonEmptyString(subTaskCard?.requestId);
  if (subTaskRequestId) {
    return subTaskRequestId;
  }
  return normalizeNonEmptyString(metadata?.missionId);
}

export function buildTimelineAssignmentTaskKey(
  assignmentKey: string,
  scopeId?: string,
): string {
  const normalizedAssignment = normalizeNonEmptyString(assignmentKey);
  if (!normalizedAssignment) {
    return '';
  }
  const normalizedScope = normalizeNonEmptyString(scopeId);
  return normalizedScope
    ? `assign:${normalizedAssignment}@${normalizedScope}`
    : `assign:${normalizedAssignment}`;
}

export function resolveTimelineTaskResultKey(
  metadata: Record<string, unknown> | undefined,
): string {
  const scopeId = resolveTimelineTaskCardScopeId(metadata);
  const assignmentId = normalizeNonEmptyString(metadata?.assignmentId);
  if (assignmentId) {
    return buildTimelineAssignmentTaskKey(assignmentId, scopeId);
  }
  const subTaskId = normalizeNonEmptyString(metadata?.subTaskId);
  if (subTaskId) {
    return buildTimelineAssignmentTaskKey(subTaskId, scopeId);
  }
  const subTaskCard = resolveSubTaskCardMetadata(metadata);
  const subTaskCardId = normalizeNonEmptyString(subTaskCard?.id);
  if (subTaskCardId) {
    return buildTimelineAssignmentTaskKey(subTaskCardId, scopeId);
  }
  return normalizeNonEmptyString(metadata?.cardId);
}

export function resolveTimelineWorkerLaneId(
  metadata: Record<string, unknown> | undefined,
  fallbackWorker?: unknown,
): string {
  const explicitLaneId = normalizeNonEmptyString(metadata?.laneId);
  if (explicitLaneId) {
    return explicitLaneId;
  }

  const subTaskCard = resolveSubTaskCardMetadata(metadata);
  const nestedLaneId = normalizeNonEmptyString(subTaskCard?.laneId);
  if (nestedLaneId) {
    return nestedLaneId;
  }
  const dispatchWaveId = resolveTimelineDispatchWaveId(metadata);
  const worker = normalizeWorkerSlot(
    metadata?.worker
      || metadata?.assignedWorker
      || subTaskCard?.worker
      || fallbackWorker,
  );

  if (dispatchWaveId && worker) {
    return `${dispatchWaveId}:${worker}`;
  }

  return '';
}

export function resolveTimelineWorkerLifecycleKey(
  metadata: Record<string, unknown> | undefined,
  options: { fallbackWorker?: unknown } = {},
): string {
  const laneId = resolveTimelineWorkerLaneId(metadata, options.fallbackWorker);
  if (laneId) {
    return `lane:${laneId}`;
  }

  const workerCardId = resolveTimelineWorkerCardId(metadata);
  if (workerCardId) {
    return `worker-card:${workerCardId}`;
  }

  return resolveTimelineTaskResultKey(metadata);
}
