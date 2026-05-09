import assert from 'node:assert/strict';
import { createServer } from 'vite';

const server = await createServer({
  root: process.cwd(),
  configFile: false,
  logLevel: 'silent',
  server: { middlewareMode: true },
});

try {
  const reducer = await server.ssrLoadModule('/src/stores/turn-reducer.ts');
  const projection = await server.ssrLoadModule('/src/stores/turn-projection.ts');
  const timelineRenderItems = await server.ssrLoadModule('/src/lib/timeline-render-items.ts');
  const contract = await server.ssrLoadModule('/src/shared/bridges/rust-daemon-contract.ts');
  runGoldenReplay(reducer, projection, timelineRenderItems, contract);
  console.log('canonical turn golden replay passed');
} finally {
  await server.close();
}

function runGoldenReplay(reducer, projection, timelineRenderItems, contract) {
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
  assertTerminalLateUpsertIsIgnored(reducer, projection);
  assertTerminalLateTurnStartedIsIgnored(reducer, projection);
  assertFailedAssistantTextUsesPlainMessageShell(reducer, projection);
  assertSplitToolStartedAndResultCollapseIntoOneCard(reducer, projection);
  assertCancelledToolShowsTurnResponseDuration(reducer, projection);
  assertFailedToolWithoutAssistantShowsTurnResponseDuration(reducer, projection);
  assertThreadVisibleRoleMetadataDoesNotBecomeWorkerBadge(reducer, projection);
  assertWorkerDispatchItemsCreateMainWorkerCard(reducer, projection, timelineRenderItems);
  assertBootstrapProcessingStateFromRunningCanonicalTurn(contract);
  assertBootstrapProcessingStateIgnoresTerminalCanonicalTurn(contract);
  assertBootstrapCarriesPendingChanges(contract);
}

function assertThreadVisibleRoleMetadataDoesNotBecomeWorkerBadge(reducer, projection) {
  const c = baseCase('thread-visible-role-metadata', 'session-golden-thread-role', 'turn-golden-thread-role', 9200);
  const assistant = assistantText(c, 2, 'assistant-role-main', '这是主线最终回复。', 'completed');
  assistant.worker = {
    taskId: 'task-primary',
    roleId: 'integration-dev',
    title: '最终回复',
  };
  assistant.visibility = {
    renderable: true,
    threadVisible: true,
    workerVisible: false,
  };
  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'completed', [assistant], { completedAt: 9300, responseDurationMs: 100 }),
  ]);
  const projectionValue = projection.buildCanonicalTimelineProjection(state);
  const artifact = findArtifactByTurnItemId(projectionValue, 'assistant-role-main');
  assert.ok(artifact, 'thread-visible assistant artifact should exist');
  assert.equal(
    artifact.message.source,
    'orchestrator',
    'thread-visible primary task metadata must not render as a worker badge source',
  );
  assert.equal(
    artifact.message.metadata?.roleId,
    undefined,
    'roleId should only be exposed as render source for worker-visible sidechain items',
  );
}

function assertWorkerDispatchItemsCreateMainWorkerCard(reducer, projection, timelineRenderItems) {
  const c = baseCase('worker-dispatch-card', 'session-golden-worker-card', 'turn-golden-worker-card', 9400);
  const orchestratorPhase = phase(c, 2, '编排者已接收请求，开始拆解执行步骤。', 'completed');
  orchestratorPhase.itemId = 'orchestrator-phase';
  orchestratorPhase.visibility = {
    renderable: true,
    threadVisible: true,
    workerVisible: false,
  };
  const userItem = user(c, 1, '请用任务系统完成一次验证。');
  const dispatchA = workerDispatch(c, 3, 'dispatch-a', 'lane-a', 'integration-dev', '实现验证', 'completed');
  const dispatchB = workerDispatch(c, 4, 'dispatch-b', 'lane-b', 'reviewer', '代码评审', 'completed');
  const orchestratorDispatchSummary = phase(c, 8, '已完成任务编排：2 个阶段、2 个执行动作，由 2 个负责人推进；我会在主线继续汇总关键进展。', 'completed');
  orchestratorDispatchSummary.itemId = 'orchestrator-dispatch-summary';
  orchestratorDispatchSummary.visibility = {
    renderable: true,
    threadVisible: true,
    workerVisible: false,
  };
  const workerTool = tool(c, 5, 'worker-tool-a', 'call-worker-a', 'printf worker', 'completed', { stdout: 'worker' });
  workerTool.laneId = 'lane-a';
  workerTool.worker = { taskId: 'task-a', workerId: 'worker-a', roleId: 'integration-dev', title: 'shell_exec' };
  workerTool.visibility = {
    renderable: true,
    threadVisible: false,
    workerVisible: true,
    workerTabIds: ['integration-dev'],
  };
  const failedWorkerTool = tool(c, 6, 'worker-tool-failed', 'call-worker-failed', 'git status --short', 'failed', { stderr: 'fatal: not a git repository\n', exit_code: 128 });
  failedWorkerTool.laneId = 'lane-a';
  failedWorkerTool.worker = { taskId: 'task-a', workerId: 'worker-a', roleId: 'integration-dev', title: 'shell_exec' };
  failedWorkerTool.visibility = {
    renderable: true,
    threadVisible: false,
    workerVisible: true,
    workerTabIds: ['integration-dev'],
  };
  const workerAssistant = assistantText(c, 7, 'worker-assistant-a', '已完成实现验证：工具调用结果已汇总，细节保留在 worker 详情中。', 'completed');
  workerAssistant.laneId = 'lane-a';
  workerAssistant.worker = { taskId: 'task-a', workerId: 'worker-a', roleId: 'integration-dev', title: '最终回复' };
  workerAssistant.visibility = {
    renderable: true,
    threadVisible: false,
    workerVisible: true,
    workerTabIds: ['integration-dev'],
  };
  const orchestratorFinal = assistantText(c, 9, 'orchestrator-final', '我这轮已经处理完：交付验收通过。\n\n详细步骤和工具记录已保留在任务卡里。', 'completed');
  orchestratorFinal.worker = { taskId: 'task-root', title: '任务完成' };
  orchestratorFinal.visibility = {
    renderable: true,
    threadVisible: true,
    workerVisible: false,
  };
  const state = reducer.replaceCanonicalTurns(c.sessionId, [
    turn(c, 'completed', [userItem, orchestratorPhase, dispatchA, dispatchB, workerTool, failedWorkerTool, workerAssistant, orchestratorDispatchSummary, orchestratorFinal], { completedAt: 9500, responseDurationMs: 100 }),
  ]);
  const projectionValue = projection.buildCanonicalTimelineProjection(state);
  assert.ok(projectionValue, 'worker dispatch projection should exist');
  const mainEntries = projectionValue.threadRenderEntries.map((entry) => entry.artifactId);
  assert.deepEqual(
    mainEntries,
    [
      `turn:${c.turnId}:user-message`,
      `turn:${c.turnId}:orchestrator-phase`,
      `turn:${c.turnId}:worker-dispatch-group`,
      `turn:${c.turnId}:orchestrator-dispatch-summary`,
      `turn:${c.turnId}:orchestrator-final`,
    ],
    'normal mainline should keep user text, orchestrator process text, the dispatch card, follow-up behavior, and final orchestrator close-out',
  );
  const orchestratorArtifact = findArtifactByTurnItemId(projectionValue, 'orchestrator-phase');
  assert.ok(orchestratorArtifact, 'orchestrator phase artifact should exist');
  assert.equal(
    orchestratorArtifact.message.type,
    'text',
    'thread-visible orchestrator system_notice should render as normal mainline text',
  );
  assert.equal(
    orchestratorArtifact.message.source,
    'orchestrator',
    'thread-visible system_notice belongs to the main orchestrator, not a worker badge',
  );
  assert.equal(
    orchestratorArtifact.message.metadata?.responseDurationMs,
    undefined,
    'orchestrator process text should not steal the total duration from the task card',
  );
  const workerCard = projectionValue.artifacts.find((artifact) => artifact.artifactId === `turn:${c.turnId}:worker-dispatch-group`);
  assert.ok(workerCard, 'worker card artifact should exist');
  assert.equal(
    workerCard.message.metadata?.responseDurationMs,
    undefined,
    'worker dispatch card should not steal the total duration once the orchestrator final exists',
  );
  const finalArtifact = findArtifactByTurnItemId(projectionValue, 'orchestrator-final');
  assert.ok(finalArtifact, 'orchestrator final artifact should exist');
  assert.equal(
    finalArtifact.message.metadata?.responseDurationMs,
    100,
    'orchestrator final should carry the turn response duration after task completion',
  );
  const dispatchBlock = workerCard.message.blocks?.find((block) => block.type === 'dispatch_group');
  assert.ok(dispatchBlock, 'worker card should render through a dispatch_group block');
  assert.deepEqual(
    dispatchBlock.lanes?.map((lane) => ({ title: lane.title, worker: lane.worker, status: lane.status })),
    [
      { title: '实现验证', worker: 'integration-dev', status: 'completed' },
      { title: '代码评审', worker: 'reviewer', status: 'completed' },
    ],
    'worker card lanes must keep dispatch order and use worker_dispatch as lane status authority',
  );
  assert.deepEqual(
    dispatchBlock.lanes?.map((lane) => ({ title: lane.title, summary: lane.summary, toolUseCount: lane.toolUseCount })),
    [
      { title: '实现验证', summary: workerAssistant.content, toolUseCount: 2 },
      { title: '代码评审', summary: undefined, toolUseCount: undefined },
    ],
    'main worker card should expose a compact stage summary without creating worker detail cards in the main lane',
  );
  assert.equal(
    projectionValue.workerRenderEntries['integration-dev']?.length,
    4,
    'integration worker tab should still receive its sidechain dispatch/tool items',
  );
  const artifactsById = new Map(projectionValue.artifacts.map((artifact) => [artifact.artifactId, artifact]));
  const integrationRenderItems = (projectionValue.workerRenderEntries['integration-dev'] || [])
    .map((entry) => artifactsById.get(entry.artifactId))
    .filter(Boolean)
    .map((artifact) => ({ key: artifact.artifactId, message: artifact.message }));
  const workerGroups = timelineRenderItems.buildWorkerStageRenderGroups(integrationRenderItems, {
    stageFallback: '执行步骤',
    directTitle: '任务总控',
    ungroupedTitle: '执行补充',
  });
  assert.deepEqual(
    workerGroups.map((group) => ({
      title: group.title,
      status: group.status,
      toolUseCount: group.toolUseCount,
      replyCount: group.replyCount,
      visibleItems: group.items.map((item) => item.message.metadata?.turnItemKind),
    })),
    [{
      title: '实现验证',
      status: 'completed',
      toolUseCount: 2,
      replyCount: 1,
      visibleItems: ['tool_call', 'tool_call', 'assistant_text'],
    }],
    'worker tab should use worker_dispatch as the stage header and hide dispatch lifecycle text from the stage body',
  );
  assert.deepEqual(
    integrationRenderItems
      .filter((item) => item.message.metadata?.turnItemKind === 'tool_call' || item.message.metadata?.turnItemKind === 'assistant_text')
      .map((item) => item.message.metadata?.laneTitle),
    ['实现验证', '实现验证', '实现验证'],
    'worker sidechain tool/reply artifacts should carry canonical lane titles for stable grouping',
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
    };
  });
}

function findArtifactByTurnItemId(projectionValue, itemId) {
  assert.ok(projectionValue, 'projection should exist');
  return projectionValue.artifacts.find((artifact) => (
    artifact.message.metadata?.turnItemId === itemId
  ));
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
      signatureMessage('message', 'user_message', 1, '请只回复一句 first frame ok。'),
      signatureMessageWithStatus('message', 'assistant_text', 2, '', 'running'),
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
  assert.deepEqual(
    projectionSignature(projection.buildCanonicalTimelineProjection(state)),
    [
      signatureMessage('message', 'user_message', 1, localUser.content),
      signatureMessageWithStatus('message', 'assistant_text', 2, '', 'running'),
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
    projectionSignature(projection.buildCanonicalTimelineProjection(result.state)),
    [
      signatureMessage('message', 'user_message', 1, acceptedUser.content),
      signatureMessageWithStatus('message', 'assistant_text', 2, '', 'running'),
    ],
    'accepted canonical turn should keep the same visible timeline shape',
  );
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
  assert.deepEqual(
    projectionSignature(projection.buildCanonicalTimelineProjection(state)),
    [
      signatureMessage('message', 'user_message', 1, userItem.content),
      signatureTool(2, 'shell_exec', 'success'),
      signatureTool(3, 'shell_exec', 'success'),
      signatureMessage('message', 'assistant_text', 100, assistant.content),
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
  const assistantItem = assistantPlaceholderText(c, 2, 'assistant-bootstrap-running', 'running');
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
      turn(c, 'running', [userItem, assistantItem]),
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
    'bootstrap should carry request binding metadata from canonical items',
  );
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
    ],
  }, {
    workspaceId: 'workspace-bootstrap-pending',
    sessionId: 'session-bootstrap-pending',
  });
  assert.deepEqual(
    camelCaseBootstrap.state.pendingChanges?.map((change) => change.filePath),
    ['created.txt'],
    'bootstrap should expose camelCase pendingChanges through AppState',
  );
  assert.equal(camelCaseBootstrap.state.pendingChangesStateVersion, 7200);

  const snakeCaseBootstrap = contract.normalizeRustBootstrapPayload({
    generatedAt: 7300,
    currentSession: { sessionId: 'session-bootstrap-pending', title: 'pending', createdAt: 7000, updatedAt: 7300 },
    sessions: [{ sessionId: 'session-bootstrap-pending', title: 'pending', createdAt: 7000, updatedAt: 7300 }],
    workspaces: [{ workspaceId: 'workspace-bootstrap-pending', rootPath: '/tmp/bootstrap-pending' }],
    pending_changes: [
      {
        filePath: 'modified.txt',
        snapshotId: 'session:session-bootstrap-pending:modified.txt',
        type: 'modify',
        additions: 1,
        deletions: 1,
      },
    ],
  }, {
    workspaceId: 'workspace-bootstrap-pending',
    sessionId: 'session-bootstrap-pending',
  });
  assert.deepEqual(
    snakeCaseBootstrap.state.pendingChanges?.map((change) => change.filePath),
    ['modified.txt'],
    'bootstrap should expose snake_case pending_changes through AppState',
  );
  assert.equal(snakeCaseBootstrap.state.pendingChangesStateVersion, 7300);
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
  c.expected = [
    signatureMessage('message', 'user_message', 1, userItem.content),
    signatureMessage('message', 'assistant_text', 3, assistant.content),
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
    signatureMessage('message', 'user_message', 1, userItem.content),
    signatureMessage('message', 'assistant_text', 2, assistantCompleted.content),
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
  c.expected = [
    signatureMessage('message', 'user_message', 1, userItem.content),
    signatureTool(3, 'shell_exec', 'success'),
    signatureMessage('message', 'assistant_text', 4, assistant.content),
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
  c.expected = [
    signatureMessage('message', 'user_message', 1, userItem.content),
    signatureTool(3, 'shell_exec', 'success'),
    signatureMessage('message', 'assistant_text', 4, assistant.content),
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
  c.expected = [
    signatureMessage('message', 'user_message', 1, userItem.content),
    signatureTool(3, 'shell_exec', 'success'),
    signatureTool(4, 'shell_exec', 'success'),
    signatureTool(5, 'shell_exec', 'success'),
    signatureMessage('message', 'assistant_text', 6, assistant.content),
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
  c.expected = [
    signatureMessage('message', 'user_message', 1, userItem.content),
    signatureTool(3, 'shell_exec', 'error'),
    signatureMessage('message', 'assistant_text', 4, assistant.content),
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
  c.expected = [
    signatureMessage('message', 'user_message', 1, userItem.content),
    signatureTool(3, 'shell_exec', 'error', false),
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
  c.expected = [
    signatureMessage('message', 'user_message', 1, userItem.content),
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
  c.expected = [
    signatureMessage('message', 'user_message', 1, userItem.content),
    signatureMessage('message', 'assistant_text', 2, '发送消息失败'),
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
    visibility: {
      renderable: true,
      threadVisible: true,
      workerVisible: false,
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
      renderable: true,
      threadVisible: false,
      workerVisible: false,
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
      threadVisible: false,
      workerVisible: false,
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

function workerDispatch(c, itemSeq, itemId, laneId, roleId, title, status) {
  return item(c, itemSeq, itemId, 'worker_dispatch', status, {
    laneId,
    laneSeq: itemSeq - 1,
    title,
    content: `已为 ${title} 创建执行分支。`,
    worker: {
      taskId: `task-${laneId}`,
      workerId: `worker-${laneId}`,
      roleId,
      title,
    },
    visibility: {
      renderable: true,
      threadVisible: false,
      workerVisible: true,
      workerTabIds: [roleId],
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
  };
}

function signatureTool(itemSeq, toolName, status, hasToolResult = status === 'success' || status === 'error') {
  return {
    kind: 'tool',
    itemKind: 'tool_call',
    itemSeq,
    content: undefined,
    status,
    toolName,
    hasToolResult,
  };
}
