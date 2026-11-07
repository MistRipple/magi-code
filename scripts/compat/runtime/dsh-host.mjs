// DSH 运行期适配器。上游原包执行真实 Cordis；业务与贡献准入由 Magi daemon 裁决。
import { Context, Service } from '@deepseek-ai/cordis';
import { deepEqual, volatileEntries, updateVolatile } from '@deepseek-ai/cosmokit';
import { LlmRuntime } from '@deepseek-ai/dsh-llm';
import { ToolExecutionPipeline } from '@deepseek-ai/dsh-tools/pipeline';
import { runtimeId } from '../core/identity.mjs';
import { toJsonSchema } from '../translate/schema.mjs';
import { RuntimeCatalog } from './catalog.mjs';
import { runRoute } from './routes.mjs';
import { startOpenAiBridge } from './openai-bridge.mjs';
import { requestAttachments, saveRequestImage, withRequestImages } from './attachments.mjs';
import { mountRemote } from './remote.mjs';

const clone = (value) => structuredClone(value ?? {});
const timeoutOf = (value) => value == null ? undefined : Math.min(600_000, Math.max(1_000, Number(value) || 1_000));
const freeze = (value) => {
  if (value && typeof value === 'object') { Object.values(value).forEach(freeze); Object.freeze(value); }
  return value;
};
const LOCAL_EVENTS = new Set(['loader/volatile-update', 'settings/updated', 'llm/adapters-updated', 'llm/models-updated', 'llm/stream', 'tools/change', 'tools/pre-execute', 'tools/execute', 'tools/post-execute', 'tools/result']);
// DSH 的可选兼容分支会在 Magi 不提供对应事实时仍登记监听器。静态翻译报告
// 已把这些事件标成 partial/optional；运行期必须保留“无事件可收”的降级，而不是
// 因为一个可选 UI/会话观察器把整个插件判为失败。

export async function activateDsh(api, module, options) {
  if (options?.runtimeVersion !== 2) throw new Error('DSH 兼容包版本不受支持，请从上游来源重新安装');
  const upstream = module.default ?? module;
  const ctx = new Context();
  // Cordis inject 是“服务出现时挂载”的条件 fiber；不能把可选分支的 PENDING 当成整包失败。
  const conditionalFibers = new Set();
  const inject = ctx.inject;
  ctx.inject = function (...args) {
    const child = inject.apply(this, args);
    // Cordis 返回带调用方上下文的代理；用上游唯一 uid 与 registry 中的真实 fiber 对齐。
    conditionalFibers.add(child.uid);
    return child;
  };
  const catalog = new RuntimeCatalog(api);
  const tools = new Map();
  const guards = new Set();
  const routes = new Set();
  const bridges = new Map();
  const sourceReleases = new Map();
  const savedImages = new Map();
  const published = new Map();
  let stopped = false;
  let fiber;
  let version = -1;
  let rawConfig = {};
  let catalogTimer;
  let configUpdates = Promise.resolve();
  const diagnostic = (message) => api.log(String(message));
  const reportFailure = (error) => { if (!stopped) api.fail(error); };
  const schema = upstream.Config;
  catalog.catalog.settingsSchema = schema?.toJSON ? toJsonSchema(schema.toJSON()).schema : { type: 'object', additionalProperties: true };
  await catalog.publish();

  // 不支持的宿主事件不能成为永远不触发的监听。内部事件及插件自有服务事件由 Cordis 自己拥有。
  ctx.on('internal/listener', (name) => {
    if (typeof name !== 'string' || name.startsWith('internal/') || LOCAL_EVENTS.has(name)) return;
    if (/^(agent|session|turn|system-prompt|fs|approval)\//.test(name)) {
      if (options?.optionalEvents?.includes(name)) {
        diagnostic(`DSH 事件 ${name} 在 Magi 中没有对应事实，按可选兼容分支忽略`);
        return;
      }
      throw new Error(`尚未实现 DSH 宿主事件 ${name}，拒绝注册`);
    }
  });
  ctx.on('internal/status', (child) => {
    if (catalog.live && child.state === 3) void child.await().catch(reportFailure);
  });

  const registration = (owner, kind, definition, handler) => owner.effect(() => catalog.register(kind, definition, () =>
    kind === 'tools' ? api.tool(definition.id, handler) : api.command(definition.id, handler)));

  await ctx.plugin(class extends Service {
    constructor(c) { super(c, 'tools'); }
    register(definition, registrationOptions = {}) {
      if (registrationOptions.when) throw new Error('DSH 每步工具条件尚未接入 Magi 的每步目录，拒绝注册');
      if (!definition?.output?.schema || typeof definition.output.render !== 'function') throw new Error(`工具 ${definition?.name} 必须声明 output.schema 和 output.render`);
      if (tools.has(definition.name)) throw new Error(`工具 ${definition.name} 重复注册`);
      const id = runtimeId('tool', definition.name);
      const contribution = { id, title: definition.name, description: definition.description, inputSchema: definition.parameters, readOnly: definition.readOnly === true };
      if (definition.timeoutMs !== undefined) contribution.timeoutMs = timeoutOf(definition.timeoutMs);
      return this.ctx.effect(() => {
        tools.set(definition.name, definition);
        const release = catalog.register('tools', contribution, () => api.tool(id, async (input, execContext) => {
          const pipeline = new ToolExecutionPipeline(ctx, { get: (name) => tools.get(name) });
          const prepared = await pipeline.prepare({ callId: execContext.invocationId, name: definition.name, arguments: input, signal: execContext.signal });
          const outcome = prepared.kind === 'dispatch' ? await pipeline.dispatch(prepared.exec) : prepared;
          const result = await pipeline.complete(prepared.exec, outcome);
          if (result.additionalContexts?.length || result.concludeTurn) throw new Error('此工具要求追加会话上下文或结束 Turn，当前公开合同尚未承载');
          if (result.isError) throw new Error(result.content?.filter((part) => part.type === 'text').map((part) => part.text).join('\n') || 'DSH 工具执行失败');
          return result;
        }));
        this.ctx.emit('tools/change');
        return async () => { if (tools.get(definition.name) === definition) tools.delete(definition.name); await release(); this.ctx.emit('tools/change'); };
      });
    }
    get(name) { return tools.get(name); }
    executable(name) { return tools.get(name); }
    definitions() { return [...tools.values()]; }
    schemas() { return this.definitions().map(({ name, description, parameters }) => clone({ name, description, parameters })); }
    guard(guard) { return this.ctx.effect(() => { guards.add(guard); return () => guards.delete(guard); }); }
    checkGuards(exec) {
      for (const guard of [...guards]) {
        const result = guard(exec);
        if (result?.then) throw new Error('DSH guard 必须同步返回');
        if (result?.reason) return result.reason;
      }
    }
  });

  await ctx.plugin(class extends Service {
    constructor(c) { super(c, 'commands'); }
    register(definition) {
      const id = runtimeId('cmd', definition.name);
      return registration(this.ctx, 'commands', { id, title: definition.name, description: definition.description ?? '' }, async (input, execContext) => {
        const result = await definition.handler({ commandId: execContext.invocationId, rawInput: typeof input?.text === 'string' ? input.text : '', attachments: [], signal: execContext.signal,
          get agent() { throw new Error('该命令依赖 DSH Agent；会话操作尚未映射到 Magi 公开接口'); },
        });
        if (result?.kind === 'error') throw new Error(result.text);
        if (typeof result?.text !== 'string') throw new Error('DSH 命令必须返回带 text 的结果');
        return { content: result.text };
      });
    }
  });

  await ctx.plugin(class extends Service {
    constructor(c) { super(c, 'webServer'); }
    register(definition) {
      const kind = definition.kind ?? 'exact';
      if (!['exact', 'prefix'].includes(kind) || typeof definition.path !== 'string' || !definition.path.startsWith('/')) throw new Error('不支持的 DSH 路由声明');
      if (typeof definition.handler !== 'function' && typeof definition.resource !== 'function') throw new Error('路由缺少处理器');
      const key = `${kind}:${definition.path}`;
      if (routes.has(key)) throw new Error(`路由重复：${definition.path}`);
      return this.ctx.effect(() => {
        routes.add(key);
        let release;
        catalog.enqueue(async () => { release = await api.route(definition.path, (input, context) => runRoute(definition, input, context), { match: kind }); }).catch(() => {});
        return () => { routes.delete(key); return catalog.enqueue(async () => release?.()); };
      });
    }
  });

  const checkConfig = (raw) => {
    if (fiber) return fiber.resolveConfig(clone(raw));
    if (!schema) return clone(raw);
    const result = schema['~standard'].validate(clone(raw));
    if (result?.then) throw new Error('DSH 配置不支持异步 schema');
    if (result.issues) throw new Error(result.issues.map((issue) => issue.message).join('; '));
    return result.value;
  };
  const applyConfig = async (snapshot) => {
    if (snapshot.version <= version || stopped) return;
    const next = checkConfig(snapshot.value);
    if (fiber) {
      const current = fiber.effectiveConfig;
      if (deepEqual(current, next, true)) {
        fiber.rawConfig = clone(snapshot.value);
        const changed = [];
        for (const { path, ref } of volatileEntries(current)) {
          const source = path.reduce((value, key) => value[key], next);
          if (!deepEqual(ref.get(), source.get(), true)) { updateVolatile(ref, source); changed.push(path); }
        }
        if (changed.length) {
          // 与上游 loader 一致：只通知拥有这些 Volatile 引用的 fiber。
          const subject = Object.create(fiber.ctx);
          subject[Context.filter] = (owner) => owner.fiber === fiber.ctx.fiber;
          try { fiber.ctx.emit(subject, 'loader/volatile-update', changed); }
          catch (error) { diagnostic(`配置更新监听失败: ${error?.message ?? error}`); }
        }
      } else {
        await fiber.update(clone(snapshot.value), true);
        await fiber.await();
      }
    }
    rawConfig = clone(snapshot.value); version = snapshot.version;
    await catalog.drain();
  };
  const refresh = (snapshot) => {
    const operation = configUpdates.then(() => applyConfig(snapshot));
    configUpdates = operation.catch(() => {});
    return operation.then((result) => { ctx.emit('settings/updated', options.plugin, clone(snapshot.value)); return result; });
  };
  const writeSettings = async (producer) => {
    const current = await api.settings.read();
    const next = producer(clone(current.value));
    checkConfig(next);
    await refresh(await api.settings.write(current.version, next));
  };
  await ctx.plugin(class extends Service {
    constructor(c) { super(c, 'settings'); }
    configure() { return this.ctx.effect(() => () => {}); }
    describe() { return [{ ns: options.plugin, value: clone(rawConfig) }]; }
    update(ns, patch) { this.check(ns); return writeSettings((value) => ({ ...value, ...patch })); }
    replace(ns, value) { this.check(ns); return writeSettings(() => value); }
    mutate(ns, operations) {
      this.check(ns);
      return writeSettings((value) => {
        for (const op of operations) {
          if (!Array.isArray(op.path) || !op.path.length || op.path.some((key) => ['__proto__', 'prototype', 'constructor'].includes(key))) throw new Error('配置修改路径无效');
          let cursor = value;
          for (const key of op.path.slice(0, -1)) cursor = cursor[key] ??= {};
          if (op.op === 'set') cursor[op.path.at(-1)] = op.value;
          else if (op.op === 'delete') delete cursor[op.path.at(-1)];
          else throw new Error(`不支持的设置操作 ${op.op}`);
        }
        return value;
      });
    }
    check(ns) { if (ns !== options.plugin) throw new Error(`不能修改其他插件的配置：${ns}`); }
  });
  await ctx.plugin(class extends Service {
    constructor(c) { super(c, 'attachments'); }
    async saveImage(input) {
      if (!input || !Buffer.isBuffer(input.data) || typeof input.mediaType !== 'string') throw new Error('附件图片输入无效');
      const ref = { attachmentId: `blob-${Math.random().toString(36).slice(2)}`, mediaType: input.mediaType, bytes: input.data.length };
      savedImages.set(ref.attachmentId, { ref, data: Buffer.from(input.data) });
      if (api.blob?.put) {
        const stored = await api.blob.put({ dataBase64: input.data.toString('base64'), mediaType: input.mediaType }, input.mediaType);
        if (stored?.ref) ref.attachmentId = stored.ref;
      }
      return ref;
    }
    async readImage(ref, signal) {
      const local = savedImages.get(ref?.attachmentId);
      if (local) return { ref: local.ref, data: Buffer.from(local.data) };
      const blob = await api.blob?.get?.(ref?.attachmentId);
      if (!blob?.value?.dataBase64) throw new Error('图片引用不存在或已过期');
      signal?.throwIfAborted?.();
      return { ref, data: Buffer.from(blob.value.dataBase64, 'base64') };
    }
    readImageRequest(...args) { return requestAttachments.readImageRequest(...args); }
    imageHostPath() { return undefined; }
    fileHostPath() { return undefined; }
  });
  await ctx.plugin(LlmRuntime);

  // Several widely used DSH plugins probe optional host services before
  // deciding whether to show their panel. Keep those probes harmless and
  // deterministic: the daemon remains the authority, while compatibility
  // code sees the same empty/read-only shape it would see on a DSH profile
  // without that subsystem.
  ctx.provide('connection', { requestRejection: () => undefined });
  ctx.provide('pluginManager', { list: () => [], entries: () => [] });
  ctx.provide('loader', { entries: () => [] });
  ctx.provide('sessions', { list: { getSnapshot: () => ({ sessions: [] }), subscribe: () => () => {} }, scope: () => ({ getSnapshot: () => ({}), subscribe: () => () => {} }) });
  ctx.baseUrl = 'http://127.0.0.1';

  let publishing;
  let modelRevision = 0;
  let refreshPending = false;
  const modelLookups = new AbortController();
  const lookup = (work) => new Promise((resolve, reject) => {
    const signal = modelLookups.signal;
    const abort = () => reject(signal.reason);
    if (signal.aborted) return abort();
    signal.addEventListener('abort', abort, { once: true });
    Promise.resolve(work).then(resolve, reject).finally(() => signal.removeEventListener('abort', abort));
  });
  const publish = async (revision) => {
    const obsolete = () => stopped || revision !== modelRevision;
    const active = new Set(ctx.llm.listProviders().map((provider) => provider.id));
    for (const [provider, release] of sourceReleases) {
      if (active.has(provider)) continue;
      await api.modelSource.clear(runtimeId('model', provider));
      await release(); sourceReleases.delete(provider); published.delete(provider);
      await bridges.get(provider)?.close(); bridges.delete(provider);
      if (obsolete()) return;
    }
    for (const provider of active) {
      if (obsolete()) return;
      const sourceId = runtimeId('model', provider);
      if (!sourceReleases.has(provider)) {
        const release = catalog.register('modelSources', { id: sourceId, title: provider, description: '' }, async () => undefined);
        sourceReleases.set(provider, release);
        await catalog.drain();
      }
      if (obsolete()) return;
      const models = await lookup(ctx.llm.listModels(provider));
      if (obsolete()) return;
      const entries = [];
      for (const model of models) {
        const info = await lookup(ctx.llm.resolveModelInfo(provider, model.id, modelLookups.signal));
        if (obsolete()) return;
        const entry = { id: model.id, displayName: model.name };
        if (info.context?.contextWindow) entry.contextWindowTokens = info.context.contextWindow;
        if (info.inputModalities) entry.inputModalities = [...info.inputModalities];
        if (info.reasoning?.efforts?.length) entry.reasoning = reasoningOf(info.reasoning.efforts.map((effort) => String(effort.id)), info.reasoning.defaultEffort);
        entries.push(entry);
      }
      const fingerprint = JSON.stringify(entries);
      if (published.get(provider) === fingerprint) continue;
      if (!entries.length) { await api.modelSource.clear(sourceId); published.set(provider, fingerprint); continue; }
      if (!bridges.has(provider)) bridges.set(provider, await startOpenAiBridge({ llm: ctx.llm, provider, log: diagnostic, saveImage: saveRequestImage, withRequest: withRequestImages }));
      if (obsolete()) return;
      const bridge = bridges.get(provider);
      await api.modelSource.set(sourceId, { baseUrl: bridge.baseUrl, apiKey: bridge.apiKey, apiProtocol: 'openai_chat', models: entries });
      published.set(provider, fingerprint);
    }
  };
  const refreshModels = (invalidate = true) => {
    if (stopped) return Promise.resolve();
    if (publishing && !invalidate) return publishing;
    modelRevision++;
    refreshPending = true;
    if (publishing) return publishing;
    publishing = (async () => {
      do {
        refreshPending = false;
        const revision = modelRevision;
        try { await publish(revision); }
        catch (error) { if (!stopped && revision === modelRevision) throw error; }
      } while (refreshPending && !stopped);
    })().finally(() => { publishing = null; });
    return publishing;
  };
  const modelsChanged = () => {
    if (stopped) return;
    if (catalog.live) void refreshModels().catch(reportFailure);
    else { modelRevision++; refreshPending = true; }
  };
  ctx.on('llm/adapters-updated', modelsChanged);
  ctx.on('llm/models-updated', modelsChanged);

  const dispose = async () => {
    if (stopped) return;
    stopped = true; catalog.live = false;
    clearInterval(catalogTimer);
    modelLookups.abort(new Error('扩展已卸载'));
    await publishing?.catch(() => {});
    await configUpdates;
    // 停用时 daemon 可能已经关闭写入准入；一个 RPC 失败不能阻止本地端口和 effect 回收。
    const cleanup = await Promise.allSettled([
      ctx.fiber.dispose(),
      ...[...sourceReleases].map(async ([provider, release]) => {
        try { await api.modelSource.clear(runtimeId('model', provider)); }
        finally { await release(); }
      }),
      ...[...bridges.values()].map((bridge) => bridge.close()),
    ]);
    for (const result of cleanup) if (result.status === 'rejected') diagnostic(`卸载回收: ${result.reason?.message ?? result.reason}`);
    sourceReleases.clear(); bridges.clear(); published.clear();
  };
  api.onDispose(dispose);
  try {
    await mountRemote(api, ctx);
    const settings = await api.settings.read();
    rawConfig = clone(settings.value); version = settings.version;
    fiber = ctx.plugin(upstream, clone(rawConfig));
    await fiber.await();
    // Cordis 可把缺依赖的插件留在 PENDING；Magi 不能把它报告为 running。
    for (const runtime of ctx.registry.values()) for (const child of runtime.fibers) {
      await child.await();
      if (child.state === 0) {
        if (!conditionalFibers.has(child.uid)) throw new Error(`DSH 插件 ${runtime.name ?? '(匿名)'} 缺少必需服务：${Object.keys(child.inject).join(', ')}`);
        diagnostic(`DSH 可选注入未挂载，宿主缺少服务：${Object.keys(child.inject).join(', ')}`);
      }
    }
    await catalog.drain();
    await refreshModels();
    await catalog.drain();
    api.settings.onValidate(checkConfig);
    api.onConfig(refresh);
    catalog.live = true;
    catalogTimer = setInterval(() => { void refreshModels(false).catch(reportFailure); }, 3000);
    catalogTimer.unref();
    return { dispose };
  } catch (error) { await dispose(); throw error; }
}

const EFFORT_TO_MAGI = { off: 'none', none: 'none', minimal: 'minimal', low: 'low', medium: 'medium', high: 'high', xhigh: 'xhigh', max: 'max', ultra: 'ultra' };
function reasoningOf(efforts, defaultEffort) {
  const mapped = efforts.map((effort) => {
    const value = EFFORT_TO_MAGI[effort];
    if (!value) throw new Error(`推理档位 ${effort} 无法映射到 Magi 的档位枚举`);
    return value;
  });
  const declared = { supportedEfforts: mapped, transport: 'openai', source: 'dsh' };
  if (defaultEffort !== undefined) {
    if (!EFFORT_TO_MAGI[String(defaultEffort)]) throw new Error(`默认推理档位 ${defaultEffort} 无法映射`);
    declared.defaultEffort = EFFORT_TO_MAGI[String(defaultEffort)];
  }
  return declared;
}
