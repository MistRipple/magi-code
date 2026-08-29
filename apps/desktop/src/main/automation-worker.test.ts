import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import {
  DESKTOP_BROWSER_PROTOCOL_VERSION,
  type MainToWorkerMessage,
  type BrowserSurfaceBinding,
} from "@magi/desktop-browser-contracts";
import type { UtilityProcess } from "electron";
import type { BrowserSurfaceManager } from "./browser-surface-manager.js";
import { AutomationWorker } from "./automation-worker.js";

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

class FakeUtilityProcess extends EventEmitter {
  readonly messages: MainToWorkerMessage[] = [];
  readonly stdout = null;
  readonly stderr = null;
  killed = false;

  constructor(private readonly failOnRebind = false) {
    super();
  }

  kill(): boolean {
    this.killed = true;
    queueMicrotask(() => this.emit("exit", null));
    return true;
  }

  postMessage(message: unknown): void {
    const typedMessage = message as MainToWorkerMessage;
    if (this.failOnRebind && typedMessage.type === "worker_rebind") {
      throw new Error("worker_rebind_failed");
    }
    this.messages.push(typedMessage);
    if (typedMessage.type === "worker_rebind") {
      queueMicrotask(() => this.emit("message", {
        type: "worker_rebind_ack",
        worker_epoch: typedMessage.worker_epoch,
        rebind_id: typedMessage.rebind_id,
        binding_count: typedMessage.bindings.length,
      }));
    }
  }

  emitReady(workerEpoch: string): void {
    this.emit("message", {
      type: "worker_ready",
      worker_epoch: workerEpoch,
      protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
    });
  }

  emitResult(callId: string): void {
    this.emit("message", {
      type: "worker_result",
      call_id: callId,
      binding,
      outcome: { status: "succeeded", payload: { type: "empty" } },
    });
  }
}

test("正常 ready 完成重绑并释放命令 waiter", async () => {
  const child = new FakeUtilityProcess();
  const worker = createWorker(() => child);

  const execution = worker.execute(binding, { type: "ping" });
  child.emitReady(worker.workerEpoch);
  await waitFor(() => child.messages.some((message) => message.type === "worker_command"));
  const command = child.messages.find((message) => message.type === "worker_command");
  assert.ok(command && command.type === "worker_command");
  child.emitResult(command.call_id);

  const result = await execution;
  assert.equal(result.outcome.status, "succeeded");
  assert.equal(worker.status, "ready");
  assert.ok(child.messages.some((message) => message.type === "worker_rebind"));
  assert.equal(child.killed, false);
  await worker.stop();
});

test("start 只会在 Worker 握手和重绑完成后返回", async () => {
  const child = new FakeUtilityProcess();
  const worker = createWorker(() => child);

  let settled = false;
  const starting = worker.start().then(() => {
    settled = true;
  });
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(settled, false);
  child.emitReady(worker.workerEpoch);
  await starting;

  assert.equal(worker.status, "ready");
  assert.ok(child.messages.some((message) => message.type === "worker_rebind"));
  await worker.stop();
});

test("恢复注册只会在新 Worker 完成 Surface 重绑后执行", async () => {
  const child = new FakeUtilityProcess();
  let rebindVisibleWhenReady = false;
  const worker = createWorker(() => child, async () => {
    rebindVisibleWhenReady = child.messages.some((message) => message.type === "worker_rebind");
  });

  const starting = worker.start();
  child.emitReady(worker.workerEpoch);
  await starting;

  assert.equal(rebindVisibleWhenReady, true);
  await worker.stop();
});

test("ready callback 失败会收口存活旧进程并继续重绑恢复", async () => {
  const children: FakeUtilityProcess[] = [];
  let readyCalls = 0;
  const worker = createWorker(() => {
    const child = new FakeUtilityProcess();
    children.push(child);
    return child;
  }, async () => {
    readyCalls += 1;
    if (readyCalls === 1) throw new Error("desktop_registration_failed");
  });

  const execution = worker.execute(binding, { type: "ping" });
  await waitFor(() => children.length === 1);
  children[0]?.emitReady(worker.workerEpoch);
  await assert.rejects(execution, /desktop_registration_failed/u);
  assert.equal(children[0]?.killed, true, "callback 失败后必须 kill 仍存活的旧 Worker");
  await waitFor(() => children.length === 2);

  const replacement = children[1];
  assert.ok(replacement);
  replacement.emitReady(worker.workerEpoch);
  await waitFor(() => worker.status === "ready");
  assert.equal(replacement.killed, false);
  assert.equal(readyCalls, 2);
  await worker.stop();
});

test("Worker 进程存活但 handshake 已 failed 时不会阻塞显式 restart", async () => {
  const children: FakeUtilityProcess[] = [];
  const worker = createWorker(() => {
    const child = new FakeUtilityProcess(children.length === 0);
    children.push(child);
    return child;
  }, async () => undefined);

  const execution = worker.execute(binding, { type: "ping" });
  await waitFor(() => children.length === 1);
  children[0]?.emitReady(worker.workerEpoch);
  await assert.rejects(execution, /worker_rebind_failed/u);
  assert.equal(worker.status, "restarting");
  assert.equal(children[0]?.killed, true);

  const restart = worker.restart();
  await waitFor(() => children.length === 2);
  children[1]?.emitReady(worker.workerEpoch);
  await restart;
  assert.equal(children.length, 2);
  assert.equal(worker.status, "ready");
  await worker.stop();
});

test("连续恢复失败后进入 failed 且不遗留可用性错误的 Worker 进程", async () => {
  const children: FakeUtilityProcess[] = [];
  const worker = createWorker(() => {
    const child = new FakeUtilityProcess();
    children.push(child);
    return child;
  }, async () => {
    throw new Error("registration_failed");
  });

  const execution = worker.execute(binding, { type: "ping" });
  await waitFor(() => children.length === 1);
  children[0]?.emitReady(worker.workerEpoch);
  await assert.rejects(execution, /registration_failed/u);

  for (let index = 0; index < 3; index += 1) {
    await waitFor(() => children.length === index + 2, 6_000);
    children[index + 1]?.emitReady(worker.workerEpoch);
  }
  await waitFor(() => worker.status === "failed", 6_000);
  assert.equal(children.length, 4, "达到最大恢复次数后不得再 fork 新 Worker");
  assert.equal(children.every((child) => child.killed), true);
  await worker.stop();
});

function createWorker(
  fork: () => FakeUtilityProcess,
  onReady?: () => Promise<void>,
): AutomationWorker {
  const surfaceManager = {
    bindings: () => [],
    updateControl: async () => undefined,
  } as unknown as BrowserSurfaceManager;
  return new AutomationWorker({
    entryPath: "worker-test-entry",
    surfaceManager,
    fork: () => fork() as unknown as UtilityProcess,
    ...(onReady ? { onReady } : {}),
  });
}

async function waitFor(predicate: () => boolean, timeoutMs = 2_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!predicate()) {
    if (Date.now() >= deadline) throw new Error("test_wait_timeout");
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}
