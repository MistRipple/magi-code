/**
 * 右栏「最近」：用户关闭过的文件与网页，供新标签页快速重新打开。
 *
 * 这只是视图恢复资料，不是业务事实：
 * - 只保存身份（路径、URL、标题），不保存文件内容、diff 或页面状态；
 * - 网页 URL 只保留 origin 和路径，去掉 query 与 hash，避免把令牌类参数写进浏览器存储；
 * - 重新打开时走与首次打开相同的入口（文件预览 / BrowserAuthority 创建），不复用旧的 Tab 实体；
 * - 与右栏布局一样按作用域隔离，Desktop 使用窗口级 sessionStorage。
 */

export interface RecentCodeItem {
  kind: 'code';
  key: string;
  label: string;
  filepath: string;
  displayPath?: string;
  sourceFolderId?: string;
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
  at: number;
}

export interface RecentWebItem {
  kind: 'web';
  key: string;
  label: string;
  url: string;
  workspaceId?: string;
  workspacePath?: string;
  sessionId?: string;
  at: number;
}

export type RecentPaneItem = RecentCodeItem | RecentWebItem;

const STORAGE_KEY = 'magi-right-pane-recent.v1';
export const MAX_RECENT_PER_SCOPE = 8;
const MAX_RECENT_SCOPES = 50;

type RecentMap = Record<string, RecentPaneItem[]>;

function storage(): Storage | null {
  if (typeof window === 'undefined') return null;
  const desktop = window.magiDesktop !== undefined
    || new URLSearchParams(window.location.search).get('desktopSurface') === 'app';
  try {
    return desktop ? window.sessionStorage : window.localStorage;
  } catch {
    return null;
  }
}

function isItem(value: unknown): value is RecentPaneItem {
  if (!value || typeof value !== 'object') return false;
  const item = value as Record<string, unknown>;
  if (typeof item.key !== 'string' || typeof item.label !== 'string' || typeof item.at !== 'number') return false;
  if (item.kind === 'code') return typeof item.filepath === 'string' && item.filepath.trim() !== '';
  if (item.kind === 'web') return typeof item.url === 'string' && /^https?:\/\//iu.test(item.url);
  return false;
}

function load(): RecentMap {
  const store = storage();
  if (!store) return {};
  try {
    const parsed = JSON.parse(store.getItem(STORAGE_KEY) ?? '{}') as Record<string, unknown>;
    const result: RecentMap = {};
    for (const [scope, items] of Object.entries(parsed)) {
      if (Array.isArray(items)) result[scope] = items.filter(isItem).slice(0, MAX_RECENT_PER_SCOPE);
    }
    return result;
  } catch {
    return {};
  }
}

let recent = $state<RecentMap>(load());

function commit(next: RecentMap): void {
  let kept = next;
  const scopes = Object.entries(next);
  if (scopes.length > MAX_RECENT_SCOPES) {
    scopes.sort((left, right) => (right[1][0]?.at ?? 0) - (left[1][0]?.at ?? 0));
    kept = Object.fromEntries(scopes.slice(0, MAX_RECENT_SCOPES));
  }
  recent = kept;
  try {
    storage()?.setItem(STORAGE_KEY, JSON.stringify(kept));
  } catch {
    // 配额或隐私模式：最近列表只是便利功能，失败时静默降级。
  }
}

/** 网页 URL 的可记录形态：仅 http(s)，去掉 query 与 hash。 */
export function recordableWebUrl(url: string | null | undefined): string | null {
  if (!url) return null;
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') return null;
    return `${parsed.origin}${parsed.pathname}`;
  } catch {
    return null;
  }
}

export function recentPaneItems(scopeKey: string): readonly RecentPaneItem[] {
  return recent[scopeKey] ?? [];
}

/** 记录一个刚被用户关闭的项；同一项只保留最新一次，最新的排在最前。 */
export function recordRecentPaneItem(scopeKey: string, item: RecentPaneItem): void {
  if (!scopeKey || !isItem(item)) return;
  const existing = (recent[scopeKey] ?? []).filter((entry) => entry.key !== item.key);
  commit({ ...recent, [scopeKey]: [item, ...existing].slice(0, MAX_RECENT_PER_SCOPE) });
}

/** 从列表移除（例如重新打开后）。 */
export function forgetRecentPaneItem(scopeKey: string, key: string): void {
  const items = recent[scopeKey];
  if (!items?.some((entry) => entry.key === key)) return;
  commit({ ...recent, [scopeKey]: items.filter((entry) => entry.key !== key) });
}

export function recentCodeKey(sourceFolderId: string | undefined, filepath: string): string {
  return sourceFolderId ? `code:${sourceFolderId}:${filepath}` : `code:${filepath}`;
}

export function recentWebKey(url: string): string {
  return `web:${url}`;
}
