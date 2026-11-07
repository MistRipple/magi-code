// 从 macOS 目录包启动真实 Magi Main、daemon 与随包 Node/兼容层。
// 独立状态根；DSH 夹具不读用户凭据，模型回执由夹具适配器本地生成。
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { createRequire } from 'node:module';
import { browserImageFixtures } from './lib/plugin-image-fixtures.mjs';
import { packPlugin } from './compat/translate/pack.mjs';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const resources = join(appRoot, 'Contents/Resources');
const node = join(resources, 'daemon/node/node');
const compat = join(resources, 'daemon/compat');
const cdpPort = Number(process.env.MAGI_PLUGIN_RUNTIME_CDP_PORT || 9307);
const daemonPort = Number(process.env.MAGI_PLUGIN_RUNTIME_PORT || 38937);
const origin = `http://127.0.0.1:${daemonPort}`;
const output = join(root, 'target/plugin-acceptance/runtime');
const work = await mkdtemp(join(tmpdir(), 'magi-runtime-live-'));
const run = promisify(execFile);
const reporter = createReporter();
const errors = [];
let app, page, frame, ui, pluginId;
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
const visible = (selector) => page.evaluate(`Boolean(document.querySelector(${JSON.stringify(selector)})?.getClientRects().length)`);
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

async function prepare() {
  const upstream = join(work, 'upstream');
  await mkdir(upstream);
  const deps = JSON.parse(await readFile(join(compat, 'package.json'), 'utf8')).dependencies;
  await writeFile(join(upstream, 'package.json'), JSON.stringify({
    name: 'magi-runtime-acceptance', displayName: '运行时兼容验收', version: '1.0.0', type: 'module', main: 'index.js',
    dsh: { bundle: { patch: './cordis.patch.yml' } },
    peerDependencies: { '@deepseek-ai/cordis': deps['@deepseek-ai/cordis'], '@deepseek-ai/dsh-llm': deps['@deepseek-ai/dsh-llm'] },
    dependencies: { '@deepseek-ai/schemastery': deps['@deepseek-ai/schemastery'] },
  }));
  await writeFile(join(upstream, 'index.js'), `
import Schema from '@deepseek-ai/schemastery';
import { LlmAdapter } from '@deepseek-ai/dsh-llm';
export const name = 'magi-runtime-acceptance';
export const inject = ['commands', 'tools', 'llm', 'attachments', 'webServer', 'sessions'];
export const Config = Schema.object({ label: Schema.string().default('initial'), enabled: Schema.boolean().default(true) });
export function apply(ctx, config) {
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
  ctx.webServer.register({path:'/image-roundtrip',resource:async()=>{
    const data=Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZ1cAAAAASUVORK5CYII=','base64');
    const ref=await ctx.attachments.saveImage({data,mediaType:'image/png',name:'capture.png'});
    const saved=await ctx.attachments.readImage({...ref,mediaType:'image/jpeg',name:'forged.jpg'});
    return Response.json({sameBytes:saved.data.equals(data),ref:saved.ref});
  }});
  if (config.enabled) ctx.effect(() => {
    const timer = setTimeout(() => ctx.tools.register({ name: 'late-read', description: '读取验收配置', parameters: { type: 'object' }, readOnly: true,
      output: { schema: { type: 'string' }, render: (_args, value) => [{ type: 'text', text: value }] }, execute: async () => config.label }), 1700);
    return () => clearTimeout(timer);
  });
  class Adapter extends LlmAdapter {
    async listModels(provider) { return [{ provider, id: 'fixture-model', name: 'Runtime Fixture' }]; }
    async resolveModel(provider, id) { return {provider,id,name:'Runtime Fixture',inputModalities:['text','image']}; }
    async *stream(request) {
      request.signal?.throwIfAborted();
      let text = 'DSH_MODEL:' + config.label;
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
  return { source: 'local:runtime-acceptance.zip', archiveBase64: (await readFile(archive)).toString('base64') };
}

async function verifyNativePanels(session) {
  const upstream = join(work, 'native-panels');
  await mkdir(upstream);
  await writeFile(join(upstream, 'package.json'), JSON.stringify({ name: 'magi-native-panels', version: '1.0.0', type: 'module', main: 'index.js',
    dsh: { bundle: {} }, exports: { '.': './index.js', './client': './client.js' } }));
  await writeFile(join(upstream, 'index.js'), `
export const inject = ['webServer'];
export function apply(ctx) {
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
}`);
  await writeFile(join(upstream, 'client.js'), `
window.__ModuleLoader__.load({id:'native-panels',factory(require){
  const React=require('react'), h=React.createElement;
  function attach(c,scope,ids) { const input=c.conversation.input.for(scope); return input.addAttachments(ids); }
  return { async apply(ctx) {
    await Promise.resolve();
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
          React.useEffect(()=>{
            const source=new EventSource('/panel-events');
            source.addEventListener('pulse',event=>setPulse(JSON.parse(event.data).live));
            return ()=>source.close();
          },[]);
          return h('main',{},h('h1',{id:'identity'},key),
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
  await page.call('Page.navigate', {url:`${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}`});
  await waitFor(() => ui.exists('.header-right-pane-btn'), '工作台顶栏');
  await page.evaluate(`localStorage.setItem('magi-ui-private-probe','private'); true`);
  if (!await visible('.right-pane')) await ui.click('.header-right-pane-btn');
  await waitFor(() => ui.exists('.right-pane-add-tab'), '右栏入口');
  await ui.click('.right-pane-add-tab');
  const first = plugin.manifest.contributions.views.find((view) => view.title === 'first');
  const entry = `.launcher-tool[data-plugin-id="${id}"][data-contribution-id="${first.id}"]`;
  await waitFor(() => ui.exists(entry), '第一个原生面板'); await ui.click(entry);
  for (const key of ['first', 'second']) {
    frame?.close();
    const selector = `iframe[data-plugin-id="${id}"]`;
    await waitFor(() => ui.exists(selector), '右栏 iframe');
    const url = await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).src`);
    frame = await waitFor(() => connectTarget(cdpPort, (target) => (target.type === 'iframe' || target.type === 'page') && target.url === url, '右栏原生页面', errors), '右栏原生页面');
    await waitFor(() => frame.evaluate(`document.getElementById('identity')?.textContent===${JSON.stringify(key)}`), '正确的页面身份');
    await waitFor(() => frame.evaluate(`document.getElementById('pulse')?.textContent==='1'`), 'SSE 事件通过真实 daemon 与 iframe 到达上游组件');
    reporter.check(`右栏 ${key} 持续接收 SSE，切换后只有一个活跃上游连接`, true);
    if (key === 'first') {
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
        await waitFor(() => page.evaluate(`!document.querySelector('[data-testid="input-send-button"]')?.disabled`), '图片发送按钮可用');
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
  await post(`/api/plugins/${id}/deactivate`);
  await waitFor(async () => !await ui.exists(`iframe[data-plugin-id="${id}"]`), '停用回收右栏面板');
  reporter.check('停用后右栏不保留可调用的旧插件 iframe', true);
  const stoppedStream = await api(`/api/plugins/${id}/routes`, { method:'POST', path:'/__magi_compat/streams', body:{action:'open',request:{method:'GET',path:'/panel-events'}} });
  reporter.check('停用后事件连接沿同一 daemon 准入拒绝重连', stoppedStream.status === 409);
  frame?.close(); frame = null;
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

try {
  reporter.check('验收端口未被已有实例使用',
    !await fetch(`${origin}/health`).then(() => true, () => false)
    && !await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false));
  const archive = await prepare();
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    env: { MAGI_STATE_ROOT: join(work, 'state'), MAGI_DESKTOP_DAEMON_PORT: String(daemonPort), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' },
  });
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
  reporter.check('生成设置页经原视图桥保存动态 schema 配置', true);
  await snapshot('settings-desktop');
  frame.close(); frame = null;
  const plugin = await installed();
  const session = (await post('/api/session/materialize', { scope: 'personal' })).sessionId;
  await verifySessionDirectory(session);
  const command = plugin.manifest.contributions.commands[0].id;
  const source = plugin.manifest.contributions.modelSources[0].id;
  const waitTurn = (id) => waitFor(async () => {
    const turns = (await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).value.canonicalTurns;
    return turns?.find((turn) => turn.turnId === id && ['completed', 'failed', 'cancelled', 'blocked'].includes(turn.status));
  }, `Turn ${id}`, 45_000);
  const cmd = await post('/api/session/turn', { scope: 'personal', sessionId: session, pluginCommandId: `plugin/${pluginId}/${command}`, text: '', requestId: 'runtime-command', userMessageId: 'runtime-command-user' });
  const cmdTurn = await waitTurn(cmd.turnId);
  reporter.check('配置重载后的命令沿 canonical Turn 返回新值', cmdTurn.status === 'completed' && JSON.stringify(cmdTurn).includes('DSH_REPORT:changed'), JSON.stringify(cmdTurn));
  await post('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session, config: { sourceId: `plugin/${pluginId}/${source}`, model: 'fixture-model', reasoningEffort: null } });
  const chat = await post('/api/session/turn', { scope: 'personal', sessionId: session, text: 'runtime model check', requestId: 'runtime-model', userMessageId: 'runtime-model-user' });
  const modelTurn = await waitTurn(chat.turnId);
  reporter.check('主对话通过插件模型桥与真实 DSH 适配器完成流式回复', modelTurn.status === 'completed' && JSON.stringify(modelTurn).includes('DSH_MODEL:changed'), JSON.stringify(modelTurn));
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}` });
  await waitFor(() => page.evaluate('document.body.innerText.includes("DSH_MODEL:changed")'), '回执显示');
  await snapshot('conversation');
  await settingsTab(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists('[data-testid="plugin-page-header"] input[role=switch]'), '插件启用开关');
  await ui.click('[data-testid="plugin-page-header"] input[role=switch]');
  await waitFor(async () => !(await installed()).active, '停用');
  reporter.check('界面停用回收动态工具和模型来源', (await installed()).manifest.contributions.tools.length === 0
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
  const rendererLogErrors = app.log.join('').split('\n').filter((line) => /Uncaught|可信 Renderer 控制台错误/.test(line));
  reporter.check('主 Renderer 与插件 iframe 没有运行时异常', errors.length === 0 && rendererLogErrors.length === 0,
    [...errors, ...rendererLogErrors].join('\n'));
  console.log(reporter.summary());
} catch (error) {
  if (ui) await snapshot('failure').catch(() => {});
  console.error(error);
  process.exitCode = 1;
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(reporter.results, null, 2));
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  frame?.close();
  page?.close();
  await rm(work, { recursive: true, force: true });
}
