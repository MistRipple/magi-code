import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { cp, mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { installFixedUpstreamFromNative } from './lib/plugin-native-acceptance.mjs';

const output = resolve(process.env.MAGI_DSH_SIDEBAR_OUTPUT ?? 'target/plugin-acceptance/phase-2-better-sidebar');
const bundle = join(resolve(process.env.MAGI_PLUGIN_TEST_APP ?? 'target/electron-dist/mac-arm64/Magi.app'), 'Contents');
const resources = join(bundle, 'Resources');
const run = await mkdtemp(join(tmpdir(), 'magi-sidebar-phase2-'));
const workspace = await mkdtemp(join(tmpdir(), 'magi-sidebar-project-'));
const filename = 'sidebar-check.txt';
const original = 'magi-sidebar-original';
const edited = 'magi-sidebar-saved-in-editor';
await mkdir(output, { recursive: true });
await writeFile(join(workspace, filename), original);
const port = Number(process.env.MAGI_DSH_SIDEBAR_PORT ?? 38963);
const cdpPort = Number(process.env.MAGI_DSH_SIDEBAR_CDP_PORT ?? 9341);
const origin = `http://127.0.0.1:${port}`;
const responseText = 'SIDEBAR_ACCEPTANCE_RESPONSE';
const provider = createServer(async (req, res) => {
  for await (const _ of req) { /* consume the request */ }
  if (req.method === 'GET' && req.url === '/v1/models') {
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ object: 'list', data: [{ id: 'sidebar-acceptance', object: 'model' }] }));
    return;
  }
  res.writeHead(200, { 'content-type': 'text/event-stream' });
  res.end(`data: ${JSON.stringify({ choices: [{ delta: { role: 'assistant', content: responseText }, finish_reason: null }] })}\n\n`
    + 'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
});
const manifest = JSON.parse(await readFile('docs/plugin-validation/phase-2/manifest.json', 'utf8'));
const { name, version, sha256, integrity } = manifest.rightPaneCandidates.find(item => item.name === 'dsh-better-sidebar');
const digest = async file => createHash('sha256').update(await readFile(file)).digest('hex');
const build = JSON.parse(await readFile(join(resources, 'browser-capability-manifest.json')));
const results = {
  platform: 'macOS arm64', electron: build.electronVersion,
  daemonSha256: await digest(join(resources, 'daemon/magi-daemon-app')),
  readerSha256: await digest(join(resources, 'daemon/dsh/runtime/session-reader.mjs')),
  clientSha256: await digest(join(resources, 'daemon/dsh/runtime/dist/dsh-client-host.js')),
  rendererEntrySha256: await digest(join(resources, 'web/dist/web.html')),
  sdkLockSha256: await digest(join(resources, 'daemon/dsh/package-lock.json')),
  sample: { name, version, sha256, integrity }, checks: {},
};
assert.equal(results.daemonSha256, build.components.daemon.sha256);
let app, page, frame;
const errors = [];
const frameContexts = new Map();

async function transport(path, body) {
  const res = await fetch(origin + path, {
    ...(body === undefined ? {} : { method: 'POST', body: JSON.stringify(body) }),
    headers: { origin, 'content-type': 'application/json' }, signal: AbortSignal.timeout(90_000),
  });
  const raw = await res.text();
  return { status: res.status, body: raw ? JSON.parse(raw) : null };
}
async function request(path, body) {
  const res = await transport(path, body);
  assert.equal(res.status, 200, path + ' ' + JSON.stringify(res.body));
  return res.body;
}
const pluginPath = '/api/plugins/' + encodeURIComponent(name);
const currentPlugin = async () => (await request('/api/plugins')).plugins.find(item => item.manifest.id === name);
const pluginRoute = (plugin, sessionId, endpoint, body) => transport(pluginPath + '/routes', {
  method: 'POST', path: '/sidebar/api/' + endpoint, expectedDigest: plugin.digest,
  expectedInstanceId: plugin.runtime.instanceId, sessionId, body,
});

async function start() {
  assert.equal(await fetch(origin + '/health').then(() => true, () => false), false, 'isolated port');
  app = launchApp({ executable: join(bundle, 'MacOS/Magi'), cwd: run, cdpPort, env: {
    DSH_HOME: join(run, 'dsh'), MAGI_STATE_ROOT: run, MAGI_HOST: '127.0.0.1', MAGI_DESKTOP_DAEMON_PORT: String(port),
    MAGI_OPEN_BROWSER: '0', MAGI_DESKTOP_REUSE_DAEMON: '', MAGI_EXTENSION_NODE: '', MAGI_DSH_SDK_PROJECT: '',
    MAGI_NPM_REGISTRY: 'https://registry.npmjs.org',
  } });
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), 'isolated Electron renderer', 60_000);
  frameContexts.clear();
  page.on('Runtime.executionContextsCleared', () => frameContexts.clear());
  page.on('Runtime.executionContextCreated', ({ context }) => {
    if (context.auxData?.isDefault && context.auxData.frameId) {
      frameContexts.set(context.auxData.frameId, context.id);
    }
  });
  page.on('Runtime.executionContextDestroyed', ({ executionContextId }) => {
    for (const [id, context] of frameContexts) {
      if (executionContextId === context) frameContexts.delete(id);
    }
  });
  await waitFor(() => pageTools(page).exists('[data-testid="sidebar-settings"]'), 'Magi settings');
}

async function stop() {
  frame?.close();
  frame = undefined;
  page?.close();
  page = undefined;
  if (app) await app.stop();
  app = undefined;
}

async function sidebarFrame() {
  frame?.close();
  const selector = `.right-pane-plugin-tab-host:not([hidden]) iframe[data-plugin-id=${JSON.stringify(name)}]`;
  frame = await waitFor(async () => {
    const { root } = await page.call('DOM.getDocument');
    const { nodeId } = await page.call('DOM.querySelector', { nodeId: root.nodeId, selector });
    if (!nodeId) return null;
    const { node } = await page.call('DOM.describeNode', { nodeId });
    if (!node.frameId) return null;
    const outOfProcess = await connectTarget(cdpPort, target => target.id === node.frameId, 'original plugin iframe', errors)
      .catch(() => null);
    if (outOfProcess) return outOfProcess;
    const contextId = frameContexts.get(node.frameId);
    if (contextId === undefined) return null;
    const document = await page.call('Runtime.evaluate', { contextId, expression: 'location.href', returnByValue: true }).catch(() => null);
    if (!document?.result?.value?.includes('/ui-v/')) return null;
    return {
      async evaluate(expression) {
        const res = await page.call('Runtime.evaluate', { contextId, expression, awaitPromise: true, returnByValue: true });
        if (res.exceptionDetails) throw new Error(res.exceptionDetails.exception?.description || res.exceptionDetails.text);
        return res.result.value;
      },
      close() {},
    };
  }, 'visible original right-pane iframe');
  return frame;
}

let failure;
try {
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  await start();
  const preview = await request('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: name, version });
  assert.equal(preview.candidate.digest, sha256);
  assert.equal(preview.integrity, integrity);
  await installFixedUpstreamFromNative({ page, request, packageName: name, version });
  const plugin = await waitFor(async () => {
    const current = await currentPlugin();
    return current?.runtime?.state === 'running' && current.manifest.contributions.views?.some(view =>
      view.placements.includes('rightPane') && view.priority != null) && current;
  }, 'original plugin registered Magi right-pane entries', 30000);
  results.checks.activation = {
    instanceId: plugin.runtime.instanceId,
    runtime: plugin.runtime.state,
    views: plugin.manifest.contributions.views.filter(view => view.placements.includes('rightPane')).map(view => view.title),
    unmountedSlots: plugin.runtime.unmountedSlots,
  };
  const installedRow = `[data-testid="plugin-row"][data-plugin-id=${JSON.stringify(name)}]`;
  await waitFor(() => pageTools(page).exists(installedRow), 'installed plugin row appears');
  await pageTools(page).click(installedRow);
  await waitFor(() => page.evaluate(`(() => {
    const notice = document.querySelector('[data-testid="plugin-unmounted-slots"]');
    return Boolean(notice?.getClientRects().length)
      && notice.innerText.includes('conversation.session.header.utilities')
      // 页面宿主已挂载右栏标签标题槽位：它不再属于"不可用"，设置页不得再报告它。
      && !notice.innerText.includes('sidebar.right.pane.tab.title');
  })()`), 'settings visibly reports the unmounted conversation slot and no longer the mounted tab-title slot');
  results.checks.unmountedSlotsDisclosed = true;

  const { workspaceId } = await request('/api/workspaces/register', { path: workspace });
  const { sessionId } = await request('/api/session/materialize', { scope: 'workspace', workspaceId });
  const { sessionId: otherSessionId } = await request('/api/session/materialize', { scope: 'personal' });
  const phase = (instance, id = sessionId) => pluginRoute(instance, id, 'session.phase', { sessionId: id });
  const before = await phase(plugin);
  assert.equal(before.body.body.value.blank, true);
  const cwd = await pluginRoute(plugin, sessionId, 'session.cwd', { sessionId, cwd: join(run, 'untrusted-cwd') });
  assert.equal(await readFile(join(cwd.body.body.value.cwd, filename), 'utf8'), original);
  const mismatch = await pluginRoute(plugin, sessionId, 'session.phase', { sessionId: otherSessionId });
  assert.notEqual(mismatch.body?.body?.ok, true);
  results.checks.routeScope = { actualWorkspace: true, mismatchedSessionDenied: true };

  await request('/api/settings/orchestrator/save', { expectedRevision: 0, sources: [{
    id: 'sidebar-acceptance', name: 'Sidebar acceptance', baseUrl: 'http://127.0.0.1:' + provider.address().port + '/v1',
    apiKey: 'isolated-test-placeholder', apiProtocol: 'openai_chat', models: [{ id: 'sidebar-acceptance' }],
  }] });
  await request('/api/settings/orchestrator/session/save', { scope: 'workspace', workspaceId, sessionId,
    config: { sourceId: 'custom/sidebar-acceptance', model: 'sidebar-acceptance', reasoningEffort: null } });
  const turn = await request('/api/session/turn', { scope: 'workspace', workspaceId, sessionId,
    text: 'sidebar accepted history', requestId: randomUUID(), userMessageId: randomUUID() });
  const workspaceQuery = new URLSearchParams({ scope: 'workspace', workspaceId, sessionId });
  await waitFor(async () => (await request('/api/messages?' + workspaceQuery)).canonicalTurns
    ?.some(item => item.turnId === turn.turnId && item.status === 'completed'), 'canonical turn completed', 30000);
  const after = await phase(plugin);
  assert.equal(after.body.body.value.blank, false);
  results.checks.committedHistory = { blankBeforeTurn: true, blankAfterTurn: false };

  const sessionURL = new URL(origin + '/web.html');
  sessionURL.search = new URLSearchParams({ scope: 'workspace', workspaceId, workspacePath: workspace, sessionId });
  await page.call('Page.navigate', { url: sessionURL.href });
  const ui = pageTools(page);
  await waitFor(() => page.evaluate(`document.body.innerText.includes(${JSON.stringify(responseText)})`), 'canonical reply is visible in Magi');
  await page.evaluate(`(() => {
    const trace = window.__sidebarTrace = [];
    let previous;
    new MutationObserver(() => {
      const next = JSON.stringify([...document.querySelectorAll('.right-pane-tab')].map(e => [e.dataset.tabId, e.className]));
      if (next !== previous) { trace.push(next); previous = next; }
    }).observe(document.body, { subtree: true, childList: true, attributes: true });
    window.addEventListener('message', e => {
      if (['view.close', 'slots.report', 'view.open'].includes(e.data?.operation)) {
        trace.push(JSON.stringify({ op: e.data.operation, keys: e.data.rightPaneTabs?.map(t => t.key), tabId: e.data.tabId }));
      }
    });
  })()`);
  const paneToggle = '.header-right-pane-btn';
  const clickPaneToggle = async () => {
    const point = await page.evaluate(`(() => {
      const rect = document.querySelector(${JSON.stringify(paneToggle)}).getBoundingClientRect();
      return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    })()`);
    await page.call('Input.dispatchMouseEvent', { type: 'mousePressed', button: 'left', clickCount: 1, ...point });
    await page.call('Input.dispatchMouseEvent', { type: 'mouseReleased', button: 'left', clickCount: 1, ...point });
  };
  await waitFor(() => ui.exists(paneToggle), 'Magi right-pane toggle');
  if (await ui.exists(`${paneToggle}[aria-expanded="false"]`)) await ui.click(paneToggle);
  await waitFor(() => ui.exists('.right-pane-add-tab'), 'Magi right-pane add button');
  await ui.click('.right-pane-add-tab');
  const launcherFileButton = `button[data-tool-key=${JSON.stringify('plugin:' + name + ':' + name + ':files')}]`;
  await waitFor(() => ui.exists(launcherFileButton), 'original Files contribution in Magi launcher');
  await ui.click(launcherFileButton);
  await waitFor(() => ui.exists('.right-pane-tab.active'), 'right pane displays selected tab');
  const fileButton = `[role="button"][title$=${JSON.stringify(filename)}]`;
  await waitFor(async () => {
    if (!frame) await sidebarFrame();
    try { return await pageTools(frame).exists(fileButton); }
    catch (error) {
      frame.close();
      frame = undefined;
      throw error;
    }
  }, 'original file explorer renders real directory');
  await pageTools(frame).click(fileButton);
  results.checks.fileExplorer = { filename, originalRow: true };
  await waitFor(() => ui.exists(`.right-pane-tab.active[title=${JSON.stringify(filename)}]`), 'original navigation opens a separate Magi file tab');
  await sidebarFrame();
  const editor = '.cm-content[contenteditable="true"]';
  await waitFor(() => frame.evaluate(`document.querySelector(${JSON.stringify(editor)})?.textContent.includes(${JSON.stringify(original)})`), 'original editor reads file');
  const editorDocumentId = randomUUID();
  await frame.evaluate(`(() => {
    document.documentElement.dataset.acceptanceEditor = ${JSON.stringify(editorDocumentId)};
    const editor = document.querySelector(${JSON.stringify(editor)});
    editor.focus();
    const range = new Range();
    range.selectNodeContents(editor);
    window.getSelection().removeAllRanges();
    window.getSelection().addRange(range);
    document.execCommand('insertText', false, ${JSON.stringify(edited)});
  })()`);
  await waitFor(() => frame.evaluate(`document.querySelector(${JSON.stringify(editor)})?.textContent.includes(${JSON.stringify(edited)})`), 'editor applies input');
  assert.equal(await readFile(join(workspace, filename), 'utf8'), original, 'typing alone does not save');
  await clickPaneToggle();
  await waitFor(() => ui.exists(`${paneToggle}[aria-expanded="false"]`), 'Magi collapses the right pane');
  const collapsedFrames = await page.evaluate(`(() => {
    const frames = [...document.querySelectorAll('.right-pane-plugin-tab-host iframe')];
    return { count: frames.length, outsideLayout: frames.every(node => node.getClientRects().length === 0),
      focusOnToggle: document.activeElement?.matches(${JSON.stringify(paneToggle)}) === true };
  })()`);
  assert.ok(collapsedFrames.count > 0 && collapsedFrames.outsideLayout && collapsedFrames.focusOnToggle,
    JSON.stringify(collapsedFrames));
  await clickPaneToggle();
  await waitFor(() => ui.exists(`${paneToggle}[aria-expanded="true"]`), 'Magi reopens the right pane');
  await sidebarFrame();
  assert.equal(await frame.evaluate('document.documentElement.dataset.acceptanceEditor'), editorDocumentId,
    'collapsing the right pane must preserve the original editor document');
  assert.ok(await frame.evaluate(`document.querySelector(${JSON.stringify(editor)})?.textContent.includes(${JSON.stringify(edited)})`));
  assert.equal(await readFile(join(workspace, filename), 'utf8'), original);
  results.checks.collapsedEditor = {
    hiddenOutsideLayoutAndFocus: true,
    originalDocumentRetained: true,
    unsavedTextRetained: true,
    fileNotWritten: true,
  };
  await waitFor(() => frame.evaluate(`(() => {
    const button = [...document.querySelectorAll('button[aria-label="保存"],button[aria-label="Save"]')]
      .find(item => {
        const rect = item.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0
          && item.contains(document.elementFromPoint(rect.x + rect.width / 2, rect.y + rect.height / 2));
      });
    if (!button) return false;
    button.click();
    return true;
  })()`), 'original Save action is visible and reachable');
  await waitFor(async () => (await readFile(join(workspace, filename), 'utf8')) === edited, 'original Save button writes to disk');
  assert.ok(await ui.exists('[data-testid="input-textarea"]'));
  assert.ok(await page.evaluate(`document.body.innerText.includes(${JSON.stringify(responseText)})`));
  results.checks.fileEditor = { editedOnDisk: true, nativeComposerVisible: true, nativeReplyVisible: true };
  await writeFile(join(output, 'editor.png'), Buffer.from(await ui.screenshot(), 'base64'));
  await ui.click('.right-pane-tab.active .right-pane-tab-close');
  await waitFor(async () => !(await ui.exists(`.right-pane-tab[title=${JSON.stringify(filename)}]`)), 'Magi closes the editor tab');
  results.checks.closeEditor = true;
  await ui.click(paneToggle);
  await waitFor(() => ui.exists(`${paneToggle}[aria-expanded="false"]`), 'Magi hides the still-open file browser');
  await request(pluginPath + '/disable', { expectedDigest: plugin.digest });
  await waitFor(async () => !(await ui.exists(`iframe[data-plugin-id=${JSON.stringify(name)}]`)), 'disabling removes plugin surfaces');
  const disabled = await phase(plugin);
  assert.equal(disabled.status, 403);
  await request(pluginPath + '/enable', { expectedDigest: plugin.digest });
  const enabled = await waitFor(async () => {
    const current = await currentPlugin();
    return current?.runtime?.state === 'running' && current.runtime.instanceId !== plugin.runtime.instanceId && current;
  }, 'original plugin active with a new instance');
  assert.equal((await phase(plugin)).status, 409);
  assert.equal((await transport(pluginPath + '/slots', { instanceId: plugin.runtime.instanceId })).status, 409);
  assert.equal((await phase(enabled)).body.body.value.blank, false);
  results.checks.instanceRevocation = { removesCollapsedDocuments: true, stoppedRoute: 403, obsoleteRoute: 409, obsoleteSlotReport: 409 };
  const owner = JSON.parse(await readFile(join(run, 'plugins/state.json')));
  const lock = join(owner.loading.configuration.preparation.projectDirectory, 'package-lock.json');
  await cp(lock, join(output, 'package-lock.json'));
  results.installLockSha256 = await digest(lock);
  await stop();
  await start();
  const restored = await waitFor(async () => {
    const current = await currentPlugin();
    return current?.runtime?.state === 'running' && current;
  }, 'original plugin active after application restart');
  const afterRestart = await phase(restored);
  assert.equal(afterRestart.body.body.value.blank, false);
  const file = await pluginRoute(restored, sessionId, 'fs.read', { sessionId, path: filename });
  assert.ok(JSON.stringify(file.body.body).includes(edited));
  results.checks.restart = { committedHistory: true, editedFileReadBackByOriginalPlugin: true };
} catch (error) {
  failure = error;
  results.error = String(error);
  results.diagnostics = [...errors, ...(app?.log.join('').split('\n') ?? [])]
    .filter(line => /sidebar|session|error|TypeError|ReferenceError/i.test(line)).slice(-30).map(line => line.slice(0, 600));
  if (frame) results.pluginText = await frame.evaluate('document.body.innerText.slice(0, 1000)').catch(() => undefined);
  if (page) results.mainText = await page.evaluate('document.body.innerText.slice(-1600)').catch(() => undefined);
  if (page) results.frames = await page.evaluate(`[...document.querySelectorAll('iframe[data-plugin-id]')].map(i => ({ parent: i.parentElement.className, visible: i.getClientRects().length > 0 }))`).catch(() => undefined);
  if (page) results.layout = await page.evaluate('window.magiDesktop.getSnapshot().then(s => s.layout)').catch(() => undefined);
  if (page) results.trace = await page.evaluate('window.__sidebarTrace?.slice(-30)').catch(() => undefined);
  results.finalViews = await currentPlugin().then(p => p?.manifest.contributions.views).catch(() => undefined);
} finally {
  await writeFile(join(output, 'result.json'), JSON.stringify(results, null, 2) + '\n');
  await stop();
  await new Promise(resolve => provider.close(resolve));
}
console.log(JSON.stringify(results, null, 2));
if (failure) throw failure;
