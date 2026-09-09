const ALLOWED_NAVIGATION_PROTOCOLS = new Set(["http:", "https:", "about:"]);
const BLOCKED_HOSTS = new Set(["169.254.169.254", "metadata.google.internal"]);

/**
 * Chromium 在主文档加载失败时会把 guest 切换到内部错误页。
 * chrome-error:// 不是用户导航地址，不能被导航状态机当成成功提交。
 */
export function isChromiumErrorPageUrl(value: string): boolean {
  return value.trim().toLowerCase().startsWith("chrome-error://");
}

export function normalizeNavigableUrl(value: string): string {
  const trimmed = value.trim() || "about:blank";
  if (trimmed === "about:blank") return trimmed;
  const candidate = /^[A-Za-z][A-Za-z\d+.-]*:/u.test(trimmed) ? trimmed : `https://${trimmed}`;
  const url = new URL(candidate);
  if (!ALLOWED_NAVIGATION_PROTOCOLS.has(url.protocol) || BLOCKED_HOSTS.has(url.hostname)) {
    throw new Error(`browser_navigation_url_rejected:${url.protocol}//${url.host}`);
  }
  if (url.username || url.password) throw new Error("browser_navigation_credentials_rejected");
  return url.href;
}

/**
 * 计算物理 Surface 的导航种子。
 *
 * Renderer 的激活请求可能在重建期间携带 about:blank，但已有 Surface
 * 已经拥有真实页面。只有新建 Surface 才允许把 about:blank 当作初始值；
 * 对已有 Surface 必须保留当前 Chromium URL 或已有运行态 URL，避免旧投影
 * 覆盖页面事实。
 */
export function resolveMaterializedPageUrl(
  requestedUrl: string,
  created: boolean,
  existingPageUrl: string,
  currentContentUrl: string,
): string {
  const requested = normalizeNavigableUrl(requestedUrl);
  if (created || requested !== "about:blank") return requested;
  const current = normalizeNavigableUrl(currentContentUrl);
  if (current !== "about:blank") return current;
  const existing = normalizeNavigableUrl(existingPageUrl);
  return existing !== "about:blank" ? existing : requested;
}

/**
 * 页面 URL 和标题必须来自同一份当前文档事实。
 *
 * Electron 在 guest 初始注册阶段可能让 `WebContents.getTitle()` 暂时返回
 * `about:blank`，即使主帧已经提交了真实 URL。已由导航事件或文档读取确认的
 * 标题优先级更高，不能被这个中间态覆盖。
 */
export function resolveBrowserPageTitle(
  runtimeUrl: string,
  recordedTitle: string,
  contentTitle: string,
): string {
  const recorded = recordedTitle.trim();
  const content = contentTitle.trim();
  if (recorded && !(runtimeUrl !== "about:blank" && recorded === "about:blank")) {
    return recorded;
  }
  if (content && !(runtimeUrl !== "about:blank" && content === "about:blank")) {
    return content;
  }
  return runtimeUrl === "about:blank" ? recorded || content : "";
}
