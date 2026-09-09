import assert from "node:assert/strict";
import test from "node:test";
import { DesktopWindowReadiness } from "./desktop-window-readiness.js";

test("窗口创建前的多个等待者在同一次可用事件中收到同一窗口", async () => {
  const readiness = new DesktopWindowReadiness(1_000);
  const first = readiness.wait();
  const second = readiness.wait();
  readiness.markAvailable("window-1");
  assert.deepEqual(await Promise.all([first, second]), ["window-1", "window-1"]);
  assert.equal(readiness.state, "available");
});

test("窗口等待支持取消并解除监听器", async () => {
  const readiness = new DesktopWindowReadiness(1_000);
  const controller = new AbortController();
  const pending = readiness.wait(controller.signal);
  controller.abort();
  await assert.rejects(pending, /desktop_window_wait_cancelled/u);
  readiness.markAvailable("window-after-cancel");
  assert.equal(await readiness.wait(), "window-after-cancel");
});

test("窗口创建失败和应用关闭都会结束等待", async () => {
  const failed = new DesktopWindowReadiness(1_000);
  const failedWait = failed.wait();
  failed.markFailed(new Error("native_window_error"));
  await assert.rejects(failedWait, /desktop_window_creation_failed:native_window_error/u);
  await assert.rejects(failed.wait(), /desktop_window_creation_failed:native_window_error/u);

  const closed = new DesktopWindowReadiness(1_000);
  const closedWait = closed.wait();
  closed.markClosed();
  await assert.rejects(closedWait, /desktop_window_closed/u);
  await assert.rejects(closed.wait(), /desktop_window_closed/u);
});

test("窗口未在生命周期超时内创建时返回结构化未就绪错误", async () => {
  const readiness = new DesktopWindowReadiness(10);
  await assert.rejects(readiness.wait(), /desktop_window_not_ready/u);
  assert.equal(readiness.state, "waiting");
});
