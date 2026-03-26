import type { ContentBlock as StandardContentBlock } from '../protocol/message-protocol';
import type { AgentType, WorkerSlot } from '../types/agent-types';
import {
  compareTimelineSemanticOrder,
  resolveTimelineAnchorTimestampFromMetadata,
  resolveTimelineBlockSeqFromMetadata,
  resolveTimelineCardStreamSeqFromMetadata,
  resolveTimelineDetailedVersionFromMetadata,
  resolveTimelineEventSeqFromMetadata,
  resolveStableTimelinePlacementTimestamp,
  resolveTimelineSortTimestamp,
} from '../shared/timeline-ordering';
import {
  isTimelineWorkerLifecycleMessageType,
  messageHasRenderableTimelineContent,
  resolveTimelinePrimaryToolCallName,
  resolveTimelinePresentationKind,
  resolveTimelineWorkerVisibility,
} from '../shared/timeline-presentation';
import {
  resolveTimelineDispatchWaveId,
  resolveTimelineWorkerCardId,
  resolveTimelineWorkerLaneId,
  resolveTimelineWorkerLifecycleKey,
} from '../shared/timeline-worker-lifecycle';
import { expandRenderableTimelineMessages } from '../shared/timeline-message-fragmentation';
import { isInternalControlTimelineMessage } from './timeline-classifier';

const WORKER_SLOTS: WorkerSlot[] = ['claude', 'codex', 'gemini'];
const WORKER_SLOT_SET = new Set<WorkerSlot>(WORKER_SLOTS);

export type SessionTimelineArtifactKind = 'message' | 'tool' | 'worker_lifecycle';

export interface SessionTimelineProjectionMessage {
  id: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  agent?: AgentType;
  source?: 'orchestrator' | 'worker' | 'system' | WorkerSlot;
  timestamp: number;
  updatedAt?: number;
  attachments?: { name: string; path: string; mimeType?: string }[];
  images?: Array<{ dataUrl: string }>;
  blocks?: StandardContentBlock[];
  type?: string;
  noticeType?: string;
  isStreaming?: boolean;
  isComplete?: boolean;
  metadata?: Record<string, unknown>;
}

export interface SessionTimelineProjectionArtifact {
  artifactId: string;
  kind: SessionTimelineArtifactKind;
  displayOrder: number;
  artifactVersion: number;
  anchorEventSeq: number;
  latestEventSeq: number;
  cardStreamSeq: number;
  timestamp: number;
  cardId?: string;
  lifecycleKey?: string;
  dispatchWaveId?: string;
  laneId?: string;
  workerCardId?: string;
  worker?: WorkerSlot;
  threadVisible: boolean;
  workerTabs: WorkerSlot[];
  messageIds: string[];
  message: SessionTimelineProjectionMessage;
  executionItems: SessionTimelineProjectionExecutionItem[];
}

export interface SessionTimelineProjectionExecutionItem {
  itemId: string;
  itemOrder: number;
  anchorEventSeq: number;
  latestEventSeq: number;
  cardStreamSeq: number;
  timestamp: number;
  worker?: WorkerSlot;
  threadVisible: boolean;
  workerTabs: WorkerSlot[];
  messageIds: string[];
  message: SessionTimelineProjectionMessage;
}

export interface SessionTimelineProjectionRenderEntry {
  entryId: string;
  artifactId: string;
  executionItemId?: string;
}

export interface SessionTimelineProjection {
  schemaVersion: 'session-timeline-projection.v2';
  sessionId: string;
  updatedAt: number;
  lastAppliedEventSeq: number;
  artifacts: SessionTimelineProjectionArtifact[];
  threadRenderEntries: SessionTimelineProjectionRenderEntry[];
  workerRenderEntries: Record<WorkerSlot, SessionTimelineProjectionRenderEntry[]>;
}

interface ProjectionSourceSession {
  id: string;
  updatedAt: number;
  messages: readonly SessionTimelineProjectionMessage[];
}

interface ProjectionExecutionItemAccumulator {
  itemId: string;
  anchorEventSeq: number;
  latestEventSeq: number;
  cardStreamSeq: number;
  timestamp: number;
  worker?: WorkerSlot;
  threadVisible: boolean;
  workerTabs: WorkerSlot[];
  messageIds: string[];
  message: SessionTimelineProjectionMessage;
}

interface ProjectionArtifactAccumulator {
  artifactId: string;
  kind: SessionTimelineArtifactKind;
  artifactVersion: number;
  anchorEventSeq: number;
  latestEventSeq: number;
  cardStreamSeq: number;
  timestamp: number;
  cardId?: string;
  lifecycleKey?: string;
  dispatchWaveId?: string;
  laneId?: string;
  workerCardId?: string;
  worker?: WorkerSlot;
  threadVisible: boolean;
  workerTabs: WorkerSlot[];
  messageIds: string[];
  message: SessionTimelineProjectionMessage | null;
  executionItems: ProjectionExecutionItemAccumulator[];
}

interface ProjectionFlatRenderEntry {
  entryId: string;
  artifactId: string;
  executionItemId?: string;
  groupId: string;
  message: SessionTimelineProjectionMessage;
  timestamp: number;
  displayOrder?: number;
  itemOrder?: number;
  anchorEventSeq: number;
  blockSeq: number;
  cardStreamSeq: number;
}

function cloneSerializable<T>(value: T): T {
  return structuredClone(value);
}

function isProjectionContainerOnlyMessage(
  message: Pick<SessionTimelineProjectionMessage, 'metadata'> | undefined,
): boolean {
  return message?.metadata && typeof message.metadata === 'object'
    ? (message.metadata as Record<string, unknown>).timelineContainerOnly === true
    : false;
}

function shouldRenderProjectionHostMessage(
  artifact: Pick<SessionTimelineProjectionArtifact, 'kind' | 'message' | 'executionItems'>,
): boolean {
  if (artifact.kind !== 'worker_lifecycle' && Array.isArray(artifact.executionItems) && artifact.executionItems.length > 0) {
    return false;
  }
  return !isProjectionContainerOnlyMessage(artifact.message);
}

function resolveProjectionMessageBlockSeq(
  message: Pick<SessionTimelineProjectionMessage, 'metadata'> | undefined,
): number {
  return resolveTimelineBlockSeqFromMetadata(
    message?.metadata && typeof message.metadata === 'object'
      ? message.metadata as Record<string, unknown>
      : undefined,
  );
}

function compareProjectionFlatRenderEntry(
  left: ProjectionFlatRenderEntry,
  right: ProjectionFlatRenderEntry,
): number {
  const sameGroup = left.groupId === right.groupId;
  return compareTimelineSemanticOrder(
    {
      timestamp: left.timestamp,
      stableId: left.entryId,
      displayOrder: sameGroup ? left.displayOrder : undefined,
      itemOrder: sameGroup ? left.itemOrder : undefined,
      messageType: left.message.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(left.message.blocks),
      anchorEventSeq: left.anchorEventSeq,
      blockSeq: sameGroup ? left.blockSeq : undefined,
      cardStreamSeq: left.cardStreamSeq,
    },
    {
      timestamp: right.timestamp,
      stableId: right.entryId,
      displayOrder: sameGroup ? right.displayOrder : undefined,
      itemOrder: sameGroup ? right.itemOrder : undefined,
      messageType: right.message.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(right.message.blocks),
      anchorEventSeq: right.anchorEventSeq,
      blockSeq: sameGroup ? right.blockSeq : undefined,
      cardStreamSeq: right.cardStreamSeq,
    },
  );
}

function buildProjectionPanelRenderEntries(
  artifacts: SessionTimelineProjectionArtifact[],
  displayContext: 'thread' | 'worker',
  worker?: WorkerSlot,
): SessionTimelineProjectionRenderEntry[] {
  const flatEntries: ProjectionFlatRenderEntry[] = [];

  for (const artifact of artifacts) {
    const artifactVisible = displayContext === 'thread'
      ? artifact.threadVisible
      : Boolean(worker && artifact.workerTabs.includes(worker));
    if (artifactVisible && shouldRenderProjectionHostMessage(artifact)) {
      flatEntries.push({
        entryId: `artifact:${artifact.artifactId}`,
        artifactId: artifact.artifactId,
        groupId: artifact.artifactId,
        message: artifact.message,
        timestamp: artifact.timestamp,
        displayOrder: artifact.displayOrder,
        anchorEventSeq: artifact.anchorEventSeq,
        blockSeq: resolveProjectionMessageBlockSeq(artifact.message),
        cardStreamSeq: artifact.cardStreamSeq,
      });
    }

    for (const item of artifact.executionItems || []) {
      const itemVisible = displayContext === 'thread'
        ? item.threadVisible
        : Boolean(worker && item.workerTabs.includes(worker));
      if (!itemVisible) {
        continue;
      }
      flatEntries.push({
        entryId: `item:${artifact.artifactId}:${item.itemId}`,
        artifactId: artifact.artifactId,
        executionItemId: item.itemId,
        groupId: artifact.artifactId,
        message: item.message,
        timestamp: item.timestamp,
        displayOrder: artifact.displayOrder,
        itemOrder: item.itemOrder,
        anchorEventSeq: item.anchorEventSeq,
        blockSeq: resolveProjectionMessageBlockSeq(item.message),
        cardStreamSeq: item.cardStreamSeq,
      });
    }
  }

  return flatEntries
    .sort(compareProjectionFlatRenderEntry)
    .map((entry) => ({
      entryId: entry.entryId,
      artifactId: entry.artifactId,
      ...(entry.executionItemId ? { executionItemId: entry.executionItemId } : {}),
    }));
}

function normalizeWorkerSlot(value: unknown): WorkerSlot | undefined {
  if (typeof value !== 'string') {
    return undefined;
  }
  const normalized = value.trim().toLowerCase();
  return WORKER_SLOT_SET.has(normalized as WorkerSlot)
    ? normalized as WorkerSlot
    : undefined;
}

function resolveMessageMetadata(message: Pick<SessionTimelineProjectionMessage, 'metadata'>): Record<string, unknown> | undefined {
  return message.metadata && typeof message.metadata === 'object' && !Array.isArray(message.metadata)
    ? message.metadata
    : undefined;
}

function resolveMessageRequestId(
  message: Pick<SessionTimelineProjectionMessage, 'metadata'>,
): string {
  const metadata = resolveMessageMetadata(message);
  return typeof metadata?.requestId === 'string' ? metadata.requestId.trim() : '';
}

function backfillUserRequestAnchorTimestamps(
  messages: SessionTimelineProjectionMessage[],
): SessionTimelineProjectionMessage[] {
  const earliestAnchorByRequestId = new Map<string, number>();
  for (const message of messages) {
    const requestId = resolveMessageRequestId(message);
    if (!requestId) {
      continue;
    }
    const anchorTimestamp = resolveTimelineAnchorTimestampFromMetadata(resolveMessageMetadata(message));
    if (anchorTimestamp === null) {
      continue;
    }
    const known = earliestAnchorByRequestId.get(requestId) || 0;
    earliestAnchorByRequestId.set(
      requestId,
      known > 0 ? Math.min(known, anchorTimestamp) : anchorTimestamp,
    );
  }

  return messages.map((message) => {
    if (message.type !== 'user_input') {
      return message;
    }
    const metadata = resolveMessageMetadata(message);
    if (resolveTimelineAnchorTimestampFromMetadata(metadata) !== null) {
      return message;
    }
    const requestId = resolveMessageRequestId(message);
    if (!requestId) {
      return message;
    }
    const requestAnchorTimestamp = earliestAnchorByRequestId.get(requestId) || 0;
    if (requestAnchorTimestamp <= 0) {
      return message;
    }
    return {
      ...message,
      metadata: {
        ...(metadata || {}),
        timelineAnchorTimestamp: requestAnchorTimestamp,
      },
    };
  });
}

function resolveMessageSortTimestamp(message: Pick<SessionTimelineProjectionMessage, 'timestamp' | 'metadata' | 'type'>): number {
  const metadata = resolveMessageMetadata(message);
  return resolveTimelineSortTimestamp(message.timestamp, metadata);
}

function mergeTimelineSortTimestamp(
  currentTimestamp: number | undefined,
  message: Pick<SessionTimelineProjectionMessage, 'timestamp' | 'metadata' | 'type'>,
): number {
  return resolveStableTimelinePlacementTimestamp(
    currentTimestamp,
    resolveMessageSortTimestamp(message),
  );
}

function resolveMessageEventSeq(message: Pick<SessionTimelineProjectionMessage, 'metadata'>): number {
  return resolveTimelineEventSeqFromMetadata(resolveMessageMetadata(message));
}

function resolveMessageBlockSeq(message: Pick<SessionTimelineProjectionMessage, 'metadata'>): number {
  return resolveTimelineBlockSeqFromMetadata(resolveMessageMetadata(message));
}

function resolveTimelineCardId(message: SessionTimelineProjectionMessage): string | undefined {
  const cardId = typeof message.metadata?.cardId === 'string' ? message.metadata.cardId.trim() : '';
  if (cardId) {
    return cardId;
  }
  if (!isTimelineWorkerLifecycleMessageType(message.type)) {
    return undefined;
  }
  return resolveTimelineWorkerCardId(resolveMessageMetadata(message)) || undefined;
}

function resolveProjectionDispatchWaveId(
  message: Pick<SessionTimelineProjectionMessage, 'metadata'>,
): string | undefined {
  return resolveTimelineDispatchWaveId(resolveMessageMetadata(message)) || undefined;
}

function resolveProjectionLaneId(
  message: Pick<SessionTimelineProjectionMessage, 'metadata' | 'agent' | 'source'>,
): string | undefined {
  return resolveTimelineWorkerLaneId(resolveMessageMetadata(message), message.agent || message.source) || undefined;
}

function resolveProjectionWorkerCardId(
  message: Pick<SessionTimelineProjectionMessage, 'metadata'>,
): string | undefined {
  return resolveTimelineWorkerCardId(resolveMessageMetadata(message)) || undefined;
}

function resolveTimelineLifecycleKey(message: SessionTimelineProjectionMessage): string | undefined {
  if (message.type !== 'instruction' && message.type !== 'task_card') {
    return undefined;
  }
  const metadata = resolveMessageMetadata(message);
  const resolved = resolveTimelineWorkerLifecycleKey(metadata, {
    fallbackWorker: message.agent || message.source,
  });
  if (resolved) return resolved;
  return resolveTimelineCardId(message);
}

function resolveProjectionTaskKey(message: Pick<SessionTimelineProjectionMessage, 'metadata'>): string | undefined {
  const metadata = resolveMessageMetadata(message);
  const resolved = resolveTimelineWorkerLifecycleKey(metadata);
  return resolved || undefined;
}

function resolveTimelineWorker(message: SessionTimelineProjectionMessage): WorkerSlot | undefined {
  const metadata = resolveMessageMetadata(message);
  return normalizeWorkerSlot(
    metadata?.worker
      || metadata?.assignedWorker
      || metadata?.agent
      || message.agent
      || message.source,
  );
}

function resolveProjectionDisplaySource(
  message: Pick<SessionTimelineProjectionMessage, 'source' | 'role' | 'agent' | 'metadata'>,
): 'orchestrator' | 'system' | WorkerSlot {
  const worker = resolveTimelineWorker(message as SessionTimelineProjectionMessage);
  if (typeof message.source === 'string') {
    const normalizedSource = message.source.trim().toLowerCase();
    if (normalizedSource === 'orchestrator') {
      return 'orchestrator';
    }
    if (normalizedSource === 'system') {
      return 'system';
    }
    const sourceWorker = normalizeWorkerSlot(normalizedSource);
    if (sourceWorker) {
      return sourceWorker;
    }
    if (normalizedSource === 'worker' && worker) {
      return worker;
    }
  }
  if (worker) {
    return worker;
  }
  return message.role === 'system' ? 'system' : 'orchestrator';
}

function resolveArtifactId(message: SessionTimelineProjectionMessage): string {
  const lifecycleKey = resolveTimelineLifecycleKey(message);
  if (lifecycleKey) {
    return `lifecycle:${lifecycleKey}`;
  }
  return message.id;
}

function isWorkerLifecycleMessage(message: Pick<SessionTimelineProjectionMessage, 'type'>): boolean {
  return isTimelineWorkerLifecycleMessageType(message.type);
}

function isWorkerLifecycleExecutionMessage(
  message: Pick<SessionTimelineProjectionMessage, 'type' | 'source' | 'role' | 'metadata'>,
): boolean {
  void message;
  // 会话投影与前端渲染保持一致：
  // 生命周期卡片只保存 instruction/task_card，本体之外的 worker 输出必须独立成节点。
  return false;
}

function resolveMessageVersionCursor(message: Pick<SessionTimelineProjectionMessage, 'metadata'>): number {
  return resolveTimelineDetailedVersionFromMetadata(resolveMessageMetadata(message));
}

function resolveProjectionSourceMessageIds(
  message: Pick<SessionTimelineProjectionMessage, 'id' | 'metadata'>,
): string[] {
  const metadata = resolveMessageMetadata(message);
  const aliases = Array.isArray(metadata?.sourceMessageIds)
    ? metadata.sourceMessageIds.filter((value): value is string => typeof value === 'string' && value.trim().length > 0)
    : [];
  return Array.from(new Set([message.id, ...aliases]));
}

function resolveArtifactKind(message: SessionTimelineProjectionMessage): SessionTimelineArtifactKind {
  return resolveTimelinePresentationKind(message);
}

function messageHasRenderableContent(message: SessionTimelineProjectionMessage): boolean {
  return messageHasRenderableTimelineContent(message);
}

function mergeLifecycleMessage(
  existing: SessionTimelineProjectionMessage,
  incoming: SessionTimelineProjectionMessage,
  stableId: string,
): SessionTimelineProjectionMessage {
  const existingMetadata = resolveMessageMetadata(existing) || {};
  const incomingMetadata = resolveMessageMetadata(incoming) || {};
  const existingSubTaskCard = existingMetadata.subTaskCard && typeof existingMetadata.subTaskCard === 'object' && !Array.isArray(existingMetadata.subTaskCard)
    ? existingMetadata.subTaskCard as Record<string, unknown>
    : undefined;
  const incomingSubTaskCard = incomingMetadata.subTaskCard && typeof incomingMetadata.subTaskCard === 'object' && !Array.isArray(incomingMetadata.subTaskCard)
    ? incomingMetadata.subTaskCard as Record<string, unknown>
    : undefined;
  const shouldMergeSubTaskCard = existing.type !== 'task_card' || incoming.type !== 'task_card';
  const mergedSubTaskCard = shouldMergeSubTaskCard
    ? ((existingSubTaskCard || incomingSubTaskCard)
      ? {
          ...(existingSubTaskCard || {}),
          ...(incomingSubTaskCard || {}),
        }
      : undefined)
    : (incomingSubTaskCard || existingSubTaskCard);
  const mergedMetadata: Record<string, unknown> = {
    ...existingMetadata,
    ...incomingMetadata,
    ...(mergedSubTaskCard ? { subTaskCard: mergedSubTaskCard } : {}),
    lifecycleCardMode: 'instruction_with_summary',
  };
  const mergedCardId = typeof mergedMetadata.cardId === 'string' && mergedMetadata.cardId.trim()
    ? mergedMetadata.cardId.trim()
    : stableId;
  mergedMetadata.cardId = mergedCardId;

  if (existing.type === 'instruction' && incoming.type === 'task_card') {
    const existingHasRenderableContent = messageHasRenderableContent(existing);
    return {
      ...existing,
      id: stableId,
      isStreaming: false,
      isComplete: incoming.isComplete,
      ...(existingHasRenderableContent ? {} : { content: incoming.content, blocks: cloneSerializable(incoming.blocks) }),
      metadata: mergedMetadata,
    };
  }

  if (existing.type === 'task_card' && incoming.type === 'instruction') {
    return {
      ...incoming,
      id: stableId,
      timestamp: existing.timestamp || incoming.timestamp,
      metadata: mergedMetadata,
    };
  }

  // fallback 合并：保留 lifecycle 类型（instruction/task_card），
  // 防止后续 tool_call 等非 lifecycle 消息通过 ...incoming 覆盖 type
  const mergedType = isWorkerLifecycleMessage(existing) ? existing.type : incoming.type;
  return {
    ...existing,
    ...incoming,
    id: stableId,
    type: mergedType,
    timestamp: existing.timestamp || incoming.timestamp,
    metadata: mergedMetadata,
  };
}

function resolveWorkerVisibility(message: SessionTimelineProjectionMessage): {
  threadVisible: boolean;
  workerTabs: WorkerSlot[];
} {
  const worker = resolveTimelineWorker(message);
  const visibility = resolveTimelineWorkerVisibility({
    hasWorker: Boolean(worker),
    type: message.type,
    source: message.source,
  });
  return {
    threadVisible: visibility.threadVisible,
    workerTabs: visibility.includeWorkerTab && worker ? [worker] : [],
  };
}

function compareProjectionMessages(
  left: SessionTimelineProjectionMessage,
  right: SessionTimelineProjectionMessage,
): number {
  return compareTimelineSemanticOrder(
    {
      timestamp: resolveMessageSortTimestamp(left),
      stableId: left.id,
      messageType: left.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(left.blocks),
      anchorEventSeq: resolveMessageEventSeq(left),
      blockSeq: resolveMessageBlockSeq(left),
      cardStreamSeq: resolveTimelineCardStreamSeqFromMetadata(resolveMessageMetadata(left)),
    },
    {
      timestamp: resolveMessageSortTimestamp(right),
      stableId: right.id,
      messageType: right.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(right.blocks),
      anchorEventSeq: resolveMessageEventSeq(right),
      blockSeq: resolveMessageBlockSeq(right),
      cardStreamSeq: resolveTimelineCardStreamSeqFromMetadata(resolveMessageMetadata(right)),
    },
  );
}

function compareProjectionArtifacts(
  left: ProjectionArtifactAccumulator,
  right: ProjectionArtifactAccumulator,
): number {
  return compareTimelineSemanticOrder(
    {
      timestamp: left.timestamp,
      stableId: left.artifactId,
      messageType: left.message?.type,
      primaryToolCallName: left.message ? resolveTimelinePrimaryToolCallName(left.message.blocks) : '',
      anchorEventSeq: left.anchorEventSeq,
      blockSeq: left.message ? resolveMessageBlockSeq(left.message) : 0,
      cardStreamSeq: left.cardStreamSeq,
    },
    {
      timestamp: right.timestamp,
      stableId: right.artifactId,
      messageType: right.message?.type,
      primaryToolCallName: right.message ? resolveTimelinePrimaryToolCallName(right.message.blocks) : '',
      anchorEventSeq: right.anchorEventSeq,
      blockSeq: right.message ? resolveMessageBlockSeq(right.message) : 0,
      cardStreamSeq: right.cardStreamSeq,
    },
  );
}

function normalizeProjectionMessage(message: SessionTimelineProjectionMessage): SessionTimelineProjectionMessage {
  // 不在此处做 structuredClone：调用方 (getLiveSessionTimelineProjection) 会对整个投影结果做一次统一深拷贝。
  // 移除 per-field clone 避免 N² 级 structuredClone 导致 CPU 飙高。
  return {
    id: message.id,
    role: message.role,
    content: message.content,
    agent: message.agent,
    source: resolveProjectionDisplaySource(message),
    timestamp: message.timestamp,
    updatedAt: message.updatedAt,
    attachments: Array.isArray(message.attachments) ? message.attachments : undefined,
    images: Array.isArray(message.images) ? message.images : undefined,
    blocks: Array.isArray(message.blocks) ? message.blocks : undefined,
    type: message.type,
    noticeType: message.noticeType,
    isStreaming: typeof message.isStreaming === 'boolean' ? message.isStreaming : undefined,
    isComplete: typeof message.isComplete === 'boolean' ? message.isComplete : undefined,
    metadata: resolveMessageMetadata(message) ? resolveMessageMetadata(message)! : undefined,
  };
}

function setTimelineContainerFlag(
  message: SessionTimelineProjectionMessage,
  containerOnly: boolean,
): SessionTimelineProjectionMessage {
  const metadata = {
    ...(resolveMessageMetadata(message) || {}),
  };
  if (containerOnly) {
    metadata.timelineContainerOnly = true;
  } else {
    delete metadata.timelineContainerOnly;
  }
  return normalizeProjectionMessage({
    ...message,
    ...(Object.keys(metadata).length > 0 ? { metadata } : { metadata: undefined }),
  });
}

function resolveTimelineFragmentMessages(
  message: SessionTimelineProjectionMessage,
): SessionTimelineProjectionMessage[] {
  const fragmentSource = setTimelineContainerFlag(message, false);
  const fragments = expandRenderableTimelineMessages(fragmentSource);
  if (fragments.length <= 1) {
    return [];
  }
  return fragments.map((fragment, index) => normalizeProjectionMessage({
    ...fragment,
    isStreaming: fragmentSource.isStreaming === true && index === fragments.length - 1,
  }));
}

function createExecutionItem(message: SessionTimelineProjectionMessage): ProjectionExecutionItemAccumulator {
  const worker = resolveTimelineWorker(message);
  const visibility = resolveWorkerVisibility(message);
  const itemId = message.id;
  const messageEventSeq = resolveMessageEventSeq(message);
  const cardStreamSeq = resolveTimelineCardStreamSeqFromMetadata(resolveMessageMetadata(message));
  return {
    itemId,
    anchorEventSeq: messageEventSeq,
    latestEventSeq: messageEventSeq,
    cardStreamSeq,
    timestamp: resolveMessageSortTimestamp(message),
    worker,
    threadVisible: visibility.threadVisible,
    workerTabs: visibility.workerTabs,
    messageIds: Array.from(new Set([itemId, ...resolveProjectionSourceMessageIds(message)])),
    message: normalizeProjectionMessage(message),
  };
}

function mergeExecutionItem(
  existing: ProjectionExecutionItemAccumulator,
  incomingMessage: SessionTimelineProjectionMessage,
): ProjectionExecutionItemAccumulator {
  const normalizedIncoming = normalizeProjectionMessage(incomingMessage);
  const incomingVisibility = resolveWorkerVisibility(normalizedIncoming);
  const incomingWorker = resolveTimelineWorker(normalizedIncoming);
  const incomingEventSeq = resolveMessageEventSeq(normalizedIncoming);
  const incomingCardStreamSeq = resolveTimelineCardStreamSeqFromMetadata(resolveMessageMetadata(normalizedIncoming));
  return {
    itemId: existing.itemId,
    anchorEventSeq: existing.anchorEventSeq || incomingEventSeq,
    latestEventSeq: Math.max(existing.latestEventSeq, incomingEventSeq),
    cardStreamSeq: Math.max(existing.cardStreamSeq, incomingCardStreamSeq),
    timestamp: mergeTimelineSortTimestamp(existing.timestamp, normalizedIncoming),
    worker: incomingWorker || existing.worker,
    threadVisible: existing.threadVisible || incomingVisibility.threadVisible,
    workerTabs: Array.from(new Set([
      ...existing.workerTabs,
      ...incomingVisibility.workerTabs,
    ])),
    messageIds: Array.from(new Set([
      ...existing.messageIds,
      ...resolveProjectionSourceMessageIds(incomingMessage),
    ])),
    message: normalizedIncoming,
  };
}

function compareExecutionItems(
  left: ProjectionExecutionItemAccumulator,
  right: ProjectionExecutionItemAccumulator,
): number {
  return compareProjectionMessages(left.message, right.message);
}

function buildFragmentExecutionItems(
  messages: SessionTimelineProjectionMessage[],
): ProjectionExecutionItemAccumulator[] {
  return messages
    .map((message) => createExecutionItem(message))
    .sort(compareExecutionItems);
}

function finalizeExecutionItems(
  items: ProjectionExecutionItemAccumulator[],
): SessionTimelineProjectionExecutionItem[] {
  return items
    .slice()
    .sort(compareExecutionItems)
    .map((item, index) => ({
      ...item,
      itemOrder: index + 1,
    }));
}

function createLifecycleProjectionArtifact(lifecycleKey: string): ProjectionArtifactAccumulator {
  return {
    artifactId: `lifecycle:${lifecycleKey}`,
    kind: 'worker_lifecycle',
    artifactVersion: 0,
    anchorEventSeq: 0,
    latestEventSeq: 0,
    cardStreamSeq: 0,
    timestamp: 0,
    lifecycleKey,
    dispatchWaveId: undefined,
    laneId: undefined,
    workerCardId: undefined,
    threadVisible: true,
    workerTabs: [],
    messageIds: [],
    message: null,
    executionItems: [],
  };
}

function buildProjectionArtifact(message: SessionTimelineProjectionMessage): ProjectionArtifactAccumulator {
  const stableId = resolveArtifactId(message);
  const worker = resolveTimelineWorker(message);
  const visibility = resolveWorkerVisibility(message);
  const messageEventSeq = resolveMessageEventSeq(message);
  const cardStreamSeq = resolveTimelineCardStreamSeqFromMetadata(resolveMessageMetadata(message));
  return {
    artifactId: stableId,
    kind: resolveArtifactKind(message),
    artifactVersion: resolveMessageVersionCursor(message),
    anchorEventSeq: messageEventSeq,
    latestEventSeq: messageEventSeq,
    cardStreamSeq,
    timestamp: resolveMessageSortTimestamp(message),
    cardId: resolveTimelineCardId(message),
    lifecycleKey: resolveTimelineLifecycleKey(message),
    dispatchWaveId: resolveProjectionDispatchWaveId(message),
    laneId: resolveProjectionLaneId(message),
    workerCardId: resolveProjectionWorkerCardId(message),
    worker,
    threadVisible: visibility.threadVisible,
    workerTabs: visibility.workerTabs,
    messageIds: Array.from(new Set([stableId, ...resolveProjectionSourceMessageIds(message)])),
    message: normalizeProjectionMessage({ ...message, id: stableId }),
    executionItems: [],
  };
}

function mergeStandaloneProjectionArtifact(
  existing: ProjectionArtifactAccumulator,
  incomingMessage: SessionTimelineProjectionMessage,
): ProjectionArtifactAccumulator {
  const stableId = existing.artifactId;
  const normalizedIncoming = normalizeProjectionMessage({ ...incomingMessage, id: stableId });
  const incomingVisibility = resolveWorkerVisibility(normalizedIncoming);
  const incomingWorker = resolveTimelineWorker(normalizedIncoming);
  const incomingEventSeq = resolveMessageEventSeq(normalizedIncoming);
  const incomingCardStreamSeq = resolveTimelineCardStreamSeqFromMetadata(resolveMessageMetadata(normalizedIncoming));
  return {
    artifactId: stableId,
    kind: existing.kind,
    artifactVersion: Math.max(existing.artifactVersion, resolveMessageVersionCursor(normalizedIncoming)),
    anchorEventSeq: existing.anchorEventSeq || incomingEventSeq,
    latestEventSeq: Math.max(existing.latestEventSeq, incomingEventSeq),
    cardStreamSeq: Math.max(existing.cardStreamSeq, incomingCardStreamSeq),
    timestamp: mergeTimelineSortTimestamp(existing.timestamp, normalizedIncoming),
    cardId: resolveTimelineCardId(normalizedIncoming) || existing.cardId,
    lifecycleKey: resolveTimelineLifecycleKey(normalizedIncoming) || existing.lifecycleKey,
    dispatchWaveId: resolveProjectionDispatchWaveId(normalizedIncoming) || existing.dispatchWaveId,
    laneId: resolveProjectionLaneId(normalizedIncoming) || existing.laneId,
    workerCardId: resolveProjectionWorkerCardId(normalizedIncoming) || existing.workerCardId,
    worker: incomingWorker || existing.worker,
    threadVisible: existing.threadVisible || incomingVisibility.threadVisible,
    workerTabs: Array.from(new Set([
      ...existing.workerTabs,
      ...incomingVisibility.workerTabs,
    ])),
    messageIds: Array.from(new Set([
      ...existing.messageIds,
      incomingMessage.id,
    ])),
    message: normalizedIncoming,
    executionItems: existing.executionItems,
  };
}

function mergeLifecycleProjectionArtifact(
  existing: ProjectionArtifactAccumulator,
  incomingMessage: SessionTimelineProjectionMessage,
): ProjectionArtifactAccumulator {
  const normalizedIncoming = normalizeProjectionMessage(incomingMessage);
  const incomingVisibility = resolveWorkerVisibility(normalizedIncoming);
  const incomingWorker = resolveTimelineWorker(normalizedIncoming);
  const incomingEventSeq = resolveMessageEventSeq(normalizedIncoming);
  const incomingCardStreamSeq = resolveTimelineCardStreamSeqFromMetadata(resolveMessageMetadata(normalizedIncoming));
  const nextMessageIds = Array.from(new Set([
    ...existing.messageIds,
    ...resolveProjectionSourceMessageIds(incomingMessage),
  ]));

  if (isWorkerLifecycleExecutionMessage(normalizedIncoming)) {
    const nextExecutionItems = (() => {
      const existingIndex = existing.executionItems.findIndex((item) => (
        item.itemId === normalizedIncoming.id
        || item.messageIds.includes(normalizedIncoming.id)
      ));
      if (existingIndex < 0) {
        return [...existing.executionItems, createExecutionItem(normalizedIncoming)];
      }
      const next = [...existing.executionItems];
      next[existingIndex] = mergeExecutionItem(next[existingIndex], normalizedIncoming);
      return next;
    })();
    return {
      ...existing,
      artifactVersion: Math.max(existing.artifactVersion, resolveMessageVersionCursor(normalizedIncoming)),
      latestEventSeq: Math.max(existing.latestEventSeq, incomingEventSeq),
      cardStreamSeq: Math.max(existing.cardStreamSeq, incomingCardStreamSeq),
      worker: incomingWorker || existing.worker,
      workerTabs: Array.from(new Set([
        ...existing.workerTabs,
        ...incomingVisibility.workerTabs,
      ])),
      messageIds: nextMessageIds,
      executionItems: nextExecutionItems,
    };
  }

  const stableId = existing.artifactId;
  const incomingForCard = normalizeProjectionMessage({ ...incomingMessage, id: stableId });
  const mergedMessage = existing.message
    ? mergeLifecycleMessage(existing.message, incomingForCard, stableId)
    : incomingForCard;
  return {
    artifactId: stableId,
    kind: 'worker_lifecycle',
    artifactVersion: Math.max(existing.artifactVersion, resolveMessageVersionCursor(incomingForCard)),
    anchorEventSeq: existing.anchorEventSeq || incomingEventSeq,
    latestEventSeq: Math.max(existing.latestEventSeq, incomingEventSeq),
    cardStreamSeq: Math.max(existing.cardStreamSeq, incomingCardStreamSeq),
    timestamp: existing.message
      ? mergeTimelineSortTimestamp(existing.timestamp, mergedMessage)
      : resolveMessageSortTimestamp(mergedMessage),
    cardId: resolveTimelineCardId(mergedMessage) || existing.cardId,
    lifecycleKey: resolveTimelineLifecycleKey(mergedMessage) || existing.lifecycleKey,
    dispatchWaveId: resolveProjectionDispatchWaveId(mergedMessage) || existing.dispatchWaveId,
    laneId: resolveProjectionLaneId(mergedMessage) || existing.laneId,
    workerCardId: resolveProjectionWorkerCardId(mergedMessage) || existing.workerCardId,
    worker: resolveTimelineWorker(mergedMessage) || existing.worker || incomingWorker,
    threadVisible: existing.threadVisible || incomingVisibility.threadVisible,
    workerTabs: Array.from(new Set([
      ...existing.workerTabs,
      ...incomingVisibility.workerTabs,
    ])),
    messageIds: nextMessageIds,
    message: mergedMessage,
    executionItems: existing.executionItems,
  };
}

function collectLifecycleArtifactKeys(
  messages: SessionTimelineProjectionMessage[],
): Set<string> {
  const lifecyclePresence = new Map<string, boolean>();
  for (const message of messages) {
    const lifecycleKey = resolveProjectionTaskKey(message);
    if (!lifecycleKey) {
      continue;
    }
    const hasLifecycle = isWorkerLifecycleMessage(message);
    lifecyclePresence.set(lifecycleKey, Boolean(lifecyclePresence.get(lifecycleKey)) || hasLifecycle);
  }
  return new Set(
    Array.from(lifecyclePresence.entries())
      .filter(([, hasLifecycle]) => hasLifecycle)
      .map(([lifecycleKey]) => lifecycleKey),
  );
}

export function buildSessionTimelineProjection(session: ProjectionSourceSession): SessionTimelineProjection {
  const sourceMessages = session.messages
    .filter((message) => (
      message
      && typeof message.id === 'string'
      && message.id.trim().length > 0
      && typeof message.role === 'string'
      && typeof message.content === 'string'
      && typeof message.timestamp === 'number'
      && Number.isFinite(message.timestamp)
      && !isInternalControlTimelineMessage(message)
      && messageHasRenderableContent(message)
    ));
  const normalizedMessages = backfillUserRequestAnchorTimestamps(sourceMessages)
    .map((message) => normalizeProjectionMessage(message))
    .sort(compareProjectionMessages);

  const lifecycleArtifactKeys = collectLifecycleArtifactKeys(normalizedMessages);
  const artifactsById = new Map<string, ProjectionArtifactAccumulator>();
  for (const message of normalizedMessages) {
    const fragmentMessages = resolveTimelineFragmentMessages(message);
    const usesFragmentExecutionItems = fragmentMessages.length > 1;
    const artifactMessage = usesFragmentExecutionItems
      ? setTimelineContainerFlag(message, true)
      : setTimelineContainerFlag(message, false);
    const projectionTaskKey = resolveProjectionTaskKey(message);
    const shouldRouteToLifecycleArtifact = isWorkerLifecycleMessage(message) || isWorkerLifecycleExecutionMessage(message);
    const lifecycleArtifactKey = projectionTaskKey && lifecycleArtifactKeys.has(projectionTaskKey) && shouldRouteToLifecycleArtifact
      ? projectionTaskKey
      : undefined;
    const artifactId = lifecycleArtifactKey
      ? `lifecycle:${lifecycleArtifactKey}`
      : resolveArtifactId(message);
    const existing = artifactsById.get(artifactId);
    if (!existing) {
      if (lifecycleArtifactKey) {
        artifactsById.set(
          artifactId,
          mergeLifecycleProjectionArtifact(createLifecycleProjectionArtifact(lifecycleArtifactKey), message),
        );
      } else {
        const nextArtifact = buildProjectionArtifact(artifactMessage);
        artifactsById.set(artifactId, {
          ...nextArtifact,
          executionItems: usesFragmentExecutionItems ? buildFragmentExecutionItems(fragmentMessages) : [],
        });
      }
      continue;
    }
    if (lifecycleArtifactKey) {
      artifactsById.set(
        artifactId,
        mergeLifecycleProjectionArtifact(existing, message),
      );
      continue;
    }
    const nextArtifact = mergeStandaloneProjectionArtifact(existing, artifactMessage);
    artifactsById.set(artifactId, {
      ...nextArtifact,
      executionItems: usesFragmentExecutionItems ? buildFragmentExecutionItems(fragmentMessages) : [],
    });
  }

  const artifacts = Array.from(artifactsById.values())
    .sort(compareProjectionArtifacts)
    .map((artifact, index) => {
      const finalizedExecutionItems = finalizeExecutionItems(artifact.executionItems);
      const executionVersion = finalizedExecutionItems.reduce(
        (maxVersion, item) => Math.max(maxVersion, resolveMessageVersionCursor(item.message)),
        0,
      );
      return {
        artifactId: artifact.artifactId,
        kind: artifact.kind,
        displayOrder: index + 1,
        artifactVersion: Math.max(artifact.artifactVersion, executionVersion),
        anchorEventSeq: artifact.anchorEventSeq,
        latestEventSeq: artifact.latestEventSeq,
        cardStreamSeq: artifact.cardStreamSeq,
        timestamp: artifact.timestamp,
        ...(artifact.cardId ? { cardId: artifact.cardId } : {}),
        ...(artifact.lifecycleKey ? { lifecycleKey: artifact.lifecycleKey } : {}),
        ...(artifact.dispatchWaveId ? { dispatchWaveId: artifact.dispatchWaveId } : {}),
        ...(artifact.laneId ? { laneId: artifact.laneId } : {}),
        ...(artifact.workerCardId ? { workerCardId: artifact.workerCardId } : {}),
        ...(artifact.worker ? { worker: artifact.worker } : {}),
        threadVisible: artifact.threadVisible,
        workerTabs: artifact.workerTabs,
        messageIds: artifact.messageIds,
        message: artifact.message || finalizedExecutionItems[0]?.message || normalizeProjectionMessage({
          id: artifact.artifactId,
          role: 'assistant',
          content: '',
          source: artifact.worker || 'orchestrator',
          timestamp: artifact.timestamp || session.updatedAt,
          updatedAt: artifact.timestamp || session.updatedAt,
          type: 'text',
        }),
        executionItems: finalizedExecutionItems,
      };
    });

  const threadRenderEntries = buildProjectionPanelRenderEntries(artifacts, 'thread');
  const workerRenderEntries: Record<WorkerSlot, SessionTimelineProjectionRenderEntry[]> = {
    claude: buildProjectionPanelRenderEntries(artifacts, 'worker', 'claude'),
    codex: buildProjectionPanelRenderEntries(artifacts, 'worker', 'codex'),
    gemini: buildProjectionPanelRenderEntries(artifacts, 'worker', 'gemini'),
  };

  const lastAppliedEventSeq = artifacts.reduce((maxSeq, artifact) => Math.max(maxSeq, artifact.latestEventSeq), 0);

  return {
    schemaVersion: 'session-timeline-projection.v2',
    sessionId: session.id,
    updatedAt: session.updatedAt,
    lastAppliedEventSeq,
    artifacts,
    threadRenderEntries,
    workerRenderEntries,
  };
}

export function isSessionTimelineProjection(value: unknown): value is SessionTimelineProjection {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return false;
  }
  const record = value as Record<string, unknown>;
  if (record.schemaVersion !== 'session-timeline-projection.v2') {
    return false;
  }
  if (typeof record.sessionId !== 'string' || record.sessionId.trim().length === 0) {
    return false;
  }
  if (typeof record.updatedAt !== 'number' || !Number.isFinite(record.updatedAt)) {
    return false;
  }
  if (typeof record.lastAppliedEventSeq !== 'number' || !Number.isFinite(record.lastAppliedEventSeq)) {
    return false;
  }
  if (!Array.isArray(record.artifacts)) {
    return false;
  }
  if (!Array.isArray(record.threadRenderEntries)) {
    return false;
  }
  if (!record.workerRenderEntries || typeof record.workerRenderEntries !== 'object' || Array.isArray(record.workerRenderEntries)) {
    return false;
  }
  for (const worker of WORKER_SLOTS) {
    const workerRenderEntries = (record.workerRenderEntries as Record<string, unknown>)[worker];
    if (!Array.isArray(workerRenderEntries)) {
      return false;
    }
  }
  return true;
}
