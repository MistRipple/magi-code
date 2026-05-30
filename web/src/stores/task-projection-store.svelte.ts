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
let activeTaskProjectionSessionId = '';
let refreshTimer: ReturnType<typeof setInterval> | null = null;
let settleRefreshTimer: ReturnType<typeof setTimeout> | null = null;
let sseUnsubscribe: (() => void) | null = null;
let sseDebounceTimer: ReturnType<typeof setTimeout> | null = null;
const retiredSessionRootIds = new Set<string>();

function normalizeSessionKey(sessionId: string | null | undefined): string {
  return typeof sessionId === 'string' ? sessionId.trim() : '';
}

function createClient(): RustDaemonClient {
  return new RustDaemonClient(resolveAgentBaseUrl());
}

function currentWorkspaceId(): string {
  return typeof messagesState.currentWorkspaceId === 'string'
    ? messagesState.currentWorkspaceId.trim()
    : '';
}

function sessionRootKey(sessionId: string, rootTaskId: string): string {
  return `${sessionId}\u0000${rootTaskId}`;
}

function createEmptyInternalState(): InternalSessionTaskProjectionState {
  return {
    projection: null,
    loading: false,
    error: null,
    rootTaskId: null,
    selectedTaskId: null,
    fetchGeneration: 0,
    refreshAfterLoad: false,
  };
}

function ensureSessionState(sessionId: string): InternalSessionTaskProjectionState {
  if (!sessionStates[sessionId]) {
    sessionStates = {
      ...sessionStates,
      [sessionId]: createEmptyInternalState(),
    };
  }
  return sessionStates[sessionId];
}

function readSessionState(sessionId: string): InternalSessionTaskProjectionState | null {
  return sessionStates[sessionId] ?? null;
}

function trackedSessionIds(): string[] {
  const activeSessionId = normalizeSessionKey(activeTaskProjectionSessionId);
  const activeState = activeSessionId ? sessionStates[activeSessionId] : undefined;
  return activeState?.rootTaskId ? [activeSessionId] : [];
}

async function refreshTrackedSessions(): Promise<void> {
  const sessions = trackedSessionIds();
  await Promise.all(sessions.map((sessionId) => refreshTaskProjection(sessionId)));
}

export function getTaskProjectionState(sessionId: string | null | undefined): TaskProjectionState {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return EMPTY_TASK_PROJECTION_STATE;
  }
  // 直接返回 sessionStates 中的引用，使 Svelte 响应性系统能追踪字段变化。
  return readSessionState(normalizedSessionId) ?? EMPTY_TASK_PROJECTION_STATE;
}

export function ensureTaskProjectionState(sessionId: string | null | undefined): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  ensureSessionState(normalizedSessionId);
}

export function activateTaskProjectionSession(sessionId: string | null | undefined): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    activeTaskProjectionSessionId = '';
    stopAutoRefresh();
    return;
  }
  if (activeTaskProjectionSessionId === normalizedSessionId) {
    return;
  }
  activeTaskProjectionSessionId = normalizedSessionId;
  if (trackedSessionIds().length === 0) {
    stopAutoRefresh();
  } else {
    startAutoRefresh();
  }
}

export async function fetchTaskProjection(
  sessionId: string,
  rootTaskId: string,
): Promise<void> {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  if (retiredSessionRootIds.has(sessionRootKey(normalizedSessionId, rootTaskId))) {
    return;
  }
  const state = ensureSessionState(normalizedSessionId);
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
    const projection = await client.getTaskProjection(rootTaskId, normalizedSessionId, currentWorkspaceId());
    const latestState = ensureSessionState(normalizedSessionId);
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
    const latestState = ensureSessionState(normalizedSessionId);
    if (
      latestState.fetchGeneration !== fetchGeneration
      || latestState.rootTaskId !== rootTaskId
    ) {
      return;
    }
    latestState.error = err instanceof Error ? err.message : String(err);
  } finally {
    const latestState = ensureSessionState(normalizedSessionId);
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
        const currentState = ensureSessionState(normalizedSessionId);
        if (currentState.rootTaskId && !currentState.loading) {
          void refreshTaskProjection(normalizedSessionId);
        }
      });
    }
  }
}

export async function refreshTaskProjection(sessionId: string | null | undefined): Promise<void> {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  if (activeTaskProjectionSessionId !== normalizedSessionId) {
    return;
  }
  const state = ensureSessionState(normalizedSessionId);
  if (state.rootTaskId) {
    await fetchTaskProjection(normalizedSessionId, state.rootTaskId);
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
    const activeSessions = trackedSessionIds();
    if (activeSessions.length === 0) {
      return;
    }
    let hasLoadingSession = false;
    for (const sessionId of activeSessions) {
      const state = ensureSessionState(sessionId);
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

export function clearTaskProjection(sessionId?: string | null, retiredRootTaskId?: string | null): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    sessionStates = {};
    activeTaskProjectionSessionId = '';
    stopAutoRefresh();
    return;
  }
  const normalizedRetiredRootTaskId = typeof retiredRootTaskId === 'string'
    ? retiredRootTaskId.trim()
    : '';
  if (normalizedRetiredRootTaskId) {
    retiredSessionRootIds.add(sessionRootKey(normalizedSessionId, normalizedRetiredRootTaskId));
  }
  if (activeTaskProjectionSessionId === normalizedSessionId) {
    activeTaskProjectionSessionId = '';
  }
  if (!sessionStates[normalizedSessionId]) {
    if (trackedSessionIds().length === 0) {
      stopAutoRefresh();
    }
    return;
  }
  const nextStates = { ...sessionStates };
  delete nextStates[normalizedSessionId];
  sessionStates = nextStates;
  if (trackedSessionIds().length === 0) {
    stopAutoRefresh();
  }
}

export function selectTaskProjectionTask(
  sessionId: string | null | undefined,
  taskId: string | null | undefined,
): void {
  const normalizedSessionId = normalizeSessionKey(sessionId);
  if (!normalizedSessionId) {
    return;
  }
  const state = ensureSessionState(normalizedSessionId);
  const normalizedTaskId = typeof taskId === 'string' ? taskId.trim() : '';
  state.selectedTaskId = normalizedTaskId || null;
}

export function getTaskStatusModifier(status: TaskStatus): string {
  return status;
}
