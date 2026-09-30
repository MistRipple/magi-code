/**
 * Web 模型失败卡片的主行动总线（设计基线 §5.11、《实现计划》§8、§9.2 #23）。
 *
 * 为什么走事件而不是把动作回调层层传进 `ModelFailureCard`：失败卡片位于消息
 * 列表深处，而动作的宿主在别处（右栏、输入框、设置分区）。事件是仓库里既有的
 * 跨区域动作方式（如 `magi:previewFile`），且不把 UI 状态提升成业务协议。
 *
 * 约束：这里只派发**显式动作**，不携带任何 Web 会话事实；谁处理动作，谁去
 * 读 daemon 的权威投影。
 */
export const WEB_MODEL_ACTION_EVENT = 'magi:webModelAction';
export const WEB_MODEL_SETTINGS_REQUEST_EVENT = 'magi:webModelSettingsRequest';
export const WEB_MODEL_SETTINGS_READY_EVENT = 'magi:webModelSettingsReady';

export type WebModelActionKind =
  /** 打开 GPT Web 视图（登录页 / 主页都在这一条路径上，S1–S3）。 */
  | 'openView'
  /** 运行一次站点自检（只读探测）。 */
  | 'runDiagnostics'
  /** 打开会话内主模型选择器。 */
  | 'switchModel'
  /** 把该引擎的 T2 轮数上限提高一档后重试。 */
  | 'raiseRoundLimit'
  /** 查看 T3 通道状态与具体缺哪一项。 */
  | 'openTunnelSettings'
  /** 打开可用性 / Desktop 说明所在的浏览器设置页。 */
  | 'openSettings'
  /** 发送前失败且没有副作用时，使用已恢复的 composer 草稿重试。 */
  | 'retry';

export interface WebModelActionDetail {
  kind: WebModelActionKind;
  /** 触发该动作的错误码（用于提示文案与诊断记录）。 */
  failureCode?: string;
  sessionId?: string;
  engineId?: string;
}

export type WebModelSettingsFocus = 'diagnostics' | 'tunnel' | 'roundLimit';

export interface WebModelSettingsRequest {
  focus: WebModelSettingsFocus;
  /** 引擎级设置动作的目标；诊断 / T3 通道动作不需要。 */
  engineId?: string;
}

let pendingWebModelSettingsRequest: WebModelSettingsRequest | null = null;

/**
 * 请求打开 Web 模型设置。设置面板是懒加载的，因此除事件外还保留一个
 * 一次性 pending 请求，避免失败卡片点击发生在 SettingsPanel 挂载之前时丢失动作。
 */
export function requestWebModelSettings(request: WebModelSettingsRequest): void {
  pendingWebModelSettingsRequest = request;
  if (typeof window === 'undefined') return;
  window.dispatchEvent(new CustomEvent<WebModelSettingsRequest>(
    WEB_MODEL_SETTINGS_REQUEST_EVENT,
    { detail: request },
  ));
}

export function consumeWebModelSettingsRequest(): WebModelSettingsRequest | null {
  const request = pendingWebModelSettingsRequest;
  pendingWebModelSettingsRequest = null;
  return request;
}

export function dispatchWebModelAction(detail: WebModelActionDetail): void {
  if (typeof window === 'undefined') return;
  window.dispatchEvent(new CustomEvent<WebModelActionDetail>(WEB_MODEL_ACTION_EVENT, { detail }));
}

export interface WebModelFailureAction {
  kind: WebModelActionKind;
  /** i18n key：主行动按钮文案（§8 的「呈现与主行动」列）。 */
  labelKey: string;
}

/**
 * 错误码 → 主行动（§8 的唯一映射）。
 *
 * 表里没有的错误码表示「不需要用户动作」或「由 Magi 自动恢复处理」
 * （例如 `ContextLengthExceeded` 由压缩重建路径消化），卡片不显示按钮，
 * 也不显示一个点了没用的动作。
 */
export function webModelFailureAction(code: string): WebModelFailureAction | null {
  switch (code.trim()) {
    case 'web_send_rejected':
    case 'web_write_not_confirmed':
    case 'web_tool_protocol_invalid':
    case 'web_turn_timeout':
    case 'web_queue_full':
    case 'web_queue_timeout':
    case 'web_session_busy':
      return { kind: 'retry', labelKey: 'webModel.action.retry' };
    case 'web_login_expired':
      return { kind: 'openView', labelKey: 'webModel.action.login' };
    case 'web_site_blocked':
      return { kind: 'openView', labelKey: 'webModel.action.openHome' };
    case 'web_selectors_drift':
      return { kind: 'runDiagnostics', labelKey: 'webModel.action.runDiagnostics' };
    case 'web_quota_exhausted':
      return { kind: 'switchModel', labelKey: 'webModel.action.switchModel' };
    case 'web_tool_round_limit':
      return { kind: 'raiseRoundLimit', labelKey: 'webModel.action.raiseRoundLimit' };
    case 'web_tunnel_unavailable':
      return { kind: 'openTunnelSettings', labelKey: 'webModel.action.configureTunnel' };
    case 'web_desktop_unavailable':
      return { kind: 'openSettings', labelKey: 'webModel.action.openSettings' };
    default:
      return null;
  }
}
