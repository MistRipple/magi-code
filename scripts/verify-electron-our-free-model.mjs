import assert from 'node:assert/strict';
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, createReporter, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { authorizePluginFromNative } from './lib/plugin-native-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const packageName = process.env.MAGI_OFM_PACKAGE || '@ebony-vinyl/dsh-our-free-model';
const version = process.env.MAGI_OFM_VERSION || '2.8.3';
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const output = process.env.MAGI_OFM_OUTPUT || join(root, 'target/plugin-acceptance/our-free-model-fixed-' + version);
const port = Number(process.env.MAGI_OFM_PORT || 38955), cdpPort = Number(process.env.MAGI_OFM_CDP_PORT || 9329);
const origin = `http://127.0.0.1:${port}`, work = await mkdtemp(join(tmpdir(), 'magi-ofm-live-'));
const stateRoot = join(work, 'state'), dshHome = join(work, 'dsh'), report = createReporter(), errors = [];
let app, page, frame, ui, plugin;
await mkdir(output, { recursive: true });
const save = (name, value) => writeFile(join(output, name + '.json'), JSON.stringify(value, null, 2));
async function request(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST',
    headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: AbortSignal.timeout(300_000) });
  const value = await response.json(); assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`); return value;
}
const current = async () => (await request('/api/plugins')).plugins.find(value => value.manifest.id === plugin.manifest.id);
const route = async (path, body) => request(`/api/plugins/${plugin.manifest.id}/routes`, {
  method: body === undefined ? 'GET' : 'POST', path: '/api/our-free-model' + path, ...(body === undefined ? {} : { body }),
});
async function start() {
  assert.equal(await fetch(origin + '/health').then(() => true, () => false), false, '验收端口已占用');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: {
    MAGI_STATE_ROOT: stateRoot, DSH_HOME: dshHome, MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0',
    MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '',
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
  await waitFor(() => ui.exists(nav), '独立插件设置页'); await ui.click(nav);
  const selector = `iframe[data-plugin-id=${JSON.stringify(plugin.manifest.id)}]`;
  const url = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`), '原包隔离文档');
  frame = await waitFor(() => connectTarget(cdpPort, target => ['iframe', 'page'].includes(target.type) && target.url === url, 'Our Free Model', errors), '原包页面');
  await waitFor(() => frame.evaluate('document.body.innerText.length > 200'), '原页面业务内容', 60_000);
}
try {
  await writeFile(join(output, 'state-root.txt'), work);
  await save('environment', { package: packageName, version, platform: process.platform, arch: process.arch,
    bunExecutable: process.env.OUR_FREE_MODEL_BUN ?? null });
  await start();
  const spec = { ecosystem: 'dsh', package: packageName, version };
  const preview = await request('/api/plugins/upstream/preview', spec); await save('preview', preview);
  plugin = (await request('/api/plugins/upstream/install', { ...spec, expectedDigest: preview.candidate.digest }))
    .plugins.find(value => value.manifest.id === preview.candidate.manifest.id);
  plugin = await authorizePluginFromNative({ request, page, plugin }); await save('running', plugin);
  report.check('固定原包在线安装、原生授权并启动', plugin.manifest.version === version && plugin.runtime.state === 'running');
  await settings();
  const originalPage = await frame.evaluate('({text:document.body.innerText,links:[...document.querySelectorAll("a[href]")].map(a=>a.href)})');
  await save('page', originalPage); await writeFile(join(output, 'settings.png'), Buffer.from(await ui.screenshot(), 'base64'));
  report.check('原包页面展示于设置内的独立 tab', originalPage.text.length > 200);
  const summary = await route('/summary'); await save('summary', summary);
  report.check('原包业务路由取得真实状态', summary.status === 200 && summary.body && !summary.isError);
  const configured = await route('/settings', { probeIntervalMinutes: 23 }); await save('saved-settings', configured);
  report.check('原包设置操作保存真实配置', configured.status === 200 && configured.body.settings?.probeIntervalMinutes === 23);
  const groups = await waitFor(async () => {
    const values = (await request('/api/settings/models/catalog')).groups.filter(group => group.pluginId === plugin.manifest.id);
    return values.some(group => group.models.length) && values;
  }, '原包模型发布至原生目录', 90_000);
  await save('models', groups);
  const modelId = process.env.MAGI_OFM_MODEL || 'deepseek-v4.1-flash-free';
  const group = groups.find(group => group.models.some(model => model.id === modelId));
  assert.ok(group, `原包未发布指定模型 ${modelId}`);
  const model = group.models.find(model => model.id === modelId);
  await save('selected-model', { sourceId: group.sourceId, model });
  const session = (await request('/api/session/materialize', { scope: 'personal' })).sessionId;
  await request('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session, config: { sourceId: group.sourceId, model: model.id, reasoningEffort: null } });
  const accepted = await request('/api/session/turn', { scope: 'personal', sessionId: session, text: 'Reply with exactly MAGI_OFM_READY. Do not call tools.', requestId: 'ofm-real-model', userMessageId: 'ofm-real-model-user' });
  const result = await waitFor(async () => (await request(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns
    .find(turn => turn.turnId === accepted.turnId && ['completed', 'failed', 'cancelled', 'blocked'].includes(turn.status)), '真实模型调用结果', 180_000);
  await save('model-turn', result);
  const assistantText = result.items.filter(item => item.kind === 'assistant_text').map(item => item.content ?? '').join('\n').trim();
  const modelCompleted = result.status === 'completed' && assistantText === 'MAGI_OFM_READY';
  await save('model-outcome', { completed: modelCompleted, model: model.id, sourceId: group.sourceId, assistantText });
  report.check('原包模型完成真实请求并返回指定正文', modelCompleted, JSON.stringify({ status: result.status, assistantText }));
  frame.close(); frame = undefined;
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}` });
  await waitFor(() => page.evaluate('Boolean([...document.querySelectorAll(".message-item.assistant")].find(node=>node.innerText.includes("MAGI_OFM_READY")))'), '原生助手正文展示', 60_000);
  await writeFile(join(output, 'conversation.png'), Buffer.from(await ui.screenshot(), 'base64'));
  report.check('模型回复在 Magi 原生助手正文展示', true);
  const cancelled = await request('/api/session/turn', { scope: 'personal', sessionId: session,
    text: 'Write a long detailed explanation of compiler design. Do not call tools.', requestId: 'ofm-cancel', userMessageId: 'ofm-cancel-user' });
  await waitFor(async () => (await request(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns
    .find(turn => turn.turnId === cancelled.turnId && turn.modelSteps?.some(step => step.attempts?.some(attempt => !attempt.settledAt))), '原包真实请求进入模型执行', 60_000);
  await request('/api/session/interrupt', { sessionId: session });
  const cancelledTurn = await waitFor(async () => (await request(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns
    .find(turn => turn.turnId === cancelled.turnId && turn.status === 'cancelled'), '原生取消提交', 60_000);
  await save('cancelled-turn', cancelledTurn);
  report.check('执行中的原包模型调用可由 Magi 原生取消', true);
  const before = await current();
  await request(`/api/plugins/${plugin.manifest.id}/deactivate`, {});
  await waitFor(async () => { const value = await current(); return !value.active && value.runtime?.state !== 'running'; }, '停用回收实例');
  report.check('全局停用清除原包模型来源', !(await request('/api/settings/models/catalog')).groups.some(group => group.pluginId === plugin.manifest.id));
  await request(`/api/plugins/${plugin.manifest.id}/activate`, {});
  const reactivated = await waitFor(async () => { const value = await current(); return value.runtime?.state === 'running' && value; }, '重新启用', 120_000);
  report.check('重新启用使用新实例及原摘要', reactivated.digest === before.digest && reactivated.runtime.instanceId !== before.runtime.instanceId);
  await stop('before-restart'); await start();
  const restored = await waitFor(async () => { const value = await current(); return value.runtime?.state === 'running' && value; }, '完整应用重启', 120_000);
  report.check('完整重启保留安装和授权并恢复新实例', restored.digest === before.digest && restored.runtime.instanceId !== reactivated.runtime.instanceId && JSON.stringify(restored.grants) === JSON.stringify(before.grants));
  report.check('完整重启保留原包业务配置', (await route('/summary')).body.settings?.probeIntervalMinutes === 23);
  const restoredTurns = (await request(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns;
  report.check('重启保留真实正文和取消终态，不重放已取消请求', restoredTurns.some(turn => turn.turnId === result.turnId && turn.status === 'completed'
    && turn.items.some(item => item.kind === 'assistant_text' && item.content === 'MAGI_OFM_READY'))
    && restoredTurns.some(turn => turn.turnId === cancelled.turnId && turn.status === 'cancelled'));
  await settings();
  await writeFile(join(output, 'restored.png'), Buffer.from(await ui.screenshot(), 'base64'));
  report.check('Renderer 与原页面无未捕获异常', errors.length === 0, errors.join('\n'));
} catch (error) {
  console.error(error); process.exitCode = 1;
  report.results.push({ name: 'Our Free Model 原包流程', ok: false, detail: String(error) });
  if (plugin) await save('failure-plugin', await current().catch(() => null));
  if (frame) await save('failure-page', await frame.evaluate('document.body.innerText').catch(() => null));
  if (ui) await writeFile(join(output, 'failure.png'), Buffer.from(await ui.screenshot().catch(() => ''), 'base64'));
} finally {
  await save('results', report.results); await save('errors', errors);
  if (app) await stop('electron');
  console.log(report.summary());
}
