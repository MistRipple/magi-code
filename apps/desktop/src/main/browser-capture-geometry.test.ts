import assert from "node:assert/strict";
import test from "node:test";
import { mapBrowserCaptureClipToNativeRect } from "./browser-capture-geometry.js";

test("auto 视口的页面 CSS 截图裁剪直接对应原生内容槽", () => {
  assert.deepEqual(
    mapBrowserCaptureClipToNativeRect(
      { width: 1000, height: 800 },
      { x: 250, y: 80, width: 500, height: 200 },
      1,
    ),
    { x: 250, y: 80, width: 500, height: 200 },
  );
});

test("fixed 响应式视口的标记截图按 Chromium compositor scale 映射到原生内容槽", () => {
  assert.deepEqual(
    mapBrowserCaptureClipToNativeRect(
      { width: 1000, height: 800 },
      { x: 500, y: 160, width: 1000, height: 400 },
      0.5,
    ),
    { x: 250, y: 80, width: 500, height: 200 },
  );
});

test("截图裁剪始终限制在当前原生内容槽内", () => {
  assert.deepEqual(
    mapBrowserCaptureClipToNativeRect(
      { width: 1000, height: 800 },
      { x: 1900, y: 1500, width: 200, height: 200 },
      0.5,
    ),
    { x: 950, y: 750, width: 50, height: 50 },
  );
});

test("截图几何拒绝无效尺寸，不伪造 1x1 结果", () => {
  assert.throws(
    () => mapBrowserCaptureClipToNativeRect({ width: 0, height: 800 }, null, 1),
    /browser_capture_dimension_invalid:width/u,
  );
  assert.throws(
    () => mapBrowserCaptureClipToNativeRect(
      { width: 1000, height: 800 },
      { x: 0, y: 0, width: 0, height: 200 },
      1,
    ),
    /browser_capture_dimension_invalid:clip\.width/u,
  );
  assert.throws(
    () => mapBrowserCaptureClipToNativeRect({ width: 1000, height: 800 }, null, 0),
    /browser_capture_scale_invalid/u,
  );
});
