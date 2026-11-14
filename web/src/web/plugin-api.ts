import { i18n } from '../stores/i18n.svelte';
import { getTransport } from '../shared/transport';
import { agentUrl } from './agent-api';
import type {
  PluginCandidatePreview,
  PluginCenterConfig,
  PluginCenterEntry,
  PluginCenterProjection,
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

export async function fetchPluginCenter(): Promise<PluginCenterProjection> {
  const response = await pluginRequest('/api/plugins/center');
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.center'), response);
  return await response.json() as PluginCenterProjection;
}

export async function savePluginCenterConfig(config: PluginCenterConfig): Promise<PluginCenterConfig> {
  const response = await pluginRequest('/api/plugins/center/config', {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(config),
  });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.saveCenter'), response);
  return await response.json() as PluginCenterConfig;
}

/** 从插件中心安装或升级：包内容须与目录签名绑定的摘要一致；新装即启用，升级保持原启用状态。 */
export async function installFromPluginCenter(entry: PluginCenterEntry): Promise<void> {
  const response = await pluginRequest('/api/plugins/center/install', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ id: entry.id, version: entry.version, expectedSha256: entry.sha256 }),
  });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.installCenter'), response);
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
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

/** 准备阶段：daemon 取包、核对完整性并翻译，返回翻译结果与权限差异，不写入任何状态。 */
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

/** 确认后安装：daemon 重新翻译并只接受与预览摘要一致的结果；新装即启用，升级保持原启用状态。 */
export async function installUpstreamPlugin(preview: PluginUpstreamPreview, ecosystem: PluginUpstreamEntry['ecosystem']): Promise<void> {
  const response = await pluginRequest('/api/plugins/upstream/install', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: upstreamBody({ ecosystem, package: preview.package, version: preview.version, github: preview.github }, preview.candidate.digest),
  });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.upstreamInstall'), response);
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
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

export function subscribePluginResource(
  pluginId: string,
  resourceId: string,
  onUpdate: (resource: { version: number; value: unknown }) => void,
  onError?: (error: unknown) => void,
): () => void {
  let closed = false;
  const connection = getTransport().connectEventStream(
    agentUrl(
      `/api/plugins/${encodeURIComponent(pluginId)}/resources/${encodeURIComponent(resourceId)}/events`,
    ),
    {
      onOpen() {},
      onMessage(data) {
        if (closed) return;
        try {
          const event = JSON.parse(data) as { reset?: unknown; closed?: unknown; version?: unknown; value?: unknown };
          if (event.closed === true) {
            // 插件被停用、撤权或升级：订阅已由 daemon 结束，不会再有变更。
            onError?.(new Error(i18n.t('plugins.error.subscriptionClosed')));
            return;
          }
          if (event.reset === true) {
            void readPluginResource(pluginId, resourceId)
              .then((resource) => {
                if (closed) return;
                const snapshot = resource as { version?: unknown; value?: unknown };
                if (typeof snapshot.version !== 'number') throw new Error('invalid plugin resource snapshot');
                onUpdate({ version: snapshot.version, value: snapshot.value });
              })
              .catch((error) => {
                if (!closed) onError?.(error);
              });
            return;
          }
          if (typeof event.version !== 'number') throw new Error('invalid plugin resource event');
          onUpdate({ version: event.version, value: event.value });
        } catch (error) {
          onError?.(error);
        }
      },
      onError() {
        if (!closed) onError?.(new Error(i18n.t('plugins.error.subscriptionFailed')));
      },
    },
  );
  return () => {
    closed = true;
    connection.close();
  };
}

export async function readPluginResource(
  pluginId: string,
  resourceId: string,
): Promise<unknown> {
  const response = await pluginRequest(
    `/api/plugins/${encodeURIComponent(pluginId)}/resources/${encodeURIComponent(resourceId)}`,
  );
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.readResource'), response);
  return await response.json();
}

export async function writePluginResource(
  pluginId: string,
  resourceId: string,
  expectedVersion: number,
  value: unknown,
): Promise<unknown> {
  const response = await pluginRequest(
    `/api/plugins/${encodeURIComponent(pluginId)}/resources/${encodeURIComponent(resourceId)}`,
    {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ expectedVersion, value }),
    },
  );
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.writeResource'), response);
  return await response.json();
}

export async function describePluginSettings(pluginId: string): Promise<unknown> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/settings/schema`);
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.readSettings'), response);
  return await response.json();
}

export async function readPluginSettings(pluginId: string): Promise<unknown> {
  const response = await pluginRequest(
    `/api/plugins/${encodeURIComponent(pluginId)}/settings`,
  );
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.readSettings'), response);
  return await response.json();
}

export async function writePluginSettings(
  pluginId: string,
  expectedVersion: number,
  value: unknown,
): Promise<unknown> {
  const response = await pluginRequest(
    `/api/plugins/${encodeURIComponent(pluginId)}/settings`,
    {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ expectedVersion, value }),
    },
  );
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.writeSettings'), response);
  return await response.json();
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

export async function admitPluginComposer(pluginId: string, request: import('../shared/app-server-protocol.generated').PluginComposerAdmissionRequest, signal?: AbortSignal): Promise<void> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/composer/admit`, {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(request), signal,
  });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.callRoute'), response);
}

export async function admitPluginView(pluginId: string, request: import('../shared/app-server-protocol.generated').PluginViewAdmissionRequest, signal?: AbortSignal): Promise<void> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/views/admit`, {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(request), signal,
  });
  if (!response.ok) throw await pluginFailure(i18n.t('plugins.error.load'), response);
}
