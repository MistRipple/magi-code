// 在线原包验收：真实 macOS Magi/daemon/Node，REA 前置程序由调用方准备；不改写原插件。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const reaBin = process.env.MAGI_REA_BIN_DIR || join(root, 'target/plugin-acceptance/rea-prerequisite/node_modules/.bin');
const port = 38940, cdpPort = 9310, origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-rea-live-'));
const output = join(root, 'target/plugin-acceptance/dsh-rea');
const report = createReporter(), errors = [];
let app, page, frame, ui, pluginId;
await mkdir(output, { recursive: true });
const api = async (path, body, method = body === undefined ? 'GET' : 'POST') => {
  const response = await fetch(origin + path, { method, headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(180_000) });
  const value = await response.json(); assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`); return value;
};
const installed = async () => (await api('/api/plugins')).plugins.find((item) => item.manifest.name === 'dsh-rea');
const snapshot = async (name) => writeFile(join(output, `${name}.png`), Buffer.from(await ui.screenshot(), 'base64'));
const settings = async (id) => {
  if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) await ui.click('[data-testid=sidebar-settings]');
  await waitFor(async () => {
    const selector = `[data-testid="settings-nav-${id}"]`;
    if (!await ui.exists(selector)) return false;
    await ui.click(selector);
    return page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.getAttribute('aria-current') === 'page'`);
  }, `设置分类 ${id}`);
};
const status = async () => (await api(`/api/plugins/${pluginId}/routes`, {
  method: 'POST', path: '/__magi_compat/remote', body: { endpoint: 'rea/status', args: { request: {} } },
})).body;
try {
  report.check('独立验收端口空闲', !await fetch(origin + '/health').then(() => true, () => false)
    && !await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false));
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port),
      MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '', PATH: `${reaBin}${delimiter}${process.env.PATH}` } });
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '真实 Magi 主窗口', 60_000); ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台'); await settings('plugins');
  await waitFor(() => ui.exists('[data-testid=plugin-search]'), '插件搜索');
  await page.evaluate('const input=document.querySelector("[data-testid=plugin-search]");input.value="dsh-rea";input.dispatchEvent(new Event("input",{bubbles:true}));true');
  const row = '[data-testid=plugin-row][data-plugin-id="upstream:dsh:dsh-rea"]';
  await waitFor(async () => await ui.exists(row) && !await ui.exists('[data-testid=plugin-search-loading]'), 'npm 搜索结果', 60_000);
  report.check('精确包名搜索优先', await page.evaluate('document.querySelector("[data-testid=plugin-row]")?.getAttribute("data-plugin-id")') === 'upstream:dsh:dsh-rea');
  await ui.click(row);
  await waitFor(() => page.evaluate(`Boolean(document.querySelector(${JSON.stringify(row + ' img')})?.naturalWidth)`), '上游 SVG 图标', 90_000);
  const imageUrl = await page.evaluate(`document.querySelector(${JSON.stringify(row + ' img')}).src`);
  const onlineIcon = await fetch(imageUrl), svg = await onlineIcon.text();
  report.check('安装前真实 SVG 保持矢量并可在列表和详情解码', onlineIcon.headers.get('content-type') === 'image/svg+xml'
    && svg.includes('<svg') && await page.evaluate('document.querySelector(".pane-icon img")?.naturalWidth > 0'));
  await snapshot('online-svg');
  await ui.click('[data-testid=plugin-detail-install]');
  const preview = await waitFor(async () => {
    if (await ui.exists('[data-testid=plugin-candidate]')) return { ok: true };
    const error = await ui.text('[data-testid=plugin-message]'); return error && { error };
  }, '真实在线翻译预览', 180_000);
  assert.ok(preview.ok, preview.error);
  report.check('在线翻译原包成功并展示权限预览', /sha512-/.test(await ui.text('[data-testid=plugin-candidate]')));
  await ui.click('[data-action=confirm-candidate]');
  const plugin = await waitFor(installed, '原包安装完成', 180_000); pluginId = plugin.manifest.id;
  report.check('安装保留原包 SVG 路径和在线来源', plugin.source.kind === 'upstream' && plugin.manifest.icon === 'ui/icon.svg');
  const icon = await fetch(`${origin}/api/plugins/${pluginId}/icon`);
  report.check('安装前后 SVG 原字节一致且直接导航被隔离', await icon.text() === svg
    && icon.headers.get('content-type') === 'image/svg+xml' && icon.headers.get('x-content-type-options') === 'nosniff'
    && icon.headers.get('content-security-policy')?.startsWith('sandbox;'));
  await waitFor(() => ui.exists('[data-action=activate]'), '激活入口'); await ui.click('[data-action=activate]');
  const running = await waitFor(async () => {
    const item = await installed();
    if (item.runtime?.state === 'failed') throw new Error(item.runtime.reason);
    return item.runtime?.state === 'running' && item.manifest.contributions.tools.length > 128 && item;
  }, '真实 REA MCP 工具目录（需要 rea-agents 前置程序）', 90_000);
  const count = running.manifest.contributions.tools.length;
  report.check('真实 REA 工具突破静态清单的 128 项限制并进入 daemon', count > 128, `${count} tools`);
  const remote = await status();
  report.check('通用 Typert Remote 返回真实安装状态与工具数量', remote.ok && remote.value.rea.ok && remote.value.tools.count === count, JSON.stringify(remote));
  await settings(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists('.settings-frame iframe'), '原包设置 iframe');
  const frameUrl = await page.evaluate('document.querySelector(".settings-frame iframe").src');
  frame = await waitFor(() => connectTarget(cdpPort, (target) => target.type === 'iframe' && target.url === frameUrl, 'REA 设置', errors), '原包设置上下文');
  await waitFor(() => frame.evaluate('Boolean(document.getElementById("plugin-config-dsh-rea-timeout"))'), '上游原生设置表单', 60_000);
  await waitFor(() => frame.evaluate(`document.body.innerText.includes(${JSON.stringify(remote.value.rea.version)})`), '上游 Remote 状态卡片', 30_000);
  report.check('原生 React 设置表单与状态卡片加载，无未提供模块', !await frame.evaluate('document.body.innerText.includes("界面加载失败")'));
  const ghidraLink = await frame.evaluate(`(()=>{const a=[...document.querySelectorAll('a[href]')].find((item)=>item.textContent.includes('Ghidra'));if(!a)return null;const style=getComputedStyle(a);return {href:a.href,target:a.target,color:style.color,decoration:style.textDecorationLine};})()`);
  report.check('上游 Ghidra 外链保留 href、target 与链接色', ghidraLink?.href === 'https://github.com/NationalSecurityAgency/ghidra/releases'
    && ghidraLink.target === '_blank' && ghidraLink.color !== await frame.evaluate('getComputedStyle(document.body).color'), JSON.stringify(ghidraLink));
  report.check('原生表单默认值与运行期配置一致', await frame.evaluate('document.getElementById("plugin-config-dsh-rea-timeout").value === "300000"'));
  await snapshot('settings');
  await frame.evaluate(`(()=>{const field=document.getElementById('plugin-config-dsh-rea-timeout');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(field,'310000');field.dispatchEvent(new Event('input',{bubbles:true}));field.dispatchEvent(new Event('change',{bubbles:true}));return true;})()`);
  await waitFor(() => frame.evaluate(`(()=>{const b=[...document.querySelectorAll('button')].find(b=>/保存|Save/.test(b.textContent));if(!b||b.disabled)return false;b.click();return true;})()`), '保存原生设置');
  await waitFor(async () => (await api(`/api/plugins/${pluginId}/settings`)).value.toolCallTimeoutMs === 310000, 'daemon 持久设置');
  await waitFor(async () => { const value = await status(); return value.ok && value.value.timeoutMs === 310000 && value.value.tools.count === count; }, 'Volatile 配置重载与 MCP 重新注册', 90_000);
  report.check('原生表单保存经 CAS 持久化并触发上游 MCP 配置重载', true);
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  await snapshot('settings-390');
  report.check('390px 原生设置无横向溢出', await page.evaluate('document.documentElement.scrollWidth<=innerWidth') && await frame.evaluate('document.documentElement.scrollWidth<=innerWidth'));
  await page.call('Emulation.clearDeviceMetricsOverride');
  await ui.click('[data-testid=plugin-page-header] input[role=switch]');
  await waitFor(async () => !(await installed()).active, '停用回收');
  report.check('停用清空真实 MCP 动态目录', (await installed()).manifest.contributions.tools.length === 0);
  await ui.click('[data-testid=plugin-page-header] input[role=switch]');
  await waitFor(async () => (await installed()).manifest.contributions.tools.length === count, '再次激活恢复目录', 90_000);
  report.check('重激活保留配置并恢复真实 MCP 工具', (await api(`/api/plugins/${pluginId}/settings`)).value.toolCallTimeoutMs === 310000);
  report.check('主窗口和原包 iframe 无未捕获运行异常', errors.length === 0, errors.join('\n'));
  await writeFile(join(output, 'evidence.json'), JSON.stringify({ pluginId, version: plugin.manifest.version, source: plugin.source, toolCount: count, setup: remote.value }, null, 2));
  console.log(report.summary());
} catch (error) {
  console.error(error); process.exitCode = 1;
  if (ui) await snapshot('failure').catch(() => {});
  if (frame) await writeFile(join(output, 'frame.txt'), await frame.evaluate('document.body.innerText').catch(() => '')).catch(() => {});
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(report.results, null, 2));
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  frame?.close(); page?.close(); await rm(work, { recursive: true, force: true });
}
