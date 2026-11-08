import assert from 'node:assert/strict';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { connectTarget, pageTools, waitFor } from './electron-harness.mjs';
import { createPluginThemeCases, activatePluginTheme, measurePluginContrast } from './plugin-theme-acceptance.mjs';

// 两个验收入口共享同一用户任务：原页面操作、后台生命周期、版本切换、
// 失败恢复及重启。传入的 request 只访问调用者本次独立 daemon。
export async function verifyDshUpgrader({ request, origin, plugin, page, cdpPort, errors, gate, output, stateRoot, restart, check }) {
  const name = 'dsh-plugin-release-probe', checks = {};
  assert.equal(plugin.manifest.version, '1.8.0', 'Upgrader 业务合同固定验收原包版本');
  const evidence = { package: 'dsh-plugin-upgrader@1.8.0', pluginId: plugin.manifest.id, digest: plugin.digest, completed: false, checks };
  const record = (key, label, ok, detail) => { check(label, ok, detail); checks[key] = true; };
  const plugins = async () => (await request('/api/plugins')).plugins;
  const current = async () => (await plugins()).find(p => p.source.location.startsWith(`dsh:${name}@`));
  const pluginPath = `/api/plugins/${plugin.manifest.id}`;
  const route = (action, body, query) => request(pluginPath + '/routes', {
    method: body === undefined ? 'GET' : 'POST', path: `/api/plugin-upgrader/${action}`,
    ...(body === undefined ? {} : { body }), ...(query ? { query } : {}),
  });
  let frame;
  const closeFrame = () => { frame?.close(); frame = undefined; };
  const openPage = async (navigate = true) => {
    closeFrame();
    if (navigate) {
      const session = (await request('/api/session/materialize', { scope: 'personal' })).sessionId;
      await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${session}` });
    }
    const ui = pageTools(page);
    await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), 'Upgrader 工作台');
    await ui.click('[data-testid=sidebar-settings]');
    const view = plugin.manifest.contributions.views.find(view => view.placements.includes('settings'));
    const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${plugin.manifest.id}:${view.id}`)}]`;
    await waitFor(() => ui.exists(nav), 'Upgrader 原包设置入口'); await ui.click(nav);
    const selector = `iframe[data-plugin-id=${JSON.stringify(plugin.manifest.id)}]`;
    const url = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`), 'Upgrader 原页面');
    frame = await waitFor(() => connectTarget(cdpPort, target => ['iframe', 'page'].includes(target.type) && target.url === url, 'Upgrader 原页面', errors), 'Upgrader 隔离文档');
    await waitFor(() => frame.evaluate(`Boolean(document.querySelector('.dsh-pu-row'))`), 'Upgrader 原包真实列表', 120_000);
  };
  const screenshot = async label => writeFile(join(output, `upgrader-${label}.png`), Buffer.from(await pageTools(page).screenshot(), 'base64'));
  const installTarget = async () => {
    const input = { ecosystem: 'dsh', package: name, version: '0.0.7' };
    const preview = await request('/api/plugins/upstream/preview', input);
    await request('/api/plugins/upstream/install', { ...input, expectedDigest: preview.candidate.digest });
    const target = await current(); assert.ok(target);
    const path = `/api/plugins/${target.manifest.id}`;
    await request(path + '/enable', {}); await request(path + '/authorize', { grants: target.manifest.permissions }); await request(path + '/activate', {});
    await waitFor(async () => (await current())?.runtime?.state === 'running', 'Upgrader 真实目标运行', 120_000);
    return current();
  };
  const upgradeButton = `(() => [...document.querySelectorAll('.dsh-pu-row')].find(row=>row.innerText.includes(${JSON.stringify(name)}))?.querySelector('.dsh-pu-row-actions button.is-primary'))()`;
  const beginFromPage = async () => {
    await waitFor(() => frame.evaluate(`Boolean(${upgradeButton}&&!${upgradeButton}.disabled)`), 'Upgrader 原包升级按钮', 120_000);
    await frame.evaluate(`${upgradeButton}.click()`);
    await waitFor(() => gate.held.size > 0, 'Upgrader 当前安装进入真实解析等待', 30_000);
    const overview = await route('overview'); assert.equal(overview.status, 200);
    assert.ok(overview.body.active?.id); return overview.body.active.id;
  };
  const waitJob = id => waitFor(async () => {
    const result = await route('job', undefined, `id=${id}`);
    assert.equal(result.status, 200, JSON.stringify(result));
    return ['finished', 'cancelled'].includes(result.body.status) && result.body;
  }, 'Upgrader 原包任务结算', 240_000);
  const runVersion = async (version, enabled) => {
    const submitted = await route('job', { steps: [{ name, version, action: 'switch', enabled }] });
    assert.equal(submitted.status, 202, JSON.stringify(submitted)); return waitJob(submitted.body.id);
  };
  try {
    assert.equal(await current(), undefined, '每次 Upgrader 验收从独立的未安装目标开始');
    const original = await installTarget();
    await openPage();
    await waitFor(() => frame.evaluate(`Boolean(${upgradeButton}&&!${upgradeButton}.disabled)`), 'Upgrader 原包可用升级操作');
    const { cases: themes, original: originalTheme } = await createPluginThemeCases(request);
    const appearance = [];
    try {
      for (const theme of themes) {
        await activatePluginTheme(request, page, frame, theme.id);
        const selector = `iframe[data-plugin-id=${JSON.stringify(plugin.manifest.id)}]`;
        const outer = await page.evaluate(`(() => {const r=document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();return {x:r.x,y:r.y};})()`);
        await page.call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: outer.x + 4, y: outer.y + 4 });
        await waitFor(() => frame.evaluate(`!${upgradeButton}.matches(':hover')`), 'Upgrader 原包升级按钮真实默认状态');
        await new Promise(resolve => setTimeout(resolve, 250));
        const normal = await measurePluginContrast(frame, '.dsh-pu-btn.is-primary:not(:disabled)');
        await frame.evaluate(`${upgradeButton}.scrollIntoView({block:'center'})`);
        const inner = await frame.evaluate(`(() => {const r=${upgradeButton}.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2};})()`);
        const position = await page.evaluate(`(() => {const r=document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();return {x:r.x,y:r.y};})()`);
        await page.call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: position.x + inner.x, y: position.y + inner.y });
        await waitFor(() => frame.evaluate(`${upgradeButton}.matches(':hover')`), 'Upgrader 原包升级按钮真实悬停');
        await new Promise(resolve => setTimeout(resolve, 250));
        const hover = await measurePluginContrast(frame, '.dsh-pu-btn.is-primary:not(:disabled)');
        appearance.push({ id: theme.id, normal, hover });
        if (theme.material === 'immersive') await screenshot(`buttons-${theme.mode}-${theme.material}`);
        assert.ok(normal.length > 0 && hover.length === normal.length
          && [...normal, ...hover].every(item => item.ratio >= 4.5), `${theme.id} 原包可用主按钮默认与悬停文字必须可读：${JSON.stringify({ normal, hover })}`);
      }
      record('appearance', 'Upgrader 原页面可用主按钮在八种外观的默认与真实悬停状态均可读', true);
    } finally {
      await writeFile(join(output, 'upgrader-button-contrast.json'), JSON.stringify(appearance, null, 2));
      await activatePluginTheme(request, page, frame, originalTheme);
      await page.call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 0, y: 0 });
    }
    // 来源列表是原包配置，不切换用户选择的 registry；安装仍经本次 gate。
    const customSource = gate.registry;
    await waitFor(() => frame.evaluate(`Boolean(document.querySelector('.dsh-pu-reg-btn:not(:disabled)'))`), '原包配置就绪');
    await frame.evaluate(`document.querySelector('.dsh-pu-reg-btn').click()`);
    await waitFor(() => frame.evaluate(`Boolean(document.querySelector('.dsh-pu-src-add input'))`), '原包来源编辑');
    await frame.evaluate(`(() => {const input=document.querySelector('.dsh-pu-src-add input');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,${JSON.stringify(customSource)});input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
    await waitFor(() => frame.evaluate(`(() => {const button=document.querySelector('.dsh-pu-src-add button');if(button.disabled)return false;button.click();return true;})()`), '原包添加来源');
    const savedSettings = await waitFor(async () => {
      const snapshot = await request(pluginPath + '/settings');
      return JSON.stringify(snapshot.value).includes(customSource) && snapshot;
    }, '原页面配置由 daemon 持久保存');
    record('configuration', 'Upgrader 原页面配置通过共享设置保存，未改写安装来源', true);
    await frame.evaluate(`document.querySelector('.dsh-pu-modal-head button').click()`);
    const identity = await frame.evaluate('window.__upgraderAcceptance=crypto.randomUUID()');
    gate.pause(name);
    const cancelledId = await beginFromPage();
    const busy = await route('job', { steps: [{ name, version: '0.0.8', action: 'upgrade', enabled: true }] });
    record('concurrentRejection', 'Upgrader 原包拒绝并发批次且目标旧版本继续运行', busy.status === 409 && busy.body.id === cancelledId
      && (await current()).digest === original.digest && (await current()).runtime.generation === original.runtime.generation);
    await pageTools(page).click('[data-testid=settings-back]');
    record('background', '关闭 Upgrader 设置页保留同一后台文档及实际在途任务',
      await frame.evaluate(`window.__upgraderAcceptance===${JSON.stringify(identity)}`) && (await route('overview')).body.active.id === cancelledId && gate.held.size > 0);
    await openPage(false);
    record('reopen', '重开 Upgrader 原页面沿真实任务恢复运行反馈',
      await frame.evaluate(`window.__upgraderAcceptance===${JSON.stringify(identity)}`) && (await route('overview')).body.active.id === cancelledId);
    await waitFor(() => frame.evaluate(`(() => {const button=[...document.querySelectorAll('.dsh-pu-job button')].find(button=>/^(取消|Cancel)$/.test(button.innerText.trim())&&!button.disabled);if(!button)return false;button.click();return true;})()`), '原包取消安装');
    await waitFor(() => gate.held.size === 0, '取消回收真实解析连接');
    const cancelled = await waitJob(cancelledId), afterCancel = await current();
    record('cancellation', 'Upgrader 原包取消真正停止安装并保留旧摘要、授权与运行代次', cancelled.status === 'cancelled' && cancelled.results[0]?.application === 'cancelled'
      && afterCancel.digest === original.digest && afterCancel.runtime.generation === original.runtime.generation && JSON.stringify(afterCancel.grants) === JSON.stringify(original.grants));
    await screenshot('cancelled');
    // 已结束的请求租约不能拥有后台任务；停用则必须结束实例所有任务。
    const stoppedId = await beginFromPage();
    await request(pluginPath + '/deactivate', {});
    await waitFor(() => gate.held.size === 0, '停用升级器回收真实准备进程');
    const stopped = await current();
    record('deactivation', '停用 Upgrader 取消准备中的安装，目标摘要、授权与运行意愿保持', stopped.digest === original.digest
      && stopped.active && stopped.runtime.generation === original.runtime.generation && JSON.stringify(stopped.grants) === JSON.stringify(original.grants));
    closeFrame(); gate.release();
    await request(pluginPath + '/activate', {});
    await waitFor(async () => (await plugins()).find(p => p.manifest.id === plugin.manifest.id)?.runtime?.state === 'running', 'Upgrader 重新激活', 120_000);
    await openPage();
    const oldJob = await route('job', undefined, `id=${stoppedId}`);
    record('reactivation', 'Upgrader 新实例不伪造旧批次运行，重新读取 daemon 当前版本', oldJob.status === 404 && (await route('overview')).body.active === null
      && await frame.evaluate(`window.__upgraderAcceptance===undefined`));
    const retryIdentity = await frame.evaluate('window.__upgraderAcceptance=crypto.randomUUID()');
    await waitFor(() => frame.evaluate(`Boolean(${upgradeButton}&&!${upgradeButton}.disabled)`), '停用恢复后原包显式重试');
    await frame.evaluate(`${upgradeButton}.click()`);
    const upgraded = await waitFor(async () => { const value = await current(); return value?.manifest.version === '0.0.8' && value.runtime?.state === 'running' && value; }, 'Upgrader 原页面在线更新完成', 240_000);
    await waitFor(() => frame.evaluate(`!${upgradeButton} && ![...document.querySelectorAll('.dsh-pu-job button')].some(button=>/^(取消|Cancel|正在取消…|Cancelling…)$/.test(button.innerText.trim()))`), '原包页面更新完成回执');
    record('onlineUpgrade', '重新启用后原包显式重试完成在线更新并展示实际新版', upgraded.active && upgraded.grants.length > 0);
    record('documentIdentity', '目标版本更新保留 Upgrader 页面身份与实际完成回执', await frame.evaluate(`window.__upgraderAcceptance===${JSON.stringify(retryIdentity)}`));
    await screenshot('updated');
    const switched = await runVersion('0.0.7', false), disabled = await current();
    record('versionSwitch', 'Upgrader 真实版本切换保持目标停用意愿', switched.failed === 0 && disabled.manifest.version === '0.0.7' && !disabled.enabled && !disabled.active);
    const missing = await runVersion('9999.0.0', false);
    record('failedDisabled', '不存在版本的更新失败保留旧摘要与停用意愿', missing.failed === 1 && (await current()).digest === disabled.digest && !(await current()).enabled);
    const restored = await runVersion('0.0.8', true), active = await current();
    assert.equal(restored.failed, 0); assert.equal(active.runtime.state, 'running');
    const missingActive = await runVersion('9999.0.0', true), afterMissing = await current();
    record('failedActive', '不存在版本的更新失败保留旧授权及同一运行实例', missingActive.failed === 1
      && afterMissing.digest === active.digest && afterMissing.enabled && afterMissing.active
      && afterMissing.runtime.state === 'running' && afterMissing.runtime.generation === active.runtime.generation
      && JSON.stringify(afterMissing.grants) === JSON.stringify(active.grants));
    const preview = await request('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: name, version: '0.0.7' });
    const blocked = join(stateRoot, 'plugins/versions', active.manifest.id, `${preview.candidate.digest}.zip`);
    await mkdir(blocked);
    try {
      const failed = await runVersion('0.0.7', true);
      const recovered = await waitFor(async () => { const value = await current(); return value?.digest === active.digest && value.runtime?.state === 'running' && value; }, '真实落盘失败恢复原版本', 30_000);
      record('failedReplacement', 'Upgrader 归档写入失败恢复旧版本、授权及实际运行', failed.failed === 1 && recovered.active && recovered.enabled
        && JSON.stringify(recovered.grants) === JSON.stringify(active.grants));
    } finally { await rm(blocked, { recursive: true, force: true }); }
    const retried = await runVersion('0.0.7', true);
    record('explicitRetry', '清除落盘故障后显式重试成功，不重放已完成的安装', retried.failed === 0 && (await current()).manifest.version === '0.0.7' && (await current()).runtime.state === 'running');
    const beforeRestart = await current(); closeFrame(); page = await restart();
    await waitFor(async () => (await plugins()).find(p => p.manifest.id === plugin.manifest.id)?.runtime?.state === 'running', '应用重启恢复 Upgrader', 120_000);
    await openPage();
    const afterRestart = await current(), settings = await request(pluginPath + '/settings');
    record('restart', '应用重启保留 Upgrader 配置与目标版本，不重放原包升级批次', settings.version === savedSettings.version && JSON.stringify(settings.value) === JSON.stringify(savedSettings.value)
      && afterRestart.digest === beforeRestart.digest && afterRestart.runtime.state === 'running' && (await route('overview')).body.active === null);
    await screenshot('restarted');
    Object.assign(evidence, { completed: true, cancelled, stoppedId, settings, beforeRestart, afterRestart });
    return { checks, page };
  } finally {
    await writeFile(join(output, 'upgrader-business.json'), JSON.stringify(evidence, null, 2));
    gate.release(); closeFrame();
    const target = await current();
    if (target) {
      const path = `/api/plugins/${target.manifest.id}`;
      if (target.active) await request(path + '/deactivate', {});
      if (target.enabled) await request(path + '/disable', {});
      await request(path + '/uninstall', {});
    }
  }
}
