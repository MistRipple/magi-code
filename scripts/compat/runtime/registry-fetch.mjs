// npm 的版本文档不总是接受精简 packument 媒体类型（部分边缘节点返回
// 406）。只对当前 registry 的只读版本请求协商 JSON；不改变下载、写入、
// 凭据、取消或其他服务的错误语义，不按插件名称添加网络补丁。
export function installRegistryFetch({ registry = process.env.MAGI_NPM_REGISTRY ?? 'https://registry.npmjs.org', target = globalThis } = {}) {
  const base = new URL(registry.endsWith('/') ? registry : registry + '/');
  const original = target.fetch;
  const versionDocument = (request) => {
    const url = new URL(request.url);
    if (request.method !== 'GET' || url.origin !== base.origin || !url.pathname.startsWith(base.pathname)
      || !request.headers.get('accept')?.includes('application/vnd.npm.install-v1+json')) return false;
    let path;
    try { path = decodeURIComponent(url.pathname.slice(base.pathname.length)); } catch { return false; }
    return /^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*\/[a-zA-Z0-9][a-zA-Z0-9.+_-]{0,127}$/.test(path);
  };
  const fetch = async (input, options) => {
    // 非目标请求原样交给 fetch，保留 dispatcher、流式正文等 Node 扩展选项。
    const method = options?.method ?? (input instanceof Request ? input.method : 'GET');
    if (String(method).toUpperCase() !== 'GET') return original.call(target, input, options);
    const request = new Request(input, options);
    if (!versionDocument(request)) return original.call(target, input, options);
    const response = await original.call(target, input, options);
    if (response.status !== 406) return response;
    await response.body?.cancel();
    request.signal.throwIfAborted();
    const headers = new Headers(request.headers); headers.set('accept', 'application/json');
    return original.call(target, new Request(request, { headers }), { ...options, headers });
  };
  target.fetch = fetch;
  return () => { if (target.fetch === fetch) target.fetch = original; };
}
