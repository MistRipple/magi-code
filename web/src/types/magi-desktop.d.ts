interface MagiDesktopRectangle {
  x: number;
  y: number;
  width: number;
  height: number;
}

type MagiDesktopPanelKind = 'agent' | 'browser' | 'code' | 'terminal' | null;

interface MagiDesktopOverlayItem {
  id: string;
  label: string;
  icon: string | null;
  selected: boolean;
  disabled: boolean;
}

interface MagiDesktopOverlayField {
  id: string;
  label: string;
  type: 'number' | 'text';
  value: string;
  min: number | null;
  max: number | null;
}

interface MagiDesktopOverlayState {
  overlayId: string;
  kind: 'menu' | 'annotation';
  phase: 'menu' | 'select' | 'comment';
  ownerId: string;
  placement: 'right-pane-add' | 'browser-viewport' | 'browser-annotations' | 'browser-content';
  title: string;
  items: MagiDesktopOverlayItem[];
  fields: MagiDesktopOverlayField[];
}

interface MagiDesktopOverlayAction {
  overlayId: string;
  kind: 'menu' | 'annotation';
  ownerId: string;
  interaction: 'select' | 'input';
  id: string;
  value: string | null;
}

interface MagiDesktopWindowLayoutSnapshot {
  desktopEpoch: string;
  windowId: string;
  layoutRevision: number;
  clientBounds: MagiDesktopRectangle;
  displayScaleFactor: number;
  fullscreen: boolean;
  rightPaneVisible: boolean;
  rightPaneMode: 'side-by-side' | 'overlay';
  rightPaneWidth: number;
  activePanelKind: MagiDesktopPanelKind;
  activeTabId: string | null;
  activeSurfaceId: string | null;
  appBounds: MagiDesktopRectangle;
  dividerBounds: MagiDesktopRectangle | null;
  rightPaneBounds: MagiDesktopRectangle | null;
  browserSurfaceBounds: MagiDesktopRectangle | null;
}

interface MagiDesktopWindowSnapshot {
  desktopEpoch: string;
  windowId: string;
  snapshotRevision: number;
  layout: MagiDesktopWindowLayoutSnapshot;
  activeBrowserViewport: MagiDesktopLogicalViewport | null;
}

interface MagiDesktopContextSnapshot {
  contextRevision: number;
  windowId?: string;
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
}

type MagiDesktopLayoutIntent =
  | { type: 'right_pane_width'; width: number }
  | { type: 'right_pane_reset_width' }
  | { type: 'right_pane_visibility'; visible: boolean };

type MagiDesktopLogicalViewport =
  | { mode: 'auto' }
  | {
      mode: 'fixed';
      width: number;
      height: number;
      device_scale_factor_millis: number;
      device_type: 'desktop' | 'mobile';
    };

type MagiDesktopViewportIntent =
  | { mode: 'auto' }
  | {
      mode: 'fixed';
      width: number;
      height: number;
      deviceScaleFactorMillis: number;
      deviceType: 'desktop' | 'mobile';
    };

interface MagiDesktopBrowserActivationRequest {
  tabId: string;
  browserSessionId: string;
  url: string;
  navigationRevision: number;
  viewport: MagiDesktopViewportIntent;
}

type MagiDesktopRightPaneIntentEnvelope = import('@magi/desktop-browser-contracts').DesktopRightPaneIntentEnvelope;

interface MagiDesktopUpdateSnapshot {
  status: 'idle' | 'checking' | 'available' | 'downloading' | 'downloaded' | 'failed' | 'unsupported';
  currentVersion: string;
  availableVersion: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  percent: number | null;
  error: string | null;
}

interface MagiDesktopBrowserComponentInfo {
  protocol_version: { major: number; minor: number };
  desktop_version: string;
  electron_version: string;
  chromium_version: string;
  process_id: number;
  desktop_epoch: string;
  worker_epoch: string;
  daemon_version: string;
  daemon_status: 'starting' | 'ready' | 'restarting' | 'failed' | 'stopped';
  daemon_process_id: number | null;
  automation_worker_version: string;
  automation_worker_status: 'starting' | 'ready' | 'restarting' | 'failed' | 'stopped';
  protocol_compatible: boolean;
}

type MagiDesktopContextMenuRole = 'undo' | 'redo' | 'cut' | 'copy' | 'paste' | 'selectAll';

type MagiDesktopContextMenuItem =
  | { type: 'role'; role: MagiDesktopContextMenuRole }
  | { type: 'separator' }
  | { type: 'action'; id: string; label: string; enabled?: boolean };

type MagiDesktopFileDropEvent =
  | { type: 'enter'; paths: string[]; position: { x: number; y: number } }
  | { type: 'over'; position: { x: number; y: number } }
  | { type: 'drop'; paths: string[]; position: { x: number; y: number } }
  | { type: 'leave' };

interface MagiDesktopBridge {
  readonly runtime: 'electron';
  readonly surface: 'app' | 'right-pane' | 'overlay' | null;
  readonly windowId: string | null;
  getSnapshot(): Promise<MagiDesktopWindowSnapshot>;
  setContext(context: {
    workspaceId: string;
    workspacePath: string;
    sessionId: string;
  }): Promise<MagiDesktopContextSnapshot>;
  submitLayoutIntent(intent: MagiDesktopLayoutIntent): Promise<MagiDesktopWindowSnapshot>;
  activateBrowser(request: MagiDesktopBrowserActivationRequest): Promise<MagiDesktopWindowSnapshot>;
  activatePanel(request: { kind: MagiDesktopPanelKind; tabId: string | null }): Promise<MagiDesktopWindowSnapshot>;
  setBrowserViewport(request: {
    tabId: string;
    viewport: MagiDesktopViewportIntent;
  }): Promise<MagiDesktopWindowSnapshot>;
  openRightPaneTab(request: MagiDesktopRightPaneIntentEnvelope): Promise<MagiDesktopWindowSnapshot>;
  readyRightPane(): Promise<void>;
  openOverlay(state: Omit<MagiDesktopOverlayState, 'overlayId' | 'phase'> & { overlayId?: string; phase?: MagiDesktopOverlayState['phase'] }): Promise<void>;
  closeOverlay(): Promise<void>;
  readyOverlay(): Promise<void>;
  submitOverlayAction(action: MagiDesktopOverlayAction): Promise<void>;
  focusBrowser(surfaceId: string): Promise<void>;
  openExternal(url: string): Promise<void>;
  showContextMenu(request: { items: MagiDesktopContextMenuItem[] }): Promise<string | null>;
  openWorkspaceFolder(workspaceRootPathRef: string): Promise<void>;
  revealWorkspaceFile(request: {
    targetPathRef: string;
    workspaceRootPathRef: string;
  }): Promise<void>;
  setAppearance(appearance: { backgroundColor: string; mode: 'light' | 'dark' }): Promise<void>;
  getAppVersion(): Promise<string>;
  getBrowserComponentInfo(): Promise<MagiDesktopBrowserComponentInfo>;
  restartBrowserAutomation(): Promise<MagiDesktopBrowserComponentInfo>;
  clearBrowserData(): Promise<void>;
  checkForUpdates(): Promise<MagiDesktopUpdateSnapshot>;
  downloadUpdate(): Promise<MagiDesktopUpdateSnapshot>;
  installUpdate(): Promise<never>;
  onSnapshot(listener: (snapshot: MagiDesktopWindowSnapshot) => void): () => void;
  onContext(listener: (context: MagiDesktopContextSnapshot) => void): () => void;
  onBrowserEvent(listener: (event: unknown) => void): () => void;
  onRightPaneIntent(listener: (request: MagiDesktopRightPaneIntentEnvelope) => void): () => void;
  onOverlayState(listener: (state: MagiDesktopOverlayState) => void): () => void;
  onOverlayClosed(listener: () => void): () => void;
  onOverlayAction(listener: (action: MagiDesktopOverlayAction) => void): () => void;
  onUpdate(listener: (snapshot: MagiDesktopUpdateSnapshot) => void): () => void;
  onFileDrop(listener: (event: MagiDesktopFileDropEvent) => void): () => void;
}

interface Window {
  magiDesktop?: MagiDesktopBridge;
}
