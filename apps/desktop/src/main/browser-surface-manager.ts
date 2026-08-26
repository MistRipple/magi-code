import { randomUUID } from "node:crypto";
import { basename, dirname, join } from "node:path";
import { existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import {
  BaseWindow,
  WebContentsView,
  webContents,
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

export interface BrowserInspectedNodeContext {
  backend_node_id: number;
  node_id: number | null;
  frame_id: string | null;
  node_type: number;
  node_name: string;
  local_name: string;
  node_value: string;
  child_node_count: number | null;
  document_url: string | null;
  attributes: Record<string, string>;
  outer_html: string;
  outer_html_truncated: boolean;
  bounds: { x: number; y: number; width: number; height: number } | null;
  page_url: string;
  page_title: string;
}

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
    }
  | {
      type: "node_inspected";
      binding: BrowserSurfaceBinding;
      node: BrowserInspectedNodeContext;
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
  dialogBridgeInstalled: boolean;
  cdpSessionIds: Set<string>;
  debuggerReadyPromise: Promise<void> | null;
  recoveryPromise: Promise<void> | null;
  loadPromise: Promise<void> | null;
  inspectGeneration: number;
  inspectActive: boolean;
  inspectStartPromise: Promise<void> | null;
  inspectResourcesEnabled: boolean;
}

interface SurfaceLaneContext {
  track(promise: Promise<unknown>): void;
}

const ALLOWED_NAVIGATION_PROTOCOLS = new Set(["http:", "https:", "about:"]);
const BLOCKED_HOSTS = new Set(["169.254.169.254", "metadata.google.internal"]);
// 固定资产只用于隔离世界中的可视化指针，不读取或修改页面的光标样式。
// 指针沿用 Codex 的低存在感视觉：灰紫色主体、柔和浅色描边和圆润转角，
// 让它在浅色页面上清晰可见，但不会像黑色系统光标一样抢占内容注意力。
const AGENT_CURSOR_SVG = "<svg xmlns='http://www.w3.org/2000/svg' width='26' height='26' viewBox='0 0 26 26'><path d='M5.05 3.55c-.77-.28-1.53.36-1.33 1.18l4.19 16.55c.28 1.1 1.74 1.34 2.28.32l3.7-6.95c.11-.21.28-.39.48-.51l6.35-3.61c1.16-.66.98-2.38-.3-2.79L5.05 3.55Z' fill='#4e4d66' stroke='#d5d5d9' stroke-width='1.9' stroke-linecap='round' stroke-linejoin='round'/></svg>";
const AGENT_CURSOR_ASSET = `data:image/svg+xml,${encodeURIComponent(AGENT_CURSOR_SVG)}`;
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
  "Emulation.setTouchEmulationEnabled",
  "Emulation.setEmulatedMedia",
  "Emulation.setGeolocationOverride",
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
  "Runtime.addBinding",
  "Page.removeScriptToEvaluateOnNewDocument",
  "Performance.enable",
  "Performance.getMetrics",
  "Overlay.hideHighlight",
  "Overlay.highlightNode",
  "Overlay.enable",
  "Overlay.disable",
  "Overlay.setInspectMode",
  "Runtime.evaluate",
  "Runtime.enable",
  "Runtime.disable",
  "Runtime.getHeapUsage",
  "Network.enable",
  "Tracing.end",
  "Tracing.start",
  "Tracing.recordClockSyncMarker",
]);

const DEFAULT_CDP_COMMAND_TIMEOUT_MS = 15_000;
const SCREENSHOT_CDP_COMMAND_TIMEOUT_MS = 10_000;
const SCREENSHOT_READINESS_TIMEOUT_MS = 5_000;
const CURSOR_CDP_COMMAND_TIMEOUT_MS = 5_000;
const DEFAULT_NAVIGATION_TIMEOUT_MS = 120_000;
const MAX_INSPECTED_OUTER_HTML_LENGTH = 64 * 1024;
const HIDDEN_AUTO_VIEWPORT = { width: 1280, height: 720, deviceScaleFactor: 1, mobile: false } as const;
const INSPECT_HIGHLIGHT_CONFIG = {
  showInfo: false,
  contentColor: { r: 66, g: 133, b: 244, a: 0.18 },
  paddingColor: { r: 66, g: 133, b: 244, a: 0.12 },
  borderColor: { r: 66, g: 133, b: 244, a: 0.9 },
  marginColor: { r: 66, g: 133, b: 244, a: 0.08 },
};
const DIALOG_BRIDGE_BINDING = "__magiBrowserDialog";
const DIALOG_BRIDGE_SCRIPT = String.raw`(() => {
  if (globalThis.__magiBrowserDialogInstalled) return;
  let sequence = 0;
  let active = null;
  const notify = (event) => {
    try {
      if (typeof globalThis.${DIALOG_BRIDGE_BINDING} === 'function') {
        globalThis.${DIALOG_BRIDGE_BINDING}(JSON.stringify(event));
      }
    } catch {}
  };
  const open = (type, message, defaultPrompt) => {
    const dialog = {
      id: 'magi-dialog-' + (++sequence),
      type,
      message: String(message ?? ''),
      defaultPrompt: defaultPrompt == null ? null : String(defaultPrompt),
    };
    active = dialog;
    notify({ event: 'opening', ...dialog });
    return dialog;
  };
  globalThis.__magiBrowserDialogResolve = (input) => {
    if (!active) return { handled: false };
    const dialog = active;
    active = null;
    notify({
      event: 'closed',
      id: dialog.id,
      action: String(input?.action ?? 'dismiss'),
      promptText: typeof input?.promptText === 'string' ? input.promptText : null,
    });
    return { handled: true };
  };
  globalThis.alert = (message) => { open('alert', message, null); };
  globalThis.confirm = (message) => { open('confirm', message, null); return true; };
  globalThis.prompt = (message, defaultPrompt = '') => {
    const dialog = open('prompt', message, defaultPrompt);
    return dialog.defaultPrompt ?? '';
  };
  globalThis.__magiBrowserDialogInstalled = true;
})();`;
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
    // 首次文档导航前先完成调试器对话框桥接。原生 JavaScript 对话框会
    // 阻塞 Electron Main 的事件循环，导致 Host 心跳和后续 accept/dismiss
    // 请求一起失效；桥接必须早于 loadURL 注入才能覆盖页面首个脚本。
    if (created && record.debuggerReadyPromise) await record.debuggerReadyPromise;
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
        // 页面导航不应改变桌面当前输入归属。用户点击浏览器内容时由
        // Chromium 原生命中测试接管焦点；主 Renderer 获得焦点后，导航中
        // 的 did-finish-load 不能再把键盘输入抢回网页。
        focusOnNavigation: false,
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
      dialogBridgeInstalled: false,
      cdpSessionIds: new Set(),
      debuggerReadyPromise: null,
      recoveryPromise: null,
      loadPromise: null,
      inspectGeneration: 0,
      inspectActive: false,
      inspectStartPromise: null,
      inspectResourcesEnabled: false,
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
    const debuggerReady = this.enqueueCdp(record, async () => {
      // 新建 WebContents 在第一次 loadURL 前还没有 renderer document。
      // Chromium/Electron 在这个阶段允许 attach debugger，但 Runtime domain
      // 可能一直不响应；先完成一次受控的 about:blank 初始文档，再安装
      // Runtime binding 与 new-document 脚本，随后 materialize 才会导航到
      // 用户请求的 URL。这样既保留首个业务文档的桥接时机，也避免把
      // 首次 Runtime.enable 超时误报成浏览器不可用。
      await primeInitialDocument(record.contents);
      await this.attachDebugger(record);
    });
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

  /**
   * 在 App Renderer 接管焦点前，将当前获得焦点的浏览器 Surface 从原生
   * 命中树移除。BaseWindow 没有提供子 WebContents 的统一失焦接口，卸载
   * 当前 WebContentsView 是 macOS 真正释放网页键盘焦点的原生操作。
   */
  blurWindow(windowId: string): boolean {
    const window = this.#windows.get(windowId);
    const focused = webContents.getFocusedWebContents();
    let changed = false;
    for (const record of this.#surfaces.values()) {
      if (
        record.windowId !== windowId
        || record.closed
        || !record.mounted
        || record.contents !== focused
      ) continue;
      this.unmountSurface(record, window);
      changed = true;
    }
    return changed;
  }

  /** 在不重建 WebContents 的前提下恢复 blurWindow 临时卸载的 Surface。 */
  restoreWindow(windowId: string): void {
    const window = this.#windows.get(windowId);
    for (const record of this.#surfaces.values()) {
      if (record.windowId !== windowId || record.closed || !record.slotVisible) continue;
      this.applySlot(record, record.slotBounds, window);
    }
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

  recordForBinding(
    binding: BrowserSurfaceBinding,
    options: { allowNavigationAdvance?: boolean } = {},
  ): WebContents {
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
      || (
        options.allowNavigationAdvance !== true
          ? binding.navigation_revision !== record.navigationRevision
          : binding.navigation_revision > record.navigationRevision
      )
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
    options: { allowNavigationAdvance?: boolean } = {},
  ): Promise<unknown> {
    if (!ALLOWED_WORKER_CDP_METHODS.has(method)) {
      throw new Error(`browser_cdp_method_denied:${method}`);
    }
    const record = this.requireRecord(binding.surface_id);
    // JavaScript 对话框会阻塞触发它的 Input.dispatchMouseEvent。对话框处理
    // 命令必须能够穿过当前 Surface lane 直接送达 Chromium，否则会形成：
    // Input 等待对话框关闭、dialog 又排在 Input 后面，最终 Rust Host 请求超时
    // 并把本来已经成功的点击错误收敛为 browser_host_disconnected。
    const isDialogCommand = method === "Page.handleJavaScriptDialog";
    if (!isDialogCommand) {
      await this.waitForDebugger(record);
    } else if (!record.contents.debugger.isAttached()) {
      await this.waitForDebugger(record);
    }
    // 输入事件属于一个完整的用户动作。Enter、点击或输入事件可能在
    // keyDown/mousePressed 之后立即推进 navigation_revision；后续的
    // keyUp/mouseReleased 仍然必须送到同一个 WebContents，而不是被页面
    // 自身导航误判成 Surface 已失效。Surface/Tab/桌面代次仍由上面的
    // 完整身份校验严格保护。
    const contents = this.recordForBinding(binding, {
      allowNavigationAdvance: options.allowNavigationAdvance === true || method.startsWith("Input."),
    });
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
    if (isDialogCommand) {
      return sendCdpCommandWithTimeout(
        contents,
        method,
        params,
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        sessionId,
      );
    }
    if (method === "Page.captureScreenshot") {
      // 激活 Browser Tab 的导航是非阻塞的，截图可能紧跟在 Surface
      // 物化之后到达。先给 Chromium 当前文档一个有限的稳定窗口，避免
      // 在首帧/导航切换阶段直接让 Page.captureScreenshot 卡满超时；
      // 超时后仍继续截图，不能把慢站点变成永远不可操作。
      await this.waitForScreenshotReadiness(record);
      // Electron 原生捕获覆盖普通视口、元素范围和全页范围，避免
      // Page.captureScreenshot 在 WebContentsView 尚未取得 compositor
      // frame（尤其是后台 Surface）时阻塞 CDP lane。WebP 仍保留
      // Chromium CDP 的原生编码语义。
      if (params.format === "webp") {
        return this.enqueueCdp(record, ({ track }) => sendCdpCommandWithTimeout(
          contents,
          method,
          // WebP 需要 Chromium compositor 的编码路径；Electron 的
          // nativeImage 没有 WebP 编码接口，因此明确使用 surface 捕获。
          { ...params, fromSurface: true },
          SCREENSHOT_CDP_COMMAND_TIMEOUT_MS,
          sessionId,
          track,
        ));
      }
      return this.capturePageScreenshot(record, params);
    }
    const injectsInput = method.startsWith("Input.");
    if (injectsInput) record.automationInputDepth += 1;
    try {
      const result = await this.enqueueCdp(record, ({ track }) => sendCdpCommandWithTimeout(
        contents,
        method,
        params,
        method === "Page.captureScreenshot" ? SCREENSHOT_CDP_COMMAND_TIMEOUT_MS : DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        sessionId,
        track,
      ));
      if (injectsInput && record.agentControlled) {
        const input = params as { type?: string; x?: number; y?: number };
        // 一个完整点击包含 pressed/released 两个事件，但视觉反馈只在
        // pressed 时触发一次；released 仍更新指针位置，避免一次点击出现
        // 两个连续波纹。
        const action = method === "Input.insertText"
          ? "type"
          : input.type === "mousePressed"
            ? "click"
            : input.type === "mouseWheel" ? "scroll" : "move";
        // 等待代理指针完成注入再返回输入命令，保证调用方紧接着截图时
        // 能看到与刚刚执行的动作一致的指针位置和点击反馈。
        await this.setAgentCursor(
          record,
          true,
          typeof input.x === "number" ? input.x : record.cursor.x,
          typeof input.y === "number" ? input.y : record.cursor.y,
          action,
        ).catch(() => undefined);
      }
      return result;
    } finally {
      if (injectsInput) record.automationInputDepth = Math.max(0, record.automationInputDepth - 1);
    }
  }

  private async capturePageScreenshot(
    record: BrowserSurfaceRecord,
    params: Record<string, unknown>,
  ): Promise<{ data: string }> {
    if (record.closed || record.contents.isDestroyed()) {
      throw staleSurfaceError("browser_surface_not_found");
    }
    const format = params.format === "jpeg" ? "jpeg" : "png";
    const rect = capturePageRect(record, params);
    // WebContentsView 在右栏切换/恢复的瞬间，view.getBounds() 可能暂时是
    // 0×0。capturePage 仍必须使用页面的逻辑视口尺寸，否则 Electron 会
    // 返回 1×1 空白图片；capturePageRect 会在物理尺寸不可用时使用固定
    // viewport 或隐藏 Surface 的标准视口作为确定性边界。
    const image = await record.contents.capturePage(rect, { stayHidden: true });
    const bytes = format === "jpeg"
      ? image.toJPEG(normalizeJpegQuality(params.quality))
      : image.toPNG();
    if (bytes.byteLength === 0) throw new Error("browser_screenshot_empty");
    return { data: bytes.toString("base64") };
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

  private async waitForScreenshotReadiness(record: BrowserSurfaceRecord): Promise<void> {
    if (record.closed || record.contents.isDestroyed() || !record.contents.isLoadingMainFrame()) {
      return;
    }
    const loading = record.loadPromise ?? new Promise<void>((resolve) => {
      let settled = false;
      const finish = () => {
        if (settled) return;
        settled = true;
        record.contents.off("did-stop-loading", finish);
        record.contents.off("did-fail-load", finish);
        resolve();
      };
      record.contents.once("did-stop-loading", finish);
      record.contents.once("did-fail-load", finish);
    });
    await Promise.race([
      loading.then(() => undefined, () => undefined),
      new Promise<void>((resolve) => {
        const timer = setTimeout(resolve, SCREENSHOT_READINESS_TIMEOUT_MS);
        timer.unref();
      }),
    ]);
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

  async startInspect(binding: BrowserSurfaceBinding): Promise<void> {
    const record = this.requireRecord(binding.surface_id);
    this.recordForBinding(binding);
    if (!this.isPrimary(binding) || !this.isRenderable(record)) {
      throw staleSurfaceError("browser_inspect_surface_inactive");
    }
    await this.waitForDebugger(record);
    this.recordForBinding(binding);
    if (!this.isPrimary(binding) || !this.isRenderable(record)) {
      throw staleSurfaceError("browser_inspect_surface_inactive");
    }
    if (record.inspectActive) {
      const pendingStart = record.inspectStartPromise;
      if (pendingStart) {
        await pendingStart;
        // stopInspect() or a lifecycle transition may have invalidated the
        // first start while this caller was waiting. Revalidate before
        // starting a new generation instead of reporting a false success.
        if (!record.inspectActive) {
          this.recordForBinding(binding);
          if (!this.isPrimary(binding) || !this.isRenderable(record)) {
            throw staleSurfaceError("browser_inspect_surface_inactive");
          }
        }
      }
      if (record.inspectActive) return;
    }

    const generation = ++record.inspectGeneration;
    record.inspectActive = true;
    const start = this.enqueueCdp(record, async ({ track }) => {
      if (!this.isInspectGenerationActive(record, generation)) return;
      await sendCdpCommandWithTimeout(
        record.contents,
        "Overlay.enable",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        undefined,
        track,
      );
      if (!this.isInspectGenerationActive(record, generation)) return;
      record.inspectResourcesEnabled = true;
      await sendCdpCommandWithTimeout(
        record.contents,
        "DOM.enable",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        undefined,
        track,
      );
      if (!this.isInspectGenerationActive(record, generation)) return;
      // Chromium 的 Overlay API 将“search for node”定义为 inspect mode，
      // 不是一个坐标命中或截图模拟。用户在真实 WebContents 中选中元素后，
      // 由 Overlay.inspectNodeRequested 返回该节点的 backendNodeId。
      await sendCdpCommandWithTimeout(
        record.contents,
        "Overlay.setInspectMode",
        {
          mode: "searchForNode",
          highlightConfig: INSPECT_HIGHLIGHT_CONFIG,
        },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        undefined,
        track,
      );
    });
    record.inspectStartPromise = start;
    try {
      await start;
      // 导航、Primary 切换或卸载可能在 CDP lane 等待期间取消了本次
      // inspect。不能把被取消的操作报告成成功，否则 Renderer/Host 会
      // 认为后续点击一定会产生 node_inspected 事件。
      this.recordForBinding(binding);
      if (!this.isPrimary(binding) || !this.isRenderable(record) || !record.inspectActive) {
        throw staleSurfaceError("browser_inspect_surface_inactive");
      }
    } catch (error) {
      if (record.inspectStartPromise === start) record.inspectStartPromise = null;
      if (record.inspectGeneration === generation) record.inspectActive = false;
      // Overlay.enable 可能已成功而后续命令失败。清理已建立的 CDP 资源，
      // 避免下一次检查继承半激活的 Overlay 状态。
      try {
        await this.stopInspectRecord(record);
      } catch (cleanupError) {
        if (!record.closed) {
          console.warn("[BrowserSurfaceManager] 节点检查失败后的 CDP 清理失败", {
            surfaceId: record.surfaceId,
            error: cleanupError instanceof Error ? cleanupError.message : String(cleanupError),
          });
        }
      }
      throw error;
    } finally {
      if (record.inspectStartPromise === start) record.inspectStartPromise = null;
    }
  }

  async stopInspect(binding: BrowserSurfaceBinding): Promise<void> {
    const record = this.requireRecord(binding.surface_id);
    this.recordForBinding(binding);
    await this.stopInspectRecord(record);
  }

  private isInspectGenerationActive(record: BrowserSurfaceRecord, generation: number): boolean {
    return !record.closed
      && !record.contents.isDestroyed()
      && record.primary
      && this.#surfaces.isPrimary(record)
      && record.inspectActive
      && record.inspectGeneration === generation
      && record.contents.debugger.isAttached();
  }

  private stopInspectForLifecycle(record: BrowserSurfaceRecord, reason: string): void {
    void this.stopInspectRecord(record).catch((error) => {
      if (!record.closed) {
        console.warn("[BrowserSurfaceManager] 节点检查清理失败", {
          surfaceId: record.surfaceId,
          reason,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    });
  }

  private stopInspectRecord(record: BrowserSurfaceRecord): Promise<void> {
    const hasInspectWork = record.inspectActive
      || record.inspectStartPromise !== null
      || record.inspectResourcesEnabled;
    if (!hasInspectWork) return Promise.resolve();

    const generation = ++record.inspectGeneration;
    record.inspectActive = false;
    const pendingStart = record.inspectStartPromise;
    return (async () => {
      if (pendingStart) await pendingStart.catch(() => undefined);
      if (record.inspectGeneration !== generation || record.inspectActive) return;
      if (record.closed || record.contents.isDestroyed() || !record.contents.debugger.isAttached()) {
        record.inspectResourcesEnabled = false;
        return;
      }
      // stopInspect() can race with a start that was cancelled before
      // Overlay.enable completed. In that case there is no CDP domain to
      // tear down; sending disable commands would turn an idempotent stop
      // into a spurious "domain not enabled" failure.
      if (!record.inspectResourcesEnabled) return;
      await this.enqueueCdp(record, async ({ track }) => {
        if (record.closed || record.contents.isDestroyed() || record.inspectGeneration !== generation || record.inspectActive) {
          return;
        }
        if (!record.inspectResourcesEnabled) return;
        let firstError: unknown = null;
        for (const [method, params] of [
          // 当前 Chromium 版本即使 mode=none 也要求传入 highlightConfig；
          // 保持完整参数才能让 stop、导航和 Surface 销毁清理真正幂等。
          ["Overlay.setInspectMode", { mode: "none", highlightConfig: INSPECT_HIGHLIGHT_CONFIG }],
          ["Overlay.hideHighlight", {}],
          ["Overlay.disable", {}],
        ] as const) {
          try {
            await sendCdpCommandWithTimeout(
              record.contents,
              method,
              params,
              DEFAULT_CDP_COMMAND_TIMEOUT_MS,
              undefined,
              track,
            );
          } catch (error) {
            firstError ??= error;
          }
        }
        if (firstError) throw firstError;
        record.inspectResourcesEnabled = false;
      });
    })();
  }

  private initialAgentCursorPosition(record: BrowserSurfaceRecord): { x: number; y: number } {
    const viewBounds = record.view.getBounds();
    const width = viewBounds.width > 0
      ? viewBounds.width
      : record.slotBounds && record.slotBounds.width > 0
        ? record.slotBounds.width
        : record.viewport.mode === "fixed"
          ? record.viewport.width
          : 640;
    const height = viewBounds.height > 0
      ? viewBounds.height
      : record.slotBounds && record.slotBounds.height > 0
        ? record.slotBounds.height
        : record.viewport.mode === "fixed"
          ? record.viewport.height
          : 480;
    const inset = 18;
    const clamp = (value: number, limit: number): number => Math.max(
      0,
      Math.min(Math.max(0, limit - inset), value),
    );
    return {
      x: clamp(Math.round(width / 2), width),
      y: clamp(Math.round(height / 2), height),
    };
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
    const wasSlotVisible = record.slotVisible;
    record.slotBounds = bounds ? { ...bounds } : null;
    record.slotVisible = bounds !== null;
    if (record.viewport.mode === "auto" && wasSlotVisible !== record.slotVisible) {
      record.viewportApplied = false;
      this.scheduleViewportApply(record);
    }
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
    // Browser Host 是 WindowManager 按当前右栏内容槽设置边界的独立
    // 原生容器。WebContentsView 只在 Host 的局部坐标中占满内容槽，
    // 不再把窗口坐标直接交给 WebContentsView。
    const localBounds = { x: 0, y: 0, width: bounds.width, height: bounds.height };
    if (!sameBounds(record.view.getBounds(), localBounds)) record.view.setBounds(localBounds);
    // 页面加载和调试器初始化是 Surface 内部状态，不能阻塞真实浏览器
    // 视图进入内容槽。Chromium 自行展示当前文档及加载过程；只有失败页
    // 才隐藏，避免激活流程变成“正在连接浏览器”的空白等待层。
    // 导航过程由 Chromium 自己绘制。只有 Surface 生命周期失败才解绑，
    // 页面导航失败仍保留真实 WebContentsView，避免右栏黑屏或空槽。
    record.view.setVisible(true);
    // 内容槽只管理原生 View 的物理承载范围。它的尺寸变化不能重新提交
    // 当前 Tab 的 viewport：auto 由 WebContentsView 的真实尺寸自然驱动，
    // fixed 则保持用户选择的 CSS viewport，避免拖动右栏触发页面重排、闪烁
    // 或把物理槽尺寸写入设备仿真状态。
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
      await this.installDialogBridgeInCurrentDocument(record);
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
    this.stopInspectForLifecycle(record, "surface-unmounted");
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

  private handleInspectNodeRequested(
    record: BrowserSurfaceRecord,
    params: Record<string, unknown>,
    sessionId: string,
  ): void {
    if (
      record.closed
      || !record.primary
      || !this.#surfaces.isPrimary(record)
      || !record.inspectActive
      || record.contents.isDestroyed()
      || !record.contents.debugger.isAttached()
    ) return;
    const backendNodeId = integerOrNull(params.backendNodeId);
    if (backendNodeId === null) return;
    const generation = record.inspectGeneration;
    const commandSessionId = sessionId || undefined;
    void this.enqueueCdp(record, async ({ track }) => {
      if (!this.isInspectGenerationActive(record, generation)) return;
      const description = await sendCdpCommandWithTimeout(
        record.contents,
        "DOM.describeNode",
        { backendNodeId, depth: 0, pierce: true },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        commandSessionId,
        track,
      ) as { node?: CdpNodeDescription };
      const node = description.node;
      if (!node || !this.isInspectGenerationActive(record, generation)) return;

      // DOM.Node.frameId 只在 FrameOwner 节点上返回。普通页面节点需要
      // 从同一 CDP target 的真实 frame tree 解析根 frame，不能因为该字段
      // 在 Chromium 中按协议省略，就把合法的按钮、链接等节点全部丢弃。
      const frameId = await this.resolveInspectFrameId(
        record,
        node.frameId,
        backendNodeId,
        commandSessionId,
        generation,
        track,
      );
      if (!frameId || !this.isInspectGenerationActive(record, generation)) return;

      let attributeList = Array.isArray(node.attributes) ? node.attributes : [];
      const nodeId = integerOrNull(node.nodeId);
      if (nodeId !== null && this.isInspectGenerationActive(record, generation)) {
        try {
          const attributes = await sendCdpCommandWithTimeout(
            record.contents,
            "DOM.getAttributes",
            { nodeId },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            commandSessionId,
            track,
          ) as { attributes?: unknown };
          if (Array.isArray(attributes.attributes)) attributeList = attributes.attributes;
        } catch {
          // describeNode 已经携带属性时，单独的 getAttributes 失败不应丢弃
          // 本次真实节点选择。页面导航竞态由 generation 校验负责收口。
        }
      }

      let outerHtml = "";
      if (this.isInspectGenerationActive(record, generation)) {
        try {
          const outer = await sendCdpCommandWithTimeout(
            record.contents,
            "DOM.getOuterHTML",
            { backendNodeId },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            commandSessionId,
            track,
          ) as { outerHTML?: unknown };
          if (typeof outer.outerHTML === "string") outerHtml = outer.outerHTML;
        } catch {
          // 节点仍然有效时，允许调用方使用结构化描述和属性继续处理。
        }
      }

      let bounds: { x: number; y: number; width: number; height: number } | null = null;
      if (this.isInspectGenerationActive(record, generation)) {
        try {
          const box = await sendCdpCommandWithTimeout(
            record.contents,
            "DOM.getBoxModel",
            { backendNodeId },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            commandSessionId,
            track,
          ) as CdpBoxModelResponse;
          bounds = quadBounds(box.model?.border);
        } catch {
          // 文本节点、不可布局节点和导航中的节点可能没有 box model。
        }
      }

      if (!this.isInspectGenerationActive(record, generation)) return;
      const pageUrl = record.contents.getURL() || "about:blank";
      this.#onEvent({
        type: "node_inspected",
        binding: this.binding(record),
        node: {
          backend_node_id: backendNodeId,
          node_id: nodeId,
          frame_id: frameId,
          node_type: finiteInteger(node.nodeType, 0),
          node_name: typeof node.nodeName === "string" ? node.nodeName : "",
          local_name: typeof node.localName === "string" ? node.localName : "",
          node_value: typeof node.nodeValue === "string" ? node.nodeValue : "",
          child_node_count: integerOrNull(node.childNodeCount),
          document_url: typeof node.documentURL === "string" ? node.documentURL : null,
          attributes: parseNodeAttributes(attributeList),
          ...truncateOuterHtml(outerHtml),
          bounds,
          page_url: pageUrl,
          page_title: record.contents.getTitle() || "",
        },
      });
    }).catch((error) => {
      if (!record.closed && record.inspectActive && record.inspectGeneration === generation) {
        console.warn("[BrowserSurfaceManager] 真实 DOM 节点采集失败", {
          surfaceId: record.surfaceId,
          backendNodeId,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    });
  }

  private async resolveInspectFrameId(
    record: BrowserSurfaceRecord,
    nodeFrameId: unknown,
    backendNodeId: number,
    sessionId: string | undefined,
    generation: number,
    track: (promise: Promise<unknown>) => void,
  ): Promise<string | null> {
    if (typeof nodeFrameId === "string" && nodeFrameId.length > 0) return nodeFrameId;
    if (!this.isInspectGenerationActive(record, generation)) return null;
    const objectGroup = "magi-inspect-frame";
    try {
      // 普通 DOM.Node 不带 frameId；通过真实 DOM 后端节点解析出页面对象，
      // 再读取它的 ownerDocument.defaultView.frameElement，可以得到同一
      // 文档所属的 iframe。整个过程仍在 Chromium CDP 内完成，只读取 DOM
      // 关联的 frameElement，不执行页面业务代码、不依赖坐标，也不会把
      // 主 frame 猜作子 frame。
      const resolved = await sendCdpCommandWithTimeout(
        record.contents,
        "DOM.resolveNode",
        { backendNodeId, objectGroup },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        sessionId,
        track,
      ) as { object?: { objectId?: unknown } };
      const objectId = resolved.object?.objectId;
      if (typeof objectId === "string" && objectId.length > 0 && this.isInspectGenerationActive(record, generation)) {
        const frameElement = await sendCdpCommandWithTimeout(
          record.contents,
          "Runtime.callFunctionOn",
          {
            objectId,
            objectGroup,
            functionDeclaration: "function () { return this.ownerDocument?.defaultView?.frameElement || null; }",
            returnByValue: false,
            awaitPromise: false,
          },
          DEFAULT_CDP_COMMAND_TIMEOUT_MS,
          sessionId,
          track,
        ) as { result?: { objectId?: unknown; subtype?: unknown; } };
        const frameElementObjectId = frameElement.result?.objectId;
        if (typeof frameElementObjectId === "string" && frameElementObjectId.length > 0) {
          const describedFrameElement = await sendCdpCommandWithTimeout(
            record.contents,
            "DOM.describeNode",
            { objectId: frameElementObjectId, depth: 0, pierce: false },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            sessionId,
            track,
          ) as { node?: CdpNodeDescription };
          const frameId = describedFrameElement.node?.frameId;
          if (typeof frameId === "string" && frameId.length > 0) return frameId;
        }
      }

      if (!this.isInspectGenerationActive(record, generation)) return null;
      const response = await sendCdpCommandWithTimeout(
        record.contents,
        "Page.getFrameTree",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        sessionId,
        track,
      ) as {
        frameTree?: { frame?: { id?: unknown } };
      };
      const frameId = response.frameTree?.frame?.id;
      return typeof frameId === "string" && frameId.length > 0 ? frameId : null;
    } catch {
      // Frame tree 读取失败通常意味着导航或 target detach 已经开始；
      // generation 校验会丢弃这次选择，下一次真实选择会重新采集。
      return null;
    } finally {
      // DOM.resolveNode/Runtime.callFunctionOn 会创建临时远程对象。无论
      // frame 解析成功、失败还是导航竞态，都必须释放对象组，避免每次
      // 选中节点都把远程对象留在 Chromium heap 中。
      if (!record.contents.isDestroyed() && record.contents.debugger.isAttached()) {
        await sendCdpCommandWithTimeout(
          record.contents,
          "Runtime.releaseObjectGroup",
          { objectGroup },
          DEFAULT_CDP_COMMAND_TIMEOUT_MS,
          sessionId,
          track,
        ).catch(() => undefined);
      }
    }
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
      this.stopInspectForLifecycle(record, "navigation");
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
      // Electron 的 contextIsolation 会让 CDP 注入的 new-document 脚本与
      // 自动化执行世界拥有不同的全局对象；在真实文档完成后，再通过
      // executeJavaScript 在页面主世界收敛一次同一桥接脚本，确保页面内
      // 的 window.alert/confirm/prompt 被替换，而不是落回原生阻塞对话框。
      void this.installDialogBridgeInCurrentDocument(record).catch((error) => {
        if (!record.closed) {
          console.warn("[BrowserSurfaceManager] 当前文档对话框桥接安装失败", {
            surfaceId: record.surfaceId,
            error: error instanceof Error ? error.message : String(error),
          });
        }
      });
      if (record.closed || !record.agentControlled) return;
      void this.setAgentCursor(record, true, record.cursor.x, record.cursor.y, record.cursor.action)
        .catch(() => undefined);
    });
    webContents.on("before-input-event", (_event, input) => {
      if (
        record.closed
        || record.automationInputDepth > 0
        || !["rawKeyDown", "keyDown"].includes(input.type)
      ) return;
      this.promote(record.surfaceId);
      void this.setAgentCursor(record, false, null, null, null).catch(() => undefined);
      this.#onEvent({ type: "user_takeover", binding: this.binding(record) });
    });
    webContents.on("before-mouse-event", (_event, input) => {
      if (
        record.closed
        || record.automationInputDepth > 0
        || !["mouseMove", "mouseDown", "contextMenu", "mouseWheel"].includes(input.type)
      ) return;
      this.promote(record.surfaceId);
      void this.setAgentCursor(record, false, null, null, null).catch(() => undefined);
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
        if (method === "Page.javascriptDialogOpening" || method === "Page.javascriptDialogClosed") {
          console.info("[BrowserSurfaceManager] JavaScript dialog event", {
            surfaceId: record.surfaceId,
            tabId: record.tabId,
            method,
            sessionId: sessionId ?? null,
            type: eventParams.type ?? null,
            message: eventParams.message ?? null,
          });
        }
        if (method === "Target.attachedToTarget" && typeof eventParams.sessionId === "string") {
          record.cdpSessionIds.add(eventParams.sessionId);
        }
        if (method === "Target.detachedFromTarget" && typeof eventParams.sessionId === "string") {
          record.cdpSessionIds.delete(eventParams.sessionId);
        }
        if (method === "Overlay.inspectNodeRequested") {
          this.handleInspectNodeRequested(record, eventParams, sessionId);
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
        this.stopInspectForLifecycle(record, "debugger-detached");
        console.warn("[BrowserSurfaceManager] Browser debugger detached", {
          surfaceId: record.surfaceId,
          tabId: record.tabId,
          reason,
        });
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
    if (!record.dialogBridgeInstalled) {
      // Runtime.addBinding 在 Electron 中需要先显式启用 Runtime domain；
      // 否则某些 Chromium 版本不会返回错误，而是让 sendCommand 一直挂起。
      await sendCdpCommandWithTimeout(
        record.contents,
        "Page.enable",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
      );
      await sendCdpCommandWithTimeout(
        record.contents,
        "Runtime.enable",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
      );
      await sendCdpCommandWithTimeout(
        record.contents,
        "Runtime.addBinding",
        { name: DIALOG_BRIDGE_BINDING },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
      );
      await sendCdpCommandWithTimeout(
        record.contents,
        "Page.addScriptToEvaluateOnNewDocument",
        { source: DIALOG_BRIDGE_SCRIPT },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
      );
      record.dialogBridgeInstalled = true;
      await record.contents.executeJavaScript(DIALOG_BRIDGE_SCRIPT, true);
    }
    // 内容槽可能先于调试器握手完成。固定视口在这段竞态中不能因为
    // 首次 CDP apply 提前返回而永久失效，握手完成后补交同一 Surface
    // 的最新 Tab 级 viewport 配置。后台 auto Surface 也必须收敛一次，
    // 否则 Chromium 会把未挂载 WebContentsView 的页面计算成 0×0。
    this.scheduleViewportApply(record);
  }

  private async installDialogBridgeInCurrentDocument(record: BrowserSurfaceRecord): Promise<void> {
    if (
      record.closed
      || record.contents.isDestroyed()
      || !record.contents.debugger.isAttached()
      || record.contents.isLoadingMainFrame()
    ) return;
    await this.enqueueCdp(record, async () => {
      if (record.closed || record.contents.isDestroyed()) return;
      await record.contents.executeJavaScript(DIALOG_BRIDGE_SCRIPT, true);
    });
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
    void reconnect.then(
      () => {
        if (record.debuggerReadyPromise === reconnect) record.debuggerReadyPromise = null;
      },
      () => {
        if (record.debuggerReadyPromise === reconnect) record.debuggerReadyPromise = null;
      },
    );
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
      const bounds = record.view.getBounds();
      const hasVisibleSlot = record.slotVisible && bounds.width > 0 && bounds.height > 0;
      await sendCdpCommandWithTimeout(
        record.contents,
        hasVisibleSlot ? "Emulation.clearDeviceMetricsOverride" : "Emulation.setDeviceMetricsOverride",
        hasVisibleSlot ? {} : HIDDEN_AUTO_VIEWPORT,
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
    record.viewportApplied = true;
  }

  private scheduleViewportApply(record: BrowserSurfaceRecord): void {
    record.viewportApplyDirty = true;
    if (record.viewportApplyPromise) return;
    const applyPromise = this.flushViewportApply(record);
    const settledPromise = applyPromise.finally(() => {
      record.viewportApplyPromise = null;
      if (!record.closed && record.viewportApplyDirty) this.scheduleViewportApply(record);
    });
    record.viewportApplyPromise = settledPromise;
    // 保留调用方对 CDP 失败的感知，同时为没有调用方等待的后台布局任务
    // 安装 rejection handler。Surface 关闭时的 target 销毁不应形成未处理
    // Promise，更不能污染同一 Host 的其他 Browser Tab。
    void settledPromise.catch((error) => {
      if (!record.closed) {
        console.warn("[BrowserSurfaceManager] Browser Surface 视口更新失败", {
          surfaceId: record.surfaceId,
          error: error instanceof Error ? error.message : String(error),
        });
      }
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
    const position = visible && (x === null || y === null)
      ? this.initialAgentCursorPosition(record)
      : { x, y };
    record.cursor = { visible, ...position, action };
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
            const state = ${JSON.stringify({ visible, ...position, action })};
            const cursorAsset = ${JSON.stringify(AGENT_CURSOR_ASSET)};
            const hostStyle = 'position:fixed;z-index:2147483647;pointer-events:none;width:26px;height:26px;overflow:visible;transform:translate(-3px,-3px);transition:left 60ms linear,top 60ms linear;display:none;will-change:left,top;';
            let host = document.querySelector('[data-magi-agent-cursor="true"]');
            if (!(host instanceof HTMLElement) || !host.isConnected) {
              host = document.createElement('div');
              host.dataset.magiAgentCursor = 'true';
              host.setAttribute('aria-hidden', 'true');
              (document.documentElement || document.body)?.append(host);
            }
            if (!(host instanceof HTMLElement)) return true;
            host.style.cssText = hostStyle;
            let image = host.querySelector('[data-magi-agent-cursor-image="true"], img');
            if (!(image instanceof HTMLImageElement)) {
              image = document.createElement('img');
              image.dataset.magiAgentCursorImage = 'true';
              image.alt = '';
              image.draggable = false;
              image.style.cssText = 'position:absolute;left:0;top:0;width:26px;height:26px;display:block;pointer-events:none;filter:drop-shadow(0 1px 1.5px rgba(11,13,28,.42));';
              host.append(image);
            }
            image.src = cursorAsset;
            const hasPosition = Number.isFinite(state.x) && Number.isFinite(state.y);
            const shouldShow = state.visible && hasPosition;
            host.style.display = shouldShow ? 'block' : 'none';
            if (shouldShow && state.x !== null && state.y !== null) {
              host.style.left = state.x + 'px';
              host.style.top = state.y + 'px';
            }
            const pulse = host.querySelector('[data-magi-agent-cursor-pulse]');
            if (!shouldShow) {
              pulse?.remove();
            } else if (state.action === 'click') {
              pulse?.remove();
              const clickPulse = document.createElement('span');
              clickPulse.dataset.magiAgentCursorPulse = 'true';
              clickPulse.style.cssText = 'position:absolute;left:-10px;top:-10px;width:28px;height:28px;box-sizing:border-box;border:1px solid rgba(213,213,217,.72);background:rgba(213,213,217,.10);border-radius:50%;pointer-events:none;opacity:.7;transform:scale(.42);transition:opacity 360ms cubic-bezier(.2,.7,.3,1),transform 360ms cubic-bezier(.2,.7,.3,1);';
              host.insertBefore(clickPulse, image);
              requestAnimationFrame(() => {
                clickPulse.style.opacity = '0';
                clickPulse.style.transform = 'scale(1.08)';
              });
              window.setTimeout(() => clickPulse.remove(), 400);
            }
            return true;
          })()`,
        }, CURSOR_CDP_COMMAND_TIMEOUT_MS, undefined, track);
        // Runtime.evaluate 返回只代表 DOM 已更新，不代表 Chromium 已经
        // 完成下一帧合成。等待一个渲染帧，保证紧跟在输入动作后的截图
        // 能看到代理指针和点击反馈，而不是捕获到更新前的 compositor frame。
        await new Promise<void>((resolve) => setTimeout(resolve, 16));
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
      // Inspect mode belongs to the current physical Primary Surface. A
      // promotion makes the previous surface stale even when it remains
      // mounted in another window, so terminate its Overlay/DOM domains
      // before allowing the new surface to receive inspect events.
      this.stopInspectForLifecycle(previous, "surface-not-primary");
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
    this.stopInspectForLifecycle(record, "surface-closed");
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

interface CdpNodeDescription {
  nodeId?: unknown;
  nodeType?: unknown;
  nodeName?: unknown;
  localName?: unknown;
  nodeValue?: unknown;
  childNodeCount?: unknown;
  frameId?: unknown;
  documentURL?: unknown;
  attributes?: unknown;
}

interface CdpBoxModelResponse {
  model?: { border?: unknown };
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

function capturePageRect(
  record: BrowserSurfaceRecord,
  params: Record<string, unknown>,
): Rectangle {
  const viewBounds = record.view.getBounds();
  const logicalBounds = record.viewport.mode === "fixed"
    ? { width: record.viewport.width, height: record.viewport.height }
    : HIDDEN_AUTO_VIEWPORT;
  const fallbackWidth = viewBounds.width > 0
    ? viewBounds.width
    : record.slotBounds && record.slotBounds.width > 0
      ? record.slotBounds.width
      : logicalBounds.width;
  const fallbackHeight = viewBounds.height > 0
    ? viewBounds.height
    : record.slotBounds && record.slotBounds.height > 0
      ? record.slotBounds.height
      : logicalBounds.height;
  const clip = params.clip;
  if (!clip || typeof clip !== "object") {
    return { x: 0, y: 0, width: Math.max(1, fallbackWidth), height: Math.max(1, fallbackHeight) };
  }
  const value = clip as Record<string, unknown>;
  const x = finiteNumber(value.x, 0);
  const y = finiteNumber(value.y, 0);
  const width = finiteNumber(value.width, fallbackWidth);
  const height = finiteNumber(value.height, fallbackHeight);
  const left = Math.max(0, Math.min(fallbackWidth - 1, Math.floor(x)));
  const top = Math.max(0, Math.min(fallbackHeight - 1, Math.floor(y)));
  return {
    x: left,
    y: top,
    width: Math.max(1, Math.min(fallbackWidth - left, Math.ceil(width))),
    height: Math.max(1, Math.min(fallbackHeight - top, Math.ceil(height))),
  };
}

function finiteNumber(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function finiteInteger(value: unknown, fallback: number): number {
  return Number.isSafeInteger(value) ? value as number : fallback;
}

function integerOrNull(value: unknown): number | null {
  return Number.isSafeInteger(value) && (value as number) >= 0 ? value as number : null;
}

function parseNodeAttributes(values: unknown[]): Record<string, string> {
  const attributes: Record<string, string> = {};
  for (let index = 0; index + 1 < values.length; index += 2) {
    const name = values[index];
    const value = values[index + 1];
    if (typeof name === "string" && typeof value === "string") attributes[name] = value;
  }
  return attributes;
}

function truncateOuterHtml(value: string): { outer_html: string; outer_html_truncated: boolean } {
  if (value.length <= MAX_INSPECTED_OUTER_HTML_LENGTH) {
    return { outer_html: value, outer_html_truncated: false };
  }
  return {
    outer_html: value.slice(0, MAX_INSPECTED_OUTER_HTML_LENGTH),
    outer_html_truncated: true,
  };
}

function quadBounds(value: unknown): { x: number; y: number; width: number; height: number } | null {
  if (!Array.isArray(value) || value.length < 8) return null;
  const coordinates = value.slice(0, 8);
  if (!coordinates.every((coordinate) => typeof coordinate === "number" && Number.isFinite(coordinate))) {
    return null;
  }
  const xValues = [coordinates[0], coordinates[2], coordinates[4], coordinates[6]] as number[];
  const yValues = [coordinates[1], coordinates[3], coordinates[5], coordinates[7]] as number[];
  const left = Math.min(...xValues);
  const top = Math.min(...yValues);
  return {
    x: left,
    y: top,
    width: Math.max(0, Math.max(...xValues) - left),
    height: Math.max(0, Math.max(...yValues) - top),
  };
}

function normalizeJpegQuality(value: unknown): number {
  const quality = finiteNumber(value, 90);
  return Math.max(0, Math.min(100, Math.round(quality)));
}

async function sendCdpCommandWithTimeout(
  contents: WebContents,
  method: string,
  params: Record<string, unknown>,
  timeoutMs: number,
  sessionId?: string,
  track?: (promise: Promise<unknown>) => void,
): Promise<unknown> {
  const startedAt = performance.now();
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
  try {
    const result = await timed;
    if (method === "Page.handleJavaScriptDialog" || method === "Input.dispatchMouseEvent") {
      console.info("[BrowserSurfaceManager] CDP command settled", {
        method,
        durationMs: Math.round(performance.now() - startedAt),
      });
    }
    return result;
  } catch (error) {
    console.warn("[BrowserSurfaceManager] CDP command failed", {
      method,
      durationMs: Math.round(performance.now() - startedAt),
      error: error instanceof Error ? error.message : String(error),
    });
    throw error;
  }
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

async function primeInitialDocument(contents: WebContents): Promise<void> {
  if (contents.isDestroyed()) throw new Error("browser_surface_not_found");
  await waitForNavigationEvent(
    contents,
    () => {
      // 新建 WebContents 的默认 URL 可能已经显示为 about:blank，但在
      // Chromium renderer 真正完成初始化前 Runtime domain 仍不可用。
      // 显式完成一次空白导航，确保后续 debugger.sendCommand 有稳定的
      // document/renderer 目标。
      void contents.loadURL("about:blank").catch(() => undefined);
    },
    DEFAULT_CDP_COMMAND_TIMEOUT_MS,
  );
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
