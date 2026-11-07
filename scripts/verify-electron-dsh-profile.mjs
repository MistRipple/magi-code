// 真实 npm 原包管理 profile：列表、启停、在线升级、卸载均由原包业务驱动。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, writeFile, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launchApp, connectAppPage, connectTarget, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_PROFILE_PORT || 38946), cdpPort = Number(process.env.MAGI_PROFILE_CDP_PORT || 9316), origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-profile-live-')), output = join(root, 'target/plugin-acceptance/profile');
const report = createReporter(), errors = []; let app, page, frame, ui;
await mkdir(output, { recursive: true });
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type':'application/json' }, ...(body === undefined ? {} : {body:JSON.stringify(body)}), signal:AbortSignal.timeout(300_000) });
  const value = await response.json(); assert.equal(response.status,200,`${path}: ${JSON.stringify(value)}`); return value;
}
const plugins = async () => (await api('/api/plugins')).plugins;
async function install(name,version) {
  const request={ecosystem:'dsh',package:name,version}; const preview=await api('/api/plugins/upstream/preview',request);
  const installed=await api('/api/plugins/upstream/install',{...request,expectedDigest:preview.candidate.digest});
  const plugin=installed.plugins.find((p)=>p.source.location===`dsh:${name}@${version}`);
  assert.ok(plugin); await api(`/api/plugins/${plugin.manifest.id}/enable`,{});
  await api(`/api/plugins/${plugin.manifest.id}/authorize`,{grants:plugin.manifest.permissions}); await api(`/api/plugins/${plugin.manifest.id}/activate`,{});
  await waitFor(async()=>{const p=(await plugins()).find(p=>p.manifest.id===plugin.manifest.id);if(p.runtime?.state==='failed')throw new Error(p.runtime.reason);return p.runtime?.state==='running';},'原包启动',120_000); return plugin;
}
try {
  assert.ok(!await fetch(origin+'/health').then(()=>true,()=>false),'验收端口被占用');
  app=launchApp({executable:join(appRoot,'Contents/MacOS/Magi'),cwd:root,cdpPort,env:{MAGI_STATE_ROOT:join(work,'state'),MAGI_DESKTOP_DAEMON_PORT:String(port),MAGI_EXTENSION_NODE:'',MAGI_EXTENSION_COMPAT:'',MAGI_OPEN_BROWSER:'0'}});
  await waitFor(()=>api('/health'),'daemon',60_000); page=await waitFor(()=>connectAppPage(cdpPort,port,errors),'Electron'); ui=pageTools(page);
  const manager=await install('dsh-plugin-mgr','0.2.12');
  report.check('Mgr 原包在线安装与必需 loader 启动成功',true);
  report.check('管理授权明确声明委托授权能力',manager.manifest.permissions.some(p=>p.kind==='pluginManagement'&&p.targets.includes('authorize')));
  const target=await install('dsh-plugin-release-probe','0.0.7'), targetId=target.manifest.id;
  const current=async()=>(await plugins()).find(p=>p.manifest.id===targetId);
  const route=async(action,body)=>{
    const result=await api(`/api/plugins/${manager.manifest.id}/routes`,{method:body===undefined?'GET':'POST',path:`/api/plugin-manager/${action}`,...(body===undefined?{}:{body})});
    assert.equal(result.status,200,JSON.stringify(result));assert.equal(result.body.ok,true,JSON.stringify(result)); return result.body;
  };
  const list=await route('list'); const row=list.plugins.find(p=>p.name==='dsh-plugin-release-probe');
  report.check('原包从 profile/loader 读到 daemon 的真实包名、版本和启停行',row?.version==='0.0.7'&&row.disabled===false&&row.rows.includes(targetId));
  await route('toggle',{name:'dsh-plugin-release-probe',disable:true});
  report.check('原包写入 patch 的停用提案在回执前真正提交',!(await current()).enabled&&!(await current()).active);
  await route('toggle',{name:'dsh-plugin-release-probe',disable:false});
  await waitFor(async()=>(await current()).runtime?.state==='running','重新启用',120_000);
  report.check('原包启用提案恢复目标宿主运行',true);
  const session=(await api('/api/session/materialize',{scope:'personal'})).sessionId;
  await page.call('Page.navigate',{url:`${origin}/web.html?scope=personal&sessionId=${session}`});
  await waitFor(()=>ui.exists('[data-testid=sidebar-settings]'),'设置');await ui.click('[data-testid=sidebar-settings]');
  const view=manager.manifest.contributions.views[0], nav=`[data-testid=${JSON.stringify(`settings-nav-plugin:${manager.manifest.id}:${view.id}`)}]`;
  await waitFor(()=>ui.exists(nav),'原包设置入口');await ui.click(nav);
  const selector=`iframe[data-plugin-id=${JSON.stringify(manager.manifest.id)}]`;
  await waitFor(()=>ui.exists(selector),'原生页面'); const frameUrl=await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).src`);
  // Chromium may expose an opaque sandboxed same-site iframe as a page target
  // (rather than an OOPIF target) depending on the Electron build.  The URL is
  // still the per-plugin resource URL, so accepting both target kinds keeps
  // this physical check tied to the actual iframe and avoids a false failure.
  frame=await waitFor(()=>connectTarget(cdpPort,t=>(t.type==='iframe'||t.type==='page')&&t.url===frameUrl,'Mgr 原生页面',errors),'Mgr iframe');
  await waitFor(()=>frame.evaluate('document.body.innerText.includes("dsh-plugin-release-probe")'),'原生列表');
  report.check('未经修改的 Mgr 页面展示真实目标插件',true);
  await writeFile(join(output,'manager.png'),Buffer.from(await ui.screenshot(),'base64'));
  const upgraded=await route('update',{name:'dsh-plugin-release-probe'});
  const after=await current();
  report.check('原包 dsh plugin add 调用在线翻译升级，无 PATH CLI 或代码替换',upgraded.updated===true&&upgraded.version==='0.0.8'&&after.manifest.version==='0.0.8');
  await waitFor(async()=>(await current()).runtime?.state==='running','升级后真实运行',120_000);
  report.check('已获用户授权的管理插件沿原生授权入口激活新版',after.active&&after.grants.length>0);
  await route('uninstall',{name:'dsh-plugin-release-probe'});
  report.check('原包 dsh plugin remove 调用真正停止并卸载目标',!await current());
  const location=join(work,'state/extensions/data',manager.manifest.id,'profiles/magi');
  report.check('profile 仅为调用投影，未落盘第二份安装注册表',!await access(location).then(()=>true,()=>false));
  report.check('真实 Electron 与原包页面无未捕获异常',errors.length===0,errors.join('\n'));
  console.log(report.summary());
} catch(error) { report.results.push({name:'profile 原包验收',ok:false,detail:error.stack??String(error)});console.error(error);process.exitCode=1;if(ui)await writeFile(join(output,'failure.png'),Buffer.from(await ui.screenshot(),'base64')).catch(()=>{}); }
finally {await writeFile(join(output,'results.json'),JSON.stringify({checks:report.results,errors,stateRoot:join(work,'state')},null,2));frame?.close();page?.close();if(app){await writeFile(join(output,'electron.log'),app.log.join(''));await app.stop();}}
