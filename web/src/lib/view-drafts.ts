/** 未提交的界面草稿，不是 daemon 配置或业务事实。凭据表单仅留在当前 Renderer 内存。 */
export type DraftRetention = 'session' | 'memory';
const memory = new Map<string, string>();
const prefix = 'magi:view-draft:v1:';

function read(key: string, retention: DraftRetention): string | null {
  if (retention === 'session') {
    try { return sessionStorage.getItem(prefix + key) ?? memory.get(key) ?? null; } catch { /* 存储受限时保留内存草稿 */ }
  }
  return memory.get(key) ?? null;
}

export function clearViewDraft(key: string): void {
  memory.delete(key);
  try { sessionStorage.removeItem(prefix + key); } catch { /* SSR / 存储受限 */ }
}

export function loadViewDraft<T>(key: string, baseline: string, fallback: T, retention: DraftRetention = 'session'): T {
  try {
    const saved = JSON.parse(read(key, retention) ?? 'null');
    if (saved?.baseline === baseline && Object.hasOwn(saved, 'value')) return saved.value as T;
  } catch { /* 损坏的草稿不能阻止编辑器打开 */ }
  clearViewDraft(key);
  return fallback;
}

export function saveViewDraft(key: string, baseline: string, value: unknown, retention: DraftRetention = 'session'): void {
  const encoded = JSON.stringify({ baseline, value });
  memory.set(key, encoded);
  // 仅作为存储受限时的兜底；防止长时间打开的窗口无限积累草稿。
  if (memory.size > 80) memory.delete(memory.keys().next().value!);
  if (retention === 'session') {
    try { sessionStorage.setItem(prefix + key, encoded); } catch { /* 内存副本仍可恢复 */ }
  }
}
