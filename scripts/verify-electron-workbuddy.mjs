// 在线安装真实 npm 原包并调用本机 WorkBuddy 登录态；会产生真实模型请求，按需显式运行。
// 独立 Magi / DSH 状态根，不导出凭据，不修改用户已有 Magi 会话。
import assert from 'node:assert/strict';
import { cp, mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = 38938, cdpPort = 9308, origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-workbuddy-live-'));
const output = join(root, 'target/plugin-acceptance/workbuddy');
const report = createReporter(), errors = [];
let app, page, frame, ui, pluginId;
await mkdir(output, { recursive: true });
await mkdir(join(work, 'dsh'), { mode: 0o700 });
// 沿用该用户的设备身份；账号仍由未经修改的原包从 WorkBuddy 桌面登录目录发现。
await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(work, 'dsh/.workbuddy-device.json')).catch((error) => { if (error.code !== 'ENOENT') throw error; });
const api = async (path, body) => {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(180_000) });
  const value = await response.json();
  assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`);
  return value;
};
const installed = async () => (await api('/api/plugins')).plugins.find((p) => p.manifest.name === 'DSH Connect WorkBuddy');
const screenshot = async (name) => writeFile(join(output, `${name}.png`), Buffer.from(await ui.screenshot(), 'base64'));
const tab = async (id) => {
  if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) await ui.click('[data-testid=sidebar-settings]');
  await waitFor(async () => {
    const selector = `[data-testid="settings-nav-${id}"]`;
    if (!await ui.exists(selector)) return false;
    await ui.click(selector);
    return page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.getAttribute('aria-current') === 'page'`);
  }, `设置分类 ${id}`);
};
try {
  report.check('独立验收端口空闲', !await fetch(origin + '/health').then(() => true, () => false)
    && !await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false));
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '真实 Magi 主窗口', 60_000);
  ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台');
  await tab('plugins');
  await waitFor(() => ui.exists('[data-testid=plugin-search]'), '插件搜索');
  await page.evaluate('const input=document.querySelector("[data-testid=plugin-search]"); input.value="dsh-connect-workbuddy"; input.dispatchEvent(new Event("input",{bubbles:true})); true');
  const upstreamRow = '[data-testid=plugin-row][data-plugin-id="upstream:dsh:dsh-connect-workbuddy"]';
  await waitFor(() => ui.exists(upstreamRow), 'npm 在线搜索结果', 60_000);
  await ui.click(upstreamRow);
  report.check('在线搜索呈现真实 DSH 上游包', await ui.exists('[data-testid=plugin-upstream-badges]'));
  await ui.click('[data-testid=plugin-detail-install]');
  const preview = await waitFor(async () => {
    if (await ui.exists('[data-testid=plugin-candidate][data-origin=upstream]')) return { ok: true };
    const error = await page.evaluate('[...document.querySelectorAll("[role=status]")].map(e=>e.textContent).find(t=>t.includes("翻译上游插件失败"))');
    return error && { error };
  }, '原包在线下载与翻译预览', 180_000);
  assert.ok(preview.ok, preview.error);
  const previewText = await ui.text('[data-testid=plugin-candidate]');
  report.check('预览显示 npm 完整性摘要与可选兼容限制', /sha512-/.test(previewText) && /可选路径/.test(previewText), previewText?.slice(-1500));
  await screenshot('install-preview');
  await ui.click('[data-action=confirm-candidate]');
  const plugin = await waitFor(installed, '确认在线安装', 120_000);
  pluginId = plugin.manifest.id;
  report.check(`已在线安装 ${plugin.manifest.name}@${plugin.manifest.version}`, plugin.source.kind === 'upstream');
  const row = `[data-testid=plugin-row][data-plugin-id="${pluginId}"]`;
  await waitFor(() => ui.exists(row), '已安装列表项');
  await ui.click(row);
  await waitFor(() => page.evaluate('Boolean(document.querySelector("[data-action=activate]")) && !document.querySelector("[data-action=activate]").disabled'), '授权激活');
  await ui.click('[data-action=activate]');
  const activated = await waitFor(async () => {
    const item = await installed();
    return ['running', 'failed'].includes(item.runtime?.state) && item;
  }, '真实 WorkBuddy 原包运行', 120_000);
  assert.equal(activated.runtime.state, 'running', activated.runtime.reason);
  report.check('真实 Cordis 原包激活成功', true);
  const groups = await waitFor(async () => {
    const catalog = (await api('/api/settings/models/catalog')).groups.filter((g) => g.pluginId === pluginId);
    return catalog.some((g) => g.models.length) && catalog;
  }, 'WorkBuddy 模型目录', 90_000);
  const selected = groups.flatMap((g) => g.models.map((m) => ({ sourceId: g.sourceId, ...m })))
    .find((m) => m.id === (process.env.MAGI_WORKBUDDY_MODEL || 'hy3'));
  assert.ok(selected, '所选 WorkBuddy 模型必须真实出现在目录；不自动改用其他模型');
  report.check('真实 WorkBuddy 模型数据源进入主对话目录', Boolean(selected));
  await tab(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists('.settings-frame iframe'), '原包设置界面');
  const frameUrl = await page.evaluate('document.querySelector(".settings-frame iframe").src');
  frame = await waitFor(() => connectTarget(cdpPort, (t) => (t.type === 'iframe' || t.type === 'page') && t.url === frameUrl, 'WorkBuddy 原生设置', errors), 'WorkBuddy 设置 iframe');
  await waitFor(() => frame.evaluate('document.querySelectorAll("button").length > 3 && !document.body.innerText.includes("界面加载失败")'), '原生设置卡片渲染', 60_000);
  report.check('未经修改的上游设置卡片实际渲染', true);
  await screenshot('settings');
  const session = (await api('/api/session/materialize', { scope: 'personal' })).sessionId;
  await api('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session, config: { sourceId: selected.sourceId, model: selected.id, reasoningEffort: null } });
  const turn = await api('/api/session/turn', { scope: 'personal', sessionId: session, text: 'Reply with exactly MAGI_WORKBUDDY_LIVE_OK.', requestId: 'workbuddy-live-1', userMessageId: 'workbuddy-live-user-1' });
  const finished = await waitFor(async () => {
    const turns = (await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).canonicalTurns;
    return turns?.find((t) => t.turnId === turn.turnId && ['completed', 'failed', 'cancelled', 'blocked'].includes(t.status));
  }, '真实 WorkBuddy 模型回复', 180_000);
  const assistantText = finished.items.filter((item) => item.kind === 'assistant_text').map((item) => item.content ?? '').join('\n').trim();
  report.check(`真实 WorkBuddy 登录态与 ${selected.id} 完成模型调用`, finished.status === 'completed' && assistantText === 'MAGI_WORKBUDDY_LIVE_OK',
    JSON.stringify({ turnId: finished.turnId, status: finished.status, model: selected.id, assistantText }));
  frame.close(); frame = null;
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}` });
  await waitFor(() => page.evaluate('document.body.innerText.includes("MAGI_WORKBUDDY_LIVE_OK")'), '真实回复展示');
  await screenshot('conversation');
  await tab('plugins');
  await waitFor(() => ui.exists(row), '插件列表'); await ui.click(row);
  await ui.click('[data-action=deactivate]');
  await waitFor(async () => !(await installed()).active, '停用原包');
  report.check('停用回收真实 WorkBuddy 模型数据源', !(await api('/api/settings/models/catalog')).groups.some((g) => g.pluginId === pluginId));
  await waitFor(() => page.evaluate('Boolean(document.querySelector("[data-action=activate]")) && !document.querySelector("[data-action=activate]").disabled'), '再次激活'); await ui.click('[data-action=activate]');
  await waitFor(async () => (await installed()).runtime?.state === 'running', '重新激活', 90_000);
  report.check('重新激活真实原包成功', true);
  report.check('主窗口与上游设置无未捕获异常', errors.length === 0 && !/Uncaught|可信 Renderer 控制台错误/.test(app.log.join('')), errors.join('\n'));
  await writeFile(join(output, 'installed.json'), JSON.stringify({ pluginId, version: plugin.manifest.version, source: plugin.source, model: selected.id, sessionId: session, turnId: turn.turnId, stateRoot: join(work, 'state') }, null, 2));
  console.log(report.summary());
} catch (error) {
  console.error(error);
  if (ui) await screenshot('failure').catch(() => {});
  process.exitCode = 1;
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(report.results, null, 2));
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  frame?.close(); page?.close();
  // 保留独立状态便于用户继续验证；凭据仅留在临时目录，不写入截图／结果文件。
  console.log(`独立验收状态：${work}`);
}
