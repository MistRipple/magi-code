// 打包版真实 Health 原包：诊断、修复预览、停用/恢复与原生页面。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, createReporter, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { verifyDshHealth } from './lib/dsh-health-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const output = process.env.MAGI_DSH_HEALTH_OUTPUT || join(root, 'target/plugin-acceptance/health-business');
const work = await mkdtemp(join(tmpdir(), 'magi-health-business-'));
const port = Number(process.env.MAGI_DSH_HEALTH_PORT || 38956), cdpPort = Number(process.env.MAGI_DSH_HEALTH_CDP_PORT || 9326), origin = `http://127.0.0.1:${port}`;
const report = createReporter(), errors = [];
const clipAudit = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE, 'utf8') : null;
let app, page, ui, business;
await mkdir(output, { recursive: true });
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(240_000) });
  const value = await response.json(); assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`); return value;
}
const plugins = async () => (await api('/api/plugins')).plugins;
const current = async id => (await plugins()).find(p => p.manifest.id === id);
async function install(name, version) {
  const input = { ecosystem: 'dsh', package: name, version };
  const preview = await api('/api/plugins/upstream/preview', input);
  await api('/api/plugins/upstream/install', { ...input, expectedDigest: preview.candidate.digest });
  const plugin = (await plugins()).find(p => p.source.location === `dsh:${name}@${version}`); assert.ok(plugin);
  await api(`/api/plugins/${plugin.manifest.id}/enable`, {});
  await waitFor(async () => (await current(plugin.manifest.id)).runtime?.state === 'running', `${name} 原包运行`, 90_000);
  return plugin;
}
async function start() {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口空闲');
  assert.ok(!await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false), '验收 CDP 端口空闲');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, inspectPort: cdpPort + 1,
    env: { MAGI_STATE_ROOT: join(work, 'state'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_COMPAT: '', MAGI_EXTENSION_NODE: '' } });
  await waitFor(() => fetch(origin + '/health').then(r => r.ok), 'daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'Electron', 60_000); ui = pageTools(page);
}
async function stop(log) {
  page?.close(); page = undefined;
  if (app) { await writeFile(join(output, log), app.log.join('')); await app.stop(); app = undefined; }
}
try {
  await start();
  const health = await install('dsh-plugin-health', '3.0.5');
  const target = await install('dsh-plugin-release-probe', '0.0.8');
  business = await verifyDshHealth({ request: api, plugin: health, target, page, cdpPort, errors, output,
    stateRoot: join(work, 'state'), clipAudit, check: report.check,
    inspector: () => ({ inspectPort: cdpPort + 1, pid: app.child.pid }),
    restart: async label => { await stop(label); await start(); return page; },
  });
  page = business.page; ui = pageTools(page);
} catch (error) {
  report.results.push({ name: 'Health 业务验收完成', ok: false, detail: String(error.stack || error) }); process.stderr.write(`${error.stack || error}\n`); process.exitCode = 1;
  if (ui) await writeFile(join(output, 'failure.png'), Buffer.from(await ui.screenshot(), 'base64')).catch(() => {});
} finally {
  await stop('electron.log');
  await writeFile(join(output, 'results.json'), JSON.stringify({ work, errors, checks: report.results, observations: business?.observations }, null, 2));
  process.stdout.write(`${report.summary()}\n`);
  if (report.results.some(check => !check.ok)) process.exitCode = 1;
}
