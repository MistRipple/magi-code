import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { promisify } from 'node:util';
import { launchApp, connectAppPage, connectTarget, pageTools, waitFor } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_APPSHOT_PORT || 38944), cdpPort = Number(process.env.MAGI_APPSHOT_CDP_PORT || 9314);
const origin = `http://127.0.0.1:${port}`, work = await mkdtemp(join(tmpdir(), 'magi-appshot-persistence-'));
const output = join(root, 'target/plugin-acceptance/appshot');
await mkdir(output, { recursive: true });
const clientLogs = [];
const errors = [], evidence = { package: 'dsh-plugin-appshot@0.4.2', work, checks: [], core: 'unverified' };
const execFileAsync = promisify(execFile), logs = [];
let app, page, frame, id, session, capturedImage;
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
  frame.on('Runtime.consoleAPICalled', event => clientLogs.push({ type:event.type, args:event.args.map(arg=>arg.value ?? arg.preview ?? arg.description) }));
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
  const marker = await frame.evaluate(`window.__acceptanceIdentity = crypto.randomUUID()`);
  await pageTools(page).click('[data-testid=settings-back]');
  await check('关闭设置页后原包客户端文档仍常驻且不重新加载', async () => {
    await waitFor(() => pageTools(page).exists('iframe[data-testid=plugin-client-background]'), '后台客户端');
    assert.equal(await frame.evaluate(`window.__acceptanceIdentity`), marker);
    return true;
  });
  await pageTools(page).click('[data-testid=sidebar-settings]');
  const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${id}:settings`)}]`;
  await waitFor(() => pageTools(page).exists(nav), '再次打开设置入口');
  await pageTools(page).click(nav);
  await check('重新打开设置复用同一客户端且原包界面正常渲染', async () => {
    await waitFor(() => frame.evaluate(`document.querySelectorAll('input[type=checkbox]').length === 2`), '重新渲染原包界面');
    assert.equal(await frame.evaluate(`window.__acceptanceIdentity`), marker);
    assert.equal(await page.evaluate(`document.querySelectorAll('iframe[data-plugin-id=${JSON.stringify(id)}]').length`), 1);
    return true;
  });
  await pageTools(page).click('[data-testid=settings-back]');
  if (process.env.MAGI_APPSHOT_LIVE_CAPTURE === '1') {
    await page.call('Page.bringToFront');
    await writeFile(join(output, 'live-ready.json'), JSON.stringify({ work, origin, cdpPort, id, session }));
    console.log('READY_NATIVE_CAPTURE');
    const fixture = join(work, 'magi-appshot-capture-fixture');
    await execFileAsync('swiftc', [join(root, 'scripts/lib/appshot-capture-fixture.swift'), '-o', fixture], { timeout: 30_000 });
    await execFileAsync(fixture, [], { timeout: 15_000 });
    await check('原生截图经原包后台事件和持久附件到达当前输入框', async () => {
      await waitFor(() => page.evaluate(`Boolean(document.querySelector('.image-tray img')?.naturalWidth)`), '真实截图投递', 60_000);
      assert.equal(await frame.evaluate('window.__acceptanceIdentity'), marker);
      const dataUrl = await page.evaluate(`document.querySelector('.image-tray img').src`);
      const data = Buffer.from(dataUrl.split(',')[1], 'base64');
      const sha256 = createHash('sha256').update(data).digest('hex');
      const state = JSON.parse(await readFile(join(work, 'state/plugins/resources.json'), 'utf8'));
      const stored = Object.entries(state.values).find(([key, resource]) => key.startsWith(id+'\x1f__image:') && resource.value.sha256 === sha256)?.[1]?.value;
      assert.ok(stored, '输入框图片须来自原包在 daemon 提交的持久附件');
      assert.deepEqual(await readFile(join(work, 'state/plugins/images', sha256)), data);
      const image = await page.evaluate(`(()=>{const img=document.querySelector('.image-tray img');return {name:img.alt,width:img.naturalWidth,height:img.naturalHeight,bytes:atob(img.src.split(',')[1]).length};})()`);
      image.attachmentId = stored.attachmentId; image.sha256 = sha256;
      capturedImage = { metadata: stored, data };
      assert.ok(image.bytes > 1000 && image.width > 100 && image.height > 100);
      await writeFile(join(output, 'capture-mounted.png'), Buffer.from(await pageTools(page).screenshot(), 'base64'));
      return image;
    });
    evidence.core = 'capture-mounted';
  }
  frame.close(); frame = null;
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
  if (capturedImage) await check('应用重启后原包截图元数据与持久载荷完整保留', async () => {
    const state = JSON.parse(await readFile(join(work, 'state/plugins/resources.json'), 'utf8'));
    assert.deepEqual(state.values[id+'\x1f__image:'+capturedImage.metadata.attachmentId]?.value, capturedImage.metadata);
    assert.deepEqual(await readFile(join(work, 'state/plugins/images', capturedImage.metadata.sha256)), capturedImage.data);
    return { attachmentId: capturedImage.metadata.attachmentId, sha256: capturedImage.metadata.sha256 };
  });
  await openSettings();
  await check('原包页面恢复持久开关', async () => { assert.equal(await frame.evaluate(`document.querySelector('input[type=checkbox]').checked`), false); return true; });
  await writeFile(join(output, 'settings-restored.png'), Buffer.from(await pageTools(page).screenshot(), 'base64'));
  assert.deepEqual(errors, [], '页面运行错误');
} catch (error) { evidence.error = error.stack; console.error(error); process.exitCode = 1; }
finally {
  evidence.rendererErrors = errors;
  evidence.clientLogs = clientLogs;
  frame?.close(); page?.close(); await stop();
  await writeFile(join(output, 'electron.log'), logs.join(''));
  await writeFile(join(output, 'results.json'), JSON.stringify(evidence, null, 2));
}
