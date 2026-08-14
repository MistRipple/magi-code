import assert from "node:assert/strict";
import test from "node:test";
import {
  WINDOW_LAYOUT,
  createWindowLayoutState,
  reduceWindowLayout,
  snapshotWindowLayout,
} from "./window-layout.js";

test("right pane can occupy two thirds without shrinking app below its contract", () => {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1440, height: 900 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 960 });
  const snapshot = snapshotWindowLayout(state);
  assert.equal(snapshot.rightPaneBounds?.width, 960);
  assert.equal(snapshot.appBounds.width, 1440 - 960 - WINDOW_LAYOUT.dividerWidth);
});

test("browser surface excludes tab bar, toolbar, border and resize handle", () => {
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
  assert.equal(
    snapshot.browserSurfaceBounds?.y,
    WINDOW_LAYOUT.rightPaneBorder
      + WINDOW_LAYOUT.rightPaneTabBarHeight
      + WINDOW_LAYOUT.browserToolbarHeight,
  );
  assert.equal(
    snapshot.browserSurfaceBounds?.width,
    snapshot.rightPaneBounds!.width
      - WINDOW_LAYOUT.rightPaneResizeHandleWidth
      - WINDOW_LAYOUT.rightPaneBorder,
  );
  assert.equal(
    snapshot.browserSurfaceBounds?.x,
    snapshot.rightPaneBounds!.x + WINDOW_LAYOUT.rightPaneResizeHandleWidth,
  );
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
  assert.equal(code.browserSurfaceBounds, null);
  assert.equal(terminal.browserSurfaceBounds, null);
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
  assert.equal(snapshot.browserSurfaceBounds, null);
});
