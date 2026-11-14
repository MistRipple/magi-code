// 两版目录发行包的产品升级验收：先用旧版在线安装，再以新版打开同一隔离状态根。
// --seed 必须在替换旧目录包前运行；默认阶段验证同版原包的在线组件更新。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launchApp, connectAppPage, connectTarget, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const output = process.env.MAGI_PLUGIN_COMPONENTS_OUTPUT || join(root, 'target/plugin-acceptance/components');
const seed = process.argv.includes('--seed'), port = 38955, cdpPort = 9325, origin = `http://127.0.0.1:${port}`;
const source = { ecosystem: 'dsh', package: 'dsh-plugin-health', version: '3.0.5' };
await mkdir(output, { recursive: true });
const previous = seed ? null : JSON.parse(await readFile(join(output, 'seed.json'), 'utf8'));
const work = previous?.work || await mkdtemp(join(tmpdir(), 'magi-plugin-components-'));
const reporter = createReporter(), errors = [], logs = [];
const clipAudit = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE, 'utf8') : null;
let app, page, ui, frame;
async function api(path, body, method = body === undefined ? 'GET' : 'POST') {
  const response = await fetch(origin + path, { method, headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(240_000) });
  const value = await response.json(); assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`); return value;
}
const plugin = async () => (await api('/api/plugins')).plugins.find(p => p.source.location === `dsh:${source.package}@${source.version}`);
const screenshot = async name => writeFile(join(output, `${name}.png`), Buffer.from(await ui.screenshot(), 'base64'));
async function audit(name) {
  if (!clipAudit) return;
  const result = await page.evaluate(`(${clipAudit})({viewport:true,root:'.store-detail'})`);
  await writeFile(join(output, `${name}.json`), JSON.stringify(result, null, 2));
  reporter.check(`${name} 阴影无部分裁切`, result.cut === 0, result);
}
async function start() {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口空闲');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    env: { MAGI_STATE_ROOT: join(work, 'state'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  logs.push(app.log);
  await waitFor(() => fetch(origin + '/health').then(r => r.ok), 'daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'Electron', 60_000); ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台');
}
async function stop() {
  frame?.close(); frame = null; page?.close(); page = null;
  await app?.stop(); app = null;
}
async function running() {
  return waitFor(async () => { const current = await plugin(); if (current?.runtime?.state === 'failed') throw new Error(current.runtime.reason); return current?.runtime?.state === 'running' && current; }, '原包运行', 90_000);
}
async function openStore(id) {
  await ui.click('[data-testid=sidebar-settings]');
  await waitFor(() => ui.exists('[data-testid=settings-nav-plugins]'), '插件分类');
  await ui.click('[data-testid=settings-nav-plugins]');
  await waitFor(() => ui.exists(`[data-testid=plugin-row][data-plugin-id="${id}"]`), '已装插件');
  await ui.click(`[data-testid=plugin-row][data-plugin-id="${id}"]`);
}
try {
  await start();
  if (seed) {
    const preview = await api('/api/plugins/upstream/preview', source);
    await api('/api/plugins/upstream/install', { ...source, expectedDigest: preview.candidate.digest });
    const installed = await plugin(), id = installed.manifest.id;
    await api(`/api/plugins/${id}/enable`, {});
    await api(`/api/plugins/${id}/enable`, {}); await running();
    const settings = await api(`/api/plugins/${id}/settings`);
    const saved = await api(`/api/plugins/${id}/settings`, { expectedVersion: settings?.version ?? 0, value: settings?.value ?? {} }, 'PUT');
    await writeFile(join(output, 'seed.json'), JSON.stringify({ work, source, plugin: await plugin(), settings: saved }, null, 2));
    reporter.check('旧版发行包在线安装并运行真实 Health 原包，保存设置', true);
  } else {
    const before = await running(), id = before.manifest.id;
    if (!previous.settings) previous.settings = await api(`/api/plugins/${id}/settings`, { expectedVersion: 0, value: {} }, 'PUT');
    reporter.check('新版发现既有原包页面组件需要更新，保留旧摘要和运行', before.digest === previous.plugin.digest && before.translationRuntime?.state === 'updateAvailable');
    await openStore(id);
    await waitFor(() => ui.exists('[data-testid=plugin-detail-update]'), '在线组件更新入口');
    await audit('default');
    reporter.check('同版原包有固定在线更新入口，版本不显示虚构目标', (await ui.text('[data-testid=plugin-detail-update]')).includes('组件') && !(await ui.text('[data-testid=plugin-card]')).includes('vundefined'));
    await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
    const fixed = await page.evaluate(`(()=>{const scroll=document.querySelector('[data-testid=plugin-detail-scroll]'),button=document.querySelector('[data-testid=plugin-detail-update]');const stress=document.createElement('div');stress.style.height='2000px';scroll.append(stress);const a=button.getBoundingClientRect();scroll.scrollTop=scroll.scrollHeight;const b=button.getBoundingClientRect();stress.remove();return Math.abs(a.bottom-b.bottom)<1&&b.bottom<=innerHeight&&b.top>=0&&document.documentElement.scrollWidth<=innerWidth;})()`);
    reporter.check('390px 视口在线更新操作固定可见', fixed); await screenshot('update-390');
    await page.call('Emulation.clearDeviceMetricsOverride');
    const point = await page.evaluate(`(()=>{const r=document.querySelector('[data-testid=plugin-detail-update]').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`);
    await page.call('Input.dispatchMouseEvent', { type: 'mouseMoved', ...point }); await audit('hover');
    let paused;
    const cancelOff = page.on('Fetch.requestPaused', event => { paused = event; });
    await page.call('Fetch.enable', { patterns: [{ urlPattern: `${origin}/api/plugins/upstream/preview` }] });
    await ui.click('[data-testid=plugin-detail-update]'); await waitFor(() => paused, '更新检查进行中');
    reporter.check('在线检查有真实加载反馈和可取消操作', await ui.exists('[data-testid=plugin-operation-progress]') && await ui.exists('[data-testid=plugin-cancel-preview]'));
    await ui.click('[data-testid=plugin-cancel-preview]'); await page.call('Fetch.disable'); cancelOff();
    reporter.check('取消更新检查不改动当前摘要和运行实例', (await plugin()).digest === before.digest && (await plugin()).runtime.generation === before.runtime.generation);
    await page.call('Fetch.enable', { patterns: [{ urlPattern: `${origin}/api/plugins/upstream/preview` }] });
    const off = page.on('Fetch.requestPaused', event => page.call('Fetch.fulfillRequest', { requestId: event.requestId, responseCode: 503, responseHeaders: [{ name: 'content-type', value: 'application/json' }], body: Buffer.from(JSON.stringify({ message: 'COMPONENT_PREVIEW_UNAVAILABLE' })).toString('base64') }));
    await ui.click('[data-testid=plugin-detail-update]');
    await waitFor(async () => (await ui.text('[data-testid=plugin-message]'))?.includes('COMPONENT_PREVIEW_UNAVAILABLE'), '当前操作位置的失败提示');
    await page.call('Fetch.disable'); off();
    reporter.check('更新检查失败保留原包、设置及运行实例并可重试', (await plugin()).digest === before.digest && (await plugin()).runtime.generation === before.runtime.generation);
    await ui.click('[data-testid=plugin-detail-update]');
    const installed = await waitFor(async () => { const current = await plugin(); return current.digest !== before.digest && current.translationRuntime?.state === 'current' && current; }, '在线确认提交', 240_000);
    reporter.check('同版原包切换摘要后保持启用', installed.manifest.version === before.manifest.version && installed.enabled);
    const updated = await running();
    reporter.check('新组件的原包无需再次确认即恢复运行', updated.enabled && updated.runtime.state === 'running');
    reporter.check('运行组件更新保留原包来源和已保存设置', JSON.stringify(updated.source) === JSON.stringify(before.source) && JSON.stringify(await api(`/api/plugins/${id}/settings`)) === JSON.stringify(previous.settings));
    await waitFor(() => ui.exists('[data-testid=plugin-open-settings]'), '原包设置入口'); await ui.click('[data-testid=plugin-open-settings]');
    const selector = `iframe[data-plugin-id="${id}"]`;
    const url = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`), '新页面归档');
    frame = await waitFor(() => connectTarget(cdpPort, t => ['iframe', 'page'].includes(t.type) && t.url === url, 'Health 原页面', errors), '原页面实例');
    await waitFor(() => frame.evaluate('document.body.innerText.length>30'), '原包页面渲染');
    reporter.check('更新后加载新摘要的原页面，保留原生 React 内容', url.includes(updated.digest)); await screenshot('updated-health');
    const route = await api(`/api/plugins/${id}/routes`, { method: 'GET', path: '/plugins/dsh-plugin-health/status' });
    reporter.check('真实 Health 后端业务返回成功结果', route.status === 200 && !!route.body);
    await stop(); await start(); const restarted = await running();
    reporter.check('重启后组件状态、安装摘要和设置仍一致', restarted.digest === updated.digest && restarted.translationRuntime?.state === 'current' && JSON.stringify(await api(`/api/plugins/${id}/settings`)) === JSON.stringify(previous.settings));
    reporter.check('本轮没有页面运行异常', errors.length === 0, errors);
  }
} catch (error) {
  reporter.results.push({ name: '组件在线更新验收完成', ok: false, detail: String(error.stack || error) });
  process.stderr.write(`${error.stack || error}\n`);
  if (ui) await screenshot('failure').catch(() => {});
} finally {
  await stop();
  await writeFile(join(output, seed ? 'seed.log' : 'electron.log'), logs.flat().join(''));
  await writeFile(join(output, seed ? 'seed-results.json' : 'results.json'), JSON.stringify({ work, errors, checks: reporter.results }, null, 2));
  if (reporter.results.some(check => !check.ok)) process.exitCode = 1;
}
