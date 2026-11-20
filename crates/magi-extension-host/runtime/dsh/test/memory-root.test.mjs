import assert from 'node:assert/strict';
import test from 'node:test';
import { createHash } from 'node:crypto';
import { prepareConfiguration } from '../configuration.mjs';
import { prepareConfigurationEdit } from '../configuration-edit.mjs';

const captured = (name, entries) => {
  const content = JSON.stringify(entries);
  return { name, content, sha256: createHash('sha256').update(content).digest('hex') };
};
const row = id => ({ id, name: 'business', disabled: true });
const input = entries => ({ projectDirectory: '/immutable/project', sourceDirectory: '/candidate/sources',
  sources: [captured('cordis.json', entries)], base: 'cordis.json', packages: [] });
const nextInput = (input, proposal) => ({ ...input, previous: proposal.plan.graph,
  sources: input.sources.map(source => {
    const edit = proposal.edits.find(edit => edit.name === source.name);
    return edit ? { ...source, content: edit.content, sha256: createHash('sha256').update(edit.content).digest('hex') } : source;
  }) });

test('root create, update, ordering and removal edit the real root and retain it on preparation', async () => {
  const source = input([row('from-file')]);
  const first = await prepareConfiguration(source);
  assert.equal(first.graph.version, 2);
  assert.deepEqual(first.graph.root.entries.map(node => node.options.id), ['include']);
  const created = await prepareConfigurationEdit({ ...source, previous: first.graph,
    operation: { kind: 'create', treeId: '', options: { name: 'business', disabled: true }, position: 0 } });
  assert.match(created.result, /^[a-f0-9]{16}$/);
  assert.deepEqual(created.edits, []);
  assert.equal(created.plan.sourcesSha256, first.sourcesSha256);
  assert.deepEqual(created.plan.graph.root.entries.map(node => node.options.id), [created.result, 'include']);
  const updated = await prepareConfigurationEdit({ ...nextInput(source, created), operation: {
    kind: 'update', treeId: '', id: created.result, options: { config: { explicit: null } }, position: 1,
  } });
  assert.equal(updated.result, created.result);
  assert.deepEqual(updated.edits, []);
  assert.deepEqual(updated.plan.graph.root.entries.map(node => node.options.id), ['include', created.result]);
  assert.deepEqual(updated.plan.graph.root.entries[1].options.config, { explicit: null });
  assert.deepEqual(await prepareConfiguration(nextInput(source, updated)), updated.plan);
  const removed = await prepareConfigurationEdit({ ...nextInput(source, updated),
    operation: { kind: 'remove', treeId: '', id: 'include' } });
  assert.deepEqual(removed.plan.graph.root.entries.map(node => node.options.id), [created.result]);
  assert.deepEqual(removed.edits, []);
  assert.deepEqual(await prepareConfiguration(nextInput(source, removed)), removed.plan);
});

test('root group replacement and clear do not rewrite a file or recreate the bootstrap include', async () => {
  const source = input([row('from-file')]);
  const first = await prepareConfiguration(source);
  const wanted = [{ id: 'group', name: 'cordis:group', group: true, config: [row('nested-root-member')] },
    first.graph.root.entries[0].options];
  const replaced = await prepareConfigurationEdit({ ...source, previous: first.graph,
    operation: { kind: 'reconcile', treeId: '', expectedEntries: first.graph.root.entries.map(node => node.options), entries: wanted } });
  assert.deepEqual(replaced.result, ['group', 'include']);
  assert.deepEqual(replaced.edits, []);
  assert.deepEqual(replaced.plan.graph.root.entries.map(node => node.options), wanted);
  const repeated = await prepareConfigurationEdit({ ...nextInput(source, replaced),
    operation: { kind: 'reconcile', treeId: '', entries: wanted } });
  assert.deepEqual(repeated.plan, replaced.plan);
  const cleared = await prepareConfigurationEdit({ ...nextInput(source, replaced),
    operation: { kind: 'reconcile', treeId: '', entries: [] } });
  assert.deepEqual(cleared.plan.graph.root.entries, []);
  assert.deepEqual(cleared.plan.executables, []);
  assert.deepEqual(cleared.edits, []);
  assert.deepEqual(await prepareConfiguration(nextInput(source, cleared)), cleared.plan);
});

test('moving between a file tree and the root changes identities and compiles one complete plan', async () => {
  const source = input([row('member')]);
  const moved = await prepareConfigurationEdit({ ...source,
    operation: { kind: 'update', treeId: 'include', id: 'member', options: {}, targetTreeId: '' } });
  assert.equal(moved.result, 'member');
  assert.deepEqual(moved.edits.map(edit => edit.name), ['cordis.json']);
  assert.deepEqual(moved.plan.graph.root.entries.map(node => node.options.id), ['include', 'member']);
  assert.deepEqual(moved.plan.graph.root.entries[0].children, []);
  const restored = await prepareConfigurationEdit({ ...nextInput(source, moved),
    operation: { kind: 'update', treeId: '', id: 'member', options: {}, targetTreeId: 'include' } });
  assert.equal(restored.result, 'include:member');
  assert.deepEqual(restored.plan.graph.root.entries.map(node => node.options.id), ['include']);
  assert.deepEqual(restored.plan.graph.root.entries[0].children[0].options, row('member'));
  await assert.rejects(prepareConfigurationEdit({ ...source,
    operation: { kind: 'update', treeId: '', id: 'include', options: {}, targetTreeId: 'include' } }), /own subtree/);
});

test('Include-local patches owned by the memory root save there while file layers retain their source', async () => {
  const source = { ...input([{ ...row('member'), config: { label: 'base' } }]), overlays: ['overlay.json'] };
  source.sources.push(captured('overlay.json', [{ id: 'member', config: { label: 'file-layer' } }]));
  const first = await prepareConfiguration({ ...source, memoryRoot: [{ id: 'include', name: 'cordis:include',
    config: { path: 'cordis.json', patches: [{ id: 'member', config: { label: 'memory-layer' } }] } }] });
  const proposal = await prepareConfigurationEdit({ ...source, previous: first.graph,
    operation: { kind: 'update', treeId: 'include', id: 'member', options: { config: { label: 'edited' } } } });
  assert.deepEqual(proposal.edits, []);
  assert.deepEqual(proposal.plan.graph.root.entries[0].options.config.patches, [{ id: 'member', config: { label: 'edited' } }]);
  assert.deepEqual(proposal.plan.graph.root.entries[0].children[0].options.config, { label: 'edited' });
  assert.deepEqual(await prepareConfiguration(nextInput(source, proposal)), proposal.plan);
  const fileOwned = await prepareConfigurationEdit({ ...source,
    operation: { kind: 'update', treeId: 'include', id: 'member', options: { config: { label: 'file-edited' } } } });
  assert.deepEqual(fileOwned.edits.map(edit => edit.name), ['overlay.json']);
  assert.deepEqual(fileOwned.plan.graph.root.entries[0].options, { id: 'include', name: 'cordis:include', config: { path: 'file:///candidate/sources/cordis.json' } });
});

test('invalid root edits reject without an executable proposal or changes to the source inputs', async () => {
  const source = input([row('from-file')]);
  const before = structuredClone(source);
  for (const operation of [
    { kind: 'create', treeId: '', options: { name: 'not-installed' } },
    { kind: 'create', treeId: '', options: { id: 'include', name: 'business', disabled: true } },
    { kind: 'reconcile', treeId: '', expectedEntries: [], entries: [] },
    { kind: 'update', treeId: '', id: 'include', options: { config: { path: '../outside.json' } } },
  ]) await assert.rejects(prepareConfigurationEdit({ ...source, operation }));
  assert.deepEqual(source, before);
  let nested = row('leaf');
  for (let depth = 0; depth < 70; depth++) nested = { id: `g${depth}`, name: 'cordis:group', group: true, config: [nested] };
  await assert.rejects(prepareConfiguration({ ...source, memoryRoot: [nested] }), /structural limits/);
  await assert.rejects(prepareConfiguration({ ...source, memoryRoot: [{ ...row('non-json'), config: { value: NaN } }] }), /non-JSON/);
});
