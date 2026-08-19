import assert from "node:assert/strict";
import test from "node:test";
import type {
  BrowserHostCommand,
  BrowserSurfaceBinding,
  MainToWorkerMessage,
  WorkerToMainMessage,
} from "@magi/desktop-browser-contracts";
import { CdpClient, type ParentPort } from "./cdp-client.js";
import { BrowserAutomationRuntime } from "./runtime.js";

class FakePort implements ParentPort {
  #listener: ((event: { data: MainToWorkerMessage }) => void) | null = null;
  readonly methods: string[] = [];
  responseBinding: BrowserSurfaceBinding | null = null;

  on(_event: "message", listener: (event: { data: MainToWorkerMessage }) => void): void {
    this.#listener = listener;
  }

  postMessage(message: WorkerToMainMessage): void {
    if (message.type !== "cdp_request") return;
    this.methods.push(message.method);
    queueMicrotask(() => {
      this.#listener?.({
        data: {
          type: "cdp_response",
          request_id: message.request_id,
          binding: this.responseBinding ?? message.binding,
          result: {},
        },
      });
    });
  }
}

class ScriptedPort implements ParentPort {
  #listener: ((event: { data: MainToWorkerMessage }) => void) | null = null;
  readonly requests: Array<{ method: string; params: Record<string, unknown> }> = [];

  constructor(
    private readonly respond: (method: string, params: Record<string, unknown>) => unknown,
  ) {}

  on(_event: "message", listener: (event: { data: MainToWorkerMessage }) => void): void {
    this.#listener = listener;
  }

  postMessage(message: WorkerToMainMessage): void {
    if (message.type !== "cdp_request") return;
    this.requests.push({ method: message.method, params: message.params ?? {} });
    queueMicrotask(() => {
      this.#listener?.({
        data: {
          type: "cdp_response",
          request_id: message.request_id,
          binding: message.binding,
          result: this.respond(message.method, message.params ?? {}),
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

test("Worker 为每个 CDP Surface 显式启用页面、运行时和网络事件域", async () => {
  const port = new FakePort();
  const runtime = new BrowserAutomationRuntime(new CdpClient(port));

  const first = await runtime.execute("call-1", binding, consoleCommand());
  assert.equal(first.outcome.status, "succeeded");
  assert.deepEqual(new Set(port.methods), new Set(["Page.enable", "Runtime.enable", "Network.enable"]));

  await runtime.execute("call-2", binding, consoleCommand());
  assert.equal(port.methods.length, 3, "同一 Surface 不应为每次工具调用重复启用 CDP 域");
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

test("浏览器截图的归一化区域必须转换为当前布局视口的真实裁剪区域", async () => {
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
            ? { width: 1200, height: 800 }
            : null,
        },
      };
    }
    if (method === "Page.captureScreenshot") {
      return { data: Buffer.from("png-bytes").toString("base64") };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const result = await runtime.execute("clip-call", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      clip: { x: 0.25, y: 0.1, width: 0.5, height: 0.25 },
      full_page: false,
      format: "png",
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.equal(result.binary_base64, Buffer.from("png-bytes").toString("base64"));
  const capture = port.requests.find((request) => request.method === "Page.captureScreenshot");
  assert.deepEqual(capture?.params, {
    format: "png",
    clip: { x: 300, y: 80, width: 600, height: 200, scale: 1 },
    captureBeyondViewport: false,
    fromSurface: true,
  });
});

test("截图和滚动使用页面脚本视口坐标，不把 CDP layoutViewport 的物理尺寸混入归一化坐标", async () => {
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
      return { data: Buffer.from("png-bytes").toString("base64") };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const screenshot = await runtime.execute("viewport-clip-call", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
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
  assert.deepEqual(
    port.requests.find((request) => request.method === "Input.dispatchMouseEvent")?.params,
    { type: "mouseWheel", x: 240, y: 427, deltaX: 0, deltaY: 400 },
  );
});

test("滚动目标元素时使用元素中心作为 wheel 坐标", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.trim().endsWith("globalThis.__magiBrowserAutomation.viewport()")) {
        return { result: { value: { width: 800, height: 600 } } };
      }
      if (expression.includes("globalThis.__magiBrowserAutomation.target")) {
        return { result: { value: { x: 120, y: 240, bounds: { x: 100, y: 220, width: 40, height: 40 }, editable: false, sensitive: null } } };
      }
      return { result: { value: null } };
    }
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("scroll-target", binding, {
    type: "scroll",
    payload: {
      tab_id: binding.tab_id,
      control: { mode: "user", fence: 1 },
      target: { snapshot_revision: 1, element_ref: "e:1:1" },
      delta_x: 0,
      delta_y: 300,
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  assert.deepEqual(
    port.requests.find((request) => request.method === "Input.dispatchMouseEvent")?.params,
    { type: "mouseWheel", x: 120, y: 240, deltaX: 0, deltaY: 300 },
  );
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

test("文件上传支持单文件和多文件 file input", async () => {
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
        snapshot_revision: 1,
        element_ref: "e:1:1",
        file_paths: ["/tmp/first.txt", "/tmp/second.txt"],
      },
    },
  });
  assert.equal(result.outcome.status, "succeeded");
  assert.deepEqual(
    port.requests.find((request) => request.method === "DOM.setFileInputFiles")?.params,
    { nodeId: 2, files: ["/tmp/first.txt", "/tmp/second.txt"] },
  );
});

test("文件上传拒绝无法解析为 file input 的快照目标", async () => {
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
      arguments: { snapshot_revision: 1, element_ref: "e:1:1", file_path: "/tmp/first.txt" },
    },
  });
  assert.equal(result.outcome.status, "failed");
  assert.equal(result.outcome.payload.code, "browser_upload_target_invalid");
});

test("浏览器截图收到快照根节点时必须捕获整页范围而不是把 root 当成 DOM ref", async () => {
  const port = new ScriptedPort((method) => (
    method === "Page.captureScreenshot"
      ? { data: Buffer.from("root-screenshot").toString("base64") }
      : {}
  ));
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");

  const result = await runtime.execute("root-call", binding, {
    type: "screenshot",
    payload: {
      tab_id: binding.tab_id,
      target: { snapshot_revision: 1, element_ref: "root" },
      full_page: false,
      format: "png",
    },
  });

  assert.equal(result.outcome.status, "succeeded");
  assert.equal(result.binary_base64, Buffer.from("root-screenshot").toString("base64"));
  assert.equal(port.requests.some((request) => request.method === "Runtime.evaluate"), false);
  assert.deepEqual(
    port.requests.find((request) => request.method === "Page.captureScreenshot")?.params,
    { format: "png", captureBeyondViewport: false, fromSurface: true },
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

test("Accessibility 节点引用使用 Worker isolated world 的执行上下文", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 7 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.includes("snapshot(")) {
        return { result: { value: {
          snapshot_revision: 4,
          root: { element_ref: "root", children: [] },
          returned_nodes: 0,
          total_nodes: 1,
          text_bytes: 0,
          truncated: false,
        } } };
      }
      return { result: { value: null } };
    }
    if (method === "Accessibility.getFullAXTree") {
      return { nodes: [{ nodeId: "ax-1", backendDOMNodeId: 42, role: { value: "button" }, name: { value: "提交" }, childIds: [] }] };
    }
    if (method === "DOM.describeNode") return { node: { nodeId: 11 } };
    if (method === "DOM.resolveNode") {
      assert.equal(params.executionContextId, 7);
      return { object: { objectId: "object-1" } };
    }
    if (method === "Runtime.callFunctionOn") return { result: { value: "e:4:1" } };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("ax-call", binding, {
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
  assert.equal(snapshot?.accessibility_tree?.[0]?.element_ref, "e:4:1");
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
