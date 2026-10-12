/**
 * 已激活插件贡献的前端投影。
 *
 * daemon 的激活注册表是插件可用性的唯一事实；这里只按作用域缓存它的读取结果，
 * 让设置页、右侧面板、输入区等入口看到同一份清单，并在注册表变化后一起刷新。
 * 入口出现与面板打开分别管理：投影只决定“有哪些入口”，不打开任何页面。
 */

import {
  APPLICATION_PLUGIN_SCOPE,
  loadActivePluginCommands,
  loadActivePluginManifests,
  loadActivePluginWorkflows,
  PLUGIN_REGISTRY_CHANGED_EVENT,
  type PluginContributionProjection,
  type PluginManifestProjection,
} from '../web/plugin-api';

export { APPLICATION_PLUGIN_SCOPE };

interface PluginScopeProjection {
  manifests: PluginManifestProjection[];
  commands: PluginContributionProjection[];
  workflows: PluginContributionProjection[];
  /** 至少完成过一次读取；未完成前不能据此判断插件“不存在”。 */
  loaded: boolean;
}

const EMPTY_PROJECTION: PluginScopeProjection = {
  manifests: [],
  commands: [],
  workflows: [],
  loaded: false,
};

const projections = $state<Record<string, PluginScopeProjection>>({});
/** 每次注册表变化加一；读取它的 effect 会随之重新读取 daemon 投影。 */
const registry = $state({ revision: 0 });
/** 每个作用域最近一次发起读取时的注册表修订；同一修订只读取一次，多个入口共享结果。 */
const requestedRevision = new Map<string, number>();
const inflight = new Map<string, Promise<void>>();

/** 主内容区插件页面的顶层 Tab 标识；顶部标签栏与主页入口共用，保证二者指向同一页面。 */
export function pluginMainPageId(pluginId: string, viewId: string): string {
  return `plugin:${pluginId}:${viewId}`;
}

export function pluginProjection(scope: string): PluginScopeProjection {
  return projections[scope] ?? EMPTY_PROJECTION;
}

/** 在 `$effect` 中调用：作用域或注册表修订变化时重新读取该作用域的投影。 */
export function trackPluginProjection(scope: string): void {
  const revision = registry.revision;
  if (requestedRevision.get(scope) === revision) return;
  requestedRevision.set(scope, revision);
  const isCurrent = () => requestedRevision.get(scope) === revision;
  const loading = Promise.all([
    loadActivePluginManifests(scope),
    loadActivePluginCommands(scope),
    loadActivePluginWorkflows(scope),
  ]).then(([manifests, commands, workflows]) => {
    if (!isCurrent()) return;
    revokeSelectionsOfRemovedPlugins(projections[scope]?.manifests ?? [], manifests);
    projections[scope] = { manifests, commands, workflows, loaded: true };
  }).catch(() => {
    // 读取失败按“没有可用入口”处理：入口只能来自 daemon 的成功投影。
    if (!isCurrent()) return;
    revokeSelectionsOfRemovedPlugins(projections[scope]?.manifests ?? [], []);
    projections[scope] = { manifests: [], commands: [], workflows: [], loaded: true };
  });
  inflight.set(scope, loading);
}

/** 插件从作用域的激活投影中消失（停用、撤权、升级）：输入区里尚未提交的该插件选择随之失效。 */
function revokeSelectionsOfRemovedPlugins(
  previous: PluginManifestProjection[],
  next: PluginManifestProjection[],
): void {
  if (typeof window === 'undefined') return;
  const alive = new Set(next.map((manifest) => `${manifest.id}@${manifest.version}`));
  for (const manifest of previous) {
    if (!alive.has(`${manifest.id}@${manifest.version}`)) {
      window.dispatchEvent(new CustomEvent('magi:pluginSelectionInvalidated', { detail: { pluginId: manifest.id } }));
    }
  }
}

/** 当前注册表修订；读取它的 effect 会在注册表变化（含窗口重新获得焦点）后重新执行。 */
export function pluginRegistryRevision(): number {
  return registry.revision;
}

/** 取得已加载的作用域投影：尚未加载时发起读取并等待，用于不在 effect 里的一次性判断。 */
export async function loadedPluginProjection(scope: string): Promise<PluginScopeProjection> {
  if (!projections[scope]?.loaded) {
    trackPluginProjection(scope);
    await inflight.get(scope);
  }
  return pluginProjection(scope);
}

function notifyPluginRegistryChanged(): void {
  registry.revision += 1;
}

if (typeof window !== 'undefined') {
  window.addEventListener(PLUGIN_REGISTRY_CHANGED_EVENT, notifyPluginRegistryChanged);
  // 其他客户端（桌面窗口、手机 Web）也可能改动了注册表：回到本窗口时重新读取。
  window.addEventListener('focus', notifyPluginRegistryChanged);
}
