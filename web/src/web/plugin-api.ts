import { getTransport } from '../shared/transport';
import { agentUrl } from './agent-api';
import type { PluginManifest } from '../shared/app-server-protocol.generated';

export type PluginManifestProjection = PluginManifest;

export async function loadActivePluginManifests(scope = 'application'): Promise<PluginManifestProjection[]> {
  const response = await getTransport().request(
    agentUrl(`/api/plugins/manifests?scope=${encodeURIComponent(scope)}`),
  );
  if (!response.ok) throw new Error(`插件清单请求失败: ${response.status}`);
  const payload = await response.json() as unknown;
  return Array.isArray(payload) ? payload as PluginManifestProjection[] : [];
}
