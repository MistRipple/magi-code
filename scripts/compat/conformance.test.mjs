import test from 'node:test';
import assert from 'node:assert/strict';
import { usesComposerClient } from './translate/client-assets.mjs';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Context } from '@deepseek-ai/cordis';
import { Remote, RemoteError, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol';
import { classify } from './dsh/catalog.mjs';
import { hostDependencies } from './core/host-dependencies.mjs';
import { installDependencies } from './translate/dependencies.mjs';
import { installSharedResolution } from './runtime/shared-resolution.mjs';
import { invokeRemote } from './runtime/remote.mjs';
import { createSettingsScope } from './runtime/client/settings-scope.mjs';

const temporary = async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'magi-compat-contract-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
};
const json = (file, value) => writeFile(file, JSON.stringify(value));

test('发行运行库的导入分类和安装/执行解析共用实际清单，不把未携带的 dsh 包当成宿主包', async () => {
  const root = new URL('.', import.meta.url).pathname;
  for (const name of hostDependencies(root)) assert.notEqual(classify('imports', name).status, 'unknown', name);
  assert.ok(!hostDependencies(root).has('@deepseek-ai/dsh-unshipped'));
});

test('真实 Cordis 方法与报告对齐；不存在的 Reflect 方法仍拒绝', () => {
  const ctx = new Context();
  for (const method of ['get', 'set', 'provide', 'accessor', 'mixin']) {
    assert.equal(typeof ctx.reflect[method], 'function', method);
    assert.equal(classify('calls', `ctx.reflect.${method}`).status, 'shim');
  }
  assert.equal(classify('calls', 'ctx.reflect.notARealMethod').status, 'unknown');
});

test('普通 DSH 依赖沿原包解析，共享 Cordis 仍保持单实例', async (t) => {
  const root = await temporary(t), shared = join(root, 'shared'), upstream = join(root, 'upstream');
  for (const path of [join(shared, 'node_modules/@deepseek-ai/cordis'), join(upstream, 'node_modules/@deepseek-ai/dsh-local-helper')]) await mkdir(path, { recursive: true });
  await json(join(shared, 'package.json'), { dependencies: { '@deepseek-ai/cordis': '4.1.0' } });
  for (const [base, name, value] of [[shared, '@deepseek-ai/cordis', 'shared'], [upstream, '@deepseek-ai/dsh-local-helper', 'local']]) {
    const dir = join(base, 'node_modules', name);
    await json(join(dir, 'package.json'), { name, version: '1.0.0', type: 'module', main: 'index.js' });
    await writeFile(join(dir, 'index.js'), `export default ${JSON.stringify(value)};`);
  }
  await writeFile(join(upstream, 'entry.mjs'), "import core from '@deepseek-ai/cordis'; import helper from '@deepseek-ai/dsh-local-helper'; export default [core,helper];");
  const hooks = installSharedResolution(shared);
  try { assert.deepEqual((await import(join(upstream, 'entry.mjs'))).default, ['shared', 'local']); }
  finally { hooks.deregister(); }
});

test('可选 peer 缺席不会下载或冒充宿主已提供；必需的缺失宿主包仍明确失败', async (t) => {
  const root = await temporary(t), shared = join(root, 'shared');
  await mkdir(shared);
  await json(join(shared, 'package.json'), { dependencies: {} });
  await json(join(root, 'package.json'), { name: 'generic-plugin', version: '1.0.0', peerDependencies: { '@deepseek-ai/dsh-optional': '1.0.0' }, peerDependenciesMeta: { '@deepseek-ai/dsh-optional': { optional: true } } });
  assert.deepEqual(await installDependencies(root, shared, 'http://127.0.0.1:1'), [{ name: '@deepseek-ai/dsh-optional', optional: true, skipped: 'optional-peer' }]);
  await json(join(shared, 'package.json'), { dependencies: { '@deepseek-ai/dsh-optional': '1.0.0' } });
  await assert.rejects(installDependencies(root, shared), /缺少宿主依赖/);
});

function mark(prototype, method, options = method) {
  Remote(options)(undefined, { name: method, private: false, static: false, addInitializer(fn) { fn.call(Object.create(prototype)); } });
}
async function remoteHost(t) {
  const ctx = new Context();
  t.after(() => ctx.fiber.dispose());
  class Status extends TypertRemoteService {
    constructor(c) { super(c, 'pluginStatus', { namespace: 'status' }); }
    async echo(request) { return { received: request }; }
    secret() { return 'unmarked'; }
    fail() { throw new RemoteError('plugin/not-ready', 'not ready', { retry: true }); }
    async cancelled(request, signal) { signal?.throwIfAborted(); return request; }
    invalid() { return new Date(); }
    stream() { return []; }
  }
  for (const method of ['echo', 'fail', 'cancelled', 'invalid']) mark(Status.prototype, method);
  mark(Status.prototype, 'stream', { mode: 'stream' });
  const fiber = ctx.plugin(Status);
  await fiber.await();
  return { ctx, fiber };
}

test('Remote 通过真实 Cordis 服务调用具名 JSON 方法，保留上游错误码与详情', async (t) => {
  const { ctx } = await remoteHost(t);
  assert.deepEqual(await invokeRemote(ctx, { endpoint: 'status/echo', args: { request: { text: 'hello', enabled: false } } }), { ok: true, value: { received: { text: 'hello', enabled: false } } });
  assert.deepEqual((await invokeRemote(ctx, { endpoint: 'status/fail', args: {} })).error, { code: 'plugin/not-ready', message: 'not ready', details: { retry: true } });
});

test('Remote 拒绝未声明方法、越界 namespace、额外参数、非 JSON、流与取消调用', async (t) => {
  const { ctx } = await remoteHost(t);
  for (const input of [
    { endpoint: 'status/secret', args: {} }, { endpoint: 'sessions/clear', args: {} },
    { endpoint: 'status/echo', args: { request: 1, extra: 2 } }, { endpoint: 'status/echo', args: {} },
    { endpoint: 'status/echo', args: { request: undefined } }, { endpoint: '../echo', args: {} },
    { endpoint: 'status/invalid', args: {} }, { endpoint: 'status/stream', args: {} },
  ]) assert.equal((await invokeRemote(ctx, input)).ok, false, JSON.stringify(input));
  const signal = AbortSignal.abort(new Error('cancelled'));
  assert.equal((await invokeRemote(ctx, { endpoint: 'status/cancelled', args: { request: 1 } }, signal)).error.code, 'gateway/cancelled');
});

test('Remote 每次解析活跃服务，卸载后不可复用旧实例', async (t) => {
  const { ctx, fiber } = await remoteHost(t);
  await fiber.dispose();
  assert.equal((await invokeRemote(ctx, { endpoint: 'status/echo', args: { request: 1 } })).error.code, 'gateway/method-unavailable');
});

test('通用设置表单保持稳定快照，多字段保存只提交一次 CAS，冲突后保留 daemon 新值', async () => {
  let record = { version: 4, value: { title: 'first' } };
  const writes = [];
  const request = async (input) => {
    if (input.operation === 'settings.read') return structuredClone(record);
    if (input.operation === 'settings.describe') return { schema: { type: 'object', default: {}, properties: { title: { default: 'base' }, timeout: { default: 30 } } } };
    writes.push(input);
    if (input.expectedVersion !== record.version) throw new Error('conflict');
    record = { version: record.version + 1, value: structuredClone(input.value) };
    return structuredClone(record);
  };
  const { scope, refresh } = createSettingsScope(request);
  assert.equal(scope.getSnapshot().status, 'loading');
  await refresh();
  assert.strictEqual(scope.getSnapshot(), scope.getSnapshot());
  assert.deepEqual(scope.getSnapshot().value, { title: 'first', timeout: 30 });
  assert.ok(await scope.mutate([{ op: 'set', path: ['title'], value: 'saved' }, { op: 'set', path: ['timeout'], value: 60 }], 4));
  assert.equal(writes.length, 1);
  record = { version: 6, value: { title: 'external' } };
  assert.equal(await scope.mutate([{ op: 'set', path: ['title'], value: 'stale' }], 5), false);
  assert.equal(scope.getSnapshot().value.title, 'external');
  assert.ok(await scope.mutate([{ op: 'unset', path: ['title'] }], 6));
  assert.equal(scope.getSnapshot().value.title, 'base');
  assert.deepEqual(scope.getSnapshot().user, {});
  await assert.rejects(scope.mutate([{ op: 'set', path: ['__proto__', 'polluted'], value: true }]), /路径/);
  assert.deepEqual(await Promise.all([scope.set('title', 'queued'), scope.set('timeout', 45)]), [true, true]);
  assert.deepEqual(scope.getSnapshot().value, { title: 'queued', timeout: 45 });
  assert.equal(await scope.unset('timeout'), true);
  assert.deepEqual(scope.getSnapshot().value, { title: 'queued', timeout: 30 });
});
test('图片端口权限识别支持压缩变量、属性和解构，不匹配注释与字符串', () => {
  for (const source of ['function apply(c){ c.magiComposer.addImages([]); }', 'const p=c["magiComposer"];', 'const {magiComposer: images}=c;', 'const {magiComposer}=c;']) assert.equal(usesComposerClient(source), true);
  assert.equal(usesComposerClient('// ctx.magiComposer\nconst label="ctx.magiComposer";'), false);
  assert.throws(()=>usesComposerClient('const { =;'), /语法/);
});
