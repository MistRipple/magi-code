import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { waitFor } from './lib/electron-harness.mjs';
import { appServerRequest } from './lib/app-server-harness.mjs';

// 真实 daemon + App Server，Provider 只用本地替身：验证“通过对话创建／修改角色”的完整链路——
// Skill 声明的工具才开放、保存前必须由用户确认、拒绝不写入、修改带版本号、角色文件与注册表一致。
const root = new URL('..', import.meta.url).pathname;
const fixture = await mkdtemp(join(tmpdir(), 'magi-role-conversation-'));
let daemon, logs = '', providerFailure;
const SKILL = 'magi-create-agent-role';

const created = {
  role_id: 'data-analyst', display_name: '数据分析师', description: '分析给定数据并给出结论与证据',
  system_prompt: '你负责分析父任务给出的数据，输出结论、证据与未完成部分。', constraints: ['read-only'],
  capabilities: ['general_engineering'],
};
const updated = {
  role_id: 'data-analyst', display_name: '数据分析师',
  system_prompt: '你负责分析父任务给出的数据，先说明统计口径，再输出结论、证据与未完成部分。',
  capabilities: ['general_engineering'], expected_role_revision: 1,
};
// 每个场景：提供者依次发出的工具调用；全部工具结果回灌后给出最终文本。
const scenarios = {
  ROLE_CREATE: [['agent_role_get', {}], ['agent_role_save', created]],
  ROLE_UPDATE: [['agent_role_get', { role_id: 'data-analyst' }], ['agent_role_save', updated]],
  ROLE_DENY: [['agent_role_save', { ...updated, system_prompt: '被拒绝的改动', expected_role_revision: 2 }]],
  ROLE_BUILTIN: [['agent_role_save', { ...created, role_id: 'executor' }]],
};

function respond(response, tool, args, content) {
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  const delta = tool ? { role: 'assistant', tool_calls: [{ index: 0, id: `call-${tool}-${Math.random().toString(16).slice(2)}`,
    type: 'function', function: { name: `magi_builtin_${tool}`, arguments: JSON.stringify(args) } }] } : { role: 'assistant', content };
  response.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`);
  response.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: tool ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } })}\n\ndata: [DONE]\n\n`);
}
const provider = createServer(async (request, response) => {
  try {
    if (request.method === 'GET') {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ data: [{ id: 'role-fixture' }] })); return;
    }
    let raw = '';
    for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw);
    const text = JSON.stringify(body.messages);
    const names = (body.tools ?? []).map(tool => tool.function.name);
    const marker = [...Object.keys(scenarios), 'PLAIN_TURN'].find(name => text.includes(name));
    const results = (body.messages ?? []).filter(message => message.role === 'tool').map(message => JSON.parse(message.content));
    if (marker === 'PLAIN_TURN') {
      assert.ok(!names.some(name => name.includes('agent_role_')), `没有 Skill 声明时不能出现角色工具：${names}`);
      return respond(response, null, null, 'PLAIN_DONE');
    }
    for (const name of ['agent_role_get', 'agent_role_save', 'ask_user_question']) {
      assert.ok(names.includes(`magi_builtin_${name}`), `Skill 声明的工具必须开放：${name}`);
    }
    for (const name of ['file_write', 'shell_exec', 'memory_write', 'agent_spawn']) {
      assert.ok(!names.includes(`magi_builtin_${name}`), `Skill 限定后不应开放 ${name}`);
    }
    const steps = scenarios[marker];
    if (results.length < steps.length) return respond(response, steps[results.length][0], steps[results.length][1]);
    return respond(response, null, null, `${marker}_DONE`);
  } catch (error) {
    providerFailure = error;
    response.writeHead(400, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ error: { message: String(error) } }));
  }
});
async function unusedPort() {
  const server = createServer(); await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port;
}
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
const port = await unusedPort();
const origin = `http://127.0.0.1:${port}`;
const rpc = (method, params) => appServerRequest(`ws://127.0.0.1:${port}/api/app-server`, method, params);
async function json(path, body) {
  const response = await fetch(`${origin}${path}`, body === undefined ? undefined
    : { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  assert.ok(response.ok, `${path}: ${response.status} ${await response.clone().text()}`); return response.json();
}
async function start() {
  daemon = spawn('./scripts/dev-daemon.sh', [], { cwd: root, detached: true, stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, MAGI_PORT: String(port), MAGI_WEB_DEV: '0', MAGI_OPEN_BROWSER: '0', MAGI_STATE_ROOT: join(fixture, 'state'),
      MAGI_TARGET_PRUNE_GIB: '1000', MAGI_BUILD_ID: 'role-conversation-fixture' } });
  for (const stream of [daemon.stdout, daemon.stderr]) stream.on('data', chunk => { logs = (logs + chunk).slice(-12000); });
  await waitFor(async () => { if (daemon.exitCode !== null) throw new Error(logs); return (await fetch(`${origin}/health`)).ok; }, 'daemon 启动', 300000);
}
async function stop() {
  if (!daemon || daemon.exitCode !== null) return;
  const done = once(daemon, 'exit'); process.kill(-daemon.pid, 'SIGTERM'); await done;
}
const templates = async () => (await json('/api/settings/registry/role-templates')).templates;
const roleFile = join(fixture, 'state/roles/data-analyst.md');

// decision: 对出现的第一个确认请求的处理；返回确认请求本身供断言。
async function run(marker, { skill = SKILL, decision, beforeResolve } = {}) {
  const accepted = await rpc('turn/start', { scope: 'personal', text: `${marker} 请处理角色`, requestId: marker, userMessageId: `user-${marker}`,
    ...(skill ? { skillName: skill } : {}),
    orchestratorSessionConfig: { sourceId: 'custom/fixture', model: 'role-fixture' }, accessProfile: 'restricted' });
  let approval, turn;
  const deadline = Date.now() + 90000;
  while (Date.now() < deadline) {
    if (providerFailure) throw providerFailure;
    const interactions = await rpc('session/interactions', { sessionId: accepted.sessionId });
    const pending = interactions.pendingApprovals[0];
    if (pending && !approval) {
      approval = pending;
      await beforeResolve?.(pending);
      await rpc('session/approval/resolve', { sessionId: accepted.sessionId, approvalId: pending.approvalId, decision });
    }
    turn = (await rpc('session/read', { sessionId: accepted.sessionId, includeTurns: true })).turns.find(item => item.turnId === accepted.turnId);
    if (turn && ['failed', 'cancelled', 'completed'].includes(turn.status)) break;
    await delay(150);
  }
  assert.equal(turn?.status, 'completed', `${marker} 未完成：${JSON.stringify(turn)}`);
  return { approval, sessionId: accepted.sessionId };
}
try {
  await start();
  await json('/api/settings/orchestrator/save', { expectedRevision: 0, sources: [{ id: 'fixture', name: 'Fixture',
    baseUrl: `http://127.0.0.1:${provider.address().port}/v1`, apiKey: 'fixture', apiProtocol: 'openai_chat', models: [{ id: 'role-fixture' }] }] });

  // 没有 Skill 声明时，默认工具面里没有角色工具。
  await run('PLAIN_TURN', { skill: null });
  console.log('✓ 没有 Skill 声明时不开放角色工具');

  // 创建：保存前必须有用户确认，确认前注册表和文件都不能变化。
  const create = await run('ROLE_CREATE', { decision: 'allow_once', async beforeResolve(approval) {
    assert.equal(approval.toolName, 'agent_role_save');
    assert.match(approval.reason, /创建自定义角色「数据分析师」/);
    assert.ok(!approval.reason.includes('general_engineering'), `确认说明用能力显示名而不是内部 id：${approval.reason}`);
    assert.ok(!(await templates()).some(role => role.templateId === 'data-analyst'), '确认前不能写入注册表');
    await assert.rejects(access(roleFile), '确认前不能写入角色文件');
  } });
  assert.ok(create.approval, '创建必须经过用户确认');
  let role = (await templates()).find(item => item.templateId === 'data-analyst');
  assert.ok(role, '确认后角色进入注册表');
  assert.equal(role.displayName, '数据分析师');
  const markdown = await readFile(roleFile, 'utf8');
  assert.match(markdown, /capabilities: \["general_engineering"\]/);
  assert.match(markdown, /read-only/);
  console.log('✓ 对话创建角色：确认前不写入，确认后注册表与角色文件一致');

  // 修改：带版本号，摘要只列变化，保存后版本递增。
  const update = await run('ROLE_UPDATE', { decision: 'allow_once', async beforeResolve(approval) {
    assert.match(approval.reason, /修改自定义角色「数据分析师」[\s\S]*系统提示词已修改/);
    assert.ok(!approval.reason.includes('约束'), `未改动的字段不进入摘要：${approval.reason}`);
  } });
  assert.ok(update.approval);
  const updatedMarkdown = await readFile(roleFile, 'utf8');
  assert.match(updatedMarkdown, /先说明统计口径/);
  assert.match(updatedMarkdown, /read-only/, '省略的字段沿用现有值');
  assert.match(updatedMarkdown, /role_revision: 2|roleRevision: 2/);
  console.log('✓ 对话修改角色：版本号递增，未提供的字段沿用');

  // 用户拒绝：角色保持不变。
  const before = await readFile(roleFile, 'utf8');
  const deny = await run('ROLE_DENY', { decision: 'deny' });
  assert.ok(deny.approval);
  assert.equal(await readFile(roleFile, 'utf8'), before, '用户拒绝后角色不能被修改');
  console.log('✓ 用户拒绝确认后角色保持不变');

  // 内置角色不能被覆盖：在请用户确认之前就失败。
  const builtin = await run('ROLE_BUILTIN', { decision: 'allow_once' });
  assert.equal(builtin.approval, undefined, '无法保存的角色不能要求用户确认');
  await assert.rejects(access(join(fixture, 'state/roles/executor.md')), '内置角色不能被覆盖');
  console.log('✓ 内置角色不能被覆盖，且不会请用户确认一份无法保存的角色');
  assert.equal(providerFailure, undefined);
} catch (error) {
  console.error(logs.slice(-5000)); if (providerFailure) console.error(providerFailure);
  throw error;
} finally {
  await stop(); provider.closeAllConnections(); await new Promise(resolve => provider.close(resolve));
  await rm(fixture, { recursive: true, force: true });
}
