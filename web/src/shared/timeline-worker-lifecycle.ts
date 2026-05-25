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
