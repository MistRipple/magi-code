import type { HandlerDetails } from "electron";
import { normalizeNavigableUrl } from "./browser-surface-url.js";

export type BrowserPopupBlockReason =
  | "invalid_url"
  | "unsupported_protocol"
  | "script_blank_window"
  | "named_window"
  | "opener_required"
  | "separate_window_features";

export type BrowserPopupDecision =
  | { action: "navigate_current_page"; url: string }
  | { action: "block"; reason: BrowserPopupBlockReason };

type BrowserPopupDetails = Pick<
  HandlerDetails,
  "url" | "frameName" | "features" | "disposition" | "postBody"
>;

const CURRENT_PAGE_SAFE_FEATURES = new Set(["noopener", "noreferrer"]);

/**
 * 将 Chromium 新窗口请求收敛为单页产品语义。
 *
 * HandlerDetails 不提供页面未来是否会访问 window.opener 的事实，因此
 * 脚本窗口只有显式声明 noopener/noreferrer 才能安全退化为当前页导航。
 * 命名窗口和尺寸/popup 特性都表示调用方需要独立窗口，必须明确阻止。
 */
export function decideBrowserPopup(
  details: BrowserPopupDetails,
): BrowserPopupDecision {
  const rawUrl = details.url.trim();
  if (!rawUrl) return { action: "block", reason: "invalid_url" };

  let parsedUrl: URL;
  try {
    parsedUrl = new URL(rawUrl);
  } catch {
    return { action: "block", reason: "invalid_url" };
  }
  if (parsedUrl.href === "about:blank") {
    return { action: "block", reason: "script_blank_window" };
  }
  if (parsedUrl.protocol !== "http:" && parsedUrl.protocol !== "https:") {
    return { action: "block", reason: "unsupported_protocol" };
  }

  let url: string;
  try {
    url = normalizeNavigableUrl(parsedUrl.href);
  } catch {
    return { action: "block", reason: "invalid_url" };
  }

  const frameName = details.frameName.trim().toLowerCase();
  if (frameName && frameName !== "_blank") {
    return { action: "block", reason: "named_window" };
  }

  const featureNames = popupFeatureNames(details.features);
  if (
    [...featureNames].some(
      (feature) => !CURRENT_PAGE_SAFE_FEATURES.has(feature),
    )
  ) {
    return { action: "block", reason: "separate_window_features" };
  }

  // Electron 只会为 target=_blank 表单提供 postBody；请求体与 Content-Type
  // 可以在同一 guest 的 loadURL 中完整重放，不需要独立 opener。
  if (details.postBody) return { action: "navigate_current_page", url };

  // Chromium 的 foreground/background-tab disposition 表示由链接点击产生；
  // Electron 43 对普通 target=_blank 链接可能把 frameName 归一化为空字符串。
  // 脚本 window.open 使用 default；只有该分支才需要显式断开 opener。
  if (
    details.disposition === "foreground-tab" ||
    details.disposition === "background-tab"
  ) {
    return { action: "navigate_current_page", url };
  }
  if (
    !frameName &&
    !featureNames.has("noopener") &&
    !featureNames.has("noreferrer")
  ) {
    return { action: "block", reason: "opener_required" };
  }

  return { action: "navigate_current_page", url };
}

function popupFeatureNames(value: string): Set<string> {
  return new Set(
    value
      .split(",")
      .map((part) => part.trim().toLowerCase().split("=", 1)[0] ?? "")
      .filter(Boolean),
  );
}
