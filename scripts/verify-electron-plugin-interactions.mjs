// 打包版真实 Electron：在线搜索、原包图标、翻译失败、确认安装与窄屏。
// CDP 只在取消／乱序／搜索失败用例中暂停或失败 HTTP 请求，不替代插件安装事实。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = 38939, cdpPort = 9309, origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-plugin-interactions-'));
const output = process.env.MAGI_PLUGIN_INTERACTIONS_OUTPUT || join(root, 'target/plugin-acceptance/interactions');
const clipAuditSource = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE,'utf8') : null;
await mkdir(output, { recursive: true });
const report = createReporter(), errors = [];
let app, page, ui, previews = 0;
const finishedRequests = new Set();
const row = (pkg) => `[data-testid=plugin-row][data-plugin-id="upstream:dsh:${pkg}"]`;
const visible = (selector) => page.evaluate(`(()=>{const r=document.querySelector(${JSON.stringify(selector)})?.getBoundingClientRect();return !!r && r.top>=0 && r.top<innerHeight && r.left>=0 && r.right<=innerWidth;})()`);
const first = () => page.evaluate('document.querySelector("[data-testid=plugin-row]")?.getAttribute("data-plugin-id")');
const api = async (path, body) => {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  const result = await response.json(); assert.equal(response.status, 200, JSON.stringify(result)); return result;
};
const query = (text) => page.evaluate(`(()=>{const input=document.querySelector('[data-testid=plugin-search]'); input.value=${JSON.stringify(text)};input.dispatchEvent(new Event('input',{bubbles:true}));return true;})()`);
const search = async (text) => {
  await query(text);
  await waitFor(() => ui.exists('[data-testid=plugin-search-loading]'), '搜索加载反馈');
  await waitFor(async () => !await ui.exists('[data-testid=plugin-search-loading]'), '搜索完成', 60_000);
};
async function screenshot(name) {
  await writeFile(join(output, `${name}.png`), Buffer.from(await ui.screenshot(), 'base64'));
  if (clipAuditSource) {
    const audit = await page.evaluate(`(${clipAuditSource})({root:'[data-testid=plugin-detail]',viewport:true})`);
    await writeFile(join(output,`${name}-clip.json`),JSON.stringify(audit,null,2));
    report.check(`${name} 当前详情与操作区阴影无裁切`,audit.cut===0,JSON.stringify(audit));
  }
}
async function intercept(pattern) {
  let request;
  const off = page.on('Fetch.requestPaused', (event) => { request = event; });
  await page.call('Fetch.enable', { patterns: [{ urlPattern: pattern, requestStage: 'Request' }] });
  return { request: () => waitFor(() => request, '被暂停的请求'), stop: async () => { await page.call('Fetch.disable'); off(); } };
}
async function fixedAction(button, scroller) {
  return page.evaluate(`(()=>{const button=document.querySelector(${JSON.stringify(button)}),scroll=document.querySelector(${JSON.stringify(scroller)});const stress=document.createElement('div');stress.style.height='2000px';scroll.append(stress);const before=button.getBoundingClientRect();scroll.scrollTop=scroll.scrollHeight;const after=button.getBoundingClientRect();stress.remove();scroll.scrollTop=0;return Math.abs(after.bottom-before.bottom)<1 && after.bottom<=innerHeight && after.top>=0;})()`);
}
async function narrowAction(button, scroller, name, longLabel = '') {
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  try {
    await page.evaluate('document.querySelector("[data-testid=plugin-detail]").scrollIntoView({block:"nearest"}); true');
    const fixed = await fixedAction(button, scroller);
    const fits = await page.evaluate('document.documentElement.scrollWidth <= innerWidth');
    await screenshot(name);
    let longLabelFits = true;
    if (longLabel) {
      const caption = await ui.text(button);
      try {
        await page.evaluate(`document.querySelector(${JSON.stringify(button)}).textContent=${JSON.stringify(longLabel)};true`);
        const geometry = await page.evaluate(`(()=>{const footer=document.querySelector('[data-testid=plugin-candidate-actions]'),box=footer.getBoundingClientRect();return {viewport:{width:innerWidth,height:innerHeight},footer:box.toJSON(),buttons:[...footer.querySelectorAll('button')].map(b=>b.getBoundingClientRect().toJSON()),fits:document.documentElement.scrollWidth<=innerWidth&&[...footer.querySelectorAll('button')].every(b=>{const r=b.getBoundingClientRect();return r.left>=box.left&&r.right<=box.right&&r.top>=0&&r.bottom<=innerHeight;})};})()`);
        await writeFile(join(output,`${name}-long-label.json`),JSON.stringify(geometry,null,2));
        await screenshot(`${name}-long-label`);
        longLabelFits = geometry.fits;
      } finally {
        await page.evaluate(`document.querySelector(${JSON.stringify(button)}).textContent=${JSON.stringify(caption)};true`);
      }
    }
    return fixed && fits && longLabelFits;
  } finally {
    await page.call('Emulation.clearDeviceMetricsOverride');
    await page.evaluate('document.querySelector("[data-testid=plugin-detail]").scrollIntoView({block:"nearest"}); true');
  }
}
try {
  report.check('隔离验收端口空闲', !await fetch(origin + '/health').then(() => true, () => false));
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: { MAGI_STATE_ROOT: join(work, 'state'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0' } });
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'Magi 主窗口', 60_000); ui = pageTools(page);
  await page.call('Network.enable');
  for (const event of ['Network.loadingFinished', 'Network.loadingFailed']) page.on(event, ({ requestId }) => finishedRequests.add(requestId));
  page.on('Network.requestWillBeSent', ({ request }) => { if (request.url.endsWith('/api/plugins/upstream/preview')) previews++; });
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台'); await ui.click('[data-testid=sidebar-settings]');
  await waitFor(async () => { if (!await ui.exists('[data-testid=settings-nav-plugins]')) return false; await ui.click('[data-testid=settings-nav-plugins]'); return ui.exists('[data-testid=plugin-search]'); }, '插件设置');
  await query('dsh-rea');
  await waitFor(() => ui.exists('[data-testid=plugin-search-loading]'), '搜索加载反馈');
  report.check('搜索期间有真实加载动画和可读状态', await page.evaluate('getComputedStyle(document.querySelector("[data-testid=plugin-search-loading] svg")).animationName !== "none"'));
  await waitFor(async () => !await ui.exists('[data-testid=plugin-search-loading]') && await ui.exists(row('dsh-rea')), '在线搜索 dsh-rea', 60_000);
  report.check('精确匹配 dsh-rea 排在搜索结果首位', await first() === 'upstream:dsh:dsh-rea');
  await ui.click(row('dsh-rea'));
  report.check('安装按钮固定在详情底部，滚动说明不改变位置', await fixedAction('[data-testid=plugin-detail-install]', '[data-testid=plugin-detail-scroll]'));
  report.check('390px 窄屏安装按钮固定可见', await narrowAction('[data-testid=plugin-detail-install]', '[data-testid=plugin-detail-scroll]', 'install-390'));
  const rejectedPreview = await intercept(`${origin}/api/plugins/upstream/preview`);
  const before = previews;
  await page.evaluate('const button=document.querySelector("[data-testid=plugin-detail-install]");button.click();button.click();true');
  await waitFor(() => ui.exists('[data-testid=plugin-operation-progress]'), '安装检查反馈');
  report.check('重复点击只发起一次预览，按钮显示当前检查状态', previews === before + 1 && await page.evaluate('document.querySelector("[data-testid=plugin-detail-install]").disabled'));
  const rejected = await rejectedPreview.request();
  await page.call('Fetch.fulfillRequest', { requestId: rejected.requestId, responseCode: 400, responseHeaders: [{ name: 'content-type', value: 'application/json' }], body: Buffer.from(JSON.stringify({ message: 'COMPATIBILITY_CHECK_REJECTED' })).toString('base64') });
  await rejectedPreview.stop();
  await waitFor(() => ui.exists('[data-testid=plugin-message]'), '兼容检查失败提示', 30_000);
  report.check('注入的兼容检查失败在当前详情可见', await visible('[data-testid=plugin-message]')
    && /COMPATIBILITY_CHECK_REJECTED/.test(await ui.text('[data-testid=plugin-message]'))
    && await page.evaluate('document.querySelector("[data-testid=plugin-detail]").contains(document.querySelector("[data-testid=plugin-message]"))'));
  await screenshot('dsh-rea-error');
  await search('dsh-connect-workbuddy'); await ui.click(row('dsh-connect-workbuddy'));
  await waitFor(() => page.evaluate(`Boolean(document.querySelector(${JSON.stringify(row('dsh-connect-workbuddy') + ' img')})?.naturalWidth)`), '未安装原包图标', 60_000);
  report.check('未安装插件的列表与详情显示原包真实图标', await page.evaluate('document.querySelector(".pane-icon img")?.naturalWidth > 0'));
  const paused = await intercept(`${origin}/api/plugins/upstream/preview`);
  await ui.click('[data-testid=plugin-detail-install]'); const pending = await paused.request();
  await waitFor(() => ui.exists('[data-testid=plugin-cancel-preview]'), '取消预览按钮'); await ui.click('[data-testid=plugin-cancel-preview]');
  await waitFor(() => page.evaluate('!document.querySelector("[data-testid=plugin-detail-install]").disabled'), '取消后恢复可操作');
  await page.call('Fetch.continueRequest', { requestId: pending.requestId }).catch(() => {}); await paused.stop();
  report.check('取消慢预览后恢复操作且不出现迟到候选', !await ui.exists('[data-testid=plugin-candidate]'));
  await ui.click('[data-testid=plugin-detail-install]');
  await waitFor(() => ui.exists('[data-testid=plugin-candidate]'), '在线下载与翻译预览', 180_000);
  report.check('确认预览出现在当前详情并自动进入可见区域', await visible('[data-testid=plugin-candidate]')
    && await page.evaluate('document.querySelector("[data-testid=plugin-detail]").contains(document.querySelector("[data-testid=plugin-candidate]"))'));
  report.check('长权限预览滚动时确认安装按钮仍固定可见', await fixedAction('[data-action=confirm-candidate]', '[data-testid=plugin-candidate-scroll]'));
  await screenshot('preview');
  const confirmationSize = await page.evaluate(`(()=>{const b=document.querySelector('[data-action=confirm-candidate]'),r=b.getBoundingClientRect();return {height:r.height,width:r.width,requiredHeight:parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--btn-height-xl'))};})()`);
  await writeFile(join(output,'confirmation-size.json'),JSON.stringify(confirmationSize,null,2));
  report.check('确认安装保持主要操作的共享尺寸，不缩成紧凑辅助按钮',confirmationSize.height>=confirmationSize.requiredHeight,JSON.stringify(confirmationSize));
  report.check('390px 窄屏及加长文案下确认与取消操作均可见', await narrowAction('[data-action=confirm-candidate]', '[data-testid=plugin-candidate-scroll]', 'preview-390', 'Confirm installation of this plugin and review its permissions'));
  await ui.click('[data-action=confirm-candidate]');
  await waitFor(() => page.evaluate('document.querySelector("[data-action=confirm-candidate]")?.disabled'), '安装中的确认按钮');
  report.check('确认安装期间显示进度并阻止重复提交', /安装|Installing/.test(await ui.text('[data-action=confirm-candidate]')));
  const installResult = () => waitFor(async () => {
    const plugin = (await api('/api/plugins')).plugins.find((p) => p.source.location.startsWith('dsh:dsh-connect-workbuddy@'));
    if (plugin) return { plugin };
    const error = await ui.text('[data-testid=plugin-message]');
    return error && await page.evaluate('!document.querySelector("[data-action=confirm-candidate]")?.disabled') && { error };
  }, '实际安装完成', 190_000);
  let installed = await installResult();
  if (installed.error && /network|TLS|socket|timeout|error sending request/i.test(installed.error)) {
    report.check('网络失败保留确认预览与可重试按钮', await ui.exists('[data-testid=plugin-candidate]'));
    await ui.click('[data-action=confirm-candidate]'); installed = await installResult();
  }
  assert.ok(installed.plugin, installed.error); const plugin = installed.plugin;
  await writeFile(join(output,'installed.json'),JSON.stringify({id:plugin.manifest.id,version:plugin.manifest.version,source:plugin.source},null,2));
  await waitFor(() => ui.exists(`[data-testid=plugin-card][data-plugin-id="${plugin.manifest.id}"]`), '安装后保持选中');
  await waitFor(() => page.evaluate('!document.querySelector("[data-action=activate]")?.disabled'), '后续操作恢复');
  report.check('安装后保持当前插件详情且可继续授权激活', true);
  // 为在线升级准备真实 npm 旧版；不激活、不调用模型，只在本次隔离实例安装。
  await api(`/api/plugins/${plugin.manifest.id}/uninstall`, {});
  const older = { ecosystem: 'dsh', package: 'dsh-connect-workbuddy', version: '5.20.0' };
  const oldPreview = await api('/api/plugins/upstream/preview', older);
  await api('/api/plugins/upstream/install', { ...older, expectedDigest: oldPreview.candidate.digest });
  assert.equal((await api('/api/plugins')).plugins.find((p) => p.manifest.id === plugin.manifest.id)?.manifest.version, older.version);
  await page.evaluate('window.dispatchEvent(new Event("focus")); true');
  await waitFor(() => ui.exists('[data-testid=plugin-detail-update]'), '在线更新入口', 90_000);
  report.check('在线更新显示正确目标版本且没有文件升级入口', await fixedAction('[data-testid=plugin-detail-update]', '[data-testid=plugin-detail-scroll]')
    && !await page.evaluate('Boolean(document.querySelector("[data-testid=plugin-card] input[type=file]"))')
    && !(await ui.text('[data-testid=plugin-card]')).includes('vundefined'));
  report.check('390px 窄屏在线更新按钮固定可见', await narrowAction('[data-testid=plugin-detail-update]', '[data-testid=plugin-detail-scroll]', 'update-390'));
  await ui.click('[data-testid=plugin-detail-update]');
  await waitFor(() => ui.exists('[data-testid=plugin-candidate][data-operation=upgrade]'), '在线升级预览', 190_000);
  await screenshot('update-preview');
  await ui.click('[data-action=confirm-candidate]');
  await waitFor(async () => (await api('/api/plugins')).plugins.find((p) => p.manifest.id === plugin.manifest.id)?.manifest.version === plugin.manifest.version, '在线升级完成', 190_000);
  report.check('真实 npm 旧版通过在线预览与确认更新到当前版本', true, `${older.version} → ${plugin.manifest.version}`);
  await waitFor(async () => !await ui.exists('[data-testid=plugin-search-loading]'), '安装后的搜索刷新', 60_000);
  const failure = await intercept(`${origin}/api/plugins/upstream?*`);
  await query('dsh-rea'); const failed = await failure.request();
  await page.call('Fetch.fulfillRequest', { requestId: failed.requestId, responseCode: 503, responseHeaders: [{ name: 'content-type', value: 'application/json' }], body: Buffer.from(JSON.stringify({ message: 'INTERACTION_SEARCH_FAILURE' })).toString('base64') });
  await failure.stop();
  await waitFor(() => ui.exists('[data-testid=plugin-upstream-error]'), '搜索错误反馈');
  report.check('搜索失败停止加载并提供原位重试', !await ui.exists('[data-testid=plugin-search-loading]') && (await ui.text('[data-testid=plugin-upstream-error]')).includes('INTERACTION_SEARCH_FAILURE'));
  await ui.click('[data-testid=plugin-upstream-error] button');
  await waitFor(async () => !await ui.exists('[data-testid=plugin-search-loading]') && await ui.exists(row('dsh-rea')), '重试恢复', 60_000);
  const stale = await intercept(`${origin}/api/plugins/upstream?q=dsh-rea`);
  await query('dsh-rea '); const old = await stale.request();
  await search('dsh-connect-workbuddy');
  await page.call('Fetch.continueRequest', { requestId: old.requestId }).catch(() => {}); await stale.stop();
  await waitFor(() => old.networkId && finishedRequests.has(old.networkId), '旧搜索请求结束');
  report.check('连续改词后旧请求不能覆盖新搜索结果', await first() === plugin.manifest.id);
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  await page.call('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: 'light' }] });
  await search('dsh-rea'); await ui.click(row('dsh-rea'));
  const narrowFailure = await intercept(`${origin}/api/plugins/upstream/preview`);
  await ui.click('[data-testid=plugin-detail-install]');
  const narrowRequest = await narrowFailure.request();
  await page.call('Fetch.fulfillRequest', { requestId: narrowRequest.requestId, responseCode: 400, responseHeaders: [{ name: 'content-type', value: 'application/json' }], body: Buffer.from(JSON.stringify({ message: 'COMPATIBILITY_CHECK_REJECTED' })).toString('base64') });
  await narrowFailure.stop();
  await waitFor(() => ui.exists('[data-testid=plugin-message]'), '窄屏失败反馈', 180_000);
  report.check('390px 窄屏无横向溢出，失败原因自动可见', await page.evaluate('document.documentElement.scrollWidth <= innerWidth') && await visible('[data-testid=plugin-message]'));
  await screenshot('error-390');
  report.check('交互全程无未捕获脚本异常', !errors.some((e) => /Uncaught|TypeError|ReferenceError/.test(e)), errors.join('\n'));
  console.log(report.summary());
} catch (error) {
  console.error(error); process.exitCode = 1;
  if (ui) await screenshot('failure').catch(() => {});
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(report.results, null, 2));
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  page?.close(); await rm(work, { recursive: true, force: true });
}
