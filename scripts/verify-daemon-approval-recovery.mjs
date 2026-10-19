import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, rm, access, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import electron from 'electron';
import { connectAppPage, waitFor } from './lib/electron-harness.mjs';

// 独立状态/目录/本地 Provider；仅终止本脚本创建的进程组。
const root = new URL('..', import.meta.url).pathname.replace(/\/$/u, '');
const fixture = await mkdtemp(join(tmpdir(), 'magi-approval-recovery-'));
await mkdir(join(fixture, 'workspace'));
const workspacePath = await realpath(join(fixture, 'workspace'));
const target = join(workspacePath, 'remove-once.txt');
await writeFile(target, 'approval must precede removal');
const exists = async () => { try { await access(target); return true; } catch { return false; } };
let providerRequests = 0;
let deliveredResults = 0;
const provider = createServer(async (request, response) => {
  let raw = '';
  for await (const chunk of request) raw += chunk;
  if (request.method !== 'POST') { response.end(JSON.stringify({ data: [{ id: 'approval-fixture' }] })); return; }
  try {
    const body = JSON.parse(raw);
    providerRequests += 1;
    const results = (body.messages ?? []).filter((item) => item.role === 'tool').map((item) => {
      try { return JSON.parse(item.content); } catch { return {}; }
    });
    const succeeded = results.some((item) => item.tool === 'file_remove' && item.status === 'succeeded');
    if (succeeded) deliveredResults += 1;
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    const delta = succeeded ? { role: 'assistant', content: 'APPROVAL_RECOVERY_DONE' } : {
      role: 'assistant', tool_calls: [{ index: 0, id: 'fixture-reused-call', type: 'function',
        function: { name: 'magi_builtin_file_remove', arguments: JSON.stringify({ path: target }) } }],
    };
    for (const part of [
      { choices: [{ index: 0, delta, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: succeeded ? 'stop' : 'tool_calls' }] },
    ]) response.write(`data: ${JSON.stringify({ id: 'approval-fixture', object: 'chat.completion.chunk', model: 'approval-fixture', ...part })}\n\n`);
    response.end('data: [DONE]\n\n');
  } catch (error) { response.writeHead(500); response.end(String(error)); }
});
await new Promise((resolve) => provider.listen(0, '127.0.0.1', resolve));
async function unusedPort() {
  const server = createServer(); await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port; await new Promise((resolve) => server.close(resolve)); return port;
}
const port = await unusedPort();
const cdpPort = await unusedPort();
const origin = `http://127.0.0.1:${port}`;
let daemon; let browser; let page; let logs = '';
const appendLog = (chunk) => { logs = (logs + chunk).slice(-50000); };
async function startDaemon() {
  daemon = spawn('./scripts/dev-daemon.sh', [], { cwd: root, detached: true, stdio: ['ignore', 'pipe', 'pipe'], env: {
    ...process.env, MAGI_PORT: String(port), MAGI_WEB_DEV: '0', MAGI_STATE_ROOT: join(fixture, 'state'),
    MAGI_OPEN_BROWSER: '0', MAGI_TARGET_PRUNE_GIB: '1000', MAGI_BUILD_ID: 'approval-recovery-fixture',
    MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${provider.address().port}/v1`,
    MAGI_OPENAI_COMPAT_API_KEY: 'local-fixture', MAGI_OPENAI_COMPAT_MODEL: 'approval-fixture',
  } });
  daemon.stdout.on('data', appendLog); daemon.stderr.on('data', appendLog);
  await waitFor(async () => {
    if (daemon.exitCode !== null) throw new Error(`daemon exited: ${logs.slice(-4000)}`);
    return (await fetch(`${origin}/health`)).ok;
  }, 'isolated daemon health', 240000, 250);
}
async function post(path, body) {
  const response = await fetch(`${origin}/api/${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  const text = await response.text(); assert.equal(response.status, 200, `${path}: ${text}`); return JSON.parse(text);
}
async function stopDaemon(signal) {
  if (!daemon || daemon.exitCode !== null || daemon.signalCode !== null) return;
  const ended = once(daemon, 'exit'); process.kill(-daemon.pid, signal); await ended;
  await waitFor(async () => { try { await fetch(`${origin}/health`); return false; } catch { return true; } }, 'daemon port released');
}
try {
  await startDaemon();
  const { workspaceId } = await post('workspaces/register', { path: workspacePath });
  const accepted = await post('session/turn', { scope: 'workspace', workspaceId, workspacePath, text: 'APPROVAL_RECOVERY 删除指定文件后汇报结果。',
    accessProfile: 'restricted', requestId: 'approval-fixture-input', userMessageId: 'approval-fixture-user' });
  const sessionId = accepted.sessionId;
  assert.ok(sessionId);
  const query = new URLSearchParams({ sessionId, workspaceId, workspacePath });
  const readApprovals = async () => {
    const response = await fetch(`${origin}/api/session/tool-approvals?${query}`);
    assert.equal(response.status, 200); return (await response.json()).pendingApprovals;
  };
  const pending = await waitFor(async () => (await readApprovals())[0], 'durable original approval');
  assert.equal(await exists(), true);
  assert.equal(providerRequests, 1);
  await stopDaemon('SIGKILL');
  await startDaemon();
  const restored = await waitFor(async () => (await readApprovals())[0], 'approval after process restart');
  assert.equal(restored.approvalId, pending.approvalId);
  assert.equal(providerRequests, 1, 'restarting must not replay provider/tool calls');
  assert.equal(await exists(), true);
  const entry = join(fixture, 'browser.cjs');
  const url = `${origin}/web.html?scope=workspace&${query}`;
  await writeFile(entry, `const { app, BrowserWindow } = require('electron');
app.setPath('userData', ${JSON.stringify(join(fixture, 'chromium'))});
app.whenReady().then(() => { const win = new BrowserWindow({ show: false, webPreferences: { contextIsolation: true, nodeIntegration: false, sandbox: false } }); win.loadURL(${JSON.stringify(url)}); });`);
  browser = spawn(electron, [entry, `--remote-debugging-port=${cdpPort}`, '--disable-gpu'], { cwd: root, stdio: ['ignore', 'pipe', 'pipe'] });
  browser.stderr.on('data', appendLog);
  const errors = [];
  await waitFor(async () => { try { page = await connectAppPage(cdpPort, port, errors); return true; } catch { return false; } }, 'Chromium renderer');
  await waitFor(() => page.evaluate(`(() => { const button = document.querySelector('.tool-approval .approval-button--primary'); if (!button || button.disabled) return false; button.click(); return true; })()`), 'restored approval button');
  await waitFor(async () => (await readApprovals()).length === 0, 'approval acknowledged');
  assert.equal(await exists(), true, 'approval alone must not start an interrupted task');
  const continued = await post('session/continue', { sessionId, workspaceId, workspacePath,
    requestId: 'approval-fixture-continue', userMessageId: 'approval-fixture-continue-user' });
  assert.equal(continued.runnerStarted, true);
  await waitFor(async () => !await exists() && deliveredResults > 0, 'resumed original file removal', 30000);
  await waitFor(() => page.evaluate(`document.body.innerText.includes('APPROVAL_RECOVERY_DONE')`), 'completed result in UI', 30000);
  assert.equal((await readApprovals()).length, 0, 'recovery cannot ask a second approval');
  assert.equal(providerRequests, 3, 'one original request, one resumed call and one final result');
  assert.deepEqual(errors, []);
  const layout = JSON.parse(await readFile(join(fixture, 'state', 'state-layout.json'), 'utf8'));
  assert.equal(layout.version, 18);
  process.stdout.write(JSON.stringify({ passed: true, providerRequests, checks: [
    'original operation blocked before approval', 'stable approval restored after SIGKILL',
    'Chromium approval button works after restart', 'approval acknowledgement has no side effect',
    'Continue executes original file removal once', 'completion shown without second approval', 'layout write barrier',
  ] }, null, 2) + '\n');
} catch (error) {
  if (page) process.stderr.write(`${await page.evaluate('document.body.innerText').catch(String)}\n`);
  process.stderr.write(`${logs.slice(-14000)}\n`); throw error;
}
finally {
  page?.close();
  if (browser?.exitCode === null && browser.signalCode === null) {
    const ended = once(browser, 'exit');
    browser.kill('SIGTERM');
    await ended;
  }
  await stopDaemon('SIGTERM').catch(() => {});
  provider.closeAllConnections(); await new Promise((resolve) => provider.close(resolve));
  await rm(fixture, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
}
