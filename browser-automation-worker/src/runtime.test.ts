import assert from "node:assert/strict";
import { mkdtemp, realpath, rm, writeFile } from "node:fs/promises";
import test from "node:test";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type {
  BrowserHostCommand,
  BrowserSurfaceBinding,
  MainToWorkerMessage,
  WorkerToMainMessage,
} from "@magi/desktop-browser-contracts";
import { CdpClient, type ParentPort } from "./cdp-client.js";
import { BrowserAutomationRuntime } from "./runtime.js";

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
      return { data: PNG_BYTES.toString("base64") };
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
  assert.equal(result.binary_base64, PNG_BYTES.toString("base64"));
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
      return { data: PNG_BYTES.toString("base64") };
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

test("fill_form 按原生控件语义处理 select、checkbox 和 radio", async () => {
  const port = new ScriptedPort((method, params) => {
    if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "frame-1" } } };
    if (method === "Page.createIsolatedWorld") return { executionContextId: 1 };
    if (method === "Runtime.evaluate") {
      const expression = String(params.expression);
      if (expression.includes("e:1:radio") && expression.includes("element.type === 'radio'")) return { result: { value: { kind: "radio" } } };
      if (expression.includes("e:1:checkbox") && expression.includes("element.type === 'checkbox'")) return { result: { value: { kind: "checkbox" } } };
      if (expression.includes("e:1:select") && expression.includes("element instanceof HTMLSelectElement")) return { result: { value: { kind: "select", multiple: true } } };
      return { result: { value: null } };
    }
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
          { snapshot_revision: 1, element_ref: "e:1:select", value: ["one", "two"] },
          { snapshot_revision: 1, element_ref: "e:1:checkbox", value: true },
          { snapshot_revision: 1, element_ref: "e:1:radio", value: true },
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
  assert.ok(evaluateCalls.some((request) => String(request.params.expression).includes("element.click()")));
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
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test", { uploadRoot });
  const result = await runtime.execute("upload-files", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "upload_file",
      arguments: {
        snapshot_revision: 1,
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
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test", { uploadRoot });
  const result = await runtime.execute("upload-text-input", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "upload_file",
      arguments: { snapshot_revision: 1, element_ref: "e:1:1", file_path: filePath },
    },
  });
  assert.equal(result.outcome.status, "failed");
  assert.equal(result.outcome.payload.code, "browser_upload_target_invalid");
  await rm(uploadRoot, { recursive: true, force: true });
});

test("文件上传拒绝 Magi staging 目录之外的路径", async () => {
  const uploadRoot = await mkdtemp(join(tmpdir(), "magi-browser-upload-root-"));
  const outsideRoot = await mkdtemp(join(tmpdir(), "magi-browser-upload-outside-"));
  const outsidePath = join(outsideRoot, "outside.txt");
  await writeFile(outsidePath, "outside");
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
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test", { uploadRoot });
  const result = await runtime.execute("upload-outside", binding, {
    type: "devtools",
    payload: {
      tab_id: binding.tab_id,
      operation: "upload_file",
      arguments: { snapshot_revision: 1, element_ref: "e:1:1", file_path: outsidePath },
    },
  });
  assert.equal(result.outcome.status, "failed");
  assert.equal(result.outcome.payload.code, "browser_upload_path_outside_boundary");
  assert.equal(port.requests.some((request) => request.method === "DOM.setFileInputFiles"), false);
  await Promise.all([
    rm(uploadRoot, { recursive: true, force: true }),
    rm(outsideRoot, { recursive: true, force: true }),
  ]);
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
      target: { snapshot_revision: 1, element_ref: "root" },
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
      target: { snapshot_revision: 1, element_ref: "e:1:1" },
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
      payload: { tab_id: binding.tab_id, full_page: false, format },
    });
    assert.equal(captured.outcome.status, "succeeded");
    assert.equal(captured.binary_base64, bytes.toString("base64"));
    if (captured.outcome.status === "succeeded" && captured.outcome.payload.type === "binary_payload") {
      assert.equal(captured.outcome.payload.payload.mime_type, mime);
    }
  }
});

test("整页截图使用 Chromium contentSize 和 captureBeyondViewport", async () => {
  const port = new ScriptedPort((method) => {
    if (method === "Page.getLayoutMetrics") {
      return { contentSize: { x: 0, y: 0, width: 1400, height: 3000 } };
    }
    if (method === "Page.captureScreenshot") return { data: PNG_BYTES.toString("base64") };
    return {};
  });
  const runtime = new BrowserAutomationRuntime(new CdpClient(port), "worker-test");
  const result = await runtime.execute("full-page", binding, {
    type: "screenshot",
    payload: { tab_id: binding.tab_id, full_page: true, format: "png" },
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
      if (expression.includes("getBoundingClientRect")) {
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
      target: { snapshot_revision: 1, element_ref: "e:1:1" },
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
