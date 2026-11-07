import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launchApp, waitFor } from './lib/electron-harness.mjs';

const root = dirname(fileURLToPath(import.meta.url)).replace(/\/scripts$/, '');
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_POPULAR_DSH_PORT || 38942), cdpPort = Number(process.env.MAGI_POPULAR_DSH_CDP_PORT || 9312), origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-popular-dsh-'));
const output = join(root, 'target/plugin-acceptance/popular-dsh');
await mkdir(output, { recursive: true });
const candidates = [
  ['dsh-plugin-health', '3.0.5'],
  ['dsh-plugin-ops-bundle', '0.20.8'],
  ['dsh-plugin-upgrader', '1.8.0'],
  ['dsh-plugin-debugging', '0.7.2'],
  ['dsh-plugin-appshot', '0.4.2'],
  ['dsh-plugin-share', '0.2.5'],
  ['dsh-plugin-mgr', '0.2.12'],
  ['dsh-plugin-release-probe', '0.0.8'],
  ['@99galaxy/dsh-plugin-market', '1.2.2'],
];
const results = [];
let app;
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(240_000) });
  const text = await response.text();
  let value; try { value = JSON.parse(text); } catch { value = text; }
  return { status: response.status, value };
}
async function post(path, body) {
  const result = await api(path, body);
  if (result.status !== 200) throw new Error(`${path}: HTTP ${result.status} ${JSON.stringify(result.value)}`);
  return result.value;
}
async function previewPackage(packageName, version) {
  let last;
  for (let attempt = 0; attempt < 3; attempt += 1) {
    last = await api('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: packageName, version });
    if (last.status === 200 || !/error sending request|timeout|reset|temporarily unavailable/i.test(JSON.stringify(last.value))) return last;
    await new Promise((resolve) => setTimeout(resolve, 1_000));
  }
  return last;
}
try {
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: { MAGI_STATE_ROOT: join(work, 'state'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  await waitFor(async () => (await api('/health')).status === 200, 'Magi daemon', 60_000);
  for (const [pkg, version] of candidates) {
    const item = { package: pkg, version, preview: 'pending' };
    try {
      const previewResult = await previewPackage(pkg, version);
      if (previewResult.status !== 200) {
        item.preview = 'blocked'; item.error = previewResult.value?.message || previewResult.value?.error || JSON.stringify(previewResult.value); results.push(item); continue;
      }
      const preview = previewResult.value;
      item.preview = 'translated';
      item.digest = preview.candidate.digest;
      item.translation = preview.translation;
      item.permissions = preview.candidate.manifest?.permissions?.length ?? null;
      item.contributions = {
        tools: preview.candidate.manifest?.contributions?.tools?.length ?? 0,
        commands: preview.candidate.manifest?.contributions?.commands?.length ?? 0,
        views: preview.candidate.manifest?.contributions?.views?.length ?? 0,
      };
      const installed = await post('/api/plugins/upstream/install', { ecosystem: 'dsh', package: pkg, version, expectedDigest: item.digest });
      const plugin = installed.plugins.find((entry) => entry.source?.location === `dsh:${pkg}@${version}`) || installed.plugins.find((entry) => entry.source?.location?.startsWith(`dsh:${pkg}@`));
      if (!plugin) throw new Error('安装响应未找到插件');
      item.id = plugin.manifest.id; item.installed = true;
      await post(`/api/plugins/${encodeURIComponent(item.id)}/enable`, {});
      const afterEnable = (await api('/api/plugins')).value.plugins.find((entry) => entry.manifest.id === item.id);
      await post(`/api/plugins/${encodeURIComponent(item.id)}/authorize`, { grants: afterEnable.manifest.permissions });
      await post(`/api/plugins/${encodeURIComponent(item.id)}/activate`, {});
      const running = await waitFor(async () => {
        const current = (await api('/api/plugins')).value.plugins.find((entry) => entry.manifest.id === item.id);
        if (current?.runtime?.state === 'failed') return { failed: current.runtime.reason, current };
        return current?.runtime?.state === 'running' ? current : false;
      }, `${pkg} 激活`, 120_000);
      if (running.failed) { item.runtime = 'failed'; item.error = running.failed; }
      else {
        item.runtime = 'running';
        item.runtimeContributions = {
          tools: running.manifest.contributions?.tools?.length ?? 0,
          commands: running.manifest.contributions?.commands?.length ?? 0,
          views: running.manifest.contributions?.views?.length ?? 0,
        };
        item.runtimeReason = running.runtime?.reason;
      }
      await post(`/api/plugins/${encodeURIComponent(item.id)}/deactivate`, {}).catch(() => {});
      await post(`/api/plugins/${encodeURIComponent(item.id)}/uninstall`, {}).catch(() => {});
    } catch (error) {
      item.runtime = item.runtime || 'error'; item.error = error?.message || String(error);
      if (item.id) await post(`/api/plugins/${encodeURIComponent(item.id)}/uninstall`, {}).catch(() => {});
    }
    results.push(item);
    await writeFile(join(output, 'results.json'), JSON.stringify(results, null, 2));
  }
  console.log(JSON.stringify(results, null, 2));
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(results, null, 2));
  if (app) await app.stop();
}
