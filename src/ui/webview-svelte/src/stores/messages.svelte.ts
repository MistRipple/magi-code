/**
 * 消息状态管理 - Svelte 5 Runes
 * 使用细粒度响应式实现高效的流式更新
 */

import type {
  Message,
  AgentOutputs,
  AgentType,
  TimelineExecutionItem,
  TimelineProjectionArtifact,
  TimelineProjectionRenderEntry,
  TimelineNode,
  TimelineNodeKind,
  MissionPlan,
  SessionTimelineProjection,
  Session,
  TabType,
  ProcessingActor,
  ScrollPositions,
  ScrollAnchors,
  ScrollAnchor,
  AutoScrollConfig,
  AppState,
  WebviewPersistedState,
  PersistedSessionViewState,
  WaveState,
  RequestResponseBinding,
  RetryRuntimeState,
  ModelStatusMap,
  Task,
  QueuedMessage,
  WaitForWorkersResult,
  OrchestratorRuntimeState,
  SessionNotificationRecord,
} from '../types/message';
import { vscode } from '../lib/vscode-bridge';
import { ensureArray } from '../lib/utils';
import { i18n } from './i18n.svelte';
import { deriveWorkerRuntimeMap } from '../lib/worker-panel-state';
import { normalizeWorkerSlot } from '../lib/message-classifier';
import {
  buildTimelineNodeLookup,
  buildTimelinePanelMessages,
  buildTimelineRenderItems,
} from '../lib/timeline-render-items';
import {
  compareTimelineSemanticOrder,
  resolveTimelineBlockSeqFromMetadata,
  resolveTimelineCardStreamSeqFromMetadata,
  resolveTimelineEventSeqFromMetadata,
  resolveStableTimelinePlacementTimestamp,
  resolveTimelineSortTimestamp,
  resolveTimelineVersionFromMetadata,
} from '../../../../shared/timeline-ordering';
import {
  isTimelineWorkerLifecycleMessageType,
  messageHasRenderableTimelineContent,
  resolveTimelinePrimaryToolCallName,
  resolveTimelinePresentationKind,
  resolveTimelineWorkerVisibility as resolveSharedTimelineWorkerVisibility,
} from '../../../../shared/timeline-presentation';
import { resolveTimelineWorkerLifecycleKey } from '../../../../shared/timeline-worker-lifecycle';
import {
  expandRenderableTimelineMessages,
  type TimelineFragmentMessage,
} from '../../../../shared/timeline-message-fragmentation';
import {
  normalizeMessagePayload,
  sanitizeMessagePatch,
} from '../lib/message-payload';

// ============ 状态定义 ============
// 🔧 修复：使用对象属性模式确保跨模块响应式正常工作
// Svelte 5 官方推荐：导出对象并修改其属性，而非重新赋值独立变量

/**
 * 核心消息状态
 * 使用对象属性模式确保跨模块响应式追踪
 */
export const messagesState = $state({
  // Tab 状态
  currentTopTab: 'thread' as TabType,
  currentBottomTab: 'thread' as TabType,
  messageJump: {
    messageId: null as string | null,
    nonce: 0,
  },

  // 消息状态
  timelineNodes: [] as TimelineNode[],
  timelineProjection: null as SessionTimelineProjection | null,

  // 会话状态
  sessions: [] as Session[],
  currentSessionId: null as string | null,
  queuedMessages: [] as QueuedMessage[],

  // 处理状态
  isProcessing: false,
  backendProcessing: false,
  activeMessageIds: new Set<string>(),
  pendingRequests: new Set<string>(),
  thinkingStartAt: null as number | null,
  // 防回抬保护：记录最后一次强制 idle 的时间戳
  lastForcedIdleAt: null as number | null,
  processingActor: {
    source: 'orchestrator',
    agent: 'claude',
  } as ProcessingActor,

  // 后端下发的完整状态
  appState: null as AppState | null,
  orchestratorRuntimeState: null as OrchestratorRuntimeState | null,

  // 滚动状态
  scrollPositions: {
    thread: 0,
    claude: 0,
    codex: 0,
    gemini: 0,
  } as ScrollPositions,
  scrollAnchors: {
    thread: { messageId: null, offsetTop: 0 },
    claude: { messageId: null, offsetTop: 0 },
    codex: { messageId: null, offsetTop: 0 },
    gemini: { messageId: null, offsetTop: 0 },
  } as ScrollAnchors,
  autoScrollEnabled: {
    thread: true,
    claude: true,
    codex: true,
    gemini: true,
  } as AutoScrollConfig,
});

// 消息列表限制
const IS_HOSTED_WEBVIEW = (
  typeof globalThis !== 'undefined'
  && typeof (globalThis as { acquireVsCodeApi?: unknown }).acquireVsCodeApi === 'function'
);
const MAX_TIMELINE_NODES = IS_HOSTED_WEBVIEW ? 1000 : 500;

const MAX_PERSISTED_ARRAY_LENGTH = 10000;
const WEBVIEW_STATE_SAVE_DEBOUNCE_MS = IS_HOSTED_WEBVIEW ? 120 : 900;

type ScrollPanelKey = keyof ScrollPositions;

const DEFAULT_SCROLL_ANCHOR: ScrollAnchor = { messageId: null, offsetTop: 0 };

function createDefaultScrollAnchors(): ScrollAnchors {
  return {
    thread: { ...DEFAULT_SCROLL_ANCHOR },
    claude: { ...DEFAULT_SCROLL_ANCHOR },
    codex: { ...DEFAULT_SCROLL_ANCHOR },
    gemini: { ...DEFAULT_SCROLL_ANCHOR },
  };
}

function createDefaultScrollPositions(): ScrollPositions {
  return {
    thread: 0,
    claude: 0,
    codex: 0,
    gemini: 0,
  };
}

function createDefaultAutoScrollConfig(): AutoScrollConfig {
  return {
    thread: true,
    claude: true,
    codex: true,
    gemini: true,
  };
}

function normalizeSessionId(value: string | null | undefined): string | null {
  const sessionId = typeof value === 'string' ? value.trim() : '';
  return sessionId || null;
}

function normalizePersistedScrollPositions(value: unknown): ScrollPositions {
  const defaults = createDefaultScrollPositions();
  if (!value || typeof value !== 'object') {
    return defaults;
  }
  const source = value as Partial<Record<ScrollPanelKey, unknown>>;
  return {
    thread: normalizeScrollTop(typeof source.thread === 'number' ? source.thread : 0),
    claude: normalizeScrollTop(typeof source.claude === 'number' ? source.claude : 0),
    codex: normalizeScrollTop(typeof source.codex === 'number' ? source.codex : 0),
    gemini: normalizeScrollTop(typeof source.gemini === 'number' ? source.gemini : 0),
  };
}

function normalizePersistedScrollAnchors(value: unknown): ScrollAnchors {
  const defaults = createDefaultScrollAnchors();
  if (!value || typeof value !== 'object') {
    return defaults;
  }
  const source = value as Partial<Record<ScrollPanelKey, unknown>>;
  return {
    thread: normalizeScrollAnchor(source.thread as ScrollAnchor | null | undefined),
    claude: normalizeScrollAnchor(source.claude as ScrollAnchor | null | undefined),
    codex: normalizeScrollAnchor(source.codex as ScrollAnchor | null | undefined),
    gemini: normalizeScrollAnchor(source.gemini as ScrollAnchor | null | undefined),
  };
}

function normalizePersistedAutoScrollConfig(value: unknown): AutoScrollConfig {
  const defaults = createDefaultAutoScrollConfig();
  if (!value || typeof value !== 'object') {
    return defaults;
  }
  const source = value as Partial<Record<ScrollPanelKey, unknown>>;
  return {
    thread: typeof source.thread === 'boolean' ? source.thread : defaults.thread,
    claude: typeof source.claude === 'boolean' ? source.claude : defaults.claude,
    codex: typeof source.codex === 'boolean' ? source.codex : defaults.codex,
    gemini: typeof source.gemini === 'boolean' ? source.gemini : defaults.gemini,
  };
}

function normalizePersistedTimelineProjection(
  value: unknown,
  expectedSessionId: string | null,
): SessionTimelineProjection | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return null;
  }
  const record = value as Record<string, unknown>;
  if (record.schemaVersion !== 'session-timeline-projection.v2') {
    return null;
  }
  const sessionId = typeof record.sessionId === 'string' ? record.sessionId.trim() : '';
  if (!sessionId) {
    return null;
  }
  if (expectedSessionId && sessionId !== expectedSessionId) {
    return null;
  }
  if (!Array.isArray(record.artifacts) || !Array.isArray(record.threadRenderEntries)) {
    return null;
  }
  if (!record.workerRenderEntries || typeof record.workerRenderEntries !== 'object' || Array.isArray(record.workerRenderEntries)) {
    return null;
  }
  for (const worker of ['claude', 'codex', 'gemini'] as const) {
    if (!Array.isArray((record.workerRenderEntries as Record<string, unknown>)[worker])) {
      return null;
    }
  }
  return record as unknown as SessionTimelineProjection;
}

function normalizePersistedSessionViewState(
  sessionId: string,
  value: unknown,
): PersistedSessionViewState | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return null;
  }
  const record = value as Record<string, unknown>;
  const normalizedSessionId = normalizeSessionId(typeof record.sessionId === 'string' ? record.sessionId : sessionId);
  if (!normalizedSessionId || normalizedSessionId !== sessionId) {
    return null;
  }
  const timelineProjection = normalizePersistedTimelineProjection(record.timelineProjection, normalizedSessionId);
  if (!timelineProjection) {
    return null;
  }
  return {
    sessionId: normalizedSessionId,
    timelineProjection,
    scrollPositions: normalizePersistedScrollPositions(record.scrollPositions),
    scrollAnchors: normalizePersistedScrollAnchors(record.scrollAnchors),
    autoScrollEnabled: normalizePersistedAutoScrollConfig(record.autoScrollEnabled),
  };
}

function normalizePersistedSessionViewStateMap(
  value: unknown,
): Record<string, PersistedSessionViewState> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return {};
  }
  const normalized: Record<string, PersistedSessionViewState> = {};
  let count = 0;
  for (const [rawSessionId, rawViewState] of Object.entries(value as Record<string, unknown>)) {
    if (count >= MAX_PERSISTED_ARRAY_LENGTH) {
      break;
    }
    const sessionId = normalizeSessionId(rawSessionId);
    if (!sessionId) {
      continue;
    }
    const next = normalizePersistedSessionViewState(sessionId, rawViewState);
    if (!next) {
      continue;
    }
    normalized[sessionId] = next;
    count += 1;
  }
  return normalized;
}

function clonePersistablePayload<T>(value: T): T | null {
  if (value === null || value === undefined) {
    return null;
  }
  try {
    return JSON.parse(JSON.stringify(value)) as T;
  } catch {
    return null;
  }
}

function resetPanelScrollRuntimeState(): void {
  messagesState.scrollPositions = createDefaultScrollPositions();
  messagesState.scrollAnchors = createDefaultScrollAnchors();
  messagesState.autoScrollEnabled = createDefaultAutoScrollConfig();
}

let deferredWebviewStateSaveTimer: ReturnType<typeof setTimeout> | null = null;
let sessionViewStateBySession = $state<Record<string, PersistedSessionViewState>>({});
let webviewStateBatchDepth = 0;
let webviewStateBatchPending = false;

function scheduleSaveWebviewState(): void {
  if (webviewStateBatchDepth > 0) {
    webviewStateBatchPending = true;
    return;
  }
  if (deferredWebviewStateSaveTimer) {
    clearTimeout(deferredWebviewStateSaveTimer);
  }
  deferredWebviewStateSaveTimer = setTimeout(() => {
    deferredWebviewStateSaveTimer = null;
    saveWebviewState();
  }, WEBVIEW_STATE_SAVE_DEBOUNCE_MS);
}

function compareTimelineProjectionFreshness(
  left: Pick<SessionTimelineProjection, 'lastAppliedEventSeq' | 'updatedAt' | 'artifacts'> | null | undefined,
  right: Pick<SessionTimelineProjection, 'lastAppliedEventSeq' | 'updatedAt' | 'artifacts'> | null | undefined,
): number {
  const leftEventSeq = typeof left?.lastAppliedEventSeq === 'number' ? left.lastAppliedEventSeq : 0;
  const rightEventSeq = typeof right?.lastAppliedEventSeq === 'number' ? right.lastAppliedEventSeq : 0;
  if (leftEventSeq !== rightEventSeq) {
    return leftEventSeq - rightEventSeq;
  }

  const leftArtifactCount = Array.isArray(left?.artifacts) ? left.artifacts.length : 0;
  const rightArtifactCount = Array.isArray(right?.artifacts) ? right.artifacts.length : 0;
  return leftArtifactCount - rightArtifactCount;
}

function normalizeScrollTop(value: number): number {
  if (!Number.isFinite(value) || value <= 0) {
    return 0;
  }
  return Math.round(value);
}

function normalizeScrollAnchor(value: ScrollAnchor | null | undefined): ScrollAnchor {
  if (!value || typeof value !== 'object') {
    return { ...DEFAULT_SCROLL_ANCHOR };
  }
  const messageId = typeof value.messageId === 'string' && value.messageId.trim().length > 0
    ? value.messageId.trim()
    : null;
  const offsetTop = Number.isFinite(value.offsetTop) ? Math.round(value.offsetTop) : 0;
  return {
    messageId,
    offsetTop,
  };
}

function isValidPersistedArray(value: unknown, max: number): value is unknown[] {
  if (!Array.isArray(value)) return false;
  const length = value.length;
  if (!Number.isFinite(length) || length < 0 || length > max) return false;
  return true;
}

// 新增状态：任务、变更、阶段、Toast、模型状态
let tasks = $state<Task[]>([]);
let edits = $state<Array<{ filePath: string; snapshotId?: string; type?: string; additions?: number; deletions?: number; contributors?: string[]; workerId?: string; missionId?: string }>>([]);
export type WorkerWaitResult = WaitForWorkersResult;
export type WorkerWaitResultMap = Record<string, WorkerWaitResult | null>;

let workerWaitResults = $state<WorkerWaitResultMap>({});
let timelineProjectionDirty = false;

const timelineNodeLookup = $derived.by(() => buildTimelineNodeLookup(messagesState.timelineNodes));

// 统一 Worker 运行态（唯一权威来源）
const messageProjection = $derived.by(() => ({
  thread: buildTimelinePanelMessages(timelineNodeLookup, messagesState.timelineProjection, 'thread'),
  workers: {
    claude: buildTimelinePanelMessages(timelineNodeLookup, messagesState.timelineProjection, 'worker', 'claude'),
    codex: buildTimelinePanelMessages(timelineNodeLookup, messagesState.timelineProjection, 'worker', 'codex'),
    gemini: buildTimelinePanelMessages(timelineNodeLookup, messagesState.timelineProjection, 'worker', 'gemini'),
  },
}));

const workerRuntime = $derived.by(() => deriveWorkerRuntimeMap({
  messagesByWorker: {
    claude: messageProjection.workers.claude,
    codex: messageProjection.workers.codex,
    gemini: messageProjection.workers.gemini,
  },
  pendingRequestIds: messagesState.pendingRequests,
  tasks,
  runtimeState: messagesState.orchestratorRuntimeState,
}));

export type ToastCategory = 'incident' | 'audit' | 'feedback';
export type NotificationCategory = 'incident' | 'audit';
export type ToastDisplayMode = 'toast' | 'notification_center' | 'silent';

export interface ToastOptions {
  category?: ToastCategory;
  source?: string;
  actionRequired?: boolean;
  persistToCenter?: boolean;
  countUnread?: boolean;
  displayMode?: ToastDisplayMode;
  duration?: number;
}

interface ToastRecord {
  id: string;
  type: string;
  title?: string;
  message: string;
  category: ToastCategory;
  source?: string;
  actionRequired?: boolean;
  duration?: number;
}

let toasts = $state<ToastRecord[]>([]);

// 通知历史（持久化在会话内，不自动消失）
export interface Notification {
  id: string;
  type: string;
  title?: string;
  message: string;
  category: NotificationCategory;
  source?: string;
  actionRequired?: boolean;
  timestamp: number;
  read: boolean;
}
const MAX_NOTIFICATIONS_PER_SESSION = 200;

let notifications = $state<Notification[]>([]);
let unreadNotificationCount = $state(0);
let notificationsBySession = $state<Record<string, Notification[]>>({});

let modelStatus = $state<ModelStatusMap>({
  claude: { status: 'checking' },
  codex: { status: 'checking' },
  gemini: { status: 'checking' },
  orchestrator: { status: 'checking' },
  auxiliary: { status: 'checking' },
});
const timelineNodeIdByMessageId = new Map<string, string>();
const timelineNodeIdByCardId = new Map<string, string>();
const timelineNodeIdByLifecycleKey = new Map<string, string>();
const timelineExecutionItemTargetByMessageId = new Map<string, { nodeId: string; itemId: string }>();

function resolveMessageMetadataRecord(message: Pick<Message, 'metadata'> | undefined): Record<string, unknown> | undefined {
  return message?.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : undefined;
}

function resolveMessageSortTimestamp(message: Pick<Message, 'timestamp' | 'metadata' | 'type'>): number {
  return resolveTimelineSortTimestamp(message.timestamp, resolveMessageMetadataRecord(message));
}

function mergeTimelineSortTimestamp(
  currentTimestamp: number | undefined,
  message: Pick<Message, 'timestamp' | 'metadata' | 'type'>,
): number {
  return resolveStableTimelinePlacementTimestamp(
    currentTimestamp,
    resolveMessageSortTimestamp(message),
  );
}

function normalizeProjectionRestoredMessage(message: Message): Message {
  return normalizeMessagePayload(message, '[MessagesStore] 投影消息');
}

function normalizeSessionNotificationRecord(raw: unknown): Notification | null {
  if (!raw || typeof raw !== 'object') return null;
  const item = raw as Record<string, unknown>;
  const id = typeof item.notificationId === 'string' ? item.notificationId.trim() : '';
  if (!id) return null;
  const type = typeof item.level === 'string' ? item.level : 'info';
  const message = typeof item.message === 'string' ? item.message : '';
  if (!message) return null;
  const category = item.kind === 'incident' ? 'incident' : item.kind === 'audit' || item.kind === 'center' ? 'audit' : null;
  if (!category) return null;
  const persistToCenter = item.persistToCenter !== false;
  if (!persistToCenter) {
    return null;
  }
  const timestamp = typeof item.createdAt === 'number' && Number.isFinite(item.createdAt)
    ? item.createdAt
    : Date.now();
  const read = Boolean(item.read);
  const title = typeof item.title === 'string' ? item.title : undefined;
  const source = typeof item.source === 'string' ? item.source : undefined;
  const actionRequired = typeof item.actionRequired === 'boolean' ? item.actionRequired : undefined;
  return {
    id,
    type,
    title,
    message,
    category,
    source,
    actionRequired,
    timestamp,
    read,
  };
}

function normalizeSessionNotificationList(raw: unknown): Notification[] {
  if (!isValidPersistedArray(raw, MAX_PERSISTED_ARRAY_LENGTH)) {
    return [];
  }
  const seen = new Set<string>();
  const normalized: Notification[] = [];
  for (const item of raw) {
    const next = normalizeSessionNotificationRecord(item);
    if (!next || seen.has(next.id)) {
      continue;
    }
    seen.add(next.id);
    normalized.push(next);
    if (normalized.length >= MAX_NOTIFICATIONS_PER_SESSION) {
      break;
    }
  }
  return normalized;
}

function normalizeIncomingMessage(message: Message): Message {
  return normalizeMessagePayload(message, '[MessagesStore] 输入消息');
}

function isTimelineContainerOnlyMessage(message: Pick<Message, 'metadata'> | undefined): boolean {
  return resolveMessageMetadataRecord(message)?.timelineContainerOnly === true;
}

function setTimelineContainerFlag(message: Message, containerOnly: boolean): Message {
  const metadata = {
    ...(resolveMessageMetadataRecord(message) || {}),
  };
  if (containerOnly) {
    metadata.timelineContainerOnly = true;
  } else {
    delete metadata.timelineContainerOnly;
  }
  return normalizeIncomingMessage({
    ...message,
    ...(Object.keys(metadata).length > 0 ? { metadata } : { metadata: undefined }),
  });
}

function resolveTimelineFragmentMessages(message: Message): Message[] {
  const fragmentSource = setTimelineContainerFlag(message, false);
  const fragments = expandRenderableTimelineMessages(fragmentSource as unknown as TimelineFragmentMessage) as unknown as Message[];
  if (fragments.length <= 1) {
    return [];
  }
  return fragments.map((fragment, index) => normalizeIncomingMessage({
    ...fragment,
    isStreaming: fragmentSource.isStreaming === true && index === fragments.length - 1,
  }));
}

function normalizeWorkerTabList(workerTabs: AgentType[] | undefined): AgentType[] {
  if (!Array.isArray(workerTabs)) return [];
  const next = new Set<AgentType>();
  for (const worker of workerTabs) {
    if (worker === 'claude' || worker === 'codex' || worker === 'gemini') {
      next.add(worker);
    }
  }
  return Array.from(next);
}

function normalizeTimelineLaneOrder(
  laneOrder: TimelineNode['laneOrder'] | undefined,
): TimelineNode['laneOrder'] | undefined {
  if (!laneOrder || typeof laneOrder !== 'object') {
    return undefined;
  }
  const normalizedThread = typeof laneOrder.thread === 'number' && Number.isFinite(laneOrder.thread)
    ? Math.max(1, Math.floor(laneOrder.thread))
    : undefined;
  const nextWorkers: Partial<Record<AgentType, number>> = {};
  const rawWorkers = laneOrder.workers;
  if (rawWorkers && typeof rawWorkers === 'object') {
    for (const worker of ['claude', 'codex', 'gemini'] as AgentType[]) {
      const rawOrder = rawWorkers[worker];
      if (typeof rawOrder === 'number' && Number.isFinite(rawOrder)) {
        nextWorkers[worker] = Math.max(1, Math.floor(rawOrder));
      }
    }
  }
  if (normalizedThread === undefined && Object.keys(nextWorkers).length === 0) {
    return undefined;
  }
  return {
    ...(normalizedThread !== undefined ? { thread: normalizedThread } : {}),
    ...(Object.keys(nextWorkers).length > 0 ? { workers: nextWorkers } : {}),
  };
}

function resolveTimelineCardId(message: Message): string | undefined {
  const rawCardId = typeof message.metadata?.cardId === 'string' ? message.metadata.cardId.trim() : '';
  if (rawCardId) {
    return rawCardId;
  }
  if (!isTimelineWorkerLifecycleMessageType(message.type)) {
    return undefined;
  }
  const rawWorkerCardId = typeof message.metadata?.workerCardId === 'string'
    ? message.metadata.workerCardId.trim()
    : '';
  return rawWorkerCardId || undefined;
}

function resolveTimelineLifecycleKey(message: Message): string | undefined {
  if (!isTimelineWorkerLifecycleMessageType(message.type)) {
    return undefined;
  }
  const metadata = message.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : undefined;
  const resolved = resolveTimelineWorkerLifecycleKey(metadata, {
    fallbackWorker: message.source,
  });
  if (resolved) return resolved;
  return resolveTimelineCardId(message);
}

function resolveProjectionTaskKey(message: Pick<Message, 'metadata'>): string | undefined {
  const metadata = message.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : undefined;
  const resolved = resolveTimelineWorkerLifecycleKey(metadata);
  return resolved || undefined;
}

function isWorkerLifecycleAttachmentMessage(message: Pick<Message, 'type' | 'source' | 'role' | 'metadata'>): boolean {
  void message;
  // 生命周期卡片只承载任务说明与任务总结。
  // 普通 worker 模型输出、thinking、tool_call、result 必须保持独立时间线节点，
  // 不能再被包进任务卡片，否则会破坏时间轴顺序与语义边界。
  return false;
}

function resolveTimelineWorker(message: Message): AgentType | undefined {
  const worker = normalizeWorkerSlot(
    message.metadata?.worker
      || message.metadata?.assignedWorker
      || message.metadata?.agent
      || message.source
  );
  return worker || undefined;
}

function resolveWorkerVisibility(message: Message): {
  threadVisible: boolean;
  workerTabs: AgentType[];
} {
  const worker = resolveTimelineWorker(message);
  const visibility = resolveSharedTimelineWorkerVisibility({
    hasWorker: Boolean(worker),
    type: message.type,
    source: message.source,
  });
  return {
    threadVisible: visibility.threadVisible,
    workerTabs: visibility.includeWorkerTab && worker ? [worker] : [],
  };
}

function resolveTimelineNodeKind(message: Message): TimelineNodeKind {
  return resolveTimelinePresentationKind(message);
}

function resolveTimelineNodeId(message: Message): string {
  const lifecycleKey = resolveTimelineLifecycleKey(message);
  if (lifecycleKey) {
    return `lifecycle:${lifecycleKey}`;
  }
  return message.id;
}

function resolveTimelineAliasId(rawId: string | undefined): string {
  const normalized = typeof rawId === 'string' ? rawId.trim() : '';
  if (!normalized) return '';
  return timelineNodeIdByMessageId.get(normalized) || normalized;
}

function messageHasRenderableContent(message: Message): boolean {
  return messageHasRenderableTimelineContent(message);
}

function mergeLifecycleTimelineMessage(existing: Message, incoming: Message, stableId: string): Message {
  const existingMetadata = (existing.metadata || {}) as Record<string, unknown>;
  const incomingMetadata = (incoming.metadata || {}) as Record<string, unknown>;
  const existingSubTaskCard = existingMetadata.subTaskCard && typeof existingMetadata.subTaskCard === 'object'
    ? existingMetadata.subTaskCard as Record<string, unknown>
    : undefined;
  const incomingSubTaskCard = incomingMetadata.subTaskCard && typeof incomingMetadata.subTaskCard === 'object'
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
      ...(existingHasRenderableContent ? {} : { content: incoming.content, blocks: incoming.blocks }),
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

  return {
    ...existing,
    ...incoming,
    id: stableId,
    timestamp: existing.timestamp || incoming.timestamp,
    metadata: mergedMetadata,
  };
}

function getMessageBlockSeq(message: Pick<Message, 'metadata'> | undefined): number {
  return resolveTimelineBlockSeqFromMetadata(resolveMessageMetadataRecord(message));
}

function normalizeTimelineExecutionItem(item: TimelineExecutionItem): TimelineExecutionItem {
  const normalizedMessage = normalizeIncomingMessage(item.message);
  const messageEventSeq = getMessageEventSeq(normalizedMessage) ?? 0;
  const anchorEventSeq = typeof item.anchorEventSeq === 'number' && Number.isFinite(item.anchorEventSeq)
    ? Math.max(0, Math.floor(item.anchorEventSeq))
    : messageEventSeq;
  const latestEventSeq = typeof item.latestEventSeq === 'number' && Number.isFinite(item.latestEventSeq)
    ? Math.max(anchorEventSeq, Math.floor(item.latestEventSeq))
    : messageEventSeq;
  const cardStreamSeq = typeof item.cardStreamSeq === 'number' && Number.isFinite(item.cardStreamSeq)
    ? Math.max(0, Math.floor(item.cardStreamSeq))
    : getMessageCardStreamSeq(normalizedMessage);
  const workerTabs = normalizeWorkerTabList(item.workerTabs);
  const itemId = typeof item.itemId === 'string' && item.itemId.trim()
    ? item.itemId.trim()
    : normalizedMessage.id;
  return {
    itemId,
    itemOrder: typeof item.itemOrder === 'number' && Number.isFinite(item.itemOrder)
      ? Math.max(1, Math.floor(item.itemOrder))
      : 1,
    anchorEventSeq,
    latestEventSeq,
    cardStreamSeq,
    timestamp: mergeTimelineSortTimestamp(item.timestamp, normalizedMessage),
    worker: resolveTimelineWorker(normalizedMessage) || item.worker,
    threadVisible: item.threadVisible !== false,
    workerTabs,
    messageIds: Array.from(new Set([
      itemId,
      normalizedMessage.id,
      ...ensureArray<string>(item.messageIds),
    ])),
    message: normalizedMessage,
  };
}

function compareTimelineExecutionItemOrder(left: TimelineExecutionItem, right: TimelineExecutionItem): number {
  return compareTimelineSemanticOrder(
    {
      timestamp: left.timestamp,
      stableId: left.itemId,
      itemOrder: left.itemOrder,
      messageType: left.message?.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(left.message?.blocks),
      anchorEventSeq: left.anchorEventSeq,
      blockSeq: getMessageBlockSeq(left.message),
      cardStreamSeq: left.cardStreamSeq,
    },
    {
      timestamp: right.timestamp,
      stableId: right.itemId,
      itemOrder: right.itemOrder,
      messageType: right.message?.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(right.message?.blocks),
      anchorEventSeq: right.anchorEventSeq,
      blockSeq: getMessageBlockSeq(right.message),
      cardStreamSeq: right.cardStreamSeq,
    },
  );
}

function buildFragmentExecutionItems(
  messages: Message[],
  visibility: { thread?: boolean; workerTabs?: AgentType[] },
): TimelineExecutionItem[] {
  return messages
    .map((message) => buildExecutionItemFromMessage(message, visibility))
    .sort(compareTimelineExecutionItemOrder)
    .map((item, index) => normalizeTimelineExecutionItem({
      ...item,
      itemOrder: index + 1,
    }));
}

function normalizeTimelineNode(node: TimelineNode): TimelineNode {
  const normalizedMessage = normalizeIncomingMessage(node.message);
  const stableNodeId = typeof node.nodeId === 'string' && node.nodeId.trim()
    ? node.nodeId.trim()
    : resolveTimelineNodeId(normalizedMessage);
  const stableMessage = {
    ...normalizedMessage,
    id: stableNodeId,
  };
  const messageEventSeq = getMessageEventSeq(stableMessage);
  const explicitAnchorEventSeq = typeof node.anchorEventSeq === 'number' && Number.isFinite(node.anchorEventSeq)
    ? Math.floor(node.anchorEventSeq)
    : 0;
  const explicitLatestEventSeq = typeof node.latestEventSeq === 'number' && Number.isFinite(node.latestEventSeq)
    ? Math.floor(node.latestEventSeq)
    : 0;
  const anchorEventSeq = explicitAnchorEventSeq > 0
    ? explicitAnchorEventSeq
    : (messageEventSeq ?? 0);
  const latestEventSeq = Math.max(anchorEventSeq, explicitLatestEventSeq, messageEventSeq ?? 0);
  const cardStreamSeq = getMessageCardStreamSeq(stableMessage)
    || (typeof node.cardStreamSeq === 'number' && Number.isFinite(node.cardStreamSeq) ? Math.floor(node.cardStreamSeq) : 0);
  const lifecycleKey = resolveTimelineLifecycleKey(stableMessage);
  const cardId = resolveTimelineCardId(stableMessage);
  const dispatchWaveId = typeof stableMessage.metadata?.dispatchWaveId === 'string'
    ? stableMessage.metadata.dispatchWaveId.trim()
    : (typeof node.dispatchWaveId === 'string' ? node.dispatchWaveId.trim() : '');
  const laneId = typeof stableMessage.metadata?.laneId === 'string'
    ? stableMessage.metadata.laneId.trim()
    : (typeof node.laneId === 'string' ? node.laneId.trim() : '');
  const workerCardId = typeof stableMessage.metadata?.workerCardId === 'string'
    ? stableMessage.metadata.workerCardId.trim()
    : (typeof node.workerCardId === 'string' ? node.workerCardId.trim() : '');
  const worker = resolveTimelineWorker(stableMessage) || node.worker;
  const workerTabs = normalizeWorkerTabList(node.workerTabs);
  const messageIds = Array.from(new Set([
    stableNodeId,
    ...ensureArray<string>(node.messageIds),
  ]));
  const executionItems = ensureArray<TimelineExecutionItem>(node.executionItems)
    .map((item) => normalizeTimelineExecutionItem(item))
    .sort(compareTimelineExecutionItemOrder);
  const laneOrder = normalizeTimelineLaneOrder(node.laneOrder);
  return {
    nodeId: stableNodeId,
    kind: node.kind || resolveTimelineNodeKind(stableMessage),
    displayOrder: typeof node.displayOrder === 'number' && Number.isFinite(node.displayOrder)
      ? Math.max(0, Math.floor(node.displayOrder))
      : undefined,
    ...(laneOrder ? { laneOrder } : {}),
    artifactVersion: typeof node.artifactVersion === 'number' && Number.isFinite(node.artifactVersion)
      ? Math.max(0, Math.floor(node.artifactVersion))
      : resolveTimelineVersionFromMetadata(resolveMessageMetadataRecord(stableMessage)),
    anchorEventSeq,
    latestEventSeq,
    cardStreamSeq,
    timestamp: mergeTimelineSortTimestamp(node.timestamp, stableMessage),
    ...(cardId ? { cardId } : {}),
    ...(lifecycleKey ? { lifecycleKey } : {}),
    ...(dispatchWaveId ? { dispatchWaveId } : {}),
    ...(laneId ? { laneId } : {}),
    ...(workerCardId ? { workerCardId } : {}),
    ...(worker ? { worker } : {}),
    visibleInThread: node.visibleInThread !== false,
    workerTabs,
    messageIds,
    message: stableMessage,
    executionItems,
  };
}

function rebuildTimelineIndexes(): void {
  timelineNodeIdByMessageId.clear();
  timelineNodeIdByCardId.clear();
  timelineNodeIdByLifecycleKey.clear();
  timelineExecutionItemTargetByMessageId.clear();
  for (const node of messagesState.timelineNodes) {
    timelineNodeIdByMessageId.set(node.nodeId, node.nodeId);
    for (const messageId of node.messageIds) {
      if (typeof messageId === 'string' && messageId.trim()) {
        timelineNodeIdByMessageId.set(messageId.trim(), node.nodeId);
      }
    }
    if (node.cardId) {
      timelineNodeIdByCardId.set(node.cardId, node.nodeId);
    }
    if (node.lifecycleKey) {
      timelineNodeIdByLifecycleKey.set(node.lifecycleKey, node.nodeId);
    }
    for (const item of ensureArray<TimelineExecutionItem>(node.executionItems)) {
      timelineNodeIdByMessageId.set(item.itemId, node.nodeId);
      timelineExecutionItemTargetByMessageId.set(item.itemId, { nodeId: node.nodeId, itemId: item.itemId });
      for (const messageId of item.messageIds) {
        if (typeof messageId === 'string' && messageId.trim()) {
          const normalizedId = messageId.trim();
          timelineNodeIdByMessageId.set(normalizedId, node.nodeId);
          timelineExecutionItemTargetByMessageId.set(normalizedId, { nodeId: node.nodeId, itemId: item.itemId });
        }
      }
    }
  }
}

function compareTimelineNodeOrder(left: TimelineNode, right: TimelineNode): number {
  return compareTimelineSemanticOrder(
    {
      timestamp: left.timestamp,
      stableId: left.nodeId,
      displayOrder: left.displayOrder,
      messageType: left.message?.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(left.message?.blocks),
      anchorEventSeq: left.anchorEventSeq,
      blockSeq: getMessageBlockSeq(left.message),
      cardStreamSeq: left.cardStreamSeq,
    },
    {
      timestamp: right.timestamp,
      stableId: right.nodeId,
      displayOrder: right.displayOrder,
      messageType: right.message?.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(right.message?.blocks),
      anchorEventSeq: right.anchorEventSeq,
      blockSeq: getMessageBlockSeq(right.message),
      cardStreamSeq: right.cardStreamSeq,
    },
  );
}

interface LocalProjectionFlatRenderEntry {
  entryId: string;
  artifactId: string;
  executionItemId?: string;
  groupId: string;
  message: Message;
  timestamp: number;
  displayOrder?: number;
  itemOrder?: number;
  anchorEventSeq: number;
  blockSeq: number;
  cardStreamSeq: number;
}

function shouldRenderTimelineNodeHost(
  node: Pick<TimelineNode, 'kind' | 'message' | 'executionItems'>,
): boolean {
  if (node.kind !== 'worker_lifecycle' && ensureArray(node.executionItems).length > 0) {
    return false;
  }
  return !isTimelineContainerOnlyMessage(node.message);
}

function compareLocalProjectionRenderEntry(
  left: LocalProjectionFlatRenderEntry,
  right: LocalProjectionFlatRenderEntry,
): number {
  const sameGroup = left.groupId === right.groupId;
  return compareTimelineSemanticOrder(
    {
      timestamp: left.timestamp,
      stableId: left.entryId,
      displayOrder: sameGroup ? left.displayOrder : undefined,
      itemOrder: sameGroup ? left.itemOrder : undefined,
      messageType: left.message?.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(left.message?.blocks),
      anchorEventSeq: left.anchorEventSeq,
      blockSeq: sameGroup ? left.blockSeq : undefined,
      cardStreamSeq: left.cardStreamSeq,
    },
    {
      timestamp: right.timestamp,
      stableId: right.entryId,
      displayOrder: sameGroup ? right.displayOrder : undefined,
      itemOrder: sameGroup ? right.itemOrder : undefined,
      messageType: right.message?.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(right.message?.blocks),
      anchorEventSeq: right.anchorEventSeq,
      blockSeq: sameGroup ? right.blockSeq : undefined,
      cardStreamSeq: right.cardStreamSeq,
    },
  );
}

function buildProjectionRenderEntriesFromArtifacts(
  artifacts: TimelineProjectionArtifact[],
  displayContext: 'thread' | 'worker',
  worker?: AgentType,
): TimelineProjectionRenderEntry[] {
  const flatEntries: LocalProjectionFlatRenderEntry[] = [];

  for (const artifact of artifacts) {
    const artifactVisible = displayContext === 'thread'
      ? artifact.threadVisible
      : Boolean(worker && artifact.workerTabs.includes(worker));
    if (artifactVisible && shouldRenderTimelineNodeHost({
      kind: artifact.kind as TimelineNodeKind,
      message: artifact.message,
      executionItems: artifact.executionItems,
    })) {
      flatEntries.push({
        entryId: `artifact:${artifact.artifactId}`,
        artifactId: artifact.artifactId,
        groupId: artifact.artifactId,
        message: artifact.message,
        timestamp: artifact.timestamp,
        displayOrder: artifact.displayOrder,
        anchorEventSeq: artifact.anchorEventSeq,
        blockSeq: getMessageBlockSeq(artifact.message),
        cardStreamSeq: artifact.cardStreamSeq,
      });
    }

    for (const item of ensureArray<TimelineExecutionItem>(artifact.executionItems)) {
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
        blockSeq: getMessageBlockSeq(item.message),
        cardStreamSeq: item.cardStreamSeq,
      });
    }
  }

  return flatEntries
    .sort(compareLocalProjectionRenderEntry)
    .map((entry) => ({
      entryId: entry.entryId,
      artifactId: entry.artifactId,
      ...(entry.executionItemId ? { executionItemId: entry.executionItemId } : {}),
    }));
}

function isProjectionArtifact(
  artifact: unknown,
): artifact is SessionTimelineProjection['artifacts'][number] {
  return Boolean(
    artifact
    && typeof artifact === 'object'
    && typeof (artifact as SessionTimelineProjection['artifacts'][number]).artifactId === 'string'
    && (artifact as SessionTimelineProjection['artifacts'][number]).message,
  );
}

function compareProjectionExecutionItemCanonicalOrder(
  left: TimelineExecutionItem,
  right: TimelineExecutionItem,
): number {
  return compareTimelineSemanticOrder(
    {
      timestamp: left.timestamp,
      stableId: left.itemId,
      messageType: left.message?.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(left.message?.blocks),
      anchorEventSeq: left.anchorEventSeq,
      blockSeq: getMessageBlockSeq(left.message),
      cardStreamSeq: left.cardStreamSeq,
    },
    {
      timestamp: right.timestamp,
      stableId: right.itemId,
      messageType: right.message?.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(right.message?.blocks),
      anchorEventSeq: right.anchorEventSeq,
      blockSeq: getMessageBlockSeq(right.message),
      cardStreamSeq: right.cardStreamSeq,
    },
  );
}

function compareProjectionArtifactCanonicalOrder(
  left: TimelineProjectionArtifact,
  right: TimelineProjectionArtifact,
): number {
  return compareTimelineSemanticOrder(
    {
      timestamp: left.timestamp,
      stableId: left.artifactId,
      messageType: left.message?.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(left.message?.blocks),
      anchorEventSeq: left.anchorEventSeq,
      blockSeq: getMessageBlockSeq(left.message),
      cardStreamSeq: left.cardStreamSeq,
    },
    {
      timestamp: right.timestamp,
      stableId: right.artifactId,
      messageType: right.message?.type,
      primaryToolCallName: resolveTimelinePrimaryToolCallName(right.message?.blocks),
      anchorEventSeq: right.anchorEventSeq,
      blockSeq: getMessageBlockSeq(right.message),
      cardStreamSeq: right.cardStreamSeq,
    },
  );
}

function canonicalizeProjectionExecutionItems(
  executionItems: TimelineExecutionItem[] | undefined,
): TimelineExecutionItem[] {
  return ensureArray<TimelineExecutionItem>(executionItems)
    .map((item) => normalizeTimelineExecutionItem({
      ...item,
      message: normalizeProjectionRestoredMessage(item.message),
    }))
    .sort(compareProjectionExecutionItemCanonicalOrder)
    .map((item, index) => normalizeTimelineExecutionItem({
      ...item,
      itemOrder: index + 1,
    }));
}

function canonicalizeTimelineProjection(
  projection: SessionTimelineProjection,
): SessionTimelineProjection {
  const artifacts = ensureArray(projection.artifacts)
    .filter(isProjectionArtifact)
    .map((artifact) => {
      const message = normalizeProjectionRestoredMessage(artifact.message);
      const executionItems = canonicalizeProjectionExecutionItems(artifact.executionItems);
      const artifactMessageId = typeof artifact.artifactId === 'string' && artifact.artifactId.trim()
        ? artifact.artifactId.trim()
        : message.id;
      return {
        artifactId: artifactMessageId,
        kind: artifact.kind || resolveTimelineNodeKind(message),
        displayOrder: typeof artifact.displayOrder === 'number' && Number.isFinite(artifact.displayOrder)
          ? Math.max(0, Math.floor(artifact.displayOrder))
          : 0,
        artifactVersion: typeof artifact.artifactVersion === 'number' && Number.isFinite(artifact.artifactVersion)
          ? Math.max(0, Math.floor(artifact.artifactVersion))
          : resolveTimelineVersionFromMetadata(resolveMessageMetadataRecord(message)),
        anchorEventSeq: typeof artifact.anchorEventSeq === 'number' && Number.isFinite(artifact.anchorEventSeq)
          ? Math.max(0, Math.floor(artifact.anchorEventSeq))
          : (getMessageEventSeq(message) ?? 0),
        latestEventSeq: typeof artifact.latestEventSeq === 'number' && Number.isFinite(artifact.latestEventSeq)
          ? Math.max(0, Math.floor(artifact.latestEventSeq))
          : (getMessageEventSeq(message) ?? 0),
        cardStreamSeq: typeof artifact.cardStreamSeq === 'number' && Number.isFinite(artifact.cardStreamSeq)
          ? Math.max(0, Math.floor(artifact.cardStreamSeq))
          : getMessageCardStreamSeq(message),
        timestamp: typeof artifact.timestamp === 'number' && Number.isFinite(artifact.timestamp)
          ? Math.floor(artifact.timestamp)
          : resolveMessageSortTimestamp(message),
        cardId: artifact.cardId || resolveTimelineCardId(message),
        lifecycleKey: artifact.lifecycleKey || resolveTimelineLifecycleKey(message),
        dispatchWaveId: artifact.dispatchWaveId,
        laneId: artifact.laneId,
        workerCardId: artifact.workerCardId,
        worker: resolveTimelineWorker(message) || artifact.worker,
        threadVisible: artifact.threadVisible !== false,
        workerTabs: normalizeWorkerTabList(artifact.workerTabs),
        messageIds: Array.from(new Set([
          artifactMessageId,
          ...ensureArray<string>(artifact.messageIds),
        ])),
        message: {
          ...message,
          id: artifactMessageId,
        },
        executionItems,
      } satisfies TimelineProjectionArtifact;
    })
    .sort(compareProjectionArtifactCanonicalOrder)
    .map((artifact, index) => ({
      ...artifact,
      displayOrder: index + 1,
    }));

  return {
    ...projection,
    sessionId: normalizeSessionId(projection.sessionId) || projection.sessionId,
    artifacts,
    threadRenderEntries: buildProjectionRenderEntriesFromArtifacts(artifacts, 'thread'),
    workerRenderEntries: {
      claude: buildProjectionRenderEntriesFromArtifacts(artifacts, 'worker', 'claude'),
      codex: buildProjectionRenderEntriesFromArtifacts(artifacts, 'worker', 'codex'),
      gemini: buildProjectionRenderEntriesFromArtifacts(artifacts, 'worker', 'gemini'),
    },
  };
}

function buildLiveTimelineProjection(
  sessionId: string,
  sourceNodes: TimelineNode[],
  seed: SessionTimelineProjection | null,
): SessionTimelineProjection {
  const normalizedNodes = sourceNodes
    .map((node) => normalizeTimelineNode(node))
    .sort(compareTimelineNodeOrder);

  const artifacts: TimelineProjectionArtifact[] = normalizedNodes.map((node, index) => {
    const executionItems = ensureArray<TimelineExecutionItem>(node.executionItems)
      .map((item) => normalizeTimelineExecutionItem(item))
      .sort(compareTimelineExecutionItemOrder)
      .map((item, itemIndex) => normalizeTimelineExecutionItem({
        ...item,
        itemOrder: item.itemOrder || (itemIndex + 1),
      }));

    return {
      artifactId: node.nodeId,
      kind: node.kind,
      displayOrder: index + 1,
      artifactVersion: node.artifactVersion,
      anchorEventSeq: node.anchorEventSeq,
      latestEventSeq: node.latestEventSeq,
      cardStreamSeq: node.cardStreamSeq,
      timestamp: node.timestamp,
      cardId: node.cardId,
      lifecycleKey: node.lifecycleKey,
      dispatchWaveId: node.dispatchWaveId,
      laneId: node.laneId,
      workerCardId: node.workerCardId,
      worker: node.worker,
      threadVisible: node.visibleInThread,
      workerTabs: normalizeWorkerTabList(node.workerTabs),
      messageIds: Array.from(new Set(node.messageIds)),
      message: normalizeIncomingMessage(node.message),
      executionItems,
    };
  });

  const threadRenderEntries = buildProjectionRenderEntriesFromArtifacts(artifacts, 'thread');
  const workerRenderEntries = {
    claude: buildProjectionRenderEntriesFromArtifacts(artifacts, 'worker', 'claude'),
    codex: buildProjectionRenderEntriesFromArtifacts(artifacts, 'worker', 'codex'),
    gemini: buildProjectionRenderEntriesFromArtifacts(artifacts, 'worker', 'gemini'),
  };

  const lastAppliedEventSeq = artifacts.reduce((maxSeq, artifact) => {
    const itemMax = ensureArray<TimelineExecutionItem>(artifact.executionItems)
      .reduce((currentMax, item) => Math.max(currentMax, item.latestEventSeq), 0);
    return Math.max(maxSeq, artifact.latestEventSeq, itemMax);
  }, seed?.lastAppliedEventSeq || 0);

  const updatedAt = artifacts.reduce((maxTimestamp, artifact) => {
    const artifactUpdatedAt = Math.max(
      artifact.message.updatedAt || 0,
      artifact.message.timestamp || 0,
      artifact.timestamp,
    );
    const itemUpdatedAt = ensureArray<TimelineExecutionItem>(artifact.executionItems)
      .reduce((currentMax, item) => Math.max(
        currentMax,
        item.message.updatedAt || 0,
        item.message.timestamp || 0,
        item.timestamp,
      ), 0);
    return Math.max(maxTimestamp, artifactUpdatedAt, itemUpdatedAt);
  }, seed?.updatedAt || 0);

  return {
    schemaVersion: 'session-timeline-projection.v2',
    sessionId,
    updatedAt: updatedAt > 0 ? updatedAt : (seed?.updatedAt || Date.now()),
    lastAppliedEventSeq,
    artifacts,
    threadRenderEntries,
    workerRenderEntries,
  };
}

function syncTimelineProjectionFromNodes(
  sessionId: string | null | undefined,
  options: { persist?: boolean } = {},
): void {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  messagesState.timelineProjection = buildLiveTimelineProjection(
    normalizedSessionId,
    messagesState.timelineNodes,
    messagesState.timelineProjection,
  );
  timelineProjectionDirty = false;
  upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(normalizedSessionId));
  if (options.persist !== false) {
    scheduleSaveWebviewState();
  }
}

function ensureTimelineProjectionSnapshotCurrent(
  sessionId: string | null | undefined,
): SessionTimelineProjection | null {
  if (!timelineProjectionDirty) {
    return messagesState.timelineProjection;
  }
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    return messagesState.timelineProjection;
  }
  syncTimelineProjectionFromNodes(normalizedSessionId, { persist: false });
  return messagesState.timelineProjection;
}

function sortAndSyncTimelineNodes(nextNodes: TimelineNode[]): void {
  const normalized = nextNodes.map((node) => normalizeTimelineNode(node));
  normalized.sort(compareTimelineNodeOrder);
  messagesState.timelineNodes = normalized;
  rebuildTimelineIndexes();
}

function replaceTimelineNodes(nextNodes: TimelineNode[]): void {
  sortAndSyncTimelineNodes(nextNodes);
}

function setTimelineProjectionNodes(nextNodes: TimelineNode[]): void {
  replaceTimelineNodes(nextNodes);
}

function mutateTimelineNodes(mutator: (nodes: TimelineNode[]) => TimelineNode[]): void {
  replaceTimelineNodes(mutator([...messagesState.timelineNodes]));
  syncTimelineProjectionFromNodes(messagesState.currentSessionId, { persist: false });
  scheduleSaveWebviewState();
}

function mergeTimelineNodeAliases(
  existingNode: TimelineNode,
  nextMessage: Message,
  visibility: { thread?: boolean; workerTabs?: AgentType[] },
  options: { replaceMessageId?: string; displayOrder?: number } = {},
): TimelineNode {
  const lifecycleKey = resolveTimelineLifecycleKey(nextMessage);
  const cardId = resolveTimelineCardId(nextMessage);
  const nextWorkerTabs = normalizeWorkerTabList(visibility.workerTabs);
  return normalizeTimelineNode({
    ...existingNode,
    displayOrder: existingNode.displayOrder ?? options.displayOrder,
    cardId: cardId || existingNode.cardId,
    lifecycleKey: lifecycleKey || existingNode.lifecycleKey,
    worker: existingNode.worker || resolveTimelineWorker(nextMessage),
    visibleInThread: existingNode.visibleInThread || visibility.thread !== false,
    workerTabs: [
      ...existingNode.workerTabs,
      ...nextWorkerTabs,
    ],
    messageIds: Array.from(new Set([
      ...existingNode.messageIds,
      existingNode.nodeId,
      nextMessage.id,
      ...(options.replaceMessageId ? [options.replaceMessageId] : []),
    ])),
  });
}

function buildExecutionItemFromMessage(
  message: Message,
  visibility: { thread?: boolean; workerTabs?: AgentType[] },
  options: { replaceMessageId?: string } = {},
): TimelineExecutionItem {
  const nextWorkerTabs = normalizeWorkerTabList(visibility.workerTabs);
  const eventSeq = getMessageEventSeq(message) ?? 0;
  return normalizeTimelineExecutionItem({
    itemId: message.id,
    itemOrder: 1,
    anchorEventSeq: eventSeq,
    latestEventSeq: eventSeq,
    cardStreamSeq: getMessageCardStreamSeq(message),
    timestamp: resolveMessageSortTimestamp(message),
    worker: resolveTimelineWorker(message),
    threadVisible: visibility.thread === true,
    workerTabs: nextWorkerTabs,
    messageIds: Array.from(new Set([
      message.id,
      ...(options.replaceMessageId ? [options.replaceMessageId] : []),
    ])),
    message,
  });
}

function mergeExecutionItemWithMessage(
  existingItem: TimelineExecutionItem,
  message: Message,
  visibility: { thread?: boolean; workerTabs?: AgentType[] },
  options: { replaceMessageId?: string } = {},
): TimelineExecutionItem {
  const nextWorkerTabs = normalizeWorkerTabList(visibility.workerTabs);
  return normalizeTimelineExecutionItem({
    ...existingItem,
    latestEventSeq: Math.max(existingItem.latestEventSeq, getMessageEventSeq(message) ?? existingItem.latestEventSeq),
    cardStreamSeq: Math.max(existingItem.cardStreamSeq, getMessageCardStreamSeq(message)),
    timestamp: mergeTimelineSortTimestamp(existingItem.timestamp, message),
    worker: resolveTimelineWorker(message) || existingItem.worker,
    threadVisible: existingItem.threadVisible || visibility.thread === true,
    workerTabs: [
      ...existingItem.workerTabs,
      ...nextWorkerTabs,
    ],
    messageIds: Array.from(new Set([
      ...existingItem.messageIds,
      message.id,
      ...(options.replaceMessageId ? [options.replaceMessageId] : []),
    ])),
    message,
  });
}

function extractExecutionItemFromNode(node: TimelineNode): TimelineExecutionItem | null {
  if (!isWorkerLifecycleAttachmentMessage(node.message)) {
    return null;
  }
  return normalizeTimelineExecutionItem({
    itemId: node.message.id,
    itemOrder: 1,
    anchorEventSeq: node.anchorEventSeq,
    latestEventSeq: node.latestEventSeq,
    cardStreamSeq: node.cardStreamSeq,
    timestamp: node.timestamp,
    worker: node.worker,
    threadVisible: node.visibleInThread,
    workerTabs: node.workerTabs,
    messageIds: node.messageIds,
    message: node.message,
  });
}

export function upsertTimelineNode(
  message: Message,
  visibility: { thread?: boolean; workerTabs?: AgentType[] },
  options: { replaceMessageId?: string; displayOrder?: number } = {},
): Message {
  const normalizedMessage = normalizeIncomingMessage(message);
  const lifecycleKey = resolveTimelineLifecycleKey(normalizedMessage);
  const attachmentLifecycleKey = resolveProjectionTaskKey(normalizedMessage);
  const cardId = resolveTimelineCardId(normalizedMessage);
  const lifecycleHostNodeId = attachmentLifecycleKey
    ? timelineNodeIdByLifecycleKey.get(attachmentLifecycleKey)
    : undefined;
  const shouldAttachToLifecycle = Boolean(
    lifecycleHostNodeId
    && attachmentLifecycleKey
    && isWorkerLifecycleAttachmentMessage(normalizedMessage),
  );
  const explicitNodeId = shouldAttachToLifecycle
    ? lifecycleHostNodeId!
    : resolveTimelineNodeId(normalizedMessage);
  const replaceNodeId = !shouldAttachToLifecycle && options.replaceMessageId
    ? resolveTimelineAliasId(options.replaceMessageId)
    : undefined;
  const existingNodeId = shouldAttachToLifecycle
    ? lifecycleHostNodeId
    : (replaceNodeId
      || (lifecycleKey ? timelineNodeIdByLifecycleKey.get(lifecycleKey) : undefined)
      || timelineNodeIdByMessageId.get(normalizedMessage.id)
      || (cardId ? timelineNodeIdByCardId.get(cardId) : undefined)
      || undefined);
  const stableNodeId = existingNodeId || explicitNodeId;
  const stableMessage: Message = shouldAttachToLifecycle
    ? normalizedMessage
    : {
        ...normalizedMessage,
        id: stableNodeId,
      };
  const fragmentMessages = shouldAttachToLifecycle ? [] : resolveTimelineFragmentMessages(stableMessage);
  const usesFragmentExecutionItems = fragmentMessages.length > 1;
  const hostMessage = usesFragmentExecutionItems
    ? setTimelineContainerFlag(stableMessage, true)
    : setTimelineContainerFlag(stableMessage, false);
  const nextWorkerTabs = normalizeWorkerTabList(visibility.workerTabs);
  const existingNode = existingNodeId
    ? messagesState.timelineNodes.find((node) => node.nodeId === existingNodeId)
    : undefined;

  if (shouldAttachToLifecycle && existingNode) {
    const existingExecutionItems = ensureArray<TimelineExecutionItem>(existingNode.executionItems);
    const existingItemIndex = existingExecutionItems.findIndex((item) => (
      item.itemId === normalizedMessage.id
      || item.messageIds.includes(normalizedMessage.id)
      || (options.replaceMessageId ? item.messageIds.includes(options.replaceMessageId) : false)
    ));
    const nextExecutionItems = [...existingExecutionItems];
    if (existingItemIndex >= 0) {
      nextExecutionItems[existingItemIndex] = mergeExecutionItemWithMessage(
        nextExecutionItems[existingItemIndex],
        stableMessage,
        visibility,
        options,
      );
    } else {
      nextExecutionItems.push(buildExecutionItemFromMessage(stableMessage, visibility, options));
    }
    const mergedNode = normalizeTimelineNode({
      ...existingNode,
      latestEventSeq: Math.max(existingNode.latestEventSeq, getMessageEventSeq(stableMessage) ?? existingNode.latestEventSeq),
      cardStreamSeq: Math.max(existingNode.cardStreamSeq, getMessageCardStreamSeq(stableMessage)),
      workerTabs: [
        ...existingNode.workerTabs,
        ...nextWorkerTabs,
      ],
      executionItems: nextExecutionItems,
      messageIds: Array.from(new Set([
        ...existingNode.messageIds,
        normalizedMessage.id,
        ...(options.replaceMessageId ? [options.replaceMessageId] : []),
      ])),
    });
    mutateTimelineNodes((nodes) => nodes.map((node) => (
      node.nodeId === stableNodeId ? mergedNode : node
    )));
    return nextExecutionItems[existingItemIndex >= 0 ? existingItemIndex : nextExecutionItems.length - 1].message;
  }

  const nextAnchorEventSeq = getMessageEventSeq(stableMessage)
    ?? (existingNode?.anchorEventSeq || 0);
  const nextLatestEventSeq = getMessageEventSeq(stableMessage)
    ?? (existingNode?.latestEventSeq || nextAnchorEventSeq);
  const nextCardStreamSeq = getMessageCardStreamSeq(stableMessage) || (existingNode?.cardStreamSeq || 0);
  if (existingNode && compareIncomingMessageVersion(existingNode, stableMessage) < 0) {
    const aliasedNode = mergeTimelineNodeAliases(existingNode, stableMessage, visibility, options);
    mutateTimelineNodes((nodes) => nodes.map((node) => (
      node.nodeId === stableNodeId ? aliasedNode : node
    )));
    return aliasedNode.message;
  }
  const mergedMessage = existingNode?.kind === 'worker_lifecycle'
    ? mergeLifecycleTimelineMessage(existingNode.message, hostMessage, stableNodeId)
    : hostMessage;
  const absorbedExecutionNodes = lifecycleKey
    ? messagesState.timelineNodes.filter((node) => (
        node.nodeId !== stableNodeId
        && node.kind !== 'worker_lifecycle'
        && resolveProjectionTaskKey(node.message) === lifecycleKey
        && isWorkerLifecycleAttachmentMessage(node.message)
      ))
    : [];
  const absorbedExecutionItems = absorbedExecutionNodes
    .map((node) => extractExecutionItemFromNode(node))
    .filter((item): item is TimelineExecutionItem => Boolean(item));
  const executionItems = (() => {
    if (usesFragmentExecutionItems) {
      return buildFragmentExecutionItems(fragmentMessages, visibility);
    }
    const merged = [...(existingNode?.executionItems || [])];
    for (const item of absorbedExecutionItems) {
      const existingIndex = merged.findIndex((current) => (
        current.itemId === item.itemId
        || current.messageIds.some((messageId) => item.messageIds.includes(messageId))
      ));
      if (existingIndex >= 0) {
        merged[existingIndex] = normalizeTimelineExecutionItem({
          ...merged[existingIndex],
          latestEventSeq: Math.max(merged[existingIndex].latestEventSeq, item.latestEventSeq),
          cardStreamSeq: Math.max(merged[existingIndex].cardStreamSeq, item.cardStreamSeq),
          timestamp: Math.min(merged[existingIndex].timestamp, item.timestamp),
          worker: merged[existingIndex].worker || item.worker,
          threadVisible: merged[existingIndex].threadVisible || item.threadVisible,
          workerTabs: [
            ...merged[existingIndex].workerTabs,
            ...item.workerTabs,
          ],
          messageIds: Array.from(new Set([
            ...merged[existingIndex].messageIds,
            ...item.messageIds,
          ])),
          message: item.message,
        });
      } else {
        merged.push(item);
      }
    }
    return merged;
  })();
  const nextNode: TimelineNode = normalizeTimelineNode({
    nodeId: stableNodeId,
    kind: existingNode?.kind || resolveTimelineNodeKind(mergedMessage),
    displayOrder: existingNode?.displayOrder ?? options.displayOrder,
    laneOrder: existingNode?.laneOrder,
    artifactVersion: existingNode?.artifactVersion,
    anchorEventSeq: existingNode?.anchorEventSeq || nextAnchorEventSeq,
    latestEventSeq: Math.max(existingNode?.latestEventSeq || 0, nextLatestEventSeq),
    cardStreamSeq: Math.max(existingNode?.cardStreamSeq || 0, nextCardStreamSeq),
    timestamp: mergeTimelineSortTimestamp(existingNode?.timestamp, mergedMessage),
    cardId: cardId || existingNode?.cardId,
    lifecycleKey: lifecycleKey || existingNode?.lifecycleKey,
    worker: resolveTimelineWorker(mergedMessage) || existingNode?.worker,
    visibleInThread: existingNode?.visibleInThread || visibility.thread !== false,
    workerTabs: [
      ...(existingNode?.workerTabs || []),
      ...nextWorkerTabs,
    ],
    messageIds: Array.from(new Set([
      ...(existingNode?.messageIds || []),
      stableNodeId,
      normalizedMessage.id,
      ...absorbedExecutionNodes.flatMap((node) => node.messageIds),
      ...(options.replaceMessageId ? [options.replaceMessageId] : []),
    ])),
    message: mergedMessage,
    executionItems,
  });

  mutateTimelineNodes((nodes) => {
    const filteredNodes = lifecycleKey
      ? nodes.filter((node) => !absorbedExecutionNodes.some((absorbed) => absorbed.nodeId === node.nodeId))
      : nodes;
    const index = filteredNodes.findIndex((node) => node.nodeId === stableNodeId);
    if (index >= 0) {
      const next = [...filteredNodes];
      next[index] = nextNode;
      return next;
    }
    return [...filteredNodes, nextNode];
  });

  return nextNode.message;
}
function findTimelineNodeByAlias(messageId: string): TimelineNode | undefined {
  const stableId = resolveTimelineAliasId(messageId);
  return messagesState.timelineNodes.find((node) => node.nodeId === stableId);
}

interface TimelineMessageTarget {
  node: TimelineNode;
  executionItemIndex: number | null;
  executionItem: TimelineExecutionItem | null;
  flushKey: string;
}

function findTimelineMessageTargetByAlias(messageId: string): TimelineMessageTarget | undefined {
  const normalizedId = typeof messageId === 'string' ? messageId.trim() : '';
  if (!normalizedId) {
    return undefined;
  }
  const executionTarget = timelineExecutionItemTargetByMessageId.get(normalizedId);
  if (executionTarget) {
    const node = messagesState.timelineNodes.find((item) => item.nodeId === executionTarget.nodeId);
    if (!node) {
      return undefined;
    }
    const executionItems = ensureArray<TimelineExecutionItem>(node.executionItems);
    const executionItemIndex = executionItems.findIndex((item) => item.itemId === executionTarget.itemId);
    if (executionItemIndex < 0) {
      return undefined;
    }
    return {
      node,
      executionItemIndex,
      executionItem: executionItems[executionItemIndex],
      flushKey: `item:${node.nodeId}:${executionTarget.itemId}`,
    };
  }

  const node = findTimelineNodeByAlias(normalizedId);
  if (!node) {
    return undefined;
  }
  return {
    node,
    executionItemIndex: null,
    executionItem: null,
    flushKey: `node:${node.nodeId}`,
  };
}

function updateTimelineNodeByAlias(messageId: string, updates: Partial<Message>): Message | undefined {
  const target = findTimelineMessageTargetByAlias(messageId);
  if (!target) {
    return undefined;
  }
  const stableId = target.node.nodeId;
  const currentNode = target.node;

  if (target.executionItem && target.executionItemIndex !== null) {
    if (compareIncomingMessageVersion(target.executionItem, updates as Pick<Message, 'metadata'>) < 0) {
      return target.executionItem.message;
    }
    const nextMessage = normalizeIncomingMessage({
      ...target.executionItem.message,
      ...updates,
      id: target.executionItem.itemId,
    });
    const executionVisibility = resolveWorkerVisibility(nextMessage);
    const nextExecutionItem = normalizeTimelineExecutionItem({
      ...target.executionItem,
      latestEventSeq: Math.max(
        target.executionItem.latestEventSeq,
        getMessageEventSeq(nextMessage) ?? target.executionItem.latestEventSeq,
      ),
      cardStreamSeq: Math.max(
        target.executionItem.cardStreamSeq,
        getMessageCardStreamSeq(nextMessage) || target.executionItem.cardStreamSeq,
      ),
      worker: resolveTimelineWorker(nextMessage) || target.executionItem.worker,
      threadVisible: target.executionItem.threadVisible || executionVisibility.threadVisible,
      workerTabs: [
        ...target.executionItem.workerTabs,
        ...executionVisibility.workerTabs,
      ],
      messageIds: Array.from(new Set([
        ...target.executionItem.messageIds,
        messageId,
        nextMessage.id,
      ])),
      timestamp: mergeTimelineSortTimestamp(target.executionItem.timestamp, nextMessage),
      message: nextMessage,
    });
    const nextExecutionItems = [...ensureArray<TimelineExecutionItem>(currentNode.executionItems)];
    nextExecutionItems[target.executionItemIndex] = nextExecutionItem;
    const nextNode = normalizeTimelineNode({
      ...currentNode,
      latestEventSeq: Math.max(currentNode.latestEventSeq, nextExecutionItem.latestEventSeq),
      cardStreamSeq: Math.max(currentNode.cardStreamSeq, nextExecutionItem.cardStreamSeq),
      workerTabs: [
        ...currentNode.workerTabs,
        ...nextExecutionItem.workerTabs,
      ],
      executionItems: nextExecutionItems,
    });
    mutateTimelineNodes((nodes) => nodes.map((node) => (
      node.nodeId === stableId ? nextNode : node
    )));
    return nextExecutionItem.message;
  }

  if (compareIncomingMessageVersion(currentNode, updates as Pick<Message, 'metadata'>) < 0) {
    return currentNode.message;
  }
  const nextMessage = normalizeIncomingMessage({
    ...currentNode.message,
    ...updates,
    id: stableId,
  });
  const fragmentMessages = currentNode.kind === 'worker_lifecycle'
    ? []
    : resolveTimelineFragmentMessages(nextMessage);
  const usesFragmentExecutionItems = fragmentMessages.length > 1;
  const nextVisibleMessage = usesFragmentExecutionItems
    ? setTimelineContainerFlag(nextMessage, true)
    : setTimelineContainerFlag(nextMessage, false);
  const nextNode = normalizeTimelineNode({
    ...currentNode,
    latestEventSeq: Math.max(
      currentNode.latestEventSeq,
      getMessageEventSeq(nextMessage) ?? currentNode.latestEventSeq,
    ),
    cardStreamSeq: Math.max(
      currentNode.cardStreamSeq,
      getMessageCardStreamSeq(nextMessage) || currentNode.cardStreamSeq,
    ),
    worker: resolveTimelineWorker(nextMessage) || currentNode.worker,
    cardId: resolveTimelineCardId(nextMessage) || currentNode.cardId,
    lifecycleKey: resolveTimelineLifecycleKey(nextMessage) || currentNode.lifecycleKey,
    workerTabs: currentNode.workerTabs,
    messageIds: currentNode.messageIds,
    timestamp: mergeTimelineSortTimestamp(currentNode.timestamp, nextMessage),
    message: nextVisibleMessage,
    executionItems: usesFragmentExecutionItems
      ? buildFragmentExecutionItems(fragmentMessages, {
          thread: currentNode.visibleInThread,
          workerTabs: currentNode.workerTabs,
        })
      : (isTimelineContainerOnlyMessage(currentNode.message) ? [] : currentNode.executionItems),
  });
  mutateTimelineNodes((nodes) => nodes.map((node) => (
    node.nodeId === stableId ? nextNode : node
  )));
  return nextNode.message;
}

function normalizeMissionPlan(plan: MissionPlan | null): MissionPlan | null {
  if (!plan || typeof plan !== 'object') return null;
  const assignmentSeen = new Set<string>();
  const assignments = ensureArray(plan.assignments)
    .filter((assignment: any) => assignment && typeof assignment === 'object')
    .map((assignment: any) => {
      const assignmentId = typeof assignment.id === 'string' && assignment.id.trim() ? assignment.id.trim() : '';
      if (!assignmentId) {
        throw new Error('[MessagesStore] MissionPlan assignment 缺少 id');
      }
      if (assignmentSeen.has(assignmentId)) {
        throw new Error(`[MessagesStore] MissionPlan assignment id 重复: ${assignmentId}`);
      }
      assignmentSeen.add(assignmentId);
      const todoSeen = new Set<string>();
      const todos = ensureArray(assignment.todos)
        .filter((todo: any) => todo && typeof todo === 'object')
        .map((todo: any) => {
          const todoId = typeof todo.id === 'string' && todo.id.trim() ? todo.id.trim() : '';
          if (!todoId) {
            throw new Error('[MessagesStore] MissionPlan todo 缺少 id');
          }
          if (todoSeen.has(todoId)) {
            throw new Error(`[MessagesStore] MissionPlan todo id 重复: ${todoId}`);
          }
          todoSeen.add(todoId);
          return { ...todo, id: todoId, assignmentId };
        });
      return { ...assignment, id: assignmentId, todos };
    });
  return { ...plan, missionId: plan.missionId || '', assignments };
}

function normalizeOrchestratorRuntimeState(
  input: OrchestratorRuntimeState | null | undefined,
): OrchestratorRuntimeState | null {
  if (!input || typeof input !== 'object') return null;
  const status = input.status === 'idle'
    || input.status === 'running'
    || input.status === 'waiting'
    || input.status === 'paused'
    || input.status === 'completed'
    || input.status === 'failed'
    || input.status === 'cancelled'
    ? input.status
    : null;
  const phase = typeof input.phase === 'string' && input.phase.trim().length > 0
    ? input.phase.trim()
    : '';
  const statusChangedAt = typeof input.statusChangedAt === 'number' && Number.isFinite(input.statusChangedAt) && input.statusChangedAt > 0
    ? Math.floor(input.statusChangedAt)
    : null;
  const lastEventAt = typeof input.lastEventAt === 'number' && Number.isFinite(input.lastEventAt) && input.lastEventAt > 0
    ? Math.floor(input.lastEventAt)
    : null;
  if (!status || !phase || statusChangedAt === null || lastEventAt === null) {
    return null;
  }
  const sessionId = typeof input.sessionId === 'string' && input.sessionId.trim().length > 0
    ? input.sessionId.trim()
    : undefined;
  const requestId = typeof input.requestId === 'string' && input.requestId.trim().length > 0
    ? input.requestId.trim()
    : undefined;
  const statusReason = typeof input.statusReason === 'string' && input.statusReason.trim().length > 0
    ? input.statusReason.trim()
    : undefined;
  const canResume = input.canResume === true ? true : undefined;
  const runtimeReason = typeof input.runtimeReason === 'string' && input.runtimeReason.trim().length > 0
    ? input.runtimeReason.trim()
    : undefined;
  const failureReason = typeof input.failureReason === 'string' && input.failureReason.trim().length > 0
    ? input.failureReason.trim()
    : undefined;
  const errors = Array.isArray(input.errors)
    ? input.errors
      .filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
      .map((item) => item.trim())
    : [];
  const runtimeSnapshot = input.runtimeSnapshot && typeof input.runtimeSnapshot === 'object'
    ? JSON.parse(JSON.stringify(input.runtimeSnapshot))
    : null;
  const runtimeDecisionTrace = Array.isArray(input.runtimeDecisionTrace)
    ? input.runtimeDecisionTrace
      .filter((entry) => entry && typeof entry === 'object')
      .map((entry) => JSON.parse(JSON.stringify(entry)))
    : [];
  const assignments = Array.isArray(input.assignments)
    ? input.assignments
      .filter((entry) => entry && typeof entry === 'object')
      .map((entry) => JSON.parse(JSON.stringify(entry)))
    : [];
  const chain = input.chain && typeof input.chain === 'object'
    ? JSON.parse(JSON.stringify(input.chain))
    : undefined;
  const startedAt = typeof input.startedAt === 'number' && Number.isFinite(input.startedAt) && input.startedAt > 0
    ? Math.floor(input.startedAt)
    : undefined;
  const endedAt = typeof input.endedAt === 'number' && Number.isFinite(input.endedAt) && input.endedAt > 0
    ? Math.floor(input.endedAt)
    : undefined;
  const opsView = input.opsView && typeof input.opsView === 'object'
    ? JSON.parse(JSON.stringify(input.opsView))
    : null;
  return {
    status,
    phase,
    errors,
    statusChangedAt,
    lastEventAt,
    assignments,
    ...(sessionId ? { sessionId } : {}),
    ...(requestId ? { requestId } : {}),
    ...(chain ? { chain } : {}),
    ...(statusReason ? { statusReason } : {}),
    ...(canResume ? { canResume } : {}),
    ...(runtimeReason ? { runtimeReason } : {}),
    ...(failureReason ? { failureReason } : {}),
    ...(startedAt ? { startedAt } : {}),
    ...(endedAt ? { endedAt } : {}),
    runtimeSnapshot,
    runtimeDecisionTrace,
    ...(opsView ? { opsView } : {}),
  };
}

function normalizeProcessingStateSnapshot(
  input: AppState['processingState'],
): AppState['processingState'] {
  if (!input || typeof input !== 'object') {
    return null;
  }
  const pendingRequestIds = Array.isArray(input.pendingRequestIds)
    ? Array.from(new Set(
        input.pendingRequestIds
          .filter((item): item is string => typeof item === 'string' && item.trim().length > 0)
          .map((item) => item.trim()),
      ))
    : [];
  const source = typeof input.source === 'string' && input.source.trim().length > 0
    ? (() => {
        const normalized = input.source.trim();
        return normalized === 'orchestrator' || normalized === 'worker'
          ? normalized as NonNullable<AppState['processingState']>['source']
          : null;
      })()
    : null;
  const agent = typeof input.agent === 'string' && input.agent.trim().length > 0
    ? input.agent.trim()
    : null;
  const startedAt = typeof input.startedAt === 'number' && Number.isFinite(input.startedAt) && input.startedAt > 0
    ? Math.floor(input.startedAt)
    : null;
  return {
    isProcessing: input.isProcessing === true,
    source,
    agent,
    startedAt,
    pendingRequestIds,
  };
}

function resolveOrchestratorRuntimeStateVersion(
  snapshot: OrchestratorRuntimeState,
): number {
  return Math.max(
    snapshot.lastEventAt,
    snapshot.statusChangedAt,
    snapshot.startedAt ?? 0,
    snapshot.endedAt ?? 0,
  );
}

function shouldReplaceOrchestratorRuntimeState(
  next: OrchestratorRuntimeState | null,
): boolean {
  if (!next) {
    return true;
  }
  const current = messagesState.orchestratorRuntimeState;
  if (!current) {
    return true;
  }
  const nextVersion = resolveOrchestratorRuntimeStateVersion(next);
  const currentVersion = resolveOrchestratorRuntimeStateVersion(current);
  if (nextVersion !== currentVersion) {
    return nextVersion > currentVersion;
  }
  if (next.statusChangedAt !== current.statusChangedAt) {
    return next.statusChangedAt > current.statusChangedAt;
  }
  return true;
}

export function applyAuthoritativeProcessingState(input: AppState['processingState']): void {
  const snapshot = normalizeProcessingStateSnapshot(input);
  if (!snapshot) {
    return;
  }
  // 防回抬保护：如果在 forced idle 冷却期内，拒绝后端权威状态覆盖
  const lastForcedIdleAt = messagesState.lastForcedIdleAt;
  if (lastForcedIdleAt !== null && (Date.now() - lastForcedIdleAt) < ANTI_LIFT_BACK_COOLDOWN_MS) {
    // 冷却期内只同步 actor，不改变 processing 状态
    if (snapshot.source) {
      setProcessingActor(snapshot.source, snapshot.agent || undefined);
    }
    return;
  }
  const pendingRequestIds = new Set(snapshot.pendingRequestIds);
  const nextIsProcessing = snapshot.isProcessing
    || messagesState.activeMessageIds.size > 0
    || pendingRequestIds.size > 0;

  messagesState.backendProcessing = snapshot.isProcessing;
  messagesState.pendingRequests = pendingRequestIds;
  if (snapshot.source) {
    setProcessingActor(snapshot.source, snapshot.agent || undefined);
  }
  if (nextIsProcessing) {
    messagesState.thinkingStartAt = snapshot.startedAt
      || messagesState.thinkingStartAt
      || Date.now();
  } else {
    messagesState.thinkingStartAt = null;
  }
  messagesState.isProcessing = nextIsProcessing;
}

// 交互请求状态
let pendingRecovery = $state<{ taskId: string; error: unknown; canRetry: boolean; canRollback: boolean } | null>(null);
let pendingClarification = $state<{ questions: string[]; context?: string; ambiguityScore?: number; originalPrompt?: string } | null>(null);
let pendingWorkerQuestion = $state<{ workerId: string; question: string; context?: string; options?: unknown } | null>(null);

// 执行链中断状态（可恢复）
let interruptedChain = $state<{ chainId: string; recoverable: boolean } | null>(null);

let missionPlan = $state<Map<string, MissionPlan>>(new Map());

// Wave 执行状态（提案 4.6）
let waveState = $state<WaveState | null>(null);

// 请求-响应绑定状态（消息响应流设计）
let requestBindings = $state<Map<string, RequestResponseBinding>>(new Map());

// LLM 重试运行态（非持久化，仅用于当前活跃消息展示）
export const retryRuntimeState = $state({
  byMessageId: new Map<string, RetryRuntimeState>(),
});

// 请求超时时间（30秒）

// ============ 直接导出响应式状态（Svelte 5 推荐方式）============
// 🔧 修复响应式追踪问题：通过 messagesState 对象属性访问
// Svelte 5 官方推荐：导出对象属性读取，确保响应式追踪正常

export function getThreadMessages() {
  return messageProjection.thread;
}

export function getAgentOutputs() {
  return {
    claude: messageProjection.workers.claude,
    codex: messageProjection.workers.codex,
    gemini: messageProjection.workers.gemini,
  } as AgentOutputs;
}

export function getCurrentBottomTab() {
  return messagesState.currentBottomTab;
}

export function getCurrentTopTab() {
  return messagesState.currentTopTab;
}

export function getIsProcessing() {
  return messagesState.isProcessing;
}

export function getThinkingStartAt() {
  return messagesState.thinkingStartAt;
}

export function getProcessingActor() {
  return messagesState.processingActor;
}

export function getSessions() {
  return messagesState.sessions;
}

export function getCurrentSessionId() {
  return messagesState.currentSessionId;
}

export function getQueuedMessages() {
  return messagesState.queuedMessages;
}

export function getAppState() {
  return messagesState.appState;
}

export function getScrollPositions() {
  return messagesState.scrollPositions;
}

export function getScrollAnchors() {
  return messagesState.scrollAnchors;
}

export function getAutoScrollEnabled() {
  return messagesState.autoScrollEnabled;
}

export function getTasks() {
  return tasks;
}

export function getEdits() {
  return edits;
}

export function getWorkerWaitResults() {
  return workerWaitResults;
}

export function getOrchestratorRuntimeState() {
  return messagesState.orchestratorRuntimeState;
}

export function getToasts() {
  return toasts;
}

export function getModelStatus() {
  return modelStatus;
}

export function getPendingRecovery() {
  return pendingRecovery;
}

export function getInterruptedChain() {
  return interruptedChain;
}

export function setInterruptedChain(value: { chainId: string; recoverable: boolean } | null) {
  interruptedChain = value;
}

export function getPendingClarification() {
  return pendingClarification;
}

export function getPendingWorkerQuestion() {
  return pendingWorkerQuestion;
}

export function getMissionPlan(): Map<string, MissionPlan> {
  return missionPlan;
}

export function getWaveState() {
  return waveState;
}

function mergeWorkerWaitResultPayload(
  currentPayload: WaitForWorkersResult | null | undefined,
  incomingPayload: WaitForWorkersResult,
): WaitForWorkersResult {
  const mergedResultsByTaskId = new Map<string, WaitForWorkersResult['results'][number]>();
  for (const result of currentPayload?.results || []) {
    if (typeof result?.task_id === 'string' && result.task_id.trim()) {
      mergedResultsByTaskId.set(result.task_id.trim(), result);
    }
  }
  for (const result of incomingPayload.results || []) {
    if (typeof result?.task_id === 'string' && result.task_id.trim()) {
      mergedResultsByTaskId.set(result.task_id.trim(), result);
    }
  }

  return {
    ...(currentPayload || {}),
    ...incomingPayload,
    results: Array.from(mergedResultsByTaskId.values()),
    pending_task_ids: Array.isArray(incomingPayload.pending_task_ids)
      ? incomingPayload.pending_task_ids
      : (currentPayload?.pending_task_ids || []),
    audit: incomingPayload.audit ?? currentPayload?.audit,
  };
}

export function updateWorkerWaitResults(next: Partial<Record<string, WaitForWorkersResult | null>>) {
  const current = workerWaitResults;
  const merged: Record<string, WaitForWorkersResult | null> = { ...current };
  for (const [cardKey, payload] of Object.entries(next)) {
    const key = cardKey;
    if (!payload) {
      merged[key] = null;
      continue;
    }
    const incomingAt = typeof payload.updatedAt === 'number' ? payload.updatedAt : 0;
    const currentPayload = current[key];
    const currentAt = typeof currentPayload?.updatedAt === 'number' ? (currentPayload?.updatedAt as number) : 0;
    const shouldPreserveCompletedAt = Boolean(
      currentPayload
        && currentPayload.wait_status === 'completed'
        && !currentPayload.timed_out
        && currentAt > 0
        && payload.wait_status === 'completed'
        && !payload.timed_out
        && incomingAt >= currentAt
    );
    if (incomingAt >= currentAt) {
      const mergedPayload = mergeWorkerWaitResultPayload(currentPayload, payload);
      merged[key] = shouldPreserveCompletedAt
        ? { ...mergedPayload, updatedAt: currentAt }
        : mergedPayload;
    }
  }
  workerWaitResults = merged;
}

// ============ getState() 仅用于现有调用方（Svelte 5 迁移中）============
// ⚠️ 注意：此函数返回的对象无法被 Svelte 5 正确追踪
// 建议使用上面的独立 getter 函数或直接使用 messagesState

export function getState() {
  return {
    get currentTopTab() { return messagesState.currentTopTab; },
    get currentBottomTab() { return messagesState.currentBottomTab; },
    get messageJump() { return messagesState.messageJump; },
    get timelineNodes() { return messagesState.timelineNodes; },
    get timelineProjection() { return messagesState.timelineProjection; },
    get threadMessages() { return messageProjection.thread; },
    get agentOutputs() {
      return {
        claude: messageProjection.workers.claude,
        codex: messageProjection.workers.codex,
        gemini: messageProjection.workers.gemini,
      } as AgentOutputs;
    },
    get sessions() { return messagesState.sessions; },
    get currentSessionId() { return messagesState.currentSessionId; },
    get queuedMessages() { return messagesState.queuedMessages; },
    set queuedMessages(v) { messagesState.queuedMessages = ensureArray<QueuedMessage>(v) as QueuedMessage[]; },
    get isProcessing() { return messagesState.isProcessing; },
    get thinkingStartAt() { return messagesState.thinkingStartAt; },
    get processingActor() { return messagesState.processingActor; },
    get appState() { return messagesState.appState; },
    get scrollPositions() { return messagesState.scrollPositions; },
    get autoScrollEnabled() { return messagesState.autoScrollEnabled; },
    // 新增
    get tasks() { return tasks; },
    set tasks(v) { tasks = v; },
    get edits() { return edits; },
    set edits(v) { edits = v; },
    get workerWaitResults() { return workerWaitResults; },
    set workerWaitResults(v) { workerWaitResults = v; },
    get orchestratorRuntimeState() { return messagesState.orchestratorRuntimeState; },
    set orchestratorRuntimeState(v) { setOrchestratorRuntimeState(v); },
    get toasts() { return toasts; },
    set toasts(v) { toasts = v; },
    get notifications() { return notifications; },
    get unreadNotificationCount() { return unreadNotificationCount; },
    get modelStatus() { return modelStatus; },
    set modelStatus(v) { modelStatus = v; },
    get pendingRecovery() { return pendingRecovery; },
    set pendingRecovery(v) { pendingRecovery = v; },
    get pendingClarification() { return pendingClarification; },
    set pendingClarification(v) { pendingClarification = v; },
    get pendingWorkerQuestion() { return pendingWorkerQuestion; },
    set pendingWorkerQuestion(v) { pendingWorkerQuestion = v; },
    get missionPlan() { return missionPlan; },
    set missionPlan(v) { missionPlan = v; },
    // Wave 状态（提案 4.6）
    get waveState() { return waveState; },
    set waveState(v) { waveState = v; },
    // Worker 运行态（统一入口）
    get workerRuntime() { return workerRuntime; },
    getThreadRenderItems() {
      return buildTimelineRenderItems(timelineNodeLookup, messagesState.timelineProjection, 'thread');
    },
    getWorkerRenderItems(worker: AgentType) {
      return buildTimelineRenderItems(timelineNodeLookup, messagesState.timelineProjection, 'worker', worker);
    },
  };
}

// ============ 状态更新函数 ============

function trimTimelineNodes() {
  if (messagesState.timelineNodes.length <= MAX_TIMELINE_NODES) {
    return;
  }
  replaceTimelineNodes(messagesState.timelineNodes.slice(-MAX_TIMELINE_NODES));
  syncTimelineProjectionFromNodes(messagesState.currentSessionId, { persist: false });
}

function createSessionViewStateSnapshot(sessionId: string | null | undefined): PersistedSessionViewState | null {
  if (!IS_HOSTED_WEBVIEW) {
    return null;
  }
  const normalizedSessionId = normalizeSessionId(sessionId);
  const projection = messagesState.timelineProjection;
  if (!normalizedSessionId || !projection || projection.sessionId !== normalizedSessionId) {
    return null;
  }
  const clonedProjection = normalizePersistedTimelineProjection(
    clonePersistablePayload(projection),
    normalizedSessionId,
  );
  if (!clonedProjection) {
    return null;
  }
  return {
    sessionId: normalizedSessionId,
    timelineProjection: clonedProjection,
    scrollPositions: normalizePersistedScrollPositions(clonePersistablePayload(messagesState.scrollPositions)),
    scrollAnchors: normalizePersistedScrollAnchors(clonePersistablePayload(messagesState.scrollAnchors)),
    autoScrollEnabled: normalizePersistedAutoScrollConfig(clonePersistablePayload(messagesState.autoScrollEnabled)),
  };
}

function upsertSessionViewStateSnapshot(snapshot: PersistedSessionViewState | null): void {
  if (!snapshot) {
    return;
  }
  sessionViewStateBySession = {
    ...sessionViewStateBySession,
    [snapshot.sessionId]: snapshot,
  };
}

function captureCurrentSessionViewState(): void {
  if (!IS_HOSTED_WEBVIEW) {
    return;
  }
  ensureTimelineProjectionSnapshotCurrent(messagesState.currentSessionId);
  upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(messagesState.currentSessionId));
}

function getSessionViewState(sessionId: string | null | undefined): PersistedSessionViewState | null {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    return null;
  }
  return sessionViewStateBySession[normalizedSessionId] || null;
}

function pruneSessionViewStateByKnownSessions(): void {
  const knownSessionIds = new Set<string>();
  for (const session of messagesState.sessions) {
    const sessionId = normalizeSessionId(session?.id);
    if (sessionId) {
      knownSessionIds.add(sessionId);
    }
  }
  const currentSessionId = normalizeSessionId(messagesState.currentSessionId);
  if (currentSessionId) {
    knownSessionIds.add(currentSessionId);
  }
  if (knownSessionIds.size === 0) {
    return;
  }
  const nextEntries = Object.entries(sessionViewStateBySession)
    .filter(([sessionId]) => knownSessionIds.has(sessionId));
  if (nextEntries.length === Object.keys(sessionViewStateBySession).length) {
    return;
  }
  sessionViewStateBySession = Object.fromEntries(nextEntries);
}

function applySessionViewState(sessionId: string | null | undefined): boolean {
  const snapshot = getSessionViewState(sessionId);
  if (!snapshot) {
    return false;
  }
  const normalizedSessionId = normalizeSessionId(sessionId);
  const normalizedSnapshot = normalizedSessionId
    ? normalizePersistedSessionViewState(normalizedSessionId, clonePersistablePayload(snapshot))
    : null;
  if (!normalizedSnapshot) {
    return false;
  }
  const canonicalProjection = canonicalizeTimelineProjection(normalizedSnapshot.timelineProjection);
  messagesState.timelineProjection = canonicalProjection;
  setTimelineProjectionNodes(buildTimelineNodesFromProjection(canonicalProjection));
  timelineProjectionDirty = false;
  messagesState.scrollPositions = normalizePersistedScrollPositions(normalizedSnapshot.scrollPositions);
  messagesState.scrollAnchors = normalizePersistedScrollAnchors(normalizedSnapshot.scrollAnchors);
  messagesState.autoScrollEnabled = normalizePersistedAutoScrollConfig(normalizedSnapshot.autoScrollEnabled);
  workerWaitResults = {};
  return true;
}

// 保存状态到 VS Code
function saveWebviewState() {
  if (webviewStateBatchDepth > 0) {
    webviewStateBatchPending = true;
    return;
  }
  if (deferredWebviewStateSaveTimer) {
    clearTimeout(deferredWebviewStateSaveTimer);
    deferredWebviewStateSaveTimer = null;
  }
  try {
    if (IS_HOSTED_WEBVIEW) {
      trimTimelineNodes();
      ensureTimelineProjectionSnapshotCurrent(messagesState.currentSessionId);
      captureCurrentSessionViewState();
      pruneSessionViewStateByKnownSessions();
    }
    const state: WebviewPersistedState = {
      currentTopTab: messagesState.currentTopTab,
      currentBottomTab: messagesState.currentBottomTab,
      sessions: messagesState.sessions,
      currentSessionId: messagesState.currentSessionId,
      scrollPositions: messagesState.scrollPositions,
      scrollAnchors: messagesState.scrollAnchors,
      autoScrollEnabled: messagesState.autoScrollEnabled,
      ...(IS_HOSTED_WEBVIEW
        ? {
            currentTimelineProjection: messagesState.timelineProjection,
            sessionViewStateBySession,
          }
        : {}),
    };
    vscode.setState(state);
  } catch (error) {
    console.warn('[MessagesStore] Webview 状态持久化失败，已降级继续运行', error);
  }
}

export function batchWebviewStatePersistence(mutator: () => void): void {
  webviewStateBatchDepth += 1;
  try {
    mutator();
  } finally {
    webviewStateBatchDepth = Math.max(0, webviewStateBatchDepth - 1);
    if (webviewStateBatchDepth === 0 && webviewStateBatchPending) {
      webviewStateBatchPending = false;
      saveWebviewState();
    }
  }
}

export function setOrchestratorRuntimeState(input: OrchestratorRuntimeState | null): void {
  const next = normalizeOrchestratorRuntimeState(input);
  if (!shouldReplaceOrchestratorRuntimeState(next)) {
    return;
  }
  messagesState.orchestratorRuntimeState = next;
}

export function updatePanelScrollState(
  panel: ScrollPanelKey,
  input: { scrollTop?: number; autoScrollEnabled?: boolean; anchor?: ScrollAnchor | null },
  options: { persist?: boolean } = {}
): void {
  let changed = false;

  if (typeof input.scrollTop === 'number') {
    const nextScrollTop = normalizeScrollTop(input.scrollTop);
    if (messagesState.scrollPositions[panel] !== nextScrollTop) {
      messagesState.scrollPositions = {
        ...messagesState.scrollPositions,
        [panel]: nextScrollTop,
      };
      changed = true;
    }
  }

  if (typeof input.autoScrollEnabled === 'boolean' && messagesState.autoScrollEnabled[panel] !== input.autoScrollEnabled) {
    messagesState.autoScrollEnabled = {
      ...messagesState.autoScrollEnabled,
      [panel]: input.autoScrollEnabled,
    };
    changed = true;
  }

  if ('anchor' in input) {
    const nextAnchor = normalizeScrollAnchor(input.anchor);
    const currentAnchor = messagesState.scrollAnchors[panel];
    if (currentAnchor.messageId !== nextAnchor.messageId || currentAnchor.offsetTop !== nextAnchor.offsetTop) {
      messagesState.scrollAnchors = {
        ...messagesState.scrollAnchors,
        [panel]: nextAnchor,
      };
      changed = true;
    }
  }

  if (changed && options.persist !== false) {
    scheduleSaveWebviewState();
  }
}

// Tab 操作
export function setCurrentTopTab(tab: TabType) {
  messagesState.currentTopTab = tab;
  saveWebviewState();
}

export function setCurrentBottomTab(tab: TabType) {
  messagesState.currentBottomTab = tab;
  saveWebviewState();
}

export function requestMessageJump(messageId: string): void {
  const normalized = typeof messageId === 'string' ? messageId.trim() : '';
  if (!normalized) return;
  messagesState.messageJump = {
    messageId: normalized,
    nonce: messagesState.messageJump.nonce + 1,
  };
}

export function clearMessageJump(): void {
  if (!messagesState.messageJump.messageId) return;
  messagesState.messageJump = {
    messageId: null,
    nonce: messagesState.messageJump.nonce,
  };
}

// 会话操作
export function setCurrentSessionId(id: string | null) {
  const nextSessionId = normalizeSessionId(id);
  const previousSessionId = normalizeSessionId(messagesState.currentSessionId);
  const hasChanged = previousSessionId !== nextSessionId;
  let restoredSessionView = false;
  if (hasChanged) {
    captureCurrentSessionViewState();
  }
  messagesState.currentSessionId = nextSessionId;
  if (hasChanged) {
    // 底部 worker 面板是“当前会话内的执行细节视图”，不能跨会话继承。
    // 否则用户从上一会话停留在 worker tab，新会话会直接落到 worker 面板，
    // 造成“主线/worker 边界混淆”的产品错觉。
    messagesState.currentBottomTab = 'thread';
    restoredSessionView = applySessionViewState(nextSessionId);
    if (!restoredSessionView) {
      replaceTimelineNodes([]);
      messagesState.timelineProjection = null;
      timelineProjectionDirty = false;
      resetPanelScrollRuntimeState();
    }
  }
  syncNotificationsFromSession(nextSessionId);
  saveWebviewState();
}

export function updateSessions(newSessions: Session[]) {
  const seen = new Set<string>();
  messagesState.sessions = ensureArray<Session>(newSessions)
    .filter((session): session is Session => !!session && typeof session === 'object' && typeof session.id === 'string' && session.id.trim().length > 0)
    .filter((session) => {
      if (seen.has(session.id)) return false;
      seen.add(session.id);
      return true;
    });
  pruneSessionViewStateByKnownSessions();
  saveWebviewState();
}

export function setQueuedMessages(newQueuedMessages: QueuedMessage[]) {
  const seen = new Set<string>();
  messagesState.queuedMessages = ensureArray<QueuedMessage>(newQueuedMessages)
    .filter((item): item is QueuedMessage => (
      !!item
      && typeof item === 'object'
      && typeof item.id === 'string'
      && item.id.trim().length > 0
      && typeof item.content === 'string'
      && typeof item.createdAt === 'number'
      && Number.isFinite(item.createdAt)
    ))
    .filter((item) => {
      if (seen.has(item.id)) return false;
      seen.add(item.id);
      return true;
    })
    .map((item) => ({
      id: item.id,
      content: item.content,
      createdAt: item.createdAt,
    }));
}

// 处理状态操作
export function setIsProcessing(value: boolean) {
  messagesState.backendProcessing = value;
  updateProcessingState();
}

export function setThinkingStartAt(value: number | null) {
  messagesState.thinkingStartAt = value;
}

export function setProcessingActor(source: string, agent?: string) {
  messagesState.processingActor = {
    source: source as ProcessingActor['source'],
    agent: (agent || 'claude') as ProcessingActor['agent'],
  };
}

export function setAppState(nextState: AppState | null) {
  messagesState.appState = nextState;
}

export function setMissionPlan(plan: MissionPlan | null) {
  const normalized = normalizeMissionPlan(plan);
  if (!normalized) return;
  const next = new Map(missionPlan);
  next.set(normalized.missionId, normalized);
  missionPlan = next;
}

// 防回抬冷却期（ms）：forced idle 后的短暂窗口内，拒绝任何来源的 processing=true
const ANTI_LIFT_BACK_COOLDOWN_MS = 2000;

// Worker 执行状态操作
function updateProcessingState() {
  const nextIsProcessing = messagesState.backendProcessing
    || messagesState.activeMessageIds.size > 0
    || messagesState.pendingRequests.size > 0;

  // 防回抬保护：forced idle 冷却期内，拒绝从 false 被抬回 true
  if (nextIsProcessing && !messagesState.isProcessing) {
    const lastForcedIdleAt = messagesState.lastForcedIdleAt;
    if (lastForcedIdleAt !== null && (Date.now() - lastForcedIdleAt) < ANTI_LIFT_BACK_COOLDOWN_MS) {
      // 冷却期内，拒绝抬回 — 保持 idle
      return;
    }
    messagesState.thinkingStartAt = Date.now();
  } else if (!nextIsProcessing && messagesState.isProcessing) {
    messagesState.thinkingStartAt = null;
  }

  messagesState.isProcessing = nextIsProcessing;
}

export function markMessageActive(id: string) {
  if (!id) return;
  if (!messagesState.activeMessageIds.has(id)) {
    const next = new Set(messagesState.activeMessageIds);
    next.add(id);
    messagesState.activeMessageIds = next;
    updateProcessingState();
  }
}

export function markMessageComplete(id: string) {
  if (!id) return;
  if (messagesState.activeMessageIds.has(id)) {
    const next = new Set(messagesState.activeMessageIds);
    next.delete(id);
    messagesState.activeMessageIds = next;
    updateProcessingState();
  }
  clearRetryRuntime(id);
}

export function addPendingRequest(id: string) {
  if (!id) return;
  if (!messagesState.pendingRequests.has(id)) {
    const next = new Set(messagesState.pendingRequests);
    next.add(id);
    messagesState.pendingRequests = next;
    updateProcessingState();
  }
}

export function clearPendingRequest(id: string) {
  if (!id) return;
  if (messagesState.pendingRequests.has(id)) {
    const next = new Set(messagesState.pendingRequests);
    next.delete(id);
    messagesState.pendingRequests = next;
    updateProcessingState();
  }
}

export function clearProcessingState(options?: {
  /** 跳过防回抬保护（会话切换场景使用）。
   *  会话切换后紧接着 applyAuthoritativeProcessingState 恢复新会话的权威状态，
   *  不能让旧的 lastForcedIdleAt 阻断新状态写入。 */
  skipAntiLiftBack?: boolean;
}) {
  messagesState.backendProcessing = false;
  messagesState.activeMessageIds = new Set();
  messagesState.pendingRequests = new Set();
  clearAllRetryRuntime();
  if (options?.skipAntiLiftBack) {
    // 会话切换：清除防回抬标记，允许新会话的权威状态正常写入
    messagesState.lastForcedIdleAt = null;
  } else {
    // 用户手动中断/强制 idle：设置防回抬，阻止后端残留事件抬回 processing
    messagesState.lastForcedIdleAt = Date.now();
  }
  updateProcessingState();
}

export function settleProcessingForManualInteraction() {
  for (const binding of requestBindings.values()) {
    if (binding.timeoutId) {
      clearTimeout(binding.timeoutId);
    }
  }
  requestBindings = new Map();
  clearProcessingState();
}

/**
 * 终结所有未完成的流式消息和残留占位消息
 *
 * 任务结束（完成/打断/失败）时调用，确保：
 * 1. 已输出内容的流式消息标记为完成，保留内容展示
 * 2. 无内容的空占位消息被移除（避免残留"正在思考..."动画）
 * 3. 有内容的占位消息转为正常消息（去除占位标记）
 */

// 终结 instruction 消息中残留的 running lane tasks
function sealRunningLaneTasks(): boolean {
  let changed = false;
  mutateTimelineNodes((nodes) => nodes.map((node) => {
    const message = node.message;
    if (message.type !== 'instruction' || !Array.isArray(message.metadata?.laneTasks)) {
      return node;
    }
    const laneTasks = message.metadata.laneTasks as Array<Record<string, unknown>>;
    const hasRunning = laneTasks.some((task) => task.status === 'running');
    if (!hasRunning) {
      return node;
    }
    changed = true;
    return normalizeTimelineNode({
      ...node,
      message: {
        ...message,
        metadata: {
          ...message.metadata,
          laneTasks: laneTasks.map((task) => (
            task.status === 'running' ? { ...task, status: 'cancelled' } : task
          )),
        },
      },
    });
  }));
  return changed;
}

function sealRunningTaskCardsInList(list: Message[]): { changed: boolean; messages: Message[] } {
  let changed = false;
  const messages = list.map((message) => {
    if (message.type !== 'task_card') {
      return message;
    }
    const subTaskCard = message.metadata?.subTaskCard;
    if (!subTaskCard || typeof subTaskCard !== 'object') {
      return message;
    }
    const currentStatus = typeof (subTaskCard as { status?: unknown }).status === 'string'
      ? ((subTaskCard as { status: string }).status || '').trim()
      : '';
    const currentWaitStatus = typeof (subTaskCard as { wait_status?: unknown }).wait_status === 'string'
      ? (((subTaskCard as { wait_status: string }).wait_status) || '').trim()
      : '';
    const hasRunningStatus = currentStatus === 'running' || currentStatus === 'in_progress';
    const hasRunningWaitStatus = currentWaitStatus === 'running' || currentWaitStatus === 'in_progress';
    if (!hasRunningStatus && !hasRunningWaitStatus) {
      return message;
    }
    changed = true;
    return {
      ...message,
      metadata: {
        ...(message.metadata || {}),
        subTaskCard: {
          ...subTaskCard,
          ...(hasRunningStatus ? { status: 'cancelled' } : {}),
          ...(hasRunningWaitStatus ? { wait_status: 'cancelled' } : {}),
        },
      },
    };
  });
  return { changed, messages };
}

function sealRunningTaskCards(): boolean {
  let changed = false;
  mutateTimelineNodes((nodes) => nodes.map((node) => {
    const sealed = sealRunningTaskCardsInList([node.message]);
    if (!sealed.changed) {
      return node;
    }
    changed = true;
    return normalizeTimelineNode({
      ...node,
      message: sealed.messages[0],
    });
  }));
  return changed;
}

function sealRunningTasksStore(): boolean {
  let changed = false;
  const nextTasks = tasks.map((task) => {
    const nextSubTasks = (task.subTasks || []).map((subTask) => {
      if (subTask.status !== 'running' && subTask.status !== 'in_progress') {
        return subTask;
      }
      changed = true;
      return {
        ...subTask,
        status: 'cancelled' as const,
      };
    });
    const shouldSealTask = task.status === 'running';
    if (!shouldSealTask && nextSubTasks.every((subTask, index) => subTask === (task.subTasks || [])[index])) {
      return task;
    }
    if (shouldSealTask) {
      changed = true;
    }
    return {
      ...task,
      status: shouldSealTask ? 'cancelled' as const : task.status,
      subTasks: nextSubTasks,
    };
  });
  if (changed) {
    tasks = nextTasks;
  }
  return changed;
}

// 终结 mission plan 中残留的 running assignment/todo
function sealRunningMissionAssignments(): boolean {
  let changed = false;
  const next = new Map(missionPlan);
  for (const [missionId, plan] of next) {
    const hasRunning = (plan.assignments || []).some(a =>
      a.status === 'running'
      || (a.todos || []).some(t => t.status === 'running' || t.status === 'in_progress'),
    );
    if (!hasRunning) continue;
    changed = true;
    next.set(missionId, {
      ...plan,
      assignments: (plan.assignments || []).map(a => ({
        ...a,
        status: a.status === 'running' ? 'cancelled' : a.status,
        todos: (a.todos || []).map(t => ({
          ...t,
          status: (t.status === 'running' || t.status === 'in_progress') ? 'cancelled' : t.status,
        })),
      })),
    });
  }
  if (changed) {
    missionPlan = next;
  }
  return changed;
}

export function sealAllStreamingMessages() {
  // 先刷新所有 RAF 合并队列，确保封口前状态是最新的
  flushAllStreamUpdates();

  let threadChanged = false;
  let agentChanged = false;

  // 判断消息是否有可渲染内容
  const hasContent = (m: Message): boolean => {
    if (m.content && m.content.trim().length > 0) return true;
    if (m.blocks && m.blocks.length > 0) {
      return m.blocks.some(b => {
        if (!b || typeof b !== 'object') return false;
        if ('content' in b && typeof b.content === 'string' && b.content.trim().length > 0) return true;
        if (b.type === 'thinking' && b.thinking?.content && b.thinking.content.trim().length > 0) return true;
        if (b.type === 'tool_call') return true;
        if (b.type === 'plan' || b.type === 'file_change') return true;
        return false;
      });
    }
    return false;
  };

  // 处理单条消息：返回 null 表示应移除，返回新对象表示应更新
  const sealMessage = (m: Message): Message | null => {
    const isPlaceholder = Boolean(m.metadata?.isPlaceholder);
    const isStreaming = m.isStreaming;

    if (!isPlaceholder && !isStreaming) return m; // 无需处理

    // 空占位消息（无内容）→ 移除
    if (isPlaceholder && !hasContent(m)) return null;

    // 有内容的流式消息 / 有内容的占位消息 → 标记完成，保留内容
    return {
      ...m,
      isStreaming: false,
      isComplete: true,
      metadata: {
        ...(m.metadata || {}),
        isPlaceholder: false,
        placeholderState: undefined,
        wasPlaceholder: isPlaceholder ? true : m.metadata?.wasPlaceholder,
      },
    };
  };

  mutateTimelineNodes((nodes) => {
    const next: TimelineNode[] = [];
    for (const node of nodes) {
      const result = sealMessage(node.message);
      if (result === null) {
        threadChanged = true;
        agentChanged = true;
        continue;
      }
      if (result !== node.message) {
        threadChanged = true;
        agentChanged = true;
        next.push(normalizeTimelineNode({
          ...node,
          message: result,
        }));
        continue;
      }
      next.push(node);
    }
    return next;
  });

  // 终结 instruction 消息中残留的 running lane tasks 和 mission plan 中 running 的 assignment，
  // 避免插件重启/强制停止后 Worker 圆点持续显示"执行中"动画。
  const laneTasksChanged = sealRunningLaneTasks();
  const taskCardsChanged = sealRunningTaskCards();
  const tasksChanged = sealRunningTasksStore();
  const missionChanged = sealRunningMissionAssignments();

  if (threadChanged || agentChanged || laneTasksChanged || taskCardsChanged || tasksChanged || missionChanged) {
    saveWebviewState();
  }
}

/** 获取后端处理状态（用于时序判断） */
export function getBackendProcessing(): boolean {
  return messagesState.backendProcessing;
}

export function clearPendingInteractions() {
  pendingRecovery = null;
  pendingClarification = null;
  pendingWorkerQuestion = null;
}

function recomputeUnreadNotificationCount() {
  unreadNotificationCount = notifications.filter((n) => !n.read).length;
}

function resolveNotificationSessionId(sessionId: string | null | undefined): string {
  const normalized = typeof sessionId === 'string' ? sessionId.trim() : '';
  return normalized;
}

function getCurrentNotificationSessionId(): string {
  return resolveNotificationSessionId(messagesState.currentSessionId);
}

function applyNotificationList(nextList: Notification[]): Notification[] {
  const trimmed = nextList.slice(0, MAX_NOTIFICATIONS_PER_SESSION);
  notifications = trimmed;
  recomputeUnreadNotificationCount();
  return trimmed;
}

function syncNotificationsFromSession(sessionId: string | null | undefined): void {
  const resolvedSessionId = resolveNotificationSessionId(sessionId);
  const list = resolvedSessionId ? ensureArray<Notification>(notificationsBySession[resolvedSessionId]) : [];
  applyNotificationList(list);
}

function replaceSessionNotificationList(sessionId: string, nextList: Notification[]): void {
  const normalizedSessionId = resolveNotificationSessionId(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  const next = nextList.slice(0, MAX_NOTIFICATIONS_PER_SESSION);
  notificationsBySession = {
    ...notificationsBySession,
    [normalizedSessionId]: next,
  };
}

function updateCurrentSessionNotifications(updater: (current: Notification[]) => Notification[]): void {
  const sessionId = getCurrentNotificationSessionId();
  if (!sessionId) {
    applyNotificationList([]);
    return;
  }
  const current = ensureArray<Notification>(notificationsBySession[sessionId]);
  const next = applyNotificationList(updater(current));
  notificationsBySession = {
    ...notificationsBySession,
    [sessionId]: next,
  };
}

function resolveToastPolicy(options?: ToastOptions): {
  category: ToastCategory;
  persistToCenter: boolean;
  countUnread: boolean;
  source?: string;
  actionRequired?: boolean;
  displayMode: ToastDisplayMode;
  duration?: number;
} {
  const category = options?.category ?? 'feedback';
  const defaultPersistToCenter = false;
  const persistToCenter = options?.persistToCenter ?? defaultPersistToCenter;
  const defaultCountUnread = category === 'incident';
  const countUnread = persistToCenter ? (options?.countUnread ?? defaultCountUnread) : false;
  const actionRequired = options?.actionRequired ?? (category === 'incident');
  const displayMode = options?.displayMode ?? 'toast';
  return {
    category,
    persistToCenter,
    countUnread,
    source: options?.source,
    actionRequired,
    displayMode,
    duration: options?.duration,
  };
}

// 右下角同时可见的 toast 上限，防止密集通知堆积遮挡主阅读区
const MAX_VISIBLE_TOASTS = 5;

export function addToast(type: string, message: string, title?: string, options?: ToastOptions) {
  const policy = resolveToastPolicy(options);
  const id = `toast_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`;
  if (policy.displayMode === 'toast') {
    const toast: ToastRecord = {
      id,
      type,
      title,
      message,
      category: policy.category,
      source: policy.source,
      actionRequired: policy.actionRequired,
      duration: policy.duration,
    };
    // 超过上限时丢弃最旧的非 actionRequired toast
    let nextToasts = [...toasts, toast];
    while (nextToasts.length > MAX_VISIBLE_TOASTS) {
      const discardIndex = nextToasts.findIndex((t) => !t.actionRequired);
      if (discardIndex >= 0) {
        nextToasts.splice(discardIndex, 1);
      } else {
        break; // 全部都是 actionRequired，不丢弃
      }
    }
    toasts = nextToasts;
  }

  if (policy.displayMode === 'silent' || !policy.persistToCenter || policy.category === 'feedback') {
    return;
  }

  // 仅归档高价值通知到通知历史
  const notificationCategory: NotificationCategory = policy.category === 'incident' ? 'incident' : 'audit';
  const notification: Notification = {
    id,
    type,
    title,
    message,
    category: notificationCategory,
    source: policy.source,
    actionRequired: policy.actionRequired,
    timestamp: Date.now(),
    read: !policy.countUnread,
  };
  updateCurrentSessionNotifications((current) => [notification, ...current]);
}

export function getNotifications() {
  return notifications;
}

export function getUnreadNotificationCount() {
  return unreadNotificationCount;
}

export function markAllNotificationsRead() {
  updateCurrentSessionNotifications((current) => current.map((n) => ({ ...n, read: true })));
  vscode.postMessage({ type: 'markAllNotificationsRead' });
}

export function clearAllNotifications() {
  updateCurrentSessionNotifications(() => []);
  vscode.postMessage({ type: 'clearAllNotifications' });
}

export function removeNotification(id: string) {
  updateCurrentSessionNotifications((current) => current.filter((n) => n.id !== id));
  vscode.postMessage({ type: 'removeNotification', notificationId: id });
}

export function applySessionNotifications(
  sessionId: string,
  rawNotifications: { records?: SessionNotificationRecord[] } | SessionNotificationRecord[] | unknown,
): void {
  const normalizedSessionId = resolveNotificationSessionId(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  const normalized = normalizeSessionNotificationList(
    rawNotifications && typeof rawNotifications === 'object'
      ? (rawNotifications as { records?: unknown }).records
      : rawNotifications,
  );
  replaceSessionNotificationList(normalizedSessionId, normalized);
  if (normalizedSessionId === getCurrentNotificationSessionId()) {
    applyNotificationList(normalized);
  }
}

export function getActiveInteractionType(): string | null {
  if (pendingRecovery) return 'recovery';
  if (pendingClarification) return 'clarification';
  if (pendingWorkerQuestion) return 'workerQuestion';
  return null;
}

function getMessageEventSeq(message: Message | undefined): number | null {
  if (!message) return null;
  const normalized = resolveTimelineEventSeqFromMetadata(resolveMessageMetadataRecord(message));
  return normalized > 0 ? normalized : null;
}

function getMessageCardStreamSeq(message: Pick<Message, 'metadata'> | undefined): number {
  return resolveTimelineCardStreamSeqFromMetadata(resolveMessageMetadataRecord(message));
}

function compareIncomingMessageVersion(
  current: Pick<TimelineNode, 'latestEventSeq' | 'cardStreamSeq'>,
  incoming: Pick<Message, 'metadata'>,
): number {
  const currentEventSeq = typeof current.latestEventSeq === 'number' && Number.isFinite(current.latestEventSeq)
    ? Math.max(0, Math.floor(current.latestEventSeq))
    : 0;
  const currentCardStreamSeq = typeof current.cardStreamSeq === 'number' && Number.isFinite(current.cardStreamSeq)
    ? Math.max(0, Math.floor(current.cardStreamSeq))
    : 0;
  const currentVersion = resolveTimelineVersionFromMetadata({
    eventSeq: currentEventSeq,
    cardStreamSeq: currentCardStreamSeq,
  });
  const incomingVersion = resolveTimelineVersionFromMetadata(resolveMessageMetadataRecord(incoming));

  if (currentVersion > 0 && incomingVersion > 0 && incomingVersion !== currentVersion) {
    return incomingVersion > currentVersion ? 1 : -1;
  }

  if (currentVersion === 0 && incomingVersion > 0) {
    return 1;
  }

  return 0;
}

// ============ 流式更新 RAF 合并层 ============
// 在同一个动画帧内的多次 append delta 会被合并为一次 Svelte 状态更新，
// 消除逐 token 触发的 .map() + 数组重建风暴。

interface PendingStreamFlush {
  messageId: string;
  updates: Partial<Message>;
  /** 合并次数（调试用） */
  mergedCount: number;
}

const pendingTimelineFlushes = new Map<string, PendingStreamFlush>();
let streamFlushRAF: number | undefined;

function scheduleStreamFlush(): void {
  if (streamFlushRAF !== undefined) return;
  streamFlushRAF = requestAnimationFrame(flushAllStreamUpdates);
}

function flushAllStreamUpdates(): void {
  streamFlushRAF = undefined;
  if (pendingTimelineFlushes.size > 0) {
    const entries = Array.from(pendingTimelineFlushes.entries());
    pendingTimelineFlushes.clear();
    for (const [, flush] of entries) {
      updateTimelineNodeByAlias(flush.messageId, flush.updates);
    }
  }
}

/**
 * 将流式增量更新排入 RAF 合并队列。
 * 同一消息在同一帧内的多个 updates 会被 Object.assign 合并。
 * 对于 content 和 blocks 这类追加型字段，调用方（applyStreamUpdate）已经
 * 计算好了累积值，所以这里直接覆盖即可。
 */
function enqueueTimelineStreamUpdate(messageId: string, updates: Partial<Message>): void {
  const target = findTimelineMessageTargetByAlias(messageId);
  const flushKey = target?.flushKey || messageId;
  const existing = pendingTimelineFlushes.get(flushKey);
  if (existing) {
    Object.assign(existing.updates, updates);
    existing.mergedCount++;
  } else {
    pendingTimelineFlushes.set(flushKey, { messageId, updates: { ...updates }, mergedCount: 1 });
  }
  scheduleStreamFlush();
}

function normalizeMessageUpdates(updates: Partial<Message>): Partial<Message> {
  return sanitizeMessagePatch(updates, '[MessagesStore] 消息更新');
}

function getEffectiveTimelineMessage(messageId: string): Message | undefined {
  const target = findTimelineMessageTargetByAlias(messageId);
  if (!target) return undefined;
  const baseMessage = target.executionItem?.message || target.node.message;
  const pending = pendingTimelineFlushes.get(target.flushKey);
  if (!pending) return baseMessage;
  return { ...baseMessage, ...pending.updates };
}

function patchTimelineMessageByAlias(messageId: string, updates: Partial<Message>): Message | undefined {
  const target = findTimelineMessageTargetByAlias(messageId);
  if (!target) {
    return undefined;
  }
  const normalizedUpdates = normalizeMessageUpdates(updates);
  const baseMessage = target.executionItem?.message || target.node.message;
  const effectiveMessage = getEffectiveTimelineMessage(messageId) || baseMessage;
  if (compareIncomingMessageVersion(
    {
      latestEventSeq: getMessageEventSeq(effectiveMessage) ?? (target.executionItem?.latestEventSeq || target.node.latestEventSeq),
      cardStreamSeq: getMessageCardStreamSeq(effectiveMessage) || (target.executionItem?.cardStreamSeq || target.node.cardStreamSeq),
    },
    normalizedUpdates as Pick<Message, 'metadata'>,
  ) < 0) {
    return effectiveMessage;
  }
  const isStreamingAppend = baseMessage.isStreaming
    && !('isComplete' in normalizedUpdates && normalizedUpdates.isComplete)
    && !('isStreaming' in normalizedUpdates && normalizedUpdates.isStreaming === false);

  if (isStreamingAppend) {
    enqueueTimelineStreamUpdate(messageId, normalizedUpdates);
    return getEffectiveTimelineMessage(messageId);
  }

  pendingTimelineFlushes.delete(target.flushKey);
  return updateTimelineNodeByAlias(messageId, normalizedUpdates);
}

export function patchThreadPlaceholderMessage(messageId: string, updates: Partial<Message>) {
  const target = findTimelineMessageTargetByAlias(messageId);
  if (target?.node.visibleInThread) {
    // 仅允许请求占位消息在后端 projection 快照到达前做局部状态补丁。
    patchTimelineMessageByAlias(messageId, updates);
  }
}

/**
 * 应用流式增量更新到时间线消息。
 * 由 message-handler 的 handleStandardUpdate 调用，
 * 将后端 unifiedUpdate 中的 append/replace/block_update 补丁应用到对应的时间线节点。
 */
export function applyTimelineStreamPatch(messageId: string, updates: Partial<Message>): void {
  patchTimelineMessageByAlias(messageId, updates);
}

/**
 * 根据 messageId 获取当前时间线中的消息（包含 RAF 队列中的 pending 更新）。
 */
export function getTimelineMessageById(messageId: string): Message | undefined {
  return getEffectiveTimelineMessage(messageId);
}

/**
 * 根据 cardId 获取当前时间线中的宿主消息。
 * 用于处理 update.messageId 与锚点 messageId 不一致，但 cardId 一致的流式场景。
 */
export function getTimelineMessageByCardId(cardId: string): Message | undefined {
  const normalizedCardId = typeof cardId === 'string' ? cardId.trim() : '';
  if (!normalizedCardId) {
    return undefined;
  }
  const nodeId = timelineNodeIdByCardId.get(normalizedCardId);
  if (!nodeId) {
    return undefined;
  }
  return getEffectiveTimelineMessage(nodeId);
}

// 清空所有消息（用于会话切换/新建）
export function clearAllMessages(options: {
  persist?: boolean;
  resetTimelineView?: boolean;
  resetPanelState?: boolean;
  /** 跨 session 切换时设为 true，跳过防回抬保护 */
  skipAntiLiftBack?: boolean;
} = {}) {
  captureCurrentSessionViewState();
  pendingTimelineFlushes.clear();
  if (streamFlushRAF !== undefined) {
    cancelAnimationFrame(streamFlushRAF);
    streamFlushRAF = undefined;
  }
  if (options.resetTimelineView !== false) {
    replaceTimelineNodes([]);
    messagesState.timelineProjection = null;
    timelineProjectionDirty = false;
  }
  workerWaitResults = {};
  messagesState.orchestratorRuntimeState = null;
  messagesState.queuedMessages = [];
  messagesState.messageJump = {
    messageId: null,
    nonce: messagesState.messageJump.nonce,
  };
  clearPendingInteractions();
  clearProcessingState({ skipAntiLiftBack: options.skipAntiLiftBack });
  // 会话级运行时状态：会话切换时必须清理，避免旧数据泄漏到新会话
  waveState = null;
  missionPlan = new Map();
  if (options.resetPanelState !== false) {
    resetPanelScrollRuntimeState();
  }
  if (options.persist !== false) {
    saveWebviewState();
  }
}

function buildTimelineNodesFromProjection(projection: SessionTimelineProjection): TimelineNode[] {
  const canonicalProjection = canonicalizeTimelineProjection(projection);
  const nextNodes = ensureArray(canonicalProjection.artifacts)
    .filter(isProjectionArtifact)
    .map((artifact) => {
      const message = normalizeProjectionRestoredMessage(artifact.message);
      const worker = resolveTimelineWorker(message) || artifact.worker;
      const executionItems = ensureArray<TimelineExecutionItem>(artifact.executionItems)
        .map((item) => normalizeTimelineExecutionItem({
          ...item,
          message: normalizeProjectionRestoredMessage(item.message),
        }));
      return normalizeTimelineNode({
        nodeId: artifact.artifactId,
        kind: artifact.kind || resolveTimelineNodeKind(message),
        displayOrder: artifact.displayOrder,
        artifactVersion: typeof artifact.artifactVersion === 'number' ? artifact.artifactVersion : undefined,
        anchorEventSeq: typeof artifact.anchorEventSeq === 'number' ? artifact.anchorEventSeq : (getMessageEventSeq(message) ?? 0),
        latestEventSeq: typeof artifact.latestEventSeq === 'number' ? artifact.latestEventSeq : (getMessageEventSeq(message) ?? 0),
        cardStreamSeq: typeof artifact.cardStreamSeq === 'number'
          ? artifact.cardStreamSeq
          : getMessageCardStreamSeq(message),
        timestamp: typeof artifact.timestamp === 'number' ? artifact.timestamp : resolveMessageSortTimestamp(message),
        cardId: artifact.cardId || resolveTimelineCardId(message),
        lifecycleKey: artifact.lifecycleKey || resolveTimelineLifecycleKey(message),
        dispatchWaveId: artifact.dispatchWaveId,
        laneId: artifact.laneId,
        workerCardId: artifact.workerCardId,
        worker,
        visibleInThread: artifact.threadVisible !== false,
        workerTabs: normalizeWorkerTabList(artifact.workerTabs),
        messageIds: Array.from(new Set([
          artifact.artifactId,
          ...ensureArray<string>(artifact.messageIds),
        ])),
        message: {
          ...message,
          id: artifact.artifactId,
        },
        executionItems,
      });
    });
  return nextNodes;
}

export function setTimelineProjection(projection: SessionTimelineProjection) {
  const canonicalProjection = canonicalizeTimelineProjection(projection);
  const nextNodes = buildTimelineNodesFromProjection(canonicalProjection);
  messagesState.timelineProjection = canonicalProjection;
  setTimelineProjectionNodes(nextNodes);
  timelineProjectionDirty = false;
  workerWaitResults = {};
  upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(canonicalProjection.sessionId));
  saveWebviewState();
}

export function restoreTimelineProjectionIfNewer(
  projection: SessionTimelineProjection,
): boolean {
  const normalizedSessionId = normalizeSessionId(projection.sessionId);
  if (!normalizedSessionId) {
    return false;
  }
  const currentSessionId = normalizeSessionId(messagesState.currentSessionId);
  if (currentSessionId && currentSessionId !== normalizedSessionId) {
    return false;
  }
  const currentProjection = ensureTimelineProjectionSnapshotCurrent(normalizedSessionId);
  if (compareTimelineProjectionFreshness(projection, currentProjection) <= 0) {
    return false;
  }
  setTimelineProjection(projection);
  return true;
}

// 导出状态初始化
export function initializeState() {
  clearAllRetryRuntime();
  resetPanelScrollRuntimeState();
  sessionViewStateBySession = {};
  const persisted = vscode.getState<WebviewPersistedState>();
  if (persisted) {
    const requestedSessionId = typeof messagesState.currentSessionId === 'string'
      ? messagesState.currentSessionId.trim()
      : '';
    const persistedSessionId = typeof persisted.currentSessionId === 'string'
      ? persisted.currentSessionId.trim()
      : '';
    const shouldRestoreSessionScopedState = !requestedSessionId || !persistedSessionId || requestedSessionId === persistedSessionId;
    const validSessions = isValidPersistedArray(persisted.sessions, MAX_PERSISTED_ARRAY_LENGTH);
    if (!validSessions) {
      replaceTimelineNodes([]);
      messagesState.sessions = [];
      messagesState.currentSessionId = messagesState.currentSessionId || null;
      notificationsBySession = {};
      messagesState.orchestratorRuntimeState = null;
      clearPendingInteractions();
      clearProcessingState();
      saveWebviewState();
      return;
    }
    // Tab 状态不持久化，每次打开都默认显示主对话 tab
    messagesState.currentTopTab = 'thread';
    messagesState.currentBottomTab = 'thread';
    replaceTimelineNodes([]);
    sessionViewStateBySession = normalizePersistedSessionViewStateMap(persisted.sessionViewStateBySession);
    const sessionSeen = new Set<string>();
    messagesState.sessions = ensureArray<Session>(persisted.sessions)
      .filter((session) => !!session && typeof session.id === 'string' && session.id.trim().length > 0)
      .filter((session) => {
        if (sessionSeen.has(session.id)) return false;
        sessionSeen.add(session.id);
        return true;
    });
    messagesState.currentSessionId = shouldRestoreSessionScopedState
      ? (persisted.currentSessionId || messagesState.currentSessionId || null)
      : (messagesState.currentSessionId || null);
    const restoredSessionViewState = shouldRestoreSessionScopedState
      ? applySessionViewState(messagesState.currentSessionId)
      : false;
    if (shouldRestoreSessionScopedState && !restoredSessionViewState) {
      messagesState.scrollPositions = normalizePersistedScrollPositions(persisted.scrollPositions);
      messagesState.scrollAnchors = normalizePersistedScrollAnchors(persisted.scrollAnchors);
      messagesState.autoScrollEnabled = normalizePersistedAutoScrollConfig(persisted.autoScrollEnabled);
    }
    notificationsBySession = {};
    messagesState.orchestratorRuntimeState = null;
    syncNotificationsFromSession(messagesState.currentSessionId);

    const persistedProjection = shouldRestoreSessionScopedState
      ? normalizePersistedTimelineProjection(
          persisted.currentTimelineProjection,
          messagesState.currentSessionId,
        )
      : null;
    if (!restoredSessionViewState && persistedProjection) {
      setTimelineProjection(persistedProjection);
    }

    // 启动恢复：历史展示可先从本地恢复，但运行态必须以后端快照为唯一真相源。
    clearPendingInteractions();
    clearProcessingState();
    saveWebviewState();
  }
}

// ============ Wave 状态操作（提案 4.6） ============

export function setWaveState(state: WaveState | null) {
  waveState = state;
}

export function updateWaveProgress(waveIndex: number, status: WaveState['status']) {
  if (waveState) {
    waveState = {
      ...waveState,
      currentWave: waveIndex,
      status,
    };
  }
}

export function clearWaveState() {
  waveState = null;
}

// ============ 请求-响应绑定操作（消息响应流设计） ============

/**
 * 创建请求绑定
 */
export function createRequestBinding(binding: RequestResponseBinding): void {
  const next = new Map(requestBindings);
  next.set(binding.requestId, binding);
  requestBindings = next;
}

export function setRetryRuntime(messageId: string, runtime: RetryRuntimeState): void {
  if (!messageId) return;
  const next = new Map(retryRuntimeState.byMessageId);
  next.set(messageId, runtime);
  retryRuntimeState.byMessageId = next;
}

export function clearRetryRuntime(messageId: string): void {
  if (!messageId || !retryRuntimeState.byMessageId.has(messageId)) {
    return;
  }
  const next = new Map(retryRuntimeState.byMessageId);
  next.delete(messageId);
  retryRuntimeState.byMessageId = next;
}

export function clearAllRetryRuntime(): void {
  retryRuntimeState.byMessageId = new Map();
}

/**
 * 获取请求绑定
 */
export function getRequestBinding(requestId: string): RequestResponseBinding | undefined {
  return requestBindings.get(requestId);
}

/**
 * 更新请求绑定（添加 realMessageId）
 */
export function updateRequestBinding(
  requestId: string,
  updates: Partial<RequestResponseBinding>
): void {
  const existing = requestBindings.get(requestId);
  if (existing) {
    const updated = { ...existing, ...updates };
    const next = new Map(requestBindings);
    next.set(requestId, updated);
    requestBindings = next;
  }
}

/**
 * 清除请求绑定
 */
export function clearRequestBinding(requestId: string): void {
  const next = new Map(requestBindings);
  next.delete(requestId);
  requestBindings = next;
}

/**
 * 根据占位消息 ID 查找请求绑定
 */
export function findBindingByPlaceholder(placeholderMessageId: string): RequestResponseBinding | undefined {
  for (const binding of requestBindings.values()) {
    if (binding.placeholderMessageId === placeholderMessageId) {
      return binding;
    }
  }
  return undefined;
}

/**
 * 清除所有请求绑定（会话切换时使用）
 */
export function clearAllRequestBindings(): void {
  requestBindings = new Map();
}
