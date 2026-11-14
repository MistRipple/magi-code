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
import { dirname, join } from 'node:path';
import { existsSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { createHash, randomUUID } from 'node:crypto';

const MAX_FRAME = 4 * 1024 * 1024;
const PROTOCOL = { major: 1, minor: 0 };
const FEATURES = ['hook.guard', 'plugin.management', 'plugin.management.background', 'plugin.installations', 'plugin.directory.events', 'plugin.reconciliation', 'plugin.changes', 'plugin.skills', 'session.directory', 'session.messages', 'images.store', 'tool.outcome', 'route.payload', 'model.operations', 'model.streaming'];
let negotiatedFeatures = new Set();
const pluginRequest = (method, input, background = false) => {
  if (!negotiatedFeatures.has('plugin.management')) throw new Error('daemon 未协商 plugin.management');
  if (background && !negotiatedFeatures.has('plugin.management.background')) throw new Error('daemon 未协商 plugin.management.background');
  if (method === 'plugins.reconcile' && !negotiatedFeatures.has('plugin.reconciliation')) throw new Error('daemon 未协商 plugin.reconciliation');
  if (['plugins.installBundle', 'plugins.cancelInstall'].includes(method) && !negotiatedFeatures.has('plugin.installations')) throw new Error('daemon 未协商 plugin.installations');
  if ((method === 'plugins.finishChange' || input?.changeId) && !negotiatedFeatures.has('plugin.changes')) throw new Error('daemon 未协商 plugin.changes');
  return request(method, background ? { ...input, background: true } : input, background);
};
const imageRequest = (method, input) => {
  if (!negotiatedFeatures.has('images.store')) throw new Error('daemon 未协商 images.store');
  // 私有图片存储拥有独立、显式的实例授权，不继承初始化回调留下的 Turn 租约。
  return invocationContext.run(undefined, () => request(method, input, true));
};

const endpoint = process.env.MAGI_EXT_ENDPOINT;
const token = process.env.MAGI_EXT_TOKEN;
const entry = process.env.MAGI_EXT_ENTRY;
// 翻译包（DSH 等）：兼容运行时目录。上游代码的裸导入与 `@magi/compat-dsh` 都解析到它。
const compatRoot = process.env.MAGI_EXT_COMPAT_ROOT;
delete process.env.MAGI_EXT_COMPAT_ROOT;
// 翻译包的私有数据目录：DSH 插件把状态写在 DSH_HOME 下，指向 Magi 管理的插件目录，
// 不触碰用户真实的 ~/.dsh（那是 DSH 自己的数据）。HOME 保持不变，上游读取其他应用数据仍用真实家目录。
if (process.env.MAGI_EXT_COMPAT_HOME) process.env.DSH_HOME = process.env.MAGI_EXT_COMPAT_HOME;
delete process.env.MAGI_EXT_COMPAT_HOME;
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
const modelProgress = new Map();
let generation = null;
let configValues = null;
let configRevision = 0;
let identity = null;
const handlers = new Map(); // hook name -> handler
const toolHandlers = new Map(); // tool id -> handler
// 工具输出本身可以包含任意 JSON 字段，不能把业务对象中的 isError 或
// completion 猜成执行控制。只有本次调用经公开工厂创建的结果才解包。
const toolResults = new WeakMap();
const commandHandlers = new Map(); // command id -> handler
const routeHandlers = new Map(); // 视图后端路径 -> handler
const inflight = new Map(); // invocationId -> AbortController
// 只保存一次已完成响应的传输字节，不保存业务任务或文件事实。读取不重跑路由。
const routePayloads = new Map();
const MAX_ROUTE_RESULT = 24 * 1024 * 1024;
const MAX_ROUTE_PAYLOAD_POOL = 48 * 1024 * 1024;
const pruneRoutePayloads = () => {
  for (const [id, payload] of routePayloads) if (payload.expires <= Date.now()) routePayloads.delete(id);
};
const routePayloadTimer = setInterval(pruneRoutePayloads, 1000);
routePayloadTimer.unref();
function deliverRouteResult(result, owner) {
  const bytes = Buffer.from(JSON.stringify(result));
  if (bytes.length <= 3 * 1024 * 1024 + 64 * 1024) return result;
  if (!negotiatedFeatures.has('route.payload')) throw new Error('daemon 未协商 route.payload，不能交付大路由响应');
  if (bytes.length > MAX_ROUTE_RESULT) throw new RangeError('路由响应超过 24 MiB 编码上限');
  pruneRoutePayloads();
  if (routePayloads.size >= 8 || [...routePayloads.values()].reduce((total, item) => total + item.bytes.length, bytes.length) > MAX_ROUTE_PAYLOAD_POOL) throw new Error('路由响应传输容量已满，请完成或取消当前读取后重试');
  const id = randomUUID();
  routePayloads.set(id, { bytes, owner, expires: Date.now() + 30_000 });
  return { routePayload: { id, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex'), generation } };
}
function releaseRoutePayload(params) {
  if (params?.generation === generation) routePayloads.delete(params.id);
}
const disposers = new Set();
const configListeners = new Set();
const pluginDirectoryListeners = new Set();
let configQueue = Promise.resolve();
let shuttingDown = false;
let validateSettings = null;
async function disposeExtension() {
  pluginDirectoryListeners.clear();
  if (shuttingDown) return;
  shuttingDown = true;
  modelProgress.clear();
  clearInterval(routePayloadTimer);
  routePayloads.clear();
  for (const controller of inflight.values()) controller.abort();
  for (const dispose of [...disposers].reverse()) await dispose();
  disposers.clear();
}

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

function request(method, params, independent = false) {
  const context = invocationContext.getStore();
  if (context?.signal.aborted) return Promise.reject(Object.assign(new Error('调用已取消'), { kind: 'Cancelled' }));
  // 钩子身份只关联取消与诊断，不是工具／命令的业务租约。实例级读取仍经
  // daemon 授权；业务服务缺少租约时由 HostHandler 明确拒绝重入。
  if (!independent && context && !context.hookId) params = { ...params, invocationId: context.invocationId };
  const id = `h${nextId++}`;
  return new Promise((resolve, reject) => {
    if (pending.size >= 256) return reject(Object.assign(new Error('挂起请求过多'), { kind: 'Backpressure' }));
    // 上游准备由 daemon 在 840 秒内结束；管理写入还要等待排空。
    // 等待预算大于准备预算，不能先向插件报告超时再迟到提交版本。
    const timer = setTimeout(() => {
      pending.delete(id);
      reject(Object.assign(new Error(`${method} 超时`), { kind: 'DeadlineExceeded' }));
    }, ['plugins.preview', 'plugins.install', 'plugins.installBundle', 'plugins.cancelInstall'].includes(method) ? 1_000_000 : 120_000);
    pending.set(id, { resolve, reject, timer });
    try { send({ id, method, params }); }
    catch (error) { clearTimeout(timer); pending.delete(id); reject(error); }
  });
}

async function handleRequest(id, method, params) {
  try {
    if (method === 'route.result.read') {
      if (params?.generation !== generation) return send({ id, error: rpcError('StaleGeneration', '路由响应实例已失效') });
      pruneRoutePayloads();
      const payload = routePayloads.get(params.id);
      if (!payload) return send({ id, error: rpcError('BlobExpired', '路由响应已释放或过期；读取不会重新执行业务') });
      if (!Number.isSafeInteger(params.offset) || params.offset < 0 || params.offset >= payload.bytes.length) return send({ id, error: rpcError('InvalidParams', '路由响应读取位置无效') });
      payload.expires = Date.now() + 30_000;
      return send({ id, result: { offset: params.offset, data: payload.bytes.subarray(params.offset, params.offset + 64 * 1024).toString('base64') } });
    }
    if (method === 'settings.validate') {
      if (!validateSettings) throw new Error('扩展未提供设置校验器');
      if (params.generation !== generation) throw new Error('设置校验实例已失效');
      await validateSettings(params.value);
      return send({ id, result: {} });
    }
    if (method === 'host.ping') return send({ id, result: {} });
    if (method === 'host.shutdown') {
      await disposeExtension();
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
        const result = output && typeof output === 'object' ? toolResults.get(output) : undefined;
        if (result && (result.signal !== controller.signal || result.consumed)) throw new Error('工具结果不属于当前调用或已使用');
        if (result) result.consumed = true;
        return send({ id, result: result ? result.value : { output: output === undefined ? null : output } });
      } catch (error) {
        // 工具自身抛出的错误是模型可见的工具失败，不是协议错误。
        return send({ id, result: { isError: true, output: String(error?.message ?? error).slice(0, 4096) } });
      } finally {
        inflight.delete(ctx.invocationId);
      }
    }
    if (method === 'route.invoke') {
      const ctx = params?.ctx ?? {};
      if (generation !== null && ctx.generation !== generation) {
        return send({ id, error: rpcError('StaleGeneration', '实例代次不符') });
      }
      const handler = routeHandlers.get(params.route);
      if (!handler) return send({ id, error: rpcError('MethodNotFound', `后端路由 ${params.route} 未绑定`) });
      if (typeof ctx.invocationId !== 'string' || !ctx.invocationId || inflight.has(ctx.invocationId)) {
        return send({ id, error: rpcError('InvalidParams', '调用身份缺失或重复') });
      }
      const controller = new AbortController();
      inflight.set(ctx.invocationId, controller);
      try {
        const output = await invocationContext.run({ ...ctx, signal: controller.signal }, () => handler(params.input, { ...ctx, signal: controller.signal }));
        const status = Number.isInteger(output?.status) && output.status >= 100 && output.status <= 599 ? output.status : 200;
        const contentType = output?.contentType;
        if (contentType !== undefined && (typeof contentType !== 'string' || contentType.length > 128 || /[\r\n\0]/.test(contentType))) throw new TypeError('路由内容类型无效');
        if (output?.bodyBase64 !== undefined && output?.body != null) throw new TypeError('路由不能同时返回 JSON 与字节载荷');
        const result = { status, ...(contentType ? { contentType } : {}),
          ...(output?.headers === undefined ? {} : { headers: output.headers }),
          ...(output?.bodyBase64 === undefined ? { body: output?.body ?? null } : { bodyBase64: output.bodyBase64 }),
        };
        controller.signal.throwIfAborted();
        return send({ id, result: deliverRouteResult(result, ctx.invocationId) });
      } catch (error) {
        // 路由自身抛出的错误是视图可见的 500，不是协议错误。
        return send({ id, result: { status: 500, body: { error: String(error?.message ?? error).slice(0, 4096) } } });
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
  if (method === 'model.delta' && params?.generation === generation) {
    modelProgress.get(params.operationId)?.(params.delta);
  } else if (method === 'plugins.changed' && negotiatedFeatures.has('plugin.directory.events')) {
    for (const listener of [...pluginDirectoryListeners]) {
      Promise.resolve().then(() => {
        if (pluginDirectoryListeners.has(listener)) return listener();
      }).catch((error) => api.log(`插件目录订阅失败: ${error?.message ?? error}`));
    }
  } else if (method === 'config.changed') {
    if (Number.isSafeInteger(params?.revision) && params.revision > configRevision) {
      configValues = deepFreeze(params.values ?? null);
      configRevision = params.revision;
      const snapshot = { version: configRevision, value: configValues };
      configQueue = configQueue.then(async () => {
        for (const listener of [...configListeners]) await listener(snapshot);
      }).catch((error) => fail(`配置更新失败: ${error?.message ?? error}`));
    }
  } else if (method === 'host.cancel') {
    inflight.get(params?.invocationId)?.abort();
    for (const [id, payload] of routePayloads) if (payload.owner === params?.invocationId) routePayloads.delete(id);
  } else if (method === 'route.result.release') {
    releaseRoutePayload(params);
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

/**
 * 处理器释放。daemon 侧的绑定集合以名字为键，没有代次，所以“旧释放”和“同名新注册”的先后必须在宿主这一侧排好：
 * - 本地表里已是别的处理器（新注册已落地）：不通知 daemon，否则旧释放会把新绑定一起收回；
 * - 新注册的请求已发出、尚未落地：登记为占位，等它落地后再决定；占位存在时同样不通知 daemon。
 * 上游框架重新挂载时常常先注册新的、再释放旧的，二者不会互相等待。
 */
const registering = new Map();
function release(method, table, id, handler) {
  let released = false;
  return async () => {
    if (released || table.get(id) !== handler) return;
    released = true;
    table.delete(id);
    if ((registering.get(`${method}:${id}`) ?? 0) > 0) return; // 同名新注册已在途：绑定归它
    await request(method, { name: id });
  };
}
async function register(method, releaseMethod, table, id, handler) {
  const key = `${releaseMethod}:${id}`;
  registering.set(key, (registering.get(key) ?? 0) + 1);
  try {
    await request(method, { name: id });
    table.set(id, handler);
  } finally {
    const left = (registering.get(key) ?? 1) - 1;
    if (left <= 0) registering.delete(key); else registering.set(key, left);
  }
  return release(releaseMethod, table, id, handler);
}

function deepFreeze(value) {
  if (value && typeof value === 'object') {
    for (const child of Object.values(value)) deepFreeze(child);
    Object.freeze(value);
  }
  return value;
}

const pluginApi = (background = false) => Object.freeze({
  list: (input = {}) => request('plugins.list', input, background),
  onChanged: (listener) => {
    if (!negotiatedFeatures.has('plugin.directory.events')) throw new Error('daemon 未协商 plugin.directory.events');
    if (typeof listener !== 'function') throw new TypeError('listener 必须是函数');
    pluginDirectoryListeners.add(listener);
    return () => pluginDirectoryListeners.delete(listener);
  },
  reconcile: (input) => pluginRequest('plugins.reconcile', input, background),
  preview: (input) => pluginRequest('plugins.preview', input, background),
  install: (input) => pluginRequest('plugins.install', input, background),
  installBundle: (input) => pluginRequest('plugins.installBundle', input, background),
  cancelInstall: (requestId) => pluginRequest('plugins.cancelInstall', { requestId }, background),
  compatibility: () => request('plugins.compatibility', {}, background),
  setVersionExemption: (input) => pluginRequest('plugins.setVersionExemption', input, background),
  setEnabled: (pluginId, enabled, expectedDigest, options = {}) => pluginRequest('plugins.setEnabled', { pluginId, enabled,
    ...(expectedDigest === undefined ? {} : { expectedDigest }),
    ...(options.expectedState === undefined ? {} : { expectedState: options.expectedState }),
    ...(options.reason === undefined ? {} : { reason: options.reason }),
  }, background),
  remove: (pluginId, expectedDigest, options = {}) => pluginRequest('plugins.remove', { pluginId, ...(expectedDigest === undefined ? {} : { expectedDigest }),
    ...(options.changeId === undefined ? {} : { changeId: options.changeId }) }, background),
  finishChange: (input) => pluginRequest('plugins.finishChange', input, background),
  ...(!background ? { background: pluginApi(true) } : {}),
});

const api = Object.freeze({
  get identity() { return identity; },
  fail(error) { fail(`扩展运行失败: ${error?.message ?? error}`); },
  /** 握手时 daemon 下发的配置快照（只读）。 */
  get config() { return configValues; },
  get configRevision() { return configRevision; },
  onDispose(dispose) {
    if (typeof dispose !== 'function') throw new TypeError('dispose 必须是函数');
    disposers.add(dispose);
    return () => disposers.delete(dispose);
  },
  onConfig(listener) {
    if (typeof listener !== 'function') throw new TypeError('listener 必须是函数');
    configListeners.add(listener);
    return () => configListeners.delete(listener);
  },
  catalog: Object.freeze({
    replace: (expectedRevision, catalog) => {
      if (catalog.skills?.length && !negotiatedFeatures.has('plugin.skills')) throw new Error('daemon 未协商 plugin.skills');
      return request('catalog.replace', { expectedRevision, catalog });
    },
  }),
  resource: Object.freeze({
    read: (id) => request('resource.read', { resourceId: id }),
    write: (id, expectedVersion, value) => request('resource.write', { resourceId: id, expectedVersion, value }),
  }),
  settings: Object.freeze({
    describe: () => request('settings.describe', {}),
    onValidate(handler) {
      if (typeof handler !== 'function' || validateSettings) throw new TypeError('设置校验器必须且只能注册一个');
      validateSettings = handler;
    },
    async read() {
      const snapshot = await request('settings.read', {});
      configValues = deepFreeze(snapshot.value);
      configRevision = snapshot.version;
      return snapshot;
    },
    async write(expectedVersion, value) {
      if (validateSettings) await validateSettings(value);
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
  /** 绑定工具处理器；工具必须已在清单中声明并获得 `tools` 授权。返回释放函数。 */
  async tool(id, handler) {
    if (typeof handler !== 'function') throw new TypeError('handler 必须是函数');
    return register('register.tool', 'unregister.tool', toolHandlers, id, handler);
  },
  toolResult(output, options = {}) {
    if (!negotiatedFeatures.has('tool.outcome')) throw new Error('daemon 未协商 tool.outcome');
    const context = invocationContext.getStore();
    if (!context || context.activity !== 'tool' || context.signal.aborted
      || inflight.get(context.invocationId)?.signal !== context.signal) throw new Error('工具结果需要当前活动的工具调用');
    if (!options || typeof options !== 'object' || Array.isArray(options)
      || Object.keys(options).some(key => !['isError', 'completion'].includes(key))
      || options.isError !== undefined && typeof options.isError !== 'boolean') throw new TypeError('工具结果选项无效');
    const { isError = false, completion } = options;
    if (completion !== undefined && (isError || typeof completion !== 'string' || !completion.trim()
      || Buffer.byteLength(completion) > 1_048_576)) throw new TypeError('只有成功工具可以提供非空且有界的完成内容');
    const value = Object.freeze({});
    toolResults.set(value, { signal: context.signal, value: {
      output: output === undefined ? null : output, isError, ...(completion === undefined ? {} : { completion }),
    } });
    return value;
  },
  /** 绑定用户命令处理器；命令必须已在清单中声明。返回字符串或 `{ content }`。返回释放函数。 */
  async command(id, handler) {
    if (typeof handler !== 'function') throw new TypeError('handler 必须是函数');
    return register('register.command', 'unregister.command', commandHandlers, id, handler);
  },
  /**
   * 绑定视图后端路由：本插件自己的视图经桥的 `route.call` 调用。路径以 / 开头且不含查询；
   * `match` 为 `exact`（默认）或 `prefix`（最长前缀优先）。
   * 处理器收到 `{ method, path, query, body }`，返回 `{ status?, body }`；抛错即 500。返回释放函数。
   */
  async route(path, handler, options = {}) {
    if (typeof handler !== 'function') throw new TypeError('handler 必须是函数');
    const match = options.match ?? 'exact';
    if (match !== 'exact' && match !== 'prefix') throw new TypeError('match 必须是 exact 或 prefix');
    return register('register.route', 'unregister.route', routeHandlers, `${match}:${path}`, handler);
  },
  model: Object.freeze({
    selection() {
      if (!negotiatedFeatures.has('model.operations')) throw new Error('daemon 未协商 model.operations');
      return request('model.selection', {});
    },
    async invoke(input = {}, { signal, onDelta } = {}) {
      const context = invocationContext.getStore();
      signal = signal && context?.signal ? AbortSignal.any([signal, context.signal]) : signal ?? context?.signal;
      signal?.throwIfAborted();
      if (onDelta !== undefined && (typeof onDelta !== 'function' || !negotiatedFeatures.has('model.streaming') || !input.operationId)) throw new Error('模型流需要 model.streaming、operationId 和 onDelta 回调');
      if (onDelta && context?.activity !== 'route') throw new Error('模型进度回调需要活动页面路由');
      if (modelProgress.has(input.operationId)) throw new Error('同一模型操作仍在执行');
      let progressError;
      const cancel = () => { if (input.operationId) void request('model.cancel', { operationId: input.operationId }).catch(() => {}); };
      if (onDelta) modelProgress.set(input.operationId, delta => invocationContext.run(context, () => {
        if (signal?.aborted || progressError) return;
        try { onDelta(delta); } catch (error) { progressError=error; cancel(); }
      }));
      signal?.addEventListener('abort', cancel, { once: true });
      try {
        const result = await request('model.invoke', onDelta ? { ...input, stream:true } : input);
        if (progressError) throw progressError;
        signal?.throwIfAborted();
        return result;
      } catch(error) { cancel(); throw progressError ?? error; }
      finally { modelProgress.delete(input.operationId); signal?.removeEventListener('abort', cancel); }
    },
  }),
  tools: Object.freeze({ call: (tool, input = {}) => request('tool.call', { tool, input }) }),
  task: Object.freeze({ spawn: (input = {}) => request('task.spawn', input) }),
  notify: Object.freeze({ user: (input = {}) => request('notify.user', input) }),
  ask: Object.freeze({ user: (input = {}) => request('ask.user', input) }),
  session: Object.freeze({
    snapshot: (input = {}) => request('session.snapshot', input),
    message(input) {
      if (!negotiatedFeatures.has('session.messages')) throw new Error('daemon 未协商 session.messages');
      return request('session.message', input);
    },
    directory(input = {}) {
      if (!negotiatedFeatures.has('session.directory')) throw new Error('daemon 未协商 session.directory');
      return request('session.directory', input);
    },
  }),
  /** 读取 daemon 的插件目录；不包含插件配置、私有资源或凭据。 */
  plugins: pluginApi(),
  credential: Object.freeze({ ref: (input = {}) => request('credential.ref', input) }),
  web: Object.freeze({
    read: () => request('web.read', {}),
    configure: input => request('web.configure', input),
  }),
  /** 向主对话的模型目录提交（整体替换）清单声明过且获 `modelSources` 授权的数据源；宿主退出时自动撤销。 */
  modelSource: Object.freeze({
    set: (sourceId, input = {}) => request('modelSource.set', { sourceId, ...input }),
    clear: (sourceId) => request('modelSource.clear', { sourceId }),
  }),
  blob: Object.freeze({
    put: (value, contentType) => request('blob.put', { value, contentType }),
    get: (ref) => request('blob.get', { ref }),
  }),
  images: Object.freeze({
    async save(input) {
      if (!input || !(input.data instanceof Uint8Array) || !input.data.length || input.data.length > 10 * 1024 * 1024) throw new TypeError('图片必须为不超过 10 MiB 的字节数组');
      const data = Buffer.from(input.data);
      const upload = await imageRequest('images.begin', { mediaType: input.mediaType, name: input.name ?? 'image', bytes: data.length });
      try {
        for (let offset = 0; offset < data.length; offset += 786432) {
          const bytes = data.subarray(offset, offset + 786432);
          const result = await imageRequest('images.append', { uploadId: upload.uploadId, offset, dataBase64: bytes.toString('base64') });
          if (result.offset !== offset + bytes.length) throw new Error('daemon 图片上传偏移不符');
        }
        return await imageRequest('images.commit', upload);
      } finally { await imageRequest('images.abort', upload).catch(() => {}); }
    },
    async read(attachmentId) {
      const ref = await imageRequest('images.describe', { attachmentId });
      if (!Number.isSafeInteger(ref.bytes) || ref.bytes < 1 || ref.bytes > 10 * 1024 * 1024 || ref.attachmentId !== attachmentId) throw new Error('daemon 图片附件元数据无效');
      const chunks = [];
      for (let offset = 0; offset < ref.bytes;) {
        const length = Math.min(786432, ref.bytes - offset);
        const part = await imageRequest('images.read', { attachmentId, offset, length });
        const bytes = Buffer.from(part.dataBase64, 'base64');
        if (part.offset !== offset || part.totalBytes !== ref.bytes || bytes.length !== length) throw new Error('daemon 图片附件分块无效');
        chunks.push(bytes); offset += bytes.length;
      }
      const data = Buffer.concat(chunks);
      if (createHash('sha256').update(data).digest('hex') !== ref.sha256) throw new Error('daemon 图片附件摘要不符');
      return { ref, data };
    },
    release: (attachmentId) => imageRequest('images.release', { attachmentId }),
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
    negotiatedFeatures = new Set(hello.features ?? []);
    generation = hello.instance.generation;
    identity = deepFreeze(hello.instance);
    configValues = deepFreeze(hello.config?.values ?? null);
    configRevision = hello.config?.revision ?? 0;
    if (compatRoot) {
      const { installSharedResolution } = await import(pathToFileURL(join(compatRoot, 'runtime/shared-resolution.mjs')).href);
      installSharedResolution(compatRoot, {
        '@magi/compat-dsh': join(compatRoot, 'runtime/dsh-host.mjs'),
        ...(existsSync(join(dirname(entry), 'upstream')) ? { '@magi/compat-upstream-root': join(dirname(entry), 'upstream') } : {}),
      });
    }
    const module = await import(pathToFileURL(entry).href);
    if (typeof module.default !== 'function') throw new Error('入口必须默认导出 activate(api)');
    const activated = await module.default(api);
    if (typeof activated === 'function') disposers.add(activated);
    else if (typeof activated?.dispose === 'function') disposers.add(() => activated.dispose());
    send({ method: 'host.ready', params: { generation } });
  } catch (error) {
    try { await disposeExtension(); } catch {}
    fail(`激活失败: ${error?.message ?? error}`);
  }
});
