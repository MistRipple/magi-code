import assert from 'node:assert/strict';
import { createServer } from 'vite';

const WORKSPACE_ID = 'workspace-bridge-live-adopt';
const WORKSPACE_PATH = '/tmp/workspace-bridge-live-adopt';
const SESSION_ID = 'session-bridge-live-adopt';
const TURN_ID = 'turn-bridge-live-adopt';
const USER_ITEM_ID = 'user-bridge-live-adopt';
const ACCEPTED_AT = 1780390000000;
let acceptedPublished = false;

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
  const activeIntervals = new Set();
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
    setTimeout,
    clearTimeout,
    setInterval: trackedSetInterval,
    clearInterval: trackedClearInterval,
    __clearGoldenTimers() {
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
          updatedAt: ACCEPTED_AT,
          messageCount: 1,
          workspaceId: WORKSPACE_ID,
        },
      ]
    : [];
  return {
    workspace: {
      workspaceId: WORKSPACE_ID,
      rootPath: WORKSPACE_PATH,
    },
    sessionId: acceptedPublished ? SESSION_ID : '',
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
    canonicalTurns: [],
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
  globalThis.fetch = async (url) => {
    const parsed = new URL(String(url));
    if (parsed.pathname === '/health') {
      return new Response('ok', { status: 200 });
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

function acceptedEnvelope() {
  const imageMetadata = {
    images: [
      {
        name: 'bridge-live.png',
        dataUrl: 'data:image/png;base64,AAA',
      },
    ],
  };
  const canonicalItem = {
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
    metadata: imageMetadata,
  };
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

async function waitFor(predicate, label) {
  const deadline = Date.now() + 2000;
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

  stream.onmessage?.({ data: JSON.stringify(acceptedEnvelope()) });
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
  assert.ok(
    window.location.href.includes(`sessionId=${encodeURIComponent(SESSION_ID)}`),
    'live accepted session must update the browser binding',
  );
  await waitFor(
    () => messagesStore.messagesState.sessions.some((session) => session.id === SESSION_ID),
    'created live session must refresh the workspace session summary',
  );

  console.log('web client bridge golden replay passed');
} finally {
  window.__clearGoldenTimers?.();
  await server.close();
}
