import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => (typeof value === 'function' ? value() : value);
globalThis.$derived.by = (fn) => fn();

const WORKSPACE_ID = 'workspace-task-projection-golden';
const WORKSPACE_PATH = '/tmp/workspace-task-projection-golden';
const SESSION_ID = 'session-task-projection-golden';
const ROOT_TASK_ID = 'task-root-task-projection-golden';

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
      title: '任务投影',
      goal: '验证任务投影刷新过滤',
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

function installFetchStub(fetches) {
  globalThis.fetch = async (url) => {
    const parsed = new URL(String(url));
    if (parsed.pathname.startsWith('/api/tasks/projection/')) {
      fetches.push(parsed);
      const rootTaskId = decodeURIComponent(parsed.pathname.split('/').pop() || '');
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
const projectionFetches = [];
installFetchStub(projectionFetches);

await withGoldenViteServer(async (server) => {
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  const taskStore = await server.ssrLoadModule('/src/stores/task-projection-store.svelte.ts');
  const bridge = createBridge();
  bridgeRuntime.setClientBridge(bridge);

  taskStore.activateTaskProjectionSession(SESSION_ID, WORKSPACE_ID, WORKSPACE_PATH);
  await taskStore.fetchTaskProjection(SESSION_ID, ROOT_TASK_ID, WORKSPACE_ID, WORKSPACE_PATH);
  taskStore.startAutoRefresh(60_000);

  assert.equal(projectionFetches.length, 1, 'initial projection fetch should run once');
  await delay(1700);
  const settledFetchCount = projectionFetches.length;
  assert.equal(settledFetchCount, 2, 'settle refresh should run once after tracking starts');

  bridge.emit({
    type: 'rustTaskEvent',
    eventType: 'task.status.changed',
    workspaceId: WORKSPACE_ID,
    sessionId: 'session-task-projection-other',
    rootTaskIds: [ROOT_TASK_ID],
    payload: {},
  });
  await delay(380);
  assert.equal(
    projectionFetches.length,
    settledFetchCount,
    'task event for another session must not refresh the active projection',
  );

  bridge.emit({
    type: 'rustTaskEvent',
    eventType: 'task.status.changed',
    workspaceId: 'workspace-task-projection-other',
    sessionId: SESSION_ID,
    rootTaskIds: [ROOT_TASK_ID],
    payload: {},
  });
  await delay(380);
  assert.equal(
    projectionFetches.length,
    settledFetchCount,
    'task event for another workspace must not refresh the active projection',
  );

  bridge.emit({
    type: 'rustTaskEvent',
    eventType: 'task.status.changed',
    workspaceId: WORKSPACE_ID,
    sessionId: SESSION_ID,
    rootTaskIds: ['task-root-task-projection-other'],
    payload: {},
  });
  await delay(380);
  assert.equal(
    projectionFetches.length,
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
    projectionFetches.length,
    settledFetchCount + 1,
    'matching task event should refresh the active projection',
  );

  taskStore.stopAutoRefresh();
  console.log('task projection store golden replay passed');
});
