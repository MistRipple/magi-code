/**
 * 应用级 GPT Web（Web 模型浏览器）的渲染层常量。
 *
 * 这里只放渲染层需要的最小事实，不复制 daemon 或站点适配层的任何状态。
 */

/** 应用级 GPT Web 会话的固定持久分区。 */
export const WEB_MODEL_PARTITION = 'persist:magi-web-model';

/**
 * 应用级 GPT Web 会话的固定主页 Tab id。
 *
 * 与 daemon 的 `WEB_MODEL_HOME_TAB_ID`（`crates/magi-api/src/routes/browser.rs`）
 * 必须保持一致；推理页面由推理通道按需创建，不在这里创建。
 */
export const WEB_MODEL_HOME_TAB_ID = 'browser-tab-web-model-home';

/** 应用级会话 id 由 daemon 按 `browser-session-app-<now>-<seq>` 生成。 */
export const WEB_MODEL_SESSION_ID_PREFIX = 'browser-session-app-';

/** 判断某个浏览器会话是否就是应用级 GPT Web 会话。 */
export function isWebModelBrowserSession(browserSessionId: string): boolean {
  return browserSessionId.startsWith(WEB_MODEL_SESSION_ID_PREFIX);
}

/**
 * Renderer 侧唯一的分区派生点。
 *
 * 应用级会话固定使用 `persist:magi-web-model`；普通浏览器 Tab 沿用
 * `magi-browser-<id>` 的内存会话语义。宿主侧 `browserPartitionId` 派生同一结果。
 */
export function browserPartitionForSession(browserSessionId: string): string {
  if (isWebModelBrowserSession(browserSessionId)) {
    return WEB_MODEL_PARTITION;
  }
  return `magi-browser-${browserSessionId.replace(/[^A-Za-z0-9._-]/gu, '_')}`;
}
