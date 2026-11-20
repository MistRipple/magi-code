import assert from 'node:assert/strict';
import test from 'node:test';
import { ConfigurationAccess } from '../configuration-access.mjs';

const deferred = () => {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};
function scope(id = 'business') {
  const controller = new AbortController();
  return { controller, signal: controller.signal, caller: Object.freeze({ pluginId: id, instanceId: id,
    hostGeneration: 1, activationGeneration: 1, version: '1.0.0', configurationEpoch: 1 }),
  assertCurrent() { controller.signal.throwIfAborted(); } };
}

test('all edits and synchronization share one bounded queue, immutable proposals and accepted revisions', async () => {
  let revision = 1, saved;
  const calls = [], diagnostics = [], gate = deferred();
  const actor = scope();
  const access = new ConfigurationAccess({ currentRevision: () => revision, onDiagnostic: value => diagnostics.push(value),
    requestOwner: async (method, params) => {
      calls.push({ method, params });
      if (calls.length === 1) await gate.promise;
      if (method === 'loading.edit.submit') return { revision: ++revision, result: 'include:business' };
      return { revision };
    } });
  const operation = { kind: 'update', treeId: 'include', id: 'business', options: { config: { value: 2 } } };
  const first = access.edit(actor, operation, { onAccepted: () => { saved = 2; } });
  operation.options.config.value = 99;
  const second = access.edit(actor, () => ({ ...operation, expectedConfig: { value: saved } }));
  const flush = access.synchronize(actor, { refresh: true });
  await Promise.resolve();
  assert.equal(calls.length, 1);
  gate.resolve();
  await Promise.all([first, second, flush]);
  assert.equal(calls[0].params.operation.options.config.value, 2);
  assert.deepEqual(calls.map(call => call.method), ['loading.edit.submit', 'loading.edit.submit', 'loading.sources.synchronize']);
  assert.deepEqual(calls.slice(0, 2).map(call => call.params.expectedRevision), [1, 2]);
  assert.deepEqual(calls[1].params.operation.expectedConfig, { value: 2 });
  assert.equal(calls[2].params.minimumRevision, 3);
  assert.equal(calls[2].params.refresh, true);
  assert.deepEqual(diagnostics, []);
});

test('failed and malformed receipts reject the caller and never replay or poison later explicit actions', async () => {
  const calls = [], diagnostics = [];
  const replies = [new Error('source conflict'), { revision: 1 }, { revision: 2, result: 'wrong' }, { revision: 2, result: 'correct' }];
  const access = new ConfigurationAccess({ currentRevision: () => 2, onDiagnostic: value => diagnostics.push(value),
    requestOwner: async (...args) => { calls.push(args); const reply = replies.shift(); if (reply instanceof Error) throw reply; return reply; } });
  const actor = scope();
  await assert.rejects(access.edit(actor, {}), /source conflict/);
  await assert.rejects(access.edit(actor, {}), /acceptance receipt/);
  await assert.rejects(access.edit(actor, {}, { validateResult: result => result === 'correct' }), /acceptance receipt/);
  assert.deepEqual(await access.edit(actor, {}), { revision: 2, result: 'correct' });
  assert.equal(calls.length, 4);
  assert.equal(diagnostics.length, 3);
});

test('synchronization rejects before a matching applied graph and rejects stale acknowledgments', async () => {
  const replies = [{ revision: 4 }, { revision: 1 }, { revision: 3 }];
  const access = new ConfigurationAccess({ currentRevision: () => 3, onDiagnostic() {},
    requestOwner: async () => replies.shift() });
  await assert.rejects(access.synchronize(scope()), /matching local graph/);
  await assert.rejects(access.synchronize(scope()), /matching local graph/);
  assert.deepEqual(await access.synchronize(scope()), { revision: 3 });
});

test('scope revocation ends a pending synchronization and a late response cannot revive or advance it', async () => {
  const firstReply = deferred(), started = deferred(), calls = [];
  const actor = scope();
  const access = new ConfigurationAccess({ currentRevision: () => 3, onDiagnostic() {},
    requestOwner: async (method, params) => {
      calls.push({ method, params });
      if (calls.length === 1) { started.resolve(); return firstReply.promise; }
      return { revision: 3 };
    } });
  const pending = access.synchronize(actor);
  await started.promise;
  actor.controller.abort(new Error('instance replaced'));
  await assert.rejects(pending, /instance replaced/);
  firstReply.resolve({ revision: 99 });
  assert.deepEqual(await access.synchronize(scope('next')), { revision: 3 });
  assert.equal(calls[1].params.minimumRevision, 3);
  assert.throws(() => access.edit(actor, {}), /instance replaced/);
});

test('queued synchronization cancels without waiting for an earlier unknown save and capacity remains bounded', async () => {
  const gate = deferred(), started = deferred(), calls = [];
  const access = new ConfigurationAccess({ pendingLimit: 2, currentRevision: () => 1, onDiagnostic() {},
    requestOwner: async (...args) => { calls.push(args); started.resolve(); return gate.promise; } });
  const first = access.edit(scope('saving'), {});
  const actor = scope('queued');
  const queued = access.synchronize(actor);
  await started.promise;
  actor.controller.abort(new Error('stopped while queued'));
  await assert.rejects(queued, /stopped while queued/);
  assert.throws(() => access.edit(scope('overflow'), {}), /limit exceeded/);
  gate.resolve({ revision: 1 });
  await first;
  await access.edit(scope('later'), {});
  assert.equal(calls.length, 2, 'the cancelled queued action must not send an owner request');
});

test('an object snapshot revision is not silently advanced to an acknowledged or observed revision', async () => {
  const calls = [];
  const access = new ConfigurationAccess({ currentRevision: () => 3, onDiagnostic() {},
    requestOwner: async (_, params) => {
      calls.push(params.expectedRevision);
      if (params.expectedRevision !== 3) throw new Error('stale snapshot');
      return { revision: 3 };
    } });
  await assert.rejects(access.edit(scope(), {}, { expectedRevision: 1 }), /stale snapshot/);
  await access.edit(scope(), {});
  assert.deepEqual(calls, [1, 3]);
});
