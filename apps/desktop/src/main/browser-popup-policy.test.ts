import assert from "node:assert/strict";
import test from "node:test";
import type { HandlerDetails } from "electron";
import { decideBrowserPopup } from "./browser-popup-policy.js";

function details(
  overrides: Partial<HandlerDetails> = {},
): HandlerDetails {
  return {
    url: "https://example.com/next",
    frameName: "_blank",
    features: "",
    disposition: "foreground-tab",
    referrer: { url: "https://example.com/", policy: "default" },
    ...overrides,
  };
}

test("普通 _blank 链接复用当前页面", () => {
  assert.deepEqual(decideBrowserPopup(details()), {
    action: "navigate_current_page",
    url: "https://example.com/next",
  });
});

test("Electron 将 _blank 链接 frameName 归一化为空时仍按点击 disposition 复用当前页", () => {
  assert.deepEqual(
    decideBrowserPopup(details({ frameName: "", disposition: "foreground-tab" })),
    { action: "navigate_current_page", url: "https://example.com/next" },
  );
});

test("_blank POST 表单保留请求语义并复用当前页面", () => {
  assert.deepEqual(
    decideBrowserPopup(
      details({
        disposition: "default",
        postBody: {
          contentType: "application/x-www-form-urlencoded",
          data: [{ type: "rawData", bytes: Buffer.from("query=magi") }],
        },
      }),
    ),
    { action: "navigate_current_page", url: "https://example.com/next" },
  );
});

test("显式 noopener 的脚本窗口允许当前页导航", () => {
  assert.deepEqual(
    decideBrowserPopup(
      details({ frameName: "", features: "noopener,noreferrer", disposition: "default" }),
    ),
    { action: "navigate_current_page", url: "https://example.com/next" },
  );
});

test("需要 opener 的脚本窗口不能伪装为当前页成功", () => {
  assert.deepEqual(
    decideBrowserPopup(
      details({ frameName: "", disposition: "default" }),
    ),
    { action: "block", reason: "opener_required" },
  );
});

test("命名窗口和独立窗口特性被明确阻止", () => {
  assert.deepEqual(
    decideBrowserPopup(details({ frameName: "oauth" })),
    { action: "block", reason: "named_window" },
  );
  assert.deepEqual(
    decideBrowserPopup(details({ features: "popup,width=640,height=720" })),
    { action: "block", reason: "separate_window_features" },
  );
});

test("空白脚本窗口、非 HTTP 协议和无效 URL 使用不同原因", () => {
  assert.deepEqual(decideBrowserPopup(details({ url: "about:blank" })), {
    action: "block",
    reason: "script_blank_window",
  });
  assert.deepEqual(decideBrowserPopup(details({ url: "mailto:test@example.com" })), {
    action: "block",
    reason: "unsupported_protocol",
  });
  assert.deepEqual(decideBrowserPopup(details({ url: "http://[" })), {
    action: "block",
    reason: "invalid_url",
  });
});
