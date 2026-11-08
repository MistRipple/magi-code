// 真实 Ops 原包通过公开路由读写 daemon 的兼容策略；不改上游页面与业务代码。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile, access } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, createReporter, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_DSH_COMPATIBILITY_PORT || 38945);
const cdpPort = Number(process.env.MAGI_DSH_COMPATIBILITY_CDP_PORT || 9315);
const origin = `http://127.0.0.1:${port}`;
const output = process.env.MAGI_DSH_COMPATIBILITY_OUTPUT || join(root, 'target/plugin-acceptance/dsh-compatibility');
const work = await mkdtemp(join(tmpdir(), 'magi-dsh-compatibility-'));
const stateRoot = join(work, 'state'), dshHome = join(work, 'dsh');
const report = createReporter(), errors = [];
let app, page, frame, ui, ops;
await mkdir(output, { recursive: true });
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(300_000) });
  const value = await response.json();
  assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`);
  return value;
}
const plugins = async () => (await api('/api/plugins')).plugins;
const installedTarget = async () => (await plugins()).find((p) => p.source.location.startsWith('dsh:dsh-plugin-release-probe@'));
async function activate(plugin, grants = plugin.manifest.permissions) {
  const path = `/api/plugins/${plugin.manifest.id}`;
  await api(path + '/enable', {});
  await api(path + '/authorize', { grants });
  await api(path + '/activate', {});
  await waitFor(async () => (await plugins()).find((p) => p.manifest.id === plugin.manifest.id)?.runtime?.state === 'running', '原包实际运行', 120_000);
}
async function route(path, body) {
  return api(`/api/plugins/${ops.manifest.id}/routes`, { method: body === undefined ? 'GET' : 'POST', path: '/dsh-ops' + path, ...(body === undefined ? {} : { body }) });
}
async function start() {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口已占用');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: { MAGI_STATE_ROOT: stateRoot, DSH_HOME: dshHome, MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  await waitFor(() => api('/health'), '随包 daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '真实 Electron');
  ui = pageTools(page);
}
async function stop(logName) {
  frame?.close(); frame = undefined; page?.close(); page = undefined;
  await writeFile(join(output, logName), app.log.join(''));
  await app.stop(); app = undefined;
}
async function settings() {
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '设置入口');
  await ui.click('[data-testid=sidebar-settings]');
  const view = ops.manifest.contributions.views.find((v) => v.placements.includes('settings'));
  const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${ops.manifest.id}:${view.id}`)}]`;
  await waitFor(() => ui.exists(nav), 'Ops 原生设置入口'); await ui.click(nav);
  const selector = `iframe[data-plugin-id=${JSON.stringify(ops.manifest.id)}]`;
  await waitFor(() => ui.exists(selector), 'Ops 页面');
  const url = await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).src`);
  frame = await waitFor(() => connectTarget(cdpPort, (t) => ['iframe', 'page'].includes(t.type) && t.url === url, 'Ops 原生页面', errors), '原包 iframe');
  await waitFor(() => frame.evaluate('document.body.innerText.includes("magi (2)") && document.body.innerText.includes("dsh-plugin-release-probe@0.0.8")'), '原包扫描与真实豁免清单', 60_000);
  await writeFile(join(output, 'ops-exemptions.png'), Buffer.from(await ui.screenshot(), 'base64'));
}
try {
  await start();
  const input = { ecosystem: 'dsh', package: 'dsh-plugin-ops-bundle', version: '0.20.8' };
  const preview = await api('/api/plugins/upstream/preview', input);
  ops = (await api('/api/plugins/upstream/install', { ...input, expectedDigest: preview.candidate.digest })).plugins.find((p) => p.source.location === 'dsh:dsh-plugin-ops-bundle@0.20.8');
  report.check('ctx.get 取得的管理服务也声明独立兼容授权', ops.manifest.permissions.some((p) => p.kind === 'pluginManagement' && p.targets.includes('compatibility')));
  await api(`/api/plugins/${ops.manifest.id}/enable`, {});
  const incomplete = ops.manifest.permissions.map((p) => p.kind === 'pluginManagement' ? { ...p, targets: p.targets.filter((t) => t !== 'compatibility') } : p);
  await api(`/api/plugins/${ops.manifest.id}/authorize`, { grants: incomplete });
  let activationDenied = false;
  try { await api(`/api/plugins/${ops.manifest.id}/activate`, {}); } catch (error) { activationDenied = /权限|FORBIDDEN/i.test(error.message); }
  report.check('完全信任和安装权限不能代替 compatibility 委托', activationDenied);
  const runtimeVersion = JSON.parse(await readFile(join(appRoot, 'Contents/Resources/daemon/compat/node_modules/@deepseek-ai/dsh-tools/package.json'), 'utf8')).version;
  const target = { name: 'dsh-plugin-release-probe', version: '0.0.8', spec: 'dsh-plugin-release-probe@0.0.8', runtimeVersion };
  const grants = ops.manifest.permissions;
  await activate(ops, grants);
  const missing = { name: 'magi-dsh-compatibility-nonexistent', version: '0.0.1', spec: 'magi-dsh-compatibility-nonexistent@0.0.1', runtimeVersion };
  const rolledBack = await route('/api/adapt/apply', missing);
  report.check('上游失败安装撤销刚提交的精确版本豁免', rolledBack.body.ok === false && rolledBack.body.rolledBack === true, JSON.stringify(rolledBack));
  const applied = await route('/api/adapt/apply', target);
  report.check('Ops 原包实际在线安装并激活 release-probe', applied.body.ok === true && (await installedTarget())?.runtime?.state === 'running', JSON.stringify(applied));
  const exemptions = await route('/api/adapt/exemptions');
  report.check('原包豁免目录读取 daemon 提交事实', exemptions.body.exemptions['dsh-plugin-release-probe@0.0.8']?.includes(runtimeVersion) && !exemptions.body.exemptions['magi-dsh-compatibility-nonexistent@0.0.1']);
  const scan = await api(`/api/plugins/${ops.manifest.id}/routes`, { method: 'GET', path: '/dsh-ops/api/scan', query: 'profile=magi' });
  report.check('Ops 健康扫描检查真实 magi profile 并返回结构化结果', scan.status === 200 && scan.body.profile === 'magi' && typeof scan.body.counts?.fatal === 'number', JSON.stringify(scan));
  await settings();
  report.check('原生页面显示真实扫描及精确版本豁免', (await frame.evaluate('document.body.innerText')).includes('dsh-plugin-release-probe@0.0.8'));
  await stop('electron-before-restart.log'); await start();
  await waitFor(async () => (await plugins()).find((p) => p.manifest.id === ops.manifest.id)?.runtime?.state === 'running', '重启恢复 Ops', 120_000);
  const persisted = await route('/api/adapt/exemptions');
  report.check('重启后版本豁免仍由 daemon 持久化恢复', persisted.body.exemptions['dsh-plugin-release-probe@0.0.8']?.includes(runtimeVersion));
  const revoked = await route('/api/adapt/revoke', { name: target.name });
  report.check('Ops 原包撤销豁免立即生效', revoked.body.ok === true && revoked.body.removed === 1 && Object.keys((await route('/api/adapt/exemptions')).body.exemptions).length === 0, JSON.stringify(revoked));
  const removed = await route('/api/adapt/remove', { name: target.name });
  report.check('原包卸载沿 daemon 停止并删除目标插件', removed.body.ok === true && !await installedTarget(), JSON.stringify(removed));
  const physical = await Promise.all([join(dshHome, 'profiles/magi'), join(stateRoot, 'extensions/data', ops.manifest.id, 'profiles/magi')].map((dir) => access(dir).then(() => true, () => false)));
  report.check('没有创建第二份物理 DSH profile 或豁免文件', physical.every((exists) => !exists));
  report.check('Electron 与上游原生页面无未捕获异常', errors.length === 0, errors.join('\n'));
  console.log(report.summary());
} catch (error) {
  report.results.push({ name: '真实兼容策略验收完成', ok: false, detail: error.stack ?? String(error) });
  console.error(error); process.exitCode = 1;
  if (ui) await writeFile(join(output, 'failure.png'), Buffer.from(await ui.screenshot(), 'base64')).catch(() => {});
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify({ checks: report.results, errors, stateRoot, scope: 'Ops 原包扫描/兼容豁免/在线安装/持久化/卸载；不代表生态完整业务通过' }, null, 2));
  if (app) await stop('electron.log');
}
