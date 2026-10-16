import assert from "node:assert/strict";
import test from "node:test";
import { fitBrowserViewportScale, browserInputDisplayParams } from "./browser-viewport-scale.js";

test("宽屏设备画布按内容槽较短边完整缩小", () => {
  assert.equal(
    fitBrowserViewportScale(
      { width: 1280, height: 800 },
      { width: 480, height: 854 },
    ),
    0.375,
  );
  assert.equal(
    fitBrowserViewportScale(
      { width: 1280, height: 800 },
      { width: 480, height: 300 },
    ),
    0.375,
  );
});

test("内容槽足够大时不放大设备画布", () => {
  assert.equal(
    fitBrowserViewportScale(
      { width: 390, height: 844 },
      { width: 480, height: 854 },
    ),
    1,
  );
});

test("极端自定义视口仍按真实比例完整容纳", () => {
  assert.equal(
    fitBrowserViewportScale(
      { width: 7680, height: 4320 },
      { width: 320, height: 240 },
    ),
    0.041666,
  );
});

test("固定画布的原生鼠标输入与光标共享显示比例，键盘参数保持原语义", () => {
  const mouse = { type: "mousePressed", x: 640, y: 400, button: "left" };
  assert.deepEqual(browserInputDisplayParams("Input.dispatchMouseEvent", mouse, 0.375), {
    ...mouse, x: 240, y: 150,
  });
  assert.equal(browserInputDisplayParams("Input.dispatchMouseEvent", mouse, 1), mouse);
  const key = { type: "keyDown", key: "Enter" };
  assert.equal(browserInputDisplayParams("Input.dispatchKeyEvent", key, 0.375), key);
});
