import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import { connectTarget, pageTools, waitFor } from './electron-harness.mjs';
import { capturePluginDownload } from './plugin-download-acceptance.mjs';
import { waitForInstalledPlugin } from './plugin-native-acceptance.mjs';

// 专项和社区共用完整 Share 原包流程；页面、二维码和原始回执不改写。
export async function verifyDshShare({ request, plugin, page, cdpPort, inspector, errors, gate, output, stateRoot, restart, check, clipAudit }) {
  assert.equal(plugin.manifest.version, '0.2.5');
  const id = plugin.manifest.id, name = 'dsh-plugin-release-probe', githubSpec = 'github:easonmws/dsh-plugin-health#v3.0.5';
  const evidence = { package: 'dsh-plugin-share@0.2.5', pluginId: id, digest: plugin.digest, completed: false, checks: {} };
  const record = (key, label, ok, detail) => { check(label, ok, detail); evidence.checks[key] = true; };
  const plugins = async () => (await request('/api/plugins')).plugins;
  const current = async packageName => (await plugins()).find(value => value.source.location.startsWith(`dsh:${packageName}@`));
  const route = (action, body) => request(`/api/plugins/${id}/routes`, { method: body === undefined ? 'GET' : 'POST', path: `/api/plugin-share/${action}`, ...(body === undefined ? {} : { body }) });
  const call = async (action, body) => { const result = await route(action, body); assert.equal(result.status, 200, JSON.stringify(result)); assert.equal(result.body.ok, true, JSON.stringify(result)); return result.body; };
  const journal = () => readFile(join(stateRoot, 'plugins/installations.json'), 'utf8');
  let ui = pageTools(page), frame;
  const closeFrame = () => { frame?.close(); frame = undefined; };
  const screenshot = label => ui.screenshot().then(bytes => writeFile(join(output, `share-${label}.png`), Buffer.from(bytes, 'base64')));
  const openPage = async () => {
    closeFrame();
    if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) {
      await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), 'Share 工作台初始化');
      await ui.click('[data-testid=sidebar-settings]');
    }
    const view = plugin.manifest.contributions.views.find(value => value.placements.includes('settings'));
    const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${id}:${view.id}`)}]`;
    await waitFor(() => ui.exists(nav), 'Share 原包设置入口'); await ui.click(nav);
    const selector = `iframe[data-plugin-id=${JSON.stringify(id)}]`;
    const url = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`), 'Share 原包文档');
    frame = await waitFor(() => connectTarget(cdpPort, target => ['iframe', 'page'].includes(target.type) && target.url === url, 'Share 原页面', errors), 'Share 隔离文档');
    await waitFor(() => frame.evaluate('Boolean(document.querySelector("textarea.dps-code"))'), 'Share 原包页面就绪');
  };
  const clickText = text => frame.evaluate(`(() => {const button=[...document.querySelectorAll('button')].find(button=>button.innerText.trim()===${JSON.stringify(text)}&&!button.disabled);if(!button)return false;button.click();return true;})()`);
  const parse = async code => {
    await frame.evaluate(`(() => {const input=document.querySelector('textarea.dps-code');Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(input,${JSON.stringify(code)});input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
    await waitFor(() => clickText('解析组合码'), 'Share 原包解析组合码');
    await waitFor(() => frame.evaluate('[...document.querySelectorAll("button")].some(button=>button.innerText.trim()==="全部可处理项加入队列"&&!button.disabled)'), 'Share 原包解析结果');
  };
  const history = () => frame.evaluate(`JSON.parse(localStorage.getItem('dsh-plugin-share:results:v1')??'null')`);
  const authorize = async target => {
    const before = await journal();
    const active = await waitForInstalledPlugin({ request, page, plugin: target });
    assert.equal(await journal(), before, '原生授权不能重放原包安装任务');
    await openPage(); return active;
  };
  const remove = async packageName => {
    const target = await current(packageName); if (!target) return;
    const path = `/api/plugins/${target.manifest.id}`;
    if (target.enabled) await request(path + '/disable', {});
    if (target.enabled) await request(path + '/disable', {});
    await request(path + '/uninstall', {});
  };
  try {
    assert.equal(await current(name), undefined); assert.equal(await current('dsh-plugin-health'), undefined);
    record('noDelegation', 'Share 原包未申请代授权能力', !plugin.manifest.permissions.some(value => value.kind === 'pluginManagement' && value.targets.includes('authorize')));
    const exported = await call('export', {});
    const parsed = await call('parse', { code: exported.code });
    record('export', '原包导出和解析包含 daemon 的真实包及版本', exported.entries.some(value => value.name === 'dsh-plugin-share' && value.version === '0.2.5') && parsed.preview.length > 0);
    await openPage();
    const code = (await call('export', { entries: [{ kind: 'npm', name, version: '0.0.7', enabled: true }] })).code;
    await parse(code); gate.pause(name);
    await waitFor(() => clickText('全部可处理项加入队列'), 'Share 原包入队');
    await waitFor(() => gate.held.size > 0, 'Share 真实在线准备');
    const identity = await frame.evaluate('window.__shareAcceptance=crypto.randomUUID()');
    await ui.click('[data-testid=settings-back]'); await openPage();
    record('background', '关闭并重开设置保留 Share 原队列文档及在途安装', await frame.evaluate(`window.__shareAcceptance===${JSON.stringify(identity)}`) && gate.held.size > 0);
    await waitFor(() => clickText('取消当前安装'), 'Share 原页面取消安装');
    await waitFor(() => gate.held.size === 0, 'Share 取消释放真实连接');
    await waitFor(() => frame.evaluate('document.body.innerText.includes("队列已暂停")'), 'Share 原包取消回执');
    record('cancellation', '原页面取消准备保留未安装事实并记录真实取消', !await current(name) && (await history()).items.some(value => value.outcome === 'cancelled'));
    gate.release();
    await waitFor(() => clickText('重试当前项'), 'Share 原包取消后显式重试');
    const pending = await waitFor(async () => { const value = await current(name); return value?.manifest.version === '0.0.7' && value; }, 'Share 在线提交 npm 原包', 240_000);
    const active = await authorize(pending);
    record('installEnabled', 'Share 原包在线导入后直接启用并运行同一摘要', pending.enabled && active.digest === pending.digest);
    for (const enabled of [false, true]) {
      const toggle = (await call('export', { entries: [{ kind: 'npm', name, version: '0.0.7', enabled }] })).code;
      await call('import', { code: toggle, confirm: true, requestId: `share-toggle-${enabled}` });
      const target = await current(name); assert.equal(target.enabled, enabled);
      if (enabled) assert.equal(target.runtime.state, 'running');
    }
    record('lifecycle', 'Share 原包导入启停设置实际停止及恢复宿主', true);
    const newer = (await call('export', { entries: [{ kind: 'npm', name, version: '0.0.8', enabled: true }] })).code;
    const refused = await route('import', { code: newer, confirm: true, requestId: 'share-manual-update' });
    record('manualUpdate', '保留原包要求手动更新的回执和当前真实版本', refused.status === 409 && refused.body.error === 'manual-update-required'
      && (await current(name)).digest === active.digest && (await current(name)).runtime.state === 'running');
    // 原页面保留授权失败后暂停的队列；原生授权不应替原包清空它。
    // 沿原页面显式结束当前项，才能开始解析下一份组合码。
    const beforeSkip = await journal();
    await waitFor(() => clickText('跳过并继续'), 'Share 原页面结束已恢复的暂停项');
    await waitFor(() => frame.evaluate('[...document.querySelectorAll("button")].some(button=>button.innerText.trim()==="解析组合码"&&!button.disabled)'), 'Share 原队列恢复可解析');
    record('queueContinuation', '用户结束原暂停项后可解析新组合码，不重放已提交安装或清除失败记录', await journal() === beforeSkip
      && (await current(name)).digest === active.digest && JSON.stringify(await history()) === JSON.stringify(failedHistory));
    const gitCode = (await call('export', { entries: [{ kind: 'github', spec: githubSpec, version: '3.0.5', enabled: false }] })).code;
    await parse(gitCode);
    await waitFor(() => clickText('展示二维码'), 'Share 原包二维码入口');
    await waitFor(() => frame.evaluate('[...document.querySelectorAll("button")].some(button=>button.innerText.trim()==="保存 SVG")'), 'Share 二维码生成');
    const directory = join(output, 'exports');
    const svg = await capturePluginDownload(inspector(), directory, () => clickText('保存 SVG'));
    const cancelled = await capturePluginDownload(inspector(), directory, () => clickText('保存 PNG'), { cancel: true });
    const png = await capturePluginDownload(inspector(), directory, () => clickText('保存 PNG'));
    const sharp = createRequire(new URL('../compat/package.json', import.meta.url))('sharp');
    const image = await sharp(png.bytes).metadata();
    const decoded = await sharp(png.bytes).removeAlpha().greyscale().raw().toBuffer({ resolveWithObject: true });
    const scanned = await call('scan', { width: decoded.info.width, height: decoded.info.height, gray: decoded.data.toString('base64') });
    record('downloads', '原页面保存 SVG、取消 PNG 后重试，实际位图由原包识别回同一 GitHub 组合码', svg.state === 'completed' && svg.suggestedFilename === 'dsh-plugin-share-code.svg'
      && svg.bytes.toString().includes('<svg') && svg.bytes.toString().includes('<path') && cancelled.state === 'cancelled' && png.state === 'completed'
      && png.suggestedFilename === 'dsh-plugin-share-code.png' && image.format === 'png' && image.width > 100 && image.width === image.height && scanned.code === gitCode);
    await screenshot('qr-exports');
    await waitFor(() => clickText('全部可处理项加入队列'), 'Share 原页面 GitHub 安装');
    const gitInstalled = await waitFor(async () => {
      const target = await current('dsh-plugin-health');
      return target?.manifest.version === '3.0.5' && target;
    }, 'Share 提交真实 GitHub 原包', 300_000);
    const completedGit = await waitFor(async () => (await history()).items.find(value => value.spec === githubSpec && value.outcome === 'success'), 'Share GitHub 安装成功回执', 60_000);
    await waitFor(() => page.evaluate(`(() => {const values=JSON.parse(localStorage.getItem(${JSON.stringify(`magi:plugin-ui:${id}`)})??'{}');return JSON.parse(values['dsh-plugin-share:results:v1']??'null')?.items.some(value=>value.id===${JSON.stringify(completedGit.id)}&&value.outcome==='success');})()`), 'Share GitHub 成功结果实际持久化');
    record('githubImport', 'Share 从 GitHub 安装固定提交并启用', gitInstalled.enabled
      && gitInstalled.source.github?.commit === '8e1d4366fb71012b82a7c2bc476398a764eab76c');
    const collected = await call('state');
    record('githubSource', '原包导出目录保留 GitHub 来源及真实版本', collected.bundles.some(value => value.name === 'dsh-plugin-health' && value.source === githubSpec && value.version === '3.0.5')
      && collected.entries.some(value => value.kind === 'github' && value.value === githubSpec));
    const gitActive = await authorize(gitInstalled);
    const wrongSource = await ui.post('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: 'dsh-plugin-health', version: '3.0.5' });
    record('sourceProtection', '同名 npm 包不能冒用 GitHub 更新来源，旧版继续运行', wrongSource.status !== 200 && (await current('dsh-plugin-health')).digest === gitActive.digest
      && (await current('dsh-plugin-health')).runtime.generation === gitActive.runtime.generation);
    closeFrame(); await ui.click('[data-testid=settings-nav-plugins]');
    await waitFor(() => ui.exists('[data-testid=plugin-sources-open]'), '原生来源入口'); await ui.click('[data-testid=plugin-sources-open]');
    const previewUpdate = async () => {
      if (!await ui.exists('[data-testid=plugin-address-input]')) await ui.click('[data-testid=plugin-sources-open]');
      await page.evaluate(`(() => {const input=document.querySelector('[data-testid=plugin-address-input]');input.value='github:easonmws/dsh-plugin-health#v3.0.6';input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
      await ui.click('[data-testid=plugin-address-install]'); await waitFor(() => ui.exists('[data-testid=plugin-candidate-github]'), 'GitHub 在线更新预览', 300_000);
    };
    await previewUpdate();
    record('onlinePreview', '原生在线更新明确展示固定 GitHub 提交', await page.evaluate('document.querySelector("[data-testid=plugin-candidate]").dataset.operation==="upgrade"&&document.querySelector("[data-testid=plugin-candidate-github]").innerText.includes("f2b7388a131916dc2c5427652c8ab450a24838ce")'));
    await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
    const layout = await page.evaluate(`(() => {const button=document.querySelector('[data-action=confirm-candidate]'), candidate=document.querySelector('[data-testid=plugin-candidate]'), r=button.getBoundingClientRect();return {left:r.left,right:r.right,top:r.top,bottom:r.bottom,width:innerWidth,height:innerHeight,scroll:candidate.scrollWidth,client:candidate.clientWidth};})()`);
    record('narrowPreview', '390px GitHub 确认保持固定可达且无横向溢出', layout.left >= 0 && layout.right <= layout.width && layout.top >= 0 && layout.bottom <= layout.height && layout.scroll <= layout.client, layout);
    await screenshot('github-preview-390');
    if (clipAudit) { const audit = await page.evaluate(`(${clipAudit})({viewport:true,root:'.store-detail'})`); await writeFile(join(output, 'share-preview-clip.json'), JSON.stringify(audit, null, 2)); record('clip', 'GitHub 预览阴影无部分裁切', audit.cut === 0, audit); }
    await page.call('Emulation.clearDeviceMetricsOverride');
    await ui.click('[data-action=cancel-candidate]');
    record('updateCancellation', '取消在线更新保留原提交、授权与运行实例', (await current('dsh-plugin-health')).digest === gitActive.digest && (await current('dsh-plugin-health')).runtime.generation === gitActive.runtime.generation);
    await previewUpdate(); await ui.click('[data-action=confirm-candidate]');
    const gitUpdated = await waitFor(async () => { const target = await current('dsh-plugin-health'); return target?.manifest.version === '3.0.6' && target; }, '原生提交 GitHub 新版本', 300_000);
    record('onlineUpdate', 'GitHub 在线更新提交新版本并保持启用', gitUpdated.source.github?.reference === 'v3.0.6' && gitUpdated.enabled);
    await authorize(gitUpdated);
    await ui.click('[data-testid=settings-nav-plugins]');
    const gitRow = `[data-testid=plugin-row][data-plugin-id=${JSON.stringify(gitUpdated.manifest.id)}]`;
    await waitFor(() => ui.exists(gitRow), '已安装 GitHub 条目'); await ui.click(gitRow);
    await waitFor(() => ui.exists('[data-testid=plugin-detail-update]'), '固定来源在线检查'); await ui.click('[data-testid=plugin-detail-update]');
    await waitFor(() => page.evaluate('/无需安装|nothing to install/.test(document.querySelector("[data-testid=plugin-message]")?.innerText??"")'), '固定标签无更新回执', 300_000);
    record('fixedSource', '固定 GitHub 标签再次在线检查保持相同摘要及授权运行', (await current('dsh-plugin-health')).digest === gitUpdated.digest && (await current('dsh-plugin-health')).runtime.state === 'running');
    const invalid = await ui.post('/api/plugins/upstream/preview', { ecosystem: 'dsh', github: { repository: 'easonmws/dsh-plugin-health', reference: 'magi-missing-revision-acceptance' } });
    record('failedUpdate', '不存在的 GitHub 引用保留当前授权及实际运行', invalid.status !== 200 && (await current('dsh-plugin-health')).digest === gitUpdated.digest && (await current('dsh-plugin-health')).runtime.state === 'running');
    await openPage();
    const savedHistory = await history(), savedSettings = await request(`/api/plugins/${id}/settings`), beforeRestart = await journal();
    closeFrame(); page = await restart('before-share-restart.log'); ui = pageTools(page);
    await waitFor(async () => (await plugins()).find(value => value.manifest.id === id)?.runtime?.state === 'running', '重启恢复 Share');
    await waitFor(async () => (await current('dsh-plugin-health'))?.runtime?.state === 'running', '重启恢复 GitHub 插件');
    await openPage();
    record('restart', '完整应用重启保留来源、授权、原包配置与结果记录，不重放导入', (await current('dsh-plugin-health')).digest === gitUpdated.digest
      && (await current(name)).digest === active.digest && (await current(name)).runtime.state === 'running'
      && JSON.stringify(await request(`/api/plugins/${id}/settings`)) === JSON.stringify(savedSettings) && JSON.stringify(await history()) === JSON.stringify(savedHistory) && await journal() === beforeRestart);
    await screenshot('restarted');
    await remove(name); await parse(code); gate.pause(name);
    await waitFor(() => clickText('全部可处理项加入队列'), 'Share 停用前发起真实导入');
    await waitFor(() => gate.held.size > 0, 'Share 停用前真实准备');
    closeFrame(); await request(`/api/plugins/${id}/disable`, {});
    await waitFor(() => gate.held.size === 0, 'Share 停用取消准备'); gate.release();
    await waitFor(() => page.evaluate(`!document.querySelector('iframe[data-plugin-id="${id}"]')`), 'Share 停用释放文档');
    const stoppedJournal = await waitFor(async () => { const value = await journal(); return Object.values(JSON.parse(value).records).every(record => record.phase === 'finished') && value; }, 'Share 停用结算原导入持久回执');
    record('deactivation', '停用 Share 回收原包文档和在途导入，未提交目标保持未安装', !await current(name)
      && await page.evaluate(`!document.querySelector('iframe[data-plugin-id="${id}"]')`));
    await request(`/api/plugins/${id}/enable`, {});
    await waitFor(async () => (await plugins()).find(value => value.manifest.id === id)?.runtime?.state === 'running', 'Share 重新启用');
    await openPage();
    const restoredHistory = await history();
    const restoredExport = await call('export', {});
    const restoration = { entries: restoredExport.entries, historyPreserved: savedHistory.items.every(value => restoredHistory.items.some(item => JSON.stringify(item) === JSON.stringify(value))),
      journalUnchanged: await journal() === stoppedJournal, targetAbsent: !await current(name) };
    evidence.reactivation = restoration;
    record('reactivation', 'Share 重新启用仍可导出真实原包，既有历史保持且旧导入未重放', restoredExport.entries.some(value => value.spec === 'github:easonmws/dsh-plugin-health#v3.0.6' && value.version === '3.0.6' && value.enabled === true)
      && restoration.historyPreserved && restoration.journalUnchanged && restoration.targetAbsent, restoration);
    await remove(name); await remove('dsh-plugin-health');
    record('uninstall', '导入的 npm 与 GitHub 原包沿同一生命周期入口移除', !await current(name) && !await current('dsh-plugin-health'));
    record('pageErrors', 'Share 原页面与 Renderer 无运行异常', errors.length === 0, errors);
    Object.assign(evidence, { completed: true, savedHistory, savedSettings, gitUpdated });
    return { page, checks: evidence.checks };
  } catch (error) {
    evidence.error = String(error.stack ?? error);
    evidence.page = await frame?.evaluate('document.body.innerText').catch(() => null);
    await screenshot('failure').catch(() => {}); throw error;
  } finally {
    closeFrame();
    if (!evidence.completed) await request(`/api/plugins/${id}/disable`, {}).catch(() => {});
    gate.release();
    await remove(name); await remove('dsh-plugin-health');
    await writeFile(join(output, 'share-business.json'), JSON.stringify(evidence, null, 2));
  }
}
