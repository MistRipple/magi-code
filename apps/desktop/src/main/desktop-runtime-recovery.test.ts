import assert from "node:assert/strict";
import test from "node:test";
import { DesktopRuntimeRecoveryCoordinator } from "./desktop-runtime-recovery.js";

test("Control、daemon 注册和窗口创建按任意顺序到齐后只恢复一次", async () => {
  for (const order of [
    ["control", "daemon", "window"],
    ["daemon", "control", "window"],
    ["window", "control", "daemon"],
  ]) {
    let windowAvailable = false;
    let restoreCalls = 0;
    let notifyCalls = 0;
    const coordinator = new DesktopRuntimeRecoveryCoordinator({
      hasWindow: () => windowAvailable,
      waitForHostReady: async () => true,
      restoreAfterDaemonReady: async () => { restoreCalls += 1; },
      notifyBrowserRuntimeReady: () => { notifyCalls += 1; },
      onFailure: (cause) => { throw cause; },
    });

    for (const event of order) {
      if (event === "control") coordinator.onControlConnection(true);
      if (event === "daemon") coordinator.onDaemonRegistered(7);
      if (event === "window") {
        windowAvailable = true;
        coordinator.onWindowAvailable();
      }
    }
    await waitUntil(() => restoreCalls === 1);
    assert.equal(restoreCalls, 1, order.join(" -> "));
    assert.equal(notifyCalls, 1, order.join(" -> "));

    coordinator.onDaemonRegistered(7);
    coordinator.onWindowAvailable();
    await waitUntil(() => restoreCalls === 1);
    assert.equal(restoreCalls, 1, "同一连接代次不得重复恢复");
    coordinator.shutdown();
  }
});

test("没有窗口时不会发布 ready，窗口创建事件会触发唯一恢复事务", async () => {
  let windowAvailable = false;
  let waitCalls = 0;
  let notifyCalls = 0;
  const coordinator = new DesktopRuntimeRecoveryCoordinator({
    hasWindow: () => windowAvailable,
    waitForHostReady: async () => { waitCalls += 1; return true; },
    restoreAfterDaemonReady: async () => undefined,
    notifyBrowserRuntimeReady: () => { notifyCalls += 1; },
    onFailure: (cause) => { throw cause; },
  });

  coordinator.onControlConnection(true);
  coordinator.onDaemonRegistered(11);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(waitCalls, 0);

  windowAvailable = true;
  coordinator.onWindowAvailable();
  await waitUntil(() => notifyCalls === 1);
  assert.equal(waitCalls, 1);
  assert.equal(notifyCalls, 1);
  coordinator.shutdown();
});

test("连接断开会取消旧恢复，迟到的 host ready 不能通知 Renderer", async () => {
  let windowAvailable = true;
  let notifyCalls = 0;
  const coordinator = new DesktopRuntimeRecoveryCoordinator({
    hasWindow: () => windowAvailable,
    waitForHostReady: async (_revision, _generation, signal) => new Promise<boolean>((resolve) => {
      signal.addEventListener("abort", () => resolve(false), { once: true });
    }),
    restoreAfterDaemonReady: async () => undefined,
    notifyBrowserRuntimeReady: () => { notifyCalls += 1; },
    onFailure: (cause) => { throw cause; },
  });

  coordinator.onControlConnection(true);
  coordinator.onDaemonRegistered(13);
  await new Promise((resolve) => setImmediate(resolve));
  coordinator.onControlConnection(false);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(notifyCalls, 0);
  coordinator.shutdown();
});

async function waitUntil(predicate: () => boolean): Promise<void> {
  const deadline = Date.now() + 1_000;
  while (Date.now() < deadline) {
    if (predicate()) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail("等待恢复事务完成超时");
}
