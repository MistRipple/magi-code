// 页面 fetch 只访问本插件路由，沿唯一视图桥执行；浏览器负责 Request 输入校验。
import { encodeRouteBytes, decodeRouteBytes, readRouteBytes } from '../route-payload.mjs';

export function createRouteFetch(request, baseUri) {
  const base = new URL(baseUri);
  return async (input, init) => {
    const original = input instanceof Request ? input : new URL(String(input), base);
    const outgoing = new Request(original, init);
    const url = new URL(outgoing.url);
    if (url.origin !== base.origin) throw new TypeError(`插件界面只能访问自己宿主的后端路由：${url.href}`);
    outgoing.signal.throwIfAborted();
    let bodyBase64;
    if (outgoing.body !== null) {
      const bytes = await readRouteBytes(outgoing.body, outgoing.signal);
      outgoing.signal.throwIfAborted();
      bodyBase64 = encodeRouteBytes(bytes);
    }
    const result = await request({
      operation: 'route.call', method: outgoing.method, path: url.pathname,
      query: url.search.slice(1), headers: Object.fromEntries(outgoing.headers),
      ...(bodyBase64 === undefined ? {} : { bodyBase64 }),
    }, outgoing.signal);
    outgoing.signal.throwIfAborted();
    const headers = new Headers(result.headers);
    if (result.contentType) headers.set('content-type', result.contentType);
    if (!headers.has('content-type')) headers.set('content-type', result.bodyBase64 !== undefined ? 'application/octet-stream' : typeof result.body === 'string' ? 'text/plain' : 'application/json');
    if (result.bodyBase64 !== undefined && result.body != null) throw new TypeError('插件路由响应载荷冲突');
    const payload = result.bodyBase64 !== undefined ? decodeRouteBytes(result.bodyBase64)
      : result.body == null ? '' : typeof result.body === 'string' ? result.body : JSON.stringify(result.body);
    return new Response(outgoing.method === 'HEAD' || [204, 205, 304].includes(result.status) ? null : payload, {
      status: result.status, headers,
    });
  };
}
