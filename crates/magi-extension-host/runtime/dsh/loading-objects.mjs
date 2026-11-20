// Context-owned management handles over the actual Cordis execution tree.
// Data is a projection; every persisted edit goes through configuration access.
import { randomBytes } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';

function snapshot(value) {
  const result = structuredClone(value);
  function freeze(item) {
    if (!item || typeof item !== 'object' || Object.isFrozen(item)) return;
    for (const child of Object.values(item)) freeze(child);
    Object.freeze(item);
  }
  freeze(result);
  return result;
}
const treeId = tree => tree.ctx.fiber.entry?.id ?? '';
const parentId = group => group === group.tree.root ? null : group.ctx.fiber.entry.options.id;

export function loadingObjects({ loader, context, access, revision, resolvePublicLoader }) {
  const state = { pending: new Set(), failure: undefined };
  let observedRevision, view, service;
  const publicLoader = typeof resolvePublicLoader === 'function' ? () => resolvePublicLoader() ?? service : () => service;
  const current = () => {
    access.assertCurrent();
    const next = revision();
    if (!view || next !== observedRevision) {
      observedRevision = next;
      view = loadingSnapshot({ loader, context, access, revision, state, publicLoader });
    }
    return view;
  };
  // The loader service is live; entry/group handles remain tied to the revision
  // they observed. A completed edit must not permanently stale ctx.loader.
  service = {};
  for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(current()))) {
    Object.defineProperty(service, key, descriptor.get
      ? { enumerable: true, get: () => current()[key] }
      : { enumerable: true, value: (...args) => current()[key](...args) });
  }
  return Object.freeze(service);
}

function loadingSnapshot({ loader, context, access, revision, state, publicLoader }) {
  const observedRevision = revision();
  const trees = new WeakMap(), groups = new WeakMap(), entries = new WeakMap(), handles = new WeakMap();
  const { pending } = state;
  const assertScope = () => access.assertCurrent();
  const assertSnapshot = () => {
    assertScope();
    if (revision() !== observedRevision) throw new Error('Loading handle revision is stale; resolve a new handle');
  };
  const track = task => {
    pending.add(task);
    void task.then(() => pending.delete(task), () => pending.delete(task));
    return task;
  };
  const submit = (operation, synchronize = true, refresh) => {
    assertSnapshot();
    return track(access.edit(operation, { expectedRevision: observedRevision }).then(async receipt => {
      if (refresh && receipt.revision === observedRevision) await refresh();
      else if (synchronize) await access.synchronize();
      return receipt.result;
    }));
  };
  const detached = task => { void task.catch(error => { state.failure ??= error; }); };
  const stop = (id, includeEntry = false) => track(access.stop({ id, includeEntry }, observedRevision));
  const synchronize = async refresh => {
    assertScope();
    await Promise.allSettled([...pending]);
    if (state.failure) {
      const observed = state.failure;
      state.failure = undefined;
      throw observed;
    }
    await access.synchronize({ refresh });
  };
  const readonlyContext = raw => context.extend({ baseUrl: raw.ctx.baseUrl });
  const currentTree = raw => {
    assertScope();
    if (raw.root !== loader.root && raw.ctx.fiber.entry?.subtree !== raw) throw new Error('Loading tree handle has ended');
  };
  const currentEntry = raw => {
    assertScope();
    currentTree(raw.parent.tree);
    if (raw.parent.tree.store[raw.options.id] !== raw) throw new Error('Loading entry handle has ended');
  };

  function entry(raw) {
    const fiberUid = raw.fiber?.uid;
    const cached = entries.get(raw);
    if (cached && cached.fiberUid === fiberUid) return cached.api;
    const assert = () => {
      assertSnapshot(); currentEntry(raw);
      if (raw.fiber?.uid !== fiberUid) throw new Error('Loading entry runtime generation has changed');
    };
    const options = snapshot(raw.options);
    const start = (refresh) => {
      assert();
      if (raw.disabled) {
        if (refresh) return Promise.resolve();
        throw new Error('Entry is disabled in the committed configuration');
      }
      if (raw.fiber?.uid != null) return Promise.resolve();
      return track(access.activate(raw, observedRevision));
    };
    const api = {
      get id() { assert(); return raw.id; },
      get options() { assert(); return options; },
      get disabled() { assert(); return raw.disabled; },
      get parent() { assert(); return group(raw.parent); },
      get loader() { assert(); return publicLoader(); },
      get subgroup() { assert(); return raw.subgroup && group(raw.subgroup); },
      get subtree() { assert(); return raw.subtree && tree(raw.subtree); },
      get ctx() { assert(); return readonlyContext(raw); },
      get context() { return this.ctx; },
      get moduleNamespace() { assert(); return raw.moduleNamespace; },
      get fiber() {
        assert();
        const fiber = raw.fiber;
        return fiber && Object.freeze({ uid: fiber.uid, state: fiber.state, entry: api });
      },
      get _initTask() { assert(); return raw._initTask; },
      evaluate(expression) { assert(); return raw.evaluate(expression); },
      getOuterStack() { assert(); return raw.getOuterStack(); },
      update(candidate, create = false, force = false) {
        assert();
        if (typeof create !== 'boolean' || typeof force !== 'boolean') throw new Error('Invalid entry update options');
        if (!candidate || typeof candidate !== 'object' || Array.isArray(candidate)) throw new Error('Entry update requires options');
        const next = create ? structuredClone(candidate) : structuredClone(options);
        if (!create) for (const [key, value] of Object.entries(structuredClone(candidate))) {
          if (value === null || value === undefined) delete next[key];
          else next[key] = value;
        }
        if (next.id !== options.id || typeof next.name !== 'string' || !next.name) throw new Error('Entry replacement must retain its id and declare a module');
        const members = structuredClone(raw.parent.data);
        const position = members.findIndex(value => value.id === options.id);
        if (position < 0) throw new Error('Entry is not in its parent group');
        members[position] = next;
        const refresh = force ? () => {
          assert();
          if (raw.disabled) return;
          return access.forceUpdate(raw, observedRevision);
        } : undefined;
        return submit({ kind: 'reconcile', treeId: treeId(raw.parent.tree), parent: parentId(raw.parent),
          expectedEntries: snapshot(raw.parent.data), entries: members }, true, refresh).then(() => undefined);
      },
      refresh() { return start(true); },
      init() { return start(false); },
    };
    Object.freeze(api);
    entries.set(raw, { fiberUid, api });
    handles.set(api, { raw, kind: 'entry' });
    return api;
  }

  function group(raw) {
    if (groups.has(raw)) return groups.get(raw);
    const assert = () => {
      assertSnapshot(); currentTree(raw.tree);
      if (raw !== raw.tree.root && raw.ctx.fiber.entry?.subgroup !== raw) throw new Error('Loading group handle has ended');
    };
    const data = snapshot(raw.data);
    const api = {
      get data() { assert(); return data; },
      get config() { assert(); return data; },
      get ctx() { assert(); return readonlyContext(raw); },
      get context() { return this.ctx; },
      get tree() { assert(); return raw.tree === loader ? publicLoader() : tree(raw.tree); },
      create(options) { assert(); return create(raw, options); },
      update(config) { assert(); return submit({ kind: 'reconcile', treeId: treeId(raw.tree), parent: parentId(raw),
        expectedEntries: data, entries: config }).then(() => undefined); },
      remove(id, isDispose = false) {
        assert();
        const child = raw.tree.store[id];
        if (!child || child.parent !== raw) return;
        if (isDispose) { detached(stop(child.id, true)); return; }
        detached(submit({ kind: 'remove', treeId: treeId(raw.tree), id, expectedOptions: snapshot(child.options) }, false));
      },
      unlink() { assert(); throw new Error('EntryGroup.unlink cannot mutate uncommitted group data directly; use group.remove or tree.update'); },
      stop() { assert(); detached(stop(raw.ctx.fiber.entry?.id ?? null)); },
    };
    Object.freeze(api);
    groups.set(raw, api);
    handles.set(api, { raw, kind: 'group' });
    return api;
  }

  function create(destination, options, position = Infinity) {
    const operation = { kind: 'create', treeId: treeId(destination.tree), parent: parentId(destination),
      expectedEntries: snapshot(destination.data), options };
    if (position !== Infinity) operation.position = position;
    return submit(operation);
  }

  function tree(raw) {
    if (trees.has(raw)) return trees.get(raw);
    const assert = () => { assertSnapshot(); currentTree(raw); };
    const api = {
      get root() { assert(); return group(raw.root); },
      get store() { assert(); return Object.freeze(Object.fromEntries(Object.entries(raw.store).map(([id, value]) => [id, entry(value)]))); },
      get ctx() { assert(); return readonlyContext(raw); },
      get context() { return this.ctx; },
      get enableLogs() { assert(); return raw.enableLogs; },
      get filename() { assert(); return raw.filename; },
      get config() { assert(); return snapshot(raw.config); },
      get envData() { assert(); return snapshot(raw.envData); },
      *entries() { assert(); for (const value of raw.entries()) { assert(); yield entry(value); } },
      resolve(id) { assert(); return entry(raw.resolve(id)); },
      resolveGroup(id) { assert(); return group(raw.resolveGroup(id)); },
      getTasks() { assert(); return [...pending, ...raw.getTasks()]; },
      async await() {
        await synchronize(false);
        currentTree(raw);
        // This is the SDK's lifecycle wait, distinct from source flush.
        await raw.await();
        assertScope();
      },
      ensureId(options) {
        assert();
        if (!options || typeof options !== 'object' || Array.isArray(options)) throw new Error('Id allocation requires options');
        if (!options.id) do { options.id = randomBytes(8).toString('hex'); } while (raw.store[options.id]);
        if (typeof options.id !== 'string') throw new Error('Entry id must be a string');
        return options.id;
      },
      create(options, parent = null, position = Infinity) { assert(); return create(raw.resolveGroup(parent), options, position); },
      update(id, options, parent, position) {
        assert();
        const target = raw.resolve(id), source = target.parent.tree;
        const operation = { kind: 'update', treeId: treeId(source), id: target.options.id,
          expectedOptions: snapshot(target.options), options };
        if (parent !== undefined) {
          const destination = raw.resolveGroup(parent);
          operation.parent = parentId(destination);
          operation.targetTreeId = treeId(destination.tree);
        }
        if (position !== undefined && position !== Infinity) operation.position = position;
        return submit(operation).then(() => undefined);
      },
      remove(id) {
        assert();
        const target = raw.resolve(id);
        detached(submit({ kind: 'remove', treeId: treeId(target.parent.tree), id: target.options.id,
          expectedOptions: snapshot(target.options) }, false));
      },
      commit(change) {
        assert();
        if (!change || typeof change.id !== 'string') throw new Error('Invalid loading commit');
        const destination = handles.get(change.group);
        if (destination?.kind !== 'group' || destination.raw.tree !== raw) throw new Error('Loading commit requires a group handle from this tree');
        if (change.from && change.from !== change.group) throw new Error('Use tree.update for an atomic entry move');
        if (!change.options) return this.remove(change.id);
        if (change.options.id !== change.id) throw new Error('Loading commit entry identity does not match its options');
        if (!change.legacy) { detached(create(destination.raw, change.options)); return; }
        const target = raw.store[change.id];
        if (!target || !isDeepStrictEqual(target.options, change.legacy)) throw new Error('Loading commit base is stale');
        detached(entry(target).update(change.options, true));
      },
      import(name, outerStack) { assert(); return raw.import(name, outerStack); },
      flush() { currentTree(raw); return synchronize(false); },
      refresh() { currentTree(raw); return synchronize(true); },
      locate(fiber = context.fiber) { assert(); return loader.locate(fiber); },
      unwrapExports(value) { assert(); return loader.unwrapExports(value); },
    };
    if (raw === loader) {
      api.exit = () => {
        assert();
        detached(track(access.restartHost(observedRevision)));
      };
    } else {
      api.stop = async () => {
        assert();
        // Await prior source edits. A changed revision requires a new handle.
        await synchronize(false);
        assert();
        await stop(raw.ctx.fiber.entry.id);
      };
    }
    Object.freeze(api);
    trees.set(raw, api);
    handles.set(api, { raw, kind: 'tree' });
    return api;
  }
  return tree(loader);
}
