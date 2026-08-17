import assert from "node:assert/strict";
import test from "node:test";
import {
  WINDOW_LAYOUT,
  createWindowLayoutState,
  reduceWindowLayout,
  shouldShowBrowserSurface,
  snapshotWindowLayout,
} from "./window-layout.js";

test("right pane keeps the left and middle workbench usable at its maximum", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1440, height: 900 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 960 });
  const snapshot = snapshotWindowLayout(state);
  assert.equal(snapshot.rightPaneBounds?.width, 960);
  assert.deepEqual(snapshot.appBounds, { x: 0, y: 0, width: 1440, height: 900 });
  assert.equal(snapshot.rightPaneBounds?.x, 1440 - 960);
  assert.equal(snapshot.rightPaneBounds?.y, 0);
  assert.equal(snapshot.rightPaneBounds?.height, 900);
  assert.equal(snapshot.dividerBounds?.width, WINDOW_LAYOUT.rightPaneResizeHandleWidth);
  assert.equal(
    snapshot.dividerBounds?.x,
    snapshot.rightPaneBounds!.x - WINDOW_LAYOUT.rightPaneResizeHandleWidth,
  );
  assert.equal(
    snapshot.rightPaneBounds!.x + snapshot.rightPaneBounds!.width,
    1440,
  );
});

test("side-by-side width leaves the unified renderer conversation track usable", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 840, height: 700 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  const snapshot = snapshotWindowLayout(state);
  assert.equal(snapshot.rightPaneMode, "side-by-side");
  assert.equal(snapshot.rightPaneWidth, 384);
  assert.equal(
    snapshot.rightPaneWidth
      + WINDOW_LAYOUT.rightPaneResizeHandleWidth
      + WINDOW_LAYOUT.minWorkbenchContentWidth,
    840,
  );
});

test("browser tab changes only logical panel identity", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1200, height: 800 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, {
    type: "active_panel",
    kind: "browser",
    tabId: "tab-1",
    surfaceId: "surface-1",
  });
  const snapshot = snapshotWindowLayout(state);
  assert.equal(snapshot.activePanelKind, "browser");
  assert.equal(snapshot.activeTabId, "tab-1");
  assert.equal(snapshot.activeSurfaceId, "surface-1");
  assert.equal("browserContentBounds" in snapshot, false);
});

test("non-browser right-pane tabs keep the same pane geometry and hide BrowserSurface", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1440, height: 900 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 920 });
  const browser = snapshotWindowLayout(reduceWindowLayout(state, {
    type: "active_panel",
    kind: "browser",
    tabId: "browser-tab",
    surfaceId: "surface-1",
  }));
  const code = snapshotWindowLayout(reduceWindowLayout(state, {
    type: "active_panel",
    kind: "code",
    tabId: "code-tab",
    surfaceId: null,
  }));
  const terminal = snapshotWindowLayout(reduceWindowLayout(state, {
    type: "active_panel",
    kind: "terminal",
    tabId: "terminal-tab",
    surfaceId: null,
  }));

  assert.deepEqual(code.rightPaneBounds, browser.rightPaneBounds);
  assert.deepEqual(terminal.rightPaneBounds, browser.rightPaneBounds);
  assert.equal(code.activeSurfaceId, null);
  assert.equal(terminal.activeSurfaceId, null);
});

test("small windows use overlay without changing browser viewport ownership", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 720, height: 600 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  const snapshot = snapshotWindowLayout(state);
  assert.equal(snapshot.rightPaneMode, "overlay");
  assert.equal(snapshot.appBounds.width, 720);
  assert.equal(snapshot.dividerBounds, null);
  assert.equal(snapshot.rightPaneBounds?.y, 0);
  assert.equal(snapshot.rightPaneBounds?.height, 600);
});

test("browser surface identity survives window geometry changes until the new slot arrives", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1440, height: 900 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, {
    type: "active_panel",
    kind: "browser",
    tabId: "browser-tab",
    surfaceId: "surface-1",
  });
  const resized = reduceWindowLayout(state, {
    type: "client_bounds",
    bounds: { x: 0, y: 0, width: 1200, height: 760 },
    displayScaleFactor: 2,
    fullscreen: false,
  });

  assert.equal(resized.activePanelKind, "browser");
  assert.equal(resized.activeTabId, "browser-tab");
  assert.equal(resized.activeSurfaceId, "surface-1");
  assert.equal(shouldShowBrowserSurface(snapshotWindowLayout(resized), true), true);
});

test("browser content slot width does not change the parent right pane width", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1440, height: 900 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 480 });

  // BrowserTabContent 的内容槽可以因为边框、工具栏或内部布局小于右栏轨道。
  // 该值只供 WebContentsView 定位，不能作为新的 right_pane_width intent。
  const browserContentSlot = { x: 960, y: 74, width: 448, height: 826 };
  const snapshot = snapshotWindowLayout(state);

  assert.equal(browserContentSlot.width < snapshot.rightPaneBounds!.width, true);
  assert.equal(snapshot.rightPaneWidth, 480);
  assert.equal(snapshot.rightPaneBounds?.x, 960);
});

test("layout revisions are monotonic for every accepted intent", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1200, height: 800 },
  });
  const revisions = [state.layoutRevision];
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  revisions.push(state.layoutRevision);
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 600 });
  revisions.push(state.layoutRevision);
  state = reduceWindowLayout(state, {
    type: "client_bounds",
    bounds: { x: 0, y: 0, width: 1000, height: 700 },
    displayScaleFactor: 2,
    fullscreen: false,
  });
  revisions.push(state.layoutRevision);
  assert.deepEqual(revisions, [0, 1, 2, 3]);
});

test("double-click reset restores the default width without changing pane visibility", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1440, height: 900 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 880 });
  state = reduceWindowLayout(state, { type: "right_pane_reset_width" });
  const snapshot = snapshotWindowLayout(state);
  assert.equal(snapshot.rightPaneWidth, WINDOW_LAYOUT.defaultRightPaneWidth);
  assert.equal(snapshot.rightPaneVisible, true);
});

test("right pane minimum is the content track width", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1440, height: 900 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 1 });
  const snapshot = snapshotWindowLayout(state);
  assert.equal(snapshot.rightPaneWidth, WINDOW_LAYOUT.minRightPaneWidth);
});

test("collapsed right pane never leaves the BrowserSurface visible", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1440, height: 900 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, {
    type: "active_panel",
    kind: "browser",
    tabId: "browser-tab",
    surfaceId: "surface-1",
  });
  assert.equal(shouldShowBrowserSurface(snapshotWindowLayout(state), true), true);

  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: false });
  assert.equal(shouldShowBrowserSurface(snapshotWindowLayout(state), true), false);
});

test("没有绑定当前 Surface 时，旧内容槽不能重新显示原生 View", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1440, height: 900 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, {
    type: "active_panel",
    kind: "browser",
    tabId: "browser-tab",
    surfaceId: null,
  });

  assert.equal(shouldShowBrowserSurface(snapshotWindowLayout(state), true), false);
});
