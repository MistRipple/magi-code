// 只管理隔离客户端的物理挂载位置。激活、授权和包身份仍来自 daemon 投影。
export interface PluginClientOutlet {
  node: HTMLElement;
  entry: string;
  placement: 'settings' | 'page';
  frameTestId: string;
}
interface ClientHost { select(outlet: PluginClientOutlet | null): void; dispose(): void }
const outlets = new Map<string, PluginClientOutlet[]>();
const hosts = new Map<string, ClientHost>();
const notify = (plugin: string) => hosts.get(plugin)?.select(outlets.get(plugin)?.filter(outlet => outlet.node.isConnected).at(-1) ?? null);

export function registerPluginClientOutlet(plugin: string, outlet: PluginClientOutlet): () => void {
  const list = outlets.get(plugin) ?? [];
  list.push(outlet); outlets.set(plugin, list); notify(plugin);
  return () => {
    const index = list.indexOf(outlet);
    if (index !== -1) list.splice(index, 1);
    if (!list.length) outlets.delete(plugin);
    // 先把真实客户端移回常驻容器，再由框架删除 outlet，保留 iframe 文档。
    notify(plugin);
  };
}

export function registerPluginClientHost(plugin: string, host: ClientHost): () => void {
  if (hosts.has(plugin)) throw new Error('插件客户端重复挂载');
  hosts.set(plugin, host); notify(plugin);
  return () => { if (hosts.get(plugin) === host) { hosts.delete(plugin); host.dispose(); } };
}

export function movePluginClient(node: HTMLElement, parent: HTMLElement): void {
  if (node.parentElement === parent) return;
  // appendChild 会重载 iframe；原子移动保留文档、React 状态与 SSE。
  if (typeof parent.moveBefore !== 'function') throw new Error('当前浏览器不支持保留插件客户端状态的页面切换');
  parent.moveBefore(node, null);
}

