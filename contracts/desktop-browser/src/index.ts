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

export const DESKTOP_BROWSER_PROTOCOL_VERSION = { major: 3, minor: 8 } as const;

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

/** 宿主光标的一次展示请求；只确认呈现，不授予页面控制权。 */
export interface BrowserAgentCursorPresentation extends BrowserSurfaceIdentity {
  motionId: string;
}

export interface BrowserAgentCursorPresentationResult extends BrowserAgentCursorPresentation {
  outcome: "arrived" | "hidden" | "interrupted";
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
export type BrowserHostCommand = { type: "ping" }
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
 | { type: "stop_navigation"; payload: { tab_id: BrowserTabId } }
 | {
      type: "snapshot";
      payload: {
        tab_id: BrowserTabId;
        navigation_revision: number;
        snapshot_revision: number;
        limits: { max_nodes: number; max_text_bytes: number };
        selector?: string;
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
  /** 动作的可观察结果。没有结果证据的命令（如按键、滚动）省略。 */
  effect?: BrowserActionEffect | null;
}

/**
 * 交互动作实际产生的效果，由 Worker 在页面内观察得出，是 Agent 判断“操作是否生效”的唯一依据。
 * - `navigated`：动作触发了文档或路由跳转，旧节点引用已失效；
 * - `changed`：页面或目标自身状态发生了变化（展开、勾选、弹层、内容刷新、目标被移除）；
 * - `no_visible_change`：原生事件已送达，但没有观察到任何变化，重复同一动作通常没有帮助。
 */
export interface BrowserActionEffect {
  verdict: "navigated" | "changed" | "no_visible_change";
  /** `confirmed`：页面内确认收到可信事件；`inferred`：页面已跳转，无法回读，按已完成的输入派发推断。 */
  delivered: "confirmed" | "inferred";
  navigated: boolean;
  /** 动作后稳定窗口内的 DOM 变化次数（上限 99）；页面已跳转时为 null。 */
  dom_mutations: number | null;
  /** 目标节点动作后是否仍在文档中；被移除常是菜单项、弹层按钮成功的信号。页面已跳转时为 null。 */
  target_connected: boolean | null;
  /** 目标自身的无障碍状态摘要（如 `expanded=true`）；没有可读状态时为 null。 */
  target_state: { before: string; after: string } | null;
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
  device_type?: BrowserDeviceType;
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
  | { type: "user_activity"; payload: { binding: BrowserSurfaceBinding } }
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
        saved_path?: string;
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
