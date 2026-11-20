import assert from 'node:assert/strict';
import test from 'node:test';
import { PluginLifecycle } from '../lifecycle.mjs';

function setup(t) {
  const statuses = [], diagnostics = [];
  const host = new PluginLifecycle({ hostGeneration: 3, onStatus: value => statuses.push(value), onDiagnostic: value => diagnostics.push(value) });
  t.after(() => host.close());
  const identity = (pluginId, activationGeneration = 1) => ({ hostGeneration: 3, pluginId, instanceId: pluginId, activationGeneration });
  const activate = (pluginId, plugin, activationGeneration = 1, config = {}) => host.activate({ ...identity(pluginId, activationGeneration), version: '1.0.0', plugin, config });
  return { host, activate, identity, statuses, diagnostics };
}

test('shared services suspend and resume dependents; old call leases stay revoked', async t => {
  const { host, activate, identity, statuses } = setup(t);
  const observations = [];
  const consumer = { inject: ['sampleService'], apply(ctx) {
    observations.push(ctx.sampleService.value);
    ctx.effect(() => () => observations.push('released'));
  } };
  const provider = (ctx, config) => { ctx.provide('sampleService', { value: config.value }); };
  const waiting = await activate('consumer', consumer);
  assert.equal(waiting.state, 'waiting');
  assert.deepEqual(waiting.missingServices, ['sampleService']);
  assert.throws(() => host.acquire(identity('consumer')), /not running/);
  await activate('provider', provider, 1, { value: 'first' });
  await host.settled();
  assert.equal(host.snapshot(identity('consumer')).state, 'running');
  const lease = host.acquire(identity('consumer'));
  const configurationLease = host.acquireConfiguration(identity('consumer'));
  await host.deactivate(identity('provider'));
  await host.settled();
  assert.deepEqual(observations, ['first', 'released']);
  assert.equal(host.snapshot(identity('consumer')).state, 'waiting');
  assert.equal(lease.signal.aborted, true);
  assert.equal(configurationLease.signal.aborted, true);
  await activate('provider', provider, 2, { value: 'second' });
  await host.settled();
  assert.deepEqual(observations, ['first', 'released', 'second']);
  assert.throws(() => lease.assertCurrent(), /not running/);
  host.acquire(identity('consumer')).assertCurrent();
  host.acquireConfiguration(identity('consumer')).assertCurrent();
  assert.throws(() => configurationLease.assertCurrent(), /scope ended/);
  assert.ok(statuses.some(status => status.pluginId === 'consumer' && status.state === 'waiting'));
});

test('startup failure releases its service and resources without stopping other plugins', async t => {
  const { host, activate, identity, diagnostics } = setup(t);
  let released = 0;
  await activate('healthy', () => {});
  await assert.rejects(activate('broken', (ctx) => {
    ctx.provide('transient', {});
    ctx.effect(() => () => released++);
    throw new Error('actual startup failure');
  }), /actual startup failure/);
  assert.equal(released, 1);
  host.acquire(identity('healthy')).assertCurrent();
  const waiting = await activate('dependent', { inject: ['transient'], apply() { assert.fail('failed service must be removed'); } });
  assert.equal(waiting.state, 'waiting');
  assert.ok(diagnostics.some(item => item.pluginId === 'broken' && item.message.includes('actual startup failure')));
});

test('daemon identities fence replacement, old commands and host generations', async t => {
  const { host, activate, identity } = setup(t);
  await activate('sample', () => {});
  const old = host.acquire(identity('sample'));
  await assert.rejects(activate('sample', () => {}, 2), /finish deactivation/);
  await host.deactivate(identity('sample'));
  await assert.rejects(activate('sample', () => {}), /reused/);
  await activate('sample', () => {}, 2);
  assert.throws(() => old.assertCurrent(), /activation ended/);
  await assert.rejects(host.deactivate(identity('sample')), /stale/);
  assert.throws(() => host.acquire({ ...identity('sample', 2), hostGeneration: 2 }), /stale/);
  host.acquire(identity('sample', 2)).assertCurrent();
});

test('an unawaited nested plugin failure revokes its whole package, not other packages', async t => {
  const { host, activate, identity, statuses } = setup(t);
  const failChild = Promise.withResolvers();
  let cleaned = false;
  await activate('other', () => {});
  await activate('bundle', ctx => {
    ctx.effect(() => () => { cleaned = true; });
    ctx.plugin(async () => { await failChild.promise; throw new Error('nested startup failed'); });
  });
  const lease = host.acquire(identity('bundle'));
  failChild.resolve();
  await host.settled();
  assert.equal(cleaned, true);
  assert.equal(lease.signal.aborted, true);
  assert.throws(() => host.acquire(identity('bundle')), /stale/);
  host.acquire(identity('other')).assertCurrent();
  assert.ok(statuses.some(status => status.pluginId === 'bundle' && status.state === 'failed' && status.error === 'nested startup failed'));
});

test('disabling during asynchronous activation drains cleanup and never returns running', async t => {
  const { host, activate, identity } = setup(t);
  const started = Promise.withResolvers(), finish = Promise.withResolvers();
  let released = 0;
  const activating = activate('slow', async (ctx) => {
    ctx.effect(() => () => released++);
    started.resolve();
    await finish.promise;
  });
  const rejected = assert.rejects(activating, /stale|stopping/);
  await started.promise;
  const stopping = host.deactivate(identity('slow'));
  finish.resolve();
  await Promise.all([rejected, stopping]);
  assert.equal(released, 1);
  await activate('slow', () => {}, 2);
  host.acquire(identity('slow', 2)).assertCurrent();
});

test('optional injection remains optional and releases when its provider stops', async t => {
  const { host, activate, identity } = setup(t);
  let live = 0;
  await activate('optional', ctx => {
    ctx.inject(['optionalService'], child => {
      live++;
      child.effect(() => () => live--);
    });
  });
  assert.equal(host.snapshot(identity('optional')).state, 'running');
  await activate('provider', ctx => { ctx.provide('optionalService', {}); });
  await host.settled();
  assert.equal(live, 1);
  await host.deactivate(identity('provider'));
  assert.equal(live, 0);
  host.acquire(identity('optional')).assertCurrent();
});

test('cleanup failure poisons the host instead of reporting successful deactivation', async () => {
  const host = new PluginLifecycle({ hostGeneration: 1, onStatus() {}, onDiagnostic() {} });
  const identity = { hostGeneration: 1, pluginId: 'broken-cleanup', instanceId: 'broken-cleanup', activationGeneration: 1 };
  await host.activate({ ...identity, version: '1', config: {}, plugin(ctx) {
    ctx.effect(() => () => { throw new Error('cannot close resource'); });
  } });
  await assert.rejects(host.deactivate(identity), /cleanup failure/);
  assert.throws(() => host.acquire(identity), /requires restart/);
  await host.close();
});
