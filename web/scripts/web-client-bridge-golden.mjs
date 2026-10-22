import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const dataMessageHandlersSource = await readFile(
  new URL('../src/lib/data-message-handlers.ts', import.meta.url),
  'utf8',
);
const webClientBridgeSource = await readFile(
  new URL('../src/shared/bridges/web-client-bridge.ts', import.meta.url),
  'utf8',
);
const messageProtocolSource = await readFile(
  new URL('../src/shared/protocol/message-protocol.ts', import.meta.url),
  'utf8',
);
assert.match(
  dataMessageHandlersSource,
  /case 'sessionBootstrapLoaded':[\s\S]*?handleSessionBootstrapLoaded\(asMessage\(\{[\s\S]*?scope: payload\.scope,/,
  'session bootstrap dispatch must preserve workspace or personal scope for navigation settlement',
);
assert.match(
  webClientBridgeSource,
  /clearWorkspaceSessionBinding\([\s\S]*?await fetchBootstrap\(\{[\s\S]*?forceFresh: true,[\s\S]*?\}\);[\s\S]*?return;/,
  'missing persisted session must restart bootstrap after clearing the stale binding',
);
assert.match(
  webClientBridgeSource,
  /captureProcessingRequestSnapshot[\s\S]*?settleProcessingRequestSnapshot[\s\S]*?processingRequestIds/,
  'bootstrap and interrupt recovery must use an immutable request snapshot',
);
assert.match(
  webClientBridgeSource,
  /const text = typeof input\.text === 'string' \? input\.text\.trimEnd\(\) : null;/,
  'submitted message text must drop trailing contenteditable whitespace before local and canonical projection',
);
assert.doesNotMatch(
  messageProtocolSource,
  /TASK_(?:STARTED|COMPLETED|FAILED)\s*=\s*['"]task_(?:started|completed|failed)['"]|task_(?:started|completed|failed)/,
  'obsolete task lifecycle control messages must have no protocol type or consumer',
);

const WORKSPACE_ID = 'workspace-bridge-live-adopt';
const WORKSPACE_PATH = '/tmp/workspace-bridge-live-adopt';
const PARTIAL_WORKSPACE_ID = 'workspace-bridge-partial-scope';
const RACE_WORKSPACE_ID = 'workspace-bridge-bootstrap-race';
const RACE_WORKSPACE_PATH = '/tmp/workspace-bridge-bootstrap-race';
const RACE_SESSION_ID = 'session-bridge-bootstrap-race';
const NAVIGATION_BARRIER_SESSION_ID = 'session-bridge-navigation-barrier';
const SESSION_ID = 'session-bridge-live-adopt';
const TURN_ID = 'turn-bridge-live-adopt';
const USER_ITEM_ID = 'user-bridge-live-adopt';
const ACCEPTED_AT = 1780390000000;
const RUNTIME_EPOCH = 'bridge-golden-runtime';
const EVENT_STREAM_NEXT_SEQUENCE = 10;
let acceptedPublished = false;
let terminalPublished = false;
let summaryMessageCount = 1;
let summaryUpdatedAt = ACCEPTED_AT;
let workspaceListPayload = null;
const capturedTurnBodies = [];
const capturedGuideQueueBodies = [];
const capturedRemoveQueueBodies = [];
const bootstrapInterceptors = [];
const sessionNavigationInterceptors = [];
const workspaceSessionsInterceptors = [];
const sessionQueueInterceptors = [];
const bridgeMutationRequests = [];
const bridgeMutationInterceptors = [];
const sessionTurnInterceptors = [];
let messagesSnapshotRequestCount = 0;
let sessionNavigationRequestCount = 0;
let bootstrapRequestCount = 0;
let workspaceSessionsRequestCount = 0;
let workspaceSessionIsRunning = false;
let workspaceSessionsPayloadOverride = null;
let pendingChangesRequestCount = 0;
let pendingChangesPayloadOverride = null;
let queuedTurnPayloads = [];
let interruptRequestCount = 0;
const goalRefreshQueries = [];

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

  message(value) {
    this.emit('message', { data: JSON.stringify(value) });
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
  const activeIntervals = new Map();
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
    activeIntervals.set(interval, { handler, args });
    return interval;
  };
  const trackedClearInterval = (interval) => {
    activeIntervals.delete(interval);
    clearInterval(interval);
  };
  const windowObject = {
    location: {
      href: `http://127.0.0.1:38123/web.html?scope=workspace&workspaceId=${encodeURIComponent(WORKSPACE_ID)}&workspacePath=${encodeURIComponent(WORKSPACE_PATH)}`,
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
      for (const interval of Array.from(activeIntervals.keys())) {
        trackedClearInterval(interval);
      }
    },
    __runGoldenIntervals() {
      for (const { handler, args } of Array.from(activeIntervals.values())) {
        handler(...args);
      }
    },
    addEventListener: target.addEventListener.bind(target),
    removeEventListener: target.removeEventListener.bind(target),
    dispatchEvent: target.dispatchEvent.bind(target),
  };

  globalThis.window = windowObject;
  globalThis.localStorage = storage;
  globalThis.EventSource = FakeEventSource;
  globalThis.WebSocket = FakeWebSocket;
  globalThis.CustomEvent = FakeCustomEvent;
}

function lastWebSocketRequest(socket, method) {
  return [...socket.sent].reverse().find((message) => message.method === method);
}

function replyWebSocket(socket, request, result) {
  socket.message({ jsonrpc: '2.0', id: request.id, result });
}

function appServerInitializeResult() {
  return {
    runtimeEpoch: RUNTIME_EPOCH,
    serverInfo: { name: 'magi-app-server', version: 'golden' },
    protocol: { major: 1, minor: 0 },
    capabilities: { streaming: true },
  };
}

function appServerSubscribeResult() {
  return {
    runtimeEpoch: RUNTIME_EPOCH,
    subscribed: true,
    nextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    resyncRequired: false,
  };
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
    scope: 'workspace',
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
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    canonicalEventNextSequence: terminalPublished ? 3 : acceptedPublished ? 2 : 1,
    agent: {
      runtimeEpoch: RUNTIME_EPOCH,
    },
  };
}

function scopedBootstrapPayload(workspaceId, workspacePath, sessionId, title) {
  const session = {
    sessionId,
    title,
    createdAt: ACCEPTED_AT + 2000,
    updatedAt: ACCEPTED_AT + 2000,
    messageCount: 0,
    workspaceId,
  };
  return {
    scope: 'workspace',
    workspace: {
      workspaceId,
      rootPath: workspacePath,
    },
    currentSession: session,
    sessions: [session],
    state: {
      currentSessionId: sessionId,
      currentWorkspaceId: workspaceId,
      currentWorkspacePath: workspacePath,
      sessions: [session],
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
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    canonicalEventNextSequence: 1,
    agent: {
      runtimeEpoch: RUNTIME_EPOCH,
    },
  };
}

function scopedBootstrapPayloadWithPendingChange(workspaceId, workspacePath, sessionId, title) {
  return {
    ...scopedBootstrapPayload(workspaceId, workspacePath, sessionId, title),
    pendingChanges: [
      {
        sessionId,
        workspaceId,
        workspacePath,
        filePath: 'app.js',
        snapshotId: `session:${sessionId}:app.js`,
        updatedAt: ACCEPTED_AT + 3000,
        type: 'modify',
        additions: 1,
        deletions: 1,
        contentKind: 'text',
        sourceKind: 'tool',
      },
      {
        session_id: 'legacy-session-scope',
        workspace_id: 'legacy-workspace-scope',
        workspace_path: '/tmp/legacy-workspace-scope',
        filePath: 'legacy-scope.js',
        snapshotId: 'legacy:legacy-scope.js',
        updatedAt: ACCEPTED_AT + 3001,
        type: 'modify',
        additions: 1,
        deletions: 0,
        contentKind: 'text',
        sourceKind: 'tool',
      },
    ],
    pendingChangesState: {
      status: 'ready',
      pendingCount: 2,
    },
  };
}

function settingsBootstrapPayload() {
  return {
    scope: 'workspace',
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
      const body = JSON.parse(String(init.body || '{}'));
      capturedTurnBodies.push(body);
      const interceptor = sessionTurnInterceptors.shift();
      if (interceptor) {
        return interceptor(parsed, init, body);
      }
      return jsonResponse({
        sessionId: 'session-bridge-partial-scope',
        entryId: 'timeline-bridge-partial-scope',
        eventId: 'event-bridge-partial-scope',
        acceptedAt: ACCEPTED_AT + capturedTurnBodies.length,
        runtimeEpoch: RUNTIME_EPOCH,
        eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
        createdSession: false,
        route: 'chat',
        userMessageItemId: 'user-bridge-partial-scope',
        canonicalSchemaVersion: null,
        canonicalEventKind: null,
        canonicalTurn: null,
        canonicalItem: null,
      });
    }
    if (parsed.pathname === '/api/session/queue' && init.method !== 'POST') {
      const interceptor = sessionQueueInterceptors.shift();
      if (interceptor) {
        return interceptor(parsed, init);
      }
      return jsonResponse({
        sessionId: parsed.searchParams.get('sessionId') || SESSION_ID,
        queuedTurns: queuedTurnPayloads,
      });
    }
    if (parsed.pathname === '/api/session/queue/remove') {
      const body = JSON.parse(String(init.body || '{}'));
      capturedRemoveQueueBodies.push(body);
      queuedTurnPayloads = queuedTurnPayloads.filter((turn) => turn.queueId !== body.queueId);
      return jsonResponse({
        sessionId: body.sessionId || SESSION_ID,
        queuedTurns: queuedTurnPayloads,
      });
    }
    if (parsed.pathname === '/api/session/queue/guide') {
      const body = JSON.parse(String(init.body || '{}'));
      capturedGuideQueueBodies.push(body);
      queuedTurnPayloads = queuedTurnPayloads.filter((turn) => turn.queueId !== body.queueId);
      return jsonResponse({
        sessionId: body.sessionId || SESSION_ID,
        queuedTurns: queuedTurnPayloads,
      });
    }
    if (parsed.pathname === '/api/session/interrupt') {
      interruptRequestCount += 1;
      queuedTurnPayloads = queuedTurnPayloads.slice(1);
      return jsonResponse({
        interrupted: true,
        sessionId: SESSION_ID,
        workspaceId: WORKSPACE_ID,
        turnId: TURN_ID,
        eventId: `event-interrupt-${interruptRequestCount}`,
        requestedAt: ACCEPTED_AT + 2600,
        remainingQueuedTurnCount: queuedTurnPayloads.length,
        nextQueuedTurnStarted: true,
        removedTimelineEntryIds: [],
      });
    }
    if (parsed.pathname === '/api/session/navigation') {
      sessionNavigationRequestCount += 1;
      const body = JSON.parse(String(init.body || '{}'));
      const interceptor = sessionNavigationInterceptors.shift();
      if (interceptor) {
        return interceptor(parsed, init);
      }
      const payload = bootstrapPayload();
      if (body.target === 'session') {
        return jsonResponse(payload);
      }
      return jsonResponse({
        ...payload,
        currentSession: null,
        state: {
          ...payload.state,
          currentSessionId: '',
        },
        timeline: [],
        canonicalTurns: [],
      });
    }
    if (parsed.pathname === '/api/session/viewed') {
      const body = JSON.parse(String(init.body || '{}'));
      return jsonResponse({
        runtimeEpoch: RUNTIME_EPOCH,
        eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
        sessionId: body.sessionId || SESSION_ID,
        hasUnreadCompletion: false,
      });
    }
    if (parsed.pathname === '/api/messages') {
      messagesSnapshotRequestCount += 1;
      return jsonResponse({
        generatedAt: ACCEPTED_AT,
        currentSession: null,
        sessions: [],
        timeline: [],
        canonicalTurns: [],
        notifications: [],
        sessionId: SESSION_ID,
        hasMoreBefore: false,
        beforeCursor: null,
      });
    }
    if (parsed.pathname === '/api/goals/current') {
      goalRefreshQueries.push(parsed.searchParams);
      return jsonResponse({
        sessionId: parsed.searchParams.get('sessionId') || '',
        workspaceId: parsed.searchParams.get('workspaceId') || '',
        workspacePath: parsed.searchParams.get('workspacePath') || '',
        goal: null,
        plan: null,
        allowedActions: {
          canEdit: false,
          canPause: false,
          canResume: false,
          canClear: false,
        },
      });
    }
    if (parsed.pathname === '/api/changes' && (!init.method || init.method === 'GET')) {
      pendingChangesRequestCount += 1;
      const sessionId = parsed.searchParams.get('sessionId') || SESSION_ID;
      const workspaceId = parsed.searchParams.get('workspaceId') || WORKSPACE_ID;
      const workspacePath = parsed.searchParams.get('workspacePath') || WORKSPACE_PATH;
      return jsonResponse({
        generatedAt: Date.now() + pendingChangesRequestCount,
        sessionId,
        workspaceId,
        workspacePath,
        pendingChanges: pendingChangesPayloadOverride || [],
        pendingChangesState: {
          status: 'ready',
          pendingCount: pendingChangesPayloadOverride?.length || 0,
        },
      });
    }
    if (
      parsed.pathname === '/api/changes/approve'
      || parsed.pathname === '/api/changes/revert'
      || parsed.pathname === '/api/changes/approve-all'
      || parsed.pathname === '/api/changes/revert-all'
      || parsed.pathname === '/api/changes/revert-execution-group'
    ) {
      bridgeMutationRequests.push({
        pathname: parsed.pathname,
        body: init.body ? JSON.parse(String(init.body)) : null,
      });
      const interceptor = bridgeMutationInterceptors.shift();
      if (interceptor) {
        return interceptor(parsed, init);
      }
      return jsonResponse({ ok: true });
    }
    if (parsed.pathname === '/bootstrap') {
      bootstrapRequestCount += 1;
      const interceptor = bootstrapInterceptors.shift();
      if (interceptor) {
        return interceptor(parsed);
      }
      if (!parsed.searchParams.get('sessionId')) {
        const payload = bootstrapPayload();
        return jsonResponse({
          ...payload,
          currentSession: null,
          timeline: [],
          canonicalTurns: [],
        });
      }
      return jsonResponse(bootstrapPayload());
    }
    if (parsed.pathname === '/api/workspaces' && Array.isArray(workspaceListPayload)) {
      return jsonResponse({ workspaces: workspaceListPayload });
    }
    if (parsed.pathname === '/api/workspaces/sessions') {
      workspaceSessionsRequestCount += 1;
      const interceptor = workspaceSessionsInterceptors.shift();
      if (interceptor) {
        return interceptor(parsed, init);
      }
      if (workspaceSessionsPayloadOverride) {
        return jsonResponse(workspaceSessionsPayloadOverride);
      }
      const requestedWorkspaceId = parsed.searchParams.get('workspaceId') || '';
      if (requestedWorkspaceId && requestedWorkspaceId !== WORKSPACE_ID) {
        const requestedWorkspacePath = parsed.searchParams.get('workspacePath') || '';
        return jsonResponse({
          runtimeEpoch: RUNTIME_EPOCH,
          eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
          workspace: {
            workspaceId: requestedWorkspaceId,
            rootPath: requestedWorkspacePath,
          },
          sessions: [],
        });
      }
      const payload = bootstrapPayload();
      return jsonResponse({
        runtimeEpoch: RUNTIME_EPOCH,
        eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
        workspace: payload.workspace,
        sessions: payload.sessions.map((session) => ({
          ...session,
          isRunning: workspaceSessionIsRunning,
        })),
      });
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
    itemSeq: 3,
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

function acceptedEnvelope(requestId = '') {
  const canonicalItem = acceptedCanonicalUserItem();
  return {
    event_id: `event-session-turn-task-${ACCEPTED_AT}`,
    event_type: 'session.turn.task.accepted',
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
      session_summary: {
        sessionId: SESSION_ID,
        workspaceId: WORKSPACE_ID,
        title: '新建会话',
        status: 'Running',
        createdAt: ACCEPTED_AT,
        updatedAt: ACCEPTED_AT,
        messageCount: 1,
        isRunning: true,
        runningTaskCount: 1,
        hasUnreadCompletion: false,
      },
      ...(requestId ? { request_id: requestId, requestId } : {}),
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

function modelRetryRuntimeEnvelope(phase, sequence = 3) {
  return {
    event_id: `event-model-retry-runtime-${phase}`,
    event_type: 'model.retry.runtime',
    category: 'domain',
    occurred_at: ACCEPTED_AT + sequence,
    sequence,
    workspace_id: WORKSPACE_ID,
    session_id: SESSION_ID,
    payload: {
      session_id: SESSION_ID,
      workspace_id: WORKSPACE_ID,
      message_id: 'assistant-bridge-retry-runtime',
      phase,
      attempt: 1,
      max_attempts: 5,
      ...(phase === 'scheduled' ? { delay_ms: 10000 } : {}),
    },
  };
}

const contextBreakdownFixture = {
  conversation_tokens: 12_000, image_tokens: 1_000, system_instruction_tokens: 3_000,
  project_context_tokens: 2_000, skill_tokens: 1_000, context_reference_tokens: 1_000,
  builtin_tool_tokens: 2_000, mcp_tool_tokens: 1_000, skill_tool_tokens: 1_000, other_tool_tokens: 0,
};
const contextBreakdownView = {
  conversationTokens: 12_000, imageTokens: 1_000, systemInstructionTokens: 3_000,
  projectContextTokens: 2_000, skillTokens: 1_000, contextReferenceTokens: 1_000,
  builtinToolTokens: 2_000, mcpToolTokens: 1_000, skillToolTokens: 1_000, otherToolTokens: 0,
};

function contextUsageEnvelope() {
  return {
    event_id: 'event-session-context-usage-streaming',
    event_type: 'session.context.pressure.updated',
    category: 'domain',
    occurred_at: ACCEPTED_AT + 2,
    sequence: 3,
    workspace_id: WORKSPACE_ID,
    session_id: SESSION_ID,
    payload: {
      session_id: SESSION_ID,
      workspace_id: WORKSPACE_ID,
      turn_id: TURN_ID,
      call_id: 'call-context-streaming',
      resolved_model: 'gpt-5.6',
      phase: 'streaming',
      measurement: 'estimated',
      projected_request_tokens: 24_000,
      context_breakdown: contextBreakdownFixture,
      remaining_tokens: 248_000,
      context_window_limit_tokens: 272_000,
      usage_ratio: 24_000 / 272_000,
      pressure_level: 'normal',
      observed_at: ACCEPTED_AT + 2,
    },
  };
}

function completedTurnItemEnvelope() {
  const canonicalTurn = completedCanonicalTurn();
  const canonicalItem = completedCanonicalAssistantItem();
  return {
    event_id: `event-session-turn-item-completed-${ACCEPTED_AT}`,
    event_type: 'session.turn.item',
    category: 'domain',
    occurred_at: ACCEPTED_AT + 2000,
    sequence: 5,
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

function terminalEnvelopeForRequest(requestId, sequence) {
  return {
    event_id: `event-session-turn-completed-${requestId}`,
    event_type: 'session.turn.completed',
    category: 'domain',
    occurred_at: ACCEPTED_AT + sequence,
    sequence,
    workspace_id: WORKSPACE_ID,
    session_id: SESSION_ID,
    payload: {
      session_id: SESSION_ID,
      workspace_id: WORKSPACE_ID,
      request_id: requestId,
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

function planUpdatedEnvelope() {
  return {
    event_id: 'event-session-plan-updated-goal-refresh',
    event_type: 'session.plan.updated',
    category: 'domain',
    occurred_at: ACCEPTED_AT + 1500,
    sequence: 4,
    workspace_id: WORKSPACE_ID,
    session_id: SESSION_ID,
    payload: {
      session_id: SESSION_ID,
      workspace_id: WORKSPACE_ID,
      plan: {
        planId: 'plan-bridge-goal-refresh',
        sessionId: SESSION_ID,
        goalId: 'goal-bridge-goal-refresh',
        revision: 2,
        language: 'zh-CN',
        state: 'active',
        items: [
          {
            itemId: 'plan-bridge-goal-refresh-item-1',
            title: '恢复执行',
            status: 'in_progress',
          },
        ],
        taskBindings: {},
        taskStatuses: {},
        updatedAt: ACCEPTED_AT + 1500,
      },
    },
  };
}

function laggedEnvelope(sequence = 6) {
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

function findArtifactByRequestId(projection, requestId) {
  return projection?.artifacts?.find((artifact) => artifact.message?.metadata?.requestId === requestId);
}

function currentSessionSummary(messagesStore) {
  return messagesStore.messagesState.workspaceSessionProjection.sessions.find((session) => session.id === SESSION_ID);
}

function beginSyntheticLocalSubmission(messagesStore, requestId) {
  const startedAt = Date.now();
  messagesStore.beginLocalTurnSubmission({
    requestId,
    sessionId: messagesStore.messagesState.currentSessionId,
    placeholderMessageId: `assistant-${requestId}`,
    startedAt,
    message: {
      id: `user-${requestId}`,
      role: 'user',
      source: 'user',
      content: requestId,
      timestamp: startedAt,
      isStreaming: false,
      isComplete: true,
      type: 'user_input',
      metadata: {
        requestId,
        localSubmission: true,
      },
    },
  });
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function verifyBrowserNodeSelectionTurn(bridge, messagesStore) {
  const requestId = 'request-browser-node-selection-forwarding';
  const targetSessionId = messagesStore.messagesState.currentSessionId?.trim() || SESSION_ID;
  const expectedSelection = {
    browserSessionId: 'browser-session-normalized',
    tabId: 'browser-tab-normalized',
    surfaceId: 'surface-normalized',
    navigationRevision: 3,
    url: 'https://example.com/form',
    title: 'Example form',
    frameId: null,
    backendDomNodeId: 42,
    domNodeId: 43,
    nodeName: 'BUTTON',
    attributes: {
      type: 'submit',
      'aria-label': 'Submit',
    },
    textExcerpt: 'Submit',
    outerHtml: '<button type="submit">Submit</button>',
    outerHtmlTruncated: false,
    ariaRole: 'button',
    ariaName: 'Submit',
    bounds: {
      x: 10,
      y: 20,
      width: 100,
      height: 40,
    },
  };
  const inputSelection = {
    ...expectedSelection,
    browserSessionId: `  ${expectedSelection.browserSessionId}  `,
    tabId: ` ${expectedSelection.tabId} `,
    surfaceId: ` ${expectedSelection.surfaceId} `,
  };
  const invalidSelection = {
    ...expectedSelection,
    backendDomNodeId: '42',
  };
  const selectionWithoutTruncationState = { ...expectedSelection };
  delete selectionWithoutTruncationState.outerHtmlTruncated;
  const acceptedResponse = deferred();
  sessionTurnInterceptors.push(() => acceptedResponse.promise);
  const acceptedPayload = {
    sessionId: targetSessionId,
    entryId: 'timeline-browser-node-selection-forwarding',
    eventId: 'event-browser-node-selection-forwarding',
    acceptedAt: ACCEPTED_AT + capturedTurnBodies.length,
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    createdSession: false,
    route: 'chat',
    userMessageItemId: 'user-browser-node-selection-forwarding',
    canonicalSchemaVersion: null,
    canonicalEventKind: null,
    canonicalTurn: null,
    canonicalItem: null,
  };
  bridge.postMessage({
    type: 'executeTask',
    text: '请分析选中的按钮。',
    requestId,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: targetSessionId,
    browserNodeSelections: [inputSelection, invalidSelection, selectionWithoutTruncationState],
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === requestId),
    'browser node selection submit must reach the canonical session turn endpoint',
  );
  const turnBody = capturedTurnBodies.find((body) => body.requestId === requestId);
  assert.deepEqual(
    turnBody.browserNodeSelections,
    [expectedSelection],
    'browser node selections must trim identity fields and reject malformed selections before submit',
  );
  assert.equal(
    findArtifactByRequestId(
      messagesStore.messagesState.canonicalTimelineProjection,
      requestId,
    ),
    undefined,
    'browser node selections must not create a local canonical message before the server accepts the turn',
  );
  // accepted 返回后再清理 request binding，确保节点选择已经随 canonical turn 接受。
  acceptedResponse.resolve(jsonResponse(acceptedPayload));
  await new Promise((resolve) => setTimeout(resolve, 0));
  messagesStore.clearRequestBinding(requestId);
}

installBrowserGlobals();
installFetchStub();

await withGoldenViteServer(async (server) => {
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  const bridgeModule = await server.ssrLoadModule('/src/shared/bridges/web-client-bridge.ts');
  const sessionNavigation = await server.ssrLoadModule('/src/shared/session-navigation.svelte.ts');
  const messageHandler = await server.ssrLoadModule('/src/lib/message-handler.ts');
  const messagesStore = await server.ssrLoadModule('/src/stores/messages.svelte.ts');
  const turnStore = await server.ssrLoadModule('/src/stores/turn-store.svelte.ts');
  const submissionLifecycleEvents = [];
  window.addEventListener('magi:sessionTurnSubmissionSettled', (event) => {
    submissionLifecycleEvents.push(event.detail);
  });

  const resumedRootHints = bridgeModule.extractBootstrapAgentRunTrackingHints(
    { sessionId: 'session-resumed-root' },
    {
      runtimeReadModel: {
        details: {
          sessions: [{
            session_id: 'session-resumed-root',
            current_status: 'detached',
            root_task_id: null,
            active_task_ids: [],
            active_execution_group_ids: [],
          }],
          tasks: [
            { task_id: 'task-interrupted-root', current_status: 'killed' },
            { task_id: 'task-resumed-root', current_status: 'completed' },
          ],
          execution_groups: [],
        },
      },
      recentEvents: [
        {
          session_id: 'session-resumed-root',
          task_id: 'task-interrupted-root',
          payload: { root_task_id: 'task-interrupted-root' },
        },
        {
          session_id: 'session-resumed-root',
          task_id: 'task-resumed-root',
          payload: { root_task_id: 'task-resumed-root' },
        },
      ],
    },
  );
  assert.deepEqual(
    resumedRootHints,
    { rootTaskId: 'task-resumed-root', activeTaskIds: [] },
    'detached 会话必须从最近事件恢复最新 root，不能继续固定已被续跑取代的中断任务',
  );

  const bridge = bridgeModule.createWebClientBridge();
  bridgeRuntime.setClientBridge(bridge);
  // 这些用例直接测试 Web Client Bridge，本身不经过 Header/Sidebar 的
  // navigateSession 协调器。先登记与真实 UI 相同的 pending 事务，才能
  // 验证 bridge 返回的 bootstrap 是否提交给了正确的导航请求。
  const originalBridgePostMessage = bridge.postMessage.bind(bridge);
  bridge.postMessage = (message) => {
    if (message.type === 'navigateSession') {
      const target = message.scope === 'workspace'
        ? {
            kind: message.target,
            scope: 'workspace',
            workspaceId: message.workspaceId,
            workspacePath: message.workspacePath,
            ...(message.target === 'session' ? { sessionId: message.sessionId } : {}),
          }
        : {
            kind: message.target,
            scope: 'personal',
            ...(message.target === 'session' ? { sessionId: message.sessionId } : {}),
          };
      sessionNavigation.sessionNavigationState.pending = {
        requestId: message.requestId,
        target,
        startedAt: Date.now(),
        completion: Promise.resolve(),
      };
    }
    originalBridgePostMessage(message);
  };
  messagesStore.initializeState();
  messageHandler.primeEventSeqTracking(messagesStore.messagesState.currentSessionId, messagesStore.messagesState.currentWorkspaceId);
  messageHandler.initMessageHandler(bridge);
  bridge.notifyReady();

  await waitFor(() => FakeEventSource.instances.length > 0, 'bootstrap must connect workspace-scoped SSE');
  const stream = FakeEventSource.instances[0];
  assert.ok(
    stream.url.includes(`/events?scope=workspace&workspaceId=${encodeURIComponent(WORKSPACE_ID)}`),
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

  // 不打开后台工作区，也必须更新其目录；突发通知按工作区合并。
  const directoryWorkspaceId = 'workspace-never-opened';
  const directorySessionId = 'session-never-opened';
  const directoryRequestsBefore = workspaceSessionsRequestCount;
  const directoryBootstrapBefore = bootstrapRequestCount;
  const foregroundBindingBefore = messagesStore.messagesState.currentWorkspaceId;
  workspaceSessionsInterceptors.push(() => jsonResponse({
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    workspace: { workspaceId: directoryWorkspaceId, rootPath: '/tmp/never-opened' },
    sessions: [{ sessionId: directorySessionId, title: '后台新标题', createdAt: ACCEPTED_AT,
      updatedAt: ACCEPTED_AT + 1, messageCount: 2, isRunning: false, hasUnreadCompletion: true }],
  }));
  for (let i = 0; i < 3; i += 1) {
    recoveredStream.onmessage?.({ data: JSON.stringify({
      event_id: `directory-event-${i}`, event_type: 'session.directory.changed', category: 'domain',
      sequence: 0, occurred_at: ACCEPTED_AT, workspace_id: directoryWorkspaceId,
      session_id: directorySessionId, payload: { scope: 'workspace', workspace_id: directoryWorkspaceId },
    }) });
  }
  await waitFor(() => messagesStore.messagesState.workspaceSessionProjections[directoryWorkspaceId]
    ?.sessions.some((session) => session.id === directorySessionId), '未打开工作区应接收目录变化');
  assert.equal(workspaceSessionsRequestCount, directoryRequestsBefore + 1, '同工作区突发通知应合并请求');
  assert.equal(bootstrapRequestCount, directoryBootstrapBefore, '目录通知不得重载当前会话');
  assert.equal(messagesStore.messagesState.currentWorkspaceId, foregroundBindingBefore, '后台刷新不得改变当前导航');
  assert.equal(messagesStore.messagesState.workspaceSessionProjection.workspaceId, foregroundBindingBefore,
    '后台目录不得替换前台投影');
  const directoryProjection = messagesStore.messagesState.workspaceSessionProjections[directoryWorkspaceId];
  assert.equal(directoryProjection.sessions[0].name, '后台新标题');
  assert.equal(directoryProjection.sessions[0].hasUnreadCompletion, true);
  assert.equal(messagesStore.replaceWorkspaceSessionProjection(directoryWorkspaceId, [], {
    runtimeEpoch: RUNTIME_EPOCH, eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE - 1,
  }, { mirrorCurrentProjection: false }), false, '旧快照不得清除后台新目录');

  const liveAcceptedRequestId = 'request-live-accepted';
  const liveAcceptedResponse = deferred();
  sessionTurnInterceptors.push(() => liveAcceptedResponse.promise);
  bridge.postMessage({
    type: 'executeTask',
    text: '通过真实 submission context 建立会话。',
    requestId: liveAcceptedRequestId,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: '',
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === liveAcceptedRequestId),
    'workspace-only page 首条提交必须建立 submission context',
  );
  recoveredStream.onmessage?.({ data: JSON.stringify(acceptedEnvelope(liveAcceptedRequestId)) });
  acceptedPublished = true;
  await waitFor(
    () => messagesStore.messagesState.currentSessionId === SESSION_ID,
    'workspace-only page must adopt live accepted session',
  );
  liveAcceptedResponse.resolve(jsonResponse({
    sessionId: SESSION_ID,
    entryId: 'timeline-live-accepted',
    eventId: 'event-live-accepted',
    acceptedAt: ACCEPTED_AT,
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    createdSession: true,
    route: 'chat',
    userMessageItemId: USER_ITEM_ID,
    canonicalSchemaVersion: null,
    canonicalEventKind: null,
    canonicalTurn: null,
    canonicalItem: null,
  }));
  messagesStore.clearPendingRequest(liveAcceptedRequestId);

  recoveredStream.onmessage?.({ data: JSON.stringify(contextUsageEnvelope()) });
  assert.deepEqual(
    messagesStore.messagesState.orchestratorRuntimeState?.runtimeSnapshot?.budgetState,
    {
      tokenUsed: 24_000,
      tokenLimit: 272_000,
      remainingTokens: 248_000,
      usageRatio: 24_000 / 272_000,
      warningLevel: 'normal',
      measurement: 'estimated',
      projectedRequestTokens: 24_000,
      contextBreakdown: contextBreakdownView,
      phase: 'streaming',
      updatedAt: ACCEPTED_AT + 2,
      eventSequence: 3,
      turnId: TURN_ID,
      callId: 'call-context-streaming',
      resolvedModel: 'gpt-5.6',
      pressureLevel: 'normal',
    },
    '运行中上下文用量必须通过 SSE 立即进入当前会话 runtime state',
  );

  const beforeReplacement = messagesStore.messagesState.orchestratorRuntimeState;
  messagesStore.setOrchestratorRuntimeState({
    ...beforeReplacement,
    runtimeSnapshot: {
      ...beforeReplacement.runtimeSnapshot,
      budgetState: {
        ...beforeReplacement.runtimeSnapshot.budgetState,
        providerContextTokens: 12_000,
        responseReserveTokens: 8_000,
        recoveryBufferTokens: 4_000,
        proactiveThresholdTokens: 220_000,
        hardRequestLimitTokens: 260_000,
        lastCompactionAt: ACCEPTED_AT,
        lastCompactionReason: 'manual',
      },
    },
  });
  const nextPressure = contextUsageEnvelope();
  nextPressure.sequence = 4;
  nextPressure.event_id = 'event-next-pressure';
  nextPressure.payload.call_id = 'next-call';
  nextPressure.payload.context_breakdown = null;
  delete nextPressure.payload.resolved_model;
  delete nextPressure.payload.turn_id;
  recoveredStream.onmessage?.({ data: JSON.stringify(nextPressure) });
  assert.equal(messagesStore.messagesState.orchestratorRuntimeState?.runtimeSnapshot?.budgetState?.contextBreakdown,
    undefined, 'a new call without classification must clear the old call breakdown');
  const replacedBudget = messagesStore.messagesState.orchestratorRuntimeState.runtimeSnapshot.budgetState;
  for (const key of ['providerContextTokens', 'responseReserveTokens', 'recoveryBufferTokens',
    'proactiveThresholdTokens', 'hardRequestLimitTokens', 'resolvedModel', 'turnId']) {
    assert.equal(replacedBudget[key], undefined, key + ' must not leak from the previous call');
  }
  assert.equal(replacedBudget.lastCompactionAt, ACCEPTED_AT);
  assert.equal(replacedBudget.lastCompactionReason, 'manual');

  recoveredStream.onmessage?.({ data: JSON.stringify(modelRetryRuntimeEnvelope('scheduled')) });
  assert.equal(
    messagesStore.retryRuntimeState.byMessageId.get('assistant-bridge-retry-runtime')?.phase,
    'scheduled',
    '当前会话的模型重连事件必须进入消息运行态存储',
  );
  recoveredStream.onmessage?.({ data: JSON.stringify(modelRetryRuntimeEnvelope('attempt_started', 4)) });
  assert.equal(
    messagesStore.retryRuntimeState.byMessageId.get('assistant-bridge-retry-runtime')?.phase,
    'attempt_started',
    '重连开始事件必须更新同一条消息的运行态',
  );
  recoveredStream.onmessage?.({ data: JSON.stringify(modelRetryRuntimeEnvelope('settled', 5)) });
  assert.equal(
    messagesStore.retryRuntimeState.byMessageId.has('assistant-bridge-retry-runtime'),
    false,
    '重连结束事件必须清理消息运行态，不能留下永久动画',
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

  const streamCountBeforeActiveIdleCheck = FakeEventSource.instances.length;
  const originalDateNowForActiveIdleCheck = Date.now;
  try {
    Date.now = () => originalDateNowForActiveIdleCheck() + 12_000;
    window.__runGoldenIntervals();
    await new Promise((resolve) => setTimeout(resolve, 30));
  } finally {
    Date.now = originalDateNowForActiveIdleCheck;
  }
  assert.equal(
    FakeEventSource.instances.length,
    streamCountBeforeActiveIdleCheck,
    'active turn must not rebuild SSE after only two missed keep-alives on tunnel/mobile links',
  );
  await waitFor(
    () => messagesStore.messagesState.workspaceSessionProjection.sessions.some((session) => session.id === SESSION_ID),
    'created live session must enter the workspace session projection from the accepted increment',
  );

  summaryMessageCount = 2;
  summaryUpdatedAt = ACCEPTED_AT + 1000;
  recoveredStream.onmessage?.({ data: JSON.stringify(messageCreatedEnvelope()) });
  await waitFor(
    () => currentSessionSummary(messagesStore)?.messageCount === 2,
    'current-session message events must refresh the workspace session summary without waiting for a full page reload',
  );

  const goalRefreshCountBeforePlanEvent = goalRefreshQueries.length;
  recoveredStream.onmessage?.({ data: JSON.stringify(planUpdatedEnvelope()) });
  await waitFor(
    () => goalRefreshQueries.length > goalRefreshCountBeforePlanEvent,
    'plan events must refresh the authoritative Goal state because plan snapshots cannot update Goal timing or actions',
  );

  const bootstrapRequestsBeforeTerminalEvent = bootstrapRequestCount;
  const workspaceSessionRequestsBeforeTerminalEvent = workspaceSessionsRequestCount;
  summaryUpdatedAt = ACCEPTED_AT + 2_000;
  terminalPublished = true;
  recoveredStream.onmessage?.({ data: JSON.stringify(completedTurnItemEnvelope()) });
  await waitFor(
    () => goalRefreshQueries.length > 0,
    'canonical turn item events must refresh the matching session Goal projection',
  );
  assert.equal(goalRefreshQueries.at(-1)?.get('sessionId'), SESSION_ID);
  assert.equal(goalRefreshQueries.at(-1)?.get('workspaceId'), WORKSPACE_ID);
  await waitFor(
    () => messagesStore.messagesState.isProcessing === false,
    'terminal session turn event with canonical payload must settle processing without waiting for recovery',
  );
  assert.equal(
    messagesStore.messagesState.orchestratorRuntimeState?.status,
    'completed',
    'terminal canonical event must settle the runtime panel without waiting for a later bootstrap',
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
  await waitFor(
    () => workspaceSessionsRequestCount > workspaceSessionRequestsBeforeTerminalEvent,
    'terminal session turn event must refresh the lightweight session summary',
  );
  assert.equal(
    bootstrapRequestCount,
    bootstrapRequestsBeforeTerminalEvent,
    'canonical terminal event must not trigger a heavyweight bootstrap refresh',
  );
  await waitFor(
    () => currentSessionSummary(messagesStore)?.updatedAt === summaryUpdatedAt,
    'terminal session summary refresh must finish applying before tunnel reconciliation starts',
  );
  assert.equal(
    messagesStore.messagesState.orchestratorRuntimeState?.status,
    'completed',
    'a stale terminal bootstrap must not lift the runtime panel back to running',
  );
  messagesStore.clearRequestBinding(liveAcceptedRequestId);

  const trailingWhitespaceRequestId = 'request-trailing-whitespace';
  bridge.postMessage({
    type: 'executeTask',
    text: '第一行\n第二行\n\n   ',
    requestId: trailingWhitespaceRequestId,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === trailingWhitespaceRequestId),
    'trailing contenteditable whitespace test must reach the session turn endpoint',
  );
  const trailingWhitespaceBody = capturedTurnBodies.find(
    (body) => body.requestId === trailingWhitespaceRequestId,
  );
  assert.equal(
    trailingWhitespaceBody.text,
    '第一行\n第二行',
    'canonical request must omit trailing contenteditable whitespace while preserving internal line breaks',
  );
  const localTrailingWhitespace = messagesStore
    .getLocalTurnSubmissionRenderItems(SESSION_ID)
    .find((item) => item.message.metadata?.requestId === trailingWhitespaceRequestId);
  assert.equal(
    localTrailingWhitespace?.message.content,
    '第一行\n第二行',
    'optimistic user bubble must use the same trimmed text as the canonical request',
  );
  messagesStore.clearRequestBinding(trailingWhitespaceRequestId);

  const canonicalTurnsBeforeLaterRound = structuredClone(turnStore.turnStoreState.reducer.turns);
  const laterRoundAccepted = deferred();
  sessionTurnInterceptors.push(() => laterRoundAccepted.promise);
  bridge.postMessage({
    type: 'executeTask',
    text: '后续轮次也必须立刻出现发送反馈。',
    requestId: 'request-later-round-immediate-feedback',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
  });
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    '已有会话的后续轮次必须在提交事件返回前进入 processing 状态',
  );
  assert.equal(
    findArtifactByRequestId(
      messagesStore.messagesState.canonicalTimelineProjection,
      'request-later-round-immediate-feedback',
    ),
    undefined,
    '后续轮次在服务端 accepted 前只能显示 processing，不能伪造 canonical turn',
  );
  laterRoundAccepted.resolve(jsonResponse({
    sessionId: SESSION_ID,
    entryId: 'timeline-later-round-immediate-feedback',
    eventId: 'event-later-round-immediate-feedback',
    acceptedAt: ACCEPTED_AT + 2250,
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    createdSession: false,
    route: 'chat',
    userMessageItemId: 'user-later-round-immediate-feedback',
    canonicalSchemaVersion: null,
    canonicalEventKind: null,
    canonicalTurn: null,
    canonicalItem: null,
  }));
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === 'request-later-round-immediate-feedback'),
    '后续轮次必须正常提交到后端',
  );
  turnStore.replaceCanonicalSessionTurns(SESSION_ID, canonicalTurnsBeforeLaterRound);
  messagesStore.settleAuthoritativeIdleState();

  const localUrlBeforeTunnelSync = window.location.href;
  const workspaceSessionRequestsBeforeTunnelSync = workspaceSessionsRequestCount;
  const bootstrapRequestsBeforeTunnelSync = bootstrapRequestCount;
  beginSyntheticLocalSubmission(messagesStore, 'tunnel-stale-running-state');
  workspaceSessionIsRunning = true;
  window.location.href = `${localUrlBeforeTunnelSync}&tunnel_token=golden-token`;
  const originalDateNowForTunnelSync = Date.now;
  try {
    Date.now = () => originalDateNowForTunnelSync() + 3_000;
    window.__runGoldenIntervals();
    await waitFor(
      () => workspaceSessionsRequestCount > workspaceSessionRequestsBeforeTunnelSync,
      'tunnel busy sync must query the lightweight workspace session summary',
    );
    assert.equal(
      bootstrapRequestCount,
      bootstrapRequestsBeforeTunnelSync,
      'tunnel busy sync must not repeatedly fetch the heavy bootstrap while the backend is still running',
    );
    await waitFor(
      () => currentSessionSummary(messagesStore)?.isRunning === true,
      'tunnel busy sync must apply the running workspace session summary',
    );
    await waitFor(
      () => messagesStore.messagesState.isProcessing === true,
      'backend running summary must preserve the public tunnel processing state',
    );
    await new Promise((resolve) => setTimeout(resolve, 0));

    workspaceSessionIsRunning = false;
    Date.now = () => originalDateNowForTunnelSync() + 6_000;
    window.__runGoldenIntervals();
    await waitFor(
      () => workspaceSessionRequestsBeforeTunnelSync + 2 === workspaceSessionsRequestCount,
      'tunnel terminal sync must recheck the lightweight workspace session summary',
    );
    assert.equal(
      messagesStore.messagesState.isProcessing,
      true,
      'session summary without request identity must not clear an active local turn',
    );
    recoveredStream.onmessage?.({
      data: JSON.stringify(terminalEnvelopeForRequest('tunnel-stale-running-state', 6)),
    });
    await waitFor(
      () => messagesStore.messagesState.isProcessing === false,
      'request-scoped terminal event must settle the stale public tunnel turn immediately',
    );
    assert.equal(
      workspaceSessionsRequestCount,
      workspaceSessionRequestsBeforeTunnelSync + 2,
      'tunnel terminal sync must recheck the lightweight workspace session summary',
    );
    await waitFor(
      () => bootstrapRequestCount > bootstrapRequestsBeforeTunnelSync,
      'request-scoped terminal event must trigger authoritative bootstrap recovery',
    );
  } finally {
    Date.now = originalDateNowForTunnelSync;
    workspaceSessionIsRunning = false;
    window.location.href = localUrlBeforeTunnelSync;
    messagesStore.clearPendingRequest('tunnel-stale-running-state');
  }

  const sessionsBeforeDraft = messagesStore.messagesState.workspaceSessionProjection.sessions.map((session) => session.id);
  messagesStore.messagesState.settingsBootstrapSnapshot = {
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
    orchestratorSessionConfig: {
      model: 'model-inherited-from-current-session',
      reasoningEffort: 'high',
    },
    effectiveOrchestratorConfig: {
      model: 'model-inherited-from-current-session',
      reasoningEffort: 'high',
    },
  };
  bridge.postMessage({
    type: 'navigateSession',
    target: 'draft',
    scope: 'workspace',
    requestId: 'navigation-draft-from-current-session',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    orchestratorSessionConfig: {
      model: 'model-inherited-from-current-session',
      reasoningEffort: 'high',
    },
  });
  await waitFor(
    () => !messagesStore.messagesState.currentSessionId,
    'new session action must enter workspace draft state',
  );
  assert.deepEqual(
    messagesStore.messagesState.draftOrchestratorSessionConfig,
    {
      model: 'model-inherited-from-current-session',
      reasoningEffort: 'high',
    },
    'new session draft must inherit the model selected by the source session',
  );
  messagesStore.messagesState.draftOrchestratorSessionConfig = {
    model: 'model-selected-in-draft',
    reasoningEffort: 'xhigh',
  };
  bridge.postMessage({
    type: 'navigateSession',
    target: 'draft',
    scope: 'workspace',
    requestId: 'navigation-reopen-current-draft',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    orchestratorSessionConfig: {
      model: 'model-selected-in-draft',
      reasoningEffort: 'xhigh',
    },
  });
  await waitFor(
    () => messagesStore.messagesState.draftOrchestratorSessionConfig.model === 'model-selected-in-draft',
    'reopening the same draft must preserve its explicit model selection',
  );
  assert.deepEqual(
    messagesStore.messagesState.workspaceSessionProjection.sessions.map((session) => session.id),
    sessionsBeforeDraft,
    'entering and abandoning a new-session draft must not erase the sidebar session list',
  );
  assert.equal(
    messagesStore.messagesState.workspaceSessionProjection.workspaceId,
    WORKSPACE_ID,
    'new-session draft must preserve the explicit workspace scope of the session list projection',
  );
  bridge.postMessage({
    type: 'navigateSession',
    target: 'draft',
    scope: 'workspace',
    requestId: 'navigation-idempotent-workspace-draft',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    orchestratorSessionConfig: {
      model: 'model-selected-in-draft',
      reasoningEffort: 'xhigh',
    },
  });
  assert.deepEqual(
    messagesStore.messagesState.workspaceSessionProjection.sessions.map((session) => session.id),
    sessionsBeforeDraft,
    'idempotent workspace-only binding sync must not clear the preserved session list',
  );

  const bootstrapRequestsBeforeDraftFocus = bootstrapRequestCount;
  const streamsBeforeDraftFocus = FakeEventSource.instances.length;
  window.dispatchEvent(new Event('focus'));
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(
    bootstrapRequestCount,
    bootstrapRequestsBeforeDraftFocus,
    '窗口重新获得焦点时不得用 bootstrap 自动选回旧会话覆盖新会话草稿',
  );
  assert.equal(
    FakeEventSource.instances.length,
    streamsBeforeDraftFocus,
    '工作区级事件流正常时，草稿页面重新获得焦点不得重复重连',
  );
  assert.equal(messagesStore.messagesState.currentSessionId, null,
    '窗口重新获得焦点后必须继续停留在未发送的新会话草稿');

  const activeDraftStream = FakeEventSource.instances.at(-1);
  const workspaceSessionRequestsBeforeDraftRecovery = workspaceSessionsRequestCount;
  activeDraftStream.onerror?.();
  window.dispatchEvent(new Event('focus'));
  await waitForWithin(
    () => FakeEventSource.instances.length > streamsBeforeDraftFocus,
    '草稿事件流中断后必须按工作区绑定恢复',
    350,
  );
  const recoveredDraftStream = FakeEventSource.instances.at(-1);
  recoveredDraftStream.onopen?.();
  await waitFor(
    () => workspaceSessionsRequestCount > workspaceSessionRequestsBeforeDraftRecovery,
    '草稿恢复必须重新同步工作区会话目录',
  );
  assert.equal(
    bootstrapRequestCount,
    bootstrapRequestsBeforeDraftFocus + 1,
    '草稿断流恢复必须通过工作区级 bootstrap 原子恢复目录与空会话选择',
  );
  assert.equal(messagesStore.messagesState.currentSessionId, null,
    '草稿断流恢复后必须保持无会话绑定');
  assert.deepEqual(
    messagesStore.messagesState.draftOrchestratorSessionConfig,
    {
      model: 'model-selected-in-draft',
      reasoningEffort: 'xhigh',
    },
    '草稿断流恢复不得覆盖用户在新会话中尚未发送的配置状态',
  );
  assert.deepEqual(
    messagesStore.messagesState.workspaceSessionProjection.sessions.map((session) => session.id),
    sessionsBeforeDraft,
    '草稿断流恢复只能刷新目录，不能把页面切回旧会话',
  );

  // 草稿态不能被同一工作区其他会话的 accepted 事件接管。
  recoveredDraftStream.onmessage?.({ data: JSON.stringify(acceptedEnvelope()) });
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    null,
    '草稿态收到没有 requestId 的其他会话 accepted 事件时必须保持空绑定',
  );

  // 首条消息的 accepted 事件必须与本次仍在等待的 requestId 关联，不能只
  // 因为页面处于草稿态就接受任意 sessionId。
  const draftTurnAccepted = deferred();
  const draftTurnRequestId = 'request-draft-first-turn';
  sessionTurnInterceptors.push(() => draftTurnAccepted.promise);
  bridge.postMessage({
    type: 'executeTask',
    text: '新会话首条消息必须绑定到本次提交。',
    requestId: draftTurnRequestId,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: '',
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === draftTurnRequestId),
    '新会话首条消息必须到达 session turn 接口',
  );
  const draftTurnBody = capturedTurnBodies.find((body) => body.requestId === draftTurnRequestId);
  assert.equal(
    draftTurnBody.sessionId,
    null,
    '新会话首条消息不能把本地或其他会话 ID 作为显式 sessionId 发送',
  );
  const staleDraftBootstrap = bootstrapPayload();
  bootstrapInterceptors.push(() => jsonResponse({
    ...staleDraftBootstrap,
    currentSession: null,
    state: {
      ...staleDraftBootstrap.state,
      currentSessionId: '',
    },
    timeline: [],
    canonicalTurns: [],
  }));
  bridge.postMessage({ type: 'requestState' });
  await waitFor(
    () => messagesStore.messagesState.pendingRequests.has(draftTurnRequestId),
    'pending 首条消息收到空会话 bootstrap 时必须保留待确认请求状态',
  );
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    null,
    '首条消息 accepted 前不得创建本地 sessionId',
  );
  recoveredDraftStream.onmessage?.({ data: JSON.stringify(acceptedEnvelope('request-draft-foreign')) });
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    null,
    '草稿态收到其他 requestId 的 accepted 事件时必须保持空 session 绑定',
  );
  recoveredDraftStream.onmessage?.({ data: JSON.stringify(acceptedEnvelope(draftTurnRequestId)) });
  assert.notEqual(
    messagesStore.messagesState.currentSessionId,
    null,
    '与当前首条消息 requestId 匹配的 accepted 事件必须建立会话绑定',
  );
  draftTurnAccepted.resolve(jsonResponse({
    sessionId: SESSION_ID,
    entryId: 'timeline-draft-first-turn',
    eventId: 'event-draft-first-turn',
    acceptedAt: ACCEPTED_AT + 2900,
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    createdSession: true,
    route: 'chat',
    userMessageItemId: 'user-draft-first-turn',
    canonicalSchemaVersion: null,
    canonicalEventKind: null,
    canonicalTurn: null,
    canonicalItem: null,
  }));
  await waitFor(
    () => messagesStore.messagesState.currentSessionId === SESSION_ID,
    '新会话首条消息 accepted 后必须绑定服务端返回的 session',
  );
  // 该夹具只验证首条消息的绑定，不模拟后续 terminal event。显式结束本次
  // 请求绑定，保证后面的导航/排队场景不会把它当作仍在运行的本地 turn。
  messagesStore.clearPendingRequest(draftTurnRequestId);
  messagesStore.clearRequestBinding(draftTurnRequestId);

  // HTTP accepted 返回晚于用户切换草稿时，旧提交不能重新夺回 currentSessionId。
  const staleDraftTurnAccepted = deferred();
  const staleDraftTurnRequestId = 'request-stale-draft-after-navigation';
  sessionTurnInterceptors.push(() => staleDraftTurnAccepted.promise);
  bridge.postMessage({
    type: 'navigateSession',
    target: 'draft',
    scope: 'workspace',
    requestId: 'navigation-enter-stale-draft',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
  });
  await waitFor(
    () => !messagesStore.messagesState.currentSessionId,
    '切换新草稿必须先清除已提交会话绑定',
  );
  bridge.postMessage({
    type: 'executeTask',
    text: '旧草稿提交不能污染后续草稿。',
    requestId: staleDraftTurnRequestId,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: '',
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === staleDraftTurnRequestId),
    '旧草稿提交必须到达后端后再模拟导航竞态',
  );
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    null,
    '旧草稿 accepted 前不得创建本地 session 作为竞态锚点',
  );
  bridge.postMessage({
    type: 'navigateSession',
    target: 'draft',
    scope: 'workspace',
    requestId: 'navigation-replace-stale-draft',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
  });
  await waitFor(
    () => !messagesStore.messagesState.currentSessionId,
    '用户进入新的草稿后必须保持空 session 绑定',
  );
  staleDraftTurnAccepted.resolve(jsonResponse({
    sessionId: 'session-stale-draft-response',
    entryId: 'timeline-stale-draft-response',
    eventId: 'event-stale-draft-response',
    acceptedAt: ACCEPTED_AT + 3001,
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    createdSession: true,
    route: 'chat',
    userMessageItemId: 'user-stale-draft-response',
    canonicalSchemaVersion: null,
    canonicalEventKind: null,
    canonicalTurn: null,
    canonicalItem: null,
  }));
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    null,
    '旧草稿 HTTP accepted 返回后不得恢复已失效的 sessionId',
  );
  messagesStore.clearRequestBinding(staleDraftTurnRequestId);

  bridge.postMessage({
    type: 'navigateSession',
    target: 'session',
    scope: 'workspace',
    requestId: 'navigation-back-to-current-session',
    sessionId: SESSION_ID,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
  });
  await waitFor(
    () => messagesStore.messagesState.currentSessionId === SESSION_ID,
    'selecting an existing session after abandoning the draft must restore it normally',
  );

  const streamAfterDraftReturn = FakeEventSource.instances.at(-1);
  streamAfterDraftReturn.onopen?.();

  const queuedTurnAccepted = deferred();
  sessionTurnInterceptors.push(() => queuedTurnAccepted.promise);
  beginSyntheticLocalSubmission(messagesStore, 'busy-before-queued-follow-up');
  bridge.postMessage({
    type: 'executeTask',
    text: '排队消息必须在出队提交时立刻进入主线。',
    requestId: 'request-queued-immediate-feedback',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
    goalMode: true,
    contextReferences: [{
      kind: 'file',
      path: '/tmp/reference-queued.md',
      pathRef: 'mhp1:u:reference-queued',
      name: 'reference-queued.md',
    }],
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === 'request-queued-immediate-feedback'),
    'busy follow-up must submit immediately so the daemon owns the queue',
  );
  const queuedGoalBody = capturedTurnBodies.find((body) => body.requestId === 'request-queued-immediate-feedback');
  assert.equal(
    queuedGoalBody.goalMode,
    true,
    'queued follow-up must preserve the structured goal mode flag',
  );
  assert.deepEqual(
    queuedGoalBody.contextReferences,
    [{
      kind: 'file',
      path: '/tmp/reference-queued.md',
      pathRef: 'mhp1:u:reference-queued',
      name: 'reference-queued.md',
    }],
    'queued follow-up must preserve authoritative pathRef context references in the POST body',
  );
  assert.equal(
    findArtifactByRequestId(
      messagesStore.messagesState.canonicalTimelineProjection,
      'request-queued-immediate-feedback',
    ),
    undefined,
    'queued follow-up must be tracked as pending without creating a local canonical turn',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'queued follow-up must keep processing visible while the daemon response is unresolved',
  );
  queuedTurnPayloads = [
    {
      queueId: 'queued-session-turn-golden',
      revision: 6,
      queuePosition: 1,
      requestId: 'request-queued-immediate-feedback',
      sessionId: SESSION_ID,
      workspaceId: WORKSPACE_ID,
      workspacePath: WORKSPACE_PATH,
      acceptedAt: ACCEPTED_AT + 2500,
      content: '排队消息必须在出队提交时立刻进入主线。',
      text: '排队消息必须在出队提交时立刻进入主线。',
      command: null,
      skillName: null,
      goalMode: true,
      accessProfile: null,
      images: [],
      contextReferences: [{
        kind: 'file',
        path: '/tmp/reference-queued.md',
        pathRef: 'mhp1:u:reference-queued',
        name: 'reference-queued.md',
      }],
      browserAnnotationRefs: [],
      guide: { status: 'unsupported_executor', targetTurnId: 'turn-guide-snapshot' },
      retryCount: 0,
    },
    {
      queueId: 'queued-session-turn-guide-golden',
      revision: 7,
      queuePosition: 2,
      requestId: 'request-queued-guide-feedback',
      sessionId: SESSION_ID,
      workspaceId: WORKSPACE_ID,
      workspacePath: WORKSPACE_PATH,
      acceptedAt: ACCEPTED_AT + 2501,
      content: '请优先给出结论。',
      text: '请优先给出结论。',
      command: null,
      skillName: null,
      goalMode: false,
      accessProfile: null,
      images: [],
      contextReferences: [],
      browserAnnotationRefs: [],
      guide: { status: 'ready', targetTurnId: 'turn-guide-snapshot' },
      retryCount: 0,
    },
  ];
  queuedTurnAccepted.resolve(jsonResponse({
    sessionId: SESSION_ID,
    entryId: 'queued-session-turn-golden',
    eventId: 'event-queued-immediate-feedback',
    acceptedAt: ACCEPTED_AT + 2500,
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    createdSession: false,
    route: 'chat',
    userMessageItemId: 'user-queued-immediate-feedback',
    queued: true,
    queueId: 'queued-session-turn-golden',
    queuePosition: 1,
    canonicalSchemaVersion: null,
    canonicalEventKind: null,
    canonicalTurn: null,
    canonicalItem: null,
  }));
  await waitFor(
    () => messagesStore.messagesState.queuedMessages.some((message) => (
      message.id === 'queued-session-turn-golden'
      && message.requestId === 'request-queued-immediate-feedback'
    )),
    'accepted queued turn must be rendered from the daemon queue snapshot',
  );
  assert.equal(
    messagesStore.messagesState.queuedMessages.find((message) => (
      message.id === 'queued-session-turn-guide-golden'
    ))?.guide?.status,
    'ready',
    'queue normalization must preserve the server-authoritative guidance capability',
  );
  assert.deepEqual(
    messagesStore.messagesState.queuedMessages.find((message) => (
      message.id === 'queued-session-turn-golden'
    ))?.guide,
    { status: 'unsupported_executor', targetTurnId: 'turn-guide-snapshot' },
    'queue normalization must preserve the executor refusal so the UI can explain why guidance is disabled',
  );
  assert.equal(
    messagesStore.messagesState.queuedMessages.find((message) => (
      message.id === 'queued-session-turn-guide-golden'
    ))?.revision,
    7,
    'the queue revision must reach the composer so guidance and editing can use CAS',
  );
  assert.equal(
    findArtifactByRequestId(
      messagesStore.messagesState.canonicalTimelineProjection,
      'request-queued-immediate-feedback',
    ),
    undefined,
    'daemon-queued turns must not remain as fake running conversation turns',
  );
  messagesStore.clearPendingRequest('busy-before-queued-follow-up');
  const queueSnapshotBeforeGuide = structuredClone(queuedTurnPayloads);
  const staleQueueBeforeGuide = deferred();
  let staleQueueBeforeGuideStarted = false;
  sessionQueueInterceptors.push(() => {
    staleQueueBeforeGuideStarted = true;
    return staleQueueBeforeGuide.promise;
  });
  function queueEvent(eventType, eventId) {
    streamAfterDraftReturn.onmessage?.({ data: JSON.stringify({
      event_id: eventId,
      event_type: eventType,
      category: 'domain',
      occurred_at: ACCEPTED_AT + 2550,
      workspace_id: WORKSPACE_ID,
      session_id: SESSION_ID,
      payload: { session_id: SESSION_ID },
    }) });
  }
  queueEvent('session.turn.queued', 'queue-read-before-guide');
  await waitFor(() => staleQueueBeforeGuideStarted, 'queue notification must read the daemon queue');
  bridge.postMessage({
    type: 'guideQueuedMessage',
    queuedMessageId: 'queued-session-turn-guide-golden',
    expectedTurnId: 'turn-guide-snapshot',
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(capturedGuideQueueBodies.length, 0,
    '补充缺少点击时版本时不能用最新快照代填，防止补充另一客户端刚编辑的输入');
  bridge.postMessage({
    type: 'guideQueuedMessage',
    queuedMessageId: 'queued-session-turn-guide-golden',
    expectedTurnId: 'turn-guide-snapshot',
    expectedRevision: 7,
  });
  await waitFor(
    () => messagesStore.messagesState.queuedMessages.length === 1,
    'guiding a queued turn must atomically update the daemon queue snapshot',
  );
  assert.deepEqual(
    capturedGuideQueueBodies.at(-1),
    {
      workspaceId: WORKSPACE_ID,
      workspacePath: WORKSPACE_PATH,
      sessionId: SESSION_ID,
      queueId: 'queued-session-turn-guide-golden',
      expectedTurnId: 'turn-guide-snapshot',
      expectedRevision: 7,
    },
    'queued guidance must use the dedicated server-authoritative endpoint',
  );
  staleQueueBeforeGuide.resolve(jsonResponse({ sessionId: SESSION_ID, queuedTurns: queueSnapshotBeforeGuide }));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(messagesStore.messagesState.queuedMessages.length, 1, 'an old GET must not resurrect a message after guidance removed it');
  bridge.postMessage({
    type: 'removeQueuedMessage',
    queuedMessageId: 'queued-session-turn-golden',
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(capturedRemoveQueueBodies.length, 0,
    '缺少点击时的版本不能用最新快照补齐后删除，以免撤销另一客户端刚编辑的输入');
  bridge.postMessage({
    type: 'removeQueuedMessage',
    queuedMessageId: 'queued-session-turn-golden',
    expectedRevision: messagesStore.messagesState.queuedMessages[0].revision,
  });
  await waitFor(() => capturedRemoveQueueBodies.length === 1,
    '删除队列消息必须提交点击时的 Input 版本');
  assert.equal(capturedRemoveQueueBodies[0].expectedRevision, 6);
  await waitFor(
    () => messagesStore.messagesState.queuedMessages.length === 0,
    'removing a queued turn must update the daemon queue and then refresh the UI snapshot',
  );

  // 两次队列快照逆序返回：conversation 出队事件必须刷新队列，迟到的旧快照不能覆盖新状态。
  queuedTurnPayloads = [queueSnapshotBeforeGuide[1]];
  queueEvent('session.turn.queued', 'queue-refill-for-dequeue');
  await waitFor(() => messagesStore.messagesState.queuedMessages.length === 1, 'queued event must render the new queue');
  const staleQueueBeforeDequeue = deferred();
  let staleQueueBeforeDequeueStarted = false;
  sessionQueueInterceptors.push(() => {
    staleQueueBeforeDequeueStarted = true;
    return staleQueueBeforeDequeue.promise;
  });
  queueEvent('session.turn.queued', 'queue-read-before-dequeue');
  await waitFor(() => staleQueueBeforeDequeueStarted, 'the old queue read must be in flight');
  queuedTurnPayloads = [];
  queueEvent('session.turn.conversation.accepted', 'queue-conversation-dequeued');
  await waitFor(() => messagesStore.messagesState.queuedMessages.length === 0, 'conversation acceptance must remove the sent queue entry');
  staleQueueBeforeDequeue.resolve(jsonResponse({ sessionId: SESSION_ID, queuedTurns: [queueSnapshotBeforeGuide[1]] }));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(messagesStore.messagesState.queuedMessages.length, 0, 'a late queue response must not restore an already sent message');

  queuedTurnPayloads = [{
    queueId: 'queued-after-interrupt-golden',
    queuePosition: 1,
    requestId: 'request-after-interrupt-golden',
    sessionId: SESSION_ID,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    acceptedAt: ACCEPTED_AT + 2600,
    content: '保留压缩重点。',
    text: '保留压缩重点。',
    command: 'compact',
    skillName: null,
    goalMode: false,
    accessProfile: null,
    images: [],
    contextReferences: [],
    browserAnnotationRefs: [],
    retryCount: 0,
  }];
  messagesStore.setQueuedMessages(queuedTurnPayloads.map((turn) => ({
    id: turn.queueId,
    requestId: turn.requestId,
    content: turn.content,
    text: turn.text,
    command: turn.command,
    sessionId: turn.sessionId,
    workspaceId: turn.workspaceId,
    workspacePath: turn.workspacePath,
    createdAt: turn.acceptedAt,
    skillName: turn.skillName,
    goalMode: turn.goalMode,
    accessProfile: turn.accessProfile,
    images: turn.images,
    contextReferences: turn.contextReferences,
    browserAnnotationRefs: turn.browserAnnotationRefs,
    guide: turn.guide,
  })));
  assert.equal(
    messagesStore.messagesState.queuedMessages[0]?.command,
    'compact',
    'queue snapshots must preserve session commands as structured metadata',
  );
  const interruptRequestId = 'request-interrupt-current';
  beginSyntheticLocalSubmission(messagesStore, interruptRequestId);
  const interruptCanonicalTurn = completedCanonicalTurn();
  interruptCanonicalTurn.status = 'running';
  delete interruptCanonicalTurn.completedAt;
  delete interruptCanonicalTurn.responseDurationMs;
  interruptCanonicalTurn.metadata = { requestId: interruptRequestId };
  turnStore.replaceCanonicalSessionTurns(
    SESSION_ID,
    [interruptCanonicalTurn],
    2,
  );
  bridge.postMessage({ type: 'interruptTask' });
  await waitFor(
    () => interruptRequestCount === 1 && messagesStore.messagesState.queuedMessages.length === 0,
    '停止当前轮次后必须同步服务端已按 FIFO 启动的队首消息',
  );
  assert.equal(
    messagesStore.messagesState.pendingRequests.has(interruptRequestId),
    false,
    '中断 API 确认后必须在队列同步完成前精确收敛被中断 request',
  );

  const streamCountBeforeLagged = FakeEventSource.instances.length;
  const pendingChangesRequestsBeforeLagged = pendingChangesRequestCount;
  pendingChangesPayloadOverride = [{
    sessionId: SESSION_ID,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    filePath: 'recovered-change.ts',
    type: 'modify',
    additions: 3,
    deletions: 1,
    revertible: true,
  }];
  const originalWarn = console.warn;
  try {
    console.warn = () => {};
    streamAfterDraftReturn.onmessage?.({ data: JSON.stringify(laggedEnvelope()) });
    await waitForWithin(
      () => FakeEventSource.instances.length > streamCountBeforeLagged,
      'lagged SSE event must immediately rebuild the event stream through bootstrap recovery',
      350,
    );
  } finally {
    console.warn = originalWarn;
  }
  assert.equal(
    streamAfterDraftReturn.closed,
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
  await waitFor(
    () => messagesStore.messagesState.edits.some((edit) => edit.filePath === 'recovered-change.ts'),
    'workspace recovery must refresh pending changes before the edits tab is opened',
  );
  assert.equal(
    pendingChangesRequestCount,
    pendingChangesRequestsBeforeLagged + 1,
    'one authoritative changes refresh must accompany a successful workspace recovery',
  );
  pendingChangesPayloadOverride = null;

  const navigationBarrierResponse = deferred();
  const navigationRequestsBeforeBarrier = sessionNavigationRequestCount;
  sessionNavigationInterceptors.push(() => navigationBarrierResponse.promise.then(() => jsonResponse(
    scopedBootstrapPayload(
      WORKSPACE_ID,
      WORKSPACE_PATH,
      NAVIGATION_BARRIER_SESSION_ID,
      '导航屏障会话',
    ),
  )));
  const terminalRefreshBootstrapRequests = [];
  bridge.postMessage({
    type: 'navigateSession',
    target: 'session',
    scope: 'workspace',
    requestId: 'navigation-terminal-refresh-barrier',
    sessionId: NAVIGATION_BARRIER_SESSION_ID,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
  });
  await waitFor(
    () => sessionNavigationRequestCount > navigationRequestsBeforeBarrier,
    '导航屏障场景必须先发起会话导航',
  );
  laggedRecoveredStream.onmessage?.({ data: JSON.stringify(completedTurnItemEnvelope()) });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(
    terminalRefreshBootstrapRequests.length,
    0,
    'turn 终态后台 bootstrap 必须等待用户导航原子提交，不能抢占导航事务',
  );
  navigationBarrierResponse.resolve();
  await waitFor(
    () => messagesStore.messagesState.currentSessionId === NAVIGATION_BARRIER_SESSION_ID,
    '后台终态刷新并发发生时，首次用户导航仍必须提交到目标会话',
  );
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(
    terminalRefreshBootstrapRequests.length,
    0,
    'canonical 终态事件不应在导航提交后补发冗余 bootstrap',
  );
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    NAVIGATION_BARRIER_SESSION_ID,
    '导航后的后台 bootstrap 不得夺回旧会话',
  );

  sessionNavigationInterceptors.push(() => jsonResponse(scopedBootstrapPayload(
    WORKSPACE_ID,
    WORKSPACE_PATH,
    SESSION_ID,
    '返回当前会话',
  )));
  bridge.postMessage({
    type: 'navigateSession',
    target: 'session',
    scope: 'workspace',
    requestId: 'navigation-return-after-terminal-refresh-barrier',
    sessionId: SESSION_ID,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
  });
  await waitFor(
    () => messagesStore.messagesState.currentSessionId === SESSION_ID,
    '导航屏障回归完成后必须恢复原测试会话',
  );

  const switchBootstrapRequests = [];
  pendingChangesPayloadOverride = scopedBootstrapPayloadWithPendingChange(
    WORKSPACE_ID,
    WORKSPACE_PATH,
    SESSION_ID,
    '切换后带变更会话',
  ).pendingChanges;
  sessionNavigationInterceptors.push((parsed) => {
    switchBootstrapRequests.push(parsed);
    return jsonResponse(scopedBootstrapPayloadWithPendingChange(
      WORKSPACE_ID,
      WORKSPACE_PATH,
      SESSION_ID,
      '切换后带变更会话',
    ));
  });
  const messagesSnapshotBeforeSwitch = messagesSnapshotRequestCount;
  bridge.postMessage({
    type: 'navigateSession',
    target: 'session',
    scope: 'workspace',
    requestId: 'navigation-restore-authoritative-bootstrap',
    sessionId: SESSION_ID,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
  });
  await waitFor(
    () => switchBootstrapRequests.length === 1,
    'switchSession must restore from authoritative bootstrap',
  );
  assert.equal(
    messagesSnapshotRequestCount,
    messagesSnapshotBeforeSwitch,
    'switchSession must not restore from /api/messages partial snapshot',
  );
  await waitFor(
    () => messagesStore.messagesState.edits.length === 2,
    'switchSession must load pending changes through the independent changes projection',
  );
  assert.equal(
    messagesStore.messagesState.edits.length,
    2,
    'switchSession bootstrap must preserve pending changes in edits panel state',
  );
  const legacyScopeEdit = messagesStore.messagesState.edits.find((edit) => edit.filePath === 'legacy-scope.js');
  assert.equal(
    legacyScopeEdit?.sessionId,
    undefined,
    'pendingChanges must not derive session scope from legacy snake_case fields',
  );
  assert.equal(
    legacyScopeEdit?.workspaceId,
    undefined,
    'pendingChanges must not derive workspace scope from legacy snake_case fields',
  );
  assert.equal(
    legacyScopeEdit?.workspacePath,
    undefined,
    'pendingChanges must not derive workspace path from legacy snake_case fields',
  );
  pendingChangesPayloadOverride = null;

  const backgroundSessionId = 'session-bridge-background-running';
  const backgroundSession = {
    sessionId: backgroundSessionId,
    title: '后台运行会话',
    createdAt: ACCEPTED_AT - 1000,
    updatedAt: ACCEPTED_AT + 4000,
    messageCount: 2,
    workspaceId: WORKSPACE_ID,
  };
  const foregroundBootstrap = scopedBootstrapPayloadWithPendingChange(
    WORKSPACE_ID,
    WORKSPACE_PATH,
    SESSION_ID,
    '前台查看会话',
  );
  pendingChangesPayloadOverride = foregroundBootstrap.pendingChanges;
  foregroundBootstrap.sessions = [backgroundSession, ...foregroundBootstrap.sessions];
  foregroundBootstrap.state.sessions = foregroundBootstrap.sessions;
  workspaceSessionsPayloadOverride = {
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    workspace: foregroundBootstrap.workspace,
    sessions: [
      { ...backgroundSession, isRunning: true, runningTaskCount: 1 },
      { ...foregroundBootstrap.sessions[1], isRunning: false, runningTaskCount: 0 },
    ],
  };
  sessionNavigationInterceptors.push(() => jsonResponse(foregroundBootstrap));
  bridge.postMessage({
    type: 'navigateSession',
    target: 'session',
    scope: 'workspace',
    requestId: 'navigation-foreground-running-session',
    sessionId: SESSION_ID,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
  });
  await waitFor(
    () => messagesStore.messagesState.workspaceSessionProjection.sessions.some((session) => (
      session.id === backgroundSessionId
      && session.isRunning === true
      && session.runningTaskCount === 1
    )),
    'switchSession bootstrap must preserve the authoritative running state of background sessions',
  );
  await waitFor(
    () => messagesStore.messagesState.edits.length === 2,
    'foreground session must restore pending changes through the independent changes projection',
  );
  workspaceSessionsPayloadOverride = null;
  pendingChangesPayloadOverride = null;

  const editsBeforeLiveRefresh = structuredClone(messagesStore.messagesState.edits);
  const currentChangesVersion = messagesStore.messagesState.appState?.pendingChangesStateVersion ?? 0;
  const foreignRefreshApplied = messagesStore.applyPendingChangesProjection({
    generatedAt: currentChangesVersion + 1,
    sessionId: 'session-foreign-changes-refresh',
    workspaceId: WORKSPACE_ID,
    pendingChanges: [],
    pendingChangesState: { status: 'ready', pendingCount: 0 },
  });
  assert.equal(foreignRefreshApplied, false, 'live changes refresh must reject a foreign session');
  assert.equal(messagesStore.messagesState.edits.length, 2);

  const liveRefreshApplied = messagesStore.applyPendingChangesProjection({
    generatedAt: currentChangesVersion + 1,
    sessionId: SESSION_ID,
    workspaceId: WORKSPACE_ID,
    pendingChanges: [{
      sessionId: SESSION_ID,
      workspaceId: WORKSPACE_ID,
      workspacePath: WORKSPACE_PATH,
      filePath: 'watcher-refresh.ts',
      type: 'add',
      additions: 1,
      deletions: 0,
      revertible: true,
    }],
    pendingChangesState: { status: 'ready', pendingCount: 1 },
  });
  assert.equal(liveRefreshApplied, true, 'live changes refresh must update the active session');
  assert.deepEqual(
    messagesStore.messagesState.edits.map((edit) => edit.filePath),
    ['watcher-refresh.ts'],
  );
  messagesStore.applyPendingChangesProjection({
    generatedAt: currentChangesVersion + 2,
    sessionId: SESSION_ID,
    workspaceId: WORKSPACE_ID,
    pendingChanges: editsBeforeLiveRefresh,
    pendingChangesState: { status: 'ready', pendingCount: editsBeforeLiveRefresh.length },
  });

  const blockedMutation = deferred();
  const duplicateMutationBootstrap = deferred();
  const duplicateMutationBootstrapRequests = [];
  const mutationRequestsBeforeDedupe = bridgeMutationRequests.length;
  bridgeMutationInterceptors.push(() => blockedMutation.promise.then(() => jsonResponse({ ok: true })));
  bootstrapInterceptors.push((parsed) => {
    duplicateMutationBootstrapRequests.push(parsed);
    return duplicateMutationBootstrap.promise.then(() => jsonResponse(scopedBootstrapPayloadWithPendingChange(
      WORKSPACE_ID,
      WORKSPACE_PATH,
      SESSION_ID,
      '重复操作后快照',
    )));
  });
  bridge.postMessage({
    type: 'approveChange',
    filePath: 'app.js',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
  });
  await waitFor(
    () => bridgeMutationRequests.length === mutationRequestsBeforeDedupe + 1,
    'first change mutation must reach backend',
  );
  await waitFor(
    () => messagesStore.messagesState.changeMutationStatus?.isMutating === true
      && messagesStore.messagesState.changeMutationStatus?.sessionId === SESSION_ID
      && messagesStore.messagesState.changeMutationStatus?.workspaceId === WORKSPACE_ID,
    'change mutation start must be reflected in the message store',
  );
  bridge.postMessage({
    type: 'approveChange',
    filePath: 'app.js',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
  });
  bridge.postMessage({
    type: 'revertAllChanges',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
  });
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(
    bridgeMutationRequests.length,
    mutationRequestsBeforeDedupe + 1,
    'in-flight change mutation must suppress duplicate and overlapping mutations for the same scope',
  );
  assert.equal(
    messagesStore.messagesState.changeMutationStatus?.isMutating,
    true,
    'deduped change mutations must keep the original in-flight status active',
  );
  blockedMutation.resolve();
  await waitFor(
    () => duplicateMutationBootstrapRequests.length === 1,
    'finished change mutation must refresh authoritative bootstrap once',
  );
  duplicateMutationBootstrap.resolve();
  await waitFor(
    () => messagesStore.messagesState.changeMutationStatus === null,
    'change mutation status must clear after the authoritative bootstrap refresh completes',
  );
  const mutationRequestsAfterDedupeCompletion = bridgeMutationRequests.length;
  bridge.postMessage({
    type: 'revertChange',
    filePath: 'app.js',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
  });
  await waitFor(
    () => bridgeMutationRequests.length === mutationRequestsAfterDedupeCompletion + 1,
    'change mutation scope lock must be released after the authoritative bootstrap refresh completes',
  );
  await waitFor(
    () => messagesStore.messagesState.changeMutationStatus === null,
    'subsequent change mutation must also clear its status after refresh',
  );

  const slowBootstrap = deferred();
  const freshBootstrap = deferred();
  const mutationBootstrapRequests = [];
  bootstrapInterceptors.push((parsed) => {
    mutationBootstrapRequests.push(parsed);
    return slowBootstrap.promise.then(() => jsonResponse(scopedBootstrapPayloadWithPendingChange(
      WORKSPACE_ID,
      WORKSPACE_PATH,
      SESSION_ID,
      '旧飞行中快照',
    )));
  });
  bridge.postMessage({ type: 'requestState' });
  await waitFor(
    () => mutationBootstrapRequests.length === 1,
    'mutation setup must start an in-flight bootstrap request',
  );
  bootstrapInterceptors.push((parsed) => {
    mutationBootstrapRequests.push(parsed);
    return freshBootstrap.promise.then(() => jsonResponse(scopedBootstrapPayload(
      WORKSPACE_ID,
      WORKSPACE_PATH,
      SESSION_ID,
      '批准后新快照',
    )));
  });
  bridge.postMessage({
    type: 'approveChange',
    filePath: 'app.js',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
  });
  await waitFor(
    () => bridgeMutationRequests.some((request) => request.pathname === '/api/changes/approve')
      && mutationBootstrapRequests.length === 2,
    'approveChange must start a fresh bootstrap instead of reusing a stale in-flight request',
  );
  freshBootstrap.resolve();
  slowBootstrap.resolve();
  await waitFor(
    () => messagesStore.messagesState.edits.length === 0,
    'approveChange fresh bootstrap must clear accepted pending changes',
  );

  bridge.postMessage({
    type: 'executeTask',
    text: '验证 partial workspace scope',
    workspaceId: PARTIAL_WORKSPACE_ID,
    requestId: 'request-bridge-partial-scope',
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === 'request-bridge-partial-scope'),
    'partial workspace submit must reach backend',
  );
  const partialWorkspaceBody = capturedTurnBodies.find((body) => body.requestId === 'request-bridge-partial-scope');
  assert.equal(partialWorkspaceBody.workspaceId, PARTIAL_WORKSPACE_ID);
  assert.equal(
    partialWorkspaceBody.workspacePath,
    null,
    'workspaceId-only bridge submit must not inherit stale current workspacePath',
  );
  assert.equal(
    partialWorkspaceBody.sessionId,
    null,
    'workspaceId-only bridge submit must not inherit stale current sessionId',
  );

  const staleUrlBootstrapRequests = [];
  bridge.postMessage({
    type: 'navigateSession',
    target: 'draft',
    scope: 'workspace',
    requestId: 'navigation-clear-stale-url',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
  });
  await waitFor(
    () => !messagesStore.messagesState.currentSessionId,
    'workspace-scoped authoritative binding must clear current session before stale URL guard',
  );
  window.location.href = `http://127.0.0.1:38123/web.html?workspaceId=${encodeURIComponent(WORKSPACE_ID)}&workspacePath=${encodeURIComponent(WORKSPACE_PATH)}&sessionId=session-stale-url`;
  bootstrapInterceptors.push((parsed) => {
    staleUrlBootstrapRequests.push(parsed);
    const payload = scopedBootstrapPayload(
      WORKSPACE_ID,
      WORKSPACE_PATH,
      SESSION_ID,
      'stale URL guard bootstrap',
    );
    return jsonResponse({
      ...payload,
      sessionId: '',
      currentSession: null,
      state: {
        ...payload.state,
        currentSessionId: '',
      },
    });
  });
  bridge.postMessage({ type: 'requestState' });
  await waitFor(
    () => staleUrlBootstrapRequests.length === 1,
    'stale URL guard must issue a bootstrap request',
  );
  assert.equal(
    staleUrlBootstrapRequests[0].searchParams.has('sessionId'),
    false,
    'authoritative workspace-only binding must not let stale URL sessionId re-enter bootstrap',
  );

  const firstTurnAccepted = deferred();
  sessionTurnInterceptors.push(() => firstTurnAccepted.promise);
  bridge.postMessage({
    type: 'executeTask',
    text: '首条消息必须立刻进入主线。',
    requestId: 'request-first-turn-immediate-feedback',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: '',
  });
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    '提交事件返回前必须先进入本地 processing 状态，让按钮与等待动画立即响应',
  );
  assert.equal(
    findArtifactByRequestId(
      messagesStore.messagesState.canonicalTimelineProjection,
      'request-first-turn-immediate-feedback',
    ),
    undefined,
    '首条消息 accepted 前只能进入 processing，不能产生本地伪 canonical turn',
  );
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    null,
    '首条消息 accepted 前必须保持空 session 绑定',
  );
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === 'request-first-turn-immediate-feedback'),
    'first turn submit must reach backend',
  );
  const firstTurnBody = capturedTurnBodies.find((body) => body.requestId === 'request-first-turn-immediate-feedback');
  assert.equal(
    firstTurnBody.sessionId,
    null,
    'first turn optimistic session must not be sent as a real backend sessionId',
  );
  firstTurnAccepted.resolve(jsonResponse({
    sessionId: SESSION_ID,
    entryId: 'timeline-first-turn-immediate-feedback',
    eventId: 'event-first-turn-immediate-feedback',
    acceptedAt: ACCEPTED_AT + 2600,
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    createdSession: true,
    route: 'chat',
    userMessageItemId: 'user-first-turn-immediate-feedback',
    canonicalSchemaVersion: null,
    canonicalEventKind: null,
    canonicalTurn: null,
    canonicalItem: null,
  }));
  await waitFor(
    () => messagesStore.messagesState.currentSessionId === SESSION_ID,
    'first turn accepted response must replace the local-only session binding with the real session',
  );
  messagesStore.clearPendingRequest('request-first-turn-immediate-feedback');

  await new Promise((resolve) => setTimeout(resolve, 20));
  const continueTurnAccepted = deferred();
  const continueBootstrapRequestCount = bootstrapRequestCount;
  const continueWorkspaceSessionsRequestCount = workspaceSessionsRequestCount;
  const continueCapturedTurnCount = capturedTurnBodies.length;
  sessionTurnInterceptors.push(() => continueTurnAccepted.promise);
  bridge.postMessage({ type: 'continueTask' });
  assert.equal(
    workspaceSessionsRequestCount,
    continueWorkspaceSessionsRequestCount,
    'continueTask must not synchronously refresh the workspace session list before submission',
  );
  await waitFor(
    () => capturedTurnBodies.length === continueCapturedTurnCount + 1,
    'continueTask must submit through the canonical session turn endpoint',
  );
  const continueTurnBody = capturedTurnBodies.at(-1);
  assert.equal(continueTurnBody.text, '继续');
  assert.equal(continueTurnBody.sessionId, SESSION_ID);
  assert.equal(
    bootstrapRequestCount,
    continueBootstrapRequestCount,
    'continueTask must not refresh the full bootstrap before submission',
  );
  const continueLocalArtifact = findArtifactByRequestId(
    messagesStore.messagesState.canonicalTimelineProjection,
    continueTurnBody.requestId,
  );
  assert.equal(
    continueLocalArtifact,
    undefined,
    'continueTask must keep the request pending without fabricating a canonical turn before acceptance',
  );
  continueTurnAccepted.resolve(jsonResponse({
    sessionId: SESSION_ID,
    entryId: 'timeline-continue-no-refresh',
    eventId: 'event-continue-no-refresh',
    acceptedAt: ACCEPTED_AT + 2700,
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    createdSession: false,
    route: 'continue',
    rootTaskId: 'task-continue-no-refresh',
    actionTaskId: 'task-continue-no-refresh',
    userMessageItemId: 'user-continue-no-refresh',
    canonicalSchemaVersion: null,
    canonicalEventKind: null,
    canonicalTurn: null,
    canonicalItem: null,
  }));
  await waitFor(
    () => messagesStore.messagesState.pendingRequests.has(continueTurnBody.requestId),
    'continueTask accepted response must keep the request tracked until terminal runtime output arrives',
  );
  assert.equal(bootstrapRequestCount, continueBootstrapRequestCount);
  messagesStore.clearPendingRequest(continueTurnBody.requestId);

  const slowWorkspaceSessions = deferred();
  const slowSessionQueue = deferred();
  workspaceSessionsInterceptors.push(() => slowWorkspaceSessions.promise);
  sessionQueueInterceptors.push(() => slowSessionQueue.promise);
  bootstrapInterceptors.push(() => jsonResponse(scopedBootstrapPayload(
    WORKSPACE_ID,
    WORKSPACE_PATH,
    SESSION_ID,
    '首屏非阻塞会话',
  )));
  window.location.href = `http://127.0.0.1:38123/web.html?workspaceId=${encodeURIComponent(WORKSPACE_ID)}&workspacePath=${encodeURIComponent(WORKSPACE_PATH)}&sessionId=${encodeURIComponent(SESSION_ID)}`;
  messagesStore.messagesState.bootstrapped = false;
  bridge.postMessage({ type: 'requestState' });
  await waitForWithin(
    () => messagesStore.messagesState.bootstrapped === true,
    '核心 bootstrap 必须在会话摘要和排队请求完成前解除首屏等待',
    250,
  );
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    SESSION_ID,
    '核心 bootstrap 必须先恢复当前会话',
  );
  slowWorkspaceSessions.resolve(jsonResponse({
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    workspace: {
      workspaceId: WORKSPACE_ID,
      rootPath: WORKSPACE_PATH,
    },
    sessionId: SESSION_ID,
    sessions: scopedBootstrapPayload(
      WORKSPACE_ID,
      WORKSPACE_PATH,
      SESSION_ID,
      '首屏非阻塞会话',
    ).sessions,
  }));
  slowSessionQueue.resolve(jsonResponse({
    sessionId: SESSION_ID,
    queuedTurns: [],
  }));
  await new Promise((resolve) => setTimeout(resolve, 20));

  const originalRaceWarn = console.warn;
  try {
    console.warn = () => {};
    const firstBootstrap = deferred();
    const secondBootstrap = deferred();
    const raceBootstrapRequests = [];
    bootstrapInterceptors.push((parsed) => {
      raceBootstrapRequests.push(parsed);
      return firstBootstrap.promise.then(() => jsonResponse(bootstrapPayload()));
    });
    messagesStore.messagesState.bootstrapped = false;
    bridge.postMessage({ type: 'requestState' });
    await waitFor(
      () => raceBootstrapRequests.length === 1,
      'race setup must start the first bootstrap request',
    );

    sessionNavigationInterceptors.push((parsed) => {
      raceBootstrapRequests.push(parsed);
      return secondBootstrap.promise.then(() => jsonResponse(scopedBootstrapPayload(
        RACE_WORKSPACE_ID,
        RACE_WORKSPACE_PATH,
        RACE_SESSION_ID,
        '启动竞态会话',
      )));
    });
    bridge.postMessage({
      type: 'navigateSession',
      target: 'session',
      scope: 'workspace',
      requestId: 'navigation-bootstrap-race-latest',
      workspaceId: RACE_WORKSPACE_ID,
      workspacePath: RACE_WORKSPACE_PATH,
      sessionId: RACE_SESSION_ID,
    });
    await waitFor(
      () => raceBootstrapRequests.length === 2,
      'binding-changed requestState must start a fresh bootstrap instead of reusing stale recovery',
    );
    secondBootstrap.resolve();
    firstBootstrap.resolve();
    await waitFor(
      () => messagesStore.messagesState.currentSessionId === RACE_SESSION_ID,
      'binding-changed bootstrap race must adopt the latest session',
    );
    assert.equal(
      messagesStore.messagesState.bootstrapped,
      true,
      'binding-changed bootstrap race must clear the startup overlay',
    );
    assert.equal(
      messagesStore.messagesState.currentWorkspaceId,
      RACE_WORKSPACE_ID,
      'binding-changed bootstrap race must adopt the latest workspace',
    );
  } finally {
    console.warn = originalRaceWarn;
  }

  const missingSessionId = 'session-bridge-explicitly-missing';
  const navigationRequestsBeforeMissingSession = sessionNavigationRequestCount;
  sessionNavigationInterceptors.push(() => new Response(
    JSON.stringify({ message: `session 不存在: ${missingSessionId}` }),
    { status: 404, headers: { 'content-type': 'application/json' } },
  ));
  bridge.postMessage({
    type: 'navigateSession',
    target: 'session',
    scope: 'workspace',
    requestId: 'navigation-missing-session',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: missingSessionId,
  });
  await waitFor(
    () => sessionNavigationRequestCount > navigationRequestsBeforeMissingSession,
    'unknown explicit session navigation must reach backend',
  );
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    RACE_SESSION_ID,
    'failed navigation must preserve the previously committed session',
  );

  workspaceListPayload = [];
  const staleEmptyBootstrap = deferred();
  let staleEmptyBootstrapRequested = false;
  bootstrapInterceptors.push(() => {
    staleEmptyBootstrapRequested = true;
    return staleEmptyBootstrap.promise.then(() => new Response(
      JSON.stringify({ message: 'workspace missing' }),
      { status: 404, headers: { 'content-type': 'application/json' } },
    ));
  });
  messagesStore.messagesState.bootstrapped = false;
  bridge.postMessage({ type: 'requestState' });
  await waitFor(
    () => staleEmptyBootstrapRequested,
    'stale empty-workspace bootstrap request must start',
  );
  sessionNavigationInterceptors.push(() => jsonResponse(scopedBootstrapPayload(
    WORKSPACE_ID,
    WORKSPACE_PATH,
    SESSION_ID,
    '最新会话',
  )));
  bridge.postMessage({
    type: 'navigateSession',
    target: 'session',
    scope: 'workspace',
    requestId: 'navigation-after-stale-empty-bootstrap',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
  });
  await waitFor(
    () => messagesStore.messagesState.currentWorkspaceId === WORKSPACE_ID
      && messagesStore.messagesState.currentSessionId === SESSION_ID,
    'fresh bootstrap must adopt the latest binding before stale empty response resolves',
  );
  staleEmptyBootstrap.resolve();
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(
    messagesStore.messagesState.currentWorkspaceId,
    WORKSPACE_ID,
    'stale empty-workspace bootstrap must not clear the latest workspace binding',
  );
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    SESSION_ID,
    'stale empty-workspace bootstrap must not clear the latest session binding',
  );

  bootstrapInterceptors.push(() => new Response(
    JSON.stringify({ message: 'workspace missing' }),
    { status: 404, headers: { 'content-type': 'application/json' } },
  ));
  messagesStore.messagesState.bootstrapped = false;
  bridge.postMessage({ type: 'requestState' });
  await waitFor(
    () => !messagesStore.messagesState.currentWorkspaceId
      && !messagesStore.messagesState.currentSessionId,
    'authoritative empty-workspace bootstrap must clear stale workspace and session bindings',
  );
  assert.equal(
    window.location.href.includes('workspaceId='),
    false,
    'authoritative empty-workspace bootstrap must remove stale workspace from URL',
  );
  assert.deepEqual(
    messagesStore.messagesState.workspaceSessionProjection.sessions,
    [],
    'authoritative empty-workspace bootstrap must clear stale session summaries',
  );
  workspaceListPayload = null;

  const directoryRaceSessionId = 'session-directory-race-increment';
  messagesStore.replaceWorkspaceSessionProjection(
    RACE_WORKSPACE_ID,
    [],
    { runtimeEpoch: RUNTIME_EPOCH, eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE + 9 },
    { allowRuntimeEpochChange: true },
  );
  const acceptedDirectorySession = {
    id: directoryRaceSessionId,
    workspaceId: RACE_WORKSPACE_ID,
    name: 'accepted 目录会话',
    createdAt: ACCEPTED_AT + 4000,
    updatedAt: ACCEPTED_AT + 4000,
    messageCount: 1,
    isRunning: true,
    runningTaskCount: 1,
  };
  assert.equal(
    messagesStore.upsertAcceptedSessionDirectoryEntry(
      acceptedDirectorySession,
      { runtimeEpoch: RUNTIME_EPOCH, eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE + 10 },
    ),
    true,
    'accepted directory increment must be applied to the active projection',
  );
  assert.equal(
    messagesStore.messagesState.workspaceSessionProjection.sessions.some((session) => session.id === directoryRaceSessionId),
    true,
    'accepted directory increment must make the new session visible immediately',
  );
  assert.equal(
    messagesStore.replaceWorkspaceSessionProjection(
      RACE_WORKSPACE_ID,
      [],
      { runtimeEpoch: RUNTIME_EPOCH, eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE + 9 },
      { allowRuntimeEpochChange: true },
    ),
    false,
    'an older directory snapshot must be rejected after the accepted increment',
  );
  assert.equal(
    messagesStore.messagesState.workspaceSessionProjection.sessions.some((session) => session.id === directoryRaceSessionId),
    true,
    'an older directory snapshot must not remove the accepted session',
  );
  messagesStore.replaceWorkspaceSessionProjection(
    RACE_WORKSPACE_ID,
    [],
    { runtimeEpoch: RUNTIME_EPOCH, eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE + 11 },
    { allowRuntimeEpochChange: true },
  );
  assert.equal(
    messagesStore.messagesState.workspaceSessionProjection.sessions.some((session) => session.id === directoryRaceSessionId),
    false,
    'a newer authoritative snapshot may remove a session that it does not contain',
  );

  messagesStore.upsertAcceptedSessionDirectoryEntry(
    acceptedDirectorySession,
    { runtimeEpoch: RUNTIME_EPOCH, eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE + 12 },
  );
  messagesStore.replaceWorkspaceSessionProjection(
    RACE_WORKSPACE_ID,
    [{
      ...acceptedDirectorySession,
      name: '权威目录会话',
      updatedAt: ACCEPTED_AT + 4001,
    }],
    { runtimeEpoch: RUNTIME_EPOCH, eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE + 13 },
    { allowRuntimeEpochChange: true },
  );
  assert.equal(
    messagesStore.messagesState.workspaceSessionProjection.sessions.find((session) => session.id === directoryRaceSessionId)?.name,
    '权威目录会话',
    'a newer authoritative snapshot must replace the accepted increment with canonical fields',
  );
  messagesStore.replaceWorkspaceSessionProjection(
    RACE_WORKSPACE_ID,
    [],
    { runtimeEpoch: RUNTIME_EPOCH, eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE + 14 },
    { allowRuntimeEpochChange: true },
  );
  assert.equal(
    messagesStore.messagesState.workspaceSessionProjection.sessions.some((session) => session.id === directoryRaceSessionId),
    false,
    'once an authoritative directory snapshot confirms the session, normal deletion semantics must resume',
  );

  // 已提交会话的 HTTP accepted 晚于会话切换时，也不能把 currentSessionId 切回旧会话。
  const currentCommittedSessionId = messagesStore.messagesState.currentSessionId || SESSION_ID;
  const staleCommittedTurnAccepted = deferred();
  const staleCommittedTurnRequestId = 'request-stale-committed-after-navigation';
  sessionTurnInterceptors.push(() => staleCommittedTurnAccepted.promise);
  bridge.postMessage({
    type: 'executeTask',
    text: '已提交会话的旧响应也不能污染新页面。',
    requestId: staleCommittedTurnRequestId,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: currentCommittedSessionId,
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === staleCommittedTurnRequestId),
    '已提交会话的竞态消息必须到达后端后再模拟导航',
  );
  sessionNavigationInterceptors.push(() => jsonResponse(scopedBootstrapPayload(
    RACE_WORKSPACE_ID,
    RACE_WORKSPACE_PATH,
    RACE_SESSION_ID,
    '竞态切换后的会话',
  )));
  bridge.postMessage({
    type: 'navigateSession',
    target: 'session',
    scope: 'workspace',
    requestId: 'navigation-replace-committed-turn',
    sessionId: RACE_SESSION_ID,
    workspaceId: RACE_WORKSPACE_ID,
    workspacePath: RACE_WORKSPACE_PATH,
  });
  await waitFor(
    () => messagesStore.messagesState.currentSessionId === RACE_SESSION_ID,
    '切换会话必须先建立新的 currentSessionId',
  );
  staleCommittedTurnAccepted.resolve(jsonResponse({
    sessionId: currentCommittedSessionId,
    entryId: 'timeline-stale-committed-response',
    eventId: 'event-stale-committed-response',
    acceptedAt: ACCEPTED_AT + 3002,
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    createdSession: false,
    route: 'chat',
    userMessageItemId: 'user-stale-committed-response',
    canonicalSchemaVersion: null,
    canonicalEventKind: null,
    canonicalTurn: null,
    canonicalItem: null,
  }));
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(
    messagesStore.messagesState.currentSessionId,
    RACE_SESSION_ID,
    '旧已提交会话 HTTP accepted 返回后不得夺回新会话的 currentSessionId',
  );
  messagesStore.clearRequestBinding(staleCommittedTurnRequestId);

  await verifyBrowserNodeSelectionTurn(bridge, messagesStore);

  const failedSubmissionRequestId = 'request-submission-lifecycle-failed';
  sessionTurnInterceptors.push(() => new Response(JSON.stringify({
    error: 'simulated transport rejection',
  }), {
    status: 503,
    headers: { 'content-type': 'application/json' },
  }));
  bridge.postMessage({
    type: 'executeTask',
    text: '失败提交必须通知编辑器恢复草稿。',
    requestId: failedSubmissionRequestId,
    workspaceId: RACE_WORKSPACE_ID,
    workspacePath: RACE_WORKSPACE_PATH,
    sessionId: RACE_SESSION_ID,
  });
  await waitFor(
    () => submissionLifecycleEvents.some((event) => (
      event?.requestId === failedSubmissionRequestId && event?.status === 'failed'
    )),
    'session turn submit failure must notify the composer through the request lifecycle event',
  );
  assert.deepEqual(
    submissionLifecycleEvents.find((event) => event?.requestId === failedSubmissionRequestId),
    { requestId: failedSubmissionRequestId, status: 'failed' },
    'submission lifecycle event must preserve the request identity and terminal status',
  );

  // Electron 使用 App Server WebSocket 时，发送前预热必须复用同一条
  // 当前会话连接。旧实现把 App Server 键误和 SSE 键比较，导致每次发送
  // 都关闭并重建 WebSocket，进而制造消息事件竞态。
  const appServerSocketCountBefore = FakeWebSocket.instances.length;
  window.magiDesktop = { runtime: 'electron' };
  const electronTurnResponse = (index) => jsonResponse({
    sessionId: RACE_SESSION_ID,
    entryId: `timeline-electron-reuse-${index}`,
    eventId: `event-electron-reuse-${index}`,
    acceptedAt: ACCEPTED_AT + 4000 + index,
    runtimeEpoch: RUNTIME_EPOCH,
    eventStreamNextSequence: EVENT_STREAM_NEXT_SEQUENCE,
    createdSession: false,
    route: 'chat',
    userMessageItemId: `user-electron-reuse-${index}`,
    canonicalSchemaVersion: null,
    canonicalEventKind: null,
    canonicalTurn: null,
    canonicalItem: null,
  });
  sessionTurnInterceptors.push(() => electronTurnResponse(1));
  bridge.postMessage({
    type: 'executeTask',
    text: '初始化 Electron App Server 连接。',
    requestId: 'request-electron-reuse-1',
    workspaceId: RACE_WORKSPACE_ID,
    workspacePath: RACE_WORKSPACE_PATH,
    sessionId: RACE_SESSION_ID,
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === 'request-electron-reuse-1'),
    'Electron 首次提交必须进入 canonical session turn endpoint',
  );
  await waitFor(
    () => FakeWebSocket.instances.length === appServerSocketCountBefore + 1,
    'Electron 首次提交必须创建一条 App Server WebSocket',
  );
  const electronSocket = FakeWebSocket.instances.at(-1);
  electronSocket.open();
  await waitFor(
    () => lastWebSocketRequest(electronSocket, 'initialize'),
    'Electron App Server WebSocket 必须完成 initialize',
  );
  replyWebSocket(electronSocket, lastWebSocketRequest(electronSocket, 'initialize'), appServerInitializeResult());
  await waitFor(
    () => lastWebSocketRequest(electronSocket, 'events/subscribe'),
    'Electron App Server initialize 后必须完成 events/subscribe',
  );
  replyWebSocket(electronSocket, lastWebSocketRequest(electronSocket, 'events/subscribe'), appServerSubscribeResult());
  await new Promise((resolve) => setTimeout(resolve, 0));
  const appServerSocketCountAfterFirst = FakeWebSocket.instances.length;
  const initializeCountAfterFirst = electronSocket.sent.filter((message) => message.method === 'initialize').length;
  const subscribeCountAfterFirst = electronSocket.sent.filter((message) => message.method === 'events/subscribe').length;

  sessionTurnInterceptors.push(() => electronTurnResponse(2));
  bridge.postMessage({
    type: 'executeTask',
    text: '复用已有 Electron App Server 连接继续发送。',
    requestId: 'request-electron-reuse-2',
    workspaceId: RACE_WORKSPACE_ID,
    workspacePath: RACE_WORKSPACE_PATH,
    sessionId: RACE_SESSION_ID,
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === 'request-electron-reuse-2'),
    'Electron 第二次提交必须进入 canonical session turn endpoint',
  );
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(
    FakeWebSocket.instances.length,
    appServerSocketCountAfterFirst,
    'Electron 第二次提交不得因错误连接键关闭并重建 App Server WebSocket',
  );
  assert.equal(
    electronSocket.sent.filter((message) => message.method === 'initialize').length,
    initializeCountAfterFirst,
    '复用连接时不得重复 initialize',
  );
  assert.equal(
    electronSocket.sent.filter((message) => message.method === 'events/subscribe').length,
    subscribeCountAfterFirst,
    '复用连接时不得重复 events/subscribe',
  );
  messagesStore.clearRequestBinding('request-electron-reuse-1');
  messagesStore.clearRequestBinding('request-electron-reuse-2');
  sessionTurnInterceptors.push(() => electronTurnResponse(3));
  bridge.postMessage({
    type: 'executeTask',
    pluginCommandId: 'plugin/openai.chatgpt-web/setup',
    text: null,
    requestId: 'request-gpt-setup-command',
    workspaceId: RACE_WORKSPACE_ID,
    workspacePath: RACE_WORKSPACE_PATH,
    sessionId: RACE_SESSION_ID,
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === 'request-gpt-setup-command'),
    '没有正文的插件配置命令也必须进入 canonical Turn 入口',
  );
  assert.equal(
    capturedTurnBodies.find((body) => body.requestId === 'request-gpt-setup-command').pluginCommandId,
    'plugin/openai.chatgpt-web/setup',
    'Renderer Bridge 不得丢弃插件命令身份，否则配置会被当成模型对话',
  );
  messagesStore.clearRequestBinding('request-gpt-setup-command');

  sessionTurnInterceptors.push(() => electronTurnResponse(3));
  bridge.postMessage({
    type: 'executeTask',
    text: null,
    requestId: 'request-workflow-command',
    workflowId: 'plugin/queue/workflow',
    workspaceId: RACE_WORKSPACE_ID,
    workspacePath: RACE_WORKSPACE_PATH,
    sessionId: RACE_SESSION_ID,
  });
  await waitFor(
    () => capturedTurnBodies.some((body) => body.requestId === 'request-workflow-command'),
    '没有正文的工作流选择也必须进入 canonical Turn 入口',
  );
  assert.equal(
    capturedTurnBodies.find((body) => body.requestId === 'request-workflow-command').workflowId,
    'plugin/queue/workflow',
    'Renderer Bridge 不得丢弃 workflowId，否则选择工作流会退化成普通对话',
  );
  messagesStore.clearRequestBinding('request-workflow-command');

  const externalApprovalEvents = [];
  const onExternalApproval = () => externalApprovalEvents.push(true);
  window.addEventListener('magi:mcp-approvals-changed', onExternalApproval);
  for (const eventType of ['tool.approval.requested', 'tool.approval.resolved']) {
    electronSocket.message({
      jsonrpc: '2.0',
      method: `event/${eventType}`,
      params: {
        sequence: 10000 + externalApprovalEvents.length,
        event: {
          ...acceptedEnvelope(),
          event_type: eventType,
          workspace_id: 'other-workspace',
          session_id: 'external-tool-other-client',
          payload: { external: true },
        },
      },
    });
  }
  await waitFor(() => externalApprovalEvents.length === 2, '外部审批必须在工作区和会话过滤前通知全局托盘');
  window.removeEventListener('magi:mcp-approvals-changed', onExternalApproval);

  // 即使更晚发起的读取返回了旧持久版本，也不能覆盖已观察到的 Inbox。
  const versionedInput = { ...queueSnapshotBeforeGuide[1], queueId: 'versioned-input',
    requestId: 'versioned-request', sessionId: RACE_SESSION_ID, workspaceId: RACE_WORKSPACE_ID, revision: 1 };
  let newestInboxRead = false;
  sessionQueueInterceptors.push(() => {
    newestInboxRead = true;
    return jsonResponse({ sessionId: RACE_SESSION_ID, cursor: { epoch: 'persistent-inbox', throughSequence: 100 }, queuedTurns: [] });
  });
  const inboxEvent = (sequence) => electronSocket.message({ jsonrpc: '2.0', method: 'event/session.inbox.changed', params: {
    sequence, event: { ...acceptedEnvelope(), event_id: `versioned-inbox-${sequence}`, event_type: 'session.inbox.changed',
      workspace_id: RACE_WORKSPACE_ID, session_id: RACE_SESSION_ID, payload: {} },
  } });
  inboxEvent(20000);
  await waitFor(() => newestInboxRead, 'Inbox 提交事件必须刷新权威投影');
  await new Promise((resolve) => setImmediate(resolve));
  let staleInboxRead = false;
  sessionQueueInterceptors.push(() => {
    staleInboxRead = true;
    return jsonResponse({ sessionId: RACE_SESSION_ID, cursor: { epoch: 'persistent-inbox', throughSequence: 99 }, queuedTurns: [versionedInput] });
  });
  inboxEvent(20001);
  await waitFor(() => staleInboxRead, '更晚发起的旧版本快照必须被测试读取');
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(messagesStore.messagesState.queuedMessages.length, 0, '旧服务端版本不能复活已消费输入');

  const pluginSelection = {
    pluginId: 'plugin-queue', resourceId: 'resource-queue', version: 3,
    label: '选中资源', summary: '原始上下文摘要', selection: { ids: ['selected-1'] },
  };
  const queueMetadata = [
    { workflowId: 'plugin/queue/workflow', pluginSelections: [pluginSelection] },
    { pluginCommandId: 'plugin/queue/command' },
    { interruptTurnId: 'turn-interrupted-queue', interruptionError: '资源尚未结算' },
  ];
  let metadataInboxRead = false;
  sessionQueueInterceptors.push(() => {
    metadataInboxRead = true;
    return jsonResponse({ sessionId: RACE_SESSION_ID,
      cursor: { epoch: 'persistent-inbox', throughSequence: 101 },
      queuedTurns: queueMetadata.map((metadata, index) => ({
        ...versionedInput, ...metadata, queueId: `metadata-input-${index}`, revision: index + 2,
        guide: { status: 'unsupported_input', targetTurnId: null },
      })) });
  });
  inboxEvent(20002);
  await waitFor(() => metadataInboxRead && messagesStore.messagesState.queuedMessages.length === 3,
    'Inbox metadata must pass through the real bridge projection');
  for (const [index, metadata] of queueMetadata.entries()) {
    const projected = messagesStore.messagesState.queuedMessages[index];
    assert.equal(projected.revision, index + 2, 'retry/edit must keep the original CAS revision');
    for (const [field, value] of Object.entries(metadata)) {
      assert.deepEqual(projected[field], value, `queue projection must preserve ${field}`);
    }
  }

  const goalReadsBeforeUsage = goalRefreshQueries.length;
  electronSocket.message({ jsonrpc: '2.0', method: 'event/session.goal.usage.changed', params: {
    sequence: 20003, event: { ...acceptedEnvelope(), event_id: 'late-goal-usage', event_type: 'session.goal.usage.changed',
      workspace_id: RACE_WORKSPACE_ID, session_id: RACE_SESSION_ID, payload: { goal_id: 'goal-late', step_id: 'late-response' } },
  } });
  await waitFor(() => goalRefreshQueries.length > goalReadsBeforeUsage, 'Turn 结束后的迟到用量提交也必须刷新服务端 Goal 快照');

  console.log('web client bridge golden replay passed');
}, {
  configFile: 'vite.web.config.ts',
  cleanup: () => window.__clearGoldenTimers?.(),
});
