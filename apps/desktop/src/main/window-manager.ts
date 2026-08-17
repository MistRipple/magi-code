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
  browserSlotRevision: number;
  browserSlotLayoutRevision: number;
  browserSlot: { tabId: string; bounds: Rectangle } | null;
  blockingOverlayActive: boolean;
  rendererLoadFailed: boolean;
  rendererRecoveryUrl: string | null;
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
    window.contentView.addChildView(appLayer);
    window.contentView.addChildView(browserLayer);
    window.contentView.addChildView(overlayLayer);
    // 空的原生层不能参与窗口命中测试。它们仍固定挂在 contentView 上，
    // 但只有真正拥有可见子视图时才打开；否则会挡住 App Renderer 的 DOM
    // 点击，表现为“按钮能看到但点击没有响应”。
    browserLayer.setVisible(false);
    overlayLayer.setVisible(false);
    const appView = this.createTrustedView("app", windowId);
    appLayer.addChildView(appView);
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
      browserSlotRevision: 0,
      browserSlotLayoutRevision: -1,
      browserSlot: null,
      blockingOverlayActive: false,
      rendererLoadFailed: false,
      rendererRecoveryUrl: null,
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
      surfaceId: null,
    });
    record.browserSlot = null;
    record.browserSlotRevision = 0;
    record.browserSlotLayoutRevision = -1;
    this.#surfaceManager.updateBrowserSlot(input.windowId, "", null);
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
    const browserSlot = readBrowserSlot(record);
    const snapshot = this.applyLayout(record);
    // Renderer 可能在物化等待期间已经上报过内容槽。该槽位属于当前
    // Browser Tab，不能因为 activeSurfaceId 当时还是 null 而丢失；物化
    // 完成后必须显式把已有槽位重新交给 Surface Manager，确保 ready
    // Surface 从 BaseWindow 子视图树卸载后能再次挂载并显示。
    if (browserSlot?.tabId === input.tabId) {
      this.#surfaceManager.updateBrowserSlot(
        input.windowId,
        input.tabId,
        browserSlot.bounds,
      );
    }
    return snapshot;
  }

  updateBrowserSlot(
    windowId: string,
    tabId: string,
    slotRevision: number,
    layoutRevision: number,
    bounds: Rectangle | null,
  ): DesktopWindowSnapshot {
    const record = this.requireWindow(windowId);
    if (
      bounds
      && (!Number.isFinite(bounds.x) || !Number.isFinite(bounds.y)
        || !Number.isFinite(bounds.width) || !Number.isFinite(bounds.height)
        || bounds.width <= 0 || bounds.height <= 0)
    ) {
      throw new Error("browser_slot_bounds_invalid");
    }
    if (!record.layout.rightPaneVisible) {
      // 折叠右栏时，即使 Renderer 的旧 ResizeObserver 晚到，也必须撤下
      // 全部原生 Surface，避免失效 View 拦截主 Renderer 的设置和工具栏点击。
      this.#surfaceManager.updateBrowserSlot(windowId, "", null);
      return this.snapshot(windowId);
    }
    if (record.blockingOverlayActive) {
      // Modal 打开后，Renderer 的 ResizeObserver 仍可能晚到。阻塞期间只
      // 隐藏原生 Surface，不接受任何新的显示槽位，避免 Surface 重新盖住 Modal。
      this.#surfaceManager.updateBrowserSlot(windowId, "", null);
      return this.snapshot(windowId);
    }
    if (
      record.layout.activePanelKind !== "browser"
      || record.layout.activeTabId !== tabId
    ) {
      if (!bounds) this.#surfaceManager.updateBrowserSlot(windowId, tabId, null);
      return this.snapshot(windowId);
    }
    if (layoutRevision !== record.layout.layoutRevision) return this.snapshot(windowId);
    if (slotRevision <= record.browserSlotRevision) return this.snapshot(windowId);
    record.browserSlotRevision = slotRevision;
    if (!bounds) {
      record.browserSlot = null;
      this.#surfaceManager.updateBrowserSlot(windowId, tabId, null);
      this.#overlayManager.close(windowId);
      return this.snapshot(windowId);
    }
    const normalizedBounds = bounds
      ? normalizeBrowserSlotBoundsWithinPane(
          normalizeBrowserSlotBounds(bounds),
          snapshotWindowLayout(record.layout).rightPaneBounds,
        )
      : null;
    if (bounds && !normalizedBounds) throw new Error("browser_slot_outside_right_pane");
    record.browserSlot = normalizedBounds ? { tabId, bounds: normalizedBounds } : null;
    record.browserSlotLayoutRevision = normalizedBounds ? layoutRevision : -1;
    // 激活请求尚未完成时先保留 DOM 槽位，但不允许旧 Surface 提前显示。
    // activateBrowser 成功后会用同一槽位重新绑定当前 Surface。
    this.#surfaceManager.updateBrowserSlot(
      windowId,
      tabId,
      record.layout.activeSurfaceId ? normalizedBounds : null,
    );
    // 内容槽只负责定位原生 Surface。它可能因为内容边框、CSS 内边距或
    // 子布局收敛而小于右栏轨道，不能反向改写 rightPaneWidth，否则会形成
    // ResizeObserver -> Main -> DOM -> ResizeObserver 的几何反馈环。
    const snapshot = this.snapshot(windowId);
    this.#overlayManager.updateLayout(windowId, snapshot.layout, normalizedBounds);
    return snapshot;
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
    record.browserSlot = null;
    record.browserSlotRevision = 0;
    record.browserSlotLayoutRevision = -1;
    return this.applyLayout(record);
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
    this.#overlayManager.open(
      windowId,
      state,
      snapshotWindowLayout(record.layout),
      record.browserSlot?.bounds ?? null,
    );
  }

  closeOverlay(windowId: string): void {
    this.requireWindow(windowId);
    this.#overlayManager.close(windowId);
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
        record.browserSlot = null;
        record.browserSlotRevision = 0;
        record.browserSlotLayoutRevision = -1;
        this.#surfaceManager.updateBrowserSlot(record.windowId, "", null);
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
        record.browserSlot = null;
        record.browserSlotRevision = 0;
        record.browserSlotLayoutRevision = -1;
        this.#surfaceManager.updateBrowserSlot(record.windowId, "", null);
        this.#overlayManager.close(record.windowId);
        console.error("[WindowManager] 可信 Renderer 进程退出", {
          windowId: record.windowId,
          reason: details.reason,
          exitCode: details.exitCode,
        });
        // Renderer 已经丢失旧 DOM，必须先撤下旧槽位，再恢复 Renderer。
        // 恢复完成后由新的 ResizeObserver 重新发布槽位，禁止旧坐标复活。
        void this.loadAppRenderer(
          record,
          record.rendererRecoveryUrl ?? this.rendererUrl(record.windowId),
        );
      });
    }
  }

  private async loadAppRenderer(record: DesktopWindowRecord, url: string): Promise<void> {
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
    record.browserLayer.setBounds(layout.appBounds as Rectangle);
    record.overlayLayer.setBounds(layout.appBounds as Rectangle);
    record.appView.setBounds(layout.appBounds as Rectangle);
    record.appView.setVisible(true);
    const browserSlot = record.browserSlot;
    const browserSlotMatchesActiveTab = browserSlot?.tabId === layout.activeTabId;
    const browserSlotMatchesLayout = browserSlotMatchesActiveTab
      && record.browserSlotLayoutRevision === layout.layoutRevision;
    const showBrowserSurface = !record.blockingOverlayActive
      && shouldShowBrowserSurface(layout, browserSlotMatchesLayout);
    if (browserSlot && browserSlotMatchesActiveTab && !browserSlotMatchesLayout) {
      record.browserSlot = null;
      record.browserSlotLayoutRevision = -1;
    }
    if (!showBrowserSurface) {
      this.#surfaceManager.updateBrowserSlot(record.windowId, "", null);
      // 右栏折叠时不能保留旧内容槽。否则 App Renderer 已经移除右栏 DOM 后，
      // 上一次的 WebContentsView 仍会作为 BaseWindow 的兄弟视图留在窗口上，
      // 形成“右栏已经关闭但浏览器还覆盖着窗口”的悬浮层。
      if (!layout.rightPaneVisible) record.browserSlot = null;
    } else if (browserSlot && browserSlotMatchesLayout) {
      // 恢复全局 modal 后只使用 Renderer 最近一次上报的槽位，不能重新
      // 计算或持久化浏览器视口几何。
      this.#surfaceManager.updateBrowserSlot(
        record.windowId,
        browserSlot.tabId,
        browserSlot.bounds,
      );
    }
    // BrowserLayer 的可见性只由 BrowserSurfaceManager 根据真实 Surface 状态
    // 收敛。WindowManager 只负责提交当前内容槽，避免布局状态提前显示一个
    // 空的父层并拦截 App Renderer 的输入。
    this.#overlayManager.updateLayout(record.windowId, layout, browserSlot?.bounds ?? null);
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
      record.window.contentView.removeChildView(record.browserLayer);
      record.window.contentView.removeChildView(record.overlayLayer);
    }
    if (!record.appView.webContents.isDestroyed()) record.appView.webContents.close();
    this.#records.delete(record.windowId);
    this.#windows.delete(record.windowId);
    if (!record.window.isDestroyed()) record.window.destroy();
  }
}

function normalizeBrowserSlotBounds(bounds: Rectangle): Rectangle {
  return {
    x: Math.round(bounds.x),
    y: Math.round(bounds.y),
    width: Math.max(1, Math.round(bounds.width)),
    height: Math.max(1, Math.round(bounds.height)),
  };
}

function normalizeBrowserSlotBoundsWithinPane(
  bounds: Rectangle,
  rightPaneBounds: Rectangle | null,
): Rectangle | null {
  if (!rightPaneBounds) return null;
  const epsilon = 2;
  const right = bounds.x + bounds.width;
  const bottom = bounds.y + bounds.height;
  const paneRight = rightPaneBounds.x + rightPaneBounds.width;
  const paneBottom = rightPaneBounds.y + rightPaneBounds.height;
  if (
    bounds.x < rightPaneBounds.x - epsilon
    || bounds.y < rightPaneBounds.y - epsilon
    || right > paneRight + epsilon
    || bottom > paneBottom + epsilon
  ) return null;
  // 仅消除 DIP/CSS 四舍五入产生的 1~2px 误差；真实越界必须拒绝，不能
  // 静默裁剪成一个看似有效但内容不完整的浏览器 Surface。
  const x = Math.max(bounds.x, rightPaneBounds.x);
  const y = Math.max(bounds.y, rightPaneBounds.y);
  const clippedRight = Math.min(right, paneRight);
  const clippedBottom = Math.min(bottom, paneBottom);
  if (clippedRight <= x || clippedBottom <= y) return null;
  return { x, y, width: clippedRight - x, height: clippedBottom - y };
}

function isStaleActivationError(error: unknown): boolean {
  return error instanceof Error
    && error.name === "BrowserSurfaceError"
    && error.message === "browser_surface_activation_stale";
}

function readBrowserSlot(record: DesktopWindowRecord): DesktopWindowRecord["browserSlot"] {
  return record.browserSlot;
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
