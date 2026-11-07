import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { launchApp, connectAppPage, connectTarget, pageTools, waitFor } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_APPSHOT_PORT || 38944), cdpPort = Number(process.env.MAGI_APPSHOT_CDP_PORT || 9314);
const origin = `http://127.0.0.1:${port}`, work = await mkdtemp(join(tmpdir(), 'magi-appshot-persistence-'));
const output = join(root, 'target/plugin-acceptance/appshot');
await mkdir(output, { recursive: true });
const errors = [], evidence = { package: 'dsh-plugin-appshot@0.4.2', work, checks: [], core: 'unverified' };
const execFileAsync = promisify(execFile), logs = [];
let app, page, frame, id, session;
async function nativeProcesses() {
  const { stdout } = await execFileAsync('ps', ['-ax', '-o', 'pid=,ppid=,command=']);
  return stdout.split('\n').filter(line => line.includes(work.replace(/^\/var\//, '/private/var/')) || line.includes(work))
    .filter(line => line.includes('/Contents/MacOS/appshot-macos')).map(line => line.trim());
}
async function stop() {
  if (!app) return;
  await app.stop(); logs.push(...app.log); app = null;
}
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(240_000) });
  const value = await response.json();
  assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`);
  return value;
}
const pluginPath = () => `/api/plugins/${encodeURIComponent(id)}`;
async function config() {
  const result = await api(pluginPath() + '/routes', { method: 'GET', path: '/plugins/appshot/config' });
  assert.equal(result.status, 200);
  return result.body;
}
async function start() {
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  await waitFor(async () => (await api('/health')).status === 'ok', 'daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'App Renderer');
}
async function running() {
  return waitFor(async () => {
    const plugin = (await api('/api/plugins')).plugins.find(p => p.manifest.id === id);
    if (plugin?.runtime?.state === 'failed') throw new Error(plugin.runtime.reason);
    return plugin?.runtime?.state === 'running' && plugin;
  }, 'Appshot 激活', 60_000);
}
async function openSettings() {
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}` });
  const ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台');
  await ui.click('[data-testid=sidebar-settings]');
  const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${id}:settings`)}]`;
  await waitFor(() => ui.exists(nav), '原包设置入口'); await ui.click(nav);
  const selector = `iframe[data-plugin-id=${JSON.stringify(id)}]`;
  await waitFor(() => ui.exists(selector), '原包 iframe');
  const url = await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).src`);
  frame = await waitFor(() => connectTarget(cdpPort, t => t.type === 'iframe' && t.url === url, '原包设置页', errors), '原包设置页');
  await waitFor(() => frame.evaluate(`document.querySelectorAll('input[type=checkbox]').length === 2 && !document.querySelector('select')?.disabled`), '设置读取');
}
async function check(name, fn) { const value = await fn(); evidence.checks.push({ name, ok: true, value }); console.log(`PASS ${name}`); }
try {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口被占用');
  await start();
  session = (await api('/api/session/materialize', { scope: 'personal' })).sessionId;
  const preview = await api('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: 'dsh-plugin-appshot', version: '0.4.2' });
  const installed = await api('/api/plugins/upstream/install', { ecosystem: 'dsh', package: 'dsh-plugin-appshot', version: '0.4.2', expectedDigest: preview.candidate.digest });
  const plugin = installed.plugins.find(p => p.source?.location === 'dsh:dsh-plugin-appshot@0.4.2'); id = plugin.manifest.id;
  await api(pluginPath() + '/enable', {}); await api(pluginPath() + '/authorize', { grants: plugin.manifest.permissions }); await api(pluginPath() + '/activate', {}); await running();
  await check('原包原生辅助进程启动', () => waitFor(async () => { const processes = await nativeProcesses(); return processes.length === 1 && processes; }, 'Appshot 原生辅助进程'));
  await openSettings();
  await check('原包页面保存声音开关', async () => {
    await frame.evaluate(`document.querySelector('input[type=checkbox]').click()`);
    await waitFor(async () => (await config()).soundEnabled === false, '原包保存');
    await waitFor(() => frame.evaluate(`!document.querySelector('select')?.disabled`), '保存结束');
    return config();
  });
  frame.close(); frame = null;
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}` });
  await check('关闭页面后配置仍在', async () => { assert.equal((await config()).soundEnabled, false); return config(); });
  await api(pluginPath() + '/deactivate', {});
  await check('停用回收原生辅助进程', () => waitFor(async () => !(await nativeProcesses()).length, '原生辅助进程回收'));
  await api(pluginPath() + '/activate', {}); await running();
  await check('停用再激活恢复配置', async () => { assert.equal((await config()).soundEnabled, false); return config(); });
  page.close(); page = null; await stop();
  await check('退出应用回收原生辅助进程', () => waitFor(async () => !(await nativeProcesses()).length, '退出回收辅助进程'));
  await start();
  await api(pluginPath() + '/activate', {}); await running();
  await check('重启后恢复原生辅助进程', () => waitFor(async () => { const processes = await nativeProcesses(); return processes.length === 1 && processes; }, '重启后辅助进程'));
  await check('应用重启恢复配置', async () => { assert.equal((await config()).soundEnabled, false); return config(); });
  await openSettings();
  await check('原包页面恢复持久开关', async () => { assert.equal(await frame.evaluate(`document.querySelector('input[type=checkbox]').checked`), false); return true; });
  await writeFile(join(output, 'settings-restored.png'), Buffer.from(await pageTools(page).screenshot(), 'base64'));
  assert.deepEqual(errors, [], '页面运行错误');
} catch (error) { evidence.error = error.stack; console.error(error); process.exitCode = 1; }
finally {
  evidence.rendererErrors = errors;
  frame?.close(); page?.close(); await stop();
  await writeFile(join(output, 'electron.log'), logs.join(''));
  await writeFile(join(output, 'results.json'), JSON.stringify(evidence, null, 2));
}
