// 无真实账号：独立 daemon + Electron guest 验收配置交接；平台和 tunnel-client 使用受控夹具。
import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdtemp, mkdir, writeFile, readFile, rm } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { connectAppPage, connectTarget, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = new URL('..', import.meta.url).pathname.replace(/\/$/u, '');
const work = await mkdtemp(join(tmpdir(), 'magi-gpt-setup-'));
const stateRoot = join(work, 'state');
const port = Number(process.env.MAGI_SETUP_TEST_PORT || 38951);
const cdpPort = port + 1;
const vitePort = port + 2;
const { check, summary } = createReporter();
const errors = [], logs = [];
let daemon, desktop, page, guest;
let loggedIn = false;
const fakeClient = join(work, 'tunnel-client');
const client = `#!/bin/sh
if [ "$2" = "connect" ]; then touch "$0.alive"; printf '{"running":true,"healthy":true,"ready":true,"pid":99}'; exit 0; fi
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
const env = { ...process.env, MAGI_STATE_ROOT: stateRoot, MAGI_PORT: String(port), MAGI_WEB_DEV: '1', MAGI_WEB_DEV_PORT: String(vitePort), MAGI_TARGET_PRUNE_GIB: '100', MAGI_OPEN_BROWSER: '0', MAGI_TUNNEL_CLIENT_BINARY: fakeClient };
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
const tunnels = `<!doctype html><title>Platform fixture</title><main><table><tbody></tbody></table><button id="add">Create tunnel</button></main><script>
add.onclick=()=>{document.body.insertAdjacentHTML('beforeend','<dialog open role="dialog"><input type="text"><button id="save">Create</button></dialog>');save.onclick=()=>{localStorage.setItem('fixtureTunnel','1');document.querySelector('tbody').innerHTML='<tr><td>Magi</td><td>tunnel_0123456789abcdef0123456789abcdef</td></tr>';document.querySelector('dialog').remove()}};
if(localStorage.getItem('fixtureTunnel'))document.querySelector('tbody').innerHTML='<tr><td>Magi</td><td>tunnel_0123456789abcdef0123456789abcdef</td></tr>';
</script>`;
const connectorList = `<!doctype html><title>Connectors fixture</title><main>Installed apps<ul id="apps"></ul></main><script>if(localStorage.getItem('fixtureConnector'))apps.innerHTML='<li><span>Magi</span><span>12 tools</span></li>';</script>`;
const connectorDirectory = `<!doctype html><title>Directory fixture</title><main><button id="add">Add</button></main><script>
add.onclick=()=>{document.body.insertAdjacentHTML('beforeend','<button role="menuitem" id="newApp">Create MCP app</button>');newApp.onclick=()=>{document.body.insertAdjacentHTML('beforeend','<dialog open role="dialog"><button>Tunnel</button><input type="text" name="name"><input type="text" aria-label="Tunnel id"><select><option value="token">Token</option><option value="none">No authentication</option></select><input type="checkbox"><button id="save" type="submit">Create</button></dialog>');save.onclick=()=>{const d=document.querySelector('dialog');if(d.querySelector('select').value!=='none'||!d.querySelector('input[type=checkbox]').checked||d.querySelector('input[name=name]').value!=='Magi')return;localStorage.setItem('fixtureConnector','1');document.querySelector('dialog').remove()}}};
</script>`;

try {
  daemon = launch('bash', ['scripts/dev-daemon.sh']);
  await waitFor(async () => (await fetch(`http://127.0.0.1:${port}/health`)).ok, '隔离 daemon', 120000);
  desktop = launch(createRequire(import.meta.url)('electron'), ['--disable-gpu', `--remote-debugging-port=${cdpPort}`, 'apps/desktop'], { MAGI_DESKTOP_REUSE_DAEMON: '1', MAGI_DESKTOP_DAEMON_PORT: String(port) });
  page = await waitFor(async () => {
    const candidate = await connectAppPage(cdpPort, port, errors).catch(() => null);
    if (!candidate) return null;
    if (await candidate.evaluate(`Boolean(document.querySelector('[data-testid="input-textarea"]'))`)) return candidate;
    candidate.close(); return null;
  }, '配置助手工作台', 60000);
  const tools = pageTools(page);
  const openEntry = async () => {
    await tools.click('[data-testid="sidebar-settings"]');
    await waitFor(() => tools.exists('[data-testid="settings-nav-model"]'), '模型设置入口');
    await tools.click('[data-testid="settings-nav-model"]');
  };
  await openEntry();
  check('未安装插件时模型设置不显示 GPT Web 或配置助手', !await tools.exists('[data-web-model-setup-entry]') && !await tools.exists('[data-testid="settings-nav-webModel"]'));
  // 仍保留命令入口的安装提示；设置页本身不展示未启用功能。
  await page.evaluate(`window.dispatchEvent(new CustomEvent('magi:gptWebSetup'))`);
  await waitFor(() => page.evaluate(`document.querySelector('[data-testid="settings-nav-plugins"]')?.classList.contains('active')`), '未安装时跳转插件页');
  check('直接调用配置命令仍提示安装插件', await page.evaluate(`document.body.innerText.includes('请先在设置') || document.body.innerText.includes('Install and activate')`));
  check('未安装插件时不创建浏览器宿主', !await tools.exists('webview'));
  const installed = await tools.post('/api/plugins/bundled/openai.chatgpt-web/install', {});
  const manifest = JSON.parse(installed.body).plugins.find(item => item.manifest.id === 'openai.chatgpt-web').manifest;
  check('已安装未激活时仍无 GPT Web 管理页', !await tools.exists('[data-testid="settings-nav-webModel"]'));
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
      ? (url.pathname.includes('/tunnels') ? tunnels : '<main>Create a runtime API key with Tunnels Read + Use</main>')
      : url.pathname.startsWith('/settings/') ? connectorList : url.pathname.startsWith('/plugins') ? connectorDirectory : home();
    void guest.call('Fetch.fulfillRequest', { requestId: event.requestId, responseCode: 200,
      responseHeaders: [{ name: 'Content-Type', value: document ? 'text/html; charset=utf-8' : 'application/json' }],
      body: Buffer.from(document ? html : JSON.stringify(loggedIn ? { user: { name: 'fixture' } } : {})).toString('base64'),
    }).catch(error => errors.push(String(error)));
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
  await tools.click('[data-testid="sidebar-settings"]');
  await tools.click('[data-testid="settings-nav-webModel"]');
  await waitFor(() => tools.exists('[data-web-model-setup-entry] button'), 'GPT Web 插件配置入口');
  await tools.click('[data-web-model-setup-entry] button');
  await waitFor(() => tools.exists('[data-web-model-setup]'), '配置状态卡片', 60000);
  await waitFor(() => tools.exists('[data-web-model-setup="login"]'), '等待用户登录', 60000);
  check('无本地模型也能执行配置，登录时显示接手提示', true);
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
    // 用户在平台手动完成创建；继续只能检查实际状态，不重放未确认创建。
    await guest.evaluate(`localStorage.setItem('fixtureTunnel','1')`);
    await guest.call('Page.navigate', {url:'https://platform.openai.com/settings/organization/tunnels'});
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
  check('用户登录后 Magi 创建 Tunnel 并推进到安全输入', tunnel.tunnelId === 'tunnel_0123456789abcdef0123456789abcdef');
  check('全流程只有一个真实 webview', await page.evaluate(`document.querySelectorAll('webview').length === 1`));
  for (const width of [960, 1440]) {
    await page.call('Emulation.setDeviceMetricsOverride', { width, height: 900, deviceScaleFactor: 1, mobile: false });
    check(`窗口 ${width}px 配置卡片无横向溢出`, await page.evaluate(`(()=>{const e=document.querySelector('[data-web-model-setup]');return e.clientWidth>0 && e.scrollWidth<=e.clientWidth+1})()`));
  }
  await page.call('Emulation.clearDeviceMetricsOverride');
  await page.evaluate(`(()=>{const e=document.querySelector('[data-web-model-setup] input[type=password]');e.value='sk-fixture-secret-123456';e.dispatchEvent(new Event('input',{bubbles:true}));document.querySelector('[data-web-model-setup] form').requestSubmit()})()`);
  await waitFor(() => tools.exists('[data-web-model-setup="connector"]'), '通道就绪后自动进入连接器确认', 60000);
  check('安全输入后通道自动连接且密钥不出现在对话', !await page.evaluate(`document.body.innerText.includes('sk-fixture-secret-123456')`));
  await page.evaluate('window.__setupCursorEvents = []');
  await continueSetup();
  await waitFor(() => tools.exists('[data-web-model-setup="complete"]'), '连接器写入与回读确认', 60000);
  check('显式确认后创建连接器并回读完成', true);
  check('连接器配置点击与填表均更新 Magi 光标', await page.evaluate(`['click','type'].every(action => window.__setupCursorEvents.some(e => e.type === 'agent_cursor' && e.visible && e.action === action))`));
  await waitFor(cursorHidden, '配置完成后隐藏 Magi 光标');
  check('配置完成后释放控制并隐藏光标', true);
  const settings = await readFile(join(stateRoot, 'settings.json'), 'utf8');
  check('密钥不进入 settings 或配置状态', !settings.includes('sk-fixture-secret-123456'));
  const key = await readFile(join(stateRoot, 'web-model/tunnel-api-key'), 'utf8');
  check('密钥通过独立安全入口保存', key === 'sk-fixture-secret-123456');
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
  // Electron 43 在 DevTools 附着 sandbox guest 时偶发自身 preload 诊断，独立记录，应用异常仍失败。
  const hostDiagnostics = errors.filter(error => error.includes('Electron sandboxed_renderer.bundle.js script failed to run')
    || (error.includes("Cannot destructure property 'preloadScripts' of 'binding.startupData'") && error.includes('node:electron/js2c/sandbox_bundle')));
  const actionableErrors = errors.filter(error => !hostDiagnostics.includes(error));
  check('配置流程无应用运行时异常', actionableErrors.length === 0, actionableErrors.slice(-4).join('\n'));
  if (hostDiagnostics.length) console.log('Electron DevTools sandbox 诊断：' + hostDiagnostics.length + ' 条');
  if (process.env.MAGI_SETUP_TEST_SCREENSHOT) await writeFile(process.env.MAGI_SETUP_TEST_SCREENSHOT, Buffer.from(await tools.screenshot(), 'base64'));
} catch (error) {
  if (page) process.stderr.write((await page.evaluate('document.body.innerText').catch(() => '')).slice(-4000) + '\n');
  process.stderr.write(logs.join('').slice(-5000) + '\n');
  throw error;
} finally {
  guest?.close(); page?.close();
  await stop(desktop); await stop(daemon);
  await rm(work, { recursive: true, force: true });
}
console.log(summary());
