/**
 * 已激活插件贡献的前端投影。
 *
 * daemon 的激活注册表是插件可用性的唯一事实；这里只缓存它的读取结果，
 * 让设置页、右侧面板、输入区等入口看到同一份清单，并在注册表变化后一起刷新。
 * 入口出现与面板打开分别管理：投影只决定“有哪些入口”，不打开任何页面。
 */

import { untrack } from 'svelte';
import type { PluginInstalled } from '../shared/app-server-protocol.generated';
import {
  listInstalledPlugins,
  loadEnabledPluginCommands,
  loadEnabledPluginManifests,
  PLUGIN_REGISTRY_CHANGED_EVENT,
  type PluginCommandProjection,
  type PluginManifestProjection,
} from '../web/plugin-api';
import { RUNTIME_CONNECTION_EVENT, type AgentConnectionEventDetail } from '../web/agent-api';


interface PluginProjection {
  manifests: PluginManifestProjection[];
  /** 已安装插件（含未激活）：设置导航据此保留停用后的插件页，让用户能在页面顶部重新开启。 */
  installed: PluginInstalled[];
  commands: PluginCommandProjection[];
  /** 当前修订读取成功；加载中或失败时不能据旧清单判断插件“不存在”。 */
  loaded: boolean;
}

const EMPTY_PROJECTION: PluginProjection = {
  manifests: [],
  installed: [],
  commands: [],
  loaded: false,
};

const projection = $state<PluginProjection>({ ...EMPTY_PROJECTION });
const registry = $state({ revision: 0 });
let requestedRevision: number | undefined;
let inflight: Promise<void> | undefined;

/** 主内容区插件页面的顶层 Tab 标识；顶部标签栏与主页入口共用，保证二者指向同一页面。 */
export function pluginMainPageId(pluginId: string, viewId: string): string {
  return `plugin:${pluginId}:${viewId}`;
}

export function pluginProjection(): PluginProjection { return projection; }

/** 全应用只有一份 daemon 插件投影。 */
export function trackPluginProjection(): void {
  const revision = registry.revision;
  if (requestedRevision === revision) return;
  requestedRevision = revision;
  untrack(() => { projection.loaded = false; });
  const isCurrent = () => registry.revision === revision && requestedRevision === revision;
  inflight = Promise.all([loadEnabledPluginManifests(), loadEnabledPluginCommands(), listInstalledPlugins()])
    .then(([manifests, commands, list]) => {
      if (isCurrent()) Object.assign(projection, { manifests, commands, installed: list.plugins, loaded: true });
    }).catch(() => { if (isCurrent()) requestedRevision = undefined; });
}

/** 当前注册表修订；读取它的 effect 会在注册表变化（含窗口重新获得焦点）后重新执行。 */
export function pluginRegistryRevision(): number {
  return registry.revision;
}

/** 取得已加载的全局投影：尚未加载时发起读取并等待，用于不在 effect 里的一次性判断。 */
export async function loadedPluginProjection(): Promise<PluginProjection> {
  let loading: Promise<void> | undefined;
  let revision: number;
  do {
    revision = registry.revision;
    trackPluginProjection();
    loading = inflight;
    await loading;
  } while (revision !== registry.revision || loading !== inflight);
  return pluginProjection();
}

function notifyPluginRegistryChanged(): void {
  registry.revision += 1;
}

if (typeof window !== 'undefined') {
  window.addEventListener(PLUGIN_REGISTRY_CHANGED_EVENT, notifyPluginRegistryChanged);
  // 其他客户端（桌面窗口、手机 Web）也可能改动了注册表：回到本窗口时重新读取。
  window.addEventListener('focus', notifyPluginRegistryChanged);
  window.addEventListener(RUNTIME_CONNECTION_EVENT, (event: Event) => {
    if ((event as CustomEvent<AgentConnectionEventDetail>).detail?.status === 'connected') {
      notifyPluginRegistryChanged();
    }
  });
}
