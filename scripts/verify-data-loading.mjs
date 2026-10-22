import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import electron from 'electron';
import { connectTarget, waitFor } from './lib/electron-harness.mjs';

// 独立 daemon + 真实 Chromium；只用本地 Provider，不接触用户配置和正在运行的 Magi。
const root = new URL('..', import.meta.url).pathname;
const fixture = await mkdtemp(join(tmpdir(), 'magi-data-loading-'));
const richReply = [
  '# RENDER_UPGRADE_OK', '**bold-upgrade** and `inline-upgrade`',
  '| Column | Value |', '| --- | --- |', '| fixture | table-upgrade |',
  '', '$x^2 + y^2 = z^2$', '', '```javascript', 'const upgrade = true;', '```',
  '', '```mermaid', 'flowchart LR', '  A[Upgrade] --> B[Verified]', '```',
].join('\n');
const provider = createServer(async (request, response) => {
  let body = '';
  for await (const chunk of request) body += chunk;
  if (request.method === 'GET') {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify({ data: [{ id: 'loading-fixture' }] }));
    return;
  }
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  const content = body.includes('render-upgrade-fixture') ? richReply : 'LOADING_OK';
  response.end(`data: ${JSON.stringify({ choices: [{ delta: { content }, finish_reason: 'stop' }] })}\n\ndata: [DONE]\n\n`);
});
const unusedPort = async () => {
  const server = createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
};
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
const daemonPort = await unusedPort();
const cdpPort = await unusedPort();
const origin = `http://127.0.0.1:${daemonPort}`;
let daemon, browser, page, logs = '';
const errors = [];
const counts = new Map();
const workspaces = [];
const held = new Map();
const json = async (path, body) => {
  const response = await fetch(`${origin}${path}`, body ? {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body),
  } : undefined);
  assert.ok(response.ok, `${path}: ${response.status} ${response.ok ? '' : await response.text()}`);
  return response.json();
};
async function start() {
  daemon = spawn('./scripts/dev-daemon.sh', [], {
    cwd: root, detached: true, stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, MAGI_PORT: String(daemonPort), MAGI_WEB_DEV: '0', MAGI_OPEN_BROWSER: '0',
      MAGI_STATE_ROOT: join(fixture, 'state'), MAGI_TARGET_PRUNE_GIB: '1000',
      MAGI_BUILD_ID: 'data-loading-fixture',
      MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${provider.address().port}/v1`,
      MAGI_OPENAI_COMPAT_API_KEY: 'local-fixture', MAGI_OPENAI_COMPAT_MODEL: 'loading-fixture' },
  });
  for (const stream of [daemon.stdout, daemon.stderr]) stream.on('data', chunk => { logs = (logs + chunk).slice(-15000); });
  await waitFor(async () => {
    if (daemon.exitCode !== null) throw new Error(logs);
    return (await fetch(`${origin}/health`)).ok;
  }, '独立 daemon 启动', 240000, 250);
}
async function stop() {
  if (!daemon || daemon.exitCode !== null) return;
  const done = once(daemon, 'exit');
  process.kill(-daemon.pid, 'SIGTERM');
  await done;
}
async function register(name) {
  const path = join(fixture, name);
  await mkdir(path);
  await writeFile(join(path, 'visible.txt'), name);
  await writeFile(join(path, '.hidden.txt'), 'hidden');
  const result = await json('/api/workspaces/register', { path });
  const workspace = { id: result.workspaceId, path, name };
  workspaces.push(workspace);
  return workspace;
}
async function submit(workspace, text) {
  const accepted = await json('/api/session/turn', { scope: 'workspace', workspaceId: workspace.id,
    text, images: [], requestId: `${text}-${Date.now()}`, userMessageId: `${text}-user` });
  await waitFor(async () => {
    const directory = await json(`/api/workspaces/sessions?workspaceId=${workspace.id}`);
    return directory.sessions.find(row => row.sessionId === accepted.sessionId && !row.isRunning && row.messageCount > 0);
  }, '本地 Turn 结算');
  return accepted;
}
const selector = workspace => `.workspace-header-btn[data-workspace-id="${workspace.id}"]`;
const click = selector => page.evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
const count = id => counts.get(id) ?? 0;
try {
  await start();
  for (const name of ['fast', 'failed', 'slow']) {
    const workspace = await register(name);
    workspace.session = (await submit(workspace, `history-${name}`)).sessionId;
  }
  const entry = join(fixture, 'browser.cjs');
  await writeFile(entry, `const {app,BrowserWindow}=require('electron');
app.setPath('userData',${JSON.stringify(join(fixture, 'chromium'))});
app.whenReady().then(()=>{const win=new BrowserWindow({show:false,webPreferences:{sandbox:false}});win.loadURL(${JSON.stringify(`${origin}/web.html?scope=personal`)});});`);
  browser = spawn(electron, [entry, `--remote-debugging-port=${cdpPort}`, '--disable-gpu'], { cwd: root, stdio: 'ignore' });
  await waitFor(async () => {
    try { page = await connectTarget(cdpPort, target => target.type === 'page' && target.url.includes('/web.html'), 'fixture', errors); return true; } catch { return false; }
  }, 'Chromium');
  let holdFiles = false;
  let holdSettings = false;
  let staleSettings;
  let settingsResponses = 0;
  await page.call('Network.enable');
  page.on('Network.responseReceived', ({ response }) => {
    if (new URL(response.url).pathname === '/api/settings/bootstrap') settingsResponses++;
  });
  let fileReads = 0;
  page.on('Fetch.requestPaused', async ({ requestId, request }) => {
    try {
      const url = new URL(request.url);
      const id = url.searchParams.get('workspaceId');
      if (url.pathname === '/api/settings/bootstrap' && holdSettings) {
        holdSettings = false;
        staleSettings = await (await fetch(request.url)).text();
        held.set('settings', requestId);
        return;
      }
      if (url.pathname === '/api/workspaces/sessions') {
        counts.set(id, count(id) + 1);
        if (id === workspaces[2].id && count(id) === 1) { held.set('slow', requestId); return; }
        if (id === workspaces[1].id && count(id) === 1) {
          await page.call('Fetch.fulfillRequest', { requestId, responseCode: 503,
            responseHeaders: [{ name: 'content-type', value: 'application/json' }], body: Buffer.from('{}').toString('base64') });
          return;
        }
      }
      if (url.pathname === '/api/filesystem/list') {
        fileReads++;
        if (holdFiles) { held.set('files', requestId); holdFiles = false; return; }
      }
      await page.call('Fetch.continueRequest', { requestId });
    } catch (error) { errors.push(String(error)); }
  });
  await page.call('Fetch.enable', { patterns: [{ urlPattern: `${origin}/api/*`, requestStage: 'Request' }] });
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal` });
  await waitFor(() => workspaces.every(workspace => count(workspace.id) > 0), '折叠工作区也读取目录');
  await waitFor(() => page.evaluate(`!!document.querySelector('[data-testid="input-textarea"]')`), '输入区');
  // 数据已加载时展开只显示缓存，不应再次请求；slow 仍被阻塞。
  const fast = workspaces[0];
  const beforeExpand = count(fast.id);
  await click(selector(fast));
  await waitFor(() => page.evaluate(`!!document.querySelector('[data-session-id="${fast.session}"]')`), '慢目录不阻塞快目录');
  assert.equal(count(fast.id), beforeExpand, '展开前已就绪，不能靠点击补读');
  assert.ok(held.has('slow'));
  assert.equal(await page.evaluate(`new URL(location.href).searchParams.get('scope')`), 'personal', '后台加载不能改变导航');
  console.log('✓ 启动预取折叠目录、慢目录隔离、导航保持');
  await click(selector(workspaces[1]));
  await waitFor(() => page.evaluate(`!!document.querySelector('[data-session-id="${workspaces[1].session}"]')`), '失败后展开重试');
  await page.call('Fetch.continueRequest', { requestId: held.get('slow') }); held.delete('slow');
  const beforeBackground = count(fast.id);
  const background = await submit(fast, 'background-new-session');
  await waitFor(() => count(fast.id) > beforeBackground, '后台事件主动刷新目录');
  await waitFor(() => page.evaluate(`!!document.querySelector('[data-session-id="${background.sessionId}"]')`), '后台新会话进入侧栏');
  console.log('✓ 临时失败可重试、未订阅会话自动更新');
  const added = await register('external-registration');
  await waitFor(() => page.evaluate(`!!document.querySelector(${JSON.stringify(selector(added))})`), '另一客户端注册工作区');
  const epoch = (await json('/health')).runtimeEpoch;
  await stop(); await start();
  assert.notEqual((await json('/health')).runtimeEpoch, epoch);
  const beforeReconnect = count(fast.id);
  await waitFor(() => count(fast.id) > beforeReconnect, 'daemon 重启后目录重读', 30000);
  await waitFor(() => page.evaluate(`!!document.querySelector('[data-session-id="${background.sessionId}"]')`), '新代际目录恢复');
  console.log('✓ 跨客户端工作区变化、daemon 重启恢复');
  await click(`.session-item[data-session-id="${fast.session}"]`);
  await waitFor(() => page.evaluate(`new URL(location.href).searchParams.get('sessionId') === '${fast.session}'`), '切入工作区');
  holdFiles = true;
  await click('button[data-sidebar-mode="files"]');
  await waitFor(() => held.has('files'), '文件树首次请求在途');
  await click('button[aria-label="显示/隐藏隐藏文件"]');
  await page.call('Fetch.continueRequest', { requestId: held.get('files') }); held.delete('files');
  await waitFor(() => page.evaluate(`document.querySelector('.project-file-tree')?.textContent.includes('.hidden.txt')`), '隐藏文件切换补读');
  assert.ok(fileReads >= 2);
  console.log('✓ 文件树读取期间切换条件会补读');
  await click('button[data-sidebar-mode="projects"]');
  const richTurn = await submit(fast, 'render-upgrade-fixture');
  await waitFor(() => page.evaluate(`!!document.querySelector('.session-item[data-session-id="${richTurn.sessionId}"]')`), '富文本会话目录更新');
  await click(`.session-item[data-session-id="${richTurn.sessionId}"]`);
  await waitFor(() => page.evaluate(`Array.from(document.querySelectorAll('.markdown-content h1')).some(node => node.textContent === 'RENDER_UPGRADE_OK')`), 'Markdown 2 标题');
  assert.ok(await page.evaluate(`Array.from(document.querySelectorAll('.markdown-content strong')).some(node => node.textContent === 'bold-upgrade')`));
  assert.ok(await page.evaluate(`Array.from(document.querySelectorAll('.markdown-content td')).some(node => node.textContent === 'table-upgrade')`));
  await waitFor(() => page.evaluate(`!!document.querySelector('.markdown-content .katex')`), 'KaTeX 公式');
  await waitFor(() => page.evaluate(`!!document.querySelector('.markdown-content .language-javascript .hljs-keyword')`), '代码高亮');
  await waitFor(() => page.evaluate(`!!document.querySelector('.markdown-content .svg-wrapper svg')`), 'Mermaid 12 流程图');
  console.log('✓ Markdown 标题/表格/公式/代码高亮与 Mermaid 真实渲染');
  // 已挂载的设置页重新显示、focus 和重连都要重读配置，同时保留本地草稿。
  const modelUrl = '#model-config-orch-base-url';
  const savedUrl = suffix => `http://127.0.0.1:${provider.address().port}/${suffix}`;
  const saveConnection = suffix => json('/api/settings/orchestrator/save', {
    baseUrl: savedUrl(suffix), apiKey: 'local-fixture', apiProtocol: 'openai_chat', urlMode: 'standard',
  });
  const waitConnection = suffix => waitFor(() => page.evaluate(`document.querySelector('${modelUrl}')?.value === ${JSON.stringify(savedUrl(suffix))}`), `设置刷新 ${suffix}`);
  await click('[data-testid="sidebar-settings"]');
  await waitFor(() => page.evaluate(`!!document.querySelector('[data-testid="settings-nav-model"]')`), '设置页挂载');
  await click('[data-testid="settings-nav-model"]');
  await waitFor(() => page.evaluate(`!!document.querySelector('${modelUrl}')`), '模型连接表单');
  await click('[data-testid="settings-back"]');
  await saveConnection('reopen');
  await click('[data-testid="sidebar-settings"]');
  await waitConnection('reopen');
  holdSettings = true;
  await page.evaluate(`window.dispatchEvent(new Event('focus'))`);
  await waitFor(() => held.has('settings'), '旧设置读取在途');
  await saveConnection('newer');
  await page.evaluate(`window.dispatchEvent(new Event('focus'))`);
  await waitConnection('newer');
  await page.call('Fetch.fulfillRequest', { requestId: held.get('settings'), responseCode: 200,
    responseHeaders: [{ name: 'content-type', value: 'application/json' }], body: Buffer.from(staleSettings).toString('base64') });
  held.delete('settings');
  // 等待一次新读取确认旧响应已处理，不能把页面恢复到 reopen。
  await page.evaluate(`new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))`);
  await waitConnection('newer');
  await page.evaluate(`{ const input = document.querySelector('${modelUrl}'); input.value = ${JSON.stringify(savedUrl('draft'))}; input.dispatchEvent(new Event('input', { bubbles: true })); }`);
  await saveConnection('external');
  const beforeDraftRefresh = settingsResponses;
  await page.evaluate(`window.dispatchEvent(new Event('focus'))`);
  await waitFor(() => settingsResponses > beforeDraftRefresh, '编辑期间设置刷新返回');
  await page.evaluate(`new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))`);
  assert.equal(await page.evaluate(`document.querySelector('${modelUrl}').value`), savedUrl('draft'));
  console.log('✓ 设置页重新显示/focus 刷新、旧响应隔离、未保存草稿保留');
  assert.deepEqual(errors.filter(error => !error.includes('503')), []);
} catch (error) {
  console.error(logs.slice(-4000));
  if (page) console.error(await page.evaluate('document.body.innerText').catch(() => ''));
  throw error;
} finally {
  page?.close();
  if (browser && browser.exitCode === null) { const done = once(browser, 'exit'); browser.kill(); await done; }
  await stop();
  provider.closeAllConnections(); await new Promise(resolve => provider.close(resolve));
  await rm(fixture, { recursive: true, force: true });
}
