import type { BrowserHostCommand } from "@magi/desktop-browser-contracts";

/**
 * 命令是否会在页面上产生用户可见、不可安全重放的副作用。
 *
 * 与 Rust `BrowserHostCommand::is_side_effecting` 的归类一致：Rust 侧据此判定
 * “请求发出后连接丢失”，Desktop 侧据此判定“动作已交给 Worker 之后的失败”。
 * 新增命令必须在这里显式归类，否则类型检查失败。
 */
export function isSideEffectingBrowserCommand(command: BrowserHostCommand): boolean {
  const type = command.type;
  switch (type) {
    case "navigate":
    case "click":
    case "type":
    case "press":
    case "scroll":
    case "web_write_text":
    case "web_submit":
    case "web_cancel_generation":
    case "web_configure_connector":
      return true;
    case "ping":
    case "cancel":
    case "create_page":
    case "restore_page":
    case "ensure_surface":
    case "set_logical_viewport":
    case "get_logical_viewport":
    case "set_annotations":
    case "configure_network_policy":
    case "inspect_start":
    case "inspect_stop":
    case "close_page":
    case "stop_navigation":
    case "snapshot":
    case "devtools":
    case "screenshot":
    case "hit_test":
    case "update_control":
    case "web_model_probe":
    case "web_observe":
    case "web_turn_state":
    case "web_saved_conversations":
    case "web_saved_messages":
    case "web_read_image":
    case "web_connector_status":
    case "shutdown":
      return false;
    default: {
      const unclassified: never = type;
      throw new Error(`browser_command_unclassified:${String(unclassified)}`);
    }
  }
}

/**
 * 写命令已经交给 Chromium 执行之后发生的失败。动作可能已经生效，
 * 只能报告为“结果未确认”，不能报告为可安全重试的失败。
 */
export class BrowserSideEffectStartedError extends Error {
  constructor(cause: unknown) {
    super(cause instanceof Error ? cause.message : String(cause));
    this.name = "BrowserSideEffectStartedError";
    if (cause instanceof Error && cause.stack) this.stack = cause.stack;
  }
}
