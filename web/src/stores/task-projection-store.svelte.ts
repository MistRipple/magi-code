/**
 * Task Projection Store - 以 session 为边界缓存 Task Projection。
 *
 * 设计约束：
 * - 任务投影轮询、SSE 刷新都必须绑定当前会话 session
 * - 不再保留全局唯一 rootTaskId / projection
 * - workspace 仍然共用同一条事件流，但刷新按 session-keyed 状态执行
 */

import type {
  TaskProjectionDto,
  TaskStatus,
} from '../shared/rust-backend-types';
import { RustDaemonClient } from '../shared/rust-daemon-client';
import { resolveAgentBaseUrl } from '../web/agent-api';
import { onBridgeMessage } from '../shared/bridges/bridge-runtime';
import { messagesState } from './messages.svelte';

export interface TaskProjectionState {
  projection: TaskProjectionDto | null;
  loading: boolean;
  error: string | null;
  rootTaskId: string | null;
  selectedTaskId: string | null;
}

interface InternalSessionTaskProjectionState extends TaskProjectionState {
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
  fetchGeneration: number;
  refreshAfterLoad: boolean;
}

const EMPTY_TASK_PROJECTION_STATE: TaskProjectionState = {
  projection: null,
  loading: false,
  error: null,
  rootTaskId: null,
  selectedTaskId: null,
};
const SSE_DEBOUNCE_MS = 300;
const SETTLE_REFRESH_DELAY_MS = 1500;

let sessionStates = $state<Record<string, InternalSessionTaskProjectionState>>({});
let activeTaskProjectionScopeKey = '';
let refreshTimer: ReturnType<typeof setInterval> | null = null;
let settleRefreshTimer: ReturnType<typeof setTimeout> | null = null;
let sseUnsubscribe: (() => void) | null = null;
let sseDebounceTimer: ReturnType<typeof setTimeout> | null = null;
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

function createEmptyInternalState(
  workspaceId: string,
  workspacePath: string,
  sessionId: string,
): InternalSessionTaskProjectionState {
  return {
    workspaceId,
    workspacePath,
    sessionId,
    projection: null,
    loading: false,
    error: null,
    rootTaskId: null,
    selectedTaskId: null,
    fetchGeneration: 0,
    refreshAfterLoad: false,
  };
}

function ensureSessionState(
  sessionId: string,
  workspaceId: string | null | undefined = currentWorkspaceId(),
  workspacePath: string | null | undefined = currentWorkspacePath(),
): InternalSessionTaskProjectionState {
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
): InternalSessionTaskProjectionState | null {
  const scopeKey = projectionScopeKey(workspaceId, sessionId);
  return scopeKey ? sessionStates[scopeKey] ?? null : null;
}

function trackedSessionStates(): InternalSessionTaskProjectionState[] {
  const activeState = activeTaskProjectionScopeKey ? sessionStates[activeTaskProjectionScopeKey] : undefined;
  return activeState?.rootTaskId ? [activeState] : [];
}

async function refreshTrackedSessions(): Promise<void> {
  const sessions = trackedSessionStates();
  await Promise.all(sessions.map((state) => (
    refreshTaskProjection(state.sessionId, state.workspaceId, state.workspacePath)
  )));
}

export function getTaskProjectionState(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined = currentWorkspaceId(),
): TaskProjectionState {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return EMPTY_TASK_PROJECTION_STATE;
  }
  // 直接返回 sessionStates 中的引用，使 Svelte 响应性系统能追踪字段变化。
  return readSessionState(normalizedSessionId, workspaceId) ?? EMPTY_TASK_PROJECTION_STATE;
}

export function ensureTaskProjectionState(
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

export function activateTaskProjectionSession(
  sessionId: string | null | undefined,
  workspaceId: string | null | undefined = currentWorkspaceId(),
  workspacePath: string | null | undefined = currentWorkspacePath(),
): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    activeTaskProjectionScopeKey = '';
    stopAutoRefresh();
    return;
  }
  const normalizedWorkspaceId = normalizeWorkspaceKey(workspaceId);
  const normalizedWorkspacePath = typeof workspacePath === 'string' ? workspacePath.trim() : '';
  const scopeKey = projectionScopeKey(normalizedWorkspaceId, normalizedSessionId);
  if (activeTaskProjectionScopeKey === scopeKey) {
    if (normalizedWorkspacePath) {
      ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
    }
    return;
  }
  activeTaskProjectionScopeKey = scopeKey;
  ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
  if (trackedSessionStates().length === 0) {
    stopAutoRefresh();
  } else {
    startAutoRefresh();
  }
}

export async function fetchTaskProjection(
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
  const fetchGeneration = state.fetchGeneration + 1;
  state.fetchGeneration = fetchGeneration;
  const rootChanged = state.rootTaskId !== rootTaskId;
  state.rootTaskId = rootTaskId;
  if (rootChanged) {
    state.selectedTaskId = null;
  }
  state.loading = true;
  state.error = null;

  try {
    const client = createClient();
    const projection = await client.getTaskProjection(
      rootTaskId,
      normalizedSessionId,
      normalizedWorkspaceId,
      normalizedWorkspacePath,
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
    if (latestState.selectedTaskId) {
      const selectedTask = projection.tasks.find((task) => task.task_id === latestState.selectedTaskId);
      if (!selectedTask || selectedTask.status === 'killed') {
        latestState.selectedTaskId = null;
      }
    }
  } catch (err) {
    console.warn('[task-projection-store] task projection refresh failed:', err);
    const latestState = ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
    if (
      latestState.fetchGeneration !== fetchGeneration
      || latestState.rootTaskId !== rootTaskId
    ) {
      return;
    }
    latestState.error = 'load_failed';
  } finally {
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
          void refreshTaskProjection(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
        }
      });
    }
  }
}

export async function refreshTaskProjection(
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
  const scopeKey = projectionScopeKey(normalizedWorkspaceId, normalizedSessionId);
  if (activeTaskProjectionScopeKey !== scopeKey) {
    return;
  }
  const state = ensureSessionState(normalizedSessionId, normalizedWorkspaceId, normalizedWorkspacePath);
  if (state.rootTaskId) {
    await fetchTaskProjection(
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
    let hasLoadingSession = false;
    for (const state of activeSessions) {
      if (state.loading) {
        state.refreshAfterLoad = true;
        hasLoadingSession = true;
      }
    }
    if (hasLoadingSession) {
      return;
    }
    if (sseDebounceTimer !== null) {
      clearTimeout(sseDebounceTimer);
    }
    sseDebounceTimer = setTimeout(() => {
      sseDebounceTimer = null;
      void refreshTrackedSessions();
    }, SSE_DEBOUNCE_MS);
  });
}

function disconnectFromSSE(): void {
  if (sseDebounceTimer !== null) {
    clearTimeout(sseDebounceTimer);
    sseDebounceTimer = null;
  }
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

export function clearTaskProjection(
  sessionId?: string | null,
  retiredRootTaskId?: string | null,
  workspaceId: string | null | undefined = currentWorkspaceId(),
): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    sessionStates = {};
    activeTaskProjectionScopeKey = '';
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
  if (activeTaskProjectionScopeKey === scopeKey) {
    activeTaskProjectionScopeKey = '';
  }
  if (!sessionStates[scopeKey]) {
    if (trackedSessionStates().length === 0) {
      stopAutoRefresh();
    }
    return;
  }
  const nextStates = { ...sessionStates };
  delete nextStates[scopeKey];
  sessionStates = nextStates;
  if (trackedSessionStates().length === 0) {
    stopAutoRefresh();
  }
}

export function selectTaskProjectionTask(
  sessionId: string | null | undefined,
  taskId: string | null | undefined,
  workspaceId: string | null | undefined = currentWorkspaceId(),
  workspacePath: string | null | undefined = currentWorkspacePath(),
): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  const state = ensureSessionState(normalizedSessionId, workspaceId, workspacePath);
  const normalizedTaskId = typeof taskId === 'string' ? taskId.trim() : '';
  state.selectedTaskId = normalizedTaskId || null;
}

export function getTaskStatusModifier(status: TaskStatus): string {
  return status;
}
