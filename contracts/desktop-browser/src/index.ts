export const DESKTOP_BROWSER_PROTOCOL_VERSION = { major: 3, minor: 4 } as const;

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

export interface BrowserSnapshotTarget {
  snapshot_revision: number;
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
        subtree_ref?: string | null;
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
  visible: boolean;
  bounds: BrowserNormalizedRect | null;
  children: BrowserSnapshotNode[];
}

export interface BrowserSnapshot {
  tab_id: BrowserTabId;
  navigation_revision: number;
  snapshot_revision: number;
  root: BrowserSnapshotNode;
  returned_nodes: number;
  total_nodes: number;
  text_bytes: number;
  truncated: boolean;
  continuation_refs: string[];
  accessibility_tree?: BrowserAccessibilityNode[];
}

export interface BrowserAccessibilityNode {
  node_id: string;
  element_ref?: string | null;
  parent_id?: string | null;
  child_ids: string[];
  role?: string | null;
  name?: string | null;
  value?: string | null;
  description?: string | null;
  ignored: boolean;
  properties: Record<string, unknown>;
  actions: string[];
  backend_dom_node_id?: number | null;
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
      payload: { binding: BrowserSurfaceBinding; url: string };
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
