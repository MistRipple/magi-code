import test from 'node:test';
import assert from 'node:assert/strict';
import { usesComposerClient } from './install/client-assets.mjs';
import { translateClientBusiness } from './install/client-business.mjs';
import { createSessionComposerServices } from './runtime/client/session-composer.mjs';
import { createComposerClient } from './runtime/client/composer.mjs';
import vm from 'node:vm';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { Context } from '@deepseek-ai/cordis';
import { Remote, RemoteError, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol';
import { classify } from './dsh/catalog.mjs';
import { hostDependencies } from './core/host-dependencies.mjs';
import { installDependencies } from './install/dependencies.mjs';
import { installSharedResolution } from './runtime/shared-resolution.mjs';
import { invokeRemote } from './runtime/remote.mjs';
import { createSettingsScope } from './runtime/client/settings-scope.mjs';
import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { create as createTar } from 'tar';
import { installRegistryFetch } from './runtime/registry-fetch.mjs';
import { prepareDsh } from './install/prepare.mjs';
import { backendEntry } from './core/package.mjs';
import { createViewTransport } from './runtime/client/view-transport.mjs';
import { createRouteFetch } from './runtime/client/route-fetch.mjs';
import { getEventListeners } from 'node:events';
import { runRoute } from './runtime/routes.mjs';
import { decodeRouteBytes, encodeRouteBytes, readRouteBytes, MAX_ROUTE_BINARY_BYTES } from './runtime/route-payload.mjs';

const temporary = async (t) => {
  const root = await mkdtemp(join(tmpdir(), 'magi-compat-contract-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
};
const json = (file, value) => writeFile(file, JSON.stringify(value));

test('页面取消只结束对应路由，迟到回执不污染后续调用，关闭文档释放等待和信号监听', async () => {
  const sent = [], transport = createViewTransport(message => sent.push(message));
  const first = new AbortController(), second = new AbortController();
  const route = { operation: 'route.call', method: 'GET', path: '/slow' };
  const cancelled = transport.request(route, first.signal);
  const other = transport.request(route, second.signal);
  const reason = new Error('user cancelled');
  first.abort(reason);
  await assert.rejects(cancelled, error => error === reason);
  assert.deepEqual(sent[2], { type: 'cancel', requestId: sent[0].requestId });
  assert.equal(getEventListeners(first.signal, 'abort').length, 0);
  transport.receive({ requestId: sent[0].requestId, ok: true, result: 'late' });
  transport.receive({ requestId: sent[1].requestId, ok: true, result: 'current' });
  assert.equal(await other, 'current');
  assert.equal(getEventListeners(second.signal, 'abort').length, 0);
  const count = sent.length;
  await assert.rejects(transport.request(route, first.signal), error => error === reason);
  assert.equal(sent.length, count, '已取消的输入不能发起业务');
  const closing = transport.request(route, second.signal);
  transport.dispose(); transport.dispose();
  await assert.rejects(closing, { name: 'AbortError' });
  assert.equal(getEventListeners(second.signal, 'abort').length, 0);
  await assert.rejects(transport.request(route), { name: 'AbortError' });
  const writes = [], live = createViewTransport(message => writes.push(message));
  const controller = new AbortController();
  const writing = live.request({ operation: 'settings.write' }, controller.signal);
  controller.abort(); await assert.rejects(writing, { name: 'AbortError' });
  assert.equal(writes.length, 1, '停止等待不伪装为设置提交已取消');
});

test('页面 fetch 使用真实 Request 输入、相对路径和信号优先级，保留状态与 HEAD 空正文', async () => {
  const calls = [];
  const fetchRoute = createRouteFetch(async (message, signal) => {
    signal.throwIfAborted(); calls.push(message);
    return { status: 202, contentType: 'application/json', body: { accepted: true } };
  }, 'https://magi.example/ui-v/digest/ui/index.html');
  const old = new AbortController(); old.abort();
  const active = new AbortController();
  const input = new Request('https://magi.example/run?q=1', { method: 'POST', headers: { 'x-plugin-key': 'test' }, body: '{"value":2}', signal: old.signal });
  const response = await fetchRoute(input, { signal: active.signal });
  assert.equal(response.status, 202); assert.deepEqual(await response.json(), { accepted: true });
  assert.deepEqual(calls[0], { operation: 'route.call', method: 'POST', path: '/run', query: 'q=1', headers: { 'content-type': 'text/plain;charset=UTF-8', 'x-plugin-key': 'test' }, bodyBase64: Buffer.from('{"value":2}').toString('base64') });
  await fetchRoute('action'); assert.equal(calls[1].path, '/ui-v/digest/ui/action');
  const head = await fetchRoute('/status', { method: 'HEAD' });
  assert.equal(head.body, null); assert.equal(await head.text(), '');
  await assert.rejects(fetchRoute('/never', { signal: old.signal }), { name: 'AbortError' });
  await assert.rejects(fetchRoute('https://foreign.example/run'), /只能访问/);
  await assert.rejects(fetchRoute('/never', { method: 'GET', body: 'invalid' }), TypeError);
  assert.equal(calls.length, 3);
});

test('fetch 的两种 signal 输入沿同一个传输取消，后续请求正常完成', async () => {
  const sent = [], transport = createViewTransport(message => sent.push(message));
  const fetchRoute = createRouteFetch(transport.request, 'https://magi.example/ui/index.html');
  for (const asRequest of [false, true]) {
    const controller = new AbortController();
    const pending = asRequest ? fetchRoute(new Request('https://magi.example/slow', { signal: controller.signal })) : fetchRoute('/slow', { signal: controller.signal });
    const requestId = sent.at(-1).requestId;
    controller.abort(); await assert.rejects(pending, { name: 'AbortError' });
    assert.deepEqual(sent.at(-1), { type: 'cancel', requestId });
    transport.receive({ requestId, ok: true, result: { status: 200, body: 'late' } });
  }
  const current = fetchRoute('/retry');
  transport.receive({ requestId: sent.at(-1).requestId, ok: true, result: { status: 200, body: 'actual' } });
  assert.equal(await (await current).text(), 'actual');
  transport.dispose();
});

test('原包 fetch 二进制上传、multipart、图片与下载共用路由载荷，保留字节、类型和应用响应头', async () => {
  const bytes = Uint8Array.from([0, 255, 128, 13, 10, 0, 195, 40]);
  const seen = [];
  const definition = { async handler(req, res) {
    const parts = []; for await (const part of req) parts.push(part);
    const body = Buffer.concat(parts);
    seen.push({ bytes: body, headers: req.headers });
    res.writeHead(201, { 'content-type': 'image/png', 'content-disposition': 'attachment; filename="result.png"',
      'x-plugin-result': 'actual', 'set-cookie': 'must-not-reach-magi=1', 'content-length': String(body.length) });
    res.write(new Uint8Array(body.subarray(0, 3))); res.end(body.subarray(3));
  } };
  const fetchRoute = createRouteFetch((input, signal) => runRoute(definition, input, { signal }), 'https://magi.example/ui/index.html');
  const response = await fetchRoute('/upload', { method: 'POST', body: new Blob([bytes], { type: 'application/octet-stream' }) });
  assert.deepEqual(new Uint8Array(await response.arrayBuffer()), bytes);
  assert.deepEqual(seen[0].bytes, Buffer.from(bytes));
  assert.equal(seen[0].headers['content-length'], String(bytes.length));
  assert.equal(response.status, 201);
  assert.equal(response.headers.get('content-type'), 'image/png');
  assert.equal(response.headers.get('content-disposition'), 'attachment; filename="result.png"');
  assert.equal(response.headers.get('x-plugin-result'), 'actual');
  // 这里只验证上游处理器与 Fetch；不借单元驱动冒充 daemon 的响应头裁决。

  const form = new FormData(); form.append('description', '中文'); form.append('image', new File([bytes], 'sample.bin'));
  const request = new Request('https://magi.example/upload', { method: 'POST', body: form });
  const expected = Buffer.from(await request.clone().arrayBuffer());
  await fetchRoute(request);
  assert.deepEqual(seen[1].bytes, expected, 'multipart 分隔符和文件字节不能经过文本/JSON 重写');
  assert.equal(seen[1].headers['content-type'], request.headers.get('content-type'));

  const fromResource = createRouteFetch((input, signal) => runRoute({ resource: () => new Response(bytes, {
    headers: { 'content-type': 'application/zip', 'x-package-version': '1.0' },
  }) }, input, { signal }), 'https://magi.example/');
  const resource = await fromResource('/archive');
  assert.deepEqual(new Uint8Array(await resource.arrayBuffer()), bytes);
  assert.equal(resource.headers.get('x-package-version'), '1.0');
  const head = await fromResource('/archive', { method: 'HEAD' });
  assert.equal(head.body, null); assert.equal(head.headers.get('content-type'), 'application/zip');
});

test('路由字节载荷在业务前拒绝伪造、冲突和超限，流读取取消即时释放且不留下监听', async () => {
  const signal = new AbortController().signal;
  let invoked = 0;
  const definition = { handler(_req, res) { invoked++; res.end('ok'); } };
  for (const bodyBase64 of ['?', 'Zg=', 'Zh==', 'Zg==\n', 'A'.repeat(Math.ceil(MAX_ROUTE_BINARY_BYTES / 3) * 4 + 4)]) {
    await assert.rejects(runRoute(definition, { method: 'POST', path: '/', bodyBase64 }, { signal }), /base64|载荷/);
  }
  await assert.rejects(runRoute(definition, { method: 'POST', path: '/', bodyBase64: '', body: {} }, { signal }), /同时/);
  await assert.rejects(runRoute(definition, { method: 'HEAD', path: '/', bodyBase64: '' }, { signal }), /不能携带正文/);
  assert.equal(invoked, 0);
  const max = new Uint8Array(MAX_ROUTE_BINARY_BYTES).fill(255);
  assert.deepEqual(decodeRouteBytes(encodeRouteBytes(max)), max);
  let sent = 0;
  const client = createRouteFetch(async () => { sent++; return { status: 200 }; }, 'https://magi.example/');
  await assert.rejects(client('/', { method: 'POST', body: new Uint8Array(MAX_ROUTE_BINARY_BYTES + 1) }), /大小上限/);
  assert.equal(sent, 0);
  const controller = new AbortController();
  let closed = 0;
  const stream = new ReadableStream({ cancel() { closed++; } });
  const reading = readRouteBytes(stream, controller.signal);
  controller.abort();
  await assert.rejects(reading, { name: 'AbortError' });
  assert.equal(closed, 1); assert.equal(stream.locked, false);
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  const deferred = new AbortController();
  let release, released = 0;
  const pending = runRoute({ resource: () => new Promise(resolve => { release = resolve; }) }, { method: 'GET', path: '/delayed' }, { signal: deferred.signal });
  await Promise.resolve(); deferred.abort();
  await assert.rejects(pending, error => error.kind === 'Cancelled');
  release(new Response(new ReadableStream({ cancel() { released++; } })));
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(released, 1, '取消后的迟到资源响应释放自己的流，不交给失效页面');
  assert.equal(getEventListeners(deferred.signal, 'abort').length, 0);
});

test('原包封装只观察默认启动，扫描与运行共用条件导出，原生页面字节保留且归档可复现', async (t) => {
  const root = await temporary(t), upstream = join(root, 'upstream'); await mkdir(upstream);
  const sharedRoot = new URL('.', import.meta.url).pathname;
  await json(join(upstream, 'package.json'), { name: 'dsh-cleanup-contract', version: '1.0.0', type: 'module',
    main: 'unused.js', exports: { '.': { node: { import: './index.js' }, default: './unused.js' }, './client': './client.js' },
    peerDependencies: { '@deepseek-ai/cordis': '4.1.0' } });
  await writeFile(join(upstream, 'index.js'), `
    export const inject = ['hmr'];
    export const Config = value => ({ enabled: false, ...value });
    Config.toJSON = () => ({ uid: 1, refs: { 1: { type: 'object', dict: { enabled: 2 } }, 2: { type: 'boolean', meta: { default: false } } } });
    export function apply(ctx, config) {
      if (typeof ctx.hmr.reconcileProfile !== 'function') throw new Error('profile 重载服务缺失');
      if (config.enabled) throw new Error('安装期不能改写作者默认配置');
      ctx.inject(['settings'], c => { if (config.enabled) c.settings.register('late', value => value ?? {}); });
    }
  `);
  const client = `/* preserve page */ window.__ModuleLoader__.load({ id: 'contract', factory: require => {
    const getModules = () => require;
    return { apply(ctx) {
      const sdk = getModules()('@deepseek-ai/dsh-client-ui-primitives');
      if (typeof sdk.Modal !== 'function' || !sdk.Button) throw new Error('公共页面 SDK 缺失');
      ctx.slots.register({ name: 'settings.section', id: 'contract' }, () => null);
    } };
  } });`;
  await writeFile(join(upstream, 'client.js'), client);
  const prepared = [];
  for (const name of ['first', 'second']) {
    const outDir = join(root, name);
    const result = await prepareDsh({ upstreamDir: upstream, outDir, sharedRoot, source: { integrity: 'contract' } });
    assert.deepEqual(result.manifest.contributions.tools, []);
    assert.deepEqual(result.manifest.contributions.commands, []);
    assert.ok(result.manifest.permissions.some(permission => permission.kind === 'storage' && permission.targets.includes('dsh-registered-settings')));
    assert.deepEqual(result.manifest.permissions.find(permission => permission.kind === 'pluginManagement').targets, ['lifecycle', 'background'], '单独重载只声明生命周期授权');
    assert.equal(await readFile(join(outDir, 'ui/client.js'), 'utf8'), client);
    assert.match(await readFile(join(outDir, `ui/dsh-config-${result.manifest.contributions.views[0].id}.js`), 'utf8'), /ui-primitives\.js/);
    assert.ok((await readFile(join(outDir, 'ui/ui-primitives.js'))).length > 0);
    assert.equal(result.report.variants, undefined);
    assert.match(await readFile(join(outDir, 'extension/index.mjs'), 'utf8'), /settingsNamespaces":true/);
    const { packPlugin } = await import('./core/pack.mjs');
    const archive = join(root, `${name}.zip`); packPlugin(outDir, archive); prepared.push(await readFile(archive));
  }
  assert.deepEqual(prepared[0], prepared[1]);
  assert.equal(backendEntry({ exports: { default: './first.js', node: './second.js' } }), 'first.js');
  for (const exports of ['../outside.js', { '.': { browser: './browser.js' } }]) assert.throws(() => backendEntry({ exports }), /入口/);
});

test('npm 版本元数据遇到 406 只重新协商 JSON，保留取消与请求选项，其他服务和写入原样执行', async () => {
  const calls = [], registry = 'https://registry.example.test/npm/';
  const target = { async fetch(input, options) {
    const request = new Request(input, options); calls.push({ input, options, request });
    return new Response('metadata', { status: request.headers.get('accept') === 'application/json' ? 200 : 406 });
  } };
  const original = target.fetch, release = installRegistryFetch({ registry, target });
  const dispatcher = {}, controller = new AbortController();
  const options = { headers: { accept: 'application/vnd.npm.install-v1+json', authorization: 'test-token' }, signal: controller.signal, dispatcher };
  const result = await target.fetch(`${registry}dsh-example/latest`, options);
  assert.equal(result.status, 200); assert.equal(calls.length, 2);
  assert.equal(calls[0].options, options); assert.equal(calls[1].options.dispatcher, dispatcher);
  assert.equal(calls[1].request.headers.get('authorization'), 'test-token');
  assert.equal(calls[1].request.headers.get('accept'), 'application/json');
  for (const url of [`${registry}%40scope%2fplugin/1.0.0`, `${registry}@scope/plugin/next`]) {
    calls.length = 0; assert.equal((await target.fetch(new Request(url, options))).status, 200); assert.equal(calls.length, 2);
  }
  for (const [url, init] of [
    ['https://other.example.test/npm/plugin/latest', options], ['https://registry.example.test/other/plugin/latest', options],
    [`${registry}plugin/-/file.tgz`, options], [`${registry}plugin/latest`, { ...options, method: 'POST', body: 'write' }],
    [`${registry}plugin/latest`, { headers: { accept: 'text/plain' } }],
  ]) {
    calls.length = 0; assert.equal((await target.fetch(url, init)).status, 406); assert.equal(calls.length, 1);
    assert.equal(calls[0].input, url); assert.equal(calls[0].options, init);
  }
  controller.abort(); assert.equal(calls.length, 1);
  release(); assert.equal(target.fetch, original);
  let count = 0;
  const aborted = { async fetch() { count++; controller.abort(); return new Response('', { status: 406 }); } };
  const restore = installRegistryFetch({ registry, target: aborted });
  await assert.rejects(aborted.fetch(`${registry}plugin/latest`, options), { name: 'AbortError' });
  assert.equal(count, 1, '取消后不能补发元数据请求'); restore();
});

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
  assert.equal(classify('imports', '@deepseek-ai/cordis/package.json').status, 'bundled');
  assert.equal(classify('imports', '@deepseek-ai/cordis/unknown-api').status, 'unknown');
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

test('Cordis peer 在线隔离安装，主入口、子路径和 CJS 元数据使用同一版本，宿主不被污染', async (t) => {
  const root = await temporary(t), shared = join(root, 'shared'), upstream = join(root, 'upstream');
  const name = '@deepseek-ai/cordis';
  // 发布物来自独立本地 registry，校验和及安装流程与 npm 一致。
  // 宿主预发布版不满足 ^4.0.0，但 registry 的稳定版满足，不能按 minVersion 猜测而拒绝。
  const source = join(root, 'source'); await mkdir(source);
  await json(join(source, 'package.json'), { name, version: '4.1.1', type: 'module', exports: {
    '.': { import: './index.js', require: './index.cjs' }, './package.json': './package.json', './feature': './feature.js',
  } });
  await writeFile(join(source, 'index.js'), "export default 'local';");
  await writeFile(join(source, 'index.cjs'), "module.exports = 'local-cjs';");
  await writeFile(join(source, 'feature.js'), "export default 'local-feature';");
  const archive = join(root, 'cordis.tgz');
  await createTar({ cwd: source, gzip: true, file: archive, prefix: 'package' }, ['package.json', 'index.js', 'index.cjs', 'feature.js']);
  const tarball = await readFile(archive);
  const integrity = `sha512-${createHash('sha512').update(tarball).digest('base64')}`;
  let registry, metadataRequests = 0, archiveRequests = 0, rejectManifest = false;
  const server = createServer((request, response) => {
    if (request.url === '/cordis.tgz') {
      archiveRequests++;
      if (archiveRequests === 1) { response.writeHead(503); response.end('temporary download failure'); return; }
      response.end(tarball); return;
    }
    metadataRequests++;
    if (rejectManifest) { response.writeHead(404); response.end('not found'); return; }
    if (metadataRequests === 1) { request.socket.destroy(); return; }
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ name, 'dist-tags': { latest: '4.1.1' }, versions: {
      '4.1.1': { name, version: '4.1.1', dist: { tarball: `${registry}/cordis.tgz`, integrity } },
    } }));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  registry = `http://127.0.0.1:${server.address().port}`;
  const host = join(shared, 'node_modules', name); await mkdir(host, { recursive: true });
  await json(join(shared, 'package.json'), { dependencies: { [name]: '4.2.0-rc.1' } });
  await json(join(host, 'package.json'), { name, version: '4.2.0-rc.1', type: 'module', exports: { '.': './index.js', './package.json': './package.json', './feature': './feature.js' } });
  await writeFile(join(host, 'index.js'), "export default 'host';");
  await writeFile(join(host, 'feature.js'), "export default 'host-feature';");
  await mkdir(upstream);
  await json(join(upstream, 'package.json'), { name: 'peer-plugin', version: '1.0.0', peerDependencies: { [name]: '^4.0.0' } });
  const installed = await installDependencies(upstream, shared, registry);
  assert.equal(metadataRequests, 2, '元数据连接断开由同一网络层恢复一次');
  assert.equal(archiveRequests, 2, '依赖归档 503 由同一网络层恢复一次，不重跑安装');
  rejectManifest = true;
  await assert.rejects(installDependencies(upstream, shared, registry), error => error.code === 'E404');
  assert.equal(metadataRequests, 3, '缺失版本属于明确失败，不进入网络重试');
  assert.equal(archiveRequests, 2, '失败读取不能重做已验证的归档安装');
  assert.deepEqual(installed, [{ name, version: '4.1.1', integrity, host: false, isolated: true }]);
  await writeFile(join(upstream, 'entry.mjs'), `import core from '${name}'; import feature from '${name}/feature'; import metadata from '${name}/package.json' with { type: 'json' }; import {createRequire} from 'node:module'; const require=createRequire(import.meta.url); export default [core, feature, metadata.version, require('${name}'), require('${name}/package.json').version];`);
  await writeFile(join(shared, 'entry.mjs'), `import core from '${name}'; import feature from '${name}/feature'; export default [core, feature];`);
  // 上游目录之外的同名 node_modules 不能改变共享运行库的身份。
  const ambient = join(root, 'node_modules', name); await mkdir(ambient, { recursive: true });
  await json(join(ambient, 'package.json'), { name, version: '9.0.0', type: 'module', main: 'index.js' });
  await writeFile(join(ambient, 'index.js'), "export default 'ambient';");
  const adapter = join(root, 'adapter'); await mkdir(adapter);
  await writeFile(join(adapter, 'entry.mjs'), `import core from '${name}'; export default core;`);
  const hooks = installSharedResolution(shared, { '@magi/compat-upstream-root': upstream });
  try {
    assert.deepEqual((await import(join(upstream, 'entry.mjs'))).default, ['local', 'local-feature', '4.1.1', 'local-cjs', '4.1.1']);
    assert.deepEqual((await import(join(shared, 'entry.mjs'))).default, ['host', 'host-feature']);
    assert.equal((await import(join(adapter, 'entry.mjs'))).default, 'host', '原包之外的适配器不使用物理祖先中的同名依赖');
  } finally { hooks.deregister(); }
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

test('DSH 会话图片业务调用只在静态可证明路径上转换为异步真实回执', async () => {
  const source = 'function apply(ctx){ const input=ctx.conversation.input.for(ctx.sessions.scope("s")); const {addAttachments}=input; return addAttachments(["draft"]); }';
  const transformed = translateClientBusiness(source);
  assert.equal(transformed.operations.includes('addAttachments'), true);
  assert.match(transformed.code, /addAttachmentsAsync/);
  assert.match(transformed.code, /await/);
  assert.doesNotThrow(() => new vm.Script(transformed.code));
  assert.throws(() => translateClientBusiness('function apply(ctx){ const x=ctx.conversation.input.for(scope); return x[method](ids); }'), /动态|逃逸|未支持/);

  const snapshots = [{ sessionId: 's', attachmentIds: [], writable: true }];
  const requests = [];
  const composer = createComposerClient(async (message) => {
    requests.push(message);
    if (message.operation === 'composer.snapshot') return snapshots[0];
    if (message.operation === 'composer.addImages') return { accepted: true, ids: ['attachment-1'], snapshot: { sessionId: 's', attachmentIds: ['attachment-1'], writable: true } };
    return { removed: true, snapshot: { sessionId: 's', attachmentIds: [], writable: true } };
  });
  composer.update(snapshots[0]);
  const facade = createSessionComposerServices(composer.api);
  const [draft] = facade.conversation.createDrafts('s', [new File(['x'], 'x.png', { type: 'image/png' })]);
  const scope = facade.sessions.scope('s');
  assert.equal(await facade.conversation.input.for(scope).addAttachmentsAsync([draft.id]), true);
  assert.deepEqual(facade.conversation.input.for(scope).snapshot.attachmentIds, [draft.id]);
  facade.dispose(); composer.dispose();
  assert.ok(requests.some((request) => request.operation === 'composer.addImages'));
});

test('业务转换后的同步布尔分支等待宿主，失败回执不会变成 truthy Promise', async () => {
  const original = 'function apply(c){ const input=c.conversation.input.for(scope); function add(){return input.addAttachments(["draft"])} return add() ? "mounted" : "busy"; }';
  const transformed = translateClientBusiness(original);
  let acknowledge;
  const context = vm.createContext({ scope: {}, ctx: { conversation: { input: { for: () => ({ addAttachmentsAsync: () => new Promise((resolve) => { acknowledge = resolve; }) }) } } } });
  new vm.Script(`${transformed.code}; globalThis.pending=apply(ctx);`).runInContext(context);
  let settled = false;
  const pending = context.pending.then((value) => { settled = true; return value; });
  await Promise.resolve(); assert.equal(settled, false);
  acknowledge(false); assert.equal(await pending, 'busy');
  assert.throws(() => translateClientBusiness('function apply(ctx){const input=ctx.conversation.input.for(scope);function Render(){return input.addAttachments(ids)} React.createElement(Render)}'), /逃逸|渲染/);
  assert.throws(() => translateClientBusiness('function apply(ctx){const input=ctx.conversation.input.for(scope); save({input}); }'), /逃逸/);
  assert.throws(() => translateClientBusiness('function apply(ctx){const input=ctx.conversation.input.for(scope); return ()=>external(input); }'), /逃逸/);
  assert.equal(translateClientBusiness('function apply(ordinary){return ordinary.other.addAttachments(ids)}').awaitedCalls, 0);
  const markup = 'const style=".view { color: red }"; function View(){return h("a",{href:"https://example.test"},"下载 Ghidra")}; function apply(ctx){ const input=ctx.conversation.input.for(scope); return input.addAttachments(ids)}';
  assert.ok(translateClientBusiness(markup).code.startsWith(markup.slice(0,markup.indexOf('function apply'))));
});

test('DSH 草稿端口拒绝同步真值、跨会话/伪造身份，并在宿主发送或移除后回收引用', async () => {
  let active = { sessionId: 'first', attachmentIds: [], writable: true }, listener, accepted = false;
  const composer = {
    getSnapshot: () => active,
    subscribe(fn) { listener = fn; return () => { listener = undefined; }; },
    async addImages(_files, sessionId) { assert.equal(sessionId, 'first'); return { accepted, ids: accepted ? ['host-id'] : [], snapshot: active }; },
    async removeImage(id, sessionId) { assert.equal(id, 'host-id'); assert.equal(sessionId, 'first'); active = {...active,attachmentIds:[]}; listener(); return { removed: true, snapshot: active }; },
    async focus() {}, async notify() {},
  };
  const facade = createSessionComposerServices(composer);
  const scope = facade.sessions.scope('first'), input = facade.conversation.input.for(scope);
  assert.strictEqual(facade.sessions.list.getSnapshot(),facade.sessions.list.getSnapshot());
  assert.throws(()=>input.addAttachments([]),/同步/);
  assert.throws(()=>facade.conversation.input.for({}),/不属于/);
  const [draft] = facade.conversation.createDrafts('first',[new File(['x'],'x.png',{type:'image/png'})]);
  assert.equal(await input.addAttachmentsAsync([draft.id]),false);
  assert.deepEqual(input.snapshot.attachmentIds,[]);
  accepted=true; active={...active,attachmentIds:['host-id']};
  assert.equal(await input.addAttachmentsAsync([draft.id]),true);
  active={...active,sessionId:'second',attachmentIds:[]}; listener();
  assert.throws(()=>input.snapshot,/不属于/);
  active={...active,sessionId:'first',attachmentIds:['host-id']}; listener();
  assert.equal(await input.removeAttachmentAsync(draft.id),true);
  assert.deepEqual(input.snapshot.attachmentIds,[]);
  assert.equal(facade.conversation.resolveDraftAttachments([draft.id]).length,1,'移除挂载不等于释放未发送 File 草稿');
  facade.conversation.releaseDraftAttachment(draft.id);
  assert.deepEqual(facade.conversation.resolveDraftAttachments([draft.id]),[]);
  facade.dispose(); assert.throws(()=>input.snapshot,/失效|不属于/);
});
