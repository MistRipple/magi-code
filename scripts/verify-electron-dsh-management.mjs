// 真实目录包 + npm/GitHub 原包。Share 的页面与业务不修改；管理服务夹具只验证
// 无上游升级页面覆盖的 RPC/授权/摘要/卸载机制，不计入生态兼容率。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { packPlugin } from './compat/core/pack.mjs';
import { connectAppPage, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';
import { createPluginRegistryGate } from './lib/plugin-registry-gate.mjs';
import { verifyDshUpgrader } from './lib/dsh-upgrader-acceptance.mjs';
import { verifyDshShare } from './lib/dsh-share-acceptance.mjs';
import { verifyDshMarket } from './lib/dsh-market-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_DSH_MANAGEMENT_PORT || 38944), cdpPort = Number(process.env.MAGI_DSH_MANAGEMENT_CDP_PORT || 9314);
const origin = `http://127.0.0.1:${port}`;
const output = process.env.MAGI_DSH_MANAGEMENT_OUTPUT || join(root, 'target/plugin-acceptance/dsh-management');
const work = await mkdtemp(join(tmpdir(), 'magi-dsh-management-'));
const launchOptions = { executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, inspectPort: cdpPort + 1, env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_NPM_REGISTRY: '', MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } };
const report = createReporter(), errors = [];
const clipAuditSource = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE, 'utf8') : null;
let app, page, ui;
let registryGate;
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
  await waitFor(async () => (await plugins()).find((p) => p.manifest.id === plugin.manifest.id)?.runtime?.state === 'running', '原包运行', 120_000);
}
async function installUpstream(name, version) {
  const input = { ecosystem: 'dsh', package: name, version };
  const preview = await api('/api/plugins/upstream/preview', input);
  const list = await api('/api/plugins/upstream/install', { ...input, expectedDigest: preview.candidate.digest });
  return list.plugins.find((plugin) => plugin.source.location === `dsh:${name}@${version}`);
}
async function callRoute(id, path, body) {
  return api(`/api/plugins/${id}/routes`, { method: body === undefined ? 'GET' : 'POST', path, ...(body === undefined ? {} : { body }) });
}
async function screenshot(name) { await writeFile(join(output, name + '.png'), Buffer.from(await ui.screenshot(), 'base64')); }
async function restart(label) {
  page.close(); await writeFile(join(output, `electron-${label}`), app.log.join('')); await app.stop();
  app = launchApp(launchOptions); await waitFor(() => api('/health'), '重启 daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '重启 Electron'); ui = pageTools(page); return page;
}
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
  registryGate = await createPluginRegistryGate();
  const registry = registryGate.registry;
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口已占用');
  launchOptions.env.MAGI_NPM_REGISTRY = registry;
  app = launchApp(launchOptions);
  await waitFor(() => api('/health'), 'daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'Electron'); ui = pageTools(page);
  const share = await installUpstream('dsh-plugin-share', '0.2.5');
  report.check('真实 Share 原包声明独立插件管理授权', share.manifest.permissions.some((p) => p.kind === 'pluginManagement'));
  await authorize(share);
  const shareBusiness = await verifyDshShare({ request: api, plugin: share, page, cdpPort, errors,
    inspector: () => ({ inspectPort: cdpPort + 1, pid: app.child.pid }),
    gate: registryGate, output, stateRoot: join(work, 'state'), clipAudit: clipAuditSource, restart, check: report.check,
  });
  page = shareBusiness.page; ui = pageTools(page);
  const manage = await installFixture();
  const market = await installUpstream('@99galaxy/dsh-plugin-market', '1.2.2'); await authorize(market);
  const marketBusiness = await verifyDshMarket({ request: api, origin, plugin: market, page, cdpPort, errors,
    output, stateRoot: join(work, 'state'), restart: () => restart('before-market-restart.log'), check: report.check,
  });
  page = marketBusiness.page; ui = pageTools(page);
  // 保留独立管理 API 的摘要与授权回归，原包业务由上面的共用合同验证。
  const baseline = await installUpstream(targetPackage, '0.0.7'); await authorize(baseline);
  const input = { ecosystem: 'dsh', package: targetPackage, version: '0.0.8' };
  const preview = await manage('preview', input); assert.equal(preview.ok, true, JSON.stringify(preview));
  const mismatch = await manage('install', { ...input, expectedDigest: 'wrong' });
  report.check('管理服务拒绝错误摘要，保留旧版运行', !mismatch.ok && (await target()).manifest.version === '0.0.7' && (await target()).enabled);
  const updated = await manage('install', { ...input, expectedDigest: preview.value.candidate.digest });
  assert.equal(updated.ok, true, JSON.stringify(updated));
  const newer = await target();
  report.check('管理服务在线升级真实 npm 原包，排空旧宿主并保持启用', newer.manifest.version === '0.0.8' && newer.enabled);
  await authorize(newer);
  const removed = await manage('remove', { pluginId: newer.manifest.id });
  report.check('管理服务移除运行中的插件，先停止宿主再删除安装事实', removed.ok && !await target());
  const self = await manage('remove', { pluginId: 'magi.management-test' });
  report.check('自身移除明确拒绝，宿主不会在回执前自杀', !self.ok && (await plugins()).some((p) => p.manifest.id === 'magi.management-test' && p.enabled));
  const upgrader = await installUpstream('dsh-plugin-upgrader', '1.7.5'); await authorize(upgrader);
  const business = await verifyDshUpgrader({ request: api, origin, plugin: upgrader, page, cdpPort, errors,
    gate: registryGate, output, stateRoot: join(work, 'state'), check: report.check,
    restart: () => restart('before-upgrader-restart.log'),
  });
  page = business.page; ui = pageTools(page);
  report.check('Electron 与原包页面无未捕获异常', errors.length === 0, errors.join('\n'));
  console.log(report.summary());
} catch (error) {
  report.results.push({ name: '真实管理验收完成', ok: false, detail: error.stack ?? String(error) });
  console.error(error); process.exitCode = 1;
  if (ui) await screenshot('failure').catch(() => {});
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify({ checks: report.results, errors, stateRoot: join(work, 'state'), scope: 'Share/Market/Upgrader 原包管理，npm/GitHub 在线安装、升级与恢复；不代表生态全集业务覆盖率' }, null, 2));
  page?.close();
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  await registryGate?.close();
}
