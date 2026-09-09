import assert from "node:assert/strict";
import test from "node:test";
import {
  WINDOW_LAYOUT,
  createWindowLayoutState,
  reduceWindowLayout,
  resolveRightPaneMode,
  snapshotWindowLayout,
} from "./window-layout.js";

function baseState(width = 1440, height = 900) {
  return createWindowLayoutState({
    desktopEpoch: "desktop-test",
    windowId: "window-test",
    clientBounds: { x: 0, y: 0, width, height },
  });
}

test("右栏几何由父窗口布局唯一决定", () => {
  let state = baseState();
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  const snapshot = snapshotWindowLayout(state);

  assert.deepEqual(snapshot.appBounds, { x: 0, y: 0, width: 1440, height: 900 });
  assert.deepEqual(snapshot.rightPaneBounds, { x: 960, y: 0, width: 480, height: 900 });
  assert.deepEqual(snapshot.dividerBounds, {
    x: 952,
    y: 0,
    width: WINDOW_LAYOUT.rightPaneResizeHandleWidth,
    height: 900,
  });
});

test("右栏拖动只改变父容器宽度并保持工作区最小宽度", () => {
  let state = baseState(1000, 800);
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 900 });
  const snapshot = snapshotWindowLayout(state);

  assert.equal(snapshot.rightPaneWidth, 544);
  assert.equal(
    snapshot.rightPaneWidth
      + WINDOW_LAYOUT.rightPaneResizeHandleWidth
      + WINDOW_LAYOUT.minWorkbenchContentWidth,
    1000,
  );
  assert.deepEqual(snapshot.rightPaneBounds, { x: 456, y: 0, width: 544, height: 800 });
});

test("窄窗口使用整窗右栏模式，不创建伪造的浏览器内容坐标", () => {
  let state = baseState(720, 600);
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  const snapshot = snapshotWindowLayout(state);

  assert.equal(resolveRightPaneMode(720), "overlay");
  assert.equal(snapshot.rightPaneMode, "overlay");
  assert.deepEqual(snapshot.rightPaneBounds, { x: 0, y: 0, width: 720, height: 600 });
  assert.equal(snapshot.dividerBounds, null);
});

test("浏览器面板只保存逻辑 Surface 身份，非浏览器面板清除 Surface 身份", () => {
  let state = baseState();
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, {
    type: "active_panel",
    kind: "browser",
    tabId: "browser-tab",
    surfaceId: "surface-1",
  });
  assert.equal(state.activePanelKind, "browser");
  assert.equal(state.activeSurfaceId, "surface-1");

  state = reduceWindowLayout(state, {
    type: "active_panel",
    kind: "code",
    tabId: "code-tab",
    surfaceId: null,
  });
  assert.equal(state.activePanelKind, "code");
  assert.equal(state.activeTabId, "code-tab");
  assert.equal(state.activeSurfaceId, null);
});

test("窗口缩放由父布局同步，浏览器内容不参与反向测量", () => {
  let state = baseState();
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 600 });
  const previousRevision = state.layoutRevision;
  state = reduceWindowLayout(state, {
    type: "client_bounds",
    bounds: { x: 0, y: 0, width: 1200, height: 760 },
    displayScaleFactor: 2,
    fullscreen: false,
  });

  const snapshot = snapshotWindowLayout(state);
  assert.equal(state.layoutRevision, previousRevision + 1);
  assert.equal(snapshot.displayScaleFactor, 2);
  assert.deepEqual(snapshot.rightPaneBounds, { x: 600, y: 0, width: 600, height: 760 });
});

test("右栏隐藏后保留宽度意图，重新显示时由父布局恢复", () => {
  let state = baseState();
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 560 });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: false });
  assert.equal(snapshotWindowLayout(state).rightPaneBounds, null);

  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  assert.deepEqual(snapshotWindowLayout(state).rightPaneBounds, {
    x: 880,
    y: 0,
    width: 560,
    height: 900,
  });
});
