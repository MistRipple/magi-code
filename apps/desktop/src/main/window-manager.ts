import { randomUUID } from "node:crypto";
import {
  BaseWindow,
  screen,
  WebContentsView,
  type Rectangle,
} from "electron";
import type {
  BrowserLogicalViewport,
  DesktopRightPaneIntentEnvelope,
  DesktopRightPaneTabIntent,
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
  rightPaneView: WebContentsView;
  layout: WindowLayoutState;
  context: DesktopRendererContext;
  rightPaneReady: boolean;
  pendingRightPaneIntents: DesktopRightPaneIntentEnvelope[];
  closed: boolean;
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
      backgroundColor: "#0f1115",
      show: false,
    });
    const appView = this.createTrustedView("app", windowId);
    const rightPaneView = this.createTrustedView("right-pane", windowId);
    window.contentView.addChildView(appView);
    window.contentView.addChildView(rightPaneView);
    this.#overlayManager.create(windowId, window);
    rightPaneView.setVisible(false);
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
      appView,
      rightPaneView,
      layout,
      context: {
        contextRevision: 0,
        windowId,
        workspaceId: "",
        workspacePath: "",
        sessionId: "",
      },
      rightPaneReady: false,
      pendingRightPaneIntents: [],
      closed: false,
    };
    this.#records.set(windowId, record);
    this.#windows.set(windowId, window);
    this.#activeWindowId = windowId;
    this.installWindowEvents(record);
    this.applyLayout(record);
    // 隐藏的 WebContentsView 在 Electron 中不会获得真实 renderer viewport，
    // 直到首次显示前其 innerWidth/innerHeight 都可能保持为 0，导致 Svelte
    // 右栏永远停留在空文档。窗口尚未展示，先让可信右栏完成一次真实布局，
    // 等两个 renderer 都就绪后再按权威布局隐藏它。
    rightPaneView.setVisible(true);
    void Promise.all([
      appView.webContents.loadURL(this.rendererUrl("app", windowId)),
      rightPaneView.webContents.loadURL(this.rendererUrl("right-pane", windowId)),
    ]).then(() => {
      if (window.isDestroyed()) return;
      this.applyLayout(record);
      window.show();
    });
    return windowId;
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
    const binding = await this.#surfaceManager.materialize({
      windowId: input.windowId,
      tabId: input.tabId,
      browserSessionId: input.browserSessionId,
      initialUrl: input.url,
      navigationRevision: input.navigationRevision,
      viewport: input.viewport,
    });
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
    record.layout = reduceWindowLayout(record.layout, {
      type: "active_panel",
      kind,
      tabId,
      surfaceId: null,
    });
    return this.applyLayout(record);
  }

  openRightPaneTab(
    windowId: string,
    envelope: DesktopRightPaneIntentEnvelope,
  ): DesktopWindowSnapshot {
    const record = this.requireWindow(windowId);
    const panel = panelForRightPaneIntent(envelope.intent);
    record.layout = reduceWindowLayout(record.layout, {
      type: "right_pane_visibility",
      visible: true,
    });
    record.layout = reduceWindowLayout(record.layout, {
      type: "active_panel",
      kind: panel.kind,
      tabId: panel.tabId,
      surfaceId: null,
    });
    const snapshot = this.applyLayout(record);
    if (record.rightPaneReady && !record.rightPaneView.webContents.isDestroyed()) {
      record.rightPaneView.webContents.send("magi-desktop:right-pane-intent", envelope);
    } else {
      record.pendingRightPaneIntents.push(envelope);
    }
    return snapshot;
  }

  handleRightPaneReady(windowId: string): void {
    const record = this.requireWindow(windowId);
    record.rightPaneReady = true;
    if (!record.rightPaneView.webContents.isDestroyed()) {
      record.rightPaneView.webContents.send("magi-desktop:context", record.context);
      for (const envelope of record.pendingRightPaneIntents.splice(0)) {
        record.rightPaneView.webContents.send("magi-desktop:right-pane-intent", envelope);
      }
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
    if (!record.rightPaneView.webContents.isDestroyed()) {
      record.rightPaneView.webContents.send("magi-desktop:context", record.context);
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
    this.#overlayManager.open(windowId, state, snapshotWindowLayout(record.layout));
  }

  closeOverlay(windowId: string): void {
    this.requireWindow(windowId);
    this.#overlayManager.close(windowId);
  }

  handleOverlayAction(windowId: string, action: DesktopOverlayAction): void {
    this.#overlayManager.handleAction(windowId, action);
  }

  handleOverlayReady(windowId: string): void {
    this.#overlayManager.handleReady(windowId);
  }

  setAppearance(windowId: string, backgroundColor: string): void {
    const record = this.requireWindow(windowId);
    record.window.setBackgroundColor(backgroundColor);
    record.appView.setBackgroundColor(backgroundColor);
    record.rightPaneView.setBackgroundColor(backgroundColor);
  }

  closeAll(): void {
    for (const record of [...this.#records.values()]) this.closeRecord(record);
  }

  broadcast(windowId: string, channel: string, value: unknown): void {
    const record = this.requireWindow(windowId);
    for (const view of [record.appView, record.rightPaneView]) {
      if (!view.webContents.isDestroyed()) view.webContents.send(channel, value);
    }
  }

  windowIdForWebContents(webContentsId: number): string | null {
    const overlayWindowId = this.#overlayManager.windowIdForWebContents(webContentsId);
    if (overlayWindowId) return overlayWindowId;
    for (const record of this.#records.values()) {
      if (
        record.appView.webContents.id === webContentsId
        || record.rightPaneView.webContents.id === webContentsId
      ) {
        return record.windowId;
      }
    }
    return null;
  }

  rendererRoleForWebContents(webContentsId: number): "app" | "right-pane" | "overlay" | null {
    if (this.#overlayManager.isWebContents(webContentsId)) return "overlay";
    for (const record of this.#records.values()) {
      if (record.appView.webContents.id === webContentsId) return "app";
      if (record.rightPaneView.webContents.id === webContentsId) return "right-pane";
    }
    return null;
  }

  private createTrustedView(surface: "app" | "right-pane", windowId: string): WebContentsView {
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

  private rendererUrl(surface: "app" | "right-pane", windowId: string): string {
    const url = new URL("/web.html", this.#agentOrigin);
    url.searchParams.set("desktopSurface", surface);
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
      record.layout = reduceWindowLayout(record.layout, {
        type: "client_bounds",
        bounds: { x: 0, y: 0, width: bounds.width, height: bounds.height },
        displayScaleFactor: display.scaleFactor,
        fullscreen: window.isFullScreen(),
      });
      this.applyLayout(record);
    };
    window.on("resize", updateBounds);
    window.on("enter-full-screen", updateBounds);
    window.on("leave-full-screen", updateBounds);
    window.on("closed", () => this.closeRecord(record));
    for (const view of [record.appView, record.rightPaneView]) {
      view.webContents.on("render-process-gone", () => {
        if (record.closed) return;
        if (view === record.rightPaneView) record.rightPaneReady = false;
        void view.webContents.reload();
      });
    }
  }

  private applyLayout(record: DesktopWindowRecord): DesktopWindowSnapshot {
    const snapshot = this.snapshot(record.windowId);
    const { layout } = snapshot;
    record.appView.setBounds(layout.appBounds as Rectangle);
    record.appView.setVisible(true);
    if (layout.rightPaneBounds) record.rightPaneView.setBounds(layout.rightPaneBounds as Rectangle);
    record.rightPaneView.setVisible(layout.rightPaneVisible);
    this.#surfaceManager.hideWindowSurfaces(record.windowId);
    if (layout.activeSurfaceId && layout.browserSurfaceBounds) {
      this.#surfaceManager.setBounds(
        layout.activeSurfaceId,
        layout.browserSurfaceBounds as Rectangle,
      );
    }
    this.#overlayManager.updateLayout(record.windowId, layout);
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
    for (const view of [record.appView, record.rightPaneView]) {
      if (!view.webContents.isDestroyed()) view.webContents.close();
    }
    this.#records.delete(record.windowId);
    this.#windows.delete(record.windowId);
    if (!record.window.isDestroyed()) record.window.destroy();
  }
}

function panelForRightPaneIntent(intent: DesktopRightPaneTabIntent): {
  kind: PanelKind;
  tabId: string;
} {
  switch (intent.kind) {
    case "agent":
      return { kind: "agent", tabId: `agent:${intent.agentRunId}` };
    case "code":
      return { kind: "code", tabId: `code:${intent.filepath}` };
    case "terminal":
      return { kind: "terminal", tabId: `terminal:${intent.terminalTabId}` };
  }
}
