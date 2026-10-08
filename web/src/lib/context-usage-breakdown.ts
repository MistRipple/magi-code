export const CONTEXT_USAGE_FIELDS = {
  conversationTokens: 'conversation_tokens',
  imageTokens: 'image_tokens',
  systemInstructionTokens: 'system_instruction_tokens',
  projectContextTokens: 'project_context_tokens',
  skillTokens: 'skill_tokens',
  contextReferenceTokens: 'context_reference_tokens',
  builtinToolTokens: 'builtin_tool_tokens',
  mcpToolTokens: 'mcp_tool_tokens',
  skillToolTokens: 'skill_tool_tokens',
  otherToolTokens: 'other_tool_tokens',
} as const;

export type ContextUsageBreakdown = Record<keyof typeof CONTEXT_USAGE_FIELDS, number>;

/** REST 和 SSE 共用同一契约解码；快照不完整时不拼接上一调用的分类。 */
export function normalizeContextUsageBreakdown(raw: unknown): ContextUsageBreakdown | undefined {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return undefined;
  const record = raw as Record<string, unknown>;
  const entries: Array<[string, number]> = [];
  for (const [target, source] of Object.entries(CONTEXT_USAGE_FIELDS)) {
    const value = record[source];
    if (typeof value !== 'number' || !Number.isFinite(value) || value < 0) return undefined;
    entries.push([target, Math.floor(value)]);
  }
  return Object.fromEntries(entries) as ContextUsageBreakdown;
}
