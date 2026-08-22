export const DESKTOP_BROWSER_PROTOCOL_VERSION = { major: 3, minor: 2 } as const;

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
  | { type: "close_page"; payload: { tab_id: BrowserTabId } }
  | {
      type: "navigate";
      payload: { tab_id: BrowserTabId; control: BrowserControl; navigation: BrowserNavigation };
    }
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
      payload: { tab_id: BrowserTabId; control: BrowserControl; target: BrowserSnapshotTarget };
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
      payload: { tab_id: BrowserTabId; navigation_revision: number; x: number; y: number };
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
  origin?: string | null;
  title: string;
  navigation_revision: number;
}

export interface BrowserSnapshotNode {
  element_ref: string;
  role?: string | null;
  name?: string | null;
  value?: string | null;
  description?: string | null;
  disabled: boolean;
  focused: boolean;
  editable: boolean;
  sensitive_input_kind?: "password" | "one_time_code" | "payment_card" | null;
  visible: boolean;
  bounds?: BrowserNormalizedRect | null;
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
  | { type: "primary_surface_changed"; payload: { binding: BrowserSurfaceBinding } }
  | { type: "user_takeover"; payload: { binding: BrowserSurfaceBinding } }
  | {
      type: "control_revoked";
      payload: { binding: BrowserSurfaceBinding; reason: string };
    }
  | {
      type: "page_updated";
      payload: { binding: BrowserSurfaceBinding; page_state: BrowserPageState };
    }
  | { type: "page_failed"; payload: { binding: BrowserSurfaceBinding; reason: string } }
  | { type: "loading_changed"; payload: { binding: BrowserSurfaceBinding; loading: boolean } }
  | { type: "page_crashed"; payload: { binding: BrowserSurfaceBinding; diagnostic?: string | null } }
  | { type: "console"; payload: { tab_id: BrowserTabId; level: string; text: string } }
  | { type: "dialog"; payload: { tab_id: BrowserTabId; dialog_id: number; dialog_type: string; message: string } }
  | { type: "download"; payload: { tab_id: BrowserTabId; suggested_filename: string; state: string; byte_length?: number; error?: string } }
  | { type: "popup_blocked"; payload: { binding: BrowserSurfaceBinding; url: string } }
  | { type: "agent_cursor"; payload: { tab_id: BrowserTabId; visible: boolean; x: number | null; y: number | null; action: string | null } }
  | { type: "binary_payload_ready"; payload: BrowserBinaryPayload }
  | { type: "heartbeat"; payload: { monotonic_millis: number } };

export interface BrowserHostEventEnvelope {
  protocol_version: ProtocolVersion;
  sequence: number;
  event: BrowserHostEvent;
}

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

export interface WorkerCdpRequest {
  type: "cdp_request";
  request_id: string;
  binding: BrowserSurfaceBinding;
  method: string;
  params?: Record<string, unknown>;
  session_id?: string;
}

export interface WorkerCdpResponse {
  type: "cdp_response";
  request_id: string;
  binding: BrowserSurfaceBinding;
  result?: unknown;
  error?: BrowserCommandError;
}

export interface WorkerCdpEvent {
  type: "cdp_event";
  binding: BrowserSurfaceBinding;
  method: string;
  params: Record<string, unknown>;
  session_id?: string;
}

export interface WorkerRebindRequest {
  type: "worker_rebind";
  bindings: BrowserSurfaceBinding[];
}

export interface WorkerReadyMessage {
  type: "worker_ready";
  worker_epoch: string;
  protocol_version: ProtocolVersion;
}

export type MainToWorkerMessage = WorkerCommandRequest | WorkerCdpResponse | WorkerCdpEvent | WorkerRebindRequest;
export type WorkerToMainMessage = WorkerCommandResponse | WorkerCdpRequest | WorkerReadyMessage;
