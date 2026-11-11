// 真实 Mgr 原包专项；与社区入口共享同一套管理业务、恢复和界面合同。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launchApp, connectAppPage, waitFor, createReporter } from './lib/electron-harness.mjs';
import { createPluginRegistryGate } from './lib/plugin-registry-gate.mjs';
import { verifyDshManager } from './lib/dsh-manager-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_PROFILE_PORT || 38946), cdpPort = Number(process.env.MAGI_PROFILE_CDP_PORT || 9316), origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-profile-live-')), stateRoot = join(work, 'state');
const output = process.env.MAGI_DSH_PROFILE_OUTPUT || join(root, 'target/plugin-acceptance/profile');
const report = createReporter(), errors = [];
const clipAudit = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE, 'utf8') : null;
let app, page, gate;
await mkdir(output, { recursive: true });
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(300_000) });
  const value = await response.json(); assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`); return value;
}
async function start() {
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: { MAGI_STATE_ROOT: stateRoot, DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_NPM_REGISTRY: gate.registry, MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '', MAGI_OPEN_BROWSER: '0' } });
  await waitFor(() => api('/health'), 'daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'Electron');
  return page;
}
async function restart(label) {
  page?.close(); await writeFile(join(output, label), app.log.join('')); await app.stop();
  return start();
}
try {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口被占用');
  assert.ok(!await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false), '验收 CDP 端口被占用');
  gate = await createPluginRegistryGate();
  await start();
  const request = { ecosystem: 'dsh', package: 'dsh-plugin-mgr', version: '0.2.12' };
  const preview = await api('/api/plugins/upstream/preview', request);
  const installed = await api('/api/plugins/upstream/install', { ...request, expectedDigest: preview.candidate.digest });
  const plugin = installed.plugins.find(value => value.manifest.id === preview.candidate.manifest.id);
  assert.ok(plugin);
  await api(`/api/plugins/${plugin.manifest.id}/enable`, {});
  await api(`/api/plugins/${plugin.manifest.id}/authorize`, { grants: plugin.manifest.permissions });
  await api(`/api/plugins/${plugin.manifest.id}/activate`, {});
  const running = await waitFor(async () => {
    const value = (await api('/api/plugins')).plugins.find(value => value.manifest.id === plugin.manifest.id);
    assert.notEqual(value?.runtime?.state, 'failed', value?.runtime?.reason);
    return value?.runtime?.state === 'running' && value;
  }, 'Mgr 原包启动', 120_000);
  const result = await verifyDshManager({ request: api, origin, plugin: running, page, cdpPort, errors, gate, output, stateRoot, restart, appPid: () => app.child.pid, clipAudit, check: report.check });
  page = result.page;
} catch (error) {
  report.results.push({ name: 'profile 原包验收', ok: false, detail: error.stack ?? String(error) });
  console.error(error); process.exitCode = 1;
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify({ checks: report.results, errors, stateRoot }, null, 2));
  page?.close();
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  await gate?.close();
  console.log(report.summary());
}
