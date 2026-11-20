import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { assembleAssistantStream, expandAssistantStream } from '@deepseek-ai/dsh-llm';

// `fixtures/attempt-stream.json` is the output of `magi-bridge-client`'s `AttemptStreamRecorder` for one attempt:
// reasoning "想想", text "好", then one tool call `shell_exec` with arguments `{"a":1}`, finishing with tool_calls.
// The records are what the daemon persists for the attempt; DSH must read them with its own functions.
const records = JSON.parse(readFileSync(new URL('./fixtures/attempt-stream.json', import.meta.url), 'utf8'));

test('DSH expands the recorded attempt into the exact timed chunk sequence', () => {
  const chunks = expandAssistantStream(records);
  assert.deepEqual(
    chunks.map(({ chunk }) => chunk.type),
    ['block-start', 'reasoning-delta', 'reasoning-delta', 'block-start', 'text-delta', 'block-start',
      'tool-call-delta', 'tool-call-delta', 'tool-call-delta', 'block-end', 'block-end', 'block-end', 'finish'],
  );
  // Delta boundaries and times survive: two reasoning deltas 10 ms apart, not one joined delta.
  const reasoning = chunks.filter(({ chunk }) => chunk.type === 'reasoning-delta');
  assert.deepEqual(reasoning.map(({ time, chunk }) => [time, chunk.text]), [[1000, '想'], [1010, '想']]);
  const calls = chunks.filter(({ chunk }) => chunk.type === 'tool-call-delta');
  assert.deepEqual(calls.map(({ chunk }) => chunk.argumentsDelta), ['', '{"a":', '1}']);
  assert.equal(calls[0].chunk.name, 'shell_exec');
  assert.equal(calls[1].chunk.name, undefined);
});

test('DSH assembles the recorded attempt: its own assembly from the deltas equals the recorded block-end blocks', () => {
  const assembled = assembleAssistantStream(records);
  assert.deepEqual(assembled.order, [0, 1, 2]);
  assert.deepEqual(assembled._finish, { kind: 'tool-calls' });
  const ends = expandAssistantStream(records).filter(({ chunk }) => chunk.type === 'block-end').map(({ chunk }) => chunk.block);
  assert.deepEqual(ends, [
    { type: 'reasoning', text: '想想' },
    { type: 'text', text: '好' },
    { type: 'tool-call', id: 'c1', name: 'shell_exec', arguments: '{"a":1}' },
  ]);
  // DSH rebuilt each block from the delta records alone; the blocks the recorder closed must match them.
  const rebuilt = [...assembled.partials.values()].map(partial => partial.block);
  assert.deepEqual(rebuilt, ends);
});

const failedRecords = JSON.parse(readFileSync(new URL('./fixtures/attempt-stream-failed.json', import.meta.url), 'utf8'));
const abortedRecords = JSON.parse(readFileSync(new URL('./fixtures/attempt-stream-aborted.json', import.meta.url), 'utf8'));

test('DSH expands and assembles a failed attempt stream with open blocks and error finish', () => {
  const chunks = expandAssistantStream(failedRecords);
  assert.deepEqual(
    chunks.map(({ chunk }) => chunk.type),
    ['block-start', 'reasoning-delta', 'reasoning-delta', 'block-start', 'text-delta', 'block-start', 'tool-call-delta', 'finish'],
  );
  assert.equal(chunks.some(({ chunk }) => chunk.type === 'block-end'), false);
  const assembled = assembleAssistantStream(failedRecords);
  assert.deepEqual(assembled.finish, {
    kind: 'error',
    failure: {
      message: 'provider rejected request: service unavailable',
      code: 'provider_http_503',
      status: 503,
    },
  });
  assert.deepEqual(assembled.blocks(), [
    { type: 'reasoning', text: '想查' },
    { type: 'text', text: '正在' },
    { type: 'tool-call', id: 'c1', name: 'shell_exec', arguments: '{"a":' },
  ]);
});

test('DSH expands and assembles an aborted attempt stream with open text deltas and aborted finish', () => {
  const chunks = expandAssistantStream(abortedRecords);
  assert.deepEqual(
    chunks.map(({ chunk }) => chunk.type),
    ['block-start', 'text-delta', 'text-delta', 'finish'],
  );
  assert.equal(chunks.some(({ chunk }) => chunk.type === 'block-end'), false);
  const assembled = assembleAssistantStream(abortedRecords);
  assert.deepEqual(assembled.finish, {
    kind: 'aborted',
    failure: {
      message: 'model invocation cancelled',
      code: 'CANCELLED',
    },
  });
  assert.deepEqual(assembled.blocks(), [{ type: 'text', text: '半截' }]);
  assert.deepEqual(assembled.interruptedBlocks(), [{ type: 'text', text: '半截' }]);
});
