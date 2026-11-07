import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import Schema from '@deepseek-ai/schemastery';
import { LlmAdapter } from '@deepseek-ai/dsh-llm';
import sharp from 'sharp';
import { activateDsh } from './runtime/dsh-host.mjs';
import { runtimeId } from './core/identity.mjs';
import { startOpenAiBridge } from './runtime/openai-bridge.mjs';
import { requestAttachments, saveRequestImage, withRequestImages } from './runtime/attachments.mjs';
import * as McpClient from '@deepseek-ai/dsh-mcp-client';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

// 只替代 daemon RPC；插件加载、依赖、effect、工具管线及模型目录运行真实上游代码。
function host(value = {}) {
  let revision = 0;
  let snapshot = { version: 0, value };
  let listener, validate;
  const state = { catalog: {}, tools: new Map(), commands: new Map(), routes: new Map(), models: new Map(), failures: [], logs: [] };
  const bind = (kind) => async (id, handler) => {
    assert.ok(state.catalog[kind].some((item) => item.id === id));
    state[kind].set(id, handler);
    return () => { if (state[kind].get(id) === handler) state[kind].delete(id); };
  };
  const api = {
    catalog: { async replace(expected, catalog) {
      assert.equal(expected, revision);
      state.catalog = structuredClone(catalog);
      return { revision: ++revision };
    } },
    tool: bind('tools'), command: bind('commands'),
    async route(path, handler) { state.routes.set(path, handler); return () => state.routes.delete(path); },
    modelSource: {
      async set(id, source) { state.models.set(id, source); },
      async clear(id) { state.models.delete(id); },
    },
    settings: {
      async read() { return structuredClone(snapshot); },
      async write(expected, value) {
        assert.equal(expected, snapshot.version);
        snapshot = { version: expected + 1, value: structuredClone(value) };
        return structuredClone(snapshot);
      },
      onValidate(handler) { validate = handler; },
    },
    onConfig(handler) { listener = handler; return () => { listener = undefined; }; },
    onDispose() {}, log(message) { state.logs.push(message); }, fail(error) { state.failures.push(error); },
  };
  return {
    api, state,
    async configure(value) {
      await validate(value);
      snapshot = { version: snapshot.version + 1, value };
      await listener(structuredClone(snapshot));
    },
  };
}

const options = { runtimeVersion: 2, plugin: 'fixture' };

test('DSH 插件目录读取 daemon 当前事实，保留作用域包名、版本和停用状态，不伪造空列表或写入口', async () => {
  const { createPluginManager } = await import('./runtime/plugin-manager.mjs');
  let enabled = true;
  const manager = createPluginManager({ plugins: { async list() { return { plugins: [
    { manifest: { name: 'Scoped plugin', description: 'description' }, source: { kind: 'upstream', location: 'dsh:@scope/sample@1.2.3' }, enabled, active: false },
    { manifest: { name: 'Native' }, source: { kind: 'local', location: 'native.zip' }, enabled: true, active: true },
  ] }; } } }, 'https://registry.npmjs.org');
  const first = await manager.listBundles();
  assert.equal(first.length, 1); assert.equal(first[0].name, '@scope/sample');
  assert.equal(first[0].removable, true, '原生管理入口可移除；DSH 写操作不会被伪造');
  assert.equal(first[0].version, '1.2.3'); assert.equal(first[0].enabled, true);
  enabled = false;
  assert.equal((await manager.listBundles())[0].enabled, false);
  assert.equal(manager.installBundle, undefined, '没有实施的写操作不冒充可用能力');
  assert.deepEqual(await manager.registries(), { registry: 'https://registry.npmjs.org' });
});

test('未接入的 loader 和 sessions 不再以空服务满足必需依赖', async (t) => {
  const h = host();
  const running = await activateDsh(h.api, { apply(ctx) {
    assert.equal(ctx.get('sessions', false), undefined);
    assert.equal(ctx.get('loader', false), undefined);
  } }, options);
  t.after(() => running.dispose());
  for (const dependency of ['loader', 'sessions']) await assert.rejects(activateDsh(host().api, { inject: [dependency], apply() { assert.fail('必需服务缺失不能启动'); } }, options), /缺少必需服务/);
});

test('DSH 管理写操作使用预览精确版本和摘要，不伪造新包授权，启停和移除映射真实 ID', async () => {
  const { createPluginManager, parseBundleSpec } = await import('./runtime/plugin-manager.mjs');
  const installed = { manifest: { id: 'translated.scoped', name: 'Scoped' }, source: { kind: 'upstream', location: 'dsh:@scope/sample@1.2.3' }, enabled: false, active: false };
  const calls = [];
  const plugins = {
    async list() { return { plugins: [installed] }; },
    async preview(input) { calls.push(['preview', input]); return { version: '1.2.3', candidate: { digest: 'confirmed' } }; },
    async install(input) { calls.push(['install', input]); return { plugins: [installed] }; },
    async setEnabled(id, enabled) { calls.push(['enabled', id, enabled]); if (enabled) throw Object.assign(new Error('缺少新版授权'), { kind: 'PermissionDenied' }); return { plugins: [installed] }; },
    async remove(id) { calls.push(['remove', id]); },
  };
  const manager = createPluginManager({ plugins });
  assert.deepEqual(parseBundleSpec('@scope/sample@latest'), { ecosystem: 'dsh', package: '@scope/sample' });
  assert.equal(parseBundleSpec('sample@^1').version, '^1');
  assert.equal(parseBundleSpec('sample@next').version, 'next');
  for (const bad of ['../sample', 'https://example.org/sample', 'sample@1.0.0;touch x', '@scope/sample@']) assert.throws(() => parseBundleSpec(bad));
  const result = await manager.installBundle('@scope/sample', { enabled: false });
  assert.equal(result.application, 'applied');
  assert.deepEqual(calls[1], ['install', { ecosystem: 'dsh', package: '@scope/sample', version: '1.2.3', expectedDigest: 'confirmed' }]);
  const unapproved = await manager.installBundle('@scope/sample');
  assert.equal(unapproved.application, 'failed');
  assert.equal(unapproved.installed, true); assert.equal(unapproved.error.code, 'authorization-required');
  assert.equal((await manager.setBundleEnabled('@scope/sample', false)).application, 'applied');
  assert.deepEqual(calls.at(-1), ['enabled', 'translated.scoped', false]);
  assert.equal((await manager.removeBundle('@scope/sample')).application, 'applied');
  assert.deepEqual(calls.at(-1), ['remove', 'translated.scoped']);
  assert.equal((await manager.installBundle('../sample')).application, 'failed');
});

test('DSH npm 标签和版本范围从 registry 解析为精确版本后才进入 daemon 预览，拒绝静默切换安装源', async (t) => {
  const { createServer } = await import('node:http');
  const { createPluginManager } = await import('./runtime/plugin-manager.mjs');
  const server = createServer((_req, res) => {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ name: 'fixture-range', 'dist-tags': { latest: '1.2.0', next: '2.0.0' }, versions: Object.fromEntries(['1.0.0', '1.2.0', '2.0.0'].map((version) => [version, { name: 'fixture-range', version, dist: { tarball: 'https://example.test/unused.tgz' } }])) }));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const versions = [];
  const manager = createPluginManager({ plugins: {
    async preview(input) { versions.push(input.version); return { version: input.version, candidate: { digest: 'confirmed' } }; },
    async install(input) { return { plugins: [{ manifest: { id: 'translated.range', name: 'Range' }, source: { location: `dsh:fixture-range@${input.version}` }, active: false, enabled: false }] }; },
  } }, `http://127.0.0.1:${server.address().port}`);
  assert.equal((await manager.installBundle('fixture-range@^1', { enabled: false })).application, 'applied');
  assert.equal((await manager.installBundle('fixture-range@next', { enabled: false })).application, 'applied');
  assert.deepEqual(versions, ['1.2.0', '2.0.0']);
  assert.equal((await manager.installBundle('fixture-range', { registry: 'https://different.test' })).application, 'failed');
  assert.equal(versions.length, 2, '安装源不一致不能发送预览或写入');
});
const execution = () => ({ invocationId: 'test-call', signal: new AbortController().signal });
const tool = (name, execute = async () => 'ok') => ({
  name, description: name, parameters: { type: 'object' }, execute,
  output: { schema: { type: 'string' }, render: (_args, value) => [{ type: 'text', text: value }] },
});
async function until(predicate) {
  for (let n = 0; n < 200; n++) { if (predicate()) return; await delay(10); }
  assert.fail('运行状态未在 2 秒内收敛');
}

test('真实 DSH MCP 客户端发现、调用、工具目录变化、取消与停用回收子进程', async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'magi-mcp-contract-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const file = join(directory, 'server.mjs');
  await writeFile(file, `import {createInterface} from 'node:readline';
let names=['echo'];
const send=value=>process.stdout.write(JSON.stringify(value)+'\\n');
createInterface({input:process.stdin}).on('line',line=>{
 const m=JSON.parse(line);if(!Object.hasOwn(m,'id'))return;
 let result;
 if(m.method==='initialize')result={protocolVersion:'2025-11-25',serverInfo:{name:'contract',version:'1'},capabilities:{tools:{listChanged:true}}};
 else if(m.method==='tools/list')result={tools:names.map(name=>({name,description:'contract tool',inputSchema:{type:'object',properties:{replace:{type:'boolean'}}}}))};
 else if(m.method==='tools/call'){
  result={content:[{type:'text',text:JSON.stringify({pid:process.pid,name:m.params.name})}]};
  if(m.params.arguments?.replace){names=['replacement'];send({jsonrpc:'2.0',method:'notifications/tools/list_changed'});}
 }else result={};
 send({jsonrpc:'2.0',id:m.id,result});
});`);
  const h = host();
  const running = await activateDsh(h.api, {
    async apply(ctx) { const fiber = ctx.plugin(McpClient, { transport: 'stdio', serverName: 'contract', command: process.execPath, args: [file], failOnStartupError: true }); await fiber.await(); },
  }, options);
  t.after(() => running.dispose());
  const oldId = runtimeId('tool', 'mcp__contract__echo');
  assert.ok(h.state.tools.has(oldId));
  const response = await h.state.tools.get(oldId)({ replace: true }, execution());
  const receipt = JSON.parse(response.content[0].text);
  assert.equal(receipt.name, 'echo');
  const nextId = runtimeId('tool', 'mcp__contract__replacement');
  await until(() => !h.state.tools.has(oldId) && h.state.tools.has(nextId));
  await assert.rejects(h.state.tools.get(nextId)({}, { invocationId: 'cancelled', signal: AbortSignal.abort(new Error('cancelled')) }), /cancel|abort/i);
  await running.dispose();
  assert.equal(h.state.tools.size, 0);
  await until(() => { try { process.kill(receipt.pid, 0); return false; } catch { return true; } });
  assert.deepEqual(h.state.failures, []);
});

test('DSH 初始化失败、必需依赖缺失和未支持事件均不能就绪，并回收已有工具', async () => {
  for (const plugin of [
    { inject: ['tools'], apply(ctx) { ctx.tools.register(tool('early')); throw new Error('startup failed'); } },
    { inject: ['absent'], apply() { assert.fail('缺依赖不能执行'); } },
    { apply(ctx) { ctx.plugin({ inject: ['absent'], apply() { assert.fail('子插件必需依赖不能绕过'); } }); } },
    { apply(ctx) { ctx.on('session/event', () => {}); } },
  ]) {
    const h = host();
    await assert.rejects(activateDsh(h.api, plugin, options), /startup failed|缺少必需服务|尚未实现/);
    assert.equal(h.state.tools.size, 0);
  }
});

test('DSH 条件注入缺服务保留 pending，注册失败交由上游捕获；可用能力正常执行', async (t) => {
  const h = host();
  let missingRan = false, caught = false;
  const running = await activateDsh(h.api, { apply(ctx) {
    ctx.inject(['sessionProjections'], () => { missingRan = true; });
    ctx.inject(['tools'], (c) => c.tools.register(tool('available')));
    try { ctx.on('session/event', () => assert.fail('不得伪造宿主事件')); }
    catch { caught = true; }
  } }, options);
  t.after(() => running.dispose());
  assert.equal(missingRan, false);
  assert.equal(caught, true);
  assert.ok(h.state.logs.some((line) => line.includes('可选注入') && line.includes('sessionProjections')));
  const result = await h.state.tools.get(runtimeId('tool', 'available'))({}, execution());
  assert.equal(result.content[0].text, 'ok');
  assert.deepEqual(h.state.failures, []);
});

test('DSH 就绪后的注册、撤销、同名重绑沿 effect 生效，卸载清空目录', async (t) => {
  const h = host();
  let ctx;
  const running = await activateDsh(h.api, { inject: ['tools', 'llm'], apply(c) { ctx = c; } }, options);
  t.after(() => running.dispose());
  const id = runtimeId('tool', 'late');
  const off = ctx.tools.register(tool('late', async () => 'first'));
  await until(() => h.state.tools.has(id));
  assert.equal((await h.state.tools.get(id)({}, execution())).content[0].text, 'first');
  await off();
  assert.equal(h.state.tools.has(id), false);
  ctx.tools.register(tool('late', async () => 'second'));
  await until(() => h.state.tools.has(id));
  assert.equal((await h.state.tools.get(id)({}, execution())).content[0].text, 'second');
  await running.dispose();
  assert.equal(h.state.tools.size, 0);
  assert.deepEqual(h.state.catalog.tools, []);
  assert.deepEqual(h.state.failures, []);
});

test('DSH 普通配置重载并重绑工具；无效配置不替换运行实例', async (t) => {
  const h = host({ label: 'first' });
  const lifecycle = [];
  const running = await activateDsh(h.api, {
    inject: ['tools'],
    Config: Schema.object({ label: Schema.string().required() }),
    apply(ctx, config) {
      lifecycle.push(`start:${config.label}`);
      ctx.tools.register(tool('configured', async () => config.label));
      ctx.effect(() => () => { lifecycle.push(`stop:${config.label}`); });
    },
  }, options);
  t.after(() => running.dispose());
  const call = () => h.state.tools.get(runtimeId('tool', 'configured'))({}, execution());
  await h.configure({ label: 'second' });
  assert.equal((await call()).content[0].text, 'second');
  assert.deepEqual(lifecycle, ['start:first', 'stop:first', 'start:second']);
  await assert.rejects(h.configure({ label: 42 }));
  assert.equal((await call()).content[0].text, 'second');
  assert.deepEqual(h.state.failures, []);
});

test('DSH Volatile 配置更新保留实例且投递更新路径', async (t) => {
  const h = host({ label: 'first' });
  let starts = 0, ctx;
  const paths = [];
  const running = await activateDsh(h.api, {
    inject: ['tools'],
    Config: Schema.object({ label: Schema.string().default('default').volatile() }),
    apply(c, config) {
      ctx = c;
      starts++;
      ctx.on('loader/volatile-update', (changed) => paths.push(changed));
      ctx.tools.register(tool('configured', async () => config.label.get()));
    },
  }, options);
  t.after(() => running.dispose());
  await h.configure({ label: 'second' });
  assert.equal(starts, 1);
  assert.deepEqual(paths, [[['label']]]);
  assert.equal((await h.state.tools.get(runtimeId('tool', 'configured'))({}, execution())).content[0].text, 'second');
  await ctx.fiber.restart();
  await until(() => h.state.tools.has(runtimeId('tool', 'configured')));
  assert.equal(starts, 2);
  assert.equal((await h.state.tools.get(runtimeId('tool', 'configured'))({}, execution())).content[0].text, 'second');
});

test('DSH 并发配置通知按版本串行重载，不把旧实例的工具留在新目录', async (t) => {
  const h = host({ label: 'initial' });
  const changes = [];
  const running = await activateDsh(h.api, {
    inject: ['tools'],
    apply(ctx, config) {
      changes.push(`start:${config.label}`);
      ctx.tools.register(tool('configured', async () => config.label));
      ctx.effect(() => async () => { await delay(10); changes.push(`stop:${config.label}`); });
    },
  }, options);
  t.after(() => running.dispose());
  await Promise.all([h.configure({ label: 'first' }), h.configure({ label: 'second' })]);
  assert.equal((await h.state.tools.get(runtimeId('tool', 'configured'))({}, execution())).content[0].text, 'second');
  assert.deepEqual(changes, ['start:initial', 'stop:initial', 'start:first', 'stop:first', 'start:second']);
  assert.deepEqual(h.state.failures, []);
});

test('DSH 工具沿真实管线校验输出、执行 guard，并传递取消', async (t) => {
  const h = host();
  let called = 0;
  const running = await activateDsh(h.api, { inject: ['tools'], apply(ctx) {
    ctx.tools.guard((exec) => exec.name === 'blocked' ? { reason: 'denied by plugin' } : undefined);
    ctx.tools.register(tool('blocked', async () => { called++; return 'unexpected'; }));
    ctx.tools.register(tool('invalid', async () => 123));
    ctx.tools.register(tool('cancel', async (_args, exec) => {
      await delay(1000, null, { signal: exec.signal });
      return 'unexpected';
    }));
  } }, options);
  t.after(() => running.dispose());
  const invoke = (name, context = execution()) => h.state.tools.get(runtimeId('tool', name))({}, context);
  await assert.rejects(invoke('blocked'), /denied by plugin/);
  assert.equal(called, 0);
  await assert.rejects(invoke('invalid'), /output|schema|string/i);
  const controller = new AbortController();
  const pending = invoke('cancel', { invocationId: 'cancel-call', signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, /abort|cancel/i);
});

test('DSH 运行期子插件初始化失败被报告，不能保留成功状态', async (t) => {
  const h = host();
  let ctx;
  const running = await activateDsh(h.api, { inject: ['tools', 'llm'], apply(c) { ctx = c; } }, options);
  t.after(() => running.dispose());
  ctx.plugin({ async apply() { await delay(5); throw new Error('late failure'); } });
  await until(() => h.state.failures.length > 0);
  assert.match(h.state.failures[0].message, /late failure/);
});

test('DSH 模型目录刷新期间发生撤销，不发布已撤销的来源', async (t) => {
  const h = host();
  let ctx, entered, resume;
  const listing = new Promise((resolve) => { entered = resolve; });
  const released = new Promise((resolve) => { resume = resolve; });
  const running = await activateDsh(h.api, { inject: ['tools', 'llm'], apply(c) { ctx = c; } }, options);
  t.after(() => running.dispose());
  class Adapter extends LlmAdapter {
    async listModels() { entered(); await released; return [{ provider: 'fixture', id: 'model', name: 'Model' }]; }
  }
  const off = ctx.llm.registerAdapter(['fixture'], new Adapter());
  await listing;
  off();
  resume();
  await until(() => h.state.catalog.modelSources.length === 0);
  assert.equal(h.state.models.size, 0);
  assert.deepEqual(h.state.failures, []);
});

test('DSH 卸载不等待未完成的模型发现，迟到结果不会重新发布', async () => {
  const h = host();
  let ctx, entered, resume;
  const listing = new Promise((resolve) => { entered = resolve; });
  const released = new Promise((resolve) => { resume = resolve; });
  const running = await activateDsh(h.api, { inject: ['llm'], apply(c) { ctx = c; } }, options);
  class Adapter extends LlmAdapter {
    async listModels() { entered(); await released; return [{ provider: 'fixture', id: 'model', name: 'Model' }]; }
  }
  ctx.llm.registerAdapter(['fixture'], new Adapter());
  await listing;
  await running.dispose();
  resume();
  await delay(10);
  assert.equal(h.state.models.size, 0);
  assert.deepEqual(h.state.catalog.modelSources, []);
  assert.deepEqual(h.state.failures, []);
});

test('DSH 初始化期间的模型撤销也参与刷新代次，不能丢弃初始化事件', async (t) => {
  const h = host();
  let off, entered, resume;
  const listing = new Promise((resolve) => { entered = resolve; });
  const released = new Promise((resolve) => { resume = resolve; });
  class Adapter extends LlmAdapter {
    async listModels() { entered(); await released; return [{ provider: 'fixture', id: 'model', name: 'Model' }]; }
  }
  const starting = activateDsh(h.api, { inject: ['llm'], apply(ctx) {
    off = ctx.llm.registerAdapter(['fixture'], new Adapter());
  } }, options);
  await listing;
  off(); resume();
  const running = await starting;
  t.after(() => running.dispose());
  assert.equal(h.state.models.size, 0);
  assert.deepEqual(h.state.catalog.modelSources, []);
  assert.deepEqual(h.state.failures, []);
});

test('DSH 卸载在 daemon 拒绝回写时仍关闭模型端口', async (t) => {
  const h = host();
  class Adapter extends LlmAdapter {
    async listModels() { return [{ provider: 'fixture', id: 'model', name: 'Model' }]; }
  }
  const running = await activateDsh(h.api, { inject: ['llm'], apply(ctx) {
    ctx.llm.registerAdapter(['fixture'], new Adapter());
  } }, options);
  t.after(() => running.dispose());
  const source = [...h.state.models.values()][0];
  assert.ok(source);
  const response = await fetch(`${source.baseUrl}/models`, { headers: { authorization: `Bearer ${source.apiKey}` } });
  assert.equal(response.status, 200);
  await response.json();
  h.api.modelSource.clear = async () => { throw new Error('实例已失效'); };
  await running.dispose();
  await assert.rejects(fetch(`${source.baseUrl}/models`));
});

test('图片经真实本机 HTTP 模型桥转换，附件引用只在该请求内有效', async (t) => {
  const bytes = await sharp({ create: { width: 8, height: 6, channels: 3, background: '#2468ab' } }).png().toBuffer();
  const dataUrl = `data:image/png;base64,${bytes.toString('base64')}`;
  let reference;
  const bridge = await startOpenAiBridge({
    provider: 'fixture', saveImage: saveRequestImage, withRequest: withRequestImages,
    llm: { async *stream(request) {
      assert.equal(request.messages[0].content[0].text, 'describe');
      reference = request.messages[0].content[1].attachment;
      const stored = await requestAttachments.readImage(reference, request.signal);
      const metadata = await sharp(stored.data).metadata();
      assert.equal(metadata.width, 8);
      assert.equal(metadata.height, 6);
      const variant = await requestAttachments.readImageRequest(reference, { width: 4, height: 4, maxBytes: 4096 }, request.signal);
      assert.equal(variant.width, 4);
      assert.equal(variant.height, 3);
      yield { type: 'text-delta', text: 'image received' };
      yield { type: 'usage', usage: { inputTokens: 1, outputTokens: 2 } };
      yield { type: 'finish', reason: { kind: 'stop' } };
    } },
  });
  t.after(() => bridge.close());
  const response = await fetch(`${bridge.baseUrl}/chat/completions`, {
    method: 'POST', headers: { authorization: `Bearer ${bridge.apiKey}`, 'content-type': 'application/json' },
    body: JSON.stringify({ model: 'image', stream: true, messages: [{ role: 'user', content: [
      { type: 'text', text: 'describe' }, { type: 'image_url', image_url: { url: dataUrl } },
    ] }] }),
  });
  assert.equal(response.status, 200);
  const output = await response.text();
  assert.match(output, /image received/);
  assert.match(output, /data: \[DONE\]/);
  await assert.rejects(requestAttachments.readImage(reference), /当前 Magi 模型请求/);
  await withRequestImages(async () => {
    await assert.rejects(requestAttachments.readImage(reference), /不属于当前请求/);
    const ref = await saveRequestImage(dataUrl);
    await assert.rejects(requestAttachments.readImage({ ...ref, width: 9 }), /元数据不匹配/);
    const controller = new AbortController(); controller.abort();
    await assert.rejects(requestAttachments.readImage(ref, controller.signal), /abort/i);
  });
});

test('模型图片桥接受安全 SVG 并拒绝活动内容', async () => {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="8" height="6"><rect width="8" height="6" fill="red"/></svg>`;
  const dataUrl = `data:image/svg+xml;base64,${Buffer.from(svg).toString('base64')}`;
  const ref = await withRequestImages(() => saveRequestImage(dataUrl));
  assert.equal(ref.mediaType, 'image/png');
  await assert.rejects(withRequestImages(() => saveRequestImage(`data:image/svg+xml;base64,${Buffer.from('<svg><script>alert(1)</script></svg>').toString('base64')}`)), /活动|外部|根节点/);
});
