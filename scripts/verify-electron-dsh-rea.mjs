// 在线原包验收：真实 macOS Magi/daemon/Node，REA 前置程序由调用方准备；不改写原插件。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, writeFile, readFile, cp, rm } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { tmpdir, homedir } from 'node:os';
import { delimiter, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';
import { acceptanceChildrenOf, acceptanceHostPid, waitForInstalledPlugin, previewFixedUpstreamFromNative, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const reaBin = process.env.MAGI_REA_BIN_DIR || join(root, 'target/plugin-acceptance/rea-prerequisite/node_modules/.bin');
const port = Number(process.env.MAGI_REA_PORT ?? 38940);
const cdpPort = Number(process.env.MAGI_REA_CDP_PORT ?? 9310), origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-rea-live-'));
const stateRoot = join(work, 'state');
const dshHome = join(stateRoot, 'plugins', 'dsh-home');
const baseline = JSON.parse(await readFile(new URL('../docs/plugin-validation/phase-1/manifest.json', import.meta.url)));
const sample = baseline.packages.find(row => row.name === 'dsh-rea' && row.version === '1.2.0');
const output = process.env.MAGI_REA_OUTPUT || join(root, 'target/plugin-acceptance/dsh-rea');
const business = process.env.MAGI_REA_BUSINESS === '1';
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
const timeoutValue = () => frame.evaluate('document.getElementById("plugin-config-dsh-rea-timeout")?.value');
const committedTimeout = async () => (await readFile(join(stateRoot, 'plugins', 'loading-source', 'cordis.patch.yml'), 'utf8'))
  .match(/^ +toolCallTimeoutMs: (\d+)$/m)?.[1];
const frameSource = () => page.evaluate('document.querySelector(".settings-frame:not([hidden]) iframe")?.src');
async function openFrame() {
  frame?.close();
  const url = await waitFor(frameSource, '原包设置 iframe');
  frame = await waitFor(() => connectTarget(cdpPort, target => ['iframe', 'page'].includes(target.type) && target.url === url,
    'REA 设置', errors), '原包设置上下文');
  await waitFor(timeoutValue, '上游原包设置表单', 60_000);
}
async function verifyAnalysis() {
  // 本地待分析应用是业务输入；不加载样本代码，不替换 REA、MCP 或模型结果。
  const directory = join(work, 'sample-application'), channel = 'rea-audit:read-version';
  await mkdir(directory);
  const sources = {
    'package.json': JSON.stringify({ name: 'rea-analysis-sample', version: '1.0.0', main: 'main.cjs' }),
    'main.cjs': `const {app,BrowserWindow,ipcMain}=require('electron');\nconst path=require('node:path');\nipcMain.handle('${channel}',()=>app.getVersion());\napp.whenReady().then(()=>{const win=new BrowserWindow({webPreferences:{contextIsolation:true,nodeIntegration:false,preload:path.join(__dirname,'preload.cjs')}});win.loadFile('index.html');});\n`,
    'preload.cjs': `const {contextBridge,ipcRenderer}=require('electron');\ncontextBridge.exposeInMainWorld('sampleApi',{readVersion:()=>ipcRenderer.invoke('${channel}')});\n`,
    'index.html': '<!doctype html><title>REA sample</title><button id="version">Read version</button>',
  };
  for (const [name, content] of Object.entries(sources)) await writeFile(join(directory, name), content);
  const sourceDigests = Object.fromEntries(Object.entries(sources).map(([name, content]) => [name, createHash('sha256').update(content).digest('hex')]));
  const request = { ecosystem: 'dsh', package: 'dsh-connect-workbuddy', version: '5.21.2' };
  const preview = await api('/api/plugins/upstream/preview', request);
  const locked = baseline.packages.find(row => row.name === request.package && row.version === request.version);
  assert.equal(preview.candidate.digest, locked.sha256);
  assert.equal(preview.integrity, locked.integrity);
  const buddy = (await api('/api/plugins/upstream/install', { ...request, expectedDigest: preview.candidate.digest })).plugins.find(item => item.manifest.id === preview.candidate.manifest.id);
  await settings('plugins');
  await page.evaluate(`(()=>{const input=document.querySelector('[data-testid=plugin-search]');input.value='';input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  await waitForInstalledPlugin({ request: api, page, plugin: buddy });
  // 固定验收使用 WorkBuddy 的 workbuddy 提供方。workbuddy-global 有独立目录和登录态。
  const sourceId = `plugin/${buddy.manifest.id}/workbuddy`;
  const model = await waitForPluginModel({ request: api, pluginId: buddy.manifest.id, sourceId,
    record: value => writeFile(join(output, 'model-selection.json'), JSON.stringify(value, null, 2)) });
  const original = await installed(), tool = original.manifest.contributions.tools.find(item => item.title === 'mcp__rea__analyze_javascript_application');
  await writeFile(join(output, 'analysis-plugin.json'), JSON.stringify(original, null, 2));
  assert.ok(tool, '真实 REA 目录须包含静态 JavaScript 应用分析工具');
  const modelToolName = tool.title;
  await writeFile(join(output, 'analysis-input.json'), JSON.stringify({ pluginId, version: original.manifest.version, digest: original.digest, instanceId: original.runtime.instanceId,
    tool, model: { sourceId: model.sourceId, id: model.id }, directory, sourceDigests, channel }, null, 2));
  const beginAnalysis = (session, requestId) => api('/api/session/turn', { scope: 'personal', sessionId: session,
    text: `Discover the REA tool ${modelToolName} using tool_catalog, then call it once on the local directory ${directory} with input_path=${JSON.stringify(directory)}, format="directory", integrity_policy="fail", detail="complete". From the analysis result, report the literal IPC channel and preload API/member. If the displayed analysis is truncated or the tool fails, report that without retrying. Do not run other analysis tools or launch the application.`,
    requestId, userMessageId: requestId + '-user' });
  const readTurn = async (session, turn) => (await api(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns.find(item => item.turnId === turn.turnId);
  const terminal = current => ['completed', 'failed', 'cancelled', 'blocked'].includes(current?.status);
  async function runAnalysis(requestId, whileRunning) {
    const session = (await api('/api/session/materialize', { scope: 'personal' })).sessionId;
    await api('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session,
      config: { sourceId: model.sourceId, model: model.id, reasoningEffort: null } });
    await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${session}` });
    await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '真实分析对话');
    const turn = await beginAnalysis(session, requestId);
    const approved = new Set();
    let acted = false;
    const result = await waitFor(async () => {
      const current = await readTurn(session, turn);
      if (current) await writeFile(join(output, requestId + '-turn.json'), JSON.stringify(current, null, 2));
      for (const pending of (await api(`/api/session/tool-approvals?sessionId=${session}`)).pendingApprovals) {
        if (pending.turnId !== turn.turnId || approved.has(pending.approvalId)) continue;
        const call = current?.items.find(item => item.tool?.callId === pending.toolCallId)?.tool;
        assert.equal(pending.toolName, modelToolName, '只批准本次本地样本的静态分析');
        assert.equal(call?.arguments.format, 'directory');
        assert.equal(call?.arguments.integrity_policy, 'fail');
        assert.equal(call?.arguments.detail, 'complete');
        assert.ok([directory, '[path]'].includes(call?.arguments.input_path), JSON.stringify(call?.arguments));
        await waitFor(() => page.evaluate(`(()=>{const card=[...document.querySelectorAll('[data-tool-call-id]')].find(node=>node.dataset.toolCallId===${JSON.stringify(pending.toolCallId)});const buttons=[...(card?.querySelectorAll('.tool-approval .btn--primary')??[])].filter(button=>!button.disabled);if(buttons.length!==1)return false;buttons[0].click();return true;})()`), '原生审批执行 REA 分析');
        approved.add(pending.approvalId);
      }
      if (whileRunning && !acted && approved.size > 0
          && current?.items.some(item => item.tool?.name === modelToolName && item.status === 'running')) {
        acted = true;
        await whileRunning(current, session);
      }
      return terminal(current) && current;
    }, '真实模型、审批和 REA 分析业务', 240_000);
    assert.equal(approved.size, 1, `每场景在独立会话实际批准一次分析（轮次终态：${result.status}）`);
    assert.ok(!whileRunning || acted, '分析调用必须进入执行后才能结算故障场景');
    return { session, turn, result };
  }
  const { session, turn, result } = await runAnalysis('rea-analysis-business');
  const calls = result.items.filter(item => item.kind === 'tool_call' && item.tool?.name !== 'tool_catalog');
  assert.equal(result.status, 'completed', result.items.filter(item => item.status === 'failed').map(item => item.content).join('\n'));
  assert.deepEqual(calls.map(call => ({ name: call.tool.name, status: call.status })), [
    { name: modelToolName, status: 'completed' },
  ]);
  const receipt = calls[0].tool.result;
  const task = await api(`/api/agent-runs/projection/${turn.rootTaskId}?scope=personal&sessionId=${session}`);
  await writeFile(join(output, 'analysis-task.json'), JSON.stringify(task, null, 2));
  await writeFile(join(output, 'analysis-tool-receipt.json'), JSON.stringify(receipt, null, 2));
  const raw = JSON.stringify(receipt);
  report.check('真实模型经原生 Task 与原生审批执行 REA 原包分析，而非只枚举工具', task.root_task.status === 'completed' && result.modelSteps.length >= 2 && !receipt.isError, raw.slice(0, 500));
  report.check('原始 REA 结果包含样本 IPC、桥 API 与源文件 SHA-256', raw.includes(channel) && raw.includes('sampleApi') && raw.includes('readVersion')
    && raw.includes(sourceDigests['main.cjs']) && raw.includes(sourceDigests['preload.cjs']), raw.slice(0, 500));
  const answer = result.items.filter(item => item.kind === 'assistant_text').map(item => item.content ?? '').join('\n');
  report.check('真实模型根据 REA 原始结果给出事实或明确报告结果截断',
    answer.includes(channel) && (answer.includes('sampleApi') && answer.includes('readVersion')
      || /truncat|截断|unavailable|不可用/i.test(answer)));
  for (const [name, content] of Object.entries(sources)) assert.equal(await readFile(join(directory, name), 'utf8'), content);
  report.check('静态分析保留样本原字节，插件在独立新会话无需再次启用', (await installed()).digest === original.digest && (await installed()).runtime.instanceId === original.runtime.instanceId);
  await snapshot('analysis-conversation');

  const toolCalls = current => current.items.filter(item => item.tool?.name === modelToolName);
  const saveDispatch = (name, current) => writeFile(join(output, name + '.json'),
    JSON.stringify({ turnId: current.turnId, status: current.status, calls: toolCalls(current) }, null, 2));
  async function withPausedRea(run) {
    const host = acceptanceHostPid(app.child.pid);
    const children = acceptanceChildrenOf(host, 'rea');
    const servers = acceptanceChildrenOf(host, '--mcp').filter(pid => children.includes(pid));
    assert.equal(servers.length, 1, '只暂停本轮共享宿主的真实 REA MCP 子进程');
    const [server] = servers;
    process.kill(server, 'SIGSTOP');
    let paused = true;
    const resume = () => {
      if (!paused) return;
      paused = false;
      try { process.kill(server, 'SIGCONT'); }
      catch (error) { if (error.code !== 'ESRCH') throw error; }
    };
    try {
      return await run(host, resume);
    } finally {
      resume();
    }
  }
  const cancelled = await withPausedRea(() => runAnalysis('rea-analysis-cancel', async (current, session) => {
    await saveDispatch('cancel-dispatch', current);
    await api('/api/session/interrupt', { sessionId: session });
  }));
  report.check('REA 分析在执行中可取消', cancelled.result.status === 'cancelled'
    && toolCalls(cancelled.result).length === 1 && toolCalls(cancelled.result)[0].status === 'cancelled');
  const crashed = await withPausedRea((host, resume) => runAnalysis('rea-analysis-crash', async current => {
    await saveDispatch('crash-dispatch', current);
    process.kill(host, 'SIGKILL');
    resume();
  }));
  const failedCalls = toolCalls(crashed.result);
  report.check('REA 分析在共享宿主崩溃后结算且不重放', failedCalls.length === 1
    && failedCalls[0].status === 'failed' && failedCalls[0].tool.result?.error_code === 'plugin_tool_rpc_error');
  const recovered = await waitFor(async () => {
    const row = await installed();
    return row?.runtime?.state === 'running' && row.runtime.instanceId !== original.runtime.instanceId
      && row.manifest.contributions.tools.some(item => item.title === modelToolName) && row;
  }, '恢复 REA 原包和工具目录', 90_000);
  const recoveredModel = await waitForPluginModel({ request: api, pluginId: buddy.manifest.id, sourceId, record: async () => {} });
  assert.equal(recoveredModel.sourceId, model.sourceId);
  const retry = await runAnalysis('rea-analysis-recovered');
  report.check('宿主恢复后能够重新完成 REA 分析，历史取消终态保持', recovered.digest === original.digest
    && retry.result.status === 'completed' && JSON.stringify(toolCalls(retry.result)[0]?.tool.result).includes(channel)
    && (await readTurn(cancelled.session, cancelled.turn)).status === 'cancelled');
}
async function start() {
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    env: { MAGI_STATE_ROOT: stateRoot, MAGI_HOST: '127.0.0.1', MAGI_DESKTOP_DAEMON_PORT: String(port),
      MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_DSH_SDK_ROOT: '', MAGI_DESKTOP_REUSE_DAEMON: '', PATH: `${reaBin}${delimiter}${process.env.PATH}` } });
  page = await waitFor(async () => {
    const candidate = await connectAppPage(cdpPort, port, errors);
    if (await pageTools(candidate).exists('[data-testid=sidebar-settings]').catch(() => false)) return candidate;
    candidate.close();
    return null;
  }, '真实 Magi 主窗口工作台', 60_000);
  ui = pageTools(page);
}
async function stop() {
  frame?.close(); frame = undefined;
  page?.close(); page = undefined;
  if (app) { await app.stop(); app = undefined; }
}
try {
  report.check('独立验收端口空闲', !await fetch(origin + '/health').then(() => true, () => false)
    && !await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false));
  if (business) {
    await mkdir(dshHome, { mode: 0o700, recursive: true });
    await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(dshHome, '.workbuddy-device.json'));
  }
  await start(); await settings('plugins');
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
  const fixed = await api('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: sample.name, version: sample.version });
  assert.equal(fixed.candidate.digest, sample.sha256);
  assert.equal(fixed.integrity, sample.integrity);
  await previewFixedUpstreamFromNative({ page, packageName: sample.name, version: sample.version });
  const preview = await waitFor(async () => {
    if (await ui.exists('[data-testid=plugin-candidate]')) return { ok: true };
    const error = await ui.text('[data-testid=plugin-message]'); return error && { error };
  }, '真实原包安装预览', 180_000);
  assert.ok(preview.ok, preview.error);
  report.check('在线原包预览展示完整性摘要', /sha512-/.test(await ui.text('[data-testid=plugin-candidate]')));
  await ui.click('[data-action=confirm-candidate]');
  const plugin = await waitFor(installed, '原包安装完成', 180_000); pluginId = plugin.manifest.id;
  assert.equal(plugin.manifest.version, '1.2.0');
  report.check('安装保留原包 SVG 和在线来源', plugin.source.kind === 'upstream');
  const icon = await fetch(`${origin}/api/plugins/${pluginId}/icon`);
  report.check('安装前后 SVG 原字节一致且直接导航被隔离', await icon.text() === svg
    && icon.headers.get('content-type') === 'image/svg+xml' && icon.headers.get('x-content-type-options') === 'nosniff'
    && icon.headers.get('content-security-policy')?.startsWith('sandbox;'));
  // 安装完成后默认激活，不再需要点击激活入口。
  const running = await waitFor(async () => {
    const item = await installed();
    if (item.runtime?.state === 'failed') throw new Error(item.runtime.reason);
    return item.runtime?.state === 'running' && item.manifest.contributions.tools.length > 128 && item;
  }, '真实 REA MCP 工具目录（需要 rea-agents 前置程序）', 90_000);
  const count = running.manifest.contributions.tools.length;
  report.check('真实 REA 工具突破静态清单的 128 项限制并进入 daemon', count > 128, `${count} tools`);
  await settings(`plugin:${pluginId}:settings`);
  await openFrame();
  // Magi 解析 PATH 时可能把本机已有的 `rea` 排在前置目录之前：页面显示的版本必须是本机可见的某个真实 `rea` 的版本。
  const reaVersions = new Set([reaBin, ...(process.env.PATH ?? '').split(delimiter)].filter(Boolean).flatMap((directory) => {
    const file = join(directory, 'rea');
    if (!existsSync(file)) return [];
    try { return [execFileSync(file, ['--version'], { env: { ...process.env, PATH: `${reaBin}${delimiter}${process.env.PATH}` } }).toString().trim()]; } catch { return []; }
  }));
  const statusCard = await waitFor(() => frame.evaluate(`[...document.querySelectorAll('.dshr-row')]
    .map(row => row.innerText).filter(text => /已安装 \\d+\\.\\d+\\.\\d+/.test(text) || text.includes(${JSON.stringify(String(count))})).join('\\n')`),
    '原页面通过公开连接显示真实 REA 状态');
  const shown = /已安装 (\d+\.\d+\.\d+)/.exec(statusCard)?.[1];
  report.check('REA 版本与工具数由原页面状态卡读取', reaVersions.has(shown) && statusCard.includes(String(count)), `页面 ${shown}，本机可见 ${[...reaVersions].join('/')}，${count} 个工具`);
  report.check('原生 React 设置表单与状态卡片加载，无未提供模块', !await frame.evaluate('document.body.innerText.includes("界面加载失败")'));
  const ghidraLink = await frame.evaluate(`(()=>{const a=[...document.querySelectorAll('a[href]')].find((item)=>item.href.includes('/ghidra/releases'));if(!a)return null;const style=getComputedStyle(a);return {href:a.href,target:a.target,color:style.color,decoration:style.textDecorationLine};})()`);
  report.check('上游 Ghidra 外链保留 href、target 与链接色', ghidraLink?.href === 'https://github.com/NationalSecurityAgency/ghidra/releases'
    && ghidraLink.target === '_blank' && ghidraLink.color !== await frame.evaluate('getComputedStyle(document.body).color'), JSON.stringify(ghidraLink));
  report.check('原生表单默认值与运行期配置一致', await frame.evaluate('document.getElementById("plugin-config-dsh-rea-timeout").value === "300000"'));
  await snapshot('settings');
  await frame.evaluate(`(()=>{const field=document.getElementById('plugin-config-dsh-rea-timeout');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(field,'310000');field.dispatchEvent(new Event('input',{bubbles:true}));field.dispatchEvent(new Event('change',{bubbles:true}));return true;})()`);
  await waitFor(() => frame.evaluate(`(()=>{const b=[...document.querySelectorAll('button')].find(b=>/保存|Save/.test(b.textContent));if(!b||b.disabled)return false;b.click();return true;})()`), '保存原生设置');
  await waitFor(async () => await committedTimeout() === '310000', 'daemon 持久设置');
  await waitFor(async () => (await installed()).manifest.contributions.tools.length === count, 'MCP 目录恢复', 90_000);
  report.check('原生表单保存经 CAS 持久化并触发上游 MCP 配置重载', true);
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  await snapshot('settings-390');
  report.check('390px 原生设置无横向溢出', await page.evaluate('document.documentElement.scrollWidth<=innerWidth') && await frame.evaluate('document.documentElement.scrollWidth<=innerWidth'));
  await page.call('Emulation.clearDeviceMetricsOverride');
  const settingsIdentity = await frame.evaluate('window.__settingsVisitIdentity = crypto.randomUUID()');
  await frame.evaluate(`(()=>{const field=document.getElementById('plugin-config-dsh-rea-timeout');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(field,'320000');field.dispatchEvent(new Event('input',{bubbles:true}));field.dispatchEvent(new Event('change',{bubbles:true}));})()`);
  const scroll = await page.evaluate(`(async () => {
    const frame = document.querySelector('.settings-frame:not([hidden]) iframe');
    window.__reaSettingsFrame = frame;
    const scroller = frame.closest('.settings-frame');
    scroller.style.maxHeight = '240px';
    scroller.style.flex = 'none';
    scroller.scrollTop = 120;
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    return scroller.scrollTop;
  })()`);
  assert.ok(scroll > 0, '设置页必须有真实可恢复的滚动位置');
  await settings('appearance');
  report.check('切换设置分类保留隔离文档并移出可交互区域', await page.evaluate(`window.__reaSettingsFrame.isConnected
    && !window.__reaSettingsFrame.getClientRects().length
    && Boolean(window.__reaSettingsFrame.closest('.plugin-client-parking, .settings-frame[inert]'))`));
  await settings(`plugin:${pluginId}:settings`);
  await waitFor(() => page.evaluate(`window.__reaSettingsFrame.closest('.settings-frame')?.scrollTop === ${scroll}`), '恢复原设置滚动位置');
  await writeFile(join(output,'settings-cache-return.json'), JSON.stringify({expected:{settingsIdentity,scroll},
    document:await frame.evaluate('({identity:window.__settingsVisitIdentity,value:document.getElementById("plugin-config-dsh-rea-timeout").value})'),
    host:await page.evaluate('({same:document.querySelector(".settings-frame:not([hidden]) iframe")===window.__reaSettingsFrame,scroll:window.__reaSettingsFrame.closest(".settings-frame").scrollTop,visible:!!window.__reaSettingsFrame.getClientRects().length})')},null,2));
  report.check('返回插件设置复用同一文档，保留未保存表单与滚动位置',
    await frame.evaluate(`window.__settingsVisitIdentity===${JSON.stringify(settingsIdentity)} && document.getElementById('plugin-config-dsh-rea-timeout').value==='320000'`)
      && await page.evaluate(`document.querySelector('.settings-frame:not([hidden]) iframe')===window.__reaSettingsFrame && window.__reaSettingsFrame.closest('.settings-frame').scrollTop===${scroll}`));
  report.check('设置页保活不会把未提交草稿写成 daemon 配置', await committedTimeout() === '310000');
  await ui.click('[data-testid=settings-back]');
  await waitFor(() => page.evaluate('!document.querySelector("[data-testid=settings-page]").getClientRects().length'), '返回原生对话');
  await settings(`plugin:${pluginId}:settings`);
  report.check('返回对话再打开设置仍保留同一原包表单', await frame.evaluate(`window.__settingsVisitIdentity===${JSON.stringify(settingsIdentity)} && document.getElementById('plugin-config-dsh-rea-timeout').value==='320000'`));
  await ui.click('[data-testid=plugin-page-header] input[role=switch]');
  await waitFor(async () => !(await installed()).enabled, '停用回收');
  report.check('停用清空真实 MCP 动态目录', (await installed()).manifest.contributions.tools.length === 0);
  await waitFor(() => page.evaluate('!window.__reaSettingsFrame.isConnected'), '停用释放已保活的设置文档');
  report.check('全局停用释放设置文档，不把页面保活当成插件继续运行', true);
  await ui.click('[data-testid=plugin-page-header] input[role=switch]');
  await waitFor(async () => (await installed()).manifest.contributions.tools.length === count, '再次激活恢复目录', 90_000);
  report.check('重激活保留配置并恢复真实 MCP 工具', await committedTimeout() === '310000');
  await openFrame();
  await waitFor(() => frame.evaluate('document.getElementById("plugin-config-dsh-rea-timeout")?.value === "310000"'), '新页面读取已提交配置');
  report.check('重新启用创建新文档并读取 daemon 配置，不复活旧页面草稿', await frame.evaluate('window.__settingsVisitIdentity === undefined'));
  if (business) await verifyAnalysis();
  report.check('主窗口和原包 iframe 无未捕获运行异常', errors.length === 0, errors.join('\n'));
  const beforeRestart = await installed();
  await stop();
  await start();
  const restored = await waitFor(async () => {
    const row = await installed();
    return row?.runtime?.state === 'running' && row.manifest.contributions.tools.length === count && row;
  }, '完整应用重启恢复工具目录', 120_000);
  assert.notEqual(restored.runtime.instanceId, beforeRestart.runtime.instanceId);
  await settings(`plugin:${pluginId}:settings`);
  await openFrame();
  report.check('完整应用重启恢复实际 REA 工具及已提交的设置', await timeoutValue() === '310000');
  const resources = join(appRoot, 'Contents', 'Resources');
  const build = JSON.parse(await readFile(join(resources, 'browser-capability-manifest.json')));
  const state = JSON.parse(await readFile(join(stateRoot, 'plugins', 'state.json')));
  const lock = await readFile(join(state.loading.configuration.preparation.projectDirectory, 'package-lock.json'));
  await writeFile(join(output, 'package-lock.json'), lock);
  await writeFile(join(output, 'result.json'), JSON.stringify({ completed: true, businessVerified: business,
    package: sample.name, version: sample.version, archiveSha256: sample.sha256, integrity: sample.integrity, toolCount: count, reaVersion: shown, visibleReaVersions: [...reaVersions],
    buildIdentity: build.buildIdentity, daemonSha256: build.components.daemon.sha256,
    sdkLockSha256: build.files.find(file => file.path === 'daemon/dsh/package-lock.json').sha256,
    dependenciesSha256: createHash('sha256').update(lock).digest('hex'),
    checks: report.results.map(({ name, ok }) => ({ name, ok })) }, null, 2) + '\n');
  console.log(report.summary());
} catch (error) {
  console.error(error); process.exitCode = 1;
  if (!report.results.some(result => !result.ok)) report.results.push({ name: business ? 'REA 原包分析完整流程' : 'REA 安装与设置完整流程', ok: false, detail: String(error) });
  if (ui) await snapshot('failure').catch(() => {});
  if (frame) await writeFile(join(output, 'frame.txt'), await frame.evaluate('document.body.innerText').catch(() => '')).catch(() => {});
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(report.results, null, 2));
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  frame?.close(); page?.close(); await rm(work, { recursive: true, force: true });
}
