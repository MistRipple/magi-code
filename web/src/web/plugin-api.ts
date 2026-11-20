import { i18n } from '../stores/i18n.svelte';
import { getTransport } from '../shared/transport';
import { agentUrl } from './agent-api';
import type {
  PluginCandidatePreview,
  PluginInstalled,
  PluginList,
  PluginManifest,
  PluginUpstreamEntry,
  PluginUpstreamPreview,
  PluginUpstreamRequest,
  PluginUpstreamProjection,
} from '../shared/app-server-protocol.generated';

export type PluginManifestProjection = PluginManifest;

export type InstalledPluginProjection = PluginInstalled;

export const PLUGIN_REGISTRY_CHANGED_EVENT = 'magi-plugin-registry-changed';

/** 已激活插件声明的用户命令；身份形如 `plugin/{插件}/{命令}`。 */
export interface PluginCommandProjection {
  id: string;
  pluginId: string;
  contributionId: string;
  version: string;
  title: string;
  description: string;
}

async function pluginRequest(path: string, init?: RequestInit): Promise<Response> {
  return getTransport().request(agentUrl(path), init);
}

/** 把 daemon 的错误响应转成带原因的错误：生命周期冲突（如排空超时）必须让用户看到原因。 */
async function pluginFailure(action: string, response: Response): Promise<Error> {
  let detail = '';
  try {
    const body = await response.json() as { message?: unknown };
    if (typeof body.message === 'string') detail = body.message.trim();
  } catch {
    // 响应体不是 JSON 时只报告状态码。
  }
  return new Error(detail ? i18n.t('common.actionFailureDetail', { action, detail }) : `${action} (HTTP ${response.status})`);
}

export async function listInstalledPlugins(): Promise<PluginList> {
  const response = await pluginRequest('/api/plugins');
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.load'), response);
  return await response.json() as PluginList;
}

function encodeBase64(archive: ArrayBuffer): string {
  const bytes = new Uint8Array(archive);
  let binary = '';
  for (let index = 0; index < bytes.length; index += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
  }
  return btoa(binary);
}

/** 待安装或升级的候选包：来自本地上传（archive）或地址（source 为 `address:<url>`）。 */
export interface PluginCandidate {
  source: string;
  archive?: ArrayBuffer;
}

function candidateBody(candidate: PluginCandidate, expectedDigest?: string): string {
  return JSON.stringify({
    source: candidate.source,
    ...(candidate.archive ? { archiveBase64: encodeBase64(candidate.archive) } : {}),
    ...(expectedDigest ? { expectedDigest } : {}),
  });
}

/** 准备阶段：daemon 校验候选包并给出与已安装版本的权限差异，不写入任何状态。 */
export async function previewPluginCandidate(candidate: PluginCandidate, signal?: AbortSignal): Promise<PluginCandidatePreview> {
  const response = await pluginRequest('/api/plugins/preview', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: candidateBody(candidate),
    signal,
  });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.preview'), response);
  return await response.json() as PluginCandidatePreview;
}

/**
 * 确认后安装或升级：只接受与预览摘要一致的包。新装即启用（用户在预览里已看到全部权限与影响，确认安装即同意）；
 * 升级由 daemon 先排空插件再切换，保持原启用状态。
 */
export async function applyPluginCandidate(candidate: PluginCandidate, preview: PluginCandidatePreview): Promise<void> {
  const upgrade = preview.operation === 'upgrade';
  const response = await pluginRequest(upgrade ? '/api/plugins/upgrade' : '/api/plugins/install', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: candidateBody(candidate, preview.digest),
  });
  if (!response.ok) throw await pluginFailure(upgrade ? i18n.t('plugins.error.upgrade') : i18n.t('plugins.error.install'), response);
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
}

/** 插件作者指南（Markdown）：随 daemon 发布，与仓库文档同源。 */
export async function fetchPluginGuide(): Promise<string> {
  const response = await pluginRequest('/api/plugins/guide');
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.guide'), response);
  return await response.text();
}

/** 在上游生态（DSH、pi）的官方 npm 发布中搜索；空查询列出该生态的常见包。 */
export async function searchUpstreamPlugins(query: string, signal?: AbortSignal): Promise<PluginUpstreamProjection> {
  const response = await pluginRequest(`/api/plugins/upstream?q=${encodeURIComponent(query)}`, { signal });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.upstream'), response);
  return await response.json() as PluginUpstreamProjection;
}

function upstreamBody(entry: PluginUpstreamRequest, expectedDigest?: string): string {
  return JSON.stringify({
    ecosystem: entry.ecosystem,
    package: entry.package,
    version: entry.version,
    github: entry.github,
    ...(expectedDigest ? { expectedDigest } : {}),
  });
}

/** 准备阶段只读取并校验原包，返回确认安装所需的归档摘要。 */
export async function previewUpstreamPlugin(entry: PluginUpstreamRequest, signal?: AbortSignal): Promise<PluginUpstreamPreview> {
  const response = await pluginRequest('/api/plugins/upstream/preview', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: upstreamBody(entry),
    signal,
  });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.upstreamPreview'), response);
  return await response.json() as PluginUpstreamPreview;
}

/** 提交预览确认的原包，等待 daemon 结算。 */
export async function installUpstreamPlugin(preview: PluginUpstreamPreview, ecosystem: PluginUpstreamEntry['ecosystem'], requestId?: string): Promise<boolean> {
  const response = await pluginRequest('/api/plugins/upstream/install', {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...(requestId ? { 'x-magi-plugin-request-id': requestId } : {}) },
    body: upstreamBody({ ecosystem, package: preview.package, version: preview.version, github: preview.github }, preview.candidate.digest),
  });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.upstreamInstall'), response);
  if (response.status === 204) return false;
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
  return true;
}

export async function cancelPluginInstall(requestId: string): Promise<'cancelled' | 'too-late' | 'not-running'> {
  const action = i18n.t('settings.plugins.candidate.cancelOperation');
  const response = await pluginRequest(`/api/plugins/install/${encodeURIComponent(requestId)}/cancel`, { method: 'POST' });
  if (!response.ok) throw await pluginFailure(action, response);
  const { status } = await response.json() as { status: unknown };
  if (status === 'cancelled' || status === 'too-late' || status === 'not-running') return status;
  throw new Error(i18n.t('common.actionFailureDetail', { action, detail: i18n.t('settings.plugins.candidate.cancelUnknown') }));
}

/** 启停绑定用户看到的安装；失败也刷新已提交事实，不伪装整个操作已回滚。 */
async function changePluginEnabled(plugin: PluginInstalled, action: 'enable' | 'disable'): Promise<void> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(plugin.manifest.id)}/${action}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ expectedDigest: plugin.digest }),
  });
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
  if (!response.ok) throw await pluginFailure(i18n.t(`plugins.error.${action}`), response);
}

export function enablePlugin(plugin: PluginInstalled): Promise<void> {
  return changePluginEnabled(plugin, 'enable');
}

export function disablePlugin(plugin: PluginInstalled): Promise<void> {
  return changePluginEnabled(plugin, 'disable');
}

export async function uninstallPlugin(pluginId: string): Promise<void> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/uninstall`, { method: 'POST' });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.uninstall'), response);
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
}

export async function loadEnabledPluginManifests(): Promise<PluginManifestProjection[]> {
  const response = await getTransport().request(
    agentUrl(`/api/plugins/manifests`),
  );
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.manifest'), response);
  const payload = await response.json() as unknown;
  return Array.isArray(payload) ? payload as PluginManifestProjection[] : [];
}

export async function loadEnabledPluginCommands(): Promise<PluginCommandProjection[]> {
  const response = await pluginRequest(`/api/plugins/commands`);
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.commands'), response);
  const payload = await response.json() as unknown;
  return Array.isArray(payload) ? payload as PluginCommandProjection[] : [];
}

/** 视图后端路由：请求经 daemon 转交给该插件的扩展宿主。返回路由处理器给出的状态码与内容。 */
export async function callPluginRoute(
  pluginId: string,
  request: import('../shared/app-server-protocol.generated').PluginRouteCallRequest,
  signal?: AbortSignal,
): Promise<import('../shared/app-server-protocol.generated').PluginRouteCallResult> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/routes`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(request),
    signal,
  });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.callRoute'), response);
  return await response.json();
}

export interface PluginClientSlotsReport {
  instanceId?: string;
  mountedSlots: string[];
  unmountedSlots: string[];
  hasSettingsView: boolean;
  mainKeys: string[];
  rightPaneTabs?: { key: string; title: string; launcherOrder?: number }[];
}

export async function reportPluginClientSlots(pluginId: string, report: PluginClientSlotsReport): Promise<void> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/slots`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(report),
  });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.load'), response);
}

export async function restartPluginHost(): Promise<void> {
  const response = await pluginRequest('/api/plugins/host/restart', { method: 'POST' });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.restartHost'), response);
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
}
