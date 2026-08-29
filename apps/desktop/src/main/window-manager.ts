import { randomUUID } from "node:crypto";
import {
  BaseWindow,
  screen,
  View,
  WebContentsView,
  type Rectangle,
} from "electron";
import type {
  BrowserLogicalViewport,
  BrowserSurfaceBinding,
  BrowserSurfaceIdentity,
} from "@magi/desktop-browser-contracts";
import type { BrowserSurfaceManager } from "./browser-surface-manager.js";
import {
  DesktopOverlayManager,
  type DesktopOverlayAction,
  type DesktopOverlayClosedEvent,
  type DesktopOverlayCloseRequest,
  type DesktopOverlayState,
} from "./desktop-overlay-manager.js";
import {
  browserContentBounds,
  createWindowLayoutState,
  invalidateRendererGeometry,
  reduceWindowLayout,
  snapshotWindowLayout,
  type PanelKind,
  type WindowLayoutIntent,
  type WindowLayoutSnapshot,
  type WindowLayoutState,
} from "./window-layout.js";

interface DesktopWindowRecord {
  windowId: string;
  window: BaseWindow;
  appView: WebContentsView;
  layout: WindowLayoutState;
  context: DesktopRendererContext;
  browserActivationRevision: number;
  blockingOverlayActive: boolean;
  rendererLoadFailed: boolean;
  rendererRecoveryUrl: string | null;
  rendererLoadPromise: Promise<void> | null;
  closed: boolean;
}

export interface DesktopAppearance {
  backgroundColor: string;
  accentColor: string;
  material: "clear" | "translucent" | "immersive";
  mode: "light" | "dark";
}

export interface DesktopRendererContext {
  contextRevision: number;
  windowId: string;
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
}

export interface DesktopWindowSnapshot {
  desktopEpoch: string;
  windowId: string;
  snapshotRevision: number;
  layout: WindowLayoutSnapshot;
  activeBrowserViewport: BrowserLogicalViewport | null;
}

export class WindowManager {
  readonly #desktopEpoch: string;
  readonly #preloadPath: string;
  readonly #agentOrigin: string;
  readonly #surfaceManager: BrowserSurfaceManager;
  readonly #overlayManager: DesktopOverlayManager;
  readonly #windows: Map<string, BaseWindow>;
  readonly #records = new Map<string, DesktopWindowRecord>();
  readonly #onSnapshot: (snapshot: DesktopWindowSnapshot) => void;
  #appearance: DesktopAppearance = {
    backgroundColor: "#0f1115",
    accentColor: "#2563eb",
    material: "clear",
    mode: "dark",
  };
  #activeWindowId = "";

  constructor(input: {
    desktopEpoch: string;
    preloadPath: string;
    agentOrigin: string;
    surfaceManager: BrowserSurfaceManager;
    overlayManager: DesktopOverlayManager;
    windows: Map<string, BaseWindow>;
    onSnapshot: (snapshot: DesktopWindowSnapshot) => void;
  }) {
    this.#desktopEpoch = input.desktopEpoch;
    this.#preloadPath = input.preloadPath;
    this.#agentOrigin = input.agentOrigin;
    this.#surfaceManager = input.surfaceManager;
    this.#overlayManager = input.overlayManager;
    this.#windows = input.windows;
    this.#onSnapshot = input.onSnapshot;
  }

  createWindow(): string {
    const windowId = `window-${randomUUID()}`;
    const display = screen.getPrimaryDisplay();
    const width = Math.min(1440, Math.max(960, display.workAreaSize.width - 80));
    const height = Math.min(960, Math.max(680, display.workAreaSize.height - 80));
    const window = new BaseWindow({
      width,
      height,
      minWidth: 720,
      minHeight: 520,
      title: "Magi",
      // App Renderer 完成主题握手前窗口保持隐藏；这里的背景只是 native
      // view 的首帧兜底，并始终使用最近一次已同步的主题材质。
      backgroundColor: this.#appearance.backgroundColor,
      show: false,
    });
    this.applyNativeAppearance(window, this.#appearance);
    const contentBounds = window.getContentBounds();
    // BaseWindow 不会替多子 View 管理 contentView 的内部布局。若根 View
    // 保持 Electron 默认的 800x600，子 WebContentsView 即使拿到正确的
    // 窗口 bounds，Chromium compositor 仍会按父 View 的默认 viewport 排版。
    // 先把唯一原生根 View 绑定到窗口内容区，后续所有子 View 才共享同一
    // 个真实坐标系。
    setViewBounds(window.contentView, {
      x: 0,
      y: 0,
      width: contentBounds.width,
      height: contentBounds.height,
    });
    const appView = this.createTrustedView("app", windowId);
    // contentView 是唯一的原生合成根。App Renderer 固定在第 0 层；当前
    // Browser WebContentsView 与当前 Overlay WebContentsView 由各自 Manager
    // 按需直接作为根节点的兄弟视图挂到第 1/2 层。禁止再引入中间 View：中间
    // 容器会让子 Chromium Renderer 保留父容器默认 800x600 viewport，导致
    // 网页与菜单虽然被裁到右栏，却没有按右栏尺寸真实重排。
    window.contentView.addChildView(appView, 0);
    this.#surfaceManager.attachWindow(windowId, window, window.contentView);
    this.#overlayManager.create(windowId, window, window.contentView);
    const layout = createWindowLayoutState({
      desktopEpoch: this.#desktopEpoch,
      windowId,
      clientBounds: { x: 0, y: 0, width: contentBounds.width, height: contentBounds.height },
      displayScaleFactor: display.scaleFactor,
    });
    const record: DesktopWindowRecord = {
      windowId,
      window,
      appView,
      layout,
      context: {
        contextRevision: 0,
        windowId,
        workspaceId: "",
        workspacePath: "",
        sessionId: "",
      },
      browserActivationRevision: 0,
      blockingOverlayActive: false,
      rendererLoadFailed: false,
      rendererRecoveryUrl: null,
      rendererLoadPromise: null,
      closed: false,
    };
    this.#records.set(windowId, record);
    this.#windows.set(windowId, window);
    this.#activeWindowId = windowId;
    this.installWindowEvents(record);
    this.applyLayout(record);
    void this.loadAppRenderer(record, this.rendererUrl(windowId));
    return windowId;
  }

  async restoreAfterDaemonReady(): Promise<void> {
    await this.#overlayManager.restoreAfterDaemonReady();
    // Electron 开发启动可早于 daemon。此时首次 loadURL 仍在 reject 路径中
    // 而 daemon 的 ready 回调已经到达，直接筛选 rendererLoadFailed 会漏掉
    // 这一次恢复机会，留下只有原生窗口外框的空白界面。先等待当前加载
    // 结算，再对确认失败的 Renderer 做一次权威恢复。
    await Promise.all([...this.#records.values()].map((record) => record.rendererLoadPromise));
    const failed = [...this.#records.values()].filter((record) => (
      !record.closed
      && record.rendererLoadFailed
      && !record.appView.webContents.isDestroyed()
    ));
    await Promise.all(failed.map((record) => this.loadAppRenderer(
      record,
      record.rendererRecoveryUrl ?? this.rendererUrl(record.windowId),
    )));
  }

  activeWindowId(): string {
    const active = this.#records.get(this.#activeWindowId);
    if (active && !active.closed) return active.windowId;
    const first = [...this.#records.values()].find((record) => !record.closed);
    if (!first) throw new Error("desktop_window_not_found");
    this.#activeWindowId = first.windowId;
    return first.windowId;
  }

  snapshot(windowId: string): DesktopWindowSnapshot {
    const record = this.requireWindow(windowId);
    const layout = snapshotWindowLayout(record.layout);
    return {
      desktopEpoch: this.#desktopEpoch,
      windowId,
      snapshotRevision: layout.layoutRevision,
      layout,
      activeBrowserViewport: this.#surfaceManager.viewportForSurface(layout.activeSurfaceId),
    };
  }

  submitLayoutIntent(windowId: string, intent: WindowLayoutIntent): DesktopWindowSnapshot {
    const record = this.requireWindow(windowId);
    record.layout = reduceWindowLayout(record.layout, intent);
    return this.applyLayout(record);
  }

  async activateBrowser(input: {
    windowId: string;
    tabId: string;
    browserSessionId: string;
    url: string;
    navigationRevision: number;
    viewport: BrowserLogicalViewport;
  }): Promise<DesktopWindowSnapshot> {
    const record = this.requireWindow(input.windowId);
    const activationRevision = ++record.browserActivationRevision;
    this.#surfaceManager.setActivationGeneration(input.windowId, activationRevision);
    // Surface 物化和调试器握手是异步操作。在目标 Surface 可用之前保持
    // 当前面板和当前页面不变；不能先把布局切到一个没有 surface_id 的
    // Browser Tab，否则 applyLayout 会把旧页面解绑并制造黑屏中间态。
    let binding;
    try {
      binding = await this.#surfaceManager.materialize({
        windowId: input.windowId,
        tabId: input.tabId,
        browserSessionId: input.browserSessionId,
        initialUrl: input.url,
        navigationRevision: input.navigationRevision,
        viewport: input.viewport,
        // 激活右栏只负责挂载真实 Chromium Surface；网络导航在 Surface
        // 内部异步完成，不能阻塞主 Renderer 的布局和工具栏响应。
        awaitPageLoad: false,
        activationGeneration: activationRevision,
      });
    } catch (error) {
      if (isStaleActivationError(error)) {
        if (record.closed) throw new Error("desktop_window_closed");
        return this.snapshot(record.windowId);
      }
      throw error;
    }
    if (record.closed) throw new Error("desktop_window_closed");
    if (activationRevision !== record.browserActivationRevision) {
      return this.snapshot(record.windowId);
    }
    record.layout = reduceWindowLayout(record.layout, {
      type: "right_pane_visibility",
      visible: true,
    });
    record.layout = reduceWindowLayout(record.layout, {
      type: "active_panel",
      kind: "browser",
      tabId: input.tabId,
      surfaceId: binding.surface_id,
    });
    return this.applyLayout(record);
  }

  activatePanel(windowId: string, kind: PanelKind, tabId: string | null): DesktopWindowSnapshot {
    const record = this.requireWindow(windowId);
    record.browserActivationRevision += 1;
    this.#surfaceManager.setActivationGeneration(windowId, record.browserActivationRevision);
    record.layout = reduceWindowLayout(record.layout, {
      type: "active_panel",
      kind,
      tabId,
      surfaceId: null,
    });
    const snapshot = this.applyLayout(record);
    // 面板身份已经在 Main 事务中完成切换。非浏览器面板的键盘和后续
    // DOM 交互必须回到 App Renderer，不能由 Renderer 在 pointerdown/focusin
    // 中再次抢焦点，否则原生 WebContents 切换会打断当前 click 事件。
    if (kind !== "browser" && !record.appView.webContents.isDestroyed()) {
      record.appView.webContents.focus();
    }
    return snapshot;
  }

  handleRightPaneReady(windowId: string): void {
    const record = this.requireWindow(windowId);
    if (!record.appView.webContents.isDestroyed()) {
      record.appView.webContents.send("magi-desktop:context", record.context);
    }
    // Web Renderer 只有在权威主题应用完成后才会发送该握手。延迟到这里
    // 首次显示，保证 native 外壳与 App Renderer 不会出现主题闪烁或错色。
    if (!record.window.isDestroyed() && !record.window.isVisible()) {
      // BaseWindow 创建时虽然已经带有目标窗口尺寸，但隐藏状态下首次
      // 挂载 WebContentsView 不一定会让 Chromium 的 RenderWidget 收到
      // 内容区 resize。通过 Electron 原生内容尺寸 API 在显示前提交同一
      // 个真实 client size，建立一次完整的窗口 -> contentView -> App
      // Renderer 尺寸链，避免首帧落回默认 800x600。
      record.window.setContentSize(
        record.layout.clientBounds.width,
        record.layout.clientBounds.height,
        false,
      );
      record.window.show();
      // BaseWindow 隐藏期间，WebContentsView 可能仍保留创建时的默认
      // 800x600 compositor viewport。首次显示会让 contentView 获得最终
      // DIP 尺寸；这里必须无条件重放一次 bounds，即使 View 自己报告的
      // bounds 已经相同，因为隐藏期间的那次 setBounds 可能没有触发
      // compositor 的 viewport resize。确保 App Renderer 先填满整窗，
      // 再由 DOM 上报真实右栏内容槽。否则 Renderer 会以错误的默认视口
      // 排版，browserContentSlot 会落在主进程右栏之外并永远无法确认。
      setViewBounds(record.window.contentView, record.layout.clientBounds);
      record.appView.setBounds(record.layout.clientBounds);
      this.applyLayout(record);
    }
  }

  setRendererContext(
    windowId: string,
    input: Omit<DesktopRendererContext, "contextRevision" | "windowId">,
  ): DesktopRendererContext {
    const record = this.requireWindow(windowId);
    record.context = {
      contextRevision: record.context.contextRevision + 1,
      windowId,
      ...input,
    };
    if (!record.appView.webContents.isDestroyed()) {
      record.appView.webContents.send("magi-desktop:context", record.context);
    }
    return record.context;
  }

  focusApp(windowId: string): void {
    const record = this.requireWindow(windowId);
    if (record.window.isDestroyed() || record.appView.webContents.isDestroyed()) return;
    // Browser Surface 只覆盖真实浏览器内容槽，工具栏、Tab 栏和其他右栏
    // 面板仍由 appView 命中。切换 App 焦点不需要隐藏或卸载 Browser
    // Surface；隐藏再 setImmediate 恢复会在每次 pointerdown/focusin 时
    // 产生可见闪烁，并使同一点击后续的截图、标记等 IPC 命中失效。
    record.window.focus();
    record.appView.setVisible(true);
    record.appView.webContents.focus();
  }

  async setBrowserViewport(
    windowId: string,
    tabId: string,
    viewport: BrowserLogicalViewport,
  ): Promise<DesktopWindowSnapshot> {
    const record = this.requireWindow(windowId);
    const binding = this.#surfaceManager.bindingForTabInWindow(tabId, windowId);
    if (!binding) {
      throw new Error(`browser_surface_not_found:${tabId}`);
    }
    await this.#surfaceManager.setViewport(binding, viewport);
    return this.snapshot(record.windowId);
  }

  async startBrowserInspect(
    windowId: string,
    request: { tabId: string; surfaceId: string; navigationRevision: number },
  ): Promise<DesktopWindowSnapshot> {
    const { record, binding } = this.requireActiveBrowserBinding(windowId, {
      tab_id: request.tabId,
      surface_id: request.surfaceId,
      navigation_revision: request.navigationRevision,
    });
    await this.#surfaceManager.startInspect(binding);
    return this.snapshot(record.windowId);
  }

  async stopBrowserInspect(
    windowId: string,
    request: { tabId: string; surfaceId: string; navigationRevision: number },
  ): Promise<DesktopWindowSnapshot> {
    const { record, binding } = this.requireActiveBrowserBinding(windowId, {
      tab_id: request.tabId,
      surface_id: request.surfaceId,
      navigation_revision: request.navigationRevision,
    });
    await this.#surfaceManager.stopInspect(binding);
    return this.snapshot(record.windowId);
  }

  private requireActiveBrowserBinding(
    windowId: string,
    request: BrowserSurfaceIdentity,
  ): { record: DesktopWindowRecord; binding: BrowserSurfaceBinding } {
    const record = this.requireWindow(windowId);
    const layout = record.layout;
    if (
      !layout.rightPaneVisible
      || layout.activePanelKind !== "browser"
      || layout.activeTabId !== request.tab_id
      || layout.activeSurfaceId !== request.surface_id
    ) {
      throw new Error("browser_inspect_surface_stale");
    }
    const binding = this.#surfaceManager.bindingForTabInWindow(request.tab_id, windowId);
    if (
      !binding
      || binding.tab_id !== request.tab_id
      || binding.surface_id !== request.surface_id
      || binding.navigation_revision !== request.navigation_revision
    ) {
      throw new Error("browser_inspect_surface_stale");
    }
    return { record, binding };
  }

  openOverlay(windowId: string, state: DesktopOverlayState): void {
    const record = this.requireWindow(windowId);
    const layout = snapshotWindowLayout(record.layout);
    this.#overlayManager.open(
      windowId,
      state,
      layout,
    );
  }

  closeOverlay(
    windowId: string,
    expected: DesktopOverlayCloseRequest | null = null,
  ): DesktopOverlayClosedEvent | null {
    const record = this.requireWindow(windowId);
    const closed = this.#overlayManager.close(windowId, expected);
    // 迟到的卸载/关闭请求只属于旧 owner，不能触发新的 Overlay 重排或
    // 焦点切换。身份不匹配时主进程保持当前浮层不变。
    if (!closed) return null;
    // 关闭原生 Overlay 后重新提交同一布局事务，显式恢复 Browser Surface
    // 的边界和可见性。Overlay 与 Browser Surface 是同一窗口内容树中的
    // 兄弟视图，不能假定移除 Overlay 子视图会让 Chromium 视图自动回到
    // 原来的合成状态；否则菜单第一次打开/关闭后，右栏会留下空白内容槽。
    this.applyLayout(record);
    const activeBrowserTabId = record.layout.activePanelKind === "browser"
      ? record.layout.activeTabId
      : null;
    // 菜单由 App Renderer 的工具栏发起，关闭后仍应把焦点留在 App；只有
    // 标记选择/备注这类覆盖浏览器页面的交互结束后，才恢复当前网页焦点。
    if (closed.kind === "annotation" && activeBrowserTabId
      && this.#surfaceManager.focusTab(windowId, activeBrowserTabId)) return closed;
    if (!record.appView.webContents.isDestroyed()) record.appView.webContents.focus();
    return closed;
  }

  closeBrowserOverlay(windowId: string, tabId: string): void {
    const record = this.requireWindow(windowId);
    if (!this.#overlayManager.closeBrowserOverlay(windowId, tabId)) return;
    this.applyLayout(record);
  }

  setBlockingOverlay(windowId: string, active: boolean): DesktopWindowSnapshot {
    const record = this.requireWindow(windowId);
    record.blockingOverlayActive = active;
    if (active && !record.appView.webContents.isDestroyed()) {
      this.#overlayManager.close(windowId);
      // Settings、全局确认框等属于 App Renderer 的 modal。Browser Surface
      // 位于 App Renderer 之上时必须暂时让出原生合成层和焦点，不能依赖
      // 某个具体 Settings 组件的 z-index。
      record.appView.webContents.focus();
    }
    return this.applyLayout(record);
  }

  handleOverlayAction(windowId: string, action: DesktopOverlayAction): void {
    this.#overlayManager.handleAction(windowId, action);
  }

  handleOverlayReady(windowId: string): void {
    this.#overlayManager.handleReady(windowId);
  }

  setAppearance(windowId: string, appearance: DesktopAppearance): void {
    this.requireWindow(windowId);
    this.#appearance = { ...appearance };
    // 外观是应用级状态，不应只更新触发 IPC 的那个窗口。新建窗口也从
    // #appearance 读取当前材质，避免多窗口之间出现不同的 native 背景。
    for (const record of this.#records.values()) {
      if (record.closed || record.window.isDestroyed()) continue;
      this.applyNativeAppearance(record.window, appearance);
      record.appView.setBackgroundColor(appearance.backgroundColor);
    }
  }

  private applyNativeAppearance(window: BaseWindow, appearance: DesktopAppearance): void {
    window.setBackgroundColor(appearance.backgroundColor);
    if (process.platform === "win32") {
      // Windows 的系统强调色会影响非客户区边框和激活状态；它必须和
      // Magi 主题的 accent 保持一致，不能只改 Renderer 内的按钮颜色。
      try {
        window.setAccentColor(appearance.accentColor);
        window.setBackgroundMaterial(nativeWindowsMaterial(appearance.material));
      } catch (error) {
        // 老版本 Windows 可能不支持系统材质，窗口仍使用不透明主题底色。
        console.warn("[WindowManager] Windows 原生外观不可用", error);
      }
      return;
    }
    if (process.platform === "darwin") {
      try {
        window.setVibrancy(nativeMacVibrancy(appearance.material));
      } catch (error) {
        // macOS 的 vibrancy 失败不影响网页壳层和主题令牌。
        console.warn("[WindowManager] macOS 原生材质不可用", error);
      }
    }
  }

  closeAll(): void {
    for (const record of [...this.#records.values()]) this.closeRecord(record);
  }

  broadcast(windowId: string, channel: string, value: unknown): void {
    const record = this.requireWindow(windowId);
    if (!record.appView.webContents.isDestroyed()) record.appView.webContents.send(channel, value);
  }

  windowIdForWebContents(webContentsId: number): string | null {
    const overlayWindowId = this.#overlayManager.windowIdForWebContents(webContentsId);
    if (overlayWindowId) return overlayWindowId;
    for (const record of this.#records.values()) {
      if (
        record.appView.webContents.id === webContentsId
      ) {
        return record.windowId;
      }
    }
    return null;
  }

  rendererRoleForWebContents(webContentsId: number): "app" | "overlay" | null {
    if (this.#overlayManager.isWebContents(webContentsId)) return "overlay";
    for (const record of this.#records.values()) {
      if (record.appView.webContents.id === webContentsId) return "app";
    }
    return null;
  }

  private createTrustedView(surface: "app", windowId: string): WebContentsView {
    const view = new WebContentsView({
      webPreferences: {
        preload: this.#preloadPath,
        additionalArguments: [
          `--magi-desktop-surface=${surface}`,
          `--magi-desktop-window-id=${windowId}`,
        ],
        partition: "persist:magi-app",
        nodeIntegration: false,
        contextIsolation: true,
        sandbox: true,
        webSecurity: true,
      },
    });
    return view;
  }

  private rendererUrl(windowId: string): string {
    const url = new URL("/web.html", this.#agentOrigin);
    url.searchParams.set("desktopSurface", "app");
    url.searchParams.set("desktopWindowId", windowId);
    // 每次桌面端启动都使用新的 URL，避免持久化 Renderer Session 继续
    // 命中旧的 web.html，导致桌面端实际运行的前端与当前源码不一致。
    url.searchParams.set("desktopEpoch", this.#desktopEpoch);
    return url.href;
  }

  private installWindowEvents(record: DesktopWindowRecord): void {
    const { window } = record;
    window.on("focus", () => {
      if (!record.closed) this.#activeWindowId = record.windowId;
    });
    const updateBounds = () => {
      if (record.closed || window.isDestroyed()) return;
      const bounds = window.getContentBounds();
      setViewBounds(window.contentView, {
        x: 0,
        y: 0,
        width: bounds.width,
        height: bounds.height,
      });
      const display = screen.getDisplayMatching(window.getBounds());
      const previous = record.layout;
      const next = reduceWindowLayout(previous, {
        type: "client_bounds",
        bounds: { x: 0, y: 0, width: bounds.width, height: bounds.height },
        displayScaleFactor: display.scaleFactor,
        fullscreen: window.isFullScreen(),
      });
      record.layout = next;
      this.applyLayout(record);
    };
    window.on("resize", updateBounds);
    window.on("enter-full-screen", updateBounds);
    window.on("leave-full-screen", updateBounds);
    const handleDisplayMetricsChanged = (
      _event: Electron.Event,
      display: Electron.Display,
      changedMetrics: string[],
    ) => {
      if (record.closed || window.isDestroyed()) return;
      const windowDisplay = screen.getDisplayMatching(window.getBounds());
      if (
        display.id === windowDisplay.id
        || changedMetrics.includes("scaleFactor")
        || changedMetrics.includes("bounds")
        || changedMetrics.includes("workArea")
      ) {
        // Native WebContentsView 使用 DIP 坐标，Renderer 使用 CSS 像素。
        // 跨显示器或系统缩放变化时，即使窗口尺寸没有变化，也必须重新
        // 提交同一布局事务，避免原生 Surface 继续使用旧 DPI 的坐标。
        updateBounds();
      }
    };
    screen.on("display-metrics-changed", handleDisplayMetricsChanged);
    window.on("closed", () => this.closeRecord(record));
    window.once("closed", () => screen.off("display-metrics-changed", handleDisplayMetricsChanged));
    for (const view of [record.appView]) {
      const surface = "app";
      view.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
      view.webContents.on("will-navigate", (event, url) => {
        if (!this.isTrustedAppRendererUrl(url, record.windowId)) event.preventDefault();
      });
      view.webContents.on("did-finish-load", () => {
        if (record.closed || view.webContents.isDestroyed()) return;
        if (record.rendererLoadFailed) return;
        console.info("[WindowManager] 可信 Renderer 加载完成", {
          windowId: record.windowId,
          surface,
          url: view.webContents.getURL(),
        });
      });
      view.webContents.on("did-fail-load", (_event, errorCode, errorDescription, validatedURL, isMainFrame) => {
        if (record.closed || !isMainFrame) return;
        record.rendererLoadFailed = true;
        record.rendererRecoveryUrl = this.trustedRendererUrl(record.windowId, validatedURL);
        this.#overlayManager.close(record.windowId);
        this.clearRendererGeometry(record);
        console.error("[WindowManager] 可信 Renderer 加载失败", {
          windowId: record.windowId,
          surface,
          errorCode,
          errorDescription,
          validatedURL,
        });
      });
      view.webContents.on("console-message", (_event, level, message, line, sourceId) => {
        if (record.closed || level < 2) return;
        console.error("[WindowManager] 可信 Renderer 控制台错误", {
          windowId: record.windowId,
          surface,
          level,
          message,
          line,
          sourceId,
        });
      });
      view.webContents.on("render-process-gone", (_event, details) => {
        if (record.closed) return;
        record.rendererLoadFailed = true;
        record.rendererRecoveryUrl = this.trustedRendererUrl(record.windowId, view.webContents.getURL());
        this.#overlayManager.close(record.windowId);
        this.clearRendererGeometry(record);
        console.error("[WindowManager] 可信 Renderer 进程退出", {
          windowId: record.windowId,
          reason: details.reason,
          exitCode: details.exitCode,
        });
        // Renderer 已经丢失旧 DOM，必须先撤下旧 Surface，再恢复 Renderer。
        // 恢复完成后由 Main 的布局事务重新绑定当前 Browser Surface。
        void this.loadAppRenderer(
          record,
          record.rendererRecoveryUrl ?? this.rendererUrl(record.windowId),
        );
      });
    }
  }

  private loadAppRenderer(record: DesktopWindowRecord, url: string): Promise<void> {
    const inFlight = record.rendererLoadPromise;
    if (inFlight) return inFlight;
    const load = this.loadAppRendererInternal(record, url);
    record.rendererLoadPromise = load;
    void load.finally(() => {
      if (record.rendererLoadPromise === load) record.rendererLoadPromise = null;
    });
    return load;
  }

  private async loadAppRendererInternal(record: DesktopWindowRecord, url: string): Promise<void> {
    if (record.closed || record.appView.webContents.isDestroyed()) return;
    record.rendererLoadFailed = false;
    record.rendererRecoveryUrl = null;
    try {
      await record.appView.webContents.loadURL(url);
      if (record.closed || record.appView.webContents.isDestroyed()) return;
      record.rendererLoadFailed = false;
      record.rendererRecoveryUrl = null;
      this.applyLayout(record);
    } catch (error) {
      if (record.closed || record.appView.webContents.isDestroyed()) return;
      record.rendererLoadFailed = true;
      record.rendererRecoveryUrl ??= this.trustedRendererUrl(record.windowId, url);
      console.error("[WindowManager] 可信 Renderer 加载失败", {
        windowId: record.windowId,
        error: error instanceof Error ? error.message : String(error),
      });
      if (!record.window.isDestroyed()) record.window.show();
    }
  }

  private trustedRendererUrl(windowId: string, candidate: string): string {
    try {
      const url = new URL(candidate);
      if (
        url.origin === this.#agentOrigin
        && url.pathname === "/web.html"
        && url.searchParams.get("desktopSurface") === "app"
        && url.searchParams.get("desktopWindowId") === windowId
      ) {
        return url.href;
      }
    } catch {
      // 无效或非 Magi URL 不得成为原生 Renderer 的恢复目标。
    }
    return this.rendererUrl(windowId);
  }

  private isTrustedAppRendererUrl(value: string, windowId: string): boolean {
    try {
      const url = new URL(value);
      return url.origin === this.#agentOrigin
        && url.pathname === "/web.html"
        && url.searchParams.get("desktopSurface") === "app"
        && url.searchParams.get("desktopWindowId") === windowId;
    } catch {
      return false;
    }
  }

  private applyLayout(record: DesktopWindowRecord): DesktopWindowSnapshot {
    const snapshot = this.snapshot(record.windowId);
    const { layout } = snapshot;
    const browserSurfaceActive = !record.blockingOverlayActive
      && layout.rightPaneVisible
      && layout.activePanelKind === "browser"
      && Boolean(layout.activeTabId)
      && Boolean(layout.activeSurfaceId);
    const currentBrowserContentBounds = browserSurfaceActive ? browserContentBounds(layout) : null;
    const currentBrowserParentBounds = browserSurfaceActive
      ? layout.rendererGeometry?.rightPaneBounds ?? null
      : null;
    // Root View 是所有原生子 View 的坐标系，必须在每个布局事务开始时
    // 先收敛到同一内容区，不能只调整子 View。
    setViewBounds(record.window.contentView, layout.clientBounds as Rectangle);
    setViewBounds(record.appView, layout.appBounds as Rectangle);
    record.appView.setVisible(true);
    if (!browserSurfaceActive) {
      this.#surfaceManager.bindContentSurface(record.windowId, "", null);
    } else if (layout.activeTabId) {
      // Renderer 已在 DOM 完成排版后报告真实内容槽。Main 只消费经过
      // revision 校验的槽位并同步原生 Surface，不反向修改 Renderer 布局，
      // 也不在拖动期间导航、刷新或重建页面。bounds 暂缺时 Surface
      // 退出命中树但保留同一 WebContents，下一份有效槽位到达后复用。
      this.#surfaceManager.bindContentSurface(
        record.windowId,
        layout.activeTabId,
        currentBrowserContentBounds,
        currentBrowserParentBounds,
      );
    }
    this.#overlayManager.updateLayout(record.windowId, layout);
    this.#onSnapshot(snapshot);
    return snapshot;
  }

  private clearRendererGeometry(record: DesktopWindowRecord): void {
    record.layout = invalidateRendererGeometry(record.layout);
    this.applyLayout(record);
  }

  private requireWindow(windowId: string): DesktopWindowRecord {
    const record = this.#records.get(windowId);
    if (!record || record.closed || record.window.isDestroyed()) {
      throw new Error(`desktop_window_not_found:${windowId}`);
    }
    return record;
  }

  private closeRecord(record: DesktopWindowRecord): void {
    if (record.closed) return;
    record.closed = true;
    this.#overlayManager.closeWindow(record.windowId);
    this.#surfaceManager.closeWindow(record.windowId);
    if (!record.window.isDestroyed()) {
      record.window.contentView.removeChildView(record.appView);
    }
    if (!record.appView.webContents.isDestroyed()) record.appView.webContents.close();
    this.#records.delete(record.windowId);
    this.#windows.delete(record.windowId);
    if (!record.window.isDestroyed()) record.window.destroy();
  }
}

function setViewBounds(view: View, bounds: Rectangle): void {
  if (!sameBounds(view.getBounds(), bounds)) view.setBounds(bounds);
}

function sameBounds(left: Rectangle, right: Rectangle): boolean {
  return left.x === right.x
    && left.y === right.y
    && left.width === right.width
    && left.height === right.height;
}

function isStaleActivationError(error: unknown): boolean {
  return error instanceof Error
    && error.name === "BrowserSurfaceError"
    && error.message === "browser_surface_activation_stale";
}

function nativeWindowsMaterial(
  material: DesktopAppearance["material"],
): "none" | "acrylic" | "mica" {
  if (material === "immersive") return "mica";
  if (material === "translucent") return "acrylic";
  return "none";
}

function nativeMacVibrancy(
  material: DesktopAppearance["material"],
): "titlebar" | "under-window" | null {
  if (material === "immersive") return "under-window";
  if (material === "translucent") return "titlebar";
  return null;
}
