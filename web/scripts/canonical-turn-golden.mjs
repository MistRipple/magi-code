import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const reducer = await server.ssrLoadModule('/src/stores/turn-reducer.ts');
  const projection = await server.ssrLoadModule('/src/stores/turn-projection.ts');
  const bridgeRuntime = await server.ssrLoadModule('/src/shared/bridges/bridge-runtime.ts');
  installGoldenMemoryBridge(bridgeRuntime);
  const messagesStore = await server.ssrLoadModule('/src/stores/messages.svelte.ts');
  const dataHandlers = await server.ssrLoadModule('/src/lib/data-message-handlers.ts');
  const timelineRenderItems = await server.ssrLoadModule('/src/lib/timeline-render-items.ts');
  const agentOutput = await server.ssrLoadModule('/src/lib/agent-output.ts');
  const contract = await server.ssrLoadModule('/src/shared/bridges/rust-daemon-contract.ts');
  const viewImagePreview = await server.ssrLoadModule('/src/lib/view-image-preview.ts');
  const canonicalProtocol = await server.ssrLoadModule('/src/shared/protocol/canonical-turn.ts');
  const blockRegistry = await server.ssrLoadModule('/src/lib/block-registry.ts');
  const markdownUrl = await server.ssrLoadModule('/src/lib/markdown-url.ts');
  runGoldenReplay(reducer, projection, messagesStore, dataHandlers, timelineRenderItems, agentOutput, contract, viewImagePreview, canonicalProtocol, blockRegistry, markdownUrl);
  console.log('canonical turn golden replay passed');
}, { configFile: 'vite.web.config.ts' });

function installGoldenMemoryBridge(bridgeRuntime) {
  let bridgeState;
  const listeners = new Set();
  bridgeRuntime.setClientBridge({
    kind: 'web',
    postMessage() {},
    onMessage(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    getState() {
      return bridgeState;
    },
    setState(nextState) {
      bridgeState = nextState;
    },
    getInitialSessionId() {
      return '';
    },
    getInitialLocale() {
      return 'zh-CN';
    },
    notifyReady() {},
  });
}

function runGoldenReplay(reducer, projection, messagesStore, dataHandlers, timelineRenderItems, agentOutput, contract, viewImagePreview, canonicalProtocol, blockRegistry, markdownUrl) {
  const cases = [
    acceptedFirstFrameCase(),
    ordinaryChatCase(),
    toolFirstCase(),
    singleToolCase(),
    multiToolOutOfOrderCase(),
    failedToolCase(),
    cancelledToolCase(),
    terminalEmptyAssistantCase(),
    localFailureCase(),
  ];

  for (const testCase of cases) {
    const live = replayLive(reducer, testCase);
    const liveSignature = projectionSignature(projection.buildCanonicalTimelineProjection(live));
    assert.deepEqual(liveSignature, testCase.expected, `${testCase.name}: live signature mismatch`);

    const bootstrap = reducer.replaceCanonicalTurns(testCase.sessionId, live.turns);
    const bootstrapSignature = projectionSignature(projection.buildCanonicalTimelineProjection(bootstrap));
    assert.deepEqual(bootstrapSignature, liveSignature, `${testCase.name}: bootstrap signature mismatch`);

    const durableReload = reducer.replaceCanonicalTurns(testCase.sessionId, bootstrap.turns);
    const durableSignature = projectionSignature(projection.buildCanonicalTimelineProjection(durableReload));
    assert.deepEqual(durableSignature, liveSignature, `${testCase.name}: durable reload signature mismatch`);

    const duplicateReplay = testCase.events.reduce((state, event) => reducer.reduceCanonicalTurnEvent(state, event).state, live);
    assert.deepEqual(
      projectionSignature(projection.buildCanonicalTimelineProjection(duplicateReplay)),
      liveSignature,
      `${testCase.name}: duplicate event replay must be idempotent`,
    );
  }

  assertAcceptedFirstFrameRunning(reducer, projection);
  assertLocalPendingTurnIsReplacedByAcceptedCanonicalTurn(reducer, projection);
  assertLocalPendingImageSurvivesRegularAcceptedTurn(reducer, projection);
  assertCrossSessionCanonicalEventIsRejected(reducer, projection);
  assertReplaceCanonicalTurnsFiltersForeignSessions(reducer);
  assertMessagesStoreRejectsForeignSessionProjection(reducer, projection, messagesStore);
  assertMessagesStoreAdoptsLiveCanonicalEventForEmptySession(dataHandlers, messagesStore);
  assertWorkspaceDraftPreservesSessionList(dataHandlers, messagesStore);
  assertSameSessionBootstrapAppliesAuthoritativeSnapshotWhenProjectionIsEmpty(dataHandlers, messagesStore);
  assertSameSessionStaleIdleBootstrapPreservesActiveTurn(dataHandlers, messagesStore);
  assertMessagesStoreSettlesProcessingFromLiveTerminalCanonicalEvent(dataHandlers, messagesStore);
  assertHistoricalTerminalReplayDoesNotClearCurrentTurn(dataHandlers, messagesStore);
  assertTerminalLateUpsertIsIgnored(reducer, projection);
  assertTerminalLateTurnStartedIsIgnored(reducer, projection);
  assertSupersededTurnDisappearsAndRejectsLateEvents(reducer, projection);
  assertFailedAssistantTextUsesPlainMessageShell(reducer, projection);
  assertSplitToolStartedAndResultCollapseIntoOneCard(reducer, projection);
  assertCancelledToolShowsTurnResponseDuration(reducer, projection);
  assertFailedToolWithoutAssistantShowsTurnResponseDuration(reducer, projection);
  assertUserImageMetadataProjectsToMessage(reducer, projection, timelineRenderItems);
  assertViewImageToolResultProjectsAsPreview(reducer, projection, viewImagePreview);
  assertAgentSpawnToolCardStaysOnMainlineAndTaskTabsFilterByTaskId(reducer, projection, timelineRenderItems);
  assertRuntimeInternalAgentWaitIsHiddenFromCanonicalMainline(reducer, projection, timelineRenderItems);
  assertParallelAgentSpawnUsesTaskIdTabs(reducer, projection, timelineRenderItems);
  assertAgentTerminalOutputExtractsFinalText(agentOutput);
  assertBootstrapProcessingStateFromRunningCanonicalTurn(contract);
  assertBootstrapProcessingStateIgnoresForeignSessionRunningTurn(contract);
  assertBootstrapProcessingStateIgnoresTerminalCanonicalTurn(contract);
  assertBootstrapCarriesPendingChanges(contract);
  assertBootstrapFiltersForeignWorkspaceSessions(contract);
  assertBootstrapExplicitWorkspaceWinsOverForeignCurrentSession(contract);
  assertMessagesStoreClearsLocalPendingFromAuthoritativeIdle(messagesStore);
  assertHistoricalIdlePreservesBoundLocalSubmission(messagesStore);
  assertHistoricalForcedIdlePreservesBoundLocalSubmission(dataHandlers, messagesStore);
  assertSessionSwitchClearsExecutionState(messagesStore);
  assertLocalTurnSubmissionStartsAtomically(messagesStore);
  assertCanonicalTurnModelRejectsSnakeCase(canonicalProtocol);
  assertCanonicalStreamPayloadParsesWithoutSnapshots(canonicalProtocol);
  assertCanonicalStreamDeltaUpdatesOneItem(reducer, projection);
  assertCanonicalBlocksProjectToFirstClassCards(reducer, projection);
  assertBootstrapSeedsCanonicalEventWatermark(reducer);
  assertUnknownCanonicalBlockHasNoTextFallback(blockRegistry);
  assertMarkdownUrlSanitizerKeepsOnlyValidFileLinks(markdownUrl);
}

function assertSupersededTurnDisappearsAndRejectsLateEvents(reducer, projection) {
  const c = baseCase('superseded-turn', 'session-golden-superseded', 'turn-golden-superseded', 11_500);
  const cancelledTurn = turn(c, 'cancelled', [
    user(c, 1, '需要修改的消息'),
    assistantText(c, 2, 'assistant-cancelled', '未完成响应', 'cancelled'),
  ]);
  let state = reducer.replaceCanonicalTurns(c.sessionId, [cancelledTurn]);
  assert.equal(
    projection.buildCanonicalTimelineProjection(state).artifacts.length,
    2,
    'cancelled turn remains visible before the user submits an edit',
  );

  const superseded = reducer.reduceCanonicalTurnEvent(state, event(c, 2, 'turn_superseded', {
    turn: {
      ...cancelledTurn,
      status: 'superseded',
      metadata: { supersededReason: 'user_edit' },
    },
  }));
  assert.equal(superseded.error, undefined);
  assert.equal(superseded.changed, true);
  state = superseded.state;
  assert.equal(
    projection.buildCanonicalTimelineProjection(state).artifacts.length,
    0,
    'superseded turn must be removed from the normal timeline projection',
  );

  const lateCancelled = reducer.reduceCanonicalTurnEvent(state, event(c, 3, 'turn_completed', {
    turn: cancelledTurn,
  }));
  assert.equal(lateCancelled.error, undefined);
  assert.equal(lateCancelled.changed, false, 'late events from a superseded turn must be ignored');
  assert.equal(lateCancelled.state.turns[0].status, 'superseded');
}

function assertLocalTurnSubmissionStartsAtomically(messagesStore) {
  messagesStore.messagesState.currentWorkspaceId = 'workspace-local-submit';
  messagesStore.messagesState.currentWorkspacePath = '/tmp/workspace-local-submit';
  messagesStore.setCurrentSessionId('session-local-submit');
  messagesStore.clearAllMessages({
    persist: false,
    resetTimelineView: true,
    resetPanelState: true,
    skipAntiLiftBack: true,
  });
  messagesStore.beginLocalTurnSubmission({
    requestId: 'request-local-submit',
    placeholderMessageId: 'assistant-local-submit',
    startedAt: 25_000,
    source: 'orchestrator',
    agent: 'orchestrator',
  });

  assert.equal(messagesStore.messagesState.isProcessing, true);
  assert.equal(messagesStore.messagesState.thinkingStartAt, 25_000);
  assert.equal(messagesStore.messagesState.pendingRequests.has('request-local-submit'), true);
  assert.equal(messagesStore.messagesState.activeMessageIds.has('assistant-local-submit'), true);
  assert.equal(messagesStore.messagesState.processingActor.source, 'orchestrator');

  messagesStore.settleAuthoritativeIdleState();
  messagesStore.setCurrentSessionId(null);
}

function assertMarkdownUrlSanitizerKeepsOnlyValidFileLinks(markdownUrl) {
  const linkContext = { type: 'link', tag: 'a' };
  const imageContext = { type: 'image', tag: 'img' };
  assert.equal(
    markdownUrl.sanitizeMarkdownUrl('file:///Users/xie/code/TEST/README.md', linkContext),
    'file:///Users/xie/code/TEST/README.md',
    '可验证的本地文件 Markdown 链接必须保留给文件预览处理器',
  );
  assert.equal(
    markdownUrl.sanitizeMarkdownUrl('file:///Users/xie/code/TEST/README.md', imageContext),
    '',
    '本地文件协议不得作为图片资源直接加载',
  );
  assert.equal(
    markdownUrl.sanitizeMarkdownUrl('javascript:alert(1)', linkContext),
    '',
    '危险协议必须继续被默认安全策略拒绝',
  );
}

function assertUnknownCanonicalBlockHasNoTextFallback(blockRegistry) {
  assert.equal(
    blockRegistry.getBlockRenderer({ type: 'future_unknown_block' }),
    undefined,
    'unknown structured blocks must not be silently rendered as plain text',
  );
}

function assertBootstrapSeedsCanonicalEventWatermark(reducer) {
  const c = baseCase('bootstrap-watermark', 'session-golden-bootstrap-watermark', 'turn-golden-bootstrap-watermark', 14_000);
  const assistantItem = assistantText(c, 1, 'assistant-stream', '快照内容', 'running');
  assistantItem.itemVersion = 4;
  const state = reducer.replaceCanonicalTurns(
    c.sessionId,
    [turn(c, 'running', [assistantItem])],
    100,
  );
  assert.equal(state.lastAppliedEventSeq, 100, 'bootstrap must seed the reducer event watermark');
  const replayed = reducer.reduceCanonicalTurnEvent(state, event(c, 100, 'turn_item_upsert', {
    stream: {
      itemId: assistantItem.itemId,
      itemVersion: 5,
      itemStatus: 'running',
      baseContentLength: 4,
      delta: '重复',
      contentLength: 6,
      reset: false,
    },
  }));
  assert.equal(replayed.changed, false, 'event at the bootstrap watermark must not replay');
  assert.equal(replayed.state.turns[0].items[0].content, '快照内容');
}

function assertCanonicalStreamPayloadParsesWithoutSnapshots(canonicalProtocol) {
  const event = canonicalProtocol.parseCanonicalTurnEventPayload({
    canonical_schema_version: 'canonical-turn.v1',
    canonical_event_kind: 'turn_item_upsert',
    session_id: 'session-stream-payload',
    turn_id: 'turn-stream-payload',
    turn_seq: 7,
    canonical_item_id: 'assistant-stream',
    canonical_item_version: 3,
    canonical_item_status: 'running',
    stream_base_content_length: 2,
    stream_delta: '世界',
    stream_content_length: 4,
    stream_reset: false,
  }, {
    eventId: 'event-stream-payload',
    eventSeq: 12,
    occurredAt: 100,
  });

  assert.ok(event, 'delta-only canonical payload should parse without canonical_turn/canonical_item');
  assert.equal(event.turn, undefined, 'delta-only stream payload must not require a full turn snapshot');
  assert.equal(event.item, undefined, 'delta-only stream payload must not require a full item snapshot');
  assert.deepEqual(event.stream, {
    itemId: 'assistant-stream',
    itemVersion: 3,
    itemStatus: 'running',
    baseContentLength: 2,
    delta: '世界',
    contentLength: 4,
    reset: false,
  });
}

function assertCanonicalStreamDeltaUpdatesOneItem(reducer, projection) {
  const c = baseCase('incremental-stream-delta', 'session-golden-incremental-stream', 'turn-golden-incremental-stream', 12_000);
  const userItem = user(c, 1, '请流式回复。');
  const assistantItem = assistantText(c, 2, 'assistant-stream', '你', 'running');
  assistantItem.itemVersion = 1;
  let state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'running', [userItem, assistantItem]),
  ]);
  const initialProjection = projection.buildCanonicalTimelineProjection(state);
  const initialUserArtifact = findArtifactByTurnItemId(initialProjection, userItem.itemId);
  const initialAssistantArtifact = findArtifactByTurnItemId(initialProjection, assistantItem.itemId);

  const appended = reducer.reduceCanonicalTurnEvent(state, event(c, 2, 'turn_item_upsert', {
    stream: {
      itemId: assistantItem.itemId,
      itemVersion: 2,
      itemStatus: 'running',
      baseContentLength: 1,
      delta: '好',
      contentLength: 2,
      reset: false,
    },
  }));
  assert.equal(appended.error, undefined);
  assert.equal(appended.changed, true, 'new stream item version should update reducer state');
  assert.deepEqual(appended.changedTurnIds, [c.turnId]);
  assert.equal(appended.state.turns[0].items[1].content, '你好');
  assert.equal(appended.state.turns[0].items[1].itemVersion, 2);
  assert.equal(
    appended.state.turns[0].items[0],
    state.turns[0].items[0],
    'delta update must preserve unchanged item identity',
  );

  const incrementalProjection = projection.updateCanonicalTimelineProjection(
    initialProjection,
    appended.state,
    appended.changedTurnIds,
  );
  assert.equal(
    findArtifactByTurnItemId(incrementalProjection, userItem.itemId),
    initialUserArtifact,
    'incremental projection must retain unchanged historical artifacts',
  );
  assert.notEqual(
    findArtifactByTurnItemId(incrementalProjection, assistantItem.itemId),
    initialAssistantArtifact,
    'changed stream item must receive a fresh artifact projection',
  );

  state = appended.state;
  const duplicate = reducer.reduceCanonicalTurnEvent(state, event(c, 3, 'turn_item_upsert', {
    stream: {
      itemId: assistantItem.itemId,
      itemVersion: 2,
      itemStatus: 'running',
      baseContentLength: 1,
      delta: '好',
      contentLength: 2,
      reset: false,
    },
  }));
  assert.equal(duplicate.changed, false, 'duplicate item version must not append delta twice');
  assert.equal(duplicate.cursorAdvanced, true, 'duplicate facts should still advance the transport cursor');
  assert.equal(duplicate.state.lastAppliedEventSeq, 3);
  assert.equal(duplicate.state.turns[0].items[1].content, '你好');

  const conflictingDuplicate = reducer.reduceCanonicalTurnEvent(state, event(c, 4, 'turn_item_upsert', {
    stream: {
      itemId: assistantItem.itemId,
      itemVersion: 2,
      itemStatus: 'running',
      baseContentLength: 1,
      delta: '错',
      contentLength: 2,
      reset: false,
    },
  }));
  assert.match(
    conflictingDuplicate.error,
    /reused itemVersion 2 with different facts/,
    'same stream version with different facts must trigger protocol recovery',
  );

  const reset = reducer.reduceCanonicalTurnEvent(state, event(c, 5, 'turn_item_upsert', {
    stream: {
      itemId: assistantItem.itemId,
      itemVersion: 3,
      itemStatus: 'running',
      baseContentLength: 2,
      delta: '重新',
      contentLength: 2,
      reset: true,
    },
  }));
  assert.equal(reset.changed, true);
  assert.equal(reset.state.turns[0].items[1].content, '重新');

  const stale = reducer.reduceCanonicalTurnEvent(reset.state, event(c, 6, 'turn_item_upsert', {
    stream: {
      itemId: assistantItem.itemId,
      itemVersion: 2,
      itemStatus: 'running',
      baseContentLength: 2,
      delta: '旧帧',
      contentLength: 4,
      reset: false,
    },
  }));
  assert.equal(stale.changed, false, 'older item version must be rejected even with a newer event sequence');
  assert.equal(stale.state.turns[0].items[1].content, '重新');

  const snapshotItem = assistantText(c, 2, 'assistant-stream', '你好', 'running');
  const snapshotState = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'running', [userItem, snapshotItem]),
  ], 5);
  const overlapped = reducer.reduceCanonicalTurnEvent(snapshotState, event(c, 7, 'turn_item_upsert', {
    stream: {
      itemId: snapshotItem.itemId,
      itemVersion: 2,
      itemStatus: 'running',
      baseContentLength: 1,
      delta: '好呀',
      contentLength: 3,
      reset: false,
    },
  }));
  assert.equal(overlapped.error, undefined, 'bootstrap-covered delta prefix should reconcile without recovery');
  assert.equal(overlapped.state.turns[0].items[1].content, '你好呀');
}

function assertCanonicalBlocksProjectToFirstClassCards(reducer, projection) {
  const c = baseCase('canonical-blocks', 'session-golden-canonical-blocks', 'turn-golden-canonical-blocks', 13_000);
  const blockItem = assistantText(c, 1, 'assistant-blocks', '', 'completed');
  blockItem.blocks = [
    { type: 'code', blockId: 'code-main', language: 'ts', content: 'const ready = true;' },
    { type: 'plan', blockId: 'plan-main', goal: '完成消息链路重构', constraints: ['单一 canonical 权威链路'] },
    { type: 'file_change', filePath: 'src/main.ts', changeType: 'modify', additions: 2, deletions: 1 },
  ];
  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'completed', [blockItem], { completedAt: 13_100, responseDurationMs: 100 }),
  ]);
  const artifact = findArtifactByTurnItemId(
    projection.buildCanonicalTimelineProjection(state),
    blockItem.itemId,
  );
  assert.ok(artifact, 'renderable canonical blocks must create an artifact even when text content is empty');
  assert.deepEqual(
    artifact.message.blocks.map((block) => block.type),
    ['code', 'plan', 'file_change'],
    'canonical blocks must remain first-class cards instead of being discarded',
  );
}

function assertWorkspaceDraftPreservesSessionList(dataHandlers, messagesStore) {
  const workspaceId = 'workspace-golden-draft-preserve';
  const workspacePath = '/tmp/workspace-golden-draft-preserve';
  const sessions = [
    {
      id: 'session-golden-draft-preserve-a',
      title: '保留会话 A',
      createdAt: 20_000,
      updatedAt: 20_000,
      messageCount: 2,
      workspaceId,
    },
    {
      id: 'session-golden-draft-preserve-b',
      title: '保留会话 B',
      createdAt: 21_000,
      updatedAt: 21_000,
      messageCount: 1,
      workspaceId,
    },
  ];

  messagesStore.messagesState.currentWorkspaceId = workspaceId;
  messagesStore.messagesState.currentWorkspacePath = workspacePath;
  messagesStore.updateSessions(sessions);
  messagesStore.setCurrentSessionId(sessions[0].id);

  dataHandlers.handleUnifiedData({
    id: 'golden-workspace-draft-started',
    category: 'data',
    type: 'system',
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: 'completed',
    blocks: [],
    timestamp: 22_000,
    updatedAt: 22_000,
    data: {
      dataType: 'workspaceDraftStarted',
      payload: { workspaceId, workspacePath },
    },
  });

  assert.equal(messagesStore.messagesState.currentSessionId, null,
    'starting a new-session draft must clear only the active session pointer');
  assert.deepEqual(
    messagesStore.messagesState.sessions.map((session) => session.id),
    sessions.map((session) => session.id),
    'starting a draft must preserve the persisted workspace session list',
  );
  assert.deepEqual(
    messagesStore.messagesState.appState?.sessions?.map((session) => session.id),
    sessions.map((session) => session.id),
    'draft app state must expose the same preserved session list',
  );
}

function assertAgentTerminalOutputExtractsFinalText(agentOutput) {
  const task = {
    output_refs: [
      JSON.stringify({
        blocks: [
          { type: 'tool_call', content: 'shell_exec: ok' },
          { type: 'text', content: '代理完成：发现知识库索引卡住在加载态。' },
        ],
      }),
    ],
  };
  assert.deepEqual(
    agentOutput.agentTerminalOutput(task),
    {
      text: '代理完成：发现知识库索引卡住在加载态。',
      sourceRefIndex: 0,
      truncated: false,
    },
    'agent tab should render the terminal final_text from task output refs',
  );

  const toolOnly = {
    output_refs: [
      JSON.stringify({
        blocks: [
          { type: 'tool_call', content: 'shell_exec: ok' },
          { type: 'text', content: '' },
        ],
      }),
    ],
  };
  assert.equal(
    agentOutput.agentTerminalOutput(toolOnly),
    null,
    'tool-only JSON output refs must not leak raw transport JSON into the agent tab',
  );
}

function assertCanonicalTurnModelRejectsSnakeCase(canonicalProtocol) {
  const snakeItem = {
    session_id: 'session-snake-rejected',
    turn_id: 'turn-snake-rejected',
    turn_seq: 1,
    item_id: 'item-snake-rejected',
    item_seq: 1,
    kind: 'assistant_text',
    created_at: 1780390000000,
    updated_at: 1780390000000,
    status: 'completed',
    source_thread_id: 'thread-snake-rejected',
    visibility: { renderable: true },
  };
  const snakeTurn = {
    session_id: 'session-snake-rejected',
    turn_id: 'turn-snake-rejected',
    turn_seq: 1,
    accepted_at: 1780390000000,
    status: 'completed',
    items: [snakeItem],
  };

  assert.equal(
    canonicalProtocol.normalizeCanonicalTurnItem(snakeItem),
    undefined,
    'canonical item model must not accept snake_case fields',
  );
  assert.equal(
    canonicalProtocol.normalizeCanonicalTurn(snakeTurn),
    undefined,
    'canonical turn model must not accept snake_case fields',
  );
  assert.equal(
    canonicalProtocol.normalizeCanonicalTurnItem({
      sessionId: 'session-invalid-block',
      turnId: 'turn-invalid-block',
      turnSeq: 1,
      itemId: 'item-invalid-block',
      itemSeq: 1,
      kind: 'assistant_text',
      createdAt: 1,
      updatedAt: 1,
      status: 'completed',
      sourceThreadId: 'thread-invalid-block',
      visibility: { renderable: true },
      blocks: [{ type: 'future_unknown_block', content: '不能静默进入 projection' }],
    }),
    undefined,
    'canonical parser must reject unsupported structured blocks at the protocol boundary',
  );
}

function assertAgentSpawnToolCardStaysOnMainlineAndTaskTabsFilterByTaskId(reducer, projection, timelineRenderItems) {
  const c = baseCase('agent-spawn-task-tab', 'session-golden-agent-spawn', 'turn-golden-agent-spawn', 9400);
  const userItem = user(c, 1, '请用任务系统完成一次验证。');
  const spawnItem = agentSpawnTool(c, 2, 'spawn-a', 'call-spawn-a', 'executor', '实现验证员', 'task-child-a', 'completed');
  spawnItem.worker = { taskId: 'task-root', title: 'agent_spawn' };
  const childTool = tool(c, 3, 'child-tool-a', 'call-child-a', 'printf child', 'completed', { stdout: 'child' });
  childTool.worker = { taskId: 'task-child-a', workerId: 'worker-child-a', roleId: 'executor', title: 'shell_exec' };
  const childFinal = assistantText(c, 4, 'child-final-a', '代理已完成验证。', 'completed');
  childFinal.worker = { taskId: 'task-child-a', workerId: 'worker-child-a', roleId: 'executor', title: '最终回复' };
  const rootFinal = assistantText(c, 5, 'root-final', '我已汇总代理结果，验证通过。', 'completed');
  rootFinal.worker = { taskId: 'task-root', title: '最终回复' };

  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'completed', [userItem, spawnItem, childTool, childFinal, rootFinal], { completedAt: 9500, responseDurationMs: 100 }),
  ]);
  const projectionValue = projection.buildCanonicalTimelineProjection(state);
  assert.ok(projectionValue, 'agent_spawn projection should exist');
  assert.deepEqual(
    projectionValue.threadRenderEntries.map((entry) => entry.artifactId),
    [
      `turn:${c.turnId}:user-message`,
      `turn:${c.turnId}:spawn-a`,
      `turn:${c.turnId}:root-final`,
    ],
    'mainline should keep user message, agent_spawn ToolCall card and root final only',
  );

  const spawnArtifact = findArtifactByTurnItemId(projectionValue, 'spawn-a');
  assert.ok(spawnArtifact, 'agent_spawn artifact should exist');
  const spawnBlock = spawnArtifact.message.blocks?.find((block) => block.type === 'tool_call');
  assert.equal(spawnBlock?.toolCall?.name, 'agent_spawn', 'agent_spawn must render as a normal ToolCall block');
  assert.equal(spawnArtifact.message.metadata?.taskId, 'task-root', 'root task id remains factual metadata');
  assert.equal(spawnArtifact.taskId, undefined, 'root agent_spawn card must stay on mainline even when it carries root taskId');

  const childTaskItems = timelineRenderItems.buildTimelineRenderItems(projectionValue, 'task', 'task-child-a');
  assert.deepEqual(
    childTaskItems.map((entry) => entry.message.metadata?.turnItemId),
    ['child-tool-a', 'child-final-a'],
    'task context should show only artifacts whose metadata.taskId equals the child task id',
  );
  assert.deepEqual(
    childTaskItems.map((entry) => entry.message.metadata?.roleId),
    ['executor', 'executor'],
    'role remains display metadata and does not become the tab key',
  );

  const durableReload = reducer.replaceCanonicalTurns(c.sessionId, state.turns);
  const durableProjection = projection.buildCanonicalTimelineProjection(durableReload);
  assert.ok(durableProjection, 'durable reload projection should exist');
  assert.deepEqual(
    timelineRenderItems.buildTimelineRenderItems(durableProjection, 'task', 'task-child-a')
      .map((entry) => entry.message.metadata?.turnItemId),
    childTaskItems.map((entry) => entry.message.metadata?.turnItemId),
    'task transcript should survive snapshot reload by taskId without role-tab state',
  );
}

function assertRuntimeInternalAgentWaitIsHiddenFromCanonicalMainline(reducer, projection, timelineRenderItems) {
  const c = baseCase('agent-wait-hidden', 'session-golden-agent-wait-hidden', 'turn-golden-agent-wait-hidden', 9550);
  const userItem = user(c, 1, '请等待子代理完成。');
  const spawnItem = agentSpawnTool(c, 2, 'spawn-a', 'call-spawn-a', 'executor', '验证代理', 'task-child-a', 'completed');
  spawnItem.worker = { taskId: 'task-root', title: 'agent_spawn' };
  const waitItem = agentWaitTool(c, 3, 'wait-a', 'call-wait-a', 'task-child-a', 'completed');
  waitItem.worker = { taskId: 'task-root', title: 'agent_wait' };
  const childFinal = assistantText(c, 4, 'child-final-a', '子代理已完成。', 'completed');
  childFinal.worker = { taskId: 'task-child-a', workerId: 'worker-child-a', roleId: 'executor', title: '最终回复' };
  const rootFinal = assistantText(c, 5, 'root-final', '已收到子代理结果。', 'completed');
  rootFinal.worker = { taskId: 'task-root', title: '最终回复' };

  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'completed', [userItem, spawnItem, waitItem, childFinal, rootFinal], { completedAt: 9650, responseDurationMs: 100 }),
  ]);
  const projectionValue = projection.buildCanonicalTimelineProjection(state);
  assert.ok(projectionValue, 'agent_wait hidden projection should exist');
  assert.deepEqual(
    projectionValue.threadRenderEntries.map((entry) => entry.artifactId),
    [
      `turn:${c.turnId}:user-message`,
      `turn:${c.turnId}:spawn-a`,
      `turn:${c.turnId}:root-final`,
    ],
    'runtime-internal agent_wait must not appear in mainline render entries',
  );
  assert.equal(
    findArtifactByTurnItemId(projectionValue, 'wait-a'),
    undefined,
    'runtime-internal agent_wait must not create a render artifact',
  );
  assert.ok(
    !timelineRenderItems.buildTimelineRenderItems(projectionValue, 'task', 'task-root')
      .map((entry) => entry.message.metadata?.turnItemId)
      .includes('wait-a'),
    'root task tab must not reintroduce runtime-internal agent_wait',
  );
  assert.deepEqual(
    timelineRenderItems.buildTimelineRenderItems(projectionValue, 'task', 'task-child-a')
      .map((entry) => entry.message.metadata?.turnItemId),
    ['child-final-a'],
    'child task tab should still show child task artifacts',
  );
}

function assertParallelAgentSpawnUsesTaskIdTabs(reducer, projection, timelineRenderItems) {
  const c = baseCase('parallel-agent-spawn-task-tabs', 'session-golden-parallel-agent', 'turn-golden-parallel-agent', 9600);
  const userItem = user(c, 1, '请让两个 executor 并行处理。');
  const spawnA = agentSpawnTool(c, 2, 'spawn-a', 'call-spawn-a', 'executor', '登录流程审查员', 'task-login-review', 'completed');
  const spawnB = agentSpawnTool(c, 3, 'spawn-b', 'call-spawn-b', 'executor', '权限流程审查员', 'task-auth-review', 'completed');
  const finalA = assistantText(c, 4, 'final-a', '登录流程审查完成。', 'completed');
  finalA.worker = { taskId: 'task-login-review', workerId: 'worker-login-review', roleId: 'executor', title: '最终回复' };
  const finalB = assistantText(c, 5, 'final-b', '权限流程审查完成。', 'completed');
  finalB.worker = { taskId: 'task-auth-review', workerId: 'worker-auth-review', roleId: 'executor', title: '最终回复' };

  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'completed', [userItem, spawnA, spawnB, finalA, finalB], { completedAt: 9700, responseDurationMs: 100 }),
  ]);
  const projectionValue = projection.buildCanonicalTimelineProjection(state);
  assert.ok(projectionValue, 'parallel agent projection should exist');
  assert.deepEqual(
    projectionValue.threadRenderEntries.map((entry) => entry.artifactId),
    [
      `turn:${c.turnId}:user-message`,
      `turn:${c.turnId}:spawn-a`,
      `turn:${c.turnId}:spawn-b`,
    ],
    'parallel agent_spawn calls should remain as separate mainline ToolCall cards',
  );
  assert.deepEqual(
    timelineRenderItems.buildTimelineRenderItems(projectionValue, 'task', 'task-login-review')
      .map((entry) => entry.message.metadata?.turnItemId),
    ['final-a'],
    'first executor instance should open by its own taskId',
  );
  assert.deepEqual(
    timelineRenderItems.buildTimelineRenderItems(projectionValue, 'task', 'task-auth-review')
      .map((entry) => entry.message.metadata?.turnItemId),
    ['final-b'],
    'second executor instance should open by its own taskId, not by shared role',
  );
}

function replayLive(reducer, testCase) {
  let state = reducer.createCanonicalTurnReducerState(testCase.sessionId);
  for (const event of testCase.events) {
    const result = reducer.reduceCanonicalTurnEvent(state, event);
    assert.equal(result.error, undefined, `${testCase.name}: ${result.error || ''}`);
    state = result.state;
  }
  return state;
}

function projectionSignature(value) {
  assert.ok(value, 'projection should exist');
  const artifactIds = value.artifacts.map((artifact) => artifact.artifactId);
  assert.equal(
    new Set(artifactIds).size,
    artifactIds.length,
    'projection artifacts must be unique by artifactId',
  );
  const artifactsById = new Map(value.artifacts.map((artifact) => [artifact.artifactId, artifact]));
  return value.threadRenderEntries.map((entry) => {
    const artifact = artifactsById.get(entry.artifactId);
    assert.ok(artifact, `missing artifact ${entry.artifactId}`);
    const message = artifact.message;
    const toolCall = message.blocks?.find((block) => block.type === 'tool_call')?.toolCall;
    return {
      kind: artifact.kind,
      itemKind: message.metadata?.turnItemKind,
      itemSeq: message.metadata?.itemSeq,
      content: toolCall ? undefined : message.content,
      status: toolCall?.status || (message.isStreaming ? 'running' : 'complete'),
      toolName: toolCall?.name,
      hasToolResult: Boolean(toolCall?.result),
      hasToolError: Boolean(toolCall?.error),
    };
  });
}

function findArtifactByTurnItemId(projectionValue, itemId) {
  assert.ok(projectionValue, 'projection should exist');
  return projectionValue.artifacts.find((artifact) => (
    artifact.message.metadata?.turnItemId === itemId
  ));
}

function assertUserImageMetadataProjectsToMessage(reducer, projection, timelineRenderItems) {
  const c = baseCase('user-image-metadata', 'session-golden-user-image', 'turn-golden-user-image', 9300);
  const imageMetadata = {
    images: [
      {
        name: 'paste.png',
        dataUrl: 'data:image/png;base64,AAA',
      },
    ],
    contextReferences: [
      {
        kind: 'file',
        path: '/tmp/reference.md',
        name: 'reference.md',
      },
    ],
  };
  const userItem = user(c, 1, '请分析这张图片。');
  userItem.metadata = imageMetadata;
  const assistantItem = assistantText(c, 2, 'assistant-final', '图片中包含测试内容。', 'completed');
  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'completed', [userItem, assistantItem], { completedAt: 9400, responseDurationMs: 100 }),
  ]);
  const projectionValue = projection.buildCanonicalTimelineProjection(state);
  const userArtifact = findArtifactByTurnItemId(projectionValue, 'user-message');
  assert.ok(userArtifact, 'user image message artifact should exist');
  assert.deepEqual(
    userArtifact.message.images,
    imageMetadata.images,
    'user image metadata must project to first-class Message.images for MessageItem rendering',
  );
  assert.equal(
    userArtifact.message.metadata?.images,
    undefined,
    'transport image metadata should not remain duplicated on projected message metadata',
  );
  assert.deepEqual(
    userArtifact.message.contextReferences,
    imageMetadata.contextReferences,
    'user context reference metadata must project to first-class Message.contextReferences',
  );
  const threadItems = timelineRenderItems.buildTimelineRenderItems(projectionValue, 'thread');
  assert.deepEqual(
    threadItems.find((entry) => entry.message.type === 'user_input')?.message.contextReferences,
    imageMetadata.contextReferences,
    'timeline render cloning must preserve context references for MessageItem rendering',
  );
  assert.equal(
    userArtifact.message.metadata?.contextReferences,
    undefined,
    'transport context reference metadata should not remain duplicated on projected message metadata',
  );
}

function assertViewImageToolResultProjectsAsPreview(reducer, projection, viewImagePreview) {
  const c = baseCase('view-image-tool-preview', 'session-golden-view-image', 'turn-golden-view-image', 9350);
  const imageData = 'A'.repeat(300);
  const viewImagePayload = {
    tool: 'view_image',
    status: 'succeeded',
    access_mode: 'read_only',
    path: '/tmp/pixel.png',
    mime: 'image/png',
    bytes: 68,
    summary: '已读取图片 /tmp/pixel.png (mime=image/png, bytes=68)',
    model_content: [
      {
        type: 'text',
        text: '已读取图片 /tmp/pixel.png (mime=image/png, bytes=68)',
      },
      {
        type: 'image',
        source: {
          type: 'base64',
          media_type: 'image/png',
          data: imageData,
        },
      },
    ],
  };
  const toolItem = item(c, 1, 'tool-view-image', 'tool_call', 'completed', {
    content: '命令执行成功: view_image',
    title: 'view_image',
    tool: {
      callId: 'call-view-image',
      name: 'view_image',
      arguments: { path: '/tmp/pixel.png' },
      result: viewImagePayload,
    },
  });
  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'completed', [toolItem], { completedAt: 9450, responseDurationMs: 100 }),
  ]);
  const projectionValue = projection.buildCanonicalTimelineProjection(state);
  const toolArtifact = findArtifactByTurnItemId(projectionValue, 'tool-view-image');
  assert.ok(toolArtifact, 'view_image tool artifact should exist');
  const toolCall = toolArtifact.message.blocks?.find((block) => block.type === 'tool_call')?.toolCall;
  assert.equal(toolCall?.name, 'view_image', 'view_image tool call should keep canonical tool name');
  assert.equal(typeof toolCall?.result, 'string', 'canonical projection keeps ToolCall.result as display string');

  const preview = viewImagePreview.parseViewImagePreview(toolCall.name, toolCall.result);
  assert.deepEqual(
    preview,
    {
      src: `data:image/png;base64,${imageData}`,
      path: '/tmp/pixel.png',
      mime: 'image/png',
      bytes: 68,
    },
    'projected view_image result must remain parseable as an image preview',
  );

  const formatted = viewImagePreview.formatViewImageToolOutput(toolCall.name, toolCall.result);
  assert.ok(formatted, 'view_image formatted output should exist');
  assert.ok(
    !formatted.includes(imageData),
    'view_image text output must not expose raw base64 image data',
  );
  assert.ok(
    formatted.includes('[base64 image data omitted: 300 chars]'),
    'view_image text output should show an explicit base64 omission marker',
  );
  assert.equal(
    viewImagePreview.isViewImageTool('mcp__local__view_image'),
    true,
    'MCP-qualified view_image names should use the same preview parser',
  );
}

function assertTerminalLateUpsertIsIgnored(reducer, projection) {
  const testCase = singleToolCase();
  const completed = replayLive(reducer, testCase);
  const before = projectionSignature(projection.buildCanonicalTimelineProjection(completed));
  const lateRunningTool = event(testCase, 999, 'turn_item_upsert', {
    turn: turn(testCase, 'running', [
      tool(testCase, 3, 'tool-a', 'call-a', 'pwd', 'running'),
    ]),
    item: tool(testCase, 3, 'tool-a', 'call-a', 'pwd', 'running'),
  });
  const result = reducer.reduceCanonicalTurnEvent(completed, lateRunningTool);
  assert.equal(result.error, undefined, 'late running upsert should not become a protocol error');
  assert.deepEqual(
    projectionSignature(projection.buildCanonicalTimelineProjection(result.state)),
    before,
    'late running upsert must not roll terminal item or turn back',
  );
}

function assertTerminalLateTurnStartedIsIgnored(reducer, projection) {
  const testCase = acceptedFirstFrameCase();
  const completed = replayLive(reducer, testCase);
  const before = projectionSignature(projection.buildCanonicalTimelineProjection(completed));
  const userItem = user(testCase, 1, '请只回复一句 first frame ok。');
  const lateRunningAssistant = assistantPlaceholderText(
    testCase,
    2,
    'assistant-placeholder',
    'running',
  );
  const lateAccepted = event(testCase, 0, 'turn_started', {
    turn: turn(testCase, 'running', [userItem, lateRunningAssistant]),
    item: lateRunningAssistant,
  });
  const result = reducer.reduceCanonicalTurnEvent(completed, lateAccepted);
  assert.equal(result.error, undefined, 'late accepted running event should not become a protocol error');
  assert.deepEqual(
    projectionSignature(projection.buildCanonicalTimelineProjection(result.state)),
    before,
    'late accepted running event must not roll terminal assistant text back',
  );
}

function assertAcceptedFirstFrameRunning(reducer, projection) {
  const testCase = acceptedFirstFrameCase();
  const firstFrame = replayLive(reducer, {
    ...testCase,
    events: [testCase.events[0]],
  });
  assert.deepEqual(
    projectionSignature(projection.buildCanonicalTimelineProjection(firstFrame)),
    [
      signatureMessage('message', 'user_message', 1000, '请只回复一句 first frame ok。'),
      signatureMessageWithStatus('message', 'assistant_text', 2000, '', 'running'),
    ],
    'accepted first frame should render an empty running assistant in canonical projection',
  );
}

function assertLocalPendingTurnIsReplacedByAcceptedCanonicalTurn(reducer, projection) {
  const requestMetadata = {
    requestId: 'request-local-pending',
    userMessageId: 'user-message',
    placeholderMessageId: 'assistant-placeholder',
  };
  const local = baseCase(
    'local-pending-turn',
    'session-golden-local-pending',
    'turn-local-request-local-pending',
    10,
  );
  const accepted = baseCase(
    'accepted-replaces-local-pending',
    local.sessionId,
    'turn-session-1777600000000',
    1777600000000,
  );
  const localUser = user(local, 1, '本地 pending 应该原位接管。');
  localUser.metadata = { ...requestMetadata, localOptimistic: true };
  const localAssistant = assistantPlaceholderText(local, 2, 'assistant-placeholder', 'running');
  localAssistant.metadata = { ...requestMetadata, localOptimistic: true };
  const acceptedUser = user(accepted, 1, localUser.content);
  acceptedUser.metadata = requestMetadata;
  const acceptedAssistant = assistantPlaceholderText(accepted, 2, 'assistant-placeholder', 'running');
  acceptedAssistant.metadata = requestMetadata;

  let state = reducer.createCanonicalTurnReducerState(local.sessionId);
  let result = reducer.reduceCanonicalTurnEvent(state, event(local, 0, 'turn_started', {
    turn: {
      ...turn(local, 'running', [localUser, localAssistant]),
      metadata: { requestId: requestMetadata.requestId, localOptimistic: true },
    },
    item: localAssistant,
  }));
  assert.equal(result.error, undefined, 'local pending canonical event should reduce');
  state = result.state;
  const localProjection = projection.buildCanonicalTimelineProjection(state);
  assert.deepEqual(
    projectionSignature(localProjection),
    [
      signatureMessage('message', 'user_message', 1000, localUser.content),
      signatureMessageWithStatus('message', 'assistant_text', 2000, '', 'running'),
    ],
    'local pending canonical turn should render before backend accepted',
  );

  result = reducer.reduceCanonicalTurnEvent(state, event(accepted, 1, 'turn_started', {
    turn: turn(accepted, 'running', [acceptedUser, acceptedAssistant]),
    item: acceptedAssistant,
  }));
  assert.equal(result.error, undefined, 'accepted canonical event should replace local pending');
  assert.equal(result.state.turns.length, 1, 'local optimistic turn must not remain as a duplicate');
  assert.equal(result.state.turns[0].turnId, accepted.turnId);
  assert.deepEqual(
    result.changedTurnIds,
    [local.turnId, accepted.turnId],
    'optimistic replacement must invalidate both the old and accepted turn artifacts',
  );
  const incrementalProjection = projection.updateCanonicalTimelineProjection(
    localProjection,
    result.state,
    result.changedTurnIds,
  );
  assert.ok(
    incrementalProjection.artifacts.every((artifact) => artifact.message.metadata?.turnId === accepted.turnId),
    'incremental projection must remove every optimistic artifact after acceptance',
  );
  assert.deepEqual(
    projectionSignature(incrementalProjection),
    [
      signatureMessage('message', 'user_message', 1000, acceptedUser.content),
      signatureMessageWithStatus('message', 'assistant_text', 2000, '', 'running'),
    ],
    'accepted canonical turn should keep the same visible timeline shape',
  );
}

function assertLocalPendingImageSurvivesRegularAcceptedTurn(reducer, projection) {
  const requestMetadata = {
    requestId: 'request-local-image',
    userMessageId: 'user-image-message',
    placeholderMessageId: 'assistant-image-placeholder',
  };
  const imageMetadata = {
    images: [
      {
        name: 'paste.png',
        dataUrl: 'data:image/png;base64,AAA',
      },
    ],
  };
  const local = baseCase(
    'local-pending-image-turn',
    'session-golden-local-image',
    'turn-local-request-local-image',
    20,
  );
  const accepted = baseCase(
    'accepted-image-replaces-local-pending',
    local.sessionId,
    'turn-session-1777600000020',
    1777600000020,
  );
  const localUser = user(local, 1, '请分析这张图片。');
  localUser.itemId = requestMetadata.userMessageId;
  localUser.metadata = { ...requestMetadata, ...imageMetadata, localOptimistic: true };
  const localAssistant = assistantPlaceholderText(local, 2, requestMetadata.placeholderMessageId, 'running');
  localAssistant.metadata = { ...requestMetadata, localOptimistic: true };
  const acceptedUser = user(accepted, 1, localUser.content);
  acceptedUser.itemId = requestMetadata.userMessageId;
  acceptedUser.metadata = { ...requestMetadata, ...imageMetadata };

  let state = reducer.createCanonicalTurnReducerState(local.sessionId);
  let result = reducer.reduceCanonicalTurnEvent(state, event(local, 0, 'turn_started', {
    turn: {
      ...turn(local, 'running', [localUser, localAssistant]),
      metadata: { requestId: requestMetadata.requestId, localOptimistic: true },
    },
    item: localAssistant,
  }));
  assert.equal(result.error, undefined, 'local pending image canonical event should reduce');
  state = result.state;
  let projectionValue = projection.buildCanonicalTimelineProjection(state);
  let userArtifact = findArtifactByTurnItemId(projectionValue, requestMetadata.userMessageId);
  assert.deepEqual(
    userArtifact?.message.images,
    imageMetadata.images,
    'local pending image should enter Message.images immediately',
  );

  result = reducer.reduceCanonicalTurnEvent(state, event(accepted, 1, 'turn_started', {
    turn: turn(accepted, 'running', [acceptedUser]),
  }));
  assert.equal(result.error, undefined, 'regular accepted canonical image event should reduce');
  assert.equal(result.state.turns.length, 1, 'accepted canonical image turn must replace local optimistic turn');
  assert.equal(result.state.turns[0].turnId, accepted.turnId);
  projectionValue = projection.buildCanonicalTimelineProjection(result.state);
  userArtifact = findArtifactByTurnItemId(projectionValue, requestMetadata.userMessageId);
  assert.deepEqual(
    userArtifact?.message.images,
    imageMetadata.images,
    'accepted canonical user-only turn should keep image thumbnails in the message area',
  );
}

function assertCrossSessionCanonicalEventIsRejected(reducer, projection) {
  const active = baseCase(
    'active-session-event-scope',
    'session-golden-active-scope',
    'turn-golden-active-scope',
    11000,
  );
  const foreign = baseCase(
    'foreign-session-event-scope',
    'session-golden-foreign-scope',
    'turn-golden-foreign-scope',
    11100,
  );
  const activeUser = user(active, 1, '当前会话内容。');
  const activeAssistant = assistantText(active, 2, 'assistant-active', '当前会话已完成。', 'completed');
  const foreignUser = user(foreign, 1, '其他会话内容。');
  const foreignAssistant = assistantPlaceholderText(foreign, 2, 'assistant-foreign', 'running');
  const state = reducer.replaceCanonicalTurns(active.sessionId, [
    turn(active, 'completed', [activeUser, activeAssistant], { completedAt: 11050, responseDurationMs: 50 }),
  ]);
  const before = projectionSignature(projection.buildCanonicalTimelineProjection(state));

  const result = reducer.reduceCanonicalTurnEvent(state, event(foreign, 1, 'turn_started', {
    turn: turn(foreign, 'running', [foreignUser, foreignAssistant]),
    item: foreignAssistant,
  }));

  assert.match(
    result.error || '',
    /session mismatch/,
    'cross-session canonical event must be rejected instead of being applied to the active reducer',
  );
  assert.equal(result.changed, false);
  assert.equal(result.state.sessionId, active.sessionId);
  assert.equal(result.state.turns.length, 1);
  assert.deepEqual(
    projectionSignature(projection.buildCanonicalTimelineProjection(result.state)),
    before,
    'cross-session canonical event must not mutate active session projection',
  );
}

function assertReplaceCanonicalTurnsFiltersForeignSessions(reducer) {
  const active = baseCase(
    'active-session-bootstrap-scope',
    'session-golden-bootstrap-active',
    'turn-golden-bootstrap-active',
    11200,
  );
  const foreign = baseCase(
    'foreign-session-bootstrap-scope',
    'session-golden-bootstrap-foreign',
    'turn-golden-bootstrap-foreign',
    11300,
  );
  const activeTurn = turn(active, 'completed', [
    user(active, 1, '当前会话 bootstrap 内容。'),
    assistantText(active, 2, 'assistant-active-bootstrap', '当前会话完成。', 'completed'),
  ], { completedAt: 11250, responseDurationMs: 50 });
  const foreignTurn = turn(foreign, 'running', [
    user(foreign, 1, '其他会话 bootstrap 内容。'),
    assistantPlaceholderText(foreign, 2, 'assistant-foreign-bootstrap', 'running'),
  ]);

  const state = reducer.replaceCanonicalTurns(active.sessionId, [activeTurn, foreignTurn]);

  assert.equal(state.sessionId, active.sessionId);
  assert.deepEqual(
    state.turns.map((item) => item.sessionId),
    [active.sessionId],
    'bootstrap canonical turn replacement must keep only the requested session turns',
  );
}

function assertMessagesStoreRejectsForeignSessionProjection(reducer, projection, messagesStore) {
  const active = baseCase(
    'active-store-projection-scope',
    'session-golden-store-active',
    'turn-golden-store-active',
    11600,
  );
  const foreign = baseCase(
    'foreign-store-projection-scope',
    'session-golden-store-foreign',
    'turn-golden-store-foreign',
    11700,
  );
  const activeProjection = projection.buildCanonicalTimelineProjection(reducer.replaceCanonicalTurns(active.sessionId, [
    turn(active, 'completed', [
      user(active, 1, '当前 store 会话内容。'),
      assistantText(active, 2, 'assistant-store-active', '当前会话完成。', 'completed'),
    ], { completedAt: 11650, responseDurationMs: 50 }),
  ]));
  const foreignProjection = projection.buildCanonicalTimelineProjection(reducer.replaceCanonicalTurns(foreign.sessionId, [
    turn(foreign, 'completed', [
      user(foreign, 1, '其他 store 会话内容。'),
      assistantText(foreign, 2, 'assistant-store-foreign', '其他会话完成。', 'completed'),
    ], { completedAt: 11750, responseDurationMs: 50 }),
  ]));
  assert.ok(activeProjection, 'active store projection should exist');
  assert.ok(foreignProjection, 'foreign store projection should exist');

  messagesStore.setCurrentSessionId(active.sessionId);
  assert.equal(
    messagesStore.setCanonicalTimelineProjection(activeProjection),
    true,
    'active session projection should be accepted by messages store',
  );
  assert.equal(
    messagesStore.messagesState.canonicalTimelineProjection,
    activeProjection,
    'messages store must retain the canonical projection instead of cloning the full timeline',
  );
  const before = projectionSignature(messagesStore.messagesState.canonicalTimelineProjection);

  const originalWarn = console.warn;
  try {
    console.warn = () => {};
    assert.equal(
      messagesStore.setCanonicalTimelineProjection(foreignProjection),
      false,
      'foreign session projection must be rejected by messages store',
    );
  } finally {
    console.warn = originalWarn;
  }
  assert.deepEqual(
    projectionSignature(messagesStore.messagesState.canonicalTimelineProjection),
    before,
    'foreign session projection must not overwrite the active messages store projection',
  );
  messagesStore.setCurrentSessionId(null);
}

function assertMessagesStoreAdoptsLiveCanonicalEventForEmptySession(dataHandlers, messagesStore) {
  messagesStore.messagesState.currentWorkspaceId = 'workspace-golden-live-adopt';
  messagesStore.messagesState.currentWorkspacePath = '/tmp/workspace-golden-live-adopt';
  messagesStore.setCurrentSessionId(null);
  messagesStore.clearAllMessages({
    persist: false,
    resetTimelineView: true,
    resetPanelState: true,
    skipAntiLiftBack: true,
  });

  const c = baseCase(
    'live-empty-session-image-adopt',
    'session-golden-live-image-adopt',
    'turn-golden-live-image-adopt',
    11800,
  );
  const imageMetadata = {
    images: [
      {
        name: 'live.png',
        dataUrl: 'data:image/png;base64,AAA',
      },
    ],
  };
  const userItem = user(c, 1, '实时图片消息。');
  userItem.metadata = imageMetadata;
  const canonicalEvent = event(c, 1, 'turn_started', {
    turn: turn(c, 'running', [userItem]),
    item: userItem,
  });

  dataHandlers.handleUnifiedData({
    id: 'golden-live-empty-session-canonical-event',
    category: 'data',
    type: 'system',
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: 'completed',
    blocks: [],
    timestamp: c.turnSeq,
    updatedAt: c.turnSeq,
    data: {
      dataType: 'sessionTurnCanonicalEventUpdated',
      payload: {
        sessionId: c.sessionId,
        canonicalEvent,
      },
    },
  });

  assert.equal(
    messagesStore.messagesState.currentSessionId,
    c.sessionId,
    'empty current session must adopt the live canonical event session',
  );
  const userArtifact = findArtifactByTurnItemId(messagesStore.messagesState.canonicalTimelineProjection, 'user-message');
  assert.deepEqual(
    userArtifact?.message.images,
    imageMetadata.images,
    'live canonical image must enter the message projection without requiring refresh',
  );
  messagesStore.setCurrentSessionId(null);
}

function assertSameSessionBootstrapAppliesAuthoritativeSnapshotWhenProjectionIsEmpty(dataHandlers, messagesStore) {
  const workspaceId = 'workspace-golden-bootstrap-empty-projection';
  const workspacePath = '/tmp/workspace-golden-bootstrap-empty-projection';
  const c = baseCase(
    'same-session-bootstrap-empty-projection',
    'session-golden-bootstrap-empty-projection',
    'turn-golden-bootstrap-empty-projection',
    12000,
  );
  const imageMetadata = {
    images: [
      {
        name: 'bootstrap.png',
        dataUrl: 'data:image/png;base64,AAA',
      },
    ],
  };
  const userItem = user(c, 1, '启动恢复图片消息。');
  userItem.metadata = imageMetadata;
  const assistantFailed = assistantText(c, 2, 'assistant-bootstrap-failed', '模型服务暂时不可用，请稍后重试。', 'failed');
  const canonicalTurn = turn(c, 'failed', [userItem, assistantFailed], {
    completedAt: c.turnSeq + 100,
    responseDurationMs: 100,
  });
  const sessions = [
    {
      id: c.sessionId,
      sessionId: c.sessionId,
      title: '启动恢复图片消息',
      createdAt: c.turnSeq,
      updatedAt: c.turnSeq + 100,
      messageCount: 1,
      workspaceId,
    },
  ];

  messagesStore.messagesState.currentWorkspaceId = workspaceId;
  messagesStore.messagesState.currentWorkspacePath = workspacePath;
  messagesStore.messagesState.bootstrapped = false;
  messagesStore.setCurrentSessionId(c.sessionId);
  messagesStore.clearAllMessages({
    persist: false,
    resetTimelineView: true,
    resetPanelState: true,
    skipAntiLiftBack: true,
  });
  messagesStore.addPendingRequest('request-bootstrap-empty-projection');
  assert.equal(
    messagesStore.messagesState.canonicalTimelineProjection,
    null,
    'same-session bootstrap repro starts with an empty canonical projection',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'same-session bootstrap repro keeps local processing active before authoritative snapshot',
  );

  dataHandlers.handleUnifiedData({
    id: 'golden-same-session-bootstrap-empty-projection',
    category: 'data',
    type: 'system',
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: 'completed',
    blocks: [],
    timestamp: c.turnSeq + 100,
    updatedAt: c.turnSeq + 100,
    data: {
      dataType: 'sessionBootstrapLoaded',
      payload: {
        sessionId: c.sessionId,
        workspace: {
          workspaceId,
          rootPath: workspacePath,
        },
        sessions,
        state: {
          currentSessionId: c.sessionId,
          currentWorkspaceId: workspaceId,
          currentWorkspacePath: workspacePath,
          sessions,
          isProcessing: false,
          processingState: null,
          messages: [],
          edits: [],
          changedFiles: [],
          pendingChanges: [],
          pendingChangesState: null,
        },
        canonicalTurns: [canonicalTurn],
        notifications: {
          notifications: [],
        },
        orchestratorRuntimeState: null,
        hasMoreBefore: false,
        beforeCursor: null,
      },
    },
  });

  const projectionValue = messagesStore.messagesState.canonicalTimelineProjection;
  const userArtifact = findArtifactByTurnItemId(projectionValue, 'user-message');
  assert.deepEqual(
    userArtifact?.message.images,
    imageMetadata.images,
    'same-session bootstrap must project authoritative user images when local projection is empty',
  );
  const assistantArtifact = findArtifactByTurnItemId(projectionValue, 'assistant-bootstrap-failed');
  assert.equal(
    assistantArtifact?.message.content,
    assistantFailed.content,
    'same-session bootstrap must project terminal assistant content without requiring refresh',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    false,
    'authoritative same-session bootstrap must settle processing after applying terminal snapshot',
  );
  assert.equal(
    messagesStore.messagesState.bootstrapped,
    true,
    'authoritative same-session bootstrap must clear the startup overlay',
  );
  assert.equal(
    messagesStore.messagesState.pendingRequests.size,
    0,
    'authoritative same-session bootstrap must clear stale local pending request ids',
  );
  messagesStore.setCurrentSessionId(null);
}

function assertSameSessionStaleIdleBootstrapPreservesActiveTurn(dataHandlers, messagesStore) {
  const workspaceId = 'workspace-golden-stale-idle-bootstrap';
  const workspacePath = '/tmp/workspace-golden-stale-idle-bootstrap';
  const sessionId = 'session-golden-stale-idle-bootstrap';
  const requestId = 'request-stale-idle-bootstrap';
  const sessions = [
    {
      id: sessionId,
      title: '活跃轮次',
      createdAt: 10_000,
      updatedAt: 10_000,
      messageCount: 0,
      workspaceId,
    },
  ];

  messagesStore.messagesState.currentWorkspaceId = workspaceId;
  messagesStore.messagesState.currentWorkspacePath = workspacePath;
  messagesStore.setCurrentSessionId(sessionId);
  messagesStore.clearAllMessages({
    persist: false,
    resetTimelineView: true,
    resetPanelState: true,
    skipAntiLiftBack: true,
  });
  messagesStore.addPendingRequest(requestId);
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'stale idle bootstrap repro starts with an active local turn',
  );

  dataHandlers.handleUnifiedData({
    id: 'golden-stale-idle-bootstrap',
    category: 'data',
    type: 'system',
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: 'completed',
    blocks: [],
    timestamp: 10_001,
    updatedAt: 10_001,
    data: {
      dataType: 'sessionBootstrapLoaded',
      payload: {
        sessionId,
        workspace: {
          workspaceId,
          rootPath: workspacePath,
        },
        sessions,
        state: {
          currentSessionId: sessionId,
          currentWorkspaceId: workspaceId,
          currentWorkspacePath: workspacePath,
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
        orchestratorRuntimeState: null,
        hasMoreBefore: false,
        beforeCursor: null,
      },
    },
  });

  assert.equal(
    messagesStore.messagesState.pendingRequests.has(requestId),
    true,
    'stale idle bootstrap without terminal turn must not clear the active request',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'stale idle bootstrap without terminal turn must not interrupt the active conversation flow',
  );
  messagesStore.clearProcessingState({ skipAntiLiftBack: true });
  messagesStore.clearAllRequestBindings();
  messagesStore.setCurrentSessionId(null);
}

function assertMessagesStoreSettlesProcessingFromLiveTerminalCanonicalEvent(dataHandlers, messagesStore) {
  const c = baseCase(
    'live-terminal-processing-settle',
    'session-golden-live-terminal-settle',
    'turn-golden-live-terminal-settle',
    11900,
  );
  const requestMetadata = {
    requestId: 'request-live-terminal-settle',
    userMessageId: 'user-live-terminal-settle',
    placeholderMessageId: 'assistant-live-terminal-settle',
  };
  const userItem = user(c, 1, '请完成处理态收敛验证。');
  userItem.itemId = requestMetadata.userMessageId;
  userItem.metadata = requestMetadata;
  const runningAssistant = assistantPlaceholderText(c, 2, requestMetadata.placeholderMessageId, 'running');
  runningAssistant.metadata = requestMetadata;
  const completedAssistant = assistantText(c, 2, requestMetadata.placeholderMessageId, '处理态已收敛。', 'completed');
  completedAssistant.metadata = requestMetadata;

  messagesStore.messagesState.currentWorkspaceId = 'workspace-golden-live-terminal-settle';
  messagesStore.messagesState.currentWorkspacePath = '/tmp/workspace-golden-live-terminal-settle';
  messagesStore.setCurrentSessionId(c.sessionId);
  messagesStore.clearAllMessages({
    persist: false,
    resetTimelineView: true,
    resetPanelState: true,
    skipAntiLiftBack: true,
  });
  messagesStore.addPendingRequest(requestMetadata.requestId);

  dataHandlers.handleUnifiedData({
    id: 'golden-live-running-canonical-event',
    category: 'data',
    type: 'system',
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: 'completed',
    blocks: [],
    timestamp: c.turnSeq,
    updatedAt: c.turnSeq,
    data: {
      dataType: 'sessionTurnCanonicalEventUpdated',
      payload: {
        sessionId: c.sessionId,
        canonicalEvent: event(c, 1, 'turn_item_upsert', {
          turn: turn(c, 'running', [userItem, runningAssistant]),
          item: runningAssistant,
        }),
      },
    },
  });
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'running canonical event should keep processing state active',
  );

  dataHandlers.handleUnifiedData({
    id: 'golden-live-terminal-canonical-event',
    category: 'data',
    type: 'system',
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: 'completed',
    blocks: [],
    timestamp: c.turnSeq + 1,
    updatedAt: c.turnSeq + 1,
    data: {
      dataType: 'sessionTurnCanonicalEventUpdated',
      payload: {
        sessionId: c.sessionId,
        canonicalEvent: event(c, 2, 'turn_completed', {
          turn: turn(c, 'completed', [userItem, completedAssistant], {
            completedAt: c.turnSeq + 100,
            responseDurationMs: 100,
          }),
          item: completedAssistant,
        }),
      },
    },
  });
  assert.equal(
    messagesStore.messagesState.isProcessing,
    false,
    'terminal canonical event must settle processing state immediately',
  );
  assert.equal(
    messagesStore.messagesState.pendingRequests.size,
    0,
    'terminal canonical event must clear pending request ids',
  );
  messagesStore.setCurrentSessionId(null);
}

function assertHistoricalTerminalReplayDoesNotClearCurrentTurn(dataHandlers, messagesStore) {
  const sessionId = 'session-golden-historical-replay';
  const historical = baseCase(
    'historical-replay-old-turn',
    sessionId,
    'turn-golden-historical-old',
    12000,
  );
  const current = baseCase(
    'historical-replay-current-turn',
    sessionId,
    'turn-golden-historical-current',
    12100,
  );
  const historicalMetadata = { requestId: 'request-historical-old' };
  const currentMetadata = { requestId: 'request-historical-current' };
  const historicalUser = user(historical, 1, '历史轮次。');
  historicalUser.metadata = historicalMetadata;
  const historicalRunningAssistant = assistantPlaceholderText(
    historical,
    2,
    'assistant-historical-old',
    'running',
  );
  historicalRunningAssistant.metadata = historicalMetadata;
  const historicalCompletedAssistant = assistantText(
    historical,
    2,
    'assistant-historical-old',
    '历史轮次完成。',
    'completed',
  );
  historicalCompletedAssistant.metadata = historicalMetadata;
  const currentUser = user(current, 1, '当前轮次。');
  currentUser.metadata = currentMetadata;
  const currentRunningAssistant = assistantPlaceholderText(
    current,
    2,
    'assistant-historical-current',
    'running',
  );
  currentRunningAssistant.metadata = currentMetadata;
  const currentCompletedAssistant = assistantText(
    current,
    2,
    'assistant-historical-current',
    '当前轮次完成。',
    'completed',
  );
  currentCompletedAssistant.metadata = currentMetadata;

  messagesStore.messagesState.currentWorkspaceId = 'workspace-golden-historical-replay';
  messagesStore.messagesState.currentWorkspacePath = '/tmp/workspace-golden-historical-replay';
  messagesStore.setCurrentSessionId(sessionId);
  messagesStore.clearAllMessages({
    persist: false,
    resetTimelineView: true,
    resetPanelState: true,
    skipAntiLiftBack: true,
  });
  const localStartedAt = 12_150;
  messagesStore.beginLocalTurnSubmission({
    requestId: currentMetadata.requestId,
    placeholderMessageId: 'assistant-historical-current',
    startedAt: localStartedAt,
    source: 'orchestrator',
    agent: 'orchestrator',
  });

  const dispatchCanonicalEvent = (id, canonicalEvent) => {
    dataHandlers.handleUnifiedData({
      id,
      category: 'data',
      type: 'system',
      source: 'orchestrator',
      agent: 'orchestrator',
      lifecycle: 'completed',
      blocks: [],
      timestamp: canonicalEvent.occurredAt,
      updatedAt: canonicalEvent.occurredAt,
      data: {
        dataType: 'sessionTurnCanonicalEventUpdated',
        payload: { sessionId, canonicalEvent },
      },
    });
  };

  dispatchCanonicalEvent('golden-current-running', event(current, 10, 'turn_item_upsert', {
    turn: turn(current, 'running', [currentUser, currentRunningAssistant]),
    item: currentRunningAssistant,
  }));
  dispatchCanonicalEvent('golden-historical-running', event(historical, 11, 'turn_item_upsert', {
    turn: turn(historical, 'running', [historicalUser, historicalRunningAssistant]),
    item: historicalRunningAssistant,
  }));
  dispatchCanonicalEvent('golden-historical-terminal', event(historical, 12, 'turn_completed', {
    turn: turn(historical, 'completed', [historicalUser, historicalCompletedAssistant], {
      completedAt: historical.turnSeq + 100,
      responseDurationMs: 100,
    }),
    item: historicalCompletedAssistant,
  }));

  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    '历史终态事件回放时，当前运行轮次必须继续保持 processing',
  );
  assert.equal(
    messagesStore.messagesState.pendingRequests.has(currentMetadata.requestId),
    true,
    '历史终态事件不得清除当前轮次 requestId',
  );
  assert.equal(
    messagesStore.messagesState.pendingRequests.has(historicalMetadata.requestId),
    false,
    '历史轮次完成后必须从会话级 pending 集合移除',
  );
  assert.equal(
    messagesStore.messagesState.thinkingStartAt,
    localStartedAt,
    '历史事件回放不得覆盖当前本地轮次的计时起点',
  );

  dispatchCanonicalEvent('golden-current-terminal', event(current, 13, 'turn_completed', {
    turn: turn(current, 'completed', [currentUser, currentCompletedAssistant], {
      completedAt: current.turnSeq + 100,
      responseDurationMs: 100,
    }),
    item: currentCompletedAssistant,
  }));
  assert.equal(
    messagesStore.messagesState.isProcessing,
    false,
    '当前轮次终态到达后才允许会话处理态收敛',
  );
  messagesStore.setCurrentSessionId(null);
}

function resetMessagesStoreForGoldenProcessing(messagesStore) {
  messagesStore.messagesState.currentWorkspaceId = 'workspace-golden-processing';
  messagesStore.setCurrentSessionId('session-golden-processing');
  messagesStore.clearAllMessages({
    persist: false,
    resetTimelineView: true,
    resetPanelState: true,
    skipAntiLiftBack: true,
  });
  messagesStore.messagesState.lastForcedIdleAt = null;
}

function assertMessagesStoreClearsLocalPendingFromAuthoritativeIdle(messagesStore) {
  resetMessagesStoreForGoldenProcessing(messagesStore);

  messagesStore.addPendingRequest('request-authoritative-idle');
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'local pending request should raise processing state before authoritative idle arrives',
  );
  messagesStore.applyAuthoritativeProcessingState({
    isProcessing: false,
    source: 'orchestrator',
    agent: 'orchestrator',
    startedAt: null,
    pendingRequestIds: [],
  });
  assert.equal(
    messagesStore.messagesState.pendingRequests.size,
    0,
    'authoritative idle snapshot must clear local pending request ids',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    false,
    'authoritative idle snapshot must settle processing state',
  );

  messagesStore.addPendingRequest('request-authoritative-null');
  messagesStore.applyAuthoritativeProcessingState(null);
  assert.equal(
    messagesStore.messagesState.pendingRequests.size,
    0,
    'missing backend processingState means authoritative idle and must clear local pending',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    false,
    'missing backend processingState must not preserve local processing',
  );

  messagesStore.createRequestBinding({
    requestId: 'request-binding-clear',
    userMessageId: 'user-binding-clear',
    placeholderMessageId: 'assistant-binding-clear',
    createdAt: 9000,
  });
  messagesStore.addPendingRequest('request-binding-clear');
  messagesStore.clearRequestBinding('request-binding-clear');
  assert.equal(
    messagesStore.messagesState.pendingRequests.has('request-binding-clear'),
    false,
    'clearing a request binding must also clear its pending request',
  );

  messagesStore.setCurrentSessionId(null);
}

function assertHistoricalIdlePreservesBoundLocalSubmission(messagesStore) {
  resetMessagesStoreForGoldenProcessing(messagesStore);

  const requestId = 'request-bound-local-submission';
  const startedAt = 11_000;
  messagesStore.createRequestBinding({
    requestId,
    userMessageId: 'user-bound-local-submission',
    placeholderMessageId: 'assistant-bound-local-submission',
    createdAt: startedAt,
  });
  messagesStore.beginLocalTurnSubmission({
    requestId,
    placeholderMessageId: 'assistant-bound-local-submission',
    startedAt,
    source: 'orchestrator',
    agent: 'orchestrator',
  });

  messagesStore.applyAuthoritativeProcessingState(null);
  assert.equal(
    messagesStore.messagesState.pendingRequests.has(requestId),
    true,
    'historical idle must not clear a locally bound submission before canonical acceptance',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'historical idle must keep the locally bound submission in processing state',
  );
  assert.equal(
    messagesStore.messagesState.thinkingStartAt,
    startedAt,
    'historical idle must preserve the local submission timer origin',
  );

  messagesStore.clearRequestBinding(requestId);
  assert.equal(
    messagesStore.messagesState.isProcessing,
    false,
    'request lifecycle completion must still settle the preserved local submission',
  );
  messagesStore.setCurrentSessionId(null);
}

function assertHistoricalForcedIdlePreservesBoundLocalSubmission(dataHandlers, messagesStore) {
  resetMessagesStoreForGoldenProcessing(messagesStore);

  const requestId = 'request-bound-forced-idle';
  messagesStore.createRequestBinding({
    requestId,
    userMessageId: 'user-bound-forced-idle',
    placeholderMessageId: 'assistant-bound-forced-idle',
    createdAt: 12_000,
  });
  messagesStore.beginLocalTurnSubmission({
    requestId,
    placeholderMessageId: 'assistant-bound-forced-idle',
    startedAt: 12_000,
    source: 'orchestrator',
    agent: 'orchestrator',
  });

  dataHandlers.handleUnifiedData({
    id: 'historical-forced-idle-during-bound-submission',
    category: 'data',
    type: 'system',
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: 'completed',
    blocks: [],
    timestamp: 12_001,
    updatedAt: 12_001,
    data: {
      dataType: 'processingStateChanged',
      payload: {
        isProcessing: false,
        transitionKind: 'forced',
        reason: 'session_turn_completed',
      },
    },
  });

  assert.equal(
    messagesStore.messagesState.pendingRequests.has(requestId),
    true,
    'historical forced idle must not clear a locally bound submission',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'historical forced idle must keep the locally bound submission active',
  );

  dataHandlers.handleUnifiedData({
    id: 'user-interrupt-forced-idle-during-bound-submission',
    category: 'data',
    type: 'system',
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: 'completed',
    blocks: [],
    timestamp: 12_002,
    updatedAt: 12_002,
    data: {
      dataType: 'processingStateChanged',
      payload: {
        isProcessing: false,
        transitionKind: 'forced',
        reason: 'user_session_interrupt_requested',
      },
    },
  });
  assert.equal(
    messagesStore.messagesState.isProcessing,
    false,
    'explicit user interrupt must still settle a locally bound submission',
  );
  messagesStore.clearAllRequestBindings();
  messagesStore.setCurrentSessionId(null);
}

function assertSessionSwitchClearsExecutionState(messagesStore) {
  messagesStore.messagesState.currentWorkspaceId = 'workspace-golden-session-switch';
  messagesStore.messagesState.currentWorkspacePath = '/tmp/workspace-golden-session-switch';
  messagesStore.setCurrentSessionId('session-golden-running-a');
  messagesStore.clearAllMessages({
    persist: false,
    resetTimelineView: true,
    resetPanelState: true,
    skipAntiLiftBack: true,
  });
  messagesStore.messagesState.backendProcessing = true;
  messagesStore.messagesState.activeMessageIds = new Set(['assistant-running-a']);
  messagesStore.messagesState.thinkingStartAt = 12_000;
  messagesStore.addPendingRequest('request-running-a');
  assert.equal(
    messagesStore.messagesState.isProcessing,
    true,
    'session switch repro starts with a running source session',
  );

  messagesStore.setCurrentSessionId('session-golden-draft-b');
  assert.equal(
    messagesStore.messagesState.backendProcessing,
    false,
    'switching session must clear source-session backend processing',
  );
  assert.equal(
    messagesStore.messagesState.pendingRequests.size,
    0,
    'switching session must clear source-session pending requests',
  );
  assert.equal(
    messagesStore.messagesState.activeMessageIds.size,
    0,
    'switching session must clear source-session active messages',
  );
  assert.equal(
    messagesStore.messagesState.thinkingStartAt,
    null,
    'switching session must clear source-session thinking timer',
  );
  assert.equal(
    messagesStore.messagesState.isProcessing,
    false,
    'draft target session must not inherit the source session running state',
  );

  messagesStore.setCurrentSessionId(null);
}

function assertFailedAssistantTextUsesPlainMessageShell(reducer, projection) {
  const c = baseCase('failed-assistant-text', 'session-golden-failed-assistant', 'turn-golden-failed-assistant', 6000);
  const userItem = user(c, 1, '请调用工具后回答。');
  const failedAssistant = assistantText(c, 2, 'assistant-error', '模型在工具调用后未返回最终回复', 'failed');
  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'failed', [userItem, failedAssistant], { completedAt: 6100, responseDurationMs: 100 }),
  ]);
  const projectionValue = projection.buildCanonicalTimelineProjection(state);
  assert.ok(projectionValue, 'failed assistant projection should exist');
  const assistantArtifact = projectionValue.artifacts.find((artifact) => (
    artifact.message.metadata?.turnItemId === 'assistant-error'
  ));
  assert.ok(assistantArtifact, 'failed assistant artifact should exist');
  assert.equal(
    assistantArtifact.message.type,
    'text',
    'failed assistant_text should render as normal assistant text, not an error card shell',
  );
  assert.equal(
    assistantArtifact.message.blocks,
    undefined,
    'plain assistant_text should not be wrapped in a text block',
  );
  assert.equal(
    assistantArtifact.message.metadata?.responseDurationMs,
    100,
    'failed assistant_text should keep the turn response duration',
  );
}

function assertSplitToolStartedAndResultCollapseIntoOneCard(reducer, projection) {
  const c = baseCase(
    'split-tool-start-result',
    'session-golden-split-tool',
    'turn-golden-split-tool',
    8500,
  );
  const userItem = user(c, 1, '请连续运行两个工具，第二个先返回。');
  const firstToolRunning = tool(c, 2, 'tool-a-started', 'call-a', 'printf a', 'running');
  const secondToolCompleted = tool(c, 3, 'tool-b', 'call-b', 'printf b', 'completed', { stdout: 'b' });
  const firstToolCompleted = tool(c, 99, 'tool-a-result', 'call-a', 'printf a', 'completed', { stdout: 'a' });
  const assistant = assistantText(c, 100, 'assistant-final', '两个工具都完成了。', 'completed');
  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'completed', [userItem, firstToolRunning, secondToolCompleted, firstToolCompleted, assistant], {
      completedAt: 8600,
      responseDurationMs: 100,
    }),
  ]);
  // ordered=[user(1), firstToolRunning(2), secondTool(3), firstToolCompleted(99), assistant(100)]
  // → presentationSeq 1/2/3/4/5；first/second tool collapse 后 stableItemSeq 取首
  // artifact 的呈现序：first tool → 2000，second tool → 3000，assistant → 5000。
  assert.deepEqual(
    projectionSignature(projection.buildCanonicalTimelineProjection(state)),
    [
      signatureMessage('message', 'user_message', 1000, userItem.content),
      signatureTool(2000, 'shell_exec', 'success'),
      signatureTool(3000, 'shell_exec', 'success'),
      signatureMessage('message', 'assistant_text', 5000, assistant.content),
    ],
    'split tool started/result items should render as one stable card in invocation order',
  );
}

function assertCancelledToolShowsTurnResponseDuration(reducer, projection) {
  const testCase = cancelledToolCase();
  const state = replayLive(reducer, testCase);
  const projectionValue = projection.buildCanonicalTimelineProjection(state);
  const toolArtifact = findArtifactByTurnItemId(projectionValue, 'tool-cancelled');
  assert.ok(toolArtifact, 'cancelled tool artifact should exist');
  assert.equal(
    toolArtifact.message.metadata?.responseDurationMs,
    100,
    'cancelled tool should show the terminal turn response duration',
  );
}

function assertFailedToolWithoutAssistantShowsTurnResponseDuration(reducer, projection) {
  const c = baseCase('failed-tool-without-assistant', 'session-golden-failed-tool-only', 'turn-golden-failed-tool-only', 9000);
  const userItem = user(c, 1, '请运行一个失败命令，不要补充最终回复。');
  const failedTool = tool(c, 2, 'tool-failed-only', 'call-failed-only', 'sh -c "echo fail; exit 7"', 'failed', { stderr: 'fail\n', exit_code: 7 });
  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'failed', [userItem, failedTool], { completedAt: 9250, responseDurationMs: 250 }),
  ]);
  const projectionValue = projection.buildCanonicalTimelineProjection(state);
  const toolArtifact = findArtifactByTurnItemId(projectionValue, 'tool-failed-only');
  assert.ok(toolArtifact, 'failed tool artifact should exist');
  assert.equal(
    toolArtifact.message.metadata?.responseDurationMs,
    250,
    'failed tool without assistant final should show the terminal turn response duration',
  );
}

function assertBootstrapProcessingStateFromRunningCanonicalTurn(contract) {
  const c = baseCase(
    'bootstrap-running-canonical-turn',
    'session-bootstrap-running',
    'turn-bootstrap-running',
    7000,
  );
  const userItem = user(c, 1, '刷新后仍应恢复运行态。');
  userItem.metadata = { requestId: 'request-bootstrap-running' };
  const guidedUserItem = user(c, 2, '同一轮引导不应成为新的处理中请求。');
  guidedUserItem.metadata = { requestId: 'request-bootstrap-guide' };
  const assistantItem = assistantPlaceholderText(c, 3, 'assistant-bootstrap-running', 'running');
  assistantItem.metadata = { requestId: 'request-bootstrap-running' };
  const bootstrap = contract.normalizeRustBootstrapPayload({
    generatedAt: 7100,
    currentSession: {
      sessionId: c.sessionId,
      title: '运行中恢复',
      createdAt: 7000,
      updatedAt: 7100,
      messageCount: 1,
    },
    sessions: [{
      sessionId: c.sessionId,
      title: '运行中恢复',
      createdAt: 7000,
      updatedAt: 7100,
      messageCount: 1,
    }],
    canonicalTurns: [
      turn(c, 'running', [userItem, guidedUserItem, assistantItem]),
    ],
    runtimeReadModel: {
      details: {
        sessions: [],
        tasks: [],
      },
    },
  }, { sessionId: c.sessionId });

  assert.equal(
    bootstrap.state.isProcessing,
    true,
    'bootstrap should recover processing state from running canonical turn without task runtime',
  );
  assert.equal(bootstrap.state.processingState?.startedAt, 7000);
  assert.deepEqual(
    bootstrap.state.processingState?.pendingRequestIds,
    ['request-bootstrap-running'],
    'bootstrap should recover only the root request and ignore same-turn guide request ids',
  );
}

function assertBootstrapProcessingStateIgnoresForeignSessionRunningTurn(contract) {
  const active = baseCase(
    'bootstrap-active-session-with-foreign-running',
    'session-bootstrap-active-no-running',
    'turn-bootstrap-active-no-running',
    11400,
  );
  const foreign = baseCase(
    'bootstrap-foreign-session-running',
    'session-bootstrap-foreign-running',
    'turn-bootstrap-foreign-running',
    11500,
  );
  const activeUser = user(active, 1, '当前会话已经完成。');
  const activeAssistant = assistantText(active, 2, 'assistant-bootstrap-active', '已完成', 'completed');
  const foreignUser = user(foreign, 1, '其他会话仍在运行。');
  const foreignAssistant = assistantPlaceholderText(foreign, 2, 'assistant-bootstrap-foreign', 'running');
  foreignAssistant.metadata = { requestId: 'request-foreign-running' };

  const bootstrap = contract.normalizeRustBootstrapPayload({
    generatedAt: 11600,
    currentSession: {
      sessionId: active.sessionId,
      title: '当前完成会话',
      createdAt: 11400,
      updatedAt: 11600,
      messageCount: 2,
    },
    sessions: [
      {
        sessionId: active.sessionId,
        title: '当前完成会话',
        createdAt: 11400,
        updatedAt: 11600,
        messageCount: 2,
      },
      {
        sessionId: foreign.sessionId,
        title: '其他运行会话',
        createdAt: 11500,
        updatedAt: 11600,
        messageCount: 1,
      },
    ],
    canonicalTurns: [
      turn(active, 'completed', [activeUser, activeAssistant], { completedAt: 11480, responseDurationMs: 80 }),
      turn(foreign, 'running', [foreignUser, foreignAssistant]),
    ],
    runtimeReadModel: {
      details: {
        sessions: [],
        tasks: [],
      },
    },
  }, { sessionId: active.sessionId });

  assert.equal(
    bootstrap.state.isProcessing,
    false,
    'foreign session running canonical turn must not lift current session processing state',
  );
  assert.equal(bootstrap.state.processingState, null);
}

function assertBootstrapProcessingStateIgnoresTerminalCanonicalTurn(contract) {
  const c = baseCase(
    'bootstrap-terminal-canonical-turn',
    'session-bootstrap-terminal',
    'turn-bootstrap-terminal',
    8000,
  );
  const userItem = user(c, 1, '完成后刷新不应回到运行态。');
  const assistantItem = assistantText(c, 2, 'assistant-bootstrap-terminal', '已完成', 'completed');
  const bootstrap = contract.normalizeRustBootstrapPayload({
    generatedAt: 8100,
    currentSession: {
      sessionId: c.sessionId,
      title: '完成态恢复',
      createdAt: 8000,
      updatedAt: 8100,
      messageCount: 2,
    },
    sessions: [{
      sessionId: c.sessionId,
      title: '完成态恢复',
      createdAt: 8000,
      updatedAt: 8100,
      messageCount: 2,
    }],
    canonicalTurns: [
      turn(c, 'completed', [userItem, assistantItem], { completedAt: 8050, responseDurationMs: 50 }),
    ],
    runtimeReadModel: {
      details: {
        sessions: [],
        tasks: [],
      },
    },
  }, { sessionId: c.sessionId });

  assert.equal(
    bootstrap.state.isProcessing,
    false,
    'terminal canonical turn must not be restored as running',
  );
  assert.equal(bootstrap.state.processingState, null);
}

function assertBootstrapCarriesPendingChanges(contract) {
  const camelCaseBootstrap = contract.normalizeRustBootstrapPayload({
    generatedAt: 7200,
    currentSession: { sessionId: 'session-bootstrap-pending', title: 'pending', createdAt: 7000, updatedAt: 7200 },
    sessions: [{ sessionId: 'session-bootstrap-pending', title: 'pending', createdAt: 7000, updatedAt: 7200 }],
    workspaces: [{ workspaceId: 'workspace-bootstrap-pending', rootPath: '/tmp/bootstrap-pending' }],
    pendingChanges: [
      {
        filePath: 'created.txt',
        snapshotId: 'session:session-bootstrap-pending:created.txt',
        type: 'add',
        additions: 1,
        deletions: 0,
      },
      {
        filePath: 'modified.ts',
        snapshotId: 'session:session-bootstrap-pending:modified.ts',
        type: 'modify',
        additions: 4,
        deletions: 2,
      },
      {
        filePath: 'renamed.rs',
        oldPath: 'old.rs',
        snapshotId: 'session:session-bootstrap-pending:renamed.rs',
        type: 'rename',
        additions: 0,
        deletions: 0,
      },
      {
        filePath: 'deleted.md',
        snapshotId: 'session:session-bootstrap-pending:deleted.md',
        type: 'delete',
        additions: 0,
        deletions: 8,
      },
    ],
  }, {
    workspaceId: 'workspace-bootstrap-pending',
    sessionId: 'session-bootstrap-pending',
  });
  assert.deepEqual(
    camelCaseBootstrap.state.pendingChanges?.map((change) => change.filePath),
    ['created.txt', 'modified.ts', 'renamed.rs', 'deleted.md'],
    'bootstrap should expose the complete camelCase pendingChanges collection through AppState',
  );
  assert.equal(camelCaseBootstrap.state.pendingChangesStateVersion, 7200);

}

function assertBootstrapFiltersForeignWorkspaceSessions(contract) {
  const bootstrap = contract.normalizeRustBootstrapPayload({
    generatedAt: 7400,
    currentSession: {
      sessionId: 'session-workspace-a',
      workspaceId: 'workspace-a',
      title: 'A',
      createdAt: 7000,
      updatedAt: 7400,
    },
    sessions: [
      {
        sessionId: 'session-workspace-a',
        workspaceId: 'workspace-a',
        title: 'A',
        createdAt: 7000,
        updatedAt: 7400,
      },
      {
        sessionId: 'session-workspace-b',
        workspaceId: 'workspace-b',
        title: 'B',
        createdAt: 7100,
        updatedAt: 7300,
      },
      {
        sessionId: 'session-legacy',
        title: 'Legacy',
        createdAt: 7100,
        updatedAt: 7200,
      },
    ],
    workspaces: [
      { workspaceId: 'workspace-a', rootPath: '/tmp/workspace-a' },
      { workspaceId: 'workspace-b', rootPath: '/tmp/workspace-b' },
    ],
  }, {
    workspaceId: 'workspace-a',
  });

  assert.deepEqual(
    bootstrap.sessions.map((session) => session.id),
    ['session-workspace-a', 'session-legacy'],
    'bootstrap must not expose sessions explicitly bound to a foreign workspace',
  );
  assert.equal(
    bootstrap.sessions[0].workspaceId,
    'workspace-a',
    'bootstrap should keep session workspace scope for frontend guards',
  );
  assert.deepEqual(
    bootstrap.state.sessions.map((session) => session.id),
    bootstrap.sessions.map((session) => session.id),
    'AppState sessions must use the same workspace-filtered list as bootstrap sessions',
  );
}

function assertBootstrapExplicitWorkspaceWinsOverForeignCurrentSession(contract) {
  const bootstrap = contract.normalizeRustBootstrapPayload({
    generatedAt: 7500,
    currentSession: {
      sessionId: 'session-workspace-b-current',
      workspaceId: 'workspace-b',
      title: 'B current',
      createdAt: 7000,
      updatedAt: 7500,
    },
    sessions: [
      {
        sessionId: 'session-workspace-a-visible',
        workspaceId: 'workspace-a',
        title: 'A visible',
        createdAt: 7000,
        updatedAt: 7400,
      },
      {
        sessionId: 'session-workspace-b-current',
        workspaceId: 'workspace-b',
        title: 'B current',
        createdAt: 7000,
        updatedAt: 7500,
      },
    ],
    workspaces: [
      { workspaceId: 'workspace-a', rootPath: '/tmp/workspace-a' },
      { workspaceId: 'workspace-b', rootPath: '/tmp/workspace-b' },
    ],
  }, {
    workspaceId: 'workspace-a',
    workspacePath: '/tmp/workspace-a',
  });

  assert.equal(
    bootstrap.workspace.workspaceId,
    'workspace-a',
    'explicit bootstrap workspace must not be replaced by a foreign currentSession workspace',
  );
  assert.deepEqual(
    bootstrap.sessions.map((session) => session.id),
    ['session-workspace-a-visible'],
    'explicit bootstrap workspace must keep only sessions from that workspace',
  );
  assert.equal(
    bootstrap.sessionId,
    '',
    'foreign currentSession must be discarded instead of making the frontend guess a replacement session',
  );
  assert.equal(
    bootstrap.state.currentSessionId,
    '',
    'AppState currentSessionId must mirror the discarded foreign currentSession',
  );
}

function ordinaryChatCase() {
  const c = baseCase('ordinary-chat', 'session-golden-chat', 'turn-golden-chat', 1000);
  const userItem = user(c, 1, '多场景验证 1：请用一句话回复 normal chat ok。');
  const phaseItem = phase(c, 2, '正在理解请求并生成回复。', 'completed');
  const assistant = assistantText(c, 3, 'assistant-final', 'normal chat ok', 'completed');
  c.events = [
    event(c, 1, 'turn_item_upsert', { turn: turn(c, 'running', [userItem]), item: userItem }),
    event(c, 2, 'turn_item_upsert', { turn: turn(c, 'running', [phaseItem]), item: phaseItem }),
    event(c, 3, 'turn_item_upsert', { turn: turn(c, 'running', [assistantText(c, 3, 'assistant-final', 'normal', 'running')]), item: assistantText(c, 3, 'assistant-final', 'normal', 'running') }),
    event(c, 4, 'turn_completed', { turn: turn(c, 'completed', [userItem, phaseItem, assistant], { completedAt: 1100, responseDurationMs: 100 }) }),
  ];
  // ordered=[user, phase, assistant] → presentationSeq 1/2/3 → itemSeq ×1000
  c.expected = [
    signatureMessage('message', 'user_message', 1000, userItem.content),
    signatureMessage('message', 'assistant_text', 3000, assistant.content),
  ];
  return c;
}

function acceptedFirstFrameCase() {
  const c = baseCase('accepted-first-frame', 'session-golden-first-frame', 'turn-golden-first-frame', 1500);
  const userItem = user(c, 1, '请只回复一句 first frame ok。');
  const assistantPlaceholder = assistantPlaceholderText(c, 2, 'assistant-placeholder', 'running');
  const assistantStreaming = assistantText(c, 2, 'assistant-placeholder', 'first', 'running');
  const assistantCompleted = assistantText(c, 2, 'assistant-placeholder', 'first frame ok', 'completed');
  c.events = [
    event(c, 1, 'turn_started', { turn: turn(c, 'running', [userItem, assistantPlaceholder]), item: assistantPlaceholder }),
    event(c, 2, 'turn_item_upsert', { turn: turn(c, 'running', [assistantStreaming]), item: assistantStreaming }),
    event(c, 3, 'turn_completed', { turn: turn(c, 'completed', [userItem, assistantCompleted], { completedAt: 1600, responseDurationMs: 100 }) }),
  ];
  c.expected = [
    signatureMessage('message', 'user_message', 1000, userItem.content),
    signatureMessage('message', 'assistant_text', 2000, assistantCompleted.content),
  ];
  return c;
}

function singleToolCase() {
  const c = baseCase('single-tool', 'session-golden-single-tool', 'turn-golden-single-tool', 2000);
  const userItem = user(c, 1, '多场景验证 2：请使用 shell 工具运行 pwd，然后用一句话说明当前目录。');
  const phaseItem = phase(c, 2, '正在理解请求并准备调用工具。', 'completed');
  const toolRunning = tool(c, 3, 'tool-a', 'call-a', 'pwd', 'running');
  const toolCompleted = tool(c, 3, 'tool-a', 'call-a', 'pwd', 'completed', { stdout: '/\n' });
  const assistant = assistantText(c, 4, 'assistant-final', '当前目录是 `/`。', 'completed');
  c.events = [
    event(c, 1, 'turn_item_upsert', { turn: turn(c, 'running', [userItem]), item: userItem }),
    event(c, 2, 'turn_item_upsert', { turn: turn(c, 'running', [phaseItem]), item: phaseItem }),
    event(c, 3, 'turn_item_upsert', { turn: turn(c, 'running', [toolRunning]), item: toolRunning }),
    event(c, 4, 'turn_item_upsert', { turn: turn(c, 'running', [toolCompleted]), item: toolCompleted }),
    event(c, 5, 'turn_completed', { turn: turn(c, 'completed', [userItem, phaseItem, toolCompleted, assistant], { completedAt: 2100, responseDurationMs: 100 }) }),
  ];
  // ordered=[user, phase, tool, assistant] → presentationSeq 1/2/3/4 → ×1000
  c.expected = [
    signatureMessage('message', 'user_message', 1000, userItem.content),
    signatureTool(3000, 'shell_exec', 'success'),
    signatureMessage('message', 'assistant_text', 4000, assistant.content),
  ];
  return c;
}

function toolFirstCase() {
  const c = baseCase('tool-first', 'session-golden-tool-first', 'turn-golden-tool-first', 1800);
  const userItem = user(c, 1, '请直接用 shell 运行 printf tool-first。');
  const assistantPlaceholder = assistantPlaceholderText(c, 2, 'assistant-placeholder', 'running');
  const retiredPlaceholder = hiddenAssistantPlaceholderText(c, 2, 'assistant-placeholder', 'completed');
  const toolRunning = tool(c, 3, 'tool-first', 'call-tool-first', 'printf tool-first', 'running');
  const toolCompleted = tool(c, 3, 'tool-first', 'call-tool-first', 'printf tool-first', 'completed', { stdout: 'tool-first' });
  const assistant = assistantText(c, 4, 'assistant-final', '工具输出是 tool-first。', 'completed');
  c.events = [
    event(c, 1, 'turn_started', { turn: turn(c, 'running', [userItem, assistantPlaceholder]), item: assistantPlaceholder }),
    event(c, 2, 'turn_item_upsert', { turn: turn(c, 'running', [retiredPlaceholder, toolRunning]), item: toolRunning }),
    event(c, 3, 'turn_item_upsert', { turn: turn(c, 'running', [toolCompleted]), item: toolCompleted }),
    event(c, 4, 'turn_completed', { turn: turn(c, 'completed', [userItem, retiredPlaceholder, toolCompleted, assistant], { completedAt: 1900, responseDurationMs: 100 }) }),
  ];
  // ordered=[user, retiredPlaceholder, tool, assistant] → presentationSeq 1/2/3/4 → ×1000
  c.expected = [
    signatureMessage('message', 'user_message', 1000, userItem.content),
    signatureTool(3000, 'shell_exec', 'success'),
    signatureMessage('message', 'assistant_text', 4000, assistant.content),
  ];
  return c;
}

function multiToolOutOfOrderCase() {
  const c = baseCase('multi-tool-out-of-order', 'session-golden-multi-tool', 'turn-golden-multi-tool', 3000);
  const userItem = user(c, 1, '多场景验证 3：请连续使用 shell 工具分别运行 pwd、whoami、date +%s。');
  const phaseItem = phase(c, 2, '正在理解请求并准备调用工具。', 'completed');
  const pwdRunning = tool(c, 3, 'tool-pwd', 'call-pwd', 'pwd', 'running');
  const whoamiRunning = tool(c, 4, 'tool-whoami', 'call-whoami', 'whoami', 'running');
  const dateRunning = tool(c, 5, 'tool-date', 'call-date', 'date +%s', 'running');
  const pwdCompleted = tool(c, 3, 'tool-pwd', 'call-pwd', 'pwd', 'completed', { stdout: '/\n' });
  const whoamiCompleted = tool(c, 4, 'tool-whoami', 'call-whoami', 'whoami', 'completed', { stdout: 'xie\n' });
  const dateCompleted = tool(c, 5, 'tool-date', 'call-date', 'date +%s', 'completed', { stdout: '1777604465\n' });
  const assistant = assistantText(c, 6, 'assistant-final', '按执行顺序，pwd 结果是 `/`，whoami 结果是 `xie`，date +%s 结果是 `1777604465`。', 'completed');
  c.events = [
    event(c, 1, 'turn_item_upsert', { turn: turn(c, 'running', [userItem]), item: userItem }),
    event(c, 2, 'turn_item_upsert', { turn: turn(c, 'running', [phaseItem]), item: phaseItem }),
    event(c, 3, 'turn_item_upsert', { turn: turn(c, 'running', [pwdRunning]), item: pwdRunning }),
    event(c, 4, 'turn_item_upsert', { turn: turn(c, 'running', [whoamiRunning]), item: whoamiRunning }),
    event(c, 5, 'turn_item_upsert', { turn: turn(c, 'running', [dateRunning]), item: dateRunning }),
    event(c, 6, 'turn_item_upsert', { turn: turn(c, 'running', [dateCompleted]), item: dateCompleted }),
    event(c, 7, 'turn_item_upsert', { turn: turn(c, 'running', [whoamiCompleted]), item: whoamiCompleted }),
    event(c, 8, 'turn_item_upsert', { turn: turn(c, 'running', [pwdCompleted]), item: pwdCompleted }),
    event(c, 9, 'turn_completed', { turn: turn(c, 'completed', [userItem, phaseItem, pwdCompleted, whoamiCompleted, dateCompleted, assistant], { completedAt: 3100, responseDurationMs: 100 }) }),
  ];
  // ordered=[user, phase, pwd, whoami, date, assistant] → 1/2/3/4/5/6 → ×1000
  c.expected = [
    signatureMessage('message', 'user_message', 1000, userItem.content),
    signatureTool(3000, 'shell_exec', 'success'),
    signatureTool(4000, 'shell_exec', 'success'),
    signatureTool(5000, 'shell_exec', 'success'),
    signatureMessage('message', 'assistant_text', 6000, assistant.content),
  ];
  return c;
}

function failedToolCase() {
  const c = baseCase('failed-tool', 'session-golden-failed-tool', 'turn-golden-failed-tool', 4000);
  const userItem = user(c, 1, '多场景验证 4：请使用 shell 工具运行 ls /definitely-not-exist-canonical-test。');
  const phaseItem = phase(c, 2, '正在理解请求并准备调用工具。', 'completed');
  const failed = tool(c, 3, 'tool-failed', 'call-failed', 'ls /definitely-not-exist-canonical-test', 'failed', { stderr: 'No such file or directory\n', exit_code: 1 });
  const assistant = assistantText(c, 4, 'assistant-final', '失败原因是目标路径不存在。', 'completed');
  c.events = [
    event(c, 1, 'turn_item_upsert', { turn: turn(c, 'running', [userItem]), item: userItem }),
    event(c, 2, 'turn_item_upsert', { turn: turn(c, 'running', [phaseItem]), item: phaseItem }),
    event(c, 3, 'turn_item_upsert', { turn: turn(c, 'running', [failed]), item: failed }),
    event(c, 4, 'turn_completed', { turn: turn(c, 'completed', [userItem, phaseItem, failed, assistant], { completedAt: 4100, responseDurationMs: 100 }) }),
  ];
  // ordered=[user, phase, failed, assistant] → 1/2/3/4 → ×1000
  c.expected = [
    signatureMessage('message', 'user_message', 1000, userItem.content),
    signatureTool(3000, 'shell_exec', 'error'),
    signatureMessage('message', 'assistant_text', 4000, assistant.content),
  ];
  return c;
}

function cancelledToolCase() {
  const c = baseCase('cancelled-tool', 'session-golden-cancelled-tool', 'turn-golden-cancelled-tool', 5000);
  const userItem = user(c, 1, '多场景验证 5：请使用 shell 工具运行 sleep 20。');
  const phaseItem = phase(c, 2, '正在理解请求并准备调用工具。', 'cancelled');
  const running = tool(c, 3, 'tool-cancelled', 'call-cancelled', 'sleep 20 && echo sleep done', 'running');
  const cancelled = tool(c, 3, 'tool-cancelled', 'call-cancelled', 'sleep 20 && echo sleep done', 'cancelled');
  c.events = [
    event(c, 1, 'turn_item_upsert', { turn: turn(c, 'running', [userItem]), item: userItem }),
    event(c, 2, 'turn_item_upsert', { turn: turn(c, 'running', [running]), item: running }),
    event(c, 3, 'turn_completed', { turn: turn(c, 'cancelled', [userItem, phaseItem, cancelled], { completedAt: 5100, responseDurationMs: 100 }) }),
  ];
  // ordered=[user, phase, cancelled] → 1/2/3 → ×1000
  c.expected = [
    signatureMessage('message', 'user_message', 1000, userItem.content),
    signatureTool(3000, 'shell_exec', 'error', false),
  ];
  return c;
}

function terminalEmptyAssistantCase() {
  const c = baseCase('terminal-empty-assistant', 'session-golden-empty-terminal', 'turn-golden-empty-terminal', 6000);
  const userItem = user(c, 1, '请发送一个空终态校验。');
  const assistantPending = assistantPlaceholderText(c, 2, 'assistant-placeholder', 'running');
  const assistantEmptyCompleted = assistantPlaceholderText(c, 2, 'assistant-placeholder', 'completed');
  assistantEmptyCompleted.content = '';
  c.events = [
    event(c, 1, 'turn_started', { turn: turn(c, 'running', [userItem, assistantPending]), item: assistantPending }),
    event(c, 2, 'turn_completed', { turn: turn(c, 'completed', [userItem, assistantEmptyCompleted], { completedAt: 6100, responseDurationMs: 100 }) }),
  ];
  // ordered=[user, assistantEmptyCompleted]; empty assistant 不可见 → 仅 user(1) → ×1000
  c.expected = [
    signatureMessage('message', 'user_message', 1000, userItem.content),
  ];
  return c;
}

function localFailureCase() {
  const c = baseCase('local-failure-settles', 'session-golden-local-failure', 'turn-golden-local-failure', 7000);
  const userItem = user(c, 1, '请模拟发送失败。');
  const assistantPending = assistantPlaceholderText(c, 2, 'assistant-placeholder', 'running');
  const assistantFailed = item(c, 2, 'assistant-placeholder', 'assistant_text', 'failed', {
    title: '发送失败',
    content: '发送消息失败',
  });
  c.events = [
    event(c, 1, 'turn_started', { turn: turn(c, 'running', [userItem, assistantPending]), item: assistantPending }),
    event(c, 2, 'turn_completed', { turn: turn(c, 'failed', [userItem, assistantFailed], { completedAt: 7100, responseDurationMs: 100 }) }),
  ];
  // ordered=[user, assistantFailed] → 1/2 → ×1000
  c.expected = [
    signatureMessage('message', 'user_message', 1000, userItem.content),
    signatureMessage('message', 'assistant_text', 2000, '发送消息失败'),
  ];
  return c;
}

function baseCase(name, sessionId, turnId, turnSeq) {
  return { name, sessionId, turnId, turnSeq, events: [], expected: [] };
}

function event(c, eventSeq, kind, payload) {
  return {
    eventId: `${c.name}-event-${eventSeq}`,
    eventSeq,
    kind,
    sessionId: c.sessionId,
    turnId: c.turnId,
    occurredAt: c.turnSeq + eventSeq,
    ...payload,
  };
}

function turn(c, status, items, overrides = {}) {
  return {
    sessionId: c.sessionId,
    turnId: c.turnId,
    turnSeq: c.turnSeq,
    acceptedAt: c.turnSeq,
    status,
    items,
    ...overrides,
  };
}

function item(c, itemSeq, itemId, kind, status, fields = {}) {
  const worker = fields.worker;
  const sourceThreadId = fields.sourceThreadId
    || (worker && typeof worker.roleId === 'string' && worker.roleId && worker.roleId !== 'orchestrator'
      ? `thread-${worker.roleId}`
      : `thread-${c.sessionId}`);
  return {
    sessionId: c.sessionId,
    turnId: c.turnId,
    turnSeq: c.turnSeq,
    itemId,
    itemSeq,
    kind,
    status,
    createdAt: c.turnSeq,
    updatedAt: c.turnSeq + itemSeq,
    sourceThreadId,
    visibility: {
      renderable: true,
    },
    ...fields,
  };
}

function user(c, itemSeq, content) {
  return item(c, itemSeq, 'user-message', 'user_message', 'completed', { content });
}

function phase(c, itemSeq, content, status) {
  return item(c, itemSeq, 'phase', 'system_notice', status, {
    content,
    title: '理解请求',
    visibility: {
      renderable: false,
    },
  });
}

function assistantText(c, itemSeq, itemId, content, status) {
  return item(c, itemSeq, itemId, 'assistant_text', status, { content, title: '最终回复' });
}

function assistantPlaceholderText(c, itemSeq, itemId, status) {
  return item(c, itemSeq, itemId, 'assistant_text', status, { title: '生成回复' });
}

function hiddenAssistantPlaceholderText(c, itemSeq, itemId, status) {
  return item(c, itemSeq, itemId, 'assistant_text', status, {
    title: '生成回复',
    visibility: {
      renderable: false,
    },
  });
}

function tool(c, itemSeq, itemId, callId, command, status, result = undefined) {
  const failed = status === 'failed' || status === 'cancelled';
  const toolCall = {
    callId,
    name: 'shell_exec',
    arguments: { command },
    ...(result ? { result } : {}),
    ...(failed ? { error: JSON.stringify(result || { status }) } : {}),
  };
  return item(c, itemSeq, itemId, 'tool_call', status, {
    content: status === 'running' ? '正在调用工具：shell_exec' : `命令执行${failed ? '失败' : '成功'}: ${command}`,
    title: 'shell_exec',
    tool: toolCall,
  });
}

function agentSpawnTool(c, itemSeq, itemId, callId, role, displayName, childTaskId, status) {
  const terminal = status === 'completed' || status === 'failed' || status === 'cancelled';
  const failed = status === 'failed' || status === 'cancelled';
  const toolCall = {
    callId,
    name: 'agent_spawn',
    arguments: {
      role,
      display_name: displayName,
      goal: `${displayName}执行目标`,
    },
    ...(terminal ? {
      result: {
        tool: 'agent_spawn',
        status: failed ? 'failed' : 'succeeded',
        child_task_id: childTaskId,
        role,
        title: displayName,
        ...(failed ? { error: '代理执行失败' } : { output_refs: ['代理执行完成'] }),
      },
    } : {}),
    ...(failed ? { error: '代理执行失败' } : {}),
  };
  return item(c, itemSeq, itemId, 'tool_call', status, {
    content: status === 'running' ? `正在派发代理：${displayName}` : `代理完成：${displayName}`,
    title: displayName,
    tool: toolCall,
  });
}

function agentWaitTool(c, itemSeq, itemId, callId, childTaskId, status) {
  const failed = status === 'failed' || status === 'cancelled';
  const toolCall = {
    callId,
    name: 'agent_wait',
    arguments: {
      task_ids: [childTaskId],
    },
    result: {
      tool: 'agent_wait',
      status: failed ? 'failed' : 'succeeded',
      task_ids: [childTaskId],
    },
    ...(failed ? { error: '代理等待失败' } : {}),
  };
  return item(c, itemSeq, itemId, 'tool_call', status, {
    content: status === 'running' ? '正在等待代理完成' : '代理等待完成',
    title: 'agent_wait',
    tool: toolCall,
    visibility: {
      renderable: false,
    },
  });
}

function signatureMessage(kind, itemKind, itemSeq, content) {
  return signatureMessageWithStatus(kind, itemKind, itemSeq, content, 'complete');
}

function signatureMessageWithStatus(kind, itemKind, itemSeq, content, status) {
  return {
    kind,
    itemKind,
    itemSeq,
    content,
    status,
    toolName: undefined,
    hasToolResult: false,
    hasToolError: false,
  };
}

function signatureTool(
  itemSeq,
  toolName,
  status,
  hasToolResult = status === 'success',
  hasToolError = status === 'error',
) {
  return {
    kind: 'tool',
    itemKind: 'tool_call',
    itemSeq,
    content: undefined,
    status,
    toolName,
    hasToolResult,
    hasToolError,
  };
}
