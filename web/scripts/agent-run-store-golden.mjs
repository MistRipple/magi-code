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
const NEXT_ROOT_TASK_ID = 'task-root-agent-run-next';
const ARCHIVE_SESSION_ID = 'session-agent-run-archive';
const ARCHIVE_ROOT_TASK_ID = 'task-root-agent-run-archive';
let releaseSlowProjection = null;
let hasDelayedSlowProjection = false;
const transferRequests = [];
let releaseTransfer;
let rejectTransfer = false;
const delayedProjections = new Map();
const delayedActions = new Map();

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
    if (parsed.pathname === '/api/agent-runs/transfer') {
      const payload = JSON.parse(String(init.body));
      transferRequests.push(payload);
      await new Promise(resolve => { releaseTransfer = resolve; });
      if (rejectTransfer) throw new Error('transfer response lost');
      return jsonResponse({ sessionId: payload.sessionId, taskId: payload.taskId,
        operationId: payload.operationId, owner: payload.owner, runnerStarted: true });
    }
    if (parsed.pathname.startsWith('/api/agent-runs/projection/')) {
      fetches.push(parsed);
      const rootTaskId = decodeURIComponent(parsed.pathname.split('/').pop() || '');
      const delayed = delayedProjections.get(rootTaskId);
      if (delayed) {
        delayedProjections.delete(rootTaskId);
        return delayed.promise;
      }
      if (terminalAgentRunRootIds.has(rootTaskId)) {
        return new Response('not found', { status: 404 });
      }
      if (rootTaskId === SLOW_ROOT_TASK_ID && !hasDelayedSlowProjection) {
        hasDelayedSlowProjection = true;
        await new Promise((resolve) => {
          releaseSlowProjection = resolve;
        });
        return new Response('old root removed', { status: 404 });
      }
      return jsonResponse(projectionPayload(rootTaskId));
    }
    if (parsed.pathname === '/api/agent-runs/action') {
      const payload = JSON.parse(String(init.body || '{}'));
      actionRequests.push(payload);
      const delayed = delayedActions.get(payload.taskId);
      if (delayed) {
        delayedActions.delete(payload.taskId);
        return delayed.promise;
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
      return new Response('unsupported action', { status: 422 });
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
  const { selectActiveAgentRunProjection } = await server.ssrLoadModule('/src/lib/active-agent-center.ts');
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
    NEXT_ROOT_TASK_ID,
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  await delay(0);
  assert.equal(
    agentRunFetches.filter((url) => url.pathname.endsWith(NEXT_ROOT_TASK_ID)).length,
    1,
    '切换 root attempt 必须立即启动新投影请求，不能被旧 root 的 loading 状态阻塞',
  );
  releaseSlowProjection();
  await Promise.all([firstSlowProjection, secondSlowProjection, switchedProjection]);
  await delay(50);
  const switchedState = agentRunStore.getAgentRunState(SLOW_SESSION_ID, WORKSPACE_ID);
  assert.equal(switchedState.rootTaskId, NEXT_ROOT_TASK_ID);
  assert.equal(switchedState.loading, false, '旧 root 请求结束后不能把新 root 留在永久 loading 状态');
  const oldRootReads = agentRunFetches.filter((url) => url.pathname.endsWith(SLOW_ROOT_TASK_ID)).length;
  await agentRunStore.fetchAgentRunProjection(SLOW_SESSION_ID, SLOW_ROOT_TASK_ID, WORKSPACE_ID, WORKSPACE_PATH);
  assert.equal(
    agentRunFetches.filter((url) => url.pathname.endsWith(SLOW_ROOT_TASK_ID)).length, oldRootReads + 1,
    '迟到的 404 不能将过期请求结论提交为永久退休，显式重读仍须经过 daemon',
  );

  // 重连清空再加载同一 root 时，请求计数可能重新从 1 开始；旧请求仍属于已失效的缓存。
  for (const staleResult of ['success', 'not_found']) {
    const sessionId = `session-reloaded-${staleResult}`;
    const rootTaskId = `root-reloaded-${staleResult}`;
    const delayed = Promise.withResolvers();
    delayedProjections.set(rootTaskId, delayed);
    const staleFetch = agentRunStore.fetchAgentRunProjection(sessionId, rootTaskId, WORKSPACE_ID);
    await delay(0);
    agentRunStore.clearAgentRunProjection(sessionId, undefined, WORKSPACE_ID);
    assert.equal(selectActiveAgentRunProjection(agentRunStore.getAgentRunState(sessionId, WORKSPACE_ID).projection), null);
    await agentRunStore.fetchAgentRunProjection(sessionId, rootTaskId, WORKSPACE_ID);
    const restoredProjection = agentRunStore.getAgentRunState(sessionId, WORKSPACE_ID).projection;
    delayed.resolve(staleResult === 'success'
      ? jsonResponse({ ...projectionPayload(rootTaskId), agents: [] })
      : new Response('old root removed', { status: 404 }));
    await staleFetch;
    assert.equal(agentRunStore.getAgentRunState(sessionId, WORKSPACE_ID).projection, restoredProjection,
      '重连前的旧成功响应或 404 都不能覆盖、退休重连后的同一 root');
    assert.equal(selectActiveAgentRunProjection(restoredProjection).agents.length, 4);
    agentRunStore.clearAgentRunProjection(sessionId, undefined, WORKSPACE_ID);
  }

  agentRunStore.activateAgentRunSession(ACTION_SESSION_ID, WORKSPACE_ID, WORKSPACE_PATH);
  await agentRunStore.fetchAgentRunProjection(
    ACTION_SESSION_ID,
    ACTION_ROOT_TASK_ID,
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  // 用户重新发消息会产生新的 root：运行视图只是切换到 daemon 给出的新 root，不存在“重新执行”动作。
  await agentRunStore.fetchAgentRunProjection(
    ACTION_SESSION_ID,
    NEXT_ROOT_TASK_ID,
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  assert.equal(
    agentRunStore.getAgentRunState(ACTION_SESSION_ID, WORKSPACE_ID).rootTaskId,
    NEXT_ROOT_TASK_ID,
    '新的 root 必须成为当前运行，而不是复用或覆盖失败任务树',
  );

  const transferRequest = {
    sessionId: ACTION_SESSION_ID, taskId: `${ACTION_ROOT_TASK_ID}-child-4`,
    scope: 'workspace', workspaceId: WORKSPACE_ID, workspacePath: WORKSPACE_PATH,
    operationId: 'server-projected-operation', expectedParentTurnId: 'original-parent-turn',
    expectedGeneration: 1, owner: 'session',
  };
  const transferring = agentRunStore.transferAgentRun(transferRequest);
  await delay(0);
  assert.equal(agentRunStore.getAgentRunState(ACTION_SESSION_ID, WORKSPACE_ID).transferInFlight, transferRequest.taskId);
  assert.equal(agentRunStore.getAgentRunState(SESSION_ID_B, WORKSPACE_ID).transferInFlight, null);
  assert.equal(await agentRunStore.transferAgentRun(transferRequest), null, '重复点击不能创建并发转交');
  const actionCount = actionRequests.length;
  assert.equal(await agentRunStore.performAgentRunAction(ACTION_SESSION_ID, NEXT_ROOT_TASK_ID, 'archive', WORKSPACE_ID), null);
  assert.equal(actionRequests.length, actionCount, '转交与主线控制不得并发提交');
  releaseTransfer();
  assert.equal((await transferring).taskId, transferRequest.taskId);
  assert.deepEqual(transferRequests[0], transferRequest, 'UI 必须原样提交服务端命令，不能推断代际或拥有方');
  assert.equal(agentRunStore.getAgentRunState(ACTION_SESSION_ID, WORKSPACE_ID).rootTaskId, NEXT_ROOT_TASK_ID,
    '旧代理转交不能把当前主线切回原 root 或改为子 Task');

  rejectTransfer = true;
  const failedTransfer = agentRunStore.transferAgentRun(transferRequest);
  const rejected = assert.rejects(failedTransfer, /transfer response lost/u);
  await delay(0);
  releaseTransfer();
  await rejected;
  assert.equal(agentRunStore.getAgentRunState(ACTION_SESSION_ID, WORKSPACE_ID).transferInFlight, null);
  assert.equal(agentRunStore.getAgentRunState(ACTION_SESSION_ID, WORKSPACE_ID).transferError.taskId, transferRequest.taskId);
  assert.equal(agentRunStore.getAgentRunState(SESSION_ID_B, WORKSPACE_ID).transferError, null, '错误不能污染其他会话');
  rejectTransfer = false;
  const retry = agentRunStore.transferAgentRun(transferRequest);
  await delay(0);
  releaseTransfer();
  await retry;
  assert.deepEqual(transferRequests.at(-1), transferRequest, '响应丢失后重试必须保留原 operation 与全部控制字段');
  assert.equal(agentRunStore.getAgentRunState(ACTION_SESSION_ID, WORKSPACE_ID).transferError, null);

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
  assert.equal(selectActiveAgentRunProjection(agentRunStore.getAgentRunState(ARCHIVE_SESSION_ID, WORKSPACE_ID).projection), null,
    '归档成功后列表与动作同步消失');
  const readsAfterArchive = agentRunFetches.length;
  await agentRunStore.fetchAgentRunProjection(ARCHIVE_SESSION_ID, ARCHIVE_ROOT_TASK_ID, WORKSPACE_ID, WORKSPACE_PATH);
  assert.equal(agentRunFetches.length, readsAfterArchive, '旧 root 刷新不能复活已归档运行');

  for (const actionResult of ['success', 'failure']) {
    const sessionId = `session-late-archive-${actionResult}`;
    const rootTaskId = `root-late-archive-${actionResult}`;
    await agentRunStore.fetchAgentRunProjection(sessionId, rootTaskId, WORKSPACE_ID);
    const delayed = Promise.withResolvers();
    delayedActions.set(rootTaskId, delayed);
    const archiving = agentRunStore.performAgentRunAction(sessionId, rootTaskId, 'archive', WORKSPACE_ID);
    const result = actionResult === 'failure' ? assert.rejects(archiving, /archive rejected/) : archiving;
    await delay(0);
    assert.equal(agentRunStore.getAgentRunState(sessionId, WORKSPACE_ID).rootTaskId, rootTaskId,
      '归档响应到达前仍保留原运行');
    await agentRunStore.fetchAgentRunProjection(sessionId, NEXT_ROOT_TASK_ID, WORKSPACE_ID);
    agentRunStore.activateAgentRunSession(SESSION_ID_B, WORKSPACE_ID, WORKSPACE_PATH);
    const nextProjection = agentRunStore.getAgentRunState(sessionId, WORKSPACE_ID).projection;
    const otherProjection = agentRunStore.getAgentRunState(SESSION_ID_B, WORKSPACE_ID).projection;
    if (actionResult === 'success') {
      delayed.resolve(jsonResponse({ action: 'archive', archived: true, sessionId, rootTaskId }));
    } else {
      delayed.reject(new Error('archive rejected'));
    }
    await result;
    const latest = agentRunStore.getAgentRunState(sessionId, WORKSPACE_ID);
    assert.equal(latest.projection, nextProjection, '旧归档回执不能清除同会话的新 root');
    assert.equal(latest.actionError, null, '旧归档失败不能覆盖新 root 的错误状态');
    assert.equal(latest.actionInFlight, null);
    assert.equal(agentRunStore.getAgentRunState(SESSION_ID_B, WORKSPACE_ID).projection, otherProjection,
      '切换会话后的归档回执不能影响当前会话');
    if (actionResult === 'success') {
      const reads = agentRunFetches.length;
      await agentRunStore.fetchAgentRunProjection(sessionId, rootTaskId, WORKSPACE_ID);
      assert.equal(agentRunFetches.length, reads, '迟到的成功回执仍要退休原 root');
    }
    agentRunStore.clearAgentRunProjection(sessionId, undefined, WORKSPACE_ID);
  }

  const failedRoot = 'root-archive-rejected';
  await agentRunStore.fetchAgentRunProjection(ARCHIVE_SESSION_ID, failedRoot, WORKSPACE_ID);
  const failedArchive = Promise.withResolvers();
  delayedActions.set(failedRoot, failedArchive);
  const failure = assert.rejects(
    agentRunStore.performAgentRunAction(ARCHIVE_SESSION_ID, failedRoot, 'archive', WORKSPACE_ID), /archive rejected/);
  await delay(0);
  failedArchive.reject(new Error('archive rejected'));
  await failure;
  const failedState = agentRunStore.getAgentRunState(ARCHIVE_SESSION_ID, WORKSPACE_ID);
  assert.equal(failedState.rootTaskId, failedRoot, '归档失败必须保留原运行供重试');
  assert.equal(failedState.actionError, 'archive rejected');
  assert.equal(selectActiveAgentRunProjection(failedState.projection).agents.length, 4);

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
