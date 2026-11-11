import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { promisify } from 'node:util';
import { connectTarget, pageTools, waitFor } from './electron-harness.mjs';

const root = dirname(dirname(dirname(fileURLToPath(import.meta.url))));
const execFileAsync = promisify(execFile);

// 专项和社区共用真实截图、原生进程、输入投递及持久化恢复；未启用原生截图时不算核心业务通过。
export async function verifyDshAppshot({ request: api, origin, plugin, page, cdpPort, errors, output, stateRoot, restart, check, liveCapture = false }) {
  assert.equal(plugin.manifest.version, '0.4.2');
  const id = plugin.manifest.id, work = dirname(stateRoot), clientLogs = [];
  const session = (await api('/api/session/materialize', { scope: 'personal' })).sessionId;
  const evidence = { package: 'dsh-plugin-appshot@0.4.2', digest: plugin.digest, work, checks: [], core: 'unverified', completed: false };
  let frame, capturedImage;
  async function nativeProcesses() {
    const { stdout } = await execFileAsync('ps', ['-ax', '-o', 'pid=,ppid=,command=']);
    return stdout.split('\n').filter(line => line.includes(work.replace(/^\/var\//, '/private/var/')) || line.includes(work))
      .filter(line => line.includes('/Contents/MacOS/appshot-macos')).map(line => line.trim());
  }
  const pluginPath = () => `/api/plugins/${encodeURIComponent(id)}`;
  async function config() {
    const result = await api(pluginPath() + '/routes', { method: 'GET', path: '/plugins/appshot/config' });
    assert.equal(result.status, 200);
    return result.body;
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
    frame = await waitFor(() => connectTarget(cdpPort, t => ['iframe', 'page'].includes(t.type) && t.url === url, '原包设置页', errors), '原包设置页');
    frame.on('Runtime.consoleAPICalled', event => clientLogs.push({ type:event.type, args:event.args.map(arg=>arg.value ?? arg.preview ?? arg.description) }));
    await waitFor(() => frame.evaluate(`document.querySelectorAll('input[type=checkbox]').length === 2 && !document.querySelector('select')?.disabled`), '设置读取');
  }
  async function verify(name, fn) { const value = await fn(); check(name, true, value); evidence.checks.push({ name, ok: true, value }); }
  try {
    await running();
    await verify('原包原生辅助进程启动', () => waitFor(async () => { const processes = await nativeProcesses(); return processes.length === 1 && processes; }, 'Appshot 原生辅助进程'));
    await openSettings();
    await verify('原包页面保存声音开关', async () => {
      await frame.evaluate(`document.querySelector('input[type=checkbox]').click()`);
      await waitFor(async () => (await config()).soundEnabled === false, '原包保存');
      await waitFor(() => frame.evaluate(`!document.querySelector('select')?.disabled`), '保存结束');
      return config();
    });
    const marker = await frame.evaluate(`window.__acceptanceIdentity = crypto.randomUUID()`);
    await pageTools(page).click('[data-testid=settings-back]');
    await verify('关闭设置页后原包客户端文档仍常驻且不重新加载', async () => {
      await waitFor(() => pageTools(page).exists('iframe[data-testid=plugin-client-background]'), '后台客户端');
      assert.equal(await frame.evaluate(`window.__acceptanceIdentity`), marker);
      return true;
    });
    await verify('重复确认同一原包保持原生进程、常驻客户端和配置', async () => {
      const processes = await nativeProcesses();
      assert.equal(processes.length, 1);
      const before = await running();
      const same = { ecosystem: 'dsh', package: 'dsh-plugin-appshot', version: '0.4.2' };
      const repeated = await api('/api/plugins/upstream/preview', same);
      assert.equal(repeated.candidate.operation, 'unchanged');
      assert.equal(repeated.candidate.digest, before.digest);
      const result = await api('/api/plugins/upstream/install', { ...same, expectedDigest: repeated.candidate.digest });
      const current = result.plugins.find(p => p.manifest.id === id);
      assert.equal(current.active, true);
      assert.deepEqual(current.grants, before.grants);
      assert.equal((await config()).soundEnabled, false);
      // 客户端此时停放在后台；通过真实配置调用后核对进程和文档身份，
      // 不等待被隐藏 iframe 正常节流的动画帧。
      assert.equal(await frame.evaluate('window.__acceptanceIdentity'), marker);
      assert.deepEqual(await nativeProcesses(), processes);
      return { unchanged: true, processes };
    });
    await verify('旧接口原包预览明确说明输入操作缺口，保留当前版本及截图进程', async () => {
      const before = await running(), processes = await nativeProcesses();
      const response = await pageTools(page).post('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: 'dsh-plugin-appshot', version: '0.4.1' });
      assert.notEqual(response.status, 200); const error = JSON.parse(response.body);
      assert.match(error.message, /未支持.*输入操作.*addImages/); assert.match(error.message, /在线更新插件/);
      const after = await running();
      assert.equal(after.digest, before.digest); assert.deepEqual(after.grants, before.grants);
      assert.deepEqual(await nativeProcesses(), processes); assert.equal((await config()).soundEnabled, false);
      assert.equal(await frame.evaluate('window.__acceptanceIdentity'), marker);
      return { version: after.manifest.version, error };
    });
    await pageTools(page).click('[data-testid=sidebar-settings]');
    const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${id}:settings`)}]`;
    await waitFor(() => pageTools(page).exists(nav), '再次打开设置入口');
    await pageTools(page).click(nav);
    await verify('重新打开设置复用同一客户端且原包界面正常渲染', async () => {
      await waitFor(() => frame.evaluate(`document.querySelectorAll('input[type=checkbox]').length === 2`), '重新渲染原包界面');
      assert.equal(await frame.evaluate(`window.__acceptanceIdentity`), marker);
      assert.equal(await page.evaluate(`document.querySelectorAll('iframe[data-plugin-id=${JSON.stringify(id)}]').length`), 1);
      return true;
    });
    await pageTools(page).click('[data-testid=settings-back]');
    if (liveCapture) {
      await page.call('Page.bringToFront');
      const fixture = join(work, 'magi-appshot-capture-fixture');
      await execFileAsync('swiftc', [join(root, 'scripts/lib/appshot-capture-fixture.swift'), '-o', fixture], { timeout: 30_000 });
      await execFileAsync(fixture, [], { timeout: 15_000 });
      await verify('原生截图经原包后台事件和持久附件到达当前输入框', async () => {
        await waitFor(() => page.evaluate(`Boolean(document.querySelector('.image-tray img')?.naturalWidth)`), '真实截图投递', 60_000);
        assert.equal(await frame.evaluate('window.__acceptanceIdentity'), marker);
        const dataUrl = await page.evaluate(`document.querySelector('.image-tray img').src`);
        const data = Buffer.from(dataUrl.split(',')[1], 'base64');
        const sha256 = createHash('sha256').update(data).digest('hex');
        const state = JSON.parse(await readFile(join(stateRoot, 'plugins/resources.json'), 'utf8'));
        const stored = Object.entries(state.values).find(([key, resource]) => key.startsWith(id+'\x1f__image:') && resource.value.sha256 === sha256)?.[1]?.value;
        assert.ok(stored, '输入框图片须来自原包在 daemon 提交的持久附件');
        assert.deepEqual(await readFile(join(stateRoot, 'plugins/images', sha256)), data);
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
    await verify('关闭页面后配置仍在', async () => { assert.equal((await config()).soundEnabled, false); return config(); });
    await api(pluginPath() + '/deactivate', {});
    await verify('停用回收原生辅助进程', () => waitFor(async () => !(await nativeProcesses()).length, '原生辅助进程回收'));
    await api(pluginPath() + '/activate', {}); await running();
    await verify('停用再激活恢复配置', async () => { assert.equal((await config()).soundEnabled, false); return config(); });
    const oldProcesses = await nativeProcesses();
    page = await restart(); await running();
    await verify('完整应用重启回收旧辅助进程并恢复唯一新实例', () => waitFor(async () => {
      const processes = await nativeProcesses();
      return processes.length === 1 && oldProcesses.every(old => !processes.some(value => value.split(/\s+/)[0] === old.split(/\s+/)[0])) && processes;
    }, '重启后原生辅助进程'));
    await verify('应用重启恢复配置', async () => { assert.equal((await config()).soundEnabled, false); return config(); });
    if (capturedImage) await verify('应用重启后原包截图元数据与持久载荷完整保留', async () => {
      const state = JSON.parse(await readFile(join(stateRoot, 'plugins/resources.json'), 'utf8'));
      assert.deepEqual(state.values[id+'\x1f__image:'+capturedImage.metadata.attachmentId]?.value, capturedImage.metadata);
      assert.deepEqual(await readFile(join(stateRoot, 'plugins/images', capturedImage.metadata.sha256)), capturedImage.data);
      return { attachmentId: capturedImage.metadata.attachmentId, sha256: capturedImage.metadata.sha256 };
    });
    await openSettings();
    await verify('原包页面恢复持久开关', async () => { assert.equal(await frame.evaluate(`document.querySelector('input[type=checkbox]').checked`), false); return true; });
    await writeFile(join(output, 'settings-restored.png'), Buffer.from(await pageTools(page).screenshot(), 'base64'));
    assert.deepEqual(errors, [], '页面运行错误');
    evidence.completed = true;
    return { page, checks: { ...Object.fromEntries(evidence.checks.map(value => [value.name, true])), liveCapture: capturedImage ? true : 'unverified' }, evidence };
  } catch (error) {
    evidence.error = error.stack ?? String(error); throw error;
  } finally {
    evidence.rendererErrors = [...errors]; evidence.clientLogs = clientLogs;
    frame?.close();
    await writeFile(join(output, 'appshot-business.json'), JSON.stringify(evidence, null, 2));
  }
}
