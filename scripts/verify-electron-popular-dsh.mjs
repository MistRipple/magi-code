import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, pageTools, launchApp, waitFor } from './lib/electron-harness.mjs';
import { createPluginRegistryGate } from './lib/plugin-registry-gate.mjs';
import { verifyDshUpgrader } from './lib/dsh-upgrader-acceptance.mjs';
import { verifyDshHealth } from './lib/dsh-health-acceptance.mjs';

const root = dirname(fileURLToPath(import.meta.url)).replace(/\/scripts$/, '');
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_POPULAR_DSH_PORT || 38942), cdpPort = Number(process.env.MAGI_POPULAR_DSH_CDP_PORT || 9312), origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-popular-dsh-'));
const output = process.env.MAGI_DSH_ACCEPTANCE_OUTPUT || join(root, 'target/plugin-acceptance/popular-dsh');
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
async function removePlugin(id) {
  const installed = (await api('/api/plugins')).value.plugins.find(plugin => plugin.manifest.id === id);
  if (!installed) return;
  if (installed.active) await post(`/api/plugins/${encodeURIComponent(id)}/deactivate`, {});
  if (installed.enabled) await post(`/api/plugins/${encodeURIComponent(id)}/disable`, {});
  await post(`/api/plugins/${encodeURIComponent(id)}/uninstall`, {});
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
  for (const [pkg, version] of candidates) {
    const item = { package: pkg, version, preview: 'pending', core: 'unverified', upgrade: 'unverified' };
    console.log(`CHECK ${pkg}@${version}`);
    try {
      const previewResult = await api('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: pkg, version });
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
          let targetId;
          try {
            const code = (await call('export', { entries: [{ kind: 'npm', name: 'dsh-plugin-release-probe', version: '0.0.7', enabled: true }] })).code;
            const targetQr = await call('qr', { code });
            const scale = 5, quiet = 4, size = (targetQr.size + quiet * 2) * scale;
            const gray = Buffer.alloc(size * size, 255);
            for (let y = 0; y < targetQr.size; y++) for (let x = 0; x < targetQr.size; x++) {
              if (targetQr.modules[y][x] !== '1') continue;
              for (let dy = 0; dy < scale; dy++) gray.fill(0, ((y + quiet) * scale + dy) * size + (x + quiet) * scale,
                ((y + quiet) * scale + dy) * size + (x + quiet + 1) * scale);
            }
            const scanned = await call('scan', { width: size, height: size, gray: gray.toString('base64') });
            assert.equal(scanned.code, code, '原包必须从二维码图片识别回分享码');
            await call('import', { code, confirm: true, requestId: 'sample-share-install' });
            const target = await currentTarget(); assert.ok(target); targetId = target.manifest.id;
            assert.equal(target.manifest.version, '0.0.7'); assert.equal(target.runtime.state, 'running');
            const disabledCode = (await call('export', { entries: [{ kind: 'npm', name: 'dsh-plugin-release-probe', version: '0.0.7', enabled: false }] })).code;
            await call('import', { code: disabledCode, confirm: true, requestId: 'sample-share-disable' });
            assert.equal((await currentTarget()).enabled, false); assert.equal((await currentTarget()).active, false);
            await call('import', { code, confirm: true, requestId: 'sample-share-enable' });
            assert.equal((await currentTarget()).runtime.state, 'running');
            const newer = (await call('export', { entries: [{ kind: 'npm', name: 'dsh-plugin-release-probe', version: '0.0.8', enabled: true }] })).code;
            const blocked = await post(`/api/plugins/${item.id}/routes`, { method: 'POST', path: '/api/plugin-share/import', body: { code: newer, confirm: true, requestId: 'sample-share-upgrade' } });
            assert.equal(blocked.status, 409); assert.equal(blocked.body.error, 'manual-update-required');
            assert.equal((await currentTarget()).digest, target.digest, '上游拒绝更新不应修改当前目标');
            item.functionalChecks = { export: true, parse: true, qr: true, scan: true, import: true, disable: true, enable: true,
              upstreamManualUpdate: true, githubImport: 'unverified', queueRecovery: 'unverified' };
          } finally {
            const target = await currentTarget();
            if (target) await removePlugin(targetId || target.manifest.id);
          }
        }
        if (pkg === '@99galaxy/dsh-plugin-market' && item.smoke?.ok) {
          const list = async (query) => {
            const result = await post(`/api/plugins/${item.id}/routes`, { method: 'GET', path: '/dsh-plugin-market/list', query });
            assert.equal(result.status, 200, JSON.stringify(result)); assert.equal(result.body.ok, true);
            return result.body.payload;
          };
          const self = await list('only=installed&limit=5000');
          item.marketInventory = self;
          assert.ok(self.items.some(row => row.installedAs === pkg && row.installedVersion === version), `市场自己的已安装版本来自 daemon 目录：${JSON.stringify(self)}`);
          const install = await post(`/api/plugins/${item.id}/routes`, { method: 'POST', path: '/dsh-plugin-market/install',
            headers: { 'content-type': 'application/json' }, body: { install: 'dsh plugin add dsh-plugin-release-probe@0.0.7' } });
          item.marketInstall = install;
          assert.equal(install.status, 200, JSON.stringify(install));
          const target = await currentTarget();
          assert.equal(target?.manifest.version, '0.0.7', '市场安装必须写入 daemon 的真实安装目录');
          assert.equal(target?.runtime?.state, 'running', '市场安装须完成已授权启用');
          const inventory = await list('q=dsh-plugin-release-probe&only=installed');
          assert.equal(inventory.unknownTotal, self.unknownTotal + 1, '原目录未收录的已装包按上游语义显示为版本未知');
          const update = await post(`/api/plugins/${item.id}/routes`, { method: 'POST', path: '/dsh-plugin-market/install',
            headers: { 'content-type': 'application/json' }, body: { install: 'dsh plugin add dsh-plugin-release-probe@0.0.8' } });
          item.marketUpdate = update;
          assert.equal(update.status, 200, JSON.stringify(update));
          assert.equal((await currentTarget()).manifest.version, '0.0.8');
          assert.equal((await currentTarget()).runtime.state, 'running');
          const failed = await post(`/api/plugins/${item.id}/routes`, { method: 'POST', path: '/dsh-plugin-market/install',
            headers: { 'content-type': 'application/json' }, body: { install: 'dsh plugin add dsh-plugin-release-probe@9999.0.0' } });
          assert.equal(failed.body.ok, false, '不存在的原包版本必须向原页面报错');
          assert.equal((await currentTarget()).manifest.version, '0.0.8');
          assert.equal((await currentTarget()).runtime.state, 'running');
          const reload = () => post(`/api/plugins/${item.id}/routes`, { method: 'POST', path: '/dsh-plugin-market/reload',
            headers: { 'content-type': 'application/json' }, body: {} });
          const beforeReload = await currentTarget();
          item.marketReload = await reload();
          assert.equal(item.marketReload.status, 200, JSON.stringify(item.marketReload));
          assert.equal(item.marketReload.body.ok, true);
          assert.deepEqual(item.marketReload.body.warnings, []);
          assert.deepEqual(item.marketReload.body.pending, []);
          assert.equal(item.marketReload.body.restartReason, '');
          const afterReload = await currentTarget();
          assert.equal(afterReload.runtime.generation, beforeReload.runtime.generation, '未变化插件继续使用原运行实例');
          assert.equal(afterReload.digest, beforeReload.digest);
          assert.deepEqual(afterReload.grants, beforeReload.grants);
          await post(`/api/plugins/${target.manifest.id}/deactivate`, {});
          await post(`/api/plugins/${target.manifest.id}/disable`, {});
          const disabled = await reload();
          assert.equal(disabled.status, 200, JSON.stringify(disabled));
          assert.equal(disabled.body.ok, true);
          assert.equal((await currentTarget()).enabled, false, 'profile 重载保留停用意图');
          assert.equal((await currentTarget()).active, false);
          await removePlugin(target.manifest.id);
          await viewCheck(running, running.manifest.contributions.views[0], session, async frame => {
            await waitFor(() => frame.evaluate('Boolean(document.querySelector(".dsxpm-input"))'), '市场原包搜索输入框');
            await frame.evaluate(`(() => {const input=document.querySelector('.dsxpm-input');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,'dsh-plugin-mgr');input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
            await waitFor(() => frame.evaluate(`(() => {const button=[...document.querySelectorAll('button')].find(button=>button.innerText==='搜索');if(!button||button.disabled)return false;button.click();return true;})()`), '市场原包搜索操作可用后提交');
            await waitFor(() => frame.evaluate(`Boolean([...document.querySelectorAll('.dsxpm-card')].find(card=>card.querySelector('.dsxpm-name')?.textContent==='dsh-plugin-mgr'))`), '市场原包搜索结果');
            const managerEntry=(await list('q=dsh-plugin-mgr')).items.find(row=>row.name==='dsh-plugin-mgr');
            assert.ok(managerEntry, '原包目录须提供待安装的管理器条目');
            assert.match(managerEntry.install, /\badd dsh-plugin-mgr$/, '此原包入口未锁版本，应沿在线 latest 安装');
            // 省略版本沿正式预览入口解析 registry latest；不依赖搜索索引
            // 是否已收录该包，也不把过期的社区目录版本当成本次安装版本。
            const expectedManager=(await post('/api/plugins/upstream/preview',{ecosystem:'dsh',package:'dsh-plugin-mgr'})).candidate;
            const expectedManagerVersion=expectedManager.manifest.version;
            await frame.evaluate(`[...document.querySelectorAll('.dsxpm-card')].find(card=>card.querySelector('.dsxpm-name')?.textContent==='dsh-plugin-mgr').querySelector('button').click()`);
            const mgr = await waitFor(async () => {
              const installed = (await api('/api/plugins')).value.plugins.find(plugin=>plugin.source.location===`dsh:dsh-plugin-mgr@${expectedManagerVersion}`);
              return installed?.runtime?.state==='running' ? installed : false;
            }, '原包按钮在线安装 Mgr 并运行', 180_000);
            assert.equal(mgr.digest, expectedManager.digest, '原包未锁版本安装须与同一在线预览的内容身份一致');
            await waitFor(() => frame.evaluate(`Boolean([...document.querySelectorAll('.dsxpm-card')].find(card=>card.querySelector('.dsxpm-name')?.textContent==='dsh-plugin-mgr')?.querySelector('.dsxpm-installed'))`), '市场原包页面显示已安装结果');
            await writeFile(join(output,'market-installed.png'),Buffer.from(await ui.screenshot(),'base64'));
            const updatedList = await list('only=installed&q=dsh-plugin-mgr');
            assert.ok(updatedList.items.some(row=>row.installedAs==='dsh-plugin-mgr'&&row.installedVersion===expectedManagerVersion));
            await frame.evaluate(`window.__magiReloadDocument='before-reload';[...document.querySelectorAll('button')].find(button=>button.innerText==='重载 profile').click()`);
            await waitFor(() => frame.evaluate(`window.__magiReloadDocument===undefined && Boolean(document.querySelector('.dsxpm-input'))`), '原包重载完成并实际刷新文档', 30_000);
            await frame.evaluate(`(() => {const input=document.querySelector('.dsxpm-input');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,'dsh-plugin-mgr');input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
            await frame.evaluate(`[...document.querySelectorAll('button')].find(button=>button.innerText==='搜索').click()`);
            await waitFor(() => frame.evaluate(`Boolean([...document.querySelectorAll('.dsxpm-card')].find(card=>card.querySelector('.dsxpm-name')?.textContent==='dsh-plugin-mgr')?.querySelector('.dsxpm-installed'))`), '重载后新文档的桥和真实安装列表继续可用');
            const currentMgr = (await api('/api/plugins')).value.plugins.find(plugin=>plugin.manifest.id===mgr.manifest.id);
            const currentMarket = (await api('/api/plugins')).value.plugins.find(plugin=>plugin.manifest.id===item.id);
            assert.equal(currentMgr.runtime.generation, mgr.runtime.generation);
            assert.equal(currentMarket.runtime.generation, running.runtime.generation);
            assert.equal(currentMgr.runtime.state, 'running');
            await writeFile(join(output,'market-reloaded.png'),Buffer.from(await ui.screenshot(),'base64'));
            await removePlugin(mgr.manifest.id);
          });
          item.upgrade = 'passed';
          item.functionalChecks = { onlineInstall: true, daemonOwnership: true, activation: true, onlineUpdate: true, installedInventory: true,
            originalPageInstall: true, failedInstallPreservesVersion: true, gitSources: 'unverified', profileReload: true,
            unchangedInstances: true, disabledStateAfterReload: true, originalPageReload: true, bridgeAfterReload: true };
        }
        if (pkg === 'dsh-plugin-mgr' && item.smoke?.ok) {
          const target = await installTarget();
          const call = async (action, body) => {
            const result = await post(`/api/plugins/${item.id}/routes`, { method: body === undefined ? 'GET' : 'POST', path: `/api/plugin-manager/${action}`, ...(body === undefined ? {} : { body }) });
            assert.equal(result.status, 200, JSON.stringify(result)); assert.equal(result.body.ok, true, JSON.stringify(result)); return result.body;
          };
          try {
            item.daemonPluginsBeforeManagerRoute = (await api('/api/plugins')).value.plugins
              .filter((entry) => entry.source?.kind === 'upstream').map((entry) => entry.source.location);
            const row = (await call('list')).plugins.find((entry) => entry.name === 'dsh-plugin-release-probe');
            assert.equal(row.version, '0.0.7'); assert.equal(row.disabled, false); assert.ok(row.rows.includes(target.manifest.id));
            await call('toggle', { name: row.name, disable: true });
            assert.equal((await currentTarget()).enabled, false); assert.equal((await currentTarget()).active, false);
            await call('toggle', { name: row.name, disable: false });
            assert.equal((await currentTarget()).runtime.state, 'running');
            const update = (await call('updates')).updates;
            item.updateCheck = update;
            assert.equal(update?.[row.name]?.latest, '0.0.8', '原包须从 registry 检查到目标新版');
            await call('update', { name: row.name });
            const after = await currentTarget();
            assert.equal(after.manifest.version, '0.0.8'); assert.equal(after.runtime.state, 'running');
            await call('uninstall', { name: row.name }); assert.equal(await currentTarget(), undefined);
            item.functionalChecks = { list: true, disable: true, enable: true, checkUpdates: true, onlineUpgrade: true, uninstall: true, runtimeFailureDisplay: 'unverified' };
            item.upgrade = 'passed';
          } finally { const target = await currentTarget(); if (target) await removePlugin(target.manifest.id); }
        }
        if (pkg === 'dsh-plugin-upgrader' && process.env.MAGI_DSH_FUNCTIONAL_UPGRADE === '1') {
          item.businessAttempted = true;
          const business = await verifyDshUpgrader({ request: post, origin, plugin: running, page, cdpPort, errors,
            gate: registryGate, output, stateRoot: join(work, 'state'),
            check: (name, ok, detail) => { assert.ok(ok, `${name}: ${JSON.stringify(detail ?? '')}`); console.log(`✓ ${name}`); },
            restart: () => restart('before-upgrader-restart.log'),
          });
          page = business.page; ui = pageTools(page);
          item.functionalChecks = business.checks;
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
      await removePlugin(item.id);
      item.cleaned = true;
    } catch (error) {
      if (item.marketInstall || item.businessAttempted) item.core = 'failed';
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
