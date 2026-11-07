// 记录进程：加载上游原入口，用桩服务运行 apply，把注册输出为 JSON。不联网、不执行注册之外的动作。
// 运行在 `node --permission` 受限模式下（见 record.mjs）：不能写用户目录、不能起子进程、不能联网。
import { pathToFileURL } from 'node:url';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { configureProfile, withProfile, loaderEntries, profilePath } from '../runtime/profile.mjs';
import { installSharedResolution } from '../runtime/shared-resolution.mjs';
import { dshEnvironment } from '../core/dsh-compatibility.mjs';
import { createBackendSessions } from '../runtime/backend-sessions.mjs';

const [entry, overridesPath, sharedRoot, resultPath, upstreamDir, readSessionDirectory] = process.argv.slice(2);
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

const out = { name: upstream.name ?? null, inject: upstream.inject ?? [], tools: [], commands: [], providers: [], routes: [], listeners: [], configSchema: null };
const ctx = new Context();
// 只提供真实服务的命名空间；记录器没有 daemon 会话事实，调用读取即拒绝。
if (readSessionDirectory === '1') ctx.provide('sessions', createBackendSessions({}).service);
ctx.provide('loader', {entries:loaderEntries});
ctx.provide('profileContext', Object.freeze({ name: 'magi', dir: profilePath() }));
ctx.baseUrl = pathToFileURL(profilePath() + '/').href;
// 记录上游监听的事件名：拦截类事件要在清单里静态声明为 Magi 钩子，运行期才出现的监听会被明确拒绝。
// 通过 Cordis 自带的 `internal/listener` 事件观察，不改写 ctx.on，也不执行监听器。
const listenerNames = new Set();
ctx.on('internal/listener', (name) => { if (typeof name === 'string' && !name.startsWith('internal/')) listenerNames.add(name); });
ctx.plugin(LlmRuntime);
const stub = (name, register) => ctx.plugin(class extends Service { constructor(c) { super(c, name); } register(definition) { return register(definition); } });
stub('tools', (d) => { out.tools.push({ id: d.name, description: d.description, parameters: d.parameters, readOnly: d.readOnly === true, timeoutMs: d.timeoutMs ?? null }); return () => {}; });
stub('commands', (d) => { out.commands.push({ id: d.name, description: d.description, input: d.input ?? null }); return () => {}; });
stub('webServer', (d) => {
  if (d.kind !== undefined && d.kind !== 'exact' && d.kind !== 'prefix') throw new Error(`路由类型 ${d.kind} 不受支持（只支持 exact 与 prefix）`);
  out.routes.push({ kind: d.kind === 'prefix' ? 'prefix' : 'exact', path: d.path });
  return () => {};
});
ctx.plugin(class extends Service { constructor(c) { super(c, 'settings'); } configure() { return () => {}; } async update() {} async replace() {} async mutate() {} });
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

const overrides = JSON.parse(readFileSync(overridesPath, 'utf8'));
const schema = upstream.Config;
if (schema) out.configSchema = schema.toJSON();
const config = schema ? schema(overrides) : overrides;
// 启动必须是记录合同的一部分。此前只调用 ctx.plugin 而不等待 fiber，Cordis
// 会把 apply() 的异常变成后台失败，记录器仍然输出“成功”的空清单，最终导致
// 一个永远无法激活的翻译包通过安装校验。
let fiber, startup;
const settleTimeout = Number(process.env.MAGI_RECORD_START_TIMEOUT_MS ?? 15_000);
await withProfile(async () => {
fiber = ctx.plugin({ apply: upstream.apply, inject: upstream.inject, name: upstream.name }, config);
startup = await Promise.race([
  fiber.await().then(() => 'ready'),
  new Promise((resolve) => setTimeout(() => resolve('timeout'), settleTimeout)),
]);
});
if (startup === 'timeout') console.error(`记录阶段在 ${settleTimeout}ms 内未完全就绪，保留已观察的静态注册并交由 Magi 运行期继续收敛`);
// 贡献可以在 apply 返回后由上游异步注册（例如 MCP 发现或延迟工具）。给它一个
// 有界的稳定窗口；运行期目录仍然支持动态增删，但安装期不能把已观察到的启动
// 失败隐藏成成功。
await new Promise((r) => setTimeout(r, Number(process.env.MAGI_RECORD_SETTLE_MS ?? 3000)));
if (startup === 'ready') await fiber.await();
out.listeners = [...listenerNames].sort();
// 结果写文件而不是 stdout：上游常用 console.log 打印日志，会混进输出。
writeFileSync(resultPath, JSON.stringify(out));
process.exit(0);
