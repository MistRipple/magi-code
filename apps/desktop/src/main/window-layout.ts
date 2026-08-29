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
} as const;

export interface Rectangle {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface BrowserContentSlot {
  // 内容槽身份必须来自产生这份 DOMRect 的 BrowserTabContent 节点。
  // 几何帧本身负责 revision，槽位不再维护第二个 revision。
  tabId: string;
  bounds: Rectangle;
}

export type RendererGeometryCoordinateSpace = "window-content-css-px";

/**
 * Renderer 在同一帧确认的完整 DOM 几何。
 *
 * 这是原生 BrowserSurface 和原生 Overlay 唯一允许消费的坐标来源。
 * layoutRevision 标识父布局意图，revision 标识 Renderer 对该父布局的
 * 确认顺序。父布局变化不会清空上一帧；新帧到达前，原生视图保持在上一
 * 个已确认位置，避免解绑/重挂造成黑屏和闪烁。
 */
export interface RendererGeometryFrame {
  revision: number;
  layoutRevision: number;
  coordinateSpace: RendererGeometryCoordinateSpace;
  rightPaneBounds: Rectangle | null;
  browserContentSlot: BrowserContentSlot | null;
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
  // Renderer reports the actual DOM geometry after CSS layout. This is
  // ephemeral window state, never session state or browser-tab state.
  rendererGeometry: RendererGeometryFrame | null;
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
    }
  | {
      type: "renderer_geometry";
      frame: RendererGeometryFrame;
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
    rendererGeometry: null,
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
      next = {
        ...state,
        rightPaneVisible: intent.visible,
      };
      break;
    case "active_panel":
      next = {
        ...state,
        activePanelKind: intent.kind,
        activeTabId: intent.tabId,
        activeSurfaceId: intent.kind === "browser" ? intent.surfaceId : null,
        rendererGeometry: state.rendererGeometry
          ? {
              ...state.rendererGeometry,
              // 同一 Tab 的面板状态变化只更新逻辑面板，不污染已确认的
              // 内容槽。切换 Browser Tab 时必须立即撤销旧槽，防止旧页面
              // 在新 Tab 的位置继续接收点击。
              browserContentSlot: intent.kind === "browser"
                && intent.tabId === state.activeTabId
                && intent.surfaceId === state.activeSurfaceId
                ? state.rendererGeometry.browserContentSlot
                : null,
            }
          : null,
      };
      break;
    case "renderer_geometry": {
      const frame = intent.frame;
      if (
        !Number.isSafeInteger(frame.revision)
        || frame.revision < 0
        || frame.revision <= (state.rendererGeometry?.revision ?? -1)
        || !Number.isSafeInteger(frame.layoutRevision)
        || frame.layoutRevision < 0
        || frame.layoutRevision !== state.layoutRevision
        || frame.coordinateSpace !== "window-content-css-px"
      ) {
        return state;
      }
      const reportedRightPaneBounds = normalizeRendererRectangle(
        frame.rightPaneBounds,
        state.clientBounds,
      );
      // 右栏可见时，父容器必须是完整帧的一部分。拒绝不完整帧而不是
      // 把上一帧的 BrowserSurface 解绑，避免 DOM 重排期间出现黑屏。
      if (state.rightPaneVisible && !reportedRightPaneBounds) return state;
      const rightPaneBounds = state.rightPaneVisible ? reportedRightPaneBounds : null;
      const normalizedBrowserContentBounds = frame.browserContentSlot
        ? normalizeRendererRectangle(frame.browserContentSlot.bounds, state.clientBounds)
        : null;
      const reportedBrowserContentSlot = frame.browserContentSlot
        && frame.browserContentSlot.tabId.trim()
        ? normalizedBrowserContentBounds
          ? {
              tabId: frame.browserContentSlot.tabId.trim(),
              bounds: normalizedBrowserContentBounds,
            }
          : null
        : null;
      const browserPanelActive = state.activePanelKind === "browser" && Boolean(state.activeTabId);
      // 当前浏览器面板必须一次提交父容器和内容槽。中间帧直接拒绝，
      // 下一次 DOM 稳定后会重新上报，不让原生视图进入错误坐标。
      if (browserPanelActive && state.rightPaneVisible && (
        !reportedBrowserContentSlot
        || reportedBrowserContentSlot.tabId !== state.activeTabId
        || !reportedRightPaneBounds
        || !containsRectangle(reportedRightPaneBounds, reportedBrowserContentSlot.bounds)
      )) return state;
      const browserContentSlot = browserPanelActive && state.rightPaneVisible
        ? reportedBrowserContentSlot
        : null;
      next = {
        ...state,
        rendererGeometry: {
          revision: frame.revision,
          layoutRevision: frame.layoutRevision,
          coordinateSpace: frame.coordinateSpace,
          rightPaneBounds,
          browserContentSlot,
        },
      };
      break;
    }
  }
  return {
    ...next,
    // layoutRevision 是父布局的 generation，不是所有状态写入的序号。
    // Renderer geometry 必须在同一 generation 内确认，避免几何响应自身
    // 再次触发一轮无意义的上报，也避免 Main 的布局写入抢占 Renderer 序列。
    layoutRevision: intent.type === "renderer_geometry"
      ? state.layoutRevision
      : state.layoutRevision + 1,
  };
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
  // Main 只拥有右栏宽度这个意图；右栏的实际位置和尺寸只能来自主
  // Renderer 已提交的 DOM 几何。这里绝不能用 clientBounds/rightPaneWidth
  // 再推导一份“看起来一样”的矩形，否则原生 View 会在 Renderer 重排期间
  // 使用另一套坐标，表现为悬浮、遮挡工具栏或覆盖中栏。
  const rightPaneBounds = state.rightPaneVisible
    ? state.rendererGeometry?.rightPaneBounds ?? null
    : null;
  return {
    ...state,
    rightPaneWidth,
    // 桌面端只保留一个可信 Renderer，它覆盖整个窗口并在 DOM 中排布左、中、右三栏。
    // 浏览器 Surface 是 contentView 中与 App View 并列的原生 View；其 bounds
    // 只来自 Renderer 已完成排版后报告的 browserContentSlot，不由 Main 再推导一套右栏布局。
    appBounds: fullBounds,
    // 分隔条紧贴已确认的右栏内容轨道左侧，不占用 rightPaneWidth。
    dividerBounds: state.rightPaneVisible && sideBySide && rightPaneBounds
      ? {
        x: Math.max(0, rightPaneBounds.x - WINDOW_LAYOUT.rightPaneResizeHandleWidth),
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
    && hasBrowserSurface
    && browserContentBounds(layout) !== null;
}

/** Renderer 上报过物理 DOM 内容槽后，Main 才允许 Browser Surface 进入命中树。 */
export function browserContentBounds(layout: WindowLayoutSnapshot): Rectangle | null {
  const frame = layout.rendererGeometry;
  const slot = frame?.browserContentSlot;
  const rightPaneBounds = frame?.rightPaneBounds;
  if (
    !frame
    || !slot
    || layout.rightPaneVisible !== true
    || layout.activePanelKind !== "browser"
    || !layout.activeTabId
    || slot.tabId !== layout.activeTabId
    || !rightPaneBounds
    || !containsRectangle(rightPaneBounds, slot.bounds)
  ) return null;
  // 内容槽必须同时属于当前 Browser Tab 和已确认的右栏父容器。
  // 父布局变化期间继续使用上一份完整帧，直到新帧原子替换它。
  return { ...slot.bounds };
}

export function invalidateRendererGeometry(state: WindowLayoutState): WindowLayoutState {
  return {
    ...state,
    rendererGeometry: null,
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

function normalizeRendererRectangle(
  value: Rectangle | null,
  clientBounds: Rectangle,
): Rectangle | null {
  if (
    !value
    || !Number.isFinite(value.x)
    || !Number.isFinite(value.y)
    || !Number.isFinite(value.width)
    || !Number.isFinite(value.height)
    || value.width <= 0
    || value.height <= 0
  ) return null;
  const right = value.x + value.width;
  const bottom = value.y + value.height;
  if (!Number.isFinite(right) || !Number.isFinite(bottom)) return null;
  if (
    value.x < 0
    || value.y < 0
    || right > clientBounds.width
    || bottom > clientBounds.height
  ) return null;

  // DOMRect 边缘可能是 fractional CSS px。按左右、上下边缘分别取整，
  // 与 Electron DIP bounds 共用同一矩形，避免 width 独立取整造成 1px
  // 的截断或重叠。
  const left = Math.max(0, Math.round(value.x));
  const top = Math.max(0, Math.round(value.y));
  const boundedRight = Math.round(right);
  const boundedBottom = Math.round(bottom);
  if (boundedRight <= left || boundedBottom <= top) return null;
  return {
    x: left,
    y: top,
    width: boundedRight - left,
    height: boundedBottom - top,
  };
}

function containsRectangle(outer: Rectangle, inner: Rectangle): boolean {
  return inner.x >= outer.x
    && inner.y >= outer.y
    && inner.x + inner.width <= outer.x + outer.width
    && inner.y + inner.height <= outer.y + outer.height;
}

function finitePositive(value: number | undefined, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : fallback;
}
