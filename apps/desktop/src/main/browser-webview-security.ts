import type { WebPreferences } from "electron";

const BROWSER_PARTITION_PATTERN = /^magi-browser-[A-Za-z0-9._-]+$/u;

export interface BrowserWebviewAttachmentDecision {
  allowed: boolean;
  reason: "browser_webview_initial_url_invalid" | "browser_webview_partition_invalid" | null;
}

/**
 * 在 Electron 创建 guest 前收敛安全参数。逻辑 Surface 身份仍在 guest
 * 创建后的注册阶段校验，这里只允许 Magi 的隔离 partition 和空白初始页。
 */
export function secureBrowserWebviewAttachment(
  webPreferences: WebPreferences,
  params: Record<string, string>,
): BrowserWebviewAttachmentDecision {
  if (params.src !== "about:blank") {
    return { allowed: false, reason: "browser_webview_initial_url_invalid" };
  }
  if (!BROWSER_PARTITION_PATTERN.test(params.partition ?? "")) {
    return { allowed: false, reason: "browser_webview_partition_invalid" };
  }

  delete params.preload;
  delete params.webpreferences;
  delete webPreferences.preload;
  webPreferences.nodeIntegration = false;
  webPreferences.nodeIntegrationInWorker = false;
  webPreferences.nodeIntegrationInSubFrames = false;
  webPreferences.contextIsolation = true;
  webPreferences.sandbox = true;
  webPreferences.webSecurity = true;
  webPreferences.allowRunningInsecureContent = false;
  webPreferences.webviewTag = false;
  webPreferences.focusOnNavigation = false;
  webPreferences.navigateOnDragDrop = false;

  return { allowed: true, reason: null };
}
