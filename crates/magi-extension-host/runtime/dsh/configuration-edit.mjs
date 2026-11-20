// Source edits only. Nothing here changes Cordis entries or writes a file.
import { createHash, randomBytes } from 'node:crypto';
import { isDeepStrictEqual as deepEqual } from 'node:util';
import { Scalar, isMap, isSeq, parseDocument, visit } from 'yaml';
import { flatten, merge, PatchIndex, routeJournal } from './include-journal.generated.mjs';
import { compileConfiguration, loadingEntryId, parseConfigurationSource, serializeConfigurationSource } from './configuration.mjs';
import { groupEntries, orderEntries, positionEntry } from './configuration-order.mjs';
import { reconcileGroup } from './configuration-group.mjs';

function object(value) { return value !== null && typeof value === 'object' && !Array.isArray(value); }

// Removing only the last override would reveal an inherited value again.
// Clear each contributing definition of a field before routing the delta.
function clearRemovedFields(tree, pending) {
  const removed = new Map([...pending ?? []].filter(([, record]) => record.kind === 'upsert')
    .map(([id, record]) => [id, Object.keys(record.changes).filter(key => record.changes[key] === undefined)])
    .filter(([, keys]) => keys.length));
  if (!removed.size) return;
  const effective = flatten(tree.effective);
  const clear = options => {
    for (const key of removed.get(options.id) ?? []) delete options[key];
  };
  const walk = entries => {
    for (const options of entries) {
      const children = options.group && Array.isArray(options.config) ? options.config : [];
      clear(options);
      walk(children);
    }
  };
  walk(tree.data);
  for (const patch of tree.patches) {
    if (patch.insert) { walk(patch.insert); continue; }
    const row = effective.get(patch.id)?.options;
    if (!row || (patch.name && patch.name !== row.name)) continue;
    if (row.group && Array.isArray(patch.config)) walk(patch.config);
    clear(patch);
  }
}

// A later patch can override fields of a bundle-inserted row. Write those
// fields in the effective patch; Include's journal owns the remaining delta.
function routeInsertedOverrides(tree, pending) {
  const index = new PatchIndex(tree.patches);
  const effective = flatten(tree.effective);
  const remaining = new Map(pending);
  for (const [id, record] of pending) {
    const owner = index.entry(id);
    const row = effective.get(id);
    if (owner.type !== 'insert' || record.kind !== 'upsert' || record.created || !row
      || (record.parent !== undefined && record.parent !== row.parent)) continue;
    const changes = { ...record.changes };
    for (let i = tree.patches.length - 1; i > owner.index; i--) {
      const patch = tree.patches[i];
      if (patch.insert || patch.id !== id || (patch.name && patch.name !== row.options.name)) continue;
      for (const key of Object.keys(changes)) {
        if (!Object.hasOwn(patch, key)) continue;
        patch[key] = changes[key];
        delete changes[key];
      }
    }
    remaining.set(id, { ...record, changes });
  }
  return remaining;
}

/** Compile the same source ownership rules as Include, before domain admission. */
export async function prepareConfigurationEdit(input) {
  const { operation, ...preparation } = input;
  if (object(operation) && operation.kind === 'profilePatchConfig') {
    const allowed = ['kind', 'id', 'name', 'nextConfig', 'clearOverride'];
    if (Object.keys(operation).some(key => !allowed.includes(key))
      || typeof operation.id !== 'string' || !operation.id
      || typeof operation.name !== 'string' || !operation.name
      || typeof operation.clearOverride !== 'boolean') {
      throw new Error('Invalid profilePatchConfig edit');
    }
    const { plan, trees, inputSha256 } = await compileConfiguration(preparation);
    const patchSource = input.sources.find(source => source.name === 'cordis.patch.yml');
    if (!patchSource) throw new Error('profilePatchConfig requires captured cordis.patch.yml');
    const before = patchSource.content.trim() === '' ? '[]\n' : patchSource.content;
    const document = parseDocument(before, {
      customTags: [{ tag: 'tag:yaml.org,2002:js', resolve: value => value }],
    });
    if (document.errors[0] !== undefined) throw document.errors[0];
    if (!isSeq(document.contents)) throw new Error('Profile patch must be a YAML sequence');
    document.contents.flow = false;
    const lastIdx = document.contents.items.findLastIndex((item, index) =>
      isMap(item) && document.getIn([index, 'id']) === operation.id && !item.has('insert')
        && (!item.has('name') || document.getIn([index, 'name']) === operation.name),
    );
    if (operation.clearOverride) {
      for (let index = document.contents.items.length - 1; index >= 0; index--) {
        const row = document.contents.items[index];
        if (!isMap(row) || document.getIn([index, 'id']) !== operation.id || row.has('insert')) continue;
        row.delete('config');
        if (row.items.length === Number(row.has('id')) + Number(row.has('name'))) {
          document.delete(index);
        }
      }
    } else if (lastIdx < 0) {
      document.add(document.createNode({
        id: operation.id,
        name: operation.name,
        config: operation.nextConfig,
      }));
    } else {
      document.setIn([lastIdx, 'config'], document.createNode(operation.nextConfig));
    }
    visit(document, {
      Map(_key, node) {
        if (node.items.length !== 1 || typeof node.get('__jsExpr') !== 'string') return;
        const expression = new Scalar(node.get('__jsExpr'));
        expression.tag = 'tag:yaml.org,2002:js';
        return expression;
      },
    });
    const nextContent = String(document);
    parseConfigurationSource({ name: 'cordis.patch.yml', content: nextContent });
    const edits = nextContent === patchSource.content ? [] : [{
      name: 'cordis.patch.yml',
      expectedSha256: patchSource.sha256,
      content: nextContent,
    }];
    const nextSources = input.sources.map(source => source.name === 'cordis.patch.yml' && edits.length
      ? { name: 'cordis.patch.yml', content: nextContent, sha256: createHash('sha256').update(nextContent).digest('hex') }
      : source);
    const next = await compileConfiguration({
      ...preparation,
      sources: nextSources,
      previous: plan.graph,
      memoryRoot: trees.get('').data,
    });
    return { baseSourcesSha256: inputSha256, edits, plan: next.plan, result: operation.id };
  }
  if (!object(operation) || !['create', 'update', 'remove', 'reconcile'].includes(operation.kind)
    || typeof operation.treeId !== 'string') throw new Error('Invalid loading edit');
  const fields = {
    create: ['kind', 'treeId', 'options', 'parent', 'position', 'expectedEntries'],
    update: ['kind', 'treeId', 'id', 'options', 'parent', 'targetTreeId', 'position', 'expectedConfig', 'expectedOptions'],
    remove: ['kind', 'treeId', 'id', 'expectedConfig', 'expectedOptions'],
    reconcile: ['kind', 'treeId', 'entries', 'parent', 'expectedEntries'],
  };
  if (Object.keys(operation).some(key => !fields[operation.kind].includes(key))) throw new Error('Unknown loading edit field');
  const { plan, trees, inputSha256 } = await compileConfiguration(preparation);
  const documents = new Map(input.sources.map(source => [source.name, parseConfigurationSource(source).entries]));
  const changed = new Map();
  const journals = new Map();
  const layouts = new Map();
  const orders = new Map();
  const originalPatches = new Map([...trees].map(([id, tree]) => [id, structuredClone(tree.patches)]));
  const originalPatchSources = new Map([...trees].map(([id, tree]) => [id, [...tree.patchSources]]));
  const warn = (format, ...args) => { throw new Error(`Cannot persist loading edit: ${format} ${args.join(' ')}`); };
  const journal = tree => {
    if (!journals.has(tree.id)) journals.set(tree.id, new Map());
    return journals.get(tree.id);
  };
  const layout = (tree, id, parent, position) => {
    if (!layouts.has(tree.id)) layouts.set(tree.id, []);
    layouts.get(tree.id).push({ id, parent, position });
  };
  const order = (tree, parent, ids) => {
    if (!orders.has(tree.id)) orders.set(tree.id, new Map());
    orders.get(tree.id).set(parent, ids);
  };
  const write = (name, entries) => {
    if (changed.has(name) && !deepEqual(changed.get(name), entries)) throw new Error(`Conflicting edits of shared loading source: ${name}`);
    if (!deepEqual(documents.get(name), entries)) changed.set(name, structuredClone(entries));
  };
  const locate = (treeId, id) => {
    const tree = trees.get(treeId);
    const row = tree && flatten(tree.effective).get(id);
    if (!row) throw new Error(`Loading entry is not in the accepted source tree: ${treeId}:${id}`);
    return { tree, row };
  };
  const targetGroup = (treeId, parent) => {
    const tree = trees.get(treeId);
    if (!tree) throw new Error(`Loading tree is not in the captured source closure: ${treeId}`);
    if (parent !== null) {
      const row = flatten(tree.effective).get(parent);
      if (!row?.options.group || !Array.isArray(row.options.config)) throw new Error(`Loading parent is not a group: ${parent}`);
    }
    return tree;
  };
  const position = operation.position ?? Infinity;
  if (position !== Infinity && (!Number.isSafeInteger(position) || position < 0)) throw new Error('Invalid loading entry position');
  let result;
  let groupChange;
  if (operation.kind === 'create') {
    if (!object(operation.options) || typeof operation.options.name !== 'string' || !operation.options.name) throw new Error('Created entry requires a module name');
    const parent = operation.parent ?? null;
    const tree = targetGroup(operation.treeId, parent);
    if (Object.hasOwn(operation, 'expectedEntries') && !deepEqual(groupEntries(tree, parent), operation.expectedEntries)) {
      throw new Error('Loading group changed since the caller read it');
    }
    const used = flatten(tree.effective);
    const options = structuredClone(operation.options);
    if (options.id === undefined || options.id === null || options.id === '') {
      do { options.id = randomBytes(8).toString('hex'); } while (used.has(options.id));
    }
    if (typeof options.id !== 'string' || used.has(options.id)) throw new Error('Loading entry id already exists or is invalid');
    const { id, ...changes } = options;
    merge(journal(tree), id, { kind: 'upsert', created: true, parent, position, changes });
    layout(tree, id, parent, position);
    result = loadingEntryId(tree.id, id);
  } else if (operation.kind === 'reconcile') {
    const tree = targetGroup(operation.treeId, operation.parent ?? null);
    groupChange = reconcileGroup({ operation, tree, journal, order });
    result = groupChange.result;
  } else {
    if (typeof operation.id !== 'string' || !operation.id) throw new Error('Loading edit requires a local entry id');
    const { tree, row } = locate(operation.treeId, operation.id);
    if (Object.hasOwn(operation, 'expectedOptions') && !deepEqual(row.options, operation.expectedOptions)) {
      throw new Error('Loading entry changed since the caller read it');
    }
    if (Object.hasOwn(operation, 'expectedConfig') && !deepEqual(row.options.config ?? null, operation.expectedConfig)) {
      throw new Error('Loading configuration changed since the caller read its entry');
    }
    if (operation.kind === 'remove') merge(journal(tree), operation.id, { kind: 'remove' });
    else {
      if (!object(operation.options) || 'id' in operation.options || 'name' in operation.options) throw new Error('Loading update cannot change entry identity');
      const changes = Object.fromEntries(Object.entries(structuredClone(operation.options)).map(([key, value]) => [key, value ?? undefined]));
      const parent = operation.parent;
      const destination = targetGroup(operation.targetTreeId ?? tree.id, parent ?? null);
      if (destination.id !== tree.id) {
        for (let ancestor = destination; ancestor.parent; ancestor = trees.get(ancestor.parent.treeId)) {
          if (ancestor.parent.treeId === tree.id && ancestor.parent.id === operation.id) {
            throw new Error('A loading entry cannot move into its own subtree');
          }
        }
        const moving = [row.options.id, ...(row.options.group && Array.isArray(row.options.config) ? flatten(row.options.config).keys() : [])];
        if (moving.some(id => flatten(destination.effective).has(id))) throw new Error('Moved entry conflicts with the target tree');
        const next = { ...structuredClone(row.options), ...changes };
        delete next.id;
        merge(journal(tree), operation.id, { kind: 'remove' });
        merge(journal(destination), operation.id, { kind: 'upsert', created: true, parent: parent ?? null, position, changes: next });
        layout(destination, operation.id, parent ?? null, position);
        result = loadingEntryId(destination.id, operation.id);
      } else {
        merge(journal(tree), operation.id, { kind: 'upsert', created: false, parent, position, changes });
        if (parent !== undefined || operation.position !== undefined) layout(tree, operation.id, parent ?? row.parent, position);
        result = loadingEntryId(tree.id, operation.id);
      }
    }
  }
  // Child patch changes are themselves a config edit of the owning Include.
  // Process deepest trees first so every write reaches its actual source owner.
  const ordered = [...trees.values()].sort((a, b) => b.depth - a.depth);
  for (const tree of ordered) {
    const pending = journals.get(tree.id);
    if (!pending?.size && !layouts.has(tree.id) && !orders.has(tree.id)) continue;
    const before = structuredClone(tree.data);
    if (tree.id === operation.treeId && groupChange?.detach.size) routeJournal(groupChange.detach, tree.data, tree.patches, warn);
    clearRemovedFields(tree, pending);
    if (pending?.size) routeJournal(routeInsertedOverrides(tree, pending), tree.data, tree.patches, warn);
    for (const move of layouts.get(tree.id) ?? []) positionEntry(tree, move.id, move.parent, move.position);
    for (const [parent, ids] of orders.get(tree.id) ?? []) orderEntries(tree, parent, ids);
    const patchesChanged = !deepEqual(originalPatches.get(tree.id), tree.patches);
    if (groupChange && tree.id === operation.treeId
      && !deepEqual(groupEntries(tree, groupChange.parent), groupChange.wanted)) {
      throw new Error('Loading source composition does not match the requested group');
    }
    if (tree.source !== null && !deepEqual(before, tree.data)) write(tree.source, tree.data);
    if (!patchesChanged) continue;
    const names = new Set([...originalPatchSources.get(tree.id), ...tree.patchSources].filter(name => name !== null));
    for (const name of names) write(name, tree.patches.filter((_, index) => tree.patchSources[index] === name));
    const localPatches = patches => patches.filter((_, index) => tree.patchSources[index] === null);
    const previousLocal = originalPatches.get(tree.id).filter((_, index) => originalPatchSources.get(tree.id)[index] === null);
    if (tree.parent && !deepEqual(previousLocal, localPatches(tree.patches))) {
      const { tree: parent, row } = locate(tree.parent.treeId, tree.parent.id);
      const pendingConfig = journal(parent).get(row.options.id)?.changes?.config;
      const config = { ...structuredClone(pendingConfig ?? row.options.config), patches: localPatches(tree.patches) };
      merge(journal(parent), row.options.id, { kind: 'upsert', created: false, parent: undefined, position: Infinity, changes: { config } });
    }
  }
  const edits = input.sources.filter(source => changed.has(source.name)).map(source => {
    const entries = changed.get(source.name);
    const content = serializeConfigurationSource(source.name, entries);
    parseConfigurationSource({ name: source.name, content });
    return { name: source.name, expectedSha256: source.sha256, content };
  });
  const byName = new Map(edits.map(edit => [edit.name, edit.content]));
  const nextSources = input.sources.map(source => byName.has(source.name)
    ? { name: source.name, content: byName.get(source.name), sha256: createHash('sha256').update(byName.get(source.name)).digest('hex') } : source);
  const next = await compileConfiguration({ ...preparation, sources: nextSources, previous: plan.graph,
    memoryRoot: trees.get('').data });
  if (groupChange) {
    const tree = next.trees.get(operation.treeId);
    if (!tree || !deepEqual(groupEntries(tree, groupChange.parent), groupChange.wanted)) {
      throw new Error('Saved loading sources do not match the requested group');
    }
  }
  if (operation.kind === 'update') {
    const tree = next.trees.get(operation.targetTreeId ?? operation.treeId);
    const row = tree && flatten(tree.effective).get(operation.id);
    if (!row || Object.entries(operation.options).some(([key, value]) => !deepEqual(row.options[key], value ?? undefined))) {
      throw new Error('Saved loading sources do not match the requested entry update');
    }
  }
  return { baseSourcesSha256: inputSha256, edits, plan: next.plan, result: result ?? null };
}
