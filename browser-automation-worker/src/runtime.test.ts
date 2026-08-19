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

test("未注册的浏览器能力返回结构化 capability_unavailable", async () => {
  const port = new FakePort();
  const runtime = new BrowserAutomationRuntime(new CdpClient(port));

  for (const operation of ["upload_file", "third_party", "lighthouse"]) {
    const result = await runtime.execute("unsupported-call", binding, {
      type: "devtools",
      payload: {
        tab_id: binding.tab_id,
        operation,
        arguments: {},
      },
    });
    assert.equal(result.outcome.status, "failed");
    assert.equal(result.outcome.payload.code, "capability_unavailable");
  }
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
