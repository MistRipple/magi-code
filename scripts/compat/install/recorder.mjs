// 记录进程：加载上游原入口，用桩服务运行 apply，把注册输出为 JSON。不联网、不执行注册之外的动作。
// 运行在 `node --permission` 受限模式下（见 record.mjs）：不能写用户目录、不能起子进程、不能联网。
import { installSkills } from '../runtime/skills.mjs';
import { installPromptContributions } from '../runtime/prompt.mjs';
import { pathToFileURL } from 'node:url';
import { createRequire, syncBuiltinESMExports } from 'node:module';
import { promisify } from 'node:util';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { configureProfile, withProfile, loaderEntries, profilePath, createProfileReload } from '../runtime/profile.mjs';
import { installSharedResolution } from '../runtime/shared-resolution.mjs';
import { dshEnvironment } from '../core/dsh-compatibility.mjs';
import { createBackendSessions } from '../runtime/backend-sessions.mjs';
import { profilePackageManager } from '../runtime/profile-process-host.mjs';
import { installNullableEvents } from '../runtime/event-subscriptions.mjs';
import { installWeb } from '../runtime/web.mjs';
import { createRegisteredSettings } from '../runtime/registered-settings.mjs';

const [entry, sharedRoot, resultPath, upstreamDir, readSessionDirectory, nullableEvents, webProviders, settingsNamespace] = process.argv.slice(2);
// 观察上游创建的定时器，用于判断启动后是否还有会触发的一次性工作。必须先于上游入口加载。
// 只登记不改变语义：回调、返回值、清除与 promisify 行为都保持原样。
const scheduled = [];
const nativeSetTimeout = globalThis.setTimeout;
{
  const require = createRequire(import.meta.url);
  const timers = require('node:timers'), timersPromises = require('node:timers/promises');
  const delayOf = (value) => Math.max(1, Number(value) || 1);
  const timerWrapper = (original, periodic) => {
    const tracked = function (...args) {
      const timer = original.apply(this, args);
      scheduled.push({ timer, periodic, dueAt: Date.now() + delayOf(args[1]) });
      return timer;
    };
    // promisify.custom 由下面单独接管；其余属性（含 length/name）原样复制。
    const { [promisify.custom]: custom, ...descriptors } = Object.getOwnPropertyDescriptors(original);
    Object.defineProperties(tracked, descriptors);
    if (custom) Object.defineProperty(tracked, promisify.custom, { ...custom, configurable: true });
    return tracked;
  };
  const trackedTimeout = timerWrapper(globalThis.setTimeout, false), trackedInterval = timerWrapper(globalThis.setInterval, true);
  // timers/promises 的 setTimeout 返回 Promise；以其落定作为“已触发或已取消”。
  const original = timersPromises.setTimeout;
  const trackedSleep = (delay, ...rest) => {
    const record = { done: false, periodic: false, dueAt: Date.now() + delayOf(delay), ref: rest[1]?.ref !== false };
    scheduled.push(record);
    const promise = original(delay, ...rest);
    promise.then(() => { record.done = true; }, () => { record.done = true; });
    return promise;
  };
  globalThis.setTimeout = timers.setTimeout = trackedTimeout;
  globalThis.setInterval = timers.setInterval = trackedInterval;
  timersPromises.setTimeout = trackedSleep;
  Object.defineProperty(trackedTimeout, promisify.custom, { value: trackedSleep, configurable: true });
  syncBuiltinESMExports();
}
// 上游常把运行状态写进 ~/.dsh；HOME 是记录用的空临时目录，预建它即可，不触碰真实用户目录。
mkdirSync(`${process.env.HOME}/.dsh`, { recursive: true });
installSharedResolution(sharedRoot, { '@magi/compat-upstream-root': upstreamDir + '/' });
const { Context, Service } = await import('@deepseek-ai/cordis');
const { LlmRuntime } = await import('@deepseek-ai/dsh-llm');
const metadata = JSON.parse(readFileSync(new URL('package.json', pathToFileURL(upstreamDir + '/')), 'utf8'));
configureProfile({ plugins: {
  async list() { return { plugins: [{manifest: {id:'recording.plugin', description:metadata.description}, upstreamManifest:metadata, digest:'recording', source:{kind:'upstream', location:`dsh:${metadata.name}@${metadata.version}`}, enabled:true, active:false}] }; },
  async compatibility() { return { ...dshEnvironment(sharedRoot), revision: 0, exemptions: {} }; },
} }, { home: process.env.HOME + '/.dsh' });
const upstream = await withProfile(() => import(pathToFileURL(entry).href));

// 记录进程由 Node 权限模型限制：没有网络、子进程、Worker、原生插件，也不能写临时目录以外的位置。
// 上游 apply 常在本机起监听端点（只为之后接收请求）。记录阶段不接收任何请求，因此把监听替换成
// “假装已监听”：对象状态与事件齐全，但不绑定端口。任何真正的出站连接仍由权限模型拒绝。
const net = await import('node:net');
net.Server.prototype.listen = function listen(...args) {
  const callback = args.find((arg) => typeof arg === 'function');
  const options = typeof args[0] === 'object' && args[0] !== null ? args[0] : { port: args[0], host: typeof args[1] === 'string' ? args[1] : undefined };
  const address = { address: options.host || '127.0.0.1', family: 'IPv4', port: Number(options.port) || 49152 };
  this.address = () => address;
  Object.defineProperty(this, 'listening', { value: true, configurable: true });
  process.nextTick(() => { callback?.call(this); this.emit('listening'); });
  return this;
};
net.Server.prototype.close = function close(callback) {
  Object.defineProperty(this, 'listening', { value: false, configurable: true });
  process.nextTick(() => { callback?.call(this); this.emit('close'); });
  return this;
};

// 记录阶段不发送任何请求：上游对本机端点的健康探测、模型发现等一律明确失败（上游自己处理失败路径）。
globalThis.fetch = (input) => Promise.reject(new Error(`构建期记录不发送请求：${typeof input === 'string' ? input : (input.url ?? String(input))}`));

const out = { tools: [], commands: [], providers: [], routes: [], listeners: [], settingsNamespaces: [], profilePackageManager: false };
const ctx = new Context();
// 记录器只拥有临时观察值；与运行期共用 schema、base、句柄及订阅语义，
// 不以简化设置替身掩盖启动时实际调用的成员。
let recordedSettings = { version: 0, value: null };
const registeredSettings = createRegisteredSettings({ resource: {
  async read() { return structuredClone(recordedSettings); },
  async write(_id, version, value) {
    if (version !== recordedSettings.version) throw new Error('记录阶段设置版本冲突');
    return recordedSettings = { version: version + 1, value: structuredClone(value) };
  } }, log: message => console.error(message) }, true);
// 只提供真实服务的命名空间；记录器没有 daemon 会话事实，调用读取即拒绝。
if (readSessionDirectory === '1') ctx.provide('sessions', createBackendSessions({}).service);
ctx.provide('loader', {entries:loaderEntries});
ctx.provide('hmr', createProfileReload());
ctx.provide('profileContext', Object.freeze({ name: 'magi', dir: profilePath(), get packageManager() { out.profilePackageManager = true; return profilePackageManager(); } }));
ctx.baseUrl = pathToFileURL(profilePath() + '/').href;
// 记录默认启动观察到的事件名，供兼容报告核对；不据此生成 Magi 钩子。
// 通过 Cordis 自带的 `internal/listener` 观察实际登记，不执行监听器。
const listenerNames = new Set();
installNullableEvents(ctx, JSON.parse(nullableEvents ?? '[]'));
ctx.on('internal/listener', (name) => { if (typeof name === 'string' && !name.startsWith('internal/')) listenerNames.add(name); });
await installSkills(ctx, () => () => {});
await installPromptContributions(ctx);
ctx.plugin(LlmRuntime);
const stub = (name, register) => ctx.plugin(class extends Service { constructor(c) { super(c, name); } register(definition) { return register(definition); } });
stub('tools', (d) => { out.tools.push({ id: d.name, description: d.description, parameters: d.parameters, readOnly: d.readOnly === true, timeoutMs: d.timeoutMs ?? null }); return () => {}; });
stub('commands', (d) => { out.commands.push({ id: d.name, description: d.description, input: d.input ?? null }); return () => {}; });
stub('webServer', (d) => {
  if (d.kind !== undefined && d.kind !== 'exact' && d.kind !== 'prefix') throw new Error(`路由类型 ${d.kind} 不受支持（只支持 exact 与 prefix）`);
  out.routes.push({ kind: d.kind === 'prefix' ? 'prefix' : 'exact', path: d.path });
  return () => {};
});
let recordedWeb;
if (webProviders === '1') {
  let snapshot = { revision: 0, search: null, fetch: null };
  recordedWeb = await installWeb(ctx, { web: {
    async read() { return structuredClone(snapshot); },
    async configure({ expectedRevision, changes }) {
      if (expectedRevision !== snapshot.revision) throw new Error('记录阶段联网配置冲突');
      for (const change of changes) snapshot[change.kind] = change.providerId ? { pluginId: 'recording.plugin', providerId: change.providerId, contributionId: change.contributionId } : null;
      snapshot.revision++; return structuredClone(snapshot);
    },
  }, tool() { return () => {}; } }, { register(_kind, definition) { out.tools.push(definition); return () => {}; }, async drain() {} }, ns => ctx.emit('settings/document-updated', ns));
}
ctx.plugin(class extends Service {
  constructor(c) { super(c, 'settings'); }
  describe() { return [{ ns: settingsNamespace || metadata.name, value: {} }, ...(recordedWeb ? [recordedWeb.describe()] : [])]; }
  configure() { return () => {}; }
  register(ns, schema, options) {
    if (typeof ns !== 'string' || !ns || ns.length > 128 || ['__proto__', 'prototype', 'constructor'].includes(ns) || typeof schema !== 'function') throw new Error('记录阶段设置注册无效');
    if (!out.settingsNamespaces.includes(ns)) out.settingsNamespaces.push(ns);
    return registeredSettings.register(this.ctx, ns, schema, options);
  }
  get(ns) { return registeredSettings.get(ns); }
  update(ns, patch) { return registeredSettings.update(ns, patch); }
  replace(ns, value) { return registeredSettings.replace(ns, value); }
  mutate(ns, operations) { return ns === 'web' && recordedWeb ? recordedWeb.mutate(operations) : registeredSettings.mutate(ns, operations); }
});
ctx.plugin(class extends Service {
  constructor(c) { super(c, 'attachments'); }
  saveImage() { throw new Error('记录阶段没有 daemon 附件存储，不能保存业务图片'); }
  readImage() { throw new Error('记录阶段没有 daemon 附件存储，不能读取业务图片'); }
  readImageRequest() { throw new Error('记录阶段没有模型图片请求'); }
});
await new Promise((r) => setTimeout(r, 0));
const llm = ctx.llm;
const originalAdapter = llm.registerAdapter.bind(llm);
llm.registerAdapter = (providers, adapter) => { for (const id of providers) out.providers.push({ id }); return originalAdapter(providers, adapter); };

const config = upstream.Config ? upstream.Config({}) : {};
// 启动必须是记录合同的一部分。此前只调用 ctx.plugin 而不等待 fiber，Cordis
// 会把 apply() 的异常变成后台失败，记录器仍然输出“成功”的空清单，最终导致
// 一个永远无法激活的翻译包通过安装校验。
let fiber, startup;
// 记录进程不继承环境变量，下列时间是固定的记录合同，不能由外部改写。
const settleTimeout = 15_000;
await withProfile(() => registeredSettings.run(async () => {
fiber = ctx.plugin({ apply: upstream.apply, inject: upstream.inject, name: upstream.name }, config);
startup = await Promise.race([
  fiber.await().then(() => 'ready'),
  new Promise((resolve) => setTimeout(() => resolve('timeout'), settleTimeout)),
]);
}));
if (startup === 'timeout') console.error(`记录阶段在 ${settleTimeout}ms 内未完全就绪，保留已观察的静态注册并交由 Magi 运行期继续收敛`);
// 贡献可以在 apply 返回后由上游异步注册（例如延迟工具）。记录进程没有网络、子进程和 Worker，
// 之后还能登记贡献的只有：尚未触发的一次性定时器、立即回调，以及周期定时器。
// 所以“没有未完成的一次性工作”就是真实的静止态，不依赖机器快慢；
// 周期定时器无法证明会停止，保留 3 秒观察并要求连续 1 秒没有新登记。
// 15 秒上限保证上游用很长的一次性定时器时也能结束；运行期目录仍然支持动态增删，
// 但安装期不能把已观察到的启动失败隐藏成成功。
const activity = () => JSON.stringify([out.tools.length, out.commands.length, out.providers.length, out.routes.length,
  out.settingsNamespaces.length, listenerNames.size, ctx.registry.size]);
const pendingWork = (now) => {
  let oneShot = 0, periodic = 0;
  for (let index = scheduled.length - 1; index >= 0; index--) {
    const record = scheduled[index];
    const live = record.timer ? !record.timer._destroyed && record.timer.hasRef() : !record.done && record.ref;
    if (!live) { scheduled.splice(index, 1); continue; }
    if (record.periodic) periodic++;
    // 只等待登记在观察范围内会触发的一次性定时器；更久之后才触发的由运行期继续发布。
    else if (record.dueAt <= now + HORIZON) oneShot++;
  }
  return { oneShot, periodic };
};
const HORIZON = 5000;
{
  const began = Date.now(), PERIODIC_MIN = 3000, QUIET = 1000, MAX_SETTLE = 15_000, IDLE_POLLS = 5;
  let last = activity(), changedAt = began, idle = 0;
  for (;;) {
    await new Promise((r) => nativeSetTimeout(r, 20));
    const now = Date.now(), current = activity(), { oneShot, periodic } = pendingWork(now);
    if (current !== last) { last = current; changedAt = now; }
    // 文件请求等在线程池里完成，不是定时器；连续多次空闲后才认定静止。
    idle = oneShot === 0 ? idle + 1 : 0;
    if (now - began >= MAX_SETTLE) break;
    if (idle >= IDLE_POLLS && (periodic === 0 || (now - began >= PERIODIC_MIN && now - changedAt >= QUIET))) break;
  }
}
if (startup === 'ready') await fiber.await();
// 条件注入有自己的 fiber；根 apply 成功不能掩盖已观察到的子模块失败。
for (const runtime of ctx.registry.values()) for (const child of runtime.fibers) {
  if (child.state === 3) await child.await();
}
await recordedWeb?.drain();
out.listeners = [...listenerNames].sort();
// 结果写文件而不是 stdout：上游常用 console.log 打印日志，会混进输出。
writeFileSync(resultPath, JSON.stringify(out));
process.exit(0);
