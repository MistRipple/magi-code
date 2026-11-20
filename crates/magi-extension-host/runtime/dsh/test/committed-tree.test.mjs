import assert from 'node:assert/strict';
import test from 'node:test';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { PluginLifecycle } from '../lifecycle.mjs';
import { CommittedTree } from '../committed-tree.mjs';
import { prepareConfiguration } from '../configuration.mjs';
import { prepareConfigurationEdit } from '../configuration-edit.mjs';

async function fixture(t, modules, options = {}) {
  const projectDirectory = await realpath(await mkdtemp(join(tmpdir(), 'magi-committed-tree-')));
  const sourceDirectory = join(projectDirectory, 'configuration sources');
  await mkdir(sourceDirectory);
  const packages = [];
  await writeFile(join(projectDirectory, 'package.json'), '{"private":true}');
  for (const [name, source] of Object.entries(modules)) {
    const directory = join(projectDirectory, 'node_modules', name);
    await mkdir(directory, { recursive: true });
    const manifest = JSON.stringify({ name, version: '1.0.0', main: 'index.mjs' });
    await writeFile(join(directory, 'package.json'), manifest);
    await writeFile(join(directory, 'index.mjs'), source);
    packages.push({ name, version: '1.0.0', directory, projectDirectory, manifestSha256: createHash('sha256').update(manifest).digest('hex') });
  }
  const statuses = [], diagnostics = [];
  const lifecycle = new PluginLifecycle({ hostGeneration: 1, onStatus: value => statuses.push(value), onDiagnostic: value => diagnostics.push(value) });
  const tree = new CommittedTree({ lifecycle, hostGeneration: 1, onDiagnostic: value => diagnostics.push(value),
    requestOwner: options.requestOwner });
  t.after(async () => {
    try { try { await tree.close(); } finally { await lifecycle.close(); } }
    finally { await rm(projectDirectory, { recursive: true, force: true }); }
  });
  let sequence = 0;
  const counters = new Map(), bindings = new Map();
  async function apply(documents, previous) {
    const sources = [];
    for (const [name, content] of Object.entries(documents)) {
      await writeFile(join(sourceDirectory, name), content);
      sources.push({ name, content, sha256: createHash('sha256').update(content).digest('hex') });
    }
    const plan = await prepareConfiguration({ projectDirectory, sourceDirectory, sources, base: 'cordis.yml', packages, previous });
    return applyPrepared(plan);
  }
  async function applyPrepared(plan, revision = sequence + 1, instanceAction) {
    const reactivated = new Set((instanceAction?.activateTargets ?? []).map(target => target.instanceId));
    const desired = [];
    for (const executable of plan.executables) {
      const old = bindings.get(executable.instanceId);
      if (old && old.executable.fingerprint === executable.fingerprint && !reactivated.has(executable.instanceId)) desired.push(old);
      else {
        const generation = (counters.get(executable.instanceId) ?? 0) + 1;
        counters.set(executable.instanceId, generation);
        desired.push({ executable, hostGeneration: 1, activationGeneration: generation, activationOrder: old?.activationOrder ?? desired.length + 1 });
      }
    }
    bindings.clear();
    for (const binding of desired) bindings.set(binding.executable.instanceId, binding);
    const configuration = { ...plan, revision };
    const result = await tree.apply({ token: { revision, sequence: ++sequence, hostGeneration: 1 }, configuration, activations: desired, instanceAction });
    return { result, configuration, activations: desired };
  }
  const identity = binding => ({ hostGeneration: 1, pluginId: binding.executable.pluginId, instanceId: binding.executable.instanceId, activationGeneration: binding.activationGeneration });
  return { tree, lifecycle, projectDirectory, sourceDirectory, packages, apply, applyPrepared, identity, statuses, diagnostics };
}

test('committed source rows execute in real Cordis fibers and preserve distinct configured instances', async t => {
  const f = await fixture(t, { business: `import { writeFileSync, appendFileSync } from 'node:fs';
    export function apply(ctx, config) {
      writeFileSync(config.receipt, JSON.stringify({ value: config.value, entry: ctx.fiber.entry.id, loader: ctx.get('loader') !== undefined }));
      ctx.effect(() => () => appendFileSync(config.receipt, ':closed'));
    }` });
  const left = join(f.projectDirectory, 'left.txt'), right = join(f.projectDirectory, 'right.txt');
  const doc = value => JSON.stringify([{ id: 'left', name: 'business', config: { receipt: left, value } },
    { id: 'right', name: 'business', config: { receipt: right, value: 'right' } }]);
  const first = await f.apply({ 'cordis.yml': doc('first') });
  assert.deepEqual(first.result.instances.map(instance => instance.state), ['running', 'running']);
  assert.deepEqual(JSON.parse(await readFile(left, 'utf8')), { value: 'first', entry: 'include:left', loader: true });
  const oldLeft = f.lifecycle.acquire(f.identity(first.activations[0]));
  const rightLease = f.lifecycle.acquire(f.identity(first.activations[1]));
  const updateStart = f.statuses.length;
  const second = await f.apply({ 'cordis.yml': doc('second') }, first.configuration.graph);
  assert.equal(second.activations[0].activationGeneration, first.activations[0].activationGeneration, 'ordinary config uses the original fiber update contract');
  assert.equal(JSON.parse(await readFile(left, 'utf8')).value, 'second');
  assert.equal(oldLeft.signal.aborted, true);
  assert.equal(f.statuses.slice(updateStart).some(status => status.instanceId === 'include:left'
    && ['failed', 'stopped'].includes(status.state)), false, 'an ordinary update must not revoke the daemon instance');
  rightLease.assertCurrent();
  assert.equal((await readFile(right, 'utf8')).endsWith(':closed'), false);
  const stopped = await f.apply({ 'cordis.yml': JSON.stringify([{ id: 'left', name: 'business', disabled: true },
    { id: 'right', name: 'business', config: { receipt: right, value: 'right' } }]) }, second.configuration.graph);
  assert.equal(stopped.result.instances.length, 1);
  assert.equal((await readFile(left, 'utf8')).endsWith(':closed'), true);
  rightLease.assertCurrent();
  await assert.rejects(f.tree.apply({ token: { revision: 1, sequence: 1, hostGeneration: 1 }, configuration: first.configuration, activations: first.activations }), /Stale/);
});

test('real group isolation and nested includes retain service scope while public ctx.loader hides private execution internals', async t => {
  const f = await fixture(t, {
    provider: `export function apply(ctx, config) { ctx.provide('scopedService', { value: config.value }); }`,
    consumer: `import { writeFileSync } from 'node:fs'; export const inject = ['scopedService'];
      export function apply(ctx, config) { writeFileSync(config.receipt, ctx.scopedService.value); }`,
    needsloader: `import { writeFileSync } from 'node:fs'; export const inject = ['loader'];
      export function apply(ctx, config) {
        const self = ctx.loader.resolve('include:public-loader');
        writeFileSync(config.receipt, JSON.stringify({
          located: ctx.loader.locate(),
          hasBuiltins: 'builtins' in ctx.loader,
          hasInternal: 'internal' in ctx.loader,
          frozenOptions: Object.isFrozen(self.options),
          loaderScoped: self.loader.locate() === 'include:public-loader' && !('builtins' in self.loader),
        }));
      }`,
  });
  const receipt = join(f.projectDirectory, 'scoped.txt');
  const loaderReceipt = join(f.projectDirectory, 'loader.json');
  const config = [{ id: 'group', name: 'cordis:group', group: true, isolate: { scopedService: 'one' }, config: [
    { id: 'provider', name: 'provider', config: { value: 'isolated' } }, { id: 'include', name: 'cordis:include', config: { path: 'nested.yml' } }] },
    { id: 'outside', name: 'consumer', config: { receipt: join(f.projectDirectory, 'outside.txt') } },
    { id: 'public-loader', name: 'needsloader', config: { receipt: loaderReceipt } }];
  const prepared = await f.apply({ 'cordis.yml': JSON.stringify(config), 'nested.yml': JSON.stringify([{ id: 'inside', name: 'consumer', config: { receipt } }]) });
  assert.equal(await readFile(receipt, 'utf8'), 'isolated');
  assert.equal(prepared.result.instances.find(instance => instance.instanceId === 'include:include:inside').state, 'running');
  assert.deepEqual(prepared.result.instances.find(instance => instance.instanceId === 'include:outside').missingServices, ['scopedService']);
  assert.equal(prepared.result.instances.find(instance => instance.instanceId === 'include:public-loader').state, 'running');
  assert.deepEqual(JSON.parse(await readFile(loaderReceipt, 'utf8')), {
    located: 'include:public-loader',
    hasBuiltins: false,
    hasInternal: false,
    frozenOptions: true,
    loaderScoped: true,
  });
});

test('an Include reports the exact admitted file when its absolute path contains URL delimiters', async t => {
  const f = await fixture(t, {
    observer: `export function apply(ctx) { ctx.provide('objectContext', { ctx }); }`,
  });
  const name = 'configuration #2%.yml';
  const filename = join(f.sourceDirectory, name);
  const config = [{ id: 'nested', name: 'cordis:include', config: { path: filename } },
    { id: 'observer', name: 'observer' }];
  const graph = await f.apply({ 'cordis.yml': JSON.stringify(config), [name]: '[]' });
  assert.equal(graph.configuration.graph.root.entries[0].children[0].source, name);
  const loader = f.tree.objects(f.lifecycle.context.objectContext.ctx);
  assert.equal(loader.resolve('include:nested').subtree.filename, filename);
});

test('a blocked include suspends its configured descendants and can recreate their fibers after service recovery', async t => {
  const f = await fixture(t, { business: 'export function apply() {}' });
  const first = await f.apply({
    'cordis.yml': JSON.stringify([{ id: 'included', name: 'cordis:include', inject: ['gate'], config: { path: 'nested.yml' } }]),
    'nested.yml': JSON.stringify([{ id: 'leaf', name: 'business' }]),
  });
  const activation = first.activations[0];
  assert.equal(first.result.instances[0].state, 'waiting');
  assert.deepEqual(first.result.instances[0].missingServices, ['gate']);
  const provider = generation => f.lifecycle.activate({ hostGeneration: 1, pluginId: 'provider', instanceId: 'provider',
    activationGeneration: generation, version: '1.0.0', config: {}, plugin(ctx) { ctx.provide('gate', {}); } });
  const providerIdentity = { hostGeneration: 1, pluginId: 'provider', instanceId: 'provider', activationGeneration: 1 };
  await provider(1);
  await f.lifecycle.settled();
  assert.equal(f.lifecycle.snapshot(f.identity(activation)).state, 'running');
  const call = f.lifecycle.acquire(f.identity(activation));
  await f.lifecycle.deactivate(providerIdentity);
  await f.lifecycle.settled();
  assert.equal(call.signal.aborted, true);
  assert.equal(f.lifecycle.snapshot(f.identity(activation)).state, 'waiting');
  assert.deepEqual(f.lifecycle.snapshot(f.identity(activation)).missingServices, ['gate']);
  await provider(2);
  await f.lifecycle.settled();
  assert.equal(f.lifecycle.snapshot(f.identity(activation)).state, 'running');
  assert.throws(() => call.assertCurrent());
  f.lifecycle.acquire(f.identity(activation)).assertCurrent();
  assert.equal(f.statuses.some(status => status.instanceId === activation.executable.instanceId
    && ['failed', 'stopped'].includes(status.state)), false);
});

test('explicit self-disposal stays terminal and cannot be confused with a suspended ancestor', async t => {
  const f = await fixture(t, { business: `export function apply(ctx) {
    ctx.provide('selfControl', { stop: () => ctx.fiber.dispose() });
  }` });
  const first = await f.apply({ 'cordis.yml': '[{"id":"business","name":"business"}]' });
  const activation = f.identity(first.activations[0]);
  const call = f.lifecycle.acquire(activation);
  await f.lifecycle.context.selfControl.stop();
  await f.lifecycle.settled();
  assert.equal(f.lifecycle.peek(activation).state, 'stopped');
  assert.equal(call.signal.aborted, true);
  assert.throws(() => call.assertCurrent());
});

test('graph application does not await business startup that needs a later accepted provider', async t => {
  const f = await fixture(t, {
    launcher: `import { writeFileSync } from 'node:fs';
      export async function apply(ctx, config) {
        await new Promise(resolve => ctx.inject(['laterProvider'], resolve));
        writeFileSync(config.receipt, 'provider available');
      }`,
    provider: `export function apply(ctx) { ctx.provide('laterProvider', {}); }`,
  });
  const receipt = join(f.projectDirectory, 'startup.txt');
  const document = disabled => JSON.stringify([
    { id: 'launcher', name: 'launcher', config: { receipt } },
    { id: 'provider', name: 'provider', disabled },
  ]);
  const first = await f.apply({ 'cordis.yml': document(true) });
  assert.equal(first.result.instances[0].state, 'starting');
  assert.throws(() => f.lifecycle.acquire(f.identity(first.activations[0])), /not running/);
  const second = await f.apply({ 'cordis.yml': document(false) }, first.configuration.graph);
  await f.lifecycle.settled();
  assert.equal(await readFile(receipt, 'utf8'), 'provider available');
  assert.equal(f.lifecycle.snapshot(f.identity(second.activations[0])).state, 'running');
  assert.equal(second.activations[0].activationGeneration, first.activations[0].activationGeneration);
});

test('a failure after the graph receipt publishes failed and releases the starting instance', async t => {
  const f = await fixture(t, {
    broken: `import { writeFileSync } from 'node:fs';
      export async function apply(ctx, config) {
        const pending = Promise.withResolvers();
        ctx.provide('startupFailureControl', { fail: () => pending.reject(new Error('late startup failure')) });
        ctx.effect(() => () => writeFileSync(config.receipt, 'released'));
        await pending.promise;
      }`,
    healthy: `export function apply() {}`,
  });
  const receipt = join(f.projectDirectory, 'failure.txt');
  const first = await f.apply({ 'cordis.yml': JSON.stringify([
    { id: 'broken', name: 'broken', config: { receipt } }, { id: 'healthy', name: 'healthy' },
  ]) });
  assert.equal(first.result.instances[0].state, 'starting');
  f.lifecycle.context.startupFailureControl.fail();
  await f.lifecycle.settled();
  const failed = f.lifecycle.peek(f.identity(first.activations[0]));
  assert.equal(failed.state, 'failed');
  assert.equal(failed.error, 'late startup failure');
  assert.equal(await readFile(receipt, 'utf8'), 'released');
  assert.equal(f.lifecycle.context.get('startupFailureControl'), undefined);
  f.lifecycle.acquire(f.identity(first.activations[1])).assertCurrent();
});

test('a source save cannot mutate local config when the authoritative owner is unavailable', async t => {
  const f = await fixture(t, { business: `export function apply(ctx, config) {
    ctx.provide('sourceSaveControl', { save: value => ctx.fiber.update({ value }), value: () => ctx.fiber.effectiveConfig.value });
  }` });
  const first = await f.apply({ 'cordis.yml': '[{"id":"business","name":"business","config":{"value":"accepted"}}]' });
  assert.throws(() => f.lifecycle.context.sourceSaveControl.save('unaccepted'), /owner service is unavailable/);
  assert.equal(f.lifecycle.context.sourceSaveControl.value(), 'accepted');
  assert.equal(f.lifecycle.snapshot(f.identity(first.activations[0])).state, 'running');
});

test('configuration access belongs to the actual plugin context and cannot outlive its fiber', async t => {
  const calls = [];
  const f = await fixture(t, { business: `export function apply(ctx) {
    ctx.provide('configurationContexts', { root: ctx, child: ctx.plugin(() => {}).ctx });
  }` }, { requestOwner: async (method, params) => { calls.push({ method, params }); return { revision: 1 }; } });
  const first = await f.apply({ 'cordis.yml': '[{"id":"business","name":"business"}]' });
  const contexts = f.lifecycle.context.configurationContexts;
  const access = f.tree.configuration(contexts.root);
  assert.deepEqual(await access.synchronize(), { revision: 1 });
  assert.deepEqual(await f.tree.configuration(contexts.child).synchronize(), { revision: 1 });
  assert.equal(calls[0].params.caller.instanceId, 'include:business');
  assert.equal(calls[1].params.caller.instanceId, 'include:business');
  assert.throws(() => f.tree.configuration(f.lifecycle.context), /committed plugin identity/);
  assert.throws(() => f.lifecycle.acquireConfiguration(f.identity(first.activations[0]), f.lifecycle.context), /not owned/);
  await f.apply({ 'cordis.yml': '[]' }, first.configuration.graph);
  assert.throws(() => access.synchronize(), /ended|stale|inactive/);
  assert.equal(calls.length, 2);
});

test('loading object reads are immutable, resolve real source paths and preserve caller resource ownership', async t => {
  const f = await fixture(t, {
    observer: `export function apply(ctx) { ctx.provide('objectContext', { ctx }); }`,
    business: `export function apply() {}`,
  });
  const source = '[{"id":"observer","name":"observer"},{"id":"business","name":"business"}]';
  await f.apply({ 'cordis.yml': source });
  const context = f.lifecycle.context.objectContext.ctx;
  const objects = f.tree.objects(context);
  const rows = [...objects.entries()];
  assert.deepEqual(rows.map(row => row.id), ['include', 'include:observer', 'include:business']);
  const include = objects.resolve('include');
  assert.equal(fileURLToPath(include.options.config.path), join(f.sourceDirectory, 'cordis.yml'));
  assert.equal(await readFile(new URL(include.options.config.path), 'utf8'), source);
  assert.equal(include.subtree.filename, fileURLToPath(include.options.config.path));
  const target = objects.resolve('include:business');
  assert.equal(target.parent.tree, include.subtree);
  assert.equal(target.ctx.fiber.uid, context.fiber.uid, 'using a target context cannot transfer resource ownership');
  assert.equal(target.fiber.state, 2);
  assert.equal(target.fiber.entry, target);
  assert.equal(target.fiber.dispose, undefined, 'a lifecycle projection is not a writable runtime fiber');
  assert.throws(() => { target.options.disabled = true; }, TypeError);
  assert.throws(() => { target.update = () => {}; }, TypeError);
  assert.throws(() => { include.subtree.root.data.splice(0, 1); }, TypeError);
  assert.equal(f.lifecycle.snapshot(f.identity((await f.apply({ 'cordis.yml': source })).activations[1])).state, 'running');
  assert.throws(() => target.options, /revision is stale/);
  assert.equal(objects.resolve('include:business').fiber.state, 2, 'the loader service can read a new revision');
  assert.equal(objects.resolve('include:business').loader, objects);
});

test('self-disposal leaves saved enabled intent intact and entry refresh uses the current owner scope', async t => {
  const requests = [];
  const f = await fixture(t, {
    observer: `export function apply(ctx) { ctx.provide('objectContext', { ctx }); }`,
    business: `export function apply(ctx) { ctx.provide('businessContext', { ctx }); }`,
  }, { requestOwner: async (method, params) => {
    requests.push({ method, params });
    throw new Error('Owner declined activation');
  } });
  const source = '[{"id":"observer","name":"observer"},{"id":"business","name":"business"},{"id":"off","name":"business","disabled":true}]';
  const applied = await f.apply({ 'cordis.yml': source });
  const context = f.lifecycle.context.objectContext.ctx;
  const loader = f.tree.objects(context);
  const running = loader.resolve('include:business');
  await running.refresh();
  await loader.resolve('include:off').refresh();
  assert.throws(() => loader.resolve('include:off').init(), /disabled in the committed configuration/);
  assert.equal(requests.length, 0, 'active and disabled entries do not request a new activation');

  await f.lifecycle.context.businessContext.ctx.fiber.dispose();
  const status = f.lifecycle.peek(f.identity(applied.activations[1]));
  assert.equal(status.state, 'stopped');
  assert.throws(() => running.options, /runtime generation has changed/);
  const entry = loader.resolve('include:business');
  assert.equal(entry.disabled, false);
  assert.equal(entry.options.disabled, undefined);
  assert.equal(await readFile(join(f.sourceDirectory, 'cordis.yml'), 'utf8'), source);
  await assert.rejects(entry.refresh(), /Owner declined activation/);
  assert.equal(loader.resolve('include:business').fiber.state, 4);
  assert.deepEqual(requests, [{ method: 'loading.instances.change', params: {
    caller: { hostGeneration: 1, pluginId: 'observer', instanceId: 'include:observer',
      activationGeneration: 1, version: '1.0.0',
      configurationEpoch: f.lifecycle.peek(f.identity(applied.activations[0])).configurationEpoch },
    expectedRevision: 1, action: 'activate', targets: [{ instanceId: 'include:business', activationGeneration: 1 }],
  } }]);
});

test('group and Include disposal select actual loaded descendants and keep configured rows', async t => {
  const stopped = [], requests = [];
  let f, current;
  f = await fixture(t, {
    observer: `export function apply(ctx) { ctx.provide('objectContext', { ctx }); }`,
    business: `export function apply(ctx, config) { ctx.effect(() => () => ctx.emit('business/closed', config.id)); }`,
  }, { requestOwner: async (method, params) => {
    if (method === 'loading.sources.synchronize') return { revision: current.revision };
    assert.equal(method, 'loading.instances.change');
    assert.equal(params.action, 'stop');
    requests.push(params.targets.map(value => value.instanceId));
    const outcome = await f.applyPrepared(current, current.revision, { kind: 'stop', targets: params.targets });
    assert.deepEqual(outcome.result.instances.filter(value => params.targets.some(t => t.instanceId === value.instanceId))
      .map(instance => instance.state), params.targets.map(() => 'stopped'));
    return { revision: current.revision };
  } });
  f.lifecycle.context.on('business/closed', id => stopped.push(id));
  const sources = {
    'cordis.yml': JSON.stringify([
      { id: 'observer', name: 'observer' },
      { id: 'group', name: 'cordis:group', group: true, config: [
        { id: 'remove', name: 'business', config: { id: 'remove' } },
        { id: 'nested', name: 'cordis:include', config: { path: 'nested.yml' } },
      ] },
      { id: 'peer', name: 'business', config: { id: 'peer' } },
      { id: 'other', name: 'cordis:include', config: { path: 'other.yml' } },
    ]),
    'other.yml': '[{"id":"other-entry","name":"business","config":{"id":"other"}}]',
    'nested.yml': '[{"id":"child","name":"business","config":{"id":"child"}}]',
  };
  current = (await f.apply(sources)).configuration;
  const loader = f.tree.objects(f.lifecycle.context.objectContext.ctx);
  const group = loader.resolveGroup('include:group');
  group.remove('remove', true);
  await loader.flush();
  assert.deepEqual(stopped, ['remove']);
  group.stop();
  await loader.flush();
  assert.deepEqual(stopped, ['remove', 'child']);
  await loader.resolve('include:other').subtree.stop();
  assert.deepEqual(stopped, ['remove', 'child', 'other']);
  assert.deepEqual(requests, [['include:remove'], ['include:remove', 'include:nested:child'], ['include:other:other-entry']]);
  assert.equal(loader.resolve('include:peer').fiber.state, 2);
  assert.equal(loader.resolve('include:nested:child').disabled, false);
  assert.equal(loader.resolve('include:nested:child').fiber.state, 4);
  for (const [name, document] of Object.entries(sources)) {
    assert.equal(await readFile(join(f.sourceDirectory, name), 'utf8'), document);
  }
});

test('object edits submit source proposals before executing actual changes and stale handles cannot overwrite them', async t => {
  const calls = [];
  let f, current, accepted, rejectNext = true;
  f = await fixture(t, {
    observer: `export function apply(ctx) { ctx.provide('objectContext', { ctx }); }`,
    business: `export function apply(ctx, config) { ctx.provide('objectBusiness', { kind: 'original', value: config.value }); }`,
    replacement: `export function apply(ctx, config) { ctx.provide('objectBusiness', { kind: 'replacement', value: config.value }); }`,
  }, { requestOwner: async (method, params) => {
    // Node client/locked source-compiler fixture. Native CAS and persistence are
    // verified separately in loading_runtime.rs, not claimed by this adapter.
    calls.push({ method, params });
    if (method === 'loading.edit.submit') {
      if (rejectNext) { rejectNext = false; throw new Error('source conflict'); }
      assert.equal(params.expectedRevision, (accepted ?? current).revision);
      const content = await readFile(join(f.sourceDirectory, 'cordis.yml'), 'utf8');
      const source = { name: 'cordis.yml', content, sha256: createHash('sha256').update(content).digest('hex') };
      const proposal = await prepareConfigurationEdit({ projectDirectory: f.projectDirectory, sourceDirectory: f.sourceDirectory,
        sources: [source], base: 'cordis.yml', packages: f.packages, previous: (accepted ?? current).graph, operation: params.operation });
      for (const edit of proposal.edits) await writeFile(join(f.sourceDirectory, edit.name), edit.content);
      const unchanged = proposal.edits.length === 0 && JSON.stringify(proposal.plan.graph) === JSON.stringify(current.graph);
      accepted = { ...proposal.plan, revision: unchanged ? current.revision : (accepted ?? current).revision + 1 };
      return { revision: accepted.revision, result: proposal.result };
    }
    if (method === 'loading.instances.change') {
      assert.equal(params.action, 'refresh');
      assert.equal(params.expectedRevision, current.revision);
      await f.applyPrepared(current, current.revision, { kind: 'refresh', targets: params.targets });
      return { revision: current.revision };
    }
    assert.equal(method, 'loading.sources.synchronize');
    assert.ok(params.minimumRevision <= accepted.revision);
    if (current.revision < accepted.revision) current = (await f.applyPrepared(accepted, accepted.revision)).configuration;
    return { revision: current.revision };
  } });
  const source = JSON.stringify([{ id: 'observer', name: 'observer' }, { id: 'group', name: 'cordis:group', group: true,
    config: [{ id: 'member', name: 'business', config: { value: 'before' } }] }]);
  current = (await f.apply({ 'cordis.yml': source })).configuration;
  const context = f.lifecycle.context.objectContext.ctx;
  const objects = f.tree.objects(context);
  const first = objects.resolve('include:member');
  await assert.rejects(first.update({ config: { value: 'unaccepted' } }, false, true), /source conflict/);
  assert.deepEqual(f.lifecycle.context.objectBusiness, { kind: 'original', value: 'before' });
  assert.equal(await readFile(join(f.sourceDirectory, 'cordis.yml'), 'utf8'), source);
  const refreshed = [];
  const releaseObserver = f.lifecycle.context.on('loader/partial-dispose', (entry) => refreshed.push(entry.id), { global: true });
  const uid = first.fiber.uid;
  await first.update({}, false, true);
  assert.deepEqual(refreshed, ['include:member']);
  assert.equal(first.fiber.uid, uid);
  assert.equal(current.revision, 1);
  assert.equal(await readFile(join(f.sourceDirectory, 'cordis.yml'), 'utf8'), source);
  releaseObserver();
  await first.update({ config: { value: 'after' } }, false, true);
  assert.deepEqual(f.lifecycle.context.objectBusiness, { kind: 'original', value: 'after' });
  assert.equal(calls.filter(call => call.method === 'loading.instances.change').length, 1,
    'a source revision already applies the changed config without another refresh');
  assert.throws(() => first.options, /revision is stale/);
  await objects.resolve('include:member').update({ id: 'member', name: 'replacement', config: { value: 'new-module' } }, true);
  assert.deepEqual(f.lifecycle.context.objectBusiness, { kind: 'replacement', value: 'new-module' });
  await objects.update('include:member', {}, null);
  assert.equal(objects.resolve('member').parent.tree, objects);
  assert.deepEqual(objects.resolve('include').subtree.resolveGroup('group').data, []);
  assert.deepEqual(f.lifecycle.context.objectBusiness, { kind: 'replacement', value: 'new-module' });
  rejectNext = true;
  objects.remove('member');
  await assert.rejects(objects.flush(), /source conflict/);
  assert.equal(objects.resolve('member').fiber.state, 2, 'a rejected void edit did not remove the runtime');
  const rejectedSubmissions = calls.filter(call => call.method === 'loading.edit.submit').length;
  await objects.flush();
  assert.equal(calls.filter(call => call.method === 'loading.edit.submit').length, rejectedSubmissions,
    'observing a prior failure permits synchronization without replaying the rejected action');
  const beforeRemoval = calls.length;
  assert.equal(objects.remove('member'), undefined);
  await objects.flush();
  assert.equal(f.lifecycle.context.get('objectBusiness'), undefined);
  assert.equal(calls[beforeRemoval].params.operation.kind, 'remove');
  assert.deepEqual(calls[beforeRemoval].params.operation.expectedOptions,
    { id: 'member', name: 'replacement', config: { value: 'new-module' } });
  await f.apply({ 'cordis.yml': '[]' }, current.graph);
  assert.throws(() => objects.resolve('include'), /ended|stale|inactive/);
});

test('forced updates on group and Include carriers follow upstream boundary and reactivate only group-owned stopped entries', async t => {
  const calls = [];
  let f, current;
  f = await fixture(t, {
    observer: `export function apply(ctx) { ctx.provide('objectContext', { ctx }); }`,
    business: `export function apply(ctx, config) { ctx.effect(() => () => ctx.emit('business/closed', config.id)); }`,
  }, { requestOwner: async (method, params) => {
    calls.push({ method, params });
    if (method === 'loading.edit.submit') return { revision: current.revision, result: [] };
    if (method === 'loading.sources.synchronize') return { revision: current.revision };
    assert.equal(method, 'loading.instances.change');
    if (params.action === 'stop') {
      await f.applyPrepared(current, current.revision, { kind: 'stop', targets: params.targets });
      return { revision: current.revision };
    }
    assert.equal(params.action, 'refresh');
    await f.applyPrepared(current, current.revision, {
      kind: 'refresh',
      entryId: params.entryId,
      targets: params.targets,
      activateTargets: params.activateTargets,
    });
    return { revision: current.revision };
  } });
  const sources = {
    'cordis.yml': JSON.stringify([
      { id: 'observer', name: 'observer' },
      { id: 'group', name: 'cordis:group', group: true, config: [
        { id: 'keep', name: 'business', config: { id: 'keep' } },
        { id: 'stopped', name: 'business', config: { id: 'stopped' } },
        { id: 'off', name: 'business', disabled: true, config: { id: 'off' } },
        { id: 'inner', name: 'cordis:group', group: true, config: [
          { id: 'inner-stopped', name: 'business', config: { id: 'inner-stopped' } },
        ] },
        { id: 'subfile', name: 'cordis:include', config: { path: 'sub.yml' } },
      ] },
    ]),
    'sub.yml': '[{"id":"sub-stopped","name":"business","config":{"id":"sub-stopped"}}]',
  };
  const initial = await f.apply(sources);
  current = initial.configuration;
  const loader = f.tree.objects(f.lifecycle.context.objectContext.ctx);
  const group = loader.resolveGroup('include:group');
  group.remove('stopped', true);
  loader.resolveGroup('include:inner').remove('inner-stopped', true);
  await loader.resolve('include:subfile').subtree.stop();
  await loader.flush();
  assert.equal(loader.resolve('include:stopped').fiber.state, 4);
  assert.equal(loader.resolve('include:inner-stopped').fiber.state, 4);
  assert.equal(loader.resolve('include:subfile:sub-stopped').fiber.state, 4);

  const partialDisposed = [];
  const releasePartial = f.lifecycle.context.on('loader/partial-dispose', entry => partialDisposed.push(entry.id), { global: true });
  const keepUid = loader.resolve('include:keep').fiber.uid;
  const keepGeneration = f.lifecycle.peek({ hostGeneration: 1, pluginId: 'business', instanceId: 'include:keep',
    activationGeneration: initial.activations.find(a => a.executable.instanceId === 'include:keep').activationGeneration }).activationGeneration;

  // Forced update on an Include carrier refreshes only the Include carrier itself.
  await loader.resolve('include:subfile').update({}, false, true);
  assert.deepEqual(partialDisposed, ['include:subfile']);
  assert.equal(loader.resolve('include:subfile:sub-stopped').fiber.state, 4);

  partialDisposed.length = 0;
  await loader.resolve('include:group').update({}, false, true);
  releasePartial();

  assert.ok(partialDisposed.includes('include:group'));
  assert.ok(partialDisposed.includes('include:keep'));
  assert.ok(partialDisposed.includes('include:inner'));
  assert.ok(partialDisposed.includes('include:subfile'));
  assert.equal(loader.resolve('include:keep').fiber.uid, keepUid);
  assert.equal(f.lifecycle.snapshot({ hostGeneration: 1, pluginId: 'business', instanceId: 'include:keep', activationGeneration: keepGeneration }).state, 'running');
  assert.equal(loader.resolve('include:stopped').fiber.state, 2);
  assert.equal(loader.resolve('include:inner-stopped').fiber.state, 2);
  assert.equal(loader.resolve('include:off').fiber, undefined);
  assert.equal(loader.resolve('include:subfile:sub-stopped').fiber.state, 4,
    'a child Include carrier does not re-execute its separate file subtree on a parent group refresh');
  for (const [name, document] of Object.entries(sources)) {
    assert.equal(await readFile(join(f.sourceDirectory, name), 'utf8'), document);
  }
});

test('public ctx.loader supports Loader.Intercept.await, dynamic !!js disabled entries, epoch renewal and ordered exit()', async t => {
  const gate = Promise.withResolvers();
  globalThis.__magiLoaderGate = gate.promise;
  t.after(() => { gate.resolve(); delete globalThis.__magiLoaderGate; });
  const ownerCalls = [];
  let f, current, accepted;
  f = await fixture(t, {
    slow: `export async function apply() { await globalThis.__magiLoaderGate; }`,
    awaiter: `export const inject = { loader: { await: true } };
      export function apply(ctx, config) { ctx.provide('awaitedView', { step: config.step, get entries() { return [...ctx.loader.entries()].map(e => e.id); }, ctx }); }`,
    worker: `export function apply(ctx) { ctx.provide('workerActive', { ok: true }); }`,
  }, { requestOwner: async (method, params) => {
    ownerCalls.push({ method, params });
    if (method === 'loading.edit.submit') {
      const content = await readFile(join(f.sourceDirectory, 'cordis.yml'), 'utf8');
      const source = { name: 'cordis.yml', content, sha256: createHash('sha256').update(content).digest('hex') };
      const proposal = await prepareConfigurationEdit({ projectDirectory: f.projectDirectory, sourceDirectory: f.sourceDirectory,
        sources: [source], base: 'cordis.yml', packages: f.packages, previous: (accepted ?? current).graph, operation: params.operation });
      for (const edit of proposal.edits) await writeFile(join(f.sourceDirectory, edit.name), edit.content);
      accepted = { ...proposal.plan, revision: (accepted ?? current).revision + 1 };
      return { revision: accepted.revision, result: proposal.result };
    }
    if (method === 'loading.sources.synchronize') {
      if (accepted && current.revision < accepted.revision) current = (await f.applyPrepared(accepted, accepted.revision)).configuration;
      return { revision: current.revision };
    }
    assert.equal(method, 'loading.host.restart');
    return { hostGeneration: 1 };
  } });
  const makeYaml = step => [
    '- id: slow',
    '  name: slow',
    '- id: awaiter',
    '  name: awaiter',
    '  config:',
    `    step: ${step}`,
    '- id: dynamic-off',
    '  name: worker',
    '  disabled: !!js "1 === 1"',
    '- id: dynamic-group',
    '  name: cordis:group',
    '  group: true',
    '  disabled: !!js "true"',
    '  config:',
    '    - id: group-off',
    '      name: worker',
    '- id: dynamic-on',
    '  name: worker',
    '  disabled: !!js "false"',
  ].join('\n') + '\n';
  const applied = await f.apply({ 'cordis.yml': makeYaml(1) });
  current = applied.configuration;
  const byId = Object.fromEntries(applied.result.instances.map(item => [item.instanceId, item]));
  assert.equal(byId['include:slow'].state, 'starting');
  assert.equal(byId['include:slow'].activating, true);
  assert.equal(byId['include:awaiter'].state, 'waiting');
  assert.equal(byId['include:awaiter'].activating, false);
  assert.deepEqual(byId['include:awaiter'].missingServices, ['loader']);
  assert.equal(byId['include:dynamic-off'].state, 'stopped');
  assert.equal(byId['include:group-off'].state, 'stopped');
  assert.equal(byId['include:dynamic-on'].state, 'running');

  gate.resolve();
  await new Promise(resolve => setImmediate(resolve));
  await f.lifecycle.settled();
  assert.equal(f.lifecycle.snapshot(f.identity(applied.activations.find(a => a.executable.instanceId === 'include:slow'))).activating, false);
  const awaiterIdentity = f.identity(applied.activations.find(a => a.executable.instanceId === 'include:awaiter'));
  assert.equal(f.lifecycle.snapshot(awaiterIdentity).state, 'running');
  const firstEpoch = f.lifecycle.snapshot(awaiterIdentity).configurationEpoch;
  const callerCtx = f.lifecycle.context.awaitedView.ctx;
  assert.ok(f.lifecycle.context.awaitedView.entries.includes('include:dynamic-on'));

  // An ordinary config update reloads the same fiber and increments configurationEpoch; ctx.loader on callerCtx must renew cleanly.
  const updated = await f.apply({ 'cordis.yml': makeYaml(2) }, current.graph);
  current = updated.configuration;
  await f.lifecycle.settled();
  const renewedIdentity = f.identity(updated.activations.find(a => a.executable.instanceId === 'include:awaiter'));
  assert.ok(f.lifecycle.snapshot(renewedIdentity).configurationEpoch > firstEpoch);
  assert.equal(f.lifecycle.context.awaitedView.ctx, callerCtx);
  assert.throws(() => callerCtx.loader.resolve('include').subtree.root.unlink({}), /EntryGroup\.unlink/);
  assert.equal('exit' in callerCtx.loader.resolve('include').subtree, false);
  callerCtx.loader.remove('include:dynamic-on');
  callerCtx.loader.exit();
  await callerCtx.loader.flush();
  assert.deepEqual(ownerCalls.map(c => c.method), ['loading.edit.submit', 'loading.host.restart', 'loading.sources.synchronize']);
  assert.equal(ownerCalls[1].params.minimumRevision, 3);
});

test('fixed original Mgr helpers locate the actual source through loading objects without executing saved intent', {
  skip: !process.env.MAGI_LOADING_PREPARED_PROJECT && 'Requires the fixed original mgr-0.2.13 project',
}, async t => {
  const project = await realpath(process.env.MAGI_LOADING_PREPARED_PROJECT);
  const packageDirectory = join(project, 'node_modules/dsh-plugin-mgr');
  const baseline = JSON.parse(await readFile(new URL('../../../../../docs/plugin-validation/phase-1/source-sync-2026-11-18/result.json', import.meta.url), 'utf8'));
  const verifyOriginal = async () => {
    for (const [name, expected] of Object.entries(baseline.original.verifiedOriginalFiles)) {
      assert.equal(createHash('sha256').update(await readFile(join(packageDirectory, name))).digest('hex'), expected, name);
    }
  };
  await verifyOriginal();
  const { _internal } = await import(pathToFileURL(join(packageDirectory, 'dist/internal.js')).href);
  const f = await fixture(t, { observer: `export function apply(ctx) { ctx.provide('objectContext', { ctx }); }` });
  const source = '[{"id":"observer","name":"observer"}]';
  const applied = await f.apply({ 'cordis.yml': source, 'cordis.patch.yml': '[]\n' });
  const loader = f.tree.objects(f.lifecycle.context.objectContext.ctx);
  const ctx = { loader };
  assert.deepEqual(_internal.chooseIncludeLayer(ctx), { id: 'include', path: join(f.sourceDirectory, 'cordis.yml') });
  const { patchPath } = _internal.locateProfile(ctx);
  assert.equal(patchPath, join(f.sourceDirectory, 'cordis.patch.yml'));
  assert.deepEqual(await _internal.disableRows(patchPath, ['observer']), { ok: true, reason: null });
  assert.deepEqual(_internal.readUserPatchState(patchPath).disables, ['observer']);
  assert.equal(f.lifecycle.snapshot(f.identity(applied.activations[0])).state, 'running',
    'the original helper only saved candidate intent; no native admission or runtime change is implied');
  assert.deepEqual(await _internal.enableRows(patchPath, ['observer']), { ok: true, reason: null });
  assert.deepEqual(_internal.readUserPatchState(patchPath).disables, []);
  assert.deepEqual(_internal.readUserPatchState(patchPath).forced, ['observer']);
  assert.equal(await readFile(join(f.sourceDirectory, 'cordis.yml'), 'utf8'), source);
  await verifyOriginal();
});
