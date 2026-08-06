export const PROTOCOL_VERSION = { major: 1, minor: 8 } as const;
export const DEFAULT_SNAPSHOT_LIMITS = {
  max_nodes: 400,
  max_text_bytes: 32 * 1024,
} as const;

export interface ProtocolVersion {
  major: number;
  minor: number;
}

export interface RequestEnvelope {
  request_id: string;
  protocol_version: ProtocolVersion;
  command: HostCommand;
}

export type HostCommand =
  | { type: "ping" }
  | {
      type: "create_page";
      payload: {
        tab_id: string;
        initial_url: string;
        viewport: HostViewport;
        navigation_revision: number;
        snapshot_revision: number;
      };
    }
  | {
      type: "set_viewport";
      payload: {
        tab_id: string;
        viewport: HostViewport;
      };
    }
  | { type: "close_page"; payload: { tab_id: string } }
  | { type: "activate_page"; payload: { tab_id: string } }
  | {
      type: "navigate";
      payload: {
        tab_id: string;
        control: HostControl;
        navigation: Navigation;
      };
    }
  | {
      type: "snapshot";
      payload: {
        tab_id: string;
        limits: SnapshotLimits;
        subtree_ref?: string | null;
      };
    }
  | {
      type: "click";
      payload: {
        tab_id: string;
        control: HostControl;
        target: SnapshotTarget;
      };
    }
  | {
      type: "type";
      payload: {
        tab_id: string;
        control: HostControl;
        target: SnapshotTarget;
        text: string;
        replace: boolean;
      };
    }
  | {
      type: "press";
      payload: { tab_id: string; control: HostControl; key: string };
    }
  | {
      type: "scroll";
      payload: {
        tab_id: string;
        control: HostControl;
        target?: SnapshotTarget | null;
        delta_x: number;
        delta_y: number;
      };
    }
  | {
      type: "screenshot";
      payload: {
        tab_id: string;
        target?: SnapshotTarget | null;
        clip?: NormalizedRect | null;
        full_page: boolean;
        format: "png" | "jpeg";
      };
    }
  | {
      type: "hit_test";
      payload: {
        tab_id: string;
        navigation_revision: number;
        x: number;
        y: number;
      };
    }
  | {
      type: "start_screencast";
      payload: {
        tab_id: string;
        format: "jpeg" | "png";
        quality: number;
        max_width: number;
        max_height: number;
      };
    }
  | { type: "stop_screencast"; payload: { tab_id: string } }
  | {
      type: "user_input";
      payload: {
        tab_id: string;
        control: HostControl;
        event: UserInputEvent;
      };
    }
  | {
      type: "update_control";
      payload: { fence: number; mode: HostControlMode };
    }
  | { type: "shutdown" };

export type HostControl =
  | { mode: "agent"; lease_id: string; fence: number }
  | { mode: "user"; fence: number };

export type HostControlMode = "agent" | "user" | "disabled";

export interface HostViewport {
  width: number;
  height: number;
  device_scale_factor_millis: number;
  device_type: HostDeviceType;
}

export type HostDeviceType = "desktop" | "mobile";

export type Navigation =
  | { action: "url"; url: string }
  | { action: "back" }
  | { action: "forward" }
  | { action: "reload" };

export interface SnapshotLimits {
  max_nodes: number;
  max_text_bytes: number;
}

export interface SnapshotTarget {
  snapshot_revision: number;
  element_ref: string;
}

export interface NormalizedRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type MouseButton =
  | "none"
  | "left"
  | "middle"
  | "right"
  | "back"
  | "forward";

export type UserInputEvent =
  | { type: "mouse_move"; x: number; y: number }
  | {
      type: "mouse_down";
      x: number;
      y: number;
      button: MouseButton;
      click_count: number;
    }
  | {
      type: "mouse_up";
      x: number;
      y: number;
      button: MouseButton;
      click_count: number;
    }
  | {
      type: "mouse_wheel";
      x: number;
      y: number;
      delta_x: number;
      delta_y: number;
    }
  | {
      type: "key_down";
      key: string;
      code: string;
      key_code: number;
      modifiers: number;
    }
  | {
      type: "key_up";
      key: string;
      code: string;
      key_code: number;
      modifiers: number;
    }
  | { type: "insert_text"; text: string };

export interface ResponseEnvelope {
  request_id: string;
  protocol_version: ProtocolVersion;
  outcome: CommandOutcome;
}

export type CommandOutcome =
  | { status: "succeeded"; payload: CommandResult }
  | { status: "failed"; payload: CommandError }
  | { status: "cancelled" }
  | { status: "indeterminate"; payload: CommandError };

export type CommandResult =
  | { type: "empty" }
  | { type: "pong"; payload: { monotonic_millis: number } }
  | { type: "page_state"; payload: PageState }
  | { type: "snapshot"; payload: HostSnapshot }
  | { type: "binary_payload"; payload: BinaryPayload }
  | { type: "hit_test"; payload: HitTest }
  | { type: "clipboard_text"; payload: ClipboardText };

export interface ClipboardText {
  operation: "copy" | "cut";
  text: string;
}

export interface PageState {
  tab_id: string;
  url: string;
  origin?: string | null;
  title: string;
  navigation_revision: number;
}

export interface HostSnapshot {
  tab_id: string;
  snapshot_revision: number;
  root: SnapshotNode;
  returned_nodes: number;
  total_nodes: number;
  text_bytes: number;
  truncated: boolean;
  continuation_refs: string[];
}

export interface SnapshotNode {
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
  bounds?: Rect | null;
  children: SnapshotNode[];
}

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface BinaryPayload {
  payload_id: string;
  mime_type: string;
  byte_length: number;
  sha256: string;
}

export interface HitTest {
  frame_sequence: number;
  navigation_revision: number;
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
  bounds: Rect;
}

export interface CommandError {
  code: string;
  message: string;
  recoverable: boolean;
  side_effect_started: boolean;
  diagnostic?: string | null;
}

export interface EventEnvelope {
  protocol_version: ProtocolVersion;
  sequence: number;
  event: HostEvent;
}

export type HostEvent =
  | { type: "ready"; payload: HostHandshake }
  | { type: "page_updated"; payload: PageState }
  | {
      type: "page_crashed";
      payload: { tab_id: string; diagnostic?: string | null };
    }
  | {
      type: "console";
      payload: { tab_id: string; level: string; text: string };
    }
  | {
      type: "dialog";
      payload: { tab_id: string; dialog_type: string; message: string };
    }
  | {
      type: "download";
      payload: {
        tab_id: string;
        suggested_filename: string;
        state: string;
      };
    }
  | { type: "screencast_frame"; payload: ScreencastFrame }
  | { type: "binary_payload_ready"; payload: BinaryPayload }
  | { type: "heartbeat"; payload: { monotonic_millis: number } };

export interface HostHandshake {
  protocol_version: ProtocolVersion;
  runtime_version: string;
  host_version: string;
  playwright_version: string;
  chromium_version: string;
  process_id: number;
  runtime_epoch: number;
}

export interface ScreencastFrame {
  tab_id: string;
  frame_sequence: number;
  navigation_revision: number;
  payload_id: string;
  mime_type: string;
  byte_length: number;
  sha256: string;
  width: number;
  height: number;
  device_scale_factor_millis: number;
}
