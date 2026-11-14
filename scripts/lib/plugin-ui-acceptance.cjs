// 真实 Chromium 工作台验收；业务事实始终从同一个 daemon 读取和提交。
const {app, BrowserWindow, nativeTheme} = require('electron');
const fs = require('node:fs/promises');
const path = require('node:path');
const {MAGI_PLUGIN_UI_URL:url,MAGI_PLUGIN_UI_SESSION:session,MAGI_PLUGIN_UI_OUTPUT:output,MAGI_PLUGIN_UI_USER_DATA:userData} = process.env;
app.setPath('userData', userData);
app.whenReady().then(async () => {
  const win = new BrowserWindow({width:1280, height:1000, show:false, webPreferences:{contextIsolation:true, nodeIntegration:false}});
  const js = source => win.webContents.executeJavaScript(source);
  const select = selector => `document.querySelector(${JSON.stringify(selector)})`;
  const click = selector => js(`${select(selector)}.click()`);
  async function wait(source, label) {
    const end = Date.now() + 45000;
    while (Date.now() < end) { if (await (typeof source === 'function' ? source() : js(source))) return; await new Promise(r => setTimeout(r,100)); }
    throw Error(`UI timeout: ${label}`);
  }
  await win.loadURL(url);
  await wait(`Boolean(${select('[data-testid="sidebar-settings"]')})`, 'workbench');
  await click('[data-testid="sidebar-settings"]');
  await wait(`Boolean(${select('[data-testid="settings-nav-plugins"]')})`, 'plugins nav');
  await new Promise(r => setTimeout(r,1000));
  await click('[data-testid="settings-nav-plugins"]');
  await wait(`Boolean(${select('[data-testid="plugin-row"][data-plugin-id="test.local"]')})`, 'installed plugin');
  await click('[data-testid="plugin-row"][data-plugin-id="test.local"]');
  // 详情页：不再有计划管理表单；已声明图标的插件在列表与详情头显示它的位图。
  await wait(`Boolean(${select('[data-testid="plugin-detail"] .pane-icon')})`, 'detail header');
  if (await js(`Boolean(${select('[data-testid="plugin-jobs"]')})`)) throw Error('plugin schedule form must be gone');
  await wait(`Boolean(${select('[data-testid="plugin-row"][data-plugin-id="test.local"] img')})`, 'list icon');
  if (!await js(`${select('[data-testid="plugin-row"][data-plugin-id="test.local"] img')}.naturalWidth>0`)) throw Error('list icon not loaded');
  if (!await js(`${select('[data-testid="plugin-detail"] .pane-icon img')}.naturalWidth>0`)) throw Error('detail icon not loaded');
  await fs.writeFile(path.join(output,'plugin-detail-desktop.png'), (await win.webContents.capturePage()).toPNG());
  // 设置导航里的插件页：顶部栏（图标、名称、开关、卸载）+ 与原生分类同一内容列。
  const navId = '[data-testid="settings-nav-plugin:test.local:page"]';
  await wait(`Boolean(${select(navId)})`, 'plugin settings nav');
  if (!await js(`${select(navId)}.querySelector('img').naturalWidth>0`)) throw Error('nav icon not loaded');
  if (await js(`Boolean(${select('iframe[data-plugin-id="test.local"]')})`)) throw Error('unvisited settings page must not load');
  await click(navId);
  await wait(`Boolean(${select('[data-testid="plugin-page-header"]')})`, 'plugin page header');
  const pluginFrame = () => win.webContents.mainFrame.framesInSubtree.find(frame => frame.url.includes('/api/plugins/test.local/'));
  await wait(async () => pluginFrame() && pluginFrame().executeJavaScript('Boolean(document.querySelector("#draft"))'), 'plugin settings document');
  const retainedFrame = pluginFrame();
  await retainedFrame.executeJavaScript('window.retentionToken=crypto.randomUUID();document.querySelector("#draft").value="unsaved draft";true');
  const retainedState = await retainedFrame.executeJavaScript('JSON.stringify([window.retentionToken,document.querySelector("#draft").value])');
  const scrollPosition = () => js(`${select('iframe[data-plugin-id="test.local"]')}.closest('.settings-frame').scrollTop`);
  await wait(`(()=>{const page=${select('iframe[data-plugin-id="test.local"]')}.closest('.settings-frame');page.scrollTop=120;return page.scrollTop===120;})()`, 'settings page scroll');
  const assertRetained = async () => {
    const currentState = await pluginFrame().executeJavaScript('JSON.stringify([window.retentionToken,document.querySelector("#draft").value])');
    if (currentState !== retainedState) throw Error(`settings navigation lost document/draft/scroll: ${retainedState} -> ${currentState}`);
    if (await scrollPosition() !== 120) throw Error('settings navigation lost page scroll');
  };
  const header = await js(`(() => { const h=${select('[data-testid="plugin-page-header"]')}; const main=document.querySelector('.main-header'); const mb=main.getBoundingClientRect(); const title=main.querySelector('h2').getBoundingClientRect(); const r=h.getBoundingClientRect(); return {inHeader:main.contains(h), title:main.querySelector('h2').innerText, headerIcon:Boolean(main.querySelector('.header-plugin-icon img')), text:h.innerText, on:h.querySelector('input[role=switch]').checked, rightEdge:Math.round(mb.right-r.right), sameRow:Math.abs((r.top+r.height/2)-(title.top+title.height/2))<8, titleLeft:Math.round(title.left), bodyDup:Boolean(document.querySelector('.settings-frame [data-testid=plugin-page-header]'))}; })()`);
  if (!header.inHeader || header.bodyDup) throw Error('plugin actions must live in the page header only: ' + JSON.stringify(header));
  if (!header.title.includes('本地验收插件') || !header.headerIcon || !header.on || !header.sameRow) throw Error('plugin page header incomplete: ' + JSON.stringify(header));
  // 插件页的标题与原生分类的标题同一左边距：切到原生分类量一次再比较。
  const pluginTitleLeft = header.titleLeft;
  await click('[data-testid="settings-nav-plugins"]');
  await new Promise(r => setTimeout(r, 500));
  if (!await js(`(() => {const page=${select('iframe[data-plugin-id="test.local"]')}.closest('.settings-frame');return page.hidden && page.inert && page.getClientRects().length===0;})()`)) throw Error('hidden settings page must leave layout and focus navigation');
  const nativeLeft = await js(`Math.round(document.querySelector('.main-header h2').getBoundingClientRect().left)`);
  const iconWidth = 30 + 12;
  if (Math.abs(pluginTitleLeft - nativeLeft - iconWidth) > 2 && Math.abs(pluginTitleLeft - nativeLeft) > 2) throw Error(`plugin header not aligned with the settings column: ${pluginTitleLeft} vs ${nativeLeft}`);
  await click(navId);
  await wait(`Boolean(${select('[data-testid="plugin-page-header"]')})`, 'plugin page header again');
  await assertRetained();
  await click('[data-testid="settings-back"]');
  await wait(`!${select('[data-testid="settings-page"]')}.getClientRects().length`, 'back to conversation');
  await click('[data-testid="sidebar-settings"]');
  await wait(`Boolean(${select('[data-testid="settings-page"]')}.getClientRects().length)`, 'return to settings');
  await assertRetained();
  await fs.writeFile(path.join(output,'plugin-page-desktop.png'), (await win.webContents.capturePage()).toPNG());
  // 关闭开关：daemon 停用，设置页与导航按“已安装”保留，页面只剩顶部栏和停用提示。
  await js(`${select('[data-testid="plugin-page-header"] input[role=switch]')}.click()`);
  await wait(`Boolean(${select('[data-testid="plugin-page-inactive"]')})`, 'inactive note');
  if (await js(`Boolean(${select('iframe[data-plugin-id="test.local"]')})`)) throw Error('inactive plugin must release its retained view');
  await wait(`fetch('/api/plugins/test.local/resources/notes').then(r=>r.status===403)`, 'daemon deactivated');
  // 窄视口无横向溢出
  win.setContentSize(390,844); nativeTheme.themeSource='dark'; await new Promise(r=>setTimeout(r,500));
  if (!await js('document.documentElement.scrollWidth<=innerWidth')) throw Error('UI viewport overflow at 390px');
  await fs.writeFile(path.join(output,'plugin-page-narrow.png'), (await win.webContents.capturePage()).toPNG());
  // 重新启用：回到插件列表里用详情面板的“启用”开启，页面随之恢复。
  await click('[data-testid="settings-nav-plugins"]');
  await wait(`Boolean(${select('[data-testid="plugin-row"][data-plugin-id="test.local"]')})`, 'plugin row');
  await click('[data-testid="plugin-row"][data-plugin-id="test.local"]');
  await wait(`Boolean(${select('[data-action="enable"]')})`, 'enable button');
  await click('[data-action="enable"]');
  await wait(`fetch('/api/plugins').then(r=>r.json()).then(j=>{const p=j.plugins.find(p=>p.manifest.id==='test.local');return p?.enabled===true&&p?.runtime?.state==='running';})`, 'plugin re-activated from the list and host running');
  await click(navId);
  await wait(`Boolean(${select('.settings-frame:not([hidden]) iframe')})`, 'plugin view restored');
  await wait(async () => pluginFrame() && pluginFrame() !== retainedFrame && pluginFrame().executeJavaScript('Boolean(document.querySelector("#draft"))'), 'new document after reactivation');
  if (!await pluginFrame().executeJavaScript('!window.retentionToken && document.querySelector("#draft").value===""')) throw Error('reactivation reused a stale document');
  console.log('UI: lazy settings mount, document/draft/scroll retention, hidden page isolation, reactivation invalidation, icons, header, alignment, 390px viewport passed');
  app.exit(0);
}).catch(e => { console.error(e); app.exit(1); });
