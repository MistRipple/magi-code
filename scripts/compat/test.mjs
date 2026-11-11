import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { renderText } from './core/report.mjs';
import { createDshTranslator } from './dsh/translator.mjs';

const fixture = fileURLToPath(new URL('./fixtures/mini-dsh', import.meta.url));
const cli = fileURLToPath(new URL('./cli.mjs', import.meta.url));
const find = (report, name) => report.entries.find((entry) => entry.name === name);

test('真实管道完整输出超过 64 KiB 的准入报告，保留末尾阻塞证据和退出码', { timeout: 10000 }, async t => {
  const { mkdtempSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { spawn } = await import('node:child_process');
  const { once } = await import('node:events');
  const root = mkdtempSync(join(tmpdir(), 'magi-large-inspection-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(join(root, 'package.json'), JSON.stringify({ name: 'large-inspection', version: '1.0.0', main: 'index.js', dsh: { bundle: {} } }));
  writeFileSync(join(root, 'index.js'), `export function apply(ctx) {${Array.from({length: 500}, (_, index) => `ctx.unknown${index}();`).join('')}ctx.lastUnsupported();}`);
  const child = spawn(process.execPath, [cli, 'inspect', root, '--json'], { stdio: ['ignore', 'pipe', 'pipe'] });
  t.after(() => child.kill());
  const bytes = []; let diagnostic = '';
  child.stdout.on('data', value => bytes.push(value)); child.stderr.on('data', value => diagnostic += value);
  const [code] = await once(child, 'close');
  const output = Buffer.concat(bytes); assert.ok(output.length > 64 * 1024);
  assert.equal(code, 1, diagnostic);
  const report = JSON.parse(output.toString());
  assert.equal(report.verdict.blocked.length, 501);
  assert.ok(report.verdict.blocked.some(entry => entry.name === 'ctx.lastUnsupported'));
});

test('daemon 的独占管道关闭后正在联网的翻译工具链退出，不保留孤儿重试', {timeout:10000}, async t => {
  const {spawn}=await import('node:child_process');const {createServer}=await import('node:http');const {once}=await import('node:events');
  let began;const started=new Promise(resolve=>{began=resolve;});
  const server=createServer((request,response)=>{began();});
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  t.after(()=>{server.closeAllConnections();server.close();});
  const child=spawn(process.execPath,[cli,'resolve','dsh','magi-ownership-probe','1.0.0'],{
    env:{...process.env,MAGI_TRANSLATOR_OWNER_PIPE:'1',MAGI_NPM_REGISTRY:`http://127.0.0.1:${server.address().port}`},stdio:['pipe','pipe','pipe'],
  });
  t.after(()=>child.kill());child.stdout.resume();child.stderr.resume();
  const exited=once(child,'exit');await Promise.race([started,exited.then(()=>assert.fail('工具链在联网前意外退出'))]);
  child.stdin.end();assert.equal((await exited)[0],1);
});

test('运行组件身份可复现，同时覆盖业务转换源码与实际页面产物', async t => {
  const { mkdtempSync, mkdirSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { writeTranslationRuntime, translationRuntimeId } = await import('./core/translation-runtime.mjs');
  const root = mkdtempSync(join(tmpdir(), 'magi-translation-identity-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const name of ['cli.mjs', 'package.json', 'package-lock.json']) writeFileSync(join(root, name), name);
  for (const directory of ['core', 'dsh', 'runtime', 'install', 'dist']) mkdirSync(join(root, directory));
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

async function inspectOptional(t, code, required = ['session', 'hooks', 'tools']) {
  const { mkdtempSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'magi-optional-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: 'optional-dsh', version: '1.0.0', main: 'index.js' }));
  writeFileSync(join(dir, 'index.js'), code);
  return createDshTranslator().inspect(dir, { required });
}

test('读取已支持路由服务不误判为服务器操作，未接入的成员仍拒绝', async t => {
  const report=await inspectOptional(t,"export function apply(ctx) {const server=ctx.webServer;server.register({path:'/example',handler(req,res){res.end('original')}});}",['ui']);
  assert.equal(report.verdict.blocked.length,0);
  assert.equal(find(report,'ctx.webServer').status,'mapped');
  for(const member of ['host','port','registerFallback','unknown']) {
    const blocked=await inspectOptional(t,`export function apply(ctx) {return ctx.webServer.${member};}`,['ui']);
    assert.ok(blocked.verdict.blocked.length>0,member);
  }
});

test('profile 启停文件写入有独立准入证据，普通文件写入不申请插件管理权限', async t => {
  const report = await inspectOptional(t, "import {writeFileSync as save} from 'node:fs'; import {join} from 'node:path'; export function apply(ctx) {} export function disable(dir){save(join(dir,'cordis.patch.yml'),'[]')}");
  assert.equal(report.profileMutations.length, 1);
  const ordinary = await inspectOptional(t, "import fs from 'node:fs'; export function apply(ctx) {fs.writeFileSync('capture.png','bytes')}");
  assert.deepEqual(ordinary.profileMutations, []);
  const read = await inspectOptional(t, "import {readFileSync} from 'node:fs'; export function apply(ctx) {readFileSync('cordis.patch.yml')}");
  assert.deepEqual(read.profileMutations, []);
});

test('延迟执行的标准包管理子进程保留安装授权证据，普通进程和同名函数不混入', async t => {
  const report = await inspectOptional(t, "import {spawn as launch} from 'node:child_process'; export function apply(ctx) {} export function install(dir){const manager='npm';return launch(manager,['install','target@1.0.0'],{cwd:dir})}");
  assert.deepEqual(report.profilePackageProcesses, ['index.js']);
  const nodeManager = await inspectOptional(t, "import cp from 'node:child_process'; export function apply(ctx) {} export function install(dir){return cp.spawn(process.execPath,['/tools/npm/bin/npm-cli.js','install','target@1.0.0'],{cwd:dir})}");
  assert.deepEqual(nodeManager.profilePackageProcesses, ['index.js']);
  const ordinary = await inspectOptional(t, "import cp from 'node:child_process'; export function apply(ctx) {cp.spawn('capture',['screen.png'])}");
  assert.deepEqual(ordinary.profilePackageProcesses, []);
  const unrelated = await inspectOptional(t, "function spawn(name) {return name}; export function apply(ctx) {spawn('npm')}");
  assert.deepEqual(unrelated.profilePackageProcesses, []);
});

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

test('第三方 Desktop 服务保持缺席，跨环境原包可走自身路径，必需使用仍拒绝', async t => {
  const conditional = "export function apply(ctx) { if (ctx.get('desktopProfiles') === undefined) return; ctx.inject(['desktopPnpm'], c => c.desktopPnpm.runPlugin(['add', 'example'], '/profile')); }";
  const optional = await inspectOptional(t, conditional, ['runtime']);
  assert.equal(optional.verdict.blocked.length, 0);
  assert.equal(find(optional, 'desktopPnpm').status, 'dropped');
  assert.equal(find(optional, 'desktopPnpm').optional, true);
  for (const code of [
    "export const inject = ['desktopPnpm'];\n" + conditional,
    "export function apply(ctx) { ctx.desktopPnpm.runPlugin(['add', 'example'], '/profile'); }",
    "export const inject = ['desktopProfiles']; export function apply(ctx) { console.log(ctx.desktopProfiles.current); }",
  ]) assert.ok((await inspectOptional(t, code, ['runtime'])).verdict.blocked.length > 0, code);
});

test('缺席服务的条件动态模块不阻塞其余业务，其他入口及未知依赖仍完整准入', async t => {
  const { mkdtempSync, writeFileSync, rmSync } = await import('node:fs');
  const { join } = await import('node:path');
  const { tmpdir } = await import('node:os');
  const dir = mkdtempSync(join(tmpdir(), 'magi-conditional-module-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: 'conditional-module', version: '1.0.0', main: 'index.js' }));
  writeFileSync(join(dir, 'account.js'), "import '@deepseek-ai/dsh-credentials'; export function apply(ctx) { ctx.on('agent/error', () => {}); }");
  const inspect = async code => {
    writeFileSync(join(dir, 'index.js'), code);
    return createDshTranslator().inspect(dir);
  };
  const conditional = "ctx.inject(['credentials', 'llm'], scoped => { void import('./account.js').then(pack => pack.apply(scoped)); });";
  const report = await inspect(`export const inject=['llm']; export function apply(ctx) { ${conditional} ctx.llm.registerAdapter([], {}); }`);
  assert.equal(report.verdict.blocked.length, 0);
  assert.deepEqual(report.scanned.fileList, ['index.js']);
  assert.deepEqual(report.deferredImports, [{ file: 'index.js', module: './account.js', unavailableServices: ['credentials'] }]);
  assert.ok(report.entries.some(entry => entry.kind === 'modules' && entry.optional && entry.status === 'dropped'));
  for (const code of [
    `import './account.js'; export function apply(ctx) { ${conditional} }`,
    `export function apply(ctx) { ${conditional} void import('./account.js'); }`,
    "export function apply(ctx) { ctx.inject(['unknownService'], scoped => { void import('./account.js'); }); }",
    "export function apply(ctx) { ctx.inject(['llm'], scoped => { void import('./account.js'); }); }",
    `export const inject=['credentials']; export function apply(ctx) { ${conditional} }`,
    `export function apply(ctx) { ctx.provide('credentials', {}); ${conditional} }`,
    `class OwnCredentials extends Service { constructor(ctx) {super(ctx, 'credentials');} } export function apply(ctx) {ctx.plugin(OwnCredentials); ${conditional}}`,
    "export function apply(ctx) { ctx.inject(['credentials'], scoped => { exported = () => import('./account.js'); }); }",
    "export function apply(ctx) { const other={inject(){}}; other.inject(['credentials'], scoped => { void import('./account.js'); }); }",
  ]) assert.ok((await inspect(code)).verdict.blocked.length > 0, code);
});

test('真实 Cordis registry 和 llm 通知可以读取，未登记 llm 成员仍拒绝', async t => {
  const report = await inspectOptional(t, "export function apply(ctx) { const registry=ctx.registry; const llm=ctx.llm; ctx.llm.emitAdaptersUpdated(); }", ['runtime', 'llm']);
  assert.equal(report.verdict.blocked.length, 0);
  assert.ok((await inspectOptional(t, "export function apply(ctx) { ctx.llm.unknown(); }", ['llm'])).verdict.blocked.length > 0);
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

test('上下文常量别名跨文件保留必需事件证据，同名局部参数和变量不会冒充上下文', async t => {
  const report = await inspectOptional(t, `export function apply(ctx) { const host=ctx; register(host); }
    function register(host) { const local=host; local.on('agent/created',()=>{});
      (()=>{const host={directory:'ordinary'};console.log(host.directory);})();
      ((local)=>console.log(local.version))({version:'ordinary'});
      (({host})=>console.log(host.version))({host:{version:'ordinary'}});
      try {throw {directory:'ordinary'};}catch(local){console.log(local.directory);}
      for(const loop=local;false;)loop.on('session/event',()=>{});
      (()=>{const loop={version:'ordinary'};console.log(loop.version);})();
    }`);
  assert.ok(report.verdict.blocked.some(item => item.name === 'agent/created'));
  assert.equal(find(report, 'ctx.directory'), undefined); assert.equal(find(report, 'ctx.version'), undefined);
});

test('只有明确返回 null 的可选事件调用可协商不可用，必需、未知和混用仍拒绝', async t => {
  const optional = "export function apply(ctx) { const host=ctx; const subscribe=()=>host.on?.('agent/created',()=>{}) ?? null; console.log(subscribe()); }";
  const report = await inspectOptional(t, optional);
  assert.equal(report.verdict.blocked.length, 0); assert.equal(find(report, 'agent/created').nullable, true);
  for (const code of [
    "export function apply(ctx) { ctx.on?.('agent/created',()=>{}); }",
    "export function apply(ctx) { const off=ctx.on('agent/created',()=>{}) ?? null; }",
    "export function apply(ctx) { const off=ctx.on?.('agent/created',()=>{}) ?? (()=>{}); }",
    "export function apply(ctx) { ctx.on?.('agent/created',()=>{}) ?? null; ctx.on('agent/created',()=>{}); }",
    "export function apply(ctx) { ctx.on?.('unknown/event',()=>{}) ?? null; }",
    "export function apply(ctx) { ctx.on?.('agent/created',()=>{}) ?? null; try {ctx.on('agent/created',()=>{});}catch{} }",
  ]) assert.ok((await inspectOptional(t, code)).verdict.blocked.length > 0, code);
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

test('DSH 产品入口识别原包，拒绝其他生态与无关目录', async t => {
  const translator = createDshTranslator();
  assert.ok(await translator.detect(fixture));
  const { mkdtempSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const unrelated = mkdtempSync(join(tmpdir(), 'magi-detect-'));
  t.after(() => rmSync(unrelated, { recursive: true, force: true }));
  writeFileSync(join(unrelated, 'package.json'), JSON.stringify({ name: 'unrelated', version: '1.0.0' }));
  assert.equal(await translator.detect(unrelated), 0);
  const rejected = spawnSync(process.execPath, [cli, 'inspect', fixture, '--kind', 'pi'], { encoding: 'utf8' });
  assert.equal(rejected.status, 2);
  assert.match(rejected.stderr, /pi 没有翻译器/);
  // resolve / prepare 同样按生态分发：没有登记的生态不进入 DSH 的解析与策略。
  const resolved = spawnSync(process.execPath, [cli, 'resolve', 'pi', 'some-package', '1.0.0'], { encoding: 'utf8' });
  assert.equal(resolved.status, 0);
  assert.equal(JSON.parse(resolved.stdout).error.code, 'ecosystem-unsupported');
  const prepared = spawnSync(process.execPath, [cli, 'prepare', 'pi', fixture, '--out', join(unrelated, 'out'), '--integrity', 'sha512-x'], { encoding: 'utf8' });
  assert.equal(prepared.status, 2);
  assert.match(prepared.stderr, /pi 没有翻译器/);
});

test('静态分析：每个能力都有明确状态，未登记的上游服务为 unknown 而非静默通过', async () => {
  const report = await createDshTranslator().inspect(fixture, { required: ['llm', 'tools'] });
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
  const translator = createDshTranslator();
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

test('默认静态检查与在线安装共用完整业务准入，分项诊断不宣称插件可用', async t => {
  const { mkdtempSync, writeFileSync, existsSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join, dirname } = await import('node:path');
  const { prepareDsh } = await import('./install/prepare.mjs');
  const root = mkdtempSync(join(tmpdir(), 'magi-admission-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(join(root, 'package.json'), JSON.stringify({ name: 'admission-test', version: '1.0.0', main: 'index.js', dsh: { bundle: {} } }));
  writeFileSync(join(root, 'index.js'), 'export function apply(ctx) { ctx.systemPrompt.context({ name: "business", order: 1, text: "Required runtime context" }); }');
  const run = (...flags) => spawnSync(process.execPath, [cli, 'inspect', root, ...flags], { encoding: 'utf8' });
  const defaultCheck = run('--json');
  assert.equal(defaultCheck.status, 1, defaultCheck.stderr);
  assert.ok(JSON.parse(defaultCheck.stdout).verdict.blocked.some(entry => entry.name === 'ctx.systemPrompt.context'));
  await assert.rejects(prepareDsh({ upstreamDir: root, outDir: join(root, 'prepared'), sharedRoot: dirname(cli), source: { integrity: 'sha512-test' } }), /ctx\.systemPrompt\.context/);
  assert.equal(existsSync(join(root, 'prepared')), false, '静态阻塞在依赖获取与生成安装产物之前拒绝');
  const diagnostic = run('--require', 'llm');
  assert.equal(diagnostic.status, 0, diagnostic.stderr);
  assert.match(diagnostic.stdout, /静态检查范围：llm/);
  assert.match(diagnostic.stdout, /安装与实际业务仍待验证/);
  assert.doesNotMatch(diagnostic.stdout, /必需能力均可翻译/);
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
  const report = await createDshTranslator().inspect(dir, { required: [] });
  assert.equal(report.scanned.files, 2);
  assert.equal(find(report, 'ctx.webServer.register').status, 'mapped');

  const empty = mkdtempSync(join(tmpdir(), 'compat-empty-'));
  writeFileSync(join(empty, 'package.json'), JSON.stringify({ name: 'empty-dsh', version: '1.0.0', main: 'lib/index.js' }));
  await assert.rejects(createDshTranslator().inspect(empty, { required: [] }), /没有扫描到任何源文件/);
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
  const report = await createDshTranslator().inspect(dir, { required: ['hooks'] });
  assert.equal(find(report, 'tools/pre-execute').status, 'partial');
  assert.equal(find(report, 'tools/execute').status, 'dropped');
  assert.equal(find(report, 'agent/turn-stopping').status, 'dropped');
  assert.equal(find(report, '<dynamic>').status, 'shim');
  const blocked = report.verdict.blocked.map((entry) => entry.name);
  for (const name of ['tools/pre-execute', 'tools/execute', 'agent/turn-stopping']) assert.ok(blocked.includes(name), `${name} 应阻塞`);
});

async function dshFixture(code) {
  const { mkdtempSync, writeFileSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const dir = mkdtempSync(join(tmpdir(), 'compat-dsh-'));
  writeFileSync(join(dir, 'package.json'), JSON.stringify({ name: 'dsh-fixture', version: '1.0.0', main: 'index.js', dsh: {} }));
  writeFileSync(join(dir, 'index.js'), code);
  return dir;
}

test('DSH 规则引用的能力存在，partial/dropped 写明原因，none 给出依据', async () => {
  const { DSH_RULES } = await import('./dsh/catalog.mjs');
  const { CAPABILITIES } = await import('./core/capabilities.mjs');
  for (const r of DSH_RULES) {
    if (!r.capability) continue;
    assert.ok(CAPABILITIES[r.capability], `${r.match} 引用了不存在的能力`);
    if (r.fidelity !== 'full') assert.ok(r.note, `${r.match} 是 ${r.fidelity}，必须写明原因`);
    if (r.fidelity === 'none') assert.ok(r.source, `${r.match} 标为 none，必须给出文档依据`);
  }
});

test('上下文传给上游辅助函数沿真实 Cordis 保留；未知宿主成员仍由规则阻断', async () => {
  const dir = await dshFixture(`
    export function apply(ctx) {
      const holder = { ctx };
      unknown(holder);
      setup(ctx);
    }
    function setup(context) { context.on('tools/pre-execute', () => {}); }
  `);
  const report = await createDshTranslator().inspect(dir, { required: [] });
  assert.equal(find(report, '<context-escape>').status, 'shim');
  assert.ok(!report.verdict.blocked.some((entry) => entry.name === '<context-escape>'));
  assert.equal(find(report, 'tools/pre-execute').status, 'partial');
});

test('同名变量不是上下文：LLM 请求里的 context.messages 不会被当成宿主 API 调用', async () => {
  const dir = await dshFixture(`
    export function apply(ctx) { ctx.on('tools/pre-execute', () => {}); }
    function unrelated(context) { return context.messages.length + context.tools.length; }
  `);
  const report = await createDshTranslator().inspect(dir, { required: [] });
  assert.equal(report.entries.find((entry) => /messages|ctx\.tools|context\./.test(entry.name)), undefined);
});

test('路由执行层：Node 风格与 Fetch 风格处理器都按上游语义运行；JSON 还原、状态码保留、抛错回 400、超限明确失败', async () => {
  const { runRoute } = await import('./runtime/routes.mjs');
  const { MAX_ROUTE_RESPONSE_BYTES } = await import('./runtime/route-payload.mjs');
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
  const oversize = await call({ handler: (_req, res) => { res.writeHead(200); res.write(Buffer.alloc(MAX_ROUTE_RESPONSE_BYTES)); res.end(Buffer.alloc(1)); } });
  assert.equal(oversize.status, 502, '超限不得变成 200 空正文');
  assert.match(oversize.body.error, /超过/);
  // 恰好等于上限的响应是合法的。
  const exact = await call({ handler: (_req, res) => { res.writeHead(200, { 'content-type': 'text/plain' }); res.end(Buffer.alloc(MAX_ROUTE_RESPONSE_BYTES, 97)); } });
  assert.equal(exact.status, 200);
  assert.equal(exact.body.length, MAX_ROUTE_RESPONSE_BYTES);
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

test('原页面主题快照来自原生外观，通知保持稳定引用并随文档释放', async () => {
  const { createThemeClient } = await import('./runtime/client/theme.mjs');
  const { createSlotContext } = await import('./runtime/client/slot-context.mjs');
  const pack = (id, schemePolicy) => ({ id, schemePolicy, schemes: { light: { accent: 'blue', background: 'white', foreground: 'black' }, dark: { accent: 'cyan', background: 'black', foreground: 'white' } } });
  const themes = [pack('builtin.light', 'light'), pack('builtin.dark', 'dark'), pack('builtin.system', 'adaptive'), pack('light', 'adaptive')];
  const client = createThemeClient(() => Promise.resolve(), assert.fail);
  const slots = createSlotContext({ scope: {}, theme: client });
  assert.throws(() => slots.ctx.theme.getTheme(), /尚未就绪/);
  const observed = [];
  slots.ctx.on('theme/change', next => observed.push(next));
  assert.throws(() => slots.ctx.on('unknown/event', () => {}), /尚未接入/);
  let injected;
  slots.ctx.inject(['theme'], ctx => { injected = ctx.theme; });
  assert.equal(injected, slots.ctx.theme);
  const theme = { mode: 'dark', tokens: { '--foreground': 'white' }, appearance: { revision: 7, preference: 'builtin.system', activeTheme: themes[2], themes } };
  client.update(theme);
  const first = slots.ctx.theme.getTheme();
  assert.equal(first.preference, 'system');
  assert.equal(first.active.colorScheme, 'dark');
  assert.equal(first.active.tokens['--dsw-alias-label-primary'], 'white');
  assert.deepEqual(first.themes.map(theme => theme.id), ['light', 'dark', 'magi:light']);
  assert.throws(() => { first.active.tokens['--dsw-alias-label-primary'] = 'red'; }, TypeError);
  client.update(structuredClone(theme));
  assert.equal(slots.ctx.theme.getTheme(), first);
  assert.equal(observed.length, 1);
  const preview = { ...theme, appearance: { ...theme.appearance, activeTheme: pack('unsaved-preview', 'light') } };
  client.update(preview);
  assert.equal(slots.ctx.theme.getTheme().active.id, 'magi:unsaved-preview');
  assert.equal(slots.ctx.theme.getTheme().preference, 'system', '临时预览不改写持久偏好');
  slots.dispose(); client.update(theme);
  assert.equal(observed.length, 2, '关闭文档回收原页面订阅');
  client.dispose();
  assert.throws(() => client.service.setTheme('dark'), /已失效/);
});

test('原页面切换主题使用原生 revision，失败不发布偏好并保留真实恢复错误', async () => {
  const { createThemeClient } = await import('./runtime/client/theme.mjs');
  const requests = [], errors = [];
  let reject, resolve, successes = 0;
  const client = createThemeClient(message => { requests.push(message); return new Promise((done, fail) => { resolve = done; reject = fail; }); }, error => errors.push(error), () => successes++);
  const themes = ['light', 'dark'].map(mode => ({ id: `builtin.${mode}`, schemePolicy: mode, schemes: { [mode]: { accent: 'blue', background: 'black', foreground: 'white' } } }));
  client.update({ mode: 'light', tokens: {}, appearance: { revision: 13, preference: 'builtin.light', activeTheme: themes[0], themes } });
  const before = client.service.getTheme();
  assert.throws(() => client.service.setTheme('constructor'), /不存在/);
  assert.equal(requests.length, 0);
  client.service.setTheme('dark');
  assert.deepEqual(requests, [{ operation: 'host.theme.set', themeId: 'builtin.dark', expectedRevision: 13 }]);
  assert.equal(client.service.getTheme(), before);
  const failure = new Error('appearance revision conflict'); reject(failure);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(errors, [failure]);
  assert.equal(client.service.getTheme(), before);
  client.service.setTheme('dark'); resolve();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(successes, 1, '实际提交成功后清理先前错误');
  assert.equal(client.service.getTheme(), before, '提交回执不冒充新主题快照');
  client.service.setTheme('dark'); client.dispose(); reject(new Error('page closed'));
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(errors, [failure], '关闭文档后的异步失败不再渲染');
});

test('界面槽位：ctx 的实现只含上游入口用到的成员；未知服务缺席，未知槽位按策略分类', async () => {
  const { createSlotContext, slotDisposition } = await import('./runtime/client/slot-context.mjs');
  const scope = { getSnapshot: () => ({ value: {} }), subscribe: () => () => {}, set: async () => true };
  const slots = createSlotContext({ scope });
  assert.equal(slots.ctx.get('layout'), undefined, 'DSH 外壳服务缺席');
  assert.equal(slots.ctx.get('configForms').get('x'), scope);
  assert.throws(() => slots.ctx.uiRenderer.mount(), TypeError, '未提供的成员是 TypeError，不静默吞掉');
  const dispose = slots.ctx.slots.inject('plugins.bundle.config', () => slots.ctx.slots.register({ name: 'plugins.bundle.config', key: 'contract' }, () => null));
  assert.equal(slots.live().length, 1);
  dispose();
  assert.equal(slots.live().length, 0);
  assert.equal(slotDisposition('plugins.bundle.config'), 'mounted');
  assert.equal(slotDisposition('plugins.row.config'), 'alternate');
  assert.equal(slotDisposition('settings.section'), 'mounted');
  assert.equal(slotDisposition('settings.section.icon'), 'alternate');
  assert.equal(slotDisposition('conversation.input.right'), 'mounted');
  slots.ctx.locale.register('ns', { zh: { hi: '你好 {name}' }, en: { hi: 'Hi {name}' } });
  const t = slots.ctx.locale.bind('ns');
  assert.equal(t('hi', { name: 'A' }), '你好 A');
  slots.setLocale('en-US');
  assert.equal(t('hi', { name: 'A' }), 'Hi A');
  assert.equal(t('missing'), 'missing');
});

test('槽位使用上游条目身份与优先级，释放后恢复同一单元的原注册', async () => {
  const { createSlotContext } = await import('./runtime/client/slot-context.mjs');
  const context = createSlotContext({ scope: {} });
  const slots = context.ctx.slots;
  const original = () => null, replacement = () => null;
  const releaseOriginal = slots.register({ name: 'plugins.bundle.config', key: 'business' }, original);
  const entry = slots.entries('plugins.bundle.config')[0];
  const changes = [];
  const off = slots.subscribe('plugins.bundle.config', () => changes.push(slots.entriesOfSlot('plugins.bundle.config')[0]));
  await Promise.resolve(); changes.length = 0;
  assert.throws(() => slots.register({ name: 'plugins.bundle.config', key: 'business' }, replacement), /already has an entry/);
  const releaseReplacement = slots.register({ name: 'plugins.bundle.config', key: 'business', priority: -1 }, replacement);
  const shadow = slots.entries('plugins.bundle.config').find(item => item.component === replacement);
  assert.equal(slots.isLive(entry), true);
  assert.equal(context.live()[0].component, replacement);
  await Promise.resolve(); assert.deepEqual(changes, [shadow]);
  releaseReplacement(); releaseReplacement();
  await Promise.resolve(); assert.deepEqual(changes, [shadow, entry]);
  assert.equal(slots.isLive(shadow), false);
  assert.equal(slots.entries('plugins.bundle.config')[0], entry);
  assert.equal(context.live()[0].component, original);
  assert.throws(() => slots.register({ name: 'settings.section.icon', key: 'invalid' }, original), /requires options.id/);
  off(); releaseOriginal(); context.dispose();
  assert.deepEqual(slots.entries('plugins.bundle.config'), []);
  assert.throws(() => slots.register({ name: 'plugins.bundle.config', key: 'business' }, original), /已失效/);
});

test('子槽位依声明生命周期注入，旧清理不删除新注册，失败初始化完整回收', async () => {
  const { createSlotContext } = await import('./runtime/client/slot-context.mjs');
  const context = createSlotContext({ scope: {} });
  const { slots } = context.ctx;
  let setups = 0, cleanups = 0;
  const off = slots.inject('feature.body', function* () {
    setups++;
    yield slots.register({ name: 'feature.body', id: 'body' }, () => null);
    yield () => cleanups++;
  });
  assert.equal(setups, 0);
  const declare = () => slots.register({ name: 'main', key: 'feature', children: { 'feature.body': { kind: 'list', scope: 'root' } } }, () => null);
  const first = declare();
  const old = slots.entries('feature.body')[0];
  assert.equal(setups, 1); assert.equal(slots.isLive(old), true);
  first(); assert.equal(cleanups, 1); assert.equal(slots.spec('feature.body'), undefined);
  const second = declare();
  const current = slots.entries('feature.body')[0];
  first(); assert.equal(slots.entries('feature.body')[0], current);
  assert.equal(setups, 2); assert.equal(slots.isLive(old), false);
  off(); off(); assert.equal(cleanups, 2); assert.deepEqual(slots.entries('feature.body'), []);
  second();
  assert.throws(() => slots.inject('settings.section', () => {
    slots.register({ name: 'settings.section', id: 'failed' }, () => null);
    throw new Error('setup failed');
  }), /setup failed/);
  assert.deepEqual(slots.entries('settings.section'), [], '初始化失败不会留下半份注册');
  const cleaned = [];
  const release = context.ctx.effect(() => () => cleaned.push('explicit'));
  release(); release();
  context.ctx.effect(() => () => cleaned.push('remaining'));
  context.ctx.effect(() => () => { cleaned.push('throwing'); throw new Error('cleanup failed'); });
  assert.throws(() => context.dispose(), AggregateError);
  assert.deepEqual(cleaned, ['explicit', 'throwing', 'remaining']);
  assert.deepEqual(context.live(), []);
  context.dispose();
});

test('延迟子槽位初始化失败保留父注册释放句柄，按上游合同异步报告原错误', () => {
  const source = `
    import assert from 'node:assert/strict';
    import { createSlotContext } from './scripts/compat/runtime/client/slot-context.mjs';
    const context = createSlotContext({ scope: {} });
    const slots = context.ctx.slots;
    const errors = [];
    process.on('uncaughtException', error => errors.push(error.message));
    slots.inject('feature.body', () => {
      slots.register({ name: 'feature.body', id: 'partial' }, () => null);
      throw new Error('child setup failed');
    });
    const release = slots.register({ name: 'main', key: 'feature', children: { 'feature.body': { kind: 'list', scope: 'root' } } }, () => null);
    assert.equal(typeof release, 'function');
    assert.deepEqual(slots.entries('feature.body'), []);
    release();
    assert.deepEqual(context.live(), []);
    context.dispose();
    setImmediate(() => { assert.deepEqual(errors, ['child setup failed']); });
  `;
  const result = spawnSync(process.execPath, ['--input-type=module', '-e', source], { cwd: fileURLToPath(new URL('../..', import.meta.url)), encoding: 'utf8', timeout: 5000 });
  assert.equal(result.status, 0, result.stderr);
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

test('原配置组件的 main 别名只保留设置入口，独立业务页面不被名称或包名过滤', async t => {
  const { mkdtempSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { analyzeNativeClient } = await import('./install/client-assets.mjs');
  const { clientViews } = await import('./install/client-views.mjs');
  const directory = mkdtempSync(join(tmpdir(), 'magi-settings-location-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  writeFileSync(join(directory, 'client.js'), `window.__ModuleLoader__.load({id:'arbitrary-plugin',factory(){return {apply(ctx){
    const Configuration=()=>null, Business=()=>null;
    ctx.slots.register({name:'main',key:'old-page',label:'Same title'}, Configuration);
    ctx.slots.register({name:'main',key:'business',label:'Same title'}, Business);
    ctx.slots.register({name:'settings.section',id:'preferences',label:'Same title'}, Configuration);
  }}}});`);
  const analysis = analyzeNativeClient({ upstreamDir: directory, pkg: { exports: { './client': './client.js' } }, sharedRoot: fileURLToPath(new URL('.', import.meta.url)) });
  const views = clientViews(analysis.mounted, 'arbitrary-plugin');
  assert.deepEqual(views.map(view => [view.slot.key, view.placements]), [['business', ['main', 'rightPane']], ['preferences', ['settings']]]);
  assert.ok(analysis.unmounted.some(slot => slot.name === 'main' && slot.key === 'old-page' && slot.settingsDuplicate));
});

test('原生对话替换在安装记录和延迟注册时均拒绝，不能借正常设置页绕过', async t => {
  const { mkdtempSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { analyzeNativeClient } = await import('./install/client-assets.mjs');
  const { createSlotContext } = await import('./runtime/client/slot-context.mjs');
  const directory = mkdtempSync(join(tmpdir(), 'magi-ui-boundary-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  for (const name of ['conversation.chat.node', 'conversation.input.dock']) {
    writeFileSync(join(directory, 'client.js'), `window.__ModuleLoader__.load({id:'business',factory(){return {async apply(ctx){
      ctx.slots.register({name:'plugins.bundle.config',key:'preferences'},()=>null);
      await Promise.resolve();
      ctx.slots.register({name:${JSON.stringify(name)},key:'assistant-step',id:'input'},()=>null);
    }}}});`);
    assert.throws(() => analyzeNativeClient({ upstreamDir: directory, pkg: { exports: { './client': './client.js' } }, sharedRoot: fileURLToPath(new URL('.', import.meta.url)) }), /PLUGIN_UI_BOUNDARY_CONFLICT.*不能安装或启用/);
    const context = createSlotContext({ scope: {} });
    context.ctx.slots.register({ name:'plugins.bundle.config', key:'preferences' }, () => null);
    await Promise.resolve();
    assert.throws(() => context.ctx.slots.register({ name, key:'assistant-step', id:'input' }, () => null), /PLUGIN_UI_BOUNDARY_CONFLICT/);
    assert.equal(context.live().length, 1, '被拒绝注册不能留下替换组件，正常设置页仍可用');
    context.dispose();
  }
  writeFileSync(join(directory, 'client.js'), `window.__ModuleLoader__.load({id:'business',factory(){return {apply(ctx){
    ctx.slots.register({name:'plugins.bundle.config',key:'preferences'},()=>null);
    ctx.slots.register({name:'conversation.session.header.actions',id:'action'},()=>null);
  }}}});`);
  assert.throws(() => analyzeNativeClient({ upstreamDir: directory, pkg: { exports: { './client': './client.js' } }, sharedRoot: fileURLToPath(new URL('.', import.meta.url)) }), /尚未接入的界面位置.*conversation.session.header.actions/);
});

test('独立业务页、设置和受控输入扩展保留，外壳注入不能按 UI 类别静默放行', async t => {
  const { mkdtempSync, writeFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os');
  const { join } = await import('node:path');
  const { createSlotContext } = await import('./runtime/client/slot-context.mjs');
  const context = createSlotContext({ scope: {} });
  for (const descriptor of [{name:'main',key:'business'}, {name:'plugins.bundle.config',key:'preferences'}, {name:'conversation.input.left',id:'action'}, {name:'conversation.input.right',id:'credits'}]) {
    context.ctx.slots.register(descriptor, () => null);
  }
  assert.equal(context.live().length, 4);
  const { clientViews } = await import('./install/client-views.mjs');
  assert.deepEqual(clientViews(context.live().map(entry=>({name:entry.descriptor.name,key:entry.descriptor.key??entry.descriptor.id})), 'Business').map(view=>view.placements), [['main','rightPane'],['settings'],['composerLeft'],['composerLeft']]);
  context.dispose();
  const root = mkdtempSync(join(tmpdir(), 'magi-ui-shell-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(join(root, 'package.json'), JSON.stringify({name:'shell-injection',version:'1.0.0',main:'index.js',dsh:{}}));
  writeFileSync(join(root, 'index.js'), 'export function apply(ctx){ctx.webServer.tapIndex(()=>"<style>body{display:none}</style>")}');
  const report = await createDshTranslator().inspect(root);
  assert.ok(report.verdict.blocked.some(entry => entry.name === 'ctx.webServer.tapIndex'));
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
