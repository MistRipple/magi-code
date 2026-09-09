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
import type {
  BrowserSurfaceActivationInput,
  BrowserDownloadRuntimeSnapshot,
  BrowserSurfaceManager,
} from "./browser-surface-manager.js";
import { DesktopWindowReadiness } from "./desktop-window-readiness.js";
import {
  createWindowLayoutState,
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
  rendererLoadFailed: boolean;
  rendererRecoveryUrl: string | null;
  rendererLoadPromise: Promise<void> | null;
  appRendererReady: boolean;
  closed: boolean;
}

interface BrowserSurfaceReadinessWaiter {
  resolve: () => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
}

const BROWSER_SURFACE_READY_TIMEOUT_MS = 15_000;

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
  activeBrowserNavigationRevision: number | null;
  activeBrowserDownloads: BrowserDownloadRuntimeSnapshot[];
}

export class WindowManager {
  readonly #desktopEpoch: string;
  readonly #preloadPath: string;
  readonly #agentOrigin: string;
  readonly #surfaceManager: BrowserSurfaceManager;
  readonly #windows: Map<string, BaseWindow>;
  readonly #records = new Map<string, DesktopWindowRecord>();
  readonly #browserSurfaceReadiness = new Map<
    string,
    Set<BrowserSurfaceReadinessWaiter>
  >();
  readonly #windowReadiness = new DesktopWindowReadiness();
  readonly #onSnapshot: (snapshot: DesktopWindowSnapshot) => void;
  #browserRuntimeReadyRevision = 0;
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
    windows: Map<string, BaseWindow>;
    onSnapshot: (snapshot: DesktopWindowSnapshot) => void;
  }) {
    this.#desktopEpoch = input.desktopEpoch;
    this.#preloadPath = input.preloadPath;
    this.#agentOrigin = input.agentOrigin;
    this.#surfaceManager = input.surfaceManager;
    this.#windows = input.windows;
    this.#onSnapshot = input.onSnapshot;
  }

  createWindow(): string {
    const windowId = `window-${randomUUID()}`;
    const display = screen.getPrimaryDisplay();
    const width = Math.min(
      1440,
      Math.max(960, display.workAreaSize.width - 80),
    );
    const height = Math.min(
      960,
      Math.max(680, display.workAreaSize.height - 80),
    );
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
    // BaseWindow 的 contentView 需要先绑定到真实内容区，确保唯一的 App
    // Renderer 从首帧开始使用窗口的实际尺寸。
    setViewBounds(window.contentView, {
      x: 0,
      y: 0,
      width: contentBounds.width,
      height: contentBounds.height,
    });
    const appView = this.createTrustedView("app", windowId);
    // contentView 是唯一的原生合成根，App Renderer 负责全部右栏 DOM。
    // Browser guest 由 App Renderer 内的 <webview> 承载。
    window.contentView.addChildView(appView, 0);
    this.#surfaceManager.attachWindow(windowId, appView.webContents);
    const layout = createWindowLayoutState({
      desktopEpoch: this.#desktopEpoch,
      windowId,
      clientBounds: {
        x: 0,
        y: 0,
        width: contentBounds.width,
        height: contentBounds.height,
      },
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
      rendererLoadFailed: false,
      rendererRecoveryUrl: null,
      rendererLoadPromise: null,
      appRendererReady: false,
      closed: false,
    };
    this.#records.set(windowId, record);
    this.#windows.set(windowId, window);
    this.#activeWindowId = windowId;
    this.installWindowEvents(record);
    this.applyLayout(record);
    this.#windowReadiness.markAvailable(windowId);
    void this.loadAppRenderer(record, this.rendererUrl(windowId));
    return windowId;
  }

  /** 等待 Desktop 至少创建出一个可承载 Browser Surface 的窗口。 */
  waitForActiveWindow(signal?: AbortSignal): Promise<string> {
    try {
      return Promise.resolve(this.activeWindowId());
    } catch (cause) {
      if (
        !(cause instanceof Error) ||
        cause.message !== "desktop_window_not_found"
      )
        throw cause;
      return this.#windowReadiness.wait(signal);
    }
  }

  /** 启动窗口创建失败时，结算所有尚未开始的 Desktop 命令。 */
  notifyWindowCreationFailed(cause: unknown): void {
    this.#windowReadiness.markFailed(cause);
  }

  async restoreAfterDaemonReady(): Promise<void> {
    // Electron 开发启动可早于 daemon。此时首次 loadURL 仍在 reject 路径中
    // 而 daemon 的 ready 回调已经到达，直接筛选 rendererLoadFailed 会漏掉
    // 这一次恢复机会，留下只有原生窗口外框的空白界面。先等待当前加载
    // 结算，再对确认失败的 Renderer 做一次权威恢复。
    await Promise.all(
      [...this.#records.values()].map((record) => record.rendererLoadPromise),
    );
    const failed = [...this.#records.values()].filter(
      (record) =>
        !record.closed &&
        record.rendererLoadFailed &&
        !record.appView.webContents.isDestroyed(),
    );
    await Promise.all(
      failed.map((record) =>
        this.loadAppRenderer(
          record,
          record.rendererRecoveryUrl ?? this.rendererUrl(record.windowId),
        ),
      ),
    );
    // Worker/daemon 重连只恢复控制协议。App Renderer 自己根据逻辑 Browser
    // Tab 重新绑定 guest；窗口布局不参与浏览器内容的几何计算。
    for (const record of [...this.#records.values()]) {
      if (record.closed || record.window.isDestroyed()) continue;
      this.applyLayout(record);
    }
  }

  /**
   * 发布一次新的 Desktop Browser 运行时代次。
   *
   * Host 注册只说明控制协议已经可用，并不代表逻辑 Browser Tab 对应的
   * Chromium Surface 仍然存在。Renderer 必须在自己的 DOM 已恢复后收到这
   * 个边界，再按当前 Tab 意图重新走一次物化和内容槽几何确认。
   */
  notifyBrowserRuntimeReady(): void {
    this.#browserRuntimeReadyRevision += 1;
    for (const record of this.#records.values()) {
      if (record.closed || record.window.isDestroyed()) continue;
      this.publishBrowserRuntimeReady(record);
    }
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
    this.reconcileActiveBrowserSurface(record);
    const layout = snapshotWindowLayout(record.layout);
    return {
      desktopEpoch: this.#desktopEpoch,
      windowId,
      snapshotRevision: layout.layoutRevision,
      layout,
      activeBrowserViewport: this.#surfaceManager.viewportForSurface(
        layout.activeSurfaceId,
      ),
      activeBrowserNavigationRevision:
        this.#surfaceManager.navigationRevisionForSurface(
          layout.activeSurfaceId,
        ),
      activeBrowserDownloads:
        this.#surfaceManager.browserDownloadSnapshotsForWindow(windowId),
    };
  }

  submitLayoutIntent(
    windowId: string,
    intent: WindowLayoutIntent,
  ): DesktopWindowSnapshot {
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
    this.#surfaceManager.setActivationGeneration(
      input.windowId,
      activationRevision,
    );
    // Surface 物化和调试器握手是异步操作。在目标 Surface 可用之前保持
    // 当前面板和当前页面不变；不能先把布局切到一个没有 surface_id 的
    // Browser Tab，否则 applyLayout 会把旧页面解绑并制造黑屏中间态。
    let binding: BrowserSurfaceBinding | null;
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
    const surfaceId =
      binding?.surface_id ??
      this.#surfaceManager.surfaceIdForTabInWindow(input.tabId, input.windowId);
    if (!surfaceId) throw new Error("browser_surface_not_found");
    record.layout = reduceWindowLayout(record.layout, {
      type: "right_pane_visibility",
      visible: true,
    });
    record.layout = reduceWindowLayout(record.layout, {
      type: "active_panel",
      kind: "browser",
      tabId: input.tabId,
      surfaceId,
    });
    return this.applyLayout(record);
  }

  registerEmbeddedWebview(
    windowId: string,
    input: Parameters<BrowserSurfaceManager["registerEmbeddedWebview"]>[1],
  ): DesktopWindowSnapshot {
    const record = this.requireWindow(windowId);
    this.#surfaceManager.registerEmbeddedWebview(windowId, input);
    this.resolveBrowserSurfaceReadiness(record);
    return this.snapshot(windowId);
  }

  releaseEmbeddedWebview(
    windowId: string,
    input: Parameters<BrowserSurfaceManager["releaseEmbeddedWebview"]>[1],
  ): DesktopWindowSnapshot {
    const record = this.requireWindow(windowId);
    this.#surfaceManager.releaseEmbeddedWebview(windowId, input);
    return this.snapshot(record.windowId);
  }

  resolveBrowserSurfaceReadinessForBinding(
    binding: BrowserSurfaceBinding,
  ): void {
    const record = this.#records.get(binding.window_id);
    if (!record || record.closed) return;
    // WebContents 注册早于 CDP 调试器初始化。Surface 在调试器完成后
    // 通过 primary_changed 发布有效 binding，此时重新判定同一个就绪等待器，
    // 避免首次 Browser Tab 永久等待到超时。
    this.resolveBrowserSurfaceReadiness(record);
  }

  async ensureBrowserSurface(
    input: BrowserSurfaceActivationInput,
  ): Promise<DesktopWindowSnapshot> {
    const readiness = this.createBrowserSurfaceReadinessWaiter(
      input.windowId,
      input.tabId,
    );
    try {
      await this.activateBrowser(input);
      await readiness.promise;
      return this.snapshot(input.windowId);
    } finally {
      readiness.cancel();
    }
  }

  async waitForBrowserSurface(input: {
    windowId: string;
    tabId: string;
  }): Promise<DesktopWindowSnapshot> {
    const readiness = this.createBrowserSurfaceReadinessWaiter(
      input.windowId,
      input.tabId,
    );
    try {
      await readiness.promise;
      return this.snapshot(input.windowId);
    } finally {
      readiness.cancel();
    }
  }

  activatePanel(
    windowId: string,
    kind: PanelKind,
    tabId: string | null,
  ): DesktopWindowSnapshot {
    const record = this.requireWindow(windowId);
    record.browserActivationRevision += 1;
    this.#surfaceManager.setActivationGeneration(
      windowId,
      record.browserActivationRevision,
    );
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
    record.appRendererReady = true;
    if (!record.appView.webContents.isDestroyed()) {
      record.appView.webContents.send("magi-desktop:context", record.context);
      this.publishBrowserRuntimeReady(record);
    }
    // Web Renderer 只有在权威主题应用完成后才会发送该握手。延迟到这里
    // 首次显示，保证 native 外壳与 App Renderer 不会出现主题闪烁或错色。
    if (!record.window.isDestroyed() && !record.window.isVisible()) {
      // BaseWindow 创建时虽然已经带有目标窗口尺寸，但隐藏状态下首次
      // 挂载 App Renderer 不一定会让 Chromium 的 RenderWidget 收到
      // 内容区 resize。通过 Electron 原生内容尺寸 API 在显示前提交同一
      // 个真实 client size，建立一次完整的窗口 -> contentView -> App
      // Renderer 尺寸链，避免首帧落回默认 800x600。
      record.window.setContentSize(
        record.layout.clientBounds.width,
        record.layout.clientBounds.height,
        false,
      );
      record.window.show();
      // 首次显示前无条件重放一次 App Renderer bounds，确保隐藏期间创建的
      // Renderer 不会以默认尺寸完成首帧排版。
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
    if (record.window.isDestroyed() || record.appView.webContents.isDestroyed())
      return;
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

  cancelBrowserDownload(
    windowId: string,
    tabId: string,
    downloadId: string,
  ): boolean {
    return this.#surfaceManager.cancelBrowserDownload(
      windowId,
      tabId,
      downloadId,
    );
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
      !layout.rightPaneVisible ||
      layout.activePanelKind !== "browser" ||
      layout.activeTabId !== request.tab_id ||
      layout.activeSurfaceId !== request.surface_id
    ) {
      throw new Error("browser_inspect_surface_stale");
    }
    const binding = this.#surfaceManager.bindingForTabInWindow(
      request.tab_id,
      windowId,
    );
    if (
      !binding ||
      binding.tab_id !== request.tab_id ||
      binding.surface_id !== request.surface_id ||
      binding.navigation_revision !== request.navigation_revision
    ) {
      throw new Error("browser_inspect_surface_stale");
    }
    return { record, binding };
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

  private applyNativeAppearance(
    window: BaseWindow,
    appearance: DesktopAppearance,
  ): void {
    window.setBackgroundColor(appearance.backgroundColor);
    if (process.platform === "win32") {
      // Windows 的系统强调色会影响非客户区边框和激活状态；它必须和
      // Magi 主题的 accent 保持一致，不能只改 Renderer 内的按钮颜色。
      try {
        window.setAccentColor(appearance.accentColor);
        window.setBackgroundMaterial(
          nativeWindowsMaterial(appearance.material),
        );
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
    this.#windowReadiness.markClosed();
    for (const record of [...this.#records.values()]) this.closeRecord(record);
  }

  broadcast(windowId: string, channel: string, value: unknown): void {
    const record = this.requireWindow(windowId);
    if (!record.appView.webContents.isDestroyed())
      record.appView.webContents.send(channel, value);
  }

  windowIdForWebContents(webContentsId: number): string | null {
    for (const record of this.#records.values()) {
      if (record.appView.webContents.id === webContentsId) {
        return record.windowId;
      }
    }
    return null;
  }

  rendererRoleForWebContents(webContentsId: number): "app" | null {
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
        webviewTag: surface === "app",
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
        display.id === windowDisplay.id ||
        changedMetrics.includes("scaleFactor") ||
        changedMetrics.includes("bounds") ||
        changedMetrics.includes("workArea")
      ) {
        // Native View 使用 DIP 坐标，Renderer 使用 CSS 像素。
        // 跨显示器或系统缩放变化时，即使窗口尺寸没有变化，也必须重新
        // 提交同一布局事务，避免原生 Surface 继续使用旧 DPI 的坐标。
        updateBounds();
      }
    };
    screen.on("display-metrics-changed", handleDisplayMetricsChanged);
    window.on("closed", () => this.closeRecord(record));
    window.once("closed", () =>
      screen.off("display-metrics-changed", handleDisplayMetricsChanged),
    );
    for (const view of [record.appView]) {
      const surface = "app";
      view.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
      view.webContents.on("will-navigate", (event, url) => {
        if (!this.isTrustedAppRendererUrl(url, record.windowId))
          event.preventDefault();
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
      view.webContents.on(
        "did-fail-load",
        (_event, errorCode, errorDescription, validatedURL, isMainFrame) => {
          if (record.closed || !isMainFrame) return;
          record.appRendererReady = false;
          record.rendererLoadFailed = true;
          record.rendererRecoveryUrl = this.trustedRendererUrl(
            record.windowId,
            validatedURL,
          );
          console.error("[WindowManager] 可信 Renderer 加载失败", {
            windowId: record.windowId,
            surface,
            errorCode,
            errorDescription,
            validatedURL,
          });
        },
      );
      view.webContents.on(
        "console-message",
        (_event, level, message, line, sourceId) => {
          if (record.closed || level < 2) return;
          console.error("[WindowManager] 可信 Renderer 控制台错误", {
            windowId: record.windowId,
            surface,
            level,
            message,
            line,
            sourceId,
          });
        },
      );
      view.webContents.on("render-process-gone", (_event, details) => {
        if (record.closed) return;
        record.appRendererReady = false;
        record.rendererLoadFailed = true;
        record.rendererRecoveryUrl = this.trustedRendererUrl(
          record.windowId,
          view.webContents.getURL(),
        );
        console.error("[WindowManager] 可信 Renderer 进程退出", {
          windowId: record.windowId,
          reason: details.reason,
          exitCode: details.exitCode,
        });
        // Renderer 已经丢失旧 DOM，恢复唯一的 App Renderer；浏览器 guest
        // 由右栏在新 DOM 挂载后重新注册并复用自己的逻辑 Tab。
        void this.loadAppRenderer(
          record,
          record.rendererRecoveryUrl ?? this.rendererUrl(record.windowId),
        );
      });
    }
  }

  private loadAppRenderer(
    record: DesktopWindowRecord,
    url: string,
  ): Promise<void> {
    const inFlight = record.rendererLoadPromise;
    if (inFlight) return inFlight;
    const load = this.loadAppRendererInternal(record, url);
    record.rendererLoadPromise = load;
    void load.finally(() => {
      if (record.rendererLoadPromise === load)
        record.rendererLoadPromise = null;
    });
    return load;
  }

  private async loadAppRendererInternal(
    record: DesktopWindowRecord,
    url: string,
  ): Promise<void> {
    if (record.closed || record.appView.webContents.isDestroyed()) return;
    record.appRendererReady = false;
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
      record.rendererRecoveryUrl ??= this.trustedRendererUrl(
        record.windowId,
        url,
      );
      console.error("[WindowManager] 可信 Renderer 加载失败", {
        windowId: record.windowId,
        error: error instanceof Error ? error.message : String(error),
      });
      if (!record.window.isDestroyed()) record.window.show();
    }
  }

  private publishBrowserRuntimeReady(record: DesktopWindowRecord): void {
    if (
      !record.appRendererReady ||
      record.closed ||
      record.appView.webContents.isDestroyed() ||
      this.#browserRuntimeReadyRevision <= 0
    )
      return;
    record.appView.webContents.send("magi-desktop:browser-runtime-ready", {
      revision: this.#browserRuntimeReadyRevision,
    });
  }

  private trustedRendererUrl(windowId: string, candidate: string): string {
    try {
      const url = new URL(candidate);
      if (
        url.origin === this.#agentOrigin &&
        url.pathname === "/web.html" &&
        url.searchParams.get("desktopSurface") === "app" &&
        url.searchParams.get("desktopWindowId") === windowId
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
      return (
        url.origin === this.#agentOrigin &&
        url.pathname === "/web.html" &&
        url.searchParams.get("desktopSurface") === "app" &&
        url.searchParams.get("desktopWindowId") === windowId
      );
    } catch {
      return false;
    }
  }

  private applyLayout(record: DesktopWindowRecord): DesktopWindowSnapshot {
    const snapshot = this.snapshot(record.windowId);
    const { layout } = snapshot;
    // BaseWindow 只承载唯一的 App Renderer。真实 Chromium guest 是右栏
    // DOM 的子节点，窗口布局事务不读取、不计算也不更新浏览器几何。
    setViewBounds(record.window.contentView, layout.clientBounds as Rectangle);
    setViewBounds(record.appView, layout.appBounds as Rectangle);
    record.appView.setVisible(true);
    this.resolveBrowserSurfaceReadiness(record);
    this.#onSnapshot(snapshot);
    return snapshot;
  }

  private reconcileActiveBrowserSurface(record: DesktopWindowRecord): void {
    const { layout } = record;
    if (
      layout.activePanelKind !== "browser" ||
      !layout.activeTabId ||
      !layout.activeSurfaceId
    )
      return;
    const binding = this.#surfaceManager.bindingForSurface(
      layout.activeSurfaceId,
    );
    if (
      binding &&
      binding.window_id === record.windowId &&
      binding.tab_id === layout.activeTabId
    )
      return;
    // activeSurfaceId 是 Browser Tab 的逻辑页面身份。只有完整关闭后
    // 才清理它；guest 尚未注册时仍需保留该身份，等待 webview 注册。
    if (this.#surfaceManager.hasSurface(layout.activeSurfaceId)) return;
    record.layout = reduceWindowLayout(record.layout, {
      type: "active_panel",
      kind: "browser",
      tabId: layout.activeTabId,
      surfaceId: null,
    });
  }

  private createBrowserSurfaceReadinessWaiter(
    windowId: string,
    tabId: string,
  ): {
    promise: Promise<void>;
    cancel: () => void;
  } {
    const record = this.requireWindow(windowId);
    if (this.isBrowserSurfaceReady(record, tabId)) {
      return { promise: Promise.resolve(), cancel: () => undefined };
    }

    const key = browserSurfaceReadinessKey(windowId, tabId);
    let active = true;
    let resolvePromise!: () => void;
    let rejectPromise!: (error: Error) => void;
    const promise = new Promise<void>((resolve, reject) => {
      resolvePromise = resolve;
      rejectPromise = reject;
    });
    // ensureBrowserSurface owns this promise, including activation failure
    // paths. This handler prevents a concurrent window close from creating an
    // unhandled rejection after that path has already returned.
    void promise.catch(() => undefined);
    const finish = (error?: Error): void => {
      if (!active) return;
      active = false;
      clearTimeout(timer);
      const waiters = this.#browserSurfaceReadiness.get(key);
      waiters?.delete(waiter);
      if (waiters && waiters.size === 0)
        this.#browserSurfaceReadiness.delete(key);
      if (error) rejectPromise(error);
      else resolvePromise();
    };
    const timer = setTimeout(() => {
      finish(new Error("browser_surface_content_slot_unavailable"));
    }, BROWSER_SURFACE_READY_TIMEOUT_MS);
    timer.unref();
    const waiter: BrowserSurfaceReadinessWaiter = {
      resolve: () => finish(),
      reject: (error) => finish(error),
      timer,
    };
    const waiters = this.#browserSurfaceReadiness.get(key) ?? new Set();
    waiters.add(waiter);
    this.#browserSurfaceReadiness.set(key, waiters);
    this.resolveBrowserSurfaceReadiness(record);
    return {
      promise,
      cancel: () => finish(),
    };
  }

  private resolveBrowserSurfaceReadiness(record: DesktopWindowRecord): void {
    for (const [key, waiters] of this.#browserSurfaceReadiness) {
      if (!key.startsWith(`${record.windowId}\u0000`)) continue;
      const tabId = key.slice(record.windowId.length + 1);
      if (!this.isBrowserSurfaceReady(record, tabId)) continue;
      for (const waiter of [...waiters]) waiter.resolve();
    }
  }

  private isBrowserSurfaceReady(
    record: DesktopWindowRecord,
    tabId: string,
  ): boolean {
    const layout = record.layout;
    if (
      record.closed ||
      !layout.rightPaneVisible ||
      layout.activePanelKind !== "browser" ||
      layout.activeTabId !== tabId ||
      !layout.activeSurfaceId
    )
      return false;
    const binding = this.#surfaceManager.bindingForTabInWindow(
      tabId,
      record.windowId,
    );
    return Boolean(
      binding &&
      binding.surface_id === layout.activeSurfaceId &&
      this.#surfaceManager.isPrimary(binding) &&
      this.#surfaceManager.isContentSlotBoundBinding(binding),
    );
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
    for (const [key, waiters] of this.#browserSurfaceReadiness) {
      if (!key.startsWith(`${record.windowId}\u0000`)) continue;
      for (const waiter of [...waiters])
        waiter.reject(new Error("desktop_window_closed"));
    }
    this.#surfaceManager.closeWindow(record.windowId);
    if (!record.window.isDestroyed()) {
      record.window.contentView.removeChildView(record.appView);
    }
    if (!record.appView.webContents.isDestroyed())
      record.appView.webContents.close();
    this.#records.delete(record.windowId);
    this.#windows.delete(record.windowId);
    if (this.#records.size === 0) this.#windowReadiness.markClosed();
    if (!record.window.isDestroyed()) record.window.destroy();
  }
}

function setViewBounds(view: View, bounds: Rectangle): void {
  if (!sameBounds(view.getBounds(), bounds)) view.setBounds(bounds);
}

function sameBounds(left: Rectangle, right: Rectangle): boolean {
  return (
    left.x === right.x &&
    left.y === right.y &&
    left.width === right.width &&
    left.height === right.height
  );
}

function browserSurfaceReadinessKey(windowId: string, tabId: string): string {
  return `${windowId}\u0000${tabId}`;
}

function isStaleActivationError(error: unknown): boolean {
  return (
    error instanceof Error &&
    error.name === "BrowserSurfaceError" &&
    error.message === "browser_surface_activation_stale"
  );
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
