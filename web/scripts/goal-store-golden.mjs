import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => (typeof value === 'function' ? value() : value);
globalThis.$derived.by = (fn) => fn();

const WORKSPACE_ID = 'workspace-goal-store-golden';
const WORKSPACE_PATH = '/tmp/workspace-goal-store-golden';
const SESSION_ID = 'session-goal-store-golden';

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
      todoItems: [
        {
          content: '梳理目标模式',
          activeForm: '正在梳理目标模式',
          status: 'completed',
        },
        {
          content: '验证任务清单抽屉',
          activeForm: '正在验证任务清单抽屉',
          status: 'in_progress',
        },
      ],
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
  assert.equal(state.response?.todoItems?.length, 2);
  assert.equal(state.response?.todoItems?.[0]?.status, 'completed');
  assert.equal(state.response?.todoItems?.[1]?.activeForm, '正在验证任务清单抽屉');

  const emptyWorkspaceState = goalStore.getGoalState(SESSION_ID, 'workspace-goal-store-other');
  assert.equal(
    emptyWorkspaceState.response,
    null,
    'goal state must not fall back to same session id from another workspace',
  );
  console.log('goal store golden passed');
});
