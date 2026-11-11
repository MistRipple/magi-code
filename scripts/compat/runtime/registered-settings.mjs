import { AsyncLocalStorage } from 'node:async_hooks';
import { isDeepStrictEqual } from 'node:util';

export const REGISTERED_SETTINGS_RESOURCE = 'dsh-registered-settings';
const clone = value => structuredClone(value);
const merge = (base, value) => {
  if (!base || !value || typeof base !== 'object' || typeof value !== 'object' || Array.isArray(base) || Array.isArray(value)) return clone(value);
  const result = clone(base);
  for (const [key, child] of Object.entries(value)) Object.defineProperty(result, key, {
    value: Object.hasOwn(base, key) ? merge(base[key], child) : clone(child), enumerable: true, configurable: true, writable: true,
  });
  return result;
};
export const validSettingsNamespace = ns => typeof ns === 'string' && ns.length > 0 && ns.length <= 128 && !['__proto__', 'prototype', 'constructor'].includes(ns);

// 旧版 DSH 的命名空间表单独立于现代 Config。所有值沿 daemon 的插件
// 资源与 CAS 持久化；本对象只持有已确认的快照和 fiber 生命周期内的 schema。
export function createRegisteredSettings(api, namespaces = [], emit = () => {}) {
  const enabled = namespaces === true || Array.isArray(namespaces) && namespaces.length > 0;
  const entries = new Map(), calls = new AsyncLocalStorage();
  const notifications = new Set();
  let snapshot = { version: 0, value: null }, queue = Promise.resolve(), closed = false;
  const alive = () => { if (closed) throw new Error('设置服务已失效'); };
  const read = async () => {
    alive();
    const result = await api.resource.read(REGISTERED_SETTINGS_RESOURCE);
    alive();
    if (!Number.isSafeInteger(result?.version) || result.version < 0 || (result.value !== null && (typeof result.value !== 'object' || Array.isArray(result.value)))) throw new Error('daemon 设置资源响应无效');
    accept(result);
    return result;
  };
  const get = (entry, document = snapshot.value) => {
    alive();
    if (entries.get(entry.ns) !== entry) throw new Error(`设置 ${entry.ns} 注册已失效`);
    const value = entry.schema(merge(entry.base, document && Object.hasOwn(document, entry.ns) ? document[entry.ns] : {}));
    if (value?.then) throw new Error('DSH 设置 schema 不支持异步校验');
    return clone(value);
  };
  const accept = result => {
    if (result.version < snapshot.version) return;
    snapshot = clone(result);
    for (const entry of entries.values()) {
      const next = get(entry), previous = entry.value;
      if (isDeepStrictEqual(next, previous)) continue;
      entry.value = next;
      for (const watcher of entry.watchers) {
        const notification = watcher.tail.then(() => {
          if (watcher.active && !closed && entries.get(entry.ns) === entry) return watcher.callback(clone(next), clone(previous));
        }).catch(error => api.log?.(`设置 ${entry.ns} 观察失败：${error.message ?? error}`));
        watcher.tail = notification;
        notifications.add(notification); notification.finally(() => notifications.delete(notification));
      }
    }
  };
  const change = (ns, producer) => {
    alive();
    const entry = entries.get(ns);
    if (!entry) throw new Error(`未注册设置命名空间：${ns}`);
    const work = queue.then(async () => {
      const current = await read(); get(entry, current.value);
      const next = producer(clone(current.value?.[ns] ?? {}));
      const checked = entry.schema(merge(entry.base, next));
      if (checked?.then) throw new Error('DSH 设置 schema 不支持异步校验');
      const document = clone(current.value ?? {});
      Object.defineProperty(document, ns, { value: clone(next), enumerable: true, configurable: true, writable: true });
      const written = await api.resource.write(REGISTERED_SETTINGS_RESOURCE, current.version, document);
      alive();
      accept(written);
      emit(ns, get(entry));
    });
    queue = work.catch(() => {});
    calls.getStore()?.writes.push(work);
    // 旧版调用者可能没有 await；调用边界仍等待并传播写入失败。
    work.catch(() => {});
    return work;
  };
  return {
    has: ns => entries.has(ns),
    register(ctx, ns, schema, options = {}) {
      alive();
      if (!validSettingsNamespace(ns) || !enabled) throw new Error(`插件未声明注册设置资源或命名空间无效：${ns}`);
      if (typeof schema !== 'function') throw new Error(`设置 ${ns} 缺少同步 schema`);
      if (options.applies !== undefined && options.applies !== 'live') throw new Error(`设置 ${ns} 暂不支持 ${options.applies} 应用范围`);
      if (entries.has(ns)) throw new Error(`设置命名空间 ${ns} 已注册`);
      if (entries.size >= 128) throw new Error('设置命名空间超过 128 项');
      const entry = { ns, schema, base: clone(options.base ?? {}), watchers: new Set() };
      entries.set(ns, entry);
      try { entry.value = get(entry); } catch (error) { entries.delete(ns); throw error; }
      ctx.effect(() => () => { if (entries.get(ns) === entry) { entries.delete(ns); for (const watcher of entry.watchers) watcher.active = false; entry.watchers.clear(); } });
      return Object.freeze({
        get: () => get(entry),
        update: patch => { get(entry); return this.update(ns, patch); },
        replace: value => { get(entry); return this.replace(ns, value); },
        watch: callback => {
          get(entry);
          if (typeof callback !== 'function') throw new TypeError('设置观察器必须是函数');
          const watcher = { callback, active: true, tail: Promise.resolve() };
          entry.watchers.add(watcher);
          return () => { watcher.active = false; entry.watchers.delete(watcher); };
        },
      });
    },
    get(ns) { return entries.has(ns) ? get(entries.get(ns)) : undefined; },
    update(ns, patch) { return change(ns, value => merge(value, patch)); },
    replace(ns, value) { return change(ns, () => clone(value)); },
    mutate(ns, operations) { return change(ns, value => {
      const resolved = get(entries.get(ns));
      for (const operation of operations) {
        const { path } = operation;
        if (!Array.isArray(path) || !path.length || path.length > 16 || path.some(key => !validSettingsNamespace(key)) || !['set', 'unset', 'delete'].includes(operation.op)) throw new Error('设置字段路径或操作无效');
        let target = value, inherited = resolved;
        for (const key of path.slice(0, -1)) {
          if (!Object.hasOwn(target, key)) target[key] = clone(inherited?.[key] ?? {});
          target = target[key];
          inherited = inherited?.[key];
          if (!target || typeof target !== 'object') throw new Error('设置字段父路径无效');
        }
        const key = path.at(-1);
        if (operation.op === 'set') Object.defineProperty(target, key, { value: clone(operation.value), enumerable: true, configurable: true, writable: true });
        else if (Array.isArray(target)) {
          if (!/^(0|[1-9]\d*)$/.test(key) || Number(key) >= target.length) throw new Error('设置数组索引无效');
          target.splice(Number(key), 1);
        } else delete target[key];
      }
      return value;
    }); },
    async run(callback) {
      alive();
      if (calls.getStore()) return callback();
      if (enabled) await read();
      const call = { writes: [] };
      return calls.run(call, async () => {
        let result, failure;
        try { result = await callback(); } catch (error) { failure = error; }
        // 写入监听器可以再发起写操作，回执前收敛当前调用中完整的写入集合。
        for (let offset = 0; offset < call.writes.length;) {
          const batch = call.writes.slice(offset); offset += batch.length;
          for (const write of await Promise.allSettled(batch)) if (write.status === 'rejected') failure ??= write.reason;
        }
        if (failure) throw failure;
        return result;
      });
    },
    async dispose() { closed = true; await queue; await Promise.all([...notifications]); entries.clear(); },
  };
}
