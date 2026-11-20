// 在线安装真实 npm 原包并调用本机 WorkBuddy 登录态；会产生真实模型请求，按需显式运行。
// 独立 Magi / DSH 状态根，不导出凭据，不修改用户已有 Magi 会话。
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cp, mkdir, mkdtemp, writeFile, readFile } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';
import { verifyWorkbuddyStats } from './lib/workbuddy-stats-acceptance.mjs';
import { installFixedUpstreamFromNative, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_WORKBUDDY_PORT ?? 38938);
const cdpPort = Number(process.env.MAGI_WORKBUDDY_CDP_PORT ?? 9308), origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-workbuddy-live-'));
const output = process.env.MAGI_WORKBUDDY_OUTPUT || join(root, 'target/plugin-acceptance/workbuddy');
const report = createReporter(), errors = [];
const lifecycleRequests = [];
const packageName = 'dsh-connect-workbuddy', version = '5.21.2';
const stateRoot = join(work, 'state');
const baseline = JSON.parse(await readFile(new URL('../docs/plugin-validation/phase-1/manifest.json', import.meta.url)));
let app, page, frame, ui, pluginId;
await mkdir(output, { recursive: true });
const dshHome = join(stateRoot, 'plugins', 'dsh-home');
await mkdir(dshHome, { recursive: true, mode: 0o700 });
// 使用独立状态；登录目录仍由原包自己读取。
await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(dshHome, '.workbuddy-device.json'))
  .catch(error => { if (error.code !== 'ENOENT') throw error; });
const api = async (path, body, method = body === undefined ? 'GET' : 'POST') => {
  const response = await fetch(origin + path, { method, headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(180_000) });
  const raw = await response.text();
  const value = raw ? JSON.parse(raw) : null;
  assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`);
  return value;
};
const installed = async () => (await api('/api/plugins')).plugins.find((p) => p.manifest.id === packageName);
const screenshot = async (name) => {
  // DOM 已渲染不等于合成帧已提交；截图要记录实际完成绘制的页面。
  await page.evaluate('new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))');
  await writeFile(join(output, `${name}.png`), Buffer.from(await ui.screenshot(), 'base64'));
};
const tab = async (id) => {
  if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) await ui.click('[data-testid=sidebar-settings]');
  await waitFor(async () => {
    const selector = `[data-testid="settings-nav-${id}"]`;
    if (!await ui.exists(selector)) return false;
    await ui.click(selector);
    return page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.getAttribute('aria-current') === 'page'`);
  }, `设置分类 ${id}`);
};
async function start() {
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    env: { MAGI_STATE_ROOT: stateRoot, MAGI_HOST: '127.0.0.1', MAGI_DESKTOP_DAEMON_PORT: String(port),
      MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_DSH_SDK_ROOT: '', MAGI_DESKTOP_REUSE_DAEMON: '' } });
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '真实 Magi 主窗口', 60_000);
  await page.call('Network.enable');
  page.on('Network.requestWillBeSent', ({ request }) => {
    const action = request.url.match(/\/api\/plugins\/[^/]+\/(enable|disable)$/)?.[1];
    if (action && request.method === 'POST') lifecycleRequests.push({ action, ...JSON.parse(request.postData || '{}') });
  });
  ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台');
}
async function openSettings() {
  frame?.close();
  await tab(`plugin:${pluginId}:settings`);
  const url = await waitFor(() => page.evaluate('document.querySelector(".settings-frame:not([hidden]) iframe")?.src'), '原包设置界面');
  frame = await waitFor(() => connectTarget(cdpPort, target => ['iframe', 'page'].includes(target.type) && target.url === url,
    'WorkBuddy 原包设置', errors), '原包设置 iframe');
  await waitFor(() => frame.evaluate('document.querySelectorAll("button").length > 3 && !document.body.innerText.includes("界面加载失败")'), '原包设置卡片', 60_000);
}
const poolTab = `[...document.querySelectorAll('button,[role=tab]')].find(node => /^(账号池|Account pool)$/i.test(node.innerText.trim()))`;
const selectedAccounts = () => frame.evaluate(
  String.raw`document.body.innerText.match(/^(?:已选|Selected)\s+\d+\s*\/\s*\d+$/im)?.[0]`);
async function accountPool() {
  await waitFor(() => frame.evaluate(`(() => {
    if (document.querySelector('.dsm-workbuddy-pool-members-actions')) return true;
    (${poolTab})?.click();
    return false;
  })()`), '原包账号池');
  return await waitFor(selectedAccounts, '原包账号池成员计数');
}
const patchPath = join(stateRoot, 'plugins', 'loading-source', 'cordis.patch.yml');
try {
  report.check('独立验收端口空闲', !await fetch(origin + '/health').then(() => true, () => false)
    && !await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false));
  await start();
  const preview = await api('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: packageName, version });
  const sample = baseline.packages.find(row => row.name === packageName && row.version === version);
  assert.equal(preview.candidate.digest, sample.sha256);
  assert.equal(preview.integrity, sample.integrity);
  const plugin = await installFixedUpstreamFromNative({ page, request: api, packageName, version });
  pluginId = plugin.manifest.id;
  const row = `[data-testid=plugin-row][data-plugin-id="${pluginId}"]`;
  report.check('固定官方原包从设置页在线安装并启用', plugin.manifest.version === version
    && plugin.source.location === `dsh:${packageName}@${version}`
    && plugin.enabled && plugin.runtime.state === 'running' && lifecycleRequests.length === 0);
  const selected = await waitForPluginModel({ request: api, pluginId, modelId: process.env.MAGI_WORKBUDDY_MODEL || 'deepseek-v4.1-flash',
    record: value => writeFile(join(output, 'model-selection.json'), JSON.stringify(value, null, 2)) });
  report.check('真实 WorkBuddy 模型数据源进入主对话目录', Boolean(selected));
  await openSettings();
  report.check('原包配置在独立设置 Tab 展示', await page.evaluate(`document.querySelector(${JSON.stringify(`[data-testid="settings-nav-plugin:${pluginId}:settings"]`)})?.getAttribute('aria-current') === 'page'
    && document.querySelector('.settings-frame:not([hidden]) iframe')?.dataset.pluginId === ${JSON.stringify(pluginId)}`));
  const beforeMembers = await accountPool();
  const patchBefore = await readFile(patchPath, 'utf8');
  const pick = /^\D*0\s*\//.test(beforeMembers) ? '全选|Select all' : '全不选|Select none|Deselect all';
  await frame.evaluate(`[...document.querySelectorAll('.dsm-workbuddy-pool-members-actions button')]
    .find(node => new RegExp(${JSON.stringify('^(?:' + pick + ')$')}, 'i').test(node.innerText.trim())).click()`);
  const afterMembers = await waitFor(async () => {
    const count = await selectedAccounts();
    return count !== beforeMembers && count;
  }, '原包修改成员后显示草稿');
  await waitFor(() => frame.evaluate(`(() => {
    const button = [...document.querySelectorAll('.dsm-workbuddy-pool button')].find(node => /^(保存|Save)$/.test(node.innerText.trim()));
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`), '原包保存草稿');
  await waitFor(async () => (await readFile(patchPath, 'utf8')) !== patchBefore, 'daemon 提交原包配置');
  await waitFor(() => frame.evaluate('!document.querySelector(".dsm-workbuddy-pool-dirty")'), '原包显示已保存');
  await tab('plugins');
  await openSettings();
  await accountPool();
  await waitFor(async () => await selectedAccounts() === afterMembers, '切回后页面读取已提交账号池');
  report.check('原包设置保存后切回仍读到新成员数', true);
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
  const statsVerification = process.env.MAGI_WORKBUDDY_STATS === '1' ? await verifyWorkbuddyStats({ request:api,page,session,output,check:report.check }) : null;
  const cancelled = await api('/api/session/turn', { scope: 'personal', sessionId: session,
    text: 'Write a long explanation of the tides. Do not call tools.', requestId: 'workbuddy-cancel', userMessageId: 'workbuddy-cancel-user' });
  const readCancelled = async () => (await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).canonicalTurns
    .find(value => value.turnId === cancelled.turnId);
  await waitFor(async () => (await readCancelled())?.modelSteps.some(step => step.attempts?.some(attempt => !attempt.settledAt)), '模型请求在途');
  await api('/api/session/interrupt', { sessionId: session });
  const cancelledTurn = await waitFor(async () => {
    const value = await readCancelled();
    return value?.status === 'cancelled' && value;
  }, '原生取消提交');
  report.check('在途 WorkBuddy 模型请求可由 Magi 取消', cancelledTurn.status === 'cancelled');
  const beforeCrash = await installed();
  const dropped = await api('/api/session/turn', { scope: 'personal', sessionId: session,
    text: 'Write a long explanation of tidal currents. Do not call tools.', requestId: 'workbuddy-host-crash', userMessageId: 'workbuddy-host-crash-user' });
  const readDropped = async () => (await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).canonicalTurns
    .find(value => value.turnId === dropped.turnId);
  await waitFor(async () => (await readDropped())?.modelSteps.some(step => step.attempts?.some(attempt => !attempt.settledAt)), '宿主崩溃场景的在途请求');
  const processes = execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,command='], { encoding: 'utf8' })
    .trim().split('\n').map(line => line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/)).filter(Boolean);
  const daemons = processes.filter(row => Number(row[2]) === app.child.pid && row[3].includes('/daemon/magi-daemon-app'));
  assert.equal(daemons.length, 1, '等待隔离验收应用的唯一 daemon');
  const hosts = processes.filter(row => row[2] === daemons[0][1] && row[3].includes('/dsh/runtime/entry.mjs'));
  assert.equal(hosts.length, 1, '只终止本轮独立 daemon 的共享插件宿主');
  process.kill(Number(hosts[0][1]), 'SIGKILL');
  console.log('已终止验收应用的共享插件宿主，核对调用与新代次');
  const droppedTurn = await waitFor(async () => {
    const value = await readDropped();
    await writeFile(join(output, 'host-crash-turn.json'), JSON.stringify(value, null, 2));
    return ['failed', 'cancelled', 'completed', 'blocked'].includes(value?.status) && value;
  }, '宿主崩溃后在途模型请求结算', 60_000);
  report.check('宿主崩溃期间的已开始模型请求结束且未重新派发',
    droppedTurn.status !== 'completed' && droppedTurn.modelSteps.flatMap(step => step.attempts ?? []).length === 1,
    JSON.stringify({ status: droppedTurn.status, attempts: droppedTurn.modelSteps.flatMap(step => step.attempts ?? []).length }));
  const afterCrash = await waitFor(async () => {
    const row = await installed();
    return row?.runtime?.state === 'running' && row.runtime.instanceId !== beforeCrash.runtime.instanceId && row;
  }, '共享宿主自动恢复 WorkBuddy 新实例', 90_000);
  assert.equal(afterCrash.digest, beforeCrash.digest);
  await waitForPluginModel({ request: api, pluginId, modelId: selected.id, record: async () => {} });
  report.check('崩溃后恢复固定原包模型目录及新实例', true);
  await tab(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists(`iframe[data-plugin-id="${pluginId}"]`), '停用前真实原包文档');
  const beforeStop = await installed();
  const staleDocument = await page.evaluate(`document.querySelector('iframe[data-plugin-id="${pluginId}"]').src`);
  await tab('plugins');
  await waitFor(() => ui.exists(row), '插件列表'); await ui.click(row);
  await ui.click('[data-action=disable]');
  await waitFor(async () => !(await installed()).enabled, '停用原包');
  report.check('停用回收真实 WorkBuddy 模型数据源', !(await api('/api/settings/models/catalog')).groups.some((g) => g.pluginId === pluginId));
  await waitFor(() => page.evaluate(`!document.querySelector('iframe[data-plugin-id="${pluginId}"]')`), '停用回收保活设置文档');
  const deniedDocument = await fetch(staleDocument);
  report.check('停用回收隐藏页面，旧文档地址不能继续取得业务页面', [403,409].includes(deniedDocument.status));
  await tab(`plugin:${pluginId}:settings`);
  const requestOffset = lifecycleRequests.length;
  await waitFor(() => ui.exists('[data-testid=plugin-page-header] [role=switch]:not([disabled])'), '设置页的重新启用开关');
  await ui.click('[data-testid=plugin-page-header] [role=switch]');
  await waitFor(async () => (await installed()).runtime?.state === 'running', '重新启用', 90_000);
  const reactivated = await installed();
  report.check('设置页重新启用创建新实例，只发出一次启用请求', reactivated.runtime.instanceId !== beforeStop.runtime.instanceId
    && lifecycleRequests.slice(requestOffset).map(item => item.action).join(',') === 'enable'
    && lifecycleRequests.slice(requestOffset).every(item => item.expectedDigest === plugin.digest));
  await waitFor(() => ui.exists(`iframe[data-plugin-id="${pluginId}"]`), '重新启用后的原包文档');
  await writeFile(join(output, 'lifecycle-requests.json'), JSON.stringify(lifecycleRequests, null, 2));
  const beforeRestart = await installed();
  const statsBeforeRestart = statsVerification && await api('/api/settings/stats?sessionId=' + encodeURIComponent(statsVerification.sessionId));
  frame?.close(); frame = null; page.close(); page = null;
  await writeFile(join(output, 'before-restart.log'), app.log.join('')); await app.stop();
  await start();
  const restored = await waitFor(async () => { const item = await installed(); return item?.runtime?.state === 'running' && item; }, '应用重启恢复原包', 120_000);
  await waitForPluginModel({ request: api, pluginId, modelId: selected.id,
    record: value => writeFile(join(output, 'model-selection-restarted.json'), JSON.stringify(value, null, 2)) });
  report.check('应用重启保持同一原包、授权和配置并恢复新宿主及真实模型目录', restored.digest === beforeRestart.digest
    && restored.runtime.instanceId !== beforeRestart.runtime.instanceId
    && restored.enabled);
  await openSettings();
  await accountPool();
  await waitFor(async () => await selectedAccounts() === afterMembers, '重启后页面读取已提交账号池');
  report.check('应用重启后原页面读取已保存成员数', true);
  const restoredCancellation = await readCancelled();
  report.check('应用重启后保持取消终态和原执行次数', restoredCancellation.status === 'cancelled'
    && restoredCancellation.modelSteps.length === cancelledTurn.modelSteps.length);
  if (statsVerification) {
    const restoredStats = await api('/api/settings/stats?sessionId='+encodeURIComponent(statsVerification.sessionId));
    await writeFile(join(output,'stats-restored.json'),JSON.stringify(restoredStats,null,2));
    report.check('完整重启恢复包含取消调用的统计，无重复计数或模型重放',
      JSON.stringify(restoredStats.totals) === JSON.stringify(statsBeforeRestart.totals));
  }
  report.check('主窗口与上游设置无未捕获异常', errors.length === 0 && !/Uncaught|可信 Renderer 控制台错误/.test(app.log.join('')), errors.join('\n'));
  const resources = join(appRoot, 'Contents', 'Resources');
  const build = JSON.parse(await readFile(join(resources, 'browser-capability-manifest.json')));
  const state = JSON.parse(await readFile(join(stateRoot, 'plugins', 'state.json')));
  const lock = await readFile(join(state.loading.configuration.preparation.projectDirectory, 'package-lock.json'));
  await writeFile(join(output, 'package-lock.json'), lock);
  await writeFile(join(output, 'result.json'), JSON.stringify({ completed: true, package: packageName, version,
    archiveSha256: sample.sha256, integrity: sample.integrity, model: selected.id, buildIdentity: build.buildIdentity,
    daemonSha256: build.components.daemon.sha256,
    sdkLockSha256: build.files.find(file => file.path === 'daemon/dsh/package-lock.json').sha256,
    dependenciesSha256: createHash('sha256').update(lock).digest('hex'),
    checks: report.results.map(({ name, ok }) => ({ name, ok })) }, null, 2) + '\n');
  console.log(report.summary());
} catch (error) {
  console.error(error);
  if (frame) await writeFile(join(output, 'failure-page.txt'), await frame.evaluate('document.body.innerText').catch(() => ''));
  if (ui) await screenshot('failure').catch(() => {});
  process.exitCode = 1;
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(report.results, null, 2));
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  frame?.close(); page?.close();
  // 保留独立状态便于用户继续验证；凭据仅留在临时目录，不写入截图／结果文件。
  console.log(`独立验收状态：${work}`);
}
