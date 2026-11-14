import assert from 'node:assert/strict';
import { readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { connectTarget, pageTools, waitFor } from './electron-harness.mjs';
import { waitForInstalledPlugin } from './plugin-native-acceptance.mjs';

// 专项和社区验证同一 Market 原包任务，不把后续用户授权改记成原安装成功。
export async function verifyDshMarket({ request, origin, plugin, page, cdpPort, errors, output, stateRoot, restart, check }) {
  assert.equal(plugin.manifest.version, '1.2.2');
  const id = plugin.manifest.id, probe = 'dsh-plugin-release-probe', health = 'dsh-plugin-health', mgr = 'dsh-plugin-mgr';
  const evidence = { package: '@99galaxy/dsh-plugin-market@1.2.2', digest: plugin.digest, completed: false, checks: {}, receipts: [] };
  const record = (key, label, ok, detail) => { check(label, ok, detail); evidence.checks[key] = true; };
  const plugins = async () => (await request('/api/plugins')).plugins;
  const current = async name => (await plugins()).find(value => value.source.location.startsWith(`dsh:${name}@`));
  const route = (action, body, query) => request(`/api/plugins/${id}/routes`, {
    method: body === undefined ? 'GET' : 'POST', path: `/dsh-plugin-market/${action}`,
    ...(body === undefined ? {} : { body, headers: { 'content-type': 'application/json' } }), ...(query ? { query } : {}),
  });
  const list = async query => { const result = await route('list', undefined, query); assert.equal(result.status, 200, JSON.stringify(result)); assert.equal(result.body.ok, true); return result.body.payload; };
  const install = spec => route('install', { install: `dsh plugin add ${spec}` });
  const journal = () => readFile(join(stateRoot, 'plugins/installations.json'), 'utf8');
  const remove = async name => {
    const value = await current(name); if (!value) return;
    const path = `/api/plugins/${value.manifest.id}`;
    if (value.enabled) await request(path + '/disable', {});
    if (value.enabled) await request(path + '/disable', {});
    await request(path + '/uninstall', {});
  };
  let ui = pageTools(page), frame;
  const closeFrame = () => { frame?.close(); frame = undefined; };
  const screenshot = label => ui.screenshot().then(bytes => writeFile(join(output, `market-${label}.png`), Buffer.from(bytes, 'base64')));
  const authorize = async target => {
    const before = await journal();
    const active = await waitForInstalledPlugin({ request, page, plugin: target });
    assert.equal(await journal(), before, '原生授权不能创建或重放 Market 的安装任务');
    await ui.click('[data-testid=settings-back]');
    return active;
  };
  const installPending = async (spec, name, version) => {
    const receipt = await install(spec), pending = await current(name);
    evidence.receipts.push({ spec, receipt, pending });
    assert.equal(receipt.status, 200, JSON.stringify(receipt)); assert.equal(receipt.body.ok, true);
    assert.equal(pending?.manifest.version, version); assert.equal(pending.enabled, true);
    return authorize(pending);
  };
  const openPage = async () => {
    closeFrame();
    const session = (await request('/api/session/materialize', { scope: 'personal' })).sessionId;
    await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${session}` });
    await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), 'Market 工作台');
    if (!await page.evaluate('Boolean(document.querySelector(".right-pane")?.getClientRects().length)')) await ui.click('.header-right-pane-btn');
    await waitFor(() => ui.exists('.right-pane-add-tab'), 'Market 右栏'); await ui.click('.right-pane-add-tab');
    const view = plugin.manifest.contributions.views.find(value => value.placements.includes('rightPane'));
    const tool = `.launcher-tool[data-plugin-id=${JSON.stringify(id)}][data-contribution-id=${JSON.stringify(view.id)}]`;
    await waitFor(() => ui.exists(tool), 'Market 原包入口'); await ui.click(tool);
    const selector = `iframe[data-plugin-id=${JSON.stringify(id)}]`;
    const url = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`), 'Market 原包文档');
    frame = await waitFor(() => connectTarget(cdpPort, target => ['iframe', 'page'].includes(target.type) && target.url === url, 'Market 原页面', errors), 'Market 隔离文档');
    await waitFor(() => frame.evaluate('Boolean(document.querySelector(".dsxpm-input"))'), 'Market 原搜索框');
  };
  const clickText = text => frame.evaluate(`(() => {const button=[...document.querySelectorAll('button')].find(button=>button.innerText.trim()===${JSON.stringify(text)}&&!button.disabled);if(!button)return false;button.click();return true;})()`);
  const searchManager = async () => {
    await frame.evaluate(`(() => {const input=document.querySelector('.dsxpm-input');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,${JSON.stringify(mgr)});input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
    await waitFor(() => clickText('搜索'), 'Market 原页面提交搜索');
    await waitFor(() => frame.evaluate(`Boolean([...document.querySelectorAll('.dsxpm-card')].find(card=>card.querySelector('.dsxpm-name')?.textContent===${JSON.stringify(mgr)}))`), 'Market 原搜索结果');
  };
  try {
    const initial = (await plugins()).find(value => value.manifest.id === id && value.digest === plugin.digest);
    assert.equal(initial?.runtime?.state, 'running', 'Market 运行基线必须读取 daemon 当前事实');
    for (const name of [probe, health, mgr]) assert.equal(await current(name), undefined, `独立 Market 验收不能覆盖已有 ${name}`);
    record('noDelegation', 'Market 原包未申请代授权能力', !plugin.manifest.permissions.some(value => value.kind === 'pluginManagement' && value.targets.includes('authorize')));
    const inventory = await list('only=installed&limit=5000');
    record('inventory', 'Market 已安装目录来自 daemon 的真实版本', inventory.items.some(value => value.installedAs === '@99galaxy/dsh-plugin-market' && value.installedVersion === '1.2.2'));
    await installPending(`${probe}@0.0.7`, probe, '0.0.7');
    record('npmInstall', 'Market npm 安装保留未授权失败，用户原生授权同一安装后运行', (await current(probe)).runtime.state === 'running');
    const installed = await waitFor(async () => { const value = await list(`q=${probe}&only=installed`); return value.unknownTotal === inventory.unknownTotal + 1 && value; }, 'Market 原包后台刷新安装快照', 30_000);
    record('unknownInventory', '未收录包按原目录语义显示真实安装及版本未知', installed.unknownTotal === inventory.unknownTotal + 1);
    const updated = await installPending(`${probe}@0.0.8`, probe, '0.0.8');
    record('npmUpdate', 'Market npm 在线更新后由用户授权新摘要，无重复安装', updated.manifest.version === '0.0.8');
    const missing = await install(`${probe}@9999.0.0`), preserved = await current(probe);
    record('failurePreservation', '不存在版本保留旧包、授权和原运行实例', missing.body.ok === false && preserved.digest === updated.digest && preserved.runtime.generation === updated.runtime.generation);
    await installPending('github:easonmws/dsh-plugin-health#v3.0.5', health, '3.0.5');
    const gitUpdated = await installPending('git+https://github.com/easonmws/dsh-plugin-health.git#v3.0.6', health, '3.0.6');
    record('gitSources', 'Market GitHub 安装和 HTTPS Git 更新经原生授权后真实运行', gitUpdated.runtime.state === 'running' && gitUpdated.source.github.reference === 'v3.0.6');
    const gitMissing = await install('github:easonmws/dsh-plugin-health#magi-missing-revision-acceptance');
    record('gitFailure', 'Git 不存在引用保留已授权版本和运行实例', gitMissing.body.ok === false && (await current(health)).digest === gitUpdated.digest && (await current(health)).runtime.generation === gitUpdated.runtime.generation);
    const reload = await route('reload', {}), afterReload = await current(probe);
    record('profileReload', '原包重载保留未变化的摘要、授权和运行实例', reload.status === 200 && reload.body.ok === true && reload.body.pending.length === 0 && reload.body.warnings.length === 0 && reload.body.restartReason === '' && afterReload.digest === updated.digest && afterReload.runtime.generation === updated.runtime.generation);
    await request(`/api/plugins/${updated.manifest.id}/disable`, {});
    const disabledReload = await route('reload', {}), disabled = await current(probe);
    record('disabledIntent', 'Market 重载保留目标插件的全局停用意愿', disabledReload.body.ok === true && !disabled.enabled && !disabled.enabled);
    await remove(probe); await remove(health);
    await openPage(); await searchManager();
    const entry = (await list(`q=${mgr}`)).items.find(value => value.name === mgr);
    assert.match(entry.install, /\badd dsh-plugin-mgr$/);
    const expected = (await request('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: mgr })).candidate;
    const marker = await frame.evaluate('window.__marketAcceptance=crypto.randomUUID()');
    await frame.evaluate(`[...document.querySelectorAll('.dsxpm-card')].find(card=>card.querySelector('.dsxpm-name')?.textContent===${JSON.stringify(mgr)}).querySelector('button').click()`);
    const pending = await waitFor(async () => { const value = await current(mgr); return value?.digest === expected.digest && value; }, 'Market 原按钮在线安装 Mgr', 240_000);
    const manager = await authorize(pending);
    record('originalPageInstall', 'Market 原按钮在线安装后直接启用并运行，原页面文档保持不变', pending.enabled && manager.digest === expected.digest
      && await frame.evaluate(`window.__marketAcceptance===${JSON.stringify(marker)}`));
    // 安装完成后沿原按钮刷新快照。
    await waitFor(() => clickText('强制重新获取'), 'Market 原页面显式刷新');
    await waitFor(() => frame.evaluate(`Boolean([...document.querySelectorAll('.dsxpm-card')].find(card=>card.querySelector('.dsxpm-name')?.textContent===${JSON.stringify(mgr)})?.querySelector('.dsxpm-installed'))`), 'Market 原页面刷新后的已安装事实', 120_000);
    record('originalPageInventory', '原页面显式刷新后展示同一在线版本的真实安装', (await list(`only=installed&q=${mgr}`)).items.some(value => value.installedAs === mgr && value.installedVersion === expected.manifest.version));
    await waitFor(() => clickText('重载 profile'), 'Market 原页面重载');
    await waitFor(() => frame.evaluate('window.__marketAcceptance===undefined&&Boolean(document.querySelector(".dsxpm-input"))'), 'Market 原页面实际刷新文档');
    await searchManager();
    record('originalPageReload', '原页面重载后的新文档桥继续可用且不重启未变化宿主', (await current(mgr)).runtime.generation === manager.runtime.generation && (await plugins()).find(value => value.manifest.id === id).runtime.generation === initial.runtime.generation);
    await screenshot('reloaded'); closeFrame();
    await request(`/api/plugins/${id}/disable`, {});
    await waitFor(async () => !await ui.exists(`iframe[data-plugin-id=${JSON.stringify(id)}]`), 'Market 停用释放原包文档');
    const beforeEnable = await journal(); await request(`/api/plugins/${id}/enable`, {});
    await waitFor(async () => (await plugins()).find(value => value.manifest.id === id)?.runtime?.state === 'running', 'Market 重新启用运行');
    await openPage(); await searchManager();
    record('lifecycle', 'Market 全局停用和重新启用保留安装事实且不重放安装', await journal() === beforeEnable && (await current(mgr)).digest === manager.digest && (await current(mgr)).runtime.state === 'running');
    closeFrame(); const beforeRestart = await journal(); page = await restart(); ui = pageTools(page);
    await waitFor(async () => (await current(mgr))?.runtime?.state === 'running' && (await plugins()).find(value => value.manifest.id === id)?.runtime?.state === 'running', '完整应用重启恢复 Market 及目标插件', 120_000);
    await openPage(); await searchManager();
    record('restart', '完整应用重启恢复真实目录、授权和业务页面且不重放安装', await journal() === beforeRestart && (await current(mgr)).digest === manager.digest && (await list(`only=installed&q=${mgr}`)).items.some(value => value.installedVersion === expected.manifest.version));
    await screenshot('restarted'); await remove(mgr);
    record('uninstall', 'Market 安装的目标沿唯一卸载入口移除', !await current(mgr));
    evidence.completed = true; return { page, checks: evidence.checks };
  } catch (error) {
    evidence.error = error.stack ?? String(error); evidence.page = await frame?.evaluate('document.body.innerText').catch(() => null);
    await screenshot('failure').catch(() => {}); throw error;
  } finally {
    closeFrame(); await writeFile(join(output, 'market-business.json'), JSON.stringify(evidence, null, 2));
    for (const name of [probe, health, mgr]) await remove(name);
  }
}
