export const WINDOW_LAYOUT = {
  minRightPaneWidth: 320,
  defaultRightPaneWidth: 480,
  maxRightPaneRatio: 2 / 3,
  overlayBreakpoint: 840,
  minWorkbenchContentWidth: 448,
  rightPaneResizeHandleWidth: 8,
} as const;

export interface Rectangle {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type RightPaneMode = "side-by-side" | "overlay";
export type PanelKind = "agent" | "browser" | "code" | "terminal" | null;

/** 主进程只保存窗口布局意图；浏览器内容由 Renderer 的 webview 自然布局。 */
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
  /** 仅供通用原生浮层锚点使用，不得作为浏览器内容入口。 */
  rightPaneBounds: Rectangle | null;
}

export type WindowLayoutIntent =
  | { type: "client_bounds"; bounds: Rectangle; displayScaleFactor: number; fullscreen: boolean }
  | { type: "right_pane_width"; width: number }
  | { type: "right_pane_reset_width" }
  | { type: "right_pane_visibility"; visible: boolean }
  | { type: "active_panel"; kind: PanelKind; tabId: string | null; surfaceId: string | null };

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
    rightPaneWidth: clampRightPaneWidth(WINDOW_LAYOUT.defaultRightPaneWidth, input.clientBounds.width, mode),
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
      next = { ...state, rightPaneWidth: clampRightPaneWidth(intent.width, state.clientBounds.width, state.rightPaneMode) };
      break;
    case "right_pane_reset_width":
      next = {
        ...state,
        rightPaneWidth: clampRightPaneWidth(WINDOW_LAYOUT.defaultRightPaneWidth, state.clientBounds.width, state.rightPaneMode),
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
  const rightPaneWidth = clampRightPaneWidth(state.rightPaneWidth, width, state.rightPaneMode);
  const sideBySide = state.rightPaneMode === "side-by-side";
  const rightPaneBounds = state.rightPaneVisible
    ? sideBySide
      ? { x: width - rightPaneWidth, y: 0, width: rightPaneWidth, height }
      : { x: 0, y: 0, width, height }
    : null;
  return {
    ...state,
    rightPaneWidth,
    appBounds: fullBounds,
    dividerBounds: state.rightPaneVisible && sideBySide && rightPaneBounds
      ? { x: Math.max(0, rightPaneBounds.x - WINDOW_LAYOUT.rightPaneResizeHandleWidth), y: 0, width: WINDOW_LAYOUT.rightPaneResizeHandleWidth, height }
      : null,
    rightPaneBounds,
  };
}

export function resolveRightPaneMode(windowWidth: number): RightPaneMode {
  return windowWidth < WINDOW_LAYOUT.overlayBreakpoint ? "overlay" : "side-by-side";
}

export function clampRightPaneWidth(requestedWidth: number, windowWidth: number, mode: RightPaneMode): number {
  const available = Math.max(1, Math.round(windowWidth));
  const maxByRatio = Math.floor(available * WINDOW_LAYOUT.maxRightPaneRatio);
  const maxByWorkbench = mode === "side-by-side"
    ? available - WINDOW_LAYOUT.rightPaneResizeHandleWidth - WINDOW_LAYOUT.minWorkbenchContentWidth
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
