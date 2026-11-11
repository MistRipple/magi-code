import test from 'node:test';
import assert from 'node:assert/strict';
import nativeFs from 'node:fs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { configureProfile, withProfile, profilePath, loaderEntries, createProfileReload } from './runtime/profile.mjs';
import fs from './runtime/profile-fs.mjs';
import { load, dump } from 'js-yaml';
import { installSharedResolution } from './runtime/shared-resolution.mjs';
import { createPluginManager } from './runtime/plugin-manager.mjs';
import { profileSpawn } from './runtime/profile.mjs';
import { once } from 'node:events';
import { AsyncLocalStorage } from 'node:async_hooks';
import { createHash } from 'node:crypto';
import { zipSync, strToU8 } from 'fflate';
import { spawn as spawnProfileProcess } from './runtime/profile-process.mjs';
import { pathToFileURL } from 'node:url';

test('管理服务直接安装、启停和移除后的 profile 读取反映本次 daemon 已提交事实', async t => {
  const home=await mkdtemp(join(tmpdir(),'magi-profile-manager-'));t.after(()=>rm(home,{recursive:true,force:true}));
  let plugins=[{manifest:{id:'dsh.target',name:'Target'},digest:'old',enabled:true,active:true,runtime:{state:'running'},
    source:{kind:'upstream',location:'dsh:target@1.0.0'}}];
  const api={plugins:{
    list:async()=>({plugins:structuredClone(plugins)}),
    async installBundle(){plugins[0]={...plugins[0],digest:'new',source:{kind:'upstream',location:'dsh:target@2.0.0'}};
      return {application:'applied',bundle:'target',installed:true,version:'2.0.0',changed:true};},
    async setEnabled(id,enabled,digest){assert.equal(id,'dsh.target');assert.equal(digest,plugins[0].digest);plugins[0].enabled=enabled;plugins[0].active=enabled;},
    async remove(id,digest){assert.equal(id,'dsh.target');assert.equal(digest,plugins[0].digest);plugins=[];},
    async finishChange(input){assert.equal(input.commit,true);assert.ok(input.changeId);return {restored:[]};},
  }};
  const release=configureProfile(api,{home});t.after(release);
  const manager=createPluginManager(api);
  await withProfile(async()=>{
    const manifest=()=>JSON.parse(fs.readFileSync(join(profilePath(),'package.json'),'utf8'));
    assert.equal(manifest().dependencies.target,'1.0.0');
    assert.equal((await manager.installBundle('target@2.0.0')).application,'applied');
    assert.equal(manifest().dependencies.target,'2.0.0','同一次调用不能继续读到更新前版本');
    assert.equal(loaderEntries()[0].fiber.state,2);
    await manager.setBundleEnabled('target',false);
    assert.equal(loaderEntries()[0].disabled,true,'成功启停后 loader 与 daemon 一致');
    await manager.removeBundle('target');
    assert.deepEqual(manifest().dependencies,{});
    assert.deepEqual(loaderEntries(),[],'移除后不保留旧安装条目');
  });
});

test('管理修改先结算有效配置提案，串行读取新版本；取消、无效提案和未知回执不重放业务', async t => {
  const home=await mkdtemp(join(tmpdir(),'magi-profile-mutation-'));t.after(()=>rm(home,{recursive:true,force:true}));
  let plugin={manifest:{id:'dsh.target'},digest:'old',enabled:true,active:true,source:{kind:'upstream',location:'dsh:target@1.0.0'}};
  let failRead=false,installations=0;
  const actions=[];
  const api={plugins:{
    list:async()=>{if(failRead)throw new Error('目录读取中断');return {plugins:[structuredClone(plugin)]};},
    async installBundle(){installations++;plugin={...plugin,digest:'new',source:{kind:'upstream',location:'dsh:target@2.0.0'}};
      return {application:'applied',bundle:'target',version:'2.0.0',installed:true,changed:true};},
    async setEnabled(id,enabled,digest){assert.equal(id,'dsh.target');assert.equal(digest,plugin.digest);actions.push([enabled,digest]);plugin.enabled=enabled;},
  }};
  const release=configureProfile(api,{home});t.after(release);
  const manager=createPluginManager(api);
  await withProfile(async()=>{
    const patch=join(profilePath(),'cordis.patch.yml');
    fs.writeFileSync(patch,dump([{id:'dsh.target',name:'target',disabled:true}]));
    const installed=manager.installBundle('target@2.0.0');
    const enabled=manager.setBundleEnabled('target',true);
    await Promise.all([installed,enabled]);
    assert.deepEqual(actions,[[false,'old'],[true,'new']],'配置提案不能因重读丢失或用旧摘要覆盖新版');
    assert.equal(loaderEntries()[0].disabled,false);
  });
  await assert.rejects(withProfile(async()=>{
    const file=join(profilePath(),'package.json'),document=JSON.parse(fs.readFileSync(file,'utf8'));
    document.dependencies.target='unverified';fs.writeFileSync(file,JSON.stringify(document));
    const result=await manager.installBundle('target@2.0.0');
    assert.equal(result.application,'failed');
    assert.equal(installations,1,'无效清单提案在下一次管理副作用前拒绝');
  }),/清单提案/);
  const controller=new AbortController();
  await assert.rejects(withProfile(async()=>{
    controller.abort(new Error('用户取消'));
    assert.equal((await manager.installBundle('target@2.0.0')).application,'failed');
  },controller.signal),/用户取消/);
  assert.equal(installations,1);
  await withProfile(async()=>{
    api.plugins.installBundle=async()=>{installations++;failRead=true;return {application:'applied',bundle:'target',version:'2.0.0',installed:true,changed:false};};
    const result=await manager.installBundle('target@2.0.0');
    assert.equal(result.application,'failed');assert.equal(result.installed,true);assert.equal(result.version,'2.0.0');
    assert.equal(result.changed,false);assert.equal(result.error.code,'profile-refresh-failed');
    assert.equal(result.stage,'reconcile');
    assert.match(result.error.message,/不要重复已提交/);assert.equal(installations,2);
    failRead=false;
  });
  api.plugins.installBundle=async()=>{installations++;plugin.source.location='dsh:target@3.0.0';throw new Error('安装响应中断');};
  await withProfile(async()=>{
    assert.equal((await manager.installBundle('target@3.0.0')).application,'failed');
    assert.equal(JSON.parse(fs.readFileSync(join(profilePath(),'package.json'),'utf8')).dependencies.target,'3.0.0',
      '业务报错后仍重读 daemon，不将已变更状态藏在旧快照里');
  });
  assert.equal(installations,3);
});

test('同调用移除和安装携带同一恢复身份，失败与取消在关闭快照前结算；成功卸载明确提交', async t => {
  const home=await mkdtemp(join(tmpdir(),'magi-profile-change-'));t.after(()=>rm(home,{recursive:true,force:true}));
  const original={manifest:{id:'dsh.target'},digest:'old',enabled:true,active:true,source:{kind:'upstream',location:'dsh:target@1.0.0'}};
  let plugins=[original],pending,changeId;const receipts=[];
  const api={plugins:{
    list:async()=>({plugins}),
    async remove(id,digest,options){assert.equal(id,original.manifest.id);assert.equal(digest,original.digest);pending=plugins;plugins=[];changeId=options.changeId;assert.ok(changeId);},
    async installBundle(input){assert.equal(input.changeId,changeId);return {application:'failed',error:{message:'下载失败'}};},
    async finishChange(input){assert.equal(input.changeId,changeId);receipts.push(input.commit);if(!input.commit)plugins=pending;pending=undefined;return {restored:input.commit?[]:['dsh.target']};},
  }};
  const off=configureProfile(api,{home});t.after(off);const manager=createPluginManager(api);
  const result=await withProfile(async()=>{
    assert.equal((await manager.removeBundle('target')).application,'applied');
    assert.deepEqual(loaderEntries(),[],'移除时不伪装目标仍已安装');
    const child=profileSpawn('dsh',['plugin','add','target@2.0.0']);
    let stderr='';child.stdout.resume();child.stderr.on('data',chunk=>stderr+=chunk);
    assert.equal((await once(child,'close'))[0],1);
    assert.match(stderr,/下载失败/,'受管进程保留同一移除恢复身份及真实安装错误');
    return {status:500,body:{ok:false,error:'下载失败'}};
  });
  assert.equal(result.status,500);assert.deepEqual(plugins,[original]);
  assert.match(result.body.error,/下载失败[\s\S]*原安装已恢复/);
  const controller=new AbortController();
  await assert.rejects(withProfile(async()=>{await manager.removeBundle('target');controller.abort(new Error('用户取消'));},controller.signal),/用户取消/);
  assert.deepEqual(plugins,[original]);
  await assert.rejects(withProfile(async()=>{await manager.removeBundle('target');throw new Error('原包错误');}),/原包错误/);
  assert.deepEqual(plugins,[original]);
  await withProfile(()=>manager.removeBundle('target'));
  assert.deepEqual(plugins,[]);assert.deepEqual(receipts,[false,false,false,true]);
});

test('显式后台管理在请求结束后读取独立新快照，前台旧调用与被替换实例不能继续修改', async t => {
  const home=await mkdtemp(join(tmpdir(),'magi-profile-background-'));t.after(()=>rm(home,{recursive:true,force:true}));
  let installed=0,reads=0;
  const plugins={
    list:async()=>{reads++;return {plugins:[]};},
    installBundle:async()=>{installed++;return {application:'applied',bundle:'target',installed:true};},
  };
  const off=configureProfile({plugins},{home});
  let ended=false;t.after(()=>{if(!ended)off();});
  const foreground=createPluginManager({plugins});
  const background=createPluginManager({plugins:{...plugins,background:plugins}});
  const controller=new AbortController();let lateForeground,lateBackground;
  await withProfile(()=>{
    lateForeground=AsyncLocalStorage.bind(()=>foreground.installBundle('target@1.0.0'));
    lateBackground=AsyncLocalStorage.bind(()=>background.installBundle('target@1.0.0'));
  },controller.signal);
  controller.abort();
  assert.equal((await lateForeground()).application,'failed');assert.equal(installed,0);
  const before=reads;
  assert.equal((await lateBackground()).application,'applied','后台不借用已取消的前台请求');
  assert.equal(installed,1);assert.ok(reads>=before+2,'后台开始与结算分别读取 daemon 当前事实');
  let finish,began;
  const started=new Promise(resolve=>{began=resolve;});
  plugins.installBundle=async()=>{installed++;began();await new Promise(resolve=>{finish=resolve;});return {application:'applied',installed:true};};
  let inFlight;
  await withProfile(async()=>{inFlight=background.installBundle('target@1.0.0');await started;});
  finish();assert.equal((await inFlight).application,'applied','前台结束不丢弃已开始后台操作的真实回执');
  off();ended=true;
  assert.equal((await lateBackground()).application,'failed');assert.equal(installed,2,'过期实例的后台委托不能继续');
});

test('Node 辅助进程在管理快照读取期间退出，释放读取后不能迟到发起安装', {timeout:10000}, async t => {
  const home=await mkdtemp(join(tmpdir(),'magi-profile-process-read-'));t.after(()=>rm(home,{recursive:true,force:true}));
  let hold=false,installations=0,releaseRead,began;
  const waiting=new Promise(resolve=>{began=resolve;}),gate=new Promise(resolve=>{releaseRead=resolve;});
  const off=configureProfile({plugins:{
    async list(){if(hold){began();await gate;}return {plugins:[]};},
    async installBundle(){installations++;return {application:'applied'};},
    async cancelInstall(){return {status:'not-running'};},
  }},{home});t.after(off);t.after(releaseRead);
  const entry=join(home,'install.mjs');
  await writeFile(entry,`import {callProfileProcess} from ${JSON.stringify(new URL('./runtime/profile-process-call.mjs',import.meta.url).href)};
process.stdout.write('READY\\n');await new Promise(resolve=>process.stdin.once('data',resolve));
await callProfileProcess('install',['add','target@1.0.0','--dir',${JSON.stringify(join(home,'profiles/magi'))}]);`);
  await withProfile(async()=>{
    const child=profileSpawn(process.execPath,[entry],{env:{...process.env,DSH_HOME:home}});
    t.after(()=>child.kill());
    let text='';
    await new Promise(resolve=>child.stdout.on('data',chunk=>{text+=chunk;if(text.includes('READY'))resolve();}));
    child.stderr.resume();hold=true;child.stdin.write('go');await waiting;
    const closed=once(child,'close');child.kill();await closed;
    releaseRead();await new Promise(resolve=>setImmediate(resolve));
    assert.equal(installations,0,'子进程退出后不得借新快照复活安装请求');
  });
});

test('读取管理快照期间替换实例或取消调用，迟到快照不能启动业务或提交配置', async t => {
  const home=await mkdtemp(join(tmpdir(),'magi-profile-snapshot-owner-'));t.after(()=>rm(home,{recursive:true,force:true}));
  let releaseRead,began,installations=0;
  const waiting=new Promise(resolve=>{began=resolve;}),gate=new Promise(resolve=>{releaseRead=resolve;});
  const plugins={
    async list(){began();await gate;return {plugins:[]};},
    async installBundle(){installations++;return {application:'applied'};},
  };
  const off=configureProfile({plugins},{home});
  const pending=createPluginManager({plugins:{...plugins,background:plugins}}).installBundle('target@1.0.0');
  await waiting;off();
  const release=configureProfile({plugins:{list:async()=>({plugins:[]})}},{home});
  releaseRead();
  const result=await pending;
  assert.equal(result.application,'failed');assert.match(result.error.message,/当前实例/);
  assert.equal(installations,0,'旧实例的快照回执不能借新实例身份发起安装');
  release();

  let reads=0,writes=0;
  const controller=new AbortController();
  const close=configureProfile({plugins:{
    async list(){
      if(++reads===2)controller.abort(new Error('用户取消配置提交'));
      return {plugins:[{manifest:{id:'dsh.target'},digest:'one',enabled:true,source:{kind:'upstream',location:'dsh:target@1.0.0'}}]};
    },
    async setEnabled(){writes++;},
  }},{home});t.after(close);
  await assert.rejects(withProfile(()=>{
    fs.writeFileSync(join(profilePath(),'cordis.patch.yml'),dump([{id:'dsh.target',name:'target',disabled:true}]));
  },controller.signal),/用户取消配置提交/);
  assert.equal(writes,0,'目录读取期间取消后不能继续提交启停提案');
});

test('明确指定外部 DSH 实例的 CLI 和 Node 辅助进程保留原环境，不进入 Magi 安装委托', async t => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-ownership-')); t.after(() => rm(home, { recursive: true, force: true }));
  const external = join(home, 'external'), directory = join(external, 'profiles', 'web');
  nativeFs.mkdirSync(directory, { recursive: true });
  nativeFs.writeFileSync(join(directory, 'package.json'), JSON.stringify({ name: 'external-profile', private: true }));
  let installs = 0;
  const release = configureProfile({ plugins: {
    list: async () => ({ plugins: [] }),
    installBundle() { installs++; throw new Error('外部安装不能进入 Magi'); },
  } }, { home }); t.after(release);
  const probe = join(home, 'external-probe.cjs');
  await writeFile(probe, `const fs=require('node:fs');console.log(JSON.stringify({
    home:process.env.DSH_HOME,profile:process.env.DSH_PROFILE,dir:process.env.DSH_PROFILE_DIR,
    delegated:Boolean(process.env.MAGI_PROFILE_PROCESS_TOKEN),
    manifest:JSON.parse(fs.readFileSync(process.env.DSH_PROFILE_DIR+'/package.json','utf8'))
  }));`);
  const env = { ...process.env, DSH_HOME: external, DSH_PROFILE: 'web', DSH_PROFILE_DIR: directory };
  await withProfile(async () => {
    assert.equal(profileSpawn('dsh', ['plugin', '--profile', 'web', 'add', 'outside@1.0.0'], { env }), undefined);
    assert.equal(profileSpawn(process.execPath, [probe], { env }), undefined);
    assert.equal(profileSpawn('dsh', ['--profile', 'web', '--dump-config'], { env: {} }), undefined);
    assert.equal(profileSpawn('dsh', ['plugin', 'add', 'outside@1.0.0'], { env: { ...env, DSH_HOME: join(home, 'not-created') } }), undefined);
    const child = spawnProfileProcess(process.execPath, [probe], { env });
    let output = '', errors = '';
    child.stdout.on('data', data => output += data); child.stderr.on('data', data => errors += data);
    assert.equal((await once(child, 'close'))[0], 0, errors);
    assert.deepEqual(JSON.parse(output), { home: external, profile: 'web', dir: directory, delegated: false,
      manifest: { name: 'external-profile', private: true } });
    const managed = profileSpawn('dsh', ['--profile', 'magi', '--dump-config'], { env: { ...env, DSH_HOME: nativeFs.realpathSync(home) } });
    assert.ok(managed, '当前目录的真实路径仍由唯一 profile 入口管理');
    managed.stdout.resume(); managed.stderr.resume();
    assert.equal((await once(managed, 'close'))[0], 1);
  });
  assert.equal(installs, 0);
  assert.equal(nativeFs.existsSync(join(home, 'profiles')), false);
});

test('profile 配置复制沿同一调用提案，来源说明从 daemon 重读，删除停用项可恢复且旧来源不能覆盖新操作', async t => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-reason-')); t.after(() => rm(home, { recursive: true, force: true }));
  const plugin = { manifest: { id: 'dsh.target' }, digest: 'one', enabled: true, active: true,
    source: { kind: 'upstream', location: 'dsh:target@1.0.0' } };
  const writes = [];
  const release = configureProfile({ plugins: {
    list: async () => ({ plugins: [structuredClone(plugin)] }),
    setEnabled: async (id, enabled, digest, options) => {
      assert.equal(id, plugin.manifest.id); assert.equal(digest, plugin.digest);
      assert.deepEqual(options.expectedState, { enabled: plugin.enabled, ...(plugin.disabledBy ? { disabledBy: plugin.disabledBy } : {}) });
      writes.push({ enabled, options }); plugin.enabled = enabled; plugin.active = enabled;
      if (!enabled && options.reason) plugin.disabledBy = { pluginId: 'manager', name: 'Manager', reason: options.reason };
      else delete plugin.disabledBy;
    },
  } }, { home }); t.after(release);
  const patch = join(profilePath(), 'cordis.patch.yml'), backup = patch + '.backup';
  await withProfile(async () => {
    const original = fs.readFileSync(patch);
    fs.copyFileSync(patch, backup, fs.constants.COPYFILE_EXCL);
    assert.deepEqual(fs.readFileSync(backup), original);
    assert.throws(() => fs.copyFileSync(patch, backup, fs.constants.COPYFILE_EXCL), { code: 'EEXIST' });
    assert.throws(() => fs.copyFileSync(patch, backup, fs.constants.COPYFILE_FICLONE_FORCE), { code: 'ENOTSUP' });
    const exported = join(home, 'exported-config.yml');
    fs.copyFileSync(patch, exported);
    assert.deepEqual(nativeFs.readFileSync(exported), original, '导出使用 daemon 已确认的快照原字节');
    fs.copyFileSync(exported, backup);
    assert.deepEqual(fs.readFileSync(backup), original, '真实备份可读入同一提案');
    assert.throws(() => fs.copyFileSync(exported, join(profilePath(), 'node_modules/target/entry.js')), { code: 'EROFS' });
    const rows = load(original.toString());
    rows.push({ id: 'dsh.target', name: 'target', disabled: true, comment: 'operator paused for diagnosis' });
    fs.writeFileSync(patch, dump(rows));
    assert.equal(plugin.enabled, true);
  });
  assert.equal(plugin.enabled, false); assert.equal(writes[0].options.reason, 'operator paused for diagnosis');
  await withProfile(async () => {
    const rows = load(fs.readFileSync(patch, 'utf8'));
    assert.equal(rows[0].comment, plugin.disabledBy.reason);
    fs.writeFileSync(patch, dump(rows.filter(row => !row.comment)));
  });
  assert.equal(plugin.enabled, true); assert.equal(plugin.disabledBy, undefined);
  assert.equal(writes[1].enabled, true);
  await withProfile(async () => {
    assert.equal(fs.existsSync(backup), false, '备份只存在于调用中，不形成第二份 profile');
    await fs.promises.copyFile(patch, backup);
    assert.deepEqual(await fs.promises.readFile(backup), fs.readFileSync(patch));
  });
  assert.equal(nativeFs.existsSync(patch), false);
});

test('无说明的停用项也可删除恢复，保留其他停用项且旧提案不能覆盖后续操作', async t => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-enable-')); t.after(() => rm(home, { recursive: true, force: true }));
  const plugins = ['target', 'keeper'].map(name => ({ manifest: { id: `dsh.${name}` }, digest: 'one', enabled: name === 'target', active: name === 'target',
    source: { kind: 'upstream', location: `dsh:${name}@1.0.0` } }));
  const writes = [];
  const release = configureProfile({ plugins: {
    list: async () => ({ plugins: structuredClone(plugins) }),
    setEnabled: async (id, enabled, digest, options) => {
      const plugin = plugins.find(value => value.manifest.id === id);
      assert.equal(digest, plugin.digest);
      assert.deepEqual(options.expectedState, { enabled: plugin.enabled, ...(plugin.disabledBy ? { disabledBy: plugin.disabledBy } : {}) });
      writes.push([id, enabled]); plugin.enabled = enabled; plugin.active = enabled;
    },
  } }, { home }); t.after(release);
  const patch = join(profilePath(), 'cordis.patch.yml');
  await withProfile(async () => {
    const rows = load(fs.readFileSync(patch, 'utf8'));
    rows.push({ id: 'dsh.target', disabled: true });
    fs.writeFileSync(patch, dump(rows));
  });
  assert.equal(plugins[0].enabled, false); assert.equal(plugins[0].disabledBy, undefined);
  await withProfile(async () => {
    const rows = load(fs.readFileSync(patch, 'utf8'));
    fs.writeFileSync(patch, dump(rows.filter(row => row.id !== 'dsh.target')));
  });
  assert.equal(plugins[0].enabled, true); assert.equal(plugins[1].enabled, false);
  assert.deepEqual(writes, [['dsh.target', false], ['dsh.target', true]]);
  await assert.rejects(withProfile(async () => {
    fs.writeFileSync(patch, '[]');
    plugins[1].disabledBy = { pluginId: 'another.manager', name: 'Another manager', reason: 'new operation' };
  }), /expectedState|deep-equal/);
  assert.equal(plugins[1].enabled, false); assert.equal(writes.length, 2);
});

test('私有业务文件与安装投影经过符号链接时保持相同路径身份，且不生成安装副本', async t => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-reports-')); t.after(() => rm(home, { recursive: true, force: true }));
  const alias = join(home, 'home-link');
  nativeFs.symlinkSync(home, alias, process.platform === 'win32' ? 'junction' : 'dir');
  const release = configureProfile({ plugins: { list: async () => ({ plugins: [] }) } }, { home: alias }); t.after(release);
  const root = profilePath(), directory = join(root, 'diagnostics'), report = join(directory, 'brief.txt');
  await withProfile(async () => {
    fs.mkdirSync(directory, { recursive: true }); fs.writeFileSync(report, 'real report');
    assert.equal(fs.realpathSync(directory), nativeFs.realpathSync(directory));
    assert.equal(fs.readFileSync(report, 'utf8'), 'real report');
    const entries = fs.readdirSync(root, { withFileTypes: true });
    assert.ok(entries.find(e => e.name === 'diagnostics').isDirectory());
    assert.ok(entries.find(e => e.name === 'package.json').isFile());
    const canonical = fs.realpathSync(root);
    assert.equal(canonical, nativeFs.realpathSync(root), '虚拟根与真实业务文件采用同一个系统路径身份');
    assert.equal(fs.readFileSync(join(canonical, 'package.json'), 'utf8'), fs.readFileSync(join(root, 'package.json'), 'utf8'));
    assert.deepEqual(fs.realpathSync(root, { encoding: 'buffer' }), Buffer.from(canonical));
    assert.equal(fs.realpathSync.native(root), canonical);
    assert.equal(await fs.promises.realpath(root), canonical);
    assert.equal(await new Promise((resolve, reject) => fs.realpath(root, (error, path) => error ? reject(error) : resolve(path))), canonical);
    assert.deepEqual(fs.readdirSync(canonical), fs.readdirSync(root));
  });
  assert.equal(nativeFs.readFileSync(report, 'utf8'), 'real report');
  assert.equal(nativeFs.existsSync(join(root, 'package.json')), false);
  assert.equal(nativeFs.existsSync(join(root, 'cordis.patch.yml')), false);
  await withProfile(async () => assert.equal(fs.readFileSync(report, 'utf8'), 'real report'));
});

test('profile 从 daemon 当前不可变归档读取真实原包文件，保留字节与时间且不物化第二份安装目录', async t => {
  const home=await mkdtemp(join(tmpdir(),'magi-profile-artifacts-'));
  t.after(()=>rm(home,{recursive:true,force:true}));
  const original={name:'@scope/artifact',version:'1.0.0',main:'lib/index.js'};
  const payload=Buffer.from([0,255,3,42]);
  const archive=zipSync({'extension/upstream/package.json':strToU8(JSON.stringify(original)),
    'extension/upstream/lib/index.js':payload,'extension/index.mjs':strToU8('not an upstream file')});
  const archivePath=join(home,'installed.zip');await writeFile(archivePath,archive);
  const digest=createHash('sha256').update(archive).digest('hex');
  const plugin={manifest:{id:'dsh.artifact'},digest,enabled:false,active:false,
    source:{kind:'upstream',location:'dsh:@scope/artifact@1.0.0'},upstreamManifest:original,upstreamArchivePath:archivePath};
  const off=configureProfile({plugins:{list:async()=>({plugins:[plugin]})}},{home});t.after(off);
  await withProfile(async()=>{
    const root=join(profilePath(),'node_modules/@scope/artifact');
    assert.deepEqual(fs.readFileSync(join(root,'lib/index.js')),payload);
    assert.equal(fs.statSync(join(root,'lib/index.js')).mtimeMs,nativeFs.statSync(archivePath).mtimeMs);
    assert.deepEqual(fs.readdirSync(join(root,'lib')),['index.js']);
    assert.equal(fs.existsSync(join(root,'missing.js')),false);
    assert.equal(fs.existsSync(join(root,'index.mjs')),false,'不能把翻译宿主入口混为原包文件');
    assert.throws(()=>fs.writeFileSync(join(root,'lib/index.js'),'change'),{code:'EROFS'});
    assert.equal(nativeFs.existsSync(join(home,'profiles')),false);
    const handle=await fs.promises.open(join(root,'lib/index.js'));
    assert.deepEqual(await handle.readFile(),payload);await handle.close();
  });
  await writeFile(archivePath,'modified archive');
  await assert.rejects(withProfile(()=>fs.readFileSync(join(profilePath(),'node_modules/@scope/artifact/lib/index.js'))),{code:'ECONFLICT'});
});

test('GitHub 原包目录与 profile 共用 daemon 来源，导出保留分支，版本仍来自不可变原包', async t => {
  const home = await mkdtemp(join(tmpdir(), 'magi-github-profile-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  const plugin = { manifest: { id: 'dsh.github', name: 'Original', description: '' }, digest: 'current', enabled: true, active: true,
    source: { kind: 'upstream', location: 'dsh:@scope/github@1.2.0', github: { repository: 'owner/repo', reference: 'main', commit: 'a'.repeat(40) } },
    upstreamManifest: { name: '@scope/github', version: '1.2.0', engines: { dsh: '^0.2' } } };
  const api = { plugins: { list: async () => ({ plugins: [plugin] }) } };
  const off = configureProfile(api, { home }); t.after(off);
  await withProfile(async () => {
    assert.equal(JSON.parse(fs.readFileSync(join(profilePath(), 'package.json'), 'utf8')).dependencies['@scope/github'], 'github:owner/repo#main');
    assert.equal(JSON.parse(fs.readFileSync(join(profilePath(), 'node_modules/@scope/github/package.json'), 'utf8')).version, '1.2.0');
    const [bundle] = await createPluginManager(api).listBundles();
    assert.equal(bundle.source, 'github:owner/repo#main'); assert.equal(bundle.version, '1.2.0');
    assert.equal(loaderEntries()[0].options.name, '@scope/github');
  });
});

test('profile 重载串行提交真实启停意图、等待 daemon 收敛；失败、嵌套、取消与过期调用不冒充成功', async t => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-reconcile-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  const plugins = [{ manifest: { id: 'dsh.target' }, digest: 'current', enabled: true, active: true,
    source: { kind: 'upstream', location: 'dsh:target@1.0.0' }, runtime: { state: 'running', generation: 7 } }];
  let failure, calls = 0;
  const off = configureProfile({ plugins: {
    async list() { return { plugins }; },
    async setEnabled(id, enabled, digest) {
      assert.equal(id, 'dsh.target'); assert.equal(digest, 'current');
      plugins[0].enabled = enabled; plugins[0].active = enabled;
    },
    async reconcile(input) {
      calls++;
      assert.deepEqual(input, { pluginIds: ['dsh.target'] });
      if (failure) throw new Error(failure);
      return { plugins };
    },
  } }, { home });
  let released = false; t.after(() => { if (!released) off(); });
  const reload = createProfileReload();
  await withProfile(async () => {
    await assert.rejects(reload.reconcileProfile(), /inside runExclusive/);
    assert.deepEqual(await reload.runExclusive(() => reload.reconcileProfile(['dsh.target'])), { warnings: [], pending: [] });
    assert.equal(plugins[0].runtime.generation, 7, '原实例保持，不通过重启模拟重载');
    await assert.rejects(reload.runExclusive(() => reload.runExclusive(() => {})), /cannot be nested/);
    fs.writeFileSync(join(profilePath(), 'cordis.patch.yml'), dump([{ id: 'dsh.target', disabled: true }]));
    await reload.runExclusive(() => reload.reconcileProfile());
    assert.equal(plugins[0].active, false, '提案先沿原提交入口生效，再核对运行状态');
    await assert.rejects(reload.runExclusive(() => reload.reconcileProfile(['dsh.target'])), /必需条目未运行/);
    assert.equal(plugins[0].enabled, false, '重载不能重新启用停用项');
    plugins[0].enabled = true;
    const inactive = await reload.runExclusive(() => reload.reconcileProfile());
    assert.match(inactive.warnings[0], /尚未激活/);
    assert.equal(plugins[0].active, false, '未激活项只报告真实状态，不代授或自动激活');
    failure = '真实宿主启动失败';
    await assert.rejects(reload.runExclusive(() => reload.reconcileProfile()), /真实宿主启动失败/);
    failure = undefined;
    const order = [];
    let release, began;
    const started = new Promise(resolve => { began = resolve; });
    const first = reload.runExclusive(async () => { order.push('start'); began(); await new Promise(resolve => { release = resolve; }); order.push('end'); });
    await started;
    const second = reload.runExclusive(() => order.push('second'));
    assert.deepEqual(order, ['start']); release(); await Promise.all([first, second]);
    assert.deepEqual(order, ['start', 'end', 'second']);
  });
  let late;
  await withProfile(() => { late = AsyncLocalStorage.bind(() => reload.runExclusive(() => reload.reconcileProfile())); });
  const before = calls;
  await assert.rejects(late(), /调用快照/);
  const controller = new AbortController();
  await assert.rejects(withProfile(async () => {
    controller.abort(new Error('用户已取消'));
    await reload.runExclusive(() => reload.reconcileProfile());
  }, controller.signal), /用户已取消/);
  off(); released = true;
  await assert.rejects(reload.runExclusive(() => reload.reconcileProfile()), /实例已失效/);
  assert.equal(calls, before, '失效和取消不进入 daemon 收敛入口');
});

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
    assert.ok(Number.isInteger(child.pid) && child.pid > 0, '管理命令必须提供真实进程 PID');
    child.stdout.resume(); child.stderr.resume();
    const closed=once(child,'close'), id=await began;
    // 与原包相同：POSIX 尝试进程组后回到真实 ChildProcess.kill。
    if (process.platform === 'win32') child.kill();
    else try { process.kill(-child.pid, 'SIGTERM'); } catch { child.kill(); }
    assert.deepEqual(await closed,[null,'SIGTERM']);assert.equal(cancelled,id);
  });
});

test('npm/pnpm 当前 profile 包操作共用 daemon 目录，无物理安装树且外部目录保持独立', async t => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-standard-manager-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  let plugins = [], installations = 0;
  const off = configureProfile({ plugins: {
    list: async () => ({ plugins: structuredClone(plugins) }),
    async installBundle(input) {
      installations++; assert.equal(input.package, '@scope/target'); assert.equal(input.enabled, true);
      plugins = [{ manifest: { id: 'dsh.target' }, digest: input.version, enabled: true, active: true,
        source: { kind: 'upstream', location: `dsh:@scope/target@${input.version}` },
        upstreamManifest: { name: '@scope/target', version: input.version } }];
      return { application: 'applied', installed: true, version: input.version };
    },
    async remove(id, digest) { assert.equal(id, 'dsh.target'); assert.equal(digest, plugins[0].digest); plugins = []; },
    async finishChange(input) { assert.equal(input.commit, true); return { restored: [] }; },
  } }, { home }); t.after(off);
  const run = async (file, args, options) => {
    const child = profileSpawn(file, args, options); assert.ok(child);
    let stderr = '', stdout = ''; child.stdout.on('data', bytes => stdout += bytes); child.stderr.on('data', bytes => stderr += bytes);
    return { code: (await once(child, 'close'))[0], stderr, stdout };
  };
  await withProfile(async () => {
    const directory = profilePath(); nativeFs.mkdirSync(directory, { recursive: true });
    assert.equal((await run('npm', ['install', '@scope/target@1.0.0', '--ignore-scripts', '--no-audit', '--no-fund', '--no-progress'], { cwd: directory })).code, 0);
    assert.equal(JSON.parse(fs.readFileSync(join(directory, 'node_modules/@scope/target/package.json'), 'utf8')).version, '1.0.0');
    assert.equal((await run('pnpm', ['add', '@scope/target@2.0.0', '--ignore-scripts', '--reporter=append-only', '--yes', '--dir', directory], { cwd: home })).code, 0);
    assert.equal(JSON.parse(fs.readFileSync(join(directory, 'package.json'), 'utf8')).dependencies['@scope/target'], '2.0.0');
    assert.equal((await run(process.execPath, [join(home, 'npm-cli.js'), 'install', '@scope/target@3.0.0'], { cwd: pathToFileURL(directory) })).code, 0);
    const alias = join(home, 'profile-alias'); nativeFs.symlinkSync(directory, alias, process.platform === 'win32' ? 'junction' : 'dir');
    assert.equal((await run('npm', ['install', '@scope/target@4.0.0'], { cwd: alias })).code, 0);
    assert.equal((await run('npm.cmd', ['uninstall', '@scope/target', '--prefix', directory], { cwd: home })).code, 0);
    assert.deepEqual(loaderEntries(), []);
    for (const args of [
      ['install'], ['install', '@scope/target@1.0.0', '--ignore-scripts=false'],
      ['install', '@scope/target@1.0.0', '--prefix', home, '--prefix', directory],
      ['install', '@scope/target@1.0.0', 'another@1.0.0'],
      ['install', '@scope/target@1.0.0', '--reporter=ndjson', '--reporter=append-only'],
    ]) assert.equal((await run('npm', args, { cwd: directory })).code, 1);
    assert.equal(installations, 4, '无目标或不支持参数不能创建任何安装');
    const reported = await run('dsh', ['plugin', '--profile', 'magi', 'add', '-w', '@scope/target@5.0.0', '--reporter=ndjson']);
    assert.equal(reported.code, 0, reported.stderr);
    assert.deepEqual(JSON.parse(reported.stdout), { name: 'magi:plugin-management', application: 'applied', installed: true, version: '5.0.0' });
    assert.equal(installations, 5, 'DSH 命令与标准包管理使用同一目标解析及真实安装入口');
    assert.equal(profileSpawn('npm', ['install', 'outside@1.0.0'], { cwd: home }), undefined);
    assert.equal(profileSpawn('npm', ['install', 'outside@1.0.0', '--prefix', home], { cwd: directory }), undefined);
    for (const file of ['package.json', 'package-lock.json', 'node_modules']) assert.equal(nativeFs.existsSync(join(directory, file)), false);
  });
  const denied = await run('npm', ['install', '@scope/target@4.0.0'], { cwd: profilePath() });
  assert.equal(denied.code, 1); assert.match(denied.stderr, /当前实例调用快照/);
});

test('标准包管理命令取消同一 native 安装任务，取消后仍可显式重试', async t => {
  const home = await mkdtemp(join(tmpdir(), 'magi-profile-npm-cancel-'));
  t.after(() => rm(home, { recursive: true, force: true }));
  let began, finish, cancelled, attempts = 0;
  const started = new Promise(resolve => { began = resolve; });
  const off = configureProfile({ plugins: {
    list: async () => ({ plugins: [] }),
    installBundle(input) {
      attempts++; if (attempts > 1) return { application: 'applied' };
      began(input.requestId); return new Promise(resolve => { finish = resolve; });
    },
    async cancelInstall(id) { cancelled = id; finish({ application: 'cancelled' }); return { status: 'cancelled' }; },
  } }, { home }); t.after(off);
  await withProfile(async () => {
    const run = () => { const child = profileSpawn('pnpm', ['add', 'target@1.0.0'], { cwd: profilePath() }); child.stdout.resume(); child.stderr.resume(); return child; };
    const child = run(), closed = once(child, 'close'), requestId = await started;
    child.kill(); assert.deepEqual(await closed, [null, 'SIGTERM']); assert.equal(cancelled, requestId);
    assert.equal((await once(run(), 'close'))[0], 0); assert.equal(attempts, 2);
  });
});

test('DSH 管理命令在目录读取期间终止，迟到读取不能继续安装且同调用可显式重试', {timeout:10000}, async t => {
  const home=await mkdtemp(join(tmpdir(),'magi-profile-command-admission-'));t.after(()=>rm(home,{recursive:true,force:true}));
  let hold=false,installations=0,enabled=true,proposals=0,began,releaseRead;
  const waiting=new Promise(resolve=>{began=resolve;}),gate=new Promise(resolve=>{releaseRead=resolve;});
  const off=configureProfile({plugins:{
    async list(){if(hold){began();await gate;}return {plugins:[{manifest:{id:'dsh.keeper'},digest:'one',enabled,source:{kind:'upstream',location:'dsh:keeper@1.0.0'}}]};},
    async installBundle(){installations++;return {application:'applied'};},
    async setEnabled(id,value){assert.equal(id,'dsh.keeper');enabled=value;proposals++;},
    async cancelInstall(){return {status:'not-running'};},
  }},{home});t.after(off);t.after(releaseRead);
  await withProfile(async()=>{
    fs.writeFileSync(join(profilePath(),'cordis.patch.yml'),dump([{id:'dsh.keeper',name:'keeper',disabled:true}]));hold=true;
    const child=profileSpawn('dsh',['plugin','add','target@1.0.0']);
    child.stdout.resume();child.stderr.resume();
    const closed=once(child,'close');await waiting;
    child.kill();hold=false;releaseRead();
    assert.deepEqual(await closed,[null,'SIGTERM']);
    assert.equal(installations,0,'准备中的命令被终止，不能在收到目录后迟到开始安装');
    assert.equal(proposals,1);assert.equal(loaderEntries()[0].disabled,true,'父请求已有的配置提案仍正常结算');
    const retry=profileSpawn('dsh',['plugin','add','target@1.0.0']);
    retry.stdout.resume();retry.stderr.resume();
    assert.equal((await once(retry,'close'))[0],0);
    assert.equal(installations,1,'取消单条命令不污染调用快照或后续显式重试');
  });
});

test('DSH 移除命令等待读取目标时终止，不删除插件或取消同请求其他操作', {timeout:10000}, async t => {
  const home=await mkdtemp(join(tmpdir(),'magi-profile-remove-admission-'));t.after(()=>rm(home,{recursive:true,force:true}));
  let hold=false,removals=0,began,releaseRead;
  const waiting=new Promise(resolve=>{began=resolve;}),gate=new Promise(resolve=>{releaseRead=resolve;});
  const off=configureProfile({plugins:{
    async list(){if(hold){began();await gate;}return {plugins:[{manifest:{id:'dsh.target'},digest:'one',enabled:true,source:{kind:'upstream',location:'dsh:target@1.0.0'}}]};},
    async remove(){removals++;},
    async finishChange(input){assert.equal(input.commit,true);return {restored:[]};},
  }},{home});t.after(off);t.after(releaseRead);
  await withProfile(async()=>{
    hold=true;
    const child=profileSpawn('dsh',['plugin','remove','target']);
    child.stdout.resume();child.stderr.resume();
    const closed=once(child,'close');await waiting;
    child.kill();hold=false;releaseRead();
    assert.deepEqual(await closed,[null,'SIGTERM']);
    assert.equal(removals,0,'终止后收到的目标快照不能继续触发删除');
    const retry=profileSpawn('dsh',['plugin','remove','target']);
    retry.stdout.resume();retry.stderr.resume();
    assert.equal((await once(retry,'close'))[0],0);
    assert.equal(removals,1);
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
    const entries = loaderEntries(); assert.equal(entries.length,1); assert.equal(entries[0].options.name, '@scope/example'); assert.equal(entries[0].fiber.state, 2);
    fs.writeFileSync(join(root, 'cordis.patch.yml'), dump([{ id: 'dsh.example', disabled: true }]));
    assert.equal(enabled, true, '调用结束前没有第二条状态写入路径');
  });
  assert.equal(enabled, false); assert.deepEqual(calls, [false]);
  await withProfile(async () => { assert.equal(loaderEntries()[0].disabled, true); });
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
