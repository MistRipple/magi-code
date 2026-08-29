import assert from "node:assert/strict";
import test from "node:test";
import {
  WINDOW_LAYOUT,
  browserContentBounds,
  createWindowLayoutState,
  reduceWindowLayout,
  shouldShowBrowserSurface,
  snapshotWindowLayout,
  type BrowserContentSlot,
  type Rectangle,
  type RendererGeometryFrame,
  type WindowLayoutState,
} from "./window-layout.js";

const FRAME_COORDINATE_SPACE = "window-content-css-px" as const;

function submitGeometry(
  state: WindowLayoutState,
  input: {
    rightPaneBounds: Rectangle | null;
    browserContentSlot: BrowserContentSlot | null;
    revision?: number;
    layoutRevision?: number;
    coordinateSpace?: RendererGeometryFrame["coordinateSpace"];
  },
): WindowLayoutState {
  const revision = input.revision ?? ((state.rendererGeometry?.revision ?? -1) + 1);
  return reduceWindowLayout(state, {
    type: "renderer_geometry",
    frame: {
      revision,
      layoutRevision: input.layoutRevision ?? state.layoutRevision,
      coordinateSpace: input.coordinateSpace ?? FRAME_COORDINATE_SPACE,
      rightPaneBounds: input.rightPaneBounds,
      browserContentSlot: input.browserContentSlot,
    },
  });
}

function baseState(width = 1440, height = 900): WindowLayoutState {
  return createWindowLayoutState({
    desktopEpoch: "desktop-1",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width, height },
  });
}

function activateBrowser(
  state: WindowLayoutState,
  tabId = "browser-tab",
  surfaceId = "surface-1",
): WindowLayoutState {
  let next = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  return reduceWindowLayout(next, {
    type: "active_panel",
    kind: "browser",
    tabId,
    surfaceId,
  });
}

function completeBrowserFrame(
  state: WindowLayoutState,
  options: {
    tabId?: string;
    rightPaneBounds?: Rectangle;
    contentBounds?: Rectangle;
    revision?: number;
    layoutRevision?: number;
  } = {},
): WindowLayoutState {
  const tabId = options.tabId ?? state.activeTabId ?? "browser-tab";
  const rightPaneBounds = options.rightPaneBounds ?? { x: 960, y: 0, width: 480, height: 900 };
  const contentBounds = options.contentBounds ?? {
    x: rightPaneBounds.x + 1,
    y: rightPaneBounds.y + 74,
    width: rightPaneBounds.width - 2,
    height: rightPaneBounds.height - 74,
  };
  return submitGeometry(state, {
    ...(options.revision === undefined ? {} : { revision: options.revision }),
    ...(options.layoutRevision === undefined ? {} : { layoutRevision: options.layoutRevision }),
    rightPaneBounds,
    browserContentSlot: { tabId, bounds: contentBounds },
  });
}

test("最大右栏只由 Renderer 几何帧提供物理矩形，仍为左中区域保留逻辑空间", () => {
  let state = baseState();
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 960 });
  const beforeFrame = snapshotWindowLayout(state);

  assert.equal(beforeFrame.rightPaneWidth, 960);
  assert.equal(beforeFrame.rightPaneBounds, null);
  assert.deepEqual(beforeFrame.appBounds, { x: 0, y: 0, width: 1440, height: 900 });

  state = submitGeometry(state, {
    rightPaneBounds: { x: 480, y: 0, width: 960, height: 900 },
    browserContentSlot: null,
  });
  const snapshot = snapshotWindowLayout(state);
  assert.deepEqual(snapshot.rightPaneBounds, { x: 480, y: 0, width: 960, height: 900 });
  assert.equal(snapshot.dividerBounds?.width, WINDOW_LAYOUT.rightPaneResizeHandleWidth);
  assert.equal(snapshot.dividerBounds?.x, 480 - WINDOW_LAYOUT.rightPaneResizeHandleWidth);
});

test("side-by-side 逻辑宽度为工作区保留最小可用内容轨道", () => {
  let state = baseState(840, 700);
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
  assert.equal(snapshot.rightPaneBounds, null);
});

test("Browser Tab 切换只改变逻辑身份，并撤销旧 frame 中的浏览器槽位", () => {
  let state = completeBrowserFrame(activateBrowser(baseState(), "tab-1", "surface-1"));
  assert.equal(browserContentBounds(snapshotWindowLayout(state))?.x, 961);

  state = reduceWindowLayout(state, {
    type: "active_panel",
    kind: "browser",
    tabId: "tab-2",
    surfaceId: "surface-2",
  });
  const switched = snapshotWindowLayout(state);
  assert.equal(switched.activeTabId, "tab-2");
  assert.equal(switched.activeSurfaceId, "surface-2");
  assert.equal(switched.rendererGeometry?.browserContentSlot, null);
  assert.equal(browserContentBounds(switched), null);

  state = completeBrowserFrame(state, { tabId: "tab-2" });
  assert.equal(state.rendererGeometry?.browserContentSlot?.tabId, "tab-2");
  assert.deepEqual(browserContentBounds(snapshotWindowLayout(state)), {
    x: 961,
    y: 74,
    width: 478,
    height: 826,
  });
});

test("浏览器 Surface 只接受当前 Browser Tab 所在的完整 frame", () => {
  let state = activateBrowser(baseState());
  const initial = state;
  state = submitGeometry(state, {
    rightPaneBounds: { x: 960, y: 0, width: 480, height: 900 },
    browserContentSlot: { tabId: "stale-tab", bounds: { x: 961, y: 74, width: 478, height: 826 } },
  });
  assert.strictEqual(state, initial);
  assert.equal(browserContentBounds(snapshotWindowLayout(state)), null);
  assert.equal(shouldShowBrowserSurface(snapshotWindowLayout(state), true), false);
});

test("不完整、越界或父布局代次错误的 frame 被整体拒绝", () => {
  let state = completeBrowserFrame(activateBrowser(baseState()));
  const accepted = state;
  const acceptedRevision = accepted.rendererGeometry!.revision;
  const invalidFrames: Array<Parameters<typeof submitGeometry>[1]> = [
    {
      rightPaneBounds: null,
      browserContentSlot: null,
      revision: acceptedRevision + 1,
    },
    {
      rightPaneBounds: { x: 960, y: 0, width: 480, height: 900 },
      browserContentSlot: { tabId: "browser-tab", bounds: { x: 900, y: 74, width: 600, height: 826 } },
      revision: acceptedRevision + 1,
    },
    {
      rightPaneBounds: { x: 960, y: 0, width: 480, height: 900 },
      browserContentSlot: { tabId: "browser-tab", bounds: { x: 961, y: 74, width: 478, height: 826 } },
      revision: acceptedRevision + 1,
      layoutRevision: accepted.layoutRevision + 1,
    },
  ];
  for (const invalid of invalidFrames) {
    state = submitGeometry(state, invalid);
    assert.strictEqual(state, accepted);
  }

  state = submitGeometry(state, {
    rightPaneBounds: { x: 960, y: 0, width: 480, height: 900 },
    browserContentSlot: { tabId: "browser-tab", bounds: { x: 961, y: 74, width: 478, height: 826 } },
    revision: acceptedRevision + 1,
  });
  assert.notStrictEqual(state, accepted);
});

test("过期 frame 不得替换最后一次已确认的完整几何", () => {
  let state = completeBrowserFrame(activateBrowser(baseState()));
  const accepted = state;
  const acceptedRevision = accepted.rendererGeometry!.revision;
  state = submitGeometry(state, {
    rightPaneBounds: { x: 800, y: 0, width: 640, height: 900 },
    browserContentSlot: { tabId: "browser-tab", bounds: { x: 801, y: 74, width: 638, height: 826 } },
    revision: acceptedRevision,
  });
  assert.strictEqual(state, accepted);
  assert.deepEqual(snapshotWindowLayout(state).rightPaneBounds, {
    x: 960,
    y: 0,
    width: 480,
    height: 900,
  });
});

test("右栏拖动期间保留最后一次完整 frame，新 frame 原子替换全部几何", () => {
  let state = completeBrowserFrame(activateBrowser(baseState()));
  const previousFrame = state.rendererGeometry;
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 560 });
  assert.strictEqual(state.rendererGeometry, previousFrame);
  assert.deepEqual(snapshotWindowLayout(state).rightPaneBounds, previousFrame!.rightPaneBounds);

  state = completeBrowserFrame(state, {
    rightPaneBounds: { x: 880, y: 0, width: 560, height: 900 },
    contentBounds: { x: 881, y: 74, width: 558, height: 826 },
  });
  assert.deepEqual(state.rendererGeometry?.rightPaneBounds, {
    x: 880,
    y: 0,
    width: 560,
    height: 900,
  });
  assert.deepEqual(browserContentBounds(snapshotWindowLayout(state)), {
    x: 881,
    y: 74,
    width: 558,
    height: 826,
  });
});

test("Renderer 暂时读不到 DOM 几何时不提交半帧，也不清空最后一次完整 frame", () => {
  let state = completeBrowserFrame(activateBrowser(baseState()));
  const accepted = state;
  state = submitGeometry(state, {
    rightPaneBounds: null,
    browserContentSlot: null,
  });
  assert.strictEqual(state, accepted);
  assert.ok(browserContentBounds(snapshotWindowLayout(state)));
});

test("非浏览器右栏面板保留父容器 frame，但浏览器槽位不再有效", () => {
  let browserState = completeBrowserFrame(activateBrowser(baseState()), {
    rightPaneBounds: { x: 520, y: 0, width: 920, height: 900 },
    contentBounds: { x: 521, y: 74, width: 918, height: 826 },
  });
  const browser = snapshotWindowLayout(browserState);
  const code = snapshotWindowLayout(reduceWindowLayout(browserState, {
    type: "active_panel",
    kind: "code",
    tabId: "code-tab",
    surfaceId: null,
  }));
  const terminal = snapshotWindowLayout(reduceWindowLayout(browserState, {
    type: "active_panel",
    kind: "terminal",
    tabId: "terminal-tab",
    surfaceId: null,
  }));

  assert.deepEqual(code.rightPaneBounds, browser.rightPaneBounds);
  assert.deepEqual(terminal.rightPaneBounds, browser.rightPaneBounds);
  assert.equal(code.activeSurfaceId, null);
  assert.equal(code.rendererGeometry?.browserContentSlot, null);
  assert.equal(terminal.rendererGeometry?.browserContentSlot, null);
  assert.equal(browserContentBounds(code), null);
});

test("窄窗口进入 overlay 模式，但原生矩形仍只由 Renderer frame 提供", () => {
  let state = baseState(720, 600);
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  const snapshot = snapshotWindowLayout(state);
  assert.equal(snapshot.rightPaneMode, "overlay");
  assert.equal(snapshot.appBounds.width, 720);
  assert.equal(snapshot.dividerBounds, null);
  assert.equal(snapshot.rightPaneBounds, null);
});

test("内容槽尺寸变化不反向修改右栏父容器尺寸", () => {
  let state = activateBrowser(baseState());
  state = completeBrowserFrame(state, {
    rightPaneBounds: { x: 960, y: 0, width: 480, height: 900 },
    contentBounds: { x: 980, y: 100, width: 400, height: 700 },
  });
  const snapshot = snapshotWindowLayout(state);
  assert.equal(snapshot.rightPaneBounds?.width, 480);
  assert.equal(snapshot.rendererGeometry?.browserContentSlot?.bounds.width, 400);
  assert.equal(snapshot.rightPaneWidth, 480);
});

test("窗口尺寸变化只更新逻辑布局，不把新尺寸伪装成 Renderer 已确认几何", () => {
  let state = completeBrowserFrame(activateBrowser(baseState()));
  const previousFrame = state.rendererGeometry;
  state = reduceWindowLayout(state, {
    type: "client_bounds",
    bounds: { x: 0, y: 0, width: 1100, height: 760 },
    displayScaleFactor: 2,
    fullscreen: false,
  });
  assert.equal(state.displayScaleFactor, 2);
  assert.strictEqual(state.rendererGeometry, previousFrame);
  assert.deepEqual(snapshotWindowLayout(state).rightPaneBounds, previousFrame!.rightPaneBounds);
});

test("收起右栏后 Browser Surface 不再可见，但不需要制造新的浏览器 frame", () => {
  let state = completeBrowserFrame(activateBrowser(baseState()));
  const previousFrame = state.rendererGeometry;
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: false });
  const snapshot = snapshotWindowLayout(state);
  assert.strictEqual(state.rendererGeometry, previousFrame);
  assert.equal(snapshot.rightPaneBounds, null);
  assert.equal(browserContentBounds(snapshot), null);
  assert.equal(shouldShowBrowserSurface(snapshot, true), false);
});

test("invalidateRendererGeometry 明确清除原生 Surface 可消费的 frame", () => {
  let state = completeBrowserFrame(activateBrowser(baseState()));
  state = reduceWindowLayout(state, {
    type: "client_bounds",
    bounds: { x: 0, y: 0, width: 1200, height: 800 },
    displayScaleFactor: 1,
    fullscreen: false,
  });
  assert.notEqual(state.rendererGeometry, null);
});

test("逻辑布局 revision 在非几何意图上递增，Renderer frame 在同一代次内递增", () => {
  let state = baseState();
  const initialLayoutRevision = state.layoutRevision;
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  assert.equal(state.layoutRevision, initialLayoutRevision + 1);
  state = submitGeometry(state, {
    rightPaneBounds: { x: 960, y: 0, width: 480, height: 900 },
    browserContentSlot: null,
  });
  assert.equal(state.layoutRevision, initialLayoutRevision + 1);
  assert.equal(state.rendererGeometry?.revision, 0);
  state = submitGeometry(state, {
    rightPaneBounds: { x: 960, y: 0, width: 480, height: 900 },
    browserContentSlot: null,
  });
  assert.equal(state.rendererGeometry?.revision, 1);
  assert.equal(state.layoutRevision, initialLayoutRevision + 1);
});

test("双击重置只改变逻辑右栏宽度，不伪造 DOM 几何", () => {
  let state = baseState();
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 600 });
  state = reduceWindowLayout(state, { type: "right_pane_reset_width" });
  assert.equal(state.rightPaneVisible, true);
  assert.equal(state.rightPaneWidth, WINDOW_LAYOUT.defaultRightPaneWidth);
  assert.equal(snapshotWindowLayout(state).rightPaneBounds, null);
});

test("右栏宽度仍受最小内容轨道约束", () => {
  let state = baseState(1000, 800);
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  state = reduceWindowLayout(state, { type: "right_pane_width", width: 1 });
  assert.equal(state.rightPaneWidth, WINDOW_LAYOUT.minRightPaneWidth);
});
