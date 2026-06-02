function readString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
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
  return (
    readString(payload.error_code)
    || readString(payload.errorCode)
    || readString(payload.code)
  ).toLowerCase();
}

export function toolPayloadStatus(content: unknown): string {
  const payload = parseToolPayloadRecord(content);
  if (!payload) return '';
  return readString(payload.status).toLowerCase();
}

export function isStructuredToolErrorPayload(content: unknown): boolean {
  if (!toolPayloadErrorCode(content)) {
    return false;
  }
  const status = toolPayloadStatus(content);
  return !status || !['succeeded', 'success', 'ok'].includes(status);
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
