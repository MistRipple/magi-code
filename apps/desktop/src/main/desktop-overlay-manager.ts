import { createRequire } from "node:module";
import type { BaseWindow, Rectangle, View, WebContentsView } from "electron";
import type { WindowLayoutSnapshot } from "./window-layout.js";
import {
  createDesktopOverlayClosedIdentityEvent,
  sameDesktopOverlayIdentity,
  type DesktopOverlayClosedIdentityEvent,
  type DesktopOverlayIdentity,
} from "./desktop-overlay-lifecycle.js";

export type { DesktopOverlayIdentity } from "./desktop-overlay-lifecycle.js";

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

export type DesktopOverlayKind = "menu" | "annotation";
export type DesktopOverlayPhase = "menu" | "select" | "comment";

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
  kind: DesktopOverlayKind;
  phase: DesktopOverlayPhase;
  ownerId: string;
  placement: DesktopOverlayPlacement;
  // 菜单由 App Renderer 读取真实 DOM 锚点后提交的最终矩形。标记流程不使用。
  popupBounds: Rectangle | null;
  title: string;
  items: DesktopOverlayItem[];
  fields: DesktopOverlayField[];
}

export interface DesktopOverlayAction {
  overlayId: string;
  kind: DesktopOverlayKind;
  ownerId: string;
  interaction: "select" | "input";
  id: string;
  value: string | null;
}

export interface DesktopOverlayClosedEvent extends DesktopOverlayClosedIdentityEvent {
  kind: DesktopOverlayKind;
}

export type DesktopOverlayCloseRequest = DesktopOverlayIdentity;

interface OverlayRecord {
  windowId: string;
  window: BaseWindow;
  view: WebContentsView | null;
  contentRoot: View;
  state: DesktopOverlayState | null;
  visible: boolean;
  loaded: boolean;
  loadFailed: boolean;
  ready: boolean;
  // 当前 bounds 是否来自最近一次完整 Renderer 几何帧。
  geometryAvailable: boolean;
  mounted: boolean;
  layout: WindowLayoutSnapshot | null;
  loadPromise: Promise<void> | null;
  viewportAppliedBounds: Rectangle | null;
  viewportApplyPromise: Promise<void> | null;
  viewportApplyDirty: boolean;
}

const TRANSPARENT_VIEW_BACKGROUND = "rgba(0, 0, 0, 0)";
const requireElectron = createRequire(import.meta.url);

export type DesktopOverlayViewFactory = (options: {
  webPreferences: {
    preload: string;
    additionalArguments: string[];
    partition: string;
    nodeIntegration: false;
    contextIsolation: true;
    sandbox: true;
    webSecurity: true;
  };
}) => WebContentsView;

export class DesktopOverlayManager {
  readonly #preloadPath: string;
  readonly #agentOrigin: string;
  readonly #desktopEpoch: string;
  readonly #createView: DesktopOverlayViewFactory;
  readonly #records = new Map<string, OverlayRecord>();
  readonly #onAction: (windowId: string, action: DesktopOverlayAction) => void;
  readonly #onClosed: (windowId: string, event: DesktopOverlayClosedEvent) => void;

  constructor(input: {
    preloadPath: string;
    agentOrigin: string;
    desktopEpoch: string;
    createView?: DesktopOverlayViewFactory;
    onAction: (windowId: string, action: DesktopOverlayAction) => void;
    onClosed: (windowId: string, event: DesktopOverlayClosedEvent) => void;
  }) {
    this.#preloadPath = input.preloadPath;
    this.#agentOrigin = input.agentOrigin;
    this.#desktopEpoch = input.desktopEpoch;
    this.#createView = input.createView ?? ((options) => {
      const electron = requireElectron("electron") as typeof import("electron");
      return new electron.WebContentsView(options);
    });
    this.#onAction = input.onAction;
    this.#onClosed = input.onClosed;
  }

  create(windowId: string, window: BaseWindow, contentRoot: View): void {
    if (this.#records.has(windowId)) return;
    const record: OverlayRecord = {
      windowId,
      window,
      view: null,
      contentRoot,
      state: null,
      visible: false,
      loaded: false,
      loadFailed: false,
      ready: false,
      geometryAvailable: false,
      mounted: false,
      layout: null,
      loadPromise: null,
      viewportAppliedBounds: null,
      viewportApplyPromise: null,
      viewportApplyDirty: false,
    };
    this.#records.set(windowId, record);
    this.createRenderer(record);
  }

  async restoreAfterDaemonReady(): Promise<void> {
    const failed = [...this.#records.values()].filter((record) => (
      record.loadFailed && !!record.view && !record.view.webContents.isDestroyed()
    ));
    await Promise.all(failed.map((record) => this.loadRenderer(record)));
  }

  open(
    windowId: string,
    state: DesktopOverlayState,
    layout: WindowLayoutSnapshot,
  ): void {
    const record = this.requireRecord(windowId);
    const previous = record.state;
    // 打开和后续重排都必须消费同一份 Renderer geometry frame，避免
    // 发起方的旧坐标把浮层放到浏览器内容或工具栏之上。
    const geometry = resolveCurrentOverlayGeometry(state, layout);
    if (!geometry) {
      throw new Error("desktop_overlay_browser_content_unavailable");
    }
    // 一个窗口只有一个原生 Overlay WebContents。替换必须先完成新状态的
    // 边界校验，再以单个 ownership 事务通知旧 owner。这样新 Overlay 打开
    // 失败时旧 Overlay 仍然有效，且替换不会移除/重挂 WebContents 造成闪烁。
    if (record.visible && previous && !sameDesktopOverlayIdentity(previous, state)) {
      this.notifyClosed(record, {
        kind: previous.kind,
        ...createDesktopOverlayClosedIdentityEvent(
          previous,
          "replaced",
          { overlayId: state.overlayId, ownerId: state.ownerId },
        ),
      });
    }
    record.state = state;
    record.visible = true;
    record.layout = layout;
    record.geometryAvailable = true;
    this.mountOnLayer(record);
    this.setOverlayBounds(record, geometry.bounds);
    this.syncVisibility(record);
    if (record.loadFailed && record.view && !record.view.webContents.isDestroyed()) {
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
    if (!record || record.loadFailed || !record.view || record.view.webContents.isDestroyed()) return;
    record.ready = true;
    if (record.state && record.visible) {
      this.reconcileGeometry(record);
    }
    this.scheduleViewportApply(record);
    this.syncVisibility(record);
    if (record.visible && record.state) this.publishState(record);
  }

  close(
    windowId: string,
    expected: DesktopOverlayCloseRequest | null = null,
  ): DesktopOverlayClosedEvent | null {
    const record = this.#records.get(windowId);
    if (!record) return null;
    const current = record.state;
    if (!record.visible || !current) return null;
    if (expected && (
      current.overlayId !== expected.overlayId || current.ownerId !== expected.ownerId
    )) return null;
    return this.closeRecord(record, "closed");
  }

  private closeRecord(
    record: OverlayRecord,
    reason: DesktopOverlayClosedEvent["reason"],
  ): DesktopOverlayClosedEvent {
    const current = record.state;
    if (!current) {
      throw new Error("desktop_overlay_not_open");
    }
    const closed = {
      kind: current.kind,
      ...createDesktopOverlayClosedIdentityEvent(current, reason),
    } satisfies DesktopOverlayClosedEvent;
    record.visible = false;
    record.state = null;
    record.layout = null;
    record.geometryAvailable = false;
    this.syncVisibility(record);
    // Overlay 是窗口级合成树的一部分，不能在每次关闭菜单时移除或销毁
    // WebContentsView。Electron/macOS 在移除一个已显示的兄弟 View 后可能
    // 让整个 contentView 重新合成，表现为 App Renderer 与 Browser Surface
    // 同时黑屏；重新创建 Renderer 还会丢失 Accessibility/输入上下文。
    // 关闭事务只把 View 隐藏并收敛为零尺寸，保持稳定的子 View 层级；下一次
    // 打开复用同一个 WebContents，窗口关闭时才由 closeWindow 统一释放。
    this.notifyClosed(record, closed);
    return closed;
  }

  private notifyClosed(record: OverlayRecord, event: DesktopOverlayClosedEvent): void {
    const view = record.view;
    if (view && !view.webContents.isDestroyed()) {
      view.webContents.send("magi-desktop:overlay-closed", event);
    }
    this.#onClosed(record.windowId, event);
  }

  private reconcileGeometry(record: OverlayRecord): void {
    const state = record.state;
    const layout = record.layout;
    if (!state || !layout || !record.visible) {
      record.geometryAvailable = false;
      this.clearOverlayBounds(record);
      return;
    }
    const geometry = resolveCurrentOverlayGeometry(state, layout);
    if (geometry) {
      record.geometryAvailable = true;
      this.mountOnLayer(record);
      this.setOverlayBounds(record, geometry.bounds);
      return;
    }
    // 正常布局变化不会走到这里：WindowLayout 会保留最后一份完整几何帧，
    // 直到 Renderer 提交下一份完整帧。这里仅处理首次加载或真实失效，
    // 隐藏当前 View 但保留 WebContents，避免重新加载造成闪烁。
    record.geometryAvailable = false;
    this.clearOverlayBounds(record);
  }

  closeBrowserOverlay(windowId: string, tabId: string): boolean {
    const record = this.#records.get(windowId);
    if (!record?.visible || record.state?.ownerId !== `browser:${tabId}`) return false;
    return this.close(windowId, {
      overlayId: record.state.overlayId,
      ownerId: record.state.ownerId,
    }) !== null;
  }

  updateLayout(
    windowId: string,
    layout: WindowLayoutSnapshot,
  ): void {
    const record = this.#records.get(windowId);
    if (!record || !record.visible || !record.state) return;
    // rightPaneBounds 是几何轨道，即使右栏当前折叠也会存在；可见性必须
    // 由 rightPaneVisible 判断。切换到其他面板时，浏览器弹出层也必须
    // 立即关闭，避免旧菜单留在主 Renderer 之外继续拦截输入。
    const browserOverlay = record.state.ownerId.startsWith("browser:");
    const geometry = resolveCurrentOverlayGeometry(record.state, layout);
    const shouldClose = (
      !layout.rightPaneVisible
      || (browserOverlay && layout.activePanelKind !== "browser")
      || (browserOverlay && layout.activeTabId !== record.state.ownerId.slice("browser:".length))
    );
    if (shouldClose) {
      this.close(windowId);
      return;
    }
    record.layout = layout;
    if (!geometry) {
      record.geometryAvailable = false;
      this.clearOverlayBounds(record);
      this.syncVisibility(record);
      return;
    }
    record.geometryAvailable = true;
    this.mountOnLayer(record);
    this.setOverlayBounds(record, geometry.bounds);
    this.syncVisibility(record);
    // 几何帧可能晚于 Overlay Renderer 的 ready 握手到达。此时仅切换
    // 可见性会留下一个已加载但没有状态的空 View，后续也不会再触发
    // did-finish-load/ready。几何确认和状态发布必须属于同一个提交点。
    if (record.loaded && record.ready) this.publishState(record);
  }

  handleAction(windowId: string, action: DesktopOverlayAction): void {
    const record = this.#records.get(windowId);
    const state = record?.state;
    if (!record || !record.visible || !state) return;
    const knownAction = state.fields.some((field) => field.id === action.id)
      || state.items.some((item) => item.id === action.id)
      || ["selection", "save", "cancel"].includes(action.id);
    if (
      action.overlayId !== state.overlayId
      || action.kind !== state.kind
      || action.ownerId !== state.ownerId
      || !knownAction
    ) {
      return;
    }
    // Overlay Manager 只负责验证并分发动作。关闭属于业务动作的一部分，
    // 必须由 App Renderer 在完成视口更新、面板切换或标记保存后显式执行。
    // 这样 overlay-closed 不会在业务处理前清空 desktopOverlayId，也不会
    // 在 Browser Surface 尚未完成切换时抢先恢复旧焦点。
    this.#onAction(windowId, action);
  }

  isWebContents(webContentsId: number): boolean {
    return [...this.#records.values()].some((record) => record.view?.webContents.id === webContentsId);
  }

  windowIdForWebContents(webContentsId: number): string | null {
    for (const record of this.#records.values()) {
      if (record.view?.webContents.id === webContentsId) return record.windowId;
    }
    return null;
  }

  closeWindow(windowId: string): void {
    const record = this.#records.get(windowId);
    if (!record) return;
    const view = record.view;
    record.visible = false;
    record.state = null;
    record.geometryAvailable = false;
    record.layout = null;
    if (view) {
      view.setBounds({ x: 0, y: 0, width: 0, height: 0 });
      view.setVisible(false);
    }
    record.view = null;
    this.syncVisibility(record);
    if (!record.window.isDestroyed() && record.mounted) {
      try {
        if (view) record.contentRoot.removeChildView(view);
      } catch {
        // 窗口销毁与关闭动作可能交错到达，移除必须幂等。
      }
      record.mounted = false;
    }
    if (view && !view.webContents.isDestroyed()) view.webContents.close();
    this.#records.delete(windowId);
  }

  private createRenderer(record: OverlayRecord): void {
    if (
      record.window.isDestroyed()
      || record.view
      || this.#records.get(record.windowId) !== record
    ) return;
    const view = this.#createView({
      webPreferences: {
        preload: this.#preloadPath,
        additionalArguments: [
          "--magi-desktop-surface=overlay",
          `--magi-desktop-window-id=${record.windowId}`,
        ],
        partition: "persist:magi-app",
        nodeIntegration: false,
        contextIsolation: true,
        sandbox: true,
        webSecurity: true,
      },
    });
    record.view = view;
    // Overlay 是浏览器内容槽上的透明交互层。Electron 的原生 View 默认
    // 会绘制不透明背景；仅依赖 DOM 的 background: transparent 不足以让
    // 下方 WebContentsView 可见，最终表现为打开标记后整个页面黑屏。
    view.setBackgroundColor(TRANSPARENT_VIEW_BACKGROUND);
    this.clearOverlayBounds(record);
    // 窗口创建阶段就把 Overlay 加入固定的原生子 View 合成树。之后打开、关闭
    // 只改变可见性和 bounds，不能在用户操作期间动态增删 WebContentsView；
    // macOS 的 compositor 会在动态改树时重建整棵 contentView，导致 App 和
    // Browser Surface 黑屏，同时破坏键盘焦点和 Accessibility 树。
    this.mountOnLayer(record);
    this.syncVisibility(record);
    view.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
    view.webContents.on("will-navigate", (event, url) => {
      if (record.view !== view) return;
      if (!this.isTrustedRendererUrl(url, record.windowId)) event.preventDefault();
    });
    view.webContents.on("did-finish-load", () => {
      if (record.view !== view || record.loadFailed) return;
      record.loaded = true;
      if (record.state && record.visible) {
        this.reconcileGeometry(record);
      }
      this.scheduleViewportApply(record);
      this.syncVisibility(record);
      if (record.state && record.visible && record.ready) this.publishState(record);
    });
    view.webContents.on("did-fail-load", (_event, _errorCode, _errorDescription, _validatedURL, isMainFrame) => {
      if (record.view !== view || !isMainFrame) return;
      record.loaded = false;
      record.ready = false;
      record.loadFailed = true;
      this.syncVisibility(record);
    });
    view.webContents.on("render-process-gone", () => {
      if (record.view !== view) return;
      record.loaded = false;
      record.ready = false;
      record.loadFailed = true;
      this.syncVisibility(record);
      if (record.visible && !record.window.isDestroyed()) {
        void this.loadRenderer(record);
      }
    });
    void this.loadRenderer(record);
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
      || !record.geometryAvailable
      || !record.view
      || record.view.webContents.isDestroyed()
    ) return;
    record.view.webContents.send("magi-desktop:overlay-state", record.state);
  }

  private scheduleViewportApply(record: OverlayRecord): void {
    const view = record.view;
    record.viewportApplyDirty = true;
    if (
      record.window.isDestroyed()
      || !view
      || view.webContents.isDestroyed()
      || !record.loaded
      || record.loadFailed
    ) return;
    if (record.viewportApplyPromise) return;
    const apply = this.flushViewportApply(record, view);
    const settled = apply.finally(() => {
      if (record.viewportApplyPromise === settled) record.viewportApplyPromise = null;
      if (
        record.view === view
        && !record.window.isDestroyed()
        && record.loaded
        && !record.loadFailed
        && record.viewportApplyDirty
      ) {
        this.scheduleViewportApply(record);
      }
    });
    record.viewportApplyPromise = settled;
    void settled.catch((error) => {
      if (!record.window.isDestroyed()) {
        console.warn("[DesktopOverlayManager] Overlay viewport 同步失败", {
          windowId: record.windowId,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    });
  }

  private async flushViewportApply(record: OverlayRecord, view: WebContentsView): Promise<void> {
    while (record.viewportApplyDirty) {
      record.viewportApplyDirty = false;
      if (
        record.window.isDestroyed()
        || record.view !== view
        || view.webContents.isDestroyed()
        || !record.loaded
        || record.loadFailed
        || !record.geometryAvailable
      ) return;
      const bounds = view.getBounds();
      if (bounds.width <= 0 || bounds.height <= 0) return;
      if (record.viewportAppliedBounds && sameBounds(record.viewportAppliedBounds, bounds)) return;
      // 单独的 Overlay WebContents 与 App Renderer、网页 WebContents 隔离。
      // 这里同步它自身的真实弹层 viewport，避免菜单按整窗默认 800x600
      // 布局后再被原生 bounds 裁切，从而看起来没有吸附到触发按钮。
      const debuggerApi = view.webContents.debugger;
      if (!debuggerApi) return;
      if (!debuggerApi.isAttached()) debuggerApi.attach("1.3");
      const width = Math.round(bounds.width);
      const height = Math.round(bounds.height);
      await debuggerApi.sendCommand("Emulation.setDeviceMetricsOverride", {
        width,
        height,
        deviceScaleFactor: 1,
        mobile: false,
        scale: 1,
        screenWidth: width,
        screenHeight: height,
        screenOrientation: {
          type: width > height ? "landscapePrimary" : "portraitPrimary",
          angle: width > height ? 90 : 0,
        },
      });
      if (record.view !== view || view.webContents.isDestroyed()) return;
      record.viewportAppliedBounds = { ...bounds };
    }
  }

  private async loadRenderer(record: OverlayRecord): Promise<void> {
    const view = record.view;
    if (record.window.isDestroyed() || !view || view.webContents.isDestroyed()) return;
    if (record.loadPromise) return record.loadPromise;
    record.loaded = false;
    record.ready = false;
    record.loadFailed = false;
    record.geometryAvailable = false;
    record.viewportAppliedBounds = null;
    const load = (async () => {
      try {
        await view.webContents.loadURL(this.rendererUrl(record.windowId));
        if (record.window.isDestroyed() || record.view !== view || view.webContents.isDestroyed()) return;
        record.loaded = true;
        record.loadFailed = false;
        this.syncVisibility(record);
        this.scheduleViewportApply(record);
        if (record.state && record.visible && record.ready) this.publishState(record);
      } catch {
        if (record.window.isDestroyed() || record.view !== view || view.webContents.isDestroyed()) return;
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
    if (record.window.isDestroyed() || !record.view) return;
    const hasRenderableOverlay = (
      !record.view.webContents.isDestroyed()
      && record.visible
      && record.loaded
      && record.ready
      && !record.loadFailed
      && record.geometryAvailable
    );
    // 原生 Overlay 在窗口创建时已经挂载。关闭时保留最后一个有效矩形，
    // 只切换原生 View 的可见性；把 WebContentsView 压到 0x0 会让 Chromium
    // 在下一次恢复到正常尺寸时丢失 backing surface，表现为 App/Browser
    // 同时黑屏。View 不可见时不会参与命中测试，窗口销毁时才清零 bounds。
    if (!hasRenderableOverlay) {
      record.viewportAppliedBounds = null;
    }
    if (record.view.getVisible() !== hasRenderableOverlay) {
      record.view.setVisible(hasRenderableOverlay);
    }
    // 布局事务只负责合成层的可见性和 bounds，不能隐式改变焦点归属。
    // 这里抢焦点会在右栏拖动、菜单重排或页面加载时把键盘输入从对话框
    // 或浏览器地址栏转移到 Overlay WebContents，造成“点击后输入跑错位置”。
  }

  private setOverlayBounds(record: OverlayRecord, bounds: Rectangle): void {
    // Overlay 直接挂在 contentView 的第 2 层，因此必须使用 Renderer 提交的
    // 窗口坐标。不能再把菜单坐标转换成中间 Layer 的局部坐标，否则菜单会从
    // 按钮下方漂移，且其 Chromium viewport 会退回默认窗口尺寸。
    if (!record.view) return;
    if (!sameBounds(record.view.getBounds(), bounds)) record.view.setBounds(bounds);
    this.scheduleViewportApply(record);
  }

  private clearOverlayBounds(record: OverlayRecord): void {
    record.viewportAppliedBounds = null;
  }

  private mountOnLayer(record: OverlayRecord): void {
    if (record.window.isDestroyed() || !record.view) return;
    if (!record.mounted) {
      // Overlay WebContentsView 与 Browser Surface 同为 contentView 的直接
      // 子视图。第 2 层保证菜单和标记覆盖网页，第 0 层仍由 App Renderer
      // 管理右栏 Tab 栏、工具栏与其他功能面板。
      record.contentRoot.addChildView(record.view, 2);
      record.mounted = true;
    }
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

export function resolveCurrentOverlayGeometry(
  state: DesktopOverlayState,
  layout: WindowLayoutSnapshot,
): { bounds: Rectangle } | null {
  const frame = layout.rendererGeometry;
  // Renderer 是右栏结构的唯一事实来源。Overlay 只绑定它声明的容器矩形，
  // Main 不推导菜单宽度、高度或 anchor 偏移。
  const parent = frame?.rightPaneBounds;
  if (!layout.rightPaneVisible || !parent) return null;
  if (state.kind === "menu") {
    const bounds = state.popupBounds;
    if (!bounds) return null;
    if (!isContainedByLayout(layout, bounds)) return null;
    return { bounds: { ...bounds } };
  }
  if (!state.ownerId.startsWith("browser:")) return null;
  const browserTabId = state.ownerId.slice("browser:".length);
  if (
    layout.activePanelKind !== "browser"
    || layout.activeTabId !== browserTabId
    || !layout.activeSurfaceId
  ) return null;

  // 原生 Overlay 只承担需要覆盖真实网页像素的标记流程，几何完全等于
  // Renderer 已提交的当前 Tab 内容槽，不再由 Main 推导任何浮层尺寸。
  const slot = frame?.browserContentSlot;
  if (!slot || slot.tabId !== browserTabId) return null;
  const bounds: Rectangle = { ...slot.bounds };
  if (!isContainedByLayout(layout, bounds)) return null;
  return { bounds };
}

function isContainedByLayout(
  layout: WindowLayoutSnapshot,
  bounds: Rectangle,
): boolean {
  return Boolean(layout.rightPaneBounds)
    && containsRectangle(layout.appBounds, bounds)
    && containsRectangle(layout.rightPaneBounds!, bounds);
}

function containsRectangle(outer: Rectangle, inner: Rectangle): boolean {
  return inner.x >= outer.x
    && inner.y >= outer.y
    && inner.width > 0
    && inner.height > 0
    && inner.x + inner.width <= outer.x + outer.width
    && inner.y + inner.height <= outer.y + outer.height;
}

function sameBounds(left: Rectangle, right: Rectangle): boolean {
  return left.x === right.x
    && left.y === right.y
    && left.width === right.width
    && left.height === right.height;
}
