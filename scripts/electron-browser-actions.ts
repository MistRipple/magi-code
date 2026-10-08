// Opt-in real Chromium regression: production Worker -> SurfaceManager -> webview CDP.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { app, BrowserWindow } from 'electron';
import { BrowserSurfaceManager } from '../apps/desktop/src/main/browser-surface-manager.js';
import { BrowserAutomationRuntime } from '../browser-automation-worker/src/runtime.js';
import { CdpClient } from '../browser-automation-worker/src/cdp-client.js';

app.setPath('userData', process.env.MAGI_ACTIONS_TEST_DATA!);
app.dock?.hide();
app.whenReady().then(async () => {
  const html = '<!doctype html><meta charset="utf-8"><title>验收</title>'
    + `<button id="button" onclick="window.clicks=(window.clicks||0)+1">Click</button>`
    + `<button id="disabled" disabled>Disabled</button><button id="blocked">Blocked</button>`
    + `<div id="cover" style="position:absolute;background:red;z-index:10"></div>`
    + `<input id="text" aria-label="Text" value="old"><input id="email" type="email" aria-label="Email" value="old@example.com">`
    + `<input id="number" type="number" aria-label="Number" value="12"><textarea id="textarea" aria-label="Textarea">old</textarea>`
    + `<div id="editor" contenteditable aria-label="Editor" style="min-height:20px">old</div>`
    + `<input id="limited" maxlength="2" aria-label="Limited"><input id="readonly" readonly aria-label="Readonly">`
    + `<div id="secrets"><label>Password<input type="password" value="SECRET_PASSWORD"></label>`
    + `<label>Code<textarea autocomplete="section-login one-time-code">SECRET_CODE</textarea></label>`
    + `<select autocomplete="section-card cc-number"><option>SECRET_CARD</option></select></div>`
    + `<input id="check" type="checkbox" aria-label="Check"><section id="scope"><button>Scoped</button></section>`
    + `<div id="shadow"></div><iframe id="frame" src="/frame"></iframe>`
    + `<button id="navigate" onclick="location.href='/next'">Navigate</button>`
    + `<script>const r=blocked.getBoundingClientRect();Object.assign(cover.style,{left:r.x+"px",top:r.y+"px",width:r.width+"px",height:r.height+"px"});`
    + `shadow.attachShadow({mode:"open"}).innerHTML='<button onclick="window.shadowClicks=(window.shadowClicks||0)+1">Shadow</button>';</script>`;
  const server = createServer((req, res) => {
    res.setHeader('content-type', 'text/html; charset=utf-8');
    res.end(req.url === '/frame' ? '<button onclick="parent.frameClicks=(parent.frameClicks||0)+1">Frame</button>' : req.url === '/next' ? '<title>Next</title><button>Next</button>' : html);
  });
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const url = 'http://127.0.0.1:' + (server.address() as { port: number }).port;
  const win = new BrowserWindow({ show: false, width: 1100, height: 850, webPreferences: { webviewTag: true, sandbox: true, contextIsolation: true } });
  let receive: (event: { data: unknown }) => void;
  const surfaces = new BrowserSurfaceManager({ desktopEpoch: 'acceptance', onEvent(event) {
    if (event.type === 'cdp_event') receive?.({ data: { type: 'cdp_event', binding: event.binding, method: event.method, params: event.params, ...(event.sessionId ? { session_id: event.sessionId } : {}) } });
  } });
  const runtime = new BrowserAutomationRuntime(new CdpClient({
    on(_event, listener) { receive = listener; },
    async postMessage(message) {
      if (message.type !== 'cdp_request') return;
      const reply = { type: 'cdp_response', call_id: message.call_id, request_id: message.request_id, binding: message.binding };
      try {
        const result = await surfaces.sendCdp(message.binding, message.method, message.params, message.session_id);
        receive({ data: { ...reply, result } });
      } catch (cause) {
        receive({ data: { ...reply, error: { code: 'browser_cdp_error', message: String(cause), recoverable: false, side_effect_started: false } } });
      }
    },
  }));
  surfaces.attachWindow('window', win.webContents);
  await surfaces.materialize({ windowId: 'window', tabId: 'tab', browserSessionId: 'acceptance', initialUrl: url, navigationRevision: 0, viewport: { mode: 'auto' } });
  const attached = new Promise<Electron.WebContents>((resolve) => win.webContents.once('did-attach-webview', (_event, guest) => resolve(guest)));
  await win.loadURL('data:text/html,' + encodeURIComponent('<input id="host"><webview partition="magi-browser-acceptance" src="' + url + '" webpreferences="focusOnNavigation=no" style="width:1000px;height:700px"></webview>'));
  const guest = await attached;
  if (guest.isLoadingMainFrame()) await new Promise<void>((resolve) => guest.once('did-finish-load', () => resolve()));
  surfaces.registerEmbeddedWebview('window', { tabId: 'tab', browserSessionId: 'acceptance', navigationRevision: 0, webContentsId: guest.id, displaySize: { width: 1000, height: 700 } });
  let call = 0, revision = 0;
  const run = async (type: string, payload: object) => {
    const binding = surfaces.primaryBindingForTab('tab')!;
    runtime.rebind([binding]);
    return (await runtime.execute('call-' + ++call, binding, { type, payload: { tab_id: 'tab', control: { mode: 'user', fence: 1 }, ...payload } } as any)).outcome;
  };
  const snapshot = async (limits = { max_nodes: 96, max_text_bytes: 10240 }, selector?: string) => {
    const result = await run('snapshot', { navigation_revision: surfaces.primaryBindingForTab('tab')!.navigation_revision, snapshot_revision: ++revision, limits, ...(selector ? { selector } : {}) });
    assert.equal(result.status, 'succeeded', JSON.stringify(result));
    return (result as any).payload.payload;
  };
  const success = (result: any) => assert.equal(result.status, 'succeeded', JSON.stringify(result));
  let snap = await snapshot();
  const target = (name: string) => ({ element_ref: snap.root.children.find((node: any) => node.name === name)?.element_ref });
  const click = (name: string) => run('click', { target: target(name) });
  const type = (name: string, text: string, replace = true) => run('type', { target: target(name), text, replace, submit_key: null });
  assert.equal(JSON.stringify(snap).includes('SECRET_'), false);
  assert.equal(snap.root.children.filter((node: any) => node.sensitive_input_kind).length, 3);
  await guest.executeJavaScript('for(const e of document.querySelectorAll("#secrets input,#secrets textarea,#secrets select")) Object.defineProperty(e,"value",{get(){throw new Error("sensitive value read")}});undefined');
  const read = await run('devtools', { operation: 'read', arguments: {} });
  success(read);
  assert.equal(JSON.stringify(read).includes('SECRET_'), false);
  await snapshot();
  const tiny = await snapshot({ max_nodes: 96, max_text_bytes: 12 });
  assert.ok(tiny.root.children.length > 0);
  assert.ok(tiny.truncated && tiny.text_bytes <= 12);
  const scoped = await snapshot(undefined, '#scope');
  assert.ok(scoped.root.children.some((node: any) => node.name === 'Scoped'));
  assert.ok(!scoped.root.children.some((node: any) => node.name === 'Click'));
  await guest.executeJavaScript('document.body.insertAdjacentHTML("beforeend", "<div id=large>"+"<input aria-label=大量输入><a href=#>链接</a>".repeat(150)+"<section id=late><button>Late target</button></section></div>")');
  const bounded = await snapshot();
  assert.equal(bounded.returned_nodes, 96);
  assert.equal(bounded.root.children.length, 96);
  assert.equal(bounded.truncated, true);
  assert.ok(bounded.root.children.some((node: any) => node.name === 'late'));
  const bytes = (bounded.root.name ? Buffer.byteLength(bounded.root.name) : 0) + bounded.root.children.reduce((sum: number, node: any) => sum + ['role','name','value','description'].reduce((size, key) => size + (node[key] ? Buffer.byteLength(node[key]) : 0), 0), 0);
  assert.equal(bounded.text_bytes, bytes);
  const late = await snapshot(undefined, '#late');
  assert.ok(late.root.children.some((node: any) => node.name === 'Late target'));
  await guest.executeJavaScript('document.getElementById("large").remove()');
  console.log('PASS sensitive values, UTF-8 budget, retained nodes and scoped snapshot');
  snap = await snapshot();
  for (const mode of ['hidden', 'host-focused', 'offscreen']) {
    if (mode === 'host-focused') { win.show(); win.webContents.focus(); await win.webContents.executeJavaScript('host.focus()'); }
    if (mode === 'offscreen') { await win.webContents.executeJavaScript('document.querySelector("webview").style.cssText="position:fixed;left:-12000px;width:1000px;height:700px"'); win.hide(); }
    success(await click('Click'));
    for (const [name, id, text] of [['Text', 'text', 'new'], ['Email', 'email', 'new@example.com'], ['Number', 'number', '34'], ['Textarea', 'textarea', '一行'], ['Editor', 'editor', 'hello']]) {
      success(await type(name, text));
      success(await type(name, name === 'Email' ? '.cn' : name === 'Number' ? '5' : '追加', false));
      assert.equal(await guest.executeJavaScript('document.getElementById(' + JSON.stringify(id) + ').' + (id === 'editor' ? 'innerText' : 'value')), text + (name === 'Email' ? '.cn' : name === 'Number' ? '5' : '追加'));
      success(await type(name, ''));
    }
  }
  assert.equal(await guest.executeJavaScript('window.clicks'), 3);
  assert.equal(await win.webContents.executeJavaScript('host.value'), '');
  console.log('PASS native click exactly once; replace/append/clear in hidden, host-focused and offscreen guests');
  for (const name of ['Disabled', 'Blocked']) assert.equal((await click(name)).status, 'failed');
  assert.equal((await type('Readonly', 'x')).status, 'failed');
  assert.equal((await type('Limited', 'too long')).status, 'indeterminate');
  success(await click('Shadow'));
  success(await click('Frame'));
  assert.deepEqual(await guest.executeJavaScript('[window.shadowClicks,window.frameClicks]'), [1, 1]);
  await guest.executeJavaScript('frame.style.transform="scale(0.65)";frame.style.transformOrigin="top left"');
  success(await click('Frame'));
  assert.equal(await guest.executeJavaScript('window.frameClicks'), 2);
  const multiline = 'first' + String.fromCharCode(10) + 'second';
  success(await type('Editor', multiline));
  success(await type('Editor', ' tail', false));
  success(await type('Editor', ''));
  await guest.executeJavaScript('editor.dir="rtl"');
  success(await type('Editor', 'שלום'));
  success(await type('Editor', ' עולם', false));
  success(await type('Editor', ''));
  await guest.executeJavaScript('document.body.insertAdjacentHTML("beforeend", "<select id=select aria-label=Select><option value=one>One</option><option value=two>Two</option></select>")');
  snap = await snapshot();
  success(await run('devtools', { operation: 'fill_form', arguments: { fields: [{ ...target('Select'), value: 'two' }] } }));
  assert.equal(await guest.executeJavaScript('document.getElementById("select").value'), 'two');
  success(await run('devtools', { operation: 'fill_form', arguments: { fields: [{ ...target('Check'), value: true }] } }));
  assert.equal(await guest.executeJavaScript('check.checked'), true);
  console.log('PASS disabled/obscured/readonly rejection, input mismatch, shadow DOM, scaled iframe, multiline editor and checkbox');
  // 同一真实 guest 验证坐标点击、悬停、下拉禁用与原生拖拽，复用现有验收入口。
  await guest.executeJavaScript(`document.body.innerHTML = '<button id="point">Point</button><button id="nav" onclick="location.href=&quot;/next&quot;">Navigate</button><select id="choices" aria-label="Choices" multiple><option value="one" selected>One</option><option value="two">Two</option><option value="disabled" disabled>Disabled</option><optgroup disabled><option value="group">Group</option></optgroup></select><div id="dragSource" draggable="true" style="width:100px;height:80px;background:red">Source</div><div id="dragTarget" style="width:100px;height:80px;background:blue;margin-left:200px">Target</div><button id="far" style="margin-top:2400px">Far</button>';undefined`);
  await guest.executeJavaScript(`window.clicks=0;window.doubles=0;window.hovers=0;window.changes=0;window.drops=0;window.trusted=[];window.cancelDrag=false;window.acceptDrop=true;
    point.onclick=()=>window.clicks++;point.ondblclick=()=>window.doubles++;far.onmouseenter=()=>window.hovers++;
    choices.onchange=()=>window.changes++;
    dragSource.ondragstart=e=>{window.trusted.push(e.isTrusted);if(window.cancelDrag)e.preventDefault();else e.dataTransfer.setData('text/plain','native-data')};
    dragTarget.ondragover=e=>{if(window.acceptDrop)e.preventDefault()};dragTarget.ondrop=e=>{window.drops++;window.trusted.push(e.isTrusted);window.droppedText=e.dataTransfer.getData('text/plain')};undefined`);
  snap = await snapshot();
  const coordinates = async (id: string) => guest.executeJavaScript('(()=>{const e=document.getElementById('+JSON.stringify(id)+');e.scrollIntoView({block:"center"});const r=e.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()');
  const at = (args: object) => run('devtools', { operation: 'click_at', arguments: args });
  for (const coords of [{x:-1,y:5}, {x:5,y:-1}, {x:1000,y:5}, {x:5,y:700}]) assert.equal((await at(coords)).status, 'failed');
  assert.equal(await guest.executeJavaScript('window.clicks'), 0);
  success(await at({...await coordinates('point'),double_click:true}));
  assert.deepEqual(await guest.executeJavaScript('[window.clicks,window.doubles]'), [2,1]);
  success(await run('devtools',{operation:'hover',arguments:target('Far')}));
  assert.equal(await guest.executeJavaScript('window.hovers'), 1);
  assert.ok(await guest.executeJavaScript('scrollY > 0'));
  await guest.executeJavaScript('(()=>{const r=far.getBoundingClientRect();const cover=document.createElement("div");cover.id="hoverCover";Object.assign(cover.style,{position:"fixed",left:r.x+"px",top:r.y+"px",width:r.width+"px",height:r.height+"px",zIndex:99});document.body.append(cover)})()');
  assert.equal((await run('devtools',{operation:'hover',arguments:target('Far')})).status, 'failed');
  await guest.executeJavaScript('document.getElementById("hoverCover").remove()');
  const fill = (value: string[]) => run('devtools', {operation:'fill_form', arguments:{fields:[{...target('Choices'),value}]}});
  for (const value of [['two','disabled'], ['group']]) assert.equal((await fill(value)).status, 'failed');
  assert.deepEqual(await guest.executeJavaScript('[[...choices.selectedOptions].map(e=>e.value),window.changes]'), [['one'],0]);
  success(await fill(['two']));
  assert.deepEqual(await guest.executeJavaScript('[[...choices.selectedOptions].map(e=>e.value),window.changes]'), [['two'],1]);
  const drag = () => run('devtools', {operation:'drag',arguments:{source:target('Source'),target:target('Target')}});
  success(await drag());
  assert.deepEqual(await guest.executeJavaScript('[window.drops,window.droppedText,window.trusted.every(Boolean)]'),[1,'native-data',true]);
  await guest.executeJavaScript('window.cancelDrag=true');
  assert.equal((await drag()).status, 'indeterminate');
  assert.equal(await guest.executeJavaScript('window.drops'), 1);
  await guest.executeJavaScript('window.cancelDrag=false;window.acceptDrop=false');
  assert.equal((await drag()).status, 'indeterminate');
  assert.equal(await guest.executeJavaScript('window.drops'), 1);
  console.log('PASS coordinate bounds and trusted double-click, offscreen hover, disabled options/optgroup, native drop and cancelled/rejected drag');
  // 跨域页面只允许原生坐标输入，不能把父 frame 没有收到事件冒充已确认。
  await guest.executeJavaScript('window.scrollTo(0,0);document.body.insertAdjacentHTML("afterbegin", "<iframe id=cross style=width:300px;height:150px></iframe>");cross.src='+JSON.stringify(url.replace('127.0.0.1','localhost')+'/frame')+';new Promise(resolve=>cross.onload=()=>resolve(true))');
  const crossPoint = await guest.executeJavaScript('(()=>{const r=cross.getBoundingClientRect();return {x:r.x+25,y:r.y+25}})()');
  assert.equal(await guest.executeJavaScript('cross.contentDocument === null'), true);
  assert.equal((await at(crossPoint)).status, 'indeterminate');
  await guest.executeJavaScript('cross.remove()');
  console.log('PASS inaccessible frame never reports an unobserved click as confirmed');
  const beforeNavigation = target('Point');
  await click('Navigate');
  if (guest.isLoadingMainFrame()) await new Promise<void>((resolve) => guest.once('did-finish-load', () => resolve()));
  assert.ok(guest.getURL().endsWith('/next'));
  snap = await snapshot();
  assert.equal((await run('click', { target: beforeNavigation })).status, 'failed');
  console.log('PASS navigation invalidates prior node references');
  win.destroy(); server.close(); app.exit(0);
}).catch((cause) => { console.error(cause); app.exit(1); });
