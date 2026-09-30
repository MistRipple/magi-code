import type { WebPreferences } from "electron";
import { WEB_MODEL_PARTITION } from "./web-model-session.js";

const BROWSER_PARTITION_PATTERN = /^magi-browser-[A-Za-z0-9._-]+$/u;
/**
 * 应用级 GPT Web 会话的固定持久分区。
 *
 * Electron 中只有带 `persist:` 前缀的 partition 才会落盘；现有
 * `magi-browser-<id>` 是内存会话，进程退出即丢登录态（设计基线 A21）。
 * 该分区名与应用级 `browserSessionId` 无关，因此 id 变化不影响登录态。
 */
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
  const partition = params.partition ?? "";
  if (
    !BROWSER_PARTITION_PATTERN.test(partition) &&
    partition !== WEB_MODEL_PARTITION
  ) {
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
