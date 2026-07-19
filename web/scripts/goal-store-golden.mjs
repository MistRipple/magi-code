import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => (typeof value === 'function' ? value() : value);
globalThis.$derived.by = (fn) => fn();

const WORKSPACE_ID = 'workspace-goal-store-golden';
const WORKSPACE_PATH = '/tmp/workspace-goal-store-golden';
const SESSION_ID = 'session-goal-store-golden';
const SLOW_SESSION_ID = 'session-goal-store-slow';
let releaseSlowGoalRequest = null;

function installBrowserGlobals() {
  globalThis.window = {
    location: {
      href: `http://127.0.0.1:38123/web.html?workspaceId=${encodeURIComponent(WORKSPACE_ID)}&workspacePath=${encodeURIComponent(WORKSPACE_PATH)}&sessionId=${encodeURIComponent(SESSION_ID)}`,
    },
    localStorage: {
      getItem() { return null; },
      setItem() {},
      removeItem() {},
    },
  };
  globalThis.localStorage = globalThis.window.localStorage;
}

function jsonResponse(payload) {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
}

installBrowserGlobals();
const fetches = [];
globalThis.fetch = async (url) => {
  const parsed = new URL(String(url));
  fetches.push(parsed);
  if (parsed.pathname === '/api/goals/current') {
    if (parsed.searchParams.get('sessionId') === SLOW_SESSION_ID) {
      await new Promise((resolve) => {
        releaseSlowGoalRequest = resolve;
      });
    }
    return jsonResponse({
      sessionId: parsed.searchParams.get('sessionId'),
      workspaceId: parsed.searchParams.get('workspaceId'),
      workspacePath: parsed.searchParams.get('workspacePath'),
      goal: {
        goalId: 'goal-store-golden',
        sessionId: SESSION_ID,
        threadId: 'thread-goal-store-golden',
        objective: '验证 Goal store 刷新',
        status: 'complete',
        tokenBudget: 256000,
        tokensUsed: 1024,
        timeUsedSeconds: 3,
        createdAt: 1780000000000,
        updatedAt: 1780000003000,
      },
      plan: {
        planId: 'plan-store-golden',
        sessionId: SESSION_ID,
        revision: 4,
        language: 'zh-CN',
        state: 'completed',
        items: [
          { itemId: 'inspect-goal', title: '梳理目标模式', status: 'completed' },
          { itemId: 'inspect-agent', title: '检查代理面板', status: 'completed' },
          { itemId: 'inspect-change', title: '检查变更面板', status: 'completed' },
          { itemId: 'verify-plan', title: '验证执行计划抽屉', status: 'completed' },
        ],
        taskBindings: {},
        taskStatuses: {},
        updatedAt: 1780000003000,
      },
    });
  }
  return new Response('not found', { status: 404 });
};

await withGoldenViteServer(async (server) => {
  const goalStore = await server.ssrLoadModule('/src/stores/goal-store.svelte.ts');
  goalStore.ensureGoalState(SESSION_ID, WORKSPACE_ID, WORKSPACE_PATH);
  await goalStore.refreshCurrentGoal(SESSION_ID, WORKSPACE_ID, WORKSPACE_PATH);
  const state = goalStore.getGoalState(SESSION_ID, WORKSPACE_ID);

  assert.equal(fetches.length, 1, 'goal refresh should issue one request');
  assert.equal(fetches[0].pathname, '/api/goals/current');
  assert.equal(fetches[0].searchParams.get('sessionId'), SESSION_ID);
  assert.equal(fetches[0].searchParams.get('workspaceId'), WORKSPACE_ID);
  assert.equal(fetches[0].searchParams.get('workspacePath'), WORKSPACE_PATH);
  assert.equal(state.loading, false);
  assert.equal(state.error, null);
  assert.equal(state.response?.goal?.status, 'complete');
  assert.equal(state.response?.goal?.objective, '验证 Goal store 刷新');
  assert.equal(state.response?.plan?.items.length, 4);
  assert.deepEqual(
    state.response?.plan?.items.map((item) => item.title),
    ['梳理目标模式', '检查代理面板', '检查变更面板', '验证执行计划抽屉'],
    'goal store must expose the complete authoritative plan',
  );
  assert.ok(
    state.response?.plan?.items.every((item) => item.status === 'completed'),
    'completed plan history must remain visible as a complete collection',
  );
  assert.equal(
    goalStore.applySessionPlanSnapshot(SESSION_ID, WORKSPACE_ID, {
      ...state.response.plan,
      revision: 3,
      state: 'active',
    }),
    true,
  );
  assert.equal(
    goalStore.getGoalState(SESSION_ID, WORKSPACE_ID).response?.plan?.revision,
    4,
    'stale plan events must not overwrite a newer revision',
  );
  assert.equal(
    goalStore.applySessionPlanSnapshot(
      SESSION_ID,
      WORKSPACE_ID,
      null,
      'another-plan',
      9,
    ),
    true,
  );
  assert.equal(
    goalStore.getGoalState(SESSION_ID, WORKSPACE_ID).response?.plan?.planId,
    'plan-store-golden',
    'clear events from another plan must not remove the current plan',
  );
  assert.equal(
    goalStore.applySessionPlanSnapshot(SESSION_ID, WORKSPACE_ID, {
      ...state.response.plan,
      planId: 'stale-plan',
      revision: 99,
      updatedAt: 1780000001000,
    }),
    false,
    'events from another plan must trigger an authoritative refresh',
  );
  assert.equal(
    goalStore.getGoalState(SESSION_ID, WORKSPACE_ID).response?.plan?.planId,
    'plan-store-golden',
    'events from another plan must not overwrite the current plan',
  );

  const emptyWorkspaceState = goalStore.getGoalState(SESSION_ID, 'workspace-goal-store-other');
  assert.equal(
    emptyWorkspaceState.response,
    null,
    'goal state must not fall back to same session id from another workspace',
  );

  const firstSlowRefresh = goalStore.refreshCurrentGoal(
    SLOW_SESSION_ID,
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  const secondSlowRefresh = goalStore.refreshCurrentGoal(
    SLOW_SESSION_ID,
    WORKSPACE_ID,
    WORKSPACE_PATH,
  );
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(
    fetches.filter((url) => url.searchParams.get('sessionId') === SLOW_SESSION_ID).length,
    1,
    'overlapping goal refreshes must share one in-flight request',
  );
  releaseSlowGoalRequest();
  await Promise.all([firstSlowRefresh, secondSlowRefresh]);
  console.log('goal store golden passed');
});
