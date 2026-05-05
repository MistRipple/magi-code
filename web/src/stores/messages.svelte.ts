/**
 * 消息状态管理 - Svelte 5 Runes
 * 使用细粒度响应式实现高效的流式更新
 */

import type {
  Message,
  AgentOutputs,
  AgentId,
  TimelineProjectionArtifact,
  TimelineProjectionRenderEntry,
  TimelineNode,
  TimelineNodeKind,
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
  QueuedMessage,
  OrchestratorRuntimeState,
  SessionNotificationRecord,
} from '../types/message';
import { vscode } from '../lib/vscode-bridge';
import { ensureArray } from '../lib/utils';

import { deriveWorkerRuntimeMap } from '../lib/worker-panel-state';
import { normalizeWorkerSlot } from '../lib/message-classifier';
import {
  buildTimelinePanelMessages,
} from '../lib/timeline-render-items';
import {
  compareTimelineSemanticOrder,
  type TimelineSemanticOrderInput,
  resolveTimelineBlockSeqFromMetadata,
  resolveTimelineCardStreamSeqFromMetadata,
  resolveTimelineEventSeqFromMetadata,
  resolveTimelineItemSeqFromMetadata,
  resolveTimelineLaneSeqFromMetadata,
  resolveTimelineSortTimestamp,
  resolveTimelineTurnOrderSeqFromMetadata,
  resolveTimelineVersionFromMetadata,
} from '../shared/timeline-ordering';
import {
  resolveTimelinePresentationKind,
} from '../shared/timeline-presentation';
import {
  resolveTimelineWorkerId,
} from '../shared/timeline-worker-lifecycle';
import {
  normalizeMessagePayload,
} from '../lib/message-payload';
import type { SettingsBootstrapSnapshot } from '../shared/settings-bootstrap';
import type { RoleTemplate } from '../shared/types/role-templates';
import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';

interface SettingsRegistrySnapshot {
  roleTemplates: RoleTemplate[];
  registryEngines: ModelEngine[];
  registryAgents: AgentBinding[];
}

export type TaskComposerIntent = 'supplement_context' | 'append_task' | 'replan';

export interface TaskComposerDraft {
  intent: TaskComposerIntent;
  taskId: string | null;
  text: string;
}

type NotificationCenterOperation = 'load' | 'append' | 'mark-read' | 'clear' | 'remove';

export interface NotificationCenterStatus {
  isLoading: boolean;
  operation: NotificationCenterOperation | null;
  error: string | null;
  updatedAt: number | null;
}

interface NotificationOperationScope {
  sessionId: string;
  workspaceId: string;
  workspacePath: string;
}

// ============ 状态定义 ============
// 🔧 修复：使用对象属性模式确保跨模块响应式正常工作
// Svelte 5 官方推荐：导出对象并修改其属性，而非重新赋值独立变量

/**
 * 核心消息状态
 * 使用对象属性模式确保跨模块响应式追踪
 */
export const messagesState = $state({
  // 启动状态：后端 bootstrap 数据是否已就绪
  bootstrapped: false,

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
  currentWorkspaceId: null as string | null,
  currentWorkspacePath: '' as string,
  sessions: [] as Session[],
  currentSessionId: null as string | null,
  sessionHistory: {
    workspaceId: null as string | null,
    sessionId: null as string | null,
    hasMoreBefore: false,
    beforeCursor: null as string | null,
    isLoadingBefore: false,
  },
  queuedMessages: [] as QueuedMessage[],
  taskComposerDraft: null as TaskComposerDraft | null,
  notificationCenter: {
    isLoading: false,
    operation: null,
    error: null,
    updatedAt: null,
  } as NotificationCenterStatus,

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
    agent: 'orchestrator',
  } as ProcessingActor,

  // 后端下发的完整状态
  appState: null as AppState | null,
  orchestratorRuntimeState: null as OrchestratorRuntimeState | null,
  settingsBootstrapSnapshot: null as SettingsBootstrapSnapshot | null,
  settingsRegistrySnapshot: null as SettingsRegistrySnapshot | null,

  // 滚动状态（动态 key，初始只保留 thread）
  scrollPositions: {
    thread: 0,
  } as ScrollPositions,
  scrollAnchors: {
    thread: { messageId: null, offsetTop: 0 },
  } as ScrollAnchors,
  autoScrollEnabled: {
    thread: true,
  } as AutoScrollConfig,
});

// 消息列表限制
const IS_HOSTED_WEBVIEW = (
  typeof globalThis !== 'undefined'
  && typeof (globalThis as { acquireVsCodeApi?: unknown }).acquireVsCodeApi === 'function'
);
const MAX_TIMELINE_NODES = IS_HOSTED_WEBVIEW ? 2000 : 1000;

const MAX_PERSISTED_ARRAY_LENGTH = 10000;
const WEBVIEW_STATE_SAVE_DEBOUNCE_MS = 120;

/** 全局递增 displayOrder 计数器：节点首次创建时分配稳定序号，后续永不重算 */
let timelineDisplayOrderCounter = 0;
/** 本地 turnOrderSeq 计数器：发送意图创建时分配，作为 live 渲染轮次事实 */
let localTimelineTurnOrderSeqCounter = 0;

type ScrollPanelKey = keyof ScrollPositions;

const DEFAULT_SCROLL_ANCHOR: ScrollAnchor = { messageId: null, offsetTop: 0 };

function createDefaultScrollAnchors(): ScrollAnchors {
  return {
    thread: { ...DEFAULT_SCROLL_ANCHOR },
  };
}

function createDefaultScrollPositions(): ScrollPositions {
  return {
    thread: 0,
  };
}

function createDefaultAutoScrollConfig(): AutoScrollConfig {
  return {
    thread: true,
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
  const source = value as Record<string, unknown>;
  const result: ScrollPositions = { thread: normalizeScrollTop(typeof source.thread === 'number' ? source.thread : 0) };
  // 动态恢复所有持久化的 worker 滚动位置
  for (const key of Object.keys(source)) {
    if (key !== 'thread' && typeof source[key] === 'number') {
      result[key] = normalizeScrollTop(source[key] as number);
    }
  }
  return result;
}

function normalizePersistedScrollAnchors(value: unknown): ScrollAnchors {
  const defaults = createDefaultScrollAnchors();
  if (!value || typeof value !== 'object') {
    return defaults;
  }
  const source = value as Record<string, unknown>;
  const result: ScrollAnchors = { thread: normalizeScrollAnchor(source.thread as ScrollAnchor | null | undefined) };
  // 动态恢复所有持久化的 worker 滚动锚点
  for (const key of Object.keys(source)) {
    if (key !== 'thread' && source[key] && typeof source[key] === 'object') {
      result[key] = normalizeScrollAnchor(source[key] as ScrollAnchor | null | undefined);
    }
  }
  return result;
}

function normalizePersistedAutoScrollConfig(value: unknown): AutoScrollConfig {
  const defaults = createDefaultAutoScrollConfig();
  if (!value || typeof value !== 'object') {
    return defaults;
  }
  const source = value as Record<string, unknown>;
  const result: AutoScrollConfig = { thread: typeof source.thread === 'boolean' ? source.thread : defaults.thread };
  // 动态恢复所有持久化的 worker 自动滚动配置
  for (const key of Object.keys(source)) {
    if (key !== 'thread' && typeof source[key] === 'boolean') {
      result[key] = source[key] as boolean;
    }
  }
  return result;
}

type QueuedMessageImageItem = NonNullable<QueuedMessage['images']>[number];

function normalizeQueuedMessageImage(image: unknown): QueuedMessageImageItem | null {
  if (!image || typeof image !== 'object') {
    return null;
  }
  const item = image as { name?: unknown; dataUrl?: unknown };
  if (typeof item.dataUrl !== 'string' || item.dataUrl.trim().length === 0) {
    return null;
  }
  return {
    name: typeof item.name === 'string' && item.name.trim() ? item.name.trim() : 'image',
    dataUrl: item.dataUrl,
  };
}

function normalizeQueuedMessageList(value: unknown): QueuedMessage[] {
  const seen = new Set<string>();
  return ensureArray<QueuedMessage>(value)
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
      const id = item.id.trim();
      if (seen.has(id)) return false;
      seen.add(id);
      return true;
    })
    .map((item) => ({
      id: item.id.trim(),
      requestId: typeof item.requestId === 'string' && item.requestId.trim()
        ? item.requestId.trim()
        : undefined,
      localMessageId: typeof item.localMessageId === 'string' && item.localMessageId.trim()
        ? item.localMessageId.trim()
        : undefined,
      blockedByUserMessageId: typeof item.blockedByUserMessageId === 'string' && item.blockedByUserMessageId.trim()
        ? item.blockedByUserMessageId.trim()
        : undefined,
      blockedByUserContent: typeof item.blockedByUserContent === 'string' && item.blockedByUserContent.trim()
        ? item.blockedByUserContent.trim()
        : undefined,
      content: item.content,
      text: typeof item.text === 'string' ? item.text : null,
      createdAt: item.createdAt,
      mode: item.mode === 'guide' ? 'guide' : 'queue',
      deepTask: item.deepTask === true,
      skillName: typeof item.skillName === 'string' && item.skillName.trim()
        ? item.skillName.trim()
        : null,
      images: ensureArray(item.images)
        .map(normalizeQueuedMessageImage)
        .filter((image): image is QueuedMessageImageItem => image !== null),
    }));
}

function normalizePersistedQueuedMessageMap(value: unknown): Record<string, QueuedMessage[]> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return {};
  }
  const normalized: Record<string, QueuedMessage[]> = {};
  let count = 0;
  for (const [rawSessionId, rawMessages] of Object.entries(value as Record<string, unknown>)) {
    if (count >= MAX_PERSISTED_ARRAY_LENGTH) {
      break;
    }
    const sessionId = normalizeSessionId(rawSessionId);
    if (!sessionId) {
      continue;
    }
    const queued = normalizeQueuedMessageList(rawMessages);
    if (queued.length === 0) {
      continue;
    }
    normalized[sessionId] = queued;
    count += 1;
  }
  return normalized;
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
  return {
    sessionId: normalizedSessionId,
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
interface PersistedSessionExecutionState {
  sessionId: string;
  edits: Array<{
    filePath: string;
    snapshotId?: string;
    type?: string;
    additions?: number;
    deletions?: number;
    contributors?: string[];
    workerId?: string;
    executionGroupId?: string;
  }>;
  orchestratorRuntimeState: OrchestratorRuntimeState | null;
  pendingChanges: unknown[];
}
let sessionExecutionStateBySession = $state<Record<string, PersistedSessionExecutionState>>({});
let sessionQueuedMessagesBySession = $state<Record<string, QueuedMessage[]>>({});
let webviewStateBatchDepth = 0;
let webviewStateBatchPending = false;
let timelineProjectionSource: 'none' | 'live' | 'authoritative' = 'none';

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

function getCurrentPanelScrollTop(panel: ScrollPanelKey): number {
  return normalizeScrollTop(messagesState.scrollPositions[panel] ?? 0);
}

function getCurrentPanelAutoScrollEnabled(panel: ScrollPanelKey): boolean {
  const value = messagesState.autoScrollEnabled[panel];
  return typeof value === 'boolean' ? value : true;
}

function getCurrentPanelScrollAnchor(panel: ScrollPanelKey): ScrollAnchor {
  return normalizeScrollAnchor(messagesState.scrollAnchors[panel]);
}

function isValidPersistedArray(value: unknown, max: number): value is unknown[] {
  if (!Array.isArray(value)) return false;
  const length = value.length;
  if (!Number.isFinite(length) || length < 0 || length > max) return false;
  return true;
}

// 新增状态：变更、阶段、Toast、模型状态
let edits = $state<Array<{ filePath: string; snapshotId?: string; type?: string; additions?: number; deletions?: number; contributors?: string[]; workerId?: string; executionGroupId?: string; diff?: string; originalContent?: string; previewContent?: string; previewAbsolutePath?: string; previewCanOpenWorkspaceFile?: boolean }>>([]);
let timelineProjectionDirty = false;
// 统一 Worker 运行态（唯一权威来源）
// 当前主线以 authoritative projection 为准，仅叠加尚未被后端接纳的本地乐观节点。



const messageProjection = $derived.by(() => {
  const projection = messagesState.timelineProjection;
  const workers: Record<string, Message[]> = {};
  if (!projection) {
    return {
      thread: [],
      workers,
    };
  }
  const workerKeys = new Set<string>();
  for (const artifact of projection.artifacts || []) {
    for (const workerId of artifact.workerTabs || []) {
      if (typeof workerId === 'string' && workerId.trim()) {
        workerKeys.add(workerId.trim());
      }
    }
  }
  for (const workerId of workerKeys) {
    workers[workerId] = buildTimelinePanelMessages(projection, 'worker', workerId);
  }
  return {
    thread: buildTimelinePanelMessages(projection, 'thread'),
    workers,
  };
});

const workerRuntime = $derived.by(() => deriveWorkerRuntimeMap({
  messagesByWorker: messageProjection.workers,
  pendingRequestIds: messagesState.pendingRequests,
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
  countUnread: boolean;
  timestamp: number;
  read: boolean;
}
const MAX_NOTIFICATIONS_PER_SESSION = 200;

let notifications = $state<Notification[]>([]);
let unreadNotificationCount = $state(0);
let notificationsBySession = $state<Record<string, Notification[]>>({});

let modelStatus = $state<ModelStatusMap>({
  orchestrator: { status: 'checking' },
  auxiliary: { status: 'checking' },
});

// ============ 角色驱动 Tab 状态 ============
// 前端轻量 Agent 信息（从 AgentBinding + RoleTemplate 合成）
export interface EnabledAgent {
  /** 角色模板 ID（= 运行时 agentId = workerSlot） */
  templateId: string;
  /** 模型来源：继承编排模型或显式绑定引擎 */
  modelSource: 'orchestrator' | 'engine';
  /** 展示名称（来自 RoleTemplate.displayName） */
  displayName: string;
  /** 展示名称国际化 key（若存在则优先用于 UI 本地化） */
  displayNameKey?: string;
  /** 绑定的引擎 ID */
  engineId: string;
  /** 排序序号 */
  order: number;
  /** CSS 颜色 token（来自 RoleTemplate.defaultUI.colorToken） */
  colorToken: string;
  /** 图标名称 */
  icon?: string;
}

let enabledAgents = $state<EnabledAgent[]>([]);
const timelineNodeIdByMessageId = new Map<string, string>();
const timelineNodeIdByCardId = new Map<string, string>();
const timelineNodeIdByLifecycleKey = new Map<string, string>();

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
  const existing = currentTimestamp ?? 0;
  if (existing > 0) {
    return existing;
  }
  return resolveMessageSortTimestamp(message) || 0;
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
  const category = item.kind === 'incident'
    ? 'incident'
    : 'audit';
  const persistToCenter = item.persistToCenter !== false;
  if (!persistToCenter) {
    return null;
  }
  const timestamp = typeof item.createdAt === 'number' && Number.isFinite(item.createdAt)
    ? item.createdAt
    : Date.now();
  const read = typeof item.read === 'boolean'
    ? item.read
    : Boolean(item.handled);
  const countUnread = typeof item.countUnread === 'boolean'
    ? item.countUnread
    : category === 'incident';
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
    countUnread,
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

function preserveStableTurnOrderFact(
  existingMessage: Message | undefined,
  nextMessage: Message,
): Message {
  if (!existingMessage) {
    return nextMessage;
  }
  const existingMetadata = resolveMessageMetadataRecord(existingMessage);
  const nextMetadata = resolveMessageMetadataRecord(nextMessage);
  const existingTurnOrderSeq = normalizePositiveSequence(existingMetadata?.turnOrderSeq);
  const nextTurnOrderSeq = normalizePositiveSequence(nextMetadata?.turnOrderSeq);
  if (existingTurnOrderSeq <= 0 || nextTurnOrderSeq > 0) {
    return nextMessage;
  }
  return {
    ...nextMessage,
    metadata: {
      ...nextMetadata,
      turnOrderSeq: existingTurnOrderSeq,
    },
  };
}

function normalizeWorkerTabList(workerTabs: AgentId[] | undefined): AgentId[] {
  if (!Array.isArray(workerTabs)) return [];
  const next = new Set<AgentId>();
  for (const worker of workerTabs) {
    const normalizedWorker = normalizeWorkerSlot(worker);
    if (normalizedWorker) {
      next.add(normalizedWorker);
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
  const nextWorkers: Partial<Record<AgentId, number>> = {};
  const rawWorkers = laneOrder.workers;
  if (rawWorkers && typeof rawWorkers === 'object') {
    for (const worker of Object.keys(rawWorkers)) {
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
  return rawCardId || undefined;
}

function resolveTimelineWorker(message: Message): AgentId | undefined {
  const worker = resolveTimelineWorkerId(
    resolveMessageMetadataRecord(message),
    { fallbacks: [message.source] },
  );
  return worker || undefined;
}

function resolveTimelineNodeKind(message: Message): TimelineNodeKind {
  return resolveTimelinePresentationKind(message);
}


function resolveCanonicalTurnItemId(message: Pick<Message, 'id' | 'metadata'> | undefined): string {
  const metadata = resolveMessageMetadataRecord(message);
  const rawTurnItemId = metadata?.turnItemId;
  return typeof rawTurnItemId === 'string' && rawTurnItemId.trim()
    ? rawTurnItemId.trim()
    : '';
}

function resolveTimelineNodeId(message: Message): string {
  return resolveCanonicalTurnItemId(message) || message.id;
}

function normalizePositiveSequence(value: unknown): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    return 0;
  }
  const normalized = Math.floor(value);
  return normalized > 0 ? normalized : 0;
}

function maxTimelineTurnOrderSeqFromMessage(message: Pick<Message, 'metadata'> | undefined): number {
  return resolveTimelineTurnOrderSeqFromMetadata(resolveMessageMetadataRecord(message));
}

function maxTimelineTurnOrderSeqFromArtifacts(artifacts: TimelineProjectionArtifact[] | undefined): number {
  return ensureArray<TimelineProjectionArtifact>(artifacts).reduce(
    (maxSeq, artifact) => Math.max(maxSeq, maxTimelineTurnOrderSeqFromMessage(artifact.message)),
    0,
  );
}

function maxTimelineTurnOrderSeqFromNodes(nodes: TimelineNode[] | undefined): number {
  return ensureArray<TimelineNode>(nodes).reduce(
    (maxSeq, node) => Math.max(maxSeq, maxTimelineTurnOrderSeqFromMessage(node.message)),
    0,
  );
}

function resolveTimelineAliasId(rawId: string | undefined): string {
  const normalized = typeof rawId === 'string' ? rawId.trim() : '';
  if (!normalized) return '';
  return timelineNodeIdByMessageId.get(normalized) || normalized;
}

function getMessageBlockSeq(message: Pick<Message, 'metadata'> | undefined): number {
  return resolveTimelineBlockSeqFromMetadata(resolveMessageMetadataRecord(message));
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
  const explicitCardStreamSeq = typeof node.cardStreamSeq === 'number' && Number.isFinite(node.cardStreamSeq)
    ? Math.max(0, Math.floor(node.cardStreamSeq))
    : 0;
  const cardStreamSeq = explicitCardStreamSeq || getMessageCardStreamSeq(stableMessage);
  const cardId = resolveTimelineCardId(stableMessage);
  const dispatchWaveId = typeof stableMessage.metadata?.dispatchWaveId === 'string'
    ? stableMessage.metadata.dispatchWaveId.trim()
    : (typeof node.dispatchWaveId === 'string' ? node.dispatchWaveId.trim() : '');
  const laneId = typeof stableMessage.metadata?.laneId === 'string'
    ? stableMessage.metadata.laneId.trim()
    : (typeof node.laneId === 'string' ? node.laneId.trim() : '');
  const laneOrder = normalizeTimelineLaneOrder(node.laneOrder);
  const worker = resolveTimelineWorker(stableMessage) || node.worker;
  const workerTabs = normalizeWorkerTabList(node.workerTabs);
  const messageIds = Array.from(new Set([
    stableNodeId,
    ...ensureArray<string>(node.messageIds),
  ]));
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
    ...(dispatchWaveId ? { dispatchWaveId } : {}),
    ...(laneId ? { laneId } : {}),
    ...(worker ? { worker } : {}),

    visibleInThread: node.visibleInThread !== false,
    workerTabs,
    messageIds,
    message: stableMessage,
  };
}

function rebuildTimelineIndexes(): void {
  timelineNodeIdByMessageId.clear();
  timelineNodeIdByCardId.clear();
  timelineNodeIdByLifecycleKey.clear();
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
  }
}

function nodeToOrderInput(node: TimelineNode): TimelineSemanticOrderInput {
  const metadata = resolveMessageMetadataRecord(node.message);
  return {
    turnOrderSeq: resolveTimelineTurnOrderSeqFromMetadata(metadata),
    itemSeq: resolveTimelineItemSeqFromMetadata(metadata),
    laneSeq: resolveTimelineLaneSeqFromMetadata(metadata),
    blockSeq: resolveTimelineBlockSeqFromMetadata(metadata),
    displayOrder: node.displayOrder || 0,
  };
}

function compareTimelineNodeOrder(left: TimelineNode, right: TimelineNode): number {
  return compareTimelineSemanticOrder(nodeToOrderInput(left), nodeToOrderInput(right));
}

interface LocalProjectionFlatRenderEntry {
  entryId: string;
  artifactId: string;
  message: Message;
  timestamp: number;
  turnOrderSeq: number;
  itemSeq: number;
  laneSeq: number;
  anchorEventSeq: number;
  blockSeq: number;
  cardStreamSeq: number;
}

function renderEntryToOrderInput(entry: LocalProjectionFlatRenderEntry): TimelineSemanticOrderInput {
  return {
    turnOrderSeq: entry.turnOrderSeq,
    itemSeq: entry.itemSeq,
    laneSeq: entry.laneSeq,
    blockSeq: entry.blockSeq,
    displayOrder: entry.cardStreamSeq || 0,
  };
}

function compareRenderEntryOrder(
  left: LocalProjectionFlatRenderEntry,
  right: LocalProjectionFlatRenderEntry,
): number {
  return compareTimelineSemanticOrder(
    renderEntryToOrderInput(left),
    renderEntryToOrderInput(right),
  );
}

function buildDynamicWorkerRenderEntries(
  artifacts: TimelineProjectionArtifact[],
): Record<string, TimelineProjectionRenderEntry[]> {
  const workerIds = new Set<string>();
  for (const artifact of artifacts) {
    for (const workerId of ensureArray<AgentId>(artifact.workerTabs)) {
      workerIds.add(workerId);
    }
  }
  const entries: Record<string, TimelineProjectionRenderEntry[]> = {};
  for (const workerId of workerIds) {
    entries[workerId] = buildProjectionRenderEntriesFromArtifacts(artifacts, 'worker', workerId);
  }
  return entries;
}

function buildProjectionRenderEntriesFromArtifacts(
  artifacts: TimelineProjectionArtifact[],
  displayContext: 'thread' | 'worker',
  worker?: AgentId,
): TimelineProjectionRenderEntry[] {
  const flatEntries: LocalProjectionFlatRenderEntry[] = [];

  for (const artifact of artifacts) {
    const artifactVisible = displayContext === 'thread'
      ? artifact.threadVisible
      : Boolean(worker && artifact.workerTabs.includes(worker));
    if (!artifactVisible) {
      continue;
    }
    const artifactMetadata = resolveMessageMetadataRecord(artifact.message);
    flatEntries.push({
      entryId: `artifact:${artifact.artifactId}`,
      artifactId: artifact.artifactId,
      message: artifact.message,
      timestamp: artifact.timestamp,
      turnOrderSeq: resolveTimelineTurnOrderSeqFromMetadata(artifactMetadata),
      itemSeq: resolveTimelineItemSeqFromMetadata(artifactMetadata) || artifact.displayOrder,
      laneSeq: resolveTimelineLaneSeqFromMetadata(artifactMetadata),
      anchorEventSeq: artifact.anchorEventSeq,
      blockSeq: getMessageBlockSeq(artifact.message),
      cardStreamSeq: artifact.cardStreamSeq,
    });
  }

  return flatEntries
    .sort(compareRenderEntryOrder)
    .map((entry) => ({
      entryId: entry.entryId,
      artifactId: entry.artifactId,
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

function canonicalizeTimelineProjection(
  projection: SessionTimelineProjection,
): SessionTimelineProjection {
  const artifacts = ensureArray(projection.artifacts)
    .filter(isProjectionArtifact)
    .map((artifact) => {
      const message = normalizeProjectionRestoredMessage(artifact.message);
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
        dispatchWaveId: artifact.dispatchWaveId,
        laneId: artifact.laneId,
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
      } satisfies TimelineProjectionArtifact;
    })
    .sort(compareProjectionArtifactsSemanticOrder)
    .map((artifact) => ({
      ...artifact,
      displayOrder: artifact.displayOrder || 0,
    }));
  return {
    ...projection,
    sessionId: normalizeSessionId(projection.sessionId) || projection.sessionId,
    artifacts,
    threadRenderEntries: buildProjectionRenderEntriesFromArtifacts(artifacts, 'thread'),
    workerRenderEntries: buildDynamicWorkerRenderEntries(artifacts),
  };
}

function compareProjectionArtifactsSemanticOrder(
  left: TimelineProjectionArtifact,
  right: TimelineProjectionArtifact,
): number {
  const leftMetadata = resolveMessageMetadataRecord(left.message);
  const rightMetadata = resolveMessageMetadataRecord(right.message);
  const semanticOrder = compareTimelineSemanticOrder(
    {
      turnOrderSeq: resolveTimelineTurnOrderSeqFromMetadata(leftMetadata),
      itemSeq: resolveTimelineItemSeqFromMetadata(leftMetadata),
      laneSeq: resolveTimelineLaneSeqFromMetadata(leftMetadata),
      blockSeq: resolveTimelineBlockSeqFromMetadata(leftMetadata),
      displayOrder: left.displayOrder || 0,
    },
    {
      turnOrderSeq: resolveTimelineTurnOrderSeqFromMetadata(rightMetadata),
      itemSeq: resolveTimelineItemSeqFromMetadata(rightMetadata),
      laneSeq: resolveTimelineLaneSeqFromMetadata(rightMetadata),
      blockSeq: resolveTimelineBlockSeqFromMetadata(rightMetadata),
      displayOrder: right.displayOrder || 0,
    },
  );
  if (semanticOrder !== 0) {
    return semanticOrder;
  }
  return left.artifactId.localeCompare(right.artifactId);
}

function buildLiveTimelineProjection(
  sessionId: string,
  sourceNodes: TimelineNode[],
  seed: SessionTimelineProjection | null,
): SessionTimelineProjection {
  const normalizedNodes = sourceNodes
    .map((node) => normalizeTimelineNode(node))
    .sort(compareTimelineNodeOrder);

  const artifacts: TimelineProjectionArtifact[] = normalizedNodes.map((node, index) => ({
      artifactId: node.nodeId,
      kind: node.kind,
      displayOrder: typeof node.displayOrder === 'number' && Number.isFinite(node.displayOrder)
        ? Math.max(0, Math.floor(node.displayOrder))
        : index + 1,
      artifactVersion: node.artifactVersion,
      anchorEventSeq: node.anchorEventSeq,
      latestEventSeq: node.latestEventSeq,
      cardStreamSeq: node.cardStreamSeq,
      timestamp: node.timestamp,
      cardId: node.cardId,
      lifecycleKey: node.lifecycleKey,
      dispatchWaveId: node.dispatchWaveId,
      laneId: node.laneId,
      worker: node.worker,
      threadVisible: node.visibleInThread,
      workerTabs: normalizeWorkerTabList(node.workerTabs),
      messageIds: Array.from(new Set(node.messageIds)),
      message: normalizeIncomingMessage(node.message),
    }));

  const threadRenderEntries = buildProjectionRenderEntriesFromArtifacts(artifacts, 'thread');
  const workerRenderEntries = buildDynamicWorkerRenderEntries(artifacts);

  const lastAppliedEventSeq = artifacts.reduce(
    (maxSeq, artifact) => Math.max(maxSeq, artifact.latestEventSeq),
    seed?.lastAppliedEventSeq || 0,
  );

  const updatedAt = artifacts.reduce((maxTimestamp, artifact) => {
    const artifactUpdatedAt = Math.max(
      artifact.message.updatedAt || 0,
      artifact.message.timestamp || 0,
      artifact.timestamp,
    );
    return Math.max(maxTimestamp, artifactUpdatedAt);
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
  if (timelineProjectionSource === 'authoritative') {
    timelineProjectionDirty = false;
    if (normalizedSessionId) {
      upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(normalizedSessionId));
    }
    if (options.persist !== false) {
      scheduleSaveWebviewState();
    }
    return;
  }
  const projectionSessionId = normalizedSessionId
    || (messagesState.timelineNodes.length > 0 ? '__pending-session__' : '');
  if (!projectionSessionId) {
    return;
  }
  messagesState.timelineProjection = buildLiveTimelineProjection(
    projectionSessionId,
    messagesState.timelineNodes,
    messagesState.timelineProjection,
  );
  timelineProjectionSource = 'live';
  timelineProjectionDirty = false;
  if (normalizedSessionId) {
    upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(normalizedSessionId));
  }
  if (options.persist !== false) {
    scheduleSaveWebviewState();
  }
}

/**
 * 增量补丁投影：仅更新指定节点对应的 artifact，跳过全量 normalize + sort。
 * 前提：内容性更新不改变后端事件序列。
 */
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

/**
 * 增量更新路径：直接更新目标节点的消息内容，跳过全量 normalize-sort-reindex。
 *
 * 前提：后端事件序列保证内容性更新不改变排序位置。
 * 此函数仅更新节点消息，不触发 sortAndSyncTimelineNodes，
 * 通过 Svelte 5 $state 代理的数组索引赋值触发响应式更新。
 */
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
  visibility: { thread?: boolean; workerTabs?: AgentId[] },
  options: { replaceMessageId?: string; displayOrder?: number } = {},
): TimelineNode {
  const cardId = resolveTimelineCardId(nextMessage);
  const nextWorkerTabs = normalizeWorkerTabList(visibility.workerTabs);
  return normalizeTimelineNode({
    ...existingNode,
    displayOrder: existingNode.displayOrder ?? options.displayOrder,
    cardId: cardId || existingNode.cardId,
    worker: existingNode.worker || resolveTimelineWorker(nextMessage),
    visibleInThread: existingNode.visibleInThread || visibility.thread !== false,
    workerTabs: [
      ...existingNode.workerTabs,
      ...nextWorkerTabs,
    ],
    messageIds: Array.from(new Set([
      ...existingNode.messageIds,
      existingNode.nodeId,
      resolveTimelineNodeId(nextMessage),
      nextMessage.id,
      ...(options.replaceMessageId ? [options.replaceMessageId] : []),
    ])),
  });
}

export function upsertTimelineNode(
  message: Message,
  visibility: { thread?: boolean; workerTabs?: AgentId[] },
  options: { replaceMessageId?: string; displayOrder?: number } = {},
): Message {
  const normalizedMessage = normalizeIncomingMessage(message);

  const cardId = resolveTimelineCardId(normalizedMessage);
  const explicitNodeId = resolveTimelineNodeId(normalizedMessage);
  const replaceNodeId = options.replaceMessageId
    ? resolveTimelineAliasId(options.replaceMessageId)
    : undefined;
  const existingNodeId = timelineNodeIdByMessageId.get(explicitNodeId)
    || replaceNodeId
    || timelineNodeIdByMessageId.get(normalizedMessage.id)
    || (cardId ? timelineNodeIdByCardId.get(cardId) : undefined)
    || undefined;
  const stableNodeId = existingNodeId || explicitNodeId;
  const nextWorkerTabs = normalizeWorkerTabList(visibility.workerTabs);
  const existingNode = existingNodeId
    ? messagesState.timelineNodes.find((node) => node.nodeId === existingNodeId)
    : undefined;
  const existingNodeIsPlaceholder = existingNode?.message.metadata?.isPlaceholder === true;
  const stableMessage = preserveStableTurnOrderFact(existingNode?.message, {
    ...normalizedMessage,
    id: stableNodeId,
  });
  const nextAnchorEventSeq = getMessageEventSeq(stableMessage)
    ?? (existingNode?.anchorEventSeq || 0);
  const nextLatestEventSeq = getMessageEventSeq(stableMessage)
    ?? (existingNode?.latestEventSeq || nextAnchorEventSeq);
  const incomingCardStreamSeq = getMessageCardStreamSeq(stableMessage);
  const nextCardStreamSeq = existingNode && !existingNodeIsPlaceholder
    ? (existingNode.cardStreamSeq || incomingCardStreamSeq)
    : incomingCardStreamSeq;
  if (existingNode && compareIncomingMessageVersion(existingNode, stableMessage) < 0) {
    const aliasedNode = mergeTimelineNodeAliases(existingNode, stableMessage, visibility, options);
    mutateTimelineNodes((nodes) => nodes.map((node) => (
      node.nodeId === stableNodeId ? aliasedNode : node
    )));
    return aliasedNode.message;
  }
  const mergedMessage = stableMessage;
  const nextNode: TimelineNode = normalizeTimelineNode({
    nodeId: stableNodeId,
    kind: existingNode?.kind || resolveTimelineNodeKind(mergedMessage),
    displayOrder: existingNode?.displayOrder ?? options.displayOrder ?? (++timelineDisplayOrderCounter),
    laneOrder: existingNode?.laneOrder,
    artifactVersion: existingNode?.artifactVersion,
    anchorEventSeq: existingNode?.anchorEventSeq || nextAnchorEventSeq,
    latestEventSeq: Math.max(existingNode?.latestEventSeq || 0, nextLatestEventSeq),
    cardStreamSeq: nextCardStreamSeq,
    timestamp: mergeTimelineSortTimestamp(existingNode?.timestamp, mergedMessage),
    cardId: cardId || existingNode?.cardId,
    worker: resolveTimelineWorker(mergedMessage) || existingNode?.worker,
    visibleInThread: existingNode?.visibleInThread || visibility.thread !== false,
    workerTabs: [
      ...(existingNode?.workerTabs || []),
      ...nextWorkerTabs,
    ],
    messageIds: Array.from(new Set([
      ...(existingNode?.messageIds || []),
      stableNodeId,
      explicitNodeId,
      normalizedMessage.id,
      ...(options.replaceMessageId ? [options.replaceMessageId] : []),
    ])),
    message: mergedMessage,
  });

  mutateTimelineNodes((nodes) => {
    const index = nodes.findIndex((node) => node.nodeId === stableNodeId);
    const nextNodes = [...nodes];
    if (index >= 0) {
      nextNodes[index] = nextNode;
    } else {
      nextNodes.push(nextNode);
    }
    return nextNodes;
  });

  const updatedNode = messagesState.timelineNodes.find((node) => node.nodeId === stableNodeId);
  return updatedNode?.message || nextNode.message;
}
function findTimelineNodeByAlias(messageId: string): TimelineNode | undefined {
  const stableId = resolveTimelineAliasId(messageId);
  return messagesState.timelineNodes.find((node) => node.nodeId === stableId);
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
  const hasLocalPendingRequest = messagesState.pendingRequests.size > 0;
  if (!snapshot) {
    messagesState.backendProcessing = false;
    if (hasLocalPendingRequest) {
      updateProcessingState();
      return;
    }
    messagesState.pendingRequests = new Set();
    messagesState.activeMessageIds = new Set();
    if (!runtimeStateIndicatesProcessing() && !canonicalProjectionIndicatesProcessing()) {
      sealAllStreamingMessages();
    }
    updateProcessingState();
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
  if (!snapshot.isProcessing && pendingRequestIds.size === 0 && hasLocalPendingRequest) {
    messagesState.backendProcessing = false;
    if (snapshot.source) {
      setProcessingActor(snapshot.source, snapshot.agent || undefined);
    }
    updateProcessingState();
    return;
  }
  const activeMessageIds = snapshot.isProcessing
    ? messagesState.activeMessageIds
    : new Set<string>();
  const nextIsProcessing = snapshot.isProcessing
    || runtimeStateIndicatesProcessing()
    || canonicalProjectionIndicatesProcessing()
    || activeMessageIds.size > 0
    || pendingRequestIds.size > 0;

  messagesState.backendProcessing = snapshot.isProcessing;
  messagesState.pendingRequests = pendingRequestIds;
  messagesState.activeMessageIds = activeMessageIds;
  if (snapshot.source) {
    setProcessingActor(snapshot.source, snapshot.agent || undefined);
  }
  if (nextIsProcessing) {
    messagesState.thinkingStartAt = snapshot.startedAt
      || messagesState.thinkingStartAt
      || Date.now();
  } else {
    messagesState.thinkingStartAt = null;
    sealAllStreamingMessages();
  }
  messagesState.isProcessing = nextIsProcessing;
}

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

export function getCurrentSessionId() {
  return messagesState.currentSessionId;
}

export function getQueuedMessages() {
  return messagesState.queuedMessages;
}

export function getToasts() {
  return toasts;
}

export function getEnabledAgents() {
  return enabledAgents;
}

export function setEnabledAgents(agents: EnabledAgent[]) {
  enabledAgents = agents;
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
      return messageProjection.workers as AgentOutputs;
    },
    get sessions() { return messagesState.sessions; },
    get currentWorkspaceId() { return messagesState.currentWorkspaceId; },
    get currentWorkspacePath() { return messagesState.currentWorkspacePath; },
    get currentSessionId() { return messagesState.currentSessionId; },
    get sessionHistory() { return messagesState.sessionHistory; },
    get queuedMessages() { return messagesState.queuedMessages; },
    set queuedMessages(v) { setQueuedMessages(ensureArray<QueuedMessage>(v)); },
    get isProcessing() { return messagesState.isProcessing; },
    get thinkingStartAt() { return messagesState.thinkingStartAt; },
    get processingActor() { return messagesState.processingActor; },
    get appState() { return messagesState.appState; },
    get settingsBootstrapSnapshot() { return messagesState.settingsBootstrapSnapshot; },
    set settingsBootstrapSnapshot(v) { messagesState.settingsBootstrapSnapshot = v; },
    get settingsRegistrySnapshot() { return messagesState.settingsRegistrySnapshot; },
    set settingsRegistrySnapshot(v) { messagesState.settingsRegistrySnapshot = v; },
    get scrollPositions() { return messagesState.scrollPositions; },
    get autoScrollEnabled() { return messagesState.autoScrollEnabled; },
    get edits() { return edits; },
    set edits(v) { edits = v; },
    get orchestratorRuntimeState() { return messagesState.orchestratorRuntimeState; },
    set orchestratorRuntimeState(v) { setOrchestratorRuntimeState(v); },
    get toasts() { return toasts; },
    set toasts(v) { toasts = v; },
    get notifications() { return notifications; },
    get unreadNotificationCount() { return unreadNotificationCount; },
    get modelStatus() { return modelStatus; },
    set modelStatus(v) { modelStatus = v; },
    // 角色驱动 Tab 状态
    get enabledAgents() { return enabledAgents; },
    set enabledAgents(v) { enabledAgents = v; },
    // Wave 状态（提案 4.6）
    get waveState() { return waveState; },
    set waveState(v) { waveState = v; },
    // Worker 运行态（统一入口）
    get workerRuntime() { return workerRuntime; },
  };
}

export function allocateTurnOrderSeq(): number {
  localTimelineTurnOrderSeqCounter = Math.max(
    normalizePositiveSequence(localTimelineTurnOrderSeqCounter),
    maxTimelineTurnOrderSeqFromNodes(messagesState.timelineNodes),
    maxTimelineTurnOrderSeqFromArtifacts(messagesState.timelineProjection?.artifacts),
  );
  localTimelineTurnOrderSeqCounter += 1;
  return localTimelineTurnOrderSeqCounter;
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
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    return null;
  }
  return {
    sessionId: normalizedSessionId,
    scrollPositions: normalizePersistedScrollPositions(clonePersistablePayload(messagesState.scrollPositions)),
    scrollAnchors: normalizePersistedScrollAnchors(clonePersistablePayload(messagesState.scrollAnchors)),
    autoScrollEnabled: normalizePersistedAutoScrollConfig(clonePersistablePayload(messagesState.autoScrollEnabled)),
  };
}

function createSessionExecutionStateSnapshot(
  sessionId: string | null | undefined,
): PersistedSessionExecutionState | null {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    return null;
  }
  return {
    sessionId: normalizedSessionId,
    edits: clonePersistablePayload(edits) as PersistedSessionExecutionState['edits'],
    orchestratorRuntimeState: clonePersistablePayload(messagesState.orchestratorRuntimeState) as OrchestratorRuntimeState | null,
    pendingChanges: ensureArray(clonePersistablePayload(messagesState.appState?.pendingChanges)),
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
  ensureTimelineProjectionSnapshotCurrent(messagesState.currentSessionId);
  upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(messagesState.currentSessionId));
  const executionSnapshot = createSessionExecutionStateSnapshot(messagesState.currentSessionId);
  if (executionSnapshot) {
    sessionExecutionStateBySession = {
      ...sessionExecutionStateBySession,
      [executionSnapshot.sessionId]: executionSnapshot,
    };
  }
}

function getSessionViewState(sessionId: string | null | undefined): PersistedSessionViewState | null {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    return null;
  }
  return sessionViewStateBySession[normalizedSessionId] || null;
}

function restoreQueuedMessagesForSession(sessionId: string | null | undefined): void {
  const normalizedSessionId = normalizeSessionId(sessionId);
  messagesState.queuedMessages = normalizedSessionId
    ? normalizeQueuedMessageList(sessionQueuedMessagesBySession[normalizedSessionId])
    : [];
  if (!normalizedSessionId) {
    sessionQueuedMessagesBySession = {};
  }
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
  const nextExecutionEntries = Object.entries(sessionExecutionStateBySession)
    .filter(([sessionId]) => knownSessionIds.has(sessionId));
  if (nextExecutionEntries.length !== Object.keys(sessionExecutionStateBySession).length) {
    sessionExecutionStateBySession = Object.fromEntries(nextExecutionEntries);
  }
  const nextQueuedEntries = Object.entries(sessionQueuedMessagesBySession)
    .filter(([sessionId]) => knownSessionIds.has(sessionId) && sessionQueuedMessagesBySession[sessionId].length > 0);
  if (nextQueuedEntries.length !== Object.keys(sessionQueuedMessagesBySession).length) {
    sessionQueuedMessagesBySession = Object.fromEntries(nextQueuedEntries);
  }
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
  messagesState.scrollPositions = normalizePersistedScrollPositions(normalizedSnapshot.scrollPositions);
  messagesState.scrollAnchors = normalizePersistedScrollAnchors(normalizedSnapshot.scrollAnchors);
  messagesState.autoScrollEnabled = normalizePersistedAutoScrollConfig(normalizedSnapshot.autoScrollEnabled);
  const executionSnapshot = normalizedSessionId
    ? sessionExecutionStateBySession[normalizedSessionId] || null
    : null;
  if (executionSnapshot) {
    edits = clonePersistablePayload(executionSnapshot.edits) as PersistedSessionExecutionState['edits'];
    messagesState.orchestratorRuntimeState = clonePersistablePayload(executionSnapshot.orchestratorRuntimeState) as OrchestratorRuntimeState | null;
    if (messagesState.appState) {
      messagesState.appState = {
        ...messagesState.appState,
        pendingChanges: ensureArray(clonePersistablePayload(executionSnapshot.pendingChanges)),
      };
    }
  }
  return true;
}

function resetSessionScopedExecutionState(): void {
  edits = [];
  messagesState.taskComposerDraft = null;
  messagesState.orchestratorRuntimeState = null;
  if (messagesState.appState) {
    messagesState.appState = {
      ...messagesState.appState,
      pendingChanges: [],
    };
  }
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
    trimTimelineNodes();
    ensureTimelineProjectionSnapshotCurrent(messagesState.currentSessionId);
    captureCurrentSessionViewState();
    pruneSessionViewStateByKnownSessions();
    const state: WebviewPersistedState = {
      currentTopTab: messagesState.currentTopTab,
      currentBottomTab: messagesState.currentBottomTab,
      sessions: messagesState.sessions,
      currentSessionId: messagesState.currentSessionId,
      scrollPositions: messagesState.scrollPositions,
      scrollAnchors: messagesState.scrollAnchors,
      autoScrollEnabled: messagesState.autoScrollEnabled,
      sessionViewStateBySession,
      sessionQueuedMessagesBySession,
    };
    vscode.setState(state);
  } catch (error) {
    console.warn('[MessagesStore] Webview 状态持久化失败，已降级继续运行', error);
  }
}

// 非 hosted webview 环境（独立 web 客户端）注册 beforeunload 同步保存，
// 防止 900ms debounce 窗口内的刷新丢失数据。
if (!IS_HOSTED_WEBVIEW && typeof window !== 'undefined') {
  window.addEventListener('beforeunload', () => {
    if (deferredWebviewStateSaveTimer) {
      clearTimeout(deferredWebviewStateSaveTimer);
      deferredWebviewStateSaveTimer = null;
    }
    saveWebviewState();
  });
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
  if (runtimeStateIsTerminal(next)) {
    sealAllStreamingMessages();
  }
  updateProcessingState();
}

export function replaceOrchestratorRuntimeState(input: OrchestratorRuntimeState | null): void {
  const next = normalizeOrchestratorRuntimeState(input);
  messagesState.orchestratorRuntimeState = next;
  if (runtimeStateIsTerminal(next)) {
    sealAllStreamingMessages();
  }
  updateProcessingState();
}

export function updatePanelScrollState(
  panel: ScrollPanelKey,
  input: { scrollTop?: number; autoScrollEnabled?: boolean; anchor?: ScrollAnchor | null },
  options: { persist?: boolean } = {}
): void {
  let changed = false;
  const currentScrollTop = getCurrentPanelScrollTop(panel);
  const currentAutoScrollEnabled = getCurrentPanelAutoScrollEnabled(panel);
  const currentAnchor = getCurrentPanelScrollAnchor(panel);

  if (typeof input.scrollTop === 'number') {
    const nextScrollTop = normalizeScrollTop(input.scrollTop);
    if (currentScrollTop !== nextScrollTop) {
      messagesState.scrollPositions = {
        ...messagesState.scrollPositions,
        [panel]: nextScrollTop,
      };
      changed = true;
    }
  }

  if (typeof input.autoScrollEnabled === 'boolean' && currentAutoScrollEnabled !== input.autoScrollEnabled) {
    messagesState.autoScrollEnabled = {
      ...messagesState.autoScrollEnabled,
      [panel]: input.autoScrollEnabled,
    };
    changed = true;
  }

  if ('anchor' in input) {
    const nextAnchor = normalizeScrollAnchor(input.anchor);
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

export function setTaskComposerDraft(input: {
  intent: TaskComposerIntent;
  taskId?: string | null;
  text: string;
}): void {
  const normalizedText = typeof input.text === 'string' ? input.text.trim() : '';
  if (!normalizedText) return;
  messagesState.taskComposerDraft = {
    intent: input.intent,
    taskId: normalizeSessionId(input.taskId),
    text: normalizedText,
  };
}

export function clearTaskComposerDraft(): void {
  if (!messagesState.taskComposerDraft) return;
  messagesState.taskComposerDraft = null;
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
    messagesState.sessionHistory = {
      workspaceId: messagesState.currentWorkspaceId,
      sessionId: nextSessionId,
      hasMoreBefore: false,
      beforeCursor: null,
      isLoadingBefore: false,
    };
    resetNotificationCenterStatus();
  }
  if (hasChanged) {
    // 底部 worker 面板是“当前会话内的执行细节视图”，不能跨会话继承。
    // 否则用户从上一会话停留在 worker tab，新会话会直接落到 worker 面板，
    // 造成“主线/worker 边界混淆”的产品错觉。
    messagesState.currentBottomTab = 'thread';
    resetSessionScopedExecutionState();
    // 会话切换时消息内容以后端分页快照为唯一真相源。
    // 本地只恢复滚动/定位等轻量视图状态，避免旧 session 的主线或 worker 内容短暂残留。
    replaceTimelineNodes([]);
    messagesState.timelineProjection = null;
    timelineProjectionDirty = false;
    restoredSessionView = applySessionViewState(nextSessionId);
    if (!restoredSessionView) {
      resetPanelScrollRuntimeState();
    }
    restoreQueuedMessagesForSession(nextSessionId);
  }
  syncNotificationsFromSession(nextSessionId);
  saveWebviewState();
}

export function adoptCurrentSessionIdForLiveTurn(id: string | null | undefined): boolean {
  const nextSessionId = normalizeSessionId(id);
  if (!nextSessionId) {
    return false;
  }
  const currentSessionId = normalizeSessionId(messagesState.currentSessionId);
  if (currentSessionId === nextSessionId) {
    return true;
  }
  if (currentSessionId) {
    return false;
  }
  messagesState.currentSessionId = nextSessionId;
  messagesState.sessionHistory = {
    workspaceId: messagesState.currentWorkspaceId,
    sessionId: nextSessionId,
    hasMoreBefore: false,
    beforeCursor: null,
    isLoadingBefore: false,
  };
  resetNotificationCenterStatus();
  syncNotificationsFromSession(nextSessionId);
  saveWebviewState();
  return true;
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
  const normalized = normalizeQueuedMessageList(newQueuedMessages);
  messagesState.queuedMessages = normalized;
  const sessionId = normalizeSessionId(messagesState.currentSessionId);
  if (sessionId) {
    sessionQueuedMessagesBySession = {
      ...sessionQueuedMessagesBySession,
      ...(normalized.length > 0 ? { [sessionId]: normalized } : {}),
    };
    if (normalized.length === 0 && sessionQueuedMessagesBySession[sessionId]) {
      const { [sessionId]: _removed, ...rest } = sessionQueuedMessagesBySession;
      sessionQueuedMessagesBySession = rest;
    }
  }
  saveWebviewState();
}

export function enqueueQueuedMessage(message: QueuedMessage) {
  setQueuedMessages([
    ...messagesState.queuedMessages,
    message,
  ]);
}

export function dequeueQueuedMessage(): QueuedMessage | null {
  const [next, ...rest] = messagesState.queuedMessages;
  setQueuedMessages(rest);
  return next || null;
}

export function removeQueuedMessage(id: string) {
  const normalizedId = typeof id === 'string' ? id.trim() : '';
  if (!normalizedId) {
    return;
  }
  setQueuedMessages(messagesState.queuedMessages.filter((message) => message.id !== normalizedId));
}

function markQueuedUserEchoAsGuide(id: string): void {
  mutateTimelineNodes((nodes) => nodes.map((node) => {
    const message = node.message;
    if (message.type !== 'user_input') {
      return node;
    }
    const metadata = message.metadata && typeof message.metadata === 'object'
      ? message.metadata
      : {};
    const extra = metadata.extra && typeof metadata.extra === 'object' && !Array.isArray(metadata.extra)
      ? metadata.extra as Record<string, unknown>
      : null;
    if (!extra || extra.queued !== true || extra.queueMode === 'guide') {
      return node;
    }
    const requestId = typeof metadata.requestId === 'string' ? metadata.requestId.trim() : '';
    const queuedMessageId = typeof extra.queuedMessageId === 'string' ? extra.queuedMessageId.trim() : '';
    if (message.id !== id && requestId !== id && queuedMessageId !== id) {
      return node;
    }
    return {
      ...node,
      message: {
        ...message,
        metadata: {
          ...metadata,
          extra: {
            ...extra,
            queueMode: 'guide',
          },
        },
      },
    };
  }));
}

export function markQueuedMessageAsGuide(id: string): boolean {
  const normalizedId = typeof id === 'string' ? id.trim() : '';
  if (!normalizedId) {
    return false;
  }
  const index = messagesState.queuedMessages.findIndex((message) => (
    message.id === normalizedId || message.requestId === normalizedId
  ));
  if (index < 0) {
    return false;
  }
  const target = {
    ...messagesState.queuedMessages[index],
    mode: 'guide' as const,
  };
  setQueuedMessages([
    target,
    ...messagesState.queuedMessages.filter((_, itemIndex) => itemIndex !== index),
  ]);
  markQueuedUserEchoAsGuide(normalizedId);
  return true;
}

// 处理状态操作
export function setIsProcessing(value: boolean) {
  messagesState.backendProcessing = value;
  updateProcessingState();
}

export function setProcessingActor(source: string, agent?: string) {
  messagesState.processingActor = {
    source: source as ProcessingActor['source'],
    agent: (agent || source || 'orchestrator') as ProcessingActor['agent'],
  };
}

export function setAppState(nextState: AppState | null) {
  if (nextState) {
    messagesState.appState = nextState;
    messagesState.bootstrapped = true;
  } else {
    messagesState.appState = null;
  }
}

// setMissionPlan removed — old Mission/Assignment incremental handlers superseded by Task Graph.

// 防回抬冷却期（ms）：forced idle 后的短暂窗口内，拒绝任何来源的 processing=true
const ANTI_LIFT_BACK_COOLDOWN_MS = 2000;

// Worker 执行状态操作
function runtimeStateIndicatesProcessing(): boolean {
  const status = messagesState.orchestratorRuntimeState?.status;
  return status === 'running' || status === 'waiting' || status === 'paused';
}

function runtimeStateIsTerminal(runtimeState: OrchestratorRuntimeState | null): boolean {
  const status = runtimeState?.status;
  return status === 'idle'
    || status === 'completed'
    || status === 'failed'
    || status === 'cancelled';
}

function canonicalProjectionIndicatesProcessing(): boolean {
  const artifacts = messagesState.timelineProjection?.artifacts;
  if (!Array.isArray(artifacts) || artifacts.length === 0) {
    return false;
  }
  return artifacts.some((artifact) => {
    const metadata = artifact.message?.metadata;
    if (metadata?.canonical !== true) {
      return false;
    }
    const turnStatus = typeof metadata.turnStatus === 'string'
      ? metadata.turnStatus.trim()
      : '';
    const status = typeof metadata.turnItemStatus === 'string'
      ? metadata.turnItemStatus.trim()
      : '';
    if (turnStatus) {
      return turnStatus === 'pending' || turnStatus === 'running';
    }
    return status === 'pending' || status === 'running';
  });
}

function updateProcessingState() {
  const nextIsProcessing = messagesState.backendProcessing
    || runtimeStateIndicatesProcessing()
    || canonicalProjectionIndicatesProcessing()
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

function timelineHasStreamingMessage(): boolean {
  return messagesState.timelineNodes.some((node) => node.message?.isStreaming === true);
}

export function hasActiveLocalTimelineTurn(): boolean {
  return messagesState.isProcessing
    || messagesState.backendProcessing
    || runtimeStateIndicatesProcessing()
    || canonicalProjectionIndicatesProcessing()
    || messagesState.pendingRequests.size > 0
    || messagesState.activeMessageIds.size > 0
    || timelineHasStreamingMessage();
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
    if (
      next.size === 0
      && !messagesState.backendProcessing
      && !runtimeStateIndicatesProcessing()
      && messagesState.activeMessageIds.size === 0
      && !timelineHasStreamingMessage()
    ) {
      sealAllStreamingMessages();
    }
    updateProcessingState();
  }
}

export function settleProcessingAfterResponseCompletion() {
  if (
    messagesState.backendProcessing
    || canonicalProjectionIndicatesProcessing()
    || messagesState.pendingRequests.size > 0
    || messagesState.activeMessageIds.size > 0
  ) {
    return;
  }
  if (runtimeStateIndicatesProcessing()) {
    updateProcessingState();
    return;
  }
  messagesState.lastForcedIdleAt = Date.now();
  updateProcessingState();
}

export function settleAuthoritativeIdleState() {
  if (canonicalProjectionIndicatesProcessing()) {
    updateProcessingState();
    return;
  }
  messagesState.backendProcessing = false;
  messagesState.pendingRequests = new Set();
  messagesState.activeMessageIds = new Set();
  messagesState.thinkingStartAt = null;
  sealAllStreamingMessages();
  updateProcessingState();
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
  messagesState.orchestratorRuntimeState = null;
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
  clearPendingInteractions();
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

export function sealAllStreamingMessages() {
  let threadChanged = false;
  let agentChanged = false;

  // 处理单条消息：返回 null 表示应移除，返回新对象表示应更新
  const sealMessage = (m: Message): Message | null => {
    if (!m.isStreaming) return m; // 无需处理
    const hasVisibleContent = (typeof m.content === 'string' && m.content.trim().length > 0)
      || (Array.isArray(m.blocks) && m.blocks.length > 0);
    if (m.metadata?.isPlaceholder === true && !hasVisibleContent) {
      return null;
    }

    return {
      ...m,
      isStreaming: false,
      isComplete: true,
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

  // 注意：这里仅负责“封口流式消息”，不允许本地推断任务终态。
  // 任务/worker/mission 的 completed/failed/cancelled 必须只来自后端真相源，
  // 否则会把真实 completed 误写成 cancelled。
  if (threadChanged || agentChanged) {
    saveWebviewState();
  }
}

/** 获取后端处理状态（用于时序判断） */
export function getBackendProcessing(): boolean {
  return messagesState.backendProcessing;
}

export function clearPendingInteractions() {
  for (const binding of requestBindings.values()) {
    if (binding.timeoutId) {
      clearTimeout(binding.timeoutId);
    }
  }
  requestBindings = new Map();
  messagesState.pendingRequests = new Set();
}

function recomputeUnreadNotificationCount() {
  unreadNotificationCount = notifications.filter((n) => n.countUnread && !n.read).length;
}

function resolveNotificationSessionId(sessionId: string | null | undefined): string {
  const normalized = typeof sessionId === 'string' ? sessionId.trim() : '';
  return normalized;
}

function getCurrentNotificationSessionId(): string {
  return resolveNotificationSessionId(messagesState.currentSessionId);
}

function resolveNotificationWorkspaceId(workspaceId: string | null | undefined): string {
  return typeof workspaceId === 'string' ? workspaceId.trim() : '';
}

function getCurrentNotificationWorkspaceId(): string {
  return resolveNotificationWorkspaceId(messagesState.currentWorkspaceId);
}

function notificationScopeMatchesCurrentSession(
  sessionId: string,
  workspaceId: string | null | undefined,
): boolean {
  const normalizedSessionId = resolveNotificationSessionId(sessionId);
  if (!normalizedSessionId || normalizedSessionId !== getCurrentNotificationSessionId()) {
    return false;
  }
  return resolveNotificationWorkspaceId(workspaceId) === getCurrentNotificationWorkspaceId();
}

function createNotificationCenterIdleStatus(): NotificationCenterStatus {
  return {
    isLoading: false,
    operation: null,
    error: null,
    updatedAt: null,
  };
}

function resetNotificationCenterStatus(): void {
  messagesState.notificationCenter = createNotificationCenterIdleStatus();
}

function getCurrentNotificationOperationScope(): NotificationOperationScope | null {
  const sessionId = getCurrentNotificationSessionId();
  if (!sessionId) {
    return null;
  }
  return {
    sessionId,
    workspaceId: getCurrentNotificationWorkspaceId(),
    workspacePath: typeof messagesState.currentWorkspacePath === 'string'
      ? messagesState.currentWorkspacePath.trim()
      : '',
  };
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
  const scope = getCurrentNotificationOperationScope();
  if (!scope) {
    return;
  }
  vscode.postMessage({
    type: 'appendSessionNotification',
    ...scope,
    notification: {
      notificationId: id,
      kind: notificationCategory,
      level: type,
      title,
      message,
      source: policy.source,
      persistToCenter: true,
      actionRequired: policy.actionRequired,
      countUnread: policy.countUnread,
      displayMode: policy.displayMode,
      duration: policy.duration,
    },
  });
}

export function getNotifications() {
  return notifications;
}

export function getUnreadNotificationCount() {
  return unreadNotificationCount;
}

export function getNotificationCenterStatus(): NotificationCenterStatus {
  return messagesState.notificationCenter;
}

export function removeToast(id: string) {
  const normalizedId = typeof id === 'string' ? id.trim() : '';
  if (!normalizedId) {
    return;
  }
  toasts = toasts.filter((toast) => toast.id !== normalizedId);
}

export function loadSessionNotifications() {
  const scope = getCurrentNotificationOperationScope();
  if (!scope) {
    return;
  }
  vscode.postMessage({ type: 'loadSessionNotifications', ...scope });
}

export function markAllNotificationsRead() {
  const scope = getCurrentNotificationOperationScope();
  if (!scope) {
    return;
  }
  vscode.postMessage({ type: 'markAllNotificationsRead', ...scope });
}

export function clearAllNotifications() {
  const scope = getCurrentNotificationOperationScope();
  if (!scope) {
    return;
  }
  vscode.postMessage({ type: 'clearAllNotifications', ...scope });
}

export function removeNotification(id: string) {
  const normalizedId = typeof id === 'string' ? id.trim() : '';
  if (!normalizedId) {
    return;
  }
  const scope = getCurrentNotificationOperationScope();
  if (!scope) {
    return;
  }
  vscode.postMessage({ type: 'removeNotification', notificationId: normalizedId, ...scope });
}

export function applySessionNotifications(
  sessionId: string,
  rawNotifications: { records?: SessionNotificationRecord[] } | unknown,
  workspaceId?: string | null,
): void {
  const normalizedSessionId = resolveNotificationSessionId(sessionId);
  if (!notificationScopeMatchesCurrentSession(normalizedSessionId, workspaceId)) {
    return;
  }
  const normalized = normalizeSessionNotificationList(
    rawNotifications && typeof rawNotifications === 'object'
      ? (rawNotifications as { records?: unknown }).records
      : undefined,
  );
  replaceSessionNotificationList(normalizedSessionId, normalized);
  applyNotificationList(normalized);
}

export function applySessionNotificationsStatus(rawStatus: unknown): void {
  if (!rawStatus || typeof rawStatus !== 'object' || Array.isArray(rawStatus)) {
    return;
  }
  const status = rawStatus as Record<string, unknown>;
  const statusSessionId = resolveNotificationSessionId(
    typeof status.sessionId === 'string' ? status.sessionId : null,
  );
  const statusWorkspaceId = resolveNotificationWorkspaceId(
    typeof status.workspaceId === 'string' ? status.workspaceId : null,
  );
  if (!notificationScopeMatchesCurrentSession(statusSessionId, statusWorkspaceId)) {
    return;
  }
  const operation = typeof status.operation === 'string'
    && ['load', 'append', 'mark-read', 'clear', 'remove'].includes(status.operation)
    ? status.operation as NotificationCenterOperation
    : null;
  messagesState.notificationCenter = {
    isLoading: status.isLoading === true,
    operation,
    error: typeof status.error === 'string' && status.error.trim()
      ? status.error.trim()
      : null,
    updatedAt: typeof status.updatedAt === 'number' && Number.isFinite(status.updatedAt)
      ? status.updatedAt
      : Date.now(),
  };
}

export function getActiveInteractionType(): string | null {
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
  current: Pick<TimelineNode, 'latestEventSeq'>,
  incoming: Pick<Message, 'metadata'>,
): number {
  const currentEventSeq = typeof current.latestEventSeq === 'number' && Number.isFinite(current.latestEventSeq)
    ? Math.max(0, Math.floor(current.latestEventSeq))
    : 0;
  const currentVersion = resolveTimelineVersionFromMetadata({ eventSeq: currentEventSeq });
  const incomingVersion = resolveTimelineVersionFromMetadata(resolveMessageMetadataRecord(incoming));

  if (currentVersion > 0 && incomingVersion === 0) {
    return -1;
  }

  if (currentVersion > 0 && incomingVersion > 0 && incomingVersion !== currentVersion) {
    return incomingVersion > currentVersion ? 1 : -1;
  }

  if (currentVersion === 0 && incomingVersion > 0) {
    return 1;
  }

  return 0;
}

export function getTimelineMessageById(messageId: string): Message | undefined {
  return findTimelineNodeByAlias(messageId)?.message;
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
  if (options.resetTimelineView !== false) {
    replaceTimelineNodes([]);
    messagesState.timelineProjection = null;
    timelineProjectionDirty = false;
    timelineDisplayOrderCounter = 0;
    localTimelineTurnOrderSeqCounter = 0;
  }
  messagesState.orchestratorRuntimeState = null;
  messagesState.sessionHistory = {
    workspaceId: messagesState.currentWorkspaceId,
    sessionId: messagesState.currentSessionId,
    hasMoreBefore: false,
    beforeCursor: null,
    isLoadingBefore: false,
  };
  messagesState.queuedMessages = [];
  messagesState.messageJump = {
    messageId: null,
    nonce: messagesState.messageJump.nonce,
  };
  clearPendingInteractions();
  clearProcessingState({ skipAntiLiftBack: options.skipAntiLiftBack });
  // 会话级运行时状态：会话切换时必须清理，避免旧数据泄漏到新会话
  waveState = null;
  if (options.resetPanelState !== false) {
    resetPanelScrollRuntimeState();
  }
  if (options.persist !== false) {
    saveWebviewState();
  }
}

export function setSessionHistoryState(
  sessionId: string | null | undefined,
  input: {
    workspaceId?: string | null;
    hasMoreBefore?: boolean;
    beforeCursor?: string | null;
    isLoadingBefore?: boolean;
    preserveLoadedWindow?: boolean;
  },
): void {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    messagesState.sessionHistory = {
      workspaceId: null,
      sessionId: null,
      hasMoreBefore: false,
      beforeCursor: null,
      isLoadingBefore: false,
    };
    return;
  }
  const current = messagesState.sessionHistory;
  const normalizedWorkspaceId = typeof input.workspaceId === 'string'
    ? input.workspaceId.trim() || null
    : (messagesState.currentWorkspaceId || null);
  const inputBeforeCursor = typeof input.beforeCursor === 'string' && input.beforeCursor.trim()
    ? input.beforeCursor.trim()
    : null;
  if (
    (current.sessionId && current.sessionId !== normalizedSessionId)
    || (current.workspaceId && current.workspaceId !== normalizedWorkspaceId)
  ) {
    messagesState.sessionHistory = {
      workspaceId: normalizedWorkspaceId,
      sessionId: normalizedSessionId,
      hasMoreBefore: input.hasMoreBefore === true,
      beforeCursor: inputBeforeCursor,
      isLoadingBefore: input.isLoadingBefore === true,
    };
    return;
  }
  const shouldPreserveLoadedWindow = input.preserveLoadedWindow === true
    && current.sessionId === normalizedSessionId
    && current.workspaceId === normalizedWorkspaceId
    && (current.beforeCursor !== null || current.hasMoreBefore);
  messagesState.sessionHistory = {
    workspaceId: normalizedWorkspaceId,
    sessionId: normalizedSessionId,
    hasMoreBefore: shouldPreserveLoadedWindow
      ? current.hasMoreBefore
      : (input.hasMoreBefore ?? current.hasMoreBefore),
    beforeCursor: shouldPreserveLoadedWindow
      ? current.beforeCursor
      : (input.beforeCursor !== undefined ? inputBeforeCursor : current.beforeCursor),
    isLoadingBefore: input.isLoadingBefore ?? current.isLoadingBefore,
  };
}

function buildTimelineNodesFromProjection(projection: SessionTimelineProjection): TimelineNode[] {
  const canonicalProjection = canonicalizeTimelineProjection(projection);
  const validArtifacts = ensureArray(canonicalProjection.artifacts).filter(isProjectionArtifact);

  const nextNodes: TimelineNode[] = [];
  for (const artifact of validArtifacts) {

    const message = normalizeProjectionRestoredMessage(artifact.message);
    const worker = resolveTimelineWorker(message) || artifact.worker;

    nextNodes.push(normalizeTimelineNode({
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
      dispatchWaveId: artifact.dispatchWaveId,
      laneId: artifact.laneId,
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
        metadata: {
          ...(resolveMessageMetadataRecord(message) || {}),
          eventSeq: getMessageEventSeq(message)
            ?? (typeof artifact.latestEventSeq === 'number' && Number.isFinite(artifact.latestEventSeq)
              ? Math.max(0, Math.floor(artifact.latestEventSeq))
              : 0),
        },
      },
    }));
  }
  return nextNodes;
}

export function setTimelineProjection(projection: SessionTimelineProjection) {
  const canonicalProjection = canonicalizeTimelineProjection(projection);
  const nextNodes = buildTimelineNodesFromProjection(canonicalProjection);
  // 从后端恢复的 projection 中取最大 displayOrder 初始化计数器，确保后续新建节点序号不冲突
  timelineDisplayOrderCounter = canonicalProjection.artifacts.reduce(
    (max, a) => Math.max(max, a.displayOrder || 0),
    timelineDisplayOrderCounter,
  );
  localTimelineTurnOrderSeqCounter = Math.max(
    localTimelineTurnOrderSeqCounter,
    maxTimelineTurnOrderSeqFromArtifacts(canonicalProjection.artifacts),
  );
  messagesState.timelineProjection = canonicalProjection;
  timelineProjectionSource = 'authoritative';
  setTimelineProjectionNodes(nextNodes);
  timelineProjectionDirty = false;
  upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(canonicalProjection.sessionId));
  saveWebviewState();
}


// 导出状态初始化
export function initializeState() {
  clearAllRetryRuntime();
  resetPanelScrollRuntimeState();
  sessionViewStateBySession = {};
  sessionExecutionStateBySession = {};
  sessionQueuedMessagesBySession = {};
  timelineProjectionSource = 'none';
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
      clearProcessingState({ skipAntiLiftBack: true });
      saveWebviewState();
      return;
    }
    // Tab 状态不持久化，每次打开都默认显示主对话 tab
    messagesState.currentTopTab = 'thread';
    messagesState.currentBottomTab = 'thread';
    replaceTimelineNodes([]);
    sessionViewStateBySession = normalizePersistedSessionViewStateMap(persisted.sessionViewStateBySession);
    sessionQueuedMessagesBySession = normalizePersistedQueuedMessageMap(persisted.sessionQueuedMessagesBySession);
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
    restoreQueuedMessagesForSession(messagesState.currentSessionId);
    notificationsBySession = {};
    messagesState.orchestratorRuntimeState = null;
    syncNotificationsFromSession(messagesState.currentSessionId);

    // 启动恢复：消息内容只以后端 bootstrap 为唯一真相源。
    // 浏览器本地持久化只保留滚动/定位状态，不再恢复消息内容，
    // 避免 persisted projection 与 live/bootstrap 双轨竞争。
    clearPendingInteractions();
    clearProcessingState({ skipAntiLiftBack: true });
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

export function listRequestBindings(): RequestResponseBinding[] {
  return Array.from(requestBindings.values());
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
/**
 * 清除所有请求绑定（会话切换时使用）
 */
export function clearAllRequestBindings(): void {
  requestBindings = new Map();
}
