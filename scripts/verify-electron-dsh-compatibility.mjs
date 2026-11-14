// 真实 Ops 原包通过公开路由读写 daemon 的兼容策略；不改上游页面与业务代码。
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, writeFile, access, cp, readdir } from 'node:fs/promises';
import { tmpdir, homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { connectAppPage, connectTarget, createReporter, launchApp, pageTools, waitFor } from './lib/electron-harness.mjs';
import { waitForInstalledPlugin, waitForPluginModel } from './lib/plugin-native-acceptance.mjs';

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const appRoot = process.env.MAGI_PLUGIN_TEST_APP || join(root, 'target/electron-dist/mac-arm64/Magi.app');
const port = Number(process.env.MAGI_DSH_COMPATIBILITY_PORT || 38945);
const cdpPort = Number(process.env.MAGI_DSH_COMPATIBILITY_CDP_PORT || 9315);
const origin = `http://127.0.0.1:${port}`;
const output = process.env.MAGI_DSH_COMPATIBILITY_OUTPUT || join(root, 'target/plugin-acceptance/dsh-compatibility');
const chatOnly = process.env.MAGI_DSH_OPS_CHAT === '1';
const work = await mkdtemp(join(tmpdir(), 'magi-dsh-compatibility-'));
const stateRoot = join(work, 'state'), dshHome = join(work, 'dsh');
const report = createReporter(), errors = [];
let app, page, frame, ui, ops;
await mkdir(output, { recursive: true });
async function api(path, body) {
  const response = await fetch(origin + path, { method: body === undefined ? 'GET' : 'POST', headers: { origin, 'content-type': 'application/json' }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(300_000) });
  const value = await response.json();
  assert.equal(response.status, 200, `${path}: ${JSON.stringify(value)}`);
  return value;
}
const plugins = async () => (await api('/api/plugins')).plugins;
const installedTarget = async () => (await plugins()).find((p) => p.source.location.startsWith('dsh:dsh-plugin-release-probe@'));
async function activate(plugin) {
  const path = `/api/plugins/${plugin.manifest.id}`;
  await api(path + '/enable', {});
  await waitFor(async () => (await plugins()).find((p) => p.manifest.id === plugin.manifest.id)?.runtime?.state === 'running', '原包实际运行', 120_000);
}
async function route(path, body) {
  return api(`/api/plugins/${ops.manifest.id}/routes`, { method: body === undefined ? 'GET' : 'POST', path: '/dsh-ops' + path, ...(body === undefined ? {} : { body }) });
}
async function start() {
  assert.ok(!await fetch(origin + '/health').then(() => true, () => false), '验收端口已占用');
  app = launchApp({ executable: join(appRoot, 'Contents/MacOS/Magi'), cwd: root, cdpPort, env: { RUST_LOG:'info,magi_extension_host=debug', MAGI_STATE_ROOT: stateRoot, DSH_HOME: dshHome, MAGI_DESKTOP_DAEMON_PORT: String(port), MAGI_OPEN_BROWSER: '0', MAGI_EXTENSION_NODE: '', MAGI_EXTENSION_COMPAT: '' } });
  await waitFor(() => api('/health'), '随包 daemon', 60_000);
  page = await waitFor(() => connectAppPage(cdpPort, port, errors), '真实 Electron');
  ui = pageTools(page);
}
async function stop(logName) {
  frame?.close(); frame = undefined; page?.close(); page = undefined;
  await writeFile(join(output, logName), app.log.join(''));
  await app.stop(); app = undefined;
}
async function settings(expectExemption = false) {
  frame?.close(); frame = undefined;
  await waitFor(() => ui.exists('[data-testid=sidebar-settings]'), '设置入口');
  if (!await page.evaluate('Boolean(document.querySelector("[data-testid=settings-page]")?.getClientRects().length)')) await ui.click('[data-testid=sidebar-settings]');
  const view = ops.manifest.contributions.views.find((v) => v.placements.includes('settings'));
  const nav = `[data-testid=${JSON.stringify(`settings-nav-plugin:${ops.manifest.id}:${view.id}`)}]`;
  await waitFor(() => ui.exists(nav), 'Ops 原生设置入口'); await ui.click(nav);
  const selector = `iframe[data-plugin-id=${JSON.stringify(ops.manifest.id)}]`;
  await waitFor(() => ui.exists(selector), 'Ops 页面');
  const url = await page.evaluate(`document.querySelector(${JSON.stringify(selector)}).src`);
  frame = await waitFor(() => connectTarget(cdpPort, (t) => ['iframe', 'page'].includes(t.type) && t.url === url, 'Ops 原生页面', errors), '原包 iframe');
  await waitFor(() => frame.evaluate(`document.body.innerText.includes("magi (") && ${expectExemption ? 'document.body.innerText.includes("dsh-plugin-release-probe@0.0.8")' : 'true'}`), '原包扫描与真实目录', 60_000);
  await writeFile(join(output, 'ops-exemptions.png'), Buffer.from(await ui.screenshot(), 'base64'));
}
async function installFromPage(spec) {
  await waitFor(() => frame.evaluate(`Boolean(document.querySelector('input[placeholder*="dsh-aimail"]'))`), 'Ops 原页面安装输入');
  await frame.evaluate(`(() => {const input=document.querySelector('input[placeholder*="dsh-aimail"]');Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(input,${JSON.stringify(spec)});input.dispatchEvent(new Event('input',{bubbles:true}));})()`);
  await waitFor(() => frame.evaluate(`(() => {const button=document.querySelector('input[placeholder*="dsh-aimail"]').parentElement.querySelector('button');if(button.disabled)return false;button.click();return true;})()`), 'Ops 原页面检查并安装');
}
try {
  await start();
  const input = { ecosystem: 'dsh', package: 'dsh-plugin-ops-bundle', version: '0.20.8' };
  const preview = await api('/api/plugins/upstream/preview', input);
  ops = (await api('/api/plugins/upstream/install', { ...input, expectedDigest: preview.candidate.digest })).plugins.find((p) => p.source.location === 'dsh:dsh-plugin-ops-bundle@0.20.8');
  report.check('ctx.get 取得的管理服务也声明独立兼容授权', ops.manifest.permissions.some((p) => p.kind === 'pluginManagement' && p.targets.includes('compatibility')));
  const runtimeVersion = JSON.parse(await readFile(join(appRoot, 'Contents/Resources/daemon/compat/node_modules/@deepseek-ai/dsh-tools/package.json'), 'utf8')).version;
  const target = { name: 'dsh-plugin-release-probe', version: '0.0.8', spec: 'dsh-plugin-release-probe@0.0.8', runtimeVersion };
  await activate(ops);
  if (chatOnly) {
    // 同一 daemon 先证明用户选择的真实模型可用，再检验原包业务，排除未配置模型。
    await mkdir(dshHome, { recursive: true, mode: 0o700 });
    await cp(join(homedir(), '.dsh/.workbuddy-device.json'), join(dshHome, '.workbuddy-device.json'));
    const request = { ecosystem: 'dsh', package: 'dsh-connect-workbuddy', version: '5.21.2' };
    const candidate = await api('/api/plugins/upstream/preview', request);
    const buddy = (await api('/api/plugins/upstream/install', { ...request, expectedDigest: candidate.candidate.digest })).plugins.find(p => p.manifest.id === candidate.candidate.manifest.id);
    await activate(buddy);
    const model = await waitForPluginModel({ request: api, pluginId: buddy.manifest.id,
      modelId: process.env.MAGI_DSH_OPS_MODEL || 'deepseek-v4.1-flash', record: value => writeFile(join(output, 'model-selection.json'), JSON.stringify(value, null, 2)) });
    const reasoningEffort = process.env.MAGI_DSH_OPS_EFFORT || null;
    if (reasoningEffort) assert.ok(model.reasoningCapability?.supportedEfforts.includes(reasoningEffort), '指定推理档位必须由真实模型声明');
    const session = (await api('/api/session/materialize', { scope: 'personal' })).sessionId;
    await api('/api/settings/orchestrator/session/save', { scope: 'personal', sessionId: session, config: { sourceId: model.sourceId, model: model.id, reasoningEffort } });
    const submitted = await api('/api/session/turn', { scope: 'personal', sessionId: session, text: 'Reply with exactly MAGI_OPS_MODEL_READY', requestId: 'ops-model-ready', userMessageId: 'ops-model-ready-user' });
    const nativeTurn = await waitFor(async () => (await api(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns.find(t => t.turnId === submitted.turnId && ['completed', 'failed', 'cancelled', 'blocked'].includes(t.status)), '原生真实模型答复', 180_000);
    await writeFile(join(output, 'native-model-turn.json'), JSON.stringify(nativeTurn, null, 2));
    report.check('同一 daemon 的指定 WorkBuddy 模型完成实际调用并保存模型步骤', nativeTurn.status === 'completed' && nativeTurn.modelSteps.length > 0
      && nativeTurn.items.some(item => item.kind === 'assistant_text' && item.content?.includes('MAGI_OPS_MODEL_READY')));
    await settings();
    // 仅记录原页面真实响应，不改变请求、回执或业务内容。
    await frame.evaluate(`(() => {const original=globalThis.fetch;globalThis.fetch=async(...args)=>{const response=await original(...args);if(String(args[0]?.url??args[0]).includes('/api/chat'))globalThis.__opsChatReceipt={status:response.status,body:await response.clone().json()};return response;};})()`);
    await frame.evaluate(`(() => {const button=[...document.querySelectorAll('.dshops-chatbar button')].find(node=>node.innerText==='发送');button.parentElement.querySelector('input').focus();})()`);
    await frame.call('Input.insertText', { text: '请解释当前扫描结果，并说明哪些项目需要处理。' });
    await waitFor(() => frame.evaluate(`(() => {const button=[...document.querySelectorAll('button')].find(node=>node.innerText==='发送');if(!button||button.disabled)return false;button.click();return true;})()`), 'Ops 原页面发送诊断问题');
    const receipt = await waitFor(() => frame.evaluate('globalThis.__opsChatReceipt'), 'Ops 原始诊断对话回执', 90_000);
    await waitFor(() => frame.evaluate(`Boolean([...document.querySelectorAll('button')].find(node=>node.innerText==='发送'&&!node.disabled))`), 'Ops 诊断对话结束等待');
    const text = await frame.evaluate('document.body.innerText');
    await writeFile(join(output, 'ops-chat-page.txt'), text);
    await writeFile(join(output, 'ops-chat-receipt.json'), JSON.stringify(receipt, null, 2));
    assert.deepEqual((await api(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns, [nativeTurn]);
    report.check('Ops 诊断不替换原生模型对话或追加用户消息', true);
    report.check('Ops 原页面诊断对话取得实际模型答复', receipt.status === 200 && receipt.body.ok === true && typeof receipt.body.reply === 'string'
      && receipt.body.reply.trim().length > 0 && await frame.evaluate(`[...document.querySelectorAll('.dshops-msg-assistant')].some(node=>node.innerText===${JSON.stringify(receipt.body.reply ?? '')})`), JSON.stringify(receipt));
    await frame.evaluate('document.querySelector(".dshops-chat")?.scrollIntoView({block:"center",behavior:"instant"})');
    await writeFile(join(output,'ops-chat-answer.png'),Buffer.from(await ui.screenshot(),'base64'));
    await frame.evaluate(`(() => {const original=globalThis.fetch;globalThis.fetch=async(...args)=>{const response=await original(...args);if(String(args[0]?.url??args[0]).includes('/api/knowledge/deposit'))globalThis.__opsDepositReceipt={status:response.status,body:await response.clone().json()};return response;};[...document.querySelectorAll('button')].find(node=>node.innerText==='沉淀为知识').click();})()`);
    const deposit=await waitFor(()=>frame.evaluate('globalThis.__opsDepositReceipt'),'原包知识沉淀实际回执',90_000);
    await writeFile(join(output,'ops-deposit-receipt.json'),JSON.stringify(deposit,null,2));
    report.check('Ops 原按钮使用真实默认模型沉淀知识',deposit.status===200&&deposit.body.ok===true&&typeof deposit.body.id==='string',JSON.stringify(deposit));
    await waitFor(()=>frame.evaluate('document.body.innerText.includes("已沉淀为知识条目")'),'原包知识沉淀展示');
    const knowledge=(await route('/api/knowledge')).body.entries.find(entry=>entry.id===deposit.body.id);
    assert.ok(knowledge?.title);
    const ledger=async()=> (await Promise.all((await readdir(join(stateRoot,'audit-usage-ledger'))).filter(name=>name.endsWith('.jsonl')).map(name=>readFile(join(stateRoot,'audit-usage-ledger',name),'utf8')))).join('\n').split('\n').filter(Boolean).map(line=>JSON.parse(line));
    const owned=entries=>entries.filter(entry=>entry.event_type==='model.usage.recorded'&&entry.payload.plugin?.pluginId===ops.manifest.id);
    async function statsEvidence(phase, calls) {
      const all = await api('/api/settings/stats');
      const sessionStats = await api(`/api/settings/stats?sessionId=${encodeURIComponent(session)}`);
      const rows = all.items.filter(item => item.pluginId === ops.manifest.id);
      assert.equal(rows.length, 1, '真实插件调用应保留同一统计归属');
      assert.equal(rows[0].displayName, ops.manifest.name);
      assert.equal(rows[0].llmCallCount, calls);
      assert.equal(rows[0].successCount, 2);
      assert.ok(rows[0].totalTokens > 0, '保留实际供应商用量');
      assert.ok(sessionStats.items.every(item => !item.pluginId), '全局插件操作不归入当前会话');
      await writeFile(join(output, `stats-${phase}.json`), JSON.stringify({ all, session: sessionStats }, null, 2));
      return rows[0];
    }
    const before=owned(await ledger());
    report.check('诊断和知识用量归属真实插件及实例，未借用用户会话',before.length===2&&before.every(entry=>!entry.context.sessionId&&!entry.payload.sessionId&&entry.payload.status==='success'&&entry.payload.callIdentity.transport.httpStatus===200));
    await statsEvidence('completed', 2);
    report.check('原包成功调用进入统一统计并与当前会话用量分开', true);
    await frame.evaluate(`(() => {const controller=new AbortController();globalThis.__opsCancel=()=>controller.abort();globalThis.__opsCancelled=undefined;void fetch('/dsh-ops/api/chat',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({lang:'zh',messages:[{role:'user',content:'请逐项详细分析全部扫描结果。'}]}),signal:controller.signal}).then(response=>response.json()).then(value=>{globalThis.__opsCancelled={value};},error=>{globalThis.__opsCancelled={error:error.name};});})()`);
    await waitFor(async()=>new Set((await ledger()).filter(entry=>entry.event_type==='model.auxiliary.transport.observed'&&entry.payload.plugin?.pluginId===ops.manifest.id).map(entry=>entry.payload.plugin.operationId)).size===3,'第三次原页面请求已进入同一传输记录器');
    await frame.evaluate('globalThis.__opsCancel()');
    await waitFor(()=>frame.evaluate('globalThis.__opsCancelled?.error === "AbortError"'),'原页面取消真实请求');
    await waitFor(async()=>owned(await ledger()).length===3,'取消调用结算持久用量',30_000);
    const cancelled=owned(await ledger()).at(-1);
    const cancelledFact=(await ledger()).filter(entry=>entry.event_type==='model.auxiliary.transport.observed'&&entry.payload.plugin?.operationId===cancelled.payload.plugin.operationId).at(-1);
    report.check('页面取消传递至同一模型操作，不交付成功或重放',cancelled.payload.status==='cancelled'&&cancelledFact.payload.transport.outcome==='cancelled',JSON.stringify(cancelled.payload));
    const settledStats = await statsEvidence('cancelled', 3);
    report.check('取消调用仅结算一次并保留实际已观测用量', settledStats.failureCount === 1);
    const ids=owned(await ledger()).map(entry=>entry.event_id);
    await writeFile(join(output,'plugin-model-usage.json'),JSON.stringify(owned(await ledger()),null,2));
    await stop('electron-chat-before-restart.log');await start();
    await waitFor(async()=> (await plugins()).find(p=>p.manifest.id===ops.manifest.id)?.runtime?.state==='running','重启恢复 Ops',120_000);
    await settings();
    await waitFor(()=>frame.evaluate(`document.body.innerText.includes(${JSON.stringify(knowledge.title)})`),'重启后原页面知识条目');
    report.check('完整重启保留原包知识并且不重放模型操作',JSON.stringify(owned(await ledger()).map(entry=>entry.event_id))===JSON.stringify(ids)
      &&JSON.stringify((await route('/api/knowledge')).body.entries.find(entry=>entry.id===deposit.body.id))===JSON.stringify(knowledge)
      &&await frame.evaluate(`document.body.innerText.includes(${JSON.stringify(knowledge.title)})`));
    const restoredStats = await statsEvidence('restored', 3);
    report.check('重启从同一账本恢复完全相同的插件统计且不重复计数', JSON.stringify(restoredStats) === JSON.stringify(settledStats));
    await frame.evaluate(`[...document.querySelectorAll('.dshops-item')].find(node=>node.querySelector('strong')?.innerText===${JSON.stringify(knowledge.title)})?.scrollIntoView({block:'center',behavior:'instant'})`);
    await writeFile(join(output,'ops-knowledge-restored.png'),Buffer.from(await ui.screenshot(),'base64'));
    assert.deepEqual((await api(`/api/messages?scope=personal&sessionId=${session}`)).canonicalTurns,[nativeTurn]);
    await ui.click('[data-testid=settings-nav-stats]');
    await waitFor(()=>page.evaluate(`document.querySelector('.stats-split')?.innerText.includes(${JSON.stringify(ops.manifest.name)})`),'统计模型明细显示真实插件名称');
    await page.evaluate(`document.querySelector('.seg-toggle button:nth-child(2)').click()`);
    const pluginRow=await waitFor(()=>page.evaluate(`(() => {const row=[...document.querySelectorAll('.stats-row')].find(row=>row.querySelector('.row-name-txt')?.textContent===${JSON.stringify(ops.manifest.name)});if(!row)return null;row.click();return row.innerText;})()`),'按角色查看真实插件');
    report.check('现有统计页面两种视角保留插件名称和真实调用次数', pluginRow.split('\n').includes('3'));
    await writeFile(join(output,'ops-plugin-stats.png'),Buffer.from(await ui.screenshot(),'base64'));
    await api('/api/settings/stats/reset', {});
    assert.equal((await api('/api/settings/stats')).totals.llmCallCount, 0);
    await stop('electron-stats-before-reset-restart.log'); await start();
    const resetStats = await api('/api/settings/stats');
    report.check('全局重置后重启不从插件传输事实重新补入旧统计', resetStats.totals.llmCallCount === 0 && resetStats.items.length === 0);
    await writeFile(join(output,'stats-reset-restored.json'),JSON.stringify(resetStats,null,2));
    report.check('Ops 诊断页面无未捕获异常', errors.length === 0, errors.join('\n'));
    console.log(report.summary());
    // 单独流程不重复安装管理目标；业务门槛与管理流程分别报告。
    await stop('electron-chat.log');
    process.exitCode = 0;
  } else {
    const missing = { name: 'magi-dsh-compatibility-nonexistent', version: '0.0.1', spec: 'magi-dsh-compatibility-nonexistent@0.0.1', runtimeVersion };
    const rolledBack = await route('/api/adapt/apply', missing);
    report.check('上游失败安装撤销刚提交的精确版本豁免', rolledBack.body.ok === false && rolledBack.body.rolledBack === true, JSON.stringify(rolledBack));
    await settings(); await installFromPage(target.spec);
    const pending = await waitFor(installedTarget, 'Ops 原按钮在线提交真实目标', 240_000);
    const authorized = await waitForInstalledPlugin({ request: api, page, plugin: pending });
    report.check('Ops 原按钮在线安装后直接启用并运行', pending.manifest.version === target.version && pending.enabled && authorized.digest === pending.digest);
    await settings(); await installFromPage(target.spec);
    await waitFor(() => frame.evaluate('document.body.innerText.includes("安装完成")'), 'Ops 原页面显式重试成功');
    const retried = await installedTarget();
    report.check('用户回到原包显式重试读取现有安装，保留同一运行实例', retried.digest === authorized.digest && retried.runtime.generation === authorized.runtime.generation);
    for (const [label, enabled] of [['禁用', false], ['启用', true]]) {
      await waitFor(() => frame.evaluate(`(() => {const row=[...document.querySelectorAll('.dshops-row')].find(node=>node.innerText.includes('dsh-plugin-release-probe'));const button=row?.querySelector('button');if(!button||button.disabled||button.innerText!==${JSON.stringify(label)})return false;button.click();return true;})()`), `Ops 原页面${label}目标`);
      await waitFor(async () => { const current = await installedTarget(); return current.enabled === enabled
        && (enabled ? current.runtime?.state === 'running' : !current.runtime || current.runtime.state === 'stopped'); }, `Ops 真实目标${label}`);
    }
    const resumed = await installedTarget();
    report.check('Ops 原页面启停沿 daemon 全局生命周期真实停止并恢复目标', resumed.digest === retried.digest
      && resumed.runtime.instanceId && resumed.runtime.instanceId !== retried.runtime.instanceId);
    const applied = await route('/api/adapt/apply', target);
    report.check('Ops 对已授权包登记精确版本豁免并保持实际运行', applied.body.ok === true && (await installedTarget())?.runtime?.state === 'running', JSON.stringify(applied));
    const exemptions = await route('/api/adapt/exemptions');
    report.check('原包豁免目录读取 daemon 提交事实', exemptions.body.exemptions['dsh-plugin-release-probe@0.0.8']?.includes(runtimeVersion) && !exemptions.body.exemptions['magi-dsh-compatibility-nonexistent@0.0.1']);
    const scan = await api(`/api/plugins/${ops.manifest.id}/routes`, { method: 'GET', path: '/dsh-ops/api/scan', query: 'profile=magi' });
    report.check('Ops 健康扫描检查真实 magi profile 并返回结构化结果', scan.status === 200 && scan.body.profile === 'magi' && typeof scan.body.counts?.fatal === 'number', JSON.stringify(scan));
    await frame.evaluate('location.reload()'); await settings(true);
    report.check('原生页面显示真实扫描及精确版本豁免', (await frame.evaluate('document.body.innerText')).includes('dsh-plugin-release-probe@0.0.8'));
    await stop('electron-before-restart.log'); await start();
    await waitFor(async () => (await plugins()).find((p) => p.manifest.id === ops.manifest.id)?.runtime?.state === 'running', '重启恢复 Ops', 120_000);
    const persisted = await route('/api/adapt/exemptions');
    report.check('重启后版本豁免仍由 daemon 持久化恢复', persisted.body.exemptions['dsh-plugin-release-probe@0.0.8']?.includes(runtimeVersion));
    const revoked = await route('/api/adapt/revoke', { name: target.name });
    report.check('Ops 原包撤销豁免立即生效', revoked.body.ok === true && revoked.body.removed === 1 && Object.keys((await route('/api/adapt/exemptions')).body.exemptions).length === 0, JSON.stringify(revoked));
    const removed = await route('/api/adapt/remove', { name: target.name });
    report.check('原包卸载沿 daemon 停止并删除目标插件', removed.body.ok === true && !await installedTarget(), JSON.stringify(removed));
    const physical = await Promise.all([join(dshHome, 'profiles/magi'), join(stateRoot, 'extensions/data', ops.manifest.id, 'profiles/magi')].map((dir) => access(dir).then(() => true, () => false)));
    report.check('没有创建第二份物理 DSH profile 或豁免文件', physical.every((exists) => !exists));
    report.check('Electron 与上游原生页面无未捕获异常', errors.length === 0, errors.join('\n'));
    console.log(report.summary());
  }
} catch (error) {
  report.results.push({ name: '真实兼容策略验收完成', ok: false, detail: error.stack ?? String(error) });
  console.error(error); process.exitCode = 1;
  await writeFile(join(output, 'failure-plugins.json'), JSON.stringify(await plugins(), null, 2)).catch(() => {});
  if (frame) await writeFile(join(output, 'failure-page.txt'), await frame.evaluate('document.body.innerText')).catch(() => {});
  if (ui) await writeFile(join(output, 'failure.png'), Buffer.from(await ui.screenshot(), 'base64')).catch(() => {});
} finally {
  await writeFile(join(output, 'results.json'), JSON.stringify({ checks: report.results, errors, stateRoot,
    scope: chatOnly ? 'Ops 原页面诊断对话与原生真实模型对照；不代表 Ops 完整业务通过'
      : 'Ops 原包扫描/兼容豁免/在线安装/持久化/卸载；不代表生态完整业务通过' }, null, 2));
  if (app) await stop('electron.log');
}
