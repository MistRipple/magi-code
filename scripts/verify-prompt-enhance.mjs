import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import electron from 'electron';
import { connectTarget, waitFor } from './lib/electron-harness.mjs';

// 真实 daemon + Chromium 点击；隔离状态根，本地 HTTP Provider，不使用用户模型配置。
// 先运行 npm --prefix web run build，再运行 node scripts/verify-prompt-enhance.mjs。
const root = new URL('..', import.meta.url).pathname;
const fixture = await mkdtemp(join(tmpdir(), 'magi-prompt-enhance-'));
const enhanced = '请整理项目结构，说明各模块职责，并列出需要确认的问题。';
let providerMode = 'success';
let providerCalls = 0;
let daemon, browser, page, logs = '';
const errors = [];
const provider = createServer(async (request, response) => {
  let raw = '';
  for await (const chunk of request) raw += chunk;
  providerCalls++;
  assert.equal(request.url, '/v1/chat/completions');
  const body = JSON.parse(raw);
  assert.equal(body.model, 'enhance-fixture');
  assert.ok(JSON.stringify(body.messages).includes('<user_prompt>'));
  response.writeHead(providerMode === 'error' ? 400 : 200, { 'content-type': 'application/json' });
  response.end(JSON.stringify(providerMode === 'error' ? { error: { message: 'fixture rejection' } } : {
    choices: [{ message: { role: 'assistant', content: providerMode === 'empty' ? '' : enhanced }, finish_reason: 'stop' }],
    usage: { prompt_tokens: 20, completion_tokens: 15, total_tokens: 35 },
  }));
});
async function unusedPort() {
  const server = createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
}
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
const port = await unusedPort();
const cdpPort = await unusedPort();
const origin = `http://127.0.0.1:${port}`;
async function post(path, body) {
  return fetch(`${origin}${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
}
async function json(path, body) {
  const response = await post(path, body);
  assert.ok(response.ok, `${path}: ${response.status} ${await response.clone().text()}`);
  return response.json();
}
async function navigate(query) {
  await page.call('Page.navigate', { url: `${origin}/web.html?${new URLSearchParams(query)}` });
  await waitFor(() => page.evaluate('!!document.querySelector("[data-testid=input-textarea]")'), '输入框就绪');
}
async function typeDraft(text) {
  await page.evaluate(`{ const editor = document.querySelector('[data-testid=input-textarea]');
    editor.focus(); editor.textContent = ${JSON.stringify(text)};
    editor.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: ${JSON.stringify(text)} })); }`);
}
async function clickEnhance() {
  await waitFor(() => page.evaluate('!!document.querySelector("button.ia-enhance:not(.ia-enhance-restore):not(:disabled)")'), '强化按钮可用');
  await page.evaluate('document.querySelector("button.ia-enhance:not(.ia-enhance-restore)").click()');
}
const editorText = () => page.evaluate('document.querySelector("[data-testid=input-textarea]").textContent');
try {
  daemon = spawn('./scripts/dev-daemon.sh', [], { cwd: root, detached: true, stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, MAGI_PORT: String(port), MAGI_WEB_DEV: '0', MAGI_OPEN_BROWSER: '0',
      MAGI_STATE_ROOT: join(fixture, 'state'), MAGI_TARGET_PRUNE_GIB: '1000', MAGI_BUILD_ID: 'prompt-enhance-fixture' } });
  for (const stream of [daemon.stdout, daemon.stderr]) stream.on('data', chunk => { logs = (logs + chunk).slice(-10000); });
  await waitFor(async () => {
    if (daemon.exitCode !== null) throw new Error(logs);
    return (await fetch(`${origin}/health`)).ok;
  }, '独立 daemon 启动', 240000, 250);
  const config = { baseUrl: `http://127.0.0.1:${provider.address().port}/v1`, apiKey: 'fixture', model: 'enhance-fixture', apiProtocol: 'openai_chat' };
  await json('/api/settings/auxiliary/save', config);
  const workspacePath = join(fixture, 'workspace');
  await mkdir(workspacePath);
  const { workspaceId } = await json('/api/workspaces/register', { path: workspacePath });
  const personal = await json('/api/session/materialize', { scope: 'personal' });
  const project = await json('/api/session/materialize', { scope: 'workspace', workspaceId });
  for (const binding of [{}, { sessionId: personal.sessionId }, { workspaceId }, { workspaceId, sessionId: project.sessionId }]) {
    const result = await json('/api/prompt/enhance', { ...binding, prompt: '整理项目', locale: 'zh-CN' });
    assert.equal(result.enhancedPrompt, enhanced);
  }
  console.log('✓ HTTP：个人/项目草稿与已创建会话均强化成功');
  const beforeInvalid = providerCalls;
  for (const prompt of ['', 'x'.repeat(12001)]) {
    const response = await post('/api/prompt/enhance', { prompt });
    assert.equal(response.status, 400);
  }
  assert.equal(providerCalls, beforeInvalid, '无效输入不能调用 Provider');
  const entry = join(fixture, 'browser.cjs');
  await writeFile(entry, `const {app,BrowserWindow}=require('electron');
    app.setPath('userData',${JSON.stringify(join(fixture, 'chromium'))});
    app.whenReady().then(()=>{const win=new BrowserWindow({show:false,webPreferences:{sandbox:false}});
      win.loadURL(${JSON.stringify(`${origin}/web.html?scope=personal`)});});`);
  browser = spawn(electron, [entry, `--remote-debugging-port=${cdpPort}`, '--disable-gpu'], { cwd: root, stdio: 'ignore' });
  await waitFor(async () => {
    try { page = await connectTarget(cdpPort, target => target.type === 'page' && target.url.includes('/web.html'), '提示词强化页面', errors); return true; }
    catch { return false; }
  }, 'Chromium');
  for (const query of [{ scope: 'personal' }, { scope: 'personal', sessionId: personal.sessionId },
    { scope: 'workspace', workspaceId, sessionId: project.sessionId }]) {
    await navigate(query);
    await typeDraft('整理项目');
    await clickEnhance();
    await waitFor(async () => await editorText() === enhanced, '点击强化后回填文本');
    await page.evaluate('document.querySelector(".ia-enhance-restore").click()');
    await waitFor(async () => await editorText() === '整理项目', '还原原文');
  }
  console.log('✓ Chromium：个人草稿/个人会话/项目会话点击强化、回填、还原原文');
  await navigate({ scope: 'personal' });
  for (const mode of ['error', 'empty']) {
    providerMode = mode;
    await typeDraft('失败时保留原文');
    await clickEnhance();
    await waitFor(() => page.evaluate('!!document.querySelector(".toast.error, .toast.warning")'), `${mode} 提示`);
    assert.equal(await editorText(), '失败时保留原文');
    await waitFor(() => page.evaluate('!document.querySelector("button.ia-enhance.loading")'), '强化按钮结束加载');
    await page.evaluate('document.querySelectorAll(".toast-close").forEach(button => button.click())');
  }
  providerMode = 'success';
  await clickEnhance();
  await waitFor(async () => await editorText() === enhanced, '失败后重试成功');
  await json('/api/settings/auxiliary/save', {});
  await navigate({ scope: 'personal' });
  await typeDraft('没有辅助模型');
  await waitFor(() => page.evaluate('document.querySelector("button.ia-enhance").disabled'), '未配置辅助模型时禁用按钮');
  const missing = await post('/api/prompt/enhance', { prompt: '整理想法' });
  assert.equal(missing.status, 400);
  assert.match((await missing.json()).message, /辅助模型未配置/);
  assert.deepEqual(errors, []);
  console.log('✓ 失败/空响应保留草稿、重试恢复、未配置模型禁用与明确错误；无浏览器运行异常');
} catch (error) {
  console.error(logs.slice(-4000));
  throw error;
} finally {
  page?.close();
  if (browser && browser.exitCode === null) { const done = once(browser, 'exit'); browser.kill(); await done; }
  if (daemon && daemon.exitCode === null) { const done = once(daemon, 'exit'); process.kill(-daemon.pid, 'SIGTERM'); await done; }
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  await rm(fixture, { recursive: true, force: true });
}
