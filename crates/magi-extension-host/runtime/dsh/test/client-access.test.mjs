import test from 'node:test';
import assert from 'node:assert/strict';

// The client packages are browser bundles that register themselves with `window.__ModuleLoader__`.
// The page host's loader also pulls in browser-only assets, so this test installs the same contract
// with just the modules these packages require.
const externals = {
  react: await import('react'),
  'react/jsx-runtime': await import('react/jsx-runtime'),
  '@deepseek-ai/cordis': await import('@deepseek-ai/cordis'),
  '@deepseek-ai/cosmokit': await import('@deepseek-ai/cosmokit'),
  '@deepseek-ai/dsh-client-store': await import('@deepseek-ai/dsh-client-store'),
  '@deepseek-ai/dsh-client-ui-slots': await import('@deepseek-ai/dsh-client-ui-slots'),
  '@deepseek-ai/dsh-api-gateway/client': await import('@deepseek-ai/dsh-api-gateway/client'),
  '@deepseek-ai/dsh-typert-protocol': await import('@deepseek-ai/dsh-typert-protocol'),
  zod: await import('zod'),
};
const definitions = new Map();
const evaluated = new Map();
function requireModule(id) {
  if (Object.hasOwn(externals, id)) return externals[id];
  if (evaluated.has(id)) return evaluated.get(id);
  const definition = definitions.get(id)
    ?? (id.endsWith('/client') ? definitions.get(id.slice(0, -'/client'.length)) : undefined);
  if (!definition) throw new Error(`module ${id} is not available to this test`);
  const exports = definition.factory(requireModule);
  evaluated.set(id, exports);
  return exports;
}
globalThis.window = globalThis;
globalThis.__ModuleLoader__ = { load: definition => definitions.set(definition.id, definition), require: requireModule };
await import('@deepseek-ai/dsh-api-remote-access/client');
await import('@deepseek-ai/dsh-client-file-upload/client');
await import('@deepseek-ai/dsh-api-session-controller/client');
await import('@deepseek-ai/dsh-client-ui-session/client');
await import('@deepseek-ai/dsh-client-resources/client');
const { Context, Service } = externals['@deepseek-ai/cordis'];
const { RemoteStream } = externals['@deepseek-ai/dsh-api-gateway/client'];
const { REMOTE_ACCESS_SNAPSHOT_KEY } = await import('@deepseek-ai/dsh-api-remote-access');
const { installClientTypertRegistry, installGlobalStandardKit } = await import('../client-host/standard-kit.mjs');
const { createSessionGateway } = await import('../session-gateway.mjs');
const { toSessionEvent } = await import('../session-observer.mjs');

function harness() {
  const roots = [];
  const ctx = new Context();
  // The two services the real access client injects, reduced to what its mount uses.
  ctx.provide('slots', {
    provideRoot: contribution => { roots.push(contribution); return () => {}; },
    installScope: () => {},
  });
  class Remote extends Service {
    constructor(owner) { super(owner, 'remote'); }
    async $mount() { return async () => {}; }
  }
  ctx.plugin(Remote);
  return { ctx, roots };
}

test('the real access client answers the admitted owner baseline for every resource and action', async t => {
  Reflect.set(globalThis, REMOTE_ACCESS_SNAPSHOT_KEY, { type: 'baseline', owner: true });
  t.after(() => Reflect.deleteProperty(globalThis, REMOTE_ACCESS_SNAPSHOT_KEY));
  const { ctx, roots } = harness();
  const access = requireModule('@deepseek-ai/dsh-api-remote-access');
  await ctx.plugin(access.default ?? access).await();

  assert.equal(ctx.remoteAccess.source('session', 's1', 'session/send').getSnapshot(), true);
  const global = roots.find(root => root.keyedHooks?.remoteAccess).keyedHooks.remoteAccess;
  assert.equal(global('session/send').getSnapshot(), true);
});

test('without an access snapshot the real client answers false rather than inventing permission', async () => {
  const { ctx } = harness();
  const access = requireModule('@deepseek-ai/dsh-api-remote-access');
  await ctx.plugin(access.default ?? access).await();
  assert.equal(ctx.remoteAccess.source('session', 's1', 'session/send').getSnapshot(), false);
});

test('standard kit provides shell-owned panelInfo and workspaceAccess without handwritten session facts', async t => {
  Reflect.set(globalThis, REMOTE_ACCESS_SNAPSHOT_KEY, { type: 'baseline', owner: true });
  t.after(() => Reflect.deleteProperty(globalThis, REMOTE_ACCESS_SNAPSHOT_KEY));
  const { ctx, roots } = harness();
  const access = requireModule('@deepseek-ai/dsh-api-remote-access');
  await ctx.plugin(access.default ?? access).await();
  const standardKit = installGlobalStandardKit(ctx);
  const kit = roots.find(root => root.hooks?.panelInfo);
  assert.equal(kit.keyedHooks.workspaceAccess('w1', 'workspace/open').getSnapshot(), true);
  assert.equal(kit.keyedHooks.workspaceAccess('w1'), undefined);
  assert.deepEqual(Object.keys(kit.hooks).sort(), ['contextMenu', 'panelInfo', 'workspaces']);
  assert.deepEqual(Object.keys(kit.keyedHooks).sort(), ['workspaceAccess']);
  assert.deepEqual(kit.hooks.workspaces.getSnapshot(), []);
  assert.equal(kit.hooks.contextMenu.getSnapshot().open, false);
  assert.deepEqual(kit.hooks.panelInfo.getSnapshot(), { activePanelId: undefined, active: false });
  standardKit.updateHostFacts({ placement: 'page', activePanelId: 'mission-control' });
  assert.deepEqual(kit.hooks.panelInfo.getSnapshot(), { activePanelId: 'mission-control', active: true });
  // Session hooks & scopes are no longer fabricated here; ui-session owns them.
  assert.equal(kit.hooks.sessions, undefined);
  assert.equal(kit.keyedHooks.sessionRetainInfo, undefined);
  assert.equal(kit.keyedHooks.sessionAccess, undefined);
  assert.equal(kit.keyedHooks.resource, undefined);
  assert.equal(kit.props?.useContextMenu, undefined);
});

test('locked dsh-api-session-controller and dsh-client-ui-session mount over the real session gateway', async t => {
  Reflect.set(globalThis, REMOTE_ACCESS_SNAPSHOT_KEY, { type: 'baseline', owner: true });
  t.after(() => Reflect.deleteProperty(globalThis, REMOTE_ACCESS_SNAPSHOT_KEY));

  const sessionEvents = [
    { seq: 0, time: 1000, ...toSessionEvent({ kind: 'turnStarted', turn: 1 }) },
    { seq: 1, time: 1005, ...toSessionEvent({ kind: 'textInput', messageId: 'u1', text: 'hello' }) },
    { seq: 2, time: 1010, ...toSessionEvent({ kind: 'subagentCatalog', childId: 'task-sub-1', childCreatedAt: 1010, mode: 'one-shot', label: 'worker' }) },
  ];
  const remoteListeners = new Map();
  const gateway = createSessionGateway({
    hostGeneration: 1,
    root: {
      emit(event, ...args) {
        for (const fn of remoteListeners.get(event) ?? []) fn(...args);
      },
    },
    requestOwner: async (method, params) => {
      const fact = {
        sessionId: 's1',
        title: '真实会话',
        status: 'Active',
        createdAt: 500,
        updatedAt: 1010,
        isRunning: false,
        messageCount: 1,
        hasUnreadCompletion: false,
        cwd: '/work/demo',
      };
      if (method === 'host.session.list') {
        return { factsSequence: 1, sessions: [fact] };
      }
      if (method === 'host.session.get' && params?.sessionId === 's1') {
        return { factsSequence: 1, session: fact };
      }
      return { factsSequence: 1, session: null };
    },
    readLog: async identity => ({
      createdAt: identity.createdAt,
      initialDirectory: identity.initialDirectory,
      through: 10,
      events: Object.freeze(sessionEvents),
      sourceSequences: Object.freeze([1, 2, 3]),
    }),
  });
  t.after(() => gateway.close());

  const proto = gateway.SessionController.prototype;
  const controllerSvc = Object.create(proto);
  const callSession = (method, ...args) => proto[method].call(controllerSvc, ...args);

  const roots = [];
  const scopes = new Map();
  const ctx = new Context();
  installClientTypertRegistry(ctx);
  ctx.provide('slots', {
    provideRoot: contribution => { roots.push(contribution); return () => {}; },
    installScope: (name, adapter) => { scopes.set(name, adapter); },
    bindStoreScope: () => () => {},
  });
  const gen = Object.freeze({ id: 'gen-1', host: { home: '/dsh' } });
  const generation = { getSnapshot: () => gen, subscribe: () => () => {} };
  ctx.provide('connection', {
    generation,
    state: { getSnapshot: () => ({ status: 'connected' }), subscribe: () => () => {} },
    isLoopback: true,
    reconnect() {},
  });
  const okCall = fn => async (...args) => {
    try {
      return { ok: true, value: await fn(...args) };
    } catch (error) {
      return {
        ok: false,
        error: {
          code: error?.code ?? 'gateway/internal',
          message: String(error?.message ?? error),
          details: error?.details ?? {},
        },
      };
    }
  };
  const sessionRemote = {
    list: okCall((req, signal) => callSession('list', req, signal)),
    control: signal => callSession('control', signal),
    follow: (req, signal) => callSession('follow', req, signal),
    page: okCall((req, signal) => callSession('page', req, signal)),
    projections: okCall((req, signal) => callSession('projections', req, signal)),
    create: okCall((req, signal) => callSession('create', req, signal)),
    prompt: okCall((req, signal) => callSession('prompt', req, signal)),
    cancel: okCall((req, signal) => callSession('cancel', req, signal)),
    rename: okCall((req, signal) => callSession('rename', req, signal)),
    updateQueue: okCall((req, signal) => callSession('updateQueue', req, signal)),
  };
  const remote = {
    async $mount() { return async () => {}; },
    $stream(options) {
      return new RemoteStream({ generation }, options);
    },
    $on(event, fn) {
      let set = remoteListeners.get(event);
      if (!set) { set = new Set(); remoteListeners.set(event, set); }
      set.add(fn);
      return () => set.delete(fn);
    },
    session: sessionRemote,
    commands: {},
    subagents: {},
  };
  ctx.provide('remote', remote);
  ctx.provide('remote.session', sessionRemote);
  ctx.provide('remote.commands', remote.commands);
  ctx.provide('remote.subagents', remote.subagents);

  const mountMod = id => {
    const mod = requireModule(id);
    return ctx.plugin(mod.default ?? mod);
  };
  const fibers = [
    mountMod('@deepseek-ai/dsh-api-remote-access'),
    mountMod('@deepseek-ai/dsh-client-file-upload'),
    mountMod('@deepseek-ai/dsh-api-session-controller'),
    mountMod('@deepseek-ai/dsh-client-ui-session'),
  ];
  t.after(async () => {
    for (const f of [...fibers].reverse()) {
      try { await f.dispose(); } catch { /* ignore */ }
    }
  });
  await Promise.all(fibers.map(f => f.await()));
  await new Promise(resolve => {
    const f = ctx.inject(['sessions', 'uiSession'], () => {
      queueMicrotask(() => { f.dispose(); resolve(); });
    });
  });

  // Wait for initial session/list to settle.
  for (let i = 0; i < 50 && ctx.sessions.list.getSnapshot().phase !== 'ready'; i++) {
    await new Promise(r => setTimeout(r, 10));
  }
  const listSnap = ctx.sessions.list.getSnapshot();
  assert.equal(listSnap.phase, 'ready');
  assert.deepEqual(listSnap.ids, ['s1']);
  assert.equal(listSnap.byId.s1.title, '真实会话');
  assert.equal(listSnap.byId.s1.cwd, '/work/demo');

  // ui-session provides the real standard hooks and the `session` scope adapter.
  const uiSessionRoot = roots.find(r => r.hooks?.sessions && r.hooks?.sessionStatus);
  assert.ok(uiSessionRoot, 'ui-session must provide sessions and sessionStatus root hooks');
  assert.equal(uiSessionRoot.keyedHooks.sessionAccess('s1', 'session/send').getSnapshot(), true);
  assert.equal(uiSessionRoot.keyedHooks.sessionRetainInfo('s1').getSnapshot().referenceCount, 0);

  const ref = ctx.sessions.retain('s1', { source: 'mainView' });
  t.after(() => ref.release());
  await ref.ready;

  const retainSnap = uiSessionRoot.keyedHooks.sessionRetainInfo('s1').getSnapshot();
  assert.equal(retainSnap.referenceCount, 1);
  assert.deepEqual({ ...retainSnap.retainedBy }, { mainView: 1 });
  const sessionAdapter = scopes.get('session');
  assert.ok(sessionAdapter, 'ui-session must install the session scope adapter');
  const currentBinding = sessionAdapter.current.getSnapshot();
  assert.equal(currentBinding.key, 's1');
  assert.equal(currentBinding.props.sessionId, 's1');
  assert.equal(currentBinding.hooks.session.getSnapshot().openState, 'open');
  assert.deepEqual(currentBinding.keyedHooks.projection('subagentCatalog').getSnapshot(), [
    { id: 'task-sub-1', createdAt: 1010, mode: 'one-shot', label: 'worker' },
  ]);
  assert.equal(currentBinding.keyedHooks.access('session/prompt').getSnapshot(), true);
});

test('transport bridge exposes ConnectionWebSocket over postMessage without a separate $events hack', async t => {
  const { createHostBridge } = await import('../client-host/transport.mjs');
  const posted = [];
  const listeners = new Set();
  const prevParent = globalThis.parent;
  const prevAdd = globalThis.addEventListener;
  const prevRemove = globalThis.removeEventListener;
  const prevDocument = globalThis.document;
  const prevNavigator = globalThis.navigator;
  const prevFetch = globalThis.fetch;
  const prevOpen = globalThis.open;
  const prevTransport = globalThis.__DSH_TRANSPORT__;
  globalThis.parent = { postMessage: msg => posted.push(msg) };
  globalThis.addEventListener = (type, fn) => { if (type === 'message') listeners.add(fn); };
  globalThis.removeEventListener = (type, fn) => { if (type === 'message') listeners.delete(fn); };
  globalThis.document = { addEventListener() {} };
  t.after(() => {
    globalThis.parent = prevParent;
    globalThis.addEventListener = prevAdd;
    globalThis.removeEventListener = prevRemove;
    globalThis.document = prevDocument;
    if (prevNavigator !== undefined) Object.defineProperty(globalThis, 'navigator', { configurable: true, value: prevNavigator });
    globalThis.fetch = prevFetch;
    globalThis.open = prevOpen;
    globalThis.__DSH_TRANSPORT__ = prevTransport;
  });

  const emitFromParent = data => {
    for (const fn of listeners) fn({ source: globalThis.parent, data });
  };
  const bridge = createHostBridge({});
  bridge.installBrowserHooks();
  assert.equal(typeof globalThis.__DSH_TRANSPORT__.createWebSocket, 'function');
  assert.equal(globalThis.__DSH_TRANSPORT__.openStream, undefined);

  emitFromParent({ channel: 'magi-plugin-v1', instanceId: 'inst-1', type: 'init' });
  const ws = globalThis.__DSH_TRANSPORT__.createWebSocket('ws://dsh.local/api/remote.mux');
  assert.equal(ws.readyState, 0);
  const openMsg = posted.find(m => m.type === 'ws.open');
  assert.ok(openMsg);
  assert.equal(openMsg.path, '/api/remote.mux');

  const events = [];
  ws.addEventListener('open', () => events.push('open'));
  ws.addEventListener('message', ev => events.push(`msg:${ev.data}`));
  ws.addEventListener('close', ev => events.push(`close:${ev.code}`));

  emitFromParent({ channel: 'magi-plugin-v1', instanceId: 'inst-1', type: 'ws.open', socketId: openMsg.socketId });
  assert.equal(ws.readyState, 1);
  ws.send('{"type":"ping"}');
  assert.deepEqual(
    posted.find(m => m.type === 'ws.send'),
    { channel: 'magi-plugin-v1', instanceId: 'inst-1', type: 'ws.send', socketId: openMsg.socketId, data: '{"type":"ping"}' },
  );
  emitFromParent({ channel: 'magi-plugin-v1', instanceId: 'inst-1', type: 'ws.message', socketId: openMsg.socketId, data: '{"type":"pong"}' });
  ws.close(1000, 'bye');
  assert.equal(ws.readyState, 2);
  emitFromParent({ channel: 'magi-plugin-v1', instanceId: 'inst-1', type: 'ws.close', socketId: openMsg.socketId, code: 1000, reason: 'bye' });
  assert.equal(ws.readyState, 3);
  assert.deepEqual(events, ['open', 'msg:{"type":"pong"}', 'close:1000']);
});

test('the real resources package provides contract states, not a host-invented status', async () => {
  const { ctx, roots } = harness();
  const calls = [];
  // Resources also registers a slot factory; record it without rendering.
  ctx.slots.registerFactory = (...args) => { calls.push(args); return () => {}; };
  const resources = requireModule('@deepseek-ai/dsh-client-resources');
  await ctx.plugin(resources.default ?? resources).await();
  const snapshot = ctx.resources.source('dsh-resource://file/example').getSnapshot();
  assert.ok(['none', 'loading', 'live', 'failed'].includes(snapshot.status), snapshot.status);
  assert.ok(roots.some(root => root.hooks || root.keyedHooks || root.props), 'the package publishes its own standard hook');
});
