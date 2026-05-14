export const SYSTEM_AGENT_SET = new Set(['orchestrator', 'auxiliary', 'system']);

function normalizeNonEmptyString(value: unknown): string {
  if (typeof value !== 'string') {
    return '';
  }
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : '';
}

/**
 * 归一化 worker 标识
 * 动态架构下：任何非系统 agent 的非空字符串都是合法的 worker id
 *
 * 此函数为唯一规范实现，所有模块（前端/后端）统一导入。
 * 返回空字符串表示非 worker 标识。
 */
export function normalizeWorkerSlot(value: unknown): string {
  if (typeof value !== 'string') {
    return '';
  }
  const normalized = value.trim();
  if (!normalized || SYSTEM_AGENT_SET.has(normalized.toLowerCase())) {
    return '';
  }
  return normalized;
}

function resolveSubTaskCardMetadata(
  metadata: Record<string, unknown> | undefined,
): Record<string, unknown> | undefined {
  const subTaskCard = metadata?.subTaskCard;
  return subTaskCard && typeof subTaskCard === 'object' && !Array.isArray(subTaskCard)
    ? subTaskCard as Record<string, unknown>
    : undefined;
}

/**
 * 解析时间线消息归属的 worker Tab 聚合键。
 *
 * P1 身份契约：单一真源 `metadata.workerTabId`（由 canonical projection 写入，本质为 roleId）。
 * 旧 legacy 字段（worker / assignedWorker / agent / subTaskCard.worker / source）不再参与解析——
 * 这些字段在 role-first 架构里语义模糊（有时是实例 id、有时是 role id、有时是显示名），
 * 继续兜底会让前端身份判断不可预测。没有 workerTabId 的消息视为"非 worker 消息"。
 */
export function resolveTimelineWorkerId(
  metadata: Record<string, unknown> | undefined,
): string {
  return normalizeWorkerSlot(metadata?.workerTabId);
}

/**
 * 解析时间线消息归属的 worker 实例 id（card 身份）。
 *
 * 单一真源：`metadata.workerId`（由 canonical projection 写入）。
 * 返回空字符串表示消息不隶属于任何 worker 实例（例如 orchestrator 内生消息）。
 */
export function resolveTimelineWorkerInstanceId(
  metadata: Record<string, unknown> | undefined,
): string {
  return normalizeWorkerSlot(metadata?.workerId);
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
  return normalizeNonEmptyString(metadata?.executionGroupId)
    || normalizeNonEmptyString(subTaskCard?.executionGroupId);
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
  return normalizeNonEmptyString(metadata?.executionGroupId);
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
  const worker = resolveTimelineWorkerId(metadata);

  if (dispatchWaveId && worker) {
    return `${dispatchWaveId}:${worker}`;
  }

  return '';
}
