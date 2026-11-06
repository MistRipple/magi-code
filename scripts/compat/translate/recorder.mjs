// 记录进程：加载上游原入口，用桩服务运行 apply，把注册输出为 JSON。不联网、不执行注册之外的动作。
// 运行在 `node --permission` 受限模式下（见 record.mjs）：不能写用户目录、不能起子进程、不能联网。
import { pathToFileURL } from 'node:url';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { installSharedResolution } from '../runtime/shared-resolution.mjs';

const [entry, overridesPath, sharedRoot, resultPath] = process.argv.slice(2);
// 上游常把运行状态写进 ~/.dsh；HOME 是记录用的空临时目录，预建它即可，不触碰真实用户目录。
mkdirSync(`${process.env.HOME}/.dsh`, { recursive: true });
installSharedResolution(sharedRoot);
const { Context, Service } = await import('@deepseek-ai/cordis');
const { LlmRuntime } = await import('@deepseek-ai/dsh-llm');
const upstream = await import(pathToFileURL(entry).href);

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
await new Promise((r) => setTimeout(r, 0));
const llm = ctx.llm;
const originalAdapter = llm.registerAdapter.bind(llm);
llm.registerAdapter = (providers, adapter) => { for (const id of providers) out.providers.push({ id }); return originalAdapter(providers, adapter); };

const overrides = JSON.parse(readFileSync(overridesPath, 'utf8'));
const schema = upstream.Config;
if (schema) out.configSchema = schema.toJSON();
const config = schema ? schema(overrides) : overrides;
ctx.plugin({ apply: upstream.apply, inject: upstream.inject, name: upstream.name }, config);
await new Promise((r) => setTimeout(r, 1500));
out.listeners = [...listenerNames].sort();
// 结果写文件而不是 stdout：上游常用 console.log 打印日志，会混进输出。
writeFileSync(resultPath, JSON.stringify(out));
process.exit(0);
