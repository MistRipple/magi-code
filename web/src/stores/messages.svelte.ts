/**
 * 消息状态管理 - Svelte 5 Runes
 * 使用细粒度响应式实现高效的流式更新
 */

import type {
  Message,
  TimelineRenderItem,
  TimelineProjectionArtifact,
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
  Edit,
  ChangeMutationStatus,
} from '../types/message';
import { vscode } from '../lib/vscode-bridge';
import { ensureArray } from '../lib/utils';
import {
  normalizeIncidentRecords,
  shouldDisplayToast,
  type NormalizedIncidentRecord,
} from '../lib/notification-policy';

import {
  buildTimelinePanelMessages,
} from '../lib/timeline-render-items';
import {
  clearCanonicalSessionTurns,
  prependCanonicalSessionTurns as prependCanonicalTurns,
} from './turn-store.svelte';
import type { SettingsBootstrapSnapshot } from '../shared/settings-bootstrap';
import type { RoleTemplate } from '../shared/types/role-templates';
import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';
import { shouldUseHostProxyTransport } from '../shared/transport';
import { hasPendingToolApproval, toolApprovalState } from './tool-approval-store.svelte';
import {
  isCanonicalTerminalStatus,
  type CanonicalTurn,
} from '../shared/protocol/canonical-turn';
import { canonicalTurnRequestId } from '../shared/protocol/canonical-processing';
import type { TurnStage } from '../shared/protocol/processing-state';

interface SettingsRegistrySnapshot {
  roleTemplates: RoleTemplate[];
  registryEngines: ModelEngine[];
  registryAgents: AgentBinding[];
}

type NotificationCenterOperation = 'load' | 'report' | 'mark-read' | 'clear' | 'resolve' | 'remove';

export interface NotificationCenterStatus {
  isLoading: boolean;
  operation: NotificationCenterOperation | null;
  error: string | null;
  updatedAt: number | null;
}

interface NotificationOperationScope {
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
}

export type SessionHistoryLoadStatus = 'idle' | 'loading' | 'exhausted' | 'error' | 'no-progress';

export interface SessionHistoryState {
  workspaceId: string | null;
  workspacePath: string | null;
  sessionId: string | null;
  canonicalHasMoreBefore: boolean;
  canonicalBeforeCursor: string | null;
  historyLoadStatus: SessionHistoryLoadStatus;
  revision: number;
}

export interface TurnEditingDraft {
  sessionId: string;
  turnId: string;
  messageId: string;
  text: string;
  images: NonNullable<Message['images']>;
  contextReferences: NonNullable<Message['contextReferences']>;
  browserAnnotationRefs: NonNullable<Message['browserAnnotationRefs']>;
  browserNodeSelections: NonNullable<Message['browserNodeSelections']>;
  skillName: string | null;
  goalMode: boolean;
}

export interface LocalTurnSubmissionProjection {
  requestId: string;
  sessionId: string;
  status: 'submitting' | 'queued';
  workspaceId: string | null;
  workspacePath: string;
  placeholderMessageId: string;
  message: Message;
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
  messageJump: {
    messageId: null as string | null,
    nonce: 0,
  },

  // 消息状态
  canonicalTimelineProjection: null as SessionTimelineProjection | null,

  // 会话状态
  currentWorkspaceId: null as string | null,
  currentWorkspacePath: '' as string,
  workspaceSessionProjection: {
    workspaceId: null as string | null,
    sessions: [] as Session[],
    runtimeEpoch: null as string | null,
    eventStreamNextSequence: 0,
  },
  // 侧栏可以同时展示多个工作区。目录快照按 workspaceId 分片保存，避免
  // 后台工作区的状态刷新覆盖当前工作区的主投影。
  workspaceSessionProjections: {} as Record<string, WorkspaceSessionProjection>,
  personalSessionProjection: {
    sessions: [] as Session[],
    runtimeEpoch: null as string | null,
    eventStreamNextSequence: 0,
  },
  currentSessionId: null as string | null,
  sessionHydrating: false,
  sessionHistory: {
    workspaceId: null as string | null,
    workspacePath: null as string | null,
    sessionId: null as string | null,
    canonicalHasMoreBefore: false,
    canonicalBeforeCursor: null as string | null,
    historyLoadStatus: 'idle' as SessionHistoryLoadStatus,
    revision: 0,
  } as SessionHistoryState,
  queuedMessages: [] as QueuedMessage[],
  editingTurn: null as TurnEditingDraft | null,
  notificationCenter: {
    isLoading: false,
    operation: null,
    error: null,
    updatedAt: null,
  } as NotificationCenterStatus,

  // 处理状态
  isProcessing: false,
  backendProcessing: false,
  turnStage: null as TurnStage | null,
  activeMessageIds: new Set<string>(),
  pendingRequests: new Set<string>(),
  thinkingStartAt: null as number | null,
  processingActor: {
    source: 'orchestrator',
    agent: 'orchestrator',
  } as ProcessingActor,

  // 后端下发的完整状态
  appState: null as AppState | null,
  edits: [] as Edit[],
  changeMutationStatus: null as ChangeMutationStatus | null,
  orchestratorRuntimeState: null as OrchestratorRuntimeState | null,
  settingsBootstrapSnapshot: null as SettingsBootstrapSnapshot | null,
  draftOrchestratorSessionConfig: {} as Record<string, unknown>,
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

const IS_HOSTED_WEBVIEW = shouldUseHostProxyTransport();

const MAX_PERSISTED_ARRAY_LENGTH = 10000;
const WEBVIEW_STATE_SAVE_DEBOUNCE_MS = 120;
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

export function isPersistedSessionId(value: string | null | undefined): boolean {
  const sessionId = normalizeSessionId(value);
  return Boolean(sessionId);
}

function normalizeWorkspaceId(value: string | null | undefined): string | null {
  const workspaceId = typeof value === 'string' ? value.trim() : '';
  return workspaceId || null;
}

function normalizeWorkspacePath(value: string | null | undefined): string | null {
  const workspacePath = typeof value === 'string' ? value.trim() : '';
  return workspacePath || null;
}

function createEmptySessionHistoryState(
  workspaceId: string | null | undefined,
  workspacePath: string | null | undefined,
  sessionId: string | null | undefined,
  revision = 0,
): SessionHistoryState {
  return {
    workspaceId: normalizeWorkspaceId(workspaceId),
    workspacePath: normalizeWorkspacePath(workspacePath),
    sessionId: normalizeSessionId(sessionId),
    canonicalHasMoreBefore: false,
    canonicalBeforeCursor: null,
    historyLoadStatus: 'idle',
    revision,
  };
}

function createSessionScopeKey(
  workspaceId: string | null | undefined,
  sessionId: string | null | undefined,
): string | null {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    return null;
  }
  const normalizedWorkspaceId = normalizeWorkspaceId(workspaceId);
  return normalizedWorkspaceId
    ? `${normalizedWorkspaceId}\u0000${normalizedSessionId}`
    : `session:${normalizedSessionId}`;
}

function currentSessionScopeKey(sessionId: string | null | undefined = messagesState.currentSessionId): string | null {
  return createSessionScopeKey(messagesState.currentWorkspaceId, sessionId);
}

function normalizePersistedScrollPositions(value: unknown): ScrollPositions {
  const defaults = createDefaultScrollPositions();
  if (!value || typeof value !== 'object') {
    return defaults;
  }
  const source = value as Record<string, unknown>;
  const result: ScrollPositions = { thread: normalizeScrollTop(typeof source.thread === 'number' ? source.thread : 0) };
  // 动态恢复所有持久化的右侧面板滚动位置
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
  // 动态恢复所有持久化的右侧面板滚动锚点
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
  // 动态恢复所有持久化的右侧面板自动滚动配置
  for (const key of Object.keys(source)) {
    if (key !== 'thread' && typeof source[key] === 'boolean') {
      result[key] = source[key] as boolean;
    }
  }
  return result;
}

type QueuedMessageImageItem = NonNullable<QueuedMessage['images']>[number];
type QueuedMessageContextReferenceItem = NonNullable<QueuedMessage['contextReferences']>[number];
type QueuedMessageBrowserNodeSelectionItem = NonNullable<QueuedMessage['browserNodeSelections']>[number];

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

function normalizeQueuedMessageContextReference(
  reference: unknown,
): QueuedMessageContextReferenceItem | null {
  if (!reference || typeof reference !== 'object') return null;
  const item = reference as { kind?: unknown; path?: unknown; pathRef?: unknown; name?: unknown };
  if (item.kind !== 'file' && item.kind !== 'directory') return null;
  if (typeof item.path !== 'string' || !item.path.trim()) return null;
  const path = item.path.trim();
  return {
    kind: item.kind,
    path,
    ...(typeof item.pathRef === 'string' && item.pathRef.trim()
      ? { pathRef: item.pathRef.trim() }
      : {}),
    name: typeof item.name === 'string' && item.name.trim()
      ? item.name.trim()
      : path.split(/[\\/]/u).filter(Boolean).pop() || path,
  };
}

function normalizeQueuedMessageBrowserNodeSelection(
  selection: unknown,
): QueuedMessageBrowserNodeSelectionItem | null {
  if (!selection || typeof selection !== 'object' || Array.isArray(selection)) return null;
  const item = selection as Record<string, unknown>;
  const browserSessionId = typeof item.browserSessionId === 'string' ? item.browserSessionId.trim() : '';
  const tabId = typeof item.tabId === 'string' ? item.tabId.trim() : '';
  const surfaceId = typeof item.surfaceId === 'string' ? item.surfaceId.trim() : '';
  const navigationRevision = Number.isSafeInteger(item.navigationRevision) && Number(item.navigationRevision) >= 0
    ? Number(item.navigationRevision)
    : -1;
  const backendDomNodeId = Number.isSafeInteger(item.backendDomNodeId) && Number(item.backendDomNodeId) > 0
    ? Number(item.backendDomNodeId)
    : -1;
  const nodeName = typeof item.nodeName === 'string' ? item.nodeName.trim() : '';
  if (
    !browserSessionId
    || !tabId
    || !surfaceId
    || navigationRevision < 0
    || backendDomNodeId < 1
    || !nodeName
    || typeof item.outerHtmlTruncated !== 'boolean'
  ) return null;
  const attributes = item.attributes && typeof item.attributes === 'object' && !Array.isArray(item.attributes)
    ? Object.fromEntries(Object.entries(item.attributes as Record<string, unknown>)
      .filter(([, value]) => typeof value === 'string')) as Record<string, string>
    : {};
  const bounds = item.bounds && typeof item.bounds === 'object' && !Array.isArray(item.bounds)
    ? item.bounds as Record<string, unknown>
    : null;
  const normalizedBounds = bounds
    && ['x', 'y', 'width', 'height'].every((key) => typeof bounds[key] === 'number' && Number.isFinite(bounds[key]))
    ? {
        x: Number(bounds.x),
        y: Number(bounds.y),
        width: Math.max(0, Number(bounds.width)),
        height: Math.max(0, Number(bounds.height)),
      }
    : null;
  return {
    browserSessionId,
    tabId,
    surfaceId,
    navigationRevision,
    url: typeof item.url === 'string' ? item.url.trim() : '',
    title: typeof item.title === 'string' ? item.title.trim() : '',
    frameId: typeof item.frameId === 'string' ? item.frameId.trim() || null : null,
    backendDomNodeId,
    domNodeId: Number.isSafeInteger(item.domNodeId) && Number(item.domNodeId) > 0 ? Number(item.domNodeId) : null,
    nodeName,
    attributes,
    textExcerpt: typeof item.textExcerpt === 'string' ? item.textExcerpt : '',
    outerHtml: typeof item.outerHtml === 'string' ? item.outerHtml : '',
    outerHtmlTruncated: item.outerHtmlTruncated,
    ariaRole: typeof item.ariaRole === 'string' ? item.ariaRole.trim() || null : null,
    ariaName: typeof item.ariaName === 'string' ? item.ariaName.trim() || null : null,
    bounds: normalizedBounds,
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
      workspaceId: typeof item.workspaceId === 'string' && item.workspaceId.trim()
        ? item.workspaceId.trim()
        : undefined,
      workspacePath: typeof item.workspacePath === 'string' && item.workspacePath.trim()
        ? item.workspacePath.trim()
        : undefined,
      sessionId: typeof item.sessionId === 'string'
        ? item.sessionId.trim()
        : undefined,
      content: item.content,
      text: typeof item.text === 'string' ? item.text : null,
      createdAt: item.createdAt,
      skillName: typeof item.skillName === 'string' && item.skillName.trim()
        ? item.skillName.trim()
        : null,
      goalMode: item.goalMode === true,
      accessProfile: item.accessProfile === 'read_only'
        || item.accessProfile === 'restricted'
        || item.accessProfile === 'full_access'
        ? item.accessProfile
        : null,
      images: ensureArray(item.images)
        .map(normalizeQueuedMessageImage)
        .filter((image): image is QueuedMessageImageItem => image !== null),
      contextReferences: ensureArray(item.contextReferences)
        .map(normalizeQueuedMessageContextReference)
        .filter((reference): reference is QueuedMessageContextReferenceItem => reference !== null),
      browserAnnotationRefs: ensureArray(item.browserAnnotationRefs)
        .filter((annotationId): annotationId is string => typeof annotationId === 'string')
        .map((annotationId) => annotationId.trim())
        .filter(Boolean),
      browserNodeSelections: ensureArray(item.browserNodeSelections)
        .map(normalizeQueuedMessageBrowserNodeSelection)
        .filter((selection): selection is QueuedMessageBrowserNodeSelectionItem => selection !== null),
      canGuide: item.canGuide === true,
    }));
}

function normalizePersistedSessionViewState(
  scopeKey: string,
  value: unknown,
): PersistedSessionViewState | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return null;
  }
  const record = value as Record<string, unknown>;
  const normalizedSessionId = normalizeSessionId(typeof record.sessionId === 'string' ? record.sessionId : null);
  const normalizedWorkspaceId = normalizeWorkspaceId(typeof record.workspaceId === 'string' ? record.workspaceId : null);
  const expectedScopeKey = createSessionScopeKey(normalizedWorkspaceId, normalizedSessionId);
  if (!normalizedSessionId || !expectedScopeKey || expectedScopeKey !== scopeKey) {
    return null;
  }
  return {
    workspaceId: normalizedWorkspaceId,
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
  for (const [rawScopeKey, rawViewState] of Object.entries(value as Record<string, unknown>)) {
    if (count >= MAX_PERSISTED_ARRAY_LENGTH) {
      break;
    }
    const scopeKey = typeof rawScopeKey === 'string' ? rawScopeKey.trim() : '';
    if (!scopeKey) {
      continue;
    }
    const next = normalizePersistedSessionViewState(scopeKey, rawViewState);
    if (!next) {
      continue;
    }
    normalized[scopeKey] = next;
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
let sessionViewStateByScope = $state<Record<string, PersistedSessionViewState>>({});
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

// 新增状态：阶段、Toast、模型状态
// 统一 Worker 运行态（唯一权威来源）
// 当前主线以 authoritative projection 为准，仅叠加尚未被后端接纳的本地乐观节点。



const messageProjection = $derived.by(() => {
  const projection = messagesState.canonicalTimelineProjection;
  if (!projection) {
    return { thread: [] };
  }
  return {
    thread: buildTimelinePanelMessages(projection, 'thread'),
  };
});

export interface ToastOptions {
  source?: string;
  actionRequired?: boolean;
  duration?: number;
  forceVisible?: boolean;
}

interface ToastRecord {
  id: string;
  type: string;
  title?: string;
  message: string;
  source?: string;
  actionRequired?: boolean;
  duration?: number;
}

let toasts = $state<ToastRecord[]>([]);

export type Notification = NormalizedIncidentRecord;
const MAX_NOTIFICATIONS_PER_CONTEXT = 200;

let notifications = $state<Notification[]>([]);
let unreadNotificationCount = $state(0);
let notificationsByContext = $state<Record<string, Notification[]>>({});

let modelStatus = $state<ModelStatusMap>({
  orchestrator: { status: 'checking' },
  auxiliary: { status: 'checking' },
});

export function getModelStatus(): ModelStatusMap {
  return modelStatus;
}

export function setModelStatus(next: ModelStatusMap): void {
  modelStatus = next;
}

// ============ 角色驱动 Tab 状态 ============
// 前端轻量 Agent 信息（从 AgentBinding + RoleTemplate 合成）
export interface EnabledAgent {
  /** 角色模板 ID（= 运行时 agentId = workerSlot） */
  templateId: string;
  /** 展示名称（来自 RoleTemplate.displayName） */
  displayName: string;
  /** 展示名称国际化 key（若存在则优先用于 UI 本地化） */
  displayNameKey?: string;
  /** 绑定的引擎 ID：空串 = 继承编排模型，非空 = 显式绑定 */
  engineId: string;
  /** 排序序号 */
  order: number;
  /** CSS 颜色 token（来自 RoleTemplate.defaultUI.colorToken） */
  colorToken: string;
  /** 图标名称 */
  icon?: string;
}

let enabledAgents = $state<EnabledAgent[]>([]);

function normalizeOrchestratorRuntimeState(
  input: OrchestratorRuntimeState | null | undefined,
): OrchestratorRuntimeState | null {
  if (!input || typeof input !== 'object') return null;
  const status = input.status === 'idle'
    || input.status === 'running'
    || input.status === 'waiting'
    || input.status === 'paused'
    || input.status === 'blocked'
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
  const stage = input.stage === 'pending'
    || input.stage === 'preparing'
    || input.stage === 'streaming'
    || input.stage === 'finalizing'
    || input.stage === 'done'
    ? input.stage
    : null;
  return {
    isProcessing: input.isProcessing === true,
    source,
    agent,
    startedAt,
    pendingRequestIds,
    stage,
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
  const previousPendingRequestIds = messagesState.pendingRequests;
  const previousThinkingStartAt = messagesState.thinkingStartAt;
  const sessionKey = executionSessionKey(messagesState.currentSessionId);
  const localPendingRequestIds = localSubmissionRequestIdsForSession(sessionKey);
  const settledRequestIds = terminalRequestIdsBySession.get(sessionKey) ?? new Set<string>();
  const snapshot = normalizeProcessingStateSnapshot(input);
  if (!snapshot) {
    messagesState.backendProcessing = false;
    messagesState.pendingRequests = localPendingRequestIds;
    if (localPendingRequestIds.size === 0) {
      messagesState.activeMessageIds = new Set();
      messagesState.thinkingStartAt = null;
      messagesState.turnStage = 'done';
    } else if (!messagesState.thinkingStartAt) {
      messagesState.thinkingStartAt = Date.now();
      messagesState.turnStage = 'pending';
    }
    updateProcessingState();
    saveCurrentExecutionProjection();
    return;
  }
  const canonicalPendingRequestIds = snapshot.pendingRequestIds
    .filter((requestId) => !settledRequestIds.has(requestId));
  const canonicalIsProcessing = snapshot.isProcessing
    && (snapshot.pendingRequestIds.length === 0 || canonicalPendingRequestIds.length > 0);
  const pendingRequestIds = new Set([
    ...canonicalPendingRequestIds,
    ...localPendingRequestIds,
  ]);
  const activeMessageIds = canonicalIsProcessing || localPendingRequestIds.size > 0
    ? messagesState.activeMessageIds
    : new Set<string>();
  const nextIsProcessing = canonicalIsProcessing || localPendingRequestIds.size > 0;
  const nextTurnStage = canonicalIsProcessing
    ? snapshot.stage || 'pending'
    : localPendingRequestIds.size > 0 ? 'pending' : 'done';

  messagesState.backendProcessing = canonicalIsProcessing;
  messagesState.turnStage = nextTurnStage;
  messagesState.pendingRequests = pendingRequestIds;
  messagesState.activeMessageIds = activeMessageIds;
  if (snapshot.source) {
    setProcessingActor(snapshot.source, snapshot.agent || undefined);
  }
  if (nextIsProcessing) {
    const preservesActiveLocalRequest = [...previousPendingRequestIds]
      .some((requestId) => pendingRequestIds.has(requestId));
    messagesState.thinkingStartAt = preservesActiveLocalRequest && previousThinkingStartAt
      ? previousThinkingStartAt
      : snapshot.startedAt || previousThinkingStartAt || Date.now();
  } else {
    messagesState.thinkingStartAt = null;
  }
  messagesState.isProcessing = nextIsProcessing;
  saveCurrentExecutionProjection();
}

// Wave 执行状态（提案 4.6）
let waveState = $state<WaveState | null>(null);

// 请求-响应绑定状态（消息响应流设计）
let requestBindings = $state<Map<string, RequestResponseBinding>>(new Map());
let localTurnSubmissionProjections = $state<Map<string, LocalTurnSubmissionProjection>>(new Map());
let terminalRequestIdsBySession = $state<Map<string, Set<string>>>(new Map());

type SessionExecutionProjection = {
  backendProcessing: boolean;
  turnStage: TurnStage | null;
  pendingRequests: Set<string>;
  activeMessageIds: Set<string>;
  thinkingStartAt: number | null;
  isProcessing: boolean;
  processingActor: ProcessingActor;
};

// 执行态属于 session，而不是当前页面。切换会话只切换这里的投影，不能清掉
// 其他会话仍在后台运行的 request binding 或 processing 状态。
let sessionExecutionProjections = $state<Map<string, SessionExecutionProjection>>(new Map());

function executionSessionKey(sessionId: string | null | undefined): string {
  const normalized = typeof sessionId === 'string' ? sessionId.trim() : '';
  return normalized || '__draft__';
}

function localSubmissionRequestIdsForSession(sessionId: string): Set<string> {
  return new Set(
    [...localTurnSubmissionProjections.values()]
      .filter((submission) => (
        submission.sessionId === sessionId
        && submission.status === 'submitting'
      ))
      .map((submission) => submission.requestId),
  );
}

function requestBelongsToSession(sessionId: string, requestId: string): boolean {
  if (terminalRequestIdsBySession.get(sessionId)?.has(requestId)) {
    return true;
  }
  const binding = requestBindings.get(requestId);
  if (binding && executionSessionKey(binding.sessionId) === sessionId) {
    return true;
  }
  const submission = localTurnSubmissionProjections.get(requestId);
  if (submission?.sessionId === sessionId) {
    return true;
  }
  if (
    executionSessionKey(messagesState.currentSessionId) === sessionId
    && messagesState.pendingRequests.has(requestId)
  ) {
    return true;
  }
  return sessionExecutionProjections.get(sessionId)?.pendingRequests.has(requestId) === true;
}

function removeRequestBindingOnly(requestId: string): RequestResponseBinding | undefined {
  const binding = requestBindings.get(requestId);
  if (!binding) return undefined;
  if (binding.timeoutId) {
    clearTimeout(binding.timeoutId);
  }
  const next = new Map(requestBindings);
  next.delete(requestId);
  requestBindings = next;
  return binding;
}

function rekeyLocalTurnSubmissionProjections(
  previousSessionId: string | null,
  nextSessionId: string,
): void {
  const previousKey = executionSessionKey(previousSessionId);
  const nextKey = executionSessionKey(nextSessionId);
  if (previousKey === nextKey) return;
  const next = new Map(localTurnSubmissionProjections);
  let changed = false;
  for (const [requestId, submission] of next) {
    if (submission.sessionId !== previousKey) continue;
    next.set(requestId, { ...submission, sessionId: nextKey });
    changed = true;
  }
  if (changed) {
    localTurnSubmissionProjections = next;
  }
}

function rekeyRequestBindings(previousSessionId: string | null, nextSessionId: string): void {
  const previousKey = executionSessionKey(previousSessionId);
  const nextKey = executionSessionKey(nextSessionId);
  if (previousKey === nextKey) return;
  const next = new Map(requestBindings);
  let changed = false;
  for (const [requestId, binding] of next) {
    if (executionSessionKey(binding.sessionId) !== previousKey) continue;
    next.set(requestId, { ...binding, sessionId: nextKey });
    changed = true;
  }
  if (changed) {
    requestBindings = next;
  }
}

function rememberTerminalRequest(sessionId: string, requestId: string): void {
  const existing = terminalRequestIdsBySession.get(sessionId) ?? new Set<string>();
  if (existing.has(requestId)) return;
  const nextIds = new Set(existing);
  nextIds.add(requestId);
  const next = new Map(terminalRequestIdsBySession);
  next.set(sessionId, nextIds);
  terminalRequestIdsBySession = next;
}

function forgetTerminalRequest(sessionId: string, requestId: string): void {
  const existing = terminalRequestIdsBySession.get(sessionId);
  if (!existing?.has(requestId)) return;
  const nextIds = new Set(existing);
  nextIds.delete(requestId);
  const next = new Map(terminalRequestIdsBySession);
  if (nextIds.size > 0) {
    next.set(sessionId, nextIds);
  } else {
    next.delete(sessionId);
  }
  terminalRequestIdsBySession = next;
}

function cloneExecutionProjection(
  projection: SessionExecutionProjection,
): SessionExecutionProjection {
  return {
    ...projection,
    pendingRequests: new Set(projection.pendingRequests),
    activeMessageIds: new Set(projection.activeMessageIds),
    processingActor: { ...projection.processingActor },
};
}

function saveCurrentExecutionProjection(sessionId = messagesState.currentSessionId): void {
  const next = new Map(sessionExecutionProjections);
  next.set(executionSessionKey(sessionId), {
    backendProcessing: messagesState.backendProcessing,
    turnStage: messagesState.turnStage,
    pendingRequests: new Set(messagesState.pendingRequests),
    activeMessageIds: new Set(messagesState.activeMessageIds),
    thinkingStartAt: messagesState.thinkingStartAt,
    isProcessing: messagesState.isProcessing,
    processingActor: { ...messagesState.processingActor },
  });
  sessionExecutionProjections = next;
}

function restoreExecutionProjection(sessionId: string | null): void {
  const stored = sessionExecutionProjections.get(executionSessionKey(sessionId));
  const projection = stored
    ? cloneExecutionProjection(stored)
    : {
      backendProcessing: false,
      turnStage: null,
      pendingRequests: new Set<string>(),
      activeMessageIds: new Set<string>(),
      thinkingStartAt: null,
      isProcessing: false,
      processingActor: { source: 'orchestrator', agent: 'orchestrator' } as ProcessingActor,
    };
  messagesState.backendProcessing = projection.backendProcessing;
  messagesState.turnStage = projection.turnStage;
  messagesState.pendingRequests = projection.pendingRequests;
  messagesState.activeMessageIds = projection.activeMessageIds;
  messagesState.thinkingStartAt = projection.thinkingStartAt;
  messagesState.isProcessing = projection.isProcessing;
  messagesState.processingActor = projection.processingActor;
}

function rekeyExecutionProjection(
  previousSessionId: string | null,
  nextSessionId: string | null,
): void {
  const previousKey = executionSessionKey(previousSessionId);
  const nextKey = executionSessionKey(nextSessionId);
  if (previousKey === nextKey) {
    return;
  }
  const previous = sessionExecutionProjections.get(previousKey);
  if (!previous) {
    return;
  }
  const next = new Map(sessionExecutionProjections);
  next.delete(previousKey);
  next.set(nextKey, cloneExecutionProjection(previous));
  sessionExecutionProjections = next;
}

function updateStoredExecutionProjection(
  sessionId: string,
  update: (projection: SessionExecutionProjection) => SessionExecutionProjection,
): void {
  const key = executionSessionKey(sessionId);
  const existing = sessionExecutionProjections.get(key) ?? {
    backendProcessing: false,
    turnStage: null,
    pendingRequests: new Set<string>(),
    activeMessageIds: new Set<string>(),
    thinkingStartAt: null,
    isProcessing: false,
    processingActor: { source: 'orchestrator', agent: 'orchestrator' } as ProcessingActor,
  };
  const next = new Map(sessionExecutionProjections);
  next.set(key, cloneExecutionProjection(update(cloneExecutionProjection(existing))));
  sessionExecutionProjections = next;
}

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

export function hasQueuedMessagesAcrossSessions(): boolean {
  return messagesState.queuedMessages.length > 0;
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

function normalizeOptionalStatusString(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value.trim() : null;
}

export function setChangeMutationStatus(status: ChangeMutationStatus | null): void {
  if (!status?.isMutating) {
    messagesState.changeMutationStatus = null;
    return;
  }
  messagesState.changeMutationStatus = {
    isMutating: true,
    sessionId: normalizeOptionalStatusString(status.sessionId),
    workspaceId: normalizeOptionalStatusString(status.workspaceId),
    workspacePath: normalizeOptionalStatusString(status.workspacePath),
    updatedAt: typeof status.updatedAt === 'number' && Number.isFinite(status.updatedAt)
      ? status.updatedAt
      : Date.now(),
  };
}

export function applyPendingChangesProjection(payload: {
  generatedAt?: number;
  sessionId?: string;
  workspaceId?: string;
  pendingChanges?: unknown[];
  pendingChangesState?: unknown;
}): boolean {
  const sessionId = normalizeOptionalStatusString(payload.sessionId);
  const workspaceId = normalizeOptionalStatusString(payload.workspaceId);
  if (!sessionId || sessionId !== normalizeOptionalStatusString(messagesState.currentSessionId)) {
    return false;
  }
  if (workspaceId && workspaceId !== normalizeOptionalStatusString(messagesState.currentWorkspaceId)) {
    return false;
  }

  const generatedAt = typeof payload.generatedAt === 'number' && Number.isFinite(payload.generatedAt)
    ? payload.generatedAt
    : 0;
  const currentVersion = typeof messagesState.appState?.pendingChangesStateVersion === 'number'
    ? messagesState.appState.pendingChangesStateVersion
    : 0;
  if (generatedAt < currentVersion) {
    return false;
  }

  const pendingChanges = ensureArray<Edit>(payload.pendingChanges).filter((change) => (
    !!change && typeof change.filePath === 'string' && change.filePath.trim().length > 0
  ));
  messagesState.edits = pendingChanges;
  if (messagesState.appState) {
    messagesState.appState = {
      ...messagesState.appState,
      pendingChanges,
      pendingChangesState: payload.pendingChangesState ?? null,
      pendingChangesStateVersion: generatedAt,
    };
  }
  return true;
}

function syncEditsFromAppState(nextState: AppState | null): void {
  if (!nextState) {
    messagesState.edits = [];
    return;
  }
  if (Array.isArray(nextState.pendingChanges)) {
    messagesState.edits = ensureArray<Edit>(nextState.pendingChanges);
  }
}

// ============ getState() 仅用于现有调用方（Svelte 5 迁移中）============
// ⚠️ 注意：此函数返回的对象无法被 Svelte 5 正确追踪
// 建议使用上面的独立 getter 函数或直接使用 messagesState

export function getState() {
  return {
    get currentTopTab() { return messagesState.currentTopTab; },
    get messageJump() { return messagesState.messageJump; },
    get canonicalTimelineProjection() { return messagesState.canonicalTimelineProjection; },
    get threadMessages() { return messageProjection.thread; },
    get sessions() { return messagesState.workspaceSessionProjection.sessions; },
    get currentWorkspaceId() { return messagesState.currentWorkspaceId; },
    get currentWorkspacePath() { return messagesState.currentWorkspacePath; },
    get currentSessionId() { return messagesState.currentSessionId; },
    get sessionHistory() { return messagesState.sessionHistory; },
    get queuedMessages() { return messagesState.queuedMessages; },
    set queuedMessages(v) { setQueuedMessages(ensureArray<QueuedMessage>(v)); },
    get isProcessing() { return messagesState.isProcessing; },
    get turnStage() { return messagesState.turnStage; },
    get thinkingStartAt() { return messagesState.thinkingStartAt; },
    get processingActor() { return messagesState.processingActor; },
    get appState() { return messagesState.appState; },
    get changeMutationStatus() { return messagesState.changeMutationStatus; },
    get settingsBootstrapSnapshot() { return messagesState.settingsBootstrapSnapshot; },
    set settingsBootstrapSnapshot(v) { messagesState.settingsBootstrapSnapshot = v; },
    get draftOrchestratorSessionConfig() { return messagesState.draftOrchestratorSessionConfig; },
    set draftOrchestratorSessionConfig(v) { messagesState.draftOrchestratorSessionConfig = v; },
    get settingsRegistrySnapshot() { return messagesState.settingsRegistrySnapshot; },
    set settingsRegistrySnapshot(v) { messagesState.settingsRegistrySnapshot = v; },
    get scrollPositions() { return messagesState.scrollPositions; },
    get autoScrollEnabled() { return messagesState.autoScrollEnabled; },
    get edits() { return messagesState.edits; },
    set edits(v) { messagesState.edits = ensureArray<Edit>(v); },
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
  };
}

// ============ 状态更新函数 ============

function createSessionViewStateSnapshot(sessionId: string | null | undefined): PersistedSessionViewState | null {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    return null;
  }
  const workspaceId = normalizeWorkspaceId(messagesState.currentWorkspaceId);
  return {
    workspaceId,
    sessionId: normalizedSessionId,
    scrollPositions: normalizePersistedScrollPositions(clonePersistablePayload(messagesState.scrollPositions)),
    scrollAnchors: normalizePersistedScrollAnchors(clonePersistablePayload(messagesState.scrollAnchors)),
    autoScrollEnabled: normalizePersistedAutoScrollConfig(clonePersistablePayload(messagesState.autoScrollEnabled)),
  };
}

function upsertSessionViewStateSnapshot(snapshot: PersistedSessionViewState | null): void {
  if (!snapshot) {
    return;
  }
  const scopeKey = createSessionScopeKey(snapshot.workspaceId, snapshot.sessionId);
  if (!scopeKey) {
    return;
  }
  sessionViewStateByScope = {
    ...sessionViewStateByScope,
    [scopeKey]: snapshot,
  };
}

function captureCurrentSessionViewState(): void {
  upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(messagesState.currentSessionId));
}

function getSessionViewState(sessionId: string | null | undefined): PersistedSessionViewState | null {
  const scopeKey = currentSessionScopeKey(sessionId);
  if (!scopeKey) {
    return null;
  }
  return sessionViewStateByScope[scopeKey] || null;
}

function pruneSessionViewStateByKnownSessions(): void {
  const currentWorkspaceId = normalizeWorkspaceId(
    messagesState.workspaceSessionProjection.workspaceId,
  );
  const knownScopeKeys = new Set<string>();
  for (const session of messagesState.workspaceSessionProjection.sessions) {
    const scopeKey = createSessionScopeKey(currentWorkspaceId, session?.id);
    if (scopeKey) {
      knownScopeKeys.add(scopeKey);
    }
  }
  const currentScopeKey = currentSessionScopeKey();
  if (currentScopeKey) {
    knownScopeKeys.add(currentScopeKey);
  }
  if (knownScopeKeys.size === 0) {
    return;
  }
  const nextEntries = Object.entries(sessionViewStateByScope)
    .filter(([scopeKey, snapshot]) => (
      normalizeWorkspaceId(snapshot.workspaceId) !== currentWorkspaceId
      || knownScopeKeys.has(scopeKey)
    ));
  if (nextEntries.length === Object.keys(sessionViewStateByScope).length) {
    return;
  }
  sessionViewStateByScope = Object.fromEntries(nextEntries);
}

function applySessionViewState(sessionId: string | null | undefined): boolean {
  const snapshot = getSessionViewState(sessionId);
  if (!snapshot) {
    return false;
  }
  const scopeKey = currentSessionScopeKey(sessionId);
  const normalizedSnapshot = scopeKey
    ? normalizePersistedSessionViewState(scopeKey, clonePersistablePayload(snapshot))
    : null;
  if (!normalizedSnapshot) {
    return false;
  }
  messagesState.scrollPositions = normalizePersistedScrollPositions(normalizedSnapshot.scrollPositions);
  messagesState.scrollAnchors = normalizePersistedScrollAnchors(normalizedSnapshot.scrollAnchors);
  messagesState.autoScrollEnabled = normalizePersistedAutoScrollConfig(normalizedSnapshot.autoScrollEnabled);
  return true;
}

function resetSessionScopedExecutionState(sessionId: string | null): void {
  messagesState.edits = [];
  messagesState.orchestratorRuntimeState = null;
  restoreExecutionProjection(sessionId);
  if (messagesState.appState) {
    messagesState.appState = {
      ...messagesState.appState,
      pendingChanges: [],
      pendingChangesState: null,
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
    captureCurrentSessionViewState();
    pruneSessionViewStateByKnownSessions();
    const state: WebviewPersistedState = {
      currentTopTab: messagesState.currentTopTab,
      scrollPositions: messagesState.scrollPositions,
      scrollAnchors: messagesState.scrollAnchors,
      autoScrollEnabled: messagesState.autoScrollEnabled,
      sessionViewStateByScope,
    };
    vscode.setState(state);
  } catch (error) {
    console.warn('[MessagesStore] Webview 状态持久化失败，已降级继续运行', error);
  }
}

// 非 hosted webview 环境（独立 web 客户端）注册 beforeunload 同步保存，
// 防止短暂的 debounce 窗口内刷新丢失数据。
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
  updateProcessingState();
}

export function replaceOrchestratorRuntimeState(input: OrchestratorRuntimeState | null): void {
  const next = normalizeOrchestratorRuntimeState(input);
  messagesState.orchestratorRuntimeState = next;
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
    saveCurrentExecutionProjection(previousSessionId);
  }
  messagesState.currentSessionId = nextSessionId;
  if (hasChanged) {
    messagesState.editingTurn = null;
    messagesState.sessionHistory = createEmptySessionHistoryState(
      messagesState.currentWorkspaceId,
      messagesState.currentWorkspacePath,
      nextSessionId,
      messagesState.sessionHistory.revision + 1,
    );
    resetNotificationCenterStatus();
  }
  if (hasChanged) {
    resetSessionScopedExecutionState(nextSessionId);
    // 会话切换时消息内容以后端分页快照为唯一真相源。
    // 本地只恢复滚动/定位等轻量视图状态，避免旧 session 的主线或右侧面板内容短暂残留。
    clearCanonicalSessionTurns(nextSessionId ?? undefined);
    messagesState.canonicalTimelineProjection = null;
    restoredSessionView = applySessionViewState(nextSessionId);
    if (!restoredSessionView) {
      resetPanelScrollRuntimeState();
    }
    messagesState.queuedMessages = [];
  }
  syncNotificationsFromContext(nextSessionId);
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
  saveCurrentExecutionProjection(currentSessionId);
  rekeyExecutionProjection(currentSessionId, nextSessionId);
  rekeyLocalTurnSubmissionProjections(currentSessionId, nextSessionId);
  rekeyRequestBindings(currentSessionId, nextSessionId);
  clearCanonicalSessionTurns(nextSessionId);
  messagesState.currentSessionId = nextSessionId;
  messagesState.sessionHistory = createEmptySessionHistoryState(
    messagesState.currentWorkspaceId,
    messagesState.currentWorkspacePath,
    nextSessionId,
    messagesState.sessionHistory.revision + 1,
  );
  resetNotificationCenterStatus();
  syncNotificationsFromContext(nextSessionId);
  saveWebviewState();
  return true;
}

export interface WorkspaceSessionProjection {
  workspaceId: string;
  sessions: Session[];
  runtimeEpoch: string | null;
  eventStreamNextSequence: number;
}

export interface WorkspaceSessionProjectionCursor {
  runtimeEpoch: string;
  eventStreamNextSequence: number;
}

function normalizeWorkspaceSessionProjectionCursor(
  cursor: WorkspaceSessionProjectionCursor,
): WorkspaceSessionProjectionCursor {
  const runtimeEpoch = cursor.runtimeEpoch?.trim() || '';
  const eventStreamNextSequence = Number.isFinite(cursor.eventStreamNextSequence)
    ? Math.floor(cursor.eventStreamNextSequence)
    : 0;
  if (!runtimeEpoch || eventStreamNextSequence < 1) {
    throw new Error('更新工作区会话目录时必须提供有效的运行时代际和事件游标');
  }
  return { runtimeEpoch, eventStreamNextSequence };
}

export function canApplyWorkspaceSessionProjectionCursor(
  workspaceId: string,
  cursor: WorkspaceSessionProjectionCursor,
  options: { allowRuntimeEpochChange?: boolean } = {},
): boolean {
  const currentWorkspaceId = normalizeWorkspaceId(workspaceId);
  if (!currentWorkspaceId) {
    return false;
  }
  const normalized = normalizeWorkspaceSessionProjectionCursor(cursor);
  const current = messagesState.workspaceSessionProjections[currentWorkspaceId]
    ?? (normalizeWorkspaceId(messagesState.workspaceSessionProjection.workspaceId) === currentWorkspaceId
      ? messagesState.workspaceSessionProjection
      : null);
  if (!current) {
    return true;
  }
  const currentRuntimeEpoch = current.runtimeEpoch?.trim() || '';
  if (
    currentRuntimeEpoch
    && currentRuntimeEpoch !== normalized.runtimeEpoch
    && options.allowRuntimeEpochChange !== true
  ) {
    return false;
  }
  return currentRuntimeEpoch !== normalized.runtimeEpoch
    || normalized.eventStreamNextSequence >= current.eventStreamNextSequence;
}

export function replaceWorkspaceSessionProjection(
  workspaceId: string,
  newSessions: Session[],
  cursor: WorkspaceSessionProjectionCursor,
  options: {
    allowRuntimeEpochChange?: boolean;
    mirrorCurrentProjection?: boolean;
  } = {},
): boolean {
  const seen = new Set<string>();
  const currentWorkspaceId = normalizeWorkspaceId(workspaceId);
  if (!currentWorkspaceId) {
    throw new Error('更新工作区会话目录时必须提供 workspaceId');
  }
  const normalizedCursor = normalizeWorkspaceSessionProjectionCursor(cursor);
  if (!canApplyWorkspaceSessionProjectionCursor(currentWorkspaceId, normalizedCursor, options)) {
    return false;
  }
  const sessions = ensureArray<Session>(newSessions)
    .filter((session): session is Session => !!session && typeof session === 'object' && typeof session.id === 'string' && session.id.trim().length > 0)
    .filter((session) => {
      const sessionWorkspaceId = normalizeWorkspaceId(session.workspaceId);
      return !currentWorkspaceId || !sessionWorkspaceId || sessionWorkspaceId === currentWorkspaceId;
    })
    .filter((session) => {
      if (seen.has(session.id)) return false;
      seen.add(session.id);
      return true;
    });
  const nextProjection: WorkspaceSessionProjection = {
    workspaceId: currentWorkspaceId,
    sessions,
    runtimeEpoch: normalizedCursor.runtimeEpoch,
    eventStreamNextSequence: normalizedCursor.eventStreamNextSequence,
  };
  messagesState.workspaceSessionProjections = {
    ...messagesState.workspaceSessionProjections,
    [currentWorkspaceId]: nextProjection,
  };
  const shouldMirrorCurrentProjection = options.mirrorCurrentProjection !== false;
  if (shouldMirrorCurrentProjection) {
    messagesState.workspaceSessionProjection = nextProjection;
    pruneSessionViewStateByKnownSessions();
    saveWebviewState();
  }
  return true;
}

export function replacePersonalSessionProjection(
  newSessions: Session[],
  cursor: WorkspaceSessionProjectionCursor,
  options: { allowRuntimeEpochChange?: boolean } = {},
): boolean {
  const normalizedCursor = normalizeWorkspaceSessionProjectionCursor(cursor);
  const current = messagesState.personalSessionProjection;
  if (
    current.runtimeEpoch
    && current.runtimeEpoch !== normalizedCursor.runtimeEpoch
    && options.allowRuntimeEpochChange !== true
  ) {
    return false;
  }
  if (
    current.runtimeEpoch === normalizedCursor.runtimeEpoch
    && normalizedCursor.eventStreamNextSequence < current.eventStreamNextSequence
  ) {
    return false;
  }
  const seen = new Set<string>();
  const sessions = ensureArray<Session>(newSessions)
    .filter((session): session is Session => !!session && typeof session === 'object' && typeof session.id === 'string' && session.id.trim().length > 0)
    .filter((session) => !normalizeWorkspaceId(session.workspaceId))
    .filter((session) => {
      if (seen.has(session.id)) return false;
      seen.add(session.id);
      return true;
    });
  messagesState.personalSessionProjection = {
    sessions,
    runtimeEpoch: normalizedCursor.runtimeEpoch,
    eventStreamNextSequence: normalizedCursor.eventStreamNextSequence,
  };
  pruneSessionViewStateByKnownSessions();
  saveWebviewState();
  return true;
}

/**
 * 应用首条消息 accepted 事件携带的权威会话目录增量。
 *
 * 会话目录的快照只负责全量 bootstrap，事件流负责提交后的增量。两者
 * 通过同一个事件游标排序，因此旧快照可以被拒绝，或在 accepted 之后
 * 到达时只更新游标而不删除已提交的会话。
 */
export function upsertAcceptedSessionDirectoryEntry(
  session: Session,
  cursor: WorkspaceSessionProjectionCursor,
): boolean {
  const sessionId = normalizeSessionId(session.id);
  const runtimeEpoch = cursor.runtimeEpoch?.trim() || '';
  if (!sessionId || !runtimeEpoch) {
    return false;
  }
  const normalizedSession: Session = { ...session, id: sessionId };
  const normalizedCursor = normalizeWorkspaceSessionProjectionCursor(cursor);
  const workspaceId = normalizeWorkspaceId(normalizedSession.workspaceId);
  const activeWorkspaceId = normalizeWorkspaceId(messagesState.currentWorkspaceId);

  if (workspaceId) {
    const current = messagesState.workspaceSessionProjections[workspaceId]
      ?? (normalizeWorkspaceId(messagesState.workspaceSessionProjection.workspaceId) === workspaceId
        ? messagesState.workspaceSessionProjection
        : {
            workspaceId,
            sessions: [],
            runtimeEpoch: null,
            eventStreamNextSequence: 0,
          });
    if (current.workspaceId && normalizeWorkspaceId(current.workspaceId) !== workspaceId) {
      return false;
    }
    if (current.runtimeEpoch && current.runtimeEpoch !== runtimeEpoch) {
      return false;
    }
    const sessions = current.sessions.some((candidate) => candidate.id === sessionId)
      ? current.sessions.map((candidate) => candidate.id === sessionId ? normalizedSession : candidate)
      : [...current.sessions, normalizedSession];
    const effectiveCursor: WorkspaceSessionProjectionCursor = current.runtimeEpoch === runtimeEpoch
      ? {
          runtimeEpoch,
          eventStreamNextSequence: Math.max(
            current.eventStreamNextSequence,
            normalizedCursor.eventStreamNextSequence,
          ),
        }
      : normalizedCursor;
    return replaceWorkspaceSessionProjection(workspaceId, sessions, effectiveCursor, {
      mirrorCurrentProjection: activeWorkspaceId === workspaceId
        || normalizeWorkspaceId(messagesState.workspaceSessionProjection.workspaceId) === workspaceId,
    });
  }

  if (activeWorkspaceId) {
    return false;
  }
  const current = messagesState.personalSessionProjection;
  if (current.runtimeEpoch && current.runtimeEpoch !== runtimeEpoch) {
    return false;
  }
  const sessions = current.sessions.some((candidate) => candidate.id === sessionId)
    ? current.sessions.map((candidate) => candidate.id === sessionId ? normalizedSession : candidate)
    : [...current.sessions, normalizedSession];
  const effectiveCursor: WorkspaceSessionProjectionCursor = current.runtimeEpoch === runtimeEpoch
    ? {
        runtimeEpoch,
        eventStreamNextSequence: Math.max(
          current.eventStreamNextSequence,
          normalizedCursor.eventStreamNextSequence,
        ),
      }
    : normalizedCursor;
  return replacePersonalSessionProjection(sessions, effectiveCursor);
}

export function advanceWorkspaceSessionProjectionCursor(
  workspaceId: string,
  cursor: WorkspaceSessionProjectionCursor,
): boolean {
  const currentWorkspaceId = normalizeWorkspaceId(workspaceId);
  if (!currentWorkspaceId) {
    return false;
  }
  const current = messagesState.workspaceSessionProjections[currentWorkspaceId]
    ?? (normalizeWorkspaceId(messagesState.workspaceSessionProjection.workspaceId) === currentWorkspaceId
      ? messagesState.workspaceSessionProjection
      : null);
  if (!current) {
    return false;
  }
  const normalizedCursor = normalizeWorkspaceSessionProjectionCursor(cursor);
  if (!canApplyWorkspaceSessionProjectionCursor(currentWorkspaceId, normalizedCursor)) {
    return false;
  }
  if (
    current.runtimeEpoch === normalizedCursor.runtimeEpoch
    && current.eventStreamNextSequence === normalizedCursor.eventStreamNextSequence
  ) {
    return true;
  }
  const nextProjection: WorkspaceSessionProjection = {
    ...current,
    runtimeEpoch: normalizedCursor.runtimeEpoch,
    eventStreamNextSequence: normalizedCursor.eventStreamNextSequence,
  };
  messagesState.workspaceSessionProjections = {
    ...messagesState.workspaceSessionProjections,
    [currentWorkspaceId]: nextProjection,
  };
  if (normalizeWorkspaceId(messagesState.currentWorkspaceId) === currentWorkspaceId
    || normalizeWorkspaceId(messagesState.workspaceSessionProjection.workspaceId) === currentWorkspaceId) {
    messagesState.workspaceSessionProjection = nextProjection;
  }
  saveWebviewState();
  return true;
}

export function updateWorkspaceSessionProjectionSessions(
  workspaceId: string,
  newSessions: Session[],
): boolean {
  const current = messagesState.workspaceSessionProjection;
  const currentWorkspaceId = normalizeWorkspaceId(workspaceId);
  if (
    !currentWorkspaceId
    || normalizeWorkspaceId(current.workspaceId) !== currentWorkspaceId
    || !current.runtimeEpoch
    || current.eventStreamNextSequence < 1
  ) {
    return false;
  }
  return replaceWorkspaceSessionProjection(currentWorkspaceId, newSessions, {
    runtimeEpoch: current.runtimeEpoch,
    eventStreamNextSequence: current.eventStreamNextSequence,
  });
}

export function clearWorkspaceSessionProjection() {
  messagesState.workspaceSessionProjections = {};
  messagesState.workspaceSessionProjection = {
    workspaceId: null,
    sessions: [],
    runtimeEpoch: null,
    eventStreamNextSequence: 0,
  };
  pruneSessionViewStateByKnownSessions();
  saveWebviewState();
}

export function setQueuedMessages(newQueuedMessages: QueuedMessage[]) {
  const normalized = normalizeQueuedMessageList(newQueuedMessages);
  messagesState.queuedMessages = normalized;
}

export function setProcessingActor(source: string, agent?: string) {
  messagesState.processingActor = {
    source: source as ProcessingActor['source'],
    agent: (agent || source || 'orchestrator') as ProcessingActor['agent'],
  };
}

export function setAppState(nextState: AppState | null) {
  if (nextState) {
    syncEditsFromAppState(nextState);
    messagesState.appState = nextState;
    messagesState.bootstrapped = true;
  } else {
    syncEditsFromAppState(null);
    messagesState.appState = null;
  }
}

function updateProcessingState() {
  const nextIsProcessing = messagesState.backendProcessing
    || messagesState.pendingRequests.size > 0;

  if (nextIsProcessing && !messagesState.isProcessing) {
    messagesState.thinkingStartAt = Date.now();
  } else if (!nextIsProcessing && messagesState.isProcessing) {
    messagesState.thinkingStartAt = null;
  }

  messagesState.isProcessing = nextIsProcessing;
}

function timelineHasStreamingMessage(): boolean {
  return ensureArray<TimelineProjectionArtifact>(messagesState.canonicalTimelineProjection?.artifacts)
    .some((artifact) => artifact.message?.isStreaming === true);
}

export function hasActiveLocalTimelineTurn(): boolean {
  // 是否存在尚未结束的本地轮次：等价于 isProcessing 或 timeline 中仍有流式消息。
  // isProcessing 已经覆盖 backendProcessing + pendingRequests 单一事实源。
  return messagesState.isProcessing || timelineHasStreamingMessage();
}

export function markMessageActive(id: string) {
  if (!id) return;
  if (!messagesState.activeMessageIds.has(id)) {
    const next = new Set(messagesState.activeMessageIds);
    next.add(id);
    messagesState.activeMessageIds = next;
    saveCurrentExecutionProjection();
  }
}

export function markMessageComplete(id: string) {
  if (!id) return;
  if (messagesState.activeMessageIds.has(id)) {
    const next = new Set(messagesState.activeMessageIds);
    next.delete(id);
    messagesState.activeMessageIds = next;
    saveCurrentExecutionProjection();
  }
  clearRetryRuntime(id);
}

export function beginLocalTurnSubmission(input: {
  requestId: string;
  sessionId?: string | null;
  placeholderMessageId: string;
  startedAt: number;
  message: Message;
  workspaceId?: string | null;
  workspacePath?: string;
  source?: string;
  agent?: string;
}): void {
  const requestId = input.requestId.trim();
  const placeholderMessageId = input.placeholderMessageId.trim();
  const messageId = input.message.id.trim();
  if (!requestId || !placeholderMessageId || !messageId) {
    throw new Error('本地轮次提交缺少 requestId、messageId 或 placeholderMessageId');
  }
  const sessionId = executionSessionKey(input.sessionId ?? messagesState.currentSessionId);
  const nextSubmissions = new Map(localTurnSubmissionProjections);
  nextSubmissions.set(requestId, {
    requestId,
    sessionId,
    status: 'submitting',
    workspaceId: normalizeWorkspaceId(input.workspaceId ?? messagesState.currentWorkspaceId),
    workspacePath: typeof input.workspacePath === 'string'
      ? input.workspacePath.trim()
      : messagesState.currentWorkspacePath.trim(),
    placeholderMessageId,
    message: {
      ...input.message,
      id: messageId,
      metadata: {
        ...(input.message.metadata || {}),
        requestId,
        localSubmission: true,
      },
    },
  });
  localTurnSubmissionProjections = nextSubmissions;
  forgetTerminalRequest(sessionId, requestId);

  messagesState.turnStage = 'pending';
  messagesState.pendingRequests = new Set([...messagesState.pendingRequests, requestId]);
  messagesState.activeMessageIds = new Set([
    ...messagesState.activeMessageIds,
    placeholderMessageId,
  ]);
  messagesState.processingActor = {
    source: (input.source || 'orchestrator') as ProcessingActor['source'],
    agent: (input.agent || input.source || 'orchestrator') as ProcessingActor['agent'],
  };
  messagesState.thinkingStartAt = Number.isFinite(input.startedAt) && input.startedAt > 0
    ? input.startedAt
    : Date.now();
  messagesState.isProcessing = true;
  saveCurrentExecutionProjection();
}

export function getLocalTurnSubmissionRenderItems(
  sessionId: string | null | undefined = messagesState.currentSessionId,
): TimelineRenderItem[] {
  const sessionKey = executionSessionKey(sessionId);
  return [...localTurnSubmissionProjections.values()]
    .filter((submission) => submission.sessionId === sessionKey)
    .sort((left, right) => (
      left.message.timestamp - right.message.timestamp
      || left.requestId.localeCompare(right.requestId)
    ))
    .map((submission) => ({
      key: `local-submission:${submission.requestId}`,
      message: submission.message,
      ...(submission.sessionId === '__draft__' ? {} : { sessionId: submission.sessionId }),
      ...(submission.workspaceId ? { workspaceId: submission.workspaceId } : {}),
      ...(submission.workspacePath ? { workspacePath: submission.workspacePath } : {}),
    }));
}

export function removeLocalTurnSubmission(requestId: string): void {
  const normalizedRequestId = requestId.trim();
  if (!normalizedRequestId) return;
  const submission = localTurnSubmissionProjections.get(normalizedRequestId);
  if (!submission) return;
  const next = new Map(localTurnSubmissionProjections);
  next.delete(normalizedRequestId);
  localTurnSubmissionProjections = next;
  if (submission.sessionId === executionSessionKey(messagesState.currentSessionId)) {
    const activeMessageIds = new Set(messagesState.activeMessageIds);
    activeMessageIds.delete(submission.placeholderMessageId);
    messagesState.activeMessageIds = activeMessageIds;
  }
}

export function markLocalTurnSubmissionQueued(
  requestId: string,
  sessionId: string,
): boolean {
  const normalizedRequestId = requestId.trim();
  const normalizedSessionId = sessionId.trim();
  if (
    !normalizedRequestId
    || !normalizedSessionId
    || normalizedSessionId === '__draft__'
    || !requestBelongsToSession(normalizedSessionId, normalizedRequestId)
  ) {
    return false;
  }

  const submission = localTurnSubmissionProjections.get(normalizedRequestId);
  if (submission && submission.sessionId === normalizedSessionId) {
    const nextSubmissions = new Map(localTurnSubmissionProjections);
    nextSubmissions.set(normalizedRequestId, {
      ...submission,
      status: 'queued',
      message: {
        ...submission.message,
        metadata: {
          ...(submission.message.metadata || {}),
          sendingAnimation: false,
          queuedSubmission: true,
        },
      },
    });
    localTurnSubmissionProjections = nextSubmissions;
  }

  const binding = removeRequestBindingOnly(normalizedRequestId);
  const currentSessionId = executionSessionKey(messagesState.currentSessionId);
  if (normalizedSessionId !== currentSessionId) {
    updateStoredExecutionProjection(normalizedSessionId, (projection) => {
      projection.pendingRequests.delete(normalizedRequestId);
      projection.activeMessageIds.delete(
        submission?.placeholderMessageId || binding?.placeholderMessageId || '',
      );
      projection.isProcessing = projection.backendProcessing || projection.pendingRequests.size > 0;
      if (!projection.isProcessing) {
        projection.thinkingStartAt = null;
        projection.turnStage = 'done';
      }
      return projection;
    });
    return true;
  }

  const pendingRequests = new Set(messagesState.pendingRequests);
  pendingRequests.delete(normalizedRequestId);
  messagesState.pendingRequests = pendingRequests;
  const activeMessageIds = new Set(messagesState.activeMessageIds);
  activeMessageIds.delete(submission?.placeholderMessageId || binding?.placeholderMessageId || '');
  messagesState.activeMessageIds = activeMessageIds;
  updateProcessingState();
  if (!messagesState.isProcessing) {
    messagesState.turnStage = 'done';
  }
  saveCurrentExecutionProjection();
  return true;
}

export function adoptQueuedTurnSubmissions(
  sessionId: string,
  requestIds: readonly string[],
): void {
  const normalizedSessionId = sessionId.trim();
  if (!normalizedSessionId) return;
  const authoritativeRequestIds = new Set(
    requestIds.map((requestId) => requestId.trim()).filter(Boolean),
  );
  if (authoritativeRequestIds.size === 0) return;
  for (const [requestId, submission] of localTurnSubmissionProjections) {
    if (
      submission.sessionId === normalizedSessionId
      && authoritativeRequestIds.has(requestId)
    ) {
      removeLocalTurnSubmission(requestId);
    }
  }
}

export function adoptCanonicalTurnSubmissions(
  sessionId: string,
  turns: readonly CanonicalTurn[],
): void {
  const sessionKey = executionSessionKey(sessionId);
  const canonicalRequestIds = new Set<string>();
  const terminalRequestIds = new Set<string>();
  for (const turn of turns) {
    if (turn.sessionId !== sessionId) continue;
    const requestId = canonicalTurnRequestId(turn);
    if (!requestId) continue;
    canonicalRequestIds.add(requestId);
    if (isCanonicalTerminalStatus(turn.status)) {
      terminalRequestIds.add(requestId);
    }
  }
  if (canonicalRequestIds.size === 0) return;
  for (const [requestId, submission] of localTurnSubmissionProjections) {
    if (submission.sessionId === sessionKey && canonicalRequestIds.has(requestId)) {
      removeLocalTurnSubmission(requestId);
    }
  }
  for (const requestId of terminalRequestIds) {
    settleTerminalTurn({ sessionId, requestId });
  }
}

export function clearPendingRequest(id: string) {
  if (!id) return;
  const binding = requestBindings.get(id);
  const localSubmission = localTurnSubmissionProjections.get(id);
  const ownerSessionId = binding?.sessionId?.trim()
    || localSubmission?.sessionId
    || messagesState.currentSessionId?.trim()
    || '__draft__';
  removeLocalTurnSubmission(id);
  if (ownerSessionId !== (messagesState.currentSessionId?.trim() || '__draft__')) {
    updateStoredExecutionProjection(ownerSessionId, (projection) => {
      projection.pendingRequests.delete(id);
      projection.activeMessageIds.delete(binding?.placeholderMessageId || '');
      projection.isProcessing = projection.backendProcessing || projection.pendingRequests.size > 0;
      if (!projection.isProcessing) {
        projection.thinkingStartAt = null;
        projection.turnStage = 'done';
      }
      return projection;
    });
    return;
  }
  if (messagesState.pendingRequests.has(id)) {
    const next = new Set(messagesState.pendingRequests);
    next.delete(id);
    messagesState.pendingRequests = next;
    updateProcessingState();
    if (!messagesState.isProcessing) {
      messagesState.turnStage = 'done';
    }
    saveCurrentExecutionProjection();
  }
}

/**
 * 捕获指定 session 在某个异步操作开始时的 pending request 集合。
 * 调用方必须把返回值当作不可变代际快照使用，不能在异步完成时重新读取全局 pending 集合。
 */
export function listPendingRequestIdsForSession(
  sessionId: string | null | undefined,
): string[] {
  const sessionKey = executionSessionKey(sessionId);
  const currentSessionKey = executionSessionKey(messagesState.currentSessionId);
  const pendingRequests = currentSessionKey === sessionKey
    ? messagesState.pendingRequests
    : sessionExecutionProjections.get(sessionKey)?.pendingRequests;
  return pendingRequests ? [...pendingRequests] : [];
}

/**
 * 仅终结调用方事先捕获的 request。旧异步操作完成后不得重新扫描并清理新 request。
 */
export function settlePendingRequestSnapshot(input: {
  sessionId: string | null | undefined;
  requestIds: readonly string[];
}): void {
  const sessionKey = executionSessionKey(input.sessionId);
  for (const requestId of input.requestIds) {
    const normalizedRequestId = requestId.trim();
    if (!normalizedRequestId) continue;
    if (sessionKey === '__draft__') {
      clearRequestBinding(normalizedRequestId);
      continue;
    }
    settleTerminalTurn({
      sessionId: sessionKey,
      requestId: normalizedRequestId,
    });
  }
}

export function settleProcessingAfterResponseCompletion() {
  if (messagesState.backendProcessing || messagesState.pendingRequests.size > 0) {
    return;
  }
  messagesState.turnStage = 'done';
  updateProcessingState();
}

export function settleAuthoritativeIdleState() {
  messagesState.backendProcessing = false;
  messagesState.turnStage = 'done';
  messagesState.pendingRequests = new Set();
  messagesState.activeMessageIds = new Set();
  messagesState.thinkingStartAt = null;
  updateProcessingState();
  saveCurrentExecutionProjection();
}

export function settleTerminalTurn(input: {
  sessionId: string;
  requestId: string;
}): boolean {
  const sessionId = input.sessionId.trim();
  const requestId = input.requestId.trim();
  if (
    !sessionId
    || sessionId === '__draft__'
    || !requestId
    || !requestBelongsToSession(sessionId, requestId)
  ) {
    return false;
  }
  if (terminalRequestIdsBySession.get(sessionId)?.has(requestId)) {
    return true;
  }

  const currentSessionId = executionSessionKey(messagesState.currentSessionId);
  const binding = requestBindings.get(requestId);
  const submission = localTurnSubmissionProjections.get(requestId);
  rememberTerminalRequest(sessionId, requestId);
  removeRequestBindingOnly(requestId);
  removeLocalTurnSubmission(requestId);

  if (sessionId !== currentSessionId) {
    updateStoredExecutionProjection(sessionId, (projection) => {
      projection.pendingRequests.delete(requestId);
      projection.activeMessageIds.delete(
        submission?.placeholderMessageId || binding?.placeholderMessageId || '',
      );
      if (projection.pendingRequests.size === 0) {
        projection.backendProcessing = false;
      }
      projection.isProcessing = projection.backendProcessing || projection.pendingRequests.size > 0;
      if (!projection.isProcessing) {
        projection.thinkingStartAt = null;
        projection.turnStage = 'done';
      }
      return projection;
    });
    return true;
  }

  const pendingRequests = new Set(messagesState.pendingRequests);
  pendingRequests.delete(requestId);
  messagesState.pendingRequests = pendingRequests;
  const activeMessageIds = new Set(messagesState.activeMessageIds);
  activeMessageIds.delete(submission?.placeholderMessageId || binding?.placeholderMessageId || '');
  messagesState.activeMessageIds = activeMessageIds;
  if (messagesState.pendingRequests.size === 0) {
    messagesState.backendProcessing = false;
  }
  updateProcessingState();
  if (!messagesState.isProcessing) {
    messagesState.turnStage = 'done';
    messagesState.thinkingStartAt = null;
  }
  saveCurrentExecutionProjection();
  return true;
}

export function clearProcessingState() {
  messagesState.backendProcessing = false;
  messagesState.turnStage = null;
  messagesState.activeMessageIds = new Set();
  messagesState.pendingRequests = new Set();
  clearAllRetryRuntime();
  updateProcessingState();
  saveCurrentExecutionProjection();
}

/** 获取后端处理状态（用于时序判断） */
export function getBackendProcessing(): boolean {
  return messagesState.backendProcessing;
}

export function clearPendingInteractions() {
  const currentSessionId = messagesState.currentSessionId?.trim() || '__draft__';
  const nextBindings = new Map(requestBindings);
  for (const [requestId, binding] of requestBindings) {
    const ownerSessionId = binding.sessionId?.trim() || currentSessionId;
    if (ownerSessionId !== currentSessionId) {
      continue;
    }
    if (binding.timeoutId) {
      clearTimeout(binding.timeoutId);
    }
    nextBindings.delete(requestId);
  }
  requestBindings = nextBindings;
  const nextSubmissions = new Map(localTurnSubmissionProjections);
  for (const [requestId, submission] of nextSubmissions) {
    if (submission.sessionId === currentSessionId) {
      nextSubmissions.delete(requestId);
    }
  }
  localTurnSubmissionProjections = nextSubmissions;
  messagesState.pendingRequests = new Set();
  messagesState.activeMessageIds = new Set();
  messagesState.backendProcessing = false;
  messagesState.turnStage = null;
  messagesState.isProcessing = false;
  messagesState.thinkingStartAt = null;
  saveCurrentExecutionProjection(currentSessionId);
}

function recomputeUnreadNotificationCount() {
  unreadNotificationCount = notifications.filter((n) => n.countUnread && !n.read).length;
}

function resolveNotificationWorkspaceId(workspaceId: string | null | undefined): string {
  return typeof workspaceId === 'string' ? workspaceId.trim() : '';
}

function getCurrentNotificationWorkspaceId(): string | null {
  const workspaceId = resolveNotificationWorkspaceId(messagesState.currentWorkspaceId);
  return workspaceId || null;
}

function createNotificationContextKey(
  workspaceId: string | null | undefined,
  sessionId: string | null | undefined,
): string {
  const normalizedWorkspaceId = resolveNotificationWorkspaceId(workspaceId);
  const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
  if (!normalizedWorkspaceId && !normalizedSessionId) {
    return '';
  }
  return `${normalizedWorkspaceId || 'personal'}\u0000${normalizedSessionId || '*'}`;
}

function notificationContextMatchesCurrent(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined,
): boolean {
  const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
  const currentSessionId = typeof messagesState.currentSessionId === 'string'
    ? messagesState.currentSessionId.trim()
    : '';
  return (resolveNotificationWorkspaceId(workspaceId) || null) === getCurrentNotificationWorkspaceId()
    && normalizedSessionId === currentSessionId;
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
  const workspaceId = getCurrentNotificationWorkspaceId();
  const sessionId = typeof messagesState.currentSessionId === 'string'
    ? messagesState.currentSessionId.trim()
    : '';
  if (!workspaceId && !sessionId) {
    return null;
  }
  const workspacePath = workspaceId && typeof messagesState.currentWorkspacePath === 'string'
    ? messagesState.currentWorkspacePath.trim()
    : '';
  return {
    ...(workspaceId ? { workspaceId } : {}),
    ...(workspacePath ? { workspacePath } : {}),
    ...(sessionId ? { sessionId } : {}),
  };
}

function applyNotificationList(nextList: Notification[]): Notification[] {
  const trimmed = nextList.slice(0, MAX_NOTIFICATIONS_PER_CONTEXT);
  notifications = trimmed;
  recomputeUnreadNotificationCount();
  return trimmed;
}

function syncNotificationsFromContext(sessionId: string | null | undefined): void {
  const scopeKey = createNotificationContextKey(messagesState.currentWorkspaceId, sessionId);
  const list = scopeKey ? ensureArray<Notification>(notificationsByContext[scopeKey]) : [];
  applyNotificationList(list);
}

function replaceNotificationContextList(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined,
  nextList: Notification[],
): void {
  const scopeKey = createNotificationContextKey(workspaceId, sessionId);
  if (!scopeKey) {
    return;
  }
  const next = nextList.slice(0, MAX_NOTIFICATIONS_PER_CONTEXT);
  notificationsByContext = {
    ...notificationsByContext,
    [scopeKey]: next,
  };
}

// 右下角同时可见的 toast 上限，防止密集通知堆积遮挡主阅读区
const MAX_VISIBLE_TOASTS = 5;

export function addToast(type: string, message: string, title?: string, options?: ToastOptions) {
  if (!options?.forceVisible && !shouldDisplayToast(type)) {
    return;
  }
  const id = `toast_${Date.now()}_${Math.random().toString(36).slice(2, 7)}`;
  const toast: ToastRecord = {
    id,
    type,
    title,
    message,
    source: options?.source,
    actionRequired: options?.actionRequired,
    duration: options?.duration,
  };
  const duplicateIndex = toasts.findIndex((item) => (
    item.type === toast.type
    && item.title === toast.title
    && item.message === toast.message
    && item.source === toast.source
  ));
  const baseToasts = duplicateIndex >= 0
    ? toasts.filter((_, index) => index !== duplicateIndex)
    : toasts;
  let nextToasts = [...baseToasts, toast];
  while (nextToasts.length > MAX_VISIBLE_TOASTS) {
    const discardIndex = nextToasts.findIndex((item) => !item.actionRequired);
    if (discardIndex >= 0) {
      nextToasts.splice(discardIndex, 1);
    } else {
      nextToasts.shift();
    }
  }
  toasts = nextToasts;
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

export function loadNotifications() {
  const scope = getCurrentNotificationOperationScope();
  if (!scope) {
    return;
  }
  vscode.postMessage({ type: 'loadNotifications', ...scope });
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

export function resolveNotification(id: string) {
  const normalizedId = typeof id === 'string' ? id.trim() : '';
  if (!normalizedId) {
    return;
  }
  const scope = getCurrentNotificationOperationScope();
  if (!scope) {
    return;
  }
  vscode.postMessage({ type: 'resolveNotification', notificationId: normalizedId, ...scope });
}

export function applyNotificationsSnapshot(
  sessionId: string | null | undefined,
  rawNotifications: { records?: unknown[] } | unknown,
  workspaceId?: string | null,
): void {
  if (!notificationContextMatchesCurrent(sessionId, workspaceId)) {
    return;
  }
  const normalized = normalizeIncidentRecords(
    rawNotifications && typeof rawNotifications === 'object'
      ? (rawNotifications as { records?: unknown }).records
      : undefined,
  );
  replaceNotificationContextList(sessionId, workspaceId, normalized);
  applyNotificationList(normalized);
}

export function applyNotificationsStatus(rawStatus: unknown): void {
  if (!rawStatus || typeof rawStatus !== 'object' || Array.isArray(rawStatus)) {
    return;
  }
  const status = rawStatus as Record<string, unknown>;
  const statusSessionId = typeof status.sessionId === 'string' ? status.sessionId.trim() : '';
  const statusWorkspaceId = resolveNotificationWorkspaceId(
    typeof status.workspaceId === 'string' ? status.workspaceId : null,
  );
  if (!notificationContextMatchesCurrent(statusSessionId, statusWorkspaceId)) {
    return;
  }
  const operation = typeof status.operation === 'string'
    && ['load', 'report', 'mark-read', 'clear', 'resolve', 'remove'].includes(status.operation)
    ? status.operation as NotificationCenterOperation
    : null;
  messagesState.notificationCenter = {
    isLoading: status.isLoading === true,
    operation,
    error: typeof status.error === 'string' && status.error.trim()
      ? 'operation_failed'
      : null,
    updatedAt: typeof status.updatedAt === 'number' && Number.isFinite(status.updatedAt)
      ? status.updatedAt
      : Date.now(),
  };
}

export function getActiveInteractionType(): string | null {
  const currentSessionId = messagesState.currentSessionId?.trim() || '';
  if (
    currentSessionId
    && toolApprovalState.sessionId === currentSessionId
    && toolApprovalState.hydrated
  ) {
    return hasPendingToolApproval(currentSessionId) ? 'tool_approval' : null;
  }
  return null;
}

export function getTimelineProjectionMessageById(messageId: string): Message | undefined {
  const normalizedId = typeof messageId === 'string' ? messageId.trim() : '';
  if (!normalizedId) {
    return undefined;
  }
  const artifacts = ensureArray<TimelineProjectionArtifact>(messagesState.canonicalTimelineProjection?.artifacts);
  const matched = artifacts.find((artifact) => (
    artifact.artifactId === normalizedId
    || artifact.message?.id === normalizedId
    || ensureArray<string>(artifact.messageIds).includes(normalizedId)
  ));
  return matched?.message;
}

export function beginTurnEditing(draft: TurnEditingDraft): void {
  const sessionId = normalizeSessionId(draft.sessionId);
  const turnId = draft.turnId.trim();
  const messageId = draft.messageId.trim();
  if (!sessionId || !turnId || !messageId || sessionId !== normalizeSessionId(messagesState.currentSessionId)) {
    return;
  }
  messagesState.editingTurn = {
    sessionId,
    turnId,
    messageId,
    text: draft.text,
    images: draft.images.map((image) => ({ ...image })),
    contextReferences: draft.contextReferences.map((reference) => ({ ...reference })),
    browserAnnotationRefs: draft.browserAnnotationRefs.map((reference) => ({ ...reference })),
    browserNodeSelections: draft.browserNodeSelections.map((selection) => ({
      ...selection,
      attributes: { ...selection.attributes },
      bounds: selection.bounds ? { ...selection.bounds } : selection.bounds,
    })),
    skillName: draft.skillName?.trim() || null,
    goalMode: draft.goalMode === true,
  };
}

export function cancelTurnEditing(): void {
  messagesState.editingTurn = null;
}

export function completeTurnEditing(replacedTurnId: string): void {
  const normalizedTurnId = replacedTurnId.trim();
  if (!normalizedTurnId || messagesState.editingTurn?.turnId !== normalizedTurnId) {
    return;
  }
  messagesState.editingTurn = null;
}

// 清空所有消息（用于会话切换/新建）
export function clearAllMessages(options: {
  persist?: boolean;
  resetTimelineView?: boolean;
  resetPanelState?: boolean;
  /** 切换会话时保留后台请求的执行态，由 setCurrentSessionId 负责切换投影。 */
  preserveExecutionState?: boolean;
} = {}) {
  captureCurrentSessionViewState();
  if (options.resetTimelineView !== false) {
    messagesState.canonicalTimelineProjection = null;
  }
  messagesState.orchestratorRuntimeState = null;
  messagesState.sessionHistory = createEmptySessionHistoryState(
    messagesState.currentWorkspaceId,
    messagesState.currentWorkspacePath,
    messagesState.currentSessionId,
    messagesState.sessionHistory.revision + 1,
  );
  messagesState.queuedMessages = [];
  messagesState.editingTurn = null;
  messagesState.messageJump = {
    messageId: null,
    nonce: messagesState.messageJump.nonce,
  };
  if (options.preserveExecutionState !== true) {
    clearPendingInteractions();
    clearProcessingState();
  }
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
    workspacePath?: string | null;
    canonicalHasMoreBefore?: boolean;
    canonicalBeforeCursor?: string | null;
    historyLoadStatus?: SessionHistoryLoadStatus;
    preserveLoadedWindow?: boolean;
    expectedRevision?: number;
    expectedCanonicalBeforeCursor?: string | null;
  },
): void {
  const normalizedSessionId = normalizeSessionId(sessionId);
  const current = messagesState.sessionHistory;
  if (
    input.expectedRevision !== undefined
    && current.revision !== input.expectedRevision
  ) {
    return;
  }
  if (
    input.expectedCanonicalBeforeCursor !== undefined
    && current.canonicalBeforeCursor !== normalizeHistoryCursor(input.expectedCanonicalBeforeCursor)
  ) {
    return;
  }
  if (!normalizedSessionId) {
    messagesState.sessionHistory = createEmptySessionHistoryState(
      input.workspaceId,
      input.workspacePath ?? messagesState.currentWorkspacePath,
      null,
      current.revision + 1,
    );
    return;
  }
  const normalizedWorkspaceId = input.workspaceId !== undefined
    ? normalizeWorkspaceId(input.workspaceId)
    : normalizeWorkspaceId(messagesState.currentWorkspaceId);
  const normalizedWorkspacePath = input.workspacePath !== undefined
    ? normalizeWorkspacePath(input.workspacePath)
    : normalizeWorkspacePath(messagesState.currentWorkspacePath);
  if (
    current.sessionId !== normalizedSessionId
    || current.workspaceId !== normalizedWorkspaceId
    || current.workspacePath !== normalizedWorkspacePath
  ) {
    const canonicalHasMoreBefore = input.canonicalHasMoreBefore === true;
    const canonicalBeforeCursor = normalizeHistoryCursor(input.canonicalBeforeCursor);
    messagesState.sessionHistory = {
      workspaceId: normalizedWorkspaceId,
      workspacePath: normalizedWorkspacePath,
      sessionId: normalizedSessionId,
      canonicalHasMoreBefore,
      canonicalBeforeCursor,
      historyLoadStatus: resolveHistoryLoadStatus(
        canonicalHasMoreBefore,
        canonicalBeforeCursor,
        input.historyLoadStatus,
        'idle',
      ),
      revision: current.revision + 1,
    };
    return;
  }
  const shouldPreserveLoadedWindow = input.preserveLoadedWindow === true
    && current.sessionId === normalizedSessionId
    && current.workspaceId === normalizedWorkspaceId
    && current.workspacePath === normalizedWorkspacePath
    && (
      current.canonicalBeforeCursor !== null
      || current.canonicalHasMoreBefore
    );
  const canonicalHasMoreBefore = shouldPreserveLoadedWindow
    ? current.canonicalHasMoreBefore
    : (input.canonicalHasMoreBefore ?? current.canonicalHasMoreBefore);
  const canonicalBeforeCursor = shouldPreserveLoadedWindow
    ? current.canonicalBeforeCursor
    : (input.canonicalBeforeCursor !== undefined
      ? normalizeHistoryCursor(input.canonicalBeforeCursor)
      : current.canonicalBeforeCursor);
  const preserveFailedLoad = shouldPreserveLoadedWindow
    && input.historyLoadStatus === undefined
    && (current.historyLoadStatus === 'error' || current.historyLoadStatus === 'no-progress');
  const preserveInFlightLoad = shouldPreserveLoadedWindow
    && input.historyLoadStatus === undefined
    && current.historyLoadStatus === 'loading';
  const hasWindowInput = input.canonicalHasMoreBefore !== undefined
    || input.canonicalBeforeCursor !== undefined;
  const historyLoadStatus = preserveFailedLoad || preserveInFlightLoad
    ? current.historyLoadStatus
    : hasWindowInput || input.historyLoadStatus !== undefined
      ? resolveHistoryLoadStatus(
        canonicalHasMoreBefore,
        canonicalBeforeCursor,
        input.historyLoadStatus,
        'idle',
      )
      : current.historyLoadStatus;
  const nextState: SessionHistoryState = {
    workspaceId: normalizedWorkspaceId,
    workspacePath: normalizedWorkspacePath,
    sessionId: normalizedSessionId,
    canonicalHasMoreBefore,
    canonicalBeforeCursor,
    historyLoadStatus,
    revision: current.revision,
  };
  const loadedWindowChanged = nextState.canonicalHasMoreBefore !== current.canonicalHasMoreBefore
    || nextState.canonicalBeforeCursor !== current.canonicalBeforeCursor;
  messagesState.sessionHistory = loadedWindowChanged
    ? { ...nextState, revision: current.revision + 1 }
    : nextState;
}

function normalizeHistoryCursor(value: string | null | undefined): string | null {
  return typeof value === 'string' && value.trim() ? value.trim() : null;
}

function resolveHistoryLoadStatus(
  canonicalHasMoreBefore: boolean,
  canonicalBeforeCursor: string | null,
  requestedStatus: SessionHistoryLoadStatus | undefined,
  fallbackStatus: SessionHistoryLoadStatus,
): SessionHistoryLoadStatus {
  if (requestedStatus === 'loading' || requestedStatus === 'error' || requestedStatus === 'no-progress') {
    return requestedStatus;
  }
  if (canonicalHasMoreBefore && !canonicalBeforeCursor) {
    return 'no-progress';
  }
  if (!canonicalHasMoreBefore) {
    return 'exhausted';
  }
  return requestedStatus === 'idle' || fallbackStatus === 'idle' ? 'idle' : fallbackStatus;
}

export interface SessionHistoryPageCommitResult {
  accepted: boolean;
  reason: 'committed' | 'stale' | 'no-progress';
  addedTurnCount: number;
}

function noProgressHistoryPageResult(): SessionHistoryPageCommitResult {
  const current = messagesState.sessionHistory;
  if (current.historyLoadStatus === 'loading') {
    messagesState.sessionHistory = {
      ...current,
      historyLoadStatus: 'no-progress',
      revision: current.revision + 1,
    };
  }
  return { accepted: false, reason: 'no-progress', addedTurnCount: 0 };
}

export function commitOlderSessionHistoryPage(input: {
  sessionId: string;
  workspaceId: string | null;
  workspacePath: string | null;
  revision: number;
  canonicalBeforeCursor: string | null;
  turns: readonly CanonicalTurn[];
  canonicalHasMoreBefore: boolean;
  nextCanonicalBeforeCursor: string | null;
}): SessionHistoryPageCommitResult {
  const sessionId = normalizeSessionId(input.sessionId);
  const workspaceId = normalizeWorkspaceId(input.workspaceId);
  const workspacePath = normalizeWorkspacePath(input.workspacePath);
  const current = messagesState.sessionHistory;
  if (
    !sessionId
    || current.sessionId !== sessionId
    || current.workspaceId !== workspaceId
    || current.workspacePath !== workspacePath
    || current.revision !== input.revision
    || current.canonicalBeforeCursor !== normalizeHistoryCursor(input.canonicalBeforeCursor)
    || current.historyLoadStatus !== 'loading'
    || normalizeSessionId(messagesState.currentSessionId) !== sessionId
    || normalizeWorkspaceId(messagesState.currentWorkspaceId) !== workspaceId
    || normalizeWorkspacePath(messagesState.currentWorkspacePath) !== workspacePath
  ) {
    return { accepted: false, reason: 'stale', addedTurnCount: 0 };
  }

  const nextCanonicalBeforeCursor = normalizeHistoryCursor(input.nextCanonicalBeforeCursor);
  const canonicalHasMoreBefore = input.canonicalHasMoreBefore === true;
  // /messages 的 canonical cursor 始终指向本页最早 turn，即使本页已经是最后一页；
  // 非空页缺少 cursor 时，不能把它当作已耗尽，否则下一次请求会重复同一页。
  if (!nextCanonicalBeforeCursor || !Array.isArray(input.turns) || input.turns.length === 0) {
    return noProgressHistoryPageResult();
  }
  const pageCommit = prependCanonicalTurns(sessionId, [...input.turns], {
    expectedBeforeCursor: normalizeHistoryCursor(input.canonicalBeforeCursor),
    nextBeforeCursor: nextCanonicalBeforeCursor,
  });
  if (!pageCommit || pageCommit.addedTurnCount < 1 || !setCanonicalTimelineProjection(pageCommit.projection)) {
    return noProgressHistoryPageResult();
  }

  messagesState.sessionHistory = {
    ...current,
    canonicalHasMoreBefore,
    canonicalBeforeCursor: nextCanonicalBeforeCursor,
    historyLoadStatus: canonicalHasMoreBefore ? 'idle' : 'exhausted',
    revision: current.revision + 1,
  };
  return {
    accepted: true,
    reason: 'committed',
    addedTurnCount: pageCommit.addedTurnCount,
  };
}

export function setCanonicalTimelineProjection(projection: SessionTimelineProjection): boolean {
  const projectionSessionId = normalizeSessionId(projection.sessionId);
  const currentSessionId = normalizeSessionId(messagesState.currentSessionId);
  if (!projectionSessionId || !currentSessionId || projectionSessionId !== currentSessionId) {
    console.warn('[messages-store] 忽略非当前会话的 canonical timeline projection', {
      projectionSessionId,
      currentSessionId,
    });
    return false;
  }
  messagesState.canonicalTimelineProjection = projection;
  upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(projection.sessionId));
  return true;
}


// 导出状态初始化
export function initializeState() {
  clearAllRetryRuntime();
  resetPanelScrollRuntimeState();
  sessionViewStateByScope = {};
  const persisted = vscode.getState<WebviewPersistedState>();
  if (persisted) {
    // Tab 状态不持久化，每次打开都默认显示主对话 tab
    messagesState.currentTopTab = 'thread';
    sessionViewStateByScope = normalizePersistedSessionViewStateMap(persisted.sessionViewStateByScope);
    const explicitSessionId = normalizeSessionId(messagesState.currentSessionId);
    const restoredSessionViewState = explicitSessionId
      ? applySessionViewState(explicitSessionId)
      : false;
    if (explicitSessionId && !restoredSessionViewState) {
      resetPanelScrollRuntimeState();
    }
    messagesState.queuedMessages = [];
    notificationsByContext = {};
    messagesState.orchestratorRuntimeState = null;
    syncNotificationsFromContext(messagesState.currentSessionId);

    // 启动恢复：workspace/session 列表与激活会话只以后端 bootstrap 为唯一真相源。
    // 浏览器本地持久化只保留按 session 归档的轻量视图状态，
    // 避免旧 workspace 的本地列表在首屏污染当前工作区。
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
  next.set(binding.requestId, {
    ...binding,
    sessionId: binding.sessionId?.trim() || messagesState.currentSessionId?.trim() || '__draft__',
  });
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
  if (!requestId) return;
  clearPendingRequest(requestId);
  removeRequestBindingOnly(requestId);
}

/**
 * 根据占位消息 ID 查找请求绑定
 */
/**
 * 清除所有请求绑定（会话切换时使用）
 */
export function clearAllRequestBindings(): void {
  for (const binding of requestBindings.values()) {
    if (binding.timeoutId) {
      clearTimeout(binding.timeoutId);
    }
  }
  requestBindings = new Map();
  localTurnSubmissionProjections = new Map();
  terminalRequestIdsBySession = new Map();
  sessionExecutionProjections = new Map();
  messagesState.pendingRequests = new Set();
  messagesState.activeMessageIds = new Set();
  messagesState.backendProcessing = false;
  messagesState.thinkingStartAt = null;
  updateProcessingState();
}
