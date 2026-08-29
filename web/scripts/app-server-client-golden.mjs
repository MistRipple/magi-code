import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

class FakeWebSocket {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  static instances = [];

  constructor(url) {
    this.url = url;
    this.readyState = FakeWebSocket.CONNECTING;
    this.sent = [];
    this.listeners = new Map();
    this.closed = false;
    this.throwOnSend = false;
    FakeWebSocket.instances.push(this);
  }

  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }

  emit(type, event = {}) {
    for (const listener of this.listeners.get(type) ?? []) {
      listener({ type, target: this, ...event });
    }
  }

  open() {
    this.readyState = FakeWebSocket.OPEN;
    this.emit('open');
  }

  send(value) {
    if (this.throwOnSend) {
      throw new Error('fake send failed');
    }
    if (this.readyState !== FakeWebSocket.OPEN) {
      throw new Error('fake socket is not open');
    }
    this.sent.push(JSON.parse(value));
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.readyState = FakeWebSocket.CLOSED;
    this.emit('close');
  }

  lateClose() {
    this.emit('close');
  }

  message(value) {
    this.emit('message', { data: JSON.stringify(value) });
  }
}

function installBrowserGlobals() {
  globalThis.WebSocket = FakeWebSocket;
  globalThis.window = {
    location: { href: 'http://127.0.0.1:38123/web.html' },
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
  };
}

function lastRequest(socket, method) {
  return [...socket.sent].reverse().find((message) => message.method === method);
}

function reply(socket, request, result) {
  socket.message({ jsonrpc: '2.0', id: request.id, result });
}

function initializeResult(runtimeEpoch = 'runtime-golden-1') {
  return {
    runtimeEpoch,
    serverInfo: { name: 'magi-app-server', version: 'golden' },
    protocol: { major: 1, minor: 0 },
    capabilities: { streaming: true },
  };
}

function subscribeResult(runtimeEpoch = 'runtime-golden-1', nextSequence = 1) {
  return {
    runtimeEpoch,
    subscribed: true,
    nextSequence,
    resyncRequired: false,
  };
}

async function connectReady(AppServerClient, options = {}) {
  const client = new AppServerClient({
    endpoint: 'http://127.0.0.1:38123/api/app-server',
    clientInfo: { name: 'golden-client', title: 'Golden Client', version: 'test' },
    requestTimeoutMs: 500,
    reconnect: false,
    ...options,
  });
  const connected = client.connect();
  const socket = FakeWebSocket.instances.at(-1);
  assert.ok(socket, 'connect must create a WebSocket');
  socket.open();
  await Promise.resolve();
  const initialize = lastRequest(socket, 'initialize');
  assert.ok(initialize, 'open must send initialize');
  reply(socket, initialize, initializeResult());
  await Promise.resolve();
  if (options.subscription) {
    const subscribe = lastRequest(socket, 'events/subscribe');
    assert.ok(subscribe, 'initialize must be followed by events/subscribe');
    reply(socket, subscribe, subscribeResult());
  }
  await connected;
  assert.equal(client.state, 'ready');
  return { client, socket };
}

installBrowserGlobals();

await withGoldenViteServer(async (server) => {
  const { AppServerClient } = await server.ssrLoadModule('/src/shared/app-server-client.ts');

  {
    const client = new AppServerClient({
      endpoint: 'http://127.0.0.1:38123/api/app-server',
      clientInfo: { name: 'golden-client', title: 'Golden Client', version: 'test' },
      requestTimeoutMs: 500,
      reconnect: false,
    });
    const connecting = client.connect();
    const socket = FakeWebSocket.instances.at(-1);
    assert.ok(socket);
    client.close();
    await assert.rejects(connecting, /连接已关闭/);
    assert.equal(client.state, 'closed');
    assert.equal(socket.closed, true, 'close must close an opening socket');
  }

  {
    const { client, socket } = await connectReady(AppServerClient, {
      subscription: { sessionId: 'session-golden', afterSequence: 0 },
    });
    const snapshots = [];
    const events = [];
    const subscribedClient = new AppServerClient({
      endpoint: 'http://127.0.0.1:38123/api/app-server',
      clientInfo: { name: 'golden-client', title: 'Golden Client', version: 'test' },
      requestTimeoutMs: 500,
      reconnect: false,
      subscription: { sessionId: 'session-golden', afterSequence: 0 },
      onSnapshot: (snapshot) => snapshots.push(snapshot),
      onEvent: (event) => events.push(event),
    });
    // The helper above validates the subscription lifecycle. Close it before
    // exercising the explicit notification path with the instrumented client.
    client.close();
    const instrumented = subscribedClient.connect();
    const instrumentedSocket = FakeWebSocket.instances.at(-1);
    instrumentedSocket.open();
    await Promise.resolve();
    reply(instrumentedSocket, lastRequest(instrumentedSocket, 'initialize'), initializeResult());
    await Promise.resolve();
    reply(instrumentedSocket, lastRequest(instrumentedSocket, 'events/subscribe'), subscribeResult());
    await instrumented;
    instrumentedSocket.message({
      jsonrpc: '2.0',
      method: 'events/snapshot',
      params: { next_sequence: 4, recent_events: [] },
    });
    instrumentedSocket.message({
      jsonrpc: '2.0',
      method: 'event/session.turn.completed',
      params: {
        sequence: 4,
        event: {
          event_id: 'event-golden',
          event_type: 'session.turn.completed',
          category: 'session',
          occurred_at: 1,
          sequence: 4,
          payload: {},
        },
      },
    });
    assert.equal(subscribedClient.lastSequence, 4);
    assert.equal(snapshots.length, 1);
    assert.equal(events.length, 1);
    subscribedClient.close();
    assert.equal(socket.closed, true);
  }

  {
    const { client, socket } = await connectReady(AppServerClient);
    const requestPromise = client.request('session/list', {});
    await Promise.resolve();
    const request = lastRequest(socket, 'session/list');
    assert.ok(request);
    socket.close();
    await assert.rejects(requestPromise, /已断开/);
    client.close();
  }

  {
    const { client, socket } = await connectReady(AppServerClient);
    const controller = new AbortController();
    const requestPromise = client.request('session/list', {}, {
      timeoutMs: 100,
      signal: controller.signal,
    });
    await Promise.resolve();
    controller.abort();
    await assert.rejects(requestPromise, /已取消/);
    assert.equal(lastRequest(socket, '$/cancelRequest')?.params?.id !== undefined, true);

    const timeoutPromise = client.request('session/list', {}, { timeoutMs: 100 });
    await assert.rejects(timeoutPromise, /超时/);
    assert.equal(
      socket.sent.filter((message) => message.method === '$/cancelRequest').length,
      2,
      'abort and timeout must each send one cancellation notification',
    );
    client.close();
  }

  {
    const { client, socket: oldSocket } = await connectReady(AppServerClient, {
      reconnect: true,
      reconnectBaseDelayMs: 0,
      reconnectMaxDelayMs: 0,
    });
    const requestPromise = client.request('session/list', {});
    await Promise.resolve();
    const request = lastRequest(oldSocket, 'session/list');
    assert.ok(request);
    oldSocket.close();
    await assert.rejects(requestPromise, /已断开/);

    await new Promise((resolve) => setTimeout(resolve, 10));
    const newSocket = FakeWebSocket.instances.at(-1);
    assert.notEqual(newSocket, oldSocket, 'reconnect must create a new socket');
    newSocket.open();
    await Promise.resolve();
    const initialize = lastRequest(newSocket, 'initialize');
    assert.ok(initialize);
    // A stale response from the old generation must not affect the new one.
    oldSocket.message({ jsonrpc: '2.0', id: request.id, result: { sessions: [], runtimeEpoch: 'stale' } });
    reply(newSocket, initialize, initializeResult('runtime-golden-2'));
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(client.state, 'ready');
    oldSocket.lateClose();
    assert.equal(client.state, 'ready', 'a late close from an old socket must be ignored');
    client.close();
  }

  {
    const { client, socket } = await connectReady(AppServerClient);
    const listPromise = client.request('browser/tools/list', {
      sessionId: 'session-golden',
    });
    await Promise.resolve();
    const listRequest = lastRequest(socket, 'browser/tools/list');
    assert.ok(listRequest);
    assert.deepEqual(listRequest.params, { sessionId: 'session-golden' });
    reply(socket, listRequest, {
      tools: [
        {
          name: 'browser_navigate',
          access: 'write',
          description: '导航、后退、前进或刷新当前浏览器页面。',
          inputSchema: { type: 'object', properties: { url: { type: 'string' } } },
        },
        {
          name: 'browser_snapshot',
          access: 'read',
          description: '读取当前页面的可交互 DOM 与辅助功能快照。',
          inputSchema: { type: 'object', properties: {} },
        },
      ],
      capabilities: {
        revision: 7,
        inAppBrowserEnabled: true,
        browserUseEnabled: true,
        hostStatus: 'ready',
        hostProtocolCompatible: true,
        accessProfile: 'full_access',
      },
      runtimeEpoch: 'runtime-golden-browser-1',
    });
    const listResult = await listPromise;
    assert.deepEqual(listResult.tools.map((tool) => tool.name), ['browser_navigate', 'browser_snapshot']);
    assert.equal(listResult.tools[0].description, '导航、后退、前进或刷新当前浏览器页面。');
    assert.equal(listResult.tools[0].inputSchema.properties.url.type, 'string');
    assert.equal(listResult.capabilities.hostStatus, 'ready');
    assert.equal(listResult.runtimeEpoch, 'runtime-golden-browser-1');

    const toolParams = {
      sessionId: 'session-golden',
      tool: 'browser_navigate',
      arguments: { url: 'https://www.baidu.com/' },
      workspaceId: null,
      callId: 'browser-call-golden',
      browserCapabilityRevision: 7,
    };
    const toolPromise = client.request('browser/tool', toolParams);
    await Promise.resolve();
    const toolRequest = lastRequest(socket, 'browser/tool');
    assert.ok(toolRequest);
    assert.deepEqual(toolRequest.params, toolParams, 'browser/tool 必须完整携带 session、tool 和结构化 arguments');
    reply(socket, toolRequest, {
      tool: 'browser_navigate',
      status: 'completed',
      payload: { url: 'https://www.baidu.com/', title: '百度一下，你就知道' },
      itemId: 'item-browser-golden',
      turnId: 'turn-browser-golden',
      runtimeEpoch: 'runtime-golden-browser-1',
    });
    const toolResult = await toolPromise;
    assert.equal(toolResult.tool, toolParams.tool);
    assert.equal(toolResult.status, 'completed');
    assert.equal(toolResult.itemId, 'item-browser-golden');
    assert.equal(toolResult.turnId, 'turn-browser-golden');
    assert.equal(toolResult.runtimeEpoch, 'runtime-golden-browser-1');
    assert.equal(toolResult.payload.title, '百度一下，你就知道');
    client.close();
  }

  {
    const { client, socket } = await connectReady(AppServerClient);
    let serverRequestContext;
    const requestClient = new AppServerClient({
      endpoint: 'http://127.0.0.1:38123/api/app-server',
      clientInfo: { name: 'golden-client', title: 'Golden Client', version: 'test' },
      requestTimeoutMs: 500,
      reconnect: false,
      onServerRequest: (context) => {
        serverRequestContext = context;
      },
    });
    const connecting = requestClient.connect();
    const requestSocket = FakeWebSocket.instances.at(-1);
    requestSocket.open();
    await Promise.resolve();
    reply(requestSocket, lastRequest(requestSocket, 'initialize'), initializeResult());
    await connecting;
    requestSocket.message({
      jsonrpc: '2.0',
      id: 'approval-golden',
      method: 'approval/request',
      params: { sessionId: 'session-golden', reason: 'golden test' },
    });
    assert.ok(serverRequestContext);
    requestSocket.close();
    serverRequestContext.respond({ approved: true });
    assert.equal(
      requestSocket.sent.some((message) => message.id === 'approval-golden'),
      false,
      'a response after connection close must not be written to the old socket',
    );
    requestClient.close();
    client.close();
  }

  {
    const client = new AppServerClient({
      endpoint: 'http://127.0.0.1:38123/api/app-server',
      clientInfo: { name: 'golden-client', title: 'Golden Client', version: 'test' },
      requestTimeoutMs: 500,
      reconnect: false,
    });
    const connecting = client.connect();
    const socket = FakeWebSocket.instances.at(-1);
    socket.open();
    socket.throwOnSend = true;
    await assert.rejects(connecting, /fake send failed/);
    assert.equal(client.state, 'closed');
    client.close();
  }

  console.log('app-server client golden replay passed');
}, { configFile: 'vite.web.config.ts' });
