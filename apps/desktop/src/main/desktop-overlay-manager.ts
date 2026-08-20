import type { BaseWindow, Rectangle, View, WebContentsView } from "electron";
import { WebContentsView as ElectronWebContentsView } from "electron";
import type { WindowLayoutSnapshot } from "./window-layout.js";

export type DesktopOverlayPlacement =
  | "right-pane-add"
  | "browser-viewport"
  | "browser-annotations";

export interface DesktopOverlayItem {
  id: string;
  label: string;
  icon: string | null;
  selected: boolean;
  disabled: boolean;
}

export interface DesktopOverlayField {
  id: string;
  label: string;
  type: "number" | "text";
  value: string;
  min: number | null;
  max: number | null;
}

export interface DesktopOverlayState {
  overlayId: string;
  kind: "menu" | "annotation";
  phase: "menu" | "select" | "comment";
  ownerId: string;
  placement: DesktopOverlayPlacement;
  anchorBounds: Rectangle | null;
  title: string;
  items: DesktopOverlayItem[];
  fields: DesktopOverlayField[];
}

export interface DesktopOverlayAction {
  overlayId: string;
  kind: "menu" | "annotation";
  ownerId: string;
  interaction: "select" | "input";
  id: string;
  value: string | null;
}

interface OverlayRecord {
  windowId: string;
  window: BaseWindow;
  view: WebContentsView;
  layer: View;
  state: DesktopOverlayState | null;
  visible: boolean;
  loaded: boolean;
  loadFailed: boolean;
  ready: boolean;
  mounted: boolean;
  layout: WindowLayoutSnapshot | null;
  browserContentBounds: Rectangle | null;
  loadPromise: Promise<void> | null;
}

const OVERLAY_WIDTH: Record<DesktopOverlayPlacement, number> = {
  "right-pane-add": 184,
  "browser-viewport": 264,
  "browser-annotations": 320,
};
const TRANSPARENT_VIEW_BACKGROUND = "rgba(0, 0, 0, 0)";

export class DesktopOverlayManager {
  readonly #preloadPath: string;
  readonly #agentOrigin: string;
  readonly #desktopEpoch: string;
  readonly #records = new Map<string, OverlayRecord>();
  readonly #onAction: (windowId: string, action: DesktopOverlayAction) => void;
  readonly #onClosed: (windowId: string) => void;

  constructor(input: {
    preloadPath: string;
    agentOrigin: string;
    desktopEpoch: string;
    onAction: (windowId: string, action: DesktopOverlayAction) => void;
    onClosed: (windowId: string) => void;
  }) {
    this.#preloadPath = input.preloadPath;
    this.#agentOrigin = input.agentOrigin;
    this.#desktopEpoch = input.desktopEpoch;
    this.#onAction = input.onAction;
    this.#onClosed = input.onClosed;
  }

  create(windowId: string, window: BaseWindow, layer: View): void {
    if (this.#records.has(windowId)) return;
    const view = new ElectronWebContentsView({
      webPreferences: {
        preload: this.#preloadPath,
        additionalArguments: [
          "--magi-desktop-surface=overlay",
          `--magi-desktop-window-id=${windowId}`,
        ],
        partition: "persist:magi-app",
        nodeIntegration: false,
        contextIsolation: true,
        sandbox: true,
        webSecurity: true,
      },
    });
    const record: OverlayRecord = {
      windowId,
      window,
      view,
      layer,
      state: null,
      visible: false,
      loaded: false,
      loadFailed: false,
      ready: false,
      mounted: false,
      layout: null,
      browserContentBounds: null,
      loadPromise: null,
    };
    this.#records.set(windowId, record);
    // Overlay 是浏览器内容槽上的透明交互层。Electron 的原生 View 默认
    // 会绘制不透明背景；仅依赖 DOM 的 background: transparent 不足以让
    // 下方 WebContentsView 可见，最终表现为打开标记后整个页面黑屏。
    layer.setBackgroundColor(TRANSPARENT_VIEW_BACKGROUND);
    view.setBackgroundColor(TRANSPARENT_VIEW_BACKGROUND);
    // OverlayLayer 固定存在于窗口层级中，但空闲时必须隐藏整个层，不能
    // 让一个没有子内容的原生 View 覆盖 App Renderer 的交互区域。
    this.syncVisibility(record);
    view.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
    view.webContents.on("will-navigate", (event, url) => {
      if (!this.isTrustedRendererUrl(url, windowId)) event.preventDefault();
    });
    view.webContents.on("did-finish-load", () => {
      if (record.loadFailed) return;
      record.loaded = true;
      if (record.state && record.visible) {
        this.setOverlayBounds(record, overlayBoundsForRecord(record));
      }
      this.syncVisibility(record);
      if (record.state && record.visible && record.ready) this.publishState(record);
    });
    view.webContents.on("did-fail-load", (_event, _errorCode, _errorDescription, _validatedURL, isMainFrame) => {
      if (!isMainFrame) return;
      record.loaded = false;
      record.ready = false;
      record.loadFailed = true;
      this.syncVisibility(record);
    });
    view.webContents.on("render-process-gone", () => {
      record.loaded = false;
      record.ready = false;
      record.loadFailed = true;
      this.syncVisibility(record);
      if (record.visible && !window.isDestroyed()) {
        void this.loadRenderer(record);
      }
    });
    void this.loadRenderer(record);
  }

  async restoreAfterDaemonReady(): Promise<void> {
    const failed = [...this.#records.values()].filter((record) => (
      record.loadFailed && !record.view.webContents.isDestroyed()
    ));
    await Promise.all(failed.map((record) => this.loadRenderer(record)));
  }

  open(
    windowId: string,
    state: DesktopOverlayState,
    layout: WindowLayoutSnapshot,
    browserContentBounds: Rectangle | null = null,
  ): void {
    const record = this.requireRecord(windowId);
    record.state = state;
    record.visible = true;
    record.layout = layout;
    record.browserContentBounds = browserContentBounds ? { ...browserContentBounds } : null;
    this.mountOnLayer(record);
    this.setOverlayBounds(record, overlayBounds(layout, state, record.browserContentBounds));
    this.syncVisibility(record);
    if (record.loadFailed && !record.view.webContents.isDestroyed()) {
      // Overlay Renderer 可能在 daemon/Vite 刚重启的瞬间加载失败。下一次
      // 打开必须主动重试，不能把一次瞬时失败永久变成“按钮无响应”。
      void this.loadRenderer(record);
    }
    if (record.loaded && record.ready) this.publishState(record);
  }

  handleReady(windowId: string): void {
    const record = this.#records.get(windowId);
    // Renderer 的握手可能早于 did-finish-load 事件抵达主进程。就绪是
    // Renderer 自身的状态，不能因为主进程尚未刷新 loaded 标记而丢弃；
    // syncVisibility 会在两个状态都满足后再显示视图，did-finish-load
    // 也会负责补发当前状态。
    if (!record || record.loadFailed || record.view.webContents.isDestroyed()) return;
    record.ready = true;
    if (record.state && record.visible) {
      this.setOverlayBounds(record, overlayBoundsForRecord(record));
    }
    this.syncVisibility(record);
    if (record.visible && record.state) this.publishState(record);
  }

  close(windowId: string): void {
    const record = this.#records.get(windowId);
    if (!record) return;
    record.visible = false;
    record.state = null;
    record.layout = null;
    record.browserContentBounds = null;
    if (!record.view.webContents.isDestroyed()) {
      record.view.webContents.send("magi-desktop:overlay-closed", null);
    }
    this.syncVisibility(record);
    // 仅隐藏会让原生 Accessibility 树继续保留已关闭菜单并让焦点停在
    // Overlay Renderer。移除视图但保留 WebContents，下一次打开仍复用同一
    // Renderer，不会重建页面或引入闪烁。
    if (record.mounted) {
      record.layer.removeChildView(record.view);
      record.mounted = false;
    }
    this.#onClosed(windowId);
  }

  updateLayout(
    windowId: string,
    layout: WindowLayoutSnapshot,
    browserContentBounds: Rectangle | null = null,
  ): void {
    const record = this.#records.get(windowId);
    if (!record || !record.visible || !record.state) return;
    record.layout = layout;
    record.browserContentBounds = browserContentBounds ? { ...browserContentBounds } : null;
    // rightPaneBounds 是几何轨道，即使右栏当前折叠也会存在；可见性必须
    // 由 rightPaneVisible 判断。切换到其他面板时，浏览器弹出层也必须
    // 立即关闭，避免旧菜单留在主 Renderer 之外继续拦截输入。
    const browserOverlay = record.state.ownerId.startsWith("browser:");
    if (
      !layout.rightPaneVisible
      || (browserOverlay && layout.activePanelKind !== "browser")
      || (browserOverlay && layout.activeTabId !== record.state.ownerId.slice("browser:".length))
      || (record.state.kind === "annotation" && !record.browserContentBounds)
    ) {
      this.close(windowId);
      return;
    }
    this.mountOnLayer(record);
    this.setOverlayBounds(record, overlayBounds(layout, record.state, record.browserContentBounds));
    this.syncVisibility(record);
  }

  handleAction(windowId: string, action: DesktopOverlayAction): void {
    const record = this.#records.get(windowId);
    const state = record?.state;
    if (!record || !record.visible || !state) return;
    const knownAction = (
      state.items.some((item) => item.id === action.id && !item.disabled)
      || state.fields.some((field) => field.id === action.id)
      || (state.kind === "annotation" && ["selection", "save", "cancel"].includes(action.id))
    );
    if (
      action.overlayId !== state.overlayId
      || action.kind !== state.kind
      || action.ownerId !== state.ownerId
      || !knownAction
    ) {
      return;
    }
    const shouldCloseBeforeDispatch = (
      action.interaction === "select"
      && !(state.kind === "annotation" && action.id === "selection")
    );
    // 先收口原生 Overlay，再把选择事件交给 App Renderer。选择事件通常会
    // 立即切换右栏 Tab、物化 Browser Surface 或重新布局窗口；如果先广播，
    // 这些布局事务会与 Overlay 的关闭/卸载重入，导致菜单残留并继续拦截
    // 鼠标和键盘输入。
    if (shouldCloseBeforeDispatch) this.close(windowId);
    this.#onAction(windowId, action);
  }

  isWebContents(webContentsId: number): boolean {
    return [...this.#records.values()].some((record) => record.view.webContents.id === webContentsId);
  }

  windowIdForWebContents(webContentsId: number): string | null {
    for (const record of this.#records.values()) {
      if (record.view.webContents.id === webContentsId) return record.windowId;
    }
    return null;
  }

  closeWindow(windowId: string): void {
    const record = this.#records.get(windowId);
    if (!record) return;
    record.visible = false;
    record.state = null;
    record.browserContentBounds = null;
    this.syncVisibility(record);
    if (!record.window.isDestroyed() && record.mounted) {
      record.layer.removeChildView(record.view);
      record.mounted = false;
    }
    if (!record.view.webContents.isDestroyed()) record.view.webContents.close();
    this.#records.delete(windowId);
  }

  closeAll(): void {
    for (const windowId of [...this.#records.keys()]) this.closeWindow(windowId);
  }

  private publishState(record: OverlayRecord): void {
    if (
      !record.state
      || !record.visible
      || !record.loaded
      || !record.ready
      || record.loadFailed
      || record.view.webContents.isDestroyed()
    ) return;
    record.view.webContents.send("magi-desktop:overlay-state", record.state);
  }

  private async loadRenderer(record: OverlayRecord): Promise<void> {
    if (record.window.isDestroyed() || record.view.webContents.isDestroyed()) return;
    if (record.loadPromise) return record.loadPromise;
    record.loaded = false;
    record.ready = false;
    record.loadFailed = false;
    const load = (async () => {
      try {
        await record.view.webContents.loadURL(this.rendererUrl(record.windowId));
        if (record.window.isDestroyed() || record.view.webContents.isDestroyed()) return;
        record.loaded = true;
        record.loadFailed = false;
        this.syncVisibility(record);
        if (record.state && record.visible && record.ready) this.publishState(record);
      } catch {
        if (record.window.isDestroyed() || record.view.webContents.isDestroyed()) return;
        record.loaded = false;
        record.ready = false;
        record.loadFailed = true;
        this.syncVisibility(record);
      }
    })();
    record.loadPromise = load;
    await load.finally(() => {
      if (record.loadPromise === load) record.loadPromise = null;
    });
  }

  private syncVisibility(record: OverlayRecord): void {
    if (record.window.isDestroyed()) return;
    const visible = (
      !record.view.webContents.isDestroyed()
      && record.visible
      && record.loaded
      && record.ready
      && !record.loadFailed
    );
    // Electron 的原生 View 即使设为不可见，也可能继续参与父窗口的命中测试。
    // OverlayLayer 是 App Renderer 的最后一层，隐藏时必须同时收敛为零尺寸，
    // 确保启动阶段、关闭弹层和 Renderer 恢复期间不会挡住整窗 DOM。
    if (!visible) {
      record.layer.setBounds({ x: 0, y: 0, width: 0, height: 0 });
    }
    record.layer.setVisible(visible);
    record.view.setVisible(visible);
    if (visible && !record.view.webContents.isFocused()) record.view.webContents.focus();
  }

  private setOverlayBounds(record: OverlayRecord, bounds: Rectangle): void {
    // OverlayLayer 只覆盖真实弹层区域，避免一个可见的整窗父层拦截
    // App Renderer 在弹层之外的鼠标事件。子视图使用父层内坐标。
    record.layer.setBounds(bounds);
    record.view.setBounds({ x: 0, y: 0, width: bounds.width, height: bounds.height });
  }

  private mountOnLayer(record: OverlayRecord): void {
    if (record.window.isDestroyed()) return;
    if (!record.mounted) {
      // Overlay WebContentsView 只允许作为 OverlayLayer 的子视图存在，
      // 这样它不会绕过主 Renderer 的右栏框架。
      record.layer.addChildView(record.view);
      record.mounted = true;
    }
    // OverlayLayer 在 WindowManager 创建窗口时就以固定的最后层级挂在
    // contentView 上。这里只挂载 Overlay WebContentsView，不重新插入
    // OverlayLayer，避免原生层级在窗口拖动/重排时被重复重建。
  }

  private rendererUrl(windowId: string): string {
    const url = new URL("/web.html", this.#agentOrigin);
    url.searchParams.set("desktopSurface", "overlay");
    url.searchParams.set("desktopWindowId", windowId);
    // Overlay Renderer 与 App Renderer 必须使用同一桌面启动代次，避免
    // 持久化 partition 让弹层继续运行旧的前端代码。
    url.searchParams.set("desktopEpoch", this.#desktopEpoch);
    return url.href;
  }

  private isTrustedRendererUrl(value: string, windowId: string): boolean {
    try {
      const url = new URL(value);
      return url.origin === this.#agentOrigin
        && url.pathname === "/web.html"
        && url.searchParams.get("desktopSurface") === "overlay"
        && url.searchParams.get("desktopWindowId") === windowId;
    } catch {
      return false;
    }
  }

  private requireRecord(windowId: string): OverlayRecord {
    const record = this.#records.get(windowId);
    if (!record || record.window.isDestroyed()) throw new Error("desktop_overlay_not_found");
    return record;
  }
}

function overlayBoundsForRecord(record: OverlayRecord): Rectangle {
  if (!record.layout || !record.state) {
    return { x: 0, y: 0, width: 0, height: 0 };
  }
  return overlayBounds(record.layout, record.state, record.browserContentBounds);
}

function overlayBounds(
  layout: WindowLayoutSnapshot,
  state: DesktopOverlayState,
  browserContentBounds: Rectangle | null,
): Rectangle {
  // 标记选择和备注编辑都属于当前页面内容，不是顶部菜单。二者使用同一
  // Browser 内容槽，选择框与最终截图才会保持相同坐标系。
  if (state.kind === "annotation") {
    if (!browserContentBounds) throw new Error("desktop_overlay_browser_content_unavailable");
    return { ...browserContentBounds };
  }
  const pane = layout.rightPaneBounds;
  if (!pane) throw new Error("desktop_overlay_right_pane_unavailable");
  const anchor = state.anchorBounds;
  if (!anchor) throw new Error("desktop_overlay_anchor_unavailable");
  const width = Math.min(
    OVERLAY_WIDTH[state.placement],
    Math.max(160, pane.width - 12),
  );
  const itemHeight = 32;
  const fieldHeight = 58;
  const contentHeight = 12
    + state.items.length * itemHeight
    + state.fields.length * fieldHeight
    + (state.items.length > 0 && state.fields.length > 0 ? 8 : 0);
  const height = Math.min(420, Math.max(42, contentHeight));
  const paneRight = pane.x + pane.width;
  const paneBottom = pane.y + pane.height;
  const x = clamp(
    anchor.x + anchor.width - width,
    pane.x + 4,
    paneRight - width - 4,
  );
  const below = anchor.y + anchor.height + 4;
  const above = anchor.y - height - 4;
  const y = below + height <= paneBottom - 4
    ? below
    : Math.max(pane.y + 4, above);
  return { x, y, width, height };
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(Math.max(minimum, maximum), Math.max(minimum, value));
}
