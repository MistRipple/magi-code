/**
 * 消息状态管理 - Svelte 5 Runes
 * 使用细粒度响应式实现高效的流式更新
 */

import type {
  Message,
  TimelineProjectionArtifact,
  TimelineProjectionRenderEntry,
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
  type NormalizedIncidentRecord,
} from '../lib/notification-policy';

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
  resolveTimelineSortTimestamp,
  resolveTimelineTurnOrderSeqFromMetadata,
  resolveTimelineVersionFromMetadata,
} from '../shared/timeline-ordering';
import {
  resolveTimelinePresentationKind,
} from '../shared/timeline-presentation';
import {
  normalizeMessagePayload,
} from '../lib/message-payload';
import {
  clearCanonicalSessionTurns,
  rebindCanonicalSessionTurns,
} from './turn-store.svelte';
import type { SettingsBootstrapSnapshot } from '../shared/settings-bootstrap';
import type { RoleTemplate } from '../shared/types/role-templates';
import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';
import { shouldUseHostProxyTransport } from '../shared/transport';

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
  workspaceId: string;
  workspacePath: string;
  sessionId?: string;
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
  sessions: [] as Session[],
  currentSessionId: null as string | null,
  sessionHydrating: false,
  sessionHistory: {
    workspaceId: null as string | null,
    sessionId: null as string | null,
    hasMoreBefore: false,
    beforeCursor: null as string | null,
    isLoadingBefore: false,
  },
  queuedMessages: [] as QueuedMessage[],
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
  edits: [] as Edit[],
  changeMutationStatus: null as ChangeMutationStatus | null,
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

const IS_HOSTED_WEBVIEW = shouldUseHostProxyTransport();

const MAX_PERSISTED_ARRAY_LENGTH = 10000;
const WEBVIEW_STATE_SAVE_DEBOUNCE_MS = 120;
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

function normalizeWorkspaceId(value: string | null | undefined): string | null {
  const workspaceId = typeof value === 'string' ? value.trim() : '';
  return workspaceId || null;
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
    }));
}

function normalizePersistedQueuedMessageMap(value: unknown): Record<string, QueuedMessage[]> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return {};
  }
  const normalized: Record<string, QueuedMessage[]> = {};
  let count = 0;
  for (const [rawScopeKey, rawMessages] of Object.entries(value as Record<string, unknown>)) {
    if (count >= MAX_PERSISTED_ARRAY_LENGTH) {
      break;
    }
    const scopeKey = typeof rawScopeKey === 'string' ? rawScopeKey.trim() : '';
    if (!scopeKey) {
      continue;
    }
    const queued = normalizeQueuedMessageList(rawMessages);
    if (queued.length === 0) {
      continue;
    }
    normalized[scopeKey] = queued;
    count += 1;
  }
  return normalized;
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
let sessionQueuedMessagesByScope = $state<Record<string, QueuedMessage[]>>({});
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

function resolveMessageMetadataRecord(message: Pick<Message, 'metadata'> | undefined): Record<string, unknown> | undefined {
  return message?.metadata && typeof message.metadata === 'object'
    ? message.metadata as Record<string, unknown>
    : undefined;
}

function resolveMessageSortTimestamp(message: Pick<Message, 'timestamp' | 'metadata' | 'type'>): number {
  return resolveTimelineSortTimestamp(message.timestamp, resolveMessageMetadataRecord(message));
}

function normalizeProjectionRestoredMessage(message: Message): Message {
  return normalizeMessagePayload(message, '[MessagesStore] 投影消息');
}

function resolveTimelineCardId(message: Message): string | undefined {
  const rawCardId = typeof message.metadata?.cardId === 'string' ? message.metadata.cardId.trim() : '';
  return rawCardId || undefined;
}

function normalizeMetadataString(metadata: Record<string, unknown> | undefined, key: string): string {
  const value = metadata?.[key];
  return typeof value === 'string' ? value.trim() : '';
}

function resolveSidechainTaskId(message: Message): string | undefined {
  const metadata = resolveMessageMetadataRecord(message);
  const taskId = normalizeMetadataString(metadata, 'taskId');
  if (!taskId) {
    return undefined;
  }
  const roleId = normalizeMetadataString(metadata, 'roleId');
  const workerId = normalizeMetadataString(metadata, 'workerId');
  if ((roleId && roleId !== 'orchestrator') || workerId) {
    return taskId;
  }
  return undefined;
}

function resolveProjectionArtifactKind(message: Message): TimelineProjectionArtifact['kind'] {
  return resolveTimelinePresentationKind(message);
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

function getMessageBlockSeq(message: Pick<Message, 'metadata'> | undefined): number {
  return resolveTimelineBlockSeqFromMetadata(resolveMessageMetadataRecord(message));
}

interface LocalProjectionFlatRenderEntry {
  entryId: string;
  artifactId: string;
  message: Message;
  timestamp: number;
  turnOrderSeq: number;
  itemSeq: number;
  anchorEventSeq: number;
  blockSeq: number;
  cardStreamSeq: number;
}

function renderEntryToOrderInput(entry: LocalProjectionFlatRenderEntry): TimelineSemanticOrderInput {
  return {
    turnOrderSeq: entry.turnOrderSeq,
    itemSeq: entry.itemSeq,
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

function buildProjectionRenderEntriesFromArtifacts(
  artifacts: TimelineProjectionArtifact[],
): TimelineProjectionRenderEntry[] {
  const flatEntries: LocalProjectionFlatRenderEntry[] = [];

  for (const artifact of artifacts) {
    if (artifact.taskId) {
      // 代理 artifacts 仅在 RightPane agent run tab 内按 metadata.taskId 过滤呈现，主时间线不收纳
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
        kind: artifact.kind || resolveProjectionArtifactKind(message),
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
        taskId: artifact.taskId || resolveSidechainTaskId(message),
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
    threadRenderEntries: buildProjectionRenderEntriesFromArtifacts(artifacts),
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
      blockSeq: resolveTimelineBlockSeqFromMetadata(leftMetadata),
      displayOrder: left.displayOrder || 0,
    },
    {
      turnOrderSeq: resolveTimelineTurnOrderSeqFromMetadata(rightMetadata),
      itemSeq: resolveTimelineItemSeqFromMetadata(rightMetadata),
      blockSeq: resolveTimelineBlockSeqFromMetadata(rightMetadata),
      displayOrder: right.displayOrder || 0,
    },
  );
  if (semanticOrder !== 0) {
    return semanticOrder;
  }
  return left.artifactId.localeCompare(right.artifactId);
}

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
    // 后端明确没有活跃处理态：权威 idle 必须收敛本地乐观 pending。
    // 活跃轮次期间的旧 idle 快照由 bootstrap 的 preserveLocalProcessing 调用方拦截。
    messagesState.backendProcessing = false;
    messagesState.pendingRequests = new Set();
    messagesState.activeMessageIds = new Set();
    messagesState.thinkingStartAt = null;
    updateProcessingState();
    return;
  }
  const pendingRequestIds = new Set(snapshot.pendingRequestIds);
  // 防回抬保护：如果在 forced idle 冷却期内，拒绝后端权威状态覆盖
  const lastForcedIdleAt = messagesState.lastForcedIdleAt;
  if (
    lastForcedIdleAt !== null
    && (Date.now() - lastForcedIdleAt) < ANTI_LIFT_BACK_COOLDOWN_MS
    && (snapshot.isProcessing || pendingRequestIds.size > 0)
  ) {
    // 冷却期内只拒绝 running 回抬；idle 快照仍必须继续收敛本地状态。
    if (snapshot.source) {
      setProcessingActor(snapshot.source, snapshot.agent || undefined);
    }
    return;
  }
  const activeMessageIds = snapshot.isProcessing
    ? messagesState.activeMessageIds
    : new Set<string>();
  // 单一事实源：后端权威 isProcessing + 本地乐观 pendingRequests。
  // 不再叠加 runtimeState / canonical projection 推断，避免多路 OR 信号让陈旧状态
  // 把发送按钮卡在"响应中"。
  const nextIsProcessing = snapshot.isProcessing || pendingRequestIds.size > 0;

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
    get changeMutationStatus() { return messagesState.changeMutationStatus; },
    get settingsBootstrapSnapshot() { return messagesState.settingsBootstrapSnapshot; },
    set settingsBootstrapSnapshot(v) { messagesState.settingsBootstrapSnapshot = v; },
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

export function allocateTurnOrderSeq(): number {
  localTimelineTurnOrderSeqCounter = Math.max(
    normalizePositiveSequence(localTimelineTurnOrderSeqCounter),
    maxTimelineTurnOrderSeqFromArtifacts(messagesState.canonicalTimelineProjection?.artifacts),
  );
  localTimelineTurnOrderSeqCounter += 1;
  return localTimelineTurnOrderSeqCounter;
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

function restoreQueuedMessagesForSession(sessionId: string | null | undefined): void {
  const scopeKey = currentSessionScopeKey(sessionId);
  messagesState.queuedMessages = scopeKey
    ? normalizeQueuedMessageList(sessionQueuedMessagesByScope[scopeKey])
    : [];
}

function pruneSessionViewStateByKnownSessions(): void {
  const currentWorkspaceId = normalizeWorkspaceId(messagesState.currentWorkspaceId);
  const knownScopeKeys = new Set<string>();
  for (const session of messagesState.sessions) {
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
  const nextQueuedEntries = Object.entries(sessionQueuedMessagesByScope)
    .filter(([scopeKey, queuedMessages]) => {
      if (queuedMessages.length === 0) {
        return false;
      }
      const firstQueued = queuedMessages[0];
      const queuedWorkspaceId = normalizeWorkspaceId(firstQueued?.workspaceId);
      return queuedWorkspaceId !== currentWorkspaceId || knownScopeKeys.has(scopeKey);
    });
  if (nextQueuedEntries.length !== Object.keys(sessionQueuedMessagesByScope).length) {
    sessionQueuedMessagesByScope = Object.fromEntries(nextQueuedEntries);
  }
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

function resetSessionScopedExecutionState(): void {
  messagesState.edits = [];
  messagesState.orchestratorRuntimeState = null;
  messagesState.backendProcessing = false;
  messagesState.pendingRequests = new Set();
  messagesState.activeMessageIds = new Set();
  messagesState.thinkingStartAt = null;
  messagesState.isProcessing = false;
  messagesState.lastForcedIdleAt = null;
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
      sessionQueuedMessagesByScope,
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
    resetSessionScopedExecutionState();
    // 会话切换时消息内容以后端分页快照为唯一真相源。
    // 本地只恢复滚动/定位等轻量视图状态，避免旧 session 的主线或右侧面板内容短暂残留。
    clearCanonicalSessionTurns(nextSessionId ?? undefined);
    messagesState.canonicalTimelineProjection = null;
    restoredSessionView = applySessionViewState(nextSessionId);
    if (!restoredSessionView) {
      resetPanelScrollRuntimeState();
    }
    restoreQueuedMessagesForSession(nextSessionId);
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
  clearCanonicalSessionTurns(nextSessionId);
  messagesState.currentSessionId = nextSessionId;
  messagesState.sessionHistory = {
    workspaceId: messagesState.currentWorkspaceId,
    sessionId: nextSessionId,
    hasMoreBefore: false,
    beforeCursor: null,
    isLoadingBefore: false,
  };
  resetNotificationCenterStatus();
  syncNotificationsFromContext(nextSessionId);
  saveWebviewState();
  return true;
}

export function adoptAcceptedSessionIdForLocalTurn(
  localSessionId: string | null | undefined,
  acceptedSessionId: string | null | undefined,
): boolean {
  const previousSessionId = normalizeSessionId(localSessionId);
  const nextSessionId = normalizeSessionId(acceptedSessionId);
  if (!previousSessionId || !nextSessionId) {
    return false;
  }
  if (previousSessionId === nextSessionId) {
    return true;
  }
  if (normalizeSessionId(messagesState.currentSessionId) !== previousSessionId) {
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
  const reboundProjection = rebindCanonicalSessionTurns(previousSessionId, nextSessionId);
  if (reboundProjection) {
    messagesState.canonicalTimelineProjection = reboundProjection;
  }
  syncNotificationsFromContext(nextSessionId);
  saveWebviewState();
  return true;
}

export function updateSessions(newSessions: Session[]) {
  const seen = new Set<string>();
  const currentWorkspaceId = normalizeWorkspaceId(messagesState.currentWorkspaceId);
  messagesState.sessions = ensureArray<Session>(newSessions)
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
  pruneSessionViewStateByKnownSessions();
  saveWebviewState();
}

export function setQueuedMessages(newQueuedMessages: QueuedMessage[]) {
  const normalized = normalizeQueuedMessageList(newQueuedMessages);
  messagesState.queuedMessages = normalized;
  const scopeKey = currentSessionScopeKey();
  if (scopeKey) {
    sessionQueuedMessagesByScope = {
      ...sessionQueuedMessagesByScope,
      ...(normalized.length > 0 ? { [scopeKey]: normalized } : {}),
    };
    if (normalized.length === 0 && sessionQueuedMessagesByScope[scopeKey]) {
      const { [scopeKey]: _removed, ...rest } = sessionQueuedMessagesByScope;
      sessionQueuedMessagesByScope = rest;
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
    syncEditsFromAppState(nextState);
    messagesState.appState = nextState;
    messagesState.bootstrapped = true;
  } else {
    syncEditsFromAppState(null);
    messagesState.appState = null;
  }
}

// 防回抬冷却期（ms）：forced idle 后的短暂窗口内，拒绝任何来源的 processing=true
const ANTI_LIFT_BACK_COOLDOWN_MS = 2000;

function updateProcessingState() {
  // 单一事实源：后端权威 backendProcessing + 本地乐观 pendingRequests。
  // 不再叠加 orchestratorRuntimeState / canonical projection / activeMessageIds，
  // 这些都是同一份后端事实的衍生订阅，多路 OR 会让陈旧状态把按钮卡死。
  const nextIsProcessing = messagesState.backendProcessing
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
  }
}

export function markMessageComplete(id: string) {
  if (!id) return;
  if (messagesState.activeMessageIds.has(id)) {
    const next = new Set(messagesState.activeMessageIds);
    next.delete(id);
    messagesState.activeMessageIds = next;
  }
  clearRetryRuntime(id);
}

export function addPendingRequest(id: string, options?: { resetAntiLiftBack?: boolean }) {
  if (!id) return;
  if (options?.resetAntiLiftBack) {
    messagesState.lastForcedIdleAt = null;
  }
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

export function settleProcessingAfterResponseCompletion() {
  // 后端权威发出"已结束"信号时尝试落 idle：只看单一事实源。
  if (messagesState.backendProcessing || messagesState.pendingRequests.size > 0) {
    return;
  }
  messagesState.lastForcedIdleAt = Date.now();
  updateProcessingState();
}

export function settleAuthoritativeIdleState() {
  // 后端权威 idle：直接把单一事实源以及衍生状态全部清零。
  messagesState.backendProcessing = false;
  messagesState.pendingRequests = new Set();
  messagesState.activeMessageIds = new Set();
  messagesState.thinkingStartAt = null;
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
  // 不再在此清 orchestratorRuntimeState：
  // 该函数被「轮次正常结束」事件（processingStateChanged forced idle / canonical 终态）调用，
  // 那时 runtimeState 仍持有「completed / failed / cancelled」的终态语义，需要继续在
  // 顶部状态栏展示，避免出现「面板瞬间消失再被下一个 runtimeState 事件重新挂载」的跳动。
  // runtimeState 的清理生命周期由专属路径负责（session/workspace 切换 + bootstrap 恢复）。
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

function resolveNotificationWorkspaceId(workspaceId: string | null | undefined): string {
  return typeof workspaceId === 'string' ? workspaceId.trim() : '';
}

function getCurrentNotificationWorkspaceId(): string {
  return resolveNotificationWorkspaceId(messagesState.currentWorkspaceId);
}

function createNotificationContextKey(
  workspaceId: string | null | undefined,
  sessionId: string | null | undefined,
): string {
  const normalizedWorkspaceId = resolveNotificationWorkspaceId(workspaceId);
  if (!normalizedWorkspaceId) {
    return '';
  }
  const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
  return `${normalizedWorkspaceId}\u0000${normalizedSessionId || '*'}`;
}

function notificationContextMatchesCurrent(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined,
): boolean {
  const normalizedSessionId = typeof sessionId === 'string' ? sessionId.trim() : '';
  const currentSessionId = typeof messagesState.currentSessionId === 'string'
    ? messagesState.currentSessionId.trim()
    : '';
  return resolveNotificationWorkspaceId(workspaceId) === getCurrentNotificationWorkspaceId()
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
  if (!workspaceId) {
    return null;
  }
  const sessionId = typeof messagesState.currentSessionId === 'string'
    ? messagesState.currentSessionId.trim()
    : '';
  return {
    workspaceId,
    workspacePath: typeof messagesState.currentWorkspacePath === 'string'
      ? messagesState.currentWorkspacePath.trim()
      : '',
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
    messagesState.canonicalTimelineProjection = null;
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

export function setCanonicalTimelineProjection(projection: SessionTimelineProjection): boolean {
  const canonicalProjection = canonicalizeTimelineProjection(projection);
  const projectionSessionId = normalizeSessionId(canonicalProjection.sessionId);
  const currentSessionId = normalizeSessionId(messagesState.currentSessionId);
  if (!projectionSessionId || !currentSessionId || projectionSessionId !== currentSessionId) {
    console.warn('[messages-store] 忽略非当前会话的 canonical timeline projection', {
      projectionSessionId,
      currentSessionId,
    });
    return false;
  }
  localTimelineTurnOrderSeqCounter = Math.max(
    localTimelineTurnOrderSeqCounter,
    maxTimelineTurnOrderSeqFromArtifacts(canonicalProjection.artifacts),
  );
  messagesState.canonicalTimelineProjection = canonicalProjection;
  upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(canonicalProjection.sessionId));
  saveWebviewState();
  return true;
}


// 导出状态初始化
export function initializeState() {
  clearAllRetryRuntime();
  resetPanelScrollRuntimeState();
  sessionViewStateByScope = {};
  sessionQueuedMessagesByScope = {};
  const persisted = vscode.getState<WebviewPersistedState>();
  if (persisted) {
    // Tab 状态不持久化，每次打开都默认显示主对话 tab
    messagesState.currentTopTab = 'thread';
    sessionViewStateByScope = normalizePersistedSessionViewStateMap(persisted.sessionViewStateByScope);
    sessionQueuedMessagesByScope = normalizePersistedQueuedMessageMap(persisted.sessionQueuedMessagesByScope);
    const explicitSessionId = normalizeSessionId(messagesState.currentSessionId);
    const restoredSessionViewState = explicitSessionId
      ? applySessionViewState(explicitSessionId)
      : false;
    if (explicitSessionId && !restoredSessionViewState) {
      resetPanelScrollRuntimeState();
    }
    restoreQueuedMessagesForSession(explicitSessionId);
    notificationsByContext = {};
    messagesState.orchestratorRuntimeState = null;
    syncNotificationsFromContext(messagesState.currentSessionId);

    // 启动恢复：workspace/session 列表与激活会话只以后端 bootstrap 为唯一真相源。
    // 浏览器本地持久化只保留按 session 归档的轻量视图状态，
    // 避免旧 workspace 的本地列表在首屏污染当前工作区。
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
  if (!requestId) return;
  clearPendingRequest(requestId);
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
  if (messagesState.pendingRequests.size > 0) {
    messagesState.pendingRequests = new Set();
    updateProcessingState();
  }
}
