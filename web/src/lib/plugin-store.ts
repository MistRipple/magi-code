/**
 * 插件商店的列表模型：把已安装插件和插件中心目录合并成一份按 ID 去重的列表。
 * 只做展示投影，安装与生命周期事实仍在 daemon。
 */
import type {
  PluginCenterEntry,
  PluginInstalled,
  PluginManifest,
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
 * 插件在设置里的专属分类（用于从插件中心一键跳转）：取声明的设置视图或自动生成的设置表单。
 * 没有设置时返回 null。
 */
export function pluginSettingsTab(manifest: PluginManifest): string | null {
  const view = (manifest.contributions.views ?? []).find((entry) => entry.placements.includes('settings'));
  if (view) return `plugin:${manifest.id}:${view.id}`;
  const schema = manifest.settingsSchema as { properties?: Record<string, unknown> } | undefined;
  return schema?.properties && Object.keys(schema.properties).length > 0 ? `plugin:${manifest.id}:settings` : null;
}
