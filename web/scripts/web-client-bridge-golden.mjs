import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

const WORKSPACE_ID = 'workspace-bridge-live-adopt';
const WORKSPACE_PATH = '/tmp/workspace-bridge-live-adopt';
const PARTIAL_WORKSPACE_ID = 'workspace-bridge-partial-scope';
const RACE_WORKSPACE_ID = 'workspace-bridge-bootstrap-race';
const RACE_WORKSPACE_PATH = '/tmp/workspace-bridge-bootstrap-race';
const RACE_SESSION_ID = 'session-bridge-bootstrap-race';
const SESSION_ID = 'session-bridge-live-adopt';
const TURN_ID = 'turn-bridge-live-adopt';
const USER_ITEM_ID = 'user-bridge-live-adopt';
const ACCEPTED_AT = 1780390000000;
let acceptedPublished = false;
let terminalPublished = false;
let summaryMessageCount = 1;
let summaryUpdatedAt = ACCEPTED_AT;
let workspaceListPayload = null;
const capturedTurnBodies = [];
const bootstrapInterceptors = [];
const workspaceSessionsInterceptors = [];
const sessionQueueInterceptors = [];
const bridgeMutationRequests = [];
const bridgeMutationInterceptors = [];
const sessionTurnInterceptors = [];
let messagesSnapshotRequestCount = 0;
let switchSessionRequestCount = 0;
let bootstrapRequestCount = 0;
let workspaceSessionsRequestCount = 0;
let workspaceSessionIsRunning = false;
let workspaceSessionsPayloadOverride = null;
let pendingChangesRequestCount = 0;
let pendingChangesPayloadOverride = null;
let queuedTurnPayloads = [];
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
    eventStreamNextSequence: 1,
    agent: {
      runtimeEpoch: `${workspaceId}:${sessionId}`,
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
      queuedTurnPayloads = queuedTurnPayloads.filter((turn) => turn.queueId !== body.queueId);
      return jsonResponse({
        sessionId: body.sessionId || SESSION_ID,
        queuedTurns: queuedTurnPayloads,
      });
    }
    if (parsed.pathname === '/api/session/switch') {
      switchSessionRequestCount += 1;
      return jsonResponse({
        sessionId: SESSION_ID,
        currentSession: {
          sessionId: SESSION_ID,
          title: '桥接层实时会话',
          createdAt: ACCEPTED_AT,
          updatedAt: summaryUpdatedAt,
          messageCount: summaryMessageCount,
          workspaceId: WORKSPACE_ID,
        },
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
          requiresBudgetIncrease: false,
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
        const requestedSessionId = parsed.searchParams.get('sessionId') || '';
        const requestedWorkspacePath = parsed.searchParams.get('workspacePath') || '';
        const sessions = requestedSessionId
          ? [{
              sessionId: requestedSessionId,
              title: requestedSessionId,
              createdAt: ACCEPTED_AT,
              updatedAt: ACCEPTED_AT,
              messageCount: 0,
              workspaceId: requestedWorkspaceId,
              isRunning: false,
              runningTaskCount: 0,
            }]
          : [];
        return jsonResponse({
          workspace: {
            workspaceId: requestedWorkspaceId,
            rootPath: requestedWorkspacePath,
          },
          sessionId: requestedSessionId,
          sessions,
        });
      }
      const payload = bootstrapPayload();
      return jsonResponse({
        workspace: payload.workspace,
        sessionId: payload.currentSession?.sessionId || '',
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

function acceptedEnvelope() {
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

function contextUsageEnvelope() {
  return {
    event_id: 'event-session-context-usage-streaming',
    event_type: 'session.context.usage.updated',
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
      accuracy: 'estimated',
      token_used: 24_000,
      remaining_tokens: 248_000,
      token_limit: 272_000,
      usage_ratio: 24_000 / 272_000,
      warning_level: 'normal',
      updated_at: ACCEPTED_AT + 2,
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

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

installBrowserGlobals();
installFetchStub();

await withGoldenViteServer(async (server) => {
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  const bridgeModule = await server.ssrLoadModule('/src/shared/bridges/web-client-bridge.ts');
  const messageHandler = await server.ssrLoadModule('/src/lib/message-handler.ts');
  const messagesStore = await server.ssrLoadModule('/src/stores/messages.svelte.ts');
  const turnStore = await server.ssrLoadModule('/src/stores/turn-store.svelte.ts');

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
      phase: 'streaming',
      updatedAt: ACCEPTED_AT + 2,
      eventSequence: 3,
      turnId: TURN_ID,
      callId: 'call-context-streaming',
      resolvedModel: 'gpt-5.6',
    },
    '运行中上下文用量必须通过 SSE 立即进入当前会话 runtime state',
  );

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
    'created live session must refresh the workspace session summary',
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
    () => bootstrapRequestCount > bootstrapRequestsBeforeTerminalEvent,
    'terminal session turn event must finish its authoritative bootstrap refresh',
  );
  await waitFor(
    () => currentSessionSummary(messagesStore)?.updatedAt === summaryUpdatedAt,
    'terminal bootstrap refresh must finish applying before tunnel reconciliation starts',
  );
  assert.equal(
    messagesStore.messagesState.orchestratorRuntimeState?.status,
    'completed',
    'a stale terminal bootstrap must not lift the runtime panel back to running',
  );

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
  const laterRoundLocalArtifact = findArtifactByRequestId(
    messagesStore.messagesState.canonicalTimelineProjection,
    'request-later-round-immediate-feedback',
  );
  assert.equal(
    laterRoundLocalArtifact?.message?.type,
    'user_input',
    '后续轮次必须在提交事件返回前同步投影本地 pending turn，不能只显示等待动画',
  );
  assert.equal(laterRoundLocalArtifact?.message?.content, '后续轮次也必须立刻出现发送反馈。');
  laterRoundAccepted.resolve(jsonResponse({
    sessionId: SESSION_ID,
    entryId: 'timeline-later-round-immediate-feedback',
    eventId: 'event-later-round-immediate-feedback',
    acceptedAt: ACCEPTED_AT + 2250,
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
  messagesStore.addPendingRequest('tunnel-stale-running-state', { resetAntiLiftBack: true });
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
      () => messagesStore.messagesState.isProcessing === false,
      'backend terminal summary must settle stale public tunnel processing state',
    );
    assert.equal(
      workspaceSessionsRequestCount,
      workspaceSessionRequestsBeforeTunnelSync + 2,
      'tunnel terminal sync must recheck the lightweight workspace session summary',
    );
    assert.equal(
      bootstrapRequestCount,
      bootstrapRequestsBeforeTunnelSync + 1,
      'tunnel terminal sync must fetch the full bootstrap exactly once for final content',
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
    type: 'newSession',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
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
    type: 'newSession',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
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
    type: 'workspaceBindingChanged',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: '',
  });
  assert.deepEqual(
    messagesStore.messagesState.workspaceSessionProjection.sessions.map((session) => session.id),
    sessionsBeforeDraft,
    'idempotent workspace-only binding sync must not clear the preserved session list',
  );
  bridge.postMessage({
    type: 'switchSession',
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
  messagesStore.addPendingRequest('busy-before-queued-follow-up');
  bridge.postMessage({
    type: 'executeTask',
    text: '排队消息必须在出队提交时立刻进入主线。',
    requestId: 'request-queued-immediate-feedback',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
    goalMode: true,
    followUpMode: 'queue',
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
  assert.ok(
    findArtifactByRequestId(
      messagesStore.messagesState.canonicalTimelineProjection,
      'request-queued-immediate-feedback',
    ),
    'queued follow-up must become a local pending turn before /api/session/turn resolves',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'queued follow-up may show a local pending turn only while the daemon response is unresolved',
  );
  queuedTurnPayloads = [{
    queueId: 'queued-session-turn-golden',
    queuePosition: 1,
    requestId: 'request-queued-immediate-feedback',
    sessionId: SESSION_ID,
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    acceptedAt: ACCEPTED_AT + 2500,
    content: '排队消息必须在出队提交时立刻进入主线。',
    text: '排队消息必须在出队提交时立刻进入主线。',
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
    retryCount: 0,
  }];
  queuedTurnAccepted.resolve(jsonResponse({
    sessionId: SESSION_ID,
    entryId: 'queued-session-turn-golden',
    eventId: 'event-queued-immediate-feedback',
    acceptedAt: ACCEPTED_AT + 2500,
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
    findArtifactByRequestId(
      messagesStore.messagesState.canonicalTimelineProjection,
      'request-queued-immediate-feedback',
    ),
    undefined,
    'daemon-queued turns must not remain as fake running conversation turns',
  );
  messagesStore.clearPendingRequest('busy-before-queued-follow-up');
  bridge.postMessage({
    type: 'removeQueuedMessage',
    queuedMessageId: 'queued-session-turn-golden',
  });
  await waitFor(
    () => messagesStore.messagesState.queuedMessages.length === 0,
    'removing a queued turn must update the daemon queue and then refresh the UI snapshot',
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

  const switchBootstrapRequests = [];
  pendingChangesPayloadOverride = scopedBootstrapPayloadWithPendingChange(
    WORKSPACE_ID,
    WORKSPACE_PATH,
    SESSION_ID,
    '切换后带变更会话',
  ).pendingChanges;
  bootstrapInterceptors.push((parsed) => {
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
    type: 'switchSession',
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
    workspace: foregroundBootstrap.workspace,
    sessionId: SESSION_ID,
    sessions: [
      { ...backgroundSession, isRunning: true, runningTaskCount: 1 },
      { ...foregroundBootstrap.sessions[1], isRunning: false, runningTaskCount: 0 },
    ],
  };
  bootstrapInterceptors.push(() => jsonResponse(foregroundBootstrap));
  bridge.postMessage({
    type: 'switchSession',
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
    type: 'workspaceBindingChanged',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: '',
  });
  await waitFor(
    () => !messagesStore.messagesState.currentSessionId,
    'workspace-scoped authoritative binding must clear current session before stale URL guard',
  );
  window.location.href = `http://127.0.0.1:38123/web.html?workspaceId=${encodeURIComponent(WORKSPACE_ID)}&workspacePath=${encodeURIComponent(WORKSPACE_PATH)}&sessionId=session-stale-url`;
  bootstrapInterceptors.push((parsed) => {
    staleUrlBootstrapRequests.push(parsed);
    return jsonResponse(scopedBootstrapPayload(
      WORKSPACE_ID,
      WORKSPACE_PATH,
      SESSION_ID,
      'stale URL guard bootstrap',
    ));
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
  const firstTurnLocalArtifact = findArtifactByRequestId(
    messagesStore.messagesState.canonicalTimelineProjection,
    'request-first-turn-immediate-feedback',
  );
  assert.equal(
    firstTurnLocalArtifact?.message?.type,
    'user_input',
    '首条消息必须与 processing 同步进入 canonical 投影，不能产生空白对话首帧',
  );
  assert.equal(firstTurnLocalArtifact?.message?.content, '首条消息必须立刻进入主线。');
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
  assert.ok(
    messagesStore.messagesState.currentSessionId?.startsWith('session-local-request-first-turn-immediate-feedback'),
    'first turn must create a local-only session binding before backend accepted response',
  );
  assert.ok(
    findArtifactByRequestId(
      messagesStore.messagesState.canonicalTimelineProjection,
      'request-first-turn-immediate-feedback',
    ),
    'first turn must become a local pending turn before /api/session/turn resolves',
  );
  firstTurnAccepted.resolve(jsonResponse({
    sessionId: SESSION_ID,
    entryId: 'timeline-first-turn-immediate-feedback',
    eventId: 'event-first-turn-immediate-feedback',
    acceptedAt: ACCEPTED_AT + 2600,
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
  assert.equal(
    workspaceSessionsRequestCount,
    continueWorkspaceSessionsRequestCount,
    'continueTask must not refresh the workspace session list before submission',
  );
  const continueLocalArtifact = findArtifactByRequestId(
    messagesStore.messagesState.canonicalTimelineProjection,
    continueTurnBody.requestId,
  );
  assert.equal(
    continueLocalArtifact?.message?.content,
    '继续',
    'continueTask must render the user input immediately while the backend resumes the runner',
  );
  continueTurnAccepted.resolve(jsonResponse({
    sessionId: SESSION_ID,
    entryId: 'timeline-continue-no-refresh',
    eventId: 'event-continue-no-refresh',
    acceptedAt: ACCEPTED_AT + 2700,
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
  assert.equal(workspaceSessionsRequestCount, continueWorkspaceSessionsRequestCount);
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
  bridge.postMessage({
    type: 'workspaceBindingChanged',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
  });
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

    bridge.postMessage({
      type: 'workspaceBindingChanged',
      workspaceId: RACE_WORKSPACE_ID,
      workspacePath: RACE_WORKSPACE_PATH,
      sessionId: RACE_SESSION_ID,
    });
    bootstrapInterceptors.push((parsed) => {
      raceBootstrapRequests.push(parsed);
      return secondBootstrap.promise.then(() => jsonResponse(scopedBootstrapPayload(
        RACE_WORKSPACE_ID,
        RACE_WORKSPACE_PATH,
        RACE_SESSION_ID,
        '启动竞态会话',
      )));
    });
    bridge.postMessage({ type: 'requestState' });
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
  bridge.postMessage({
    type: 'workspaceBindingChanged',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: missingSessionId,
  });
  bootstrapInterceptors.push(() => new Response(
    JSON.stringify({ message: `session 不存在: ${missingSessionId}` }),
    { status: 404, headers: { 'content-type': 'application/json' } },
  ));
  bridge.postMessage({ type: 'requestState' });
  await waitFor(
    () => messagesStore.messagesState.currentWorkspaceId === WORKSPACE_ID
      && !messagesStore.messagesState.currentSessionId,
    'unknown explicit session bootstrap must keep workspace but clear the invalid session binding',
  );
  assert.equal(
    window.location.href.includes('sessionId='),
    false,
    'unknown explicit session bootstrap must remove the invalid session from URL',
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
  bridge.postMessage({
    type: 'workspaceBindingChanged',
    workspaceId: WORKSPACE_ID,
    workspacePath: WORKSPACE_PATH,
    sessionId: SESSION_ID,
  });
  bootstrapInterceptors.push(() => jsonResponse(scopedBootstrapPayload(
    WORKSPACE_ID,
    WORKSPACE_PATH,
    SESSION_ID,
    '最新会话',
  )));
  bridge.postMessage({ type: 'requestState' });
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

  console.log('web client bridge golden replay passed');
}, {
  configFile: 'vite.web.config.ts',
  cleanup: () => window.__clearGoldenTimers?.(),
});
