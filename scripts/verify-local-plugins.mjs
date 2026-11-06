// 真实 daemon + Node 纵向回归；只调用公开 API，使用独立临时状态与本地模型。
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { execFile, spawn } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { setTimeout as sleep } from 'node:timers/promises';
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const work = await mkdtemp(join(tmpdir(), 'magi-local-plugins-'));
const port = Number(process.env.MAGI_PLUGIN_TEST_PORT || 38123);
const origin = `http://127.0.0.1:${port}`;
const run = promisify(execFile);
const requests = [];
const authorizations = [];
let daemon, log = '', checks = 0;
function check(name, value) { assert.ok(value, name); checks++; console.log(`✓ ${name}`); }
async function wait(probe, label, timeout = 30000) {
  const end = Date.now() + timeout;
  let error;
  while (Date.now() < end) {
    try { const value = await probe(); if (value) return value; } catch (e) { error = e; }
    if (daemon?.exitCode != null) throw new Error(`daemon 退出 ${daemon.exitCode}\n${log.slice(-4000)}`);
    await sleep(100);
  }
  throw new Error(`等待超时：${label}; ${error?.message || ''}\n${log.slice(-4000)}`);
}
async function api(path, body, method = body === undefined ? 'GET' : 'POST') {
  const r = await fetch(`${origin}${path}`, {method, headers:{origin, 'content-type':'application/json'}, ...(body === undefined ? {} : {body:JSON.stringify(body)})});
  const text = await r.text(); let value; try { value = JSON.parse(text); } catch { value = text; }
  return {status:r.status, value};
}
async function post(path, body = {}, method = 'POST') {
  const r = await api(path, body, method); assert.equal(r.status, 200, `${path}: ${JSON.stringify(r.value)}`); return r.value;
}
const provider = createServer(async (req, res) => {
  if (req.url === '/health') return void res.end('ok');
  let bytes = ''; for await (const chunk of req) bytes += chunk;
  if (req.url !== '/v1/chat/completions') return void res.writeHead(404).end();
  const body = JSON.parse(bytes); requests.push(body); authorizations.push({model: body.model, authorization: req.headers.authorization});
  if (body.stream) {
    const chunk = (delta, finish_reason = null, extra = {}) => `data: ${JSON.stringify({id:'local', object:'chat.completion.chunk', choices:[{index:0, delta, finish_reason}], ...extra})}\n\n`;
    return void res.writeHead(200, {'content-type':'text/event-stream'}).end(chunk({role:'assistant', content:'LOCAL_STREAM_OK'}) + chunk({}, 'stop') + chunk({}, null, {choices:[], usage:{prompt_tokens:10, completion_tokens:3, total_tokens:13}}) + 'data: [DONE]\n\n');
  }
  res.writeHead(200, {'content-type':'application/json'}).end(JSON.stringify({id:'local', object:'chat.completion', choices:[{index:0, message:{role:'assistant', content:'LOCAL_MODEL_OK'}, finish_reason:'stop'}], usage:{prompt_tokens:10, completion_tokens:3, total_tokens:13}}));
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
const providerOrigin = `http://127.0.0.1:${provider.address().port}`;
const installed = async () => (await api('/api/plugins')).value.plugins.find(p => p.manifest.id === 'test.local');
const history = async session => (await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).value.canonicalTurns;
let serial = 0;
async function command(session, id, text = '') {
  return post('/api/session/turn', {scope:'personal', sessionId:session, pluginCommandId:`plugin/test.local/${id}`, text, requestId:`local-${++serial}`, userMessageId:`local-user-${serial}`});
}
async function settled(session, turn) {
  return wait(async () => (await history(session))?.find(t => t.turnId === turn && ['completed','failed','cancelled','blocked'].includes(t.status)), `Turn ${turn}`);
}
async function pack(version, dataSchemaVersion) {
  const directory = join(work, version); await mkdir(join(directory, 'extension'), {recursive:true});
  const manifest = {sdkVersion:2, id:'test.local', version, name:'本地验收插件', description:'公开接口纵向验收', icon:'ui/icon.png', permissions:[{kind:'extension',targets:['full-trust']},{kind:'storage',targets:['notes','__settings']},{kind:'models',targets:['default']}], extension:{hooks:[{name:'input',mode:'guard'}]}, dataSchemaVersion, settingsSchema:{type:'object',properties:{label:{type:'string'}},additionalProperties:false}, contributions:{resources:[{id:'notes',title:'笔记',description:''}],views:[{id:'page',title:'设置',entry:'ui/page.html',placements:['settings']}],commands:['probe','status','wait','ask','taskless'].map(id=>({id,title:id,description:''}))}};
  await writeFile(join(directory,'manifest.json'),JSON.stringify(manifest));
  await mkdir(join(directory,'ui'),{recursive:true});
  // 1x1 PNG：只为验证图标在列表、详情和设置导航中显示。
  await writeFile(join(directory,'ui/icon.png'),Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==','base64'));
  await writeFile(join(directory,'ui/page.html'),'<!doctype html><meta charset="utf-8"><p>local plugin page</p>');
  await writeFile(join(directory,'extension/index.mjs'), `
import {execFile} from 'node:child_process'; import {promisify} from 'node:util';
import {mkdtemp,writeFile,readFile,rm} from 'node:fs/promises'; import {tmpdir} from 'node:os'; import {join} from 'node:path';
export default async api => {
  await api.on('input', input=> input.raw?.text?.includes('LOCAL_BLOCK') ? {decision:'block',reason:'LOCAL_GUARD_OK'} : {decision:'continue'});
  await api.command('status', async()=> 'LOCAL_STATUS:'+JSON.stringify(await api.resource.read('notes')));
  await api.command('wait', (_,ctx)=>new Promise(resolve=>ctx.signal.addEventListener('abort',()=>resolve('cancelled'),{once:true})));
  await api.command('ask', async()=> 'LOCAL_ANSWER:'+JSON.stringify(await api.ask.user({questions:[{header:'验收',question:'选择结果',options:[{label:'A'},{label:'B'}]}]})));
  await api.command('taskless', async()=> {
    for (const call of [()=>api.tools.call('tool_catalog',{}),()=>api.task.spawn({})]) {
      try { await call(); throw Error('Taskless action accepted'); }
      catch (error) { if(!String(error.message).includes('需要活动 Task')) throw error; }
    }
    return 'LOCAL_TASKLESS_DENIED';
  });
  await api.command('probe', async(_,ctx)=> {
    const current=await api.resource.read('notes'); await api.resource.write('notes',current.version,{runs:(current.value?.runs||0)+1});
    if((await api.settings.read()).value?.label!=='configured') throw Error('settings mismatch');
    const ref=await api.credential.ref(); const model=await api.model.invoke({prompt:'Local check',credentialRef:ref.ref,maxOutputTokens:64});
    if(model.content!=='LOCAL_MODEL_OK') throw Error('model mismatch:'+JSON.stringify(model));
    const snapshot=await api.session.snapshot();
    if(snapshot.session.sessionId!==ctx.owner.sessionId) throw Error('snapshot owner:'+JSON.stringify(snapshot.session));
    const blob=await api.blob.put({hello:'world'}); if((await api.blob.get(blob.ref)).value.hello!=='world') throw Error('blob mismatch');
    await api.notify.user({message:'LOCAL_NOTIFICATION_OK'});
    const d=await mkdtemp(join(tmpdir(),'magi-plugin-direct-')); try {await writeFile(join(d,'note'),'direct-file'); if(await readFile(join(d,'note'),'utf8')!=='direct-file') throw Error('file mismatch');} finally {await rm(d,{recursive:true,force:true});}
    const child=await promisify(execFile)(process.execPath,['-e','process.stdout.write("direct-process")']); if(child.stdout!=='direct-process') throw Error('process mismatch');
    if(await (await fetch(${JSON.stringify(`${providerOrigin}/health`)})).text()!=='ok') throw Error('network mismatch');
    return 'LOCAL_PROBE_OK';
  });
}`);
  const archive=join(work,`${version}.zip`); await run(process.execPath,[join(root,'scripts/pack-plugin.mjs'),directory,archive]);
  return {source:'local:test.zip',archiveBase64:(await readFile(archive)).toString('base64')};
}
// 只提供模型数据源的插件：把本地 provider 作为“第三方反代”贡献给主对话目录。
async function packProxy() {
  const directory = join(work, 'proxy'); await mkdir(join(directory, 'extension'), {recursive:true});
  const manifest = {sdkVersion:2, id:'test.proxy', version:'1.0.0', name:'本地反代插件', description:'模型数据源纵向验收', permissions:[{kind:'extension',targets:['full-trust']},{kind:'modelSources',targets:['gateway']}], extension:{hooks:[]}, dataSchemaVersion:1, settingsSchema:{type:'object',additionalProperties:false}, contributions:{modelSources:[{id:'gateway',title:'本地反代',description:''}]}};
  await writeFile(join(directory,'manifest.json'),JSON.stringify(manifest));
  await writeFile(join(directory,'extension/index.mjs'), `
export default async api => {
  await api.modelSource.set('gateway', {baseUrl:${JSON.stringify(`${providerOrigin}/v1`)}, apiKey:'plugin-key', apiProtocol:'openai_chat', models:[{id:'proxy-model', displayName:'Proxy Model'}]});
}`);
  const archive=join(work,'proxy.zip'); await run(process.execPath,[join(root,'scripts/pack-plugin.mjs'),directory,archive]);
  return {source:'local:proxy.zip',archiveBase64:(await readFile(archive)).toString('base64')};
}
async function verifyUi(session) {
  const electronRoot = join(root, 'node_modules/electron');
  const executable = join(electronRoot, 'dist', (await readFile(join(electronRoot, 'path.txt'), 'utf8')).trim());
  const output = join(root, 'target/plugin-acceptance');
  await mkdir(output, {recursive:true});
  const result = await run(executable, ['--headless', '--disable-gpu', '--use-mock-keychain', join(root, 'scripts/lib/plugin-ui-acceptance.cjs')], {cwd:root, timeout:90000, maxBuffer:1024*1024, env:{...process.env, MAGI_PLUGIN_UI_URL:`${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}`,MAGI_PLUGIN_UI_SESSION:session,MAGI_PLUGIN_UI_OUTPUT:output,MAGI_PLUGIN_UI_USER_DATA:join(work,'electron')}});
  console.log(result.stdout.trim());
  check('真实工作台展示插件图标、设置页与启停恢复，390px 无横向溢出', true);
}
try {
  check('验收不接管已有 daemon', !await fetch(`${origin}/health`).then(()=>true,()=>false));
  // MAGI_PLUGIN_TEST_APP=<Magi.app 路径>：直接启动发行包里的 daemon，只使用随包 Node
  // （不设置 MAGI_EXTENSION_NODE，也不读 PATH），验证正式交付形态下的同一条纵向流程。
  const packagedApp=process.env.MAGI_PLUGIN_TEST_APP;
  const providerEnv={MAGI_OPENAI_COMPAT_BASE_URL:`${providerOrigin}/v1`,MAGI_OPENAI_COMPAT_API_KEY:'local-test',MAGI_OPENAI_COMPAT_MODEL:'local-test'};
  if (packagedApp) {
    const resources=join(packagedApp,'Contents','Resources');
    const env={...process.env,MAGI_PORT:String(port),MAGI_WEB_DEV:'0',MAGI_WEB_DIST_ROOT:join(resources,'web','dist'),MAGI_STATE_ROOT:join(work,'state'),MAGI_OPEN_BROWSER:'0',...providerEnv};
    delete env.MAGI_EXTENSION_NODE;
    daemon=spawn(join(resources,'daemon','magi-daemon-app'),[],{cwd:work,env,stdio:['ignore','pipe','pipe']});
  } else daemon=spawn(join(root,'scripts/dev-daemon.sh'),[],{cwd:root,env:{...process.env,MAGI_TARGET_PRUNE_GIB:'64',MAGI_PORT:String(port),MAGI_WEB_DEV:'0',MAGI_STATE_ROOT:join(work,'state'),MAGI_OPEN_BROWSER:'0',MAGI_EXTENSION_NODE:process.env.MAGI_EXTENSION_NODE||process.execPath,MAGI_OPENAI_COMPAT_BASE_URL:`${providerOrigin}/v1`,MAGI_OPENAI_COMPAT_API_KEY:'local-test',MAGI_OPENAI_COMPAT_MODEL:'local-test'},stdio:['ignore','pipe','pipe']});
  daemon.stdout.on('data',c=>{log+=c;}); daemon.stderr.on('data',c=>{log+=c;});
  await wait(async()=> (await fetch(`${origin}/health`)).ok,'daemon ready',180000);
  check('daemon 主入口提供静态工作台',(await fetch(`${origin}/web.html`)).ok);
  const first=await pack('1.0.0',1); const preview=await post('/api/plugins/preview',first);
  check('安装预览明确完全信任及包摘要', preview.digest && preview.addedPermissions.some(p=>p.kind==='extension'));
  await post('/api/plugins/install',{...first,expectedDigest:preview.digest});
  check('未授权插件资源不可读',(await api('/api/plugins/test.local/resources/notes')).status===403);
  check('旧安装 scope 被明确拒绝',(await api('/api/plugins/test.local/activate',{scope:'application'})).status>=400);
  await post('/api/plugins/test.local/authorize',{grants:preview.manifest.permissions}); await post('/api/plugins/test.local/enable'); await post('/api/plugins/test.local/activate');
  await wait(async()=> (await installed())?.runtime?.state==='running','Node ready');
  await post('/api/plugins/test.local/settings',{expectedVersion:0,value:{label:'configured'}},'PUT');
  const session=(await post('/api/session/materialize',{scope:'personal'})).sessionId;
  await post('/api/settings/orchestrator/session/save',{scope:'personal',sessionId:session,config:{model:'local-test',reasoningEffort:null}});
  const beforeGuard = await history(session);
  const blocked = await api('/api/session/turn',{scope:'personal',sessionId:session,text:'LOCAL_BLOCK',requestId:'local-block',userMessageId:'local-block-user'});
  check('真实 input guard 拒绝输入且不创建 Turn',blocked.status===403 && JSON.stringify(blocked.value).includes('LOCAL_GUARD_OK') && JSON.stringify(await history(session))===JSON.stringify(beforeGuard));
  const accepted=await command(session,'probe'); const turn=await settled(session,accepted.turnId);
  check('资源／设置／模型／快照／凭据／blob／通知与直接文件网络进程形成闭环',turn.status==='completed' && JSON.stringify(turn).includes('LOCAL_PROBE_OK'));
  check('模型输出预算传入真实 HTTP 请求',requests.some(r=>r.max_tokens===64 || r.max_completion_tokens===64));
  const notes=(await api('/api/plugins/test.local/resources/notes')).value;
  check('反向资源写入沿同一版本事实',notes.version===1 && notes.value.runs===1);
  check('过期版本写入明确冲突',(await api('/api/plugins/test.local/resources/notes',{expectedVersion:0,value:{}},'PUT')).status===409);
  const asking=await command(session,'ask');
  const question=await wait(async()=> (await api(`/api/session/user-questions?sessionId=${encodeURIComponent(session)}`)).value.pendingQuestions?.[0], '扩展提问');
  await post('/api/session/user-question',{sessionId:session,questionId:question.questionId,response:{kind:'answered',answers:[{selected:['A']}]}});
  const answered=await settled(session,asking.turnId);
  check('扩展提问经用户答复收口且无遗留等待',answered.status==='completed' && JSON.stringify(answered).includes('LOCAL_ANSWER') && (await api(`/api/session/user-questions?sessionId=${encodeURIComponent(session)}`)).value.pendingQuestions.length===0);
  const taskless=await command(session,'taskless');
  const denied=await settled(session,taskless.turnId);
  check('无 Task 的命令明确拒绝原生工具和子任务服务',denied.status==='completed' && JSON.stringify(denied).includes('LOCAL_TASKLESS_DENIED'));
  const plan=await post('/api/plugins/test.local/jobs',{commandId:'status',sessionId:session,text:'',runAt:Date.now(),intervalMs:null,budget:{maxModelCalls:0,maxOutputTokens:64,timeoutMs:5000}});
  await wait(async()=> (await api('/api/plugins/test.local/jobs')).value.find(j=>j.id===plan.id)?.lastOutcome==='completed','持久计划结算');
  check('daemon 原生调度执行命令并持久回执',true);
  if (process.argv.includes('--ui')) await verifyUi(session);
  const slow=await command(session,'wait'); await sleep(200); await post('/api/plugins/test.local/deactivate');
  const stopped=await settled(session,slow.turnId); check('停用在途宿主后 Turn 有限结算',stopped.status==='failed'||stopped.status==='cancelled');
  check('停用后资源和调用均撤销',(await api('/api/plugins/test.local/resources/notes')).status===403);
  await post('/api/plugins/test.local/activate'); await wait(async()=> (await installed())?.runtime?.state==='running','重新激活');
  const next=await pack('2.0.0',2); const nextPreview=await post('/api/plugins/preview',next);
  check('数据格式变化升级明确预告清空',nextPreview.dataReset===true);
  await post('/api/plugins/upgrade',{...next,expectedDigest:nextPreview.digest});
  const upgraded=await installed(); check('升级只保留启用意愿，清空授权和激活',upgraded.manifest.version==='2.0.0'&&upgraded.enabled&&!upgraded.active&&upgraded.grants.length===0);
  await post('/api/plugins/test.local/authorize',{grants:nextPreview.manifest.permissions}); await post('/api/plugins/test.local/activate'); await wait(async()=> (await installed())?.runtime?.state==='running','新版激活');
  const reset=(await api('/api/plugins/test.local/resources/notes')).value; check('新版不会读到旧格式资源',reset.version===0&&reset.value===null);
  await post('/api/plugins/test.local/deactivate'); await post('/api/plugins/test.local/disable'); await post('/api/plugins/test.local/uninstall');
  check('卸载后插件与贡献消失',!(await installed()));
  // ---- 插件贡献主对话模型数据源：授权预览、目录追加、真实请求走插件端点、撤销后不回退 ----
  const proxy=await packProxy(); const proxyPreview=await post('/api/plugins/preview',proxy);
  check('预览单独列出 modelSources 授权（区别于 models）',proxyPreview.addedPermissions.some(p=>p.kind==='modelSources'&&p.targets.includes('gateway'))&&!proxyPreview.addedPermissions.some(p=>p.kind==='models'));
  await post('/api/plugins/install',{...proxy,expectedDigest:proxyPreview.digest});
  const catalogGroups=async()=> (await api('/api/settings/models/catalog')).value.groups;
  check('未激活的插件不向目录追加数据源',!(await catalogGroups()).some(g=>g.kind==='plugin'));
  await post('/api/plugins/test.proxy/authorize',{grants:proxyPreview.manifest.permissions}); await post('/api/plugins/test.proxy/enable'); await post('/api/plugins/test.proxy/activate');
  const group=await wait(async()=> (await catalogGroups()).find(g=>g.sourceId==='plugin/test.proxy/gateway'),'插件数据源进入目录');
  check('插件数据源以插件分组追加，用户的环境数据源保持在前且不含连接信息',group.kind==='plugin'&&group.pluginId==='test.proxy'&&group.name==='本地反代'&&group.models[0].id==='proxy-model'&&(await catalogGroups())[0].kind==='custom'&&!JSON.stringify(await catalogGroups()).includes('plugin-key'));
  const proxySession=(await post('/api/session/materialize',{scope:'personal'})).sessionId;
  await post('/api/settings/orchestrator/session/save',{scope:'personal',sessionId:proxySession,config:{sourceId:'plugin/test.proxy/gateway',model:'proxy-model',reasoningEffort:null}});
  const before=authorizations.length;
  const proxied=await post('/api/session/turn',{scope:'personal',sessionId:proxySession,text:'PLUGIN_SOURCE_CHECK',requestId:'proxy-1',userMessageId:'proxy-user-1'});
  const proxiedTurn=await settled(proxySession,proxied.turnId);
  check('选用插件数据源的主对话请求真实发往插件提供的端点并带插件密钥',proxiedTurn.status==='completed'&&authorizations.slice(before).some(r=>r.model==='proxy-model'&&r.authorization==='Bearer plugin-key'));
  await post('/api/plugins/test.proxy/deactivate');
  await wait(async()=> !(await catalogGroups()).some(g=>g.kind==='plugin'),'插件停用后数据源撤销');
  const afterRevoke=authorizations.length;
  const orphan=await api('/api/session/turn',{scope:'personal',sessionId:proxySession,text:'AFTER_REVOKE',requestId:'proxy-2',userMessageId:'proxy-user-2'});
  const orphanSettled=orphan.status===200 ? await settled(proxySession,orphan.value.turnId) : null;
  check('插件数据源撤销后会话明确失败，不静默回退到其他模型',(orphan.status>=400||orphanSettled?.status==='failed')&&authorizations.length===afterRevoke);
  await post('/api/plugins/test.proxy/disable'); await post('/api/plugins/test.proxy/uninstall');
  check('MCP 管理面不依赖插件',(await api('/api/mcp-server/status')).status===200);
} catch(error) {console.error(error); console.error('plugin status:', JSON.stringify(await installed().catch(()=>null))); process.exitCode=1;}
finally {
  if(daemon&&daemon.exitCode==null) {daemon.kill('SIGTERM'); await Promise.race([new Promise(resolve=>daemon.once('exit',resolve)),sleep(15000)]); if(daemon.exitCode==null) daemon.kill('SIGKILL');}
  provider.closeAllConnections(); await new Promise(resolve=>provider.close(resolve));
  if(process.exitCode) {await writeFile(join(work,'daemon.log'),log); console.error(`验收资料保留：${work}`);}
  else {console.log(`${checks} 项本地插件验收通过`); await rm(work,{recursive:true,force:true});}
}
