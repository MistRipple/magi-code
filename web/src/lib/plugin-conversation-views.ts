import type { PluginManifest } from '../shared/app-server-protocol.generated';

// 只列出 daemon 已激活的消息附加视图，入口顺序稳定；不复制安装或授权事实，也不选择正文替代者。
export function assistantMessageViews(manifests: PluginManifest[]) {
  return manifests.flatMap(manifest => (manifest.contributions.views ?? [])
    .filter(view => view.placements.includes('chatAssistant'))
    .map(view => ({ pluginId: manifest.id, view })))
    .sort((a, b) => (a.view.priority ?? 0) - (b.view.priority ?? 0) || a.pluginId.localeCompare(b.pluginId) || a.view.id.localeCompare(b.view.id));
}
