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
    tasks: [],
    edges: [],
    groups: [],
    active_task_ids: [rootTaskId],
    updated_at: 1780390000000,
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

function installFetchStub(fetches, terminalAgentRunRootIds) {
  globalThis.fetch = async (url) => {
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
installFetchStub(agentRunFetches, terminalAgentRunRootIds);

await withGoldenViteServer(async (server) => {
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  const agentRunStore = await server.ssrLoadModule('/src/stores/agent-run-store.svelte.ts');
  const bridge = createBridge();
  bridgeRuntime.setClientBridge(bridge);

  agentRunStore.activateAgentRunSession(SESSION_ID, WORKSPACE_ID, WORKSPACE_PATH);
  await agentRunStore.fetchAgentRunProjection(SESSION_ID, ROOT_TASK_ID, WORKSPACE_ID, WORKSPACE_PATH);
  agentRunStore.startAutoRefresh(60_000);

  assert.equal(agentRunFetches.length, 1, 'initial projection fetch should run once');
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
  releaseSlowProjection();
  await Promise.all([firstSlowProjection, secondSlowProjection]);
  await delay(50);

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
