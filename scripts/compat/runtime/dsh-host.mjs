// DSH 运行期适配器。上游原包执行真实 Cordis；业务与贡献准入由 Magi daemon 裁决。
import { installSkills } from './skills.mjs';
import { Context, Service } from '@deepseek-ai/cordis';
import { pathToFileURL } from 'node:url';
import { deepEqual, volatileEntries, updateVolatile } from '@deepseek-ai/cosmokit';
import { LlmRuntime } from '@deepseek-ai/dsh-llm';
import { ToolExecutionPipeline } from '@deepseek-ai/dsh-tools/pipeline';
import { runtimeId } from '../core/identity.mjs';
import { toJsonSchema } from './settings-schema.mjs';
import { RuntimeCatalog } from './catalog.mjs';
import { runRoute } from './routes.mjs';
import { createRouteStreams, STREAM_ROUTE } from './route-streams.mjs';
import { startOpenAiBridge } from './openai-bridge.mjs';
import { requestAttachments, saveRequestImage, withRequestImages } from './attachments.mjs';
import { mountRemote } from './remote.mjs';
import { createPluginManager } from './plugin-manager.mjs';
import { configureProfile, withProfile as runProfile, loaderEntries, profilePath, createProfileReload } from './profile.mjs';
import { createBackendSessions } from './backend-sessions.mjs';
import { createRegisteredSettings } from './registered-settings.mjs';
import { installRegistryFetch } from './registry-fetch.mjs';
import { createPluginDirectory } from './plugin-directory.mjs';
import { profilePackageManager } from './profile-process-host.mjs';
import { withInvocation, invocationAgent } from './invocation.mjs';

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
  let releaseRegistryFetch = () => {};
  const releaseProfile = configureProfile(api);
  const sessions = options.readSessionDirectory ? createBackendSessions(api) : undefined;
  const ctx = new Context();
  const registeredSettings = createRegisteredSettings(api, options.settingsNamespaces, (ns, value) => ctx.emit('settings/updated', ns, value));
  const withProfile = (callback, signal) => registeredSettings.run(() => sessions
    ? sessions.run(() => runProfile(callback, signal), signal)
    : runProfile(callback, signal));
  const registerRoute = (path, handler, options) => api.route(path,
    (input, context) => withProfile(() => handler(input, context), context.signal), options);
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
  const routes = new Map();
  const routeStreams = createRouteStreams((path) => routes.get(`exact:${path}`)
    ?? [...routes].filter(([key]) => key.startsWith('prefix:') && path.startsWith(key.slice(7))).sort(([a], [b]) => b.length - a.length)[0]?.[1]);
  const bridges = new Map();
  const sourceReleases = new Map();
  const published = new Map();
  let stopped = false;
  let fiber;
  let version = -1;
  let rawConfig = {};
  let catalogTimer;
  let configUpdates = Promise.resolve();
  const diagnostic = (message) => api.log(String(message));
  const pluginDirectory = createPluginDirectory(api, diagnostic);
  // 全局 internal/status 是跨插件的只读观察；真实本地 fiber 仍由 Cordis 派发。
  // 外部状态只交给观察者，不进入本实例的启动、失败或处置流程。
  ctx.on('internal/listener', function (name, listener, eventOptions) {
    if (name !== 'internal/status' || !eventOptions.global) return;
    const local = this.events.register('ctx.on("internal/status")', name, listener, eventOptions);
    const external = this.effect(() => pluginDirectory.subscribe(listener));
    return () => { external(); return local(); };
  });
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
        const release = catalog.register('tools', contribution, () => api.tool(id, (input, execContext) => withInvocation(execContext, () => withProfile(async () => {
          const pipeline = new ToolExecutionPipeline(ctx, { get: (name) => tools.get(name) });
          const agent = invocationAgent();
          const prepared = await pipeline.prepare({ callId: execContext.invocationId, name: definition.name, arguments: input, signal: execContext.signal, ...(agent ? { agent } : {}) });
          const outcome = prepared.kind === 'dispatch' ? await pipeline.dispatch(prepared.exec) : prepared;
          return pipeline.complete(prepared.exec, outcome);
        }, execContext.signal), api, {
          // deferContext 与 Agent.inject 共用 daemon 的当前会话输入入口。
          // 失败管线的纠正上下文也交付；交付失败不能覆盖已执行的业务结果。
          contexts: result => result.additionalContexts ?? [],
          complete: (result, deliveryError) => api.toolResult(deliveryError ? {
            result, contextDelivery: { status: 'failed', error: String(deliveryError.message ?? deliveryError),
              recovery: 'Business execution has settled. Retry context delivery explicitly; do not repeat the tool business operation.' },
          } : result, {
            isError: result.isError || Boolean(deliveryError),
            ...(!deliveryError && result.concludesTurn === true ? { completion: result.content.filter(part => part.type === 'text').map(part => part.text).join('\n') || JSON.stringify(result.value) } : {}),
          }),
        })));
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
      const handler = async (input, execContext) => {
        const result = await definition.handler({ commandId: execContext.invocationId, rawInput: typeof input?.text === 'string' ? input.text : '', attachments: [], signal: execContext.signal,
          get agent() { return invocationAgent(); },
        });
        if (result?.kind === 'error') throw new Error(result.text);
        if (typeof result?.text !== 'string') throw new Error('DSH 命令必须返回带 text 的结果');
        return { content: result.text };
      };
      return this.ctx.effect(() => catalog.register('commands', { id, title: definition.name, description: definition.description ?? '' }, () =>
        api.command(id, (input, context) => withInvocation(context, () => withProfile(() => handler(input, context), context.signal), api))));
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
        if (definition.path === STREAM_ROUTE) throw new Error('插件路由使用了兼容层保留路径');
        routes.set(key, definition);
        let release;
        catalog.enqueue(async () => { release = await registerRoute(definition.path, (input, context) => runRoute(definition, input, context), { match: kind }); }).catch(() => {});
        return () => { routes.delete(key); routeStreams.closePath(definition.path); return catalog.enqueue(async () => release?.()); };
      });
    }
  });

  const releaseStreamRoute = await registerRoute(STREAM_ROUTE, async (input, context) => ({ body: await routeStreams.call(input.body, context) }));

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
    const operation = configUpdates.then(() => withProfile(() => applyConfig(snapshot)));
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
    register(ns, schema, presentation) { return registeredSettings.register(this.ctx, ns, schema, presentation); }
    get(ns) { return registeredSettings.get(ns); }
    update(ns, patch) {
      if (registeredSettings.has(ns)) return registeredSettings.update(ns, patch);
      this.check(ns); return writeSettings((value) => ({ ...value, ...clone(patch) }));
    }
    replace(ns, value) {
      if (registeredSettings.has(ns)) return registeredSettings.replace(ns, value);
      this.check(ns); return writeSettings(() => clone(value));
    }
    mutate(ns, operations) {
      if (registeredSettings.has(ns)) return registeredSettings.mutate(ns, operations);
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
      if (!api.images?.save) throw new Error('daemon 未提供持久图片附件服务');
      const stored = await api.images.save(input);
      if (!stored?.attachmentId) throw new Error('daemon 未返回已保存附件身份');
      return stored;
    }
    async readImage(ref, signal) {
      signal?.throwIfAborted?.();
      if (!api.images?.read) throw new Error('daemon 未提供持久图片附件服务');
      const image = await api.images.read(ref?.attachmentId);
      signal?.throwIfAborted?.();
      return image;
    }
    readImageRequest(...args) { return requestAttachments.readImageRequest(...args); }
    imageHostPath() { return undefined; }
    fileHostPath() { return undefined; }
  });
  await installSkills(ctx, skill => catalog.register('skills', skill));
  await ctx.plugin(LlmRuntime);

  // 路由在 daemon 已完成插件准入后才到达此连接。插件目录每次读 daemon，
  // 会话服务仅在安装包请求独立目录授权时出现；可选未支持分支继续缺席。
  ctx.provide('connection', { requestRejection: () => undefined });
  if (sessions) ctx.provide('sessions', sessions.service);
  if (api.plugins?.list) ctx.provide('pluginManager', createPluginManager(api));
  if (api.plugins?.list && profilePath()) {
    ctx.provide('loader', { entries: loaderEntries });
    ctx.provide('hmr', createProfileReload());
    ctx.provide('profileContext', Object.freeze({ name: 'magi', dir: profilePath(), packageManager: profilePackageManager() }));
  }
  // Loader 的 baseUrl 是当前 profile 目录，用于上游识别运行身份；不是 HTTP 地址。
  if (profilePath()) ctx.baseUrl = pathToFileURL(profilePath() + '/').href;

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
    pluginDirectory.dispose();
    releaseRegistryFetch();
    routeStreams.dispose();
    clearInterval(catalogTimer);
    modelLookups.abort(new Error('扩展已卸载'));
    await publishing?.catch(() => {});
    await configUpdates;
    // 停用时 daemon 可能已经关闭写入准入；一个 RPC 失败不能阻止本地端口和 effect 回收。
    const cleanup = await Promise.allSettled([
      ctx.fiber.dispose(),
      releaseStreamRoute?.(),
      ...[...sourceReleases].map(async ([provider, release]) => {
        try { await api.modelSource.clear(runtimeId('model', provider)); }
        finally { await release(); }
      }),
      ...[...bridges.values()].map((bridge) => bridge.close()),
    ]);
    for (const result of cleanup) if (result.status === 'rejected') diagnostic(`卸载回收: ${result.reason?.message ?? result.reason}`);
    sourceReleases.clear(); bridges.clear(); published.clear();
    releaseProfile();
    sessions?.dispose();
    await registeredSettings.dispose();
  };
  api.onDispose(dispose);
  try {
    releaseRegistryFetch = installRegistryFetch();
    await mountRemote({ route: registerRoute }, ctx);
    const settings = await api.settings.read();
    rawConfig = clone(settings.value); version = settings.version;
    await withProfile(async () => {
      fiber = ctx.plugin(upstream, clone(rawConfig));
      await fiber.await();
    });
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
    await pluginDirectory.refresh();
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
