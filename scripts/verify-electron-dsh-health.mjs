// 打包版真实 Health 原包：诊断、修复预览、停用/恢复与原生页面。
import assert from 'node:assert/strict';
import { access, mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, createReporter, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { capturePluginDownload } from './lib/plugin-download-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const output = process.env.MAGI_DSH_HEALTH_OUTPUT || join(root, 'target/plugin-acceptance/health-business');
const work = await mkdtemp(join(tmpdir(), 'magi-health-business-'));
const port = Number(process.env.MAGI_DSH_HEALTH_PORT || 38956), cdpPort = Number(process.env.MAGI_DSH_HEALTH_CDP_PORT || 9326), origin = `http://127.0.0.1:${port}`;
const report = createReporter(), errors = [], observations = {};
const clipAudit = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE, 'utf8') : null;
let app, page, frame, ui, health, target, key;
await mkdir(output, { recursive: true });
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(240_000) });
  const value = await response.json(); assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`); return value;
}
const plugins = async () => (await api('/api/plugins')).plugins;
const current = async id => (await plugins()).find(p => p.manifest.id === id);
async function install(name, version) {
  const input = { ecosystem: 'dsh', package: name, version };
  const preview = await api('/api/plugins/upstream/preview', input);
  await api('/api/plugins/upstream/install', { ...input, expectedDigest: preview.candidate.digest });
  const plugin = (await plugins()).find(p => p.source.location === `dsh:${name}@${version}`); assert.ok(plugin);
  await api(`/api/plugins/${plugin.manifest.id}/enable`, {});
  await api(`/api/plugins/${plugin.manifest.id}/authorize`, { grants: plugin.manifest.permissions });
  await api(`/api/plugins/${plugin.manifest.id}/activate`, {});
  await waitFor(async () => (await current(plugin.manifest.id)).runtime?.state === 'running', `${name} 原包运行`, 90_000);
  return plugin;
}
async function call(path, body, headers = { 'x-plugin-health-key': key }) {
  const result = await api(`/api/plugins/${health.manifest.id}/routes`, { method: body === undefined ? 'GET' : 'POST',
    path: `/plugins/dsh-plugin-health${path}`, headers, ...(body === undefined ? {} : { body }) });
  observations[path] = result;
  assert.equal(result.status, 200, `${path}: ${JSON.stringify(result)}`); return result.body;
}
async function start() {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口空闲');
  assert.ok(!await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false), '验收 CDP 端口空闲');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, inspectPort: cdpPort + 1,
    env: { MAGI_STATE_ROOT: join(work, 'state'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_COMPAT: '', MAGI_EXTENSION_NODE: '' } });
  await waitFor(() => fetch(origin + '/health').then(r => r.ok), 'daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'Electron', 60_000); ui = pageTools(page);
}
async function stop(log) {
  frame?.close(); frame = undefined; page?.close(); page = undefined;
  if (app) { await writeFile(join(output, log), app.log.join('')); await app.stop(); app = undefined; }
}
async function audit(name, document = frame, options = { viewport: true }) {
  if (!clipAudit) return;
  const result = await document.evaluate(`(${clipAudit})(${JSON.stringify(options)})`);
  await writeFile(join(output, `${name}-clip-audit.json`), JSON.stringify(result, null, 2));
  report.check(`${name} 阴影无部分裁切`, result.cut === 0, result);
}
try {
  await start();
  health = await install('dsh-plugin-health', '3.0.5'); target = await install('dsh-plugin-release-probe', '0.0.8');
  const status = await call('/status', undefined, {}); key = status.key;
  report.check('Health 原包检查真实安装目录与当前运行实例', status.profile === 'magi' && status.review.packages.some(p => p.name === 'dsh-plugin-release-probe' && p.version === '0.0.8') && status.liveReview.entries.some(p => p.name === 'dsh-plugin-release-probe' && p.state === 'active'));
  const preview = await call('/repair', { dryRun: true });
  report.check('修复预览返回真实计划和一次性确认身份', preview.dryRun && typeof preview.planToken === 'string' && Array.isArray(preview.planned));
  const invalid = await api(`/api/plugins/${health.manifest.id}/routes`, { method: 'POST', path: '/plugins/dsh-plugin-health/repair', headers: { 'x-plugin-health-key': key }, body: { dryRun: false, planToken: 'invalid' } });
  report.check('未经预览的修复确认被拒绝，目标仍运行', invalid.status === 409 && (await current(target.manifest.id)).runtime.state === 'running');
  const repaired = await call('/repair', { dryRun: false, planToken: preview.planToken });
  report.check('健康目录的修复确认如实返回空操作，不改动安装', !repaired.dryRun && repaired.actions.length === 0 && (await current(target.manifest.id)).digest === target.digest);
  const disabled = await call('/rescue/disable', { plugin: 'dsh-plugin-release-probe' });
  report.check('Health 原包停用提案提交到 daemon 并停止目标', disabled.disabled === true && !(await current(target.manifest.id)).enabled && !(await current(target.manifest.id)).active);
  const originInfo = (await current(target.manifest.id)).disabledBy;
  report.check('停用来源与原因由 daemon 记录，未改变目标包和授权', originInfo?.pluginId === health.manifest.id && originInfo.reason.includes('dsh-plugin-health-disabled') && (await current(target.manifest.id)).digest === target.digest);
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '设置入口'); await ui.click('[data-testid=sidebar-settings]');
  await waitFor(() => ui.exists('[data-testid=settings-nav-plugins]'), '插件分类'); await ui.click('[data-testid=settings-nav-plugins]');
  const row = `[data-testid=plugin-row][data-plugin-id="${target.manifest.id}"]`;
  await waitFor(() => ui.exists(row), '目标插件'); await ui.click(row);
  await waitFor(async () => (await ui.text('[data-testid=plugin-disabled-by]'))?.includes(originInfo.reason), '原生详情显示停用来源');
  report.check('原生插件详情就地显示真实停用者和原因', (await ui.text('[data-testid=plugin-disabled-by]')).includes(originInfo.name));
  await audit('disabled-default', page, { viewport: true, root: '.store-detail' });
  await writeFile(join(output, 'disabled-source.png'), Buffer.from(await ui.screenshot(), 'base64'));
  const hover = await page.evaluate(`(()=>{const r=document.querySelector('[data-action=activate]').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`);
  await page.call('Input.dispatchMouseEvent', { type: 'mouseMoved', ...hover });
  await audit('disabled-hover', page, { viewport: true, root: '.store-detail' });
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  const narrow = await page.evaluate(`(()=>{const notice=document.querySelector('[data-testid=plugin-disabled-by]');notice.scrollIntoView({block:'center'});const r=notice.getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom,width:innerWidth,height:innerHeight,scrollWidth:notice.scrollWidth,clientWidth:notice.clientWidth};})()`);
  report.check('390px 视口的停用原因完整换行且可见，无横向溢出', narrow.left >= 0 && narrow.right <= narrow.width && narrow.top >= 0 && narrow.bottom <= narrow.height && narrow.scrollWidth <= narrow.clientWidth, narrow);
  await writeFile(join(output, 'disabled-source-390.png'), Buffer.from(await ui.screenshot(), 'base64'));
  await page.call('Emulation.clearDeviceMetricsOverride');
  await stop('before-restart.log'); await start();
  await waitFor(async () => (await current(health.manifest.id)).runtime?.state === 'running', 'Health 重启运行', 90_000);
  const restartedStatus = await call('/status', undefined, {}); key = restartedStatus.key;
  report.check('应用重启保留停用来源与意愿，目标没有被自动启动', !(await current(target.manifest.id)).enabled && JSON.stringify((await current(target.manifest.id)).disabledBy) === JSON.stringify(originInfo));
  const enabled = await call('/rescue/enable', { plugin: 'dsh-plugin-release-probe' });
  report.check('Health 原包恢复提案使真实目标重新运行', enabled.enabled === true && (await current(target.manifest.id)).runtime.state === 'running' && !(await current(target.manifest.id)).disabledBy);
  const handoff = await call('/rescue/handoff', { plugin: 'dsh-plugin-release-probe', version: '0.0.8' });
  report.check('诊断交接生成原包报告文件与可用提示词', handoff.files.length > 0 && handoff.prompt.includes('dsh-plugin-release-probe') && (await readFile(join(handoff.directory, 'prompt.md'), 'utf8')) === handoff.prompt);
  const privateProfile = join(work, 'state/extensions/data', health.manifest.id, 'profiles/magi');
  const residue = join(privateProfile, 'verification-residue.lock');
  await writeFile(residue, 'RECOVERABLE_PRIVATE_DATA');
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '设置入口'); await ui.click('[data-testid=sidebar-settings]');
  const view = health.manifest.contributions.views.find(v => v.placements.includes('settings'));
  const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${health.manifest.id}:${view.id}`)}]`;
  await waitFor(() => ui.exists(nav), 'Health 原页面入口'); await ui.click(nav);
  const selector = `iframe[data-plugin-id="${health.manifest.id}"]`;
  const url = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`), 'Health iframe');
  frame = await waitFor(() => connectTarget(cdpPort, t => ['iframe', 'page'].includes(t.type) && t.url === url, 'Health 原页面', errors), '原页面实例');
  await waitFor(() => frame.evaluate('/清理\\s*1|Clean up\\s*1/i.test(document.body.innerText)'), '原包显示真实修复发现');
  observations.page = await frame.evaluate('({text:document.body.innerText,buttons:[...document.querySelectorAll("button")].map(b=>({text:b.innerText,disabled:b.disabled}))})');
  await writeFile(join(output, 'health.png'), Buffer.from(await ui.screenshot(), 'base64'));
  report.check('原生 Health 页面展示实际诊断与可执行的修复入口', true);
  const clickText = async expression => frame.evaluate(`(()=>{const button=[...document.querySelectorAll('button')].find(b=>${expression});if(!button)return false;button.click();return true;})()`);
  const selectMode = mode => frame.evaluate(`(()=>{const select=document.querySelector('.dph-mode select');if(!select)throw new Error('原包显示方式入口缺失');select.value=${JSON.stringify(mode)};select.dispatchEvent(new Event('change',{bubbles:true}));})()`);
  await selectMode('advanced');
  const snapshot = await capturePluginDownload({ inspectPort: cdpPort + 1, pid: app.child.pid }, join(output, 'exports'), async () => {
    await waitFor(() => clickText(`b.getClientRects().length > 0 && /下载当前检查记录|下载当前配置快照|Download current check record|Download current snapshot/.test(b.innerText)`), '原包下载检查记录入口');
  });
  const snapshotData = JSON.parse(snapshot.bytes.toString());
  report.check('Health 原页面导出真实检查 JSON 文件，包含当前原包身份', snapshot.state === 'completed'
    && snapshot.suggestedFilename === 'dsh-profile-snapshot.json' && snapshotData.schemaVersion === 1
    && snapshotData.packages.some(p => p.name === 'dsh-plugin-health' && p.version === health.manifest.version));
  await selectMode('daily');
  await waitFor(() => clickText(`/清理\\s*1|Clean up\\s*1/i.test(b.innerText)`), '原包非空修复计划入口');
  await waitFor(() => frame.evaluate(`document.body.innerText.includes('确认修复') || document.body.innerText.includes('Confirm repair')`), '原包修复确认');
  report.check('原页面先显示非空修复预览，确认前文件仍存在', await access(residue).then(() => true, () => false));
  await audit('repair-preview');
  await writeFile(join(output, 'repair-preview.png'), Buffer.from(await ui.screenshot(), 'base64'));
  assert.ok(await clickText(`['取消','Cancel'].includes(b.innerText.trim())`));
  await waitFor(() => frame.evaluate(`![...document.querySelectorAll('button')].some(b=>['确认修复','Confirm repair'].includes(b.innerText.trim()))`), '原包取消修复');
  report.check('取消原包修复预览保留真实文件，可重新发起', await access(residue).then(() => true, () => false));
  await waitFor(() => clickText(`/清理\\s*1|Clean up\\s*1/i.test(b.innerText)`), '取消后重新预览修复');
  await waitFor(() => frame.evaluate(`[...document.querySelectorAll('button')].some(b=>['确认修复','Confirm repair'].includes(b.innerText.trim()))`), '重新确认修复');
  assert.ok(await clickText(`['确认修复','Confirm repair'].includes(b.innerText.trim())`));
  await waitFor(async () => !await access(residue).then(() => true, () => false), '真实文件修复隔离');
  const privateHome = dirname(dirname(privateProfile));
  const runs = await import('node:fs/promises').then(fs => fs.readdir(join(privateHome, '.plugin-health/quarantine')));
  const ledgers = await Promise.all(runs.map(run => readFile(join(privateHome, '.plugin-health/quarantine', run, 'repair-log.json'), 'utf8').then(JSON.parse)));
  const action = ledgers.flatMap(ledger => ledger.actions).find(item => item.from === residue);
  report.check('原页面确认后将真实文件隔离并保留恢复日志与原字节', action?.kind === 'quarantine' && await readFile(action.to, 'utf8') === 'RECOVERABLE_PRIVATE_DATA');
  report.check('修复业务文件不生成第二份安装元数据', !await access(join(privateProfile, 'package.json')).then(() => true, () => false) && !await access(join(privateProfile, 'node_modules')).then(() => true, () => false));
  await waitFor(() => frame.evaluate(`![...document.querySelectorAll('button')].some(b=>['确认修复','Confirm repair'].includes(b.innerText.trim()))`), '原包修复完成反馈');
  await audit('repair-complete');
  await writeFile(join(output, 'repair-complete.png'), Buffer.from(await ui.screenshot(), 'base64'));
  await waitFor(() => clickText(`!b.disabled && /^(检查更新|Check for updates|Check updates)$/.test(b.innerText.trim())`), 'Health 原页面主动在线检查更新');
  const update = await waitFor(async () => {
    const value = await call('/update'); return value.status === 'ok' && value.updateAvailable && value;
  }, 'Health 原包当前在线版本', 30_000);
  const beforeUpdate = await current(health.manifest.id);
  const beforeSettings = await api(`/api/plugins/${health.manifest.id}/settings`);
  const beforeDocument = await frame.evaluate('window.__healthSelfUpdate=crypto.randomUUID()');
  await waitFor(() => clickText(`!b.disabled && /^(立即更新|Update now)$/.test(b.innerText.trim())`), 'Health 原页面自更新入口');
  const afterUpdate = await waitFor(async () => {
    const plugin = await current(health.manifest.id);
    return plugin.manifest.version === update.latestVersion && plugin.digest !== beforeUpdate.digest && plugin.runtime?.state === 'running' && plugin;
  }, 'Health 原页面自更新后实际新版宿主运行', 300_000);
  frame.close();
  const updatedUrl = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src.includes(${JSON.stringify(afterUpdate.digest)})&&document.querySelector(${JSON.stringify(selector)}).src`), 'Health 自更新替换原文档');
  frame = await waitFor(() => connectTarget(cdpPort, t => ['iframe', 'page'].includes(t.type) && t.url === updatedUrl, 'Health 更新后原页面', errors), 'Health 新版页面');
  const newStatus = await call('/status', undefined, {}); key = newStatus.key;
  const journal = await waitFor(async () => {
    const state = JSON.parse(await readFile(join(work, 'state/plugins/installations.json'), 'utf8'));
    return Object.values(state.records).find(record => record.plugin === health.manifest.id && record.candidate?.digest === afterUpdate.digest && record.phase === 'finished');
  }, 'daemon 自更新持久终态');
  observations.selfUpdate = { before: beforeUpdate, after: afterUpdate, result: journal.result };
  await audit('self-update-complete');
  await writeFile(join(output, 'health-self-update.png'), Buffer.from(await ui.screenshot(), 'base64'));
  report.check('Health 原页面在线自更新由 daemon 完成替换与实际新版运行，旧调用结束不丢提交', journal.result.application === 'applied'
    && afterUpdate.active && newStatus.update.currentVersion === update.latestVersion, journal.result);
  report.check('自更新保留原设置和已批权限范围，新文档不复用旧页面状态', JSON.stringify(await api(`/api/plugins/${health.manifest.id}/settings`)) === JSON.stringify(beforeSettings)
    && afterUpdate.grants.every(permission => beforeUpdate.grants.some(grant => JSON.stringify(grant) === JSON.stringify(permission)))
    && typeof beforeDocument === 'string' && await frame.evaluate('window.__healthSelfUpdate===undefined'));
  await stop('before-self-update-restart.log'); await start();
  await waitFor(async () => (await current(health.manifest.id)).runtime?.state === 'running', '自更新后应用重启恢复新版', 90_000);
  report.check('应用重启保留自更新最终摘要及配置，不重放旧任务', (await current(health.manifest.id)).digest === afterUpdate.digest
    && JSON.stringify(await api(`/api/plugins/${health.manifest.id}/settings`)) === JSON.stringify(beforeSettings));
  report.check('本轮没有页面运行异常', errors.length === 0, errors);
} catch (error) {
  report.results.push({ name: 'Health 业务验收完成', ok: false, detail: String(error.stack || error) }); process.stderr.write(`${error.stack || error}\n`); process.exitCode = 1;
  if (ui) await writeFile(join(output, 'failure.png'), Buffer.from(await ui.screenshot(), 'base64')).catch(() => {});
} finally {
  await stop('electron.log');
  await writeFile(join(output, 'results.json'), JSON.stringify({ work, errors, checks: report.results, observations }, null, 2));
  process.stdout.write(`${report.summary()}\n`);
  if (report.results.some(check => !check.ok)) process.exitCode = 1;
}
