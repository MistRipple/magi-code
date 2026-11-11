import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launchApp, connectAppPage, waitFor } from './lib/electron-harness.mjs';
import { verifyDshAppshot } from './lib/dsh-appshot-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_APPSHOT_PORT || 38944), cdpPort = Number(process.env.MAGI_APPSHOT_CDP_PORT || 9314);
const origin = `http://127.0.0.1:${port}`, work = await mkdtemp(join(tmpdir(), 'magi-appshot-persistence-'));
const output = process.env.MAGI_APPSHOT_OUTPUT || join(root, 'target/plugin-acceptance/appshot');
await mkdir(output, { recursive: true });
const errors = [], logs = []; let app, page, id;
async function stop() {
  if (!app) return;
  await app.stop(); logs.push(...app.log); app = null;
}
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(1_020_000) });
  const value = await response.json();
  assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`);
  return value;
}
const pluginPath = () => `/api/plugins/${encodeURIComponent(id)}`;
async function start() {
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  await waitFor(async () => (await api('/health')).status === 'ok', 'daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'App Renderer');
}
try {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口被占用');
  await start();
  const preview = await api('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: 'dsh-plugin-appshot', version: '0.4.2' });
  const installed = await api('/api/plugins/upstream/install', { ecosystem: 'dsh', package: 'dsh-plugin-appshot', version: '0.4.2', expectedDigest: preview.candidate.digest });
  const plugin = installed.plugins.find(p => p.source?.location === 'dsh:dsh-plugin-appshot@0.4.2'); id = plugin.manifest.id;
  await api(pluginPath() + '/enable', {}); await api(pluginPath() + '/authorize', { grants: plugin.manifest.permissions }); await api(pluginPath() + '/activate', {});
  const result = await verifyDshAppshot({ request: api, origin, plugin, page, cdpPort, errors, output, stateRoot: join(work, 'state'),
    liveCapture: process.env.MAGI_APPSHOT_LIVE_CAPTURE === '1', check: name => console.log(`PASS ${name}`),
    restart: async () => { page.close(); await stop(); await start(); return page; },
  });
  page = result.page;
} catch (error) { console.error(error); process.exitCode = 1; }
finally {
  page?.close(); await stop();
  await writeFile(join(output, 'electron.log'), logs.join(''));
  const evidence = await readFile(join(output, 'appshot-business.json'), 'utf8').catch(() => JSON.stringify({ error: '原包安装或启动失败', rendererErrors: errors }));
  await writeFile(join(output, 'results.json'), evidence);
}
