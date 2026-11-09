// JSON 消息中的原始 HTTP 载荷。字节限制在编码前计算，wire 限制为 RPC 帧留出元数据空间。
export const MAX_ROUTE_BINARY_BYTES = 2 * 1024 * 1024;
export const MAX_ROUTE_WIRE_BYTES = 3 * 1024 * 1024 + 64 * 1024;
export const MAX_ROUTE_RESPONSE_BYTES = 16 * 1024 * 1024;
export const MAX_ROUTE_RESPONSE_WIRE_BYTES = 24 * 1024 * 1024;

export function encodeRouteBytes(bytes, limit = MAX_ROUTE_BINARY_BYTES) {
  if (bytes.byteLength > limit) throw new RangeError(`插件路由二进制载荷超过 ${limit} 字节`);
  let text = '';
  for (let offset = 0; offset < bytes.length; offset += 32768) text += String.fromCharCode(...bytes.subarray(offset, offset + 32768));
  return btoa(text);
}

export function decodeRouteBytes(value, limit = MAX_ROUTE_BINARY_BYTES) {
  if (typeof value !== 'string' || value.length > Math.ceil(limit / 3) * 4 || value.length % 4 || !/^[A-Za-z0-9+/]*={0,2}$/.test(value)) throw new TypeError('插件路由 base64 载荷无效');
  const text = atob(value);
  const bytes = Uint8Array.from(text, c => c.charCodeAt(0));
  if (encodeRouteBytes(bytes, limit) !== value) throw new TypeError('插件路由 base64 载荷无效');
  return bytes;
}

export async function readRouteBytes(stream, signal, limit = MAX_ROUTE_BINARY_BYTES) {
  signal.throwIfAborted();
  if (!stream) return new Uint8Array();
  const reader = stream.getReader(), chunks = [];
  let size = 0, abort;
  const cancelled = new Promise((_, reject) => {
    abort = () => { void reader.cancel().catch(() => {}); reject(signal.reason); };
    signal.addEventListener('abort', abort, { once: true });
  });
  try {
    while (true) {
      const chunk = await Promise.race([reader.read(), cancelled]);
      signal.throwIfAborted();
      if (chunk.done) break;
      size += chunk.value.byteLength;
      if (size > limit) throw new RangeError('插件路由载荷超过大小上限');
      chunks.push(chunk.value);
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    return bytes;
  } catch (error) { void reader.cancel().catch(() => {}); throw error; }
  finally { signal.removeEventListener('abort', abort); reader.releaseLock(); }
}
