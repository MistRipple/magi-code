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
  const contract = await server.ssrLoadModule('/src/shared/bridges/rust-daemon-contract.ts');
  runGoldenReplay(reducer, projection, contract);
  console.log('canonical turn golden replay passed');
} finally {
  await server.close();
}

function runGoldenReplay(reducer, projection, contract) {
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
  assertCancelledToolShowsTurnResponseDuration(reducer, projection);
  assertFailedToolWithoutAssistantShowsTurnResponseDuration(reducer, projection);
  assertBootstrapProcessingStateFromRunningCanonicalTurn(contract);
  assertBootstrapProcessingStateIgnoresTerminalCanonicalTurn(contract);
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
