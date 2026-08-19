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
  placement: 'right-pane-add' | 'browser-viewport' | 'browser-annotations';
  anchorBounds: MagiDesktopRectangle | null;
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

interface MagiDesktopUpdateSnapshot {
  status: 'idle' | 'checking' | 'available' | 'downloading' | 'downloaded' | 'failed' | 'unsupported';
  currentVersion: string;
  availableVersion: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
  percent: number | null;
  error: string | null;
}

type MagiDesktopBrowserComponentStatus = 'starting' | 'ready' | 'restarting' | 'failed' | 'stopped';

interface MagiDesktopBrowserComponentError {
  target: 'daemon' | 'worker' | 'protocol';
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
    compatible: boolean;
    error: MagiDesktopBrowserComponentError | null;
  };
  error: MagiDesktopBrowserComponentError | null;
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
  readonly surface: 'app' | 'overlay' | null;
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
  readyRightPane(): Promise<void>;
  focusApp(): Promise<void>;
  openOverlay(state: Omit<MagiDesktopOverlayState, 'overlayId' | 'phase'> & { overlayId?: string; phase?: MagiDesktopOverlayState['phase'] }): Promise<void>;
  closeOverlay(): Promise<void>;
  setBlockingOverlay(request: { active: boolean }): Promise<MagiDesktopWindowSnapshot>;
  readyOverlay(): Promise<void>;
  submitOverlayAction(action: MagiDesktopOverlayAction): Promise<void>;
  openExternal(url: string): Promise<void>;
  showContextMenu(request: { items: MagiDesktopContextMenuItem[] }): Promise<string | null>;
  openWorkspaceFolder(workspaceRootPathRef: string): Promise<void>;
  revealWorkspaceFile(request: {
    targetPathRef: string;
    workspaceRootPathRef: string;
  }): Promise<void>;
  setAppearance(appearance: {
    backgroundColor: string;
    accentColor: string;
    material: 'clear' | 'translucent' | 'immersive';
    mode: 'light' | 'dark';
  }): Promise<void>;
  getAppVersion(): Promise<string>;
  getBrowserComponentInfo(): Promise<MagiDesktopBrowserComponentSnapshot>;
  restartBrowserAutomation(): Promise<MagiDesktopBrowserComponentSnapshot>;
  clearBrowserData(): Promise<void>;
  checkForUpdates(): Promise<MagiDesktopUpdateSnapshot>;
  downloadUpdate(): Promise<MagiDesktopUpdateSnapshot>;
  installUpdate(): Promise<never>;
  onSnapshot(listener: (snapshot: MagiDesktopWindowSnapshot) => void): () => void;
  onContext(listener: (context: MagiDesktopContextSnapshot) => void): () => void;
  onBrowserEvent(listener: (event: unknown) => void): () => void;
  onBrowserComponent(listener: (snapshot: MagiDesktopBrowserComponentSnapshot) => void): () => void;
  onOverlayState(listener: (state: MagiDesktopOverlayState) => void): () => void;
  onOverlayClosed(listener: () => void): () => void;
  onOverlayAction(listener: (action: MagiDesktopOverlayAction) => void): () => void;
  onUpdate(listener: (snapshot: MagiDesktopUpdateSnapshot) => void): () => void;
  onFileDrop(listener: (event: MagiDesktopFileDropEvent) => void): () => void;
}

interface Window {
  magiDesktop?: MagiDesktopBridge;
}
