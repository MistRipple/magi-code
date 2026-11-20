import test from 'node:test';
import assert from 'node:assert/strict';
import { createSessionGateway, toSessionSummary } from '../session-gateway.mjs';

const fact = (sessionId, extra = {}) => ({
  sessionId, status: 'Active', updatedAt: 10, messageCount: 2, isRunning: false, ...extra,
});

function harness(directory, options = {}) {
  const calls = [];
  const events = [];
  let sequence = options.sequence ?? 0;
  const state = new Map(directory.map(item => [item.sessionId, item]));
  const gateway = createSessionGateway({
    hostGeneration: 7,
    root: { emit: (...args) => events.push(args) },
    onDiagnostic: message => calls.push(['diagnostic', message]),
    requestOwner: async (method, params) => {
      calls.push([method, params]);
      if (options.fail?.(method)) throw new Error('daemon unavailable');
      if (method === 'host.session.list') return { factsSequence: sequence, sessions: [...state.values()] };
      if (method === 'host.session.get') return { factsSequence: sequence, session: state.get(params.sessionId) ?? null };
      throw new Error(`unexpected ${method}`);
    },
  });
  const service = Object.create(gateway.SessionController.prototype);
  return {
    gateway, calls, events, state,
    list: () => gateway.SessionController.prototype.list.call(service, {}, new AbortController().signal),
    advance: () => ++sequence,
    notify: (changes, extra = {}) => gateway.observeChanged({ hostGeneration: 7, factsSequence: sequence, changes, ...extra }),
    settle: () => new Promise(resolve => setTimeout(resolve, 20)),
  };
}

test('a directory fact becomes the DSH summary with only the members Magi has facts for', () => {
  assert.deepEqual(toSessionSummary(fact('a', { cwd: '/work/p', messageCount: 0, isRunning: true })), {
    agentAvailable: true, formatStatus: 'current', sessionId: 'a', updatedAt: 10, running: true, blank: true, cwd: '/work/p',
  });
  const archived = toSessionSummary(fact('b', { status: 'Archived' }));
  assert.equal(archived.agentAvailable, false);
  assert.equal('cwd' in archived, false);
  // The title is published only when the session has a real one; an untitled session carries none.
  assert.equal('title' in archived, false);
  assert.equal(toSessionSummary(fact('c', { title: '重构计划' })).title, '重构计划');
  assert.equal('title' in toSessionSummary(fact('d', { title: '' })), false);
  assert.throws(() => toSessionSummary({ sessionId: 'x' }), /Invalid session directory fact/);
});

test('list returns the whole directory and a cursor is refused instead of ignored', async () => {
  const h = harness([fact('a'), fact('b')]);
  const { items } = await h.list();
  assert.deepEqual(items.map(item => item.sessionId), ['a', 'b']);
  await assert.rejects(
    h.gateway.SessionController.prototype.list.call(Object.create(h.gateway.SessionController.prototype), { cursor: 'x' }, new AbortController().signal),
    /no cursor/,
  );
});

test('notifications become added, status, activity and removed increments after a listing', async () => {
  const h = harness([fact('a'), fact('b')]);
  await h.list();
  assert.deepEqual(h.events.map(e => e[0]), ['api-session/added', 'api-session/added'], 'the first listing publishes the directory');
  h.events.length = 0;

  h.state.set('a', fact('a', { isRunning: true, updatedAt: 20 }));
  h.state.set('c', fact('c'));
  h.state.delete('b');
  h.advance();
  h.notify([{ kind: 'changed', sessionId: 'a' }, { kind: 'added', sessionId: 'c' }, { kind: 'removed', sessionId: 'b' }]);
  await h.settle();
  const names = h.events.map(e => [e[0], e[1]?.sessionId ?? e[1], e[2]]);
  assert.deepEqual(names.sort(), [
    ['api-session/activity', 'a', 20],
    ['api-session/added', 'c', undefined],
    ['api-session/removed', 'b', undefined],
    ['api-session/status', 'a', true],
  ]);
});

test('a notification already covered by the listing, or before any listing, is ignored', async () => {
  const h = harness([fact('a')], { sequence: 5 });
  h.notify([{ kind: 'changed', sessionId: 'a' }]);
  await h.settle();
  assert.equal(h.calls.length, 0, 'no listing yet: nothing to diff against');
  await h.list();
  h.calls.length = 0;
  h.notify([{ kind: 'changed', sessionId: 'a' }]);
  await h.settle();
  assert.equal(h.calls.length, 0, 'the listing was read at this sequence');
  h.gateway.observeChanged({ hostGeneration: 99, factsSequence: 6, changes: [] });
  await h.settle();
  assert.equal(h.calls.length, 0, 'another host generation is not ours');
});

test('a gap, a resync flag or an overflow re-reads the whole directory', async () => {
  for (const make of [
    h => { h.advance(); h.advance(); h.notify([{ kind: 'changed', sessionId: 'a' }]); },
    h => { h.advance(); h.notify([], { resync: true }); },
    h => { h.advance(); h.notify(Array.from({ length: 257 }, (_, i) => ({ kind: 'changed', sessionId: `s${i}` }))); },
  ]) {
    const h = harness([fact('a')]);
    await h.list();
    h.events.length = 0;
    h.calls.length = 0;
    h.state.set('a', fact('a', { isRunning: true }));
    make(h);
    await h.settle();
    assert.deepEqual(h.calls.map(c => c[0]), ['host.session.list']);
    assert.deepEqual(h.events.map(e => e[0]), ['api-session/status']);
  }
});

test('reading one session never skips the notification of another that arrives while it is read', async () => {
  const h = harness([fact('a'), fact('b')]);
  await h.list();
  h.events.length = 0;
  // Both changes happened before the first read, so that read already reports the later sequence.
  h.state.set('a', fact('a', { isRunning: true }));
  h.state.set('b', fact('b', { isRunning: true }));
  h.advance();
  const first = { hostGeneration: 7, factsSequence: 1, changes: [{ kind: 'changed', sessionId: 'a' }] };
  h.advance();
  const second = { hostGeneration: 7, factsSequence: 2, changes: [{ kind: 'changed', sessionId: 'b' }] };
  h.gateway.observeChanged(first);
  h.gateway.observeChanged(second);
  await h.settle();
  assert.deepEqual(h.events.map(e => [e[0], e[1]]).sort(), [['api-session/status', 'a'], ['api-session/status', 'b']]);
});

test('a failed read keeps the published directory and is retried as a whole', async () => {
  let failing = true;
  const h = harness([fact('a')], { fail: method => failing && method === 'host.session.get' });
  await h.list();
  h.events.length = 0;
  h.state.set('a', fact('a', { isRunning: true }));
  h.advance();
  h.notify([{ kind: 'changed', sessionId: 'a' }]);
  await h.settle();
  assert.ok(h.calls.some(c => c[0] === 'diagnostic'));
  assert.equal(h.events.length, 0);
  failing = false;
  await new Promise(resolve => setTimeout(resolve, 1100));
  assert.deepEqual(h.events.map(e => e[0]), ['api-session/status']);
  h.gateway.close();
});
