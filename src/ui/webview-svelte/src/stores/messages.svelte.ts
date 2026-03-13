/**
 * 消息状态管理 - Svelte 5 Runes
 * 使用细粒度响应式实现高效的流式更新
 */

import type {
  Message,
  AgentOutputs,
  AgentType,
  MissionPlan,
  Session,
  TabType,
  ProcessingActor,
  ContentBlock,
  ScrollPositions,
  ScrollAnchors,
  ScrollAnchor,
  AutoScrollConfig,
  AppState,
  WebviewPersistedState,
  WaveState,
  WorkerSessionState,
  RequestResponseBinding,
  RetryRuntimeState,
  ModelStatusMap,
  Task,
  QueuedMessage,
  WaitForWorkersResult,
} from '../types/message';
import { vscode } from '../lib/vscode-bridge';
import { ensureArray } from '../lib/utils';
import { i18n } from './i18n.svelte';
import { terminalSessions } from './terminal-sessions.svelte';
import { deriveWorkerRuntimeMap } from '../lib/worker-panel-state';

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
  threadMessages: [] as Message[],
  agentOutputs: {
    claude: [],
    codex: [],
    gemini: [],
  } as AgentOutputs,

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
  processingActor: {
    source: 'orchestrator',
    agent: 'claude',
  } as ProcessingActor,

  // 后端下发的完整状态
  appState: null as AppState | null,

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
const MAX_THREAD_MESSAGES = 500;
const MAX_AGENT_MESSAGES = 200;

const MAX_PERSISTED_ARRAY_LENGTH = 10000;
const WEBVIEW_STATE_SAVE_DEBOUNCE_MS = 120;

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

let deferredWebviewStateSaveTimer: ReturnType<typeof setTimeout> | null = null;

function scheduleSaveWebviewState(): void {
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

function isValidPersistedArray(value: unknown, max: number): value is unknown[] {
  if (!Array.isArray(value)) return false;
  const length = value.length;
  if (!Number.isFinite(length) || length < 0 || length > max) return false;
  return true;
}

function isValidMessageSource(message: Message | null | undefined): boolean {
  if (!message || typeof message !== 'object') return false;
  const source = (message as Message).source;
  return typeof source === 'string' && source.length > 0;
}

function hasInvalidMessageSource(messages: Message[]): boolean {
  return messages.some((msg) => !isValidMessageSource(msg));
}

// 新增状态：任务、变更、阶段、Toast、模型状态
let tasks = $state<Task[]>([]);
let edits = $state<Array<{ filePath: string; snapshotId?: string; type?: string; additions?: number; deletions?: number; contributors?: string[]; workerId?: string; missionId?: string }>>([]);
export type WorkerWaitResult = WaitForWorkersResult;
export type WorkerWaitResultMap = Record<string, WorkerWaitResult | null>;

let workerWaitResults = $state<WorkerWaitResultMap>({});

// 统一 Worker 运行态（唯一权威来源）
const workerRuntime = $derived.by(() => deriveWorkerRuntimeMap({
  messagesByWorker: messagesState.agentOutputs,
  pendingRequestIds: messagesState.pendingRequests,
  tasks,
}));

export type ToastCategory = 'incident' | 'audit' | 'feedback';
export type NotificationCategory = 'incident' | 'audit';

export interface ToastOptions {
  category?: ToastCategory;
  source?: string;
  actionRequired?: boolean;
  persistToCenter?: boolean;
  countUnread?: boolean;
}

interface ToastRecord {
  id: string;
  type: string;
  title?: string;
  message: string;
  category: ToastCategory;
  source?: string;
  actionRequired?: boolean;
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
const GLOBAL_NOTIFICATION_BUCKET = '__global__';
const MAX_NOTIFICATIONS_PER_SESSION = 200;
const NOTIFICATION_ARCHIVE_RUNTIME_SOURCES = new Set(['model-runtime', 'task-runtime', 'tool-runtime']);

let notifications = $state<Notification[]>([]);
let unreadNotificationCount = $state(0);
let notificationBuckets = $state<Record<string, Notification[]>>({});

let modelStatus = $state<ModelStatusMap>({
  claude: { status: 'checking' },
  codex: { status: 'checking' },
  gemini: { status: 'checking' },
  orchestrator: { status: 'checking' },
  auxiliary: { status: 'checking' },
});
let interactionMode = $state<'ask' | 'auto'>('auto');
let requestedInteractionMode = $state<'ask' | 'auto' | null>(null);
let interactionModeUpdatedAt = $state<number>(0);
const INTERACTION_MODE_SYNC_TIMEOUT_MS = 10000;
let interactionModeSyncTimer: ReturnType<typeof setTimeout> | null = null;

function sanitizeMessageBlocks(blocks: unknown): ContentBlock[] {
  const list = ensureArray(blocks);
  const invalid = list.filter(
    (block) => !block || typeof block !== 'object' || !('type' in (block as Record<string, unknown>))
  );
  if (invalid.length > 0) {
    throw new Error('[MessagesStore] 消息块无效');
  }
  return list as ContentBlock[];
}

function normalizePersistedMessages(messages: Message[] | undefined): Message[] {
  const seen = new Set<string>();
  const normalized: Message[] = [];
  for (const msg of ensureArray<Message>(messages)) {
    if (!msg || typeof msg !== 'object') {
      throw new Error('[MessagesStore] 持久化消息包含无效对象');
    }
    const id = typeof msg.id === 'string' && msg.id.trim().length > 0 ? msg.id.trim() : '';
    if (!id) {
      throw new Error('[MessagesStore] 持久化消息缺少 id');
    }
    if (seen.has(id)) {
      throw new Error(`[MessagesStore] 持久化消息 id 重复: ${id}`);
    }
    seen.add(id);
    const blocks = sanitizeMessageBlocks(msg.blocks);
    const metadata = msg.metadata && typeof msg.metadata === 'object'
      ? {
        ...msg.metadata,
        isPlaceholder: false,
        placeholderState: undefined,
      }
      : undefined;
    normalized.push({
      ...msg,
      id,
      blocks: blocks.length > 0 ? blocks : undefined,
      // Webview 重新进入时，持久化消息一律视为历史完成态，防止残留流式动画。
      isStreaming: false,
      isComplete: true,
      metadata,
    });
  }
  return normalized;
}

function normalizePersistedNotification(raw: unknown): Notification | null {
  if (!raw || typeof raw !== 'object') return null;
  const item = raw as Record<string, unknown>;
  const id = typeof item.id === 'string' ? item.id.trim() : '';
  if (!id) return null;
  const type = typeof item.type === 'string' ? item.type : 'info';
  const message = typeof item.message === 'string' ? item.message : '';
  if (!message) return null;
  const category = item.category === 'incident' ? 'incident' : item.category === 'audit' ? 'audit' : null;
  if (!category) return null;
  const timestamp = typeof item.timestamp === 'number' && Number.isFinite(item.timestamp)
    ? item.timestamp
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

function normalizeNotificationBuckets(raw: unknown): Record<string, Notification[]> {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
    return {};
  }
  const result: Record<string, Notification[]> = {};
  for (const [bucketId, list] of Object.entries(raw as Record<string, unknown>)) {
    if (typeof bucketId !== 'string' || bucketId.length === 0) {
      continue;
    }
    if (!isValidPersistedArray(list, MAX_PERSISTED_ARRAY_LENGTH)) {
      continue;
    }
    const seen = new Set<string>();
    const normalized: Notification[] = [];
    for (const item of list) {
      const next = normalizePersistedNotification(item);
      if (!next || seen.has(next.id)) {
        continue;
      }
      seen.add(next.id);
      normalized.push(next);
      if (normalized.length >= MAX_NOTIFICATIONS_PER_SESSION) {
        break;
      }
    }
    if (normalized.length > 0) {
      result[bucketId] = normalized;
    }
  }
  return result;
}

function normalizeIncomingMessage(message: Message): Message {
  if (!message || typeof message !== 'object') {
    throw new Error('[MessagesStore] 输入消息无效');
  }
  const id = typeof message.id === 'string' && message.id.trim().length > 0 ? message.id.trim() : '';
  if (!id) {
    throw new Error('[MessagesStore] 输入消息缺少 id');
  }
  const blocks = sanitizeMessageBlocks(message.blocks);
  return { ...message, id, blocks: blocks.length > 0 ? blocks : undefined };
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

// 交互请求状态
let pendingConfirmation = $state<{ plan: unknown; formattedPlan?: string; forceManual?: boolean } | null>(null);
let pendingRecovery = $state<{ taskId: string; error: unknown; canRetry: boolean; canRollback: boolean } | null>(null);
let pendingDeliveryRepair = $state<{ missionId: string; summary: string; details?: string; round: number; maxRounds: number } | null>(null);
let pendingClarification = $state<{ questions: string[]; context?: string; ambiguityScore?: number; originalPrompt?: string } | null>(null);
let pendingWorkerQuestion = $state<{ workerId: string; question: string; context?: string; options?: unknown } | null>(null);
let pendingToolAuthorization = $state<{ requestId: string; toolName: string; toolArgs: unknown } | null>(null);
let missionPlan = $state<Map<string, MissionPlan>>(new Map());

// Wave 执行状态（提案 4.6）
let waveState = $state<WaveState | null>(null);

// Worker Session 状态（提案 4.1）
let workerSessions = $state<Map<string, WorkerSessionState>>(new Map());

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
  return messagesState.threadMessages;
}

export function getAgentOutputs() {
  return messagesState.agentOutputs;
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

export function getToasts() {
  return toasts;
}

export function getModelStatus() {
  return modelStatus;
}

export function getInteractionMode() {
  return interactionMode;
}

export function getRequestedInteractionMode() {
  return requestedInteractionMode;
}

export function isInteractionModeSyncing() {
  return requestedInteractionMode !== null && requestedInteractionMode !== interactionMode;
}

export function getInteractionModeUpdatedAt() {
  return interactionModeUpdatedAt;
}

function clearInteractionModeSyncTimer() {
  if (interactionModeSyncTimer) {
    clearTimeout(interactionModeSyncTimer);
    interactionModeSyncTimer = null;
  }
}

function scheduleInteractionModeSyncTimeout(expectedMode: 'ask' | 'auto') {
  clearInteractionModeSyncTimer();
  interactionModeSyncTimer = setTimeout(() => {
    if (requestedInteractionMode === expectedMode) {
      requestedInteractionMode = null;
      addToast('warning', i18n.t('messageHandler.interactionModeSyncTimeout'), undefined, {
        category: 'audit',
        source: 'interaction-mode',
        countUnread: false,
      });
    }
    interactionModeSyncTimer = null;
  }, INTERACTION_MODE_SYNC_TIMEOUT_MS);
}

export function requestInteractionMode(mode: 'ask' | 'auto') {
  requestedInteractionMode = mode;
  scheduleInteractionModeSyncTimeout(mode);
}

export function clearRequestedInteractionMode() {
  requestedInteractionMode = null;
  clearInteractionModeSyncTimer();
}

export function setInteractionMode(mode: 'ask' | 'auto', updatedAt?: number) {
  const nextUpdatedAt = typeof updatedAt === 'number' ? updatedAt : Date.now();
  interactionMode = mode;
  interactionModeUpdatedAt = nextUpdatedAt;
  if (messagesState.appState) {
    messagesState.appState = {
      ...messagesState.appState,
      interactionMode: mode,
      interactionModeUpdatedAt: nextUpdatedAt,
    };
  }
  if (requestedInteractionMode === mode) {
    requestedInteractionMode = null;
    clearInteractionModeSyncTimer();
  }
}

export function getPendingConfirmation() {
  return pendingConfirmation;
}

export function getPendingRecovery() {
  return pendingRecovery;
}

export function getPendingClarification() {
  return pendingClarification;
}

export function getPendingWorkerQuestion() {
  return pendingWorkerQuestion;
}

export function getPendingToolAuthorization() {
  return pendingToolAuthorization;
}

export function getMissionPlan(): Map<string, MissionPlan> {
  return missionPlan;
}

export function getWaveState() {
  return waveState;
}

export function getWorkerSessions() {
  return workerSessions;
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
      merged[key] = shouldPreserveCompletedAt
        ? { ...payload, updatedAt: currentAt }
        : payload;
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
    get threadMessages() { return messagesState.threadMessages; },
    get agentOutputs() { return messagesState.agentOutputs; },
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
    get toasts() { return toasts; },
    set toasts(v) { toasts = v; },
    get notifications() { return notifications; },
    get unreadNotificationCount() { return unreadNotificationCount; },
    get modelStatus() { return modelStatus; },
    set modelStatus(v) { modelStatus = v; },
    get interactionMode() { return interactionMode; },
    set interactionMode(v) {
      const nextMode = v === 'ask' ? 'ask' : 'auto';
      setInteractionMode(nextMode);
    },
    get pendingConfirmation() { return pendingConfirmation; },
    set pendingConfirmation(v) { pendingConfirmation = v; },
    get pendingRecovery() { return pendingRecovery; },
    set pendingRecovery(v) { pendingRecovery = v; },
    get pendingDeliveryRepair() { return pendingDeliveryRepair; },
    set pendingDeliveryRepair(v) { pendingDeliveryRepair = v; },
    get pendingClarification() { return pendingClarification; },
    set pendingClarification(v) { pendingClarification = v; },
    get pendingWorkerQuestion() { return pendingWorkerQuestion; },
    set pendingWorkerQuestion(v) { pendingWorkerQuestion = v; },
    get pendingToolAuthorization() { return pendingToolAuthorization; },
    set pendingToolAuthorization(v) { pendingToolAuthorization = v; },
    get missionPlan() { return missionPlan; },
    set missionPlan(v) { missionPlan = v; },
    // Wave 状态（提案 4.6）
    get waveState() { return waveState; },
    set waveState(v) { waveState = v; },
    // Worker Session 状态（提案 4.1）
    get workerSessions() { return workerSessions; },
    set workerSessions(v) { workerSessions = v; },
    // Worker 运行态（统一入口）
    get workerRuntime() { return workerRuntime; },
  };
}

// ============ 状态更新函数 ============

// 裁剪消息列表
function trimMessageLists() {
  if (messagesState.threadMessages.length > MAX_THREAD_MESSAGES) {
    messagesState.threadMessages = messagesState.threadMessages.slice(-MAX_THREAD_MESSAGES);
  }
  (['claude', 'codex', 'gemini'] as const).forEach((agent) => {
    if (messagesState.agentOutputs[agent].length > MAX_AGENT_MESSAGES) {
      messagesState.agentOutputs = {
        ...messagesState.agentOutputs,
        [agent]: messagesState.agentOutputs[agent].slice(-MAX_AGENT_MESSAGES),
      };
    }
  });
}

// 保存状态到 VS Code
function saveWebviewState() {
  if (deferredWebviewStateSaveTimer) {
    clearTimeout(deferredWebviewStateSaveTimer);
    deferredWebviewStateSaveTimer = null;
  }
  trimMessageLists();
  const state: WebviewPersistedState = {
    currentTopTab: messagesState.currentTopTab,
    currentBottomTab: messagesState.currentBottomTab,
    threadMessages: messagesState.threadMessages,
    agentOutputs: messagesState.agentOutputs,
    sessions: messagesState.sessions,
    currentSessionId: messagesState.currentSessionId,
    scrollPositions: messagesState.scrollPositions,
    scrollAnchors: messagesState.scrollAnchors,
    autoScrollEnabled: messagesState.autoScrollEnabled,
    notificationBuckets,
  };
  vscode.setState(state);
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
  messagesState.currentSessionId = id;
  syncNotificationsFromBucket(id);
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

// Worker 执行状态操作
function updateProcessingState() {
  const nextIsProcessing = messagesState.backendProcessing
    || messagesState.activeMessageIds.size > 0
    || messagesState.pendingRequests.size > 0;

  if (nextIsProcessing && !messagesState.isProcessing) {
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

export function clearProcessingState() {
  messagesState.backendProcessing = false;
  messagesState.activeMessageIds = new Set();
  messagesState.pendingRequests = new Set();
  clearAllRetryRuntime();
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
function sealRunningLaneTasks() {
  const agents: AgentType[] = ['claude', 'codex', 'gemini'];
  for (const agent of agents) {
    const list = messagesState.agentOutputs[agent];
    let changed = false;
    const sealed: Message[] = [];
    for (const m of list) {
      if (m.type !== 'instruction' || !Array.isArray(m.metadata?.laneTasks)) {
        sealed.push(m);
        continue;
      }
      const laneTasks = m.metadata!.laneTasks as Array<Record<string, unknown>>;
      const hasRunning = laneTasks.some(t => t.status === 'running');
      if (!hasRunning) {
        sealed.push(m);
        continue;
      }
      changed = true;
      sealed.push({
        ...m,
        metadata: {
          ...m.metadata,
          laneTasks: laneTasks.map(t =>
            t.status === 'running' ? { ...t, status: 'cancelled' } : t,
          ),
        },
      });
    }
    if (changed) {
      messagesState.agentOutputs = { ...messagesState.agentOutputs, [agent]: sealed };
    }
  }
}

// 终结 mission plan 中残留的 running assignment/todo
function sealRunningMissionAssignments() {
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
}

export function sealAllStreamingMessages() {
  let threadChanged = false;
  let agentChanged = false;

  // 判断消息是否有可渲染内容
  const hasContent = (m: Message): boolean => {
    if (m.content && m.content.trim().length > 0) return true;
    if (m.blocks && m.blocks.length > 0) {
      return m.blocks.some(b => {
        if (!b || typeof b !== 'object') return false;
        if ('content' in b && typeof b.content === 'string' && b.content.trim().length > 0) return true;
        if (b.type === 'tool_call') return true;
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

  // 处理 threadMessages
  const sealedThread: Message[] = [];
  for (const m of messagesState.threadMessages) {
    const result = sealMessage(m);
    if (result === null) {
      threadChanged = true; // 消息被移除
    } else if (result !== m) {
      sealedThread.push(result);
      threadChanged = true; // 消息被更新
    } else {
      sealedThread.push(m);
    }
  }
  if (threadChanged) {
    messagesState.threadMessages = sealedThread;
  }

  // 处理 agentOutputs
  const agents: AgentType[] = ['claude', 'codex', 'gemini'];
  for (const agent of agents) {
    const list = messagesState.agentOutputs[agent];
    const sealedList: Message[] = [];
    let changed = false;
    for (const m of list) {
      const result = sealMessage(m);
      if (result === null) {
        changed = true;
      } else if (result !== m) {
        sealedList.push(result);
        changed = true;
      } else {
        sealedList.push(m);
      }
    }
    if (changed) {
      messagesState.agentOutputs = { ...messagesState.agentOutputs, [agent]: sealedList };
      agentChanged = true;
    }
  }

  if (threadChanged || agentChanged) {
    saveWebviewState();
  }

  // 终结 instruction 消息中残留的 running lane tasks 和 mission plan 中 running 的 assignment，
  // 避免插件重启/强制停止后 Worker 圆点持续显示"执行中"动画。
  sealRunningLaneTasks();
  sealRunningMissionAssignments();
}

/** 获取后端处理状态（用于时序判断） */
export function getBackendProcessing(): boolean {
  return messagesState.backendProcessing;
}

export function clearPendingInteractions() {
  pendingConfirmation = null;
  pendingRecovery = null;
  pendingClarification = null;
  pendingWorkerQuestion = null;
  pendingToolAuthorization = null;
}

function recomputeUnreadNotificationCount() {
  unreadNotificationCount = notifications.filter((n) => !n.read).length;
}

function resolveNotificationBucketId(sessionId: string | null | undefined): string {
  const normalized = typeof sessionId === 'string' ? sessionId.trim() : '';
  return normalized || GLOBAL_NOTIFICATION_BUCKET;
}

function getCurrentNotificationBucketId(): string {
  return resolveNotificationBucketId(messagesState.currentSessionId);
}

function applyNotificationList(nextList: Notification[]): Notification[] {
  const trimmed = nextList.slice(0, MAX_NOTIFICATIONS_PER_SESSION);
  notifications = trimmed;
  recomputeUnreadNotificationCount();
  return trimmed;
}

function syncNotificationsFromBucket(sessionId: string | null | undefined): void {
  const bucketId = resolveNotificationBucketId(sessionId);
  const list = ensureArray<Notification>(notificationBuckets[bucketId]);
  applyNotificationList(list);
}

function updateCurrentBucketNotifications(updater: (current: Notification[]) => Notification[]): void {
  const bucketId = getCurrentNotificationBucketId();
  const current = ensureArray<Notification>(notificationBuckets[bucketId]);
  const next = applyNotificationList(updater(current));
  notificationBuckets = {
    ...notificationBuckets,
    [bucketId]: next,
  };
  saveWebviewState();
}

function shouldArchiveToastToNotificationCenter(policy: {
  category: ToastCategory;
  persistToCenter: boolean;
  source?: string;
}): boolean {
  if (!policy.persistToCenter || policy.category === 'feedback') {
    return false;
  }
  if (!policy.source || !NOTIFICATION_ARCHIVE_RUNTIME_SOURCES.has(policy.source)) {
    return false;
  }
  return true;
}

function resolveToastPolicy(options?: ToastOptions): {
  category: ToastCategory;
  persistToCenter: boolean;
  countUnread: boolean;
  source?: string;
  actionRequired?: boolean;
} {
  const category = options?.category ?? 'feedback';
  const defaultPersistToCenter = category !== 'feedback';
  const persistToCenter = options?.persistToCenter ?? defaultPersistToCenter;
  const defaultCountUnread = category === 'incident';
  const countUnread = persistToCenter ? (options?.countUnread ?? defaultCountUnread) : false;
  const actionRequired = options?.actionRequired ?? (category === 'incident');
  return {
    category,
    persistToCenter,
    countUnread,
    source: options?.source,
    actionRequired,
  };
}

export function addToast(type: string, message: string, title?: string, options?: ToastOptions) {
  const policy = resolveToastPolicy(options);
  const id = `toast_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`;
  const toast: ToastRecord = {
    id,
    type,
    title,
    message,
    category: policy.category,
    source: policy.source,
    actionRequired: policy.actionRequired,
  };
  toasts = [...toasts, toast];

  if (!shouldArchiveToastToNotificationCenter(policy)) {
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
  updateCurrentBucketNotifications((current) => [notification, ...current]);
}

export function getNotifications() {
  return notifications;
}

export function getUnreadNotificationCount() {
  return unreadNotificationCount;
}

export function markAllNotificationsRead() {
  updateCurrentBucketNotifications((current) => current.map((n) => ({ ...n, read: true })));
}

export function clearAllNotifications() {
  updateCurrentBucketNotifications(() => []);
}

export function removeNotification(id: string) {
  updateCurrentBucketNotifications((current) => current.filter((n) => n.id !== id));
}

export function getActiveInteractionType(): string | null {
  if (pendingRecovery) return 'recovery';
  if (pendingConfirmation) return 'confirmation';
  if (pendingToolAuthorization) return 'toolAuthorization';
  if (pendingClarification) return 'clarification';
  if (pendingWorkerQuestion) return 'workerQuestion';
  return null;
}
// 消息操作
export function addThreadMessage(message: Message) {
  const safeMessage = JSON.parse(JSON.stringify(normalizeIncomingMessage(message))) as Message;

  if (messagesState.threadMessages.some((m) => m.id === safeMessage.id)) {
    return;
  }
  messagesState.threadMessages = [...messagesState.threadMessages, safeMessage];
  
  saveWebviewState();
}

export function updateThreadMessage(messageId: string, updates: Partial<Message>) {
  const index = messagesState.threadMessages.findIndex((m) => m.id === messageId);
  if (index !== -1) {
    // 必须完全重建数组，不能直接修改索引
    // 使用 JSON 序列化确保脱离 Svelte 5 $state 响应式代理
    // structuredClone 无法克隆 Proxy 对象，改用 JSON 往返
    const normalizedUpdates: Partial<Message> = JSON.parse(JSON.stringify(updates));
    if ('blocks' in normalizedUpdates) {
      const blocks = sanitizeMessageBlocks(normalizedUpdates.blocks);
      normalizedUpdates.blocks = blocks.length > 0 ? blocks : undefined;
    }
    const newMessages = messagesState.threadMessages.map((msg, i) => {
      if (i === index) {
        return { ...msg, ...normalizedUpdates };
      }
      return msg;
    });
    messagesState.threadMessages = newMessages;
    // 不触发保存，由流式管理器批量保存
  }
}

export function replaceThreadMessage(oldMessageId: string, message: Message) {
  const safeMessage = JSON.parse(JSON.stringify(normalizeIncomingMessage(message))) as Message;

  const index = messagesState.threadMessages.findIndex((m) => m.id === oldMessageId);
  if (index === -1) {
    addThreadMessage(safeMessage);
    return;
  }
  if (messagesState.threadMessages.some((m, i) => m.id === safeMessage.id && i !== index)) {
    return;
  }
  const next = [...messagesState.threadMessages];
  next[index] = safeMessage;
  messagesState.threadMessages = next;
  
  saveWebviewState();
}

export function removeThreadMessage(messageId: string) {
  if (!messagesState.threadMessages.length) return;
  messagesState.threadMessages = messagesState.threadMessages.filter((m) => m.id !== messageId);
  saveWebviewState();
}

export function clearThreadMessages() {
  messagesState.threadMessages = [];
  saveWebviewState();
}

export function addAgentMessage(agent: AgentType, message: Message) {
  const safeMessage = JSON.parse(JSON.stringify(normalizeIncomingMessage(message))) as Message;
  if (messagesState.agentOutputs[agent].some((m) => m.id === safeMessage.id)) {
    console.warn(`[MessagesStore] 跳过重复的 agent message: ${safeMessage.id}`);
    return;
  }

  // 🔧 调试日志：追踪 Worker 消息添加
  console.log('[DEBUG] addAgentMessage:', {
    agent,
    messageId: safeMessage.id,
    contentPreview: safeMessage.content?.substring(0, 100),
    currentCount: messagesState.agentOutputs[agent]?.length || 0,
  });

  messagesState.agentOutputs = {
    ...messagesState.agentOutputs,
    [agent]: [...messagesState.agentOutputs[agent], safeMessage],
  };

  console.log('[DEBUG] addAgentMessage 完成:', {
    agent,
    newCount: messagesState.agentOutputs[agent]?.length || 0,
  });

  saveWebviewState();
}

export function updateAgentMessage(agent: AgentType, messageId: string, updates: Partial<Message>) {
  const list = messagesState.agentOutputs[agent];
  const index = list.findIndex((m) => m.id === messageId);
  if (index !== -1) {
    // 使用 JSON 序列化确保脱离 Svelte 5 $state 响应式代理
    const normalizedUpdates: Partial<Message> = JSON.parse(JSON.stringify(updates));
    if ('blocks' in normalizedUpdates) {
      const blocks = sanitizeMessageBlocks(normalizedUpdates.blocks);
      normalizedUpdates.blocks = blocks.length > 0 ? blocks : undefined;
    }
    const next = list.map((msg, i) => (i === index ? { ...msg, ...normalizedUpdates } : msg));
    messagesState.agentOutputs = { ...messagesState.agentOutputs, [agent]: next };
    // 不触发保存，由流式管理器批量保存
  }
}

export function replaceAgentMessage(agent: AgentType, oldMessageId: string, message: Message) {
  const safeMessage = JSON.parse(JSON.stringify(normalizeIncomingMessage(message))) as Message;
  const index = messagesState.agentOutputs[agent].findIndex((m) => m.id === oldMessageId);
  if (index === -1) {
    addAgentMessage(agent, safeMessage);
    return;
  }
  if (messagesState.agentOutputs[agent].some((m, i) => m.id === safeMessage.id && i !== index)) {
    console.warn(`[MessagesStore] 替换 agent message id 冲突: ${safeMessage.id}`);
    return;
  }
  const next = [...messagesState.agentOutputs[agent]];
  next[index] = safeMessage;
  messagesState.agentOutputs = { ...messagesState.agentOutputs, [agent]: next };
  saveWebviewState();
}

export function removeAgentMessage(agent: AgentType, messageId: string) {
  const list = messagesState.agentOutputs[agent];
  if (!list.length) return;
  const next = list.filter((m) => m.id !== messageId);
  if (next.length === list.length) return;
  messagesState.agentOutputs = { ...messagesState.agentOutputs, [agent]: next };
  saveWebviewState();
}

export function clearAgentMessages(agent: AgentType) {
  messagesState.agentOutputs = { ...messagesState.agentOutputs, [agent]: [] };
  saveWebviewState();
}

export function clearAgentOutputs() {
  messagesState.agentOutputs = {
    claude: [],
    codex: [],
    gemini: [],
  };
  saveWebviewState();
}

// 清空所有消息（用于会话切换/新建）
export function clearAllMessages() {
  messagesState.threadMessages = [];
  messagesState.agentOutputs = {
    claude: [],
    codex: [],
    gemini: [],
  };
  workerWaitResults = {};
  messagesState.queuedMessages = [];
  messagesState.messageJump = {
    messageId: null,
    nonce: messagesState.messageJump.nonce,
  };
  clearPendingInteractions();
  clearProcessingState();
  terminalSessions.clear();
  saveWebviewState();
}

// 设置完整的消息列表（用于会话切换时加载历史）
export function setThreadMessages(messages: Message[]) {
  messagesState.threadMessages = normalizePersistedMessages(messages).map(m => JSON.parse(JSON.stringify(m)) as Message);
  workerWaitResults = {};
  terminalSessions.clear();
  saveWebviewState();
}

// 设置完整的 agent 消息列表（用于会话切换时加载历史）
export function setAgentOutputs(outputs: AgentOutputs) {
  messagesState.agentOutputs = {
    claude: normalizePersistedMessages(outputs.claude).map(m => JSON.parse(JSON.stringify(m)) as Message),
    codex: normalizePersistedMessages(outputs.codex).map(m => JSON.parse(JSON.stringify(m)) as Message),
    gemini: normalizePersistedMessages(outputs.gemini).map(m => JSON.parse(JSON.stringify(m)) as Message),
  };
  saveWebviewState();
}

// 导出状态初始化
export function initializeState() {
  clearAllRetryRuntime();
  const persisted = vscode.getState<WebviewPersistedState>();
  if (persisted) {
    const validThread = isValidPersistedArray(persisted.threadMessages, MAX_PERSISTED_ARRAY_LENGTH);
    const validClaude = isValidPersistedArray(persisted.agentOutputs?.claude, MAX_PERSISTED_ARRAY_LENGTH);
    const validCodex = isValidPersistedArray(persisted.agentOutputs?.codex, MAX_PERSISTED_ARRAY_LENGTH);
    const validGemini = isValidPersistedArray(persisted.agentOutputs?.gemini, MAX_PERSISTED_ARRAY_LENGTH);
    const validSessions = isValidPersistedArray(persisted.sessions, MAX_PERSISTED_ARRAY_LENGTH);
    if (!validThread || !validClaude || !validCodex || !validGemini || !validSessions) {
      throw new Error('[MessagesStore] 持久化数据结构无效');
    }
    // Tab 状态不持久化，每次打开都默认显示主对话 tab
    messagesState.currentTopTab = 'thread';
    messagesState.currentBottomTab = 'thread';
    messagesState.threadMessages = normalizePersistedMessages(persisted.threadMessages);
    messagesState.agentOutputs = {
      claude: normalizePersistedMessages(persisted.agentOutputs?.claude),
      codex: normalizePersistedMessages(persisted.agentOutputs?.codex),
      gemini: normalizePersistedMessages(persisted.agentOutputs?.gemini),
    };
    if (
      hasInvalidMessageSource(messagesState.threadMessages) ||
      hasInvalidMessageSource(messagesState.agentOutputs.claude) ||
      hasInvalidMessageSource(messagesState.agentOutputs.codex) ||
      hasInvalidMessageSource(messagesState.agentOutputs.gemini)
    ) {
      throw new Error('[MessagesStore] 持久化消息来源无效');
    }
    const sessionSeen = new Set<string>();
    messagesState.sessions = ensureArray<Session>(persisted.sessions)
      .filter((session) => !!session && typeof session.id === 'string' && session.id.trim().length > 0)
      .filter((session) => {
        if (sessionSeen.has(session.id)) return false;
        sessionSeen.add(session.id);
        return true;
      });
    messagesState.currentSessionId = persisted.currentSessionId || null;
    messagesState.scrollPositions = persisted.scrollPositions || { thread: 0, claude: 0, codex: 0, gemini: 0 };
    messagesState.scrollAnchors = persisted.scrollAnchors ? {
      thread: normalizeScrollAnchor(persisted.scrollAnchors.thread),
      claude: normalizeScrollAnchor(persisted.scrollAnchors.claude),
      codex: normalizeScrollAnchor(persisted.scrollAnchors.codex),
      gemini: normalizeScrollAnchor(persisted.scrollAnchors.gemini),
    } : createDefaultScrollAnchors();
    messagesState.autoScrollEnabled = persisted.autoScrollEnabled || { thread: true, claude: true, codex: true, gemini: true };
    notificationBuckets = normalizeNotificationBuckets(persisted.notificationBuckets);
    syncNotificationsFromBucket(messagesState.currentSessionId);

    // 启动恢复：持久化状态只保留历史展示，不继承运行态。
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

// ============ Worker Session 状态操作（提案 4.1） ============

export function addWorkerSession(session: WorkerSessionState) {
  const newSessions = new Map(workerSessions);
  newSessions.set(session.sessionId, session);
  workerSessions = newSessions;
}

export function updateWorkerSession(sessionId: string, updates: Partial<WorkerSessionState>) {
  const existing = workerSessions.get(sessionId);
  if (existing) {
    const newSessions = new Map(workerSessions);
    newSessions.set(sessionId, { ...existing, ...updates });
    workerSessions = newSessions;
  }
}

export function removeWorkerSession(sessionId: string) {
  const newSessions = new Map(workerSessions);
  newSessions.delete(sessionId);
  workerSessions = newSessions;
}

export function clearWorkerSessions() {
  workerSessions = new Map();
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
