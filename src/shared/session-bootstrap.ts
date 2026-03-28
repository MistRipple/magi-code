import type { StandardMessage } from '../protocol/message-protocol';
import { canBindRequestPlaceholder } from './request-placeholder-binding';
import {
  buildSessionTimelineProjection,
  type SessionTimelineProjection,
  type SessionTimelineProjectionMessage,
} from '../session/session-timeline-projection';

export interface BootstrapQueuedMessage {
  id: string;
  content: string;
  createdAt: number;
}

export interface SessionBootstrapSnapshot {
  sessionId: string;
  sessions: unknown[];
  state: unknown;
  timelineProjection: SessionTimelineProjection;
  notifications?: {
    sessionId: string;
    notifications: unknown;
  };
  queuedMessages: BootstrapQueuedMessage[];
  orchestratorRuntimeState?: unknown;
  /** 当前 session 的执行链摘要（停止/继续/放弃按钮状态） */
  executionChainSummary?: ExecutionChainBootstrapSummary;
}

/**
 * 执行链前端摘要（browser-safe，纯基础类型）
 *
 * 供前端判断当前 session 是否存在可恢复的执行链，
 * 从而决定渲染"继续"或"放弃"按钮。
 */
export interface ExecutionChainBootstrapSummary {
  /** 是否存在可恢复的执行链 */
  hasRecoverableChain: boolean;
  /** 可恢复链的 ID（如果有） */
  recoverableChainId?: string;
  /** 可恢复链关联的 mission 标题 */
  recoverableChainTitle?: string;
  /** 最近完成/失败/取消的链状态 */
  lastChainStatus?: string;
}

export interface SessionBootstrapSourceSession {
  id: string;
  updatedAt: number;
  projectionMessages: readonly SessionTimelineProjectionMessage[];
}

function resolveMessageSessionId(message: Pick<StandardMessage, 'metadata'>): string {
  const metadata = message.metadata as Record<string, unknown> | undefined;
  return typeof metadata?.sessionId === 'string' ? metadata.sessionId.trim() : '';
}

function resolveRequestId(
  message: Pick<SessionTimelineProjectionMessage, 'metadata'>,
): string {
  const metadata = message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)
    ? message.metadata as Record<string, unknown>
    : undefined;
  return typeof metadata?.requestId === 'string' ? metadata.requestId.trim() : '';
}

function isPlaceholderMessage(
  message: Pick<SessionTimelineProjectionMessage, 'metadata'>,
): boolean {
  const metadata = message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)
    ? message.metadata as Record<string, unknown>
    : undefined;
  return metadata?.isPlaceholder === true;
}

function collapseResolvedRequestPlaceholders(
  messages: SessionTimelineProjectionMessage[],
): SessionTimelineProjectionMessage[] {
  const resolvedRequestIds = new Set<string>();
  for (const message of messages) {
    const requestId = resolveRequestId(message);
    if (!requestId) {
      continue;
    }
    if (canBindRequestPlaceholder({
      type: message.type,
      source: message.source,
      metadata: message.metadata,
    })) {
      resolvedRequestIds.add(requestId);
    }
  }

  if (resolvedRequestIds.size === 0) {
    return messages;
  }

  return messages.filter((message) => {
    const requestId = resolveRequestId(message);
    if (!requestId || !resolvedRequestIds.has(requestId)) {
      return true;
    }
    return !isPlaceholderMessage(message);
  });
}

export function buildSessionBootstrapTimelineProjection(input: {
  session: SessionBootstrapSourceSession;
  liveMessages?: readonly StandardMessage[];
}): SessionTimelineProjection {
  const mergedMessages = new Map<string, SessionTimelineProjectionMessage>();
  let updatedAt = input.session.updatedAt;

  for (const message of input.session.projectionMessages) {
    if (!message?.id || typeof message.id !== 'string') {
      continue;
    }
    // 不在此处做 structuredClone：调用方 (getLiveSessionTimelineProjection) 会对整个投影结果做一次统一深拷贝
    mergedMessages.set(
      message.id,
      message as unknown as SessionTimelineProjectionMessage,
    );
  }

  for (const message of input.liveMessages ?? []) {
    if (!message?.id || resolveMessageSessionId(message) !== input.session.id) {
      continue;
    }
    mergedMessages.set(
      message.id,
      message as unknown as SessionTimelineProjectionMessage,
    );
    const snapshotUpdatedAt = typeof message.updatedAt === 'number' && Number.isFinite(message.updatedAt)
      ? Math.floor(message.updatedAt)
      : (
        typeof message.timestamp === 'number' && Number.isFinite(message.timestamp)
          ? Math.floor(message.timestamp)
          : updatedAt
      );
    updatedAt = Math.max(updatedAt, snapshotUpdatedAt);
  }

  return buildSessionTimelineProjection({
    id: input.session.id,
    updatedAt,
    messages: collapseResolvedRequestPlaceholders(Array.from(mergedMessages.values())),
  });
}
