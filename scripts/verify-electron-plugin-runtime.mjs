// 从 macOS 目录包启动真实 Magi Main、daemon 与随包 Node/兼容层。
// 独立状态根；DSH 夹具不读用户凭据，模型回执由夹具适配器本地生成。
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const resources = join(appRoot, 'Contents/Resources');
const node = join(resources, 'daemon/node/node');
const compat = join(resources, 'daemon/compat');
const cdpPort = Number(process.env.MAGI_PLUGIN_RUNTIME_CDP_PORT || 9307);
const daemonPort = Number(process.env.MAGI_PLUGIN_RUNTIME_PORT || 38937);
const origin = `http://127.0.0.1:${daemonPort}`;
const output = join(root, 'target/plugin-acceptance/runtime');
const work = await mkdtemp(join(tmpdir(), 'magi-runtime-live-'));
const run = promisify(execFile);
const reporter = createReporter();
const errors = [];
let app, page, frame, ui, pluginId;
await mkdir(output, { recursive: true });
async function api(path, body, method = body === undefined ? 'GET' : 'POST') {
  const response = await fetch(`${origin}${path}`, {
    method, headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(180_000),
  });
  const raw = await response.text();
  let value; try { value = JSON.parse(raw); } catch { value = raw; }
  return { status: response.status, value };
}
async function post(path, body = {}, method = 'POST') {
  const result = await api(path, body, method);
  assert.equal(result.status, 200, `${path}: ${JSON.stringify(result.value)}`);
  return result.value;
}
const installed = async () => (await api('/api/plugins')).value.plugins.find((item) => item.manifest.id === pluginId);
const visible = (selector) => page.evaluate(`Boolean(document.querySelector(${JSON.stringify(selector)})?.getClientRects().length)`);
async function settingsTab(id) {
  const nav = `[data-testid="settings-nav-${id}"]`;
  if (!await visible('[data-testid="settings-page"]')) await ui.click('[data-testid="sidebar-settings"]');
  await waitFor(async () => {
    if (!await ui.exists(nav)) return false;
    await ui.click(nav);
    return page.evaluate(`document.querySelector(${JSON.stringify(nav)})?.getAttribute('aria-current') === 'page'`);
  }, `设置分类 ${id}`);
}
async function snapshot(name) { await writeFile(join(output, `${name}.png`), Buffer.from(await ui.screenshot(), 'base64')); }

async function prepare() {
  const upstream = join(work, 'upstream');
  await mkdir(upstream);
  const deps = JSON.parse(await readFile(join(compat, 'package.json'), 'utf8')).dependencies;
  await writeFile(join(upstream, 'package.json'), JSON.stringify({
    name: 'magi-runtime-acceptance', displayName: '运行时兼容验收', version: '1.0.0', type: 'module', main: 'index.js',
    dsh: { bundle: { patch: './cordis.patch.yml' } },
    peerDependencies: { '@deepseek-ai/cordis': deps['@deepseek-ai/cordis'], '@deepseek-ai/dsh-llm': deps['@deepseek-ai/dsh-llm'] },
    dependencies: { '@deepseek-ai/schemastery': deps['@deepseek-ai/schemastery'] },
  }));
  await writeFile(join(upstream, 'index.js'), `
import Schema from '@deepseek-ai/schemastery';
import { LlmAdapter } from '@deepseek-ai/dsh-llm';
export const name = 'magi-runtime-acceptance';
export const inject = ['commands', 'tools', 'llm'];
export const Config = Schema.object({ label: Schema.string().default('initial'), enabled: Schema.boolean().default(true) });
export function apply(ctx, config) {
  ctx.commands.register({ name: 'report', description: '报告当前配置', handler: async () => ({ kind: 'text', text: 'DSH_REPORT:' + config.label }) });
  if (config.enabled) ctx.effect(() => {
    const timer = setTimeout(() => ctx.tools.register({ name: 'late-read', description: '读取验收配置', parameters: { type: 'object' }, readOnly: true,
      output: { schema: { type: 'string' }, render: (_args, value) => [{ type: 'text', text: value }] }, execute: async () => config.label }), 1700);
    return () => clearTimeout(timer);
  });
  class Adapter extends LlmAdapter {
    async listModels(provider) { return [{ provider, id: 'fixture-model', name: 'Runtime Fixture' }]; }
    async *stream(request) {
      request.signal?.throwIfAborted();
      const text = 'DSH_MODEL:' + config.label;
      yield { type: 'block-start', index: 0, blockType: 'text' };
      yield { type: 'text-delta', index: 0, text };
      yield { type: 'block-end', index: 0, block: { type: 'text', text } };
      yield { type: 'usage', usage: { inputTokens: 1, outputTokens: 1, totalTokens: 2 } };
      yield { type: 'finish', reason: { kind: 'stop' } };
    }
  }
  ctx.llm.registerAdapter(['runtime-fixture'], new Adapter());
}
`);
  const archive = join(work, 'plugin.zip');
  const result = await run(node, [join(compat, 'cli.mjs'), 'prepare', 'dsh', upstream, '--out', join(work, 'prepared'), '--integrity', 'local-acceptance', '--archive', archive], { timeout: 120_000, maxBuffer: 1024 * 1024 });
  const prepared = JSON.parse(result.stdout);
  pluginId = prepared.id;
  reporter.check('随包 Node 与兼容层完成原包封装，安装期不冻结动态贡献', prepared.report.runtimeContributions.includes('tools') && prepared.report.tools.length === 0);
  return { source: 'local:runtime-acceptance.zip', archiveBase64: (await readFile(archive)).toString('base64') };
}

try {
  reporter.check('验收端口未被已有实例使用',
    !await fetch(`${origin}/health`).then(() => true, () => false)
    && !await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false));
  const archive = await prepare();
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    env: { MAGI_STATE_ROOT: join(work, 'state'), MAGI_DESKTOP_DAEMON_PORT: String(daemonPort), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' },
  });
  page = await waitFor(() => connectAppPage(cdpPort, daemonPort, errors), '真实 Magi Renderer', 60_000);
  ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid="sidebar-settings"]'), '工作台');
  reporter.check('真实 Magi Main 托管 daemon 与工作台启动', (await api('/health')).status === 200);
  const preview = await post('/api/plugins/preview', archive);
  await post('/api/plugins/install', { ...archive, expectedDigest: preview.digest });
  await settingsTab('plugins');
  const row = `[data-testid="plugin-row"][data-plugin-id="${pluginId}"]`;
  await waitFor(() => ui.exists(row), '已安装插件');
  await ui.click(row);
  await waitFor(() => ui.exists('[data-action="activate"]'), '授权激活按钮');
  await ui.click('[data-action="activate"]');
  await waitFor(async () => {
    const plugin = await installed();
    if (plugin?.runtime?.state === 'failed') throw new Error(plugin.runtime.reason);
    return plugin?.runtime?.state === 'running';
  }, 'DSH 宿主就绪', 30_000);
  reporter.check('通过界面授权后运行真实 Cordis 插件', true);
  await waitFor(async () => (await installed()).manifest.contributions.tools.length === 1, '就绪后的晚注册工具');
  reporter.check('晚于初始化窗口的工具进入 daemon 动态目录', true);
  // 复现设置读取撞上生命周期排空；真实 iframe 应展示错误，不能留下未捕获拒绝或允许盲写。
  let settingsRead;
  const stopIntercept = page.on('Fetch.requestPaused', (event) => { settingsRead = event.requestId; });
  await page.call('Fetch.enable', { patterns: [{ urlPattern: `${origin}/api/plugins/${pluginId}/settings`, requestStage: 'Request' }] });
  await settingsTab(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists('.settings-frame iframe'), '插件设置视图');
  const frameUrl = await page.evaluate('document.querySelector(".settings-frame iframe").src');
  frame = await waitFor(() => connectTarget(cdpPort, (target) => target.type === 'iframe' && target.url === frameUrl, '插件隔离视图', errors), '插件隔离视图');
  const evaluateFrame = (expression) => frame.evaluate(expression);
  await waitFor(() => settingsRead, '设置读取请求');
  reporter.check('设置读取完成前禁止保存', await evaluateFrame('document.getElementById("save").disabled'));
  await page.call('Fetch.fulfillRequest', { requestId: settingsRead, responseCode: 409,
    responseHeaders: [{ name: 'content-type', value: 'application/json' }],
    body: Buffer.from(JSON.stringify({ error_code: 'CONFLICT', message: 'RUNTIME_SETTINGS_DRAINING' })).toString('base64') });
  await waitFor(() => evaluateFrame('document.getElementById("status").textContent.includes("RUNTIME_SETTINGS_DRAINING")'), '设置读取错误反馈');
  reporter.check('设置读取失败显示原因且禁止覆盖配置', await evaluateFrame('document.getElementById("save").disabled')
    && (await api(`/api/plugins/${pluginId}/settings`)).value.version === 0 && errors.length === 0, errors.join('\n'));
  await page.call('Fetch.disable'); stopIntercept();
  await page.evaluate('document.querySelector(".settings-frame iframe").src += ""; true');
  await waitFor(() => evaluateFrame('Boolean(document.getElementById("save")) && !document.getElementById("save").disabled'), '重新加载设置恢复');
  await waitFor(() => evaluateFrame('Boolean(document.getElementById("f-label"))'), '动态 schema 设置字段');
  reporter.check('首次打开设置页展示与插件执行一致的 schema 默认值',
    await evaluateFrame('document.getElementById("f-label").value === "initial" && document.getElementById("f-enabled").checked'));
  await evaluateFrame('document.getElementById("f-label").value="changed"; document.getElementById("f-enabled").checked=true; document.getElementById("save").click(); true');
  await waitFor(async () => (await api(`/api/plugins/${pluginId}/settings`)).value.value?.label === 'changed', '界面配置保存');
  reporter.check('生成设置页经原视图桥保存动态 schema 配置', true);
  await snapshot('settings-desktop');
  frame.close(); frame = null;
  const plugin = await installed();
  const session = (await post('/api/session/materialize', { scope: 'personal' })).sessionId;
  const command = plugin.manifest.contributions.commands[0].id;
  const source = plugin.manifest.contributions.modelSources[0].id;
  const waitTurn = (id) => waitFor(async () => {
    const turns = (await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).value.canonicalTurns;
    return turns?.find((turn) => turn.turnId === id && ['completed', 'failed', 'cancelled', 'blocked'].includes(turn.status));
  }, `Turn ${id}`, 45_000);
  const cmd = await post('/api/session/turn', { scope: 'personal', sessionId: session, pluginCommandId: `plugin/${pluginId}/${command}`, text: '', requestId: 'runtime-command', userMessageId: 'runtime-command-user' });
  const cmdTurn = await waitTurn(cmd.turnId);
  reporter.check('配置重载后的命令沿 canonical Turn 返回新值', cmdTurn.status === 'completed' && JSON.stringify(cmdTurn).includes('DSH_REPORT:changed'), JSON.stringify(cmdTurn));
  await post('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session, config: { sourceId: `plugin/${pluginId}/${source}`, model: 'fixture-model', reasoningEffort: null } });
  const chat = await post('/api/session/turn', { scope: 'personal', sessionId: session, text: 'runtime model check', requestId: 'runtime-model', userMessageId: 'runtime-model-user' });
  const modelTurn = await waitTurn(chat.turnId);
  reporter.check('主对话通过插件模型桥与真实 DSH 适配器完成流式回复', modelTurn.status === 'completed' && JSON.stringify(modelTurn).includes('DSH_MODEL:changed'), JSON.stringify(modelTurn));
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}` });
  await waitFor(() => page.evaluate('document.body.innerText.includes("DSH_MODEL:changed")'), '回执显示');
  await snapshot('conversation');
  await settingsTab(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists('[data-testid="plugin-page-header"] input[role=switch]'), '插件启用开关');
  await ui.click('[data-testid="plugin-page-header"] input[role=switch]');
  await waitFor(async () => !(await installed()).active, '停用');
  reporter.check('界面停用回收动态工具和模型来源', (await installed()).manifest.contributions.tools.length === 0
    && !(await api('/api/settings/models/catalog')).value.groups.some((group) => group.pluginId === pluginId));
  await waitFor(() => ui.exists('[data-testid="plugin-page-inactive"]'), '停用提示');
  await page.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false });
  await page.call('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: 'dark' }] });
  await snapshot('inactive-390');
  reporter.check('390px 设置页无横向溢出且保留停用状态', await page.evaluate('document.documentElement.scrollWidth <= innerWidth'));
  await page.call('Emulation.clearDeviceMetricsOverride');
  await settingsTab('plugins');
  await waitFor(() => ui.exists(row), '停用后的插件列表');
  await ui.click(row);
  await waitFor(() => ui.exists('[data-action="activate"]'), '重新激活按钮');
  await ui.click('[data-action="activate"]');
  await waitFor(async () => (await installed()).runtime?.state === 'running', '重新激活');
  reporter.check('重新激活恢复持久配置并建立新运行目录', (await api(`/api/plugins/${pluginId}/settings`)).value.value.label === 'changed');
  const upstream = await api('/api/plugins/upstream/preview', { ecosystem: 'dsh', package: 'dsh-connect-workbuddy', version: '5.18.1' });
  reporter.check('真实 npm WorkBuddy 原包的未支持必需契约被明确拒绝', upstream.status >= 400 && /sessionProjections|session\/event/.test(JSON.stringify(upstream.value)), JSON.stringify(upstream.value));
  await writeFile(join(output, 'workbuddy-result.json'), JSON.stringify(upstream, null, 2));
  const rendererLogErrors = app.log.join('').split('\n').filter((line) => /Uncaught|可信 Renderer 控制台错误/.test(line));
  reporter.check('主 Renderer 与插件 iframe 没有运行时异常', errors.length === 0 && rendererLogErrors.length === 0,
    [...errors, ...rendererLogErrors].join('\n'));
  console.log(reporter.summary());
} catch (error) {
  if (ui) await snapshot('failure').catch(() => {});
  console.error(error);
  process.exitCode = 1;
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(reporter.results, null, 2));
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  frame?.close();
  page?.close();
  await rm(work, { recursive: true, force: true });
}
