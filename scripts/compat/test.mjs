import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { TranslatorRegistry } from './core/registry.mjs';
import { renderText } from './core/report.mjs';
import { createRegistry } from './index.mjs';

const fixture = fileURLToPath(new URL('./fixtures/mini-dsh', import.meta.url));
const cli = fileURLToPath(new URL('./cli.mjs', import.meta.url));
const find = (report, name) => report.entries.find((entry) => entry.name === name);

test('运行组件身份可复现，同时覆盖业务转换源码与实际页面产物', async t => {
  const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { writeTranslationRuntime, translationRuntimeId } = await import('./core/translation-runtime.mjs');
  const root = mkdtempSync(join(tmpdir(), 'magi-translation-identity-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const name of ['cli.mjs', 'index.mjs', 'package.json', 'package-lock.json']) writeFileSync(join(root, name), name);
  for (const directory of ['core', 'dsh', 'pi', 'runtime', 'install', 'dist']) mkdirSync(join(root, directory));
  writeFileSync(join(root, 'runtime/host.mjs'), 'first');
  writeFileSync(join(root, 'dist/host.js'), 'page');
  const original = writeTranslationRuntime(root);
  assert.equal(translationRuntimeId(root), original.id);
  assert.deepEqual(writeTranslationRuntime(root), original, '生成的身份本身不参与下一次身份计算');
  writeFileSync(join(root, 'runtime/host.mjs'), 'second');
  assert.notEqual(writeTranslationRuntime(root).id, original.id);
  writeFileSync(join(root, 'runtime/host.mjs'), 'first');
  writeFileSync(join(root, 'dist/host.js'), 'updated page');
  assert.notEqual(writeTranslationRuntime(root).id, original.id);
  writeFileSync(join(root, 'dist/translation-runtime.json'), '{}');
  assert.throws(() => translationRuntimeId(root), /身份无效/);
});

test('可复现归档只携带原生辅助文件的执行意图，普通源码不会继承执行位', async (t) => {
  const { mkdtempSync, mkdirSync, writeFileSync, chmodSync, readFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { packPlugin } = await import('./core/pack.mjs');
  const root = mkdtempSync(join(tmpdir(), 'magi-pack-modes-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const tree = join(root, 'package'); mkdirSync(join(tree, 'extension/upstream/native/linux'), { recursive: true });
  writeFileSync(join(tree, 'manifest.json'), JSON.stringify({ id: 'fixture.native', version: '1.0.0' }));
  for (const [name, mode] of [['extension/index.mjs', 0o755], ['extension/upstream/native/linux/helper', 0o755], ['extension/upstream/native/linux/data', 0o644]]) {
    writeFileSync(join(tree, name), name); chmodSync(join(tree, name), mode);
  }
  const first = join(root, 'first.zip'), second = join(root, 'second.zip');
  packPlugin(tree, first); packPlugin(tree, second);
  const bytes = readFileSync(first); assert.deepEqual(bytes, readFileSync(second));
  const entries = new Map();
  for (let offset = 0; offset < bytes.length - 46; offset++) if (bytes.readUInt32LE(offset) === 0x02014b50) {
    const name = bytes.subarray(offset + 46, offset + 46 + bytes.readUInt16LE(offset + 28)).toString();
    entries.set(name, bytes.readUInt32LE(offset + 38) >>> 16);
  }
  assert.equal(entries.get('extension/index.mjs') & 0o7777, 0o644);
  assert.equal(entries.get('extension/upstream/native/linux/helper') & 0o7777, process.platform === 'win32' ? 0o644 : 0o755);
  assert.equal(entries.get('extension/upstream/native/linux/data') & 0o7777, 0o644);
});

async function inspectOptional(t, code) {
  const { mkdtempSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'magi-optional-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: 'optional-dsh', version: '1.0.0', main: 'index.js' }));
  writeFileSync(join(dir, 'index.js'), code);
  return createRegistry().create('dsh').inspect(dir, { required: ['session', 'hooks', 'tools'] });
}

test('DSH 可选服务保留缺失报告；必需依赖、未知服务和分支里的其他未支持服务仍阻塞', async (t) => {
  const optional = "export function apply(ctx) { ctx.inject(['sessionProjections'], c => c.sessionProjections.stateOf()); }";
  const report = await inspectOptional(t, optional);
  assert.equal(report.verdict.blocked.length, 0);
  assert.equal(find(report, 'sessionProjections').status, 'dropped');
  assert.equal(find(report, 'sessionProjections').optional, true);
  for (const code of [
    "export const inject = ['sessionProjections'];\n" + optional,
    "export function apply(ctx) { ctx.inject(['unknownService'], c => c.unknownService.read()); }",
    "export function apply(ctx) { ctx.inject(['tools'], c => c.compaction.run()); }",
  ]) assert.ok((await inspectOptional(t, code)).verdict.blocked.length > 0);
});

test('ctx.get 服务读取沿上下文辅助函数记录，供管理权限声明使用；普通同名对象不混入', async (t) => {
  const report = await inspectOptional(t, "export function apply(ctx) { lookup(ctx); const other = { get() {} }; other.get('unrelated'); } function lookup(c) { c.get('pluginManager'); c.get('loader'); }");
  assert.deepEqual(Object.keys(report.serviceReads), ['loader', 'pluginManager']);
  assert.equal(report.serviceReads.pluginManager.count, 1);
  assert.equal(report.verdict.blocked.length, 0, '读取可缺席服务不会被变成必需依赖');
});

test('profileContext 只读身份与包管理委托分开识别，条件取服务仍保留成员使用证据', async t => {
  const read = await inspectOptional(t, "export function apply(ctx) { const profile = ctx.get('profileContext'); console.log(profile.name, profile.dir); }");
  assert.equal(Object.hasOwn(read.serviceMembers, 'profileContext.packageManager'), false);
  const manager = await inspectOptional(t, "export function apply(ctx) { const profile = typeof ctx.get==='function' ? ctx.get('profileContext') : null; console.log(profile.packageManager); }");
  assert.equal(manager.serviceMembers['profileContext.packageManager'].count, 1);
  const direct = await inspectOptional(t, "export function apply(ctx) { console.log(ctx.profileContext.packageManager.command); }");
  assert.equal(direct.verdict.blocked.length, 0);
});

test('profile 重载只声明已提供的公共操作；文件监听和任意模块替换仍不能冒充已支持', async t => {
  const report = await inspectOptional(t, "export const inject=['hmr']; export function apply(ctx) { ctx.hmr.runExclusive(() => ctx.hmr.reconcileProfile()); }");
  assert.equal(report.verdict.blocked.length, 0);
  for (const method of ['watchConfig', 'reloadModule']) {
    const unsupported = await inspectOptional(t, `export function apply(ctx) { ctx.hmr.${method}(); }`);
    assert.ok(unsupported.verdict.blocked.length > 0);
  }
});

test('后端 sessions 命名空间准入不代表完整 Session 对象或写入方法已翻译', async(t)=>{
  assert.equal((await inspectOptional(t,"export const inject=['sessions']; export function apply(ctx) {}" )).verdict.blocked.length,0);
  for(const code of ["ctx.sessions.list()","ctx.sessions.get('s').header","ctx.sessions.create('s')","ctx.sessions.fork('s')"]){
    const report=await inspectOptional(t,`export const inject=['sessions']; export function apply(ctx) { ${code}; }`);
    assert.ok(report.verdict.blocked.length>0,code);
  }
});

test('DSH 原包的 createRequire.resolve 与 ESM 共享同一真实运行库解析', async (t) => {
  const { installSharedResolution } = await import('./runtime/shared-resolution.mjs');
  const { createRequire } = await import('node:module');
  const { mkdtemp, mkdir, writeFile, rm } = await import('node:fs/promises');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const root = await mkdtemp(join(tmpdir(), 'magi-runtime-metadata-')); t.after(() => rm(root, { recursive: true, force: true }));
  const shared = new URL('./', import.meta.url).pathname;
  await writeFile(join(root, 'entry.mjs'), "import {createRequire} from 'node:module'; export default () => createRequire(import.meta.url).resolve('@deepseek-ai/dsh-app-boot');");
  const hooks = installSharedResolution(shared, { '@magi/compat-upstream-root': root + '/' }); t.after(() => hooks.deregister());
  const resolved = (await import(join(root, 'entry.mjs'))).default();
  assert.equal(resolved, createRequire(new URL('./package.json', import.meta.url)).resolve('@deepseek-ai/dsh-app-boot'));
});

test('DSH 同步事件注册失败有明确 catch 时允许上游降级；无保护、异步与重抛仍阻塞', async (t) => {
  const report = await inspectOptional(t, "export function apply(ctx) { try { ctx.on('session/event', () => {}); } catch (error) { ctx.logger.warn(error); } }");
  assert.equal(report.verdict.blocked.length, 0);
  assert.equal(find(report, 'session/event').status, 'partial');
  for (const code of [
    "export function apply(ctx) { ctx.on('session/event', () => {}); }",
    "export function apply(ctx) { try { ctx.on('session/event', () => {}); } catch (error) { throw error; } }",
    "export function apply(ctx) { try { (() => ctx.on('session/event', () => {}))(); } catch {} }",
    "export function apply(ctx) { try { ctx.on('session/event', () => {}); } catch {} ctx.on('session/event', () => {}); }",
  ]) assert.ok((await inspectOptional(t, code)).verdict.blocked.length > 0);
});

test('DSH 交给普通函数的注册回调必须在所有可追踪调用点捕获，不把逃逸回调当可选', async (t) => {
  const mount = "export function apply(ctx) { mount({ subscribe: () => ctx.on('session/event', () => {}) }); }";
  const good = mount + "function mount(api) { read(api); try { api.subscribe(); } catch { return; } } function read(api) { return api.label; }";
  assert.equal((await inspectOptional(t, good)).verdict.blocked.length, 0);
  for (const helper of [
    'function mount(api) { api.subscribe(); }',
    'function mount(api) { try { api.subscribe(); } catch {} return api.subscribe; }',
    'function mount(api) { unknown(api); try { api.subscribe(); } catch {} }',
    'function mount(api) { nested(api); } function nested(api) { api.subscribe(); }',
  ]) assert.ok((await inspectOptional(t, mount + helper)).verdict.blocked.length > 0);
  assert.ok((await inspectOptional(t, good.replace('subscribe: ()', 'subscribe: async ()'))).verdict.blocked.length > 0);
});

test('DSH peer 沿上游准入策略计入预发布版本；越界版本、直接依赖和普通 peer 不放宽', async (t) => {
  const { installDependencies } = await import('./install/dependencies.mjs');
  const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const root = mkdtempSync(join(tmpdir(), 'magi-peer-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const shared = join(root, 'shared');
  const check = async (name, version, range, field = 'peerDependencies') => {
    const target = join(shared, 'node_modules', name); mkdirSync(target, { recursive: true });
    writeFileSync(join(shared, 'package.json'), JSON.stringify({ dependencies: { [name]: version } }));
    writeFileSync(join(target, 'package.json'), JSON.stringify({ name, version }));
    writeFileSync(join(root, 'package.json'), JSON.stringify({ name: 'peer-fixture', version: '1.0.0', [field]: { [name]: range } }));
    return installDependencies(root, shared);
  };
  const range = '>=0.1.7-rc.1 <0.3.0-0';
  assert.equal((await check('@deepseek-ai/dsh-llm', '0.2.2-rc.1', range))[0].version, '0.2.2-rc.1');
  await assert.rejects(check('@deepseek-ai/dsh-llm', '0.3.0-rc.1', range), /不满足/);
  await assert.rejects(check('@deepseek-ai/dsh-llm', '0.2.2-rc.1', range, 'dependencies'), /不满足/);
  await assert.rejects(check('react', '19.0.0', '^18.0.0'), /不满足/);
});

test('DSH 版本门禁保持 engines 优先与预发布语义；豁免只覆盖精确包版本和运行时', async () => {
  const { unmetDshRequirements, checkDshCompatibility } = await import('./core/dsh-compatibility.mjs');
  const environment = { runtimeVersion: '0.2.2-rc.1', runtimeId: '0123456789abcdef01234567' };
  const pkg = { name: '@scope/plugin', version: '1.0.0', engines: { dsh: '>=0.3' }, peerDependencies: { '@deepseek-ai/dsh-tools': '*' } };
  assert.deepEqual(unmetDshRequirements(pkg, environment.runtimeVersion), { 'engines.dsh': '>=0.3' });
  assert.equal(unmetDshRequirements({ ...pkg, engines: { dsh: '>=0.2.1 <0.3' }, peerDependencies: { '@deepseek-ai/dsh-tools': '0.1.0' } }, environment.runtimeVersion), null);
  assert.equal(unmetDshRequirements({ peerDependencies: { '@deepseek-ai/dsh-tools': 'workspace:^' } }, environment.runtimeVersion), null);
  assert.throws(() => checkDshCompatibility(pkg, environment), /版本声明不接受/);
  const policy = { ...environment, revision: 1, exemptions: { '@scope/plugin@1.0.0': [environment.runtimeVersion] } };
  assert.equal(checkDshCompatibility(pkg, environment, policy).exempted, true);
  assert.throws(() => checkDshCompatibility({ ...pkg, version: '1.0.1' }, environment, policy), /版本声明不接受/);
  assert.throws(() => checkDshCompatibility(pkg, { ...environment, runtimeId: 'abcdef0123456789abcdef01' }, policy), /运行时不一致/);
});

test('工厂按名称创建，重复注册、未知名称与非法名称都明确失败', () => {
  const registry = createRegistry();
  assert.deepEqual(registry.kinds(), ['dsh', 'pi']);
  assert.equal(registry.create('dsh').kind, 'dsh');
  assert.throws(() => registry.register('dsh', () => ({})), /已注册/);
  assert.equal(registry.create('pi').kind, 'pi');
  assert.throws(() => registry.create('nope'), /未注册的翻译器: nope/);
  assert.throws(() => registry.register('Bad Name', () => ({})), /名称无效/);
  assert.throws(() => new TranslatorRegistry().register('x', () => ({ kind: 'y' })).create('x'), /kind 与注册名不一致/);
});

test('按目录特征识别生态；没有把握或并列时不猜', async () => {
  const registry = createRegistry();
  assert.equal((await registry.detect(fixture)).kind, 'dsh');
  const { mkdtempSync, writeFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const unrelated = mkdtempSync(join(tmpdir(), 'magi-detect-'));
  writeFileSync(join(unrelated, 'package.json'), JSON.stringify({ name: 'unrelated', version: '1.0.0' }));
  await assert.rejects(registry.detect(unrelated), /无法识别/);
  // 新增生态只需注册工厂：两个同分翻译器必须要求显式指定。
  const twin = (kind) => () => ({ kind, detect: async () => 7, inspect: async () => ({}) });
  const ambiguous = new TranslatorRegistry().register('a', twin('a')).register('b', twin('b'));
  await assert.rejects(ambiguous.detect(fixture), /同时匹配 a 与 b/);
});

test('静态分析：每个能力都有明确状态，未登记的上游服务为 unknown 而非静默通过', async () => {
  const report = await createRegistry().create('dsh').inspect(fixture, { required: ['llm', 'tools'] });
  assert.equal(report.upstream.name, 'mini-dsh-plugin');
  assert.equal(find(report, 'ctx.logger.warn').status, 'shim');
  assert.equal(find(report, 'ctx.effect').status, 'shim');
  assert.equal(find(report, 'ctx.llm.registerAdapter').status, 'mapped');
  assert.equal(find(report, 'ctx.tools.register').status, 'mapped');
  assert.equal(find(report, 'ctx.webServer.register').status, 'mapped');
  assert.equal(find(report, 'node:fs/promises').status, 'bundled');
  assert.equal(find(report, './helper').category, 'internal');
  assert.equal(find(report, '@deepseek-ai/dsh-mystery').status, 'unknown');
  assert.equal(find(report, 'tools').kind, 'injects');
  assert.equal(Object.values(report.summary).reduce((a, b) => a + b, 0), report.entries.length);
});

test('必需能力判定：unknown 永远阻塞；partial 与 dropped 在被要求的类别里阻塞；已映射的不阻塞', async () => {
  const translator = createRegistry().create('dsh');
  const names = (report) => report.verdict.blocked.map((entry) => entry.name);
  const onlyLlm = await translator.inspect(fixture, { required: ['llm'] });
  assert.ok(!names(onlyLlm).includes('ctx.llm.registerAdapter'), 'llm 注册已映射，不应阻塞');
  assert.ok(names(onlyLlm).includes('@deepseek-ai/dsh-mystery'), 'unknown 永远阻塞');
  const ui = await translator.inspect(fixture, { required: ['ui'] });
  assert.ok(!names(ui).includes('ctx.webServer.register'), 'webServer.register 已映射到 api.route，不应阻塞');
  assert.ok(names(ui).includes('ctx.webServer.registerFallback'), 'SPA 回退在 Magi 没有对应物，ui 类别要求时阻塞');
  const none = await translator.inspect(fixture, { required: [] });
  assert.deepEqual(names(none), ['@deepseek-ai/dsh-mystery']);
  assert.match(renderText(ui), /阻塞翻译/);
});

test('命令行：可翻译返回 0 之外的码表示阻塞，输入错误返回 2', () => {
  const run = (...args) => spawnSync(process.execPath, [cli, ...args], { encoding: 'utf8' });
  const blocked = run('inspect', fixture, '--require', 'llm');
  assert.equal(blocked.status, 1);
  assert.match(blocked.stdout, /dropped/);
  assert.equal(run('inspect', fixture, '--kind', 'dsh', '--json').status, 1);
  assert.equal(run('inspect', fixture, '--kind', 'nope').status, 2);
  assert.equal(run('bogus').status, 2);
});

test('已构建的 npm 产物（没有 src/）从 main 入口沿相对导入扫描；扫描不到任何文件时明确失败，不给出“可翻译”', async () => {
  const { mkdtempSync, mkdirSync, writeFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'compat-built-'));
  mkdirSync(join(dir, 'lib'));
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: 'built-dsh', version: '1.0.0', main: 'lib/index.js', type: 'module', peerDependencies: { '@deepseek-ai/dsh-llm': '^0.2.0' } }));
  writeFileSync(join(dir, 'lib/index.js'), "import { inner } from './inner.js';\nexport function apply(ctx) { ctx.webServer.register('/x', inner); }\n");
  writeFileSync(join(dir, 'lib/inner.js'), 'export const inner = () => {};\n');
  const report = await createRegistry().create('dsh').inspect(dir, { required: [] });
  assert.equal(report.scanned.files, 2);
  assert.equal(find(report, 'ctx.webServer.register').status, 'mapped');

  const empty = mkdtempSync(join(tmpdir(), 'compat-empty-'));
  writeFileSync(join(empty, 'package.json'), JSON.stringify({ name: 'empty-dsh', version: '1.0.0', main: 'lib/index.js' }));
  await assert.rejects(createRegistry().create('dsh').inspect(empty, { required: [] }), /没有扫描到任何源文件/);
});

test('能力目录与 Magi 源码一致：钩子名/模式取自 protocol.rs，RPC 方法取自宿主与 daemon 分发；引用不存在的能力在加载时失败', async () => {
  const { CAPABILITIES } = await import('./core/capabilities.mjs');
  const { readHookCatalog, readHostMethods } = await import('./core/magi-facts.mjs');
  const hooks = readHookCatalog();
  const methods = readHostMethods();
  for (const [id, capability] of Object.entries(CAPABILITIES)) {
    if (capability.hook) {
      const spec = hooks.get(capability.hook.name);
      assert.ok(spec, `${id}：Magi 没有钩子 ${capability.hook.name}`);
      assert.ok(spec.wired.includes(capability.hook.mode), `${id}：钩子 ${capability.hook.name} 的 ${capability.hook.mode} 模式尚未接线（wired=${spec.wired}）`);
    }
    if (capability.rpc) assert.ok(methods.has(capability.rpc), `${id}：Magi 宿主没有 RPC 方法 ${capability.rpc}`);
  }
  const { rule } = await import('./core/rules.mjs');
  assert.throws(() => rule('calls', /x/, { capability: 'no.such', fidelity: 'full' }), /未登记的 Magi 能力/);
  assert.throws(() => rule('calls', /x/, { capability: 'tool.register' }), /fidelity/);
  assert.throws(() => rule('calls', /x/, {}), /capability 或 fixed/);
});

test('规则表里的每条规则都能解析，且 mapped 只来自翻译器已实现的能力', async () => {
  const { DSH_RULES, DSH_IMPLEMENTED } = await import('./dsh/catalog.mjs');
  const { resolveRule } = await import('./core/rules.mjs');
  const { CAPABILITIES } = await import('./core/capabilities.mjs');
  for (const id of DSH_IMPLEMENTED) assert.ok(CAPABILITIES[id], `已实现的能力 ${id} 不在能力目录里`);
  for (const r of DSH_RULES) {
    const resolved = resolveRule(r, DSH_IMPLEMENTED);
    if (resolved.status === 'mapped') assert.ok(DSH_IMPLEMENTED.has(r.capability), `${r.match} 标为 mapped，但翻译器尚未实现 ${r.capability}`);
    if (r.capability && r.fidelity !== 'none') assert.ok(r.source || r.note !== undefined, `${r.match} 缺少依据`);
  }
});

test('拦截类 API：tools/pre-execute 等不再被当作“已可翻译”，partial 与 dropped 如实阻塞', async () => {
  const { mkdtempSync, mkdirSync, writeFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'compat-hooks-'));
  mkdirSync(join(dir, 'lib'));
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: 'hooky', version: '1.0.0', main: 'lib/index.js', type: 'module', peerDependencies: { '@deepseek-ai/dsh-llm': '^0.2.0' } }));
  writeFileSync(join(dir, 'lib/index.js'), [
    'export function apply(ctx) {',
    "  ctx.on('tools/pre-execute', async (exec, next) => next());",
    "  ctx.on('tools/execute', async (exec, next) => next());",
    "  ctx.on('agent/turn-stopping', () => {});",
    "  ctx.on(dynamicName(), () => {});",
    '}', 'function dynamicName() { return Math.random() > 2 ? "a" : "b"; }', '',
  ].join('\n'));
  const report = await createRegistry().create('dsh').inspect(dir, { required: ['hooks'] });
  assert.equal(find(report, 'tools/pre-execute').status, 'partial');
  assert.equal(find(report, 'tools/execute').status, 'dropped');
  assert.equal(find(report, 'agent/turn-stopping').status, 'dropped');
  assert.equal(find(report, '<dynamic>').status, 'shim');
  const blocked = report.verdict.blocked.map((entry) => entry.name);
  for (const name of ['tools/pre-execute', 'tools/execute', 'agent/turn-stopping']) assert.ok(blocked.includes(name), `${name} 应阻塞`);
});

async function piFixture(files, pkg) {
  const { mkdtempSync, mkdirSync, writeFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join, dirname } = await import('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'compat-pi-'));
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: 'pi-fixture', version: '1.0.0', ...pkg }));
  for (const [name, text] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, name)), { recursive: true });
    writeFileSync(join(dir, name), text);
  }
  return dir;
}

test('pi 翻译器：入口来自 pi.extensions 或 extensions/ 约定目录；上下文靠数据流识别，处理器里的第二个参数也算', async () => {
  const dir = await piFixture({
    'extensions/a.ts': [
      'import { helper } from "./lib/helper.ts";',
      'export default function (pi) {',
      '  pi.on("tool_call", async (event, ctx) => { ctx.ui.notify("x"); helper(ctx); });',
      '  pi.registerCommand("c", { description: "d", handler: async (args, context) => { context.cwd; } });',
      '}',
    ].join('\n'),
    'extensions/lib/helper.ts': 'export function helper(ctx) { ctx.sessionManager.getEntries(); }\n',
  }, { keywords: ['pi-package'], pi: {} });
  const registry = createRegistry();
  assert.equal((await registry.detect(dir)).kind, 'pi');
  const report = await registry.create('pi').inspect(dir, { required: [] });
  assert.equal(report.scanned.files, 2, '应从约定目录入口出发并沿相对导入找到 helper.ts');
  assert.equal(find(report, 'tool_call').status, 'partial');
  assert.equal(find(report, 'ctx.ui.notify').status, 'partial');
  assert.equal(find(report, 'ctx.cwd').status, 'shim', '处理器的第二个参数被识别为上下文，名字叫 context 也算');
  assert.equal(find(report, 'ctx.sessionManager.getEntries').status, 'partial', '上下文经函数实参传播到相对导入的 helper');
});

test('pi 翻译器：dropped 的会话驱动与请求改写被如实阻塞；解析不了的入口与 glob 明确失败', async () => {
  const dir = await piFixture({
    'extensions/x.ts': [
      'export default function (pi) {',
      '  pi.sendUserMessage("go");',
      '  pi.on("before_provider_request", (e) => e.payload);',
      '  pi.on("agent_before_settle", () => ({ continue: true }));',
      '  pi.on("some_unread_event", () => {});',
      '}',
    ].join('\n'),
  }, {});
  const report = await createRegistry().create('pi').inspect(dir, { required: ['hooks', 'session'] });
  assert.equal(find(report, 'pi.sendUserMessage').status, 'dropped');
  assert.equal(find(report, 'before_provider_request').status, 'dropped');
  assert.equal(find(report, 'agent_before_settle').status, 'dropped');
  assert.equal(find(report, 'some_unread_event').status, 'unknown', '文档已读区域之外的事件不猜');
  assert.ok(report.verdict.blocked.length >= 4);

  const missing = await piFixture({}, { pi: { extensions: ['./nope.ts'] } });
  await assert.rejects(createRegistry().create('pi').inspect(missing), /不存在/);
  const noEntry = await piFixture({ 'README.md': 'x' }, {});
  await assert.rejects(createRegistry().create('pi').inspect(noEntry), /找不到 pi 扩展入口/);
  const badGlob = await piFixture({ 'a.ts': 'export default function (pi) {}' }, { pi: { extensions: ['./{a,b}.ts'] } });
  await assert.rejects(createRegistry().create('pi').inspect(badGlob), /尚不支持的 glob/);
});

test('规则表：pi 与 dsh 引用的能力都存在；partial/dropped 的规则必须写明原因，none 的规则必须有依据', async () => {
  const { PI_RULES, PI_SYMBOL_RULES } = await import('./pi/catalog.mjs');
  const { DSH_RULES } = await import('./dsh/catalog.mjs');
  const { CAPABILITIES } = await import('./core/capabilities.mjs');
  for (const [name, rules] of [['pi', [...PI_RULES, ...PI_SYMBOL_RULES]], ['dsh', DSH_RULES]]) {
    for (const r of rules) {
      if (r.capability) {
        assert.ok(CAPABILITIES[r.capability], `${name}: ${r.match} 引用了不存在的能力`);
        if (r.fidelity !== 'full') assert.ok(r.note, `${name}: ${r.match} 是 ${r.fidelity}，必须写明原因`);
        if (r.fidelity === 'none') assert.ok(r.source, `${name}: ${r.match} 标为 none，必须给出文档依据`);
      }
    }
  }
});

test('上下文传给上游辅助函数沿真实 Cordis 保留；未知宿主成员仍由规则阻断', async () => {
  const dir = await piFixture({
    'extensions/e.ts': [
      'export default function (pi) {',
      '  const holder = { pi };',
      '  holder.pi.registerTool({ name: "t" });',
      '  setup(pi);',
      '}',
      'function setup(p) { p.on("tool_call", () => {}); }',
    ].join('\n'),
  }, {});
  const report = await createRegistry().create('pi').inspect(dir, { required: [] });
  assert.equal(find(report, '<context-escape>').status, 'shim');
  assert.ok(!report.verdict.blocked.some((entry) => entry.name === '<context-escape>'));
  // 同文件函数 setup(pi) 里的订阅仍被传播识别
  assert.equal(find(report, 'tool_call').status, 'partial');
});

test('同名变量不是上下文：LLM 请求里的 context.messages 不会被当成宿主 API 调用', async () => {
  const dir = await piFixture({
    'extensions/n.ts': [
      'export default function (pi) {',
      '  pi.on("tool_call", () => {});',
      '}',
      'function unrelated(context) { return context.messages.length + context.tools.length; }',
    ].join('\n'),
  }, {});
  const report = await createRegistry().create('pi').inspect(dir, { required: [] });
  assert.equal(report.entries.find((entry) => /messages|ctx\.tools|context\./.test(entry.name)), undefined);
});

test('路由执行层：Node 风格与 Fetch 风格处理器都按上游语义运行；JSON 还原、状态码保留、抛错回 400、超限明确失败', async () => {
  const { runRoute, MAX_ROUTE_BODY_BYTES } = await import('./runtime/routes.mjs');
  const signal = new AbortController().signal;
  const call = (definition, input = {}) => runRoute(definition, { method: 'GET', path: '/p', ...input }, { signal });

  // Node 风格：读请求体、写状态码与响应头、res.end。
  const echo = await call({
    handler: async (req, res) => {
      const chunks = [];
      for await (const chunk of req) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      res.writeHead(201, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ got: body, url: req.url, method: req.method, host: req.headers.host }));
    },
  }, { method: 'POST', path: '/echo', query: 'a=1', body: { x: 1 } });
  assert.equal(echo.status, 201);
  assert.deepEqual(echo.body, { got: { x: 1 }, url: '/echo?a=1', method: 'POST', host: '127.0.0.1' });

  // 上游用 req.socket.localAddress / headers.host 做回环校验：必须是回环值。
  const loop = await call({ handler: (req, res) => { res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify([req.socket.localAddress, req.headers.origin ?? null])); } });
  assert.deepEqual(loop.body, ['127.0.0.1', null]);

  // 分块写出（NDJSON）整段返回为文本。
  const stream = await call({ handler: (_req, res) => { res.writeHead(200, { 'Content-Type': 'application/x-ndjson' }); res.write('{"a":1}\n'); res.write('{"b":2}\n'); res.end(); } });
  assert.equal(stream.contentType, 'application/x-ndjson');
  assert.equal(stream.body, '{"a":1}\n{"b":2}\n');

  // Fetch 风格：只服务 GET / HEAD。
  const fetched = await call({ resource: async (request) => new Response(JSON.stringify({ path: new URL(request.url).pathname }), { headers: { 'content-type': 'application/json' } }) });
  assert.deepEqual(fetched.body, { path: '/p' });
  const refused = await call({ resource: async () => new Response('x') }, { method: 'POST' });
  assert.equal(refused.status, 405);

  // 处理器抛错：与 DSH 一致回 400，宿主不失败。
  const thrown = await call({ handler: () => { throw new Error('坏了'); } });
  assert.equal(thrown.status, 400);
  assert.match(thrown.body.error, /坏了/);

  // 响应超限：明确失败，不截断。
  const oversize = await call({ handler: (_req, res) => { res.writeHead(200); res.write(Buffer.alloc(MAX_ROUTE_BODY_BYTES)); res.end(Buffer.alloc(1)); } });
  assert.equal(oversize.status, 502, '超限不得变成 200 空正文');
  assert.match(oversize.body.error, /超过/);
  // 恰好等于上限的响应是合法的。
  const exact = await call({ handler: (_req, res) => { res.writeHead(200, { 'content-type': 'text/plain' }); res.end(Buffer.alloc(MAX_ROUTE_BODY_BYTES, 97)); } });
  assert.equal(exact.status, 200);
  assert.equal(exact.body.length, MAX_ROUTE_BODY_BYTES);
});

test('路由执行层：调用被取消时立即以 Cancelled 结束，不等处理器', async () => {
  const { runRoute } = await import('./runtime/routes.mjs');
  const controller = new AbortController();
  const pending = runRoute({ handler: () => new Promise(() => {}) }, { method: 'GET', path: '/slow' }, { signal: controller.signal });
  setTimeout(() => controller.abort(), 20);
  await assert.rejects(pending, (error) => error.kind === 'Cancelled');
});

test('路由执行层：已取消调用不执行业务，取消关闭响应一次并拒绝迟到写入', async () => {
  const { runRoute } = await import('./runtime/routes.mjs');
  const { getEventListeners } = await import('node:events');
  let called = 0;
  const cancelled = new AbortController(); cancelled.abort();
  await assert.rejects(runRoute({ handler() { called++; } }, { method: 'GET', path: '/never' }, { signal: cancelled.signal }), { name: 'AbortError' });
  assert.equal(called, 0);
  const controller = new AbortController();
  let response, closed = 0;
  const pending = runRoute({ handler(_req, res) { response = res; res.on('close', () => closed++); } }, { method: 'GET', path: '/slow' }, { signal: controller.signal });
  await Promise.resolve(); controller.abort();
  await assert.rejects(pending, error => error.kind === 'Cancelled');
  assert.equal(response.writable, false); assert.equal(response.write('late'), false);
  response.end('late'); response.destroy();
  assert.equal(closed, 1);
  const complete = new AbortController();
  await runRoute({ handler(_req, res) { res.end('ok'); } }, { method: 'GET', path: '/done' }, { signal: complete.signal });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(getEventListeners(complete.signal, 'abort').length, 0, '已完成请求不保留监听');
});

test('界面槽位：ctx 的实现只含上游入口用到的成员；未知服务缺席，未知槽位按策略分类', async () => {
  const { createSlotContext, slotDisposition } = await import('./runtime/client/slot-context.mjs');
  const scope = { getSnapshot: () => ({ value: {} }), subscribe: () => () => {}, set: async () => true };
  const slots = createSlotContext({ scope });
  assert.equal(slots.ctx.get('layout'), undefined, 'DSH 外壳服务缺席');
  assert.equal(slots.ctx.get('configForms').get('x'), scope);
  assert.throws(() => slots.ctx.uiRenderer.mount(), TypeError, '未提供的成员是 TypeError，不静默吞掉');
  const dispose = slots.ctx.slots.inject('plugins.bundle.config', () => slots.ctx.slots.register({ name: 'plugins.bundle.config' }, () => null));
  assert.equal(slots.live().length, 1);
  dispose();
  assert.equal(slots.live().length, 0);
  assert.equal(slotDisposition('plugins.bundle.config'), 'mounted');
  assert.equal(slotDisposition('plugins.row.config'), 'alternate');
  assert.equal(slotDisposition('settings.section'), 'mounted');
  assert.equal(slotDisposition('settings.section.icon'), 'alternate');
  assert.equal(slotDisposition('conversation.input.right'), 'unmounted');
  slots.ctx.locale.register('ns', { zh: { hi: '你好 {name}' }, en: { hi: 'Hi {name}' } });
  const t = slots.ctx.locale.bind('ns');
  assert.equal(t('hi', { name: 'A' }), '你好 A');
  slots.setLocale('en-US');
  assert.equal(t('hi', { name: 'A' }), 'Hi A');
  assert.equal(t('missing'), 'missing');
});

test('原生页面保留异步和条件注册的多个右栏页签，页面身份与显示名称不混淆', async (t) => {
  const { mkdtempSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { analyzeNativeClient } = await import('./install/client-assets.mjs');
  const { clientViews } = await import('./install/client-views.mjs');
  const directory = mkdtempSync(join(tmpdir(), 'magi-native-panels-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  writeFileSync(join(directory, 'client.js'), `window.__ModuleLoader__.load({ id: 'panels', factory(require) { return { async apply(ctx) {
    await Promise.resolve();
    const P=require('@deepseek-ai/dsh-client-ui-primitives');
    const form=new P.SettingsFormModel(ctx.configForms.get('preferences'),[P.settingsTextField('title')]);
    if (!form.shell().available) throw new Error('真实表单模型未初始化');
    ctx.inject(['sidebarRight','sidebarRightTabs'], c => {
      for (const id of ['one/a', 'one-a']) {
        c.effect(() => c.sidebarRightTabs.register({ id, kind: id, title: () => 'Panel ' + id }));
        ctx.slots.register({ name:'sidebar.right.pane.tab', key:id }, () => null);
      }
    });
    ctx.slots.register({name:'settings.section',id:'preferences',label:()=> 'Preferences'}, () => null);
    ctx.slots.register({name:'settings.section',id:'account',label:()=> 'Account'}, () => null);
  } }; } });`);
  const analysis = analyzeNativeClient({ upstreamDir: directory, pkg: { exports: { './client': './client.js' } }, sharedRoot: fileURLToPath(new URL('.', import.meta.url)) });
  const views = clientViews(analysis.mounted, 'fallback');
  assert.equal(views.length, 4);
  assert.equal(views.filter((view) => view.placements.includes('rightPane')).length, 2);
  assert.equal(views.filter((view) => view.placements.includes('settings')).length, 2);
  assert.equal(new Set(views.map((view) => view.id)).size, 4, '同一槽位及同形 slug 不会覆盖页面');
  assert.equal(views[0].title, 'Panel one/a');
  assert.equal(views[2].id, 'settings');
  assert.throws(() => clientViews([analysis.mounted[0], analysis.mounted[0]], 'x'), /身份重复/);
  writeFileSync(join(directory, 'client.js'), `window.__ModuleLoader__.load({id:'failed',factory(require){return {async apply(){await Promise.resolve();throw new Error('async failed')}}}})`);
  assert.throws(() => analyzeNativeClient({ upstreamDir: directory, pkg: { exports: { './client': './client.js' } }, sharedRoot: fileURLToPath(new URL('.', import.meta.url)) }), /async failed/);
});

test('右栏导航只打开已注册页签，停用清理注册；语言改变通知上游订阅', async () => {
  const { createSlotContext } = await import('./runtime/client/slot-context.mjs');
  const opened = [], locales = [];
  const context = createSlotContext({ scope: {}, onOpenTab: (id) => opened.push(id) });
  const off = context.ctx.locale.subscribe(() => locales.push(context.ctx.locale.getSnapshot()));
  context.setLocale('en'); context.setLocale('en'); off(); context.setLocale('zh');
  assert.deepEqual(locales, ['en']);
  context.ctx.inject(['sidebarRight', 'sidebarRightTabs'], c => {
    c.effect(() => c.sidebarRightTabs.register({ id: 'inspector', kind: 'inspect' }));
    c.sidebarRight.openTab('inspect');
  });
  assert.deepEqual(opened, ['inspector']);
  assert.throws(() => context.ctx.sidebarRight.openTab('unknown'), /未注册/);
  context.dispose();
  assert.throws(() => context.ctx.sidebarRight.openTab('inspect'), /未注册/);
});

test('隔离页面 Web Storage 外观保留字符串、空值、特殊键、删除与配额语义', async () => {
  const { createWebStorage } = await import('./runtime/client/web-storage.mjs');
  const mutations = [];
  const storage = createWebStorage({ old: 'value' }, (mutation) => mutations.push(mutation));
  storage.setItem('__proto__', 'ordinary-key'); storage.setItem('zero', 0); storage.setItem('empty', '');
  assert.equal(storage.getItem('zero'), '0'); assert.equal(storage.getItem('empty'), '');
  assert.equal(storage.getItem('__proto__'), 'ordinary-key'); assert.equal(storage.getItem('missing'), null);
  assert.equal(storage.length, 4); assert.equal(storage.key(0), 'old'); assert.equal(storage.key(10), null);
  storage.removeItem('old'); assert.equal(storage.length, 3);
  const limited = createWebStorage({}, () => {}, 20);
  assert.throws(() => limited.setItem('x', 'v'.repeat(30)), { name: 'QuotaExceededError' });
  assert.equal(limited.length, 0);
  storage.clear(); assert.equal(storage.length, 0); assert.equal(mutations.at(-1).kind, 'clear');
});

test('原生页面自定义鉴权头到达 Node/Fetch 处理器，宿主身份和传输头不可伪造', async () => {
  const { runRoute, routeHeaders } = await import('./runtime/routes.mjs');
  const signal = new AbortController().signal;
  const input = { method: 'GET', path: '/authenticated', headers: { 'X-Panel-Key': 'token', Accept: 'application/json' } };
  const node = await runRoute({ handler(req, res) { res.setHeader('content-type', 'application/json'); res.end(JSON.stringify({ key: req.headers['x-panel-key'], host: req.headers.host })); } }, input, { signal });
  assert.deepEqual(node.body, { key: 'token', host: '127.0.0.1' });
  const fetchStyle = await runRoute({ resource(request) { return new Response(request.headers.get('x-panel-key')); } }, input, { signal });
  assert.equal(fetchStyle.body, 'token');
  for (const name of ['Host', 'Origin', 'Cookie', 'Content-Length', 'X-Forwarded-For', 'Sec-Fetch-Site']) assert.throws(() => routeHeaders({ [name]: 'spoof' }), /不能覆盖/);
  for (const headers of [{ 'bad key': 'x' }, { valid: 'x\r\ny' }, { valid: 1 }, { valid: 'x'.repeat(8193) }]) assert.throws(() => routeHeaders(headers), /无效/);
});

test('翻译保留 PNG/SVG 原始图片字节；越界、伪造和超限拒绝，内容准入由 Magi 包统一校验', async (t) => {
  const { mkdtempSync, writeFileSync, mkdirSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { resolveUpstreamIcon } = await import('./install/icon.mjs');
  const sharp = (await import('sharp')).default;
  const png = await sharp({ create: { width: 8, height: 8, channels: 4, background: '#2478ac' } }).png().toBuffer();
  const make = (declared, file, bytes) => {
    const dir = mkdtempSync(join(tmpdir(), 'magi-icon-'));
    t.after(() => rmSync(dir, { recursive: true, force: true }));
    mkdirSync(join(dir, 'up/icons'), { recursive: true });
    if (file && bytes) writeFileSync(join(dir, 'up', file), bytes);
    return { upstreamDir: join(dir, 'up'), pkg: { name: 'x', ...(declared === undefined ? {} : { icon: declared }) } };
  };
  const icon = await resolveUpstreamIcon(make('icons/a.png', 'icons/a.png', png));
  assert.equal(icon.to, 'ui/icon.png');
  assert.equal((await sharp(icon.bytes).metadata()).width, 8);
  assert.equal(await resolveUpstreamIcon(make(undefined)), null);
  const svg = '<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="red"/></svg>';
  const vector = await resolveUpstreamIcon(make('icons/a.svg', 'icons/a.svg', Buffer.from(svg)));
  assert.equal(vector.to, 'ui/icon.svg');
  assert.equal(vector.bytes.toString(), svg);
  await assert.rejects(resolveUpstreamIcon(make('icons/a.png', 'icons/a.png', Buffer.from('<script>'))), /内容与其扩展名不符/);
  await assert.rejects(resolveUpstreamIcon(make('icons/a.png', 'icons/a.png', Buffer.alloc(300 * 1024, 1))), /超过 256 KiB/);
  await assert.rejects(resolveUpstreamIcon(make('../../etc/hosts')), /指向包外/);
  await assert.rejects(resolveUpstreamIcon(make('icons/missing.png')), /文件不存在/);
});
