// 插件自有 SSE 的传输适配。仍走原 daemon 路由准入；队列只保留未确认的传输字节，
// 不存储会话/业务事实。断开、停用和过期会关闭同一个上游请求。
import { randomUUID } from 'node:crypto';
import { StringDecoder } from 'node:string_decoder';
import { makeRouteRequest, makeRouteResponse, routeHeaders } from './routes.mjs';

export const STREAM_ROUTE = '/__magi_compat/streams';
const SSE = /^text\/event-stream(?:\s*;|$)/i;

export function createRouteStreams(find, { maxStreams = 8, maxBytes = 2 * 1024 * 1024, idleMs = 45_000, pollMs = 15_000, headerMs = 15_000 } = {}) {
  const streams = new Map();
  let disposed = false;
  const drop = (id) => {
    const stream = streams.get(id);
    if (!stream) return;
    streams.delete(id);
    stream.ended = true;
    clearTimeout(stream.expiry);
    stream.controller.abort(new Error('插件事件连接已关闭'));
    stream.wake?.();
  };
  const touch = (id, stream) => {
    clearTimeout(stream.expiry);
    stream.expiry = setTimeout(() => drop(id), idleMs);
    stream.expiry.unref?.();
  };
  const open = async (input, signal) => {
    signal.throwIfAborted();
    if (disposed || streams.size >= maxStreams) throw new Error('插件事件连接已停止或超过数量上限');
    if (input?.method !== 'GET' || typeof input.path !== 'string' || !/^\/[\x21-\x7e]{0,255}$/.test(input.path) || input.path.includes('?') || input.path === STREAM_ROUTE
      || (input.query !== undefined && (typeof input.query !== 'string' || input.query.length > 2048))) throw new Error('事件连接只允许插件自己的 GET 路由');
    const definition = find(input.path);
    if (!definition) throw new Error('插件事件路由未注册');
    routeHeaders(input.headers);
    const id = randomUUID(), controller = new AbortController(), decoder = new StringDecoder('utf8');
    const stream = { path: definition.path ?? input.path, controller, queue: [], bytes: 0, next: 1, ack: 0, ended: false, error: undefined, wake: undefined, expiry: undefined, polling: false };
    streams.set(id, stream);
    touch(id, stream);
    let resolveHeaders, rejectHeaders, headersDone = false;
    const headers = new Promise((resolve, reject) => { resolveHeaders = resolve; rejectHeaders = reject; });
    const fail = (error) => {
      if (!headersDone) { headersDone = true; rejectHeaders(error); }
      stream.error = String(error?.message ?? error).slice(0, 2000);
      stream.ended = true;
      controller.abort(error);
      stream.wake?.();
    };
    const publishHeaders = (value) => {
      if (headersDone) return;
      if (value.status === 204) { headersDone = true; resolveHeaders(value); return; }
      if (value.status !== 200 || !SSE.test(value.contentType ?? '')) return fail(new Error(`事件路由须返回 200 text/event-stream，实际为 ${value.status} ${value.contentType ?? ''}`));
      headersDone = true;
      resolveHeaders(value);
    };
    const enqueue = (text) => {
      if (stream.ended || !text) return;
      const bytes = Buffer.byteLength(text);
      if (stream.bytes + bytes > maxBytes || stream.queue.length >= 8192) return fail(new Error('插件事件未确认数据超过上限，连接已关闭'));
      stream.bytes += bytes;
      // 每个传输块有独立序号，响应序列化后仍小于宿主帧上限；SSE 解析器处理跨块帧。
      for (let offset = 0; offset < text.length;) {
        let end = Math.min(text.length, offset + 16_384);
        if (end < text.length && /[\uD800-\uDBFF]/.test(text[end - 1])) end--;
        stream.queue.push({ seq: stream.next++, text: text.slice(offset, end) });
        offset = end;
      }
      stream.wake?.();
    };
    const end = () => {
      enqueue(decoder.end());
      stream.ended = true;
      stream.wake?.();
    };
    const abortOpen = () => { fail(signal.reason ?? new Error('事件连接已取消')); drop(id); };
    signal.addEventListener('abort', abortOpen, { once: true });
    const headerTimeout = setTimeout(() => fail(new Error('事件路由响应头超时')), headerMs);
    void Promise.resolve().then(async () => {
      if (typeof definition.resource === 'function') {
        const url = new URL(input.query ? `${input.path}?${input.query}` : input.path, 'http://127.0.0.1');
        const response = await definition.resource(new Request(url, { headers: routeHeaders(input.headers), signal: controller.signal }));
        publishHeaders({ status: response.status, contentType: response.headers.get('content-type') });
        if (controller.signal.aborted) { await response.body?.cancel(); return; }
        const reader = response.body?.getReader();
        if (reader) {
          const cancel = () => { void reader.cancel().catch(() => {}); };
          controller.signal.addEventListener('abort', cancel, { once: true });
          try { while (!controller.signal.aborted) { const chunk = await reader.read(); if (chunk.done) break; enqueue(decoder.write(Buffer.from(chunk.value))); } }
          finally { controller.signal.removeEventListener('abort', cancel); reader.releaseLock(); }
        }
        end();
      } else {
        const req = makeRouteRequest(input, controller.signal, { autoDestroy: false });
        const res = makeRouteResponse(end, { headers: publishHeaders, chunk: (bytes) => enqueue(decoder.write(bytes)) });
        controller.signal.addEventListener('abort', () => { if (res.writable) { res.writable = false; res.emit('close'); } }, { once: true });
        await definition.handler(req, res);
      }
    }).catch(fail);
    try {
      const metadata = await headers;
      signal.throwIfAborted();
      if (metadata.status === 204) { drop(id); return metadata; }
      return { id, ...metadata };
    } catch (error) { drop(id); throw error; }
    finally { clearTimeout(headerTimeout); signal.removeEventListener('abort', abortOpen); }
  };
  const read = async (id, after, signal) => {
    signal.throwIfAborted();
    const stream = streams.get(id);
    if (!stream) throw new Error('插件事件连接不存在或已过期');
    if (stream.polling) throw new Error('事件连接已有在途读取');
    if (!Number.isSafeInteger(after) || after < stream.ack || after >= stream.next) throw new Error('事件确认序号无效');
    stream.ack = after;
    while (stream.queue[0]?.seq <= after) stream.bytes -= Buffer.byteLength(stream.queue.shift().text);
    touch(id, stream);
    stream.polling = true;
    try {
      if (!stream.queue.length && !stream.ended) await new Promise((resolve, reject) => {
        const finish = () => { clearTimeout(timer); signal.removeEventListener('abort', abort); stream.wake = undefined; resolve(); };
        const abort = () => { finish(); reject(signal.reason); };
        const timer = setTimeout(finish, pollMs);
        stream.wake = finish;
        signal.addEventListener('abort', abort, { once: true });
        if (signal.aborted) abort();
      });
      signal.throwIfAborted();
      if (!streams.has(id)) throw new Error('插件事件连接已关闭');
      const chunks = stream.queue.slice(0, 4);
      const drained = chunks.length === stream.queue.length;
      return { chunks, ended: stream.ended && drained, ...(stream.error && drained ? { error: stream.error } : {}) };
    } finally { stream.polling = false; }
  };
  return {
    async call(input, { signal }) {
      if (input?.action === 'open') return open(input.request, signal);
      if (input?.action === 'read') return read(input.id, input.after, signal);
      if (input?.action === 'close') { drop(input.id); return null; }
      throw new Error('无效的插件事件操作');
    },
    closePath(path) { for (const [id, stream] of streams) if (stream.path === path) drop(id); },
    dispose() { disposed = true; for (const id of streams.keys()) drop(id); },
  };
}
