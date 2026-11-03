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
    while (Date.now() < end) { if (await js(source)) return; await new Promise(r => setTimeout(r,100)); }
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
  await wait(`Boolean(${select('[data-testid="plugin-jobs"] form')})`, 'job form');
  const text = await js(`${select('[data-testid="plugin-jobs"]')}.innerText`);
  if (!text.includes(session)) throw Error('UI lost target session');
  await js(`(() => { const form=${select('[data-testid="plugin-jobs"] form')}; const command=form.querySelector('select'); command.value='status'; command.dispatchEvent(new Event('change',{bubbles:true})); const delay=form.querySelector('input[type="number"]'); delay.value='0'; delay.dispatchEvent(new Event('input',{bubbles:true})); })()`);
  await new Promise(r => setTimeout(r,100));
  await click('[data-testid="plugin-jobs"] button[type="submit"]');
  await wait(`fetch('/api/plugins/test.local/jobs').then(r=>r.json()).then(j=>j.filter(v=>v.lastOutcome==='completed').length>=2)`, 'schedule created from UI');
  await wait(`${select('[data-testid="plugin-jobs"]')}.innerText.includes('completed')`, 'daemon result rendered');
  await js(`${select('[data-testid="plugin-jobs"]')}.scrollIntoView({block:'start'})`);
  await new Promise(r => setTimeout(r,300));
  await fs.writeFile(path.join(output,'plugin-jobs-desktop.png'), (await win.webContents.capturePage()).toPNG());
  win.setContentSize(390,844); nativeTheme.themeSource='dark'; await new Promise(r=>setTimeout(r,500));
  await js(`${select('[data-testid="plugin-jobs"]')}.scrollIntoView({block:'start'})`);
  await new Promise(r => setTimeout(r,300));
  if (!await js('document.documentElement.scrollWidth<=innerWidth')) throw Error('UI viewport overflow at 390px');
  await fs.writeFile(path.join(output,'plugin-jobs-narrow.png'), (await win.webContents.capturePage()).toPNG());
  console.log('UI: schedule creation, target/budget/result, desktop and 390px viewport passed');
  app.exit(0);
}).catch(e => { console.error(e); app.exit(1); });
