// Health 专项与固定社区批次执行同一原包业务合同，不合并不同运行的结果。
import assert from 'node:assert/strict';
import { access, readdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { connectTarget, pageTools, waitFor } from './electron-harness.mjs';
import { capturePluginDownload } from './plugin-download-acceptance.mjs';

export async function verifyDshHealth({ request: api, plugin: health, target, page, cdpPort, errors, output, stateRoot, inspector, restart, check, clipAudit }) {
  assert.equal(health.manifest.version, '3.0.5', 'Health 业务合同固定验收初始原包版本');
  const checks = {}, observations = {};
  const evidence = { package: 'dsh-plugin-health@3.0.5', pluginId: health.manifest.id, digest: health.digest, completed: false, checks, observations };
  const record = (label, ok, detail) => { check(label, ok, detail); checks[label] = true; };
  const current = async id => (await api('/api/plugins')).plugins.find(plugin => plugin.manifest.id === id);
  let ui = pageTools(page), frame, key;
  const selector = `iframe[data-plugin-id="${health.manifest.id}"]`;
  const view = health.manifest.contributions.views.find(v => v.placements.includes('settings'));
  const openPage = async () => {
    closeFrame();
    // 按用户入口关闭／重开设置页；应用整页导航是另一种生命周期。
    if (await ui.exists('[data-testid=settings-back]')) await ui.click('[data-testid=settings-back]');
    await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '设置入口'); await ui.click('[data-testid=sidebar-settings]');
    const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${health.manifest.id}:${view.id}`)}]`;
    await waitFor(() => ui.exists(nav), 'Health 原页面入口'); await ui.click(nav);
    const url = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`), 'Health iframe');
    frame = await waitFor(() => connectTarget(cdpPort, t => ['iframe', 'page'].includes(t.type) && t.url === url, 'Health 原页面', errors), '原页面实例');
    await waitFor(() => frame.evaluate('Boolean(document.querySelector(".dph-mode select"))'), 'Health 原包页面就绪');
  };
  const closeFrame = () => { frame?.close(); frame = undefined; };
  const restartApp = async label => {
    closeFrame(); page = await restart(label); ui = pageTools(page);
  };
  const call = async (path, body, headers = { 'x-plugin-health-key': key }) => {
    const result = await api(`/api/plugins/${health.manifest.id}/routes`, { method: body === undefined ? 'GET' : 'POST',
      path: `/plugins/dsh-plugin-health${path}`, headers, ...(body === undefined ? {} : { body }) });
    observations[path] = result;
    assert.equal(result.status, 200, `${path}: ${JSON.stringify(result)}`); return result.body;
  };
  const audit = async (name, document = frame, options = { viewport: true }) => {
    if (!clipAudit) return;
    const result = await document.evaluate(`(${clipAudit})(${JSON.stringify(options)})`);
    await writeFile(join(output, `${name}-clip-audit.json`), JSON.stringify(result, null, 2));
    record(`${name} 阴影无部分裁切`, result.cut === 0, result);
  };
  try {
    const reviewed = status => status.profile === 'magi' && status.review.packages.some(p => p.name === 'dsh-plugin-release-probe' && p.version === '0.0.8') && status.liveReview.entries.some(p => p.name === 'dsh-plugin-release-probe' && p.state === 'active');
    // Health 对 /status 有 5 秒节流缓存：更早的读取（目标安装前）可能仍在缓存里。
    // 等缓存过期后核对真实安装目录，条件本身不变；超时直接失败。
    const status = await waitFor(async () => { const value = await call('/status', undefined, {}); return reviewed(value) ? value : false; }, 'Health 审视到已安装目标', 30_000);
    key = status.key;
    record('Health 原包检查真实安装目录与当前运行实例', reviewed(status));
    const preview = await call('/repair', { dryRun: true });
    record('修复预览返回真实计划和一次性确认身份', preview.dryRun && typeof preview.planToken === 'string' && Array.isArray(preview.planned));
    const invalid = await api(`/api/plugins/${health.manifest.id}/routes`, { method: 'POST', path: '/plugins/dsh-plugin-health/repair', headers: { 'x-plugin-health-key': key }, body: { dryRun: false, planToken: 'invalid' } });
    record('未经预览的修复确认被拒绝，目标仍运行', invalid.status === 409 && (await current(target.manifest.id)).runtime.state === 'running');
    const repaired = await call('/repair', { dryRun: false, planToken: preview.planToken });
    record('健康目录的修复确认如实返回空操作，不改动安装', !repaired.dryRun && repaired.actions.length === 0 && (await current(target.manifest.id)).digest === target.digest);
    const disabled = await call('/rescue/disable', { plugin: 'dsh-plugin-release-probe' });
    record('Health 原包停用提案提交到 daemon 并停止目标', disabled.disabled === true && !(await current(target.manifest.id)).enabled && !(await current(target.manifest.id)).active);
    const originInfo = (await current(target.manifest.id)).disabledBy;
    record('停用来源与原因由 daemon 记录，未改变目标包和授权', originInfo?.pluginId === health.manifest.id && originInfo.reason.includes('dsh-plugin-health-disabled') && (await current(target.manifest.id)).digest === target.digest);
    await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '设置入口'); await ui.click('[data-testid=sidebar-settings]');
    await waitFor(() => ui.exists('[data-testid=settings-nav-plugins]'), '插件分类'); await ui.click('[data-testid=settings-nav-plugins]');
    const row = `[data-testid=plugin-row][data-plugin-id="${target.manifest.id}"]`;
    await waitFor(() => ui.exists(row), '目标插件'); await ui.click(row);
    await waitFor(async () => (await ui.text('[data-testid=plugin-disabled-by]'))?.includes(originInfo.reason), '原生详情显示停用来源');
    record('原生插件详情就地显示真实停用者和原因', (await ui.text('[data-testid=plugin-disabled-by]')).includes(originInfo.name));
    await audit('disabled-default', page, { viewport: true, root: '.store-detail' });
    await writeFile(join(output, 'disabled-source.png'), Buffer.from(await ui.screenshot(), 'base64'));
    const hover = await page.evaluate(`(()=>{const r=document.querySelector('[data-action=activate]').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`);
    await page.call('Input.dispatchMouseEvent', { type: 'mouseMoved', ...hover });
    await audit('disabled-hover', page, { viewport: true, root: '.store-detail' });
    await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
    const narrow = await page.evaluate(`(()=>{const notice=document.querySelector('[data-testid=plugin-disabled-by]');notice.scrollIntoView({block:'center'});const r=notice.getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom,width:innerWidth,height:innerHeight,scrollWidth:notice.scrollWidth,clientWidth:notice.clientWidth};})()`);
    record('390px 视口的停用原因完整换行且可见，无横向溢出', narrow.left >= 0 && narrow.right <= narrow.width && narrow.top >= 0 && narrow.bottom <= narrow.height && narrow.scrollWidth <= narrow.clientWidth, narrow);
    await writeFile(join(output, 'disabled-source-390.png'), Buffer.from(await ui.screenshot(), 'base64'));
    await page.call('Emulation.clearDeviceMetricsOverride');
    await restartApp('before-restart.log');
    await waitFor(async () => (await current(health.manifest.id)).runtime?.state === 'running', 'Health 重启运行', 90_000);
    const restartedStatus = await call('/status', undefined, {}); key = restartedStatus.key;
    record('应用重启保留停用来源与意愿，目标没有被自动启动', !(await current(target.manifest.id)).enabled && JSON.stringify((await current(target.manifest.id)).disabledBy) === JSON.stringify(originInfo));
    const enabled = await call('/rescue/enable', { plugin: 'dsh-plugin-release-probe' });
    record('Health 原包恢复提案使真实目标重新运行', enabled.enabled === true && (await current(target.manifest.id)).runtime.state === 'running' && !(await current(target.manifest.id)).disabledBy);
    const handoff = await call('/rescue/handoff', { plugin: 'dsh-plugin-release-probe', version: '0.0.8' });
    record('诊断交接生成原包报告文件与可用提示词', handoff.files.length > 0 && handoff.prompt.includes('dsh-plugin-release-probe') && (await readFile(join(handoff.directory, 'prompt.md'), 'utf8')) === handoff.prompt);
    const privateProfile = join(stateRoot, 'extensions/data', health.manifest.id, 'profiles/magi');
    const residue = join(privateProfile, 'verification-residue.lock');
    await writeFile(residue, 'RECOVERABLE_PRIVATE_DATA');
    const stalePreview = await call('/repair', { dryRun: true });
    await writeFile(residue, 'CHANGED_PRIVATE_DATA');
    const staleRepair = await api(`/api/plugins/${health.manifest.id}/routes`, { method: 'POST', path: '/plugins/dsh-plugin-health/repair',
      headers: { 'x-plugin-health-key': key }, body: { dryRun: false, planToken: stalePreview.planToken } });
    record('修复预览后的文件变化使旧计划失效，失败不移动或覆盖新字节', staleRepair.status === 409
      && staleRepair.body.code === 'repair-plan-changed' && await readFile(residue, 'utf8') === 'CHANGED_PRIVATE_DATA', staleRepair);
    await writeFile(residue, 'RECOVERABLE_PRIVATE_DATA');
    await openPage();
    await waitFor(() => frame.evaluate('/清理\\s*1|Clean up\\s*1/i.test(document.body.innerText)'), '原包显示真实修复发现');
    observations.page = await frame.evaluate('({text:document.body.innerText,buttons:[...document.querySelectorAll("button")].map(b=>({text:b.innerText,disabled:b.disabled}))})');
    await writeFile(join(output, 'health.png'), Buffer.from(await ui.screenshot(), 'base64'));
    record('原生 Health 页面展示实际诊断与可执行的修复入口', true);
    const clickText = async expression => frame.evaluate(`(()=>{const button=[...document.querySelectorAll('button')].find(b=>${expression});if(!button)return false;button.click();return true;})()`);
    const selectMode = async mode => {
      await frame.evaluate(`(()=>{const select=document.querySelector('.dph-mode select');if(!select)throw new Error('原包显示方式入口缺失');select.value=${JSON.stringify(mode)};select.dispatchEvent(new Event('change',{bubbles:true}));})()`);
      await waitFor(() => frame.evaluate(`document.querySelector('.dph-mode select').value===${JSON.stringify(mode)}`), '原包显示模式切换完成');
      await waitFor(() => page.evaluate(`JSON.parse(localStorage.getItem(${JSON.stringify(`magi:plugin-ui:${health.manifest.id}`)})??'{}')['dsh-plugin-health:view']===${JSON.stringify(mode)}`), '原包偏好经隔离桥实际持久化');
    };
    await selectMode('advanced');
    await openPage();
    record('关闭并重开原页面保留实际显示偏好和同一待修复文件', await frame.evaluate('document.querySelector(".dph-mode select").value==="advanced"')
      && await readFile(residue, 'utf8') === 'RECOVERABLE_PRIVATE_DATA');
    const snapshot = await capturePluginDownload(inspector(), join(output, 'exports'), async () => {
      await waitFor(() => clickText(`b.getClientRects().length > 0 && /下载当前检查记录|下载当前配置快照|Download current check record|Download current snapshot/.test(b.innerText)`), '原包下载检查记录入口');
    });
    const snapshotData = JSON.parse(snapshot.bytes.toString());
    record('Health 原页面导出真实检查 JSON 文件，包含当前原包身份', snapshot.state === 'completed'
      && snapshot.suggestedFilename === 'dsh-profile-snapshot.json' && snapshotData.schemaVersion === 1
      && snapshotData.packages.some(p => p.name === 'dsh-plugin-health' && p.version === health.manifest.version));
    await selectMode('daily');
    await waitFor(() => clickText(`/清理\\s*1|Clean up\\s*1/i.test(b.innerText)`), '原包非空修复计划入口');
    await waitFor(() => frame.evaluate(`document.body.innerText.includes('确认修复') || document.body.innerText.includes('Confirm repair')`), '原包修复确认');
    record('原页面先显示非空修复预览，确认前文件仍存在', await access(residue).then(() => true, () => false));
    await audit('repair-preview');
    await writeFile(join(output, 'repair-preview.png'), Buffer.from(await ui.screenshot(), 'base64'));
    assert.ok(await clickText(`['取消','Cancel'].includes(b.innerText.trim())`));
    await waitFor(() => frame.evaluate(`![...document.querySelectorAll('button')].some(b=>['确认修复','Confirm repair'].includes(b.innerText.trim()))`), '原包取消修复');
    record('取消原包修复预览保留真实文件，可重新发起', await access(residue).then(() => true, () => false));
    await waitFor(() => clickText(`/清理\\s*1|Clean up\\s*1/i.test(b.innerText)`), '取消后重新预览修复');
    await waitFor(() => frame.evaluate(`[...document.querySelectorAll('button')].some(b=>['确认修复','Confirm repair'].includes(b.innerText.trim()))`), '重新确认修复');
    assert.ok(await clickText(`['确认修复','Confirm repair'].includes(b.innerText.trim())`));
    await waitFor(async () => !await access(residue).then(() => true, () => false), '真实文件修复隔离');
    const privateHome = dirname(dirname(privateProfile));
    const runs = await readdir(join(privateHome, '.plugin-health/quarantine'));
    const ledgers = await Promise.all(runs.map(run => readFile(join(privateHome, '.plugin-health/quarantine', run, 'repair-log.json'), 'utf8').then(JSON.parse)));
    const action = ledgers.flatMap(ledger => ledger.actions).find(item => item.from === residue);
    record('原页面确认后将真实文件隔离并保留恢复日志与原字节', action?.kind === 'quarantine' && await readFile(action.to, 'utf8') === 'RECOVERABLE_PRIVATE_DATA');
    record('修复业务文件不生成第二份安装元数据', !await access(join(privateProfile, 'package.json')).then(() => true, () => false) && !await access(join(privateProfile, 'node_modules')).then(() => true, () => false));
    await waitFor(() => frame.evaluate(`![...document.querySelectorAll('button')].some(b=>['确认修复','Confirm repair'].includes(b.innerText.trim()))`), '原包修复完成反馈');
    await audit('repair-complete');
    await writeFile(join(output, 'repair-complete.png'), Buffer.from(await ui.screenshot(), 'base64'));
    await selectMode('advanced');
    await waitFor(() => clickText(`!b.disabled && /^(检查更新|Check for updates|Check updates)$/.test(b.innerText.trim())`), 'Health 原页面主动在线检查更新');
    const update = await waitFor(async () => {
      const value = await call('/update'); return value.status === 'ok' && value.updateAvailable && value;
    }, 'Health 原包当前在线版本', 30_000);
    const beforeUpdate = await current(health.manifest.id);
    record('Health 在线自更新不依赖替其他插件授权的委托', !beforeUpdate.grants.some(permission => permission.kind === 'pluginManagement' && permission.targets.includes('authorize')));
    const beforeSettings = await api(`/api/plugins/${health.manifest.id}/settings`);
    const beforeDocument = await frame.evaluate('window.__healthSelfUpdate=crypto.randomUUID()');
    await waitFor(() => clickText(`!b.disabled && /^(立即更新|Update now)$/.test(b.innerText.trim())`), 'Health 原页面自更新入口');
    const journal = await waitFor(async () => {
      const state = JSON.parse(await readFile(join(stateRoot, 'plugins/installations.json'), 'utf8'));
      return Object.values(state.records).find(record => record.plugin === health.manifest.id && record.phase === 'finished'
        && record.candidate?.version === update.latestVersion && record.candidate.digest !== beforeUpdate.digest);
    }, 'daemon 自更新持久终态', 300_000);
    observations.selfUpdate = { before: beforeUpdate, after: await current(health.manifest.id), result: journal.result };
    assert.equal(journal.result.application, 'applied', JSON.stringify(journal.result));
    const afterUpdate = await waitFor(async () => {
      const plugin = await current(health.manifest.id);
      return plugin.manifest.version === update.latestVersion && plugin.digest !== beforeUpdate.digest && plugin.runtime?.state === 'running' && plugin;
    }, 'Health 原页面自更新后实际新版宿主运行', 300_000);
    frame.close();
    const updatedUrl = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src.includes(${JSON.stringify(afterUpdate.digest)})&&document.querySelector(${JSON.stringify(selector)}).src`), 'Health 自更新替换原文档');
    frame = await waitFor(() => connectTarget(cdpPort, t => ['iframe', 'page'].includes(t.type) && t.url === updatedUrl, 'Health 更新后原页面', errors), 'Health 新版页面');
    const newStatus = await call('/status', undefined, {}); key = newStatus.key;
    observations.selfUpdate = { before: beforeUpdate, after: afterUpdate, result: journal.result };
    await audit('self-update-complete');
    await writeFile(join(output, 'health-self-update.png'), Buffer.from(await ui.screenshot(), 'base64'));
    record('Health 原页面在线自更新由 daemon 完成替换与实际新版运行，旧调用结束不丢提交', journal.result.application === 'applied' && journal.candidate.digest === afterUpdate.digest
      && afterUpdate.active && newStatus.update.currentVersion === update.latestVersion, journal.result);
    record('自更新保留原设置和已批权限范围，新文档不复用旧页面状态', JSON.stringify(await api(`/api/plugins/${health.manifest.id}/settings`)) === JSON.stringify(beforeSettings)
      && afterUpdate.grants.every(permission => beforeUpdate.grants.some(grant => JSON.stringify(grant) === JSON.stringify(permission)))
      && typeof beforeDocument === 'string' && await frame.evaluate('window.__healthSelfUpdate===undefined'));
    record('原页面显示偏好跨在线更新保持，修复日志仍为原字节', await frame.evaluate('document.querySelector(".dph-mode select").value==="advanced"')
      && await readFile(action.to, 'utf8') === 'RECOVERABLE_PRIVATE_DATA');
    await restartApp('before-self-update-restart.log');
    await waitFor(async () => (await current(health.manifest.id)).runtime?.state === 'running', '自更新后应用重启恢复新版', 90_000);
    record('应用重启保留自更新最终摘要及配置，不重放旧任务', (await current(health.manifest.id)).digest === afterUpdate.digest
      && JSON.stringify(await api(`/api/plugins/${health.manifest.id}/settings`)) === JSON.stringify(beforeSettings));
    await openPage();
    record('应用重启后原页面偏好及修复隔离记录恢复', await frame.evaluate('document.querySelector(".dph-mode select").value==="advanced"')
      && await readFile(action.to, 'utf8') === 'RECOVERABLE_PRIVATE_DATA');
    closeFrame();
    const healthPath = `/api/plugins/${health.manifest.id}`;
    const beforeDisableKey = (await call('/status', undefined, {})).key;
    await api(healthPath + '/deactivate', {}); await api(healthPath + '/disable', {});
    await waitFor(() => page.evaluate(`!document.querySelector(${JSON.stringify(selector)})`), 'Health 停用释放原文档');
    const inactive = await current(health.manifest.id);
    record('停用 Health 释放页面与动态工具，保留已更新包和私有修复数据', !inactive.enabled && !inactive.active
      && inactive.manifest.contributions.tools.length === 0 && inactive.digest === afterUpdate.digest
      && await readFile(action.to, 'utf8') === 'RECOVERABLE_PRIVATE_DATA');
    await api(healthPath + '/enable', {}); await api(healthPath + '/activate', {});
    await waitFor(async () => (await current(health.manifest.id)).runtime?.state === 'running', 'Health 重新启用运行');
    await openPage();
    const restored = await call('/status', undefined, {}); key = restored.key;
    const oldControl = await api(healthPath + '/routes', { method: 'POST', path: '/plugins/dsh-plugin-health/repair',
      headers: { 'x-plugin-health-key': beforeDisableKey }, body: { dryRun: true } });
    observations.reactivation = { keyChanged: key !== beforeDisableKey, oldControlStatus: oldControl.status,
      current: await current(health.manifest.id), mode: await frame.evaluate('document.querySelector(".dph-mode select").value') };
    record('重新启用 Health 创建新实例并可继续诊断，保留页面偏好和原授权', restored.profile === 'magi'
      && key !== beforeDisableKey && oldControl.status === 401
      && await frame.evaluate('document.querySelector(".dph-mode select").value==="advanced"')
      && JSON.stringify((await current(health.manifest.id)).grants) === JSON.stringify(afterUpdate.grants));
    const finalJournal = JSON.parse(await readFile(join(stateRoot, 'plugins/installations.json'), 'utf8'));
    record('重启及重新启用没有重放自更新，原持久回执保持不变', JSON.stringify(Object.values(finalJournal.records)
      .find(record => record.plugin === health.manifest.id && record.candidate?.digest === afterUpdate.digest)) === JSON.stringify(journal));
    record('本轮没有页面运行异常', errors.length === 0, errors);
    evidence.completed = true;
    return { page, checks, observations, plugin: await current(health.manifest.id) };
  } catch (error) {
    evidence.error = String(error.stack || error);
    evidence.page = await frame?.evaluate('({text:document.body.innerText})').catch(() => null);
    await writeFile(join(output, 'health-failure.png'), Buffer.from(await ui.screenshot(), 'base64')).catch(() => {});
    throw error;
  } finally {
    closeFrame();
    await writeFile(join(output, 'health-business.json'), JSON.stringify(evidence, null, 2));
  }
}
