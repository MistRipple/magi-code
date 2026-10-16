// Current Desktop + daemon + real public pages. The local provider only selects production tools.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import electron from 'electron';
import { setTimeout as delay } from 'node:timers/promises';
import { connectAppPage, connectTarget, pageTools, waitFor } from './lib/electron-harness.mjs';

const root = new URL('..', import.meta.url).pathname.replace(/\/$/, '');
const work = await mkdtemp(join(tmpdir(), 'magi-cursor-desktop-live-'));
const artifact = process.env.MAGI_CURSOR_LIVE_ARTIFACTS || join(work, 'evidence');
await mkdir(artifact, { recursive: true });
const logs = [], errors = [], plans = new Map(), held = new Map(), results = new Map();
const processes = [];
let page, guest;
const unusedPort = async () => {
  const server = createServer();
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
};
const daemonPort = await unusedPort(), cdpPort = await unusedPort();
const provider = createServer(async (request, response) => {
  try {
    if (request.method !== 'POST') return void response.end(JSON.stringify({ data: [{ id: 'cursor-live-model' }] }));
    let body = ''; for await (const chunk of request) body += chunk;
    const input = JSON.parse(body), messages = input.messages || [];
    const prompt = [...messages].reverse().find(m => m.role === 'user');
    const key = [...plans.keys()].find(k => JSON.stringify(prompt?.content).includes(k));
    assert.ok(key, 'known test prompt');
    const toolMessages = messages.filter(m => m.role === 'tool' && m.tool_call_id?.startsWith(key + '-'));
    for (const m of toolMessages) results.set(m.tool_call_id, JSON.parse(m.content));
    const index = toolMessages.length;
    held.set(`${key}-${index}`, () => {
      const delta = index === plans.get(key).length ? { content: key + '_DONE' } : (() => {
        const [name, args] = plans.get(key)[index];
        const actualName = input.tools?.find(t => t.function.name === name || t.function.name.endsWith('_' + name) || t.function.name.endsWith('.' + name))?.function.name || name;
        return { tool_calls: [{ index: 0, id: `${key}-${index}`, type: 'function', function: { name: actualName, arguments: JSON.stringify(typeof args === 'function' ? args() : args) } }] };
      })();
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      response.end(`data: ${JSON.stringify({ choices: [{ delta, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: index === plans.get(key).length ? 'stop' : 'tool_calls' }] })}\n\ndata: [DONE]\n\n`);
    });
  } catch (error) { errors.push(String(error)); response.writeHead(500).end(String(error)); }
});
await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
const env = { ...process.env, MAGI_STATE_ROOT: join(work, 'state'), MAGI_PORT: String(daemonPort), MAGI_WEB_DEV: '0',
  MAGI_TARGET_PRUNE_GIB: '1000', MAGI_OPEN_BROWSER: '0', MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
  MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${provider.address().port}/v1`,
  MAGI_OPENAI_COMPAT_API_KEY: 'local-test', MAGI_OPENAI_COMPAT_MODEL: 'cursor-live-model' };
const launch = (command, args, extra = {}) => {
  const child = spawn(command, args, { cwd: root, env: { ...env, ...extra }, stdio: ['ignore', 'pipe', 'pipe'] });
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => {
    logs.push(String(chunk));
    if (process.env.MAGI_CURSOR_LIVE_VERBOSE) process.stdout.write(chunk);
  });
  child.on('exit', code => { if (code) console.error(`${command} exited ${code}: ${logs.join('').slice(-6000)}`); });
  processes.push(child); return child;
};
const pause = key => waitFor(() => held.has(key), key, 90000, 100);
const resume = key => { const reply = held.get(key); assert.ok(reply, key); held.delete(key); reply(); };
const success = key => { const r = results.get(key); assert.equal(r?.status, 'succeeded', JSON.stringify(r)); return r; };
const element = (key, match) => {
  const r = success(key), nodes = r.snapshot?.elements || r.elements;
  const node = nodes?.find(match); assert.ok(node, JSON.stringify(r)); return node.element_ref;
};
const probe = `window.__cursorLiveEvents=[];for(const type of ['mousedown','focusin','input','click'])document.addEventListener(type,e=>window.__cursorLiveEvents.push({type,time:Date.now(),x:e.clientX,y:e.clientY,id:e.target.id,tag:e.target.tagName}),true);`;
async function send(key, plan) {
  plans.set(key, plan);
  await waitFor(() => page.evaluate(`document.querySelector('[data-testid=input-textarea]')?.getAttribute('contenteditable')==='true'`), 'composer');
  await page.evaluate(`(()=>{const e=document.querySelector('[data-testid=input-textarea]');e.textContent=${JSON.stringify(key)};e.dispatchEvent(new InputEvent('input',{bubbles:true,inputType:'insertText',data:${JSON.stringify(key)}}));})()`);
  await waitFor(() => page.evaluate(`Boolean(document.querySelector('[data-testid=input-send-button]:not([disabled])'))`), 'send');
  await page.evaluate(`document.querySelector('[data-testid=input-send-button]').click()`);
  await pause(key + '-0');
}
async function finish(key, index) {
  await pause(`${key}-${index}`); resume(`${key}-${index}`);
  await waitFor(() => page.evaluate(`document.body.innerText.includes(${JSON.stringify(key + '_DONE')})`), 'turn completed');
}
async function attach(host) {
  guest?.close();
  guest = await waitFor(() => connectTarget(cdpPort, t => t.type === 'webview' && t.url.includes(host), host).catch(() => null), 'real guest ' + host, 30000);
}
async function screenshot(name) { await writeFile(join(artifact, name + '.png'), Buffer.from((await page.call('Page.captureScreenshot', { format: 'png' })).data, 'base64')); }
async function verifyArrival(label) {
  const events = await guest.evaluate('window.__cursorLiveEvents');
  const samples = await page.evaluate('window.__cursorLiveSamples');
  const down = events.filter(e => e.type === 'mousedown').at(-1);
  assert.ok(down, label + ' native mousedown');
  const motion = (await page.evaluate('window.__cursorLiveMotions')).filter(e => e.presentation && e.time <= down.time).at(-1);
  assert.ok(motion, label + ' motion');
  const before = samples.filter(s => s.time <= down.time && s.time >= motion.time);
  assert.ok(before.some(s => Math.hypot(s.x - motion.displayX, s.y - motion.displayY) < 2), label + ' cursor rendered at target before mousedown');
  assert.ok(before.some(s => Math.hypot(s.x - motion.displayX, s.y - motion.displayY) > 2), label + ' visible travel from previous position');
  console.log('PASS ' + label + ': rendered cursor arrival precedes native mousedown');
}

try {
  const daemon = launch('bash', ['scripts/dev-daemon.sh']);
  const deadline = Date.now() + 240000;
  while (!(await fetch(`http://127.0.0.1:${daemonPort}/health`).then(r => r.ok).catch(() => false))) {
    if (daemon.exitCode !== null) throw new Error('daemon exited: ' + daemon.exitCode);
    if (Date.now() > deadline) throw new Error('current daemon startup timeout');
    await delay(500);
  }
  console.log('PASS current daemon ready');
  launch(electron, [`--remote-debugging-port=${cdpPort}`, 'apps/desktop'], { MAGI_DESKTOP_REUSE_DAEMON: '1' });
  page = await waitFor(async () => {
    const p = await connectAppPage(cdpPort, daemonPort, errors).catch(() => null);
    if (!p) return null;
    if (await p.evaluate(`Boolean(document.querySelector('[data-testid=input-textarea]'))`)) return p;
    p.close(); return null;
  }, 'full Magi Desktop workbench', 60000);
  const tools = pageTools(page);
  await page.evaluate(`(()=>{window.__cursorLiveSamples=[];window.__cursorLiveMotions=[];window.magiDesktop.onBrowserEvent(e=>{if(e.type==='agent_cursor')window.__cursorLiveMotions.push({...e,time:Date.now()})});const sample=()=>{const e=document.querySelector('.agent-cursor-layer:popover-open .agent-cursor');if(e){const m=new DOMMatrix(getComputedStyle(e).transform);window.__cursorLiveSamples.push({time:Date.now(),x:m.m41,y:m.m42})}requestAnimationFrame(sample)};requestAnimationFrame(sample)})()`);
  console.log('PASS current full Desktop renderer + preload + Main + Worker connected');

  const first = 'CURSOR_LIVE_EXAMPLE';
  await send(first, [
    ['browser_navigate', { url: 'https://example.com', include_snapshot: true, init_script: probe }],
    ['browser_click', () => ({ element_ref: element(first + '-0', e => e.role === 'link'), include_snapshot: true })],
  ]);
  resume(first + '-0'); await pause(first + '-1'); success(first + '-0');
  await tools.click('.header-right-pane-btn');
  await page.call('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }] });
  await attach('example.com');
  await waitFor(() => tools.exists('.agent-cursor-layer:popover-open .agent-cursor'), 'visible agent cursor');
  await screenshot('01-example-before-click');
  resume(first + '-1'); await pause(first + '-2');
  console.log('RESULT example click ' + JSON.stringify(results.get(first + '-1')));
  await attach('iana.org');
  assert.match(await guest.evaluate('location.href'), /iana.org/);
  await screenshot('02-iana-after-native-click');
  console.log('PASS Example.com link navigates real guest to IANA');
  await finish(first, 2);

  const wiki = 'CURSOR_LIVE_WIKIPEDIA';
  await send(wiki, [
    ['browser_navigate', { url: 'https://www.wikipedia.org', include_snapshot: true, init_script: probe }],
    ['browser_type', () => ({ element_ref: element(wiki + '-0', e => e.editable), text: 'Rust programming language', replace: true })],
    ['browser_type', () => ({ element_ref: element(wiki + '-0', e => e.editable), text: 'Magi cursor test', replace: true })],
    ['browser_scroll', { delta_y: 400 }],
  ]);
  resume(wiki + '-0'); await pause(wiki + '-1'); success(wiki + '-0'); await attach('wikipedia.org');
  resume(wiki + '-1'); await pause(wiki + '-2'); success(wiki + '-1');
  assert.equal(await guest.evaluate('document.activeElement.value'), 'Rust programming language');
  await verifyArrival('Wikipedia search focus and typing');
  await screenshot('03-wikipedia-typed');
  resume(wiki + '-2'); await pause(wiki + '-3'); success(wiki + '-2');
  assert.equal(await guest.evaluate('document.activeElement.value'), 'Magi cursor test');
  resume(wiki + '-3'); await pause(wiki + '-4'); success(wiki + '-3');
  console.log('PASS Wikipedia native replace input and scroll'); await finish(wiki, 4);

  const rust = 'CURSOR_LIVE_RUST';
  await send(rust, [
    ['browser_navigate', { url: 'https://doc.rust-lang.org/book/', include_snapshot: true, init_script: probe }],
    ['browser_click', () => ({ element_ref: element(rust + '-0', e => e.role === 'button' && /search/i.test(e.name)), include_snapshot: true })],
    ['browser_type', () => ({ element_ref: element(rust + '-1', e => e.editable), text: 'ownership', replace: true, submit_key: 'Enter' })],
    ['browser_snapshot', {}],
  ]);
  resume(rust + '-0'); await pause(rust + '-1'); success(rust + '-0'); await attach('doc.rust-lang.org');
  resume(rust + '-1'); await pause(rust + '-2'); success(rust + '-1'); await verifyArrival('Rust Book search button');
  resume(rust + '-2'); await pause(rust + '-3');
  const typed = results.get(rust + '-2');
  // mdBook 用 history.replaceState 更新搜索 URL；不重放已生效的写入，重新读取新导航。
  assert.ok(['succeeded', 'indeterminate'].includes(typed?.status), JSON.stringify(typed));
  assert.equal(await guest.evaluate('document.activeElement.value'), 'ownership');
  resume(rust + '-3'); await pause(rust + '-4');
  assert.ok(success(rust + '-3').snapshot.elements.some(e => e.editable && e.value === 'ownership'));
  await waitFor(() => guest.evaluate(`location.search.includes('ownership') && document.querySelectorAll('#mdbook-searchresults li').length > 0`), 'actual Rust Book search results');
  await screenshot('04-rust-book-search'); await finish(rust, 4);
  console.log('PASS Rust Book opens search, receives native text, and confirms value after search URL update');

  const takeover = 'CURSOR_LIVE_TAKEOVER';
  await send(takeover, [
    ['browser_scroll', { delta_y: 20 }],
    ['browser_snapshot', {}],
    ['browser_click', () => ({ element_ref: element(takeover + '-1', e => e.role === 'button' && /theme/i.test(e.name)) })],
  ]);
  resume(takeover + '-0'); await pause(takeover + '-1'); success(takeover + '-0');
  resume(takeover + '-1'); await pause(takeover + '-2'); success(takeover + '-1');
  await tools.click('.header-right-pane-btn');
  await waitFor(() => page.evaluate(`!document.querySelector('.agent-cursor-layer:popover-open .agent-cursor')`), 'collapsed cursor hidden');
  await tools.click('.header-right-pane-btn');
  await waitFor(() => tools.exists('.agent-cursor-layer:popover-open .agent-cursor'), 'reopened cursor restored');
  console.log('PASS full Desktop pane collapse and reopen restore the agent cursor');
  await page.call('Runtime.addBinding', { name: '__cursorLiveTakeover' });
  let userInput;
  const off = page.on('Runtime.bindingCalled', event => {
    if (event.name !== '__cursorLiveTakeover' || userInput) return;
    userInput = (async () => {
      await guest.call('Input.dispatchMouseEvent', { type: 'mousePressed', x: 120, y: 120, button: 'left', buttons: 1, clickCount: 1 });
      await guest.call('Input.dispatchMouseEvent', { type: 'mouseReleased', x: 120, y: 120, button: 'left', buttons: 0, clickCount: 1 });
    })();
  });
  await page.evaluate(`window.magiDesktop.onBrowserEvent(e=>{if(e.type==='agent_cursor'&&e.presentation)window.__cursorLiveTakeover(e.presentation.motionId)})`);
  resume(takeover + '-2'); await pause(takeover + '-3');
  assert.ok(userInput, 'user intervenes during actual cursor motion'); await userInput; off();
  assert.notEqual(results.get(takeover + '-2')?.status, 'succeeded');
  await waitFor(() => tools.exists('.hand-back-button'), 'user has control');
  assert.equal(await tools.exists('.agent-cursor-layer:popover-open .agent-cursor'), false);
  assert.equal(await guest.evaluate(`document.querySelector('#mdbook-theme-toggle').getAttribute('aria-expanded')`), 'false');
  await screenshot('05-user-takeover'); await finish(takeover, 3);
  console.log('PASS user click during movement stops agent click, hides cursor, and offers hand-back');
  await tools.click('.hand-back-button');
  const returned = 'CURSOR_LIVE_RETURNED';
  await send(returned, [
    ['browser_snapshot', {}],
    ['browser_click', () => ({ element_ref: element(returned + '-0', e => e.role === 'button' && /theme/i.test(e.name)) })],
  ]);
  resume(returned + '-0'); await pause(returned + '-1'); success(returned + '-0');
  resume(returned + '-1'); await pause(returned + '-2'); success(returned + '-1');
  assert.equal(await guest.evaluate(`document.querySelector('#mdbook-theme-toggle').getAttribute('aria-expanded')`), 'true');
  await screenshot('06-returned-to-agent'); await finish(returned, 2);
  console.log('PASS hand-back resumes agent control and native page interaction');
  assert.deepEqual(errors.filter(e => !e.includes('GPT Web 插件未安装或未激活')), [], 'App Renderer exceptions');
  await writeFile(join(artifact, 'results.json'), JSON.stringify({ results: Object.fromEntries(results), errors,
    motions: await page.evaluate('window.__cursorLiveMotions'), samples: await page.evaluate('window.__cursorLiveSamples') }, null, 2));
  console.log('PASS full Desktop public-web cursor acceptance complete');
} catch (error) {
  await screenshot('failure').catch(() => {});
  console.error(error); console.error('RESULTS', JSON.stringify(Object.fromEntries(results)));
  process.exitCode = 1;
} finally {
  await writeFile(join(artifact, 'runtime.log'), logs.join(''));
  console.log('Evidence: ' + artifact);
  page?.close(); guest?.close();
  for (const child of processes.reverse()) {
    if (child.exitCode !== null) continue;
    child.kill('SIGTERM');
    await Promise.race([new Promise(resolve => child.once('exit', resolve)), new Promise(resolve => setTimeout(resolve, 3000))]);
    if (child.exitCode === null) child.kill('SIGKILL');
  }
  provider.closeAllConnections(); provider.close();
}
