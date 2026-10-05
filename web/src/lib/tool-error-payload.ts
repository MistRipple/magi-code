function readString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

export const ACCESS_MODE_APPROVAL_ERROR_CODES = [
  'tool_policy_needs_approval',
  'skill_tool_policy_needs_approval',
  'skill_tool_needs_approval',
  'external_tool_needs_approval',
  'tool_safety_needs_approval',
  'tool_approval_denied',
];

export type ToolPolicyRestrictionKind =
  | 'approval_required'
  | 'read_only'
  | 'path_scope'
  | 'tool_scope'
  | 'policy';

export interface ToolApprovalPayload {
  approvalId: string;
  sessionId: string;
  taskId: string;
  turnId: string;
  toolCallId: string;
  toolName: string;
  reason: string;
  requestedAt?: number;
  /** 由子代理发起时的代理身份；主线请求为空。 */
  agent?: { role: string; title: string } | null;
}

export function parseToolPayloadRecord(content: unknown): Record<string, unknown> | null {
  if (content && typeof content === 'object' && !Array.isArray(content)) {
    return content as Record<string, unknown>;
  }
  if (typeof content !== 'string') {
    return null;
  }
  const trimmed = content.trim();
  if (!trimmed.startsWith('{')) {
    return null;
  }
  try {
    const parsed = JSON.parse(trimmed);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : null;
  } catch {
    return null;
  }
}

export function toolPayloadErrorCode(content: unknown): string {
  const payload = parseToolPayloadRecord(content);
  if (!payload) return '';
  return readString(payload.error_code).toLowerCase();
}

export function toolPayloadStatus(content: unknown): string {
  const payload = parseToolPayloadRecord(content);
  if (!payload) return '';
  return readString(payload.status).toLowerCase();
}

export function toolPayloadRestrictionKind(content: unknown): ToolPolicyRestrictionKind | '' {
  const payload = parseToolPayloadRecord(content);
  if (!payload) return '';
  const kind = readString(payload.restriction_kind);
  return ['approval_required', 'read_only', 'path_scope', 'tool_scope', 'policy'].includes(kind)
    ? kind as ToolPolicyRestrictionKind
    : '';
}

export function isAccessModeApprovalErrorPayload(content: unknown): boolean {
  const errorCode = toolPayloadErrorCode(content);
  return ACCESS_MODE_APPROVAL_ERROR_CODES.some((pattern) => errorCode.includes(pattern));
}

export function parseToolApprovalPayload(content: unknown): ToolApprovalPayload | null {
  const payload = parseToolPayloadRecord(content);
  if (!payload || toolPayloadStatus(payload) !== 'awaiting_approval') {
    return null;
  }
  const approval = payload.approval && typeof payload.approval === 'object' && !Array.isArray(payload.approval)
    ? payload.approval as Record<string, unknown>
    : {};
  // `approval` 是 PendingToolApproval 的 camelCase 序列化。
  const approvalId = readString(approval.approvalId);
  const sessionId = readString(approval.sessionId);
  if (!approvalId || !sessionId) {
    return null;
  }
  const requestedAt = typeof approval.requestedAt === 'number' && Number.isFinite(approval.requestedAt)
    ? approval.requestedAt
    : undefined;
  const agentRecord = approval.agent && typeof approval.agent === 'object' && !Array.isArray(approval.agent)
    ? approval.agent as Record<string, unknown>
    : null;
  const agent = agentRecord && (readString(agentRecord.role) || readString(agentRecord.title))
    ? { role: readString(agentRecord.role), title: readString(agentRecord.title) }
    : null;
  return {
    approvalId,
    sessionId,
    taskId: readString(approval.taskId),
    turnId: readString(approval.turnId),
    toolCallId: readString(approval.toolCallId),
    toolName: readString(approval.toolName),
    reason: readString(approval.reason) || readString(payload.error),
    requestedAt,
    agent,
  };
}

export function isStructuredToolErrorPayload(content: unknown): boolean {
  if (!toolPayloadErrorCode(content)) {
    return false;
  }
  const status = toolPayloadStatus(content);
  return !status || !['succeeded', 'success', 'ok', 'completed', 'cancelled', 'canceled', 'killed', 'aborted'].includes(status);
}

export function publicToolPayloadMessage(content: unknown): string {
  if (!toolPayloadErrorCode(content)) {
    return '';
  }
  const payload = parseToolPayloadRecord(content);
  if (!payload) return '';
  return (
    readString(payload.error)
    || readString(payload.summary)
    || readString(payload.message)
  );
}
