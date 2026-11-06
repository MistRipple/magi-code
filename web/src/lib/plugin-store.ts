/**
 * 插件商店的列表模型：把已安装插件和插件中心目录合并成一份按 ID 去重的列表。
 * 只做展示投影，安装与生命周期事实仍在 daemon。
 */
import type {
  PluginCenterEntry,
  PluginInstalled,
  PluginManifest,
  PluginUpstreamEntry,
} from '../shared/app-server-protocol.generated';

export type StoreStatus = 'active' | 'installed' | 'available';
export type StoreFilter = 'all' | 'installed' | 'available' | 'updatable';

export interface StoreItem {
  id: string;
  name: string;
  description: string;
  /** 已安装时是已安装版本，否则是可安装版本。 */
  version: string;
  installed: PluginInstalled | null;
  catalog: PluginCenterEntry | null;
  /** 上游生态（DSH、pi）的 npm 发布包；安装时由 daemon 翻译。 */
  upstream: PluginUpstreamEntry | null;
  status: StoreStatus;
  /** 插件中心有比已安装版本更新的版本。 */
  updatable: boolean;
}

type Text = (manifest: PluginManifest, field: 'name' | 'description') => string;

/** 列表分组：有更新的排最前，其次是已安装，最后是尚未安装的。 */
export type StoreGroup = 'updatable' | 'installed' | 'available';
const GROUP_ORDER: Record<StoreGroup, number> = { updatable: 0, installed: 1, available: 2 };
const STATUS_ORDER: Record<StoreStatus, number> = { active: 0, installed: 1, available: 2 };

export function storeGroup(item: StoreItem): StoreGroup {
  if (item.updatable) return 'updatable';
  return item.installed ? 'installed' : 'available';
}

function statusOf(installed: PluginInstalled | null): StoreStatus {
  if (!installed) return 'available';
  return installed.active ? 'active' : 'installed';
}

export function buildStoreItems(
  installedPlugins: PluginInstalled[],
  catalog: PluginCenterEntry[],
  upstream: PluginUpstreamEntry[],
  text: Text,
): StoreItem[] {
  const items = new Map<string, StoreItem>();
  for (const installed of installedPlugins) {
    items.set(installed.manifest.id, {
      id: installed.manifest.id,
      name: text(installed.manifest, 'name'),
      description: text(installed.manifest, 'description'),
      version: installed.manifest.version,
      installed,
      catalog: null,
      upstream: null,
      status: statusOf(installed),
      updatable: false,
    });
  }
  for (const entry of catalog) {
    const existing = items.get(entry.id);
    if (existing) {
      existing.catalog = entry;
      existing.updatable = entry.state === 'updatable';
      continue;
    }
    items.set(entry.id, {
      id: entry.id,
      name: entry.name,
      description: entry.description,
      version: entry.version,
      installed: null,
      catalog: entry,
      upstream: null,
      status: 'available',
      updatable: false,
    });
  }
  // 已安装插件的来源就是 `生态:包名@版本`，据此归并，不依赖上游搜索结果是否已刷新：
  // 刚安装完时搜索结果还是旧的，若只看 installedPluginId 会短暂出现重复条目。
  const installedByUpstream = new Map<string, string>();
  for (const installed of installedPlugins) {
    if (installed.source.kind !== 'upstream') continue;
    const at = installed.source.location.lastIndexOf('@');
    if (at > 0) installedByUpstream.set(installed.source.location.slice(0, at), installed.manifest.id);
  }
  for (const entry of upstream) {
    // 已安装的上游包并入对应的已安装条目，其余用 生态+包名 作为临时身份（安装翻译后才有 Magi 插件 ID）。
    const installedId = entry.installedPluginId ?? installedByUpstream.get(`${entry.ecosystem}:${entry.package}`);
    const existing = installedId ? items.get(installedId) : undefined;
    if (existing) {
      existing.upstream = entry;
      existing.updatable = existing.updatable || entry.state === 'updatable';
      continue;
    }
    const id = `upstream:${entry.ecosystem}:${entry.package}`;
    items.set(id, {
      id,
      name: entry.name,
      description: entry.description,
      version: entry.version,
      installed: null,
      catalog: null,
      upstream: entry,
      status: 'available',
      updatable: false,
    });
  }
  return [...items.values()].sort(
    (left, right) => GROUP_ORDER[storeGroup(left)] - GROUP_ORDER[storeGroup(right)]
      || STATUS_ORDER[left.status] - STATUS_ORDER[right.status]
      || left.name.localeCompare(right.name),
  );
}

export function filterStoreItems(items: StoreItem[], filter: StoreFilter, query: string): StoreItem[] {
  const needle = query.trim().toLowerCase();
  return items.filter((item) => {
    if (filter === 'installed' && !item.installed) return false;
    if (filter === 'available' && item.installed) return false;
    if (filter === 'updatable' && !item.updatable) return false;
    // 上游条目的匹配由 registry 搜索完成（它按关键词与相关度匹配，不一定包含查询原文）。
    if (item.upstream && !item.installed) return true;
    return !needle
      || item.name.toLowerCase().includes(needle)
      || item.id.toLowerCase().includes(needle)
      || item.description.toLowerCase().includes(needle);
  });
}

export function paginate<T>(items: T[], page: number, pageSize: number): { rows: T[]; pages: number; page: number } {
  const pages = Math.max(1, Math.ceil(items.length / pageSize));
  const current = Math.min(Math.max(1, page), pages);
  return { rows: items.slice((current - 1) * pageSize, current * pageSize), pages, page: current };
}

/**
 * 插件在设置里的专属分类（用于从插件中心一键跳转）：只取插件自己声明的 settings 视图。
 * 没有该视图时返回 null；宿主不为插件生成设置表单。
 */
export function pluginSettingsTab(manifest: PluginManifest): string | null {
  const view = (manifest.contributions.views ?? []).find((entry) => entry.placements.includes('settings'));
  return view ? `plugin:${manifest.id}:${view.id}` : null;
}
