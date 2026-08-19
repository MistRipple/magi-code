export const WINDOW_LAYOUT = {
  // Desktop 的 appView 覆盖整窗，左栏和中栏由 Renderer 自己按实际 DOM
  // 几何排布。rightPaneWidth 只表示右栏内容轨道宽度；分隔条是唯一的
  // 独立轨道。Desktop Renderer 的根网格没有额外外边距，Main 必须使用
  // 同一坐标系，否则原生 Surface 和右栏 DOM 会产生固定偏移。
  minRightPaneWidth: 320,
  defaultRightPaneWidth: 480,
  maxRightPaneRatio: 2 / 3,
  overlayBreakpoint: 840,
  minWorkbenchContentWidth: 448,
  rightPaneBorder: 1,
  rightPaneResizeHandleWidth: 8,
  rightPaneTabBarHeight: 38,
  browserToolbarHeight: 36,
} as const;

export interface Rectangle {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type RightPaneMode = "side-by-side" | "overlay";
export type PanelKind = "agent" | "browser" | "code" | "terminal" | null;

export interface WindowLayoutState {
  desktopEpoch: string;
  windowId: string;
  layoutRevision: number;
  clientBounds: Rectangle;
  displayScaleFactor: number;
  fullscreen: boolean;
  rightPaneVisible: boolean;
  rightPaneMode: RightPaneMode;
  rightPaneWidth: number;
  activePanelKind: PanelKind;
  activeTabId: string | null;
  activeSurfaceId: string | null;
}

export interface WindowLayoutSnapshot extends WindowLayoutState {
  appBounds: Rectangle;
  dividerBounds: Rectangle | null;
  rightPaneBounds: Rectangle | null;
}

export type WindowLayoutIntent =
  | { type: "client_bounds"; bounds: Rectangle; displayScaleFactor: number; fullscreen: boolean }
  | { type: "right_pane_width"; width: number }
  | { type: "right_pane_reset_width" }
  | { type: "right_pane_visibility"; visible: boolean }
  | {
      type: "active_panel";
      kind: PanelKind;
      tabId: string | null;
      surfaceId: string | null;
    };

export function createWindowLayoutState(input: {
  desktopEpoch: string;
  windowId: string;
  clientBounds: Rectangle;
  displayScaleFactor?: number;
}): WindowLayoutState {
  const mode = resolveRightPaneMode(input.clientBounds.width);
  return {
    desktopEpoch: input.desktopEpoch,
    windowId: input.windowId,
    layoutRevision: 0,
    clientBounds: normalizeRectangle(input.clientBounds),
    displayScaleFactor: finitePositive(input.displayScaleFactor, 1),
    fullscreen: false,
    rightPaneVisible: false,
    rightPaneMode: mode,
    rightPaneWidth: clampRightPaneWidth(
      WINDOW_LAYOUT.defaultRightPaneWidth,
      input.clientBounds.width,
      mode,
    ),
    activePanelKind: null,
    activeTabId: null,
    activeSurfaceId: null,
  };
}

export function reduceWindowLayout(
  state: WindowLayoutState,
  intent: WindowLayoutIntent,
): WindowLayoutState {
  let next: WindowLayoutState;
  switch (intent.type) {
    case "client_bounds": {
      const bounds = normalizeRectangle(intent.bounds);
      const mode = resolveRightPaneMode(bounds.width);
      next = {
        ...state,
        clientBounds: bounds,
        displayScaleFactor: finitePositive(intent.displayScaleFactor, 1),
        fullscreen: intent.fullscreen,
        rightPaneMode: mode,
        rightPaneWidth: clampRightPaneWidth(state.rightPaneWidth, bounds.width, mode),
      };
      break;
    }
    case "right_pane_width":
      next = {
        ...state,
        rightPaneWidth: clampRightPaneWidth(
          intent.width,
          state.clientBounds.width,
          state.rightPaneMode,
        ),
      };
      break;
    case "right_pane_reset_width":
      next = {
        ...state,
        rightPaneWidth: clampRightPaneWidth(
          WINDOW_LAYOUT.defaultRightPaneWidth,
          state.clientBounds.width,
          state.rightPaneMode,
        ),
      };
      break;
    case "right_pane_visibility":
      next = { ...state, rightPaneVisible: intent.visible };
      break;
    case "active_panel":
      next = {
        ...state,
        activePanelKind: intent.kind,
        activeTabId: intent.tabId,
        activeSurfaceId: intent.kind === "browser" ? intent.surfaceId : null,
      };
      break;
  }
  return { ...next, layoutRevision: state.layoutRevision + 1 };
}

export function snapshotWindowLayout(state: WindowLayoutState): WindowLayoutSnapshot {
  const { width, height } = state.clientBounds;
  const fullBounds = { x: 0, y: 0, width, height };
  const rightPaneWidth = clampRightPaneWidth(
    state.rightPaneWidth,
    width,
    state.rightPaneMode,
  );
  const sideBySide = state.rightPaneMode === "side-by-side";
  const rightPaneX = Math.max(0, width - rightPaneWidth);
  // rightPaneBounds 与 Renderer 的右栏内容轨道一一对应，不包含分隔条和
  // 任何窗口外边距。这样 Overlay 可以直接使用与 Renderer 相同的坐标系。
  const rightPaneBounds = {
    x: rightPaneX,
    y: 0,
    width: rightPaneWidth,
    height,
  };
  return {
    ...state,
    rightPaneWidth,
    // 桌面端只保留一个可信 Renderer，它覆盖整个窗口并在 DOM 中排布左、中、右三栏。
    // 右栏的几何只由主 Renderer 的 DOM 布局消费；浏览器 Guest 是当前
    // Browser Tab 的 DOM 子节点，不在 Main 侧维护第二套内容槽坐标。
    appBounds: fullBounds,
    // 分隔条紧贴右栏内容轨道左侧，不占用 rightPaneWidth。
    dividerBounds: state.rightPaneVisible && sideBySide
      ? {
        x: Math.max(0, rightPaneX - WINDOW_LAYOUT.rightPaneResizeHandleWidth),
        y: 0,
        width: WINDOW_LAYOUT.rightPaneResizeHandleWidth,
        height,
      }
      : null,
    rightPaneBounds,
  };
}

export function resolveRightPaneMode(windowWidth: number): RightPaneMode {
  return windowWidth < WINDOW_LAYOUT.overlayBreakpoint ? "overlay" : "side-by-side";
}

export function shouldShowBrowserSurface(
  layout: WindowLayoutSnapshot,
  hasBrowserSurface: boolean,
): boolean {
  return layout.rightPaneVisible
    && layout.activePanelKind === "browser"
    && Boolean(layout.activeTabId)
    && Boolean(layout.activeSurfaceId)
    && hasBrowserSurface;
}

/**
 * Browser Surface 的物理内容槽由 Main 的布局事务唯一计算。
 * 右栏 DOM 使用同一组固定 Chrome 高度，因此不再把 ResizeObserver 的
 * 窗口坐标往返给 Main，也不会在拖动期间产生两个几何所有者。
 */
export function browserContentBounds(layout: WindowLayoutSnapshot): Rectangle | null {
  if (!layout.rightPaneBounds) return null;
  const chromeHeight = WINDOW_LAYOUT.rightPaneTabBarHeight + WINDOW_LAYOUT.browserToolbarHeight;
  const height = layout.rightPaneBounds.height - chromeHeight;
  if (layout.rightPaneBounds.width <= 0 || height <= 0) return null;
  return {
    x: layout.rightPaneBounds.x,
    y: layout.rightPaneBounds.y + chromeHeight,
    width: layout.rightPaneBounds.width,
    height,
  };
}

export function clampRightPaneWidth(
  requestedWidth: number,
  windowWidth: number,
  mode: RightPaneMode,
): number {
  const available = Math.max(1, Math.round(windowWidth));
  const maxByRatio = Math.floor(available * WINDOW_LAYOUT.maxRightPaneRatio);
  const maxByWorkbench = mode === "side-by-side"
    ? available
      - WINDOW_LAYOUT.rightPaneResizeHandleWidth
      - WINDOW_LAYOUT.minWorkbenchContentWidth
    : available;
  const maximum = Math.max(
    Math.min(WINDOW_LAYOUT.minRightPaneWidth, available),
    Math.min(maxByRatio, maxByWorkbench),
  );
  const minimum = Math.min(WINDOW_LAYOUT.minRightPaneWidth, maximum);
  return Math.min(maximum, Math.max(minimum, Math.round(requestedWidth)));
}

function normalizeRectangle(value: Rectangle): Rectangle {
  return {
    x: Math.round(Number.isFinite(value.x) ? value.x : 0),
    y: Math.round(Number.isFinite(value.y) ? value.y : 0),
    width: Math.max(1, Math.round(Number.isFinite(value.width) ? value.width : 1)),
    height: Math.max(1, Math.round(Number.isFinite(value.height) ? value.height : 1)),
  };
}

function finitePositive(value: number | undefined, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : fallback;
}
