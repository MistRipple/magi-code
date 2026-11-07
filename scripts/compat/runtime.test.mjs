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

test('DSH 管理写操作使用精确版本与摘要，未获委托不能授权新包，启停和移除映射真实 ID', async () => {
  const { createPluginManager, parseBundleSpec } = await import('./runtime/plugin-manager.mjs');
  const installed = { digest: 'installed-digest', manifest: { id: 'translated.scoped', name: 'Scoped' }, source: { kind: 'upstream', location: 'dsh:@scope/sample@1.2.3' }, enabled: false, active: false };
  const calls = [];
  const plugins = {
    async list() { return { plugins: [installed] }; },
    async preview(input) { calls.push(['preview', input]); return { version: '1.2.3', candidate: { digest: 'confirmed' } }; },
    async install(input) { calls.push(['install', input]); return { plugins: [installed] }; },
    async setEnabled(id, enabled, expectedDigest) { calls.push(['enabled', id, enabled, expectedDigest]); if (enabled) throw Object.assign(new Error('缺少新版授权'), { kind: 'PermissionDenied' }); return { plugins: [installed] }; },
    async remove(id, expectedDigest) { calls.push(['remove', id, expectedDigest]); },
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
  assert.deepEqual(calls.at(-1), ['enabled', 'translated.scoped', false, 'installed-digest']);
  assert.equal((await manager.removeBundle('@scope/sample')).application, 'applied');
  assert.deepEqual(calls.at(-1), ['remove', 'translated.scoped', 'installed-digest']);
  assert.equal((await manager.installBundle('../sample')).application, 'failed');
});

test('获委托的管理插件连续安装与升级时只授权安装回执中的摘要，拒绝后不启用，禁用安装不代授', async () => {
  const { createPluginManager } = await import('./runtime/plugin-manager.mjs');
  const permissions = [{ kind: 'extension', targets: ['full-trust'] }];
  const records = new Map(), calls = [];
  let delegated = true, replaced = false;
  const manager = createPluginManager({ plugins: {
    async list() { return { plugins: [...records.values()] }; },
    async preview(request) { return { version: request.version, candidate: { digest: `digest-${request.version}` } }; },
    async install(request) {
      const row = { digest: request.expectedDigest, source: { kind: 'upstream', location: `dsh:${request.package}@${request.version}` },
        manifest: { id: `translated.${request.package}`, name: request.package, permissions }, active: false, enabled: false };
      records.set(row.manifest.id, row); return { plugins: [...records.values()] };
    },
    async authorize(id, digest, grants) {
      calls.push(['authorize', id, digest]);
      if (!delegated) throw Object.assign(new Error('缺少 authorize 委托'), { kind: 'PermissionDenied' });
      assert.equal(records.get(id).digest, digest); assert.deepEqual(grants, permissions);
      if (replaced) records.set(id, { ...records.get(id), digest: 'replacement' });
    },
    async setEnabled(id, enabled, digest) {
      calls.push(['enable', id, digest]);
      assert.equal(enabled, true);
      if (records.get(id).digest !== digest) throw Object.assign(new Error('目标版本已变化'), { kind: 'Conflict' });
      records.get(id).active = records.get(id).enabled = true;
      return { plugins: [...records.values()] };
    },
  } });
  for (const spec of ['first@1.0.0', 'second@1.0.0', 'first@2.0.0']) {
    assert.equal((await manager.installBundle(spec)).application, 'applied');
  }
  assert.deepEqual(calls.filter(([action]) => action === 'authorize').map(([, id, digest]) => [id, digest]), [
    ['translated.first', 'digest-1.0.0'], ['translated.second', 'digest-1.0.0'], ['translated.first', 'digest-2.0.0'],
  ]);
  delegated = false;
  const denied = await manager.installBundle('denied@1.0.0');
  assert.equal(denied.error.code, 'authorization-required');
  assert.equal(records.get('translated.denied').active, false);
  assert.equal(calls.at(-1)[0], 'authorize', '拒绝委托后不能继续激活');
  const count = calls.length;
  assert.equal((await manager.installBundle('disabled@1.0.0', { enabled: false })).application, 'applied');
  assert.equal(calls.length, count, '禁用安装不能请求授权');
  delegated = true; replaced = true;
  assert.equal((await manager.installBundle('racing@1.0.0')).application, 'failed');
  assert.equal(records.get('translated.racing').active, false, '授权后被替换的包不能被旧请求激活');
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

test('版本不兼容回执保留精确诊断，未豁免不发起预览；豁免后仍走 daemon 安装', async (t) => {
  const { createServer } = await import('node:http');
  const { createPluginManager } = await import('./runtime/plugin-manager.mjs');
  const policy = { runtimeVersion: '0.2.2-rc.1', runtimeId: '0123456789abcdef01234567', exemptions: {} };
  const server = createServer((_req, res) => {
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ name: 'fixture-incompatible', 'dist-tags': { latest: '1.0.0' }, versions: {
      '1.0.0': { name: 'fixture-incompatible', version: '1.0.0', engines: { dsh: '^9.0.0' }, dist: { tarball: 'https://example.test/unused.tgz' } },
    } }));
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => server.close(resolve)));
  const calls = [];
  const manager = createPluginManager({ plugins: {
    async compatibility() { return structuredClone(policy); },
    async preview(input) { calls.push('preview'); return { version: input.version, candidate: { digest: 'exact' } }; },
    async install(input) { calls.push('install'); assert.equal(input.expectedDigest, 'exact'); return { plugins: [{ manifest: { id: 'fixture', name: 'Fixture' }, source: { location: `dsh:fixture-incompatible@${input.version}` }, enabled: false }] }; },
  } }, `http://127.0.0.1:${server.address().port}`);
  const refused = await manager.installBundle('fixture-incompatible@1.0.0', { enabled: false });
  assert.equal(refused.application, 'failed');
  assert.equal(refused.error.code, 'incompatible-version');
  assert.deepEqual(refused.error.incompatible, [{ name: 'fixture-incompatible', version: '1.0.0', runtimeVersion: policy.runtimeVersion, requirements: { 'engines.dsh': '^9.0.0' } }]);
  assert.deepEqual(calls, []);
  policy.exemptions['fixture-incompatible@1.0.0'] = [policy.runtimeVersion];
  assert.equal((await manager.installBundle('fixture-incompatible@1.0.0', { enabled: false })).application, 'applied');
  assert.deepEqual(calls, ['preview', 'install']);
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
