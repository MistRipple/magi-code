import assert from "node:assert/strict";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { BrowserCursorPresentations } from "./browser-cursor-presentation.js";
import type { BrowserAgentCursorPresentation } from "@magi/desktop-browser-contracts";

const identity = { tab_id: "tab", surface_id: "surface", navigation_revision: 3 };

test("光标只接受所属窗口、Surface、导航和本次 motion 的完成确认", async () => {
  const gate = new BrowserCursorPresentations();
  let request!: BrowserAgentCursorPresentation;
  let done = false;
  const wait = gate.wait("window", identity, value => { request = value; }, () => {}).then(() => { done = true; });
  gate.complete("other", { ...request, outcome: "arrived" });
  gate.complete("window", { ...request, navigation_revision: 2, outcome: "arrived" });
  gate.complete("window", { ...request, motionId: "old", outcome: "arrived" });
  await delay(30);
  assert.equal(done, false);
  gate.complete("window", { ...request, outcome: "arrived" });
  await wait;
  assert.equal(done, true);
});

test("隐藏展示立即放行；动画中断不能放行点击", async () => {
  for (const outcome of ["hidden", "interrupted"] as const) {
    const gate = new BrowserCursorPresentations();
    const wait = gate.wait("window", identity, request => {
      gate.complete("window", { ...request, outcome });
    }, () => {});
    if (outcome === "hidden") await wait;
    else await assert.rejects(wait, /interrupted/);
  }
});

test("移动途中接管、导航及取消在 ACK 前终止，不等超时", async () => {
  for (const reason of ["browser_user_control_held", "browser_surface_stale"]) {
    const gate = new BrowserCursorPresentations();
    let valid = true;
    const wait = gate.wait("window", identity, () => {}, () => { if (!valid) throw Error(reason); });
    valid = false;
    await assert.rejects(wait, new RegExp(reason));
  }
  const abort = new AbortController();
  const gate = new BrowserCursorPresentations();
  const wait = gate.wait("window", identity, () => {}, () => {}, abort.signal);
  abort.abort();
  await assert.rejects(wait, /cancelled/);
});

test("展示确认超时拒绝动作，迟到确认不能把失败改成成功", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout", "setInterval"] });
  const gate = new BrowserCursorPresentations();
  let request!: BrowserAgentCursorPresentation;
  const wait = gate.wait("window", identity, value => { request = value; }, () => {});
  const rejected = assert.rejects(wait, /browser_cursor_presentation_timeout/);
  context.mock.timers.tick(3_000);
  await rejected;
  gate.complete("window", { ...request, outcome: "arrived" });
});
