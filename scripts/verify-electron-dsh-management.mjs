// 真实目录包 + npm/GitHub 原包。Share 的页面与业务不修改；管理服务夹具只验证
// 无上游升级页面覆盖的 RPC/授权/摘要/卸载机制，不计入生态兼容率。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'node:http';
import { Readable } from 'node:stream';
import { packPlugin } from './compat/core/pack.mjs';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_DSH_MANAGEMENT_PORT || 38944), cdpPort = Number(process.env.MAGI_DSH_MANAGEMENT_CDP_PORT || 9314);
const origin = `http://127.0.0.1:${port}`;
const output = process.env.MAGI_DSH_MANAGEMENT_OUTPUT || join(root, 'target/plugin-acceptance/dsh-management');
const work = await mkdtemp(join(tmpdir(), 'magi-dsh-management-'));
const launchOptions = { executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_NPM_REGISTRY: '', MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } };
const report = createReporter(), errors = [];
const clipAuditSource = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE, 'utf8') : null;
let app, page, frame, ui;
// 转发真实 npm 元数据，仅暂停指定安装任务的解析请求来确定性验证取消。
// 原包、integrity、页面及业务均不替换；不影响用户正在运行的实例。
let holdResolution = false;
const held = new Set();
const registryProxy = createServer(async (request, response) => {
  if (holdResolution && request.headers['pacote-pkg-id'] === `registry:${targetPackage}`) {
    held.add(response);
    response.on('close', () => held.delete(response));
    return;
  }
  try {
    const upstream = await fetch(`https://registry.npmjs.org${request.url}`, {
      headers: { accept: request.headers.accept ?? 'application/json' }, signal: AbortSignal.timeout(120_000),
    });
    response.writeHead(upstream.status, { 'content-type': upstream.headers.get('content-type') ?? 'application/json' });
    Readable.fromWeb(upstream.body).on('error', () => response.destroy()).pipe(response);
  } catch (error) { response.writeHead(502); response.end(String(error)); }
});
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
  return api(`/api/plugins/${id}/routes`, { method: body === undefined ? 'GET' : 'POST', path, ...(body === undefined ? {} : { body }) });
}
async function screenshot(name) { await writeFile(join(output, name + '.png'), Buffer.from(await ui.screenshot(), 'base64')); }
async function openSettings(plugin) {
  frame?.close();
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '设置入口');
  await ui.click('[data-testid=sidebar-settings]');
  const view = plugin.manifest.contributions.views.find((v) => v.placements.includes('settings'));
  const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${plugin.manifest.id}:${view.id}`)}]`;
  await waitFor(() => ui.exists(nav), '原生页面入口'); await ui.click(nav);
  const selector = `iframe[data-plugin-id=${JSON.stringify(plugin.manifest.id)}]`;
  await waitFor(() => ui.exists(selector), '原包页面');
  const frameUrl = await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).src`);
  frame = await waitFor(() => connectTarget(cdpPort, (t) => (t.type === 'iframe' || t.type === 'page') && t.url === frameUrl, '原生页面', errors), '原包 iframe');
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
  await new Promise((resolve) => registryProxy.listen(0, '127.0.0.1', resolve));
  const registry = `http://127.0.0.1:${registryProxy.address().port}`;
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口已占用');
  launchOptions.env.MAGI_NPM_REGISTRY = registry;
  app = launchApp(launchOptions);
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
  const manage = await installFixture();
  // GitHub 来源使用同一原包管理任务，页面、来源回执与重启恢复均实际验证。
  const health = async () => (await plugins()).find(p => p.source.location.startsWith('dsh:dsh-plugin-health@'));
  const githubSpec = 'github:easonmws/dsh-plugin-health#v3.0.5';
  const gitCode = await shareCall('export', { entries: [{ kind: 'github', spec: githubSpec, version: '3.0.5', enabled: false }] });
  await openSettings(share);
  await waitFor(() => frame.evaluate('Boolean(document.querySelector("textarea.dps-code"))'), 'GitHub 组合码输入');
  await frame.evaluate(`(() => {const el=document.querySelector('textarea.dps-code');Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(el,${JSON.stringify(gitCode.code)});el.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  await waitFor(() => clickText('解析组合码'), '解析 GitHub 组合码');
  await waitFor(() => clickText('全部可处理项加入队列'), 'Share 原页面 GitHub 安装');
  const gitOutcome = await waitFor(async () => {
    const installed=await health(); if(installed) return { installed };
    const failed=await frame.evaluate('document.body.innerText.includes("队列已暂停") ? document.body.innerText : null');
    if(failed) return { failed };
    return false;
  }, 'GitHub 原包安装提交', 300_000);
  if (gitOutcome.failed) { await writeFile(join(output, 'github-install-failure.txt'), gitOutcome.failed); throw new Error(gitOutcome.failed); }
  const gitInstalled = gitOutcome.installed;
  await waitFor(() => frame.evaluate('document.body.innerText.includes("项完成")'), 'GitHub 安装完成回执', 60_000);
  report.check('Share 原页面从 GitHub 安装原包并固定提交，保持禁用且不代授权',
    gitInstalled.manifest.version === '3.0.5' && !gitInstalled.active && !gitInstalled.enabled && gitInstalled.grants.length === 0
      && gitInstalled.source.github?.commit === '8e1d4366fb71012b82a7c2bc476398a764eab76c');
  const collected = await shareCall('state');
  report.check('原包导出与管理目录保留 GitHub 来源及真实安装版本',
    collected.bundles.some(b => b.name === 'dsh-plugin-health' && b.source === githubSpec && b.version === '3.0.5')
      && collected.entries.some(e => e.kind === 'github' && e.value === githubSpec));
  await screenshot('share-github-installed');
  await authorize(gitInstalled);
  const beforeGitUpdate = await health();
  const wrongSource = await manage('preview', { ecosystem: 'dsh', package: 'dsh-plugin-health', version: '3.0.5' });
  report.check('npm 同名包不能冒用 GitHub 更新来源，失败保留运行实例',
    !wrongSource.ok && (await health()).digest === beforeGitUpdate.digest && (await health()).runtime.generation === beforeGitUpdate.runtime.generation);
  frame.close(); frame = undefined;
  await ui.click('[data-testid=settings-nav-plugins]');
  await waitFor(() => ui.exists('[data-testid=plugin-sources-open]'), '来源入口');
  await ui.click('[data-testid=plugin-sources-open]');
  await page.evaluate(`(() => { const el=document.querySelector('[data-testid=plugin-address-input]');el.value='github:easonmws/dsh-plugin-health#v3.0.6';el.dispatchEvent(new Event('input',{bubbles:true})); })()`);
  await ui.click('[data-testid=plugin-address-install]');
  await waitFor(() => ui.exists('[data-testid=plugin-candidate-github]'), 'GitHub 在线升级预览', 300_000);
  report.check('原生在线升级展示固定提交并走原确认面板',
    await page.evaluate('document.querySelector("[data-testid=plugin-candidate]").dataset.operation === "upgrade" && document.querySelector("[data-testid=plugin-candidate-github]").innerText.includes("f2b7388a131916dc2c5427652c8ab450a24838ce")'));
  await screenshot('github-update-preview');
  if (clipAuditSource) {
    const audit = await page.evaluate(`(${clipAuditSource})({viewport:true,root:'.store-detail'})`);
    await writeFile(join(output, 'github-preview-clip.json'), JSON.stringify(audit, null, 2));
    report.check('GitHub 更新面板阴影无裁切', audit.cut === 0, JSON.stringify(audit));
  }
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  const narrowLayout = await page.evaluate(`(() => {
    const button=document.querySelector('[data-action=confirm-candidate]'), candidate=document.querySelector('[data-testid=plugin-candidate]');
    const b=button.getBoundingClientRect();return { left:b.left, right:b.right, top:b.top, bottom:b.bottom, width:innerWidth, height:innerHeight, scrollWidth:candidate.scrollWidth, clientWidth:candidate.clientWidth };
  })()`);
  report.check('390px GitHub 升级确认保持固定操作可达且无横向溢出', narrowLayout.left>=0 && narrowLayout.right<=narrowLayout.width && narrowLayout.top>=0 && narrowLayout.bottom<=narrowLayout.height && narrowLayout.scrollWidth<=narrowLayout.clientWidth, JSON.stringify(narrowLayout));
  await screenshot('github-update-preview-390');
  await page.call('Emulation.clearDeviceMetricsOverride');
  await ui.click('[data-action=confirm-candidate]');
  await waitFor(async () => (await health())?.manifest.version === '3.0.6', 'GitHub 在线升级提交', 300_000);
  const gitUpdated = await health();
  report.check('GitHub 升级沿原生命周期替换版本并要求新摘要授权',
    gitUpdated.source.github?.reference === 'v3.0.6' && !gitUpdated.active && gitUpdated.grants.length === 0);
  await authorize(gitUpdated);
  await waitFor(() => ui.exists(`[data-testid=plugin-row][data-plugin-id="${gitUpdated.manifest.id}"]`), '已安装 GitHub 条目');
  await ui.click(`[data-testid=plugin-row][data-plugin-id="${gitUpdated.manifest.id}"]`);
  await ui.click('[data-testid=plugin-detail-update]');
  await waitFor(() => page.evaluate('/无需安装|nothing to install/.test(document.querySelector("[data-testid=plugin-message]")?.innerText ?? "")'), '固定标签在线检查回执', 300_000);
  const badGit = await manage('preview', { ecosystem: 'dsh', github: { repository: 'easonmws/dsh-plugin-health', reference: 'magi-missing-revision-acceptance' } });
  report.check('GitHub 版本不存在时保留原授权和运行摘要', !badGit.ok && (await health()).digest === gitUpdated.digest && (await health()).active);
  await screenshot('github-current');
  page.close(); await writeFile(join(output, 'electron-before-restart.log'), app.log.join('')); await app.stop();
  app = launchApp(launchOptions);
  await waitFor(() => api('/health'), '重启 daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '重启 Electron'); ui = pageTools(page);
  const recovered = await waitFor(async () => { const p=await health(); return p?.runtime?.state === 'running' ? p : false; }, 'GitHub 原包重启恢复', 120_000);
  report.check('重启从原安装事实恢复 GitHub 提交、授权与真实宿主', recovered.digest === gitUpdated.digest && recovered.source.github.commit === gitUpdated.source.github.commit && recovered.active);
  await waitFor(async () => (await plugins()).find(p => p.manifest.id === 'magi.management-test')?.runtime?.state === 'running', '管理宿主恢复');
  const gitRemoved = await manage('remove', { pluginId: recovered.manifest.id });
  report.check('GitHub 原包卸载沿原排空与移除入口完成', gitRemoved.ok && !await health());
  const market = await installUpstream('@99galaxy/dsh-plugin-market', '1.2.2'); await authorize(market);
  const marketInstall = async spec => api(`/api/plugins/${market.manifest.id}/routes`, {
    method: 'POST', path: '/dsh-plugin-market/install', headers: { 'content-type': 'application/json' }, body: { install: `dsh plugin add ${spec}` },
  });
  const marketGit = await marketInstall(githubSpec);
  const marketTarget = await health();
  report.check('Market 原包 Node 辅助进程实际安装 GitHub 原包并完成授权运行', marketGit.status === 200 && marketGit.body.ok && marketTarget?.manifest.version === '3.0.5' && marketTarget.runtime?.state === 'running', JSON.stringify(marketGit));
  const marketUpdate = await marketInstall('git+https://github.com/easonmws/dsh-plugin-health.git#v3.0.6');
  const marketUpdated = await health();
  report.check('Market 原包通过 HTTPS Git 来源在线更新，当前目录与运行版本一致', marketUpdate.status === 200 && marketUpdate.body.ok && marketUpdated?.manifest.version === '3.0.6' && marketUpdated.runtime?.state === 'running' && marketUpdated.source.github?.reference === 'v3.0.6', JSON.stringify(marketUpdate));
  const marketMissing = await marketInstall('github:easonmws/dsh-plugin-health#magi-missing-revision-acceptance');
  report.check('Market Git 失败回执保留旧版、授权与运行实例', marketMissing.body.ok === false && (await health()).digest === marketUpdated.digest && (await health()).runtime.generation === marketUpdated.runtime.generation, JSON.stringify(marketMissing));
  const marketRemoved = await manage('remove', { pluginId: marketUpdated.manifest.id });
  report.check('Market 安装的 GitHub 原包仍从同一卸载入口排空移除', marketRemoved.ok && !await health());
  const exported = await shareCall('export', { entries: [{ kind: 'npm', name: targetPackage, version: '0.0.7', enabled: false }] });
  const parsed = await shareCall('parse', { code: exported.code });
  const qr = await shareCall('qr', { code: exported.code });
  report.check('原包业务生成组合码、解析并生成二维码', parsed.preview[0].action === 'install' && qr.modules.length > 0, JSON.stringify({parsed,qr}));
  await openSettings(share);
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
  const upgrader = await installUpstream('dsh-plugin-upgrader', '1.8.0'); await authorize(upgrader);
  const original = await installUpstream(targetPackage, '0.0.7'); await authorize(original);
  const beforeCancel = await target();
  await openSettings(upgrader);
  await page.evaluate(`(() => { globalThis.__installationTrace=[]; window.addEventListener('message', event => {const m=event.data;if(m?.channel==='magi-plugin-v1' && m.operation==='route.call') {globalThis.__installationTrace.push(m);if(globalThis.__installationTrace.length>100)globalThis.__installationTrace.shift();}}); })()`);
  await frame.evaluate(`(() => { globalThis.__installationTrace=[]; window.addEventListener('message', event => {const m=event.data;if(m?.channel==='magi-plugin-v1') {globalThis.__installationTrace.push(m);if(globalThis.__installationTrace.length>100)globalThis.__installationTrace.shift();}}); })()`);
  const upgradeButton = `(() => [...document.querySelectorAll('.dsh-pu-row')].find(el => el.innerText.includes(${JSON.stringify(targetPackage)}))?.querySelector('.dsh-pu-row-actions button.is-primary'))()`;
  await waitFor(() => frame.evaluate(`Boolean(${upgradeButton})`), 'Upgrader 原包目标升级按钮', 120_000);
  holdResolution = true;
  await frame.evaluate(`${upgradeButton}.click()`);
  await waitFor(() => held.size > 0, 'daemon 在线安装版本解析请求', 30_000);
  const running = await callRoute(upgrader.manifest.id, '/api/plugin-upgrader/overview');
  const jobId = running.body.active.id;
  await waitFor(() => frame.evaluate(`(() => {const button=[...document.querySelectorAll('.dsh-pu-job button')].find(el => /^(取消|Cancel)$/.test(el.innerText.trim()) && !el.disabled);if (!button) return false;button.click();return true;})()`), '原包取消按钮');
  await waitFor(() => held.size === 0, '取消回收解析子进程连接');
  const cancellation = await waitFor(async () => {
    const result = await api(`/api/plugins/${upgrader.manifest.id}/routes`, { method: 'GET', path: '/api/plugin-upgrader/job', query: `id=${jobId}` });
    return result.body.status === 'cancelled' ? result.body : false;
  }, '原包取消结算');
  const afterCancel = await target();
  report.check('原包升级及取消按钮真正停止当前 daemon 安装，保留旧版本、授权和运行实例',
    cancellation.results[0]?.application === 'cancelled'
      && afterCancel.digest === beforeCancel.digest && afterCancel.runtime.state === 'running'
      && JSON.stringify(afterCancel.grants) === JSON.stringify(beforeCancel.grants)
      && afterCancel.runtime.generation === beforeCancel.runtime.generation, JSON.stringify({ cancellation, beforeCancel, afterCancel }));
  holdResolution = false;
  await waitFor(() => frame.evaluate(`Boolean(${upgradeButton} && !${upgradeButton}.disabled)`), '取消后恢复升级按钮');
  await screenshot('upgrader-cancelled');
  await frame.evaluate(`${upgradeButton}.click()`);
  await waitFor(async () => { const plugin = await target(); return plugin.manifest.version === '0.0.8' && plugin.runtime?.state === 'running'; }, '原包显式重试在线升级并运行', 240_000);
  await waitFor(() => frame.evaluate(`!${upgradeButton} && ![...document.querySelectorAll('.dsh-pu-job button')].some(el => /^(取消|Cancel|正在取消…|Cancelling…)$/.test(el.innerText.trim()))`), '原包页面完成回执与新版列表');
  report.check('取消后从原包按钮显式重试，在线安装新版并恢复运行', (await target()).active);
  report.check('其他插件安装状态变化保留当前页面消息桥身份和完成回执',
    await frame.evaluate(`new Set(globalThis.__installationTrace.map(m => m.instanceId)).size === 1`));
  await screenshot('upgrader-retried');
  report.check('Electron 与原包页面无未捕获异常', errors.length === 0, errors.join('\n'));
  console.log(report.summary());
} catch (error) {
  report.results.push({ name: '真实管理验收完成', ok: false, detail: error.stack ?? String(error) });
  console.error(error); process.exitCode = 1;
  if (ui) await screenshot('failure').catch(() => {});
} finally {
  await writeFile(join(output, 'route-trace.json'), JSON.stringify({ parent: await page?.evaluate('globalThis.__installationTrace').catch(() => null), frame: await frame?.evaluate('globalThis.__installationTrace').catch(() => null) }, null, 2));
  await writeFile(join(output, 'results.json'), JSON.stringify({ checks: report.results, errors, stateRoot: join(work, 'state'), scope: 'Share/Upgrader 原包管理，npm/GitHub 在线安装、升级与恢复；不代表生态全集业务覆盖率' }, null, 2));
  frame?.close(); page?.close();
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  registryProxy.closeAllConnections();
  await new Promise((resolve) => registryProxy.close(resolve));
}
