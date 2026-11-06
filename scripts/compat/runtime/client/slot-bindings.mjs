import { useSyncExternalStore } from 'react';

const hooks = new WeakMap();
// DSH 槽位注入的 SnapshotStore → use<Name>(selector)。只订阅上游 store，
// 不复制它的草稿、保存状态或配置事实；不同插件共用同一绑定规则。
export function bindSlotProps(descriptor, injected, locale) {
  const props = { ...injected, ...(descriptor.locale ? { t: locale.bind(descriptor.locale) } : {}) };
  for (const [name, store] of Object.entries(injected.hooks ?? {})) {
    if (!store || typeof store.getSnapshot !== 'function' || typeof store.subscribe !== 'function') throw new Error(`槽位 hook ${name} 不是 SnapshotStore`);
    let hook = hooks.get(store);
    if (!hook) {
      const subscribe = (listener) => store.subscribe(listener), snapshot = () => store.getSnapshot();
      hook = (selector = (value) => value) => selector(useSyncExternalStore(subscribe, snapshot, snapshot));
      hooks.set(store, hook);
    }
    props[`use${name.charAt(0).toUpperCase()}${name.slice(1)}`] = hook;
  }
  return props;
}
