import assert from "node:assert/strict";
import { createServer } from "node:http";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";
import electron from "electron";
import { connectAppPage, waitFor } from "./lib/electron-harness.mjs";

const root = new URL("..", import.meta.url).pathname.replace(/\/$/, "");
const executable = process.env.MAGI_ELECTRON_STATS_APP_EXECUTABLE?.trim()
  || join(root, "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi");
const daemonPort = Number(process.env.MAGI_ELECTRON_STATS_DAEMON_PORT || 39321);
const cdpPort = Number(process.env.MAGI_ELECTRON_STATS_CDP_PORT || 9391);
const stateRoot = await mkdtemp(join(tmpdir(), "magi-electron-session-stats-"));
const evidencePath = process.env.MAGI_ELECTRON_STATS_EVIDENCE_PATH?.trim()
  || join(stateRoot, "evidence.json");

const requests = [];
const provider = createServer(async (request, response) => {
  if (request.method === "GET" && request.url === "/v1/models") {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ object: "list", data: [{ id: "electron-stats-model" }] }));
    return;
  }
  if (request.method !== "POST" || request.url !== "/v1/chat/completions") {
    response.writeHead(404).end();
    return;
  }
  let raw = "";
  for await (const chunk of request) raw += chunk;
  const body = JSON.parse(raw);
  requests.push(body);
  const messages = Array.isArray(body.messages) ? body.messages : [];
  const promptText = messages.map((message) => typeof message?.content === "string"
    ? message.content : JSON.stringify(message?.content || "")).join("\n");
  const toolResults = messages.filter((message) => message?.role === "tool");
  const hasTool = (name) => Array.isArray(body.tools) && body.tools.some((tool) => {
    const candidate = tool?.function?.name || "";
    return candidate === name || candidate.endsWith(`_${name}`) || candidate.endsWith(`.${name}`);
  });
  const toolName = (name) => body.tools?.find((tool) => {
    const candidate = tool?.function?.name || "";
    return candidate === name || candidate.endsWith(`_${name}`) || candidate.endsWith(`.${name}`);
  })?.function?.name || name;

  let output;
  if (promptText.includes("WORKER_STATS_TEST") && hasTool("agent_spawn") && toolResults.length === 0) {
    output = { type: "tool", name: toolName("agent_spawn"), id: "stats-agent-spawn", args: {
      task_name: "stats_worker", role: "executor", display_name: "统计验收 worker",
      goal: "完成统计验收并回复 WORKER_CHILD_DONE",
      delegation: { purpose: "user_requested", reason: "真实桌面统计验收" },
      context_package: { summary: "统计验收", constraints: ["只回复结果"], expected_output: "WORKER_CHILD_DONE", references: [] },
    } };
  } else if (promptText.includes("WORKER_STATS_TEST") && hasTool("agent_wait") && toolResults.length === 1) {
    output = { type: "tool", name: toolName("agent_wait"), id: "stats-agent-wait", args: {
      task_ids: [JSON.parse(toolResults[0].content || "{}").child_task_id].filter(Boolean), timeout_ms: 60_000,
    } };
  } else if (promptText.includes("WORKER_STATS_TEST")) {
    output = { type: "text", content: "WORKER_STATS_DONE" };
  } else {
    output = { type: "text", content: "MAIN_STATS_DONE" };
  }

  const promptTokens = Math.max(128, messages.length * 64);
  const completionTokens = output.type === "text" ? Math.max(4, Math.ceil(output.content.length / 4)) : 12;
  const zeroUsage = promptText.includes("ZERO_USAGE_TEST");
  const missingUsage = promptText.includes("MISSING_USAGE_TEST") && !zeroUsage;
  const first = output.type === "tool"
    ? { tool_calls: [{ index: 0, id: output.id, type: "function", function: { name: output.name, arguments: JSON.stringify(output.args) } }] }
    : { content: output.content };
  const payload = [
    `data: ${JSON.stringify({ choices: [{ delta: first, finish_reason: null }] })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: output.type === "tool" ? "tool_calls" : "stop" }] })}\n\n`,
    missingUsage ? "" : `data: ${JSON.stringify({ choices: [], usage: zeroUsage ? { prompt_tokens:0, completion_tokens:0 } : { prompt_tokens: promptTokens, prompt_tokens_details: { cached_tokens: requests.length > 1 ? Math.floor(promptTokens / 2) : 0 }, completion_tokens: completionTokens, completion_tokens_details: { reasoning_tokens: 0 } } })}\n\n`,
    "data: [DONE]\n\n",
  ].join("");
  response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache", connection: "keep-alive" });
  response.end(payload);
});
await new Promise((resolve) => provider.listen(0, "127.0.0.1", resolve));
const providerPort = provider.address().port;

const electronEnv = {
  ...process.env,
  MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
  MAGI_STATE_ROOT: stateRoot,
  MAGI_OPEN_BROWSER: "0",
  MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
  MAGI_OPENAI_COMPAT_API_KEY: "electron-stats-test-key",
  MAGI_OPENAI_COMPAT_MODEL: "electron-stats-model",
};
function launch(port) {
  return spawn(executable, [
    `--remote-debugging-port=${port}`,
    "--disable-gpu",
    "--disable-backgrounding-occluded-windows",
    "--disable-renderer-backgrounding",
    "--disable-background-timer-throttling",
  ], { cwd: root, env: electronEnv, stdio: ["ignore", "pipe", "pipe"] });
}
let child = launch(cdpPort);
const logs = [];
for (const stream of [child.stdout, child.stderr]) stream.on("data", (chunk) => logs.push(String(chunk)));
let page;

async function stop() {
  if (child.exitCode !== null) return;
  child.kill("SIGTERM");
  await Promise.race([new Promise((resolve) => child.once("exit", resolve)), delay(15_000)]);
  if (child.exitCode === null) child.kill("SIGKILL");
}

async function stopProcess(process) {
  if (process.exitCode !== null) return;
  process.kill("SIGTERM");
  await Promise.race([new Promise((resolve) => process.once("exit", resolve)), delay(15_000)]);
  if (process.exitCode === null) process.kill("SIGKILL");
}

async function state() {
  return page.evaluate(`JSON.parse(JSON.stringify({
    url: location.href,
    body: document.body?.innerText || '',
    input: Boolean(document.querySelector('[data-testid="input-textarea"]')),
    model: [...document.querySelectorAll('.ia-model-btn')].map((item) => item.innerText || '').join(' '),
  }))`);
}

async function setText(value) {
  await page.evaluate(`(() => { const input = document.querySelector('[data-testid="input-textarea"]'); if (!input) throw new Error('composer missing'); input.textContent = ${JSON.stringify(value)}; input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: ${JSON.stringify(value)} })); })()`);
  await waitFor(() => page.evaluate(`Boolean(document.querySelector('[data-testid="input-send-button"]:not([disabled])'))`), "发送按钮");
  await page.evaluate(`document.querySelector('[data-testid="input-send-button"]')?.click()`);
}

async function statsFor(sessionId) {
  return page.evaluate(`fetch('/api/settings/stats?sessionId=' + encodeURIComponent(${JSON.stringify(sessionId)})).then(async (response) => ({ status: response.status, body: await response.json() }))`);
}

async function openStatsAndSelect(scope, expected) {
  await waitFor(() => page.evaluate(`Boolean(document.querySelector('[data-testid="session-stats-trigger"]'))`), "统计入口");
  await page.call("Page.bringToFront").catch(() => {});
  if (!await page.evaluate(`document.querySelector('[data-testid="session-stats-trigger"]')?.getAttribute('aria-expanded') === 'true'`)) {
    const rect = await page.evaluate(`(() => { const r = document.querySelector('[data-testid="session-stats-trigger"]')?.getBoundingClientRect(); return r ? { x: r.left + r.width / 2, y: r.top + r.height / 2 } : null; })()`);
    assert.ok(rect, "统计入口必须可见");
    await page.call("Input.dispatchMouseEvent", { type: "mouseMoved", x: rect.x, y: rect.y });
    await page.call("Input.dispatchMouseEvent", { type: "mousePressed", x: rect.x, y: rect.y, button: "left", clickCount: 1 });
    await page.call("Input.dispatchMouseEvent", { type: "mouseReleased", x: rect.x, y: rect.y, button: "left", clickCount: 1 });
  }
  await waitFor(() => page.evaluate(`Boolean(document.querySelector('.session-stats-popover'))`), "统计面板").catch(async (error) => {
    const debug = await page.evaluate(`JSON.stringify({ button: document.querySelector('[data-testid="session-stats-trigger"]')?.outerHTML, expanded: document.querySelector('[data-testid="session-stats-trigger"]')?.getAttribute('aria-expanded'), body: document.body.innerText.slice(-500) })`);
    throw new Error(`${error.message}; ${debug}`);
  });
  if (scope) await page.evaluate(`(() => { const select = document.querySelector('.session-stats-popover select'); if (!select) throw new Error('stats scope missing'); select.value = ${JSON.stringify(scope)}; select.dispatchEvent(new Event('change', { bubbles: true })); })()`);
  await waitFor(() => page.evaluate(`(() => {
    const text = document.querySelector('.session-stats-popover')?.innerText || '';
    return text.includes(${JSON.stringify(String(expected?.netOutputTokens ?? ''))})
      && text.includes(${JSON.stringify(`${expected?.tokenObservedCallCount ?? 0} / ${expected?.llmCallCount ?? 0}`)});
  })()`), "统计面板同步最新账本");
  return page.evaluate(`document.querySelector('.session-stats-popover')?.innerText || ''`);
}

try {
  await waitFor(async () => {
    if (child.exitCode !== null) throw new Error(`Electron exited ${child.exitCode}: ${logs.join("").slice(-2000)}`);
    try {
      page = await connectAppPage(cdpPort, daemonPort, []);
      const current = await state();
      return current.input && current.model.includes("electron-stats-model") ? current : null;
    } catch { return null; }
  }, "真实 Electron App Renderer", 90_000);

  await setText("MAIN_STATS_TEST");
  await waitFor(() => page.evaluate(`document.body.innerText.includes('MAIN_STATS_DONE')`), "主对话完成");
  await setText("WORKER_STATS_TEST：请派发一个 worker 并等待完成");
  await waitFor(() => page.evaluate(`document.body.innerText.includes('WORKER_STATS_DONE')`), "worker 对话完成", 90_000);
  const sessionId = new URL((await state()).url).searchParams.get("sessionId");
  assert.ok(sessionId, "真实桌面会话必须有 sessionId");

  const response = await statsFor(sessionId);
  assert.equal(response.status, 200, JSON.stringify(response.body));
  const items = response.body.items || [];
  const orchestrator = items.filter((item) => item.role === "orchestrator");
  const worker = items.filter((item) => item.role === "worker");
  assert.ok(orchestrator.length > 0 && orchestrator.some((item) => item.llmCallCount > 0), "主对话统计缺失");
  assert.ok(worker.length > 0 && worker.some((item) => item.llmCallCount > 0), "worker 统计缺失");
  assert.equal(response.body.totals.llmCallCount, items.reduce((sum, item) => sum + item.llmCallCount, 0));
  assert.equal(response.body.totals.tokenObservedCallCount, response.body.totals.llmCallCount);
  assert.ok(response.body.totals.performance.ttftSampleCount > 0, "TTFT 统计缺失");
  assert.ok(response.body.totals.cache.observedCallCount > 0, "缓存观测缺失");

  const allPanel = await openStatsAndSelect("all", response.body.totals);
  assert.match(allPanel, /TTFT|首 token/i);
  assert.match(allPanel, /TPS|输出速度/i);
  assert.match(allPanel, /缓存|cache/i);
  const workerTotals = worker.reduce((total, item) => ({
    ...total,
    llmCallCount: total.llmCallCount + item.llmCallCount,
    tokenObservedCallCount: total.tokenObservedCallCount + (item.tokenObservedCallCount || 0),
    netOutputTokens: total.netOutputTokens + item.netOutputTokens,
  }), { llmCallCount: 0, tokenObservedCallCount: 0, netOutputTokens: 0 });
  const workerPanel = await openStatsAndSelect("worker", workerTotals);
  assert.match(workerPanel, /worker|子代理/i);
  await page.evaluate(`document.querySelector('[data-testid="session-stats-trigger"]')?.click()`);

  const beforeRestart = response.body;
  page.close();
  await stopProcess(child);
  child = launch(cdpPort + 1);
  page = await waitFor(async () => {
    if (child.exitCode !== null) throw new Error(`Electron restart exited ${child.exitCode}`);
    try {
      const next = await connectAppPage(cdpPort + 1, daemonPort, []);
      await next.call("Page.navigate", { url: `http://127.0.0.1:${daemonPort}/web.html?scope=personal&sessionId=${encodeURIComponent(sessionId)}` });
      return next;
    } catch { return null; }
  }, "真实 Electron 重启恢复会话", 90_000);
  const afterRestart = await statsFor(sessionId);
  assert.equal(afterRestart.status, 200);
  assert.deepEqual(afterRestart.body.totals, beforeRestart.totals, "桌面重启后统计必须保持一致");

  // 真实 daemon 产生缺失与明确零用量，核对设置页两种视角及其自动刷新。
  const api = (path, body) => page.evaluate(`fetch(${JSON.stringify(path)}, ${JSON.stringify(body === undefined ? {} : {
    method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify(body),
  })}).then(async response => { if(!response.ok) throw new Error(await response.text()); return response.json(); })`);
  await api('/api/settings/stats/reset', {});
  const presenceSession = (await api('/api/session/materialize', {scope:'personal'})).sessionId;
  const runPresenceTurn = async marker => {
    const {turnId} = await api('/api/session/turn', {scope:'personal',sessionId:presenceSession,text:marker,requestId:marker,userMessageId:marker+'-user'});
    const result = await waitFor(async () => (await api(`/api/messages?scope=personal&sessionId=${presenceSession}`)).canonicalTurns
      .find(turn=>turn.turnId===turnId && ['completed','failed','cancelled','blocked'].includes(turn.status)), marker);
    assert.equal(result.status,'completed');
    return (await statsFor(presenceSession)).body;
  };
  const missingStats = await runPresenceTurn('MISSING_USAGE_TEST');
  await writeFile(evidencePath+'.missing.json',JSON.stringify({missingStats,turns:await api(`/api/messages?scope=personal&sessionId=${presenceSession}`),requests},null,2));
  assert.equal(missingStats.totals.llmCallCount,1);
  assert.equal(missingStats.totals.tokenObservedCallCount,0);
  await waitFor(()=>page.evaluate(`Boolean(document.querySelector('[data-testid="sidebar-settings"]'))`),'设置入口');
  await page.evaluate(`document.querySelector('[data-testid="sidebar-settings"]').click()`);
  await waitFor(()=>page.evaluate(`Boolean(document.querySelector('[data-testid="settings-nav-stats"]'))`),'统计分类');
  await page.evaluate(`document.querySelector('[data-testid="settings-nav-stats"]').click()`);
  const settingsText = () => page.evaluate(`({
    total:document.querySelector('.insight-cell:nth-child(2) .insight-headline')?.innerText,
    selected:document.querySelector('.slice-kpi .kpi-value')?.innerText,
    note:document.querySelector('.token-usage-note')?.innerText,
    body:document.querySelector('.stats-tab-inner')?.innerText,
  })`);
  const missingUi = await waitFor(async()=>{
    const value=await settingsText();
    return /未返回|Not reported/.test(value.total) && /未返回|Not reported/.test(value.selected) && value.note.includes('0/1') && value;
  },'缺少用量显示未知，保留真实调用数');
  await page.evaluate(`document.querySelectorAll('.seg-toggle button')[1].click()`);
  await waitFor(async()=>/未返回|Not reported/.test((await settingsText()).selected),'角色视角保持未知');
  const zeroStats = await runPresenceTurn('ZERO_USAGE_TEST');
  assert.equal(zeroStats.totals.llmCallCount,2);
  assert.equal(zeroStats.totals.tokenObservedCallCount,1);
  const zeroUi = await waitFor(async()=>{
    const value=await settingsText();
    return value.total==='0' && value.selected==='0' && value.note.includes('1/2') && value;
  },'统计页面自动显示明确零值及部分覆盖');
  await page.evaluate(`document.querySelectorAll('.seg-toggle button')[0].click()`);
  await waitFor(async()=>(await settingsText()).selected==='0','模型视角保留真实零值');
  await page.evaluate('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))');
  await writeFile(evidencePath+'.png',Buffer.from((await page.call('Page.captureScreenshot',{format:'png'})).data,'base64'));

  const evidence = {
    type: "electron_session_stats_live",
    status: "passed",
    sessionId,
    providerRequests: requests.length,
    totals: beforeRestart.totals,
    roles: { orchestrator: orchestrator.map((item) => item.llmCallCount), worker: worker.map((item) => item.llmCallCount) },
    ui: { allPanel, workerPanel },
    restartVerified: true,
    usagePresence: {sessionId:presenceSession,missingStats,zeroStats,missingUi,zeroUi},
    note: "真实 Electron/Renderer/daemon/worker 链路；Provider 为本地受控 HTTP，仅证明统计归属和核算正确性。",
  };
  await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  console.log(JSON.stringify(evidence, null, 2));
} finally {
  page?.close();
  await stop();
  if (provider.closeAllConnections) provider.closeAllConnections();
  await new Promise((resolve) => provider.close(resolve));
}
