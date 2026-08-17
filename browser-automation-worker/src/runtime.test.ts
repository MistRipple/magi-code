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
