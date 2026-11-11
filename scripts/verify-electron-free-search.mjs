import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, createReporter, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { authorizePluginFromNative, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

// 同一官方原包：原页面配置、实际联网工具、全局选择和完整应用重启。
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_FREE_SEARCH_PORT || 38954), cdpPort = Number(process.env.MAGI_FREE_SEARCH_CDP_PORT || 9328);
const origin = `http://127.0.0.1:${port}`;
const output = process.env.MAGI_FREE_SEARCH_OUTPUT || join(root, 'target/plugin-acceptance/free-search');
const version = process.env.MAGI_FREE_SEARCH_VERSION || '0.9.41';
// Google 的公开 DNS JSON 是真实公网 HTTPS 内容；直接公网地址不依赖本机代理的 fake-IP DNS。
const fetchUrl = 'https://8.8.8.8/resolve?name=example.com&type=ANY';
const work = await mkdtemp(join(tmpdir(), 'magi-free-search-'));
const stateRoot = join(work, 'state'), dshHome = join(work, 'dsh');
const report = createReporter(), errors = [];
let app, page, frame, ui, plugin;
await mkdir(output, { recursive: true });
const save = (name, value) => writeFile(join(output, name + '.json'), JSON.stringify(value, null, 2));
async function request(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(300_000) });
  const value = await response.json(); assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`); return value;
}
const plugins = async () => (await request('/api/plugins')).plugins;
const current = async () => (await plugins()).find(value => value.manifest.id === plugin.manifest.id);
const webState = async () => JSON.parse(await readFile(join(stateRoot, 'plugins/state.json'), 'utf8')).web;
async function install(spec) {
  const preview = await request('/api/plugins/upstream/preview', { ecosystem: 'dsh', ...spec });
  await save(spec.package + '-preview', preview);
  const installed = (await request('/api/plugins/upstream/install', { ecosystem: 'dsh', ...spec, expectedDigest: preview.candidate.digest }))
    .plugins.find(value => value.manifest.id === preview.candidate.manifest.id);
  assert.ok(installed); assert.equal(installed.digest, preview.candidate.digest);
  return authorizePluginFromNative({ request, page, plugin: installed });
}
async function start() {
  assert.equal(await fetch(origin + '/health').then(() => true, () => false), false, '验收端口已占用');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: {
    MAGI_STATE_ROOT: stateRoot, DSH_HOME: dshHome, MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '',
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
  if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) await ui.click('[data-testid=sidebar-settings]');
  const view = plugin.manifest.contributions.views.find(value => value.placements.includes('settings'));
  const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${plugin.manifest.id}:${view.id}`)}]`;
  await waitFor(() => ui.exists(nav), 'Free Search 原页面入口'); await ui.click(nav);
  const selector = `iframe[data-plugin-id=${JSON.stringify(plugin.manifest.id)}]`;
  const url = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`), '原包隔离文档');
  frame = await waitFor(() => connectTarget(cdpPort, target => ['iframe', 'page'].includes(target.type) && target.url === url, 'Free Search', errors), '原包页面');
  await waitFor(() => frame.evaluate('Boolean(document.querySelector(".dshfs-footerRight button:not(:disabled)"))'), '原页面配置加载', 60_000);
}
async function turn(session, text, name) {
  const submitted = await request('/api/session/turn', { scope: 'personal', sessionId: session, text, requestId: name, userMessageId: name + '-user' });
  const result = await waitFor(async () => (await request(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns
    .find(value => value.turnId === submitted.turnId && ['completed', 'failed', 'cancelled', 'blocked'].includes(value.status)), name, 180_000);
  await save(name, result); assert.equal(result.status, 'completed', JSON.stringify(result.items.filter(item => item.status === 'failed')).slice(0, 1500)); return result;
}
try {
  await writeFile(join(output, 'state-root.txt'), work);
  await start();
  plugin = await install({ package: 'dsh-free-search', version });
  await save('running', plugin);
  report.check('官方原包在线安装、原生授权并实际运行', plugin.runtime.state === 'running');
  const selection = await waitFor(async () => { const value = await webState(); return value?.search?.providerId === 'ddg' && value?.fetch?.providerId === 'fs-quality-fetch' && value; }, '原包注册并全局选择搜索与抓取');
  await save('selection', selection);
  report.check('搜索和抓取共用 daemon 的全局选择', selection.search.pluginId === plugin.manifest.id && selection.fetch.pluginId === plugin.manifest.id);
  await settings();
  await save('page', await frame.evaluate('({text:document.body.innerText,selects:[...document.querySelectorAll("select")].map(n=>({value:n.value,options:[...n.options].map(o=>o.value)})),buttons:[...document.querySelectorAll("button")].map(n=>n.innerText)})'));
  await writeFile(join(output, 'settings.png'), Buffer.from(await ui.screenshot(), 'base64'));
  report.check('原页面完整加载引擎、提示词和官网链接', await frame.evaluate(`document.querySelectorAll('select').length === 8 && Boolean(document.querySelector('a[href="https://github.com/DDDMUC/dsh-free-search"]')) && document.querySelectorAll('a[href^="https://"]').length >= 2`));
  await frame.evaluate(`(() => {const node=[...document.querySelectorAll('select')].find(n=>[...n.options].some(o=>o.value==='bing')&&[...n.options].some(o=>o.value==='parallel'));Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(node,'bing');node.dispatchEvent(new Event('change',{bubbles:true}));})()`);
  await waitFor(() => frame.evaluate(`(() => {const button=[...document.querySelectorAll('button')].find(n=>/^(保存|Save)$/.test(n.innerText.trim()));if(!button||button.disabled)return false;button.click();return true;})()`), '原页面保存引擎');
  await waitFor(async () => (await request(`/api/plugins/${plugin.manifest.id}/settings`)).value?.provider === 'bing', '配置由 daemon 提交');
  report.check('原页面保存引擎后原生配置实际持久化', true);
  await frame.evaluate(`(() => {const original=globalThis.fetch;globalThis.fetch=async(...args)=>{const response=await original(...args);if(String(args[0]?.url??args[0]).endsWith('/raw-search'))globalThis.__searchReceipt={status:response.status,body:await response.clone().json()};return response;};[...document.querySelectorAll('button')].find(n=>/^(测试引擎|Test engine)$/.test(n.innerText.trim())).click();})()`);
  const receipt = await waitFor(() => frame.evaluate('globalThis.__searchReceipt'), '原引擎真实联网回执', 120_000);
  await save('engine-search', receipt);
  report.check('原页面测试引擎返回真实来源并展示结果', receipt.status === 200 && receipt.body.ok && receipt.body.value.sources.length > 0);
  await waitFor(() => frame.evaluate('Boolean(document.querySelector(".dshfs-testOk"))'), '原引擎结果展示');
  const beforeCandidate = await current();
  const rejected = await fetch(origin + '/api/plugins/upstream/preview', { method: 'POST', headers: { origin, 'content-type': 'application/json' },
    body: JSON.stringify({ ecosystem: 'dsh', package: 'dsh-free-search', version: '0.9.39' }), signal: AbortSignal.timeout(180_000) });
  const rejectedBody = await rejected.json(); await save('invalid-candidate', { status: rejected.status, body: rejectedBody });
  const afterCandidate = await current();
  report.check('旧原包的实际子模块失败在在线预览中明确报告，当前业务保持原实例', rejected.status === 400 && JSON.stringify(rejectedBody).includes('parameters.maxResults.minimum')
    && afterCandidate.digest === beforeCandidate.digest && afterCandidate.runtime.instanceId === beforeCandidate.runtime.instanceId && JSON.stringify(afterCandidate.grants) === JSON.stringify(beforeCandidate.grants));

  await mkdir(dshHome, { recursive: true, mode: 0o700 });
  await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(dshHome, '.workbuddy-device.json'));
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
    const binding = selection[tool === 'web_search' ? 'search' : 'fetch'];
    const providerTool = `plugin__${plugin.manifest.id}__${binding.contributionId}`.replace(/[^a-zA-Z0-9-]/g, '_').slice(0, 51)
      + '_' + createHash('sha256').update(`${plugin.manifest.id}\x1f${binding.contributionId}`).digest('hex').slice(0, 12);
    const receipt = item?.tool?.result;
    const fetched = tool === 'web_fetch' && receipt?.body?.content ? JSON.parse(receipt.body.content) : null;
    report.check(`新会话的原生 ${tool} 使用同一全局原包提供方完成业务`, receipt?.status === 'succeeded' && receipt.providerTool === providerTool && receipt.providerId === binding.providerId
      && (tool === 'web_search' ? receipt.sources?.length > 0 : receipt.statusCode === 200 && fetched?.Status === 0 && fetched.Question?.some(item => /^example\.com\.?$/.test(item.name))), JSON.stringify(receipt));
  }
  const beforeStop = await current();
  await request(`/api/plugins/${plugin.manifest.id}/deactivate`, {});
  await waitFor(async () => { const value = await current(); return !value.active && (!value.runtime || value.runtime.state === 'stopped'); }, '停用后回收宿主');
  const fallback = await turn(sessions[1], `请调用 web_fetch 一次，url=${JSON.stringify(fetchUrl)}，返回本次取得的 DNS 记录摘要，不要调用其他工具。`, 'native-after-deactivate');
  const nativeReceipt = fallback.items.filter(item => item.tool?.name === 'web_fetch').at(-1)?.tool?.result;
  report.check('全局停用后原生联网继续可用且未交给已停用的原包', nativeReceipt?.status === 'succeeded' && !nativeReceipt.providerTool, JSON.stringify(nativeReceipt));
  await request(`/api/plugins/${plugin.manifest.id}/activate`, {});
  const reactivated = await waitFor(async () => { const value = await current(); return value?.runtime?.state === 'running' && value; }, '全局重新激活', 120_000);
  report.check('重新激活恢复原选择及设置，释放旧实例', reactivated.runtime.instanceId !== beforeStop.runtime.instanceId && reactivated.digest === beforeStop.digest
    && JSON.stringify(await webState()) === JSON.stringify(selection) && (await request(`/api/plugins/${plugin.manifest.id}/settings`)).value.provider === 'bing');
  const before = await current(); await stop('before-restart'); await start();
  const restored = await waitFor(async () => { const value = await current(); return value?.runtime?.state === 'running' && value; }, '完整重启恢复原包', 120_000);
  assert.deepEqual(await webState(), selection);
  assert.equal((await request(`/api/plugins/${plugin.manifest.id}/settings`)).value.provider, 'bing');
  report.check('完整应用重启保留全局选择、原设置和授权并恢复新实例', restored.digest === before.digest && restored.runtime.instanceId !== before.runtime.instanceId && JSON.stringify(restored.grants) === JSON.stringify(before.grants));
  await settings(); await writeFile(join(output, 'restored.png'), Buffer.from(await ui.screenshot(), 'base64'));
  report.check('Renderer 和原包页面无异常', errors.length === 0, errors.join('\n'));
  await save('evidence', { completed: true, package: 'dsh-free-search', version: plugin.manifest.version, digest: plugin.digest, selection, sessions, checks: report.results.length });
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
