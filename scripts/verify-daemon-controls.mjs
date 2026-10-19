import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, access, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { waitFor } from './lib/electron-harness.mjs';
import { appServerRequest } from './lib/app-server-harness.mjs';

// 直接验证 daemon 的 App Server 合同；Provider 只使用本地替身。
const root = fileURLToPath(new URL('..', import.meta.url));
const fixture = await mkdtemp(join(tmpdir(), 'magi-controls-'));
const workspace = join(fixture, 'workspace');
await mkdir(workspace);
const target = join(workspace, 'approval-target.txt');
await writeFile(target, 'approval required');
const imageDataUrl = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Wl6ON8AAAAASUVORK5CYII=';
let daemon;
let logs = '';
let providerRequests = 0;
let lastProviderRequest;
let goalHeld = false;
let completeGoal = false;
let releaseGoal;
let liveParentHeld = false;
let releaseLiveParent;
let transferredApprovalTarget;
let liveStepSkill;
let liveSkillRound = 0;
const goalGate = new Promise(resolve => { releaseGoal = resolve; });
const liveParentGate = new Promise(resolve => { releaseLiveParent = resolve; });
function writeResponse(response, tool, args, text) {
  if (response.destroyed) return;
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  const delta = tool ? { role: 'assistant', tool_calls: [{ index: 0, id: 'reused-control-call', type: 'function',
    function: { name: `magi_builtin_${tool}`, arguments: JSON.stringify(args) } }] } : { role: 'assistant', content: text };
  for (const part of [
    { choices: [{ index: 0, delta, finish_reason: null }] },
    { choices: [{ index: 0, delta: {}, finish_reason: tool ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110 } },
  ]) response.write(`data: ${JSON.stringify({ id: 'control-fixture', object: 'chat.completion.chunk', model: 'control-fixture', ...part })}\n\n`);
  response.end('data: [DONE]\n\n');
}
const provider = createServer(async (request, response) => {
  try {
    let raw = '';
    for await (const chunk of request) raw += chunk;
    if (request.method !== 'POST') {
      response.writeHead(200, {'content-type':'application/json'});
      response.end(JSON.stringify({data:[{id:'control-fixture'}]})); return;
    }
    providerRequests++;
    const body = JSON.parse(raw);
    lastProviderRequest = body;
    const messages = body.messages ?? [];
    const text = JSON.stringify(messages);
    const results = messages.filter(message => message.role === 'tool').flatMap(message => {try {return [JSON.parse(message.content)];} catch {return [];}});
    if (text.includes('CONTROL_LIVE_SKILL_STEP')) {
      const step = liveSkillRound++;
      const names = (body.tools ?? []).map(tool=>tool.function.name).sort();
      const expected = [['magi_builtin_web_fetch'],['magi_builtin_tool_catalog'],
        ['magi_builtin_tool_catalog','magi_builtin_web_fetch'],[]][step];
      assert.deepEqual(names,expected,'每个 Step 使用当前 Skill 目录，包括收紧和放宽');
      if (step === 3) return writeResponse(response,null,null,'CONTROL_LIVE_SKILL_DONE');
      const nextAllowed = [['tool_catalog'],['web_fetch','tool_catalog'],[]][step];
      await writeFile(join(liveStepSkill,'config.json'),JSON.stringify({allowed_tools:nextAllowed}));
      await installLocalSkill(liveStepSkill);
      const tool = step === 1 ? 'tool_catalog' : 'web_fetch';
      return writeResponse(response,tool,tool === 'web_fetch' ? {url:`https://example.invalid/step-${step}`} : {});
    }
    if (text.includes('CONTROL_WORKFLOW_MODEL')) {
      assert.deepEqual(body.tools.map(tool=>tool.function.name),['magi_builtin_web_fetch']);
      assert.ok(body.tools[0].function.parameters.properties);
      response.writeHead(200,{'content-type':'application/json'});
      return response.end(JSON.stringify({id:'workflow-model',object:'chat.completion',model:'control-fixture',
        choices:[{index:0,message:{role:'assistant',content:'CONTROL_WORKFLOW_MODEL_RESULT'},finish_reason:'stop'}],
        usage:{prompt_tokens:100,completion_tokens:10,total_tokens:110}}));
    }
    if (text.includes('CONTROL_IMAGE_NEXT')) return writeResponse(response,null,null,'CONTROL_IMAGE_NEXT_DONE');
    if (text.includes('CONTROL_JOB_STOP')) {
      const jobResults = results.filter(result => result.tool === 'shell_exec');
      const launched = jobResults[0];
      if (!launched) return writeResponse(response, 'shell_exec', {
        command: text.includes('CONTROL_JOB_STOP_CANCEL') ? 'sleep 30' :
          `printf settled-output; exit ${text.includes('CONTROL_JOB_STOP_FAILED') ? 7 : 0}`,
        background: true,
      });
      assert.ok(launched.job_id,'后台启动必须产生持久 Job');
      for (const stopped of jobResults.slice(1)) {
        assert.equal(stopped.mode,'background_kill');assert.equal(stopped.status,'succeeded');
      }
      if (jobResults.length < 3) {
        return writeResponse(response, 'shell_exec', {action:'kill',job_id:launched.job_id});
      }
      return writeResponse(response, null, null, 'CONTROL_JOB_STOP_DONE');
    }
    if (text.includes('CONTROL_DELEGATION_GATE')) {
      const rejection = results.find(result => result.stage === 'tool_call_validation' && result.status === 'rejected');
      if (rejection) {
        assert.equal(rejection.tool, 'agent_spawn');
        assert.match(rejection.instruction, /主线直接完成/u);
        return writeResponse(response, null, null, 'CONTROL_DELEGATION_MAINLINE_DONE');
      }
      return writeResponse(response, 'agent_spawn', {
        task_name:'unnecessary_child', role:'executor', display_name:'不应创建的代理', goal:'简单配置检查',
        delegation:{purpose:'parallel_work',reason:'工作独立'},
        context_package:{summary:'普通检查',constraints:[],expected_output:'配置结论',references:[]},
      });
    }
    if (text.includes('CONTROL_NATIVE_PARTIAL')) {
      const coordinator = body.tools?.some(tool => tool.function?.name === 'magi_builtin_agent_spawn');
      if (!coordinator) {
        if (!results.some(result => result.tool === 'file_write' && result.status === 'succeeded')) {
          return writeResponse(response, 'file_write', {path:'native-partial-result.txt',content:'written once before provider failure'});
        }
        response.writeHead(400, {'content-type':'application/json'});
        response.end(JSON.stringify({error:{type:'invalid_request_error',message:'controlled failure after successful child write'}}));
        return;
      }
      const child = results.find(result => result.tool === 'agent_spawn')?.child_task_id;
      if (!child) return writeResponse(response, 'agent_spawn', {
        task_name:'partial_evidence',display_name:'失败前证据',role:'executor',
        delegation:{purpose:'user_requested',reason:'验收明确要求一个真实代理执行写入后失败并收集原证据'},
        goal:'CONTROL_NATIVE_PARTIAL 写入 native-partial-result.txt 后汇报',
        context_package:{summary:'CONTROL_NATIVE_PARTIAL',constraints:[],expected_output:'文件及执行证据',references:[]},
      });
      const waited = results.find(result => result.tool === 'agent_wait');
      if (!waited) return writeResponse(response,'agent_wait',{task_ids:[child],timeout_ms:60000});
      const cancelled = results.find(result => result.tool === 'agent_cancel');
      if (!cancelled) return writeResponse(response,'agent_cancel',{task_id:child,reason:'核对已失败代理的原始副作用'});
      return writeResponse(response,null,null,'CONTROL_NATIVE_PARTIAL_DONE');
    }
    if (text.includes('CONTROL_SESSION_AGENT')) {
      const coordinator = body.tools?.some(tool => tool.function?.name === 'magi_builtin_agent_spawn');
      const parentAgent=body.tools?.some(tool=>tool.function?.name==='magi_builtin_context_request');
      const transferredApproval = text.includes('CONTROL_TRANSFERRED_APPROVAL');
      const dirtyRecovery = text.includes('CONTROL_SESSION_AGENT_DIRTY');
      if (!coordinator) {
        if (text.includes('CONTROL_SESSION_AGENT_FAILED_DESCENDANT')) {
          response.writeHead(400, {'content-type':'application/json'});
          return response.end(JSON.stringify({error:{type:'invalid_request_error',message:'controlled terminal descendant before restart'}}));
        }
        if (dirtyRecovery) {
          if (!results.some(result => result.tool === 'file_write' && result.status === 'succeeded')) {
            return writeResponse(response, 'file_write', {path:'retained-before-restart.txt',content:'agent work before restart'});
          }
          if (results.some(result => ['answered','skipped'].includes(result.status))) {
            const read = results.findLast(result => result.tool === 'file_read');
            if (!read) return writeResponse(response, 'file_read', {path:'retained-before-restart.txt'});
            assert.equal(read.status, 'succeeded', '恢复必须重新挂接原代理目录');
            assert.ok(JSON.stringify(read).includes('agent work before restart'));
            return writeResponse(response, null, null, 'CONTROL_SESSION_AGENT_CHILD_DONE');
          }
        }
        if (text.includes('CONTROL_SESSION_AGENT_APPROVAL')) {
          if (results.some(result => result.tool === 'file_remove' && result.status === 'succeeded')) return writeResponse(response, null, null, 'CONTROL_SESSION_AGENT_CHILD_DONE');
          return writeResponse(response, 'file_remove', {path:transferredApproval?'transferred-approval.txt':target});
        }
        if (results.some(result => ['answered','skipped'].includes(result.status))) return writeResponse(response, null, null, 'CONTROL_SESSION_AGENT_CHILD_DONE');
        return writeResponse(response, 'ask_user_question', {questions:[{question:'子代理继续？',header:'代理',multiSelect:false,options:[{label:'A',description:'继续 A'},{label:'B',description:'继续 B'}]}]});
      }
      const spawned = results.filter(result => result.tool === 'agent_spawn' && result.child_task_id);
      const terminalChild = parentAgent && text.includes('CONTROL_TERMINAL_CHILD');
      if (terminalChild) {
        const failed = spawned[0]?.child_task_id;
        if (!failed) return writeResponse(response, 'agent_spawn', {
          task_name:'prior_failure',display_name:'原失败代理',role:'executor',
          delegation:{purpose:'user_requested',reason:'验收明确要求保留一个失败后代并只继续后续中断的工作'},
          goal:'CONTROL_SESSION_AGENT_FAILED_DESCENDANT',
          context_package:{summary:'受控失败，结果必须保留',constraints:[],expected_output:'失败回执',references:[]},
        });
        const collected = results.find(result => result.tool === 'agent_wait' && result.results?.some(item => item.child_task_id === failed));
        if (!collected) return writeResponse(response, 'agent_wait', {task_ids:[failed],timeout_ms:60000});
        assert.equal(collected.failed_count,1);
      }
      const child = spawned[terminalChild ? 1 : 0]?.child_task_id;
      if (!child) return writeResponse(response, 'agent_spawn', {task_name:'retained',delegation:{purpose:'user_requested',reason:'验收请求明确要求持续代理及直接/嵌套恢复'},display_name:'持续代理',role:'executor',lifetime:parentAgent?'turn':'session',
        allow_delegation:text.includes('CONTROL_SESSION_AGENT_NESTED')&&!parentAgent,
        goal:dirtyRecovery?'CONTROL_SESSION_AGENT_DIRTY':text.includes('CONTROL_SESSION_AGENT_APPROVAL')?`CONTROL_SESSION_AGENT_APPROVAL${transferredApproval?' CONTROL_TRANSFERRED_APPROVAL':''}`:'CONTROL_SESSION_AGENT_QUESTION',
        context_package:{summary:text.includes('CONTROL_SESSION_AGENT_NESTED')?`CONTROL_SESSION_AGENT_NESTED${text.includes('CONTROL_TERMINAL_CHILD')?' CONTROL_TERMINAL_CHILD':''}`:dirtyRecovery?'CONTROL_SESSION_AGENT_DIRTY':text.includes('CONTROL_SESSION_AGENT_APPROVAL')?`CONTROL_SESSION_AGENT_APPROVAL${transferredApproval?' CONTROL_TRANSFERRED_APPROVAL':''}`:'CONTROL_SESSION_AGENT_QUESTION',constraints:[],expected_output:'CONTROL_SESSION_AGENT_CHILD_DONE',references:[]}});
      if (results.findLast(result => result.tool === 'agent_wait' && result.results?.some(item => item.child_task_id === child))?.all_terminal) {
        if (text.includes('CONTROL_SESSION_AGENT_LIVE')) { liveParentHeld = true; await liveParentGate; }
        return writeResponse(response,null,null,'CONTROL_SESSION_AGENT_ROOT_DONE');
      }
      return writeResponse(response,'agent_wait',{task_ids:[child],timeout_ms:60000});
    }
    if (text.includes('CONTROL_CONTROL_GOAL')) {
      const goal = results.findLast(result => result.goal)?.goal;
      const plan = results.findLast(result => result.plan)?.plan;
      const forced = (body.tool_choice?.function?.name ?? (body.tools?.length === 1 ? body.tools[0].function?.name : ''))?.replace(/^magi_builtin_/u, '');
      if (forced === 'get_goal') return writeResponse(response, 'get_goal', {});
      if (!goal) return writeResponse(response, 'create_goal', { objective:'CONTROL_CONTROL_GOAL 完成原定目标' });
      if (forced === 'update_plan' || (completeGoal && plan?.items?.some(item => item.status !== 'completed'))) {
        return writeResponse(response, 'update_plan', { planId:plan?.planId ?? null, expectedRevision:plan?.revision ?? 0,
          expectedGoalId:goal.goalId, expectedGoalControlRevision:goal.controlRevision, language:'zh-CN',
          plan:[{itemId:plan?.items?.[0]?.itemId ?? null,step:'执行控制验收',status:completeGoal?'completed':'in_progress'}] });
      }
      if (!completeGoal) { goalHeld = true; await goalGate; }
      if (response.destroyed) return;
      if (goal.status !== 'complete') return writeResponse(response, 'update_goal', {goal_id:goal.goalId,expected_revision:goal.controlRevision,
        expected_plan_revision:plan?.revision ?? null,status:'complete',completion_summary:'App Server 目标控制已验收',evidence_refs:['control-fixture']});
      return writeResponse(response, null, null, 'CONTROL_GOAL_DONE');
    }
    if (text.includes('CONTROL_QUESTION')) {
      if (results.some(result => ['answered','skipped'].includes(result.status))) return writeResponse(response, null, null, 'CONTROL_QUESTION_DONE');
      return writeResponse(response, 'ask_user_question', {questions:[{question:'选择执行方案',header:'方案',multiSelect:false,options:[{label:'A',description:'方案 A'},{label:'B',description:'方案 B'}]}]});
    }
    if (text.includes('CONTROL_APPROVAL')) {
      if (results.some(result => result.tool === 'file_remove' && result.status === 'succeeded')) return writeResponse(response, null, null, 'CONTROL_APPROVAL_DONE');
      return writeResponse(response, 'file_remove', {path:target});
    }
    writeResponse(response, null, null, 'CONTROL_CONTROL_READY');
  } catch (error) { response.destroy(error); }
});
async function unusedPort() {
  const server = createServer(); await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
}
function processResult(child) {
  let stdout = ''; let stderr = '';
  child.stdout?.on('data', chunk => { stdout += chunk; }); child.stderr?.on('data', chunk => { stderr += chunk; });
  return new Promise((resolve,reject) => {
    const timer = setTimeout(() => {child.kill('SIGKILL');reject(new Error(`子进程超时: ${stderr.slice(-3000)}`));},240000);
    child.once('error',error=>{clearTimeout(timer);reject(error);});
    child.once('exit',(code,signal)=>{clearTimeout(timer);resolve({code,signal,stdout,stderr});});
  });
}
async function stopDaemon(signal = 'SIGTERM') {
  if (!daemon || daemon.exitCode !== null || daemon.signalCode !== null) return;
  const child = daemon;
  const ended = new Promise(resolve => child.once('exit', resolve));
  process.kill(-child.pid, signal);
  await Promise.race([ended,new Promise(resolve=>setTimeout(resolve,5000))]);
  if (child.exitCode === null && child.signalCode === null) { process.kill(-child.pid,'SIGKILL'); await ended; }
}
await new Promise(resolve => provider.listen(0,'127.0.0.1',resolve));
const port = await unusedPort();
const origin = `http://127.0.0.1:${port}`;
const endpoint = `ws://127.0.0.1:${port}/api/app-server`;
async function startDaemon() {
  daemon = spawn('./scripts/dev-daemon.sh',[],{cwd:root,detached:true,stdio:['ignore','pipe','pipe'],env:{...process.env,
    MAGI_PORT:String(port),MAGI_WEB_DEV:'0',MAGI_STATE_ROOT:join(fixture,'state'),MAGI_OPEN_BROWSER:'0',MAGI_TARGET_PRUNE_GIB:'1000',
    MAGI_BUILD_ID:'controls-fixture',MAGI_OPENAI_COMPAT_BASE_URL:`http://127.0.0.1:${provider.address().port}/v1`,
    MAGI_OPENAI_COMPAT_API_KEY:'local-fixture',MAGI_OPENAI_COMPAT_MODEL:'control-fixture'}});
  for (const stream of [daemon.stdout,daemon.stderr]) stream.on('data',chunk=>{logs=(logs+chunk).slice(-60000);});
  await waitFor(async()=>{if(daemon.exitCode!==null||daemon.signalCode!==null)throw new Error(logs.slice(-4000));return (await fetch(`${origin}/health`)).ok;},'独立 daemon 启动',240000,250);
}
const rpc = (method, params) => appServerRequest(endpoint, method, params);
const read = sessionId => rpc('session/read', { sessionId, includeTurns: true });
async function scoped(method, sessionId, params = {}) {
  const { session } = await read(sessionId);
  return rpc(method, { sessionId, workspaceId: session.workspaceId, ...params });
}
async function submit(params) {
  const scope = params.sessionId ? (await read(params.sessionId)).session : params;
  return rpc('turn/start', { scope: scope.workspaceId ? 'workspace' : 'personal',
    workspaceId: scope.workspaceId, locale: 'zh-CN',
    userMessageId: `user-${params.requestId}`, placeholderMessageId: `assistant-${params.requestId}`,
    ...params });
}
async function rejected(promise) {
  let error;
  try { await promise; } catch (value) { error = value; }
  assert.ok(error, '请求必须拒绝');
  return error;
}
async function terminal(session, marker) {
  return waitFor(async()=>{const snapshot=await read(session);
    const mainline=snapshot.turns.findLast(turn=>!turn.metadata?.agentExecution);
    if (!snapshot.session.isRunning && mainline?.status==='failed') throw new Error(JSON.stringify(snapshot.turns.map(turn=>({turnId:turn.turnId,status:turn.status,items:turn.items.map(item=>({kind:item.kind,content:item.content,tool:item.tool}))}))));
    return !snapshot.session.isRunning&&mainline?.status==='completed'&&mainline.items.some(item=>item.content?.includes(marker))&&snapshot;},marker,30000);
}
async function exists() { try {await access(target);return true;} catch {return false;} }
async function installLocalSkill(directoryPath) {
  const response=await fetch(`${origin}/api/settings/skills/install-local`,{method:'POST',
    headers:{'content-type':'application/json'},body:JSON.stringify({directoryPath})});
  assert.equal(response.status,200,await response.text());
}
async function assertUnavailableSkill(requestId, skillName) {
  const beforeMissingSkill = providerRequests;
  const missingSkill = await submit({requestId,skillName,text:'CONTROL_MISSING_SKILL'});
  const missingSkillTurn = await waitFor(async()=>{
    const snapshot = await read(missingSkill.sessionId);
    const turn = snapshot.turns.find(turn=>turn.turnId===missingSkill.turnId);
    return !snapshot.session.isRunning && turn?.status==='failed' && turn;
  },'失效 Skill 明确失败，不退回无限制工具面');
  assert.match(JSON.stringify(missingSkillTurn),/skill_unavailable/u);
  assert.equal(providerRequests,beforeMissingSkill,'失效 Skill 不能进入模型调用');
}
try {
  await startDaemon();
  await assertUnavailableSkill('control-missing-skill','removed-control-skill');
  const workflowSkill=join(fixture,'closed-workflow');
  await mkdir(workflowSkill);
  await writeFile(join(workflowSkill,'SKILL.md'),'# Workflow scope\nOnly web_fetch is allowed.');
  await writeFile(join(workflowSkill,'config.json'),JSON.stringify({allowed_tools:['web_fetch']}));
  const installedSkill=await fetch(`${origin}/api/settings/skills/install-local`,{method:'POST',
    headers:{'content-type':'application/json'},body:JSON.stringify({directoryPath:workflowSkill})});
  assert.equal(installedSkill.status,200,await installedSkill.text());
  liveStepSkill=join(fixture,'live-step-skill');
  await mkdir(liveStepSkill);
  await writeFile(join(liveStepSkill,'SKILL.md'),'# Live Step scope\nUse the current tool scope.');
  await writeFile(join(liveStepSkill,'config.json'),JSON.stringify({allowed_tools:['web_fetch']}));
  await installLocalSkill(liveStepSkill);
  const liveSkillStart = await submit({requestId:'control-live-skill-step',skillName:'live-step-skill',text:'CONTROL_LIVE_SKILL_STEP'});
  const liveSkillSnapshot = await terminal(liveSkillStart.sessionId,'CONTROL_LIVE_SKILL_DONE');
  const liveSkillTurn = liveSkillSnapshot.turns.find(turn=>turn.turnId===liveSkillStart.turnId);
  assert.equal(liveSkillTurn.modelSteps.length,4);
  assert.deepEqual(liveSkillTurn.items.filter(item=>item.tool).map(item=>item.tool.result.status),['rejected','succeeded','rejected']);
  await writeFile(join(liveStepSkill,'config.json'),'{');
  await installLocalSkill(liveStepSkill);
  await assertUnavailableSkill('control-corrupt-skill','live-step-skill');
  // 真实工作流 Worker、摘要展示与原请求在停用/升级/重启后的只读回执。
  const flowId='test.controls-flow', flowDirectory=join(fixture,'workflow'), flowArchive=join(fixture,'workflow.zip');
  await mkdir(flowDirectory);
  const flowManifest={sdkVersion:1,id:flowId,version:'1.0.0',name:'Controls Flow',description:'',
    backend:'plugin.mjs',permissions:[],dataSchemaVersion:1,settingsSchema:{type:'object',additionalProperties:false},
    contributions:{workflows:[{id:'main',title:'Controls Flow',description:''}]}};
  await writeFile(join(flowDirectory,'plugin.mjs'), `export default (input) => ({
    checkpoint_version:1, stage:input.stage === 'start' ? 'saved' : input.stage === 'saved' && input.user_input.includes('CONTROL_WORKFLOW_SCOPE_LIVE') ? 'activated' : 'modeled',
    action:input.stage === 'start' ? {kind:'save_stage',action_id:input.action_id,stage:'saved',checkpoint:{saved:true}} :
      input.stage === 'saved' && input.user_input.includes('CONTROL_WORKFLOW_SCOPE_LIVE') ? {kind:'tool_call',action_id:input.action_id,tool:'skill_apply',input:{skill_name:'closed-workflow'}} :
      ['saved','activated'].includes(input.stage) ? {kind:'model_request',action_id:input.action_id,model:null,system:'CONTROL_WORKFLOW_MODEL',context_refs:[],tools:[input.user_input.includes('CONTROL_WORKFLOW_SCOPE_') ? 'get_goal' : 'web_fetch']} :
      {kind:'complete',action_id:input.action_id,summary:input.config.saved && input.model_result?.content === 'CONTROL_WORKFLOW_MODEL_RESULT' ? 'CONTROL_WORKFLOW_DONE' : 'WORKFLOW_RESULT_LOST'}
  });`);
  const pluginPost=async(path,body)=>{
    const response=await fetch(`${origin}/api/plugins/${path}`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
    const result=await response.json();assert.equal(response.status,200,JSON.stringify(result));return result;
  };
  const packFlow=async()=>{
    await writeFile(join(flowDirectory,'manifest.json'),JSON.stringify(flowManifest));
    const packed=await processResult(spawn(process.execPath,[join(root,'scripts/pack-plugin.mjs'),flowDirectory,flowArchive],{cwd:root,stdio:['ignore','pipe','pipe']}));
    assert.equal(packed.code,0,packed.stderr);
  };
  await packFlow();await pluginPost('install',{source:`local:${flowArchive}`});
  await pluginPost(`${flowId}/enable`,{scope:'application'});await pluginPost(`${flowId}/activate`,{scope:'application'});
  const originalRequest={requestId:'control-workflow',workflowId:`plugin/${flowId}/main`,skillName:'closed-workflow'};
  const flowStarted=await submit(originalRequest);
  const flowTurn=(await terminal(flowStarted.sessionId,'CONTROL_WORKFLOW_DONE')).turns.find(turn=>turn.turnId===flowStarted.turnId);
  assert.equal(flowTurn.modelSteps.length,1,'工作流模型调用必须进入原生 Step 日志');
  assert.equal(flowTurn.modelSteps[0].status,'response_received');
  assert.equal(flowTurn.modelSteps[0].toolCount,1);
  const oldDigest=flowTurn.items.find(item=>item.kind==='user_message').metadata['magi.workflowPackageDigest'];assert.ok(oldDigest);
  for (const live of [false,true]) {
    const beforeDeniedWorkflow=providerRequests;
    const deniedWorkflow=await submit({...originalRequest,requestId:`control-workflow-scope-${live}`,
      skillName:live?null:'closed-workflow',text:live?'CONTROL_WORKFLOW_SCOPE_LIVE':'CONTROL_WORKFLOW_SCOPE_DENIED'});
    const deniedSnapshot=await waitFor(async()=>{const snapshot=await read(deniedWorkflow.sessionId);
      return !snapshot.session.isRunning&&snapshot.turns.find(turn=>turn.turnId===deniedWorkflow.turnId)?.status==='failed'&&snapshot;
    },'真实工作流拒绝初始/运行中 Skill 范围外的工具');
    const deniedTurn=deniedSnapshot.turns.find(turn=>turn.turnId===deniedWorkflow.turnId);
    assert.equal(deniedTurn.modelSteps.length,0);
    assert.match(JSON.stringify(deniedTurn),/get_goal/u);
    assert.equal(providerRequests,beforeDeniedWorkflow,'Skill 范围外的工具不能进入模型请求');
  }
  await pluginPost(`${flowId}/deactivate`,{scope:'application'});
  assert.equal((await submit(originalRequest)).turnId,flowStarted.turnId);
  await rejected(submit({...originalRequest,requestId:'control-workflow-disabled'}));
  flowManifest.version='1.1.0';await packFlow();await pluginPost('upgrade',{source:`local:${flowArchive}`});
  await pluginPost(`${flowId}/activate`,{scope:'application'});
  const nextFlow=await submit({...originalRequest,sessionId:flowStarted.sessionId,requestId:'control-workflow-upgraded'});
  const upgradedFlow=await terminal(nextFlow.sessionId,'CONTROL_WORKFLOW_DONE');
  const newDigest=upgradedFlow.turns.find(turn=>turn.turnId===nextFlow.turnId).items
    .find(item=>item.kind==='user_message').metadata['magi.workflowPackageDigest'];assert.notEqual(newDigest,oldDigest);
  const beforeFlowRestart=providerRequests;
  await stopDaemon('SIGKILL');await startDaemon();
  assert.equal((await submit(originalRequest)).turnId,flowStarted.turnId);
  const restoredFlow=(await read(flowStarted.sessionId)).turns;
  assert.equal(restoredFlow.length,2,'升级和重放不能产生第三个 Turn');
  assert.deepEqual(restoredFlow.find(turn=>turn.turnId===flowStarted.turnId).items,flowTurn.items);
  assert.equal(providerRequests,beforeFlowRestart,'工作流重启或重放不能重复调用模型');
  const rejectedSpawn = await submit({requestId:'control-delegation-gate',text:'CONTROL_DELEGATION_GATE 普通检查直接完成'});
  const afterGate = await terminal(rejectedSpawn.sessionId,'CONTROL_DELEGATION_MAINLINE_DONE');
  const gateTurn = afterGate.turns.find(turn=>turn.turnId===rejectedSpawn.turnId);
  const gateResult = gateTurn.items.find(item=>item.tool?.name==='agent_spawn')?.tool?.result;
  assert.equal(gateResult?.status,'rejected');
  assert.equal(gateResult.stage,'tool_call_validation');
  assert.equal(gateResult.child_task_id,undefined,'拒绝时没有 child task 回执');
  const requestsAfterGate = providerRequests;
  await stopDaemon('SIGKILL');await startDaemon();
  const reloadedGate = (await read(rejectedSpawn.sessionId)).turns.find(turn=>turn.turnId===rejectedSpawn.turnId);
  assert.deepEqual(reloadedGate.items.find(item=>item.tool?.name==='agent_spawn')?.tool?.result,gateResult);
  assert.equal(providerRequests,requestsAfterGate,'校验拒绝后的重载不重新请求模型或派发代理');
  const partial = await submit({workspaceId:(await fetch(`${origin}/api/workspaces/register`, {
    method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({path:workspace}),
  }).then(response=>response.json())).workspaceId,requestId:'native-partial',accessProfile:'full_access',
    text:'CONTROL_NATIVE_PARTIAL 使用一个真实子代理，检查失败前的工具事实，不重复写入或重新派发。'});
  const partialSnapshot = await terminal(partial.sessionId,'CONTROL_NATIVE_PARTIAL_DONE');
  const partialTurn = partialSnapshot.turns.find(turn=>turn.turnId===partial.turnId);
  const partialTools = partialTurn.items.filter(item=>item.tool).map(item=>item.tool);
  assert.equal(partialTools.filter(tool=>tool.name==='agent_spawn').length,1);
  const partialWait = partialTools.find(tool=>tool.name==='agent_wait').result;
  const partialCancel = partialTools.find(tool=>tool.name==='agent_cancel').result;
  assert.equal(partialWait.results[0].child_status,'failed');
  assert.equal(partialWait.results[0].activity.evidence_source,'canonical_tool_items');
  assert.equal(partialWait.results[0].activity.tool_calls,1);
  assert.deepEqual(partialWait.results[0].activity.files_changed,['native-partial-result.txt']);
  assert.deepEqual(partialCancel.results,partialWait.results,'等待与终态取消必须保留同一原生执行证据');
  assert.equal(await readFile(join(workspace,'native-partial-result.txt'),'utf8'),'written once before provider failure');
  const partialRequests = providerRequests;
  await stopDaemon('SIGKILL');await startDaemon();
  const restoredPartial = (await read(partial.sessionId)).turns.find(turn=>turn.turnId===partial.turnId);
  assert.deepEqual(restoredPartial.items.find(item=>item.tool?.name==='agent_wait').tool.result,partialWait);
  assert.equal(providerRequests,partialRequests,'失败后的原生副作用证据重载不能自动重放');
  const jobReceipts = [];
  for (const [scenario, status, exitCode] of [['COMPLETED','completed',0],['FAILED','failed',7],['CANCEL','cancelled',null]]) {
    const started = await submit({workspaceId:partialSnapshot.session.workspaceId,
      requestId:`job-stop-${scenario}`,accessProfile:'full_access',text:`CONTROL_JOB_STOP_${scenario}`});
    const snapshot = await terminal(started.sessionId,'CONTROL_JOB_STOP_DONE');
    const turn = snapshot.turns.find(turn=>turn.turnId===started.turnId);
    const calls = turn.items.filter(item=>item.tool?.name==='shell_exec');
    assert.equal(calls.length,3,'启动一次、停止两次；不重放命令或轮询模型');
    const job = calls[0].metadata.backgroundJob;
    assert.equal(job.status,status);assert.equal(job.exitCode,exitCode);
    for (const stopped of calls.slice(1)) {
      assert.equal(stopped.tool.result.status,'succeeded');
      assert.equal(stopped.tool.result.resources_settled,true);
    }
    assert.deepEqual(calls[2].tool.result.job,job,'重复停止读取原结算事实');
    if (scenario !== 'CANCEL') assert.deepEqual(calls[1].tool.result.job,job);
    jobReceipts.push({sessionId:started.sessionId,turnId:started.turnId,items:turn.items});
  }
  const jobRequests = providerRequests;
  await stopDaemon('SIGKILL');await startDaemon();
  for (const receipt of jobReceipts) {
    const turn = (await read(receipt.sessionId)).turns.find(turn=>turn.turnId===receipt.turnId);
    assert.deepEqual(turn.items,receipt.items,'重启保持 Job 退出码、revision、完成时间和停止回执');
  }
  assert.equal(providerRequests,jobRequests,'已结算 Job 重启后不得重放命令或重新请求模型');
  for (const skip of [false,true]) {
    const initial=await submit({requestId: skip?'control-skip-root':'control-answer-root', images: skip?[]:[{name:'control-image.png',dataUrl:imageDataUrl}], text: skip?'CONTROL_QUESTION_SKIP':'CONTROL_QUESTION_ANSWER'});
    const session=initial.sessionId;
    const question=await waitFor(async()=>{const pending=await scoped('session/interactions',session);return pending.pendingQuestions[0];},'App Server 待回答问题');
    assert.ok(question.turnId&&question.toolCallId&&question.canSkip);
    if (!skip) {
      const attemptsBeforeRestart=providerRequests;
      await stopDaemon('SIGKILL');await startDaemon();
      const recovered=(await scoped('session/interactions',session)).pendingQuestions[0];
      assert.equal(recovered.questionId,question.questionId);
      const restoredImageUser = (await read(session)).turns.flatMap(turn=>turn.items)
        .find(item=>item.kind==='user_message'&&item.content?.includes('CONTROL_QUESTION_ANSWER'));
      assert.deepEqual(restoredImageUser.metadata.images,[{name:'control-image.png',dataUrl:imageDataUrl}]);
      await rejected(submit({sessionId:session,requestId:'control-question-resume',resume:true,text:'继续执行中断的任务'}));
      assert.equal(providerRequests,attemptsBeforeRestart,'未答问题不能启动恢复请求');
    }

    const bad=await rejected(scoped('session/question/resolve',session,{questionId:'wrong-question',response:{kind:'skipped'}}));assert.ok(bad.message.includes('App Server'));
    assert.equal((await scoped('session/interactions',session)).pendingQuestions.length,1);
    if(skip) await scoped('session/question/resolve',session,{questionId:question.questionId,response:{kind:'skipped'}});
    else {
      const answers=[{selected:['A']}];
      const competing=await Promise.allSettled([scoped('session/question/resolve',session,{questionId:question.questionId,response:{kind:'answered',answers:answers}}),scoped('session/question/resolve',session,{questionId:question.questionId,response:{kind:'answered',answers:[{selected:['B']}]}})]);
      assert.equal(competing.filter(result=>result.status==='fulfilled').length,1,'两个客户端只有一个决定提交成功');
    }
    if(!skip) await submit({sessionId:session,requestId:'control-question-resume',resume:true,text:'继续执行中断的任务'});
    await terminal(session,'CONTROL_QUESTION_DONE');
    assert.equal((await scoped('session/interactions',session)).pendingQuestions.length,0);
  }
  const registered=await fetch(`${origin}/api/workspaces/register`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({path:workspace})});
  assert.equal(registered.status,200);const {workspaceId}=await registered.json();
  const approvalStart=await submit({workspaceId:workspaceId,requestId:'control-approval-root',text:'CONTROL_APPROVAL 删除指定文件'});
  const approvalSession=approvalStart.sessionId;
  const approval=await waitFor(async()=>{const pending=await scoped('session/interactions',approvalSession);return pending.pendingApprovals[0];},'App Server 持久审批');
  assert.equal(await exists(),true);
  const beforeRestart=providerRequests;
  await stopDaemon('SIGKILL');await startDaemon();
  const restored=(await scoped('session/interactions',approvalSession)).pendingApprovals[0];
  assert.equal(restored.approvalId,approval.approvalId);assert.equal(providerRequests,beforeRestart);
  const beforeRejectedContinue=await read(approvalSession);
  await rejected(submit({sessionId:approvalSession,requestId:'control-premature-continue',resume:true,text:'继续执行中断的任务'}));
  const afterRejectedContinue=await read(approvalSession);
  assert.deepEqual(afterRejectedContinue.turns,beforeRejectedContinue.turns,'未处理审批的 Continue 不得改写 canonical 或 Coordinator 来源');
  await scoped('session/approval/resolve',approvalSession,{approvalId:approval.approvalId,decision:'allow_once'});
  assert.equal(await exists(),true,'批准恢复审批本身不能启动副作用');
  assert.equal((await scoped('session/interactions',approvalSession)).pendingApprovals.length,0);
  await submit({sessionId:approvalSession,requestId:'control-premature-continue',resume:true,text:'继续执行中断的任务'});
  await terminal(approvalSession,'CONTROL_APPROVAL_DONE');assert.equal(await exists(),false);
  assert.equal(providerRequests,beforeRestart+2,'恢复只执行一次工具调用和结果汇报');
  const nestedWorkspace=join(fixture,'nested-workspace');
  await mkdir(nestedWorkspace);
  for (const args of [['init','-b','main'],['-c','user.name=Magi Test','-c','user.email=test@magi.invalid','commit','--allow-empty','-m','fixture']]) {
    const result=await processResult(spawn('git',['-C',nestedWorkspace,...args],{cwd:root,stdio:['ignore','pipe','pipe']}));
    assert.equal(result.code,0,result.stderr);
  }
  const nestedRegistered=await fetch(`${origin}/api/workspaces/register`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({path:nestedWorkspace})});
  assert.equal(nestedRegistered.status,200);
  const nestedWorkspaceId=(await nestedRegistered.json()).workspaceId;
  transferredApprovalTarget=join(nestedWorkspace,'transferred-approval.txt');
  for (const scenario of ['live','question','approval','nested','dirty']) {
    const approvalCase=scenario==='approval';
    const nestedCase=scenario==='nested';
    const dirtyCase=scenario==='dirty';
    await writeFile(target,'retained agent target');
    const marker=scenario==='live'?'CONTROL_SESSION_AGENT_LIVE':nestedCase?'CONTROL_SESSION_AGENT_NESTED':dirtyCase?'CONTROL_SESSION_AGENT_DIRTY':approvalCase?'CONTROL_SESSION_AGENT_APPROVAL':'CONTROL_SESSION_AGENT_QUESTION';
    const started=await submit({workspaceId:nestedCase||dirtyCase?nestedWorkspaceId:workspaceId,requestId:`retained-${scenario}`,text:marker});
    const session=started.sessionId;
    const pending=await waitFor(async()=>{
      const interactions=await scoped('session/interactions',session);
      return approvalCase?interactions.pendingApprovals[0]:interactions.pendingQuestions[0];
    },'独立 Agent Turn 交互',30000);
    if (dirtyCase) {
      const before = await read(session);
      assert.equal(before.turns.flatMap(turn=>turn.items).filter(item=>item.tool?.name==='file_write').length,1);
      await assert.rejects(access(join(nestedWorkspace,'retained-before-restart.txt')), '主目录不能代替代理目录承载改动');
    }
    assert.notEqual(pending.turnId,started.turnId);
    assert.ok(pending.turnId.startsWith('turn-agent-'));
    if (scenario==='live') {
      await scoped('session/question/resolve',session,{questionId:pending.questionId,response:{kind:'answered',answers:[{selected:['A']}]}});
      await waitFor(()=>liveParentHeld,'父轮已取得代理结果并保持运行');
      await waitFor(async()=>{
        const snapshot=await read(session);
        return snapshot.turns.find(turn=>turn.turnId===pending.turnId)?.status==='completed';
      },'父轮仍运行时代理终态写入原 Turn',30000);
      assert.equal((await read(session)).turns.find(turn=>turn.turnId===started.turnId)?.status,'running');
      releaseLiveParent();
      const completed=await terminal(session,'CONTROL_SESSION_AGENT_ROOT_DONE');
      assert.equal(completed.turns.find(turn=>turn.turnId===started.turnId)?.status,'completed');
      assert.equal(completed.turns.filter(turn=>turn.metadata?.agentExecution).length,1);
      continue;
    }
    const beforeRestart=providerRequests;
    await stopDaemon('SIGKILL');await startDaemon();
    const interactions=await scoped('session/interactions',session);
    const recovered=approvalCase?interactions.pendingApprovals[0]:interactions.pendingQuestions[0];
    assert.equal(recovered?.turnId,pending.turnId,'重启保留独立 Turn 交互来源');
    assert.equal(providerRequests,beforeRestart,'重启不自动重放代理');
    await rejected(submit({sessionId:session,requestId:`retained-resume-${scenario}`,resume:true,text:'继续执行中断的任务'}));
    if (approvalCase) await scoped('session/approval/resolve',session,{approvalId:pending.approvalId,decision:'allow_once'});
    else await scoped('session/question/resolve',session,{questionId:pending.questionId,response:{kind:'answered',answers:[{selected:['A']}]}});
    assert.equal(providerRequests,beforeRestart,'处理交互本身不运行代理');
    await submit({sessionId:session,requestId:`retained-resume-${scenario}`,resume:true,text:'继续执行中断的任务'});
    const finished=await terminal(session,'CONTROL_SESSION_AGENT_ROOT_DONE');
    assert.ok(finished.turns.some(turn=>turn.turnId===pending.turnId&&turn.status==='interrupted'));
    const agentTurns=finished.turns.filter(turn=>turn.metadata?.agentExecution);
    assert.equal(agentTurns.length,nestedCase?4:2,'恢复为每个 Agent Task 创建新的执行 Turn');
    const resumedAgent=agentTurns.find(turn=>turn.metadata.agentExecution.previousTurnId===pending.turnId);
    assert.equal(resumedAgent.status,'completed');
    assert.equal(resumedAgent.metadata.agentExecution.previousTurnId,pending.turnId);
    assert.equal(resumedAgent.metadata.agentExecution.taskId,agentTurns.find(turn=>turn.turnId===pending.turnId).metadata.agentExecution.taskId);
    if (dirtyCase) {
      assert.equal(finished.turns.flatMap(turn=>turn.items).filter(item=>item.tool?.name==='file_write').length,1,'恢复不重放写入，只读取原代理目录');
      await assert.rejects(access(join(nestedWorkspace,'retained-before-restart.txt')),'代理结果仍由分支保留，不能写入主目录');
    }
    if (approvalCase) assert.equal(await exists(),false);
    const finalInteractions=await scoped('session/interactions',session);
    assert.equal(finalInteractions.pendingQuestions.length,0);
    assert.equal(finalInteractions.pendingApprovals.length,0);
    const requestsBeforeReload=providerRequests;
    await stopDaemon('SIGKILL');await startDaemon();
    const reloaded=await read(session);
    assert.deepEqual(reloaded.turns.map(turn=>[turn.turnId,turn.status]),finished.turns.map(turn=>[turn.turnId,turn.status]));
    assert.equal(providerRequests,requestsBeforeReload,'完成后重启只恢复来源链，不重新执行');
  }
  // App Server 的真实转交入口必须与 HTTP 共用拥有方和幂等合同。
  // 原 App Server 断开、主线停止、daemon 重启都不能把观察操作变成新的 activation。
  for (const scenario of ['complete','restart','restart_approval','restart_nested']) {
    const approvalCase=scenario==='restart_approval';
    const nestedCase=scenario==='restart_nested';
    if(approvalCase) await writeFile(transferredApprovalTarget,'requires explicit approval');
    const started=await submit({workspaceId:nestedWorkspaceId,requestId:`transfer-${scenario}`,text:approvalCase?'CONTROL_SESSION_AGENT_APPROVAL CONTROL_TRANSFERRED_APPROVAL 转交并恢复原审批':nestedCase?'CONTROL_SESSION_AGENT_NESTED CONTROL_TERMINAL_CHILD 明确要求保留失败后代，再启动父子两层代理独立运行并转交恢复':'CONTROL_SESSION_AGENT_TRANSFER 运行独立代理并等待问题'});
    const session=started.sessionId;
    const pending=await waitFor(async()=>{
      const interactions=await scoped('session/interactions',session);
      return approvalCase?interactions.pendingApprovals[0]:interactions.pendingQuestions[0];
    },'转交代理的交互',30000);
    const snapshot=await read(session);
    const spawned=snapshot.turns.find(turn=>turn.turnId===started.turnId).items.map(item=>item.tool?.result)
      .find(result=>result?.child_task_id);
    const independentTask=spawned.child_task_id;
    const sourceAgentTurn=snapshot.turns.find(turn=>turn.metadata?.agentExecution?.taskId===independentTask);
    const failedAgentTurn=nestedCase?snapshot.turns.find(turn=>turn.metadata?.agentExecution?.parentTaskId===independentTask&&turn.status==='failed'):null;
    if(nestedCase) assert.ok(failedAgentTurn,'中断前已有一个真正失败并收集的后代');
    const projection=async()=>{
      const query=new URLSearchParams({scope:'workspace',workspaceId:nestedWorkspaceId,sessionId:session});
      const response=await fetch(`${origin}/api/agent-runs/projection/${independentTask}?${query}`);
      assert.equal(response.status,200);return response.json();
    };
    const failedTask=nestedCase?(await projection()).tasks.find(task=>task.task_id===failedAgentTurn.metadata.agentExecution.taskId):null;
    if(nestedCase) assert.equal(failedTask?.status,'failed');
    assert.ok(spawned?.activation?.generation,'转交版本必须来自服务端 activation');
    const transferParams={scope:'workspace',taskId:independentTask,expectedParentTurnId:started.turnId,
      expectedGeneration:spawned.activation.generation,operationId:scenario==='complete'?'转'.repeat(200):`transfer-${scenario}`,owner:'session'};
    await rejected(scoped('agent/transfer',session,{...transferParams,operationId:'x'.repeat(201)}));
    await rejected(scoped('agent/transfer',session,{...transferParams,expectedGeneration:spawned.activation.generation+1}));
    const transferred=await scoped('agent/transfer',session,transferParams);
    assert.equal(transferred.taskId,independentTask);
    assert.equal(transferred.sourceTurnId,started.turnId);
    assert.equal(transferred.runnerStarted,true);
    assert.deepEqual(await scoped('agent/transfer',session,transferParams),transferred,'App Server 幂等重试读取原转交');
    await scoped('turn/interrupt',session,{expectedTurnId:started.turnId});
    const stopped=await read(session);
    assert.equal(stopped.turns.find(turn=>turn.turnId===started.turnId)?.status,'cancelled');
    const afterStop=await scoped('session/interactions',session);
    assert.equal(approvalCase?afterStop.pendingApprovals[0]?.approvalId:afterStop.pendingQuestions[0]?.questionId,
      approvalCase?pending.approvalId:pending.questionId,'主线停止不能消费独立代理的交互');
    if (scenario.startsWith('restart')) {
      const requestsBeforeRestart=providerRequests;
      await stopDaemon('SIGKILL');await startDaemon();
      const restored=await scoped('session/interactions',session);
      assert.equal(approvalCase?restored.pendingApprovals[0]?.approvalId:restored.pendingQuestions[0]?.questionId,
        approvalCase?pending.approvalId:pending.questionId);
      const replay=await scoped('agent/transfer',session,transferParams);
      assert.deepEqual(replay,{...transferred,runnerStarted:false});
      assert.equal(providerRequests,requestsBeforeRestart,'重启后读取原转交不重启代理');
      const continuation={scope:'workspace',taskId:independentTask,expectedTurnId:sourceAgentTurn.turnId,expectedGeneration:spawned.activation.generation,operationId:scenario==='restart'?'续'.repeat(200):`independent-continue-${scenario}`};
      await rejected(scoped('agent/continue',session,continuation));
      assert.equal(providerRequests,requestsBeforeRestart,'原交互未处理时继续必须拒绝且不执行');
      if(approvalCase) {
        await scoped('session/approval/resolve',session,{approvalId:pending.approvalId,decision:'allow_once'});
        await access(transferredApprovalTarget);
        await rejected(scoped('session/approval/resolve',session,{approvalId:pending.approvalId,decision:'allow_once'}));
      } else {
        await scoped('session/question/resolve',session,{questionId:pending.questionId,response:{kind:'answered',answers:[{selected:['A']}]}});
        await rejected(scoped('session/question/resolve',session,{questionId:pending.questionId,response:{kind:'answered',answers:[{selected:['A']}]}}));
      }
      const recovered=await read(session);
      assert.equal(recovered.turns.find(turn=>turn.turnId===pending.turnId)?.status,'interrupted');
      assert.equal(recovered.turns.filter(turn=>turn.metadata?.agentExecution).length,nestedCase?3:1);
      if(nestedCase) assert.deepEqual(recovered.turns.find(turn=>turn.turnId===failedAgentTurn.turnId),failedAgentTurn);
      assert.equal(providerRequests,requestsBeforeRestart,'答复已中断后台代理不自动重放 Step');
      const projected=(await projection()).agents.find(agent=>agent.agentRunId===independentTask);
      assert.equal(projected.continueRequest?.expectedTurnId,sourceAgentTurn.turnId,'GUI 的继续命令不能被原失败后代阻断');
      assert.equal(projected.continueUnavailableReason,null);
      await rejected(scoped('agent/continue',session,{...continuation,expectedTurnId:'stale-agent-turn'}));
      await rejected(scoped('agent/continue',session,{...continuation,expectedGeneration:spawned.activation.generation+1}));
      await rejected(scoped('agent/continue',session,{...continuation,operationId:'x'.repeat(201)}));
      await rejected(scoped('agent/continue',session,{...continuation,operationId:'续'.repeat(201)}));
      assert.deepEqual((await read(session)).turns,recovered.turns,'无效 operation 不创建恢复 Turn 或改写旧事实');
      const responses=await Promise.all([scoped('agent/continue',session,continuation),scoped('agent/continue',session,continuation)]);
      const resumed=responses.find(response=>!response.replayed);
      assert.ok(resumed,'两个客户端只接纳一个继续');
      assert.equal(resumed.sourceTurnId,sourceAgentTurn.turnId);
      assert.equal(resumed.taskId,independentTask);
      assert.deepEqual(responses.find(response=>response.replayed),{...resumed,replayed:true});
      const completed=await waitFor(async()=>{const snapshot=await read(session);return snapshot.turns.find(turn=>turn.turnId===resumed.turnId)?.status==='completed'&&snapshot;},'独立继续原代理并结算新 Turn',30000);
      assert.equal(completed.turns.find(turn=>turn.turnId===started.turnId)?.status,'cancelled','继续代理不能恢复原父轮');
      assert.equal(completed.turns.find(turn=>turn.turnId===pending.turnId)?.status,'interrupted');
      assert.equal(completed.turns.filter(turn=>turn.metadata?.agentExecution).length,nestedCase?5:2);
      if(nestedCase) {
        assert.deepEqual(completed.turns.find(turn=>turn.turnId===failedAgentTurn.turnId),failedAgentTurn,'继续不得改写原失败证据');
        assert.equal(completed.turns.filter(turn=>turn.metadata?.agentExecution?.taskId===failedAgentTurn.metadata.agentExecution.taskId).length,1,'失败后代不得重新派发');
        assert.deepEqual((await projection()).tasks.find(task=>task.task_id===failedTask.task_id),failedTask,'失败 Task 的结果、时间和恢复代际均保持不变');
      }
      const requestsAfterCompletion=providerRequests;
      assert.equal(requestsAfterCompletion - requestsBeforeRestart, nestedCase ? 3 : approvalCase ? 2 : 1,
        '恢复直接继承原答复；批准只执行原操作并汇报，不重复询问或空转');
      assert.deepEqual(await scoped('agent/continue',session,continuation),{...resumed,replayed:true});
      assert.equal(providerRequests,requestsAfterCompletion,'幂等回执不能启动新的执行');
      if(approvalCase) {
        await access(transferredApprovalTarget);
        const removals=completed.turns.flatMap(turn=>turn.items).filter(item=>
          item.tool?.name==='file_remove'&&item.tool.result?.status==='succeeded');
        assert.equal(removals.length,1, '原批准只产生一个成功的删除结果');
        assert.equal(removals[0].turnId,resumed.turnId);
        assert.notEqual(removals[0].tool.result.path,transferredApprovalTarget,'副作用只能落在代理原副本');
        await assert.rejects(access(removals[0].tool.result.path));
      }
      await stopDaemon('SIGKILL');await startDaemon();
      assert.deepEqual(await scoped('agent/continue',session,continuation),{...resumed,replayed:true});
      assert.equal(providerRequests,requestsAfterCompletion,'再次重启读取继续回执不重放');
    } else {
      await scoped('session/question/resolve',session,{questionId:pending.questionId,response:{kind:'answered',answers:[{selected:['A']}]}});
      const completed=await waitFor(async()=>{
        const snapshot=await read(session);
        return snapshot.turns.find(turn=>turn.turnId===pending.turnId)?.status==='completed'&&snapshot;
      },'转交代理结算原 Turn',30000);
      assert.equal(completed.turns.find(turn=>turn.turnId===started.turnId)?.status,'cancelled');
      assert.equal(completed.turns.filter(turn=>turn.metadata?.agentExecution).length,1);
      assert.deepEqual(await scoped('agent/transfer',session,transferParams),{...transferred,runnerStarted:false},'终态转交重试不启动执行');
    }
    assert.equal((await scoped('session/interactions',session)).pendingQuestions.length,0);
    assert.equal((await scoped('session/interactions',session)).pendingApprovals.length,0);
  }
  const goalStart=await submit({requestId:'control-goal-start',text:'CONTROL_CONTROL_GOAL 完成原定目标',goalMode:true});
  const goalSession=goalStart.sessionId;
  await waitFor(()=>goalHeld,'Goal 已创建并保持在途');
  const current=await scoped('goal/read',goalSession,{scope:'personal'});
  assert.equal(current.goal.status,'active');assert.equal(current.allowedActions.canPause,true);
  const goalParams=snapshot=>({scope:'personal',goalId:snapshot.goal.goalId,expectedRevision:snapshot.goal.controlRevision,expectedPlanRevision:snapshot.plan?.revision});
  const paused=await scoped('goal/pause',goalSession,goalParams(current));assert.equal(paused.goal.status,'paused');
  await waitFor(async()=>{
    const snapshot=await read(goalSession);
    return !snapshot.session.isRunning&&snapshot.turns.find(turn=>turn.turnId===goalStart.turnId)?.status==='cancelled';
  },'暂停 Goal 结算其拥有的 Turn',30000);
  await rejected(scoped('goal/resume',goalSession,goalParams(current)));
  const updated=await scoped('goal/update',goalSession,{...goalParams(paused),objective:'CONTROL_CONTROL_GOAL 更新验收要求'});
  assert.equal(updated.goal.objective,'CONTROL_CONTROL_GOAL 更新验收要求');assert.ok(updated.goal.controlRevision>paused.goal.controlRevision);
  completeGoal=true;releaseGoal();
  const resumed=await scoped('goal/resume',goalSession,goalParams(updated));assert.equal(resumed.goal.status,'active');
  const completed=await waitFor(async()=>{const snapshot=await scoped('goal/read',goalSession,{scope:'personal'});return snapshot.goal?.status==='complete'&&snapshot;},'Goal 恢复后完成',30000);
  assert.ok(completed.goal.tokensUsed>0);assert.equal('tokenBudget' in completed.goal,false);
  const cleared=await scoped('goal/clear',goalSession,goalParams(completed));assert.equal(cleared.goal,null);assert.equal(cleared.plan,null);
  process.stdout.write(JSON.stringify({passed:true,providerRequests,checks:['同一 Turn 内 Skill 工具目录热更新与拒绝保持','真实工作流 Worker/摘要/升级重启/原回执保持','完成/失败/取消 Job 重复停止与 SIGKILL 后原回执保持','子代理写入后失败的等待/取消/重启证据一致且不重放','无效代理决策拒绝/主线接管/SIGKILL 后回执保留','真实 App Server 待交互读取','明确答复/跳过','双客户端一次提交','错误交互身份拒绝','SIGKILL 后问题/审批身份恢复','独立 Agent Turn 的交互/显式 Continue/再次重启','父轮仍运行时代理原 Turn 已完成','App Server 转交版本检查/幂等/主线停止后独立完成','转交后 SIGKILL/交互恢复/读取回执不重放','直接与嵌套转交子树显式继续','双客户端同 operation 单次接纳','原中断记录保持终态','批准后显式 Continue','文件删除仅执行一次','Goal 暂停/更新/恢复/完成/清除','旧 revision 拒绝','用量仅统计']} )+'\n');
} catch(error) {
  if (process.env.MAGI_KEEP_TEST_FIXTURE === '1') await writeFile(join(fixture, 'last-provider-request.json'), JSON.stringify(lastProviderRequest, null, 2));
  process.stderr.write(logs.slice(-12000)+'\n');throw error;
}
finally {releaseGoal();releaseLiveParent();await stopDaemon();provider.closeAllConnections();await new Promise(resolve=>provider.close(resolve));
  if (process.env.MAGI_KEEP_TEST_FIXTURE==='1') process.stderr.write(`fixture: ${fixture}\n`);
  else await rm(fixture,{recursive:true,force:true});}
