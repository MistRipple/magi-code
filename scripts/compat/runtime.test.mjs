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
