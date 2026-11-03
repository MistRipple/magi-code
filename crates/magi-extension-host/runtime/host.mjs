// Magi 扩展宿主运行时（Extension Protocol v1 客户端）。
//
// 由 daemon 以固定版本 Node 直接启动：node host.mjs。环境变量由 daemon 注入：
//   MAGI_EXT_ENDPOINT  本地套接字路径 / 命名管道名
//   MAGI_EXT_TOKEN     一次性握手令牌
//   MAGI_EXT_ENTRY     插件入口（包内 extension/index.mjs 的绝对路径）
// 插件以用户权限运行，本运行时不提供也不声称任何 OS 沙箱。

import net from 'node:net';
import { AsyncLocalStorage } from 'node:async_hooks';
const invocationContext = new AsyncLocalStorage();
import { pathToFileURL } from 'node:url';

const MAX_FRAME = 4 * 1024 * 1024;
const PROTOCOL = { major: 1, minor: 0 };
const FEATURES = ['hook.guard'];

const endpoint = process.env.MAGI_EXT_ENDPOINT;
const token = process.env.MAGI_EXT_TOKEN;
const entry = process.env.MAGI_EXT_ENTRY;
delete process.env.MAGI_EXT_TOKEN; // 令牌一次性使用，不留给插件代码读取

function fail(message) {
  process.stderr.write(`magi-ext-host: ${message}\n`);
  process.exit(2);
}
if (!endpoint || !token || !entry) fail('缺少 MAGI_EXT_ENDPOINT / MAGI_EXT_TOKEN / MAGI_EXT_ENTRY');

const socket = net.createConnection(endpoint);
let buffer = Buffer.alloc(0);
let nextId = 1;
const pending = new Map();
let generation = null;
let configValues = null;
let configRevision = 0;
const handlers = new Map(); // hook name -> handler
const toolHandlers = new Map(); // tool id -> handler
const commandHandlers = new Map(); // command id -> handler
const inflight = new Map(); // invocationId -> AbortController

function send(message) {
  const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', ...message }));
  if (body.length > MAX_FRAME) throw new Error('帧超过上限');
  const header = Buffer.alloc(4);
  header.writeUInt32BE(body.length);
  socket.write(Buffer.concat([header, body]));
}

function rpcError(kind, message) {
  return { code: -32000, message, data: { kind, retryable: false, detail: null } };
}

function request(method, params) {
  const context = invocationContext.getStore();
  if (context?.signal.aborted) return Promise.reject(Object.assign(new Error('调用已取消'), { kind: 'Cancelled' }));
  if (context) params = { ...params, invocationId: context.invocationId };
  const id = `h${nextId++}`;
  return new Promise((resolve, reject) => {
    if (pending.size >= 256) return reject(Object.assign(new Error('挂起请求过多'), { kind: 'Backpressure' }));
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(Object.assign(new Error(`${method} 超时`), { kind: 'DeadlineExceeded' }));
    }, 120_000);
    pending.set(id, { resolve, reject, timer });
    try { send({ id, method, params }); }
    catch (error) { clearTimeout(timer); pending.delete(id); reject(error); }
  });
}

async function handleRequest(id, method, params) {
  try {
    if (method === 'host.ping') return send({ id, result: {} });
    if (method === 'host.shutdown') {
      send({ id, result: {} });
      setImmediate(() => process.exit(0));
      return;
    }
    if (method === 'hook.invoke') {
      const ctx = params?.ctx ?? {};
      if (generation !== null && ctx.generation !== generation) {
        return send({ id, error: rpcError('StaleGeneration', '实例代次不符') });
      }
      const handler = handlers.get(params.hook);
      if (!handler) return send({ id, result: { decision: 'continue' } });
      if (typeof ctx.invocationId !== 'string' || !ctx.invocationId || inflight.has(ctx.invocationId)) {
        return send({ id, error: rpcError('InvalidParams', '调用身份缺失或重复') });
      }
      const controller = new AbortController();
      inflight.set(ctx.invocationId, controller);
      try {
        const result = await invocationContext.run({ ...ctx, signal: controller.signal }, () => handler(params.input, { ...ctx, signal: controller.signal }));
        return send({ id, result: normalizeDecision(result) });
      } finally {
        inflight.delete(ctx.invocationId);
      }
    }
    if (method === 'tool.invoke') {
      const ctx = params?.ctx ?? {};
      if (generation !== null && ctx.generation !== generation) {
        return send({ id, error: rpcError('StaleGeneration', '实例代次不符') });
      }
      const handler = toolHandlers.get(params.tool);
      if (!handler) return send({ id, error: rpcError('MethodNotFound', `工具 ${params.tool} 未绑定`) });
      if (typeof ctx.invocationId !== 'string' || !ctx.invocationId || inflight.has(ctx.invocationId)) {
        return send({ id, error: rpcError('InvalidParams', '调用身份缺失或重复') });
      }
      const controller = new AbortController();
      inflight.set(ctx.invocationId, controller);
      try {
        const output = await invocationContext.run({ ...ctx, signal: controller.signal }, () => handler(params.input, { ...ctx, signal: controller.signal }));
        return send({ id, result: { output: output === undefined ? null : output } });
      } catch (error) {
        // 工具自身抛出的错误是模型可见的工具失败，不是协议错误。
        return send({ id, result: { isError: true, output: String(error?.message ?? error).slice(0, 4096) } });
      } finally {
        inflight.delete(ctx.invocationId);
      }
    }
    if (method === 'command.invoke' || method === 'job.run') {
      const ctx = params?.ctx ?? {};
      if (generation !== null && ctx.generation !== generation) {
        return send({ id, error: rpcError('StaleGeneration', '实例代次不符') });
      }
      const handler = commandHandlers.get(params.command);
      if (!handler) return send({ id, error: rpcError('MethodNotFound', `命令 ${params.command} 未绑定`) });
      if (typeof ctx.invocationId !== 'string' || !ctx.invocationId || inflight.has(ctx.invocationId)) {
        return send({ id, error: rpcError('InvalidParams', '调用身份缺失或重复') });
      }
      const controller = new AbortController();
      inflight.set(ctx.invocationId, controller);
      try {
        const output = await invocationContext.run({ ...ctx, signal: controller.signal }, () => handler(params.input, { ...ctx, signal: controller.signal }));
        const content = typeof output === 'string' ? output : output?.content;
        if (typeof content !== 'string' || !content.trim()) {
          return send({ id, result: { isError: true, content: '命令必须返回非空文本，或 { content: string }' } });
        }
        return send({ id, result: { content: content.slice(0, 1048576) } });
      } catch (error) {
        return send({ id, result: { isError: true, content: String(error?.message ?? error).slice(0, 4096) } });
      } finally {
        inflight.delete(ctx.invocationId);
      }
    }
    return send({ id, error: rpcError('MethodNotFound', method) });
  } catch (error) {
    send({ id, error: rpcError('Internal', String(error?.message ?? error)) });
  }
}

function normalizeDecision(result) {
  if (result === undefined || result === null) return { decision: 'continue' };
  if (typeof result !== 'object' || !['continue', 'block', 'replace'].includes(result.decision)) {
    throw new Error('钩子必须返回 { decision: continue | block | replace }');
  }
  return result;
}

function handleNotification(method, params) {
  if (method === 'config.changed') {
    if (Number.isSafeInteger(params?.revision) && params.revision > configRevision) {
      configValues = deepFreeze(params.values ?? null);
      configRevision = params.revision;
    }
  } else if (method === 'host.cancel') {
    inflight.get(params?.invocationId)?.abort();
  } else if (method === 'hook.notify') {
    const handler = handlers.get(params?.hook);
    if (handler) Promise.resolve().then(() => handler(params.input, { ...(params.ctx ?? {}), seq: params.seq })).catch(() => {});
  }
}

function onFrame(value) {
  if (value.method !== undefined) {
    if (value.id !== undefined) void handleRequest(value.id, value.method, value.params);
    else handleNotification(value.method, value.params);
    return;
  }
  const waiter = pending.get(value.id);
  if (!waiter) return;
  pending.delete(value.id);
  clearTimeout(waiter.timer);
  if (value.error) waiter.reject(Object.assign(new Error(value.error.message), { rpc: value.error, kind: value.error.data?.kind }));
  else waiter.resolve(value.result);
}

socket.on('data', (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  while (buffer.length >= 4) {
    const length = buffer.readUInt32BE(0);
    if (length > MAX_FRAME) fail('入站帧超过上限');
    if (buffer.length < 4 + length) return;
    const body = buffer.subarray(4, 4 + length);
    buffer = buffer.subarray(4 + length);
    onFrame(JSON.parse(body.toString('utf8')));
  }
});
socket.on('close', () => process.exit(0));
socket.on('error', (error) => fail(`连接失败: ${error.message}`));

function deepFreeze(value) {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) deepFreeze(child);
    Object.freeze(value);
  }
  return value;
}

const api = Object.freeze({
  /** 握手时 daemon 下发的配置快照（只读）。 */
  get config() { return configValues; },
  get configRevision() { return configRevision; },
  resource: Object.freeze({
    read: (id) => request('resource.read', { resourceId: id }),
    write: (id, expectedVersion, value) => request('resource.write', { resourceId: id, expectedVersion, value }),
  }),
  settings: Object.freeze({
    async read() {
      const snapshot = await request('settings.read', {});
      configValues = deepFreeze(snapshot.value);
      configRevision = snapshot.version;
      return snapshot;
    },
    async write(expectedVersion, value) {
      const snapshot = await request('settings.write', { expectedVersion, value });
      configValues = deepFreeze(snapshot.value);
      configRevision = snapshot.version;
      return snapshot;
    },
  }),
  /** 绑定钩子处理器；钩子必须已在清单中声明并被 daemon 批准。 */
  async on(hook, handler) {
    if (typeof handler !== 'function') throw new TypeError('handler 必须是函数');
    await request('register.hook', { name: hook });
    handlers.set(hook, handler);
  },
  /** 绑定工具处理器；工具必须已在清单中声明并获得 `tools` 授权。 */
  async tool(id, handler) {
    if (typeof handler !== 'function') throw new TypeError('handler 必须是函数');
    await request('register.tool', { name: id });
    toolHandlers.set(id, handler);
  },
  /** 绑定用户命令处理器；命令必须已在清单中声明。返回字符串或 `{ content }`。 */
  async command(id, handler) {
    if (typeof handler !== 'function') throw new TypeError('handler 必须是函数');
    await request('register.command', { name: id });
    commandHandlers.set(id, handler);
  },
  model: Object.freeze({ invoke: (input = {}) => request('model.invoke', input) }),
  tools: Object.freeze({ call: (tool, input = {}) => request('tool.call', { tool, input }) }),
  task: Object.freeze({ spawn: (input = {}) => request('task.spawn', input) }),
  notify: Object.freeze({ user: (input = {}) => request('notify.user', input) }),
  ask: Object.freeze({ user: (input = {}) => request('ask.user', input) }),
  session: Object.freeze({ snapshot: (input = {}) => request('session.snapshot', input) }),
  credential: Object.freeze({ ref: (input = {}) => request('credential.ref', input) }),
  blob: Object.freeze({
    put: (value, contentType) => request('blob.put', { value, contentType }),
    get: (ref) => request('blob.get', { ref }),
  }),
  log(message) {
    send({ method: 'host.log', params: { message: String(message).slice(0, 4096) } });
  },
});

socket.on('connect', async () => {
  try {
    const hello = await request('host.hello', {
      token,
      protocol: PROTOCOL,
      extensionApi: { major: 1, minor: 0 },
      runtime: { node: process.versions.node, platform: process.platform, arch: process.arch },
      features: FEATURES,
    });
    generation = hello.instance.generation;
    configValues = deepFreeze(hello.config?.values ?? null);
    configRevision = hello.config?.revision ?? 0;
    const module = await import(pathToFileURL(entry).href);
    if (typeof module.default !== 'function') throw new Error('入口必须默认导出 activate(api)');
    await module.default(api);
    send({ method: 'host.ready', params: { generation } });
  } catch (error) {
    fail(`激活失败: ${error?.message ?? error}`);
  }
});
