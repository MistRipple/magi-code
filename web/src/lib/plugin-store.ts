/**
 * 插件商店的列表模型：把已安装插件和插件中心目录合并成一份按 ID 去重的列表。
 * 只做展示投影，安装与生命周期事实仍在 daemon。
 */
import type {
  PluginCenterEntry,
  PluginInstalled,
  PluginManifest,
  PluginPermission,
  PluginUpstreamEntry,
  PluginUpstreamRequest,
} from '../shared/app-server-protocol.generated';

/** 安装预览与已安装详情共用同一权限说明，语言由调用方当前的 i18n 提供。 */
export function permissionText(permission: PluginPermission, t: (key: string, vars?: Record<string, string | number>) => string): string {
  if (permission.kind === 'webProviders') return t('settings.plugins.permission.webProviders');
  if (permission.kind === 'client') return t('settings.plugins.permission.client');
  if (permission.kind === 'attachments') return t('settings.plugins.permission.attachments');
  if (permission.kind === 'sessionWrite') return permission.targets.map(target => t(target === 'wake' ? 'settings.plugins.permission.sessionWrite.wake' : 'settings.plugins.permission.sessionWrite.context')).join('；');
  if (permission.kind === 'sessionRead') return permission.targets.map(target => t(target === 'render' ? 'settings.plugins.permission.sessionRender' : 'settings.plugins.permission.sessionRead')).join('；');
  if (permission.kind === 'composer') return permission.targets.map(target => t(target === 'text' ? 'settings.plugins.permission.composerText' : 'settings.plugins.permission.composer')).join('；');
  if (permission.kind === 'extension') return t('settings.plugins.permission.extensionFullTrust');
  if (permission.kind === 'pluginManagement') return [
    t(permission.targets.includes('authorize') ? 'settings.plugins.permission.pluginAuthorization' : 'settings.plugins.permission.pluginManagement'),
    ...(permission.targets.includes('compatibility') ? [t('settings.plugins.permission.pluginCompatibility')] : []),
    ...(permission.targets.includes('background') ? [t('settings.plugins.permission.pluginBackground')] : []),
  ].join('；');
  if (permission.kind === 'contextRewrite') return t('settings.plugins.permission.contextRewrite');
  if (permission.kind === 'modelSources') return t('settings.plugins.permission.modelSources', { sources: permission.targets.join(', ') });
  return `${permission.kind}${permission.targets.length ? `: ${permission.targets.join(', ')}` : ''}`;
}

/** 只解释已声明的贡献与权限；安装预览、在线更新及激活前共用，不增加授权状态。 */
export function pluginImpactNotices(manifest: PluginManifest, t: (key: string, vars?: Record<string, string | number>) => string) {
  const notices: { id: string; text: string }[] = [];
  const placements = new Set((manifest.contributions.views ?? []).flatMap(view => view.placements));
  if (placements.size) notices.push({ id: 'ui', text: t('settings.plugins.impact.ui', {
    locations: (['settings', 'main', 'rightPane', 'home', 'homeCard', 'composerLeft', 'chatAssistant'] as const)
      .filter(placement => placements.has(placement)).map(placement => t(`settings.plugins.impact.${placement}`)).join(' / '),
  }) });
  const hooks = manifest.extension?.hooks ?? [];
  if (hooks.some(hook => hook.name === 'prompt.contribute')) notices.push({ id: 'global-prompt', text: t('settings.plugins.globalPromptNotice') });
  if (hooks.some(hook => hook.mode === 'guard')) notices.push({ id: 'guard', text: t('settings.plugins.impact.guard') });
  if (manifest.permissions.some(permission => permission.kind === 'contextRewrite')) notices.push({ id: 'context', text: t('settings.plugins.impact.context') });
  return notices;
}

/** 只决定显示授权入口还是启用开关；真正准入仍由 daemon 按摘要检查。 */
export function pluginNeedsAuthorization(plugin: PluginInstalled): boolean {
  return plugin.manifest.permissions.some(permission => !plugin.grants.some(grant =>
    grant.kind === permission.kind && permission.targets.every(target => grant.targets.includes(target))));
}

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
  /** daemon 检测到运行组件更新，或在线来源有新版本。 */
  updatable: boolean;
  /** registry 返回的相关度顺序；只用于当前搜索投影。 */
  searchOrder?: number;
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
      updatable: installed.translationRuntime?.state === 'updateAvailable' && installed.source.kind === 'upstream',
    });
  }
  for (const entry of catalog) {
    const existing = items.get(entry.id);
    if (existing) {
      existing.catalog = entry;
      existing.updatable ||= entry.state === 'updatable';
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
    const source = installedUpstreamRequest(installed);
    if (source) installedByUpstream.set(`${source.ecosystem}:${source.package}`, installed.manifest.id);
  }
  for (const [searchOrder, entry] of upstream.entries()) {
    // 已安装的上游包并入对应的已安装条目，其余用 生态+包名 作为临时身份（安装翻译后才有 Magi 插件 ID）。
    const installedId = entry.installedPluginId ?? installedByUpstream.get(`${entry.ecosystem}:${entry.package}`);
    const existing = installedId ? items.get(installedId) : undefined;
    if (existing) {
      // 搜索可以命中同名仓库原包，但 npm 元数据不能改绑其在线更新来源。
      if (existing.installed?.source.github) { existing.searchOrder = searchOrder; continue; }
      existing.upstream = entry;
      existing.searchOrder = searchOrder;
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
      searchOrder,
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

/** 组件更新固定当前原包，沿原来源在线重新准备；GitHub 不被同名 npm 包替换。 */
export function installedUpstreamRequest(installed: PluginInstalled): PluginUpstreamRequest | null {
  if (installed.source.kind !== 'upstream') return null;
  const location = installed.source.location;
  const colon = location.indexOf(':'), at = location.lastIndexOf('@');
  const ecosystem = location.slice(0, colon);
  if (colon < 1 || at <= colon + 1 || !['dsh', 'pi'].includes(ecosystem)) return null;
  return {
    ecosystem: ecosystem as PluginUpstreamRequest['ecosystem'],
    package: location.slice(colon + 1, at), version: location.slice(at + 1),
    ...(installed.source.github ? { github: installed.source.github } : {}),
  };
}

export function filterStoreItems(items: StoreItem[], filter: StoreFilter, query: string): StoreItem[] {
  const needle = query.trim().toLowerCase();
  const matches = items.filter((item) => {
    if (filter === 'installed' && !item.installed) return false;
    if (filter === 'available' && item.installed) return false;
    if (filter === 'updatable' && !item.updatable) return false;
    // 上游条目的匹配由 registry 搜索完成（它按关键词与相关度匹配，不一定包含查询原文）。
    if (item.upstream && !item.installed) return true;
    return !needle
      || item.name.toLowerCase().includes(needle)
      || item.id.toLowerCase().includes(needle)
      || item.installed?.source.location.toLowerCase().includes(needle)
      || item.description.toLowerCase().includes(needle);
  });
  if (!needle) return matches;
  const rank = (item: StoreItem): number => {
    const names = [item.name, item.upstream?.package ?? item.id].map((name) => name.toLowerCase());
    if (names.some((name) => name === needle)) return 0;
    if (names.some((name) => name.split('/').at(-1) === needle)) return 1;
    if (names.some((name) => name.startsWith(needle) || name.split('/').at(-1)?.startsWith(needle))) return 2;
    if (names.some((name) => name.includes(needle))) return 3;
    if (item.description.toLowerCase().includes(needle)) return 4;
    return 5;
  };
  return matches.sort((a, b) => rank(a) - rank(b)
    || (a.searchOrder ?? Number.MAX_SAFE_INTEGER) - (b.searchOrder ?? Number.MAX_SAFE_INTEGER));
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
