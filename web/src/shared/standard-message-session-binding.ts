import type { StandardMessage } from './protocol/message-protocol';

export interface StandardMessageSessionBinding {
  sessionId: string | null;
  source: 'metadata' | 'data' | 'trace' | 'none';
  metadataSessionId: string | null;
  traceSessionId: string | null;
  dataPayloadSessionId: string | null;
  hasConflict: boolean;
}

function normalizeSessionId(value: unknown): string | null {
  if (typeof value !== 'string') {
    return null;
  }
  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

function isConversationSessionId(value: string | null): value is string {
  return typeof value === 'string' && value.startsWith('session-');
}

export function resolveStandardMessageSessionBinding(
  message: Pick<StandardMessage, 'traceId' | 'metadata' | 'category' | 'data'>,
): StandardMessageSessionBinding {
  const dataPayload = message.data && typeof message.data === 'object'
    ? (message.data as { payload?: unknown }).payload
    : undefined;
  const rawDataPayloadSessionId = (() => {
    if (dataPayload && typeof dataPayload === 'object') {
      const payloadRecord = dataPayload as Record<string, unknown>;
      const direct = normalizeSessionId(payloadRecord.sessionId);
      if (direct) {
        return direct;
      }
      const state = payloadRecord.state;
      if (state && typeof state === 'object') {
        return normalizeSessionId((state as Record<string, unknown>).currentSessionId);
      }
    }
    return null;
  })();
  const rawMetadataSessionId = normalizeSessionId(message.metadata?.sessionId);
  const rawTraceSessionId = normalizeSessionId(message.traceId);
  const metadataSessionId = isConversationSessionId(rawMetadataSessionId) ? rawMetadataSessionId : null;
  const traceSessionId = isConversationSessionId(rawTraceSessionId) ? rawTraceSessionId : null;
  const dataPayloadSessionId = isConversationSessionId(rawDataPayloadSessionId)
    ? rawDataPayloadSessionId
    : null;

  if (metadataSessionId) {
    return {
      sessionId: metadataSessionId,
      source: 'metadata',
      metadataSessionId,
      traceSessionId: rawTraceSessionId,
      dataPayloadSessionId,
      hasConflict: Boolean(rawTraceSessionId && rawTraceSessionId !== metadataSessionId),
    };
  }

  if (dataPayloadSessionId) {
    return {
      sessionId: dataPayloadSessionId,
      source: 'data',
      metadataSessionId: rawMetadataSessionId,
      traceSessionId: rawTraceSessionId,
      dataPayloadSessionId,
      hasConflict: Boolean(
        (rawMetadataSessionId && rawMetadataSessionId !== dataPayloadSessionId)
        || (rawTraceSessionId && rawTraceSessionId !== dataPayloadSessionId),
      ),
    };
  }

  // traceId 只允许回退到真实对话会话。
  // worker 内部恢复会话使用 ses_*，绝不能再冒充用户对话会话。
  if (traceSessionId) {
    return {
      sessionId: traceSessionId,
      source: 'trace',
      metadataSessionId: rawMetadataSessionId,
      traceSessionId,
      dataPayloadSessionId,
      hasConflict: false,
    };
  }

  return {
    sessionId: null,
    source: 'none',
    metadataSessionId: rawMetadataSessionId,
    traceSessionId: rawTraceSessionId,
    dataPayloadSessionId,
    hasConflict: Boolean(rawMetadataSessionId && rawTraceSessionId && rawMetadataSessionId !== rawTraceSessionId),
  };
}
