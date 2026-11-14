// 在线安装真实 npm 原包并调用本机 WorkBuddy 登录态；会产生真实模型请求，按需显式运行。
// 独立 Magi / DSH 状态根，不导出凭据，不修改用户已有 Magi 会话。
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { cp, mkdir, mkdtemp, writeFile, readFile, realpath } from 'node:fs/promises';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, launchApp, pageTools, waitFor, createReporter } from './lib/electron-harness.mjs';
import { verifyWorkbuddyStats } from './lib/workbuddy-stats-acceptance.mjs';
import { previewFixedUpstreamFromNative, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = 38938, cdpPort = 9308, origin = `http://127.0.0.1:${port}`;
const work = await mkdtemp(join(tmpdir(), 'magi-workbuddy-live-'));
const output = process.env.MAGI_WORKBUDDY_OUTPUT || join(root, 'target/plugin-acceptance/workbuddy');
const report = createReporter(), errors = [];
const lifecycleRequests = [];
const clipAuditSource = process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE ? await readFile(process.env.MAGI_PLUGIN_CLIP_AUDIT_SOURCE, 'utf8') : null;
let app, page, frame, ui, pluginId;
await mkdir(output, { recursive: true });
await mkdir(join(work, 'dsh'), { mode: 0o700 });
// 沿用该用户的设备身份；账号仍由未经修改的原包从 WorkBuddy 桌面登录目录发现。
await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(work, 'dsh/.workbuddy-device.json')).catch((error) => { if (error.code !== 'ENOENT') throw error; });
const api = async (path, body, method = body === undefined ? 'GET' : 'POST') => {
  const response = await fetch(origin + path, { method, headers: { origin, 'content-type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(180_000) });
  const raw = await response.text();
  const value = raw ? JSON.parse(raw) : null;
  assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`);
  return value;
};
const installed = async () => (await api('/api/plugins')).plugins.find((p) => p.manifest.name === 'DSH Connect WorkBuddy');
const screenshot = async (name) => {
  // DOM 已渲染不等于合成帧已提交；截图要记录实际完成绘制的页面。
  await page.evaluate('new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))');
  await writeFile(join(output, `${name}.png`), Buffer.from(await ui.screenshot(), 'base64'));
};
const tab = async (id) => {
  if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) await ui.click('[data-testid=sidebar-settings]');
  await waitFor(async () => {
    const selector = `[data-testid="settings-nav-${id}"]`;
    if (!await ui.exists(selector)) return false;
    await ui.click(selector);
    return page.evaluate(`document.querySelector(${JSON.stringify(selector)})?.getAttribute('aria-current') === 'page'`);
  }, `设置分类 ${id}`);
};
async function start() {
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort,
    env: { MAGI_STATE_ROOT: join(work, 'state'), DSH_HOME: join(work, 'dsh'), MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '真实 Magi 主窗口', 60_000);
  await page.call('Network.enable');
  page.on('Network.requestWillBeSent', ({ request }) => {
    const action = request.url.match(/\/api\/plugins\/[^/]+\/(enable|disable)$/)?.[1];
    if (action && request.method === 'POST') lifecycleRequests.push({ action, ...JSON.parse(request.postData || '{}') });
  });
  ui = pageTools(page);
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '工作台');
}
async function verifyCliCommand(selected) {
  const binary = process.env.MAGI_WORKBUDDY_CLI_BINARY;
  if (!binary) return;
  const settings = await api(`/api/plugins/${pluginId}/settings`);
  const injectResult=process.env.MAGI_WORKBUDDY_CLI_INJECT_RESULT==='1';
  await api(`/api/plugins/${pluginId}/settings`, { expectedVersion: settings.version, value: {
    ...settings.value, cliBridge: { ...settings.value?.cliBridge, enabled: true, binary,
      permissionMode: 'dontAsk', injectResult, timeoutSecs: 90,
      ...(process.env.MAGI_WORKBUDDY_CLI_MODEL ? {model:process.env.MAGI_WORKBUDDY_CLI_MODEL} : {}),
    },
  } }, 'PUT');
  const command = await waitFor(async () => (await installed())?.manifest.contributions.commands.find(command=>command.title==='codebuddy'), '在线原包的 CodeBuddy 命令');
  const directory = join(work, 'cli-project'); await mkdir(directory);
  await promisify(execFile)('git',['init','--quiet',directory]);
  // CLI 拥有自己的工具权限；这里只授权验收文件的读取。
  await mkdir(join(directory,'.codebuddy'));
  await writeFile(join(directory,'.codebuddy/settings.local.json'),JSON.stringify({permissions:{allow:['Read(./verification.txt)'],deny:['Edit','Write','Bash']}}));
  const marker = 'MAGI_COMMAND_FILE_'+Date.now().toString(36);
  await writeFile(join(directory, 'verification.txt'), marker);
  const {workspace} = await api('/api/workspaces/create',{name:'CodeBuddy command acceptance',sourceFolders:[{path:directory}]});
  const session = (await api('/api/session/materialize',{scope:'workspace',workspaceId:workspace.workspaceId})).sessionId;
  if(injectResult) await api('/api/settings/orchestrator/session/save',{scope:'workspace',workspaceId:workspace.workspaceId,sessionId:session,
    config:{sourceId:selected.sourceId,model:selected.id,reasoningEffort:null}});
  const turn = await api('/api/session/turn',{scope:'workspace',workspaceId:workspace.workspaceId,sessionId:session,
    pluginCommandId:`plugin/${pluginId}/${command.id}`,text:'Read verification.txt in the current directory and reply with only its full contents. Do not modify any file.',
    requestId:'workbuddy-cli-live',userMessageId:'workbuddy-cli-live-user'});
  const result = await waitFor(async () => (await api(`/api/messages?scope=workspace&workspaceId=${workspace.workspaceId}&sessionId=${session}`)).canonicalTurns
    ?.find(item=>item.turnId===turn.turnId && ['completed','failed','cancelled','blocked'].includes(item.status)), '真实 CodeBuddy CLI 读项目文件',120_000);
  const text = result.items.filter(item=>item.kind==='assistant_text').map(item=>item.content??'').join('\n');
  const task = turn.rootTaskId && await api(`/api/agent-runs/projection/${turn.rootTaskId}?scope=workspace&workspaceId=${workspace.workspaceId}&sessionId=${session}`);
  await writeFile(join(output,'codebuddy-command.json'),JSON.stringify({version:(await installed()).manifest.version,binary,
    model:process.env.MAGI_WORKBUDDY_CLI_MODEL??null,sessionId:session,turnId:turn.turnId,taskId:turn.rootTaskId,
    taskStatus:task?.root_task.status,status:result.status,text,injectResult},null,2));
  report.check('真实 CodeBuddy 命令由原生 Task 调度并完成结算', Boolean(turn.rootTaskId)
    && task.root_task.status==='completed' && result.items.find(item=>item.kind==='user_message')?.metadata.executionProfile==='task',
    JSON.stringify({taskStatus:task?.root_task.status,turnStatus:result.status,text}));
  report.check('原包 /codebuddy 在真实会话目录调用实际 CLI 并读取文件结果', result.status==='completed'
    && text.includes(marker) && text.includes(await realpath(directory)), JSON.stringify({status:result.status,text}));
  report.check('CodeBuddy 命令保持只读业务结果与原有文件', await readFile(join(directory,'verification.txt'),'utf8')===marker);
  if(injectResult) {
    const injected=await waitFor(async()=>{
      const turns=(await api(`/api/messages?scope=workspace&workspaceId=${workspace.workspaceId}&sessionId=${session}`)).canonicalTurns;
      return turns.find(turn=>turn.items.some(item=>item.metadata?.pluginMessage?.pluginId===pluginId)
        && ['completed','failed','cancelled','blocked'].includes(turn.status));
    },'未经修改的 CodeBuddy 原包回注后由真实主模型答复',180000);
    const message=injected.items.find(item=>item.metadata?.pluginMessage?.pluginId===pluginId);
    const answer=injected.items.filter(item=>item.kind==='assistant_text').map(item=>item.content??'').join('\n').trim();
    await writeFile(join(output,'codebuddy-injection.json'),JSON.stringify({sessionId:session,commandTurnId:turn.turnId,
      status:injected.status,origin:message?.metadata.pluginMessage,message:message?.content,answer,modelSteps:injected.modelSteps},null,2));
    report.check('原包自动回注实际 CLI 结果并保存来源，沿原生队列唤醒真实主模型',injected.status==='completed'
      && message?.content.includes(marker)&&message.metadata.pluginMessage.sourceTurnId===result.turnId
      && message.metadata.pluginMessage.delivery==='steer'&&injected.modelSteps.length>0&&answer.length>0,
      JSON.stringify({status:injected.status,origin:message?.metadata.pluginMessage,answer}));
    await page.call('Page.navigate',{url:`${origin}/web.html?scope=workspace&workspaceId=${workspace.workspaceId}&sessionId=${session}`});
    await waitFor(()=>page.evaluate('Boolean(document.querySelector("[data-source=plugin]"))'),'真实原包回注的会话展示');
    await screenshot('codebuddy-injection');
  }

}
try {
  report.check('独立验收端口空闲', !await fetch(origin + '/health').then(() => true, () => false)
    && !await fetch(`http://127.0.0.1:${cdpPort}/json/version`).then(() => true, () => false));
  await start();
  await tab('plugins');
  await waitFor(() => ui.exists('[data-testid=plugin-search]'), '插件搜索');
  await page.evaluate('const input=document.querySelector("[data-testid=plugin-search]"); input.value="dsh-connect-workbuddy"; input.dispatchEvent(new Event("input",{bubbles:true})); true');
  const upstreamRow = '[data-testid=plugin-row][data-plugin-id="upstream:dsh:dsh-connect-workbuddy"]';
  await waitFor(() => ui.exists(upstreamRow), 'npm 在线搜索结果', 60_000);
  await ui.click(upstreamRow);
  report.check('在线搜索呈现真实 DSH 上游包', await ui.exists('[data-testid=plugin-upstream-badges]'));
  await previewFixedUpstreamFromNative({ page, packageName: 'dsh-connect-workbuddy', version: '5.21.2' });
  const preview = await waitFor(async () => {
    if (await ui.exists('[data-testid=plugin-candidate][data-origin=upstream]')) return { ok: true };
    const error = await page.evaluate('[...document.querySelectorAll("[role=status]")].map(e=>e.textContent).find(t=>t.includes("翻译上游插件失败"))');
    return error && { error };
  }, '原包在线下载与翻译预览', 180_000);
  assert.ok(preview.ok, preview.error);
  const previewText = await ui.text('[data-testid=plugin-candidate]');
  report.check('在线安装预览展示真实界面影响说明', await ui.exists('[data-testid=plugin-impact-ui]')
    && /设置页/.test(await ui.text('[data-testid=plugin-impact-ui]')));
  report.check('预览显示 npm 完整性摘要与可选兼容限制', /sha512-/.test(previewText) && /可选路径/.test(previewText), previewText?.slice(-1500));
  await screenshot('install-preview');
  await ui.click('[data-action=confirm-candidate]');
  const plugin = await waitFor(installed, '确认在线安装', 120_000);
  pluginId = plugin.manifest.id;
  assert.equal(plugin.manifest.version, '5.21.2');
  report.check('WorkBuddy 工具和命令仍独立声明结果回注授权', plugin.manifest.permissions.some(permission => permission.kind === 'sessionWrite'
    && permission.targets.includes('context') && permission.targets.includes('wake')));
  report.check(`已在线安装 ${plugin.manifest.name}@${plugin.manifest.version}`, plugin.source.kind === 'upstream');
  const row = `[data-testid=plugin-row][data-plugin-id="${pluginId}"]`;
  await tab(`plugin:${pluginId}:settings`);
  const activated = await waitFor(async () => {
    const item = await installed();
    return ['running', 'failed'].includes(item.runtime?.state) && item;
  }, '真实 WorkBuddy 原包运行', 120_000);
  assert.equal(activated.runtime.state, 'running', activated.runtime.reason);
  report.check('真实 Cordis 原包启动成功', true);
  report.check('安装完成即启用，不再发出任何授权或启用请求', lifecycleRequests.length === 0 && activated.enabled);
  report.check('配置只在设置页声明，原包同组件主区别名不生成主对话或右栏入口', activated.manifest.contributions.views.filter(view=>view.placements.includes('settings')).length === 1
    && !activated.manifest.contributions.views.some(view=>view.placements.includes('main')||view.placements.includes('rightPane'))
    && !await ui.exists('[data-testid=top-tab-plugin]'));
  const selected = await waitForPluginModel({ request: api, pluginId, modelId: process.env.MAGI_WORKBUDDY_MODEL || 'deepseek-v4.1-flash',
    record: value => writeFile(join(output, 'model-selection.json'), JSON.stringify(value, null, 2)) });
  report.check('真实 WorkBuddy 模型数据源进入主对话目录', Boolean(selected));
  await tab(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists('.settings-frame:not([hidden]) iframe'), '原包设置界面');
  const frameUrl = await page.evaluate('document.querySelector(".settings-frame:not([hidden]) iframe").src');
  frame = await waitFor(() => connectTarget(cdpPort, (t) => (t.type === 'iframe' || t.type === 'page') && t.url === frameUrl, 'WorkBuddy 原生设置', errors), 'WorkBuddy 设置 iframe');
  await waitFor(() => frame.evaluate('document.querySelectorAll("button").length > 3 && !document.body.innerText.includes("界面加载失败")'), '原生设置卡片渲染', 60_000);
  report.check('未经修改的上游设置卡片实际渲染', true);
  report.check('原包配置通过独立插件设置 Tab 展示，主对话没有配置页签', await page.evaluate(`document.querySelector(${JSON.stringify(`[data-testid="settings-nav-plugin:${pluginId}:settings"]`)})?.getAttribute('aria-current') === 'page'`)
    && !await ui.exists('[data-testid=top-tab-plugin]'));
  await screenshot('settings');
  const session = (await api('/api/session/materialize', { scope: 'personal' })).sessionId;
  await api('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session, config: { sourceId: selected.sourceId, model: selected.id, reasoningEffort: null } });
  const turn = await api('/api/session/turn', { scope: 'personal', sessionId: session, text: 'Reply with exactly MAGI_WORKBUDDY_LIVE_OK.', requestId: 'workbuddy-live-1', userMessageId: 'workbuddy-live-user-1' });
  const finished = await waitFor(async () => {
    const turns = (await api(`/api/messages?scope=personal&sessionId=${encodeURIComponent(session)}`)).canonicalTurns;
    return turns?.find((t) => t.turnId === turn.turnId && ['completed', 'failed', 'cancelled', 'blocked'].includes(t.status));
  }, '真实 WorkBuddy 模型回复', 180_000);
  const assistantText = finished.items.filter((item) => item.kind === 'assistant_text').map((item) => item.content ?? '').join('\n').trim();
  report.check(`真实 WorkBuddy 登录态与 ${selected.id} 完成模型调用`, finished.status === 'completed' && assistantText === 'MAGI_WORKBUDDY_LIVE_OK',
    JSON.stringify({ turnId: finished.turnId, status: finished.status, model: selected.id, assistantText }));
  frame.close(); frame = null;
  await page.call('Page.navigate', { url: `${origin}/web.html?scope=personal&sessionId=${encodeURIComponent(session)}` });
  await waitFor(() => page.evaluate('document.body.innerText.includes("MAGI_WORKBUDDY_LIVE_OK")'), '真实回复展示');
  await screenshot('conversation');
  const statsVerification = process.env.MAGI_WORKBUDDY_STATS === '1' ? await verifyWorkbuddyStats({ request:api,page,session,output,check:report.check }) : null;
  await tab(`plugin:${pluginId}:settings`);
  await waitFor(() => ui.exists(`iframe[data-plugin-id="${pluginId}"]`), '停用前真实原包文档');
  const beforeStop = await installed();
  const staleDocument = await page.evaluate(`document.querySelector('iframe[data-plugin-id="${pluginId}"]').src`);
  await tab('plugins');
  await waitFor(() => ui.exists(row), '插件列表'); await ui.click(row);
  await ui.click('[data-action=disable]');
  await waitFor(async () => !(await installed()).enabled, '停用原包');
  report.check('停用回收真实 WorkBuddy 模型数据源', !(await api('/api/settings/models/catalog')).groups.some((g) => g.pluginId === pluginId));
  await waitFor(() => page.evaluate(`!document.querySelector('iframe[data-plugin-id="${pluginId}"]')`), '停用回收保活设置文档');
  const deniedDocument = await fetch(staleDocument);
  report.check('停用回收隐藏页面，旧文档地址不能继续取得业务页面', [403,409].includes(deniedDocument.status));
  await tab(`plugin:${pluginId}:settings`);
  const requestOffset = lifecycleRequests.length;
  await waitFor(() => ui.exists('[data-testid=plugin-page-header] [role=switch]:not([disabled])'), '设置页的重新启用开关');
  await ui.click('[data-testid=plugin-page-header] [role=switch]');
  await waitFor(async () => (await installed()).runtime?.state === 'running', '重新启用', 90_000);
  const reactivated = await installed();
  report.check('设置页重新启用创建新实例，只发出一次启用请求', reactivated.runtime.instanceId !== beforeStop.runtime.instanceId
    && lifecycleRequests.slice(requestOffset).map(item => item.action).join(',') === 'enable'
    && lifecycleRequests.slice(requestOffset).every(item => item.expectedDigest === plugin.digest));
  await waitFor(() => ui.exists(`iframe[data-plugin-id="${pluginId}"]`), '重新启用后的原包文档');
  await writeFile(join(output, 'lifecycle-requests.json'), JSON.stringify(lifecycleRequests, null, 2));
  await verifyCliCommand(selected);
  const beforeRestart = await installed(), savedSettings = await api(`/api/plugins/${pluginId}/settings`);
  frame?.close(); frame = null; page.close(); page = null;
  await writeFile(join(output, 'before-restart.log'), app.log.join('')); await app.stop();
  await start();
  const restored = await waitFor(async () => { const item = await installed(); return item?.runtime?.state === 'running' && item; }, '应用重启恢复原包', 120_000);
  await waitForPluginModel({ request: api, pluginId, modelId: selected.id,
    record: value => writeFile(join(output, 'model-selection-restarted.json'), JSON.stringify(value, null, 2)) });
  report.check('应用重启保持同一原包、授权和配置并恢复新宿主及真实模型目录', restored.digest === beforeRestart.digest
    && restored.runtime.instanceId !== beforeRestart.runtime.instanceId
    && JSON.stringify(await api(`/api/plugins/${pluginId}/settings`)) === JSON.stringify(savedSettings));
  if (statsVerification) {
    const restoredStats = await api('/api/settings/stats?sessionId='+encodeURIComponent(statsVerification.sessionId));
    await writeFile(join(output,'stats-restored.json'),JSON.stringify(restoredStats,null,2));
    report.check('完整重启恢复同一统计，无重复计数或模型重放',JSON.stringify(restoredStats.totals)===JSON.stringify(statsVerification.stats.totals));
  }
  report.check('主窗口与上游设置无未捕获异常', errors.length === 0 && !/Uncaught|可信 Renderer 控制台错误/.test(app.log.join('')), errors.join('\n'));
  await writeFile(join(output, 'installed.json'), JSON.stringify({ completed: true, pluginId, version: plugin.manifest.version, digest: plugin.digest, source: plugin.source, model: selected.id, sessionId: session, turnId: turn.turnId, stateRoot: join(work, 'state') }, null, 2));
  console.log(report.summary());
} catch (error) {
  console.error(error);
  if (ui) await screenshot('failure').catch(() => {});
  process.exitCode = 1;
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify(report.results, null, 2));
  if (app) { await writeFile(join(output, 'electron.log'), app.log.join('')); await app.stop(); }
  frame?.close(); page?.close();
  // 保留独立状态便于用户继续验证；凭据仅留在临时目录，不写入截图／结果文件。
  console.log(`独立验收状态：${work}`);
}
