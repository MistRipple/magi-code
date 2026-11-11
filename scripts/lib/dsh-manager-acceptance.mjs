import assert from 'node:assert/strict';
import { mkdir, writeFile, readFile, access, rename, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { connectTarget, pageTools, waitFor } from './electron-harness.mjs';
import { createPluginThemeCases, activatePluginTheme, measurePluginContrast } from './plugin-theme-acceptance.mjs';
import { authorizePluginFromNative, upgradePluginFromNative } from './plugin-native-acceptance.mjs';

// 专项与社区只运行这一份原包管理合同；状态、安装和授权均属于传入的 daemon。
export async function verifyDshManager({ request: api, origin, plugin, page, cdpPort, errors, gate, output, stateRoot, restart, appPid, clipAudit, check }) {
  assert.equal(plugin.manifest.version, '0.2.12', '从真实旧版开始验证管理器自身在线更新');
  const targetName = 'dsh-plugin-release-probe', clipAuditSource = clipAudit;
  const checks = {}, evidence = { package: 'dsh-plugin-mgr@0.2.13', initialVersion: plugin.manifest.version, initialDigest: plugin.digest, checks, completed: false };
  const report = { check(name, ok, detail) { check(name, ok, detail); checks[name] = true; } };
  let frame, ui = pageTools(page);
  const plugins = async () => (await api('/api/plugins')).plugins;
  const restartPage = async label => {
    frame?.close(); frame = undefined;
    page = await restart(label); ui = pageTools(page);
    await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '重启工作台初始化');
  };
  async function install(name,version) {
    const request={ecosystem:'dsh',package:name,version}; const preview=await api('/api/plugins/upstream/preview',request);
    const installed=await api('/api/plugins/upstream/install',{...request,expectedDigest:preview.candidate.digest});
    const plugin=installed.plugins.find((p)=>p.source.location===`dsh:${name}@${version}`);
    assert.ok(plugin); await api(`/api/plugins/${plugin.manifest.id}/enable`,{});
    await api(`/api/plugins/${plugin.manifest.id}/authorize`,{grants:plugin.manifest.permissions}); await api(`/api/plugins/${plugin.manifest.id}/activate`,{});
    await waitFor(async()=>{const p=(await plugins()).find(p=>p.manifest.id===plugin.manifest.id);if(p.runtime?.state==='failed')throw new Error(p.runtime.reason);return p.runtime?.state==='running';},'原包启动',120_000); return plugin;
  }
  async function authorizeFromNative(pluginId) {
    return authorizePluginFromNative({ request: api, page, plugin: (await plugins()).find(value => value.manifest.id === pluginId) });
  }
  try {
    let manager=plugin;
    report.check('Mgr 原包在线安装与必需 loader 启动成功',true);
    report.check('管理器保留安装与启停请求，不默认取得代授权能力',manager.manifest.permissions.some(p=>p.kind==='pluginManagement'&&p.targets.includes('install')&&p.targets.includes('lifecycle')&&!p.targets.includes('authorize')));
    // 旧发行版经真实管理路由调用 dsh 命令，核对公共命令边界；
    // 这不将旧版页面的 Button 缺陷计为已修复。
    const cliTarget=await install('dsh-plugin-release-probe','0.0.7');
    const cliUpdate=await api(`/api/plugins/${manager.manifest.id}/routes`,{
      method:'POST',path:'/api/plugin-manager/update',body:{name:'dsh-plugin-release-probe'},
    });
    const cliUpdated=await waitFor(async()=>{
      const plugin=(await plugins()).find(p=>p.manifest.id===cliTarget.manifest.id);
      return plugin?.manifest.version==='0.0.8'&&plugin;
    },'原包 dsh add 在线替换',180_000);
    report.check('旧版原包更新已提交新版但未代授，错误同时说明版本和原生恢复入口',
      !cliUpdate.body.ok&&!cliUpdated.active&&cliUpdated.grants.length===0&&JSON.stringify(cliUpdate.body).includes('已安装 dsh-plugin-release-probe@0.0.8')&&JSON.stringify(cliUpdate.body).includes('Magi 插件管理'),JSON.stringify(cliUpdate));
    await authorizeFromNative(cliTarget.manifest.id);
    report.check('用户从原生入口授权后，旧版管理原包安装的新版实际运行',true);
    const cliRemove=await api(`/api/plugins/${manager.manifest.id}/routes`,{
      method:'POST',path:'/api/plugin-manager/uninstall',body:{name:'dsh-plugin-release-probe'},
    });
    report.check('旧版原包业务通过公共 dsh remove 完成排空与移除',
      cliRemove.status===200&&cliRemove.body.ok&&!(await plugins()).some(p=>p.manifest.id===cliTarget.manifest.id),JSON.stringify(cliRemove));
    manager = await upgradePluginFromNative({ request: api, page, plugin: manager, version: '0.2.13',
      check: (_key, label, ok, detail) => report.check(label, ok, detail),
    });
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
    const themes=await createPluginThemeCases(api);
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
    const entry=join(stateRoot,'extensions/packages',targetId,target.digest,'extension/index.mjs');
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
    const beforeFailure=await current();gate.fail(targetName);
    await frame.evaluate(`[...document.querySelectorAll('[role=dialog] button')].find(button=>/^(更新|Update)$/.test(button.innerText.trim())).click();`);
    const failedFeedback=await waitFor(()=>frame.evaluate(`document.querySelector('.dshpm-toast-err')?.innerText.includes('原安装已恢复')&&document.querySelector('.dshpm-toast-err').innerText`),'原页面说明更新失败且恢复原安装',180_000);
    gate.release();
    const recovered=await waitFor(async()=>{const p=await current();return p?.runtime?.state==='running'&&p;},'恢复原版本真实运行',120_000);
    report.check('先移除后解析失败恢复原摘要、授权及启停意愿，原页面说明恢复结果',
      gate.rejected(targetName)>0&&recovered.digest===beforeFailure.digest&&recovered.enabled===beforeFailure.enabled&&recovered.active===beforeFailure.active
        &&JSON.stringify(recovered.grants)===JSON.stringify(beforeFailure.grants),JSON.stringify({failedFeedback,rejectedResolutions:gate.rejected(targetName)}));
    await frame.evaluate(`Promise.all(document.querySelector('.dshpm-toast-err').getAnimations().map(animation=>animation.finished.catch(()=>{})))`);
    const themeResults=[];
    for(const theme of themes.cases) {
      await activatePluginTheme(api,page,frame,theme.id);
      const contrast=await measurePluginContrast(frame,'.dshpm-toast-err');
      report.check(`${theme.id} 原包真实更新失败与恢复说明使用不透明状态底色且文字可读`,
        contrast.length===1&&contrast.every(item=>item.fill[3]===255&&item.foreground[3]===255&&item.ratio>=4.5),JSON.stringify(contrast));
      themeResults.push({id:theme.id,contrast});
      if(theme.material==='immersive') {
        await page.call('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false});
        const fits=await frame.evaluate(`(() => {const toast=document.querySelector('.dshpm-toast-err'),box=toast.getBoundingClientRect(),close=toast.querySelector('button').getBoundingClientRect();return box.left>=0&&box.right<=innerWidth&&box.top>=0&&box.bottom<=innerHeight&&close.left>=0&&close.right<=innerWidth&&document.documentElement.scrollWidth<=innerWidth;})()`);
        report.check(`${theme.id} 390px 长错误说明与关闭操作可达`,fits);
        await writeFile(join(output,`manager-error-${theme.mode}-390.png`),Buffer.from(await ui.screenshot(),'base64'));
        if(clipAuditSource) {
          const audit=await frame.evaluate(`(${clipAuditSource})({viewport:true})`);
          await writeFile(join(output,`manager-error-${theme.mode}-clip.json`),JSON.stringify(audit,null,2));
          report.check(`${theme.id} 原包错误浮层阴影无裁切`,audit.cut===0,JSON.stringify(audit));
        }
        await page.call('Emulation.clearDeviceMetricsOverride');
      }
    }
    await writeFile(join(output,'manager-error-theme-contrast.json'),JSON.stringify(themeResults,null,2));
    await activatePluginTheme(api,page,frame,themes.original);
    await writeFile(join(output,'manager-update-failed-restored.png'),Buffer.from(await ui.screenshot(),'base64'));
    const detailEvidence=[];
    for(const width of [1280,390]) {
      await page.call('Emulation.setDeviceMetricsOverride',{width,height:844,deviceScaleFactor:1,mobile:false});
      await frame.evaluate(`document.querySelector('.dshpm-toast-text').click()`);
      const detail=await waitFor(()=>frame.evaluate(`(() => {
        const toast=document.querySelector('.dshpm-toast-err'),text=toast.querySelector('.dshpm-toast-text');
        const box=toast.getBoundingClientRect(),textBox=text.getBoundingClientRect();
        if(text.classList.contains('is-clamped')||textBox.bottom>box.bottom||toast.scrollHeight>toast.clientHeight)return false;
        return {viewport:{width:innerWidth,height:innerHeight},box:box.toJSON(),textBox:textBox.toJSON(),text:text.innerText};
      })()`),`${width}px 原包展开真实错误后全文处于可见区域`);
      detailEvidence.push(detail);
      report.check(`${width}px 原包展开后完整显示真实失败原因与恢复说明`,detail.text.includes('原安装已恢复'));
      await writeFile(join(output,`manager-error-expanded-${width}.png`),Buffer.from(await ui.screenshot(),'base64'));
      if(clipAuditSource) {
        const audit=await frame.evaluate(`(${clipAuditSource})({viewport:true})`);
        await writeFile(join(output,`manager-error-expanded-${width}-clip.json`),JSON.stringify(audit,null,2));
        report.check(`${width}px 原包展开错误浮层阴影无裁切`,audit.cut===0,JSON.stringify(audit));
      }
      await frame.evaluate(`document.querySelector('.dshpm-toast-text').click()`);
      report.check(`${width}px 原包全文收起恢复折叠`,await frame.evaluate(`document.querySelector('.dshpm-toast-text').classList.contains('is-clamped')`));
    }
    await writeFile(join(output,'manager-error-expanded.json'),JSON.stringify(detailEvidence,null,2));
    await page.call('Page.bringToFront');
    // 响应式视口切换后先真实点击 toast 内边距，把输入焦点交回隔离文档；
    // element.focus() 单独执行只改变该文档 activeElement，不代表物理焦点。
    const focusPoint=await frame.evaluate(`(() => {const r=document.querySelector('.dshpm-toast-err').getBoundingClientRect();return {x:r.x+3,y:r.y+3};})()`);
    await frame.call('Input.dispatchMouseEvent',{type:'mousePressed',button:'left',clickCount:1,...focusPoint});
    await frame.call('Input.dispatchMouseEvent',{type:'mouseReleased',button:'left',clickCount:1,...focusPoint});
    const keyboardFocus=await frame.evaluate(`(() => {const button=document.querySelector('.dshpm-toast-close');button.focus();return {focused:document.activeElement===button,documentFocused:document.hasFocus()};})()`);
    await writeFile(join(output,'manager-error-keyboard-focus.json'),JSON.stringify(keyboardFocus,null,2));
    assert.ok(keyboardFocus.focused&&keyboardFocus.documentFocused,'真实键盘输入前须确认原包关闭按钮取得物理文档焦点');
    // Chromium 的 Enter 原生激活依赖字符事件；由主页面向当前聚焦 iframe
    // 派发完整按键，不能把只有 keyDown 的协议消息当成实际 Enter。
    await page.call('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter',windowsVirtualKeyCode:13,text:'\r',unmodifiedText:'\r'});
    await page.call('Input.dispatchKeyEvent',{type:'keyUp',key:'Enter',code:'Enter',windowsVirtualKeyCode:13});
    await waitFor(()=>frame.evaluate(`!document.querySelector('.dshpm-toast-err')`),'键盘关闭原包错误提示');
    report.check('原包关闭按钮接受真实键盘激活，关闭提示不改变已恢复安装',
      (await current()).digest===beforeFailure.digest&&(await current()).runtime.state==='running');
    await page.call('Emulation.clearDeviceMetricsOverride');
    gate.pause(targetName);
    const interrupted=api(`/api/plugins/${manager.manifest.id}/routes`,{method:'POST',path:'/api/plugin-manager/update',body:{name:'dsh-plugin-release-probe'}})
      .then(value=>({value}),error=>({error:String(error)}));
    await waitFor(()=>gate.held.size>0,'旧版已移除后的候选解析等待',30_000);
    const checkpoint=JSON.parse(await readFile(join(stateRoot,'plugins/state.json'),'utf8'));
    report.check('移除后的原安装与归档引用持久保存在 daemon 唯一安装状态',
      !checkpoint.plugins[targetId]&&checkpoint.removals[targetId]?.installed.digest===beforeFailure.digest);
    await writeFile(join(output,'state-before-interruption.json'),JSON.stringify(checkpoint,null,2));
    const {stdout}=await promisify(execFile)('ps',['-axo','pid=,ppid=,command=']);
    const processes=stdout.split('\n').map(line=>line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/)).filter(Boolean);
    const daemon=processes.find(([,pid,parent,command])=>Number(parent)===appPid()&&command.includes('/daemon/magi-daemon'));
    assert.ok(daemon,'仅终止本次验收应用的受控 daemon');
    const resolver=processes.find(([,pid,parent,command])=>Number(parent)===Number(daemon[1])&&command.includes('/compat/cli.mjs resolve dsh-plugin-release-probe'));
    assert.ok(resolver,'核对本次 daemon 正在运行的真实翻译进程');
    frame.close();frame=undefined;await page.call('Page.navigate',{url:'about:blank'});page.close();page=undefined;
    process.kill(Number(daemon[1]),'SIGKILL');
    await waitFor(()=>fetch(origin+'/health').then(()=>false,()=>true),'中断 daemon 已停止监听');
    await waitFor(()=>{try{process.kill(Number(resolver[1]),0);return false;}catch(error){if(error.code==='ESRCH')return true;throw error;}},'daemon 崩溃后所有者管道结束真实翻译进程',30_000);
    report.check('daemon 断进程后联网中的翻译工具链退出，旧解析连接不再重试',true);
    await interrupted;
    gate.release();
    const resolutionsBeforeRestart=gate.requests(targetName);
    await restartPage('manager-interrupted.log');
    const restored=await waitFor(async()=>{const p=await current();return p?.runtime?.state==='running'&&p;},'断进程后原安装自动恢复运行',120_000);
    report.check('候选提交前 daemon 断进程后恢复原版本及授权，不重新下载或重放安装',
      restored.digest===beforeFailure.digest&&restored.enabled===beforeFailure.enabled&&restored.active===beforeFailure.active
        &&JSON.stringify(restored.grants)===JSON.stringify(beforeFailure.grants)&&gate.requests(targetName)===resolutionsBeforeRestart,
      JSON.stringify({restored,beforeFailure,targetResolutions:gate.requests(targetName),resolutionsBeforeRestart}));
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
    report.check('原包更新提交新版后保留待授权事实，没有继承旧版本授权',!after.active&&after.grants.length===0);
    const updateFeedback=await waitFor(async()=>{const feedback=await frame.evaluate(`Array.from(document.querySelectorAll('.dshpm-toast')).map(node=>({text:node.innerText,error:node.classList.contains('dshpm-toast-err')}))`);return feedback.length&&feedback;},'原包完成更新并显示业务回执',180_000);
    await writeFile(join(output,'update-feedback.json'),JSON.stringify(updateFeedback,null,2));
    report.check('原包更新回执说明已经安装的实际新版、待授权原因和原生恢复操作',updateFeedback.some(item=>item.error&&item.text.includes('已安装 dsh-plugin-release-probe@0.0.8')&&item.text.includes('授权')&&item.text.includes('Magi 插件管理')&&item.text.includes('无需重复安装')),JSON.stringify(updateFeedback));
    await frame.evaluate(`Promise.all(document.querySelector('.dshpm-toast-err').getAnimations().map(animation=>animation.finished.catch(()=>{})))`);
    await waitFor(()=>frame.evaluate(`getComputedStyle(document.querySelector('.dshpm-toast-err')).opacity==='1'`),'原包待授权反馈入场结束，不在淡入中截取验收截图');
    const authorizationContrast=await measurePluginContrast(frame,'.dshpm-toast-err');
    report.check('原包更新后的待授权反馈使用不透明状态底色且文字可读',
      authorizationContrast.length>0&&authorizationContrast.every(item=>item.fill[3]===255&&item.ratio>=4.5),JSON.stringify(authorizationContrast));
    // 原包错误分支保留上次加载的列表。沿原“刷新”操作读取当前目录，
    // 核对安装版本字段，不能误把旁边的 registry 最新版本当成已安装版本。
    await frame.evaluate(`[...document.querySelectorAll('button')].find(b=>/^(刷新|Refresh)$/.test(b.innerText.trim())&&!b.disabled).click()`);
    await waitFor(()=>frame.evaluate(`(()=>{const c=[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe');return c?.querySelector('.dshpm-field-val')?.textContent==='v0.0.8';})()`),'原页面刷新后安装版本字段显示真实新版');
    report.check('原包显式刷新读取实际已安装版本，不将最新版本字段冒充安装结果',true);
    await writeFile(join(output,'manager-update-authorization-required.png'),Buffer.from(await ui.screenshot(),'base64'));
    await page.call('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false});
    await frame.evaluate(`document.querySelector('.dshpm-toast-text').click()`);
    await waitFor(()=>frame.evaluate(`(()=>{const t=document.querySelector('.dshpm-toast-err'),text=t.querySelector('.dshpm-toast-text'),box=t.getBoundingClientRect();return !text.classList.contains('is-clamped')&&box.left>=0&&box.right<=innerWidth&&box.top>=0&&box.bottom<=innerHeight&&t.scrollHeight<=t.clientHeight&&document.documentElement.scrollWidth<=innerWidth;})()`),'390px 待授权原因和恢复说明全文可读');
    report.check('390px 原包可展开已安装版本、授权原因和恢复说明',true);
    await writeFile(join(output,'manager-update-authorization-required-390.png'),Buffer.from(await ui.screenshot(),'base64'));
    if(clipAuditSource) {
      const audit=await frame.evaluate(`(${clipAuditSource})({viewport:true})`);
      await writeFile(join(output,'manager-update-authorization-required-clip.json'),JSON.stringify(audit,null,2));
      report.check('待授权反馈展开后阴影无裁切',audit.cut===0,JSON.stringify(audit));
    }
    await page.call('Emulation.clearDeviceMetricsOverride');
    frame.close();frame=undefined;
    const resolutionsBeforeAuthorization=gate.requests(targetName);
    await authorizeFromNative(targetId);
    report.check('用户从 Magi 原生入口授权已提交新版，无需重复下载安装即可恢复运行',(await current()).digest===after.digest&&gate.requests(targetName)===resolutionsBeforeAuthorization);
    await ui.click(nav);
    const authorizedUrl=await waitFor(()=>page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`),'授权后原包管理页面');
    frame=await waitFor(()=>connectTarget(cdpPort,t=>['iframe','page'].includes(t.type)&&t.url===authorizedUrl,'授权恢复后的 Mgr 页面',errors),'授权后原包文档');
    await waitFor(()=>frame.evaluate(`Boolean([...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe'))`),'原包目录读取已授权新版');
    await frame.evaluate(`(()=>{const c=[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe');if(!c.classList.contains('is-open'))c.querySelector('.dshpm-row').click();})()`);
    report.check('原生授权后原包重新读取的安装版本与 daemon 一致',await frame.evaluate(`(()=>{const c=[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe');return c?.querySelector('.dshpm-field-val')?.textContent==='v0.0.8';})()`));
    await writeFile(join(output,'manager-update-authorized.png'),Buffer.from(await ui.screenshot(),'base64'));
    await frame.evaluate(`(() => {const card=[...document.querySelectorAll('.dshpm-card')].find(el=>el.querySelector('.dshpm-name')?.textContent==='dsh-plugin-release-probe');card.querySelector('.dshpm-btn-danger').click();})()`);
    await waitFor(()=>frame.evaluate(`Boolean(document.querySelector('[role=dialog]'))`),'原包卸载确认');
    report.check('卸载确认包含真实包名和已更新版本',await frame.evaluate(`document.querySelector('[role=dialog]').innerText.includes('dsh-plugin-release-probe') && document.querySelector('[role=dialog]').innerText.includes('0.0.8')`));
    await writeFile(join(output,'manager-uninstall-confirm.png'),Buffer.from(await ui.screenshot(),'base64'));
    await frame.evaluate(`[...document.querySelectorAll('[role=dialog] button')].find(button=>/^(取消|Cancel)$/.test(button.innerText.trim())).click();`);
    await waitFor(()=>frame.evaluate(`!document.querySelector('[role=dialog]')`),'取消原包卸载');
    report.check('取消原包卸载保留更新后真实版本与运行',(await current()).digest===after.digest&&(await current()).runtime.state==='running');
    await restartPage('manager-before-restart.log');
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
    const location=join(stateRoot,'extensions/data',manager.manifest.id,'profiles/magi');
    report.check('profile 仅为调用投影，未落盘第二份安装注册表',!await access(location).then(()=>true,()=>false));
    report.check('真实 Electron 与原包页面无未捕获异常',errors.length===0,errors.join('\n'));
    evidence.completed = true;
    evidence.updatedVersion = restarted.manifest.version;
    evidence.updatedDigest = restarted.digest;
    return { page, plugin: restarted, checks, evidence };
  } catch (error) {
    if (frame) {
      const diagnostics = await frame.evaluate(`(() => {
        const ui=window.__MAGI_DSH_MODULES__?.['@deepseek-ai/dsh-client-ui-primitives'];
        return {moduleLoaded:Boolean(ui),exports:Object.fromEntries(['Modal','Button'].map(name=>[name,{type:typeof ui?.[name],reactType:String(ui?.[name]?.$$typeof ?? '')}])),dialogVisible:Boolean(document.querySelector('[role=dialog]'))};
      })()`).catch(() => null);
      await writeFile(join(output, 'ui-modules.json'), JSON.stringify(diagnostics, null, 2));
    }
    await writeFile(join(output, 'manager-failure.png'), Buffer.from(await ui.screenshot(), 'base64')).catch(() => {});
    throw error;
  } finally {
    frame?.close(); gate.release();
    await writeFile(join(output, 'manager-business.json'), JSON.stringify(evidence, null, 2));
  }
}
