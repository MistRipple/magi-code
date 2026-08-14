import { createHash, randomUUID } from "node:crypto";
import {
  session,
  type BaseWindow,
  type HandlerDetails,
  type Rectangle,
  WebContentsView,
} from "electron";
import type {
  BrowserControlUpdate,
  BrowserLogicalViewport,
  BrowserPageState,
  BrowserSurfaceBinding,
} from "@magi/desktop-browser-contracts";

export type BrowserSurfaceEvent =
  | { type: "primary_changed"; binding: BrowserSurfaceBinding }
  | { type: "page_updated"; binding: BrowserSurfaceBinding; page: BrowserPageState }
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
}

interface BrowserSurfaceRecord {
  windowId: string;
  surfaceId: string;
  surfaceRevision: number;
  tabId: string;
  browserSessionId: string;
  partitionId: string;
  view: WebContentsView;
  targetId: string;
  navigationRevision: number;
  viewport: BrowserLogicalViewport;
  slot: { width: number; height: number };
  visible: boolean;
  primary: boolean;
  closed: boolean;
  automationInputDepth: number;
  agentControlled: boolean;
  cursor: { visible: boolean; x: number | null; y: number | null; action: string | null };
  viewportApplied: boolean;
  viewportApplyPromise: Promise<void> | null;
  viewportApplyDirty: boolean;
}

const ALLOWED_NAVIGATION_PROTOCOLS = new Set(["http:", "https:", "about:"]);
const BLOCKED_HOSTS = new Set(["169.254.169.254", "metadata.google.internal"]);
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
  "Page.getFrameTree",
  "Page.getLayoutMetrics",
  "Page.handleJavaScriptDialog",
  "Page.removeScriptToEvaluateOnNewDocument",
  "Performance.enable",
  "Performance.getMetrics",
  "Overlay.hideHighlight",
  "Overlay.highlightNode",
  "Runtime.evaluate",
  "Runtime.getHeapUsage",
  "Tracing.end",
  "Tracing.start",
]);

export class BrowserSurfaceManager {
  readonly #desktopEpoch: string;
  readonly #windows: Map<string, BaseWindow>;
  readonly #records = new Map<string, BrowserSurfaceRecord>();
  readonly #primaryByTab = new Map<string, string>();
  readonly #configuredPartitions = new Set<string>();
  readonly #onEvent: (event: BrowserSurfaceEvent) => void;

  constructor(input: {
    desktopEpoch: string;
    windows: Map<string, BaseWindow>;
    onEvent: (event: BrowserSurfaceEvent) => void;
  }) {
    this.#desktopEpoch = input.desktopEpoch;
    this.#windows = input.windows;
    this.#onEvent = input.onEvent;
  }

  async materialize(input: MaterializeSurfaceInput): Promise<BrowserSurfaceBinding> {
    const existing = this.surfaceForTab(input.tabId, input.windowId);
    if (existing) {
      existing.navigationRevision = Math.max(
        existing.navigationRevision,
        input.navigationRevision,
      );
      this.promote(existing.surfaceId);
      return this.binding(existing);
    }

    const window = this.#windows.get(input.windowId);
    if (!window || window.isDestroyed()) {
      throw new Error(`desktop window does not exist: ${input.windowId}`);
    }
    const initialUrl = normalizeNavigableUrl(input.initialUrl);
    const partitionId = browserPartitionId(input.browserSessionId);
    this.configurePartition(partitionId);
    const view = new WebContentsView({
      webPreferences: {
        partition: partitionId,
        nodeIntegration: false,
        contextIsolation: true,
        sandbox: true,
        webSecurity: true,
        allowRunningInsecureContent: false,
        spellcheck: false,
        autoplayPolicy: "document-user-activation-required",
      },
    });
    view.setBackgroundColor("#ffffff");
    const record: BrowserSurfaceRecord = {
      windowId: input.windowId,
      surfaceId: `surface-${randomUUID()}`,
      surfaceRevision: 1,
      tabId: input.tabId,
      browserSessionId: input.browserSessionId,
      partitionId,
      view,
      targetId: "",
      navigationRevision: input.navigationRevision,
      viewport: input.viewport,
      slot: { width: 0, height: 0 },
      visible: false,
      primary: false,
      closed: false,
      automationInputDepth: 0,
      agentControlled: false,
      cursor: { visible: false, x: null, y: null, action: null },
      // Chromium's Emulation domain is only ready after the renderer has a
      // document.  Auto mode uses Chromium's native window metrics and needs
      // no command during surface creation; fixed mode is applied from
      // `did-finish-load` without delaying Tab creation.
      viewportApplied: input.viewport.mode === "auto",
      viewportApplyPromise: null,
      viewportApplyDirty: false,
    };
    this.#records.set(record.surfaceId, record);
    window.contentView.addChildView(view);
    // 逻辑 Tab 可能先于右栏激活而由 daemon 恢复。Surface 在这段时间仍然
    // 隐藏，但不能以零尺寸存在，否则 Chromium 的布局、DOM 和截图 CDP
    // 命令会等待一个永远不会产生的 renderer viewport。初始槽位仍在同一个
    // BaseWindow 内容树内，右栏激活后立即由 WindowLayoutSnapshot 原子覆盖。
    const initialBounds = initialHiddenSurfaceBounds(window);
    record.slot = { width: initialBounds.width, height: initialBounds.height };
    view.setBounds(initialBounds);
    view.setVisible(false);
    this.installSurfacePolicy(record);
    await this.attachDebugger(record);
    this.promote(record.surfaceId);
    // 物理 Surface 在这里已经完成创建、挂载和 CDP 握手。页面导航属于真实
    // WebContents 的异步生命周期，不能让网络响应时间决定 Browser Tab 是否能创建。
    // 固定 viewport 等文档就绪后由 did-finish-load 应用，慢页面不会阻塞创建。
    void view.webContents.loadURL(initialUrl).catch((error: unknown) => {
      if (record.closed) return;
      this.#onEvent({
        type: "loading_changed",
        binding: this.binding(record),
        loading: false,
      });
      console.warn(`[BrowserSurfaceManager] 页面导航失败: ${error instanceof Error ? error.message : String(error)}`);
    });
    return this.binding(record);
  }

  bindingForTab(tabId: string): BrowserSurfaceBinding | null {
    const primary = this.#primaryByTab.get(tabId);
    const record = primary ? this.#records.get(primary) : undefined;
    return record && !record.closed ? this.binding(record) : null;
  }

  bindingForTabInWindow(tabId: string, windowId: string): BrowserSurfaceBinding | null {
    const record = this.surfaceForTab(tabId, windowId);
    return record ? this.binding(record) : null;
  }

  isPrimary(binding: BrowserSurfaceBinding): boolean {
    const record = this.#records.get(binding.surface_id);
    return Boolean(
      record
      && !record.closed
      && record.primary
      && this.#primaryByTab.get(record.tabId) === record.surfaceId
      && binding.surface_revision === record.surfaceRevision,
    );
  }

  viewportStateForSurface(surfaceId: string | null): {
    viewport: BrowserLogicalViewport;
    slot: { width: number; height: number };
  } | null {
    if (!surfaceId) return null;
    const record = this.#records.get(surfaceId);
    return record && !record.closed
      ? { viewport: structuredClone(record.viewport), slot: { ...record.slot } }
      : null;
  }

  viewportForSurface(surfaceId: string | null): BrowserLogicalViewport | null {
    return this.viewportStateForSurface(surfaceId)?.viewport ?? null;
  }

  recordForBinding(binding: BrowserSurfaceBinding): WebContentsView {
    const record = this.#records.get(binding.surface_id);
    if (!record || record.closed) {
      throw staleSurfaceError("browser_surface_not_found");
    }
    if (
      binding.desktop_epoch !== this.#desktopEpoch
      || binding.window_id !== record.windowId
      || binding.surface_revision !== record.surfaceRevision
      || binding.tab_id !== record.tabId
      || binding.web_contents_id !== record.view.webContents.id
      || binding.target_id !== record.targetId
    ) {
      throw staleSurfaceError("browser_surface_stale");
    }
    return record.view;
  }

  async sendCdp(
    binding: BrowserSurfaceBinding,
    method: string,
    params: Record<string, unknown> = {},
  ): Promise<unknown> {
    const view = this.recordForBinding(binding);
    if (!ALLOWED_WORKER_CDP_METHODS.has(method)) {
      throw new Error(`browser_cdp_method_denied:${method}`);
    }
    if (!view.webContents.debugger.isAttached()) {
      throw staleSurfaceError("browser_debugger_detached");
    }
    const record = this.requireRecord(binding.surface_id);
    const injectsInput = method.startsWith("Input.");
    if (injectsInput) record.automationInputDepth += 1;
    try {
      const result = await view.webContents.debugger.sendCommand(method, params);
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

  setBounds(surfaceId: string, bounds: Rectangle | null): void {
    const record = this.#records.get(surfaceId);
    if (!record || record.closed) return;
    if (!bounds) {
      if (!record.visible) return;
      record.visible = false;
      record.view.setVisible(false);
      return;
    }
    const normalized = normalizeBounds(bounds);
    const boundsChanged = !sameBounds(record.view.getBounds(), normalized);
    record.slot = { width: normalized.width, height: normalized.height };
    if (boundsChanged) record.view.setBounds(normalized);
    if (!record.visible) record.view.setVisible(true);
    record.visible = true;
    // The native parent owns geometry. A fixed responsive viewport is a
    // Chromium emulation choice, not a reason to replay metrics while the
    // right pane is being dragged.
  }

  hideWindowSurfaces(windowId: string): void {
    for (const record of this.#records.values()) {
      if (record.windowId !== windowId || record.closed) continue;
      record.visible = false;
      record.view.setVisible(false);
    }
  }

  focus(surfaceId: string): void {
    const record = this.requireRecord(surfaceId);
    record.view.webContents.focus();
    this.promote(surfaceId);
  }

  async navigate(
    binding: BrowserSurfaceBinding,
    navigation:
      | { action: "url"; url: string }
      | { action: "back" }
      | { action: "forward" }
      | { action: "reload"; ignore_cache?: boolean },
  ): Promise<BrowserPageState> {
    const view = this.recordForBinding(binding);
    switch (navigation.action) {
      case "url":
        await view.webContents.loadURL(normalizeNavigableUrl(navigation.url));
        break;
      case "back":
        if (view.webContents.navigationHistory.canGoBack()) {
          await view.webContents.navigationHistory.goBack();
        }
        break;
      case "forward":
        if (view.webContents.navigationHistory.canGoForward()) {
          await view.webContents.navigationHistory.goForward();
        }
        break;
      case "reload":
        navigation.ignore_cache ? view.webContents.reloadIgnoringCache() : view.webContents.reload();
        await waitForTopLevelLoad(view);
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
    if (record.view.webContents.isLoadingMainFrame()) return;
    this.scheduleViewportApply(record);
    await record.viewportApplyPromise;
  }

  async updateControl(tabId: string, surfaceId: string, control: BrowserControlUpdate): Promise<void> {
    const binding = this.bindingForTab(tabId);
    if (!binding || binding.surface_id !== surfaceId) {
      throw staleSurfaceError("browser_surface_stale");
    }
    const record = this.requireRecord(surfaceId);
    record.agentControlled = control.mode === "agent";
    await this.setAgentCursor(
      record,
      record.agentControlled,
      record.cursor.x,
      record.cursor.y,
      record.cursor.action ?? "move",
    );
  }

  async closeTab(tabId: string): Promise<void> {
    const records = [...this.#records.values()].filter((record) => record.tabId === tabId);
    for (const record of records) this.closeRecord(record);
    this.#primaryByTab.delete(tabId);
  }

  closeWindow(windowId: string): void {
    for (const record of [...this.#records.values()]) {
      if (record.windowId === windowId) this.closeRecord(record);
    }
  }

  closeAll(): void {
    for (const record of [...this.#records.values()]) this.closeRecord(record);
    this.#primaryByTab.clear();
  }

  async clearBrowsingData(): Promise<void> {
    const partitions = [...this.#configuredPartitions];
    await Promise.all(partitions.map(async (partitionId) => {
      const browserSession = session.fromPartition(partitionId, { cache: false });
      await Promise.all([
        browserSession.clearCache(),
        browserSession.clearStorageData(),
      ]);
    }));
    for (const record of this.#records.values()) {
      if (!record.closed) record.view.webContents.reloadIgnoringCache();
    }
  }

  private surfaceForTab(tabId: string, windowId: string): BrowserSurfaceRecord | null {
    return [...this.#records.values()].find((record) => (
      !record.closed && record.tabId === tabId && record.windowId === windowId
    )) ?? null;
  }

  private configurePartition(partitionId: string): void {
    if (this.#configuredPartitions.has(partitionId)) return;
    this.#configuredPartitions.add(partitionId);
    const browserSession = session.fromPartition(partitionId, { cache: false });
    browserSession.setPermissionCheckHandler(() => false);
    browserSession.setPermissionRequestHandler((_webContents, _permission, callback) => {
      callback(false);
    });
  }

  private installSurfacePolicy(record: BrowserSurfaceRecord): void {
    const { webContents } = record.view;
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
      this.#onEvent({
        type: "loading_changed",
        binding: this.binding(record),
        loading: true,
      });
    });
    const publishPage = () => {
      if (record.closed) return;
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
      if (record.closed) return;
      this.#onEvent({
        type: "loading_changed",
        binding: this.binding(record),
        loading: false,
      });
      publishPage();
    });
    webContents.on("did-finish-load", () => {
      if (!record.closed && record.viewport.mode === "fixed" && !record.viewportApplied) {
        this.scheduleViewportApply(record);
      }
      if (record.closed || !record.agentControlled) return;
      void this.setAgentCursor(record, true, record.cursor.x, record.cursor.y, record.cursor.action);
    });
    webContents.on("focus", () => {
      if (!record.closed) {
        this.promote(record.surfaceId);
      }
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
      const binding = this.binding(record);
      this.#onEvent({
        type: "page_crashed",
        binding,
        reason: details.reason,
      });
      record.surfaceRevision += 1;
    });
  }

  private async attachDebugger(record: BrowserSurfaceRecord): Promise<void> {
    const debuggerApi = record.view.webContents.debugger;
    debuggerApi.attach("1.3");
    const target = await debuggerApi.sendCommand("Target.getTargetInfo") as {
      targetInfo?: { targetId?: string };
    };
    record.targetId = target.targetInfo?.targetId?.trim() || `webcontents-${record.view.webContents.id}`;
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
      record.surfaceRevision += 1;
    });
  }

  private async applyViewport(record: BrowserSurfaceRecord): Promise<void> {
    if (record.closed || !record.view.webContents.debugger.isAttached()) return;
    if (record.view.webContents.isLoadingMainFrame()) return;
    if (record.viewport.mode === "auto") {
      await record.view.webContents.debugger.sendCommand("Emulation.clearDeviceMetricsOverride");
      await record.view.webContents.debugger.sendCommand("Emulation.setTouchEmulationEnabled", {
        enabled: false,
      });
      record.viewportApplied = true;
      return;
    }
    if (record.slot.width <= 1 || record.slot.height <= 1) return;
    const width = Math.max(320, Math.round(record.viewport.width));
    const height = Math.max(240, Math.round(record.viewport.height));
    const mobile = record.viewport.device_type === "mobile";
    await record.view.webContents.debugger.sendCommand("Emulation.setDeviceMetricsOverride", {
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
    await record.view.webContents.debugger.sendCommand("Emulation.setTouchEmulationEnabled", {
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
      if (record.view.webContents.isLoadingMainFrame()) return;
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
    if (record.closed || record.view.webContents.isDestroyed()) return;
    record.cursor = { visible, x, y, action };
    this.#onEvent({
      type: "agent_cursor",
      binding: this.binding(record),
      visible,
      x,
      y,
      action,
    });
    const payload = JSON.stringify({ visible, x, y, action });
    try {
      await record.view.webContents.executeJavaScript(`(() => {
        const state = ${payload};
        let cursor = document.getElementById('magi-agent-cursor');
        if (!cursor) {
          cursor = document.createElement('div');
          cursor.id = 'magi-agent-cursor';
          cursor.setAttribute('aria-hidden', 'true');
          cursor.style.cssText = 'position:fixed;z-index:2147483647;pointer-events:none;width:18px;height:18px;transform:translate(-2px,-2px);transition:left 60ms linear,top 60ms linear;display:none;';
          document.documentElement.appendChild(cursor);
        }
        cursor.style.display = state.visible ? 'block' : 'none';
        if (state.visible && state.x !== null && state.y !== null) {
          cursor.style.left = state.x + 'px';
          cursor.style.top = state.y + 'px';
        }
        cursor.innerHTML = '<span style="display:block;width:0;height:0;border-top:9px solid transparent;border-bottom:3px solid transparent;border-left:13px solid #2563eb;filter:drop-shadow(0 0 1px #fff);transform:rotate(-18deg);transform-origin:2px 2px"></span>';
      })()`, true);
    } catch {
      // 页面提交导航时 DOM 会短暂不可用，did-finish-load 会重新注入。
    }
  }

  private promote(surfaceId: string): void {
    const record = this.requireRecord(surfaceId);
    const previousId = this.#primaryByTab.get(record.tabId);
    if (previousId === surfaceId) {
      record.primary = true;
      return;
    }
    const previous = previousId ? this.#records.get(previousId) : undefined;
    if (previous && !previous.closed) {
      previous.primary = false;
      previous.surfaceRevision += 1;
    }
    record.primary = true;
    record.surfaceRevision += 1;
    this.#primaryByTab.set(record.tabId, surfaceId);
    this.#onEvent({ type: "primary_changed", binding: this.binding(record) });
  }

  private pageState(record: BrowserSurfaceRecord): BrowserPageState {
    const url = record.view.webContents.getURL() || "about:blank";
    return {
      tab_id: record.tabId,
      url,
      origin: safeOrigin(url),
      title: record.view.webContents.getTitle() || "",
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
      web_contents_id: record.view.webContents.id,
      target_id: record.targetId,
      browser_context_id: record.partitionId,
      navigation_revision: record.navigationRevision,
    };
  }

  private requireRecord(surfaceId: string): BrowserSurfaceRecord {
    const record = this.#records.get(surfaceId);
    if (!record || record.closed) throw staleSurfaceError("browser_surface_not_found");
    return record;
  }

  private closeRecord(record: BrowserSurfaceRecord): void {
    if (record.closed) return;
    record.closed = true;
    record.visible = false;
    const window = this.#windows.get(record.windowId);
    if (window && !window.isDestroyed()) {
      window.contentView.removeChildView(record.view);
    }
    if (record.view.webContents.debugger.isAttached()) {
      record.view.webContents.debugger.detach();
    }
    record.view.webContents.close();
    this.#records.delete(record.surfaceId);
    if (this.#primaryByTab.get(record.tabId) === record.surfaceId) {
      this.#primaryByTab.delete(record.tabId);
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

function normalizeBounds(bounds: Rectangle): Rectangle {
  return {
    x: Math.round(bounds.x),
    y: Math.round(bounds.y),
    width: Math.max(1, Math.round(bounds.width)),
    height: Math.max(1, Math.round(bounds.height)),
  };
}

function initialHiddenSurfaceBounds(window: BaseWindow): Rectangle {
  const bounds = window.getContentBounds();
  return normalizeBounds({
    x: 0,
    y: 0,
    width: Math.max(320, bounds.width),
    height: Math.max(240, bounds.height),
  });
}

function sameBounds(left: Rectangle, right: Rectangle): boolean {
  return left.x === right.x
    && left.y === right.y
    && left.width === right.width
    && left.height === right.height;
}

function browserPartitionId(browserSessionId: string): string {
  return `magi-browser-${createHash("sha256").update(browserSessionId).digest("hex")}`;
}

function staleSurfaceError(code: string): Error {
  const error = new Error(code);
  error.name = "BrowserSurfaceError";
  return error;
}

async function waitForTopLevelLoad(view: WebContentsView): Promise<void> {
  if (!view.webContents.isLoadingMainFrame()) return;
  await new Promise<void>((resolve) => {
    const done = () => {
      view.webContents.off("did-stop-loading", done);
      resolve();
    };
    view.webContents.on("did-stop-loading", done);
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
