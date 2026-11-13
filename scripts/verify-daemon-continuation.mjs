import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { once } from "node:events";
import electron from "electron";
import { connectAppPage, waitFor } from "./lib/electron-harness.mjs";

// 真实 daemon + Chromium Web UI 的“停止后继续”验收：任何停止（用户主动停止、模型失败）之后，
// daemon 在读模型里给出 continuation，输入框的发送按钮据此变成“继续”，点击后重新激活并继续推进；
// 已完成的会话既没有 continuation，daemon 也拒绝 resume 请求。仅本地 Provider，不使用用户配置、
// 用户进程或付费模型。运行前构建 web/dist；daemon 通过正式开发脚本启动。
const root = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const fixtureRoot = await mkdtemp(join(tmpdir(), "magi-continuation-"));
const evidenceRoot = join(root, "target", "run-feedback-acceptance");
const requests = [];
const heldResponses = new Set();
let failMode = false;
// 每个验收标记允许被“挂起”的主线请求次数；挂起的请求在被停止（连接关闭）时才结束。
const holdBudget = { SCN_STOP: 1, SCN_TWICE: 2, SCN_SUPER: 1, SCN_RELOAD: 1, SCN_MULTI: 1, SCN_RESTART: 1 };
let partialFailureMode = true;
let partialStarted = false;
let partialStreamResponse = null;
let retryBudget = 0;
const MARKERS = ["SCN_STOP", "SCN_FAIL", "SCN_BAD_TOOL", "SCN_PARTIAL", "SCN_RETRY", "SCN_DONE", "SCN_TWICE", "SCN_SUPER", "SCN_RELOAD", "SCN_MULTI", "SCN_RESTART"];

const provider = createServer(async (request, response) => {
  try {
    if (request.method !== "POST") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ data: [{ id: "continuation-fixture" }] }));
      return;
    }
    let raw = "";
    for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw);
    const userMessages = (body.messages ?? []).filter(message => message.role === "user");
    // 请求尾部可能附带运行上下文消息；以最近一条带验收标记的用户消息为准。
        const text = JSON.stringify(userMessages.findLast(message => MARKERS.some(name => JSON.stringify(message).includes(name))) ?? "");
    const main = Array.isArray(body.tools) && body.tools.length > 0;
    const marker = MARKERS.find(name => text.includes(name)) ?? "OTHER";
    requests.push({ marker, main, resumed: text.includes("继续") });
    if (main && marker === "SCN_RETRY" && retryBudget > 0) {
      retryBudget -= 1;
      response.writeHead(503, { "content-type": "application/json", "retry-after": "5" });
      response.end(JSON.stringify({ error: { message: "fixture temporarily unavailable", type: "server_error" } }));
      return;
    }
    if (main && marker === "SCN_FAIL" && failMode) {
      response.writeHead(400, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: { message: "fixture rejects this request", type: "invalid_request_error" } }));
      return;
    }
    if (main && marker === "SCN_PARTIAL" && partialFailureMode && partialStarted) {
      response.writeHead(400, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: { message: "fixture rejects stream recovery", type: "invalid_request_error" } }));
      return;
    }
    if (main && marker === "SCN_PARTIAL" && partialFailureMode) {
      partialStarted = true;
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.write(`data: ${JSON.stringify({ id: "continuation-fixture", object: "chat.completion.chunk", model: "continuation-fixture", choices: [{ index: 0, delta: { role: "assistant", content: `SCN_PARTIAL_VISIBLE_${text.includes('summary') ? 'summary' : 'original'}` }, finish_reason: null }] })}\n\n`);
      partialStreamResponse = response;
      return;
    }
    if (main && (holdBudget[marker] ?? 0) > 0) {
      holdBudget[marker] -= 1;
      heldResponses.add(response);
      await new Promise(resolve => response.on("close", resolve));
      heldResponses.delete(response);
      return;
    }
    if (response.destroyed) return;
    response.writeHead(200, { "content-type": "text/event-stream" });
    if (main && marker === "SCN_BAD_TOOL") {
      response.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: { tool_calls: [{ index: 0,
        id: `unavailable-${requests.length}`, type: "function",
        function: { name: "unavailable_fixture_tool", arguments: "{}" } }] }, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] })}\n\ndata: [DONE]\n\n`);
      return;
    }
    for (const chunk of [
      { choices: [{ index: 0, delta: { role: "assistant", content: `${marker}_ANSWER${marker === 'SCN_RETRY' ? `_${text.includes('summary') ? 'summary' : 'original'}` : ''}` }, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110 } },
    ]) response.write(`data: ${JSON.stringify({ id: "continuation-fixture", object: "chat.completion.chunk", model: "continuation-fixture", ...chunk })}\n\n`);
    response.end("data: [DONE]\n\n");
  } catch (error) {
    response.writeHead(500);
    response.end(String(error));
  }
});

async function unusedPort() {
  const server = createServer();
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
}

await new Promise(resolve => provider.listen(0, "127.0.0.1", resolve));
const daemonPort = await unusedPort();
const cdpPort = await unusedPort();
const origin = `http://127.0.0.1:${daemonPort}`;
let daemon;
let browser;
let page;
let logs = "";
const errors = [];
const turnSubmissions = [];
const checks = [];
const pass = name => { checks.push(name); process.stdout.write(`✓ ${name}\n`); };

async function send(text) {
  await waitFor(() => page.evaluate(`document.querySelector('[data-testid="input-textarea"]')?.getAttribute('contenteditable') === 'true'`), "输入区可用", 20000);
  await page.evaluate(`(() => {
    const input = document.querySelector('[data-testid="input-textarea"]');
    if (input?.getAttribute('contenteditable') !== 'true') throw new Error('输入区不可用');
    input.focus(); input.textContent = ${JSON.stringify(text)};
    input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: ${JSON.stringify(text)} }));
  })()`);
  await waitFor(() => page.evaluate(`(() => {
    const button = document.querySelector('[data-testid="input-send-button"]');
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`), "输入提交按钮");
}

const click = (testid, label) => waitFor(() => page.evaluate(`(() => {
  const button = document.querySelector('[data-testid="${testid}"]');
  if (!button || button.disabled) return false;
  button.click(); return true;
})()`), label);
const exists = testid => page.evaluate(`!!document.querySelector('[data-testid="${testid}"]')`);
const continueAction = () => page.evaluate(`document.querySelector('[data-testid="input-continue-button"]')?.getAttribute('data-continue-action') ?? null`);
const currentSessionId = () => page.evaluate(`document.querySelector('#personal-session-content .session-item.active[data-session-id]')?.getAttribute('data-session-id')`);

async function reloadPage() {
  const loaded = new Promise((resolve, reject) => {
    const timer = setTimeout(() => { off(); reject(new Error("页面重载超时")); }, 20000);
    const off = page.on("Page.loadEventFired", () => { clearTimeout(timer); off(); resolve(); });
  });
  await page.call("Page.reload", { ignoreCache: true });
  await loaded;
  await waitFor(() => page.evaluate(`document.querySelector('[data-testid="input-textarea"]')?.getAttribute('contenteditable') === 'true'`), "重载后输入区可用");
}

async function checkMainlineNotice(name, continuation) {
  const content = await page.evaluate(`(() => { const banner = document.querySelector('[data-testid="agent-run-banner"]');
    return { present: Boolean(banner), text: banner?.innerText ?? '', buttons: banner?.querySelectorAll('button').length ?? 0,
      tone: banner?.className ?? '', dialogs: document.querySelectorAll('[role="dialog"][aria-modal="true"]').length }; })()`);
  assert.equal(content.buttons, 0, `${name} 普通主线错误不提供代理或归档操作`);
  assert.equal(content.present, false, `${name} 普通主线不显示额外运行横幅`);
  assert.equal(content.dialogs, 0, `${name} 不自动打开阻塞弹窗`);
  if (name !== 'user-stop') {
    const primary = await page.evaluate(`(() => {
      const card = [...document.querySelectorAll('[data-model-failure-code]')].at(-1);
      const turnId = card?.closest('[data-turn-id]')?.dataset.turnId;
      return { count: [...document.querySelectorAll('[data-model-failure-code]')]
        .filter(item => item.closest('[data-turn-id]')?.dataset.turnId === turnId).length,
        open: card?.querySelector('details')?.open,
        text: card?.innerText ?? '', toasts: document.querySelectorAll('.toast.error').length };
    })()`);
    assert.equal(primary.count, 1, `${name} 同一轮只有一张主要错误卡片`);
    assert.equal(primary.open, false, `${name} 技术诊断默认折叠`);
    assert.ok(primary.text.includes('错误详情') && !primary.text.includes('原始诊断'), '原因可读，诊断按需展开');
    assert.equal(primary.toasts, 0, `${name} 错误条目不额外叠加 toast`);
  }
  if (content.present) assert.ok(content.text.includes("继续"));
  if (continuation === "resume_turn") assert.ok(!content.text.includes("检查点"), "不得把接续轮次描述成检查点恢复");
  for (const width of [1280, 390]) {
    await page.call("Emulation.setDeviceMetricsOverride", { width, height: 900, deviceScaleFactor: 1, mobile: false });
    await page.evaluate(`new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))`);
    const overflow = await page.evaluate(`(() => { const row = document.querySelector('[data-testid="agent-run-banner"]') ?? document.querySelector('.model-failure');
      return row ? { scroll: row.scrollWidth, client: row.clientWidth } : null; })()`);
    await writeFile(join(evidenceRoot, `${name}-${width}.png`), Buffer.from((await page.call("Page.captureScreenshot", { format: "png" })).data, "base64"));
    if (overflow) assert.ok(overflow.scroll <= overflow.client + 1, `${name} ${width}px 错误提示不能溢出：${JSON.stringify(overflow)}`);
  }
  if (name !== 'user-stop') {
    await page.evaluate(`([...document.querySelectorAll('.model-failure-details summary')].at(-1)).click()`);
    await waitFor(() => page.evaluate(`([...document.querySelectorAll('.model-failure-details')].at(-1))?.open === true`), '展开诊断');
    const expanded = await page.evaluate(`(() => { const card = [...document.querySelectorAll('[data-model-failure-code]')].at(-1);
      return { text: card.innerText, scroll: card.scrollWidth, client: card.clientWidth }; })()`);
    assert.ok(expanded.text.includes('原始诊断') && expanded.text.includes('错误代码'), '展开保留完整失败证据');
    assert.ok(expanded.scroll <= expanded.client + 1, '窄屏展开诊断不溢出');
    await writeFile(join(evidenceRoot, `${name}-details-390.png`), Buffer.from((await page.call('Page.captureScreenshot', { format: 'png' })).data, 'base64'));
    await page.evaluate(`([...document.querySelectorAll('.model-failure-details summary')].at(-1)).click()`);
    assert.equal(await page.evaluate(`([...document.querySelectorAll('.model-failure-details')].at(-1)).open`), false);
    const previousErrors = errors.length;
    await page.call('Browser.setPermission', { permission: { name: 'clipboard-write' }, setting: 'denied', origin });
    await page.evaluate(`([...document.querySelectorAll('.model-failure-copy')].at(-1)).click()`);
    await waitFor(() => page.evaluate(`([...document.querySelectorAll('[data-model-failure-code]')].at(-1))?.querySelector('.model-failure-copy-error')?.innerText.includes('复制诊断信息失败')`), '复制失败就地反馈');
    assert.equal(errors.length, previousErrors, '复制失败不能变成未捕获异常');
    await page.call('Browser.resetPermissions');
    pass(`${name}：单一错误卡片、诊断默认折叠、展开及收起、无重复 toast`);
    pass(`${name}：剪贴板拒绝写入时就地反馈，无未捕获异常`);
  }
  await page.call("Emulation.clearDeviceMetricsOverride");
  pass(`${name}：无代理操作、不弹阻塞框、继续语义准确、宽窄布局可用`);
  return content;
}

async function readSessionEntry(sessionId) {
  const query = new URLSearchParams({ scope: "personal", sessionId });
  const response = await fetch(`${origin}/bootstrap?${query}`);
  assert.equal(response.status, 200, `读取会话引导失败 ${response.status}`);
  const model = await response.json();
  const sessions = model.runtimeReadModel?.details?.sessions ?? model.details?.sessions ?? [];
  return { entry: sessions.find(item => item.session_id === sessionId) ?? null };
}

async function startDaemon() {
  daemon = spawn("./scripts/dev-daemon.sh", [], {
    cwd: root, detached: true, stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, MAGI_PORT: String(daemonPort), MAGI_WEB_DEV: "0",
      MAGI_STATE_ROOT: join(fixtureRoot, "state"), MAGI_OPEN_BROWSER: "0",
      MAGI_TARGET_PRUNE_GIB: "1000", MAGI_BUILD_ID: "continuation-fixture",
      MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${provider.address().port}/v1`,
      MAGI_OPENAI_COMPAT_API_KEY: "local-fixture", MAGI_OPENAI_COMPAT_MODEL: "continuation-fixture" },
  });
  for (const stream of [daemon.stdout, daemon.stderr]) stream.on("data", chunk => { logs = (logs + chunk).slice(-60000); });
  await waitFor(async () => {
    if (daemon.exitCode !== null) throw new Error(`daemon 提前退出：${logs.slice(-3000)}`);
    return (await fetch(`${origin}/health`)).ok;
  }, "独立 daemon 启动", 600000, 250);
}

try {
  await mkdir(evidenceRoot, { recursive: true });
  await startDaemon();
  assert.equal((await (await fetch(`${origin}/health`)).json()).buildIdentity, "continuation-fixture");
  const entry = join(fixtureRoot, "browser.cjs");
  await writeFile(entry, `const { app, BrowserWindow } = require("electron");
app.setPath('userData', ${JSON.stringify(join(fixtureRoot, "chromium"))});
app.whenReady().then(() => { const win = new BrowserWindow({ show: false, webPreferences: { contextIsolation: true, nodeIntegration: false, sandbox: false } }); win.loadURL(${JSON.stringify(`${origin}/web.html`)}); });`);
  const browserEnv = { ...process.env };
  delete browserEnv.ELECTRON_RUN_AS_NODE;
  browser = spawn(electron, [entry, `--remote-debugging-port=${cdpPort}`, "--disable-gpu",
    "--disable-backgrounding-occluded-windows", "--disable-renderer-backgrounding", "--disable-background-timer-throttling"], { cwd: root, env: browserEnv, stdio: ["ignore", "pipe", "pipe"] });
  browser.stderr.on("data", chunk => { logs = (logs + chunk).slice(-60000); });
  await waitFor(async () => {
    try { page = await connectAppPage(cdpPort, daemonPort, errors); return true; } catch { return false; }
  }, "真实 Chromium 页面", 20000);
  await page.call("Network.enable");
  page.on("Network.requestWillBeSent", ({ request }) => {
    if (request.url === `${origin}/api/session/turn` && request.postData) turnSubmissions.push(JSON.parse(request.postData));
  });
  await waitFor(() => page.evaluate(`document.querySelector('[data-testid="input-textarea"]')?.getAttribute('contenteditable') === 'true'`), "输入区初始化");

  // ── 场景一：用户主动停止 ──────────────────────────────────────────────
  await send("SCN_STOP 请持续处理这件事。");
  await waitFor(() => heldResponses.size > 0, "本地模型正在采样");
  const sessionId = await waitFor(currentSessionId, "当前真实会话身份");
  assert.equal(await exists("input-continue-button"), false, "运行中不显示继续");
  await click("input-stop-button", "停止当前运行");
  await waitFor(() => page.evaluate(`!document.querySelector('[data-testid="input-stop-button"]')`), "停止完成");
  pass("用户主动停止后运行结束");

  const stopped = await waitFor(async () => {
    const { entry } = await readSessionEntry(sessionId);
    return entry?.continuation ? entry : false;
  }, "daemon 读模型给出 continuation");
  assert.equal(stopped.continuation, "resume_turn");
  pass("daemon 读模型对用户停止给出 continuation=resume_turn");
  await waitFor(async () => (await continueAction()) === "resume_turn", "发送按钮变成继续");
  pass("输入框为空时发送按钮变成继续，且与 daemon 判定一致");
  const stopNotice = await checkMainlineNotice("user-stop", stopped.continuation);
  assert.ok(!stopNotice.present || stopNotice.tone.includes("run-banner--muted"), "用户主动停止不冒充错误");

  // 有内容一定是发送：不是继续。此处不提交，只验证按钮形态。
  await page.evaluate(`(() => {
    const input = document.querySelector('[data-testid="input-textarea"]');
    input.focus(); input.textContent = '草稿';
    input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: '草稿' }));
  })()`);
  assert.equal(await exists("input-continue-button"), false, "输入框有内容时一定是发送");
  pass("输入框有内容时一定是发送，不是继续");
  await page.evaluate(`(() => {
    const input = document.querySelector('[data-testid="input-textarea"]');
    input.textContent = '';
    input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward' }));
  })()`);
  const waitIdle = label => waitFor(() => page.evaluate(`!document.querySelector('[data-testid="input-stop-button"]')`), label);
  const pageHas = text => page.evaluate(`document.body.innerText.includes(${JSON.stringify(text)})`);
  const continuationOf = async () => (await readSessionEntry(sessionId)).entry?.continuation ?? null;

  // 清掉草稿后，点击继续：重新激活被停止的那一轮并继续推进。
  await page.evaluate(`(() => {
    const input = document.querySelector('[data-testid="input-textarea"]');
    input.textContent = '';
    input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward' }));
  })()`);
  const submissionsBefore = turnSubmissions.length;
  await waitFor(() => exists("input-continue-button"), "清空草稿后继续按钮恢复");
  await click("input-continue-button", "点击继续");
  await waitFor(() => requests.some(item => item.main && item.marker === "SCN_STOP" && item.resumed), "继续触发新的模型请求");
  await waitFor(() => pageHas("SCN_STOP_ANSWER"), "继续后模型完成回答");
  await waitIdle("继续后运行结束");
  const resumeSubmission = turnSubmissions.slice(submissionsBefore).find(item => item.resume === true);
  assert.ok(resumeSubmission, "继续必须以 resume=true 提交会话 Turn");
  pass("点击继续：以 resume=true 重新激活并继续推进，模型完成回答");
  await waitFor(async () => (await continuationOf()) === null, "继续完成后 continuation 清除");
  await waitFor(async () => !(await exists("input-continue-button")), "继续完成后按钮恢复为发送", 10000);
  pass("继续完成后 daemon 不再给出 continuation，按钮恢复为发送");

  // ── 场景二：模型失败 ────────────────────────────────────────────────
  failMode = true;
  await send("SCN_FAIL 这个请求会被模型拒绝。");
  await waitFor(() => requests.some(item => item.main && item.marker === "SCN_FAIL"), "失败请求到达 Provider");
  await waitIdle("失败的运行结束");
  const failedAction = await waitFor(async () => (await continuationOf()) && await continueAction(), "失败后读模型与按钮都给出继续");
  assert.ok(["resume_turn", "resume_chain"].includes(failedAction), `失败后的继续方式：${failedAction}`);
  assert.equal(await continuationOf(), failedAction, "按钮显示的继续方式与 daemon 读模型一致");
  pass(`模型失败后 daemon 与按钮一致给出 ${failedAction}`);
  await checkMainlineNotice("model-failure", failedAction);
  failMode = false;
  await click("input-continue-button", "失败后点击继续");
  await waitFor(() => pageHas("SCN_FAIL_ANSWER"), "失败后继续成功完成");
  await waitIdle("失败后继续的运行结束");
  await waitFor(async () => (await continuationOf()) === null, "失败后继续完成，continuation 清除");
  pass("模型失败后点击继续：重新推进并完成");

  await send("SCN_BAD_TOOL 调用了当前环境不存在的工具。");
  await waitFor(() => requests.some(item => item.main && item.marker === "SCN_BAD_TOOL"), "工具校验请求");
  await waitIdle("工具校验失败结束");
  const toolAction = await waitFor(async () => (await continuationOf()) && await continueAction(), "工具失败继续资格");
  await waitFor(() => page.evaluate(`Boolean(document.querySelector('[data-model-failure-code]'))`), "真实工具失败诊断保留");
  await checkMainlineNotice("tool-unavailable", toolAction);

  // ── 场景二补充：已输出部分内容后连接中断 ────────────────────────────────
  for (const mode of ['original', 'summary']) {
    const update = await fetch(`${origin}/api/settings/update`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ key: 'conversationDisplayMode', value: mode }) });
    assert.equal(update.status, 200);
    await reloadPage();
    await waitFor(() => page.evaluate(`document.querySelector('.message-list')?.dataset.conversationDisplayMode === ${JSON.stringify(mode)}`), `${mode} 模式生效`);
    partialFailureMode = true;
    partialStarted = false;
    await send(`SCN_PARTIAL ${mode} 输出一半后连接中断。`);
    await waitFor(() => partialStreamResponse, "部分输出请求到达 Provider");
    await waitFor(() => pageHas(`SCN_PARTIAL_VISIBLE_${mode}`), "部分输出进入对话");
    partialStreamResponse.end();
    partialStreamResponse = null;
    await waitIdle("部分输出中断后的运行结束");
    await waitFor(() => page.evaluate(`Boolean(document.querySelector('[data-model-failure-code]'))`), "部分输出错误诊断保留");
    const partialBody = await page.evaluate(`document.body.innerText`);
    assert.ok(partialBody.includes(`SCN_PARTIAL_VISIBLE_${mode}`), `${mode} 流式中断后已输出内容仍保留`);
    await checkMainlineNotice(`partial-stream-${mode}`, "resume_chain");
    pass(`${mode} 部分输出后断流：已有内容保留、错误卡片单独呈现、无重复代理操作`);

    retryBudget = 1;
    await send(`SCN_RETRY ${mode} 暂时不可用后恢复。`);
    await waitFor(() => page.evaluate(`document.querySelector('.turn-runtime-stage')?.innerText.includes('重试')`), '退避重试反馈可见');
    assert.ok(await exists('input-stop-button'), '退避重试中可停止');
    await page.evaluate(`(() => {
      const input = document.querySelector('[data-testid="input-textarea"]');
      input.focus(); input.textContent = '重试期间保留的草稿';
      input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: '重试期间保留的草稿' }));
    })()`);
    await writeFile(join(evidenceRoot, `retrying-${mode}.png`), Buffer.from((await page.call('Page.captureScreenshot', { format: 'png' })).data, 'base64'));
    await waitFor(() => pageHas(`SCN_RETRY_ANSWER_${mode}`), '退避后正常答复');
    await waitFor(async () => (await continuationOf()) === null, '重试成功后恢复空闲');
    await waitFor(() => page.evaluate(`!document.querySelector('.turn-runtime-stage')`), '重试状态清除');
    const draft = await page.evaluate(`(() => { const input = document.querySelector('[data-testid="input-textarea"]'); return { text: input.innerText, focused: document.activeElement === input, retrying: Boolean(document.querySelector('.turn-runtime-stage')), errors: document.querySelectorAll('.toast.error').length }; })()`);
    assert.equal(draft.text, '重试期间保留的草稿');
    assert.equal(draft.focused, true, '重试结束不抢走输入焦点');
    assert.equal(draft.retrying, false, '完成后清除重试状态');
    assert.equal(draft.errors, 0, '自动恢复过程中不弹终态错误 Toast');
    await page.evaluate(`(() => { const input = document.querySelector('[data-testid="input-textarea"]'); input.textContent = ''; input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward' })); })()`);
    pass(`${mode} 自动重试：等待可见、保留草稿和焦点、恢复后清除运行提示`);

    retryBudget = 1;
    const requestsBeforeRetryStop = requests.length;
    await send(`SCN_RETRY ${mode} 重试等待中停止。`);
    await waitFor(() => page.evaluate(`document.querySelector('.turn-runtime-stage')?.innerText.includes('重试')`), '停止前重试已安排');
    await click('input-stop-button', '停止自动重试');
    await waitIdle('重试取消完成');
    await waitFor(async () => (await continueAction()) !== null, '重试停止后显式继续资格');
    assert.equal(requests.slice(requestsBeforeRetryStop).filter(item => item.marker === 'SCN_RETRY' && item.main).length, 1, '停止后不得继续发送下一次模型重试');
    assert.equal(await page.evaluate(`Boolean(document.querySelector('.turn-runtime-stage'))`), false);
    pass(`${mode} 停止重试：停止请求有效、无迟到重试、回到显式继续`);

  }
  partialFailureMode = false;

  // ── 场景三：正常完成的会话不可继续 ───────────────────────────────────
  await send("SCN_DONE 这是一个正常完成的任务。");
  await waitFor(() => pageHas("SCN_DONE_ANSWER"), "正常任务完成");
  await waitIdle("正常任务运行结束");
  assert.equal(await continuationOf(), null);
  assert.equal(await exists("input-continue-button"), false);
  const replay = await fetch(`${origin}/api/session/turn`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ ...resumeSubmission, requestId: `replay-${Date.now()}`, userMessageId: `replay-user-${Date.now()}` }),
  });
  assert.ok(replay.status >= 400 && replay.status < 500, `已完成的会话必须拒绝 resume，实际 ${replay.status}`);
  pass(`正常完成后没有 continuation，按钮是发送；daemon 拒绝 resume（${replay.status}）`);


  // ═══ 更全面的场景：每个场景使用新会话，互不影响 ═══════════════════════════
  const openNewSession = async () => {
    await click("sidebar-new-session", "新建会话");
    // 新会话在首次发送前还没有会话条目，只能等输入区重新可用且没有遗留的继续/停止按钮。
    await waitFor(() => page.evaluate(`document.querySelector('[data-testid="input-textarea"]')?.getAttribute('contenteditable') === 'true'
      && !document.querySelector('[data-testid="input-continue-button"], [data-testid="input-stop-button"]')`), "新会话输入区可用", 20000);
  };
  const emptyInput = () => page.evaluate(`(() => {
    const input = document.querySelector('[data-testid="input-textarea"]');
    input.textContent = '';
    input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward' }));
  })()`);
  const continuationFor = async id => (await readSessionEntry(id)).entry?.continuation ?? null;
  const sessionIdAfterSend = async text => {
    const previous = await currentSessionId();
    await send(text);
    return waitFor(async () => { const id = await currentSessionId(); return id && id !== previous ? id : false; }, "新会话身份");
  };
  const reload = reloadPage;
  const resumeCount = () => turnSubmissions.filter(item => item.resume === true).length;

  // ── 场景四：重载页面后仍然可继续；重复点击只提交一次 ──────────────────────
  await openNewSession();
  const reloadSession = await sessionIdAfterSend("SCN_RELOAD 重载验收。");
  await waitFor(() => heldResponses.size > 0, "SCN_RELOAD 正在采样");
  await click("input-stop-button", "停止 SCN_RELOAD");
  await waitIdle("SCN_RELOAD 停止完成");
  await waitFor(async () => (await continuationFor(reloadSession)) === "resume_turn", "SCN_RELOAD 读模型 continuation");
  await reload();
  await waitFor(async () => (await continueAction()) === "resume_turn", "重载后按钮仍是继续");
  pass("停止后重载页面，继续按钮由 daemon 快照恢复，与读模型一致");
  const resumesBefore = resumeCount();
  await page.evaluate(`(() => { const b = document.querySelector('[data-testid="input-continue-button"]'); b.click(); b.click(); b.click(); })()`);
  await waitFor(() => pageHas("SCN_RELOAD_ANSWER"), "重载后继续完成");
  await waitIdle("重载后继续运行结束");
  assert.equal(resumeCount() - resumesBefore, 1, "连点三次继续只能提交一次 resume");
  pass("连点三次继续只提交一次 resume，且重载后继续成功");

  // ── 场景五：连续两次停止，每次都能继续 ─────────────────────────────────
  await openNewSession();
  const twiceSession = await sessionIdAfterSend("SCN_TWICE 连续停止验收。");
  await waitFor(() => heldResponses.size > 0, "SCN_TWICE 第一次采样");
  await click("input-stop-button", "第一次停止");
  await waitIdle("第一次停止完成");
  await waitFor(async () => (await continueAction()) === "resume_turn", "第一次停止后按钮是继续");
  await click("input-continue-button", "第一次继续");
  await waitFor(() => heldResponses.size > 0, "继续后第二次被挂起");
  await waitFor(() => exists("input-stop-button"), "第二次运行中显示停止");
  assert.equal(await exists("input-continue-button"), false, "继续后的运行中不显示继续");
  await click("input-stop-button", "第二次停止");
  await waitIdle("第二次停止完成");
  await waitFor(async () => (await continueAction()) === "resume_turn" && (await continuationFor(twiceSession)) === "resume_turn", "第二次停止后仍可继续");
  await click("input-continue-button", "第二次继续");
  await waitFor(() => pageHas("SCN_TWICE_ANSWER"), "两次停止后最终完成");
  await waitIdle("最终运行结束");
  await waitFor(async () => (await continuationFor(twiceSession)) === null && !(await exists("input-continue-button")), "完成后无 continuation");
  pass("连续停止两次，每次读模型与按钮都给出继续，最终完成");

  // ── 场景六：被新消息取代的取消不可继续 ────────────────────────────────
  await openNewSession();
  const superSession = await sessionIdAfterSend("SCN_SUPER 将被新消息取代。");
  await waitFor(() => heldResponses.size > 0, "SCN_SUPER 正在采样");
  await page.evaluate(`(() => {
    const input = document.querySelector('[data-testid="input-textarea"]');
    input.focus(); input.textContent = 'SCN_DONE 替代输入';
    input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: 'SCN_DONE 替代输入' }));
  })()`);
  await click("input-replace-button", "用新输入取代当前运行");
  await waitFor(() => pageHas("SCN_DONE_ANSWER"), "替代输入完成");
  await waitIdle("替代后运行结束");
  assert.equal(await continuationFor(superSession), null, "被取代的取消不是停止，不可继续");
  assert.equal(await exists("input-continue-button"), false);
  pass("被新输入取代的取消不产生 continuation，按钮保持发送");

  // ── 场景七：多会话互不串扰 ────────────────────────────────────────────
  await openNewSession();
  const multiSession = await sessionIdAfterSend("SCN_MULTI 多会话验收。");
  await waitFor(() => heldResponses.size > 0, "SCN_MULTI 正在采样");
  await click("input-stop-button", "停止 SCN_MULTI");
  await waitIdle("SCN_MULTI 停止完成");
  await waitFor(async () => (await continueAction()) === "resume_turn", "SCN_MULTI 显示继续");
  await openNewSession();
  const otherSession = await sessionIdAfterSend("SCN_DONE 另一个会话正常完成。");
  await waitFor(() => pageHas("SCN_DONE_ANSWER"), "另一个会话完成");
  await waitIdle("另一个会话运行结束");
  assert.notEqual(otherSession, multiSession);
  assert.equal(await exists("input-continue-button"), false, "另一个会话不显示继续");
  await page.evaluate(`document.querySelector('.session-item[data-session-id="${multiSession}"]').click()`);
  await waitFor(async () => (await currentSessionId()) === multiSession && (await continueAction()) === "resume_turn", "切回被停止的会话，继续按钮恢复");
  pass("切换会话后继续状态按会话区分，不串扰");
  await click("input-continue-button", "切回后继续");
  await waitFor(() => pageHas("SCN_MULTI_ANSWER"), "切回后继续完成");
  await waitIdle("切回后继续运行结束");

  // ── 场景八：daemon 被强杀重启后，中断的会话可继续，且不会自动重放 ──────────
  await openNewSession();
  const restartSession = await sessionIdAfterSend("SCN_RESTART 重启验收。");
  await waitFor(() => heldResponses.size > 0, "SCN_RESTART 正在采样");
  const exited = once(daemon, "exit");
  process.kill(-daemon.pid, "SIGKILL");
  await exited;
  await waitFor(async () => { try { await fetch(`${origin}/health`); return false; } catch { return true; } }, "原 daemon 端口释放");
  const requestsBeforeRestart = requests.length;
  await startDaemon();
  await reload();
  await waitFor(async () => (await currentSessionId()) === restartSession || (await page.evaluate(`!!document.querySelector('.session-item[data-session-id="${restartSession}"]')`)), "重启后会话仍在");
  if ((await currentSessionId()) !== restartSession) {
    await page.evaluate(`document.querySelector('.session-item[data-session-id="${restartSession}"]').click()`);
  }
  const restartedKind = await waitFor(async () => await continuationFor(restartSession), "重启后读模型给出 continuation", 30000);
  assert.ok(["resume_turn", "resume_chain"].includes(restartedKind), `重启后的继续方式：${restartedKind}`);
  await waitFor(async () => (await continueAction()) === restartedKind, "重启后按钮与读模型一致");
  assert.equal(requests.length, requestsBeforeRestart, "重启和读取不能自动重新执行");
  pass(`daemon 强杀重启后：中断的会话给出 ${restartedKind}，按钮一致，且没有自动重放`);
  await click("input-continue-button", "重启后点击继续");
  await waitFor(() => pageHas("SCN_RESTART_ANSWER"), "重启后继续完成");
  await waitIdle("重启后继续运行结束");
  await waitFor(async () => (await continuationFor(restartSession)) === null && !(await exists("input-continue-button")), "重启后继续完成，无 continuation");
  pass("重启后点击继续：重新推进并完成，continuation 清除");

  process.stdout.write(JSON.stringify({ passed: true, providerRequests: requests.length, checks }, null, 2) + "\n");
} catch (error) {
  process.stderr.write(JSON.stringify(await page?.evaluate("document.body.innerText.slice(-3000)").catch(() => null)) + "\n");
  process.stderr.write(`${logs.slice(-8000)}\n`);
  throw error;
} finally {
  partialStreamResponse?.destroy();
  for (const response of heldResponses) response.destroy();
  page?.close();
  for (const child of [browser, daemon]) {
    if (!child || child.exitCode !== null) continue;
    if (child === daemon) { try { process.kill(-child.pid, "SIGTERM"); } catch {} }
    else child.kill("SIGTERM");
    await Promise.race([new Promise(resolve => child.once("exit", resolve)), new Promise(resolve => setTimeout(resolve, 5000))]);
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
  }
  provider.closeAllConnections();
  await new Promise(resolve => provider.close(resolve));
  await rm(fixtureRoot, { recursive: true, force: true });
}
