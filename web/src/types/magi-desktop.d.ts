interface MagiDesktopRectangle {
  x: number;
  y: number;
  width: number;
  height: number;
}

type MagiDesktopPanelKind = "agent" | "browser" | "code" | "terminal" | null;

interface MagiDesktopWindowLayoutSnapshot {
  desktopEpoch: string;
  windowId: string;
  layoutRevision: number;
  clientBounds: MagiDesktopRectangle;
  displayScaleFactor: number;
  fullscreen: boolean;
  rightPaneVisible: boolean;
  rightPaneWidth: number;
  activePanelKind: MagiDesktopPanelKind;
  activeTabId: string | null;
  activeSurfaceId: string | null;
  appBounds: MagiDesktopRectangle;
  dividerBounds: MagiDesktopRectangle | null;
  rightPaneBounds: MagiDesktopRectangle | null;
}

/**
 * 窗口内某个 Browser Tab 的承载身份。
 *
 * 会话级 Tab 仍以 `layout` 的活动指针为准；应用级 GPT Web 会话不允许写激活
 * 意图（A25），只能靠这份清单拿到 `surfaceId` 与 `navigationRevision`。
 */
interface MagiDesktopBrowserSurfaceSnapshot {
  tabId: string;
  browserSessionId: string;
  surfaceId: string;
  navigationRevision: number;
  /** 代理正在使用该页面：不可见时也必须保持挂载（离屏），不得卸载 guest。 */
  retainedForAgent: boolean;
}

interface MagiDesktopWindowSnapshot {
  desktopEpoch: string;
  windowId: string;
  snapshotRevision: number;
  layout: MagiDesktopWindowLayoutSnapshot;
  activeBrowserViewport: MagiDesktopLogicalViewport | null;
  activeBrowserDisplayMetrics: MagiDesktopBrowserDisplayMetrics | null;
  activeBrowserNavigationRevision: number | null;
  activeBrowserDownloads: MagiDesktopBrowserDownloadSnapshot[];
  browserSurfaces: MagiDesktopBrowserSurfaceSnapshot[];
}

interface MagiDesktopBrowserDisplayMetrics {
  width: number;
  height: number;
  scale: number;
}

interface MagiDesktopBrowserDownloadSnapshot {
  downloadId: string;
  tabId: string;
  suggestedFilename: string;
  state: "started" | "progressing" | "completed" | "cancelled" | "interrupted";
  receivedBytes: number;
  totalBytes: number | null;
}

interface MagiDesktopContextSnapshot {
  contextRevision: number;
  windowId?: string;
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
  sessionTitle: string;
}

type MagiDesktopLayoutIntent =
  | {
      type: "client_bounds";
      bounds: MagiDesktopRectangle;
      displayScaleFactor: number;
      fullscreen: boolean;
    }
  | { type: "right_pane_width"; width: number }
  | { type: "right_pane_reset_width" }
  | { type: "right_pane_visibility"; visible: boolean }
  | {
      type: "active_panel";
      kind: MagiDesktopPanelKind;
      tabId: string | null;
      surfaceId: string | null;
    };

type MagiDesktopLogicalViewport =
  | { mode: "auto" }
  | {
      mode: "fixed";
      width: number;
      height: number;
      device_scale_factor_millis: number;
      device_type: "desktop" | "mobile";
    };

type MagiDesktopViewportIntent =
  | { mode: "auto" }
  | {
      mode: "fixed";
      width: number;
      height: number;
      deviceScaleFactorMillis: number;
      deviceType: "desktop" | "mobile";
    };

interface MagiDesktopBrowserActivationRequest {
  tabId: string;
  browserSessionId: string;
  url: string;
  navigationRevision: number;
  viewport: MagiDesktopViewportIntent;
}

interface MagiDesktopBrowserDisplaySize {
  width: number;
  height: number;
}

interface MagiDesktopEmbeddedBrowserWebviewRequest {
  tabId: string;
  browserSessionId: string;
  navigationRevision: number;
  webContentsId: number;
  displaySize: MagiDesktopBrowserDisplaySize;
}

interface MagiDesktopReleasedBrowserWebviewRequest {
  tabId: string;
  browserSessionId: string;
  navigationRevision: number;
  webContentsId: number;
}

interface MagiDesktopBrowserDisplaySizeRequest
  extends MagiDesktopReleasedBrowserWebviewRequest {
  displaySize: MagiDesktopBrowserDisplaySize;
}

interface MagiDesktopBrowserInspectRequest {
  tabId: string;
  surfaceId: string;
  navigationRevision: number;
}

interface MagiDesktopUpdateSnapshot {
  status:
    | "idle"
    | "checking"
    | "available"
    | "downloading"
    | "downloaded"
    | "failed"
    | "unsupported";
  currentVersion: string;
  availableVersion: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  percent: number | null;
  error: string | null;
  installable: boolean;
}

type MagiDesktopBrowserComponentStatus =
  "starting" | "ready" | "restarting" | "failed" | "stopped";
type MagiDesktopBrowserProtocolStatus =
  MagiDesktopBrowserComponentStatus | "incompatible";

interface MagiDesktopBrowserComponentError {
  target: "daemon" | "worker" | "protocol" | "version";
  code: string;
  message: string;
}

interface MagiDesktopBrowserComponentSnapshot {
  product_version: string;
  electron_version: string;
  chromium_version: string;
  process_id: number;
  desktop_epoch: string;
  daemon: {
    version: string;
    status: MagiDesktopBrowserComponentStatus;
    process_id: number | null;
  };
  worker: {
    version: string;
    epoch: string;
    status: MagiDesktopBrowserComponentStatus;
  };
  protocol: {
    version: { major: number; minor: number };
    status: MagiDesktopBrowserProtocolStatus;
    compatible: boolean;
    error: MagiDesktopBrowserComponentError | null;
  };
  runtime: {
    status: MagiDesktopBrowserComponentStatus;
    ready: boolean;
    version_compatible: boolean;
    error: MagiDesktopBrowserComponentError | null;
  };
  error: MagiDesktopBrowserComponentError | null;
}

type MagiDesktopContextMenuRole =
  "undo" | "redo" | "cut" | "copy" | "paste" | "selectAll";

type MagiDesktopContextMenuItem =
  | { type: "role"; role: MagiDesktopContextMenuRole }
  | { type: "separator" }
  | { type: "action"; id: string; label: string; enabled?: boolean };

type MagiDesktopFileDropEvent =
  | { type: "enter"; paths: string[]; position: { x: number; y: number } }
  | { type: "over"; position: { x: number; y: number } }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" };

interface MagiDesktopBridge {
  readonly runtime: "electron";
  readonly surface: "app" | null;
  readonly windowId: string | null;
  getSnapshot(): Promise<MagiDesktopWindowSnapshot>;
  setContext(context: {
    workspaceId: string;
    workspacePath: string;
    sessionId: string;
    sessionTitle: string;
  }): Promise<MagiDesktopContextSnapshot>;
  submitLayoutIntent(
    intent: MagiDesktopLayoutIntent,
  ): Promise<MagiDesktopWindowSnapshot>;
  activateBrowser(
    request: MagiDesktopBrowserActivationRequest,
  ): Promise<MagiDesktopWindowSnapshot>;
  registerBrowserWebview(
    request: MagiDesktopEmbeddedBrowserWebviewRequest,
  ): Promise<MagiDesktopWindowSnapshot>;
  updateBrowserDisplaySize(
    request: MagiDesktopBrowserDisplaySizeRequest,
  ): Promise<MagiDesktopWindowSnapshot>;
  releaseBrowserWebview(
    request: MagiDesktopReleasedBrowserWebviewRequest,
  ): Promise<MagiDesktopWindowSnapshot>;
  waitForBrowserSurface(request: {
    tabId: string;
  }): Promise<MagiDesktopWindowSnapshot>;
  activatePanel(request: {
    kind: MagiDesktopPanelKind;
    tabId: string | null;
  }): Promise<MagiDesktopWindowSnapshot>;
  setBrowserViewport(request: {
    tabId: string;
    viewport: MagiDesktopViewportIntent;
  }): Promise<MagiDesktopWindowSnapshot>;
  cancelBrowserDownload(request: {
    tabId: string;
    downloadId: string;
  }): Promise<MagiDesktopWindowSnapshot>;
  /** 在系统文件管理器中显示已完成的下载；文件已不存在时返回 false。 */
  revealBrowserDownload(request: {
    tabId: string;
    downloadId: string;
  }): Promise<boolean>;
  /** 弹出系统保存对话框把已完成的下载另存；用户取消或文件已不存在时返回 false。 */
  saveBrowserDownload(request: {
    tabId: string;
    downloadId: string;
  }): Promise<boolean>;
  startBrowserInspect(
    request: MagiDesktopBrowserInspectRequest,
  ): Promise<MagiDesktopWindowSnapshot>;
  stopBrowserInspect(
    request: MagiDesktopBrowserInspectRequest,
  ): Promise<MagiDesktopWindowSnapshot>;
  startBrowserAnnotationCapture(
    request: MagiDesktopBrowserInspectRequest,
  ): Promise<void>;
  stopBrowserAnnotationCapture(
    request: MagiDesktopBrowserInspectRequest,
  ): Promise<void>;
  focusApp(): Promise<void>;
  readyRightPane(): Promise<void>;
  openExternal(url: string): Promise<void>;
  showContextMenu(request: {
    items: MagiDesktopContextMenuItem[];
  }): Promise<string | null>;
  openWorkspaceFolder(workspaceRootPathRef: string): Promise<void>;
  revealWorkspaceFile(request: {
    targetPathRef: string;
    workspaceRootPathRef: string;
  }): Promise<void>;
  setAppearance(appearance: {
    backgroundColor: string;
    accentColor: string;
    material: "clear" | "translucent" | "immersive";
    mode: "light" | "dark";
  }): Promise<void>;
  getAppVersion(): Promise<string>;
  getBrowserComponentInfo(): Promise<MagiDesktopBrowserComponentSnapshot>;
  restartBrowserAutomation(): Promise<MagiDesktopBrowserComponentSnapshot>;
  clearBrowserData(): Promise<void>;
  /**
   * 只清应用级 GPT Web 持久分区（登录态）。
   *
   * 与 `clearBrowserData` 的区别是不动其他浏览器 Tab 的站点数据（§5.13）。
   */
  clearWebModelData(): Promise<void>;
  checkForUpdates(): Promise<MagiDesktopUpdateSnapshot>;
  downloadUpdate(): Promise<MagiDesktopUpdateSnapshot>;
  installUpdate(): Promise<never>;
  onSnapshot(
    listener: (snapshot: MagiDesktopWindowSnapshot) => void,
  ): () => void;
  onBrowserRuntimeReady(
    listener: (event: { revision: number }) => void,
  ): () => void;
  onContext(
    listener: (context: MagiDesktopContextSnapshot) => void,
  ): () => void;
  onBrowserEvent(listener: (event: unknown) => void): () => void;
  onBrowserComponent(
    listener: (snapshot: MagiDesktopBrowserComponentSnapshot) => void,
  ): () => void;
  onUpdate(listener: (snapshot: MagiDesktopUpdateSnapshot) => void): () => void;
  onFileDrop(listener: (event: MagiDesktopFileDropEvent) => void): () => void;
}

interface Window {
  magiDesktop?: MagiDesktopBridge;
}
