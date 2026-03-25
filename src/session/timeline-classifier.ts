import type {
  StandardMessage,
  ContentBlock,
  MessageVisibility,
  MessageSource,
} from '../protocol/message-protocol';
import { MessageCategory, MessageType } from '../protocol/message-protocol';
import {
  resolveNotificationPresentation,
  shouldPersistNotificationRecord,
} from '../shared/notification-presentation';
import {
  isTimelineWorkerLifecycleMessageType,
  resolveTimelineWorkerVisibility,
} from '../shared/timeline-presentation';
import {
  resolveTimelineDispatchWaveId,
  resolveTimelineWorkerCardId,
  resolveTimelineWorkerLifecycleKey,
  resolveTimelineWorkerLaneId,
} from '../shared/timeline-worker-lifecycle';
import { expandRenderableTimelineMessages } from '../shared/timeline-message-fragmentation';
import type {
  SessionNotificationRecord,
  TimelineMessageLike,
  TimelineRecord,
  TimelineRecordKind,
} from './timeline-record';
import { resolveTimelineAnchorTimestampFromMetadata, resolveTimelineCardStreamSeqFromMetadata, resolveTimelineEventSeqFromMetadata, resolveTimelineSortTimestamp, resolveTimelineVersionFromMetadata } from '../shared/timeline-ordering';
import type { WorkerSlot } from '../types/agent-types';

const WORKER_SLOTS = new Set<WorkerSlot>(['claude', 'codex', 'gemini']);

export type SessionPersistenceTarget = 'timeline' | 'notification' | 'ignore';

function normalizeWorkerSlot(value: unknown): WorkerSlot | undefined {
  if (typeof value !== 'string') {
    return undefined;
  }
  const normalized = value.trim().toLowerCase();
  return WORKER_SLOTS.has(normalized as WorkerSlot)
    ? normalized as WorkerSlot
    : undefined;
}

function resolveMetadata(message: { metadata?: unknown }): Record<string, unknown> | undefined {
  return message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)
    ? message.metadata as Record<string, unknown>
    : undefined;
}

function extractTextFromBlocks(blocks: ContentBlock[] | undefined): string {
  if (!Array.isArray(blocks) || blocks.length === 0) {
    return '';
  }
  return blocks
    .filter((block) => block?.type === 'text' || block?.type === 'thinking')
    .map((block) => block.content || '')
    .join('\n')
    .trim();
}

function resolveRole(message: TimelineMessageLike): 'user' | 'assistant' | 'system' {
  if (message.role === 'user' || message.type === MessageType.USER_INPUT) {
    return 'user';
  }
  if (message.role === 'system' || message.type === MessageType.SYSTEM) {
    return 'system';
  }
  return 'assistant';
}

function resolveLifecycleKey(metadata: Record<string, unknown> | undefined): string | undefined {
  return resolveTimelineWorkerLifecycleKey(metadata) || undefined;
}

function resolveCardId(
  metadata: Record<string, unknown> | undefined,
  messageId: string,
  messageType?: string,
): string {
  const cardId = typeof metadata?.cardId === 'string' ? metadata.cardId.trim() : '';
  if (cardId) {
    return cardId;
  }
  if (isTimelineWorkerLifecycleMessageType(messageType)) {
    const workerCardId = resolveTimelineWorkerCardId(metadata);
    if (workerCardId) {
      return workerCardId;
    }
  }
  return messageId;
}

function resolveWorker(message: TimelineMessageLike): WorkerSlot | undefined {
  const metadata = resolveMetadata(message);
  return normalizeWorkerSlot(
    metadata?.worker
      || metadata?.assignedWorker
      || metadata?.agent
      || message.agent
      || message.source,
  );
}

function resolveRecordVisibility(message: TimelineMessageLike): {
  threadVisible: boolean;
  workerViews: WorkerSlot[];
} {
  const worker = resolveWorker(message);
  const visibility = resolveTimelineWorkerVisibility({
    hasWorker: Boolean(worker),
    type: message.type,
    source: message.source,
  });
  return {
    threadVisible: visibility.threadVisible,
    workerViews: visibility.includeWorkerTab && worker ? [worker] : [],
  };
}

function hasRenderableThinkingBlock(blocks: ContentBlock[] | undefined): boolean {
  return Array.isArray(blocks) && blocks.some((block) => block?.type === 'thinking');
}

function hasRenderableToolBlock(blocks: ContentBlock[] | undefined): boolean {
  return Array.isArray(blocks) && blocks.some((block) => {
    const blockType = typeof (block as { type?: unknown } | undefined)?.type === 'string'
      ? (block as { type: string }).type
      : '';
    return blockType === 'tool_call'
      || blockType === 'file_change';
  });
}

function resolveRecordKind(message: TimelineMessageLike): TimelineRecordKind {
  if (message.type === MessageType.USER_INPUT) {
    return 'user_input';
  }
  if (message.type === MessageType.INSTRUCTION || message.type === MessageType.TASK_CARD) {
    return 'worker_lifecycle';
  }
  if (message.type === MessageType.RESULT && resolveWorker(message)) {
    return 'worker_result';
  }
  if (message.type === MessageType.PROGRESS) {
    return 'progress';
  }
  if (message.type === MessageType.SYSTEM) {
    return 'system_notice';
  }
  if (message.type === MessageType.THINKING || hasRenderableThinkingBlock(message.blocks)) {
    return 'thinking';
  }
  if (message.type === MessageType.TOOL_CALL || hasRenderableToolBlock(message.blocks)) {
    return 'tool_card';
  }
  return 'assistant_text';
}

function resolveStableKey(message: TimelineMessageLike, kind: TimelineRecordKind): string {
  const metadata = resolveMetadata(message);
  const cardId = resolveCardId(metadata, message.id, message.type);
  const originMessageId = typeof metadata?.originMessageId === 'string' ? metadata.originMessageId.trim() : '';
  const isFragmentedMessage = Boolean(originMessageId) && originMessageId !== message.id;
  if (kind === 'worker_lifecycle') {
    const lifecycleKey = resolveLifecycleKey(metadata) || cardId;
    return `lifecycle:${lifecycleKey}`;
  }
  if (kind === 'tool_card') {
    return isFragmentedMessage ? `tool:${message.id}` : `tool:${cardId}`;
  }
  if (kind === 'thinking') {
    return isFragmentedMessage ? `thinking:${message.id}` : `thinking:${cardId}`;
  }
  if (kind === 'user_input') {
    return `user_input:${message.id}`;
  }
  if (kind === 'worker_result') {
    return `result:${cardId}`;
  }
  if (kind === 'progress') {
    return `progress:${cardId}`;
  }
  if (kind === 'system_notice') {
    return `system_notice:${cardId}`;
  }
  return `message:${message.id}`;
}

function resolveVisibility(message: TimelineMessageLike): MessageVisibility {
  return message.visibility || 'user';
}

export function isInternalControlTimelineMessage(
  message: { metadata?: unknown },
): boolean {
  const metadata = resolveMetadata(message);
  return metadata?.phase === 'system_section';
}

function isLegacyTimelineSystemNotice(message: TimelineMessageLike): boolean {
  if (message.type !== MessageType.SYSTEM) {
    return true;
  }
  const metadata = resolveMetadata(message);
  return metadata?.isStatusMessage === true;
}

export function resolveSessionPersistenceTarget(
  message: Pick<StandardMessage, 'category' | 'visibility' | 'metadata'>,
): SessionPersistenceTarget {
  if (message.category === MessageCategory.NOTIFY) {
    return 'notification';
  }
  if (message.category === MessageCategory.CONTROL || message.category === MessageCategory.DATA) {
    return 'ignore';
  }
  if (message.visibility === 'system' || message.visibility === 'debug') {
    return 'ignore';
  }
  if (isInternalControlTimelineMessage(message)) {
    return 'ignore';
  }
  if (message.category === MessageCategory.CONTENT) {
    return 'timeline';
  }
  return 'ignore';
}

export function resolveSessionPersistenceTargetFromMessageLike(message: TimelineMessageLike): SessionPersistenceTarget {
  const category = typeof message.category === 'string' ? message.category : '';
  if (category === MessageCategory.NOTIFY) {
    return 'notification';
  }
  if (category === MessageCategory.CONTROL || category === MessageCategory.DATA) {
    return 'ignore';
  }
  if (resolveVisibility(message) === 'system' || resolveVisibility(message) === 'debug') {
    return 'ignore';
  }
  if (isInternalControlTimelineMessage(message)) {
    return 'ignore';
  }
  if (message.type === MessageType.SYSTEM && !isLegacyTimelineSystemNotice(message)) {
    return 'ignore';
  }
  return 'timeline';
}

function buildTimelineRecordFromSingleMessageLike(message: TimelineMessageLike): TimelineRecord | null {
  if (resolveSessionPersistenceTargetFromMessageLike(message) !== 'timeline') {
    return null;
  }
  // 空内容的 system-notice / progress 消息不持久化到时间轴
  if (message.type === MessageType.SYSTEM || message.type === MessageType.PROGRESS) {
    const textContent = message.content?.trim() || extractTextFromBlocks(message.blocks);
    if (!textContent) {
      return null;
    }
  }
  const metadata = resolveMetadata(message);
  const kind = resolveRecordKind(message);
  const stableKey = resolveStableKey(message, kind);
  const cardId = resolveCardId(metadata, message.id);
  const lifecycleKey = kind === 'worker_lifecycle'
    ? (resolveLifecycleKey(metadata) || cardId)
    : undefined;
  const visibility = resolveRecordVisibility(message);
  const content = message.content?.trim() || extractTextFromBlocks(message.blocks);
  const createdAt = resolveTimelineSortTimestamp(message.timestamp, metadata);
  const updatedAt = typeof message.updatedAt === 'number' && Number.isFinite(message.updatedAt)
    ? Math.floor(message.updatedAt)
    : (typeof message.timestamp === 'number' && Number.isFinite(message.timestamp)
      ? Math.floor(message.timestamp)
      : Date.now());
  return {
    recordId: stableKey,
    nodeId: stableKey,
    stableKey,
    messageId: message.id,
    kind,
    role: resolveRole(message),
    source: typeof message.source === 'string' ? message.source : undefined,
    agent: message.agent,
    messageType: message.type,
    category: typeof message.category === 'string' ? message.category : MessageCategory.CONTENT,
    visibility: resolveVisibility(message),
    requestId: typeof metadata?.requestId === 'string' ? metadata.requestId : undefined,
    turnId: typeof metadata?.turnId === 'string' ? metadata.turnId : undefined,
    missionId: typeof metadata?.missionId === 'string' ? metadata.missionId : undefined,
    dispatchWaveId: resolveTimelineDispatchWaveId(metadata) || undefined,
    assignmentId: typeof metadata?.assignmentId === 'string' ? metadata.assignmentId : undefined,
    laneId: resolveTimelineWorkerLaneId(metadata) || undefined,
    workerCardId: resolveTimelineWorkerCardId(metadata) || undefined,
    worker: resolveWorker(message),
    threadVisible: visibility.threadVisible,
    workerViews: visibility.workerViews,
    cardId,
    lifecycleKey,
    anchorEventSeq: resolveTimelineEventSeqFromMetadata(metadata),
    anchorTimestamp: resolveTimelineAnchorTimestampFromMetadata(metadata) || createdAt,
    cardStreamSeq: resolveTimelineCardStreamSeqFromMetadata(metadata),
    messageTimestamp: typeof message.timestamp === 'number' && Number.isFinite(message.timestamp)
      ? Math.floor(message.timestamp)
      : createdAt,
    createdAt,
    updatedAt,
    version: resolveTimelineVersionFromMetadata(metadata),
    content,
    attachments: Array.isArray(message.attachments) && message.attachments.length > 0 ? message.attachments : undefined,
    images: Array.isArray(message.images) && message.images.length > 0 ? message.images : undefined,
    blocks: Array.isArray(message.blocks) && message.blocks.length > 0 ? message.blocks : undefined,
    noticeType: typeof message.noticeType === 'string' ? message.noticeType : undefined,
    isStreaming: typeof message.isStreaming === 'boolean' ? message.isStreaming : undefined,
    isComplete: typeof message.isComplete === 'boolean' ? message.isComplete : undefined,
    interaction: message.interaction,
    metadata,
  };
}

export function buildTimelineRecordsFromMessageLike(message: TimelineMessageLike): TimelineRecord[] {
  return expandRenderableTimelineMessages(message)
    .map((fragment) => buildTimelineRecordFromSingleMessageLike(fragment as TimelineMessageLike))
    .filter((record): record is TimelineRecord => Boolean(record));
}

export function buildTimelineRecordFromMessageLike(message: TimelineMessageLike): TimelineRecord | null {
  return buildTimelineRecordsFromMessageLike(message)[0] || null;
}

function resolveNotificationKind(kind: 'incident' | 'audit' | 'feedback'): SessionNotificationRecord['kind'] {
  return kind === 'incident' ? 'incident' : 'audit';
}

export function buildNotificationRecordFromStandardMessage(
  message: StandardMessage,
): SessionNotificationRecord | null {
  if (resolveSessionPersistenceTarget(message) !== 'notification') {
    return null;
  }
  const content = extractTextFromBlocks(message.blocks);
  if (!content) {
    return null;
  }
  const presentation = resolveNotificationPresentation(message.notify, 'model-runtime');
  if (!shouldPersistNotificationRecord(presentation)) {
    return null;
  }
  return {
    notificationId: message.id,
    kind: resolveNotificationKind(presentation.category),
    level: presentation.level,
    title: presentation.title,
    message: content,
    source: presentation.source,
    createdAt: typeof message.timestamp === 'number' && Number.isFinite(message.timestamp)
      ? Math.floor(message.timestamp)
      : Date.now(),
    read: false,
    persistToCenter: presentation.persistToCenter,
    actionRequired: presentation.actionRequired,
    countUnread: presentation.countUnread,
    displayMode: presentation.displayMode,
    duration: presentation.duration,
  };
}

export function mergeTimelineRecord(existing: TimelineRecord, incoming: TimelineRecord): TimelineRecord {
  const existingMetadata = resolveMetadata(existing);
  const incomingMetadata = resolveMetadata(incoming);
  const sourceMessageIds = Array.from(new Set([
    existing.messageId,
    ...(Array.isArray(existingMetadata?.sourceMessageIds)
      ? existingMetadata.sourceMessageIds.filter((value): value is string => typeof value === 'string' && value.trim().length > 0)
      : []),
    incoming.messageId,
    ...(Array.isArray(incomingMetadata?.sourceMessageIds)
      ? incomingMetadata.sourceMessageIds.filter((value): value is string => typeof value === 'string' && value.trim().length > 0)
      : []),
  ]));
  const mergedMetadata = {
    ...(existingMetadata ? structuredClone(existingMetadata) : {}),
    ...(incomingMetadata ? structuredClone(incomingMetadata) : {}),
    ...(sourceMessageIds.length > 1 ? { sourceMessageIds } : {}),
  };
  if (sourceMessageIds.length <= 1) {
    delete mergedMetadata.sourceMessageIds;
  }
  return {
    ...existing,
    ...incoming,
    workerViews: Array.from(new Set([
      ...existing.workerViews,
      ...incoming.workerViews,
    ])),
    createdAt: Math.min(existing.createdAt, incoming.createdAt),
    updatedAt: Math.max(existing.updatedAt, incoming.updatedAt),
    version: Math.max(existing.version, incoming.version),
    anchorEventSeq: existing.anchorEventSeq || incoming.anchorEventSeq,
    anchorTimestamp: existing.anchorTimestamp || incoming.anchorTimestamp,
    cardStreamSeq: Math.max(existing.cardStreamSeq, incoming.cardStreamSeq),
    messageTimestamp: Math.max(existing.messageTimestamp, incoming.messageTimestamp),
    metadata: Object.keys(mergedMetadata).length > 0 ? mergedMetadata : undefined,
  };
}
