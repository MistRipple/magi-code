import { randomUUID } from "node:crypto";
import { dirname } from "node:path";
import { existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import {
  BaseWindow,
  WebContentsView,
  session,
  type HandlerDetails,
  type Rectangle,
  type View,
  type WebContents,
} from "electron";
import type {
  BrowserControlUpdate,
  BrowserLogicalViewport,
  BrowserPageState,
  BrowserNavigation,
  BrowserSurfaceBinding,
} from "@magi/desktop-browser-contracts";
import { BrowserSurfaceRegistry } from "./browser-surface-registry.js";

export type BrowserSurfaceEvent =
  | { type: "primary_changed"; binding: BrowserSurfaceBinding }
  | { type: "page_updated"; binding: BrowserSurfaceBinding; page: BrowserPageState }
  | { type: "page_failed"; binding: BrowserSurfaceBinding; reason: string }
  | { type: "page_crashed"; binding: BrowserSurfaceBinding; reason: string }
  | { type: "loading_changed"; binding: BrowserSurfaceBinding; loading: boolean }
  | { type: "popup_blocked"; binding: BrowserSurfaceBinding; url: string }
  | { type: "user_takeover"; binding: BrowserSurfaceBinding }
  | {
      type: "agent_cursor";
      binding: BrowserSurfaceBinding;
      visible: boolean;
      x: number | null;
      y: number | null;
      action: string | null;
    }
  | {
      type: "cdp_event";
      binding: BrowserSurfaceBinding;
      method: string;
      params: Record<string, unknown>;
    };

export interface MaterializeSurfaceInput {
  windowId: string;
  tabId: string;
  browserSessionId: string;
  initialUrl: string;
  navigationRevision: number;
  viewport: BrowserLogicalViewport;
  /** Window activation must not wait for a slow network document. */
  awaitPageLoad?: boolean;
  /**
   * 由 WindowManager 分配的 Browser Tab 激活代次。没有代次的调用只适用于
   * 后台恢复/自动化物化，不得参与右栏 Surface 挂载竞争。
   */
  activationGeneration?: number;
}

interface BrowserSurfaceRecord {
  windowId: string;
  surfaceId: string;
  surfaceRevision: number;
  tabId: string;
  browserSessionId: string;
  partitionId: string;
  view: WebContentsView;
  contents: WebContents;
  layer: View;
  /**
   * 每个 Browser Tab 保留独立 WebContents，后台 Surface 仍挂在同一窗口的
   * 内容层但保持零尺寸和不可见，不销毁页面状态，也不允许覆盖 App Renderer。
   */
  mounted: boolean;
  slotVisible: boolean;
  slotBounds: Rectangle | null;
  activationGeneration: number | null;
  priming: boolean;
  loadFailed: boolean;
  targetId: string;
  navigationRevision: number;
  navigationOperationId: number;
  navigationTargetUrl: string | null;
  viewport: BrowserLogicalViewport;
  primary: boolean;
  closed: boolean;
  automationInputDepth: number;
  agentControlled: boolean;
  cursor: { visible: boolean; x: number | null; y: number | null; action: string | null };
  cursorExecutionContextId: number | null;
  cursorUpdatePromise: Promise<void> | null;
  viewportApplied: boolean;
  viewportApplyPromise: Promise<void> | null;
  viewportApplyDirty: boolean;
  debuggerListenersInstalled: boolean;
  recoveryPromise: Promise<void> | null;
  loadPromise: Promise<void> | null;
}

const ALLOWED_NAVIGATION_PROTOCOLS = new Set(["http:", "https:", "about:"]);
const BLOCKED_HOSTS = new Set(["169.254.169.254", "metadata.google.internal"]);
// 固定资产只用于隔离世界中的可视化指针，不读取或修改页面的光标样式。
// 使用内联矢量资源避免依赖页面外部网络和站点 CSS，且在 Shadow DOM 内保持封装。
const AGENT_CURSOR_ASSET = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='20' height='20' viewBox='0 0 20 20'%3E%3Cpath d='M3 1.8 17.2 11l-6.1 1.1 3.7 5.1-2.2 1.6-3.7-5.1-3.4 5.2Z' fill='%232563eb' stroke='white' stroke-width='1.2' stroke-linejoin='round'/%3E%3C/svg%3E";
const ALLOWED_WORKER_CDP_METHODS = new Set([
  "DOM.getDocument",
  "DOM.querySelector",
  "Accessibility.getFullAXTree",
  "Emulation.clearGeolocationOverride",
  "Emulation.clearDeviceMetricsOverride",
  "Emulation.setCPUThrottlingRate",
  "Emulation.setDeviceMetricsOverride",
  "Emulation.setEmulatedMedia",
  "Emulation.setGeolocationOverride",
  "Emulation.setTouchEmulationEnabled",
  "Emulation.setUserAgentOverride",
  "HeapProfiler.addHeapSnapshotChunk",
  "HeapProfiler.disable",
  "HeapProfiler.enable",
  "HeapProfiler.takeHeapSnapshot",
  "Input.dispatchKeyEvent",
  "Input.dispatchMouseEvent",
  "Input.insertText",
  "Network.emulateNetworkConditions",
  "Network.getResponseBody",
  "Network.setExtraHTTPHeaders",
  "Page.captureScreenshot",
  "Page.addScriptToEvaluateOnNewDocument",
  "Page.createIsolatedWorld",
  "Page.enable",
  "Page.getFrameTree",
  "Page.getLayoutMetrics",
  "Page.handleJavaScriptDialog",
  "Page.removeScriptToEvaluateOnNewDocument",
  "Performance.enable",
  "Performance.getMetrics",
  "Overlay.hideHighlight",
  "Overlay.highlightNode",
  "Runtime.evaluate",
  "Runtime.enable",
  "Runtime.getHeapUsage",
  "Network.enable",
  "Tracing.end",
  "Tracing.start",
]);

const DEFAULT_CDP_COMMAND_TIMEOUT_MS = 15_000;
const SCREENSHOT_CDP_COMMAND_TIMEOUT_MS = 10_000;
const CURSOR_CDP_COMMAND_TIMEOUT_MS = 5_000;
const DEFAULT_NAVIGATION_TIMEOUT_MS = 120_000;
export class BrowserSurfaceManager {
  readonly #desktopEpoch: string;
  readonly #surfaces = new BrowserSurfaceRegistry<BrowserSurfaceRecord>();
  readonly #configuredPartitions = new Set<string>();
  readonly #knownPartitions = new Set<string>();
  readonly #partitionRegistryPath: string | null;
  readonly #windows = new Map<string, BaseWindow>();
  readonly #layers = new Map<string, View>();
  readonly #activationGenerations = new Map<string, number>();
  readonly #onEvent: (event: BrowserSurfaceEvent) => void;

  constructor(input: {
    desktopEpoch: string;
    onEvent: (event: BrowserSurfaceEvent) => void;
    partitionRegistryPath?: string;
  }) {
    this.#desktopEpoch = input.desktopEpoch;
    this.#onEvent = input.onEvent;
    this.#partitionRegistryPath = input.partitionRegistryPath?.trim() || null;
    for (const partitionId of readPartitionRegistry(this.#partitionRegistryPath)) {
      this.#knownPartitions.add(partitionId);
    }
  }

  attachWindow(windowId: string, window: BaseWindow, layer: View): void {
    this.#windows.set(windowId, window);
    this.#layers.set(windowId, layer);
    this.#activationGenerations.set(windowId, 0);
    layer.setVisible(false);
  }

  setActivationGeneration(windowId: string, generation: number): void {
    if (!Number.isSafeInteger(generation) || generation < 0) {
      throw new Error("browser_activation_generation_invalid");
    }
    const current = this.#activationGenerations.get(windowId) ?? 0;
    if (generation < current) return;
    this.#activationGenerations.set(windowId, generation);
  }

  async materialize(input: MaterializeSurfaceInput): Promise<BrowserSurfaceBinding> {
    this.assertActivationCurrent(input.windowId, input.activationGeneration);
    let record = this.surfaceForTab(input.tabId, input.windowId);
    const created = !record;
    if (!record) record = await this.createSurface(input);
    if (input.activationGeneration !== undefined) {
      record.activationGeneration = input.activationGeneration;
    }
    if (record.recoveryPromise) await record.recoveryPromise;
    try {
      this.assertActivationCurrent(input.windowId, input.activationGeneration);
    } catch (error) {
      // 只有仍由本次过期请求创建、且没有被更新请求认领的 Surface 才能
      // 回收。相同 Tab 的新请求可能已经复用了同一个 WebContents。
      if (
        created
        && record.activationGeneration === input.activationGeneration
        && this.#surfaces.get(record.surfaceId) === record
      ) {
        this.closeRecord(record, false);
      }
      throw error;
    }
    record.navigationRevision = Math.max(record.navigationRevision, input.navigationRevision);
    if (!this.#surfaces.primaryForTab(record.tabId)) this.promote(record.surfaceId);
    const initialUrl = normalizeNavigableUrl(input.initialUrl);
    this.assertActivationCurrent(input.windowId, input.activationGeneration);
    if (
      (record.contents.getURL() || "") === ""
      || (
        initialUrl !== "about:blank"
        && (record.contents.getURL() || "about:blank") === "about:blank"
      )
    ) {
      const load = this.startLoad(record, initialUrl);
      if (input.awaitPageLoad !== false) await load;
      else void load.catch(() => undefined);
    } else if (!record.contents.isLoadingMainFrame()) {
      this.scheduleViewportApply(record);
    }
    return this.binding(record);
  }
  private async createSurface(input: MaterializeSurfaceInput): Promise<BrowserSurfaceRecord> {
    const window = this.#windows.get(input.windowId);
    const layer = this.#layers.get(input.windowId);
    if (!window || window.isDestroyed()) throw new Error("desktop_window_not_found");
    if (!layer) throw new Error("desktop_browser_layer_not_found");
    const partitionId = browserPartitionId(input.browserSessionId);
    this.configurePartition(partitionId);
    const view = new WebContentsView({
      webPreferences: {
        partition: partitionId,
        nodeIntegration: false,
        contextIsolation: true,
        sandbox: true,
        webSecurity: true,
      },
    });
    view.setVisible(false);
    const contents = view.webContents;
    const record: BrowserSurfaceRecord = {
      windowId: input.windowId,
      surfaceId: `surface-${randomUUID()}`,
      surfaceRevision: this.#surfaces.nextRevision(input.tabId),
      tabId: input.tabId,
      browserSessionId: input.browserSessionId,
      partitionId,
      view,
      contents,
      layer,
      mounted: false,
      slotVisible: false,
      slotBounds: null,
      activationGeneration: input.activationGeneration ?? null,
      priming: true,
      loadFailed: false,
      targetId: "",
      navigationRevision: input.navigationRevision,
      navigationOperationId: 0,
      navigationTargetUrl: null,
      viewport: input.viewport,
      primary: false,
      closed: false,
      automationInputDepth: 0,
      agentControlled: false,
      cursor: { visible: false, x: null, y: null, action: null },
      cursorExecutionContextId: null,
      cursorUpdatePromise: null,
      viewportApplied: false,
      viewportApplyPromise: null,
      viewportApplyDirty: false,
      debuggerListenersInstalled: false,
      recoveryPromise: null,
      loadPromise: null,
    };
    this.#surfaces.add(record);
    // 每个真实 Browser Surface 都必须挂在所属 BaseWindow 的内容层，但没有
    // 当前 DOM 内容槽时只能以零尺寸、不可见状态存在。这样 Chromium 页面
    // 生命周期和 CDP 文档树不会依赖右栏 ResizeObserver 的时序，同时绝不
    // 创建覆盖窗口的隐藏预渲染层。
    this.applySlot(record, null, window);
    contents.once("destroyed", () => {
      if (record.closed) return;
      record.closed = true;
      this.detachSurface(record, window);
      this.removeRecordIndexes(record);
      this.promoteFallback(record.tabId);
    });
    try {
      await this.attachDebugger(record);
      this.installSurfacePolicy(record);
      return record;
    } catch (error) {
      this.closeRecord(record);
      throw error;
    }
  }

  updateBrowserSlot(windowId: string, tabId: string, bounds: Rectangle | null): void {
    const window = this.#windows.get(windowId);
    for (const record of this.#surfaces.values()) {
      if (record.windowId !== windowId || record.closed) continue;
      if (!tabId) {
        this.applySlot(record, null, window);
        continue;
      }
      if (record.tabId === tabId) {
        this.applySlot(record, bounds, window);
      } else if (bounds) {
        // 一个窗口只能有一个当前 Browser Surface；切换到新的内容槽时，
        // 其他 WebContents 保留状态并以零尺寸留在 BaseWindow 子视图树中。
        record.slotBounds = null;
        record.slotVisible = false;
        this.unmountSurface(record, window);
      }
    }
    this.syncLayerVisibility(windowId);
  }

  bindingForTabInWindow(tabId: string, windowId: string): BrowserSurfaceBinding | null {
    const record = this.#surfaces.forWindowTab(windowId, tabId);
    return record && !record.closed ? this.binding(record) : null;
  }

  primaryBindingForTab(tabId: string): BrowserSurfaceBinding | null {
    const record = this.#surfaces.primaryForTab(tabId);
    return record && !record.closed ? this.binding(record) : null;
  }

  bindingForSurface(surfaceId: string): BrowserSurfaceBinding | null {
    const record = this.#surfaces.get(surfaceId);
    return record && !record.closed ? this.binding(record) : null;
  }

  bindings(): BrowserSurfaceBinding[] {
    return [...this.#surfaces.values()]
      .filter((record) => !record.closed)
      .map((record) => this.binding(record));
  }

  isPrimary(binding: BrowserSurfaceBinding): boolean {
    const record = this.#surfaces.get(binding.surface_id);
    return Boolean(
      record
      && !record.closed
      && record.primary
      && this.#surfaces.isPrimary(record)
      && binding.surface_revision === record.surfaceRevision,
    );
  }

  viewportStateForSurface(surfaceId: string | null): { viewport: BrowserLogicalViewport } | null {
    if (!surfaceId) return null;
    const record = this.#surfaces.get(surfaceId);
    return record && !record.closed
      ? { viewport: structuredClone(record.viewport) }
      : null;
  }

  viewportForSurface(surfaceId: string | null): BrowserLogicalViewport | null {
    return this.viewportStateForSurface(surfaceId)?.viewport ?? null;
  }

  recordForBinding(binding: BrowserSurfaceBinding): WebContents {
    const record = this.#surfaces.get(binding.surface_id);
    if (!record || record.closed) {
      throw staleSurfaceError("browser_surface_not_found");
    }
    if (
      binding.desktop_epoch !== this.#desktopEpoch
      || binding.window_id !== record.windowId
      || binding.surface_revision !== record.surfaceRevision
      || binding.tab_id !== record.tabId
      || binding.web_contents_id !== record.contents.id
      || binding.target_id !== record.targetId
      || binding.browser_context_id !== record.partitionId
      || binding.navigation_revision !== record.navigationRevision
    ) {
      throw staleSurfaceError("browser_surface_stale");
    }
    return record.contents;
  }

  async sendCdp(
    binding: BrowserSurfaceBinding,
    method: string,
    params: Record<string, unknown> = {},
  ): Promise<unknown> {
    const contents = this.recordForBinding(binding);
    if (!ALLOWED_WORKER_CDP_METHODS.has(method)) {
      throw new Error(`browser_cdp_method_denied:${method}`);
    }
    const record = this.requireRecord(binding.surface_id);
    if (method === "Page.captureScreenshot" && (!record.slotVisible || !record.slotBounds || !record.mounted)) {
      throw browserSurfaceError("browser_surface_no_content_slot");
    }
    if (!contents.debugger.isAttached()) {
      throw staleSurfaceError("browser_debugger_detached");
    }
    const injectsInput = method.startsWith("Input.");
    if (injectsInput) record.automationInputDepth += 1;
    try {
      const result = await sendCdpCommandWithTimeout(
        contents,
        method,
        params,
        method === "Page.captureScreenshot"
          ? SCREENSHOT_CDP_COMMAND_TIMEOUT_MS
          : DEFAULT_CDP_COMMAND_TIMEOUT_MS,
      );
      if (injectsInput && record.agentControlled) {
        const input = params as { type?: string; x?: number; y?: number };
        const action = method === "Input.insertText"
          ? "type"
          : input.type === "mousePressed" || input.type === "mouseReleased"
            ? "click"
            : input.type === "mouseWheel" ? "scroll" : "move";
        void this.setAgentCursor(
          record,
          true,
          typeof input.x === "number" ? input.x : record.cursor.x,
          typeof input.y === "number" ? input.y : record.cursor.y,
          action,
        );
      }
      return result;
    } finally {
      if (injectsInput) record.automationInputDepth = Math.max(0, record.automationInputDepth - 1);
    }
  }

  async navigate(
    binding: BrowserSurfaceBinding,
    navigation: BrowserNavigation,
  ): Promise<BrowserPageState> {
    const contents = this.recordForBinding(binding);
    switch (navigation.action) {
      case "url":
        await this.loadPage(
          this.requireRecord(binding.surface_id),
          normalizeNavigableUrl(navigation.url),
          navigation.timeout_ms,
        );
        break;
      case "back":
        if (contents.navigationHistory.canGoBack()) {
        await this.waitForNavigation(
            contents,
            () => contents.navigationHistory.goBack(),
            navigation.timeout_ms,
          );
        }
        break;
      case "forward":
        if (contents.navigationHistory.canGoForward()) {
        await this.waitForNavigation(
            contents,
            () => contents.navigationHistory.goForward(),
            navigation.timeout_ms,
          );
        }
        break;
      case "reload":
        await reloadAndWait(
          contents,
          navigation.ignore_cache === true,
          navigation.timeout_ms,
        );
        break;
    }
    const record = this.requireRecord(binding.surface_id);
    return this.pageState(record);
  }

  async setViewport(
    binding: BrowserSurfaceBinding,
    viewport: BrowserLogicalViewport,
  ): Promise<void> {
    const record = this.requireRecord(binding.surface_id);
    this.recordForBinding(binding);
    record.viewport = viewport;
    record.viewportApplied = false;
    if (record.contents.isLoadingMainFrame()) return;
    this.scheduleViewportApply(record);
    await record.viewportApplyPromise;
  }

  async updateControl(tabId: string, surfaceId: string, control: BrowserControlUpdate): Promise<void> {
    const binding = this.bindingForSurface(surfaceId);
    if (!binding || binding.tab_id !== tabId || !this.isPrimary(binding)) {
      throw staleSurfaceError("browser_surface_stale");
    }
    const record = this.requireRecord(surfaceId);
    record.agentControlled = control.mode === "agent";
    // 控制权变更只更新宿主状态；光标绘制依赖页面文档树，不能占用该 Tab
    // 的命令队列。新建 about:blank 尚未完成首帧时 Page.getFrameTree 会等待
    // 到文档建立，若在这里等待会把后续 navigate/viewport 一并锁死。
    void this.setAgentCursor(
      record,
      record.agentControlled,
      record.cursor.x,
      record.cursor.y,
      record.cursor.action ?? "move",
    ).catch((error) => {
      if (!record.closed) {
        console.warn("[BrowserSurfaceManager] Agent 光标更新失败", {
          surfaceId: record.surfaceId,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    });
  }

  async closeTab(tabId: string): Promise<void> {
    const records = [...this.#surfaces.values()].filter((record) => record.tabId === tabId);
    for (const record of records) this.closeRecord(record, false);
  }

  closeWindow(windowId: string): void {
    for (const record of [...this.#surfaces.values()]) {
      if (record.windowId === windowId) this.closeRecord(record);
    }
    this.#windows.delete(windowId);
    this.#layers.delete(windowId);
    this.#activationGenerations.delete(windowId);
  }

  closeAll(): void {
    for (const record of [...this.#surfaces.values()]) this.closeRecord(record, false);
    this.#surfaces.clear();
    this.#windows.clear();
    this.#layers.clear();
    this.#activationGenerations.clear();
  }

  async clearBrowsingData(): Promise<void> {
    // 不能只依赖当前进程已经创建过 WebContentsView 的 partition。浏览器
    // 会话由 daemon 持久化，应用重启后未激活的 Tab 仍然拥有同一磁盘上下文，
    // 清理数据必须覆盖这些会话，否则“清理成功”会变成空操作。
    const partitions = new Set([...this.#knownPartitions, ...this.#configuredPartitions]);
    await Promise.all([...partitions].map(async (partitionId) => {
      const browserSession = session.fromPartition(partitionId, { cache: false });
      await Promise.all([
        browserSession.clearCache(),
        browserSession.clearStorageData(),
      ]);
    }));
    for (const record of this.#surfaces.values()) {
      if (!record.closed) record.contents.reloadIgnoringCache();
    }
  }

  private surfaceForTab(tabId: string, windowId: string): BrowserSurfaceRecord | null {
    const record = this.#surfaces.forWindowTab(windowId, tabId);
    return record && !record.closed ? record : null;
  }

  private applySlot(
    record: BrowserSurfaceRecord,
    bounds: Rectangle | null,
    window: BaseWindow | undefined,
  ): void {
    record.slotBounds = bounds ? { ...bounds } : null;
    record.slotVisible = bounds !== null;
    if (!window || window.isDestroyed()) {
      this.detachSurface(record, window);
      return;
    }
    const wasMounted = record.mounted;
    if (!record.mounted) {
      // Browser Surface 只能挂在固定 BrowserLayer。层级由 WindowManager
      // 一次性建立，Surface 生命周期不得通过 addChildView 重排整窗原生层。
      record.layer.addChildView(record.view);
      record.mounted = true;
    }
    if (bounds && !sameBounds(record.view.getBounds(), bounds)) record.view.setBounds(bounds);
    const visible = bounds !== null && !record.priming && !record.loadFailed;
    record.view.setVisible(visible);
    // 新挂载的真实页面必须成为当前输入目标，否则第一次打开 Browser Tab
    // 后键盘/剪贴板事件仍留在 App Renderer，表现为“页面能看但不能操作”。
    // 后续仅调整 bounds 不重新抢焦点，避免拖动右栏时打断用户正在操作的工具栏。
    if (!wasMounted && visible) record.contents.focus();
    this.syncLayerVisibility(record.windowId);
  }

  private async loadPage(
    record: BrowserSurfaceRecord,
    url: string,
    timeoutMs = DEFAULT_NAVIGATION_TIMEOUT_MS,
  ): Promise<void> {
    if (record.closed || record.contents.isDestroyed()) {
      throw staleSurfaceError("browser_surface_not_found");
    }
    const operationId = ++record.navigationOperationId;
    record.navigationTargetUrl = url;
    record.priming = true;
    record.loadFailed = false;
    // 新导航必须先终止旧导航。旧 loadURL 的 Promise 和迟到事件仍可能
    // 返回，但下面所有收敛动作都会校验 operationId，不能再把当前页面
    // 隐藏成失败态或重新显示旧 Surface。
    if (record.contents.isLoadingMainFrame()) {
      try {
        record.contents.stop();
      } catch {
        // WebContents 销毁竞态下 stop() 可能同步失败；新的 loadURL 仍会
        // 通过 operationId 成为唯一有效导航。
      }
    }
    try {
      await withNavigationTimeout(
        record.contents.loadURL(url),
        clampNavigationTimeout(timeoutMs),
      );
    } catch (error) {
      if (record.navigationOperationId !== operationId) {
        throw staleSurfaceError("browser_navigation_superseded");
      }
      if (!record.closed) {
        record.priming = true;
        record.loadFailed = true;
        this.unmountSurface(record, this.#windows.get(record.windowId));
        this.#onEvent({
          type: "page_failed",
          binding: this.binding(record),
          reason: error instanceof Error ? error.message : String(error),
        });
        try {
          record.contents.stop();
        } catch {
          // WebContents 销毁竞态下 stop() 可能同步失败，失败状态已收敛。
        }
      }
      throw error;
    }
    if (record.closed) throw staleSurfaceError("browser_surface_not_found");
    if (record.navigationOperationId !== operationId) {
      throw staleSurfaceError("browser_navigation_superseded");
    }
    record.navigationTargetUrl = null;
    record.priming = false;
    record.loadFailed = false;
    this.applySlot(
      record,
      record.slotVisible ? record.slotBounds : null,
      this.#windows.get(record.windowId),
    );
    this.scheduleViewportApply(record);
  }

  private startLoad(record: BrowserSurfaceRecord, url: string): Promise<void> {
    if (record.loadPromise) return record.loadPromise;
    const load = this.loadPage(record, url);
    record.loadPromise = load;
    void load.then(() => {
      if (record.loadPromise === load) record.loadPromise = null;
    }, () => {
      if (record.loadPromise === load) record.loadPromise = null;
    });
    return load;
  }

  private async waitForNavigation(
    contents: WebContents,
    start: () => void,
    timeoutMs?: number,
  ): Promise<void> {
    return waitForNavigationEvent(contents, start, clampNavigationTimeout(timeoutMs));
  }

  private unmountSurface(record: BrowserSurfaceRecord, window: BaseWindow | undefined): void {
    if (!record.mounted) return;
    record.view.setVisible(false);
    // 隐藏 Surface 时保留最后有效的非零 bounds。将 WebContentsView 改成 0x0
    // 会触发 Chromium 的 viewport 重排，导致页面滚动、媒体查询和 SPA 状态
    // 在切换 Tab、模态层或窗口尺寸时反复变化；Surface 关闭时才从宿主移除。
    void window;
    this.syncLayerVisibility(record.windowId);
  }

  private detachSurface(record: BrowserSurfaceRecord, window: BaseWindow | undefined): void {
    if (!record.mounted) return;
    record.view.setVisible(false);
    if (window && !window.isDestroyed()) record.layer.removeChildView(record.view);
    record.mounted = false;
    this.syncLayerVisibility(record.windowId);
  }

  private syncLayerVisibility(windowId: string): void {
    const layer = this.#layers.get(windowId);
    if (!layer) return;
    const visible = [...this.#surfaces.values()].some((record) => (
      record.windowId === windowId
      && !record.closed
      && record.mounted
      && record.slotVisible
      && !record.priming
      && !record.loadFailed
    ));
    layer.setVisible(visible);
  }

  private configurePartition(partitionId: string): void {
    if (this.#configuredPartitions.has(partitionId)) return;
    this.#configuredPartitions.add(partitionId);
    this.#knownPartitions.add(partitionId);
    persistPartitionRegistry(this.#partitionRegistryPath, this.#knownPartitions);
    const browserSession = session.fromPartition(partitionId, { cache: false });
    browserSession.setPermissionCheckHandler(() => false);
    browserSession.setPermissionRequestHandler((_webContents, _permission, callback) => {
      callback(false);
    });
  }

  private installSurfacePolicy(record: BrowserSurfaceRecord): void {
    const { contents: webContents } = record;
    webContents.setWindowOpenHandler((details) => {
      try {
        const url = normalizeNavigableUrl(details.url);
        void loadPopupInCurrentPage(webContents, url, details).catch(() => {
          if (!record.closed) {
            this.#onEvent({
              type: "popup_blocked",
              binding: this.binding(record),
              url: details.url,
            });
          }
        });
        return { action: "deny" };
      } catch {
        // 统一走 popup_blocked 事件，不创建第二个 Target。
      }
      this.#onEvent({
        type: "popup_blocked",
        binding: this.binding(record),
        url: details.url,
      });
      return { action: "deny" };
    });
    webContents.on("will-navigate", (event, url) => {
      try {
        normalizeNavigableUrl(url);
      } catch {
        event.preventDefault();
      }
    });
    webContents.on("did-start-navigation", (_event, _url, _isInPlace, isMainFrame) => {
      if (!isMainFrame) return;
      record.navigationRevision += 1;
      const wasFailed = record.loadFailed;
      record.loadFailed = false;
      if (wasFailed) record.priming = true;
      record.cursorExecutionContextId = null;
      this.#onEvent({
        type: "loading_changed",
        binding: this.binding(record),
        loading: true,
      });
    });
    const publishPage = () => {
      if (
        record.closed
        || record.loadFailed
        || (record.navigationTargetUrl && webContents.isLoadingMainFrame())
      ) return;
      this.#onEvent({
        type: "page_updated",
        binding: this.binding(record),
        page: this.pageState(record),
      });
    };
    webContents.on("did-navigate", publishPage);
    webContents.on("did-navigate-in-page", publishPage);
    webContents.on("page-title-updated", publishPage);
    webContents.on("did-stop-loading", () => {
      if (record.closed || record.navigationTargetUrl && webContents.isLoadingMainFrame()) return;
      this.#onEvent({
        type: "loading_changed",
        binding: this.binding(record),
        loading: false,
      });
      publishPage();
    });
    webContents.on("did-fail-load", (_event, errorCode, errorDescription, _validatedURL, isMainFrame) => {
      if (record.closed || !isMainFrame || errorCode === -3 || record.loadFailed) return;
      if (record.navigationTargetUrl && webContents.isLoadingMainFrame()) return;
      record.loadFailed = true;
      record.priming = true;
      this.unmountSurface(record, this.#windows.get(record.windowId));
      this.#onEvent({
        type: "page_failed",
        binding: this.binding(record),
        reason: errorDescription || `net_error_${errorCode}`,
      });
    });
    webContents.on("did-finish-load", () => {
      if (record.navigationTargetUrl && webContents.isLoadingMainFrame()) return;
      if (record.loadFailed) {
        // Chromium 会在 did-fail-load 后加载 chrome-error:// 页面。它不是用户
        // 请求页面的成功首帧，必须保持 Surface 隐藏，让 Renderer 展示结构化
        // 错误状态，避免错误页白屏重新覆盖右栏。
        record.view.setVisible(false);
        return;
      }
      record.navigationTargetUrl = null;
      record.priming = false;
      this.applySlot(
        record,
        record.slotVisible ? record.slotBounds : null,
        this.#windows.get(record.windowId),
      );
      if (!record.closed && !record.viewportApplied) {
        this.scheduleViewportApply(record);
      }
      if (record.closed || !record.agentControlled) return;
      void this.setAgentCursor(record, true, record.cursor.x, record.cursor.y, record.cursor.action);
    });
    webContents.on("before-input-event", (_event, input) => {
      if (
        record.closed
        || record.automationInputDepth > 0
        || !["rawKeyDown", "keyDown"].includes(input.type)
      ) return;
      this.promote(record.surfaceId);
      void this.setAgentCursor(record, false, null, null, null);
      this.#onEvent({ type: "user_takeover", binding: this.binding(record) });
    });
    webContents.on("before-mouse-event", (_event, input) => {
      if (
        record.closed
        || record.automationInputDepth > 0
        || !["mouseDown", "contextMenu", "mouseWheel"].includes(input.type)
      ) return;
      this.promote(record.surfaceId);
      void this.setAgentCursor(record, false, null, null, null);
      this.#onEvent({ type: "user_takeover", binding: this.binding(record) });
    });
    webContents.on("render-process-gone", (_event, details) => {
      if (record.closed) return;
      this.unmountSurface(record, this.#windows.get(record.windowId));
      const binding = this.binding(record);
      this.#onEvent({
        type: "page_crashed",
        binding,
        reason: details.reason,
      });
      this.invalidateAndRecover(record, `render-process-gone:${details.reason}`);
    });
  }

  private async attachDebugger(record: BrowserSurfaceRecord): Promise<void> {
    const debuggerApi = record.contents.debugger;
    if (!debuggerApi.isAttached()) debuggerApi.attach("1.3");
    await this.refreshDebuggerTarget(record);
    if (record.debuggerListenersInstalled) return;
    record.debuggerListenersInstalled = true;
    debuggerApi.on("message", (_event, method, params) => {
      if (record.closed) return;
      this.#onEvent({
        type: "cdp_event",
        binding: this.binding(record),
        method,
        params: (params ?? {}) as Record<string, unknown>,
      });
    });
    debuggerApi.on("detach", (_event, reason) => {
      if (record.closed) return;
      const binding = this.binding(record);
      this.#onEvent({
        type: "page_crashed",
        binding,
        reason: `debugger-detached:${reason}`,
      });
      this.invalidateAndRecover(record, `debugger-detached:${reason}`);
    });
  }

  private async refreshDebuggerTarget(record: BrowserSurfaceRecord): Promise<void> {
    const target = await record.contents.debugger.sendCommand("Target.getTargetInfo") as {
      targetInfo?: { targetId?: string };
    };
    const targetId = target.targetInfo?.targetId?.trim() || `webcontents-${record.contents.id}`;
    if (record.targetId !== targetId) {
      record.surfaceRevision = this.#surfaces.nextRevision(record.tabId);
      record.cursorExecutionContextId = null;
    }
    record.targetId = targetId;
  }

  private invalidateAndRecover(record: BrowserSurfaceRecord, reason: string): void {
    if (record.recoveryPromise || record.closed || record.contents.isDestroyed()) return;
    record.priming = true;
    record.loadFailed = false;
    this.unmountSurface(record, this.#windows.get(record.windowId));
    record.targetId = "";
    record.surfaceRevision = this.#surfaces.nextRevision(record.tabId);
    const recovery = this.recover(record, reason);
    record.recoveryPromise = recovery;
    void recovery.then(
      () => {
        if (record.recoveryPromise === recovery) record.recoveryPromise = null;
      },
      () => {
        if (record.recoveryPromise === recovery) record.recoveryPromise = null;
      },
    );
  }

  private async recover(record: BrowserSurfaceRecord, reason: string): Promise<void> {
    try {
      await reloadAndWait(record.contents, true);
      if (record.closed) return;
      if (!record.contents.debugger.isAttached()) record.contents.debugger.attach("1.3");
      await this.refreshDebuggerTarget(record);
      record.priming = false;
      record.loadFailed = false;
      record.viewportApplied = false;
      this.scheduleViewportApply(record);
      this.applySlot(
        record,
        record.slotVisible ? record.slotBounds : null,
        this.#windows.get(record.windowId),
      );
      if (record.primary) this.#onEvent({ type: "primary_changed", binding: this.binding(record) });
    } catch (cause) {
      if (!record.closed) {
        console.error("[BrowserSurfaceManager] Browser Surface 恢复失败", {
          surfaceId: record.surfaceId,
          reason,
          error: cause instanceof Error ? cause.message : String(cause),
        });
        this.closeRecord(record);
      }
      throw cause;
    }
  }

  private async applyViewport(record: BrowserSurfaceRecord): Promise<void> {
    if (record.closed || !record.contents.debugger.isAttached()) return;
    if (record.contents.isLoadingMainFrame()) return;
    if (record.viewport.mode === "auto") {
      await record.contents.debugger.sendCommand("Emulation.clearDeviceMetricsOverride");
      await record.contents.debugger.sendCommand("Emulation.setTouchEmulationEnabled", {
        enabled: false,
      });
      record.viewportApplied = true;
      return;
    }
    const width = Math.max(320, Math.round(record.viewport.width));
    const height = Math.max(240, Math.round(record.viewport.height));
    const mobile = record.viewport.device_type === "mobile";
    await record.contents.debugger.sendCommand("Emulation.setDeviceMetricsOverride", {
      width,
      height,
      deviceScaleFactor: record.viewport.device_scale_factor_millis / 1_000,
      mobile,
      screenWidth: width,
      screenHeight: height,
      screenOrientation: {
        type: width > height ? "landscapePrimary" : "portraitPrimary",
        angle: width > height ? 90 : 0,
      },
    });
    await record.contents.debugger.sendCommand("Emulation.setTouchEmulationEnabled", {
      enabled: mobile,
      maxTouchPoints: mobile ? 5 : 1,
    });
    record.viewportApplied = true;
  }

  private scheduleViewportApply(record: BrowserSurfaceRecord): void {
    record.viewportApplyDirty = true;
    if (record.viewportApplyPromise) return;
    record.viewportApplyPromise = this.flushViewportApply(record).finally(() => {
      record.viewportApplyPromise = null;
      if (!record.closed && record.viewportApplyDirty) this.scheduleViewportApply(record);
    });
  }

  private async flushViewportApply(record: BrowserSurfaceRecord): Promise<void> {
    while (!record.closed && record.viewportApplyDirty) {
      record.viewportApplyDirty = false;
      if (record.contents.isLoadingMainFrame()) return;
      await this.applyViewport(record);
    }
  }

  private async setAgentCursor(
    record: BrowserSurfaceRecord,
    visible: boolean,
    x: number | null,
    y: number | null,
    action: string | null,
  ): Promise<void> {
    if (record.closed || record.contents.isDestroyed()) return;
    record.cursor = { visible, x, y, action };
    this.#onEvent({
      type: "agent_cursor",
      binding: this.binding(record),
      visible,
      x,
      y,
      action,
    });
    const update = async () => {
      if (record.closed || record.contents.isDestroyed()) return;
      // Page.getFrameTree 对尚未建立首个文档的 WebContents 不会返回。首个
      // 文档由 materialize 统一完成；导航期间则由 did-finish-load 重新应用
      // 最新状态，不能把页面命令队列绑定到绘制光标的 CDP 请求。
      if (record.priming || record.contents.isLoadingMainFrame() || !record.contents.getURL()) return;
      try {
        if (!record.contents.debugger.isAttached()) return;
        if (record.cursorExecutionContextId === null) {
          const frameTree = await sendCdpCommandWithTimeout(
            record.contents,
            "Page.getFrameTree",
            {},
            CURSOR_CDP_COMMAND_TIMEOUT_MS,
          ) as {
            frameTree?: { frame?: { id?: string } };
          };
          const frameId = frameTree.frameTree?.frame?.id;
          if (!frameId) return;
          const world = await sendCdpCommandWithTimeout(record.contents, "Page.createIsolatedWorld", {
            frameId,
            worldName: "magi-agent-cursor",
            grantUniveralAccess: false,
          }, CURSOR_CDP_COMMAND_TIMEOUT_MS) as { executionContextId?: number };
          if (!world.executionContextId) return;
          record.cursorExecutionContextId = world.executionContextId;
        }
        await sendCdpCommandWithTimeout(record.contents, "Runtime.evaluate", {
          contextId: record.cursorExecutionContextId,
          returnByValue: true,
          expression: `(() => {
            const state = ${JSON.stringify({ visible, x, y, action })};
            const cursorAsset = ${JSON.stringify(AGENT_CURSOR_ASSET)};
            let host = document.getElementById('magi-agent-cursor');
            if (!host) {
              host = document.createElement('div');
              host.id = 'magi-agent-cursor';
              host.setAttribute('aria-hidden', 'true');
              host.style.cssText = 'position:fixed;z-index:2147483647;pointer-events:none;width:20px;height:20px;transform:translate(-2px,-2px);transition:left 60ms linear,top 60ms linear;display:none;';
              const shadow = host.attachShadow({ mode: 'closed' });
              const image = document.createElement('div');
              image.style.cssText = 'width:20px;height:20px;background: center / contain no-repeat;';
              image.style.backgroundImage = 'url(' + cursorAsset + ')';
              shadow.append(image);
              (document.documentElement || document.body)?.append(host);
            }
            host.style.display = state.visible ? 'block' : 'none';
            if (state.visible && state.x !== null && state.y !== null) {
              host.style.left = state.x + 'px';
              host.style.top = state.y + 'px';
            }
            return true;
          })()`,
        }, CURSOR_CDP_COMMAND_TIMEOUT_MS);
      } catch {
        // 导航会清理 isolated world；did-finish-load 会按最新状态重建它。
        record.cursorExecutionContextId = null;
      }
    };
    const previous = record.cursorUpdatePromise ?? Promise.resolve();
    const next = previous.catch(() => undefined).then(update);
    record.cursorUpdatePromise = next;
    await next.finally(() => {
      if (record.cursorUpdatePromise === next) record.cursorUpdatePromise = null;
    });
  }

  private promote(surfaceId: string): void {
    const record = this.requireRecord(surfaceId);
    const promotion = this.#surfaces.promote(surfaceId);
    if (promotion.previous?.surfaceId === surfaceId) {
      return;
    }
    const previous = promotion.previous;
    if (previous && !previous.closed) {
      previous.surfaceRevision = this.#surfaces.nextRevision(previous.tabId);
    }
    record.surfaceRevision = this.#surfaces.nextRevision(record.tabId);
    this.#onEvent({ type: "primary_changed", binding: this.binding(record) });
  }

  private promoteFallback(tabId: string): void {
    const fallback = this.#surfaces.promoteFallback(tabId);
    if (!fallback) return;
    fallback.surfaceRevision = this.#surfaces.nextRevision(fallback.tabId);
    this.#onEvent({ type: "primary_changed", binding: this.binding(fallback) });
  }

  private pageState(record: BrowserSurfaceRecord): BrowserPageState {
    const url = record.contents.getURL() || "about:blank";
    return {
      tab_id: record.tabId,
      url,
      origin: safeOrigin(url),
      title: record.contents.getTitle() || "",
      navigation_revision: record.navigationRevision,
    };
  }

  private binding(record: BrowserSurfaceRecord): BrowserSurfaceBinding {
    return {
      desktop_epoch: this.#desktopEpoch,
      window_id: record.windowId,
      surface_id: record.surfaceId,
      surface_revision: record.surfaceRevision,
      tab_id: record.tabId,
      web_contents_id: record.contents.id,
      target_id: record.targetId,
      browser_context_id: record.partitionId,
      navigation_revision: record.navigationRevision,
    };
  }

  private requireRecord(surfaceId: string): BrowserSurfaceRecord {
    const record = this.#surfaces.get(surfaceId);
    if (!record || record.closed) throw staleSurfaceError("browser_surface_not_found");
    return record;
  }

  private closeRecord(record: BrowserSurfaceRecord, promoteFallback = true): void {
    if (record.closed) return;
    record.closed = true;
    if (!record.contents.isDestroyed() && record.contents.debugger.isAttached()) {
      try {
        record.contents.debugger.detach();
      } catch {
        // WebContents 正在销毁时 detach 可能同步抛错，关闭流程不能因此中断。
      }
    }
    const window = this.#windows.get(record.windowId);
    this.detachSurface(record, window);
    if (!record.contents.isDestroyed()) record.contents.close();
    this.removeRecordIndexes(record);
    if (promoteFallback) this.promoteFallback(record.tabId);
  }

  private removeRecordIndexes(record: BrowserSurfaceRecord): void {
    this.#surfaces.remove(record);
  }

  private assertActivationCurrent(windowId: string, generation: number | undefined): void {
    if (generation === undefined) return;
    const current = this.#activationGenerations.get(windowId);
    if (current !== generation) {
      throw staleSurfaceError("browser_surface_activation_stale");
    }
  }
}

function normalizeNavigableUrl(value: string): string {
  const trimmed = value.trim() || "about:blank";
  if (trimmed === "about:blank") return trimmed;
  const candidate = /^[A-Za-z][A-Za-z\d+.-]*:/u.test(trimmed) ? trimmed : `https://${trimmed}`;
  const url = new URL(candidate);
  if (!ALLOWED_NAVIGATION_PROTOCOLS.has(url.protocol) || BLOCKED_HOSTS.has(url.hostname)) {
    throw new Error(`browser_navigation_url_rejected:${url.protocol}//${url.host}`);
  }
  if (url.username || url.password) throw new Error("browser_navigation_credentials_rejected");
  return url.href;
}

function safeOrigin(value: string): string | null {
  try {
    const url = new URL(value);
    return url.origin === "null" ? null : url.origin;
  } catch {
    return null;
  }
}

function browserPartitionId(browserSessionId: string): string {
  const safe = browserSessionId.replace(/[^A-Za-z0-9._-]/gu, "_");
  return `magi-browser-${safe}`;
}

function readPartitionRegistry(path: string | null): string[] {
  if (!path || !existsSync(path)) return [];
  try {
    const value: unknown = JSON.parse(readFileSync(path, "utf8"));
    if (!Array.isArray(value)) return [];
    return value.filter((entry): entry is string => (
      typeof entry === "string" && /^magi-browser-[A-Za-z0-9._-]+$/u.test(entry)
    ));
  } catch {
    return [];
  }
}

function persistPartitionRegistry(path: string | null, partitions: Set<string>): void {
  if (!path) return;
  const temporaryPath = `${path}.${process.pid}.tmp`;
  try {
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(temporaryPath, `${JSON.stringify([...partitions].sort())}\n`, "utf8");
    renameSync(temporaryPath, path);
  } catch (error) {
    try {
      if (existsSync(temporaryPath)) unlinkSync(temporaryPath);
    } catch {
      // The registry is an optimization for complete cleanup coverage; an
      // inability to update it must not make opening a browser tab fail.
    }
    console.warn("[BrowserSurfaceManager] 浏览器 partition 注册表写入失败", error);
  }
}

function staleSurfaceError(code: string): Error {
  const error = new Error(code);
  error.name = "BrowserSurfaceError";
  return error;
}

function browserSurfaceError(code: string): Error {
  const error = new Error(code);
  error.name = "BrowserSurfaceError";
  return error;
}

function sameBounds(left: Rectangle, right: Rectangle): boolean {
  return left.x === right.x
    && left.y === right.y
    && left.width === right.width
    && left.height === right.height;
}

async function sendCdpCommandWithTimeout(
  contents: WebContents,
  method: string,
  params: Record<string, unknown>,
  timeoutMs: number,
): Promise<unknown> {
  return withTimeout(contents.debugger.sendCommand(method, params), timeoutMs, method);
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, method: string): Promise<T> {
  let timer: NodeJS.Timeout | null = null;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`browser_cdp_timeout:${method}`)), timeoutMs);
        timer.unref();
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function clampNavigationTimeout(value: number | undefined): number {
  if (!Number.isFinite(value)) return DEFAULT_NAVIGATION_TIMEOUT_MS;
  return Math.max(1_000, Math.min(DEFAULT_NAVIGATION_TIMEOUT_MS, Math.round(value as number)));
}

async function withNavigationTimeout<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  let timer: NodeJS.Timeout | null = null;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error("browser_navigation_timeout")), timeoutMs);
        timer.unref();
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function waitForNavigationEvent(
  contents: WebContents,
  start: () => void,
  timeoutMs: number,
): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    let settled = false;
    const cleanup = () => {
      clearTimeout(timer);
      contents.off("did-stop-loading", done);
      contents.off("did-fail-load", failed);
    };
    const finish = (error?: Error) => {
      if (settled) return;
      settled = true;
      cleanup();
      if (error) reject(error);
      else resolve();
    };
    const done = () => finish();
    const failed = (
      _event: Electron.Event,
      errorCode: number,
      errorDescription: string,
      _validatedURL: string,
      isMainFrame: boolean,
    ) => {
      if (!isMainFrame || errorCode === -3) return;
      finish(new Error(`browser_navigation_failed:${errorCode}:${errorDescription}`));
    };
    const timer = setTimeout(() => finish(new Error("browser_navigation_timeout")), timeoutMs);
    timer.unref();
    contents.once("did-stop-loading", done);
    contents.once("did-fail-load", failed);
    try {
      start();
    } catch (error) {
      finish(error instanceof Error ? error : new Error(String(error)));
    }
  });
}

async function reloadAndWait(
  contents: WebContents,
  ignoreCache: boolean,
  timeoutMs?: number,
): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(new Error("browser_navigation_timeout"));
    }, clampNavigationTimeout(timeoutMs));
    timer.unref();
    const cleanup = () => {
      clearTimeout(timer);
      contents.off("did-stop-loading", done);
      contents.off("did-fail-load", failed);
    };
    const finish = () => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve();
    };
    const failed = (_event: Electron.Event, errorCode: number, errorDescription: string, _validatedURL: string, isMainFrame: boolean) => {
      if (!isMainFrame || errorCode === -3) return;
      if (settled) return;
      settled = true;
      cleanup();
      reject(new Error(`browser_navigation_failed:${errorCode}:${errorDescription}`));
    };
    const done = () => finish();
    contents.once("did-stop-loading", done);
    contents.once("did-fail-load", failed);
    if (ignoreCache) contents.reloadIgnoringCache();
    else contents.reload();
  });
}

async function loadPopupInCurrentPage(
  webContents: Electron.WebContents,
  url: string,
  details: HandlerDetails,
): Promise<void> {
  const postBody = details.postBody;
  const contentType = postBody?.boundary
    ? `${postBody.contentType}; boundary=${postBody.boundary}`
    : postBody?.contentType;
  await webContents.loadURL(url, {
    httpReferrer: details.referrer,
    ...(postBody ? { postData: postBody.data } : {}),
    ...(contentType ? { extraHeaders: `Content-Type: ${contentType}` } : {}),
  });
}
