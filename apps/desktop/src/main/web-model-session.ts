/**
 * 应用级（GPT Web）浏览器会话的身份常量。
 *
 * 单独成模块，供 `browser-surface-manager`（含 Electron 依赖）与
 * `desktop-control-server`（可在无 Electron 的测试装配下加载）共用，
 * 避免把 `electron` 的 `session` 依赖带进控制面。
 *
 * 设计依据：A21（固定持久分区）、A25 / R49（App 级不激活驱动路径）。
 */

/** 应用级 GPT Web 会话的固定持久分区。Electron 中只有 `persist:` 前缀会落盘。 */
export const WEB_MODEL_PARTITION = "persist:magi-web-model";

/** 应用级会话 id 由 daemon 按 `browser-session-app-<now>-<seq>` 生成。 */
export const WEB_MODEL_SESSION_ID_PREFIX = "browser-session-app-";

/** 判断某个浏览器会话是否就是应用级 GPT Web 会话。 */
export function isWebModelBrowserSession(browserSessionId: string): boolean {
  return browserSessionId.startsWith(WEB_MODEL_SESSION_ID_PREFIX);
}

/**
 * Map a daemon Browser session identity to the Chromium partition owned by it.
 *
 * App-level sessions deliberately do not derive a partition from their
 * session id: the id is a durable logical identity, while the fixed
 * `persist:` partition is the durable login-state owner. Ordinary Browser
 * sessions keep their existing per-session in-memory partition semantics.
 */
export function browserPartitionForSession(browserSessionId: string): string {
  if (isWebModelBrowserSession(browserSessionId)) return WEB_MODEL_PARTITION;
  const safe = browserSessionId.replace(/[^A-Za-z0-9._-]/gu, "_");
  return `magi-browser-${safe}`;
}
