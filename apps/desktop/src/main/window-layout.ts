export const WINDOW_LAYOUT = {
  dividerWidth: 8,
  minAppWidth: 360,
  minRightPaneWidth: 320,
  defaultRightPaneWidth: 480,
  maxRightPaneRatio: 2 / 3,
  overlayBreakpoint: 840,
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
  browserSurfaceBounds: Rectangle | null;
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
  const dividerWidth = sideBySide ? WINDOW_LAYOUT.dividerWidth : 0;
  const rightPaneX = Math.max(0, width - rightPaneWidth);
  const appWidth = sideBySide ? Math.max(0, rightPaneX - dividerWidth) : width;
  const rightPaneBounds = { x: rightPaneX, y: 0, width: rightPaneWidth, height };
  const browserTop = WINDOW_LAYOUT.rightPaneBorder
    + WINDOW_LAYOUT.rightPaneTabBarHeight
    + WINDOW_LAYOUT.browserToolbarHeight;
  const browserSurfaceBounds = state.rightPaneVisible
    && state.activePanelKind === "browser"
    && state.activeSurfaceId
      ? {
        // 拖拽手柄位于 RightPaneChromeView 左边缘。原生 BrowserSurface 必须从
        // 命中区之后开始，否则浏览器 Tab 激活后会因 z-order 覆盖分隔条。
        x: rightPaneX + WINDOW_LAYOUT.rightPaneResizeHandleWidth,
        y: browserTop,
        width: Math.max(
          1,
          rightPaneWidth
            - WINDOW_LAYOUT.rightPaneResizeHandleWidth
            - WINDOW_LAYOUT.rightPaneBorder,
        ),
        height: Math.max(1, height - browserTop - WINDOW_LAYOUT.rightPaneBorder),
      }
    : null;

  return {
    ...state,
    rightPaneWidth,
    appBounds: state.rightPaneVisible ? { x: 0, y: 0, width: appWidth, height } : fullBounds,
    dividerBounds: state.rightPaneVisible && sideBySide
      ? { x: appWidth, y: 0, width: dividerWidth, height }
      : null,
    rightPaneBounds,
    browserSurfaceBounds,
  };
}

export function resolveRightPaneMode(windowWidth: number): RightPaneMode {
  return windowWidth < WINDOW_LAYOUT.overlayBreakpoint ? "overlay" : "side-by-side";
}

export function clampRightPaneWidth(
  requestedWidth: number,
  windowWidth: number,
  mode: RightPaneMode,
): number {
  const available = Math.max(1, Math.round(windowWidth));
  const maxByRatio = Math.floor(available * WINDOW_LAYOUT.maxRightPaneRatio);
  const maxByApp = mode === "side-by-side"
    ? available - WINDOW_LAYOUT.dividerWidth - WINDOW_LAYOUT.minAppWidth
    : available;
  const maximum = Math.max(
    Math.min(WINDOW_LAYOUT.minRightPaneWidth, available),
    Math.min(maxByRatio, maxByApp),
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
