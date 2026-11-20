/** 打包 Electron 验收 Mgr 固定原包。registry 暂停真实 npm 依赖响应，不替换发布字节。 */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { acceptanceHostPid, installFixedUpstreamFromNative, previewFixedUpstreamFromNative, upgradePluginFromNative } from './lib/plugin-native-acceptance.mjs';
import { createPluginRegistryGate } from './lib/plugin-registry-gate.mjs';

async function freePort() {
  const socket = createServer();
  await new Promise((resolve, reject) => socket.once('error', reject).listen(0, '127.0.0.1', resolve));
  const { port } = socket.address();
  await new Promise((resolve, reject) => socket.close(error => error ? reject(error) : resolve()));
  return port;
}

const appRoot = resolve(process.env.MAGI_PLUGIN_TEST_APP ?? 'target/electron-dist/mac-arm64/Magi.app');
const output = resolve(process.env.MAGI_DSH_MANAGEMENT_OUTPUT ?? 'target/plugin-acceptance/dsh-management');
const baseline = JSON.parse(await readFile(new URL('../docs/plugin-validation/phase-1/manifest.json', import.meta.url)));
const packageName = 'dsh-plugin-mgr';
const samples = baseline.packages.filter(row => row.name === packageName);
const targetPackage = 'dsh-free-search';
const targetBefore = baseline.packages.find(row => row.name === targetPackage);
// Mgr 本身只提供更新到 latest；变化后要重新锁定验收基线，不跟着在线结果漂移。
const targetAfter = {
  name: targetPackage, version: '0.9.70',
  sha256: 'd856dd1719dcb1768d1838f6b29faf720a351849061b2496c09a7b8da1c662cd',
  integrity: 'sha512-kTqL6JAK1CnRkQFFddli2kyh8exk9B7841cwHruFfP7k5qMvVfPVbxBq8Eo6On7lPc8+nxyQ3oeNspFw3n+CuQ==',
};
const state = await mkdtemp(join(tmpdir(), 'magi-dsh-upgrade-'));
const daemonPort = await freePort();
const cdpPort = await freePort();
const gate = await createPluginRegistryGate();
// Mgr 的"更新到 latest"以锁定基线为准：上游发布新版本不改变这条验收的目标。
gate.pinLatest(targetPackage, targetAfter.version);
const resources = join(appRoot, 'Contents/Resources');
const result = { scope: 'macOS arm64 Electron directory package, Mgr native settings', samples: [], checks: {} };
const launch = () => launchApp({
  executable: join(appRoot, 'Contents/MacOS/Magi'),
  cwd: state,
  cdpPort,
  env: {
    MAGI_STATE_ROOT: state,
    MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
    MAGI_DESKTOP_REUSE_DAEMON: '',
    MAGI_EXTENSION_NODE: '',
    MAGI_DSH_SDK_ROOT: '',
    MAGI_NPM_REGISTRY: gate.registry,
    DSH_PLUGIN_MGR_REGISTRY: gate.registry,
    npm_config_cache: join(state, 'npm-cache'),
    npm_config_prefer_online: 'true',
  },
});
let app = launch();
await mkdir(output, { recursive: true });
let page, frame;
try {
  page = await waitFor(async () => {
    const candidate = await connectAppPage(cdpPort, daemonPort, []);
    if (await pageTools(candidate).exists('[data-testid="sidebar-settings"]')) return candidate;
    candidate.close();
    return false;
  }, 'Electron 设置入口', 75_000);
  const ui = pageTools(page);
  const origin = `http://127.0.0.1:${daemonPort}`;
  const digest = bytes => createHash('sha256').update(bytes).digest('hex');
  const installation = async label => {
    const { loading: { configuration } } = JSON.parse(await readFile(join(state, 'plugins', 'state.json')));
    const path = join(configuration.preparation.projectDirectory, 'package-lock.json');
    const lock = await readFile(path);
    if (!label) return { revision: configuration.revision, dependenciesSha256: digest(lock) };
    const dependencyLock = label + '-package-lock.json';
    await writeFile(join(output, dependencyLock), lock);
    return { revision: configuration.revision, dependenciesSha256: digest(lock), dependencyLock };
  };
  const lockTarget = async sample => {
    const { status, body } = await ui.post('/api/plugins/upstream/preview', {
      ecosystem: 'dsh', package: sample.name, version: sample.version,
    });
    assert.equal(status, 200, body);
    const { candidate, integrity } = JSON.parse(body);
    assert.equal(candidate.digest, sample.sha256);
    assert.equal(integrity, sample.integrity);
  };
  const request = async path => {
    const response = await fetch(origin + path, { signal: AbortSignal.timeout(10_000) });
    assert.equal(response.status, 200);
    return response.json();
  };
  const getMgr = async () => (await request('/api/plugins')).plugins.find(row => row.manifest.id === packageName);
  const preview = async version => {
    const { status, body } = await ui.post('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: packageName, version });
    assert.equal(status, 200, body);
    const { candidate, integrity } = JSON.parse(body);
    const pinned = samples.find(row => row.version === version);
    assert.equal(candidate.digest, pinned.sha256);
    assert.equal(integrity, pinned.integrity);
    result.samples.push({ name: packageName, version, sha256: candidate.digest, integrity });
    return candidate;
  };
  await ui.click('[data-testid="sidebar-settings"]');
  await waitFor(() => ui.exists('[data-testid="settings-nav-plugins"]'), '插件设置导航');
  await ui.click('[data-testid="settings-nav-plugins"]');
  const search = '[data-testid="plugin-search"]';
  await waitFor(() => ui.exists(search), '插件搜索');
  await page.evaluate(`(() => { const field = document.querySelector(${JSON.stringify(search)});
    field.value = ${JSON.stringify(packageName)}; field.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  const row = `[data-testid="plugin-row"][data-plugin-id="upstream:dsh:${packageName}"]`;
  await waitFor(() => ui.exists(row), '固定原包进入在线搜索结果', 60_000);
  await ui.click(row);
  await waitFor(() => ui.exists('[data-testid=plugin-detail-install]'), '原包安装入口', 60_000);
  await preview('0.2.12');
  await previewFixedUpstreamFromNative({ page, packageName, version: '0.2.12' });
  const confirm = '[data-action=confirm-candidate]';
  await waitFor(() => ui.exists(confirm), '固定版本预览');
  await ui.click(confirm);
  const before = await waitFor(async () => {
    const mgr = await getMgr();
    return mgr?.manifest.version === '0.2.12' && mgr.runtime?.state === 'running' && mgr;
  }, '从设置页安装 Mgr 0.2.12', 240_000);
  console.log('设置页安装 Mgr 0.2.12 通过');
  const firstInstallation = await installation();
  Object.assign(result.samples[0], await installation('mgr-0.2.12'));
  result.checks.initialInstallFromSettings = true;
  const candidate = await preview('0.2.13');
  assert.equal(candidate.operation, 'upgrade');

  const bad = await ui.post('/api/plugins/upstream/install', {
    ecosystem: 'dsh', package: packageName, version: '0.2.13', expectedDigest: '0'.repeat(64),
  });
  assert.equal(bad.status, 409, bad.body);
  assert.equal((await getMgr()).runtime.instanceId, before.runtime.instanceId);
  result.checks.wrongDigestRejected = true;

  const update = '[data-testid="plugin-detail-update"]';
  const cancel = '[data-testid="plugin-cancel-install"]';
  const projectRoot = join(state, 'plugins', 'projects');
  const beforeProjects = (await readdir(projectRoot)).sort();
  await waitFor(() => ui.exists(update), '原生在线更新入口');
  gate.pause('js-yaml');
  await ui.click(update);
  await waitFor(() => gate.held.size > 0, '新版实际进入 npm 依赖解析', 45_000);
  console.log('新版已进入 npm 依赖解析，开始取消');
  await waitFor(() => ui.exists(cancel), '进行中的升级可取消');
  const stagedProjects = (await readdir(projectRoot)).filter(name => !beforeProjects.includes(name));
  assert.equal(stagedProjects.length, 1, '只有一个新版暂存项目');
  await writeFile(join(output, 'upgrade-in-progress.png'), Buffer.from(await ui.screenshot(), 'base64'));
  await ui.click(cancel);
  await waitFor(async () => /已取消安装|Installation cancelled/.test(await ui.text('[data-testid="plugin-message"]')),
    '设置页显示 daemon 已取消升级', 20_000);
  await waitFor(() => gate.held.size === 0, '被取消的 npm 连接关闭', 10_000);
  assert.deepEqual((await readdir(projectRoot)).sort(), beforeProjects);
  gate.release();
  const retained = await getMgr();
  assert.equal(retained.manifest.version, before.manifest.version);
  assert.equal(retained.digest, before.digest);
  assert.equal(retained.runtime.instanceId, before.runtime.instanceId);
  assert.equal(retained.enabled, before.enabled);
  assert.deepEqual(await installation(), firstInstallation);
  result.checks.cancelledDuringNpm = { connectionClosed: true, stagingRemoved: true, retainedVersion: retained.manifest.version, sameInstance: true };
  console.log('取消后保留旧版运行实例，重试在线升级');

  const upgraded = await upgradePluginFromNative({
    request, page, plugin: retained, version: '0.2.13',
    check(id, label, ok) { assert.ok(ok, label); result.checks[id] = true; },
  });
  Object.assign(result.samples[1], await installation('mgr-0.2.13'));
  const list = await ui.post(`/api/plugins/${packageName}/routes`, {
    method: 'GET', path: '/api/plugin-manager/list',
    expectedDigest: upgraded.digest, expectedInstanceId: upgraded.runtime.instanceId,
  });
  assert.equal(list.status, 200, list.body);
  assert.equal(JSON.parse(list.body).status, 200, list.body);
  result.checks.originalListReturned = true;
  const stale = await ui.post(`/api/plugins/${packageName}/routes`, {
    method: 'GET', path: '/api/plugin-manager/list',
    expectedDigest: before.digest, expectedInstanceId: before.runtime.instanceId,
  });
  assert.equal(stale.status, 409);
  result.checks.oldGatewayStatus = stale.status;

  await lockTarget(targetBefore);
  await installFixedUpstreamFromNative({ page, request, packageName: targetPackage, version: targetBefore.version });
  await lockTarget(targetAfter);
  result.targetSamples = [targetBefore, targetAfter].map(({ name, version, sha256, integrity }) => ({ name, version, sha256, integrity }));
  Object.assign(result.targetSamples[0], await installation('target-' + targetBefore.version));

  // Hold the original Mgr update query at the registry before cutting the old lease.
  gate.pause(targetPackage + '/latest');
  const pendingUpdates = ui.post(`/api/plugins/${packageName}/routes`, {
    method: 'GET', path: '/api/plugin-manager/updates',
    expectedDigest: upgraded.digest, expectedInstanceId: upgraded.runtime.instanceId,
  }).then(response => ({ response }), error => ({ error }));
  let interrupted;
  try {
    await waitFor(() => gate.held.size > 0, 'Mgr 原包更新查询正在等待 registry', 10_000);
    process.kill(acceptanceHostPid(app.child.pid), 'SIGKILL');
    const outcome = await pendingUpdates;
    if (outcome.error) throw outcome.error;
    interrupted = outcome.response;
    assert.equal(interrupted.status, 409, interrupted.body);
    await waitFor(() => gate.held.size === 0, '原包在途更新查询的 registry 连接关闭', 10_000);
  } finally {
    gate.release();
  }
  const recovered = await waitFor(async () => {
    const current = await getMgr();
    return current?.runtime?.state === 'running'
      && current.runtime.instanceId !== upgraded.runtime.instanceId
      && current;
  }, 'Mgr 宿主崩溃后恢复新实例', 90_000);
  const staleAfterCrash = await ui.post(`/api/plugins/${packageName}/routes`, {
    method: 'GET', path: '/api/plugin-manager/list',
    expectedDigest: upgraded.digest, expectedInstanceId: upgraded.runtime.instanceId,
  });
  assert.equal(staleAfterCrash.status, 409);
  const liveAfterCrash = await ui.post(`/api/plugins/${packageName}/routes`, {
    method: 'GET', path: '/api/plugin-manager/list',
    expectedDigest: recovered.digest, expectedInstanceId: recovered.runtime.instanceId,
  });
  assert.equal(liveAfterCrash.status, 200, liveAfterCrash.body);
  result.checks.hostCrash = {
    oldRouteStatus: staleAfterCrash.status,
    inFlightRouteStatus: interrupted.status,
    newRouteStatus: liveAfterCrash.status,
    recoveredVersion: recovered.manifest.version,
    newInstance: recovered.runtime.instanceId,
  };

  const configPath = join(state, 'plugins', 'loading-source', 'cordis.patch.yml');
  const lastConfig = await readFile(configPath);
  const invalidConfig = '- id: dsh-plugin-mgr\n  disabled: [\n';
  await writeFile(configPath, invalidConfig);
  const configIssue = '[data-testid="plugin-configuration-error"]';
  try {
    await waitFor(() => ui.exists(configIssue), '设置页显示真实文件拒绝原因', 20_000);
    const rejected = await ui.post(`/api/plugins/${packageName}/disable`, { expectedDigest: upgraded.digest });
    assert.equal(rejected.status, 409, rejected.body);
    assert.equal(await readFile(configPath, 'utf8'), invalidConfig);
    assert.equal((await getMgr()).runtime.instanceId, recovered.runtime.instanceId);
    result.checks.invalidConfig = { reportedInSettings: true, mutationStatus: rejected.status, retainedRunningInstance: true };
  } finally {
    await writeFile(configPath, lastConfig);
  }
  await waitFor(async () => !(await ui.exists(configIssue)), '修复来源后设置页告警消失', 20_000);
  assert.equal((await getMgr()).runtime.instanceId, recovered.runtime.instanceId);
  result.checks.fileRepairRestored = true;

  // Return to the list after the file-repair assertions.
  const settingsTab = '[data-testid="settings-nav-plugin:dsh-plugin-mgr:settings"]';
  await waitFor(() => ui.exists(settingsTab), 'Mgr 独立设置标签');
  await ui.click(settingsTab);
  const src = await waitFor(() => page.evaluate('document.querySelector(\'iframe[data-plugin-id="dsh-plugin-mgr"]\')?.src'), 'Mgr 设置页帧');
  frame = await waitFor(() => connectTarget(cdpPort, target => target.url === src, 'Mgr 设置页帧'), '原包设置目标');
  const manager = pageTools(frame);
  const card = `.dshpm-card:has(.dshpm-name[title="${targetPackage}"])`;
  const targetPlugin = async () => (await request('/api/plugins')).plugins.find(row => row.manifest.id === targetPackage);
  const toggle = card + ' [role="switch"][aria-disabled="false"]';
  await waitFor(() => frame.evaluate(`(() => {
    const button = [...document.querySelectorAll('.dshpm-root button')].find(node => /^(刷新|Refresh)$/.test(node.innerText.trim()));
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`), '刷新原包管理列表');
  await waitFor(() => manager.exists(toggle), '原页面显示真实管理列表');
  const original = await targetPlugin();
  await manager.click(toggle);
  await waitFor(async () => (await targetPlugin())?.enabled === false
    && await manager.exists(toggle + '[aria-checked="false"]'), '原页面停用目标');
  await manager.click(toggle);
  const resumed = await waitFor(async () => {
    const plugin = await targetPlugin();
    return plugin?.enabled && plugin.runtime.state === 'running' && plugin;
  }, '原页面恢复目标');
  assert.notEqual(resumed.runtime.instanceId, original.runtime.instanceId);
  result.checks.originalPageToggle = true;

  const expand = async () => {
    const arrow = card + ' .dshpm-arrow[aria-expanded="false"]';
    if (await manager.exists(arrow)) await manager.click(arrow);
  };
  const confirmAction = async pattern => await waitFor(() => frame.evaluate(`(() => {
    const buttons = [...document.querySelectorAll('[role="dialog"] button, dialog[open] button')];
    const button = buttons.find(node => !node.disabled && new RegExp(${JSON.stringify(pattern)}, 'i').test(node.innerText.trim()));
    if (!button) return false;
    button.click(); return true;
  })()`), '原页面确认操作');
  await expand();
  await waitFor(() => frame.evaluate(`document.querySelector(${JSON.stringify(card + ' .dshpm-pill.is-update')})?.title.includes(${JSON.stringify(targetAfter.version)})`),
    '原页面更新检测返回锁定的最新版本');
  const updateTarget = card + ' .dshpm-btn-update:not([disabled])';
  await waitFor(() => manager.exists(updateTarget), '原页面更新按钮可用');
  await manager.click(updateTarget);
  await confirmAction('^(更新|Update)$');
  await waitFor(async () => {
    const plugin = await targetPlugin();
    return plugin?.manifest.version === targetAfter.version && plugin.runtime.state === 'running';
  }, '原页面在线更新并激活目标', 240_000);
  Object.assign(result.targetSamples[1], await installation('target-' + targetAfter.version));
  result.checks.originalPageUpdate = true;
  await waitFor(() => frame.evaluate(`document.querySelector(${JSON.stringify(card)})?.innerText.includes(${JSON.stringify(targetAfter.version)})`),
    '原页面展示目标的新版本');
  await expand();
  const removeTarget = card + ' .dshpm-btn-danger:not([disabled])';
  await waitFor(() => manager.exists(removeTarget), '原页面卸载按钮可用');
  await manager.click(removeTarget);
  await confirmAction('^(卸载|Uninstall)$');
  await waitFor(async () => !(await targetPlugin()) && !(await manager.exists(card)), '原页面卸载目标', 240_000);
  assert.equal((await getMgr()).runtime.instanceId, recovered.runtime.instanceId);
  result.checks.originalPageUninstall = true;
  const beforeRestart = await installation();
  frame.close();
  frame = undefined;
  page.close();
  page = undefined;
  await app.stop();
  app = launch();
  page = await waitFor(() => connectAppPage(cdpPort, daemonPort, []), '完整重启后的 Electron 工作台', 75_000);
  const restored = await waitFor(async () => {
    const mgr = await getMgr();
    return mgr?.runtime?.state === 'running' && mgr;
  }, '完整重启后重新激活 Mgr', 90_000);
  assert.equal(restored.digest, recovered.digest);
  assert.notEqual(restored.runtime.instanceId, recovered.runtime.instanceId);
  assert.deepEqual(await installation(), beforeRestart);
  assert.equal(await targetPlugin(), undefined);
  // 重启后的页面可能还停在 about:blank：相对地址的 fetch 需要页面已加载到应用地址。
  await waitFor(() => page.evaluate('location.protocol === "http:"'), '重启后工作台页面加载到应用地址', 30_000);
  const restoredList = await pageTools(page).post(`/api/plugins/${packageName}/routes`, {
    method: 'GET', path: '/api/plugin-manager/list',
    expectedDigest: restored.digest, expectedInstanceId: restored.runtime.instanceId,
  });
  assert.equal(restoredList.status, 200, restoredList.body);
  assert.equal(JSON.parse(restoredList.body).status, 200, restoredList.body);
  result.checks.applicationRestart = { targetStillRemoved: true, revision: beforeRestart.revision, originalListStatus: restoredList.status };
  const hash = async relative => digest(await readFile(join(resources, relative)));
  result.buildIdentity = JSON.parse(await readFile(join(resources, 'browser-capability-manifest.json'))).buildIdentity;
  result.daemonSha256 = await hash('daemon/magi-daemon-app');
  result.sdkLockSha256 = await hash('daemon/dsh/package-lock.json');
  result.result = 'passed';
} catch (error) {
  result.result = 'failed';
  result.error = error.message;
  console.error(error);
  process.exitCode = 1;
  if (page) {
    const screenshot = await pageTools(page).screenshot().catch(() => null);
    if (screenshot) await writeFile(join(output, 'failure.png'), Buffer.from(screenshot, 'base64'));
  }
  if (frame) await writeFile(join(output, 'failure-page.txt'), await frame.evaluate('document.body.innerText').catch(() => ''));
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(result, null, 2) + '\n');
  page?.close();
  frame?.close();
  await app.stop();
  await gate.close();
  console.log(JSON.stringify(result, null, 2));
}
