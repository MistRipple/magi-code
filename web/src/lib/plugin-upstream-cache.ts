import type { PluginUpstreamProjection } from '../shared/app-server-protocol.generated';

/**
 * 上游插件搜索结果的展示缓存。daemon 仍是唯一事实来源；这里只让重新进入插件页、窗口重新获得焦点时
 * 先显示上一次的结果，再静默刷新，避免每次都出现“搜索中”并让列表与徽标跳动。
 */
const projections = new Map<string, PluginUpstreamProjection>();

/** 已见过的上游包的发布者信息；跨重启保留，使已安装插件的详情徽标一开始就完整。 */
export interface UpstreamBadge { publisher: string; official: boolean }
const BADGES_KEY = 'magi.plugin-upstream-badges.v1';
let badges: Record<string, UpstreamBadge> | null = null;

function badgeKey(ecosystem: string, pkg: string): string { return `${ecosystem}:${pkg}`; }

function loadBadges(): Record<string, UpstreamBadge> {
  if (badges) return badges;
  badges = {};
  try {
    const raw = window.localStorage.getItem(BADGES_KEY);
    const parsed = raw ? JSON.parse(raw) : null;
    if (parsed && typeof parsed === 'object') badges = parsed as Record<string, UpstreamBadge>;
  } catch { /* 缓存不可用时按首次进入处理 */ }
  return badges;
}

function persistBadges(): void {
  try { window.localStorage.setItem(BADGES_KEY, JSON.stringify(badges ?? {})); } catch { /* 无法持久化时仍可使用内存缓存 */ }
}

export function cachedUpstreamProjection(query: string): PluginUpstreamProjection | null {
  return projections.get(query) ?? null;
}

/** 只缓存成功的结果；失败不能覆盖上一次可用的内容。 */
export function rememberUpstreamProjection(query: string, projection: PluginUpstreamProjection): void {
  if (projection.error) return;
  projections.set(query, projection);
  const known = loadBadges();
  let changed = false;
  for (const entry of projection.entries) {
    const key = badgeKey(entry.ecosystem, entry.package);
    const previous = known[key];
    if (previous?.publisher === entry.publisher && previous.official === entry.official) continue;
    known[key] = { publisher: entry.publisher, official: entry.official };
    changed = true;
  }
  if (changed) persistBadges();
}

export function cachedUpstreamBadge(ecosystem: string, pkg: string): UpstreamBadge | null {
  return loadBadges()[badgeKey(ecosystem, pkg)] ?? null;
}
