import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn, execFileSync } from "node:child_process";
import { once } from "node:events";
import electron from "electron";
import { connectAppPage, waitFor } from "./lib/electron-harness.mjs";
import { appServerRequest } from "./lib/app-server-harness.mjs";

// 真实 daemon + Chromium Web UI 的输入合同验收。仅本地 Provider，不使用用户配置、
// 用户进程或付费模型。运行前构建 web/dist；daemon 通过正式开发脚本启动。
const root = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const fixtureRoot = await mkdtemp(join(tmpdir(), "magi-input-queue-"));
const requests = [];
const heldResponses = new Set();
let releaseReplacement;
const replacementGate = new Promise(resolve => { releaseReplacement = resolve; });
const goalMarker = "GOAL_USAGE";
const goalObjective = `${goalMarker} 验证长期目标无任务预算，累计用量只做统计。`;
let goalRequests = 0;
let agentChildHeld = false;
let releaseAgentChild;
const agentChildGate = new Promise(resolve => { releaseAgentChild = resolve; });
function writeToolResponse(response, name, args) {
  response.writeHead(200, { "content-type": "text/event-stream" });
  response.write(`data: ${JSON.stringify({ id: "goal-fixture", object: "chat.completion.chunk", model: "input-queue-fixture", choices: [{ index: 0, delta: { role: "assistant", tool_calls: [{ index: 0, id: `goal-${goalRequests}`, type: "function", function: { name: `magi_builtin_${name}`, arguments: JSON.stringify(args) } }] }, finish_reason: null }] })}\n\n`);
  response.write(`data: ${JSON.stringify({ id: "goal-fixture", object: "chat.completion.chunk", model: "input-queue-fixture", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }] })}\n\n`);
  // 工具响应故意缺少 usage：不能把未知用量当作执行门禁。
  response.end("data: [DONE]\n\n");
}
let releaseRoot;
const rootGate = new Promise((resolve) => { releaseRoot = resolve; });
let releaseGuidance;
const guidanceGate = new Promise((resolve) => { releaseGuidance = resolve; });
const provider = createServer(async (request, response) => {
  try {
    if (request.method !== "POST") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ data: [{ id: "input-queue-fixture" }] }));
      return;
    }
    let raw = "";
    for await (const chunk of request) raw += chunk;
    const body = JSON.parse(raw);
    requests.push(body);
    const text = JSON.stringify(body.messages ?? []);
    if (text.includes('WEB_AGENT_NEXT')) {
      // 同一会话后续主线独立完成，保留原代理审计历史不应再次唤起它。
    } else if (text.includes('WEB_SESSION_AGENT')) {
      const coordinator = body.tools?.some(tool => tool.function?.name === 'magi_builtin_agent_spawn');
      const results = (body.messages ?? []).filter(message => message.role === 'tool').flatMap(message => {
        try { return [JSON.parse(message.content)]; } catch { return []; }
      });
      if (coordinator) {
        const child = results.findLast(result => result.child_task_id)?.child_task_id;
        if (!child) {
          writeToolResponse(response, 'agent_spawn', {
            task_name:'web_transfer', display_name:'Web 转交验收代理', role:'executor', lifetime:'session',
            delegation:{purpose:'user_requested',reason:'验收请求明确要求真实代理独立执行并通过 Web 转交'},
            goal:'WEB_SESSION_AGENT_CHILD 返回独立代理执行证据',
            context_package:{summary:'WEB_SESSION_AGENT_CHILD',constraints:[],expected_output:'WEB_SESSION_AGENT_CHILD_DONE',references:[]},
          });
          return;
        }
        if (!results.findLast(result => result.tool === 'agent_wait')?.all_terminal) {
          writeToolResponse(response, 'agent_wait', {task_ids:[child],timeout_ms:60000});
          return;
        }
      } else {
        agentChildHeld = true;
        heldResponses.add(response);
        await agentChildGate;
        heldResponses.delete(response);
      }
    }
    if (text.includes(goalMarker)) {
      goalRequests += 1;
      const results = (body.messages ?? []).filter((message) => message.role === "tool").flatMap((message) => {
        try { return [JSON.parse(message.content)]; } catch { return []; }
      });
      const latest = results.findLast((result) => result.goal)?.goal;
      const forced = (body.tool_choice?.function?.name ?? (body.tools?.length === 1 ? body.tools[0].function?.name : null))?.replace(/^magi_builtin_/u, "");
      const plan = results.findLast((result) => result.plan)?.plan;
      if (forced === "get_goal") { writeToolResponse(response, "get_goal", {}); return; }
      if (forced === "update_plan" && latest) {
        writeToolResponse(response, "update_plan", { planId: plan?.planId ?? null, expectedRevision: plan?.revision ?? 0,
          expectedGoalId: latest.goalId, expectedGoalControlRevision: latest.controlRevision,
          language: "zh-CN", plan: [{ itemId: plan?.items?.[0]?.itemId ?? null, step: "验证目标完成", status: goalRequests >= 8 ? "completed" : "in_progress" }] });
        return;
      }
      if (!latest) { writeToolResponse(response, "create_goal", { objective: goalObjective }); return; }
      if (goalRequests < 8) { writeToolResponse(response, "get_goal", {}); return; }
      if (plan?.items?.some((item) => item.status !== "completed")) {
        writeToolResponse(response, "update_plan", { planId: plan.planId, expectedRevision: plan.revision,
          expectedGoalId: latest.goalId, expectedGoalControlRevision: latest.controlRevision,
          language: "zh-CN", plan: plan.items.map((item) => ({ itemId: item.itemId, step: item.step, status: "completed" })) });
        return;
      }
      if (latest.status !== "complete") {
        writeToolResponse(response, "update_goal", { goal_id: latest.goalId, expected_revision: latest.controlRevision,
          expected_plan_revision: plan?.revision ?? null, status: "complete", completion_summary: "目标执行已验证", evidence_refs: ["local-provider-goal-fixture"] });
        return;
      }
    }
    if (!text.includes(goalMarker) && text.includes("INBOX_ROOT") && !text.includes("INBOX_GUIDE") && !text.includes("INBOX_NEXT")) {
      heldResponses.add(response);
      await rootGate;
      heldResponses.delete(response);
    }
    if (!text.includes(goalMarker) && text.includes("INBOX_GUIDE") && !text.includes("INBOX_NEXT")) {
      await guidanceGate;
    }
    if (text.includes("REPLACE_ROOT") && !text.includes("REPLACE_NEW") && !text.includes("REPLACE_FIFO") && !text.includes(goalMarker)) {
      heldResponses.add(response);
      await replacementGate;
      heldResponses.delete(response);
    }
    const answer = text.includes('WEB_AGENT_NEXT') ? 'WEB_AGENT_NEXT_DONE'
      : text.includes('WEB_SESSION_AGENT_CHILD') ? 'WEB_SESSION_AGENT_CHILD_DONE'
      : text.includes('WEB_SESSION_AGENT') ? 'WEB_SESSION_AGENT_ROOT_DONE'
      : text.includes("REPLACE_FIFO") && !text.includes(goalMarker) ? "REPLACE_FIFO_DONE"
      : text.includes("REPLACE_NEW") && !text.includes(goalMarker) ? "REPLACE_NEW_DONE"
      : text.includes(goalMarker) ? "GOAL_USAGE_DONE" : text.includes("INBOX_NEXT") ? "INBOX_NEXT_DONE"
      : text.includes("INBOX_GUIDE") ? "INBOX_GUIDE_DONE" : "INBOX_ROOT_DONE";
    if (response.destroyed) return;
    response.writeHead(200, { "content-type": "text/event-stream" });
    for (const chunk of [
      { choices: [{ index: 0, delta: { role: "assistant", content: answer }, finish_reason: null }] },
      { choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 200, completion_tokens: text.includes(goalMarker) ? 1_000_000 : 10, total_tokens: text.includes(goalMarker) ? 1_000_200 : 210 } },
    ]) response.write(`data: ${JSON.stringify({ id: "input-fixture", object: "chat.completion.chunk", model: "input-queue-fixture", ...chunk })}\n\n`);
    response.end("data: [DONE]\n\n");
  } catch (error) {
    response.writeHead(500);
    response.end(String(error));
  }
});

async function unusedPort() {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  await new Promise((resolve) => server.close(resolve));
  return port;
}

await new Promise((resolve) => provider.listen(0, "127.0.0.1", resolve));
const daemonPort = await unusedPort();
const cdpPort = await unusedPort();
const origin = `http://127.0.0.1:${daemonPort}`;
let daemon;
let browser;
let page;
let logs = "";
const errors = [];
const guideSubmissions = [];
const editSubmissions = [];
const editResponses = [];
const removeSubmissions = [];

async function send(text, selector = '[data-testid="input-followup-send-button"], [data-testid="input-send-button"]') {
  await page.evaluate(`(() => {
    const input = document.querySelector('[data-testid="input-textarea"]');
    if (input?.getAttribute('contenteditable') !== 'true') throw new Error('输入区不可用');
    input.focus(); input.textContent = ${JSON.stringify(text)};
    input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: ${JSON.stringify(text)} }));
  })()`);
  await waitFor(() => page.evaluate(`(() => {
    const button = document.querySelector(${JSON.stringify(selector)});
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`), "输入提交按钮");
}

const queue = () => page.evaluate(`Array.from(document.querySelectorAll('.ia-queue-item')).map(row => ({
  text: row.innerText, disabled: row.querySelector('.ia-queue-guide')?.disabled,
  title: row.querySelector('.ia-queue-guide')?.title,
}))`);

async function reloadPage() {
  const loaded = new Promise((resolve, reject) => {
    const timer = setTimeout(() => { off(); reject(new Error('页面重载超时')); }, 15000);
    const off = page.on('Page.loadEventFired', () => { clearTimeout(timer); off(); resolve(); });
  });
  await page.call('Page.reload', { ignoreCache: true });
  await loaded;
}

async function startDaemon() {
  daemon = spawn("./scripts/dev-daemon.sh", [], {
    cwd: root, detached: true, stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, MAGI_PORT: String(daemonPort), MAGI_WEB_DEV: "0",
      MAGI_STATE_ROOT: join(fixtureRoot, "state"), MAGI_OPEN_BROWSER: "0",
      MAGI_TARGET_PRUNE_GIB: "1000", MAGI_BUILD_ID: "input-queue-worktree-fixture",
      MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${provider.address().port}/v1`,
      MAGI_OPENAI_COMPAT_API_KEY: "local-fixture", MAGI_OPENAI_COMPAT_MODEL: "input-queue-fixture" },
  });
  for (const stream of [daemon.stdout, daemon.stderr]) stream.on("data", (chunk) => { logs = (logs + chunk).slice(-60000); });
  await waitFor(async () => {
    if (daemon.exitCode !== null) throw new Error(`daemon 提前退出：${logs.slice(-3000)}`);
    return (await fetch(`${origin}/health`)).ok;
  }, "独立 daemon 启动", 240000, 250);
}

try {
  await startDaemon();
  const health = await (await fetch(`${origin}/health`)).json();
  assert.equal(health.buildIdentity, "input-queue-worktree-fixture");
  assert.equal((await fetch(`${origin}/web.html`)).status, 200);
  const entry = join(fixtureRoot, "browser.cjs");
  await writeFile(entry, `const { app, BrowserWindow } = require("electron");
app.setPath('userData', ${JSON.stringify(join(fixtureRoot, "chromium"))});
app.whenReady().then(() => { const win = new BrowserWindow({ show: false, webPreferences: { contextIsolation: true, nodeIntegration: false, sandbox: false } }); win.loadURL(${JSON.stringify(`${origin}/web.html`)}); });`);
  browser = spawn(electron, [entry, `--remote-debugging-port=${cdpPort}`, "--disable-gpu"], {
    cwd: root, stdio: ["ignore", "pipe", "pipe"],
  });
  browser.stderr.on("data", (chunk) => { logs = (logs + chunk).slice(-60000); });
  await waitFor(async () => {
    try { page = await connectAppPage(cdpPort, daemonPort, errors); return true; } catch { return false; }
  }, "真实 Chromium 页面", 20000);
  await page.call('Network.enable');
  page.on('Network.requestWillBeSent', ({ request }) => {
    if (request.url === `${origin}/api/session/queue/guide` && request.postData) {
      guideSubmissions.push(JSON.parse(request.postData));
    }
    if (request.url === `${origin}/api/session/queue/update` && request.postData) {
      editSubmissions.push(JSON.parse(request.postData));
    }
    if (request.url === `${origin}/api/session/queue/remove` && request.postData) {
      removeSubmissions.push(JSON.parse(request.postData));
    }
  });
  page.on('Network.responseReceived', ({ response }) => {
    if (response.url === `${origin}/api/session/queue/update`) editResponses.push(response.status);
  });
  await waitFor(() => page.evaluate(`document.querySelector('[data-testid="input-textarea"]')?.getAttribute('contenteditable') === 'true'`), "输入区初始化");
  await send("INBOX_ROOT 请等待后续补充。");
  await waitFor(() => heldResponses.size > 0, "本地模型正在采样");
  await send("INBOX_GUIDE 请在当前任务下一步生效。");
  const pending = await waitFor(async () => { const rows = await queue(); return rows.length === 1 && rows[0]; }, "消息进入持久队列");
  assert.equal(pending.disabled, false, pending.title);
  const sessionId = await page.evaluate(`document.querySelector('#recent-session-content .session-item.active[data-session-id]')?.getAttribute('data-session-id')`);
  assert.ok(sessionId, "队列验收需要当前真实会话身份");
  const readQueue = async () => {
    const response = await fetch(`${origin}/api/session/queue?sessionId=${encodeURIComponent(sessionId)}`);
    assert.equal(response.status, 200);
    return response.json();
  };
  const beforeEdit = (await readQueue()).queuedTurns[0];
  await page.evaluate(`document.querySelector('.ia-queue-item button[aria-label="编辑消息"]').click()`);
  await waitFor(() => page.evaluate(`!!document.querySelector('[data-testid="input-queue-save-button"]')`), '编辑使用保存入口');
  assert.deepEqual((await readQueue()).queuedTurns[0], beforeEdit, '点击编辑不能删除或修改原输入');
  // 保存仍是原 Input；顺序、创建时间及 request 身份不变，重载后可继续补充原 Turn。
  await send('INBOX_GUIDE 保存编辑后在当前任务下一步生效。', '[data-testid="input-queue-save-button"]');
  await waitFor(() => page.evaluate(`!document.querySelector('[data-testid="input-editing-bar"]')`), '服务端保存成功后清空编辑器');
  const afterEdit = (await readQueue()).queuedTurns[0];
  for (const key of ['queueId', 'queuePosition', 'requestId', 'acceptedAt', 'sessionId']) {
    assert.equal(afterEdit[key], beforeEdit[key], `${key} 保持原输入身份`);
  }
  assert.equal(afterEdit.revision, beforeEdit.revision + 1);
  assert.equal(afterEdit.text, 'INBOX_GUIDE 保存编辑后在当前任务下一步生效。');
  assert.equal(editSubmissions.length, 1);
  // 同一客户端显示旧版本后，另一客户端保存的新内容不能被无版本或过期操作改投递/删除。
  const rpc = (method, params) => appServerRequest(`${origin.replace('http:', 'ws:')}/api/app-server`, method, params);
  for (const action of ['remove', 'retry', 'guide']) {
    for (const revision of [0, beforeEdit.revision]) {
      const params = { sessionId, queueId: afterEdit.queueId, expectedRevision: revision,
        ...(action === 'guide' ? { expectedTurnId: afterEdit.guide.targetTurnId } : {}) };
      const rejected = await fetch(`${origin}/api/session/queue/${action}`, {
        method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(params),
      });
      assert.equal(rejected.status, revision === beforeEdit.revision ? 409 : 400, `${action}: ${revision}`);
      await assert.rejects(rpc(`session/queue/${action}`, params),
        new RegExp(`App Server ${revision === beforeEdit.revision ? '-32010' : '-32602'}:`));
      assert.deepEqual((await readQueue()).queuedTurns[0], afterEdit);
    }
  }
  for (const action of ['remove', 'retry', 'guide']) {
    for (const revision of [undefined, null]) {
      await assert.rejects(rpc(`session/queue/${action}`, { sessionId, queueId: afterEdit.queueId,
        expectedRevision: revision, ...(action === 'guide' ? { expectedTurnId: afterEdit.guide.targetTurnId } : {}) }),
      /App Server -32602:/);
    }
  }
  await reloadPage();
  await waitFor(async () => { const rows = await queue(); return rows.length === 1 && rows[0].disabled === false; }, "重载后补充能力恢复");
  await page.evaluate(`document.querySelector('.ia-queue-guide').click()`);
  await waitFor(() => guideSubmissions.length === 1, '真实引导提交携带输入 CAS 身份');
  assert.ok(Number.isSafeInteger(guideSubmissions[0].expectedRevision) && guideSubmissions[0].expectedRevision > 0);
  assert.ok(guideSubmissions[0].expectedTurnId);
  await waitFor(async () => (await queue()).length === 0, "已接纳补充立即退出队列");
  const delivery = () => page.evaluate(`document.querySelector('[data-input-delivery]')?.getAttribute('data-input-delivery')`);
  await waitFor(async () => (await delivery()) === 'waiting', "已接收补充明确等待下一步");
  const replayGuide = async () => {
    const params = guideSubmissions[0];
    const before = await readQueue();
    const replayed = await fetch(`${origin}/api/session/queue/guide`, {
      method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(params),
    });
    assert.equal(replayed.status, 200);
    assert.deepEqual(await replayed.json(), before);
    assert.deepEqual(await rpc('session/queue/guide', params), before);
    const stale = { ...params, expectedRevision: beforeEdit.revision };
    const rejected = await fetch(`${origin}/api/session/queue/guide`, {
      method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(stale),
    });
    assert.equal(rejected.status, 409, '绑定之后也不能把旧编辑版本误报成补充成功');
    await assert.rejects(rpc('session/queue/guide', stale), /App Server -32010:/);
    assert.deepEqual(await readQueue(), before);
  };
  await replayGuide();
  await send("INBOX_NEXT 请在下一轮执行。");
  await waitFor(async () => (await queue()).length === 1, "下一轮输入保留在队列");
  const nextQueued = (await readQueue()).queuedTurns[0];
  releaseRoot();
  await waitFor(() => requests.some((request) => JSON.stringify(request.messages).includes("INBOX_GUIDE")), "补充进入后续模型请求");
  await waitFor(async () => (await delivery()) === 'in_request', "冻结请求后的实时消费回执");
  await reloadPage();
  await waitFor(async () => (await delivery()) === 'in_request', "重载保留请求在途回执");
  await replayGuide();
  await waitFor(() => page.evaluate(`(() => {
    const button = document.querySelector('.ia-queue-item button[aria-label="编辑消息"]');
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`), '领取竞争前保留编辑草稿');
  await waitFor(() => page.evaluate(`!!document.querySelector('[data-testid="input-queue-save-button"]')`), '保存草稿不改变投递');
  // 视图草稿不锁住原 Input；原输入可以按 FIFO 正常被领取。
  releaseGuidance();
  await waitFor(() => requests.some((request) => JSON.stringify(request.messages).includes("INBOX_NEXT")), "队列消费后启动下一轮");
  await waitFor(async () => (await queue()).length === 0, "已发送消息从待处理列表移出");
  await waitFor(() => page.evaluate(`document.body.innerText.includes('INBOX_NEXT_DONE') && !document.querySelector('[data-testid="input-stop-button"]')`), "下一轮完成");
  const requestsBeforeConflict = requests.length;
  await send('INBOX_LATE_DRAFT 领取之后仍保留我的修改。', '[data-testid="input-queue-save-button"]');
  await waitFor(() => editResponses.length === 2, '保存取得服务端响应');
  assert.deepEqual(editResponses, [200, 409]);
  await waitFor(() => page.evaluate(`(() => {
    const button = document.querySelector('[data-testid="input-queue-save-button"]');
    return button && !button.disabled && document.querySelector('[data-testid="input-editing-bar"] [role="alert"]')?.textContent.includes('输入已被领取');
  })()`), '领取后的保存明确返回冲突');
  assert.equal(await page.evaluate(`document.querySelector('[data-testid="input-textarea"]').textContent`), 'INBOX_LATE_DRAFT 领取之后仍保留我的修改。');
  assert.equal(requests.length, requestsBeforeConflict, '冲突不能把草稿静默投递到新 Turn');
  await page.evaluate(`document.querySelector('[data-testid="input-editing-bar"] button').click()`);
  assert.equal(editSubmissions.length, 2);
  await reloadPage();
  await waitFor(async () => (await delivery()) === 'used', '重载保留最终消费回执');
  const users = await page.evaluate(`Array.from(document.querySelectorAll('[data-source="user"]')).map(row => ({ text: row.innerText, turnId: row.getAttribute('data-turn-id') }))`);
  const rootInput = users.filter((row) => row.text.includes("INBOX_ROOT"));
  const guideInput = users.filter((row) => row.text.includes("INBOX_GUIDE"));
  const nextInput = users.filter((row) => row.text.includes("INBOX_NEXT"));
  assert.equal(rootInput.length, 1);
  assert.equal(guideInput.length, 1);
  assert.equal(nextInput.length, 1);
  assert.equal(rootInput[0].turnId, guideInput[0].turnId);
  assert.notEqual(rootInput[0].turnId, nextInput[0].turnId);
  const mistakenGuide = { sessionId, queueId: nextQueued.queueId, expectedRevision: nextQueued.revision,
    expectedTurnId: nextInput[0].turnId };
  const providerRequestsBeforeReplay = requests.length;
  const claimedAsGuide = await fetch(`${origin}/api/session/queue/guide`, {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(mistakenGuide),
  });
  assert.equal(claimedAsGuide.status, 409, '普通下一轮领取不能冒充补充成功回执');
  await assert.rejects(rpc('session/queue/guide', mistakenGuide), /App Server -32010:/);
  assert.equal(requests.length, providerRequestsBeforeReplay, '读取错误投递回执不能创建新模型请求');
  await send("REPLACE_ROOT 原任务保持在途。");
  await waitFor(() => requests.some(request => JSON.stringify(request.messages).includes("REPLACE_ROOT")), "原执行进入请求");
  await send("REPLACE_FIFO 保留原下一轮。");
  await waitFor(async () => (await queue()).length === 1, "普通下一轮先排队");
  await send("REPLACE_NEW 停止原任务并优先改做。", '[data-testid="input-replace-button"]');
  await waitFor(() => page.evaluate(`document.body.innerText.includes('REPLACE_FIFO_DONE') && !document.querySelector('[data-testid="input-stop-button"]')`), "替代输入先执行，随后普通队列完成", 30000);
  const replacedResponse = await fetch(`${origin}/api/messages?scope=personal&sessionId=${encodeURIComponent(sessionId)}`);
  assert.equal(replacedResponse.status, 200);
  const replaced = await replacedResponse.json();
  const replacementTurns = replaced.canonicalTurns.filter(turn => turn.items.some(item => item.kind === 'user_message' && item.content?.includes('REPLACE_'))).sort((a,b) => a.turnSeq-b.turnSeq);
  assert.equal(replacementTurns.length, 3);
  assert.equal(replacementTurns[0].status, 'cancelled');
  assert.ok(replacementTurns[1].items.some(item => item.kind === 'user_message' && item.content.includes('REPLACE_NEW')));
  assert.ok(replacementTurns[2].items.some(item => item.kind === 'user_message' && item.content.includes('REPLACE_FIFO')));
  assert.equal((await queue()).length, 0);

  const readGoal = async () => {
    const response = await fetch(`${origin}/api/goals/current?scope=personal&sessionId=${encodeURIComponent(sessionId)}`);
    assert.equal(response.status, 200);
    return response.json();
  };
  const goalAcceptance = await fetch(`${origin}/api/session/turn`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ scope: "personal", sessionId, text: goalObjective, goalMode: true,
      requestId: "goal-fixture-input", userMessageId: "goal-fixture-user" }),
  });
  assert.equal(goalAcceptance.status, 200, await goalAcceptance.text());
  await waitFor(async () => { const snapshot = await readGoal(); return snapshot.goal?.status === "complete" && snapshot; }, "无任务预算的目标执行并完成", 30000);
  await waitFor(() => page.evaluate(`document.body.innerText.includes('GOAL_USAGE_DONE') && !document.querySelector('[data-testid="input-stop-button"]')`), "目标结果展示");
  assert.ok(goalRequests >= 8, "缺少 usage 的请求仍可继续");
  const settled = await readGoal();
  assert.ok(settled.goal.tokensUsed >= 1_000_000, "大量实际用量仅做统计");
  assert.ok(settled.goal.modelAttemptsUsed >= 8);
  for (const key of ["tokenBudget", "tokensReserved", "budgetRequiredTokens", "runBudget", "budgetActiveMillis"]) {
    assert.equal(key in settled.goal, false, `${key} 不应出现在目标 DTO`);
  }
  for (const key of ["requiresBudgetIncrease", "minimumTokenBudget", "runBudgetExhausted"]) {
    assert.equal(key in settled.allowedActions, false, `${key} 不应出现在操作 DTO`);
  }
  await page.evaluate(`document.querySelector('.goal-drawer-toggle').click()`);
  await waitFor(() => page.evaluate(`!!document.querySelector('.goal-metrics')`), "目标用量展示");
  assert.equal(await page.evaluate(`!!document.querySelector('.goal-budget-resume-field')`), false);

  // 转交是已有 Task 的拥有方变更；点击、主线停止和后续输入都不能重新派发。
  const workspacePath = join(fixtureRoot, 'agent-workspace');
  await mkdir(workspacePath);
  for (const args of [['init','-b','main'],['-c','user.name=Magi Test','-c','user.email=test@magi.invalid','commit','--allow-empty','-m','fixture']]) {
    execFileSync('git', ['-C', workspacePath, ...args], {stdio:'pipe'});
  }
  const post = async (path, body) => {
    const response = await fetch(`${origin}/api/${path}`, {method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(body)});
    const text = await response.text();
    assert.equal(response.status, 200, `${path}: ${text}`);
    return JSON.parse(text);
  };
  const { workspaceId } = await post('workspaces/register', {path:workspacePath});
  const accepted = await post('session/turn', {scope:'workspace',workspaceId,workspacePath,
    text:'WEB_SESSION_AGENT 明确要求真实代理独立执行，并等待用户通过界面转交。',
    requestId:'web-agent-transfer',userMessageId:'web-agent-transfer-user'});
  await waitFor(() => agentChildHeld, '真实子代理开始执行');
  const query = new URLSearchParams({scope:'workspace',workspaceId,workspacePath,sessionId:accepted.sessionId});
  const readProjection = async taskId => {
    const response = await fetch(`${origin}/api/agent-runs/projection/${taskId}?${query}`);
    assert.equal(response.status, 200);
    return response.json();
  };
  const original = await readProjection(accepted.rootTaskId);
  assert.equal(original.agents.length, 1);
  const agent = original.agents[0];
  assert.equal(agent.transferRequest.owner, 'session');
  // 只按路径提交的消息在服务端投影中已有解析后的 workspaceId。
  // 界面必须能用同一项目的 ID 保存编辑，而不是把它误判为跨工作区。
  const pathQueued = await post('session/turn', {scope:'workspace',workspacePath,
    sessionId:accepted.sessionId,text:'PATH_BOUND_QUEUE 原排队消息',
    requestId:'web-path-bound-input',userMessageId:'web-path-bound-user'});
  assert.equal(pathQueued.queued, true);
  await page.call('Page.navigate', {url:`${origin}/web.html?${query}`});
  await waitFor(() => page.evaluate(`(() => {
    const button = document.querySelector('.ia-queue-item button[aria-label="编辑消息"]');
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`), '项目路径绑定消息进入编辑');
  await send('PATH_BOUND_QUEUE 已保存编辑', '[data-testid="input-queue-save-button"]');
  await waitFor(() => page.evaluate(`!document.querySelector('[data-testid="input-editing-bar"]') && document.querySelector('.ia-queue-item')?.innerText.includes('PATH_BOUND_QUEUE 已保存编辑')`), '路径/ID 归一化后保存到原队列');
  const pathQueueQuery = new URLSearchParams({ sessionId: accepted.sessionId, workspaceId, workspacePath });
  const pathSnapshot = await fetch(`${origin}/api/session/queue?${pathQueueQuery}`).then(response => response.json());
  assert.equal(pathSnapshot.queuedTurns.length, 1);
  assert.equal(pathSnapshot.queuedTurns[0].queueId, pathQueued.queueId);
  assert.equal(pathSnapshot.queuedTurns[0].requestId, 'web-path-bound-input');
  await page.evaluate(`document.querySelector('.ia-queue-item button[aria-label="删除消息"]').click()`);
  await waitFor(async () => (await queue()).length === 0, '撤销路径绑定验收消息');
  assert.equal(removeSubmissions.at(-1)?.expectedRevision, pathSnapshot.queuedTurns[0].revision,
    '真实 GUI 删除必须提交用户看到的 Input 版本');
  await page.evaluate(`(() => {
    const input = document.querySelector('[data-testid="input-textarea"]');
    input.textContent = '核对主线控制目标';
    input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText' }));
  })()`);
  await waitFor(() => page.evaluate(`!!document.querySelector('[data-testid="input-replace-button"]')`), '主线与子代理并行时提供停止并改做');
  assert.equal(await page.evaluate(`document.querySelector('[data-testid="input-replace-button"]').disabled`), false,
    '子代理独立 Turn 不能让主线控制目标变得不明确');
  await page.evaluate(`(() => {
    const input = document.querySelector('[data-testid="input-textarea"]');
    input.textContent = '';
    input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward' }));
  })()`);
  await waitFor(() => page.evaluate(`(() => {
    const card = document.querySelector('[data-agent-task-id="${agent.agentRunId}"]');
    if (!card || card.disabled) return false;
    card.click(); return true;
  })()`), '打开真实代理详情');
  await waitFor(() => page.evaluate(`(() => {
    const button = document.querySelector('[data-testid="agent-transfer-button"]');
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`), '显式转交按钮');
  await waitFor(() => page.evaluate(`document.querySelector('.agent-transfer strong')?.innerText === '由会话管理'`), '服务端确认拥有方后展示成功');
  assert.equal((await readProjection(accepted.rootTaskId)).agents[0].executionOwner, 'session');
  await waitFor(() => page.evaluate(`(() => {
    const button = document.querySelector('[data-testid="input-stop-button"]');
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`), '停止原主线');
  await waitFor(() => page.evaluate(`!document.querySelector('[data-testid="input-stop-button"]')`), '原主线停止完成');
  assert.equal((await readProjection(agent.agentRunId)).agents[0].status, 'running', '转交代理不随父轮停止');
  await send('WEB_AGENT_NEXT 主线自行完成新的普通输入。');
  await waitFor(() => page.evaluate(`document.body.innerText.includes('WEB_AGENT_NEXT_DONE') && !document.querySelector('[data-testid="input-stop-button"]')`), '后续主线独立完成');
  await waitFor(() => page.evaluate(`document.querySelector('.agent-transfer strong')?.innerText === '由会话管理'`), '跨主线读取原代理归属');
  const requestsBeforeRestart = requests.length;
  const stopped = once(daemon, 'exit');
  process.kill(-daemon.pid, 'SIGKILL');
  await stopped;
  await waitFor(async () => {
    try { await fetch(`${origin}/health`); return false; } catch { return true; }
  }, '原 daemon 端口释放');
  releaseAgentChild();
  await startDaemon();
  const interrupted = await readProjection(agent.agentRunId);
  assert.equal(interrupted.agents[0].status, 'failed');
  assert.ok(interrupted.agents[0].continueRequest, 'daemon 提供原代理的继续命令');
  assert.equal(requests.length, requestsBeforeRestart, '重启和读取不能自动重新执行');
  await page.call('Page.reload');
  await waitFor(() => page.evaluate(`(() => {
    const button = document.querySelector('[data-testid="agent-continue-button"]');
    if (!button || button.disabled) return false;
    button.click(); return true;
  })()`), 'Web 显式继续原代理');
  await waitFor(async () => (await readProjection(agent.agentRunId)).agents[0].status === 'completed', '原代理独立完成');
  await waitFor(() => page.evaluate(`document.querySelector('.agent-tab-content')?.innerText.includes('WEB_SESSION_AGENT_CHILD_DONE')`), '详情继续展示原代理结果');
  const history = await waitFor(async () => {
    const response = await fetch(`${origin}/api/messages?${query}`);
    assert.equal(response.status, 200);
    const snapshot = await response.json();
    const turns = snapshot.canonicalTurns.filter(turn => turn.metadata?.agentExecution);
    return turns.length === 2 && turns[1].status === 'completed' ? snapshot : false;
  }, '原代理 Turn 终态收口');
  const agentTurns = history.canonicalTurns.filter(turn => turn.metadata?.agentExecution);
  assert.equal(agentTurns.length, 2, '显式恢复创建新 Turn，保留同一 activation');
  assert.equal(agentTurns[0].status, 'interrupted');
  assert.equal(agentTurns[1].status, 'completed');
  assert.equal(agentTurns[1].metadata.agentExecution.previousTurnId, agentTurns[0].turnId);
  assert.equal(agentTurns[1].metadata.agentExecution.taskId, agentTurns[0].metadata.agentExecution.taskId);
  assert.equal(requests.length, requestsBeforeRestart + 1, '恢复只请求一次模型，不重复派发代理');
  assert.equal(history.canonicalTurns.find(turn => turn.turnId === accepted.turnId)?.status, 'cancelled');
  assert.ok(history.canonicalTurns.some(turn => turn.status === 'completed' && turn.items.some(item => item.content?.includes('WEB_AGENT_NEXT_DONE'))));
  assert.deepEqual(errors, []);
  process.stdout.write(JSON.stringify({ passed: true, health, providerRequests: requests.length,
    checks: ["编辑前原输入留在队列", "保存保持输入身份和 FIFO", "领取后保存返回 409 并保留草稿", "路径绑定消息可用解析后 ID 保存编辑", "task 引导可点击", "重载保留待处理输入", "补充在原 Turn 下一步消费", "下一轮 FIFO 消费", "发送后队列无残留", "用户消息无重复", "未知用量不阻止目标执行", "大量用量只做统计", "目标完成且无预算 DTO 或配置", "Web 停止并改做优先于普通队列", "原执行先取消并结算", "Web 显式转交原代理", "父轮停止及换轮不停止已转交代理", "SIGKILL 后 Web 显式继续原代理", "原中断记录和新主线保持终态", "详情跨主线读取原 Task 结果", "继续只创建新 Turn，不重复派发代理"] }, null, 2) + "\n");
} catch (error) {
  process.stderr.write(JSON.stringify(await page?.evaluate("document.body.innerText.slice(-4000)").catch(() => null)) + "\n");
  process.stderr.write(`${logs.slice(-12000)}\n`);
  throw error;
} finally {
  releaseGuidance();
  releaseReplacement();
  releaseRoot();
  releaseAgentChild();
  page?.close();
  for (const child of [browser, daemon]) {
    if (!child || child.exitCode !== null) continue;
    if (child === daemon) { try { process.kill(-child.pid, "SIGTERM"); } catch {} }
    else child.kill("SIGTERM");
    await Promise.race([new Promise((resolve) => child.once("exit", resolve)), new Promise((resolve) => setTimeout(resolve, 5000))]);
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
  }
  provider.closeAllConnections();
  await new Promise((resolve) => provider.close(resolve));
  await rm(fixtureRoot, { recursive: true, force: true });
}
