// 上游生态来源的真实 Chromium 验收：在设置 → 插件里搜索 npm 关键词，核对条目标注，
// 预览翻译结果，确认安装，再核对已安装状态。业务事实始终来自 daemon。
const {app, BrowserWindow} = require('electron');
const fs = require('node:fs/promises');
const path = require('node:path');
const {MAGI_PLUGIN_UI_URL: url, MAGI_PLUGIN_UI_OUTPUT: output, MAGI_PLUGIN_UI_USER_DATA: userData, MAGI_UPSTREAM_QUERY: query = 'dsh-connect-workbuddy', MAGI_UPSTREAM_PACKAGE: pkg = 'dsh-connect-workbuddy'} = process.env;
app.setPath('userData', userData);
app.whenReady().then(async () => {
  const win = new BrowserWindow({width: 1280, height: 1000, show: false, webPreferences: {contextIsolation: true, nodeIntegration: false}});
  const js = source => win.webContents.executeJavaScript(source);
  const q = selector => `document.querySelector(${JSON.stringify(selector)})`;
  async function wait(source, label, ms = 60000) {
    const end = Date.now() + ms;
    while (Date.now() < end) { if (await js(source)) return; await new Promise(r => setTimeout(r, 150)); }
    throw Error(`UI timeout: ${label}`);
  }
  const results = [];
  const check = (name, ok, extra = '') => { results.push({name, ok}); console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${extra ? ' — ' + extra : ''}`); if (!ok) process.exitCode = 1; };
  try {
    await win.loadURL(url);
    await wait(`Boolean(${q('[data-testid="sidebar-settings"]')})`, 'workbench');
    await js(`${q('[data-testid="sidebar-settings"]')}.click()`);
    await wait(`Boolean(${q('[data-testid="settings-nav-plugins"]')})`, 'plugins nav');
    await new Promise(r => setTimeout(r, 800));
    await js(`${q('[data-testid="settings-nav-plugins"]')}.click()`);
    await wait(`Boolean(${q('[data-testid="plugin-search"]')})`, 'search box');
    check('空搜索显示上游搜索提示', await js(`Boolean(${q('[data-testid="plugin-upstream-hint"]')})`));
    await js(`(() => { const i=${q('[data-testid="plugin-search"]')}; i.value=${JSON.stringify(query)}; i.dispatchEvent(new Event('input',{bubbles:true})); })()`);
    const rowSel = `[data-testid="plugin-row"][data-plugin-id="upstream:dsh:${pkg}"]`;
    await wait(`Boolean(${q(rowSel)})`, 'upstream row');
    check('搜索结果出现 DSH 上游条目', true);
    await js(`${q(rowSel)}.click()`);
    await wait(`Boolean(${q('[data-testid="plugin-upstream-badges"]')})`, 'badges');
    const badges = await js(`${q('[data-testid="plugin-upstream-badges"]')}.innerText`);
    check('详情标注生态与发布者', /DSH/.test(badges) && /发布者/.test(badges), badges.replace(/\n/g, ' | '));
    check('详情说明发布者不代表审核', await js(`/不代表/.test(${q('[data-testid="plugin-upstream-notice"]')}?.innerText ?? '')`));
    check('安装按钮可用', await js(`!${q('[data-testid="plugin-detail-install"]')}.disabled`));
    await fs.writeFile(path.join(output, 'upstream-detail.png'), (await win.webContents.capturePage()).toPNG());
    await js(`${q('[data-testid="plugin-detail-install"]')}.click()`);
    await wait(`Boolean(${q('[data-testid="plugin-candidate"][data-origin="upstream"]')})`, 'candidate preview', 120000);
    const card = await js(`${q('[data-testid="plugin-candidate"]')}.innerText`);
    check('预览展示翻译结果（工具、命令、模型数据源）', /codebuddy_ask/.test(card) && /codebuddy/.test(card) && /workbuddy/.test(card), '');
    check('预览展示完全信任权限与 integrity', /完全信任|full-trust|extension/i.test(card) && /sha512-/.test(card));
    check('预览列出不显示的界面位置', /不显示的界面位置/.test(card));
    await fs.writeFile(path.join(output, 'upstream-preview.png'), (await win.webContents.capturePage()).toPNG());
    await js(`${q('[data-action="confirm-candidate"]')}.click()`);
    await wait(`Boolean(${q('[data-testid="plugin-row"][data-plugin-id="dsh.dsh-connect-workbuddy"]')})`, 'installed row', 60000);
    check('安装后以 Magi 插件 ID 出现在列表', true);
    await js(`${q('[data-testid="plugin-row"][data-plugin-id="dsh.dsh-connect-workbuddy"]')}.click()`);
    await wait(`/上游生态/.test(${q('[data-testid="plugin-source"]')}?.innerText ?? '')`, 'source label');
    const source = await js(`${q('[data-testid="plugin-source"]')}.innerText`);
    check('来源显示为上游生态与精确版本', /上游生态/.test(source) && /dsh-connect-workbuddy@\d/.test(source), source.replace(/\n/g, ' '));
    check('不再重复出现临时上游条目', !await js(`Boolean(${q(rowSel)})`));
    check('已装条目的图标已加载', await js(`(() => { const i=${q('[data-testid="plugin-row"][data-plugin-id="dsh.dsh-connect-workbuddy"] img')}; return !!i && i.naturalWidth>0; })()`));
    await fs.writeFile(path.join(output, 'upstream-installed.png'), (await win.webContents.capturePage()).toPNG());
  } catch (error) {
    console.log('ERROR', error.message); process.exitCode = 1;
    try { await fs.writeFile(path.join(output, 'upstream-error.png'), (await win.webContents.capturePage()).toPNG()); } catch {}
  }
  app.quit();
});
