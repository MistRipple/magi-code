/**
 * Agent Run Store - 以 session 为边界缓存代理运行投影。
 *
 * 设计约束：
 * - 代理运行轮询、SSE 刷新都必须绑定当前会话 session
 * - 不再保留全局唯一 rootTaskId / projection，避免多 session 串台
 * - workspace 仍然共用同一条事件流，但刷新按 session-keyed 状态执行
 */

import type {
  ClientBridgeMessage,
} from '../shared/bridges/client-bridge';
import type {
  AgentRunProjectionDto,
  TaskStatus,
} from '../shared/rust-backend-types';
import { RustDaemonClient } from '../shared/rust-daemon-client';
import { resolveAgentBaseUrl } from '../web/agent-api';
import { onBridgeMessage } from '../shared/bridges/bridge-runtime';
import { messagesState } from './messages.svelte';

export interface AgentRunState {
  projection: AgentRunProjectionDto | null;
  loading: boolean;
  error: string | null;
  rootTaskId: string | null;
  selectedAgentRunId: string | null;
}

interface InternalSessionAgentRunState extends AgentRunState {
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
  fetchGeneration: number;
  refreshAfterLoad: boolean;
}

const EMPTY_AGENT_RUN_STATE: AgentRunState = {
  projection: null,
  loading: false,
  error: null,
  rootTaskId: null,
  selectedAgentRunId: null,
};
const SSE_DEBOUNCE_MS = 300;
const SETTLE_REFRESH_DELAY_MS = 1500;

let sessionStates = $state<Record<string, InternalSessionAgentRunState>>({});
let activeAgentRunScopeKey = '';
let refreshTimer: ReturnType<typeof setInterval> | null = null;
let settleRefreshTimer: ReturnType<typeof setTimeout> | null = null;
let sseUnsubscribe: (() => void) | null = null;
let sseDebounceTimer: ReturnType<typeof setTimeout> | null = null;
const pendingSseRefreshScopeKeys = new Set<string>();
const retiredSessionRootIds = new Set<string>();

function normalizeSessionKey(sessionId: string | null | undefined): string {
  return typeof sessionId === 'string' ? sessionId.trim() : '';
}

function normalizeWorkspaceKey(workspaceId: string | null | undefined): string {
  return typeof workspaceId === 'string' ? workspaceId.trim() : '';
}

function projectionScopeKey(
  workspaceId: string | null | undefined,
  sessionId: string | null | undefined,
): string {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return '';
  }
  const normalizedWorkspaceId = normalizeWorkspaceKey(workspaceId);
  return normalizedWorkspaceId
    ? `${normalizedWorkspaceId}\u0000${normalizedSessionId}`
    : `session:${normalizedSessionId}`;
}

function createClient(): RustDaemonClient {
  return new RustDaemonClient(resolveAgentBaseUrl());
}

function currentWorkspaceId(): string {
  return typeof messagesState.currentWorkspaceId === 'string'
    ? messagesState.currentWorkspaceId.trim()
    : '';
}

function currentWorkspacePath(): string {
  return typeof messagesState.currentWorkspacePath === 'string'
    ? messagesState.currentWorkspacePath.trim()
    : '';
}

function sessionRootKey(workspaceId: string, sessionId: string, rootTaskId: string): string {
  return `${workspaceId}\u0000${sessionId}\u0000${rootTaskId}`;
}

function stateScopeKey(state: InternalSessionAgentRunState): string {
  return projectionScopeKey(state.workspaceId, state.sessionId);
}

function isRunningTaskStatus(status: unknown): boolean {
  return status === 'pending' || status === 'running';
}

function shouldTrackSessionState(state: InternalSessionAgentRunState): boolean {
  if (!state.rootTaskId) {
    return false;
  }
  if (retiredSessionRootIds.has(sessionRootKey(state.workspaceId, state.sessionId, state.rootTaskId))) {
    return false;
  }
  const projection = state.projection;
  if (!projection) {
    return true;
  }
  if ((projection.pending_tasks?.length ?? 0) > 0 || (projection.running_tasks?.length ?? 0) > 0) {
    return true;
  }
  return isRunningTaskStatus(projection.root_task?.status)
    || isRunningTaskStatus(projection.aggregate_status)
    || isRunningTaskStatus(projection.runner_status);
}

function isTerminalProjectionLoadError(error: unknown): boolean {
  if (!(error instanceof Error)) {
    return false;
  }
  const message = error.message;
  return message.includes('HTTP 404:') || message.includes('HTTP 410:');
}

function createEmptyInternalState(
  workspaceId: string,
  workspacePath: string,
  sessionId: string,
): InternalSessionAgentRunState {
  return {
    workspaceId,
    workspacePath,
    sessionId,
    projection: null,
    loading: false,
    error: null,
    rootTaskId: null,
    selectedAgentRunId: null,
    fetchGeneration: 0,
    refreshAfterLoad: false,
  };
}

function ensureSessionState(
  sessionId: string,
  workspaceId: string | null | undefined = currentWorkspaceId(),
  workspacePath: string | null | undefined = currentWorkspacePath(),
): InternalSessionAgentRunState {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  const normalizedWorkspaceId = normalizeWorkspaceKey(workspaceId);
  const normalizedWorkspacePath = typeof workspacePath === 'string' ? workspacePath.trim() : '';
  const scopeKey = projectionScopeKey(normalizedWorkspaceId, normalizedSessionId);
  if (!scopeKey) {
    return createEmptyInternalState('', '', '');
  }
  if (!sessionStates[scopeKey]) {
    sessionStates = {
      ...sessionStates,
      [scopeKey]: createEmptyInternalState(normalizedWorkspaceId, normalizedWorkspacePath, normalizedSessionId),
    };
  } else if (normalizedWorkspacePath) {
    sessionStates[scopeKey].workspacePath = normalizedWorkspacePath;
  }
  return sessionStates[scopeKey];
}

function readSessionState(
  sessionId: string,
  workspaceId: string | null | undefined = currentWorkspaceId(),
): InternalSessionAgentRunState | null {
  const scopeKey = projectionScopeKey(workspaceId, sessionId);
  return scopeKey ? sessionStates[scopeKey] ?? null : null;
}

function trackedSessionStates(): InternalSessionAgentRunState[] {
  return Object.values(sessionStates).filter(shouldTrackSessionState);
}

function bridgeStringValue(value: unknown): string {
  return typeof value === 'string' ? value.trim() : '';
}

function bridgeRecordValue(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : {};
}

function bridgePayloadString(
  payload: Record<string, unknown>,
  snakeKey: string,
  camelKey: string,
): string {
  return bridgeStringValue(payload[snakeKey]) || bridgeStringValue(payload[camelKey]);
}

function taskEventRootTaskIds(message: ClientBridgeMessage): Set<string> {
  const payload = bridgeRecordValue(message.payload);
  const values = [
    ...(Array.isArray(message.rootTaskIds) ? message.rootTaskIds.map(bridgeStringValue) : []),
    bridgeStringValue(message.rootTaskId),
    bridgePayloadString(payload, 'root_task_id', 'rootTaskId'),
    bridgePayloadString(payload, 'old_root_task_id', 'oldRootTaskId'),
    bridgePayloadString(payload, 'new_root_task_id', 'newRootTaskId'),
  ].filter(Boolean);
  return new Set(values);
}

function taskEventMatchesState(
  message: ClientBridgeMessage,
  state: InternalSessionAgentRunState,
): boolean {
  const payload = bridgeRecordValue(message.payload);
  const eventWorkspaceId = bridgeStringValue(message.workspaceId)
    || bridgePayloadString(payload, 'workspace_id', 'workspaceId');
  if (eventWorkspaceId && state.workspaceId && eventWorkspaceId !== state.workspaceId) {
    return false;
  }

  const eventSessionId = bridgeStringValue(message.sessionId)
    || bridgePayloadString(payload, 'session_id', 'sessionId');
  if (eventSessionId && eventSessionId !== state.sessionId) {
    return false;
  }

  const rootTaskIds = taskEventRootTaskIds(message);
  if (rootTaskIds.size > 0 && state.rootTaskId && !rootTaskIds.has(state.rootTaskId)) {
    return false;
  }

  return true;
}

async function refreshSessions(sessions: InternalSessionAgentRunState[]): Promise<void> {
  const uniqueStates = Array.from(
    new Map(sessions.map((state) => [stateScopeKey(state), state])).values(),
  );
  await Promise.all(uniqueStates.map((state) => (
    refreshAgentRunProjection(state.sessionId, state.workspaceId, state.workspacePath)
  )));
}

async function refreshTrackedSessions(): Promise<void> {
  await refreshSessions(trackedSessionStates());
}

function reconcileAutoRefresh(): void {
  if (trackedSessionStates().length === 0) {
    stopAutoRefresh();
    return;
  }
  if (refreshTimer === null) {
    startAutoRefresh();
    return;
  }
  connectToSSE();
}

export function getAgentRunState(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined = currentWorkspaceId(),
): AgentRunState {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return EMPTY_AGENT_RUN_STATE;
  }
  // 直接返回 sessionStates 中的引用，使 Svelte 响应性系统能追踪字段变化。
  return readSessionState(normalizedSessionId, workspaceId) ?? EMPTY_AGENT_RUN_STATE;
}

export function ensureAgentRunState(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined = currentWorkspaceId(),
  workspacePath: string | null | undefined = currentWorkspacePath(),
): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  ensureSessionState(normalizedSessionId, workspaceId, workspacePath);
}

export function activateAgentRunSession(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined = currentWorkspaceId(),
  workspacePath: string | null | undefined = currentWorkspacePath(),
): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    activeAgentRunScopeKey = '';
    stopAutoRefresh();
    return;
  }
  const normalizedWorkspaceId = normalizeWorkspaceKey(workspaceId);
  const normalizedWorkspacePath = typeof workspacePath === 'string' ? workspacePath.trim() : '';
  const scopeKey = projectionScopeKey(normalizedWorkspaceId, normalizedSessionId);
  if (activeAgentRunScopeKey === scopeKey) {
    if (normalizedWorkspacePath) {
      ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
    }
    return;
  }
  activeAgentRunScopeKey = scopeKey;
  ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
  reconcileAutoRefresh();
}

export async function fetchAgentRunProjection(
  sessionId: string,
  rootTaskId: string,
  workspaceId: string | null | undefined = currentWorkspaceId(),
  workspacePath: string | null | undefined = currentWorkspacePath(),
): Promise<void> {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  const normalizedWorkspaceId = normalizeWorkspaceKey(workspaceId);
  const normalizedWorkspacePath = typeof workspacePath === 'string' ? workspacePath.trim() : '';
  if (retiredSessionRootIds.has(sessionRootKey(normalizedWorkspaceId, normalizedSessionId, rootTaskId))) {
    return;
  }
  const state = ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
  if (state.loading) {
    state.refreshAfterLoad = true;
    return;
  }
  const fetchGeneration = state.fetchGeneration + 1;
  state.fetchGeneration = fetchGeneration;
  const rootChanged = state.rootTaskId !== rootTaskId;
  state.rootTaskId = rootTaskId;
  if (rootChanged) {
    state.selectedAgentRunId = null;
  }
  state.loading = true;
  state.error = null;
  let terminalProjectionLoadError = false;

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort('agent_run_request_timeout'), 10_000);
  try {
    const client = createClient();
    const projection = await client.getAgentRunProjection(
      rootTaskId,
      normalizedSessionId,
      normalizedWorkspaceId,
      normalizedWorkspacePath,
      controller.signal,
    );
    const latestState = ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
    if (
      latestState.fetchGeneration !== fetchGeneration
      || latestState.rootTaskId !== rootTaskId
    ) {
      return;
    }
    latestState.projection = projection;
    latestState.error = null;
    if (latestState.selectedAgentRunId) {
      const selectedTask = projection.tasks.find((task) => task.task_id === latestState.selectedAgentRunId);
      if (!selectedTask || selectedTask.status === 'killed') {
        latestState.selectedAgentRunId = null;
      }
    }
  } catch (err) {
    terminalProjectionLoadError = isTerminalProjectionLoadError(err);
    if (terminalProjectionLoadError) {
      retiredSessionRootIds.add(sessionRootKey(normalizedWorkspaceId, normalizedSessionId, rootTaskId));
      const scopeKey = projectionScopeKey(normalizedWorkspaceId, normalizedSessionId);
      if (scopeKey && sessionStates[scopeKey]?.rootTaskId === rootTaskId) {
        const nextStates = { ...sessionStates };
        delete nextStates[scopeKey];
        sessionStates = nextStates;
      }
      if (activeAgentRunScopeKey === scopeKey) {
        activeAgentRunScopeKey = '';
      }
      reconcileAutoRefresh();
      return;
    }
    console.warn('[agent-run-store] agent run projection refresh failed:', err);
    const latestState = ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
    if (
      latestState.fetchGeneration !== fetchGeneration
      || latestState.rootTaskId !== rootTaskId
    ) {
      return;
    }
    latestState.error = 'load_failed';
  } finally {
    clearTimeout(timeout);
    if (terminalProjectionLoadError) {
      return;
    }
    const latestState = ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
    if (
      latestState.fetchGeneration !== fetchGeneration
      || latestState.rootTaskId !== rootTaskId
    ) {
      return;
    }
    latestState.loading = false;
    if (latestState.refreshAfterLoad && latestState.rootTaskId) {
      latestState.refreshAfterLoad = false;
      queueMicrotask(() => {
        const currentState = ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
        if (currentState.rootTaskId && !currentState.loading) {
          void refreshAgentRunProjection(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
        }
      });
    }
    reconcileAutoRefresh();
  }
}

export async function refreshAgentRunProjection(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined = currentWorkspaceId(),
  workspacePath: string | null | undefined = currentWorkspacePath(),
): Promise<void> {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  const normalizedWorkspaceId = normalizeWorkspaceKey(workspaceId);
  const normalizedWorkspacePath = typeof workspacePath === 'string' ? workspacePath.trim() : '';
  const state = ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
  if (state.rootTaskId) {
    await fetchAgentRunProjection(
      normalizedSessionId,
      state.rootTaskId,
      normalizedWorkspaceId,
      state.workspacePath || normalizedWorkspacePath,
    );
  }
}

function connectToSSE(): void {
  if (sseUnsubscribe) {
    return;
  }
  sseUnsubscribe = onBridgeMessage((message) => {
    if (message.type !== 'rustTaskEvent') {
      return;
    }
    const activeSessions = trackedSessionStates();
    if (activeSessions.length === 0) {
      return;
    }
    const matchingSessions = activeSessions.filter((state) => taskEventMatchesState(message, state));
    if (matchingSessions.length === 0) {
      return;
    }
    let hasPendingRefresh = false;
    for (const state of matchingSessions) {
      if (state.loading) {
        state.refreshAfterLoad = true;
        continue;
      }
      const scopeKey = stateScopeKey(state);
      if (scopeKey) {
        pendingSseRefreshScopeKeys.add(scopeKey);
        hasPendingRefresh = true;
      }
    }
    if (!hasPendingRefresh) {
      return;
    }
    if (sseDebounceTimer !== null) {
      clearTimeout(sseDebounceTimer);
    }
    sseDebounceTimer = setTimeout(() => {
      sseDebounceTimer = null;
      const scopeKeys = Array.from(pendingSseRefreshScopeKeys);
      pendingSseRefreshScopeKeys.clear();
      const sessions = scopeKeys
        .map((scopeKey) => sessionStates[scopeKey])
        .filter((state): state is InternalSessionAgentRunState => Boolean(state));
      void refreshSessions(sessions);
    }, SSE_DEBOUNCE_MS);
  });
}

function disconnectFromSSE(): void {
  if (sseDebounceTimer !== null) {
    clearTimeout(sseDebounceTimer);
    sseDebounceTimer = null;
  }
  pendingSseRefreshScopeKeys.clear();
  if (sseUnsubscribe) {
    sseUnsubscribe();
    sseUnsubscribe = null;
  }
}

export function startAutoRefresh(intervalMs = 5000): void {
  stopAutoRefresh();
  connectToSSE();
  settleRefreshTimer = setTimeout(() => {
    settleRefreshTimer = null;
    void refreshTrackedSessions();
  }, SETTLE_REFRESH_DELAY_MS);
  refreshTimer = setInterval(() => {
    void refreshTrackedSessions();
  }, intervalMs);
}

export function stopAutoRefresh(): void {
  if (refreshTimer !== null) {
    clearInterval(refreshTimer);
    refreshTimer = null;
  }
  if (settleRefreshTimer !== null) {
    clearTimeout(settleRefreshTimer);
    settleRefreshTimer = null;
  }
  disconnectFromSSE();
}

export function clearAgentRunProjection(
  sessionId?: string | null,
  retiredRootTaskId?: string | null,
  workspaceId: string | null | undefined = currentWorkspaceId(),
): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    sessionStates = {};
    activeAgentRunScopeKey = '';
    stopAutoRefresh();
    return;
  }
  const normalizedWorkspaceId = normalizeWorkspaceKey(workspaceId);
  const scopeKey = projectionScopeKey(normalizedWorkspaceId, normalizedSessionId);
  const normalizedRetiredRootTaskId = typeof retiredRootTaskId === 'string'
    ? retiredRootTaskId.trim()
    : '';
  if (normalizedRetiredRootTaskId) {
    retiredSessionRootIds.add(sessionRootKey(normalizedWorkspaceId, normalizedSessionId, normalizedRetiredRootTaskId));
  }
  if (activeAgentRunScopeKey === scopeKey) {
    activeAgentRunScopeKey = '';
  }
  if (!sessionStates[scopeKey]) {
    reconcileAutoRefresh();
    return;
  }
  const nextStates = { ...sessionStates };
  delete nextStates[scopeKey];
  sessionStates = nextStates;
  reconcileAutoRefresh();
}

export function selectAgentRun(
  sessionId: string | null | undefined,
  agentRunId: string | null | undefined,
  workspaceId: string | null | undefined = currentWorkspaceId(),
  workspacePath: string | null | undefined = currentWorkspacePath(),
): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  const state = ensureSessionState(normalizedSessionId, workspaceId, workspacePath);
  const normalizedAgentRunId = typeof agentRunId === 'string' ? agentRunId.trim() : '';
  state.selectedAgentRunId = normalizedAgentRunId || null;
}

export function getAgentRunStatusModifier(status: TaskStatus): string {
  return status;
}
