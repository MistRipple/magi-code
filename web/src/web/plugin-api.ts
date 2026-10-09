import { getTransport } from '../shared/transport';
import { agentUrl } from './agent-api';
import type { PluginInstalled, PluginManifest } from '../shared/app-server-protocol.generated';

export type PluginManifestProjection = PluginManifest;

export type InstalledPluginProjection = PluginInstalled;

async function pluginRequest(path: string, init?: RequestInit): Promise<Response> {
  return getTransport().request(agentUrl(path), init);
}

export async function listInstalledPlugins(): Promise<InstalledPluginProjection[]> {
  const response = await pluginRequest('/api/plugins');
  if (!response.ok) throw new Error(`插件列表请求失败: ${response.status}`);
  const payload = await response.json() as { plugins?: InstalledPluginProjection[] };
  return Array.isArray(payload.plugins) ? payload.plugins : [];
}

export async function fetchPluginManifest(pluginId: string): Promise<PluginManifest> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/manifest`);
  if (!response.ok) throw new Error(`插件清单请求失败: ${response.status}`);
  return await response.json() as PluginManifest;
}

export async function installPluginArchive(archive: ArrayBuffer, source = 'local:browser-upload'): Promise<void> {
  const bytes = new Uint8Array(archive);
  let binary = '';
  for (let index = 0; index < bytes.length; index += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
  }
  const response = await pluginRequest('/api/plugins/install', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ source, archiveBase64: btoa(binary) }),
  });
  if (!response.ok) throw new Error(`安装插件失败: ${response.status}`);
}

export async function installPluginAddress(source: string): Promise<void> {
  const response = await pluginRequest('/api/plugins/install', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ source }),
  });
  if (!response.ok) throw new Error(`安装插件失败: ${response.status}`);
}

export async function authorizeAndActivatePlugin(pluginId: string, manifest: PluginManifest, scope: string): Promise<void> {
  const scopeKind = scope === 'application' ? 'application' : 'workspace';
  const grants = manifest.permissions
    .filter((permission) => permission.scope === scopeKind)
    .map((permission) => ({ ...permission, scope: scopeKind }));
  const body = JSON.stringify({ scope, grants });
  for (const path of [
    `/api/plugins/${encodeURIComponent(pluginId)}/authorize`,
    `/api/plugins/${encodeURIComponent(pluginId)}/enable`,
    `/api/plugins/${encodeURIComponent(pluginId)}/activate`,
  ]) {
    const response = await pluginRequest(path, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: path.endsWith('/authorize') ? body : JSON.stringify({ scope }),
    });
    if (!response.ok) throw new Error(`插件激活失败: ${response.status}`);
  }
}

export async function uninstallPlugin(pluginId: string): Promise<void> {
  const response = await pluginRequest(`/api/plugins/${encodeURIComponent(pluginId)}/uninstall`, { method: 'POST' });
  if (!response.ok) throw new Error(`卸载插件失败: ${response.status}`);
}

export async function loadActivePluginManifests(scope = 'application'): Promise<PluginManifestProjection[]> {
  const response = await getTransport().request(
    agentUrl(`/api/plugins/manifests?scope=${encodeURIComponent(scope)}`),
  );
  if (!response.ok) throw new Error(`插件清单请求失败: ${response.status}`);
  const payload = await response.json() as unknown;
  return Array.isArray(payload) ? payload as PluginManifestProjection[] : [];
}
