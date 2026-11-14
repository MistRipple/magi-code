import { pageTools, waitFor } from './electron-harness.mjs';
import assert from 'node:assert/strict';

// 只固定发给真实 daemon 的版本参数；不替换在线响应、原包或执行结果。
export async function previewFixedUpstreamFromNative({ page, packageName, version }) {
  let pending;
  const off = page.on('Fetch.requestPaused', event => { pending = event; });
  await page.call('Fetch.enable', { patterns: [{ urlPattern: '*/api/plugins/upstream/preview', requestStage: 'Request' }] });
  try {
    await pageTools(page).click('[data-testid=plugin-detail-install]');
    const event = await waitFor(() => pending, '原生安装提交的真实预览请求');
    const input = JSON.parse(event.request.postData);
    assert.equal(input.package, packageName);
    await page.call('Fetch.continueRequest', { requestId: event.requestId,
      postData: Buffer.from(JSON.stringify({ ...input, version })).toString('base64') });
  } finally { await page.call('Fetch.disable'); off(); }
}

// 上游先发布启动目录，再异步刷新在线目录；只在验收驱动等待，不改变产品选模。
export async function waitForPluginModel({ request, pluginId, modelId = 'deepseek-v4.1-flash', record }) {
  let previous, stableSince = Date.now();
  const result = await waitFor(async () => {
    const groups = (await request('/api/settings/models/catalog')).groups.filter(group => group.pluginId === pluginId);
    const models = groups.flatMap(group => group.models.map(model => ({ ...model, sourceId: group.sourceId })));
    const fingerprint = JSON.stringify(models);
    if (previous !== fingerprint) { previous = fingerprint; stableSince = Date.now(); }
    const selected = models.find(model => model.id === modelId);
    return selected && Date.now() - stableSince >= 6000 && { selected, groups };
  }, `真实在线目录稳定后的指定模型 ${modelId}`, 90_000, 500);
  await record(result);
  return result.selected;
}

// 在线候选、取消、提交及授权都通过现有原生 UI；调用者随后验收新版原包业务。
export async function upgradePluginFromNative({ request, page, plugin, version, check }) {
  const ui = pageTools(page), id = plugin.manifest.id;
  if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) await ui.click('[data-testid=sidebar-settings]');
  await ui.click('[data-testid=settings-nav-plugins]');
  const search = '[data-testid=plugin-search]';
  await page.evaluate(`(() => {const input=document.querySelector(${JSON.stringify(search)});Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,${JSON.stringify(plugin.manifest.name)});input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  const row = `[data-testid=plugin-row][data-plugin-id=${JSON.stringify(id)}]`;
  await waitFor(() => ui.exists(row), '已安装原包'); await ui.click(row);
  const preview = async () => {
    await waitFor(() => ui.exists('[data-testid=plugin-detail-update]'), '原生在线更新入口', 90_000);
    await ui.click('[data-testid=plugin-detail-update]');
    await waitFor(() => ui.exists('[data-testid=plugin-candidate][data-operation=upgrade]'), '实际在线升级预览', 240_000);
    assert.ok((await ui.text('[data-testid=plugin-candidate]')).includes(version), '预览必须包含实际目标版本');
  };
  const current = async () => (await request('/api/plugins')).plugins.find(item => item.manifest.id === id);
  await preview(); await ui.click('[data-action=cancel-candidate]');
  const cancelled = await current();
  check('selfUpdateCancelled', '取消原生在线更新保留原包摘要和当前实例', cancelled.digest === plugin.digest
    && cancelled.enabled && cancelled.runtime.instanceId === plugin.runtime.instanceId);
  await preview(); await ui.click('[data-action=confirm-candidate]');
  const pending = await waitFor(async () => { const value = await current(); return value?.manifest.version === version && value; }, '提交实际在线新版', 240_000);
  check('selfUpdateCommitted', '原生在线更新提交新摘要并保持启用', pending.digest !== plugin.digest && pending.enabled);
  const installed = await waitForInstalledPlugin({ request, page, plugin: pending });
  check('selfUpdateActivated', '同一新版运行且旧宿主实例已回收', installed.digest === pending.digest && installed.runtime.instanceId !== plugin.runtime.instanceId);
  return installed;
}

// 安装完成即启用；验收只等待同一安装的宿主真实运行，不再有授权或激活步骤。
export async function waitForInstalledPlugin({ request, plugin }) {
  const id = plugin.manifest.id;
  return waitFor(async () => (await request('/api/plugins')).plugins.find(value => value.manifest.id === id
    && value.digest === plugin.digest && value.enabled && value.runtime?.state === 'running'), '同一安装运行', 120_000);
}
