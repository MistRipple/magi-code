import test from 'node:test';
import assert from 'node:assert/strict';
import nativeFs from 'node:fs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configureProfile, withProfile, profilePath, loaderEntries } from './runtime/profile.mjs';
import fs from './runtime/profile-fs.mjs';
import { load, dump } from 'js-yaml';
import { installSharedResolution } from './runtime/shared-resolution.mjs';
import { createPluginManager } from './runtime/plugin-manager.mjs';
import { profileSpawn } from './runtime/profile.mjs';
import { once } from 'node:events';

test('原包 Node 辅助进程读取真实 profile，内部包管理经同一 daemon 任务提交并在同步退出前结算', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-child-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  let plugins = [], request;
  const off = configureProfile({ plugins: {
    list: async () => ({ plugins }),
    async installBundle(input) {
      request = input;
      plugins = [{ manifest: { id: 'dsh.child', description: 'native' }, digest: 'current',
        upstreamManifest: { name: '@scope/child', version: '2.0.0', dsh: { bundle: {} } },
        source: { kind: 'upstream', location: 'dsh:@scope/child@2.0.0' }, enabled: true, active: true }];
      return { application: 'applied', version: '2.0.0' };
    },
    async setEnabled(id, enabled, digest) {
      assert.equal(id, 'dsh.child'); assert.equal(digest, 'current');
      plugins[0].enabled = enabled; plugins[0].active = enabled;
    },
  } }, { home }); t.after(off);
  const script = join(home, 'helper.cjs');
  await writeFile(script, `
const fs=require('node:fs'),cp=require('node:child_process');
const path=process.env.DSH_PROFILE_DIR+'/package.json';
const before=JSON.parse(fs.readFileSync(path,'utf8'));
const output=fs.openSync(${JSON.stringify(join(home, 'install.log'))},'w');
const r=cp.spawnSync(process.env.DSH_PACKAGE_MANAGER_COMMAND,[...JSON.parse(process.env.DSH_PACKAGE_MANAGER_ARGS),'add','@scope/child@2.0.0','-w','--dir',process.env.DSH_PROFILE_DIR],{stdio:['ignore',output,output]});
fs.closeSync(output);
if(r.status!==0)throw new Error('install failed');
if(!(r.pid>0))throw new Error('missing real process identity');
const after=JSON.parse(fs.readFileSync(path,'utf8'));
fs.writeFileSync(path,JSON.stringify(after));
fs.writeFileSync(process.env.DSH_PROFILE_DIR+'/cordis.patch.yml','- id: dsh.child\\n  disabled: true\\n');
console.log(JSON.stringify({before:before.dependencies,after:after.dependencies,argv:process.argv.slice(2)}));
process.exit(0);`);
  await withProfile(async () => {
    const child = profileSpawn(process.execPath, [script, 'original-argument']);
    let out = '', err = ''; child.stdout.on('data', data => out += data); child.stderr.on('data', data => err += data);
    assert.equal((await once(child, 'close'))[0], 0, err);
    assert.deepEqual(JSON.parse(out), { before: {}, after: { '@scope/child': '2.0.0' }, argv: ['original-argument'] });
  });
  assert.equal(request.package, '@scope/child'); assert.equal(request.version, '2.0.0'); assert.equal(request.enabled, true);
  assert.ok(request.requestId);
  assert.equal(plugins[0].enabled, false, '同步退出前真正提交启停提案');
  assert.match(nativeFs.readFileSync(join(home, 'install.log'), 'utf8'), /daemon.*提交/);
  assert.equal(nativeFs.existsSync(profilePath()), false, '辅助进程也不物化第二份 profile');
  const nested = join(home, 'nested.mjs');
  await writeFile(nested, `import {spawn} from 'node:child_process';const child=spawn(process.execPath,[${JSON.stringify(script)},'original-argument'],{stdio:['ignore','pipe','pipe']});child.stdout.pipe(process.stdout);child.stderr.pipe(process.stderr);child.on('close',code=>process.exitCode=code);`);
  await withProfile(async () => {
    const child = profileSpawn(process.execPath, [nested]);
    let out='', err=''; child.stdout.on('data',data=>out+=data); child.stderr.on('data',data=>err+=data);
    assert.equal((await once(child, 'close'))[0], 0, err);
    assert.equal(JSON.parse(out).after['@scope/child'], '2.0.0', '派生辅助进程沿父实例继续委托，不产生独立安装事实');
  });
});

test('辅助进程退出或调用取消回收委托并取消自身准备中的安装，其他 profile 参数不进入安装入口', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-child-cancel-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  let resolveInstall, started, cancelled;
  const began = new Promise(resolve => { started = resolve; });
  const stopped = new Promise(resolve => { cancelled = resolve; });
  let calls = 0;
  const off = configureProfile({ plugins: {
    list: async () => ({ plugins: [] }),
    installBundle(input) { calls++; started(input.requestId); return new Promise(resolve => { resolveInstall = resolve; }); },
    async cancelInstall(id) { cancelled(id); resolveInstall({ application: 'cancelled' }); return { status: 'cancelled' }; },
  } }, { home }); t.after(off);
  const script = join(home, 'helper.mjs');
  await writeFile(script, `import {spawnSync} from 'node:child_process';
const result=spawnSync(process.env.DSH_PACKAGE_MANAGER_COMMAND,[...JSON.parse(process.env.DSH_PACKAGE_MANAGER_ARGS),'add','test@1.0.0','-w','--dir',process.argv[2]],{encoding:'utf8'});
console.log(JSON.stringify({status:result.status,stderr:result.stderr}));`);
  await withProfile(async () => {
    const denied = profileSpawn(process.execPath, [script, join(home, 'other-profile')]);
    let output=''; denied.stdout.on('data', data => output += data); denied.stderr.resume();
    await once(denied, 'close');
    assert.match(JSON.parse(output).stderr, /其他 profile/); assert.equal(calls, 0);
  });
  const failedScript = join(home, 'failed-helper.mjs');
  await writeFile(failedScript, `import {writeFileSync} from 'node:fs'; writeFileSync(process.env.DSH_PROFILE_DIR+'/package.json','{"dependencies":{"uncommitted":"*"}}'); process.exit(1);`);
  await withProfile(async () => {
    const child = profileSpawn(process.execPath, [failedScript]);
    let errors = ''; child.stderr.on('data', data => errors += data); child.stdout.resume();
    assert.equal((await once(child, 'close'))[0], 1);
    assert.equal(errors, '', '失败退出丢弃提案，不尝试提交损坏清单');
  });
  const controller = new AbortController();
  await assert.rejects(withProfile(async () => {
    const child = profileSpawn(process.execPath, [script, profilePath()]);
    child.stdout.resume(); child.stderr.resume();
    const closed = once(child, 'close'), id = await began;
    controller.abort(new Error('真实调用已取消'));
    assert.equal(await stopped, id, '取消的是父实例已登记的同一 native 安装任务');
    await closed;
  }, controller.signal), /真实调用已取消/);
});

test('DSH 管理命令的终止请求取消同一个 daemon 安装任务，不留下迟到安装', async t => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-command-cancel-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  let started, finish, cancelled;
  const began = new Promise(resolve => { started=resolve; });
  const off=configureProfile({ plugins: {
    list: async()=>({plugins:[]}),
    installBundle(input) { started(input.requestId); return new Promise(resolve=>{finish=resolve;}); },
    async cancelInstall(id) { cancelled=id; finish({application:'cancelled'}); return {status:'cancelled'}; },
  } }, {home});t.after(off);
  await withProfile(async()=>{
    const child=profileSpawn('dsh',['plugin','add','test@1.0.0']);
    child.stdout.resume(); child.stderr.resume();
    const closed=once(child,'close'), id=await began;
    child.kill();
    assert.equal((await closed)[0],1);assert.equal(cancelled,id);
  });
});

test('profile 同步读取真实调用快照，启停提案经 daemon 与摘要校验提交，无物理副本和失败写入', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-test-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  let enabled = true, digest = 'current';
  const calls = [];
  const off = configureProfile({ plugins: {
    async list() { return { plugins: [{ manifest: { id: 'dsh.example', description: 'real' }, upstreamManifest: { name: '@scope/example', version: '1.2.3' }, digest, source: { kind: 'upstream', location: 'dsh:@scope/example@1.2.3' }, enabled, active: enabled, runtime: { state: enabled ? 'running' : 'stopped' } }] }; },
    async setEnabled(id, value, expected) { assert.equal(id, 'dsh.example'); assert.equal(expected, digest); calls.push(value); enabled = value; },
  } }, { home });
  t.after(off);
  const root = profilePath();
  assert.equal(nativeFs.existsSync(root), false);
  assert.throws(() => fs.readFileSync(join(root, 'package.json')), /调用快照/);
  await withProfile(async () => {
    const manifest = JSON.parse(fs.readFileSync(join(root, 'package.json'), 'utf8'));
    assert.equal(manifest.dependencies['@scope/example'], '1.2.3');
    assert.deepEqual(manifest.dsh.profile.bundles, ['@scope/example']);
    assert.equal(JSON.parse(await fs.promises.readFile(join(root, 'node_modules/@scope/example/package.json'), 'utf8')).name, '@scope/example');
    assert.deepEqual(fs.readdirSync(join(root, '..')), ['magi']);
    const entries = loaderEntries(); assert.equal(entries[1].options.name, '@scope/example'); assert.equal(entries[1].fiber.state, 2);
    fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'dsh.example', disabled: true }]));
    assert.equal(enabled, true, '调用结束前没有第二条状态写入路径');
  });
  assert.equal(enabled, false); assert.deepEqual(calls, [false]);
  await withProfile(async () => { assert.equal(loaderEntries()[1].disabled, true); });
  await assert.rejects(withProfile(async () => {
    fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'dsh.example', disabled: false }]));
    throw new Error('business failed');
  }), /business failed/);
  assert.equal(enabled, false);
  await withProfile(async () => {
    fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'dsh.example', disabled: false }]));
    return { status: 500 };
  });
  assert.equal(enabled, false);
  await assert.rejects(withProfile(async () => { fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'unknown', disabled: false }])); }), /当前已安装/);
  await assert.rejects(withProfile(async () => { fs.writeFileSync(join(root, 'package.json'), JSON.stringify({ dependencies: { unsafe: '*' } })); }), /不一致/);
  await assert.rejects(withProfile(async () => {
    fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'dsh.example', disabled: false }]));
    digest = 'replaced';
  }), /版本.*变化/);
  assert.equal(nativeFs.existsSync(root), false);
});

test('Node 导入外观仅覆盖上游的 profile 访问，其他物理文件仍由原生 fs 读取', async (t) => {
  const dir = await mkdtemp(join(tmpdir(), 'magi-profile-import-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const shared = new URL('.', import.meta.url).pathname;
  const hooks = installSharedResolution(shared, { '@magi/compat-upstream-root': dir + '/' });
  t.after(() => hooks.deregister());
  const off = configureProfile({ plugins: { async list() { return { plugins: [] }; } } }, { home: dir }); t.after(off);
  const physical = join(dir, 'real.txt'); await writeFile(physical, 'physical');
  const entry = join(dir, 'entry.mjs');
  await writeFile(entry, `import {readFileSync} from 'node:fs';import {readFile,opendir,constants} from 'node:fs/promises';export const read=()=>readFileSync(process.env.DSH_PROFILE_DIR+'/package.json','utf8');export const physical=()=>readFile(${JSON.stringify(physical)},'utf8');export async function directories(){const names=[];for await(const entry of await opendir(process.env.DSH_PROFILE_DIR))names.push(entry.name);return names;}export const accessMode=constants.R_OK;`);
  const upstream = await import(entry);
  await withProfile(async () => {
    assert.deepEqual(JSON.parse(upstream.read()).dependencies, {});
    assert.equal(await upstream.physical(), 'physical');
    assert.ok((await upstream.directories()).includes('package.json'));
    assert.equal(upstream.accessMode, nativeFs.constants.R_OK);
  });
  const cjsEntry = join(dir, 'entry.cjs');
  await writeFile(cjsEntry, `const fs=require('node:fs');const promises=require('fs/promises');module.exports={read:()=>fs.readFileSync(process.env.DSH_PROFILE_DIR+'/package.json','utf8'),scan:async()=>{const names=[];for await(const entry of await promises.opendir(process.env.DSH_PROFILE_DIR))names.push(entry.name);return names;}};`);
  const cjs = (await import(cjsEntry)).default;
  await withProfile(async () => {
    assert.deepEqual(JSON.parse(cjs.read()).dependencies, {});
    assert.ok((await cjs.scan()).includes('package.json'));
  });
});

test('profile 包清单保留不可变原包全部元数据，缺失元数据不伪造，身份不匹配明确失败', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-metadata-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  let upstreamManifest = { name: '@scope/example', version: '1.2.3', engines: { dsh: '^0.2' },
    peerDependencies: { '@deepseek-ai/cordis': '~4.1.0' }, repository: { url: 'https://example.test/source' },
    dsh: { bundle: { patch: 'config/loader.yml' }, client: { inject: ['slots'] } }, custom: { retained: true } };
  const off = configureProfile({ plugins: { async list(input) {
    assert.deepEqual(input, { includeUpstreamManifest: true });
    return { plugins: [{ manifest: { id: 'dsh.example' }, upstreamManifest, source: { kind: 'upstream', location: 'dsh:@scope/example@1.2.3' } }] };
  } } }, { home });
  t.after(off);
  const manifestPath = join(profilePath(), 'node_modules/@scope/example/package.json');
  await withProfile(async () => {
    assert.deepEqual(JSON.parse(fs.readFileSync(manifestPath, 'utf8')), upstreamManifest);
    assert.equal(load(fs.readFileSync(join(profilePath(), 'node_modules/@scope/example/config/loader.yml'), 'utf8'))[0].insert[0].id, 'dsh.example');
  });
  upstreamManifest.dsh.bundle.patch = ['a.yml', 'b.yml'];
  await withProfile(async () => {
    assert.equal(load(fs.readFileSync(join(profilePath(), 'node_modules/@scope/example/a.yml'), 'utf8'))[0].insert[0].id, 'dsh.example');
    assert.deepEqual(load(fs.readFileSync(join(profilePath(), 'node_modules/@scope/example/b.yml'), 'utf8')), []);
  });
  upstreamManifest = undefined;
  await withProfile(async () => { assert.equal(fs.existsSync(manifestPath), false); });
  upstreamManifest = { name: 'other', version: '1.2.3' };
  await assert.rejects(withProfile(async () => {}), /元数据身份/);
});

test('通用原子写入后缀只在调用内存在，取消不提交启停提案', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-atomic-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  const off = configureProfile({ plugins: { async list() { return { plugins: [] }; } } }, { home });
  t.after(off);
  const root = profilePath(), temporary = join(root, 'package.json.tmp-random');
  await withProfile(async () => {
    fs.writeFileSync(temporary, fs.readFileSync(join(root, 'package.json')));
    fs.renameSync(temporary, join(root, 'package.json'));
    assert.equal(fs.existsSync(temporary), false);
  });
  const controller = new AbortController();
  await assert.rejects(withProfile(async () => {
    fs.writeFileSync(join(root, 'cordis.patch.yml'), '[]');
    controller.abort();
  }, controller.signal), { name: 'AbortError' });
  assert.equal(nativeFs.existsSync(root), false);
});

test('fs/promises 的目录扫描保留 DSH profile 的异步迭代语义', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-opendir-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  const off = configureProfile({ plugins: { async list() { return { plugins: [] }; } } }, { home });
  t.after(off);
  const root = profilePath();
  await withProfile(async () => {
    const dir = await fs.promises.opendir(root);
    const names = [];
    for await (const entry of dir) names.push(entry.name);
    assert.deepEqual(names.sort(), ['.npmrc', 'cordis.patch.yml', 'cordis.yml', 'package.json']);
    await assert.rejects(dir.read(), { code: 'ERR_DIR_CLOSED' });
    const callbackDir = await new Promise((resolve, reject) => fs.opendir(root, (error, dir) => error ? reject(error) : resolve(dir)));
    const entry = await new Promise((resolve, reject) => callbackDir.read((error, entry) => error ? reject(error) : resolve(entry)));
    assert.equal(entry.isFile(), true);
    await callbackDir.close();
  });
});

test('只读 fd 保留快照、定位读取与游标语义，关闭及跨调用后失效', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-fd-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  const off = configureProfile({ plugins: { async list() { return { plugins: [] }; } } }, { home });
  t.after(off);
  const path = join(profilePath(), 'package.json');
  let stale;
  await withProfile(async () => {
    const expected = fs.readFileSync(path);
    const fd = fs.openSync(path, 'r');
    assert.ok(Number.isInteger(fd) && fd >= 0);
    assert.equal(fs.fstatSync(fd).size, expected.length);
    assert.equal(fs.fstatSync(fd).isFile(), true);
    const buffer = Buffer.alloc(expected.length + 1);
    assert.equal(fs.readSync(fd, buffer, 0, 4, 2), 4);
    assert.deepEqual(buffer.subarray(0, 4), expected.subarray(2, 6));
    assert.equal(fs.readSync(fd, buffer, { length: 3 }), 3, '定位读取不推进文件游标');
    assert.deepEqual(buffer.subarray(0, 3), expected.subarray(0, 3));
    assert.equal(fs.readSync(fd, buffer, 3, buffer.length - 3, null), expected.length - 3);
    assert.deepEqual(buffer.subarray(0, expected.length), expected);
    assert.equal(fs.readSync(fd, buffer, 0, 1, null), 0);
    assert.equal(fs.readSync(fd, buffer, 0, 1, expected.length + 100), 0);
    fs.closeSync(fd);
    assert.throws(() => fs.fstatSync(fd), { code: 'EBADF' });
    assert.throws(() => fs.openSync(path, 'w'), { code: 'EROFS' });
    stale = fs.openSync(path, 0);
    const callbackFd = await new Promise((resolve, reject) => fs.open(path, 'r', (error, fd) => error ? reject(error) : resolve(fd)));
    const readBuffer = Buffer.alloc(2);
    const count = await new Promise((resolve, reject) => fs.read(callbackFd, readBuffer, 0, 2, null, (error, count, actualBuffer) => {
      if (error) return reject(error);
      assert.equal(actualBuffer, readBuffer); resolve(count);
    }));
    assert.equal(count, 2); assert.deepEqual(readBuffer, expected.subarray(0, 2));
    assert.deepEqual(fs.readFileSync(callbackFd), expected.subarray(2));
    await new Promise((resolve, reject) => fs.close(callbackFd, (error) => error ? reject(error) : resolve()));
    const handle = await fs.promises.open(path, 'r');
    assert.equal((await handle.stat()).size, expected.length);
    assert.equal((await handle.read(readBuffer, { length: 2 })).bytesRead, 2);
    assert.equal(await handle.readFile('utf8'), expected.subarray(2).toString('utf8'));
    await handle.close(); await handle.close();
    assert.equal(handle.fd, -1);
    await assert.rejects(handle.stat(), { code: 'EBADF' });
  });
  await withProfile(async () => { assert.throws(() => fs.readSync(stale, Buffer.alloc(1)), { code: 'EBADF' }); });
  assert.equal(nativeFs.existsSync(profilePath()), false);
});

test('管理器同步读当前 daemon 豁免快照，CAS 回执同步到虚拟文件；新调用读取外部变更', async (t) => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-policy-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  let policy = { revision: 0, runtimeVersion: '0.2.2-rc.1', runtimeId: '0123456789abcdef01234567', exemptions: {} };
  const calls = [];
  const api = { plugins: {
    async list() { return { plugins: [] }; },
    async compatibility() { return structuredClone(policy); },
    async setVersionExemption(input) {
      calls.push(input);
      assert.equal(input.expectedRuntimeId, policy.runtimeId);
      assert.equal(input.expectedRevision, policy.revision);
      assert.equal(input.acceptRisk, true);
      policy = { ...policy, revision: policy.revision + 1, exemptions: { [input.target]: [input.runtimeVersion] } };
      return structuredClone(policy);
    },
  } };
  const off = configureProfile(api, { home }); t.after(off);
  const manager = createPluginManager(api);
  assert.throws(() => manager.listVersionExemptions(), /调用快照/);
  await withProfile(async () => {
    assert.deepEqual(manager.listVersionExemptions(), { runtimeVersion: policy.runtimeVersion, exemptions: {} });
    await manager.setVersionExemption('@scope/plugin@1.0.0', policy.runtimeVersion, true, true);
    assert.deepEqual(JSON.parse(fs.readFileSync(join(profilePath(), 'compatibility.json'), 'utf8')), policy.exemptions);
    assert.equal(manager.listVersionExemptions().exemptions['@scope/plugin@1.0.0'][0], policy.runtimeVersion);
  });
  assert.equal(calls.length, 1);
  policy = { ...policy, revision: policy.revision + 1, exemptions: {} };
  await withProfile(async () => { assert.deepEqual(manager.listVersionExemptions().exemptions, {}); });
  assert.equal(nativeFs.existsSync(profilePath()), false);
});
