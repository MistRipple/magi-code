// 受控验收：真实 daemon / Electron / Worker，模型与第三方网页使用离线夹具。
// 仅证明 AI 工具循环、真实输入、检查点、daemon 侧证据闸门和安全交接；不代表真实账号规划质量。
// 夹具模型只根据最新快照中的页面内容决定动作，不读取 URL 路径；真实账号的规划质量由真实模型验收。
import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { createServer as createHttpsServer } from 'node:https';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:http';
import { connectAppPage, connectTarget, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = new URL('..', import.meta.url).pathname;
const work = await mkdtemp(join(tmpdir(), 'magi-ai-setup-'));
const stateRoot = join(work, 'state');
const { check, summary } = createReporter();
const errors = [], logs = [], calls = [];
let gateProbed = false;
const id = 'tunnel_0123456789abcdef0123456789abcdef';
let daemon, desktop, page, sessionIdForDiagnostics;
const unusedPort = async () => {
  const s = createServer(); await new Promise(r => s.listen(0, '127.0.0.1', r));
  const port = s.address().port; await new Promise(r => s.close(r)); return port;
};
const port = await unusedPort(), cdpPort = await unusedPort();
const fakeClient = join(work, 'tunnel-client');
const client = `#!/bin/sh
case "$2" in
connect) touch "$0.alive"; printf '{"running":true,"healthy":true,"ready":true,"pid":99}';;
stop) rm -f "$0.alive";;
cleanup) if [ -f "$0.alive" ]; then printf '{"entries":[{"alias":"magi-web-model","runtime_state":"ready","live_runtime":{"system":{"pid":99}}}]}'; else printf '{"entries":[]}'; fi;;
*) exit 17;;
esac
`;
await mkdir(stateRoot, { recursive: true });
await writeFile(fakeClient, client, { mode: 0o700 });
await writeFile(fakeClient + '.sha256', createHash('sha256').update(client).digest('hex'));
const home = '<main><h1>ChatGPT</h1><button aria-label="Open profile menu">Fixture account</button><button onclick="location.href=\'/settings/plugins-settings\'">Integrations</button><div role="textbox" contenteditable id="prompt-textarea"></div></main>';
const tunnels = `<main><h1>Bridges</h1><p>Acme Org</p><div id="rows"></div><button onclick="document.querySelector('dialog').open=true">Provision bridge</button><dialog><label>Bridge name<input id="name"></label><label>Description<textarea id="desc"></textarea></label><button onclick="localStorage.setItem('created','1');show();this.closest('dialog').open=false">Commit bridge</button></dialog></main><script>function show(){rows.textContent=localStorage.created?'Magi ${id}':''}show()</script>`;
// 密钥页：背景是很长的密钥列表（大量复选框和按钮），表单在最上层的模态对话框里。
// 快照预算若不优先对话框，表单控件会被背景列表挤出，Agent 就看不到它们。
const keyRows = Array.from({ length: 150 }, (_, index) => `<tr><td><input type="checkbox" aria-label="Select key ${index}"></td><td><button>View key ${index}</button></td></tr>`).join('');
const keys = `<main><h1>Runtime credentials</h1><p>Acme Org</p><p>Default project</p><table>${keyRows}</table></main><div role="dialog" aria-modal="true" aria-label="Create key"><label>Runtime label<input></label><label>Lifetime<select><option value="30">30 days</option><option value="1">1 day</option></select></label><label><input type="radio" name="p" checked>All</label><label><input type="radio" name="p">Restricted</label><label><input type="checkbox" onchange="count()">Tunnels Read</label><label><input type="checkbox" onchange="count()">Tunnels Use</label><p id="count">0 selected permissions</p><button onclick="localStorage.setItem('generated','1')">Create secret key</button></div><script>function count(){document.querySelector('#count').textContent=document.querySelectorAll('input[type=checkbox]:checked').length+' selected permissions'}</script>`;
const connectors = `<main><h1>Magi</h1><p>${id}</p><button onclick="this.disabled=true;fetch('https://chatgpt.com/__fixture/refresh-tools').then(r=>r.json()).then(v=>{document.querySelector('#result').textContent='Tools refreshed: '+v.count+' tools available';this.disabled=false})">Reload tool catalog</button><p id="result"></p></main>`;
// 夹具扮演 ChatGPT：刷新时经 Tunnel 槽位端点（本机 socket）真实读取一次 Magi 工具目录。
const slotToolCount = () => new Promise((resolve, reject) => {
  const digest = createHash('sha256').update('magi-mcp-endpoint-v1:' + join(stateRoot, 'web-slot')).digest('hex').slice(0, 24);
  const socket = net.connect(join(tmpdir(), `magi-mcp-${digest}.sock`));
  let buffer = '';
  socket.once('error', reject);
  socket.on('data', chunk => {
    buffer += chunk;
    if (!buffer.includes('\n')) return;
    socket.end();
    try { resolve(JSON.parse(buffer.split('\n')[0]).result.tools.length); } catch (error) { reject(error); }
  });
  socket.write('{"jsonrpc":"2.0","id":1,"method":"tools/list"}\n');
});
// 接管场景：第 3 次模型调用在返回前停住（Agent 正在「思考」，两条命令之间），等脚本以用户身份接管后再放行。
let releaseModelHold; const modelHold = new Promise(resolve => { releaseModelHold = resolve; });
const provider = createServer(async (req, res) => {
  if (req.method === 'GET' && req.url === '/v1/models') {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ object: 'list', data: [{ id: 'setup-fixture', object: 'model' }] }));
    return;
  }
  try {
    let raw = ''; for await (const chunk of req) raw += chunk;
    if (!raw.trim()) { logs.push(`FIXTURE_PROVIDER empty-body ${req.method} ${req.url}\n`); res.writeHead(200, { 'content-type': 'application/json' }); res.end('{}'); return; } // 连接探测等无请求体的非模型调用
    const body = JSON.parse(raw); calls.push(body);
    if (calls.length === 3) await modelHold;
    const latest = body.messages.at(-1).content;
    const data = latest.split('最新页面观察：')[1]?.split('\n上一动作结果：')[0];
    assert.ok(data, 'Agent 必须把最新观察交给模型');
    const observation = JSON.parse(data);
    const elements = observation.elements;
    const find = name => elements.find(e => e.name === name) || elements.find(e => e.name?.startsWith(name)); // select 的无障碍名称会拼接选项文本
    const field = (name, value) => ({ element_ref: find(name)?.element_ref, value });
    const click = name => ['browser_click', { element_ref: find(name)?.element_ref }];
    const navigate = url => ['browser_navigate', { url }];
    const saved = body.messages[1].content.includes('已有运行时密钥：true');
    const known = body.messages[1].content.includes(`当前 Tunnel ID：${id}`);
    const text = JSON.stringify(observation);
    const onTunnels = Boolean(find('Provision bridge') || find('Bridge name') || text.includes('Bridges'));
    const onKeys = Boolean(find('Runtime label') || text.includes('Runtime credentials'));
    const onConnector = Boolean(find('Reload tool catalog'));
    let action;
    if (onTunnels) {
      if (text.includes(id) && known) action = navigate(saved ? 'https://chatgpt.com/settings/plugins-settings' : 'https://platform.openai.com/settings/organization/api-keys');
      else if (text.includes(id)) action = ['setup_checkpoint', { outcome: 'tunnel_ready', tunnel_id: id, organization: 'Acme Org', detail: '已从当前页面核实 Magi Tunnel', evidence: [id] }];
      else if (find('Bridge name')) {
        if (find('Bridge name').value !== 'Magi') action = ['browser_fill_form', { fields: [field('Bridge name', 'Magi'), field('Description', 'Connect local Magi tools')] }];
        else action = click('Commit bridge');
      } else action = click('Provision bridge');
    } else if (onKeys) {
      if (saved) action = navigate('https://chatgpt.com/settings/plugins-settings');
      else if (find('Runtime label')?.value !== 'Magi Tunnel Runtime') action = ['setup_actions', { steps: [
        { tool: 'browser_type', element_ref: find('Runtime label')?.element_ref, text: 'Magi Tunnel Runtime', replace: true },
        { tool: 'browser_fill_form', fields: [field('Lifetime', '30')] },
        { tool: 'browser_click', element_ref: find('Restricted')?.element_ref },
        { tool: 'browser_click', element_ref: find('Tunnels Read')?.element_ref },
        { tool: 'browser_click', element_ref: find('Tunnels Use')?.element_ref },
      ] }];
      else action = ['setup_checkpoint', { outcome: "key_prepared", organization: "Acme Org", project: "Default project", detail: "表单已准备。请生成密钥并直接安全保存到 Magi。", evidence: ["Restricted", "Tunnels Read", "Tunnels Use"] }];
    } else if (onConnector) {
      if (text.includes('Tools refreshed:')) action = ['setup_checkpoint', { outcome: 'connector_ready', detail: '已核对 Magi 与 Tunnel，工具刷新完成。', evidence: ['Tools refreshed:'] }];
      else if (find('Reload tool catalog').disabled) action = ['setup_wait', { milliseconds: 1500 }];
      else if (!gateProbed) { gateProbed = true; action = ['setup_checkpoint', { outcome: 'connector_ready', detail: '尚未刷新即宣布完成（应被 daemon 拒绝）', evidence: ['Magi'] }]; }
      else action = click('Reload tool catalog');
    } else if (find('Open profile menu') || text.includes('ChatGPT')) {
      action = navigate(saved ? 'https://chatgpt.com/settings/plugins-settings' : known ? 'https://platform.openai.com/settings/organization/api-keys' : 'https://platform.openai.com/settings/organization/tunnels');
    } else action = ['setup_wait', { milliseconds: 800 }]; // 页面尚未渲染出可识别内容：等待并重新观察
    // 目标控件尚未出现在无障碍快照里（页面仍在渲染）：和真实模型一样先等待再观察，不发明节点。
    const missingTarget = (action[0] === 'browser_click' && action[1].element_ref === undefined)
      || (action[0] === 'setup_actions' && action[1].steps.some(item => item.element_ref === undefined && !item.fields))
      || (action[0] === 'browser_fill_form' && action[1].fields.some(item => item.element_ref === undefined));
    if (missingTarget) action = ['setup_wait', { milliseconds: 500 }];
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { role: 'assistant', tool_calls: [{ index: 0, id: 'fixture-' + calls.length, type: 'function', function: { name: action[0], arguments: JSON.stringify(action[1]) } }] }, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }], usage: { prompt_tokens: 100, completion_tokens: 10 } })}\n\ndata: [DONE]\n\n`);
  } catch (error) { errors.push(String(error)); res.writeHead(500); res.end('fixture failure'); }
});
await new Promise(r => provider.listen(0, '127.0.0.1', r));
// 夹具站点：Electron 经 host-resolver-rules 把两个真实域名解析到本机 HTTPS 服务，页面 URL 和 origin 保持真实；
// 不使用 CDP 拦截，因此跨站导航、Surface 替换和重载都不会让请求落到真实网络。
execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', join(work, 'key.pem'), '-out', join(work, 'cert.pem'), '-days', '1', '-subj', '/CN=chatgpt.com'], { stdio: 'ignore' });
const site = createHttpsServer({ key: readFileSync(join(work, 'key.pem')), cert: readFileSync(join(work, 'cert.pem')) }, async (req, res) => {
  try {
    const u = new URL(req.url, 'https://fixture.invalid');
    if (u.pathname === '/__fixture/refresh-tools') {
      const count = await slotToolCount().catch(error => { errors.push('slot tools/list: ' + error); return 0; });
      res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify({ count })); return;
    }
    if (req.headers['sec-fetch-dest'] !== 'document') { res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify({ user: { name: 'fixture' } })); return; }
    const host = String(req.headers.host).split(':')[0];
    const html = host === 'platform.openai.com' ? u.pathname.includes('/tunnels') ? tunnels : keys : u.pathname.includes('/settings/plugins-settings') ? connectors : home;
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
    res.end('<!doctype html><meta charset="utf-8"><title>AI setup fixture</title>' + html);
  } catch (error) { errors.push('site: ' + error); res.writeHead(500); res.end('fixture failure'); }
});
await new Promise(r => site.listen(0, '127.0.0.1', r));
const sitePort = site.address().port;
// 页面刷新后的 webview 目标会随跨站导航变化：每次按当前目标连接并求值。
const guestEval = async expression => {
  const target = await connectTarget(cdpPort, candidate => candidate.type === 'webview' && candidate.url.startsWith('https://'), '夹具 guest');
  try { return await target.evaluate(expression); } finally { target.close(); }
};
const env = { ...process.env, MAGI_STATE_ROOT: stateRoot, MAGI_PORT: String(port), MAGI_WEB_DEV: '0', MAGI_TARGET_PRUNE_GIB: '100', MAGI_OPEN_BROWSER: '0', MAGI_TUNNEL_CLIENT_BINARY: fakeClient };
const launch = (bin, args, extra = {}) => {
  const child = spawn(bin, args, { cwd: root, env: { ...env, ...extra }, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stdout.on('data', value => logs.push(String(value))); child.stderr.on('data', value => logs.push(String(value))); return child;
};
const stop = async child => {
  if (!child || child.exitCode !== null) return;
  child.kill('SIGTERM'); await Promise.race([new Promise(r => child.once('exit', r)), new Promise(r => setTimeout(r, 3000))]);
  if (child.exitCode === null) child.kill('SIGKILL');
};
try {
  daemon = launch('bash', ['scripts/dev-daemon.sh']);
  await waitFor(async () => (await fetch(`http://127.0.0.1:${port}/health`)).ok, '隔离 daemon', 180000);
  desktop = launch(createRequire(import.meta.url)('electron'), [`--remote-debugging-port=${cdpPort}`, `--host-resolver-rules=MAP chatgpt.com 127.0.0.1:${sitePort},MAP platform.openai.com 127.0.0.1:${sitePort}`, '--ignore-certificate-errors', 'apps/desktop'], { MAGI_DESKTOP_REUSE_DAEMON: '1', MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_DESKTOP_USER_DATA: join(work, 'desktop') });
  page = await waitFor(async () => {
    const p = await connectAppPage(cdpPort, port, errors).catch(() => null);
    if (p && await p.evaluate(`Boolean(document.querySelector('[data-testid="input-textarea"]'))`)) return p;
    p?.close(); return null;
  }, '工作台', 60000);
  page.on('Runtime.exceptionThrown', p => logs.push('EXC_DETAIL ' + JSON.stringify({ url: p.exceptionDetails.url, line: p.exceptionDetails.lineNumber, stack: (p.exceptionDetails.stackTrace?.callFrames || []).slice(0, 5).map(f => `${f.functionName}@${f.url.split('/').pop()}:${f.lineNumber}`) }) + '\n'));
  const t = pageTools(page);
  check('未安装插件拒绝配置状态', (await t.api('/api/browser/web-models/setup?sessionId=absent')).status === 403);
  const installed = await t.post('/api/plugins/bundled/openai.chatgpt-web/install', {});
  const manifest = JSON.parse(installed.body).plugins.find(p => p.manifest.id === 'openai.chatgpt-web').manifest;
  for (const action of ['enable', 'authorize', 'activate']) check('插件 ' + action, (await t.post('/api/plugins/openai.chatgpt-web/' + action, { scope: 'application', ...(action === 'authorize' ? { grants: manifest.permissions } : {}) })).status === 200);
  check('配置原生工具模型', (await t.post('/api/settings/orchestrator/save', { baseUrl: `http://127.0.0.1:${provider.address().port}/v1`, apiKey: 'fixture', apiProtocol: 'openai_chat' })).status === 200);
  const materialized = await t.post('/api/session/materialize', { scope: 'personal' });
  check('创建隔离验收会话', materialized.status === 200, materialized.body);
  const { sessionId } = JSON.parse(materialized.body);
  sessionIdForDiagnostics = sessionId;
  const selected = await t.post('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId, config: { model: 'setup-fixture', reasoningEffort: null } });
  check('为当前会话选择原生工具模型', selected.status === 200, selected.body);
  await page.call('Page.addScriptToEvaluateOnNewDocument', { source: "window.addEventListener('unhandledrejection', e => console.error('UNHANDLED ' + (e.reason && e.reason.stack || e.reason))); const ce = console.error.bind(console); console.error = (...a) => ce(...a.map(x => x instanceof Error ? x.stack : x));" });
  await page.call('Page.navigate', { url: `http://127.0.0.1:${port}/web.html?scope=personal&sessionId=${encodeURIComponent(sessionId)}` });
  await waitFor(() => t.exists('[data-testid="input-textarea"]'), '已选择验收会话');
  await page.evaluate(`window.dispatchEvent(new CustomEvent('magi-plugin-registry-changed'))`);
  await page.evaluate(`window.dispatchEvent(new CustomEvent('magi-plugin-registry-changed'))`);
  await t.click('[data-testid="sidebar-settings"]');
  await waitFor(() => t.exists('[data-testid="settings-nav-webModel"]'), 'GPT Web 设置'); await t.click('[data-testid="settings-nav-webModel"]');
  await waitFor(() => t.exists('[data-web-model-connect]'), '页面入口'); await t.click('[data-web-model-connect]');
  await waitFor(() => connectTarget(cdpPort, target => target.type === 'webview', 'guest').then(g => { g.close(); return true; }).catch(() => null), '单一 guest', 60000);
  await t.click('[data-testid="sidebar-settings"]');
  await waitFor(() => t.exists('[data-testid="settings-nav-webModel"]'), '返回 GPT Web 设置');
  await t.click('[data-testid="settings-nav-webModel"]');
  await waitFor(() => page.evaluate(`(() => { const button = document.querySelector('[data-web-model-setup-entry] button'); if (!button) return false; button.click(); return true; })()`), 'AI 配置入口');
  // Agent 正在「思考」（第 3 次模型调用停在返回前）时，它仍持有页面控制：这样用户此刻碰页面，
  // Main 才认得出这是对 Agent 的接管；若每条命令后就释放，两条命令之间的接管无从识别。
  const appTab = async () => JSON.parse((await t.api('/api/browser/sessions/app')).body).session;
  await waitFor(() => calls.length >= 3, 'Agent 已开始工作（第 3 次模型调用停在返回前）', 60000, 100);
  const thinking = await appTab();
  check('Agent 思考期间仍持有页面控制', thinking.agentOccupied === true && thinking.controlMode === 'agent', JSON.stringify({ occupied: thinking.agentOccupied, mode: thinking.controlMode }));
  releaseModelHold();
  await waitFor(() => page.evaluate(`document.querySelector('[data-web-model-setup="credentials"]')?.textContent.includes('表单已准备')`), 'AI 自主创建 Tunnel 和准备密钥', 120000);
  const handedOff = await appTab();
  check('交接给用户后释放页面控制（不残留 Agent 占用）', handedOff.agentOccupied !== true && handedOff.controlMode !== 'agent', JSON.stringify({ occupied: handedOff.agentOccupied, mode: handedOff.controlMode }));
  check('配置时右栏展开并选中 GPT Web 标签', await page.evaluate(`(() => { const aside = document.querySelector('aside.right-pane'); const selected = [...document.querySelectorAll('.right-pane-tab')].some(tab => tab.getAttribute('aria-selected') === 'true' && tab.textContent.includes('GPT Web')); return Boolean(aside) && aside.getBoundingClientRect().width > 200 && selected; })()`));
  check('页面变化经真实模型工具循环推进', calls.length >= 5);
  check('原生填写密钥表单但未生成凭据', await guestEval(`document.querySelector('[role=dialog] input').value==='Magi Tunnel Runtime' && document.querySelectorAll('[role=dialog] input[type=checkbox]:checked').length===2 && !localStorage.generated`));
  check('密钥表单经一次批量动作完成，且每一步都有确认结果', calls.some(c => c.messages.some(m => m.role === 'user' && String(m.content).includes('批量全部执行完成'))));
  check('模型收到标准工具历史', calls.some(c => c.messages.some(m => m.role === 'tool') && c.messages.some(m => m.role === 'assistant' && m.tool_calls?.length)));
  const secret = 'sk-fixture-private-runtime-key';
  await page.evaluate(`(() => { const input=document.querySelector('[data-web-model-setup-credentials] input[name="apiKey"]');input.value=${JSON.stringify(secret)};input.dispatchEvent(new Event('input',{bubbles:true})); })()`);
  await t.click('[data-web-model-setup-credentials] button[type="submit"]');
  await waitFor(() => t.exists('[data-web-model-setup="complete"]'), '通道连接后 AI 刷新并确认连接器', 120000);
  check('安全凭据不进入模型上下文', !JSON.stringify(calls).includes(secret));
  check('完成有本次工具刷新页面证据', await guestEval(`/^Tools refreshed: [1-9]\\d* tools available$/.test(document.querySelector('#result').textContent)`));
  check('daemon 先拒绝了未经刷新的完成声明', calls.some(c => c.messages.some(m => m.role === 'user' && String(m.content).includes('daemon 尚未观察到 ChatGPT 经 Tunnel 读取 Magi 工具目录'))));
  const setupTools = new Set(['setup_actions', 'browser_snapshot', 'browser_click', 'browser_type', 'browser_press', 'browser_scroll', 'browser_fill_form', 'browser_hover', 'browser_navigate', 'setup_checkpoint', 'setup_wait']);
  const toolCalls = calls.filter(c => c.tools?.length);
  const bare = tool => tool.function.name.replace(/^magi_plugin_/, ''); // 桥接层给插件来源工具加命名空间前缀
  check('配置只暴露通用浏览器原语与检查点', toolCalls.length > 0 && toolCalls.every(c => c.tools.every(tool => setupTools.has(bare(tool)))), JSON.stringify([...new Set(toolCalls.flatMap(c => c.tools.map(bare)))].filter(name => !setupTools.has(name))));
  check('无 Renderer / fixture 异常', errors.length === 0, errors.join('\n'));
  process.stdout.write(`受控 AI 配置验收：${summary()}\n`);
} catch (error) {
  const pageState = await guestEval(`JSON.stringify({ href: location.href, title: document.title, text: document.body?.innerText.slice(0, 300) })`).catch(String);
  await writeFile(join(work, 'failure.log'), logs.join('') + '\n' + errors.join('\n') + '\nLAST_RESULTS ' + calls.slice(-6).map(c => String(c.messages.at(-1).content).split('上一动作结果：')[1]?.split('\n本次已执行')[0]).join(' || ') + '\nPANE_STATE ' + await page?.evaluate(`JSON.stringify((() => { const a = document.querySelector('aside.right-pane'); const r = a?.getBoundingClientRect(); const tabs = [...document.querySelectorAll('.right-pane-tab')].map(t => t.textContent.trim() + (t.getAttribute('aria-selected') === 'true' ? '*' : '')); return { aside: r ? [Math.round(r.width), Math.round(r.height)] : null, tabs, webviews: document.querySelectorAll('webview').length }; })())`).catch(String) + '\nLAST_ELEMENTS ' + (() => { try { const o = JSON.parse(String(calls.at(-1).messages.at(-1).content).split('最新页面观察：')[1].split('\n上一动作结果：')[0]); return JSON.stringify(o.elements.map(e => [e.role, e.name, e.value])); } catch (e) { return String(e); } })() + '\nGUEST_STATE ' + pageState + '\nSETUP_STATUS ' + JSON.stringify(await fetch(`http://127.0.0.1:${port}/api/browser/web-models/setup?sessionId=${sessionIdForDiagnostics}`).then(r => r.text()).catch(String)));
  throw new Error(`${error.message}\n诊断：${join(work, 'failure.log')}`);
} finally {
  page?.close(); await stop(desktop); await stop(daemon);
  await new Promise(r => provider.close(r)); await new Promise(r => site.close(r));
  if (!process.env.MAGI_SETUP_KEEP_ARTIFACTS) await rm(work, { recursive: true, force: true });
}
