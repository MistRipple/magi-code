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
import { browserImageFixtures, bmpEncodingFixtures } from '../lib/plugin-image-fixtures.mjs';
import { createBackendSessions } from './runtime/backend-sessions.mjs';
import { REGISTERED_SETTINGS_RESOURCE } from './runtime/registered-settings.mjs';
import { createRegisteredSettings } from './runtime/registered-settings.mjs';

// 只替代 daemon RPC；插件加载、依赖、effect、工具管线及模型目录运行真实上游代码。
function host(value = {}) {
  let revision = 0;
  let snapshot = { version: 0, value };
  let listener, validate;
  const state = { catalog: {}, tools: new Map(), commands: new Map(), routes: new Map(), models: new Map(), failures: [], logs: [], outcomes: [] };
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
    toolResult(output, options) { state.outcomes.push({ output, options }); return output; },
    async route(path, handler) { state.routes.set(path, handler); return () => state.routes.delete(path); },
    modelSource: {
      async set(id, source) { state.models.set(id, source); },
      async clear(id) { state.models.delete(id); },
    },
    session: { async directory() { return { sessions: [{ sessionId: 'fixture-session', workspaceId: null, title: 'Fixture', createdAt: 1, updatedAt: 2 }], hasMore: false, nextAfterSessionId: null }; } },
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

test('命令与工具读取同一 daemon 调用的会话身份和执行目录，并发、配置替换与取消不串用投影', async (t) => {
  const h = host({ label: 'first' });
  let retained, crossCall;
  const running = await activateDsh(h.api, { inject: ['commands', 'tools'], apply(ctx, config) {
    const read = async (agent, signal) => {
      const session = agent.session, header = session.header;
      assert.throws(() => agent.inject({}), /插件消息必须/);
      assert.throws(() => session.append('message', {}), /尚未接入/);
      assert.throws(() => { header.cwd = '/forged'; }, TypeError);
      if (crossCall) assert.throws(() => crossCall.id, /失效|不属于/);
      await delay(5, undefined, { signal });
      retained = { agent, session, header };
      return `${config.label}:${agent.id}:${session.id}:${header.cwd}`;
    };
    ctx.commands.register({ name: 'directory', handler: async invocation => ({ kind: 'success', text: await read(invocation.agent, invocation.signal) }) });
    ctx.tools.register(tool('directory', async (_args, exec) => read(exec.agent, exec.signal)));
  } }, options);
  t.after(() => running.dispose());
  const context = (id) => ({ ...execution(), owner: { sessionId: id }, execution: { workingDirectory: `/execution/${id}`, sourceFolders: [], accessProfile: 'restricted' } });
  const command = (id) => h.state.commands.get(runtimeId('cmd', 'directory'))({}, context(id));
  const [a, b] = await Promise.all([command('one'), command('two')]);
  assert.equal(a.content, 'first:one:one:/execution/one');
  assert.equal(b.content, 'first:two:two:/execution/two');
  for (const object of Object.values(retained)) assert.throws(() => object.id, /失效/);
  crossCall = retained.agent;
  const value = await h.state.tools.get(runtimeId('tool', 'directory'))({}, context('three'));
  assert.equal(value.content[0].text, 'first:three:three:/execution/three');
  await h.configure({ label: 'second' });
  assert.equal((await command('four')).content, 'second:four:four:/execution/four');
  const controller = new AbortController();
  const pending = h.state.commands.get(runtimeId('cmd', 'directory'))({}, { ...context('cancelled'), signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, /abort/i);
  await assert.rejects(h.state.commands.get(runtimeId('cmd', 'directory'))({}, { ...context('cancelled-before-start'), signal: controller.signal }), /abort/i);
  assert.equal((await command('five')).content, 'second:five:five:/execution/five');
});

test('会话业务缺少 daemon 执行上下文时明确失败，不猜进程 cwd 或创建上游会话', async (t) => {
  const h = host();
  const running = await activateDsh(h.api, { inject: ['commands'], apply(ctx) {
    ctx.commands.register({ name: 'directory', handler: invocation => ({ kind: 'success', text: invocation.agent.session.header.cwd }) });
  } }, options);
  t.after(() => running.dispose());
  await assert.rejects(h.state.commands.get(runtimeId('cmd', 'directory'))({}, { ...execution(), owner: { sessionId: 's' } }), /未提供有效/);
});

test('技能沿真实 Cordis effect 进入同一动态目录，配置替换与停用撤销旧正文', async () => {
  const h = host();
  const Config = Schema.object({ label: Schema.string().default('first'), enabled: Schema.boolean().default(true) });
  const running = await activateDsh(h.api, { Config, inject: ['skills'], apply(ctx, config) {
    if (config.enabled) ctx.effect(function* () {
      yield ctx.skills.register({ name: 'inspect-runtime', description: 'Inspect the current runtime', content: `Original body: ${config.label}`, source: 'runtime' });
      const duplicate = ctx.skills.register({ name: 'inspect-runtime', description: 'Duplicate', content: 'Must not replace the original', source: 'runtime' });
      duplicate();
    });
    assert.throws(() => ctx.skills.register({ name: 'empty', description: 'Empty', content: '' }), /正文/);
    assert.throws(() => ctx.skills.register({ name: 'policy', description: 'Policy', content: 'body', invocation: { modelInvocable: false, userInvocable: true } }), /调用策略/);
    assert.throws(() => ctx.skills.register({ name: 'resource', description: 'Resources', content: 'body', resourceBase: { kind: 'directory', path: '/tmp' } }), /resourceBase/);
  } }, options);
  try {
    assert.deepEqual(h.state.catalog.skills, [{ id: runtimeId('skill', 'inspect-runtime'), title: 'inspect-runtime', description: 'Inspect the current runtime', instruction: 'Original body: first' }]);
    await h.configure({ label: 'second', enabled: true });
    assert.equal(h.state.catalog.skills.length, 1);
    assert.equal(h.state.catalog.skills[0].instruction, 'Original body: second');
    await h.configure({ label: 'second', enabled: false });
    assert.deepEqual(h.state.catalog.skills, []);
    await h.configure({ label: 'third', enabled: true });
    assert.equal(h.state.catalog.skills[0].instruction, 'Original body: third');
  } finally { await running.dispose(); }
  assert.deepEqual(h.state.catalog.skills, []);
  assert.deepEqual(h.state.failures, []);
});

test('设置命名空间可在配置重载后动态出现，资源授权与 CAS 仍由 daemon 裁决', async () => {
  let snapshot = { version: 0, value: null }, fail = false;
  const settings = createRegisteredSettings({ resource: {
    async read(id) { assert.equal(id, REGISTERED_SETTINGS_RESOURCE); return structuredClone(snapshot); },
    async write(id, version, value) {
      assert.equal(id, REGISTERED_SETTINGS_RESOURCE); assert.equal(version, snapshot.version);
      if (fail) throw new Error('permission revoked');
      return snapshot = { version: version + 1, value: structuredClone(value) };
    },
  } }, true);
  const cleanup = [], ctx = { effect: callback => cleanup.push(callback()) };
  await settings.run(async () => {
    const late = settings.register(ctx, 'runtime-selected', Schema.object({ enabled: Schema.boolean().default(true) }));
    await settings.update('runtime-selected', { enabled: false }); assert.equal(late.get().enabled, false);
    fail = true;
  });
  await assert.rejects(settings.run(() => settings.update('runtime-selected', { enabled: true })), /permission revoked/);
  assert.equal(settings.get('runtime-selected').enabled, false);
  for (const release of cleanup) release();
  assert.equal(settings.get('runtime-selected'), undefined);
  await settings.dispose();
  const denied = createRegisteredSettings({}, false);
  assert.throws(() => denied.register(ctx, 'unapproved', Schema.object({})), /未声明/);
});

test('DSH 注册设置按命名空间保存到 daemon 资源，未 await 的写入在路由回执前落盘并在重建实例后恢复', async (t) => {
  let h = host({ configOnly: true });
  let stored = { version: 0, value: null }, failure, scopes, observed = [];
  const resource = {
    async read(id) { assert.equal(id, REGISTERED_SETTINGS_RESOURCE); return structuredClone(stored); },
    async write(id, expected, value) {
      assert.equal(id, REGISTERED_SETTINGS_RESOURCE); assert.equal(expected, stored.version);
      await delay(5); if (failure) throw failure;
      stored = { version: expected + 1, value: structuredClone(value) }; return structuredClone(stored);
    },
  };
  h.api.resource = resource;
  const upstream = { inject: ['settings', 'webServer'], apply(ctx) {
    const schema = value => ({ enabled: value.enabled ?? true, items: value.items ?? ['a', 'b'] });
    scopes = ['one', 'two'].map(ns => ctx.settings.register(ns, schema, { applies: 'live' }));
    ctx.on('settings/updated', (ns, value) => observed.push([ns, value]));
    ctx.webServer.register({ path: '/settings', handler: async (request, response) => {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const input = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      if (input.op === 'get') { response.end(JSON.stringify(scopes.map(scope => scope.get()))); return; }
      if (input.op === 'mutate') ctx.settings.mutate(input.ns, input.ops);
      else ctx.settings.update(input.ns, input.patch);
      response.end(JSON.stringify({ saved: true }));
    } });
  } };
  const settingsOptions = { ...options, settingsNamespaces: ['one', 'two'] };
  let running = await activateDsh(h.api, upstream, settingsOptions);
  t.after(() => running.dispose());
  const invoke = body => h.state.routes.get('/settings')({ method: 'POST', path: '/settings', body }, { signal: new AbortController().signal });
  await invoke({ ns: 'one', patch: { enabled: false } });
  assert.equal(stored.value.one.enabled, false);
  assert.equal(stored.value.two, undefined);
  assert.equal(observed[0][0], 'one');
  assert.deepEqual((await h.api.settings.read()).value, { configOnly: true }, '旧版设置不能覆盖现代 Config');
  await Promise.all([invoke({ ns: 'one', patch: { items: ['c'] } }), invoke({ ns: 'two', patch: { enabled: false } })]);
  assert.deepEqual(stored.value.one, { enabled: false, items: ['c'] });
  assert.equal(stored.value.two.enabled, false);
  await invoke({ ns: 'two', op: 'mutate', ops: [{ op: 'unset', path: ['items', '0'] }] });
  assert.deepEqual(stored.value.two.items, ['b']);
  const old = scopes[0]; await running.dispose();
  assert.throws(() => old.get(), /已失效/);
  h = host({ configOnly: true }); h.api.resource = resource;
  running = await activateDsh(h.api, upstream, settingsOptions);
  assert.deepEqual(scopes.map(scope => scope.get()), [stored.value.one, stored.value.two]);
  failure = new Error('permission revoked'); const previous = structuredClone(stored);
  await assert.rejects(invoke({ ns: 'one', patch: { enabled: true } }), /permission revoked/);
  assert.deepEqual(stored, previous);
  assert.deepEqual(scopes[0].get(), previous.value.one, '写失败不能发布乐观快照');
  failure = null;
  await assert.rejects(invoke({ ns: 'two', op: 'mutate', ops: [{ op: 'set', path: ['__proto__', 'polluted'], value: true }] }), /路径/);
});

test('后端会话目录消费真实分页，保持 list/get 形状且拒绝跨调用、取消和未知 Session 字段', async () => {
  let version=0, saved, calls=[];
  const backend=createBackendSessions({session:{async directory(input){
    calls.push(input);
    return input.afterSessionId ? {sessions:[{sessionId:'b',createdAt:1}],hasMore:false,nextAfterSessionId:null}
      : {sessions:[{sessionId:`a${version}`,createdAt:2}],hasMore:true,nextAfterSessionId:`a${version}`};
  }}});
  assert.equal(typeof backend.service.list,'function');
  assert.equal(backend.service.scope,undefined);
  assert.throws(()=>backend.service.list(),/当前 daemon 调用/);
  await backend.run(()=>{
    assert.deepEqual(backend.service.list().map(session=>session.id),['b','a0']);
    saved=backend.service.get('a0');
    assert.throws(()=>saved.header,/尚未映射/);
    assert.equal(backend.service.get('unknown'),undefined);
  });
  assert.throws(()=>saved.id,/当前 daemon 调用/);
  version++;
  await backend.run(()=>{
    assert.equal(backend.service.get('a0'),undefined);
    assert.equal(backend.service.get('a1').id,'a1');
    assert.throws(()=>saved.id,/不属于当前调用/);
  });
  assert.equal(calls.length,4);
  const controller=new AbortController();controller.abort(new Error('cancelled'));
  await assert.rejects(backend.run(()=>assert.fail(),controller.signal),/cancelled/);
  assert.equal(calls.length,4);
  backend.dispose();
  await assert.rejects(backend.run(()=>assert.fail()),/已失效/);
});

test('后端会话目录分页错误、撤权与处置不会回落旧快照或继续执行业务',async()=>{
  for(const directory of [
    async()=>({sessions:[{sessionId:'same',createdAt:1}],hasMore:true,nextAfterSessionId:'same'}),
    async()=>({sessions:[],hasMore:true,nextAfterSessionId:null}),
    async()=>{throw new Error('permission revoked');},
  ]){
    const backend=createBackendSessions({session:{directory}});
    await assert.rejects(backend.run(()=>assert.fail('目录未成功不得执行业务')),/身份重复|游标|revoked/);
    assert.throws(()=>backend.service.list(),/当前 daemon 调用/);
  }
  let finish;
  const backend=createBackendSessions({session:{directory:()=>new Promise(resolve=>{finish=resolve;})}});
  const request=backend.run(()=>assert.fail('处置期间不能使用迟到目录'));
  backend.dispose();finish({sessions:[],hasMore:false,nextAfterSessionId:null});
  await assert.rejects(request,/已失效/);
});

test('附件读取沿 daemon 持久身份，不保留绕过授权的本地图片副本', async (t) => {
  const h = host();
  const values = new Map();
  let gets = 0, attachments;
  h.api.images = {
    async save(input) {
      assert.equal(input.mediaType, 'image/png');
      const ref={attachmentId:'daemon-image',mediaType:input.mediaType,name:input.name,bytes:input.data.length};
      values.set('daemon-image', {ref,data:Buffer.from(input.data)});
      return ref;
    },
    async read(id) {
      gets++;
      if (!values.has(id)) throw new Error('附件不存在或已撤权');
      return {ref:{...values.get(id).ref},data:Buffer.from(values.get(id).data)};
    },
  };
  const running = await activateDsh(h.api, {inject:['attachments'],apply(ctx){ attachments=ctx.attachments; }}, options);
  t.after(()=>running.dispose());
  const bytes=Buffer.from('image payload'), ref=await attachments.saveImage({data:bytes,mediaType:'image/png',name:'capture.png'});
  assert.deepEqual(ref,{attachmentId:'daemon-image',mediaType:'image/png',bytes:bytes.length,name:'capture.png'});
  bytes.fill(0);
  const read=await attachments.readImage({...ref,mediaType:'image/jpeg',name:'forged.jpg'});
  assert.equal(read.data.toString(),'image payload');
  assert.equal(read.ref.mediaType,'image/png');
  assert.equal(read.ref.name,'capture.png');
  assert.equal(gets,1);
  values.clear();
  await assert.rejects(attachments.readImage(ref),/不存在或已撤权/);
  const controller=new AbortController(); controller.abort(new Error('cancelled'));
  await assert.rejects(attachments.readImage(ref,controller.signal),/cancelled/);
  assert.equal(gets,2,'已取消的请求不读取 daemon');
  h.api.images.save=async()=>({});
  await assert.rejects(attachments.saveImage({data:Buffer.from('x'),mediaType:'image/png'}),/未返回已保存附件身份/);
  delete h.api.images;
  await assert.rejects(attachments.saveImage({data:Buffer.from('x'),mediaType:'image/png'}),/未提供持久图片附件服务/);
});

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
  let sessions, ids;
  const projected = await activateDsh(host().api, { inject: ['sessions'], apply(ctx) { sessions = ctx.sessions; ids = sessions.list().map(session=>session.id); } }, { ...options, readSessionDirectory: true });
  t.after(() => projected.dispose());
  assert.deepEqual(ids, ['fixture-session']);
  assert.throws(()=>sessions.list(), /当前 daemon 调用/);
});

test('DSH 管理安装只转发到 daemon 任务，保留版本来源、requestId、取消回执与失败结果', async () => {
  const { createPluginManager, parseBundleSpec } = await import('./runtime/plugin-manager.mjs');
  const calls = [];
  let result = { application: 'applied', bundle: '@scope/sample', version: '1.2.3', changed: true };
  const manager = createPluginManager({ plugins: {
    async installBundle(input) { calls.push(input); return result; },
    async cancelInstall(requestId) { calls.push(['cancel', requestId]); return { status: 'too-late' }; },
    async preview() { assert.fail('翻译层不能另起预览链路'); },
    async install() { assert.fail('翻译层不能另起写入链路'); },
  } });
  assert.equal(parseBundleSpec('sample@^1').version, '^1');
  assert.equal(parseBundleSpec('sample@next').version, 'next');
  assert.equal(parseBundleSpec('sample@>=1 <2').version, '>=1 <2');
  for (const spec of ['github:Owner/Repo#feature/desktop', 'git+https://github.com/Owner/Repo.git#feature/desktop', 'Owner/Repo#feature/desktop']) {
    assert.deepEqual(parseBundleSpec(spec), { ecosystem: 'dsh', github: { repository: 'owner/repo', reference: 'feature/desktop' } });
  }
  for (const bad of ['../sample', 'https://example.org/sample', 'sample@1.0.0;touch x', '@scope/sample@']) assert.throws(() => parseBundleSpec(bad));
  assert.deepEqual(await manager.installBundle('@scope/sample@^1', { requestId: 'upgrader-1', enabled: false }), result);
  assert.deepEqual(calls[0], { ecosystem: 'dsh', package: '@scope/sample', version: '^1', enabled: false, requestId: 'upgrader-1' });
  assert.deepEqual(await manager.cancelInstall('upgrader-1'), { status: 'too-late' });
  assert.deepEqual(calls[1], ['cancel', 'upgrader-1']);
  result = { application: 'cancelled', changed: false };
  assert.equal((await manager.installBundle('sample@next')).application, 'cancelled');
  assert.match(calls[2].requestId, /^[0-9a-f-]{36}$/);
  result = { application: 'failed', installed: true, stage: 'enable', error: { code: 'authorization-required', message: '请确认权限' } };
  assert.deepEqual(await manager.installBundle('sample@1.0.0'), result);
  result = { application: 'failed', error: { code: 'incompatible-version', incompatible: [{ name: 'sample', version: '1.0.0' }] } };
  assert.deepEqual(await manager.installBundle('sample@1.0.0'), result);
  const count = calls.length;
  for (const [spec, options] of [['../sample', {}], ['sample', { enabled: 'yes' }], ['sample', { registry: 'https://different.test' }], ['sample', { approvedBuilds: ['unsafe'] }]]) {
    assert.equal((await manager.installBundle(spec, options)).application, 'failed');
  }
  assert.equal(calls.length, count, '本地格式或来源拒绝不能发起安装');
});

const execution = () => ({ invocationId: 'test-call', signal: new AbortController().signal });

test('全局插件状态观察来自 daemon 当前目录，其他插件失败不终止本实例，恢复及卸载清除故障', async () => {
  const h = host(), failures = new Map();
  let notify;
  const plugin = (state, reason) => ({ manifest: { id: 'dsh.other' }, digest: 'other-digest',
    source: { kind: 'upstream', location: 'dsh:@scope/other@1.0.0' }, active: true, runtime: { state, reason, generation: 1 } });
  let plugins = [plugin('failed', '真实启动失败')];
  h.api.plugins = {
    async list() { return { plugins: structuredClone(plugins) }; },
    onChanged(listener) { notify = listener; return () => { notify = undefined; }; },
  };
  const running = await activateDsh(h.api, { apply(ctx) {
    ctx.on('internal/status', fiber => {
      const name = fiber.entry?.options?.name;
      if (!name) return;
      if (fiber.state === 3) assert.ok(typeof fiber._error === 'string' && fiber._error.length > 0, '同步诊断与异步观察必须保留同一个 daemon 故障原因');
      if (fiber.state === 3) fiber.await().catch(error => failures.set(name, error.message));
      else if ([2, 4].includes(fiber.state)) failures.delete(name);
    }, { global: true });
  } }, options);
  try {
    assert.equal(failures.get('@scope/other'), '真实启动失败');
    assert.deepEqual(h.state.failures, [], '外部故障不能进入本实例的失败处理');
    plugins = [plugin('running')]; await notify();
    assert.equal(failures.size, 0);
    plugins = [plugin('failed', '再次失败')]; await notify();
    assert.equal(failures.get('@scope/other'), '再次失败');
    plugins = []; await notify();
    assert.equal(failures.size, 0);
  } finally { await running.dispose(); }
  assert.equal(notify, undefined, '处置回收目录订阅');
});

test('插件目录观察合并变更并补读在途更新，取消订阅与处置拒绝迟到快照', async () => {
  const { createPluginDirectory } = await import('./runtime/plugin-directory.mjs');
  let notify, resolveRead, reads = 0;
  const calls = [], errors = [];
  const api = { plugins: {
    list() { reads++; return new Promise(resolve => { resolveRead = resolve; }); },
    onChanged(listener) { notify = listener; return () => { notify = undefined; }; },
  } };
  const directory = createPluginDirectory(api, message => errors.push(message));
  const plugin = (state) => ({ manifest: { id: 'dsh.other' }, digest: 'digest', source: { kind: 'upstream', location: 'dsh:other@1.0.0' }, active: true, runtime: { state, generation: 1 } });
  const off = directory.subscribe(value => { calls.push(value.state); });
  const change = notify();
  resolveRead({ plugins: [plugin('starting')] });
  await until(() => reads === 2);
  resolveRead({ plugins: [plugin('running')] }); await change;
  assert.deepEqual(calls, [1, 2]);
  const late = notify(); off();
  resolveRead({ plugins: [plugin('failed')] }); await late;
  assert.deepEqual(calls, [1, 2]);
  directory.subscribe(value => { calls.push(value.state); });
  directory.dispose();
  resolveRead({ plugins: [plugin('failed')] });
  await delay(0);
  assert.deepEqual(calls, [1, 2]); assert.deepEqual(errors, []);
});

test('Loader baseUrl 使用 daemon 调用期 profile 身份，不使用伪造 HTTP 地址', async (t) => {
  const { pathToFileURL } = await import('node:url');
  const directory = await mkdtemp(join(tmpdir(), 'magi-profile-identity-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const h = host(); h.api.plugins = { async list() { return { plugins: [] }; } };
  const original = process.env.DSH_HOME; process.env.DSH_HOME = directory;
  try {
    let identity;
    const running = await activateDsh(h.api, { apply(ctx) { identity = ctx.baseUrl; } }, options);
    try { assert.equal(identity, pathToFileURL(join(directory, 'profiles/magi') + '/').href); }
    finally { await running.dispose(); }
  } finally {
    if (original === undefined) delete process.env.DSH_HOME; else process.env.DSH_HOME = original;
  }
});
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
  assert.match(JSON.stringify(await invoke('blocked')), /denied by plugin/);
  assert.equal(h.state.outcomes.at(-1).options.isError, true);
  assert.equal(called, 0);
  assert.match(JSON.stringify(await invoke('invalid')), /output|schema|string/i);
  assert.equal(h.state.outcomes.at(-1).options.isError, true);
  const controller = new AbortController();
  const pending = invoke('cancel', { invocationId: 'cancel-call', signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, /abort|cancel/i);
});

test('工具补充上下文与失败纠正共用原生消息交付，完成内容只来自成功管线', async t => {
  const h = host(), delivered = [];
  h.api.session.message = async input => { delivered.push(input); return { inputId: input.messageId, state: 'attached' }; };
  const contextMessage = id => ({ id, role: 'user', content: [{ type: 'text', text: `CONTEXT:${id}` }], source: { kind: 'tool-context' } });
  const running = await activateDsh(h.api, { inject: ['tools'], apply(ctx) {
    for (const name of ['continue', 'finish', 'failed', 'invalid-context']) ctx.tools.register(tool(name, (_args, exec) => {
      exec.deferContext(name === 'invalid-context' ? { ...contextMessage(name), source: {kind:'user'} } : contextMessage(name));
      if (name !== 'continue') exec.concludeTurn();
      if (name === 'failed') throw new Error('business failed with context');
      return `ACTUAL:${name}`;
    }));
  } }, options);
  t.after(() => running.dispose());
  const context = { ...execution(), activity: 'tool', owner: { sessionId: 's' }, execution: { workingDirectory: '/execution' } };
  for (const name of ['continue', 'finish', 'failed']) {
    const result = await h.state.tools.get(runtimeId('tool', name))({}, context);
    const outcome = h.state.outcomes.at(-1);
    assert.equal(result.isError, name === 'failed');
    assert.equal(outcome.options.isError, name === 'failed');
    assert.equal(outcome.options.completion, name === 'finish' ? 'ACTUAL:finish' : undefined);
  }
  assert.deepEqual(delivered.map(({ messageId, text, sourceKind, delivery }) => ({ messageId, text, sourceKind, delivery })),
    ['continue', 'finish', 'failed'].map(id => ({ messageId: id, text: `CONTEXT:${id}`, sourceKind: 'tool-context', delivery: 'context' })));
  const invalid = await h.state.tools.get(runtimeId('tool', 'invalid-context'))({}, context);
  assert.equal(invalid.result.value, 'ACTUAL:invalid-context');
  assert.equal(invalid.contextDelivery.status, 'failed');
  assert.equal(h.state.outcomes.at(-1).options.completion, undefined);
  assert.equal(h.state.outcomes.at(-1).options.isError, true);
  h.api.session.message = async () => { throw new Error('context permission denied'); };
  const undelivered = await h.state.tools.get(runtimeId('tool', 'finish'))({}, context);
  assert.equal(undelivered.result.value, 'ACTUAL:finish');
  assert.equal(undelivered.contextDelivery.status, 'failed');
  assert.match(undelivered.contextDelivery.error, /context permission denied/);
  assert.equal(h.state.outcomes.at(-1).options.isError, true);
  assert.equal(h.state.outcomes.at(-1).options.completion, undefined);
  const aborted = new AbortController(); aborted.abort();
  await assert.rejects(h.state.tools.get(runtimeId('tool', 'finish'))({}, { ...context, signal: aborted.signal }), /abort/i);
  assert.equal(delivered.length, 3);
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

test('浏览器通用格式通过真实 HTTP 模型桥变成可读位图，伪造和超限图片拒绝', async (t) => {
  const fixtures = await browserImageFixtures(sharp);
  let calls = 0;
  const bridge = await startOpenAiBridge({ provider: 'fixture', saveImage: saveRequestImage, withRequest: withRequestImages,
    llm: { async *stream(request) {
      const blocks = request.messages[0].content;
      assert.equal(blocks.length, fixtures.length);
      for (let index = 0; index < fixtures.length; index++) {
        const [, , , width, height] = fixtures[index];
        const image = await requestAttachments.readImage(blocks[index].attachment, request.signal);
        const meta = await sharp(image.data).metadata();
        assert.equal(meta.mediaType, 'image/png'); assert.equal(meta.width, width); assert.equal(meta.height, height);
        assert.equal(meta.depth, 'uchar');
        const pixel = await sharp(image.data).flatten({background:'#ffffff'}).raw().toBuffer();
        assert.ok(pixel[0] > pixel[1] + 80 && pixel[0] > pixel[2] + 80, `${fixtures[index][0]} 保留红色像素，不能把透明度或通道转换错`);
      }
      calls++;
      yield { type: 'text-delta', text: 'all formats received' };
      yield { type: 'usage', usage: { inputTokens: 1, outputTokens: 2 } };
      yield { type: 'finish', reason: { kind: 'stop' } };
    } },
  });
  t.after(() => bridge.close());
  const response = await fetch(`${bridge.baseUrl}/chat/completions`, { method: 'POST',
    headers: { authorization: `Bearer ${bridge.apiKey}`, 'content-type': 'application/json' },
    body: JSON.stringify({ model: 'image', stream: true, messages: [{ role: 'user', content: fixtures.map(([, mime, bytes]) => ({ type: 'image_url', image_url: { url: `data:${mime};base64,${bytes.toString('base64')}` } })) }] }),
  });
  assert.equal(response.status, 200, await response.clone().text());
  assert.match(await response.text(), /all formats received/); assert.equal(calls, 1);
  await withRequestImages(async () => {
    const bmp = Buffer.from(fixtures.find(([ext]) => ext === 'bmp')[2]);
    bmp.writeInt32LE(100_000, 18); bmp.writeInt32LE(100_000, 22);
    await assert.rejects(saveRequestImage(`data:image/bmp;base64,${bmp.toString('base64')}`), /尺寸/);
    const ico = Buffer.from(fixtures.find(([ext]) => ext === 'ico')[2]);
    ico[6] = 100;
    await assert.rejects(saveRequestImage(`data:image/x-icon;base64,${ico.toString('base64')}`), /尺寸/);
    await assert.rejects(saveRequestImage(`data:image/x-icon;base64,${fixtures[0][2].toString('base64')}`), /ICO/);
    await assert.rejects(saveRequestImage(`data:image/avif;base64,${fixtures[0][2].toString('base64')}`), /声明不一致/);
  });
});

test('BMP 调色板、位域和 RLE 保留颜色及不透明度，不将保留字节当透明度', async () => {
  await withRequestImages(async () => {
    for (const [encoding, input] of bmpEncodingFixtures()) {
      const ref = await saveRequestImage(`data:image/bmp;base64,${input.toString('base64')}`);
      const { data } = await requestAttachments.readImage(ref);
      const pixels = await sharp(data).ensureAlpha().raw().toBuffer();
      assert.equal(ref.width, 2); assert.equal(ref.height, 1);
      for (let at = 0; at < pixels.length; at += 4) {
        assert.ok(pixels[at] > 240 && pixels[at + 1] === 0 && pixels[at + 2] === 0 && pixels[at + 3] === 255, `${encoding}: ${[...pixels]}`);
      }
    }
  });
});


test('插件消息同步业务等待真实 daemon 回执，区分上下文与唤醒且不泄漏过期投影', async () => {
  const {withInvocation,invocationAgent}=await import('./runtime/invocation.mjs');
  const calls=[];let done,agent;
  const api={session:{message:async input=>{calls.push(input);await new Promise(resolve=>{done=resolve});return {inputId:'input',state:'pending'}}}};
  const context={...execution(),activity:'command',owner:{sessionId:'s'},execution:{workingDirectory:'/execution',sourceFolders:[],accessProfile:'restricted'}};
  const message={id:'m1',role:'user',content:[{type:'text',text:'RESULT'}],source:{kind:'example-result'}};
  let finished=false;
  const work=withInvocation(context,async()=>{
    agent=invocationAgent();assert.equal(agent.status,'idle');
    agent.steer(message);agent.steer(message);
    return 'original';
  },api).then(value=>{finished=true;return value});
  await until(()=>calls.length===1);assert.equal(finished,false);
  assert.deepEqual(calls,[{messageId:'m1',text:'RESULT',sourceKind:'example-result',delivery:'steer'}]);
  done();assert.equal(await work,'original');
  assert.throws(()=>agent.inject(message),/失效/);
  await assert.rejects(withInvocation(context,()=>{invocationAgent().inject({...message,source:{kind:'user'}})},api),/独立来源/);
  await assert.rejects(withInvocation(context,()=>{invocationAgent().inject(message);throw new Error('business failed')},api),/business failed/);
  assert.equal(calls.length,1,'失败业务不能提交临时消息');
  const rejected={session:{message:async()=>{throw new Error('permission denied')}}};
  await assert.rejects(withInvocation(context,()=>{invocationAgent().inject(message)},rejected),/permission denied/);
  const delivered=[];
  await withInvocation({...context,activity:'tool'},()=>{const a=invocationAgent();assert.equal(a.status,'running');a.inject(message);a.followup({...message,id:'m2'});},
    {session:{message:async input=>{delivered.push(input.delivery);return {inputId:'i',state:'attached'}}}});
  assert.deepEqual(delivered,['context','followup']);
});


test('原包工具与命令共用已装配的消息端口，状态与真实 daemon 交付方式一致', async t => {
  const h=host(), messages=[];
  h.api.session.message=async input=>{messages.push(input);return {inputId:'accepted',state:'pending'}};
  const body={id:'command-message',role:'user',content:[{type:'text',text:'ORIGINAL'}],source:{kind:'result'}};
  const running=await activateDsh(h.api,{inject:['commands','tools'],apply(ctx){
    ctx.commands.register({name:'deliver',handler:invocation=>{
      assert.equal(invocation.agent.status,'idle');
      invocation.agent.steer(body);
      return {kind:'success',text:'COMMAND'};
    }});
    ctx.tools.register(tool('deliver',(_args,exec)=>{
      assert.equal(exec.agent.status,'running');
      exec.agent.inject({...body,id:'tool-message'});
      return 'TOOL';
    }));
  }},options);
  t.after(()=>running.dispose());
  const context={...execution(),owner:{sessionId:'session'},execution:{workingDirectory:'/execution'}};
  await h.state.commands.get(runtimeId('cmd','deliver'))({}, {...context,activity:'command'});
  await h.state.tools.get(runtimeId('tool','deliver'))({},context);
  assert.deepEqual(messages.map(({messageId,delivery})=>({messageId,delivery})),[
    {messageId:'command-message',delivery:'steer'},{messageId:'tool-message',delivery:'context'},
  ]);
});
