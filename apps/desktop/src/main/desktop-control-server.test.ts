import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { WebSocket, type RawData } from "ws";
import {
  DESKTOP_BROWSER_PROTOCOL_VERSION,
  normalizeOptionalDomNodeId,
  type BrowserCommandOutcome,
  type BrowserHostCommand,
  type BrowserHostRequestEnvelope,
  type BrowserSurfaceBinding,
  type DesktopBrowserHandshake,
} from "@magi/desktop-browser-contracts";
import {
  assertDesktopIpcMessage,
  parseBrowserHostRequest,
  parseWorkerToMainMessage,
} from "@magi/desktop-browser-contracts/validation";
import { DesktopControlServer } from "./desktop-control-server.js";
import type { AutomationWorker } from "./automation-worker.js";
import type { BrowserSurfaceEvent, BrowserSurfaceManager } from "./browser-surface-manager.js";

const binding: BrowserSurfaceBinding = {
  desktop_epoch: "desktop-test",
  window_id: "window-test",
  surface_id: "surface-test",
  surface_revision: 1,
  tab_id: "tab-test",
  web_contents_id: 1,
  target_id: "target-test",
  browser_context_id: "partition-test",
  navigation_revision: 1,
};

const handshake: DesktopBrowserHandshake = {
  protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
  desktop_version: "test",
  electron_version: "test",
  chromium_version: "test",
  process_id: process.pid,
  desktop_epoch: binding.desktop_epoch,
  worker_epoch: "worker-test",
};
let socketSequence = 0;

test("并发 start/close 通过单一生命周期队列收口为一个控制端点", async () => {
  const worker = {
    execute: async () => failedOutcome("unused"),
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);

  await Promise.all([server.start(), server.start()]);
  await Promise.all([server.close(), server.close()]);
  assert.equal(existsSync(socketPath), false);

  await server.start();
  assert.equal(existsSync(socketPath), true);
  await server.close();
  assert.equal(existsSync(socketPath), false);
});

test("只有 Control WebSocket 建立后才发布 Host 可执行状态", async () => {
  const worker = {
    execute: async () => failedOutcome("unused"),
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const states: boolean[] = [];
  const { server, socketPath } = createControlServer(worker, {
    onConnectionState: (connected) => states.push(connected),
  });
  await server.start();
  const client = await connect(socketPath);
  try {
    assert.deepEqual(states, [true]);
  } finally {
    await closeSocket(client);
    await waitUntil(() => states.length === 2);
    assert.deepEqual(states, [true, false]);
    await server.close();
  }
});

test("控制连接断开只释放浏览器自动化控制态，不关闭逻辑 Tab", async () => {
  const worker = {
    execute: async () => failedOutcome("unused"),
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  let releaseCount = 0;
  const { server, socketPath } = createControlServer(worker, {
    onHostControlReleased: () => { releaseCount += 1; },
  });
  await server.start();
  const client = await connect(socketPath);
  try {
    await closeSocket(client);
    await waitUntil(() => releaseCount === 1);
    assert.equal(releaseCount, 1);
  } finally {
    await server.close();
  }
});

test("primary_changed 未完成 Worker 重绑前，同一 Tab 的命令不得执行", async () => {
  const rebind = deferred<void>();
  const calls: BrowserHostCommand[] = [];
  const worker = {
    execute: async (_binding: BrowserSurfaceBinding, command: BrowserHostCommand) => {
      calls.push(command);
      return failedOutcome("unused");
    },
    forwardSurfaceEvent: (event: BrowserSurfaceEvent) => (
      event.type === "primary_changed" ? rebind.promise : Promise.resolve()
    ),
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);
  await server.start();
  const client = await connect(socketPath);
  try {
    server.handleSurfaceEvent({ type: "primary_changed", binding });
    const responsePromise = nextJsonMatching(client, (message) => message.request_id === "rebind-gated");
    client.send(JSON.stringify(request("rebind-gated", snapshotCommand())));
    await new Promise((resolve) => setTimeout(resolve, 20));
    assert.equal(calls.length, 0);

    rebind.resolve();
    await responsePromise;
    assert.equal(calls.length, 1);
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

test("连接断开后同一资源队列等待旧底层操作收口", async () => {
  const firstOperation = deferred<{ outcome: BrowserCommandOutcome }>();
  const calls: BrowserHostCommand[] = [];
  const worker = {
    execute: async (_binding: BrowserSurfaceBinding, command: BrowserHostCommand) => {
      calls.push(command);
      if (calls.length === 1) return firstOperation.promise;
      return failedOutcome("second-command");
    },
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);
  await server.start();
  const first = await connect(socketPath);
  try {
    first.send(JSON.stringify(request("first", snapshotCommand())));
    await waitUntil(() => calls.length === 1);
    await closeSocket(first);

    const second = await connect(socketPath);
    try {
      second.send(JSON.stringify(request("second", snapshotCommand())));
      await new Promise((resolve) => setTimeout(resolve, 20));
      assert.equal(calls.length, 1, "旧 Worker Promise 未完成时不得启动新同资源命令");
      firstOperation.resolve(failedOutcome("late-first"));
      const response = await nextJson(second);
      assert.equal(response.request_id, "second");
      const outcome = response.outcome;
      assert.ok(outcome);
      assert.equal(outcome.status, "failed");
      assert.equal(calls.length, 2, "新连接的同资源命令不应等待旧 Worker Promise");
    } finally {
      await closeSocket(second);
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  } finally {
    await server.close();
  }
});

test("取消后的底层操作完成时不会向连接发送迟到的成功响应", async () => {
  const operation = deferred<{ outcome: BrowserCommandOutcome }>();
  const worker = {
    execute: async () => operation.promise,
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);
  await server.start();
  const client = await connect(socketPath);
  try {
    client.send(JSON.stringify(request("command", snapshotCommand())));
    client.send(JSON.stringify(request("cancel", { type: "cancel", payload: { request_id: "command" } })));
    const response = await nextJson(client);
    assert.equal(response.request_id, "command");
    const outcome = response.outcome;
    assert.ok(outcome);
    assert.equal(outcome.status, "indeterminate");

    operation.resolve({
      outcome: {
        status: "succeeded",
        payload: { type: "json", payload: { value: "late-success" } },
      },
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    assert.deepEqual(await receivedJson(client), [], "取消后的成功结果不得迟到回写");
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

test("旧连接处于 CLOSING 时，新连接可以完成握手", async () => {
  const worker = {
    execute: async () => failedOutcome("unused"),
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);
  await server.start();
  const first = await connect(socketPath);
  try {
    first.close();
    const second = await connect(socketPath);
    try {
      assert.equal(second.readyState, WebSocket.OPEN);
    } finally {
      await closeSocket(second);
    }
  } finally {
    await server.close();
  }
});

test("Desktop、Worker 和 Renderer 协议拒绝未知字段与错误 discriminator", () => {
  assert.throws(
    () => parseBrowserHostRequest({
      request_id: "request-1",
      protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
      command: { type: "ping" },
      extra: true,
    }),
    /browser_protocol_invalid/u,
  );
  assert.throws(
    () => parseBrowserHostRequest({
      request_id: "request-1",
      protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
      command: { type: "unknown_command" },
    }),
    /browser_protocol_invalid/u,
  );
  assert.throws(
    () => parseWorkerToMainMessage({
      type: "worker_ready",
      worker_epoch: "worker-1",
      protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
      extra: true,
    }),
    /worker_ipc_invalid/u,
  );
  assert.throws(
    () => assertDesktopIpcMessage("magi-desktop:activate-panel", {
      kind: "browser",
      tabId: "tab-1",
      extra: true,
    }),
    /desktop_ipc_invalid/u,
  );
});

test("Desktop Control Server 对 malformed 请求返回结构化协议错误", async () => {
  const worker = {
    execute: async () => failedOutcome("unused"),
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);
  await server.start();
  const client = await connect(socketPath);
  try {
    client.send(JSON.stringify({
      request_id: "malformed",
      protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
      command: { type: "ping" },
      unknown: true,
    }));
    const response = await nextJson(client);
    assert.equal(response.request_id, "invalid-request");
    assert.equal(response.outcome?.status, "failed");
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

test("不同 Browser Tab 使用独立资源队列，可以并行执行而不互相阻塞", async () => {
  const tabA = bindingForTab("tab-a");
  const tabB = bindingForTab("tab-b");
  const firstOperation = deferred<{ outcome: BrowserCommandOutcome }>();
  const started: string[] = [];
  const worker = {
    execute: async (commandBinding: BrowserSurfaceBinding) => {
      started.push(commandBinding.tab_id);
      if (commandBinding.tab_id === tabA.tab_id) return firstOperation.promise;
      return failedOutcome("tab-b-completed");
    },
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker, {
    bindings: [tabA, tabB],
    primaryTabId: tabA.tab_id,
  });
  await server.start();
  const client = await connect(socketPath);
  try {
    client.send(JSON.stringify(request("tab-a-request", snapshotCommandFor(tabA))));
    await waitUntil(() => started.length === 1);

    const tabBResponsePromise = nextJsonMatching(
      client,
      (message) => message.request_id === "tab-b-request",
    );
    client.send(JSON.stringify(request("tab-b-request", snapshotCommandFor(tabB))));
    await waitUntil(() => started.length === 2);
    assert.deepEqual(started, [tabA.tab_id, tabB.tab_id]);

    const tabBResponse = await tabBResponsePromise;
    assert.equal(tabBResponse.request_id, "tab-b-request");
    assert.equal(tabBResponse.outcome?.status, "failed");

    firstOperation.resolve(failedOutcome("tab-a-completed"));
    const tabAResponse = await nextJsonMatching(client, (message) => message.request_id === "tab-a-request");
    assert.equal(tabAResponse.request_id, "tab-a-request");
    assert.equal(tabAResponse.outcome?.status, "failed");
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

test("新建 Browser Page 只物化 WebContents，不等待可见内容槽", async () => {
  const worker = {
    execute: async () => failedOutcome("unused"),
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  let materializeCalled = false;
  let ensureCalled = false;
  const contentSlotGate = new Promise<void>(() => undefined);
  const newBinding = bindingForTab("tab-new");
  const { server, socketPath } = createControlServer(worker, {
    bindings: [newBinding],
    primaryTabId: newBinding.tab_id,
    materialize: async () => {
      materializeCalled = true;
      return newBinding;
    },
    recordForBinding: () => ({
      getURL: () => "https://example.test/",
      getTitle: () => "Example",
    }),
    ensureBrowserSurface: async () => {
      ensureCalled = true;
      await contentSlotGate;
    },
  });
  await server.start();
  const client = await connect(socketPath);
  try {
    const responsePromise = nextJsonMatching(
      client,
      (message) => message.request_id === "create-page",
    );
    client.send(JSON.stringify(request("create-page", createPageCommand(newBinding.tab_id))));
    const response = await Promise.race([
      responsePromise,
      new Promise<never>((_, reject) => {
        const timer = setTimeout(() => reject(new Error("创建 Browser Page 不应等待内容槽")), 500);
        timer.unref();
      }),
    ]);
    assert.equal(response.outcome?.status, "succeeded");
    assert.equal(materializeCalled, true);
    assert.equal(ensureCalled, false);
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

test("ensure_surface 在逻辑 Surface 尚未绑定 guest 时等待并返回真实 binding", async () => {
  const surface = bindingForTab("tab-await-surface");
  let bindingReady = false;
  let ensureInput: unknown = null;
  const worker = {
    execute: async () => failedOutcome("unused"),
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker, {
    bindings: [surface],
    primaryTabId: surface.tab_id,
    bindingAvailable: () => bindingReady,
    ensureBrowserSurface: async (input) => {
      ensureInput = input;
      bindingReady = true;
    },
  });
  await server.start();
  const client = await connect(socketPath);
  try {
    const responsePromise = nextJsonMatching(
      client,
      (message) => message.request_id === "ensure-surface",
    );
    client.send(JSON.stringify(request("ensure-surface", {
      type: "ensure_surface",
      payload: { tab_id: surface.tab_id },
    })));
    const response = await responsePromise;
    assert.equal(response.outcome?.status, "succeeded");
    const outcome = response.outcome as { status: string; payload?: unknown } | undefined;
    assert.deepEqual(outcome?.payload, {
      type: "surface_binding",
      payload: surface,
    });
    assert.deepEqual(ensureInput, {
      windowId: surface.window_id,
      tabId: surface.tab_id,
      browserSessionId: surface.browser_context_id,
      url: "https://example.test/",
      navigationRevision: surface.navigation_revision,
      viewport: { mode: "auto" },
    });
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

test("Host 重新连接时重放当前 Primary Surface，重启后自动化仍有权威 Tab 身份", async () => {
  const primary = bindingForTab("tab-primary");
  const background = bindingForTab("tab-background");
  const worker = {
    execute: async () => failedOutcome("unused"),
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker, {
    bindings: [primary, background],
    primaryTabId: primary.tab_id,
  });
  await server.start();
  const connected = await connectWithInitialMessages(socketPath);
  const client = connected.client;
  try {
    const replay = connected.initialMessages.find((message) => (
      (message.event as { type?: string } | undefined)?.type === "primary_surface_changed"
    ));
    assert.ok(replay, "连接握手必须包含 Primary Surface 重放事件");
    assert.deepEqual(
      (replay.event as { payload?: { binding?: BrowserSurfaceBinding } }).payload?.binding,
      primary,
      "重连只应重放当前 Primary，不能把后台 Tab 当成可见 Surface",
    );
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

test("持久化标记在页面刷新后按最后一次 Authority 投影重放", async () => {
  const calls: Array<{ binding: BrowserSurfaceBinding; command: BrowserHostCommand }> = [];
  const worker = {
    execute: async (targetBinding: BrowserSurfaceBinding, command: BrowserHostCommand) => {
      calls.push({ binding: targetBinding, command });
      return {
        outcome: {
          status: "succeeded" as const,
          payload: { type: "json" as const, payload: { value: { rendered: 1 } } },
        },
      };
    },
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);
  await server.start();
  const client = await connect(socketPath);
  try {
    const annotations = [{ annotation_id: "annotation-1", sequence: 1, status: "active" }];
    const responsePromise = nextJsonMatching(
      client,
      (message) => message.request_id === "set-annotations",
    );
    client.send(JSON.stringify(request("set-annotations", {
      type: "set_annotations",
      payload: { tab_id: binding.tab_id, annotations },
    })));
    const response = await responsePromise;
    assert.equal(response.outcome?.status, "succeeded");
    assert.equal(calls.length, 1);

    const refreshedBinding = { ...binding, navigation_revision: binding.navigation_revision + 1 };
    server.handleSurfaceEvent({
      type: "page_updated",
      binding: refreshedBinding,
      page: {
        tab_id: binding.tab_id,
        url: "https://example.test/after-refresh",
        origin: "https://example.test",
        title: "After refresh",
        navigation_revision: refreshedBinding.navigation_revision,
      },
    } as BrowserSurfaceEvent);
    await waitUntil(() => calls.length === 2);
    assert.deepEqual(calls[1], {
      binding,
      command: {
        type: "set_annotations",
        payload: { tab_id: binding.tab_id, annotations },
      },
    });
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

test("文档就绪信号会重放刷新后尚未落地的标记投影", async () => {
  const calls: BrowserHostCommand[] = [];
  let attempts = 0;
  const worker = {
    execute: async (_targetBinding: BrowserSurfaceBinding, command: BrowserHostCommand) => {
      calls.push(command);
      attempts += 1;
      if (attempts === 1) return failedOutcome("browser_page_runtime_not_ready");
      return {
        outcome: {
          status: "succeeded" as const,
          payload: { type: "empty" as const },
        },
      };
    },
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);
  await server.start();
  const client = await connect(socketPath);
  try {
    const annotations = [{ annotation_id: "annotation-ready", sequence: 1, status: "active" }];
    client.send(JSON.stringify(request("set-annotations-ready", {
      type: "set_annotations",
      payload: { tab_id: binding.tab_id, annotations },
    })));
    await waitUntil(() => calls.length === 1);
    server.handleSurfaceDocumentReady(binding);
    await waitUntil(() => calls.length === 2);
    assert.deepEqual(calls[1], {
      type: "set_annotations",
      payload: { tab_id: binding.tab_id, annotations },
    });
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

test("标记投影按 Tab 合并最新 revision，不取消正在执行的 Chromium 请求", async () => {
  const firstOperation = deferred<{ outcome: BrowserCommandOutcome }>();
  const calls: BrowserHostCommand[] = [];
  const worker = {
    execute: async (_binding: BrowserSurfaceBinding, command: BrowserHostCommand) => {
      calls.push(command);
      if (calls.length === 1) return firstOperation.promise;
      return {
        outcome: {
          status: "succeeded" as const,
          payload: { type: "empty" as const },
        },
      };
    },
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);
  await server.start();
  const client = await connect(socketPath);
  try {
    const responsePromise = new Promise<Map<string, { request_id?: string; outcome?: { status: string }; event?: unknown }>>((resolve) => {
      const responses = new Map<string, { request_id?: string; outcome?: { status: string }; event?: unknown }>();
      const onMessage = (data: RawData) => {
        const message = JSON.parse(data.toString()) as { request_id?: string; outcome?: { status: string }; event?: unknown };
        if (message.request_id === "set-annotations-1" || message.request_id === "set-annotations-2") {
          responses.set(message.request_id, message);
        }
        if (responses.size === 2) {
          client.off("message", onMessage);
          resolve(responses);
        }
      };
      client.on("message", onMessage);
    });
    client.send(JSON.stringify(request("set-annotations-1", {
      type: "set_annotations",
      payload: {
        tab_id: binding.tab_id,
        annotations: [{ annotation_id: "annotation-1", sequence: 1, status: "active" }],
      },
    })));
    await waitUntil(() => calls.length === 1);

    client.send(JSON.stringify(request("set-annotations-2", {
      type: "set_annotations",
      payload: {
        tab_id: binding.tab_id,
        annotations: [
          { annotation_id: "annotation-1", sequence: 1, status: "active" },
          { annotation_id: "annotation-2", sequence: 2, status: "active" },
        ],
      },
    })));
    firstOperation.resolve({
      outcome: {
        status: "succeeded",
        payload: { type: "empty" },
      },
    });

    const responses = await responsePromise;
    const firstResponse = responses.get("set-annotations-1");
    const secondResponse = responses.get("set-annotations-2");
    assert.ok(firstResponse);
    assert.ok(secondResponse);
    assert.equal(firstResponse.outcome?.status, "succeeded");
    assert.equal(secondResponse.outcome?.status, "succeeded");
    assert.equal(calls.length, 2, "连续更新只能向 Chromium 投影首个和最新 revision");
    assert.deepEqual(
      (calls[1] as Extract<BrowserHostCommand, { type: "set_annotations" }>).payload.annotations,
      [
        { annotation_id: "annotation-1", sequence: 1, status: "active" },
        { annotation_id: "annotation-2", sequence: 2, status: "active" },
      ],
    );
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

test("标记投影执行缓慢时不占用普通浏览器命令队列", async () => {
  const annotationOperation = deferred<{ outcome: BrowserCommandOutcome }>();
  const calls: BrowserHostCommand[] = [];
  const worker = {
    execute: async (_binding: BrowserSurfaceBinding, command: BrowserHostCommand) => {
      calls.push(command);
      if (command.type === "set_annotations") return annotationOperation.promise;
      return {
        outcome: {
          status: "succeeded" as const,
          payload: { type: "empty" as const },
        },
      };
    },
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);
  await server.start();
  const client = await connect(socketPath);
  try {
    client.send(JSON.stringify(request("set-annotations-slow", {
      type: "set_annotations",
      payload: {
        tab_id: binding.tab_id,
        annotations: [{ annotation_id: "annotation-1", sequence: 1, status: "active" }],
      },
    })));
    await waitUntil(() => calls.some((command) => command.type === "set_annotations"));

    const commandResponse = nextJsonMatching(client, (message) => message.request_id === "snapshot-fast");
    client.send(JSON.stringify(request("snapshot-fast", snapshotCommand())));
    const response = await commandResponse;
    assert.equal(response.outcome?.status, "succeeded");
    assert.equal(calls.filter((command) => command.type === "snapshot").length, 1);

    annotationOperation.resolve({
      outcome: {
        status: "succeeded",
        payload: { type: "empty" },
      },
    });
  } finally {
    annotationOperation.resolve({
      outcome: {
        status: "succeeded",
        payload: { type: "empty" },
      },
    });
    await closeSocket(client);
    await server.close();
  }
});

test("停止导航绕过同一 Tab 的长导航队列并立即进入 SurfaceManager", async () => {
  const navigation = deferred<{ tab_id: string; url: string; origin: string; title: string; navigation_revision: number }>();
  let navigateStarted = false;
  let stopCalled = false;
  const worker = {
    execute: async () => failedOutcome("unused"),
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker, {
    navigate: async () => {
      navigateStarted = true;
      return navigation.promise;
    },
    stopNavigation: async () => {
      stopCalled = true;
      return {
        tab_id: binding.tab_id,
        url: "https://example.test/previous",
        origin: "https://example.test",
        title: "Previous",
        navigation_revision: binding.navigation_revision + 1,
      };
    },
  });
  await server.start();
  const client = await connect(socketPath);
  try {
    client.send(JSON.stringify(request("navigate-long", {
      type: "navigate",
      payload: {
        tab_id: binding.tab_id,
        control: { mode: "user", fence: 0 },
        navigation: { action: "url", url: "https://example.test/slow" },
      },
    })));
    await waitUntil(() => navigateStarted);
    const stopResponsePromise = nextJsonMatching(client, (message) => message.request_id === "stop-fast");
    client.send(JSON.stringify(request("stop-fast", {
      type: "stop_navigation",
      payload: { tab_id: binding.tab_id },
    })));
    const stopResponse = await stopResponsePromise;
    assert.equal(stopResponse.outcome?.status, "succeeded");
    assert.equal(stopCalled, true, "Stop 必须在长导航完成前进入 SurfaceManager");

    navigation.resolve({
      tab_id: binding.tab_id,
      url: "https://example.test/slow",
      origin: "https://example.test",
      title: "Slow",
      navigation_revision: binding.navigation_revision + 1,
    });
    const navigateResponse = await nextJsonMatching(client, (message) => message.request_id === "navigate-long");
    assert.equal(navigateResponse.outcome?.status, "succeeded");
  } finally {
    navigation.resolve({
      tab_id: binding.tab_id,
      url: "https://example.test/slow",
      origin: "https://example.test",
      title: "Slow",
      navigation_revision: binding.navigation_revision + 1,
    });
    await closeSocket(client);
    await server.close();
  }
});

test("规范化后的 Chromium DOM.nodeId=null 可以完整进入节点选择消息", async () => {
  const worker = {
    execute: async () => failedOutcome("unused"),
    forwardSurfaceEvent: () => undefined,
  } as unknown as AutomationWorker;
  const { server, socketPath } = createControlServer(worker);
  await server.start();
  const client = await connect(socketPath);
  try {
    const selectionPromise = nextJsonMatching(
      client,
      (message) => (message.event as { type?: string } | undefined)?.type === "node_selection",
    );
    server.handleSurfaceEvent({
      type: "node_inspected",
      binding,
      node: {
        browser_session_id: "browser-session-test",
        backend_node_id: 42,
        node_id: normalizeOptionalDomNodeId(0),
        frame_id: null,
        node_type: 1,
        node_name: "BUTTON",
        local_name: "button",
        node_value: "提交",
        child_node_count: 0,
        document_url: "https://example.test/",
        attributes: { class: "primary" },
        outer_html: "<button class=\"primary\">提交</button>",
        outer_html_truncated: false,
        bounds: null,
        page_url: "https://example.test/",
        page_title: "Example",
      },
    } satisfies BrowserSurfaceEvent);
    const message = await selectionPromise;
    assert.equal(
      (message.event as { payload?: { dom_node_id?: number | null } }).payload?.dom_node_id,
      null,
    );
  } finally {
    await closeSocket(client);
    await server.close();
  }
});

function createControlServer(
  worker: AutomationWorker,
  options: {
    bindings?: BrowserSurfaceBinding[];
    primaryTabId?: string;
    bindingAvailable?: () => boolean;
    materialize?: (input: unknown) => Promise<BrowserSurfaceBinding>;
    recordForBinding?: () => { getURL: () => string; getTitle: () => string };
    navigate?: (binding: BrowserSurfaceBinding, navigation: unknown) => Promise<unknown>;
    stopNavigation?: (binding: BrowserSurfaceBinding) => Promise<unknown>;
    ensureBrowserSurface?: (input: unknown) => Promise<void>;
    onConnectionState?: (connected: boolean) => void;
    onHostControlReleased?: () => void;
  } = {},
): { server: DesktopControlServer; socketPath: string } {
  // macOS limits Unix-domain socket paths to a little over 100 bytes. The
  // system temp directory is already long enough that a UUID-based name can
  // exceed that limit, so keep the test socket basename deliberately short.
  const socketRoot = process.platform === "win32" ? tmpdir() : "/tmp";
  const socketPath = join(socketRoot, `magi-dc-${process.pid}-${socketSequence++}.sock`);
  const bindings = options.bindings ?? [binding];
  let primaryTabId = options.primaryTabId ?? binding.tab_id;
  const surfaceManager = {
    primaryBindingForTab: (tabId: string) => (
      options.bindingAvailable?.() !== false
        ? bindings.find((candidate) => candidate.tab_id === tabId && candidate.tab_id === primaryTabId) ?? null
        : null
    ),
    activationInputForTab: (tabId: string) => {
      const candidate = bindings.find((item) => item.tab_id === tabId);
      if (!candidate) return null;
      return {
        windowId: candidate.window_id,
        tabId: candidate.tab_id,
        browserSessionId: candidate.browser_context_id,
        url: "https://example.test/",
        navigationRevision: candidate.navigation_revision,
        viewport: { mode: "auto" as const },
      };
    },
    isContentSlotBoundBinding: (candidate: BrowserSurfaceBinding) => (
      options.bindingAvailable?.() !== false && candidate.tab_id === primaryTabId
    ),
    bindings: () => bindings,
    isPrimary: (candidate: BrowserSurfaceBinding) => candidate.tab_id === primaryTabId,
    releaseDisconnectedHostControl: () => options.onHostControlReleased?.(),
    materialize: options.materialize ?? (async () => binding),
    recordForBinding: options.recordForBinding ?? (() => ({
      getURL: () => "https://example.test/",
      getTitle: () => "Example",
    })),
    navigate: options.navigate ?? (async () => ({
      tab_id: binding.tab_id,
      url: "https://example.test/",
      origin: "https://example.test",
      title: "Example",
      navigation_revision: binding.navigation_revision,
    })),
    stopNavigation: options.stopNavigation ?? (async () => ({
      tab_id: binding.tab_id,
      url: "https://example.test/",
      origin: "https://example.test",
      title: "Example",
      navigation_revision: binding.navigation_revision,
    })),
  } as unknown as BrowserSurfaceManager;
  const server = new DesktopControlServer({
    socketPath,
    token: "desktop-test-token",
    surfaceManager,
    worker,
    waitForActiveWindow: async () => binding.window_id,
    ensureBrowserSurface: async (input) => {
      if (options.ensureBrowserSurface) {
        await options.ensureBrowserSurface(input);
        return;
      }
      primaryTabId = input.tabId;
    },
    handshake: () => handshake,
    ...(options.onConnectionState ? { onConnectionState: options.onConnectionState } : {}),
  });
  return { server, socketPath };
}

async function connect(socketPath: string): Promise<WebSocket> {
  const connected = await connectWithInitialMessages(socketPath);
  return connected.client;
}

async function connectWithInitialMessages(socketPath: string): Promise<{
  client: WebSocket;
  initialMessages: Array<{ request_id?: string; outcome?: { status: string }; event?: unknown }>;
}> {
  const client = new WebSocket(`ws+unix:${socketPath}:/control`, {
    headers: { authorization: "Bearer desktop-test-token" },
  });
  const initialMessages: Array<{ request_id?: string; outcome?: { status: string }; event?: unknown }> = [];
  await new Promise<void>((resolve, reject) => {
    const onMessage = (data: RawData) => {
      initialMessages.push(JSON.parse(data.toString()) as {
        request_id?: string;
        outcome?: { status: string };
        event?: unknown;
      });
      if (initialMessages.length >= 2) {
        cleanup();
        resolve();
      }
    };
    const onOpen = () => {
      // 初始事件可能在 open 回调之前已经由服务端发出，消息监听器必须
      // 在构造 WebSocket 后立即注册，不能等到 open 之后再安装。
    };
    const onError = (error: Error) => {
      cleanup();
      reject(error);
    };
    const cleanup = () => {
      client.off("message", onMessage);
      client.off("open", onOpen);
      client.off("error", onError);
    };
    client.on("message", onMessage);
    client.once("open", onOpen);
    client.once("error", onError);
  });
  return { client, initialMessages };
}

function request(requestId: string, command: BrowserHostCommand): BrowserHostRequestEnvelope {
  return {
    request_id: requestId,
    protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
    command,
  };
}

function snapshotCommand(): BrowserHostCommand {
  return snapshotCommandFor(binding);
}

function createPageCommand(tabId: string): BrowserHostCommand {
  return {
    type: "create_page",
    payload: {
      tab_id: tabId,
      browser_session_id: "browser-session-test",
      initial_url: "https://example.test/",
      logical_viewport: { mode: "auto" },
      navigation_revision: 0,
      snapshot_revision: 0,
      allow_page_eviction: false,
    },
  };
}

function snapshotCommandFor(target: BrowserSurfaceBinding): BrowserHostCommand {
  return {
    type: "snapshot",
    payload: {
      tab_id: target.tab_id,
      navigation_revision: target.navigation_revision,
      snapshot_revision: 1,
      limits: { max_nodes: 10, max_text_bytes: 1_000 },
    },
  };
}

function bindingForTab(tabId: string): BrowserSurfaceBinding {
  return {
    ...binding,
    surface_id: `surface-${tabId}`,
    tab_id: tabId,
    target_id: `target-${tabId}`,
  };
}

function failedOutcome(value: string): { outcome: BrowserCommandOutcome } {
  return {
    outcome: {
      status: "failed",
      payload: {
        code: "browser_test_failure",
        message: value,
        recoverable: true,
        side_effect_started: false,
        diagnostic: null,
      },
    },
  };
}

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

async function nextJson(client: WebSocket): Promise<{ request_id?: string; outcome?: { status: string }; event?: unknown }> {
  return new Promise((resolve, reject) => {
    const onMessage = (data: RawData) => {
      cleanup();
      resolve(JSON.parse(data.toString()) as { request_id?: string; outcome?: { status: string }; event?: unknown });
    };
    const onError = (error: Error) => {
      cleanup();
      reject(error);
    };
    const cleanup = () => {
      client.off("message", onMessage);
      client.off("error", onError);
    };
    client.on("message", onMessage);
    client.on("error", onError);
  });
}

async function nextJsonMatching(
  client: WebSocket,
  predicate: (message: { request_id?: string; outcome?: { status: string }; event?: unknown }) => boolean,
): Promise<{ request_id?: string; outcome?: { status: string }; event?: unknown }> {
  for (;;) {
    const message = await nextJson(client);
    if (predicate(message)) return message;
  }
}

async function receivedJson(client: WebSocket): Promise<unknown[]> {
  const messages: unknown[] = [];
  const onMessage = (data: RawData) => messages.push(JSON.parse(data.toString()));
  client.on("message", onMessage);
  await new Promise((resolve) => setTimeout(resolve, 10));
  client.off("message", onMessage);
  return messages;
}

async function closeSocket(client: WebSocket): Promise<void> {
  if (client.readyState === WebSocket.CLOSED) return;
  await new Promise<void>((resolve) => {
    client.once("close", () => resolve());
    client.close();
  });
}

async function waitUntil(predicate: () => boolean): Promise<void> {
  const deadline = Date.now() + 2_000;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  assert.fail("等待条件超时");
}
