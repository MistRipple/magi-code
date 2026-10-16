// Production Main + Worker + actual Svelte overlay + real Chromium guest. No account or daemon data.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { app, BrowserWindow, ipcMain } from 'electron';
import { BrowserSurfaceManager, type BrowserSurfaceEvent } from '../apps/desktop/src/main/browser-surface-manager.js';
import { BrowserAutomationRuntime } from '../browser-automation-worker/src/runtime.js';
import { CdpClient } from '../browser-automation-worker/src/cdp-client.js';
import { assertDesktopIpcMessage } from '../contracts/desktop-browser/src/validation.js';
import type { BrowserAgentCursorPresentationResult, BrowserControl, BrowserSurfaceBinding } from '../contracts/desktop-browser/src/index.js';

const root = process.env.MAGI_CURSOR_TEST_ROOT!;
app.setPath('userData', join(root, 'data'));
app.whenReady().then(async () => {
  const renderer = await readFile(join(root, 'renderer.js'), 'utf8');
  const fixture = '<!doctype html><style>body{margin:0;min-height:600px}button,input{position:absolute;left:700px;width:120px;height:36px}button{top:70px}input{top:180px}</style><button id="target">Target</button><input id="text" aria-label="Text"><script>window.events=[]; for(const t of ["mousedown","click","focusin","input"])document.addEventListener(t,e=>events.push({type:t,target:e.target.id,time:performance.timeOrigin+performance.now()}),true)</script>';
  const server = createServer((req, res) => {
    res.setHeader('content-type', req.url === '/renderer.js' ? 'text/javascript' : 'text/html');
    res.end(req.url === '/renderer.js' ? renderer : req.url === '/host'
      ? `<!doctype html><style>body{margin:0;--primary:#2563eb}.slot{width:1000px;height:600px;anchor-name:--test-guest}webview{width:100%;height:100%}</style><div class="slot"><webview partition="magi-browser-cursor" src="${origin}/page"></webview></div><div id="overlay"></div><script src="/renderer.js"></script>` : fixture);
  });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `http://127.0.0.1:${(server.address() as { port: number }).port}`;
  const win = new BrowserWindow({ width: 1050, height: 760, show: true, webPreferences: { webviewTag: true, sandbox: true, contextIsolation: true, preload: join(root, 'preload.cjs') } });
  let receive: (event: { data: unknown }) => void;
  let mode: 'normal' | 'hold' = 'normal';
  let lastMotion: Extract<BrowserSurfaceEvent, { type: 'agent_cursor' }> | null = null;
  const acks: Array<BrowserAgentCursorPresentationResult & { time: number }> = [];
  const surfaces = new BrowserSurfaceManager({ desktopEpoch: 'cursor-test', onEvent(event) {
    if (event.type === 'cdp_event') receive?.({ data: { type: 'cdp_event', binding: event.binding, method: event.method, params: event.params, ...(event.sessionId ? { session_id: event.sessionId } : {}) } });
    if (event.type === 'agent_cursor') {
      if (event.presentation) lastMotion = event;
      if (mode === 'normal' || !event.presentation) win.webContents.send('cursor-event', event);
    }
  } });
  ipcMain.handle('magi-desktop:complete-browser-agent-cursor', (event, result) => {
    assert.equal(event.sender.id, win.webContents.id);
    assertDesktopIpcMessage('magi-desktop:complete-browser-agent-cursor', result);
    acks.push({ ...result, time: Date.now() });
    surfaces.completeAgentCursorPresentation('window', result);
  });
  let control: BrowserControl = { mode: 'agent', lease_id: 'cursor-lease', fence: 1 };
  let abort: AbortController | null = null;
  const runtime = new BrowserAutomationRuntime(new CdpClient({
    on(_event, listener) { receive = listener; },
    async postMessage(message) {
      if (message.type !== 'cdp_request') return;
      const reply = { type: 'cdp_response', call_id: message.call_id, request_id: message.request_id, binding: message.binding };
      try {
        const result = await surfaces.sendCdp(message.binding, message.method, message.params, message.session_id, {
          ...(control.mode === 'agent' ? { expectedAgentControl: control } : {}), ...(abort ? { signal: abort.signal } : {}),
        });
        receive({ data: { ...reply, result } });
      } catch (error) { receive({ data: { ...reply, error: { code: 'browser_cdp_error', message: String(error), recoverable: false, side_effect_started: false, diagnostic: null } } }); }
    },
  }));
  surfaces.attachWindow('window', win.webContents);
  await surfaces.materialize({ windowId: 'window', tabId: 'tab', browserSessionId: 'cursor', initialUrl: origin + '/page', navigationRevision: 0, viewport: { mode: 'auto' } });
  const attached = new Promise<Electron.WebContents>(resolve => win.webContents.once('did-attach-webview', (_event, guest) => resolve(guest)));
  await win.loadURL(origin + '/host');
  const guest = await attached;
  if (guest.isLoadingMainFrame()) await new Promise<void>(resolve => guest.once('did-finish-load', () => resolve()));
  surfaces.registerEmbeddedWebview('window', { tabId: 'tab', browserSessionId: 'cursor', navigationRevision: 0, webContentsId: guest.id, displaySize: { width: 1000, height: 600 } });
  const until = async (predicate: () => Promise<unknown> | unknown) => { const end = Date.now() + 6000; while (!await predicate()) { if (Date.now() > end) throw Error('condition timeout'); await delay(15); } };
  await until(() => win.webContents.executeJavaScript('window.__cursorReady'));
  win.webContents.debugger.attach('1.3');
  await win.webContents.debugger.sendCommand('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }] });
  let calls = 0;
  const binding = (): BrowserSurfaceBinding => surfaces.primaryBindingForTab('tab')!;
  const acquire = async () => {
    control = { mode: 'agent', lease_id: 'cursor-lease-' + ++calls, fence: calls };
    await surfaces.updateControl('tab', binding().surface_id, control);
    await delay(40);
  };
  const run = async (type: string, payload: object) => {
    runtime.rebind([binding()]);
    return (await runtime.execute('cursor-call-' + ++calls, binding(), { type, payload: { tab_id: 'tab', control, ...payload } } as any)).outcome;
  };
  const click = (x: number, y: number) => run('devtools', { operation: 'click_at', arguments: { x, y } });
  const ok = (outcome: any) => assert.equal(outcome.status, 'succeeded', JSON.stringify(outcome));
  await acquire();
  const started = Date.now();
  const first = click(760, 88);
  await until(() => lastMotion !== null);
  await delay(60);
  assert.equal(await guest.executeJavaScript('events.some(e=>e.type==="mousedown")'), false);
  // Actual computed transform must still be between the old and target positions while no DOM click has fired.
  const intermediate = await win.webContents.executeJavaScript(`(()=>{const e=document.querySelector('.agent-cursor');const m=new DOMMatrix(getComputedStyle(e).transform);return {x:m.m41,y:m.m42}})()`);
  assert.ok(intermediate.x < lastMotion!.displayX! - 2, JSON.stringify(intermediate));
  ok(await first);
  const pressed = await guest.executeJavaScript('events.find(e=>e.type==="mousedown").time');
  assert.ok(pressed >= acks.at(-1)!.time - 2);
  assert.ok(Date.now() - started >= 160);
  console.log('PASS real overlay travels before native click; DOM mousedown follows presentation ACK');
  const count = acks.length;
  ok(await click(760, 88));
  assert.equal(acks.length, count);
  console.log('PASS repeated same-position click does not wait for another glide');
  const snap: any = await run('snapshot', { navigation_revision: binding().navigation_revision, snapshot_revision: 1, limits: { max_nodes: 96, max_text_bytes: 10240 } });
  ok(snap);
  const ref = snap.payload.payload.root.children.find((node: any) => node.name === 'Text').element_ref;
  ok(await run('type', { target: { element_ref: ref }, text: 'Magi cursor', replace: true, submit_key: null }));
  const focus = await guest.executeJavaScript('events.find(e=>e.type==="focusin" && e.target==="text").time');
  assert.ok(focus >= acks.at(-1)!.time - 2);
  assert.equal(await guest.executeJavaScript('text.value'), 'Magi cursor');
  console.log('PASS input gets native focus only after arrival, then receives text');
  win.webContents.send('cursor-event', { type: 'visibility', visible: false });
  await delay(40);
  ok(await click(20, 20));
  assert.equal(acks.at(-1)!.outcome, 'hidden');
  console.log('PASS background overlay skips animation via hidden ACK');
  win.webContents.send('cursor-event', { type: 'visibility', visible: true });
  await delay(40);
  await win.webContents.debugger.sendCommand('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
  ok(await click(300, 300));
  assert.equal(acks.at(-1)!.outcome, 'arrived');
  const reduced = await guest.executeJavaScript('events.filter(e=>e.type==="mousedown").at(-1).time');
  assert.ok(reduced >= acks.at(-1)!.time - 2);
  console.log('PASS reduced motion still confirms rendered position before native input');
  await win.webContents.debugger.sendCommand('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }] });
  await surfaces.setViewport(binding(), { mode: 'fixed', width: 1280, height: 800, device_scale_factor_millis: 2000, device_type: 'desktop' });
  ok(await click(760, 88));
  const scaled = await guest.executeJavaScript('events.filter(e=>e.type==="mousedown").at(-1)');
  assert.equal(scaled.target, 'target');
  assert.ok(scaled.time >= acks.at(-1)!.time - 2);
  console.log('PASS scaled fixed viewport with DPR 2 keeps rendered cursor and native button hit aligned');
  await surfaces.setViewport(binding(), { mode: 'fixed', width: 1280, height: 800, device_scale_factor_millis: 2000, device_type: 'mobile' });
  ok(await click(760, 88));
  assert.equal(await guest.executeJavaScript('events.filter(e=>e.type==="mousedown").at(-1).target'), 'target');
  console.log('PASS scaled mobile viewport uses the same native pointer coordinates');
  mode = 'hold'; lastMotion = null;
  const intercepted = click(750, 88);
  await until(() => lastMotion !== null);
  const before = await guest.executeJavaScript('events.filter(e=>e.type==="click" && e.target==="target").length');
  guest.sendInputEvent({ type: 'mouseDown', x: 30, y: 30, button: 'left', clickCount: 1 });
  guest.sendInputEvent({ type: 'mouseUp', x: 30, y: 30, button: 'left', clickCount: 1 });
  assert.notEqual((await intercepted).status, 'succeeded');
  surfaces.completeAgentCursorPresentation('window', { ...lastMotion!.presentation!, outcome: 'arrived' });
  assert.equal(await guest.executeJavaScript('events.filter(e=>e.type==="click" && e.target==="target").length'), before);
  console.log('PASS physical user takeover during movement cancels pending click; late ACK cannot replay');
  await acquire(); lastMotion = null;
  abort = new AbortController();
  const beforeCancel = await guest.executeJavaScript('events.filter(e=>e.type==="mousedown").length');
  const cancelled = click(700, 90);
  await until(() => lastMotion !== null);
  abort.abort();
  assert.notEqual((await cancelled).status, 'succeeded');
  assert.equal(await guest.executeJavaScript('events.filter(e=>e.type==="mousedown").length'), beforeCancel);
  abort = null;
  console.log('PASS cancellation during movement never dispatches click');
  await acquire(); lastMotion = null;
  // 取消时 Main 已记录展示目标，但没有到位确认；相同坐标也不能绕过握手。
  const navigating = click(700, 90);
  await until(() => lastMotion !== null);
  console.log('PASS same-position action after cancellation requests a fresh presentation ACK');
  await guest.loadURL(origin + '/next');
  assert.notEqual((await navigating).status, 'succeeded');
  assert.equal(await guest.executeJavaScript('events.length'), 0);
  console.log('PASS navigation during movement rejects old action before native input');
  surfaces.closeAll();
  console.log('PASS cursor presentation acceptance complete');
  win.destroy(); server.close(); app.quit();
}).catch(error => { console.error(error); app.exit(1); });
