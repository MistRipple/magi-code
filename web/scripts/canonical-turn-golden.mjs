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
  runGoldenReplay(reducer, projection);
  console.log('canonical turn golden replay passed');
} finally {
  await server.close();
}

function runGoldenReplay(reducer, projection) {
  const cases = [
    ordinaryChatCase(),
    singleToolCase(),
    multiToolOutOfOrderCase(),
    failedToolCase(),
    cancelledToolCase(),
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

  assertTerminalLateUpsertIsIgnored(reducer, projection);
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
  return {
    kind,
    itemKind,
    itemSeq,
    content,
    status: 'complete',
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
