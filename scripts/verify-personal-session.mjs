import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, rm, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import electron from 'electron';
import { connectTarget, waitFor } from './lib/electron-harness.mjs';
import { appServerRequest } from './lib/app-server-harness.mjs';
import { personalMatrixMarkers, personalMatrixStep, personalAgentStep } from './lib/personal-tool-matrix.mjs';

// 隔离状态根 + 真实 daemon/App Server/Chromium；Provider 只用本地替身。
const root = new URL('..', import.meta.url).pathname;
const fixture = await mkdtemp(join(tmpdir(), 'magi-personal-live-'));
let daemon, browser, page, logs = '';
const image = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aZ1cAAAAASUVORK5CYII=';
const errors = [];
let providerCalls = 0;
let providerFailure;
const runToolMatrix = process.argv.includes('--tools') || process.env.MAGI_PERSONAL_TOOL_MATRIX === '1';
const matrixCalls = new Set();
function respond(response, tool, args, content) {
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  const delta = tool ? { role: 'assistant', tool_calls: [{ index: 0, id: `call-${tool}-${providerCalls}`,
    type: 'function', function: { name: `magi_builtin_${tool}`, arguments: JSON.stringify(args) } }] } : { role: 'assistant', content };
  response.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`);
  response.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: tool ? 'tool_calls' : 'stop' }], usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } })}\n\ndata: [DONE]\n\n`);
}
const provider = createServer(async (request, response) => {
  try {
    if (request.method === 'GET') {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ data: [{ id: 'personal-fixture' }] })); return;
    }
    let raw = '';
    for await (const chunk of request) raw += chunk;
    if (request.url.endsWith('/images/generations')) {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end(JSON.stringify({ data: [{ b64_json: image }] })); return;
    }
    providerCalls++;
    const body = JSON.parse(raw);
    const text = JSON.stringify(body.messages);
    const project = text.includes('PROJECT_CASE');
    const gitCase = text.includes('PERSONAL_GIT_CASE');
    const marker = project ? 'PROJECT_CASE' : text.includes('PERSONAL_B') ? 'PERSONAL_B' : 'PERSONAL_A';
    const names = (body.tools ?? []).map(tool => tool.function.name);
    const matrix = personalMatrixMarkers.find(marker => text.includes(marker));
    const agentCase = matrix?.endsWith('AGENTS') || matrix?.endsWith('CANCEL');
    if (matrix !== 'PERSONAL_MATRIX_GOAL' && !agentCase) {
      for (const name of ['file_write', 'shell_exec', 'search_text', 'update_execution_steps']) assert.ok(names.includes(`magi_builtin_${name}`), name);
      assert.ok(names.includes('magi_builtin_git_status'));
    }
    assert.equal(names.includes('magi_builtin_memory_write'), project);
    assert.equal(names.includes('magi_builtin_search_semantic'), project);
    if (!project) {
      assert.ok(text.includes('未关联项目的个人会话'));
      assert.ok(!text.includes('默认指这个工作区'));
      assert.ok(!text.includes('PROJECT_RULE_MUST_NOT_LEAK'));
    }
    const toolMessages = (body.messages ?? []).filter(message => message.role === 'tool');
    const results = toolMessages.map(message => {
      // view_image 的 Provider 回灌是图片 + 可读说明，canonical 结果仍为结构化事实。
      if (typeof message.content === 'string' && message.content.startsWith('已读取图片 ')) {
        return { tool: 'view_image', status: 'succeeded' };
      }
      return JSON.parse(message.content);
    });
    if (!agentCase) for (const result of results) assert.equal(result.status,
      result.tool === 'ask_user_question' ? 'answered' : 'succeeded', JSON.stringify(result));
    if (matrix) {
      if (agentCase) for (const result of results) {
        assert.ok(!result.error_code, JSON.stringify(result));
        assert.ok(['succeeded', 'started', 'queued', 'sent', 'completed', 'timeout', 'timed_out', 'waiting', 'attention_required', 'ok', 'resolved', 'cancelled', 'killed'].includes(result.status), JSON.stringify(result));
      }
      const next = agentCase ? personalAgentStep(matrix, results, names.includes('magi_builtin_context_search')) : personalMatrixStep(matrix, results, toolMessages, origin);
      if (!next) return respond(response, null, null, `${matrix}_DONE`);
      assert.ok(names.includes(`magi_builtin_${next[0]}`), `${matrix}: 模型工具目录缺少 ${next[0]}`);
      matrixCalls.add(next[0]);
      return respond(response, ...next);
    }
    if (gitCase) {
      const calls = results.map(result => result.tool);
      if (!calls.length) return respond(response, 'shell_exec', {
        command: 'git init -b main repo-a && git -C repo-a config user.name Fixture && git -C repo-a config user.email fixture@example.test && git -C repo-a commit --allow-empty -m initial && git init -b main repo-b && git -C repo-b config user.name Fixture && git -C repo-b config user.email fixture@example.test && git -C repo-b commit --allow-empty -m initial', access_mode: 'explicit_write',
      });
      if (!calls.includes('git_status')) return respond(response, 'git_status', { repositoryPath: 'repo-a' });
      if (!calls.includes('git_branch_create')) return respond(response, 'git_branch_create', { repositoryPath: 'repo-a', branch: 'feature' });
      if (!calls.includes('git_branch_list')) return respond(response, 'git_branch_list', { repositoryPath: 'repo-b' });
      return respond(response, null, null, 'PERSONAL_GIT_DONE');
    }
    const stepsResult = results.findLast(result => result.tool === 'update_execution_steps');
    const writeIndex = results.findIndex(result => result.tool === 'file_write');
    const searchIndex = results.findIndex(result => result.tool === 'shell_exec');
    const step = (stepId, title, status, refs = []) => ({ stepId, title, status, dependsOn: [], evidenceRefs: refs, taskIds: [] });
    const revision = stepsResult?.execution?.revision ?? 0;
    const writeRef = writeIndex < 0 ? [] : [toolMessages[writeIndex].tool_call_id];
    if (!stepsResult) return respond(response, 'update_execution_steps', { expectedRevision: 0, steps: [step('write', '生成报告', 'running'), step('verify', '核验报告', 'pending')] });
    const files = [
      ['report.md', `${marker} report\n`],
      ['index.html', `<link rel="stylesheet" href="style.css"><main>${marker}_SITE</main><script src="app.js"></script>`],
      ['style.css', 'main { color: rgb(12, 34, 56); }'],
      ['app.js', 'document.body.dataset.ready="yes"'],
    ];
    const written = results.filter(result => result.tool === 'file_write').length;
    if (written < files.length) return respond(response, 'file_write', { path: files[written][0], content: files[written][1] });
    if (!results.some(result => result.tool === 'image_generate')) return respond(response, 'image_generate', { prompt: 'fixture image', output_path: 'image.png' });
    if (stepsResult.execution.steps[0].status !== 'completed') return respond(response, 'update_execution_steps', {
      expectedRevision: revision, steps: [step('write', '生成报告', 'completed', writeRef), step('verify', '核验报告', 'running')],
    });
    if (searchIndex < 0) return respond(response, 'shell_exec', { command: 'cat report.md', access_mode: 'read_only' });
    if (stepsResult.execution.steps[1].status !== 'completed') return respond(response, 'update_execution_steps', {
      expectedRevision: revision, steps: [step('write', '生成报告', 'completed', writeRef), step('verify', '核验报告', 'completed', [toolMessages[searchIndex].tool_call_id])],
    });
    respond(response, null, null, `${marker}_DONE [report.md](report.md)`);
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
const port = await unusedPort(), cdpPort = await unusedPort();
const origin = `http://127.0.0.1:${port}`;
const rpc = (method, params) => appServerRequest(`ws://127.0.0.1:${port}/api/app-server`, method, params);
const post = (path, body) => fetch(`${origin}${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
async function json(path, body) {
  const response = body === undefined ? await fetch(`${origin}${path}`) : await post(path, body);
  assert.ok(response.ok, `${path}: ${response.status} ${await response.clone().text()}`); return response.json();
}
async function start() {
  daemon = spawn('./scripts/dev-daemon.sh', [], { cwd: root, detached: true, stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, MAGI_PORT: String(port), MAGI_WEB_DEV: '0', MAGI_OPEN_BROWSER: '0', MAGI_STATE_ROOT: join(fixture, 'state'),
      MAGI_TARGET_PRUNE_GIB: '1000', MAGI_BUILD_ID: 'personal-session-fixture' } });
  for (const stream of [daemon.stdout, daemon.stderr]) stream.on('data', chunk => { logs = (logs + chunk).slice(-12000); });
  await waitFor(async () => { if (daemon.exitCode !== null) throw new Error(logs); return (await fetch(`${origin}/health`)).ok; }, 'daemon 启动', 240000);
}
async function stop() {
  if (!daemon || daemon.exitCode !== null) return;
  const done = once(daemon, 'exit'); process.kill(-daemon.pid, 'SIGTERM'); await done;
}
async function submit(marker, workspaceId) {
  const accepted = await rpc('turn/start', { scope: workspaceId ? 'workspace' : 'personal', workspaceId,
    goalMode: marker === 'PERSONAL_MATRIX_GOAL',
    text: `${marker} 生成并核验报告`, requestId: marker, userMessageId: `user-${marker}`,
    orchestratorSessionConfig: { model: 'personal-fixture' }, accessProfile: marker === 'PERSONAL_GIT_CASE' || marker.startsWith('PERSONAL_MATRIX_') ? 'full_access' : 'restricted' });
  let snapshot;
  const deadline = Date.now() + 60000;
  while (Date.now() < deadline) {
    if (providerFailure) throw providerFailure;
    snapshot = await rpc('session/read', { sessionId: accepted.sessionId, includeTurns: true });
    if (marker === 'PERSONAL_MATRIX_FILES') {
      const interactions = await rpc('session/interactions', { sessionId: accepted.sessionId });
      const question = interactions.pendingQuestions[0];
      if (question) await rpc('session/question/resolve', { sessionId: accepted.sessionId,
        questionId: question.questionId, response: { kind: 'answered', answers: [{ selected: ['继续'] }] } });
    }
    const turn = snapshot.turns.find(turn => turn.turnId === accepted.turnId);
    if (turn && ['failed', 'cancelled'].includes(turn.status)) throw new Error(JSON.stringify(turn));
    if (turn?.status === 'completed') break;
    await delay(150);
  }
  assert.equal(snapshot?.turns.find(turn => turn.turnId === accepted.turnId)?.status, 'completed', `${marker} 完成超时`);
  const steps = snapshot.turns.flatMap(turn => turn.items).find(item => item.metadata?.executionSteps)?.metadata.executionSteps;
  if (marker !== 'PERSONAL_GIT_CASE' && !marker.startsWith('PERSONAL_MATRIX_')) assert.ok(steps?.steps.every(step => step.status === 'completed'));
  return accepted;
}
try {
  await start();
  await json('/api/settings/orchestrator/save', { baseUrl: `http://127.0.0.1:${provider.address().port}/v1`, apiKey: 'fixture', apiProtocol: 'openai_chat' });
  await json('/api/settings/image-generation/save', { baseUrl: `http://127.0.0.1:${provider.address().port}/v1`, apiKey: 'fixture', model: 'fixture-image', apiProtocol: 'openai_chat' });
  const projectRoot = join(fixture, 'project'); await mkdir(projectRoot);
  await writeFile(join(projectRoot, 'AGENTS.md'), 'PROJECT_RULE_MUST_NOT_LEAK');
  const { workspaceId } = await json('/api/workspaces/register', { path: projectRoot });
  const a = await submit('PERSONAL_A'), b = await submit('PERSONAL_B'), project = await submit('PROJECT_CASE', workspaceId);
  const gitCase = await submit('PERSONAL_GIT_CASE');
  const gitRoot = join(fixture, 'state/personal-sessions', gitCase.sessionId);
  assert.match(await readFile(join(gitRoot, 'repo-a/.git/HEAD'), 'utf8'), /refs\/heads\/feature/);
  assert.match(await readFile(join(gitRoot, 'repo-b/.git/HEAD'), 'utf8'), /refs\/heads\/main/);
  assert.equal((await rpc('session/read', { sessionId: gitCase.sessionId })).session.workspaceId, null);
  console.log('✓ 真实无项目模型 Turn：Shell 建仓、Git 状态/切分支、双仓库隔离与无项目归属');
  if (runToolMatrix) {
    for (const marker of personalMatrixMarkers) {
      const accepted = await submit(marker);
      assert.equal((await rpc('session/read', { sessionId: accepted.sessionId })).session.workspaceId, null);
      const directory = join(fixture, 'state/personal-sessions', accepted.sessionId);
      if (marker.endsWith('FILES')) {
        assert.equal(await readFile(join(directory, 'scratch/source.txt'), 'utf8'), 'gamma\nbeta\n');
        assert.equal(await readFile(join(directory, 'scratch/added.txt'), 'utf8'), 'matrix added\n');
        await assert.rejects(access(join(directory, 'scratch/moved.txt')));
      }
      if (marker.endsWith('GIT')) assert.equal((await readFile(join(directory, 'remote.git/refs/heads/main'), 'utf8')).trim().length, 40);
      if (marker.endsWith('GOAL')) assert.equal(await readFile(join(directory, 'goal-evidence.txt'), 'utf8'), 'goal verified');
      console.log(`✓ ${marker}: 真实 Turn 工具回灌与持久副作用`);
    }
    for (const required of ['file_read', 'file_write', 'file_patch', 'apply_patch', 'file_remove', 'file_mkdir', 'file_copy', 'file_move',
      'search_text', 'shell_exec', 'process_inspect', 'diff_preview', 'diagram_render', 'tool_catalog', 'web_fetch', 'image_generate', 'view_image',
      'ask_user_question', 'create_goal', 'get_goal', 'update_goal', 'update_plan', 'agent_spawn', 'agent_send', 'agent_wait', 'agent_cancel',
      'context_search', 'context_read', 'context_request', 'git_status', 'git_branch_list', 'git_branch_create', 'git_branch_switch', 'git_merge_preview',
      'git_merge', 'git_branch_delete', 'git_push', 'git_pull', 'git_worktree_create', 'git_worktree_list', 'git_worktree_remove']) assert.ok(matrixCalls.has(required), required);
    console.log(`✓ 个人工具矩阵成功调用 ${matrixCalls.size} 种工具：${[...matrixCalls].sort().join(', ')}`);
  }
  for (const [entry, marker] of [[a, 'PERSONAL_A'], [b, 'PERSONAL_B']]) {
    const result = await json(`/api/files/content?sessionId=${entry.sessionId}&filePath=report.md`);
    assert.equal(result.content, `${marker} report\n`); assert.equal(result.workspaceId, null);
    const files = await json(`/api/filesystem/list?sessionId=${entry.sessionId}`);
    assert.ok(files.entries.some(file => file.name === 'report.md'));
  }
  const escape = await fetch(`${origin}/api/files/content?sessionId=${a.sessionId}&filePath=${encodeURIComponent(join(fixture, 'state/personal-sessions', b.sessionId, 'report.md'))}`);
  assert.equal(escape.status, 400);
  console.log('✓ 真实文件/Shell/执行步骤闭环，个人目录与项目规则隔离，跨会话预览拒绝');
  const entry = join(fixture, 'browser.cjs');
  await writeFile(entry, `const {app,BrowserWindow}=require('electron');app.setPath('userData',${JSON.stringify(join(fixture, 'chromium'))});app.whenReady().then(()=>{const w=new BrowserWindow({show:false,webPreferences:{sandbox:false}});w.loadURL(${JSON.stringify(`${origin}/web.html?scope=personal&sessionId=${a.sessionId}`)});});`);
  browser = spawn(electron, [entry, `--remote-debugging-port=${cdpPort}`, '--disable-gpu'], { cwd: root, stdio: 'ignore' });
  await waitFor(async () => { try { page = await connectTarget(cdpPort, tab => tab.type === 'page' && tab.url.includes('/web.html'), '个人会话页面', errors); return true; } catch { return false; } }, 'Chromium');
  await waitFor(() => page.evaluate('document.body.innerText.includes("PERSONAL_A_DONE")'), '历史恢复');
  await waitFor(() => page.evaluate('Boolean(document.querySelector(".generated-image-block img")?.naturalWidth)'), '个人生成图片从原会话加载');
  assert.ok(await page.evaluate('document.querySelector(".ia-workspace-btn").textContent.includes("未关联项目")'));
  await waitFor(() => page.evaluate(`document.querySelectorAll('.recent-sessions-section [data-session-id]').length === ${runToolMatrix ? 4 + personalMatrixMarkers.length : 4}`), '最近包含个人与项目会话');
  await page.evaluate('document.querySelector("button[data-sidebar-mode=files]").click()');
  await waitFor(() => page.evaluate('document.querySelector("[role=tree]")?.innerText.includes("report")'), '会话文件树');
  await page.evaluate('[...document.querySelectorAll("[role=treeitem] button")].find(button => button.title.endsWith("report.md")).click()');
  await waitFor(() => page.evaluate('document.querySelector(".right-pane-markdown")?.innerText.includes("PERSONAL_A report")'), '个人文件真实预览');
  const download = await page.evaluate('document.querySelector("a[download]")?.href');
  assert.ok(download); assert.equal(await (await fetch(download)).text(), 'PERSONAL_A report\n');
  const site = await fetch(`${origin}/api/files/site-open?sessionId=${a.sessionId}&filePath=index.html`, { redirect: 'manual' });
  assert.equal(site.status, 307);
  const siteUrl = new URL(site.headers.get('location'), origin).href;
  await page.call('Page.navigate', { url: siteUrl });
  await waitFor(() => page.evaluate('document.body.dataset.ready === "yes" && document.querySelector("main")?.textContent === "PERSONAL_A_SITE"'), 'HTML 相对脚本执行');
  assert.equal(await page.evaluate('getComputedStyle(document.querySelector("main")).color'), 'rgb(12, 34, 56)');
  assert.ok(await page.evaluate('location.hostname === "site.localhost"'), 'HTML 在隔离来源执行');
  assert.equal(await page.evaluate('fetch("/api/settings/bootstrap").then(response => response.status)'), 403, '预览来源不能访问 daemon 业务 API');
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${a.sessionId}` });
  await waitFor(() => page.evaluate('document.body.innerText.includes("PERSONAL_A_DONE")'), '预览后回到会话');
  console.log('✓ 图片生成与原会话加载、HTML/CSS/JS 真实渲染及预览来源隔离');
  const calls = providerCalls;
  const epoch = (await json('/health')).runtimeEpoch;
  await stop(); await start();
  assert.notEqual((await json('/health')).runtimeEpoch, epoch);
  await waitFor(() => page.evaluate('!document.body.innerText.includes("连接中断")'), '页面重连');
  const previousDocument = await page.evaluate('performance.timeOrigin');
  await page.call('Page.reload');
  await waitFor(() => page.evaluate(`performance.timeOrigin !== ${previousDocument} && document.body.innerText.includes("PERSONAL_A_DONE") && Boolean(document.querySelector(".generated-image-block img")?.naturalWidth)`), '重启后刷新恢复历史与文件引用');
  await waitFor(() => page.evaluate('Boolean(document.querySelector("button[data-sidebar-mode=projects]"))'), '文件树返回按钮');
  await page.evaluate('document.querySelector("button[data-sidebar-mode=projects]").click()');
  await waitFor(() => page.evaluate(`Boolean(document.querySelector('.recent-sessions-section [data-session-id="${project.sessionId}"]'))`), '重启后最近目录恢复');
  await page.evaluate(`document.querySelector('.recent-sessions-section [data-session-id="${project.sessionId}"]').click()`);
  await waitFor(() => page.evaluate('document.body.innerText.includes("PROJECT_CASE_DONE") && !document.querySelector(".ia-workspace-btn").textContent.includes("未关联项目")'), '从最近进入项目会话保持项目归属');
  await page.evaluate('document.querySelector("[data-testid=sidebar-new-session]").click()');
  await waitFor(() => page.evaluate('document.querySelector(".ia-workspace-btn")?.textContent.includes("未关联项目") && !new URL(location.href).searchParams.get("sessionId")'), '项目会话中全局新建仍进入无项目草稿');
  await page.evaluate(`document.querySelector('.recent-sessions-section [data-session-id="${a.sessionId}"]').click()`);
  await waitFor(() => page.evaluate('document.body.innerText.includes("PERSONAL_A_DONE") && document.querySelector(".ia-workspace-btn").textContent.includes("未关联项目")'), '从最近返回个人会话无项目上下文泄漏');
  assert.equal((await json(`/api/files/content?sessionId=${a.sessionId}&filePath=report.md`)).content, 'PERSONAL_A report\n');
  assert.equal((await rpc('session/read', { sessionId: a.sessionId, includeTurns: true })).session.workspaceId, null);
  assert.equal(providerCalls, calls, '恢复不能重新执行 Provider 或文件操作');
  await json('/api/session/delete', { sessionId: a.sessionId });
  await assert.rejects(access(join(fixture, 'state/personal-sessions', a.sessionId)));
  assert.equal(await readFile(join(fixture, 'state/personal-sessions', b.sessionId, 'report.md'), 'utf8'), 'PERSONAL_B report\n');
  assert.ok((await rpc('session/read', { sessionId: project.sessionId })).session.workspaceId);
  assert.deepEqual(errors, []);
  console.log('✓ Chromium 最近导航、全局新建不继承项目、文件预览/下载、重启刷新不重放、删除隔离、项目历史保留');
} catch (error) {
  console.error(logs.slice(-5000)); if (providerFailure) console.error(providerFailure);
  if (page) { console.error(await page.evaluate('document.body.innerText').catch(() => '')); console.error(errors); }
  throw error;
} finally {
  page?.close(); if (browser && browser.exitCode === null) { const done = once(browser, 'exit'); browser.kill(); await done; }
  await stop(); provider.closeAllConnections(); await new Promise(resolve => provider.close(resolve));
  await rm(fixture, { recursive: true, force: true });
}
