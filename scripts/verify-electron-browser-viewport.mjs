import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as sleep } from 'node:timers/promises';
import { connectAppPage, connectTarget, launchApp, waitFor, pageTools } from './lib/electron-harness.mjs';

// 独立状态根、本地 Provider、真实打包桌面：验证 AI 工具、用户接管和 Chromium 事实。
const root = new URL('..', import.meta.url).pathname.replace(/\/$/u, '');
const cdpPort = Number(process.env.MAGI_VIEWPORT_CDP_PORT || 9363);
const daemonPort = Number(process.env.MAGI_VIEWPORT_DAEMON_PORT || 38973);
const work = await mkdtemp(join(tmpdir(), 'magi-viewport-'));
const errors = [];
const results = new Map();
const requests = [];
const held = new Map();
const fixture = createServer((_q, r) => {
  r.writeHead(200, { 'content-type': 'text/html; charset=utf-8' });
  r.end('<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1"><style>body{margin:0;min-height:2000px;background:#edf2fa}button{margin:20px;padding:12px}footer{position:fixed;bottom:0;left:0;right:0;height:24px;background:#165e45;color:white}</style><footer onclick="this.dataset.clicked=String(Number(this.dataset.clicked||0)+1)">Viewport bottom</footer><button onclick="this.textContent=\'Clicked\'">Viewport target</button><script>window.documentKey=Math.random()</script>');
});
await new Promise(resolve => fixture.listen(0, '127.0.0.1', resolve));
const url = `http://127.0.0.1:${fixture.address().port}/`;
const plans = {
  VIEWPORT_FIRST: [
    ['browser_navigate', { url }],
    ['browser_viewport', { action: 'get' }],
    ['browser_viewport', { action: 'set', mode: 'fixed', width: 390, height: 700, device_type: 'desktop' }],
    ['browser_viewport', { action: 'set', mode: 'fixed', width: 844, height: 390, device_type: 'mobile', device_scale_factor_millis: 2000 }],
    ['browser_click_at', { x: 35, y: 35 }],
    ['browser_viewport', { action: 'set', mode: 'fixed', width: 500, height: 600 }],
  ],
  VIEWPORT_AUTO: [['browser_viewport', { action: 'get' }]],
  VIEWPORT_SECOND: [
    ['browser_scroll', { delta_y: 30 }],
    ['browser_viewport', { action: 'get' }],
  ],
  VIEWPORT_CLICK: [
    ['browser_navigate', { url: url + '?return' }],
    ['browser_viewport', { action: 'get' }],
    ['browser_viewport', { action: 'set', mode: 'fixed', width: 500, height: 600 }],
  ],
  VIEWPORT_RETURN: [['browser_scroll', { delta_y: 20 }], ['browser_viewport', { action: 'get' }]],
};
function stream(delta, finish) {
  return `data: ${JSON.stringify({ choices: [{ delta, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: finish }] })}\n\ndata: [DONE]\n\n`;
}
const provider = createServer((q, r) => {
  let body = '';
  q.on('data', chunk => { body += chunk; });
  q.on('end', () => {
    if (q.url === '/v1/models') return void r.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ data: [{ id: 'viewport-model', object: 'model' }] }));
    if (q.url !== '/v1/chat/completions') return void r.writeHead(404).end();
    const request = JSON.parse(body);
    requests.push(request);
    const messages = request.messages || [];
    const text = m => typeof m?.content === 'string' ? m.content : JSON.stringify(m?.content);
    const prompt = text([...messages].reverse().find(m => m.role === 'user'));
    const key = Object.keys(plans).find(key => prompt?.includes(key)) ?? null;
    const plan = plans[key];
    const toolMessages = messages.filter(m => m.role === 'tool' && m.tool_call_id?.startsWith(key + '-'));
    for (const m of toolMessages) results.set(m.tool_call_id, JSON.parse(m.content));
    const index = toolMessages.length;
    const reply = () => {
      r.writeHead(200, { 'content-type': 'text/event-stream' });
      if (!plan || index >= plan.length) return void r.end(stream({ content: `${key || 'VIEWPORT'}_DONE` }, 'stop'));
      const [name, args] = plan[index];
      const actualName = request.tools?.find(t => t.function.name === name || t.function.name.endsWith('_' + name) || t.function.name.endsWith('.' + name))?.function.name || name;
      r.end(stream({ tool_calls: [{ index: 0, id: `${key}-${index}`, type: 'function', function: { name: actualName, arguments: JSON.stringify(args) } }] }, 'tool_calls'));
    };
    if ((key === 'VIEWPORT_FIRST' && [2, 3, 5, 6].includes(index)) || (key === 'VIEWPORT_SECOND' && index === 2) || (key === 'VIEWPORT_AUTO' && index === 1) || (key === 'VIEWPORT_CLICK' && [2, 3].includes(index)) || (key === 'VIEWPORT_RETURN' && index === 2)) held.set(`${key}-${index}`, reply);
    else reply();
  });
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
const app = launchApp({
  executable: process.env.MAGI_VIEWPORT_APP_EXECUTABLE || join(root, 'target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi'),
  cwd: root, cdpPort, inspectPort: cdpPort + 1,
  env: { MAGI_STATE_ROOT: work, MAGI_DESKTOP_DAEMON_PORT: String(daemonPort), MAGI_OPEN_BROWSER: '0',
    MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${provider.address().port}/v1`, MAGI_OPENAI_COMPAT_API_KEY: 'viewport-fixture', MAGI_OPENAI_COMPAT_MODEL: 'viewport-model' },
});
let page, guest;
const result = index => results.get(index);
async function pause(key) { await waitFor(() => held.has(key), key, 45000, 200); }
function resume(key) { const reply = held.get(key); assert.ok(reply, key); held.delete(key); reply(); }
try {
  page = await waitFor(async () => {
    const p = await connectAppPage(cdpPort, daemonPort, errors).catch(() => null);
    if (!p) return null;
    if (await p.evaluate('Boolean(document.querySelector("[data-testid=input-textarea]"))')) return p;
    p.close(); return null;
  }, 'desktop ready', 45000, 300);
  const tools = pageTools(page);
  const openViewportToolbar = async () => {
    if (!await tools.exists('.viewport-toolbar')) await tools.click('.viewport-button');
    await waitFor(() => tools.exists('.viewport-toolbar'), 'device toolbar expanded');
    await waitFor(() => guest.evaluate('navigator.maxTouchPoints > 0'), 'mobile input semantics applied');
  };
  async function send(prompt) {
    await waitFor(() => page.evaluate('document.querySelector("[data-testid=input-textarea]")?.getAttribute("contenteditable") === "true"'), 'composer ready');
    await page.evaluate(`(()=>{const e=document.querySelector('[data-testid=input-textarea]');e.textContent=${JSON.stringify(prompt)};e.dispatchEvent(new InputEvent('input',{bubbles:true,inputType:'insertText',data:${JSON.stringify(prompt)}}));})()`);
    await waitFor(() => page.evaluate('Boolean(document.querySelector("[data-testid=input-send-button]:not([disabled])"))'), 'send ready');
    await tools.click('[data-testid=input-send-button]');
  }
  const setViewport = async viewport => {
    const next = await page.evaluate(`window.magiDesktop.setBrowserViewport({tabId:document.querySelector('.browser-surface-slot').dataset.browserTabId,viewport:${JSON.stringify(viewport)}})`);
    if (viewport.mode === 'auto') {
      await waitFor(async () => {
        const rect = await page.evaluate(`(()=>{const r=document.querySelector('.browser-surface-slot').getBoundingClientRect();return {width:Math.round(r.width),height:Math.round(r.height)}})()`);
        const measured = await actual();
        return measured.width === rect.width && measured.height === rect.height;
      }, 'Chromium natural viewport follows rendered auto canvas');
    }
    return next;
  };
  async function setPanelWidth(width) {
    await page.evaluate(`window.magiDesktop.submitLayoutIntent({type:'right_pane_width',width:${width}})`);
    await waitFor(() => page.evaluate(`(()=>{const toolbar=document.querySelector('.browser-toolbar');return toolbar?.clientWidth === ${width} && toolbar.querySelectorAll('.browser-toolbar-actions button').length === ${width >= 480 ? 4 : 0}})()`), `adaptive toolbar ready at ${width}px`);
    await page.evaluate('new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))');
  }
  const cursorVisible = () => page.evaluate(`Boolean(document.querySelector('.right-pane-browser-tab-host.active .agent-cursor-layer:popover-open .agent-cursor'))`);
  const actual = () => guest.evaluate('({width:innerWidth,height:innerHeight,dpr:devicePixelRatio,touch:navigator.maxTouchPoints,key:window.documentKey})');
  const drag = async () => {
    const { x, y } = await page.evaluate(`(()=>{const r=document.querySelector('.desktop-right-pane-resize-handle').getBoundingClientRect();return {x:r.x+4,y:r.y+r.height/2}})()`);
    await page.call('Input.dispatchMouseEvent', { type: 'mouseMoved', x, y });
    await page.call('Input.dispatchMouseEvent', { type: 'mousePressed', button: 'left', buttons: 1, clickCount: 1, x, y });
    await page.call('Input.dispatchMouseEvent', { type: 'mouseMoved', buttons: 1, x: x - 70, y });
    await page.call('Input.dispatchMouseEvent', { type: 'mouseReleased', button: 'left', clickCount: 1, x: x - 70, y });
    await sleep(300);
  };
  await send('VIEWPORT_FIRST');
  await pause('VIEWPORT_FIRST-2');
  const initial = result('VIEWPORT_FIRST-1');
  assert.equal(initial.status, 'succeeded', JSON.stringify(initial));
  assert.deepEqual([initial.actual_viewport.width, initial.actual_viewport.height], [1280, 800]);
  const hidden = await page.evaluate(`!document.querySelector('.desktop-right-pane-column') || document.querySelector('.desktop-right-pane-column').getAttribute('aria-hidden') === 'true'`);
  assert.equal(hidden, true, 'AI must not expand the right pane');
  await tools.click('.header-right-pane-btn');
  await waitFor(() => tools.exists('.right-pane-browser-tab-host webview'), 'browser visible');
  guest = await connectTarget(cdpPort, t => t.url === url, 'viewport fixture guest');
  const documentKey = (await actual()).key;
  await waitFor(cursorVisible, 'agent cursor visible on retained foreground tab');
  await sleep(2000);
  assert.equal(await cursorVisible(), true, 'cursor remains visible while model waits');
  await tools.click('.header-right-pane-btn');
  await waitFor(async () => !(await cursorVisible()), 'collapsed pane hides cursor');
  await tools.click('.header-right-pane-btn');
  await waitFor(cursorVisible, 'reopened pane restores idle cursor');
  assert.equal(await tools.exists('.agent-badge'), false);
  assert.deepEqual([(await actual()).width, (await actual()).height], [1280, 800]);
  assert.equal(await page.evaluate(`document.querySelector('.viewport-button').getAttribute('aria-pressed')`), 'false');
  assert.equal(await tools.exists('.viewport-toolbar'), false, 'desktop has no emulation settings');
  console.log('PASS agent cursor remains visible while idle and restores after pane collapse; no toolbar badge');
  resume('VIEWPORT_FIRST-2'); await pause('VIEWPORT_FIRST-3');
  assert.equal(result('VIEWPORT_FIRST-2').actual_viewport.device_type, 'desktop');
  assert.deepEqual([(await actual()).width, (await actual()).touch], [390, 0]);
  console.log('PASS narrow desktop keeps desktop input semantics');
  resume('VIEWPORT_FIRST-3'); await pause('VIEWPORT_FIRST-5');
  const mobile = result('VIEWPORT_FIRST-3');
  assert.deepEqual([mobile.actual_viewport.width, mobile.actual_viewport.height, mobile.actual_viewport.device_type], [844, 390, 'mobile']);
  assert.equal((await actual()).dpr, 2); assert.ok((await actual()).touch > 0);
  assert.equal((await actual()).key, documentKey, 'viewport changes must not reload');
  assert.equal(result('VIEWPORT_FIRST-4').status, 'succeeded', JSON.stringify(result('VIEWPORT_FIRST-4')));
  await waitFor(() => guest.evaluate('document.querySelector("button").textContent === "Clicked"'), 'AI click reaches page button');
  console.log('PASS landscape mobile, DPR, touch, scaled AI click and no reload');
  await openViewportToolbar();
  assert.match(await tools.text('.viewport-preview-scale'), /\d+%/);
  const viewportToolbar = await page.evaluate(`(()=>{const menu=document.querySelector('.viewport-toolbar');const box=menu.getBoundingClientRect();return {width:box.width,height:box.height,scrollWidth:menu.scrollWidth,scrollHeight:menu.scrollHeight}})()`);
  assert.ok(viewportToolbar.width >= 320 && viewportToolbar.height <= 80, JSON.stringify(viewportToolbar));
  assert.equal(viewportToolbar.width, viewportToolbar.scrollWidth, JSON.stringify(viewportToolbar));
  await writeFile(join(tmpdir(), 'magi-viewport-mobile.png'), Buffer.from(await tools.screenshot(), 'base64'));
  await page.evaluate(`(()=>{const e=document.querySelector('.viewport-custom input');e.focus();e.value='700';e.dispatchEvent(new Event('input',{bubbles:true}));e.blur()})()`);
  await waitFor(() => guest.evaluate('innerWidth === 700'), 'custom width applied');
  assert.equal((await actual()).dpr, 2); assert.ok((await actual()).touch > 0);
  assert.equal(await page.evaluate(`document.querySelectorAll('.viewport-toolbar select').length`), 1, 'mobile has a single phone-size selector');
  await tools.click('.viewport-rotate');
  await waitFor(() => guest.evaluate('innerWidth === 390 && innerHeight === 700'), 'rotate swaps dimensions and keeps mobile emulation');
  assert.equal((await actual()).dpr, 2); assert.ok((await actual()).touch > 0);
  await tools.click('.viewport-rotate');
  await waitFor(() => guest.evaluate('innerWidth === 700 && innerHeight === 390'), 'rotate returns to landscape');
  await page.evaluate(`document.querySelector('.viewport-custom input').focus()`);
  for (const event of [{type:'keyDown',key:'ArrowUp',code:'ArrowUp'},{type:'keyUp',key:'ArrowUp',code:'ArrowUp'}]) await page.call('Input.dispatchKeyEvent',event);
  await waitFor(() => guest.evaluate('innerWidth === 701'), 'arrow key increases width by one');
  for (const event of [{type:'keyDown',key:'ArrowDown',code:'ArrowDown',modifiers:8},{type:'keyUp',key:'ArrowDown',code:'ArrowDown',modifiers:8}]) await page.call('Input.dispatchKeyEvent',event);
  await waitFor(() => guest.evaluate('innerWidth === 691'), 'shift arrow decreases width by ten');
  await page.evaluate(`(()=>{const e=document.querySelector('.viewport-custom input');e.value='invalid';e.dispatchEvent(new Event('input',{bubbles:true}))})()`);
  await waitFor(() => tools.exists('.viewport-custom input[aria-invalid="true"]'), 'invalid dimensions show validation');
  assert.equal((await actual()).width, 691);
  await page.call('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter'});
  assert.equal(await tools.exists('.viewport-input-error'), true, 'invalid Enter leaves actionable validation');
  await page.call('Input.dispatchKeyEvent',{type:'keyDown',key:'Escape',code:'Escape'});
  assert.equal(await tools.exists('.viewport-toolbar'), true, 'input Escape restores size without closing toolbar');
  assert.equal(await page.evaluate(`document.querySelector('.viewport-custom input').value`), '691');
  await page.evaluate(`(()=>{const e=document.querySelector('.viewport-custom input');e.value='700';e.dispatchEvent(new Event('input',{bubbles:true}))})()`);
  await page.call('Input.dispatchKeyEvent',{type:'keyDown',key:'Enter',code:'Enter'});
  await waitFor(() => guest.evaluate('innerWidth === 700'), 'Enter commits valid dimensions');
  await tools.click('.address-form input');
  assert.equal(await tools.exists('.viewport-toolbar'), true, 'outside interaction keeps device controls available');
  console.log('PASS device toolbar: rotate preserves DPR/touch, arrow and Shift steps, invalid validation, Enter commit, Escape reverts pending input, outside focus keeps it open');
  await waitFor(() => tools.exists('.hand-back-button'), 'compact hand-back control');
  assert.equal(await cursorVisible(), false, 'explicit user viewport hides agent cursor');
  console.log('PASS custom dimensions preserve device/DPR and take control from AI');
  resume('VIEWPORT_FIRST-5'); await pause('VIEWPORT_FIRST-6');
  assert.notEqual(result('VIEWPORT_FIRST-5').status, 'succeeded', JSON.stringify(result('VIEWPORT_FIRST-5')));
  assert.equal((await actual()).width, 700, 'AI must not resize after user takeover');
  resume('VIEWPORT_FIRST-6');
  await waitFor(() => page.evaluate(`document.body.innerText.includes('VIEWPORT_FIRST_DONE')`), 'first turn finished');
  await tools.click('.hand-back-button');
  await setViewport({ mode: 'auto' });
  const autoBefore = await actual();
  await send('VIEWPORT_SECOND'); await pause('VIEWPORT_SECOND-2');
  const acquired = result('VIEWPORT_SECOND-1');
  assert.equal(acquired.mode, 'fixed');
  assert.deepEqual([acquired.actual_viewport.width, acquired.actual_viewport.height], [autoBefore.width, autoBefore.height]);
  await waitFor(cursorVisible, 'hand-back and next agent tool restores cursor');
  await writeFile(join(tmpdir(), 'magi-intervention-agent.png'), Buffer.from(await tools.screenshot(), 'base64'));
  await drag();
  await waitFor(async () => (await page.evaluate('window.magiDesktop.getSnapshot()')).activeBrowserViewport?.mode === 'auto', 'user pane drag restores responsive viewport');
  await waitFor(() => tools.exists('.hand-back-button'), 'drag takes user control');
  assert.equal(await cursorVisible(), false);
  assert.notEqual((await actual()).width, autoBefore.width);
  assert.equal((await actual()).touch, 0);
  console.log('PASS hand-back restores agent cursor; manual pane drag takes control and follows panel');
  resume('VIEWPORT_SECOND-2');
  await waitFor(() => page.evaluate(`document.body.innerText.includes('VIEWPORT_SECOND_DONE')`), 'second turn finished');
  await tools.click('.hand-back-button');
  await send('VIEWPORT_CLICK'); await pause('VIEWPORT_CLICK-2');
  await waitFor(cursorVisible, 'cursor survives navigation');
  guest.close();
  guest = await connectTarget(cdpPort, t => t.url === url + '?return', 'navigated guest');
  const screenPoint = await page.evaluate(`(()=>{const slot=document.querySelector('.browser-viewport').getBoundingClientRect();const layer=document.querySelector('.agent-cursor-layer:popover-open').getBoundingClientRect();return {slot:{x:slot.x,y:slot.y,w:slot.width,h:slot.height},layer:{x:layer.x,y:layer.y,w:layer.width,h:layer.height}}})()`);
  assert.deepEqual(screenPoint.layer, screenPoint.slot, 'cursor layer matches guest display slot');
  // 直接向 guest 注入真实输入，绕开 Host 自动化标记，模拟用户点击。
  await guest.call('Input.dispatchMouseEvent', {type:'mousePressed',x:30,y:30,button:'left',buttons:1,clickCount:1});
  await guest.call('Input.dispatchMouseEvent', {type:'mouseReleased',x:30,y:30,button:'left',clickCount:1});
  await waitFor(() => tools.exists('.hand-back-button'), 'page click takes control');
  await waitFor(async () => (await page.evaluate('window.magiDesktop.getSnapshot()')).activeBrowserViewport?.mode === 'auto', 'page click follows panel');
  assert.equal(await cursorVisible(), false);
  await waitFor(() => guest.evaluate('document.querySelector("button").textContent === "Clicked"'), 'user click reaches page button');
  resume('VIEWPORT_CLICK-2'); await pause('VIEWPORT_CLICK-3');
  assert.notEqual(result('VIEWPORT_CLICK-2').status, 'succeeded', 'agent cannot reclaim before hand-back');
  resume('VIEWPORT_CLICK-3');
  await waitFor(() => page.evaluate(`document.body.innerText.includes('VIEWPORT_CLICK_DONE')`), 'click turn finished');
  await setPanelWidth(320);
  await tools.click('.browser-actions-button');
  await waitFor(() => tools.exists('.browser-actions-popover:popover-open'), 'more menu opens in narrow panel');
  const compact = await page.evaluate(`(()=>{const toolbar=document.querySelector('.browser-toolbar');const box=toolbar.getBoundingClientRect();const input=toolbar.querySelector('input').getBoundingClientRect();const menu=document.querySelector('.browser-actions-popover').getBoundingClientRect();return {width:toolbar.clientWidth,scroll:toolbar.scrollWidth,input:input.width,menuLeft:menu.left,menuRight:menu.right,left:box.left,right:box.right,buttons:[...toolbar.querySelectorAll('button')].filter(e=>e.getBoundingClientRect().width>0).map(e=>({label:e.getAttribute('aria-label'),x:e.getBoundingClientRect().x,right:e.getBoundingClientRect().right}))}})()`);
  assert.ok(compact.scroll <= compact.width, JSON.stringify(compact));
  assert.ok(compact.input >= 100, JSON.stringify(compact));
  assert.ok(compact.menuLeft >= compact.left && compact.menuRight <= compact.right, JSON.stringify(compact));
  assert.ok(compact.buttons.every(b=>b.x>=compact.left && b.right<=compact.right), JSON.stringify(compact));
  assert.equal(await tools.exists('.agent-badge'), false);
  await writeFile(join(tmpdir(), 'magi-intervention-narrow.png'), Buffer.from(await tools.screenshot(), 'base64'));
  await tools.click('.browser-actions-button');
  await tools.click('.hand-back-button');
  await send('VIEWPORT_RETURN'); await pause('VIEWPORT_RETURN-2');
  await waitFor(cursorVisible, 'cursor returns at current narrow size');
  const returned = result('VIEWPORT_RETURN-1');
  assert.ok(returned.actual_viewport.width <= 320);
  assert.equal(returned.mode, 'fixed');
  await writeFile(join(tmpdir(), 'magi-intervention-returned.png'), Buffer.from(await tools.screenshot(), 'base64'));
  resume('VIEWPORT_RETURN-2');
  await waitFor(() => page.evaluate(`document.body.innerText.includes('VIEWPORT_RETURN_DONE')`), 'return turn finished');
  console.log('PASS page click hides cursor and restores responsive viewport; held user control blocks AI; 320px toolbar and menu fit; hand-back uses current size');
  await setViewport({ mode: 'auto' });
  const following = await actual(); await drag();
  await waitFor(async () => (await actual()).width !== following.width, 'follow panel reflows');
  const autoCurrent = await actual();
  await send('VIEWPORT_AUTO'); await pause('VIEWPORT_AUTO-1');
  const autoResult = result('VIEWPORT_AUTO-0');
  assert.equal(autoResult.mode, 'auto');assert.equal(autoResult.viewport, null);
  assert.deepEqual([autoResult.actual_viewport.width, autoResult.actual_viewport.height], [autoCurrent.width, autoCurrent.height]);
  resume('VIEWPORT_AUTO-1');
  await waitFor(() => page.evaluate(`document.body.innerText.includes('VIEWPORT_AUTO_DONE')`), 'read-only turn finished');
  console.log('PASS auto viewport tool returns actual Chromium dimensions');
  await setPanelWidth(320);
  await setViewport({mode:'fixed',width:1280,height:800,deviceScaleFactorMillis:1000,deviceType:'desktop'});
  const overflow = await page.evaluate(`(()=>{const e=document.querySelector('.browser-toolbar');return {width:e.clientWidth,scroll:e.scrollWidth}})()`);
  assert.ok(overflow.scroll <= overflow.width, JSON.stringify(overflow));
  console.log('PASS compact 320px toolbar remains inside panel');
  await openViewportToolbar();
  const mobileStart = await actual();
  assert.equal(mobileStart.touch > 0, true);
  assert.equal(mobileStart.width, 320, 'mobile starts at the current panel width instead of a phone preset');
  const selectPreset = async name => {
    await page.evaluate(`(()=>{const e=document.querySelector('.viewport-preset');e.value=${JSON.stringify(name)};e.dispatchEvent(new Event('change',{bubbles:true}))})()`);
  };
  assert.equal(await page.evaluate(`document.querySelector('.viewport-preset').value`), '', 'entering mobile keeps current dimensions as custom');
  for (const [name, width, height, dpr] of [
    ['iPhone SE', 375, 667, 2], ['iPhone 12 / 13', 390, 844, 3],
    ['iPhone 16', 393, 852, 3], ['iPhone 16 Pro Max', 440, 956, 3], ['Pixel 7', 412, 915, 2.625],
  ]) {
    await selectPreset(name);
    await waitFor(async () => {
      const v = await actual();
      return v.width === width && v.height === height && v.dpr === dpr && v.touch > 0;
    }, name + ' applies dimensions, DPR and mobile semantics to real Chromium');
    assert.equal((await actual()).key, mobileStart.key, 'preset does not reload the page');
    await waitFor(() => page.evaluate(`document.querySelector('.viewport-preset').value === ${JSON.stringify(name)} && !document.querySelector('.viewport-rotate').disabled`), 'preset selection synchronized');
    await tools.click('.viewport-rotate');
    await waitFor(() => guest.evaluate(`innerWidth === ${height} && innerHeight === ${width} && devicePixelRatio === ${dpr}`), 'rotation swaps real dimensions and preserves DPR');
    assert.equal(await page.evaluate(`document.querySelector('.viewport-preset').value`), name, 'landscape retains device label');
  }
  await page.evaluate(`(()=>{const e=document.querySelector('.viewport-custom input');e.focus();e.value='430';e.dispatchEvent(new Event('input',{bubbles:true}));e.blur()})()`);
  await waitFor(() => guest.evaluate('innerWidth === 430 && devicePixelRatio === 2.625'), 'custom dimensions retain selected device DPR');
  assert.equal(await page.evaluate(`document.querySelector('.viewport-preset').value`), '', 'manual resize shows custom size');
  await selectPreset('iPhone 16');
  await waitFor(() => guest.evaluate('innerWidth === 393 && innerHeight === 852 && devicePixelRatio === 3'), 'choosing preset resets custom dimensions to portrait');
  console.log('PASS five phone presets apply real CSS dimensions/DPR without reloading; rotation retains device; custom dimensions clear device label');
  for (const width of [320, 480, 600, 800]) {
    await setPanelWidth(width);
    const metrics = await page.evaluate(`(()=>{const menu=document.querySelector('.viewport-toolbar');const box=menu.getBoundingClientRect();const toolbar=document.querySelector('.browser-toolbar').getBoundingClientRect();return {width:box.width,height:box.height,left:box.left,right:box.right,panelLeft:toolbar.left,panelRight:toolbar.right,controls:[...menu.querySelectorAll('select,input,button')].map(e=>{const r=e.getBoundingClientRect();return {x:r.left,right:r.right,width:r.width}})}})()`);
    assert.ok(metrics.width === width && metrics.height <= 42, JSON.stringify(metrics));
    assert.ok(metrics.left >= metrics.panelLeft && metrics.right <= metrics.panelRight, JSON.stringify(metrics));
    assert.ok(metrics.controls.every(c => c.width >= 27 && c.x >= metrics.left && c.right <= metrics.right), JSON.stringify(metrics));
    await writeFile(join(tmpdir(), `magi-compact-viewport-${width}.png`), Buffer.from(await tools.screenshot(), 'base64'));
  }
  await setPanelWidth(480);
  await tools.click('.viewport-button');
  await waitFor(async () => (await page.evaluate('window.magiDesktop.getSnapshot()')).activeBrowserViewport?.mode === 'auto', 'device icon returns to desktop auto');
  await waitFor(() => guest.evaluate('innerWidth === 480 && navigator.maxTouchPoints === 0'), 'desktop follows current panel width');
  assert.equal(await tools.exists('.viewport-toolbar'), false, 'desktop auto hides emulation controls');
  console.log('PASS single device toggle, mobile presets stay on one line at 320/480/600/800px; desktop restores auto');
  await writeFile(join(tmpdir(), 'magi-viewport-acceptance.png'), Buffer.from(await tools.screenshot(), 'base64'));
  const actionLabels = [];
  for (const width of [640, 520, 480, 479, 320]) {
    await setPanelWidth(width);
    const layout = await page.evaluate(`(()=>{const toolbar=document.querySelector('.browser-toolbar');const r=toolbar.getBoundingClientRect();return {width:toolbar.clientWidth,scroll:toolbar.scrollWidth,input:toolbar.querySelector('input').getBoundingClientRect().width,direct:[...toolbar.querySelectorAll('.browser-toolbar-actions button')].map(e=>({label:e.getAttribute('aria-label'),left:e.getBoundingClientRect().left,right:e.getBoundingClientRect().right})),left:r.left,right:r.right,more:!!toolbar.querySelector('.browser-actions-button')}})()`);
    assert.ok(layout.scroll <= layout.width && layout.input >= 100, JSON.stringify(layout));
    assert.ok(layout.direct.every(b => b.left >= layout.left && b.right <= layout.right), JSON.stringify(layout));
    assert.equal(layout.more, width < 480, JSON.stringify(layout));
    if (width === 640) actionLabels.push(...layout.direct.map(b => b.label));
    if (width < 480) {
      await tools.click('.browser-actions-button');
      await waitFor(() => tools.exists('.browser-actions-popover:popover-open'), 'overflow visible');
      assert.deepEqual(await page.evaluate(`[...document.querySelectorAll('.browser-actions-menu > button')].map(e=>e.getAttribute('aria-label'))`), actionLabels);
      const menuLayout = await page.evaluate(`(()=>{const toolbar=document.querySelector('.browser-toolbar').getBoundingClientRect();const menu=document.querySelector('.browser-actions-popover').getBoundingClientRect();return {width:menu.width,height:menu.height,left:menu.left,right:menu.right,bottom:menu.bottom,top:menu.top,panelLeft:toolbar.left,panelRight:toolbar.right,toolbarBottom:toolbar.bottom,windowHeight:innerHeight,rows:[...document.querySelectorAll('.browser-actions-menu > button')].map(e=>{const r=e.getBoundingClientRect();const icon=e.querySelector('svg').getBoundingClientRect();const label=e.querySelector('span').getBoundingClientRect();return {height:r.height,iconLeft:icon.left,labelLeft:label.left,labelRight:label.right,overflow:e.scrollWidth>e.clientWidth,align:getComputedStyle(e).textAlign}})}})()`);
      assert.ok(menuLayout.width >= 200 && menuLayout.width <= 280, JSON.stringify(menuLayout));
      assert.ok(menuLayout.left >= menuLayout.panelLeft && menuLayout.bottom <= menuLayout.windowHeight, JSON.stringify(menuLayout));
      assert.equal(menuLayout.right, menuLayout.panelRight - 6, JSON.stringify(menuLayout));
      assert.equal(menuLayout.top, menuLayout.toolbarBottom + 4, JSON.stringify(menuLayout));
      assert.ok(menuLayout.rows.every(row => row.height >= 32 && row.align === 'left' && !row.overflow && row.iconLeft === menuLayout.rows[0].iconLeft && row.labelLeft === menuLayout.rows[0].labelLeft && row.labelRight <= menuLayout.right - 8), JSON.stringify(menuLayout));
      await setPanelWidth(640);
      await waitFor(async () => !(await tools.exists('.browser-actions-popover:popover-open')), 'expanding closes overflow');
    }
  }
  await page.evaluate(`window.__toolbarCaptureListener = event => {window.__toolbarCapture = {size:event.detail.size,png:event.detail.dataUrl.startsWith('data:image/png;base64,iVBOR')}};window.addEventListener('magi:browserScreenshotCaptured',window.__toolbarCaptureListener)`);
  for (const width of [320, 640]) {
    await setPanelWidth(width);
    await page.evaluate('window.__toolbarCapture=null');
    if (width < 480) {
      await tools.click('.browser-actions-button');
      await waitFor(() => tools.exists('.browser-actions-popover:popover-open'), 'capture menu visible');
    }
    await writeFile(join(tmpdir(), `magi-adaptive-toolbar-${width}-before.png`), Buffer.from(await tools.screenshot(), 'base64'));
    const button = await page.evaluate(`(()=>{const buttons=document.querySelectorAll(${JSON.stringify(width < 480 ? '.browser-actions-menu > button' : '.browser-toolbar-actions > button')});const r=buttons[1].getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()`);
    await page.call('Input.dispatchMouseEvent', {type:'mouseMoved',...button});
    await page.call('Input.dispatchMouseEvent', {type:'mousePressed',button:'left',buttons:1,clickCount:1,...button});
    await page.call('Input.dispatchMouseEvent', {type:'mouseReleased',button:'left',clickCount:1,...button});
    const captured = await waitFor(() => page.evaluate('window.__toolbarCapture'), `real screenshot at ${width}px`);
    assert.ok(captured.png && captured.size > 0, JSON.stringify(captured));
    assert.equal(await tools.exists('.browser-actions-popover:popover-open'), false);
    await writeFile(join(tmpdir(), `magi-adaptive-toolbar-${width}.png`), Buffer.from(await tools.screenshot(), 'base64'));
  }
  await page.evaluate(`window.removeEventListener('magi:browserScreenshotCaptured',window.__toolbarCaptureListener);delete window.__toolbarCaptureListener;delete window.__toolbarCapture`);
  console.log('PASS adaptive toolbar at 320/479/480/520/640px; resize closes menu; physical clicks capture real PNGs from both layouts');
  // 固定设备不只修改逻辑指标：真实 guest 和底部命中区域都必须匹配画布。
  await setPanelWidth(640);
  await openViewportToolbar();
  await waitFor(() => guest.evaluate('navigator.maxTouchPoints > 0'), 'mobile emulation ready before custom editing');
  for (const dimensions of [{width:390,height:300},{width:390,height:844},{width:800,height:600}]) {
    await page.evaluate(`(()=>{const fields=document.querySelectorAll('.viewport-custom input');for(const [index,value] of [${dimensions.width},${dimensions.height}].entries()){fields[index].focus();fields[index].value=String(value);fields[index].dispatchEvent(new Event('input',{bubbles:true}));fields[index].blur()}})()`);
    await waitFor(() => guest.evaluate(`innerWidth === ${dimensions.width} && innerHeight === ${dimensions.height}`), 'custom dimensions applied to real mobile guest');
    await waitFor(() => page.evaluate(`(()=>{const s=window.magiDesktop;const canvas=document.querySelector('.browser-viewport').getBoundingClientRect();return s.getSnapshot().then(snapshot=>{const surface=snapshot.browserSurfaces.find(s=>s.tabId===document.querySelector('.browser-surface-slot').dataset.browserTabId);return Math.abs(canvas.width-surface.displayViewport.width)<1 && Math.abs(canvas.height-surface.displayViewport.height)<1})})()`), 'fixed canvas matches projected display size');
    const box = await page.evaluate(`(()=>{const slot=document.querySelector('.browser-surface-slot').getBoundingClientRect();const canvas=document.querySelector('.browser-viewport').getBoundingClientRect();const guest=document.querySelector('webview').getBoundingClientRect();return {slot:{left:slot.left,top:slot.top,width:slot.width,height:slot.height},canvas:{left:canvas.left,top:canvas.top,width:canvas.width,height:canvas.height},guest:{left:guest.left,top:guest.top,width:guest.width,height:guest.height}}})()`);
    assert.deepEqual(box.guest, box.canvas, JSON.stringify(box));
    assert.ok(Math.abs((box.canvas.left-box.slot.left)*2+box.canvas.width-box.slot.width)<1, JSON.stringify(box));
    assert.equal(box.canvas.top, box.slot.top);
    assert.ok(box.canvas.height <= box.slot.height && box.canvas.width <= box.slot.width, JSON.stringify(box));
    assert.ok(Math.abs(box.canvas.width/box.canvas.height-dimensions.width/dimensions.height)<.01, JSON.stringify(box));
    assert.ok((await actual()).touch > 0, 'custom width and height preserve mobile mode');
    const bottom = await guest.evaluate('document.querySelector("footer").getBoundingClientRect().bottom');
    assert.equal(bottom, dimensions.height);
    await writeFile(join(tmpdir(), `magi-fixed-canvas-${dimensions.width}x${dimensions.height}.png`), Buffer.from(await tools.screenshot(), 'base64'));
  }
  await page.call('Input.dispatchKeyEvent', {type:'keyDown',key:'Escape',code:'Escape'});
  await page.call('Input.dispatchKeyEvent', {type:'keyUp',key:'Escape',code:'Escape'});
  await tools.click('.viewport-button');
  const main = await connectTarget(cdpPort + 1, target => target.type === 'node', 'isolated acceptance Main');
  await main.evaluate(`process.getBuiltinModule('module')._load('electron').BrowserWindow.getAllWindows()[0].setContentSize(1800,1000)`);
  main.close();
  await setPanelWidth(1000);
  await setViewport({mode:'fixed',width:800,height:600,deviceScaleFactorMillis:1000,deviceType:'mobile'});
  await waitFor(() => page.evaluate(`(()=>{const slot=document.querySelector('.browser-surface-slot').getBoundingClientRect();const guest=document.querySelector('webview').getBoundingClientRect();const canvas=document.querySelector('.viewport-resize-frame');const style=getComputedStyle(canvas);return guest.width===800 && guest.height===600 && Math.abs(guest.left-slot.left-100)<1 && style.outlineStyle==='solid' && parseFloat(style.outlineWidth)>=1})()`), '1000px pane centers 800px viewport and frames its real boundary');
  await writeFile(join(tmpdir(), 'magi-fixed-canvas-1000-pane-800-viewport.png'), Buffer.from(await tools.screenshot(), 'base64'));
  const footerPoint = await page.evaluate(`(()=>{const r=document.querySelector('.browser-viewport').getBoundingClientRect();return {x:r.left+r.width/2,y:r.bottom-5}})()`);
  await page.call('Input.dispatchMouseEvent', {type:'mousePressed',button:'left',buttons:1,clickCount:1,...footerPoint});
  await page.call('Input.dispatchMouseEvent', {type:'mouseReleased',button:'left',clickCount:1,...footerPoint});
  await waitFor(() => guest.evaluate('document.querySelector("footer").dataset.clicked === "1"'), 'physical click hits bottom of centered mobile viewport');
  // 通过真实 Host 指针拖动，覆盖跨入 guest、实时提交和手机语义保持。
  await waitFor(() => tools.exists('.viewport-resize-frame:popover-open .viewport-resize-handle--corner'), 'viewport resize corner is visible');
  const corner = await page.evaluate(`(()=>{const r=document.querySelector('.viewport-resize-handle--corner').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()`);
  await page.call('Input.dispatchMouseEvent', {type:'mouseMoved',...corner});
  await page.call('Input.dispatchMouseEvent', {type:'mousePressed',button:'left',buttons:1,clickCount:1,...corner});
  await page.call('Input.dispatchMouseEvent', {type:'mouseMoved',buttons:1,x:corner.x-20,y:corner.y+30});
  await waitFor(() => guest.evaluate('innerWidth === 760 && innerHeight === 630'), 'drag updates real viewport before pointer release');
  await page.call('Input.dispatchMouseEvent', {type:'mouseReleased',button:'left',clickCount:1,x:corner.x-20,y:corner.y+30});
  assert.ok((await actual()).touch > 0, 'drag preserves mobile device mode');
  assert.equal((await actual()).dpr, 1, 'drag preserves DPR');
  await writeFile(join(tmpdir(), 'magi-viewport-border-drag.png'), Buffer.from(await tools.screenshot(), 'base64'));
  await page.evaluate(`document.querySelector('.viewport-resize-handle--bottom').focus()`);
  await page.call('Input.dispatchKeyEvent', {type:'keyDown',key:'ArrowDown',code:'ArrowDown'});
  await page.call('Input.dispatchKeyEvent', {type:'keyUp',key:'ArrowDown',code:'ArrowDown'});
  await waitFor(() => guest.evaluate('innerHeight === 640 && innerWidth === 760'), 'keyboard resizes height without changing width');
  await setViewport({mode:'auto'});
  await waitFor(() => page.evaluate(`(()=>{const slot=document.querySelector('.browser-surface-slot').getBoundingClientRect();const canvas=document.querySelector('.browser-viewport').getBoundingClientRect();return slot.width===canvas.width && slot.height===canvas.height && slot.left===canvas.left})()`), 'auto returns to full slot');
  await waitFor(async () => !(await tools.exists('.viewport-resize-frame:popover-open')), 'auto viewport hides resize frame');
  console.log('PASS border drag resizes real guest live; pointer capture crosses guest; device/DPR preserved; keyboard and auto work');
  await openViewportToolbar();
  await waitFor(() => guest.evaluate('navigator.maxTouchPoints > 0'), 'mobile preview for final UX acceptance');
  await setPanelWidth(640);
  for (const mode of ['dark', 'light']) {
    const bootstrap = await tools.api('/api/appearance/bootstrap');
    assert.equal(bootstrap.status, 200, bootstrap.body);
    const activation = await tools.post('/api/appearance/activate', {themeId:'builtin.' + mode, expectedRevision:JSON.parse(bootstrap.body).revision});
    assert.equal(activation.status, 200, activation.body);
    await page.evaluate(`window.dispatchEvent(new CustomEvent('magi:appearanceChanged'))`);
    await waitFor(() => page.evaluate(`document.documentElement.classList.contains('theme-${mode}')`), mode + ' theme applied through appearance API');
    const colors = await page.evaluate(`(()=>{const e=document.querySelector('.viewport-toolbar');const s=getComputedStyle(e);const i=getComputedStyle(e.querySelector('input'));return {background:s.backgroundColor,color:s.color,inputBackground:i.backgroundColor,inputColor:i.color}})()`);
    assert.notEqual(colors.color,colors.background);
    assert.notEqual(colors.inputColor,colors.inputBackground);
    await writeFile(join(tmpdir(), 'magi-device-toolbar-' + mode + '.png'), Buffer.from(await tools.screenshot(), 'base64'));
  }
  const beforeReloadKey = await guest.evaluate('window.documentKey');
  await tools.click('.browser-toolbar button[aria-label="刷新"]');
  await waitFor(async () => (await guest.evaluate('window.documentKey')) !== beforeReloadKey, 'guest reload completed');
  assert.equal(await tools.exists('.viewport-toolbar'), true, 'navigation keeps the device toolbar open');
  await waitFor(() => tools.exists('.viewport-button:not([disabled])'), 'device toggle ready after reload');
  await tools.click('.viewport-button');
  await waitFor(async () => !(await tools.exists('.viewport-toolbar')), 'device toggle hides mobile controls');
  await waitFor(() => guest.evaluate('innerWidth === 640 && navigator.maxTouchPoints === 0'), 'turning emulation off returns to desktop auto');
  console.log('PASS dark/light themes through real appearance API; reload keeps controls; device toggle restores desktop auto');
  assert.deepEqual(errors, [], 'Renderer has no runtime errors');
  console.log('Renderer diagnostics:', JSON.stringify(errors));
} catch (error) {
  if (page) {
    await writeFile(join(tmpdir(), 'magi-viewport-failure.png'), Buffer.from(await pageTools(page).screenshot(), 'base64')).catch(() => {});
    console.error('UI:', await page.evaluate(`({frame:document.querySelector('.viewport-resize-frame')?.outerHTML,toolbar:document.querySelector('.browser-toolbar')?.outerHTML,menu:document.querySelector('.browser-actions-popover')?.outerHTML,error:document.querySelector('.browser-action-error')?.textContent})`).catch(() => null));
  }
  await writeFile(join(tmpdir(), 'magi-viewport-provider-requests.json'), JSON.stringify(requests, null, 2));
  console.error('Results:', JSON.stringify([...results]));
  console.error(app.log.slice(-8).join(''));
  throw error;
} finally {
  guest?.close(); page?.close(); await app.stop();
  for (const reply of held.values()) { try { reply(); } catch {} }
  fixture.closeAllConnections(); fixture.close(); provider.closeAllConnections(); provider.close();
  await rm(work, { recursive: true, force: true });
}
