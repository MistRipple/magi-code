// 真实 npm 原包管理 profile：列表、启停、在线升级、卸载均由原包业务驱动。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, writeFile, readFile, access, rename, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { Readable } from 'node:stream';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { launchApp, connectAppPage, connectTarget, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_PROFILE_PORT || 38946), cdpPort = Number(process.env.MAGI_PROFILE_CDP_PORT || 9316), origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-profile-live-')), output = process.env.MAGI_DSH_PROFILE_OUTPUT || join(root, 'target/plugin-acceptance/profile');
const report = createReporter(), errors = []; let app, page, frame, ui;
let failResolution = false, holdResolution = false, rejectedResolutions = 0, targetResolutions = 0;
const held = new Set();
// 只使本次 daemon 的候选解析失败；原包、组件和 npm 原字节不替换。
const registryProxy = createServer(async (request,response)=>{
  if(request.headers['pacote-pkg-id']==='registry:dsh-plugin-release-probe') {
    targetResolutions++;
    if(holdResolution) {held.add(response);response.on('close',()=>held.delete(response));return;}
  }
  if(failResolution&&request.headers['pacote-pkg-id']==='registry:dsh-plugin-release-probe') {
    rejectedResolutions++;response.writeHead(404,{'content-type':'application/json'});response.end('{"error":"acceptance-resolution-failure"}');return;
  }
  try {
    const result=await fetch(`https://registry.npmjs.org${request.url}`,{headers:{accept:request.headers.accept??'application/json'},signal:AbortSignal.timeout(120_000)});
    response.writeHead(result.status,{'content-type':result.headers.get('content-type')??'application/json'});
    Readable.fromWeb(result.body).on('error',()=>response.destroy()).pipe(response);
  } catch(error) {response.writeHead(502);response.end(String(error));}
});
await new Promise(resolve=>registryProxy.listen(0,'127.0.0.1',resolve));
const registry=`http://127.0.0.1:${registryProxy.address().port}`;
await mkdir(output, { recursive: true });
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type':'application/json' }, ...(body === undefined ? {} : {body:JSON.stringify(body)}), signal:AbortSignal.timeout(300_000) });
  const value = await response.json(); assert.equal(response.status,200,`${path}: ${JSON.stringify(value)}`); return value;
}
const plugins = async () => (await api('/api/plugins')).plugins;
async function start() {
  app=launchApp({executable:join(appRoot,'Contents/MacOS/Magi'),cwd:root,cdpPort,env:{MAGI_STATE_ROOT:join(work,'state'),MAGI_DESKTOP_DAEMON_PORT:String(port),MAGI_NPM_REGISTRY:registry,MAGI_EXTENSION_NODE:'',MAGI_EXTENSION_COMPAT:'',MAGI_OPEN_BROWSER:'0'}});
  await waitFor(()=>api('/health'),'daemon',60_000);
  page=await waitFor(()=>connectAppPage(cdpPort,port,errors),'Electron');ui=pageTools(page);
  await waitFor(()=>ui.exists('[data-testid=sidebar-settings]'),'工作台初始化');
}
async function stop(logName) {
  frame?.close();frame=undefined;page?.close();page=undefined;
  await writeFile(join(output,logName),app.log.join(''));await app.stop();app=undefined;
}
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
  await start();
  let manager=await install('dsh-plugin-mgr','0.2.12');
  report.check('Mgr 原包在线安装与必需 loader 启动成功',true);
  report.check('管理授权明确声明委托授权能力',manager.manifest.permissions.some(p=>p.kind==='pluginManagement'&&p.targets.includes('authorize')));
  // 旧发行版经真实管理路由调用 dsh 命令，核对公共命令边界；
  // 这不将旧版页面的 Button 缺陷计为已修复。
  const cliTarget=await install('dsh-plugin-release-probe','0.0.7');
  const cliUpdate=await api(`/api/plugins/${manager.manifest.id}/routes`,{
    method:'POST',path:'/api/plugin-manager/update',body:{name:'dsh-plugin-release-probe'},
  });
  const cliUpdated=await waitFor(async()=>{
    const plugin=(await plugins()).find(p=>p.manifest.id===cliTarget.manifest.id);
    return plugin?.manifest.version==='0.0.8'&&plugin.runtime?.state==='running'&&plugin;
  },'原包 dsh add 在线替换与运行',180_000);
  report.check('旧版原包业务通过公共 dsh 命令完成在线替换且回执读取实际新版',
    cliUpdate.status===200&&cliUpdate.body.ok&&cliUpdate.body.updated&&cliUpdate.body.version==='0.0.8'&&cliUpdated.active,JSON.stringify(cliUpdate));
  const cliRemove=await api(`/api/plugins/${manager.manifest.id}/routes`,{
    method:'POST',path:'/api/plugin-manager/uninstall',body:{name:'dsh-plugin-release-probe'},
  });
  report.check('旧版原包业务通过公共 dsh remove 完成排空与移除',
    cliRemove.status===200&&cliRemove.body.ok&&!(await plugins()).some(p=>p.manifest.id===cliTarget.manifest.id),JSON.stringify(cliRemove));
  const oldManager=manager;
  await ui.click('[data-testid=sidebar-settings]');
  await waitFor(()=>ui.exists('[data-testid=settings-nav-plugins]'),'原生插件管理入口');await ui.click('[data-testid=settings-nav-plugins]');
  const nativeRow=`[data-testid=plugin-row][data-plugin-id=${JSON.stringify(manager.manifest.id)}]`;
  await waitFor(()=>ui.exists(nativeRow),'已安装旧版 Mgr');await ui.click(nativeRow);
  await waitFor(()=>ui.exists('[data-testid=plugin-detail-update]'),'原生在线更新入口',90_000);
  await ui.click('[data-testid=plugin-detail-update]');
  await waitFor(()=>ui.exists('[data-testid=plugin-candidate][data-operation=upgrade]'),'Mgr 在线升级预览',240_000);
  report.check('原生在线更新预览实际获取 Mgr 0.2.13，不替换本地页面代码',
    (await ui.text('[data-testid=plugin-candidate]')).includes('0.2.13'));
  await ui.click('[data-action=cancel-candidate]');
  const cancelled=(await plugins()).find(p=>p.manifest.id===manager.manifest.id);
  report.check('取消 Mgr 在线升级保留旧摘要、授权及真实运行',cancelled.digest===oldManager.digest&&cancelled.active&&cancelled.runtime.state==='running');
  await ui.click('[data-testid=plugin-detail-update]');
  await waitFor(()=>ui.exists('[data-testid=plugin-candidate][data-operation=upgrade]'),'再次预览在线升级',240_000);
  await ui.click('[data-action=confirm-candidate]');
  manager=await waitFor(async()=>{const value=(await plugins()).find(p=>p.manifest.id===oldManager.manifest.id);return value.manifest.version==='0.2.13'&&value;},'在线提交新版 Mgr',240_000);
  report.check('Mgr 0.2.12 沿原生在线事务更新到 0.2.13，新摘要要求重新授权',
    manager.digest!==oldManager.digest&&manager.enabled&&!manager.active&&manager.grants.length===0);
  await waitFor(()=>ui.exists('[data-action=activate]'),'新版原生授权入口');await ui.click('[data-action=activate]');
  manager=await waitFor(async()=>{const value=(await plugins()).find(p=>p.manifest.id===oldManager.manifest.id);return value.runtime?.state==='running'&&value;},'新版 Mgr 授权后运行',120_000);
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
  await frame.evaluate(`document.querySelector('.dshpm-search-icon').click();const input=document.querySelector('.dshpm-search-box input');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,'release-probe');input.dispatchEvent(new Event('input',{bubbles:true}));`);
  await waitFor(()=>frame.evaluate(`document.querySelectorAll('.dshpm-card').length===1 && document.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe'`),'原包搜索过滤到真实目标');
  report.check('原包搜索按输入过滤真实安装目录',true);
  await frame.evaluate(`document.querySelector('.dshpm-search-clear').click();document.querySelectorAll('.dshpm-seg-btn')[1].click();`);
  await waitFor(()=>frame.evaluate(`document.querySelector('.dshpm-root')?.classList.contains('is-cols-2')`),'原包两列布局');
  await frame.evaluate(`window.__acceptanceDocument=true;location.reload();`);
  await waitFor(()=>frame.evaluate(`window.__acceptanceDocument===undefined && document.querySelector('.dshpm-root')?.classList.contains('is-cols-2')`),'重开原包页面恢复布局偏好');
  report.check('原包布局偏好在真实隔离页面重载后恢复',true);
  await writeFile(join(output,'manager.png'),Buffer.from(await ui.screenshot(),'base64'));
  // 只在本次隔离状态根阻止已校验原包的物化，不修改 npm 原包或业务代码。
  await route('toggle',{name:'dsh-plugin-release-probe',disable:true});
  const entry=join(work,'state/extensions/packages',targetId,target.digest,'extension/index.mjs');
  const saved=entry+'.acceptance-save';
  await rename(entry,saved); await mkdir(entry);
  try {
    await api(`/api/plugins/${targetId}/enable`,{}); await api(`/api/plugins/${targetId}/activate`,{});
    const failed=await waitFor(async()=>{const p=await current();return p.runtime?.state==='failed'?p:false;},'目标真实运行失败',30_000);
    const row=await waitFor(async()=>{const p=(await route('list')).plugins.find(p=>p.name==='dsh-plugin-release-probe');return p?.error?p:false;},'Mgr 读取 daemon 故障');
    report.check('其他插件真实启动失败进入 Mgr 原包故障详情，管理器保持运行',
      row.error.includes(failed.runtime.reason) && (await plugins()).find(p=>p.manifest.id===manager.manifest.id).runtime.state==='running',JSON.stringify({row,failed:failed.runtime}));
    await api(`/api/plugins/${manager.manifest.id}/deactivate`,{});
    await waitFor(async()=>!await ui.exists(selector),'停止管理器后回收旧页面');frame.close();
    await api(`/api/plugins/${manager.manifest.id}/activate`,{});
    await waitFor(async()=>(await plugins()).find(p=>p.manifest.id===manager.manifest.id).runtime?.state==='running','管理器重启');
    await waitFor(()=>ui.exists(selector),'管理器页面恢复');
    frame=await waitFor(()=>connectTarget(cdpPort,t=>(t.type==='iframe'||t.type==='page')&&t.url===frameUrl,'恢复的 Mgr 页面',errors),'恢复的 Mgr iframe');
    report.check('管理器自身重启后从 daemon 重新读取已有故障，不依赖旧实例缓存',
      (await route('list')).plugins.find(p=>p.name==='dsh-plugin-release-probe').error===row.error);
    await waitFor(()=>frame.evaluate('Boolean(document.querySelector(".dshpm-state.is-err"))'),'重启后原包自动加载错误卡片，无需刷新或重试');
    report.check('原包页面等待后台就绪再初始化，重启后首个列表请求自动成功',true);
    await frame.evaluate(`(() => {const card=[...document.querySelectorAll('.dshpm-card')].find(el=>el.innerText.includes('dsh-plugin-release-probe'));if(!card.classList.contains('is-open'))card.querySelector('.dshpm-row').click();})()`);
    await waitFor(()=>frame.evaluate(`document.body.innerText.includes(${JSON.stringify(failed.runtime.reason)})`),'原包详情展示真实错误原因');
    await writeFile(join(output,'manager-failed.png'),Buffer.from(await ui.screenshot(),'base64'));
    await route('toggle',{name:'dsh-plugin-release-probe',disable:true});
  } finally { await rm(entry,{recursive:true,force:true});await rename(saved,entry); }
  await route('toggle',{name:'dsh-plugin-release-probe',disable:false});
  await waitFor(async()=>(await current()).runtime?.state==='running','修复后恢复运行',120_000);
  await waitFor(async()=>!(await route('list')).plugins.find(p=>p.name==='dsh-plugin-release-probe')?.error,'Mgr 清除恢复后的故障');
  await waitFor(()=>frame.evaluate(`(() => {const b=[...document.querySelectorAll('button')].find(el=>/^(刷新|Refresh)$/.test(el.innerText.trim())&&!el.disabled);if(!b)return false;b.click();return true;})()`),'恢复后原包刷新');
  await waitFor(()=>frame.evaluate('!document.querySelector(".dshpm-state.is-err")'),'恢复后原包正常卡片');
  report.check('修复后沿原包重新启用恢复实际运行，原包页面清除故障',true);
  await writeFile(join(output,'manager-recovered.png'),Buffer.from(await ui.screenshot(),'base64'));
  await waitFor(()=>frame.evaluate(`Boolean([...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe')?.querySelector('.dshpm-btn-update'))`),'原包显示在线更新入口',60_000);
  await frame.evaluate(`(() => {const card=[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe');card.querySelector('.dshpm-btn-update').click();})()`);
  await waitFor(()=>frame.evaluate(`Boolean(document.querySelector('[role=dialog]'))`),'原包更新确认');
  report.check('原包更新确认显示目标名称和实际新版',await frame.evaluate(`document.querySelector('[role=dialog]').innerText.includes('dsh-plugin-release-probe') && document.querySelector('[role=dialog]').innerText.includes('0.0.8')`));
  await writeFile(join(output,'manager-update-confirm.png'),Buffer.from(await ui.screenshot(),'base64'));
  await page.call('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false});
  const narrowConfirm=await waitFor(()=>frame.evaluate(`(() => {
    const dialog=document.querySelector('[role=dialog]'),button=[...dialog.querySelectorAll('button')].find(el=>/^(更新|Update)$/.test(el.innerText.trim()));
    const box=dialog.getBoundingClientRect(),action=button.getBoundingClientRect();
    return box.width>0&&box.left>=0&&box.right<=innerWidth&&box.top>=0&&box.bottom<=innerHeight
      &&action.width>0&&action.left>=0&&action.right<=innerWidth&&action.top>=0&&action.bottom<=innerHeight
      &&document.documentElement.scrollWidth<=innerWidth;
  })()`),'390px 原包确认框与主要操作完整可达');
  report.check('390px 原包更新确认完整展示并保留可见主操作',narrowConfirm);
  await writeFile(join(output,'manager-update-confirm-390.png'),Buffer.from(await ui.screenshot(),'base64'));
  await page.call('Emulation.clearDeviceMetricsOverride');
  await frame.evaluate(`[...document.querySelectorAll('[role=dialog] button')].find(button=>/^(取消|Cancel)$/.test(button.innerText.trim())).click();`);
  await waitFor(()=>frame.evaluate(`!document.querySelector('[role=dialog]')`),'取消原包更新');
  report.check('取消原包更新确认保留目标版本与运行',(await current()).digest===target.digest&&(await current()).runtime.state==='running');
  await frame.evaluate(`[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe').querySelector('.dshpm-btn-update').click();`);
  await waitFor(()=>frame.evaluate(`Boolean(document.querySelector('[role=dialog]'))`),'故障验收更新确认');
  const beforeFailure=await current();failResolution=true;
  await frame.evaluate(`[...document.querySelectorAll('[role=dialog] button')].find(button=>/^(更新|Update)$/.test(button.innerText.trim())).click();`);
  const failedFeedback=await waitFor(()=>frame.evaluate(`document.querySelector('.dshpm-toast-err')?.innerText.includes('原安装已恢复')&&document.querySelector('.dshpm-toast-err').innerText`),'原页面说明更新失败且恢复原安装',180_000);
  failResolution=false;
  const recovered=await waitFor(async()=>{const p=await current();return p?.runtime?.state==='running'&&p;},'恢复原版本真实运行',120_000);
  report.check('先移除后解析失败恢复原摘要、授权及启停意愿，原页面说明恢复结果',
    rejectedResolutions>0&&recovered.digest===beforeFailure.digest&&recovered.enabled===beforeFailure.enabled&&recovered.active===beforeFailure.active
      &&JSON.stringify(recovered.grants)===JSON.stringify(beforeFailure.grants),JSON.stringify({failedFeedback,rejectedResolutions}));
  await frame.evaluate(`Promise.all(document.querySelector('.dshpm-toast-err').getAnimations().map(animation=>animation.finished.catch(()=>{})))`);
  await writeFile(join(output,'manager-update-failed-restored.png'),Buffer.from(await ui.screenshot(),'base64'));
  holdResolution=true;
  const interrupted=api(`/api/plugins/${manager.manifest.id}/routes`,{method:'POST',path:'/api/plugin-manager/update',body:{name:'dsh-plugin-release-probe'}})
    .then(value=>({value}),error=>({error:String(error)}));
  await waitFor(()=>held.size>0,'旧版已移除后的候选解析等待',30_000);
  const checkpoint=JSON.parse(await readFile(join(work,'state/plugins/state.json'),'utf8'));
  report.check('移除后的原安装与归档引用持久保存在 daemon 唯一安装状态',
    !checkpoint.plugins[targetId]&&checkpoint.removals[targetId]?.installed.digest===beforeFailure.digest);
  await writeFile(join(output,'state-before-interruption.json'),JSON.stringify(checkpoint,null,2));
  const {stdout}=await promisify(execFile)('ps',['-axo','pid=,ppid=,command=']);
  const processes=stdout.split('\n').map(line=>line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/)).filter(Boolean);
  const daemon=processes.find(([,pid,parent,command])=>Number(parent)===app.child.pid&&command.includes('/daemon/magi-daemon'));
  assert.ok(daemon,'仅终止本次验收应用的受控 daemon');
  const resolver=processes.find(([,pid,parent,command])=>Number(parent)===Number(daemon[1])&&command.includes('/compat/cli.mjs resolve dsh-plugin-release-probe'));
  assert.ok(resolver,'核对本次 daemon 正在运行的真实翻译进程');
  frame.close();frame=undefined;await page.call('Page.navigate',{url:'about:blank'});page.close();page=undefined;
  process.kill(Number(daemon[1]),'SIGKILL');
  await waitFor(()=>fetch(origin+'/health').then(()=>false,()=>true),'中断 daemon 已停止监听');
  await waitFor(()=>{try{process.kill(Number(resolver[1]),0);return false;}catch(error){if(error.code==='ESRCH')return true;throw error;}},'daemon 崩溃后所有者管道结束真实翻译进程',30_000);
  report.check('daemon 断进程后联网中的翻译工具链退出，旧解析连接不再重试',true);
  await stop('electron-interrupted.log');await interrupted;
  holdResolution=false;for(const response of held)response.destroy();
  const resolutionsBeforeRestart=targetResolutions;
  await start();
  const restored=await waitFor(async()=>{const p=await current();return p?.runtime?.state==='running'&&p;},'断进程后原安装自动恢复运行',120_000);
  report.check('候选提交前 daemon 断进程后恢复原版本及授权，不重新下载或重放安装',
    restored.digest===beforeFailure.digest&&restored.enabled===beforeFailure.enabled&&restored.active===beforeFailure.active
      &&JSON.stringify(restored.grants)===JSON.stringify(beforeFailure.grants)&&targetResolutions===resolutionsBeforeRestart,
    JSON.stringify({restored,beforeFailure,targetResolutions,resolutionsBeforeRestart}));
  await ui.click('[data-testid=sidebar-settings]');await waitFor(()=>ui.exists(nav),'断进程恢复后原包入口');await ui.click(nav);
  const recoveredUrl=await waitFor(()=>page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`),'断进程恢复后原包页面');
  frame=await waitFor(()=>connectTarget(cdpPort,t=>['iframe','page'].includes(t.type)&&t.url===recoveredUrl,'恢复 Mgr 页面',errors),'断进程后原包文档');
  await waitFor(()=>frame.evaluate(`Boolean([...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe'))`),'断进程后原包真实目录');
  await frame.evaluate(`(() => {const card=[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe');if(!card.classList.contains('is-open'))card.querySelector('.dshpm-row').click();})()`);
  await waitFor(()=>frame.evaluate(`Boolean([...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe')?.querySelector('.dshpm-btn-update'))`),'断进程后原包更新入口恢复',120_000);
  await frame.evaluate(`[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe').querySelector('.dshpm-btn-update').click();`);
  await waitFor(()=>frame.evaluate(`Boolean(document.querySelector('[role=dialog]'))`),'重新打开更新确认');
  await frame.evaluate(`[...document.querySelectorAll('[role=dialog] button')].find(button=>/^(更新|Update)$/.test(button.innerText.trim())).click();`);
  await waitFor(async()=>(await current()).manifest.version==='0.0.8','原包确认按钮完成在线更新',180_000);
  const after=await current();
  report.check('原包确认按钮在线翻译升级，无 PATH CLI 或代码替换',after.manifest.version==='0.0.8');
  await waitFor(async()=>(await current()).runtime?.state==='running','升级后真实运行',120_000);
  report.check('已获用户授权的管理插件沿原生授权入口激活新版',after.active&&after.grants.length>0);
  await waitFor(()=>frame.evaluate(`Boolean([...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe')?.innerText.includes('0.0.8'))`),'原包页面显示更新结果');
  const updateFeedback=await waitFor(async()=>{const feedback=await frame.evaluate(`Array.from(document.querySelectorAll('.dshpm-toast')).map(node=>({text:node.innerText,error:node.classList.contains('dshpm-toast-err')}))`);return feedback.length&&feedback;},'原包完成更新并显示业务回执',180_000);
  await writeFile(join(output,'update-feedback.json'),JSON.stringify(updateFeedback,null,2));
  report.check('原包更新回执与实际新版一致，不把已经完成的更新误报为未更新',updateFeedback.some(item=>!item.error&&item.text.includes('0.0.8'))&&!updateFeedback.some(item=>item.error),JSON.stringify(updateFeedback));
  await frame.evaluate(`(() => {const card=[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe');card.querySelector('.dshpm-btn-danger').click();})()`);
  await waitFor(()=>frame.evaluate(`Boolean(document.querySelector('[role=dialog]'))`),'原包卸载确认');
  report.check('卸载确认包含真实包名和已更新版本',await frame.evaluate(`document.querySelector('[role=dialog]').innerText.includes('dsh-plugin-release-probe') && document.querySelector('[role=dialog]').innerText.includes('0.0.8')`));
  await writeFile(join(output,'manager-uninstall-confirm.png'),Buffer.from(await ui.screenshot(),'base64'));
  await frame.evaluate(`[...document.querySelectorAll('[role=dialog] button')].find(button=>/^(取消|Cancel)$/.test(button.innerText.trim())).click();`);
  await waitFor(()=>frame.evaluate(`!document.querySelector('[role=dialog]')`),'取消原包卸载');
  report.check('取消原包卸载保留更新后真实版本与运行',(await current()).digest===after.digest&&(await current()).runtime.state==='running');
  await stop('electron-before-restart.log');await start();
  const restarted=await waitFor(async()=>{const value=(await plugins()).find(p=>p.manifest.id===manager.manifest.id);return value?.runtime?.state==='running'&&value;},'应用重启恢复新版 Mgr',120_000);
  report.check('应用重启保留在线更新的 Mgr 与目标插件，不重新替换版本',restarted.digest===manager.digest&&(await current()).digest===after.digest);
  await ui.click('[data-testid=sidebar-settings]');await waitFor(()=>ui.exists(nav),'重启后原包入口');await ui.click(nav);
  const restartedUrl=await waitFor(()=>page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`),'重启后原包页面');
  frame=await waitFor(()=>connectTarget(cdpPort,t=>['iframe','page'].includes(t.type)&&t.url===restartedUrl,'重启 Mgr 原页面',errors),'重启后原包文档');
  await waitFor(()=>frame.evaluate(`document.querySelector('.dshpm-root')?.classList.contains('is-cols-2')&&document.body.innerText.includes('dsh-plugin-release-probe')`),'应用重启恢复原包布局及真实安装列表');
  await frame.evaluate(`(() => {const card=[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe');if(!card.classList.contains('is-open'))card.querySelector('.dshpm-row').click();})()`);
  await waitFor(()=>frame.evaluate(`Boolean([...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe')?.innerText.includes('0.0.8'))`),'展开重启后的原包详情读取实际版本');
  report.check('应用重启后原包隔离布局偏好与实际更新结果一致',true);
  await frame.evaluate(`[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe').querySelector('.dshpm-btn-danger').click();`);
  await waitFor(()=>frame.evaluate(`Boolean(document.querySelector('[role=dialog]'))`),'重启后再次卸载确认');
  await frame.evaluate(`[...document.querySelectorAll('[role=dialog] button')].find(button=>/^(卸载|Uninstall)$/.test(button.innerText.trim())).click();`);
  await waitFor(async()=>!await current(),'原包确认按钮完成卸载',60_000);
  report.check('原包 dsh plugin remove 调用真正停止并卸载目标',!await current());
  const location=join(work,'state/extensions/data',manager.manifest.id,'profiles/magi');
  report.check('profile 仅为调用投影，未落盘第二份安装注册表',!await access(location).then(()=>true,()=>false));
  report.check('真实 Electron 与原包页面无未捕获异常',errors.length===0,errors.join('\n'));
  console.log(report.summary());
} catch(error) {
  report.results.push({name:'profile 原包验收',ok:false,detail:error.stack??String(error)});
  console.error(error);process.exitCode=1;
  // 记录原包所见的真实运行库，不能把无异常但缺失的确认操作算成通过。
  if(frame) {
    const diagnostics=await frame.evaluate(`(() => {
      const ui=window.__MAGI_DSH_MODULES__?.['@deepseek-ai/dsh-client-ui-primitives'];
      return {moduleLoaded:Boolean(ui),exports:Object.fromEntries(['Modal','Button'].map(name=>[name,{type:typeof ui?.[name],reactType:String(ui?.[name]?.$$typeof ?? '')}])),dialogVisible:Boolean(document.querySelector('[role=dialog]'))};
    })()`).catch(()=>null);
    await writeFile(join(output,'ui-modules.json'),JSON.stringify(diagnostics,null,2));
  }
  if(ui)await writeFile(join(output,'failure.png'),Buffer.from(await ui.screenshot(),'base64')).catch(()=>{});
}
finally {await writeFile(join(output,'results.json'),JSON.stringify({checks:report.results,errors,stateRoot:join(work,'state')},null,2));frame?.close();page?.close();if(app){await writeFile(join(output,'electron.log'),app.log.join(''));await app.stop();}registryProxy.closeAllConnections();await new Promise(resolve=>registryProxy.close(resolve));}
