// 在线原包验收：真实 macOS Magi/daemon/Node，REA 前置程序由调用方准备；不改写原插件。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, writeFile, readFile, cp, rm } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir, homedir } from 'node:os';
import { delimiter, dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';
import { authorizePluginFromNative, previewFixedUpstreamFromNative, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const reaBin = process.env.MAGI_REA_BIN_DIR || join(root, 'target/plugin-acceptance/rea-prerequisite/node_modules/.bin');
const port = 38940, cdpPort = 9310, origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-rea-live-'));
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
const status = async () => (await api(`/api/plugins/${pluginId}/routes`, {
  method: 'POST', path: '/__magi_compat/remote', body: { endpoint: 'rea/status', args: { request: {} } },
})).body;
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
  const buddy = (await api('/api/plugins/upstream/install', { ...request, expectedDigest: preview.candidate.digest })).plugins.find(item => item.manifest.id === preview.candidate.manifest.id);
  await settings('plugins');
  await page.evaluate(`(()=>{const input=document.querySelector('[data-testid=plugin-search]');input.value='';input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  await authorizePluginFromNative({ request: api, page, plugin: buddy });
  const model = await waitForPluginModel({ request: api, pluginId: buddy.manifest.id,
    record: value => writeFile(join(output, 'model-selection.json'), JSON.stringify(value, null, 2)) });
  const session = (await api('/api/session/materialize', { scope: 'personal' })).sessionId;
  await api('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session, config: { sourceId: model.sourceId, model: model.id, reasoningEffort: null } });
  const original = await installed(), tool = original.manifest.contributions.tools.find(item => item.title === 'mcp__rea__analyze_javascript_application');
  await writeFile(join(output, 'analysis-plugin.json'), JSON.stringify(original, null, 2));
  assert.ok(tool, '真实 REA 目录须包含静态 JavaScript 应用分析工具');
  const modelName = tool => `plugin__${pluginId}__${tool.id}`.replace(/[^a-zA-Z0-9-]/g, '_').slice(0, 51)
    + '_' + createHash('sha256').update(`${pluginId}\x1f${tool.id}`).digest('hex').slice(0, 12);
  const modelToolName = modelName(tool);
  const traceTool = original.manifest.contributions.tools.find(item => item.title === 'mcp__rea__trace_application_feature');
  assert.ok(traceTool, '真实 REA 应提供已分析证据的特征追踪');
  const traceName = modelName(traceTool);
  const evidenceTools = new Map(original.manifest.contributions.tools.filter(item => [
    'mcp__rea__analyze_javascript_application', 'mcp__rea__trace_application_feature',
    'mcp__rea__trace_javascript_semantics', 'mcp__rea__inspect_analysis_view',
  ].includes(item.title)).map(item => [modelName(item), item]));
  await writeFile(join(output, 'analysis-input.json'), JSON.stringify({ pluginId, version: original.manifest.version, digest: original.digest, instanceId: original.runtime.instanceId,
    tool, model: { sourceId: model.sourceId, id: model.id }, directory, sourceDigests, channel }, null, 2));
  const turn = await api('/api/session/turn', { scope: 'personal', sessionId: session,
    text: `Use the installed dsh-rea plugin tool ${tool.title} (model tool name ${modelToolName}) to statically analyze the local application directory ${directory}. Call the analyzer exactly once with input_path=${JSON.stringify(directory)}, format="directory", integrity_policy="fail", detail="complete". Use tool_catalog to discover and load the exact input schema before each new tool. If the large result is truncated, use the REA trace_application_feature tool (${traceName}) on that retained Evidence, with seed {kind:"module",value:"preload.cjs",match:"exact",case_sensitive:false}, direction:"outgoing", to read the preload's exposed API. The application/source argument is the direct object {kind:"retained-evidence",evidence_id:<actual parent analyzer evidence_id>}, without a history wrapper. Continue with focused trace_application_feature, trace_javascript_semantics or inspect_analysis_view calls on that same retained Evidence until you establish the literal API and member names. Module paths may have several graph records; inspect the modules page to obtain a unique node_id before selecting an individual module. You can trace all APIs using seed {kind:"api",value:"",match:"exact"} and then narrow to strings actually observed in results. These retained-evidence operations are authorized; do not ask for additional confirmation or guess the names. Do not repeat analysis, execute the application or use shell/file tools. Report the actual literal IPC channel, contextBridge API name and member name from evidence; counts alone are insufficient.`,
    requestId: 'rea-analysis-business', userMessageId: 'rea-analysis-business-user' });
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${session}` });
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '真实分析对话');
  const approved = new Set();
  const result = await waitFor(async () => {
    const current = (await api(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns.find(item => item.turnId === turn.turnId);
    if (current) await writeFile(join(output, 'analysis-turn.json'), JSON.stringify(current, null, 2));
    for (const pending of (await api(`/api/session/tool-approvals?sessionId=${session}`)).pendingApprovals) {
      if (pending.turnId !== turn.turnId || approved.has(pending.approvalId)) continue;
      const call = current?.items.find(item => item.tool?.callId === pending.toolCallId)?.tool;
      assert.ok(evidenceTools.has(pending.toolName), `拒绝批准非指定 REA 工具：${pending.toolName}`);
      if (pending.toolName === modelToolName) {
        assert.equal(call?.arguments.format, 'directory'); assert.equal(call?.arguments.integrity_policy, 'fail'); assert.equal(call?.arguments.detail, 'complete');
        // canonical 公开投影可隐藏绝对路径，实际分析结果及输入摘要在下方核验。
        assert.ok([directory, '[path]'].includes(call?.arguments.input_path), JSON.stringify(call?.arguments));
      } else {
        const source = call?.arguments.application ?? call?.arguments.source;
        assert.equal(source?.kind, 'retained-evidence');
        assert.ok(/^ev_[a-f0-9]{64}$/.test(source.evidence_id));
        assert.ok(JSON.stringify(current.items.find(item => item.tool?.name === modelToolName)?.tool.result).includes(source.evidence_id));
        assert.ok(!call.arguments.native_observations?.length, '不能增加本地样本之外的原生观察');
      }
      await waitFor(() => page.evaluate(`(()=>{const card=[...document.querySelectorAll('[data-tool-call-id]')].find(node=>node.dataset.toolCallId===${JSON.stringify(pending.toolCallId)});const buttons=[...(card?.querySelectorAll('.tool-approval .btn--primary')??[])].filter(button=>!button.disabled);if(buttons.length!==1)return false;buttons[0].click();return true;})()`), '原生审批执行 REA 分析');
      approved.add(pending.approvalId);
    }
    return current && ['completed', 'failed', 'cancelled', 'blocked'].includes(current.status) && current;
  }, '真实模型、审批和 REA 分析业务', 480_000);
  const calls = result.items.filter(item => item.kind === 'tool_call' && evidenceTools.has(item.tool?.name));
  const refusedNames = result.items.filter(item => item.kind === 'tool_call'
    && item.tool.name !== 'tool_catalog' && !evidenceTools.has(item.tool.name));
  assert.equal(result.status, 'completed', result.items.filter(item => item.status === 'failed').map(item => item.content).join('\n'));
  assert.equal(calls.filter(call => call.tool.name === modelToolName).length, 1);
  const receipt = calls.find(call => call.tool.name === modelToolName).tool.result;
  assert.equal(calls.find(call => call.tool.name === modelToolName).status, 'completed', '分析器必须实际成功，不能将重放或失败当作分析完成');
  const recoveries = [];
  for (const call of refusedNames) {
    // 模型名称错误只能在原生准入时拒绝，不能执行目录外工具或靠别名猜测派发。
    const error = call.tool.result;
    assert.equal(call.status, 'failed');
    assert.equal(error?.status, 'rejected');
    assert.equal(error?.stage, 'tool_call_validation');
    assert.equal(error?.error_code, 'tool_not_available');
    assert.equal(error?.expected_input_schema, null);
    const source = call.tool.arguments.application ?? call.tool.arguments.source;
    assert.equal(source?.kind, 'retained-evidence');
    assert.ok(/^ev_[a-f0-9]{64}$/.test(source.evidence_id) && JSON.stringify(receipt).includes(source.evidence_id));
    const next = result.items.slice(result.items.indexOf(call) + 1).find(item => {
      const target = item.tool?.arguments.application ?? item.tool?.arguments.source;
      return item.status === 'completed' && evidenceTools.has(item.tool?.name)
        && target?.kind === 'retained-evidence' && target.evidence_id === source.evidence_id;
    });
    assert.ok(next, '未知名称必须未派发，并在同一证据上由真实已注册工具成功恢复');
    recoveries.push({ callId: call.tool.callId, requestedTool: call.tool.name, code: error.error_code, recoveredBy: next.tool.callId });
  }
  for (const [index, call] of calls.entries()) {
    if (call.status === 'completed') continue;
    assert.equal(call.status, 'failed');
    const error = call.tool.result;
    const validation = error?.error_code === 'tool_arguments_invalid' && error.stage === 'tool_call_validation' && error.status === 'rejected';
    let refusal;
    if (error?.error_code === 'plugin_tool_error' && typeof error.error?.error?.message === 'string') refusal = JSON.parse(error.error.error.message).error;
    assert.ok(validation || refusal?.code === 'invalid_request' && refusal.category === 'invalid_input' && refusal.retryable === true,
      '仅允许有明确回执的输入拒绝；未知执行错误、超时或权限失败不能算作恢复');
    const next = calls.slice(index + 1).find(item => {
      const source = item.tool.arguments.application ?? item.tool.arguments.source;
      return item.status === 'completed' && item.tool.name !== modelToolName && source?.kind === 'retained-evidence'
        && /^ev_[a-f0-9]{64}$/.test(source.evidence_id) && JSON.stringify(receipt).includes(source.evidence_id);
    });
    assert.ok(next, '参数拒绝后必须在同一原分析证据上取得后续成功回执');
    recoveries.push({ callId: call.tool.callId, code: validation ? error.error_code : refusal.code, recoveredBy: next.tool.callId });
  }
  // 准入校验拒绝发生在审批／派发前；实际进入原包的输入错误仍须有审批记录。
  assert.equal(approved.size, calls.filter(call => !evidenceTools.get(call.tool.name).readOnly && call.tool.result?.error_code !== 'tool_arguments_invalid').length);
  await writeFile(join(output, 'analysis-recoveries.json'), JSON.stringify(recoveries, null, 2));
  const task = await api(`/api/agent-runs/projection/${turn.rootTaskId}?scope=personal&sessionId=${session}`);
  await writeFile(join(output, 'analysis-task.json'), JSON.stringify(task, null, 2));
  await writeFile(join(output, 'analysis-tool-receipt.json'), JSON.stringify(receipt, null, 2));
  const raw = JSON.stringify(receipt);
  report.check('真实模型经原生 Task 与原生审批执行 REA 原包分析，而非只枚举工具', task.root_task.status === 'completed' && result.modelSteps.length >= 2 && !receipt.isError, raw.slice(0, 500));
  report.check('原始 REA 结果包含样本 IPC、桥 API 与源文件 SHA-256', raw.includes(channel) && raw.includes('sampleApi') && raw.includes('readVersion')
    && raw.includes(sourceDigests['main.cjs']) && raw.includes(sourceDigests['preload.cjs']), raw.slice(0, 500));
  const answer = result.items.filter(item => item.kind === 'assistant_text').map(item => item.content ?? '').join('\n');
  report.check('真实模型根据 REA 原始结果给出实际 IPC 和 API', answer.includes(channel) && answer.includes('sampleApi') && answer.includes('readVersion'));
  for (const [name, content] of Object.entries(sources)) assert.equal(await readFile(join(directory, name), 'utf8'), content);
  report.check('静态分析保留样本原字节，插件在独立新会话无需再次启用', (await installed()).digest === original.digest && (await installed()).runtime.instanceId === original.runtime.instanceId);
  await snapshot('analysis-conversation');
}
try {
  report.check('独立验收端口空闲', !await fetch(origin + '/health').then(() => true, () => false)
    && !await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false));
  if (business) {
    await mkdir(join(work, 'dsh'), { mode: 0o700 });
    await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(work, 'dsh/.workbuddy-device.json'));
  }
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port),
      MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '', PATH: `${reaBin}${delimiter}${process.env.PATH}` } });
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '真实 Magi 主窗口', 60_000); ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台'); await settings('plugins');
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
  await previewFixedUpstreamFromNative({ page, packageName: 'dsh-rea', version: '1.2.0' });
  const preview = await waitFor(async () => {
    if (await ui.exists('[data-testid=plugin-candidate]')) return { ok: true };
    const error = await ui.text('[data-testid=plugin-message]'); return error && { error };
  }, '真实在线翻译预览', 180_000);
  assert.ok(preview.ok, preview.error);
  report.check('在线翻译原包成功并展示权限预览', /sha512-/.test(await ui.text('[data-testid=plugin-candidate]')));
  await ui.click('[data-action=confirm-candidate]');
  const plugin = await waitFor(installed, '原包安装完成', 180_000); pluginId = plugin.manifest.id;
  assert.equal(plugin.manifest.version, '1.2.0');
  report.check('安装保留原包 SVG 路径和在线来源', plugin.source.kind === 'upstream' && plugin.manifest.icon === 'ui/icon.svg');
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
  const remote = await status();
  report.check('通用 Typert Remote 返回真实安装状态与工具数量', remote.ok && remote.value.rea.ok && remote.value.tools.count === count, JSON.stringify(remote));
  await settings(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists('.settings-frame:not([hidden]) iframe'), '原包设置 iframe');
  const frameUrl = await page.evaluate('document.querySelector(".settings-frame:not([hidden]) iframe").src');
  frame = await waitFor(() => connectTarget(cdpPort, (target) => (target.type === 'iframe' || target.type === 'page') && target.url === frameUrl, 'REA 设置', errors), '原包设置上下文');
  await waitFor(() => frame.evaluate('Boolean(document.getElementById("plugin-config-dsh-rea-timeout"))'), '上游原生设置表单', 60_000);
  await waitFor(() => frame.evaluate(`document.body.innerText.includes(${JSON.stringify(remote.value.rea.version)})`), '上游 Remote 状态卡片', 30_000);
  report.check('原生 React 设置表单与状态卡片加载，无未提供模块', !await frame.evaluate('document.body.innerText.includes("界面加载失败")'));
  const ghidraLink = await frame.evaluate(`(()=>{const a=[...document.querySelectorAll('a[href]')].find((item)=>item.textContent.includes('Ghidra'));if(!a)return null;const style=getComputedStyle(a);return {href:a.href,target:a.target,color:style.color,decoration:style.textDecorationLine};})()`);
  report.check('上游 Ghidra 外链保留 href、target 与链接色', ghidraLink?.href === 'https://github.com/NationalSecurityAgency/ghidra/releases'
    && ghidraLink.target === '_blank' && ghidraLink.color !== await frame.evaluate('getComputedStyle(document.body).color'), JSON.stringify(ghidraLink));
  report.check('原生表单默认值与运行期配置一致', await frame.evaluate('document.getElementById("plugin-config-dsh-rea-timeout").value === "300000"'));
  await snapshot('settings');
  await frame.evaluate(`(()=>{const field=document.getElementById('plugin-config-dsh-rea-timeout');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(field,'310000');field.dispatchEvent(new Event('input',{bubbles:true}));field.dispatchEvent(new Event('change',{bubbles:true}));return true;})()`);
  await waitFor(() => frame.evaluate(`(()=>{const b=[...document.querySelectorAll('button')].find(b=>/保存|Save/.test(b.textContent));if(!b||b.disabled)return false;b.click();return true;})()`), '保存原生设置');
  await waitFor(async () => (await api(`/api/plugins/${pluginId}/settings`)).value.toolCallTimeoutMs === 310000, 'daemon 持久设置');
  await waitFor(async () => { const value = await status(); return value.ok && value.value.timeoutMs === 310000 && value.value.tools.count === count; }, 'Volatile 配置重载与 MCP 重新注册', 90_000);
  report.check('原生表单保存经 CAS 持久化并触发上游 MCP 配置重载', true);
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  await snapshot('settings-390');
  report.check('390px 原生设置无横向溢出', await page.evaluate('document.documentElement.scrollWidth<=innerWidth') && await frame.evaluate('document.documentElement.scrollWidth<=innerWidth'));
  await page.call('Emulation.clearDeviceMetricsOverride');
  const settingsIdentity = await frame.evaluate('window.__settingsVisitIdentity = crypto.randomUUID()');
  await frame.evaluate(`(()=>{const field=document.getElementById('plugin-config-dsh-rea-timeout');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(field,'320000');field.dispatchEvent(new Event('input',{bubbles:true}));field.dispatchEvent(new Event('change',{bubbles:true}));})()`);
  const scroll = await page.evaluate(`(()=>{const frame=document.querySelector('.settings-frame:not([hidden]) iframe');window.__reaSettingsFrame=frame;const scroller=frame.closest('.settings-frame');scroller.scrollTop=120;return scroller.scrollTop;})()`);
  await settings('appearance');
  report.check('切换设置分类只隐藏已访问原包页面，不销毁隔离文档', await page.evaluate('window.__reaSettingsFrame.isConnected && !window.__reaSettingsFrame.getClientRects().length && window.__reaSettingsFrame.closest(".settings-frame").inert'));
  await settings(`plugin:${pluginId}:settings`);
  await writeFile(join(output,'settings-cache-return.json'), JSON.stringify({expected:{settingsIdentity,scroll},
    document:await frame.evaluate('({identity:window.__settingsVisitIdentity,value:document.getElementById("plugin-config-dsh-rea-timeout").value})'),
    host:await page.evaluate('({same:document.querySelector(".settings-frame:not([hidden]) iframe")===window.__reaSettingsFrame,scroll:window.__reaSettingsFrame.closest(".settings-frame").scrollTop,visible:!!window.__reaSettingsFrame.getClientRects().length})')},null,2));
  report.check('返回插件设置复用同一文档，保留未保存表单与滚动位置',
    await frame.evaluate(`window.__settingsVisitIdentity===${JSON.stringify(settingsIdentity)} && document.getElementById('plugin-config-dsh-rea-timeout').value==='320000'`)
      && await page.evaluate(`document.querySelector('.settings-frame:not([hidden]) iframe')===window.__reaSettingsFrame && window.__reaSettingsFrame.closest('.settings-frame').scrollTop===${scroll}`));
  report.check('设置页保活不会把未提交草稿写成 daemon 配置', (await api(`/api/plugins/${pluginId}/settings`)).value.toolCallTimeoutMs === 310000);
  await ui.click('[data-testid=settings-back]');
  await waitFor(() => page.evaluate('!document.querySelector("[data-testid=settings-page]").getClientRects().length'), '返回原生对话');
  await settings(`plugin:${pluginId}:settings`);
  report.check('返回对话再打开设置仍保留同一原包表单', await frame.evaluate(`window.__settingsVisitIdentity===${JSON.stringify(settingsIdentity)} && document.getElementById('plugin-config-dsh-rea-timeout').value==='320000'`));
  await ui.click('[data-testid=plugin-page-header] input[role=switch]');
  await waitFor(async () => !(await installed()).active, '停用回收');
  report.check('停用清空真实 MCP 动态目录', (await installed()).manifest.contributions.tools.length === 0);
  await waitFor(() => page.evaluate('!window.__reaSettingsFrame.isConnected'), '停用释放已保活的设置文档');
  report.check('全局停用释放设置文档，不把页面保活当成插件继续运行', true);
  await ui.click('[data-testid=plugin-page-header] input[role=switch]');
  await waitFor(async () => (await installed()).manifest.contributions.tools.length === count, '再次激活恢复目录', 90_000);
  report.check('重激活保留配置并恢复真实 MCP 工具', (await api(`/api/plugins/${pluginId}/settings`)).value.toolCallTimeoutMs === 310000);
  frame.close();
  frame = await waitFor(() => connectTarget(cdpPort, target => ['iframe', 'page'].includes(target.type) && target.url === frameUrl, '重新启用后的 REA 设置', errors), '新设置文档');
  await waitFor(() => frame.evaluate('document.getElementById("plugin-config-dsh-rea-timeout")?.value === "310000"'), '新页面读取已提交配置');
  report.check('重新启用创建新文档并读取 daemon 配置，不复活旧页面草稿', await frame.evaluate('window.__settingsVisitIdentity === undefined'));
  if (business) await verifyAnalysis();
  report.check('主窗口和原包 iframe 无未捕获运行异常', errors.length === 0, errors.join('\n'));
  await writeFile(join(output, 'evidence.json'), JSON.stringify({ completed: true, businessVerified: business, pluginId, version: plugin.manifest.version, source: plugin.source, toolCount: count, setup: remote.value }, null, 2));
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
