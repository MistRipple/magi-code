// 目录发行包的真实 daemon 断进程验收。私有 registry 仅提供测试原包；
// 下载、integrity、翻译、安装、授权、Node 启动和回执均使用产品入口。
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { packPlugin } from './compat/core/pack.mjs';
import { launchApp, connectAppPage, pageTools, createReporter, waitFor } from './lib/electron-harness.mjs';

const root=dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot=process.env.MAGI_PLUGIN_TEST_APP||join(root,'target/electron-dist/mac-arm64/Magi.app');
const port=38953,cdpPort=9323,origin=`http://127.0.0.1:${port}`;
const work=await mkdtemp(join(tmpdir(),'magi-install-recovery-'));
const output=process.env.MAGI_INSTALL_RECOVERY_OUTPUT||join(root,'target/plugin-acceptance/install-recovery');
const run=promisify(execFile),report=createReporter(),errors=[],logs=[];
const targetName='magi-install-recovery-target';
let app,page,ui,registry,registryUrl,archive,holdResolution=false,holdStartup=false,resolutions=0;
const heldResolutions=new Set(),heldStarts=new Set();
await mkdir(output,{recursive:true});
const journal=async()=>JSON.parse(await readFile(join(work,'state/plugins/installations.json'),'utf8'));
const records=async()=>Object.values((await journal()).records);
async function api(path,body){
  const response=await fetch(origin+path,{method:body===undefined?'GET':'POST',headers:{origin,'content-type':'application/json'},
    ...(body===undefined?{}:{body:JSON.stringify(body)}),signal:AbortSignal.timeout(180000)});
  const value=await response.json();assert.equal(response.status,200,JSON.stringify(value));return value;
}
const plugins=async()=>(await api('/api/plugins')).plugins;
const target=async()=>(await plugins()).find(plugin=>plugin.source.location===`dsh:${targetName}@1.0.0`);
const invoke=input=>api('/api/plugins/acme.install-recovery/routes',{method:'POST',path:'/install',body:input});
const input=requestId=>({requestId,ecosystem:'dsh',package:targetName,version:'1.0.0',enabled:true});
async function start(){
  app=launchApp({executable:join(appRoot,'Contents/MacOS/Magi'),cwd:root,cdpPort,env:{MAGI_STATE_ROOT:join(work,'state'),MAGI_DESKTOP_DAEMON_PORT:String(port),
    MAGI_NPM_REGISTRY:registryUrl,MAGI_INSTALL_RECOVERY_GATE:registryUrl+'/startup',MAGI_EXTENSION_NODE:'',MAGI_EXTENSION_COMPAT:'',MAGI_OPEN_BROWSER:'0'}});
  logs.push(app.log);
  await waitFor(()=>fetch(origin+'/health').then(r=>r.ok),'daemon 启动',60000);
  page=await waitFor(()=>connectAppPage(cdpPort,port,errors),'打包 Electron 工作台',60000);ui=pageTools(page);
  await waitFor(()=>ui.exists('[data-testid=sidebar-settings]'),'工作台初始化');
}
async function crash(){
  const {stdout}=await run('ps',['-axo','pid=,ppid=,command=']);
  const children=stdout.split('\n').map(line=>line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/)).filter(Boolean);
  const daemon=children.find(([,pid,parent,command])=>Number(parent)===app.child.pid&&command.includes('/daemon/magi-daemon'));
  assert.ok(daemon,'只允许终止当前验收 Electron 托管的 daemon');
  await writeFile(join(output,`journal-before-crash-${(await records()).length}.json`),JSON.stringify(await journal(),null,2));
  await page.call('Page.navigate',{url:'about:blank'});page.close();page=null;
  process.kill(Number(daemon[1]),'SIGKILL');
  await waitFor(async()=>!await fetch(origin+'/health').then(()=>true,()=>false),'被杀 daemon 停止监听');
  await app.stop();app=null;
}
async function ready(){
  await waitFor(async()=>(await plugins()).find(p=>p.manifest.id==='acme.install-recovery')?.runtime?.state==='running','管理插件重建宿主',60000);
}
async function prepare(){
  const upstream=join(work,'upstream/package');await mkdir(upstream,{recursive:true});
  await writeFile(join(upstream,'package.json'),JSON.stringify({name:targetName,version:'1.0.0',type:'module',main:'index.js',dsh:{bundle:{}},
    peerDependencies:{'@deepseek-ai/cordis':'^4.0.1'}}));
  await writeFile(join(upstream,'index.js'),`export const name='install-recovery-target';export const inject=['commands'];
export async function apply(ctx){ctx.commands.register({name:'report',handler:()=>({kind:'success',text:'TARGET_READY'})});
if(process.env.MAGI_EXT_ENDPOINT) await fetch(process.env.MAGI_INSTALL_RECOVERY_GATE);}`);
  const tar=join(work,'target.tgz');await run('tar',['--format=ustar','-czf',tar,'-C',join(work,'upstream'),'package']);archive=await readFile(tar);
  registry=createServer(async(request,response)=>{
    if(request.url==='/startup'){
      if(holdStartup){heldStarts.add(response);response.once('close',()=>heldStarts.delete(response));return;}
      response.end('ready');return;
    }
    if(request.url==='/target.tgz'){response.writeHead(200,{'content-type':'application/octet-stream'});response.end(archive);return;}
    if(request.url?.split('?')[0]===`/${targetName}`){
      resolutions++;
      if(holdResolution){heldResolutions.add(response);response.once('close',()=>heldResolutions.delete(response));return;}
      const document={name:targetName,version:'1.0.0',dist:{tarball:registryUrl+'/target.tgz',integrity:'sha512-'+createHash('sha512').update(archive).digest('base64')}};
      response.writeHead(200,{'content-type':'application/json'});response.end(JSON.stringify({name:targetName,'dist-tags':{latest:'1.0.0'},versions:{'1.0.0':document}}));return;
    }
    const remote=await fetch('https://registry.npmjs.org'+request.url);
    response.writeHead(remote.status,{'content-type':remote.headers.get('content-type')||'application/json'});response.end(Buffer.from(await remote.arrayBuffer()));
  });
  await new Promise(resolve=>registry.listen(0,'127.0.0.1',resolve));registryUrl=`http://127.0.0.1:${registry.address().port}`;
  const dir=join(work,'manager');await mkdir(join(dir,'extension'),{recursive:true});
  await mkdir(join(dir,'ui'));await writeFile(join(dir,'ui/index.html'),'<!doctype html><title>Installation recovery</title>');
  await writeFile(join(dir,'manifest.json'),JSON.stringify({sdkVersion:2,id:'acme.install-recovery',version:'1.0.0',name:'Install recovery',description:'',dataSchemaVersion:1,
    extension:{hooks:[]},permissions:[{kind:'extension',targets:['full-trust']},{kind:'pluginManagement',targets:['background','install','lifecycle','authorize']}],
    settingsSchema:{type:'object'},contributions:{commands:[{id:'status',title:'Status',description:''}],views:[{id:'settings',title:'Install recovery',entry:'ui/index.html',placements:['settings']}]}}));
  await writeFile(join(dir,'extension/index.mjs'),`export default async api=>{await api.route('/install',async request=>({status:200,body:await api.plugins.background.installBundle(request.body)}));};`);
  const zip=packPlugin(dir,join(work,'manager.zip')).target;
  const request={source:'local:manager.zip',archiveBase64:(await readFile(zip)).toString('base64')};
  await start();const preview=await api('/api/plugins/preview',request);await api('/api/plugins/install',{...request,expectedDigest:preview.digest});
  const manager=(await plugins()).find(p=>p.manifest.id==='acme.install-recovery');
  await api('/api/plugins/acme.install-recovery/enable',{});
await api('/api/plugins/acme.install-recovery/enable',{});await ready();
}
try{
  assert.ok(!await fetch(origin+'/health').then(()=>true,()=>false));await prepare();
  report.check('目录包完成真实管理插件安装和授权',true);
  holdResolution=true;
  let preparationResult;
  const preparing=invoke(input('crash-prepare')).then(result=>{preparationResult=result;return result;},error=>{preparationResult={error:String(error)};return null;});
  await waitFor(()=>{if(preparationResult)throw new Error(JSON.stringify(preparationResult));return heldResolutions.size>0;},'真实 registry 解析正在等待');
  report.check('下载前持久保存原始请求身份和 preparing 阶段',(await records()).some(r=>r.request_id==='crash-prepare'&&r.phase==='preparing'&&!r.result));
  await crash();await preparing;
  holdResolution=false;for(const response of heldResolutions)response.destroy();
  await start();await ready();
  const before=resolutions,recovered=(await invoke(input('crash-prepare'))).body;
  report.check('准备中断后同一请求跨宿主代次恢复明确失败，不重新解析或安装',recovered.application==='failed'&&recovered.error.code==='installation-interrupted'&&recovered.installed!==true&&resolutions===before&&!await target(),JSON.stringify(recovered));
  holdStartup=true;
  const applying=invoke(input('crash-committed')).catch(()=>null);
  await waitFor(()=>heldStarts.size>0,'候选已安装，真实 Node 启动等待就绪',120000);
  const installed=await target();
  report.check('版本提交后就绪前保留 applying 与候选摘要',(await records()).some(r=>r.request_id==='crash-committed'&&r.phase==='applying'&&r.candidate.digest===installed.digest)
    && installed.runtime.state!=='running');
  const digest=installed.digest;
  await crash();await applying;
  holdStartup=false;for(const response of heldStarts)response.destroy();
  await start();await ready();
  const count=resolutions,receipt=(await invoke(input('crash-committed'))).body;
  report.check('提交后断进程不把未知结算冒充成功，按原安装摘要返回已安装事实',receipt.application==='failed'&&receipt.error.code==='installation-interrupted'&&receipt.installed===true&&receipt.stage==='enable'
    && receipt.changed==null&&resolutions===count&&(await target()).digest===digest,JSON.stringify(receipt));
  await waitFor(async()=>(await target()).runtime?.state==='running','原激活意愿由监管器恢复运行',60000);
  const retry=(await invoke(input('explicit-retry'))).body;
  report.check('显式新请求重新核对原包并收敛成功，版本不被重复替换',retry.application==='applied'&&retry.changed===false&&(await target()).digest===digest,JSON.stringify(retry));
  await crash();await start();await ready();
  const completedCount=resolutions,completed=(await invoke(input('explicit-retry'))).body;
  report.check('已完成回执在应用重启后原样返回，重发不下载、不重装、不另起安装任务',JSON.stringify(completed)===JSON.stringify(retry)&&resolutions===completedCount
    && (await records()).filter(r=>r.request_id==='explicit-retry').length===1,JSON.stringify(completed));
  report.check('安装日志保留发起插件身份，版本仍取原安装事实源',(await records()).every(record=>record.plugin==='acme.install-recovery')&&(await target()).digest===digest);
  await writeFile(join(output,'installations.json'),JSON.stringify(await journal(),null,2));
  await writeFile(join(output,'plugin.json'),JSON.stringify(await target(),null,2));
  const targetPlugin=await target();
  const command=targetPlugin.manifest.contributions.commands.find(command=>command.title==='report');
  const session=(await api('/api/session/materialize',{scope:'personal'})).sessionId;
  const accepted=await api('/api/session/turn',{scope:'personal',sessionId:session,pluginCommandId:`plugin/${targetPlugin.manifest.id}/${command.id}`,
    text:'',requestId:'recovered-target-command',userMessageId:'recovered-target-command-user'});
  const result=await waitFor(async()=>(await api(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns?.find(turn=>turn.turnId===accepted.turnId&&['completed','failed'].includes(turn.status)),'恢复后的真实原包业务命令');
  report.check('安装恢复后的原包业务仍由原生 Task 执行并返回真实结果',result.status==='completed'&&result.items.some(item=>item.content==='TARGET_READY'));
  await page.call('Page.navigate',{url:`${origin}/web.html?scope=personal&sessionId=${session}`});
  await waitFor(()=>page.evaluate('document.body.innerText.includes("TARGET_READY")'),'恢复后的业务结果展示');
  await writeFile(join(output,'workbench.png'),Buffer.from(await ui.screenshot(),'base64'));
  report.check('真实工作台无未捕获异常',errors.length===0,errors.join('\n'));
  console.log(report.summary());
}catch(error){console.error(error);report.results.push({name:'安装崩溃恢复验收',ok:false,detail:error.stack||String(error)});process.exitCode=1;}
finally{
  await writeFile(join(output,'results.json'),JSON.stringify({checks:report.results,errors,stateRoot:join(work,'state')},null,2));
  if(page){await page.call('Page.navigate',{url:'about:blank'}).catch(()=>{});page.close();}
  if(app)await app.stop();
  for(const response of [...heldStarts,...heldResolutions])response.destroy();
  registry?.closeAllConnections();registry?.close();
  await writeFile(join(output,'electron.log'),logs.flat().join(''));
}
