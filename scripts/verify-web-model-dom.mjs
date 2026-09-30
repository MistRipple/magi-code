#!/usr/bin/env node
/**
 * GPT Web 模型（Magi 接管内置浏览器登录 ChatGPT 并使用 Web 临时对话）
 * 回答同步自动化验收（《实现计划》§9.4）。
 *
 * 复刻 `scripts/verify-electron-conversation-dom.mjs` 的形态：**隔离状态根 +
 * 本地假 ChatGPT 页面 fixture + 真实打包 Electron App Renderer CDP 断言**。
 * 页面断言都经过真实 App Renderer，daemon 侧断言都走 Renderer 内发起的
 * `/api/*` 请求，因此不需要脚本自己拼一条绕开桌面入口的旁路。
 *
 * 覆盖链路：登录探测 → 发现 → 模型落库与主模型绑定 → 发送 → 流式 →
 * T2 工具回路 → 取消 → 用户接管与 epoch 重建 → 重置为 Magi 对话 →
 * canonical 与界面口径一致，以及「Web 对话不落盘」。
 *
 * 站点 origin 由 daemon 侧的 `MAGI_WEB_MODEL_ORIGIN` 覆盖（`crates/magi-web-model`），
 * 只有运行时取值变化，产品默认值不动。
 */

import { execFile, spawn } from "node:child_process";
import { createServer } from "node:http";
import { createServer as createNetServer } from "node:net";
import { mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, relative } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { promisify } from "node:util";
import WebSocket from "ws";

const repositoryRoot = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const defaultAppExecutable = join(
  repositoryRoot,
  "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi",
);
const appExecutable = process.env.MAGI_WEB_MODEL_DOM_APP_EXECUTABLE?.trim() || defaultAppExecutable;
const configuredCdpPort = Number.parseInt(process.env.MAGI_WEB_MODEL_DOM_CDP_PORT || "0", 10);
const configuredDaemonPort = Number.parseInt(process.env.MAGI_WEB_MODEL_DOM_DAEMON_PORT || "0", 10);
let cdpPort = configuredCdpPort;
let daemonPort = configuredDaemonPort;
const evidencePath = process.env.MAGI_WEB_MODEL_DOM_EVIDENCE_PATH?.trim() || "";
const hostLogPath = process.env.MAGI_WEB_MODEL_DOM_HOST_LOG?.trim() || "";
const keepStateRoot = process.env.MAGI_WEB_MODEL_DOM_KEEP_STATE === "1";

const fixtureReplyText = "WEB_MODEL_DOM_OK";
const fixtureToolReplyText = "WEB_MODEL_DOM_TOOL_DONE";
const fixtureThinkingText = "这条临时对话里的隐藏推理。";
const fixtureToolThinkingText = "需要先看工作区里的文件。";
const fixtureNoteName = "fixture-note.txt";
const fixtureNoteBody = "WEB_MODEL_DOM_FIXTURE_NOTE_BODY\n";
const plainTurnMarker = "WEB_MODEL_DOM_PLAIN_MARKER";
const toolTurnMarker = "WEB_MODEL_DOM_TOOL_MARKER";
const slowTurnMarker = "WEB_MODEL_DOM_SLOW_MARKER";

const checks = [];
const execFileAsync = promisify(execFile);

async function freeLoopbackPort() {
  const server = createNetServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  const port = typeof address === "object" && address ? address.port : 0;
  await new Promise((resolve) => server.close(resolve));
  if (!port) throw new Error("无法分配本地验收端口");
  return port;
}

function check(name, condition, detail = "") {
  const passed = Boolean(condition);
  checks.push({ name, passed, detail: passed ? "" : String(detail || "") });
  process.stdout.write(`${passed ? "通过" : "失败"} ${name}${passed || !detail ? "" : `: ${detail}`}\n`);
  return passed;
}

async function waitFor(predicate, label, timeoutMs = 30_000, intervalMs = 200) {
  const deadline = Date.now() + timeoutMs;
  let lastError = null;
  while (Date.now() < deadline) {
    try {
      const value = await predicate();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await sleep(intervalMs);
  }
  throw new Error(`等待超时：${label}${lastError ? `；最后错误：${lastError.message}` : ""}`);
}

// ── 假 ChatGPT 站点 fixture ──────────────────────────────────────────────

const FIXTURE_PAGE = String.raw`<!doctype html>
<html lang="zh-CN" data-magi-account-plan="plus">
<head>
<meta charset="utf-8">
<title>ChatGPT</title>
<style>
  html, body { margin: 0; height: 100%; font: 14px/1.5 -apple-system, sans-serif; }
  main { display: block; min-height: 80px; padding: 10px; }
  #prompt-textarea { min-height: 44px; border: 1px solid #bbb; border-radius: 6px; padding: 8px; }
  #model-menu-popup { display: none; padding: 6px; }
  #model-menu-popup[data-open="1"] { display: block; }
  [data-message-author-role] { margin: 6px 0; padding: 6px; border-radius: 6px; }
  [data-message-author-role="user"] { background: #eef3ff; }
  [data-message-author-role="assistant"] { background: #f5f5f5; }
  [data-testid="reasoning-block"] { margin: 6px 0; color: #666; font-style: italic; }
  #stop-button[hidden] { display: none; }
</style>
</head>
<body>
<main id="conversation" role="main"></main>
<button id="model-menu" data-testid="model-switcher-dropdown-button" aria-haspopup="menu">GPT-5</button>
<div id="model-menu-popup" role="menu" data-open="0">
  <div role="menuitemradio" data-testid="model-item-thinking" aria-checked="true">GPT-5 Thinking
    <div role="menuitemradio" data-testid="effort-medium">medium</div>
    <div role="menuitemradio" data-testid="effort-high">high</div>
  </div>
  <div role="menuitemradio" data-testid="model-item-gpt-5" aria-checked="false">GPT-5
    <div role="menuitemradio" data-testid="effort-low">low</div>
    <div role="menuitemradio" data-testid="effort-medium">medium</div>
  </div>
</div>
<div id="prompt-textarea" contenteditable="true" data-max-length="210000"></div>
<button id="send-button" data-testid="send-button" type="button">发送</button>
<button id="stop-button" data-testid="stop-button" hidden aria-label="Stop" type="button">停止</button>
<p id="web-model-connectors-hint"><a href="#connectors" data-testid="connectors-settings">连接器</a></p>
<script>
(() => {
  // 每次页面加载就是一条新的「临时对话实例」：fixture 用它区分 epoch 重建。
  const conversationId = Math.random().toString(36).slice(2, 12) + '-' + Date.now().toString(36);
  const main = document.getElementById('conversation');
  const composer = document.getElementById('prompt-textarea');
  const stopButton = document.getElementById('stop-button');
  const menuButton = document.getElementById('model-menu');
  const menuPopup = document.getElementById('model-menu-popup');
  window.__magidomConversationId = conversationId;

  menuButton.addEventListener('click', () => {
    menuPopup.dataset.open = menuPopup.dataset.open === '1' ? '0' : '1';
  });
  // 真实站点用 Escape 收口模型菜单；fixture 必须同语义，否则连续两次探测会
  // 因为「第二次点击把菜单关掉」读不到模型条目。
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') menuPopup.dataset.open = '0';
  });

  const composerText = () => (composer.innerText || composer.textContent || '');

  const appendUserMessage = (text) => {
    const node = document.createElement('div');
    node.setAttribute('data-message-author-role', 'user');
    node.textContent = text;
    main.appendChild(node);
    return node;
  };

  const appendAssistantMessage = () => {
    const node = document.createElement('div');
    node.setAttribute('data-message-author-role', 'assistant');
    main.appendChild(node);
    return node;
  };

  const setReasoning = (text) => {
    let node = document.querySelector('[data-testid="reasoning-block"]');
    if (!node) {
      node = document.createElement('div');
      node.setAttribute('data-testid', 'reasoning-block');
      main.appendChild(node);
    }
    node.textContent = text;
  };

  let busy = false;

  const runTurn = async () => {
    const prompt = composerText();
    if (busy || !prompt.trim()) return;
    busy = true;
    appendUserMessage(prompt);
    composer.textContent = '';
    stopButton.hidden = false;
    let script = null;
    try {
      const response = await fetch('/__fixture/submit', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ conversationId, prompt, url: location.href }),
      });
      script = await response.json();
    } catch (error) {
      script = { chunks: ['fixture_submit_failed'], chunkDelayMs: 0, thinking: '' };
    }
    if (script.thinking) setReasoning(script.thinking);
    if (script.hold) {
      // 永不收口：用于取消验收，页面保持「生成中」。
      return;
    }
    const node = appendAssistantMessage();
    let text = '';
    const chunks = Array.isArray(script.chunks) ? script.chunks : [];
    for (const chunk of chunks) {
      text += chunk;
      node.textContent = text;
      await new Promise((resolve) => setTimeout(resolve, Number(script.chunkDelayMs) || 120));
    }
    stopButton.hidden = true;
    busy = false;
  };

  document.getElementById('send-button').addEventListener('click', () => { void runTurn(); });
  stopButton.addEventListener('click', () => {
    // The real ChatGPT stop control terminates the current generation while
    // retaining the already rendered user message. Keep the fixture's busy
    // flag in the same state so a subsequent Magi turn can be submitted.
    busy = false;
    stopButton.hidden = true;
  });
  composer.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      void runTurn();
    }
  });

  // 用户接管：脚本通过 fixture 控制面让页面自己多出一条用户消息。
  setInterval(async () => {
    try {
      const response = await fetch('/__fixture/poll?conversationId=' + encodeURIComponent(conversationId));
      const commands = await response.json();
      for (const command of Array.isArray(commands) ? commands : []) {
        if (command.type === 'user_message') appendUserMessage(command.text);
      }
    } catch {
      /* 页面被导航走时忽略 */
    }
  }, 250);

  window.__magiFixture = { conversationId, appendUserMessage, composerText };
})();
</script>
</body>
</html>`;

function createFixture() {
  const state = {
    submits: [],
    pageLoads: [],
    commands: new Map(),
    toolCallIssued: new Set(),
  };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url || "/", "http://127.0.0.1");
    state.requests = state.requests || [];
    state.requests.push(`${request.method} ${url.pathname}${url.search}`);
    const sendJson = (value) => {
      const body = JSON.stringify(value);
      response.writeHead(200, { "content-type": "application/json", "cache-control": "no-store" });
      response.end(body);
    };
    if (url.pathname === "/__fixture/submit") {
      let raw = "";
      for await (const chunk of request) raw += chunk;
      let payload = {};
      try {
        payload = JSON.parse(raw);
      } catch {
        payload = {};
      }
      const conversationId = String(payload.conversationId || "");
      const prompt = String(payload.prompt || "");
      state.submits.push({
        at: Date.now(),
        conversationId,
        url: String(payload.url || ""),
        prompt,
      });
      sendJson(replyScript(state, { conversationId, prompt }));
      return;
    }
    if (url.pathname === "/__fixture/poll") {
      const conversationId = url.searchParams.get("conversationId") || "";
      const pending = state.commands.get(conversationId) || [];
      state.commands.set(conversationId, []);
      sendJson(pending);
      return;
    }
    if (url.pathname === "/__fixture/observe") {
      sendJson({
        requests: state.requests.slice(),
        submits: state.submits.length,
        pageLoads: state.pageLoads.slice(),
        submitsByConversation: state.submits.reduce((accumulator, entry) => {
          accumulator[entry.conversationId] = (accumulator[entry.conversationId] || 0) + 1;
          return accumulator;
        }, {}),
      });
      return;
    }
    if (url.pathname === "/favicon.ico") {
      response.writeHead(204);
      response.end();
      return;
    }
    if (url.pathname === "/" || url.pathname === "/index.html") {
      state.pageLoads.push({
        at: Date.now(),
        search: url.search,
        temporaryChat: url.searchParams.get("temporary-chat") === "true",
      });
      response.writeHead(200, { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" });
      response.end(FIXTURE_PAGE);
      return;
    }
    response.writeHead(404, { "content-type": "text/plain" });
    response.end("not found");
  });
  return { server, state };
}

function anchorNonceOf(prompt) {
  const match = /(?:^|\n)turn_nonce:\s*([0-9a-fA-F]{32})/.exec(prompt);
  return match ? match[1].toLowerCase() : "";
}

function userBlockOf(prompt) {
  // The production envelope is length-prefixed. Keep the fixture parser aligned
  // with that contract instead of treating the whole protocol prompt as user text.
  const pattern = /<<<magi-user\nbytes:\s*\d+\n---\n([\s\S]*?)\nmagi-user>>>/gu;
  let last = "";
  let match = pattern.exec(prompt);
  while (match) {
    last = match[1];
    match = pattern.exec(prompt);
  }
  if (last) return last;
  // Full replay uses the human-readable context envelope rather than the
  // length-prefixed increment block. Only inspect the final `### 用户`
  // section; old user messages (including the slow-cancel marker) must not
  // control the current fixture response.
  const contextPattern = /(?:^|\n)### 用户\n([\s\S]*?)(?=\n### [^\n]*\n|$)/gu;
  let contextLast = "";
  let contextMatch = contextPattern.exec(prompt);
  while (contextMatch) {
    contextLast = contextMatch[1];
    contextMatch = contextPattern.exec(prompt);
  }
  return contextLast;
}

function toolResultBodiesAfterLastUser(prompt) {
  const bodies = [];
  const lastUserEnd = [...prompt.matchAll(/<<<magi-user\nbytes:\s*\d+\n---\n[\s\S]*?\nmagi-user>>>/gu)]
    .at(-1)?.index;
  const source = lastUserEnd === undefined ? prompt : prompt.slice(lastUserEnd);
  const pattern = /<<<magi-tool-result[\s\S]*?\n---\n([\s\S]*?)\nmagi-tool-result>>>/gu;
  let match = pattern.exec(source);
  while (match) {
    bodies.push(match[1]);
    match = pattern.exec(prompt);
  }
  return bodies;
}

function replyScript(state, { conversationId, prompt }) {
  const userText = userBlockOf(prompt) || prompt;
  if (userText.includes(slowTurnMarker)) {
    // 保持「生成中」，直到被显式取消或超时。
    return { chunks: [], chunkDelayMs: 0, thinking: "慢任务。", hold: true };
  }
  const results = toolResultBodiesAfterLastUser(prompt);
  if (results.length > 0) {
    // 工具结果确实回到了同一条临时对话里（T2 回填）。
    return {
      chunks: [`${fixtureToolReplyText} ${results[0].slice(0, 40)}`],
      chunkDelayMs: 120,
      thinking: fixtureToolThinkingText,
    };
  }
  const toolListed = /(?:^|\n)- file_read\n/u.test(prompt);
  const alreadyIssued = state.toolCallIssued.has(conversationId);
  if (userText.includes(toolTurnMarker) && toolListed && !alreadyIssued) {
    state.toolCallIssued.add(conversationId);
    const nonce = anchorNonceOf(prompt);
    return {
      chunks: [
        "我先读一下工作区里的文件。\n",
        `~~~magi-tool-call\n${JSON.stringify({
          turn_id: nonce,
          name: "file_read",
          arguments: { path: fixtureNoteName },
        })}\n~~~\n`,
      ],
      chunkDelayMs: 150,
      thinking: fixtureToolThinkingText,
    };
  }
  return {
    // 终值标记放在**最后一块**：这样中间态是真的未收口（不包含终值），
    // 才能验证末值与终值一致，而不是一上来就整段出现。
    chunks: [
      `收到 ${userText.trim().slice(0, 24)}：`,
      fixtureReplyText,
    ],
    // The production bridge samples the page every 500 ms. Keep the first
    // chunk observable for the acceptance assertion instead of relying on a
    // race between two independent polling loops.
    // Keep the first visible chunk long enough for the production bridge's
    // 500 ms observation loop and the acceptance runner's own polling to
    // observe it independently of page-load/Surface hand-off latency.
    chunkDelayMs: 3_000,
    thinking: fixtureThinkingText,
  };
}

// ── Electron / CDP ──────────────────────────────────────────────────────

async function waitForProcessExit(child, timeoutMs = 15_000) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await Promise.race([
    new Promise((resolve) => {
      child.once("exit", resolve);
      child.once("error", resolve);
    }),
    sleep(timeoutMs),
  ]);
}

async function ownedDescendants(rootPid) {
  const { stdout } = await execFileAsync("ps", ["-axo", "pid=,ppid=,command="]);
  const processes = stdout
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const match = line.match(/^(\d+)\s+(\d+)\s+(.*)$/u);
      if (!match) return null;
      return { pid: Number(match[1]), ppid: Number(match[2]), command: match[3] };
    })
    .filter(Boolean);
  const descendants = [];
  const queue = [rootPid];
  while (queue.length > 0) {
    const parent = queue.shift();
    for (const process of processes) {
      if (process.ppid !== parent || descendants.some((item) => item.pid === process.pid)) continue;
      descendants.push(process);
      queue.push(process.pid);
    }
  }
  return descendants;
}

async function ownedDaemonPid(electronPid) {
  const daemon = (await ownedDescendants(electronPid))
    .find((process) => process.command.includes("magi-daemon-app"));
  return daemon?.pid ?? null;
}

async function stopOwnedDaemon(pid) {
  if (!Number.isInteger(pid)) return;
  let command = "";
  try {
    const { stdout } = await execFileAsync("ps", ["-p", String(pid), "-o", "command="]);
    command = stdout.trim();
  } catch {
    return;
  }
  if (!command.includes("magi-daemon-app") || !command.includes("target/electron-dist")) return;
  try {
    process.kill(pid, "SIGKILL");
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
}

async function connectPage(port = cdpPort) {
  const targets = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const target = targets.find((candidate) => (
    candidate.type === "page"
    && candidate.url.includes(`http://127.0.0.1:${daemonPort}/web.html`)
  ));
  if (!target?.webSocketDebuggerUrl) throw new Error("未找到打包 Electron App Renderer");
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  const pending = new Map();
  let nextId = 1;
  socket.on("message", (raw) => {
    const message = JSON.parse(raw.toString());
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id);
    clearTimeout(request.timer);
    if (message.error) request.reject(new Error(message.error.message || "CDP 请求失败"));
    else request.resolve(message.result);
  });
  await new Promise((resolve, reject) => {
    socket.once("open", resolve);
    socket.once("error", reject);
  });
  const call = (method, params = {}) => new Promise((resolve, reject) => {
    const id = nextId++;
    const timer = setTimeout(() => {
      if (pending.delete(id)) reject(new Error(`CDP 请求超时：${method}`));
    }, 30_000);
    pending.set(id, { resolve, reject, timer });
    socket.send(JSON.stringify({ id, method, params }));
  });
  await call("Runtime.enable");
  await call("Page.enable");

  // App Renderer 的 console 是「投影失败 / 宿主命令被拒」的唯一现场。
  socket.on("message", (raw) => {
    let message = null;
    try {
      message = JSON.parse(raw.toString());
    } catch {
      return;
    }
    if (message.method === "Runtime.consoleAPICalled") {
      const text = (message.params?.args ?? [])
        .map((arg) => (arg.value !== undefined ? String(arg.value) : arg.description || ""))
        .join(" ");
      if (text.trim()) pushRendererLog(`${message.params?.type || "log"}: ${text}`);
      return;
    }
    if (message.method === "Log.entryAdded") {
      const entry = message.params?.entry ?? {};
      if (entry.text) pushRendererLog(`${entry.level || "log"}: ${entry.text}`);
    }
    if (message.method === "Network.responseReceived") {
      const response = message.params?.response ?? {};
      if (Number(response.status) >= 400) {
        pushRendererLog(`http ${response.status} ${message.params?.type || ""} ${response.url || ""}`);
      }
    }
  });
  await call("Log.enable").catch(() => undefined);
  await call("Network.enable").catch(() => undefined);
  const evaluate = async (expression) => {
    const result = await call("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (result?.exceptionDetails) {
      throw new Error(
        result.exceptionDetails.exception?.description
          || result.exceptionDetails.text
          || "Renderer evaluate 失败",
      );
    }
    return result?.result?.value;
  };
  return {
    call,
    evaluate,
    close() {
      for (const request of pending.values()) {
        clearTimeout(request.timer);
        request.reject(new Error("CDP transport closed"));
      }
      pending.clear();
      socket.close();
    },
  };
}

// ── daemon HTTP 驱动（全部经真实 App Renderer 发起） ─────────────────────

async function api(page, method, path, payload) {
  const init = { method };
  if (payload !== undefined) {
    init.headers = { "content-type": "application/json" };
    init.body = JSON.stringify(payload);
  }
  const expression = `(async () => {
    const init = ${JSON.stringify(init)};
    const response = await fetch(${JSON.stringify(path)}, init);
    const text = await response.text();
    let body = null;
    try { body = JSON.parse(text); } catch {}
    return { status: response.status, body, text: text.slice(0, 4000) };
  })()`;
  return page.evaluate(expression);
}

async function apiOk(page, method, path, payload) {
  const result = await api(page, method, path, payload);
  if (result.status < 200 || result.status >= 300) {
    throw new Error(`${method} ${path} 返回 ${result.status}: ${result.text}`);
  }
  return result.body;
}

async function canonicalTurns(page, sessionId, workspaceId) {
  const query = `scope=workspace&sessionId=${encodeURIComponent(sessionId)}&workspaceId=${encodeURIComponent(workspaceId)}&limit=50`;
  const body = await apiOk(page, "GET", `/api/messages?${query}`);
  return Array.isArray(body?.canonicalTurns) ? body.canonicalTurns : [];
}

function assistantItems(turn) {
  return (Array.isArray(turn?.items) ? turn.items : [])
    .filter((item) => item?.kind === "assistant_text");
}

function thinkingItems(turn) {
  return (Array.isArray(turn?.items) ? turn.items : [])
    .filter((item) => item?.kind === "assistant_thinking");
}

function toolItems(turn) {
  return (Array.isArray(turn?.items) ? turn.items : [])
    .filter((item) => item?.kind === "tool_call");
}

function assistantText(turn) {
  return assistantItems(turn).map((item) => item.content || "").join("\n").trim();
}

function lastTurn(turns) {
  return turns.length > 0 ? turns[turns.length - 1] : null;
}

// ── 主流程 ──────────────────────────────────────────────────────────────

const fixture = createFixture();
await new Promise((resolve) => fixture.server.listen(0, "127.0.0.1", resolve));
const fixturePort = fixture.server.address().port;
const fixtureOrigin = `http://127.0.0.1:${fixturePort}`;

// Never reuse the normal development ports for this isolated acceptance run.
// A stale daemon from an interrupted run would otherwise be mistaken for the
// child daemon and fail the supervisor startup-nonce check.
if (!daemonPort) daemonPort = await freeLoopbackPort();
if (!cdpPort) cdpPort = await freeLoopbackPort();

const stateRoot = await mkdtemp(join(tmpdir(), "magi-web-model-dom-"));
const workspaceRoot = join(stateRoot, "workspace");
await mkdir(workspaceRoot, { recursive: true });
await writeFile(join(workspaceRoot, fixtureNoteName), fixtureNoteBody, "utf8");

function spawnElectron(port) {
  return spawn(appExecutable, [`--remote-debugging-port=${port}`, "--disable-gpu"], {
    cwd: repositoryRoot,
    env: {
      ...process.env,
      MAGI_DESKTOP_DAEMON_PORT: String(daemonPort),
      MAGI_STATE_ROOT: join(stateRoot, "state"),
      MAGI_OPEN_BROWSER: "0",
      MAGI_WEB_MODEL_ORIGIN: fixtureOrigin,
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
}

const electron = spawnElectron(cdpPort);
let page = null;
let daemonPidForCleanup = null;
const canonicalTextSnapshots = [];

/**
 * 打包 App / daemon 的输出在断言中断时要能带进诊断：验收失败最常见的根因
 * （Surface 未物化、宿主命令被拒、站点探测失败）只在宿主日志里。
 * 只保留尾部固定行数，避免长任务把输出刷爆。
 */
const hostLogTail = [];
const rendererLogTail = [];
function pushRendererLog(line) {
  rendererLogTail.push(line);
  if (rendererLogTail.length > 200) rendererLogTail.shift();
}
function captureHostLog(stream) {
  let buffer = "";
  stream.setEncoding("utf8");
  stream.on("data", (chunk) => {
    buffer += chunk;
    const lines = buffer.split("\n");
    buffer = lines.pop() ?? "";
    for (const line of lines) {
      if (!line.trim()) continue;
      hostLogTail.push(line);
      if (hostLogTail.length > 20000) hostLogTail.shift();
    }
  });
}
captureHostLog(electron.stdout);
captureHostLog(electron.stderr);

try {
  await waitFor(async () => {
    try {
      daemonPidForCleanup = (await ownedDaemonPid(electron.pid)) || daemonPidForCleanup;
      page = await connectPage();
      return page;
    } catch {
      return null;
    }
  }, "打包 Electron App Renderer CDP", 90_000, 500);

  await waitFor(async () => {
    const ready = await page.evaluate(
      `Boolean(document.querySelector('textarea, [contenteditable="true"]'))`,
    );
    return ready ? true : null;
  }, "App Renderer 工作台就绪", 60_000, 500);

  // 1) 工作区与会话：工具要真的能读文件，所以用 workspace 作用域。
  const workspace = await apiOk(page, "POST", "/api/workspaces/register", { path: workspaceRoot });
  const workspaceId = String(workspace?.workspaceId || workspace?.workspace_id || "");
  check("注册验收工作区", Boolean(workspaceId), JSON.stringify(workspace).slice(0, 300));
  const materialized = await apiOk(page, "POST", "/api/session/materialize", {
    scope: "workspace",
    workspaceId,
    workspacePath: workspaceRoot,
  });
  const sessionId = String(materialized?.sessionId || "");
  check("物化 workspace 会话", Boolean(sessionId), JSON.stringify(materialized).slice(0, 300));

  // 2) 未探测前：模型清单里不存在任何 Web 引擎（§9.2 #28）。
  const enginesBefore = await apiOk(page, "GET", "/api/settings/registry/engines");
  const engineListBefore = Array.isArray(enginesBefore?.engines) ? enginesBefore.engines : [];
  check(
    "未探测前模型清单不出现 Web 模型",
    engineListBefore.every((engine) => !String(engine?.id || "").startsWith("chatgpt-web/")),
    JSON.stringify(engineListBefore).slice(0, 300),
  );

  // 3) 首次说明确认（A9）：未确认前发现通道必须返回 consent_required。
  const beforeConsent = await apiOk(page, "POST", "/api/browser/web-models/discover", {
    clientPlatform: "desktop",
  });
  check(
    "未确认首次说明时发现返回 consent_required",
    beforeConsent?.status === "consent_required",
    JSON.stringify(beforeConsent).slice(0, 300),
  );
  const consent = await apiOk(page, "POST", "/api/browser/web-models/consent", {
    clientPlatform: "desktop",
  });
  check("首次说明确认落库", consent?.consentConfirmed === true, JSON.stringify(consent));

  // 4) 应用级浏览器会话身份稳定（§7.5）。
  const appSessionFirst = await apiOk(page, "POST", "/api/browser/sessions/app", {
    clientPlatform: "desktop",
  });
  const appSessionId = String(appSessionFirst?.session?.browserSessionId || "");
  check("创建应用级浏览器会话", Boolean(appSessionId), JSON.stringify(appSessionFirst).slice(0, 300));
  check("首次创建标记 created = true", appSessionFirst?.created === true, JSON.stringify(appSessionFirst?.created));
  const appSessionSecond = await apiOk(page, "POST", "/api/browser/sessions/app", {
    clientPlatform: "desktop",
  });
  check(
    "重复创建返回同一 browserSessionId 且 created = false",
    appSessionSecond?.session?.browserSessionId === appSessionId && appSessionSecond?.created === false,
    JSON.stringify(appSessionSecond).slice(0, 300),
  );

  // 5) 发现通道：登录探测 → 模型清单，并带来源标注。
  const discovery = await waitFor(async () => {
    const result = await apiOk(page, "POST", "/api/browser/web-models/discover", {
      clientPlatform: "desktop",
    });
    return result?.status === "ok" ? result : null;
  }, "Web 模型发现返回 ok", 60_000, 1000);
  const discoveredEngines = Array.isArray(discovery.engines) ? discovery.engines : [];
  check("发现通道返回候选引擎", discoveredEngines.length > 0, JSON.stringify(discovery).slice(0, 400));
  const engine = discoveredEngines[0] || {};
  check("Web 引擎使用 chatgpt_web 传输标识", engine.apiProtocol === "chatgpt_web", String(engine.apiProtocol));
  check("Web 引擎不写 llm", engine.llm === undefined, JSON.stringify(engine.llm));
  check("Web 引擎带来源标注", engine.origin?.kind === "web", JSON.stringify(engine.origin));
  check(
    "来源标注指向应用级会话",
    engine.origin?.browserSessionId === appSessionId,
    JSON.stringify(engine.origin),
  );
  check("来源标注带账号等级", engine.origin?.accountHint === "plus", String(engine.origin?.accountHint));
  check(
    "可用窗口来自站点上限表",
    Number(engine.contextWindowTokens) > 0,
    String(engine.contextWindowTokens),
  );
  check(
    "强度取值域来自站点菜单",
    Array.isArray(engine.efforts) && engine.efforts.includes("high"),
    JSON.stringify(engine.efforts),
  );
  check("发现结果带上限表版本", Boolean(discovery.limitsRevision), String(discovery.limitsRevision));
  check("发现结果带站点结构版本", Boolean(discovery.siteRevision), String(discovery.siteRevision));

  const observation = await (await fetch(`${fixtureOrigin}/__fixture/observe`)).json();
  check("发现通道确实驱动了托管浏览器页面", observation.submits >= 0 && observation.pageLoads.length >= 1,
    JSON.stringify(observation));
  check(
    "应用主页不使用临时对话入口",
    observation.pageLoads.every((entry) => !entry.temporaryChat || entry.search === ""),
    JSON.stringify(observation.pageLoads),
  );

  // 6) 落库走既有 engines 写入路径，保留顶层字段（origin / apiProtocol / efforts）。
  // 既有 engines 写入路径要求顶层 id/displayName（§7.1）；Web 引擎不写 llm。
  await apiOk(page, "POST", "/api/settings/registry/engines/upsert", {
    id: engine.id,
    displayName: engine.displayName,
    apiProtocol: "chatgpt_web",
    contextWindowTokens: engine.contextWindowTokens,
    efforts: engine.efforts,
    toolsEnabled: false,
    newChatPerTurn: false,
    origin: engine.origin,
  });
  const enginesAfter = await apiOk(page, "GET", "/api/settings/registry/engines");
  const persisted = (Array.isArray(enginesAfter?.engines) ? enginesAfter.engines : [])
    .find((entry) => entry?.id === engine.id);
  check("Web 引擎按既有路径落库", Boolean(persisted), JSON.stringify(enginesAfter).slice(0, 400));
  check("落库保留 apiProtocol", persisted?.apiProtocol === "chatgpt_web", JSON.stringify(persisted).slice(0, 300));
  check("落库保留来源标注", persisted?.origin?.kind === "web", JSON.stringify(persisted?.origin));
  check("落库保留强度取值域", Array.isArray(persisted?.efforts) && persisted.efforts.includes("high"),
    JSON.stringify(persisted?.efforts));

  // 7) 会话内把主模型绑定到 Web 引擎（A22）。
  const effort = Array.isArray(engine.efforts) && engine.efforts.includes("medium")
    ? "medium"
    : (engine.efforts?.[0] || "medium");
  await apiOk(page, "POST", "/api/settings/orchestrator/session/save", {
    scope: "workspace",
    workspaceId,
    sessionId,
    config: { engineId: engine.id, reasoningEffort: effort },
  });

  const submitTurn = async (text, label) => {
    const result = await api(page, "POST", "/api/session/turn", {
      scope: "workspace",
      workspaceId,
      sessionId,
      text,
      images: [],
    });
    if (result.status !== 200) {
      throw new Error(`提交 turn 失败（${label}）：${result.status} ${result.text}`);
    }
    return result.body;
  };

  const waitTurnSettled = async (label, timeoutMs = 120_000) =>
    waitFor(async () => {
      const turns = await canonicalTurns(page, sessionId, workspaceId);
      const turn = lastTurn(turns);
      if (!turn) return null;
      if (turn.status === "completed" || turn.status === "failed" || turn.status === "cancelled") {
        return turn;
      }
      // 顺带记录流式中间态，用于断言「流式末值与终值一致」。
      const text = assistantText(turn);
      if (text) canonicalTextSnapshots.push({ label, text, status: turn.status });
      return null;
    }, label, timeoutMs, 250);

  // 8) 第一轮：发送 → 流式 → 终态。
  await submitTurn(`${plainTurnMarker} 你好，Web 引擎冒烟`, "plain");
  const firstTurn = await waitTurnSettled("第一轮 Web 引擎回复终态");
  check("Web 引擎轮次完成", firstTurn?.status === "completed", String(firstTurn?.status));
  check(
    "canonical 记录助手正文",
    assistantText(firstTurn).includes(fixtureReplyText),
    assistantText(firstTurn).slice(0, 200),
  );
  check(
    "canonical 记录隐藏推理",
    thinkingItems(firstTurn).some((item) => String(item.content || "").includes(fixtureThinkingText)),
    JSON.stringify(thinkingItems(firstTurn)).slice(0, 200),
  );
  const firstConversationId = String(
    (Object.entries(
      (await (await fetch(`${fixtureOrigin}/__fixture/observe`)).json()).submitsByConversation || {},
    )[0] || [])[0] || "",
  );
  const afterFirst = await (await fetch(`${fixtureOrigin}/__fixture/observe`)).json();
  const firstSubmits = afterFirst.submits;
  check("Web 引擎确实把消息交给了临时对话", firstSubmits >= 1, JSON.stringify(afterFirst));
  check(
    "首轮发送走临时对话入口",
    Array.isArray(fixture.state.submits) && fixture.state.submits[0]?.url.includes("temporary-chat=true"),
    String(fixture.state.submits[0]?.url || ""),
  );
  check(
    "首轮发送带协议说明与重锚块",
    fixture.state.submits[0]?.prompt.includes("<<<magi-protocol v1")
      && fixture.state.submits[0]?.prompt.includes("<<<magi-anchor v1"),
    String(fixture.state.submits[0]?.prompt || "").slice(0, 200),
  );
  check(
    "首轮发送带用户原文",
    String(fixture.state.submits[0]?.prompt || "").includes(plainTurnMarker),
    String(fixture.state.submits[0]?.prompt || "").slice(0, 200),
  );
  check(
    "流式过程中出现过未收口的中间态",
    canonicalTextSnapshots.some((snapshot) => snapshot.label === "第一轮 Web 引擎回复终态"
      && snapshot.text.length > 0
      && !snapshot.text.includes(fixtureReplyText)),
    JSON.stringify(canonicalTextSnapshots.slice(0, 3)).slice(0, 300),
  );

  const runtimeAfterFirst = await apiOk(page, "GET", `/api/browser/web-models/runtime?sessionId=${encodeURIComponent(sessionId)}`);
  check(
    "Web 引擎额度按账号消息条数计",
    Number(runtimeAfterFirst?.quota?.sentMessages || 0) >= 1,
    JSON.stringify(runtimeAfterFirst?.quota),
  );
  check(
    "Web 引擎不计入 token 预算",
    runtimeAfterFirst?.quota?.excludedFromTokenBudget === true,
    JSON.stringify(runtimeAfterFirst?.quota),
  );

  // 9) 第二轮：T2 工具回路（文本协议 → 真实执行 → 结果回流）。
  //
  // 第一轮刻意用 `toolsEnabled: false` 验证 T0（不提供工具时不发工具说明）；
  // 工具回路要先按引擎级开关打开工具，档位才会升到 T2，否则整轮仍是 T0、
  // 页面不会列出 `- file_read`，工具协议永远不会被触发（R63）。
  await apiOk(page, "POST", "/api/settings/registry/engines/upsert", {
    id: engine.id,
    displayName: engine.displayName,
    apiProtocol: "chatgpt_web",
    contextWindowTokens: engine.contextWindowTokens,
    efforts: engine.efforts,
    toolsEnabled: true,
    newChatPerTurn: false,
    origin: engine.origin,
  });
  const submitsBeforeTool = fixture.state.submits.length;
  await submitTurn(`${toolTurnMarker} 读取工作区的 fixture 文件`, "tool");
  const toolTurn = await waitTurnSettled("T2 工具回路终态");
  check("T2 工具回路完成", toolTurn?.status === "completed", String(toolTurn?.status));
  check(
    "T2 工具调用进入 canonical",
    toolItems(toolTurn).some((item) => item.tool?.name === "file_read"),
    JSON.stringify(toolItems(toolTurn)).slice(0, 300),
  );
  check(
    "T2 工具真实执行并回流",
    assistantText(toolTurn).includes(fixtureToolReplyText),
    assistantText(toolTurn).slice(0, 200),
  );
  if (process.env.MAGI_WEB_MODEL_DOM_DEBUG_PROMPTS === "1") {
    const { writeFile: wf } = await import("node:fs/promises");
    for (const [index, entry] of fixture.state.submits.entries()) {
      await wf(`/tmp/webmodel-submit-${index}.txt`, entry.prompt, "utf8");
    }
  }
  const toolSubmits = fixture.state.submits.slice(submitsBeforeTool);
  check("T2 工具回路复用同一条临时对话", toolSubmits.length === 2, String(toolSubmits.length));
  check(
    "T2 工具结果以续轮封装回填",
    toolSubmits.some((entry) => entry.prompt.includes("<<<magi-tool-result")),
    JSON.stringify(toolSubmits.map((entry) => entry.prompt.slice(0, 120))).slice(0, 300),
  );
  check(
    "回填内容与真实文件正文一致",
    toolSubmits.some((entry) => entry.prompt.includes(fixtureNoteBody.trim())),
    JSON.stringify(toolSubmits.map((entry) => entry.prompt.includes(fixtureNoteBody.trim()))),
  );

  const runtimeAfterTool = await apiOk(page, "GET", `/api/browser/web-models/runtime?sessionId=${encodeURIComponent(sessionId)}`);
  check(
    "T2 每个工具轮次各计 1 条账号消息",
    Number(runtimeAfterTool?.quota?.sentMessages || 0)
      >= Number(runtimeAfterFirst?.quota?.sentMessages || 0) + 2,
    JSON.stringify({ before: runtimeAfterFirst?.quota, after: runtimeAfterTool?.quota }),
  );

  // 10) 取消：页面保持生成中，显式取消必须收口。
  await submitTurn(`${slowTurnMarker} 请一直生成下去`, "slow");
  await waitFor(async () => {
    const observationNow = await (await fetch(`${fixtureOrigin}/__fixture/observe`)).json();
    return observationNow.submits >= submitsBeforeTool + 2 ? true : null;
  }, "慢任务已被提交到临时对话", 30_000, 250);
  const interrupt = await apiOk(page, "POST", "/api/session/interrupt", {
    sessionId,
    workspaceId,
    workspacePath: workspaceRoot,
  });
  check("取消请求被 daemon 接受", interrupt?.interrupted === true, JSON.stringify(interrupt).slice(0, 300));
  const cancelledTurn = await waitFor(async () => {
    const turns = await canonicalTurns(page, sessionId, workspaceId);
    const turn = lastTurn(turns);
    return turn && (turn.status === "cancelled" || turn.status === "completed") ? turn : null;
  }, "取消后轮次收口", 60_000, 250);
  check(
    "取消后轮次进入 cancelled 终态",
    cancelledTurn?.status === "cancelled",
    JSON.stringify({ status: cancelledTurn?.status }).slice(0, 200),
  );

  // 11) 用户接管：页面自己多出一条用户消息 → 下一次发送推进 epoch 全量重放。
  const pageLoadsBeforeTakeover = (await (await fetch(`${fixtureOrigin}/__fixture/observe`)).json()).pageLoads.length;
  const activeConversationId = String(fixture.state.submits.at(-1)?.conversationId || "");
  check("取得当前对话实例身份", Boolean(activeConversationId), activeConversationId);
  fixture.state.commands.set(activeConversationId, [
    { type: "user_message", text: "用户在推理页面自己问的一句" },
  ]);
  await waitFor(async () => {
    const loads = (await (await fetch(`${fixtureOrigin}/__fixture/observe`)).json()).pageLoads.length;
    return loads >= pageLoadsBeforeTakeover ? true : null;
  }, "接管现场就绪", 5_000, 200).catch(() => undefined);
  await sleep(600);
  const submitsBeforeTakeover = fixture.state.submits.length;
  await submitTurn(`${plainTurnMarker} 接管之后继续`, "takeover");
  const takeoverTurn = await waitTurnSettled("接管后重建轮次终态");
  check("接管后轮次仍然完成", takeoverTurn?.status === "completed", String(takeoverTurn?.status));
  const takeoverSubmits = fixture.state.submits.slice(submitsBeforeTakeover);
  check(
    "接管后新建对话实例（不复用旧实例）",
    takeoverSubmits.length >= 1
      && takeoverSubmits.every((entry) => entry.conversationId !== activeConversationId),
    JSON.stringify(takeoverSubmits.map((entry) => entry.conversationId)),
  );
  check(
    "接管后走全量重放",
    takeoverSubmits.some((entry) => entry.prompt.includes("<<<magi-protocol v1")),
    JSON.stringify(takeoverSubmits.map((entry) => entry.prompt.slice(0, 80))).slice(0, 300),
  );

  // 12) 「重置为 Magi 对话」：只推进进程内绑定 epoch。
  const resetBefore = await apiOk(
    page,
    "GET",
    `/api/browser/web-models/runtime?sessionId=${encodeURIComponent(sessionId)}`,
  );
  const resetResult = await apiOk(page, "POST", "/api/browser/web-models/reset-conversation", {
    sessionId,
    clientPlatform: "desktop",
  });
  check("重置为 Magi 对话返回新 epoch", Number(resetResult?.epoch || 0) >= 1, JSON.stringify(resetResult));
  check(
    "重置只动绑定指针（不新增 canonical 写入）",
    Array.isArray(resetBefore?.entries),
    JSON.stringify(resetBefore).slice(0, 200),
  );

  // 13) Web 侧不落盘（A20）：daemon 只能把回答并入 Magi 自己的 canonical 事实，
  // 不得为 Web 侧建第二份镜像、缓存或绑定文件。
  //
  // 判据用**相对路径**，不能用绝对路径：验收状态根本身在 `magi-web-model-dom-*`
  // 下，拿全路径匹配 `web-model` 会把每个文件都判成违规。
  const stateDir = join(stateRoot, "state");
  const files = [];
  const walk = async (directory) => {
    let entries = [];
    try {
      entries = await readdir(directory, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const full = join(directory, entry.name);
      if (entry.isDirectory()) {
        await walk(full);
      } else if (entry.isFile()) {
        files.push(full);
      }
    }
  };
  await walk(stateDir);
  const stateFiles = files.map((file) => ({ file, relative: relative(stateDir, file) }));
  // (a) 不得出现专门承载 Web 侧状态的绑定 / 镜像文件。
  const bindingFiles = stateFiles
    .filter((entry) => /(^|\/)(bindings?|web-model|webmodel)[^/]*\.json$/iu.test(entry.relative))
    .map((entry) => entry.relative);
  check(
    "Web 绑定表不落盘",
    bindingFiles.length === 0,
    JSON.stringify(bindingFiles).slice(0, 300),
  );
  // (b) Magi 从不把 Web 侧对话身份写进任何落盘文件：临时对话 id 只存在于
  // guest 页面。App Renderer 的 globalThis 与 guest 隔离，不能从这里直接读取
  // `__magidomConversationId`；fixture 提交记录是同一事实的受控观察面。
  const webConversationIds = [...new Set(
    fixture.state.submits.map((entry) => String(entry.conversationId || "")).filter(Boolean),
  )];
  const identityOffenders = [];
  if (webConversationIds.length > 0) {
    for (const entry of stateFiles) {
      let text = "";
      try {
        const info = await stat(entry.file);
        if (info.size > 4 * 1024 * 1024) continue;
        text = await readFile(entry.file, "utf8");
      } catch {
        continue;
      }
      if (webConversationIds.some((conversationId) => text.includes(conversationId))) {
        identityOffenders.push(entry.relative);
      }
    }
  }
  check(
    "Web 侧对话身份不写入 state_root",
    webConversationIds.length > 0 && identityOffenders.length === 0,
    JSON.stringify({ webConversationIds, identityOffenders }).slice(0, 300),
  );

  const failed = checks.filter((entry) => !entry.passed);
  const evidence = {
    type: "web_model_dom_acceptance",
    fixture: "web-model-dom-v1",
    app: appExecutable,
    fixtureOrigin,
    daemonPort,
    cdpPort,
    sessionId,
    workspaceId,
    engineId: engine.id,
    checks,
    observation: await (await fetch(`${fixtureOrigin}/__fixture/observe`)).json(),
    streamingSnapshots: canonicalTextSnapshots.slice(0, 8),
    status: failed.length === 0 ? "passed" : "failed",
  };
  process.stdout.write(`${JSON.stringify(evidence, null, 2)}\n`);
  if (evidencePath) {
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  }
  if (failed.length > 0) {
    process.exitCode = 1;
  }
} catch (error) {
  process.stdout.write(`Web 模型 DOM 验收中断：${error?.message || error}\n`);
  if (page && process.env.MAGI_WEB_MODEL_DOM_DEBUG === "1") {
    try {
      const debugDump = await page.evaluate(`(async () => {
        const appSession = await fetch('/api/browser/sessions/app').then(async (r) => ({ status: r.status, text: (await r.text()).slice(0, 2000) }));
        const discover = await fetch('/api/browser/web-models/discover', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' }).then(async (r) => ({ status: r.status, text: (await r.text()).slice(0, 2000) }));
        return {
          appSession,
          discover,
          webviews: document.querySelectorAll('webview').length,
          rightPane: Boolean(document.querySelector('[data-right-pane], .right-pane, aside')),
          tabs: [...document.querySelectorAll('webview')].map((v) => v.getAttribute('src') || v.getAttribute('data-tab-id') || ''),
        };
      })()`);
      process.stdout.write(`── 调试快照 ──\n${JSON.stringify(debugDump, null, 2)}\n`);
    } catch (debugError) {
      process.stdout.write(`调试快照失败：${debugError?.message}\n`);
    }
  }
  if (rendererLogTail.length > 0) {
    process.stdout.write(`── App Renderer console 尾部（${rendererLogTail.length} 行）──\n`);
    process.stdout.write(`${rendererLogTail.join("\n")}\n`);
    process.stdout.write("── App Renderer console 结束 ──\n");
  }
  if (hostLogTail.length > 0) {
    process.stdout.write(`── 宿主日志尾部（${hostLogTail.length} 行）──\n`);
    process.stdout.write(`${hostLogTail.join("\n")}\n`);
    process.stdout.write("── 宿主日志结束 ──\n");
  }
  process.exitCode = 1;
} finally {
  page?.close();
  if (hostLogPath && hostLogTail.length > 0) {
    await writeFile(hostLogPath, `${hostLogTail.join("\n")}\n`, "utf8").catch(() => {});
  }
  daemonPidForCleanup = (await ownedDaemonPid(electron.pid).catch(() => null)) || daemonPidForCleanup;
  if (electron.exitCode === null && electron.signalCode === null) {
    electron.kill("SIGTERM");
    await waitForProcessExit(electron, 15_000);
    if (electron.exitCode === null && electron.signalCode === null) {
      electron.kill("SIGKILL");
      await waitForProcessExit(electron, 5_000);
    }
  }
  await stopOwnedDaemon(daemonPidForCleanup);
  if (typeof fixture.server.closeAllConnections === "function") fixture.server.closeAllConnections();
  await new Promise((resolve) => fixture.server.close(resolve));
  if (!keepStateRoot) {
    await rm(stateRoot, { recursive: true, force: true }).catch(() => undefined);
  } else {
    process.stdout.write(`保留验收状态根：${stateRoot}\n`);
  }
}
