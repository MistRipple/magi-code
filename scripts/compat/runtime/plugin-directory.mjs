import { dshBundle } from './plugin-source.mjs';
// 原生目录变更只负责唤醒；版本、启停和故障始终重新读取 daemon。
// 这里保留上一份已投递的观察投影，以产生状态迁移，不拥有可执行 Cordis fiber。
export function pluginLifecycle(plugin) {
  const runtime = plugin.runtime;
  const state = !plugin.active ? 4 : runtime?.state === 'running' ? 2
    : runtime?.state === 'failed' ? 3 : ['starting', 'restarting'].includes(runtime?.state) ? 1
      : runtime?.state === 'stopped' ? 4 : 0;
  const reason = state === 3 ? runtime.reason ?? '插件启动失败，daemon 未提供原因' : undefined;
  return Object.freeze({ state, ...(reason === undefined ? {} : { _error: reason }),
    async await() { if (reason !== undefined) throw new Error(reason); } });
}

export function createPluginDirectory(api, diagnostic) {
  const listeners = new Set();
  let records = new Map(), closed = false, pending = false, refreshing, unsubscribe, epoch = 0;
  const project = (plugin) => {
    const bundle = dshBundle(plugin);
    if (!bundle) return;
    const lifecycle = pluginLifecycle(plugin);
    const value = Object.freeze({
      entry: Object.freeze({ options: Object.freeze({ id: plugin.manifest.id, name: bundle.name }) }),
      ...lifecycle,
    });
    return { id: plugin.manifest.id, value, signature: JSON.stringify([plugin.digest, plugin.runtime?.generation, lifecycle.state, plugin.runtime?.reason]) };
  };
  const deliver = async (listener, value, previous) => {
    if (closed || !listeners.has(listener)) return;
    try { await listener(value, previous); }
    catch (error) { diagnostic(`插件状态观察失败: ${error?.message ?? error}`); }
  };
  const refresh = () => {
    if (closed || !listeners.size) return Promise.resolve();
    pending = true;
    if (refreshing) return refreshing;
    refreshing = (async () => {
      while (pending && !closed) {
        pending = false;
        const readingEpoch = epoch;
        const list = await api.plugins.list();
        if (closed) return;
        if (readingEpoch !== epoch || !listeners.size) continue;
        const next = new Map(list.plugins.map(project).filter(Boolean).map(record => [record.id, record]));
        const changes = [...next.values()].filter(record => record.signature !== records.get(record.id)?.signature)
          .map(record => [record.value, records.get(record.id)?.value.state]);
        for (const [id, record] of records) if (!next.has(id)) {
          changes.push([Object.freeze({ ...record.value, state: 4, await: async () => {} }), record.value.state]);
        }
        records = next;
        for (const [value, previous] of changes) for (const listener of [...listeners]) await deliver(listener, value, previous);
      }
    })().finally(() => { refreshing = undefined; });
    return refreshing;
  };
  return {
    subscribe(listener) {
      if (closed) throw new Error('插件目录观察已失效');
      if (!unsubscribe) unsubscribe = api.plugins.onChanged(() => refresh().catch(error => diagnostic(`插件目录读取失败: ${error.message}`)));
      listeners.add(listener);
      for (const { value } of records.values()) void deliver(listener, value);
      void refresh().catch(error => diagnostic(`插件目录读取失败: ${error.message}`));
      return () => {
        listeners.delete(listener);
        if (!listeners.size) { epoch++; unsubscribe?.(); unsubscribe = undefined; records.clear(); }
      };
    },
    refresh,
    dispose() { closed = true; unsubscribe?.(); listeners.clear(); records.clear(); },
  };
}
