export {
  BROWSER_BLOCKED_CIDRS,
  BROWSER_BLOCKED_HOSTNAMES,
  BROWSER_LOOPBACK_CIDRS,
  BROWSER_LOOPBACK_HOSTNAMES,
  BROWSER_PREVIEW_HOSTNAME,
  BROWSER_PREVIEW_OPEN_PATH,
  BROWSER_PREVIEW_PATH_PREFIX,
  BROWSER_LAN_CIDRS,
} from "./browser-network-policy.generated.js";

export const DESKTOP_BROWSER_PROTOCOL_VERSION = { major: 3, minor: 7 } as const;

/**
 * Browser Surface 允许自动化附着的页面内部 Target 类型。
 * `page`、`webview` 及未知类型都不是当前一级 Browser Tab 的组成部分，
 * 不能沿协议边界被转换成子 Tab 或额外页面。
 */
export const BROWSER_CHILD_TARGET_TYPES = [
  "iframe",
  "worker",
  "service_worker",
  "shared_worker",
] as const;

export function isAllowedBrowserChildTarget(
  value: unknown,
): value is Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const type = (value as Record<string, unknown>).type;
  return (
    typeof type === "string" &&
    (BROWSER_CHILD_TARGET_TYPES as readonly string[]).includes(type)
  );
}

/**
 * 规范化 Chromium 返回的可选 DOM.nodeId。
 *
 * Chromium 的 nodeId=0 表示没有可复用的 DOM 节点身份，必须在协议边界
 * 归一化为 null；只有正的安全整数才可以继续沿着节点选择链路传递。
 */
export function normalizeOptionalDomNodeId(value: unknown): number | null {
  return Number.isSafeInteger(value) && (value as number) > 0
    ? (value as number)
    : null;
}

export type DesktopEpoch = string;
export type WindowId = string;
export type SurfaceId = string;
export type BrowserTabId = string;
export type BrowserCommandId = string;

export interface ProtocolVersion {
  major: number;
  minor: number;
}

export interface BrowserSurfaceBinding {
  desktop_epoch: DesktopEpoch;
  window_id: WindowId;
  surface_id: SurfaceId;
  surface_revision: number;
  tab_id: BrowserTabId;
  web_contents_id: number;
  target_id: string;
  browser_context_id: string;
  navigation_revision: number;
}

/**
 * 产生 inspect 请求或节点选择的真实 Browser Surface 身份。字段必须完整提供，
 * 不能从逻辑 Tab 推断，因为同一 Tab 可能重新绑定或发生导航。
 */
export interface BrowserSurfaceIdentity {
  tab_id: BrowserTabId;
  surface_id: SurfaceId;
  navigation_revision: number;
}

export interface BrowserNodeSelection extends BrowserSurfaceIdentity {
  browser_session_id: string;
  url: string;
  title: string;
  frame_id: string | null;
  backend_dom_node_id: number;
  dom_node_id: number | null;
  node_name: string;
  attributes: Record<string, string>;
  text_excerpt: string;
  outer_html: string;
  /** outer_html 是否已在 Chromium 侧截断，供模型正确判断上下文完整性。 */
  outer_html_truncated: boolean;
  aria_role: string | null;
  aria_name: string | null;
  bounds: BrowserNormalizedRect | null;
}

export interface DesktopBrowserHandshake {
  protocol_version: ProtocolVersion;
  desktop_version: string;
  electron_version: string;
  chromium_version: string;
  process_id: number;
  desktop_epoch: DesktopEpoch;
  worker_epoch: string;
}

export type BrowserDeviceType = "desktop" | "mobile";

export type BrowserPopupBlockReason =
  | "invalid_url"
  | "unsupported_protocol"
  | "script_blank_window"
  | "named_window"
  | "opener_required"
  | "separate_window_features";

export type BrowserLogicalViewport =
  | { mode: "auto" }
  | {
      mode: "fixed";
      width: number;
      height: number;
      device_scale_factor_millis: number;
      device_type: BrowserDeviceType;
    };

export type BrowserControl =
  | { mode: "agent"; lease_id: string; fence: number }
  | { mode: "user"; fence: number };

export type BrowserControlUpdate =
  | { mode: "agent"; lease_id: string; fence: number }
  | { mode: "user"; fence: number }
  | { mode: "released"; fence: number };

/** 快照元素引用（形如 e:3:12）本身就标识了所属快照；过期由页面运行时的引用表判断。 */
export interface BrowserSnapshotTarget {
  element_ref: string;
}

export interface BrowserNormalizedRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type BrowserNavigation =
  | {
      action: "url";
      url: string;
      handle_before_unload?: "accept" | "dismiss";
      init_script?: string;
      timeout_ms?: number;
    }
  | { action: "back"; timeout_ms?: number }
  | { action: "forward"; timeout_ms?: number }
  | { action: "stop" }
  | {
      action: "reload";
      ignore_cache?: boolean;
      handle_before_unload?: "accept" | "dismiss";
      timeout_ms?: number;
    };

export type BrowserHostCommand =
  | { type: "ping" }
  | { type: "cancel"; payload: { request_id: BrowserCommandId } }
  | {
      type: "create_page" | "restore_page";
      payload: {
        tab_id: BrowserTabId;
        browser_session_id: string;
        initial_url: string;
        logical_viewport: BrowserLogicalViewport;
        navigation_revision: number;
        snapshot_revision: number;
        allow_page_eviction?: boolean;
      };
    }
  | {
      type: "ensure_surface";
      payload: { tab_id: BrowserTabId };
    }
  | {
      type: "set_logical_viewport";
      payload: { tab_id: BrowserTabId; viewport: BrowserLogicalViewport };
    }
  | {
      type: "get_logical_viewport";
      payload: { tab_id: BrowserTabId };
    }
  | {
      type: "set_annotations";
      payload: { tab_id: BrowserTabId; annotations: unknown[] };
    }
  | {
      type: "configure_network_policy";
      payload: { lan_access_enabled: boolean };
    }
  | { type: "inspect_start"; payload: BrowserSurfaceIdentity }
  | { type: "inspect_stop"; payload: BrowserSurfaceIdentity }
  | { type: "close_page"; payload: { tab_id: BrowserTabId } }
  | {
      type: "navigate";
      payload: {
        tab_id: BrowserTabId;
        control: BrowserControl;
        navigation: BrowserNavigation;
      };
    }
  /**
   * 立即中断当前 Tab 的 Chromium 导航。该命令是唯一允许绕过同一 Tab
   * 普通资源队列的高优先级控制命令；它不会创建新 Surface，也不会改变
   * 右栏布局或 Browser Tab 身份。
   */
  | { type: "stop_navigation"; payload: { tab_id: BrowserTabId } }
  | {
      type: "snapshot";
      payload: {
        tab_id: BrowserTabId;
        navigation_revision: number;
        snapshot_revision: number;
        limits: { max_nodes: number; max_text_bytes: number };
      };
    }
  | {
      type: "click";
      payload: {
        tab_id: BrowserTabId;
        control: BrowserControl;
        target: BrowserSnapshotTarget;
      };
    }
  | {
      type: "type";
      payload: {
        tab_id: BrowserTabId;
        control: BrowserControl;
        target: BrowserSnapshotTarget;
        text: string;
        replace: boolean;
        submit_key?: string | null;
      };
    }
  | {
      type: "press";
      payload: { tab_id: BrowserTabId; control: BrowserControl; key: string };
    }
  | {
      type: "scroll";
      payload: {
        tab_id: BrowserTabId;
        control: BrowserControl;
        target?: BrowserSnapshotTarget | null;
        delta_x: number;
        delta_y: number;
      };
    }
  | {
      type: "devtools";
      payload: {
        tab_id: BrowserTabId;
        control?: BrowserControl | null;
        operation: string;
        arguments: Record<string, unknown>;
      };
    }
  | {
      type: "screenshot";
      payload: {
        tab_id: BrowserTabId;
        navigation_revision: number;
        target?: BrowserSnapshotTarget | null;
        clip?: BrowserNormalizedRect | null;
        full_page: boolean;
        format: "png" | "jpeg" | "webp";
        quality?: number;
      };
    }
  | {
      type: "hit_test";
      payload: {
        tab_id: BrowserTabId;
        navigation_revision: number;
        /** 内容槽相对于当前 Chromium CSS 视口的归一化横坐标。 */
        normalized_x: number;
        /** 内容槽相对于当前 Chromium CSS 视口的归一化纵坐标。 */
        normalized_y: number;
      };
    }
  | {
      type: "update_control";
      payload: {
        tab_id: BrowserTabId;
        surface_id: SurfaceId;
        control: BrowserControlUpdate;
      };
    }
  /**
   * ChatGPT 站点只读探测。
   *
   * 只读页面事实（登录态、composer、模型菜单、账号能力、连接器支持性），
   * 绝不写页面；不进入模型可见的浏览器工具目录（C10）。
   */
  | {
      type: "web_model_probe";
      payload: { tab_id: BrowserTabId };
    }
  /**
   * 站点内原子纯文本写入 + 回读校验。
   *
   * 不复用 `type`（它走 CDP `Input.insertText`，没有回读校验，且对长文本的
   * contenteditable 富文本编辑器不可靠）。回读不一致或内容被站点转成附件即失败。
   */
  | {
      type: "web_write_text";
      payload: {
        tab_id: BrowserTabId;
        selector: string;
        text: string;
        mode: "replace" | "append";
        expect_text_digest?: string | null;
        timeout_ms?: number | null;
      };
    }
  /**
   * 站点短轮询观察。
   *
   * 返回单次快照；`revision` 单调递增，供推理通道判断内容是否变化。长等待会占住
   * 该 Surface 的命令 lane，因此观察必须短轮询，不复用 `wait_for`。
   */
  | {
      type: "web_observe";
      payload: {
        tab_id: BrowserTabId;
        selector: string;
        fields: Array<"text" | "html" | "existence" | "attribute">;
        attribute?: string | null;
      };
    }
  /**
   * 提交 composer。
   *
   * 只负责把输入交给站点并回读输入框是否清空；「是否被接受」由
   * `web_turn_state` 的消息计数证据判定，不以命令返回成功为准。
   */
  | {
      type: "web_submit";
      payload: { tab_id: BrowserTabId };
    }
  /**
   * 回合语义状态。
   *
   * 站点适配层给出原始语义信号（生成中标志、助手文本、消息计数、隐藏推理）；
   * 「文本稳定且无生成标志」的完成谓词由推理通道跨两个读取周期叠加判定。
   */
  | {
      type: "web_turn_state";
      payload: { tab_id: BrowserTabId };
    }
  /**
   * 中断当前 GPT Web 页面生成，不改变页面 / Tab 身份。
   * 取消 Magi turn 时必须显式停止网页侧生成，不能只停止 daemon 轮询。
   */
  | {
      type: "web_cancel_generation";
      payload: { tab_id: BrowserTabId };
    }
  /** 读取 ChatGPT 已保存对话列表（侧栏历史）。只读。 */
  | {
      type: "web_saved_conversations";
      payload: { tab_id: BrowserTabId };
    }
  /** 读取当前已保存对话页面的远端事实（id、标题、可见消息与其稳定 id）。只读。 */
  | {
      type: "web_saved_messages";
      payload: { tab_id: BrowserTabId };
    }
  /**
   * 分块读取 GPT Web 回复里的一张图片（`source` 必须是当前页面上某个 `<img>` 的地址，例如 `blob:`）。
   * 图片字节只存在于页面里，Main/Worker 控制通道单条消息有上限，所以按块读取，由 daemon 拼接。
   */
  | {
      type: "web_read_image";
      payload: { tab_id: BrowserTabId; source: string; offset: number; length: number };
    }
  /** 只读检查 ChatGPT 侧的 Magi 连接器是否存在 / 启用。 */
  | {
      type: "web_connector_status";
      payload: { tab_id: BrowserTabId; name: string };
    }
  /** 创建 / 启用 Magi 连接器并回读确认；只写连接器设置。 */
  | {
      type: "web_configure_connector";
      payload: { tab_id: BrowserTabId; name: string; tunnel_id: string };
    }
  | { type: "shutdown" };

export interface BrowserHostRequestEnvelope {
  request_id: string;
  protocol_version: ProtocolVersion;
  command: BrowserHostCommand;
}

export interface BrowserCommandError {
  code: string;
  message: string;
  recoverable: boolean;
  side_effect_started: boolean;
  diagnostic?: string | null;
}

/** `web_model_probe` 的归一化结果：只有登录与页面可用性，不含网页的模型菜单。 */
export interface BrowserWebModelProbe {
  /** 站点适配层结构版本；selector 漂移时由实现推进。 */
  site_revision: string;
  login_state: "signed_in" | "signed_out" | "blocked";
  /** 登录态判定需要两项证据：会话有效与输入框可用。 */
  composer_available: boolean;
  /** 当前页面类型：对话页、ChatGPT 二级页面（没有输入框是正常的），或 OpenAI 平台页面。 */
  page_kind?: "chat" | "other" | "platform";
  account_hint: "plus" | "pro" | "free" | "unknown";
}

/** `web_saved_conversations` 的结果。 */
export interface BrowserWebSavedConversations {
  conversations: Array<{
    conversation_id: string;
    title: string;
    updated_at: string | null;
  }>;
}

/** `web_saved_messages` 的结果：当前已保存对话页面的远端事实。 */
export interface BrowserWebSavedMessages {
  /** 页面不是已保存对话（`/c/<id>`）时为 null。 */
  conversation_id: string | null;
  title: string | null;
  messages: Array<{
    role: "user" | "assistant";
    text: string;
    remote_id: string | null;
  }>;
  last_message_id: string | null;
  updated_at: string | null;
}

/** `web_connector_status` 的结果。 */
export interface BrowserWebConnectorStatus {
  /** 站点是否提供自定义连接器入口（套餐 / 开发者模式）。 */
  supported: boolean;
  exists: boolean;
  enabled: boolean;
  tool_count: number | null;
  reason: string | null;
}

/** `web_configure_connector` 的结果。 */
export interface BrowserWebConfigureConnectorResult {
  configured: boolean;
  /** 配置后回读确认已启用。 */
  confirmed_enabled: boolean;
  reason: string | null;
}

/** `web_write_text` 的回读校验结果。 */
export interface BrowserWebWriteTextResult {
  confirmed: boolean;
  digest: string;
  char_count: number;
  /** 站点把内容转成附件时不保证全部进入上下文。 */
  became_attachment: boolean;
}

/** `web_observe` 的单次快照。 */
export interface BrowserWebObserveResult {
  revision: number;
  nodes: Array<{
    found: boolean;
    text?: string;
    html?: string;
    attributes?: Record<string, string>;
  }>;
}

/** `web_submit` 的提交结果。 */
export interface BrowserWebSubmitResult {
  submitted: boolean;
  /** 提交后输入框是否已清空；未清空说明站点未接受这次提交。 */
  composer_empty: boolean;
  reason: "composer_missing" | "composer_empty" | null;
}

/** `web_turn_state` 的回合语义状态。 */
export interface BrowserWebTurnStateResult {
  site_revision: string;
  login_state: "signed_in" | "signed_out" | "blocked";
  /** 风险 / 验证页。 */
  blocked: boolean;
  /** 生成中标志；完成判定要求它为 false。 */
  generating: boolean;
  composer_found: boolean;
  /** 提交是否被接受以用户消息计数为准（步骤 4）。 */
  user_message_count: number;
  assistant_message_count: number;
  /** 最后一条助手消息的可见文本；流式回调以它的累积快照为准。 */
  assistant_text: string;
  /** 隐藏推理文本，映射到 `thinking`。 */
  thinking_text: string;
  /** 页面当前最后一条可见消息，用于临时 Web 对话存活判定。 */
  last_message_role: "user" | "assistant" | null;
  last_message_text: string | null;
}

export type BrowserCommandResult =
  | { type: "empty" }
  | { type: "pong"; payload: { monotonic_millis: number } }
  | { type: "page_state"; payload: BrowserPageState }
  /** Worker → Main：交互命令实际作用的元素；无元素目标（如按键）时为 null。 */
  | { type: "action_target"; payload: BrowserActionTarget | null }
  /** Main → Rust：交互命令完成后的页面状态，以及实际作用的元素。 */
  | { type: "interaction"; payload: BrowserInteraction }
  | { type: "snapshot"; payload: BrowserSnapshot }
  | { type: "binary_payload"; payload: BrowserBinaryPayload }
  | { type: "hit_test"; payload: BrowserHitTest }
  | { type: "surface_binding"; payload: BrowserSurfaceBinding }
  | { type: "json"; payload: { value: unknown } };

export type BrowserCommandOutcome =
  | { status: "succeeded"; payload: BrowserCommandResult }
  | { status: "failed"; payload: BrowserCommandError }
  | { status: "cancelled" }
  | { status: "indeterminate"; payload: BrowserCommandError };

export interface BrowserHostResponseEnvelope {
  request_id: string;
  protocol_version: ProtocolVersion;
  outcome: BrowserCommandOutcome;
}

/** 交互命令实际作用的元素，用于向模型和用户确认操作对象（不含元素的值）。 */
export interface BrowserActionTarget {
  role: string | null;
  name: string | null;
}

export interface BrowserInteraction {
  page_state: BrowserPageState;
  target: BrowserActionTarget | null;
}

export interface BrowserPageState {
  tab_id: BrowserTabId;
  url: string;
  origin: string | null;
  title: string;
  navigation_revision: number;
}

export interface BrowserSnapshotNode {
  element_ref: string;
  role: string | null;
  name: string | null;
  value: string | null;
  description: string | null;
  disabled: boolean;
  focused: boolean;
  editable: boolean;
  sensitive_input_kind?: "password" | "one_time_code" | "payment_card" | null;
  /** 勾选、展开、选中等交互状态，由页面运行时从 DOM/ARIA 属性读取。 */
  states?: BrowserNodeState[];
  visible: boolean;
  bounds: BrowserNormalizedRect | null;
  children: BrowserSnapshotNode[];
}

export type BrowserNodeState =
  | "checked"
  | "unchecked"
  | "mixed"
  | "expanded"
  | "collapsed"
  | "selected"
  | "pressed"
  | "required"
  | "invalid"
  | "read_only";

export interface BrowserSnapshot {
  tab_id: BrowserTabId;
  navigation_revision: number;
  snapshot_revision: number;
  root: BrowserSnapshotNode;
  returned_nodes: number;
  total_nodes: number;
  text_bytes: number;
  truncated: boolean;
}

export interface BrowserBinaryPayload {
  payload_id: string;
  mime_type: string;
  byte_length: number;
  sha256: string;
}

export interface BrowserHitTest {
  navigation_revision: number;
  viewport_width: number;
  viewport_height: number;
  device_scale_factor_millis: number;
  scroll_x: number;
  scroll_y: number;
  element_ref: string;
  tag_name: string;
  test_id?: string | null;
  stable_id?: string | null;
  aria_role?: string | null;
  aria_name?: string | null;
  text_excerpt?: string | null;
  css_path: string;
  ancestor_fingerprint: string;
  dom_fingerprint: string;
  bounds: BrowserNormalizedRect;
}

export type BrowserHostEvent =
  | { type: "ready"; payload: DesktopBrowserHandshake }
  | {
      type: "primary_surface_changed";
      payload: { binding: BrowserSurfaceBinding };
    }
  | {
      type: "primary_surface_closed";
      payload: { binding: BrowserSurfaceBinding };
    }
  | { type: "user_takeover"; payload: { binding: BrowserSurfaceBinding } }
  | {
      type: "control_revoked";
      payload: { binding: BrowserSurfaceBinding; reason: string };
    }
  | {
      type: "page_updated";
      payload: { binding: BrowserSurfaceBinding; page_state: BrowserPageState };
    }
  | {
      type: "page_failed";
      payload: { binding: BrowserSurfaceBinding; reason: string };
    }
  | {
      type: "loading_changed";
      payload: { binding: BrowserSurfaceBinding; loading: boolean };
    }
  | {
      type: "page_crashed";
      payload: { binding: BrowserSurfaceBinding; diagnostic?: string | null };
    }
  | {
      type: "console";
      payload: { tab_id: BrowserTabId; level: string; text: string };
    }
  | {
      type: "dialog";
      payload: {
        tab_id: BrowserTabId;
        dialog_id: number;
        dialog_type: string;
        message: string;
      };
    }
  | {
      type: "download";
      payload: {
        tab_id: BrowserTabId;
        download_id: string;
        suggested_filename: string;
        state: string;
        received_bytes: number;
        total_bytes: number | null;
        byte_length?: number;
        error?: string;
      };
    }
  | {
      type: "popup_blocked";
      payload: {
        binding: BrowserSurfaceBinding;
        url: string;
        reason: BrowserPopupBlockReason;
      };
    }
  | { type: "node_selection"; payload: BrowserNodeSelection }
  | {
      type: "agent_cursor";
      payload: {
        tab_id: BrowserTabId;
        visible: boolean;
        x: number | null;
        y: number | null;
        action: BrowserAgentCursorAction | null;
      };
    }
  | { type: "binary_payload_ready"; payload: BrowserBinaryPayload }
  | { type: "heartbeat"; payload: { monotonic_millis: number } };

export interface BrowserHostEventEnvelope {
  protocol_version: ProtocolVersion;
  sequence: number;
  event: BrowserHostEvent;
}

export type BrowserAgentCursorAction =
  "move" | "click" | "drag" | "type" | "scroll";

export interface WorkerCommandRequest {
  type: "worker_command";
  call_id: string;
  binding: BrowserSurfaceBinding;
  command: BrowserHostCommand;
}

export interface WorkerCommandResponse {
  type: "worker_result";
  call_id: string;
  binding: BrowserSurfaceBinding;
  outcome: BrowserCommandOutcome;
  binary_base64?: string;
}

export interface WorkerCancelRequest {
  type: "worker_cancel";
  call_id: string;
}

export interface WorkerCdpRequest {
  type: "cdp_request";
  call_id: string;
  request_id: string;
  binding: BrowserSurfaceBinding;
  method: string;
  params?: Record<string, unknown>;
  session_id?: string;
  /** Lighthouse 导航允许同一 WebContents 在当前 CDP 会话内推进 navigation revision。 */
  allow_navigation_advance?: boolean;
}

/** 取消 Worker 已转发给 Desktop Main 的单个 CDP 请求。 */
export interface WorkerCdpCancelRequest {
  type: "cdp_cancel";
  call_id: string;
  request_id: string;
  binding: BrowserSurfaceBinding;
}

export type WorkerCdpResponse =
  | {
      type: "cdp_response";
      call_id: string;
      request_id: string;
      binding: BrowserSurfaceBinding;
      result: unknown;
      error?: never;
    }
  | {
      type: "cdp_response";
      call_id: string;
      request_id: string;
      binding: BrowserSurfaceBinding;
      error: BrowserCommandError;
      result?: never;
    };

export interface WorkerCdpEvent {
  type: "cdp_event";
  binding: BrowserSurfaceBinding;
  method: string;
  params: Record<string, unknown>;
  session_id?: string;
}

export interface WorkerRebindRequest {
  type: "worker_rebind";
  worker_epoch: string;
  rebind_id: string;
  bindings: BrowserSurfaceBinding[];
}

export interface WorkerRebindAck {
  type: "worker_rebind_ack";
  worker_epoch: string;
  rebind_id: string;
  binding_count: number;
}

export interface WorkerReadyMessage {
  type: "worker_ready";
  worker_epoch: string;
  protocol_version: ProtocolVersion;
}

export type MainToWorkerMessage =
  | WorkerCommandRequest
  | WorkerCancelRequest
  | WorkerCdpResponse
  | WorkerCdpEvent
  | WorkerRebindRequest;
export type WorkerToMainMessage =
  | WorkerCommandResponse
  | WorkerCdpRequest
  | WorkerCdpCancelRequest
  | WorkerReadyMessage
  | WorkerRebindAck;
