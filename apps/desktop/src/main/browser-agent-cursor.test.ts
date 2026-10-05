import assert from "node:assert/strict";
import test from "node:test";
import { agentCursorActionForInput } from "./browser-agent-cursor.js";

test("代理光标动作：点击只在 pressed 触发，released 只更新位置", () => {
  assert.equal(
    agentCursorActionForInput("Input.dispatchMouseEvent", { type: "mousePressed", button: "left" }),
    "click",
  );
  assert.equal(
    agentCursorActionForInput("Input.dispatchMouseEvent", { type: "mouseReleased", button: "left" }),
    "move",
  );
});

test("代理光标动作：按住按键移动是拖动，普通移动是移动", () => {
  assert.equal(
    agentCursorActionForInput("Input.dispatchMouseEvent", { type: "mouseMoved", button: "left" }),
    "drag",
  );
  assert.equal(
    agentCursorActionForInput("Input.dispatchMouseEvent", { type: "mouseMoved", button: "none" }),
    "move",
  );
  assert.equal(
    agentCursorActionForInput("Input.dispatchMouseEvent", { type: "mouseMoved" }),
    "move",
  );
});

test("代理光标动作：键盘事件与文本输入都是输入，滚轮是滚动", () => {
  assert.equal(agentCursorActionForInput("Input.insertText", {}), "type");
  assert.equal(agentCursorActionForInput("Input.dispatchKeyEvent", { type: "keyDown" }), "type");
  assert.equal(
    agentCursorActionForInput("Input.dispatchMouseEvent", { type: "mouseWheel" }),
    "scroll",
  );
});
