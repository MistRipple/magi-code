import type { StandardMessage } from '../protocol/message-protocol';

export interface StandardMessageSessionBinding {
  sessionId: string | null;
  source: 'metadata' | 'trace' | 'none';
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

export function resolveStandardMessageSessionBinding(
  message: Pick<StandardMessage, 'traceId' | 'metadata' | 'category' | 'data'>,
): StandardMessageSessionBinding {
  const metadataSessionId = normalizeSessionId(message.metadata?.sessionId);
  const traceSessionId = normalizeSessionId(message.traceId);
  const dataPayloadSessionId = null;

  if (metadataSessionId) {
    return {
      sessionId: metadataSessionId,
      source: 'metadata',
      metadataSessionId,
      traceSessionId,
      dataPayloadSessionId,
      hasConflict: Boolean(traceSessionId && traceSessionId !== metadataSessionId),
    };
  }

  // traceId 在 Magi 系统中就是 sessionId，作为 metadata.sessionId 缺失时的唯一回退
  if (traceSessionId) {
    return {
      sessionId: traceSessionId,
      source: 'trace',
      metadataSessionId,
      traceSessionId,
      dataPayloadSessionId,
      hasConflict: false,
    };
  }

  return {
    sessionId: null,
    source: 'none',
    metadataSessionId,
    traceSessionId,
    dataPayloadSessionId,
    hasConflict: false,
  };
}
