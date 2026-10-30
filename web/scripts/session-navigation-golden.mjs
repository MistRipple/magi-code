import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

class MemoryStorage {
  constructor() {
    this.values = new Map();
  }

  getItem(key) {
    return this.values.get(String(key)) ?? null;
  }

  setItem(key, value) {
    this.values.set(String(key), String(value));
  }

  removeItem(key) {
    this.values.delete(String(key));
  }
}

const storage = new MemoryStorage();
const windowEvents = new EventTarget();
globalThis.window = {
  localStorage: storage,
  setTimeout,
  clearTimeout,
  setInterval,
  clearInterval,
  addEventListener: windowEvents.addEventListener.bind(windowEvents),
  removeEventListener: windowEvents.removeEventListener.bind(windowEvents),
  dispatchEvent: windowEvents.dispatchEvent.bind(windowEvents),
  location: { href: 'http://127.0.0.1:38123/web.html' },
};
globalThis.localStorage = storage;

await withGoldenViteServer(async (server) => {
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  const navigation = await server.ssrLoadModule('/src/shared/session-navigation.svelte.ts');
  const postedMessages = [];
  const bridge = {
    kind: 'web',
    postMessage(message) {
      postedMessages.push(message);
    },
    onMessage() {
      return () => {};
    },
    getState() {
      return undefined;
    },
    setState() {},
    getInitialSessionId() {
      return '';
    },
    getInitialLocale() {
      return '';
    },
    notifyReady() {},
  };
  bridgeRuntime.setClientBridge(bridge);

  const personalDraft = navigation.navigateSession({ kind: 'draft', scope: 'personal' });
  assert.ok(personalDraft, 'personal draft navigation should create a transaction');
  assert.equal(navigation.sessionNavigationState.pending?.requestId, personalDraft.requestId);
  assert.equal(
    navigation.settleSessionNavigation(personalDraft.requestId, {
      kind: 'draft',
      scope: 'personal',
    }),
    true,
    'personal navigation should settle only after the exact target is committed',
  );
  await personalDraft.completion;
  assert.equal(navigation.sessionNavigationState.pending, null);

  const workspaceSession = navigation.navigateSession({
    kind: 'session',
    scope: 'workspace',
    workspaceId: 'workspace-navigation-golden',
    workspacePath: '/tmp/workspace-navigation-golden',
    sessionId: 'session-navigation-golden',
  });
  assert.ok(workspaceSession, 'workspace session navigation should create a transaction');
  assert.equal(navigation.matchesSessionNavigationTarget(workspaceSession.requestId, {
    kind: 'session', scope: 'workspace', workspaceId: 'workspace-other',
    workspacePath: '/tmp/workspace-navigation-golden', sessionId: 'session-navigation-golden',
  }), false, '相同目录提示不能让其它项目提交导航');
  assert.equal(navigation.matchesSessionNavigationTarget(workspaceSession.requestId, {
    kind: 'session', scope: 'workspace', workspaceId: 'workspace-navigation-golden',
    workspacePath: '/tmp/new-primary', sessionId: 'session-other',
  }), false, '同项目不同会话必须继续拒绝');
  assert.equal(navigation.navigateSession({
    ...workspaceSession.target, workspacePath: '/tmp/new-primary',
  }), workspaceSession, '切换主要目录不能重新创建同项目同会话的在途导航');
  assert.equal(
    navigation.settleSessionNavigation(workspaceSession.requestId, {
      kind: 'session',
      scope: 'workspace',
      workspaceId: 'workspace-navigation-golden',
      workspacePath: '/tmp/new-primary',
      sessionId: 'session-navigation-golden',
    }),
    true,
    '同项目同会话的响应在主要目录变化后仍可确认导航',
  );
  await workspaceSession.completion;
  assert.equal(navigation.sessionNavigationState.pending, null);

  const mismatchedTarget = navigation.navigateSession({
    kind: 'session',
    scope: 'personal',
    sessionId: 'session-navigation-target',
  });
  assert.ok(mismatchedTarget);
  assert.equal(
    navigation.settleSessionNavigation(mismatchedTarget.requestId, {
      kind: 'session',
      scope: 'personal',
      sessionId: 'session-navigation-other',
    }),
    false,
    'a response for a different session must fail the active transaction',
  );
  await assert.rejects(mismatchedTarget.completion, /目标不一致/);
  assert.equal(navigation.sessionNavigationState.pending, null);

  const isolatedRequest = navigation.navigateSession({
    kind: 'session',
    scope: 'personal',
    sessionId: 'session-navigation-isolated',
  });
  assert.ok(isolatedRequest);
  assert.equal(
    navigation.failSessionNavigation('session-navigation-foreign-request'),
    false,
    'a late failure from another request must not clear the active transaction',
  );
  assert.equal(navigation.sessionNavigationState.pending?.requestId, isolatedRequest.requestId);
  navigation.settleSessionNavigation(isolatedRequest.requestId, {
    kind: 'session',
    scope: 'personal',
    sessionId: 'session-navigation-isolated',
  });
  await isolatedRequest.completion;

  const timedOut = navigation.navigateSession({
    kind: 'session',
    scope: 'personal',
    sessionId: 'session-navigation-timeout',
  });
  assert.ok(timedOut);
  await assert.rejects(
    navigation.waitForSessionNavigation(timedOut, 10),
    /会话导航超时/,
  );
  assert.equal(navigation.sessionNavigationState.pending, null);
  await assert.rejects(timedOut.completion, /会话导航超时/);

  assert.deepEqual(
    postedMessages.map((message) => message.type),
    ['navigateSession', 'navigateSession', 'navigateSession', 'navigateSession', 'navigateSession'],
    'each transaction must emit one typed navigation request',
  );

  console.log('session navigation golden replay passed');
}, { configFile: 'vite.web.config.ts' });
