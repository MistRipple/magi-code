// One complete group proposal, compiled before source admission or execution.
import { randomBytes } from 'node:crypto';
import { isDeepStrictEqual as deepEqual } from 'node:util';
import { flatten, merge } from './include-journal.generated.mjs';
import { groupEntries } from './configuration-order.mjs';
import { loadingEntryId } from './configuration.mjs';

const isGroup = options => options.group && Array.isArray(options.config);

// Whole-array replacement preserves source values, including explicit nulls.
// Loader's runtime diff deliberately treats null and absence as equivalent.
function diffOptions(previous, next) {
  return Object.fromEntries([...new Set([...Object.keys(previous), ...Object.keys(next)])]
    .filter(key => key !== 'id' && !deepEqual(previous[key], next[key]))
    .map(key => [key, next[key]]));
}

export function reconcileGroup({ operation, tree, journal, order }) {
  const parent = operation.parent ?? null;
  const current = groupEntries(tree, parent);
  if (!Array.isArray(operation.entries)) throw new Error('Loading group update requires an entry array');
  if (Object.hasOwn(operation, 'expectedEntries') && !deepEqual(operation.expectedEntries, current)) {
    throw new Error('Loading group changed since the caller read it');
  }
  const wanted = structuredClone(operation.entries);
  const all = flatten(tree.effective);
  const selected = flatten(current, parent);
  const used = new Set(all.keys()), nextIds = new Set();
  let nodes = 0;
  function assign(entries, depth = 0) {
    if (depth > 64) throw new Error('Loading group is too deep');
    for (const options of entries) {
      if (++nodes > 4096 || !options || typeof options !== 'object' || Array.isArray(options)
        || typeof options.name !== 'string' || !options.name) throw new Error('Invalid loading group entry');
      if (options.id === undefined || options.id === null || options.id === '') {
        do { options.id = randomBytes(8).toString('hex'); } while (used.has(options.id));
      }
      if (typeof options.id !== 'string' || !options.id || nextIds.has(options.id)
        || (all.has(options.id) && !selected.has(options.id))) throw new Error('Loading group entry identity conflicts with its tree');
      used.add(options.id);
      nextIds.add(options.id);
      if (options.group) {
        if (!Array.isArray(options.config)) throw new Error('Loading group requires an entry array');
        assign(options.config, depth + 1);
      }
    }
  }
  assign(wanted);
  const removed = new Set([...selected.keys()].filter(id => !nextIds.has(id)));
  // A new group owns its complete child array. Existing members moved into
  // that array must leave their previous source before the group is written;
  // otherwise the same stable id is present both inside and outside it.
  const detached = new Set();
  function embedded(entries, carried = false) {
    for (const options of entries) {
      if (carried && selected.has(options.id)) detached.add(options.id);
      if (isGroup(options)) embedded(options.config,
        carried || !isGroup(selected.get(options.id)?.options ?? {}));
    }
  }
  embedded(wanted);
  const detach = new Map([...detached].filter(id => !detached.has(selected.get(id).parent))
    .map(id => [id, { kind: 'remove' }]));
  for (const [id, row] of selected) {
    if (removed.has(id) && !removed.has(row.parent)) merge(journal(tree), id, { kind: 'remove' });
  }
  // Patches that target explicitly removed entries cannot contribute to this
  // replacement. Keep unrelated entries and their original source ownership.
  const retained = tree.patches.map((patch, index) => ({ patch, source: tree.patchSources[index] }))
    .filter(({ patch }) => !removed.has(patch.id) && !detached.has(patch.id));
  tree.patches = retained.map(value => value.patch);
  tree.patchSources = retained.map(value => value.source);
  const replaceOverride = (id, name, entries) => {
    const override = tree.patches.findLast(patch => !patch.insert && patch.id === id
      && Object.hasOwn(patch, 'config') && (!patch.name || patch.name === name));
    if (!override) return false;
    override.config = structuredClone(entries);
    const descendants = new Set(flatten(all.get(id)?.options.config ?? []).keys());
    // The replacement config now contains the complete effective subtree.
    // Old descendant overrides cannot keep modifying that newly owned array.
    const retained = tree.patches.map((patch, index) => ({ patch, source: tree.patchSources[index] }))
      .filter(({ patch }) => !descendants.has(patch.id));
    tree.patches = retained.map(value => value.patch);
    tree.patchSources = retained.map(value => value.source);
    for (const patch of tree.patches) if (patch.id === id && patch.insert) patch.insert = [];
    return true;
  };
  if (parent !== null && replaceOverride(parent, all.get(parent).options.name, wanted)) {
    merge(journal(tree), parent, { kind: 'upsert', created: false, parent: undefined, position: Infinity, changes: {} });
    return { parent, wanted, detach, result: wanted.map(options => loadingEntryId(tree.id, options.id)) };
  }

  const underRemoved = row => {
    let parent = row.parent;
    while (parent !== null) {
      if (removed.has(parent) || detached.has(parent)) return true;
      parent = all.get(parent)?.parent ?? null;
    }
    return false;
  };
  function update(entries, parent) {
    entries.forEach((options, position) => {
      const previous = selected.get(options.id);
      const relocated = !previous || detached.has(options.id) || previous.parent !== parent || underRemoved(previous);
      const changes = previous && !relocated ? diffOptions(previous.options, options)
        : Object.fromEntries(Object.entries(options).filter(([key]) => key !== 'id'));
      const recurse = previous && !relocated && isGroup(previous.options) && isGroup(options);
      if (previous && previous.options.name !== options.name) {
        for (const patch of tree.patches) {
          if (!patch.insert && patch.id === options.id && patch.name === previous.options.name) patch.name = options.name;
        }
      }
      if (recurse) {
        // An explicit parent-config override owns this whole member list.
        delete changes.config;
        if (!replaceOverride(options.id, options.name, options.config)) update(options.config, options.id);
      }
      if (relocated || Object.keys(changes).length) merge(journal(tree), options.id,
        { kind: 'upsert', created: !previous, parent: relocated ? parent : undefined, position, changes });
    });
    order(tree, parent, entries.map(options => options.id));
  }
  update(wanted, parent);
  return { parent, wanted, detach, result: wanted.map(options => loadingEntryId(tree.id, options.id)) };
}
