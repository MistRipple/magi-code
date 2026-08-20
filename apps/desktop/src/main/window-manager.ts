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
} from "@magi/desktop-browser-contracts";
import type { BrowserSurfaceManager } from "./browser-surface-manager.js";
import {
  DesktopOverlayManager,
  type DesktopOverlayAction,
  type DesktopOverlayState,
} from "./desktop-overlay-manager.js";
import {
  createWindowLayoutState,
  reduceWindowLayout,
  browserContentBounds,
  shouldShowBrowserSurface,
  snapshotWindowLayout,
  type PanelKind,
  type WindowLayoutIntent,
  type WindowLayoutSnapshot,
  type WindowLayoutState,
} from "./window-layout.js";

interface DesktopWindowRecord {
  windowId: string;
  window: BaseWindow;
  appLayer: View;
  browserLayer: View;
  overlayLayer: View;
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
    const appLayer = new View();
    const browserLayer = new View();
    const overlayLayer = new View();
    const appView = this.createTrustedView("app", windowId);
    window.contentView.addChildView(appLayer);
    // WebContents 直接挂到窗口内容树，保证 Electron 可以在同一
    // BaseWindow 内可靠切换 App Renderer 与 Browser Surface 的键盘焦点。
    // appLayer 仍保留为主界面背景层，不承载另一个 WebContents。
    window.contentView.addChildView(appView);
    window.contentView.addChildView(browserLayer);
    window.contentView.addChildView(overlayLayer);
    // Browser Surface 只挂在当前右栏内容槽宿主内。宿主由 Main 布局设置
    // 边界，WebContentsView 使用局部坐标，避免原生 View 的命中区域扩散
    // 到对话区，造成键盘焦点落入网页输入框。
    browserLayer.setVisible(false);
    overlayLayer.setVisible(false);
    this.#surfaceManager.attachWindow(windowId, window, browserLayer);
    this.#overlayManager.create(windowId, window, overlayLayer);
    const contentBounds = window.getContentBounds();
    const layout = createWindowLayoutState({
      desktopEpoch: this.#desktopEpoch,
      windowId,
      clientBounds: { x: 0, y: 0, width: contentBounds.width, height: contentBounds.height },
      displayScaleFactor: display.scaleFactor,
    });
    const record: DesktopWindowRecord = {
      windowId,
      window,
      appLayer,
      browserLayer,
      overlayLayer,
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
    const existingBinding = this.#surfaceManager.bindingForTabInWindow(input.tabId, input.windowId);
    // 先原子切换逻辑面板并卸载旧 Surface，避免新 WebContents 异步物化期间
    // 旧页面继续覆盖当前 Tab 的 loading 状态或其他右栏内容。
    record.layout = reduceWindowLayout(record.layout, {
      type: "right_pane_visibility",
      visible: true,
    });
    record.layout = reduceWindowLayout(record.layout, {
      type: "active_panel",
      kind: "browser",
      tabId: input.tabId,
      // 已经物化的 Browser Tab 直接复用现有原生 Surface。这样切换时
      // 只发生显隐和内容槽绑定，不需要等待 Surface/Host 初始化。
      surfaceId: existingBinding?.surface_id ?? null,
    });
    this.#surfaceManager.bindContentSurface(input.windowId, "", null);
    this.applyLayout(record);
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
      type: "active_panel",
      kind: "browser",
      tabId: input.tabId,
      surfaceId: binding?.surface_id ?? null,
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
    if (!record.window.isDestroyed() && !record.window.isVisible()) record.window.show();
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
    // Browser Surface 是 contentView 的同级原生视图。仅调用 WebContents.focus
    // 在 macOS 上不能可靠地把宿主窗口从 Guest WebContents 切回 App Renderer，
    // 因此焦点切换事务先暂时移除浏览器宿主的命中树，再显式恢复可信
    // App View 的输入焦点，最后恢复浏览器的可见性。这样不会改变页面状态，
    // 但能避免 macOS 保留上一个 WebContents 的键盘焦点。
    const browserWasVisible = record.browserLayer.getVisible();
    if (browserWasVisible) record.browserLayer.setVisible(false);
    record.window.focus();
    record.appView.setVisible(true);
    record.appView.webContents.focus();
    if (browserWasVisible) {
      setImmediate(() => {
        if (record.closed || record.window.isDestroyed()) return;
        record.browserLayer.setVisible(
          !record.blockingOverlayActive
            && shouldShowBrowserSurface(
              snapshotWindowLayout(record.layout),
              Boolean(record.layout.activeSurfaceId),
            ),
        );
        // 恢复可见性不能改变最终键盘归属。macOS 在重新加入可见的
        // WebContentsView 时可能把焦点重新交给 Browser Surface，故在
        // 原生层级恢复完成后再次提交 App Renderer 焦点。
        if (!record.appView.webContents.isDestroyed()) {
          record.appView.webContents.focus();
        }
      });
    }
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

  openOverlay(windowId: string, state: DesktopOverlayState): void {
    const record = this.requireWindow(windowId);
    const browserTabId = state.ownerId.startsWith("browser:")
      ? state.ownerId.slice("browser:".length)
      : null;
    const layout = snapshotWindowLayout(record.layout);
    const currentBrowserContentBounds = browserTabId
      && layout.activePanelKind === "browser"
      && layout.activeTabId === browserTabId
      ? browserContentBounds(layout)
      : null;
    this.#overlayManager.open(
      windowId,
      state,
      layout,
      currentBrowserContentBounds,
    );
  }

  closeOverlay(windowId: string): void {
    const record = this.requireWindow(windowId);
    this.#overlayManager.close(windowId);
    const activeBrowserTabId = record.layout.activePanelKind === "browser"
      ? record.layout.activeTabId
      : null;
    if (activeBrowserTabId && this.#surfaceManager.focusTab(windowId, activeBrowserTabId)) return;
    if (!record.appView.webContents.isDestroyed()) record.appView.webContents.focus();
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
        this.#surfaceManager.bindContentSurface(record.windowId, "", null);
        this.#overlayManager.close(record.windowId);
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
        this.#surfaceManager.bindContentSurface(record.windowId, "", null);
        this.#overlayManager.close(record.windowId);
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
    record.appLayer.setBounds(layout.appBounds as Rectangle);
    record.browserLayer.setBounds((browserContentBounds(layout) ?? {
      x: 0,
      y: 0,
      width: 0,
      height: 0,
    }) as Rectangle);
    record.overlayLayer.setBounds(layout.appBounds as Rectangle);
    record.appView.setBounds(layout.appBounds as Rectangle);
    record.appView.setVisible(true);
    const showBrowserSurface = !record.blockingOverlayActive
      && shouldShowBrowserSurface(layout, Boolean(layout.activeSurfaceId));
    const currentBrowserContentBounds = showBrowserSurface ? browserContentBounds(layout) : null;
    record.browserLayer.setVisible(Boolean(currentBrowserContentBounds));
    if (!showBrowserSurface) {
      this.#surfaceManager.bindContentSurface(record.windowId, "", null);
    } else if (layout.activeTabId && currentBrowserContentBounds) {
      // 右栏几何和原生 Surface 在同一个 Main 布局事务中更新。Renderer
      // 不再通过 ResizeObserver 反向提交坐标，因此拖动期间不会出现
      // “整窗父层 -> 内容槽父层”的中间状态，也不会隐藏页面等待下一帧。
      this.#surfaceManager.bindContentSurface(
        record.windowId,
        layout.activeTabId,
        {
          x: 0,
          y: 0,
          width: currentBrowserContentBounds.width,
          height: currentBrowserContentBounds.height,
        },
      );
    }
    this.#overlayManager.updateLayout(record.windowId, layout, currentBrowserContentBounds);
    this.#onSnapshot(snapshot);
    return snapshot;
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
      record.window.contentView.removeChildView(record.appLayer);
      record.window.contentView.removeChildView(record.appView);
      record.window.contentView.removeChildView(record.browserLayer);
      record.window.contentView.removeChildView(record.overlayLayer);
    }
    if (!record.appView.webContents.isDestroyed()) record.appView.webContents.close();
    this.#records.delete(record.windowId);
    this.#windows.delete(record.windowId);
    if (!record.window.isDestroyed()) record.window.destroy();
  }
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
