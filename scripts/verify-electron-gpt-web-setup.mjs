// 无真实账号：独立 daemon + Electron guest 验收配置交接；平台和 tunnel-client 使用受控夹具。
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:net';
import { connectAppPage, connectTarget, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = new URL('..', import.meta.url).pathname.replace(/\/$/u, '');
const work = await mkdtemp(join(tmpdir(), 'magi-gpt-setup-'));
const stateRoot = join(work, 'state');
const port = Number(process.env.MAGI_SETUP_TEST_PORT || 38951);
const cdpPort = port + 1;
const vitePort = port + 2;
const { check, summary } = createReporter();
const errors = [], fixtureErrors = [], logs = [];
let daemon, desktop, page, guest;
let loggedIn = false;
let rejectConnector = Boolean(process.env.MAGI_SETUP_TEST_CONNECTOR_RETRY);
let stallRefresh = Boolean(process.env.MAGI_SETUP_TEST_RECONFIGURE);
const fakeClient = join(work, 'tunnel-client');
const client = `#!/bin/sh
if [ "$2" = "connect" ]; then
 if [ "${process.env.MAGI_SETUP_TEST_CHANNEL_RETRY ? '1' : '0'}" = "1" ] && [ ! -f "$0.allow-connect" ]; then exit 17; fi
 if [ "${process.env.MAGI_SETUP_TEST_SLOW_CHANNEL ? '1' : '0'}" = "1" ]; then printf 'connect\n' >> "$0.connects"; sleep 3; fi
 touch "$0.alive"; printf '{"running":true,"healthy":true,"ready":true,"pid":99}'; exit 0
fi
if [ "$2" = "stop" ]; then rm -f "$0.alive"; exit 0; fi
if [ "$2" = "cleanup" ]; then
 if [ -f "$0.alive" ]; then printf '{"entries":[{"alias":"magi-web-model","runtime_state":"ready","live_runtime":{"system":{"pid":99}}}]}'; else printf '{"entries":[]}'; fi
 exit 0
fi
exit 17
`;
await writeFile(fakeClient, client, { mode: 0o700 });
await writeFile(fakeClient + '.sha256', createHash('sha256').update(client).digest('hex'));
await mkdir(stateRoot, { recursive: true });
const env = { ...process.env, MAGI_STATE_ROOT: stateRoot, MAGI_PORT: String(port), MAGI_WEB_DEV: process.env.MAGI_SETUP_TEST_STATIC ? '0' : '1', MAGI_WEB_DEV_PORT: String(vitePort), MAGI_TARGET_PRUNE_GIB: '100', MAGI_OPEN_BROWSER: '0', MAGI_TUNNEL_CLIENT_BINARY: fakeClient };
const launch = (bin, args, extra = {}) => {
  const child = spawn(bin, args, { cwd: root, env: { ...env, ...extra }, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stdout.on('data', value => logs.push(String(value)));
  child.stderr.on('data', value => logs.push(String(value)));
  return child;
};
const stop = async child => {
  if (!child || child.exitCode !== null) return;
  child.kill('SIGTERM');
  await Promise.race([new Promise(resolve => child.once('exit', resolve)), new Promise(resolve => setTimeout(resolve, 3000))]);
  if (child.exitCode === null) child.kill('SIGKILL');
};
const home = () => `<!doctype html><meta charset="utf-8"><title>Setup fixture</title><main>${loggedIn ? '<form><div id="prompt-textarea" contenteditable="true" role="textbox"></div><button data-testid="send-button">Send</button></form>' : '<button>Log in</button>'}</main><script>window.fetch=async()=>({ok:true,json:async()=>(${loggedIn ? '{user:{name:"fixture"}}' : '{}'})});</script>`;
const idOnlyTunnels = Boolean(process.env.MAGI_SETUP_TEST_ID_ONLY);
const workspaceSelection = Boolean(process.env.MAGI_SETUP_TEST_WORKSPACE);
const uniqueWorkspace = Boolean(process.env.MAGI_SETUP_TEST_UNIQUE_WORKSPACE);
const tunnelRow = idOnlyTunnels
  ? '<tr><td>tunnel_0123456789abcdef0123456789abcdef</td><td><button aria-label="Edit tunnel" onclick="editTunnel()">✎</button></td></tr>'
  : '<tr><td>Magi</td><td>tunnel_0123456789abcdef0123456789abcdef</td></tr>';
const tunnels = `<!doctype html><title>Platform fixture</title><main><p id="empty">No tunnels yet</p><table><tbody></tbody></table><button id="add">Create tunnel</button></main><script>
const row=${JSON.stringify(tunnelRow)};
function workspaceMenu(){const old=document.querySelector('#choices');if(old){old.remove();return}const menu=document.createElement('div');menu.id='choices';menu.setAttribute('role','listbox');for(const name of ${uniqueWorkspace ? "['Workspace A']" : "['Workspace A','Workspace B']"}){const option=document.createElement('button');option.setAttribute('role','option');option.textContent=name;option.onclick=()=>{workspace.textContent=name;option.setAttribute('aria-selected','true')};menu.append(option)}document.querySelector('dialog').append(menu)}
function closeTunnelDetails(){document.querySelector('dialog').remove()}
function unexpectedTunnelSave(){localStorage.setItem('unexpectedTunnelSave','1')}
function editTunnel(){localStorage.setItem('fixtureDetailReadCount',String(Number(localStorage.getItem('fixtureDetailReadCount')||0)+1));document.body.insertAdjacentHTML('beforeend','<dialog open role="dialog"><h2>Edit tunnel</h2><input type="text" value="Magi"><textarea>Connect this ChatGPT workspace to Magi tools through the local Magi client.</textarea><button onclick="closeTunnelDetails()">Cancel</button><button onclick="unexpectedTunnelSave()">Save</button></dialog>')}
add.onclick=()=>{setTimeout(()=>{document.body.insertAdjacentHTML('beforeend','<dialog open role="dialog"><h2>Create tunnel</h2><input type="text"><textarea></textarea>${workspaceSelection || uniqueWorkspace ? '<button id="workspace" onclick="workspaceMenu()">Search workspaces by name or ID</button>' : ''}<button id="save" disabled>Create</button></dialog>');setTimeout(()=>save.disabled=false,900);save.onclick=()=>{document.querySelector('#empty')?.remove();localStorage.setItem('fixtureTunnel','1');localStorage.setItem('fixtureCreationCount',String(Number(localStorage.getItem('fixtureCreationCount')||0)+1));document.querySelector('tbody').innerHTML=row;document.querySelector('dialog').remove()}},900)};
if(localStorage.getItem('fixtureTunnel')){document.querySelector('#empty')?.remove();document.querySelector('tbody').innerHTML=row;}
</script>`;
const runtimeKeys = `<!doctype html><title>API keys fixture</title><main><button onclick="createForm()">Create new secret key</button></main><script>
function createForm(){document.body.insertAdjacentHTML('beforeend','<dialog open role="dialog"><h2>Create new secret key</h2><input type="text" placeholder="My Test Key"><label id="project-label">Project<span id="project" aria-haspopup="listbox" aria-disabled="true">Loading projects...</span></label><button id="expiry" disabled aria-label="有效期 选择有效期" onclick="optionsMenu(this,[String.fromCharCode(49,32,22825),String.fromCharCode(51,48,32,22825)])">选择有效期</button><label><input type="radio" name="permissions" checked>All</label><label><input type="radio" name="permissions">Restricted</label><button id="tunnel-label" disabled>Tunnels</button><div id="access" role="button" tabindex="0" aria-labelledby="tunnel-label" onclick="permissionsMenu()">None</div><span id="count">0 selected permissions</span><button onclick="unexpectedGenerate()">Create secret key</button></dialog>');setTimeout(()=>{const control=document.createElement('button');control.id='project';control.setAttribute('aria-labelledby','project-label');control.textContent='Select project...';control.onclick=()=>optionsMenu(control,['Default project']);document.querySelector('#project').replaceWith(control)},900)}
function unexpectedGenerate(){localStorage.setItem('unexpectedKeyGeneration','1')}
function optionsMenu(target,values){const old=document.querySelector('#menu');if(old){old.remove();return}const menu=document.createElement('div');menu.id='menu';menu.setAttribute('role','listbox');for(const value of values){const option=document.createElement('button');option.setAttribute('role','option');option.textContent=value;option.onclick=()=>{target.textContent=value;if(target.id==='expiry')target.setAttribute('aria-label','有效期 '+value);if(target.id==='project')setTimeout(()=>expiry.disabled=false,500);setTimeout(()=>menu.remove(),150)};menu.append(option)}document.querySelector('dialog').append(menu)}
const selected=new Set();function permissionsMenu(){const old=document.querySelector('#menu');if(old){old.remove();return}const menu=document.createElement('div');menu.id='menu';menu.setAttribute('role','listbox');for(const value of ['Read','Use']){const option=document.createElement('button');option.setAttribute('role','option');option.setAttribute('aria-selected',String(selected.has(value)));option.textContent=value;option.onclick=()=>{selected.add(value);option.setAttribute('aria-selected','true');access.textContent=selected.size===2?'All selected':value;count.textContent=selected.size+' selected permissions'};menu.append(option)}document.querySelector('dialog').append(menu)}
</script>`;
const connectorList = `<!doctype html><title>Connectors fixture</title><main>Installed apps<ul id="apps"></ul></main><script>if(localStorage.getItem('fixtureConnector'))apps.innerHTML='<li><a href="/settings/plugins-settings/magi"><span>Magi</span><span>Magi</span></a><span>允许使用低风险工具</span></li>';else apps.innerHTML='<p>你尚未安装任何插件</p>';</script>`;
const connectorDetail = () => `<!doctype html><title>Connector details</title><main><h1>Magi</h1>tunnel_0123456789abcdef0123456789abcdef<button id="refresh" disabled>Refresh tools</button></main><script>setTimeout(()=>refresh.disabled=false,${process.env.MAGI_SETUP_TEST_SLOW_REFRESH ? '12000' : '700'});refresh.onclick=()=>{refresh.disabled=true;if(!${stallRefresh})setTimeout(()=>{refresh.disabled=false;localStorage.setItem('fixtureRefresh','1');localStorage.setItem('fixtureRefreshCount',String(Number(localStorage.getItem('fixtureRefreshCount')||0)+1))},700)};</script>`;
const connectorDirectory = () => `<!doctype html><title>Directory fixture</title><main><button id="add" disabled>Add</button></main><script>setTimeout(()=>add.disabled=false,900);
add.onclick=()=>{document.body.insertAdjacentHTML('beforeend','<button role="menuitem">创建插件</button><button role="menuitem" id="newApp">添加自定义 MCP 服务器</button>');newApp.onclick=()=>{document.body.insertAdjacentHTML('beforeend','<dialog open role="dialog"><button>Tunnel</button><input type="text" name="name"><input type="text" aria-label="Tunnel id"><select><option value="token">Token</option><option value="none">No authentication</option></select><button role="checkbox" aria-checked="false" id="acknowledge">I understand</button><button id="save" disabled type="submit">创建为插件</button></dialog>');acknowledge.onclick=()=>acknowledge.setAttribute('aria-checked','true');setTimeout(()=>save.disabled=false,900);save.onclick=()=>{const d=document.querySelector('dialog');if(d.querySelector('select').value!=='none'||d.querySelector('[role=checkbox]').getAttribute('aria-checked')!=='true'||d.querySelector('input[name=name]').value!=='Magi')return;if(${rejectConnector}){d.insertAdjacentHTML('beforeend','<p>MCP 服务器拒绝了访问。请检查隧道的工作空间访问权限。private-diagnostic-should-stay-on-page</p>');return;}localStorage.setItem('fixtureConnectorCount',String(Number(localStorage.getItem('fixtureConnectorCount')||0)+1));d.remove();setTimeout(()=>{document.body.insertAdjacentHTML('beforeend','<dialog open role="dialog"><h2>连接 Magi</h2><button id="connectMagi" disabled>连接 Magi</button><button>取消</button></dialog>');setTimeout(()=>connectMagi.disabled=false,900);connectMagi.onclick=()=>{localStorage.setItem('fixtureConnector','1');localStorage.setItem('fixtureConnectCount',String(Number(localStorage.getItem('fixtureConnectCount')||0)+1));document.querySelector('dialog').remove()}},${process.env.MAGI_SETUP_TEST_SLOW_CONFIRM ? '12000' : '900'})}}};
</script>`;

try {
  // dev-daemon 会重启占用端口的旧 Magi；验收必须先确认独立端口空闲，不能接管其他运行。
  for (const isolatedPort of [port, cdpPort, vitePort]) {
    await new Promise((resolve, reject) => {
      const server = createServer();
      server.once('error', reject);
      server.listen(isolatedPort, '127.0.0.1', () => server.close(resolve));
    });
  }
  daemon = launch('bash', ['scripts/dev-daemon.sh']);
  await waitFor(async () => (await fetch(`http://127.0.0.1:${port}/health`)).ok, '隔离 daemon', 180000);
  desktop = launch(createRequire(import.meta.url)('electron'), ['--disable-gpu', `--remote-debugging-port=${cdpPort}`, 'apps/desktop'], { MAGI_DESKTOP_REUSE_DAEMON: '1', MAGI_DESKTOP_DAEMON_PORT: String(port) });
  page = await waitFor(async () => {
    const candidate = await connectAppPage(cdpPort, port, errors).catch(() => null);
    if (!candidate) return null;
    if (await candidate.evaluate(`Boolean(document.querySelector('[data-testid="input-textarea"]'))`)) return candidate;
    candidate.close(); return null;
  }, '配置助手工作台', 60000);
  const tools = pageTools(page);
  const fillSetupDraft = async () => {
    await page.evaluate(`(() => { const e=document.querySelector('[data-testid="input-textarea"]');e.focus();e.textContent='帮我配置 GPT Web';e.dispatchEvent(new InputEvent('input',{bubbles:true,inputType:'insertText',data:'帮我配置 GPT Web'})); })()`);
  };
  const startFromChat = async () => {
    await fillSetupDraft();
    await waitFor(() => tools.exists('[data-testid="input-send-button"]:not([disabled])'), '无模型也能从主对话开始配置');
    await tools.click('[data-testid="input-send-button"]');
  };
  const openEntry = async () => {
    await tools.click('[data-testid="sidebar-settings"]');
    await waitFor(() => tools.exists('[data-testid="settings-nav-model"]'), '模型设置入口');
    await tools.click('[data-testid="settings-nav-model"]');
  };
  await openEntry();
  check('未安装插件时模型设置不显示 GPT Web 或配置助手', !await tools.exists('[data-web-model-setup-entry]') && !await tools.exists('[data-testid="settings-nav-webModel"]'));
  // 未安装时配置助手根本不接管输入或事件，后端也必须独立拒绝伪造命令。
  await page.evaluate(`window.dispatchEvent(new CustomEvent('magi:gptWebSetup'))`);
  await new Promise(resolve => setTimeout(resolve, 150));
  check('未安装时配置事件不接管页面或跳转插件管理', await page.evaluate(`document.querySelector('[data-testid="settings-nav-model"]')?.classList.contains('active')`));
  check('未安装时配置状态接口拒绝访问', (await tools.api('/api/browser/web-models/setup?sessionId=absent')).status === 403);
  const rejectedSetup = await tools.post('/api/session/turn', { scope: 'personal', text: 'start', pluginCommandId: 'plugin/openai.chatgpt-web/setup', requestId: 'uninstalled-setup' });
  check('未安装时伪造插件命令也被 daemon 拒绝', rejectedSetup.status === 403, rejectedSetup.body);
  check('未安装插件时不创建浏览器宿主', !await tools.exists('webview'));
  await tools.click('[data-testid="settings-back"]');
  await fillSetupDraft();
  check('未安装时配置文字不绕过普通模型要求且保留草稿', await page.evaluate(`document.querySelector('[data-testid="input-send-button"]').disabled && document.querySelector('[data-testid="input-textarea"]').textContent==='帮我配置 GPT Web' && !document.querySelector('[data-web-model-setup]')`));
  await openEntry();
  const installed = await tools.post('/api/plugins/bundled/openai.chatgpt-web/install', {});
  const manifest = JSON.parse(installed.body).plugins.find(item => item.manifest.id === 'openai.chatgpt-web').manifest;
  check('已安装未激活时仍无 GPT Web 管理页', !await tools.exists('[data-testid="settings-nav-webModel"]'));
  check('已安装未激活时不贡献配置命令', !JSON.parse((await tools.api('/api/plugins/commands?scope=application')).body).some(item => item.id === 'plugin/openai.chatgpt-web/setup'));
  for (const [action, body] of [['enable', {}], ['authorize', { grants: manifest.permissions }], ['activate', {}]]) {
    const result = await tools.post('/api/plugins/openai.chatgpt-web/' + action, { scope: 'application', ...body });
    check('插件 ' + action, result.status === 200, result.body);
  }
  await page.evaluate(`window.dispatchEvent(new CustomEvent('magi-plugin-registry-changed'))`);
  await waitFor(() => tools.exists('[data-testid="settings-nav-webModel"]'), '插件设置入口');
  await tools.click('[data-testid="settings-nav-model"]');
  check('插件激活后模型设置仍不展示 GPT Web 管理或引擎注册', !await page.evaluate(`document.querySelector('.settings-tab-content').innerText.includes('GPT Web')`));
  await tools.click('[data-testid="settings-nav-webModel"]');
  await waitFor(() => tools.exists('[data-web-model-setup-entry]'), '插件内配置助手');
  check('配置助手只在 GPT Web 插件页显示', true);
  if (await tools.exists('.toast-close')) await tools.click('.toast-close');
  for (const width of [360, 390, 720, 1024, 1440]) {
    await page.call('Emulation.setDeviceMetricsOverride', { width, height: 1000, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(`document.querySelector('[data-web-model-tunnel]').open = true`);
    check(`插件设置 ${width}px 宽度无横向溢出`, await page.evaluate(`(()=>{const e=document.querySelector('.web-model-settings .apple-scroller-proxy');return e.clientWidth>0 && e.scrollWidth<=e.clientWidth+1 && [...e.querySelectorAll('button,input,select')].every(c=>c.getBoundingClientRect().right<=e.getBoundingClientRect().right+1)})()`));
    if (process.env.MAGI_SETUP_SETTINGS_SCREENSHOT_PREFIX) await writeFile(process.env.MAGI_SETUP_SETTINGS_SCREENSHOT_PREFIX + '-' + width + '.png', Buffer.from(await tools.screenshot(), 'base64'));
  }
  await page.call('Emulation.clearDeviceMetricsOverride');
  await page.evaluate(`document.querySelector('[data-web-model-tunnel]').open = false`);
  await waitFor(() => tools.exists('[data-web-model-connect]'), '浏览器连接入口');
  await tools.click('[data-web-model-connect]');
  await waitFor(() => tools.exists('webview'), '单一 GPT Web guest');
  guest = await waitFor(() => connectTarget(cdpPort, target => target.type === 'webview', 'setup guest').catch(() => null), '连接 guest');
  guest.on('Fetch.requestPaused', event => {
    const url = new URL(event.request.url);
    const document = event.resourceType === 'Document';
    const html = url.hostname === 'platform.openai.com'
      ? (url.pathname.includes('/tunnels') ? tunnels : runtimeKeys)
      : url.pathname.startsWith('/settings/plugins-settings/') ? connectorDetail()
      : url.pathname.startsWith('/settings/') ? connectorList : url.pathname.startsWith('/plugins') ? connectorDirectory() : home();
    void guest.call('Fetch.fulfillRequest', { requestId: event.requestId, responseCode: 200,
      responseHeaders: [{ name: 'Content-Type', value: document ? 'text/html; charset=utf-8' : 'application/json' }],
      body: Buffer.from(document ? html : JSON.stringify(loggedIn ? { user: { name: 'fixture' } } : {})).toString('base64'),
    }).catch(error => {
      // 快速导航或停用 guest 会取消已拦截的请求；这是夹具回包竞态，
      // 与 Renderer 的 Runtime 异常分开收集，其他回包失败仍必须报错。
      if (error.message !== 'Invalid InterceptionId.') fixtureErrors.push(String(error));
    });
  });
  await guest.call('Fetch.enable', { patterns: [{ urlPattern: 'https://chatgpt.com/*' }, { urlPattern: 'https://platform.openai.com/*' }] });
  await page.evaluate(`(() => {
    window.__setupCursorEvents = [];
    window.__setupCursorRendered = false;
    window.magiDesktop.onBrowserEvent(event => {
      if (event.type === 'agent_cursor' || event.type === 'user_takeover') window.__setupCursorEvents.push(event);
    });
    new MutationObserver(() => {
      if (document.querySelector('.agent-cursor-layer:popover-open .agent-cursor')) window.__setupCursorRendered = true;
    }).observe(document.body, {subtree:true, childList:true, attributes:true});
  })()`);
  const cursorHidden = () => page.evaluate(`!document.querySelector('.agent-cursor-layer:popover-open .agent-cursor')`);
  const continueSetup = async () => {
    await waitFor(() => page.evaluate(`Boolean(document.querySelector('[data-web-model-setup] .setup-actions .btn--primary:not([disabled])'))`), '配置继续按钮可用', 45000);
    await tools.click('[data-web-model-setup] .setup-actions .btn--primary');
  };
  const paneExpanded = () => page.evaluate(`document.querySelector('.header-right-pane-btn')?.getAttribute('aria-expanded') === 'true' && Boolean(document.querySelector('.right-pane-body--web-model'))`);
  const collapsePane = async () => {
    if (await paneExpanded()) await tools.click('.header-right-pane-btn');
    await waitFor(() => page.evaluate(`document.querySelector('.header-right-pane-btn')?.getAttribute('aria-expanded') === 'false'`), '右栏折叠');
  };
  await guest.call('Page.navigate', { url: 'https://chatgpt.com/' });
  await tools.click('[data-testid="settings-back"]');
  await collapsePane();
  await startFromChat();
  await waitFor(() => tools.exists('[data-web-model-setup]'), '配置状态卡片', 60000);
  await waitFor(() => tools.exists('[data-web-model-setup="login"]'), '等待用户登录', 60000);
  check('无本地模型也能执行配置，登录时显示接手提示', true);
  check('从对话发起配置不会打开 GPT Web 设置页', !await page.evaluate(`Boolean(document.querySelector('[data-testid=\"settings-back\"]')?.checkVisibility())`));
  await waitFor(paneExpanded, '开始配置自动展开 GPT Web 右栏');
  check('开始配置自动展开右栏并选中 GPT Web', true);
  await collapsePane();
  await tools.click('[data-web-model-setup] .setup-actions .btn--secondary');
  await waitFor(paneExpanded, '查看操作页面重新展开 GPT Web 右栏');
  check('查看操作页面重新展开右栏', true);
  await collapsePane();
  check('等待用户登录时隐藏 Magi 光标', await cursorHidden());
  loggedIn = true;
  await guest.call('Page.navigate', { url: 'https://chatgpt.com/' });
  await continueSetup();
  if (process.env.MAGI_SETUP_TEST_TAKEOVER) {
    await waitFor(async () => {
      const visible = await page.evaluate(`window.__setupCursorEvents.some(e => e.type === 'agent_cursor' && e.visible && e.action === 'click')`);
      if (!visible) return false;
      await guest.call('Input.dispatchMouseEvent', { type: 'mousePressed', x: 240, y: 200, button: 'left', clickCount: 1 });
      await guest.call('Input.dispatchMouseEvent', { type: 'mouseReleased', x: 240, y: 200, button: 'left', clickCount: 1 });
      return true;
    }, '向页面投递用户鼠标接管');
    await waitFor(() => page.evaluate(`window.__setupCursorEvents.some(e => e.type === 'user_takeover')`), '配置期间用户接管');
    await waitFor(() => page.evaluate(`Boolean(document.querySelector('[data-web-model-setup="tunnel"]')) && !document.querySelector('[data-web-model-setup] .setup-actions .btn--primary')?.disabled`), '接管后配置步骤停止', 60000);
    await waitFor(cursorHidden, '接管后隐藏 Magi 光标');
    check('用户接管立即隐藏光标并停止后续输入', true);
    check('接管后不继续填表或创建 Tunnel', await guest.evaluate(`!localStorage.getItem('fixtureTunnel') && !document.querySelector('dialog input')?.value`));
    await waitFor(() => page.evaluate(`document.querySelector('[data-web-model-setup] .setup-actions .btn--primary')?.textContent.includes('检查并重试创建') || document.querySelector('[data-web-model-setup] .setup-actions .btn--primary')?.textContent.includes('Check and retry creation')`), '未确认创建的检查重试操作');
    check('未确认创建显示明确的检查重试操作', true);
    // 用户确认没有创建后交还控制；页面仍为空，必须先检查再允许显式重试。
    await guest.call('Page.navigate', {url:'https://platform.openai.com/settings/organization/tunnels'});
    check('重试前平台仍无 Tunnel', await guest.evaluate(`!localStorage.getItem('fixtureTunnel')`));
    if (process.env.MAGI_SETUP_TEST_MANUAL_TUNNEL) {
      await tools.click('[data-web-model-setup] .setup-actions button:nth-child(2)');
      await waitFor(() => tools.exists('[data-web-model-setup-credentials]'), '主对话手动 Tunnel 安全卡片');
      await page.evaluate(`(()=>{const f=document.querySelector('[data-web-model-setup-credentials]');const e=f.querySelector('[name=tunnelId]');e.value='tunnel_0123456789abcdef0123456789abcdef';e.dispatchEvent(new Event('input',{bubbles:true}));f.requestSubmit()})()`);
    } else await continueSetup();
  }
  if (workspaceSelection) {
    await waitFor(() => tools.exists('.tunnel-guide'), '真实页面工作空间选择提示', 60000);
    check('名称和描述由 Agent 填入实际平台表单', await guest.evaluate(`document.querySelector('dialog input').value === 'Magi' && document.querySelector('dialog textarea').value === 'Connect this ChatGPT workspace to Magi tools through the local Magi client.'`));
    check('配置卡片逐项解释名称、描述、组织与工作空间', await page.evaluate(`document.querySelectorAll('.tunnel-guide dt').length === 4 && document.querySelector('.tunnel-guide').textContent.includes('Organizations')`));
    check('等待用户选择工作空间时未创建 Tunnel', await guest.evaluate(`!localStorage.getItem('fixtureTunnel')`));
    if (process.env.MAGI_SETUP_TEST_GUIDE_SCREENSHOT) await writeFile(process.env.MAGI_SETUP_TEST_GUIDE_SCREENSHOT, Buffer.from(await tools.screenshot(), 'base64'));
    await waitFor(() => guest.evaluate(`Boolean(document.querySelector('#save:not([disabled])'))`), '工作空间表单提交按钮可用');
    await guest.evaluate(`document.querySelector('#workspace').click();document.querySelector('[role=option]').click();document.querySelector('#workspace').click()`);
    await continueSetup();
  }
  await waitFor(() => tools.exists('[data-web-model-setup="credentials"]'), '自动创建 Tunnel 后等待安全输入', 60000);
  await waitFor(paneExpanded, '继续配置重新展开 GPT Web 右栏');
  check('继续配置重新展开右栏并保留原 guest', true);
  check('自动操作期间实际渲染 Magi 光标覆盖层', await page.evaluate('window.__setupCursorRendered'));
  await waitFor(cursorHidden, '等待密钥输入时隐藏 Magi 光标');
  check('等待密钥输入时交还页面并隐藏光标', true);
  if (!process.env.MAGI_SETUP_TEST_TAKEOVER) {
    check('Tunnel 点击与输入均发布真实光标坐标', await page.evaluate(`['click','type'].every(action => window.__setupCursorEvents.some(e => e.type === 'agent_cursor' && e.visible && e.action === action && Number.isFinite(e.displayX) && Number.isFinite(e.displayY)))`));
  }
  const tunnel = JSON.parse((await tools.api('/api/browser/web-models/tunnel')).body);
  await waitFor(() => page.evaluate(`document.querySelector('[data-web-model-setup-credentials] [name=tunnelId]')?.value==='tunnel_0123456789abcdef0123456789abcdef'`), '卡片自动回填 Tunnel ID');
  check('无需设置页即可在安全卡片查看并修改 Tunnel ID', true);
  check('初次配置缺少密钥时安全保存不可用', await page.evaluate(`document.querySelector('[data-web-model-setup-credentials] button[type=submit]').disabled`));
  check('用户登录后 Magi 创建 Tunnel 并推进到安全输入', tunnel.tunnelId === 'tunnel_0123456789abcdef0123456789abcdef');
  await waitFor(() => page.evaluate(`document.querySelector('[data-web-model-setup]')?.textContent.includes('30 天')`), 'Magi 自动准备最小权限密钥表单', 60000);
  check('密钥名称、唯一项目、有效期与权限自动填写', await guest.evaluate(`document.querySelector('dialog input[type=text]').value==='Magi Tunnel Runtime' && project.textContent==='Default project' && expiry.textContent==='30 天' && document.querySelector('input[type=radio]:checked').parentElement.textContent==='Restricted' && access.textContent==='All selected' && count.textContent==='2 selected permissions'`));
  if (process.env.MAGI_SETUP_TEST_KEY_SCREENSHOT) await writeFile(process.env.MAGI_SETUP_TEST_KEY_SCREENSHOT, Buffer.from(await tools.screenshot(), 'base64'));
  check('生成密钥保留人工交接且没有读取或自动生成密钥', await guest.evaluate(`!localStorage.getItem('unexpectedKeyGeneration')`));
  if (uniqueWorkspace) check('唯一工作空间自动选择并创建 Tunnel', await guest.evaluate(`localStorage.getItem('fixtureCreationCount')==='1'`));

  if (idOnlyTunnels && !process.env.MAGI_SETUP_TEST_MANUAL_TUNNEL) {
    check('ID-only 列表打开详情核对 Magi 名称后自动取得 Tunnel ID', await guest.evaluate(`Number(localStorage.getItem('fixtureDetailReadCount')) >= 1`));
    check('回读详情只取消退出，不保存配置', await guest.evaluate(`!localStorage.getItem('unexpectedTunnelSave')`));
  }
  check('自动创建或手动绑定不会重复创建 Tunnel', await guest.evaluate(`Number(localStorage.getItem('fixtureCreationCount')||0) === ${process.env.MAGI_SETUP_TEST_MANUAL_TUNNEL ? 0 : 1}`));
  if (process.env.MAGI_SETUP_TEST_MANUAL_TUNNEL) check('只保存 Tunnel id 后仍提供密钥输入入口', await tools.exists('[data-web-model-setup="credentials"] input[type=password]'));
  check('全流程只有一个真实 webview', await page.evaluate(`document.querySelectorAll('webview').length === 1`));
  for (const width of [390, 720, 960, 1440]) {
    await page.call('Emulation.setDeviceMetricsOverride', { width, height: 900, deviceScaleFactor: 1, mobile: false });
    // 窄窗口先收起右栏，检查主对话中的安全卡片。
    if (width < 900) await collapsePane();
    await waitFor(() => page.evaluate(`(()=>{const e=document.querySelector('[data-web-model-setup]');return e?.clientWidth>=Math.min(300,innerWidth-24)})()`), '配置卡片获得可读宽度');
    if (process.env.MAGI_SETUP_CARD_SCREENSHOT_PREFIX) await writeFile(process.env.MAGI_SETUP_CARD_SCREENSHOT_PREFIX + '-' + width + '.png', Buffer.from(await tools.screenshot(), 'base64'));
    const cardLayout = await page.evaluate(`(()=>{const e=document.querySelector('[data-web-model-setup]');return {width:e.clientWidth,scroll:e.scrollWidth}})()`);
    check(`窗口 ${width}px 配置卡片无横向溢出`, cardLayout.width>0 && cardLayout.scroll<=cardLayout.width+1, JSON.stringify(cardLayout));
    check(`窗口 ${width}px 配置输入框与按钮保持可用宽度`, await page.evaluate(`(()=>{const e=document.querySelector('[data-web-model-setup]');const r=e.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth+1&&[...e.querySelectorAll('input')].every(c=>c.clientWidth>=180)&&[...e.querySelectorAll('input,button')].every(c=>{const b=c.getBoundingClientRect();return b.left>=r.left&&b.right<=r.right+1})})()`));
  }
  await page.call('Emulation.clearDeviceMetricsOverride');
  const submitKey = async (value) => {
    await waitFor(() => tools.exists('[data-web-model-setup="credentials"] .setup-actions button:last-child:not([disabled])'), '安全输入步骤可操作', 60000);
    await page.evaluate(`(()=>{const e=document.querySelector('[data-web-model-setup] input[type=password]');e.value=${JSON.stringify(value)};e.dispatchEvent(new Event('input',{bubbles:true}))})()`);
    await waitFor(() => tools.exists('[data-web-model-setup="credentials"] form button[type=submit]:not([disabled])'), '密钥输入已接纳');
    await page.evaluate(`document.querySelector('[data-web-model-setup] form').requestSubmit()`);
  };
  if (process.env.MAGI_SETUP_TEST_MANUAL_TUNNEL) {
    await submitKey('sk-invalid key');
    await waitFor(() => tools.exists('[data-web-model-setup] [role="alert"]'), '错误密钥的安全输入校验');
    check('错误密钥保留输入步骤且不泄露到对话', await tools.exists('[data-web-model-setup="credentials"]') && !await page.evaluate(`document.body.innerText.includes('sk-invalid key')`));
  }
  await submitKey('sk-fixture-secret-123456');
  if (process.env.MAGI_SETUP_TEST_CHANNEL_RETRY) {
    await waitFor(() => page.evaluate(`document.querySelector('[data-web-model-setup="connecting"] .setup-actions .btn--primary:not([disabled])')?.textContent.includes('重试连接')`), '通道失败可重试');
    check('通道失败展示原因和重试入口', true);
    await tools.click('[data-web-model-setup] .setup-actions button:nth-child(2)');
    await waitFor(() => tools.exists('[data-web-model-setup-credentials]'), '连接失败在主对话修改凭据');
    check('连接失败不跳转设置且不回显已存密钥', !await page.evaluate(`Boolean(document.querySelector('[data-testid=\"settings-back\"]')?.checkVisibility())`) && await page.evaluate(`document.querySelector('[data-web-model-setup-credentials] [name=apiKey]').value===''`));
    await writeFile(fakeClient + '.allow-connect', '1');
    await waitFor(() => tools.exists('[data-web-model-setup-credentials] button[type=submit]:not([disabled])'), '沿用已保存密钥可直接安全保存');
    await page.evaluate(`document.querySelector('[data-web-model-setup-credentials]').requestSubmit()`);
  }
  await waitFor(() => tools.exists(process.env.MAGI_SETUP_TEST_CONNECTOR_RETRY ? '[data-web-model-setup="connector"]' : '[data-web-model-setup="complete"]'), '安全输入后自动配置连接器', 60000);
  check('安全输入后通道和连接器自动配置且密钥不出现在对话', !await page.evaluate(`document.body.innerText.includes('sk-fixture-secret-123456')`));
  if (process.env.MAGI_SETUP_TEST_CONNECTOR_RETRY) {
    await waitFor(() => page.evaluate(`document.querySelector('[data-web-model-setup="connector"] .setup-actions .btn--primary:not([disabled])')?.textContent.includes('检查并重试连接器')`), '连接器失败后可显式授权重试', 60000);
    check('连接器失败保留原操作表单', await guest.evaluate(`location.pathname.startsWith('/plugins') && Boolean(document.querySelector('dialog'))`));
    await waitFor(() => page.evaluate(`document.querySelector('[data-web-model-setup="connector"]')?.textContent.includes('工作空间')`), '访问拒绝状态投影到配置卡片');
    check('Tunnel 访问拒绝提供工作空间处理说明', true);
    check('平台错误正文不进入对话或配置记录', !await page.evaluate(`document.body.innerText.includes('private-diagnostic-should-stay-on-page')`) && !(await readFile(join(stateRoot, 'settings.json'), 'utf8')).includes('private-diagnostic'));
    rejectConnector = false;
    await tools.click('[data-web-model-setup] .setup-actions button:nth-child(2)');
    await waitFor(() => tools.exists('[data-web-model-setup-credentials]'), '访问拒绝时主对话安全修改');
    check('访问拒绝的修复留在主对话', !await page.evaluate(`Boolean(document.querySelector('[data-testid=\"settings-back\"]')?.checkVisibility())`));
    await waitFor(() => tools.exists('[data-web-model-setup-credentials] button[type=submit]:not([disabled])'), '访问拒绝卡片已载入凭据状态');
    await page.evaluate(`(()=>{const e=document.querySelector('[data-web-model-setup-credentials] [name=tunnelId]');e.value='tunnel_fedcba9876543210fedcba9876543210';e.dispatchEvent(new Event('input',{bubbles:true}))})()`);
    check('更换 Tunnel 时必须补充对应密钥', await page.evaluate(`document.querySelector('[data-web-model-setup-credentials] [name=apiKey]').required && document.querySelector('[data-web-model-setup-credentials] button[type=submit]').disabled`));
    await page.evaluate(`(()=>{const e=document.querySelector('[data-web-model-setup-credentials] [name=apiKey]');e.value='sk-discarded-draft';e.dispatchEvent(new Event('input',{bubbles:true}))})()`);
    await tools.click('[data-web-model-setup-credentials] .credential-actions button[type=button]');
    await waitFor(() => page.evaluate(`!document.querySelector('[data-web-model-setup-credentials]')`), '取消凭据草稿');
    await tools.click('[data-web-model-setup] .setup-actions button:nth-child(2)');
    check('取消修改后清空密钥草稿并恢复已保存 Tunnel', await page.evaluate(`document.querySelector('[data-web-model-setup-credentials] [name=apiKey]').value==='' && document.querySelector('[data-web-model-setup-credentials] [name=tunnelId]').value==='tunnel_0123456789abcdef0123456789abcdef'`));
    await page.evaluate(`document.querySelector('[data-web-model-setup-credentials]').requestSubmit()`);
    await waitFor(() => page.evaluate(`!document.querySelector('[data-web-model-setup-credentials]') && document.querySelector('[data-web-model-setup="connector"]')`), '安全保存自动返回连接器检查点');
    await continueSetup();
  }
  await waitFor(() => tools.exists('[data-web-model-setup="complete"]'), '连接器写入与回读确认', 60000);
  if (process.env.MAGI_SETUP_TEST_SLOW_CHANNEL) check('通道启动期间等待原启动任务，不重复连接', (await readFile(fakeClient + '.connects', 'utf8')).trim().split('\n').length === 1);
  check('首次配置无需第二次确认即可创建连接器并回读完成', true);
  check('连接器只创建一次', await guest.evaluate(`localStorage.getItem('fixtureConnectorCount') === '1'`));
  check('创建后等待连接确认并仅连接当前 Magi 一次', await guest.evaluate(`localStorage.getItem('fixtureConnectCount') === '1'`));
  check('连接器配置点击与填表均更新 Magi 光标', await page.evaluate(`['click','type'].every(action => window.__setupCursorEvents.some(e => e.type === 'agent_cursor' && e.visible && e.action === action))`));
  await waitFor(cursorHidden, '配置完成后隐藏 Magi 光标');
  check('配置完成后释放控制并隐藏光标', true);
  const settings = await readFile(join(stateRoot, 'settings.json'), 'utf8');
  check('密钥不进入 settings 或配置状态', !settings.includes('sk-fixture-secret-123456'));
  const key = await readFile(join(stateRoot, 'web-model/tunnel-api-key'), 'utf8');
  check('密钥通过独立安全入口保存', key === 'sk-fixture-secret-123456');
  if (process.env.MAGI_SETUP_TEST_RECONFIGURE) {
    await tools.click('[data-web-model-setup] .setup-actions button');
    await startFromChat();
    await waitFor(() => tools.exists('[data-web-model-setup="connector"]'), '收起后重新开始显示配置卡片', 60000);
    check('收起完成卡片后重新配置仍能继续', true);
    await waitFor(() => page.evaluate(`document.querySelector('[data-web-model-setup="connector"] .setup-actions .btn--primary:not([disabled])')?.textContent.includes('检查并重试连接器')`), '刷新超时后等待重试', 60000);
    check('刷新超时不会因旧连接器启用而误报完成', !await tools.exists('[data-web-model-setup="complete"]') && await guest.evaluate(`location.pathname.includes('/settings/plugins-settings/')`));
    stallRefresh = false;
    await continueSetup();
    await waitFor(() => tools.exists('[data-web-model-setup="complete"]'), '显式重试后工具刷新成功', 60000);
    check('重试刷新成功且没有重复创建连接器', await guest.evaluate(`localStorage.getItem('fixtureRefresh') === '1' && localStorage.getItem('fixtureConnectorCount') === '1'`));
  }
  // 每次均从正式对话入口重入，必须观察到本轮刷新完成，不能被上一轮 complete 卡片蒙混通过。
  const repeats = Number(process.env.MAGI_SETUP_TEST_REPEAT || 0);
  if (!Number.isInteger(repeats) || repeats < 0 || repeats > 10) throw new Error('MAGI_SETUP_TEST_REPEAT 必须是 0–10 的整数');
  for (let round = 1; round <= repeats; round++) {
    const before = await guest.evaluate(`Number(localStorage.getItem('fixtureRefreshCount')||0)`);
    await tools.click('[data-web-model-setup] .setup-actions button');
    await startFromChat();
    await waitFor(() => guest.evaluate(`Number(localStorage.getItem('fixtureRefreshCount')||0) === ${before + 1}`), `重复配置第 ${round} 轮真实刷新`, 60000);
    await waitFor(() => tools.exists('[data-web-model-setup="complete"]'), `重复配置第 ${round} 轮完成回读`, 60000);
    check(`重复配置第 ${round} 轮完成且只刷新一次`, await guest.evaluate(`Number(localStorage.getItem('fixtureRefreshCount')) === ${before + 1}`));
    check(`重复配置第 ${round} 轮不重复创建连接器`, await guest.evaluate(`localStorage.getItem('fixtureConnectorCount') === '1' && localStorage.getItem('fixtureConnectCount') === '1'`));
    check(`重复配置第 ${round} 轮沿用安全凭据`, (await readFile(join(stateRoot, 'web-model/tunnel-api-key'), 'utf8')) === key && !await tools.exists('[data-web-model-setup-credentials]') && !await page.evaluate(`document.body.innerText.includes('sk-fixture-secret-123456')`));
  }
  await tools.click('[data-testid="sidebar-settings"]');
  await tools.click('[data-testid="settings-nav-webModel"]');
  const deactivated = await tools.post('/api/plugins/openai.chatgpt-web/deactivate', { scope: 'application' });
  check('停用 GPT Web 插件成功', deactivated.status === 200, deactivated.body);
  await page.evaluate(`window.dispatchEvent(new CustomEvent('magi-plugin-registry-changed'))`);
  await waitFor(() => page.evaluate(`!document.querySelector('[data-testid="settings-nav-webModel"]') && document.querySelector('[data-testid="settings-nav-plugins"]')?.classList.contains('active')`), '停用后退出插件专属设置');
  check('停用后管理页、配置入口和对话配置卡片同时消失', !await tools.exists('[data-web-model-setup-entry]') && !await tools.exists('[data-web-model-setup]'));
  await page.evaluate(`window.dispatchEvent(new CustomEvent('magi:webModelSettingsRequest', {detail:{focus:'tunnel'}}))`);
  await new Promise(resolve => setTimeout(resolve, 150));
  check('停用后深链接进入插件管理而不挂载配置页', !await tools.exists('[data-web-model-tunnel]') && await page.evaluate(`document.querySelector('[data-testid="settings-nav-plugins"]')?.classList.contains('active')`));
  check('停用后配置状态接口拒绝读取持久检查点', (await tools.api('/api/browser/web-models/setup?sessionId=absent')).status === 403);
  check('停用后配置命令从目录消失', !JSON.parse((await tools.api('/api/plugins/commands?scope=application')).body).some(item => item.id === 'plugin/openai.chatgpt-web/setup'));
  await tools.click('[data-testid="settings-back"]');
  await fillSetupDraft();
  check('停用后配置文字恢复普通输入且不显示残留卡片', await page.evaluate(`document.querySelector('[data-testid="input-send-button"]').disabled && document.querySelector('[data-testid="input-textarea"]').textContent==='帮我配置 GPT Web' && !document.querySelector('[data-web-model-setup]')`));
  await waitFor(() => tools.exists('webview').then(exists => !exists), '停用后释放 GPT Web guest', 15000);
  check('停用后物理 GPT Web guest 已释放', !await tools.exists('webview'));
  for (const action of ['disable', 'uninstall']) {
    const result = await tools.post('/api/plugins/openai.chatgpt-web/' + action, { scope: 'application' });
    check('插件 ' + action, result.status === 200, result.body);
  }
  const rejectedAfterUninstall = await tools.post('/api/session/turn', { scope: 'personal', text: 'start', pluginCommandId: 'plugin/openai.chatgpt-web/setup', requestId: 'uninstalled-setup-again' });
  const rejectedCredentialsAfterUninstall = await tools.post('/api/browser/web-models/setup/credentials', { sessionId: 'absent', tunnelId: 'tunnel_0123456789abcdef0123456789abcdef', apiKey: 'sk-fixture-secret-123456' });
  check('卸载后仍拒绝配置与凭据写入', rejectedAfterUninstall.status === 403 && rejectedCredentialsAfterUninstall.status === 403,
    `${rejectedAfterUninstall.status}/${rejectedCredentialsAfterUninstall.status}`);
  check('配置流程无运行时异常', errors.length === 0, errors.slice(-4).join('\n'));
  check('平台夹具无意外回包失败', fixtureErrors.length === 0, fixtureErrors.slice(-4).join('\n'));
  if (process.env.MAGI_SETUP_TEST_SCREENSHOT) await writeFile(process.env.MAGI_SETUP_TEST_SCREENSHOT, Buffer.from(await tools.screenshot(), 'base64'));
} catch (error) {
  await writeFile('/tmp/magi-auto-setup-full-debug.log', logs.join(''));
  if (guest) process.stderr.write('GUEST: '+String(await guest.evaluate('document.body.innerText').catch(() => ''))+'\n');
  if (errors.length) process.stderr.write(errors.slice(-5).join('\n') + '\n');
  if (page) process.stderr.write((await page.evaluate('document.body.innerText').catch(() => '')).slice(-4000) + '\n');
  process.stderr.write(logs.join('').slice(-5000) + '\n');
  throw error;
} finally {
  guest?.close(); page?.close();
  await stop(desktop); await stop(daemon);
  await rm(work, { recursive: true, force: true });
}
console.log(summary());
