import test from 'node:test';
import assert from 'node:assert/strict';
import { getEventListeners } from 'node:events';
import { foldSurface } from '@deepseek-ai/dsh-session';
import { validateSessionEventData, validateSurfaceMetadata } from '@deepseek-ai/dsh-session/surface';
import { createSessionGateway, paginate, resolveHistoryRequest } from '../session-gateway.mjs';
import { readFileSync } from 'node:fs';
import { toSessionEvent } from '../session-observer.mjs';

// Produced by magi-bridge-client's AttemptStreamRecorder (see attempt-stream.test.mjs).
const recordedStream = JSON.parse(readFileSync(new URL('./fixtures/attempt-stream.json', import.meta.url), 'utf8'));
const failedAttemptStream = JSON.parse(readFileSync(new URL('./fixtures/attempt-stream-failed.json', import.meta.url), 'utf8'));
const abortedAttemptStream = JSON.parse(readFileSync(new URL('./fixtures/attempt-stream-aborted.json', import.meta.url), 'utf8'));

// Two turns, each: turn/start, user/message, step/start, assistant/message, step/end, turn/end.
function observations() {
  const out = [];
  for (const turn of [1, 2]) {
    out.push({ kind: 'turnStarted', turn });
    out.push({ kind: 'textInput', messageId: `u${turn}`, text: `问${turn}` });
    out.push({ kind: 'stepStarted', turn, step: 1 });
    out.push({
      kind: 'assistantMessage', turn, step: 1, messageId: `a${turn}`, text: `答${turn}`, toolCalls: [],
      provider: 'p', model: 'm', usage: { inputTokens: 5, outputTokens: 2 },
    });
    out.push({ kind: 'stepEnded', turn, step: 1 });
    out.push({ kind: 'turnEnded', turn, outcome: { kind: 'completed' } });
  }
  return out;
}
const denseLog = () => observations().map((observation, seq) => Object.freeze({ seq, time: 1000 + seq, ...toSessionEvent(observation) }));

function harness({ sessions = { s1: { sessionId: 's1', createdAt: 5, cwd: '/work/p' } }, delayRead } = {}) {
  const events = denseLog();
  const reads = [];
  const gateway = createSessionGateway({
    hostGeneration: 7,
    root: { emit() {} },
    requestOwner: async (method, params) => {
      if (method === 'host.session.get') return { factsSequence: 1, session: sessions[params.sessionId] ?? null };
      throw new Error(`unexpected ${method}`);
    },
    readLog: async (identity, signal) => {
      reads.push(identity);
      await delayRead?.();
      signal?.throwIfAborted();
      return { createdAt: identity.createdAt, initialDirectory: identity.initialDirectory, through: 40, events: Object.freeze(events) };
    },
  });
  const service = Object.create(gateway.SessionController.prototype);
  const call = (method, ...args) => gateway.SessionController.prototype[method].call(service, ...args);
  return { gateway, events, reads, call };
}
const signal = () => new AbortController().signal;
const session = (sessionId, extra = {}) => ({ address: { kind: 'session', sessionId }, ...extra });
const types = page => page.records.map(record => record.event.type);

test('the produced log is a valid DSH surface: contiguous seqs fold without error', () => {
  const folded = foldSurface(denseLog());
  assert.ok(folded);
  assert.equal(denseLog().length, 12);
  assert.deepEqual(denseLog().map(event => event.seq), [...Array(12).keys()]);
});

test('pagination counts append-origin user and assistant messages and cuts on a message boundary', () => {
  const events = denseLog();
  // Newest message first: a2 (seq 9), u2 (seq 7). Budget 1 keeps only the assistant message.
  assert.deepEqual(paginate(events, undefined, 1, 11).events.map(event => event.seq), [9, 10, 11]);
  const two = paginate(events, undefined, 2, 11);
  assert.deepEqual(two.events.map(event => event.seq), [7, 8, 9, 10, 11]);
  assert.equal(two.hasMore, true);
  // The whole log fits: nothing older remains.
  const all = paginate(events, undefined, 10, 11);
  assert.equal(all.events.length, 12);
  assert.equal(all.hasMore, false);
  // beforeSeq is an exclusive upper bound: events strictly before seq 6.
  assert.deepEqual(paginate(events, 6, 10, 11).events.map(event => event.seq), [0, 1, 2, 3, 4, 5]);
  // A cut of -1 selects nothing.
  assert.deepEqual(paginate(events, undefined, 5, -1), { events: [], hasMore: false });
});

test('a turn window stops at a turn start once both minima are met', () => {
  const events = denseLog();
  // minMessages 2, minTurns 1: from the end, a2 and u2 are counted, then turn 2 starts at seq 6.
  const window = paginate(events, undefined, 50, 11, { minMessages: 2, minTurns: 1 });
  assert.deepEqual(window.events.map(event => event.seq), [6, 7, 8, 9, 10, 11]);
  assert.equal(window.hasMore, true);
  // Two turns are required: the window reaches back to the first turn start and nothing older remains.
  const two = paginate(events, undefined, 50, 11, { minMessages: 1, minTurns: 2 });
  assert.equal(two.events[0].seq, 0);
  assert.equal(two.hasMore, false);
});

test('history windows are validated exactly like the original controller', () => {
  assert.equal(resolveHistoryRequest({}).maxMessages, 50);
  for (const bad of [
    { throughSeq: -2 }, { throughSeq: 1.5 }, { beforeSeq: -1 }, { maxMessages: 0 },
    { turnWindow: { minMessages: 0, minTurns: 1 } }, { turnWindow: { minMessages: 1, minTurns: 0 } },
    { maxMessages: 3, turnWindow: { minMessages: 4, minTurns: 1 } },
    { turnWindow: { minMessages: 51, minTurns: 1 } },
  ]) {
    assert.throws(() => resolveHistoryRequest(bad), error => error.code === 'gateway/bad-request', JSON.stringify(bad));
  }
  assert.doesNotThrow(() => resolveHistoryRequest({ throughSeq: -1, maxMessages: 3, turnWindow: { minMessages: 3, minTurns: 1 } }));
});

test('page answers the cut, the records and whether older history remains', async () => {
  const { call, reads } = harness();
  const latest = await call('page', session('s1', { maxMessages: 2 }), signal());
  assert.equal(latest.cursor, 11);
  assert.equal(latest.hasMore, true);
  assert.deepEqual(latest.records[0].event.seq, 7);
  assert.ok(latest.records.every(record => record.type === 'event'));
  assert.deepEqual(reads[0], { id: 's1', createdAt: 5, initialDirectory: '/work/p' });

  const older = await call('page', session('s1', { beforeSeq: 7, maxMessages: 2 }), signal());
  assert.equal(older.cursor, 11);
  // Walking back from seq 6: a1 (seq 3) and u1 (seq 1) exhaust the budget, so the page starts at u1 with
  // only the first turn/start (seq 0) left behind.
  assert.deepEqual(types(older).slice(0, 2), ['user/message', 'step/start']);
  assert.equal(older.records[0].event.seq, 1);
  assert.equal(older.hasMore, true);
  const rest = await call('page', session('s1', { beforeSeq: 1 }), signal());
  assert.deepEqual(rest.records.map(record => record.event.seq), [0]);
  assert.equal(rest.hasMore, false);

  const pinned = await call('page', session('s1', { throughSeq: 5 }), signal());
  assert.equal(pinned.cursor, 5);
  assert.equal(pinned.records.at(-1).event.seq, 5);
  assert.equal((await call('page', session('s1', { throughSeq: -1 }), signal())).cursor, -1);
});

test('page refuses unknown sessions, subagent addresses and cuts beyond the log', async () => {
  const { call } = harness();
  await assert.rejects(call('page', session('nope'), signal()), error => error.code === 'session/not-found' && error.details?.sessionId === 'nope');
  await assert.rejects(
    call('page', { address: { kind: 'subagent', parentSessionId: 's1', childSessionId: 'c', mode: 'unknown' } }, signal()),
    error => error.code === 'subagent/not-found');
  await assert.rejects(call('page', session('s1', { throughSeq: 99 }), signal()), error => error.code === 'gateway/bad-request');
  await assert.rejects(call('page', {}, signal()), error => error.code === 'gateway/bad-request');
});

test('projections report title and subagentCatalog at the log end, and null for unknown sessions', async () => {
  const { call } = harness();
  assert.deepEqual(
    await call('projections', { sessionId: 's1' }, signal()),
    { kind: 'sequenced', asOfSeq: 11, values: { title: null, subagentCatalog: [] } },
  );
  assert.equal(await call('projections', { sessionId: 'nope' }, signal()), null);
  await assert.rejects(call('projections', { sessionId: '' }, signal()), error => error.code === 'gateway/bad-request');
});

async function collect(iterator, count) {
  const frames = [];
  for await (const frame of iterator) {
    frames.push(frame);
    if (frames.length === count) break;
  }
  return frames;
}

test('follow opens with the snapshot and then delivers later committed events as the next dense seqs', async () => {
  const { gateway, call } = harness();
  const controller = new AbortController();
  const stream = call('follow', session('s1', { maxMessages: 2, assistantStream: true }), controller.signal);
  const first = await stream.next();
  const snapshot = first.value;
  assert.equal(snapshot.type, 'snapshot');
  assert.deepEqual(snapshot.header, { version: snapshot.header.version, id: 's1', createdAt: 5, isSeeded: false, cwd: '/work/p' });
  assert.equal(snapshot.cursor, 11);
  assert.equal(snapshot.hasMore, true);
  assert.deepEqual(snapshot.projections, { asOfSeq: 11, values: { title: null, subagentCatalog: [] } });
  assert.deepEqual(snapshot.assistantStream, { revision: 0 });
  assert.equal(snapshot.records[0].event.seq, 7);

  // Covered by the snapshot (<= through 40) is dropped; later ones continue the dense log at seq 12, 13.
  gateway.observeCommitted({ sessionId: 's1', eventSeq: 40, occurredAt: 1, observation: { kind: 'turnStarted', turn: 3 } });
  gateway.observeCommitted({ sessionId: 's1', eventSeq: 41, occurredAt: 2000, observation: { kind: 'turnStarted', turn: 3 } });
  gateway.observeCommitted({ sessionId: 'other', eventSeq: 42, occurredAt: 3, observation: { kind: 'turnStarted', turn: 9 } });
  gateway.observeCommitted({ sessionId: 's1', eventSeq: 42, occurredAt: 2001, observation: { kind: 'textInput', messageId: 'u3', text: '问3' } });
  const live = [(await stream.next()).value, (await stream.next()).value];
  assert.deepEqual(live.map(frame => [frame.type, frame.event.seq, frame.event.type, frame.event.time]),
    [['event', 12, 'turn/start', 2000], ['event', 13, 'user/message', 2001]]);
  controller.abort();
  assert.equal((await stream.next()).done, true);
});

test('follow without the assistant opt-in carries no stream baseline and a follower sees events committed during its read', async () => {
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  const { gateway, call } = harness({ delayRead: () => gate });
  const controller = new AbortController();
  const stream = call('follow', session('s1'), controller.signal);
  const opening = stream.next();
  // Committed while the log is still being read: queued, then delivered after the snapshot.
  await new Promise(resolve => setTimeout(resolve, 10));
  gateway.observeCommitted({ sessionId: 's1', eventSeq: 41, occurredAt: 9, observation: { kind: 'turnStarted', turn: 3 } });
  release();
  const snapshot = (await opening).value;
  assert.equal('assistantStream' in snapshot, false);
  assert.equal(snapshot.hasMore, false);
  const event = (await stream.next()).value;
  assert.deepEqual([event.event.seq, event.event.type], [12, 'turn/start']);
  controller.abort();
});

test('a follower that falls too far behind is failed instead of silently skipping events', async () => {
  const { gateway, call } = harness();
  const controller = new AbortController();
  const stream = call('follow', session('s1'), controller.signal);
  await stream.next();
  for (let index = 0; index < 4200; index++) {
    gateway.observeCommitted({ sessionId: 's1', eventSeq: 100 + index, occurredAt: 1, observation: { kind: 'turnStarted', turn: 3 } });
  }
  await assert.rejects(stream.next(), error => error.code === 'gateway/overflow');
  controller.abort();
});

test('closing the gateway ends every follow and unknown sessions never open one', async () => {
  const { gateway, call } = harness();
  const stream = call('follow', session('s1'), new AbortController().signal);
  await stream.next();
  const ended = stream.next();
  gateway.close();
  assert.equal((await ended).done, true);
  await assert.rejects(call('follow', session('nope'), signal()).next(), error => error.code === 'session/not-found');
});

// The client's `assertSessionWireEvent` (dsh-api-session-controller src/client/session-wire-event.ts): envelope
// rules, then the event-local owner rules from dsh-session. Every record of a page or follow frame goes through it.
function assertSessionWireEvent(event) {
  for (const key of Object.keys(event)) {
    if (!['type', 'seq', 'time', 'data', 'ignorable', 'surfaceOp', 'sourceEventSeqs'].includes(key)) {
      throw new Error(`session wire event has unexpected field ${key}`);
    }
  }
  if (typeof event.type !== 'string' || !Number.isSafeInteger(event.seq) || event.seq < 0
    || !Number.isSafeInteger(event.time) || !Object.hasOwn(event, 'data') || event.data === undefined
    || (Object.hasOwn(event, 'ignorable') && event.ignorable !== true)) {
    throw new Error('session wire event has an invalid envelope');
  }
  validateSurfaceMetadata(event);
  validateSessionEventData(event, 'session wire event');
}

test('every event kind the host produces passes the client wire validation', async () => {
  const full = [
    { kind: 'turnStarted', turn: 1 },
    { kind: 'textInput', messageId: 'u1', text: '运行 ls' },
    { kind: 'stepStarted', turn: 1, step: 1 },
    {
      kind: 'requestHeader',
      reason: 'initial',
      header: {
        config: { provider: 'p', model: 'm', reasoningEffort: 'low', maxTokens: 4096 },
        tools: [{ name: 'shell_exec', description: 'Run shell', parameters: { type: 'object', properties: {} } }],
      },
    },
    { kind: 'requestContext', provider: 'p', model: 'm' },
    { kind: 'systemMessage', turn: 1, step: 1, messageId: 'system-step-1', text: 'You are Magi.' },
    { kind: 'assistantAttempt', turn: 1, step: 1, stream: failedAttemptStream },
    {
      kind: 'assistantMessage', turn: 1, step: 1, messageId: 'a1', text: '好', provider: 'p', model: 'm',
      toolCalls: [{ id: 'c1', name: 'shell_exec', arguments: '{"command":"ls"}' }],
      usage: { inputTokens: 10, outputTokens: 3, cacheReadTokens: 20, cacheWriteTokens: 2 },
      stream: recordedStream,
    },
    { kind: 'toolCall', turn: 1, step: 1, callId: 'c1', name: 'shell_exec', arguments: '{"command":"ls"}' },
    { kind: 'toolResult', turn: 1, step: 1, callId: 'c1', messageId: 't1', content: 'a.txt', failed: false },
    { kind: 'stepEnded', turn: 1, step: 1 },
    { kind: 'stepStarted', turn: 1, step: 2 },
    { kind: 'assistantMessage', turn: 1, step: 2, messageId: 'a2', text: '完成', toolCalls: [], provider: 'p', model: 'm' },
    { kind: 'stepEnded', turn: 1, step: 2 },
    { kind: 'turnEnded', turn: 1, outcome: { kind: 'completed' } },
    { kind: 'turnStarted', turn: 2 },
    { kind: 'textInput', messageId: 'u2', text: '再来' },
    { kind: 'toolCall', turn: 2, step: 1, callId: 'c2', name: 'shell_exec', arguments: '{}' },
    { kind: 'toolResult', turn: 2, step: 1, callId: 'c2', messageId: 't2', content: 'boom', failed: true, error: { name: 'ToolError', code: 'E', reason: 'x' } },
    { kind: 'turnEnded', turn: 2, outcome: { kind: 'failed', message: '模型失败', code: 'PROVIDER' } },
    { kind: 'turnStarted', turn: 3 },
    { kind: 'stepStarted', turn: 3, step: 1 },
    { kind: 'assistantAttempt', turn: 3, step: 1, stream: abortedAttemptStream },
    { kind: 'stepEnded', turn: 3, step: 1 },
    { kind: 'turnEnded', turn: 3, outcome: { kind: 'userInterrupted' } },
  ];
  const log = full.map((observation, seq) => ({ seq, time: 1000 + seq, ...toSessionEvent(observation) }));
  for (const event of log) assertSessionWireEvent(event);
  assert.ok(foldSurface(log));
  // The same events, as pages and as the follow opening, carry nothing the client would reject.
  const gateway = createSessionGateway({
    hostGeneration: 7, root: { emit() {} },
    requestOwner: async () => ({ session: { sessionId: 's1', createdAt: 5 } }),
    readLog: async identity => ({ createdAt: identity.createdAt, initialDirectory: null, through: 99, events: Object.freeze(log) }),
  });
  const proto = gateway.SessionController.prototype;
  const service = Object.create(proto);
  const page = await proto.page.call(service, session('s1'), signal());
  for (const record of page.records) assertSessionWireEvent(record.event);
  const opening = (await proto.follow.call(service, session('s1', { assistantStream: true }), signal()).next()).value;
  for (const record of opening.records) assertSessionWireEvent(record.event);
  assert.equal(opening.header.cwd, undefined);
});

test('a long follow keeps one abort listener however many events it delivers, and removes it on exit', async () => {
  const { gateway, call } = harness();
  const controller = new AbortController();
  const stream = call('follow', session('s1'), controller.signal);
  await stream.next();
  for (let index = 0; index < 50; index++) {
    gateway.observeCommitted({ sessionId: 's1', eventSeq: 100 + index, occurredAt: 1, observation: { kind: 'turnStarted', turn: 3 } });
    await stream.next();
  }
  assert.ok(getEventListeners(controller.signal, 'abort').length <= 2, 'listeners must not grow per event');
  controller.abort();
  assert.equal((await stream.next()).done, true);
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
});

test('read-only subagent mapping projects parent subagentCatalog and serves subagent page/follow/projections', async () => {
  const parentEvents = [
    { seq: 0, time: 1000, ...toSessionEvent({ kind: 'turnStarted', turn: 1 }) },
    { seq: 1, time: 1001, ...toSessionEvent({ kind: 'textInput', messageId: 'u1', text: '调研代码' }) },
    {
      seq: 2,
      time: 1010,
      ...toSessionEvent({
        kind: 'subagentCatalog',
        childId: 'task-sub-1',
        childCreatedAt: 1010,
        mode: 'one-shot',
        label: 'researcher',
      }),
    },
  ];
  const subagentEvents = [
    {
      seq: 0,
      time: 1010,
      ...toSessionEvent({
        kind: 'subagentDescriptor',
        version: 3,
        mode: 'one-shot',
        provider: 'magi',
        label: 'researcher',
      }),
    },
    { seq: 1, time: 1011, ...toSessionEvent({ kind: 'turnStarted', turn: 1 }) },
    { seq: 2, time: 1015, ...toSessionEvent({ kind: 'stepStarted', turn: 1, step: 1 }) },
    {
      seq: 3,
      time: 1020,
      ...toSessionEvent({
        kind: 'assistantMessage',
        turn: 1,
        step: 1,
        messageId: 'sub-m1',
        text: '调研完成',
        toolCalls: [],
        provider: 'openai',
        model: 'gpt-4.1',
      }),
    },
    { seq: 4, time: 1020, ...toSessionEvent({ kind: 'stepEnded', turn: 1, step: 1 }) },
    { seq: 5, time: 1030, ...toSessionEvent({ kind: 'turnEnded', turn: 1, outcome: { kind: 'completed' } }) },
  ];
  for (const event of [...parentEvents, ...subagentEvents]) assertSessionWireEvent(event);

  const gateway = createSessionGateway({
    hostGeneration: 7,
    root: { emit() {} },
    requestOwner: async (method, params) => {
      if (method === 'host.session.get' && params.sessionId === 's1') {
        return { session: { sessionId: 's1', createdAt: 5, cwd: '/work/p' } };
      }
      return { session: null };
    },
    readLog: async identity => {
      if (identity.subagentTaskId === 'task-sub-1') {
        return { createdAt: identity.createdAt, initialDirectory: identity.initialDirectory, through: 20, events: Object.freeze(subagentEvents) };
      }
      if (identity.subagentTaskId) {
        return { createdAt: identity.createdAt, initialDirectory: identity.initialDirectory, through: 20, events: Object.freeze([]) };
      }
      return { createdAt: identity.createdAt, initialDirectory: identity.initialDirectory, through: 20, events: Object.freeze(parentEvents) };
    },
  });
  const proto = gateway.SessionController.prototype;
  const service = Object.create(proto);
  const call = (method, ...args) => proto[method].call(service, ...args);

  // 1. Parent projections expose subagentCatalog.
  const parentProj = await call('projections', { sessionId: 's1' }, signal());
  assert.deepEqual(parentProj, {
    kind: 'sequenced',
    asOfSeq: 2,
    values: {
      title: null,
      subagentCatalog: [{ id: 'task-sub-1', createdAt: 1010, mode: 'one-shot', label: 'researcher' }],
    },
  });

  // 2. Subagent page works for one-shot / unknown, rejects continuable and missing children.
  const subAddr = { kind: 'subagent', parentSessionId: 's1', childSessionId: 'task-sub-1', mode: 'one-shot' };
  const subPage = await call('page', { address: subAddr }, signal());
  assert.equal(subPage.cursor, 5);
  assert.equal(subPage.records[0].event.type, 'subagent/descriptor');
  for (const record of subPage.records) assertSessionWireEvent(record.event);
  await assert.rejects(
    call('page', { address: { ...subAddr, mode: 'continuable' } }, signal()),
    error => error.code === 'subagent/unauthorized',
  );
  await assert.rejects(
    call('page', { address: { ...subAddr, childSessionId: 'missing' } }, signal()),
    error => error.code === 'subagent/not-found',
  );

  // 3. Subagent follow returns origin='subagent', parentSession, subagent & subagentTiming projections,
  // and receives live subagent committed events plus control-stream projection updates.
  const controlAc = new AbortController();
  const controlStream = call('control', controlAc.signal);
  assert.deepEqual((await controlStream.next()).value, { type: 'baseline', value: { projections: {} } });

  const subAc = new AbortController();
  const subStream = call('follow', { address: subAddr, assistantStream: true }, subAc.signal);
  const subSnap = (await subStream.next()).value;
  assert.equal(subSnap.type, 'snapshot');
  assert.equal(subSnap.header.origin, 'subagent');
  assert.equal(subSnap.header.parentSession, 's1');
  assert.equal(subSnap.header.id, 'task-sub-1');
  assert.deepEqual(subSnap.projections, {
    asOfSeq: 5,
    values: {
      subagent: { mode: 'one-shot', label: 'researcher', seq: 0 },
      subagentTiming: { settledMs: 19, lastTurnCompleted: true },
    },
  });

  gateway.observeCommitted({
    sessionId: 's1',
    subagentTaskId: 'task-sub-1',
    eventSeq: 21,
    occurredAt: 2000,
    observation: { kind: 'turnStarted', turn: 2 },
  });
  const liveSub = (await subStream.next()).value;
  assert.equal(liveSub.event.seq, 6);
  assert.equal(liveSub.event.type, 'turn/start');
  const projFrame = (await controlStream.next()).value;
  assert.equal(projFrame.type, 'projection');
  assert.equal(projFrame.sessionId, 'task-sub-1');
  assert.equal(projFrame.seq, 6);
  const timingChange = projFrame.changes.find(c => c.key === 'subagentTiming');
  assert.deepEqual(timingChange?.val, {
    settledMs: 19,
    active: { since: 2000, through: 2000 },
  });

  subAc.abort();
  controlAc.abort();
});

test('commandRun and commandDone observations map to valid DSH command lifecycle events', () => {
  const events = [
    { seq: 0, time: 100, ...toSessionEvent({ kind: 'commandRun', commandId: 'cmd-1', name: 'ping', args: ' world' }) },
    { seq: 1, time: 120, ...toSessionEvent({ kind: 'commandDone', commandId: 'cmd-1', outcome: 'success', text: 'pong:world' }) },
    { seq: 2, time: 130, ...toSessionEvent({ kind: 'commandRun', commandId: 'cmd-2', name: 'fail' }) },
    { seq: 3, time: 140, ...toSessionEvent({ kind: 'commandDone', commandId: 'cmd-2', outcome: 'error', text: 'boom' }) },
  ];
  for (const event of events) assertSessionWireEvent(event);
  assert.deepEqual(events[0], {
    seq: 0,
    time: 100,
    type: 'command/run',
    data: { commandId: 'cmd-1', name: 'ping', args: ' world', source: { kind: 'user' } },
  });
  assert.deepEqual(events[1], {
    seq: 1,
    time: 120,
    type: 'command/done',
    data: { commandId: 'cmd-1', kind: 'success', text: 'pong:world' },
  });
  assert.deepEqual(events[3], {
    seq: 3,
    time: 140,
    type: 'command/done',
    data: { commandId: 'cmd-2', kind: 'error', text: 'boom' },
  });
  assert.throws(() => toSessionEvent({ kind: 'commandDone', commandId: 'cmd-3', outcome: 'error', text: '  ' }));
});

test('sessionTitle observation maps to valid DSH session/title event and broadcasts title projection', async () => {
  const titleEvent = { seq: 0, time: 500, ...toSessionEvent({ kind: 'sessionTitle', title: '架构讨论' }) };
  assertSessionWireEvent(titleEvent);
  assert.deepEqual(titleEvent, {
    seq: 0,
    time: 500,
    type: 'session/title',
    data: { title: '架构讨论', messageSeqs: [], source: { kind: 'user' } },
  });
  assert.throws(() => toSessionEvent({ kind: 'sessionTitle', title: '   ' }));

  const { gateway, call } = harness();
  const controlAc = new AbortController();
  const controlStream = call('control', controlAc.signal);
  assert.deepEqual((await controlStream.next()).value, { type: 'baseline', value: { projections: {} } });

  const followAc = new AbortController();
  const followStream = call('follow', session('s1'), followAc.signal);
  const snap = (await followStream.next()).value;
  assert.equal(snap.projections.values.title, null);

  gateway.observeCommitted({
    sessionId: 's1',
    eventSeq: 41,
    occurredAt: 3000,
    observation: { kind: 'sessionTitle', title: '最新标题' },
  });
  const liveEvent = (await followStream.next()).value;
  assert.equal(liveEvent.event.type, 'session/title');
  assert.equal(liveEvent.event.data.title, '最新标题');
  const projFrame = (await controlStream.next()).value;
  assert.deepEqual(projFrame, {
    type: 'projection',
    sessionId: 's1',
    seq: liveEvent.event.seq,
    changes: [{ key: 'title', val: '最新标题' }],
  });
  followAc.abort();
  controlAc.abort();
});

test('backend ctx.sessions rejects unloaded history synchronously and exposes it after ctx.sessionQuery loads it', async () => {
  const { Context } = await import('@deepseek-ai/cordis');
  const { createSessionReader } = await import('../session-reader.mjs');
  const ctx = new Context();
  const reader = createSessionReader(async (method, params) => {
    if (method === 'host.session.list') {
      return {
        factsSequence: 1,
        sessions: [{ sessionId: 'cold-1', createdAt: 100, updatedAt: 200, messageCount: 1, status: 'Active', cwd: '/repo' }],
      };
    }
    if (method === 'host.session.get' && params.sessionId === 'cold-1') {
      return {
        factsSequence: 1,
        session: { sessionId: 'cold-1', createdAt: 100, updatedAt: 200, messageCount: 1, status: 'Active', cwd: '/repo' },
      };
    }
    if (method === 'host.session.history' && params.sessionId === 'cold-1') {
      return {
        session: { id: 'cold-1', createdAt: 100, cwd: '/repo' },
        through: 2,
        nextAfter: null,
        events: params.after === 0 ? [
          { seq: 0, sourceSequence: 1, occurredAt: 110, observation: { kind: 'sessionTitle', title: '冷会话标题' } },
          { seq: 1, sourceSequence: 2, occurredAt: 120, observation: { kind: 'textInput', messageId: 'm1', text: 'hello' } },
        ] : [],
      };
    }
    return { session: null };
  });
  const f1 = ctx.plugin(reader.Sessions);
  const f2 = ctx.plugin(reader.SessionPersistence);
  const f3 = ctx.plugin(reader.SessionQuery);
  await Promise.all([f1.await(), f2.await(), f3.await()]);
  await reader.syncDirectory();

  // Before loading history: header is synchronously readable, events throw a clear error.
  const listed = ctx.sessions.list();
  assert.equal(listed.length, 1);
  assert.equal(listed[0].id, 'cold-1');
  assert.equal(listed[0].header.cwd, '/repo');
  assert.throws(() => listed[0].snapshotEvents(), /not loaded.*sessionQuery/);

  // After sessionQuery reads it, title fold works and synchronous ctx.sessions reads succeed.
  const title = await ctx.sessionQuery.readTitle('cold-1');
  assert.equal(title?.title, '冷会话标题');
  const handle = ctx.sessions.get('cold-1');
  assert.equal(handle.seq, 2);
  assert.equal(handle.eventAt(0).type, 'session/title');
  assert.equal(handle.snapshotEvents().length, 2);

  await f3.dispose();
  await f2.dispose();
  await f1.dispose();
});
