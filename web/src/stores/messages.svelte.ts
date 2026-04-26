/**
 * 消息状态管理 - Svelte 5 Runes
 * 使用细粒度响应式实现高效的流式更新
 */

import type {
  Message,
  AgentOutputs,
  AgentId,
  TimelineExecutionItem,
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
  ContentBlock,
} from '../types/message';
import { vscode } from '../lib/vscode-bridge';
import { ensureArray } from '../lib/utils';

import { deriveWorkerRuntimeMap } from '../lib/worker-panel-state';
import { normalizeWorkerSlot } from '../lib/message-classifier';
import {
  compareTimelineProjectionFreshness,
  shouldPreferRicherAuthoritativeProjection,
} from '../lib/timeline-projection-freshness';
import {
  buildTimelinePanelMessages,
} from '../lib/timeline-render-items';
import {
  compareTimelineSemanticOrder,
  type TimelineSemanticOrderInput,
  resolveTimelineBlockSeqFromMetadata,
  resolveTimelineCardStreamSeqFromMetadata,
  resolveTimelineEventSeqFromMetadata,
  resolveStableTimelinePlacementTimestamp,
  resolveTimelineSortTimestamp,
  resolveTimelineVersionFromMetadata,
} from '../shared/timeline-ordering';
import {
  collectTimelineAliasIds,
  resolveTimelinePresentationKind,
  resolveTimelineWorkerVisibility as resolveSharedTimelineWorkerVisibility,
} from '../shared/timeline-presentation';
import {
  resolveTimelineWorkerId,
} from '../shared/timeline-worker-lifecycle';
import {
  expandRenderableTimelineMessages,
  type TimelineFragmentMessage,
} from '../shared/timeline-message-fragmentation';
import {
  normalizeMessagePayload,
  sanitizeMessagePatch,
} from '../lib/message-payload';
import { mergeFragmentExecutionItems } from '../lib/timeline-execution-item-merge';
import type { SettingsBootstrapSnapshot } from '../shared/settings-bootstrap';
import type { RoleTemplate } from '../shared/types/role-templates';
import type { AgentBinding, ModelEngine } from '../shared/types/registry-types';

interface SettingsRegistrySnapshot {
  roleTemplates: RoleTemplate[];
  registryEngines: ModelEngine[];
  registryAgents: AgentBinding[];
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
  sessions: [] as Session[],
  currentSessionId: null as string | null,
  sessionHistory: {
    sessionId: null as string | null,
    hasMoreBefore: false,
    beforeCursor: null as string | null,
    isLoadingBefore: false,
  },
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
  activePlan: AppState['activePlan'] | null;
}
let sessionExecutionStateBySession = $state<Record<string, PersistedSessionExecutionState>>({});
let sessionQueuedMessagesBySession = $state<Record<string, QueuedMessage[]>>({});
let webviewStateBatchDepth = 0;
let webviewStateBatchPending = false;
let timelineProjectionSource: 'none' | 'persisted' | 'live' | 'authoritative' = 'none';

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
function collectProjectionKnownMessageIds(
  projection: SessionTimelineProjection | null | undefined,
): Set<string> {
  const ids = new Set<string>();
  const add = (value: unknown): void => {
    if (typeof value !== 'string') return;
    const normalized = value.trim();
    if (normalized) ids.add(normalized);
  };
  for (const artifact of ensureArray<TimelineProjectionArtifact>(projection?.artifacts)) {
    add(artifact.artifactId);
    add(artifact.message?.id);
    for (const alias of ensureArray<string>(artifact.messageIds)) add(alias);
    for (const item of ensureArray<TimelineExecutionItem>(artifact.executionItems)) {
      add(item.itemId);
      add(item.message?.id);
      for (const alias of ensureArray<string>(item.messageIds)) add(alias);
    }
  }
  return ids;
}

function normalizeComparableMessageText(message: Message | undefined): string {
  if (!message) {
    return '';
  }
  const content = typeof message.content === 'string' ? message.content.trim() : '';
  if (content) {
    return content.replace(/\s+/g, ' ');
  }
  return ensureArray<ContentBlock>(message.blocks)
    .map((block) => {
      if (!block || typeof block !== 'object') {
        return '';
      }
      if ((block.type === 'text' || block.type === 'thinking') && typeof block.content === 'string') {
        return block.content.trim();
      }
      if (block.type === 'thinking' && typeof block.thinking?.content === 'string') {
        return block.thinking.content.trim();
      }
      return '';
    })
    .filter(Boolean)
    .join(' ')
    .replace(/\s+/g, ' ')
    .trim();
}

function comparableTextsMatch(left: string, right: string): boolean {
  if (!left || !right) {
    return false;
  }
  return left === right || left.includes(right) || right.includes(left);
}

function projectionHasAcceptedUserEcho(
  projection: SessionTimelineProjection | null | undefined,
  node: TimelineNode,
): boolean {
  const localMessage = node.message;
  if (localMessage.role !== 'user' && localMessage.type !== 'user_input') {
    return false;
  }
  const localText = normalizeComparableMessageText(localMessage);
  if (!localText) {
    return false;
  }
  const matches = (message: Message | undefined, timestamp: number | undefined): boolean => {
    if (!message || (message.role !== 'user' && message.type !== 'user_input')) {
      return false;
    }
    void timestamp;
    return normalizeComparableMessageText(message) === localText;
  };

  for (const artifact of ensureArray<TimelineProjectionArtifact>(projection?.artifacts)) {
    if (matches(artifact.message, artifact.timestamp)) {
      return true;
    }
    for (const item of ensureArray<TimelineExecutionItem>(artifact.executionItems)) {
      if (matches(item.message, item.timestamp)) {
        return true;
      }
    }
  }
  return false;
}

function projectionHasAcceptedAssistantEcho(
  projection: SessionTimelineProjection | null | undefined,
  node: TimelineNode,
): boolean {
  const localMessage = node.message;
  if (localMessage.role !== 'assistant' || localMessage.isStreaming === true) {
    return false;
  }
  const metadata = resolveMessageMetadataRecord(localMessage);
  const isLocalResponse = Boolean(
    typeof metadata?.requestId === 'string' && metadata.requestId.trim()
    || metadata?.wasPlaceholder === true
    || metadata?.isPlaceholder === true,
  );
  if (!isLocalResponse) {
    return false;
  }
  const localText = normalizeComparableMessageText(localMessage);
  if (!localText) {
    return false;
  }
  const matches = (message: Message | undefined): boolean => {
    if (!message || message.role !== 'assistant') {
      return false;
    }
    const projectedText = normalizeComparableMessageText(message);
    return comparableTextsMatch(projectedText, localText);
  };

  for (const artifact of ensureArray<TimelineProjectionArtifact>(projection?.artifacts)) {
    if (matches(artifact.message)) {
      return true;
    }
    for (const item of ensureArray<TimelineExecutionItem>(artifact.executionItems)) {
      if (matches(item.message)) {
        return true;
      }
    }
  }
  return false;
}

function isNodeKnownByProjection(node: TimelineNode, knownIds: Set<string>): boolean {
  if (knownIds.has(node.nodeId) || knownIds.has(node.message.id)) {
    return true;
  }
  for (const alias of node.messageIds || []) {
    if (knownIds.has(alias)) {
      return true;
    }
  }
  for (const item of node.executionItems || []) {
    if (knownIds.has(item.itemId) || knownIds.has(item.message?.id)) {
      return true;
    }
    for (const alias of item.messageIds || []) {
      if (knownIds.has(alias)) {
        return true;
      }
    }
  }
  return false;
}

function timelineNodeLatestEventSeq(node: TimelineNode): number {
  return Math.max(
    node.latestEventSeq || 0,
    getMessageEventSeq(node.message) || 0,
    ...ensureArray<TimelineExecutionItem>(node.executionItems).map((item) => Math.max(
      item.latestEventSeq || 0,
      getMessageEventSeq(item.message) || 0,
    )),
  );
}

function timelineProjectionLatestEventSeq(
  projection: SessionTimelineProjection | null | undefined,
): number {
  return typeof projection?.lastAppliedEventSeq === 'number'
    ? projection.lastAppliedEventSeq
    : 0;
}

function timelineNodeIsActivelyStreaming(node: TimelineNode): boolean {
  if (node.message?.isStreaming === true) {
    return true;
  }
  return ensureArray<TimelineExecutionItem>(node.executionItems)
    .some((item) => item.message?.isStreaming === true);
}

function timelineNodeIsRequestPlaceholder(node: TimelineNode): boolean {
  const isPlaceholderMessage = (message: Message | undefined): boolean => {
    const metadata = resolveMessageMetadataRecord(message);
    return metadata?.isPlaceholder === true;
  };
  if (isPlaceholderMessage(node.message)) {
    return true;
  }
  return ensureArray<TimelineExecutionItem>(node.executionItems)
    .some((item) => isPlaceholderMessage(item.message));
}

function shouldOverlayLocalTimelineNode(
  node: TimelineNode,
  projection: SessionTimelineProjection | null | undefined,
  knownIds: Set<string>,
): boolean {
  const nodeSeq = timelineNodeLatestEventSeq(node);
  const projectionSeq = timelineProjectionLatestEventSeq(projection);
  const isNewerThanProjection = nodeSeq > projectionSeq;
  const isStreaming = timelineNodeIsActivelyStreaming(node);
  const isRequestPlaceholder = timelineNodeIsRequestPlaceholder(node);
  const backendActive = messagesState.backendProcessing || messagesState.pendingRequests.size > 0;

  // 占位节点在后端不再处理时让位 projection,避免发送按钮永远卡在“已发送”状态。
  if (!backendActive && isRequestPlaceholder) {
    return false;
  }

  if (isNodeKnownByProjection(node, knownIds)) {
    // projection 已经认领该节点(通过 messageIds 别名匹配),只在仍在流式且本地更新更新时覆盖。
    return isStreaming && isNewerThanProjection;
  }
  if (projectionHasAcceptedUserEcho(projection, node)) {
    return false;
  }
  if (projectionHasAcceptedAssistantEcho(projection, node)) {
    return false;
  }
  if (isStreaming) {
    return true;
  }
  if (isRequestPlaceholder && backendActive) {
    return true;
  }
  return isNewerThanProjection;
}

function isLocalUserEchoArtifact(artifact: TimelineProjectionArtifact): boolean {
  const metadata = resolveMessageMetadataRecord(artifact.message);
  const requestId = typeof metadata?.requestId === 'string' ? metadata.requestId.trim() : '';
  if (!requestId) {
    return false;
  }
  const message = artifact.message;
  return message.role === 'user' || message.type === 'user_input';
}

function isLocalAssistantEchoArtifact(artifact: TimelineProjectionArtifact): boolean {
  const message = artifact.message;
  if (message.role !== 'assistant') {
    return false;
  }
  const metadata = resolveMessageMetadataRecord(message);
  return Boolean(
    typeof metadata?.requestId === 'string' && metadata.requestId.trim()
    || metadata?.wasPlaceholder === true
    || metadata?.isPlaceholder === true,
  );
}

function isSessionTurnAssistantArtifact(artifact: TimelineProjectionArtifact): boolean {
  const isAssistantTurnMessage = (message: Message | undefined): boolean => {
    if (!message || message.role !== 'assistant') {
      return false;
    }
    const metadata = resolveMessageMetadataRecord(message);
    const turnItemKind = typeof metadata?.turnItemKind === 'string'
      ? metadata.turnItemKind.trim()
      : '';
    return turnItemKind === 'assistant_stream'
      || turnItemKind === 'assistant_final'
      || turnItemKind === 'assistant_thinking';
  };
  if (isAssistantTurnMessage(artifact.message)) {
    return true;
  }
  return ensureArray<TimelineExecutionItem>(artifact.executionItems)
    .some((item) => isAssistantTurnMessage(item.message));
}

function projectionHasUserMessageText(
  projection: SessionTimelineProjection | null | undefined,
  text: string,
): boolean {
  if (!text) {
    return false;
  }
  const matches = (message: Message | undefined): boolean => {
    if (!message || (message.role !== 'user' && message.type !== 'user_input')) {
      return false;
    }
    return normalizeComparableMessageText(message) === text;
  };
  for (const artifact of ensureArray<TimelineProjectionArtifact>(projection?.artifacts)) {
    if (matches(artifact.message)) {
      return true;
    }
    for (const item of ensureArray<TimelineExecutionItem>(artifact.executionItems)) {
      if (matches(item.message)) {
        return true;
      }
    }
  }
  return false;
}

function projectionHasAssistantMessageText(
  projection: SessionTimelineProjection | null | undefined,
  text: string,
): boolean {
  if (!text) {
    return false;
  }
  const matches = (message: Message | undefined): boolean => {
    if (!message || message.role !== 'assistant') {
      return false;
    }
    const projectedText = normalizeComparableMessageText(message);
    return comparableTextsMatch(projectedText, text);
  };
  for (const artifact of ensureArray<TimelineProjectionArtifact>(projection?.artifacts)) {
    if (matches(artifact.message)) {
      return true;
    }
    for (const item of ensureArray<TimelineExecutionItem>(artifact.executionItems)) {
      if (matches(item.message)) {
        return true;
      }
    }
  }
  return false;
}

export function timelineProjectionConfirmsLocalAssistantResponse(
  projection: SessionTimelineProjection | null | undefined,
): boolean {
  if (!projection) {
    return false;
  }
  const localAssistantTexts = messagesState.timelineNodes
    .filter((node) => isLocalAssistantEchoArtifact({
      artifactId: node.nodeId,
      kind: node.kind,
      displayOrder: node.displayOrder,
      anchorEventSeq: node.anchorEventSeq,
      latestEventSeq: node.latestEventSeq,
      cardStreamSeq: node.cardStreamSeq,
      timestamp: node.timestamp,
      threadVisible: node.visibleInThread,
      workerTabs: node.workerTabs,
      messageIds: node.messageIds,
      message: node.message,
      executionItems: node.executionItems,
    } as TimelineProjectionArtifact))
    .map((node) => normalizeComparableMessageText(node.message))
    .filter((text) => text.length > 0);
  if (localAssistantTexts.length === 0) {
    return false;
  }
  const authoritativeTexts: string[] = [];
  for (const artifact of ensureArray<TimelineProjectionArtifact>(projection.artifacts)) {
    if (artifact.message?.role === 'assistant') {
      const text = normalizeComparableMessageText(artifact.message);
      if (text) authoritativeTexts.push(text);
    }
    for (const item of ensureArray<TimelineExecutionItem>(artifact.executionItems)) {
      if (item.message?.role === 'assistant') {
        const text = normalizeComparableMessageText(item.message);
        if (text) authoritativeTexts.push(text);
      }
    }
  }
  return localAssistantTexts.some((localText) => authoritativeTexts.some((authoritativeText) => (
    comparableTextsMatch(authoritativeText, localText)
  )));
}

function collectMessageIdentityKeys(message: Message | undefined): Set<string> {
  const keys = new Set<string>();
  const add = (value: unknown): void => {
    if (typeof value !== 'string') return;
    const normalized = value.trim();
    if (normalized) keys.add(normalized);
  };
  add(message?.id);
  const metadata = resolveMessageMetadataRecord(message);
  add(metadata?.requestId);
  add(metadata?.rustStreamItemId);
  add(metadata?.rustEventItemId);
  add(metadata?.turnItemId);
  add(metadata?.toolCallId);
  const turnId = typeof metadata?.turnId === 'string' ? metadata.turnId.trim() : '';
  const turnItemId = typeof metadata?.turnItemId === 'string' ? metadata.turnItemId.trim() : '';
  if (turnId && turnItemId) {
    add(`turn:${turnId}:${turnItemId}`);
  }
  return keys;
}

function messagesShareIdentity(left: Message | undefined, right: Message | undefined): boolean {
  const leftKeys = collectMessageIdentityKeys(left);
  if (leftKeys.size === 0) {
    return false;
  }
  for (const key of collectMessageIdentityKeys(right)) {
    if (leftKeys.has(key)) {
      return true;
    }
  }
  return false;
}

function appendUniqueMessages(base: Message[], overlay: Message[]): Message[] {
  if (overlay.length === 0) {
    return base;
  }
  const seen = new Set<string>();
  const next: Message[] = [];
  const add = (message: Message): void => {
    const id = typeof message.id === 'string' ? message.id.trim() : '';
    if (id && seen.has(id)) {
      return;
    }
    if (id) {
      seen.add(id);
    }
    next.push(message);
  };
  for (const message of base) {
    if (overlay.some((overlayMessage) => messagesShareIdentity(message, overlayMessage))) {
      continue;
    }
    add(message);
  }
  for (const message of overlay) add(message);
  return next;
}

const messageProjection = $derived.by(() => {
  const currentSessionId = normalizeSessionId(messagesState.currentSessionId);
  const liveProjectionSessionId = currentSessionId || '__pending-session__';
  const projection = messagesState.timelineProjection
    || (messagesState.timelineNodes.length > 0
      ? buildLiveTimelineProjection(liveProjectionSessionId, messagesState.timelineNodes, null)
      : null);
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
    for (const item of artifact.executionItems || []) {
      for (const workerId of item.workerTabs || []) {
        if (typeof workerId === 'string' && workerId.trim()) {
          workerKeys.add(workerId.trim());
        }
      }
    }
  }
  for (const workerId of workerKeys) {
    workers[workerId] = buildTimelinePanelMessages(projection, 'worker', workerId);
  }

  const knownIds = collectProjectionKnownMessageIds(projection);
  const overlayNodes = messagesState.timelineNodes.filter((node) => (
    shouldOverlayLocalTimelineNode(node, projection, knownIds)
  ));
  if (overlayNodes.length > 0) {
    const overlayProjection = buildLiveTimelineProjection(projection.sessionId, overlayNodes, null);
    for (const artifact of overlayProjection.artifacts || []) {
      for (const workerId of artifact.workerTabs || []) {
        if (typeof workerId === 'string' && workerId.trim()) {
          workerKeys.add(workerId.trim());
        }
      }
      for (const item of artifact.executionItems || []) {
        for (const workerId of item.workerTabs || []) {
          if (typeof workerId === 'string' && workerId.trim()) {
            workerKeys.add(workerId.trim());
          }
        }
      }
    }
    for (const workerId of workerKeys) {
      workers[workerId] = appendUniqueMessages(
        workers[workerId] || [],
        buildTimelinePanelMessages(overlayProjection, 'worker', workerId),
      );
    }
    return {
      thread: appendUniqueMessages(
        buildTimelinePanelMessages(projection, 'thread'),
        buildTimelinePanelMessages(overlayProjection, 'thread'),
      ),
      workers,
    };
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
  const read = typeof item.read === 'boolean'
    ? item.read
    : Boolean(item.handled);
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

function resolveTimelineFragmentMessages(message: Message): Message[] {
  const fragments = expandRenderableTimelineMessages(message as unknown as TimelineFragmentMessage) as unknown as Message[];
  if (fragments.length <= 1) {
    return [];
  }
  return fragments.map((fragment, index) => normalizeIncomingMessage({
    ...fragment,
    isStreaming: message.isStreaming === true && index === fragments.length - 1,
  }));
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

function resolveWorkerVisibility(message: Message): {
  threadVisible: boolean;
  workerTabs: AgentId[];
} {
  const worker = resolveTimelineWorker(message);
  const visibility = resolveSharedTimelineWorkerVisibility({
    hasWorker: Boolean(worker),
    type: message.type,
    source: message.source,
    blocks: message.blocks,
    metadata: resolveMessageMetadataRecord(message),
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
  return message.id;
}

function resolveTimelineAliasId(rawId: string | undefined): string {
  const normalized = typeof rawId === 'string' ? rawId.trim() : '';
  if (!normalized) return '';
  return timelineNodeIdByMessageId.get(normalized) || normalized;
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
      ...collectTimelineAliasIds(normalizedMessage),
      itemId,
      ...ensureArray<string>(item.messageIds),
    ])),
    message: normalizedMessage,
  };
}

function executionItemToOrderInput(item: TimelineExecutionItem): TimelineSemanticOrderInput {
  return {
    timestamp: item.timestamp,
    stableId: item.itemId,
    anchorEventSeq: item.anchorEventSeq,
    blockSeq: getMessageBlockSeq(item.message),
  };
}

function buildFragmentExecutionItems(
  messages: Message[],
  visibility: { thread?: boolean; workerTabs?: AgentId[] },
): TimelineExecutionItem[] {
  return messages
    .map((message) => buildExecutionItemFromMessage(message, visibility))
    .sort((a, b) => compareTimelineSemanticOrder(executionItemToOrderInput(a), executionItemToOrderInput(b)))
    .map((item) => normalizeTimelineExecutionItem(item));
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
  const cardId = resolveTimelineCardId(stableMessage);
  const dispatchWaveId = typeof stableMessage.metadata?.dispatchWaveId === 'string'
    ? stableMessage.metadata.dispatchWaveId.trim()
    : (typeof node.dispatchWaveId === 'string' ? node.dispatchWaveId.trim() : '');
  const laneId = typeof stableMessage.metadata?.laneId === 'string'
    ? stableMessage.metadata.laneId.trim()
    : (typeof node.laneId === 'string' ? node.laneId.trim() : '');
  const worker = resolveTimelineWorker(stableMessage) || node.worker;
  const workerTabs = normalizeWorkerTabList(node.workerTabs);
  const messageIds = Array.from(new Set([
    ...collectTimelineAliasIds(stableMessage),
    stableNodeId,
    ...ensureArray<string>(node.messageIds),
  ]));
  const executionItems = ensureArray<TimelineExecutionItem>(node.executionItems)
    .map((item) => normalizeTimelineExecutionItem(item))
    .sort((a, b) => compareTimelineSemanticOrder(executionItemToOrderInput(a), executionItemToOrderInput(b)));
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
    ...(dispatchWaveId ? { dispatchWaveId } : {}),
    ...(laneId ? { laneId } : {}),
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

function nodeToOrderInput(node: TimelineNode): TimelineSemanticOrderInput {
  return {
    timestamp: node.timestamp,
    stableId: node.nodeId,
    anchorEventSeq: node.anchorEventSeq,
    blockSeq: getMessageBlockSeq(node.message),
  };
}

function compareTimelineNodeOrder(left: TimelineNode, right: TimelineNode): number {
  return compareTimelineSemanticOrder(nodeToOrderInput(left), nodeToOrderInput(right));
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
  frozenSemanticStage?: number;
}

function shouldRenderTimelineNodeHost(
  node: Pick<TimelineNode, 'kind' | 'message' | 'executionItems'>,
  displayContext: 'thread' | 'worker',
): boolean {
  void displayContext;
  if (ensureArray(node.executionItems).length > 0) {
    return false;
  }
  return true;
}

function renderEntryToOrderInput(entry: LocalProjectionFlatRenderEntry): TimelineSemanticOrderInput {
  return {
    timestamp: entry.timestamp,
    stableId: entry.entryId,
    anchorEventSeq: entry.anchorEventSeq,
    blockSeq: entry.blockSeq,
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

/**
 * 动态构建所有 Worker 的 render entries
 *
 * 从 artifacts 的 workerTabs 集合中自动发现所有 workerId，
 * 然后为每个 workerId 构建对应的 render entries。
 */
function buildDynamicWorkerRenderEntries(
  artifacts: TimelineProjectionArtifact[],
): Record<string, TimelineProjectionRenderEntry[]> {
  const workerIds = new Set<string>();
  for (const artifact of artifacts) {
    for (const w of artifact.workerTabs) {
      workerIds.add(w);
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
    if (artifactVisible && shouldRenderTimelineNodeHost({
      kind: artifact.kind as TimelineNodeKind,
      message: artifact.message,
      executionItems: artifact.executionItems,
    }, displayContext)) {
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
    .sort(compareRenderEntryOrder)
    .map((entry) => ({
      entryId: entry.entryId,
      artifactId: entry.artifactId,
      ...(entry.executionItemId ? { executionItemId: entry.executionItemId } : {}),
    }));
}

function normalizeProjectionRenderEntries(
  entries: unknown,
  artifactById: Map<string, TimelineProjectionArtifact>,
): TimelineProjectionRenderEntry[] {
  if (!Array.isArray(entries)) {
    return [];
  }
  const normalized: TimelineProjectionRenderEntry[] = [];
  for (const entry of entries) {
    if (!entry || typeof entry !== 'object') {
      continue;
    }
    const candidate = entry as Record<string, unknown>;
    const entryId = typeof candidate.entryId === 'string' ? candidate.entryId.trim() : '';
    const artifactId = typeof candidate.artifactId === 'string' ? candidate.artifactId.trim() : '';
    const executionItemId = typeof candidate.executionItemId === 'string'
      ? candidate.executionItemId.trim()
      : '';
    if (!entryId || !artifactId) {
      continue;
    }
    const artifact = artifactById.get(artifactId);
    if (!artifact) {
      continue;
    }
    if (executionItemId) {
      const hasExecutionItem = ensureArray<TimelineExecutionItem>(artifact.executionItems)
        .some((item) => item.itemId === executionItemId);
      if (!hasExecutionItem) {
        continue;
      }
    }
    normalized.push({
      entryId,
      artifactId,
      ...(executionItemId ? { executionItemId } : {}),
    });
  }
  return normalized;
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

function artifactToOrderInput(artifact: TimelineProjectionArtifact): TimelineSemanticOrderInput {
  return {
    timestamp: artifact.timestamp,
    stableId: artifact.artifactId,
    anchorEventSeq: artifact.anchorEventSeq,
    blockSeq: getMessageBlockSeq(artifact.message),
  };
}

function canonicalizeProjectionExecutionItems(
  executionItems: TimelineExecutionItem[] | undefined,
): TimelineExecutionItem[] {
  return ensureArray<TimelineExecutionItem>(executionItems)
    .map((item) => normalizeTimelineExecutionItem({
      ...item,
      message: normalizeProjectionRestoredMessage(item.message),
    }))
    .sort((a, b) => compareTimelineSemanticOrder(executionItemToOrderInput(a), executionItemToOrderInput(b)));
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
        dispatchWaveId: artifact.dispatchWaveId,
        laneId: artifact.laneId,
        worker: resolveTimelineWorker(message) || artifact.worker,
        threadVisible: artifact.threadVisible !== false,
      workerTabs: normalizeWorkerTabList(artifact.workerTabs),
      messageIds: Array.from(new Set([
          ...collectTimelineAliasIds(message),
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
    .sort((a, b) => compareTimelineSemanticOrder(artifactToOrderInput(a), artifactToOrderInput(b)))
    .map((artifact) => ({
      ...artifact,
      displayOrder: artifact.displayOrder || 0,
    }));
  const artifactById = new Map<string, TimelineProjectionArtifact>();
  for (const artifact of artifacts) {
    artifactById.set(artifact.artifactId, artifact);
  }

  const normalizedThreadRenderEntries = normalizeProjectionRenderEntries(
    projection.threadRenderEntries,
    artifactById,
  );
  const normalizedWorkerRenderEntries = Object.fromEntries(
    Object.entries(projection.workerRenderEntries || {}).map(([workerId, entries]) => [
      workerId,
      normalizeProjectionRenderEntries(entries, artifactById),
    ]),
  );

  return {
    ...projection,
    sessionId: normalizeSessionId(projection.sessionId) || projection.sessionId,
    artifacts,
    threadRenderEntries: normalizedThreadRenderEntries.length > 0
      ? normalizedThreadRenderEntries
      : buildProjectionRenderEntriesFromArtifacts(artifacts, 'thread'),
    workerRenderEntries: Object.keys(normalizedWorkerRenderEntries).length > 0
      ? normalizedWorkerRenderEntries
      : buildDynamicWorkerRenderEntries(artifacts),
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
      .sort((a, b) => compareTimelineSemanticOrder(executionItemToOrderInput(a), executionItemToOrderInput(b)));

    return {
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
      executionItems,
    };
  });

  const threadRenderEntries = buildProjectionRenderEntriesFromArtifacts(artifacts, 'thread');
  const workerRenderEntries = buildDynamicWorkerRenderEntries(artifacts);

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
function patchProjectionArtifactsInPlace(
  patchedNodes: Map<string, TimelineNode>,
): void {
  const projection = messagesState.timelineProjection;
  if (!projection || patchedNodes.size === 0) {
    return;
  }
  let maxEventSeq = projection.lastAppliedEventSeq;
  let maxUpdatedAt = projection.updatedAt;
  const nextArtifacts = projection.artifacts.map((artifact) => {
    const patchedNode = patchedNodes.get(artifact.artifactId);
    if (!patchedNode) {
      return artifact;
    }
    const nextMessage = normalizeIncomingMessage(patchedNode.message);
    const nextLatestEventSeq = Math.max(artifact.latestEventSeq, patchedNode.latestEventSeq);
    const nextCardStreamSeq = Math.max(artifact.cardStreamSeq, patchedNode.cardStreamSeq);
    maxEventSeq = Math.max(maxEventSeq, nextLatestEventSeq);
    const messageUpdatedAt = Math.max(
      nextMessage.updatedAt || 0,
      nextMessage.timestamp || 0,
      artifact.timestamp,
    );
    maxUpdatedAt = Math.max(maxUpdatedAt, messageUpdatedAt);
    return {
      ...artifact,
      message: nextMessage,
      latestEventSeq: nextLatestEventSeq,
      cardStreamSeq: nextCardStreamSeq,
    };
  });
  messagesState.timelineProjection = {
    ...projection,
    artifacts: nextArtifacts,
    lastAppliedEventSeq: maxEventSeq,
    updatedAt: maxUpdatedAt,
  };
  timelineProjectionDirty = false;
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
  normalized.sort((a, b) => compareTimelineSemanticOrder(nodeToOrderInput(a), nodeToOrderInput(b)));
  messagesState.timelineNodes = normalized;
  rebuildTimelineIndexes();
}

/**
 * 判断流式更新是否为结构性变更（需要全量 normalize-sort-reindex）。
 *
 * 非结构性（内容性）变更：文本追加、块内容更新、metadata 不影响排序的更新。
 * 结构性变更：type 变化、新增/删除 tool_call 块、lifecycleKey 变化、
 *             isComplete 转为 true、isStreaming 转为 false。
 */
function isStructuralTimelineUpdate(
  currentNode: TimelineNode,
  updates: Partial<Message>,
): boolean {
  const currentMessage = currentNode.message;

  // type 变化 → 结构性
  if (updates.type !== undefined && updates.type !== currentMessage.type) {
    return true;
  }

  // isComplete 从 false → true → 结构性
  if (updates.isComplete === true && !currentMessage.isComplete) {
    return true;
  }

  // isStreaming 从 true → false → 结构性
  if (updates.isStreaming === false && currentMessage.isStreaming) {
    return true;
  }

  // lifecycleKey 变化 → 结构性
  if (updates.metadata !== undefined) {
    const currentCardId = currentNode.cardId;
    const incomingMetadata = typeof updates.metadata === 'object' && !Array.isArray(updates.metadata)
      ? updates.metadata as Record<string, unknown>
      : undefined;
    if (incomingMetadata) {
      const nextCardId = typeof incomingMetadata.cardId === 'string' && incomingMetadata.cardId.trim()
        ? incomingMetadata.cardId.trim()
        : undefined;
      if (currentCardId !== nextCardId) {
        return true;
      }
    }
  }

  return false;
}

/**
 * 增量更新路径：直接更新目标节点的消息内容，跳过全量 normalize-sort-reindex。
 *
 * 前提：后端事件序列保证内容性更新不改变排序位置。
 * 此函数仅更新节点消息，不触发 sortAndSyncTimelineNodes，
 * 通过 Svelte 5 $state 代理的数组索引赋值触发响应式更新。
 */
function patchTimelineNodeInPlace(
  nodeId: string,
  patcher: (node: TimelineNode) => TimelineNode,
): void {
  const index = messagesState.timelineNodes.findIndex((n) => n.nodeId === nodeId);
  if (index < 0) return;
  const current = messagesState.timelineNodes[index];
  const patched = patcher(current);
  messagesState.timelineNodes[index] = patched;
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
      nextMessage.id,
      ...(options.replaceMessageId ? [options.replaceMessageId] : []),
    ])),
  });
}

function buildExecutionItemFromMessage(
  message: Message,
  visibility: { thread?: boolean; workerTabs?: AgentId[] },
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
  const existingNodeId = replaceNodeId
    || timelineNodeIdByMessageId.get(normalizedMessage.id)
    || (cardId ? timelineNodeIdByCardId.get(cardId) : undefined)
    || undefined;
  const stableNodeId = existingNodeId || explicitNodeId;
  const stableMessage: Message = {
    ...normalizedMessage,
    id: stableNodeId,
  };
  const fragmentMessages = resolveTimelineFragmentMessages(stableMessage);
  const usesFragmentExecutionItems = fragmentMessages.length > 1;
  const hostMessage = stableMessage;
  const nextWorkerTabs = normalizeWorkerTabList(visibility.workerTabs);
  const existingNode = existingNodeId
    ? messagesState.timelineNodes.find((node) => node.nodeId === existingNodeId)
    : undefined;

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
  const mergedMessage = hostMessage;
  const executionItems = (() => {
    if (usesFragmentExecutionItems) {
      return mergeFragmentExecutionItems({
        existingItems: existingNode?.executionItems,
        nextItems: buildFragmentExecutionItems(fragmentMessages, visibility),
      });
    }
    return existingNode?.executionItems || [];
  })();
  const nextNode: TimelineNode = normalizeTimelineNode({
    nodeId: stableNodeId,
    kind: existingNode?.kind || resolveTimelineNodeKind(mergedMessage),
    displayOrder: existingNode?.displayOrder ?? options.displayOrder ?? (++timelineDisplayOrderCounter),
    laneOrder: existingNode?.laneOrder,
    artifactVersion: existingNode?.artifactVersion,
    anchorEventSeq: existingNode?.anchorEventSeq || nextAnchorEventSeq,
    latestEventSeq: Math.max(existingNode?.latestEventSeq || 0, nextLatestEventSeq),
    cardStreamSeq: Math.max(existingNode?.cardStreamSeq || 0, nextCardStreamSeq),
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
      normalizedMessage.id,
      ...(options.replaceMessageId ? [options.replaceMessageId] : []),
    ])),
    message: mergedMessage,
    executionItems,
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
  const fragmentMessages = resolveTimelineFragmentMessages(nextMessage);
  const usesFragmentExecutionItems = fragmentMessages.length > 1;
  const nextVisibleMessage = nextMessage;
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
    workerTabs: currentNode.workerTabs,
    messageIds: currentNode.messageIds,
    timestamp: mergeTimelineSortTimestamp(currentNode.timestamp, nextMessage),
    message: nextVisibleMessage,
    executionItems: usesFragmentExecutionItems
      ? mergeFragmentExecutionItems({
          existingItems: currentNode.executionItems,
          nextItems: buildFragmentExecutionItems(fragmentMessages, {
            thread: currentNode.visibleInThread,
            workerTabs: currentNode.workerTabs,
          }),
        })
      : currentNode.executionItems,
  });
  mutateTimelineNodes((nodes) => nodes.map((node) => (
    node.nodeId === stableId ? nextNode : node
  )));
  return nextNode.message;
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
  const hasLocalLiveTurn = messagesState.pendingRequests.size > 0 || messagesState.activeMessageIds.size > 0;
  if (!snapshot) {
    messagesState.backendProcessing = false;
    if (hasLocalLiveTurn) {
      updateProcessingState();
      return;
    }
    messagesState.pendingRequests = new Set();
    messagesState.activeMessageIds = new Set();
    if (!runtimeStateIndicatesProcessing()) {
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
  if (!snapshot.isProcessing && pendingRequestIds.size === 0 && hasLocalLiveTurn) {
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
    activePlan: clonePersistablePayload(messagesState.appState?.activePlan ?? null) as AppState['activePlan'] | null,
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
        activePlan: clonePersistablePayload(executionSnapshot.activePlan) as AppState['activePlan'] | null,
      };
    }
  }
  return true;
}

function resetSessionScopedExecutionState(): void {
  edits = [];
  messagesState.orchestratorRuntimeState = null;
  if (messagesState.appState) {
    messagesState.appState = {
      ...messagesState.appState,
      pendingChanges: [],
      activePlan: null,
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
      sessionId: nextSessionId,
      hasMoreBefore: false,
      beforeCursor: null,
      isLoadingBefore: false,
    };
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
  scheduleSaveWebviewState();
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
  messagesState.appState = nextState;
  if (nextState) {
    messagesState.bootstrapped = true;
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

function updateProcessingState() {
  const nextIsProcessing = messagesState.backendProcessing
    || runtimeStateIndicatesProcessing()
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
    if (
      next.size === 0
      && !messagesState.backendProcessing
      && !runtimeStateIndicatesProcessing()
      && messagesState.activeMessageIds.size === 0
    ) {
      sealAllStreamingMessages();
    }
    updateProcessingState();
  }
}

export function settleProcessingAfterResponseCompletion() {
  if (messagesState.pendingRequests.size > 0 || messagesState.activeMessageIds.size > 0) {
    return;
  }
  messagesState.backendProcessing = false;
  if (runtimeStateIndicatesProcessing()) {
    updateProcessingState();
    return;
  }
  messagesState.lastForcedIdleAt = Date.now();
  updateProcessingState();
}

export function settleAuthoritativeIdleState() {
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

export function removeToast(id: string) {
  const normalizedId = typeof id === 'string' ? id.trim() : '';
  if (!normalizedId) {
    return;
  }
  toasts = toasts.filter((toast) => toast.id !== normalizedId);
}

export function markAllNotificationsRead() {
  vscode.postMessage({ type: 'markAllNotificationsRead' });
}

export function clearAllNotifications() {
  vscode.postMessage({ type: 'clearAllNotifications' });
}

export function removeNotification(id: string) {
  const normalizedId = typeof id === 'string' ? id.trim() : '';
  if (!normalizedId) {
    return;
  }
  vscode.postMessage({ type: 'removeNotification', notificationId: normalizedId });
}

export function applySessionNotifications(
  sessionId: string,
  rawNotifications: { records?: SessionNotificationRecord[] } | unknown,
): void {
  const normalizedSessionId = resolveNotificationSessionId(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  const normalized = normalizeSessionNotificationList(
    rawNotifications && typeof rawNotifications === 'object'
      ? (rawNotifications as { records?: unknown }).records
      : undefined,
  );
  replaceSessionNotificationList(normalizedSessionId, normalized);
  if (normalizedSessionId === getCurrentNotificationSessionId()) {
    applyNotificationList(normalized);
  }
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
    let hadCanonicalUpdate = false;
    const incrementalPatchedNodes = new Map<string, TimelineNode>();
    for (const [, flush] of entries) {
      const target = findTimelineMessageTargetByAlias(flush.messageId);
      if (!target) continue;

      const shouldUseCanonicalUpdatePath = Boolean(
        target.executionItem
        || ensureArray<TimelineExecutionItem>(target.node.executionItems).length > 0
      );

      if (shouldUseCanonicalUpdatePath || isStructuralTimelineUpdate(target.node, flush.updates)) {
        hadCanonicalUpdate = true;
        updateTimelineNodeByAlias(flush.messageId, flush.updates);
      } else {
        patchTimelineNodeInPlace(target.node.nodeId, (node) => {
          const nextMessage = normalizeIncomingMessage({
            ...node.message,
            ...flush.updates,
            id: node.nodeId,
          });
          const patched = {
            ...node,
            message: nextMessage,
            latestEventSeq: Math.max(
              node.latestEventSeq,
              getMessageEventSeq(nextMessage) ?? node.latestEventSeq,
            ),
            cardStreamSeq: Math.max(
              node.cardStreamSeq,
              getMessageCardStreamSeq(nextMessage) || node.cardStreamSeq,
            ),
          };
          incrementalPatchedNodes.set(node.nodeId, patched);
          return patched;
        });
      }
    }
    if (incrementalPatchedNodes.size > 0) {
      if (hadCanonicalUpdate) {
        // canonical 路径已重建 projection，但后续增量节点未被包含，需全量同步
        syncTimelineProjectionFromNodes(messagesState.currentSessionId, { persist: false });
      } else {
        // 纯增量：仅补丁变更的 artifact，跳过全量 normalize + sort
        patchProjectionArtifactsInPlace(incrementalPatchedNodes);
      }
    }
    scheduleSaveWebviewState();
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

// ────────────────────────────────────────────────────────────────
// 流式文本收集器
//
// 核心思路：将"数据累积"和"渲染决策"分离。
//   - delta 追加到 buffer（纯字符串拼接，零渲染开销）
//   - 每次 delta 都提供完整可渲染快照；RAF 合并层负责把同帧更新压成一次状态写入
//   - finalize 时 flush 所有剩余内容
// 不能按换行符截断渲染，否则没有换行的长句会持续收流但 UI 不继续更新。
// ────────────────────────────────────────────────────────────────

class StreamingTextCollector {
  /** 累积的完整原始文本 */
  private buffer = '';

  /** 追加增量文本到缓冲区（不触发渲染） */
  pushDelta(delta: string): void {
    this.buffer += delta;
  }

  /** 当前可交给渲染层的完整快照 */
  getRenderableText(): string {
    return this.buffer;
  }

  /**
   * 流结束时 finalize：返回完整的最终文本（包含未换行的尾部）。
   */
  finalize(): string {
    const result = this.buffer;
    this.buffer = '';
    return result;
  }
}

/** 活跃的流式收集器实例，按 entryId 索引 */
const activeStreamCollectors = new Map<string, StreamingTextCollector>();

/**
 * 处理后端 task.llm.delta SSE 事件推送的流式文本增量。
 *
 * 设计参考 Codex 的 MarkdownStreamCollector + StreamController：
 * 1. delta 追加到 collector buffer（纯内存操作，无渲染开销）
 * 2. 立即生成完整可渲染快照并交给 RAF 合并层
 * 3. MarkdownContent 内部还有自适应节流，避免长文本全量解析过频
 *
 * @param entryId 后端 timeline entry ID
 * @param delta 本次新增的增量文本片段
 * @param sessionId 会话 ID，用于过滤
 */
export function applyStreamingDelta(
  entryId: string | number,
  delta: string,
  sessionId?: string,
  options: {
    replaceMessageId?: string;
    requestId?: string;
    userMessageId?: string;
  } = {},
): void {
  if (sessionId && messagesState.currentSessionId && sessionId !== messagesState.currentSessionId) {
    return;
  }

  const nodeId = `rust-timeline:${entryId}`;
  const collectorKey = String(entryId);

  // 获取或创建收集器
  let collector = activeStreamCollectors.get(collectorKey);
  if (!collector) {
    collector = new StreamingTextCollector();
    activeStreamCollectors.set(collectorKey, collector);
  }

  // 追加 delta 到缓冲区
  collector.pushDelta(delta);
  const renderableText = collector.getRenderableText();
  if (renderableText.length === 0) {
    return;
  }

  const existingMessage = getEffectiveTimelineMessage(nodeId);
  if (existingMessage) {
    enqueueTimelineStreamUpdate(nodeId, {
      content: renderableText,
      blocks: [{ type: 'text', content: renderableText }],
      isStreaming: true,
      isComplete: false,
      updatedAt: Date.now(),
    });
    markMessageActive(nodeId);
  } else {
    const now = Date.now();
    upsertTimelineNode({
      id: nodeId,
      role: 'assistant',
      source: 'orchestrator',
      content: renderableText,
      blocks: [{ type: 'text', content: renderableText }],
      timestamp: now,
      updatedAt: now,
      isStreaming: true,
      isComplete: false,
      type: 'text',
      metadata: {
        sessionId: sessionId || messagesState.currentSessionId || '',
        eventSeq: 0,
        timelineAnchorTimestamp: now,
        turnItemId: collectorKey,
        rustStreamItemId: collectorKey,
        requestId: options.requestId || undefined,
        userMessageId: options.userMessageId || undefined,
        placeholderMessageId: options.replaceMessageId || undefined,
      },
    } as Message, { thread: true }, {
      displayOrder: undefined,
      replaceMessageId: options.replaceMessageId,
    });
    markMessageActive(nodeId);
  }
}

/**
 * 流式结束：finalize 收集器，flush 所有剩余内容（含未换行的尾部），
 * 标记 isStreaming=false。
 * 参考 Codex: StreamController.finalize() → collector.finalize_and_drain()
 */
export function sealStreamingDelta(entryId: string | number): void {
  const nodeId = `rust-timeline:${entryId}`;
  const collectorKey = String(entryId);
  const collector = activeStreamCollectors.get(collectorKey);

  if (collector) {
    const finalText = collector.finalize();
    activeStreamCollectors.delete(collectorKey);

    if (finalText.length > 0) {
      enqueueTimelineStreamUpdate(nodeId, {
        content: finalText,
        blocks: [{ type: 'text', content: finalText }],
        isStreaming: false,
        isComplete: true,
        updatedAt: Date.now(),
      });
      return;
    }
  }

  // 没有收集器但节点存在：直接标记完成
  const existingMessage = getEffectiveTimelineMessage(nodeId);
  if (existingMessage) {
    enqueueTimelineStreamUpdate(nodeId, {
      isStreaming: false,
      isComplete: true,
      updatedAt: Date.now(),
    });
  }
}

/**
 * 根据 messageId 获取当前时间线中的消息（包含 RAF 队列中的 pending 更新）。
 */
export function getTimelineMessageById(messageId: string): Message | undefined {
  return getEffectiveTimelineMessage(messageId);
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
  activeStreamCollectors.clear();
  if (streamFlushRAF !== undefined) {
    cancelAnimationFrame(streamFlushRAF);
    streamFlushRAF = undefined;
  }
  if (options.resetTimelineView !== false) {
    replaceTimelineNodes([]);
    messagesState.timelineProjection = null;
    timelineProjectionDirty = false;
    timelineDisplayOrderCounter = 0;
  }
  messagesState.orchestratorRuntimeState = null;
  messagesState.sessionHistory = {
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
    hasMoreBefore?: boolean;
    beforeCursor?: string | null;
    isLoadingBefore?: boolean;
  },
): void {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    messagesState.sessionHistory = {
      sessionId: null,
      hasMoreBefore: false,
      beforeCursor: null,
      isLoadingBefore: false,
    };
    return;
  }
  const current = messagesState.sessionHistory;
  if (current.sessionId && current.sessionId !== normalizedSessionId) {
    messagesState.sessionHistory = {
      sessionId: normalizedSessionId,
      hasMoreBefore: input.hasMoreBefore === true,
      beforeCursor: typeof input.beforeCursor === 'string' && input.beforeCursor.trim()
        ? input.beforeCursor.trim()
        : null,
      isLoadingBefore: input.isLoadingBefore === true,
    };
    return;
  }
  messagesState.sessionHistory = {
    sessionId: normalizedSessionId,
    hasMoreBefore: input.hasMoreBefore ?? current.hasMoreBefore,
    beforeCursor: input.beforeCursor !== undefined
      ? (typeof input.beforeCursor === 'string' && input.beforeCursor.trim() ? input.beforeCursor.trim() : null)
      : current.beforeCursor,
    isLoadingBefore: input.isLoadingBefore ?? current.isLoadingBefore,
  };
}

export function prependTimelineProjectionPage(
  sessionId: string,
  projection: SessionTimelineProjection,
): boolean {
  const normalizedSessionId = normalizeSessionId(sessionId);
  if (!normalizedSessionId) {
    return false;
  }
  const currentSessionId = normalizeSessionId(messagesState.currentSessionId);
  if (!currentSessionId || currentSessionId !== normalizedSessionId) {
    return false;
  }
  const currentProjection = messagesState.timelineProjection;
  if (!currentProjection || normalizeSessionId(currentProjection.sessionId) !== normalizedSessionId) {
    setTimelineProjection(projection, { source: 'authoritative' });
    return true;
  }
  const incomingProjection = canonicalizeTimelineProjection(projection);
  if (incomingProjection.artifacts.length === 0) {
    return false;
  }
  const retainedCurrentArtifacts = currentProjection.artifacts.filter((artifact) => {
    if (isLocalUserEchoArtifact(artifact)) {
      const text = normalizeComparableMessageText(artifact.message);
      return !projectionHasUserMessageText(incomingProjection, text);
    }
    if (isLocalAssistantEchoArtifact(artifact)) {
      const text = normalizeComparableMessageText(artifact.message);
      return !projectionHasAssistantMessageText(incomingProjection, text);
    }
    if (isSessionTurnAssistantArtifact(artifact)) {
      const text = normalizeComparableMessageText(artifact.message);
      return !projectionHasAssistantMessageText(incomingProjection, text);
    }
    if (artifact.message?.role === 'assistant') {
      const text = normalizeComparableMessageText(artifact.message);
      if (text.length >= 24 && projectionHasAssistantMessageText(incomingProjection, text)) {
        return false;
      }
    }
    return true;
  });
  const mergedArtifacts = [
    ...retainedCurrentArtifacts,
    ...incomingProjection.artifacts,
  ];
  const artifactById = new Map<string, TimelineProjectionArtifact>();
  for (const artifact of mergedArtifacts) {
    artifactById.set(artifact.artifactId, artifact);
  }
  const mergedProjection = canonicalizeTimelineProjection({
    ...currentProjection,
    sessionId: normalizedSessionId,
    updatedAt: Math.max(currentProjection.updatedAt, incomingProjection.updatedAt),
    lastAppliedEventSeq: Math.max(
      currentProjection.lastAppliedEventSeq,
      incomingProjection.lastAppliedEventSeq,
    ),
    artifacts: [...artifactById.values()],
    threadRenderEntries: [],
    workerRenderEntries: {},
  });
  setTimelineProjection({
    ...mergedProjection,
    threadRenderEntries: buildProjectionRenderEntriesFromArtifacts(
      mergedProjection.artifacts,
      'thread',
    ),
    workerRenderEntries: buildDynamicWorkerRenderEntries(mergedProjection.artifacts),
  }, { source: 'authoritative' });
  return true;
}

function buildTimelineNodesFromProjection(projection: SessionTimelineProjection): TimelineNode[] {
  const canonicalProjection = canonicalizeTimelineProjection(projection);
  const validArtifacts = ensureArray(canonicalProjection.artifacts).filter(isProjectionArtifact);

  const nextNodes: TimelineNode[] = [];
  for (const artifact of validArtifacts) {

    const message = normalizeProjectionRestoredMessage(artifact.message);
    const worker = resolveTimelineWorker(message) || artifact.worker;
    const executionItems = ensureArray<TimelineExecutionItem>(artifact.executionItems)
      .map((item) => normalizeTimelineExecutionItem({
        ...item,
        message: normalizeProjectionRestoredMessage(item.message),
      }));

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
      },
      executionItems,
    }));
  }
  return nextNodes;
}

export function setTimelineProjection(
  projection: SessionTimelineProjection,
  options: { hydrateNodes?: boolean; source?: 'persisted' | 'authoritative' } = {},
) {
  const canonicalProjection = canonicalizeTimelineProjection(projection);
  const nextNodes = buildTimelineNodesFromProjection(canonicalProjection);
  // 从后端恢复的 projection 中取最大 displayOrder 初始化计数器，确保后续新建节点序号不冲突
  timelineDisplayOrderCounter = canonicalProjection.artifacts.reduce(
    (max, a) => Math.max(max, a.displayOrder || 0),
    timelineDisplayOrderCounter,
  );
  messagesState.timelineProjection = canonicalProjection;
  timelineProjectionSource = options.source || 'authoritative';
  setTimelineProjectionNodes(nextNodes);
  timelineProjectionDirty = false;
  upsertSessionViewStateSnapshot(createSessionViewStateSnapshot(canonicalProjection.sessionId));
  saveWebviewState();
}

export function restoreTimelineProjectionIfNewer(
  projection: SessionTimelineProjection,
  options: { hydrateNodes?: boolean; source?: 'persisted' | 'authoritative' } = {},
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
  const isStrictlyNewer = compareTimelineProjectionFreshness(projection, currentProjection) > 0;
  if (!isStrictlyNewer && !shouldPreferRicherAuthoritativeProjection(projection, currentProjection)) {
    return false;
  }
  setTimelineProjection(projection, options);
  return true;
}

export function getTimelineProjectionSource(): 'none' | 'persisted' | 'live' | 'authoritative' {
  return timelineProjectionSource;
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
      clearProcessingState();
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
