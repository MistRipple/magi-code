import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, pageTools, launchApp, waitFor } from './lib/electron-harness.mjs';
import { createPluginRegistryGate } from './lib/plugin-registry-gate.mjs';
import { verifyDshUpgrader } from './lib/dsh-upgrader-acceptance.mjs';
import { verifyDshHealth } from './lib/dsh-health-acceptance.mjs';
import { verifyDshShare } from './lib/dsh-share-acceptance.mjs';
import { verifyDshMarket } from './lib/dsh-market-acceptance.mjs';
import { verifyDshAppshot } from './lib/dsh-appshot-acceptance.mjs';
import { verifyDshManager } from './lib/dsh-manager-acceptance.mjs';

const root = dirname(fileURLToPath(import.meta.url)).replace(/\/scripts$/, '');
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_POPULAR_DSH_PORT || 38942), cdpPort = Number(process.env.MAGI_POPULAR_DSH_CDP_PORT || 9312), origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-popular-dsh-'));
const output = process.env.MAGI_DSH_ACCEPTANCE_OUTPUT || join(root, 'target/plugin-acceptance/popular-dsh');
await mkdir(output, { recursive: true });
const candidates = [
  ['dsh-plugin-health', '3.0.5'],
  ['dsh-plugin-ops-bundle', '0.20.8'],
  ['dsh-plugin-upgrader', '1.8.0', '1.7.5'],
  ['dsh-plugin-debugging', '0.7.2'],
  ['dsh-plugin-appshot', '0.4.2'],
  ['dsh-plugin-share', '0.2.5'],
  ['dsh-plugin-mgr', '0.2.13', '0.2.12'],
  ['dsh-plugin-release-probe', '0.0.8'],
  ['@99galaxy/dsh-plugin-market', '1.2.2'],
];
const expectedPackages = candidates.length;
const selected = process.env.MAGI_DSH_ACCEPTANCE_PACKAGES?.split(',').filter(Boolean);
if (selected) {
  for (const name of selected) assert.ok(candidates.some(([pkg]) => pkg === name), `未知验收样本：${name}`);
  candidates.splice(0, candidates.length, ...candidates.filter(([name]) => selected.includes(name)));
}
const results = [];
const errors = [];
const clipAudit = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE, 'utf8') : null;
let app, page, ui, registryGate;
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
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(1_020_000) });
  const text = await response.text();
  let value; try { value = JSON.parse(text); } catch { value = text; }
  return { status: response.status, value };
}
async function post(path, body) {
  const result = await api(path, body);
  if (result.status !== 200) throw new Error(`${path}: HTTP ${result.status} ${JSON.stringify(result.value)}`);
  return result.value;
}
async function removePlugin(id, { verifyPanel = false } = {}) {
  const installed = (await api('/api/plugins')).value.plugins.find(plugin => plugin.manifest.id === id);
  if (!installed) return;
  const tabSelector = `.right-pane-tab[data-tab-id^=${JSON.stringify(`plugin:${id}:`)}]`;
  const frameSelector = `iframe[data-plugin-id=${JSON.stringify(id)}]`;
  const panelWasOpen = verifyPanel && await ui.exists(tabSelector);
  const errorsBefore = errors.length;
  if (installed.active) await post(`/api/plugins/${encodeURIComponent(id)}/deactivate`, {});
  if (panelWasOpen) {
    await waitFor(async () => !await ui.exists(tabSelector) && !await ui.exists(frameSelector), '停用原包回收右栏页签与隔离文档');
    if (!await page.evaluate('Boolean(document.querySelector(".right-pane")?.getClientRects().length)')) await ui.click('.header-right-pane-btn');
    await waitFor(() => ui.exists('.right-pane-add-tab'), '停用原包后右栏仍可操作');
    await ui.click('.right-pane-add-tab');
    await waitFor(() => ui.exists('[data-testid=right-pane-launcher]'), '停用原包后重新打开原生启动页');
    assert.deepEqual(errors.slice(errorsBefore), [], '停用与页签离场不能产生页面异常');
    await writeFile(join(output, `${id}-panel-deactivated.png`), Buffer.from(await ui.screenshot(), 'base64'));
  }
  if (installed.enabled) await post(`/api/plugins/${encodeURIComponent(id)}/disable`, {});
  await post(`/api/plugins/${encodeURIComponent(id)}/uninstall`, {});
  if (panelWasOpen) {
    assert.equal((await api('/api/plugins')).value.plugins.some(plugin => plugin.manifest.id === id), false);
    assert.equal(await ui.exists(tabSelector), false);
    assert.equal(await ui.exists(frameSelector), false);
    assert.deepEqual(errors.slice(errorsBefore), [], '卸载原包后工作台继续可用且没有迟到页面异常');
  }
  return { panelWasOpen, ...(panelWasOpen ? { deactivatedPanelReleased: true, nativeLauncherUsable: true, uninstalledPanelReleased: true } : {}) };
}
async function installTarget(version = '0.0.7') {
  const request = { ecosystem: 'dsh', package: 'dsh-plugin-release-probe', version };
  const preview = await post('/api/plugins/upstream/preview', request);
  const list = await post('/api/plugins/upstream/install', { ...request, expectedDigest: preview.candidate.digest });
  const plugin = list.plugins.find((entry) => entry.source.location === `dsh:${request.package}@${version}`);
  assert.ok(plugin, 'daemon 必须确认目标安装身份');
  await post(`/api/plugins/${plugin.manifest.id}/enable`, {});
  await post(`/api/plugins/${plugin.manifest.id}/authorize`, { grants: plugin.manifest.permissions });
  await post(`/api/plugins/${plugin.manifest.id}/activate`, {});
  await waitFor(async () => (await currentTarget())?.runtime?.state === 'running', '业务验收目标运行', 120_000);
  return plugin;
}
const currentTarget = async () => (await api('/api/plugins')).value.plugins.find((entry) => entry.source.location.startsWith('dsh:dsh-plugin-release-probe@'));
async function viewCheck(plugin, view, session, interact) {
  const views = plugin.manifest.contributions.views;
  const selector = `iframe[data-plugin-id=${JSON.stringify(plugin.manifest.id)}]`;
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}` });
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台');
  if (view.placements.includes('rightPane')) {
    if (!await page.evaluate('Boolean(document.querySelector(".right-pane")?.getClientRects().length)')) await ui.click('.header-right-pane-btn');
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
    if (interact) await interact(frame);
    // 等待一次上游页面初始化；请求结果另由后端 smoke 记录，不能把页面有字视为业务完成。
    await new Promise((resolve) => setTimeout(resolve, 1500));
    const text = await frame.evaluate('document.body.innerText');
    const failure = text.match(/界面加载失败|该插件没有提供设置界面|Minified React error|[\w.]+ is not a function|Cannot read properties|ReferenceError:|TypeError:|ERR_MODULE_NOT_FOUND/)?.[0];
    const screenshot = `${plugin.manifest.id}-${view.id}.png`;
    await writeFile(join(output, screenshot), Buffer.from(await ui.screenshot(), 'base64'));
    return { id: view.id, placement: view.placements.includes('rightPane') ? 'rightPane' : 'settings', ok: !failure, ...(failure ? { failure } : {}), text: text.slice(0, 5000), screenshot, declaredViews: views.length };
  } catch (error) {
    await writeFile(join(output, `${plugin.manifest.id}-${view.id}-failure.png`), Buffer.from(await ui.screenshot(), 'base64'));
    await writeFile(join(output, `${plugin.manifest.id}-${view.id}-failure.json`), JSON.stringify({ error: error.message,
      page: await frame?.evaluate(`({text:document.body.innerText,buttons:[...document.querySelectorAll('button')].map(button=>({text:button.innerText,disabled:button.disabled}))})`).catch(() => null),
    }, null, 2));
    throw error;
  } finally { frame?.close(); }
}
try {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收 daemon 端口被占用');
  assert.ok(!await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false), '验收 CDP 端口被占用');
  registryGate = await createPluginRegistryGate();
  const launchOptions = { executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, inspectPort: cdpPort + 1, env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_NPM_REGISTRY: registryGate.registry, MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } };
  const restart = async label => {
    page.close(); await writeFile(join(output, `electron-${label}`), app.log.join('')); await app.stop();
    app = launchApp(launchOptions); await waitFor(async () => (await api('/health')).status === 200, '社区验收重启 daemon', 60_000);
    page = await waitFor(() => connectAppPage(cdpPort, port, errors), '重启社区工作台'); ui = pageTools(page); return page;
  };
  app = launchApp(launchOptions);
  await waitFor(async () => (await api('/health')).status === 200, 'Magi daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'Magi App Renderer'); ui = pageTools(page);
  const session = (await post('/api/session/materialize', { scope: 'personal' })).sessionId;
  for (const [pkg, version, previousVersion] of candidates) {
    const initialVersion = previousVersion ?? version;
    const item = { package: pkg, version, ...(previousVersion ? { initialVersion } : {}), preview: 'pending', core: 'unverified', upgrade: 'unverified' };
    console.log(`CHECK ${pkg}@${version}`);
    try {
      const previewResult = await api('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: pkg, version: initialVersion });
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
      const installed = await post('/api/plugins/upstream/install', { ecosystem: 'dsh', package: pkg, version: initialVersion, expectedDigest: item.digest });
      const plugin = installed.plugins.find((entry) => entry.source?.location === `dsh:${pkg}@${initialVersion}`);
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
          item.businessAttempted = true;
          const target = await installTarget('0.0.8');
          try {
            const business = await verifyDshHealth({ request: post, plugin: running, target, page, cdpPort, errors, output,
              stateRoot: join(work, 'state'), clipAudit, restart,
              inspector: () => ({ inspectPort: cdpPort + 1, pid: app.child.pid }),
              check: (name, ok, detail) => { assert.ok(ok, `${name}: ${JSON.stringify(detail ?? '')}`); console.log(`✓ ${name}`); },
            });
            page = business.page; ui = pageTools(page);
            item.functionalChecks = business.checks;
            item.upgrade = 'passed';
            item.updatedVersion = business.plugin.manifest.version;
          } finally { const target = await currentTarget(); if (target) await removePlugin(target.manifest.id); }
        }
        if (pkg === 'dsh-plugin-debugging') {
          const skill = running.manifest.contributions.skills.find(skill => skill.title === pkg);
          assert.ok(skill, '原包声明的技能必须进入 daemon 当前目录，不能只验证工具和页面');
          const original = await readFile(join(work, 'state/extensions/packages', item.id, running.digest, 'extension/upstream/skills', pkg, 'SKILL.md'), 'utf8');
          assert.equal(skill.instruction, original.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n/, '').trim());
          const catalog = (await api('/api/plugins/manifests')).value;
          assert.ok(catalog.some(manifest => manifest.id === item.id && manifest.contributions.skills.some(entry => entry.id === skill.id && entry.title === pkg)));
          item.functionalChecks = { runtimeSkill: true, originalSkillBody: true, skillDiscovery: true, runtimeDiagnostics: 'unverified', restart: 'unverified' };
        }
        if (pkg === 'dsh-plugin-appshot' && item.smoke?.ok) {
          item.businessAttempted = true;
          const business = await verifyDshAppshot({ request: post, origin, plugin: running, page, cdpPort, errors,
            output, stateRoot: join(work, 'state'), liveCapture: process.env.MAGI_APPSHOT_LIVE_CAPTURE === '1',
            restart: () => restart('before-appshot-restart.log'),
            check: (name, ok, detail) => { assert.ok(ok, `${name}: ${JSON.stringify(detail ?? '')}`); console.log(`✓ ${name}`); },
          });
          page = business.page; ui = pageTools(page);
          item.functionalChecks = business.checks;
        }
        if (pkg === 'dsh-plugin-share' && item.smoke?.ok) {
          item.businessAttempted = true;
          const business = await verifyDshShare({ request: post, plugin: running, page, cdpPort, errors,
            inspector: () => ({ inspectPort: cdpPort + 1, pid: app.child.pid }),
            gate: registryGate, output, stateRoot: join(work, 'state'), clipAudit,
            restart: () => restart('before-share-restart.log'),
            check: (name, ok, detail) => { assert.ok(ok, `${name}: ${JSON.stringify(detail ?? '')}`); console.log(`✓ ${name}`); },
          });
          page = business.page; ui = pageTools(page);
          item.functionalChecks = business.checks;
          item.upgrade = 'passed';
        }
        if (pkg === '@99galaxy/dsh-plugin-market' && item.smoke?.ok) {
          item.businessAttempted = true;
          const business = await verifyDshMarket({ request: post, origin, plugin: running, page, cdpPort, errors,
            output, stateRoot: join(work, 'state'), restart: () => restart('before-market-restart.log'),
            check: (name, ok, detail) => { assert.ok(ok, `${name}: ${JSON.stringify(detail ?? '')}`); console.log(`✓ ${name}`); },
          });
          page = business.page; ui = pageTools(page);
          item.functionalChecks = business.checks;
          item.upgrade = 'passed';
        }
        if (pkg === 'dsh-plugin-mgr' && item.smoke?.ok) {
          item.businessAttempted = true;
          try {
            const business = await verifyDshManager({ request: post, origin, plugin: running, page, cdpPort, errors,
              gate: registryGate, output, stateRoot: join(work, 'state'), clipAudit,
              restart, appPid: () => app.child.pid,
              check: (name, ok, detail) => { assert.ok(ok, `${name}: ${JSON.stringify(detail ?? '')}`); console.log(`✓ ${name}`); },
            });
            page = business.page; ui = pageTools(page);
            item.functionalChecks = business.checks;
            item.updatedVersion = business.plugin.manifest.version;
            item.updatedDigest = business.plugin.digest;
            item.upgrade = 'passed';
          } finally { const target = await currentTarget(); if (target) await removePlugin(target.manifest.id); }
        }
        if (pkg === 'dsh-plugin-upgrader') {
          item.businessAttempted = true;
          const business = await verifyDshUpgrader({ request: post, origin, plugin: running, page, cdpPort, errors,
            gate: registryGate, output, stateRoot: join(work, 'state'),
            check: (name, ok, detail) => { assert.ok(ok, `${name}: ${JSON.stringify(detail ?? '')}`); console.log(`✓ ${name}`); },
            restart: () => restart('before-upgrader-restart.log'),
          });
          page = business.page; ui = pageTools(page);
          item.functionalChecks = business.checks;
          item.updatedVersion = business.plugin.manifest.version;
          item.updatedDigest = business.plugin.digest;
          item.upgrade = 'passed';
        }

      }
      // 覆盖率只记录已完成的业务断言：页面有内容、启动成功或 HTTP 200
      // 不能代替核心业务。插件没有明确的功能检查时保持 unverified，避免把
      // 兼容机制样本误算进生态分母。
      if (item.functionalChecks) {
        const checks = Object.values(item.functionalChecks);
        item.core = checks.every((value) => value === true) ? 'passed'
          : checks.some((value) => value === false) ? 'failed' : 'unverified';
      }
      item.panelLifecycle = await removePlugin(item.id, { verifyPanel: true });
      item.cleaned = true;
    } catch (error) {
      if (item.businessAttempted) item.core = 'failed';
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
    expectedPackages, completeSample: results.length === expectedPackages,
    installed: results.filter((item) => item.installed).length, started: results.filter((item) => item.runtime === 'running').length,
    uiPassed: results.filter((item) => item.views?.length && item.views.every((view) => view.ok)).length,
    smokePassed: results.filter((item) => item.smoke?.ok).length,
    coreVerified: results.filter((item) => item.core === 'passed').length,
    upgradeVerified: results.filter((item) => item.upgrade === 'passed').length,
    thresholdPercent: 95, qualified: false, rendererErrors: errors };
  // 尚未执行完整业务及升级的样本不算成功。启动、页面有内容、HTTP 200 都不能替代功能验收。
  summary.qualified = summary.completeSample && [summary.installed, summary.started, summary.coreVerified, summary.upgradeVerified].every((count) => count / summary.packages >= 0.95)
    && results.every((item) => !item.views?.some((view) => !view.ok)) && errors.length === 0;
  await writeFile(join(output, 'coverage.json'), JSON.stringify(summary, null, 2));
  await writeFile(join(output, 'registry-errors.json'), JSON.stringify(registryGate?.errors ?? [], null, 2));
  if (!summary.qualified || errors.length) process.exitCode = 1;
  console.log(JSON.stringify(summary));
  page?.close();
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  await registryGate?.close();
}
