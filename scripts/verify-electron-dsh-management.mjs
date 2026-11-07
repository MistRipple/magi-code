// 真实目录包 + npm 原包。Share 的页面与业务不修改；管理服务夹具只验证
// 无上游升级页面覆盖的 RPC/授权/摘要/卸载机制，不计入生态兼容率。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { packPlugin } from './compat/translate/pack.mjs';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_DSH_MANAGEMENT_PORT || 38944), cdpPort = Number(process.env.MAGI_DSH_MANAGEMENT_CDP_PORT || 9314);
const origin = `http://127.0.0.1:${port}`;
const output = join(root, 'target/plugin-acceptance/dsh-management');
const work = await mkdtemp(join(tmpdir(), 'magi-dsh-management-'));
const report = createReporter(), errors = [];
let app, page, frame, ui;
await mkdir(output, { recursive: true });
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(300_000) });
  const value = await response.json();
  assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`);
  return value;
}
const plugins = async () => (await api('/api/plugins')).plugins;
const targetPackage = 'dsh-plugin-release-probe';
const target = async () => (await plugins()).find((plugin) => plugin.source.location.startsWith(`dsh:${targetPackage}@`));
async function authorize(plugin) {
  await api(`/api/plugins/${plugin.manifest.id}/enable`, {});
  await api(`/api/plugins/${plugin.manifest.id}/authorize`, { grants: plugin.manifest.permissions });
  await api(`/api/plugins/${plugin.manifest.id}/activate`, {});
  await waitFor(async () => (await plugins()).find((p) => p.manifest.id === plugin.manifest.id)?.runtime?.state === 'running', '原包运行', 120_000);
}
async function installUpstream(name, version) {
  const input = { ecosystem: 'dsh', package: name, version };
  const preview = await api('/api/plugins/upstream/preview', input);
  const list = await api('/api/plugins/upstream/install', { ...input, expectedDigest: preview.candidate.digest });
  return list.plugins.find((plugin) => plugin.source.location === `dsh:${name}@${version}`);
}
async function callRoute(id, path, body) {
  return api(`/api/plugins/${id}/routes`, { method: 'POST', path, body });
}
async function screenshot(name) { await writeFile(join(output, name + '.png'), Buffer.from(await ui.screenshot(), 'base64')); }
async function installFixture() {
  const dir = join(work, 'fixture'); await mkdir(join(dir, 'extension'), { recursive: true });
  const manifest = { sdkVersion: 2, id: 'magi.management-test', name: 'Management acceptance', version: '1.0.0', description: '', dataSchemaVersion: 1, extension: { hooks: [] },
    permissions: [{ kind: 'extension', targets: ['full-trust'] }, { kind: 'pluginManagement', targets: ['install', 'lifecycle'] }], settingsSchema: { type: 'object' }, contributions: { commands: [{ id: 'status', title: 'Status', description: '' }], views: [{ id: 'management', title: 'Management', entry: 'ui/settings.html', placements: ['settings'] }] } };
  await writeFile(join(dir, 'manifest.json'), JSON.stringify(manifest));
  await mkdir(join(dir, 'ui'), { recursive: true }); await writeFile(join(dir, 'ui/settings.html'), '<!doctype html><title>Management</title>');
  await writeFile(join(dir, 'extension/index.mjs'), `export default async api => {
    await api.route('/management', async request => {
      const { method, input } = request.body;
      try {
        const value = method === 'preview' ? await api.plugins.preview(input)
          : method === 'install' ? await api.plugins.install(input)
          : method === 'enable' ? await api.plugins.setEnabled(input.pluginId, input.enabled)
          : method === 'remove' ? await api.plugins.remove(input.pluginId) : (()=>{throw new Error('unknown')})();
        return {status:200,body:{ok:true,value}};
      } catch(error) { return {status:200,body:{ok:false,kind:error.kind,error:error.message}}; }
    });
  };`);
  const zip = packPlugin(dir, join(work, 'fixture.zip')).target;
  const payload = { source: 'local:management-test.zip', archiveBase64: (await readFile(zip)).toString('base64') };
  const preview = await api('/api/plugins/preview', payload);
  const installed = await api('/api/plugins/install', { ...payload, expectedDigest: preview.digest });
  const plugin = installed.plugins.find((p) => p.manifest.id === manifest.id); await authorize(plugin);
  return async (method, input) => {
    const response = await callRoute(manifest.id, '/management', { method, input });
    assert.equal(response.status, 200, JSON.stringify(response));
    return response.body;
  };
}
try {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口已占用');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  await waitFor(() => api('/health'), 'daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'Electron'); ui = pageTools(page);
  const share = await installUpstream('dsh-plugin-share', '0.2.5');
  report.check('真实 Share 原包声明独立插件管理授权', share.manifest.permissions.some((p) => p.kind === 'pluginManagement'));
  await authorize(share);
  const shareCall = async (action, body) => {
    const result = await callRoute(share.manifest.id, `/api/plugin-share/${action}`, body);
    assert.equal(result.status, 200, JSON.stringify(result)); assert.equal(result.body.ok, true, JSON.stringify(result));
    return result.body;
  };
  const exported = await shareCall('export', { entries: [{ kind: 'npm', name: targetPackage, version: '0.0.7', enabled: false }] });
  const parsed = await shareCall('parse', { code: exported.code });
  const qr = await shareCall('qr', { code: exported.code });
  report.check('原包业务生成组合码、解析并生成二维码', parsed.preview[0].action === 'install' && qr.modules.length > 0, JSON.stringify({parsed,qr}));
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '设置入口'); await ui.click('[data-testid=sidebar-settings]');
  const view = share.manifest.contributions.views.find((v) => v.placements.includes('settings'));
  const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${share.manifest.id}:${view.id}`)}]`;
  await waitFor(() => ui.exists(nav), 'Share 原生页面入口'); await ui.click(nav);
  const selector = `iframe[data-plugin-id=${JSON.stringify(share.manifest.id)}]`;
  await waitFor(() => ui.exists(selector), 'Share 页面');
  const frameUrl = await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).src`);
  frame = await waitFor(() => connectTarget(cdpPort, (t) => (t.type === 'iframe' || t.type === 'page') && t.url === frameUrl, 'Share 原生页面', errors), '原包 iframe');
  await waitFor(() => frame.evaluate('Boolean(document.querySelector("textarea.dps-code"))'), '原包组合码输入');
  await frame.evaluate(`(() => {const el=document.querySelector('textarea.dps-code');Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(el,${JSON.stringify(exported.code)});el.dispatchEvent(new Event('input',{bubbles:true}));return true;})()`);
  async function clickText(text) {
    return frame.evaluate(`(() => {const el=[...document.querySelectorAll('button')].find(el=>el.innerText.trim()===${JSON.stringify(text)} && !el.disabled);if(!el) return false;el.click();return true;})()`);
  }
  await waitFor(() => clickText('解析组合码'), '解析按钮');
  await waitFor(() => clickText('全部可处理项加入队列'), '原包安装按钮');
  const imported = await waitFor(target, 'Share 在线安装原包', 240_000);
  report.check('真实上游页面点击安装经翻译器安装原包，保持禁用且未代授权限', imported.manifest.version === '0.0.7' && !imported.active && !imported.enabled && imported.grants.length === 0);
  await waitFor(() => frame.evaluate('document.body.innerText.includes("项完成")'), '原生安装回执', 60_000);
  await screenshot('share-installed');
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  await screenshot('share-390');
  await page.call('Emulation.clearDeviceMetricsOverride');
  await authorize(imported);
  const disabledCode = await shareCall('export', { entries: [{ kind: 'npm', name: targetPackage, version: '0.0.7', enabled: false }] });
  await shareCall('import', { code: disabledCode.code, confirm: true, requestId: 'disable-probe' });
  report.check('原包导入启停状态实际停止目标宿主', !(await target()).enabled && !(await target()).active);
  const enabledCode = await shareCall('export', { entries: [{ kind: 'npm', name: targetPackage, version: '0.0.7', enabled: true }] });
  await shareCall('import', { code: enabledCode.code, confirm: true, requestId: 'enable-probe' });
  await waitFor(async () => (await target()).runtime?.state === 'running', '原包恢复运行', 120_000);
  report.check('原包导入重新启用已授权插件，真实宿主恢复', (await target()).active);
  const manage = await installFixture();
  const input = { ecosystem: 'dsh', package: targetPackage, version: '0.0.8' };
  const preview = await manage('preview', input); assert.equal(preview.ok, true, JSON.stringify(preview));
  const mismatch = await manage('install', { ...input, expectedDigest: 'wrong' });
  report.check('管理服务拒绝错误摘要，保留旧版运行', !mismatch.ok && (await target()).manifest.version === '0.0.7' && (await target()).active);
  const updated = await manage('install', { ...input, expectedDigest: preview.value.candidate.digest });
  assert.equal(updated.ok, true, JSON.stringify(updated));
  const newer = await target();
  report.check('管理服务在线升级真实 npm 原包，排空旧宿主并清空旧版本授权', newer.manifest.version === '0.0.8' && !newer.active && newer.grants.length === 0);
  const denied = await manage('enable', { pluginId: newer.manifest.id, enabled: true });
  report.check('新版本未授权时拒绝激活', !denied.ok && denied.kind === 'PermissionDenied' && !(await target()).active);
  await authorize(newer);
  const removed = await manage('remove', { pluginId: newer.manifest.id });
  report.check('管理服务移除运行中的插件，先停止宿主再删除安装事实', removed.ok && !await target());
  const self = await manage('remove', { pluginId: 'magi.management-test' });
  report.check('自身移除明确拒绝，宿主不会在回执前自杀', !self.ok && (await plugins()).some((p) => p.manifest.id === 'magi.management-test' && p.active));
  const enabledInstall = await shareCall('export', { entries: [{ kind: 'npm', name: targetPackage, version: '0.0.7', enabled: true }] });
  await shareCall('import', { code: enabledInstall.code, confirm: true, requestId: 'delegated-install' });
  await waitFor(async () => (await target())?.runtime?.state === 'running', '委托安装后真实运行', 240_000);
  report.check('Share 原包通过独立授权委托安装并实际运行新包', (await target()).active && (await target()).grants.length > 0);
  // Share 0.2.5 deliberately refuses upgrading existing bundles before calling
  // the host. Preserve that upstream business decision; Mgr covers online upgrades.
  const newerCode = await shareCall('export', { entries: [{ kind: 'npm', name: targetPackage, version: '0.0.8', enabled: true }] });
  const refused = await callRoute(share.manifest.id, '/api/plugin-share/import', { code: newerCode.code, confirm: true, requestId: 'upstream-refusal' });
  report.check('原包主动要求手动升级时保留其回执与当前运行版本', refused.status === 409 && refused.body.error === 'manual-update-required' && (await target()).manifest.version === '0.0.7' && (await target()).active);
  await manage('remove', { pluginId: (await target()).manifest.id });
  report.check('Electron 与原包页面无未捕获异常', errors.length === 0, errors.join('\n'));
  console.log(report.summary());
} catch (error) {
  report.results.push({ name: '真实管理验收完成', ok: false, detail: error.stack ?? String(error) });
  console.error(error); process.exitCode = 1;
  if (ui) await screenshot('failure').catch(() => {});
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify({ checks: report.results, errors, stateRoot: join(work, 'state'), scope: 'Share 原包安装/启停功能 + 管理服务机制；不代表生态全集兼容率' }, null, 2));
  frame?.close(); page?.close();
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
}
