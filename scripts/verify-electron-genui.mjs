// GenUI 原包边界验收：拒绝上游原生消息替换，不将其自动转换成 Magi 附加视图。
// 同一真实 Electron 内先后完成 WorkBuddy 模型业务，验证安装失败不影响原生对话。
import assert from 'node:assert/strict';
import { cp, mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir, homedir } from 'node:os';
import { resolve, join } from 'node:path';
import { connectAppPage, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';
import { authorizePluginFromNative, previewFixedUpstreamFromNative, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

const root = resolve(import.meta.dirname, '..');
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = 38947, cdpPort = 9317, origin = `http://127.0.0.1:${port}`;
const output = process.env.MAGI_GENUI_OUTPUT || join(root, 'target/plugin-acceptance/ui-boundary-genui');
const work = await mkdtemp(join(tmpdir(), 'magi-genui-boundary-'));
const report = createReporter(), errors = [];
let app, page, ui, session;
await mkdir(output, { recursive: true });
await mkdir(join(work, 'dsh'), { mode: 0o700 });
await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(work, 'dsh/.workbuddy-device.json')).catch(error => { if (error.code !== 'ENOENT') throw error; });
const save = (name, value) => writeFile(join(output, name + '.json'), JSON.stringify(value, null, 2));
const screenshot = name => ui.screenshot().then(value => writeFile(join(output, name + '.png'), Buffer.from(value, 'base64')));
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(240_000) });
  const value = await response.json(); assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`); return value;
}
const plugins = async () => (await api('/api/plugins')).plugins;
async function start() {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口须空闲');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '真实 Electron', 60_000); ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '原生工作台');
}
async function stop(name) {
  page?.close(); page = undefined;
  await writeFile(join(output, name + '.log'), app.log.join('')); await app.stop(); app = undefined;
}
async function navigate() {
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${session}` });
  await waitFor(() => ui.exists('[data-testid=input-textarea]'), '原生输入区');
}
async function reply(text, id) {
  const submitted = await api('/api/session/turn', { scope: 'personal', sessionId: session, text, requestId: id, userMessageId: id + '-user' });
  const result = await waitFor(async () => (await api(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns
    .find(turn => turn.turnId === submitted.turnId && ['completed', 'failed', 'cancelled', 'blocked'].includes(turn.status)), '真实模型回复', 180_000);
  await save(id, result); assert.equal(result.status, 'completed', JSON.stringify(result));
  assert.ok(result.items.some(item => item.kind === 'assistant_text' && item.content?.includes('MAGI_BOUNDARY_OK')), '真实助手正文');
  return result;
}
async function body(turnId) {
  return page.evaluate(`(() => [...document.querySelectorAll('.message-item.assistant[data-turn-id="${turnId}"] .markdown-content')].map(e => {
    const s=getComputedStyle(e),r=e.getBoundingClientRect();return {text:e.innerText,font:s.fontFamily,size:s.fontSize,color:s.color,lineHeight:s.lineHeight,visible:r.width>0&&r.height>0&&s.visibility!=='hidden'};
  }))()`);
}
try {
  await writeFile(join(output, 'state-root.txt'), work);
  await start();
  const spec = { ecosystem: 'dsh', package: 'dsh-connect-workbuddy', version: '5.21.2' };
  const preview = await api('/api/plugins/upstream/preview', spec); await save('workbuddy-preview', preview);
  const installed = (await api('/api/plugins/upstream/install', { ...spec, expectedDigest: preview.candidate.digest })).plugins.find(item => item.manifest.id === preview.candidate.manifest.id);
  const buddy = await authorizePluginFromNative({ request: api, page, plugin: installed });
  report.check('普通原包在线安装、原生授权和模型来源仍可用', buddy.runtime.state === 'running');
  report.check('激活前显示界面位置和原生对话边界说明', /原生对话/.test(await ui.text('[data-testid=plugin-impact-ui]')));
  const model = await waitForPluginModel({ request: api, pluginId: buddy.manifest.id, record: value => save('model-selection', value) });
  session = (await api('/api/session/materialize', { scope: 'personal' })).sessionId;
  await api('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session, config: { sourceId: model.sourceId, model: model.id, reasoningEffort: null } });
  const first = await reply('仅回复 MAGI_BOUNDARY_OK，不调用工具。', 'before-boundary');
  await navigate();
  await waitFor(async () => (await body(first.turnId)).some(item => item.text.includes('MAGI_BOUNDARY_OK') && item.visible), '详细模式原生正文');
  const before = await body(first.turnId), registry = await plugins();
  await page.evaluate(`(() => {const input=document.querySelector('[data-testid=input-textarea]');input.value='保留用户草稿';input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  await ui.click('[data-testid=sidebar-settings]'); await ui.click('[data-testid=settings-nav-plugins]');
  await page.evaluate(`(() => {const input=document.querySelector('[data-testid=plugin-search]');input.value='dsh-genui';input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  const row = '[data-testid=plugin-row][data-plugin-id="upstream:dsh:dsh-genui"]';
  await waitFor(() => ui.exists(row), '官方 GenUI 搜索结果', 60_000); await ui.click(row);
  await previewFixedUpstreamFromNative({ page, packageName: 'dsh-genui', version: '0.3.3' });
  const reason = await waitFor(async () => {const text=await ui.text('[data-testid=plugin-message]');return text?.includes('PLUGIN_UI_BOUNDARY_CONFLICT') && text;}, '明确的原生界面冲突提示', 180_000);
  await save('rejection', { package: 'dsh-genui', version: '0.3.3', reason });
  report.check('安装前说明消息替换冲突，明确拒绝且不提供覆盖授权', /conversation.chat.node/.test(reason) && /不能安装或启用/.test(reason));
  report.check('冲突原包没有可确认候选，安装注册表和既有实例不变', !await ui.exists('[data-testid=plugin-candidate]') && JSON.stringify(await plugins()) === JSON.stringify(registry));
  await screenshot('boundary-rejected');
  await ui.click('[data-testid=settings-back]');
  await waitFor(() => ui.exists('[data-testid=input-textarea]'), '返回原生对话');
  report.check('拒绝安装后原生正文和样式不变，用户草稿保留', JSON.stringify(await body(first.turnId)) === JSON.stringify(before)
    && await page.evaluate('document.querySelector("[data-testid=input-textarea]").value') === '保留用户草稿');
  report.check('未创建第三种对话模式或插件替换文档', !await ui.exists('iframe[data-testid=plugin-chatAssistant]') && !await ui.exists('[data-plugin-placement=chatAssistant]'));
  await api('/api/settings/update', { key: 'conversationDisplayMode', value: 'summary' }); await navigate();
  await waitFor(() => ui.exists('[data-conversation-display-mode=summary]'), '原生摘要模式');
  report.check('原生摘要模式继续展示真实助手正文', (await body(first.turnId)).some(item => item.text.includes('MAGI_BOUNDARY_OK') && item.visible));
  await screenshot('summary-preserved');
  await reply('再仅回复 MAGI_BOUNDARY_OK，不调用工具。', 'after-boundary');
  report.check('拒绝安装后真实模型业务仍完成', true);
  await stop('before-restart'); await start(); await navigate();
  const current = await plugins();
  report.check('完整重启保留原包和摘要模式，不恢复冲突原包', current.length === registry.length && current.some(item => item.digest === buddy.digest && item.active)
    && await ui.exists('[data-conversation-display-mode=summary]') && !await ui.exists('[data-plugin-placement=chatAssistant]'));
  await save('identity', { work, sessionId: session, workbuddy: { version: buddy.manifest.version, digest: buddy.digest }, rejected: 'dsh-genui@0.3.3', model: model.id });
  report.check('Renderer 无未捕获异常', errors.length === 0, errors.join('\n'));
  console.log(report.summary());
} catch (error) {
  console.error(error.stack || error);
  if (ui && page) await screenshot('failed').catch(() => {});
  process.exitCode = 1;
} finally {
  await save('results', { results: report.results, errors });
  if (app) await stop('app-final');
}
