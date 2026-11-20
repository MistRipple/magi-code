import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, rename, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, createReporter, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { acceptanceHostPid, installFixedUpstreamFromNative, waitForInstalledPlugin, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

// 同一官方原包：原页面配置、实际联网工具、全局选择和完整应用重启。
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_FREE_SEARCH_PORT || 38954), cdpPort = Number(process.env.MAGI_FREE_SEARCH_CDP_PORT || 9328);
const origin = `http://127.0.0.1:${port}`;
const output = process.env.MAGI_FREE_SEARCH_OUTPUT || join(root, 'target/plugin-acceptance/free-search');
const version = '0.9.41';
const packageName = 'dsh-free-search';
const baseline = JSON.parse(await readFile(new URL('../docs/plugin-validation/phase-1/manifest.json', import.meta.url)));
// Google 的公开 DNS JSON 是真实公网 HTTPS 内容；直接公网地址不依赖本机代理的 fake-IP DNS。
const fetchUrl = 'https://8.8.8.8/resolve?name=example.com&type=A';
const slowFetchUrl = 'https://8.8.8.8/resolve?name=example.net&type=A';
const work = await mkdtemp(join(tmpdir(), 'magi-free-search-'));
const stateRoot = join(work, 'state'), dshHome = join(stateRoot, 'plugins', 'dsh-home');
const report = createReporter(), errors = [];
let app, page, frame, ui, plugin;
await mkdir(output, { recursive: true });
const save = (name, value) => writeFile(join(output, name + '.json'), JSON.stringify(value, null, 2));
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
async function request(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(300_000) });
  const value = await response.json(); assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`); return value;
}
const plugins = async () => (await request('/api/plugins')).plugins;
const current = async () => (await plugins()).find(value => value.manifest.id === packageName);
const webState = async () => JSON.parse(await readFile(join(dshHome, 'host-settings.json'), 'utf8')).web;
const originalSettingsRequest = (instance, operation, body = {}) => ({
  method: 'POST', path: `/api/dsh-free-search-settings/${operation}`, body,
  expectedDigest: instance.digest, expectedInstanceId: instance.runtime.instanceId,
});
async function originalSettings(instance = plugin, operation = 'describe', body = {}) {
  const { status, body: result } = await request(`/api/plugins/${instance.manifest.id}/routes`,
    originalSettingsRequest(instance, operation, body));
  assert.equal(status, 200);
  return result;
}
async function savedProvider(instance) {
  const result = await originalSettings(instance);
  assert.equal(result.ok, true);
  return result.value.namespaces.find(entry => entry.ns === 'web-search-free');
}
async function lockedPreview(spec) {
  const preview = await request('/api/plugins/upstream/preview', { ecosystem: 'dsh', ...spec });
  const fixed = baseline.packages.find(row => row.name === spec.package && row.version === spec.version);
  assert.equal(preview.candidate.digest, fixed.sha256);
  assert.equal(preview.integrity, fixed.integrity);
  return { archiveSha256: fixed.sha256, integrity: fixed.integrity, preview };
}
async function install(spec) {
  const { preview } = await lockedPreview(spec);
  const installed = (await request('/api/plugins/upstream/install', { ecosystem: 'dsh', ...spec, expectedDigest: preview.candidate.digest }))
    .plugins.find(value => value.manifest.id === preview.candidate.manifest.id);
  assert.ok(installed);
  return waitForInstalledPlugin({ request, page, plugin: installed });
}
async function start() {
  assert.equal(await fetch(origin + '/health').then(() => true, () => false), false, '验收端口已占用');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: {
    MAGI_STATE_ROOT: stateRoot, MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '',
    MAGI_DSH_SDK_ROOT: '', MAGI_DESKTOP_REUSE_DAEMON: '',
  } });
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '真实 Electron 工作台', 60_000); ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '原生设置入口');
}
async function stop(name) {
  frame?.close(); frame = undefined; page?.close(); page = undefined;
  await writeFile(join(output, name + '.log'), app.log.join('')); await app.stop(); app = undefined;
}
async function settings() {
  frame?.close();
  if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) {
    await waitFor(() => ui.exists('[data-testid="sidebar-settings"]'), '设置入口可用');
    await ui.click('[data-testid="sidebar-settings"]');
  }
  const view = plugin.manifest.contributions.views.find(value => value.placements.includes('settings'));
  const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${plugin.manifest.id}:${view.id}`)}]`;
  await waitFor(() => ui.exists(nav), 'Free Search 原页面入口'); await ui.click(nav);
  const selector = `iframe[data-plugin-id=${JSON.stringify(plugin.manifest.id)}]`;
  const url = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`), '原包隔离文档');
  frame = await waitFor(() => connectTarget(cdpPort, target => ['iframe', 'page'].includes(target.type) && target.url === url, 'Free Search', errors), '原包页面');
  await waitFor(() => frame.evaluate('Boolean(document.querySelector(".dshfs-footerRight button:not(:disabled)"))'), '原页面配置加载', 60_000);
}
async function submitTurn(session, text, name) {
  const { turnId } = await request('/api/session/turn', {
    scope: 'personal', sessionId: session, text, requestId: name, userMessageId: name + '-user',
  });
  return async () => (await request(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns
    .find(value => value.turnId === turnId);
}
const terminal = value => ['completed', 'failed', 'cancelled', 'blocked'].includes(value?.status);
async function turn(session, text, name) {
  const readTurn = await submitTurn(session, text, name);
  const result = await waitFor(async () => { const value = await readTurn(); return terminal(value) && value; }, name, 180_000);
  await save(name, result); assert.equal(result.status, 'completed', JSON.stringify(result.items.filter(item => item.status === 'failed')).slice(0, 1500)); return result;
}
const slowFetchCalls = value => value?.items.filter(item => item.tool?.name === 'web_fetch'
  && item.tool.arguments?.url === slowFetchUrl) ?? [];
async function awaitFetchDispatch(readTurn, evidenceName) {
  await waitFor(async () => {
    const value = await readTurn();
    const calls = slowFetchCalls(value);
    if (terminal(value)) {
      await save(evidenceName, value);
      throw new Error('抓取未进入执行即结束');
    }
    if (!calls.some(item => item.status === 'running')) return false;
    await save(evidenceName, value);
    return true;
  }, '固定原包抓取调用开始', 30_000);
}

const engineSelect = `[...document.querySelectorAll('select')].find(node =>
  [...node.options].some(option => option.value === 'bing') && [...node.options].some(option => option.value === 'parallel'))`;
async function saveEngine(engine) {
  await frame.evaluate(`(() => {
    const select = ${engineSelect};
    Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(select,${JSON.stringify(engine)});
    select.dispatchEvent(new Event('change',{bubbles:true}));
  })()`);
  await waitFor(() => frame.evaluate(`(() => {
    const button = [...document.querySelectorAll('button')].find(node => /^(保存|Save)$/.test(node.innerText.trim()));
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`), '原页面保存引擎');
  return await waitFor(async () => {
    const settings = await savedProvider();
    return settings.value.provider === engine && settings;
  }, '原包读取已提交设置');
}

try {
  await writeFile(join(output, 'state-root.txt'), work);
  await start();
  const { archiveSha256, integrity } = await lockedPreview({ package: packageName, version });
  plugin = await installFixedUpstreamFromNative({ page, request, packageName, version });
  await save('running', plugin);
  report.check('官方原包在线安装、原生授权并实际运行', plugin.runtime.state === 'running');
  const selection = await waitFor(async () => {
    const value = await webState();
    return value?.searchProvider === 'ddg' && value?.fetchProvider === 'fs-quality-fetch' && value;
  }, '原包注册并全局选择搜索与抓取');
  await save('selection', selection);
  report.check('原包声明的全局搜索和抓取提供方已由 daemon 保存', true);
  await settings();
  await save('page', await frame.evaluate('({text:document.body.innerText,selects:[...document.querySelectorAll("select")].map(n=>({value:n.value,options:[...n.options].map(o=>o.value)})),buttons:[...document.querySelectorAll("button")].map(n=>n.innerText)})'));
  await writeFile(join(output, 'settings.png'), Buffer.from(await ui.screenshot(), 'base64'));
  report.check('原页面完整加载引擎、提示词和官网链接', await frame.evaluate(`document.querySelectorAll('select').length === 8 && Boolean(document.querySelector('a[href="https://github.com/DDDMUC/dsh-free-search"]')) && document.querySelectorAll('a[href^="https://"]').length >= 2`));
  const saved = await saveEngine('bing');
  report.check('原页面保存引擎后原生配置实际持久化', true);
  await frame.evaluate(`(() => {const original=globalThis.fetch;globalThis.fetch=async(...args)=>{const response=await original(...args);if(String(args[0]?.url??args[0]).endsWith('/raw-search'))globalThis.__searchReceipt={status:response.status,body:await response.clone().json()};return response;};[...document.querySelectorAll('button')].find(n=>/^(测试引擎|Test engine)$/.test(n.innerText.trim())).click();})()`);
  const receipt = await waitFor(() => frame.evaluate('globalThis.__searchReceipt'), '原引擎真实联网回执', 120_000);
  await save('engine-search', receipt);
  report.check('原页面测试引擎返回真实来源并展示结果', receipt.status === 200 && receipt.body.ok && receipt.body.value.sources.length > 0);
  await waitFor(() => frame.evaluate('Boolean(document.querySelector(".dshfs-testOk"))'), '原引擎结果展示');
  await ui.click('[data-testid="settings-nav-plugins"]');
  await settings();
  report.check('重新打开设置显示已保存引擎', await frame.evaluate(`(${engineSelect})?.value`) === 'bing');

  const patchPath = join(stateRoot, 'plugins', 'loading-source', 'cordis.patch.yml');
  const patch = await readFile(patchPath, 'utf8');
  const engineLine = /^( +provider: )bing$/gm;
  assert.equal([...patch.matchAll(engineLine)].length, 1);
  const edited = patch.replace(engineLine, '$1ddg');
  await writeFile(patchPath + '.next', edited);
  await rename(patchPath + '.next', patchPath);
  const fileEdit = await waitFor(async () => {
    const descriptor = await savedProvider();
    return descriptor.value.provider === 'ddg' && descriptor;
  }, '外部编辑进入正式配置修订');
  assert.ok(fileEdit.revision > saved.revision);
  const conflict = await originalSettings(plugin, 'mutate', {
    ns: saved.ns, expectedRevision: saved.revision,
    ops: [{ op: 'set', path: ['provider'], value: 'auto' }],
  });
  report.check('原包拒绝带旧修订的设置保存并保留外部编辑',
    conflict.ok === false && conflict.code === 'settings-conflict' && (await savedProvider()).value.provider === 'ddg'
      && await readFile(patchPath, 'utf8') === edited, JSON.stringify(conflict));
  const documentStart = await page.evaluate('performance.timeOrigin');
  await page.call('Page.reload', { ignoreCache: true });
  await waitFor(async () => await page.evaluate('performance.timeOrigin') !== documentStart
    && await ui.exists('[data-testid="sidebar-settings"]'), '重开设置读取文件编辑后的配置');
  await settings();
  report.check('原包设置重新读取文件编辑后的值', await frame.evaluate(`(${engineSelect})?.value`) === 'ddg');
  await saveEngine('bing');

  await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(dshHome, '.workbuddy-device.json'), { force: false });
  const buddy = await install({ package: 'dsh-connect-workbuddy', version: '5.21.2' });
  const model = await waitForPluginModel({ request, pluginId: buddy.manifest.id, modelId: process.env.MAGI_FREE_SEARCH_MODEL || 'deepseek-v4.1-flash',
    record: value => save('model-selection', value) });
  const sessions = [];
  for (let i = 0; i < 2; i++) {
    const session = (await request('/api/session/materialize', { scope: 'personal' })).sessionId; sessions.push(session);
    await request('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session, config: { sourceId: model.sourceId, model: model.id, reasoningEffort: null } });
  }
  for (const [i, tool, input] of [[0, 'web_search', 'query="Node.js official documentation"'], [1, 'web_fetch', `url=${JSON.stringify(fetchUrl)}`]]) {
    const result = await turn(sessions[i], `请务必调用名为 ${tool} 的原生工具一次，参数 ${input}。不要调用其他工具。然后用中文给出取得的真实内容摘要和链接。`, 'native-' + tool);
    const item = result.items.find(item => item.tool?.name === tool && item.status === 'completed');
    const receipt = item?.tool?.result;
    report.check(`新会话的 ${tool} 返回实际业务结果`, receipt?.status === 'succeeded'
      && (tool === 'web_search' ? receipt.source === selection.searchProvider && receipt.results?.some(result => /^https?:/.test(result.url))
        : receipt.url === fetchUrl && receipt.content_type === 'text/html' && receipt.content?.includes('example.com')), JSON.stringify(receipt));
  }
  const beforeStop = await current();
  await request(`/api/plugins/${plugin.manifest.id}/disable`, { expectedDigest: beforeStop.digest });
  await waitFor(async () => { const value = await current(); return !value.enabled && (!value.runtime || value.runtime.state === 'stopped'); }, '停用后回收宿主');
  const fallback = await turn(sessions[1], `请调用 web_fetch 一次，url=${JSON.stringify(fetchUrl)}，返回本次取得的 DNS 记录摘要，不要调用其他工具。`, 'native-after-deactivate');
  const nativeReceipt = fallback.items.filter(item => item.tool?.name === 'web_fetch').at(-1)?.tool?.result;
  report.check('停用后原生网页抓取读取同址 JSON', nativeReceipt?.status === 'succeeded'
    && nativeReceipt.content_type?.startsWith('application/json') && nativeReceipt.content?.includes('example.com'), JSON.stringify(nativeReceipt));
  const readFromOldPage = () => fetch(origin + `/api/plugins/${beforeStop.manifest.id}/routes`, {
    method: 'POST', headers: { origin, 'content-type': 'application/json' },
    body: JSON.stringify(originalSettingsRequest(beforeStop, 'describe')),
  });
  const stoppedPage = await readFromOldPage();
  report.check('停用后原包旧页面请求被拒绝', stoppedPage.status === 403, await stoppedPage.text());
  await request(`/api/plugins/${plugin.manifest.id}/enable`, { expectedDigest: beforeStop.digest });
  const reactivated = await waitFor(async () => { const value = await current(); return value?.runtime?.state === 'running' && value; }, '全局重新激活', 120_000);
  report.check('重新激活恢复原选择及设置，释放旧实例', reactivated.runtime.instanceId !== beforeStop.runtime.instanceId && reactivated.digest === beforeStop.digest
    && JSON.stringify(await webState()) === JSON.stringify(selection) && (await savedProvider(reactivated)).value.provider === 'bing');
  const stalePage = await readFromOldPage();
  report.check('重新启用后旧页面仍然失效', stalePage.status === 409, await stalePage.text());
  const before = await current(); await stop('before-restart'); await start();
  const restored = await waitFor(async () => { const value = await current(); return value?.runtime?.state === 'running' && value; }, '完整重启恢复原包', 120_000);
  assert.deepEqual(await webState(), selection);
  assert.equal((await savedProvider(restored)).value.provider, 'bing');
  report.check('完整应用重启保留全局选择、原设置和授权并恢复新实例', restored.digest === before.digest && restored.runtime.instanceId !== before.runtime.instanceId);
  plugin = restored;
  await settings();
  assert.equal(await frame.evaluate(`(${engineSelect})?.value`), 'bing');
  const preRestartSessions = [[sessions[0], 1], [sessions[1], 2]];
  for (const [sessionId, count] of preRestartSessions) {
    const { canonicalTurns } = await request(`/api/messages?scope=personal&sessionId=${sessionId}`);
    assert.equal(canonicalTurns.length, count);
    assert.ok(canonicalTurns.every(turn => turn.status === 'completed'));
  }
  report.check('应用重启保留已完成的会话轮次', true);

  const prompt = `请务必调用名为 web_fetch 的原生工具一次，参数 url=${JSON.stringify(slowFetchUrl)}。不要调用其他工具，不要先解释，工具调用后如实报告结果。`;
  const readCancelled = await submitTurn(sessions[1], prompt, 'free-search-cancel');
  await awaitFetchDispatch(readCancelled, 'cancel-dispatch');
  await request('/api/session/interrupt', { sessionId: sessions[1] });
  const cancelled = await waitFor(async () => { const value = await readCancelled(); return terminal(value) && value; }, '在途抓取取消');
  await save('cancelled-fetch', cancelled);
  report.check('Free Search 在途抓取由 Magi 取消', cancelled.status === 'cancelled'
    && slowFetchCalls(cancelled).every(item => item.tool.result?.status !== 'succeeded'));

  const readCrash = await submitTurn(sessions[1], prompt, 'free-search-host-crash');
  await awaitFetchDispatch(readCrash, 'crash-dispatch');
  process.kill(acceptanceHostPid(app.child.pid), 'SIGKILL');
  const crashed = await waitFor(async () => { const value = await readCrash(); return terminal(value) && value; }, '抓取期间宿主断开后的结算', 90_000);
  await save('host-crash-fetch', crashed);
  const recovered = await waitFor(async () => {
    const value = await current();
    return value?.runtime?.state === 'running' && value.runtime.instanceId !== restored.runtime.instanceId && value;
  }, '崩溃后恢复固定原包的新实例', 90_000);
  const calls = slowFetchCalls(crashed);
  report.check('崩溃期间的抓取结算为失败且未重放', calls.some(item => item.tool.result?.status === 'failed'
    && /连接已关闭|HostGone/.test(item.tool.result.error)) && new Set(calls.map(item => item.tool.callId)).size === 1,
    JSON.stringify({ turnStatus: crashed.status, calls: calls.map(item => ({ status: item.status, result: item.tool.result })) }));
  report.check('宿主恢复后继续使用原设置，旧页面身份失效', recovered.digest === restored.digest
    && (await savedProvider(recovered)).value.provider === 'bing'
    && (await fetch(origin + `/api/plugins/${restored.manifest.id}/routes`, {
      method: 'POST', headers: { origin, 'content-type': 'application/json' },
      body: JSON.stringify(originalSettingsRequest(restored, 'describe')),
    })).status === 409);
  const recoveredModel = await waitForPluginModel({ request, pluginId: 'dsh-connect-workbuddy',
    modelId: process.env.MAGI_FREE_SEARCH_MODEL || 'deepseek-v4.1-flash', record: value => save('model-selection-after-crash', value) });
  assert.equal(recoveredModel.sourceId, model.sourceId, '宿主恢复后仍使用原会话选定的模型来源');
  const retry = await turn(sessions[1], `只调用 web_fetch 一次，url=${JSON.stringify(fetchUrl)}，并报告真实结果。`, 'native-after-host-crash');
  report.check('宿主崩溃恢复后能发起新的真实抓取', retry.items.some(item => item.tool?.name === 'web_fetch'
    && item.tool.result?.status === 'succeeded' && item.tool.result.content_type === 'text/html'));
  const afterRecovery = await readCancelled();
  report.check('恢复与新抓取完成后取消终态和工具条目保持不变', afterRecovery.status === 'cancelled'
    && JSON.stringify(slowFetchCalls(afterRecovery)) === JSON.stringify(slowFetchCalls(cancelled)));
  await writeFile(join(output, 'restored.png'), Buffer.from(await ui.screenshot(), 'base64'));
  report.check('Renderer 和原包页面无异常', errors.length === 0, errors.join('\n'));
  const resources = join(appRoot, 'Contents', 'Resources');
  const manifest = JSON.parse(await readFile(join(resources, 'browser-capability-manifest.json')));
  const ownerState = JSON.parse(await readFile(join(stateRoot, 'plugins/state.json')));
  const lock = await readFile(join(ownerState.loading.configuration.preparation.projectDirectory, 'package-lock.json'));
  await writeFile(join(output, 'package-lock.json'), lock);
  await save('evidence', {
    completed: true, package: packageName, version, archiveSha256, integrity, selection,
    buildIdentity: manifest.buildIdentity,
    daemonSha256: manifest.components.daemon.sha256,
    sdkLockSha256: manifest.files.find(file => file.path === 'daemon/dsh/package-lock.json').sha256,
    dependenciesSha256: sha256(lock),
    checks: report.results.map(result => ({ name: result.name, ok: result.ok })),
  });
} catch (error) {
  console.error(error); process.exitCode = 1;
  await save('failure-plugins', await plugins().catch(() => []));
  report.results.push({ name: 'Free Search 完整原包流程', ok: false, detail: String(error) });
  if (frame) { await save('failure-page', await frame.evaluate('({text:document.body.innerText,html:document.body.innerHTML})').catch(() => null)); }
  if (ui) await writeFile(join(output, 'failure.png'), Buffer.from(await ui.screenshot().catch(() => ''), 'base64'));
} finally {
  await save('results', report.results); await save('errors', errors);
  if (app) await stop('electron');
  console.log(report.summary());
}
