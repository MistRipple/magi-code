// Layout edits of captured sources. Runtime entries and files remain untouched.
import { applyEntryPatches } from '@deepseek-ai/cordis-plugin-include';
import { flatten, PatchIndex } from './include-journal.generated.mjs';

function effective(tree) {
  const diagnostics = [];
  const entries = applyEntryPatches(tree.data, tree.patches, (format, ...args) => diagnostics.push({ format, args }));
  // The final source compiler returns these diagnostics in the accepted plan;
  // indexing a layout does not decide which source warnings are acceptable.
  return { entries, diagnostics };
}

function members(entries, parent) {
  if (parent === null) return entries;
  const group = flatten(entries).get(parent)?.options;
  if (!group?.group || !Array.isArray(group.config)) throw new Error(`Loading parent is not a group: ${parent}`);
  return group.config;
}

export function groupEntries(tree, parent) {
  return members(effective(tree).entries, parent);
}

/** Index the actual writable arrays behind rows in one composed snapshot. */
function sourceLocations(tree, entries) {
  const rows = flatten(entries), base = flatten(tree.data), index = new PatchIndex(tree.patches);
  const overrides = new Map(), configs = new WeakMap();
  for (const patch of tree.patches) {
    const group = rows.get(patch.id)?.options;
    if (!patch.insert && Object.hasOwn(patch, 'config') && (!patch.name || patch.name === group?.name)) overrides.set(patch.id, patch);
  }
  return id => {
    const row = rows.get(id);
    if (!row) throw new Error(`Cannot locate loading layout entry: ${id}`);
    const ancestors = [];
    let parent = row.parent;
    while (parent !== null) {
      const group = rows.get(parent);
      if (!group) throw new Error(`Cannot locate loading layout parent: ${parent}`);
      ancestors.unshift(group.options);
      parent = group.parent;
    }
    // Replacing an ancestor's config replaces its member arrays. Those arrays
    // belong to that overriding patch, rather than to the base document.
    for (const group of ancestors) {
      const patch = overrides.get(group.id);
      if (!Array.isArray(patch?.config)) continue;
      if (!configs.has(patch.config)) configs.set(patch.config, flatten(patch.config));
      const member = configs.get(patch.config).get(id);
      if (member) return member;
    }
    const owner = index.entry(id);
    const member = owner.type === 'insert' ? owner : base.get(id);
    if (!member) throw new Error(`Loading layout has no source owner: ${id}`);
    return member;
  };
}

// Keep a longest nondecreasing sequence of source owners. Other rows must move
// across an overlay boundary; this keeps the largest possible set with its owner.
function retainedOwners(values) {
  const tails = [], previous = [];
  values.forEach((value, index) => {
    let low = 0, high = tails.length;
    while (low < high) {
      const middle = (low + high) >>> 1;
      if (values[tails[middle]] <= value) low = middle + 1;
      else high = middle;
    }
    previous[index] = low ? tails[low - 1] : -1;
    tails[low] = index;
  });
  const keep = new Set();
  for (let index = tails.at(-1); index !== undefined && index >= 0; index = previous[index]) keep.add(index);
  return keep;
}

/** Reorder a composed group once, preserving source owners where expressible. */
export function orderEntries(tree, parent, ids) {
  const composed = effective(tree).entries;
  const current = members(composed, parent);
  const byId = new Map(current.map(options => [options.id, options]));
  if (current.length !== ids.length || new Set(ids).size !== ids.length || ids.some(id => !byId.has(id))) {
    throw new Error('Loading order does not match its group members');
  }
  if (current.every((options, index) => options.id === ids[index])) return;
  const locate = sourceLocations(tree, composed);
  const locations = new Map(current.map(options => [options.id, locate(options.id)]));
  const containers = [], owner = new Map();
  for (const options of current) {
    const list = locations.get(options.id).list;
    if (!owner.has(list)) { owner.set(list, containers.length); containers.push(list); }
    else if (containers.at(-1) !== list) throw new Error('Loading source owners do not form ordered segments');
  }
  for (const list of containers) {
    if (list.some(options => !byId.has(options.id))) throw new Error('Loading layout source contains unrelated entries');
  }
  const sourceOwners = ids.map(id => owner.get(locations.get(id).list));
  const keep = retainedOwners(sourceOwners);
  const targetOwners = [...sourceOwners];
  let next = sourceOwners[Math.max(...keep)] ?? 0;
  for (let position = ids.length - 1; position >= 0; position--) {
    if (keep.has(position)) next = sourceOwners[position];
    targetOwners[position] = next;
  }
  const outputs = containers.map(() => []);
  ids.forEach((id, position) => {
    const source = locations.get(id);
    const target = targetOwners[position];
    outputs[target].push(containers[target] === source.list ? source.options : structuredClone(byId.get(id)));
  });
  containers.forEach((list, index) => list.splice(0, list.length, ...outputs[index]));
  const actual = groupEntries(tree, parent).map(options => options.id);
  if (actual.length !== ids.length || actual.some((value, index) => value !== ids[index])) {
    throw new Error('Loading source layout cannot represent the requested order');
  }
}

/** A position uses the composed group, including rows inserted by overlays. */
export function positionEntry(tree, id, parent, position) {
  const entries = groupEntries(tree, parent);
  if (!entries.some(options => options.id === id)) throw new Error(`Loading layout entry is not in the target group: ${id}`);
  const ids = entries.filter(options => options.id !== id).map(options => options.id);
  ids.splice(Math.min(position, ids.length), 0, id);
  orderEntries(tree, parent, ids);
}
