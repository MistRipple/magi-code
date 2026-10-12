import { getTransport } from '../shared/transport';
import { agentUrl } from './agent-api';
import type {
  PluginCandidatePreview,
  PluginCenterConfig,
  PluginCenterProjection,
  PluginInstalled,
  PluginList,
  PluginManifest,
} from '../shared/app-server-protocol.generated';

/** 插件全局生效：daemon 只有这一个作用域键，启用、授权和资源都归在它名下。 */
export const APPLICATION_PLUGIN_SCOPE = 'application';

export type PluginManifestProjection = PluginManifest;

export type InstalledPluginProjection = PluginInstalled;

export const PLUGIN_REGISTRY_CHANGED_EVENT = 'magi-plugin-registry-changed';

export interface PluginContributionProjection {
  id: string;
  pluginId: string;
  contributionId: string;
  version: string;
  digest: string;
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
  return new Error(detail ? `${action}：${detail}` : `${action}（HTTP ${response.status}）`);
}

export async function listInstalledPlugins(): Promise<PluginList> {
  const response = await pluginRequest('/api/plugins');
  if (!response.ok) throw await pluginFailure('读取插件列表失败', response);
  return await response.json() as PluginList;
}

export async function listBundledPlugins(): Promise<PluginManifest[]> {
  const response = await pluginRequest('/api/plugins/bundled');
  if (!response.ok) throw await pluginFailure('内置插件列表请求失败', response);
  const payload = await response.json() as unknown;
  return Array.isArray(payload) ? payload as PluginManifest[] : [];
}

export async function installBundledPlugin(pluginId: string): Promise<void> {
  const response = await pluginRequest(`/api/plugins/bundled/${encodeURIComponent(pluginId)}/install`, {
    method: 'POST',
  });
  if (!response.ok) throw await pluginFailure('安装内置插件失败', response);
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
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
export async function previewPluginCandidate(candidate: PluginCandidate): Promise<PluginCandidatePreview> {
  const response = await pluginRequest('/api/plugins/preview', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: candidateBody(candidate),
  });
  if (!response.ok) throw await pluginFailure('读取候选插件失败', response);
  return await response.json() as PluginCandidatePreview;
}

/** 确认后安装或升级：只接受与预览摘要一致的包。升级由 daemon 先排空全部作用域再切换，新包须重新授权后激活。 */
export async function applyPluginCandidate(candidate: PluginCandidate, preview: PluginCandidatePreview): Promise<void> {
  const upgrade = preview.operation === 'upgrade';
  const response = await pluginRequest(upgrade ? '/api/plugins/upgrade' : '/api/plugins/install', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: candidateBody(candidate, preview.digest),
  });
  if (!response.ok) throw await pluginFailure(upgrade ? '升级插件失败' : '安装插件失败', response);
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
}

/** 插件作者指南（Markdown）：随 daemon 发布，与仓库文档同源。 */
export async function fetchPluginGuide(): Promise<string> {
  const response = await pluginRequest('/api/plugins/guide');
  if (!response.ok) throw await pluginFailure('读取插件开发规范失败', response);
  return await response.text();
}

export async function fetchPluginCenter(): Promise<PluginCenterProjection> {
  const response = await pluginRequest('/api/plugins/center');
  if (!response.ok) throw await pluginFailure('读取插件中心失败', response);
  return await response.json() as PluginCenterProjection;
}

export async function savePluginCenterConfig(config: PluginCenterConfig): Promise<PluginCenterConfig> {
  const response = await pluginRequest('/api/plugins/center/config', {
    method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(config),
  });
  if (!response.ok) throw await pluginFailure('保存插件中心配置失败', response);
  return await response.json() as PluginCenterConfig;
}

/** 从已签名目录安装或升级：包内容须与目录签名绑定的摘要一致。 */
export async function installFromPluginCenter(id: string, version?: string): Promise<void> {
  const response = await pluginRequest('/api/plugins/center/install', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ id, version: version ?? null }),
  });
  if (!response.ok) throw await pluginFailure('从插件中心安装失败', response);
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
}

/** 插件全局生效：一次授权清单里的全部权限，然后启用并激活。 */
export async function authorizeAndActivatePlugin(pluginId: string, manifest: PluginManifest): Promise<void> {
  const post = async (action: string, body: Record<string, unknown>, failure: string): Promise<void> => {
    const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/${action}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ scope: APPLICATION_PLUGIN_SCOPE, ...body }),
    });
    if (!response.ok) throw await pluginFailure(failure, response);
  };
  await post('authorize', { grants: manifest.permissions }, '插件授权失败');
  await post('enable', {}, '插件激活失败');
  await post('activate', {}, '插件激活失败');
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
}

async function changePluginActivation(pluginId: string, action: 'deactivate' | 'disable'): Promise<void> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/${action}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ scope: APPLICATION_PLUGIN_SCOPE }),
  });
  if (!response.ok) throw await pluginFailure(action === 'deactivate' ? '停用插件失败' : '禁用插件失败', response);
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
}

export function deactivatePlugin(pluginId: string): Promise<void> {
  return changePluginActivation(pluginId, 'deactivate');
}

export function disablePlugin(pluginId: string): Promise<void> {
  return changePluginActivation(pluginId, 'disable');
}

export async function uninstallPlugin(pluginId: string): Promise<void> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/uninstall`, { method: 'POST' });
  if (!response.ok) throw await pluginFailure('卸载插件失败', response);
  window.dispatchEvent(new CustomEvent(PLUGIN_REGISTRY_CHANGED_EVENT));
}

export async function loadActivePluginManifests(scope = APPLICATION_PLUGIN_SCOPE): Promise<PluginManifestProjection[]> {
  const response = await getTransport().request(
    agentUrl(`/api/plugins/manifests?scope=${encodeURIComponent(scope)}`),
  );
  if (!response.ok) throw await pluginFailure('插件清单请求失败', response);
  const payload = await response.json() as unknown;
  return Array.isArray(payload) ? payload as PluginManifestProjection[] : [];
}

export async function loadActivePluginCommands(scope = APPLICATION_PLUGIN_SCOPE): Promise<PluginContributionProjection[]> {
  const response = await pluginRequest(`/api/plugins/commands?scope=${encodeURIComponent(scope)}`);
  if (!response.ok) throw await pluginFailure('插件命令请求失败', response);
  const payload = await response.json() as unknown;
  return Array.isArray(payload) ? payload as PluginContributionProjection[] : [];
}

export async function loadActivePluginWorkflows(scope = APPLICATION_PLUGIN_SCOPE): Promise<PluginContributionProjection[]> {
  const response = await pluginRequest(`/api/plugins/workflows?scope=${encodeURIComponent(scope)}`);
  if (!response.ok) throw await pluginFailure('插件工作流请求失败', response);
  const payload = await response.json() as unknown;
  return Array.isArray(payload) ? payload as PluginContributionProjection[] : [];
}

export function subscribePluginResource(
  pluginId: string,
  resourceId: string,
  scope: string,
  onUpdate: (resource: { version: number; value: unknown }) => void,
  onError?: (error: unknown) => void,
): () => void {
  let closed = false;
  const connection = getTransport().connectEventStream(
    agentUrl(
      `/api/plugins/${encodeURIComponent(pluginId)}/resources/${encodeURIComponent(resourceId)}/events?scope=${encodeURIComponent(scope)}`,
    ),
    {
      onOpen() {},
      onMessage(data) {
        if (closed) return;
        try {
          const event = JSON.parse(data) as { reset?: unknown; closed?: unknown; version?: unknown; value?: unknown };
          if (event.closed === true) {
            // 插件被停用、撤权或升级：订阅已由 daemon 结束，不会再有变更。
            onError?.(new Error('插件资源订阅已关闭'));
            return;
          }
          if (event.reset === true) {
            void readPluginResource(pluginId, resourceId, scope)
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
        if (!closed) onError?.(new Error('插件资源订阅失败'));
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
  scope: string,
): Promise<unknown> {
  const response = await pluginRequest(
    `/api/plugins/${encodeURIComponent(pluginId)}/resources/${encodeURIComponent(resourceId)}?scope=${encodeURIComponent(scope)}`,
  );
  if (!response.ok) throw await pluginFailure('读取插件资源失败', response);
  return await response.json();
}

export async function writePluginResource(
  pluginId: string,
  resourceId: string,
  scope: string,
  expectedVersion: number,
  value: unknown,
): Promise<unknown> {
  const response = await pluginRequest(
    `/api/plugins/${encodeURIComponent(pluginId)}/resources/${encodeURIComponent(resourceId)}?scope=${encodeURIComponent(scope)}`,
    {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ expectedVersion, value }),
    },
  );
  if (!response.ok) throw await pluginFailure('写入插件资源失败', response);
  return await response.json();
}

export async function readPluginSettings(pluginId: string, scope: string): Promise<unknown> {
  const response = await pluginRequest(
    `/api/plugins/${encodeURIComponent(pluginId)}/settings?scope=${encodeURIComponent(scope)}`,
  );
  if (!response.ok) throw await pluginFailure('读取插件设置失败', response);
  return await response.json();
}

export async function writePluginSettings(
  pluginId: string,
  scope: string,
  expectedVersion: number,
  value: unknown,
): Promise<unknown> {
  const response = await pluginRequest(
    `/api/plugins/${encodeURIComponent(pluginId)}/settings?scope=${encodeURIComponent(scope)}`,
    {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ expectedVersion, value }),
    },
  );
  if (!response.ok) throw await pluginFailure('写入插件设置失败', response);
  return await response.json();
}
