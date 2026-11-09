// DSH webServer 路由的执行层：把视图经桥发来的请求还原成上游处理器熟悉的形态，收集响应。
// 本文件不依赖 Cordis，构建期打进扩展入口，也可在 Node 里单独测试。
//
// 上游路由有两种形态（见 DSH 的 webserver 文档）：
//   handler(req, res)      Node 风格；req 是可读流，带 method / url / headers / socket
//   resource(Request)      Fetch 风格；只服务 GET / HEAD
// 响应整体收集后一次返回；流式响应（如 NDJSON）以整段文本返回，进度不会逐行推送（兼容报告里披露）。
import { EventEmitter } from 'node:events';
import { Readable } from 'node:stream';
import { decodeRouteBytes, encodeRouteBytes, readRouteBytes, MAX_ROUTE_BINARY_BYTES, MAX_ROUTE_RESPONSE_BYTES, MAX_ROUTE_RESPONSE_WIRE_BYTES } from './route-payload.mjs';

const JSON_TYPE = /(^|[/+])json(\s*;|$)/i;

// 只传递插件页面自己提供的应用头；宿主身份与传输头由消息桥拥有。
// 不转发 Magi HTTP 请求的 Cookie、Bearer 或 Origin。
export function routeHeaders(input) {
  const values = input ?? {};
  if (typeof values !== 'object' || Array.isArray(values) || Object.keys(values).length > 64) throw new Error('插件路由请求头无效');
  const headers = Object.create(null);
  for (const [key, value] of Object.entries(values)) {
    if (!/^[!#$%&'*+.^_`|~0-9a-z-]+$/i.test(key) || typeof value !== 'string' || value.length > 8192 || /[\r\n\0]/.test(value)) throw new Error('插件路由请求头无效');
    const name = key.toLowerCase();
    if (/^(host|origin|cookie|connection|content-length|transfer-encoding|upgrade|proxy-.*|sec-.*|x-forwarded-.*|forwarded)$/.test(name)) throw new Error(`插件路由不能覆盖宿主请求头：${name}`);
    headers[name] = value;
  }
  return headers;
}

/** 把视图请求还原成上游处理器熟悉的 Node 请求对象：回环主机头、本机套接字、可读的请求体流。 */
export function makeRouteRequest(input, signal, options = {}) {
  const url = input.query ? `${input.path}?${input.query}` : input.path;
  if (input.bodyBase64 !== undefined && input.body != null) throw new TypeError('插件路由不能同时提供 JSON 与字节载荷');
  const hasBody = input.bodyBase64 !== undefined || input.body != null;
  if (hasBody && ['GET', 'HEAD'].includes(input.method)) throw new TypeError('GET/HEAD 请求不能携带正文');
  const payload = input.bodyBase64 !== undefined ? Buffer.from(decodeRouteBytes(input.bodyBase64))
    : hasBody ? Buffer.from(typeof input.body === 'string' ? input.body : JSON.stringify(input.body)) : Buffer.alloc(0);
  if (payload.length > MAX_ROUTE_BINARY_BYTES) throw new RangeError('插件路由请求超过大小上限');
  const req = Readable.from(payload.length ? [payload] : [], options);
  req.method = input.method;
  req.url = url;
  const headers = routeHeaders(input.headers);
  req.headers = { ...headers, host: '127.0.0.1', ...(hasBody ? {
    ...(!headers['content-type'] && input.bodyBase64 === undefined ? { 'content-type': 'application/json' } : {}),
    'content-length': String(payload.length),
  } : {}) };
  req.socket = { localAddress: '127.0.0.1', remoteAddress: '127.0.0.1' };
  const abort = () => req.destroy();
  signal.addEventListener('abort', abort, { once: true });
  req.once('close', () => signal.removeEventListener('abort', abort));
  if (signal.aborted) abort();
  return req;
}

/** 收集上游处理器写出的响应；`end` 之后通过 onEnd 交出状态码、内容类型和整段正文。 */
export function makeRouteResponse(onEnd, streaming) {
  const emitter = new EventEmitter();
  const chunks = [];
  const headers = Object.create(null);
  let size = 0;
  let ended = false;
  let overflow = false;
  const publishHeaders = () => streaming?.headers({ status: res.statusCode, contentType: headers['content-type'] });
  const push = (chunk, encoding) => {
    const buffer = chunk instanceof Uint8Array ? Buffer.from(chunk) : Buffer.from(String(chunk), typeof encoding === 'string' ? encoding : undefined);
    if (streaming) { streaming.chunk(buffer); return; }
    size += buffer.length;
    if (size > MAX_ROUTE_RESPONSE_BYTES) {
      // 超限不截断：丢弃已收集的内容，结束时以明确的失败交出。处理器仍可继续写，但不再占用内存。
      overflow = true;
      chunks.length = 0;
      return;
    }
    if (!overflow) chunks.push(buffer);
  };
  const res = Object.assign(emitter, {
    statusCode: 200, headersSent: false, writable: true,
    writeHead(code, extra) {
      res.statusCode = code;
      for (const [key, value] of Object.entries(extra ?? {})) headers[key.toLowerCase()] = String(value);
      res.headersSent = true;
      publishHeaders();
      return res;
    },
    setHeader(key, value) { headers[key.toLowerCase()] = String(value); },
    getHeader(key) { return headers[key.toLowerCase()]; },
    flushHeaders() { res.headersSent = true; publishHeaders(); },
    write(chunk, encoding) { if (ended) return false; res.headersSent = true; publishHeaders(); push(chunk, encoding); return true; },
    destroy() {
      if (ended) return res;
      ended = true;
      res.writable = false;
      chunks.length = 0;
      emitter.emit('close');
      return res;
    },
    end(chunk, encoding) {
      if (ended) return res;
      publishHeaders();
      if (chunk !== undefined && chunk !== null) push(chunk, encoding);
      ended = true;
      res.writable = false;
      if (overflow) {
        onEnd({ status: 502, contentType: 'application/json', body: Buffer.from(JSON.stringify({ error: `路由响应超过 ${MAX_ROUTE_RESPONSE_BYTES} 字节上限，已丢弃` })) });
      } else {
        onEnd({ status: res.statusCode, contentType: headers['content-type'], headers, body: Buffer.concat(chunks) });
      }
      emitter.emit('finish');
      emitter.emit('close');
      return res;
    },
  });
  return res;
}

/** 正文按媒体类型还原；二进制及不可无损解码的文本保留原字节。 */
export function routeResult({ status, contentType, headers, body }) {
  const text = body.toString('utf8');
  let value = text === '' ? null : text;
  const metadata = { status, ...(contentType ? { contentType } : {}), ...(headers ? { headers } : {}) };
  // 文本与 JSON 保持公开路由的结构化结果；其他媒体和无效 UTF-8 始终传原字节。
  if ((contentType && !/^(text\/|application\/(?:[\w.+-]*json|[\w.+-]*xml|javascript|x-www-form-urlencoded)(?:;|$)|image\/svg\+xml(?:;|$))/i.test(contentType)
    || !Buffer.from(text).equals(body)) && body.length) return boundedResult({ ...metadata, bodyBase64: encodeRouteBytes(body, MAX_ROUTE_RESPONSE_BYTES) });
  if (contentType && JSON_TYPE.test(contentType) && text !== '') {
    try { value = JSON.parse(text); } catch { value = text; }
  }
  return boundedResult({ ...metadata, body: value });
}

function boundedResult(result) {
  if (Buffer.byteLength(JSON.stringify(result)) > MAX_ROUTE_RESPONSE_WIRE_BYTES) throw new RangeError('插件路由响应超过消息大小上限');
  return result;
}

/**
 * 执行一条上游路由。
 * @param {{ handler?: Function, resource?: Function }} definition
 * @param {{ method: string, path: string, query?: string, body?: unknown }} input
 * @param {{ signal: AbortSignal }} execContext
 */
export async function runRoute(definition, input, execContext) {
  const signal = execContext.signal;
  signal.throwIfAborted();
  if (typeof definition.resource === 'function') {
    if (input.method !== 'GET' && input.method !== 'HEAD') {
      return { status: 405, contentType: 'application/json', body: { error: 'method not allowed' } };
    }
    const url = new URL(input.query ? `${input.path}?${input.query}` : input.path, 'http://127.0.0.1');
    let abort;
    const cancelled = new Promise((_, reject) => {
      abort = () => reject(Object.assign(new Error('路由调用已取消'), { kind: 'Cancelled' }));
      signal.addEventListener('abort', abort, { once: true });
    });
    let response;
    try {
      response = await Promise.race([Promise.resolve().then(() => {
        signal.throwIfAborted();
        return definition.resource(new Request(url, { method: input.method, headers: routeHeaders(input.headers), signal }));
      }).then(response => {
        if (signal.aborted) { void response.body?.cancel().catch(() => {}); signal.throwIfAborted(); }
        return response;
      }), cancelled]);
    } finally { signal.removeEventListener('abort', abort); }
    const buffer = Buffer.from(await readRouteBytes(response.body, signal, MAX_ROUTE_RESPONSE_BYTES));
    return routeResult({ status: response.status, contentType: response.headers.get('content-type') ?? undefined, headers: Object.fromEntries(response.headers), body: buffer });
  }
  return await new Promise((resolve, reject) => {
    const req = makeRouteRequest(input, signal);
    const res = makeRouteResponse((done) => {
      signal.removeEventListener('abort', abort);
      req.destroy();
      try { resolve(routeResult(done)); } catch (error) { reject(error); }
    });
    const abort = () => {
      res.destroy();
      reject(Object.assign(new Error('路由调用已取消'), { kind: 'Cancelled' }));
    };
    signal.addEventListener('abort', abort, { once: true });
    Promise.resolve()
      .then(() => { signal.throwIfAborted(); return definition.handler(req, res); })
      .catch((error) => {
        // 与 DSH 的 webserver 一致：处理器抛错回 400，而不是让宿主进程失败。
        if (!res.headersSent) {
          res.writeHead(400, { 'content-type': 'application/json' });
          res.end(JSON.stringify({ error: String(error?.message ?? error).slice(0, 2000) }));
        } else {
          res.end();
        }
      });
  });
}
