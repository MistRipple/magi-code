// 固定 Ops 原包设置页、真实模型诊断、知识沉淀与完整应用重启。
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { installFixedUpstreamFromNative, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

const packageName = 'dsh-plugin-ops-bundle', version = '0.20.8';
const appRoot = resolve(process.env.MAGI_PLUGIN_TEST_APP ?? 'target/electron-dist/mac-arm64/Magi.app');
const output = resolve(process.env.MAGI_DSH_OPS_OUTPUT ?? 'target/plugin-acceptance/dsh-ops');
const port = Number(process.env.MAGI_DSH_OPS_PORT ?? 38955);
const cdpPort = Number(process.env.MAGI_DSH_OPS_CDP_PORT ?? 9329);
const origin = `http://127.0.0.1:${port}`;
// 复用本轮真机验收的独立状态，可在同一固定样本环境接着检查 Ops。
const stateRoot = process.env.MAGI_DSH_OPS_STATE_ROOT
  ? resolve(process.env.MAGI_DSH_OPS_STATE_ROOT)
  : await mkdtemp(join(tmpdir(), 'magi-ops-phase1-'));
const baseline = JSON.parse(await readFile(new URL('../docs/plugin-validation/phase-1/manifest.json', import.meta.url)));
const checks = [];
const errors = [];
const samples = [];
let app, page, frame, ui, plugin;
const chatCard = '.dshops-card:has(.dshops-chat)';
await mkdir(output, { recursive: true });

async function transport(path, body) {
  return fetch(origin + path, {
    method: body === undefined ? 'GET' : 'POST', signal: AbortSignal.timeout(180_000),
    headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
}
async function request(path, body) {
  const response = await transport(path, body);
  assert.equal(response.status, 200, path);
  return response.json();
}
async function inspect(name, release) {
  const spec = { ecosystem: 'dsh', package: name, version: release };
  const preview = await request('/api/plugins/upstream/preview', spec);
  const sample = baseline.packages.find(row => row.name === name && row.version === release);
  assert.equal(preview.candidate.digest, sample.sha256);
  assert.equal(preview.integrity, sample.integrity);
  samples.push({ name, version: release, sha256: sample.sha256, integrity: sample.integrity });
}
async function start() {
  assert.equal(await fetch(origin + '/health').then(() => true, () => false), false, '验收端口已占用');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: stateRoot, cdpPort, env: {
    MAGI_STATE_ROOT: stateRoot, MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_DESKTOP_REUSE_DAEMON: '',
    MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_DSH_SDK_ROOT: '',
    MAGI_NPM_REGISTRY: 'https://registry.npmjs.org', MAGI_HOST: '127.0.0.1',
  } });
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '真实 Electron', 60_000);
  ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid="sidebar-settings"]'), '设置入口');
}
async function stop() {
  frame?.close(); frame = undefined;
  page?.close(); page = undefined;
  if (app) { await app.stop(); app = undefined; }
}
async function mountSettings() {
  frame?.close();
  if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) {
    await ui.click('[data-testid="sidebar-settings"]');
  }
  const selector = `[data-testid="settings-nav-plugin:${packageName}:settings"]`;
  await waitFor(() => ui.exists(selector), 'Ops 独立设置页');
  await ui.click(selector);
  const src = await waitFor(() => page.evaluate(`document.querySelector('iframe[data-plugin-id="${packageName}"]')?.src`), 'Ops 原包 iframe');
  frame = await waitFor(() => connectTarget(cdpPort, tab => tab.url === src, 'Ops iframe', errors), '真实 Ops iframe');
  await waitFor(() => frame.evaluate(`Boolean(document.querySelector(${JSON.stringify(chatCard + ' .dshops-chatbar input')}))`), '诊断面板');
}
const ragCheckbox = chatCard + ' input[type=checkbox]';
const readRag = () => frame.evaluate(`document.querySelector(${JSON.stringify(ragCheckbox)})?.checked`);
const current = async () => (await request('/api/plugins')).plugins.find(row => row.manifest.id === packageName);
const routeRequest = (instance, path) => ({ method: 'GET', path: '/dsh-ops/api/' + path,
  expectedDigest: instance.digest, expectedInstanceId: instance.runtime.instanceId });
async function knowledge(instance) {
  const result = await request(`/api/plugins/${packageName}/routes`, routeRequest(instance, 'knowledge'));
  assert.equal(result.status, 200, JSON.stringify(result.body));
  return result.body.entries;
}

try {
  const dshHome = join(stateRoot, 'plugins', 'dsh-home');
  await mkdir(dshHome, { recursive: true, mode: 0o700 });
  await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(dshHome, '.workbuddy-device.json'));
  await start();
  const buddy = { packageName: 'dsh-connect-workbuddy', version: '5.21.2' };
  await inspect(buddy.packageName, buddy.version);
  const installed = (await request('/api/plugins')).plugins.find(row => row.manifest.id === buddy.packageName);
  if (installed) {
    assert.equal(installed.manifest.version, buddy.version);
    assert.equal(installed.enabled, true);
  } else {
    await installFixedUpstreamFromNative({ page, request, ...buddy });
  }
  console.log('已安装 WorkBuddy，等待真实在线模型');
  const model = await waitForPluginModel({ request, pluginId: buddy.packageName,
    modelId: process.env.MAGI_DSH_OPS_MODEL ?? 'deepseek-v4.1-flash', record: async () => {} });
  const { sessionId } = await request('/api/session/materialize', { scope: 'personal' });
  await request('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId,
    config: { sourceId: model.sourceId, model: model.id, reasoningEffort: null } });

  await inspect(packageName, version);
  plugin = await installFixedUpstreamFromNative({ page, request, packageName, version });
  console.log('已安装 Ops，打开原包诊断界面');
  checks.push('settingsInstallation');
  await mountSettings();
  await frame.evaluate(`document.querySelector(${JSON.stringify(ragCheckbox)}).click()`);
  await waitFor(readRag, '增强检索已开启');
  await ui.click('[data-testid="settings-nav-plugins"]');
  await mountSettings();
  assert.equal(await readRag(), true);
  checks.push('settingsReopened');

  const marker = 'MAGI_OPS_' + Date.now().toString(36);
  const question = `请根据当前插件健康诊断解释一个可见问题，并在回答开头原样写出 ${marker}。如果扫描不能读取配置，请说明原因。请勿执行修复操作。`;
  await frame.evaluate(`(() => {
    const input = document.querySelector(${JSON.stringify(chatCard + ' .dshops-chatbar input')});
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, ${JSON.stringify(question)});
    input.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
  await frame.evaluate(`document.querySelector(${JSON.stringify(chatCard + ' .dshops-chatbar button')}).click()`);
  const diagnostic = await waitFor(() => frame.evaluate(`(() => {
    const text = document.querySelector(${JSON.stringify(chatCard + ' .dshops-err')})?.innerText;
    return text?.includes('has no readable manifest') && text;
  })()`), '诊断报告上游 profile 依赖');
  const excludedDiagnosis = diagnostic.replaceAll(stateRoot, '<isolated-state>');
  console.log('固定原包报告 CLI profile 依赖，验证独立知识保存');
  await frame.evaluate(`[...document.querySelectorAll('button')].find(button => /^(沉淀为知识|Deposit as knowledge)$/.test(button.innerText.trim())).click()`);
  const saved = await waitFor(async () => (await knowledge(plugin)).find(entry => entry.id && entry.title && entry.fix), '原页面沉淀实际知识', 105_000);
  await waitFor(() => frame.evaluate(`document.body.innerText.includes(${JSON.stringify(saved.title)})`), '知识出现在原页面');
  checks.push('knowledgeDeposited');

  await request(`/api/plugins/${packageName}/disable`, { expectedDigest: plugin.digest });
  await waitFor(async () => !((await current())?.enabled), '停用 Ops');
  assert.equal((await transport(`/api/plugins/${packageName}/routes`, routeRequest(plugin, 'knowledge'))).status, 403);
  await request(`/api/plugins/${packageName}/enable`, { expectedDigest: plugin.digest });
  const enabled = await waitFor(async () => {
    const latest = await current();
    return latest?.runtime.state === 'running' && latest;
  }, '重新启用 Ops', 60_000);
  assert.notEqual(enabled.runtime.instanceId, plugin.runtime.instanceId);
  assert.equal((await transport(`/api/plugins/${packageName}/routes`, routeRequest(plugin, 'knowledge'))).status, 409);
  assert.ok((await knowledge(enabled)).some(entry => entry.id === saved.id));
  checks.push('reactivation');

  await stop();
  await start();
  const reopened = await waitFor(async () => {
    const latest = await current();
    return latest?.runtime.state === 'running' && latest;
  }, '完整应用重启后 Ops 激活', 60_000);
  assert.notEqual(reopened.runtime.instanceId, enabled.runtime.instanceId);
  const entries = await knowledge(reopened);
  assert.deepEqual(entries.map(entry => entry.id), [saved.id]);
  await mountSettings();
  assert.equal(await readRag(), true);
  await waitFor(() => frame.evaluate(`document.body.innerText.includes(${JSON.stringify(saved.title)})`), '新页面显示原知识');
  checks.push('applicationRestart');
  await writeFile(join(output, 'ops-restored.png'), Buffer.from(await ui.screenshot(), 'base64'));
  const state = JSON.parse(await readFile(join(stateRoot, 'plugins', 'state.json')));
  const lock = await readFile(join(state.loading.configuration.preparation.projectDirectory, 'package-lock.json'));
  await writeFile(join(output, 'package-lock.json'), lock);
  const resources = join(appRoot, 'Contents', 'Resources');
  const manifest = JSON.parse(await readFile(join(resources, 'browser-capability-manifest.json')));
  const receipt = { result: 'passed', samples, checks, excludedDiagnosis, model: model.id,
    buildIdentity: manifest.buildIdentity, daemonSha256: manifest.components.daemon.sha256,
    sdkLockSha256: manifest.files.find(file => file.path === 'daemon/dsh/package-lock.json').sha256,
    dependenciesSha256: createHash('sha256').update(lock).digest('hex') };
  await writeFile(join(output, 'result.json'), JSON.stringify(receipt, null, 2) + '\n');
  console.log(receipt);
} catch (error) {
  console.error(error);
  console.error(checks);
  process.exitCode = 1;
  if (frame) {
    const text = await frame.evaluate('document.body.innerText').catch(() => '');
    await writeFile(join(output, 'failure-page.txt'), text);
  }
  if (ui) {
    const message = await ui.text('[data-testid="plugin-message"]').catch(() => null);
    if (message) console.error(message);
  }
} finally {
  if (app) await writeFile(join(output, 'electron.log'), app.log.join(''));
  await stop();
}
