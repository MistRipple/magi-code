// 页面 fetch 只访问本插件路由，沿唯一视图桥执行；浏览器负责 Request 输入校验。
export function createRouteFetch(request, baseUri) {
  const base = new URL(baseUri);
  return async (input, init) => {
    const original = input instanceof Request ? input : new URL(String(input), base);
    const outgoing = new Request(original, init);
    const url = new URL(outgoing.url);
    if (url.origin !== base.origin) throw new TypeError(`插件界面只能访问自己宿主的后端路由：${url.href}`);
    outgoing.signal.throwIfAborted();
    let body;
    if (outgoing.body !== null) {
      const text = await outgoing.text();
      outgoing.signal.throwIfAborted();
      try { body = JSON.parse(text); } catch { body = text; }
    }
    const result = await request({
      operation: 'route.call', method: outgoing.method, path: url.pathname,
      query: url.search.slice(1), headers: Object.fromEntries(outgoing.headers),
      ...(body === undefined ? {} : { body }),
    }, outgoing.signal);
    outgoing.signal.throwIfAborted();
    const contentType = result.contentType ?? (typeof result.body === 'string' ? 'text/plain' : 'application/json');
    const payload = result.body === null || result.body === undefined ? '' : typeof result.body === 'string' ? result.body : JSON.stringify(result.body);
    return new Response(outgoing.method === 'HEAD' || [204, 205, 304].includes(result.status) ? null : payload, {
      status: result.status, headers: { 'content-type': contentType },
    });
  };
}
