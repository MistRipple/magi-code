import assert from 'node:assert/strict';
import test from 'node:test';
import { createHash } from 'node:crypto';
import { prepareConfiguration, parseConfigurationSource } from '../configuration.mjs';
import { prepareConfigurationEdit } from '../configuration-edit.mjs';

const captured = (name, content) => ({ name, content, sha256: createHash('sha256').update(content).digest('hex') });
const json = (name, entries) => captured(name, JSON.stringify(entries));
const input = sources => ({ projectDirectory: '/immutable/project', sourceDirectory: '/candidate/sources',
  sources, base: sources[0].name, overlays: [], packages: [] });
const apply = (preparation, proposal) => ({ ...preparation,
  sources: preparation.sources.map(source => {
    const edit = proposal.edits.find(edit => edit.name === source.name);
    return edit ? captured(edit.name, edit.content) : source;
  }), previous: proposal.plan.graph });
const entries = (proposal, name) => parseConfigurationSource(proposal.edits.find(edit => edit.name === name)).entries;

test('source create, config update and removal produce inert candidates with exact before hashes', async () => {
  const original = captured('cordis.yml', '- id: one\n  name: business\n  disabled: true\n  config: { expr: !!js "ctx.value" }\n');
  const preparation = input([original]);
  const created = await prepareConfigurationEdit({ ...preparation,
    operation: { kind: 'create', treeId: 'include', options: { name: 'next', disabled: true } } });
  assert.match(created.result, /^include:[a-f0-9]{16}$/);
  assert.equal(created.edits[0].expectedSha256, original.sha256);
  assert.deepEqual(created.plan.executables, []);
  assert.equal(created.plan.graph.root.entries[0].children.length, 2);
  const updated = await prepareConfigurationEdit({ ...apply(preparation, created),
    operation: { kind: 'update', treeId: 'include', id: 'one', options: { config: { expr: { __jsExpr: 'ctx.other' } } } } });
  assert.match(updated.edits[0].content, /!!js/);
  assert.deepEqual(updated.plan.graph.root.entries[0].children[0].options.config, { expr: { __jsExpr: 'ctx.other' } });
  const removed = await prepareConfigurationEdit({ ...apply(apply(preparation, created), updated),
    operation: { kind: 'remove', treeId: 'include', id: created.result.split(':')[1] } });
  assert.equal(removed.plan.graph.root.entries[0].children.length, 1);
  assert.equal(preparation.sources[0], original);
  assert.match(original.content, /ctx.value/);
});

test('upstream write-back routes values and inserted rows to their owning overlay documents', async () => {
  const preparation = { ...input([
    json('cordis.json', [{ id: 'base', name: 'business', disabled: true, config: { value: 'inherited' } }]),
    json('bundle.json', [{ insert: [{ id: 'extra', name: 'business', disabled: true, config: { value: 'bundle' } }] }]),
    json('user.json', [{ id: 'base', config: { value: 'user' } }]),
  ]), overlays: ['bundle.json', 'user.json'] };
  const updated = await prepareConfigurationEdit({ ...preparation,
    operation: { kind: 'update', treeId: 'include', id: 'base', options: { config: { value: 'edited' } } } });
  assert.deepEqual(updated.edits.map(edit => edit.name), ['user.json']);
  assert.deepEqual(entries(updated, 'user.json'), [{ id: 'base', config: { value: 'edited' } }]);
  const removed = await prepareConfigurationEdit({ ...preparation,
    operation: { kind: 'remove', treeId: 'include', id: 'extra' } });
  assert.deepEqual(removed.edits.map(edit => edit.name), ['bundle.json']);
  assert.deepEqual(entries(removed, 'bundle.json'), [{ insert: [] }]);
  assert.equal(removed.plan.graph.root.entries[0].children.length, 1);
});

test('nested Include patch edits bubble through the owning Include config into the real parent source', async () => {
  const preparation = input([
    json('cordis.json', [{ id: 'included', name: 'cordis:include', config: { path: 'nested.json',
      patches: [{ id: 'leaf', config: { value: 'override' } }] } }]),
    json('nested.json', [{ id: 'leaf', name: 'business', disabled: true, config: { value: 'original' } }]),
  ]);
  const proposal = await prepareConfigurationEdit({ ...preparation,
    operation: { kind: 'update', treeId: 'include:included', id: 'leaf', options: { config: { value: 'edited' } } } });
  assert.deepEqual(proposal.edits.map(edit => edit.name), ['cordis.json']);
  assert.deepEqual(entries(proposal, 'cordis.json')[0].config.patches, [{ id: 'leaf', config: { value: 'edited' } }]);
  assert.deepEqual(proposal.plan.graph.root.entries[0].children[0].children[0].options.config, { value: 'edited' });
  assert.equal(preparation.sources[1].content, JSON.stringify([{ id: 'leaf', name: 'business', disabled: true, config: { value: 'original' } }]));
});

test('children created in a patch-owned group remain in that group and its owning source', async () => {
  const preparation = { ...input([
    json('cordis.json', []),
    json('bundle.json', [{ insert: [{ id: 'group', name: 'cordis:group', group: true, config: [] }] }]),
  ]), overlays: ['bundle.json'] };
  const proposal = await prepareConfigurationEdit({ ...preparation, operation: {
    kind: 'create', treeId: 'include', parent: 'group', options: { id: 'child', name: 'business', disabled: true },
  } });
  assert.deepEqual(proposal.edits.map(edit => edit.name), ['bundle.json']);
  assert.equal(proposal.result, 'include:child');
  assert.equal(proposal.plan.graph.root.entries[0].children[0].children[0].options.id, 'child');
  assert.equal(entries(proposal, 'bundle.json')[0].insert[0].config[0].id, 'child');
});

test('moving an entry between real source trees prepares both documents without touching either file', async () => {
  const preparation = input([
    json('cordis.json', [{ id: 'business', name: 'business', disabled: true },
      { id: 'included', name: 'cordis:include', config: { path: 'nested.json' } }]),
    json('nested.json', []),
  ]);
  const proposal = await prepareConfigurationEdit({ ...preparation,
    operation: { kind: 'update', treeId: 'include', id: 'business', options: {}, targetTreeId: 'include:included', parent: null } });
  assert.deepEqual(proposal.edits.map(edit => edit.name), ['cordis.json', 'nested.json']);
  assert.equal(proposal.result, 'include:included:business');
  assert.equal(proposal.plan.graph.root.entries[0].children.length, 1);
  assert.equal(proposal.plan.graph.root.entries[0].children[0].children[0].options.id, 'business');
});

test('editing an anonymous source row persists the same identity used by the accepted graph', async () => {
  const preparation = input([json('cordis.json', [{ name: 'business', disabled: true, config: { value: 1 } }])]);
  const first = await prepareConfiguration(preparation);
  const id = first.graph.root.entries[0].children[0].options.id;
  const proposal = await prepareConfigurationEdit({ ...preparation, previous: first.graph,
    operation: { kind: 'update', treeId: 'include', id, options: { config: { value: 2 } } } });
  assert.equal(entries(proposal, 'cordis.json')[0].id, id);
  assert.equal(proposal.plan.graph.root.entries[0].children[0].options.id, id);
  assert.deepEqual(proposal.plan.graph.root.entries[0].children[0].options.config, { value: 2 });
});

test('invalid identity, parent, module and duplicate requests reject without an executable proposal', async () => {
  const preparation = input([json('cordis.json', [{ id: 'one', name: 'business', disabled: true }])]);
  for (const operation of [
    { kind: 'create', treeId: 'missing', options: { name: 'business', disabled: true } },
    { kind: 'create', treeId: 'include', options: { id: 'one', name: 'business', disabled: true } },
    { kind: 'create', treeId: 'include', options: { name: 'unadmitted' } },
    { kind: 'update', treeId: 'include', id: 'one', parent: 'one', options: {} },
    { kind: 'update', treeId: 'include', id: 'one', options: { name: 'replacement' } },
    { kind: 'remove', treeId: 'include', id: 'missing' },
    { kind: 'remove', treeId: 'include', id: 'one', expectedOptions: { id: 'one', name: 'other', disabled: true } },
    { kind: 'create', treeId: 'include', options: { name: 'business', disabled: true }, expectedEntries: [] },
  ]) await assert.rejects(prepareConfigurationEdit({ ...preparation, operation }));
});

test('positions refer to the composed group and persist both same-owner and cross-overlay moves', async () => {
  const row = id => ({ id, name: 'business', disabled: true, config: { value: id } });
  const preparation = { ...input([json('cordis.json', [row('a'), row('b')]),
    json('bundle.json', [{ insert: [row('c'), row('d')] }])]), overlays: ['bundle.json'] };
  const moved = await prepareConfigurationEdit({ ...preparation,
    operation: { kind: 'update', treeId: 'include', id: 'b', options: {}, parent: null, position: 0 } });
  assert.deepEqual(moved.plan.graph.root.entries[0].children.map(node => node.options.id), ['b', 'a', 'c', 'd']);
  const crossed = await prepareConfigurationEdit({ ...apply(preparation, moved),
    operation: { kind: 'update', treeId: 'include', id: 'c', options: {}, parent: null, position: 0 } });
  assert.deepEqual(crossed.plan.graph.root.entries[0].children.map(node => node.options.id), ['c', 'b', 'a', 'd']);
  assert.deepEqual(crossed.edits.map(edit => edit.name), ['cordis.json', 'bundle.json']);
  const appended = await prepareConfigurationEdit({ ...apply(apply(preparation, moved), crossed),
    operation: { kind: 'create', treeId: 'include', options: row('last') } });
  assert.deepEqual(appended.plan.graph.root.entries[0].children.map(node => node.options.id), ['c', 'b', 'a', 'd', 'last']);
});

test('a complete group proposal changes values, membership and order through one source plan', async () => {
  const row = id => ({ id, name: 'business', disabled: true, config: { value: id } });
  const preparation = { ...input([json('cordis.json', [row('a'), row('b')]),
    json('bundle.json', [{ insert: [row('c'), row('d')] }]), json('user.json', [{ id: 'a', config: { value: 'user' } }])]),
    overlays: ['bundle.json', 'user.json'] };
  const original = await prepareConfiguration(preparation);
  const proposal = await prepareConfigurationEdit({ ...preparation, previous: original.graph, operation: {
    kind: 'reconcile', treeId: 'include', expectedEntries: original.graph.root.entries[0].children.map(node => node.options),
    entries: [{ ...row('d'), config: { value: 'edited-d' } }, { ...row('a'), config: { value: 'edited-a' } }, row('c'),
      { name: 'business', disabled: true }],
  } });
  assert.deepEqual(proposal.plan.graph.root.entries[0].children.slice(0, 3).map(node => node.options.id), ['d', 'a', 'c']);
  assert.match(proposal.result[3], /^include:[a-f0-9]{16}$/);
  assert.deepEqual(entries(proposal, 'user.json'), [{ id: 'a', config: { value: 'edited-a' } }]);
  assert.equal(proposal.plan.graph.root.entries[0].children[0].options.config.value, 'edited-d');
  const unchanged = await prepareConfigurationEdit({ ...apply(preparation, proposal), operation: {
    kind: 'reconcile', treeId: 'include', entries: proposal.plan.graph.root.entries[0].children.map(node => node.options),
  } });
  assert.deepEqual(unchanged.edits, []);
  assert.deepEqual(unchanged.result, proposal.result);
});

test('group replacement follows the real parent-config override and removes obsolete member patches', async () => {
  const row = id => ({ id, name: 'business', disabled: true, config: { value: id } });
  const preparation = { ...input([json('cordis.json', [{ id: 'g', name: 'cordis:group', group: true, config: [row('base')] }]),
    json('user.json', [{ id: 'g', config: [row('a'), row('b')] }, { id: 'g', insert: [row('extra')] }])]), overlays: ['user.json'] };
  const proposal = await prepareConfigurationEdit({ ...preparation, operation: {
    kind: 'reconcile', treeId: 'include', parent: 'g', entries: [{ ...row('b'), config: { value: 'new-b' } }, row('a')],
  } });
  assert.deepEqual(proposal.edits.map(edit => edit.name), ['user.json']);
  assert.deepEqual(proposal.plan.graph.root.entries[0].children[0].children.map(node => node.options.id), ['b', 'a']);
  assert.equal(proposal.plan.graph.root.entries[0].children[0].children[0].options.config.value, 'new-b');
  assert.equal(entries(proposal, 'user.json')[1].insert.length, 0);
});

test('a group proposal moves a member out of a deleted ancestor without losing its identity', async () => {
  const row = { id: 'member', name: 'business', disabled: true, config: { value: 'kept' } };
  const preparation = input([json('cordis.json', [
    { id: 'old', name: 'cordis:group', group: true, config: [row] },
    { id: 'next', name: 'cordis:group', group: true, config: [] },
  ])]);
  const proposal = await prepareConfigurationEdit({ ...preparation, operation: {
    kind: 'reconcile', treeId: 'include', entries: [{ id: 'next', name: 'cordis:group', group: true, config: [row] }],
  } });
  assert.equal(proposal.plan.graph.root.entries[0].children.length, 1);
  assert.deepEqual(proposal.plan.graph.root.entries[0].children[0].children[0].options, row);
});

test('a rejected group proposal cannot emit partial edits or mutate captured inputs', async () => {
  const row = { id: 'one', name: 'business', disabled: true };
  const preparation = input([json('cordis.json', [{ id: 'g', name: 'cordis:group', group: true, config: [row] },
    { id: 'outside', name: 'business', disabled: true }])]);
  const original = structuredClone(preparation);
  for (const operation of [
    { kind: 'reconcile', treeId: 'include', entries: [row, row] },
    { kind: 'reconcile', treeId: 'include', parent: 'g', entries: [{ ...row, id: 'outside' }] },
    { kind: 'reconcile', treeId: 'include', entries: [{ id: 'new', name: 'unadmitted' }] },
    { kind: 'reconcile', treeId: 'include', expectedEntries: [], entries: [] },
    { kind: 'reconcile', treeId: 'include', id: 'g', entries: [] },
  ]) await assert.rejects(prepareConfigurationEdit({ ...preparation, operation }));
  assert.deepEqual(preparation, original);
});

test('a new or converted group can adopt existing members without duplicate source identities', async () => {
  const row = id => ({ id, name: 'business', disabled: true, config: { value: id } });
  const group = config => ({ id: 'g', name: 'cordis:group', group: true, config });
  for (const previous of [[row('a'), row('b')], [row('g'), row('a'), row('b')]]) {
    const preparation = { ...input([
      json('cordis.json', previous),
      json('bundle.json', [{ insert: [row('c')] }]),
      json('user.json', [{ id: 'a', config: { value: 'old-override' } }]),
    ]), overlays: ['bundle.json', 'user.json'] };
    const wanted = [group([row('c'), { ...row('a'), config: { value: 'new-value' } }]), row('b')];
    const proposal = await prepareConfigurationEdit({ ...preparation,
      operation: { kind: 'reconcile', treeId: 'include', entries: wanted } });
    assert.deepEqual(proposal.plan.graph.root.entries[0].children.map(node => node.options), wanted);
    assert.deepEqual(entries(proposal, 'user.json'), [], 'obsolete descendant overrides leave their real source');
    assert.deepEqual(entries(proposal, 'bundle.json'), [{ insert: [] }]);
    const next = apply(preparation, proposal);
    const repeated = await prepareConfigurationEdit({ ...next,
      operation: { kind: 'reconcile', treeId: 'include', entries: wanted } });
    assert.deepEqual(repeated.edits, []);
  }
});

test('complete overridden group replacement retires descendant overrides and emptied source layers', async () => {
  const row = id => ({ id, name: 'business', disabled: true, config: { value: id } });
  const group = config => ({ id: 'g', name: 'cordis:group', group: true, config });
  const preparation = { ...input([
    json('cordis.json', [group([row('base')])]),
    json('bundle.json', [{ id: 'g', config: [row('a'), row('b')] }]),
    json('user.json', [{ id: 'a', config: { value: 'old-override' } }]),
  ]), overlays: ['bundle.json', 'user.json'] };
  const wanted = [{ ...row('b'), config: { value: 'new-b' } }, { ...row('a'), config: { value: 'new-a' } }];
  for (const operation of [
    { kind: 'reconcile', treeId: 'include', parent: 'g', entries: wanted },
    { kind: 'reconcile', treeId: 'include', entries: [group(wanted)] },
  ]) {
    const proposal = await prepareConfigurationEdit({ ...preparation, operation });
    assert.deepEqual(proposal.plan.graph.root.entries[0].children[0].options, group(wanted));
    assert.deepEqual(entries(proposal, 'bundle.json'), [{ id: 'g', config: wanted }]);
    assert.deepEqual(entries(proposal, 'user.json'), []);
    assert.deepEqual(proposal.plan.graph.warnings, []);
  }
});

test('removing the last patched member also saves the empty override document', async () => {
  const row = { id: 'a', name: 'business', disabled: true };
  const preparation = { ...input([json('cordis.json', [row]),
    json('user.json', [{ id: 'a', config: { value: 'override' } }])]), overlays: ['user.json'] };
  const proposal = await prepareConfigurationEdit({ ...preparation,
    operation: { kind: 'reconcile', treeId: 'include', entries: [] } });
  assert.deepEqual(proposal.edits.map(edit => edit.name), ['cordis.json', 'user.json']);
  assert.deepEqual(entries(proposal, 'user.json'), []);
  assert.deepEqual(proposal.plan.graph.warnings, []);
});

test('whole-group source replacement preserves explicit nulls even when runtime equality treats them as absent', async () => {
  const row = { id: 'a', name: 'business', disabled: true };
  const preparation = input([json('cordis.json', [row])]);
  const wanted = [{ ...row, config: { explicit: null }, inject: null }];
  const proposal = await prepareConfigurationEdit({ ...preparation,
    operation: { kind: 'reconcile', treeId: 'include', entries: wanted } });
  assert.deepEqual(entries(proposal, 'cordis.json'), wanted);
  assert.deepEqual(proposal.plan.graph.root.entries[0].children[0].options, wanted[0]);
  const cleared = await prepareConfigurationEdit({ ...apply(preparation, proposal),
    operation: { kind: 'reconcile', treeId: 'include', entries: [row] } });
  assert.deepEqual(entries(cleared, 'cordis.json'), [row]);
  await assert.rejects(prepareConfigurationEdit({ ...preparation, operation: {
    kind: 'reconcile', treeId: 'include', entries: [], expectedEntries: [{ ...row, config: null }],
  } }), /changed since/);
});

test('clearing an effective field removes its inherited definitions instead of reviving an old value', async () => {
  const row = { id: 'a', name: 'business', disabled: true };
  const preparation = { ...input([
    json('cordis.json', [{ ...row, config: { value: 'base' }, inject: ['base'] }]),
    json('bundle.json', [{ id: 'a', config: { value: 'bundle' } }]),
    json('user.json', [{ id: 'a', config: { value: 'user' }, inject: ['user'] }]),
  ]), overlays: ['bundle.json', 'user.json'] };
  for (const operation of [
    { kind: 'update', treeId: 'include', id: 'a', options: { config: null, inject: null } },
    { kind: 'reconcile', treeId: 'include', entries: [row] },
  ]) {
    const proposal = await prepareConfigurationEdit({ ...preparation, operation });
    assert.deepEqual(proposal.plan.graph.root.entries[0].children[0].options, row);
    assert.deepEqual(entries(proposal, 'cordis.json'), [row]);
    assert.deepEqual(entries(proposal, 'bundle.json'), [{ id: 'a' }]);
    assert.deepEqual(entries(proposal, 'user.json'), [{ id: 'a' }]);
    assert.deepEqual(proposal.plan.graph.warnings, []);
  }
});

test('every order across three source owners round-trips the same complete group', async () => {
  const row = id => ({ id, name: 'business', disabled: true });
  const preparation = { ...input([json('cordis.json', [row('a'), row('b')]),
    json('first.json', [{ insert: [row('c')] }]), json('last.json', [{ insert: [row('d')] }])]), overlays: ['first.json', 'last.json'] };
  function* permutations(items) {
    if (!items.length) { yield []; return; }
    for (const [index, item] of items.entries()) {
      for (const rest of permutations(items.filter((_, i) => i !== index))) yield [item, ...rest];
    }
  }
  for (const ids of permutations(['a', 'b', 'c', 'd'])) {
    const proposal = await prepareConfigurationEdit({ ...preparation,
      operation: { kind: 'reconcile', treeId: 'include', entries: ids.map(row) } });
    assert.deepEqual(proposal.plan.graph.root.entries[0].children.map(node => node.options.id), ids);
  }
});

test('group indexing preserves source diagnostics in the final plan', async () => {
  const row = { id: 'one', name: 'business', disabled: true };
  const preparation = { ...input([json('cordis.json', [row]), json('user.json', [{ id: 'not-present', disabled: true }])]), overlays: ['user.json'] };
  const initial = await prepareConfiguration(preparation);
  const proposal = await prepareConfigurationEdit({ ...preparation,
    operation: { kind: 'reconcile', treeId: 'include', entries: [row] } });
  assert.equal(initial.graph.warnings.length, 1);
  assert.deepEqual(proposal.plan.graph.warnings, initial.graph.warnings);
  assert.deepEqual(proposal.edits, []);
});
