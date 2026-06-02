import assert from 'node:assert/strict';
import { createServer } from 'vite';

const WORKSPACE_ID = 'workspace-bridge-live-adopt';
const WORKSPACE_PATH = '/tmp/workspace-bridge-live-adopt';
const PARTIAL_WORKSPACE_ID = 'workspace-bridge-partial-scope';
const SESSION_ID = 'session-bridge-live-adopt';
const TURN_ID = 'turn-bridge-live-adopt';
const USER_ITEM_ID = 'user-bridge-live-adopt';
const ACCEPTED_AT = 1780390000000;
let acceptedPublished = false;
let terminalPublished = false;
let summaryMessageCount = 1;
let summaryUpdatedAt = ACCEPTED_AT;
const capturedTurnBodies = [];

class MemoryStorage {
  constructor() {
    this.values = new Map();
  }

  getItem(key) {
    return this.values.has(key) ? this.values.get(key) : null;
  }

  setItem(key, value) {
    this.values.set(String(key), String(value));
  }

  removeItem(key) {
    this.values.delete(String(key));
  }
}

class FakeEventSource {
  static instances = [];

  constructor(url) {
    this.url = url;
    this.closed = false;
    this.onopen = null;
    this.onmessage = null;
    this.onerror = null;
    FakeEventSource.instances.push(this);
  }

  close() {
    this.closed = true;
  }
}

class FakeCustomEvent extends Event {
  constructor(type, options = {}) {
    super(type);
    this.detail = options.detail;
  }
}

function installBrowserGlobals() {
  const target = new EventTarget();
  const storage = new MemoryStorage();
  const activeTimeouts = new Set();
  const activeIntervals = new Set();
  const trackedSetTimeout = (handler, timeout, ...args) => {
    const timeoutId = setTimeout(() => {
      activeTimeouts.delete(timeoutId);
      handler(...args);
    }, timeout);
    activeTimeouts.add(timeoutId);
    return timeoutId;
  };
  const trackedClearTimeout = (timeoutId) => {
    activeTimeouts.delete(timeoutId);
    clearTimeout(timeoutId);
  };
  const trackedSetInterval = (handler, timeout, ...args) => {
    const interval = setInterval(handler, timeout, ...args);
    activeIntervals.add(interval);
    return interval;
  };
  const trackedClearInterval = (interval) => {
    activeIntervals.delete(interval);
    clearInterval(interval);
  };
  const windowObject = {
    location: {
      href: `http://127.0.0.1:38123/web.html?workspaceId=${encodeURIComponent(WORKSPACE_ID)}&workspacePath=${encodeURIComponent(WORKSPACE_PATH)}`,
    },
    history: {
      state: null,
      replaceState(state, _title, url) {
        this.state = state;
        windowObject.location.href = String(url);
      },
    },
    localStorage: storage,
    setTimeout: trackedSetTimeout,
    clearTimeout: trackedClearTimeout,
    setInterval: trackedSetInterval,
    clearInterval: trackedClearInterval,
    __clearGoldenTimers() {
      for (const timeoutId of Array.from(activeTimeouts)) {
        trackedClearTimeout(timeoutId);
      }
      for (const interval of Array.from(activeIntervals)) {
        trackedClearInterval(interval);
      }
    },
    addEventListener: target.addEventListener.bind(target),
    removeEventListener: target.removeEventListener.bind(target),
    dispatchEvent: target.dispatchEvent.bind(target),
  };

  globalThis.window = windowObject;
  globalThis.localStorage = storage;
  globalThis.EventSource = FakeEventSource;
  globalThis.CustomEvent = FakeCustomEvent;
}

function jsonResponse(payload) {
  return new Response(JSON.stringify(payload), {
    status: 200,
    headers: {
      'content-type': 'application/json',
    },
  });
}

function bootstrapPayload() {
  const sessions = acceptedPublished
    ? [
        {
          sessionId: SESSION_ID,
          title: '桥接层实时会话',
          createdAt: ACCEPTED_AT,
          updatedAt: summaryUpdatedAt,
          messageCount: summaryMessageCount,
          workspaceId: WORKSPACE_ID,
        },
      ]
    : [];
  const canonicalTurns = terminalPublished ? [completedCanonicalTurn()] : [];
  const currentSession = acceptedPublished ? sessions[0] : null;
  return {
    workspace: {
      workspaceId: WORKSPACE_ID,
      rootPath: WORKSPACE_PATH,
    },
    currentSession,
    sessions,
    state: {
      currentSessionId: acceptedPublished ? SESSION_ID : '',
      currentWorkspaceId: WORKSPACE_ID,
      currentWorkspacePath: WORKSPACE_PATH,
      sessions,
      isProcessing: false,
      processingState: null,
      messages: [],
      edits: [],
      changedFiles: [],
      pendingChanges: [],
      pendingChangesState: null,
    },
    canonicalTurns,
    notifications: {
      notifications: [],
    },
    eventStreamNextSequence: 1,
    agent: {
      runtimeEpoch: 'bridge-golden-runtime',
    },
  };
}

function settingsBootstrapPayload() {
  return {
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: '',
    runtimeSettings: {
      locale: 'zh-CN',
    },
    workerConfigs: {},
    orchestratorConfig: {},
    auxiliaryConfig: {},
    userRulesConfig: {},
    skillsConfig: {},
    safeguardConfig: {},
    repositories: [],
    mcpServers: [],
    builtinTools: [],
    capabilityDependencies: [],
    workerStatuses: {},
    roleTemplates: [],
    registryEngines: [],
    registryAgents: [],
    bootstrapScope: 'core',
    mcpServersHydrated: true,
  };
}

function installFetchStub() {
  globalThis.fetch = async (url, init = {}) => {
    const parsed = new URL(String(url));
    if (parsed.pathname === '/health') {
      return new Response('ok', { status: 200 });
    }
    if (parsed.pathname === '/api/session/turn') {
      capturedTurnBodies.push(JSON.parse(String(init.body || '{}')));
      return jsonResponse({
        sessionId: 'session-bridge-partial-scope',
        entryId: 'timeline-bridge-partial-scope',
        eventId: 'event-bridge-partial-scope',
        acceptedAt: ACCEPTED_AT + capturedTurnBodies.length,
        createdSession: false,
        route: 'chat',
        userMessageItemId: 'user-bridge-partial-scope',
        canonicalSchemaVersion: null,
        canonicalEventKind: null,
        canonicalTurn: null,
        canonicalItem: null,
      });
    }
    if (parsed.pathname === '/bootstrap') {
      return jsonResponse(bootstrapPayload());
    }
    if (parsed.pathname === '/api/settings/bootstrap') {
      return jsonResponse(settingsBootstrapPayload());
    }
    if (parsed.pathname === '/api/settings/registry/role-templates') {
      return jsonResponse({ roleTemplates: [] });
    }
    if (parsed.pathname === '/api/settings/registry/engines') {
      return jsonResponse({ engines: [] });
    }
    if (parsed.pathname === '/api/settings/registry/agents') {
      return jsonResponse({ agents: [] });
    }
    return new Response('not found', { status: 404 });
  };
}

function imageMetadata() {
  return {
    images: [
      {
        name: 'bridge-live.png',
        dataUrl: 'data:image/png;base64,AAA',
      },
    ],
  };
}

function acceptedCanonicalUserItem() {
  return {
    sessionId: SESSION_ID,
    turnId: TURN_ID,
    turnSeq: ACCEPTED_AT,
    itemId: USER_ITEM_ID,
    itemSeq: 1,
    kind: 'user_message',
    createdAt: ACCEPTED_AT,
    status: 'completed',
    updatedAt: ACCEPTED_AT,
    content: '桥接层实时图片消息。',
    sourceThreadId: `thread-${SESSION_ID}`,
    visibility: {
      renderable: true,
    },
    metadata: imageMetadata(),
  };
}

function completedCanonicalAssistantItem() {
  return {
    sessionId: SESSION_ID,
    turnId: TURN_ID,
    turnSeq: ACCEPTED_AT,
    itemId: 'assistant-bridge-live-terminal',
    itemSeq: 2,
    kind: 'assistant_text',
    createdAt: ACCEPTED_AT,
    status: 'completed',
    updatedAt: ACCEPTED_AT + 2000,
    content: '桥接层实时回复已完成。',
    sourceThreadId: `thread-${SESSION_ID}`,
    visibility: {
      renderable: true,
    },
    metadata: {},
  };
}

function completedCanonicalTurn() {
  return {
    sessionId: SESSION_ID,
    turnId: TURN_ID,
    turnSeq: ACCEPTED_AT,
    acceptedAt: ACCEPTED_AT,
    completedAt: ACCEPTED_AT + 2000,
    status: 'completed',
    responseDurationMs: 2000,
    items: [
      acceptedCanonicalUserItem(),
      completedCanonicalAssistantItem(),
    ],
  };
}

function acceptedEnvelope() {
  const canonicalItem = acceptedCanonicalUserItem();
  return {
    event_id: `event-session-turn-accepted-${ACCEPTED_AT}`,
    event_type: 'session.turn.accepted',
    category: 'domain',
    occurred_at: ACCEPTED_AT,
    sequence: 2,
    workspace_id: WORKSPACE_ID,
    session_id: SESSION_ID,
    payload: {
      session_id: SESSION_ID,
      workspace_id: WORKSPACE_ID,
      created_session: true,
      route: 'chat',
      canonical_schema_version: 'canonical-turn.v1',
      canonical_event_kind: 'turn_started',
      canonical_turn: {
        sessionId: SESSION_ID,
        turnId: TURN_ID,
        turnSeq: ACCEPTED_AT,
        acceptedAt: ACCEPTED_AT,
        status: 'running',
        items: [canonicalItem],
      },
      canonical_item: canonicalItem,
    },
  };
}

function completedEnvelope() {
  const canonicalTurn = completedCanonicalTurn();
  const canonicalItem = completedCanonicalAssistantItem();
  return {
    event_id: `event-session-turn-completed-${ACCEPTED_AT}`,
    event_type: 'session.turn.completed',
    category: 'domain',
    occurred_at: ACCEPTED_AT + 2000,
    sequence: 4,
    workspace_id: WORKSPACE_ID,
    session_id: SESSION_ID,
    payload: {
      session_id: SESSION_ID,
      workspace_id: WORKSPACE_ID,
      route: 'chat',
      created_session: true,
      canonical_schema_version: 'canonical-turn.v1',
      canonical_event_kind: 'turn_completed',
      canonical_turn: canonicalTurn,
      canonical_item: canonicalItem,
    },
  };
}

function messageCreatedEnvelope() {
  return {
    event_id: `event-message-created-${summaryUpdatedAt}`,
    event_type: 'message.created',
    category: 'domain',
    occurred_at: summaryUpdatedAt,
    sequence: 3,
    workspace_id: WORKSPACE_ID,
    session_id: SESSION_ID,
    payload: {
      session_id: SESSION_ID,
      workspace_id: WORKSPACE_ID,
      role: 'user',
      content: '同一会话远端追加消息。',
    },
  };
}

function laggedEnvelope(sequence = 5) {
  return {
    event_id: `event-stream-lagged-${sequence}`,
    event_type: 'event.stream.lagged',
    category: 'system',
    occurred_at: ACCEPTED_AT + 3000,
    sequence,
    workspace_id: WORKSPACE_ID,
    payload: {
      skipped: 7,
      recovery: 'bootstrap',
      reason: 'broadcast_lagged',
    },
  };
}

async function waitFor(predicate, label) {
  return waitForWithin(predicate, label, 2000);
}

async function waitForWithin(predicate, label, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  assert.fail(label);
}

function findArtifactByTurnItemId(projection, itemId) {
  return projection?.artifacts?.find((artifact) => artifact.message?.metadata?.turnItemId === itemId);
}

function currentSessionSummary(messagesStore) {
  return messagesStore.messagesState.sessions.find((session) => session.id === SESSION_ID);
}

installBrowserGlobals();
installFetchStub();

const server = await createServer({
  root: process.cwd(),
  configFile: 'vite.web.config.ts',
  logLevel: 'silent',
  server: { middlewareMode: true },
});

try {
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  const bridgeModule = await server.ssrLoadModule('/src/shared/bridges/web-client-bridge.ts');
  const messageHandler = await server.ssrLoadModule('/src/lib/message-handler.ts');
  const messagesStore = await server.ssrLoadModule('/src/stores/messages.svelte.ts');

  const bridge = bridgeModule.createWebClientBridge();
  bridgeRuntime.setClientBridge(bridge);
  messagesStore.initializeState();
  messageHandler.primeEventSeqTracking(messagesStore.messagesState.currentSessionId, messagesStore.messagesState.currentWorkspaceId);
  messageHandler.initMessageHandler(bridge);
  bridge.notifyReady();

  await waitFor(() => FakeEventSource.instances.length > 0, 'bootstrap must connect workspace-scoped SSE');
  const stream = FakeEventSource.instances[0];
  assert.ok(
    stream.url.includes(`/events?workspaceId=${encodeURIComponent(WORKSPACE_ID)}`),
    'event stream must subscribe by workspace scope',
  );
  stream.onopen?.();

  stream.onerror?.();
  stream.onmessage?.({ data: JSON.stringify(acceptedEnvelope()) });
  assert.notEqual(
    messagesStore.messagesState.currentSessionId,
    SESSION_ID,
    'closed stale SSE callbacks must not mutate the current workspace/session binding',
  );
  window.dispatchEvent(new Event('focus'));

  await waitForWithin(
    () => FakeEventSource.instances.length > 1,
    'immediate recovery trigger must preempt delayed SSE reconnect timer',
    350,
  );
  const recoveredStream = FakeEventSource.instances.at(-1);
  recoveredStream.onopen?.();

  recoveredStream.onmessage?.({ data: JSON.stringify(acceptedEnvelope()) });
  acceptedPublished = true;
  await waitFor(
    () => messagesStore.messagesState.currentSessionId === SESSION_ID,
    'workspace-only page must adopt live accepted session',
  );

  const artifact = findArtifactByTurnItemId(
    messagesStore.messagesState.canonicalTimelineProjection,
    USER_ITEM_ID,
  );
  assert.deepEqual(
    artifact?.message?.images,
    [
      {
        name: 'bridge-live.png',
        dataUrl: 'data:image/png;base64,AAA',
      },
    ],
    'live SSE accepted image must project into the message area without refresh',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'running accepted canonical turn must mark the UI as processing',
  );
  assert.ok(
    window.location.href.includes(`sessionId=${encodeURIComponent(SESSION_ID)}`),
    'live accepted session must update the browser binding',
  );
  await waitFor(
    () => messagesStore.messagesState.sessions.some((session) => session.id === SESSION_ID),
    'created live session must refresh the workspace session summary',
  );

  summaryMessageCount = 2;
  summaryUpdatedAt = ACCEPTED_AT + 1000;
  recoveredStream.onmessage?.({ data: JSON.stringify(messageCreatedEnvelope()) });
  await waitFor(
    () => currentSessionSummary(messagesStore)?.messageCount === 2,
    'current-session message events must refresh the workspace session summary without waiting for a full page reload',
  );

  terminalPublished = true;
  recoveredStream.onmessage?.({ data: JSON.stringify(completedEnvelope()) });
  await waitFor(
    () => messagesStore.messagesState.isProcessing === false,
    'terminal session turn event with canonical payload must settle processing without waiting for recovery',
  );
  const terminalArtifact = findArtifactByTurnItemId(
    messagesStore.messagesState.canonicalTimelineProjection,
    'assistant-bridge-live-terminal',
  );
  assert.equal(
    terminalArtifact?.message?.isStreaming,
    false,
    'terminal session turn event must project the final assistant item as non-streaming',
  );

  const streamCountBeforeLagged = FakeEventSource.instances.length;
  const originalWarn = console.warn;
  try {
    console.warn = () => {};
    recoveredStream.onmessage?.({ data: JSON.stringify(laggedEnvelope()) });
    await waitForWithin(
      () => FakeEventSource.instances.length > streamCountBeforeLagged,
      'lagged SSE event must immediately rebuild the event stream through bootstrap recovery',
      350,
    );
  } finally {
    console.warn = originalWarn;
  }
  assert.equal(
    recoveredStream.closed,
    true,
    'lagged SSE event must close the stale stream before recovery reconnects',
  );
  const laggedRecoveredStream = FakeEventSource.instances.at(-1);
  laggedRecoveredStream.onopen?.();
  await waitFor(
    () => messagesStore.messagesState.currentSessionId === SESSION_ID,
    'lagged recovery bootstrap must keep the current session binding',
  );
  const recoveredTerminalArtifact = findArtifactByTurnItemId(
    messagesStore.messagesState.canonicalTimelineProjection,
    'assistant-bridge-live-terminal',
  );
  assert.equal(
    recoveredTerminalArtifact?.message?.content,
    '桥接层实时回复已完成。',
    'lagged recovery bootstrap must preserve the authoritative terminal transcript',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    false,
    'lagged recovery bootstrap must keep processing settled after terminal transcript restore',
  );

  bridge.postMessage({
    type: 'executeTask',
    text: '验证 partial workspace scope',
    workspaceId: PARTIAL_WORKSPACE_ID,
    requestId: 'request-bridge-partial-scope',
  });
  await waitFor(() => capturedTurnBodies.length === 1, 'partial workspace submit must reach backend');
  assert.equal(capturedTurnBodies[0].workspaceId, PARTIAL_WORKSPACE_ID);
  assert.equal(
    capturedTurnBodies[0].workspacePath,
    null,
    'workspaceId-only bridge submit must not inherit stale current workspacePath',
  );
  assert.equal(
    capturedTurnBodies[0].sessionId,
    null,
    'workspaceId-only bridge submit must not inherit stale current sessionId',
  );

  console.log('web client bridge golden replay passed');
} finally {
  window.__clearGoldenTimers?.();
  await server.close();
}
