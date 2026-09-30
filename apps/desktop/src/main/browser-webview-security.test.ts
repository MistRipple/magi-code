import assert from "node:assert/strict";
import test from "node:test";
import type { WebPreferences } from "electron";
import { secureBrowserWebviewAttachment } from "./browser-webview-security.js";

test("合法 Magi Browser guest 在创建前被强制收敛到安全参数", () => {
  const preferences: WebPreferences = {
    preload: "/tmp/untrusted-preload.cjs",
    nodeIntegration: true,
    nodeIntegrationInWorker: true,
    nodeIntegrationInSubFrames: true,
    contextIsolation: false,
    sandbox: false,
    webSecurity: false,
    allowRunningInsecureContent: true,
    webviewTag: true,
    focusOnNavigation: true,
    navigateOnDragDrop: true,
  };
  const params = {
    src: "about:blank",
    partition: "magi-browser-browser-session-1",
    preload: "file:///tmp/untrusted-preload.cjs",
    webpreferences: "nodeIntegration=yes",
  };

  assert.deepEqual(secureBrowserWebviewAttachment(preferences, params), {
    allowed: true,
    reason: null,
  });
  assert.equal("preload" in preferences, false);
  assert.equal("preload" in params, false);
  assert.equal("webpreferences" in params, false);
  assert.equal(preferences.nodeIntegration, false);
  assert.equal(preferences.nodeIntegrationInWorker, false);
  assert.equal(preferences.nodeIntegrationInSubFrames, false);
  assert.equal(preferences.contextIsolation, true);
  assert.equal(preferences.sandbox, true);
  assert.equal(preferences.webSecurity, true);
  assert.equal(preferences.allowRunningInsecureContent, false);
  assert.equal(preferences.webviewTag, false);
  assert.equal(preferences.focusOnNavigation, false);
  assert.equal(preferences.navigateOnDragDrop, false);
});

test("非 Magi partition 在 guest 创建前被拒绝", () => {
  const decision = secureBrowserWebviewAttachment(
    {},
    { src: "about:blank", partition: "persist:default" },
  );
  assert.deepEqual(decision, {
    allowed: false,
    reason: "browser_webview_partition_invalid",
  });
});

test("非空白初始导航在 guest 创建前被拒绝", () => {
  const decision = secureBrowserWebviewAttachment(
    {},
    {
      src: "https://example.com/",
      partition: "magi-browser-browser-session-1",
    },
  );
  assert.deepEqual(decision, {
    allowed: false,
    reason: "browser_webview_initial_url_invalid",
  });
});

test("应用级 GPT Web 固定持久分区被放行", () => {
  const decision = secureBrowserWebviewAttachment(
    {},
    { src: "about:blank", partition: "persist:magi-web-model" },
  );
  assert.deepEqual(decision, { allowed: true, reason: null });
});

test("其它 persist 分区仍然被拒绝", () => {
  for (const partition of ["persist:magi-web-model-2", "persist:web-model", "magi-web-model"]) {
    const decision = secureBrowserWebviewAttachment(
      {},
      { src: "about:blank", partition },
    );
    assert.deepEqual(
      decision,
      { allowed: false, reason: "browser_webview_partition_invalid" },
      `partition ${partition} 不应被放行`,
    );
  }
});
