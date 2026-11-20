import test from 'node:test';
import assert from 'node:assert/strict';
import { deriveEventMessage, foldRequestHeader, headerEquals, isAppendSurfaceEvent } from '@deepseek-ai/dsh-session';
import { toSessionEvent } from '../session-observer.mjs';

const observation = (overrides = {}) => ({
  kind: 'assistantMessage', turn: 2, step: 3, messageId: 'assistant-1', text: '好的',
  toolCalls: [{ id: 'call-1', name: 'shell_exec', arguments: '{"command":"ls"}' }],
  provider: 'bound-provider', model: 'model-a', ...overrides,
});

test('a committed assistant message becomes the DSH assistant/message surface event', () => {
  const event = toSessionEvent(observation());
  assert.equal(event.type, 'assistant/message');
  assert.equal(event.surfaceOp, 'append');
  assert.deepEqual(event.data.message, {
    id: 'assistant-1', role: 'assistant',
    source: { kind: 'model', provider: 'bound-provider', model: 'model-a' },
    content: [
      { type: 'text', text: '好的' },
      { type: 'tool-call', id: 'call-1', name: 'shell_exec', arguments: '{"command":"ls"}' },
    ],
  });
  assert.equal(event.data.turn, 2);
  assert.equal(event.data.step, 3);
  // 提供方流记录没有持久化：不伪造流，也不带用量。
  assert.deepEqual(event.data.stream, []);
  assert.equal('usage' in event.data, false, 'no receipt, no usage');
  // DSH 自己的投影把它当作追加型消息事件，并还原同一条消息。
  assert.equal(isAppendSurfaceEvent(event), true);
  assert.deepEqual(deriveEventMessage({ type: event.type, data: event.data }), event.data.message);
});

test('tool-call-only and text-only messages keep exactly their own blocks', () => {
  const calls = toSessionEvent(observation({ text: '' })).data.message.content;
  assert.deepEqual(calls.map((part) => part.type), ['tool-call']);
  const text = toSessionEvent(observation({ toolCalls: [] })).data.message.content;
  assert.deepEqual(text.map((part) => part.type), ['text']);
});

test('incomplete assistant facts are rejected instead of completed with placeholders', () => {
  for (const bad of [
    { step: 0 }, { messageId: '' }, { model: '' }, { provider: undefined }, { toolCalls: undefined },
    { text: '', toolCalls: [] }, { toolCalls: [{ id: 'c', name: 'n' }] },
    { toolCalls: [{ id: '', name: 'n', arguments: '{}' }] },
  ]) {
    assert.throws(() => toSessionEvent(observation(bad)), TypeError, JSON.stringify(bad));
  }
});

test('step boundaries map to DSH step/start and step/end', () => {
  assert.deepEqual(toSessionEvent({ kind: 'stepStarted', turn: 2, step: 1 }), { type: 'step/start', data: { turn: 2, step: 1 } });
  assert.deepEqual(toSessionEvent({ kind: 'stepEnded', turn: 2, step: 1 }), { type: 'step/end', data: { turn: 2, step: 1 } });
  assert.throws(() => toSessionEvent({ kind: 'stepStarted', turn: 2, step: 0 }), TypeError);
  assert.throws(() => toSessionEvent({ kind: 'stepEnded', turn: 0, step: 1 }), TypeError);
});

test('a recorded usage receipt is passed on exactly and malformed ones are rejected', () => {
  const usage = { inputTokens: 10, outputTokens: 4, cacheReadTokens: 20 };
  assert.deepEqual(toSessionEvent(observation({ usage })).data.usage, usage);
  for (const bad of [{ inputTokens: 1 }, { inputTokens: -1, outputTokens: 0 }, { inputTokens: 1, outputTokens: 1, total: 2 },
    { inputTokens: 1.5, outputTokens: 1 }, null, 'x']) {
    assert.throws(() => toSessionEvent(observation({ usage: bad })), TypeError, JSON.stringify(bad));
  }
});

test('the recorded attempt stream rides on the assistant message and malformed streams are rejected', () => {
  const stream = [
    { type: 'chunk', time: 5, chunk: { type: 'block-start', index: 0, blockType: 'text' } },
    { type: 'text-chunks', time0: 6, index: 0, dt: [], texts: ['好的'] },
  ];
  const event = toSessionEvent(observation({ stream }));
  assert.deepEqual(event.data.stream, stream);
  assert.equal(Object.isFrozen(event.data.stream[1].texts), true);
  assert.deepEqual(toSessionEvent(observation()).data.stream, [], 'no recorded stream stays an empty array');
  for (const bad of [[], 'x', [{}], [{ type: 5 }], null]) {
    assert.throws(() => toSessionEvent(observation({ stream: bad })), TypeError, JSON.stringify(bad));
  }
});

test('a failed or cancelled attempt maps to the non-surface DSH assistant/attempt event', () => {
  const stream = [
    { type: 'chunk', time: 10, chunk: { type: 'block-start', index: 0, blockType: 'text' } },
    { type: 'text-chunks', time0: 11, index: 0, dt: [], texts: ['半截'] },
    { type: 'chunk', time: 20, chunk: { type: 'finish', reason: { kind: 'error', failure: { message: 'boom', code: 'provider_http_503', status: 503 } } } },
  ];
  const event = toSessionEvent({ kind: 'assistantAttempt', turn: 2, step: 1, stream });
  assert.equal(event.type, 'assistant/attempt');
  assert.equal('surfaceOp' in event, false);
  assert.equal('sourceEventSeqs' in event, false);
  assert.deepEqual(event.data, { turn: 2, step: 1, stream });
  assert.equal(Object.isFrozen(event.data.stream[1].texts), true);
  assert.equal(isAppendSurfaceEvent(event), false);
  assert.equal(deriveEventMessage({ type: event.type, data: event.data }), null);
  for (const bad of [
    { kind: 'assistantAttempt', turn: 2, step: 0, stream },
    { kind: 'assistantAttempt', turn: 2, step: 1, stream: [] },
    { kind: 'assistantAttempt', turn: 2, step: 1, stream: null },
    { kind: 'assistantAttempt', turn: 2, step: 1, stream: [{}] },
  ]) {
    assert.throws(() => toSessionEvent(bad), TypeError, JSON.stringify(bad));
  }
});

test('requestHeader, requestContext and systemMessage map to valid DSH events and fold cleanly', () => {
  const headerObs = {
    kind: 'requestHeader',
    reason: 'initial',
    header: {
      config: { provider: 'openai', model: 'gpt-5', reasoningEffort: 'medium', maxTokens: 4096 },
      tools: [{ name: 'shell_exec', description: 'Run command', parameters: { type: 'object', properties: {} } }],
    },
  };
  const headerEvent = toSessionEvent(headerObs);
  assert.equal(headerEvent.type, 'request/header');
  assert.equal('surfaceOp' in headerEvent, false);
  assert.equal(headerEvent.data.reason, 'initial');
  const folded = foldRequestHeader([{ seq: 0, time: 1, ...headerEvent }]);
  assert.ok(folded);
  assert.equal(headerEquals(folded, headerEvent.data.header), true);

  const ctxEvent = toSessionEvent({ kind: 'requestContext', provider: 'openai', model: 'gpt-5', contextWindow: 128000 });
  assert.equal(ctxEvent.type, 'request/context');
  assert.equal('surfaceOp' in ctxEvent, false);
  assert.deepEqual(ctxEvent.data, { provider: 'openai', model: 'gpt-5', contextWindow: 128000 });

  const sysEvent = toSessionEvent({ kind: 'systemMessage', turn: 1, step: 1, messageId: 'system-step-1', text: 'You are Magi.' });
  assert.equal(sysEvent.type, 'system/message');
  assert.equal(sysEvent.surfaceOp, 'append');
  assert.equal(isAppendSurfaceEvent(sysEvent), true);
  assert.deepEqual(deriveEventMessage({ type: sysEvent.type, data: sysEvent.data }), {
    id: 'system-step-1',
    role: 'system',
    source: { kind: 'system-prompt' },
    content: [{ type: 'text', text: 'You are Magi.' }],
  });

  // Empty system text clears system prompt without emitting a text block.
  const emptySys = toSessionEvent({ kind: 'systemMessage', turn: 2, step: 1, messageId: 'system-step-2', text: '' });
  assert.deepEqual(emptySys.data.message.content, []);
  assert.equal(deriveEventMessage({ type: emptySys.type, data: emptySys.data }), null);

  for (const bad of [
    { kind: 'requestHeader', reason: 'bad', header: headerObs.header },
    { kind: 'requestHeader', reason: 'initial', header: { config: { provider: 'p', model: 'm' }, system: 'forbidden' } },
    { kind: 'requestHeader', reason: 'initial', header: { config: { provider: 'p', model: 'm' }, tools: [] } },
    { kind: 'requestHeader', reason: 'initial', header: { config: { provider: '', model: 'm' } } },
    { kind: 'requestContext', provider: 'p', model: '' },
    { kind: 'requestContext', provider: 'p', model: 'm', contextWindow: 0 },
    { kind: 'systemMessage', turn: 1, step: 0, messageId: 's', text: 'x' },
    { kind: 'systemMessage', turn: 1, step: 1, messageId: '', text: 'x' },
  ]) {
    assert.throws(() => toSessionEvent(bad), TypeError, JSON.stringify(bad));
  }
});
