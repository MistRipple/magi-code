/**
 * 把一次 CDP 输入映射成代理光标的动作。完整点击包含 pressed/released，
 * 只有 pressed 触发点击反馈；按住按键的移动是拖动；键盘与文本输入都是输入。
 */
export function agentCursorActionForInput(
  method: string,
  input: { type?: string; button?: string },
): string {
  if (method === "Input.insertText" || method === "Input.dispatchKeyEvent") {
    return "type";
  }
  if (input.type === "mousePressed") return "click";
  if (input.type === "mouseWheel") return "scroll";
  if (input.type === "mouseMoved" && input.button && input.button !== "none") {
    return "drag";
  }
  return "move";
}
