import {
  AgentApiError,
  agentUrl,
  dispatchAgentConnectionEvent,
  getAgentSettingsBootstrap,
  loadAgentSessionSnapshot,
  probeReachableAgentBaseUrl,
  resolveAgentBaseUrl,
} from '../../web/agent-api';
import { getHostApi, getTransport, initTransport } from '../transport';
import {
  approveAgentChange,
  approveAllAgentChanges,
  addAgentKnowledgeItem,
  appendAgentNotification,
  addAgentCustomTool,
  addAgentMcpServer,
  addAgentRepository,
  clearAgentNotifications,
  clearAgentProjectKnowledge,
  clearAgentAllTasks,
  closeAgentSession,
  connectAgentMcpServer,
  deleteAgentTask,
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
  getAgentSessionNotifications,
  continueAgentSession,
  interruptAgentSession,
  interruptAgentTask,
  installAgentLocalSkill,
  installAgentSkill,
  listAgentWorkspaces,
  loadAgentSkillLibrary,
  markAllAgentNotificationsRead,
  refreshAgentMcpTools,
  refreshAgentRepository,
  removeAgentNotification,
  removeAgentInstalledSkill,
  renameAgentSession,
  resetAgentExecutionStats,
  saveAgentCurrentSession,
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
  startAgentTask,
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
import {
  persistStoredBrowserWorkspaceBinding,
  readStoredBrowserWorkspaceBinding,
} from './browser-workspace-binding';
import { buildEmptyWorkspaceAppState } from './empty-workspace-state';
import {
  normalizeRustBootstrapPayload,
  parseRustEventEnvelope,
  readRustTimelinePageMeta,
  type BootstrapPayload,
  type RustEventEnvelope,
} from './rust-daemon-contract';
import {
  CANONICAL_TURN_SCHEMA_VERSION,
  parseCanonicalTurnEventPayload,
  type CanonicalTurnEvent,
} from '../protocol/canonical-turn';
import type { SseConnection } from '../transport';
import {
  activateTaskProjectionSession,
  fetchTaskProjection,
  startAutoRefresh as startTaskAutoRefresh,
  getTaskProjectionState,
  clearTaskProjection,
} from '../../stores/task-projection-store.svelte';
import { sanitizeSvgContent } from '../svg-sanitizer';
import { RustDaemonClient } from '../rust-daemon-client';
import {
  dequeueQueuedMessage,
  enqueueQueuedMessage,
  messagesState,
  allocateTurnOrderSeq,
  addPendingRequest,
  clearRequestBinding,
  createRequestBinding,
  markMessageActive,
  setQueuedMessages,
  updateRequestBinding,
} from '../../stores/messages.svelte';
import { resolveModelListFetchBlockReason } from '../model-governance';
import type { QueuedMessage } from '../../types/message';

const listeners: Set<(message: ClientBridgeMessage) => void> = new Set();
let bridgeListenerRegistered = false;
let currentWorkspaceId = '';
let currentWorkspacePath = '';
let currentSessionId = '';
let currentInterruptTaskId = '';
let currentRuntimeEpoch = '';
let cachedSettingsBootstrap: SettingsBootstrapPayload | null = null;
let cachedSettingsBootstrapScope: 'none' | 'core' | 'full' = 'none';
const QUEUE_DRAIN_DELAY_MS = 120;
const QUEUE_DRAIN_BUSY_RETRY_MS = 1000;
let queueDrainTimer: ReturnType<typeof setTimeout> | null = null;
let queueDrainActive = false;
/** 传输层维护的 SSE 连接句柄（统一管理 Web EventSource 和宿主代理两种模式） */
let activeSseConnection: SseConnection | null = null;
let activeEventStreamKey = '';
let activeEventStreamState: 'idle' | 'connecting' | 'open' = 'idle';
let activeEventStreamOpenPromise: Promise<void> | null = null;
let activeEventStreamOpenTimeout: number | null = null;
let activeEventStreamToken = 0;
let activeEventStreamOpenResolve: (() => void) | null = null;
let activeEventStreamOpenReject: ((error: Error) => void) | null = null;
// SSE 空闲检测：后端每 15s 发 keep-alive 心跳，任何事件（包括心跳）都会刷新 lastEventStreamActivityAt。
// 超过 EVENT_STREAM_IDLE_TIMEOUT_MS 未收到任何事件即视为静默断流，触发 recovery 重拉 bootstrap，
// 让 applyAuthoritativeProcessingState 根据权威快照收敛运行态，避免前端永久卡在 running。
let lastEventStreamActivityAt = 0;
let eventStreamIdleCheckTimer: number | null = null;
let bridgeRecovering = false;
// fetchBootstrap 防重入：同一时刻只允许一个 bootstrap 请求在飞行中，
// 后续调用复用同一 Promise，避免重复 dispatchBootstrap 打乱 eventSeq 追踪。
let bootstrapInFlight: Promise<void> | null = null;
let settingsBootstrapInFlight: Promise<void> | null = null;
let recoveryAttempt = 0;
let recoveryTimer: number | null = null;
let recoveryInFlight: Promise<void> | null = null;
let sessionSnapshotGeneration = 0;
function clearActiveTurnInFlight(): void {
  // 实时 turn 投影由后端 canonical snapshot 驱动，这里保留统一清理入口。
}

const RECOVERY_BASE_DELAY_MS = 1000;
const RECOVERY_MAX_DELAY_MS = 10_000;
const EVENT_STREAM_PARSE_ERROR_DEBOUNCE_MS = 5000;
const EVENT_STREAM_OPEN_TIMEOUT_MS = 4000;
// 后端 SSE keep-alive interval 为 15s（见 crates/magi-api/src/sse.rs）。
// 给出 3 个心跳的容错窗口后再判定静默断流，避免偶发网络抖动产生误恢复。
const EVENT_STREAM_IDLE_TIMEOUT_MS = 45_000;
const EVENT_STREAM_IDLE_CHECK_INTERVAL_MS = 5_000;
const SESSION_TIMELINE_PAGE_SIZE = 50;
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
const storageWarningSignatures = new Set<string>();

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
  if (error instanceof Error) {
    return error.message;
  }
  if (typeof error === 'string' && error.trim()) {
    return error.trim();
  }
  return undefined;
}

function trimBridgeString(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
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

interface BootstrapTaskTrackingHints {
  rootTaskId: string;
  activeTaskIds: string[];
}

function isTerminalRuntimeTaskStatus(status: unknown): boolean {
  const normalized = trimBridgeString(status).toLowerCase();
  if (!normalized) {
    return false;
  }
  return normalized.includes('succeed')
    || normalized.includes('complete')
    || normalized.includes('fail')
    || normalized.includes('reject')
    || normalized.includes('abort')
    || normalized.includes('cancel')
    || normalized.includes('skip');
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

function extractBootstrapTaskTrackingHints(payload: BootstrapPayload, rawPayload: unknown): BootstrapTaskTrackingHints {
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
  const runtimeSessionStatus = trimBridgeString(activeRuntimeSession?.current_status)
    || trimBridgeString(activeRuntimeSession?.currentStatus);
  if (
    activeRuntimeSession
    && !rootTaskId
    && sessionTaskIds.length === 0
    && sessionMissionIds.size === 0
    && runtimeSessionStatus === 'detached'
  ) {
    return {
      rootTaskId: '',
      activeTaskIds: [],
    };
  }
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
    || detail.includes('local agent');
}

function emitForcedProcessingIdle(reason: string, extra?: Record<string, unknown>): void {
  clearCurrentInterruptTaskId();
  emitDataMessage('processingStateChanged', {
    isProcessing: false,
    transitionKind: 'forced',
    source: 'orchestrator',
    agent: 'orchestrator',
    reason,
    timestamp: Date.now(),
    ...(extra || {}),
  });
  scheduleQueuedTurnDrain('forced_idle');
}

function refreshBootstrapAfterTerminalTurn(reason: string): void {
  void fetchBootstrap({ forceFresh: true }).catch((error) => {
    reportExpectedRecoveryFailure('终态变更同步', '[web-client-bridge] turn 终态后 bootstrap 同步失败:', error);
    scheduleRecovery(reason, error, true);
  });
}

function emitRecoveringState(reason: string, error?: unknown): void {
  bridgeRecovering = true;
  dispatchAgentConnectionEvent({
    status: 'recovering',
    reason,
    error: normalizeErrorMessage(error),
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
    if (idleMs < EVENT_STREAM_IDLE_TIMEOUT_MS) {
      return;
    }
    // SSE 握手仍 open，但超过容错窗口没收到任何事件（含 keep-alive），判定静默断流。
    // 重置活跃时间戳避免 recovery 调度期间重复触发，由 recovery 完成后的 ensureEventStream
    // 重新建连或 closeEventStream 停止检测。
    markEventStreamActive();
    scheduleRecovery('event_stream_idle', new Error(`SSE 静默超时：${Math.round(idleMs / 1000)}s`), true);
  }, EVENT_STREAM_IDLE_CHECK_INTERVAL_MS);
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
    syncBindingFromBridgeMessage(message);
    emitMessage(message);
  });
  window.addEventListener('storage', (event) => {
    if (event.key !== 'magi-agent-base-url') {
      return;
    }
    closeEventStream();
    scheduleRecovery('agent_base_url_changed', undefined, true);
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
): { sessionId: string; workspaceId: string; workspacePath: string } {
  if (message.type !== 'unifiedMessage') {
    return { sessionId: '', workspaceId: '', workspacePath: '' };
  }
  const standard = message.message as StandardMessage | undefined;
  if (!standard || standard.category !== MessageCategory.DATA) {
    return { sessionId: '', workspaceId: '', workspacePath: '' };
  }
  if (standard.data?.dataType !== 'sessionBootstrapLoaded') {
    return { sessionId: '', workspaceId: '', workspacePath: '' };
  }
  const payload = standard.data?.payload;
  if (!payload || typeof payload !== 'object') {
    return { sessionId: '', workspaceId: '', workspacePath: '' };
  }
  const payloadRecord = payload as Record<string, unknown>;
  const workspaceRecord = payloadRecord.workspace && typeof payloadRecord.workspace === 'object'
    ? payloadRecord.workspace as Record<string, unknown>
    : undefined;
  return {
    sessionId: typeof payloadRecord.sessionId === 'string' ? payloadRecord.sessionId.trim() : '',
    workspaceId: typeof workspaceRecord?.workspaceId === 'string' ? workspaceRecord.workspaceId.trim() : '',
    workspacePath: typeof workspaceRecord?.rootPath === 'string' ? workspaceRecord.rootPath.trim() : '',
  };
}

function syncBindingFromBridgeMessage(message: ClientBridgeMessage): void {
  const binding = extractSessionBootstrapBinding(message);
  const nextSessionId = binding.sessionId;
  if (!nextSessionId || nextSessionId === currentSessionId) {
    return;
  }
  persistWorkspaceBinding(
    binding.workspaceId || currentWorkspaceId,
    binding.workspacePath || currentWorkspacePath,
    nextSessionId,
  );
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

function emitSessionTurnCanonicalEvent(canonicalEvent: CanonicalTurnEvent): void {
  emitDataMessage('sessionTurnCanonicalEventUpdated', {
    sessionId: canonicalEvent.sessionId,
    canonicalEvent,
  });
}

function emitLocalPendingCanonicalTurn(input: {
  sessionId: string;
  requestId: string;
  userMessageId: string;
  placeholderMessageId: string;
  text: string;
  turnSeq: number;
  createdAt: number;
}): boolean {
  const sessionId = input.sessionId.trim();
  if (!sessionId || !input.requestId || input.turnSeq <= 0) {
    return false;
  }
  const turnId = `turn-local-${input.requestId}`;
  const sourceThreadId = `thread-orchestrator-${sessionId}`;
  const sharedMetadata = {
    requestId: input.requestId,
    userMessageId: input.userMessageId,
    placeholderMessageId: input.placeholderMessageId,
    localOptimistic: true,
  };
  const assistantItem = {
    sessionId,
    turnId,
    turnSeq: input.turnSeq,
    itemId: input.placeholderMessageId,
    itemSeq: 2,
    kind: 'assistant_text' as const,
    createdAt: input.createdAt,
    status: 'running' as const,
    updatedAt: input.createdAt,
    title: '生成回复',
    content: '',
    sourceThreadId,
    visibility: {
      renderable: true,
    },
    metadata: sharedMetadata,
  };
  emitSessionTurnCanonicalEvent({
    schemaVersion: CANONICAL_TURN_SCHEMA_VERSION,
    eventId: `event-local-turn-started-${input.requestId}`,
    eventSeq: 0,
    kind: 'turn_started',
    sessionId,
    turnId,
    turnSeq: input.turnSeq,
    occurredAt: input.createdAt,
    turn: {
      sessionId,
      turnId,
      turnSeq: input.turnSeq,
      acceptedAt: input.createdAt,
      status: 'running',
      metadata: sharedMetadata,
      items: [
        {
          sessionId,
          turnId,
          turnSeq: input.turnSeq,
          itemId: input.userMessageId,
          itemSeq: 1,
          kind: 'user_message',
          createdAt: input.createdAt,
          status: 'completed',
          updatedAt: input.createdAt,
          content: input.text,
          sourceThreadId,
          visibility: {
            renderable: true,
          },
          metadata: sharedMetadata,
        },
        assistantItem,
      ],
    },
    item: assistantItem,
  });
  return true;
}

function emitLocalPendingCanonicalTurnFailed(input: {
  sessionId: string;
  requestId: string;
  userMessageId: string;
  placeholderMessageId: string;
  text: string;
  turnSeq: number;
  createdAt: number;
  failedAt: number;
  error: string;
}): boolean {
  const sessionId = input.sessionId.trim();
  if (!sessionId || !input.requestId || input.turnSeq <= 0) {
    return false;
  }
  const turnId = `turn-local-${input.requestId}`;
  const sourceThreadId = `thread-orchestrator-${sessionId}`;
  const sharedMetadata = {
    requestId: input.requestId,
    userMessageId: input.userMessageId,
    placeholderMessageId: input.placeholderMessageId,
    localTerminal: true,
  };
  const userItem = {
    sessionId,
    turnId,
    turnSeq: input.turnSeq,
    itemId: input.userMessageId,
    itemSeq: 1,
    kind: 'user_message' as const,
    createdAt: input.createdAt,
    status: 'completed' as const,
    updatedAt: input.createdAt,
    content: input.text,
    sourceThreadId,
    visibility: {
      renderable: true,
    },
    metadata: sharedMetadata,
  };
  const assistantItem = {
    sessionId,
    turnId,
    turnSeq: input.turnSeq,
    itemId: input.placeholderMessageId,
    itemSeq: 2,
    kind: 'assistant_text' as const,
    createdAt: input.createdAt,
    status: 'failed' as const,
    updatedAt: input.failedAt,
    title: '发送失败',
    content: input.error,
    sourceThreadId,
    visibility: {
      renderable: true,
    },
    metadata: sharedMetadata,
  };
  emitSessionTurnCanonicalEvent({
    schemaVersion: CANONICAL_TURN_SCHEMA_VERSION,
    eventId: `event-local-turn-failed-${input.requestId}`,
    eventSeq: 0,
    kind: 'turn_completed',
    sessionId,
    turnId,
    turnSeq: input.turnSeq,
    occurredAt: input.failedAt,
    turn: {
      sessionId,
      turnId,
      turnSeq: input.turnSeq,
      acceptedAt: input.createdAt,
      completedAt: input.failedAt,
      status: 'failed',
      responseDurationMs: Math.max(0, input.failedAt - input.createdAt),
      metadata: sharedMetadata,
      items: [userItem, assistantItem],
    },
    item: assistantItem,
  });
  return true;
}

function emitAcceptedCanonicalTurnFromResult(result: {
  eventId: string;
  acceptedAt: number;
  canonicalSchemaVersion?: string | null;
  canonicalEventKind?: string | null;
  canonicalTurn?: unknown;
  canonicalItem?: unknown;
}): void {
  if (!result.canonicalTurn && !result.canonicalItem) {
    return;
  }
  const canonicalEvent = parseCanonicalTurnEventPayload({
    canonical_schema_version: result.canonicalSchemaVersion || CANONICAL_TURN_SCHEMA_VERSION,
    canonical_event_kind: result.canonicalEventKind || 'turn_started',
    canonical_turn: result.canonicalTurn,
    canonical_item: result.canonicalItem,
  }, {
    eventId: result.eventId,
    eventSeq: 0,
    occurredAt: result.acceptedAt,
  });
  if (canonicalEvent) {
    emitSessionTurnCanonicalEvent(canonicalEvent);
  }
}

function handleSessionTurnItemEvent(event: RustEventEnvelope): boolean {
  const canonicalEvent = parseCanonicalTurnEventPayload(event.payload, {
    eventId: trimBridgeString(event.event_id),
    eventSeq: typeof event.sequence === 'number' && Number.isFinite(event.sequence)
      ? Math.floor(event.sequence)
      : 0,
    occurredAt: typeof event.occurred_at === 'number' && Number.isFinite(event.occurred_at)
      ? Math.floor(event.occurred_at)
      : Date.now(),
  });
  if (!canonicalEvent) {
    console.error('[web-client-bridge] session.turn.item 缺少 canonical payload，已拒绝旧 projection live 写入');
    return false;
  }
  emitSessionTurnCanonicalEvent(canonicalEvent);
  return true;
}

function shouldRefreshFromRustEvent(event: RustEventEnvelope): boolean {
  const eventWorkspaceId = trimBridgeString(event.workspace_id);
  if (eventWorkspaceId && currentWorkspaceId && eventWorkspaceId !== currentWorkspaceId) {
    return false;
  }
  const eventSessionId = trimBridgeString(event.session_id);
  if (eventSessionId && currentSessionId && eventSessionId !== currentSessionId) {
    return false;
  }
  return true;
}

const TURN_TERMINAL_EVENTS = new Set([
  'session.turn.completed',
  'session.turn.failed',
  'session.turn.interrupted',
]);

function handleRustEventStreamMessage(event: RustEventEnvelope): void {
  if (!shouldRefreshFromRustEvent(event)) {
    return;
  }
  const eventType = trimBridgeString(event.event_type);

  if (eventType === 'event.stream.lagged') {
    console.warn('[web-client-bridge] 事件流出现 lag，切换到 bootstrap recovery', {
      payload: event.payload ?? {},
      sequence: event.sequence,
    });
    closeEventStream();
    scheduleRecovery('event_stream_lagged', undefined, true);
    return;
  }

  if ((eventType === 'session.turn.accepted' || eventType === 'session.turn.task.accepted') && event.payload) {
    const acceptedSessionId = trimBridgeString(event.payload.session_id)
      || trimBridgeString(event.payload.sessionId)
      || trimBridgeString(event.session_id);
    const acceptedWorkspaceId = trimBridgeString(event.payload.workspace_id)
      || trimBridgeString(event.payload.workspaceId)
      || trimBridgeString(event.workspace_id)
      || currentWorkspaceId;
    if (acceptedSessionId && (!currentSessionId || currentSessionId === acceptedSessionId)) {
      persistWorkspaceBinding(acceptedWorkspaceId, currentWorkspacePath, acceptedSessionId);
      emitDataMessage('sessionTurnAccepted', {
        sessionId: acceptedSessionId,
        workspaceId: acceptedWorkspaceId,
        createdSession: event.payload.created_session ?? event.payload.createdSession ?? false,
        route: event.payload.route ?? (eventType === 'session.turn.task.accepted' ? 'task' : ''),
      });
    }
    const canonicalEvent = parseCanonicalTurnEventPayload(event.payload, {
      eventId: trimBridgeString(event.event_id),
      eventSeq: typeof event.sequence === 'number' && Number.isFinite(event.sequence)
        ? Math.floor(event.sequence)
        : 0,
      occurredAt: typeof event.occurred_at === 'number' && Number.isFinite(event.occurred_at)
        ? Math.floor(event.occurred_at)
        : Date.now(),
    });
    if (canonicalEvent) {
      emitSessionTurnCanonicalEvent(canonicalEvent);
    }
  }

  if (eventType === 'session.turn.item') {
    if (handleSessionTurnItemEvent(event)) {
      return;
    }
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
          initTaskTracking(acceptedSessionId, acceptedRootTaskId);
        }
      }
    }
  }

  if (TURN_TERMINAL_EVENTS.has(eventType)) {
    clearActiveTurnInFlight();
    const terminalReason = eventType === 'session.turn.failed'
      ? 'session_turn_failed'
      : eventType === 'session.turn.interrupted'
        ? 'session_turn_interrupted'
        : 'session_turn_completed';
    emitForcedProcessingIdle(
      terminalReason,
      { eventType },
    );
    refreshBootstrapAfterTerminalTurn(terminalReason);
  }

  if (eventType === 'session.title.updated') {
    void fetchBootstrap({ forceFresh: true }).catch((error) => {
      reportExpectedRecoveryFailure('会话标题刷新', '[web-client-bridge] 会话标题更新后刷新失败:', error);
      scheduleRecovery('session_title_updated_refresh', error, true);
    });
  }

  // Notify listeners about task-domain SSE events so lightweight stores
  // (e.g. task-projection-store) can react without waiting for a full bootstrap refresh.
  const isTaskProjectionRelevantEvent = eventType.startsWith('task.')
    || eventType.startsWith('mission.')
    || eventType.startsWith('assignment.');
  if (isTaskProjectionRelevantEvent) {
    emitMessage({ type: 'rustTaskEvent', eventType, payload: event.payload ?? {} } as ClientBridgeMessage);

    if (eventType === 'task.status.changed' && event.payload) {
      emitDataMessage('taskStatusChanged', {
        taskId: event.payload.task_id ?? event.payload.taskId ?? '',
        rootTaskId: event.payload.root_task_id ?? event.payload.rootTaskId ?? '',
        title: event.payload.title ?? '',
        newStatus: event.payload.new_status ?? event.payload.status ?? '',
        oldStatus: event.payload.old_status ?? '',
        kind: event.payload.kind ?? '',
      });
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

function emitBridgeErrorToast(action: string, error: unknown): void {
  const normalizedAction = action.trim() || '请求';
  const detail = normalizeErrorMessage(error);
  const content = detail ? `${normalizedAction}失败：${detail}` : `${normalizedAction}失败`;
  const now = Date.now();
  const message = createNotifyMessage(
    content,
    'error',
    `web-bridge:${normalizedAction}`,
    undefined,
    {
      title: '请求失败',
      displayMode: 'toast',
      category: 'incident',
      source: 'bridge-runtime',
      actionRequired: true,
      persistToCenter: true,
      countUnread: true,
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
  options: {
    displayMode?: 'toast' | 'notification_center';
  } = {},
): void {
  const normalizedAction = action.trim() || '请求';
  const content = detail?.trim() || `${normalizedAction}成功`;
  const now = Date.now();
  const message = createNotifyMessage(
    content,
    'success',
    `web-bridge-success:${normalizedAction}`,
    undefined,
    {
      title: '操作完成',
      displayMode: options.displayMode || 'toast',
      category: 'audit',
      source: 'bridge-runtime',
      actionRequired: false,
      persistToCenter: true,
      countUnread: false,
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
  options: {
    displayMode?: 'toast' | 'notification_center';
  } = {},
): void {
  const normalizedAction = action.trim() || '提示';
  const content = detail.trim() || normalizedAction;
  const now = Date.now();
  const message = createNotifyMessage(
    content,
    'info',
    `web-bridge-info:${normalizedAction}`,
    undefined,
    {
      title: '提示',
      displayMode: options.displayMode || 'toast',
      category: 'audit',
      source: 'bridge-runtime',
      actionRequired: false,
      persistToCenter: true,
      countUnread: false,
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

function handleEventStreamParseFailure(data: string, error: unknown): void {
  console.error('[web-client-bridge] 事件流消息解析失败:', {
    error,
    preview: data.slice(0, 240),
  });
  const now = Date.now();
  if (now - lastEventStreamParseErrorAt >= EVENT_STREAM_PARSE_ERROR_DEBOUNCE_MS) {
    lastEventStreamParseErrorAt = now;
    emitBridgeErrorToast('事件流解析', error);
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

function resolveInjectedWorkspaceBinding(): { workspaceId: string; workspacePath: string } {
  if (typeof window === 'undefined') {
    return { workspaceId: '', workspacePath: '' };
  }
  const bootstrapWindow = window as unknown as {
    __INITIAL_WORKSPACE_ID__?: string;
    __INITIAL_WORKSPACE_PATH__?: string;
  };
  return {
    workspaceId: bootstrapWindow.__INITIAL_WORKSPACE_ID__?.trim() || '',
    workspacePath: bootstrapWindow.__INITIAL_WORKSPACE_PATH__?.trim() || '',
  };
}

function resolveWorkspaceQuery(): { workspaceId: string; workspacePath: string; sessionId: string } {
  const currentUrl = getCurrentUrl();
  const injectedBinding = resolveInjectedWorkspaceBinding();
  const storedBinding = readStoredBrowserWorkspaceBinding();
  const queryWorkspaceId = currentUrl?.searchParams.get('workspaceId')?.trim() || '';
  const queryWorkspacePath = currentUrl?.searchParams.get('workspacePath')?.trim() || '';
  const querySessionId = currentUrl?.searchParams.get('sessionId')?.trim() || '';
  const workspaceId = queryWorkspaceId
    || currentWorkspaceId
    || injectedBinding.workspaceId
    || storedBinding.workspaceId
    || '';
  const workspacePath = queryWorkspacePath
    || currentWorkspacePath
    || injectedBinding.workspacePath
    || storedBinding.workspacePath
    || '';
  const sessionId = querySessionId;
  return { workspaceId, workspacePath, sessionId };
}

function hydrateCanonicalWorkspaceBinding(): void {
  const binding = resolveWorkspaceQuery();
  currentWorkspaceId = binding.workspaceId;
  currentWorkspacePath = binding.workspacePath;
  currentSessionId = binding.sessionId;
}

function persistWorkspaceBinding(workspaceId: string, workspacePath: string, sessionId: string): void {
  const normalizedWorkspaceId = workspaceId.trim();
  const normalizedWorkspacePath = workspacePath.trim();
  const incomingSessionId = sessionId.trim();

  currentWorkspaceId = normalizedWorkspaceId;
  currentWorkspacePath = normalizedWorkspacePath;
  currentSessionId = incomingSessionId;
  persistStoredBrowserWorkspaceBinding({
    workspaceId: normalizedWorkspaceId,
    workspacePath: normalizedWorkspacePath,
  });

  const currentUrl = getCurrentUrl();
  if (!currentUrl) {
    return;
  }
  const nextUrl = new URL(currentUrl.toString());
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
}

function clearWorkspaceSessionBinding(workspaceId: string, workspacePath: string): void {
  const normalizedWorkspaceId = workspaceId.trim();
  const normalizedWorkspacePath = workspacePath.trim();
  currentWorkspaceId = normalizedWorkspaceId;
  currentWorkspacePath = normalizedWorkspacePath;
  currentSessionId = '';
  clearCurrentInterruptTaskId();
  clearTaskProjection();
  if (queueDrainTimer) {
    clearTimeout(queueDrainTimer);
    queueDrainTimer = null;
  }
  queueDrainActive = false;
  persistStoredBrowserWorkspaceBinding({
    workspaceId: normalizedWorkspaceId,
    workspacePath: normalizedWorkspacePath,
  });

  const currentUrl = getCurrentUrl();
  if (!currentUrl) {
    return;
  }
  const nextUrl = new URL(currentUrl.toString());
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
}

function dispatchWorkspaceSessionCleared(workspaceId: string, workspacePath: string): void {
  closeEventStream();
  clearWorkspaceSessionBinding(workspaceId, workspacePath);
  emitDataMessage('workspaceSessionCleared', {
    workspaceId: workspaceId.trim(),
    workspacePath: workspacePath.trim(),
  });
}

function clearPersistedWorkspaceBinding(): void {
  currentWorkspaceId = '';
  currentWorkspacePath = '';
  currentSessionId = '';
  clearCurrentInterruptTaskId();
  clearTaskProjection();
  persistStoredBrowserWorkspaceBinding({
    workspaceId: '',
    workspacePath: '',
  });
  const currentUrl = getCurrentUrl();
  if (!currentUrl) {
    return;
  }
  const nextUrl = new URL(currentUrl.toString());
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
  clearEventStreamOpenTimeout();
  stopEventStreamIdleCheck();
  // SSE 断开后无法接收增量事件，结束活跃 turn 防护
  clearActiveTurnInFlight();
  if (activeSseConnection) {
    activeSseConnection.close();
    activeSseConnection = null;
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
  return normalizeRustBootstrapPayload(rawPayload, {
    workspaceId: options.workspaceId ?? currentWorkspaceId,
    workspacePath: options.workspacePath ?? currentWorkspacePath,
    sessionId: options.sessionId,
  });
}

async function restoreBridgeState(reason: string, force = false): Promise<void> {
  if (recoveryInFlight) {
    return recoveryInFlight;
  }
  recoveryInFlight = (async () => {
    const recovered = bridgeRecovering || recoveryAttempt > 0;
    const reachableBaseUrl = await probeReachableAgentBaseUrl();
    if (!reachableBaseUrl) {
      throw new Error('无法连接 Local Agent，正在等待恢复。');
    }
    if (force) {
      cachedSettingsBootstrap = null;
      cachedSettingsBootstrapScope = 'none';
    }
    await Promise.all([
      fetchBootstrap({ forceEventStreamReconnect: true }),
      dispatchSettingsBootstrap(force, 'core'),
    ]);
    clearRecoveryTimer();
    recoveryAttempt = 0;
    emitConnectedState(reason, recovered);
  })().finally(() => {
    recoveryInFlight = null;
  });
  return recoveryInFlight;
}

function scheduleRecovery(reason: string, error?: unknown, immediate = false): void {
  emitRecoveringState(reason, error);
  if (recoveryTimer !== null || recoveryInFlight) {
    return;
  }
  const delay = immediate
    ? 0
    : Math.min(RECOVERY_MAX_DELAY_MS, RECOVERY_BASE_DELAY_MS * (2 ** Math.min(recoveryAttempt, 3)));
  recoveryTimer = window.setTimeout(() => {
    recoveryTimer = null;
    void restoreBridgeState(reason, true).catch((recoveryError) => {
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
  const query = new URLSearchParams();
  if (currentWorkspaceId) {
    query.set('workspaceId', currentWorkspaceId);
  }
  if (currentWorkspacePath) {
    query.set('workspacePath', currentWorkspacePath);
  }
  const nextKey = query.toString();
  if (!nextKey) {
    closeEventStream();
    return;
  }
  if (!options.forceReconnect && activeSseConnection && activeEventStreamKey === nextKey) {
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
  activeSseConnection = getTransport().connectEventStream(
    agentUrl('/events', nextKey),
    {
      onOpen() {
        if (streamToken !== activeEventStreamToken) {
          return;
        }
        activeEventStreamState = 'open';
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
        clearEventStreamOpenTimeout();
        stopEventStreamIdleCheck();
        activeSseConnection = null;
        activeEventStreamKey = '';
        activeEventStreamState = 'idle';
        activeEventStreamOpenPromise = null;
        scheduleRecovery('event_stream_error');
      },
    },
  );
  if (options.waitUntilOpen) {
    await openPromise;
  }
}

async function dispatchBootstrap(
  payload: BootstrapPayload,
  options: { forceEventStreamReconnect?: boolean; rawPayload?: unknown } = {},
): Promise<void> {
  const previousSessionId = currentSessionId;
  const pageMeta = readRustTimelinePageMeta(options.rawPayload ?? payload);
  // 检测 runtimeEpoch 代际变化：后端重启后执行无刷新状态重建，不允许整页刷新打断用户会话。
  const incomingEpoch = payload.agent?.runtimeEpoch || '';
  if (incomingEpoch && currentRuntimeEpoch && incomingEpoch !== currentRuntimeEpoch) {
    console.warn('[web-client-bridge] runtimeEpoch 变化，后端已重启，执行无刷新状态重建', {
      previous: currentRuntimeEpoch,
      current: incomingEpoch,
    });
  }
  if (incomingEpoch) {
    currentRuntimeEpoch = incomingEpoch;
  }
  persistWorkspaceBinding(payload.workspace.workspaceId, payload.workspace.rootPath, payload.sessionId);
  activateTaskProjectionSession(payload.sessionId);
  const taskTrackingHints = extractBootstrapTaskTrackingHints(payload, options.rawPayload);
  if (previousSessionId && payload.sessionId && previousSessionId !== payload.sessionId) {
    clearCurrentInterruptTaskId();
  }
  reconcileCurrentInterruptTaskId(taskTrackingHints.activeTaskIds);
  if (!taskTrackingHints.rootTaskId && taskTrackingHints.activeTaskIds.length === 0) {
    clearTaskProjection(payload.sessionId);
  }
  emitDataMessage('sessionBootstrapLoaded', {
    ...payload,
    hasMoreBefore: pageMeta.hasMoreBefore,
    beforeCursor: pageMeta.beforeCursor,
  } as Record<string, unknown>);
  void ensureEventStream({
    forceReconnect: options.forceEventStreamReconnect === true,
    waitUntilOpen: false,
  }).catch((error) => {
    reportExpectedRecoveryFailure('事件流连接', '[web-client-bridge] bootstrap 后事件流连接失败:', error);
    scheduleRecovery('bootstrap_event_stream_connect', error, true);
  });
  // 并行加载 Registry agents（fire-and-forget，不阻断 bootstrap）
  dispatchRegistryAgents();

  if (taskTrackingHints.rootTaskId || taskTrackingHints.activeTaskIds.length > 0) {
    void autoConnectTaskTracking(payload.sessionId, taskTrackingHints.activeTaskIds, taskTrackingHints.rootTaskId).catch((error) => {
      console.warn('[web-client-bridge] Auto-connect task tracking on bootstrap failed (non-critical):', error);
    });
  }
  if ((payload.state as { isProcessing?: boolean } | undefined)?.isProcessing !== true) {
    scheduleQueuedTurnDrain('bootstrap_idle');
  }
}

async function fetchBootstrap(
  options: { forceEventStreamReconnect?: boolean; forceFresh?: boolean } = {},
): Promise<void> {
  // 防重入：如果已有 bootstrap 请求在飞行中，直接复用
  if (bootstrapInFlight && options.forceFresh !== true) {
    return bootstrapInFlight;
  }
  if (bootstrapInFlight && options.forceFresh === true) {
    try {
      await bootstrapInFlight;
    } catch {
      // 强制刷新场景需要忽略上一轮失败，继续拉取最新权威快照。
    }
  }
  const doFetch = async (): Promise<void> => {
    const { workspaceId, workspacePath, sessionId } = resolveWorkspaceQuery();
    const query = new URLSearchParams();
    if (workspaceId) {
      query.set('workspaceId', workspaceId);
    }
    if (workspacePath) {
      query.set('workspacePath', workspacePath);
    }
    if (sessionId) {
      query.set('sessionId', sessionId);
    }
    const response = await getTransport().request(agentUrl('/bootstrap', query.toString()));
    if (!response.ok) {
      if (response.status === 404) {
        const workspaces = await listAgentWorkspaces();
        if (workspaces.length === 0) {
          dispatchEmptyWorkspaceState();
          return;
        }
      }
      throw new Error(`bootstrap failed: ${response.status}`);
    }
    const rawPayload = await response.json();
    const payload = normalizeBootstrapResponse(rawPayload, {
      workspaceId,
      workspacePath,
      sessionId,
    });
    await dispatchBootstrap(payload, { ...options, rawPayload });
  };
  bootstrapInFlight = doFetch().finally(() => {
    bootstrapInFlight = null;
  });
  return bootstrapInFlight;
}

async function fetchSettingsBootstrap(
  force = false,
  scope: 'core' | 'full' = 'full',
): Promise<SettingsBootstrapPayload> {
  const cachedScopeSatisfiesRequest = cachedSettingsBootstrapScope === 'full'
    || cachedSettingsBootstrapScope === scope;
  if (!force && cachedSettingsBootstrap && cachedScopeSatisfiesRequest) {
    return cachedSettingsBootstrap;
  }
  cachedSettingsBootstrap = await getAgentSettingsBootstrap({ scope });
  cachedSettingsBootstrapScope = cachedSettingsBootstrap.bootstrapScope === 'core' ? 'core' : 'full';
  return cachedSettingsBootstrap;
}

async function dispatchSettingsBootstrap(
  force = false,
  scope: 'core' | 'full' = 'full',
): Promise<void> {
  if (!force && settingsBootstrapInFlight) {
    return settingsBootstrapInFlight;
  }
  const doDispatch = async (): Promise<void> => {
    const snapshot: SettingsBootstrapSnapshot = await fetchSettingsBootstrap(force, scope);
    emitDataMessage('settingsBootstrapLoaded', snapshot as unknown as Record<string, unknown>);
  };
  settingsBootstrapInFlight = doDispatch().finally(() => {
    settingsBootstrapInFlight = null;
  });
  return settingsBootstrapInFlight;
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

async function dispatchProjectKnowledge(): Promise<void> {
  const query = new URLSearchParams();
  const requestWorkspaceId = currentWorkspaceId;
  const requestWorkspacePath = currentWorkspacePath;
  if (requestWorkspaceId) {
    query.set('workspaceId', requestWorkspaceId);
  }
  if (requestWorkspacePath) {
    query.set('workspacePath', requestWorkspacePath);
  }
  const response = await getTransport().request(agentUrl('/api/knowledge', query.toString()));
  if (!response.ok) {
    throw new Error(`project knowledge failed: ${response.status}`);
  }
  const payload = await response.json() as Record<string, unknown>;
  emitDataMessage('projectKnowledgeLoaded', {
    ...payload,
    workspaceId: requestWorkspaceId,
    workspacePath: requestWorkspacePath,
  });
}

async function emitKnowledgePayload(): Promise<void> {
  await dispatchProjectKnowledge();
}

async function dispatchSessionSnapshot(
  rawPayload: unknown,
  options: {
    sessionId: string;
    workspaceId?: string;
    workspacePath?: string;
    forceEventStreamReconnect?: boolean;
  },
): Promise<void> {
  const previousSessionId = currentSessionId;
  const payload = normalizeBootstrapResponse(rawPayload, {
    sessionId: options.sessionId,
    workspaceId: options.workspaceId,
    workspacePath: options.workspacePath,
  });
  persistWorkspaceBinding(payload.workspace.workspaceId, payload.workspace.rootPath, payload.sessionId);
  activateTaskProjectionSession(payload.sessionId);
  const taskTrackingHints = extractBootstrapTaskTrackingHints(payload, rawPayload);
  if (previousSessionId && payload.sessionId && previousSessionId !== payload.sessionId) {
    clearCurrentInterruptTaskId();
  }
  reconcileCurrentInterruptTaskId(taskTrackingHints.activeTaskIds);
  if (!taskTrackingHints.rootTaskId && taskTrackingHints.activeTaskIds.length === 0) {
    clearTaskProjection(payload.sessionId);
  }
  emitDataMessage('sessionBootstrapLoaded', {
    ...payload,
    hasMoreBefore: false,
    beforeCursor: null,
  } as Record<string, unknown>);
  void ensureEventStream({
    forceReconnect: options.forceEventStreamReconnect === true,
    waitUntilOpen: false,
  }).catch((error) => {
    reportExpectedRecoveryFailure('事件流连接', '[web-client-bridge] 会话快照后事件流连接失败:', error);
    scheduleRecovery('session_snapshot_event_stream_connect', error, true);
  });
  if (taskTrackingHints.rootTaskId || taskTrackingHints.activeTaskIds.length > 0) {
    void autoConnectTaskTracking(payload.sessionId, taskTrackingHints.activeTaskIds, taskTrackingHints.rootTaskId).catch((error) => {
      console.warn('[web-client-bridge] Auto-connect task tracking on session snapshot failed (non-critical):', error);
    });
  }
  if ((payload.state as { isProcessing?: boolean } | undefined)?.isProcessing !== true) {
    scheduleQueuedTurnDrain('session_snapshot_idle');
  }
}

async function loadLatestSessionSnapshot(
  sessionId: string,
  options: { workspaceId?: string; workspacePath?: string } = {},
): Promise<void> {
  const requestGeneration = ++sessionSnapshotGeneration;
  const targetWorkspaceId = typeof options.workspaceId === 'string' && options.workspaceId.trim()
    ? options.workspaceId.trim()
    : currentWorkspaceId;
  const targetWorkspacePath = typeof options.workspacePath === 'string' && options.workspacePath.trim()
    ? options.workspacePath.trim()
    : currentWorkspacePath;
  const rawPayload = await loadAgentSessionSnapshot(sessionId, {
    limit: SESSION_TIMELINE_PAGE_SIZE,
    workspaceId: targetWorkspaceId,
  });
  if (requestGeneration !== sessionSnapshotGeneration) {
    return;
  }
  const forceEventStreamReconnect = targetWorkspaceId !== currentWorkspaceId
    || targetWorkspacePath !== currentWorkspacePath;
  await dispatchSessionSnapshot(rawPayload, {
    sessionId,
    workspaceId: targetWorkspaceId,
    workspacePath: targetWorkspacePath,
    forceEventStreamReconnect,
  });
}

async function switchSession(
  sessionId: string,
  options: { workspaceId?: string; workspacePath?: string } = {},
): Promise<void> {
  const targetWorkspaceId = typeof options.workspaceId === 'string' && options.workspaceId.trim()
    ? options.workspaceId.trim()
    : currentWorkspaceId;
  const targetWorkspacePath = typeof options.workspacePath === 'string' && options.workspacePath.trim()
    ? options.workspacePath.trim()
    : currentWorkspacePath;
  activateTaskProjectionSession(sessionId);
  const response = await getTransport().request(agentUrl('/api/session/switch'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      workspaceId: targetWorkspaceId,
      workspacePath: targetWorkspacePath,
      sessionId,
    }),
  });
  if (!response.ok) {
    throw new Error(`switch session failed: ${response.status}`);
  }
  await response.json();
  await loadLatestSessionSnapshot(sessionId, {
    workspaceId: targetWorkspaceId,
    workspacePath: targetWorkspacePath,
  });
}

async function deleteSession(sessionId: string): Promise<void> {
  const payload = await deleteAgentSession(sessionId);
  // 不传 sessionId 提示——它指向已被删除的会话，归一化器会用它去新列表 find()
  // 找不到然后把 currentSessionId 错误地清空。让 bootstrap 自带的 currentSession 当真值：
  // 删的是当前会话 → 后端 currentSession 为空，前端也清空；
  // 删的不是当前会话 → 后端 currentSession 仍是原会话，前端继续保持激活。
  await dispatchBootstrap(normalizeBootstrapResponse(payload), { rawPayload: payload });
  emitBridgeSuccessToast('删除会话', '会话已删除');
}

async function renameSession(sessionId: string, name: string): Promise<void> {
  const payload = await renameAgentSession(sessionId, name);
  await dispatchBootstrap(normalizeBootstrapResponse(payload, { sessionId }), { rawPayload: payload });
  emitBridgeSuccessToast('重命名会话', '会话名称已更新');
}

async function closeSession(sessionId: string): Promise<void> {
  const payload = await closeAgentSession(sessionId);
  // 同 deleteSession：关闭后该会话不再出现在列表里，hint 会让归一化器误清空 currentSessionId
  await dispatchBootstrap(normalizeBootstrapResponse(payload), { rawPayload: payload });
  emitBridgeSuccessToast('关闭会话', '会话已关闭');
}

async function saveCurrentSession(): Promise<void> {
  const payload = await saveAgentCurrentSession();
  await dispatchBootstrap(
    normalizeBootstrapResponse(payload, { sessionId: currentSessionId || '' }),
    { rawPayload: payload },
  );
  emitBridgeSuccessToast('保存会话', '当前会话已保存', { displayMode: 'notification_center' });
}

async function ensureFreshLiveBridge(reason: string): Promise<void> {
  hydrateCanonicalWorkspaceBinding();
  const hasWorkspaceBinding = Boolean(currentWorkspaceId || currentWorkspacePath);
  const hasBinding = Boolean(hasWorkspaceBinding || currentSessionId);
  if (!hasBinding) {
    await restoreBridgeState(reason, true);
    return;
  }
  if (hasWorkspaceBinding && !currentSessionId) {
    const query = new URLSearchParams();
    if (currentWorkspaceId) query.set('workspaceId', currentWorkspaceId);
    if (currentWorkspacePath) query.set('workspacePath', currentWorkspacePath);
    const expectedKey = query.toString();
    await ensureEventStream({
      forceReconnect: activeEventStreamKey !== expectedKey,
      waitUntilOpen: true,
    });
    return;
  }
  if (
    bridgeRecovering
    || recoveryInFlight
    || !activeSseConnection
    || activeEventStreamState !== 'open'
  ) {
    await restoreBridgeState(reason, true);
    return;
  }
  // 检查当前 SSE 连接的 key 是否与预期 workspace 绑定匹配。
  // 事件流按 workspace 订阅，session 展示由前端本地按 sessionId 分发，
  // 避免切换当前会话时重连 SSE 并抢占其他会话的实时输出。
  const query = new URLSearchParams();
  if (currentWorkspaceId) query.set('workspaceId', currentWorkspaceId);
  if (currentWorkspacePath) query.set('workspacePath', currentWorkspacePath);
  const expectedKey = query.toString();
  const needsReconnect = activeEventStreamKey !== expectedKey;
  await ensureEventStream({
    forceReconnect: needsReconnect,
    waitUntilOpen: true,
  });
}

// ─── Task tracking helpers ────────────────────────────────────────────

/**
 * Initialize task-projection-store tracking for a root task ID.
 * Fetches the initial projection and starts auto-refresh + SSE subscription.
 * Defensive: logs warnings on failure but never breaks the caller.
 */
function initTaskTracking(sessionId: string, rootTaskId: string): void {
  console.info('[web-client-bridge] Initializing task tracking for session/root task:', { sessionId, rootTaskId });
  activateTaskProjectionSession(sessionId);
  const currentState = getTaskProjectionState(sessionId);
  if (currentState.rootTaskId && currentState.rootTaskId !== rootTaskId) {
    clearTaskProjection(sessionId);
  }
  fetchTaskProjection(sessionId, rootTaskId)
    .then(() => {
      startTaskAutoRefresh();
    })
    .catch((error) => {
      console.warn('[web-client-bridge] Failed to initialize task tracking (non-critical):', error);
    });
}

/**
 * Auto-detect active root tasks from session runtime state and start tracking.
 * Called during bootstrap dispatch to reconnect task tracking on session load/switch.
 * Uses active_task_ids from the bootstrap runtime read model to resolve root tasks.
 */
export async function autoConnectTaskTracking(
  sessionId: string,
  activeTaskIds: string[],
  preferredRootTaskId = '',
): Promise<void> {
  if (!sessionId || sessionId !== currentSessionId) {
    return;
  }
  const currentState = getTaskProjectionState(sessionId);
  if (preferredRootTaskId) {
    if (currentState.rootTaskId === preferredRootTaskId) {
      return;
    }
    console.info('[web-client-bridge] Auto-connecting task tracking from bootstrap root task:', preferredRootTaskId);
    initTaskTracking(sessionId, preferredRootTaskId);
    return;
  }

  if (currentState.rootTaskId) {
    return;
  }

  if (!activeTaskIds || activeTaskIds.length === 0) {
    return;
  }

  try {
    const client = new RustDaemonClient(resolveAgentBaseUrl());
    const inspectedRootTaskIds = new Set<string>();
    for (const taskId of activeTaskIds) {
      let task;
      try {
        task = await client.getTask(taskId, sessionId);
      } catch {
        continue;
      }
      if (sessionId !== currentSessionId) {
        return;
      }
      const rootTaskId = typeof task.root_task_id === 'string' && task.root_task_id.trim()
        ? task.root_task_id.trim()
        : task.task_id;
      if (!rootTaskId || inspectedRootTaskIds.has(rootTaskId)) {
        continue;
      }
      inspectedRootTaskIds.add(rootTaskId);
      if (currentState.rootTaskId === rootTaskId) {
        return;
      }
      console.info('[web-client-bridge] Auto-connecting task tracking via active task:', {
        sessionId,
        taskId,
        rootTaskId,
      });
      initTaskTracking(sessionId, rootTaskId);
      return;
    }
  } catch (error) {
    console.warn('[web-client-bridge] Auto-connect task tracking failed (non-critical):', error);
  }
}

interface ExecuteTaskInput {
  text?: string | null;
  requestId?: string;
  skillName?: string | null;
  accessProfile?: 'read_only' | 'restricted' | 'full_access' | null;
  followUpMode?: 'queue';
  images: Array<{
    name: string;
    dataUrl: string;
  }>;
}

function bridgeRuntimeIsBusy(): boolean {
  return Boolean(
    messagesState.isProcessing
      || messagesState.backendProcessing
      || messagesState.pendingRequests.size > 0
      || messagesState.activeMessageIds.size > 0,
  );
}

function enqueueFollowUpTurn(input: ExecuteTaskInput, normalizedText: string): void {
  const queued: QueuedMessage = {
    id: input.requestId || generateMessageId(),
    requestId: input.requestId,
    content: normalizedText || input.skillName || '后续消息',
    text: input.text ?? null,
    createdAt: Date.now(),
    skillName: input.skillName ?? null,
    accessProfile: input.accessProfile ?? null,
    images: input.images,
  };
  enqueueQueuedMessage(queued);
  scheduleQueuedTurnDrain('enqueue_follow_up', QUEUE_DRAIN_BUSY_RETRY_MS);
}

function scheduleQueuedTurnDrain(reason: string, delayMs = QUEUE_DRAIN_DELAY_MS): void {
  if (queueDrainTimer) {
    clearTimeout(queueDrainTimer);
  }
  queueDrainTimer = setTimeout(() => {
    queueDrainTimer = null;
    void drainQueuedTurns(reason);
  }, delayMs);
}

async function drainQueuedTurns(reason: string): Promise<void> {
  if (queueDrainActive) {
    return;
  }
  if (bridgeRuntimeIsBusy()) {
    if (messagesState.queuedMessages.length > 0) {
      scheduleQueuedTurnDrain(`${reason}:busy_retry`, QUEUE_DRAIN_BUSY_RETRY_MS);
    }
    return;
  }
  const next = dequeueQueuedMessage();
  if (!next) {
    return;
  }
  queueDrainActive = true;
  let shouldScheduleNextDrain = true;
  try {
    const submitted = await executeTask({
      text: next.text ?? next.content,
      requestId: next.requestId || next.id,
      skillName: next.skillName ?? null,
      accessProfile: next.accessProfile ?? null,
      images: next.images ?? [],
    });
    if (!submitted) {
      restoreQueuedTurnToFront(next);
      shouldScheduleNextDrain = false;
    }
  } finally {
    queueDrainActive = false;
    if (shouldScheduleNextDrain && messagesState.queuedMessages.length > 0) {
      scheduleQueuedTurnDrain('after_queued_turn_submit', QUEUE_DRAIN_BUSY_RETRY_MS);
    }
  }
}

function restoreQueuedTurnToFront(queued: QueuedMessage): void {
  const exists = messagesState.queuedMessages.some((message) => (
    message.id === queued.id || (queued.requestId && message.requestId === queued.requestId)
  ));
  if (exists) {
    return;
  }
  setQueuedMessages([queued, ...messagesState.queuedMessages]);
}

async function executeTask(input: ExecuteTaskInput): Promise<boolean> {
  const text = typeof input.text === 'string' ? input.text : null;
  const normalizedText = text?.trim() || '';
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
  if (!normalizedText && !skillName && images.length === 0) {
    return false;
  }
  if (input.followUpMode === 'queue' && bridgeRuntimeIsBusy() && !queueDrainActive) {
    enqueueFollowUpTurn(
      { ...input, skillName, images },
      normalizedText,
    );
    return true;
  }

  const requestId = input.requestId || generateMessageId();
  const isQueuedDrainSubmission = queueDrainActive && Boolean(input.requestId);
  const userMessageId = isQueuedDrainSubmission
    ? `queued-user-${requestId}`
    : generateMessageId();
  const placeholderMessageId = `assistant-placeholder-${requestId}`;
  const turnOrderSeq = allocateTurnOrderSeq();
  const requestCreatedAt = Date.now();

  createRequestBinding({
    requestId,
    userMessageId,
    placeholderMessageId,
    turnOrderSeq,
    createdAt: requestCreatedAt,
  });

  emitDataMessage('processingStateChanged', {
    isProcessing: true,
    source: 'orchestrator',
    agent: 'orchestrator',
    startedAt: requestCreatedAt,
    pendingRequestIds: [requestId],
  });
  if (!isQueuedDrainSubmission) {
    addPendingRequest(requestId);
    markMessageActive(placeholderMessageId);
    emitLocalPendingCanonicalTurn({
      sessionId: currentSessionId,
      requestId,
      userMessageId,
      placeholderMessageId,
      text: normalizedText,
      turnSeq: turnOrderSeq,
      createdAt: requestCreatedAt,
    });
  }

  try {
    await ensureFreshLiveBridge('execute_task_preflight');
    const turnResult = await submitSessionTurn({
      text,
      skillName,
      images,
      accessProfile: input.accessProfile ?? null,
      requestId,
      userMessageId,
      placeholderMessageId,
    }, {
      workspaceId: currentWorkspaceId,
      workspacePath: currentWorkspacePath,
      sessionId: currentSessionId,
    });

    emitAcceptedCanonicalTurnFromResult(turnResult);

    const canonicalUserMessageId = turnResult.userMessageItemId || userMessageId;
    const canonicalTurnSeq = typeof turnResult.acceptedAt === 'number' && Number.isFinite(turnResult.acceptedAt)
      ? Math.max(1, Math.floor(turnResult.acceptedAt))
      : undefined;
    const resolvedSessionId = typeof turnResult.sessionId === 'string' && turnResult.sessionId.trim()
      ? turnResult.sessionId.trim()
      : currentSessionId;
    updateRequestBinding(requestId, {
      userMessageId: canonicalUserMessageId,
      placeholderMessageId,
      ...(typeof canonicalTurnSeq === 'number' ? { turnSeq: canonicalTurnSeq } : {}),
    });
    if (isQueuedDrainSubmission) {
      addPendingRequest(requestId);
      markMessageActive(placeholderMessageId);
    }
    if (resolvedSessionId) {
      persistWorkspaceBinding(currentWorkspaceId, currentWorkspacePath, resolvedSessionId);
    }
    if (turnResult.createdSession && resolvedSessionId) {
      void fetchBootstrap({ forceFresh: true }).catch((error) => {
        reportExpectedRecoveryFailure('新会话列表同步', '[web-client-bridge] 新会话 accepted 后刷新失败:', error);
        scheduleRecovery('new_session_accepted_refresh', error, true);
      });
    }
    const successMessage = turnResult.route === 'task'
      ? '任务已提交'
      : turnResult.route === 'continue'
        ? '继续请求已提交'
        : '消息已发送';
    emitBridgeSuccessToast('发送消息', successMessage, { displayMode: 'notification_center' });

    setCurrentInterruptTaskId(turnResult.actionTaskId || '');
    const rootTaskId = turnResult.rootTaskId;
    if (rootTaskId && resolvedSessionId) {
      initTaskTracking(resolvedSessionId, rootTaskId);
    }

    // 确保 SSE 连接存活以接收增量事件
    void ensureEventStream({ forceReconnect: false, waitUntilOpen: false }).catch((err) => {
      console.warn('[web-client-bridge] executeTask 后 SSE 连接确认失败:', err);
    });
    return true;
  } catch (error) {
    clearActiveTurnInFlight();
    clearCurrentInterruptTaskId();
    console.error('[web-client-bridge] 执行任务失败:', error);
    const errorText = normalizeErrorMessage(error) || '发送消息失败';
    if (!isQueuedDrainSubmission) {
      emitLocalPendingCanonicalTurnFailed({
        sessionId: currentSessionId,
        requestId,
        userMessageId,
        placeholderMessageId,
        text: normalizedText,
        turnSeq: turnOrderSeq,
        createdAt: requestCreatedAt,
        failedAt: Date.now(),
        error: errorText,
      });
    }
    clearRequestBinding(requestId);
    emitBridgeErrorToast('发送消息', error);
    emitForcedProcessingIdle('execute_task_failed', {
      error: normalizeErrorMessage(error),
      requestId,
    });
    if (shouldRecoverFromBridgeError(error)) {
      closeEventStream();
      scheduleRecovery('execute_task_failed', error, true);
    }
    return false;
  }
}

async function interruptTask(): Promise<void> {
  const trigger = 'user_interrupt';
  const taskId = currentInterruptTaskId;
  const sessionId = currentSessionId.trim();
  clearActiveTurnInFlight();
  if (!taskId && !sessionId) {
    emitForcedProcessingIdle('user_interrupt_missing_session', { trigger });
    emitBridgeErrorToast('停止任务', new Error('当前没有可停止的会话。'));
    return;
  }
  const idleReason = taskId ? 'user_interrupt_requested' : 'user_session_interrupt_requested';
  // 中断请求已进入后端权威链路，前端先收敛到 idle，避免停止按钮卡死。
  emitForcedProcessingIdle(idleReason, { trigger, taskId, sessionId });
  try {
    if (taskId) {
      await interruptAgentTask({ taskId });
    } else {
      await interruptAgentSession(sessionId);
    }
  } catch (error) {
    console.error('[web-client-bridge] 中断执行失败（已执行前端强制停止）:', error);
    emitBridgeErrorToast('停止任务', error);
    emitForcedProcessingIdle('user_interrupt_failed', {
      trigger,
      taskId,
      sessionId,
      error: normalizeErrorMessage(error),
    });
  }
}

async function clearAllTasks(): Promise<void> {
  await clearAgentAllTasks();
}

async function startTask(taskId: string): Promise<void> {
  try {
    await ensureFreshLiveBridge('start_task_preflight');
    await startAgentTask(taskId);
  } catch (error) {
    console.error('[web-client-bridge] 启动任务失败:', error);
    emitBridgeErrorToast('启动任务', error);
    emitForcedProcessingIdle('start_task_failed', {
      error: normalizeErrorMessage(error),
      taskId,
    });
    if (shouldRecoverFromBridgeError(error)) {
      closeEventStream();
      scheduleRecovery('start_task_failed', error, true);
    }
  }
}

async function continueSessionExecution(): Promise<void> {
  if (!currentSessionId) {
    emitBridgeErrorToast('继续会话', new Error('当前没有可继续的会话。'));
    return;
  }
  try {
    await ensureFreshLiveBridge('continue_session_preflight');
    await continueAgentSession(currentSessionId);
  } catch (error) {
    console.error('[web-client-bridge] 继续会话失败:', error);
    emitBridgeErrorToast('继续会话', error);
    emitForcedProcessingIdle('continue_session_failed', {
      error: normalizeErrorMessage(error),
      sessionId: currentSessionId,
    });
    if (shouldRecoverFromBridgeError(error)) {
      closeEventStream();
      scheduleRecovery('continue_session_failed', error, true);
    }
  }
}

async function deleteTask(taskId: string): Promise<void> {
  await deleteAgentTask(taskId);
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
    throw new Error('浏览器阻止了预览窗口，请允许当前站点打开新窗口。');
  }
  const escapedTitle = escapePreviewHtml(title);
  const escapedSubtitle = escapePreviewHtml(subtitle);
  const escapedContent = escapePreviewHtml(content);
  const bodyClass = mode === 'diff' ? 'diff' : 'file';
  popup.document.write(`<!doctype html>
<html lang="zh-CN">
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
    throw new Error('浏览器阻止了预览窗口，请允许当前站点打开新窗口。');
  }
  const escapedTitle = escapePreviewHtml(title);
  popup.document.write(`<!doctype html>
<html lang="zh-CN">
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
  const resolvedTitle = title?.trim() || '图表';
  const sanitizedSvg = typeof svgContent === 'string' ? sanitizeSvgContent(svgContent) : '';
  if (sanitizedSvg) {
    openDiagramSvgPreview(resolvedTitle, sanitizedSvg);
    return;
  }
  openPreviewWindow(resolvedTitle, '图表源码预览', source, 'file');
}

async function openFilePreview(filePath: string, previewContent?: string): Promise<void> {
  if (typeof previewContent === 'string') {
    openPreviewWindow(filePath, '文件预览', previewContent, 'file');
    return;
  }
  const payload = await getAgentFilePreview(filePath);
  openPreviewWindow(payload.filePath, '文件预览', payload.content || '', 'file');
}

async function openDiffPreview(filePath: string, diffContent?: string): Promise<void> {
  if (typeof diffContent === 'string') {
    openPreviewWindow(filePath, '差异预览', diffContent, 'diff');
    return;
  }
  const payload = await getAgentChangeDiff(filePath);
  openPreviewWindow(payload.filePath, '差异预览', payload.diff || '', 'diff');
}

async function updateSetting(key: string, value: unknown): Promise<void> {
  const payload = await updateAgentRuntimeSetting(key, value);
  if (cachedSettingsBootstrap) {
    cachedSettingsBootstrap = {
      ...cachedSettingsBootstrap,
      runtimeSettings: {
        locale: payload.locale,
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

type NotificationCenterOperation = 'load' | 'append' | 'mark-read' | 'clear' | 'remove';

interface NotificationOperationScope {
  sessionId: string;
  workspaceId: string;
  workspacePath: string;
}

async function resetExecutionStats(): Promise<void> {
  await resetAgentExecutionStats();
  await dispatchExecutionStats();
}

function resolveNotificationOperationScope(message: ClientBridgeMessage): NotificationOperationScope | null {
  const sessionId = trimBridgeString(message.sessionId);
  if (!sessionId) {
    return null;
  }
  return {
    sessionId,
    workspaceId: trimBridgeString(message.workspaceId),
    workspacePath: trimBridgeString(message.workspacePath),
  };
}

function emitSessionNotificationsStatus(
  operation: NotificationCenterOperation,
  scope: NotificationOperationScope,
  isLoading: boolean,
  error?: unknown,
): void {
  emitDataMessage('sessionNotificationsStatus', {
    sessionId: scope.sessionId,
    workspaceId: scope.workspaceId,
    workspacePath: scope.workspacePath,
    operation,
    isLoading,
    error: error === undefined ? null : normalizeErrorMessage(error),
    updatedAt: Date.now(),
  });
}

async function runNotificationOperation(
  operation: NotificationCenterOperation,
  scope: NotificationOperationScope,
  task: (scope: NotificationOperationScope) => Promise<Record<string, unknown>>,
): Promise<void> {
  emitSessionNotificationsStatus(operation, scope, true);
  try {
    const payload = await task(scope);
    emitDataMessage('sessionNotificationsLoaded', payload);
    emitSessionNotificationsStatus(operation, scope, false);
  } catch (error) {
    emitSessionNotificationsStatus(operation, scope, false, error);
    throw error;
  }
}

async function loadSessionNotifications(scope: NotificationOperationScope): Promise<void> {
  await runNotificationOperation('load', scope, async (operationScope) => (
    await getAgentSessionNotifications(operationScope) as unknown as Record<string, unknown>
  ));
}

async function appendSessionNotification(
  scope: NotificationOperationScope,
  notification: Record<string, unknown>,
): Promise<void> {
  await runNotificationOperation('append', scope, async (operationScope) => (
    await appendAgentNotification(notification, operationScope) as unknown as Record<string, unknown>
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

async function saveWorkerConfig(worker: string, config: Record<string, unknown>): Promise<void> {
  await saveAgentWorkerConfig(worker, config);
  cachedSettingsBootstrap = null;
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('保存代理配置', `代理 ${worker} 配置已保存`, { displayMode: 'notification_center' });
}

async function saveUserRules(data: Record<string, unknown>): Promise<void> {
  await saveAgentUserRules(data);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('保存用户规则', '用户规则已保存', { displayMode: 'notification_center' });
}

async function saveOrchestratorConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentOrchestratorConfig(config);
  cachedSettingsBootstrap = null;
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('保存主模型配置', '主模型配置已保存', { displayMode: 'notification_center' });
}

async function saveAuxiliaryConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentAuxiliaryConfig(config);
  cachedSettingsBootstrap = null;
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('保存辅助模型配置', '辅助模型配置已保存', { displayMode: 'notification_center' });
}

async function saveSafeguardConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentSafeguardConfig(config);
  cachedSettingsBootstrap = null;
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('保存安全防护配置', '安全防护配置已保存', { displayMode: 'notification_center' });
}

async function testWorkerConnection(worker: string, config: Record<string, unknown>): Promise<void> {
  const payload = await testAgentWorkerConnection(worker, config);
  emitDataMessage('workerConnectionTestResult', payload);
  emitBridgeSuccessToast('测试代理连接', `代理 ${worker} 连接测试已完成`, { displayMode: 'notification_center' });
}

async function testOrchestratorConnection(config: Record<string, unknown>): Promise<void> {
  const payload = await testAgentOrchestratorConnection(config);
  emitDataMessage('orchestratorConnectionTestResult', payload);
  emitBridgeSuccessToast('测试主模型连接', '主模型连接测试已完成', { displayMode: 'notification_center' });
}

async function testAuxiliaryConnection(config: Record<string, unknown>): Promise<void> {
  const payload = await testAgentAuxiliaryConnection(config);
  emitDataMessage('auxiliaryConnectionTestResult', payload);
  emitBridgeSuccessToast('测试辅助模型连接', '辅助模型连接测试已完成', { displayMode: 'notification_center' });
}

async function fetchModelList(config: Record<string, unknown>, target: string): Promise<void> {
  const blockReason = resolveModelListFetchBlockReason(config);
  if (blockReason) {
    emitBridgeInfoToast(
      '获取模型列表',
      blockReason === 'full_url_mode'
        ? '完整路径模式下不支持自动获取模型列表，请手动填写模型名'
        : '请先填写 Base URL 和 API Key',
    );
    return;
  }
  const payload = await fetchAgentModelList(config, target);
  emitDataMessage('modelListFetched', { ...payload });
  emitBridgeSuccessToast('获取模型列表', `${target} 模型列表已刷新`, { displayMode: 'notification_center' });
}

async function addMcpServer(server: Record<string, unknown>): Promise<void> {
  const payload = await addAgentMcpServer(server);
  emitDataMessage('mcpServerAdded', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('添加 MCP 服务器', 'MCP 服务器已添加');
}

async function updateMcpServer(serverId: string, updates: Record<string, unknown>): Promise<void> {
  const payload = await updateAgentMcpServer(serverId, updates);
  emitDataMessage('mcpServerUpdated', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('更新 MCP 服务器', 'MCP 服务器已更新');
}

async function deleteMcpServer(serverId: string): Promise<void> {
  const payload = await deleteAgentMcpServer(serverId);
  emitDataMessage('mcpServerDeleted', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('删除 MCP 服务器', 'MCP 服务器已删除');
}

async function getMcpServerTools(serverId: string): Promise<void> {
  const payload = await getAgentMcpServerTools(serverId);
  emitDataMessage('mcpServerTools', payload);
  emitBridgeSuccessToast('获取 MCP 工具', 'MCP 工具列表已加载', { displayMode: 'notification_center' });
}

async function refreshMcpTools(serverId: string): Promise<void> {
  const payload = await refreshAgentMcpTools(serverId);
  emitDataMessage('mcpToolsRefreshed', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('刷新 MCP 工具', 'MCP 工具已刷新', { displayMode: 'notification_center' });
}

async function connectMcpServer(serverId: string): Promise<void> {
  await connectAgentMcpServer(serverId);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('连接 MCP 服务器', 'MCP 服务器已连接', { displayMode: 'notification_center' });
}

async function disconnectMcpServer(serverId: string): Promise<void> {
  await disconnectAgentMcpServer(serverId);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('断开 MCP 服务器', 'MCP 服务器已断开', { displayMode: 'notification_center' });
}

async function addRepository(url: string): Promise<void> {
  const payload = await addAgentRepository(url);
  emitDataMessage('repositoryAdded', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('添加仓库', '仓库已添加');
}

async function updateRepository(repositoryId: string, updates: Record<string, unknown>): Promise<void> {
  await updateAgentRepository(repositoryId, updates);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('更新仓库', '仓库已更新');
}

async function deleteRepository(repositoryId: string): Promise<void> {
  const payload = await deleteAgentRepository(repositoryId);
  emitDataMessage('repositoryDeleted', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('删除仓库', '仓库已删除');
}

async function refreshRepository(repositoryId: string): Promise<void> {
  const payload = await refreshAgentRepository(repositoryId);
  emitDataMessage('repositoryRefreshed', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('刷新仓库', '仓库已刷新', { displayMode: 'notification_center' });
}

async function loadSkillLibrary(): Promise<void> {
  const payload = await loadAgentSkillLibrary();
  emitDataMessage('skillLibraryLoaded', payload);
  emitBridgeSuccessToast('加载技能库', '技能库已加载', { displayMode: 'notification_center' });
}

async function installSkill(skillId: string): Promise<void> {
  try {
    const payload = await installAgentSkill(skillId);
    emitDataMessage('skillInstalled', payload);
    await dispatchSettingsBootstrap(true);
    await loadSkillLibrary();
    emitBridgeSuccessToast('安装技能', '技能已安装');
  } catch (error) {
    emitDataMessage('skillInstallFailed', {
      skillId,
      error: error instanceof Error ? error.message : String(error),
      source: 'repository',
    });
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
    emitBridgeSuccessToast('安装本地技能', '本地技能已安装');
  } catch (error) {
    emitDataMessage('skillInstallFailed', {
      error: error instanceof Error ? error.message : String(error),
      source: 'local',
    });
  }
}

async function saveSkillsConfig(config: Record<string, unknown>): Promise<void> {
  await saveAgentSkillsConfig(config);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('保存技能配置', '技能配置已保存', { displayMode: 'notification_center' });
}

async function addCustomTool(tool: Record<string, unknown>): Promise<void> {
  const payload = await addAgentCustomTool(tool);
  emitDataMessage('customToolAdded', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('添加自定义工具', '自定义工具已添加');
}

async function removeInstalledSkill(
  skillName: string,
  source: 'custom' | 'instruction',
): Promise<void> {
  const payload = await removeAgentInstalledSkill(skillName, source);
  const messageType = source === 'custom' ? 'customToolRemoved' : 'instructionSkillRemoved';
  emitDataMessage(messageType, payload);
  await dispatchSettingsBootstrap(true);
  const label = source === 'custom' ? '自定义工具' : '指令技能';
  emitBridgeSuccessToast(`删除${label}`, `${label}已删除`);
}

async function updateSkill(skillName: string): Promise<void> {
  const payload = await updateAgentSkill(skillName);
  emitDataMessage('skillUpdated', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('更新技能', '技能已更新');
}

async function updateAllSkills(): Promise<void> {
  const payload = await updateAllAgentSkills();
  emitDataMessage('allSkillsUpdated', payload);
  await dispatchSettingsBootstrap(true);
  emitBridgeSuccessToast('更新全部技能', '全部技能已更新');
}

async function clearProjectKnowledge(): Promise<void> {
  await clearAgentProjectKnowledge();
  await emitKnowledgePayload();
  emitBridgeSuccessToast('清空项目知识', '项目知识已清空');
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
        case 'workspaceBindingChanged': {
          const workspaceId = typeof message.workspaceId === 'string' ? message.workspaceId : '';
          const workspacePath = typeof message.workspacePath === 'string' ? message.workspacePath : '';
          const sessionId = typeof message.sessionId === 'string' ? message.sessionId.trim() : '';
          persistWorkspaceBinding(workspaceId, workspacePath, sessionId);
          return;
        }
        case 'webviewReady':
        case 'getState':
        case 'requestState':
          void restoreBridgeState('request_state').catch((error) => {
            reportExpectedRecoveryFailure('bootstrap ', '[web-client-bridge] bootstrap 失败:', error);
            scheduleRecovery('request_state', error);
          });
          return;
        case 'loadSettingsBootstrap':
          void dispatchSettingsBootstrap(Boolean(message.force), 'core').catch((error) => {
            reportExpectedRecoveryFailure('settings 配置加载', '[web-client-bridge] settings 配置加载失败:', error);
          });
          return;
        case 'saveUserRules':
          if (message.data && typeof message.data === 'object') {
            void saveUserRules(message.data as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('保存用户规则', '[web-client-bridge] 保存用户规则失败:', error);
            });
          }
          return;
        case 'saveWorkerConfig':
          if (typeof message.worker === 'string' && message.config && typeof message.config === 'object') {
            void saveWorkerConfig(message.worker, message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('保存代理配置', '[web-client-bridge] 保存代理配置失败:', error);
            });
          }
          return;
        case 'saveOrchestratorConfig':
          if (message.config && typeof message.config === 'object') {
            void saveOrchestratorConfig(message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('保存主模型配置', '[web-client-bridge] 保存主模型配置失败:', error);
            });
          }
          return;
        case 'saveAuxiliaryConfig':
          if (message.config && typeof message.config === 'object') {
            void saveAuxiliaryConfig(message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('保存辅助模型配置', '[web-client-bridge] 保存辅助模型配置失败:', error);
            });
          }
          return;
        case 'saveSafeguardConfig':
          if (message.config && typeof message.config === 'object') {
            void saveSafeguardConfig(message.config as any).catch((error) => {
              logBridgeOperationFailure('保存安全防护配置', '[web-client-bridge] 保存安全防护配置失败:', error);
            });
          }
          return;
        case 'testWorkerConnection':
          if (typeof message.worker === 'string' && message.config && typeof message.config === 'object') {
            void testWorkerConnection(message.worker, message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('测试代理连接', '[web-client-bridge] 测试代理连接失败:', error);
            });
          }
          return;
        case 'testOrchestratorConnection':
          if (message.config && typeof message.config === 'object') {
            void testOrchestratorConnection(message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('测试主模型连接', '[web-client-bridge] 测试主模型连接失败:', error);
            });
          }
          return;
        case 'testAuxiliaryConnection':
          if (message.config && typeof message.config === 'object') {
            void testAuxiliaryConnection(message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('测试辅助模型连接', '[web-client-bridge] 测试辅助模型连接失败:', error);
            });
          }
          return;
        case 'fetchModelList':
          if (message.config && typeof message.config === 'object' && typeof message.target === 'string') {
            void fetchModelList(message.config as Record<string, unknown>, message.target).catch((error) => {
              logBridgeOperationFailure('获取模型列表', '[web-client-bridge] 获取模型列表失败:', error);
            });
          }
          return;
        case 'addMCPServer':
          if (message.server && typeof message.server === 'object') {
            void addMcpServer(message.server as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('添加 MCP 服务器', '[web-client-bridge] 添加 MCP 服务器失败:', error);
            });
          }
          return;
        case 'updateMCPServer':
          if (typeof message.serverId === 'string' && message.updates && typeof message.updates === 'object') {
            void updateMcpServer(message.serverId, message.updates as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('更新 MCP 服务器', '[web-client-bridge] 更新 MCP 服务器失败:', error);
            });
          }
          return;
        case 'deleteMCPServer':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void deleteMcpServer(message.serverId).catch((error) => {
              logBridgeOperationFailure('删除 MCP 服务器', '[web-client-bridge] 删除 MCP 服务器失败:', error);
            });
          }
          return;
        case 'getMCPServerTools':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void getMcpServerTools(message.serverId).catch((error) => {
              logBridgeOperationFailure('获取 MCP 工具', '[web-client-bridge] 获取 MCP 工具失败:', error);
            });
          }
          return;
        case 'refreshMCPTools':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void refreshMcpTools(message.serverId).catch((error) => {
              logBridgeOperationFailure('刷新 MCP 工具', '[web-client-bridge] 刷新 MCP 工具失败:', error);
            });
          }
          return;
        case 'addRepository':
          if (typeof message.url === 'string' && message.url.trim()) {
            void addRepository(message.url).catch((error) => {
              logBridgeOperationFailure('添加仓库', '[web-client-bridge] 添加仓库失败:', error);
            });
          }
          return;
        case 'updateRepository':
          if (typeof message.repositoryId === 'string' && message.updates && typeof message.updates === 'object') {
            void updateRepository(message.repositoryId, message.updates as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('更新仓库', '[web-client-bridge] 更新仓库失败:', error);
            });
          }
          return;
        case 'deleteRepository':
          if (typeof message.repositoryId === 'string' && message.repositoryId.trim()) {
            void deleteRepository(message.repositoryId).catch((error) => {
              logBridgeOperationFailure('删除仓库', '[web-client-bridge] 删除仓库失败:', error);
            });
          }
          return;
        case 'refreshRepository':
          if (typeof message.repositoryId === 'string' && message.repositoryId.trim()) {
            void refreshRepository(message.repositoryId).catch((error) => {
              logBridgeOperationFailure('刷新仓库', '[web-client-bridge] 刷新仓库失败:', error);
            });
          }
          return;
        case 'loadSkillLibrary':
          void loadSkillLibrary().catch((error) => {
            logBridgeOperationFailure('加载技能库', '[web-client-bridge] 加载技能库失败:', error);
          });
          return;
        case 'installSkill':
          if (typeof message.skillId === 'string' && message.skillId.trim()) {
            void installSkill(message.skillId).catch((error) => {
              logBridgeOperationFailure('安装技能', '[web-client-bridge] 安装技能失败:', error);
            });
          }
          return;
        case 'installLocalSkill':
          void installLocalSkill(typeof message.directoryPath === 'string' ? message.directoryPath : undefined).catch((error) => {
            logBridgeOperationFailure('安装本地技能', '[web-client-bridge] 安装本地技能失败:', error);
          });
          return;
        case 'removeInstalledSkill': {
          const skillName = typeof message.skillName === 'string'
            ? message.skillName
            : typeof message.toolName === 'string'
              ? message.toolName
              : '';
          const source = message.source === 'custom' || message.source === 'instruction'
            ? message.source
            : null;
          if (skillName.trim() && source) {
            const label = source === 'custom' ? '自定义工具' : '指令技能';
            void removeInstalledSkill(skillName, source).catch((error) => {
              logBridgeOperationFailure(`删除${label}`, `[web-client-bridge] 删除${label}失败:`, error);
            });
          }
          return;
        }
        case 'updateSkill':
          if (typeof message.skillName === 'string' && message.skillName.trim()) {
            void updateSkill(message.skillName).catch((error) => {
              logBridgeOperationFailure('更新技能', '[web-client-bridge] 更新技能失败:', error);
            });
          }
          return;
        case 'updateAllSkills':
          void updateAllSkills().catch((error) => {
            logBridgeOperationFailure('更新全部技能', '[web-client-bridge] 更新全部技能失败:', error);
          });
          return;
        case 'newSession':
          dispatchWorkspaceSessionCleared(currentWorkspaceId, currentWorkspacePath);
          emitBridgeSuccessToast('新建会话', '已切换到新会话面板', { displayMode: 'notification_center' });
          return;
        case 'saveCurrentSession':
          void saveCurrentSession().catch((error) => {
            logBridgeOperationFailure('保存会话', '[web-client-bridge] 保存当前会话失败:', error);
          });
          return;
        case 'loadSessionNotifications':
          {
            const scope = resolveNotificationOperationScope(message);
            if (scope) {
              void loadSessionNotifications(scope).catch((error) => {
                if (isSessionMissingError(error)) return;
                reportExpectedRecoveryFailure('加载通知', '[web-client-bridge] 加载通知失败:', error);
              });
            }
          }
          return;
        case 'appendSessionNotification':
          if (message.notification && typeof message.notification === 'object') {
            const scope = resolveNotificationOperationScope(message);
            if (scope) {
              void appendSessionNotification(scope, message.notification as Record<string, unknown>).catch((error) => {
                if (isSessionMissingError(error)) return;
                reportExpectedRecoveryFailure('写入通知', '[web-client-bridge] 写入通知失败:', error);
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
                reportExpectedRecoveryFailure('标记通知已读', '[web-client-bridge] 标记通知已读失败:', error);
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
                reportExpectedRecoveryFailure('清空通知', '[web-client-bridge] 清空通知失败:', error);
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
                reportExpectedRecoveryFailure('删除通知', '[web-client-bridge] 删除通知失败:', error);
              });
            }
          }
          return;
        case 'executeTask':
          if (
            (typeof message.text === 'string' && message.text.trim())
            || (typeof message.skillName === 'string' && message.skillName.trim())
            || (Array.isArray(message.images) && message.images.length > 0)
          ) {
            void executeTask({
              text: typeof message.text === 'string' ? message.text : null,
              requestId: typeof message.requestId === 'string' ? message.requestId : undefined,
              skillName: typeof message.skillName === 'string' ? message.skillName : null,
              accessProfile: message.accessProfile === 'read_only'
                || message.accessProfile === 'restricted'
                || message.accessProfile === 'full_access'
                ? message.accessProfile
                : null,
              followUpMode: message.followUpMode === 'queue' ? 'queue' : undefined,
              images: Array.isArray(message.images)
                ? message.images as Array<{ name: string; dataUrl: string }>
                : [],
            });
          }
          return;
        case 'interruptTask':
          void interruptTask();
          return;
        case 'continueTask':
          void continueSessionExecution();
          return;
        case 'startTask':
          if (typeof message.taskId === 'string' && message.taskId.trim()) {
            void startTask(message.taskId);
          }
          return;
        case 'deleteTask':
          if (typeof message.taskId === 'string' && message.taskId.trim()) {
            void deleteTask(message.taskId).catch((error) => {
              logBridgeOperationFailure('删除任务', '[web-client-bridge] 删除任务失败:', error);
            });
          }
          return;
        case 'clearAllTasks':
          void clearAllTasks().catch((error) => {
            logBridgeOperationFailure('清空任务', '[web-client-bridge] 清空任务失败:', error);
          });
          return;
        case 'switchSession':
          if (typeof message.sessionId === 'string' && message.sessionId.trim()) {
            void switchSession(message.sessionId, {
              workspaceId: typeof message.workspaceId === 'string' ? message.workspaceId : undefined,
              workspacePath: typeof message.workspacePath === 'string' ? message.workspacePath : undefined,
            }).catch((error) => {
              logBridgeOperationFailure('切换会话', '[web-client-bridge] 切换会话失败:', error);
            });
          }
          return;
        case 'renameSession':
          if (
            typeof message.sessionId === 'string' && message.sessionId.trim()
            && typeof message.name === 'string' && message.name.trim()
          ) {
            void renameSession(message.sessionId, message.name).catch((error) => {
              logBridgeOperationFailure('重命名会话', '[web-client-bridge] 重命名会话失败:', error);
            });
          }
          return;
        case 'closeSession':
          if (typeof message.sessionId === 'string' && message.sessionId.trim()) {
            void closeSession(message.sessionId).catch((error) => {
              logBridgeOperationFailure('关闭会话', '[web-client-bridge] 关闭会话失败:', error);
            });
          }
          return;
        case 'deleteSession':
          if (typeof message.sessionId === 'string' && message.sessionId.trim()) {
            void deleteSession(message.sessionId).catch((error) => {
              logBridgeOperationFailure('删除会话', '[web-client-bridge] 删除会话失败:', error);
            });
          }
          return;
        case 'updateSetting':
          if (typeof message.key === 'string' && (message.key === "locale")) {
            void updateSetting(message.key, message.value).catch((error) => {
              logBridgeOperationFailure('更新设置', '[web-client-bridge] 更新设置失败:', error);
            });
          }
          return;
        case 'requestExecutionStats':
          void dispatchExecutionStats().catch((error) => {
            logBridgeOperationFailure('执行统计加载', '[web-client-bridge] 执行统计加载失败:', error);
          });
          return;
        case 'resetExecutionStats':
          void resetExecutionStats().catch((error) => {
            logBridgeOperationFailure('重置执行统计', '[web-client-bridge] 重置执行统计失败:', error);
          });
          return;
        case 'openLink':
          if (forwardToVsCodeHost(message)) {
            return;
          }
          if (typeof message.url === 'string' && message.url.trim()) {
            const fileTarget = normalizeFileReferenceTarget(message.url);
            if (fileTarget) {
              if (dispatchFilePreviewEvent({ filepath: fileTarget })) {
                return;
              }
              void openFilePreview(fileTarget).catch((error) => {
                logBridgeOperationFailure('打开文件预览', '[web-client-bridge] 打开文件预览失败:', error);
              });
              return;
            }
            window.open(message.url, '_blank', 'noopener,noreferrer');
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
              void openFilePreview(filePath, previewContent).catch((error) => {
                logBridgeOperationFailure('打开文件预览', '[web-client-bridge] 打开文件预览失败:', error);
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
            void openDiffPreview(message.filePath, diffContent).catch((error) => {
              logBridgeOperationFailure('打开差异预览', '[web-client-bridge] 打开差异预览失败:', error);
            });
          }
          return;
        case 'approveChange':
          if (typeof message.filePath === 'string' && message.filePath.trim()) {
            const targetSessionId = typeof message.sessionId === 'string' && message.sessionId.trim()
              ? message.sessionId.trim()
              : currentSessionId;
            void approveAgentChange(message.filePath, targetSessionId).then(async () => {
              await fetchBootstrap();
              emitBridgeSuccessToast('批准变更', '变更已批准');
            }).catch((error) => {
              logBridgeOperationFailure('批准变更', '[web-client-bridge] 批准变更失败:', error);
            });
          }
          return;
        case 'revertChange':
          if (typeof message.filePath === 'string' && message.filePath.trim()) {
            const targetSessionId = typeof message.sessionId === 'string' && message.sessionId.trim()
              ? message.sessionId.trim()
              : currentSessionId;
            void revertAgentChange(message.filePath, targetSessionId).then(async () => {
              await fetchBootstrap();
              emitBridgeSuccessToast('还原变更', '变更已还原');
            }).catch((error) => {
              logBridgeOperationFailure('还原变更', '[web-client-bridge] 还原变更失败:', error);
            });
          }
          return;
        case 'approveAllChanges':
          void approveAllAgentChanges(
            typeof message.sessionId === 'string' && message.sessionId.trim()
              ? message.sessionId.trim()
              : currentSessionId,
          ).then(async () => {
            await fetchBootstrap();
            emitBridgeSuccessToast('批准全部变更', '全部变更已批准');
          }).catch((error) => {
            logBridgeOperationFailure('批准全部变更', '[web-client-bridge] 批准全部变更失败:', error);
          });
          return;
        case 'revertAllChanges':
          void revertAllAgentChanges(
            typeof message.sessionId === 'string' && message.sessionId.trim()
              ? message.sessionId.trim()
              : currentSessionId,
          ).then(async () => {
            await fetchBootstrap();
            emitBridgeSuccessToast('还原全部变更', '全部变更已还原');
          }).catch((error) => {
            logBridgeOperationFailure('还原全部变更', '[web-client-bridge] 还原全部变更失败:', error);
          });
          return;
        case 'revertExecutionGroup':
          if (typeof message.executionGroupId === 'string' && message.executionGroupId.trim()) {
            const targetSessionId = typeof message.sessionId === 'string' && message.sessionId.trim()
              ? message.sessionId.trim()
              : currentSessionId;
            void revertAgentExecutionGroupChanges(message.executionGroupId, targetSessionId).then(async () => {
              await fetchBootstrap();
              emitBridgeSuccessToast('还原执行分组变更', '执行分组变更已还原');
            }).catch((error) => {
              logBridgeOperationFailure(
                '还原执行分组变更',
                '[web-client-bridge] 还原执行分组变更失败:',
                error,
              );
            });
          }
          return;
        case 'getProjectKnowledge':
          void dispatchProjectKnowledge().catch((error) => {
            logBridgeOperationFailure('项目知识加载', '[web-client-bridge] 项目知识加载失败:', error);
          });
          return;
        case 'clearProjectKnowledge':
          void clearProjectKnowledge().catch((error) => {
            logBridgeOperationFailure('清空项目知识', '[web-client-bridge] 清空项目知识失败:', error);
          });
          return;
        case 'addKnowledgeItem': {
          const kind = typeof message.kind === 'string' ? message.kind : '';
          const content = typeof message.content === 'string' ? message.content : '';
          if ((kind === 'adr' || kind === 'faq' || kind === 'learning') && content) {
            const payload: AgentKnowledgeItemPayload = {
              kind,
              content,
              title: typeof message.title === 'string' ? message.title : undefined,
              tags: Array.isArray(message.tags) ? (message.tags as string[]) : [],
              context: typeof message.context === 'string' ? message.context : undefined,
            };
            void addAgentKnowledgeItem(payload).then(async () => {
              await emitKnowledgePayload();
              emitBridgeSuccessToast('添加知识条目', '知识条目已添加');
            }).catch((error) => {
              logBridgeOperationFailure('添加知识条目 ', '[web-client-bridge] 添加知识条目失败:', error);
            });
          }
          return;
        }
        case 'updateKnowledgeItem': {
          const knowledgeId = typeof message.knowledgeId === 'string' ? message.knowledgeId.trim() : '';
          if (knowledgeId) {
            const patch: AgentKnowledgeItemPatch = {
              title: typeof message.title === 'string' ? message.title : undefined,
              content: typeof message.content === 'string' ? message.content : undefined,
              tags: Array.isArray(message.tags) ? (message.tags as string[]) : undefined,
              context: typeof message.context === 'string' ? message.context : undefined,
            };
            void updateAgentKnowledgeItem(knowledgeId, patch).then(async () => {
              await emitKnowledgePayload();
              emitBridgeSuccessToast('更新知识条目', '知识条目已更新');
            }).catch((error) => {
              logBridgeOperationFailure('更新知识条目 ', '[web-client-bridge] 更新知识条目失败:', error);
            });
          }
          return;
        }
        case 'deleteKnowledgeItem': {
          const knowledgeId = typeof message.knowledgeId === 'string' ? message.knowledgeId.trim() : '';
          if (knowledgeId) {
            void deleteAgentKnowledgeItem(knowledgeId).then(async () => {
              await emitKnowledgePayload();
              emitBridgeSuccessToast('删除知识条目', '知识条目已删除');
            }).catch((error) => {
              logBridgeOperationFailure('删除知识条目 ', '[web-client-bridge] 删除知识条目失败:', error);
            });
          }
          return;
        }
        case 'connectMCPServer':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void connectMcpServer(message.serverId).catch((error) => {
              logBridgeOperationFailure('连接 MCP 服务器', '[web-client-bridge] 连接 MCP 服务器失败:', error);
            });
          }
          return;
        case 'disconnectMCPServer':
          if (typeof message.serverId === 'string' && message.serverId.trim()) {
            void disconnectMcpServer(message.serverId).catch((error) => {
              logBridgeOperationFailure('断开 MCP 服务器', '[web-client-bridge] 断开 MCP 服务器失败:', error);
            });
          }
          return;
        case 'saveSkillsConfig':
          if (message.config && typeof message.config === 'object') {
            void saveSkillsConfig(message.config as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('保存技能配置', '[web-client-bridge] 保存技能配置失败:', error);
            });
          }
          return;
        case 'addCustomTool':
          if (message.tool && typeof message.tool === 'object') {
            void addCustomTool(message.tool as Record<string, unknown>).catch((error) => {
              logBridgeOperationFailure('添加自定义工具', '[web-client-bridge] 添加自定义工具失败:', error);
            });
          }
          return;
        case 'login':
        case 'logout':
          console.info(`[web-client-bridge] Web 端忽略本地鉴权消息: ${message.type}`);
          return;
        case 'uiError':
          console.error('[web-client-bridge] UI 错误上报:', {
            component: message.component,
            detail: message.detail,
            stack: message.stack,
          });
          return;
        case 'selectWorker':
          console.info('[web-client-bridge] Web 端代理选择由前端本地视图状态自行处理。');
          return;
        case 'toggleBuiltInTool':
          console.info('[web-client-bridge] 内置工具由运行时固定管理，已忽略切换请求。');
          return;
        default:
          console.log('[web-client-bridge] 未接入的消息已忽略:', message.type);
      }
    },
    onMessage(listener: (message: ClientBridgeMessage) => void): () => void {
      listeners.add(listener);
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
      return resolveWorkspaceQuery().sessionId;
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
        reportExpectedRecoveryFailure('入口初始化', '[web-client-bridge] Web 入口初始化失败:', error);
        scheduleRecovery('notify_ready', error);
      });
    },
  };
}
