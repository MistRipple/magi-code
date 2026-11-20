import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { Context, Service } from '@deepseek-ai/cordis';
import { RemoteError } from '@deepseek-ai/dsh-typert-protocol';
import { expandAssistantStream } from '@deepseek-ai/dsh-llm/assistant-stream';
import { createSessionGateway } from '../session-gateway.mjs';

// Load the real browser-bundled `@deepseek-ai/dsh-api-session-controller/client` via the same module loader contract
// used by the page host and `client-access.test.mjs`.
const externals = {
  '@deepseek-ai/cordis': await import('@deepseek-ai/cordis'),
  '@deepseek-ai/dsh-api-gateway/client': await import('@deepseek-ai/dsh-api-gateway/client'),
  '@deepseek-ai/dsh-client-store': await import('@deepseek-ai/dsh-client-store'),
};
const definitions = new Map();
const evaluated = new Map();
function requireModule(id) {
  if (Object.hasOwn(externals, id)) return externals[id];
  if (evaluated.has(id)) return evaluated.get(id);
  const definition = definitions.get(id);
  if (!definition) throw new Error(`module ${id} is not available`);
  const exports = definition.factory(requireModule);
  evaluated.set(id, exports);
  return exports;
}
globalThis.window = globalThis;
globalThis.__ModuleLoader__ = {
  load: definition => definitions.set(definition.id, definition),
  require: requireModule,
};
await import('@deepseek-ai/dsh-api-session-controller/client');
const sessionControllerClient = requireModule('@deepseek-ai/dsh-api-session-controller');
const { RemoteStream } = externals['@deepseek-ai/dsh-api-gateway/client'];

function makeGateway(initialEvents = [], sourceSequences = [], through = 0) {
  const root = new Context();
  let currentLog = { events: initialEvents, sourceSequences, through };
  const gateway = createSessionGateway({
    root,
    hostGeneration: 1,
    requestOwner: async (method, params) => {
      if (method === 'host.session.list') {
        return {
          factsSequence: 1,
          sessions: [{
            sessionId: 's1',
            title: 'Live Stream Test',
            status: 'Active',
            createdAt: 1000,
            updatedAt: 1000,
            messageCount: 1,
            isRunning: true,
            hasUnreadCompletion: false,
          }],
        };
      }
      if (method === 'host.session.get') {
        return {
          factsSequence: 1,
          session: {
            sessionId: params.sessionId,
            title: 'Live Stream Test',
            status: 'Active',
            createdAt: 1000,
            updatedAt: 1000,
            messageCount: 1,
            isRunning: true,
            hasUnreadCompletion: false,
          },
        };
      }
      throw new Error(`unexpected owner request: ${method}`);
    },
    readLog: async ({ id, createdAt, initialDirectory }) => ({
      id,
      createdAt,
      initialDirectory,
      events: currentLog.events.map(event => ({ ...event })),
      sourceSequences: [...currentLog.sourceSequences],
      through: currentLog.through,
    }),
  });
  const controller = new gateway.SessionController(root);
  return {
    root,
    gateway,
    controller,
    setLog(events, nextSources, nextThrough) {
      currentLog = { events, sourceSequences: nextSources, through: nextThrough };
    },
  };
}

async function mountRealSessionClient(controller) {
  const ctx = new Context();
  const generation = { getSnapshot: () => 1, subscribe: () => () => {} };
  const listeners = new Map();
  let followOpens = 0;

  ctx.provide('connection', { generation });
  ctx.provide('fileUpload', {});
  ctx.provide('typert', { contexts: { registerClient: () => () => {} } });
  const okCall = fn => async (...args) => {
    try {
      return { ok: true, value: await fn(...args) };
    } catch (error) {
      return {
        ok: false,
        error: {
          code: error?.code ?? 'gateway/internal',
          message: String(error?.message ?? error),
          details: error?.details ?? {},
        },
      };
    }
  };
  const sessionRemote = {
    list: okCall((_req, signal) => controller.list(_req, signal)),
    control: signal => controller.control(signal),
    page: okCall((req, signal) => controller.page(req, signal)),
    projections: okCall((req, signal) => controller.projections(req, signal)),
    follow: (req, signal) => {
      followOpens += 1;
      return controller.follow(req, signal);
    },
    create: okCall((req, signal) => controller.create(req, signal)),
    prompt: okCall((req, signal) => controller.prompt(req, signal)),
    cancel: okCall((req, signal) => controller.cancel(req, signal)),
    rename: okCall((req, signal) => controller.rename(req, signal)),
    updateQueue: okCall((req, signal) => controller.updateQueue(req, signal)),
  };
  const remote = {
    session: sessionRemote,
    commands: {},
    subagents: {},
    $on(name, fn) {
      let set = listeners.get(name);
      if (!set) listeners.set(name, set = new Set());
      set.add(fn);
      return () => set.delete(fn);
    },
    $stream(options) {
      return new RemoteStream({ generation }, options);
    },
  };
  class RemoteService extends Service {
    constructor(owner) { super(owner, 'remote'); }
    async $mount() { return async () => {}; }
  }
  ctx.provide('remote', remote);
  ctx.provide('remote.session', sessionRemote);
  ctx.provide('remote.commands', {});
  ctx.provide('remote.subagents', {});
  const fiber = ctx.plugin(sessionControllerClient);
  await fiber.await();
  await waitFor(() => ctx.sessions.list.getSnapshot().ids.includes('s1'));
  return {
    ctx,
    getFollowOpens: () => followOpens,
    dispose: async () => { await fiber.dispose(); },
  };
}

async function waitFor(predicate, timeoutMs = 2000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  throw new Error('timed out waiting for client state');
}

describe('live assistant stream (follow assistant-stream)', () => {
  it('drives the real DSH Session client through live chunks, durable settlement, and step-end retirement with zero rebaseline', async () => {
    const initial = [
      { seq: 0, time: 1000, type: 'turn/start', data: { turn: 1 } },
      { seq: 1, time: 1001, type: 'step/start', data: { turn: 1, step: 1 } },
    ];
    const { gateway, controller } = makeGateway(initial, [1, 2], 2);
    const client = await mountRealSessionClient(controller);
    try {
      const ref = client.ctx.sessions.retain('s1', { source: 'controllerOperation' });
      const binding = await ref.ready;
      assert.equal(client.getFollowOpens(), 1);

      gateway.observeAssistantStream({
        hostGeneration: 1,
        sessionId: 's1',
        frame: { kind: 'start', attemptId: 'step-1:attempt:1', turn: 1, step: 1, afterEventSeq: 2 },
      });
      gateway.observeAssistantStream({
        hostGeneration: 1,
        sessionId: 's1',
        frame: {
          kind: 'chunk',
          attemptId: 'step-1:attempt:1',
          index: 0,
          time: 1010,
          chunk: { type: 'block-start', index: 0, blockType: 'text' },
        },
      });
      gateway.observeAssistantStream({
        hostGeneration: 1,
        sessionId: 's1',
        frame: {
          kind: 'chunk',
          attemptId: 'step-1:attempt:1',
          index: 1,
          time: 1020,
          chunk: { type: 'text-delta', index: 0, text: '你好' },
        },
      });

      // The real DSH client displays live transient assistant chunks immediately.
      await waitFor(() =>
        binding.eventSource.getSnapshot().entries.some(
          entry => entry.type === 'transient' && entry.event.data.chunk.type === 'text-delta',
        ),
      );

      gateway.observeAssistantStream({
        hostGeneration: 1,
        sessionId: 's1',
        frame: {
          kind: 'chunk',
          attemptId: 'step-1:attempt:1',
          index: 2,
          time: 1030,
          chunk: { type: 'block-end', index: 0, block: { type: 'text', text: '你好' } },
        },
      });
      gateway.observeAssistantStream({
        hostGeneration: 1,
        sessionId: 's1',
        frame: {
          kind: 'chunk',
          attemptId: 'step-1:attempt:1',
          index: 3,
          time: 1030,
          chunk: { type: 'finish', reason: { kind: 'stop' } },
        },
      });
      // Durable assistant/message + step/end on the same canonical eventSeq.
      gateway.observeCommitted({
        hostGeneration: 1,
        sessionId: 's1',
        eventSeq: 4,
        occurredAt: 1040,
        observation: {
          kind: 'assistantMessage',
          messageId: 'msg-1',
          turn: 1,
          step: 1,
          provider: 'openai',
          model: 'gpt-4.1',
          text: '你好',
          toolCalls: [],
        },
      });
      gateway.observeCommitted({
        hostGeneration: 1,
        sessionId: 's1',
        eventSeq: 4,
        occurredAt: 1040,
        observation: { kind: 'stepEnded', turn: 1, step: 1 },
      });

      // After assistant/message settles and step/end arrives, transient rows retire cleanly.
      await waitFor(() => {
        const entries = binding.eventSource.getSnapshot().entries;
        return entries.some(entry => entry.event.type === 'step/end')
          && entries.every(entry => entry.type !== 'transient');
      });
      const finalTypes = binding.eventSource.getSnapshot().entries.map(entry => entry.event.type);
      assert.deepEqual(finalTypes, ['turn/start', 'step/start', 'assistant/message', 'step/end']);
      assert.equal(client.getFollowOpens(), 1, 'real DSH client must not trigger rebaseline');
      ref.release();
    } finally {
      await client.dispose();
    }
  });

  it('settles a failed attempt as assistant/attempt and streams the retried attempt with contiguous revisions', async () => {
    const initial = [
      { seq: 0, time: 1000, type: 'turn/start', data: { turn: 1 } },
      { seq: 1, time: 1001, type: 'step/start', data: { turn: 1, step: 1 } },
    ];
    const { gateway, controller } = makeGateway(initial, [1, 2], 2);
    const ac = new AbortController();
    const iter = controller.follow({ address: { kind: 'session', sessionId: 's1' }, assistantStream: true }, ac.signal)[Symbol.asyncIterator]();
    const snap = (await iter.next()).value;
    assert.deepEqual(snap.assistantStream, { revision: 0 });

    // Attempt 1 fails.
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'start', attemptId: 'step-1:attempt:1', turn: 1, step: 1, afterEventSeq: 2 },
    });
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: {
        kind: 'chunk',
        attemptId: 'step-1:attempt:1',
        index: 0,
        time: 1010,
        chunk: { type: 'finish', reason: { kind: 'error', failure: { message: '503', code: 'provider_http_503', status: 503 } } },
      },
    });
    gateway.observeCommitted({
      hostGeneration: 1,
      sessionId: 's1',
      eventSeq: 3,
      occurredAt: 1015,
      observation: {
        kind: 'assistantAttempt',
        turn: 1,
        step: 1,
        stream: [{ type: 'chunk', time: 1010, chunk: { type: 'finish', reason: { kind: 'error', failure: { message: '503', code: 'provider_http_503', status: 503 } } } }],
      },
    });

    // Attempt 2 succeeds.
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'start', attemptId: 'step-1:attempt:2', turn: 1, step: 1, afterEventSeq: 3 },
    });
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'chunk', attemptId: 'step-1:attempt:2', index: 0, time: 1020, chunk: { type: 'finish', reason: { kind: 'stop' } } },
    });
    gateway.observeCommitted({
      hostGeneration: 1,
      sessionId: 's1',
      eventSeq: 4,
      occurredAt: 1025,
      observation: {
        kind: 'assistantMessage',
        messageId: 'msg-2',
        turn: 1,
        step: 1,
        provider: 'openai',
        model: 'gpt-4.1',
        text: 'ok',
        toolCalls: [],
      },
    });

    const frames = [];
    for (let i = 0; i < 8; i++) frames.push((await iter.next()).value);
    ac.abort();

    assert.deepEqual(
      frames.map(f => (f.type === 'assistant-stream' ? `live:${f.frame.type}` : `event:${f.event.type}`)),
      ['live:start', 'live:chunk', 'event:assistant/attempt', 'live:end', 'live:start', 'live:chunk', 'event:assistant/message', 'live:end'],
    );
    const revisions = frames.filter(f => f.type === 'assistant-stream').map(f => f.frame.revision);
    assert.deepEqual(revisions, [1, 2, 3, 4, 5, 6]);
    assert.deepEqual(frames[3].frame.outcome, { kind: 'committed', eventType: 'assistant/attempt', seq: 2 });
    assert.equal(frames[4].frame.startedAfterSeq, 2);
    assert.deepEqual(frames[7].frame.outcome, { kind: 'committed', eventType: 'assistant/message', seq: 3 });
  });

  it('reconstructs mid-attempt reconnect baseline and seamlessly settles remaining chunks in the real DSH client', async () => {
    const initial = [
      { seq: 0, time: 1000, type: 'turn/start', data: { turn: 1 } },
      { seq: 1, time: 1001, type: 'step/start', data: { turn: 1, step: 1 } },
    ];
    const { gateway, controller } = makeGateway(initial, [1, 2], 2);

    // Stream starts and emits two chunks BEFORE any follower connects.
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'start', attemptId: 'step-1:attempt:1', turn: 1, step: 1, afterEventSeq: 2 },
    });
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'chunk', attemptId: 'step-1:attempt:1', index: 0, time: 1010, chunk: { type: 'block-start', index: 0, blockType: 'text' } },
    });
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'chunk', attemptId: 'step-1:attempt:1', index: 1, time: 1020, chunk: { type: 'text-delta', index: 0, text: '半' } },
    });

    // Check raw snapshot baseline shape first.
    const ac = new AbortController();
    const rawIter = controller.follow({ address: { kind: 'session', sessionId: 's1' }, assistantStream: true }, ac.signal)[Symbol.asyncIterator]();
    const snap = (await rawIter.next()).value;
    assert.equal(snap.assistantStream.revision, 3);
    assert.equal(snap.assistantStream.activeAttempt.attemptId, 'step-1:attempt:1');
    assert.equal(snap.assistantStream.activeAttempt.startedAfterSeq, 1);
    assert.equal(snap.assistantStream.activeAttempt.nextIndex, 2);
    const expanded = expandAssistantStream(snap.assistantStream.activeAttempt.stream);
    assert.deepEqual(expanded.map(c => c.chunk.type), ['block-start', 'text-delta']);
    ac.abort();

    // Now join mid-attempt with the real DSH client.
    const client = await mountRealSessionClient(controller);
    try {
      const ref = client.ctx.sessions.retain('s1', { source: 'controllerOperation' });
      const binding = await ref.ready;
      await waitFor(() =>
        binding.eventSource.getSnapshot().entries.some(
          entry => entry.type === 'transient' && entry.event.data.chunk.text === '半',
        ),
      );

      gateway.observeAssistantStream({
        hostGeneration: 1,
        sessionId: 's1',
        frame: { kind: 'chunk', attemptId: 'step-1:attempt:1', index: 2, time: 1030, chunk: { type: 'text-delta', index: 0, text: '截' } },
      });
      gateway.observeAssistantStream({
        hostGeneration: 1,
        sessionId: 's1',
        frame: { kind: 'chunk', attemptId: 'step-1:attempt:1', index: 3, time: 1040, chunk: { type: 'block-end', index: 0, block: { type: 'text', text: '半截' } } },
      });
      gateway.observeAssistantStream({
        hostGeneration: 1,
        sessionId: 's1',
        frame: { kind: 'chunk', attemptId: 'step-1:attempt:1', index: 4, time: 1040, chunk: { type: 'finish', reason: { kind: 'stop' } } },
      });
      gateway.observeCommitted({
        hostGeneration: 1,
        sessionId: 's1',
        eventSeq: 3,
        occurredAt: 1050,
        observation: {
          kind: 'assistantMessage',
          messageId: 'msg-mid',
          turn: 1,
          step: 1,
          provider: 'openai',
          model: 'gpt-4.1',
          text: '半截',
          toolCalls: [],
        },
      });
      gateway.observeCommitted({
        hostGeneration: 1,
        sessionId: 's1',
        eventSeq: 3,
        occurredAt: 1050,
        observation: { kind: 'stepEnded', turn: 1, step: 1 },
      });

      await waitFor(() => {
        const entries = binding.eventSource.getSnapshot().entries;
        return entries.some(entry => entry.event.type === 'assistant/message')
          && entries.every(entry => entry.type !== 'transient');
      });
      assert.equal(client.getFollowOpens(), 1, 'mid-attempt reconnect must settle without rebaseline');
      ref.release();
    } finally {
      await client.dispose();
    }
  });

  it('handles abandonment and closes with gateway/overflow on daemon overflow or chunk index gap', async () => {
    const initial = [
      { seq: 0, time: 1000, type: 'turn/start', data: { turn: 1 } },
      { seq: 1, time: 1001, type: 'step/start', data: { turn: 1, step: 1 } },
    ];
    const { gateway, controller } = makeGateway(initial, [1, 2], 2);
    const ac = new AbortController();
    const iter = controller.follow({ address: { kind: 'session', sessionId: 's1' }, assistantStream: true }, ac.signal)[Symbol.asyncIterator]();
    await iter.next();

    // 1. Abandonment (e.g. replace semantics or unnormalizable stop reason).
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'start', attemptId: 'step-1:attempt:1', turn: 1, step: 1, afterEventSeq: 2 },
    });
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'chunk', attemptId: 'step-1:attempt:1', index: 0, time: 1010, chunk: { type: 'block-start', index: 0, blockType: 'text' } },
    });
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'abandon', attemptId: 'step-1:attempt:1', index: 1 },
    });

    const fStart = (await iter.next()).value;
    const fChunk = (await iter.next()).value;
    const fEnd = (await iter.next()).value;
    assert.equal(fStart.frame.revision, 1);
    assert.equal(fChunk.frame.revision, 2);
    assert.deepEqual(fEnd.frame, {
      type: 'end',
      attemptId: 'step-1:attempt:1',
      revision: 3,
      index: 1,
      outcome: { kind: 'abandoned' },
    });

    // 2. Chunk index gap triggers gateway/overflow rather than skipping a revision.
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'start', attemptId: 'step-1:attempt:2', turn: 1, step: 1, afterEventSeq: 2 },
    });
    gateway.observeAssistantStream({
      hostGeneration: 1,
      sessionId: 's1',
      frame: { kind: 'chunk', attemptId: 'step-1:attempt:2', index: 2, time: 1020, chunk: { type: 'text-delta', index: 0, text: 'skip!' } },
    });
    await assert.rejects(
      async () => {
        for (;;) {
          const next = await iter.next();
          if (next.done) break;
        }
      },
      error => error instanceof RemoteError && error.code === 'gateway/overflow',
    );
    ac.abort();
  });
});
