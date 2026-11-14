import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import Schema from '@deepseek-ai/schemastery';
import { LlmAdapter } from '@deepseek-ai/dsh-llm';
import { SettingsConflictError } from '@deepseek-ai/dsh-settings';
import sharp from 'sharp';
import { activateDsh } from './runtime/dsh-host.mjs';
import { runtimeId } from './core/identity.mjs';
import { createChunkTranslator, startOpenAiBridge } from './runtime/openai-bridge.mjs';
import { requestAttachments, saveRequestImage, withRequestImages } from './runtime/attachments.mjs';
import * as McpClient from '@deepseek-ai/dsh-mcp-client';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { browserImageFixtures, bmpEncodingFixtures } from '../lib/plugin-image-fixtures.mjs';
import { createBackendSessions } from './runtime/backend-sessions.mjs';
import { REGISTERED_SETTINGS_RESOURCE } from './runtime/registered-settings.mjs';
import { createRegisteredSettings } from './runtime/registered-settings.mjs';
import { Remote, TypertRemoteService } from '@deepseek-ai/dsh-typert-protocol';
import { REMOTE_ROUTE } from './runtime/remote.mjs';
import { readFileSync as readProfileFile } from './runtime/profile-fs.mjs';

// 只替代 daemon RPC；插件加载、依赖、effect、工具管线及模型目录运行真实上游代码。
function host(value = {}) {
  let revision = 0;
  let snapshot = { version: 0, value };
  let listener, validate;
  const state = { catalog: {}, tools: new Map(), commands: new Map(), routes: new Map(), models: new Map(), hooks: new Map(), failures: [], logs: [], outcomes: [] };
  const bind = (kind) => async (id, handler) => {
    assert.ok(state.catalog[kind].some((item) => item.id === id));
    state[kind].set(id, handler);
    return () => { if (state[kind].get(id) === handler) state[kind].delete(id); };
  };
  const api = {
    catalog: { async replace(expected, catalog) {
      assert.equal(expected, revision);
      state.catalog = structuredClone(catalog);
      return { revision: ++revision };
    } },
    tool: bind('tools'), command: bind('commands'),
    async on(name, handler) { state.hooks.set(name, handler); },
    toolResult(output, options) { state.outcomes.push({ output, options }); return output; },
    async route(path, handler) { state.routes.set(path, handler); return () => state.routes.delete(path); },
    modelSource: {
      async set(id, source) { state.models.set(id, source); },
      async clear(id) { state.models.delete(id); },
    },
    session: { async directory() { return { sessions: [{ sessionId: 'fixture-session', workspaceId: null, title: 'Fixture', createdAt: 1, updatedAt: 2 }], hasMore: false, nextAfterSessionId: null }; } },
    settings: {
      async read() { return structuredClone(snapshot); },
      async write(expected, value) {
        assert.equal(expected, snapshot.version);
        snapshot = { version: expected + 1, value: structuredClone(value) };
        return structuredClone(snapshot);
      },
      onValidate(handler) { validate = handler; },
    },
    onConfig(handler) { listener = handler; return () => { listener = undefined; }; },
    onDispose() {}, log(message) { state.logs.push(message); }, fail(error) { state.failures.push(error); },
  };
  return {
    api, state,
    async configure(value) {
      await validate(value);
      snapshot = { version: snapshot.version + 1, value };
      await listener(structuredClone(snapshot));
    },
  };
}

const options = { runtimeVersion: 2, plugin: 'fixture' };

test('现代设置以原 bundle 入口统一读取、提交和通知，npm 包名不成为第二个写入口', async t => {
  const h = host({ provider: 'original' }), events = []; let settings;
  const running = await activateDsh(h.api, { inject: ['settings'], apply(ctx) {
    settings = ctx.settings;
    ctx.on('settings/document-updated', ns => events.push(ns));
  } }, { ...options, settingsNamespace: 'original-entry' });
  t.after(() => running.dispose());
  assert.deepEqual(settings.describe().map(item => [item.ns, item.value, item.revision]), [['original-entry', { provider: 'original' }, 0]]);
  await settings.mutate('original-entry', [{ op: 'set', path: ['provider'], value: 'selected' }]);
  assert.deepEqual((await h.api.settings.read()).value, { provider: 'selected' });
  assert.deepEqual(events, ['original-entry']);
  assert.equal(settings.describe()[0].revision, 1);
  assert.throws(() => settings.mutate(options.plugin, [{ op: 'set', path: ['provider'], value: 'wrong' }]), /不能修改/);
  assert.deepEqual((await h.api.settings.read()).value, { provider: 'selected' });
});

test('现代设置拒绝旧表单的全部写入方式，保留最新配置并允许重新读取后保存', async t => {
  const h = host({ provider: 'original' }), events = []; let settings;
  const running = await activateDsh(h.api, { inject: ['settings'], apply(ctx) {
    settings = ctx.settings;
    ctx.on('settings/document-updated', (ns, revision) => events.push({ ns, revision }));
  } }, options);
  t.after(() => running.dispose());
  const stale = settings.describe()[0];
  await h.configure({ provider: 'newer', retained: true });
  events.length = 0;
  for (const write of [
    () => settings.mutate('fixture', [{ op: 'set', path: ['provider'], value: 'stale' }], stale.revision),
    () => settings.update('fixture', { provider: 'stale' }, stale.revision),
    () => settings.replace('fixture', { provider: 'stale' }, stale.revision),
  ]) {
    await assert.rejects(write, error => error instanceof SettingsConflictError
      && error.code === 'SETTINGS_CONFLICT' && error.expected === 0 && error.actual === 1);
    assert.deepEqual(await h.api.settings.read(), { version: 1, value: { provider: 'newer', retained: true } });
    assert.deepEqual(events, []);
  }
  const current = settings.describe()[0];
  await settings.mutate('fixture', [{ op: 'set', path: ['provider'], value: 'selected' }], current.revision);
  assert.deepEqual(await h.api.settings.read(), { version: 2, value: { provider: 'selected', retained: true } });
  assert.deepEqual(events, [{ ns: 'fixture', revision: 2 }]);
  await settings.update('fixture', { provider: 'updated' }, 2);
  await settings.replace('fixture', { provider: 'replaced' }, 3);
  assert.deepEqual(await h.api.settings.read(), { version: 4, value: { provider: 'replaced' } });
});

test('原页面模型业务保留角色、取消与实际用量，不把原生模型再发布成来源', async t => {
  const h = host(); let context; const requests=[]; let history=[];
  const providerContext=[{provider:'openai_chat',kind:'reasoning',data:{reasoning_text:'ORIGINAL REASONING'}}];
  h.api.model = {
    async selection() { return { sourceId:'plugin/provider/model', model:'real-model', revision:'frozen' }; },
    async invoke(input, { signal }) { requests.push(input); signal.throwIfAborted(); return {
      status:'completed', content:'REAL RESULT', thinking:'ORIGINAL REASONING', providerContext, finishReason:'stop',
      usage:{ inputTokens:15, outputTokens:3, cacheReadTokens:5, cacheReadIncludedInInput:true },
    }; },
  };
  const running = await activateDsh(h.api, { inject:['webServer','llm','agentDefaultModel'], apply(ctx) {
    context=ctx;
    ctx.webServer.register({ path:'/generate', async handler(req,res) {
      const selection=ctx.agentDefaultModel.currentSelection();
      let text='', replayState; const usage=[];
      for await(const chunk of ctx.llm.stream({ ...selection, system:'SYSTEM ORIGINAL', messages:[
        { role:'user', content:[{type:'text',text:'USER ORIGINAL'}] },
        { role:'assistant', source:{kind:'plugin'}, content:[{type:'text',text:'ASSISTANT ORIGINAL'}] },
        ...history,
      ], maxTokens:800 })) {
        if(chunk.type==='finish' && chunk.reason.kind==='error') throw new Error(chunk.reason.failure.message);
        if(chunk.type==='text-delta')text+=chunk.text;
        if(chunk.type==='usage')usage.push(chunk.usage);
        if(chunk.type==='finish')replayState=chunk.replayState;
      }
      res.setHeader('content-type','application/json');res.end(JSON.stringify({text,usage,replayState}));
    } });
  } }, { ...options, nativeModels:true });
  t.after(()=>running.dispose());
  const signal=new AbortController().signal;
  const result=await h.state.routes.get('/generate')({method:'POST',path:'/generate'}, {signal});
  assert.equal(result.body.text,'REAL RESULT',JSON.stringify(result));
  assert.deepEqual(requests[0].messages,[{role:'system',content:'SYSTEM ORIGINAL'},{role:'user',content:'USER ORIGINAL'},{role:'assistant',content:'ASSISTANT ORIGINAL'}]);
  assert.equal(requests[0].maxOutputTokens,800);
  assert.deepEqual(result.body.usage,[{inputTokens:10,outputTokens:3,cacheReadTokens:5}]);
  history=[{role:'assistant',source:{kind:'model',provider:'magi-native',model:'real-model',replayState:result.body.replayState},
    content:[{type:'reasoning',text:'ORIGINAL REASONING'},{type:'text',text:'REAL RESULT'}]}];
  await h.state.routes.get('/generate')({method:'POST',path:'/generate'},{signal});
  assert.deepEqual(requests[1].messages.at(-1),{role:'assistant',content:'REAL RESULT',providerContext});
  assert.equal(h.state.models.size,0);
  assert.throws(()=>context.agentDefaultModel.currentSelection(),/活动/);
  const controller=new AbortController();controller.abort();
  await assert.rejects(h.state.routes.get('/generate')({method:'POST',path:'/generate'},{signal:controller.signal}),/abort/i);
  assert.equal(requests.length,2);
  h.api.model.selection=async()=>{throw new Error('default model absent');};
  const unavailable=await h.state.routes.get('/generate')({method:'POST',path:'/generate'},{signal});
  assert.match(unavailable.body.error,/default model absent/);
});

test('原页面模型公开真实默认目录，流中失败和正文改写不能被翻译成正常结束', async t => {
  const h=host();let mode='failure';const requests=[];
  h.api.model={async selection(){return {model:'selected',revision:'frozen',inputModalities:['text','image']};},
    async invoke(input,{onDelta}) {
      requests.push(input);onDelta({content:'partial',thinking:''});
      if(mode==='failure') throw new Error('REAL TRANSPORT FAILURE');
      if(mode==='replace') onDelta({content:'replacement',thinking:'',replace:true});
      return {status:'completed',content:'different',finishReason:'stop'};
    }};
  const running=await activateDsh(h.api,{inject:['webServer','llm','agentDefaultModel'],apply(ctx){
    ctx.webServer.register({path:'/failure',async handler(_req,res){
      const selection=ctx.agentDefaultModel.currentSelection();
      const models=await ctx.llm.listModels(selection.provider),chunks=[];
      for await(const chunk of ctx.llm.stream({...selection,messages:[{role:'user',content:[{type:'text',text:'request'}]}]})) chunks.push(chunk);
      res.setHeader('content-type','application/json');res.end(JSON.stringify({models,chunks}));
    }});
  }},{...options,nativeModels:true});
  t.after(()=>running.dispose());
  for(mode of ['failure','replace','mismatch']) {
    const result=await h.state.routes.get('/failure')({method:'POST',path:'/failure'},{signal:new AbortController().signal});
    assert.equal(result.body.models[0].id,'selected');
    assert.deepEqual(result.body.models[0].inputModalities,['text','image']);
    assert.equal(result.body.chunks.find(chunk=>chunk.type==='text-delta').text,'partial');
    const finish=result.body.chunks.at(-1);
    assert.equal(finish.type,'finish');assert.equal(finish.reason.kind,'error');
    assert.match(finish.reason.failure.message,mode==='failure'?/REAL TRANSPORT FAILURE/:mode==='replace'?/改写正文/:/最终结果/);
  }
  assert.equal(requests.length,3,'失败不重试或重放模型请求');
});

test('原页面模型在最终结果之前交付片段，消费者退出取消同一调用', async t => {
  const h=host();let release,first,mode='complete',cancelled=false;
  const gate=new Promise(resolve=>{release=resolve;}), started=new Promise(resolve=>{first=resolve;});
  h.api.model={async selection(){return {model:'test',revision:'one'};},async invoke(_input,{signal,onDelta}) {
    onDelta({content:'first',thinking:''});
    if(mode==='cancel') {
      await new Promise(resolve=>signal.addEventListener('abort',()=>{cancelled=true;resolve();},{once:true}));
      signal.throwIfAborted();
    }
    await gate;onDelta({content:' last',thinking:''});return {status:'completed',content:'first last',finishReason:'stop'};
  }};
  const running=await activateDsh(h.api,{inject:['webServer','llm','agentDefaultModel'],apply(ctx){
    ctx.webServer.register({path:'/stream',async handler(_req,res){
      let text='';
      for await(const chunk of ctx.llm.stream({...ctx.agentDefaultModel.currentSelection(),messages:[{role:'user',content:[{type:'text',text:'hello'}]}]})) {
        if(chunk.type==='text-delta'){text+=chunk.text;first();if(mode==='cancel')break;}
      }
      res.end(text);
    }});
  }},{...options,nativeModels:true});
  t.after(()=>{release();return running.dispose();});
  let finished=false;
  const result=h.state.routes.get('/stream')({method:'POST',path:'/stream'},{signal:new AbortController().signal}).then(value=>{finished=true;return value;});
  await Promise.race([started,delay(2000).then(()=>{throw new Error('片段等待了最终结果');})]);
  assert.equal(finished,false);release();assert.equal((await result).body,'first last');
  mode='cancel';await h.state.routes.get('/stream')({method:'POST',path:'/stream'},{signal:new AbortController().signal});
  assert.equal(cancelled,true,'上游消费者结束后不能留下孤立请求');
});

test('原生模型快照保留 Magi 合同，DSH 推理元数据和关闭档位只在翻译层转换', async t => {
  const h=host(), requests=[];
  const selection={sourceId:'native-source',model:'native-model',revision:'frozen',reasoning:{
    supportedEfforts:['none','low','high'],defaultEffort:'none',transport:'openai',source:'user',
  }};
  h.api.model={async selection(){return structuredClone(selection);},async invoke(input){
    requests.push(input);return {status:'completed',content:'NATIVE RESULT',finishReason:'stop'};
  }};
  const running=await activateDsh(h.api,{inject:['webServer','llm','agentDefaultModel'],apply(ctx){
    ctx.webServer.register({path:'/reasoning',async handler(_req,res){
      const selected=ctx.agentDefaultModel.currentSelection();
      const info=await ctx.llm.resolveModelInfo(selected.provider,selected.model);
      const chunks=[];
      for await(const chunk of ctx.llm.stream({...selected,reasoningEffort:info.reasoning.defaultEffort,
        messages:[{role:'user',content:[{type:'text',text:'hello'}]}]})) chunks.push(chunk);
      res.setHeader('content-type','application/json');res.end(JSON.stringify({info,chunks}));
    }});
  }},{...options,nativeModels:true});
  t.after(()=>running.dispose());
  const response=await h.state.routes.get('/reasoning')({method:'POST',path:'/reasoning'},{signal:new AbortController().signal});
  assert.deepEqual(response.body.info.reasoning.efforts.map(effort=>effort.id),['off','low','high']);
  assert.equal(response.body.info.reasoning.defaultEffort,'off');
  assert.equal(requests.length,1,JSON.stringify(response));
  assert.deepEqual(requests[0].selection,selection,'原始选择是 daemon 的校验事实，不能回传 DSH 投影');
  assert.equal(requests[0].reasoningEffort,'none');
  assert.equal(response.body.chunks.find(chunk=>chunk.type==='text-delta').text,'NATIVE RESULT');
  const {dshReasoningOf}=await import('./runtime/reasoning.mjs');
  assert.throws(()=>dshReasoningOf({supportedEfforts:['unknown']}),/能力无效/);
  assert.throws(()=>dshReasoningOf({supportedEfforts:['low'],defaultEffort:'high'}),/不在声明/);
});

test('原页面模型保留图片、工具调用与结果、推理和生成参数，工具结果不冒充 Magi 工具执行', async t => {
  const h=host(),requests=[];
  const data=await sharp({create:{width:2,height:2,channels:3,background:'#ff0000'}}).png().toBuffer();
  const ref={attachmentId:'owned-image',mediaType:'image/png',bytes:data.length,width:2,height:2};
  h.api.images={async read(id){assert.equal(id,ref.attachmentId);return {ref,data};}};
  h.api.model={async selection(){return {model:'test',revision:'one',inputModalities:['text','image'],reasoning:{supportedEfforts:['high'],defaultEffort:'high',transport:'openai',source:'test'}};},async invoke(input){
    requests.push(input);
    return {status:'requires_tool_execution',content:'',toolCalls:[{id:'call-one',type:'function',function:{name:'business.lookup',arguments:'{"name":"sample"}'}}],finishReason:'tool_calls'};
  }};
  const history=[{role:'assistant',source:{kind:'model',provider:'magi-native',model:'test'},content:[{type:'tool-call',id:'old-call',name:'business.lookup',arguments:'{}'}]},
    {role:'tool',toolCallId:'old-call',isError:true,source:{kind:'tool',callId:'old-call'},content:[{type:'text',text:'REAL ERROR'}]}];
  const running=await activateDsh(h.api,{inject:['webServer','llm','agentDefaultModel'],apply(ctx){
    ctx.webServer.register({path:'/tools',async handler(_req,res){
      const chunks=[];
      for await(const chunk of ctx.llm.stream({...ctx.agentDefaultModel.currentSelection(),messages:[
        {role:'user',content:[{type:'text',text:'Inspect this image'},{type:'image',attachment:ref}]},...history,
      ],tools:[{name:'business.lookup',description:'Business query',parameters:{type:'object',properties:{name:{type:'string'}}}}],temperature:0.25,stop:['STOP'],reasoningEffort:'high',maxTokens:900})) chunks.push(chunk);
      res.setHeader('content-type','application/json');res.end(JSON.stringify(chunks));
    }});
  }},{...options,nativeModels:true});
  t.after(()=>running.dispose());
  const result=await h.state.routes.get('/tools')({method:'POST',path:'/tools'},{signal:new AbortController().signal});
  assert.equal(requests.length,1,JSON.stringify(result));
  const request=requests[0];
  assert.equal(request.messages[0].images[0].media_type,'image/jpeg');
  assert.equal((await sharp(Buffer.from(request.messages[0].images[0].data,'base64')).metadata()).width,2);
  assert.equal(request.tools[0].function.name,'business.lookup');
  assert.equal(request.temperature,0.25);assert.deepEqual(request.stop,['STOP']);assert.equal(request.reasoningEffort,'high');assert.equal(request.maxOutputTokens,900);
  assert.equal(request.messages[1].toolCalls[0].id,'old-call');
  assert.equal(request.messages[2].toolCallId,'old-call');assert.equal(JSON.parse(request.messages[2].content).is_error,true);
  assert.deepEqual(result.body.find(chunk=>chunk.type==='tool-call-delta'),{type:'tool-call-delta',index:0,id:'call-one',name:'business.lookup',argumentsDelta:'{"name":"sample"}'});
  assert.equal(result.body.at(-1).reason.kind,'tool-calls');assert.equal(h.state.tools.size,0);
});

test('提示贡献沿真实注册表保持顺序、插值、运行中切换和 effect 撤销，预算失败不提交部分指引', async t => {
  const h = host(); let context, release, text = '第一轮';
  const running = await activateDsh(h.api, { inject: ['systemPrompt'], apply(ctx) {
    context = ctx;
    ctx.systemPrompt.variable('value', () => text);
    ctx.systemPrompt.section({ name: 'last', order: 2, text: '{{value}}' });
    ctx.systemPrompt.section({ name: 'literal', order: 1, text: '{{literal}}', interpolate: false });
    release = ctx.systemPrompt.section({ name: 'first', order: 0, text: () => '😀'.repeat(40_000) });
  } }, { ...options, promptContributions: true });
  t.after(() => running.dispose());
  const invoke = (phase = 'round', max = 128 * 1024, signal = new AbortController().signal) =>
    h.state.hooks.get('prompt.contribute')({ phase, round: 1, budget: { maxFragmentChars: max, maxTotalChars: max } }, { signal });
  assert.deepEqual(await invoke('start'), { decision: 'continue' });
  const result = await invoke();
  assert.equal(result.patch.fragments[0].text, '😀'.repeat(40_000) + '\n\n{{literal}}\n\n第一轮');
  assert.equal(result.patch.fragments[0].stability, 'volatile');
  assert.ok(!result.patch.fragments[0].text.includes('powered by DeepSeek Harness'));
  await assert.rejects(invoke('round', 10), /未截断或提交部分指引/);
  await release(); text = '下一轮';
  assert.equal((await invoke()).patch.fragments[0].text, '{{literal}}\n\n下一轮');
  assert.throws(() => context.systemPrompt.section({ name: 'last', order: 3, text: 'duplicate' }), /already registered/);
  assert.throws(() => context.systemPrompt.section({ name: 'complete', order: 0, text: 'replace', complete: true }), /完整系统提示/);
  const invalid = context.systemPrompt.section({ name: 'async', order: 9, text: async () => 'not synchronous' });
  await assert.rejects(invoke(), /同步返回字符串/); await invalid();
  const { createScope } = await import('@deepseek-ai/dsh-scope');
  const scope = createScope(context, {});
  assert.throws(() => scope.ctx.systemPrompt.section({ name: 'scoped', order: 1, text: 'local' }), /局部指引/);
  await scope.dispose();
  const controller = new AbortController(); controller.abort();
  await assert.rejects(invoke('round', 100, controller.signal), { name: 'AbortError' });
});

test('命令与工具读取同一 daemon 调用的会话身份和执行目录，并发、配置替换与取消不串用投影', async (t) => {
  const h = host({ label: 'first' });
  let retained, crossCall;
  const running = await activateDsh(h.api, { inject: ['commands', 'tools'], apply(ctx, config) {
    const read = async (agent, signal) => {
      const session = agent.session, header = session.header;
      assert.throws(() => agent.inject({}), /插件消息必须/);
      assert.throws(() => session.append('message', {}), /尚未接入/);
      assert.throws(() => { header.cwd = '/forged'; }, TypeError);
      if (crossCall) assert.throws(() => crossCall.id, /失效|不属于/);
      await delay(5, undefined, { signal });
      retained = { agent, session, header };
      return `${config.label}:${agent.id}:${session.id}:${header.cwd}`;
    };
    ctx.commands.register({ name: 'directory', handler: async invocation => ({ kind: 'success', text: await read(invocation.agent, invocation.signal) }) });
    ctx.tools.register(tool('directory', async (_args, exec) => read(exec.agent, exec.signal)));
  } }, options);
  t.after(() => running.dispose());
  const context = (id) => ({ ...execution(), owner: { sessionId: id }, execution: { workingDirectory: `/execution/${id}`, sourceFolders: [], accessProfile: 'restricted' } });
  const command = (id) => h.state.commands.get(runtimeId('cmd', 'directory'))({}, context(id));
  const [a, b] = await Promise.all([command('one'), command('two')]);
  assert.equal(a.content, 'first:one:one:/execution/one');
  assert.equal(b.content, 'first:two:two:/execution/two');
  for (const object of Object.values(retained)) assert.throws(() => object.id, /失效/);
  crossCall = retained.agent;
  const value = await h.state.tools.get(runtimeId('tool', 'directory'))({}, context('three'));
  assert.equal(value.content[0].text, 'first:three:three:/execution/three');
  await h.configure({ label: 'second' });
  assert.equal((await command('four')).content, 'second:four:four:/execution/four');
  const controller = new AbortController();
  const pending = h.state.commands.get(runtimeId('cmd', 'directory'))({}, { ...context('cancelled'), signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, /abort/i);
  await assert.rejects(h.state.commands.get(runtimeId('cmd', 'directory'))({}, { ...context('cancelled-before-start'), signal: controller.signal }), /abort/i);
  assert.equal((await command('five')).content, 'second:five:five:/execution/five');
});

test('会话业务缺少 daemon 执行上下文时明确失败，不猜进程 cwd 或创建上游会话', async (t) => {
  const h = host();
  const running = await activateDsh(h.api, { inject: ['commands'], apply(ctx) {
    ctx.commands.register({ name: 'directory', handler: invocation => ({ kind: 'success', text: invocation.agent.session.header.cwd }) });
  } }, options);
  t.after(() => running.dispose());
  await assert.rejects(h.state.commands.get(runtimeId('cmd', 'directory'))({}, { ...execution(), owner: { sessionId: 's' } }), /未提供有效/);
});

test('技能沿真实 Cordis effect 进入同一动态目录，配置替换与停用撤销旧正文', async () => {
  const h = host();
  const Config = Schema.object({ label: Schema.string().default('first'), enabled: Schema.boolean().default(true) });
  const running = await activateDsh(h.api, { Config, inject: ['skills'], apply(ctx, config) {
    if (config.enabled) ctx.effect(function* () {
      yield ctx.skills.register({ name: 'inspect-runtime', description: 'Inspect the current runtime', content: `Original body: ${config.label}`, source: 'runtime' });
      const duplicate = ctx.skills.register({ name: 'inspect-runtime', description: 'Duplicate', content: 'Must not replace the original', source: 'runtime' });
      duplicate();
    });
    assert.throws(() => ctx.skills.register({ name: 'empty', description: 'Empty', content: '' }), /正文/);
    assert.throws(() => ctx.skills.register({ name: 'policy', description: 'Policy', content: 'body', invocation: { modelInvocable: false, userInvocable: true } }), /调用策略/);
    assert.throws(() => ctx.skills.register({ name: 'resource', description: 'Resources', content: 'body', resourceBase: { kind: 'directory', path: '/tmp' } }), /resourceBase/);
  } }, options);
  try {
    assert.deepEqual(h.state.catalog.skills, [{ id: runtimeId('skill', 'inspect-runtime'), title: 'inspect-runtime', description: 'Inspect the current runtime', instruction: 'Original body: first' }]);
    await h.configure({ label: 'second', enabled: true });
    assert.equal(h.state.catalog.skills.length, 1);
    assert.equal(h.state.catalog.skills[0].instruction, 'Original body: second');
    await h.configure({ label: 'second', enabled: false });
    assert.deepEqual(h.state.catalog.skills, []);
    await h.configure({ label: 'third', enabled: true });
    assert.equal(h.state.catalog.skills[0].instruction, 'Original body: third');
  } finally { await running.dispose(); }
  assert.deepEqual(h.state.catalog.skills, []);
  assert.deepEqual(h.state.failures, []);
});

test('设置命名空间可在配置重载后动态出现，资源授权与 CAS 仍由 daemon 裁决', async () => {
  let snapshot = { version: 0, value: null }, fail = false;
  const settings = createRegisteredSettings({ resource: {
    async read(id) { assert.equal(id, REGISTERED_SETTINGS_RESOURCE); return structuredClone(snapshot); },
    async write(id, version, value) {
      assert.equal(id, REGISTERED_SETTINGS_RESOURCE); assert.equal(version, snapshot.version);
      if (fail) throw new Error('permission revoked');
      return snapshot = { version: version + 1, value: structuredClone(value) };
    },
  } }, true);
  const cleanup = [], ctx = { effect: callback => cleanup.push(callback()) }; let retained;
  await settings.run(async () => {
    const late = settings.register(ctx, 'runtime-selected', Schema.object({ enabled: Schema.boolean().default(true) }));
    retained = late;
    await late.update({ enabled: false }); assert.equal(late.get().enabled, false);
    fail = true;
  });
  await assert.rejects(settings.run(() => settings.update('runtime-selected', { enabled: true })), /permission revoked/);
  assert.equal(settings.get('runtime-selected').enabled, false);
  for (const release of cleanup) release();
  assert.throws(() => retained.update({ enabled: true }), /注册已失效/);
  assert.equal(settings.get('runtime-selected'), undefined);
  await settings.dispose();
  const denied = createRegisteredSettings({}, false);
  assert.throws(() => denied.register(ctx, 'unapproved', Schema.object({})), /未声明/);
});

test('注册设置按已提交值通知，保留 base 继承、异步顺序、取消订阅及停用清理', async () => {
  let stored={version:0,value:null}, fail=false;
  const logs=[], cleanup=[], observed=[];
  const settings=createRegisteredSettings({resource:{
    async read(){return structuredClone(stored);},
    async write(_id,version,value){assert.equal(version,stored.version);if(fail)throw new Error('CAS rejected');return stored={version:version+1,value:structuredClone(value)};},
  },log:message=>logs.push(message)},true);
  const ctx={effect:callback=>cleanup.push(callback())};
  const schema=value=>({enabled:value.enabled??true,nested:{source:'default',...value.nested}});
  const scope=settings.register(ctx,'business',schema,{base:{enabled:false,nested:{source:'composition'}}});
  assert.deepEqual(scope.get(),{enabled:false,nested:{source:'composition'}});
  let releaseFirst;const first=new Promise(resolve=>{releaseFirst=resolve;});
  const off=scope.watch(async(next,previous)=>{observed.push([next,previous]);if(observed.length===1)await first;});
  await scope.update({nested:{edited:'one'}});await until(()=>observed.length===1);
  await scope.update({nested:{edited:'two'}});
  assert.equal(observed.length,1,'同一个异步观察器不能并发运行');
  releaseFirst();await until(()=>observed.length===2);
  assert.deepEqual(observed[1],[{enabled:false,nested:{source:'composition',edited:'two'}},{enabled:false,nested:{source:'composition',edited:'one'}}]);
  assert.deepEqual(stored.value.business,{nested:{edited:'two'}},'派生默认值和组合 base 不能写成用户覆盖');
  fail=true;await assert.rejects(scope.update({enabled:true}),/CAS rejected/);fail=false;
  assert.equal(observed.length,2,'失败提交不能发出通知');
  off();await scope.replace({});await delay(0);assert.equal(observed.length,2);
  assert.deepEqual(scope.get(),{enabled:false,nested:{source:'composition'}},'清空用户层恢复组合 base');
  const stopFault=scope.watch(()=>{throw new Error('observer failure');});
  await scope.update({enabled:true});await until(()=>logs.some(line=>line.includes('observer failure')));stopFault();
  let unblock,started=false,finished=false;
  const pending=new Promise(resolve=>{unblock=resolve;});
  const stop=scope.watch(async()=>{started=true;await pending;finished=true;});
  await scope.update({enabled:false});await until(()=>started);
  await scope.update({enabled:true});stop();
  const disposing=settings.dispose();await delay(0);assert.equal(finished,false);
  unblock();await disposing;assert.equal(finished,true);
  for(const dispose of cleanup)dispose();
  assert.throws(()=>scope.watch(()=>{}),/已失效/);
});

test('DSH 注册设置按命名空间保存到 daemon 资源，未 await 的写入在路由回执前落盘并在重建实例后恢复', async (t) => {
  let h = host({ configOnly: true });
  let stored = { version: 0, value: null }, failure, scopes, observed = [];
  const resource = {
    async read(id) { assert.equal(id, REGISTERED_SETTINGS_RESOURCE); return structuredClone(stored); },
    async write(id, expected, value) {
      assert.equal(id, REGISTERED_SETTINGS_RESOURCE); assert.equal(expected, stored.version);
      await delay(5); if (failure) throw failure;
      stored = { version: expected + 1, value: structuredClone(value) }; return structuredClone(stored);
    },
  };
  h.api.resource = resource;
  const upstream = { inject: ['settings', 'webServer'], apply(ctx) {
    const schema = value => ({ enabled: value.enabled ?? true, items: value.items ?? ['a', 'b'] });
    scopes = ['one', 'two'].map(ns => ctx.settings.register(ns, schema, { applies: 'live' }));
    ctx.on('settings/updated', (ns, value) => observed.push([ns, value]));
    ctx.webServer.register({ path: '/settings', handler: async (request, response) => {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const input = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      if (input.op === 'get') { response.end(JSON.stringify(scopes.map(scope => scope.get()))); return; }
      if (input.op === 'mutate') ctx.settings.mutate(input.ns, input.ops);
      else ctx.settings.update(input.ns, input.patch);
      response.end(JSON.stringify({ saved: true }));
    } });
  } };
  const settingsOptions = { ...options, settingsNamespaces: ['one', 'two'] };
  let running = await activateDsh(h.api, upstream, settingsOptions);
  t.after(() => running.dispose());
  const invoke = body => h.state.routes.get('/settings')({ method: 'POST', path: '/settings', body }, { signal: new AbortController().signal });
  await invoke({ ns: 'one', patch: { enabled: false } });
  assert.equal(stored.value.one.enabled, false);
  assert.equal(stored.value.two, undefined);
  assert.equal(observed[0][0], 'one');
  assert.deepEqual((await h.api.settings.read()).value, { configOnly: true }, '旧版设置不能覆盖现代 Config');
  await Promise.all([invoke({ ns: 'one', patch: { items: ['c'] } }), invoke({ ns: 'two', patch: { enabled: false } })]);
  assert.deepEqual(stored.value.one, { enabled: false, items: ['c'] });
  assert.equal(stored.value.two.enabled, false);
  await invoke({ ns: 'two', op: 'mutate', ops: [{ op: 'unset', path: ['items', '0'] }] });
  assert.deepEqual(stored.value.two.items, ['b']);
  const old = scopes[0]; await running.dispose();
  assert.throws(() => old.get(), /已失效/);
  h = host({ configOnly: true }); h.api.resource = resource;
  running = await activateDsh(h.api, upstream, settingsOptions);
  assert.deepEqual(scopes.map(scope => scope.get()), [stored.value.one, stored.value.two]);
  failure = new Error('permission revoked'); const previous = structuredClone(stored);
  await assert.rejects(invoke({ ns: 'one', patch: { enabled: true } }), /permission revoked/);
  assert.deepEqual(stored, previous);
  assert.deepEqual(scopes[0].get(), previous.value.one, '写失败不能发布乐观快照');
  failure = null;
  await assert.rejects(invoke({ ns: 'two', op: 'mutate', ops: [{ op: 'set', path: ['__proto__', 'polluted'], value: true }] }), /路径/);
});

test('Remote 与普通路由共用 daemon 调用快照、设置提交与取消边界，不丢失未 await 的写入', async t => {
  const home = await mkdtemp(join(tmpdir(), 'magi-remote-scope-'));
  const previousHome = process.env.DSH_HOME;
  process.env.DSH_HOME = home;
  let running, snapshot = { version: 0, value: null }, failure, currentSession = 'first', retained;
  t.after(async () => {
    await running?.dispose();
    if (previousHome === undefined) delete process.env.DSH_HOME; else process.env.DSH_HOME = previousHome;
    await rm(home, { recursive: true, force: true });
  });
  const h = host();
  h.api.plugins = { async list() { return { plugins: [] }; } };
  h.api.session.directory = async () => ({ sessions: [{ sessionId: currentSession, createdAt: 1 }], hasMore: false });
  h.api.resource = {
    async read() { return structuredClone(snapshot); },
    async write(_id, version, value) {
      await delay(5);
      if (failure) throw failure;
      assert.equal(version, snapshot.version);
      return snapshot = { version: version + 1, value: structuredClone(value) };
    },
  };
  class Preferences extends TypertRemoteService {
    static inject = ['sessions', 'settings', 'profileContext'];
    constructor(ctx) { super(ctx, 'preferences', { namespace: 'preferences' }); }
    read() {
      retained = this.ctx.sessions.get(currentSession);
      return { session: retained.id, enabled: this.ctx.settings.get('preferences').enabled,
        profile: JSON.parse(readProfileFile(join(this.ctx.profileContext.dir, 'package.json'), 'utf8')).name };
    }
    save(enabled) { this.ctx.settings.update('preferences', { enabled }); return { saved: true }; }
  }
  for (const method of ['read', 'save']) Remote(method)(undefined, {
    name: method, private: false, static: false, addInitializer(fn) { fn.call(Object.create(Preferences.prototype)); },
  });
  running = await activateDsh(h.api, { inject: ['settings'], async apply(ctx) {
    ctx.settings.register('preferences', value => ({ enabled: value.enabled ?? true }));
    await ctx.plugin(Preferences);
  } }, { ...options, readSessionDirectory: true, settingsNamespaces: ['preferences'] });
  const call = (method, args = {}, signal = new AbortController().signal) => h.state.routes.get(REMOTE_ROUTE)({
    method: 'POST', body: { endpoint: 'preferences/' + method, args },
  }, { signal });
  assert.deepEqual((await call('read')).body, { ok: true, value: { session: 'first', enabled: true, profile: 'magi-dsh-profile' } });
  assert.throws(() => retained.id, /调用/);
  assert.deepEqual((await call('save', { enabled: false })).body, { ok: true, value: { saved: true } });
  assert.equal(snapshot.value.preferences.enabled, false, 'Remote 回执须等待未 await 的 CAS 写入');
  currentSession = 'second';
  assert.equal((await call('read')).body.value.session, 'second', '每次 Remote 都读取 daemon 当前目录');
  failure = new Error('permission revoked');
  await assert.rejects(call('save', { enabled: true }), /permission revoked/);
  assert.equal(snapshot.value.preferences.enabled, false, '写入失败不能返回成功或污染已确认设置');
  failure = undefined;
  await assert.rejects(call('save', { enabled: true }, AbortSignal.abort()), /abort/i);
  assert.equal(snapshot.value.preferences.enabled, false, '预先取消不进入业务');
  assert.deepEqual(h.state.failures, []);
});

test('后端会话目录消费真实分页，保持 list/get 形状且拒绝跨调用、取消和未知 Session 字段', async () => {
  let version=0, saved, calls=[];
  const backend=createBackendSessions({session:{async directory(input){
    calls.push(input);
    return input.afterSessionId ? {sessions:[{sessionId:'b',createdAt:1}],hasMore:false,nextAfterSessionId:null}
      : {sessions:[{sessionId:`a${version}`,createdAt:2}],hasMore:true,nextAfterSessionId:`a${version}`};
  }}});
  assert.equal(typeof backend.service.list,'function');
  assert.equal(backend.service.scope,undefined);
  assert.throws(()=>backend.service.list(),/当前 daemon 调用/);
  await backend.run(()=>{
    assert.deepEqual(backend.service.list().map(session=>session.id),['b','a0']);
    saved=backend.service.get('a0');
    assert.throws(()=>saved.header,/尚未映射/);
    assert.equal(backend.service.get('unknown'),undefined);
  });
  assert.throws(()=>saved.id,/当前 daemon 调用/);
  version++;
  await backend.run(()=>{
    assert.equal(backend.service.get('a0'),undefined);
    assert.equal(backend.service.get('a1').id,'a1');
    assert.throws(()=>saved.id,/不属于当前调用/);
  });
  assert.equal(calls.length,4);
  const controller=new AbortController();controller.abort(new Error('cancelled'));
  await assert.rejects(backend.run(()=>assert.fail(),controller.signal),/cancelled/);
  assert.equal(calls.length,4);
  backend.dispose();
  await assert.rejects(backend.run(()=>assert.fail()),/已失效/);
});

test('后端会话目录分页错误、撤权与处置不会回落旧快照或继续执行业务',async()=>{
  for(const directory of [
    async()=>({sessions:[{sessionId:'same',createdAt:1}],hasMore:true,nextAfterSessionId:'same'}),
    async()=>({sessions:[],hasMore:true,nextAfterSessionId:null}),
    async()=>{throw new Error('permission revoked');},
  ]){
    const backend=createBackendSessions({session:{directory}});
    await assert.rejects(backend.run(()=>assert.fail('目录未成功不得执行业务')),/身份重复|游标|revoked/);
    assert.throws(()=>backend.service.list(),/当前 daemon 调用/);
  }
  let finish;
  const backend=createBackendSessions({session:{directory:()=>new Promise(resolve=>{finish=resolve;})}});
  const request=backend.run(()=>assert.fail('处置期间不能使用迟到目录'));
  backend.dispose();finish({sessions:[],hasMore:false,nextAfterSessionId:null});
  await assert.rejects(request,/已失效/);
});

test('附件读取沿 daemon 持久身份，不保留绕过授权的本地图片副本', async (t) => {
  const h = host();
  const values = new Map();
  let gets = 0, attachments;
  h.api.images = {
    async save(input) {
      assert.equal(input.mediaType, 'image/png');
      const ref={attachmentId:'daemon-image',mediaType:input.mediaType,name:input.name,bytes:input.data.length};
      values.set('daemon-image', {ref,data:Buffer.from(input.data)});
      return ref;
    },
    async read(id) {
      gets++;
      if (!values.has(id)) throw new Error('附件不存在或已撤权');
      return {ref:{...values.get(id).ref},data:Buffer.from(values.get(id).data)};
    },
  };
  const running = await activateDsh(h.api, {inject:['attachments'],apply(ctx){ attachments=ctx.attachments; }}, options);
  t.after(()=>running.dispose());
  const bytes=Buffer.from('image payload'), ref=await attachments.saveImage({data:bytes,mediaType:'image/png',name:'capture.png'});
  assert.deepEqual(ref,{attachmentId:'daemon-image',mediaType:'image/png',bytes:bytes.length,name:'capture.png'});
  bytes.fill(0);
  const read=await attachments.readImage({...ref,mediaType:'image/jpeg',name:'forged.jpg'});
  assert.equal(read.data.toString(),'image payload');
  assert.equal(read.ref.mediaType,'image/png');
  assert.equal(read.ref.name,'capture.png');
  assert.equal(gets,1);
  values.clear();
  await assert.rejects(attachments.readImage(ref),/不存在或已撤权/);
  const controller=new AbortController(); controller.abort(new Error('cancelled'));
  await assert.rejects(attachments.readImage(ref,controller.signal),/cancelled/);
  assert.equal(gets,2,'已取消的请求不读取 daemon');
  h.api.images.save=async()=>({});
  await assert.rejects(attachments.saveImage({data:Buffer.from('x'),mediaType:'image/png'}),/未返回已保存附件身份/);
  delete h.api.images;
  await assert.rejects(attachments.saveImage({data:Buffer.from('x'),mediaType:'image/png'}),/未提供持久图片附件服务/);
});

test('DSH 插件目录读取 daemon 当前事实，保留作用域包名、版本和停用状态，不伪造空列表或写入口', async () => {
  const { createPluginManager } = await import('./runtime/plugin-manager.mjs');
  let enabled = true;
  const manager = createPluginManager({ plugins: { async list() { return { plugins: [
    { manifest: { name: 'Scoped plugin', description: 'description' }, source: { kind: 'upstream', location: 'dsh:@scope/sample@1.2.3' }, enabled },
    { manifest: { name: 'Native' }, source: { kind: 'local', location: 'native.zip' }, enabled: true },
  ] }; } } }, 'https://registry.npmjs.org');
  const first = await manager.listBundles();
  assert.equal(first.length, 1); assert.equal(first[0].name, '@scope/sample');
  assert.equal(first[0].removable, true, '原生管理入口可移除；DSH 写操作不会被伪造');
  assert.equal(first[0].version, '1.2.3'); assert.equal(first[0].enabled, true);
  enabled = false;
  assert.equal((await manager.listBundles())[0].enabled, false);
  assert.equal(manager.installBundle, undefined, '没有实施的写操作不冒充可用能力');
  assert.deepEqual(await manager.registries(), { registry: 'https://registry.npmjs.org' });
});

test('未接入的 loader 和 sessions 不再以空服务满足必需依赖', async (t) => {
  const h = host();
  const running = await activateDsh(h.api, { apply(ctx) {
    assert.equal(ctx.get('sessions', false), undefined);
    assert.equal(ctx.get('loader', false), undefined);
  } }, options);
  t.after(() => running.dispose());
  for (const dependency of ['loader', 'sessions']) await assert.rejects(activateDsh(host().api, { inject: [dependency], apply() { assert.fail('必需服务缺失不能启动'); } }, options), /缺少必需服务/);
  let sessions, ids;
  const projected = await activateDsh(host().api, { inject: ['sessions'], apply(ctx) { sessions = ctx.sessions; ids = sessions.list().map(session=>session.id); } }, { ...options, readSessionDirectory: true });
  t.after(() => projected.dispose());
  assert.deepEqual(ids, ['fixture-session']);
  assert.throws(()=>sessions.list(), /当前 daemon 调用/);
});

test('DSH 管理安装只转发到 daemon 任务，保留版本来源、requestId、取消回执与失败结果', async () => {
  const { createPluginManager, parseBundleSpec } = await import('./runtime/plugin-manager.mjs');
  const calls = [];
  let result = { application: 'applied', bundle: '@scope/sample', version: '1.2.3', changed: true };
  const manager = createPluginManager({ plugins: {
    async installBundle(input) { calls.push(input); return result; },
    async cancelInstall(requestId) { calls.push(['cancel', requestId]); return { status: 'too-late' }; },
    async preview() { assert.fail('翻译层不能另起预览链路'); },
    async install() { assert.fail('翻译层不能另起写入链路'); },
  } });
  assert.equal(parseBundleSpec('sample@^1').version, '^1');
  assert.equal(parseBundleSpec('sample@next').version, 'next');
  assert.equal(parseBundleSpec('sample@>=1 <2').version, '>=1 <2');
  for (const spec of ['github:Owner/Repo#feature/desktop', 'git+https://github.com/Owner/Repo.git#feature/desktop', 'Owner/Repo#feature/desktop']) {
    assert.deepEqual(parseBundleSpec(spec), { ecosystem: 'dsh', github: { repository: 'owner/repo', reference: 'feature/desktop' } });
  }
  for (const bad of ['../sample', 'https://example.org/sample', 'sample@1.0.0;touch x', '@scope/sample@']) assert.throws(() => parseBundleSpec(bad));
  assert.deepEqual(await manager.installBundle('@scope/sample@^1', { requestId: 'upgrader-1', enabled: false }), result);
  assert.deepEqual(calls[0], { ecosystem: 'dsh', package: '@scope/sample', version: '^1', enabled: false, requestId: 'upgrader-1' });
  assert.deepEqual(await manager.cancelInstall('upgrader-1'), { status: 'too-late' });
  assert.deepEqual(calls[1], ['cancel', 'upgrader-1']);
  result = { application: 'cancelled', changed: false };
  assert.equal((await manager.installBundle('sample@next')).application, 'cancelled');
  assert.match(calls[2].requestId, /^[0-9a-f-]{36}$/);
  result = { application: 'failed', installed: true, stage: 'enable', error: { code: 'authorization-required', message: '请确认权限' } };
  assert.deepEqual(await manager.installBundle('sample@1.0.0'), result);
  result = { application: 'failed', error: { code: 'incompatible-version', incompatible: [{ name: 'sample', version: '1.0.0' }] } };
  assert.deepEqual(await manager.installBundle('sample@1.0.0'), result);
  const count = calls.length;
  for (const [spec, options] of [['../sample', {}], ['sample', { enabled: 'yes' }], ['sample', { registry: 'https://different.test' }], ['sample', { approvedBuilds: ['unsafe'] }]]) {
    assert.equal((await manager.installBundle(spec, options)).application, 'failed');
  }
  assert.equal(calls.length, count, '本地格式或来源拒绝不能发起安装');
});

const execution = () => ({ invocationId: 'test-call', signal: new AbortController().signal });

test('全局插件状态观察来自 daemon 当前目录，其他插件失败不终止本实例，恢复及卸载清除故障', async () => {
  const h = host(), failures = new Map();
  let notify;
  const plugin = (state, reason) => ({ manifest: { id: 'dsh.other' }, digest: 'other-digest',
    source: { kind: 'upstream', location: 'dsh:@scope/other@1.0.0' }, enabled: true, runtime: { state, reason, generation: 1 } });
  let plugins = [plugin('failed', '真实启动失败')];
  h.api.plugins = {
    async list() { return { plugins: structuredClone(plugins) }; },
    onChanged(listener) { notify = listener; return () => { notify = undefined; }; },
  };
  const running = await activateDsh(h.api, { apply(ctx) {
    ctx.on('internal/status', fiber => {
      const name = fiber.entry?.options?.name;
      if (!name) return;
      if (fiber.state === 3) assert.ok(typeof fiber._error === 'string' && fiber._error.length > 0, '同步诊断与异步观察必须保留同一个 daemon 故障原因');
      if (fiber.state === 3) fiber.await().catch(error => failures.set(name, error.message));
      else if ([2, 4].includes(fiber.state)) failures.delete(name);
    }, { global: true });
  } }, options);
  try {
    assert.equal(failures.get('@scope/other'), '真实启动失败');
    assert.deepEqual(h.state.failures, [], '外部故障不能进入本实例的失败处理');
    plugins = [plugin('running')]; await notify();
    assert.equal(failures.size, 0);
    plugins = [plugin('failed', '再次失败')]; await notify();
    assert.equal(failures.get('@scope/other'), '再次失败');
    plugins = []; await notify();
    assert.equal(failures.size, 0);
  } finally { await running.dispose(); }
  assert.equal(notify, undefined, '处置回收目录订阅');
});

test('插件目录观察合并变更并补读在途更新，取消订阅与处置拒绝迟到快照', async () => {
  const { createPluginDirectory } = await import('./runtime/plugin-directory.mjs');
  let notify, resolveRead, reads = 0;
  const calls = [], errors = [];
  const api = { plugins: {
    list() { reads++; return new Promise(resolve => { resolveRead = resolve; }); },
    onChanged(listener) { notify = listener; return () => { notify = undefined; }; },
  } };
  const directory = createPluginDirectory(api, message => errors.push(message));
  const plugin = (state) => ({ manifest: { id: 'dsh.other' }, digest: 'digest', source: { kind: 'upstream', location: 'dsh:other@1.0.0' }, enabled: true, runtime: { state, generation: 1 } });
  const off = directory.subscribe(value => { calls.push(value.state); });
  const change = notify();
  resolveRead({ plugins: [plugin('starting')] });
  await until(() => reads === 2);
  resolveRead({ plugins: [plugin('running')] }); await change;
  assert.deepEqual(calls, [1, 2]);
  const late = notify(); off();
  resolveRead({ plugins: [plugin('failed')] }); await late;
  assert.deepEqual(calls, [1, 2]);
  directory.subscribe(value => { calls.push(value.state); });
  directory.dispose();
  resolveRead({ plugins: [plugin('failed')] });
  await delay(0);
  assert.deepEqual(calls, [1, 2]); assert.deepEqual(errors, []);
});

test('Loader baseUrl 使用 daemon 调用期 profile 身份，不使用伪造 HTTP 地址', async (t) => {
  const { pathToFileURL } = await import('node:url');
  const directory = await mkdtemp(join(tmpdir(), 'magi-profile-identity-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const h = host(); h.api.plugins = { async list() { return { plugins: [] }; } };
  const original = process.env.DSH_HOME; process.env.DSH_HOME = directory;
  try {
    let identity;
    const running = await activateDsh(h.api, { apply(ctx) { identity = ctx.baseUrl; } }, options);
    try { assert.equal(identity, pathToFileURL(join(directory, 'profiles/magi') + '/').href); }
    finally { await running.dispose(); }
  } finally {
    if (original === undefined) delete process.env.DSH_HOME; else process.env.DSH_HOME = original;
  }
});
const tool = (name, execute = async () => 'ok') => ({
  name, description: name, parameters: { type: 'object' }, execute,
  output: { schema: { type: 'string' }, render: (_args, value) => [{ type: 'text', text: value }] },
});
async function until(predicate) {
  for (let n = 0; n < 200; n++) { if (predicate()) return; await delay(10); }
  assert.fail('运行状态未在 2 秒内收敛');
}

test('真实 DSH MCP 客户端发现、调用、工具目录变化、取消与停用回收子进程', async (t) => {
  const directory = await mkdtemp(join(tmpdir(), 'magi-mcp-contract-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const file = join(directory, 'server.mjs');
  await writeFile(file, `import {createInterface} from 'node:readline';
let names=['echo'];
const send=value=>process.stdout.write(JSON.stringify(value)+'\\n');
createInterface({input:process.stdin}).on('line',line=>{
 const m=JSON.parse(line);if(!Object.hasOwn(m,'id'))return;
 let result;
 if(m.method==='initialize')result={protocolVersion:'2025-11-25',serverInfo:{name:'contract',version:'1'},capabilities:{tools:{listChanged:true}}};
 else if(m.method==='tools/list')result={tools:names.map(name=>({name,description:'contract tool',inputSchema:{type:'object',properties:{replace:{type:'boolean'}}}}))};
 else if(m.method==='tools/call'){
  result={content:[{type:'text',text:JSON.stringify({pid:process.pid,name:m.params.name})}]};
  if(m.params.arguments?.replace){names=['replacement'];send({jsonrpc:'2.0',method:'notifications/tools/list_changed'});}
 }else result={};
 send({jsonrpc:'2.0',id:m.id,result});
});`);
  const h = host();
  const running = await activateDsh(h.api, {
    async apply(ctx) { const fiber = ctx.plugin(McpClient, { transport: 'stdio', serverName: 'contract', command: process.execPath, args: [file], failOnStartupError: true }); await fiber.await(); },
  }, options);
  t.after(() => running.dispose());
  const oldId = runtimeId('tool', 'mcp__contract__echo');
  assert.ok(h.state.tools.has(oldId));
  const response = await h.state.tools.get(oldId)({ replace: true }, execution());
  const receipt = JSON.parse(response.content[0].text);
  assert.equal(receipt.name, 'echo');
  const nextId = runtimeId('tool', 'mcp__contract__replacement');
  await until(() => !h.state.tools.has(oldId) && h.state.tools.has(nextId));
  await assert.rejects(h.state.tools.get(nextId)({}, { invocationId: 'cancelled', signal: AbortSignal.abort(new Error('cancelled')) }), /cancel|abort/i);
  await running.dispose();
  assert.equal(h.state.tools.size, 0);
  await until(() => { try { process.kill(receipt.pid, 0); return false; } catch { return true; } });
  assert.deepEqual(h.state.failures, []);
});

test('DSH 初始化失败、必需依赖缺失和未支持事件均不能就绪，并回收已有工具', async () => {
  for (const plugin of [
    { inject: ['tools'], apply(ctx) { ctx.tools.register(tool('early')); throw new Error('startup failed'); } },
    { inject: ['absent'], apply() { assert.fail('缺依赖不能执行'); } },
    { apply(ctx) { ctx.plugin({ inject: ['absent'], apply() { assert.fail('子插件必需依赖不能绕过'); } }); } },
    { apply(ctx) { ctx.on('session/event', () => {}); } },
  ]) {
    const h = host();
    await assert.rejects(activateDsh(h.api, plugin, options), /startup failed|缺少必需服务|尚未实现/);
    assert.equal(h.state.tools.size, 0);
  }
});

test('DSH 条件注入缺服务保留 pending，注册失败交由上游捕获；可用能力正常执行', async (t) => {
  const h = host();
  let missingRan = false, caught = false;
  const running = await activateDsh(h.api, { apply(ctx) {
    ctx.inject(['sessionProjections'], () => { missingRan = true; });
    ctx.inject(['tools'], (c) => c.tools.register(tool('available')));
    try { ctx.on('session/event', () => assert.fail('不得伪造宿主事件')); }
    catch { caught = true; }
  } }, options);
  t.after(() => running.dispose());
  assert.equal(missingRan, false);
  assert.equal(caught, true);
  assert.ok(h.state.logs.some((line) => line.includes('可选注入') && line.includes('sessionProjections')));
  const result = await h.state.tools.get(runtimeId('tool', 'available'))({}, execution());
  assert.equal(result.content[0].text, 'ok');
  assert.deepEqual(h.state.failures, []);
});

test('事件不可用返回原包所声明的 null，catch 路径取得真实拒绝，不能伪装订阅成功', async t => {
  const h=host(); let conditional, caught=false, context;
  const running=await activateDsh(h.api, {apply(ctx) {
    context=ctx;
    conditional=ctx.on?.('agent/created',()=>assert.fail('不得触发未提供的 Agent 事件'))??null;
    try {ctx.on('session/event',()=>{});}catch{caught=true;}
  }}, {...options, nullableEvents:['agent/created'], optionalEvents:['session/event']});
  t.after(()=>running.dispose());
  assert.equal(conditional,null);assert.equal(caught,true);
  context.emit('agent/created', {agent:{id:'must-not-be-observed'}});
  assert.deepEqual(h.state.failures,[]);
  assert.ok(h.state.logs.some(line=>line.includes('agent/created')&&line.includes('null')));
});

test('DSH 就绪后的注册、撤销、同名重绑沿 effect 生效，卸载清空目录', async (t) => {
  const h = host();
  let ctx;
  const running = await activateDsh(h.api, { inject: ['tools', 'llm'], apply(c) { ctx = c; } }, options);
  t.after(() => running.dispose());
  const id = runtimeId('tool', 'late');
  const off = ctx.tools.register(tool('late', async () => 'first'));
  await until(() => h.state.tools.has(id));
  assert.equal((await h.state.tools.get(id)({}, execution())).content[0].text, 'first');
  await off();
  assert.equal(h.state.tools.has(id), false);
  ctx.tools.register(tool('late', async () => 'second'));
  await until(() => h.state.tools.has(id));
  assert.equal((await h.state.tools.get(id)({}, execution())).content[0].text, 'second');
  await running.dispose();
  assert.equal(h.state.tools.size, 0);
  assert.deepEqual(h.state.catalog.tools, []);
  assert.deepEqual(h.state.failures, []);
});

test('DSH 普通配置重载并重绑工具；无效配置不替换运行实例', async (t) => {
  const h = host({ label: 'first' });
  const lifecycle = [];
  const running = await activateDsh(h.api, {
    inject: ['tools'],
    Config: Schema.object({ label: Schema.string().required() }),
    apply(ctx, config) {
      lifecycle.push(`start:${config.label}`);
      ctx.tools.register(tool('configured', async () => config.label));
      ctx.effect(() => () => { lifecycle.push(`stop:${config.label}`); });
    },
  }, options);
  t.after(() => running.dispose());
  const call = () => h.state.tools.get(runtimeId('tool', 'configured'))({}, execution());
  await h.configure({ label: 'second' });
  assert.equal((await call()).content[0].text, 'second');
  assert.deepEqual(lifecycle, ['start:first', 'stop:first', 'start:second']);
  await assert.rejects(h.configure({ label: 42 }));
  assert.equal((await call()).content[0].text, 'second');
  assert.deepEqual(h.state.failures, []);
});

test('DSH Volatile 配置更新保留实例且投递更新路径', async (t) => {
  const h = host({ label: 'first' });
  let starts = 0, ctx;
  const paths = [];
  const running = await activateDsh(h.api, {
    inject: ['tools'],
    Config: Schema.object({ label: Schema.string().default('default').volatile() }),
    apply(c, config) {
      ctx = c;
      starts++;
      ctx.on('loader/volatile-update', (changed) => paths.push(changed));
      ctx.tools.register(tool('configured', async () => config.label.get()));
    },
  }, options);
  t.after(() => running.dispose());
  await h.configure({ label: 'second' });
  assert.equal(starts, 1);
  assert.deepEqual(paths, [[['label']]]);
  assert.equal((await h.state.tools.get(runtimeId('tool', 'configured'))({}, execution())).content[0].text, 'second');
  await ctx.fiber.restart();
  await until(() => h.state.tools.has(runtimeId('tool', 'configured')));
  assert.equal(starts, 2);
  assert.equal((await h.state.tools.get(runtimeId('tool', 'configured'))({}, execution())).content[0].text, 'second');
});

test('DSH 并发配置通知按版本串行重载，不把旧实例的工具留在新目录', async (t) => {
  const h = host({ label: 'initial' });
  const changes = [];
  const running = await activateDsh(h.api, {
    inject: ['tools'],
    apply(ctx, config) {
      changes.push(`start:${config.label}`);
      ctx.tools.register(tool('configured', async () => config.label));
      ctx.effect(() => async () => { await delay(10); changes.push(`stop:${config.label}`); });
    },
  }, options);
  t.after(() => running.dispose());
  await Promise.all([h.configure({ label: 'first' }), h.configure({ label: 'second' })]);
  assert.equal((await h.state.tools.get(runtimeId('tool', 'configured'))({}, execution())).content[0].text, 'second');
  assert.deepEqual(changes, ['start:initial', 'stop:initial', 'start:first', 'stop:first', 'start:second']);
  assert.deepEqual(h.state.failures, []);
});

test('DSH 工具沿真实管线校验输出、执行 guard，并传递取消', async (t) => {
  const h = host();
  let called = 0;
  const running = await activateDsh(h.api, { inject: ['tools'], apply(ctx) {
    ctx.tools.guard((exec) => exec.name === 'blocked' ? { reason: 'denied by plugin' } : undefined);
    ctx.tools.register(tool('blocked', async () => { called++; return 'unexpected'; }));
    ctx.tools.register(tool('invalid', async () => 123));
    ctx.tools.register(tool('cancel', async (_args, exec) => {
      await delay(1000, null, { signal: exec.signal });
      return 'unexpected';
    }));
  } }, options);
  t.after(() => running.dispose());
  const invoke = (name, context = execution()) => h.state.tools.get(runtimeId('tool', name))({}, context);
  assert.match(JSON.stringify(await invoke('blocked')), /denied by plugin/);
  assert.equal(h.state.outcomes.at(-1).options.isError, true);
  assert.equal(called, 0);
  assert.match(JSON.stringify(await invoke('invalid')), /output|schema|string/i);
  assert.equal(h.state.outcomes.at(-1).options.isError, true);
  const controller = new AbortController();
  const pending = invoke('cancel', { invocationId: 'cancel-call', signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, /abort|cancel/i);
});

test('工具补充上下文与失败纠正共用原生消息交付，完成内容只来自成功管线', async t => {
  const h = host(), delivered = [];
  h.api.session.message = async input => { delivered.push(input); return { inputId: input.messageId, state: 'attached' }; };
  const contextMessage = id => ({ id, role: 'user', content: [{ type: 'text', text: `CONTEXT:${id}` }], source: { kind: 'tool-context' } });
  const running = await activateDsh(h.api, { inject: ['tools'], apply(ctx) {
    for (const name of ['continue', 'finish', 'failed', 'invalid-context']) ctx.tools.register(tool(name, (_args, exec) => {
      exec.deferContext(name === 'invalid-context' ? { ...contextMessage(name), source: {kind:'user'} } : contextMessage(name));
      if (name !== 'continue') exec.concludeTurn();
      if (name === 'failed') throw new Error('business failed with context');
      return `ACTUAL:${name}`;
    }));
  } }, options);
  t.after(() => running.dispose());
  const context = { ...execution(), activity: 'tool', owner: { sessionId: 's' }, execution: { workingDirectory: '/execution' } };
  for (const name of ['continue', 'finish', 'failed']) {
    const result = await h.state.tools.get(runtimeId('tool', name))({}, context);
    const outcome = h.state.outcomes.at(-1);
    assert.equal(result.isError, name === 'failed');
    assert.equal(outcome.options.isError, name === 'failed');
    assert.equal(outcome.options.completion, name === 'finish' ? 'ACTUAL:finish' : undefined);
  }
  assert.deepEqual(delivered.map(({ messageId, text, sourceKind, delivery }) => ({ messageId, text, sourceKind, delivery })),
    ['continue', 'finish', 'failed'].map(id => ({ messageId: id, text: `CONTEXT:${id}`, sourceKind: 'tool-context', delivery: 'context' })));
  const invalid = await h.state.tools.get(runtimeId('tool', 'invalid-context'))({}, context);
  assert.equal(invalid.result.value, 'ACTUAL:invalid-context');
  assert.equal(invalid.contextDelivery.status, 'failed');
  assert.equal(h.state.outcomes.at(-1).options.completion, undefined);
  assert.equal(h.state.outcomes.at(-1).options.isError, true);
  h.api.session.message = async () => { throw new Error('context permission denied'); };
  const undelivered = await h.state.tools.get(runtimeId('tool', 'finish'))({}, context);
  assert.equal(undelivered.result.value, 'ACTUAL:finish');
  assert.equal(undelivered.contextDelivery.status, 'failed');
  assert.match(undelivered.contextDelivery.error, /context permission denied/);
  assert.equal(h.state.outcomes.at(-1).options.isError, true);
  assert.equal(h.state.outcomes.at(-1).options.completion, undefined);
  const aborted = new AbortController(); aborted.abort();
  await assert.rejects(h.state.tools.get(runtimeId('tool', 'finish'))({}, { ...context, signal: aborted.signal }), /abort/i);
  assert.equal(delivered.length, 3);
});

test('DSH 运行期子插件初始化失败被报告，不能保留成功状态', async (t) => {
  const h = host();
  let ctx;
  const running = await activateDsh(h.api, { inject: ['tools', 'llm'], apply(c) { ctx = c; } }, options);
  t.after(() => running.dispose());
  ctx.plugin({ async apply() { await delay(5); throw new Error('late failure'); } });
  await until(() => h.state.failures.length > 0);
  assert.match(h.state.failures[0].message, /late failure/);
});

test('DSH 模型目录刷新期间发生撤销，不发布已撤销的来源', async (t) => {
  const h = host();
  let ctx, entered, resume;
  const listing = new Promise((resolve) => { entered = resolve; });
  const released = new Promise((resolve) => { resume = resolve; });
  const running = await activateDsh(h.api, { inject: ['tools', 'llm'], apply(c) { ctx = c; } }, options);
  t.after(() => running.dispose());
  class Adapter extends LlmAdapter {
    async listModels() { entered(); await released; return [{ provider: 'fixture', id: 'model', name: 'Model' }]; }
  }
  const off = ctx.llm.registerAdapter(['fixture'], new Adapter());
  await listing;
  off();
  resume();
  await until(() => h.state.catalog.modelSources.length === 0);
  assert.equal(h.state.models.size, 0);
  assert.deepEqual(h.state.failures, []);
});

test('DSH 卸载不等待未完成的模型发现，迟到结果不会重新发布', async () => {
  const h = host();
  let ctx, entered, resume;
  const listing = new Promise((resolve) => { entered = resolve; });
  const released = new Promise((resolve) => { resume = resolve; });
  const running = await activateDsh(h.api, { inject: ['llm'], apply(c) { ctx = c; } }, options);
  class Adapter extends LlmAdapter {
    async listModels() { entered(); await released; return [{ provider: 'fixture', id: 'model', name: 'Model' }]; }
  }
  ctx.llm.registerAdapter(['fixture'], new Adapter());
  await listing;
  await running.dispose();
  resume();
  await delay(10);
  assert.equal(h.state.models.size, 0);
  assert.deepEqual(h.state.catalog.modelSources, []);
  assert.deepEqual(h.state.failures, []);
});

test('DSH 初始化期间的模型撤销也参与刷新代次，不能丢弃初始化事件', async (t) => {
  const h = host();
  let off, entered, resume;
  const listing = new Promise((resolve) => { entered = resolve; });
  const released = new Promise((resolve) => { resume = resolve; });
  class Adapter extends LlmAdapter {
    async listModels() { entered(); await released; return [{ provider: 'fixture', id: 'model', name: 'Model' }]; }
  }
  const starting = activateDsh(h.api, { inject: ['llm'], apply(ctx) {
    off = ctx.llm.registerAdapter(['fixture'], new Adapter());
  } }, options);
  await listing;
  off(); resume();
  const running = await starting;
  t.after(() => running.dispose());
  assert.equal(h.state.models.size, 0);
  assert.deepEqual(h.state.catalog.modelSources, []);
  assert.deepEqual(h.state.failures, []);
});

test('DSH 卸载在 daemon 拒绝回写时仍关闭模型端口', async (t) => {
  const h = host();
  class Adapter extends LlmAdapter {
    async listModels() { return [{ provider: 'fixture', id: 'model', name: 'Model' }]; }
  }
  const running = await activateDsh(h.api, { inject: ['llm'], apply(ctx) {
    ctx.llm.registerAdapter(['fixture'], new Adapter());
  } }, options);
  t.after(() => running.dispose());
  const source = [...h.state.models.values()][0];
  assert.ok(source);
  const response = await fetch(`${source.baseUrl}/models`, { headers: { authorization: `Bearer ${source.apiKey}` } });
  assert.equal(response.status, 200);
  await response.json();
  h.api.modelSource.clear = async () => { throw new Error('实例已失效'); };
  await running.dispose();
  await assert.rejects(fetch(`${source.baseUrl}/models`));
});

test('推理关闭按模型实际声明双向转换，未声明及歧义档位不静默替换', async t => {
  const { reasoningOf } = await import('./runtime/reasoning.mjs');
  const received = [];
  const bridge = await startOpenAiBridge({ provider: 'fixture', llm: {
    async resolveModelInfo(_provider, model) { return { reasoning: { efforts: [{ id: model }] } }; },
    async *stream(request) {
      received.push([request.model, request.reasoningEffort]);
      yield { type: 'text-delta', text: 'actual response' };
      yield { type: 'finish', reason: { kind: 'stop' } };
    },
  } });
  t.after(() => bridge.close());
  const expected = [['disabled', 'none'], ['off', 'none'], ['none', 'none'], ['high', 'high'], ['light', 'low'], ['balanced', 'medium'], ['deep', 'high']];
  for (const [id, native] of expected) {
    const metadata = reasoningOf([id], id);
    assert.deepEqual(metadata.supportedEfforts, [native]); assert.equal(metadata.defaultEffort, native);
    const response = await fetch(`${bridge.baseUrl}/chat/completions`, { method: 'POST',
      headers: { authorization: `Bearer ${bridge.apiKey}`, 'content-type': 'application/json' },
      body: JSON.stringify({ model: id, stream: true, reasoning_effort: native, messages: [{ role: 'user', content: 'hello' }] }),
    });
    assert.equal(response.status, 200); assert.match(await response.text(), /actual response/);
  }
  assert.deepEqual(received, expected.map(([id]) => [id, id]));
  const rejected = await fetch(`${bridge.baseUrl}/chat/completions`, { method: 'POST',
    headers: { authorization: `Bearer ${bridge.apiKey}`, 'content-type': 'application/json' },
    body: JSON.stringify({ model: 'high', stream: true, reasoning_effort: 'none', messages: [{ role: 'user', content: 'hello' }] }),
  });
  assert.equal(rejected.status, 400); assert.equal(received.length, expected.length);
  assert.throws(() => reasoningOf(['off', 'disabled']), /无法区分/);
  assert.throws(() => reasoningOf(['low'], 'high'), /不在模型声明/);
  assert.throws(() => reasoningOf(['unknown']), /无法映射/);
});

test('图片经真实本机 HTTP 模型桥转换，附件引用只在该请求内有效', async (t) => {
  const bytes = await sharp({ create: { width: 8, height: 6, channels: 3, background: '#2468ab' } }).png().toBuffer();
  const dataUrl = `data:image/png;base64,${bytes.toString('base64')}`;
  let reference;
  const bridge = await startOpenAiBridge({
    provider: 'fixture', saveImage: saveRequestImage, withRequest: withRequestImages,
    llm: { async *stream(request) {
      assert.equal(request.messages[0].content[0].text, 'describe');
      reference = request.messages[0].content[1].attachment;
      const stored = await requestAttachments.readImage(reference, request.signal);
      const metadata = await sharp(stored.data).metadata();
      assert.equal(metadata.width, 8);
      assert.equal(metadata.height, 6);
      const variant = await requestAttachments.readImageRequest(reference, { width: 4, height: 4, maxBytes: 4096 }, request.signal);
      assert.equal(variant.width, 4);
      assert.equal(variant.height, 3);
      yield { type: 'text-delta', text: 'image received' };
      yield { type: 'usage', usage: { inputTokens: 1, outputTokens: 2 } };
      yield { type: 'finish', reason: { kind: 'stop' } };
    } },
  });
  t.after(() => bridge.close());
  const response = await fetch(`${bridge.baseUrl}/chat/completions`, {
    method: 'POST', headers: { authorization: `Bearer ${bridge.apiKey}`, 'content-type': 'application/json' },
    body: JSON.stringify({ model: 'image', stream: true, messages: [{ role: 'user', content: [
      { type: 'text', text: 'describe' }, { type: 'image_url', image_url: { url: dataUrl } },
    ] }] }),
  });
  assert.equal(response.status, 200);
  const output = await response.text();
  assert.match(output, /image received/);
  assert.match(output, /data: \[DONE\]/);
  await assert.rejects(requestAttachments.readImage(reference), /当前 Magi 模型请求/);
  await withRequestImages(async () => {
    await assert.rejects(requestAttachments.readImage(reference), /不属于当前请求/);
    const ref = await saveRequestImage(dataUrl);
    await assert.rejects(requestAttachments.readImage({ ...ref, width: 9 }), /元数据不匹配/);
    const controller = new AbortController(); controller.abort();
    await assert.rejects(requestAttachments.readImage(ref, controller.signal), /abort/i);
  });
});

test('模型图片桥接受安全 SVG 并拒绝活动内容', async () => {
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="8" height="6"><rect width="8" height="6" fill="red"/></svg>`;
  const dataUrl = `data:image/svg+xml;base64,${Buffer.from(svg).toString('base64')}`;
  const ref = await withRequestImages(() => saveRequestImage(dataUrl));
  assert.equal(ref.mediaType, 'image/png');
  await assert.rejects(withRequestImages(() => saveRequestImage(`data:image/svg+xml;base64,${Buffer.from('<svg><script>alert(1)</script></svg>').toString('base64')}`)), /活动|外部|根节点/);
});

test('浏览器通用格式通过真实 HTTP 模型桥变成可读位图，伪造和超限图片拒绝', async (t) => {
  const fixtures = await browserImageFixtures(sharp);
  let calls = 0;
  const bridge = await startOpenAiBridge({ provider: 'fixture', saveImage: saveRequestImage, withRequest: withRequestImages,
    llm: { async *stream(request) {
      const blocks = request.messages[0].content;
      assert.equal(blocks.length, fixtures.length);
      for (let index = 0; index < fixtures.length; index++) {
        const [, , , width, height] = fixtures[index];
        const image = await requestAttachments.readImage(blocks[index].attachment, request.signal);
        const meta = await sharp(image.data).metadata();
        assert.equal(meta.mediaType, 'image/png'); assert.equal(meta.width, width); assert.equal(meta.height, height);
        assert.equal(meta.depth, 'uchar');
        const pixel = await sharp(image.data).flatten({background:'#ffffff'}).raw().toBuffer();
        assert.ok(pixel[0] > pixel[1] + 80 && pixel[0] > pixel[2] + 80, `${fixtures[index][0]} 保留红色像素，不能把透明度或通道转换错`);
      }
      calls++;
      yield { type: 'text-delta', text: 'all formats received' };
      yield { type: 'usage', usage: { inputTokens: 1, outputTokens: 2 } };
      yield { type: 'finish', reason: { kind: 'stop' } };
    } },
  });
  t.after(() => bridge.close());
  const response = await fetch(`${bridge.baseUrl}/chat/completions`, { method: 'POST',
    headers: { authorization: `Bearer ${bridge.apiKey}`, 'content-type': 'application/json' },
    body: JSON.stringify({ model: 'image', stream: true, messages: [{ role: 'user', content: fixtures.map(([, mime, bytes]) => ({ type: 'image_url', image_url: { url: `data:${mime};base64,${bytes.toString('base64')}` } })) }] }),
  });
  assert.equal(response.status, 200, await response.clone().text());
  assert.match(await response.text(), /all formats received/); assert.equal(calls, 1);
  await withRequestImages(async () => {
    const bmp = Buffer.from(fixtures.find(([ext]) => ext === 'bmp')[2]);
    bmp.writeInt32LE(100_000, 18); bmp.writeInt32LE(100_000, 22);
    await assert.rejects(saveRequestImage(`data:image/bmp;base64,${bmp.toString('base64')}`), /尺寸/);
    const ico = Buffer.from(fixtures.find(([ext]) => ext === 'ico')[2]);
    ico[6] = 100;
    await assert.rejects(saveRequestImage(`data:image/x-icon;base64,${ico.toString('base64')}`), /尺寸/);
    await assert.rejects(saveRequestImage(`data:image/x-icon;base64,${fixtures[0][2].toString('base64')}`), /ICO/);
    await assert.rejects(saveRequestImage(`data:image/avif;base64,${fixtures[0][2].toString('base64')}`), /声明不一致/);
  });
});

test('BMP 调色板、位域和 RLE 保留颜色及不透明度，不将保留字节当透明度', async () => {
  await withRequestImages(async () => {
    for (const [encoding, input] of bmpEncodingFixtures()) {
      const ref = await saveRequestImage(`data:image/bmp;base64,${input.toString('base64')}`);
      const { data } = await requestAttachments.readImage(ref);
      const pixels = await sharp(data).ensureAlpha().raw().toBuffer();
      assert.equal(ref.width, 2); assert.equal(ref.height, 1);
      for (let at = 0; at < pixels.length; at += 4) {
        assert.ok(pixels[at] > 240 && pixels[at + 1] === 0 && pixels[at + 2] === 0 && pixels[at + 3] === 255, `${encoding}: ${[...pixels]}`);
      }
    }
  });
});


test('插件消息同步业务等待真实 daemon 回执，区分上下文与唤醒且不泄漏过期投影', async () => {
  const {withInvocation,invocationAgent}=await import('./runtime/invocation.mjs');
  const calls=[];let done,agent;
  const api={session:{message:async input=>{calls.push(input);await new Promise(resolve=>{done=resolve});return {inputId:'input',state:'pending'}}}};
  const context={...execution(),activity:'command',owner:{sessionId:'s'},execution:{workingDirectory:'/execution',sourceFolders:[],accessProfile:'restricted'}};
  const message={id:'m1',role:'user',content:[{type:'text',text:'RESULT'}],source:{kind:'example-result'}};
  let finished=false;
  const work=withInvocation(context,async()=>{
    agent=invocationAgent();assert.equal(agent.status,'idle');
    agent.steer(message);agent.steer(message);
    return 'original';
  },api).then(value=>{finished=true;return value});
  await until(()=>calls.length===1);assert.equal(finished,false);
  assert.deepEqual(calls,[{messageId:'m1',text:'RESULT',sourceKind:'example-result',delivery:'steer'}]);
  done();assert.equal(await work,'original');
  assert.throws(()=>agent.inject(message),/失效/);
  await assert.rejects(withInvocation(context,()=>{invocationAgent().inject({...message,source:{kind:'user'}})},api),/独立来源/);
  await assert.rejects(withInvocation(context,()=>{invocationAgent().inject(message);throw new Error('business failed')},api),/business failed/);
  assert.equal(calls.length,1,'失败业务不能提交临时消息');
  const rejected={session:{message:async()=>{throw new Error('permission denied')}}};
  await assert.rejects(withInvocation(context,()=>{invocationAgent().inject(message)},rejected),/permission denied/);
  const delivered=[];
  await withInvocation({...context,activity:'tool'},()=>{const a=invocationAgent();assert.equal(a.status,'running');a.inject(message);a.followup({...message,id:'m2'});},
    {session:{message:async input=>{delivered.push(input.delivery);return {inputId:'i',state:'attached'}}}});
  assert.deepEqual(delivered,['context','followup']);
});


test('原包工具与命令共用已装配的消息端口，状态与真实 daemon 交付方式一致', async t => {
  const h=host(), messages=[];
  h.api.session.message=async input=>{messages.push(input);return {inputId:'accepted',state:'pending'}};
  const body={id:'command-message',role:'user',content:[{type:'text',text:'ORIGINAL'}],source:{kind:'result'}};
  const running=await activateDsh(h.api,{inject:['commands','tools'],apply(ctx){
    ctx.commands.register({name:'deliver',handler:invocation=>{
      assert.equal(invocation.agent.status,'idle');
      invocation.agent.steer(body);
      return {kind:'success',text:'COMMAND'};
    }});
    ctx.tools.register(tool('deliver',(_args,exec)=>{
      assert.equal(exec.agent.status,'running');
      exec.agent.inject({...body,id:'tool-message'});
      return 'TOOL';
    }));
  }},options);
  t.after(()=>running.dispose());
  const context={...execution(),owner:{sessionId:'session'},execution:{workingDirectory:'/execution'}};
  await h.state.commands.get(runtimeId('cmd','deliver'))({}, {...context,activity:'command'});
  await h.state.tools.get(runtimeId('tool','deliver'))({},context);
  assert.deepEqual(messages.map(({messageId,delivery})=>({messageId,delivery})),[
    {messageId:'command-message',delivery:'steer'},{messageId:'tool-message',delivery:'context'},
  ]);
});

test('DSH 互斥输入量转换为完整 Chat 输入，保留缓存读写及推理拆分的实际零值与缺席', () => {
  const translator = createChunkTranslator('any-provider');
  const usage = value => translator.push({type:'usage',usage:value})[0].usage;
  assert.deepEqual(usage({inputTokens:120,cacheReadTokens:880,cacheWriteTokens:30,outputTokens:20,reasoningTokens:5,totalTokens:1050}),{
    prompt_tokens:1030,completion_tokens:20,total_tokens:1050,prompt_tokens_details:{cached_tokens:880},
    cache_creation_input_tokens:30,completion_tokens_details:{reasoning_tokens:5},
  });
  assert.deepEqual(usage({inputTokens:120,cacheReadTokens:880,outputTokens:20}),{
    prompt_tokens:1000,completion_tokens:20,total_tokens:1020,prompt_tokens_details:{cached_tokens:880},
  });
  assert.deepEqual(usage({inputTokens:10,cacheReadTokens:0,cacheWriteTokens:0,outputTokens:2,reasoningTokens:0}),{
    prompt_tokens:10,completion_tokens:2,total_tokens:12,prompt_tokens_details:{cached_tokens:0},
    cache_creation_input_tokens:0,completion_tokens_details:{reasoning_tokens:0},
  });
  assert.deepEqual(usage({inputTokens:10,outputTokens:2}),{prompt_tokens:10,completion_tokens:2,total_tokens:12});
});

test('DSH 模型桥即时交付正文，最终用量保留可配对的准确推理拆分而不补造缺失拆分', async t => {
  let release, delivered;
  const resume = new Promise(resolve=>{release=resolve;});
  const first = new Promise(resolve=>{delivered=resolve;});
  const bridge=await startOpenAiBridge({provider:'any-provider',llm:{async *stream() {
    yield {type:'text-delta',text:'first'};delivered();await resume;
    yield {type:'text-delta',text:' last'};
    yield {type:'usage',usage:{inputTokens:494,cacheReadTokens:21120,outputTokens:979,reasoningTokens:0}};
    yield {type:'finish',reason:{kind:'stop'}};
  }}});
  t.after(()=>{release();return bridge.close();});
  const response=await fetch(bridge.baseUrl+'/chat/completions',{method:'POST',
    headers:{authorization:'Bearer '+bridge.apiKey,'content-type':'application/json'},
    body:JSON.stringify({model:'plain',stream:true,messages:[{role:'user',content:'long answer'}]})});
  await first;
  const reader=response.body.getReader();
  const head=new TextDecoder().decode((await reader.read()).value);
  assert.match(head,/first/);assert.doesNotMatch(head,/usage|last/,'正文不能等待最终用量才输出');
  release();let tail='';while(true){const chunk=await reader.read();if(chunk.done)break;tail+=new TextDecoder().decode(chunk.value);}
  const usage=tail.split('\n\n').filter(line=>line.startsWith('data: {')).map(line=>JSON.parse(line.slice(6))).find(chunk=>chunk.usage).usage;
  assert.equal(usage.prompt_tokens,21614);assert.equal(usage.prompt_tokens_details.cached_tokens,21120);
  assert.equal(usage.completion_tokens,979);assert.equal(usage.completion_tokens_details.reasoning_tokens,0);
  assert.match(tail,/data: \[DONE\]/);
});

for (const kind of ['eof', 'throw', 'throw-after-finish', 'error', 'aborted']) {
  test(`DSH 模型桥 ${kind} 保留已报告用量、返回真实失败并关闭迭代器`, async t => {
    let closed = false;
    const bridge = await startOpenAiBridge({ provider:'any-provider', llm:{ async *stream() {
      try {
        if (kind !== 'error' && kind !== 'aborted') yield {type:'text-delta',text:'partial'};
        yield {type:'usage',usage:{inputTokens:12,cacheReadTokens:88,outputTokens:3}};
        if (kind === 'throw-after-finish') yield {type:'finish',reason:{kind:'stop'}};
        if (kind.startsWith('throw')) throw new Error('upstream connection failed');
        if (kind !== 'eof') yield {type:'finish',reason:{kind,failure:{code:'UPSTREAM_STOPPED',message:'upstream stopped'}}};
      } finally { closed = true; }
    } } });
    t.after(() => bridge.close());
    const response = await fetch(bridge.baseUrl+'/chat/completions', {
      method:'POST',headers:{authorization:'Bearer '+bridge.apiKey,'content-type':'application/json'},
      body:JSON.stringify({model:'plain',stream:true,messages:[{role:'user',content:'test'}]}),
    });
    const body = await response.text();
    assert.equal(response.status,200,'已收到的用量沿 SSE 回执提交，业务终态由 error 声明');
    const chunks = body.split('\n\n').filter(line=>line.startsWith('data: {')).map(line=>JSON.parse(line.slice(6)));
    assert.deepEqual(chunks.find(chunk=>chunk.usage)?.usage, {
      prompt_tokens:100,completion_tokens:3,total_tokens:103,prompt_tokens_details:{cached_tokens:88},
    });
    assert.ok(chunks.some(chunk=>chunk.error?.message),'不得将上游失败或缺少 finish 当作成功');
    assert.ok(!chunks.some(chunk=>chunk.choices?.some(choice=>choice.finish_reason)),'完成回执必须等待迭代器正常退出');
    assert.doesNotMatch(body,/data: \[DONE\]/);
    assert.equal(closed,true,'错误终态也必须释放上游生成器');
  });
}

test('DSH 模型桥保留无用量的真实拒绝状态，正常 finish 后的最终用量也完整交付', async t => {
  const bridge = await startOpenAiBridge({provider:'any-provider',llm:{async *stream(request) {
    if (request.model==='denied') {
      yield {type:'finish',reason:{kind:'error',failure:{status:403,code:'denied',message:'permission denied'}}};
      return;
    }
    yield {type:'text-delta',text:'ok'};
    yield {type:'finish',reason:{kind:'stop'}};
    yield {type:'usage',usage:{inputTokens:0,outputTokens:0}};
  }}});
  t.after(()=>bridge.close());
  const request = model => fetch(bridge.baseUrl+'/chat/completions', {method:'POST',
    headers:{authorization:'Bearer '+bridge.apiKey,'content-type':'application/json'},
    body:JSON.stringify({model,stream:true,messages:[{role:'user',content:'test'}]}),
  });
  const denied = await request('denied');
  assert.equal(denied.status,403);
  assert.equal((await denied.json()).error.code,'denied');
  const completed = await request('completed'), body = await completed.text();
  assert.equal(completed.status,200);
  assert.match(body,/"prompt_tokens":0,"completion_tokens":0/);
  assert.ok(body.indexOf('"usage"') < body.indexOf('"finish_reason":"stop"'));
  assert.match(body,/data: \[DONE\]/);
});

test('取消本机 HTTP 模型桥请求会中止同一上游流并执行资源释放', async t => {
  let released;
  const closed = new Promise(resolve=>{released=resolve;});
  const bridge = await startOpenAiBridge({provider:'any-provider',llm:{async *stream({signal}) {
    try {
      yield {type:'text-delta',text:'partial'};
      await new Promise(resolve=>{
        if(signal.aborted) resolve(); else signal.addEventListener('abort',resolve,{once:true});
      });
    } finally { released(signal.aborted); }
  }}});
  t.after(()=>bridge.close());
  const controller = new AbortController();
  const response = await fetch(bridge.baseUrl+'/chat/completions', {method:'POST',signal:controller.signal,
    headers:{authorization:'Bearer '+bridge.apiKey,'content-type':'application/json'},
    body:JSON.stringify({model:'test',stream:true,messages:[{role:'user',content:'test'}]}),
  });
  await response.body.getReader().read();
  controller.abort();
  assert.equal(await Promise.race([closed,new Promise((_,reject)=>{
    const timer=setTimeout(()=>reject(new Error('上游未释放')),2000);timer.unref();
  })]),true);
});
