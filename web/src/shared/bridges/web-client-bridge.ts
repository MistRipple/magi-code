import {
  AgentApiError,
  BROWSER_AUTHORITY_CHANGED_EVENT,
  agentUrl,
  dispatchAgentConnectionEvent,
  getAgentSettingsBootstrap,
  probeReachableAgentBaseUrl,
  resolveAgentBaseUrl,
  RUNTIME_BASE_URL_STORAGE_KEY,
  isPublicTunnelAccess,
} from '../../web/agent-api';
import {
  clearAgentBindingContext,
  agentBindingWorkspaceId,
  agentBindingWorkspacePath,
  resolveAgentBindingContext,
  seedAgentBindingContextFromWindow,
  setAgentBindingContext,
  type AgentBindingContext,
  type AgentBindingOverride,
  type WorkspaceAgentBindingOverride,
} from '../../web/agent-binding-context';
import { i18n } from '../../stores/i18n.svelte';
import { getHostApi, getTransport, initTransport } from '../transport';
import {
  approveAgentChange,
  approveAllAgentChanges,
  addAgentKnowledgeItem,
  listAgentKnowledgeRelations,
  reportAgentIncident,
  addAgentCustomTool,
  addAgentMcpServer,
  addAgentRepository,
  clearAgentNotifications,
  clearAgentProjectKnowledge,
  closeAgentSession,
  connectAgentMcpServer,
  deleteAgentSession,
  deleteAgentKnowledgeItem,
  deleteAgentMcpServer,
  deleteAgentRepository,
  disconnectAgentMcpServer,
  fetchAgentModelList,
  getAgentMcpServerTools,
  getAgentExecutionStats,
  getAgentChangeDiff,
  getAgentFilePreview,
  getAgentProjectKnowledge,
  getAgentNotifications,
  getAgentSessionTurnQueue,
  getPersonalSessions,
  guideAgentSessionTurnQueueItem,
  getWorkspaceSessions,
  interruptAgentSession,
  installAgentLocalSkill,
  installAgentSkill,
  listAgentWorkspaces,
  loadAgentSkillLibrary,
  markAllAgentNotificationsRead,
  refreshAgentMcpTools,
  refreshAgentRepository,
  reindexAgentProjectKnowledge,
  removeAgentNotification,
  removeAgentSessionTurnQueueItem,
  resolveAgentNotification,
  removeAgentInstalledSkill,
  renameAgentSession,
  resetAgentExecutionStats,
  saveAgentAuxiliaryConfig,
  saveAgentOrchestratorConfig,
  saveAgentUserRules,
  saveAgentSafeguardConfig,
  saveAgentSkillsConfig,
  saveAgentWorkerConfig,
  submitSessionTurn,
  revertAgentChange,
  revertAgentExecutionGroupChanges,
  revertAllAgentChanges,
  testAgentAuxiliaryConnection,
  testAgentOrchestratorConnection,
  testAgentWorkerConnection,
  updateAgentKnowledgeItem,
  updateAgentMcpServer,
  updateAgentRepository,
  updateAgentRuntimeSetting,
  updateAgentSkill,
  updateAllAgentSkills,
  listAgentRegistryAgents,
  listAgentRegistryEngines,
  listAgentRoleTemplates,
} from '../../web/agent-api';
import {
  dispatchFilePreviewEvent,
  normalizeFileReferenceTarget,
} from '../../lib/file-reference';
import { openExternalWebUrl } from '../../lib/external-link';
import type {
  AgentKnowledgeItemPatch,
  AgentKnowledgeItemPayload,
} from '../../web/agent-api';
import type { ClientBridge, ClientBridgeMessage, SupportedLocale } from './client-bridge';
import {
  createNotifyMessage,
  generateMessageId,
  MessageCategory,
  MessageLifecycle,
  MessageType,
  type DataMessageType,
  type StandardMessage,
} from '../protocol/message-protocol';
import type {
  SettingsBootstrapPayload,
  SettingsBootstrapSnapshot,
} from '../settings-bootstrap';
import { buildEmptyWorkspaceAppState } from './empty-workspace-state';
import {
  normalizeRustBootstrapPayload,
  parseRustEventEnvelope,
  readRustCanonicalHistoryPageMeta,
  type BootstrapPayload,
  type RustEventEnvelope,
} from './rust-daemon-contract';
import {
  parseCanonicalTurnEventPayload,
  isCanonicalTerminalStatus,
  type CanonicalTurnEvent,
} from '../protocol/canonical-turn';
import { canonicalTurnRequestId } from '../protocol/canonical-processing';
import { isTerminalRuntimeTaskStatus } from '../protocol/runtime-task-status';
import type { SseConnection } from '../transport';
import {
  AppServerClient,
  type AppServerConnectionState,
} from '../app-server-client';
import {
  activateAgentRunSession,
  fetchAgentRunProjection,
  startAutoRefresh as startAgentRunAutoRefresh,
  getAgentRunState,
  clearAgentRunProjection,
  setAgentRunBridgeConnected,
} from '../../stores/agent-run-store.svelte';
import {
  applySessionPlanSnapshot,
  refreshCurrentGoal,
} from '../../stores/goal-store.svelte';
import type { QueuedSessionTurnDto, SessionPlanDto } from '../rust-backend-types';
import { sanitizeSvgContent } from '../svg-sanitizer';
import {
  messagesState,
  beginLocalTurnSubmission,
  markLocalTurnSubmissionQueued,
  adoptQueuedTurnSubmissions,
  clearRequestBinding,
  clearPendingRequest,
  listPendingRequestIdsForSession,
  settlePendingRequestSnapshot,
  settleAuthoritativeIdleState,
  completeTurnEditing,
  createRequestBinding,
  getRequestBinding,
  adoptCurrentSessionIdForLiveTurn,
  setQueuedMessages,
  setOrchestratorRuntimeState,
  updateRequestBinding,
} from '../../stores/messages.svelte';
import {
  setCanonicalTimelineError,
  turnStoreState,
} from '../../stores/turn-store.svelte';
import {
  SESSION_NAVIGATION_TIMEOUT_MS,
  sessionNavigationState,
} from '../session-navigation.svelte';
import { resolveModelListFetchBlockReason } from '../model-governance';
import type {
  Message,
  MessageBrowserNodeSelection,
  OrchestratorRuntimeSnapshot,
  QueuedMessage,
} from '../../types/message';
import { refreshPendingChangesProjection } from '../../lib/pending-changes-refresh';
import { syncToolApprovals } from '../../stores/tool-approval-store.svelte';

const listeners: Set<(message: ClientBridgeMessage) => void> = new Set();
const pendingBridgeMessages: ClientBridgeMessage[] = [];
const MAX_PENDING_BRIDGE_MESSAGES = 64;
let bridgeListenerRegistered = false;
let currentWorkspaceId = '';
let currentWorkspacePath = '';
let currentSessionId = '';
let currentSessionScope: 'personal' | 'workspace' = 'personal';
let currentInterruptTaskId = '';
let currentBindingGeneration = 0;
let continueRequestId = '';
let currentRuntimeEpoch = '';
let terminalBootstrapRefreshInFlight: Promise<void> | null = null;
let terminalBootstrapRefreshPending = false;
let terminalBootstrapRefreshReason = '';
type SessionTurnSubmissionContext = {
  requestId: string;
  scope: 'personal' | 'workspace';
  workspaceId: string;
  workspacePath: string;
  /** 提交时已经存在的后端会话 ID；空值表示新会话草稿。 */
  targetSessionId: string;
  /** 草稿提交开始时的绑定代际；旧请求不能夺回后来新建的草稿。 */
  bindingGeneration: number;
  /** SSE 先于 HTTP 返回时记录已确认的后端会话。 */
  acceptedSessionId?: string;
};

type ProcessingRequestSnapshot = {
  sessionId: string;
  requestIds: string[];
};

// 每个 request 都有自己的提交上下文。旧实现只有一个全局槽位，第二个会话
// 提交时会覆盖第一个，导致 accepted/error 返回后无法找到原请求。
const sessionTurnSubmissions = new Map<string, SessionTurnSubmissionContext>();
const guidingQueuedMessageIds = new Set<string>();
let cachedSettingsBootstrap: SettingsBootstrapPayload | null = null;
let cachedSettingsBootstrapScope: 'none' | 'core' | 'full' = 'none';
let cachedSettingsBootstrapBindingKey = '';

/** 事件流连接句柄：Web 使用 SSE，Desktop 使用 App Server WebSocket。 */
type EventStreamConnection = SseConnection | AppServerClient;
let activeEventStreamConnection: EventStreamConnection | null = null;
let activeEventStreamKey = '';
let activeEventStreamState: 'idle' | 'connecting' | 'open' = 'idle';
let activeEventStreamOpenPromise: Promise<void> | null = null;
let activeEventStreamOpenTimeout: number | null = null;
let activeEventStreamToken = 0;
let activeEventStreamOpenResolve: (() => void) | null = null;
let activeEventStreamOpenReject: ((error: Error) => void) | null = null;
let eventStreamCursorScopeKey = '';
let eventStreamAfterSequence = 0;
// SSE 空闲检测：后端每 5s 发浏览器可见 keep-alive 事件，任何事件都会刷新 lastEventStreamActivityAt。
// 超过 EVENT_STREAM_IDLE_TIMEOUT_MS 未收到任何事件即视为静默断流，触发 recovery 重拉 bootstrap，
// 让 applyAuthoritativeProcessingState 根据权威快照收敛运行态，避免前端永久卡在 running。
let lastEventStreamActivityAt = 0;
let eventStreamIdleCheckTimer: number | null = null;
let bridgeRecovering = false;
// fetchBootstrap 防重入：只复用同一 workspace/session 绑定下的飞行请求。
// workspace 发生切换时必须让旧请求失效，避免旧 bootstrap 覆盖新工作区首屏状态。
let bootstrapInFlight: Promise<void> | null = null;
let bootstrapInFlightBindingKey = '';
let bootstrapRequestSeq = 0;
type SessionNavigationLease = {
  requestId: string;
  sequence: number;
  completion: Promise<void>;
  release: () => void;
};
let sessionNavigationSequence = 0;
let activeSessionNavigation: SessionNavigationLease | null = null;
let settingsBootstrapInFlight: Promise<void> | null = null;
let settingsBootstrapInFlightBindingKey = '';
let settingsBootstrapRequestSeq = 0;
let recoveryAttempt = 0;
let recoveryTimer: number | null = null;
let recoveryInFlight: Promise<void> | null = null;
let recoveryInFlightBindingKey = '';
let eventStreamSnapshotRefreshInFlight: Promise<void> | null = null;
let eventStreamSnapshotRefreshBindingKey = '';
let eventStreamTunnelSyncInFlight: Promise<void> | null = null;
let eventStreamTunnelSyncBindingKey = '';
let initialWindowBindingHydrated = false;
let sessionSummaryRefreshTimer: ReturnType<typeof setTimeout> | null = null;
type SessionSummaryRefreshTarget =
  | { scope: 'personal'; key: 'personal' }
  | { scope: 'workspace'; key: string; workspaceId: string; workspacePath: string };
const sessionSummaryRefreshTargets = new Map<string, SessionSummaryRefreshTarget>();
const inFlightChangeMutationScopes = new Set<string>();

function invalidateBootstrapRequests(): void {
  bootstrapRequestSeq += 1;
  bootstrapInFlight = null;
  bootstrapInFlightBindingKey = '';
}

function beginSessionNavigation(requestId: string): SessionNavigationLease {
  // 新导航代表最新用户意图。先释放旧屏障，让等待者继续等待新租约，
  // 旧导航响应则由独立 sequence 判定为过期，不能提交页面状态。
  activeSessionNavigation?.release();
  let release!: () => void;
  const completion = new Promise<void>((resolve) => {
    release = resolve;
  });
  const lease: SessionNavigationLease = {
    requestId,
    sequence: ++sessionNavigationSequence,
    completion,
    release,
  };
  activeSessionNavigation = lease;
  invalidateBootstrapRequests();
  return lease;
}

function isCurrentSessionNavigation(lease: SessionNavigationLease): boolean {
  return activeSessionNavigation?.sequence === lease.sequence
    && activeSessionNavigation.requestId === lease.requestId;
}

function releaseSessionNavigation(lease: SessionNavigationLease): void {
  if (isCurrentSessionNavigation(lease)) {
    activeSessionNavigation = null;
  }
  lease.release();
}

async function waitForSessionNavigationCommit(): Promise<void> {
  while (activeSessionNavigation) {
    await activeSessionNavigation.completion;
  }
}

function clearContinueRequestInFlight(requestId?: string): void {
  if (continueRequestId) {
    if (requestId && continueRequestId !== requestId) {
      return;
    }
    clearPendingRequest(continueRequestId);
    continueRequestId = '';
  }
}

function submissionContextPayload(
  context: SessionTurnSubmissionContext,
): Record<string, unknown> {
  return {
    requestId: context.requestId,
    scope: context.scope,
    workspaceId: context.workspaceId || null,
    workspacePath: context.workspacePath || null,
    targetSessionId: context.targetSessionId || null,
    bindingGeneration: context.bindingGeneration,
    ...(context.acceptedSessionId ? { acceptedSessionId: context.acceptedSessionId } : {}),
  };
}

function currentStoreSessionId(): string {
  return messagesState.currentSessionId?.trim() || '';
}

function submissionContextMatchesCurrentScope(context: SessionTurnSubmissionContext): boolean {
  if (context.scope !== currentSessionScope) {
    return false;
  }
  if (context.scope === 'workspace') {
    return context.workspaceId
      ? context.workspaceId === currentWorkspaceId
        && (!context.workspacePath || context.workspacePath === currentWorkspacePath)
      : context.workspacePath === currentWorkspacePath;
  }
  return !currentWorkspaceId && !currentWorkspacePath;
}

function submissionContextCanCommit(
  context: SessionTurnSubmissionContext,
  resolvedSessionId: string,
): boolean {
  if (sessionTurnSubmissions.get(context.requestId) !== context) {
    return false;
  }
  if (!submissionContextMatchesCurrentScope(context)) {
    return false;
  }
  const storeSessionId = currentStoreSessionId();
  if (context.targetSessionId) {
    return context.targetSessionId === resolvedSessionId
      && currentSessionId === context.targetSessionId
      && storeSessionId === context.targetSessionId;
  }
  if (context.acceptedSessionId) {
    return context.acceptedSessionId === resolvedSessionId
      && currentSessionId === resolvedSessionId
      && storeSessionId === resolvedSessionId;
  }
  // 草稿没有 sessionId。只有同一绑定代际仍处于空会话时，HTTP accepted
  // 才能把真实会话提交到当前页面；新建草稿后旧请求只能在后台完成。
  return context.bindingGeneration === currentBindingGeneration
    && !currentSessionId
    && !storeSessionId;
}

function finishSessionTurnSubmission(context: SessionTurnSubmissionContext): void {
  if (sessionTurnSubmissions.get(context.requestId) === context) {
    sessionTurnSubmissions.delete(context.requestId);
  }
}

function invalidateSessionTurnSubmissionsForNavigation(): void {
  // 导航是用户对当前页面归属的即时变更。先切断旧 submission context，
  // 再等待导航 bootstrap，避免旧草稿的迟到 accepted 响应夺回新页面。
  currentBindingGeneration += 1;
  clearContinueRequestInFlight();
}

const RECOVERY_BASE_DELAY_MS = 1000;
const RECOVERY_MAX_DELAY_MS = 10_000;
const EVENT_STREAM_PARSE_ERROR_DEBOUNCE_MS = 5000;
const EVENT_STREAM_OPEN_TIMEOUT_MS = 12_000;
// 后端 SSE keep-alive interval 为 5s（见 crates/magi-api/src/sse.rs）。
// 前端按“漏心跳数量”判断静默断流。Tunnel / 手机 / 代理链路下心跳可能抖动；
// 静默判定必须稳定，不能因为正在响应就缩短窗口，否则会在活跃 turn 中误触发
// bootstrap recovery + SSE 重连，造成多端实时输出不同步。
const EVENT_STREAM_KEEP_ALIVE_INTERVAL_MS = 5_000;
const EVENT_STREAM_IDLE_MISSED_KEEPALIVES = 6;
const EVENT_STREAM_IDLE_TIMEOUT_MS = EVENT_STREAM_KEEP_ALIVE_INTERVAL_MS * EVENT_STREAM_IDLE_MISSED_KEEPALIVES;
const EVENT_STREAM_IDLE_CHECK_INTERVAL_MS = 5_000;
const EVENT_STREAM_TUNNEL_BUSY_REFRESH_INTERVAL_MS = 2_000;
const EVENT_STREAM_TUNNEL_IDLE_TIMEOUT_MS = 30_000;
const EVENT_STREAM_TUNNEL_IDLE_CHECK_INTERVAL_MS = 1_000;
const EXTERNAL_SESSION_SUMMARY_EVENTS = new Set([
  'session.turn.task.accepted',
  'session.turn.completed',
  'session.turn.failed',
  'session.turn.interrupted',
  'session.turn.superseded',
  'session.title.updated',
  'session.deleted',
  'session.closed',
  'session.viewed',
  'message.created',
]);
const WEBVIEW_STATE_STORAGE_KEY = 'webview-state';
const WEBVIEW_STATE_WRITE_INTERVAL_MS = 1200;
const WEBVIEW_STATE_MAX_BYTES = 1_500_000;
let lastEventStreamParseErrorAt = 0;
let lastWebviewStateWriteAt = 0;
let webviewStateWriteTimer: number | null = null;
let webviewStatePersistenceDisabled = false;
let webviewStatePersistenceWarningLogged = false;
let pendingWebviewState: unknown = null;
let cachedWebviewState: unknown = null;
const goalRefreshTimers = new Map<string, ReturnType<typeof setTimeout>>();
const storageWarningSignatures = new Set<string>();

function scheduleGoalRefresh(
  sessionId: string,
  workspaceId: string,
  workspacePath: string,
): void {
  const key = `${workspaceId.trim() || 'personal'}\u0000${sessionId.trim()}`;
  if (!sessionId.trim() || goalRefreshTimers.has(key)) return;
  goalRefreshTimers.set(key, setTimeout(() => {
    goalRefreshTimers.delete(key);
    void refreshCurrentGoal(sessionId, workspaceId, workspacePath);
  }, 120));
}

function normalizeStorageErrorMessage(error: unknown): string {
  if (error instanceof Error && error.message.trim()) {
    return error.message.trim();
  }
  if (typeof error === 'string' && error.trim()) {
    return error.trim();
  }
  return 'unknown_storage_error';
}

function warnStorageFailure(action: string, key: string, error: unknown): void {
  const signature = `${action}:${key}:${normalizeStorageErrorMessage(error)}`;
  if (storageWarningSignatures.has(signature)) {
    return;
  }
  storageWarningSignatures.add(signature);
  console.warn(`[web-client-bridge] localStorage ${action} 失败(${key})，已降级处理`, error);
}

function safeLocalStorageGetItem(key: string): string {
  if (typeof window === 'undefined') {
    return '';
  }
  try {
    return localStorage.getItem(key) || '';
  } catch (error) {
    warnStorageFailure('读取', key, error);
    return '';
  }
}

function safeLocalStorageSetItem(key: string, value: string): boolean {
  if (typeof window === 'undefined') {
    return false;
  }
  try {
    localStorage.setItem(key, value);
    return true;
  } catch (error) {
    warnStorageFailure('写入', key, error);
    return false;
  }
}

function safeLocalStorageRemoveItem(key: string): boolean {
  if (typeof window === 'undefined') {
    return false;
  }
  try {
    localStorage.removeItem(key);
    return true;
  } catch (error) {
    warnStorageFailure('删除', key, error);
    return false;
  }
}

function flushPersistedWebviewState(): void {
  if (webviewStateWriteTimer !== null) {
    window.clearTimeout(webviewStateWriteTimer);
    webviewStateWriteTimer = null;
  }
  if (webviewStatePersistenceDisabled || pendingWebviewState === null) {
    return;
  }
  let serialized = '';
  try {
    serialized = JSON.stringify(pendingWebviewState);
  } catch (error) {
    warnStorageFailure('序列化', WEBVIEW_STATE_STORAGE_KEY, error);
    webviewStatePersistenceDisabled = true;
    pendingWebviewState = null;
    return;
  }
  pendingWebviewState = null;

  if (serialized.length > WEBVIEW_STATE_MAX_BYTES) {
    webviewStatePersistenceDisabled = true;
    safeLocalStorageRemoveItem(WEBVIEW_STATE_STORAGE_KEY);
    if (!webviewStatePersistenceWarningLogged) {
      webviewStatePersistenceWarningLogged = true;
      console.warn('[web-client-bridge] webview 状态体积过大，已切换为内存态持久化模式', {
        bytes: serialized.length,
        maxBytes: WEBVIEW_STATE_MAX_BYTES,
      });
    }
    return;
  }

  if (safeLocalStorageSetItem(WEBVIEW_STATE_STORAGE_KEY, serialized)) {
    lastWebviewStateWriteAt = Date.now();
    return;
  }

  webviewStatePersistenceDisabled = true;
  safeLocalStorageRemoveItem(WEBVIEW_STATE_STORAGE_KEY);
  if (!webviewStatePersistenceWarningLogged) {
    webviewStatePersistenceWarningLogged = true;
    console.warn('[web-client-bridge] webview 状态写入失败，已切换为内存态持久化模式');
  }
}

function schedulePersistedWebviewState(): void {
  if (typeof window === 'undefined' || webviewStatePersistenceDisabled) {
    return;
  }
  if (webviewStateWriteTimer !== null) {
    return;
  }
  const elapsed = Date.now() - lastWebviewStateWriteAt;
  const delay = elapsed >= WEBVIEW_STATE_WRITE_INTERVAL_MS
    ? 0
    : WEBVIEW_STATE_WRITE_INTERVAL_MS - elapsed;
  webviewStateWriteTimer = window.setTimeout(() => {
    flushPersistedWebviewState();
  }, delay);
}

function sanitizeVsCodeMessage(message: ClientBridgeMessage): ClientBridgeMessage {
  try {
    if (typeof structuredClone === 'function') {
      return structuredClone(message);
    }
  } catch {
    // fall through to JSON clone
  }
  return JSON.parse(JSON.stringify(message)) as ClientBridgeMessage;
}

function forwardToVsCodeHost(message: ClientBridgeMessage): boolean {
  const api = getHostApi();
  if (!api) {
    return false;
  }
  api.postMessage(sanitizeVsCodeMessage(message));
  return true;
}

function normalizeErrorMessage(error: unknown): string | undefined {
  if (error && typeof error === 'object') {
    const candidate = error as { detail?: unknown; message?: unknown };
    if (typeof candidate.detail === 'string' && candidate.detail.trim()) {
      return candidate.detail.trim();
    }
    if (typeof candidate.message === 'string' && candidate.message.trim()) {
      return candidate.message.trim();
    }
  }
  if (typeof error === 'string' && error.trim()) {
    return error.trim();
  }
  return undefined;
}

function trimBridgeString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function hasBridgeField(source: object, key: string): boolean {
  const record = source as Record<string, unknown>;
  return Object.prototype.hasOwnProperty.call(record, key) && record[key] !== undefined;
}

function resolveWorkspaceScopeFromSource(
  source: object,
  fallback: { workspaceId: string; workspacePath: string } = {
    workspaceId: currentWorkspaceId,
    workspacePath: currentWorkspacePath,
  },
): { workspaceId: string; workspacePath: string; hasWorkspaceOverride: boolean } {
  const record = source as Record<string, unknown>;
  const hasWorkspaceId = hasBridgeField(source, 'workspaceId');
  const hasWorkspacePath = hasBridgeField(source, 'workspacePath');
  const hasWorkspaceOverride = hasWorkspaceId || hasWorkspacePath;
  return {
    workspaceId: hasWorkspaceId
      ? trimBridgeString(record.workspaceId)
      : (hasWorkspaceOverride ? '' : fallback.workspaceId),
    workspacePath: hasWorkspacePath
      ? trimBridgeString(record.workspacePath)
      : (hasWorkspaceOverride ? '' : fallback.workspacePath),
    hasWorkspaceOverride,
  };
}

type BridgeRequestScope = AgentBindingOverride & { knowledgeRequestId?: string };

function requestScopeFromMessage(
  message: Record<string, unknown>,
  fallbackSessionId: string = currentSessionId,
): BridgeRequestScope {
  const workspaceScope = resolveWorkspaceScopeFromSource(message);
  const sessionId = hasBridgeField(message, 'sessionId')
    ? trimBridgeString(message.sessionId)
    : (!workspaceScope.hasWorkspaceOverride ? fallbackSessionId : '');
  return workspaceScope.workspaceId || workspaceScope.workspacePath
    ? {
        scope: 'workspace',
        workspaceId: workspaceScope.workspaceId,
        workspacePath: workspaceScope.workspacePath,
        ...(sessionId ? { sessionId } : {}),
      }
    : { scope: 'personal', ...(sessionId ? { sessionId } : {}) };
}

function currentSessionBindingOverride(
  sessionId = currentSessionId,
  workspaceId = currentWorkspaceId,
  workspacePath = currentWorkspacePath,
): AgentBindingOverride {
  return workspaceId || workspacePath
    ? { scope: 'workspace', workspaceId, workspacePath, sessionId }
    : { scope: 'personal', sessionId };
}

function requireWorkspaceRequestScope(scope: BridgeRequestScope): WorkspaceAgentBindingOverride {
  const workspaceScope = resolveWorkspaceScopeFromSource(scope);
  return {
    scope: 'workspace',
    workspaceId: workspaceScope.workspaceId,
    workspacePath: workspaceScope.workspacePath,
    sessionId: scope.sessionId,
  };
}

function changeMutationScopeKey(scope: BridgeRequestScope): string {
  return JSON.stringify({
    sessionId: scope.sessionId?.trim() || '',
    workspaceId: scope.workspaceId?.trim() || '',
    workspacePath: scope.workspacePath?.trim() || '',
  });
}

function emitChangeMutationStatus(scope: BridgeRequestScope, isMutating: boolean): void {
  emitDataMessage('changeMutationStatus', {
    isMutating,
    sessionId: scope.sessionId?.trim() || null,
    workspaceId: scope.workspaceId?.trim() || null,
    workspacePath: scope.workspacePath?.trim() || null,
    updatedAt: Date.now(),
  });
}

async function runChangeMutationOnce(scope: BridgeRequestScope, operation: () => Promise<void>): Promise<void> {
  const scopeKey = changeMutationScopeKey(scope);
  if (inFlightChangeMutationScopes.has(scopeKey)) {
    return;
  }
  inFlightChangeMutationScopes.add(scopeKey);
  emitChangeMutationStatus(scope, true);
  try {
    await operation();
  } finally {
    inFlightChangeMutationScopes.delete(scopeKey);
    emitChangeMutationStatus(scope, false);
  }
}

function asBridgeRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return null;
  }
  return value as Record<string, unknown>;
}

function normalizeBridgeStringArray(value: unknown): string[] {
  return Array.isArray(value)
    ? value
      .map((item) => trimBridgeString(item))
      .filter((item) => item.length > 0)
    : [];
}

interface BootstrapAgentRunTrackingHints {
  rootTaskId: string;
  activeTaskIds: string[];
}

function clearCurrentInterruptTaskId(): void {
  currentInterruptTaskId = '';
}

function setCurrentInterruptTaskId(taskId: string): void {
  currentInterruptTaskId = trimBridgeString(taskId);
}

function reconcileCurrentInterruptTaskId(activeTaskIds: string[]): void {
  if (!currentInterruptTaskId) {
    return;
  }
  if (!activeTaskIds.includes(currentInterruptTaskId)) {
    clearCurrentInterruptTaskId();
  }
}

export function extractBootstrapAgentRunTrackingHints(payload: BootstrapPayload, rawPayload: unknown): BootstrapAgentRunTrackingHints {
  const rawBootstrap = asBridgeRecord(rawPayload);
  const expectedSessionId = trimBridgeString(payload.sessionId);

  const activeTaskIds = new Set<string>();
  let rootTaskId = '';

  const rawRuntimeReadModel = asBridgeRecord(rawBootstrap?.runtimeReadModel);
  const runtimeDetails = asBridgeRecord(rawRuntimeReadModel?.details);
  const runtimeTasks = Array.isArray(runtimeDetails?.tasks)
    ? runtimeDetails.tasks
      .map((entry) => asBridgeRecord(entry))
      .filter((entry): entry is Record<string, unknown> => entry !== null)
    : [];
  const runtimeTaskMap = new Map<string, Record<string, unknown>>();
  for (const task of runtimeTasks) {
    const taskId = trimBridgeString(task.task_id);
    if (taskId) {
      runtimeTaskMap.set(taskId, task);
    }
  }
  const runtimeExecutionGroups = Array.isArray(runtimeDetails?.execution_groups)
    ? runtimeDetails.execution_groups
      .map((entry) => asBridgeRecord(entry))
      .filter((entry): entry is Record<string, unknown> => entry !== null)
    : [];
  const runtimeExecutionGroupMap = new Map<string, Record<string, unknown>>();
  for (const group of runtimeExecutionGroups) {
    const missionId = trimBridgeString(group.mission_id);
    if (missionId) {
      runtimeExecutionGroupMap.set(missionId, group);
    }
  }
  const runtimeSessions = Array.isArray(runtimeDetails?.sessions)
    ? runtimeDetails.sessions
      .map((entry) => asBridgeRecord(entry))
      .filter((entry): entry is Record<string, unknown> => entry !== null)
    : [];
  const activeRuntimeSession = runtimeSessions.find((entry) => {
    const sessionId = trimBridgeString(entry.session_id);
    return expectedSessionId ? sessionId === expectedSessionId : sessionId.length > 0;
  });
  const trackableRootTaskId = (candidate: unknown): string => {
    const taskId = trimBridgeString(candidate);
    if (!taskId) {
      return '';
    }
    return runtimeTaskMap.has(taskId) ? taskId : '';
  };
  rootTaskId = trackableRootTaskId(activeRuntimeSession?.root_task_id)
    || trackableRootTaskId(activeRuntimeSession?.rootTaskId);
  const overview = asBridgeRecord(rawRuntimeReadModel?.overview);
  const activity = asBridgeRecord(overview?.activity);
  const sessionTaskIds = normalizeBridgeStringArray(activeRuntimeSession?.active_task_ids);
  const sessionMissionIds = new Set(normalizeBridgeStringArray(activeRuntimeSession?.active_execution_group_ids));
  for (const taskId of sessionTaskIds) {
    const missionId = trimBridgeString(runtimeTaskMap.get(taskId)?.mission_id);
    if (missionId) {
      sessionMissionIds.add(missionId);
    }
  }

  const collectActiveTaskId = (taskId: string) => {
    if (!taskId) {
      return;
    }
    const taskEntry = runtimeTaskMap.get(taskId);
    if (taskEntry && isTerminalRuntimeTaskStatus(taskEntry.current_status)) {
      return;
    }
    activeTaskIds.add(taskId);
  };

  for (const taskId of sessionTaskIds) {
    collectActiveTaskId(taskId);
  }
  for (const missionId of normalizeBridgeStringArray(activeRuntimeSession?.active_execution_group_ids)) {
    const group = runtimeExecutionGroupMap.get(missionId);
    for (const taskId of normalizeBridgeStringArray(group?.active_task_ids)) {
      collectActiveTaskId(taskId);
    }
  }
  if (sessionMissionIds.size > 0) {
    for (const taskId of normalizeBridgeStringArray(activity?.active_task_ids)) {
      const taskEntry = runtimeTaskMap.get(taskId);
      const missionId = trimBridgeString(taskEntry?.mission_id);
      if (!sessionMissionIds.has(missionId)) {
        continue;
      }
      collectActiveTaskId(taskId);
    }
  }

  const recentEvents = Array.isArray(rawBootstrap?.recentEvents)
    ? rawBootstrap.recentEvents
      .map((entry) => asBridgeRecord(entry))
      .filter((entry): entry is Record<string, unknown> => entry !== null)
    : [];
  for (let index = recentEvents.length - 1; index >= 0; index -= 1) {
    const event = recentEvents[index];
    const eventPayload = asBridgeRecord(event.payload);
    const eventSessionId = trimBridgeString(event.session_id) || trimBridgeString(eventPayload?.session_id);
    const eventTaskId = trimBridgeString(event.task_id) || trimBridgeString(eventPayload?.task_id) || trimBridgeString(eventPayload?.taskId);
    const eventMissionId = trimBridgeString(event.mission_id) || trimBridgeString(eventPayload?.mission_id) || trimBridgeString(eventPayload?.missionId);
    if (expectedSessionId) {
      const belongsToExpectedSession = eventSessionId === expectedSessionId
        || (eventTaskId && sessionTaskIds.includes(eventTaskId))
        || (eventMissionId && sessionMissionIds.has(eventMissionId));
      if (!belongsToExpectedSession) {
        continue;
      }
    }
    const eventRootTaskId = trimBridgeString(eventPayload?.root_task_id) || trimBridgeString(eventPayload?.rootTaskId);
    const trackableEventRootTaskId = trackableRootTaskId(eventRootTaskId);
    if (trackableEventRootTaskId) {
      rootTaskId = trackableEventRootTaskId;
      break;
    }
  }
  return {
    rootTaskId,
    activeTaskIds: [...activeTaskIds],
  };
}

function shouldRecoverFromBridgeError(error: unknown): boolean {
  if (error instanceof AgentApiError) {
    if (error.errorCode === 'MODEL_INVOCATION_FAILED') {
      return false;
    }
    return error.status >= 500;
  }
  return true;
}

function isSessionMissingError(error: unknown): boolean {
  if (error instanceof AgentApiError) {
    if (error.errorCode === 'SESSION_NOT_FOUND') return true;
    if (error.status === 404) return true;
  }
  const detail = normalizeErrorMessage(error) || '';
  return detail.includes('SESSION_NOT_FOUND') || detail.includes('会话不存在');
}

function isExpectedRecoveryBridgeFailure(error: unknown): boolean {
  if (error instanceof AgentApiError) {
    return error.status >= 500;
  }
  const detail = normalizeErrorMessage(error)?.toLowerCase() || '';
  if (!detail) {
    return false;
  }
  return detail.includes('failed to fetch')
    || detail.includes('fetch failed')
    || detail.includes('networkerror')
    || detail.includes('network error')
    || detail.includes('connection refused')
    || detail.includes('bootstrap failed: 500')
    || detail.includes('local agent')
    || detail.includes('magi 暂时无法连接')
    || detail.includes('当前页面暂时无法连接 magi');
}

function emitForcedProcessingTerminal(input: {
  sessionId: string;
  requestId: string;
  reason: string;
  details?: Record<string, unknown>;
}): void {
  const sessionId = trimBridgeString(input.sessionId);
  const requestId = trimBridgeString(input.requestId);
  if (!sessionId || !requestId) {
    console.warn('[web-client-bridge] 忽略缺少 sessionId/requestId 的 forced terminal', {
      reason: input.reason,
    });
    return;
  }
  emitDataMessage('processingStateChanged', {
    isProcessing: false,
    transitionKind: 'forced',
    source: 'orchestrator',
    agent: 'orchestrator',
    reason: input.reason,
    sessionId,
    requestId,
    timestamp: Date.now(),
    ...(input.details || {}),
  });
}

function captureProcessingRequestSnapshot(sessionId?: string | null): ProcessingRequestSnapshot {
  const normalizedSessionId = trimBridgeString(sessionId)
    || trimBridgeString(messagesState.currentSessionId)
    || '__draft__';
  return {
    sessionId: normalizedSessionId,
    requestIds: listPendingRequestIdsForSession(normalizedSessionId),
  };
}

function settleProcessingRequestSnapshot(snapshot: ProcessingRequestSnapshot): void {
  if (snapshot.requestIds.length === 0) {
    // 没有 requestId 的历史 canonical processing 仍可能阻塞界面；只有在
    // 当前 session 此刻没有新 pending request 时才允许收敛全局 processing。
    if (
      snapshot.sessionId === (messagesState.currentSessionId?.trim() || '__draft__')
      && messagesState.pendingRequests.size === 0
      && messagesState.backendProcessing
    ) {
      settleAuthoritativeIdleState();
    }
    return;
  }
  settlePendingRequestSnapshot(snapshot);
}

function refreshBootstrapAfterTerminalTurn(reason: string): void {
  terminalBootstrapRefreshPending = true;
  terminalBootstrapRefreshReason = reason;
  if (terminalBootstrapRefreshInFlight) {
    return;
  }

  const request = (async (): Promise<void> => {
    while (terminalBootstrapRefreshPending) {
      terminalBootstrapRefreshPending = false;
      const refreshReason = terminalBootstrapRefreshReason || 'terminal_bootstrap_refresh';
      terminalBootstrapRefreshReason = '';
      try {
        // 终态事件已经直接驱动 canonical reducer。bootstrap 只做后台权威校正，
        // 失败时不得重新猜测或清除当前 UI 的 processing 状态。
        await fetchBootstrap({
          forceFresh: true,
          refreshSettingsBootstrapOnBindingChange: false,
          refreshSessionTurnQueueAfterBootstrap: false,
          settleProcessingOnFailure: false,
        });
      } catch (error) {
        reportExpectedRecoveryFailure(
          i18n.t('bridge.action.syncTurnState'),
          '[web-client-bridge] turn 终态后 bootstrap 同步失败:',
          error,
        );
        scheduleRecovery(refreshReason, error, true);
      }
    }
  })().finally(() => {
    terminalBootstrapRefreshInFlight = null;
    // 终态刷新期间可能发生新的 terminal event；finally 之后重新启动一轮，
    // 确保会话切换或连续轮次不会被前一轮吞掉。
    if (terminalBootstrapRefreshPending) {
      refreshBootstrapAfterTerminalTurn(terminalBootstrapRefreshReason || 'terminal_bootstrap_refresh');
    }
  });
  terminalBootstrapRefreshInFlight = request;
}

function emitRecoveringState(reason: string, error?: unknown): void {
  bridgeRecovering = true;
  if (error !== undefined) {
    console.warn('[web-client-bridge] 连接恢复已触发:', reason, error);
  }
  dispatchAgentConnectionEvent({
    status: 'recovering',
    reason,
    baseUrl: resolveAgentBaseUrl(),
  });
}

function emitConnectedState(reason: string, recovered: boolean): void {
  bridgeRecovering = false;
  dispatchAgentConnectionEvent({
    status: 'connected',
    reason,
    recovered,
    baseUrl: resolveAgentBaseUrl(),
  });
}

function clearRecoveryTimer(): void {
  if (recoveryTimer !== null) {
    window.clearTimeout(recoveryTimer);
    recoveryTimer = null;
  }
}

function clearEventStreamOpenTimeout(): void {
  if (activeEventStreamOpenTimeout !== null) {
    window.clearTimeout(activeEventStreamOpenTimeout);
    activeEventStreamOpenTimeout = null;
  }
}

function markEventStreamActive(): void {
  lastEventStreamActivityAt = Date.now();
}

function currentEventStreamIdleTimeoutMs(): number {
  if (isPublicTunnelAccess()) {
    return EVENT_STREAM_TUNNEL_IDLE_TIMEOUT_MS;
  }
  return EVENT_STREAM_IDLE_TIMEOUT_MS;
}

function currentEventStreamIdleCheckIntervalMs(): number {
  if (isPublicTunnelAccess()) {
    return EVENT_STREAM_TUNNEL_IDLE_CHECK_INTERVAL_MS;
  }
  return EVENT_STREAM_IDLE_CHECK_INTERVAL_MS;
}

function refreshBootstrapForSilentEventStream(reason: string, error: Error): void {
  const refreshBindingKey = bootstrapBindingKey(resolveWorkspaceQuery());
  if (
    eventStreamSnapshotRefreshInFlight
    && eventStreamSnapshotRefreshBindingKey === refreshBindingKey
  ) {
    return;
  }
  eventStreamSnapshotRefreshBindingKey = refreshBindingKey;
  const request = fetchBootstrap({
    forceFresh: true,
    forceEventStreamReconnect: false,
    refreshSettingsBootstrapOnBindingChange: false,
  }).catch((refreshError) => {
    scheduleRecovery(reason, refreshError || error, true);
  }).finally(() => {
    if (eventStreamSnapshotRefreshInFlight === request) {
      eventStreamSnapshotRefreshInFlight = null;
      eventStreamSnapshotRefreshBindingKey = '';
    }
  });
  eventStreamSnapshotRefreshInFlight = request;
}

function syncTunnelRuntimeForSilentEventStream(reason: string, error: Error): void {
  const binding = resolveWorkspaceQuery();
  const syncBindingKey = bootstrapBindingKey(binding);
  if (
    eventStreamTunnelSyncInFlight
    && eventStreamTunnelSyncBindingKey === syncBindingKey
  ) {
    return;
  }
  eventStreamTunnelSyncBindingKey = syncBindingKey;
  const request = (async (): Promise<void> => {
    if (binding.scope !== 'workspace' || !binding.workspaceId || !binding.sessionId) {
      return;
    }
    const snapshot = await getWorkspaceSessions(
      binding.workspaceId,
      binding.workspacePath,
    );
    if (bootstrapBindingKey(resolveWorkspaceQuery()) !== syncBindingKey) {
      return;
    }
    const currentSession = snapshot.sessions.find((session) => session.id === binding.sessionId);
    if (currentSession?.isRunning !== false) {
      emitDataMessage('sessionsUpdated', {
        workspaceId: binding.workspaceId,
        sessions: snapshot.sessions,
        runtimeEpoch: snapshot.runtimeEpoch,
        eventStreamNextSequence: snapshot.eventStreamNextSequence,
      });
      return;
    }
    await fetchBootstrap({
      forceFresh: true,
      forceEventStreamReconnect: false,
      refreshSettingsBootstrapOnBindingChange: false,
      refreshWorkspaceSessionsAfterBootstrap: false,
    });
  })().catch((syncError) => {
    scheduleRecovery(reason, syncError || error, true);
  }).finally(() => {
    if (eventStreamTunnelSyncInFlight === request) {
      eventStreamTunnelSyncInFlight = null;
      eventStreamTunnelSyncBindingKey = '';
    }
  });
  eventStreamTunnelSyncInFlight = request;
}

function stopEventStreamIdleCheck(): void {
  if (eventStreamIdleCheckTimer !== null) {
    window.clearInterval(eventStreamIdleCheckTimer);
    eventStreamIdleCheckTimer = null;
  }
}

function startEventStreamIdleCheck(): void {
  if (typeof window === 'undefined') {
    return;
  }
  stopEventStreamIdleCheck();
  markEventStreamActive();
  eventStreamIdleCheckTimer = window.setInterval(() => {
    if (activeEventStreamState !== 'open') {
      return;
    }
    if (recoveryInFlight || recoveryTimer !== null) {
      return;
    }
    const idleMs = Date.now() - lastEventStreamActivityAt;
    const isTunnelAccess = isPublicTunnelAccess();
    const runtimeBusy = bridgeRuntimeIsBusy();
    if (
      isTunnelAccess
      && runtimeBusy
      && idleMs >= EVENT_STREAM_TUNNEL_BUSY_REFRESH_INTERVAL_MS
    ) {
      markEventStreamActive();
      syncTunnelRuntimeForSilentEventStream(
        'event_stream_tunnel_busy_snapshot',
        new Error(`Tunnel SSE 静默刷新：${Math.round(idleMs / 1000)}s`),
      );
      return;
    }
    const timeoutMs = currentEventStreamIdleTimeoutMs();
    if (idleMs < timeoutMs) {
      return;
    }
    // SSE 握手仍 open，但超过容错窗口没收到任何事件（含 keep-alive），判定静默断流。
    // 重置活跃时间戳避免 recovery 调度期间重复触发，由 recovery 完成后的 ensureEventStream
    // 重新建连或 closeEventStream 停止检测。
    markEventStreamActive();
    const reason = runtimeBusy ? 'event_stream_active_idle' : 'event_stream_idle';
    const error = new Error(`SSE 静默超时：${Math.round(idleMs / 1000)}s`);
    if (isTunnelAccess) {
      refreshBootstrapForSilentEventStream(reason, error);
      return;
    }
    scheduleRecovery(reason, error, true);
  }, currentEventStreamIdleCheckIntervalMs());
}

function ensureWindowListener(): void {
  if (bridgeListenerRegistered || typeof window === 'undefined') {
    return;
  }
  bridgeListenerRegistered = true;

  window.addEventListener('message', (event) => {
    const message = event.data as ClientBridgeMessage;
    if (!message || typeof message !== 'object' || !message.type) return;
    // 传输层内部消息（agentApiProxyResponse / agentSseEvent / agentSseStatus）
    // 已由 transport.ts 的 window message 监听器处理，此处跳过
    if (message.type === 'agentApiProxyResponse'
      || message.type === 'agentSseEvent'
      || message.type === 'agentSseStatus') {
      return;
    }
    emitMessage(message);
    // sessionBootstrapLoaded 的导航响应由 data-message-handlers 在严格校验
    // requestId/target 后提交。这里不能在处理器之前写入 binding，也不能让
    // 迟到快照在仍有导航事务时污染当前会话。
    syncBindingFromBridgeMessage(message);
  });
  window.addEventListener('storage', (event) => {
    if (event.key !== RUNTIME_BASE_URL_STORAGE_KEY) {
      return;
    }
    closeEventStream();
    scheduleRecovery('runtime_base_url_changed', undefined, true);
  });
  window.addEventListener('pagehide', () => {
    flushPersistedWebviewState();
  });
  window.addEventListener('beforeunload', () => {
    flushPersistedWebviewState();
  });
  window.addEventListener('focus', () => {
    if (activeEventStreamState !== 'open' && (currentWorkspaceId || currentWorkspacePath || currentSessionId)) {
      scheduleRecovery('window_focus', undefined, true);
    }
  });
}

function extractSessionBootstrapBinding(
  message: ClientBridgeMessage,
): AgentBindingContext | null {
  if (message.type !== 'unifiedMessage') {
    return null;
  }
  const standard = message.message as StandardMessage | undefined;
  if (!standard || standard.category !== MessageCategory.DATA) {
    return null;
  }
  if (standard.data?.dataType !== 'sessionBootstrapLoaded') {
    return null;
  }
  const payload = standard.data?.payload;
  if (!payload || typeof payload !== 'object') {
    return null;
  }
  const payloadRecord = payload as Record<string, unknown>;
  const workspaceRecord = payloadRecord.workspace && typeof payloadRecord.workspace === 'object'
    ? payloadRecord.workspace as Record<string, unknown>
    : undefined;
  const sessionId = typeof payloadRecord.sessionId === 'string' ? payloadRecord.sessionId.trim() : '';
  if (payloadRecord.scope === 'workspace') {
    const workspaceId = typeof workspaceRecord?.workspaceId === 'string'
      ? workspaceRecord.workspaceId.trim()
      : '';
    const workspacePath = typeof workspaceRecord?.rootPath === 'string'
      ? workspaceRecord.rootPath.trim()
      : '';
    if (!workspaceId && !workspacePath) return null;
    return {
        scope: 'workspace',
        workspaceId,
        workspacePath,
        ...(sessionId ? { sessionId } : {}),
      };
  }
  if (payloadRecord.scope !== 'personal') return null;
  return { scope: 'personal', ...(sessionId ? { sessionId } : {}) };
}

function syncBindingFromBridgeMessage(message: ClientBridgeMessage): void {
  if (sessionNavigationState.pending) {
    return;
  }
  const binding = extractSessionBootstrapBinding(message);
  if (!binding) return;
  const nextSessionId = binding.sessionId;
  const nextWorkspaceId = agentBindingWorkspaceId(binding);
  const nextWorkspacePath = agentBindingWorkspacePath(binding);
  if (!nextSessionId) {
    return;
  }
  const bindingChanged = nextSessionId !== currentSessionId
    || binding.scope !== currentSessionScope
    || nextWorkspaceId !== currentWorkspaceId
    || nextWorkspacePath !== currentWorkspacePath;
  if (!bindingChanged) {
    return;
  }
  persistWorkspaceBinding(binding.scope, nextWorkspaceId, nextWorkspacePath, nextSessionId);
  ensureEventStream();
}

function emitMessage(message: ClientBridgeMessage): void {
 // SSE 首帧 runtimeEpoch：检测后端代际变化，但禁止整页刷新。
 if (message.type === 'runtimeEpoch') {
   const incomingEpoch = typeof message.epoch === 'string' ? message.epoch : '';
   if (incomingEpoch && currentRuntimeEpoch && incomingEpoch !== currentRuntimeEpoch) {
     console.warn('[web-client-bridge] SSE runtimeEpoch 变化，后端已重启，执行无刷新桥恢复', {
       previous: currentRuntimeEpoch,
       current: incomingEpoch,
     });
     resetEventStreamCursor();
     currentRuntimeEpoch = incomingEpoch;
     closeEventStream();
     scheduleRecovery('runtime_epoch_changed', undefined, true);
     return;
   }
   if (incomingEpoch) {
     currentRuntimeEpoch = incomingEpoch;
   }
   return; // runtimeEpoch 是内部控制消息，不广播给前端组件
 }
  if (listeners.size === 0) {
    pendingBridgeMessages.push(message);
    if (pendingBridgeMessages.length > MAX_PENDING_BRIDGE_MESSAGES) {
      pendingBridgeMessages.splice(0, pendingBridgeMessages.length - MAX_PENDING_BRIDGE_MESSAGES);
    }
    return;
  }
  listeners.forEach((listener) => {
    try {
      listener(message);
    } catch (error) {
      console.error('[web-client-bridge] 消息处理错误:', error);
    }
  });
}

function emitDataMessage(dataType: DataMessageType, payload: Record<string, unknown>): void {
  const now = Date.now();
  const message: StandardMessage = {
    id: `web-data-${dataType}-${now}`,
    traceId: `web-data-${dataType}`,
    category: MessageCategory.DATA,
    type: MessageType.SYSTEM,
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [],
    metadata: {},
    timestamp: now,
    updatedAt: now,
    data: {
      dataType,
      payload,
    },
  };
  emitMessage({ type: 'unifiedMessage', message });
}

function emitSessionTurnAccepted(
  payload: {
    sessionId: string;
    workspaceId?: string;
    requestId?: string;
    runtimeEpoch?: string;
    eventStreamNextSequence?: number;
    acceptedAt?: number;
    sessionSummary?: unknown;
    createdSession?: boolean;
    route?: unknown;
    canonicalSchemaVersion?: string | null;
    canonicalEventKind?: string | null;
    canonicalEventId?: string | null;
    canonicalEventSeq?: number | null;
    canonicalOccurredAt?: number | null;
    canonicalTurn?: unknown;
    canonicalItem?: unknown;
    submissionContext?: SessionTurnSubmissionContext | null;
  },
): void {
  const submissionContext = payload.submissionContext;
  emitDataMessage('sessionTurnAccepted', {
    sessionId: payload.sessionId,
    workspaceId: payload.workspaceId || '',
    ...(payload.requestId ? { requestId: payload.requestId } : {}),
    ...(payload.runtimeEpoch ? { runtimeEpoch: payload.runtimeEpoch } : {}),
    ...(typeof payload.eventStreamNextSequence === 'number'
      ? { eventStreamNextSequence: payload.eventStreamNextSequence }
      : {}),
    ...(typeof payload.acceptedAt === 'number' ? { acceptedAt: payload.acceptedAt } : {}),
    ...(payload.sessionSummary === undefined ? {} : { sessionSummary: payload.sessionSummary }),
    ...(payload.createdSession === undefined ? {} : { createdSession: payload.createdSession }),
    ...(payload.route === undefined ? {} : { route: payload.route }),
    ...(payload.canonicalSchemaVersion ? { canonicalSchemaVersion: payload.canonicalSchemaVersion } : {}),
    ...(payload.canonicalEventKind ? { canonicalEventKind: payload.canonicalEventKind } : {}),
    ...(payload.canonicalEventId ? { canonicalEventId: payload.canonicalEventId } : {}),
    ...(typeof payload.canonicalEventSeq === 'number'
      ? { canonicalEventSeq: payload.canonicalEventSeq }
      : {}),
    ...(typeof payload.canonicalOccurredAt === 'number'
      ? { canonicalOccurredAt: payload.canonicalOccurredAt }
      : {}),
    ...(payload.canonicalTurn === undefined ? {} : { canonicalTurn: payload.canonicalTurn }),
    ...(payload.canonicalItem === undefined ? {} : { canonicalItem: payload.canonicalItem }),
    ...(submissionContext ? { submissionContext: submissionContextPayload(submissionContext) } : {}),
  });
}

function emitSessionTurnSubmissionSettled(
  requestId: string,
  status: 'accepted' | 'failed',
): void {
  const normalizedRequestId = requestId.trim();
  if (!normalizedRequestId) return;
  window.dispatchEvent(new CustomEvent('magi:sessionTurnSubmissionSettled', {
    detail: { requestId: normalizedRequestId, status },
  }));
}

function parseCanonicalTurnEventFromRustEvent(
  event: RustEventEnvelope,
): CanonicalTurnEvent | undefined {
  const payload = event.payload;
  if (!payload) {
    return undefined;
  }

  // canonical turn 事实嵌在 EventBus envelope 中传输。事件生产者只需提供
  // turn/item 内容，传输层 envelope 才是实时事件 ID、顺序和时间的权威来源。
  try {
    return parseCanonicalTurnEventPayload({
      ...payload,
      canonical_event_id: payload.canonical_event_id
        ?? payload.canonicalEventId
        ?? payload.eventId
        ?? payload.event_id
        ?? event.event_id,
      canonical_event_seq: payload.canonical_event_seq
        ?? payload.canonicalEventSeq
        ?? payload.eventSeq
        ?? payload.event_seq
        ?? event.sequence,
      canonical_occurred_at: payload.canonical_occurred_at
        ?? payload.canonicalOccurredAt
        ?? payload.occurredAt
        ?? payload.occurred_at
        ?? event.occurred_at,
    });
  } catch (error) {
    setCanonicalTimelineError(error);
    console.error('[web-client-bridge] canonical event 校验失败，已拒绝 live projection:', error);
    scheduleRecovery('canonical_protocol_invalid', error, true);
    return undefined;
  }
}

function emitSessionTurnCanonicalEvent(canonicalEvent: CanonicalTurnEvent): void {
  const isTerminalEvent = isCanonicalTerminalEvent(canonicalEvent);
  if (
    canonicalEvent.sessionId === currentSessionId
    && isTerminalEvent
  ) {
    const requestId = canonicalEvent.turn
      ? canonicalTurnRequestId(canonicalEvent.turn)
      : '';
    if (requestId) {
      clearContinueRequestInFlight(requestId);
    }
    // canonical 终态事件是会话执行结束的统一协议入口。增量 reducer 负责即时收敛，
    // 权威 bootstrap 立即在后台校正任务、代理与运行态快照。
    refreshBootstrapAfterTerminalTurn('canonical_turn_terminal');
  }
  emitDataMessage('sessionTurnCanonicalEventUpdated', {
    sessionId: canonicalEvent.sessionId,
    canonicalEvent,
  });
}

function isCanonicalTerminalEvent(canonicalEvent: CanonicalTurnEvent): boolean {
  return canonicalEvent.kind === 'turn_completed'
    || canonicalEvent.kind === 'turn_superseded'
    || Boolean(
      canonicalEvent.turn
      && isCanonicalTerminalStatus(canonicalEvent.turn.status),
    );
}

function canonicalTurnSeqFromResult(result: {
  canonicalTurn?: unknown;
  canonicalItem?: unknown;
}): number | undefined {
  const turnSeqCandidates = [
    result.canonicalTurn && typeof result.canonicalTurn === 'object'
      ? (result.canonicalTurn as Record<string, unknown>).turnSeq
      : undefined,
    result.canonicalItem && typeof result.canonicalItem === 'object'
      ? (result.canonicalItem as Record<string, unknown>).turnSeq
      : undefined,
  ];
  const canonicalTurnSeq = turnSeqCandidates.find((value) => (
    typeof value === 'number'
    && Number.isSafeInteger(value)
    && value > 0
  ));
  return typeof canonicalTurnSeq === 'number' ? canonicalTurnSeq : undefined;
}

function handleSessionTurnItemEvent(event: RustEventEnvelope): boolean {
  const canonicalEvent = parseCanonicalTurnEventFromRustEvent(event);
  if (!canonicalEvent) {
    console.error('[web-client-bridge] session.turn.item 缺少 canonical payload，已拒绝旧 projection live 写入');
    return false;
  }
  emitSessionTurnCanonicalEvent(canonicalEvent);
  return true;
}

function emitCanonicalTurnEventFromRustEvent(event: RustEventEnvelope): CanonicalTurnEvent | null {
  const canonicalEvent = parseCanonicalTurnEventFromRustEvent(event);
  if (!canonicalEvent) {
    return null;
  }
  emitSessionTurnCanonicalEvent(canonicalEvent);
  return canonicalEvent;
}

function rustEventPayloadString(event: RustEventEnvelope, snakeKey: string, camelKey: string): string {
  return trimBridgeString(event.payload?.[snakeKey])
    || trimBridgeString(event.payload?.[camelKey]);
}

function rustEventWorkspaceId(event: RustEventEnvelope): string {
  return trimBridgeString(event.workspace_id)
    || rustEventPayloadString(event, 'workspace_id', 'workspaceId');
}

function rustEventWorkspacePath(event: RustEventEnvelope): string {
  return rustEventPayloadString(event, 'workspace_path', 'workspacePath');
}

function rustEventSessionId(event: RustEventEnvelope): string {
  return trimBridgeString(event.session_id)
    || rustEventPayloadString(event, 'session_id', 'sessionId');
}

function rustEventTaskId(event: RustEventEnvelope): string {
  return trimBridgeString(event.task_id)
    || rustEventPayloadString(event, 'task_id', 'taskId');
}

function rustEventRequestId(event: RustEventEnvelope): string {
  return rustEventPayloadString(event, 'request_id', 'requestId');
}

function terminalTurnIdentity(
  event: RustEventEnvelope,
  canonicalEvent: CanonicalTurnEvent | null,
): { sessionId: string; requestId: string } | null {
  const eventSessionId = rustEventSessionId(event);
  const canonicalSessionId = canonicalEvent?.sessionId.trim() || '';
  if (
    !eventSessionId
    || (canonicalSessionId && canonicalSessionId !== eventSessionId)
  ) {
    return null;
  }

  const eventRequestId = rustEventRequestId(event);
  const canonicalRequestId = canonicalEvent?.turn
    ? canonicalTurnRequestId(canonicalEvent.turn)
    : '';
  if (
    eventRequestId
    && canonicalRequestId
    && eventRequestId !== canonicalRequestId
  ) {
    return null;
  }
  const requestId = canonicalRequestId || eventRequestId;
  return requestId ? { sessionId: eventSessionId, requestId } : null;
}

function activeDraftSubmissionMatchesAcceptedEvent(
  event: RustEventEnvelope,
  eventSessionId: string,
): boolean {
  const requestId = rustEventRequestId(event);
  const context = sessionTurnSubmissions.get(requestId);
  return Boolean(
    event.event_type === 'session.turn.task.accepted'
    && eventSessionId
    && context
    && !context.targetSessionId
    && context.requestId === requestId
    && context.bindingGeneration === currentBindingGeneration
    && messagesState.pendingRequests.has(requestId)
    && getRequestBinding(requestId)
    && submissionContextMatchesCurrentScope(context)
    && !currentSessionId
    && !currentStoreSessionId(),
  );
}

function rustTaskEventRootTaskIds(event: RustEventEnvelope): string[] {
  const candidates = [
    rustEventPayloadString(event, 'root_task_id', 'rootTaskId'),
    rustEventPayloadString(event, 'old_root_task_id', 'oldRootTaskId'),
    rustEventPayloadString(event, 'new_root_task_id', 'newRootTaskId'),
  ];
  return Array.from(new Set(candidates.filter((value) => value.length > 0)));
}

function eventMatchesCurrentWorkspace(event: RustEventEnvelope): boolean {
  const eventWorkspaceId = rustEventWorkspaceId(event);
  if (eventWorkspaceId && currentWorkspaceId && eventWorkspaceId !== currentWorkspaceId) {
    return false;
  }
  return true;
}

function eventTargetsDifferentSession(event: RustEventEnvelope): boolean {
  const eventSessionId = rustEventSessionId(event);
  if (
    (event.event_type === 'session.turn.task.accepted' || TURN_TERMINAL_EVENTS.has(event.event_type || ''))
    && !eventSessionId
  ) {
    return true;
  }
  if (!eventSessionId) {
    return false;
  }
  if (currentSessionId) {
    return eventSessionId !== currentSessionId;
  }

  // 草稿态没有已提交的会话 ID。只有当前 submission context 的 requestId
  // 可以建立会话；requestBindings 或 pendingRequests 单独匹配都不够严格。
  return !activeDraftSubmissionMatchesAcceptedEvent(event, eventSessionId);
}

function shouldApplyCurrentSessionRustEvent(event: RustEventEnvelope): boolean {
  if (!eventMatchesCurrentWorkspace(event)) {
    return false;
  }
  if (eventTargetsDifferentSession(event)) {
    return false;
  }
  return true;
}

function currentSessionSummaryRefreshTarget(): SessionSummaryRefreshTarget {
  if (currentSessionScope === 'workspace' && currentWorkspaceId) {
    return {
      scope: 'workspace',
      key: `workspace:${currentWorkspaceId}`,
      workspaceId: currentWorkspaceId,
      workspacePath: currentWorkspacePath,
    };
  }
  return { scope: 'personal', key: 'personal' };
}

function sessionSummaryRefreshTargetForEvent(event: RustEventEnvelope): SessionSummaryRefreshTarget {
  const workspaceId = rustEventWorkspaceId(event);
  if (workspaceId) {
    return {
      scope: 'workspace',
      key: `workspace:${workspaceId}`,
      workspaceId,
      workspacePath: rustEventWorkspacePath(event),
    };
  }
  return currentSessionSummaryRefreshTarget();
}

function scheduleSessionSummaryRefresh(
  reason: string,
  target: SessionSummaryRefreshTarget = currentSessionSummaryRefreshTarget(),
): void {
  sessionSummaryRefreshTargets.set(target.key, target);
  if (sessionSummaryRefreshTimer) {
    return;
  }
  sessionSummaryRefreshTimer = setTimeout(() => {
    sessionSummaryRefreshTimer = null;
    const targets = Array.from(sessionSummaryRefreshTargets.values());
    sessionSummaryRefreshTargets.clear();
    for (const refreshTarget of targets) {
      const request = refreshTarget.scope === 'workspace'
        ? getWorkspaceSessions(refreshTarget.workspaceId, refreshTarget.workspacePath)
        : getPersonalSessions();
      void request.then((snapshot) => {
        emitDataMessage('sessionsUpdated', {
          ...(refreshTarget.scope === 'workspace' ? { workspaceId: refreshTarget.workspaceId } : {}),
          sessions: snapshot.sessions,
          runtimeEpoch: snapshot.runtimeEpoch,
          eventStreamNextSequence: snapshot.eventStreamNextSequence,
        });
      }).catch((error) => {
        reportExpectedRecoveryFailure(
          i18n.t('bridge.action.syncMessages'),
          `[web-client-bridge] 会话事件后刷新会话列表失败(${reason}, ${refreshTarget.key}):`,
          error,
        );
        scheduleRecovery(reason, error, true);
      });
    }
  }, 300);
}

function shouldRefreshExternalWorkspaceSessionSummary(eventType: string, event: RustEventEnvelope): boolean {
  if (!EXTERNAL_SESSION_SUMMARY_EVENTS.has(eventType)) {
    return false;
  }
  const eventWorkspaceId = rustEventWorkspaceId(event);
  return eventTargetsDifferentSession(event)
    || Boolean(eventWorkspaceId && eventWorkspaceId !== currentWorkspaceId);
}

function shouldRefreshCurrentSessionSummary(eventType: string): boolean {
  return eventType === 'message.created'
    || eventType === 'session.viewed'
    || TURN_TERMINAL_EVENTS.has(eventType)
    || eventType === 'session.turn.superseded';
}

function refreshCurrentSessionToolApprovals(reason: string): void {
  const sessionId = currentSessionId.trim();
  if (!sessionId) {
    return;
  }
  const binding = currentSessionScope === 'workspace'
    ? {
        scope: 'workspace' as const,
        workspaceId: currentWorkspaceId,
        workspacePath: currentWorkspacePath,
      }
    : { scope: 'personal' as const };
  void syncToolApprovals(sessionId, binding).catch((error) => {
    console.warn(`[web-client-bridge] 权限状态同步失败(${reason}):`, error);
  });
}

const TURN_TERMINAL_EVENTS = new Set([
  'session.turn.completed',
  'session.turn.failed',
  'session.turn.interrupted',
  'session.turn.queue_failed',
]);

function readFiniteEventNumber(
  payload: Record<string, unknown>,
  snakeKey: string,
  camelKey: string,
): number | undefined {
  const value = payload[snakeKey] ?? payload[camelKey];
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function applyContextBudgetRuntimeEvent(event: RustEventEnvelope): void {
  const payload = event.payload;
  if (!payload) return;
  const tokenUsed = readFiniteEventNumber(payload, 'projected_request_tokens', 'projectedRequestTokens')
    ?? readFiniteEventNumber(payload, 'token_used', 'tokenUsed');
  const tokenLimit = readFiniteEventNumber(payload, 'context_window_limit_tokens', 'contextWindowLimitTokens')
    ?? readFiniteEventNumber(payload, 'context_window_tokens', 'contextWindowTokens')
    ?? readFiniteEventNumber(payload, 'token_limit', 'tokenLimit');
  const remainingTokens = readFiniteEventNumber(payload, 'remaining_tokens', 'remainingTokens');
  const usageRatio = readFiniteEventNumber(payload, 'usage_ratio', 'usageRatio');
  if (tokenUsed === undefined || tokenLimit === undefined || tokenLimit <= 0) return;

  const updatedAt = readFiniteEventNumber(payload, 'updated_at', 'updatedAt')
    ?? (typeof event.occurred_at === 'number' ? event.occurred_at : Date.now());
  const measurement = trimBridgeString(payload.measurement ?? payload.accuracy) === 'authoritative'
    ? 'authoritative'
    : 'estimated';
  const current = messagesState.orchestratorRuntimeState;
  const currentBudget = current?.runtimeSnapshot?.budgetState;
  const currentUpdatedAt = currentBudget?.updatedAt ?? 0;
  const currentEventSequence = currentBudget?.eventSequence ?? 0;
  const nextEventSequence = typeof event.sequence === 'number' ? Math.floor(event.sequence) : 0;
  const currentMeasurementRank = currentBudget?.measurement === 'authoritative' ? 1 : 0;
  const nextMeasurementRank = measurement === 'authoritative' ? 1 : 0;
  if (
    currentEventSequence > nextEventSequence
    || (currentEventSequence === nextEventSequence && currentUpdatedAt > updatedAt)
    || (
      currentEventSequence === nextEventSequence
      && currentUpdatedAt === updatedAt
      && currentMeasurementRank > nextMeasurementRank
    )
  ) {
    return;
  }

  const warningLevelValue = trimBridgeString(payload.warning_level ?? payload.warningLevel);
  const warningLevel = (
    warningLevelValue === 'normal'
    || warningLevelValue === 'notice'
    || warningLevelValue === 'warning'
    || warningLevelValue === 'danger'
    || warningLevelValue === 'compaction_due'
  ) ? (warningLevelValue === 'compaction_due' ? 'danger' : warningLevelValue) : undefined;
  const budgetState: NonNullable<OrchestratorRuntimeSnapshot['budgetState']> = {
    ...currentBudget,
    tokenUsed: Math.max(0, Math.floor(tokenUsed)),
    tokenLimit: Math.max(0, Math.floor(tokenLimit)),
    remainingTokens: Math.max(
      0,
      Math.floor(remainingTokens ?? Math.max(0, tokenLimit - tokenUsed)),
    ),
    usageRatio: Math.min(1, Math.max(0, usageRatio ?? tokenUsed / tokenLimit)),
    ...(warningLevel ? { warningLevel } : {}),
    measurement,
    updatedAt: Math.floor(updatedAt),
    eventSequence: nextEventSequence,
    projectedRequestTokens: Math.max(0, Math.floor(tokenUsed)),
    ...(trimBridgeString(payload.phase) ? { phase: trimBridgeString(payload.phase) } : {}),
    ...(trimBridgeString(payload.turn_id ?? payload.turnId)
      ? { turnId: trimBridgeString(payload.turn_id ?? payload.turnId) }
      : {}),
    ...(trimBridgeString(payload.call_id ?? payload.callId)
      ? { callId: trimBridgeString(payload.call_id ?? payload.callId) }
      : {}),
    ...(trimBridgeString(payload.resolved_model ?? payload.resolvedModel)
      ? { resolvedModel: trimBridgeString(payload.resolved_model ?? payload.resolvedModel) }
      : {}),
    ...(readFiniteEventNumber(payload, 'provider_context_tokens', 'providerContextTokens') != null
      ? { providerContextTokens: readFiniteEventNumber(payload, 'provider_context_tokens', 'providerContextTokens') }
      : {}),
    ...(readFiniteEventNumber(payload, 'proactive_threshold_tokens', 'proactiveThresholdTokens') != null
      ? { proactiveThresholdTokens: readFiniteEventNumber(payload, 'proactive_threshold_tokens', 'proactiveThresholdTokens') }
      : {}),
    ...(readFiniteEventNumber(payload, 'hard_request_limit_tokens', 'hardRequestLimitTokens') != null
      ? { hardRequestLimitTokens: readFiniteEventNumber(payload, 'hard_request_limit_tokens', 'hardRequestLimitTokens') }
      : {}),
    ...(trimBridgeString(payload.pressure_level ?? payload.pressureLevel)
      ? { pressureLevel: trimBridgeString(payload.pressure_level ?? payload.pressureLevel) }
      : {}),
  };
  const eventAt = Math.max(
    Math.floor(updatedAt),
    typeof event.occurred_at === 'number' ? Math.floor(event.occurred_at) : 0,
  );
  setOrchestratorRuntimeState({
    ...(current ?? {
      status: 'running',
      phase: budgetState.phase || 'modeling',
      errors: [],
      statusChangedAt: eventAt,
      lastEventAt: eventAt,
      assignments: [],
    }),
    sessionId: rustEventSessionId(event) || currentSessionId || current?.sessionId,
    lastEventAt: Math.max(current?.lastEventAt ?? 0, eventAt),
    runtimeSnapshot: {
      ...(current?.runtimeSnapshot ?? {}),
      budgetState,
    },
  });
}

function applyContextCompactionRuntimeEvent(event: RustEventEnvelope): void {
  const payload = event.payload;
  if (!payload) return;
  const compactedTokenEstimate = readFiniteEventNumber(
    payload,
    'compacted_token_estimate',
    'compactedTokenEstimate',
  );
  const tokenLimit = readFiniteEventNumber(payload, 'token_limit', 'tokenLimit')
    ?? messagesState.orchestratorRuntimeState?.runtimeSnapshot?.budgetState?.tokenLimit;
  if (compactedTokenEstimate === undefined || tokenLimit === undefined) return;
  const requestTokenEstimate = readFiniteEventNumber(
    payload,
    'request_token_estimate',
    'requestTokenEstimate',
  ) ?? compactedTokenEstimate;
  applyContextBudgetRuntimeEvent({
    ...event,
    payload: {
      ...payload,
      token_used: requestTokenEstimate,
      token_limit: tokenLimit,
      remaining_tokens: Math.max(0, tokenLimit - requestTokenEstimate),
      usage_ratio: tokenLimit > 0 ? requestTokenEstimate / tokenLimit : 0,
      phase: 'compacted',
      accuracy: 'estimated',
      updated_at: readFiniteEventNumber(payload, 'compacted_at', 'compactedAt')
        ?? event.occurred_at,
    },
  });
  const state = messagesState.orchestratorRuntimeState;
  const budget = state?.runtimeSnapshot?.budgetState;
  if (!state || !budget) return;
  setOrchestratorRuntimeState({
    ...state,
    runtimeSnapshot: {
      ...(state.runtimeSnapshot ?? {}),
      budgetState: {
        ...budget,
        lastCompactionAt: readFiniteEventNumber(payload, 'compacted_at', 'compactedAt'),
        lastCompactionReason: trimBridgeString(payload.reason) || undefined,
        originalTokenEstimate: readFiniteEventNumber(
          payload,
          'original_token_estimate',
          'originalTokenEstimate',
        ),
        compactedTokenEstimate,
        requestTokenEstimate,
        originalMessageCount: readFiniteEventNumber(
          payload,
          'original_message_count',
          'originalMessageCount',
        ),
        compactedMessageCount: readFiniteEventNumber(
          payload,
          'compacted_message_count',
          'compactedMessageCount',
        ),
      },
    },
  });
}

function handleRustEventStreamMessage(event: RustEventEnvelope): void {
  const eventType = trimBridgeString(event.event_type);

  if (eventType === 'appearance.changed') {
    window.dispatchEvent(new CustomEvent('magi:appearanceChanged', {
      detail: event.payload ?? {},
    }));
    return;
  }

  advanceEventStreamCursorFromEvent(event);

  // 事件流可能携带当前主对话之外的会话事件。它们不能进入消息时间轴，
  // 但必须继续驱动对应的工作区目录投影，否则后台会话的状态灯会停留在旧状态。
  if (!eventMatchesCurrentWorkspace(event)) {
    if (shouldRefreshExternalWorkspaceSessionSummary(eventType, event)) {
      scheduleSessionSummaryRefresh(
        `external_${eventType.replaceAll('.', '_')}`,
        sessionSummaryRefreshTargetForEvent(event),
      );
    }
    return;
  }

  if (eventType === 'event.stream.lagged') {
    console.warn('[web-client-bridge] 事件流出现 lag，切换到 bootstrap recovery', {
      payload: event.payload ?? {},
      sequence: event.sequence,
    });
    closeEventStream();
    scheduleRecovery('event_stream_lagged', undefined, true);
    return;
  }

  if (eventType === 'event.stream.keep_alive') {
    return;
  }

  if (eventType === 'model.context_window.updated') {
    refreshSettingsBootstrapForCurrentWorkspace('model_context_window_updated');
    return;
  }

  if (eventType === 'workspace.git.context.changed') {
    window.dispatchEvent(new CustomEvent('magi:workspaceContentChanged', {
      detail: {
        reason: 'gitContextChanged',
        branch: trimBridgeString(event.payload?.branch),
        head: trimBridgeString(event.payload?.head),
      },
    }));
  }

  if (!shouldApplyCurrentSessionRustEvent(event)) {
    const acceptedPayload = event.payload;
    const hasAcceptedDirectoryIncrement =
      eventType === 'session.turn.task.accepted'
      && acceptedPayload?.created_session === true
      && Boolean(acceptedPayload?.session_summary);
    if (hasAcceptedDirectoryIncrement && acceptedPayload) {
      const acceptedSessionId = trimBridgeString(acceptedPayload.session_id)
        || trimBridgeString(acceptedPayload.sessionId)
        || trimBridgeString(event.session_id);
      const acceptedWorkspaceId = trimBridgeString(acceptedPayload.workspace_id)
        || trimBridgeString(acceptedPayload.workspaceId)
        || trimBridgeString(event.workspace_id)
        || currentWorkspaceId;
      if (acceptedSessionId) {
        emitSessionTurnAccepted({
          sessionId: acceptedSessionId,
          workspaceId: acceptedWorkspaceId,
          sessionSummary: acceptedPayload.session_summary,
          createdSession: true,
          route: acceptedPayload.route ?? 'task',
          runtimeEpoch: currentRuntimeEpoch,
          eventStreamNextSequence: typeof event.sequence === 'number' && Number.isFinite(event.sequence)
            ? Math.max(1, Math.floor(event.sequence) + 1)
            : 0,
          acceptedAt: typeof acceptedPayload.accepted_at === 'number' && Number.isFinite(acceptedPayload.accepted_at)
            ? Math.floor(acceptedPayload.accepted_at)
            : typeof acceptedPayload.acceptedAt === 'number' && Number.isFinite(acceptedPayload.acceptedAt)
              ? Math.floor(acceptedPayload.acceptedAt)
              : undefined,
        });
      }
    }
    if (shouldRefreshExternalWorkspaceSessionSummary(eventType, event) && !hasAcceptedDirectoryIncrement) {
      scheduleSessionSummaryRefresh(
        `external_${eventType.replaceAll('.', '_')}`,
        sessionSummaryRefreshTargetForEvent(event),
      );
    }
    return;
  }

  if (eventType.startsWith('browser.')) {
    window.dispatchEvent(new CustomEvent(BROWSER_AUTHORITY_CHANGED_EVENT, {
      detail: {
        eventType,
        workspaceId: rustEventWorkspaceId(event) || currentWorkspaceId,
        sessionId: rustEventSessionId(event) || currentSessionId,
        payload: event.payload ?? {},
      },
    }));
    return;
  }

  // 授权请求与授权结果都通过同一事件流进入前端。这里刷新会话级授权投影，
  // 让主会话和子代理共用一个待处理托盘，不依赖轮询或“重新打开会话”才能看到按钮。
  if (eventType === 'tool.approval.requested' || eventType === 'tool.approval.resolved') {
    refreshCurrentSessionToolApprovals(eventType);
    return;
  }

  if (eventType === 'session.turn.queued') {
    void syncSessionTurnQueue().catch((error) => {
      reportExpectedRecoveryFailure(
        i18n.t('bridge.action.syncMessages'),
        '[web-client-bridge] 排队事件后同步队列失败:',
        error,
      );
    });
    return;
  }

  if (eventType === 'session.turn.task.accepted') {
    void syncSessionTurnQueue().catch((error) => {
      reportExpectedRecoveryFailure(
        i18n.t('bridge.action.syncMessages'),
        '[web-client-bridge] 排队消息接受后同步队列失败:',
        error,
      );
    });
  }

  if (eventType === 'session.context.pressure.updated' || eventType === 'session.context.usage.updated') {
    applyContextBudgetRuntimeEvent(event);
    return;
  }

  if (eventType === 'session.context.compacted') {
    if (trimBridgeString(event.payload?.thread_scope) !== 'mainline') {
      return;
    }
    applyContextCompactionRuntimeEvent(event);
    return;
  }

  if (eventType === 'model.retry.runtime' && event.payload) {
    const messageId = trimBridgeString(event.payload.message_id)
      || trimBridgeString(event.payload.messageId);
    const phase = trimBridgeString(event.payload.phase);
    if (messageId && (phase === 'scheduled' || phase === 'attempt_started' || phase === 'settled')) {
      const attempt = typeof event.payload.attempt === 'number' && Number.isFinite(event.payload.attempt)
        ? Math.floor(event.payload.attempt)
        : 0;
      const maxAttempts = typeof event.payload.max_attempts === 'number' && Number.isFinite(event.payload.max_attempts)
        ? Math.floor(event.payload.max_attempts)
        : typeof event.payload.maxAttempts === 'number' && Number.isFinite(event.payload.maxAttempts)
          ? Math.floor(event.payload.maxAttempts)
          : 0;
      const delayMs = typeof event.payload.delay_ms === 'number' && Number.isFinite(event.payload.delay_ms)
        ? Math.max(0, Math.floor(event.payload.delay_ms))
        : typeof event.payload.delayMs === 'number' && Number.isFinite(event.payload.delayMs)
          ? Math.max(0, Math.floor(event.payload.delayMs))
          : undefined;
      emitDataMessage('llmRetryRuntime', {
        messageId,
        phase,
        attempt,
        maxAttempts,
        ...(delayMs === undefined ? {} : { delayMs }),
        ...(phase === 'scheduled' && delayMs !== undefined
          ? { nextRetryAt: Date.now() + delayMs }
          : {}),
      });
    }
    return;
  }

  if (eventType === 'session.configuration.updated') {
    refreshSettingsBootstrapForCurrentWorkspace('session_configuration_updated');
    return;
  }

  if (
    (
      eventType === 'session.plan.updated'
      || eventType === 'session.plan.paused'
      || eventType === 'session.plan.completed'
      || eventType === 'session.plan.cleared'
    )
    && event.payload
  ) {
    const sessionId = rustEventSessionId(event) || currentSessionId;
    const workspaceId = rustEventWorkspaceId(event) || currentWorkspaceId;
    const plan = event.payload.plan;
    if (sessionId && eventType === 'session.plan.cleared') {
      const eventPlanId = trimBridgeString(event.payload.plan_id)
        || trimBridgeString(event.payload.planId);
      const eventRevision = typeof event.payload.revision === 'number'
        ? event.payload.revision
        : null;
      applySessionPlanSnapshot(
        sessionId,
        workspaceId,
        null,
        eventPlanId,
        eventRevision,
      );
      void refreshCurrentGoal(sessionId, workspaceId, currentWorkspacePath);
    } else if (sessionId && plan && typeof plan === 'object' && !Array.isArray(plan)) {
      applySessionPlanSnapshot(
        sessionId,
        workspaceId,
        plan as unknown as SessionPlanDto,
      );
      // plan 事件只携带 plan，不能据此推导 Goal 状态、计时和 allowedActions。
      // 先增量展示计划，再读取权威 Goal 快照，避免恢复链已运行而卡片仍显示暂停。
      void refreshCurrentGoal(sessionId, workspaceId, currentWorkspacePath);
    }
    return;
  }

  if (eventType === 'session.turn.task.accepted' && event.payload) {
    const acceptedSessionId = trimBridgeString(event.payload.session_id)
      || trimBridgeString(event.payload.sessionId)
      || trimBridgeString(event.session_id);
    const acceptedWorkspaceId = trimBridgeString(event.payload.workspace_id)
      || trimBridgeString(event.payload.workspaceId)
      || trimBridgeString(event.workspace_id)
      || currentWorkspaceId;
    const acceptedRequestId = rustEventRequestId(event);
    const acceptedCreatedSession = event.payload.created_session === true
      || event.payload.createdSession === true;
    const acceptedMatchesCurrentSession = Boolean(
      acceptedSessionId && currentSessionId === acceptedSessionId,
    );
    const acceptedSubmissionContext = acceptedRequestId
      && sessionTurnSubmissions.get(acceptedRequestId)
      && messagesState.pendingRequests.has(acceptedRequestId)
      && getRequestBinding(acceptedRequestId)
      ? sessionTurnSubmissions.get(acceptedRequestId)
      : null;
    const acceptedMatchesDraftSubmission = Boolean(
      acceptedSubmissionContext
      && !acceptedSubmissionContext.targetSessionId
      && activeDraftSubmissionMatchesAcceptedEvent(event, acceptedSessionId),
    );
    if (acceptedSessionId && (acceptedMatchesCurrentSession || acceptedMatchesDraftSubmission)) {
      if (acceptedMatchesDraftSubmission && acceptedSubmissionContext) {
        acceptedSubmissionContext.acceptedSessionId = acceptedSessionId;
      }
      persistWorkspaceBinding(
        currentSessionScope,
        acceptedWorkspaceId,
        currentWorkspacePath,
        acceptedSessionId,
      );
      emitSessionTurnAccepted({
        sessionId: acceptedSessionId,
        workspaceId: acceptedWorkspaceId,
        requestId: acceptedRequestId || undefined,
        runtimeEpoch: currentRuntimeEpoch,
        eventStreamNextSequence: typeof event.sequence === 'number' && Number.isFinite(event.sequence)
          ? Math.max(1, Math.floor(event.sequence) + 1)
          : 0,
        acceptedAt: typeof event.payload.accepted_at === 'number' && Number.isFinite(event.payload.accepted_at)
          ? Math.floor(event.payload.accepted_at)
          : typeof event.payload.acceptedAt === 'number' && Number.isFinite(event.payload.acceptedAt)
            ? Math.floor(event.payload.acceptedAt)
            : undefined,
        sessionSummary: event.payload.session_summary ?? event.payload.sessionSummary ?? null,
        createdSession: acceptedCreatedSession,
        route: event.payload.route ?? 'task',
        submissionContext: acceptedMatchesDraftSubmission ? acceptedSubmissionContext : null,
      });
    }
    const canonicalEvent = parseCanonicalTurnEventFromRustEvent(event);
    if (canonicalEvent) {
      emitSessionTurnCanonicalEvent(canonicalEvent);
      const editingTurn = messagesState.editingTurn;
      if (
        editingTurn
        && editingTurn.sessionId === canonicalEvent.sessionId
        && editingTurn.turnId !== canonicalEvent.turnId
      ) {
        completeTurnEditing(editingTurn.turnId);
      }
    }
  }

  if (eventType === 'session.turn.item') {
    if (handleSessionTurnItemEvent(event)) {
      const sessionId = rustEventSessionId(event) || currentSessionId;
      const workspaceId = rustEventWorkspaceId(event) || currentWorkspaceId;
      if (sessionId) {
        scheduleGoalRefresh(sessionId, workspaceId, currentWorkspacePath);
      }
      return;
    }
  }

  if (eventType === 'session.turn.superseded') {
    const canonicalEvent = parseCanonicalTurnEventFromRustEvent(event);
    if (canonicalEvent) {
      emitSessionTurnCanonicalEvent(canonicalEvent);
      completeTurnEditing(canonicalEvent.turnId);
    }
    if (shouldApplyCurrentSessionRustEvent(event)) {
      scheduleSessionSummaryRefresh('current_session_turn_superseded');
    } else if (shouldRefreshExternalWorkspaceSessionSummary(eventType, event)) {
      scheduleSessionSummaryRefresh(
        'external_session_turn_superseded',
        sessionSummaryRefreshTargetForEvent(event),
      );
    }
    return;
  }

  if (eventType === 'session.action.accepted' && event.payload) {
    const acceptedSessionId = trimBridgeString(event.payload.session_id) || trimBridgeString(event.session_id);
    const acceptedActionTaskId = trimBridgeString(event.payload.action_task_id)
      || trimBridgeString(event.payload.actionTaskId);
    const acceptedRootTaskId = trimBridgeString(event.payload.root_task_id)
      || trimBridgeString(event.payload.rootTaskId);

    if (acceptedSessionId) {
      if (!currentSessionId || currentSessionId === acceptedSessionId) {
        if (acceptedActionTaskId) {
          setCurrentInterruptTaskId(acceptedActionTaskId);
        }
        if (acceptedRootTaskId) {
          initAgentRunTracking(acceptedSessionId, acceptedRootTaskId, currentWorkspaceId, currentWorkspacePath);
        }
      }
    }
  }

  if (TURN_TERMINAL_EVENTS.has(eventType)) {
    const canonicalTerminal = emitCanonicalTurnEventFromRustEvent(event);
    const hasCanonicalTerminal = Boolean(
      canonicalTerminal && isCanonicalTerminalEvent(canonicalTerminal),
    );
    const terminalIdentity = terminalTurnIdentity(event, canonicalTerminal);
    const terminalErrorCode = trimBridgeString(event.payload?.error_code)
      || trimBridgeString(event.payload?.errorCode);
    const terminalPublicMessage = trimBridgeString(event.payload?.public_message)
      || trimBridgeString(event.payload?.publicMessage)
      || trimBridgeString(event.payload?.error);
    const terminalFailureDetail = trimBridgeString(event.payload?.failure_detail)
      || trimBridgeString(event.payload?.failureDetail);
    if (eventType === 'session.turn.queue_failed' && (terminalFailureDetail || terminalPublicMessage)) {
      emitBridgeErrorToast(
        i18n.t('bridge.action.sendMessage'),
        new Error(terminalFailureDetail || terminalPublicMessage),
      );
    }
    const terminalReason = eventType === 'session.turn.failed'
      ? (terminalErrorCode || 'session_turn_failed_without_reason')
      : eventType === 'session.turn.interrupted'
        ? 'session_turn_interrupted'
        : eventType === 'session.turn.queue_failed'
          ? (terminalErrorCode || 'queued_session_turn_failed')
        : 'session_turn_completed';
    if (!hasCanonicalTerminal) {
      if (terminalIdentity) {
        emitForcedProcessingTerminal({
          ...terminalIdentity,
          reason: terminalReason,
          details: {
            eventType,
            ...(terminalErrorCode ? { errorCode: terminalErrorCode } : {}),
            ...(terminalPublicMessage ? { publicMessage: terminalPublicMessage } : {}),
          },
        });
      } else {
        console.warn('[web-client-bridge] turn 终态缺少精确 sessionId/requestId，等待权威快照收敛', {
          eventType,
        });
      }
    }
    if (!hasCanonicalTerminal) {
      refreshBootstrapAfterTerminalTurn(terminalReason);
    }
  }

  if (eventType === 'session.title.updated') {
    void fetchBootstrap({ forceFresh: true }).catch((error) => {
      reportExpectedRecoveryFailure(i18n.t('bridge.action.refreshSessionTitle'), '[web-client-bridge] 会话标题更新后刷新失败:', error);
      scheduleRecovery('session_title_updated_refresh', error, true);
    });
  }

  if (shouldRefreshCurrentSessionSummary(eventType)) {
    scheduleSessionSummaryRefresh(`current_${eventType.replaceAll('.', '_')}`);
  }

  // Notify listeners about task-domain SSE events so lightweight stores
  // (e.g. agent-run-store) can react without waiting for a full bootstrap refresh.
  const isAgentRunRelevantEvent = eventType.startsWith('task.')
    || eventType.startsWith('mission.')
    || eventType.startsWith('assignment.');
  if (isAgentRunRelevantEvent) {
    emitMessage({
      type: 'rustTaskEvent',
      eventType,
      payload: event.payload ?? {},
      workspaceId: rustEventWorkspaceId(event),
      sessionId: rustEventSessionId(event),
      taskId: rustEventTaskId(event),
      rootTaskIds: rustTaskEventRootTaskIds(event),
    } as ClientBridgeMessage);

    if (eventType === 'task.status.changed' && event.payload) {
      const taskStatus = event.payload.new_status ?? event.payload.newStatus ?? event.payload.status;
      if (isTerminalRuntimeTaskStatus(taskStatus)) {
        refreshBootstrapAfterTerminalTurn('terminal_task_status_refresh');
      }
      emitDataMessage('taskStatusChanged', {
        taskId: event.payload.task_id ?? event.payload.taskId ?? '',
        rootTaskId: event.payload.root_task_id ?? event.payload.rootTaskId ?? '',
        title: event.payload.title ?? '',
        newStatus: event.payload.new_status ?? event.payload.status ?? '',
        oldStatus: event.payload.old_status ?? '',
        kind: event.payload.kind ?? '',
        failureDetail: event.payload.failure_detail ?? event.payload.failureDetail ?? '',
        failureStage: event.payload.failure_stage ?? event.payload.failureStage ?? '',
      });
    }
  }

  if (eventType.startsWith('session.turn.') || eventType === 'session.continue.executed') {
    const sessionId = rustEventSessionId(event) || currentSessionId;
    const workspaceId = rustEventWorkspaceId(event) || currentWorkspaceId;
    if (sessionId) {
      void refreshCurrentGoal(sessionId, workspaceId, currentWorkspacePath);
    }
  }

  if (eventType.startsWith('message.') && event.payload) {
    emitDataMessage('messageCreated', {
      sessionId: event.payload.session_id ?? event.payload.sessionId ?? '',
      role: event.payload.role ?? '',
      content: event.payload.content ?? '',
    });
  }
}

function emitBridgeErrorToast(
  action: string,
  error: unknown,
  options: {
    reportIncident?: boolean;
    actionRequired?: boolean;
    duration?: number;
  } = {},
): void {
  const normalizedAction = action.trim() || i18n.t('bridge.toast.defaultAction');
  const friendlyMessage = i18n.t('bridge.toast.actionFailed', { action: normalizedAction });
  const now = Date.now();
  const incident = options.reportIncident !== false;
  const content = incident ? (normalizeErrorMessage(error) || friendlyMessage) : friendlyMessage;
  const actionRequired = options.actionRequired ?? true;
  const message = createNotifyMessage(
    content,
    'error',
    `web-bridge:${normalizedAction}`,
    options.duration,
    {
      title: i18n.t('bridge.toast.requestFailedTitle'),
      displayMode: 'toast',
      category: incident ? 'incident' : 'feedback',
      source: 'bridge-runtime',
      actionRequired,
    },
    {
      id: `web-bridge-error-${now}`,
      timestamp: now,
      updatedAt: now,
    },
  );
  emitMessage({ type: 'unifiedMessage', message });
}

function emitBridgeSuccessToast(
  action: string,
  detail?: string,
): void {
  const normalizedAction = action.trim() || i18n.t('bridge.toast.defaultAction');
  const content = detail?.trim() || i18n.t('bridge.toast.actionSucceeded', {
    action: normalizedAction,
  });
  const now = Date.now();
  const message = createNotifyMessage(
    content,
    'success',
    `web-bridge-success:${normalizedAction}`,
    undefined,
    {
      title: i18n.t('bridge.toast.operationCompletedTitle'),
      displayMode: 'silent',
      category: 'feedback',
      source: 'bridge-runtime',
      actionRequired: false,
    },
    {
      id: `web-bridge-success-${now}`,
      timestamp: now,
      updatedAt: now,
    },
  );
  emitMessage({ type: 'unifiedMessage', message });
}

function emitBridgeInfoToast(
  action: string,
  detail: string,
): void {
  const normalizedAction = action.trim() || i18n.t('bridge.toast.defaultInfoAction');
  const content = detail.trim() || normalizedAction;
  const now = Date.now();
  const message = createNotifyMessage(
    content,
    'info',
    `web-bridge-info:${normalizedAction}`,
    undefined,
    {
      title: i18n.t('bridge.toast.infoTitle'),
      displayMode: 'silent',
      category: 'feedback',
      source: 'bridge-runtime',
      actionRequired: false,
    },
    {
      id: `web-bridge-info-${now}`,
      timestamp: now,
      updatedAt: now,
    },
  );
  emitMessage({ type: 'unifiedMessage', message });
}

function logBridgeOperationFailure(
  action: string,
  logLabel: string,
  error: unknown,
  options: { suppressToast?: boolean; suppressConsole?: boolean } = {},
): void {
  if (!options.suppressConsole) {
    console.error(logLabel, error);
  }
  if (!options.suppressToast) {
    emitBridgeErrorToast(action, error);
  }
}

function logKnowledgeOperationFailure(action: string, logLabel: string, error: unknown): void {
  console.error(logLabel, error);
  emitBridgeErrorToast(action, error);
}

function isBridgeRecoveringOrUnavailable(): boolean {
  return bridgeRecovering
    || recoveryInFlight !== null
    || recoveryTimer !== null
    || activeEventStreamState !== 'open';
}

function reportExpectedRecoveryFailure(action: string, logLabel: string, error: unknown): void {
  if (isBridgeRecoveringOrUnavailable() && isExpectedRecoveryBridgeFailure(error)) {
    return;
  }
  logBridgeOperationFailure(action, logLabel, error);
}

const NOTIFICATION_OPERATION_FAILURE_TOAST_DEBOUNCE_MS = 8_000;
let lastNotificationOperationFailureToastAt = 0;

function reportNotificationOperationFailure(
  action: string,
  logLabel: string,
  error: unknown,
  options: { suppressToast?: boolean } = {},
): void {
  if (options.suppressToast && isExpectedRecoveryBridgeFailure(error)) {
    return;
  }
  if (isBridgeRecoveringOrUnavailable() && isExpectedRecoveryBridgeFailure(error)) {
    return;
  }
  const now = Date.now();
  if (now - lastNotificationOperationFailureToastAt < NOTIFICATION_OPERATION_FAILURE_TOAST_DEBOUNCE_MS) {
    return;
  }
  lastNotificationOperationFailureToastAt = now;
  console.warn(logLabel, error);
  if (options.suppressToast) {
    return;
  }
  emitBridgeErrorToast(action, error, {
    reportIncident: false,
    actionRequired: false,
  });
}

function handleEventStreamParseFailure(data: string, error: unknown): void {
  console.error('[web-client-bridge] 事件流消息解析失败:', {
    error,
    preview: data.slice(0, 240),
  });
  const now = Date.now();
  if (now - lastEventStreamParseErrorAt >= EVENT_STREAM_PARSE_ERROR_DEBOUNCE_MS) {
    lastEventStreamParseErrorAt = now;
    emitBridgeErrorToast(i18n.t('bridge.action.syncMessages'), error);
  }
  closeEventStream();
  scheduleRecovery('event_stream_parse_error', error, true);
}

function getCurrentUrl(): URL | null {
  if (typeof window === 'undefined') {
    return null;
  }
  return new URL(window.location.href);
}

function resolveWorkspaceQuery(): AgentBindingContext {
  return resolveAgentBindingContext();
}

function hydrateCanonicalWorkspaceBinding(): void {
  if (initialWindowBindingHydrated) {
    return;
  }
  initialWindowBindingHydrated = true;
  const binding = seedAgentBindingContextFromWindow();
  currentSessionScope = binding.scope;
  currentWorkspaceId = agentBindingWorkspaceId(binding);
  currentWorkspacePath = agentBindingWorkspacePath(binding);
  currentSessionId = binding.sessionId ?? '';
}

function bootstrapBindingKey(
  binding: AgentBindingContext,
): string {
  return JSON.stringify({
    scope: binding.scope,
    workspaceId: agentBindingWorkspaceId(binding),
    workspacePath: agentBindingWorkspacePath(binding),
    sessionId: binding.sessionId?.trim() ?? '',
  });
}

function buildBootstrapQuery(
  binding: AgentBindingContext,
): string {
  const query = new URLSearchParams();
  query.set('scope', binding.scope);
  if (binding.scope === 'workspace' && binding.workspaceId) {
    query.set('workspaceId', binding.workspaceId);
  }
  if (binding.scope === 'workspace' && binding.workspacePath) {
    query.set('workspacePath', binding.workspacePath);
  }
  if (binding.sessionId) {
    query.set('sessionId', binding.sessionId);
  }
  return query.toString();
}

function eventStreamBindingKey(): string {
  return eventStreamScopeKey();
}

function shouldUseAppServerEventStream(): boolean {
  return typeof window !== 'undefined'
    && window.magiDesktop?.runtime === 'electron'
    && Boolean(currentSessionId || currentWorkspaceId);
}

function appServerEventStreamKey(): string {
  return `${eventStreamBindingKey()}\u0000${currentSessionId}`;
}

function currentEventStreamKey(): string {
  return shouldUseAppServerEventStream()
    ? appServerEventStreamKey()
    : eventStreamBindingKey();
}

function appServerEventSubscription(): {
  sessionId?: string | null;
  workspaceId?: string | null;
  afterSequence: number;
} {
  return {
    sessionId: currentSessionId || null,
    workspaceId: currentSessionScope === 'workspace' ? currentWorkspaceId || null : null,
    afterSequence: eventStreamCursorScopeKey === eventStreamScopeKey()
      ? eventStreamAfterSequence
      : 0,
  };
}

function eventStreamQuery(): string {
  const query = new URLSearchParams();
  query.set('scope', currentSessionScope);
  if (currentSessionScope === 'workspace' && currentWorkspaceId) {
    query.set('workspaceId', currentWorkspaceId);
  }
  if (currentSessionScope === 'workspace' && currentWorkspacePath) {
    query.set('workspacePath', currentWorkspacePath);
  }
  if (eventStreamCursorScopeKey === eventStreamScopeKey() && eventStreamAfterSequence > 0) {
    query.set('afterSequence', String(eventStreamAfterSequence));
  }
  return query.toString();
}

function eventStreamScopeKey(): string {
  const query = new URLSearchParams();
  query.set('scope', currentSessionScope);
  if (currentSessionScope === 'workspace' && currentWorkspaceId) {
    query.set('workspaceId', currentWorkspaceId);
  }
  if (currentSessionScope === 'workspace' && currentWorkspacePath) {
    query.set('workspacePath', currentWorkspacePath);
  }
  return query.toString();
}

function resetEventStreamCursor(): void {
  eventStreamCursorScopeKey = '';
  eventStreamAfterSequence = 0;
}

function updateEventStreamCursorFromSnapshot(eventStreamNextSequence: unknown): void {
  const nextSequence = typeof eventStreamNextSequence === 'number' && Number.isFinite(eventStreamNextSequence)
    ? Math.floor(eventStreamNextSequence)
    : 0;
  if (nextSequence <= 1) {
    resetEventStreamCursor();
    return;
  }
  const scopeKey = eventStreamScopeKey();
  if (eventStreamCursorScopeKey === scopeKey) {
    eventStreamAfterSequence = Math.max(eventStreamAfterSequence, nextSequence - 1);
    return;
  }
  eventStreamCursorScopeKey = scopeKey;
  eventStreamAfterSequence = nextSequence - 1;
}

function advanceEventStreamCursorFromEvent(event: RustEventEnvelope): void {
  const sequence = typeof event.sequence === 'number' && Number.isFinite(event.sequence)
    ? Math.floor(event.sequence)
    : 0;
  if (sequence <= 0) {
    return;
  }
  const scopeKey = eventStreamScopeKey();
  if (!scopeKey) {
    return;
  }
  if (eventStreamCursorScopeKey !== scopeKey) {
    eventStreamCursorScopeKey = scopeKey;
    eventStreamAfterSequence = sequence;
    return;
  }
  if (sequence > eventStreamAfterSequence) {
    eventStreamAfterSequence = sequence;
  }
}

async function readAgentErrorPayload(
  response: Response,
): Promise<{ errorCode?: string; message?: string; detail?: string }> {
  const contentType = response.headers.get('content-type') || '';
  if (!contentType.includes('application/json')) {
    return {};
  }
  try {
    const payload = await response.json() as {
      error_code?: string;
      code?: string;
      message?: string;
      error?: string;
      detail?: string;
    };
    const errorCode = typeof payload.error_code === 'string' && payload.error_code.trim()
      ? payload.error_code.trim()
      : (typeof payload.code === 'string' && payload.code.trim() ? payload.code.trim() : undefined);
    const message = typeof payload.message === 'string' && payload.message.trim()
      ? payload.message.trim()
      : (typeof payload.error === 'string' && payload.error.trim() ? payload.error.trim() : undefined);
    const detail = typeof payload.detail === 'string' && payload.detail.trim()
      ? payload.detail.trim()
      : undefined;
    return { errorCode, message, detail };
  } catch {
    return {};
  }
}

function isCurrentBootstrapRequest(bindingKey: string, requestSeq: number): boolean {
  return requestSeq === bootstrapRequestSeq
    && bindingKey === bootstrapBindingKey(resolveWorkspaceQuery());
}

function bootstrapRequestCanClearMissingSession(
  requestBinding: AgentBindingContext,
  requestSeq: number,
): boolean {
  if (requestSeq !== bootstrapRequestSeq) {
    return false;
  }
  const currentBinding = resolveWorkspaceQuery();
  if (currentBinding.scope !== requestBinding.scope) {
    return false;
  }
  const sameWorkspace = requestBinding.scope === 'personal'
    || (requestBinding.workspaceId
      ? agentBindingWorkspaceId(currentBinding) === requestBinding.workspaceId
      : agentBindingWorkspacePath(currentBinding) === requestBinding.workspacePath);
  if (!sameWorkspace) return false;
  return !currentBinding.sessionId || currentBinding.sessionId === requestBinding.sessionId;
}

function settingsBootstrapBindingKey(
  workspaceId = currentWorkspaceId,
  workspacePath = currentWorkspacePath,
  sessionId = currentSessionId,
): string {
  return JSON.stringify({
    workspaceId: workspaceId.trim(),
    workspacePath: workspacePath.trim(),
    sessionId: sessionId.trim(),
  });
}

function clearSettingsBootstrapCache(): void {
  cachedSettingsBootstrap = null;
  cachedSettingsBootstrapScope = 'none';
  cachedSettingsBootstrapBindingKey = '';
}

function clearSettingsBootstrapCacheIfBindingChanged(
  previousWorkspaceId: string,
  previousWorkspacePath: string,
  previousSessionId: string,
  nextWorkspaceId: string,
  nextWorkspacePath: string,
  nextSessionId: string,
): boolean {
  const changed =
    settingsBootstrapBindingKey(previousWorkspaceId, previousWorkspacePath, previousSessionId)
    !== settingsBootstrapBindingKey(nextWorkspaceId, nextWorkspacePath, nextSessionId);
  if (changed) {
    clearSettingsBootstrapCache();
  }
  return changed;
}

function isCurrentSettingsBootstrapRequest(bindingKey: string, requestSeq: number): boolean {
  return requestSeq === settingsBootstrapRequestSeq
    && bindingKey === settingsBootstrapBindingKey();
}

function persistWorkspaceBinding(
  scope: 'personal' | 'workspace',
  workspaceId: string,
  workspacePath: string,
  sessionId: string,
): boolean {
  const previousWorkspaceId = currentWorkspaceId;
  const previousWorkspacePath = currentWorkspacePath;
  const previousSessionId = currentSessionId;
  const normalizedWorkspaceId = scope === 'workspace' ? workspaceId.trim() : '';
  const normalizedWorkspacePath = scope === 'workspace' ? workspacePath.trim() : '';
  const incomingSessionId = sessionId.trim();
  const incomingScope = scope;

  const settingsBindingChanged = clearSettingsBootstrapCacheIfBindingChanged(
    previousWorkspaceId,
    previousWorkspacePath,
    previousSessionId,
    normalizedWorkspaceId,
    normalizedWorkspacePath,
    incomingSessionId,
  );
  currentSessionScope = incomingScope;
  currentWorkspaceId = normalizedWorkspaceId;
  currentWorkspacePath = normalizedWorkspacePath;
  currentSessionId = incomingSessionId;
  if (
    previousWorkspaceId !== normalizedWorkspaceId
    || previousWorkspacePath !== normalizedWorkspacePath
    || previousSessionId !== incomingSessionId
  ) {
    currentBindingGeneration += 1;
    clearContinueRequestInFlight();
  }
  setAgentBindingContext(incomingScope === 'workspace' ? {
    scope: 'workspace',
    workspaceId: normalizedWorkspaceId,
    workspacePath: normalizedWorkspacePath,
    sessionId: incomingSessionId,
  } : {
    scope: 'personal',
    sessionId: incomingSessionId,
  }, {
    authoritative: true,
  });

  const currentUrl = getCurrentUrl();
  if (!currentUrl) {
    return settingsBindingChanged;
  }
  const nextUrl = new URL(currentUrl.toString());
  nextUrl.searchParams.set('scope', incomingScope);
  if (normalizedWorkspaceId) {
    nextUrl.searchParams.set('workspaceId', normalizedWorkspaceId);
  } else {
    nextUrl.searchParams.delete('workspaceId');
  }
  if (normalizedWorkspacePath) {
    nextUrl.searchParams.set('workspacePath', normalizedWorkspacePath);
  } else {
    nextUrl.searchParams.delete('workspacePath');
  }
  if (incomingSessionId) {
    nextUrl.searchParams.set('sessionId', incomingSessionId);
  } else {
    nextUrl.searchParams.delete('sessionId');
  }
  if (nextUrl.toString() !== currentUrl.toString()) {
    window.history.replaceState(window.history.state, '', nextUrl);
  }
  return settingsBindingChanged;
}

function clearWorkspaceSessionBinding(
  scope: 'personal' | 'workspace',
  workspaceId: string,
  workspacePath: string,
): boolean {
  const previousWorkspaceId = currentWorkspaceId;
  const previousWorkspacePath = currentWorkspacePath;
  const previousSessionId = currentSessionId;
  const normalizedWorkspaceId = scope === 'workspace' ? workspaceId.trim() : '';
  const normalizedWorkspacePath = scope === 'workspace' ? workspacePath.trim() : '';
  const incomingScope = scope;
  const settingsBindingChanged = clearSettingsBootstrapCacheIfBindingChanged(
    previousWorkspaceId,
    previousWorkspacePath,
    previousSessionId,
    normalizedWorkspaceId,
    normalizedWorkspacePath,
    '',
  );
  currentSessionScope = incomingScope;
  currentWorkspaceId = normalizedWorkspaceId;
  currentWorkspacePath = normalizedWorkspacePath;
  currentSessionId = '';
  if (
    previousWorkspaceId !== normalizedWorkspaceId
    || previousWorkspacePath !== normalizedWorkspacePath
    || previousSessionId !== ''
  ) {
    currentBindingGeneration += 1;
  }
  clearContinueRequestInFlight();
  setAgentBindingContext(incomingScope === 'workspace' ? {
    scope: 'workspace',
    workspaceId: normalizedWorkspaceId,
    workspacePath: normalizedWorkspacePath,
  } : { scope: 'personal' }, {
    authoritative: true,
  });
  clearCurrentInterruptTaskId();
  clearAgentRunProjection();

  const currentUrl = getCurrentUrl();
  if (!currentUrl) {
    return settingsBindingChanged;
  }
  const nextUrl = new URL(currentUrl.toString());
  nextUrl.searchParams.set('scope', incomingScope);
  if (normalizedWorkspaceId) {
    nextUrl.searchParams.set('workspaceId', normalizedWorkspaceId);
  } else {
    nextUrl.searchParams.delete('workspaceId');
  }
  if (normalizedWorkspacePath) {
    nextUrl.searchParams.set('workspacePath', normalizedWorkspacePath);
  } else {
    nextUrl.searchParams.delete('workspacePath');
  }
  nextUrl.searchParams.delete('sessionId');
  if (nextUrl.toString() !== currentUrl.toString()) {
    window.history.replaceState(window.history.state, '', nextUrl);
  }
  return settingsBindingChanged;
}

function clearPersistedWorkspaceBinding(): void {
  clearSettingsBootstrapCache();
  currentWorkspaceId = '';
  currentWorkspacePath = '';
  currentSessionId = '';
  currentSessionScope = 'personal';
  currentBindingGeneration += 1;
  clearContinueRequestInFlight();
  clearAgentBindingContext({ authoritative: true });
  clearCurrentInterruptTaskId();
  clearAgentRunProjection();
  const currentUrl = getCurrentUrl();
  if (!currentUrl) {
    return;
  }
  const nextUrl = new URL(currentUrl.toString());
  nextUrl.searchParams.set('scope', 'personal');
  nextUrl.searchParams.delete('workspaceId');
  nextUrl.searchParams.delete('workspacePath');
  nextUrl.searchParams.delete('sessionId');
  if (nextUrl.toString() !== currentUrl.toString()) {
    window.history.replaceState(window.history.state, '', nextUrl);
  }
}

function dispatchEmptyWorkspaceState(): void {
  const now = Date.now();
  closeEventStream();
  clearPersistedWorkspaceBinding();
  emitDataMessage('emptyWorkspaceStateLoaded', {
    state: buildEmptyWorkspaceAppState(now),
    workspaces: [],
  });
}


function closeEventStream(): void {
  setAgentRunBridgeConnected(false);
  clearEventStreamOpenTimeout();
  stopEventStreamIdleCheck();
  activeEventStreamOpenReject?.(new Error('事件流连接已关闭'));
  activeEventStreamToken += 1;
  if (activeEventStreamConnection) {
    activeEventStreamConnection.close();
    activeEventStreamConnection = null;
  }
  activeEventStreamKey = '';
  activeEventStreamState = 'idle';
  activeEventStreamOpenResolve = null;
  activeEventStreamOpenReject = null;
  activeEventStreamOpenPromise = null;
}

function normalizeBootstrapResponse(
  rawPayload: unknown,
  options: { workspaceId?: string; workspacePath?: string; sessionId?: string } = {},
): BootstrapPayload {
  const workspaceScope = resolveWorkspaceScopeFromSource(options);
  return normalizeRustBootstrapPayload(rawPayload, {
    workspaceId: workspaceScope.workspaceId,
    workspacePath: workspaceScope.workspacePath,
    sessionId: options.sessionId,
  });
}

async function restoreBridgeState(
  reason: string,
  force = false,
  settleProcessingOnFailure = true,
): Promise<void> {
  const recoveryBinding = resolveWorkspaceQuery();
  const recoveryBindingKey = bootstrapBindingKey(recoveryBinding);
  const processingSnapshot = settleProcessingOnFailure
    ? captureProcessingRequestSnapshot(recoveryBinding.sessionId)
    : null;
  if (recoveryInFlight && !force && recoveryInFlightBindingKey === recoveryBindingKey) {
    return recoveryInFlight;
  }
  recoveryInFlightBindingKey = recoveryBindingKey;
  const request = (async () => {
    try {
      const recovered = bridgeRecovering || recoveryAttempt > 0;
      const reachableBaseUrl = await probeReachableAgentBaseUrl();
      if (!reachableBaseUrl) {
        throw new Error('无法连接 Local Agent，正在等待恢复。');
      }
      if (force) {
        clearSettingsBootstrapCache();
      }
      await fetchBootstrap({
        forceEventStreamReconnect: true,
        refreshSettingsBootstrapOnBindingChange: false,
        settleProcessingOnFailure,
        ...(processingSnapshot ? {
          processingSessionId: processingSnapshot.sessionId,
          processingRequestIds: processingSnapshot.requestIds,
        } : {}),
      });
      clearRecoveryTimer();
      recoveryAttempt = 0;
      emitConnectedState(reason, recovered);
      void dispatchSettingsBootstrap(force, 'core').catch((error) => {
        reportExpectedRecoveryFailure(
          i18n.t('settings.toast.action.loadSettingsData'),
          '[web-client-bridge] 核心会话恢复后加载设置失败:',
          error,
        );
      });
    } catch (error) {
      if (processingSnapshot) {
        settleProcessingRequestSnapshot(processingSnapshot);
      }
      throw error;
    }
  })().finally(() => {
    if (recoveryInFlight === request) {
      recoveryInFlight = null;
      recoveryInFlightBindingKey = '';
    }
  });
  recoveryInFlight = request;
  return request;
}

function scheduleRecovery(reason: string, error?: unknown, immediate = false): void {
  emitRecoveringState(reason, error);
  const recoveryBindingKey = bootstrapBindingKey(resolveWorkspaceQuery());
  if (recoveryInFlight && recoveryInFlightBindingKey === recoveryBindingKey) {
    return;
  }
  if (recoveryTimer !== null) {
    if (!immediate) {
      return;
    }
    clearRecoveryTimer();
  }
  const delay = immediate
    ? 0
    : Math.min(RECOVERY_MAX_DELAY_MS, RECOVERY_BASE_DELAY_MS * (2 ** Math.min(recoveryAttempt, 3)));
  recoveryTimer = window.setTimeout(() => {
    recoveryTimer = null;
    void restoreBridgeState(reason, true, false).catch((recoveryError) => {
      recoveryAttempt += 1;
      scheduleRecovery('retry', recoveryError);
    });
  }, delay);
}

function createEventStreamOpenPromise(token: number): Promise<void> {
  const openPromise = new Promise<void>((resolve, reject) => {
    activeEventStreamOpenResolve = () => {
      if (token !== activeEventStreamToken) {
        return;
      }
      clearEventStreamOpenTimeout();
      activeEventStreamOpenResolve = null;
      activeEventStreamOpenReject = null;
      resolve();
    };
    activeEventStreamOpenReject = (error: Error) => {
      if (token !== activeEventStreamToken) {
        return;
      }
      clearEventStreamOpenTimeout();
      activeEventStreamOpenResolve = null;
      activeEventStreamOpenReject = null;
      reject(error);
    };
    clearEventStreamOpenTimeout();
    activeEventStreamOpenTimeout = window.setTimeout(() => {
      if (token !== activeEventStreamToken || activeEventStreamState === 'open') {
        return;
      }
      closeEventStream();
      reject(new Error('事件流连接超时'));
      scheduleRecovery('event_stream_open_timeout');
    }, EVENT_STREAM_OPEN_TIMEOUT_MS);
  });
  activeEventStreamOpenPromise = openPromise;
  activeEventStreamOpenPromise.catch(() => undefined);
  return openPromise;
}

function resolveEventStreamOpen(): void {
  activeEventStreamOpenResolve?.();
  activeEventStreamOpenPromise = Promise.resolve();
}

function rejectEventStreamOpen(error?: Error): void {
  activeEventStreamOpenReject?.(error ?? new Error('事件流连接失败'));
  activeEventStreamOpenPromise = null;
}

async function ensureEventStream(
  options: { forceReconnect?: boolean; waitUntilOpen?: boolean } = {},
): Promise<void> {
  if (typeof window === 'undefined') {
    return;
  }
  const useAppServer = shouldUseAppServerEventStream();
  const nextKey = currentEventStreamKey();
  if (!nextKey) {
    closeEventStream();
    return;
  }
  if (!options.forceReconnect && activeEventStreamConnection && activeEventStreamKey === nextKey) {
    if (options.waitUntilOpen && activeEventStreamState !== 'open' && activeEventStreamOpenPromise) {
      await activeEventStreamOpenPromise;
    }
    return;
  }
  closeEventStream();
  activeEventStreamKey = nextKey;
  activeEventStreamState = 'connecting';
  const streamToken = ++activeEventStreamToken;
  const openPromise = createEventStreamOpenPromise(streamToken);
  if (useAppServer) {
    const client = new AppServerClient({
      endpoint: agentUrl('/api/app-server'),
      clientInfo: { name: 'magi-desktop-renderer', title: 'Magi Desktop', version: currentRuntimeEpoch || null },
      capabilities: {
        desktopBrowserSurface: true,
        browserTools: true,
        approvals: true,
        streaming: true,
      },
      subscription: appServerEventSubscription(),
      onStateChange(state: AppServerConnectionState) {
        if (streamToken !== activeEventStreamToken) return;
        if (state === 'ready') {
          activeEventStreamState = 'open';
          setAgentRunBridgeConnected(true);
          startEventStreamIdleCheck();
          resolveEventStreamOpen();
        } else if (state === 'connecting' || state === 'reconnecting') {
          activeEventStreamState = 'connecting';
          setAgentRunBridgeConnected(false);
        }
      },
      onActivity() {
        if (streamToken !== activeEventStreamToken) return;
        markEventStreamActive();
      },
      onSnapshot(snapshot) {
        if (streamToken !== activeEventStreamToken) return;
        markEventStreamActive();
        const previousSequence = eventStreamAfterSequence;
        for (const event of snapshot.recent_events) {
          if (typeof event.sequence === 'number' && event.sequence <= previousSequence) continue;
          handleRustEventStreamMessage(event);
        }
        updateEventStreamCursorFromSnapshot(snapshot.next_sequence);
      },
      onEvent(event) {
        if (streamToken !== activeEventStreamToken) return;
        markEventStreamActive();
        handleRustEventStreamMessage(event);
      },
    });
    activeEventStreamConnection = client;
    const connectPromise = client.connect().catch((error: unknown) => {
      if (streamToken !== activeEventStreamToken) return;
      const normalized = error instanceof Error ? error : new Error(String(error));
      rejectEventStreamOpen(normalized);
      closeEventStream();
      scheduleRecovery('app_server_connect_error', normalized);
      throw normalized;
    });
    connectPromise.catch(() => undefined);
  } else {
    activeEventStreamConnection = getTransport().connectEventStream(
      agentUrl('/events', eventStreamQuery()),
      {
        onOpen() {
          if (streamToken !== activeEventStreamToken) {
            return;
          }
          activeEventStreamState = 'open';
          setAgentRunBridgeConnected(true);
          startEventStreamIdleCheck();
          resolveEventStreamOpen();
        },
        onMessage(data: string) {
          if (streamToken !== activeEventStreamToken) {
            return;
          }
          markEventStreamActive();
          const event = parseRustEventEnvelope(data);
          if (!event) {
            handleEventStreamParseFailure(data, new Error('Rust 事件流载荷不符合 EventEnvelope 协议'));
            return;
          }
          handleRustEventStreamMessage(event);
        },
        onError() {
          if (streamToken !== activeEventStreamToken) {
            return;
          }
          const openFailed = activeEventStreamState !== 'open';
          rejectEventStreamOpen(openFailed ? new Error('事件流连接失败') : undefined);
          closeEventStream();
          scheduleRecovery('event_stream_error');
        },
      },
    );
  }
  if (options.waitUntilOpen) {
    await openPromise;
  }
}

async function dispatchBootstrap(
  payload: BootstrapPayload,
  options: {
    forceEventStreamReconnect?: boolean;
    rawPayload?: unknown;
    refreshSettingsBootstrapOnBindingChange?: boolean;
    refreshSessionTurnQueueAfterBootstrap?: boolean;
    navigation?: {
      requestId: string;
      target: 'draft' | 'session';
      orchestratorSessionConfig: Record<string, unknown>;
    };
  } = {},
): Promise<void> {
  const previousSessionId = currentSessionId;
  const pageMeta = readRustCanonicalHistoryPageMeta(options.rawPayload ?? payload);
  // 检测 runtimeEpoch 代际变化：后端重启后执行无刷新状态重建，不允许整页刷新打断用户会话。
  const incomingEpoch = payload.agent?.runtimeEpoch || '';
  if (incomingEpoch && currentRuntimeEpoch && incomingEpoch !== currentRuntimeEpoch) {
    console.warn('[web-client-bridge] runtimeEpoch 变化，后端已重启，执行无刷新状态重建', {
      previous: currentRuntimeEpoch,
      current: incomingEpoch,
    });
    resetEventStreamCursor();
  }
  if (incomingEpoch) {
    currentRuntimeEpoch = incomingEpoch;
  }
  const settingsBindingChanged = persistWorkspaceBinding(
    payload.scope,
    payload.workspace.workspaceId,
    payload.workspace.rootPath,
    payload.sessionId,
  );
  updateEventStreamCursorFromSnapshot(payload.eventStreamNextSequence);
  activateAgentRunSession(payload.sessionId, payload.workspace.workspaceId, payload.workspace.rootPath);
  const agentRunTrackingHints = extractBootstrapAgentRunTrackingHints(payload, options.rawPayload);
  if (previousSessionId && payload.sessionId && previousSessionId !== payload.sessionId) {
    clearCurrentInterruptTaskId();
  }
  reconcileCurrentInterruptTaskId(agentRunTrackingHints.activeTaskIds);
  if (!agentRunTrackingHints.rootTaskId && agentRunTrackingHints.activeTaskIds.length === 0) {
    clearAgentRunProjection(payload.sessionId, undefined, payload.workspace.workspaceId);
  }
  emitDataMessage('sessionBootstrapLoaded', {
    ...payload,
    ...(options.navigation ? {
      navigationRequestId: options.navigation.requestId,
      navigationTarget: options.navigation.target,
      navigationOrchestratorSessionConfig: options.navigation.orchestratorSessionConfig,
    } : {}),
    canonicalHasMoreBefore: pageMeta.canonicalHasMoreBefore,
    canonicalBeforeCursor: pageMeta.canonicalBeforeCursor,
  } as Record<string, unknown>);
  if (payload.sessionId && payload.workspace.workspaceId) {
    void refreshPendingChangesProjection({
      scope: 'workspace',
      sessionId: payload.sessionId,
      workspaceId: payload.workspace.workspaceId,
      workspacePath: payload.workspace.rootPath,
    }).catch((error) => {
      console.warn('[web-client-bridge] bootstrap 后刷新变更列表失败:', error);
    });
  }
  if (options.refreshSessionTurnQueueAfterBootstrap !== false) {
    void syncSessionTurnQueue(
      payload.sessionId,
      payload.workspace.workspaceId,
      payload.workspace.rootPath,
    ).catch((error) => {
      console.warn('[web-client-bridge] bootstrap 后同步排队消息失败:', error);
    });
  }
  void ensureEventStream({
    forceReconnect: options.forceEventStreamReconnect === true,
    waitUntilOpen: false,
  }).catch((error) => {
    reportExpectedRecoveryFailure(i18n.t('bridge.action.connectEventStream'), '[web-client-bridge] bootstrap 后事件流连接失败:', error);
    scheduleRecovery('bootstrap_event_stream_connect', error, true);
  });
  // 并行加载 Registry agents（fire-and-forget，不阻断 bootstrap）
  dispatchRegistryAgents();

  if (agentRunTrackingHints.rootTaskId || agentRunTrackingHints.activeTaskIds.length > 0) {
    void autoConnectAgentRunTracking(
      payload.sessionId,
      agentRunTrackingHints.activeTaskIds,
      agentRunTrackingHints.rootTaskId,
      payload.workspace.workspaceId,
      payload.workspace.rootPath,
    ).catch((error) => {
      console.warn('[web-client-bridge] Auto-connect agent-run tracking on bootstrap failed (non-critical):', error);
    });
  }
  if (settingsBindingChanged && options.refreshSettingsBootstrapOnBindingChange !== false) {
    refreshSettingsBootstrapForCurrentWorkspace('bootstrap_binding_changed');
  }
}

async function fetchBootstrap(
  options: {
    forceEventStreamReconnect?: boolean;
    forceFresh?: boolean;
    refreshSettingsBootstrapOnBindingChange?: boolean;
    refreshWorkspaceSessionsAfterBootstrap?: boolean;
    /** 仅连接恢复流程显式开启；后台 bootstrap 失败不得改写 UI 运行态。 */
    settleProcessingOnFailure?: boolean;
    refreshSessionTurnQueueAfterBootstrap?: boolean;
    /** 内部使用：异步 bootstrap 只能结算发起时已存在的 request。 */
    processingSessionId?: string;
    processingRequestIds?: readonly string[];
  } = {},
): Promise<void> {
  // 导航响应携带目标会话的完整 bootstrap，是会话归属的唯一提交者。
  // 后台恢复和终态刷新必须排在其后，并在屏障释放后重新读取最新绑定。
  await waitForSessionNavigationCommit();
  const requestBinding = resolveWorkspaceQuery();
  const requestBindingKey = bootstrapBindingKey(requestBinding);
  // 防重入：只有同一 workspace/session 绑定才能复用 bootstrap 请求。
  if (
    bootstrapInFlight
    && options.forceFresh !== true
    && bootstrapInFlightBindingKey === requestBindingKey
  ) {
    return bootstrapInFlight;
  }
  const requestSeq = ++bootstrapRequestSeq;
  const processingSnapshot: ProcessingRequestSnapshot | null = options.settleProcessingOnFailure === true
    ? options.processingRequestIds
      ? {
        sessionId: options.processingSessionId || requestBinding.sessionId || messagesState.currentSessionId || '__draft__',
        requestIds: [...options.processingRequestIds],
      }
      : captureProcessingRequestSnapshot(requestBinding.sessionId)
    : null;
  const doFetch = async (): Promise<void> => {
    try {
      let effectiveBinding = requestBinding;
      let response = await getTransport().request(agentUrl('/bootstrap', buildBootstrapQuery(effectiveBinding)));
      let errorPayload: { errorCode?: string; message?: string; detail?: string } = {};
      if (!response.ok) {
        errorPayload = await readAgentErrorPayload(response);
      }
      if (!response.ok) {
      if (response.status === 404) {
        const explicitSessionMissing = Boolean(effectiveBinding.sessionId)
          && /(?:session|会话)/i.test(errorPayload.message || '');
        if (explicitSessionMissing) {
          if (!bootstrapRequestCanClearMissingSession(effectiveBinding, requestSeq)) {
            return;
          }
          clearWorkspaceSessionBinding(
            effectiveBinding.scope,
            agentBindingWorkspaceId(effectiveBinding),
            agentBindingWorkspacePath(effectiveBinding),
          );
          if (effectiveBinding.scope === 'workspace' && effectiveBinding.workspaceId) {
            try {
              const snapshot = await getWorkspaceSessions(
                effectiveBinding.workspaceId,
                effectiveBinding.workspacePath,
              );
              const currentBinding = resolveWorkspaceQuery();
              if (!currentBinding.sessionId && agentBindingWorkspaceId(currentBinding) === effectiveBinding.workspaceId) {
                emitDataMessage('sessionsUpdated', {
                  workspaceId: effectiveBinding.workspaceId,
                  sessions: snapshot.sessions,
                  runtimeEpoch: snapshot.runtimeEpoch,
                  eventStreamNextSequence: snapshot.eventStreamNextSequence,
                });
              }
            } catch (error) {
              console.warn('[web-client-bridge] 无效会话清理后刷新会话列表失败:', error);
            }
          }
          await fetchBootstrap({
            ...options,
            forceFresh: true,
          });
          return;
        }
        const workspaces = await listAgentWorkspaces();
        if (workspaces.length === 0) {
          if (!isCurrentBootstrapRequest(requestBindingKey, requestSeq)) {
            return;
          }
          dispatchEmptyWorkspaceState();
          return;
        }
      }
        throw new AgentApiError(
          response.status,
          errorPayload.message || `bootstrap failed: ${response.status}`,
          'bootstrap',
          errorPayload.errorCode,
          errorPayload.detail,
        );
      }
      const rawPayload = await response.json();
      const payload = normalizeBootstrapResponse(rawPayload, {
        workspaceId: agentBindingWorkspaceId(effectiveBinding),
        workspacePath: agentBindingWorkspacePath(effectiveBinding),
        sessionId: effectiveBinding.sessionId,
      });
      if (!isCurrentBootstrapRequest(requestBindingKey, requestSeq)) {
        return;
      }
      await dispatchBootstrap(payload, {
        forceEventStreamReconnect: options.forceEventStreamReconnect,
        refreshSettingsBootstrapOnBindingChange: options.refreshSettingsBootstrapOnBindingChange,
        refreshSessionTurnQueueAfterBootstrap: options.refreshSessionTurnQueueAfterBootstrap,
        rawPayload,
      });
      if (options.refreshWorkspaceSessionsAfterBootstrap !== false) {
        void refreshWorkspaceSessionsAfterBootstrap(payload, requestBindingKey, requestSeq);
      }
    } catch (error) {
      if (processingSnapshot) {
        settleProcessingRequestSnapshot(processingSnapshot);
      }
      throw error;
    }
  };
  let requestPromise: Promise<void>;
  requestPromise = doFetch().finally(() => {
    if (bootstrapInFlight === requestPromise) {
      bootstrapInFlight = null;
      bootstrapInFlightBindingKey = '';
    }
  });
  bootstrapInFlight = requestPromise;
  bootstrapInFlightBindingKey = requestBindingKey;
  return requestPromise;
}

async function refreshWorkspaceSessionsAfterBootstrap(
  payload: BootstrapPayload,
  requestBindingKey: string,
  requestSeq: number,
): Promise<void> {
  if (!payload.workspace.workspaceId) {
    return;
  }
  try {
    const snapshot = await getWorkspaceSessions(
      payload.workspace.workspaceId,
      payload.workspace.rootPath,
    );
    if (!isCurrentBootstrapRequest(requestBindingKey, requestSeq)) {
      return;
    }
    if (snapshot.workspace.workspaceId !== payload.workspace.workspaceId) {
      console.warn('[web-client-bridge] 忽略工作区不一致的会话摘要快照', {
        expectedWorkspaceId: payload.workspace.workspaceId,
        actualWorkspaceId: snapshot.workspace.workspaceId,
      });
      return;
    }
    if (payload.sessionId && !snapshot.sessions.some((session) => session.id === payload.sessionId)) {
      console.warn('[web-client-bridge] 忽略不包含当前会话的摘要快照', {
        workspaceId: payload.workspace.workspaceId,
        sessionId: payload.sessionId,
      });
      return;
    }
    emitDataMessage('sessionsUpdated', {
      workspaceId: payload.workspace.workspaceId,
      sessions: snapshot.sessions,
      runtimeEpoch: snapshot.runtimeEpoch,
      eventStreamNextSequence: snapshot.eventStreamNextSequence,
    });
  } catch (error) {
    console.warn('[web-client-bridge] bootstrap 后刷新会话摘要失败:', error);
  }
}

async function fetchSettingsBootstrap(
  force = false,
  bootstrapScope: 'core' | 'full' = 'full',
  bindingKey = settingsBootstrapBindingKey(),
  requestSeq = settingsBootstrapRequestSeq,
): Promise<SettingsBootstrapPayload> {
  const cachedScopeSatisfiesRequest = cachedSettingsBootstrapScope === 'full'
    || cachedSettingsBootstrapScope === bootstrapScope;
  if (
    !force
    && cachedSettingsBootstrap
    && cachedScopeSatisfiesRequest
    && cachedSettingsBootstrapBindingKey === bindingKey
  ) {
    return cachedSettingsBootstrap;
  }
  const snapshot = await getAgentSettingsBootstrap({ bootstrapScope });
  if (isCurrentSettingsBootstrapRequest(bindingKey, requestSeq)) {
    cachedSettingsBootstrap = snapshot;
    cachedSettingsBootstrapScope = snapshot.bootstrapScope === 'core' ? 'core' : 'full';
    cachedSettingsBootstrapBindingKey = bindingKey;
  }
  return snapshot;
}

async function dispatchSettingsBootstrap(
  force = false,
  bootstrapScope: 'core' | 'full' = 'full',
): Promise<void> {
  const bindingKey = settingsBootstrapBindingKey();
  if (
    !force
    && settingsBootstrapInFlight
    && settingsBootstrapInFlightBindingKey === bindingKey
  ) {
    return settingsBootstrapInFlight;
  }
  const requestSeq = settingsBootstrapRequestSeq + 1;
  settingsBootstrapRequestSeq = requestSeq;
  const doDispatch = async (): Promise<void> => {
    const snapshot: SettingsBootstrapSnapshot = await fetchSettingsBootstrap(
      force,
      bootstrapScope,
      bindingKey,
      requestSeq,
    );
    if (!isCurrentSettingsBootstrapRequest(bindingKey, requestSeq)) {
      return;
    }
    emitDataMessage('settingsBootstrapLoaded', snapshot as unknown as Record<string, unknown>);
  };
  const request = doDispatch().finally(() => {
    if (settingsBootstrapInFlight === request) {
      settingsBootstrapInFlight = null;
      settingsBootstrapInFlightBindingKey = '';
    }
  });
  settingsBootstrapInFlight = request;
  settingsBootstrapInFlightBindingKey = bindingKey;
  return settingsBootstrapInFlight;
}

function refreshSettingsBootstrapForCurrentWorkspace(reason: string): void {
  void dispatchSettingsBootstrap(true, 'core').catch((error) => {
    reportExpectedRecoveryFailure(
      i18n.t('settings.toast.action.loadSettingsData'),
      `[web-client-bridge] workspace 变化后刷新 settings 失败(${reason}):`,
      error,
    );
  });
}

async function dispatchExecutionStats(): Promise<void> {
  const payload = await getAgentExecutionStats();
  emitDataMessage('executionStatsUpdate', payload as unknown as Record<string, unknown>);
}

/**
 * 从 Registry 加载 enabledAgents 数据
 * 合并 AgentBinding + RoleTemplate → 前端轻量 EnabledAgent 列表
 * 通过 registryAgentsLoaded 事件推送到消息处理层
 */
async function dispatchRegistryAgents(): Promise<void> {
  try {
    const [agents, templates, engines] = await Promise.all([
      listAgentRegistryAgents(),
      listAgentRoleTemplates(),
      listAgentRegistryEngines(),
    ]);
    const templateMap = new Map<string, Record<string, unknown>>();
    for (const t of templates) {
      templateMap.set(t.templateId, t as unknown as Record<string, unknown>);
    }
    // 可派发代理角色默认可用，无需 enabled 过滤。
    const enabledAgents = agents
      .map((a) => {
        const tmpl = templateMap.get(a.templateId as string);
        const defaultUI = (tmpl?.defaultUI ?? {}) as Record<string, unknown>;
        return {
          templateId: a.templateId as string,
          displayName: (tmpl?.displayName as string) || (a.templateId as string),
          displayNameKey: (tmpl?.i18n as Record<string, unknown> | undefined)?.displayNameKey as string | undefined,
          engineId: a.engineId as string,
          order: (a.order as number) || 0,
          colorToken: (defaultUI.colorToken as string) || '',
          icon: (defaultUI.icon as string) || undefined,
        };
      })
      .sort((x: { order: number }, y: { order: number }) => x.order - y.order);
    emitDataMessage('registryAgentsLoaded', {
      enabledAgents,
      roleTemplates: templates,
      registryAgents: agents,
      registryEngines: engines,
    });
  } catch (err) {
    // Registry 加载失败不阻断主流程，任务执行展示会基于已启用角色继续渲染
    console.warn('[web-client-bridge] Registry agents 加载失败，执行展示将使用引擎 fallback', err);
  }
}

async function dispatchProjectKnowledge(scope: BridgeRequestScope = currentSessionBindingOverride()): Promise<void> {
  const requestWorkspaceScope = resolveWorkspaceScopeFromSource(scope);
  const requestWorkspaceId = requestWorkspaceScope.workspaceId;
  const requestWorkspacePath = requestWorkspaceScope.workspacePath;
  const knowledgeRequestId = trimBridgeString(scope.knowledgeRequestId);
  const [payload, relationPayload] = await Promise.all([
    getAgentProjectKnowledge({
      scope: 'workspace',
      workspaceId: requestWorkspaceId,
      workspacePath: requestWorkspacePath,
    }),
    listAgentKnowledgeRelations({
      scope: 'workspace',
      workspaceId: requestWorkspaceId,
      workspacePath: requestWorkspacePath,
    }),
  ]);
  const responseWorkspaceId = typeof payload.workspaceId === 'string' && payload.workspaceId.trim()
    ? payload.workspaceId.trim()
    : requestWorkspaceId;
  const responseWorkspacePath = typeof payload.workspacePath === 'string' && payload.workspacePath.trim()
    ? payload.workspacePath.trim()
    : requestWorkspacePath;
  emitDataMessage('projectKnowledgeLoaded', {
    ...payload,
    relations: relationPayload.relations,
    relationMeta: {
      totalRelations: relationPayload.totalRelations,
      truncated: relationPayload.truncated,
    },
    ...(knowledgeRequestId ? { knowledgeRequestId } : {}),
    workspaceId: responseWorkspaceId,
    workspacePath: responseWorkspacePath,
  });
}

async function emitKnowledgePayload(scope: BridgeRequestScope = currentSessionBindingOverride()): Promise<void> {
  await dispatchProjectKnowledge(scope);
}

async function commitSessionNavigation(
  target: 'draft' | 'session',
  binding: AgentBindingOverride,
  requestId: string,
  sessionId: string,
  carriedSessionConfig: Record<string, unknown>,
): Promise<void> {
  const workspaceId = binding.scope === 'workspace' ? binding.workspaceId : '';
  const workspacePath = binding.scope === 'workspace' ? binding.workspacePath : '';
  const previousWorkspaceId = currentWorkspaceId;
  const previousWorkspacePath = currentWorkspacePath;
  const navigationLease = beginSessionNavigation(requestId);
  invalidateSessionTurnSubmissionsForNavigation();
  const navigationBootstrapSeq = bootstrapRequestSeq;
  const navigationAbortController = new AbortController();
  let navigationTimedOut = false;
  const navigationTimeoutId = window.setTimeout(() => {
    navigationTimedOut = true;
    navigationAbortController.abort();
  }, SESSION_NAVIGATION_TIMEOUT_MS);
  try {
    // 导航事务拥有新的会话归属；旧提交即使随后收到 HTTP/SSE，也不能重新夺回当前页面。
    let response: Response;
    let rawPayload: unknown;
    try {
      response = await getTransport().request(agentUrl('/api/session/navigation'), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          target,
          scope: binding.scope,
          ...(binding.scope === 'workspace' ? { workspaceId, workspacePath } : {}),
          ...(target === 'session' ? { sessionId } : {}),
        }),
        signal: navigationAbortController.signal,
      });
      if (!response.ok) {
        throw new Error(`session navigation failed: ${response.status}`);
      }
      rawPayload = await response.json();
    } catch (error) {
      if (navigationTimedOut) {
        throw new Error('会话导航请求超时');
      }
      throw error;
    } finally {
      window.clearTimeout(navigationTimeoutId);
    }
    if (!isCurrentSessionNavigation(navigationLease)) {
      return;
    }
    const payload = normalizeBootstrapResponse(rawPayload, {
      workspaceId,
      workspacePath,
      sessionId,
    });
    if (!payload.state || typeof payload.state !== 'object' || Array.isArray(payload.state)) {
      throw new Error('会话导航响应缺少有效的 state');
    }
    const navigationBindingKey = bootstrapBindingKey({
      scope: payload.scope,
      workspaceId: payload.workspace.workspaceId,
      workspacePath: payload.workspace.rootPath,
      sessionId: payload.sessionId,
    });
    await dispatchBootstrap(payload, {
      forceEventStreamReconnect: previousWorkspaceId !== workspaceId
        || previousWorkspacePath !== workspacePath,
      rawPayload,
      navigation: {
        requestId,
        target,
        orchestratorSessionConfig: carriedSessionConfig,
      },
    });
    void refreshWorkspaceSessionsAfterBootstrap(
      payload,
      navigationBindingKey,
      navigationBootstrapSeq,
    );
  } finally {
    releaseSessionNavigation(navigationLease);
  }
}

async function navigateSession(message: ClientBridgeMessage): Promise<void> {
  const requestId = trimBridgeString(message.requestId);
  const target = message.target === 'session'
    ? 'session'
    : message.target === 'draft' ? 'draft' : null;
  if (!requestId) {
    throw new Error('会话导航缺少 requestId');
  }
  if (!target) {
    throw new Error('会话导航缺少有效的 target');
  }
  if (message.scope !== 'personal' && message.scope !== 'workspace') {
    throw new Error('会话导航缺少有效的 scope');
  }
  const binding: AgentBindingOverride = message.scope === 'personal'
    ? { scope: 'personal' }
    : requireWorkspaceRequestScope(requestScopeFromMessage(message));
  if (target === 'session') {
    const sessionId = trimBridgeString(message.sessionId);
    if (!sessionId) throw new Error('会话导航缺少 sessionId');
    await commitSessionNavigation(
      'session',
      binding,
      requestId,
      sessionId,
      {},
    );
    return;
  }
  const carriedSessionConfig = message.orchestratorSessionConfig
    && typeof message.orchestratorSessionConfig === 'object'
    && !Array.isArray(message.orchestratorSessionConfig)
    ? message.orchestratorSessionConfig as Record<string, unknown>
    : {};
  await commitSessionNavigation(
    'draft',
    binding,
    requestId,
    '',
    carriedSessionConfig,
  );
}

async function deleteSession(sessionId: string, scope: BridgeRequestScope = currentSessionBindingOverride()): Promise<void> {
  messagesState.sessionHydrating = true;
  try {
    const payload = await deleteAgentSession(sessionId, scope);
    // 不传 sessionId 提示——它指向已被删除的会话，归一化器会用它去新列表 find()
    // 找不到然后把 currentSessionId 错误地清空。让 bootstrap 自带的 currentSession 当真值：
    // 删的是当前会话 → 后端 currentSession 为空，前端也清空；
    // 删的不是当前会话 → 后端 currentSession 仍是原会话，前端继续保持激活。
    await dispatchBootstrap(normalizeBootstrapResponse(payload), { rawPayload: payload });
    emitBridgeSuccessToast(i18n.t('bridge.action.deleteSession'), i18n.t('toast.sessionDeleted'));
  } finally {
    messagesState.sessionHydrating = false;
  }
}

async function renameSession(
  sessionId: string,
  name: string,
  scope: BridgeRequestScope = currentSessionBindingOverride(),
): Promise<void> {
  const payload = await renameAgentSession(sessionId, name, scope);
  await dispatchBootstrap(normalizeBootstrapResponse(payload, { sessionId }), { rawPayload: payload });
  emitBridgeSuccessToast(i18n.t('bridge.action.renameSession'), i18n.t('toast.sessionRenamed'));
}

async function closeSession(sessionId: string, scope: BridgeRequestScope = currentSessionBindingOverride()): Promise<void> {
  const payload = await closeAgentSession(sessionId, scope);
  // 同 deleteSession：关闭后该会话不再出现在列表里，hint 会让归一化器误清空 currentSessionId
  await dispatchBootstrap(normalizeBootstrapResponse(payload), { rawPayload: payload });
  emitBridgeSuccessToast(i18n.t('bridge.action.closeSession'), i18n.t('bridge.detail.sessionClosed'));
}

// 发送消息不能被弱网络下的 SSE 握手阻塞；HTTP 提交先行，事件流在后台追上。
async function warmLiveBridgeForSubmission(reason: string): Promise<void> {
  hydrateCanonicalWorkspaceBinding();
  const hasWorkspaceBinding = Boolean(currentWorkspaceId || currentWorkspacePath);
  const hasBinding = Boolean(hasWorkspaceBinding || currentSessionId);
  if (!hasBinding) {
    await restoreBridgeState(reason, true);
    return;
  }
  const expectedKey = currentEventStreamKey();
  await ensureEventStream({
    forceReconnect: activeEventStreamKey !== expectedKey,
    waitUntilOpen: false,
  });
}

// ─── Agent-run tracking helpers ───────────────────────────────────────

/**
 * Initialize agent-run-store tracking for an agent-run root id.
 * Fetches the initial projection and starts auto-refresh + SSE subscription.
 * Defensive: logs warnings on failure but never breaks the caller.
 */
function initAgentRunTracking(
  sessionId: string,
  rootTaskId: string,
  workspaceId = currentWorkspaceId,
  workspacePath = currentWorkspacePath,
): void {
  activateAgentRunSession(sessionId, workspaceId, workspacePath);
  const currentState = getAgentRunState(sessionId, workspaceId);
  if (currentState.rootTaskId && currentState.rootTaskId !== rootTaskId) {
    clearAgentRunProjection(sessionId, undefined, workspaceId);
  }
  fetchAgentRunProjection(sessionId, rootTaskId, workspaceId, workspacePath)
    .then(() => {
      startAgentRunAutoRefresh();
    })
    .catch((error) => {
      console.warn('[web-client-bridge] Failed to initialize agent-run tracking (non-critical):', error);
    });
}

/**
 * Auto-detect the active agent-run root from session runtime state and start tracking.
 * Called during bootstrap dispatch to reconnect agent-run tracking on session load/switch.
 */
export async function autoConnectAgentRunTracking(
  sessionId: string,
  activeTaskIds: string[],
  preferredRootTaskId = '',
  workspaceId = currentWorkspaceId,
  workspacePath = currentWorkspacePath,
): Promise<void> {
  if (!sessionId || sessionId !== currentSessionId || workspaceId !== currentWorkspaceId) {
    return;
  }
  const currentState = getAgentRunState(sessionId, workspaceId);
  if (preferredRootTaskId) {
    if (currentState.rootTaskId === preferredRootTaskId) {
      return;
    }
    initAgentRunTracking(sessionId, preferredRootTaskId, workspaceId, workspacePath);
    return;
  }

  if (currentState.rootTaskId) {
    return;
  }

  if (activeTaskIds && activeTaskIds.length > 0) {
    console.warn('[web-client-bridge] active agent-run root missing; skip agent-run tracking bootstrap');
  }
}

interface ExecuteTaskInput {
  text?: string | null;
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
  requestId?: string;
  skillName?: string | null;
  goalMode?: boolean;
  accessProfile?: 'read_only' | 'restricted' | 'full_access' | null;
  orchestratorSessionConfig?: Record<string, unknown> | null;
  followUpMode?: 'queue';
  replaceTurnId?: string | null;
  images: Array<{
    name: string;
    dataUrl: string;
  }>;
  contextReferences?: Array<{
    kind: 'file' | 'directory';
    path: string;
    pathRef?: string;
    name: string;
  }>;
  browserAnnotationRefs?: string[];
  browserNodeSelections?: MessageBrowserNodeSelection[];
}

function bridgeRuntimeIsBusy(): boolean {
  return Boolean(
    messagesState.isProcessing
      || messagesState.backendProcessing
      || messagesState.sessionHydrating
      || messagesState.pendingRequests.size > 0
      || messagesState.activeMessageIds.size > 0
      || messagesState.editingTurn !== null,
  );
}

function queuedMessageFromServer(turn: QueuedSessionTurnDto): QueuedMessage {
  return {
    id: turn.queueId,
    requestId: turn.requestId?.trim() || turn.queueId,
    content: turn.content,
    text: turn.text ?? null,
    workspaceId: turn.workspaceId,
    workspacePath: turn.workspacePath?.trim() || undefined,
    sessionId: turn.sessionId,
    createdAt: turn.acceptedAt,
    skillName: turn.skillName ?? null,
    goalMode: turn.goalMode === true,
    accessProfile: turn.accessProfile ?? null,
    images: turn.images,
    contextReferences: turn.contextReferences,
    browserAnnotationRefs: turn.browserAnnotationRefs,
    browserNodeSelections: turn.browserNodeSelections,
    canGuide: turn.canGuide,
  };
}

function applyServerQueuedTurns(sessionId: string, turns: QueuedSessionTurnDto[]): void {
  setQueuedMessages(turns.map(queuedMessageFromServer));
  adoptQueuedTurnSubmissions(
    sessionId,
    turns.map((turn) => turn.requestId?.trim() || turn.queueId),
  );
}

async function syncSessionTurnQueue(
  sessionId = currentSessionId,
  workspaceId = currentWorkspaceId,
  workspacePath = currentWorkspacePath,
): Promise<void> {
  const normalizedSessionId = trimBridgeString(sessionId);
  if (!normalizedSessionId) {
    setQueuedMessages([]);
    return;
  }
  const snapshot = await getAgentSessionTurnQueue(normalizedSessionId, {
    ...currentSessionBindingOverride(normalizedSessionId, workspaceId, workspacePath),
  });
  if (currentSessionId !== normalizedSessionId) {
    return;
  }
  const snapshotSessionId = trimBridgeString(snapshot.sessionId);
  if (snapshotSessionId !== normalizedSessionId) {
    throw new Error('排队消息快照 sessionId 与请求会话不一致');
  }
  applyServerQueuedTurns(snapshotSessionId, snapshot.queuedTurns);
}

async function removeQueuedMessageFromServer(queuedMessageId: string): Promise<void> {
  const normalizedId = queuedMessageId.trim();
  if (!normalizedId || !currentSessionId) return;
  try {
    const snapshot = await removeAgentSessionTurnQueueItem(
      currentSessionId,
      normalizedId,
      currentSessionBindingOverride(),
    );
    if (snapshot.sessionId === currentSessionId) {
      applyServerQueuedTurns(snapshot.sessionId, snapshot.queuedTurns);
    }
  } catch (error) {
    emitBridgeErrorToast(i18n.t('input.queue.delete'), error);
    await syncSessionTurnQueue().catch(() => {});
  }
}

async function guideQueuedMessageFromServer(queuedMessageId: string): Promise<void> {
  const normalizedId = queuedMessageId.trim();
  if (!normalizedId || !currentSessionId || guidingQueuedMessageIds.has(normalizedId)) return;
  guidingQueuedMessageIds.add(normalizedId);
  try {
    const snapshot = await guideAgentSessionTurnQueueItem(
      currentSessionId,
      normalizedId,
      currentSessionBindingOverride(),
    );
    if (snapshot.sessionId === currentSessionId) {
      applyServerQueuedTurns(snapshot.sessionId, snapshot.queuedTurns);
      emitBridgeSuccessToast(
        i18n.t('input.queue.guide'),
        i18n.t('input.queue.guideSuccess'),
      );
    }
  } catch (error) {
    emitBridgeErrorToast(i18n.t('input.queue.guide'), error);
    await syncSessionTurnQueue().catch(() => {});
  } finally {
    guidingQueuedMessageIds.delete(normalizedId);
  }
}

async function executeTask(input: ExecuteTaskInput): Promise<boolean> {
  const text = typeof input.text === 'string' ? input.text : null;
  const normalizedText = text?.trim() || '';
  const targetWorkspaceScope = resolveWorkspaceScopeFromSource(input);
  const targetWorkspaceId = targetWorkspaceScope.workspaceId;
  const targetWorkspacePath = targetWorkspaceScope.workspacePath;
  const targetScope = targetWorkspaceId || targetWorkspacePath ? 'workspace' : 'personal';
  const targetSessionId = hasBridgeField(input, 'sessionId')
    ? trimBridgeString(input.sessionId)
    : (targetWorkspaceScope.hasWorkspaceOverride ? '' : currentSessionId);
  const replaceTurnId = trimBridgeString(input.replaceTurnId);
  if (replaceTurnId && !targetSessionId) {
    emitBridgeErrorToast(
      i18n.t('bridge.action.sendMessage'),
      new Error(i18n.t('input.editingSessionUnavailable')),
    );
    return false;
  }
  const skillName = typeof input.skillName === 'string' && input.skillName.trim()
    ? input.skillName.trim()
    : null;
  const images = Array.isArray(input.images)
    ? input.images
      .filter((image) => typeof image?.dataUrl === 'string' && image.dataUrl.trim().length > 0)
      .map((image) => ({
        name: typeof image.name === 'string' && image.name.trim().length > 0 ? image.name.trim() : 'image',
        dataUrl: image.dataUrl,
      }))
    : [];
  const contextReferences = Array.isArray(input.contextReferences)
    ? input.contextReferences
      .filter((reference) => (
        (reference?.kind === 'file' || reference?.kind === 'directory')
        && typeof reference.path === 'string'
        && reference.path.trim().length > 0
      ))
      .map((reference) => ({
        kind: reference.kind,
        path: reference.path.trim(),
        ...(typeof reference.pathRef === 'string' && reference.pathRef.trim()
          ? { pathRef: reference.pathRef.trim() }
          : {}),
        name: typeof reference.name === 'string' && reference.name.trim()
          ? reference.name.trim()
          : reference.path.trim().split(/[\\/]/u).filter(Boolean).pop() || reference.path.trim(),
      }))
    : [];
  const browserAnnotationRefs = Array.isArray(input.browserAnnotationRefs)
    ? input.browserAnnotationRefs
      .filter((annotationId): annotationId is string => typeof annotationId === 'string')
      .map((annotationId) => annotationId.trim())
      .filter(Boolean)
    : [];
  const browserNodeSelections = Array.isArray(input.browserNodeSelections)
    ? input.browserNodeSelections
      .filter((selection): selection is MessageBrowserNodeSelection => (
        Boolean(selection)
        && typeof selection.browserSessionId === 'string'
        && typeof selection.tabId === 'string'
        && typeof selection.surfaceId === 'string'
        && typeof selection.backendDomNodeId === 'number'
        && typeof selection.outerHtmlTruncated === 'boolean'
      ))
      .map((selection) => ({
        ...selection,
        browserSessionId: selection.browserSessionId.trim(),
        tabId: selection.tabId.trim(),
        surfaceId: selection.surfaceId.trim(),
        attributes: { ...selection.attributes },
        bounds: selection.bounds ? { ...selection.bounds } : selection.bounds,
      }))
      .filter((selection) => selection.browserSessionId && selection.tabId && selection.surfaceId)
    : [];
  if (!normalizedText && !skillName && images.length === 0 && contextReferences.length === 0 && browserAnnotationRefs.length === 0 && browserNodeSelections.length === 0) {
    return false;
  }
  const requestId = trimBridgeString(input.requestId) || generateMessageId();
  const userMessageId = generateMessageId();
  const placeholderMessageId = `assistant-placeholder-${requestId}`;
  const requestCreatedAt = Date.now();
  if (
    targetScope !== currentSessionScope
    || targetWorkspaceId !== currentWorkspaceId
    || targetWorkspacePath !== currentWorkspacePath
    || targetSessionId !== currentSessionId
  ) {
    persistWorkspaceBinding(targetScope, targetWorkspaceId, targetWorkspacePath, targetSessionId);
  }
  const submissionContext: SessionTurnSubmissionContext = {
    requestId,
    scope: targetScope,
    workspaceId: targetWorkspaceId,
    workspacePath: targetWorkspacePath,
    targetSessionId,
    bindingGeneration: currentBindingGeneration,
  };
  sessionTurnSubmissions.set(requestId, submissionContext);

  createRequestBinding({
    requestId,
    sessionId: targetSessionId,
    userMessageId,
    placeholderMessageId,
    createdAt: requestCreatedAt,
  });

  const localUserMessage: Message = {
    id: userMessageId,
    role: 'user',
    source: 'user',
    content: text || '',
    timestamp: requestCreatedAt,
    isStreaming: false,
    isComplete: true,
    type: 'user_input',
    images: images.map((image) => ({ ...image })),
    contextReferences: contextReferences.map((reference) => ({ ...reference })),
    browserNodeSelections: browserNodeSelections.map((selection) => ({
      ...selection,
      attributes: { ...selection.attributes },
      bounds: selection.bounds ? { ...selection.bounds } : selection.bounds,
    })),
    metadata: {
      requestId,
      localSubmission: true,
      sendingAnimation: true,
      ...(browserAnnotationRefs.length > 0 ? { browserAnnotationRefs } : {}),
      ...(skillName ? { skillName } : {}),
      ...(input.goalMode === true ? { goalMode: true } : {}),
    },
  };
  beginLocalTurnSubmission({
    requestId,
    sessionId: targetSessionId,
    placeholderMessageId,
    startedAt: requestCreatedAt,
    message: localUserMessage,
    workspaceId: targetWorkspaceId,
    workspacePath: targetWorkspacePath,
    source: 'orchestrator',
    agent: 'orchestrator',
  });

  try {
    void Promise.resolve()
      .then(() => warmLiveBridgeForSubmission('execute_task_preflight'))
      .catch((preflightError) => {
        console.warn('[web-client-bridge] 发送前事件流预连接失败，提交继续执行:', preflightError);
      });
    const turnResult = await submitSessionTurn({
      text,
      skillName,
      locale: i18n.locale,
      goalMode: input.goalMode === true,
      images,
      contextReferences,
      browserAnnotationRefs,
      browserNodeSelections,
      accessProfile: input.accessProfile ?? null,
      orchestratorSessionConfig: input.orchestratorSessionConfig ?? null,
      requestId,
      userMessageId,
      placeholderMessageId,
      replaceTurnId: replaceTurnId || null,
    }, targetScope === 'workspace' ? {
      scope: 'workspace',
      workspaceId: targetWorkspaceId,
      workspacePath: targetWorkspacePath,
      sessionId: targetSessionId,
    } : {
      scope: 'personal',
      sessionId: targetSessionId,
    });

    const resolvedSessionId = typeof turnResult.sessionId === 'string' && turnResult.sessionId.trim()
      ? turnResult.sessionId.trim()
      : targetSessionId;
    const submissionCanCommit = Boolean(
      resolvedSessionId && submissionContextCanCommit(submissionContext, resolvedSessionId),
    );
    if (submissionCanCommit && !submissionContext.targetSessionId) {
      submissionContext.acceptedSessionId = resolvedSessionId;
    }
    if (resolvedSessionId) {
      emitSessionTurnAccepted({
        sessionId: resolvedSessionId,
        workspaceId: targetWorkspaceId,
        requestId,
        runtimeEpoch: turnResult.runtimeEpoch,
        eventStreamNextSequence: turnResult.eventStreamNextSequence,
        acceptedAt: turnResult.acceptedAt,
        sessionSummary: turnResult.sessionSummary ?? null,
        createdSession: turnResult.createdSession,
        route: turnResult.route,
        canonicalSchemaVersion: turnResult.canonicalSchemaVersion,
        canonicalEventKind: turnResult.canonicalEventKind,
        canonicalEventId: turnResult.canonicalEventId,
        canonicalEventSeq: turnResult.canonicalEventSeq,
        canonicalOccurredAt: turnResult.canonicalOccurredAt,
        canonicalTurn: turnResult.canonicalTurn,
        canonicalItem: turnResult.canonicalItem,
        submissionContext: submissionCanCommit ? submissionContext : null,
      });
    }
    emitSessionTurnSubmissionSettled(requestId, 'accepted');
    if (resolvedSessionId && submissionCanCommit) {
      persistWorkspaceBinding(targetScope, targetWorkspaceId, targetWorkspacePath, resolvedSessionId);
      adoptCurrentSessionIdForLiveTurn(resolvedSessionId);
    }
    if (turnResult.queued) {
      const queuedProjectionRetained = resolvedSessionId
        ? markLocalTurnSubmissionQueued(requestId, resolvedSessionId)
        : false;
      if (!queuedProjectionRetained) {
        clearRequestBinding(requestId);
      }
      finishSessionTurnSubmission(submissionContext);
      if (resolvedSessionId && submissionCanCommit) {
        void syncSessionTurnQueue(
          resolvedSessionId,
          targetWorkspaceId,
          targetWorkspacePath,
        ).catch((error) => {
          reportExpectedRecoveryFailure(
            i18n.t('bridge.action.syncMessages'),
            '[web-client-bridge] 排队消息接受后同步队列失败:',
            error,
          );
          scheduleRecovery('queued_turn_queue_sync_failed', error, true);
        });
        void fetchBootstrap({
          forceFresh: true,
          refreshSessionTurnQueueAfterBootstrap: false,
        }).catch((error) => {
          reportExpectedRecoveryFailure(
            i18n.t('bridge.action.syncTurnState'),
            '[web-client-bridge] 排队消息接受后 bootstrap 同步失败:',
            error,
          );
          scheduleRecovery('queued_turn_bootstrap_failed', error, true);
        });
      }
      emitBridgeSuccessToast(
        i18n.t('bridge.action.sendMessage'),
        i18n.t('input.queue.header', { count: turnResult.queuePosition ?? 1 }),
      );
      return true;
    }
    // 只有仍属于当前提交代际的响应才可进入当前会话时间轴。旧草稿请求
    // 即使服务端稍后返回，也不能把 canonical event 注入新草稿或其他会话。
    if (replaceTurnId) {
      completeTurnEditing(replaceTurnId);
    }

    const canonicalUserMessageId = turnResult.userMessageItemId || userMessageId;
    // acceptedAt 是 wall-clock 时间戳，不是 turn 序号。服务端提供 canonical
    // turn/item 时只采用其权威序号；缺失时保留创建 binding 时的本地顺序号。
    const canonicalTurnSeq = canonicalTurnSeqFromResult(turnResult);
    updateRequestBinding(requestId, {
      userMessageId: canonicalUserMessageId,
      placeholderMessageId,
      ...(typeof canonicalTurnSeq === 'number' ? { turnSeq: canonicalTurnSeq } : {}),
    });
    const successMessage = turnResult.route === 'task'
      ? i18n.t('bridge.detail.taskSubmitted')
      : turnResult.route === 'continue'
        ? i18n.t('bridge.detail.continueSubmitted')
        : i18n.t('bridge.detail.messageSent');
    emitBridgeSuccessToast(
      i18n.t('bridge.action.sendMessage'),
      successMessage,
    );

    setCurrentInterruptTaskId(turnResult.actionTaskId || '');
    const rootTaskId = turnResult.rootTaskId;
    if (rootTaskId && resolvedSessionId) {
      initAgentRunTracking(resolvedSessionId, rootTaskId, targetWorkspaceId, targetWorkspacePath);
    }

    // 确保 SSE 连接存活以接收增量事件
    void ensureEventStream({ forceReconnect: false, waitUntilOpen: false }).catch((err) => {
      console.warn('[web-client-bridge] executeTask 后 SSE 连接确认失败:', err);
    });
    finishSessionTurnSubmission(submissionContext);
    return true;
  } catch (error) {
    clearCurrentInterruptTaskId();
    console.error('[web-client-bridge] 执行任务失败:', error);
    emitSessionTurnSubmissionSettled(requestId, 'failed');
    clearRequestBinding(requestId);
    finishSessionTurnSubmission(submissionContext);
    emitBridgeErrorToast(i18n.t('bridge.action.sendMessage'), error);
    if (shouldRecoverFromBridgeError(error)) {
      closeEventStream();
      scheduleRecovery('execute_task_failed', error, true);
    }
    return false;
  }
}

async function interruptTask(): Promise<void> {
  const sessionId = currentSessionId.trim();
  if (!sessionId) {
    emitBridgeErrorToast(
      i18n.t('bridge.action.stopTask'),
      new Error(i18n.t('bridge.detail.noStoppableSession')),
    );
    return;
  }
  const processingSnapshot = captureProcessingRequestSnapshot(sessionId);
  try {
    const response = await interruptAgentSession(sessionId);
    const responseSessionId = trimBridgeString(response.sessionId);
    if (responseSessionId !== sessionId) {
      throw new Error('中断响应 sessionId 与请求会话不一致');
    }
    const interruptedTurnId = trimBridgeString(response.turnId);
    const interruptedTurn = turnStoreState.reducer.sessionId === responseSessionId
      ? turnStoreState.reducer.turns.find((turn) => turn.turnId === interruptedTurnId)
      : undefined;
    const interruptedRequestId = interruptedTurn
      ? canonicalTurnRequestId(interruptedTurn)
      : '';
    const snapshotRequestId = interruptedRequestId
      && processingSnapshot.requestIds.includes(interruptedRequestId)
      ? interruptedRequestId
      : processingSnapshot.requestIds.length === 1
        ? processingSnapshot.requestIds[0]
        : '';

    if (response.interrupted === true) {
      const requestIdsToSettle = snapshotRequestId
        ? [snapshotRequestId]
        : processingSnapshot.requestIds;
      for (const requestId of requestIdsToSettle) {
        emitForcedProcessingTerminal({
          sessionId: responseSessionId,
          requestId,
          reason: 'user_interrupt_confirmed',
        });
      }
      // forced terminal 由消息处理器消费；这里同步结算同一快照，保证未注册
      // listener 或 interrupt 发生在 canonical projection 之前时也不会卡住 processing。
      settleProcessingRequestSnapshot({
        sessionId: responseSessionId,
        requestIds: requestIdsToSettle,
      });
      if (requestIdsToSettle.length === 0) {
        settleProcessingRequestSnapshot(processingSnapshot);
      }
    }
    if (response.nextQueuedTurnStarted !== true) {
      clearContinueRequestInFlight();
      clearCurrentInterruptTaskId();
    }

    void syncSessionTurnQueue(
      responseSessionId,
      currentWorkspaceId,
      currentWorkspacePath,
    ).catch((queueSyncError) => {
      reportExpectedRecoveryFailure(
        i18n.t('bridge.action.syncMessages'),
        '[web-client-bridge] 停止会话后同步排队消息失败:',
        queueSyncError,
      );
      scheduleRecovery('user_interrupt_queue_sync_failed', queueSyncError, true);
    });
    void fetchBootstrap({
      forceFresh: true,
      refreshSessionTurnQueueAfterBootstrap: false,
    }).catch((bootstrapError) => {
      reportExpectedRecoveryFailure(
        i18n.t('bridge.action.syncTurnState'),
        '[web-client-bridge] 停止会话后 bootstrap 同步失败:',
        bootstrapError,
      );
      scheduleRecovery('user_interrupt_bootstrap_failed', bootstrapError, true);
    });
  } catch (error) {
    console.error('[web-client-bridge] 中断执行失败:', error);
    settleProcessingRequestSnapshot(processingSnapshot);
    emitBridgeErrorToast(i18n.t('bridge.action.stopTask'), error);
    scheduleRecovery('user_interrupt_failed', error, true);
  }
}

async function continueSessionExecution(): Promise<void> {
  const sessionId = currentSessionId.trim();
  if (!sessionId) {
    emitBridgeErrorToast(
      i18n.t('bridge.action.continueSession'),
      new Error(i18n.t('bridge.detail.noContinuableSession')),
    );
    return;
  }
  if (continueRequestId) {
    if (messagesState.pendingRequests.has(continueRequestId)) {
      return;
    }
    continueRequestId = '';
  }
  const requestId = `continue-${generateMessageId()}`;
  continueRequestId = requestId;
  window.dispatchEvent(new CustomEvent('magi:interruptedRecoveryContinueStatus', {
    detail: { status: 'pending' },
  }));
  const accepted = await executeTask({
    text: '继续',
    workspaceId: currentWorkspaceId,
    workspacePath: currentWorkspacePath,
    sessionId,
    requestId,
    images: [],
    contextReferences: [],
  });
  if (accepted) {
    void refreshCurrentGoal(sessionId, currentWorkspaceId, currentWorkspacePath);
    window.dispatchEvent(new CustomEvent('magi:interruptedRecoveryContinueStatus', {
      detail: { status: 'accepted' },
    }));
    return;
  }
  window.dispatchEvent(new CustomEvent('magi:interruptedRecoveryContinueStatus', {
    detail: { status: 'failed' },
  }));
}

function escapePreviewHtml(content: string): string {
  return content
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;');
}

function openPreviewWindow(title: string, subtitle: string, content: string, mode: 'file' | 'diff'): void {
  const popup = window.open('', '_blank', 'noopener,noreferrer');
  if (!popup) {
    throw new Error(i18n.t('bridge.detail.previewWindowBlocked'));
  }
  const escapedTitle = escapePreviewHtml(title);
  const escapedSubtitle = escapePreviewHtml(subtitle);
  const escapedContent = escapePreviewHtml(content);
  const bodyClass = mode === 'diff' ? 'diff' : 'file';
  const lang = i18n.locale === 'en-US' ? 'en-US' : 'zh-CN';
  popup.document.write(`<!doctype html>
<html lang="${lang}">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>${escapedTitle}</title>
  <style>
    :root { color-scheme: light dark; }
    body { margin: 0; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: #0f172a; color: #e2e8f0; }
    .wrap { padding: 20px; }
    .title { font-size: 20px; font-weight: 700; margin: 0 0 4px; }
    .subtitle { font-size: 12px; color: #94a3b8; margin: 0 0 16px; }
    pre { margin: 0; padding: 16px; border-radius: 12px; background: #111827; border: 1px solid rgba(148,163,184,.18); overflow: auto; line-height: 1.55; }
    .diff pre { background: #0b1220; }
    code { font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; white-space: pre-wrap; word-break: break-word; }
  </style>
</head>
<body class="${bodyClass}">
  <div class="wrap">
    <h1 class="title">${escapedTitle}</h1>
    <p class="subtitle">${escapedSubtitle}</p>
    <pre><code>${escapedContent}</code></pre>
  </div>
</body>
</html>`);
  popup.document.close();
}

function openDiagramSvgPreview(title: string, svgContent: string): void {
  const popup = window.open('', '_blank', 'noopener,noreferrer');
  if (!popup) {
    throw new Error(i18n.t('bridge.detail.previewWindowBlocked'));
  }
  const escapedTitle = escapePreviewHtml(title);
  const lang = i18n.locale === 'en-US' ? 'en-US' : 'zh-CN';
  popup.document.write(`<!doctype html>
<html lang="${lang}">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>${escapedTitle}</title>
  <style>
    :root { color-scheme: light dark; }
    body { margin: 0; min-height: 100vh; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: #0f172a; color: #e2e8f0; }
    .wrap { min-height: 100vh; display: flex; flex-direction: column; padding: 20px; box-sizing: border-box; }
    .title { font-size: 20px; font-weight: 700; margin: 0 0 16px; }
    .diagram { flex: 1; min-height: 0; display: flex; align-items: center; justify-content: center; overflow: auto; padding: 16px; border: 1px solid rgba(148,163,184,.18); border-radius: 12px; background: #111827; }
    .diagram svg { max-width: 100%; height: auto; }
  </style>
</head>
<body>
  <div class="wrap">
    <h1 class="title">${escapedTitle}</h1>
    <div class="diagram">${svgContent}</div>
  </div>
</body>
</html>`);
  popup.document.close();
}

function openDiagramPreview(source: string, title?: string, svgContent?: string): void {
  const resolvedTitle = title?.trim() || i18n.t('bridge.preview.diagramTitle');
  const sanitizedSvg = typeof svgContent === 'string' ? sanitizeSvgContent(svgContent) : '';
  if (sanitizedSvg) {
    openDiagramSvgPreview(resolvedTitle, sanitizedSvg);
    return;
  }
  openPreviewWindow(resolvedTitle, i18n.t('bridge.preview.diagramSource'), source, 'file');
}

async function openFilePreview(
  filePath: string,
  previewContent?: string,
  scope: BridgeRequestScope = {},
): Promise<void> {
  if (typeof previewContent === 'string') {
    openPreviewWindow(filePath, i18n.t('bridge.preview.file'), previewContent, 'file');
    return;
  }
  const payload = await getAgentFilePreview(filePath, scope);
  openPreviewWindow(payload.filePath || filePath, i18n.t('bridge.preview.file'), payload.content || '', 'file');
}

async function openDiffPreview(
  filePath: string,
  diffContent?: string,
  scope: BridgeRequestScope = currentSessionBindingOverride(),
): Promise<void> {
  if (typeof diffContent === 'string') {
    openPreviewWindow(filePath, i18n.t('bridge.preview.diff'), diffContent, 'diff');
    return;
  }
  const payload = await getAgentChangeDiff(filePath, requireWorkspaceRequestScope(scope));
  openPreviewWindow(payload.filePath || filePath, i18n.t('bridge.preview.diff'), payload.diff || '', 'diff');
}

async function updateSetting(key: string, value: unknown): Promise<void> {
  const payload = await updateAgentRuntimeSetting(key, value);
  if (cachedSettingsBootstrap) {
    cachedSettingsBootstrap = {
      ...cachedSettingsBootstrap,
      runtimeSettings: {
        ...cachedSettingsBootstrap.runtimeSettings,
        locale: payload.locale,
        conversationDisplayMode: payload.conversationDisplayMode,
      },
    };
  }
  if (key === 'locale') {
    safeLocalStorageSetItem('magi-locale', payload.locale);
  }
  await dispatchSettingsBootstrap(true);
  if (key === 'locale') {
    await dispatchRegistryAgents();
  }
}

type NotificationCenterOperation = 'load' | 'report' | 'mark-read' | 'clear' | 'resolve' | 'remove';

interface NotificationOperationScope {
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
}

async function resetExecutionStats(): Promise<void> {
  await resetAgentExecutionStats();
  await dispatchExecutionStats();
}

function resolveNotificationOperationScope(message: Record<string, unknown>): NotificationOperationScope | null {
  const sessionId = trimBridgeString(message.sessionId);
  const workspaceId = trimBridgeString(message.workspaceId);
  if (!workspaceId && !sessionId) {
    return null;
  }
  return {
    ...(workspaceId ? { workspaceId } : {}),
    workspacePath: trimBridgeString(message.workspacePath),
    ...(sessionId ? { sessionId } : {}),
  };
}

function emitNotificationsStatus(
  operation: NotificationCenterOperation,
  scope: NotificationOperationScope,
  isLoading: boolean,
  error?: unknown,
): void {
  emitDataMessage('notificationsStatus', {
    sessionId: scope.sessionId,
    workspaceId: scope.workspaceId || null,
    workspacePath: scope.workspacePath,
    operation,
    isLoading,
    error: error === undefined ? null : 'operation_failed',
    updatedAt: Date.now(),
  });
}

async function runNotificationOperation(
  operation: NotificationCenterOperation,
  scope: NotificationOperationScope,
  task: (scope: NotificationOperationScope) => Promise<Record<string, unknown>>,
): Promise<void> {
  emitNotificationsStatus(operation, scope, true);
  try {
    const payload = await task(scope);
    emitDataMessage('notificationsLoaded', payload);
    emitNotificationsStatus(operation, scope, false);
  } catch (error) {
    emitNotificationsStatus(operation, scope, false, error);
    throw error;
  }
}

async function loadNotifications(scope: NotificationOperationScope): Promise<void> {
  await runNotificationOperation('load', scope, async (operationScope) => (
    await getAgentNotifications(operationScope) as unknown as Record<string, unknown>
  ));
}

async function reportIncident(
  scope: NotificationOperationScope,
  incident: Record<string, unknown>,
): Promise<void> {
  await runNotificationOperation('report', scope, async (operationScope) => (
    await reportAgentIncident(incident, operationScope) as unknown as Record<string, unknown>
  ));
}

async function markAllNotificationsRead(scope: NotificationOperationScope): Promise<void> {
  await runNotificationOperation('mark-read', scope, async (operationScope) => (
    await markAllAgentNotificationsRead(operationScope) as unknown as Record<string, unknown>
  ));
}

async function clearAllNotifications(scope: NotificationOperationScope): Promise<void> {
  await runNotificationOperation('clear', scope, async (operationScope) => (
    await clearAgentNotifications(operationScope) as unknown as Record<string, unknown>
  ));
}

async function removeNotification(scope: NotificationOperationScope, notificationId: string): Promise<void> {
  await runNotificationOperation('remove', scope, async (operationScope) => (
    await removeAgentNotification(notificationId, operationScope) as unknown as Record<string, unknown>
  ));
}

async function resolveNotification(scope: NotificationOperationScope, notificationId: string): Promise<void> {
  await runNotificationOperation('resolve', scope, async (operationScope) => (
    await resolveAgentNotification(notificationId, operationScope) as unknown as Record<string, unknown>
  ));
}

async function saveWorkerConfig(worker: string, config: Record<string, unknown>): Promise<void> {
  await saveAgentWorkerConfig(worker, config);
  clearSettingsBootstrapCache();
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.saveWorkerConfig'),
    i18n.t('settings.toast.workerConfigSaved', { worker }),
  );
}

async function saveUserRules(data: Record<string, unknown>): Promise<void> {
  await saveAgentUserRules(data);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.saveUserRules'),
    i18n.t('settings.toast.userRulesSaved'),
  );
}

async function saveOrchestratorConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentOrchestratorConfig(config);
  clearSettingsBootstrapCache();
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.saveOrchestratorConfig'),
    i18n.t('settings.toast.orchestratorConfigSaved'),
  );
}

async function saveAuxiliaryConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentAuxiliaryConfig(config);
  clearSettingsBootstrapCache();
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.saveAuxiliaryConfig'),
    i18n.t('settings.toast.auxiliaryConfigSaved'),
  );
}

async function saveSafeguardConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentSafeguardConfig(config);
  clearSettingsBootstrapCache();
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.saveSafeguardConfig'),
    i18n.t('settings.toast.safeguardConfigSaved'),
  );
}

async function testWorkerConnection(worker: string, config: Record<string, unknown>): Promise<void> {
  const payload = await testAgentWorkerConnection(worker, config);
  emitDataMessage('workerConnectionTestResult', payload);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.testWorkerConnection'),
    i18n.t('settings.toast.workerConnectionTestCompleted', { worker }),
  );
}

async function testOrchestratorConnection(config: Record<string, unknown>): Promise<void> {
  const payload = await testAgentOrchestratorConnection(config);
  emitDataMessage('orchestratorConnectionTestResult', payload);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.testOrchestratorConnection'),
    i18n.t('settings.toast.orchestratorConnectionTestCompleted'),
  );
}

async function testAuxiliaryConnection(config: Record<string, unknown>): Promise<void> {
  const payload = await testAgentAuxiliaryConnection(config);
  emitDataMessage('auxiliaryConnectionTestResult', payload);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.testAuxiliaryConnection'),
    i18n.t('settings.toast.auxiliaryConnectionTestCompleted'),
  );
}

async function fetchModelList(config: Record<string, unknown>, target: string): Promise<void> {
  const blockReason = resolveModelListFetchBlockReason(config);
  if (blockReason) {
    emitBridgeInfoToast(
      i18n.t('settings.toast.action.fetchModelList'),
      blockReason === 'full_url_mode'
        ? i18n.t('config.toast.modelListUnsupportedInFullMode')
        : i18n.t('config.toast.fillBaseUrlFirst'),
    );
    return;
  }
  const payload = await fetchAgentModelList(config, target);
  emitDataMessage('modelListFetched', { ...payload });
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.fetchModelList'),
    i18n.t('settings.toast.modelListRefreshedForTarget', { target }),
  );
}

async function addMcpServer(server: Record<string, unknown>): Promise<void> {
  const payload = await addAgentMcpServer(server);
  emitDataMessage('mcpServerAdded', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.addMcpServer'),
    i18n.t('settings.toast.mcpServerAdded'),
  );
}

async function updateMcpServer(serverId: string, updates: Record<string, unknown>): Promise<void> {
  const payload = await updateAgentMcpServer(serverId, updates);
  emitDataMessage('mcpServerUpdated', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.updateMcpServer'),
    i18n.t('settings.toast.mcpServerUpdated'),
  );
}

async function deleteMcpServer(serverId: string): Promise<void> {
  const payload = await deleteAgentMcpServer(serverId);
  emitDataMessage('mcpServerDeleted', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.deleteMcpServer'),
    i18n.t('settings.toast.mcpServerDeleted'),
  );
}

async function getMcpServerTools(serverId: string): Promise<void> {
  const payload = await getAgentMcpServerTools(serverId);
  emitDataMessage('mcpServerTools', payload);
  if (isMcpToolPayloadUnavailable(payload)) {
    await dispatchSettingsBootstrap(true);
    return;
  }
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.loadMcpToolList'),
    i18n.t('settings.toast.mcpToolListLoaded'),
  );
}

async function refreshMcpTools(serverId: string): Promise<void> {
  const payload = await refreshAgentMcpTools(serverId);
  emitDataMessage('mcpToolsRefreshed', payload);
  await dispatchSettingsBootstrap(true);
  if (isMcpToolPayloadUnavailable(payload)) {
    return;
  }
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.refreshMcpTools'),
    i18n.t('settings.toast.mcpToolsRefreshed'),
  );
}

function isMcpToolPayloadUnavailable(payload: Record<string, unknown>): boolean {
  return payload.connected === false;
}

async function connectMcpServer(serverId: string): Promise<void> {
  await connectAgentMcpServer(serverId);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.connectMcpServer'),
    i18n.t('settings.toast.mcpServerConnected'),
  );
}

async function disconnectMcpServer(serverId: string): Promise<void> {
  await disconnectAgentMcpServer(serverId);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.disconnectMcpServer'),
    i18n.t('settings.toast.mcpServerDisconnected'),
  );
}

async function addRepository(url: string): Promise<void> {
  const payload = await addAgentRepository(url);
  emitDataMessage('repositoryAdded', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.addRepository'),
    i18n.t('settings.toast.repositoryAdded'),
  );
}

async function updateRepository(repositoryId: string, updates: Record<string, unknown>): Promise<void> {
  await updateAgentRepository(repositoryId, updates);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.updateRepository'),
    i18n.t('settings.toast.repositoryUpdated'),
  );
}

async function deleteRepository(repositoryId: string): Promise<void> {
  const payload = await deleteAgentRepository(repositoryId);
  emitDataMessage('repositoryDeleted', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.deleteRepository'),
    i18n.t('settings.toast.repositoryDeleted'),
  );
}

async function refreshRepository(repositoryId: string): Promise<void> {
  const payload = await refreshAgentRepository(repositoryId);
  emitDataMessage('repositoryRefreshed', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.refreshRepository'),
    i18n.t('settings.toast.repositoryRefreshed'),
  );
}

async function loadSkillLibrary(): Promise<void> {
  const payload = await loadAgentSkillLibrary();
  emitDataMessage('skillLibraryLoaded', {
    skills: payload.skills,
    failedRepositoryCount: payload.failedRepositoryCount ?? 0,
  });
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.loadSkillLibrary'),
    i18n.t('settings.toast.skillLibraryLoaded'),
  );
}

async function installSkill(skillId: string): Promise<void> {
  try {
    const payload = await installAgentSkill(skillId);
    emitDataMessage('skillInstalled', payload);
    await dispatchSettingsBootstrap(true);
    await loadSkillLibrary();
    emitBridgeSuccessToast(
      i18n.t('settings.toast.action.installSkill'),
      i18n.t('settings.toast.skillInstalled'),
    );
  } catch (error) {
    emitDataMessage('skillInstallFailed', {
      skillId,
      error: i18n.t('bridge.toast.actionFailed', {
        action: i18n.t('settings.toast.action.installSkill'),
      }),
      source: 'repository',
    });
    emitBridgeErrorToast(i18n.t('settings.toast.action.installSkill'), error);
  }
}

async function installLocalSkill(directoryPath?: string): Promise<void> {
  try {
    const payload = await installAgentLocalSkill(directoryPath);
    if (payload.canceled === true) {
      emitDataMessage('skillInstallFailed', {
        canceled: true,
        source: 'local',
      });
      return;
    }
    emitDataMessage('skillInstalled', payload);
    await dispatchSettingsBootstrap(true);
    await loadSkillLibrary();
    emitBridgeSuccessToast(
      i18n.t('settings.toast.action.installLocalSkill'),
      i18n.t('settings.toast.localSkillInstalled'),
    );
  } catch (error) {
    emitDataMessage('skillInstallFailed', {
      error: i18n.t('settings.skillLibrary.localImportFailed'),
      source: 'local',
    });
    emitBridgeErrorToast(i18n.t('settings.toast.action.installLocalSkill'), error);
  }
}

async function saveSkillsConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentSkillsConfig(config);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.saveSkillConfig'),
    i18n.t('settings.toast.skillConfigSaved'),
  );
}

async function addCustomTool(tool: Record<string, unknown>): Promise<void> {
  const payload = await addAgentCustomTool(tool);
  emitDataMessage('customToolAdded', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.addCustomTool'),
    i18n.t('settings.toast.customToolAdded'),
  );
}

async function removeInstalledSkill(
  skillId: string,
  source: 'custom' | 'instruction',
): Promise<void> {
  const payload = await removeAgentInstalledSkill(skillId, source);
  const messageType = source === 'custom' ? 'customToolRemoved' : 'instructionSkillRemoved';
  emitDataMessage(messageType, payload);
  await dispatchSettingsBootstrap(true);
  const action = source === 'custom'
    ? i18n.t('settings.toast.action.deleteCustomTool')
    : i18n.t('settings.toast.action.deleteInstructionSkill');
  const detail = source === 'custom'
    ? i18n.t('settings.toast.customToolDeleted')
    : i18n.t('settings.toast.instructionSkillDeleted');
  emitBridgeSuccessToast(action, detail);
}

async function updateSkill(skillId: string): Promise<void> {
  const payload = await updateAgentSkill(skillId);
  emitDataMessage('skillUpdated', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.updateSkill'),
    i18n.t('settings.toast.skillUpdated'),
  );
}

async function updateAllSkills(): Promise<void> {
  const payload = await updateAllAgentSkills();
  emitDataMessage('allSkillsUpdated', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.updateAllSkills'),
    i18n.t('settings.toast.allSkillsUpdated'),
  );
}

async function clearProjectKnowledge(scope: BridgeRequestScope = currentSessionBindingOverride()): Promise<void> {
  await clearAgentProjectKnowledge(requireWorkspaceRequestScope(scope));
  await emitKnowledgePayload(scope);
  emitBridgeSuccessToast(
    i18n.t('settings.toast.action.clearProjectKnowledge'),
    i18n.t('settings.toast.projectKnowledgeCleared'),
  );
}

export function createWebClientBridge(): ClientBridge {
  // 初始化传输层（自动检测 VS Code / Web 环境，选择对应策略）
  initTransport();
  ensureWindowListener();
  hydrateCanonicalWorkspaceBinding();

  return {
    kind: 'web',
    postMessage(message: ClientBridgeMessage): void {
      switch (message.type) {
        case 'webviewReady':
        case 'getState':
        case 'requestState':
          void restoreBridgeState('request_state').catch((error) => {
            reportExpectedRecoveryFailure(i18n.t('bridge.action.syncMessages'), '[web-client-bridge] bootstrap 失败:', error);
            scheduleRecovery('request_state', error);
          });
          return;
        case 'loadSettingsBootstrap':
          void dispatchSettingsBootstrap(Boolean(message.force), 'core').catch((error) => {
            reportExpectedRecoveryFailure(
              i18n.t('settings.toast.action.loadSettingsData'),
              '[web-client-bridge] settings 配置加载失败:',
              error,
            );
          });
          return;
        case 'saveUserRules':
          if (message.data && typeof message.data === 'object') {
            void saveUserRules(message.data as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.saveUserRules'),
                '[web-client-bridge] 保存用户规则失败:',
                error,
              );
            });
          }
          return;
        case 'saveWorkerConfig':
          if (typeof message.worker === 'string' && message.config && typeof message.config === 'object') {
            void saveWorkerConfig(message.worker, message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.saveWorkerConfig'),
                '[web-client-bridge] 保存代理配置失败:',
                error,
              );
            });
          }
          return;
        case 'saveOrchestratorConfig':
          if (message.config && typeof message.config === 'object') {
            void saveOrchestratorConfig(message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.saveOrchestratorConfig'),
                '[web-client-bridge] 保存主模型配置失败:',
                error,
              );
            });
          }
          return;
        case 'saveAuxiliaryConfig':
          if (message.config && typeof message.config === 'object') {
            void saveAuxiliaryConfig(message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.saveAuxiliaryConfig'),
                '[web-client-bridge] 保存辅助模型配置失败:',
                error,
              );
            });
          }
          return;
        case 'saveSafeguardConfig':
          if (message.config && typeof message.config === 'object') {
            void saveSafeguardConfig(message.config as any).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.saveSafeguardConfig'),
                '[web-client-bridge] 保存安全防护配置失败:',
                error,
              );
            });
          }
          return;
        case 'testWorkerConnection':
          if (typeof message.worker === 'string' && message.config && typeof message.config === 'object') {
            void testWorkerConnection(message.worker, message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.testWorkerConnection'),
                '[web-client-bridge] 测试代理连接失败:',
                error,
              );
            });
          }
          return;
        case 'testOrchestratorConnection':
          if (message.config && typeof message.config === 'object') {
            void testOrchestratorConnection(message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.testOrchestratorConnection'),
                '[web-client-bridge] 测试主模型连接失败:',
                error,
              );
            });
          }
          return;
        case 'testAuxiliaryConnection':
          if (message.config && typeof message.config === 'object') {
            void testAuxiliaryConnection(message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.testAuxiliaryConnection'),
                '[web-client-bridge] 测试辅助模型连接失败:',
                error,
              );
            });
          }
          return;
        case 'fetchModelList':
          if (message.config && typeof message.config === 'object' && typeof message.target === 'string') {
            void fetchModelList(message.config as Record<string, unknown>, message.target).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.fetchModelList'),
                '[web-client-bridge] 获取模型列表失败:',
                error,
              );
            });
          }
          return;
        case 'addMCPServer':
          if (message.server && typeof message.server === 'object') {
            void addMcpServer(message.server as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.addMcpServer'),
                '[web-client-bridge] 添加 MCP 服务器失败:',
                error,
              );
            });
          }
          return;
        case 'updateMCPServer':
          if (typeof message.serverId === 'string' && message.updates && typeof message.updates === 'object') {
            void updateMcpServer(message.serverId, message.updates as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.updateMcpServer'),
                '[web-client-bridge] 更新 MCP 服务器失败:',
                error,
              );
            });
          }
          return;
        case 'deleteMCPServer':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void deleteMcpServer(message.serverId).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.deleteMcpServer'),
                '[web-client-bridge] 删除 MCP 服务器失败:',
                error,
              );
            });
          }
          return;
        case 'getMCPServerTools':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void getMcpServerTools(message.serverId).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.loadMcpToolList'),
                '[web-client-bridge] 获取 MCP 工具失败:',
                error,
              );
            });
          }
          return;
        case 'refreshMCPTools':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void refreshMcpTools(message.serverId).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.refreshMcpTools'),
                '[web-client-bridge] 刷新 MCP 工具失败:',
                error,
              );
            });
          }
          return;
        case 'addRepository':
          if (typeof message.url === 'string' && message.url.trim()) {
            void addRepository(message.url).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.addRepository'),
                '[web-client-bridge] 添加仓库失败:',
                error,
              );
            });
          }
          return;
        case 'updateRepository':
          if (typeof message.repositoryId === 'string' && message.updates && typeof message.updates === 'object') {
            void updateRepository(message.repositoryId, message.updates as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.updateRepository'),
                '[web-client-bridge] 更新仓库失败:',
                error,
              );
            });
          }
          return;
        case 'deleteRepository':
          if (typeof message.repositoryId === 'string' && message.repositoryId.trim()) {
            void deleteRepository(message.repositoryId).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.deleteRepository'),
                '[web-client-bridge] 删除仓库失败:',
                error,
              );
            });
          }
          return;
        case 'refreshRepository':
          if (typeof message.repositoryId === 'string' && message.repositoryId.trim()) {
            void refreshRepository(message.repositoryId).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.refreshRepository'),
                '[web-client-bridge] 刷新仓库失败:',
                error,
              );
            });
          }
          return;
        case 'loadSkillLibrary':
          void loadSkillLibrary().catch((error) => {
            logBridgeOperationFailure(
              i18n.t('settings.toast.action.loadSkillLibrary'),
              '[web-client-bridge] 加载技能库失败:',
              error,
            );
          });
          return;
        case 'installSkill':
          if (typeof message.skillId === 'string' && message.skillId.trim()) {
            void installSkill(message.skillId).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.installSkill'),
                '[web-client-bridge] 安装技能失败:',
                error,
              );
            });
          }
          return;
        case 'installLocalSkill':
          void installLocalSkill(typeof message.directoryPath === 'string' ? message.directoryPath : undefined).catch((error) => {
            logBridgeOperationFailure(
              i18n.t('settings.toast.action.installLocalSkill'),
              '[web-client-bridge] 安装本地技能失败:',
              error,
            );
          });
          return;
        case 'removeInstalledSkill': {
          const skillId = typeof message.skillId === 'string'
            ? message.skillId
            : '';
          const source = message.source === 'custom' || message.source === 'instruction'
            ? message.source
            : null;
          if (skillId.trim() && source) {
            const action = source === 'custom'
              ? i18n.t('settings.toast.action.deleteCustomTool')
              : i18n.t('settings.toast.action.deleteInstructionSkill');
            const logLabel = source === 'custom'
              ? '[web-client-bridge] 删除自定义工具失败:'
              : '[web-client-bridge] 删除 Skill 失败:';
            void removeInstalledSkill(skillId, source).catch((error) => {
              logBridgeOperationFailure(
                action,
                logLabel,
                error,
              );
            });
          }
          return;
        }
        case 'updateSkill':
          if (typeof message.skillId === 'string' && message.skillId.trim()) {
            void updateSkill(message.skillId).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.updateSkill'),
                '[web-client-bridge] 更新技能失败:',
                error,
              );
            });
          }
          return;
        case 'updateAllSkills':
          void updateAllSkills().catch((error) => {
            logBridgeOperationFailure(
              i18n.t('settings.toast.action.updateAllSkills'),
              '[web-client-bridge] 更新全部技能失败:',
              error,
            );
          });
          return;
        case 'navigateSession':
          void navigateSession(message).catch((error) => {
            emitDataMessage('sessionNavigationFailed', {
              requestId: trimBridgeString(message.requestId),
              target: message.target === 'session' ? 'session' : 'draft',
              workspaceId: trimBridgeString(message.workspaceId),
              workspacePath: trimBridgeString(message.workspacePath),
              sessionId: trimBridgeString(message.sessionId),
              message: normalizeErrorMessage(error) || i18n.t('web.workbenchActionFailed', {
                action: i18n.t('bridge.action.switchSession'),
              }),
            });
          });
          return;
        case 'loadNotifications':
          {
            const scope = resolveNotificationOperationScope(message);
            if (scope) {
              void loadNotifications(scope).catch((error) => {
                if (isSessionMissingError(error)) return;
                reportNotificationOperationFailure(i18n.t('bridge.action.loadNotifications'), '[web-client-bridge] 加载通知失败:', error);
              });
            }
          }
          return;
        case 'reportIncident':
          if (message.incident && typeof message.incident === 'object') {
            const incident = message.incident as Record<string, unknown>;
            const scope = resolveNotificationOperationScope(incident);
            if (scope) {
              void reportIncident(scope, incident).catch((error) => {
                if (isSessionMissingError(error)) return;
                reportNotificationOperationFailure(i18n.t('bridge.action.writeNotification'), '[web-client-bridge] 记录异常失败:', error, {
                  suppressToast: true,
                });
              });
            }
          }
          return;
        case 'markAllNotificationsRead':
          {
            const scope = resolveNotificationOperationScope(message);
            if (scope) {
              void markAllNotificationsRead(scope).catch((error) => {
                if (isSessionMissingError(error)) return;
                reportNotificationOperationFailure(i18n.t('bridge.action.markNotificationsRead'), '[web-client-bridge] 标记通知已读失败:', error);
              });
            }
          }
          return;
        case 'clearAllNotifications':
          {
            const scope = resolveNotificationOperationScope(message);
            if (scope) {
              void clearAllNotifications(scope).catch((error) => {
                if (isSessionMissingError(error)) return;
                reportNotificationOperationFailure(i18n.t('bridge.action.clearNotifications'), '[web-client-bridge] 清空通知失败:', error);
              });
            }
          }
          return;
        case 'removeNotification':
          if (typeof message.notificationId === 'string' && message.notificationId.trim()) {
            const scope = resolveNotificationOperationScope(message);
            if (scope) {
              void removeNotification(scope, message.notificationId).catch((error) => {
                if (isSessionMissingError(error)) return;
                reportNotificationOperationFailure(i18n.t('bridge.action.removeNotification'), '[web-client-bridge] 删除通知失败:', error);
              });
            }
          }
          return;
        case 'resolveNotification':
          if (typeof message.notificationId === 'string' && message.notificationId.trim()) {
            const scope = resolveNotificationOperationScope(message);
            if (scope) {
              void resolveNotification(scope, message.notificationId).catch((error) => {
                if (isSessionMissingError(error)) return;
                reportNotificationOperationFailure(i18n.t('bridge.action.removeNotification'), '[web-client-bridge] 解决通知失败:', error);
              });
            }
          }
          return;
        case 'executeTask':
          if (
            (typeof message.text === 'string' && message.text.trim())
            || (typeof message.skillName === 'string' && message.skillName.trim())
            || (Array.isArray(message.images) && message.images.length > 0)
            || (Array.isArray(message.contextReferences) && message.contextReferences.length > 0)
            || (Array.isArray(message.browserAnnotationRefs) && message.browserAnnotationRefs.length > 0)
            || (Array.isArray(message.browserNodeSelections) && message.browserNodeSelections.length > 0)
          ) {
            void executeTask({
              text: typeof message.text === 'string' ? message.text : null,
              workspaceId: typeof message.workspaceId === 'string' ? message.workspaceId : undefined,
              workspacePath: typeof message.workspacePath === 'string' ? message.workspacePath : undefined,
              sessionId: typeof message.sessionId === 'string' ? message.sessionId : undefined,
              requestId: typeof message.requestId === 'string' ? message.requestId : undefined,
              skillName: typeof message.skillName === 'string' ? message.skillName : null,
              goalMode: message.goalMode === true,
              accessProfile: message.accessProfile === 'read_only'
                || message.accessProfile === 'restricted'
                || message.accessProfile === 'full_access'
                ? message.accessProfile
                : null,
              orchestratorSessionConfig: message.orchestratorSessionConfig
                && typeof message.orchestratorSessionConfig === 'object'
                && !Array.isArray(message.orchestratorSessionConfig)
                ? message.orchestratorSessionConfig as Record<string, unknown>
                : null,
              followUpMode: message.followUpMode === 'queue' ? 'queue' : undefined,
              replaceTurnId: typeof message.replaceTurnId === 'string' ? message.replaceTurnId : null,
              images: Array.isArray(message.images)
                ? message.images as Array<{ name: string; dataUrl: string }>
                : [],
              contextReferences: Array.isArray(message.contextReferences)
                ? message.contextReferences as Array<{
                    kind: 'file' | 'directory';
                    path: string;
                    pathRef?: string;
                    name: string;
                  }>
                : [],
              browserAnnotationRefs: Array.isArray(message.browserAnnotationRefs)
                ? message.browserAnnotationRefs.filter((value): value is string => typeof value === 'string')
                : [],
              browserNodeSelections: Array.isArray(message.browserNodeSelections)
                ? message.browserNodeSelections as MessageBrowserNodeSelection[]
                : [],
            });
          }
          return;
        case 'removeQueuedMessage':
          if (typeof message.queuedMessageId === 'string') {
            void removeQueuedMessageFromServer(message.queuedMessageId);
          }
          return;
        case 'guideQueuedMessage':
          if (typeof message.queuedMessageId === 'string') {
            void guideQueuedMessageFromServer(message.queuedMessageId);
          }
          return;
        case 'interruptTask':
          void interruptTask();
          return;
        case 'continueTask':
          void continueSessionExecution();
          return;
        case 'renameSession':
          if (
            typeof message.sessionId === 'string' && message.sessionId.trim()
            && typeof message.name === 'string' && message.name.trim()
          ) {
            void renameSession(message.sessionId, message.name, requestScopeFromMessage(message)).catch((error) => {
              logBridgeOperationFailure(i18n.t('bridge.action.renameSession'), '[web-client-bridge] 重命名会话失败:', error);
            });
          }
          return;
        case 'closeSession':
          if (typeof message.sessionId === 'string' && message.sessionId.trim()) {
            void closeSession(message.sessionId, requestScopeFromMessage(message)).catch((error) => {
              logBridgeOperationFailure(i18n.t('bridge.action.closeSession'), '[web-client-bridge] 关闭会话失败:', error);
            });
          }
          return;
        case 'deleteSession':
          if (typeof message.sessionId === 'string' && message.sessionId.trim()) {
            void deleteSession(message.sessionId, requestScopeFromMessage(message)).catch((error) => {
              logBridgeOperationFailure(i18n.t('bridge.action.deleteSession'), '[web-client-bridge] 删除会话失败:', error);
            });
          }
          return;
        case 'updateSetting':
          if (
            typeof message.key === 'string'
            && (message.key === 'locale' || message.key === 'conversationDisplayMode')
          ) {
            void updateSetting(message.key, message.value).catch((error) => {
              logBridgeOperationFailure(i18n.t('bridge.action.updateSetting'), '[web-client-bridge] 更新设置失败:', error);
            });
          }
          return;
        case 'requestExecutionStats':
          void dispatchExecutionStats().catch((error) => {
            logBridgeOperationFailure(i18n.t('bridge.action.loadExecutionStats'), '[web-client-bridge] 执行统计加载失败:', error);
          });
          return;
        case 'resetExecutionStats':
          void resetExecutionStats().catch((error) => {
            logBridgeOperationFailure(i18n.t('bridge.action.resetExecutionStats'), '[web-client-bridge] 重置执行统计失败:', error);
          });
          return;
        case 'openLink':
          if (forwardToVsCodeHost(message)) {
            return;
          }
          if (typeof message.url === 'string' && message.url.trim()) {
            const fileTarget = normalizeFileReferenceTarget(message.url);
            if (fileTarget) {
              const scope = requestScopeFromMessage(message);
              if (dispatchFilePreviewEvent({ filepath: fileTarget, ...scope })) {
                return;
              }
              void openFilePreview(fileTarget, undefined, scope).catch((error) => {
                logBridgeOperationFailure(i18n.t('bridge.action.openFilePreview'), '[web-client-bridge] 打开文件预览失败:', error);
              });
              return;
            }
            void openExternalWebUrl(message.url).catch((error) => {
              logBridgeOperationFailure(i18n.t('bridge.action.openExternalLink'), '[web-client-bridge] 打开外部网页失败:', error);
            });
          }
          return;
        case 'openDiagramPanel':
          if (forwardToVsCodeHost(message)) {
            return;
          }
          {
            const source = typeof message.source === 'string' ? message.source : '';
            if (!source.trim()) {
              return;
            }
            openDiagramPreview(
              source,
              typeof message.title === 'string' ? message.title : undefined,
              typeof message.svgContent === 'string' ? message.svgContent : undefined,
            );
          }
          return;
        case 'openFile':
          if (forwardToVsCodeHost(message)) {
            return;
          }
          {
            const filePath = typeof message.filepath === 'string' && message.filepath.trim()
              ? message.filepath
              : (typeof message.filePath === 'string' ? message.filePath : '');
            if (filePath.trim()) {
              const previewContent = typeof message.previewContent === 'string'
                ? message.previewContent
                : undefined;
              void openFilePreview(filePath, previewContent, requestScopeFromMessage(message)).catch((error) => {
                logBridgeOperationFailure(i18n.t('bridge.action.openFilePreview'), '[web-client-bridge] 打开文件预览失败:', error);
              });
            }
          }
          return;
        case 'viewDiff':
          if (forwardToVsCodeHost(message)) {
            return;
          }
          if (typeof message.filePath === 'string' && message.filePath.trim()) {
            const diffContent = typeof message.diff === 'string' ? message.diff : undefined;
            void openDiffPreview(message.filePath, diffContent, requestScopeFromMessage(message)).catch((error) => {
              logBridgeOperationFailure(i18n.t('bridge.action.openDiffPreview'), '[web-client-bridge] 打开差异预览失败:', error);
            });
          }
          return;
        case 'approveChange':
          if (typeof message.filePath === 'string' && message.filePath.trim()) {
            const scope = requestScopeFromMessage(message);
            const filePath = message.filePath.trim();
            void runChangeMutationOnce(scope, async () => {
              await approveAgentChange(filePath, requireWorkspaceRequestScope(scope));
              await fetchBootstrap({ forceFresh: true });
              emitBridgeSuccessToast(i18n.t('bridge.action.approveChange'), i18n.t('toast.changeApproved'));
            }).catch((error) => {
              logBridgeOperationFailure(i18n.t('bridge.action.approveChange'), '[web-client-bridge] 批准变更失败:', error);
            });
          }
          return;
        case 'revertChange':
          if (typeof message.filePath === 'string' && message.filePath.trim()) {
            const scope = requestScopeFromMessage(message);
            const filePath = message.filePath.trim();
            void runChangeMutationOnce(scope, async () => {
              await revertAgentChange(filePath, requireWorkspaceRequestScope(scope));
              await fetchBootstrap({ forceFresh: true });
              emitBridgeSuccessToast(i18n.t('bridge.action.revertChange'), i18n.t('toast.changeReverted'));
            }).catch((error) => {
              logBridgeOperationFailure(i18n.t('bridge.action.revertChange'), '[web-client-bridge] 还原变更失败:', error);
            });
          }
          return;
        case 'approveAllChanges':
          {
            const scope = requestScopeFromMessage(message);
            void runChangeMutationOnce(scope, async () => {
              await approveAllAgentChanges(requireWorkspaceRequestScope(scope));
              await fetchBootstrap({ forceFresh: true });
              emitBridgeSuccessToast(i18n.t('bridge.action.approveAllChanges'), i18n.t('bridge.detail.allChangesApproved'));
            }).catch((error) => {
              logBridgeOperationFailure(i18n.t('bridge.action.approveAllChanges'), '[web-client-bridge] 批准全部变更失败:', error);
            });
          }
          return;
        case 'revertAllChanges':
          {
            const scope = requestScopeFromMessage(message);
            void runChangeMutationOnce(scope, async () => {
              await revertAllAgentChanges(requireWorkspaceRequestScope(scope));
              await fetchBootstrap({ forceFresh: true });
              emitBridgeSuccessToast(i18n.t('bridge.action.revertAllChanges'), i18n.t('bridge.detail.allChangesReverted'));
            }).catch((error) => {
              logBridgeOperationFailure(i18n.t('bridge.action.revertAllChanges'), '[web-client-bridge] 还原全部变更失败:', error);
            });
          }
          return;
        case 'revertExecutionGroup':
          if (typeof message.executionGroupId === 'string' && message.executionGroupId.trim()) {
            const scope = requestScopeFromMessage(message);
            const executionGroupId = message.executionGroupId.trim();
            void runChangeMutationOnce(scope, async () => {
              await revertAgentExecutionGroupChanges(
                executionGroupId,
                requireWorkspaceRequestScope(scope),
              );
              await fetchBootstrap({ forceFresh: true });
              emitBridgeSuccessToast(
                i18n.t('bridge.action.revertExecutionGroup'),
                i18n.t('bridge.detail.executionGroupReverted'),
              );
            }).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('bridge.action.revertExecutionGroup'),
                '[web-client-bridge] 还原执行分组变更失败:',
                error,
              );
            });
          }
          return;
        case 'getProjectKnowledge':
          void dispatchProjectKnowledge({
            ...requestScopeFromMessage(message, ''),
            knowledgeRequestId: trimBridgeString(message.knowledgeRequestId),
          }).catch((error) => {
            logKnowledgeOperationFailure(
              i18n.t('settings.toast.action.loadProjectKnowledge'),
              '[web-client-bridge] 项目知识加载失败:',
              error,
            );
          });
          return;
        case 'reindexProjectKnowledge': {
          const scope = {
            ...requestScopeFromMessage(message, ''),
            knowledgeRequestId: trimBridgeString(message.knowledgeRequestId),
          };
          void reindexAgentProjectKnowledge(scope)
            .then(() => dispatchProjectKnowledge(scope))
            .catch((error) => {
              logKnowledgeOperationFailure(
                i18n.t('settings.toast.action.loadProjectKnowledge'),
                '[web-client-bridge] 项目知识重建失败:',
                error,
              );
            });
          return;
        }
        case 'clearProjectKnowledge':
          void clearProjectKnowledge(requestScopeFromMessage(message, '')).catch((error) => {
            logKnowledgeOperationFailure(
              i18n.t('settings.toast.action.clearProjectKnowledge'),
              '[web-client-bridge] 清空项目知识失败:',
              error,
            );
          });
          return;
        case 'addKnowledgeItem': {
          const kind = typeof message.kind === 'string' ? message.kind : '';
          const content = typeof message.content === 'string' ? message.content : '';
          if ((kind === 'adr' || kind === 'faq' || kind === 'learning') && content) {
            const scope = requestScopeFromMessage(message, '');
            const payload: AgentKnowledgeItemPayload = {
              kind,
              content,
              title: typeof message.title === 'string' ? message.title : undefined,
              tags: Array.isArray(message.tags) ? (message.tags as string[]) : [],
              context: typeof message.context === 'string' ? message.context : undefined,
            };
            void addAgentKnowledgeItem(payload, scope).then(async () => {
              await emitKnowledgePayload(scope);
              emitBridgeSuccessToast(i18n.t('bridge.action.addKnowledgeItem'), i18n.t('bridge.detail.knowledgeItemAdded'));
            }).catch((error) => {
              logKnowledgeOperationFailure(i18n.t('bridge.action.addKnowledgeItem'), '[web-client-bridge] 添加知识条目失败:', error);
            });
          }
          return;
        }
        case 'updateKnowledgeItem': {
          const knowledgeId = typeof message.knowledgeId === 'string' ? message.knowledgeId.trim() : '';
          if (knowledgeId) {
            const scope = requestScopeFromMessage(message, '');
            const patch: AgentKnowledgeItemPatch = {
              title: typeof message.title === 'string' ? message.title : undefined,
              content: typeof message.content === 'string' ? message.content : undefined,
              tags: Array.isArray(message.tags) ? (message.tags as string[]) : undefined,
              context: typeof message.context === 'string' ? message.context : undefined,
            };
            void updateAgentKnowledgeItem(knowledgeId, patch, scope).then(async () => {
              await emitKnowledgePayload(scope);
              emitBridgeSuccessToast(i18n.t('bridge.action.updateKnowledgeItem'), i18n.t('bridge.detail.knowledgeItemUpdated'));
            }).catch((error) => {
              logKnowledgeOperationFailure(i18n.t('bridge.action.updateKnowledgeItem'), '[web-client-bridge] 更新知识条目失败:', error);
            });
          }
          return;
        }
        case 'deleteKnowledgeItem': {
          const knowledgeId = typeof message.knowledgeId === 'string' ? message.knowledgeId.trim() : '';
          if (knowledgeId) {
            const scope = requestScopeFromMessage(message, '');
            void deleteAgentKnowledgeItem(knowledgeId, scope).then(async () => {
              await emitKnowledgePayload(scope);
              emitBridgeSuccessToast(i18n.t('bridge.action.deleteKnowledgeItem'), i18n.t('bridge.detail.knowledgeItemDeleted'));
            }).catch((error) => {
              logKnowledgeOperationFailure(i18n.t('bridge.action.deleteKnowledgeItem'), '[web-client-bridge] 删除知识条目失败:', error);
            });
          }
          return;
        }
        case 'connectMCPServer':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void connectMcpServer(message.serverId).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.connectMcpServer'),
                '[web-client-bridge] 连接 MCP 服务器失败:',
                error,
              );
            });
          }
          return;
        case 'disconnectMCPServer':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void disconnectMcpServer(message.serverId).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.disconnectMcpServer'),
                '[web-client-bridge] 断开 MCP 服务器失败:',
                error,
              );
            });
          }
          return;
        case 'saveSkillsConfig':
          if (message.config && typeof message.config === 'object') {
            void saveSkillsConfig(message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.saveSkillConfig'),
                '[web-client-bridge] 保存技能配置失败:',
                error,
              );
            });
          }
          return;
        case 'addCustomTool':
          if (message.tool && typeof message.tool === 'object') {
            void addCustomTool(message.tool as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure(
                i18n.t('settings.toast.action.addCustomTool'),
                '[web-client-bridge] 添加自定义工具失败:',
                error,
              );
            });
          }
          return;
        case 'login':
        case 'logout':
          return;
        case 'uiError':
          console.error('[web-client-bridge] UI 错误上报:', {
            component: message.component,
            detail: message.detail,
            stack: message.stack,
          });
          return;
        case 'selectWorker':
          return;
        case 'toggleBuiltInTool':
          return;
        default:
          return;
      }
    },
    onMessage(listener: (message: ClientBridgeMessage) => void): () => void {
      listeners.add(listener);
      const pending = pendingBridgeMessages.splice(0);
      for (const message of pending) {
        try {
          listener(message);
        } catch (error) {
          console.error('[web-client-bridge] 补发启动消息失败:', error);
        }
      }
      return () => listeners.delete(listener);
    },
    getState<T>(): T | undefined {
      if (cachedWebviewState !== null) {
        return cachedWebviewState as T;
      }
      const stored = safeLocalStorageGetItem(WEBVIEW_STATE_STORAGE_KEY);
      if (!stored) {
        return undefined;
      }
      try {
        const parsed = JSON.parse(stored) as T;
        cachedWebviewState = parsed;
        return parsed;
      } catch (error) {
        warnStorageFailure('解析', WEBVIEW_STATE_STORAGE_KEY, error);
        safeLocalStorageRemoveItem(WEBVIEW_STATE_STORAGE_KEY);
        return undefined;
      }
    },
    setState<T>(state: T): void {
      cachedWebviewState = state;
      pendingWebviewState = state;
      schedulePersistedWebviewState();
    },
    getInitialSessionId(): string {
      return resolveWorkspaceQuery().sessionId ?? '';
    },
    getInitialLocale(): SupportedLocale {
      if (typeof window !== 'undefined') {
        const storedLocale = safeLocalStorageGetItem('magi-locale');
        if (storedLocale === 'zh-CN' || storedLocale === 'en-US') {
          return storedLocale;
        }
        const locale = (window as unknown as { __INITIAL_LOCALE__?: string }).__INITIAL_LOCALE__;
        if (locale === 'zh-CN' || locale === 'en-US') {
          return locale;
        }
      }
      return 'zh-CN';
    },
    notifyReady(): void {
      void restoreBridgeState('notify_ready').catch((error) => {
        reportExpectedRecoveryFailure(i18n.t('bridge.action.initializeApp'), '[web-client-bridge] Web 入口初始化失败:', error);
        scheduleRecovery('notify_ready', error);
      });
    },
  };
}
