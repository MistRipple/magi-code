import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';
import { createPluginRegistryGate } from './lib/plugin-registry-gate.mjs';

// 原市场页面完成自身在线更新；取消基于真实解析连接，版本以实际委托和回执核对。
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_DSHMARKET_PORT || 38953), cdpPort = Number(process.env.MAGI_DSHMARKET_CDP_PORT || 9326), origin = `http://127.0.0.1:${port}`;
const output = process.env.MAGI_DSHMARKET_OUTPUT || join(root, 'target/plugin-acceptance/dshmarket');
const work = await mkdtemp(join(tmpdir(), 'magi-dshmarket-'));
await mkdir(output, { recursive: true });
const report = createReporter(), errors = [];
let app, page, frame, gate, ui;
const visualAudits = [], layouts = [];
const clipSource = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE, 'utf8') : null;
const request = async (path, body) => {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(1_020_000) });
  const value = await response.json(); assert.equal(response.status, 200, JSON.stringify(value)); return value;
};
try {
  assert.equal(await fetch(origin + '/health').then(() => true, () => false), false);
  assert.equal(await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false), false);
  gate = await createPluginRegistryGate();
  const launchOptions = { executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, inspectPort: cdpPort + 1, env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_NPM_REGISTRY: gate.registry, MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } };
  app = launchApp(launchOptions);
  await writeFile(join(output, 'state-root.txt'), work);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '原生工作台', 60_000);
  ui = pageTools(page);
  const spec = { ecosystem: 'dsh', package: 'dshmarket', version: '1.73.15' };
  const preview = await request('/api/plugins/upstream/preview', spec);
  await writeFile(join(output, 'preview.json'), JSON.stringify(preview, null, 2));
  report.check('官方在线原包保留缺席的第三方 Desktop 能力并通过必需业务准入', preview.package === spec.package && preview.version === spec.version && preview.translation.degraded.some(value => value.name === 'desktopPnpm' && value.status === 'dropped'));
  const list = await request('/api/plugins/upstream/install', { ...spec, expectedDigest: preview.candidate.digest });
  const plugin = list.plugins.find(value => value.source.location === 'dsh:dshmarket@1.73.15');
  assert.ok(plugin);
  const path = `/api/plugins/${plugin.manifest.id}`;
  const current = async () => (await request('/api/plugins')).plugins.find(value => value.manifest.id === plugin.manifest.id);
  const journal = async () => JSON.parse(await readFile(join(work, 'state/plugins/installations.json'), 'utf8')).records;
  await request(path + '/enable', {}); await request(path + '/authorize', { grants: plugin.manifest.permissions }); await request(path + '/activate', {});
  const running = await waitFor(async () => { const value = (await request('/api/plugins')).plugins.find(value => value.manifest.id === plugin.manifest.id); return ['running','failed'].includes(value?.runtime?.state) && value; }, '原包实际运行', 120_000);
  assert.equal(running.runtime.state,'running',running.runtime.reason);
  await writeFile(join(output, 'plugin.json'), JSON.stringify(running, null, 2));
  report.check('同一官方原包在线安装并由 daemon 激活', running.digest === preview.candidate.digest);
  const route = await request(path + '/routes', { method: 'GET', path: '/dsh-market/status' });
  await writeFile(join(output, 'status.json'), JSON.stringify(route, null, 2));
  report.check('原包市场状态使用同一 Magi profile 的实际目录', route.status === 200, JSON.stringify(route));
  report.check('原包明确报告 Agent 维护不可用，未伪造维护持有或空闲清单', route.body.agentDrain === false && route.body.agentGuardAvailable === false, JSON.stringify(route));
  const session = (await request('/api/session/materialize', { scope: 'personal' })).sessionId;
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${session}` });
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台');
  const view = running.manifest.contributions.views.find(value => value.id === 'settings');
  assert.ok(view);
  const selector = `iframe[data-plugin-id=${JSON.stringify(plugin.manifest.id)}]`;
  const openMarket = async () => {
    frame?.close(); frame = undefined;
    await waitFor(() => page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length || document.querySelector("[data-testid=sidebar-settings]"))'), '原生工作台入口');
    if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) {
      await ui.click('[data-testid=sidebar-settings]');
    }
    const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${plugin.manifest.id}:${view.id}`)}]`;
    await waitFor(() => ui.exists(nav), '原包设置入口'); await ui.click(nav);
    const url = await waitFor(() => page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src`), '原包隔离文档');
    frame = await waitFor(() => connectTarget(cdpPort, value => ['iframe', 'page'].includes(value.type) && value.url === url, '原包页面', errors), '原包页面');
    return url;
  };
  const url = await openMarket();
  await waitFor(() => frame.evaluate('Boolean([...document.querySelectorAll("button")].find(n=>n.innerText.trim()==="更新插件市场"))'), '原市场自身在线更新入口', 120_000);
  const documentStarted = await frame.evaluate('performance.timeOrigin');
  const buttonPoint = async (text) => {
    const inner = await frame.evaluate(`(() => {
      const button = [...document.querySelectorAll('button')].find(node => node.innerText.includes(${JSON.stringify(text)}) && !node.disabled);
      if (!button) throw new Error('原包操作不可用');
      const rect = button.getBoundingClientRect();
      return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2, width: innerWidth, height: innerHeight };
    })()`);
    const outer = await page.evaluate(`(() => { const rect = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect(); return { x: rect.x, y: rect.y, width: rect.width, height: rect.height }; })()`);
    return { x: outer.x + inner.x * outer.width / inner.width, y: outer.y + inner.y * outer.height / inner.height };
  };
  const pointButton = async (text, click = false) => {
    const point = await buttonPoint(text);
    await page.call('Input.dispatchMouseEvent', { type: 'mouseMoved', ...point });
    if (click) {
      await page.call('Input.dispatchMouseEvent', { type: 'mousePressed', button: 'left', clickCount: 1, ...point });
      await page.call('Input.dispatchMouseEvent', { type: 'mouseReleased', button: 'left', clickCount: 1, ...point });
    }
  };
  const recordLayout = async (name) => {
    // 等原包过渡和共享容器布局实际稳定，不把入场中间帧当最终几何。
    let previous, stable = 0;
    await waitFor(async () => {
      const value = await frame.evaluate(`(() => {
        const root = document.getElementById('root');
        return JSON.stringify([root.style.cssText, root.getBoundingClientRect().height, innerWidth, innerHeight,
          document.getAnimations().filter(animation => animation.playState === 'running' && animation.effect.getTiming().iterations !== Infinity).length]);
      })()`);
      stable = value === previous ? stable + 1 : 0; previous = value;
      return stable >= 2 && JSON.parse(value).at(-1) === 0;
    }, '原包布局稳定');
    const inner = await frame.evaluate(`(() => {
      const cancel = [...document.querySelectorAll('button')].find(node => node.innerText.trim() === '取消' && !node.disabled);
      const rect = cancel?.getBoundingClientRect();
      return { width: innerWidth, scrollWidth: document.documentElement.scrollWidth, started: performance.timeOrigin,
        padding: [getComputedStyle(document.getElementById('root')).paddingLeft, getComputedStyle(document.getElementById('root')).paddingRight],
        cancel: rect ? { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom, reachable: rect.left >= 0 && rect.right <= innerWidth && rect.top >= 0 && rect.bottom <= innerHeight } : null };
    })()`);
    const outer = await page.evaluate(`(() => {
      const iframe = document.querySelector(${JSON.stringify(selector)}), rect = iframe.getBoundingClientRect(), scroller = iframe.closest('.settings-frame');
      return { width: innerWidth, scrollWidth: document.documentElement.scrollWidth, frameLeft: rect.left, frameRight: rect.right,
        contentWidth: scroller.clientWidth, contentScrollWidth: scroller.scrollWidth };
    })()`);
    const layout = { name, inner, outer };
    layouts.push(layout);
    await writeFile(join(output, name + '-layout.json'), JSON.stringify(layout, null, 2));
    await writeFile(join(output, name + '.png'), Buffer.from(await ui.screenshot(), 'base64'));
    if (clipSource) {
      const audit = await frame.evaluate(`(${clipSource})({viewport:true})`);
      visualAudits.push({ name, audit });
      await writeFile(join(output, name + '-clip.json'), JSON.stringify(audit, null, 2));
    }
  };
  await page.call('Page.bringToFront');
  await page.call('Emulation.setDeviceMetricsOverride', { width: 1280, height: 844, deviceScaleFactor: 1, mobile: false });
  await recordLayout('market-desktop-default');
  await writeFile(join(output,'before.json'),JSON.stringify(await frame.evaluate('({text:document.body.innerText,buttons:[...document.querySelectorAll("button")].map(n=>n.innerText)})'),null,2));
  const updates=await request(path+'/routes',{method:'GET',path:'/dsh-market/updates'});
  await writeFile(join(output,'updates.json'),JSON.stringify(updates,null,2));
  const status=updates.body.updates.dshmarket;const offeredVersion=status.eligibleVersion??status.latest;
  assert.match(offeredVersion,/^\d+\.\d+\.\d+/);assert.notEqual(offeredVersion,spec.version);
  report.check('原市场从在线来源识别自己的新版',true);
  gate.pause('dshmarket');
  const begin=()=>frame.evaluate('(()=>{const b=[...document.querySelectorAll("button")].find(n=>n.innerText.trim()==="更新插件市场");if(!b||b.disabled)throw new Error("更新入口不可用");b.click()})()');
  await begin();
  await waitFor(()=>gate.held.size>0,'原包自身更新进入同一 daemon 真实解析',20_000);
  await writeFile(join(output,'updating.png'),Buffer.from(await ui.screenshot(),'base64'));
  await pointButton('正在安装', true);
  await waitFor(()=>frame.evaluate('Boolean([...document.querySelectorAll("button")].find(n=>n.innerText.trim()==="取消"&&!n.disabled))'),'原市场取消操作');
  await recordLayout('cancel-menu');
  await pointButton('取消');
  await recordLayout('cancel-menu-hover');
  await pointButton('正在安装', true);
  await waitFor(()=>frame.evaluate('![...document.querySelectorAll("button")].some(n=>n.innerText.trim()==="取消"&&!n.disabled)'),'任务面板实际收起');
  await recordLayout('cancel-menu-closed');
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  await recordLayout('market-390-default');
  await pointButton('正在安装', true);
  await waitFor(()=>frame.evaluate('Boolean([...document.querySelectorAll("button")].find(n=>n.innerText.trim()==="取消"&&!n.disabled))'),'390px 原市场取消操作');
  await recordLayout('cancel-menu-390');
  await pointButton('取消');
  await recordLayout('cancel-menu-390-hover');
  await pointButton('取消', true);
  await waitFor(()=>gate.held.size===0,'取消真实请求连接');
  const cancelledJob=await waitFor(async()=>Object.values(await journal()).find(value=>value.phase==='finished'&&value.result?.application==='cancelled'),'daemon 取消任务结算');
  await writeFile(join(output,'cancelled-job.json'),JSON.stringify(cancelledJob,null,2));
  const cancelled=await current();
  report.check('取消自身在线更新保留旧摘要、授权和运行代次',cancelled.digest===running.digest&&cancelled.active&&cancelled.runtime.instanceId===running.runtime.instanceId&&JSON.stringify(cancelled.grants)===JSON.stringify(running.grants));
  await page.call('Emulation.setDeviceMetricsOverride', { width: 1280, height: 844, deviceScaleFactor: 1, mobile: false });
  gate.release();
  const completedIds = new Set(Object.keys(await journal()));
  await waitFor(()=>frame.evaluate('Boolean([...document.querySelectorAll("button")].find(n=>n.innerText.trim()==="更新插件市场"&&!n.disabled))'),'原页面显式重试自身更新');
  await begin();
  const completedJob=await waitFor(async()=>Object.entries(await journal()).find(([id,value])=>!completedIds.has(id)&&value.phase==='finished')?.[1],'原包显式重试后的真实安装结算',180_000);
  await writeFile(join(output,'updated-job.json'),JSON.stringify(completedJob,null,2));
  const selected = JSON.parse(completedJob.fingerprint), toVersion = selected.version;
  assert.equal(selected.package,'dshmarket'); assert.notEqual(toVersion,spec.version);
  assert.equal(completedJob.result.application,'applied',JSON.stringify(completedJob.result));
  assert.equal(completedJob.result.version,toVersion,'提交版本必须与原市场实际请求一致；独立的 metadata 读取不替代该请求');
  const pending=await waitFor(async()=>{const value=await current();return value?.manifest.version===toVersion&&value.active&&value.runtime?.state==='running'&&value;},'实际新版运行',120_000);
  await writeFile(join(output,'updated.json'),JSON.stringify(pending,null,2));
  report.check('原市场自身更新沿既有授权运行新摘要，未扩大授权',pending.digest!==running.digest&&pending.active&&pending.runtime.instanceId!==running.runtime.instanceId&&JSON.stringify(pending.grants)===JSON.stringify(running.grants));
  frame.close();frame=undefined;
  const newUrl=await waitFor(()=>page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.src!==${JSON.stringify(url)}&&document.querySelector(${JSON.stringify(selector)})?.src`),'新版原包隔离文档');
  frame=await waitFor(()=>connectTarget(cdpPort,value=>['iframe','page'].includes(value.type)&&value.url===newUrl,'新版原包页面',errors),'新版原包页面');
  await waitFor(()=>frame.evaluate(`document.body.innerText.includes(${JSON.stringify('v'+toVersion)})`),'新版原市场显示真实版本');
  report.check('新版原页面重新挂载并读取当前版本',true);
  await writeFile(join(output,'updated.png'),Buffer.from(await ui.screenshot(),'base64'));
  const settled = JSON.stringify(await journal());
  frame.close(); frame=undefined; page.close();
  await app.stop(); await writeFile(join(output,'before-restart.log'),app.log.join(''));
  app=launchApp(launchOptions);
  page=await waitFor(()=>connectAppPage(cdpPort,port,errors),'完整重启后的工作台',60_000);ui=pageTools(page);
  await openMarket();
  await waitFor(()=>frame.evaluate(`document.body.innerText.includes(${JSON.stringify('v'+toVersion)})`),'重启后的原市场版本');
  const restarted=await current();
  report.check('应用重启恢复实际新版、原授权且不重放安装',restarted.digest===pending.digest&&restarted.active&&JSON.stringify(restarted.grants)===JSON.stringify(pending.grants)&&JSON.stringify(await journal())===settled);
  await request(path+'/deactivate',{});
  await waitFor(()=>page.evaluate(`!document.querySelector(${JSON.stringify(selector)})`),'停用释放原文档');
  report.check('停用实际市场不保留运行文档',!(await current()).active);
  await request(path+'/activate',{});
  const reactivated=await waitFor(async()=>{const value=await current();return value.active&&value.runtime?.state==='running'&&value;},'市场重新激活',120_000);
  await openMarket();
  await waitFor(()=>frame.evaluate(`document.body.innerText.includes(${JSON.stringify('v'+toVersion)})`),'重新激活的原页面');
  report.check('重新激活使用新实例并保留同一摘要，无迟到安装',reactivated.digest===pending.digest&&reactivated.runtime.instanceId!==restarted.runtime.instanceId&&JSON.stringify(await journal())===settled);
  report.check('主窗口与所有原包文档无运行异常',errors.length===0,JSON.stringify(errors));
  await writeFile(join(output,'reactivated.png'),Buffer.from(await ui.screenshot(),'base64'));
  // 保留同一视觉门槛；先取得业务终态，避免视觉失败掩盖取消、更新及恢复结果。
  report.check('宽屏与390px原页面、设置框架无横向溢出且缩放不重建文档', layouts.every(({ inner, outer }) =>
    inner.scrollWidth <= inner.width && outer.scrollWidth <= outer.width && outer.contentScrollWidth <= outer.contentWidth
    && outer.frameLeft >= 0 && outer.frameRight <= outer.width && inner.started === documentStarted), JSON.stringify(layouts));
  report.check('原任务面板在桌面及390px保留可达取消操作', layouts.filter(value => value.inner.cancel).length === 4
    && layouts.every(({ inner }) => !inner.cancel || inner.cancel.reachable), JSON.stringify(layouts));
  report.check('收起原任务面板恢复原内容列留白', JSON.stringify(layouts.find(value => value.name === 'cancel-menu-closed').inner.padding)
    === JSON.stringify(layouts.find(value => value.name === 'market-desktop-default').inner.padding));
  for (const { name, audit } of visualAudits) report.check(`${name} 原包阴影完整`, audit.cut === 0, JSON.stringify(audit));

} catch (error) {
  report.results.push({ name: '原包业务验证', ok: false, detail: error.stack });
  if (page) await writeFile(join(output, 'failure.png'), Buffer.from(await pageTools(page).screenshot(), 'base64')).catch(() => {});
  if (frame) await writeFile(join(output, 'failure-page.json'), JSON.stringify(await frame.evaluate('({text:document.body.innerText,html:document.body.innerHTML.slice(0,3000)})'), null, 2)).catch(()=>{});
  process.exitCode = 1; console.error(error);
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(report.results, null, 2));
  await writeFile(join(output, 'errors.json'), JSON.stringify(errors, null, 2));
  frame?.close(); page?.close();
  if (app) { await app.stop(); await writeFile(join(output, 'electron.log'), app.log.join('')); }
  await gate?.close();
  console.log(report.summary());
}
