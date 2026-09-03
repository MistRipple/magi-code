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
import { browserDownloadRoot, clearBrowserDownloads } from "./browser-download-storage.js";
import {
  isAllowedBrowserChildTarget,
  normalizeOptionalDomNodeId,
  type BrowserControlUpdate,
  type BrowserLogicalViewport,
  type BrowserPageState,
  type BrowserNavigation,
  type BrowserSurfaceBinding,
} from "@magi/desktop-browser-contracts";
import { BrowserSurfaceRegistry } from "./browser-surface-registry.js";
import { matchesNavigationRevision } from "./browser-navigation-revision.js";

export interface BrowserInspectedNodeContext {
  browser_session_id: string;
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
  /**
   * 每个 Browser Tab 保留独立 WebContents；非当前 Surface 从窗口内容视图
   * 解绑但不销毁 WebContents，重新激活时再挂回，不丢失页面状态。
   */
  mounted: boolean;
  slotVisible: boolean;
  slotBounds: Rectangle | null;
  slotLeaseRevision: number;
  recoverySlot: { bounds: Rectangle; leaseRevision: number } | null;
  activationGeneration: number | null;
  priming: boolean;
  targetId: string;
  navigationRevision: number;
  navigationOperationId: number;
  navigationGeneration: number;
  navigationEventSequence: number;
  navigationOperation: NavigationOperation | null;
  navigationFailureReportedGeneration: number | null;
  viewport: BrowserLogicalViewport;
  primary: boolean;
  closed: boolean;
  automationInputDepth: number;
  agentControlled: boolean;
  cursor: { visible: boolean; x: number | null; y: number | null; action: string | null };
  cursorExecutionContextId: number | null;
  cdpLane: Promise<void>;
  viewportLifecycle: ViewportCommitLifecycle;
  debuggerListenersInstalled: boolean;
  debuggerMessageListener: DebuggerMessageListener | null;
  debuggerDetachListener: DebuggerDetachListener | null;
  debuggerSessionGeneration: number;
  debuggerSessionInitialized: boolean;
  dialogBridgeInstalled: boolean;
  nativeDialogOpeningWaiters: Set<() => void>;
  cdpSessionIds: Set<string>;
  /** 被明确拒绝的页面型 popup Target；其后续事件不能进入 Worker。 */
  blockedCdpSessionIds: Set<string>;
  debuggerReadyPromise: Promise<void> | null;
  debuggerReconnectTimer: NodeJS.Timeout | null;
  debuggerReconnectAttempt: number;
  recoveryPromise: Promise<void> | null;
  loadPromise: Promise<void> | null;
  inspectGeneration: number;
  inspectActive: boolean;
  /**
   * Inspect Mode 关闭后，Chromium 仍可能为同一次真实点击发送 mouseUp。
   * 这个状态只覆盖当前完整手势，不会把后续普通鼠标输入误认为 Inspect。
   */
  inspectGestureActive: boolean;
  inspectStartPromise: Promise<void> | null;
  inspectResourcesEnabled: boolean;
  inspectHoverPoint: { x: number; y: number } | null;
  inspectHoverPromise: Promise<void> | null;
  lifecycleEpoch: number;
  lifecycleAbort: AbortController;
}

interface NavigationFrameIdentity {
  processId: number;
  routingId: number;
}

type NavigationKind = "document" | "history" | "reload" | "in-page";

interface NavigationWaiter {
  resolve: () => void;
  reject: (error: Error) => void;
}

interface NavigationOperation {
  id: number;
  generation: number;
  kind: NavigationKind;
  targetUrl: string | null;
  frame: NavigationFrameIdentity | null;
  createdEventSequence: number;
  startEventSequence: number | null;
  commitEventSequence: number | null;
  frameFinishEventSequence: number | null;
  finishEventSequence: number | null;
  stopEventSequence: number | null;
  committedUrl: string | null;
  awaitingStart: boolean;
  started: boolean;
  loadingStarted: boolean;
  frameNavigated: boolean;
  committed: boolean;
  frameFinished: boolean;
  documentFinished: boolean;
  completed: boolean;
  failed: boolean;
  settled: boolean;
  loadingEmitted: boolean;
  pagePublished: boolean;
  waiters: Set<NavigationWaiter>;
}

type NavigationEventPhase =
  | "start-loading"
  | "commit"
  | "frame-finish"
  | "finish"
  | "stop"
  | "fail"
  | "title";

interface NavigationEventExpectation {
  generation: number;
  phase: NavigationEventPhase;
  url?: string;
  frame?: NavigationFrameIdentity;
  allowSettled?: boolean;
}

type ViewportCommitState =
  | "requested"
  | "applying"
  | "ready"
  | "superseded"
  | "invalidated"
  | "failed";

interface ViewportCommit {
  revision: number;
  bounds: Rectangle;
  viewport: BrowserLogicalViewport;
  navigationGeneration: number;
  debuggerSessionGeneration: number;
  state: ViewportCommitState;
  abort: AbortController;
  promise: Promise<void>;
  resolve: () => void;
  reject: (error: Error) => void;
}

interface ViewportCommitLifecycle {
  nextRevision: number;
  current: ViewportCommit | null;
  running: Promise<void> | null;
  applied: {
    viewport: BrowserLogicalViewport;
    scale: number | null;
  } | null;
}

interface NavigationEventClaim {
  operation: NavigationOperation;
  sequence: number;
}

type DebuggerMessageListener = (
  event: Electron.Event,
  method: string,
  params: unknown,
  sessionId?: string,
) => void;

type DebuggerDetachListener = (event: Electron.Event, reason: string) => void;

interface SurfaceLaneContext {
  track(promise: Promise<unknown>): void;
  assertCurrent(): void;
  debuggerLease(sessionId?: string): DebuggerSessionLease;
}

interface DebuggerSessionLease {
  generation: number;
  sessionId?: string;
  assertCurrent(): void;
}

export interface BrowserSurfaceActivationInput {
  windowId: string;
  tabId: string;
  browserSessionId: string;
  url: string;
  navigationRevision: number;
  viewport: BrowserLogicalViewport;
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
const MIN_NATIVE_VIEWPORT_SCALE = 0.05;
const VIEWPORT_SCALE_EPSILON = 0.001;
const DEBUGGER_RECONNECT_INITIAL_DELAY_MS = 100;
const DEBUGGER_RECONNECT_MAX_DELAY_MS = 2_000;
const MAX_INSPECTED_TEXT_LENGTH = 4 * 1024;
const MAX_INSPECTED_OUTER_HTML_LENGTH = 64 * 1024;
const INSPECT_HIGHLIGHT_CONFIG = {
  showInfo: false,
  contentColor: { r: 66, g: 133, b: 244, a: 0.18 },
  paddingColor: { r: 66, g: 133, b: 244, a: 0.12 },
  borderColor: { r: 66, g: 133, b: 244, a: 0.9 },
  marginColor: { r: 66, g: 133, b: 244, a: 0.08 },
};
const DIALOG_BRIDGE_BINDING = "__magiBrowserDialog";
const NATIVE_DIALOG_OPENED_RESULT = Object.freeze({ __magi_dialog_opened: true });
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
  readonly #downloadUserDataPath: string | null;
  readonly #downloadRoot: string | null;
  readonly #activeDownloads = new Set<Electron.DownloadItem>();
  readonly #windows = new Map<string, BaseWindow>();
  // Browser Surface 直接挂在 BaseWindow.contentView。这里保存的是窗口的
  // 原生根节点，不是额外的 BrowserHost 容器；否则子 WebContentsView 的
  // CSS viewport 会继承中间容器的默认尺寸而非右栏真实内容槽。
  readonly #contentRoots = new Map<string, View>();
  readonly #activationGenerations = new Map<string, number>();
  readonly #onEvent: (event: BrowserSurfaceEvent) => void;
  readonly #onContentSlotReady: ((binding: BrowserSurfaceBinding) => void) | undefined;
  readonly #onDocumentReady: ((binding: BrowserSurfaceBinding) => void) | undefined;
  #downloadCleanupPromise: Promise<void> | null = null;
  #downloadCleanupInProgress = false;

  constructor(input: {
    desktopEpoch: string;
    onEvent: (event: BrowserSurfaceEvent) => void;
    onContentSlotReady?: (binding: BrowserSurfaceBinding) => void;
    onDocumentReady?: (binding: BrowserSurfaceBinding) => void;
    partitionRegistryPath?: string;
  }) {
    this.#desktopEpoch = input.desktopEpoch;
    this.#onEvent = input.onEvent;
    this.#onContentSlotReady = input.onContentSlotReady;
    this.#onDocumentReady = input.onDocumentReady;
    this.#partitionRegistryPath = input.partitionRegistryPath?.trim() || null;
    this.#downloadUserDataPath = this.#partitionRegistryPath
      ? dirname(this.#partitionRegistryPath)
      : null;
    this.#downloadRoot = this.#downloadUserDataPath
      ? browserDownloadRoot(this.#downloadUserDataPath)
      : null;
    for (const partitionId of readPartitionRegistry(this.#partitionRegistryPath)) {
      this.#knownPartitions.add(partitionId);
    }
  }

  attachWindow(windowId: string, window: BaseWindow, contentRoot: View): void {
    this.#windows.set(windowId, window);
    this.#contentRoots.set(windowId, contentRoot);
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
    if (record && record.browserSessionId !== input.browserSessionId) {
      // tab_id 在一个浏览器会话内稳定，但不能把另一个会话的 WebContents
      // 或 partition 复用给它。旧 Surface 仍可能有迟到的导航/debugger
      // 事件，先按完整生命周期关闭，再创建新的物理 Surface。
      this.closeRecord(record, false);
      record = null;
    }
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
      created
      || (record.contents.getURL() || "") === ""
      || (
        initialUrl !== "about:blank"
        && (record.contents.getURL() || "about:blank") === "about:blank"
      )
    ) {
      const load = this.startLoad(record, initialUrl);
      if (input.awaitPageLoad === true) await load;
      else void load.catch(() => undefined);
    } else if (!record.contents.isLoadingMainFrame()) {
      this.scheduleViewportCommit(record);
    }
    // 真实导航和调试器初始化是两个独立生命周期。页面必须先由
    // WebContents 自己开始加载；CDP 只在工具调用时等待，不能成为首帧
    // 或导航的前置条件。
    this.startDebuggerInitialization(record);
    return this.binding(record);
  }
  private createSurface(input: MaterializeSurfaceInput): BrowserSurfaceRecord {
    const window = this.#windows.get(input.windowId);
    if (!window || window.isDestroyed()) throw new Error("desktop_window_not_found");
    const contentRoot = this.#contentRoots.get(input.windowId);
    if (!contentRoot) throw new Error("browser_surface_root_not_found");
    const partitionId = browserPartitionId(input.browserSessionId);
    this.configurePartition(partitionId);
    const view = new WebContentsView({
      webPreferences: {
        partition: partitionId,
        nodeIntegration: false,
        contextIsolation: true,
        sandbox: true,
        webSecurity: true,
        // Electron 18 起 window.open 使用原生 Chromium 行为，所有新窗口
        // 请求统一先经过下面唯一的 setWindowOpenHandler 策略；Browser Tab
        // 不依赖已移除的 nativeWindowOpen 选项，也不会为弹窗创建第二个
        // BrowserWindow/Target。
        // 页面不得再创建嵌套的 Electron guest WebContents。右栏的一级
        // Browser Tab 是唯一浏览器容器，页面内部不存在可持久化的子 Tab。
        webviewTag: false,
        allowRunningInsecureContent: false,
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
      mounted: false,
      slotVisible: false,
      slotBounds: null,
      slotLeaseRevision: 0,
      recoverySlot: null,
      activationGeneration: input.activationGeneration ?? null,
      priming: true,
      // CDP target 查询是异步握手，不参与 Surface 绑定。这个 ID 只用于
      // binding 一致性校验，使用 WebContents 生命周期内稳定的宿主 ID。
      targetId: `webcontents-${contents.id}`,
      navigationRevision: input.navigationRevision,
      navigationOperationId: 0,
      navigationGeneration: 0,
      navigationEventSequence: 0,
      navigationOperation: null,
      navigationFailureReportedGeneration: null,
      viewport: input.viewport,
      primary: false,
      closed: false,
      automationInputDepth: 0,
      agentControlled: false,
      cursor: { visible: false, x: null, y: null, action: null },
      cursorExecutionContextId: null,
      cdpLane: Promise.resolve(),
      viewportLifecycle: {
        nextRevision: 0,
        current: null,
        running: null,
        applied: null,
      },
      debuggerListenersInstalled: false,
      debuggerMessageListener: null,
      debuggerDetachListener: null,
      debuggerSessionGeneration: 0,
      debuggerSessionInitialized: false,
      dialogBridgeInstalled: false,
      nativeDialogOpeningWaiters: new Set(),
      cdpSessionIds: new Set(),
      blockedCdpSessionIds: new Set(),
      debuggerReadyPromise: null,
      debuggerReconnectTimer: null,
      debuggerReconnectAttempt: 0,
      recoveryPromise: null,
      loadPromise: null,
      inspectGeneration: 0,
      inspectActive: false,
      inspectGestureActive: false,
      inspectStartPromise: null,
      inspectResourcesEnabled: false,
      inspectHoverPoint: null,
      inspectHoverPromise: null,
      lifecycleEpoch: 0,
      lifecycleAbort: new AbortController(),
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
    return record;
  }

  bindContentSurface(
    windowId: string,
    tabId: string,
    surfaceId: string | null,
    bounds: Rectangle | null,
    parentBounds: Rectangle | null = null,
  ): void {
    const window = this.#windows.get(windowId);
    const records = [...this.#surfaces.values()].filter((record) => (
      record.windowId === windowId && !record.closed
    ));
    if (!tabId || !surfaceId) {
      for (const record of records) this.unmountSurface(record, window);
      return;
    }

    const target = records.find((record) => (
      record.tabId === tabId && record.surfaceId === surfaceId
    )) ?? null;
    if (!target) {
      for (const record of records) this.unmountSurface(record, window);
      return;
    }

    if (!bounds) {
      // 没有 Renderer 已确认的内容槽时，原生 View 必须退出命中树。
      // WebContents 保留在 Surface 记录中，下一份有效几何到达时复用同一
      // 页面；绝不能把旧 bounds 当作当前右栏位置，否则拖动或重排期间会
      // 覆盖工具栏、其他面板或中栏。
      for (const record of records) this.unmountSurface(record, window);
      return;
    }

    // ParentBounds 是 Renderer 已确认的右栏安全边界。即使上游误把一份
    // 过期或跨坐标系的内容槽传进来，也不能让 WebContentsView 越出右栏，
    // 否则它会覆盖右栏工具栏、其他面板甚至中栏内容。
    if (!parentBounds || !containsBounds(parentBounds, bounds)) {
      for (const record of records) this.unmountSurface(record, window);
      return;
    }

    // 先将新的 WebContentsView 挂入已经租约化的物理内容槽，再撤掉旧页面。
    // 不能按 Surface 创建顺序先卸载旧 View，否则 Browser Tab 切换会产生
    // 一帧空槽，用户会看到黑屏或闪断。
    this.applySlot(target, bounds, window);
    for (const record of records) {
      if (record !== target) this.unmountSurface(record, window);
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

  activationInputForTab(tabId: string): BrowserSurfaceActivationInput | null {
    const record = this.#surfaces.primaryForTab(tabId);
    if (!record || record.closed || record.contents.isDestroyed()) return null;
    return {
      windowId: record.windowId,
      tabId: record.tabId,
      browserSessionId: record.browserSessionId,
      url: record.contents.getURL() || "about:blank",
      navigationRevision: record.navigationRevision,
      viewport: structuredClone(record.viewport),
    };
  }

  isRenderableBinding(binding: BrowserSurfaceBinding): boolean {
    const record = this.#surfaces.get(binding.surface_id);
    if (!record || record.closed) return false;
    try {
      this.recordForBinding(binding);
    } catch {
      return false;
    }
    return this.isRenderable(record);
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
      || !matchesNavigationRevision(
        binding.navigation_revision,
        record.navigationRevision,
        options.allowNavigationAdvance === true,
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
    options: { allowNavigationAdvance?: boolean; signal?: AbortSignal } = {},
  ): Promise<unknown> {
    throwIfAborted(options.signal);
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
      await withOptionalAbort(this.waitForDebugger(record), options.signal);
    } else if (!record.contents.debugger.isAttached()) {
      await withOptionalAbort(this.waitForDebugger(record), options.signal);
    }
    // 输入事件属于一个完整的用户动作。Enter、点击或输入事件可能在
    // keyDown/mousePressed 之后立即推进 navigation_revision；后续的
    // keyUp/mouseReleased 仍然必须送到同一个 WebContents，而不是被页面
    // 自身导航误判成 Surface 已失效。Surface/Tab/桌面代次仍由上面的
    // 完整身份校验严格保护。
    // 命令真正进入 Surface lane 前必须严格绑定发起时的文档。允许导航
    // 代次前进只适用于命令完成后的结果校验，不能让旧命令迟到后进入新文档。
    const contents = this.recordForBinding(binding);
    if (sessionId && !record.cdpSessionIds.has(sessionId)) {
      throw staleSurfaceError("browser_cdp_session_stale");
    }
    // 所有需要页面 viewport 的自动化都必须绑定当前右栏内容槽。Detached
    // WebContents 虽然仍保留页面生命周期，但 Chromium compositor 没有有效
    // viewport，截图会超时，命中检测也不再代表用户看到的页面。
    if (!this.isPrimary(binding) || !this.isRenderable(record)) {
      throw staleSurfaceError("browser_surface_content_slot_unavailable");
    }
    if (!contents.debugger.isAttached()) {
      throw staleSurfaceError("browser_debugger_detached");
    }
    await withOptionalAbort(this.waitForViewportCommit(record), options.signal);
    this.assertRenderableBinding(binding);
    if (isDialogCommand) {
      try {
        const lease = this.createDebuggerSessionLease(record, sessionId);
        const result = await this.sendSurfaceCdpCommand(
          record,
          method,
          params,
          DEFAULT_CDP_COMMAND_TIMEOUT_MS,
          lease,
          undefined,
          options.signal,
        );
        this.assertRenderableBinding(binding, options);
        return result;
      } catch (error) {
        if (isCdpTimeoutError(error)) this.invalidateDebuggerSession(record, "cdp-timeout");
        if (isBrowserCommandCancelledError(error)) {
          this.invalidateDebuggerSession(record, "command-cancelled");
        }
        throw error;
      }
    }
    if (method === "Page.captureScreenshot") {
      // 截图统一读取当前 WebContents 的 Chromium compositor surface，
      // 使 clip、元素范围和 captureBeyondViewport 都遵循 Page domain 的
      // 页面坐标语义。宿主不再参与截图坐标换算，也不再建立 native
      // capturePage 旁路；等待页面和视口状态稳定后，统一进入同一个 CDP
      // lane，与其他页面命令按 Surface 串行执行。
      await withOptionalAbort(this.waitForScreenshotReadiness(record), options.signal);
      this.assertRenderableBinding(binding);
    }
    const injectsInput = method.startsWith("Input.");
    if (injectsInput) record.automationInputDepth += 1;
    let inputCommand: Promise<unknown> | null = null;
    let inputCompletionTracked = false;
    let inputReleased = false;
    const releaseInput = () => {
      if (inputReleased) return;
      inputReleased = true;
      record.automationInputDepth = Math.max(0, record.automationInputDepth - 1);
    };
    try {
      inputCommand = this.enqueueCdp(record, async ({ track }) => {
        // 只在当前输入真正取得 Surface lane 后注册 waiter，避免排队中的
        // 后续输入被同一次 dialog opening 错误地当成已执行。
        const nativeDialogOpening = injectsInput
          ? this.createNativeDialogOpeningWaiter(record)
          : null;
        try {
          // CDP 请求在 lane 中等待期间页面可能已发生导航。重新校验把
          // 普通 DOM/调试命令限制在原文档；只有输入事件允许完成同一动作的
          // keyUp/mouseUp 收尾，避免旧命令迟到后作用于新页面。
          // 与 lane 外的校验保持一致：排队期间文档已经换代时，旧命令必须
          // 失败并交给上层使用新的 binding 重试，不能把旧 DOM 操作投递到新页。
          this.recordForBinding(binding);
          const command = this.sendSurfaceCdpCommand(
            record,
            method,
            method === "Page.captureScreenshot" ? { ...params, fromSurface: true } : params,
            method === "Page.captureScreenshot" ? SCREENSHOT_CDP_COMMAND_TIMEOUT_MS : DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            this.createDebuggerSessionLease(record, sessionId),
            track,
            options.signal,
          );
          if (injectsInput) {
            // native dialog 会阻塞 Input.* 的 CDP response。让输入调用先以
            //“动作已触发”收口，后续 browser_dialog 才能穿过 Surface lane；
            // 原始请求仍由 enqueueCdp 追踪直到收敛，期间保持输入深度。
            inputCompletionTracked = true;
            track(command.then(releaseInput, releaseInput));
          }
          return injectsInput && nativeDialogOpening
            ? await Promise.race([
              command,
              nativeDialogOpening.promise.then(() => NATIVE_DIALOG_OPENED_RESULT),
            ])
            : await command;
        } catch (error) {
          if (injectsInput && !inputCompletionTracked) releaseInput();
          throw error;
        } finally {
          nativeDialogOpening?.cancel();
        }
      }, options.signal);
      const result = await inputCommand;
      this.assertRenderableBinding(binding, {
        allowNavigationAdvance: options.allowNavigationAdvance === true || method.startsWith("Input."),
      });
      if (result === NATIVE_DIALOG_OPENED_RESULT) return result;
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
        this.assertRenderableBinding(binding, {
          allowNavigationAdvance: true,
        });
      }
      return result;
    } finally {
      if (injectsInput && !inputCompletionTracked) releaseInput();
    }
  }

  private enqueueCdp<T>(
    record: BrowserSurfaceRecord,
    operation: (context: SurfaceLaneContext) => Promise<T>,
    signal?: AbortSignal,
  ): Promise<T> {
    const previous = record.cdpLane;
    const lifecycleEpoch = record.lifecycleEpoch;
    const tracked: Promise<unknown>[] = [];
    let operationStarted = false;
    const lane = previous.catch(() => undefined).then(async () => {
      this.assertSurfaceLifecycleCurrent(record, lifecycleEpoch);
      throwIfAborted(signal);
      try {
        operationStarted = true;
        return await operation({
          track: (promise) => tracked.push(promise),
          assertCurrent: () => this.assertSurfaceLifecycleCurrent(record, lifecycleEpoch),
          debuggerLease: (sessionId) => this.createDebuggerSessionLease(record, sessionId),
        });
      } catch (error) {
        if (isCdpTimeoutError(error)) {
          // Electron 的 debugger.sendCommand 没有可取消句柄。超时后必须
          // 废弃整个 debugger session 并让 Chromium 结束旧请求，否则迟到
          // 的响应会继续污染同一 Surface 的后续命令队列。
          this.invalidateDebuggerSession(record, "cdp-timeout");
        }
        if (operationStarted && isBrowserCommandCancelledError(error)) {
          // Electron 的 debugger.sendCommand 没有取消句柄。取消已经发给
          // Main 的 CDP 请求时，直接废弃当前 debugger session，让迟到的
          // Chromium Promise 只能落在旧 lease 上；Surface lane 则立即释放
          // 给后续导航/刷新，不再等待旧请求的 30 秒超时。
          this.invalidateDebuggerSession(record, "command-cancelled");
        }
        throw error;
      }
    });
    const run = withOptionalAbort(lane, signal);
    const settled = run.then(
      async () => { await Promise.allSettled(tracked); },
      async () => { await Promise.allSettled(tracked); },
    );
    record.cdpLane = settled.then(() => undefined, () => undefined);
    return run;
  }

  private createDebuggerSessionLease(
    record: BrowserSurfaceRecord,
    sessionId?: string,
    generation = record.debuggerSessionGeneration,
  ): DebuggerSessionLease {
    return {
      generation,
      ...(sessionId === undefined ? {} : { sessionId }),
      assertCurrent: () => {
        this.assertDebuggerSessionCurrent(record, generation);
        if (sessionId && !record.cdpSessionIds.has(sessionId)) {
          throw staleSurfaceError("browser_cdp_session_stale");
        }
      },
    };
  }

  private async sendSurfaceCdpCommand(
    record: BrowserSurfaceRecord,
    method: string,
    params: Record<string, unknown>,
    timeoutMs: number,
    lease: DebuggerSessionLease,
    track?: (promise: Promise<unknown>) => void,
    signal?: AbortSignal,
  ): Promise<unknown> {
    // lease 同时绑定 debugger generation 和 CDP 子 session。超时后即使
    // Electron 的旧 sendCommand Promise 迟到，也只能在这里被丢弃，不能把
    // 返回值写入新的 debugger session 状态。
    lease.assertCurrent();
    const result = await sendCdpCommandWithTimeout(
      record.contents,
      method,
      constrainCdpCommandParams(method, params),
      timeoutMs,
      lease.sessionId,
      track,
      signal,
    );
    lease.assertCurrent();
    return result;
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
    await this.withSurfaceLifecycle(record, Promise.race([
      loading.then(() => undefined, () => undefined),
      new Promise<void>((resolve) => {
        const timer = setTimeout(resolve, SCREENSHOT_READINESS_TIMEOUT_MS);
        timer.unref();
      }),
    ]));
  }

  private async waitForViewportCommit(record: BrowserSurfaceRecord): Promise<void> {
    while (!record.closed && !record.contents.isDestroyed()) {
      let commit = record.viewportLifecycle.current;
      if (!commit) {
        if (!record.slotVisible || !record.slotBounds || !record.mounted) return;
        commit = this.scheduleViewportCommit(record);
        if (!commit) return;
      }
      try {
        await this.withSurfaceLifecycle(record, commit.promise);
      } catch (error) {
        // 失效提交只代表布局、导航或 debugger session 已经换代。只要
        // 当前 Surface 仍有效，就重新等待最新提交；真正的 apply 失败才
        // 交给调用方，避免把旧提交的错误传给新视口。
        if (record.viewportLifecycle.current !== commit) continue;
        if (commit.state === "invalidated" || commit.state === "superseded") continue;
        throw error;
      }
      if (this.isViewportCommitInputCurrent(record, commit) && commit.state === "ready") return;
    }
    throw staleSurfaceError("browser_surface_lifecycle_stale");
  }

  private scheduleViewportCommit(record: BrowserSurfaceRecord): ViewportCommit | null {
    const commit = this.requestViewportCommit(record);
    if (!commit || record.viewportLifecycle.running) return commit;

    const running = this.flushViewportCommits(record);
    record.viewportLifecycle.running = running;
    // 后台布局没有调用方等待，但它的拒绝必须被消费；提交本身仍保留
    // reject 给显式 setViewport/截图调用方。
    void commit.promise.catch(() => undefined);
    void running.catch((error) => {
      if (!record.closed) {
        console.warn("[BrowserSurfaceManager] Browser Surface viewport 提交失败", {
          surfaceId: record.surfaceId,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    }).finally(() => {
      if (record.viewportLifecycle.running === running) record.viewportLifecycle.running = null;
    });
    return commit;
  }

  private requestViewportCommit(record: BrowserSurfaceRecord): ViewportCommit | null {
    const bounds = record.slotBounds;
    if (record.closed || !record.slotVisible || !bounds || !record.mounted) return null;
    const current = record.viewportLifecycle.current;
    const debuggerGenerationMatches = record.viewport.mode === "auto"
      || current?.debuggerSessionGeneration === record.debuggerSessionGeneration;
    if (
      current
      && ["requested", "applying", "ready"].includes(current.state)
      && current.navigationGeneration === record.navigationGeneration
      && debuggerGenerationMatches
      && sameBounds(current.bounds, bounds)
      && sameLogicalViewport(current.viewport, record.viewport)
    ) return current;
    if (current && !["superseded", "invalidated", "failed"].includes(current.state)) {
      current.state = "superseded";
      current.abort.abort();
      current.resolve();
    }
    let resolve!: () => void;
    let reject!: (error: Error) => void;
    const promise = new Promise<void>((resolvePromise, rejectPromise) => {
      resolve = resolvePromise;
      reject = rejectPromise;
    });
    const commit: ViewportCommit = {
      revision: ++record.viewportLifecycle.nextRevision,
      bounds: { ...bounds },
      viewport: structuredClone(record.viewport),
      navigationGeneration: record.navigationGeneration,
      debuggerSessionGeneration: record.debuggerSessionGeneration,
      state: "requested",
      abort: new AbortController(),
      promise,
      resolve,
      reject,
    };
    record.viewportLifecycle.current = commit;
    return commit;
  }

  private invalidateViewportCommit(
    record: BrowserSurfaceRecord,
    _reason: "slot-unmounted" | "navigation" | "debugger-session" | "surface-closed",
  ): void {
    const current = record.viewportLifecycle.current;
    if (current && !["superseded", "invalidated", "failed"].includes(current.state)) {
      current.state = "invalidated";
      current.abort.abort();
      current.resolve();
    }
    record.viewportLifecycle.current = null;
  }

  private async flushViewportCommits(record: BrowserSurfaceRecord): Promise<void> {
    while (!record.closed && !record.contents.isDestroyed()) {
      const commit = record.viewportLifecycle.current;
      if (!commit || commit.state !== "requested" && commit.state !== "applying") return;
      if (!this.isViewportCommitInputCurrent(record, commit)) return;
      // 导航期间保留旧的原生 WebContentsView 和最后一帧；新提交等到主
      // 文档结束后再进入 compositor，避免刷新/跳转期间黑屏闪烁。
      if (record.priming || record.contents.isLoadingMainFrame()) return;
      if (
        commit.viewport.mode === "fixed"
        && (!record.contents.debugger.isAttached() || !record.debuggerSessionInitialized)
      ) return;
      commit.state = "applying";
      try {
        await this.enqueueCdp(record, ({ track, debuggerLease }) => this.applyViewport(
          record,
          commit,
          debuggerLease(),
          track,
        ));
        if (!this.isViewportCommitInputCurrent(record, commit)) continue;
        // `applyViewport` 已通过同一条 Surface CDP lane 完成，且此提交只
        // 在主文档完成加载后执行。这里直接把原生 WebContentsView 标为可用。
        // 不再调用 executeJavaScript 等待 RAF：RAF 只证明页面脚本调度，不能
        // 证明 WebContentsView 的宿主合成，而且在真实桌面窗口的可见/焦点
        // 状态切换时可能永不返回，曾导致截图和标记被无故阻塞 5 秒。
        commit.state = "ready";
        if (!this.isViewportCommitInputCurrent(record, commit)) continue;
        commit.resolve();
        this.#onContentSlotReady?.(this.binding(record));
        if (record.agentControlled) {
          void this.setAgentCursor(record, true, record.cursor.x, record.cursor.y, record.cursor.action)
            .catch(() => undefined);
        }
        return;
      } catch (error) {
        if (!this.isViewportCommitInputCurrent(record, commit)) continue;
        commit.state = "failed";
        commit.reject(toError(error));
        return;
      }
    }
  }

  private isViewportCommitInputCurrent(
    record: BrowserSurfaceRecord,
    commit: ViewportCommit,
  ): boolean {
    return record.viewportLifecycle.current === commit
      && !record.closed
      && !record.contents.isDestroyed()
      && record.slotVisible
      && record.slotBounds !== null
      && record.mounted
      && record.view.getVisible()
      && sameBounds(record.slotBounds, commit.bounds)
      && record.navigationGeneration === commit.navigationGeneration
      && (
        commit.viewport.mode === "auto"
        || record.debuggerSessionGeneration === commit.debuggerSessionGeneration
      );
  }

  private withSurfaceLifecycle<T>(record: BrowserSurfaceRecord, promise: Promise<T>): Promise<T> {
    this.assertSurfaceLifecycleCurrent(record, record.lifecycleEpoch);
    return withAbortSignal(
      promise,
      record.lifecycleAbort.signal,
      staleSurfaceError("browser_surface_lifecycle_stale"),
    );
  }

  async navigate(
    binding: BrowserSurfaceBinding,
    navigation: BrowserNavigation,
  ): Promise<BrowserPageState> {
    const record = this.requireRecord(binding.surface_id);
    await this.waitForDebugger(record);
    const currentRecord = this.assertRenderableBinding(binding);
    const contents = currentRecord.contents;
    switch (navigation.action) {
      case "url": {
        let initScriptId: string | null = null;
        if (navigation.init_script?.trim()) {
          const installed = await this.enqueueCdp(record, ({ debuggerLease, track }) => this.sendSurfaceCdpCommand(
            record,
            "Page.addScriptToEvaluateOnNewDocument",
            { source: navigation.init_script },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            debuggerLease(),
            track,
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
            await this.enqueueCdp(record, ({ debuggerLease, track }) => this.sendSurfaceCdpCommand(
              record,
              "Page.removeScriptToEvaluateOnNewDocument",
              { identifier: initScriptId },
              DEFAULT_CDP_COMMAND_TIMEOUT_MS,
              debuggerLease(),
              track,
            )).catch(() => undefined);
          }
        }
        break;
      }
      case "back":
        if (contents.navigationHistory.canGoBack()) {
          const index = contents.navigationHistory.getActiveIndex();
          const entry = contents.navigationHistory.getEntryAtIndex(index - 1);
          await this.runNavigationAction(
            record,
            "history",
            entry?.url ?? null,
            () => contents.navigationHistory.goBack(),
            navigation.timeout_ms,
          );
        }
        break;
      case "forward":
        if (contents.navigationHistory.canGoForward()) {
          const index = contents.navigationHistory.getActiveIndex();
          const entry = contents.navigationHistory.getEntryAtIndex(index + 1);
          await this.runNavigationAction(
            record,
            "history",
            entry?.url ?? null,
            () => contents.navigationHistory.goForward(),
            navigation.timeout_ms,
          );
        }
        break;
      case "reload":
        await this.runNavigationAction(
          record,
          "reload",
          contents.getURL() || "about:blank",
          () => {
            if (navigation.ignore_cache === true) contents.reloadIgnoringCache();
            else contents.reload();
          },
          navigation.timeout_ms,
          navigation.handle_before_unload,
        );
        break;
    }
    // URL/back/forward/reload 都会在导航事务开始时推进 navigation_revision。
    // 物理 Surface 身份仍必须严格匹配，但完成结果应读取同一 WebContents
    // 的新文档代次；继续用旧 binding 做严格 revision 相等校验会把已完成
    // 的真实导航错误收敛成 browser_surface_stale。
    const finalRecord = this.assertRenderableBinding(binding, { allowNavigationAdvance: true });
    return this.pageState(finalRecord);
  }

  async setViewport(
    binding: BrowserSurfaceBinding,
    viewport: BrowserLogicalViewport,
  ): Promise<void> {
    const record = this.requireRecord(binding.surface_id);
    await this.waitForDebugger(record);
    this.assertRenderableBinding(binding);
    record.viewport = viewport;
    if (record.contents.isLoadingMainFrame()) return;
    this.scheduleViewportCommit(record);
    await this.waitForViewportCommit(record);
    this.assertRenderableBinding(binding);
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
    this.assertRenderableBinding(binding, {});
    await this.waitForDebugger(record);
    try {
      this.assertRenderableBinding(binding, {});
    } catch {
      throw staleSurfaceError("browser_inspect_surface_inactive");
    }
    if (record.inspectActive) {
      const pendingStart = record.inspectStartPromise;
      if (pendingStart) {
        await pendingStart;
        // stopInspect() or a lifecycle transition may have invalidated the
        // first start while this caller was waiting. Revalidate before
        // starting a new generation instead of reporting a false success.
        try {
          this.assertRenderableBinding(binding);
        } catch {
          throw staleSurfaceError("browser_inspect_surface_inactive");
        }
      }
      if (record.inspectActive) return;
    }

    const generation = ++record.inspectGeneration;
    record.inspectActive = true;
    record.inspectGestureActive = false;
    record.inspectHoverPoint = null;
    const start = this.enqueueCdp(record, async ({ track, debuggerLease }) => {
      if (!this.isInspectGenerationActive(record, generation)) return;
      const lease = debuggerLease();
      // Chromium 的 Overlay Agent 依赖已启用的 DOM Agent。先启用 DOM，
      // 再启用 Overlay，才能在所有 Electron/Chromium 版本中建立真正的
      // Inspect Mode；反过来会直接返回 “DOM should be enabled first”。
      await this.sendSurfaceCdpCommand(
        record,
        "DOM.enable",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
        track,
      );
      if (!this.isInspectGenerationActive(record, generation)) return;
      await this.sendSurfaceCdpCommand(
        record,
        "Overlay.enable",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
        track,
      );
      if (!this.isInspectGenerationActive(record, generation)) return;
      record.inspectResourcesEnabled = true;
      // Electron 的独立 WebContentsView 不会稳定发出 DevTools 前端使用的
      // DevTools 前端节点选择事件。选择事务由 before-mouse-event 拦截真实
      // 鼠标，再通过 Chromium DOM.getNodeForLocation 命中节点；Overlay
      // 只负责原生高亮，避免点击穿透到网页并触发业务操作。
      await this.sendSurfaceCdpCommand(
        record,
        "Overlay.hideHighlight",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
        track,
      );
      if (!this.isInspectGenerationActive(record, generation)) return;
    });
    record.inspectStartPromise = start;
    try {
      await start;
      // 导航、Primary 切换或卸载可能在 CDP lane 等待期间取消了本次
      // inspect。不能把被取消的操作报告成成功，否则 Renderer/Host 会
      // 认为后续点击一定会产生 node_inspected 事件。
      this.assertRenderableBinding(binding);
      if (!record.inspectActive) {
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
    this.assertRenderableBinding(binding);
    await this.stopInspectRecord(record);
    this.assertRenderableBinding(binding);
  }

  private isInspectGenerationActive(record: BrowserSurfaceRecord, generation: number): boolean {
    return !record.closed
      && !record.contents.isDestroyed()
      && this.isRenderable(record)
      && record.primary
      && this.#surfaces.isPrimary(record)
      && record.inspectActive
      && record.inspectGeneration === generation
      && record.contents.debugger.isAttached();
  }

  private stopInspectForLifecycle(record: BrowserSurfaceRecord, reason: string): void {
    // node-selected 只关闭 Chromium Overlay 资源；当前真实点击的 mouseUp
    // 仍属于 Inspect 手势，必须继续被输入策略识别，不能在清理 CDP 时把
    // 它提前降级为 user_takeover。
    void this.stopInspectRecord(record, reason === "node-selected").catch((error) => {
      if (!record.closed) {
        console.warn("[BrowserSurfaceManager] 节点检查清理失败", {
          surfaceId: record.surfaceId,
          reason,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    });
  }

  private stopInspectRecord(record: BrowserSurfaceRecord, preserveGesture = false): Promise<void> {
    // 捕获请求发生时的资源状态。Debugger session reset 会在同一个同步
    // 生命周期事务中清零 record.inspectResourcesEnabled，但这不能让已经
    // 排队的 Overlay 清理失去依据。
    const resourcesEnabledAtRequest = record.inspectResourcesEnabled;
    const hasInspectWork = record.inspectActive
      || record.inspectStartPromise !== null
      || resourcesEnabledAtRequest;
    if (!hasInspectWork) return Promise.resolve();

    const generation = ++record.inspectGeneration;
    record.inspectActive = false;
    record.inspectHoverPoint = null;
    if (!preserveGesture) record.inspectGestureActive = false;
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
      const resourcesEnabled = resourcesEnabledAtRequest || record.inspectResourcesEnabled;
      if (!resourcesEnabled) return;
      await this.enqueueCdp(record, async ({ track, debuggerLease }) => {
        if (record.closed || record.contents.isDestroyed() || record.inspectGeneration !== generation || record.inspectActive) {
          return;
        }
        if (!resourcesEnabledAtRequest && !record.inspectResourcesEnabled) return;
        let firstError: unknown = null;
        for (const [method, params] of [
          ["Overlay.hideHighlight", {}],
          ["Overlay.disable", {}],
        ] as const) {
          try {
            await this.sendSurfaceCdpCommand(
              record,
              method,
              params,
              DEFAULT_CDP_COMMAND_TIMEOUT_MS,
              debuggerLease(),
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

  private initialAgentCursorPosition(record: BrowserSurfaceRecord): { x: number; y: number } | null {
    const viewBounds = record.view.getBounds();
    const bounds = viewBounds.width > 0 && viewBounds.height > 0
      ? viewBounds
      : record.slotVisible
        && record.slotBounds
        && record.slotBounds.width > 0
        && record.slotBounds.height > 0
        ? record.slotBounds
        : null;
    if (!bounds) return null;
    const { width, height } = bounds;
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
    this.#contentRoots.delete(windowId);
    this.#activationGenerations.delete(windowId);
  }

  closeAll(): void {
    this.cancelActiveDownloads();
    for (const record of [...this.#surfaces.values()]) this.closeRecord(record, false);
    this.#surfaces.clear();
    this.#windows.clear();
    this.#contentRoots.clear();
    this.#activationGenerations.clear();
  }

  async clearDownloads(): Promise<void> {
    if (!this.#downloadUserDataPath) return;
    if (this.#downloadCleanupPromise) return this.#downloadCleanupPromise;

    const cleanup = this.performDownloadCleanup();
    this.#downloadCleanupPromise = cleanup;
    try {
      await cleanup;
    } finally {
      if (this.#downloadCleanupPromise === cleanup) this.#downloadCleanupPromise = null;
    }
  }

  async clearBrowsingData(): Promise<void> {
    await this.clearDownloads();
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
      .map((record) => this.runNavigationAction(
        record,
        "reload",
        record.contents.getURL() || "about:blank",
        () => record.contents.reloadIgnoringCache(),
        DEFAULT_NAVIGATION_TIMEOUT_MS,
      )));
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
    const hadSlotLease = record.slotVisible || record.slotBounds !== null;
    if (bounds) record.slotBounds = { ...bounds };
    // 原生 View 的 bounds 必须始终等于本次 Renderer 已确认的内容槽。
    // null 表示当前几何不可用，不能继续显示上一次的 bounds。
    const effectiveBounds = bounds ? { ...bounds } : null;
    record.slotVisible = effectiveBounds !== null;
    if (effectiveBounds && !hadSlotLease) record.slotLeaseRevision += 1;
    const contentRoot = this.#contentRoots.get(record.windowId);
    if (!window || window.isDestroyed() || !contentRoot) {
      this.detachSurface(record);
      return;
    }
    if (!effectiveBounds) {
      // 页面状态仍保留在 WebContents 中，但不可验证的中间帧必须隐藏
      // 并退出命中树。重新得到内容槽时只复用该 WebContents，不重新导航。
      this.detachSurface(record);
      return;
    }
    if (!record.mounted) {
      // WebContentsView 必须直接以 contentView 的第 1 层挂载。它的 bounds
      // 就是 Renderer 已确认的内容槽，因此 Chromium 获得的真实 viewport
      // 与右栏一致；第 0 层仍是 App Renderer，第 2 层保留给 Overlay。
      contentRoot.addChildView(record.view, 1);
      record.mounted = true;
    }
    const boundsChanged = !sameBounds(record.view.getBounds(), effectiveBounds);
    if (boundsChanged) record.view.setBounds(effectiveBounds);
    // 页面加载和调试器初始化是 Surface 内部状态，不能阻塞真实浏览器
    // 视图进入内容槽。Chromium 自行展示当前文档及加载过程；只有失败页
    // 才隐藏，避免激活流程变成“正在连接浏览器”的空白等待层。
    // 导航过程由 Chromium 自己绘制。只有 Surface 生命周期失败才解绑，
    // 页面导航失败仍保留真实 WebContentsView，避免右栏黑屏或空槽。
    record.view.setVisible(true);
    // 这是唯一的 Viewport 提交入口。它会捕获本次物理内容槽、逻辑
    // viewport、导航代次和 debugger session 代次，并在真正的 renderer
    // frame 到达后才发出 content-ready；这里不能再提前通知上层。
    this.scheduleViewportCommit(record);
    // 内容槽只管理原生 View 的物理承载范围；只有 fixed 模式才由
    // applyViewport 设置 Chromium 设备指标，禁止 CSS transform、截图映射
    // 或宿主坐标缩放。
  }

  private async loadPage(
    record: BrowserSurfaceRecord,
    url: string,
    timeoutMs = DEFAULT_NAVIGATION_TIMEOUT_MS,
    handleBeforeUnload?: "accept" | "dismiss",
    request?: () => Promise<void>,
  ): Promise<void> {
    if (record.closed || record.contents.isDestroyed()) {
      throw staleSurfaceError("browser_surface_not_found");
    }
    const operation = this.beginNavigationOperation(record, "document", url);
    const allowBeforeUnload = handleBeforeUnload === "accept"
      ? (event: Electron.Event) => event.preventDefault()
      : null;
    if (allowBeforeUnload) record.contents.on("will-prevent-unload", allowBeforeUnload);
    try {
      const loadPromise = request ? request() : record.contents.loadURL(url);
      // Electron 的 loadURL Promise 与 WebContents 导航事件不是同一个
      // 生命周期：部分版本会在 did-finish-load 之后才结算，另一些页面则
      // 只可靠地发出 did-stop-loading。它只能作为无事件导航的完成信号，
      // 不能和 navigationWait 一起放进 Promise.all，否则任一信号迟到都会
      // 把已可操作的页面和 Host 请求一起拖到超时。
      void loadPromise.then(() => {
        if (!this.isCurrentNavigationOperation(record, operation) || operation.settled) return;
        const settledUrl = record.contents.getURL() || url;
        if (!sameNavigationUrl(settledUrl, url)) return;
        this.completeNavigationFromSettledEvent(
          record,
          operation,
          settledUrl,
          this.nextNavigationEventSequence(record),
        );
      }, () => {
        // 失败详情由 did-fail-load 统一收口；这里消费 Promise rejection，
        // 避免 Electron 对网络错误或被替换导航产生未处理拒绝。
      });
      const navigationWait = this.waitForNavigationOperation(record, operation, timeoutMs);
      await this.withSurfaceLifecycle(
        record,
        withNavigationTimeout(navigationWait, clampNavigationTimeout(timeoutMs)),
      );
      if (!this.isCurrentNavigationOperation(record, operation) || !operation.completed) {
        throw staleSurfaceError("browser_navigation_superseded");
      }
    } catch (error) {
      if (!this.isCurrentNavigationOperation(record, operation) || operation.settled && !operation.failed) {
        throw staleSurfaceError("browser_navigation_superseded");
      }
      if (!operation.failed) this.failNavigationOperation(record, operation, toError(error));
      throw error;
    } finally {
      if (allowBeforeUnload) record.contents.off("will-prevent-unload", allowBeforeUnload);
    }
    if (!this.isCurrentNavigationOperation(record, operation) || !operation.completed) {
      throw staleSurfaceError("browser_navigation_superseded");
    }
    // did-finish-load 已经负责在新文档中安装桥接。导航完成不能再等待
    // 这条非关键的 CDP lane，否则导航本身会被旧的 Overlay/调试命令拖住。
  }

  private startLoad(record: BrowserSurfaceRecord, url: string): Promise<void> {
    if (
      record.loadPromise
      && record.navigationOperation
      && !record.navigationOperation.settled
      && sameNavigationUrl(record.navigationOperation.targetUrl ?? "about:blank", url)
    ) return record.loadPromise;
    // 初始导航是 Chromium 的原生加载过程，不得占用 CDP 命令队列。
    // 页面先立即进入真实 WebContentsView；调试器初始化在
    // startDebuggerInitialization() 中独立运行，不会成为导航前置条件。
    const load = this.loadPage(record, url);
    record.loadPromise = load;
    void load.then(() => {
      if (record.loadPromise === load) record.loadPromise = null;
    }, () => {
      if (record.loadPromise === load) record.loadPromise = null;
    });
    return load;
  }

  private startDebuggerInitialization(record: BrowserSurfaceRecord): void {
    if (
      record.closed
      || record.debuggerReadyPromise
      || record.debuggerSessionInitialized && record.contents.debugger.isAttached()
    ) return;
    void this.reconnectDebugger(record, "initialization").catch((error) => {
      if (!record.closed) {
        console.error("[BrowserSurfaceManager] Browser Surface 调试器初始化失败", {
          surfaceId: record.surfaceId,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    });
  }

  private publishNavigationFailure(record: BrowserSurfaceRecord, reason: string): void {
    const operation = record.navigationOperation;
    if (record.closed || !operation || record.navigationFailureReportedGeneration === operation.generation) return;
    record.navigationFailureReportedGeneration = operation.generation;
    this.#onEvent({
      type: "page_failed",
      binding: this.binding(record),
      reason,
    });
  }

  private async runNavigationAction(
    record: BrowserSurfaceRecord,
    kind: Exclude<NavigationKind, "document" | "in-page">,
    targetUrl: string | null,
    start: () => void,
    timeoutMs?: number,
    handleBeforeUnload?: "accept" | "dismiss",
  ): Promise<void> {
    const operation = this.beginNavigationOperation(record, kind, targetUrl);
    const allowBeforeUnload = handleBeforeUnload === "accept"
      ? (event: Electron.Event) => event.preventDefault()
      : null;
    if (allowBeforeUnload) record.contents.on("will-prevent-unload", allowBeforeUnload);
    try {
      start();
      await this.waitForNavigationOperation(record, operation, timeoutMs);
    } catch (error) {
      if (!this.isCurrentNavigationOperation(record, operation) || operation.settled && !operation.failed) {
        throw staleSurfaceError("browser_navigation_superseded");
      }
      if (!operation.failed) this.failNavigationOperation(record, operation, toError(error));
      throw error;
    } finally {
      if (allowBeforeUnload) record.contents.off("will-prevent-unload", allowBeforeUnload);
    }
  }

  private beginNavigationOperation(
    record: BrowserSurfaceRecord,
    kind: NavigationKind,
    targetUrl: string | null,
    createdEventSequence = this.nextNavigationEventSequence(record),
  ): NavigationOperation {
    if (record.closed || record.contents.isDestroyed()) {
      throw staleSurfaceError("browser_surface_not_found");
    }
    const previous = record.navigationOperation;
    if (previous && !previous.settled) {
      this.supersedeNavigationOperation(previous);
    }
    // 每个真实导航事务都必须获得新的文档代次。快照、DOM 节点和标记
    // 只能绑定到当前代次，不能因为地址变化而继续复用旧页面身份。
    record.navigationRevision += 1;
    const id = ++record.navigationOperationId;
    const operation: NavigationOperation = {
      id,
      generation: ++record.navigationGeneration,
      kind,
      targetUrl,
      frame: null,
      createdEventSequence,
      startEventSequence: null,
      commitEventSequence: null,
      frameFinishEventSequence: null,
      finishEventSequence: null,
      stopEventSequence: null,
      committedUrl: null,
      awaitingStart: true,
      started: false,
      loadingStarted: false,
      frameNavigated: false,
      committed: false,
      frameFinished: false,
      documentFinished: false,
      completed: false,
      failed: false,
      settled: false,
      loadingEmitted: kind !== "in-page",
      pagePublished: false,
      waiters: new Set(),
    };
    record.navigationOperation = operation;
    record.navigationFailureReportedGeneration = null;
    record.priming = kind !== "in-page";
    record.cursorExecutionContextId = null;
    this.invalidateViewportCommit(record, "navigation");
    this.stopInspectForLifecycle(record, "navigation");
    if (operation.loadingEmitted) {
      this.#onEvent({
        type: "loading_changed",
        binding: this.binding(record),
        loading: true,
      });
    }
    return operation;
  }

  private nextNavigationEventSequence(record: BrowserSurfaceRecord): number {
    record.navigationEventSequence += 1;
    return record.navigationEventSequence;
  }

  private claimNavigationEvent(
    record: BrowserSurfaceRecord,
    expectation: NavigationEventExpectation,
  ): NavigationEventClaim | null {
    const operation = record.navigationOperation;
    const sequence = this.nextNavigationEventSequence(record);
    if (
      record.closed
      || !operation
      || !this.isCurrentNavigationOperation(record, operation, expectation.generation)
      || operation.failed
      || !expectation.allowSettled && operation.settled
      || expectation.phase !== "title" && !operation.started
      || expectation.phase !== "start-loading" && expectation.phase !== "title" && operation.startEventSequence === null
    ) return null;
    if (expectation.frame && operation.frame && !sameFrame(operation.frame, expectation.frame)) return null;
    if (expectation.url) {
      const expectedUrl = expectation.phase === "commit" && !operation.committedUrl
        ? null
        : operation.committedUrl ?? operation.targetUrl;
      if (expectedUrl && !sameNavigationUrl(expectedUrl, expectation.url)) return null;
    }
    if (
      expectation.phase === "frame-finish" && operation.frameFinishEventSequence !== null
      || expectation.phase === "finish" && operation.finishEventSequence !== null
      || expectation.phase === "stop" && operation.stopEventSequence !== null
    ) return null;
    const lastOperationEvent = Math.max(
      operation.createdEventSequence,
      operation.startEventSequence ?? 0,
      operation.commitEventSequence ?? 0,
      operation.frameFinishEventSequence ?? 0,
      operation.finishEventSequence ?? 0,
      operation.stopEventSequence ?? 0,
    );
    if (sequence <= lastOperationEvent) return null;
    return { operation, sequence };
  }

  private supersedeNavigationOperation(operation: NavigationOperation): void {
    if (operation.settled) return;
    operation.settled = true;
    const error = staleSurfaceError("browser_navigation_superseded");
    for (const waiter of operation.waiters) waiter.reject(error);
    operation.waiters.clear();
  }

  private isCurrentNavigationOperation(
    record: BrowserSurfaceRecord,
    operation: NavigationOperation,
    generation = operation.generation,
  ): boolean {
    return record.navigationOperation === operation
      && record.navigationGeneration === operation.generation
      && generation === operation.generation;
  }

  private waitForNavigationOperation(
    record: BrowserSurfaceRecord,
    operation: NavigationOperation,
    timeoutMs?: number,
  ): Promise<void> {
    if (operation.completed) return Promise.resolve();
    if (operation.failed) return Promise.reject(staleSurfaceError("browser_navigation_failed"));
    if (operation.settled) return Promise.reject(staleSurfaceError("browser_navigation_superseded"));
    const waiter = new Promise<void>((resolve, reject) => {
      operation.waiters.add({ resolve, reject });
    });
    return withNavigationTimeout(waiter, clampNavigationTimeout(timeoutMs)).catch((error) => {
      if (this.isCurrentNavigationOperation(record, operation) && !operation.settled) {
        this.failNavigationOperation(record, operation, toError(error));
      }
      throw error;
    });
  }

  private completeNavigationOperation(record: BrowserSurfaceRecord, operation: NavigationOperation): void {
    if (!this.isCurrentNavigationOperation(record, operation) || operation.settled || operation.failed) return;
    operation.completed = true;
    operation.settled = true;
    record.priming = false;
    // 页面文档生命周期与 viewport commit 相互独立。标记投影必须在
    // 主文档事务真正完成后重新触发，不能只依赖某一次物理内容槽提交，
    // 否则刷新时 Worker 可能在新 isolated world 建立前错过唯一投影机会。
    this.#onDocumentReady?.(this.binding(record));
    this.applySlot(
      record,
      record.slotVisible ? record.slotBounds : null,
      this.#windows.get(record.windowId),
    );
    this.scheduleViewportCommit(record);
    if (operation.loadingEmitted) {
      this.#onEvent({
        type: "loading_changed",
        binding: this.binding(record),
        loading: false,
      });
    }
    for (const waiter of operation.waiters) waiter.resolve();
    operation.waiters.clear();
    this.startDebuggerInitialization(record);
  }

  private completeWhenMainFrameSettled(record: BrowserSurfaceRecord, operation: NavigationOperation): void {
    if (
      !this.isCurrentNavigationOperation(record, operation)
      || operation.failed
      || operation.completed
      || !operation.committed
      || !operation.committedUrl
      || !operation.loadingStarted
      || !sameNavigationUrl(operation.committedUrl, record.contents.getURL() || "")
      || !(
        operation.frameFinished
        || operation.documentFinished
        || operation.stopEventSequence !== null
      )
    ) return;
    // Electron 的 did-finish-load、did-frame-finish-load 和 did-stop-loading
    // 都可能先后到达，且不同 Chromium 页面对三者的顺序并不完全一致。
    // 主文档已经 commit 且任一主帧完成事件到达时，导航就已具备可操作性；
    // 再等待另一个事件会把正常页面误判为超时，永久锁住 Renderer 工具栏。
    this.completeNavigationOperation(record, operation);
  }

  private failNavigationOperation(
    record: BrowserSurfaceRecord,
    operation: NavigationOperation,
    error: Error,
  ): void {
    if (!this.isCurrentNavigationOperation(record, operation) || operation.settled || operation.completed) return;
    operation.failed = true;
    operation.settled = true;
    record.priming = false;
    // 导航失败不是 Surface 失效。保留真实 WebContentsView，让 Chromium
    // 自己展示当前文档或错误页，避免错误状态把右栏收敛成黑屏。
    this.applySlot(
      record,
      record.slotVisible ? record.slotBounds : null,
      this.#windows.get(record.windowId),
    );
    this.publishNavigationFailure(record, error.message);
    this.scheduleViewportCommit(record);
    if (operation.loadingEmitted) {
      this.#onEvent({
        type: "loading_changed",
        binding: this.binding(record),
        loading: false,
      });
    }
    for (const waiter of operation.waiters) waiter.reject(error);
    operation.waiters.clear();
    this.startDebuggerInitialization(record);
  }

  private completeNavigationFromSettledEvent(
    record: BrowserSurfaceRecord,
    operation: NavigationOperation,
    url: string,
    eventSequence: number,
  ): void {
    if (!this.isCurrentNavigationOperation(record, operation) || operation.settled || operation.failed) return;
    const committedUrl = url || operation.targetUrl;
    if (!committedUrl) return;
    operation.started = true;
    operation.awaitingStart = false;
    operation.loadingStarted = operation.kind !== "in-page";
    operation.committed = true;
    operation.frameNavigated = true;
    operation.committedUrl = committedUrl;
    operation.commitEventSequence ??= eventSequence;
    operation.documentFinished = true;
    operation.finishEventSequence ??= eventSequence;
    this.completeNavigationOperation(record, operation);
    this.publishPageForNavigation(record, operation);
  }

  private waitForCurrentNavigation(record: BrowserSurfaceRecord): Promise<void> {
    return (async () => {
      // 一个旧导航可能在等待期间被新导航替换。debugger 初始化不能在旧
      // operation 被 supersede 后直接继续，否则 CDP 会附着到尚未完成的新
      // 文档并把两轮生命周期混在一起；必须重新读取当前 generation。
      while (!record.closed && !record.contents.isDestroyed()) {
        const operation = record.navigationOperation;
        if (!operation || operation.settled) return;
        const generation = operation.generation;
        try {
          await this.waitForNavigationOperation(record, operation);
        } catch {
          // 导航失败仍保留真实 WebContents，debugger 可以附着当前错误页；
          // supersede 则转到最新 operation，不能让旧 waiter 提前放行。
        }
        if (record.navigationOperation !== operation || record.navigationGeneration !== generation) continue;
        return;
      }
    })();
  }

  private claimNavigationOperation(
    record: BrowserSurfaceRecord,
    kind: NavigationKind,
    url: string,
    frame: NavigationFrameIdentity,
    eventSequence = this.nextNavigationEventSequence(record),
  ): NavigationOperation | null {
    const operation = record.navigationOperation;
    // did-frame-navigate 只能确认由当前 did-start-navigation 建立的操作。
    // 已结束操作收到迟到事件时绝不能重新创建 generation，否则旧导航会
    // 被误认成新导航并覆盖当前页面状态。
    if (!operation || operation.settled || !this.isCurrentNavigationOperation(record, operation)) return null;
    if (operation.kind !== kind && !(kind === "document" && operation.kind !== "in-page")) return null;
    if (operation.frame && !sameFrame(operation.frame, frame)) return null;
    if (operation.awaitingStart) {
      if (operation.targetUrl && !sameNavigationUrl(operation.targetUrl, url)) return null;
      operation.startEventSequence ??= eventSequence;
      operation.awaitingStart = false;
      operation.started = true;
      operation.loadingStarted = true;
    }
    operation.frame ??= frame;
    return operation;
  }

  private publishPageForNavigation(
    record: BrowserSurfaceRecord,
    operation: NavigationOperation,
    allowRepeat = false,
  ): void {
    if (
      record.closed
      || !this.isCurrentNavigationOperation(record, operation)
      || operation.failed
      || !operation.committed
      || operation.pagePublished && !allowRepeat
    ) return;
    operation.pagePublished = true;
    this.#onEvent({
      type: "page_updated",
      binding: this.binding(record),
      page: this.pageState(record),
    });
  }

  private async loadPopupInCurrentPage(
    record: BrowserSurfaceRecord,
    url: string,
    details: HandlerDetails,
  ): Promise<void> {
    const postBody = details.postBody;
    const contentType = postBody?.boundary
      ? `${postBody.contentType}; boundary=${postBody.boundary}`
      : postBody?.contentType;
    await this.loadPage(
      record,
      url,
      DEFAULT_NAVIGATION_TIMEOUT_MS,
      undefined,
      () => record.contents.loadURL(url, {
        httpReferrer: details.referrer,
        ...(postBody ? { postData: postBody.data } : {}),
        ...(contentType ? { extraHeaders: `Content-Type: ${contentType}` } : {}),
      }),
    );
  }

  private unmountSurface(record: BrowserSurfaceRecord, window: BaseWindow | undefined): void {
    // 只解绑原生 View，不关闭 WebContents。这样切换 Tab、面板或失败恢复
    // 都不会让后台页面进入命中树，也不会因为反复改成 0x0 而触发 Chromium
    // viewport 重排；重新获得内容槽时 applySlot 会复用同一个 WebContents。
    this.detachSurface(record, window);
  }

  private detachSurface(record: BrowserSurfaceRecord, _window?: BaseWindow): void {
    // slotVisible/slotBounds 是一次绑定租约的状态。解绑时必须同时失效，
    // 并递增租约代次；崩溃恢复只能重新挂载它自己捕获的那一份租约，不能
    // 把面板切换后已经失效的旧 Surface 又挂回当前右栏。
    const hadSlotLease = record.slotVisible || record.slotBounds !== null || record.mounted;
    if (hadSlotLease) record.slotLeaseRevision += 1;
    this.invalidateViewportCommit(record, "slot-unmounted");
    record.slotVisible = false;
    record.slotBounds = null;
    this.stopInspectForLifecycle(record, "surface-unmounted");
    if (!record.mounted) return;
    try {
      record.view.setVisible(false);
    } catch {
      // WebContentsView 可能已随宿主窗口销毁。
    }
    try {
      this.#contentRoots.get(record.windowId)?.removeChildView(record.view);
    } catch {
      // destroyed 与 closeRecord 可能交错到达，解绑必须幂等。
    }
    record.mounted = false;
  }

  private isRenderable(record: BrowserSurfaceRecord): record is BrowserSurfaceRecord & {
    slotBounds: Rectangle;
  } {
    return !record.closed
      && !record.contents.isDestroyed()
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
    if (this.#downloadCleanupInProgress || !record || record.closed || !this.#downloadRoot) {
      // Downloads never fall back to the user's Downloads directory. A
      // browser download is either stored in Magi's private data directory or
      // explicitly rejected, so the host cannot write outside its boundary.
      event.preventDefault();
      if (record && !record.closed) {
        this.emitDownload(
          record,
          "download",
          "interrupted",
          this.#downloadCleanupInProgress
            ? "browser_download_cleanup_in_progress"
            : "browser_download_storage_unavailable",
        );
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
    this.#activeDownloads.add(item);
    item.on("updated", (_event, state) => {
      if (!record.closed) {
        this.emitDownload(record, filename, state === "progressing" ? "progressing" : "interrupted", undefined, downloadBytes(item));
      }
    });
    item.once("done", (_event, state) => {
      this.#activeDownloads.delete(item);
      if (!record.closed) this.emitDownload(record, filename, state, undefined, downloadBytes(item));
    });
  }

  private async performDownloadCleanup(): Promise<void> {
    this.#downloadCleanupInProgress = true;
    this.cancelActiveDownloads();
    try {
      await clearBrowserDownloads(this.#downloadUserDataPath!);
    } finally {
      this.#downloadCleanupInProgress = false;
    }
  }

  private cancelActiveDownloads(): void {
    for (const item of this.#activeDownloads) {
      try {
        item.cancel();
      } catch (error) {
        console.warn("[BrowserSurfaceManager] 取消浏览器下载失败", error);
      }
    }
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

  private scheduleInspectHighlight(
    record: BrowserSurfaceRecord,
    x: number,
    y: number,
  ): void {
    if (!Number.isFinite(x) || !Number.isFinite(y) || !record.inspectActive) return;
    record.inspectHoverPoint = { x: Math.max(0, Math.floor(x)), y: Math.max(0, Math.floor(y)) };
    if (record.inspectHoverPromise) return;
    const generation = record.inspectGeneration;
    const hover = (async () => {
      // 合并同一帧内的高频 mouseMove，只处理最新坐标，避免 CDP lane
      // 因指针移动积压并拖慢点击选择。
      await new Promise<void>((resolve) => setTimeout(resolve, 16));
      while (this.isInspectGenerationActive(record, generation) && record.inspectHoverPoint) {
        const point = record.inspectHoverPoint;
        record.inspectHoverPoint = null;
        await this.enqueueCdp(record, async ({ track, debuggerLease }) => {
          if (!this.isInspectGenerationActive(record, generation)) return;
          const lease = debuggerLease();
          const hit = await this.sendSurfaceCdpCommand(
            record,
            "DOM.getNodeForLocation",
            {
              x: point.x,
              y: point.y,
              includeUserAgentShadowDOM: true,
              ignorePointerEventsNone: true,
            },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            lease,
            track,
          ) as { backendNodeId?: unknown };
          const backendNodeId = normalizeOptionalDomNodeId(hit.backendNodeId);
          if (backendNodeId === null || !this.isInspectGenerationActive(record, generation)) return;
          await this.sendSurfaceCdpCommand(
            record,
            "Overlay.highlightNode",
            { backendNodeId, highlightConfig: INSPECT_HIGHLIGHT_CONFIG },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            lease,
            track,
          );
        });
      }
    })();
    record.inspectHoverPromise = hover;
    void hover.catch((error) => {
      if (!record.closed && record.inspectActive && record.inspectGeneration === generation) {
        console.warn("[BrowserSurfaceManager] 节点悬停高亮失败", {
          surfaceId: record.surfaceId,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    }).finally(() => {
      if (record.inspectHoverPromise === hover) record.inspectHoverPromise = null;
      if (record.inspectActive && record.inspectGeneration === generation && record.inspectHoverPoint) {
        const point = record.inspectHoverPoint;
        this.scheduleInspectHighlight(record, point.x, point.y);
      }
    });
  }

  private handleInspectPointRequested(
    record: BrowserSurfaceRecord,
    x: number,
    y: number,
  ): void {
    if (
      record.closed
      || !record.primary
      || !this.#surfaces.isPrimary(record)
      || !record.inspectActive
      || record.contents.isDestroyed()
      || !this.isRenderable(record)
      || !record.contents.debugger.isAttached()
      || !Number.isFinite(x)
      || !Number.isFinite(y)
    ) return;
    const generation = record.inspectGeneration;
    const commandSessionId = undefined;
    let requestedBackendNodeId: number | null = null;
    void this.enqueueCdp(record, async ({ track, debuggerLease }) => {
      if (!this.isInspectGenerationActive(record, generation)) return;
      const lease = debuggerLease(commandSessionId);
      const hit = await this.sendSurfaceCdpCommand(
        record,
        "DOM.getNodeForLocation",
        {
          x: Math.max(0, Math.floor(x)),
          y: Math.max(0, Math.floor(y)),
          includeUserAgentShadowDOM: true,
          ignorePointerEventsNone: true,
        },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
        track,
      ) as { backendNodeId?: unknown };
      const backendNodeId = normalizeOptionalDomNodeId(hit.backendNodeId);
      if (backendNodeId === null || !this.isInspectGenerationActive(record, generation)) return;
      requestedBackendNodeId = backendNodeId;
      const description = await this.sendSurfaceCdpCommand(
        record,
        "DOM.describeNode",
        { backendNodeId, depth: 0, pierce: true },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
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
        lease,
        track,
      );
      if (!frameId || !this.isInspectGenerationActive(record, generation)) return;

      let attributeList = Array.isArray(node.attributes) ? node.attributes : [];
      const nodeId = normalizeOptionalDomNodeId(node.nodeId);
      if (nodeId !== null && this.isInspectGenerationActive(record, generation)) {
        try {
          const attributes = await this.sendSurfaceCdpCommand(
            record,
            "DOM.getAttributes",
            { nodeId },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            lease,
            track,
          ) as { attributes?: unknown };
          if (!this.isInspectGenerationActive(record, generation)) return;
          if (Array.isArray(attributes.attributes)) attributeList = attributes.attributes;
        } catch {
          // describeNode 已经携带属性时，单独的 getAttributes 失败不应丢弃
          // 本次真实节点选择。页面导航竞态由 generation 校验负责收口。
        }
      }

      let outerHtml = "";
      if (this.isInspectGenerationActive(record, generation)) {
        try {
          const outer = await this.sendSurfaceCdpCommand(
            record,
            "DOM.getOuterHTML",
            { backendNodeId },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            lease,
            track,
          ) as { outerHTML?: unknown };
          if (!this.isInspectGenerationActive(record, generation)) return;
          if (typeof outer.outerHTML === "string") outerHtml = outer.outerHTML;
        } catch {
          // 节点仍然有效时，允许调用方使用结构化描述和属性继续处理。
        }
      }

      let textExcerpt = "";
      if (this.isInspectGenerationActive(record, generation)) {
        textExcerpt = await this.readInspectNodeText(
          record,
          backendNodeId,
          commandSessionId,
          generation,
          lease,
          track,
        );
      }

      let bounds: { x: number; y: number; width: number; height: number } | null = null;
      if (this.isInspectGenerationActive(record, generation)) {
        try {
          const box = await this.sendSurfaceCdpCommand(
            record,
            "DOM.getBoxModel",
            { backendNodeId },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            lease,
            track,
          ) as CdpBoxModelResponse;
          if (!this.isInspectGenerationActive(record, generation)) return;
          bounds = quadBounds(box.model?.border);
        } catch {
          // 文本节点、不可布局节点和导航中的节点可能没有 box model。
        }
      }

      if (!this.isInspectGenerationActive(record, generation)) return;
      const pageUrl = record.contents.getURL() || "about:blank";
      const inspectedNode: BrowserInspectedNodeContext = {
        browser_session_id: record.browserSessionId,
        backend_node_id: backendNodeId,
        node_id: nodeId,
        frame_id: frameId,
        node_type: finiteInteger(node.nodeType, 0),
        node_name: typeof node.nodeName === "string" ? node.nodeName : "",
        local_name: typeof node.localName === "string" ? node.localName : "",
        // DOM.Node.nodeValue 对 Element 节点按协议就是 null。这里通过
        // Chromium Runtime 在真实节点上读取 textContent，保留元素按钮、
        // 标题和容器的可见语义；读取失败时仍保留原始 nodeValue。
        node_value: textExcerpt || (typeof node.nodeValue === "string" ? node.nodeValue : ""),
        child_node_count: integerOrNull(node.childNodeCount),
        document_url: typeof node.documentURL === "string" ? node.documentURL : null,
        attributes: parseNodeAttributes(attributeList),
        ...truncateOuterHtml(outerHtml),
        bounds,
        page_url: pageUrl,
        page_title: record.contents.getTitle() || "",
      };
      // Inspect 是一次性选择事务。先使本次代次失效，再把结果发给
      // Renderer；清理命令放到当前 CDP lane 完成后执行，避免在节点采集
      // 事务内部再次入队形成自等待。这样 Renderer 不需要等待停止命令，
      // 选择结果可以立即再次触发下一次 Inspect。
      const selectionGeneration = ++record.inspectGeneration;
      record.inspectActive = false;
      record.inspectHoverPoint = null;
      this.#onEvent({
        type: "node_inspected",
        binding: this.binding(record),
        node: inspectedNode,
      });
      setImmediate(() => {
        if (
          record.closed
          || record.inspectActive
          || record.inspectGeneration !== selectionGeneration
        ) return;
        this.stopInspectForLifecycle(record, "node-selected");
      });
    }).catch((error) => {
      if (!record.closed && record.inspectActive && record.inspectGeneration === generation) {
        console.warn("[BrowserSurfaceManager] 真实 DOM 节点采集失败", {
          surfaceId: record.surfaceId,
          backendNodeId: requestedBackendNodeId,
          error: error instanceof Error ? error.message : String(error),
        });
      }
    });
  }

  private async readInspectNodeText(
    record: BrowserSurfaceRecord,
    backendNodeId: number,
    sessionId: string | undefined,
    generation: number,
    lease: DebuggerSessionLease,
    track: (promise: Promise<unknown>) => void,
  ): Promise<string> {
    if (!this.isInspectGenerationActive(record, generation)) return "";
    const objectGroup = "magi-inspect-text";
    try {
      const resolved = await this.sendSurfaceCdpCommand(
        record,
        "DOM.resolveNode",
        { backendNodeId, objectGroup },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
        track,
      ) as { object?: { objectId?: unknown } };
      const objectId = resolved.object?.objectId;
      if (typeof objectId !== "string" || objectId.length === 0 || !this.isInspectGenerationActive(record, generation)) {
        return "";
      }
      const response = await this.sendSurfaceCdpCommand(
        record,
        "Runtime.callFunctionOn",
        {
          objectId,
          objectGroup,
          functionDeclaration: "function () { const value = typeof this.textContent === 'string' ? this.textContent : ''; return value.replace(/\\s+/g, ' ').trim().slice(0, 4096); }",
          returnByValue: true,
          awaitPromise: false,
        },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
        track,
      ) as { result?: { value?: unknown } };
      if (!this.isInspectGenerationActive(record, generation)) return "";
      const value = response.result?.value;
      return typeof value === "string"
        ? value.replace(/\s+/gu, " ").trim().slice(0, MAX_INSPECTED_TEXT_LENGTH)
        : "";
    } catch {
      // Element 文本是增强上下文；页面导航或节点失效时不丢弃本次
      // 结构化选择，调用方仍可使用属性和 outerHTML。
      return "";
    } finally {
      if (!record.contents.isDestroyed() && record.contents.debugger.isAttached()) {
        await this.sendSurfaceCdpCommand(
          record,
          "Runtime.releaseObjectGroup",
          { objectGroup },
          DEFAULT_CDP_COMMAND_TIMEOUT_MS,
          lease,
          track,
        ).catch(() => undefined);
      }
    }
  }

  private async resolveInspectFrameId(
    record: BrowserSurfaceRecord,
    nodeFrameId: unknown,
    backendNodeId: number,
    sessionId: string | undefined,
    generation: number,
    lease: DebuggerSessionLease,
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
      const resolved = await this.sendSurfaceCdpCommand(
        record,
        "DOM.resolveNode",
        { backendNodeId, objectGroup },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
        track,
      ) as { object?: { objectId?: unknown } };
      const objectId = resolved.object?.objectId;
      if (typeof objectId === "string" && objectId.length > 0 && this.isInspectGenerationActive(record, generation)) {
        const frameElement = await this.sendSurfaceCdpCommand(
          record,
          "Runtime.callFunctionOn",
          {
            objectId,
            objectGroup,
            functionDeclaration: "function () { return this.ownerDocument?.defaultView?.frameElement || null; }",
            returnByValue: false,
            awaitPromise: false,
          },
          DEFAULT_CDP_COMMAND_TIMEOUT_MS,
          lease,
          track,
        ) as { result?: { objectId?: unknown; subtype?: unknown; } };
        if (!this.isInspectGenerationActive(record, generation)) return null;
        const frameElementObjectId = frameElement.result?.objectId;
        if (typeof frameElementObjectId === "string" && frameElementObjectId.length > 0) {
          const describedFrameElement = await this.sendSurfaceCdpCommand(
            record,
            "DOM.describeNode",
            { objectId: frameElementObjectId, depth: 0, pierce: false },
            DEFAULT_CDP_COMMAND_TIMEOUT_MS,
            lease,
            track,
          ) as { node?: CdpNodeDescription };
          if (!this.isInspectGenerationActive(record, generation)) return null;
          const frameId = describedFrameElement.node?.frameId;
          if (typeof frameId === "string" && frameId.length > 0) return frameId;
        }
      }

      if (!this.isInspectGenerationActive(record, generation)) return null;
      const response = await this.sendSurfaceCdpCommand(
        record,
        "Page.getFrameTree",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
        track,
      ) as {
        frameTree?: { frame?: { id?: unknown } };
      };
      if (!this.isInspectGenerationActive(record, generation)) return null;
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
        await this.sendSurfaceCdpCommand(
          record,
          "Runtime.releaseObjectGroup",
          { objectGroup },
          DEFAULT_CDP_COMMAND_TIMEOUT_MS,
          lease,
          track,
        ).catch(() => undefined);
      }
    }
  }

  private installSurfacePolicy(record: BrowserSurfaceRecord): void {
    const { contents: webContents } = record;
    // 页面中的 webview element 是另一种 guest WebContents 创建入口。即使
    // webviewTag 默认关闭，也在生命周期层明确拒绝，保证单页单 Target
    // 不依赖 Electron 的默认值。
    webContents.on("will-attach-webview", (event) => {
      event.preventDefault();
      console.warn("[BrowserSurfaceManager] 拒绝浏览器页面嵌套 WebView", {
        tabId: record.tabId,
        surfaceId: record.surfaceId,
      });
    });
    webContents.setWindowOpenHandler((details) => {
      // 这是 Chromium 创建新 WebContents 前的唯一边界。右栏 Browser Tab
      // 只有一个顶层页面：合法的网页弹窗请求转成当前页导航，禁止创建
      // 第二个 WebContents、BrowserWindow 或 Magi 子 Tab。
      try {
        const url = normalizePopupNavigationUrl(details.url);
        void this.loadPopupInCurrentPage(record, url, details).catch((error) => {
          if (!record.closed) {
            console.warn("[BrowserSurfaceManager] 当前 Browser Tab 接管 popup 导航失败", {
              tabId: record.tabId,
              surfaceId: record.surfaceId,
              url,
              error: error instanceof Error ? error.message : String(error),
            });
          }
        });
      } catch {
        // about:blank、脚本协议和其他不受信任的 popup 不能转移到当前页。
        this.#onEvent({
          type: "popup_blocked",
          binding: this.binding(record),
          url: details.url,
        });
      }
      return { action: "deny" };
    });
    webContents.on("will-navigate", (event, url) => {
      try {
        normalizeNavigableUrl(url);
      } catch {
        event.preventDefault();
      }
    });
    webContents.on("did-start-navigation", (details) => {
      if (!details.isMainFrame) return;
      const eventSequence = this.nextNavigationEventSequence(record);
      const frame = frameIdentity(details.frame);
      let operation = record.navigationOperation;
      const belongsToPendingOperation = Boolean(
        operation
        && !operation.settled
        && operation.awaitingStart
        && (!operation.targetUrl || sameNavigationUrl(operation.targetUrl, details.url))
        && (!operation.frame || !frame || sameFrame(operation.frame, frame)),
      );
      if (!belongsToPendingOperation) {
        // Every new start event establishes a new operation. In particular,
        // same-URL reloads must not be mistaken for a duplicate of the prior
        // operation; all later events are fenced by this generation.
        operation = this.beginNavigationOperation(
          record,
          details.isSameDocument ? "in-page" : "document",
          details.url,
          eventSequence,
        );
      }
      if (!operation || !this.isCurrentNavigationOperation(record, operation)) return;
      operation.startEventSequence ??= eventSequence;
      operation.awaitingStart = false;
      operation.started = true;
      operation.loadingStarted = !details.isSameDocument;
      operation.frame ??= frame;
      if (details.isSameDocument) {
        operation.kind = "in-page";
        record.priming = false;
      }
    });
    webContents.on("did-start-loading", () => {
      const claim = this.claimNavigationEvent(record, {
        generation: record.navigationGeneration,
        phase: "start-loading",
      });
      if (!claim) return;
      // Chromium 不保证 reload/history 一定先发 did-start-navigation。
      // did-start-loading 本身已经是当前事务的启动证据：记录它可以让
      // 后续 did-frame-finish-load/did-finish-load 正常收敛，而不把工具链
      // 永久阻塞到导航超时。保留 awaitingStart 让迟到的
      // did-start-navigation 仍能复用同一个事务，而不是再创建一代。
      claim.operation.started = true;
      claim.operation.loadingStarted = true;
      claim.operation.startEventSequence ??= claim.sequence;
    });
    webContents.on("did-frame-navigate", (_event, url, _httpResponseCode, _httpStatusText, isMainFrame, frameProcessId, frameRoutingId) => {
      if (!isMainFrame) return;
      const eventSequence = this.nextNavigationEventSequence(record);
      const operation = this.claimNavigationOperation(
        record,
        "document",
        url,
        { processId: frameProcessId, routingId: frameRoutingId },
        eventSequence,
      );
      if (!operation) return;
      operation.frameNavigated = true;
      operation.committed = true;
      operation.committedUrl = url;
      operation.commitEventSequence ??= eventSequence;
      this.publishPageForNavigation(record, operation);
    });
    webContents.on("did-navigate", (_event, url) => {
      const claim = this.claimNavigationEvent(record, {
        generation: record.navigationGeneration,
        phase: "commit",
        url,
      });
      if (!claim) return;
      const { operation, sequence } = claim;
      if (!operation.committed) {
        operation.committed = true;
        operation.committedUrl = url;
        operation.commitEventSequence = sequence;
      }
      operation.frameNavigated = true;
      this.publishPageForNavigation(record, operation);
    });
    webContents.on("did-navigate-in-page", (_event, url, isMainFrame, frameProcessId, frameRoutingId) => {
      if (!isMainFrame) return;
      const frame = { processId: frameProcessId, routingId: frameRoutingId };
      const claim = this.claimNavigationEvent(record, {
        generation: record.navigationGeneration,
        phase: "commit",
        url,
        frame,
      });
      if (!claim || claim.operation.kind !== "in-page") return;
      const { operation, sequence } = claim;
      operation.frame ??= frame;
      operation.frameNavigated = true;
      operation.committed = true;
      operation.committedUrl = url;
      operation.commitEventSequence ??= sequence;
      this.completeNavigationOperation(record, operation);
      this.publishPageForNavigation(record, operation);
    });
    webContents.on("did-frame-finish-load", (_event, isMainFrame, frameProcessId, frameRoutingId) => {
      if (!isMainFrame) return;
      const claim = this.claimNavigationEvent(record, {
        generation: record.navigationGeneration,
        phase: "frame-finish",
        frame: { processId: frameProcessId, routingId: frameRoutingId },
      });
      if (!claim) return;
      const { operation, sequence } = claim;
      if (!operation.committed) {
        this.completeNavigationFromSettledEvent(
          record,
          operation,
          webContents.getURL() || operation.targetUrl || "",
          sequence,
        );
        return;
      }
      operation.frameFinished = true;
      operation.frameFinishEventSequence = sequence;
      this.completeWhenMainFrameSettled(record, operation);
    });
    webContents.on("did-finish-load", () => {
      const claim = this.claimNavigationEvent(record, {
        generation: record.navigationGeneration,
        phase: "finish",
      });
      if (!claim) return;
      const { operation, sequence } = claim;
      if (!operation.loadingStarted) return;
      if (!operation.committed) {
        this.completeNavigationFromSettledEvent(
          record,
          operation,
          webContents.getURL() || operation.targetUrl || "",
          sequence,
        );
        return;
      }
      operation.finishEventSequence = sequence;
      operation.documentFinished = true;
      this.completeWhenMainFrameSettled(record, operation);
      // 某些 Chromium 页面会先结束文档事件，再由导航状态机完成最终
      // operation；无论事件顺序如何，文档已可绘制时都要重新唤醒当前
      // viewport commit，不能把加载期 requested commit 留到下一次 resize。
      if (!record.contents.isLoadingMainFrame() && !record.closed) {
        this.scheduleViewportCommit(record);
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
    webContents.on("page-title-updated", (_event, _title) => {
      const claim = this.claimNavigationEvent(record, {
        generation: record.navigationGeneration,
        phase: "title",
        url: webContents.getURL() || "",
        allowSettled: true,
      });
      if (!claim) return;
      const { operation } = claim;
      if (!operation.committed || !operation.committedUrl || record.priming) return;
      if (!sameNavigationUrl(operation.committedUrl, webContents.getURL() || "")) return;
      this.publishPageForNavigation(record, operation, true);
    });
    webContents.on("did-stop-loading", () => {
      const claim = this.claimNavigationEvent(record, {
        generation: record.navigationGeneration,
        phase: "stop",
      });
      if (!claim) return;
      const { operation, sequence } = claim;
      if (record.contents.isLoadingMainFrame()) return;
      if (!operation.committed) {
        this.completeNavigationFromSettledEvent(
          record,
          operation,
          webContents.getURL() || operation.targetUrl || "",
          sequence,
        );
        return;
      }
      if (
        !operation.committedUrl
        || !operation.loadingStarted
        || !sameNavigationUrl(operation.committedUrl, webContents.getURL() || "")
      ) return;
      operation.stopEventSequence = sequence;
      this.completeWhenMainFrameSettled(record, operation);
      if (!record.contents.isLoadingMainFrame() && !record.closed) {
        this.scheduleViewportCommit(record);
      }
    });
    webContents.on("did-fail-load", (_event, errorCode, errorDescription, validatedURL, isMainFrame, frameProcessId, frameRoutingId) => {
      if (record.closed || !isMainFrame) return;
      const claim = this.claimNavigationEvent(record, {
        generation: record.navigationGeneration,
        phase: "fail",
        frame: { processId: frameProcessId, routingId: frameRoutingId },
      });
      if (!claim) return;
      const { operation } = claim;
      // ERR_ABORTED 是旧导航被新导航替换时的正常结果，不能结束当前
      // operation。其余失败必须与当前 operation 的目标或已提交地址对应。
      if (errorCode === -3) return;
      if (
        validatedURL
        && operation.committedUrl
        && !sameNavigationUrl(operation.committedUrl, validatedURL)
      ) return;
      if (
        validatedURL
        && !operation.committedUrl
        && operation.targetUrl
        && !sameNavigationUrl(operation.targetUrl, validatedURL)
      ) return;
      operation.frame ??= { processId: frameProcessId, routingId: frameRoutingId };
      this.failNavigationOperation(
        record,
        operation,
        new Error(errorDescription || `net_error_${errorCode}`),
      );
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
    webContents.on("before-mouse-event", (event, input) => {
      if (record.closed || record.automationInputDepth > 0) return;

      // 节点选择使用 WebContentsView 的真实鼠标坐标和 Chromium DOM 命中。
      // Electron 不保证独立 WebContentsView 发出 DevTools 前端节点选择事件，
      // 因此必须在页面分发前拦截完整点击手势，避免选择动作穿透到网页。
      if (record.inspectActive || record.inspectGestureActive) {
        if (record.inspectActive && input.type === "mouseMove") {
          this.scheduleInspectHighlight(record, input.x, input.y);
        } else if (
          record.inspectActive
          && input.type === "mouseDown"
          && (input.button === undefined || input.button === "left")
        ) {
          event.preventDefault();
          record.inspectGestureActive = true;
          this.handleInspectPointRequested(record, input.x, input.y);
        } else if (record.inspectGestureActive && input.type === "mouseUp") {
          event.preventDefault();
          record.inspectGestureActive = false;
        }
        return;
      }

      if (
        // 普通浏览模式下的真实输入才表示用户接管；节点选择已在上方
        // 单独消费，不得改变 Browser Tab 的控制权状态。
        !["mouseMove", "mouseDown", "contextMenu", "mouseWheel"].includes(input.type)
      ) return;
      this.promote(record.surfaceId);
      void this.setAgentCursor(record, false, null, null, null).catch(() => undefined);
      this.#onEvent({ type: "user_takeover", binding: this.binding(record) });
    });
    webContents.on("render-process-gone", (_event, details) => {
      if (record.closed) return;
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
        await this.withSurfaceLifecycle(record, record.recoveryPromise);
      } catch {
        // Recovery owns its terminal error state. The command below performs
        // the final lifecycle validation and returns a stable surface error.
      }
    }
    if (record.closed || record.contents.isDestroyed()) {
      throw staleSurfaceError("browser_surface_not_found");
    }
    const debuggerReady = record.debuggerReadyPromise;
    if (debuggerReady) {
      try {
        await this.withSurfaceLifecycle(record, debuggerReady);
      } catch {
        // A transient attach failure must not poison the Surface forever.
      } finally {
        if (record.debuggerReadyPromise === debuggerReady) record.debuggerReadyPromise = null;
      }
    }
    if (
      !record.contents.debugger.isAttached()
      || !record.debuggerSessionInitialized
      || !record.debuggerListenersInstalled
    ) {
      await this.reconnectDebugger(record, "on-demand");
    }
    if (record.closed || record.contents.isDestroyed()) {
      throw staleSurfaceError("browser_surface_not_found");
    }
    if (
      !record.contents.debugger.isAttached()
      || !record.debuggerSessionInitialized
      || !record.debuggerListenersInstalled
    ) {
      throw staleSurfaceError("browser_debugger_detached");
    }
  }

  private async attachDebugger(record: BrowserSurfaceRecord): Promise<void> {
    const debuggerApi = record.contents.debugger;
    if (
      debuggerApi.isAttached()
      && record.debuggerSessionInitialized
      && record.debuggerListenersInstalled
    ) return;
    try {
      if (!debuggerApi.isAttached()) debuggerApi.attach("1.3");
      if (!record.debuggerListenersInstalled) this.installDebuggerListeners(record);
      const sessionGeneration = record.debuggerSessionGeneration;
      const lease = this.createDebuggerSessionLease(record, undefined, sessionGeneration);
      record.debuggerSessionInitialized = false;
      // Runtime.addBinding 在 Electron 中需要先显式启用 Runtime domain；
      // 每次新的 debugger session 都完整执行一遍初始化，不复用 detach
      // 前的 domain、binding、isolated world 或 Target session 状态。
      await this.sendSurfaceCdpCommand(record, "Page.enable", {}, DEFAULT_CDP_COMMAND_TIMEOUT_MS, lease);
      await this.sendSurfaceCdpCommand(record, "Runtime.enable", {}, DEFAULT_CDP_COMMAND_TIMEOUT_MS, lease);
      await this.sendSurfaceCdpCommand(
        record,
        "Runtime.addBinding",
        { name: DIALOG_BRIDGE_BINDING },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
      );
      await this.sendSurfaceCdpCommand(
        record,
        "Page.addScriptToEvaluateOnNewDocument",
        { source: DIALOG_BRIDGE_SCRIPT },
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
      );
      record.dialogBridgeInstalled = true;
      if (!record.contents.isLoadingMainFrame()) {
        await record.contents.executeJavaScript(DIALOG_BRIDGE_SCRIPT, true);
        lease.assertCurrent();
      }
      lease.assertCurrent();
      record.debuggerSessionInitialized = true;
      // attach 完成后重新提交当前物理内容槽。初始加载可能早于 debugger
      // 握手结束，viewport commit 必须由这条真实生命周期补齐，不能依赖
      // 下一次尺寸变化才能解除 waitForViewportCommit。
      this.scheduleViewportCommit(record);
    } catch (error) {
      // Page/Runtime domain 初始化只要有一步失败，当前 session 就不能继续
      // 复用。先解绑监听器再 detach，避免清理动作触发第二条重连链；外层
      // reconnectDebugger 会保留单飞 Promise 并安排退避重试。
      this.resetDebuggerSession(record);
      if (!record.contents.isDestroyed() && debuggerApi.isAttached()) {
        try {
          debuggerApi.detach();
        } catch {
          // renderer 销毁竞态下 detach 可能同步失败，重连仍会重新 attach。
        }
      }
      throw error;
    }
  }

  private assertDebuggerSessionCurrent(record: BrowserSurfaceRecord, generation: number): void {
    if (
      record.closed
      || record.contents.isDestroyed()
      || record.debuggerSessionGeneration !== generation
      || !record.contents.debugger.isAttached()
    ) {
      throw staleSurfaceError("browser_debugger_session_stale");
    }
  }

  private installDebuggerListeners(record: BrowserSurfaceRecord): void {
    const debuggerApi = record.contents.debugger;
    const generation = ++record.debuggerSessionGeneration;
    const messageListener: DebuggerMessageListener = (_event, method, params, sessionId) => {
      if (record.closed || record.debuggerSessionGeneration !== generation) return;
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
      // Lighthouse 需要接收 iframe/OOPIF/Worker 等内部 Target 的 CDP 会话，
      // 但页面型 Target 只能代表 popup/子窗口。它们不属于右栏一级 Browser
      // Tab，不能进入 Worker 的 child session，也不能让任何旁路把它误显示
      // 成子 Tab。收到附着事件后立即从当前根 Target detach，并丢弃该会话
      // 的后续事件；这条边界位于 Electron Main，是 Renderer/Worker 之前的
      // 唯一物理入口。
      if (method === "Target.attachedToTarget" && typeof eventParams.sessionId === "string") {
        const targetInfo = eventParams.targetInfo;
        if (isForbiddenChildPageTarget(targetInfo)) {
          record.blockedCdpSessionIds.add(eventParams.sessionId);
          void this.detachCdpTarget(record, eventParams.sessionId);
          return;
        }
        record.cdpSessionIds.add(eventParams.sessionId);
      }
      if (method === "Target.detachedFromTarget" && typeof eventParams.sessionId === "string") {
        if (record.blockedCdpSessionIds.delete(eventParams.sessionId)) return;
        record.cdpSessionIds.delete(eventParams.sessionId);
      }
      if (sessionId && record.blockedCdpSessionIds.has(sessionId)) return;
      this.#onEvent({
        type: "cdp_event",
        binding: this.binding(record),
        method,
        params: eventParams,
        ...(sessionId ? { sessionId } : {}),
      });
      if (method === "Page.javascriptDialogOpening") {
        this.notifyNativeDialogOpening(record);
      }
    };
    const detachListener: DebuggerDetachListener = (_event, reason) => {
      if (record.closed || record.debuggerSessionGeneration !== generation) return;
      this.resetDebuggerSession(record);
      console.warn("[BrowserSurfaceManager] Browser debugger detached", {
        surfaceId: record.surfaceId,
        tabId: record.tabId,
        reason,
      });
      // detach 只影响自动化通道，不隐藏或 reload 用户正在看的文档；
      // 重连会等待当前导航 operation，再完整重建 CDP session。
      void this.reconnectDebugger(record, `debugger-detached:${reason}`).catch((error) => {
        if (!record.closed) {
          console.error("[BrowserSurfaceManager] Browser Surface 调试器重连失败", {
            surfaceId: record.surfaceId,
            reason,
            error: error instanceof Error ? error.message : String(error),
          });
        }
      });
    };
    debuggerApi.on("message", messageListener);
    debuggerApi.on("detach", detachListener);
    record.debuggerMessageListener = messageListener;
    record.debuggerDetachListener = detachListener;
    record.debuggerListenersInstalled = true;
  }

  private async detachCdpTarget(record: BrowserSurfaceRecord, sessionId: string): Promise<void> {
    if (record.closed || record.contents.isDestroyed() || !record.contents.debugger.isAttached()) return;
    try {
      await record.contents.debugger.sendCommand("Target.detachFromTarget", { sessionId });
    } catch (error) {
      // 拒绝动作已经在 attached 事件边界完成；Target 可能在 detach 前自行
      // 消失，因此这里只记录诊断，不把失败传播为当前一级 Tab 的故障。
      console.warn("[BrowserSurfaceManager] 拒绝页面型子 Target 时目标已消失", {
        tabId: record.tabId,
        surfaceId: record.surfaceId,
        sessionId,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  }

  private removeDebuggerListeners(record: BrowserSurfaceRecord): void {
    const debuggerApi = record.contents.debugger;
    if (record.debuggerMessageListener) debuggerApi.off("message", record.debuggerMessageListener);
    if (record.debuggerDetachListener) debuggerApi.off("detach", record.debuggerDetachListener);
    record.debuggerMessageListener = null;
    record.debuggerDetachListener = null;
    record.debuggerListenersInstalled = false;
  }

  private createNativeDialogOpeningWaiter(record: BrowserSurfaceRecord): {
    promise: Promise<void>;
    cancel: () => void;
  } {
    let active = true;
    let resolvePromise: (() => void) | null = null;
    const waiter = () => {
      if (!active) return;
      active = false;
      record.nativeDialogOpeningWaiters.delete(waiter);
      resolvePromise?.();
    };
    const promise = new Promise<void>((resolve) => {
      resolvePromise = resolve;
      record.nativeDialogOpeningWaiters.add(waiter);
    });
    return {
      promise,
      cancel: () => {
        if (!active) return;
        active = false;
        record.nativeDialogOpeningWaiters.delete(waiter);
      },
    };
  }

  private notifyNativeDialogOpening(record: BrowserSurfaceRecord): void {
    for (const waiter of [...record.nativeDialogOpeningWaiters]) waiter();
  }

  private resetDebuggerSession(record: BrowserSurfaceRecord): void {
    // stopInspectForLifecycle 必须先读取并排队清理 Overlay 资源；下面的
    // 状态清零只负责阻止新请求，不能覆盖这次清理已经捕获的事实。
    this.stopInspectForLifecycle(record, "debugger-detached");
    this.invalidateViewportCommit(record, "debugger-session");
    this.removeDebuggerListeners(record);
    record.debuggerSessionGeneration += 1;
    record.debuggerSessionInitialized = false;
    this.clearDebuggerReconnectTimer(record);
    // 任何 session reset 都必须让旧的 ready promise 失效。新的 attach
    // 由 reconnectDebugger 单独建立，不能让旧 Promise 继续代表已失效的
    // debugger session。
    record.debuggerReadyPromise = null;
    record.dialogBridgeInstalled = false;
    // 等待原生对话框的输入请求绑定在旧 debugger session 上；session
    // 重置后不能让新文档的 opening 事件误唤醒旧请求。
    record.nativeDialogOpeningWaiters.clear();
    record.inspectResourcesEnabled = false;
    record.cdpSessionIds.clear();
    record.blockedCdpSessionIds.clear();
    record.cursorExecutionContextId = null;
    record.viewportLifecycle.applied = null;
  }

  private invalidateDebuggerSession(record: BrowserSurfaceRecord, reason: string): void {
    if (record.closed || record.contents.isDestroyed()) return;
    this.resetDebuggerSession(record);
    if (record.contents.debugger.isAttached()) {
      try {
        record.contents.debugger.detach();
      } catch {
        // Chromium renderer 销毁竞态下 detach 可能同步失败；下一次命令
        // 仍会重新建立完整 debugger session。
      }
    }
    this.scheduleDebuggerReconnect(record, reason, "browser_cdp_timeout");
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
    if (record.closed || record.contents.isDestroyed()) {
      return Promise.reject(staleSurfaceError("browser_surface_not_found"));
    }
    if (record.recoveryPromise) return record.recoveryPromise;
    if (
      record.debuggerSessionInitialized
      && record.contents.debugger.isAttached()
      && record.debuggerListenersInstalled
    ) return Promise.resolve();
    if (record.debuggerReadyPromise) return record.debuggerReadyPromise;
    this.clearDebuggerReconnectTimer(record);
    const reconnect = this.enqueueCdp(record, async () => {
      if (record.closed || record.contents.isDestroyed()) return;
      await this.waitForCurrentNavigation(record);
      if (record.closed || record.contents.isDestroyed()) return;
      await this.attachDebugger(record);
      if (
        !record.debuggerSessionInitialized
        || !record.contents.debugger.isAttached()
        || !record.debuggerListenersInstalled
      ) throw staleSurfaceError("browser_debugger_detached");
      if (record.primary) this.#onEvent({ type: "primary_changed", binding: this.binding(record) });
    });
    record.debuggerReadyPromise = reconnect;
    void reconnect.then(
      () => {
        if (record.debuggerReadyPromise === reconnect) record.debuggerReadyPromise = null;
        record.debuggerReconnectAttempt = 0;
        this.clearDebuggerReconnectTimer(record);
      },
      (error) => {
        if (record.debuggerReadyPromise === reconnect) record.debuggerReadyPromise = null;
        if (!record.closed && !record.contents.isDestroyed()) {
          this.scheduleDebuggerReconnect(
            record,
            reason,
            error instanceof Error ? error.message : String(error),
          );
        }
      },
    );
    return reconnect;
  }

  private scheduleDebuggerReconnect(
    record: BrowserSurfaceRecord,
    reason: string,
    error: string,
  ): void {
    if (
      record.closed
      || record.contents.isDestroyed()
      || record.recoveryPromise
      || record.debuggerReadyPromise
      || record.debuggerSessionInitialized && record.contents.debugger.isAttached()
      || record.debuggerReconnectTimer
    ) return;
    const attempt = record.debuggerReconnectAttempt;
    const delay = Math.min(
      DEBUGGER_RECONNECT_MAX_DELAY_MS,
      DEBUGGER_RECONNECT_INITIAL_DELAY_MS * 2 ** Math.min(attempt, 5),
    );
    record.debuggerReconnectAttempt = Math.min(attempt + 1, 31);
    const timer = setTimeout(() => {
      if (record.debuggerReconnectTimer === timer) record.debuggerReconnectTimer = null;
      if (record.closed || record.contents.isDestroyed() || record.recoveryPromise) return;
      void this.reconnectDebugger(record, `retry:${reason}`).catch((retryError) => {
        if (!record.closed) {
          console.warn("[BrowserSurfaceManager] Browser Surface 调试器重连等待下一次重试", {
            surfaceId: record.surfaceId,
            reason,
            error: retryError instanceof Error ? retryError.message : String(retryError),
          });
        }
      });
    }, delay);
    timer.unref();
    record.debuggerReconnectTimer = timer;
    console.warn("[BrowserSurfaceManager] Browser Surface 调试器重连已调度", {
      surfaceId: record.surfaceId,
      reason,
      error,
      delayMs: delay,
    });
  }

  private clearDebuggerReconnectTimer(record: BrowserSurfaceRecord): void {
    if (!record.debuggerReconnectTimer) return;
    clearTimeout(record.debuggerReconnectTimer);
    record.debuggerReconnectTimer = null;
  }

  private invalidateAndRecover(record: BrowserSurfaceRecord, reason: string): void {
    if (record.recoveryPromise || record.closed || record.contents.isDestroyed()) return;
    const previousSlot = record.slotVisible && record.slotBounds
      ? { ...record.slotBounds }
      : null;
    record.priming = true;
    this.unmountSurface(record, this.#windows.get(record.windowId));
    record.recoverySlot = previousSlot
      ? { bounds: previousSlot, leaseRevision: record.slotLeaseRevision }
      : null;
    // render-process-gone 不保证 Electron 先发 debugger.detach。无论事件
    // 顺序如何，恢复都必须从全新的 session 开始，不能把旧 domain/session
    // 状态带进 reload。
    this.resetDebuggerSession(record);
    if (!record.contents.isDestroyed() && record.contents.debugger.isAttached()) {
      try {
        record.contents.debugger.detach();
      } catch {
        // renderer 销毁竞态下 detach 可能同步失败，恢复流程仍会继续。
      }
    }
    record.surfaceRevision = this.#surfaces.nextRevision(record.tabId);
    const recovery = this.enqueueCdp(record, () => this.recover(record, reason));
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
      await this.runNavigationAction(
        record,
        "reload",
        record.contents.getURL() || "about:blank",
        () => record.contents.reloadIgnoringCache(),
        DEFAULT_NAVIGATION_TIMEOUT_MS,
      );
      if (record.closed) return;
      await this.attachDebugger(record);
      record.priming = false;
      record.debuggerReconnectAttempt = 0;
      const recoverySlot = record.recoverySlot;
      record.recoverySlot = null;
      if (recoverySlot && recoverySlot.leaseRevision === record.slotLeaseRevision) {
        this.applySlot(record, recoverySlot.bounds, this.#windows.get(record.windowId));
      }
      this.scheduleViewportCommit(record);
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
    commit: ViewportCommit,
    lease: DebuggerSessionLease,
    track?: (promise: Promise<unknown>) => void,
  ): Promise<void> {
    if (record.closed || record.contents.isDestroyed()) return;
    if (record.contents.isLoadingMainFrame()) return;
    const lifecycle = record.viewportLifecycle;
    const applied = lifecycle.applied;
    if (commit.viewport.mode === "auto") {
      // auto 不写入任何设备指标。WebContentsView 的真实 bounds 就是页面
      // viewport，Chromium 会自行触发 resize、media query 和 flex/grid 重排。
      // 这条路径不能把右栏尺寸再次转成 CDP override，否则每次拖动都会
      // 让页面经历第二套 viewport 变更，产生闪烁、跳动或状态不同步。
      // 只有从 fixed 切换回来时才需要清理旧的 CDP override；清理动作也
      // 必须允许在 Surface 暂时隐藏时执行，避免隐藏期间残留固定视口。
      if (!applied || applied.scale === null) {
        lifecycle.applied = { viewport: structuredClone(commit.viewport), scale: null };
        return;
      }
      await this.sendSurfaceCdpCommand(
        record,
        "Emulation.clearDeviceMetricsOverride",
        {},
        DEFAULT_CDP_COMMAND_TIMEOUT_MS,
        lease,
        track,
      );
      if (record.viewportLifecycle.current !== commit) {
        throw staleSurfaceError("browser_viewport_commit_stale");
      }
      lifecycle.applied = { viewport: structuredClone(commit.viewport), scale: null };
      return;
    }
    const viewport = commit.viewport;
    if (viewport.mode !== "fixed") return;
    const width = Math.max(320, Math.round(viewport.width));
    const height = Math.max(240, Math.round(viewport.height));
    const mobile = viewport.device_type === "mobile";
    const scale = nativeViewportScale(width, height, commit.bounds.width, commit.bounds.height);
    if (
      applied
      && applied.scale !== null
      && Math.abs(applied.scale - scale) < VIEWPORT_SCALE_EPSILON
      && sameLogicalViewport(applied.viewport, viewport)
    ) return;
    await this.sendSurfaceCdpCommand(
      record,
      "Emulation.setDeviceMetricsOverride",
      {
        width,
        height,
        deviceScaleFactor: viewport.device_scale_factor_millis / 1_000,
        mobile,
        // 这是 Chromium Emulation 的 compositor scale，不是 CSS transform、
        // 截图缩放或宿主坐标换算。页面仍按 width/height 进行响应式布局，
        // 只是把完整的结果视图缩放到当前原生内容槽，避免大逻辑视口被裁切。
        scale,
        screenWidth: width,
        screenHeight: height,
        screenOrientation: {
          type: width > height ? "landscapePrimary" : "portraitPrimary",
          angle: width > height ? 90 : 0,
        },
      },
      DEFAULT_CDP_COMMAND_TIMEOUT_MS,
      lease,
      track,
    );
    if (record.viewportLifecycle.current !== commit) {
      throw staleSurfaceError("browser_viewport_commit_stale");
    }
    lifecycle.applied = { viewport: structuredClone(viewport), scale };
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
      ? this.initialAgentCursorPosition(record) ?? { x: null, y: null }
      : { x, y };
    record.cursor = { visible, ...position, action };
    this.#onEvent({
      type: "agent_cursor",
      binding: this.binding(record),
      visible,
      x: position.x,
      y: position.y,
      action,
    });
    const update = async ({ track, debuggerLease }: SurfaceLaneContext) => {
      if (record.closed || record.contents.isDestroyed() || !this.isRenderable(record)) return;
      // Page.getFrameTree 对尚未建立首个文档的 WebContents 不会返回。首个
      // 文档由 materialize 统一完成；导航期间则由 did-finish-load 重新应用
      // 最新状态，不能把页面命令队列绑定到绘制光标的 CDP 请求。
      if (record.priming || record.contents.isLoadingMainFrame() || !record.contents.getURL()) return;
      try {
        if (!record.contents.debugger.isAttached()) return;
        const lease = debuggerLease();
        if (record.cursorExecutionContextId === null) {
          const frameTree = await this.sendSurfaceCdpCommand(
            record,
            "Page.getFrameTree",
            {},
            CURSOR_CDP_COMMAND_TIMEOUT_MS,
            lease,
            track,
          ) as {
            frameTree?: { frame?: { id?: string } };
          };
          if (!this.isRenderable(record)) return;
          const frameId = frameTree.frameTree?.frame?.id;
          if (!frameId) return;
          const world = await this.sendSurfaceCdpCommand(
            record,
            "Page.createIsolatedWorld",
            {
              frameId,
              worldName: "magi-agent-cursor",
              grantUniveralAccess: false,
            },
            CURSOR_CDP_COMMAND_TIMEOUT_MS,
            lease,
            track,
          ) as { executionContextId?: number };
          if (!this.isRenderable(record)) return;
          if (!world.executionContextId) return;
          record.cursorExecutionContextId = world.executionContextId;
        }
        await this.sendSurfaceCdpCommand(
          record,
          "Runtime.evaluate",
          {
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
          },
          CURSOR_CDP_COMMAND_TIMEOUT_MS,
          lease,
          track,
        );
        if (!this.isRenderable(record)) return;
        // Runtime.evaluate 返回只代表 DOM 已更新，不代表 Chromium 已经
        // 完成下一帧合成。等待一个渲染帧，保证紧跟在输入动作后的截图
        // 能看到代理指针和点击反馈，而不是捕获到更新前的 compositor frame。
        await new Promise<void>((resolve) => setTimeout(resolve, 16));
        if (!this.isRenderable(record)) return;
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

  private promoteReplacement(tabId: string): void {
    const replacement = this.#surfaces.promoteReplacement(tabId);
    if (!replacement) return;
    replacement.surfaceRevision = this.#surfaces.nextRevision(replacement.tabId);
    this.#onEvent({ type: "primary_changed", binding: this.binding(replacement) });
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

  private assertRenderableBinding(
    binding: BrowserSurfaceBinding,
    options: { allowNavigationAdvance?: boolean } = {},
  ): BrowserSurfaceRecord {
    const record = this.requireRecord(binding.surface_id);
    this.recordForBinding(binding, options);
    if (!this.isPrimary(binding) || !this.isRenderable(record)) {
      throw staleSurfaceError("browser_surface_content_slot_unavailable");
    }
    return record;
  }

  private assertSurfaceLifecycleCurrent(record: BrowserSurfaceRecord, epoch: number): void {
    if (
      record.closed
      || record.lifecycleEpoch !== epoch
      || record.contents.isDestroyed()
    ) throw staleSurfaceError("browser_surface_lifecycle_stale");
  }

  private rejectNavigationWaiters(record: BrowserSurfaceRecord, error: Error): void {
    const operation = record.navigationOperation;
    if (!operation) return;
    operation.settled = true;
    operation.failed = true;
    for (const waiter of operation.waiters) waiter.reject(error);
    operation.waiters.clear();
  }

  private closeRecord(record: BrowserSurfaceRecord, promoteReplacement = true): void {
    if (record.closed) return;
    record.closed = true;
    record.lifecycleEpoch += 1;
    record.lifecycleAbort.abort();
    this.rejectNavigationWaiters(record, staleSurfaceError("browser_surface_not_found"));
    this.resetDebuggerSession(record);
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
    if (promoteReplacement) this.promoteReplacement(record.tabId);
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

export function nativeViewportScale(
  logicalWidth: number,
  logicalHeight: number,
  availableWidth: number,
  availableHeight: number,
): number {
  if (
    !Number.isFinite(logicalWidth)
    || !Number.isFinite(logicalHeight)
    || logicalWidth <= 0
    || logicalHeight <= 0
    || !Number.isFinite(availableWidth)
    || !Number.isFinite(availableHeight)
    || availableWidth <= 0
    || availableHeight <= 0
  ) return 1;
  const fit = Math.min(1, availableWidth / logicalWidth, availableHeight / logicalHeight);
  const normalized = Math.max(MIN_NATIVE_VIEWPORT_SCALE, fit);
  return Math.abs(normalized - 1) < VIEWPORT_SCALE_EPSILON ? 1 : normalized;
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

function normalizePopupNavigationUrl(value: string): string {
  const url = normalizeNavigableUrl(value);
  if (url === "about:blank") throw new Error("browser_popup_about_blank_rejected");
  return url;
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

function toError(value: unknown): Error {
  return value instanceof Error ? value : new Error(typeof value === "string" ? value : String(value));
}

function sameNavigationUrl(left: string, right: string): boolean {
  try {
    return normalizeNavigableUrl(left) === normalizeNavigableUrl(right);
  } catch {
    return left === right;
  }
}

function sameFrame(left: NavigationFrameIdentity, right: NavigationFrameIdentity): boolean {
  return left.processId === right.processId && left.routingId === right.routingId;
}

function frameIdentity(value: unknown): NavigationFrameIdentity | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const frame = value as { processId?: unknown; routingId?: unknown };
  if (!Number.isSafeInteger(frame.processId) || !Number.isSafeInteger(frame.routingId)) return null;
  return {
    processId: frame.processId as number,
    routingId: frame.routingId as number,
  };
}

function isForbiddenChildPageTarget(value: unknown): boolean {
  // 这个函数只处理 `Target.attachedToTarget` 报告的关联 Target；当前
  // Browser Tab 的根 WebContents 不会以该事件作为自己的子 session 到达。
  // 协议共享的 allow-list 让 page、webview 和未知/缺失类型全部 fail-closed，
  // 只保留 iframe/OOPIF/Worker 这类页面内部 Target。
  return !isAllowedBrowserChildTarget(value);
}

function constrainCdpCommandParams(method: string, params: Record<string, unknown>): Record<string, unknown> {
  if (method !== "Target.setAutoAttach" || params.autoAttach !== true) return params;
  // Lighthouse 需要 iframe/OOPIF/worker 的 CDP session，但一级 Browser Tab
  // 不允许任何页面型子 Target。由 Main 在唯一 CDP 出口覆盖 Worker 传入的
  // filter，避免某个调用方遗漏过滤条件后重新打开 popup Target 旁路。
  return {
    ...params,
    flatten: true,
    waitForDebuggerOnStart: false,
    // 空过滤项会重新匹配所有 Target，抵消 page 排除规则。显式列出
    // 页面内部资源类型，才能从 CDP 源头杜绝页面型 popup 子 Target。
    filter: [
      { type: "iframe" },
      { type: "worker" },
      { type: "service_worker" },
      { type: "shared_worker" },
    ],
  };
}

function sameBounds(left: Rectangle, right: Rectangle): boolean {
  return left.x === right.x
    && left.y === right.y
    && left.width === right.width
    && left.height === right.height;
}

function sameLogicalViewport(
  left: BrowserLogicalViewport,
  right: BrowserLogicalViewport,
): boolean {
  if (left.mode !== right.mode) return false;
  if (left.mode === "auto" || right.mode === "auto") return true;
  return left.width === right.width
    && left.height === right.height
    && left.device_scale_factor_millis === right.device_scale_factor_millis
    && left.device_type === right.device_type;
}

function containsBounds(parent: Rectangle, child: Rectangle): boolean {
  return parent.width > 0
    && parent.height > 0
    && child.width > 0
    && child.height > 0
    && child.x >= parent.x
    && child.y >= parent.y
    && child.x + child.width <= parent.x + parent.width
    && child.y + child.height <= parent.y + parent.height;
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

async function sendCdpCommandWithTimeout(
  contents: WebContents,
  method: string,
  params: Record<string, unknown>,
  timeoutMs: number,
  sessionId?: string,
  track?: (promise: Promise<unknown>) => void,
  signal?: AbortSignal,
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
  const settled = withOptionalAbort(timed, signal);
  track?.(settled.then(() => undefined, () => undefined));
  try {
    const result = await settled;
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
        timer = setTimeout(() => {
          const error = new Error(`browser_cdp_timeout:${method}`);
          error.name = "BrowserCdpTimeout";
          reject(error);
        }, timeoutMs);
        timer.unref();
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function isCdpTimeoutError(value: unknown): boolean {
  return value instanceof Error && value.name === "BrowserCdpTimeout";
}

function isBrowserCommandCancelledError(value: unknown): boolean {
  return value instanceof Error && value.message === "browser_command_cancelled";
}

function withAbortSignal<T>(
  promise: Promise<T>,
  signal: AbortSignal,
  abortError: Error,
): Promise<T> {
  if (signal.aborted) return Promise.reject(abortError);
  return new Promise<T>((resolve, reject) => {
    let settled = false;
    const cleanup = () => signal.removeEventListener("abort", onAbort);
    const settle = (settler: () => void) => {
      if (settled) return;
      settled = true;
      cleanup();
      settler();
    };
    const onAbort = () => settle(() => reject(abortError));
    signal.addEventListener("abort", onAbort, { once: true });
    promise.then(
      (value) => settle(() => resolve(value)),
      (error) => settle(() => reject(error)),
    );
  });
}

function withOptionalAbort<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
  return signal
    ? withAbortSignal(promise, signal, new Error("browser_command_cancelled"))
    : promise;
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw new Error("browser_command_cancelled");
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
