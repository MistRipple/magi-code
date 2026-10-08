import assert from "node:assert/strict";
import { mkdir, mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import test from "node:test";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type {
  BrowserHostCommand,
  BrowserSurfaceBinding,
  MainToWorkerMessage,
  WorkerToMainMessage,
} from "@magi/desktop-browser-contracts";
import { isAllowedBrowserChildTarget } from "@magi/desktop-browser-contracts";
import { CdpClient, type ParentPort } from "./cdp-client.js";
import { INSTALL_PAGE_RUNTIME } from "./page-script.js";
import {
  BrowserAutomationRuntime,
  LighthouseCdpSession,
} from "./runtime.js";

function assertScreenshotHeader(binary: Buffer, format: "png" | "jpeg" | "webp"): void {
  const valid = format === "png"
    ? binary.subarray(0, 8).equals(PNG_BYTES)
    : format === "jpeg"
      ? binary.subarray(0, 3).equals(JPEG_BYTES.subarray(0, 3))
      : binary.subarray(0, 4).equals(WEBP_BYTES.subarray(0, 4))
        && binary.subarray(8, 12).equals(WEBP_BYTES.subarray(8, 12));
  assert.equal(valid, true, `${format} header should be valid`);
}

class FakePort implements ParentPort {
  #listener: ((event: { data: MainToWorkerMessage }) => void) | null = null;
  readonly methods: string[] = [];
  readonly requests: Array<{ method: string; params: Record<string, unknown> }> = [];
  responseBinding: BrowserSurfaceBinding | null = null;

  on(_event: "message", listener: (event: { data: MainToWorkerMessage }) => void): void {
    this.#listener = listener;
  }

  postMessage(message: WorkerToMainMessage): void {
    if (message.type !== "cdp_request") return;
    this.methods.push(message.method);
    this.requests.push({ method: message.method, params: message.params ?? {} });
    queueMicrotask(() => {
      this.#listener?.({
        data: {
          type: "cdp_response",
          call_id: message.call_id,
          request_id: message.request_id,
          binding: this.responseBinding ?? message.binding,
          result: {},
        },
      });
    });
  }
}

class SilentPort implements ParentPort {
  readonly messages: WorkerToMainMessage[] = [];

  on(_event: "message", _listener: (event: { data: MainToWorkerMessage }) => void): void {}

  postMessage(message: WorkerToMainMessage): void {
    this.messages.push(message);
  }
}

class ScriptedPort implements ParentPort {
  #listener: ((event: { data: MainToWorkerMessage }) => void) | null = null;
  readonly requests: Array<{
    method: string;
    params: Record<string, unknown>;
    binding: BrowserSurfaceBinding;
    sessionId?: string;
  }> = [];

  constructor(
    private readonly respond: (method: string, params: Record<string, unknown>) => unknown,
    private readonly runtimeProbe: (() => boolean) | null = null,
  ) {}

  on(_event: "message", listener: (event: { data: MainToWorkerMessage }) => void): void {
    this.#listener = listener;
  }

  postMessage(message: WorkerToMainMessage): void {
    if (message.type !== "cdp_request") return;
    this.requests.push({
      method: message.method,
      params: message.params ?? {},
      binding: message.binding,
      ...(message.session_id ? { sessionId: message.session_id } : {}),
    });
    queueMicrotask(() => {
      const result = message.method === "Runtime.evaluate"
        && String(message.params?.expression ?? "").includes("typeof globalThis.__magiBrowserAutomation.setAnnotations")
        ? { result: { value: this.runtimeProbe?.() ?? true } }
        : this.respond(message.method, message.params ?? {});
      this.#listener?.({
        data: {
          type: "cdp_response",
          call_id: message.call_id,
          request_id: message.request_id,
          binding: message.binding,
          ...(result instanceof Error
            ? {
                error: {
                  code: "browser_cdp_failed",
                  message: result.message,
                  recoverable: true,
                  side_effect_started: false,
                  diagnostic: null,
                },
              }
            : { result }),
        },
      });
    });
  }

  emit(method: string, params: Record<string, unknown> = {}, eventBinding: BrowserSurfaceBinding = binding, sessionId?: string): void {
    this.#listener?.({
      data: {
        type: "cdp_event",
        binding: eventBinding,
        method,
        params,
        ...(sessionId ? { session_id: sessionId } : {}),
      },
    });
  }
}

const binding: BrowserSurfaceBinding = {
  desktop_epoch: "desktop-1",
  window_id: "window-1",
  surface_id: "surface-1",
  surface_revision: 1,
  tab_id: "tab-1",
  web_contents_id: 10,
  target_id: "target-1",
  browser_context_id: "magi-browser-session-1",
  navigation_revision: 1,
};

const PNG_BYTES = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const JPEG_BYTES = Buffer.from([0xff, 0xd8, 0xff, 0xd9]);
const WEBP_BYTES = Buffer.from("RIFF....WEBP", "ascii");

function consoleCommand(): BrowserHostCommand {
  return {
    type: "devtools",
    payload: {
      tab_id: "tab-1",
      operation: "console",
      arguments: { action: "list" },
    },
  };
}

interface WebScriptResponses {
  probe?: Record<string, unknown>;
  write?: Record<string, unknown>;
  observe?: Record<string, unknown>;
  submit?: Record<string, unknown>;
  turnState?: Record<string, unknown>;
  cancel?: boolean;
  origin?: string;
  savedConversations?: Record<string, unknown>;
  savedMessages?: Record<string, unknown>;
  connectorStatus?: Record<string, unknown>;
  configureConnector?: Record<string, unknown>;
}

function webScriptedPort(responses: WebScriptResponses): ScriptedPort {
  let adapterInstalled = false;
  let observeCalls = 0;
  return new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") {
      adapterInstalled = false;
      return { executionContextId: 1 };
    }
    if (method !== "Runtime.evaluate") return {};
    const expression = String(params.expression ?? "");
    if (expression.includes("globalThis.__magiWebModel =")) {
      adapterInstalled = true;
      return { result: { value: null } };
    }
    if (expression === "location.origin") {
      return { result: { value: responses.origin ?? "https://chatgpt.com" } };
    }
    if (expression.includes("globalThis.__magiWebModel") && expression.includes("adapter_epoch")) {
      return { result: { value: adapterInstalled } };
    }
    if (expression.includes("globalThis.__magiWebModel.probe()")) {
      return { result: { value: responses.probe ?? null } };
    }
    if (expression.includes("globalThis.__magiWebModel.writeText(")) {
      return { result: { value: responses.write ?? null } };
    }
    if (expression.includes("globalThis.__magiWebModel.observe(")) {
      observeCalls += 1;
      const observed = responses.observe ?? { nodes: [{ found: false }] };
      return {
        result: {
          value: {
            ...observed,
            revision: observed.revision === undefined ? observeCalls : Number(observed.revision),
          },
        },
      };
    }
    if (expression.includes("globalThis.__magiWebModel.submit()")) {
      return { result: { value: responses.submit ?? null } };
    }
    if (expression.includes("globalThis.__magiWebModel.turnState()")) {
      return { result: { value: responses.turnState ?? null } };
    }
    if (expression.includes("globalThis.__magiWebModel.cancelGeneration()")) {
      return { result: { value: responses.cancel ?? false } };
    }
    if (expression.includes("globalThis.__magiWebModel.savedConversations()")) {
      return { result: { value: responses.savedConversations ?? null } };
    }
    if (expression.includes("globalThis.__magiWebModel.savedMessages()")) {
      return { result: { value: responses.savedMessages ?? null } };
    }
    if (expression.includes("globalThis.__magiWebModel.connectorStatus(")) {
      return { result: { value: responses.connectorStatus ?? null } };
    }
    if (expression.includes("globalThis.__magiWebModel.configureConnector(")) {
      return { result: { value: responses.configureConnector ?? null } };
    }
    return { result: { value: null } };
  });
}

function webCommand(type: BrowserHostCommand["type"], payload: Record<string, unknown>): BrowserHostCommand {
  return { type, payload } as BrowserHostCommand;
}

test("Lighthouse 只附着 iframe/Worker，页面型和未知 Target 永远不会产生子会话", async () => {
  assert.equal(isAllowedBrowserChildTarget(undefined), false);
  assert.equal(isAllowedBrowserChildTarget({}), false);
  assert.equal(isAllowedBrowserChildTarget({ type: "page" }), false);
  assert.equal(isAllowedBrowserChildTarget({ type: "webview" }), false);
  assert.equal(isAllowedBrowserChildTarget({ type: "unknown" }), false);
  assert.equal(isAllowedBrowserChildTarget({ type: "iframe" }), true);
  assert.equal(isAllowedBrowserChildTarget({ type: "worker" }), true);
  assert.equal(isAllowedBrowserChildTarget({ type: "service_worker" }), true);
  assert.equal(isAllowedBrowserChildTarget({ type: "shared_worker" }), true);

  const port = new ScriptedPort(() => ({}));
  const cdp = new CdpClient(port);
  const root = new LighthouseCdpSession(cdp, binding);
  const attached: LighthouseCdpSession[] = [];
  root.on("sessionattached", (child) => attached.push(child as LighthouseCdpSession));

  port.emit("Target.attachedToTarget", {
    sessionId: "page-session",
    targetInfo: { targetId: "page-target", type: "page" },
  });
  port.emit("Target.attachedToTarget", {
    sessionId: "missing-type-session",
    targetInfo: { targetId: "unknown-target" },
  });
  port.emit("Target.attachedToTarget", {
    sessionId: "iframe-session",
    targetInfo: { targetId: "iframe-target", type: "iframe" },
  });

  assert.equal(attached.length, 1);
  assert.equal(attached[0]?.id(), "iframe-session");
  await root.detach();

  const workerPort = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(workerPort), "worker-test");
  assert.equal((await runtime.execute("child-target-init", binding, consoleCommand())).outcome.status, "succeeded");
  workerPort.emit("Target.attachedToTarget", {
    sessionId: "blocked-page-session",
    targetInfo: { targetId: "blocked-page-target", type: "page" },
  });
  workerPort.emit("Runtime.consoleAPICalled", { type: "error", args: [{ value: "must-not-leak" }] }, binding, "blocked-page-session");
  const consoleResult = await runtime.execute("child-target-console", binding, consoleCommand());
  assert.equal(consoleResult.outcome.status, "succeeded");
  const consoleValue = consoleResult.outcome.payload.type === "json"
    ? consoleResult.outcome.payload.payload.value as { entries?: Array<Record<string, unknown>> }
    : null;
  assert.deepEqual(consoleValue?.entries, []);
});

test("Worker 为每个 CDP Surface 显式启用页面、运行时和网络事件域", async () => {
  const port = new FakePort();
  const runtime = new BrowserAutomationRuntime(new CdpClient(port));

  const first = await runtime.execute("call-1", binding, consoleCommand());
  assert.equal(first.outcome.status, "succeeded");
  assert.deepEqual(new Set(port.methods), new Set(["Page.enable", "Runtime.enable", "Network.enable"]));

  await runtime.execute("call-2", binding, consoleCommand());
  assert.equal(port.methods.length, 3, "同一 Surface 不应为每次工具调用重复启用 CDP 域");
});

test("域启用握手在导航竞态里被拒绝后，不会把失败永久留在新代次的页面状态上", async () => {
  class HoldingPort implements ParentPort {
    #listener: ((event: { data: MainToWorkerMessage }) => void) | null = null;
    held: Array<Extract<WorkerToMainMessage, { type: "cdp_request" }>> | null = [];

    on(_event: "message", listener: (event: { data: MainToWorkerMessage }) => void): void {
      this.#listener = listener;
    }

    postMessage(message: WorkerToMainMessage): void {
      if (message.type !== "cdp_request") return;
      if (this.held) {
        this.held.push(message);
        return;
      }
      this.reply(message, null);
    }

    reply(message: Extract<WorkerToMainMessage, { type: "cdp_request" }>, error: string | null): void {
      queueMicrotask(() => {
        this.#listener?.({
          data: {
            type: "cdp_response",
            call_id: message.call_id,
            request_id: message.request_id,
            binding: message.binding,
            ...(error
              ? {
                  error: {
                    code: "browser_surface_stale",
                    message: error,
                    recoverable: true,
                    side_effect_started: false,
                    diagnostic: null,
                  },
                }
              : { result: {} }),
          },
        });
      });
    }
  }

  const port = new HoldingPort();
  const runtime = new BrowserAutomationRuntime(new CdpClient(port));
  const advanced = { ...binding, navigation_revision: binding.navigation_revision + 1 };

  // 握手还在进行时，页面又前进了一个代次：标记投影走独立的调度链，会为新代次重建页面状态
  // 并继承这个在途握手。
  const first = runtime.execute("call-1", binding, consoleCommand());
  await new Promise((resolve) => setImmediate(resolve));
  const second = runtime.execute("call-2", advanced, {
    type: "set_annotations",
    payload: { tab_id: binding.tab_id, annotations: [] },
  });
  await new Promise((resolve) => setImmediate(resolve));

  const held = port.held ?? [];
  port.held = null;
  assert.equal(held.length, 3);
  for (const request of held) port.reply(request, "browser_surface_stale");
  await Promise.all([first, second]);

  // 之后用最新代次重试必须重新握手并成功，而不是立刻重复同一个 browser_surface_stale。
  const third = await runtime.execute("call-3", advanced, consoleCommand());
  assert.equal(third.outcome.status, "succeeded");
});

test("不同 Browser Surface 使用独立的页面运行态和 CDP binding，不串用 Tab 数据", async () => {
  const tabA = { ...binding, surface_id: "surface-a", tab_id: "tab-a", target_id: "target-a" };
  const tabB = { ...binding, surface_id: "surface-b", tab_id: "tab-b", target_id: "target-b" };
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const consoleCommandFor = (target: BrowserSurfaceBinding): BrowserHostCommand => ({
    type: "devtools",
    payload: {
      tab_id: target.tab_id,
      operation: "console",
      arguments: { action: "list" },
    },
  });

  assert.equal((await runtime.execute("init-a", tabA, consoleCommandFor(tabA))).outcome.status, "succeeded");
  assert.equal((await runtime.execute("init-b", tabB, consoleCommandFor(tabB))).outcome.status, "succeeded");

  port.emit("Runtime.consoleAPICalled", { type: "log", args: [{ value: "only-a" }] }, tabA);
  port.emit("Runtime.consoleAPICalled", { type: "error", args: [{ value: "only-b" }] }, tabB);

  const resultA = await runtime.execute("read-a", tabA, consoleCommandFor(tabA));
  const resultB = await runtime.execute("read-b", tabB, consoleCommandFor(tabB));
  assert.equal(resultA.outcome.status, "succeeded");
  assert.equal(resultB.outcome.status, "succeeded");

  const valueA = resultA.outcome.payload.type === "json"
    ? resultA.outcome.payload.payload.value as { entries?: Array<Record<string, unknown>> }
    : null;
  const valueB = resultB.outcome.payload.type === "json"
    ? resultB.outcome.payload.payload.value as { entries?: Array<Record<string, unknown>> }
    : null;
  assert.deepEqual(valueA?.entries?.map((entry) => entry.type), ["log"]);
  assert.deepEqual(valueA?.entries?.map((entry) => (entry.args as Array<{ value?: string }>)?.[0]?.value), ["only-a"]);
  assert.deepEqual(valueB?.entries?.map((entry) => entry.type), ["error"]);
  assert.deepEqual(valueB?.entries?.map((entry) => (entry.args as Array<{ value?: string }>)?.[0]?.value), ["only-b"]);

  const domainRequests = port.requests.filter((request) => ["Page.enable", "Runtime.enable", "Network.enable"].includes(request.method));
  assert.deepEqual(
    new Set(domainRequests.filter((request) => request.binding.surface_id === tabA.surface_id).map((request) => request.method)),
    new Set(["Page.enable", "Runtime.enable", "Network.enable"]),
  );
  assert.deepEqual(
    new Set(domainRequests.filter((request) => request.binding.surface_id === tabB.surface_id).map((request) => request.method)),
    new Set(["Page.enable", "Runtime.enable", "Network.enable"]),
  );
  assert.ok(domainRequests.every((request) => request.binding.tab_id === (request.binding.surface_id === tabA.surface_id ? tabA.tab_id : tabB.tab_id)));
});

test("CDP 响应的完整 Surface 身份变化必须被拒绝", async () => {
  const port = new FakePort();
  port.responseBinding = { ...binding, navigation_revision: binding.navigation_revision + 1 };
  const client = new CdpClient(port);

  await assert.rejects(
    client.send(binding, "Runtime.enable"),
    /browser_surface_stale/u,
  );
});

test("CDP 请求超时会通知 Main 取消底层请求，并保留超时错误语义", async () => {
  const port = new SilentPort();
  const client = new CdpClient(port);

  await assert.rejects(
    client.send(binding, "Runtime.enable", {}, 5),
    /browser_cdp_timeout:Runtime\.enable/u,
  );
  assert.deepEqual(
    port.messages.map((message) => message.type),
    ["cdp_request", "cdp_cancel"],
  );
});

test("浏览器按键使用 Chromium 原生 key 事件类型和键码", async () => {
  const port = new FakePort();
  const runtime = new BrowserAutomationRuntime(new CdpClient(port));
  const result = await runtime.execute("press-enter", binding, {
    type: "press",
    payload: {
      tab_id: binding.tab_id,
      control: { mode: "agent", lease_id: "lease-1", fence: 1 },
      key: "Enter",
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  const keyEvents = port.methods.filter((method) => method === "Input.dispatchKeyEvent");
  assert.equal(keyEvents.length, 2);
  const inputRequests = port.requests.filter((request) => request.method === "Input.dispatchKeyEvent");
  assert.equal(inputRequests[0]?.params.type, "rawKeyDown");
  assert.equal(inputRequests[0]?.params.key, "Enter");
  assert.equal(inputRequests[0]?.params.code, "Enter");
  assert.equal(inputRequests[0]?.params.windowsVirtualKeyCode, 13);
  assert.equal(inputRequests[1]?.params.type, "keyUp");
});

test("扩展浏览器能力已经进入 Worker 执行链", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      return { result: { value: String(params.expression).includes("location.origin") ? "https://example.test" : null } };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port));
  const result = await runtime.execute("third-party-call", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "third_party",
      arguments: { action: "list" },
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  assert.ok(port.requests.some((request) => request.method === "Runtime.evaluate"));
});

test("第三方资源工具只接受 list 和 clear action", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port));
  const result = await runtime.execute("third-party-invalid", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "third_party",
      arguments: { action: "execute" },
    },
  });
  assert.equal(result.outcome.status, "failed");
  assert.equal(result.outcome.payload.code, "browser_third_party_action_unsupported");
  assert.equal(port.requests.some((request) => request.method === "Runtime.evaluate"), false);
});

test("PWA 工具只接受 state action", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port));
  const result = await runtime.execute("pwa-invalid", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "pwa",
      arguments: { action: "install" },
    },
  });
  assert.equal(result.outcome.status, "failed");
  assert.equal(result.outcome.payload.code, "browser_pwa_action_unsupported");
});

test("wait_for 超时会返回可恢复的具体等待条件", async () => {
  const port = new ScriptedPort((method) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") return { result: { value: false } };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("wait-timeout", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "wait_for",
      arguments: { text: "敌人 5", timeout_ms: 0 },
    },
  });

  assert.equal(result.outcome.status, "failed");
  assert.equal(result.outcome.payload.code, "browser_wait_timeout");
  assert.match(result.outcome.payload.message, /敌人 5/u);
  assert.match(result.outcome.payload.message, /browser_snapshot/u);
});

test("WebMCP 执行工具时传递结构化输入而不是 JSON 字符串", async () => {
  let executeExpression = "";
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.includes("executeTool(tool")) {
        executeExpression = expression;
        return { result: { value: { ok: true } } };
      }
      return { result: { value: null } };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("webmcp-execute", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "webmcp",
      arguments: { action: "execute", tool_name: "search", input: { query: "magi" } },
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.match(executeExpression, /executeTool\(tool, \{"query":"magi"\}\)/u);
  assert.equal(executeExpression.includes('executeTool(tool, "'), false);
});

test("性能 Trace 停止时等待 tracingComplete 并返回采集事件", async () => {
  const port = new ScriptedPort((method) => {
    if (method === "Tracing.end") {
      queueMicrotask(() => port.emit("Tracing.dataCollected", { value: [{ name: "firstContentfulPaint" }] }));
      queueMicrotask(() => port.emit("Tracing.tracingComplete"));
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const start = await runtime.execute("trace-start", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "performance", arguments: { action: "start" } },
  });
  assert.equal(start.outcome.status, "succeeded");
  const stop = await runtime.execute("trace-stop", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "performance", arguments: { action: "stop" } },
  });
  assert.equal(stop.outcome.status, "succeeded");
  const value = stop.outcome.payload.type === "json" ? stop.outcome.payload.payload.value as { events?: Array<Record<string, unknown>> } : null;
  assert.deepEqual(value?.events, [{ name: "firstContentfulPaint" }]);
});

test("浏览器截图的归一化区域必须转换为当前滚动页面的绝对裁剪区域", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") {
      return { frameTree: { frame: { id: "frame-1" } } };
    }
    if (method === "Page.createIsolatedWorld") {
      return { executionContextId: 1 };
    }
    if (method === "Page.getLayoutMetrics") {
      return { layoutViewport: { clientWidth: 1200, clientHeight: 800 } };
    }
    if (method === "Runtime.evaluate") {
      return {
        result: {
          value: String(params.expression).trim().endsWith("globalThis.__magiBrowserAutomation.viewport()")
            ? { width: 1200, height: 800, scrollX: 40, scrollY: 600 }
            : null,
        },
      };
    }
    if (method === "Page.captureScreenshot") {
      return { data: PNG_BYTES.toString("base64") };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const result = await runtime.execute("clip-call", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      clip: { x: 0.25, y: 0.1, width: 0.5, height: 0.25 },
      full_page: false,
      format: "png",
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.equal(result.binary_base64, PNG_BYTES.toString("base64"));
  const capture = port.requests.find((request) => request.method === "Page.captureScreenshot");
  assert.deepEqual(capture?.params, {
    format: "png",
    clip: { x: 340, y: 680, width: 600, height: 200, scale: 1 },
    captureBeyondViewport: false,
    fromSurface: true,
  });
});

test("浏览器截图拒绝跨导航代次复用旧请求", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("stale-screenshot", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision + 1,
      full_page: false,
      format: "png",
    },
  });
  assert.equal(result.outcome.status, "failed");
  if (result.outcome.status === "failed") {
    assert.equal(result.outcome.payload.code, "browser_navigation_revision_stale");
  }
  assert.equal(
    port.requests.some((request) => request.method === "Page.captureScreenshot"),
    false,
  );
});

test("截图使用页面脚本视口坐标，滚动直接在当前文档执行", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") {
      return { frameTree: { frame: { id: "frame-1" } } };
    }
    if (method === "Page.createIsolatedWorld") {
      return { executionContextId: 1 };
    }
    if (method === "Page.getLayoutMetrics") {
      return { layoutViewport: { clientWidth: 960, clientHeight: 1708 } };
    }
    if (method === "Runtime.evaluate") {
      return {
        result: {
          value: String(params.expression).trim().endsWith("globalThis.__magiBrowserAutomation.viewport()")
            ? { width: 480, height: 854 }
            : null,
        },
      };
    }
    if (method === "Page.captureScreenshot") {
      return { data: PNG_BYTES.toString("base64") };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const screenshot = await runtime.execute("viewport-clip-call", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      clip: { x: 0.1, y: 0.1, width: 0.2, height: 0.2 },
      full_page: false,
      format: "png",
    },
  });

  assert.equal(screenshot.outcome.status, "succeeded");
  assert.deepEqual(
    port.requests.find((request) => request.method === "Page.captureScreenshot")?.params,
    {
      format: "png",
      clip: { x: 48, y: 85.4, width: 96, height: 170.8, scale: 1 },
      captureBeyondViewport: false,
      fromSurface: true,
    },
  );

  await runtime.execute("viewport-scroll-call", binding, {
    type: "scroll",
    payload: {
      tab_id: binding.tab_id,
      control: { mode: "user", fence: 1 },
      delta_x: 0,
      delta_y: 400,
    },
  });
  assert.equal(
    port.requests.some((request) => request.method === "Input.dispatchMouseEvent"),
    false,
  );
  assert.match(
    String(port.requests.find((request) => request.method === "Runtime.evaluate" && String(request.params.expression).includes("window.scrollBy"))?.params.expression),
    /window\.scrollBy/,
  );
});

test("页面脚本视口失效时兼容 CDP 仅返回 clientWidth/clientHeight 的布局视口", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") {
      return { frameTree: { frame: { id: "frame-1" } } };
    }
    if (method === "Page.createIsolatedWorld") {
      return { executionContextId: 1 };
    }
    if (method === "Page.getLayoutMetrics") {
      return {
        cssVisualViewport: { width: 0, height: 0, clientWidth: 480, clientHeight: 854 },
      };
    }
    if (method === "Runtime.evaluate") {
      return {
        result: {
          value: String(params.expression).trim().endsWith("globalThis.__magiBrowserAutomation.viewport()")
            ? { width: 0, height: 0 }
            : null,
        },
      };
    }
    if (method === "Page.captureScreenshot") {
      return { data: PNG_BYTES.toString("base64") };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const result = await runtime.execute("client-dimension-viewport", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      clip: { x: 0, y: 0, width: 0.5, height: 0.5 },
      full_page: false,
      format: "png",
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.deepEqual(
    port.requests.find((request) => request.method === "Page.captureScreenshot")?.params,
    {
      format: "png",
      clip: { x: 0, y: 0, width: 240, height: 427, scale: 1 },
      captureBeyondViewport: false,
      fromSurface: true,
    },
  );
});

test("滚动目标元素时直接调用元素 scrollBy，不依赖鼠标坐标", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") return { result: { value: null } };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("scroll-target", binding, {
    type: "scroll",
    payload: {
      tab_id: binding.tab_id,
      control: { mode: "user", fence: 1 },
      target: { element_ref: "e:1:1" },
      delta_x: 0,
      delta_y: 300,
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  assert.equal(
    port.requests.some((request) => request.method === "Input.dispatchMouseEvent"),
    false,
  );
  const scroll = port.requests.find((request) => request.method === "Runtime.evaluate" && String(request.params.expression).includes("scrollBy"));
  assert.ok(scroll);
  assert.match(String(scroll.params.expression), /__magiBrowserAutomation\.resolve/);
  assert.match(String(scroll.params.expression), /left: 0/);
  assert.match(String(scroll.params.expression), /top: 300/);
});

test("页面滚动不再经过会超时的 Input.dispatchMouseEvent CDP 通道", async () => {
  const port = new ScriptedPort((method) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Input.dispatchMouseEvent") {
      return new Error("browser_cdp_timeout:Input.dispatchMouseEvent");
    }
    return { result: { value: null } };
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const result = await runtime.execute("scroll-without-input-cdp", binding, {
    type: "scroll",
    payload: {
      tab_id: binding.tab_id,
      control: { mode: "user", fence: 1 },
      delta_x: 0,
      delta_y: 400,
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.equal(
    port.requests.some((request) => request.method === "Input.dispatchMouseEvent"),
    false,
  );
  assert.ok(port.requests.some((request) => request.method === "Runtime.evaluate" && String(request.params.expression).includes("window.scrollBy")));
});

test("click_at 的 double_click 产生完整双击序列", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("double-click", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "click_at", arguments: { x: 20, y: 30, double_click: true } },
  });
  assert.equal(result.outcome.status, "succeeded");
  assert.deepEqual(
    port.requests.filter((request) => request.method === "Input.dispatchMouseEvent").map((request) => request.params.clickCount),
    [undefined, 1, 1, 2, 2],
  );
});

test("click_at 按下触发导航后不向新文档发送释放事件", async () => {
  const nextBinding = { ...binding, navigation_revision: binding.navigation_revision + 1 };
  const port = new ScriptedPort((method, params) => {
    if (method === "Input.dispatchMouseEvent" && params.type === "mousePressed") {
      port.emit("Page.frameStartedLoading", {}, nextBinding);
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const result = await runtime.execute("click-at-navigation", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "click_at", arguments: { x: 20, y: 30 } },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.deepEqual(
    port.requests
      .filter((request) => request.method === "Input.dispatchMouseEvent")
      .map((request) => request.params.type),
    ["mouseMoved", "mousePressed"],
  );
});

test("文本插入触发导航后不再向新文档提交旧按键", async () => {
  const nextBinding = { ...binding, navigation_revision: binding.navigation_revision + 1 };
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      const target = { x:10, y:20, bounds:{x:0,y:0,width:20,height:20}, editable:true, sensitive:null };
      return { result: { value: expression.includes("prepareText") ? {target, expected:"magi"} : {...target, observed:true} } };
    }
    if (method === "Input.insertText") {
      port.emit("Page.frameStartedLoading", {}, nextBinding);
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const result = await runtime.execute("type-navigation", binding, {
    type: "type",
    payload: {
      tab_id: binding.tab_id,
      control: { mode: "user", fence: 1 },
      target: { element_ref: "e:1:search" },
      text: "magi",
      replace: false,
      submit_key: "Enter",
    },
  });

  assert.equal(result.outcome.status, "indeterminate");
  assert.equal(
    port.requests.slice(port.requests.findIndex((request) => request.method === "Input.insertText") + 1)
      .some((request) => request.method.startsWith("Input.")),
    false,
  );
});

test("鼠标按下触发对话框时返回结果未确认，不补点且保留对话框状态", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") return { result: { value: { x: 10, y: 20, bounds: { x: 0, y: 0, width: 20, height: 20 }, editable: false, sensitive: null } } };
    if (method === "Input.dispatchMouseEvent" && params.type === "mousePressed") {
      port.emit("Page.javascriptDialogOpening", { type: "alert", message: "magi-dialog" });
      return new Error("browser_cdp_timeout:Input.dispatchMouseEvent");
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("click-dialog", binding, {
    type: "click",
    payload: {
      tab_id: binding.tab_id,
      control: { mode: "user", fence: 1 },
      target: { element_ref: "e:1:dialog" },
    },
  });
  assert.equal(result.outcome.status, "indeterminate");
  assert.equal(port.requests.filter((request) => request.method === "Input.dispatchMouseEvent"
    && request.params.type === "mousePressed").length, 1);
  const dialog = await runtime.execute("dialog-after-click", binding, {
    type: "devtools", payload: { tab_id: binding.tab_id, operation: "dialog", arguments: { action: "list" } },
  });
  assert.equal(dialog.outcome.status, "succeeded");
  assert.ok(JSON.stringify(dialog.outcome).includes("magi-dialog"));
});

test("原生鼠标事件未确认时返回 indeterminate 且绝不补点", async () => {
  const port = new ScriptedPort((method) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") return { result: { value: { x: 10, y: 20, bounds: { x: 0, y: 0, width: 20, height: 20 }, editable: false, sensitive: null, role: "button", name: "  提交\n  订单 " } } };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("click-fallback", binding, {
    type: "click",
    payload: {
      tab_id: binding.tab_id,
      control: { mode: "user", fence: 1 },
      target: { element_ref: "e:1:button" },
    },
  });
  assert.equal(result.outcome.status, "indeterminate");
  assert.equal(port.requests.some((request) => String(request.params.expression).includes("fallbackClick")), false);
  assert.equal(port.requests.filter((request) => request.method === "Input.dispatchMouseEvent" && request.params.type === "mousePressed").length, 1);
});

test("输入回读缺失或不匹配时不提交、不补写，清空同样必须确认", async () => {
  for (const text of ["new", ""]) {
    for (const applied of [false, undefined, true]) {
      const port = new ScriptedPort((method, params) => {
        if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
        if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
        if (method !== "Runtime.evaluate") return {};
        const expression = String(params.expression);
        if (expression.includes(".verifyText(")) return { result: { value: applied === undefined ? null : { applied } } };
        if (expression.includes(".finishClick(")) return { result: { value: { observed: true } } };
        const target = { x: 10, y: 20, bounds: { x: 0, y: 0, width: 20, height: 20 }, editable: true, sensitive: null };
        return { result: { value: expression.includes(".prepareText(") ? { target, expected: text } : target } };
      });
      const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
      const result = await runtime.execute("verify-input", binding, {
        type: "type", payload: {
          tab_id: binding.tab_id, control: { mode: "user", fence: 1 },
          target: { element_ref: "e:1:input" }, text, replace: true, submit_key: "Enter",
        },
      });
      assert.equal(result.outcome.status, applied === true ? "succeeded" : "indeterminate");
      assert.equal(port.requests.some((request) => request.method === "Input.dispatchKeyEvent" && request.params.key === "Enter"), applied === true);
      assert.equal(port.requests.filter((request) => request.method === "Input.insertText").length, text ? 1 : 0);
      assert.equal(port.requests.some((request) => String(request.params.expression).includes("fillValue")), false);
    }
  }
});

test("点击完成后异步到达的 JavaScript 对话框事件仍可被下一次 list 读取", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") return { result: { value: { x: 10, y: 20, bounds: { x: 0, y: 0, width: 20, height: 20 }, editable: false, sensitive: null, observed:true } } };
    if (method === "Input.dispatchMouseEvent" && params.type === "mouseReleased") {
      setTimeout(() => port.emit("Page.javascriptDialogOpening", { type: "alert", message: "magi-dialog" }), 500);
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const clicked = await runtime.execute("click-dialog-race", binding, {
    type: "click",
    payload: {
      tab_id: binding.tab_id,
      control: { mode: "user", fence: 1 },
      target: { element_ref: "e:1:dialog" },
    },
  });
  assert.equal(clicked.outcome.status, "succeeded");

  const listed = await runtime.execute("dialog-race-list", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "dialog", arguments: { action: "list" } },
  });
  assert.equal(listed.outcome.status, "succeeded");
  const value = listed.outcome.payload.type === "json" ? listed.outcome.payload.payload.value as Record<string, any> : null;
  assert.deepEqual(value?.dialog, { type: "alert", message: "magi-dialog" });
});

test("drag 使用完整 HTML DragEvent 生命周期，而不是只发送一次鼠标移动", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.includes("new DragEvent") || expression.includes("dragstart")) {
        return { result: { value: { dragged: true, drag_over_accepted: true, drop_accepted: true } } };
      }
      return { result: { value: null } };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("drag-call", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "drag",
      arguments: {
        source: { element_ref: "e:1:source" },
        target: { element_ref: "e:1:target" },
      },
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  assert.equal(
    port.requests.filter((request) => request.method === "Input.dispatchMouseEvent").length,
    0,
    "拖拽不应退化为不完整的鼠标事件序列",
  );
  assert.ok(port.requests.some((request) => String(request.params.expression).includes("new DragEvent")));
});

test("对话框事件即使来自 CDP 子会话也能列出并使用同一会话处理", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  runtime.rebind([binding]);
  port.emit("Page.javascriptDialogOpening", { type: "alert", message: "需要确认" }, binding, "dialog-session");

  const listed = await runtime.execute("dialog-list", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "dialog", arguments: { action: "list" } },
  });
  assert.equal(listed.outcome.status, "succeeded");
  const listedValue = listed.outcome.payload.type === "json" ? listed.outcome.payload.payload.value as Record<string, unknown> : null;
  assert.deepEqual(listedValue?.dialog, { type: "alert", message: "需要确认" });

  const handled = await runtime.execute("dialog-accept", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "dialog", arguments: { action: "accept" } },
  });
  assert.equal(handled.outcome.status, "succeeded");
  assert.equal(
    port.requests.find((request) => request.method === "Page.handleJavaScriptDialog")?.sessionId,
    "dialog-session",
  );
});

test("非阻塞页面对话框桥接可列出并通过 Runtime.evaluate 收口", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  runtime.rebind([binding]);
  port.emit("Runtime.bindingCalled", {
    name: "__magiBrowserDialog",
    payload: JSON.stringify({
      event: "opening",
      id: "magi-dialog-1",
      type: "alert",
      message: "magi-dialog",
      defaultPrompt: null,
    }),
  });

  const listed = await runtime.execute("virtual-dialog-list", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "dialog", arguments: { action: "list" } },
  });
  const listedValue = listed.outcome.status === "succeeded" && listed.outcome.payload.type === "json"
    ? listed.outcome.payload.payload.value as Record<string, unknown>
    : null;
  assert.deepEqual(listedValue?.dialog, {
    event: "opening",
    id: "magi-dialog-1",
    type: "alert",
    message: "magi-dialog",
    defaultPrompt: null,
    virtual: true,
  });

  const handled = await runtime.execute("virtual-dialog-accept", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "dialog", arguments: { action: "accept" } },
  });
  assert.equal(handled.outcome.status, "succeeded");
  assert.equal(
    port.requests.some((request) => request.method === "Runtime.evaluate"
      && String(request.params.expression).includes("__magiBrowserDialogResolve")),
    true,
  );
});

test("网络响应体不可读时保留网络记录并返回结构化不可用原因", async () => {
  const port = new ScriptedPort((method) => (
    method === "Network.getResponseBody" ? new Error("No data found for resource with given identifier") : {}
  ));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  runtime.rebind([binding]);
  port.emit("Network.responseReceived", { requestId: "response-1", type: "Fetch", response: { url: "https://example.test/data" } });

  const result = await runtime.execute("network-body", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "network",
      arguments: { action: "get", request_id: "response-1", include_body: true },
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  const value = result.outcome.payload.type === "json" ? result.outcome.payload.payload.value as Record<string, any> : null;
  assert.equal(value?.entry?.requestId, "response-1");
  assert.equal(value?.body, null);
  assert.equal(value?.body_unavailable?.code, "browser_network_body_unavailable");
});

test("fill_form 按原生控件语义处理 select、checkbox 和 radio", async () => {
  let clickCount = 0;
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.includes("e:1:radio") && expression.startsWith("globalThis.__magiBrowserAutomation.formControl")) return { result: { value: { kind: "radio" } } };
      if (expression.includes("e:1:checkbox") && expression.startsWith("globalThis.__magiBrowserAutomation.formControl")) return { result: { value: { kind: "checkbox" } } };
      if (expression.includes("e:1:select") && expression.startsWith("globalThis.__magiBrowserAutomation.formControl")) return { result: { value: { kind: "select", multiple: true } } };
      if (expression.includes("selectedOptions")) return { result: { value: { applied: true } } };
      if (expression.includes("prepareClick")) return { result:{ value:{x:10,y:20,role:"checkbox",name:"test"} } };
      if (expression.includes("finishClick")) return { result:{value:{observed:true}} };
      if (expression.endsWith(".checked")) return { result:{value:clickCount > 0} };
      return { result: { value: null } };
    }
    if (method === "Input.dispatchMouseEvent" && params.type === "mousePressed") clickCount++;
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("fill-controls", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "fill_form",
      arguments: {
        fields: [
          { element_ref: "e:1:select", value: ["one", "two"] },
          { element_ref: "e:1:checkbox", value: true },
          { element_ref: "e:1:radio", value: true },
        ],
      },
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  assert.equal(result.outcome.payload.type, "json");
  const fillResult = result.outcome.payload.type === "json"
    ? result.outcome.payload.payload.value as { filled?: number }
    : null;
  assert.equal(fillResult?.filled, 3);
  const evaluateCalls = port.requests.filter((request) => request.method === "Runtime.evaluate");
  assert.ok(evaluateCalls.some((request) => String(request.params.expression).includes("element.options")));
  assert.equal(clickCount, 1);
  assert.ok(!evaluateCalls.some((request) => String(request.params.expression).includes("element.click()")));
});

test("性能和堆工具拒绝未实现 action，而不是返回空结果", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const performance = await runtime.execute("bad-performance", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "performance", arguments: { action: "insight" } },
  });
  const heap = await runtime.execute("bad-heap", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "heap", arguments: { action: "query_objects" } },
  });
  assert.equal(performance.outcome.status, "failed");
  assert.equal(performance.outcome.payload.code, "browser_performance_action_unsupported");
  assert.equal(heap.outcome.status, "failed");
  assert.equal(heap.outcome.payload.code, "browser_heap_action_unsupported");
});

test("堆工具要求显式 action，不使用未声明的默认 action", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("heap-missing-action", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "heap", arguments: {} },
  });
  assert.equal(result.outcome.status, "failed");
  assert.equal(result.outcome.payload.code, "browser_heap_action_required");
});

test("文件上传支持单文件和多文件 file input", async () => {
  const uploadRoot = await mkdtemp(join(tmpdir(), "magi-browser-upload-"));
  const firstPath = join(uploadRoot, "first.txt");
  const secondPath = join(uploadRoot, "second.txt");
  await writeFile(firstPath, "first");
  await writeFile(secondPath, "second");
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.includes("__magiBrowserAutomation.target")) {
        return { result: { value: { x: 30, y: 40, bounds: { x: 10, y: 20, width: 40, height: 40 }, editable: true, sensitive: null } } };
      }
      if (expression.includes("__magiBrowserAutomation.resolve")) return { result: { value: "input[type=file]" } };
      return { result: { value: null } };
    }
    if (method === "DOM.getDocument") return { root: { nodeId: 1 } };
    if (method === "DOM.querySelector") return { nodeId: 2 };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("upload-files", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "upload_file",
      arguments: {
        element_ref: "e:1:1",
        file_paths: [firstPath, secondPath],
      },
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  assert.deepEqual(
    port.requests.find((request) => request.method === "DOM.setFileInputFiles")?.params,
    { nodeId: 2, files: [await realpath(firstPath), await realpath(secondPath)] },
  );
  await rm(uploadRoot, { recursive: true, force: true });
});

test("文件上传拒绝无法解析为 file input 的快照目标", async () => {
  const uploadRoot = await mkdtemp(join(tmpdir(), "magi-browser-upload-"));
  const filePath = join(uploadRoot, "first.txt");
  await writeFile(filePath, "first");
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.includes("__magiBrowserAutomation.target")) {
        return { result: { value: { x: 30, y: 40, bounds: { x: 10, y: 20, width: 40, height: 40 }, editable: true, sensitive: null } } };
      }
      if (expression.includes("__magiBrowserAutomation.resolve")) return { result: { value: null } };
      return { result: { value: null } };
    }
    if (method === "DOM.getDocument") return { root: { nodeId: 1 } };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("upload-text-input", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "upload_file",
      arguments: { element_ref: "e:1:1", file_path: filePath },
    },
  });
  assert.equal(result.outcome.status, "failed");
  assert.equal(result.outcome.payload.code, "browser_upload_target_invalid");
  await rm(uploadRoot, { recursive: true, force: true });
});

test("文件上传只接受真实存在的绝对路径普通文件", async () => {
  const root = await mkdtemp(join(tmpdir(), "magi-browser-upload-invalid-"));
  const directoryPath = join(root, "dir");
  await mkdir(directoryPath);
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.includes("__magiBrowserAutomation.target")) {
        return { result: { value: { x: 30, y: 40, bounds: { x: 10, y: 20, width: 40, height: 40 }, editable: true, sensitive: null } } };
      }
      if (expression.includes("__magiBrowserAutomation.resolve")) return { result: { value: "input[type=file]" } };
      return { result: { value: null } };
    }
    if (method === "DOM.getDocument") return { root: { nodeId: 1 } };
    if (method === "DOM.querySelector") return { nodeId: 2 };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  for (const [name, filePath] of [
    ["relative", "relative/first.txt"],
    ["missing", join(root, "missing.txt")],
    ["directory", directoryPath],
  ] as const) {
    const result = await runtime.execute(`upload-${name}`, binding, {
      type: "devtools",
      payload: {
        tab_id: binding.tab_id,
        operation: "upload_file",
        arguments: { element_ref: "e:1:1", file_path: filePath },
      },
    });
    assert.equal(result.outcome.status, "failed", name);
    assert.equal(result.outcome.payload.code, "browser_upload_file_invalid", name);
  }
  assert.equal(port.requests.some((request) => request.method === "DOM.setFileInputFiles"), false);
  await rm(root, { recursive: true, force: true });
});

test("浏览器截图收到快照根节点时必须捕获整页范围而不是把 root 当成 DOM ref", async () => {
  const port = new ScriptedPort((method) => (
    method === "Page.captureScreenshot"
      ? { data: PNG_BYTES.toString("base64") }
      : {}
  ));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const result = await runtime.execute("root-call", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      target: { element_ref: "root" },
      full_page: false,
      format: "png",
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.equal(result.binary_base64, PNG_BYTES.toString("base64"));
  assert.equal(port.requests.some((request) => request.method === "Runtime.evaluate"), false);
  assert.deepEqual(
    port.requests.find((request) => request.method === "Page.captureScreenshot")?.params,
    { format: "png", captureBeyondViewport: false, fromSurface: true },
  );
});

test("浏览器截图必须拒绝互斥范围组合，并校验图片文件头", async () => {
  const port = new ScriptedPort((method) => (
    method === "Page.captureScreenshot" ? { data: PNG_BYTES.toString("base64") } : {}
  ));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const invalid = await runtime.execute("scope-conflict", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      target: { element_ref: "e:1:1" },
      clip: { x: 0, y: 0, width: 1, height: 1 },
      full_page: false,
      format: "png",
    },
  });
  assert.equal(invalid.outcome.status, "failed");
  assert.equal(invalid.outcome.payload.code, "invalid_screenshot_scope");
  assert.equal(port.requests.some((request) => request.method === "Page.captureScreenshot"), false);

  const mismatched = await runtime.execute("format-mismatch", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      full_page: false,
      format: "webp",
    },
  });
  assert.equal(mismatched.outcome.status, "failed");
  assert.equal(mismatched.outcome.payload.code, "browser_screenshot_format_mismatch");

  assertScreenshotHeader(PNG_BYTES, "png");
  assertScreenshotHeader(JPEG_BYTES, "jpeg");
  assertScreenshotHeader(WEBP_BYTES, "webp");
  assert.notEqual(PNG_BYTES.subarray(0, 4).toString("ascii"), "RIFF");

  const formatPort = new ScriptedPort((method, params) => {
    if (method !== "Page.captureScreenshot") return {};
    const bytes = params.format === "jpeg" ? JPEG_BYTES : params.format === "webp" ? WEBP_BYTES : PNG_BYTES;
    return { data: bytes.toString("base64") };
  });
  const formatRuntime = new BrowserAutomationRuntime(new CdpClient(formatPort), "worker-test");
  for (const [format, bytes, mime] of [
    ["png", PNG_BYTES, "image/png"],
    ["jpeg", JPEG_BYTES, "image/jpeg"],
    ["webp", WEBP_BYTES, "image/webp"],
  ] as const) {
    const captured = await formatRuntime.execute(`format-${format}`, binding, {
      type: "screenshot",
      payload: {
        tab_id: binding.tab_id,
        navigation_revision: binding.navigation_revision,
        full_page: false,
        format,
      },
    });
    assert.equal(captured.outcome.status, "succeeded");
    assert.equal(captured.binary_base64, bytes.toString("base64"));
    if (captured.outcome.status === "succeeded" && captured.outcome.payload.type === "binary_payload") {
      assert.equal(captured.outcome.payload.payload.mime_type, mime);
    }
  }
});

test("整页截图使用 Chromium contentSize 和 captureBeyondViewport", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") {
      return { frameTree: { frame: { id: "frame-1" } } };
    }
    if (method === "Page.createIsolatedWorld") {
      return { executionContextId: 1 };
    }
    if (method === "Runtime.evaluate") {
      return {
        result: {
          value: String(params.expression).trim().endsWith("globalThis.__magiBrowserAutomation.viewport()")
            ? { width: 1200, height: 800, scrollX: 0, scrollY: 400 }
            : null,
        },
      };
    }
    if (method === "Page.getLayoutMetrics") {
      return {
        contentSize: { x: 0, y: 0, width: 1400, height: 3000 },
        cssVisualViewport: {
          clientWidth: 1200,
          clientHeight: 800,
          pageX: 0,
          pageY: 400,
        },
      };
    }
    if (method === "Page.captureScreenshot") return { data: PNG_BYTES.toString("base64") };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("full-page", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      full_page: true,
      format: "png",
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  assert.deepEqual(
    port.requests.find((request) => request.method === "Page.captureScreenshot")?.params,
    {
      format: "png",
      clip: { x: 0, y: 0, width: 1400, height: 3000, scale: 1 },
      captureBeyondViewport: true,
      fromSurface: true,
    },
  );
});

test("元素截图先滚动到元素并重新读取最终 bounds", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.trim().endsWith("globalThis.__magiBrowserAutomation.viewport()")) {
        return { result: { value: { width: 1200, height: 800, scrollX: 0, scrollY: 400 } } };
      }
      if (expression.includes("scrollIntoView") && expression.includes("bounds")) {
        return { result: { value: { bounds: { x: 20, y: 30, width: 400, height: 200 } } } };
      }
      return { result: { value: null } };
    }
    if (method === "Page.captureScreenshot") return { data: PNG_BYTES.toString("base64") };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("element-shot", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      target: { element_ref: "e:1:1" },
      full_page: false,
      format: "png",
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  const expressions = port.requests
    .filter((request) => request.method === "Runtime.evaluate")
    .map((request) => String(request.params.expression));
  assert.ok(expressions.some((expression) => expression.includes("scrollIntoView")));
  assert.deepEqual(
    port.requests.find((request) => request.method === "Page.captureScreenshot")?.params,
    {
      format: "png",
      clip: { x: 20, y: 30, width: 400, height: 200, scale: 1 },
      captureBeyondViewport: true,
      fromSurface: true,
    },
  );
});

test("浏览器自动化的响应式仿真必须通过 CDP 原生设备能力改变页面布局", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const result = await runtime.execute("emulate-call", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "emulate",
      arguments: {
        user_agent: "Magi Test Mobile",
        color_scheme: "dark",
        network_conditions: "fast 3g",
      },
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.deepEqual(
    port.requests.filter((request) => request.method.startsWith("Emulation.") || request.method === "Network.emulateNetworkConditions").map((request) => request.method),
    ["Emulation.setUserAgentOverride", "Emulation.setEmulatedMedia", "Network.emulateNetworkConditions"],
  );
  assert.equal(
    port.requests.find((request) => request.method === "Emulation.setUserAgentOverride")?.params.userAgent,
    "Magi Test Mobile",
  );
});

test("浏览器仿真 clear 会清理 UA 和额外请求头", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const result = await runtime.execute("emulate-clear", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "emulate",
      arguments: { action: "clear" },
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.ok(port.requests.some((request) => request.method === "Emulation.setUserAgentOverride" && request.params.userAgent === ""));
  assert.ok(port.requests.some((request) => request.method === "Network.setExtraHTTPHeaders" && JSON.stringify(request.params.headers) === "{}"));
});

test("持久化浏览器标记通过 Host 同步到当前 Chromium 文档", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") return { result: { value: { rendered: 1 } } };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("annotation-call", binding, {
    type: "set_annotations",
    payload: {
      tab_id: binding.tab_id,
      annotations: [{ annotation_id: "annotation-1", sequence: 1, status: "active" }],
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.deepEqual(result.outcome.payload, {
    type: "json",
    payload: { value: { rendered: 1 } },
  });
  assert.match(
    String(port.requests.find((request) => request.method === "Runtime.evaluate")?.params.expression),
    /setAnnotations/u,
  );
});

test("标记命中检测把内容槽归一化坐标转换为当前 Chromium CSS 视口坐标", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.includes("__magiBrowserAutomation.hitTest(0.5, 0.25)")) {
        return {
          result: {
            value: {
              navigation_revision: 0,
              viewport_width: 1280,
              viewport_height: 720,
              device_scale_factor_millis: 2000,
              scroll_x: 0,
              scroll_y: 0,
              element_ref: "e:1:1",
              tag_name: "button",
              test_id: null,
              stable_id: "save",
              aria_role: "button",
              aria_name: "Save",
              text_excerpt: "Save",
              css_path: "#save",
              ancestor_fingerprint: "ancestor",
              dom_fingerprint: "dom",
              bounds: { x: 640, y: 180, width: 120, height: 40 },
            },
          },
        };
      }
    }
    return { result: { value: null } };
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("hit-test-call", binding, {
    type: "hit_test",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      normalized_x: 0.5,
      normalized_y: 0.25,
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.equal(
    port.requests.some((request) => request.method === "Runtime.evaluate"
      && String(request.params.expression).includes("__magiBrowserAutomation.hitTest(0.5, 0.25)")),
    true,
  );
  assert.match(INSTALL_PAGE_RUNTIME, /normalizedX \* innerWidth/u);
  assert.match(INSTALL_PAGE_RUNTIME, /normalizedY \* innerHeight/u);
});

test("标记命中检测拒绝超出 [0, 1] 的归一化坐标", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("invalid-hit-test-call", binding, {
    type: "hit_test",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      normalized_x: 1.01,
      normalized_y: 0.5,
    },
  });

  assert.equal(result.outcome.status, "failed");
  if (result.outcome.status === "failed") {
    assert.equal(result.outcome.payload.code, "browser_hit_test_coordinates_invalid");
  }
});

test("同一 Surface 的并发工具调用按资源串行，运行时安装不会暴露半初始化状态", async () => {
  let isolatedWorlds = 0;
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: ++isolatedWorlds };
    if (method === "Runtime.evaluate" && String(params.expression).includes("setAnnotations")) {
      return { result: { value: { rendered: 1 } } };
    }
    return { result: { value: null } };
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const command = (call: string): Promise<unknown> => runtime.execute(call, binding, {
    type: "set_annotations",
    payload: {
      tab_id: binding.tab_id,
      annotations: [{ annotation_id: call, sequence: 1, status: "active" }],
    },
  });

  const [first, second] = await Promise.all([command("annotation-a"), command("annotation-b")]);
  assert.equal((first as { outcome: { status: string } }).outcome.status, "succeeded");
  assert.equal((second as { outcome: { status: string } }).outcome.status, "succeeded");
  assert.equal(isolatedWorlds, 1, "同一 Surface 只能创建一个自动化 isolated world");
});

test("页面运行上下文清理后必须重建 runtime，而不是调用失效的 setAnnotations", async () => {
  let isolatedWorlds = 0;
  const port = new ScriptedPort((method) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: ++isolatedWorlds };
    if (method === "Runtime.evaluate") return { result: { value: { rendered: 1 } } };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const command = (call: string): Promise<unknown> => runtime.execute(call, binding, {
    type: "set_annotations",
    payload: { tab_id: binding.tab_id, annotations: [] },
  });

  assert.equal((await command("annotation-before-clear") as { outcome: { status: string } }).outcome.status, "succeeded");
  port.emit("Runtime.executionContextsCleared");
  assert.equal((await command("annotation-after-clear") as { outcome: { status: string } }).outcome.status, "succeeded");
  assert.equal(isolatedWorlds, 2, "上下文清理后必须为当前文档重建 isolated world");
  assert.equal(
    port.requests.filter((request) => ["Page.enable", "Runtime.enable", "Network.enable"].includes(request.method)).length,
    3,
    "文档上下文清理后不能重复启用 WebContents 级 CDP 域",
  );
});

test("缓存的执行上下文缺少页面 runtime 时会先重新安装并验证", async () => {
  let isolatedWorlds = 0;
  let probeCount = 0;
  const port = new ScriptedPort((method) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: ++isolatedWorlds };
    if (method === "Runtime.evaluate") return { result: { value: { rendered: 1 } } };
    return {};
  }, () => probeCount++ !== 1);
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const command = (call: string): Promise<unknown> => runtime.execute(call, binding, {
    type: "set_annotations",
    payload: { tab_id: binding.tab_id, annotations: [] },
  });

  const first = await command("annotation-runtime-present") as { outcome: { status: string } };
  const second = await command("annotation-runtime-missing") as { outcome: { status: string; payload?: unknown } };
  assert.equal(first.outcome.status, "succeeded");
  assert.equal(second.outcome.status, "succeeded");
  assert.equal(isolatedWorlds, 2, "缓存上下文中的 runtime 缺失时不能继续复用该上下文");
  assert.equal(probeCount, 3, "安装前后都必须完成 runtime 健康检查");
});

test("Runtime.evaluate 竞态遇到失效 context 时返回错误且不重放脚本", async () => {
  let isolatedWorlds = 0;
  let failedStaleEvaluation = false;
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: ++isolatedWorlds };
    if (
      method === "Runtime.evaluate"
      && String(params.expression).includes("globalThis.__magiBrowserAutomation.setAnnotations(")
      && params.contextId === 1
      && !failedStaleEvaluation
    ) {
      failedStaleEvaluation = true;
      return new Error("Cannot find context with specified id");
    }
    return { result: { value: null } };
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("annotation-context-race", binding, {
    type: "set_annotations",
    payload: { tab_id: binding.tab_id, annotations: [] },
  });

  assert.equal(result.outcome.status, "failed");
  assert.equal(isolatedWorlds, 1);
  const annotationEvaluations = port.requests.filter((request) =>
    request.method === "Runtime.evaluate"
    && String(request.params.expression).includes("globalThis.__magiBrowserAutomation.setAnnotations("));
  assert.deepEqual(annotationEvaluations.map((request) => request.params.contextId), [1]);
});

test("浏览器标记层只观察目标布局相关节点，不监听整个页面属性", () => {
  assert.match(INSTALL_PAGE_RUNTIME, /annotationResizeObserver/u);
  assert.match(INSTALL_PAGE_RUNTIME, /attributeFilter:.*class.*style.*hidden.*open/u);
  assert.match(INSTALL_PAGE_RUNTIME, /childList: options\.childList/u);
  assert.doesNotMatch(
    INSTALL_PAGE_RUNTIME,
    /observe\(root,\s*\{\s*childList:\s*true,\s*subtree:\s*true,\s*attributes:\s*true/u,
  );
  assert.doesNotMatch(INSTALL_PAGE_RUNTIME, /while \(shadow\.firstChild\)/u);
});

test("区域标记按创建视口归一化到当前 Chromium 视口", () => {
  assert.match(INSTALL_PAGE_RUNTIME, /const scaleX = innerWidth \/ sourceWidth/u);
  assert.match(INSTALL_PAGE_RUNTIME, /const scaleY = innerHeight \/ sourceHeight/u);
  assert.match(
    INSTALL_PAGE_RUNTIME,
    /Number\(rect\.width\) \* sourceWidth \* scaleX/u,
  );
  assert.match(
    INSTALL_PAGE_RUNTIME,
    /\(Number\(rect\.x\) \* sourceWidth \+ scrollXAtCapture\) \* scaleX - scrollX/u,
  );
});

test("快照直接返回页面运行时的节点状态，不再往返 CDP 解析无障碍树", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 7 };
    if (method === "Runtime.evaluate") {
      if (String(params.expression).startsWith("globalThis.__magiBrowserAutomation.snapshot(")) {
        return { result: { value: {
          snapshot_revision: 4,
          root: {
            element_ref: "root",
            children: [{ element_ref: "e:4:1", role: "checkbox", name: "同意条款", states: ["checked", "required"], children: [] }],
          },
          returned_nodes: 1,
          total_nodes: 2,
          text_bytes: 12,
          truncated: false,
        } } };
      }
      return { result: { value: null } };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("snapshot-states", binding, {
    type: "snapshot",
    payload: {
      tab_id: binding.tab_id,
      navigation_revision: binding.navigation_revision,
      snapshot_revision: 4,
      limits: { max_nodes: 10, max_text_bytes: 1_000 },
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  const snapshot = result.outcome.payload.type === "snapshot" ? result.outcome.payload.payload : null;
  assert.deepEqual(snapshot?.root.children[0]?.states, ["checked", "required"]);
  assert.equal("accessibility_tree" in (snapshot ?? {}), false);
  assert.equal("continuation_refs" in (snapshot ?? {}), false);
  const cdpMethods = port.requests.map((request) => request.method);
  for (const removed of ["Accessibility.getFullAXTree", "DOM.describeNode", "DOM.resolveNode", "Runtime.callFunctionOn"]) {
    assert.equal(cdpMethods.includes(removed), false, `快照不应再调用 ${removed}`);
  }
});

test("read 校验参数后把分页与检索选项交给页面运行时", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      if (String(params.expression).includes("readText(")) return { result: { value: { text: "页面正文", total_chars: 4, truncated: false } } };
      return { result: { value: null } };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const run = (call: string, args: Record<string, unknown>) => runtime.execute(call, binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "read", arguments: args },
  });

  const ok = await run("read-ok", { query: " 价格 ", max_chars: 300, offset: 20, include_links: true });
  assert.equal(ok.outcome.status, "succeeded");
  // 安装页面运行时的表达式也包含 readText 定义，这里只看真正的调用。
  const calls = () => port.requests
    .filter((request) => request.method === "Runtime.evaluate")
    .map((request) => String(request.params.expression))
    .filter((expression) => expression.startsWith("globalThis.__magiBrowserAutomation."));
  const readExpression = calls().find((expression) => expression.includes("readText("));
  assert.ok(readExpression?.includes('readText("root", {'), "未指定 element_ref 时读取整页");
  assert.ok(readExpression?.includes('"maxChars":300'));
  assert.ok(readExpression?.includes('"offset":20'));
  assert.ok(readExpression?.includes('"includeLinks":true'));
  assert.ok(readExpression?.includes('"query":"价格"'), "query 去掉首尾空白");

  const scoped = await run("read-scoped", { element_ref: "e:3:7" });
  assert.equal(scoped.outcome.status, "succeeded");
  assert.ok(calls().some((expression) => expression.includes('readText("e:3:7", {')));

  const blankRef = await run("read-blank-ref", { element_ref: "   " });
  assert.equal(blankRef.outcome.status, "succeeded", "空白引用视为未指定范围，读取整页");

  for (const [call, args] of [
    ["read-zero", { max_chars: 0 }],
    ["read-huge", { max_chars: 50_001 }],
    ["read-negative-offset", { offset: -1 }],
  ] as const) {
    const bad = await run(call, args);
    assert.equal(bad.outcome.status, "failed", call);
    assert.equal(bad.outcome.payload.code, "browser_read_invalid", call);
  }
});

test("browser_storage 的 cookie 只返回元数据，清理逐个删除当前站点 cookie，且不允许读值", async () => {
  const deleted: Array<Record<string, unknown>> = [];
  let cookieQueryUrls: unknown = null;
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      if (String(params.expression) === "location.href") return { result: { value: "https://app.example.test/login" } };
      return { result: { value: null } };
    }
    if (method === "Network.getCookies") {
      cookieQueryUrls = params.urls;
      return { cookies: [
        { name: "sid", value: "SECRET-SESSION-TOKEN", domain: "app.example.test", path: "/", expires: -1, session: true, httpOnly: true, secure: true, sameSite: "Lax", size: 23 },
        { name: "theme", value: "dark", domain: ".example.test", path: "/", expires: 1900000000, session: false, httpOnly: false, secure: false, size: 9 },
      ] };
    }
    if (method === "Network.deleteCookies") {
      deleted.push(params);
      return {};
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const run = (call: string, args: Record<string, unknown>) => runtime.execute(call, binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "storage", arguments: args },
  });

  const listed = await run("cookie-list", { area: "cookies", action: "list" });
  assert.equal(listed.outcome.status, "succeeded");
  assert.deepEqual(cookieQueryUrls, ["https://app.example.test/login"], "只查询当前页面 URL 的 cookie");
  assert.ok(!JSON.stringify(listed.outcome.payload).includes("SECRET-SESSION-TOKEN"), "cookie 值绝不能出现在结果中");
  const value = listed.outcome.payload.type === "json" ? listed.outcome.payload.payload.value as { cookies: Array<Record<string, unknown>> } : null;
  assert.deepEqual(value?.cookies.map((cookie) => cookie.name), ["sid", "theme"]);
  assert.ok(value?.cookies.every((cookie) => !("value" in cookie)), "cookie 元数据中不能有 value 字段");
  assert.equal(value?.cookies[0]?.http_only, true);
  assert.equal(value?.cookies[0]?.expires, null, "会话 cookie 没有过期时间");

  const cleared = await run("cookie-clear", { area: "cookies", action: "clear" });
  assert.equal(cleared.outcome.status, "succeeded");
  assert.deepEqual(deleted, [
    { name: "sid", domain: "app.example.test", path: "/" },
    { name: "theme", domain: ".example.test", path: "/" },
  ], "只删除当前站点的 cookie，不清空整个浏览器分区");

  const readValue = await run("cookie-get", { area: "cookies", action: "get", key: "sid" });
  assert.equal(readValue.outcome.status, "failed");
  assert.equal(readValue.outcome.payload.code, "browser_storage_cookie_action_unsupported");
  assert.equal(port.requests.some((request) => request.method === "Network.clearBrowserCookies"), false);
});

test("点击不使用固定延时且原生事件后校验目标", async () => {
  const port = new ScriptedPort((method) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") return { result: { value: { x: 10, y: 20, bounds: { x: 0, y: 0, width: 20, height: 20 }, editable: false, sensitive: null, observed: true } } };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("click-merged", binding, {
    type: "click",
    payload: {
      tab_id: binding.tab_id,
      control: { mode: "user", fence: 1 },
      target: { element_ref: "e:1:btn" },
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  // 安装页面运行时的表达式包含这些方法的定义，这里只统计对页面运行时的真实调用。
  const expressions = port.requests
    .filter((request) => request.method === "Runtime.evaluate")
    .map((request) => String(request.params.expression))
    .filter((expression) => expression.startsWith("globalThis.__magiBrowserAutomation."));
  assert.equal(expressions.filter((expression) => expression.startsWith("globalThis.__magiBrowserAutomation.prepareClick(")).length, 1);
  assert.equal(expressions.some((expression) => expression.startsWith("globalThis.__magiBrowserAutomation.focus(")), false, "不再单独往返 focus");
});

test("页面运行时对同源 iframe 使用主视口坐标，并以 nodeType 判断元素", () => {
  assert.match(INSTALL_PAGE_RUNTIME, /frame\.clientLeft/u, "frame 边框参与坐标累加");
  assert.match(INSTALL_PAGE_RUNTIME, /view\.frameElement/u);
  assert.match(INSTALL_PAGE_RUNTIME, /const isElement = \(value\) => Boolean\(value\) && value\.nodeType === 1/u);
  // 快照与引用相关路径必须用 isElement；标注功能只作用于主文档，仍可使用 instanceof。
  assert.match(INSTALL_PAGE_RUNTIME, /if \(!isElement\(element\) \|\| !visible\(element\)\) return false;/u);
});

test("导航 revision 变化后不会复用上一文档的 Console 和 Network 记录", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") return { result: { value: null } };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  port.emit("Runtime.consoleAPICalled", { type: "log", args: [{ value: "old" }] });
  port.emit("Network.responseReceived", { requestId: "old-request", response: { url: "https://old.test/app.js" } });
  const first = await runtime.execute("old-page", binding, consoleCommand());
  assert.equal(first.outcome.status, "succeeded");

  const nextBinding = { ...binding, navigation_revision: binding.navigation_revision + 1 };
  const next = await runtime.execute("new-page", nextBinding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "console", arguments: { action: "list" } },
  });
  assert.equal(next.outcome.status, "succeeded");
  const value = next.outcome.payload.type === "json" ? next.outcome.payload.payload.value as Record<string, unknown> : null;
  assert.deepEqual(value, { entries: [] });
});

test("Renderer 重启清理 Worker 保存的 CDP 运行态，避免复用失效会话", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  for (const action of ["start", "profile_start", "coverage_start"] as const) {
    const result = await runtime.execute(`lifecycle-${action}`, binding, {
      type: "devtools",
      payload: { tab_id: binding.tab_id, operation: "performance", arguments: { action } },
    });
    assert.equal(result.outcome.status, "succeeded");
  }
  port.emit("Tracing.dataCollected", { value: [{ name: "RunTask", dur: 75_000 }] });
  port.emit("Runtime.executionContextsCleared");

  const analyze = await runtime.execute("lifecycle-analyze", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "performance", arguments: { action: "analyze" } },
  });
  assert.equal(analyze.outcome.status, "succeeded");
  const analyzeValue = analyze.outcome.payload.type === "json"
    ? analyze.outcome.payload.payload.value as Record<string, unknown>
    : null;
  assert.equal(analyzeValue?.traceActive, false);
  assert.deepEqual(analyzeValue?.insights && (analyzeValue.insights as Record<string, unknown>).event_count, 0);

  const profileStop = await runtime.execute("lifecycle-profile-stop", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "performance", arguments: { action: "profile_stop" } },
  });
  assert.equal(profileStop.outcome.status, "succeeded");
  const profileValue = profileStop.outcome.payload.type === "json"
    ? profileStop.outcome.payload.payload.value as Record<string, unknown>
    : null;
  assert.equal(profileValue?.stopped, false);

  const coverageStop = await runtime.execute("lifecycle-coverage-stop", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "performance", arguments: { action: "coverage_stop" } },
  });
  assert.equal(coverageStop.outcome.status, "succeeded");
  const coverageValue = coverageStop.outcome.payload.type === "json"
    ? coverageStop.outcome.payload.payload.value as Record<string, unknown>
    : null;
  assert.equal(coverageValue?.stopped, false);
  assert.equal(port.requests.filter((request) => request.method === "Profiler.stop").length, 0);
  assert.equal(port.requests.filter((request) => request.method === "Profiler.stopPreciseCoverage").length, 0);
});

test("第三方分析按响应来源聚合请求和字节数", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      return { result: { value: expression.includes("location.origin") ? "https://app.test" : null } };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  await runtime.execute("network-response", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "console", arguments: { action: "list" } },
  });
  const result = await runtime.execute("third-party", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "third_party", arguments: { action: "list" } },
  });
  assert.equal(result.outcome.status, "succeeded");
});

test("性能工具支持 CPU profile 和 precise coverage 生命周期", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const start = await runtime.execute("profile-start", binding, {
    type: "devtools", payload: { tab_id: binding.tab_id, operation: "performance", arguments: { action: "profile_start" } },
  });
  assert.equal(start.outcome.status, "succeeded");
  const stop = await runtime.execute("profile-stop", binding, {
    type: "devtools", payload: { tab_id: binding.tab_id, operation: "performance", arguments: { action: "profile_stop" } },
  });
  assert.equal(stop.outcome.status, "succeeded");
  assert.ok(port.requests.some((request) => request.method === "Profiler.start"));
  assert.ok(port.requests.some((request) => request.method === "Profiler.stop"));
});

test("性能 analyze 返回聚合洞察，并仅按需返回原始事件", async () => {
  const port = new ScriptedPort(() => ({}));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const start = await runtime.execute("insight-start", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "performance", arguments: { action: "start" } },
  });
  assert.equal(start.outcome.status, "succeeded");
  port.emit("Tracing.dataCollected", {
    value: [
      { name: "RunTask", dur: 75_000, ts: 1_000_000 },
      { name: "firstContentfulPaint", ts: 1_050_000 },
    ],
  });
  const result = await runtime.execute("insight-analyze", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "performance",
      arguments: { action: "analyze", include_events: false },
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  const value = result.outcome.payload.type === "json" ? result.outcome.payload.payload.value as Record<string, unknown> : null;
  const insights = value?.insights as Record<string, unknown>;
  assert.equal(insights?.event_count, 2);
  assert.equal(insights?.long_task_count, 1);
  assert.equal("events" in (value ?? {}), false);
});

test("第三方分析只统计 response，并使用 loadingFinished 的编码字节数", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      return { result: { value: expression.includes("location.origin") ? "https://app.test" : null } };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  runtime.rebind([binding]);
  port.emit("Network.requestWillBeSent", { requestId: "r1", type: "Script", request: { url: "https://cdn.test/app.js" } });
  port.emit("Network.responseReceived", { requestId: "r1", type: "Script", response: { url: "https://cdn.test/app.js" } });
  port.emit("Network.loadingFinished", { requestId: "r1", encodedDataLength: 321 });
  const result = await runtime.execute("third-party-accurate", binding, {
    type: "devtools",
    payload: { tab_id: binding.tab_id, operation: "third_party", arguments: { action: "list" } },
  });
  assert.equal(result.outcome.status, "succeeded");
  const value = result.outcome.payload.type === "json" ? result.outcome.payload.payload.value as Record<string, any> : null;
  assert.deepEqual(value, { page_origin: "https://app.test", entries: [{ origin: "https://cdn.test", requests: 1, bytes: 321, resource_types: { Script: 1 }, urls: ["https://cdn.test/app.js"] }], total_requests: 1 });
  assert.equal(value?.entries?.[0]?.requests, 1);
  assert.equal(value?.entries?.[0]?.bytes, 321);
  assert.equal(value?.entries?.[0]?.resource_types?.Script, 1);
});

test("Heap 快照按 Chrome 的连续 edge 数组索引解析对象关系", async () => {
  const snapshot = JSON.stringify({
    snapshot: { meta: {
      node_fields: ["type", "name", "id", "self_size", "edge_count", "trace_node_id"],
      node_types: [["hidden", "object"]],
      edge_fields: ["type", "name_or_index", "to_node"],
      edge_types: [["context", "element", "property"]],
    } },
    nodes: [0, 0, 1, 8, 1, 0, 1, 1, 2, 16, 0, 0],
    edges: [2, 1, 6],
    strings: ["root", "child"],
  });
  const port = new ScriptedPort((method) => {
    if (method === "Runtime.getHeapUsage") return { usedSize: 1 };
    if (method === "HeapProfiler.takeHeapSnapshot") {
      queueMicrotask(() => port.emit("HeapProfiler.addHeapSnapshotChunk", { chunk: snapshot }));
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const take = await runtime.execute("heap-take", binding, {
    type: "devtools", payload: { tab_id: binding.tab_id, operation: "heap", arguments: { action: "take_snapshot" } },
  });
  assert.equal(take.outcome.status, "succeeded");
  const edges = await runtime.execute("heap-edges", binding, {
    type: "devtools", payload: { tab_id: binding.tab_id, operation: "heap", arguments: { action: "edges", node_id: 0 } },
  });
  assert.equal(edges.outcome.status, "succeeded");

  const dominators = await runtime.execute("heap-dominators", binding, {
    type: "devtools", payload: { tab_id: binding.tab_id, operation: "heap", arguments: { action: "dominators" } },
  });
  assert.equal(dominators.outcome.status, "succeeded");
  const dominatorValue = dominators.outcome.payload.type === "json"
    ? dominators.outcome.payload.payload.value as { nodes?: Array<Record<string, unknown>> }
    : null;
  assert.equal(dominatorValue?.nodes?.length, 2);
  assert.equal(dominatorValue?.nodes?.find((node) => node.id === 1)?.immediate_dominator, 0);
});

test("GPT Web 探测只返回登录与页面可用性，不含网页模型菜单", async () => {
  const port = webScriptedPort({
    probe: {
      origin: "https://chatgpt.com",
      path: "/?temporary-chat=true",
      blocked: false,
      composerFound: true,
      conversationFound: true,
      accountHint: "Plus",
    },
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("web-probe", binding, webCommand("web_model_probe", {
    tab_id: binding.tab_id,
  }));
  assert.equal(result.outcome.status, "succeeded");
  const value = result.outcome.payload.type === "json"
    ? result.outcome.payload.payload.value as Record<string, any>
    : null;
  assert.equal(value?.login_state, "signed_in");
  assert.equal(value?.composer_available, true);
  assert.equal(value?.account_hint, "plus");
  assert.equal(value?.models, undefined, "探测结果里不得出现网页的模型菜单");
  assert.equal(value?.limits_revision, undefined);
});

test("GPT Web 探测在登录着却找不到输入框时报告未登录事实，由 daemon 判为 selectors_drift", async () => {
  const port = webScriptedPort({
    probe: {
      origin: "https://chatgpt.com",
      path: "/",
      blocked: false,
      composerFound: false,
      conversationFound: true,
      accountHint: null,
    },
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("web-probe-drift", binding, webCommand("web_model_probe", {
    tab_id: binding.tab_id,
  }));
  assert.equal(result.outcome.status, "succeeded");
  const value = result.outcome.payload.type === "json"
    ? result.outcome.payload.payload.value as Record<string, any>
    : null;
  assert.equal(value?.login_state, "signed_out");
  assert.equal(value?.composer_available, false);
});

test("已保存对话命令只做校验后的透传：列表、消息、精确删除", async () => {
  const port = webScriptedPort({
    savedConversations: {
      conversations: [{ conversation_id: "11111111-1111-1111-1111-111111111111", title: "第一条", updated_at: null }],
    },
    savedMessages: {
      conversation_id: "11111111-1111-1111-1111-111111111111",
      title: "第一条",
      messages: [{ role: "user", text: "你好", remote_id: "m1" }, { role: "assistant", text: "你好！", remote_id: "m2" }],
      last_message_id: "m2",
      updated_at: null,
    },
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const json = (result: Awaited<ReturnType<typeof runtime.execute>>) => (
    result.outcome.status === "succeeded" && result.outcome.payload.type === "json"
      ? result.outcome.payload.payload.value as Record<string, any>
      : null
  );
  const listed = json(await runtime.execute("saved-list", binding, webCommand("web_saved_conversations", { tab_id: binding.tab_id })));
  assert.equal(listed?.conversations?.length, 1);
  const messages = json(await runtime.execute("saved-messages", binding, webCommand("web_saved_messages", { tab_id: binding.tab_id })));
  assert.equal(messages?.last_message_id, "m2");
  assert.equal(messages?.messages?.[1]?.role, "assistant");
});

test("页面适配器返回畸形的已保存对话 / 连接器结果时命令显式失败，不透传", async () => {
  const port = webScriptedPort({
    savedConversations: { conversations: [{ conversation_id: "", title: 1 }] },
    savedMessages: { conversation_id: null, messages: [{ role: "system", text: "x", remote_id: null }] },
    connectorStatus: { supported: true },
    configureConnector: { configured: true },
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  for (const [type, payload, code] of [
    ["web_saved_conversations", {}, "web_saved_conversations_result_invalid"],
    ["web_saved_messages", {}, "web_saved_messages_result_invalid"],
    ["web_connector_status", { name: "Magi" }, "web_connector_status_result_invalid"],
    ["web_configure_connector", { name: "Magi", tunnel_id: "t" }, "web_configure_connector_result_invalid"],
  ] as const) {
    const result = await runtime.execute(`bad-${type}`, binding, webCommand(type, { tab_id: binding.tab_id, ...payload }));
    assert.equal(result.outcome.status, "failed", type);
    if (result.outcome.status === "failed") assert.equal(result.outcome.payload.code, code);
  }
});

test("连接器状态与配置命令把页面适配器的回读结果原样返回", async () => {
  const port = webScriptedPort({
    connectorStatus: { supported: true, exists: true, enabled: true, tool_count: 14, reason: null },
    configureConnector: { configured: true, confirmed_enabled: true, reason: null },
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const status = await runtime.execute("connector-status", binding, webCommand("web_connector_status", {
    tab_id: binding.tab_id, name: "Magi",
  }));
  assert.equal(status.outcome.status, "succeeded");
  const configured = await runtime.execute("connector-configure", binding, webCommand("web_configure_connector", {
    tab_id: binding.tab_id, name: "Magi", tunnel_id: "tunnel_1",
  }));
  assert.equal(configured.outcome.status, "succeeded");
});

test("web_write_text 使用 snake_case payload、遵守 timeout_ms，并返回安全回读诊断", async () => {
  const port = webScriptedPort({
    write: { text: "站点改写后的内容", charCount: 8, becameAttachment: false },
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("web-write", binding, webCommand("web_write_text", {
    tab_id: binding.tab_id,
    selector: "@composer",
    text: "期望内容",
    mode: "replace",
    timeout_ms: 17,
  }));
  assert.equal(result.outcome.status, "succeeded");
  const value = result.outcome.payload.type === "json"
    ? result.outcome.payload.payload.value as Record<string, any>
    : null;
  assert.equal(value?.confirmed, false);
  assert.equal(value?.diagnostic, "composer_readback_mismatch");
  assert.equal(typeof value?.mismatch?.index, "number");
  const expression = port.requests
    .filter((request) => request.method === "Runtime.evaluate")
    .map((request) => String(request.params.expression))
    .find((expression) => expression.includes("__magiWebModel.writeText("));
  assert.match(expression ?? "", /timeout_ms/iu);
  assert.doesNotMatch(expression ?? "", /timeoutMs/iu);
});

test("web_observe 对不可达 selector 返回 found:false 且 revision 每次递增", async () => {
  const port = webScriptedPort({ observe: { nodes: [{ found: false }] } });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const command = webCommand("web_observe", {
    tab_id: binding.tab_id,
    selector: "@assistantMessage",
    fields: ["existence"],
  });
  const first = await runtime.execute("web-observe-1", binding, command);
  const second = await runtime.execute("web-observe-2", binding, command);
  assert.equal(first.outcome.status, "succeeded");
  assert.equal(second.outcome.status, "succeeded");
  const firstValue = first.outcome.payload.type === "json" ? first.outcome.payload.payload.value as Record<string, any> : null;
  const secondValue = second.outcome.payload.type === "json" ? second.outcome.payload.payload.value as Record<string, any> : null;
  assert.deepEqual(firstValue?.nodes, [{ found: false }]);
  assert.equal(firstValue?.revision, 1);
  assert.equal(secondValue?.revision, 2);
});

test("web_submit、web_turn_state 和取消命令保留流式回读的消息计数与正文", async () => {
  const port = webScriptedPort({
    submit: { submitted: true, composer_empty: true, reason: null },
    turnState: {
      origin: "https://chatgpt.com",
      path: "/?temporary-chat=true",
      blocked: false,
      generating: true,
      conversation_found: true,
      composer_found: true,
      user_message_count: 2,
      assistant_message_count: 2,
      assistant_text: "~~~magi-tool-call\\n{\\\"turn_id\\\":\\\"nonce\\\"}\\n~~~",
      thinking_text: "正在思考",
      last_message_role: "assistant",
      last_message_text: "~~~magi-tool-call\\n{\\\"turn_id\\\":\\\"nonce\\\"}\\n~~~",
      account_hint: "Plus",
    },
    cancel: true,
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const submit = await runtime.execute("web-submit", binding, webCommand("web_submit", {
    tab_id: binding.tab_id,
  }));
  assert.equal(submit.outcome.status, "succeeded");
  const state = await runtime.execute("web-state", binding, webCommand("web_turn_state", {
    tab_id: binding.tab_id,
  }));
  assert.equal(state.outcome.status, "succeeded");
  const stateValue = state.outcome.payload.type === "json"
    ? state.outcome.payload.payload.value as Record<string, any>
    : null;
  assert.equal(stateValue?.login_state, "signed_in");
  assert.equal(stateValue?.generating, true);
  assert.equal(stateValue?.user_message_count, 2);
  assert.match(String(stateValue?.assistant_text), /magi-tool-call/u);
  const cancelled = await runtime.execute("web-cancel", binding, webCommand("web_cancel_generation", {
    tab_id: binding.tab_id,
  }));
  assert.equal(cancelled.outcome.status, "succeeded");
});

test("同一临时对话多轮复用页面适配器，导航代次变化后重新安装", async () => {
  const port = webScriptedPort({
    turnState: {
      origin: "https://chatgpt.com",
      path: "/?temporary-chat=true",
      blocked: false,
      generating: false,
      conversation_found: true,
      composer_found: true,
      user_message_count: 1,
      assistant_message_count: 1,
      assistant_text: "第一轮",
      thinking_text: "",
      last_message_role: "assistant",
      last_message_text: "第一轮",
      account_hint: "Plus",
    },
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const stateCommand = webCommand("web_turn_state", { tab_id: binding.tab_id });
  await runtime.execute("multi-turn-1", binding, stateCommand);
  await runtime.execute("multi-turn-2", binding, stateCommand);
  const nextBinding = { ...binding, navigation_revision: binding.navigation_revision + 1 };
  await runtime.execute(
    "multi-turn-after-navigation",
    nextBinding,
    webCommand("web_turn_state", { tab_id: nextBinding.tab_id }),
  );
  const installCount = port.requests.filter((request) =>
    request.method === "Runtime.evaluate"
    && String(request.params.expression).includes("globalThis.__magiWebModel ="),
  ).length;
  assert.equal(installCount, 2);
});
