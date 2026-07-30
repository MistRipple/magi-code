import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => (typeof value === 'function' ? value() : value);
globalThis.$derived.by = (fn) => fn();

const WORKSPACE_ID = 'workspace-agent-run-golden';
const WORKSPACE_PATH = '/tmp/workspace-agent-run-golden';
const SESSION_ID = 'session-agent-run-golden';
const ROOT_TASK_ID = 'task-root-agent-run-golden';
const SESSION_ID_B = 'session-agent-run-golden-b';
const ROOT_TASK_ID_B = 'task-root-agent-run-golden-b';
const STALE_SESSION_ID = 'session-agent-run-stale';
const STALE_ROOT_TASK_ID = 'task-root-agent-run-stale';
const SLOW_SESSION_ID = 'session-agent-run-slow';
const SLOW_ROOT_TASK_ID = 'task-root-agent-run-slow';
const ACTION_SESSION_ID = 'session-agent-run-action';
const ACTION_ROOT_TASK_ID = 'task-root-agent-run-action';
const RESTARTED_ROOT_TASK_ID = 'task-root-agent-run-restarted';
const ARCHIVE_SESSION_ID = 'session-agent-run-archive';
const ARCHIVE_ROOT_TASK_ID = 'task-root-agent-run-archive';
let releaseSlowProjection = null;
let hasDelayedSlowProjection = false;

class MemoryStorage {
  constructor() {
    this.values = new Map();
  }

  getItem(key) {
    return this.values.has(String(key)) ? this.values.get(String(key)) : null;
  }

  setItem(key, value) {
    this.values.set(String(key), String(value));
  }

  removeItem(key) {
    this.values.delete(String(key));
  }
}

function jsonResponse(payload) {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

function projectionPayload(rootTaskId) {
  const tasks = Array.from({ length: 4 }, (_, index) => ({
    task_id: `${rootTaskId}-child-${index + 1}`,
    root_task_id: rootTaskId,
    parent_task_id: rootTaskId,
    title: `代理任务 ${index + 1}`,
    goal: `验证代理任务 ${index + 1}`,
    status: index === 3 ? 'running' : 'completed',
    created_at: 1780390000000 + index,
    updated_at: 1780390000100 + index,
  }));
  return {
    root_task: {
      task_id: rootTaskId,
      root_task_id: rootTaskId,
      title: '代理运行',
      goal: '验证代理运行刷新过滤',
      status: 'running',
      created_at: 1780390000000,
      updated_at: 1780390000000,
    },
    tasks,
    agents: tasks.map((task, index) => ({
      agentRunId: task.task_id,
      parentTaskId: rootTaskId,
      rootTaskId,
      displayName: task.title,
      goal: task.goal,
      role: `agent-${index + 1}`,
      modelSource: 'inherited_orchestrator',
      status: task.status,
      statusLabel: task.status === 'completed' ? '已完成' : '运行中',
      lifecycle: task.status,
      accessProfile: 'full_access',
      startedAt: task.created_at,
      updatedAt: task.updated_at,
      result: task.status === 'completed'
        ? { finalText: `${task.title} 已完成`, outputRefCount: 1, truncated: false }
        : null,
    })),
    running_tasks: [tasks[3].task_id],
    pending_tasks: [],
    completed_tasks: tasks.slice(0, 3).map((task) => task.task_id),
    edges: [],
    groups: [],
    active_task_ids: [rootTaskId],
    updated_at: 1780390000000,
    outcome: 'active',
    availableActions: [],
    failureSummary: null,
  };
}

function installBrowserGlobals() {
  const target = new EventTarget();
  globalThis.window = {
    location: {
      href: `http://127.0.0.1:38123/web.html?workspaceId=${encodeURIComponent(WORKSPACE_ID)}&workspacePath=${encodeURIComponent(WORKSPACE_PATH)}`,
    },
    localStorage: new MemoryStorage(),
    addEventListener: target.addEventListener.bind(target),
    removeEventListener: target.removeEventListener.bind(target),
    dispatchEvent: target.dispatchEvent.bind(target),
  };
  globalThis.localStorage = globalThis.window.localStorage;
}

function installFetchStub(fetches, terminalAgentRunRootIds, actionRequests) {
  globalThis.fetch = async (url, init = {}) => {
    const parsed = new URL(String(url));
    if (parsed.pathname.startsWith('/api/agent-runs/projection/')) {
      fetches.push(parsed);
      const rootTaskId = decodeURIComponent(parsed.pathname.split('/').pop() || '');
      if (terminalAgentRunRootIds.has(rootTaskId)) {
        return new Response('not found', { status: 404 });
      }
      if (rootTaskId === SLOW_ROOT_TASK_ID && !hasDelayedSlowProjection) {
        hasDelayedSlowProjection = true;
        await new Promise((resolve) => {
          releaseSlowProjection = resolve;
        });
      }
      return jsonResponse(projectionPayload(rootTaskId));
    }
    if (parsed.pathname === '/api/agent-runs/action') {
      const payload = JSON.parse(String(init.body || '{}'));
      actionRequests.push(payload);
      if (payload.action === 'restart') {
        return jsonResponse({
          action: 'restart',
          operationId: payload.operationId,
          sessionId: payload.sessionId,
          workspaceId: payload.workspaceId,
          workspacePath: payload.workspacePath,
          rootTaskId: RESTARTED_ROOT_TASK_ID,
          oldRootTaskId: payload.taskId,
          newRootTaskId: RESTARTED_ROOT_TASK_ID,
          restarted: true,
        });
      }
      if (payload.action === 'archive') {
        return jsonResponse({
          action: 'archive',
          operationId: payload.operationId,
          sessionId: payload.sessionId,
          workspaceId: payload.workspaceId,
          workspacePath: payload.workspacePath,
          rootTaskId: payload.taskId,
          archived: true,
        });
      }
      return jsonResponse({
        action: 'continue',
        operationId: payload.operationId,
        sessionId: payload.sessionId,
        workspaceId: payload.workspaceId,
        workspacePath: payload.workspacePath,
        rootTaskId: payload.taskId,
        status: 'continued',
      });
    }
    return new Response('not found', { status: 404 });
  };
}

function createBridge() {
  const listeners = new Set();
  return {
    kind: 'web',
    postMessage() {},
    onMessage(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    getState() {
      return undefined;
    },
    setState() {},
    getInitialSessionId() {
      return '';
    },
    getInitialLocale() {
      return 'zh-CN';
    },
    notifyReady() {},
    emit(message) {
      for (const listener of Array.from(listeners)) {
        listener(message);
      }
    },
  };
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

installBrowserGlobals();
const agentRunFetches = [];
const terminalAgentRunRootIds = new Set();
const actionRequests = [];
installFetchStub(agentRunFetches, terminalAgentRunRootIds, actionRequests);

await withGoldenViteServer(async (server) => {
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  const agentRunStore = await server.ssrLoadModule('/src/stores/agent-run-store.svelte.ts');
  const bridge = createBridge();
  bridgeRuntime.setClientBridge(bridge);

  agentRunStore.activateAgentRunSession(SESSION_ID, WORKSPACE_ID, WORKSPACE_PATH);
  await agentRunStore.fetchAgentRunProjection(SESSION_ID, ROOT_TASK_ID, WORKSPACE_ID, WORKSPACE_PATH);
  agentRunStore.startAutoRefresh(60_000);

  assert.equal(agentRunFetches.length, 1, 'initial projection fetch should run once');
  const initialProjection = agentRunStore.getAgentRunState(SESSION_ID, WORKSPACE_ID).projection;
  assert.equal(initialProjection?.tasks.length, 4, 'agent projection must retain every task in the run');
  assert.equal(initialProjection?.agents.length, 4, 'active agent center must receive every agent record');
  assert.deepEqual(
    initialProjection?.completed_tasks,
    [
      `${ROOT_TASK_ID}-child-1`,
      `${ROOT_TASK_ID}-child-2`,
      `${ROOT_TASK_ID}-child-3`,
    ],
    'completed agents must remain in the authoritative projection while another agent is running',
  );
  await delay(1700);
  const settledFetchCount = agentRunFetches.length;
  assert.equal(settledFetchCount, 2, 'settle refresh should run once after tracking starts');

  bridge.emit({
    type: 'rustTaskEvent',
    eventType: 'task.status.changed',
    workspaceId: WORKSPACE_ID,
    sessionId: 'session-agent-run-other',
    rootTaskIds: [ROOT_TASK_ID],
    payload: {},
  });
  await delay(380);
  assert.equal(
    agentRunFetches.length,
    settledFetchCount,
    'task event for another session must not refresh the active projection',
  );

  bridge.emit({
    type: 'rustTaskEvent',
    eventType: 'task.status.changed',
    workspaceId: 'workspace-agent-run-other',
    sessionId: SESSION_ID,
    rootTaskIds: [ROOT_TASK_ID],
    payload: {},
  });
  await delay(380);
  assert.equal(
    agentRunFetches.length,
    settledFetchCount,
    'task event for another workspace must not refresh the active projection',
  );

  bridge.emit({
    type: 'rustTaskEvent',
    eventType: 'task.status.changed',
    workspaceId: WORKSPACE_ID,
    sessionId: SESSION_ID,
    rootTaskIds: ['task-root-agent-run-other'],
    payload: {},
  });
  await delay(380);
  assert.equal(
    agentRunFetches.length,
    settledFetchCount,
    'task event for another root task must not refresh the active projection',
  );

  bridge.emit({
    type: 'rustTaskEvent',
    eventType: 'task.status.changed',
    workspaceId: WORKSPACE_ID,
    sessionId: SESSION_ID,
    rootTaskIds: [ROOT_TASK_ID],
    payload: {},
  });
  await delay(380);
  assert.equal(
    agentRunFetches.length,
    settledFetchCount + 1,
    'matching task event should refresh the active projection',
  );
  assert.equal(
    agentRunStore.getAgentRunState(SESSION_ID, WORKSPACE_ID).projection?.agents.length,
    4,
    'projection refresh must not collapse a multi-agent collection to the latest agent',
  );

  agentRunStore.activateAgentRunSession(SESSION_ID_B, WORKSPACE_ID, WORKSPACE_PATH);
  await agentRunStore.fetchAgentRunProjection(SESSION_ID_B, ROOT_TASK_ID_B, WORKSPACE_ID, WORKSPACE_PATH);
  agentRunStore.startAutoRefresh(60_000);
  const secondSessionInitialFetchCount = agentRunFetches.length;

  agentRunStore.activateAgentRunSession(SESSION_ID, WORKSPACE_ID, WORKSPACE_PATH);
  bridge.emit({
    type: 'rustTaskEvent',
    eventType: 'task.status.changed',
    workspaceId: WORKSPACE_ID,
    sessionId: SESSION_ID_B,
    rootTaskIds: [ROOT_TASK_ID_B],
    payload: {},
  });
  await delay(380);
  assert.equal(
    agentRunFetches.length,
    secondSessionInitialFetchCount + 1,
    'task event for a background running session should refresh its own projection',
  );
  assert.equal(
    agentRunFetches.at(-1).pathname,
    `/api/agent-runs/projection/${ROOT_TASK_ID_B}`,
    'background session refresh must target its own root task',
  );

  bridge.emit({
    type: 'rustTaskEvent',
    eventType: 'task.status.changed',
    workspaceId: WORKSPACE_ID,
    sessionId: SESSION_ID_B,
    rootTaskIds: [ROOT_TASK_ID],
    payload: {},
  });
  await delay(380);
  assert.equal(
    agentRunFetches.length,
    secondSessionInitialFetchCount + 1,
    'background session event with another root task must not refresh either projection',
  );

  terminalAgentRunRootIds.add(STALE_ROOT_TASK_ID);
  agentRunStore.activateAgentRunSession(STALE_SESSION_ID, WORKSPACE_ID, WORKSPACE_PATH);
  await agentRunStore.fetchAgentRunProjection(STALE_SESSION_ID, STALE_ROOT_TASK_ID, WORKSPACE_ID, WORKSPACE_PATH);
  const staleFetchCount = agentRunFetches.length;
  const staleState = agentRunStore.getAgentRunState(STALE_SESSION_ID, WORKSPACE_ID);
  assert.equal(staleState.rootTaskId, null, '404 projection should retire the stale session tracker');

  bridge.emit({
    type: 'rustTaskEvent',
    eventType: 'task.status.changed',
    workspaceId: WORKSPACE_ID,
    sessionId: STALE_SESSION_ID,
    rootTaskIds: [STALE_ROOT_TASK_ID],
    payload: {},
  });
  await delay(380);
  assert.equal(
    agentRunFetches.length,
    staleFetchCount,
    'retired stale session must not keep polling after terminal projection miss',
  );

  const firstSlowProjection = agentRunStore.fetchAgentRunProjection(
    SLOW_SESSION_ID,
    SLOW_ROOT_TASK_ID,
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  await delay(0);
  const secondSlowProjection = agentRunStore.fetchAgentRunProjection(
    SLOW_SESSION_ID,
    SLOW_ROOT_TASK_ID,
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  await delay(0);
  assert.equal(
    agentRunFetches.filter((url) => url.pathname.endsWith(SLOW_ROOT_TASK_ID)).length,
    1,
    'overlapping agent projection refreshes must not issue duplicate requests',
  );
  const switchedProjection = agentRunStore.fetchAgentRunProjection(
    SLOW_SESSION_ID,
    RESTARTED_ROOT_TASK_ID,
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  await delay(0);
  assert.equal(
    agentRunFetches.filter((url) => url.pathname.endsWith(RESTARTED_ROOT_TASK_ID)).length,
    1,
    '切换 root attempt 必须立即启动新投影请求，不能被旧 root 的 loading 状态阻塞',
  );
  releaseSlowProjection();
  await Promise.all([firstSlowProjection, secondSlowProjection, switchedProjection]);
  await delay(50);
  const switchedState = agentRunStore.getAgentRunState(SLOW_SESSION_ID, WORKSPACE_ID);
  assert.equal(switchedState.rootTaskId, RESTARTED_ROOT_TASK_ID);
  assert.equal(switchedState.loading, false, '旧 root 请求结束后不能把新 root 留在永久 loading 状态');

  agentRunStore.activateAgentRunSession(ACTION_SESSION_ID, WORKSPACE_ID, WORKSPACE_PATH);
  await agentRunStore.fetchAgentRunProjection(
    ACTION_SESSION_ID,
    ACTION_ROOT_TASK_ID,
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  const restartResponse = await agentRunStore.performAgentRunAction(
    ACTION_SESSION_ID,
    ACTION_ROOT_TASK_ID,
    'restart',
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  assert.equal(restartResponse?.newRootTaskId, RESTARTED_ROOT_TASK_ID);
  assert.equal(
    agentRunStore.getAgentRunState(ACTION_SESSION_ID, WORKSPACE_ID).rootTaskId,
    RESTARTED_ROOT_TASK_ID,
    '重新执行必须切换到新 root attempt，而不是复用或覆盖失败任务树',
  );
  assert.equal(
    actionRequests.at(-1)?.operationId?.startsWith('agent-run-restart-'),
    true,
    '统一动作请求必须携带幂等 operationId',
  );

  agentRunStore.activateAgentRunSession(ARCHIVE_SESSION_ID, WORKSPACE_ID, WORKSPACE_PATH);
  await agentRunStore.fetchAgentRunProjection(
    ARCHIVE_SESSION_ID,
    ARCHIVE_ROOT_TASK_ID,
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  await agentRunStore.performAgentRunAction(
    ARCHIVE_SESSION_ID,
    ARCHIVE_ROOT_TASK_ID,
    'archive',
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  assert.equal(
    agentRunStore.getAgentRunState(ARCHIVE_SESSION_ID, WORKSPACE_ID).rootTaskId,
    null,
    '归档成功后才允许移除当前面板状态',
  );

  agentRunStore.setAgentRunBridgeConnected(false);
  agentRunStore.startAutoRefresh(30);
  const disconnectedFetchCount = agentRunFetches.length;
  await delay(120);
  assert.equal(
    agentRunFetches.length,
    disconnectedFetchCount,
    'daemon 断线期间不得继续轮询代理投影',
  );
  agentRunStore.setAgentRunBridgeConnected(true);
  await delay(50);
  assert.ok(
    agentRunFetches.length > disconnectedFetchCount,
    'daemon 恢复后应集中刷新一次仍在跟踪的代理投影',
  );

  agentRunStore.stopAutoRefresh();
  console.log('agent run store golden replay passed');
});
