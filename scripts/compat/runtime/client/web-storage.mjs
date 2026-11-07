// 隔离文档不能直接访问 Web Storage。仅代理本插件的界面缓存，绝不共享
// Magi 自己的登录态、配置、会话或其他插件的键。
export function createWebStorage(initial, persist, quota = 4 * 1024 * 1024) {
  const values = new Map(Object.entries(initial ?? {}).map(([key, value]) => [key, String(value)]));
  const mutate = (operation) => {
    const next = new Map(values);
    if (operation.kind === 'clear') next.clear();
    else if (operation.kind === 'remove') next.delete(operation.key);
    else next.set(operation.key, operation.value);
    if (JSON.stringify(Object.fromEntries(next)).length > quota) throw new DOMException('Plugin storage quota exceeded', 'QuotaExceededError');
    values.clear(); for (const [key, value] of next) values.set(key, value);
    persist(operation);
  };
  return {
    get length() { return values.size; },
    key(index) { return [...values.keys()][Number(index) >>> 0] ?? null; },
    getItem(key) { return values.get(String(key)) ?? null; },
    setItem(key, value) { mutate({ kind: 'set', key: String(key), value: String(value) }); },
    removeItem(key) { mutate({ kind: 'remove', key: String(key) }); },
    clear() { mutate({ kind: 'clear' }); },
  };
}
