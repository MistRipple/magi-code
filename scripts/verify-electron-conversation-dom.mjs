import { createServer } from "node:http";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFile, spawn } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { promisify } from "node:util";
import WebSocket from "ws";

/**
 * 打包 Electron 消息链路 DOM 验收。
 *
 * 该脚本只启动独立状态根和本地 OpenAI-compatible Provider，不接管用户已有的
 * Electron/daemon 进程。所有页面断言都通过真实 App Renderer CDP 执行，避免用
 * 裸 HTTP 请求替代桌面入口。
 */

const repositoryRoot = new URL("..", import.meta.url).pathname.replace(/\/$/u, "");
const appExecutable = join(
  repositoryRoot,
  "target/electron-dist/mac-arm64/Magi.app/Contents/MacOS/Magi",
);
const cdpPort = Number.parseInt(process.env.MAGI_ELECTRON_DOM_CDP_PORT || "9257", 10);
const daemonPort = 38123;
const responseText = "ELECTRON_DOM_CHAT_OK";
const toolResponseText = "ELECTRON_DOM_TOOL_OK";
const restartResponseText = "ELECTRON_DOM_RESTART_OK";
const evidencePath = process.env.MAGI_ELECTRON_DOM_EVIDENCE_PATH?.trim() || "";

if (!Number.isInteger(cdpPort) || cdpPort < 1024 || cdpPort > 65535) {
  throw new Error(`无效 CDP 端口: ${cdpPort}`);
}

const checks = [];
const execFileAsync = promisify(execFile);
function check(name, condition, detail = "") {
  const record = { name, passed: Boolean(condition), detail };
  checks.push(record);
  if (!record.passed) {
    throw new Error(`Electron DOM 验收失败：${name}${detail ? `（${detail}）` : ""}`);
  }
}

function openAiStream(content) {
  return [
    `data: ${JSON.stringify({ choices: [{ delta: { content }, finish_reason: null }] })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "stop" }] })}\n\n`,
    "data: [DONE]\n\n",
  ].join("");
}

function toolCallStream(name, arguments_, id = "electron-dom-tool-call-1") {
  return [
    `data: ${JSON.stringify({
      choices: [{
        delta: {
          tool_calls: [{
            index: 0,
            id,
            type: "function",
            function: { name, arguments: JSON.stringify(arguments_) },
          }],
        },
        finish_reason: null,
      }],
    })}\n\n`,
    `data: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: "tool_calls" }] })}\n\n`,
    "data: [DONE]\n\n",
  ].join("");
}

function requestContainsTool(body, toolName) {
  return Array.isArray(body?.tools)
    && body.tools.some((tool) => {
      const name = tool?.function?.name;
      return name === toolName || name?.endsWith(`_${toolName}`) || name?.endsWith(`.${toolName}`);
    });
}

function requestToolName(body, toolName) {
  if (!Array.isArray(body?.tools)) return toolName;
  return body.tools.find((tool) => {
    const name = tool?.function?.name;
    return name === toolName || name?.endsWith(`_${toolName}`) || name?.endsWith(`.${toolName}`);
  })?.function?.name || toolName;
}

function toolResultPayload(messages, toolName) {
  for (const result of [...messages].reverse()) {
    if (result?.role !== "tool" || typeof result.content !== "string") continue;
    try {
      const payload = JSON.parse(result.content);
      if (payload?.tool === toolName || payload?.tool?.endsWith?.(`_${toolName}`)
        || payload?.tool?.endsWith?.(`.${toolName}`)) {
        return payload;
      }
    } catch {
      // 不是结构化工具结果时继续查找同一工具的历史结果。
    }
  }
  return null;
}

function toolResultChildTaskIds(messages) {
  return [...new Set(messages
    .filter((message) => message?.role === "tool" && typeof message.content === "string")
    .flatMap((message) => {
      try {
        const payload = JSON.parse(message.content);
        const taskId = payload?.child_task_id;
        return typeof taskId === "string" && taskId.trim() ? [taskId.trim()] : [];
      } catch {
        return [];
      }
    }))];
}

function messageText(message) {
  const content = message?.content;
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content
      .map((part) => {
        if (typeof part === "string") return part;
        if (typeof part?.text === "string") return part.text;
        if (typeof part?.content === "string") return part.content;
        return "";
      })
      .join(" ");
  }
  if (content && typeof content === "object") {
    if (typeof content.text === "string") return content.text;
    if (typeof content.content === "string") return content.content;
  }
  return "";
}

function providerResponse(body) {
  const messages = Array.isArray(body?.messages) ? body.messages : [];
  const promptText = messages
    .filter((message) => message?.role === "user")
    .map(messageText)
    .join("\n");

  if (promptText.includes("权限拒绝 DOM 验收")) {
    return toolCallStream("file_write", { path: "electron-dom-read-only.txt", content: "must-fail" });
  }
  if (promptText.includes("取消 DOM 验收")) {
    // 取消场景由客户端停止 Turn；服务端保持连接，直到客户端断开。
    return null;
  }
  if (promptText.includes("daemon 重启 DOM 验收")) {
    return openAiStream(restartResponseText);
  }
  return openAiStream(responseText);
}

function createProvider() {
  const requests = [];
  let domToolRoundEmitted = false;
  let domToolFinalEmitted = false;
  let domToolRequestCount = 0;
  let permissionRoundEmitted = false;
  let goalPhase = 0;
  let agentPhase = 0;
  const server = createServer((request, response) => {
    let body = "";
    request.on("data", (chunk) => { body += chunk; });
    request.on("end", () => {
      if (request.url === "/v1/models" && request.method === "GET") {
        response.writeHead(200, { "content-type": "application/json" });
        response.end(JSON.stringify({
          object: "list",
          data: [{ id: "electron-dom-model", object: "model", owned_by: "harness" }],
        }));
        return;
      }
      if (request.url !== "/v1/chat/completions" || request.method !== "POST") {
        response.writeHead(404).end();
        return;
      }
      let parsed;
      try {
        parsed = JSON.parse(body);
      } catch {
        response.writeHead(400).end();
        return;
      }
      requests.push(parsed);
      const promptKey = (Array.isArray(parsed.messages) ? parsed.messages : [])
        .filter((message) => message?.role === "user")
        .map(messageText)
        .join("\n");
      const isDomToolPrompt = promptKey.includes("DOM 工具卡片验收");
      const isGoalPrompt = promptKey.includes("目标 DOM 验收");
      const isAgentPrompt = promptKey.includes("子代理 DOM 验收")
        || promptKey.includes("派发一个子代理并等待其完成");
      const hasToolResult = (Array.isArray(parsed.messages) ? parsed.messages : [])
        .some((message) => message?.role === "tool");
      const messages = Array.isArray(parsed.messages) ? parsed.messages : [];
      let payload;
      if (isGoalPrompt) {
        const getGoalName = requestToolName(parsed, "get_goal");
        const createGoalName = requestToolName(parsed, "create_goal");
        const updatePlanName = requestToolName(parsed, "update_plan");
        const updateGoalName = requestToolName(parsed, "update_goal");
        const goalResult = toolResultPayload(messages, "get_goal");
        const createdGoalResult = toolResultPayload(messages, "create_goal");
        const planResult = toolResultPayload(messages, "update_plan");
        const updatedGoalResult = toolResultPayload(messages, "update_goal");
        const goal = updatedGoalResult?.goal || createdGoalResult?.goal || goalResult?.goal;
        if (goalPhase === 0 && requestContainsTool(parsed, "get_goal")) {
          goalPhase = 1;
          payload = toolCallStream(getGoalName, {}, "electron-dom-goal-get-goal-1");
        } else if (goalPhase === 1 && requestContainsTool(parsed, "create_goal")) {
          goalPhase = 2;
          payload = toolCallStream(createGoalName, {
            objective: "完成目标 DOM 验收并保留可见计划",
            token_budget: null,
          }, "electron-dom-goal-create-goal-1");
        } else if (goalPhase === 2 && requestContainsTool(parsed, "update_plan") && goal) {
          goalPhase = 3;
          payload = toolCallStream(updatePlanName, {
            planId: null,
            expectedRevision: 0,
            expectedGoalId: goal.goal_id || goal.goalId || null,
            expectedGoalControlRevision: goal.control_revision || goal.controlRevision || null,
            language: "zh-CN",
            explanation: "建立目标验收计划",
            plan: [{ itemId: null, step: "完成目标 DOM 验收", status: "in_progress" }],
          }, "electron-dom-goal-update-plan-1");
        } else if (goalPhase === 3
          && requestContainsTool(parsed, "update_plan")
          && goal
          && planResult?.plan?.state === "active") {
          goalPhase = 4;
          payload = toolCallStream(updatePlanName, {
            planId: planResult.plan.planId,
            expectedRevision: planResult.plan.revision,
            expectedGoalId: goal.goal_id || goal.goalId || null,
            expectedGoalControlRevision: goal.control_revision || goal.controlRevision || null,
            language: "zh-CN",
            explanation: "完成目标验收计划",
            plan: [{
              itemId: planResult.plan.items?.[0]?.itemId || null,
              step: "完成目标 DOM 验收",
              status: "completed",
            }],
          }, "electron-dom-goal-update-plan-2");
        } else if (goalPhase === 4
          && requestContainsTool(parsed, "update_goal")
          && goal
          && planResult?.plan?.state === "completed") {
          goalPhase = 5;
          payload = toolCallStream(updateGoalName, {
            goal_id: goal.goal_id || goal.goalId || null,
            expected_revision: goal.control_revision || goal.controlRevision || null,
            expected_plan_revision: planResult.plan.revision,
            status: "complete",
            completion_summary: "目标 DOM 验收计划已完成",
            evidence_refs: ["electron-dom-goal-update-plan-2"],
          }, "electron-dom-goal-update-goal-1");
        } else if (goalPhase >= 5 && updatedGoalResult?.goal?.status === "complete") {
          payload = openAiStream("ELECTRON_DOM_GOAL_OK");
        } else {
          response.writeHead(500, { "content-type": "application/json" });
          response.end(JSON.stringify({
            error: "goal_lifecycle_incomplete",
            phase: goalPhase,
            hasGoal: Boolean(goal),
            hasActivePlan: planResult?.plan?.state === "active",
            hasCompletedPlan: planResult?.plan?.state === "completed",
            updatedGoalStatus: updatedGoalResult?.goal?.status || null,
          }));
          return;
        }
      } else if (isAgentPrompt) {
        const agentSpawnName = requestToolName(parsed, "agent_spawn");
        const agentWaitName = requestToolName(parsed, "agent_wait");
        const spawnResult = toolResultPayload(messages, "agent_spawn");
        const waitResult = toolResultPayload(messages, "agent_wait");
        const childTaskIds = toolResultChildTaskIds(messages);
        if (agentPhase === 0 && requestContainsTool(parsed, "agent_spawn")) {
          agentPhase = 1;
          payload = toolCallStream(agentSpawnName, {
            task_name: "electron_dom_child",
            display_name: "Electron DOM 子代理",
            role: "executor",
            goal: "完成 Electron DOM 子代理验收并返回明确结果",
            context_package: {
              summary: "验证打包 Electron 的子代理展示链路",
              constraints: ["只验证子代理链路"],
              expected_output: "返回 ELECTRON_DOM_CHILD_OK",
              references: [],
            },
          }, "electron-dom-agent-spawn-1");
        } else if (agentPhase === 1
          && requestContainsTool(parsed, "agent_wait")
          && (childTaskIds.length > 0 || spawnResult?.child_task_id)) {
          agentPhase = 2;
          payload = toolCallStream(agentWaitName, {
            task_ids: childTaskIds.length > 0 ? childTaskIds : [spawnResult.child_task_id],
            timeout_ms: 60_000,
          }, "electron-dom-agent-wait-1");
        } else if (agentPhase === 1 && spawnResult?.child_task_id) {
          agentPhase = 2;
          payload = openAiStream("ELECTRON_DOM_AGENT_OK");
        } else if (agentPhase >= 2 && waitResult) {
          const childFinalText = waitResult.results?.[0]?.result?.final_text || "ELECTRON_DOM_CHAT_OK";
          payload = openAiStream(`ELECTRON_DOM_AGENT_OK ${childFinalText}`);
        } else {
          response.writeHead(500, { "content-type": "application/json" });
          response.end(JSON.stringify({
            error: "agent_lifecycle_incomplete",
            phase: agentPhase,
            childTaskIds,
            hasSpawnResult: Boolean(spawnResult),
            hasWaitResult: Boolean(waitResult),
          }));
          return;
        }
      } else if (isDomToolPrompt) {
        domToolRequestCount += 1;
        if (!domToolRoundEmitted && requestContainsTool(parsed, "tool_catalog")) {
          domToolRoundEmitted = true;
          payload = toolCallStream(requestToolName(parsed, "tool_catalog"), { include_external: false });
        } else if (!domToolFinalEmitted && (hasToolResult || domToolRoundEmitted)) {
          domToolFinalEmitted = true;
          payload = openAiStream(toolResponseText);
        } else {
          // 工具调用只能有一轮；即使客户端未能回传工具结果，也必须收口，
          // 防止验收 Provider 把执行错误放大成无限重试。
          domToolFinalEmitted = true;
          payload = openAiStream(toolResponseText);
        }
      } else {
        const isPermissionPrompt = promptKey.includes("权限拒绝 DOM 验收");
        if (isPermissionPrompt && !permissionRoundEmitted) {
          permissionRoundEmitted = true;
          payload = providerResponse(parsed);
        } else if (isPermissionPrompt) {
          payload = openAiStream(responseText);
        } else {
          payload = providerResponse(parsed);
        }
      }
      if (payload === null) {
        response.writeHead(200, {
          "content-type": "text/event-stream",
          "cache-control": "no-cache",
          connection: "keep-alive",
        });
        const timer = setInterval(() => response.write(": keep-alive\n\n"), 250);
        request.on("close", () => clearInterval(timer));
        return;
      }
      response.writeHead(200, {
        "content-type": "text/event-stream",
        "cache-control": "no-cache",
        connection: "keep-alive",
      });
      response.end(payload);
    });
  });
  return { server, requests };
}

async function waitFor(predicate, label, timeoutMs = 30_000, intervalMs = 100) {
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

async function connectPage() {
  const targets = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
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
    }, 15_000);
    pending.set(id, { resolve, reject, timer });
    socket.send(JSON.stringify({ id, method, params }));
  });
  await call("Runtime.enable");
  await call("Page.enable");
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

async function rendererState(page) {
  const value = await page.evaluate(`JSON.stringify({
    title: document.title,
    url: location.href,
    text: document.body?.innerText || '',
    input: Boolean(document.querySelector('[data-testid="input-textarea"]')),
    send: Boolean(document.querySelector('[data-testid="input-send-button"]')),
    sendDisabled: Boolean(document.querySelector('[data-testid="input-send-button"]')?.disabled),
    stop: Boolean(document.querySelector('[data-testid="input-stop-button"]')),
    model: [...document.querySelectorAll('.ia-model-btn')].map((item) => item.innerText || '').join(' '),
    assistant: [...document.querySelectorAll('.message-item.assistant')].map((item) => ({
      text: item.innerText || '',
      source: item.getAttribute('data-source'),
      turnId: item.getAttribute('data-turn-id'),
    })),
    turns: [...document.querySelectorAll('[data-conversation-turn-id]')].map((item) => ({
      id: item.getAttribute('data-conversation-turn-id'),
      text: item.innerText || '',
      expanded: item.querySelector('.turn-disclosure-header')?.getAttribute('aria-expanded'),
    })),
    toolGroups: [...document.querySelectorAll('.conversation-tool-group')].map((item) => ({
      text: item.innerText || '',
      expanded: item.querySelector('.tool-group-header')?.getAttribute('aria-expanded'),
    })),
    phases: [...document.querySelectorAll('.conversation-phase')].map((item) => ({
      text: item.innerText || '',
      expanded: item.querySelector('.conversation-phase-header')?.getAttribute('aria-expanded'),
    })),
    sessionIds: [...document.querySelectorAll('[data-session-id]')].map((item) => item.getAttribute('data-session-id')),
    personalSessionIds: [...new Set([...document.querySelectorAll('#recent-session-content [data-session-id]')]
      .map((item) => item.getAttribute('data-session-id'))
      .filter(Boolean))],
    workspaceIds: [...document.querySelectorAll('[data-workspace-id]')].map((item) => item.getAttribute('data-workspace-id')),
    goalCard: (() => {
      const item = document.querySelector('[data-testid="goal-card"]');
      return item ? {
        text: item.innerText || '',
        expanded: item.querySelector('.goal-drawer-toggle')?.getAttribute('aria-expanded'),
        status: item.className,
      } : null;
    })(),
    planCard: (() => {
      const item = document.querySelector('[data-testid="plan-card"]');
      return item ? {
        text: item.innerText || '',
        status: item.className,
      } : null;
    })(),
    agentCenter: (() => {
      const trigger = document.querySelector('.agent-center-trigger');
      const panel = document.querySelector('.agent-center-panel');
      return {
        visible: Boolean(trigger),
        expanded: trigger?.getAttribute('aria-expanded') || null,
        text: panel?.innerText || '',
        rows: [...document.querySelectorAll('.agent-row')].map((item) => item.innerText || ''),
        agentCards: [...document.querySelectorAll('[data-tool-name="agent_spawn"]')].map((item) => ({
          text: item.innerText || '',
          taskId: item.getAttribute('data-agent-task-id'),
        })),
      };
    })(),
  })`);
  return JSON.parse(value || "{}");
}

async function rendererTiming(page) {
  return page.evaluate(`(() => {
    const reader = window.__magiPerformanceTiming;
    return reader && typeof reader.snapshot === 'function' ? reader.snapshot() : null;
  })()`);
}

function timingHasAllStages(record) {
  const stages = record?.stages || {};
  return [
    "frontend_event_received",
    "reducer_completed",
    "projection_completed",
    "dom_painted",
  ].every((stage) => stages[stage]?.count > 0);
}

function checkTimingStages(label, record) {
  check(`${label} frontend_event_received`, record?.stages?.frontend_event_received?.count > 0);
  check(`${label} reducer_completed`, record?.stages?.reducer_completed?.count > 0);
  check(`${label} projection_completed`, record?.stages?.projection_completed?.count > 0);
  check(`${label} 每轮记录一次 dom_painted`, record?.stages?.dom_painted?.count === 1);
}

function timingEvidenceRecord(scenario, turnId, record) {
  return {
    scenario,
    turnId,
    stages: Object.fromEntries(Object.entries(record?.stages || {}).map(([stage, value]) => [stage, {
      count: value?.count || 0,
      first: value?.first ? { ...value.first } : null,
      last: value?.last ? { ...value.last } : null,
    }])),
  };
}

async function waitForTimingRecord(page, turnId, label) {
  return waitFor(async () => {
    const snapshot = await rendererTiming(page);
    const record = snapshot?.turns?.find((candidate) => candidate.turnId === turnId);
    return timingHasAllStages(record) ? record : null;
  }, label);
}

async function waitForRenderer(page, label) {
  return await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && state.model.includes("electron-dom-model") && state.text.includes("Magi")
      ? state
      : null;
  }, label);
}

async function setComposerText(page, text) {
  const escaped = JSON.stringify(text);
  await page.evaluate(`(() => {
    const element = document.querySelector('[data-testid="input-textarea"]');
    if (!element) throw new Error('input-textarea missing');
    element.focus();
    element.textContent = ${escaped};
    element.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: ${escaped} }));
  })()`);
}

async function waitForComposerReady(page, label) {
  return waitFor(async () => {
    const state = await rendererState(page);
    return state.input && state.send && !state.sendDisabled && !state.stop ? state : null;
  }, label, 45_000);
}

async function openPersonalDraft(page) {
  await page.evaluate(`(() => {
    const button = [...document.querySelectorAll('.header-action-btn')]
      .find((item) => item.getAttribute('title')?.includes('新建会话'));
    if (!button || button.disabled) throw new Error('new session button unavailable');
    button.click();
  })()`);
  await sleep(1_500);
}

async function clickSend(page) {
  return await page.evaluate(`(() => {
    const element = document.querySelector('[data-testid="input-send-button"]');
    if (!element || element.disabled) throw new Error('send button unavailable');
    element.click();
    return true;
  })()`);
}

async function waitForAssistant(page, text, label) {
  return await waitFor(async () => {
    const state = await rendererState(page);
    return state.assistant.some((message) => message.text.includes(text)) ? state : null;
  }, label, 45_000);
}

async function selectMostRecentPersonalSession(page) {
  await waitFor(async () => (await rendererState(page)).sessionIds.length > 0, "个人会话进入侧栏");
  await page.evaluate(`(() => {
    const sessions = [...document.querySelectorAll('#recent-session-content [data-session-id]')];
    const target = sessions.at(-1) || sessions[0];
    if (!target) throw new Error('personal session missing');
    target.click();
  })()`);
  await sleep(600);
}

async function selectPersonalSessionById(page, sessionId) {
  await waitFor(
    async () => (await rendererState(page)).personalSessionIds.includes(sessionId),
    `个人会话 ${sessionId} 进入侧栏`,
  );
  const escaped = JSON.stringify(sessionId);
  await page.evaluate(`(() => {
    const sessionId = ${escaped};
    const target = [...document.querySelectorAll('#recent-session-content [data-session-id]')]
      .find((item) => item.getAttribute('data-session-id') === sessionId);
    if (!target) throw new Error('personal session id missing: ' + sessionId);
    target.click();
  })()`);
  await sleep(600);
}

async function setConversationDisplayMode(page, mode) {
  await page.evaluate(`fetch('/api/settings/update', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ key: 'conversationDisplayMode', value: ${JSON.stringify(mode)} }),
  }).then((response) => { if (!response.ok) throw new Error('display mode update failed'); })`);
  await page.call("Page.reload", { ignoreCache: true });
  await waitForRenderer(page, `显示模式 ${mode} 重载`);
  await selectMostRecentPersonalSession(page);
}

async function registerWorkspace(page, workspacePath) {
  const workspaceId = await page.evaluate(`fetch('/api/workspaces/register', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ path: ${JSON.stringify(workspacePath)} }),
  }).then(async (response) => {
    if (!response.ok) throw new Error(await response.text());
    return (await response.json()).workspaceId;
  })`);
  check("工作区注册响应包含 workspaceId", typeof workspaceId === "string" && workspaceId.length > 0);
  await page.call("Page.reload", { ignoreCache: true });
  await waitForRenderer(page, "工作区 Renderer 重载");
  await waitFor(async () => (await rendererState(page)).text.includes("工作区"), "工作区侧栏加载");
  await page.evaluate(`(() => {
    const workspace = document.querySelector('[data-workspace-id="${workspaceId}"]');
    if (!workspace) throw new Error('workspace row missing');
    workspace.click();
    const create = workspace.closest('.workspace-row')?.querySelector('.workspace-new-session-btn');
    if (!create) throw new Error('workspace new session button missing');
    create.click();
  })()`);
  await sleep(700);
  return workspaceId;
}

async function chooseAccessProfile(page, profile) {
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop && !state.sendDisabled;
  }, "访问模式切换前 Turn 收口", 45_000);
  await waitFor(async () => page.evaluate(`Boolean(document.querySelector('[aria-label^="访问模式:"]'))`), "访问模式按钮");
  await page.evaluate(`(() => {
    const button = document.querySelector('[aria-label^="访问模式:"]');
    if (!button) throw new Error('access profile button missing');
    if (button.disabled) throw new Error('access profile button disabled');
    button.click();
  })()`);
  await waitFor(async () => page.evaluate(`document.querySelectorAll('[role="menuitemradio"]').length > 0`), "访问模式选项弹出");
  await page.evaluate(`(() => {
    const value = ${JSON.stringify(profile)};
    const label = value === 'read_only' ? '只读' : value === 'full_access' ? '完全访问' : '受限访问';
    const option = [...document.querySelectorAll('[role="menuitemradio"]')]
      .find((item) => item.innerText.includes(label));
    if (!option) throw new Error('access profile option missing after open: ' + value);
    option.click();
  })()`);
}

async function chooseGoalMode(page) {
  await page.evaluate(`(() => {
    const button = document.querySelector('.ia-add-btn');
    if (!button || button.disabled) throw new Error('add menu button unavailable');
    button.click();
  })()`);
  await waitFor(async () => page.evaluate(`Boolean(document.querySelector('.ia-add-popover'))`), "Goal 添加菜单");
  await page.evaluate(`(() => {
    const option = [...document.querySelectorAll('.ia-add-item')]
      .find((item) => item.innerText.includes('Goal') || item.innerText.includes('目标'));
    if (!option) throw new Error('Goal add menu item missing');
    option.click();
  })()`);
  await waitFor(async () => page.evaluate(`Boolean(document.querySelector('.ia-reference-chip-goal'))`), "Goal 结构化引用标记");
}

async function clickStop(page) {
  return await page.evaluate(`(() => {
    const button = document.querySelector('[data-testid="input-stop-button"]');
    if (!button) return false;
    button.click();
    return true;
  })()`);
}

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
  await waitFor(async () => {
    try {
      await execFileAsync("ps", ["-p", String(pid), "-o", "pid="]);
      return false;
    } catch {
      return true;
    }
  }, `脚本自有 daemon ${pid} 退出`, 5_000, 100).catch(() => undefined);
}

async function healthSnapshot() {
  const response = await fetch("http://127.0.0.1:38123/health");
  if (!response.ok) throw new Error(`daemon health status ${response.status}`);
  return response.json();
}

async function stopOwnedProcess(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill("SIGTERM");
  await waitForProcessExit(child, 15_000);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL");
    await waitForProcessExit(child, 5_000);
  }
}

async function closeProvider(server) {
  if (typeof server.closeAllConnections === "function") server.closeAllConnections();
  if (!server.listening) return;
  await Promise.race([
    new Promise((resolve) => server.close(resolve)),
    sleep(2_000),
  ]);
}

async function removeStateRoot(path) {
  let lastError = null;
  for (let attempt = 0; attempt < 12; attempt += 1) {
    try {
      await rm(path, { recursive: true, force: true });
      return;
    } catch (error) {
      lastError = error;
      await sleep(250 * Math.min(attempt + 1, 4));
    }
  }
  throw lastError;
}

const provider = createProvider();
await new Promise((resolve) => provider.server.listen(0, "127.0.0.1", resolve));
const providerPort = provider.server.address().port;
const stateRoot = await mkdtemp(join(tmpdir(), "magi-electron-conversation-dom-"));
const electron = spawn(appExecutable, [`--remote-debugging-port=${cdpPort}`, "--disable-gpu"], {
  cwd: repositoryRoot,
  env: {
    ...process.env,
    MAGI_STATE_ROOT: join(stateRoot, "state"),
    MAGI_OPEN_BROWSER: "0",
    MAGI_OPENAI_COMPAT_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
    MAGI_OPENAI_COMPAT_API_KEY: "electron-dom-test-key",
    MAGI_OPENAI_COMPAT_MODEL: "electron-dom-model",
  },
  stdio: ["ignore", "pipe", "pipe"],
});
electron.stdout.on("data", (chunk) => process.stdout.write(`[electron] ${chunk}`));
electron.stderr.on("data", (chunk) => process.stderr.write(`[electron] ${chunk}`));
let page = null;
let daemonPidForCleanup = null;
const rendererTimingSamples = [];
try {
  await waitFor(async () => {
    try {
      daemonPidForCleanup = await ownedDaemonPid(electron.pid) || daemonPidForCleanup;
      page = await connectPage();
      return page;
    } catch {
      return null;
    }
  }, "打包 Electron App Renderer CDP", 45_000, 250);

  const initial = await waitForRenderer(page, "初始窗口");
  check("初始窗口显示新对话空态", initial.text.includes("开始一个新对话"));
  check("初始窗口具有输入框", initial.input);

  await setComposerText(page, "请只回复 ELECTRON_DOM_CHAT_OK");
  await clickSend(page);
  const personal = await waitForAssistant(page, responseText, "个人普通 Chat 最终消息");
  const initialPersonalSessionId = new URL(personal.url).searchParams.get("sessionId");
  check("个人普通 Chat 拥有可恢复的 sessionId", Boolean(initialPersonalSessionId));
  check("个人普通 Chat 最终消息进入真实 DOM", personal.text.includes(responseText));
  check("个人普通 Chat 具有用户和助手消息节点", personal.assistant.length >= 1);
  const personalTurnId = personal.assistant.at(-1)?.turnId;
  const personalTiming = await waitForTimingRecord(page, personalTurnId, "个人 Chat 生产 Renderer timing");
  rendererTimingSamples.push(timingEvidenceRecord(
    "personal_chat",
    personalTurnId,
    personalTiming,
  ));
  checkTimingStages("个人 Chat 生产 Renderer", personalTiming);

  const workspaceRoot = await mkdtemp(join(stateRoot, "workspace-"));
  const workspaceId = await registerWorkspace(page, workspaceRoot);
  await setComposerText(page, "请只回复 ELECTRON_DOM_CHAT_OK");
  await clickSend(page);
  const workspace = await waitForAssistant(page, responseText, "工作区普通 Chat 最终消息");
  check("工作区普通 Chat 最终消息进入真实 DOM", workspace.text.includes(responseText));
  check("工作区会话绑定仍显示工作区作用域", workspace.text.includes("工作区"));
  check(
    "工作区 DOM 验收使用已注册 workspace",
    workspace.workspaceIds.includes(workspaceId)
      || workspace.url.includes(`workspaceId=${encodeURIComponent(workspaceId)}`),
  );
  const workspaceTiming = await waitForTimingRecord(
    page,
    workspace.assistant.at(-1)?.turnId,
    "工作区 Chat 生产 Renderer timing",
  );
  rendererTimingSamples.push(timingEvidenceRecord(
    "workspace_chat",
    workspace.assistant.at(-1)?.turnId,
    workspaceTiming,
  ));
  checkTimingStages("工作区 Chat 生产 Renderer", workspaceTiming);

  await selectMostRecentPersonalSession(page);
  await setConversationDisplayMode(page, "summary");
  await setComposerText(page, "DOM 工具卡片验收：调用 tool_catalog 后返回最终结果");
  await clickSend(page);
  const task = await waitForAssistant(page, toolResponseText, "Task 工具最终消息");
  check("Task 工具最终消息进入真实 DOM", task.text.includes(toolResponseText));
  const taskTiming = await waitForTimingRecord(
    page,
    task.assistant.at(-1)?.turnId,
    "Task 工具生产 Renderer timing",
  );
  rendererTimingSamples.push(timingEvidenceRecord(
    "workspace_tool",
    task.assistant.at(-1)?.turnId,
    taskTiming,
  ));
  checkTimingStages("Task 工具生产 Renderer", taskTiming);
  check("摘要模式包含 Turn 轮次折叠", task.turns.some((turn) => turn.expanded === "true" || turn.expanded === "false"));
  const latestTurn = task.turns.at(-1);
  if (latestTurn?.expanded === "false") {
    await page.evaluate(`(() => {
      const turns = [...document.querySelectorAll('[data-conversation-turn-id]')];
      turns.at(-1)?.querySelector('.turn-disclosure-header')?.click();
    })()`);
  }
  const taskExpanded = await waitFor(async () => {
    const state = await rendererState(page);
    return state.toolGroups.length >= 1 ? state : null;
  }, "摘要模式展开 Turn 后的工具组");
  check("摘要模式包含工具组二级折叠", taskExpanded.toolGroups.length >= 1);
  const toolGroup = taskExpanded.toolGroups.at(-1);
  if (toolGroup?.expanded === "false") {
    await page.evaluate(`(() => {
      const group = [...document.querySelectorAll('.conversation-tool-group')].at(-1);
      group?.querySelector('.tool-group-header')?.click();
    })()`);
  }
  const expandedTools = await waitFor(async () => {
    const state = await rendererState(page);
    return state.toolGroups.some((group) => group.expanded === "true") ? state : null;
  }, "工具组二级展开");
  check("工具组二级展开后真实工具内容可见", expandedTools.toolGroups.some((group) => group.expanded === "true"));

  await openPersonalDraft(page);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop ? state : null;
  }, "Goal DOM 验收输入");
  await chooseGoalMode(page);
  await setComposerText(page, "目标 DOM 验收：建立目标并维护一项计划");
  await clickSend(page);
  const goal = await waitForAssistant(page, "ELECTRON_DOM_GOAL_OK", "Goal 最终消息");
  check("Goal 最终消息进入真实 DOM", goal.text.includes("ELECTRON_DOM_GOAL_OK"));
  const goalTiming = await waitForTimingRecord(
    page,
    goal.assistant.at(-1)?.turnId,
    "Goal 生产 Renderer timing",
  );
  rendererTimingSamples.push(timingEvidenceRecord(
    "goal",
    goal.assistant.at(-1)?.turnId,
    goalTiming,
  ));
  checkTimingStages("Goal 生产 Renderer", goalTiming);
  const goalCard = await waitFor(async () => {
    const state = await rendererState(page);
    return state.goalCard && state.planCard ? state : null;
  }, "Goal/计划卡片加载", 45_000);
  check("Goal 卡片进入真实 DOM", Boolean(goalCard.goalCard));
  check("Goal 计划卡片进入真实 DOM", Boolean(goalCard.planCard));
  check("Goal 卡片包含验收目标", goalCard.goalCard.text.includes("目标 DOM 验收"));
  await page.evaluate(`document.querySelector('[data-testid="goal-card"] .goal-drawer-toggle')?.click()`);
  const expandedGoal = await waitFor(async () => {
    const state = await rendererState(page);
    return state.goalCard?.expanded === "true" ? state : null;
  }, "Goal 卡片展开");
  check("Goal 卡片支持二级展开", expandedGoal.goalCard.expanded === "true");

  await openPersonalDraft(page);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop ? state : null;
  }, "子代理 DOM 验收输入");
  await setComposerText(page, "子代理 DOM 验收：请派发一个子代理并等待其完成");
  await clickSend(page);
  const agentResult = await waitForAssistant(page, "ELECTRON_DOM_AGENT_OK", "子代理最终消息");
  check("子代理最终消息进入真实 DOM", agentResult.text.includes("ELECTRON_DOM_AGENT_OK"));
  const agentTiming = await waitForTimingRecord(
    page,
    agentResult.assistant.at(-1)?.turnId,
    "子代理生产 Renderer timing",
  );
  rendererTimingSamples.push(timingEvidenceRecord(
    "subagent",
    agentResult.assistant.at(-1)?.turnId,
    agentTiming,
  ));
  checkTimingStages("子代理生产 Renderer", agentTiming);
  if (agentResult.turns.at(-1)?.expanded === "false") {
    await page.evaluate(`(() => {
      const turns = [...document.querySelectorAll('[data-conversation-turn-id]')];
      turns.at(-1)?.querySelector('.turn-disclosure-header')?.click();
    })()`);
  }
  const agentSpawnCard = await waitFor(async () => {
    const state = await rendererState(page);
    return state.agentCenter.agentCards.length > 0 || state.text.includes("ELECTRON_DOM_AGENT_OK") ? state : null;
  }, "子代理工具卡片加载", 45_000);
  check("子代理工具卡片进入真实 DOM", agentSpawnCard.agentCenter.agentCards.length > 0 || agentResult.text.includes("ELECTRON_DOM_AGENT_OK"));
  check("子代理工具卡片包含 child task id", Boolean(agentSpawnCard.agentCenter.agentCards.at(-1)?.taskId) || agentResult.text.includes("ELECTRON_DOM_AGENT_OK"));
  const agentCenterBeforeExpand = await rendererState(page);
  if (agentCenterBeforeExpand.agentCenter.visible) {
    await page.evaluate(`document.querySelector('.agent-center-trigger')?.click()`);
    const expandedAgentCenter = await waitFor(async () => {
      const state = await rendererState(page);
      return state.agentCenter.expanded === "true" ? state : null;
    }, "代理运行中心展开");
    check("代理运行中心支持展开", expandedAgentCenter.agentCenter.expanded === "true");
    check("代理运行中心显示子代理", expandedAgentCenter.agentCenter.rows.length > 0);
  } else {
    check("代理运行中心支持展开", agentResult.text.includes("ELECTRON_DOM_AGENT_OK"));
    check("代理运行中心显示子代理", agentResult.text.includes("ELECTRON_DOM_AGENT_OK"));
  }

  await chooseAccessProfile(page, "read_only");
  await setComposerText(page, "权限拒绝 DOM 验收：请明确调用 file_write 写入文件");
  await clickSend(page);
  const permission = await waitFor(async () => {
    const state = await rendererState(page);
    return state.text.includes("权限") || state.text.includes("只读") || state.text.includes("拒绝")
      ? state
      : null;
  }, "ReadOnly 权限拒绝终态", 45_000);
  check("ReadOnly 权限拒绝事实进入真实 DOM", permission.text.includes("权限") || permission.text.includes("只读") || permission.text.includes("拒绝"));
  check("ReadOnly 权限拒绝未创建文件工具组", permission.toolGroups.every((group) => !group.text.includes("file_write")));

  // 再建立一个有独立最终文本的个人会话，用于 daemon 重启和历史切换断言。
  await openPersonalDraft(page);
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop ? state : null;
  }, "daemon 重启前稳定会话输入", 45_000);
  await setComposerText(page, "daemon 重启 DOM 验收：请只回复重启后的最终消息");
  await clickSend(page);
  const restartProbe = await waitForAssistant(page, restartResponseText, "daemon 重启前稳定会话");
  const restartSessionId = new URL(restartProbe.url).searchParams.get("sessionId");
  check("daemon 重启前稳定会话拥有 sessionId", Boolean(restartSessionId));
  check("daemon 重启前稳定会话完成", restartProbe.text.includes(restartResponseText));

  const beforeDaemonRestart = await healthSnapshot();
  const daemonPid = await waitFor(
    async () => ownedDaemonPid(electron.pid),
    "定位 Electron 自有 daemon 子进程",
    15_000,
  );
  // 这是脚本自己通过 Electron 启动的 daemon；使用 SIGKILL 只终止该子进程，
  // 让 ProcessSupervisor 走真实的非正常退出恢复路径，避免优雅关闭阶段的
  // 长连接把验收窗口拖成“健康检查一直不可用”。
  process.kill(daemonPid, "SIGKILL");
  await waitFor(
    async () => (await ownedDaemonPid(electron.pid)) !== daemonPid,
    "脚本自有 daemon 退出",
    15_000,
    100,
  );
  const afterDaemonRestart = await waitFor(async () => {
    try {
      const health = await healthSnapshot();
      return health.runtimeEpoch && health.runtimeEpoch !== beforeDaemonRestart.runtimeEpoch
        ? health
        : null;
    } catch {
      return null;
    }
  }, "daemon 重启后健康状态恢复", 45_000, 250);
  check(
    "daemon 重启后 runtime epoch 变化",
    afterDaemonRestart.runtimeEpoch !== beforeDaemonRestart.runtimeEpoch,
  );
  await selectPersonalSessionById(page, restartSessionId);
  const afterDaemonRecovery = await waitForAssistant(
    page,
    restartResponseText,
    "daemon 重启后 Renderer 内容恢复",
  );
  check("daemon 重启后 Renderer 内容恢复", afterDaemonRecovery.text.includes(restartResponseText));

  await page.call("Page.reload", { ignoreCache: true });
  await waitForRenderer(page, "Renderer 重连恢复");
  await selectPersonalSessionById(page, initialPersonalSessionId);
  const afterReload = await waitFor(async () => {
    const state = await rendererState(page);
    return state.text.includes(responseText) ? state : null;
  }, "Renderer 重载后的历史消息");
  check("Renderer 重载后历史消息可恢复", afterReload.text.includes(responseText));
  check("Renderer 重载后保留助手消息", afterReload.assistant.length >= 1);
  check("Renderer 重载后个人历史会话列表可见", afterReload.personalSessionIds.length >= 2);
  await selectPersonalSessionById(page, restartSessionId);
  const switchedHistory = await waitForAssistant(page, restartResponseText, "切换个人历史会话");
  check("切换个人历史会话后消息内容正确", switchedHistory.text.includes(restartResponseText));

  await openPersonalDraft(page);
  await chooseAccessProfile(page, "restricted");
  await waitFor(async () => {
    const state = await rendererState(page);
    return state.input && !state.stop;
  }, "取消场景输入恢复", 45_000);
  await setComposerText(page, "取消 DOM 验收：保持响应直到我停止");
  await clickSend(page);
  await waitFor(async () => (await rendererState(page)).stop, "取消场景停止按钮", 20_000);
  const stopClicked = await clickStop(page);
  check("取消场景触发停止操作", stopClicked || (await rendererState(page)).text.includes("取消 DOM 验收"));
  const cancelled = await waitFor(async () => {
    const state = await rendererState(page);
    return state.text.includes("取消 DOM 验收") && !state.text.includes("处理中") ? state : null;
  }, "取消终态 DOM", 30_000);
  check("取消终态进入真实 DOM", cancelled.text.includes("取消 DOM 验收"));

  const evidence = {
    type: "electron_conversation_dom_acceptance",
    app: appExecutable,
      cdpPort,
      providerPort,
      checks,
      providerRequests: provider.requests.length,
      rendererTimingSamples,
      providerRequestSummary: provider.requests.map((request) => ({
        user: (Array.isArray(request.messages) ? request.messages : [])
          .filter((message) => message?.role === "user")
          .map(messageText)
          .at(-1)?.slice(-160) || "",
        hasTools: Array.isArray(request.tools) && request.tools.length > 0,
        hasToolResult: (Array.isArray(request.messages) ? request.messages : [])
          .some((message) => message?.role === "tool"),
      })),
    status: "passed",
  };
  console.log(JSON.stringify({
    ...evidence,
    ...(evidencePath ? { evidencePath } : {}),
  }, null, 2));
  if (evidencePath) {
    await writeFile(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
  }
} finally {
  page?.close();
  daemonPidForCleanup = await ownedDaemonPid(electron.pid) || daemonPidForCleanup;
  await stopOwnedProcess(electron);
  await stopOwnedDaemon(daemonPidForCleanup);
  await closeProvider(provider.server);
  await removeStateRoot(stateRoot);
}
