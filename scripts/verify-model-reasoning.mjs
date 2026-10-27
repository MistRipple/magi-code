import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import electron from 'electron';
import { connectTarget, waitFor } from './lib/electron-harness.mjs';
import { appServerRequest } from './lib/app-server-harness.mjs';

// 真实 daemon、持久化、Turn 和 Chromium；模型服务以本地 fixture 检查 wire 参数。
const root = new URL('..', import.meta.url).pathname;
const fixture = await mkdtemp(join(tmpdir(), 'magi-reasoning-live-'));
const calls = [], errors = [];
const lastMainCall = () => calls.findLast(call => call.body.tools?.length);
let daemon, browser, page, logs = '', passed = false;
const models = [
  { id: 'glm-fixture', supportedReasoningEfforts: ['low', 'high', 'max'], defaultReasoningEffort: 'high' },
  { id: 'gpt-fixture', supported_reasoning_efforts: ['none', 'low', 'medium', 'high', 'xhigh', 'max'], default_reasoning_effort: 'medium' },
  { id: 'unknown-fixture' },
  { id: 'disabled-fixture', supportedEfforts: [], defaultEffort: null },
  { id: 'single-fixture', supportedEfforts: ['high'], defaultEffort: 'high' },
  { id: 'no-default-fixture', supportedEfforts: ['low', 'high'] },
];
const provider = createServer(async (request, response) => {
  if (request.method === 'GET') {
    response.writeHead(200, { 'content-type': 'application/json' });
    const versioned = new URL(request.url, 'http://fixture').searchParams.has('client_version');
    const metadata = models.map(model=>({slug:model.id,
      ...((model.supportedReasoningEfforts ?? model.supported_reasoning_efforts ?? model.supportedEfforts) !== undefined ? {
        supported_reasoning_levels:(model.supportedReasoningEfforts ?? model.supported_reasoning_efforts ?? model.supportedEfforts).map(effort=>({effort,description:'fixture level'})),
        default_reasoning_level:model.defaultReasoningEffort ?? model.default_reasoning_effort ?? model.defaultEffort ?? null,
      } : {}),
    }));
    response.end(JSON.stringify(versioned ? {models:metadata} : {data:models.map(model=>({id:model.id}))}));
    return;
  }
  let raw = ''; for await (const chunk of request) raw += chunk;
  const body = JSON.parse(raw); calls.push({ path: request.url, body });
  const marker = JSON.stringify(body.messages ?? []).match(/AGENT_REASONING_CASE_([A-Z_]+)/)?.[1];
  if (marker) {
    const results = (body.messages ?? []).filter(message => message.role === 'tool').flatMap(message => { try { return [JSON.parse(message.content)]; } catch { return []; } });
    const coordinator = body.tools?.some(tool => tool.function?.name === 'magi_builtin_agent_spawn');
    const send = (name, args, content = 'AGENT_REASONING_DONE') => {
      response.writeHead(200, {'content-type':'text/event-stream'});
      const delta = name ? {role:'assistant',tool_calls:[{index:0,id:`reasoning-${calls.length}`,type:'function',function:{name:`magi_builtin_${name}`,arguments:JSON.stringify(args)}}]} : {role:'assistant',content};
      response.end(`data: ${JSON.stringify({choices:[{index:0,delta,finish_reason:null}]})}\n\ndata: ${JSON.stringify({choices:[{index:0,delta:{},finish_reason:name?'tool_calls':'stop'}],usage:{prompt_tokens:10,completion_tokens:4}})}\n\ndata: [DONE]\n\n`);
    };
    if (!coordinator) {
      if (marker === 'RESTART' && !results.some(result=>['answered','skipped'].includes(result.status))) {
        send('ask_user_question',{questions:[{question:'恢复原强度继续？',header:'恢复',multiSelect:false,options:[{label:'A',description:'继续执行'},{label:'B',description:'其他方案'}]}]}); return;
      }
      send(null, null, 'AGENT_REASONING_CHILD_DONE'); return;
    }
    const spawned = results.findLast(result => result.tool === 'agent_spawn');
    if (!spawned) {
      const effort = {BOUND:'max', INHERITED:'low', INVALID:'ultra', DEFAULT:null, UNKNOWN:'high', RESTART:'max', NONE:'none', UNKNOWN_DEFAULT:null}[marker];
      send('agent_spawn', {task_name:'reasoning_child',role:'executor',display_name:'推理强度验收代理',goal:`AGENT_REASONING_CASE_${marker} 仅回复验收完成`,reasoning_effort:effort,
        delegation:{purpose:'user_requested',reason:'验收请求要求真实代理检查按次推理强度'},context_package:{summary:`AGENT_REASONING_CASE_${marker}`,constraints:['仅回复验收结果'],expected_output:'验收结果',references:[]}});
      return;
    }
    if (['INVALID','UNKNOWN'].includes(marker)) {
      assert.equal(spawned.status,'rejected'); assert.equal(spawned.error_code,'invalid_reasoning_effort');
      assert.ok(!spawned.child_task_id); send(null,null); return;
    }
    assert.ok(spawned.child_task_id, JSON.stringify(spawned));
    if (!results.findLast(result => result.tool === 'agent_wait')?.all_terminal) {send('agent_wait',{task_ids:[spawned.child_task_id],timeout_ms:10000});return;}
    assert.equal(results.findLast(result => result.tool === 'agent_wait').failed_count,0);
    send(null,null); return;
  }
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  response.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', content: 'REASONING_DONE' }, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage: { prompt_tokens: 5, completion_tokens: 5 } })}\n\ndata: [DONE]\n\n`);
});
async function unusedPort() {
  const server = createServer(); await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
}
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
const port = await unusedPort(), cdpPort = await unusedPort();
const origin = `http://127.0.0.1:${port}`;
const connection = { baseUrl: `http://127.0.0.1:${provider.address().port}/v1`, apiKey: 'fixture-key', apiProtocol: 'openai_chat' };
const rpc = (method, params) => appServerRequest(`ws://127.0.0.1:${port}/api/app-server`, method, params);
async function json(path, body) {
  const response = await fetch(`${origin}${path}`, body === undefined ? {} : { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  assert.ok(response.ok, `${path}: ${response.status} ${await response.clone().text()}`); return response.json();
}
async function start() {
  daemon = spawn('./scripts/dev-daemon.sh', [], { cwd: root, detached: true, stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, MAGI_PORT: String(port), MAGI_WEB_DEV: '0', MAGI_OPEN_BROWSER: '0', MAGI_STATE_ROOT: join(fixture, 'state'), MAGI_TARGET_PRUNE_GIB: '1000', MAGI_BUILD_ID: 'reasoning-live' } });
  for (const stream of [daemon.stdout, daemon.stderr]) stream.on('data', chunk => { logs = (logs + chunk).slice(-16000); });
  await Promise.race([
    waitFor(async () => (await fetch(`${origin}/health`)).ok, 'daemon 启动', 240000),
    once(daemon, 'exit').then(()=> { throw new Error(`daemon 启动失败：${logs}`); }),
  ]);
}
async function stop(signal = 'SIGTERM') {
  if (!daemon || daemon.exitCode !== null || daemon.signalCode !== null) return;
  const done = once(daemon, 'exit'); process.kill(-daemon.pid, signal); await done;
}
async function turn(model, effort) {
  const accepted = await rpc('turn/start', { scope: 'personal', text: '仅回复 REASONING_DONE', requestId: `case-${calls.length}`, userMessageId: `user-${calls.length}`,
    orchestratorSessionConfig: { model, reasoningEffort: effort }, accessProfile: 'restricted' });
  await waitFor(async () => {
    const snapshot = await rpc('session/read', { sessionId: accepted.sessionId, includeTurns: true });
    const current = snapshot.turns.find(turn => turn.turnId === accepted.turnId);
    assert.ok(!['failed', 'cancelled'].includes(current?.status), JSON.stringify(current));
    return current?.status === 'completed';
  }, `${model} Turn 完成`, 30000);
  return accepted;
}
async function navigate(sessionId) {
  const previous = await page.evaluate('performance.timeOrigin');
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${sessionId}` });
  await waitFor(() => page.evaluate(`performance.timeOrigin !== ${previous} && Boolean(document.querySelector('.ia-model-btn'))`), '模型按钮');
}
async function openPicker() {
  await page.evaluate('document.querySelector(".ia-model-btn").click()');
  await waitFor(() => page.evaluate('Boolean(document.querySelector(".ia-model-summary"))'), '推理面板');
}
async function choose(model) {
  await page.evaluate('document.querySelector(".ia-summary-model").click()');
  await waitFor(() => page.evaluate(`Boolean([...document.querySelectorAll('.ia-picker-item')].find(el => el.textContent.trim() === ${JSON.stringify(model)}))`), '模型条目');
  await page.evaluate(`[...document.querySelectorAll('.ia-picker-item')].find(el => el.textContent.trim() === ${JSON.stringify(model)}).click()`);
  await waitFor(() => page.evaluate(`document.querySelector('.ia-summary-model')?.textContent.includes(${JSON.stringify(model)}) && !document.querySelector('.ia-model-btn')?.disabled`), '模型切换完成');
}
async function slide(index) {
  const press = async (key, code) => {
    await page.call('Input.dispatchKeyEvent', { type:'keyDown', key, code:key, windowsVirtualKeyCode:code });
    await page.call('Input.dispatchKeyEvent', { type:'keyUp', key, code:key, windowsVirtualKeyCode:code });
    await waitFor(() => page.evaluate('!document.querySelector(".ia-model-btn").disabled'), '档位保存');
  };
  await page.evaluate('document.querySelector(".ia-reasoning-slider input").focus()');
  await press('Home', 36);
  for (let i = 0; i < index; i++) await press('ArrowRight', 39);
}
const snapshot = (sessionId) => json(`/api/settings/bootstrap?scope=personal&sessionId=${sessionId}&bootstrapScope=core`);
try {
  await start();
  await json('/api/settings/orchestrator/save', connection);
  const catalog = await json('/api/settings/models/fetch', { config: connection, target: 'orch' });
  assert.deepEqual(catalog.modelCapabilities['glm-fixture'].supportedEfforts, ['low', 'high', 'max']);
  assert.equal(catalog.modelCapabilities['gpt-fixture'].supportedEfforts.length, 6);
  assert.equal(catalog.modelCapabilities['unknown-fixture'].source, 'unknown');
  const glm = await turn('glm-fixture', 'xhigh');
  assert.equal(lastMainCall().body.reasoning_effort, 'high');
  assert.equal((await snapshot(glm.sessionId)).orchestratorSessionConfig.reasoningEffort, null);
  await turn('gpt-fixture', 'none'); assert.equal(lastMainCall().body.reasoning_effort, 'none');
  await turn('unknown-fixture', 'xhigh'); assert.ok(!Object.hasOwn(lastMainCall().body, 'reasoning_effort'));
  await turn('disabled-fixture', 'high'); assert.ok(!Object.hasOwn(lastMainCall().body, 'reasoning_effort'));
  console.log('✓ 模型元数据、三档/六档、非法旧档位校正、none 与未知/禁用模型实际请求');
  let lastAgent;
  for (const [marker, boundModel, expected] of [['BOUND','glm-fixture','max'],['INHERITED',null,'low'],['DEFAULT','glm-fixture','high'],['INVALID','glm-fixture',null],['UNKNOWN','unknown-fixture',null],['NONE',null,'none'],['UNKNOWN_DEFAULT','unknown-fixture','omit']]) {
    if (boundModel) await json('/api/settings/registry/engines/upsert', {id:'agent-reasoning',displayName:'Agent reasoning',llm:{...connection,model:boundModel,reasoningEffort:null}});
    await json('/api/settings/registry/agents/upsert', {templateId:'executor',engineId:boundModel?'agent-reasoning':null,enabled:true});
    const before = calls.length;
    const accepted = await rpc('turn/start', {scope:'personal',text:`AGENT_REASONING_CASE_${marker} 请使用一个真实子代理验证推理强度`,requestId:`agent-${marker}`,userMessageId:`agent-user-${marker}`,
      orchestratorSessionConfig:{model:'gpt-fixture',reasoningEffort:'medium'},accessProfile:'restricted'});
    const read = await waitFor(async () => {
      const value = await rpc('session/read',{sessionId:accepted.sessionId,includeTurns:true});
      const turn = value.turns.find(turn => turn.turnId === accepted.turnId);
      assert.ok(!['failed','cancelled'].includes(turn?.status),JSON.stringify(turn));
      return turn?.status === 'completed' && value;
    },`子代理 ${marker}`,30000);
    const turn = read.turns.find(turn => turn.turnId === accepted.turnId);
    const spawned = turn.items.find(item => item.tool?.name === 'agent_spawn')?.tool?.result;
    assert.ok(spawned,JSON.stringify(turn));
    const childCalls = calls.slice(before).filter(call => JSON.stringify(call.body.messages).includes(`AGENT_REASONING_CASE_${marker}`) && !call.body.tools?.some(tool => tool.function?.name === 'magi_builtin_agent_spawn'));
    if (expected) {
      assert.equal(spawned.model_selection.effective_effort,expected === 'omit' ? null : expected);
      assert.equal(childCalls.length,1,'每次派发只调用一次子模型，不增加档位选择调用');
      if (expected === 'omit') assert.ok(!Object.hasOwn(childCalls[0].body,'reasoning_effort'));
      else assert.equal(childCalls[0].body.reasoning_effort,expected);
      assert.equal(childCalls[0].body.model,boundModel??'gpt-fixture');
      const query = new URLSearchParams({scope:'personal',sessionId:accepted.sessionId});
      const projection = await json(`/api/agent-runs/projection/${spawned.child_task_id}?${query}`);
      const selection = projection.tasks.find(task=>task.task_id===spawned.child_task_id).executor_binding.model_selection;
      assert.deepEqual(selection,spawned.model_selection); if(marker === 'DEFAULT') lastAgent={accepted,spawned,selection,query};
    } else {assert.equal(spawned.status,'rejected');assert.equal(childCalls.length,0);}
    const prompt = calls.slice(before).find(call=>call.body.tools?.some(tool=>tool.function?.name==='magi_builtin_agent_spawn'));
    assert.ok(JSON.stringify(prompt.body.messages).includes('supportedEfforts'),'真实父请求必须包含动态模型档位');
  }
  // 中断真实子代理交互后改变角色与引擎默认值，再恢复原 Task；不能重新选档或重放提问。
  await json('/api/settings/registry/engines/upsert',{id:'agent-reasoning',displayName:'Agent reasoning',llm:{...connection,model:'glm-fixture',reasoningEffort:null}});
  await json('/api/settings/registry/agents/upsert',{templateId:'executor',engineId:'agent-reasoning'});
  const resumedCase = await rpc('turn/start',{scope:'personal',text:'AGENT_REASONING_CASE_RESTART 使用真实代理，并在中断后继续原任务',requestId:'reasoning-restart',userMessageId:'reasoning-restart-user',accessProfile:'restricted',orchestratorSessionConfig:{model:'gpt-fixture',reasoningEffort:'medium'}});
  const pending = await waitFor(async()=> (await rpc('session/interactions',{sessionId:resumedCase.sessionId})).pendingQuestions[0],'真实子代理交互等待',30000);
  const beforeRestart = await rpc('session/read',{sessionId:resumedCase.sessionId,includeTurns:true});
  const originalSpawn = beforeRestart.turns.find(turn=>turn.turnId===resumedCase.turnId).items.find(item=>item.tool?.name==='agent_spawn').tool.result;
  await json('/api/settings/registry/engines/upsert',{id:'agent-reasoning',displayName:'Agent reasoning changed',llm:{...connection,model:'gpt-fixture',reasoningEffort:'low'}});
  await json('/api/settings/registry/agents/upsert',{templateId:'executor',engineId:null});
  await stop('SIGKILL'); await start();
  const recovered = (await rpc('session/interactions',{sessionId:resumedCase.sessionId})).pendingQuestions[0];
  assert.equal(recovered.questionId,pending.questionId);
  await rpc('session/question/resolve',{sessionId:resumedCase.sessionId,questionId:pending.questionId,response:{kind:'answered',answers:[{selected:['A']}]}});
  const continued = await rpc('turn/start',{scope:'personal',sessionId:resumedCase.sessionId,resume:true,text:'继续执行中断的任务',requestId:'reasoning-continue',userMessageId:'reasoning-continue-user'});
  const finished = await waitFor(async()=>{
    const read = await rpc('session/read',{sessionId:resumedCase.sessionId,includeTurns:true});
    const turn=read.turns.findLast(turn=>!turn.metadata?.agentExecution);
    assert.notEqual(turn?.status,'failed',JSON.stringify(read.turns));
    return turn?.status==='completed' && read;
  },'原子代理恢复完成',30000);
  const resumedCalls = calls.filter(call=>JSON.stringify(call.body.messages).includes('AGENT_REASONING_CASE_RESTART')&&!call.body.tools?.some(tool=>tool.function?.name==='magi_builtin_agent_spawn'));
  assert.equal(resumedCalls.length,2,'重启前提问、恢复后继续，不额外选档或重放模型轮次');
  for (const call of resumedCalls) {assert.equal(call.body.model,'glm-fixture');assert.equal(call.body.reasoning_effort,'max');}
  assert.equal(finished.turns.flatMap(turn=>turn.items).filter(item=>item.tool?.name==='ask_user_question').length,1,'恢复不重放已回答的提问');
  const restartQuery = new URLSearchParams({scope:'personal',sessionId:resumedCase.sessionId});
  const restartedProjection = await json(`/api/agent-runs/projection/${originalSpawn.child_task_id}?${restartQuery}`);
  assert.deepEqual(restartedProjection.tasks.find(task=>task.task_id===originalSpawn.child_task_id).executor_binding.model_selection,originalSpawn.model_selection);
  assert.equal(restartedProjection.agents.find(agent=>agent.agentRunId===originalSpawn.child_task_id).model,'glm-fixture','历史代理投影不能随当前角色改绑而变换模型');
  console.log('✓ 真实中断→daemon 重启→原 Task 继续：角色改绑与引擎默认变化不改变已选择模型和强度，不重放提问');
  await json('/api/settings/registry/agents/upsert',{templateId:'executor',engineId:null,enabled:true});
  const usage = await json('/api/settings/stats');
  assert.ok(usage.models.some(model=>model.resolvedModel==='glm-fixture' && model.reasoningEffort==='max' && model.totals.llmCallCount>0),'真实子模型强度进入用量身份');
  assert.ok(usage.models.some(model=>model.resolvedModel==='gpt-fixture' && model.reasoningEffort==='low' && model.totals.llmCallCount>0),'继承模型按次强度单独归因');

  console.log('✓ 真实 agent_spawn/agent_wait：绑定模型、继承模型、默认强度、非法/未知档位拒绝及持久化快照');

  const entry = join(fixture, 'browser.cjs');
  await writeFile(entry, `const {app,BrowserWindow}=require('electron');app.setPath('userData',${JSON.stringify(join(fixture, 'chromium'))});app.whenReady().then(()=>{const w=new BrowserWindow({show:false,width:1100,height:800,webPreferences:{sandbox:false}});w.loadURL(${JSON.stringify(`${origin}/web.html?scope=personal&sessionId=${glm.sessionId}`)});});`);
  browser = spawn(electron, [entry, `--remote-debugging-port=${cdpPort}`, '--disable-gpu'], { cwd: root, stdio: 'ignore' });
  await waitFor(async () => { try { page = await connectTarget(cdpPort, tab => tab.type === 'page' && tab.url.includes('/web.html'), '推理面板', errors); return true; } catch { return false; } }, 'Chromium');
  await waitFor(() => page.evaluate('document.querySelector(".ia-model-btn")?.textContent.includes("glm-fixture")'), '历史会话恢复');
  await openPicker();
  await waitFor(() => page.evaluate('document.querySelectorAll(".ia-reasoning-ticks span").length === 3'), '三档滑块');
  const point = await page.evaluate('(() => { const r = document.querySelector(".ia-reasoning-slider input").getBoundingClientRect(); return {x:r.right-8,y:r.y+r.height/2}; })()');
  await page.call('Input.dispatchMouseEvent', { type:'mousePressed', ...point, button:'left', clickCount:1 });
  await page.call('Input.dispatchMouseEvent', { type:'mouseReleased', ...point, button:'left', clickCount:1 });
  await waitFor(async () => (await snapshot(glm.sessionId)).orchestratorSessionConfig.reasoningEffort === 'max', '原生鼠标选择最高档');
  await waitFor(() => page.evaluate('!document.querySelector(".ia-reasoning-slider input").disabled'), '档位保存完成');
  assert.equal(await page.evaluate('document.querySelector(".ia-effort-summary-header button")'), null, '强度面板不再提供恢复默认按钮');
  await choose('gpt-fixture');
  assert.equal(await page.evaluate('document.querySelectorAll(".ia-reasoning-ticks span").length'), 6);
  await slide(0); assert.equal((await snapshot(glm.sessionId)).orchestratorSessionConfig.reasoningEffort, 'none');
  await choose('glm-fixture'); assert.equal((await snapshot(glm.sessionId)).orchestratorSessionConfig.reasoningEffort, null);
  await choose('unknown-fixture'); assert.equal(await page.evaluate('document.querySelector(".ia-reasoning-slider")'), null);
  await choose('single-fixture'); assert.equal(await page.evaluate('document.querySelector(".ia-reasoning-slider")'), null);
  assert.equal(await page.evaluate('document.querySelector(".ia-effort-current").textContent'), '模型默认');
  await choose('no-default-fixture');
  assert.equal((await snapshot(glm.sessionId)).orchestratorSessionConfig.reasoningEffort, null, '切换模型保留尚未显式选择的默认状态');
  assert.equal(await page.evaluate('document.querySelector(".ia-effort-current").textContent'), '模型默认');
  await slide(0);
  assert.equal((await snapshot(glm.sessionId)).orchestratorSessionConfig.reasoningEffort, 'low', '默认未选择时 Home 可直接选中首档');
  const unspecified = await turn('no-default-fixture', null);
  assert.ok(!Object.hasOwn(lastMainCall().body, 'reasoning_effort'));
  await navigate(unspecified.sessionId);
  await page.evaluate('document.querySelector("[data-testid=sidebar-new-session]").click()');
  await waitFor(() => page.evaluate('!new URL(location.href).searchParams.get("sessionId")'), '新会话草稿');
  await openPicker(); await choose('gpt-fixture'); await slide(4);
  assert.equal(await page.evaluate('document.querySelector(".ia-effort-current").textContent'), '超高');
  await page.evaluate('document.querySelector(".ia-summary-model").click()');
  await page.evaluate('document.querySelector(".ia-section-header-row button").click()');
  await waitFor(() => page.evaluate('!document.querySelector(".ia-section-header-row button").disabled'), '刷新目录');
  assert.ok(await page.evaluate('document.querySelector(".ia-model-btn").textContent.includes("gpt-fixture")'), '刷新不覆盖草稿模型');
  await page.evaluate('[...document.querySelectorAll(".ia-picker-item")].find(el => el.dataset.modelId === "gpt-fixture").click()');
  await waitFor(() => page.evaluate('Boolean(document.querySelector(".ia-model-summary"))'), '选择当前模型后回到强度面板');
  assert.equal(await page.evaluate('document.querySelector(".ia-effort-current").textContent'), '超高', '刷新不覆盖草稿强度');
  console.log('✓ Chromium 真实模型列表切换、三/六档滑块保存、默认状态保留、未知及单档显示');
  await navigate(lastAgent.accepted.sessionId);
  await waitFor(()=>page.evaluate('Boolean(document.querySelector(".agent-spawn-model"))'),'实际子代理卡片模型强度');
  assert.ok(await page.evaluate('document.querySelector(".agent-spawn-model").textContent.includes("glm-fixture")'));
  await page.evaluate('document.querySelector(".agent-spawn-card").click()');
  await waitFor(()=>page.evaluate('Boolean(document.querySelector("[data-testid=agent-model-selection]"))'),'代理详情执行模型强度');
  assert.ok(await page.evaluate('document.querySelector("[data-testid=agent-model-selection]").textContent.includes("glm-fixture")'));
  await navigate(glm.sessionId); await openPicker();

  await page.evaluate('document.querySelector(".ia-model-btn").click(); document.querySelector("[data-testid=sidebar-settings]").click()');
  await waitFor(() => page.evaluate('Boolean(document.querySelector("[data-testid=settings-nav-model]"))'), '模型设置导航');
  await page.evaluate('document.querySelector("[data-testid=settings-nav-model]").click()');
  await waitFor(() => page.evaluate('Boolean(document.querySelector(".model-form-actions"))'), '模型配置表单');
  assert.equal(await page.evaluate('document.querySelector(".model-compatibility")'), null, '移除用户兼容规则管理');
  for (const [tab, section, field] of [['comp','auxiliary','auxiliaryConfig'], ['vision','vision','visionConfig'], ['reasoning-engine','worker','workerConfigs']]) {
    // 模型名/凭据相同，验证三个/六个档位来自缓存能力；配置和测试由各自入口保存。
    const config = { ...connection, model:'gpt-fixture', reasoningEffort:'medium' };
    if (section === 'worker') await json('/api/settings/registry/engines/upsert', { id:tab, displayName:'Reasoning fixture', llm:config });
    await json(`/api/settings/${section}/save`, section === 'worker' ? {worker:tab, config} : config);
    const savedConfig = async () => {
      const settings = await json('/api/settings/bootstrap?scope=personal&bootstrapScope=core');
      return section === 'worker' ? settings.workerConfigs[tab] : settings[field];
    };
    const previous = await page.evaluate('performance.timeOrigin');
    await page.call('Page.reload');
    await waitFor(() => page.evaluate(`performance.timeOrigin !== ${previous} && Boolean(document.querySelector('[data-testid=sidebar-settings]'))`), '设置入口恢复');
    await page.evaluate('document.querySelector("[data-testid=sidebar-settings]").click()');
    await waitFor(() => page.evaluate('Boolean(document.querySelector("[data-testid=settings-nav-model]"))'), '设置导航');
    await page.evaluate('document.querySelector("[data-testid=settings-nav-model]").click()');
    await waitFor(() => page.evaluate(`Boolean(document.querySelector('[data-tab-id="${tab}"]'))`), '配置标签');
    await page.evaluate(`document.querySelector('[data-tab-id="${tab}"]').click()`);
    const selector = `#model-config-${tab}-level`;
    await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.options.length === 7`), `${section} 六档加模型默认`);
    await page.evaluate(`(() => { const el=document.querySelector(${JSON.stringify(selector)}); el.value=''; el.dispatchEvent(new Event('change',{bubbles:true})); })()`);
    await waitFor(() => page.evaluate('!document.querySelector(".model-form-actions .btn--primary").disabled'), '中档改为默认应可保存');
    await page.evaluate('document.querySelector(".model-form-actions .btn--primary").click()');
    await waitFor(async () => (await savedConfig()).reasoningEffort === null, `${section} 默认保存`);
    await waitFor(() => page.evaluate('!document.querySelector(".model-form-actions .btn--secondary").disabled'), '保存已完成');
    await page.evaluate(`(() => { const el=document.querySelector(${JSON.stringify(selector)}); el.value='xhigh'; el.dispatchEvent(new Event('change',{bubbles:true})); })()`);
    await page.evaluate(`(() => { const el=document.querySelector('#model-config-${tab}-model'); el.value='glm-fixture'; el.dispatchEvent(new Event('input',{bubbles:true})); })()`);
    await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.options.length === 4`), `${section} 切换为三档`);
    assert.equal(await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).value`), '', '旧超高档收敛到模型默认');
    await page.evaluate(`(() => { const el=document.querySelector(${JSON.stringify(selector)}); el.value='max'; el.dispatchEvent(new Event('change',{bubbles:true})); })()`);
    await page.evaluate('document.querySelector(".model-form-actions .btn--primary").click()');
    await waitFor(async () => (await savedConfig()).reasoningEffort === 'max', `${section} 档位保存`);
    await waitFor(() => page.evaluate('!document.querySelector(".model-form-actions .btn--secondary").disabled'), '允许连接测试');
    const before = calls.length;
    await page.evaluate('document.querySelector(".model-form-actions .btn--secondary").click()');
    await waitFor(() => page.evaluate('Boolean(document.querySelector(".model-form-actions .is-success"))'), `${section} 连接测试成功`);
    assert.equal(calls.slice(before).find(call => call.body.model === 'glm-fixture')?.body.reasoning_effort, 'max');
  }
  console.log('✓ 辅助/识图/独立引擎设置真实六档→三档、旧档校正、默认脏态、保存与连接测试参数');
  const modelInput = '#model-config-reasoning-engine-model';
  for (const [model, hint] of [['unknown-fixture','尚未识别可调档位'],['disabled-fixture','不支持调整推理强度']]) {
    await page.evaluate(`(() => { const el=document.querySelector(${JSON.stringify(modelInput)}); el.value=${JSON.stringify(model)}; el.dispatchEvent(new Event('input',{bubbles:true})); })()`);
    await waitFor(() => page.evaluate(`document.querySelector('.model-row').textContent.includes(${JSON.stringify(hint)})`), hint);
    assert.equal(await page.evaluate('document.querySelector("#model-config-reasoning-engine-level")'), null);
  }
  let intercepted = false;
  const removeInterceptor = page.on('Fetch.requestPaused', event => {
    const fail = !intercepted; intercepted = true;
    void page.call(fail ? 'Fetch.fulfillRequest' : 'Fetch.continueRequest', fail
      ? { requestId:event.requestId, responseCode:503, responseHeaders:[{name:'content-type',value:'application/json'}], body:Buffer.from('{"error":"fixture"}').toString('base64') }
      : {requestId:event.requestId});
  });
  await page.call('Fetch.enable', {patterns:[{urlPattern:'*/api/settings/models/reasoning',requestStage:'Request'}]});
  await page.evaluate(`(() => { const el=document.querySelector(${JSON.stringify(modelInput)}); el.value='glm-fixture'; el.dispatchEvent(new Event('input',{bubbles:true})); })()`);
  await waitFor(() => page.evaluate('document.querySelector(".model-row").textContent.includes("读取推理档位失败")'), '读取失败与未知能力分开');
  await page.evaluate('document.querySelector(".reasoning-status--error .btn--secondary").click()');
  await waitFor(() => page.evaluate('document.querySelector("#model-config-reasoning-engine-level")?.options.length === 4'), '能力读取重试成功');
  await page.call('Fetch.disable'); removeInterceptor();
  await page.evaluate('(() => { const el=document.querySelector("#model-config-reasoning-engine-level"); el.value="max"; el.dispatchEvent(new Event("change",{bubbles:true})); })()');
  if (process.env.MAGI_REASONING_SETTINGS_SCREENSHOT) {
    const capture = await page.call('Page.captureScreenshot', {format:'png'});
    await writeFile(process.env.MAGI_REASONING_SETTINGS_SCREENSHOT, Buffer.from(capture.data,'base64'));
  }
  console.log('✓ 未知/不支持/读取失败区分和重试；设置无需维护兼容性规则');
  const restored = await turn('gpt-fixture', 'max');
  await navigate(restored.sessionId); await openPicker();
  if (process.env.MAGI_REASONING_SCREENSHOT) {
    const capture = await page.call('Page.captureScreenshot', { format: 'png' });
    await writeFile(process.env.MAGI_REASONING_SCREENSHOT, Buffer.from(capture.data, 'base64'));
  }
  await stop(); await start(); await navigate(restored.sessionId); await openPicker();
  await waitFor(() => page.evaluate('document.querySelector(".ia-effort-current")?.textContent === "最高"'), '重启后保存恢复');
  assert.equal((await snapshot(restored.sessionId)).orchestratorReasoningCapability.source, 'provider');
  const agentProjection = await json(`/api/agent-runs/projection/${lastAgent.spawned.child_task_id}?${lastAgent.query}`);
  assert.deepEqual(agentProjection.tasks.find(task=>task.task_id===lastAgent.spawned.child_task_id).executor_binding.model_selection,lastAgent.selection,'重启后保留原模型强度快照');
  const restoredSettings = await snapshot(restored.sessionId);
  for (const field of ['auxiliaryConfig','visionConfig']) assert.equal(restoredSettings[field].reasoningEffort, 'max');
  assert.equal(restoredSettings.workerConfigs['reasoning-engine'].reasoningEffort, 'max');
  assert.equal(errors.length, 0, errors.join('\n'));
  console.log('✓ 服务商能力、实际参数、daemon 重启后的配置与会话恢复；浏览器无错误');
  passed = true;
} catch (error) {
  console.error(logs); console.error('Chromium diagnostics:', errors);
  if (page) try { console.error(await page.evaluate('JSON.stringify({url:location.href,text:document.body.innerText.slice(0,3000)})')); } catch {}
  console.error(`诊断状态目录：${fixture}`); throw error;
} finally {
  page?.close(); if (browser && browser.exitCode === null) { const done = once(browser, 'exit'); browser.kill(); await done; }
  await stop(); await new Promise(resolve => provider.close(resolve));
  if (passed) await rm(fixture, { recursive: true, force: true });
}
