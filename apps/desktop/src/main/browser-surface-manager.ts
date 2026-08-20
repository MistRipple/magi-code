import { randomUUID } from "node:crypto";
import { basename, dirname, join } from "node:path";
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
  | {
      type: "download";
      binding: BrowserSurfaceBinding;
      suggestedFilename: string;
      state: "started" | "progressing" | "completed" | "cancelled" | "interrupted";
      byteLength?: number;
      error?: string;
    }
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
      sessionId?: string;
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
  host: View;
  /**
   * 每个 Browser Tab 保留独立 WebContents；非当前 Surface 从窗口内容视图
   * 解绑但不销毁 WebContents，重新激活时再挂回，不丢失页面状态。
   */
  mounted: boolean;
  slotVisible: boolean;
  slotBounds: Rectangle | null;
  activationGeneration: number | null;
  priming: boolean;
  targetId: string;
  navigationRevision: number;
  navigationOperationId: number;
  navigationFailureReportedRevision: number | null;
  navigationTargetUrl: string | null;
  viewport: BrowserLogicalViewport;
  primary: boolean;
  closed: boolean;
  automationInputDepth: number;
  agentControlled: boolean;
  cursor: { visible: boolean; x: number | null; y: number | null; action: string | null };
  cursorExecutionContextId: number | null;
  cdpLane: Promise<void>;
  viewportApplied: boolean;
  viewportApplyPromise: Promise<void> | null;
  viewportApplyDirty: boolean;
  debuggerListenersInstalled: boolean;
  cdpSessionIds: Set<string>;
  debuggerReadyPromise: Promise<void> | null;
  recoveryPromise: Promise<void> | null;
  loadPromise: Promise<void> | null;
}

interface SurfaceLaneContext {
  track(promise: Promise<unknown>): void;
}

const ALLOWED_NAVIGATION_PROTOCOLS = new Set(["http:", "https:", "about:"]);
const BLOCKED_HOSTS = new Set(["169.254.169.254", "metadata.google.internal"]);
// 固定资产只用于隔离世界中的可视化指针，不读取或修改页面的光标样式。
// 使用内联矢量资源避免依赖页面外部网络和站点 CSS，且在 Shadow DOM 内保持封装。
const AGENT_CURSOR_ASSET = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='20' height='20' viewBox='0 0 20 20'%3E%3Cpath d='M3 1.8 17.2 11l-6.1 1.1 3.7 5.1-2.2 1.6-3.7-5.1-3.4 5.2Z' fill='%232563eb' stroke='white' stroke-width='1.2' stroke-linejoin='round'/%3E%3C/svg%3E";
const ALLOWED_WORKER_CDP_METHODS = new Set([
  "DOM.getDocument",
  "DOM.querySelector",
  "DOM.enable",
  "DOM.disable",
  "DOM.describeNode",
  "DOM.getAttributes",
  "DOM.getBoxModel",
  "DOM.getContentQuads",
  "DOM.getNodeForLocation",
  "DOM.resolveNode",
  "DOM.pushNodeByPathToFrontend",
  "DOM.getOuterHTML",
  "DOMDebugger.getEventListeners",
  "DOM.setFileInputFiles",
  "DOM.focus",
  "Accessibility.getFullAXTree",
  "Browser.getVersion",
  "IO.read",
  "IO.close",
  "Network.loadNetworkResource",
  "Network.setUserAgentOverride",
  "Network.setBlockedURLs",
  "Network.setCacheDisabled",
  "Network.clearBrowserCache",
  "Network.getResponseBody",
  "Target.getTargetInfo",
  "Target.setAutoAttach",
  "Target.detachFromTarget",
  "Page.navigate",
  "Page.getNavigationHistory",
  "Page.navigateToHistoryEntry",
  "Page.setLifecycleEventsEnabled",
  "Page.reload",
  "Page.stopLoading",
  "Page.getAppManifest",
  "Page.disable",
  "Page.frameNavigated",
  "Page.domContentEventFired",
  "Page.loadEventFired",
  "Page.lifecycleEvent",
  "Runtime.runIfWaitingForDebugger",
  "Runtime.callFunctionOn",
  "Runtime.addBinding",
  "Runtime.removeBinding",
  "Runtime.terminateExecution",
  "Runtime.releaseObject",
  "Profiler.enable",
  "Profiler.disable",
  "Profiler.start",
  "Profiler.stop",
  "Profiler.startPreciseCoverage",
  "Profiler.stopPreciseCoverage",
  "Profiler.takePreciseCoverage",
  "Debugger.getScriptSource",
  "Log.startViolationsReport",
  "Log.stopViolationsReport",
  "Animation.enable",
  "Animation.disable",
  "WebMCP.enable",
  "WebMCP.disable",
  "Debugger.enable",
  "Debugger.disable",
  "Debugger.setAsyncCallStackDepth",
  "Debugger.resume",
  "Debugger.setSkipAllPauses",
  "Log.enable",
  "Log.disable",
  "Audits.enable",
  "Audits.disable",
  "CSS.enable",
  "CSS.disable",
  "CSS.getMatchedStylesForNode",
  "CSS.getStyleSheetText",
  "CSS.startRuleUsageTracking",
  "CSS.stopRuleUsageTracking",
  "Storage.getUsageAndQuota",
  "Storage.clearDataForOrigin",
  "Network.clearBrowserCache",
  "Network.setCacheDisabled",
  "Network.setBlockedURLs",
  "Network.getAllCookies",
  "Network.clearBrowserCookies",
  "Emulation.setScriptExecutionDisabled",
  "Storage.getCookies",
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
  "Tracing.recordClockSyncMarker",
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
  readonly #downloadRoot: string | null;
  readonly #windows = new Map<string, BaseWindow>();
  readonly #browserHosts = new Map<string, View>();
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
    this.#downloadRoot = this.#partitionRegistryPath
      ? join(dirname(this.#partitionRegistryPath), "browser-downloads")
      : null;
    for (const partitionId of readPartitionRegistry(this.#partitionRegistryPath)) {
      this.#knownPartitions.add(partitionId);
    }
  }

  attachWindow(windowId: string, window: BaseWindow, browserHost: View): void {
    this.#windows.set(windowId, window);
    this.#browserHosts.set(windowId, browserHost);
    this.#activationGenerations.set(windowId, 0);
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
    if (!record) record = this.createSurface(input);
    if (input.activationGeneration !== undefined) {
      record.activationGeneration = input.activationGeneration;
    }
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
      if (input.awaitPageLoad === true) await load;
      else void load.catch(() => undefined);
    } else if (!record.contents.isLoadingMainFrame()) {
      this.scheduleViewportApply(record);
    }
    return this.binding(record);
  }
  private createSurface(input: MaterializeSurfaceInput): BrowserSurfaceRecord {
    const window = this.#windows.get(input.windowId);
    if (!window || window.isDestroyed()) throw new Error("desktop_window_not_found");
    const host = this.#browserHosts.get(input.windowId);
    if (!host) throw new Error("browser_surface_host_not_found");
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
      host,
      mounted: false,
      slotVisible: false,
      slotBounds: null,
      activationGeneration: input.activationGeneration ?? null,
      priming: true,
      // CDP target 查询是异步握手，不参与 Surface 绑定。这个 ID 只用于
      // binding 一致性校验，使用 WebContents 生命周期内稳定的宿主 ID。
      targetId: `webcontents-${contents.id}`,
      navigationRevision: input.navigationRevision,
      navigationOperationId: 0,
      navigationFailureReportedRevision: null,
      navigationTargetUrl: null,
      viewport: input.viewport,
      primary: false,
      closed: false,
      automationInputDepth: 0,
      agentControlled: false,
      cursor: { visible: false, x: null, y: null, action: null },
      cursorExecutionContextId: null,
      cdpLane: Promise.resolve(),
      viewportApplied: false,
      viewportApplyPromise: null,
      viewportApplyDirty: false,
      debuggerListenersInstalled: false,
      cdpSessionIds: new Set(),
      debuggerReadyPromise: null,
      recoveryPromise: null,
      loadPromise: null,
    };
    this.#surfaces.add(record);
    // WebContents 的生命周期独立于原生 View 的挂载生命周期。没有当前
    // 内容槽时不挂载 View，避免隐藏的 Chromium 原生子视图继续参与命中测试。
    this.applySlot(record, null, window);
    contents.once("destroyed", () => {
      // destroyed 可能发生在自动化命令期间。复用统一关闭入口，确保
      // Surface 挂载和注册索引按同一顺序收敛。
      this.closeRecord(record);
    });
    this.installSurfacePolicy(record);
    const debuggerReady = this.enqueueCdp(record, () => this.attachDebugger(record));
    record.debuggerReadyPromise = debuggerReady;
    void debuggerReady.then(
      () => {
        if (record.debuggerReadyPromise === debuggerReady) record.debuggerReadyPromise = null;
      },
      (error) => {
        if (record.debuggerReadyPromise === debuggerReady) record.debuggerReadyPromise = null;
        if (!record.closed) {
          console.error("[BrowserSurfaceManager] Browser Surface 调试器初始化失败", {
            surfaceId: record.surfaceId,
            error: error instanceof Error ? error.message : String(error),
          });
        }
      },
    );
    return record;
  }

  bindContentSurface(windowId: string, tabId: string, bounds: Rectangle | null): void {
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
        // 其他 WebContents 保留状态，但从窗口内容视图解绑以彻底退出
        // 原生命中树。
        record.slotBounds = null;
        record.slotVisible = false;
        this.unmountSurface(record, window);
      }
    }
  }

  bindingForTabInWindow(tabId: string, windowId: string): BrowserSurfaceBinding | null {
    const record = this.#surfaces.forWindowTab(windowId, tabId);
    return record && !record.closed ? this.binding(record) : null;
  }

  contentBoundsForTab(windowId: string, tabId: string): Rectangle | null {
    const record = this.#surfaces.forWindowTab(windowId, tabId);
    if (!record || !this.isRenderable(record)) return null;
    return { ...record.slotBounds };
  }

  focusTab(windowId: string, tabId: string): boolean {
    const record = this.#surfaces.forWindowTab(windowId, tabId);
    if (!record || !this.isRenderable(record) || record.contents.isDestroyed()) return false;
    this.promote(record.surfaceId);
    record.contents.focus();
    return true;
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
    sessionId?: string,
  ): Promise<unknown> {
    if (!ALLOWED_WORKER_CDP_METHODS.has(method)) {
      throw new Error(`browser_cdp_method_denied:${method}`);
    }
    const record = this.requireRecord(binding.surface_id);
    await this.waitForDebugger(record);
    const contents = this.recordForBinding(binding);
    if (sessionId && !record.cdpSessionIds.has(sessionId)) {
      throw staleSurfaceError("browser_cdp_session_stale");
    }
    // 自动化面向逻辑 Browser Tab 的真实 WebContents，而不是右栏当前是否
    // 正在展示该 Tab。切换到代码、图片或另一个 Browser Tab 时，Surface 会
    // 暂时从内容槽解绑，但 Chromium 页面仍然是有效的自动化目标；截图、
    // 命中检测和 DOM 操作不应因此被错误收敛为 no_content_slot。
    if (!contents.debugger.isAttached()) {
      throw staleSurfaceError("browser_debugger_detached");
    }
    const injectsInput = method.startsWith("Input.");
    if (injectsInput) record.automationInputDepth += 1;
    try {
      const screenshotParams = params;
      const result = await this.enqueueCdp(record, ({ track }) => sendCdpCommandWithTimeout(
        contents,
        method,
        screenshotParams,
        method === "Page.captureScreenshot" ? SCREENSHOT_CDP_COMMAND_TIMEOUT_MS : DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        sessionId,
        track,
      ));
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

  private enqueueCdp<T>(
    record: BrowserSurfaceRecord,
    operation: (context: SurfaceLaneContext) => Promise<T>,
  ): Promise<T> {
    const previous = record.cdpLane;
    const tracked: Promise<unknown>[] = [];
    const run = previous.catch(() => undefined).then(() => operation({
      track: (promise) => tracked.push(promise),
    }));
    const settled = run.then(
      async () => { await Promise.allSettled(tracked); },
      async () => { await Promise.allSettled(tracked); },
    );
    record.cdpLane = settled.then(() => undefined, () => undefined);
    return run;
  }

  async navigate(
    binding: BrowserSurfaceBinding,
    navigation: BrowserNavigation,
  ): Promise<BrowserPageState> {
    const record = this.requireRecord(binding.surface_id);
    await this.waitForDebugger(record);
    const contents = this.recordForBinding(binding);
    switch (navigation.action) {
      case "url": {
        let initScriptId: string | null = null;
        if (navigation.init_script?.trim()) {
          const installed = await this.enqueueCdp(record, () => sendCdpCommandWithTimeout(
            contents,
            "Page.addScriptToEvaluateOnNewDocument",
            { source: navigation.init_script },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
          )) as { identifier?: string };
          initScriptId = typeof installed.identifier === "string" ? installed.identifier : null;
        }
        try {
          await this.loadPage(
            this.requireRecord(binding.surface_id),
            normalizeNavigableUrl(navigation.url),
            navigation.timeout_ms,
            navigation.handle_before_unload,
          );
        } finally {
          if (initScriptId) {
            await this.enqueueCdp(record, () => sendCdpCommandWithTimeout(
              contents,
              "Page.removeScriptToEvaluateOnNewDocument",
              { identifier: initScriptId },
              DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            )).catch(() => undefined);
          }
        }
        break;
      }
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
          navigation.handle_before_unload,
        );
        break;
    }
    const currentRecord = this.requireRecord(binding.surface_id);
    return this.pageState(currentRecord);
  }

  async setViewport(
    binding: BrowserSurfaceBinding,
    viewport: BrowserLogicalViewport,
  ): Promise<void> {
    const record = this.requireRecord(binding.surface_id);
    await this.waitForDebugger(record);
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
    this.#browserHosts.delete(windowId);
    this.#activationGenerations.delete(windowId);
  }

  closeAll(): void {
    for (const record of [...this.#surfaces.values()]) this.closeRecord(record, false);
    this.#surfaces.clear();
    this.#windows.clear();
    this.#browserHosts.clear();
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
    await Promise.all([...this.#surfaces.values()]
      .filter((record) => !record.closed && !record.contents.isDestroyed())
      .map((record) => reloadAndWait(record.contents, true)));
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
    if (!bounds) {
      this.detachSurface(record, window);
      return;
    }
    if (!record.mounted) {
      // Browser Host 是 WindowManager 按当前右栏内容槽设置边界的独立
      // 原生容器。WebContentsView 只在 Host 的局部坐标中占满内容槽，
      // 不再把窗口坐标直接交给 WebContentsView。
      record.host.addChildView(record.view, 0);
      record.mounted = true;
    }
    const localBounds = { x: 0, y: 0, width: bounds.width, height: bounds.height };
    if (!sameBounds(record.view.getBounds(), localBounds)) record.view.setBounds(localBounds);
    // 页面加载和调试器初始化是 Surface 内部状态，不能阻塞真实浏览器
    // 视图进入内容槽。Chromium 自行展示当前文档及加载过程；只有失败页
    // 才隐藏，避免激活流程变成“正在连接浏览器”的空白等待层。
    // 导航过程由 Chromium 自己绘制。只有 Surface 生命周期失败才解绑，
    // 页面导航失败仍保留真实 WebContentsView，避免右栏黑屏或空槽。
    record.view.setVisible(true);
  }

  private async loadPage(
    record: BrowserSurfaceRecord,
    url: string,
    timeoutMs = DEFAULT_NAVIGATION_TIMEOUT_MS,
    handleBeforeUnload?: "accept" | "dismiss",
    track?: (promise: Promise<unknown>) => void,
  ): Promise<void> {
    if (record.closed || record.contents.isDestroyed()) {
      throw staleSurfaceError("browser_surface_not_found");
    }
    const operationId = ++record.navigationOperationId;
    record.navigationTargetUrl = url;
    record.navigationFailureReportedRevision = null;
    record.priming = true;
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
    const allowBeforeUnload = handleBeforeUnload === "accept"
      ? (event: Electron.Event) => event.preventDefault()
      : null;
    if (allowBeforeUnload) record.contents.on("will-prevent-unload", allowBeforeUnload);
    try {
      const loadPromise = record.contents.loadURL(url);
      track?.(loadPromise);
      await withNavigationTimeout(
        loadPromise,
        clampNavigationTimeout(timeoutMs),
      );
    } catch (error) {
      if (record.navigationOperationId !== operationId) {
        throw staleSurfaceError("browser_navigation_superseded");
      }
      if (!record.closed) {
        // 导航失败不代表原生 Surface 失效。保留 WebContentsView 在内容槽中，
        // 让 Chromium 自己展示当前文档或错误页，避免右栏收敛成黑屏。
        record.priming = false;
        record.navigationTargetUrl = null;
        this.applySlot(
          record,
          record.slotVisible ? record.slotBounds : null,
          this.#windows.get(record.windowId),
        );
        this.publishNavigationFailure(record, error instanceof Error ? error.message : String(error));
      }
      throw error;
    } finally {
      if (allowBeforeUnload) record.contents.off("will-prevent-unload", allowBeforeUnload);
    }
    if (record.closed) throw staleSurfaceError("browser_surface_not_found");
    if (record.navigationOperationId !== operationId) {
      throw staleSurfaceError("browser_navigation_superseded");
    }
    record.navigationTargetUrl = null;
    record.priming = false;
    this.applySlot(
      record,
      record.slotVisible ? record.slotBounds : null,
      this.#windows.get(record.windowId),
    );
    this.scheduleViewportApply(record);
  }

  private startLoad(record: BrowserSurfaceRecord, url: string): Promise<void> {
    if (record.loadPromise) return record.loadPromise;
    // 初始导航是 Chromium 的原生加载过程，不得占用 CDP 命令队列。
    // 页面可以继续加载，同时截图、快照和输入仍然必须立即进入同一个
    // WebContents；否则慢页面会把整个 Browser Tab 锁在“正在连接”状态。
    const load = this.loadPage(record, url);
    record.loadPromise = load;
    void load.then(() => {
      if (record.loadPromise === load) record.loadPromise = null;
    }, () => {
      if (record.loadPromise === load) record.loadPromise = null;
    });
    return load;
  }

  private publishNavigationFailure(record: BrowserSurfaceRecord, reason: string): void {
    if (record.closed || record.navigationFailureReportedRevision === record.navigationRevision) return;
    record.navigationFailureReportedRevision = record.navigationRevision;
    this.#onEvent({
      type: "page_failed",
      binding: this.binding(record),
      reason,
    });
  }

  private async waitForNavigation(
    contents: WebContents,
    start: () => void,
    timeoutMs?: number,
  ): Promise<void> {
    return waitForNavigationEvent(contents, start, clampNavigationTimeout(timeoutMs));
  }

  private unmountSurface(record: BrowserSurfaceRecord, window: BaseWindow | undefined): void {
    // 只解绑原生 View，不关闭 WebContents。这样切换 Tab、面板或失败恢复
    // 都不会让后台页面进入命中树，也不会因为反复改成 0x0 而触发 Chromium
    // viewport 重排；重新获得内容槽时 applySlot 会复用同一个 WebContents。
    this.detachSurface(record, window);
  }

  private detachSurface(record: BrowserSurfaceRecord, window: BaseWindow | undefined): void {
    if (!record.mounted) return;
    try {
      record.view.setVisible(false);
    } catch {
      // WebContentsView 可能已随宿主窗口销毁。
    }
    try {
      if (window || record.host) record.host.removeChildView(record.view);
    } catch {
      // destroyed 与 closeRecord 可能交错到达，解绑必须幂等。
    }
    record.mounted = false;
  }

  private isRenderable(record: BrowserSurfaceRecord): record is BrowserSurfaceRecord & {
    slotBounds: Rectangle;
  } {
    return !record.closed
      && record.mounted
      && record.slotVisible
      && record.slotBounds !== null
      && record.view.getVisible();
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
    browserSession.on("will-download", (event, item, webContents) => {
      this.handleDownload(event, item, webContents);
    });
  }

  private handleDownload(
    event: Electron.Event,
    item: Electron.DownloadItem,
    webContents: WebContents,
  ): void {
    const record = this.surfaceForContents(webContents);
    if (!record || record.closed || !this.#downloadRoot) {
      // Downloads never fall back to the user's Downloads directory. A
      // browser download is either stored in Magi's private data directory or
      // explicitly rejected, so the host cannot write outside its boundary.
      event.preventDefault();
      if (record && !record.closed) {
        this.emitDownload(record, "download", "interrupted", "browser_download_storage_unavailable");
      }
      return;
    }
    const filename = safeDownloadFilename(item.getFilename());
    const directory = join(this.#downloadRoot, safeDownloadSegment(record.browserSessionId));
    try {
      mkdirSync(directory, { recursive: true });
      item.setSavePath(join(directory, `${Date.now()}-${randomUUID()}-${filename}`));
    } catch (error) {
      event.preventDefault();
      this.emitDownload(
        record,
        filename,
        "interrupted",
        error instanceof Error ? error.message : "browser_download_storage_failed",
      );
      return;
    }
    this.emitDownload(record, filename, "started", undefined, item.getTotalBytes());
    item.on("updated", (_event, state) => {
      if (!record.closed) {
        this.emitDownload(record, filename, state === "progressing" ? "progressing" : "interrupted", undefined, downloadBytes(item));
      }
    });
    item.once("done", (_event, state) => {
      if (!record.closed) this.emitDownload(record, filename, state, undefined, downloadBytes(item));
    });
  }

  private emitDownload(
    record: BrowserSurfaceRecord,
    suggestedFilename: string,
    state: "started" | "progressing" | "completed" | "cancelled" | "interrupted",
    error?: string,
    byteLength?: number,
  ): void {
    this.#onEvent({
      type: "download",
      binding: this.binding(record),
      suggestedFilename,
      state,
      ...(byteLength !== undefined && byteLength > 0 ? { byteLength } : {}),
      ...(error ? { error } : {}),
    });
  }

  private surfaceForContents(contents: WebContents): BrowserSurfaceRecord | null {
    for (const record of this.#surfaces.values()) {
      if (!record.closed && record.contents === contents) return record;
    }
    return null;
  }

  private installSurfacePolicy(record: BrowserSurfaceRecord): void {
    const { contents: webContents } = record;
    webContents.setWindowOpenHandler((details) => {
      try {
        const url = normalizeNavigableUrl(details.url);
        void this.enqueueCdp(record, () => loadPopupInCurrentPage(webContents, url, details)).catch(() => {
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
      record.navigationFailureReportedRevision = null;
      record.priming = true;
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
      if (record.closed || !isMainFrame || errorCode === -3) return;
      if (record.navigationTargetUrl && webContents.isLoadingMainFrame()) return;
      // did-fail-load 是页面导航结果，不是原生 Surface 崩溃。保持 View
      // 挂载和可见，避免失败页期间出现黑屏或重新计算内容槽。
      record.priming = false;
      record.navigationTargetUrl = null;
      this.applySlot(
        record,
        record.slotVisible ? record.slotBounds : null,
        this.#windows.get(record.windowId),
      );
      this.publishNavigationFailure(record, errorDescription || `net_error_${errorCode}`);
      this.scheduleViewportApply(record);
    });
    webContents.on("did-finish-load", () => {
      if (record.navigationTargetUrl && webContents.isLoadingMainFrame()) return;
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

  private async waitForDebugger(record: BrowserSurfaceRecord): Promise<void> {
    if (record.recoveryPromise) {
      try {
        await record.recoveryPromise;
      } catch {
        // Recovery owns its terminal error state. The command below performs
        // the final lifecycle validation and returns a stable surface error.
      }
    }
    const debuggerReady = record.debuggerReadyPromise;
    if (debuggerReady) {
      try {
        await debuggerReady;
      } catch {
        // A transient attach failure must not poison the Surface forever.
      } finally {
        if (record.debuggerReadyPromise === debuggerReady) record.debuggerReadyPromise = null;
      }
    }
    if (!record.contents.debugger.isAttached()) {
      await this.reconnectDebugger(record, "on-demand");
    }
    if (record.closed || record.contents.isDestroyed()) {
      throw staleSurfaceError("browser_surface_not_found");
    }
    if (!record.contents.debugger.isAttached()) {
      throw staleSurfaceError("browser_debugger_detached");
    }
  }

  private async attachDebugger(record: BrowserSurfaceRecord): Promise<void> {
    const debuggerApi = record.contents.debugger;
    if (!debuggerApi.isAttached()) debuggerApi.attach("1.3");
    if (!record.debuggerListenersInstalled) {
      record.debuggerListenersInstalled = true;
      debuggerApi.on("message", (_event, method, params, sessionId) => {
        if (record.closed) return;
        const eventParams = (params ?? {}) as Record<string, unknown>;
        if (method === "Target.attachedToTarget" && typeof eventParams.sessionId === "string") {
          record.cdpSessionIds.add(eventParams.sessionId);
        }
        if (method === "Target.detachedFromTarget" && typeof eventParams.sessionId === "string") {
          record.cdpSessionIds.delete(eventParams.sessionId);
        }
        this.#onEvent({
          type: "cdp_event",
          binding: this.binding(record),
          method,
          params: eventParams,
          ...(sessionId ? { sessionId } : {}),
        });
      });
      debuggerApi.on("detach", (_event, reason) => {
        if (record.closed) return;
        // 调试器是自动化通道，不是页面本身。短暂 detach 不能隐藏或 reload
        // 用户正在看的 Chromium 文档；仅后台重新 attach，页面继续保持可见。
        void this.reconnectDebugger(record, `debugger-detached:${reason}`).catch((error) => {
          if (!record.closed) {
            console.error("[BrowserSurfaceManager] Browser Surface 调试器重连失败", {
              surfaceId: record.surfaceId,
              reason,
              error: error instanceof Error ? error.message : String(error),
            });
          }
        });
      });
    }
  }

  private reconnectDebugger(record: BrowserSurfaceRecord, reason: string): Promise<void> {
    if (record.debuggerReadyPromise) return record.debuggerReadyPromise;
    const reconnect = this.enqueueCdp(record, async () => {
      if (record.closed || record.contents.isDestroyed()) return;
      await this.attachDebugger(record);
      record.cdpSessionIds.clear();
      record.cursorExecutionContextId = null;
      if (record.primary) this.#onEvent({ type: "primary_changed", binding: this.binding(record) });
      void reason;
    });
    record.debuggerReadyPromise = reconnect;
    void reconnect.finally(() => {
      if (record.debuggerReadyPromise === reconnect) record.debuggerReadyPromise = null;
    });
    return reconnect;
  }

  private invalidateAndRecover(record: BrowserSurfaceRecord, reason: string): void {
    if (record.recoveryPromise || record.closed || record.contents.isDestroyed()) return;
    record.priming = true;
    this.unmountSurface(record, this.#windows.get(record.windowId));
    record.surfaceRevision = this.#surfaces.nextRevision(record.tabId);
    const recovery = this.enqueueCdp(record, () => this.recover(record, reason));
    record.recoveryPromise = recovery;
    record.debuggerReadyPromise = recovery;
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
      await this.attachDebugger(record);
      record.priming = false;
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

  private async applyViewport(
    record: BrowserSurfaceRecord,
    track?: (promise: Promise<unknown>) => void,
  ): Promise<void> {
    if (record.closed || !record.contents.debugger.isAttached()) return;
    if (record.contents.isLoadingMainFrame()) return;
    if (record.viewport.mode === "auto") {
      await sendCdpCommandWithTimeout(
        record.contents,
        "Emulation.clearDeviceMetricsOverride",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        undefined,
        track,
      );
      await sendCdpCommandWithTimeout(
        record.contents,
        "Emulation.setTouchEmulationEnabled",
        { enabled: false },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        undefined,
        track,
      );
      record.viewportApplied = true;
      return;
    }
    const viewport = record.viewport;
    if (viewport.mode !== "fixed") return;
    const width = Math.max(320, Math.round(viewport.width));
    const height = Math.max(240, Math.round(viewport.height));
    const mobile = viewport.device_type === "mobile";
    await sendCdpCommandWithTimeout(
      record.contents,
      "Emulation.setDeviceMetricsOverride",
      {
        width,
        height,
        deviceScaleFactor: viewport.device_scale_factor_millis / 1_000,
        mobile,
        screenWidth: width,
        screenHeight: height,
        screenOrientation: {
          type: width > height ? "landscapePrimary" : "portraitPrimary",
          angle: width > height ? 90 : 0,
        },
      },
      DEFAULT_CDP_COMMAND_TIMEOUT_MS,
      undefined,
      track,
    );
    await sendCdpCommandWithTimeout(
      record.contents,
      "Emulation.setTouchEmulationEnabled",
      {
        enabled: mobile,
        maxTouchPoints: mobile ? 5 : 1,
      },
      DEFAULT_CDP_COMMAND_TIMEOUT_MS,
      undefined,
      track,
    );
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
      await this.enqueueCdp(record, ({ track }) => this.applyViewport(record, track));
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
    const update = async ({ track }: SurfaceLaneContext) => {
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
            undefined,
            track,
          ) as {
            frameTree?: { frame?: { id?: string } };
          };
          const frameId = frameTree.frameTree?.frame?.id;
          if (!frameId) return;
          const world = await sendCdpCommandWithTimeout(record.contents, "Page.createIsolatedWorld", {
            frameId,
            worldName: "magi-agent-cursor",
            grantUniveralAccess: false,
          }, CURSOR_CDP_COMMAND_TIMEOUT_MS, undefined, track) as { executionContextId?: number };
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
        }, CURSOR_CDP_COMMAND_TIMEOUT_MS, undefined, track);
      } catch {
        // 导航会清理 isolated world；did-finish-load 会按最新状态重建它。
        record.cursorExecutionContextId = null;
      }
    };
    await this.enqueueCdp(record, update);
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
    const wasClosed = record.closed;
    record.closed = true;
    if (!wasClosed && !record.contents.isDestroyed() && record.contents.debugger.isAttached()) {
      try {
        record.contents.debugger.detach();
      } catch {
        // WebContents 正在销毁时 detach 可能同步抛错，关闭流程不能因此中断。
      }
    }
    const window = this.#windows.get(record.windowId);
    this.detachSurface(record, window);
    if (!wasClosed && !record.contents.isDestroyed()) record.contents.close();
    this.removeRecordIndexes(record);
    if (!wasClosed && promoteFallback) this.promoteFallback(record.tabId);
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
  sessionId?: string,
  track?: (promise: Promise<unknown>) => void,
): Promise<unknown> {
  let command: Promise<unknown>;
  try {
    command = contents.debugger.sendCommand(method, params, sessionId);
  } catch (error) {
    throw error;
  }
  // 不要把原始 CDP Promise 放进 Surface lane。Chromium 在页面销毁、视图
  // 解绑或某些截图参数不被当前宿主接受时，sendCommand 可能永远不结算；
  // 如果 lane 继续等待它，单次超时就会永久阻塞后续快照、输入和标记。
  const timed = withTimeout(command, timeoutMs, method);
  track?.(timed.then(() => undefined, () => undefined));
  return timed;
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
  handleBeforeUnload?: "accept" | "dismiss",
): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      cleanup();
      try {
        contents.stop();
      } catch {
        // WebContents 销毁竞态下 stop() 可能同步失败，超时状态仍已收敛。
      }
      reject(new Error("browser_navigation_timeout"));
    }, clampNavigationTimeout(timeoutMs));
    timer.unref();
    const cleanup = () => {
      clearTimeout(timer);
      contents.off("did-stop-loading", done);
      contents.off("did-fail-load", failed);
      if (allowBeforeUnload) contents.off("will-prevent-unload", allowBeforeUnload);
    };
    const finish = (error?: Error) => {
      if (settled) return;
      settled = true;
      cleanup();
      if (error) reject(error);
      else resolve();
    };
    const allowBeforeUnload = handleBeforeUnload === "accept"
      ? (event: Electron.Event) => event.preventDefault()
      : null;
    const failed = (_event: Electron.Event, errorCode: number, errorDescription: string, _validatedURL: string, isMainFrame: boolean) => {
      if (!isMainFrame || errorCode === -3) return;
      if (settled) return;
      settled = true;
      cleanup();
      reject(new Error(`browser_navigation_failed:${errorCode}:${errorDescription}`));
    };
    const done = () => finish();
    if (allowBeforeUnload) contents.on("will-prevent-unload", allowBeforeUnload);
    contents.once("did-stop-loading", done);
    contents.once("did-fail-load", failed);
    try {
      if (ignoreCache) contents.reloadIgnoringCache();
      else contents.reload();
    } catch (error) {
      finish(error instanceof Error ? error : new Error(String(error)));
    }
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

function downloadBytes(item: Electron.DownloadItem): number {
  const total = item.getTotalBytes();
  return total > 0 ? total : item.getReceivedBytes();
}

function safeDownloadFilename(value: string): string {
  const filename = basename(value).replace(/[\\/:*?"<>|\u0000-\u001f]/gu, "_").trim();
  return filename || "download";
}

function safeDownloadSegment(value: string): string {
  const segment = value.replace(/[^A-Za-z0-9._-]/gu, "_").replace(/^\.+$/u, "_");
  return segment || "session";
}
