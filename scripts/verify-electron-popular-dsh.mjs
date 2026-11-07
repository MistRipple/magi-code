import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, pageTools, launchApp, waitFor } from './lib/electron-harness.mjs';

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
const errors = [];
let app, page, ui;
const probes = {
  'dsh-plugin-health': '/plugins/dsh-plugin-health/status',
  'dsh-plugin-ops-bundle': '/dsh-ops/api/sandbox/status',
  'dsh-plugin-upgrader': '/api/plugin-upgrader/overview',
  'dsh-plugin-appshot': '/plugins/appshot/config',
  'dsh-plugin-share': '/api/plugin-share/state',
  'dsh-plugin-mgr': '/api/plugin-manager/list',
  '@99galaxy/dsh-plugin-market': '/dsh-plugin-market/list',
};
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
async function removePlugin(id) {
  await post(`/api/plugins/${encodeURIComponent(id)}/deactivate`, {});
  await post(`/api/plugins/${encodeURIComponent(id)}/disable`, {});
  await post(`/api/plugins/${encodeURIComponent(id)}/uninstall`, {});
}
async function viewCheck(plugin, view, session) {
  const views = plugin.manifest.contributions.views;
  const selector = `iframe[data-plugin-id=${JSON.stringify(plugin.manifest.id)}]`;
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}` });
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台');
  if (view.placements.includes('rightPane')) {
    await ui.click('.header-right-pane-btn');
    await waitFor(() => ui.exists('.right-pane-add-tab'), '右栏');
    await ui.click('.right-pane-add-tab');
    const tool = `.launcher-tool[data-plugin-id=${JSON.stringify(plugin.manifest.id)}][data-contribution-id=${JSON.stringify(view.id)}]`;
    await waitFor(() => ui.exists(tool), '右栏原包入口');
    await ui.click(tool);
  } else {
    await ui.click('[data-testid=sidebar-settings]');
    const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${plugin.manifest.id}:${view.id}`)}]`;
    await waitFor(() => ui.exists(nav), '原包设置入口');
    await ui.click(nav);
  }
  await waitFor(() => ui.exists(selector), '原包 iframe');
  const url = await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).src`);
  let frame;
  try {
    frame = await waitFor(() => connectTarget(cdpPort, (target) => (target.type === 'iframe' || target.type === 'page') && target.url === url, '原包页面', errors), '原包页面');
    await waitFor(async () => (await frame.evaluate('document.body.innerText')).trim().length > 0, '原包内容');
    // 等待一次上游页面初始化；请求结果另由后端 smoke 记录，不能把页面有字视为业务完成。
    await new Promise((resolve) => setTimeout(resolve, 1500));
    const text = await frame.evaluate('document.body.innerText');
    const failure = text.match(/界面加载失败|该插件没有提供设置界面|Minified React error|[\w.]+ is not a function|Cannot read properties|ReferenceError:|TypeError:|ERR_MODULE_NOT_FOUND/)?.[0];
    const screenshot = `${plugin.manifest.id}-${view.id}.png`;
    await writeFile(join(output, screenshot), Buffer.from(await ui.screenshot(), 'base64'));
    return { id: view.id, placement: view.placements.includes('rightPane') ? 'rightPane' : 'settings', ok: !failure, ...(failure ? { failure } : {}), text: text.slice(0, 5000), screenshot, declaredViews: views.length };
  } finally { frame?.close(); }
}
try {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收 daemon 端口被占用');
  assert.ok(!await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false), '验收 CDP 端口被占用');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  await waitFor(async () => (await api('/health')).status === 200, 'Magi daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'Magi App Renderer'); ui = pageTools(page);
  const session = (await post('/api/session/materialize', { scope: 'personal' })).sessionId;
  for (const [pkg, version] of candidates.filter(([name]) => !process.env.MAGI_DSH_PACKAGES || process.env.MAGI_DSH_PACKAGES.split(',').includes(name))) {
    const item = { package: pkg, version, preview: 'pending', core: 'unverified', upgrade: 'unverified' };
    console.log(`CHECK ${pkg}@${version}`);
    try {
      const previewResult = await previewPackage(pkg, version);
      if (previewResult.status !== 200) {
        item.preview = 'blocked'; throw new Error(previewResult.value?.message || previewResult.value?.error || JSON.stringify(previewResult.value));
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
        item.views = [];
        for (const view of running.manifest.contributions?.views ?? []) {
          try { item.views.push(await viewCheck(running, view, session)); }
          catch (error) { item.views.push({ id: view.id, ok: false, error: error.message }); }
        }
        const path = probes[pkg];
        if (path) {
          const route = await api(`/api/plugins/${encodeURIComponent(item.id)}/routes`, { method: 'GET', path });
          item.smoke = { path, transportStatus: route.status, ...route.value };
          item.smoke.ok = route.status === 200 && route.value.status >= 200 && route.value.status < 300 && route.value.body?.ok !== false && !route.value.body?.error;
        }
        if (pkg === 'dsh-plugin-health' && item.smoke?.ok) {
          const status = item.smoke.body;
          assert.equal(status.profile, 'magi');
          assert.equal(status.review?.schemaVersion, 1, '原包可靠性报告不能被吞成 null');
          assert.ok(status.review.packages.some((entry) => entry.name === pkg && entry.version === version));
          assert.ok(status.liveReview?.entries.some((entry) => entry.name === pkg && entry.state === 'active'));
          assert.equal(status.issues.some((issue) => ['profiles-missing', 'profile-missing', 'profile-bundles-empty'].includes(issue.id)), false);
          const runtimeVersion = JSON.parse(await readFile(join(appRoot, 'Contents/Resources/daemon/compat/node_modules/@deepseek-ai/dsh-app-boot/package.json'), 'utf8')).version;
          assert.equal(status.runtimeVersion, runtimeVersion, '不能把已携带的 DSH SDK 版本显示为未知');
          assert.equal(status.review.currentVersion, runtimeVersion);
          assert.equal(status.review.packages.find((entry) => entry.name === pkg)?.declaration.state, 'compatible', '原包 engines/peer 声明必须到达实际兼容报告');
          item.functionalChecks = { fileInspection: true, reliabilityReport: true, liveActivation: true, runtimeIdentity: true, declaredCompatibility: true, repairs: 'unverified' };
        }
        if (pkg === 'dsh-plugin-share' && item.smoke?.ok) {
          const call = async (action, body) => {
            const response = await post(`/api/plugins/${encodeURIComponent(item.id)}/routes`, { method: 'POST', path: `/api/plugin-share/${action}`, body });
            assert.equal(response.status, 200, JSON.stringify(response)); assert.equal(response.body.ok, true, JSON.stringify(response.body));
            return response.body;
          };
          const exported = await call('export', {});
          assert.ok(exported.entries.some((entry) => entry.name === pkg && entry.version === version), '导出必须包含 daemon 中真实安装的包');
          const parsed = await call('parse', { code: exported.code });
          const qr = await call('qr', { code: exported.code });
          assert.ok(parsed.preview.length && qr.modules.length);
          item.functionalChecks = { export: true, parse: true, qr: true, import: 'unverified' };
        }
        if (pkg === 'dsh-plugin-upgrader' && process.env.MAGI_DSH_FUNCTIONAL_UPGRADE === '1') {
          let targetId;
          try {
            const request = { ecosystem: 'dsh', package: 'dsh-plugin-release-probe', version: '0.0.7' };
            const targetPreview = await post('/api/plugins/upstream/preview', request);
            const targetList = await post('/api/plugins/upstream/install', { ...request, expectedDigest: targetPreview.candidate.digest });
            const target = targetList.plugins.find((p) => p.source.location === 'dsh:dsh-plugin-release-probe@0.0.7');
            targetId = target.manifest.id;
            await post(`/api/plugins/${targetId}/enable`, {});
            await post(`/api/plugins/${targetId}/authorize`, { grants: target.manifest.permissions });
            await post(`/api/plugins/${targetId}/activate`, {});
            const submitted = await post(`/api/plugins/${item.id}/routes`, { method: 'POST', path: '/api/plugin-upgrader/job', body: { steps: [{ name: request.package, version: '0.0.8', action: 'upgrade', enabled: true }] } });
            assert.equal(submitted.status, 202, JSON.stringify(submitted));
            const job = await waitFor(async () => {
              const current = await post(`/api/plugins/${item.id}/routes`, { method: 'GET', path: '/api/plugin-upgrader/job', query: `id=${submitted.body.id}` });
              return current.body.status === 'finished' ? current.body : false;
            }, 'Upgrader 原包后台升级完成', 240_000);
            item.functionalUpgrade = job;
            assert.equal(job.failed, 0, JSON.stringify(job));
            const after = (await api('/api/plugins')).value.plugins.find((p) => p.manifest.id === targetId);
            assert.equal(after.manifest.version, '0.0.8');
            assert.equal(after.runtime.state, 'running');
            item.functionalChecks = { onlineUpgrade: true, rollback: 'unverified' };
          } finally { if (targetId) await removePlugin(targetId); }
        }
      }
      await removePlugin(item.id);
      item.cleaned = true;
    } catch (error) {
      item.runtime = item.runtime || 'error'; item.error = error?.message || String(error);
      if (item.id) await removePlugin(item.id).then(() => { item.cleaned = true; }, (error) => { item.cleanupError = error.message; });
    }
    results.push(item);
    await writeFile(join(output, 'results.json'), JSON.stringify(results, null, 2));
    console.log(JSON.stringify(item));
  }
  console.log(JSON.stringify(results, null, 2));
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(results, null, 2));
  const summary = { scope: '固定的社区样本，不代表 DSH 生态全集', packages: results.length,
    installed: results.filter((item) => item.installed).length, started: results.filter((item) => item.runtime === 'running').length,
    uiPassed: results.filter((item) => item.views?.length && item.views.every((view) => view.ok)).length,
    smokePassed: results.filter((item) => item.smoke?.ok).length,
    coreVerified: results.filter((item) => item.core === 'passed').length,
    upgradeVerified: results.filter((item) => item.upgrade === 'passed').length,
    thresholdPercent: 95, qualified: false, rendererErrors: errors };
  // 尚未执行完整业务及升级的样本不算成功。启动、页面有内容、HTTP 200 都不能替代功能验收。
  summary.qualified = summary.packages > 0 && [summary.installed, summary.started, summary.coreVerified, summary.upgradeVerified].every((count) => count / summary.packages >= 0.95)
    && results.every((item) => !item.views?.some((view) => !view.ok)) && errors.length === 0;
  await writeFile(join(output, 'coverage.json'), JSON.stringify(summary, null, 2));
  if (!summary.qualified || errors.length) process.exitCode = 1;
  console.log(JSON.stringify(summary));
  page?.close();
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
}
