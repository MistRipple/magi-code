export const UNKNOWN_TOOL_RUNTIME_STATUS = 'unknown';

export function normalizeToolRuntimeStatus(value: unknown): string {
  return typeof value === 'string' && value.trim()
    ? value.trim()
    : UNKNOWN_TOOL_RUNTIME_STATUS;
}
