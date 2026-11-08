// 从 macOS 目录包启动真实 Magi Main、daemon 与随包 Node/兼容层。
// 独立状态根；DSH 夹具不读用户凭据，模型回执由夹具适配器本地生成。
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile, rm, realpath, rename } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname, delimiter } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { createRequire } from 'node:module';
import { createServer } from 'node:http';
import { browserImageFixtures } from './lib/plugin-image-fixtures.mjs';
import { packPlugin } from './compat/core/pack.mjs';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const resources = join(appRoot, 'Contents/Resources');
const node = join(resources, 'daemon/node/node');
const compat = join(resources, 'daemon/compat');
const cdpPort = Number(process.env.MAGI_PLUGIN_RUNTIME_CDP_PORT || 9307);
const daemonPort = Number(process.env.MAGI_PLUGIN_RUNTIME_PORT || 38937);
const origin = `http://127.0.0.1:${daemonPort}`;
const output = process.env.MAGI_PLUGIN_RUNTIME_OUTPUT || join(root, 'target/plugin-acceptance/runtime');
const work = await mkdtemp(join(tmpdir(), 'magi-runtime-live-'));
const run = promisify(execFile);
const reporter = createReporter();
const errors = [];
const appLogs = [];
const clipAuditSource = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE, 'utf8') : null;
const externalDshCli = process.env.MAGI_DSH_EXTERNAL_CLI ? await realpath(process.env.MAGI_DSH_EXTERNAL_CLI) : null;
let app, page, frame, ui, pluginId, startupGate;
const heldStarts = new Set();
await mkdir(output, { recursive: true });
async function api(path, body, method = body === undefined ? 'GET' : 'POST') {
  const response = await fetch(`${origin}${path}`, {
    method, headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(180_000),
  });
  const raw = await response.text();
  let value; try { value = JSON.parse(raw); } catch { value = raw; }
  return { status: response.status, value };
}
async function post(path, body = {}, method = 'POST') {
  const result = await api(path, body, method);
  assert.equal(result.status, 200, `${path}: ${JSON.stringify(result.value)}`);
  return result.value;
}
const installed = async () => (await api('/api/plugins')).value.plugins.find((item) => item.manifest.id === pluginId);
const activeSkills = async () => (await api('/api/plugins/manifests')).value.flatMap(manifest => (manifest.contributions.skills ?? []).map(skill => ({ ...skill, contributionId: skill.id, id: `plugin/${manifest.id}/${skill.id}`, pluginId: manifest.id })));
const visible = (selector) => page.evaluate(`Boolean(document.querySelector(${JSON.stringify(selector)})?.getClientRects().length)`);
async function openConversation(session) {
  if (await visible('[data-testid=settings-page]')) {
    await ui.click('[data-testid=settings-back]');
    await waitFor(async()=>!await visible('[data-testid=settings-page]'),'返回工作台');
  }
  const row=`.session-item[data-session-id="${session}"]`;
  await waitFor(()=>ui.exists(row),'当前会话在原生侧栏可用');
  if (!await ui.exists(row+'.active')) await ui.click(row);
  await waitFor(async()=>await ui.exists(row+'.active') && !await visible('[data-testid=settings-page]') && await ui.exists('[data-testid=input-textarea]'),'切换到会话工作台');
}
async function settingsTab(id) {
  const nav = `[data-testid="settings-nav-${id}"]`;
  if (!await visible('[data-testid="settings-page"]')) await ui.click('[data-testid="sidebar-settings"]');
  await waitFor(async () => {
    if (!await ui.exists(nav)) return false;
    await ui.click(nav);
    return page.evaluate(`document.querySelector(${JSON.stringify(nav)})?.getAttribute('aria-current') === 'page'`);
  }, `设置分类 ${id}`);
}
async function snapshot(name) { await writeFile(join(output, `${name}.png`), Buffer.from(await ui.screenshot(), 'base64')); }
function startApp() {
  const instance = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    inspectPort: cdpPort + 2,
    env: { MAGI_STATE_ROOT: join(work, 'state'), MAGI_DESKTOP_DAEMON_PORT: String(daemonPort), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '',
      ...(externalDshCli ? { PATH: dirname(externalDshCli) + delimiter + process.env.PATH } : {}) },
  });
  appLogs.push(instance.log);
  return instance;
}

async function quitApp() {
  if (!app || app.child.exitCode !== null || app.child.signalCode !== null) return;
  // 与托盘退出共用 Main 的正式关闭顺序；先核对脚本自有进程。
  // about:blank 会触发可信文档恢复，SIGTERM 则会绕过窗口先关的退出流程。
  const main=await connectTarget(cdpPort+2,()=>true,'验收应用 Main inspector');
  try {
    assert.equal(await main.evaluate('process.pid'),app.child.pid,'不能退出其他应用实例');
    await main.evaluate("setImmediate(()=>process.getBuiltinModule('module')._load('electron').app.quit()); true");
  } finally { main.close(); }
  await waitFor(()=>app.child.exitCode!==null||app.child.signalCode!==null,'应用正式退出',30000);
}

async function prepare() {
  const upstream = join(work, 'upstream');
  await mkdir(upstream);
  const deps = JSON.parse(await readFile(join(compat, 'package.json'), 'utf8')).dependencies;
  await writeFile(join(upstream, 'package.json'), JSON.stringify({
    name: 'magi-runtime-acceptance', displayName: '运行时兼容验收', version: '1.0.0', type: 'module', main: 'index.js',
    dsh: { bundle: { patch: './cordis.patch.yml' } },
    peerDependencies: { '@deepseek-ai/cordis': '4.0.5-alpha.2', '@deepseek-ai/dsh-llm': deps['@deepseek-ai/dsh-llm'] },
    dependencies: { '@deepseek-ai/schemastery': deps['@deepseek-ai/schemastery'] },
  }));
  await writeFile(join(upstream, 'index.js'), `
import Schema from '@deepseek-ai/schemastery';
import { LlmAdapter } from '@deepseek-ai/dsh-llm';
import { Service } from '@deepseek-ai/cordis';
import cordis from '@deepseek-ai/cordis/package.json' with { type: 'json' };
import { createRequire } from 'node:module';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
const require = createRequire(import.meta.url);
export const name = 'magi-runtime-acceptance';
export const inject = ['commands', 'tools', 'llm', 'attachments', 'webServer', 'sessions', 'skills'];
export const Config = Schema.object({ label: Schema.string().default('initial'), enabled: Schema.boolean().default(true) });
export function apply(ctx, config) {
  let skillRequest;
  const toolOutcomes=[];
  ctx.webServer.register({path:'/tool-outcomes',resource:()=>Response.json(toolOutcomes)});
  ctx.skills.register({name:'runtime-inspection',description:'Inspect the current runtime',content:'RUNTIME_SKILL_BODY:'+config.label,source:'runtime'});
  ctx.webServer.register({path:'/skill-request',resource:()=>Response.json({request:skillRequest})});
  let peerService;
  ctx.plugin(class extends Service {
    constructor(c) { super(c, 'peerAcceptance'); this.version = cordis.version; peerService = this; }
  });
  ctx.webServer.register({path:'/cordis-peer',resource:()=>Response.json({
    esm:cordis.version, cjs:require('@deepseek-ai/cordis/package.json').version,
    service:peerService.version,
  })});
  let backgroundImage={pending:true};
  ctx.effect(()=>{
    const timer=setTimeout(async()=>{
      try {
        const data=Buffer.from("<svg xmlns='http://www.w3.org/2000/svg' width='1' height='1'><!--"+'x'.repeat(3*1024*1024)+"--></svg>");
        const ref=await ctx.attachments.saveImage({data,mediaType:'image/svg+xml',name:'background.svg'});
        const saved=await ctx.attachments.readImage(ref);
        backgroundImage={ref,sameBytes:saved.data.equals(data)};
      } catch(error) { backgroundImage={error:String(error)}; }
    },200);
    return ()=>clearTimeout(timer);
  });
  ctx.webServer.register({path:'/background-image',resource:()=>Response.json(backgroundImage)});
  ctx.commands.register({ name: 'report', description: '报告当前配置', handler: async () => ({ kind: 'text', text: 'DSH_REPORT:' + config.label }) });
  ctx.commands.register({name:'message',description:'向当前会话交付业务结果',handler:async invocation=>{
    const input=JSON.parse(invocation.rawInput);
    const message={id:input.id,role:'user',content:[{type:'text',text:input.text}],source:{kind:'runtime-result'}};
    const agent=invocation.agent;
    const method={context:'inject',steer:'steer',followup:'followup'}[input.delivery];
    agent[method](message);
    agent[method](message);
    if(input.fail) return {kind:'error',text:'MESSAGE_BUSINESS_FAILED'};
    if(input.wait) {
      await writeFile(join(agent.session.header.cwd,'message-waiting'),input.id);
      await new Promise(resolve=>invocation.signal.addEventListener('abort',resolve,{once:true}));
    }
    return {kind:'success',text:'MESSAGE_COMMAND_DONE:'+input.id};
  }});
  ctx.commands.register({ name: 'directory', description: '在本次会话的实际执行目录读取与写入', handler: async invocation => {
    const cwd = invocation.agent.session.header.cwd;
    const id = invocation.agent.id;
    if (invocation.rawInput === 'wait') {
      await writeFile(join(cwd, 'command-waiting'), id);
      await new Promise(resolve => invocation.signal.addEventListener('abort', resolve, {once:true}));
      await writeFile(join(cwd, 'command-cancelled'), id);
      return {kind:'error',text:'COMMAND_CANCELLED'};
    }
    const marker = await readFile(join(cwd, 'context-check.txt'), 'utf8');
    const result = {id,cwd,marker,label:config.label};
    await writeFile(join(cwd, 'command-result.json'), JSON.stringify(result));
    return {kind:'success',text:'COMMAND_DIRECTORY:'+JSON.stringify(result)};
  } });
  ctx.webServer.register({path:'/image-roundtrip',resource:async()=>{
    const data=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZ1cAAAAASUVORK5CYII=','base64');
    const ref=await ctx.attachments.saveImage({data,mediaType:'image/png',name:'capture.png'});
    const saved=await ctx.attachments.readImage({...ref,mediaType:'image/jpeg',name:'forged.jpg'});
    return Response.json({sameBytes:saved.data.equals(data),ref:saved.ref});
  }});
  if (config.enabled) ctx.effect(() => {
    const timer = setTimeout(() => ctx.tools.register({ name: 'late-read', description: '读取验收配置', parameters: { type: 'object' }, readOnly: true,
      output: { schema: { type: 'string' }, render: (_args, value) => [{ type: 'text', text: value }] }, execute: async (args,exec) => {
        if(args.outcome) {
          toolOutcomes.push({id:args.id,kind:args.outcome});
          exec.deferContext({id:'outcome-context-'+args.id,role:'user',content:[{type:'text',text:'DEFERRED_CONTEXT:'+args.id}],source:{kind:'runtime-tool-context'}});
          if(args.outcome!=='continue') exec.concludeTurn();
          if(args.outcome==='cancel') await new Promise(resolve=>exec.signal.addEventListener('abort',resolve,{once:true}));
          if(args.outcome==='failed') throw new Error('ACTUAL_BUSINESS_FAILURE:'+args.id);
          return 'ACTUAL_BUSINESS_RESULT:'+args.id;
        }
        if(args.message) {
          exec.agent.inject({id:'tool-result-message',role:'user',content:[{type:'text',text:'MESSAGE_TOOL_RESULT'}],source:{kind:'runtime-result'}});
          return 'TOOL_MESSAGE_DONE';
        }
        return args.directory ? 'TOOL_DIRECTORY:'+JSON.stringify({id:exec.agent.id,cwd:exec.agent.session.header.cwd,marker:await readFile(join(exec.agent.session.header.cwd,'context-check.txt'),'utf8')}) : config.label;
      } }), 1700);
    return () => clearTimeout(timer);
  });
  class Adapter extends LlmAdapter {
    async listModels(provider) { return [{ provider, id: 'fixture-model', name: 'Runtime Fixture' }]; }
    async resolveModel(provider, id) { return {provider,id,name:'Runtime Fixture',inputModalities:['text','image']}; }
    async *stream(request) {
      request.signal?.throwIfAborted();
      skillRequest = JSON.stringify(request);
      let text = 'DSH_MODEL:' + config.label;
      const lastUser = request.messages.findLast(message=>message.role==='user'
        && message.content.some(block=>block.type==='text' && !block.text.startsWith('<magi-context-fragment ')));
      const lastTool = request.messages.findLast(message=>message.role==='tool');
      const directoryCheck = lastUser?.content.some(block=>block.type==='text' && block.text.includes('DIRECTORY_TOOL_CHECK'));
      const directoryResult = lastTool && JSON.stringify(lastTool.content).includes('TOOL_DIRECTORY:');
      const messageCheck=lastUser?.content.some(block=>block.type==='text' && block.text.includes('MESSAGE_TOOL_CHECK'));
      const originalInput=lastUser?.content.find(block=>block.type==='text' && block.text.startsWith('ORIGINAL_TOOL_CHECK:'))?.text;
      const originalCall=originalInput?JSON.parse(originalInput.slice('ORIGINAL_TOOL_CHECK:'.length)):null;
      const originalResult=originalCall?request.messages.slice(request.messages.indexOf(lastUser)+1).findLast(message=>message.role==='tool'&&message.toolCallId==='acceptance-tool-call'):null;
      const outcomeUser=request.messages.findLast(message=>message.role==='user'&&message.content.some(block=>block.type==='text'&&block.text.startsWith('TOOL_OUTCOME_CHECK:')));
      const outcomeInput=outcomeUser?.content.find(block=>block.type==='text'&&block.text.startsWith('TOOL_OUTCOME_CHECK:'))?.text;
      const outcomeCall=outcomeInput?JSON.parse(outcomeInput.slice('TOOL_OUTCOME_CHECK:'.length)):null;
      const outcomeResult=outcomeCall?request.messages.slice(request.messages.indexOf(outcomeUser)+1).findLast(message=>message.role==='tool'&&message.toolCallId==='acceptance-tool-call'):null;
      if ((directoryCheck && !directoryResult) || (messageCheck && !(JSON.stringify(lastTool?.content)??'').includes('TOOL_MESSAGE_DONE')) || (originalCall && !originalResult) || (outcomeCall&&!outcomeResult)) {
        const tool = request.tools.find(tool=>originalCall?tool.description.startsWith(originalCall.description):tool.description==='读取验收配置');
        // 动态工具尚未绑定时，模型沿公开目录发现入口刷新；不把 running
        // 状态当作未来动态贡献已经就绪，也不在同一固定请求内盲目重试。
        const selected=tool??request.tools.find(tool=>tool.name.endsWith('tool_catalog'));
        if(!selected)throw new Error('Requested acceptance tool and discovery entry are missing from the real model catalog');
        const block = {type:'tool-call',id:tool?'acceptance-tool-call':'catalog-discovery',name:selected.name,arguments:JSON.stringify(tool?(originalCall?originalCall.args:outcomeCall?{outcome:outcomeCall.kind,id:outcomeCall.id}:messageCheck?{message:true}:{directory:true}):{include_external:true,include_schema:true})};
        yield {type:'block-start',index:0,blockType:'tool-call'};
        yield {type:'tool-call-delta',index:0,id:block.id,name:block.name,argumentsDelta:block.arguments};
        yield {type:'block-end',index:0,block};
        yield {type:'usage',usage:{inputTokens:1,outputTokens:1,totalTokens:2}};
        yield {type:'finish',reason:{kind:'tool-calls'}};
        return;
      }
      if (directoryCheck && directoryResult) text=JSON.stringify(lastTool.content);
      if (originalCall && originalResult) text=JSON.stringify(originalResult.content);
      const lastImage = request.messages.flatMap(message=>message.content).filter(block=>block.type==='image').at(-1);
      const images = lastImage ? [lastImage] : [];
      if (images.length) {
        const sizes=[];
        for(const block of images) {
          const image=await ctx.attachments.readImageRequest(block.attachment,{width:64,height:64,maxBytes:65536},request.signal);
          if(image.mediaType!=='image/jpeg' || image.bytes!==image.data.length) throw new Error('invalid model image');
          sizes.push(image.width+'x'+image.height);
        }
        text='DSH_IMAGE:'+sizes.join(',');
      }
      yield { type: 'block-start', index: 0, blockType: 'text' };
      yield { type: 'text-delta', index: 0, text };
      yield { type: 'block-end', index: 0, block: { type: 'text', text } };
      yield { type: 'usage', usage: { inputTokens: 1, outputTokens: 1, totalTokens: 2 } };
      yield { type: 'finish', reason: { kind: 'stop' } };
    }
  }
  ctx.llm.registerAdapter(['runtime-fixture'], new Adapter());
}

`);
  const archive = join(work, 'plugin.zip');
  const result = await run(node, [join(compat, 'cli.mjs'), 'prepare', 'dsh', upstream, '--out', join(work, 'prepared'), '--integrity', 'local-acceptance', '--archive', archive], { timeout: 120_000, maxBuffer: 1024 * 1024 });
  const prepared = JSON.parse(result.stdout);
  const manifest = JSON.parse(await readFile(join(work, 'prepared/manifest.json'), 'utf8'));
  pluginId = prepared.id;
  reporter.check('随包 Node 与兼容层完成原包封装，安装期不冻结动态贡献', prepared.report.runtimeContributions.includes('tools') && manifest.contributions.tools.length === 0);
  reporter.check('不替换 Magi 运行库，从 npm 在线安装满足声明的独立 Cordis peer 并保留完整性',
    prepared.report.dependencies.some(dependency => dependency.name === '@deepseek-ai/cordis'
      && dependency.isolated && dependency.host === false && dependency.version === '4.0.5-alpha.2' && dependency.integrity.startsWith('sha512-')));
  return { source: 'local:runtime-acceptance.zip', archiveBase64: (await readFile(archive)).toString('base64') };
}

async function verifyOriginalSkill(session, waitTurn) {
  const request = {ecosystem:'dsh',package:'dsh-plugin-debugging',version:'0.7.2'};
  const preview = await post('/api/plugins/upstream/preview', request);
  const list = await post('/api/plugins/upstream/install', {...request,expectedDigest:preview.candidate.digest});
  const original = list.plugins.find(plugin=>plugin.source.location==='dsh:dsh-plugin-debugging@0.7.2');
  const path = `/api/plugins/${original.manifest.id}`;
  await post(path+'/enable'); await post(path+'/authorize',{grants:original.manifest.permissions}); await post(path+'/activate');
  const running = await waitFor(async()=>{
    const plugin=(await api('/api/plugins')).value.plugins.find(plugin=>plugin.manifest.id===original.manifest.id);
    return plugin?.runtime?.state==='running' && plugin;
  },'Debugging 原包技能注册',120_000);
  const skill=(await activeSkills()).find(skill=>skill.pluginId===original.manifest.id);
  const body=running.manifest.contributions.skills.find(entry=>entry.id===skill.contributionId).instruction;
  const published=await readFile(join(work,'state/extensions/packages',original.manifest.id,running.digest,'extension/upstream/skills/dsh-plugin-debugging/SKILL.md'),'utf8');
  reporter.check('在线原包的技能正文逐字进入 daemon 当前贡献目录', body===published.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n/,'').trim());
  await openConversation(session);
  const selectSkill=async()=>{
    await waitFor(()=>ui.exists('[data-testid=input-textarea][contenteditable=true]'),'技能输入可编辑');
    await page.evaluate(`document.querySelector('[data-testid=input-textarea][contenteditable=true]').focus({preventScroll:true})`);
    await waitFor(()=>page.evaluate(`document.activeElement===document.querySelector('[data-testid=input-textarea]')`),'输入框取得真实键盘焦点');
    await page.call('Input.insertText',{text:'/dsh-plugin-debugging'});
    await waitFor(()=>page.evaluate(`Boolean([...document.querySelectorAll('.ia-slash-item')].find(item=>item.querySelector('.ia-slash-item-label')?.textContent==='/dsh-plugin-debugging'))`),'原包技能出现在斜杠菜单');
  };
  await selectSkill();
  await snapshot('original-skill-desktop');
  await page.call('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false});
  const narrow=await page.evaluate(`(()=>{const r=document.querySelector('.ia-slash-popover').getBoundingClientRect();return {left:r.left,right:r.right,width:innerWidth,overflow:document.documentElement.scrollWidth>innerWidth};})()`);
  reporter.check('390px 原包技能菜单沿共享输入组件展示且无横向溢出',narrow.left>=0&&narrow.right<=narrow.width&&!narrow.overflow,JSON.stringify(narrow));
  await snapshot('original-skill-mobile');
  await page.call('Emulation.clearDeviceMetricsOverride');
  await page.evaluate(`[...document.querySelectorAll('.ia-slash-item')].find(item=>item.querySelector('.ia-slash-item-label')?.textContent==='/dsh-plugin-debugging').dispatchEvent(new MouseEvent('mousedown',{bubbles:true,cancelable:true}))`);
  await waitFor(()=>ui.exists('.ia-skill-chip'),'原包技能被选择');
  await page.call('Input.insertText',{text:'verify original skill body'});
  const before=(await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).value.canonicalTurns?.length??0;
  await waitFor(()=>page.evaluate(`Boolean(document.querySelector('[data-testid=input-send-button]')) && !document.querySelector('[data-testid=input-send-button]').disabled`),'技能发送就绪');
  await ui.click('[data-testid=input-send-button]');
  const accepted=await waitFor(async()=>{
    const turns=(await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).value.canonicalTurns;
    return turns?.length>before&&turns.at(-1);
  },'原包技能经输入框提交');
  const completed=await waitTurn(accepted.turnId);
  const received=await post(`/api/plugins/${pluginId}/routes`,{method:'GET',path:'/skill-request'});
  reporter.check('原包技能沿真实输入框、canonical Turn 与模型请求完成调用',completed.status==='completed'
    && received.body.request.includes(JSON.stringify(body).slice(1,-1)),JSON.stringify(completed).slice(0,300));
  reporter.check('canonical 用户消息保存技能身份与接纳时名称',completed.items.some(item=>item.metadata?.skillName===skill.id&&item.metadata?.skillTitle===skill.title));
  await waitFor(()=>page.evaluate(`Array.from(document.querySelectorAll('.user-command-text')).some(node=>node.textContent==='/dsh-plugin-debugging')`),'消息显示原包技能名称');
  await snapshot('original-skill-conversation');
  await selectSkill();
  await page.evaluate(`[...document.querySelectorAll('.ia-slash-item')].find(item=>item.querySelector('.ia-slash-item-label')?.textContent==='/dsh-plugin-debugging').dispatchEvent(new MouseEvent('mousedown',{bubbles:true,cancelable:true}))`);
  await post(path+'/deactivate');
  await waitFor(async()=>!(await activeSkills()).some(entry=>entry.id===skill.id)&&!await ui.exists('.ia-skill-chip'),'停用撤销技能目录和草稿选择');
  reporter.check('停用从同一事实源撤销技能入口和已选技能',true);
  reporter.check('插件停用后历史消息仍显示接纳时技能名称',await page.evaluate(`Array.from(document.querySelectorAll('.user-command-text')).some(node=>node.textContent==='/dsh-plugin-debugging')`));
  const stale=await api('/api/session/turn',{scope:'personal',sessionId:session,text:'stale skill',skillName:skill.id,requestId:'stale-plugin-skill',userMessageId:'stale-plugin-skill-user'});
  const staleResult=stale.status===200 ? await waitTurn(stale.value.turnId) : stale.value;
  reporter.check('停用后的旧技能身份不能继续执行',JSON.stringify(staleResult).includes('skill_unavailable')
    && (stale.status>=400 || ['failed','blocked'].includes(staleResult.status)),JSON.stringify(staleResult).slice(0,500));
  await post(path+'/activate');
  await waitFor(async()=>(await activeSkills()).some(entry=>entry.id===skill.id),'重新启用恢复同一技能身份');
  const bootstrap=(await api(`/api/settings/bootstrap?scope=personal&sessionId=${encodeURIComponent(session)}`)).value;
  reporter.check('重新激活重新注册技能，不写入本地技能配置',!bootstrap.skillsConfig.instructionSkills.some(entry=>entry.skillId===skill.id||entry.name==='dsh-plugin-debugging'));
  page.close();
  await quitApp();
  await waitFor(async()=>!await fetch(`${origin}/health`).then(()=>true,()=>false),'关闭后 daemon 完成退出');
  app = startApp();
  page = await waitFor(()=>connectAppPage(cdpPort,daemonPort,errors),'同一状态根重新启动工作台',60_000);
  ui = pageTools(page);
  await waitFor(()=>page.evaluate(`Boolean(document.documentElement.dataset.magiThemeId && document.querySelector('[data-testid=sidebar-settings]'))`),'重启后的原生窗口初始化完成');
  await waitFor(async()=>(await activeSkills()).some(entry=>entry.id===skill.id&&entry.instruction===body&&entry.description===skill.description),'重启后原包重新发布技能',120_000);
  const resumed = await post('/api/session/turn',{scope:'personal',sessionId:session,text:'verify skill after restart',skillName:skill.id,requestId:'original-skill-restart',userMessageId:'original-skill-restart-user'});
  const resumedTurn = await waitTurn(resumed.turnId);
  const resumedRequest = await post(`/api/plugins/${pluginId}/routes`,{method:'GET',path:'/skill-request'});
  reporter.check('应用重启后原包技能恢复相同身份和原文并完成真实模型调用',resumedTurn.status==='completed'
    && resumedRequest.body.request.includes(JSON.stringify(body).slice(1,-1)));
  const originalTool=async(name,description,args)=>{
    const diagnosis=await post('/api/session/turn',{scope:'personal',sessionId:session,
      text:'ORIGINAL_TOOL_CHECK:'+JSON.stringify({description,args}),
      requestId:name,userMessageId:name+'-user'});
    const pending=await waitFor(async()=>{
      const approvals=(await api(`/api/session/tool-approvals?sessionId=${session}`)).value.pendingApprovals;
      return approvals.find(approval=>approval.turnId===diagnosis.turnId);
    },'原包工具沿原生权限入口等待审批');
    const current=(await api(`/api/messages?scope=personal&sessionId=${session}`)).value.canonicalTurns.find(turn=>turn.turnId===diagnosis.turnId);
    const call=current.items.find(item=>item.tool?.callId===pending.toolCallId)?.tool;
    assert.equal(call?.name,pending.toolName);
    // 公共 canonical 按既有合同隐藏绝对路径；真实原值另从模型请求及
    // 原包工具回执核对，不降低为只看 [path] 占位符。
    const publicArgs={...args,...(args.dshHome?{dshHome:'[path]'}:{})};
    assert.deepEqual(call?.arguments,publicArgs,'只能批准本次隔离验收指定的诊断参数');
    await openConversation(session);
    await waitFor(()=>page.evaluate(`(() => {const buttons=[...document.querySelectorAll('.tool-approval .btn--primary')].filter(button=>!button.disabled);if(buttons.length!==1)return false;buttons[0].click();return true;})()`),'用户通过真实原生审批继续诊断');
    const result=await waitTurn(diagnosis.turnId);
    await writeFile(join(output,`${name}.json`),JSON.stringify(result,null,2));
    assert.equal(result.status,'completed',JSON.stringify(result));
    assert.ok(result.items.some(item=>item.kind==='tool_call'&&item.status==='completed')&&result.modelSteps.length>=2,JSON.stringify(result));
    if(args.dshHome) {
      const observed=JSON.parse((await post(`/api/plugins/${pluginId}/routes`,{method:'GET',path:'/skill-request'})).body.request);
      const user=observed.messages.findLast(message=>message.role==='user'&&message.content.some(part=>part.type==='text'&&part.text.startsWith('ORIGINAL_TOOL_CHECK:')));
      const input=user.content.find(part=>part.type==='text'&&part.text.startsWith('ORIGINAL_TOOL_CHECK:')).text;
      assert.deepEqual(JSON.parse(input.slice('ORIGINAL_TOOL_CHECK:'.length)).args,args);
      const tool=observed.messages.slice(observed.messages.indexOf(user)+1).findLast(message=>message.role==='tool');
      assert.ok(JSON.stringify(tool?.content).includes(args.dshHome),'真实原包结果必须保留本次指定的外部目录');
      await writeFile(join(output,`${name}-model-request.json`),JSON.stringify(observed,null,2));
    }
    const receipt=result.items.find(item=>item.kind==='tool_call').tool.result.output;
    return {value:receipt.value,text:receipt.content.filter(part=>part.type==='text').map(part=>part.text).join('\n')};
  };
  const diagnose=(name,packageName)=>originalTool(name,'Checks whether a plugin loaded.',{package:packageName});
  const diagnosed=await diagnose('original-debugging-active','dsh-plugin-debugging');
  reporter.check('Debugging 原包经真实审批与模型工具调用读取 daemon 当前加载事实',diagnosed.value.ok===true
    &&diagnosed.value.data.key.packages[0].entryState==='present'&&diagnosed.value.data.key.packages[0].phase==='active'
    &&(diagnosed.value.data.key.observation.phaseHistogram.failed??0)===0,JSON.stringify(diagnosed));
  const targetRequest={ecosystem:'dsh',package:'dsh-plugin-release-probe',version:'0.0.7'};
  const targetPreview=await post('/api/plugins/upstream/preview',targetRequest);
  const targetList=await post('/api/plugins/upstream/install',{...targetRequest,expectedDigest:targetPreview.candidate.digest});
  const target=targetList.plugins.find(plugin=>plugin.source.location==='dsh:dsh-plugin-release-probe@0.0.7');
  const targetPath=`/api/plugins/${target.manifest.id}`;
  const disabled=await diagnose('original-debugging-disabled','dsh-plugin-release-probe');
  reporter.check('Debugging 原包将真实停用状态与启动失败区分展示',disabled.value.ok===true
    &&disabled.value.data.key.packages[0].entryState==='present'&&disabled.value.data.key.packages[0].phase==='disabled',JSON.stringify(disabled));
  await post(targetPath+'/enable');await post(targetPath+'/authorize',{grants:target.manifest.permissions});
  await post(targetPath+'/activate');
  await waitFor(async()=>(await api('/api/plugins')).value.plugins.find(plugin=>plugin.manifest.id===target.manifest.id)?.runtime?.state==='running','故障注入前原包实际运行');
  await post(targetPath+'/deactivate');
  const entry=join(work,'state/extensions/packages',target.manifest.id,target.digest,'extension/index.mjs');
  const saved=entry+'.acceptance-save';
  await rename(entry,saved);await mkdir(entry);
  try {
    await post(targetPath+'/activate');
    const failure=await waitFor(async()=>{
      const plugin=(await api('/api/plugins')).value.plugins.find(plugin=>plugin.manifest.id===target.manifest.id);
      return plugin?.runtime?.state==='failed'&&plugin;
    },'验收包入口故障到达真实监管状态',45000);
    const failed=await diagnose('original-debugging-failed','dsh-plugin-release-probe');
    reporter.check('Debugging 原包的失败诊断保留 daemon 实际原因，其他插件故障不终止诊断器',failed.value.ok===true
      &&failed.value.data.key.packages[0].phase==='failed'&&failed.value.data.key.packages[0].error===failure.runtime.reason,JSON.stringify(failed));
    await post(targetPath+'/deactivate');
  } finally { await rm(entry,{recursive:true,force:true});await rename(saved,entry); }
  await post(targetPath+'/activate');
  await waitFor(async()=>(await api('/api/plugins')).value.plugins.find(plugin=>plugin.manifest.id===target.manifest.id)?.runtime?.state==='running','恢复原包入口后重新启用');
  const recovered=await diagnose('original-debugging-recovered','dsh-plugin-release-probe');
  reporter.check('Debugging 原包重新读取恢复后的运行事实，不保留旧故障',recovered.value.ok===true&&recovered.text.includes('active')&&!recovered.text.includes('Observed load failure'),JSON.stringify(recovered));
  if (externalDshCli) {
    const externalHome=join(work,'external-dsh'), externalProfile=join(externalHome,'profiles/magi');
    await mkdir(externalProfile,{recursive:true});
    await writeFile(join(externalProfile,'package.json'),JSON.stringify({name:'external-diagnostic-profile',private:true,dsh:{profile:{bundles:[]}},dependencies:{}}));
    await writeFile(join(externalProfile,'cordis.yml'),'[]\n');
    await writeFile(join(externalProfile,'cordis.patch.yml'),'[]\n');
    const before=(await api('/api/plugins')).value.plugins.map(plugin=>({id:plugin.manifest.id,digest:plugin.digest,enabled:plugin.enabled,active:plugin.active}));
    const dumped=await originalTool('original-debugging-external-config','Shows what a profile resolves to.',{dshHome:externalHome});
    const key=dumped.value.data?.key;
    reporter.check('Debugging 原包调用真实外部 DSH CLI 导出配置并完成 schema 检查',dumped.value.ok===true
      &&key?.resolved.dshHome==='[path]'&&key.invocations.length===2&&key.invocations.every(invocation=>invocation.exitCode===0),JSON.stringify(dumped));
    const effect=await originalTool('original-debugging-external-effect','Classifies what a change needs to take effect:',{change:'config',dshHome:externalHome});
    reporter.check('外部 DSH 的生效诊断读取其真实空配置，未混入 Magi 的重载能力',effect.value.ok===true
      &&effect.value.data.key.mode==='restart-required'&&effect.value.data.key.hmr==='absent',JSON.stringify(effect));
    const after=(await api('/api/plugins')).value.plugins.map(plugin=>({id:plugin.manifest.id,digest:plugin.digest,enabled:plugin.enabled,active:plugin.active}));
    reporter.check('外部实例诊断保留独立配置文件且不改变 Magi 的安装与运行意愿',JSON.stringify(after)===JSON.stringify(before)
      &&JSON.parse(await readFile(join(externalProfile,'package.json'),'utf8')).name==='external-diagnostic-profile');
  }
  await post(targetPath+'/deactivate');await post(targetPath+'/disable');await post(targetPath+'/uninstall');
  await post(path+'/deactivate'); await post(path+'/disable'); await post(path+'/uninstall');
}

async function verifyNativePanels(session) {
  // 在真实异步激活期间保持后台未就绪，验证页面不能抢先发起业务请求。
  let holdStart = false;
  startupGate = createServer((_request, response) => {
    if (!holdStart) return response.end('ready');
    heldStarts.add(response);
    response.on('close', () => heldStarts.delete(response));
  });
  await new Promise(resolve => startupGate.listen(0, '127.0.0.1', resolve));
  const gateUrl = `http://127.0.0.1:${startupGate.address().port}`;
  const upstream = join(work, 'native-panels');
  await mkdir(upstream);
  const deps = JSON.parse(await readFile(join(compat, 'package.json'), 'utf8')).dependencies;
  await writeFile(join(upstream, 'package.json'), JSON.stringify({ name: 'magi-native-panels', version: '1.0.0', type: 'module', main: 'index.js',
    dependencies: { '@deepseek-ai/schemastery': deps['@deepseek-ai/schemastery'] },
    dsh: { bundle: {} }, exports: { '.': './index.js', './client': './client.js' } }));
  await writeFile(join(upstream, 'index.js'), `
import Schema from '@deepseek-ai/schemastery';
export const inject = ['webServer'];
export const Config = Schema.object({ startupUrl: Schema.string().default('') });
export async function apply(ctx, config) {
  if (config.startupUrl) await fetch(config.startupUrl);
  const clients=new Set();
  ctx.webServer.register({path:'/panel-events',handler(req,res){
    res.writeHead(200,{'content-type':'text/event-stream'});
    res.write(': connected\\n\\n'); clients.add(res);
    const timer=setInterval(()=>res.write('event: pulse\\ndata: '+JSON.stringify({live:clients.size})+'\\n\\n'),150);
    req.on('close',()=>{clearInterval(timer);clients.delete(res);});
    res.on('close',()=>{clearInterval(timer);clients.delete(res);});
  }});
  ctx.webServer.register({ path: '/panel-action', handler: async (req,res) => {
    let body=''; for await(const chunk of req) body+=chunk;
    const data=JSON.parse(body || '{}');
    res.setHeader('content-type','application/json');
    res.end(JSON.stringify({key:req.headers['x-panel-key'], result:String(data.text).toUpperCase(),live:clients.size}));
  }});
  ctx.webServer.register({path:'/binary-echo',handler:async(req,res)=>{
    const chunks=[];for await(const chunk of req)chunks.push(chunk);
    const bytes=Buffer.concat(chunks);
    res.writeHead(201,{'content-type':req.headers['content-type']??'application/octet-stream',
      'content-disposition':'attachment; filename="original.bin"','x-original-bytes':String(bytes.length),
      'set-cookie':'must-not-enter-magi=1','content-length':String(bytes.length)});
    res.write(bytes.subarray(0,7));res.end(bytes.subarray(7));
  }});
  ctx.webServer.register({path:'/binary-image',resource:()=>new Response(
    Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZ1cAAAAASUVORK5CYII=','base64'),
    {headers:{'content-type':'image/png','x-image-source':'original-route'}})});
  ctx.webServer.register({path:'/vector-image',resource:()=>new Response(
    '<svg xmlns="http://www.w3.org/2000/svg" width="12" height="9"><rect width="12" height="9" fill="red"/></svg>',
    {headers:{'content-type':'image/svg+xml'}})});
  ctx.webServer.register({path:'/http-method',handler(req,res){
    res.writeHead(204,{'allow':'GET, HEAD, OPTIONS','x-original-method':req.method});res.end();
  }});
  const calls = new Map();
  ctx.webServer.register({path:'/slow-action',handler(req,res){
    const name=req.headers['x-call-id'];
    const call={state:'running',res};calls.set(name,call);
    const timer=setTimeout(()=>res.end('deadline'),30000);
    res.on('close',()=>{clearTimeout(timer);if(call.state==='running')call.state='cancelled';});
  }});
  ctx.webServer.register({path:'/call-state',resource:()=>Response.json(Object.fromEntries([...calls].map(([name,call])=>[name,call.state])))});
  ctx.webServer.register({path:'/complete-call',handler:async(req,res)=>{
    let name='';for await(const chunk of req)name+=chunk;
    const call=calls.get(name);call.state='completed';call.res.end('completed:'+name);res.end('ok');
  }});
}`);
  await writeFile(join(upstream, 'client.js'), `
window.__ModuleLoader__.load({id:'native-panels',factory(require){
  const React=require('react'), h=React.createElement;
  const getModules=()=>require;
  function attach(c,scope,ids) { const input=c.conversation.input.for(scope); return input.addAttachments(ids); }
  return { async apply(ctx) {
    await Promise.resolve();
    window.__clientInstance = String(Math.random());
    window.__calls={};
    window.__startCall=(name,asRequest)=>{
      const controller=new AbortController();
      const init={headers:{'x-call-id':name},signal:controller.signal};
      const task=asRequest?fetch(new Request(new URL('/slow-action',document.baseURI),init)):fetch('/slow-action',init);
      const call={controller,state:'waiting'};window.__calls[name]=call;
      task.then(response=>response.text()).then(value=>{call.state=value;},error=>{call.state=error.name;});
    };
    window.__composer = ctx.magiComposer;
    window.__dshAdd=async(file)=>{
      const sessionId=ctx.sessions.list.getSnapshot().ids[0],scope=ctx.sessions.scope(sessionId);
      const [draft]=ctx.conversation.createDrafts(sessionId,[file]);
      const accepted=attach(ctx,scope,[draft.id]);
      return {accepted,id:draft.id,ids:ctx.conversation.input.for(scope).snapshot.attachmentIds};
    };
    window.__dshRemove=async(id)=>{
      const sessionId=ctx.sessions.list.getSnapshot().ids[0],scope=ctx.sessions.scope(sessionId);
      const removed=ctx.conversation.input.for(scope).removeAttachment(id);
      ctx.conversation.releaseDraftAttachment(id);
      return {removed,ids:ctx.conversation.input.for(scope).snapshot.attachmentIds};
    };
    ctx.inject(['sidebarRight','sidebarRightTabs'], c => {
      for(const key of ['first','second']) {
        c.effect(() => c.sidebarRightTabs.register({id:key,kind:key,title:()=>key}));
        ctx.slots.register({name:'sidebar.right.pane.tab',key}, () => {
          const [text,setText]=React.useState(key);
          const [pulse,setPulse]=React.useState('waiting');
          const [dialog,setDialog]=React.useState(false);
          const sdk=dialog?getModules()('@deepseek-ai/dsh-client-ui-primitives'):null;
          React.useEffect(()=>{
            const source=new EventSource('/panel-events');
            source.addEventListener('pulse',event=>setPulse(JSON.parse(event.data).live));
            return ()=>source.close();
          },[]);
          return h('main',{},h('h1',{id:'identity'},key),
            h('button',{id:'open-sdk-dialog',onClick:()=>setDialog(true)},'Actions'),
            dialog?h(sdk.Modal,{open:true,title:'SDK actions',closeLabel:'Close',onClose:()=>setDialog(false),footer:h(sdk.Button,{
              'data-testid':'sdk-confirm',variant:'primary',onClick:()=>{setText('sdk-confirmed');setDialog(false);}
            },'Confirm')},h(React.Fragment,null,'Original shared components',
              h('button',{'data-theme-contrast':'inverse',style:{background:'var(--dsw-alias-label-primary)',color:'var(--dsw-alias-bg-layer-3)'}},'Continue'),
              h('button',{'data-theme-contrast':'tertiary',style:{background:'var(--magi-canvas)',color:'var(--dsw-alias-label-tertiary)'}},'Details'),
              h('span',{'data-theme-contrast':'layer',style:{background:'var(--dsw-alias-bg-layer-3)',color:'var(--dsw-alias-label-primary)'}},'Layer content'))):null,
            h('button',{id:'action',onClick:async()=>{
              localStorage.setItem('panel-cache', key); sessionStorage.setItem('panel-session', 'same-client');
              const response=await fetch('/panel-action',{method:'POST',headers:{'content-type':'application/json','x-panel-key':key},body:JSON.stringify({text:key})});
              const result=await response.json(); setText(result.key+':'+result.result);
            }},'Run'),
            h('output',{id:'receipt'},text),h('output',{id:'pulse'},pulse),
            h('button',{id:'next-panel',onClick:()=>c.sidebarRight.openTab(key==='first'?'second':'first')},'Next panel'));
        });
      }
    });
  }};
}});`);
  const archive = join(work, 'native-panels.zip');
  const result = await run(node, [join(compat, 'cli.mjs'), 'prepare', 'dsh', upstream, '--out', join(work, 'native-prepared'), '--integrity', 'local-acceptance', '--archive', archive], { timeout: 120_000 });
  const prepared = JSON.parse(result.stdout), id = prepared.id;
  const payload = { source: 'local:native-panels.zip', archiveBase64: (await readFile(archive)).toString('base64') };
  const preview = await post('/api/plugins/preview', payload);
  await post('/api/plugins/install', { ...payload, expectedDigest: preview.digest });
  await post(`/api/plugins/${id}/enable`);
  const plugin = (await api('/api/plugins')).value.plugins.find((item) => item.manifest.id === id);
  await post(`/api/plugins/${id}/authorize`, { grants: plugin.manifest.permissions });
  await post(`/api/plugins/${id}/activate`);
  await waitFor(async () => (await api('/api/plugins')).value.plugins.find((item) => item.manifest.id === id)?.runtime?.state === 'running', '原生多面板插件');
  reporter.check('同一槽位的两个条件注册面板均进入 daemon 视图目录', plugin.manifest.contributions.views.length === 2 && plugin.manifest.contributions.views.every((view) => view.placements.includes('rightPane')));
  await openConversation(session);
  await waitFor(() => ui.exists('.header-right-pane-btn'), '工作台顶栏');
  await page.evaluate(`localStorage.setItem('magi-ui-private-probe','private'); true`);
  if (!await visible('.right-pane')) await ui.click('.header-right-pane-btn');
  await waitFor(() => ui.exists('.right-pane-add-tab'), '右栏入口');
  await ui.click('.right-pane-add-tab');
  const first = plugin.manifest.contributions.views.find((view) => view.title === 'first');
  const entry = `.launcher-tool[data-plugin-id="${id}"][data-contribution-id="${first.id}"]`;
  await waitFor(() => ui.exists(entry), '第一个原生面板'); await ui.click(entry);
  let clientInstance;
  for (const key of ['first', 'second']) {
    frame?.close();
    const selector = `iframe[data-plugin-id="${id}"]`;
    await waitFor(() => ui.exists(selector), '右栏 iframe');
    const url = await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).src`);
    frame = await waitFor(() => connectTarget(cdpPort, (target) => (target.type === 'iframe' || target.type === 'page') && target.url === url, '右栏原生页面', errors), '右栏原生页面');
    await waitFor(() => frame.evaluate(`document.getElementById('identity')?.textContent===${JSON.stringify(key)}`), '正确的页面身份');
    await waitFor(() => frame.evaluate(`document.getElementById('pulse')?.textContent==='1'`), 'SSE 事件通过真实 daemon 与 iframe 到达上游组件');
    reporter.check(`右栏 ${key} 持续接收 SSE，切换后只有一个活跃上游连接`, true);
    const currentInstance = await frame.evaluate('window.__clientInstance');
    if (key === 'first') clientInstance = currentInstance;
    else reporter.check('切换原生右栏页签保留同一客户端文档且不重复初始化业务', currentInstance === clientInstance);
    if (key === 'first') {
      await frame.evaluate(`document.getElementById('open-sdk-dialog').click();true`);
      await waitFor(()=>frame.evaluate(`(() => {const dialog=document.querySelector('[role=dialog]');return dialog?.innerText.includes('Original shared components')&&getComputedStyle(dialog).opacity==='1';})()`),'间接取用官方组件库打开原生对话框');
      reporter.check('公共 SDK 对话框保留实际 SVG 图形、遮罩和主按钮填色',await frame.evaluate(`(() => {
        const dialog=document.querySelector('[role=dialog]'),button=dialog.querySelector('[data-testid=sdk-confirm]'),icon=dialog.querySelector('button[aria-label=Close] svg');
        const shape=icon.getBBox(),style=getComputedStyle(button),mask=getComputedStyle(dialog.previousElementSibling,'::after');
        return shape.width>0&&shape.height>0&&style.backgroundColor!=='rgba(0, 0, 0, 0)'&&style.color!==style.backgroundColor&&mask.backgroundColor!=='rgba(0, 0, 0, 0)';
      })()`));
      await snapshot('shared-sdk-dialog');
      const appearance=(await api('/api/appearance/bootstrap')).value;
      const themeCases=['light','dark'].map(mode=>({id:`builtin.${mode}`,mode}));
      const wallpaper=appearance.themes.find(theme=>theme.pack.wallpaper)?.pack.wallpaper;
      assert.ok(wallpaper,'材质验收必须使用实际壁纸，避免半透明未生效的假阳性');
      for(const mode of ['light','dark']) for(const material of ['clear','translucent','immersive']) {
        const id=`acceptance.original-${mode}-${material}`;
        const scheme=mode==='light'?{accent:'#276749',background:'#f4ede3',foreground:'#29342c',contrast:60}
          :{accent:'#94bf85',background:'#17251f',foreground:'#e1efdf',contrast:60};
        const current=(await api('/api/appearance/bootstrap')).value;
        await post('/api/appearance/themes',{expectedRevision:current.revision,pack:{schemaVersion:1,id,name:`Original UI ${mode} ${material}`,
          schemePolicy:mode,schemes:{[mode]:scheme},material,wallpaper}});
        themeCases.push({id,mode,material});
      }
      const themeResults=[];
      for(const {id,mode,material} of themeCases) {
        const current=(await api('/api/appearance/bootstrap')).value;
        await post('/api/appearance/activate',{themeId:id,expectedRevision:current.revision});
        await waitFor(()=>page.evaluate(`document.documentElement.dataset.magiThemeId===${JSON.stringify(id)}`),'Magi 应用当前皮肤与材质');
        await waitFor(()=>frame.evaluate(`document.documentElement.dataset.theme===${JSON.stringify(mode)}`),'原页面接收真实主题变更');
        const expectedColors=await page.evaluate(`['--magi-canvas','--background'].map(name=>getComputedStyle(document.documentElement).getPropertyValue(name).trim())`);
        await waitFor(()=>frame.evaluate(`JSON.stringify(['--magi-canvas','--background'].map(name=>getComputedStyle(document.documentElement).getPropertyValue(name).trim()))===${JSON.stringify(JSON.stringify(expectedColors))}`),'原页面接收本次实际颜色与材质，不能仅按深浅模式推断完成');
        assert.ok(await frame.evaluate(`(() => {const style=getComputedStyle(document.querySelector('[data-testid=sdk-confirm]'));return style.backgroundColor!=='rgba(0, 0, 0, 0)'&&style.color!==style.backgroundColor;})()`));
        const contrast=await frame.evaluate(`(() => {
          const context=document.createElement('canvas').getContext('2d',{willReadFrequently:true});
          const pixel=color=>{context.clearRect(0,0,1,1);context.fillStyle=color;context.fillRect(0,0,1,1);return [...context.getImageData(0,0,1,1).data];};
          const luminance=rgb=>rgb.slice(0,3).map(value=>value/255).map(value=>value<=0.04045?value/12.92:((value+0.055)/1.055)**2.4).reduce((sum,value,index)=>sum+value*[0.2126,0.7152,0.0722][index],0);
          return [...document.querySelectorAll('[data-theme-contrast]')].map(element=>{
            const style=getComputedStyle(element),foreground=pixel(style.color),background=pixel(style.backgroundColor);
            const values=[luminance(foreground),luminance(background)].sort((a,b)=>a-b);
            return {kind:element.dataset.themeContrast,foreground,background,ratio:(values[1]+0.05)/(values[0]+0.05)};
          });
        })()`);
        themeResults.push({id,contrast});
        reporter.check(`${id} 原包语义色支持反色按钮、次级操作和层级背景，无专属 CSS 覆盖`,
          contrast.length===3&&contrast.every(item=>item.foreground[3]===255&&item.background[3]===255&&item.ratio>=4.5),JSON.stringify(contrast));
        if(material==='immersive') await snapshot(`original-ui-${mode}-${material}`);
      }
      await writeFile(join(output,'original-theme-contrast.json'),JSON.stringify(themeResults,null,2));
      reporter.check('公共 SDK 在原页面保留文档的同时随 Magi 切换深浅主题，主操作持续可见',await frame.evaluate('window.__clientInstance')===clientInstance);
      const currentAppearance=(await api('/api/appearance/bootstrap')).value;
      await post('/api/appearance/activate',{themeId:appearance.activeThemeId,expectedRevision:currentAppearance.revision});
      for (const narrow of [false,true]) {
        if(narrow)await page.call('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false});
        reporter.check(`${narrow?'390px':'桌面'} 公共 SDK 对话框和操作完整处于当前页面内`,await frame.evaluate(`(() => {
          const dialog=document.querySelector('[role=dialog]').getBoundingClientRect(),button=document.querySelector('[data-testid=sdk-confirm]').getBoundingClientRect();
          return dialog.left>=0&&dialog.top>=0&&dialog.right<=innerWidth&&dialog.bottom<=innerHeight&&button.width>0&&button.height>0&&document.documentElement.scrollWidth<=innerWidth;
        })()`));
        if(clipAuditSource) {
          const audit=await frame.evaluate(`(${clipAuditSource})({viewport:true})`);
          await writeFile(join(output,`shared-sdk-clip-${narrow?'390':'desktop'}.json`),JSON.stringify(audit,null,2));
          reporter.check(`${narrow?'390px':'桌面'} 公共 SDK 对话框阴影无裁切`,audit.cut===0,JSON.stringify(audit));
        }
        if(narrow)await snapshot('shared-sdk-dialog-390');
      }
      await page.call('Emulation.clearDeviceMetricsOverride');
      await frame.evaluate(`document.querySelector('[data-testid=sdk-confirm]').click();true`);
      await waitFor(()=>frame.evaluate(`!document.querySelector('[role=dialog]')&&document.getElementById('receipt')?.textContent==='sdk-confirmed'`),'官方 Button 回调完成操作并关闭对话框');
      reporter.check('交互后间接取用公共 SDK 的官方 Modal 与 Button 正常展示并完成回调，无组件改写',true);
      await frame.evaluate(`window.__startCall('cancel-init',false);window.__startCall('cancel-request',true);window.__startCall('remaining',false);true`);
      const callStates = async () => (await post(`/api/plugins/${id}/routes`,{method:'GET',path:'/call-state'})).body;
      await waitFor(async()=>Object.values(await callStates()).filter(value=>value==='running').length===3,'三个真实在途请求');
      await frame.evaluate(`window.__calls['cancel-init'].controller.abort();window.__calls['cancel-request'].controller.abort();true`);
      await waitFor(()=>frame.evaluate(`window.__calls['cancel-init'].state==='AbortError'&&window.__calls['cancel-request'].state==='AbortError'`),'原页面收到两种 fetch 的取消');
      await waitFor(async()=>{const states=await callStates();return states['cancel-init']==='cancelled'&&states['cancel-request']==='cancelled'&&states.remaining==='running';},'daemon 取消指定后台路由且保留其他调用');
      reporter.check('fetch(init.signal) 与 fetch(Request.signal) 取消到达真实后台，只关闭各自请求',true);
      await post(`/api/plugins/${id}/routes`,{method:'POST',path:'/complete-call',body:'remaining'});
      await waitFor(()=>frame.evaluate(`window.__calls.remaining.state==='completed:remaining'`),'未取消请求正常完成');
      reporter.check('并发请求取消后其他业务仍收到真实结果',true);
      const binary=await frame.evaluate(`(async()=>{
        const bytes=Uint8Array.from({length:2*1024*1024},(_,i)=>i%256);
        const response=await fetch('/binary-echo',{method:'POST',body:new Blob([bytes],{type:'application/octet-stream'})});
        const restored=new Uint8Array(await response.arrayBuffer());
        const form=new FormData();form.append('title','中文');form.append('file',new File([bytes.subarray(0,512)],'original.bin'));
        const request=new Request(new URL('/binary-echo',document.baseURI),{method:'POST',body:form});
        const expected=new Uint8Array(await request.clone().arrayBuffer());
        const multipart=await fetch(request),actual=new Uint8Array(await multipart.arrayBuffer());
        return {status:response.status,bytes:restored.length,same:restored.length===bytes.length&&restored.every((v,i)=>v===bytes[i]),
          type:response.headers.get('content-type'),filename:response.headers.get('content-disposition'),reported:response.headers.get('x-original-bytes'),
          cookie:response.headers.get('set-cookie'),length:response.headers.get('content-length'),
          multipart:actual.length===expected.length&&actual.every((v,i)=>v===expected[i]),
          boundary:multipart.headers.get('content-type')===request.headers.get('content-type')};
      })()`);
      reporter.check('真实原包页面以 Blob 上传 2 MiB 原字节并沿同一路由取回，下载名及类型保留',
        binary.status===201&&binary.bytes===2*1024*1024&&binary.same&&binary.type==='application/octet-stream'
          &&binary.reported===String(binary.bytes)&&binary.filename==='attachment; filename="original.bin"',JSON.stringify(binary));
      reporter.check('真实 Chromium 的 FormData 文件和边界完整通过，Cookie 与传输长度不进入可信页面',
        binary.multipart&&binary.boundary&&binary.cookie===null&&binary.length===null,JSON.stringify(binary));
      const media=await frame.evaluate(`(async()=>{
        const response=await fetch('/binary-image'),blob=await response.blob(),url=URL.createObjectURL(blob);
        const image=new Image();image.src=url;document.body.append(image);
        try {await image.decode();return {type:blob.type,bytes:blob.size,width:image.naturalWidth,height:image.naturalHeight,source:response.headers.get('x-image-source')};}
        finally{image.remove();URL.revokeObjectURL(url);}
      })()`);
      reporter.check('Fetch 风格原包路由返回的 PNG 保持原字节并由真实 Chromium 解码展示',
        media.type==='image/png'&&media.bytes===68&&media.width===1&&media.height===1&&media.source==='original-route',JSON.stringify(media));
      const vector=await frame.evaluate(`(async()=>{
        const response=await fetch('/vector-image'),blob=await response.blob(),url=URL.createObjectURL(blob);
        const image=new Image();image.src=url;document.body.append(image);
        try {await image.decode();return {type:blob.type,width:image.naturalWidth,height:image.naturalHeight};}
        finally{image.remove();URL.revokeObjectURL(url);}
      })()`);
      reporter.check('原生页面从业务路由取得 SVG 并通过自己的 Blob URL 展示，无需改写为 PNG',
        vector.type==='image/svg+xml'&&vector.width===12&&vector.height===9,JSON.stringify(vector));
      const methods=await frame.evaluate(`Promise.all(['HEAD','OPTIONS'].map(async method=>{
        const response=await fetch('/http-method',{method});return {method,status:response.status,body:await response.text(),seen:response.headers.get('x-original-method'),allow:response.headers.get('allow')};
      }))`);
      reporter.check('HEAD 与 OPTIONS 通过正式协议到达真实处理器，204 响应无伪造正文',
        methods.every(result=>result.status===204&&result.body===''&&result.seen===result.method&&result.allow==='GET, HEAD, OPTIONS'),JSON.stringify(methods));
      const rejected=[];
      for(const payload of [{bodyBase64:'?'},{bodyBase64:'AA==',body:{}},
        {bodyBase64:Buffer.alloc(2*1024*1024+1).toString('base64')}]) {
        const result=await api(`/api/plugins/${id}/routes`,{method:'POST',path:'/binary-echo',...payload});
        rejected.push({status:result.status,message:result.value.message});
      }
      reporter.check('非法、冲突及超限字节在 daemon 进入原包业务前被拒绝，运行实例保持可用',
        rejected.every(result=>result.status===400&&result.message)
          &&(await api('/api/plugins')).value.plugins.find(plugin=>plugin.manifest.id===id).runtime?.state==='running',JSON.stringify(rejected));
      await writeFile(join(output,'route-media.json'),JSON.stringify({binary,media,vector,methods,rejected},null,2));
      const imageData = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZ1cAAAAASUVORK5CYII=';
      const dshReceipt = await frame.evaluate(`(async()=>{
        const bytes=Uint8Array.from(atob(${JSON.stringify(imageData)}),c=>c.charCodeAt(0));
        return window.__dshAdd(new File([bytes],'dsh-original.png',{type:'image/png'}));
      })()`);
      reporter.check('DSH 原同步业务经通用调用链转换后等待真实 InputArea 回执', dshReceipt.accepted===true && dshReceipt.ids.includes(dshReceipt.id)
        && await page.evaluate(`document.querySelectorAll('.image-tray-thumb').length===1`),JSON.stringify(dshReceipt));
      const dshRemoved=await frame.evaluate(`window.__dshRemove(${JSON.stringify(dshReceipt.id)})`);
      reporter.check('DSH 原移除调用等待宿主回执后释放文档草稿身份', dshRemoved.removed===true && !dshRemoved.ids.includes(dshReceipt.id)
        && await page.evaluate(`document.querySelectorAll('.image-tray-thumb').length===0`));
      const add = (count) => frame.evaluate(`(async()=>{
        const bytes=Uint8Array.from(atob(${JSON.stringify(imageData)}),c=>c.charCodeAt(0));
        return await window.__composer.addImages(Array.from({length:${count}},(_,i)=>new File([bytes], 'native-'+i+'.png',{type:'image/png'})));
      })()`);
      const receipt = await add(1);
      reporter.check('原生插件等待真实 InputArea 挂载回执，图片保持未发送草稿', receipt.accepted === true && receipt.ids.length === 1
        && await page.evaluate(`document.querySelectorAll('.image-tray-thumb').length===1`), JSON.stringify(receipt));
      const refused = await add(5);
      reporter.check('超出容量的图片批次拒绝且不部分挂载', refused.accepted === false && refused.ids.length === 0
        && await page.evaluate(`document.querySelectorAll('.image-tray-thumb').length===1`));
      const removed = await frame.evaluate(`window.__composer.removeImage(${JSON.stringify(receipt.ids[0])})`);
      reporter.check('原生插件只移除已接纳的自有图片，输入框真实状态同步', removed.removed === true
        && await page.evaluate(`document.querySelectorAll('.image-tray-thumb').length===0`));
      // 各格式必须通过真实 Chromium 解码，不用 PNG 字节冒充其他 MIME。
      const sharp = createRequire(join(compat, 'package.json'))('sharp');
      const formats = await browserImageFixtures(sharp);
      for (const [extension, mime, bytes, width, height] of formats) {
        const attached = await frame.evaluate(`(async()=>{
          const bytes=Uint8Array.from(atob(${JSON.stringify(bytes.toString('base64'))}),c=>c.charCodeAt(0));
          return window.__composer.addImages([new File([bytes],${JSON.stringify('native.' + extension)},{type:${JSON.stringify(mime)}})]);
        })()`);
        await waitFor(() => page.evaluate(`document.querySelector('.image-tray-thumb img')?.naturalWidth > 0`), `${extension} 真实缩略图`);
        reporter.check(`${extension.toUpperCase()} 原始格式经真实 Chromium 解码并挂载草稿`, attached.accepted && attached.ids.length === 1);
        if (extension === 'svg') await snapshot('composer-svg');
        const before = (await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).value.canonicalTurns ?? [];
        const lastSeq = Math.max(0,...before.map(turn=>turn.turnSeq));
        await waitFor(() => page.evaluate(`Boolean(document.querySelector('[data-testid="input-send-button"]:enabled'))`), '图片发送按钮可用');
        await ui.click('[data-testid="input-send-button"]');
        const turn = await waitFor(async()=>{
          const turns=(await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).value.canonicalTurns ?? [];
          return turns.find(turn=>turn.turnSeq>lastSeq && ['completed','failed','cancelled','blocked'].includes(turn.status));
        }, `${extension} 图片送达 DSH 模型`,45_000);
        reporter.check(`${extension.toUpperCase()} 草稿经真实发送和 daemon 到达 DSH 适配器，原图保留在消息中`, turn.status==='completed'
          && turn.items.some(item=>item.content===`DSH_IMAGE:${width}x${height}`)
          && turn.items.some(item=>item.metadata?.images?.some(image=>image.dataUrl===`data:${mime};base64,${bytes.toString('base64')}`)), JSON.stringify(turn));
        await waitFor(() => page.evaluate(`document.querySelectorAll('.image-tray-thumb').length===0`), '发送后草稿清空');
      }
      const invalidBatch = await frame.evaluate(`(async()=>{
        const valid=Uint8Array.from(atob(${JSON.stringify(imageData)}),c=>c.charCodeAt(0));
        try { await window.__composer.addImages([new File([valid],'valid.png',{type:'image/png'}),new File(['broken'],'broken.png',{type:'image/png'})]); return false; }
        catch { return true; }
      })()`);
      reporter.check('真实解码失败的整批图片被拒绝且不留下部分草稿', invalidBatch
        && await page.evaluate(`document.querySelectorAll('.image-tray-thumb').length===0`));
    }
    if (key === 'second') reporter.check('插件隔离存储跨面板保留界面缓存且不泄露宿主键', await frame.evaluate(`localStorage.getItem('panel-cache')==='first' && sessionStorage.getItem('panel-session')==='same-client' && localStorage.getItem('magi-ui-private-probe')===null`));
    await frame.evaluate(`document.getElementById('action').click(); true`);
    await waitFor(() => frame.evaluate(`document.getElementById('receipt')?.textContent===${JSON.stringify(`${key}:${key.toUpperCase()}`)}`), '原生业务回执');
    reporter.check(`右栏 ${key} 原生页面执行真实后端动作并保留鉴权头`, true);
    await snapshot(`panel-${key}`);
    if (key === 'first') await frame.evaluate(`document.getElementById('next-panel').click(); true`);
  }
  await page.call('Emulation.setDeviceMetricsOverride', {width:390,height:844,deviceScaleFactor:1,mobile:false});
  reporter.check('390px 右栏原生面板无横向溢出', await page.evaluate('document.documentElement.scrollWidth<=innerWidth') && await frame.evaluate('document.documentElement.scrollWidth<=innerWidth'));
  await snapshot('panel-390');
  await page.call('Emulation.clearDeviceMetricsOverride');
  const settings = (await api(`/api/plugins/${id}/settings`)).value;
  await post(`/api/plugins/${id}/settings`, { expectedVersion: settings.version, value: { startupUrl: gateUrl } }, 'PUT');
  await post(`/api/plugins/${id}/deactivate`);
  await waitFor(async () => !await ui.exists(`iframe[data-plugin-id="${id}"]`), '停用回收右栏面板');
  reporter.check('停用后右栏不保留可调用的旧插件 iframe', true);
  const stoppedStream = await api(`/api/plugins/${id}/routes`, { method:'POST', path:'/__magi_compat/streams', body:{action:'open',request:{method:'GET',path:'/panel-events'}} });
  reporter.check('停用后事件连接沿同一 daemon 准入拒绝重连', stoppedStream.status === 409);
  frame?.close(); frame = null;
  holdStart = true;
  await post(`/api/plugins/${id}/activate`);
  const stateSelector = `[data-testid="plugin-view-runtime"][data-plugin-id="${id}"]`;
  await waitFor(() => heldStarts.size > 0, '后台等待真实异步初始化');
  await waitFor(() => ui.exists(stateSelector), '共享页面宿主显示启动状态');
  if (!await visible(stateSelector)) {
    if (!await visible('.right-pane')) await ui.click('.header-right-pane-btn');
    await ui.click('.right-pane-add-tab');
    await waitFor(() => ui.exists(entry), '启动中仍可选择插件面板');
    await ui.click(entry);
  }
  await waitFor(() => visible(stateSelector), '当前插件面板展示启动反馈');
  reporter.check('后台未就绪时保留真实加载反馈且不挂载原包文档',
    await page.evaluate(`document.querySelector(${JSON.stringify(stateSelector)}).getAttribute('aria-busy')==='true' && !document.querySelector('iframe[data-plugin-id="${id}"]')`));
  await snapshot('panel-starting');
  holdStart = false;
  for (const response of heldStarts) response.end('ready');
  await waitFor(() => ui.exists(`iframe[data-plugin-id="${id}"]`), '后台就绪后自动挂载新文档');
  const url = await page.evaluate(`document.querySelector('iframe[data-plugin-id="${id}"]').src`);
  frame = await waitFor(() => connectTarget(cdpPort, target => (target.type === 'iframe' || target.type === 'page') && target.url === url, '重启原包文档', errors), '重启原包文档');
  await waitFor(() => frame.evaluate('Boolean(window.__clientInstance)'), '重新初始化原包客户端');
  await waitFor(() => frame.evaluate(`document.getElementById('pulse')?.textContent==='1'`), '新文档自动恢复后端事件');
  reporter.check('后台恢复后自动创建新客户端，旧文档不会继续调用', await frame.evaluate('window.__clientInstance') !== clientInstance);
  await snapshot('panel-restarted');
  await post(`/api/plugins/${id}/deactivate`);
  await waitFor(async () => !await ui.exists(`iframe[data-plugin-id="${id}"]`), '再次停用回收客户端');
  frame.close(); frame = null;
}

async function verifySessionDirectory(session) {
  const install = async (allowed) => {
    const id = `acceptance.directory-${allowed ? 'allowed' : 'denied'}`;
    const folder = join(work, id);
    await mkdir(join(folder, 'extension'), { recursive: true });
    const manifest = { sdkVersion: 2, id, version: '1.0.0', name: id, description: '', dataSchemaVersion: 1,
      extension: { hooks: [] }, settingsSchema: {type:'object'}, permissions: [{ kind: 'extension', targets: ['full-trust'] },
        ...(allowed ? [{ kind: 'sessionRead', targets: ['directory'] }] : [])],
      contributions: { commands: [{ id: 'status', title: 'Status', description: '' }], views: [{ id: 'settings', title: 'Settings', entry: 'ui/settings.html', placements: ['settings'] }] },
    };
    await mkdir(join(folder, 'ui'));
    await writeFile(join(folder, 'ui/settings.html'), '<!doctype html><meta charset="utf-8"><p>Directory acceptance</p>');
    await writeFile(join(folder, 'manifest.json'), JSON.stringify(manifest));
    await writeFile(join(folder, 'extension/index.mjs'), `export default async api => {
      await api.command('status', async()=>({content:'ready'}));
      await api.route('/directory',async input=>{
        try { return {body:await api.session.directory(input.body ?? {})}; }
        catch(error){return {status:403,body:{kind:error.kind,message:error.message}};}
      });
      // 就绪前也必须允许当前宿主实例经独立授权读取，不依赖活动 Turn。
      ${allowed ? 'await api.session.directory({limit:1});' : ''}
    };`);
    const archive = join(work, `${id}.zip`); packPlugin(folder, archive);
    const payload = { source: `local:${id}.zip`, archiveBase64: (await readFile(archive)).toString('base64') };
    const preview = await post('/api/plugins/preview', payload);
    await post('/api/plugins/install', { ...payload, expectedDigest: preview.digest });
    await post(`/api/plugins/${id}/enable`);
    await post(`/api/plugins/${id}/authorize`, { grants: manifest.permissions });
    await post(`/api/plugins/${id}/activate`);
    await waitFor(async () => (await api('/api/plugins')).value.plugins.find(p=>p.manifest.id===id)?.runtime?.state==='running', id);
    return id;
  };
  const allowed = await install(true), denied = await install(false);
  const read = (id, body = {}) => post(`/api/plugins/${id}/routes`, { method: 'POST', path: '/directory', body });
  const first = (await read(allowed, {limit:1})).body;
  reporter.check('随包 Node 的协商会话目录读取 daemon 当前身份和最小元数据', first.sessions.length===1
    && first.sessions.every(entry=>Object.keys(entry).sort().join(',')==='createdAt,sessionId,title,updatedAt,workspaceId'));
  const secondSession = (await post('/api/session/materialize', {scope:'personal'})).sessionId;
  await post('/api/session/rename', {sessionId:session,name:'Directory live metadata'});
  const all = (await read(allowed)).body;
  reporter.check('会话创建和改名直接投影当前 daemon，无第二份会话目录', all.sessions.some(entry=>entry.sessionId===secondSession)
    && all.sessions.find(entry=>entry.sessionId===session)?.title==='Directory live metadata');
  const paged = (await read(allowed, {limit:1})).body;
  const rest = (await read(allowed, {afterSessionId:paged.nextAfterSessionId})).body;
  reporter.check('真实宿主分页游标稳定且末页无游标', paged.hasMore && rest.sessions.length>0
    && !rest.sessions.some(entry=>entry.sessionId===paged.sessions[0].sessionId) && !rest.hasMore && rest.nextAfterSessionId===null);
  const missingGrant = await read(denied);
  reporter.check('完全信任插件缺少会话目录独立授权时仍被 daemon 拒绝', missingGrant.status===403 && missingGrant.body.kind==='PermissionDenied');
  const invalid = await read(allowed, {limit:201});
  reporter.check('会话目录超限请求明确失败', invalid.status===403 && invalid.body.message.includes('InvalidParams'));
  await post(`/api/plugins/${allowed}/authorize`, {grants:[{kind:'extension',targets:['full-trust']}]});
  const revoked = await api(`/api/plugins/${allowed}/routes`, {method:'POST',path:'/directory',body:{}});
  reporter.check('撤销会话目录授权后旧插件路由不可继续读取', revoked.status===409 || revoked.status===403);
  for (const id of [allowed, denied]) {
    const item = (await api('/api/plugins')).value.plugins.find((plugin) => plugin.manifest.id === id);
    if (item?.active) await post(`/api/plugins/${id}/deactivate`);
    if (item?.enabled) await post(`/api/plugins/${id}/disable`);
  }
}

async function verifySessionMessages() {
  const session=(await post('/api/session/materialize',{scope:'personal'})).sessionId;
  const source=(await installed()).manifest.contributions.modelSources[0].id;
  await post('/api/settings/orchestrator/session/save',{scope:'personal',sessionId:session,config:{sourceId:`plugin/${pluginId}/${source}`,model:'fixture-model',reasoningEffort:null}});
  const command=(await installed()).manifest.contributions.commands.find(command=>command.title==='message').id;
  const turns=async()=>(await api(`/api/messages?scope=personal&sessionId=${session}`)).value.canonicalTurns??[];
  const queue=async()=>(await api(`/api/session/queue?sessionId=${session}`)).value.queuedTurns;
  const waitTurn=id=>waitFor(async()=>(await turns()).find(turn=>turn.turnId===id&&['completed','failed','cancelled','blocked'].includes(turn.status)),'消息命令终态',45000).catch(async error=>{await writeFile(join(output,'message-failure-turns.json'),JSON.stringify(await turns(),null,2));throw error;});
  let serial=0;
  const invoke=async(input,wait=true)=>{
    const turn=await post('/api/session/turn',{scope:'personal',sessionId:session,text:JSON.stringify(input),pluginCommandId:`plugin/${pluginId}/${command}`,requestId:`message-command-${++serial}`,userMessageId:`message-command-user-${serial}`});
    return wait ? waitTurn(turn.turnId) : turn;
  };
  const context={id:'context-result',delivery:'context',text:'MESSAGE_CONTEXT_RESULT'};
  const commandTurn=await invoke(context);
  assert.equal(commandTurn.status,'completed',JSON.stringify(commandTurn));
  const pending=(await queue()).find(input=>input.pluginMessage?.messageId===context.id);
  reporter.check('原包同步 inject 等待 daemon 入库，重复身份只留一条且不请求模型',Boolean(pending)
    && (await queue()).length===1 && (await turns()).length===1 && !commandTurn.modelSteps?.length,JSON.stringify({pending,turn:commandTurn}));
  const failed=await invoke({id:'business-failed',delivery:'context',text:'MUST_NOT_DELIVER',fail:true});
  reporter.check('失败的原包业务不提交临时消息',failed.status==='failed' && !(await queue()).some(input=>input.pluginMessage?.messageId==='business-failed'));
  const cancelled=await invoke({id:'cancelled-message',delivery:'context',text:'MUST_NOT_DELIVER',wait:true},false);
  await waitFor(()=>readFile(join(work,'state/personal-sessions',session,'message-waiting'),'utf8').then(value=>value==='cancelled-message',()=>false),'消息业务等待取消');
  await post('/api/session/interrupt',{sessionId:session});
  await waitTurn(cancelled.turnId);
  reporter.check('停止调用回收临时消息，不将迟到结果入库或唤醒模型',!(await queue()).some(input=>input.pluginMessage?.messageId==='cancelled-message'));
  const edit=await api('/api/session/queue/update',{sessionId:session,queueId:pending.queueId,expectedRevision:pending.revision,request:{scope:'personal',sessionId:session,text:'forged'}});
  reporter.check('插件消息不能借用户编辑入口改写来源',edit.status===409,JSON.stringify(edit));
  await openConversation(session);
  await waitFor(()=>page.evaluate('Boolean(document.querySelector(".ia-queue-item"))'),'持久消息队列');
  reporter.check('队列展示插件来源与等待模型状态，并保留取消操作',await page.evaluate(`(()=>{const row=document.querySelector('.ia-queue-item');return row.innerText.includes('等待下一次请求')&&!row.querySelector('.ia-queue-guide')&&!row.querySelector('[aria-label="编辑消息"]')&&Boolean(row.querySelector('[aria-label="删除消息"]'));})()`));
  await snapshot('message-context-pending');
  page.close();
  await quitApp();
  await waitFor(async()=>!await fetch(origin+'/health').then(()=>true,()=>false),'消息持久化后退出');
  app=startApp();
  page=await waitFor(()=>connectAppPage(cdpPort,daemonPort,errors),'消息持久化后重新启动',60000);ui=pageTools(page);
  await waitFor(()=>page.evaluate(`Boolean(document.documentElement.dataset.magiThemeId && document.querySelector('[data-testid=sidebar-settings]'))`),'消息重启后工作台初始化');
  await waitFor(async()=>(await installed())?.runtime?.state==='running','恢复插件宿主',120000);
  reporter.check('应用重启恢复相同待处理消息且不会自行请求模型',(await queue()).some(input=>input.queueId===pending.queueId&&input.revision===pending.revision)
    && (await turns()).length===3);
  const user=await post('/api/session/turn',{scope:'personal',sessionId:session,text:'READ_MESSAGE_CONTEXT',requestId:'message-context-consumer',userMessageId:'message-context-consumer-user'});
  const consumed=await waitTurn(user.turnId);
  const received=JSON.parse((await post(`/api/plugins/${pluginId}/routes`,{method:'GET',path:'/skill-request'})).body.request);
  const modelText=JSON.stringify(received.messages);
  reporter.check('下一次模型请求原子领取上下文，原文、来源和 Input 消费均可追踪',consumed.status==='completed'
    && consumed.items.some(item=>item.metadata?.pluginMessage?.messageId===context.id&&item.content===context.text)
    && consumed.modelSteps.some(step=>step.inputRequestIds?.includes(pending.requestId))
    && modelText.includes(context.text)&&modelText.includes('Plugin-provided message')&&(await queue()).length===0,JSON.stringify({turn:consumed,request:received}));
  for(const delivery of ['steer','followup']) {
    const id='wake-'+delivery;
    const result=await invoke({id,delivery,text:'MESSAGE_WAKE_'+delivery});
    const reply=await waitFor(async()=>(await turns()).find(turn=>turn.items.some(item=>item.metadata?.pluginMessage?.messageId===id)&&['completed','failed'].includes(turn.status)),'插件结果唤醒模型',45000);
    reporter.check(`${delivery} 沿原生持久队列启动真实模型 Task`,result.status==='completed'&&reply.status==='completed'&&reply.modelSteps?.length>0
      && reply.items.find(item=>item.metadata?.pluginMessage?.messageId===id).metadata.pluginMessage.sourceTurnId===result.turnId,JSON.stringify(reply));
  }
  const tool=await post('/api/session/turn',{scope:'personal',sessionId:session,text:'MESSAGE_TOOL_CHECK',requestId:'message-tool-check',userMessageId:'message-tool-check-user'});
  const toolTurn=await waitTurn(tool.turnId);
  reporter.check('运行中的原包工具将上下文交给同一 Turn 下一模型步骤，不另启轮次',toolTurn.status==='completed'
    && toolTurn.items.some(item=>item.metadata?.pluginMessage?.messageId==='tool-result-message')&&toolTurn.modelSteps.length>=2,JSON.stringify(toolTurn));
  const request=JSON.parse((await post(`/api/plugins/${pluginId}/routes`,{method:'GET',path:'/skill-request'})).body.request);
  reporter.check('工具回注与历史重建均保留插件来源，已消费上下文不重复交付',request.messages.filter(message=>message.content.some(block=>block.type==='text'&&block.text.startsWith('[Plugin-provided message;')&&block.text.endsWith(context.text))).length===1
    && JSON.stringify(request.messages).includes('MESSAGE_TOOL_RESULT')&&JSON.stringify(request.messages).includes('Plugin-provided message'),JSON.stringify(request.messages));
  const scheduled=await post(`/api/plugins/${pluginId}/jobs`,{commandId:command,sessionId:session,
    text:JSON.stringify({id:'job-wake-denied',delivery:'steer',text:'MUST_NOT_START_MODEL'}),runAt:Date.now(),intervalMs:null,
    budget:{maxModelCalls:0,maxOutputTokens:64,timeoutMs:5000}});
  const outcome=await waitFor(async()=>{
    const job=(await api(`/api/plugins/${pluginId}/jobs`)).value.find(job=>job.id===scheduled.id);
    return job?.lastOutcome&&job;
  },'计划作业的会话唤醒预算门禁');
  reporter.check('计划作业不能借会话消息绕过签发的模型预算',outcome.lastOutcome==='failed'
    && !(await turns()).some(turn=>turn.items.some(item=>item.metadata?.pluginMessage?.messageId==='job-wake-denied'))
    && !(await queue()).some(input=>input.pluginMessage?.messageId==='job-wake-denied'),JSON.stringify(outcome));
  await openConversation(session);
  await waitFor(()=>page.evaluate('Boolean(document.querySelector("[data-source=plugin]"))'),'插件消息正文与来源展示');
  reporter.check('插件消息原文完整显示并明确来源，不冒充用户气泡',await page.evaluate(`document.querySelectorAll('[data-source=plugin]').length>=4&&[...document.querySelectorAll('[data-source=plugin]')].every(node=>node.querySelector('.plugin-message-source')&&!node.classList.contains('user'))`));
  reporter.check('插件消息消费状态来自真实模型 Step，正文不会仅显示为接纳成功',await page.evaluate(`document.querySelectorAll('[data-source=plugin] [data-input-delivery=used]').length>=4`));
  await snapshot('message-provenance');
  await page.call('Emulation.setDeviceMetricsOverride',{width:390,height:844,deviceScaleFactor:1,mobile:false});
  reporter.check('390px 插件消息正文、来源与消费回执无横向溢出',await page.evaluate('document.documentElement.scrollWidth<=innerWidth'));
  await snapshot('message-provenance-390');
  await page.call('Emulation.clearDeviceMetricsOverride');
  const cancellable=await invoke({id:'user-cancelled-context',delivery:'context',text:'CANCEL_FROM_QUEUE'});
  assert.equal(cancellable.status,'completed');
  await waitFor(()=>ui.exists('.ia-queue-item [aria-label="删除消息"]'),'待处理插件上下文取消按钮');
  await ui.click('.ia-queue-item [aria-label="删除消息"]');
  await waitFor(async()=>(await queue()).length===0,'用户取消已入库的插件消息');
  reporter.check('用户沿原生队列取消已入库上下文，不发起模型或创建伪造用户消息',!(await turns()).some(turn=>turn.items.some(item=>item.metadata?.pluginMessage?.messageId==='user-cancelled-context')));

  await writeFile(join(output,'session-messages.json'),JSON.stringify({session,turns:await turns(),request},null,2));
}

async function verifyToolOutcomes() {
  const session=(await post('/api/session/materialize',{scope:'personal'})).sessionId;
  const source=(await installed()).manifest.contributions.modelSources[0].id;
  await post('/api/settings/orchestrator/session/save',{scope:'personal',sessionId:session,config:{sourceId:`plugin/${pluginId}/${source}`,model:'fixture-model',reasoningEffort:null}});
  const turns=async()=>(await api(`/api/messages?scope=personal&sessionId=${session}`)).value.canonicalTurns??[];
  const outcomes=async()=>(await post(`/api/plugins/${pluginId}/routes`,{method:'GET',path:'/tool-outcomes'})).body;
  const waitTurn=id=>waitFor(async()=>(await turns()).find(turn=>turn.turnId===id&&['completed','failed','cancelled','blocked'].includes(turn.status)),'工具结果结算',45000);
  const invoke=kind=>post('/api/session/turn',{scope:'personal',sessionId:session,text:'TOOL_OUTCOME_CHECK:'+JSON.stringify({kind,id:kind}),requestId:'tool-outcome-'+kind,userMessageId:'tool-outcome-user-'+kind});
  for(const kind of ['continue','finish','failed']) {
    const accepted=await invoke(kind), turn=await waitTurn(accepted.turnId);
    const tool=turn.items.find(item=>item.kind==='tool_call');
    const context=turn.items.find(item=>item.metadata?.pluginMessage?.messageId==='outcome-context-'+kind);
    const final=turn.items.find(item=>item.metadata?.assistantOutputKind==='final');
    const request=JSON.parse((await post(`/api/plugins/${pluginId}/routes`,{method:'GET',path:'/skill-request'})).body.request);
    await writeFile(join(output,`tool-outcome-${kind}.json`),JSON.stringify({turn,request},null,2));
    reporter.check(`${kind} 工具沿真实管线保留结构化业务结果与补充上下文`,turn.status==='completed'
      && tool?.status===(kind==='failed'?'failed':'completed')
      && context?.content==='DEFERRED_CONTEXT:'+kind
      && context.metadata.pluginMessage.sourceKind==='runtime-tool-context',JSON.stringify(turn));
    if(kind==='finish') {
      reporter.check('成功工具直接交付最终业务原文，保留完成来源且不请求第二次模型',final?.content==='ACTUAL_BUSINESS_RESULT:finish'
        && final.metadata.pluginToolCompletion?.[0]?.pluginId===pluginId && !final.metadata.modelStepId
        && !final.metadata.providerContext && turn.modelSteps.length===1,JSON.stringify(turn));
    } else {
      reporter.check(`${kind} 的上下文交给原生下一模型步骤，失败不能请求结束本轮`,turn.modelSteps.length===2
        && JSON.stringify(request.messages).includes('DEFERRED_CONTEXT:'+kind)
        && !final?.metadata?.pluginToolCompletion
        && (kind!=='failed'||JSON.stringify(tool.tool.result.error).includes('ACTUAL_BUSINESS_FAILURE:failed')),JSON.stringify({turn,request}));
      if(kind==='failed') reporter.check('结束本轮工具留下的补充上下文仍进入后续真实模型请求',JSON.stringify(request.messages).includes('DEFERRED_CONTEXT:finish'),JSON.stringify(request.messages));
    }
    assert.equal((await outcomes()).filter(item=>item.id===kind).length,1,'业务只执行一次');
  }
  const cancel=await invoke('cancel');
  await waitFor(async()=>(await outcomes()).some(item=>item.id==='cancel'),'工具进入等待取消的业务');
  await post('/api/session/interrupt',{sessionId:session});
  const stopped=await waitTurn(cancel.turnId);
  reporter.check('取消中止真实工具，临时补充上下文与结束意图均不能迟到提交',stopped.status==='cancelled'
    && !stopped.items.some(item=>item.metadata?.assistantOutputKind==='final'||item.metadata?.pluginMessage?.messageId==='outcome-context-cancel'),JSON.stringify(stopped));
  const before=await turns();
  await writeFile(join(output,'tool-outcomes.json'),JSON.stringify({session,turns:before,executions:await outcomes()},null,2));
  page.close();
  await quitApp();
  await waitFor(async()=>!await fetch(origin+'/health').then(()=>true,()=>false),'工具结算后退出');
  app=startApp();
  page=await waitFor(()=>connectAppPage(cdpPort,daemonPort,errors),'工具结算后重新启动',60000);ui=pageTools(page);
  await waitFor(()=>page.evaluate(`Boolean(document.documentElement.dataset.magiThemeId && document.querySelector('[data-testid=sidebar-settings]'))`),'重启后工作台初始化');
  await waitFor(async()=>(await installed())?.runtime?.state==='running','恢复结果验收插件',120000);
  const after=await turns();
  reporter.check('应用重启保留业务最终原文与来源，不重放工具或另起模型',after.length===before.length
    && after.some(turn=>turn.items.some(item=>item.metadata?.assistantOutputKind==='final'&&item.content==='ACTUAL_BUSINESS_RESULT:finish'&&item.metadata.pluginToolCompletion?.[0]?.pluginId===pluginId))
    && (await outcomes()).length===0,JSON.stringify(after));
  await openConversation(session);
  await waitFor(()=>page.evaluate('document.body.innerText.includes("ACTUAL_BUSINESS_RESULT:finish")'),'工具原文恢复展示');
  await snapshot('tool-outcomes-restored');
}

async function verifyCommandExecution(session) {
  const command = (await installed()).manifest.contributions.commands.find(command => command.title === 'directory').id;
  let serial = 0;
  const invoke = async (scope, text = '') => {
    const turn = await post('/api/session/turn', {...scope, text, pluginCommandId:`plugin/${pluginId}/${command}`,
      requestId:`directory-${++serial}`,userMessageId:`directory-user-${serial}`});
    const result = await waitFor(async () => (await api(`/api/messages?scope=${scope.scope}&sessionId=${encodeURIComponent(scope.sessionId)}${scope.workspaceId ? '&workspaceId='+encodeURIComponent(scope.workspaceId) : ''}`)).value.canonicalTurns
      ?.find(item => item.turnId === turn.turnId && ['completed','failed','cancelled','blocked'].includes(item.status)), '执行目录命令的 canonical 终态');
    assert.equal(result.status, 'completed', JSON.stringify(result));
    assert.ok(turn.rootTaskId && turn.actionTaskId === turn.rootTaskId, '插件命令必须接纳真实 root Task');
    const user = result.items.find(item=>item.kind==='user_message');
    assert.equal(user?.metadata.executionProfile, 'task');
    assert.equal(user?.worker?.taskId, turn.rootTaskId);
    assert.equal(result.modelSteps?.length ?? 0, 0, '命令不能隐式调用主模型');
    const query = new URLSearchParams({scope:scope.scope,sessionId:scope.sessionId,
      ...(scope.workspaceId ? {workspaceId:scope.workspaceId} : {})});
    const projection = await waitFor(async () => {
      const response = await api(`/api/agent-runs/projection/${turn.rootTaskId}?${query}`);
      return response.status===200 && response.value.root_task?.status==='completed' && response.value;
    }, '命令 TaskStore 完成并结算');
    reporter.check('插件命令沿原生 Task 接纳和结算，结果绑定原任务且不请求主模型', Boolean(projection));
    return result;
  };
  const personal = join(work, 'state/personal-sessions', session);
  await mkdir(personal, {recursive:true}); await writeFile(join(personal,'context-check.txt'),'PERSONAL');
  await invoke({scope:'personal',sessionId:session});
  const saved = JSON.parse(await readFile(join(personal,'command-result.json'),'utf8'));
  reporter.check('原包命令在 daemon 自有个人目录完成实际读写，不使用宿主进程目录', saved.id===session
    && saved.cwd===await realpath(personal) && saved.marker==='PERSONAL' && saved.label==='changed');
  const paths = [join(work,'first-source'),join(work,'second-source')];
  for (const [index,path] of paths.entries()) { await mkdir(path); await writeFile(join(path,'context-check.txt'),`SOURCE_${index}`); }
  const {workspace} = await post('/api/workspaces/create',{name:'Command context',sourceFolders:paths.map((path,index)=>({path,sourceFolderId:`draft-${index}`})),primarySourceFolderId:'draft-0'});
  const projectSession = (await post('/api/session/materialize',{scope:'workspace',workspaceId:workspace.workspaceId})).sessionId;
  const scope = {scope:'workspace',workspaceId:workspace.workspaceId,sessionId:projectSession,sourceFolderId:workspace.sourceFolders[1].sourceFolderId};
  await invoke(scope);
  const project = JSON.parse(await readFile(join(paths[1],'command-result.json'),'utf8'));
  reporter.check('多目录项目命令使用接纳时选定的源目录和会话身份，结果写入对应目录', project.cwd===await realpath(paths[1])
    && project.id===projectSession && project.marker==='SOURCE_1' && !await readFile(join(paths[0],'command-result.json')).then(()=>true,()=>false));
  const waiting = await post('/api/session/turn',{scope:'personal',sessionId:session,text:'wait',pluginCommandId:`plugin/${pluginId}/${command}`,requestId:'directory-cancel',userMessageId:'directory-cancel-user'});
  await waitFor(()=>readFile(join(personal,'command-waiting'),'utf8').then(value=>value===session,()=>false),'命令已进入真实业务');
  await post('/api/session/interrupt',{sessionId:session});
  await waitFor(()=>readFile(join(personal,'command-cancelled'),'utf8').then(value=>value===session,()=>false),'停止传递给原包业务信号');
  const stopped = await waitFor(async () => (await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).value.canonicalTurns
    ?.find(item=>item.turnId===waiting.turnId && item.status==='cancelled'),'取消的 canonical 终态');
  reporter.check('停止命令取消真实业务调用，迟到返回不能覆盖已取消的 Turn', stopped.status==='cancelled');
  const resume = await api('/api/session/turn',{scope:'personal',sessionId:session,text:'继续',resume:true,
    requestId:'directory-cancel-resume',userMessageId:'directory-cancel-resume-user'});
  reporter.check('已停止的命令不伪装为模型可继续任务，也不隐式重放业务副作用', [400,409].includes(resume.status));

  await invoke({scope:'personal',sessionId:session});
  reporter.check('取消后显式再次执行使用新调用并恢复真实文件结果', JSON.parse(await readFile(join(personal,'command-result.json'),'utf8')).id===session);
  await rename(paths[1], paths[1]+'-removed');
  const rejected = await api('/api/session/turn',{...scope,text:'',pluginCommandId:`plugin/${pluginId}/${command}`,requestId:'directory-missing',userMessageId:'directory-missing-user'});
  const missing = rejected.status===200 ? await waitFor(async () => (await api(`/api/messages?scope=workspace&workspaceId=${scope.workspaceId}&sessionId=${projectSession}`)).value.canonicalTurns
    ?.find(item=>item.turnId===rejected.value.turnId && ['failed','completed','cancelled','blocked'].includes(item.status)), '执行准备拒绝失效目录') : null;
  reporter.check('项目源目录失效在业务开始前拒绝，不回落主要目录或个人目录', (missing ? missing.status==='failed'
    : [400,409].includes(rejected.status))
    && !await readFile(join(paths[0],'command-result.json')).then(()=>true,()=>false), JSON.stringify({httpStatus:rejected.status,turnStatus:missing?.status}));
  await writeFile(join(output,'execution-context.json'), JSON.stringify({personal:saved,project,cancelledTurn:waiting.turnId,
    missingDirectory:{httpStatus:rejected.status,turnId:missing?.turnId,status:missing?.status}},null,2));
}

try {
  reporter.check('验收端口未被已有实例使用',
    !await fetch(`${origin}/health`).then(() => true, () => false)
    && !await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false)
    && !await fetch(`http://127.0.0.1:${cdpPort+2}/json/list`).then(() => true, () => false));
  const archive = await prepare();
  app = startApp();
  page = await waitFor(() => connectAppPage(cdpPort, daemonPort, errors), '真实 Magi Renderer', 60_000);
  ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid="sidebar-settings"]'), '工作台');
  reporter.check('真实 Magi Main 托管 daemon 与工作台启动', (await api('/health')).status === 200);
  const preview = await post('/api/plugins/preview', archive);
  await post('/api/plugins/install', { ...archive, expectedDigest: preview.digest });
  await settingsTab('plugins');
  const row = `[data-testid="plugin-row"][data-plugin-id="${pluginId}"]`;
  await waitFor(() => ui.exists(row), '已安装插件');
  await ui.click(row);
  await waitFor(() => ui.exists('[data-action="activate"]'), '授权激活按钮');
  reporter.check('图片存储独立权限在授权前可见', (await ui.text('[data-testid="plugin-card"]')).includes('图片存储'));
  await ui.click('[data-action="activate"]');
  await waitFor(async () => {
    const plugin = await installed();
    if (plugin?.runtime?.state === 'failed') throw new Error(plugin.runtime.reason);
    return plugin?.runtime?.state === 'running';
  }, 'DSH 宿主就绪', 30_000);
  reporter.check('通过界面授权后运行真实 Cordis 插件', true);
  const peer = await post(`/api/plugins/${pluginId}/routes`, { method: 'GET', path: '/cordis-peer' });
  reporter.check('实际宿主的 Cordis ESM、CJS 子路径与 Service 均使用同一隔离版本',
    peer.status === 200 && peer.body.esm === '4.0.5-alpha.2' && peer.body.cjs === peer.body.esm && peer.body.service === peer.body.esm, JSON.stringify(peer));
  const imageRoundtrip = await post(`/api/plugins/${pluginId}/routes`, {method:'GET',path:'/image-roundtrip'});
  reporter.check('真实 Node 附件经 daemon 持久图片服务保存与读取，身份和文件名采用保存事实', imageRoundtrip.status===200
    && imageRoundtrip.body.sameBytes && imageRoundtrip.body.ref.mediaType==='image/png' && imageRoundtrip.body.ref.name==='capture.png'
    && imageRoundtrip.body.ref.attachmentId.startsWith('plugin-image'), JSON.stringify(imageRoundtrip));
  const backgroundImage=await waitFor(async()=>{
    const result=await post(`/api/plugins/${pluginId}/routes`,{method:'GET',path:'/background-image'});
    if(result.body?.error) throw new Error(result.body.error);
    return result.body?.ref && result.body;
  },'初始化结束后的后台图片保存');
  reporter.check('初始化结束后的后台事件经独立实例授权分块保存并读回超过 3 MiB 图片',backgroundImage.sameBytes && backgroundImage.ref.bytes>3*1024*1024);
  await waitFor(async () => (await installed()).manifest.contributions.tools.length === 1, '就绪后的晚注册工具');
  reporter.check('晚于初始化窗口的工具进入 daemon 动态目录', true);
  // 复现设置读取撞上生命周期排空；真实 iframe 应展示错误，不能留下未捕获拒绝或允许盲写。
  let settingsRead;
  const stopIntercept = page.on('Fetch.requestPaused', (event) => { settingsRead = event.requestId; });
  await page.call('Fetch.enable', { patterns: [{ urlPattern: `${origin}/api/plugins/${pluginId}/settings`, requestStage: 'Request' }] });
  await settingsTab(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists('.settings-frame iframe'), '插件设置视图');
  const frameUrl = await page.evaluate('document.querySelector(".settings-frame iframe").src');
    frame = await waitFor(() => connectTarget(cdpPort, (target) => (target.type === 'iframe' || target.type === 'page') && target.url === frameUrl, '插件隔离视图', errors), '插件隔离视图');
  const evaluateFrame = (expression) => frame.evaluate(expression);
  await waitFor(() => settingsRead, '设置读取请求');
  reporter.check('设置读取完成前禁止保存', await evaluateFrame('document.getElementById("save").disabled'));
  await page.call('Fetch.fulfillRequest', { requestId: settingsRead, responseCode: 409,
    responseHeaders: [{ name: 'content-type', value: 'application/json' }],
    body: Buffer.from(JSON.stringify({ error_code: 'CONFLICT', message: 'RUNTIME_SETTINGS_DRAINING' })).toString('base64') });
  await waitFor(() => evaluateFrame('document.getElementById("status").textContent.includes("RUNTIME_SETTINGS_DRAINING")'), '设置读取错误反馈');
  reporter.check('设置读取失败显示原因且禁止覆盖配置', await evaluateFrame('document.getElementById("save").disabled')
    && (await api(`/api/plugins/${pluginId}/settings`)).value.version === 0 && errors.length === 0, errors.join('\n'));
  await page.call('Fetch.disable'); stopIntercept();
  await page.evaluate('document.querySelector(".settings-frame iframe").src += ""; true');
  await waitFor(() => evaluateFrame('Boolean(document.getElementById("save")) && !document.getElementById("save").disabled'), '重新加载设置恢复');
  await waitFor(() => evaluateFrame('Boolean(document.getElementById("f-label"))'), '动态 schema 设置字段');
  reporter.check('首次打开设置页展示与插件执行一致的 schema 默认值',
    await evaluateFrame('document.getElementById("f-label").value === "initial" && document.getElementById("f-enabled").checked'));
  await evaluateFrame('document.getElementById("f-label").value="changed"; document.getElementById("f-enabled").checked=true; document.getElementById("save").click(); true');
  await waitFor(async () => (await api(`/api/plugins/${pluginId}/settings`)).value.value?.label === 'changed', '界面配置保存');
  reporter.check('共享页面宿主经同一 settingsScope 保存动态 schema 配置', true);
  await snapshot('settings-desktop');
  frame.close(); frame = null;
  const plugin = await installed();
  const session = (await post('/api/session/materialize', { scope: 'personal' })).sessionId;
  await verifySessionDirectory(session);
  const command = plugin.manifest.contributions.commands.find(command=>command.title==='report').id;
  const source = plugin.manifest.contributions.modelSources[0].id;
  const waitTurn = (id) => waitFor(async () => {
    const turns = (await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).value.canonicalTurns;
    return turns?.find((turn) => turn.turnId === id && ['completed', 'failed', 'cancelled', 'blocked'].includes(turn.status));
  }, `Turn ${id}`, 45_000).catch(async error=>{
    await writeFile(join(output,'turn-timeout.json'),JSON.stringify({
      turnId:id, messages:await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`),
      modelRequest:await api(`/api/plugins/${pluginId}/routes`,{method:'GET',path:'/skill-request'}),
      plugin:await installed(),
    },null,2));
    throw error;
  });
  const cmd = await post('/api/session/turn', { scope: 'personal', sessionId: session, pluginCommandId: `plugin/${pluginId}/${command}`, text: '', requestId: 'runtime-command', userMessageId: 'runtime-command-user' });
  const cmdTurn = await waitTurn(cmd.turnId);
  reporter.check('配置重载后的命令沿 canonical Turn 返回新值', cmdTurn.status === 'completed' && JSON.stringify(cmdTurn).includes('DSH_REPORT:changed'), JSON.stringify(cmdTurn));
  await verifyCommandExecution(session);
  await verifySessionMessages();
  await verifyToolOutcomes();
  await post('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session, config: { sourceId: `plugin/${pluginId}/${source}`, model: 'fixture-model', reasoningEffort: null } });
  const toolTurn = await post('/api/session/turn',{scope:'personal',sessionId:session,text:'DIRECTORY_TOOL_CHECK',requestId:'directory-tool',userMessageId:'directory-tool-user'});
  const toolResult = await waitTurn(toolTurn.turnId);
  const toolRequest = (await post(`/api/plugins/${pluginId}/routes`,{method:'GET',path:'/skill-request'})).body;
  await writeFile(join(output,'tool-execution-request.json'),JSON.stringify(toolRequest,null,2));
  reporter.check('模型调用的原包工具读取同一真实会话目录和文件，并沿原生账本结算', toolResult.status==='completed'
    && JSON.stringify(toolResult).includes('TOOL_DIRECTORY:') && JSON.stringify(toolResult).includes('PERSONAL'), JSON.stringify(toolResult));
  const skillEntries = (await activeSkills());
  const runtimeSkill = skillEntries.find(skill => skill.pluginId === pluginId && skill.title === 'runtime-inspection');
  reporter.check('配置替换后的技能进入 daemon 统一目录且正文保持当前配置', runtimeSkill
    && plugin.manifest.contributions.skills[0].instruction === 'RUNTIME_SKILL_BODY:changed');
  const skillTurn = await post('/api/session/turn', { scope: 'personal', sessionId: session, text: 'inspect runtime', skillName: runtimeSkill.id, requestId: 'runtime-skill', userMessageId: 'runtime-skill-user' });
  const skillResult = await waitTurn(skillTurn.turnId);
  const skillRequest = await post(`/api/plugins/${pluginId}/routes`, {method:'GET',path:'/skill-request'});
  reporter.check('技能沿 canonical Turn 和真实模型请求注入当前原文', skillResult.status === 'completed'
    && skillRequest.body.request.includes('RUNTIME_SKILL_BODY:changed') && !skillRequest.body.request.includes('RUNTIME_SKILL_BODY:initial'));
  await verifyOriginalSkill(session, waitTurn);
  const chat = await post('/api/session/turn', { scope: 'personal', sessionId: session, text: 'runtime model check', requestId: 'runtime-model', userMessageId: 'runtime-model-user' });
  const modelTurn = await waitTurn(chat.turnId);
  reporter.check('主对话通过插件模型桥与真实 DSH 适配器完成流式回复', modelTurn.status === 'completed' && JSON.stringify(modelTurn).includes('DSH_MODEL:changed'), JSON.stringify(modelTurn));
  await openConversation(session);
  await waitFor(() => page.evaluate('document.body.innerText.includes("DSH_MODEL:changed")'), '回执显示');
  await snapshot('conversation');
  await settingsTab(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists('[data-testid="plugin-page-header"] input[role=switch]'), '插件启用开关');
  await ui.click('[data-testid="plugin-page-header"] input[role=switch]');
  await waitFor(async () => !(await installed()).active, '停用');
  reporter.check('界面停用回收动态工具、技能和模型来源', (await installed()).manifest.contributions.tools.length === 0
    && !(await activeSkills()).some(skill=>skill.pluginId===pluginId)
    && !(await api('/api/settings/models/catalog')).value.groups.some((group) => group.pluginId === pluginId));
  await waitFor(() => ui.exists('[data-testid="plugin-page-inactive"]'), '停用提示');
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  await page.call('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: 'dark' }] });
  await snapshot('inactive-390');
  reporter.check('390px 设置页无横向溢出且保留停用状态', await page.evaluate('document.documentElement.scrollWidth <= innerWidth'));
  await page.call('Emulation.clearDeviceMetricsOverride');
  await settingsTab('plugins');
  await waitFor(() => ui.exists(row), '停用后的插件列表');
  await ui.click(row);
  await waitFor(() => ui.exists('[data-action="activate"]'), '重新激活按钮');
  await ui.click('[data-action="activate"]');
  await waitFor(async () => (await installed()).runtime?.state === 'running', '重新激活');
  reporter.check('重新激活恢复持久配置并建立新运行目录', (await api(`/api/plugins/${pluginId}/settings`)).value.value.label === 'changed');
  const unsupported = join(work, 'unsupported');
  await mkdir(unsupported);
  await writeFile(join(unsupported, 'package.json'), JSON.stringify({ name: 'unsupported-dsh', version: '1.0.0', type: 'module', main: 'index.js', dsh: { bundle: {} } }));
  await writeFile(join(unsupported, 'index.js'), 'export function apply(ctx) { ctx.on("session/event", () => {}); }');
  await assert.rejects(run(node, [join(compat, 'cli.mjs'), 'prepare', 'dsh', unsupported, '--out', join(work, 'unsupported-out'), '--integrity', 'local-acceptance'], { timeout: 30_000 }), /session\/event/);
  reporter.check('未捕获的必需宿主事件仍被明确拒绝', true);
  await verifyNativePanels(session);
  const rendererLogErrors = appLogs.flat().join('').split('\n').filter((line) => /Uncaught|可信 Renderer 控制台错误/.test(line));
  reporter.check('主 Renderer 与插件 iframe 没有运行时异常', errors.length === 0 && rendererLogErrors.length === 0,
    [...errors, ...rendererLogErrors].join('\n'));
  console.log(reporter.summary());
} catch (error) {
  if (ui) await snapshot('failure').catch(() => {});
  console.error(error);
  process.exitCode = 1;
} finally {
  for (const response of heldStarts) response.end('closed');
  startupGate?.closeAllConnections();
  startupGate?.close();
  await writeFile(join(output, 'results.json'), JSON.stringify(reporter.results, null, 2));
  if (app) {
    await quitApp().catch(async error=>{console.error(error);process.exitCode=1;await app.stop();});
    await writeFile(join(output, 'electron.log'), appLogs.flat().join(''));
  }
  frame?.close();
  page?.close();
  await rm(work, { recursive: true, force: true });
}
