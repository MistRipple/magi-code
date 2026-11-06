// DSH SettingsFormScope 的视图投影。草稿由上游 SettingsFormModel 拥有，
// 持久值与版本只来自 daemon；多字段保存只走一次既有 settings.write CAS。
const clone = (value) => structuredClone(value);
const dangerous = new Set(['__proto__', 'constructor', 'prototype']);
function defaults(schema) {
  const declared = schema && Object.hasOwn(schema, 'default') ? clone(schema.default) : undefined;
  if (schema?.type !== 'object') return declared;
  const fields = Object.fromEntries(Object.entries(schema.properties ?? {}).flatMap(([key, value]) => {
    const result = defaults(value);
    return result === undefined ? [] : [[key, result]];
  }));
  return declared === undefined ? fields : merge(fields, declared);
}
function merge(base, user) {
  if (!base || typeof base !== 'object' || Array.isArray(base) || !user || typeof user !== 'object' || Array.isArray(user)) return clone(user);
  const result = clone(base);
  for (const [key, value] of Object.entries(user)) Object.defineProperty(result, key, { value: merge(base[key], value), enumerable: true, writable: true, configurable: true });
  return result;
}

export function createSettingsScope(request) {
  const listeners = new Set();
  let state = { status: 'loading', value: undefined, base: {}, user: {}, writable: false, revision: undefined };
  let seq = 0;
  let writes = Promise.resolve();
  const publish = (read, schema) => {
    if (state.revision !== undefined && read.version < state.revision) return;
    const base = defaults(schema) ?? state.base;
    const user = read.value ?? {};
    state = { status: 'ready', value: merge(base, user), base, user: clone(user), writable: true, revision: read.version };
    for (const listener of [...listeners]) listener();
  };
  const refresh = async () => {
    const current = ++seq;
    const [read, { schema }] = await Promise.all([request({ operation: 'settings.read' }), request({ operation: 'settings.describe' })]);
    if (current === seq && (state.revision === undefined || read.version >= state.revision)) publish(read, schema);
  };
  const applyMutation = async (ops, expectedRevision = state.revision) => {
    if (state.status !== 'ready' || !Array.isArray(ops) || ops.length > 128) return false;
    const value = clone(state.user);
    for (const operation of ops) {
      const { path } = operation;
      if (!Array.isArray(path) || !path.length || path.length > 16 || path.some((key) => typeof key !== 'string' || !key || dangerous.has(key)) || !['set', 'unset'].includes(operation.op)) throw new Error('设置字段路径或操作无效');
      let target = value;
      for (const key of path.slice(0, -1)) {
        if (!Object.hasOwn(target, key)) target[key] = {};
        target = target[key];
        if (!target || typeof target !== 'object' || Array.isArray(target)) throw new Error('设置字段父路径不是对象');
      }
      const key = path.at(-1);
      if (operation.op === 'unset') delete target[key];
      else target[key] = clone(operation.value);
    }
    try {
      const written = await request({ operation: 'settings.write', expectedVersion: expectedRevision, value });
      ++seq;
      publish(written);
      return true;
    } catch {
      await refresh().catch(() => {});
      return false;
    }
  };
  const mutate = (ops, expectedRevision) => {
    const copied = clone(ops);
    const work = writes.then(() => applyMutation(copied, expectedRevision));
    writes = work.catch(() => {});
    return work;
  };
  return {
    scope: {
      getSnapshot: () => state,
      subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener); },
      mutate,
      set: (field, value) => mutate([{ op: 'set', path: [field], value }]),
      unset: (field) => mutate([{ op: 'unset', path: [field] }]),
    },
    refresh,
  };
}
