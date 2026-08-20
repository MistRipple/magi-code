import { randomBytes, randomUUID } from "node:crypto";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { homedir, tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import {
  app,
  BaseWindow,
  ipcMain,
  Menu,
  nativeTheme,
  shell,
  type MenuItemConstructorOptions,
  type Rectangle,
} from "electron";
import {
  DESKTOP_BROWSER_PROTOCOL_VERSION,
  type BrowserLogicalViewport,
  type DesktopBrowserHandshake,
} from "@magi/desktop-browser-contracts";
import { AutomationWorker } from "./automation-worker.js";
import { BrowserSurfaceManager, type BrowserSurfaceEvent } from "./browser-surface-manager.js";
import { DesktopOverlayManager } from "./desktop-overlay-manager.js";
import { DesktopControlServer } from "./desktop-control-server.js";
import { openWorkspaceFolder, revealWorkspaceFile } from "./desktop-files.js";
import { ProcessSupervisor } from "./process-supervisor.js";
import { UpdateManager } from "./update-manager.js";
import { WindowManager } from "./window-manager.js";
import type { PanelKind, WindowLayoutIntent } from "./window-layout.js";

const stateRoot = process.env.MAGI_STATE_ROOT?.trim() || join(homedir(), ".magi");
app.setPath("userData", join(stateRoot, "desktop"));
app.commandLine.appendSwitch("use-mock-keychain");
if (process.platform === "linux") app.commandLine.appendSwitch("password-store", "basic");
app.commandLine.appendSwitch("disable-background-networking");
app.commandLine.appendSwitch("disable-component-update");
app.commandLine.appendSwitch("disable-default-apps");
app.commandLine.appendSwitch("disable-sync");
app.commandLine.appendSwitch("no-default-browser-check");
app.commandLine.appendSwitch("no-first-run");
app.commandLine.appendSwitch(
  "disable-features",
  "AutofillServerCommunication,MediaRouter,OptimizationHints,PasswordManagerOnboarding",
);

const AGENT_ORIGIN = "http://127.0.0.1:38123";
const BROWSER_CAPABILITY_MANIFEST = readBrowserCapabilityManifest();
const PRODUCT_VERSION = process.env.MAGI_PRODUCT_VERSION
  || BROWSER_CAPABILITY_MANIFEST.productVersion
  || "unknown";
const DAEMON_VERSION = BROWSER_CAPABILITY_MANIFEST.daemonVersion || PRODUCT_VERSION;
const AUTOMATION_WORKER_VERSION = BROWSER_CAPABILITY_MANIFEST.automationWorkerVersion || PRODUCT_VERSION;
const desktopEpoch = `desktop-${randomUUID()}`;
const controlToken = randomBytes(32).toString("hex");
const controlSocket = process.platform === "win32"
  ? `\\\\.\\pipe\\magi-${process.pid}`
  : join(tmpdir(), `magi-${process.pid}.sock`);
const windows = new Map<string, BaseWindow>();

let windowManager: WindowManager | null = null;
let surfaceManager: BrowserSurfaceManager | null = null;
let automationWorker: AutomationWorker | null = null;
let controlServer: DesktopControlServer | null = null;
let processSupervisor: ProcessSupervisor | null = null;
let updateManager: UpdateManager | null = null;
let desktopConnectionGeneration: number | null = null;
let workerInvalidatedDesktopConnection = false;
let browserComponentError: BrowserComponentError | null = null;
let browserComponentSnapshotTimer: NodeJS.Timeout | null = null;
let lastBrowserComponentSnapshot = "";
let daemonHostStatus: BrowserComponentStatus | null = null;
let daemonHostProtocolCompatible = false;
let daemonHostErrorCode: string | null = null;
let daemonHostProbeAt = 0;
let daemonHostProbe: Promise<void> | null = null;
let shuttingDown = false;

const singleInstance = app.requestSingleInstanceLock();
if (!singleInstance) app.quit();

if (singleInstance) {
  app.on("second-instance", () => {
    const manager = windowManager;
    if (!manager) return;
    const window = windows.get(manager.activeWindowId());
    if (!window || window.isDestroyed()) return;
    if (window.isMinimized()) window.restore();
    window.show();
    window.focus();
  });

  app.whenReady().then(async () => {
  const paths = resolveRuntimePaths();
  let control: DesktopControlServer | null = null;
  let worker: AutomationWorker | null = null;
  const surfaces = new BrowserSurfaceManager({
    desktopEpoch,
    partitionRegistryPath: join(app.getPath("userData"), "browser-partitions.json"),
    onEvent: (event) => {
      control?.handleSurfaceEvent(event);
      publishBrowserEvent(event);
    },
  });
  const browserUploadRoot = join(app.getPath("userData"), "browser-uploads");
  mkdirSync(browserUploadRoot, { recursive: true });
  surfaceManager = surfaces;
  const overlays = new DesktopOverlayManager({
    preloadPath: paths.preload,
    agentOrigin: AGENT_ORIGIN,
    desktopEpoch,
    onAction: (windowId, action) => {
      windowManager?.broadcast(windowId, "magi-desktop:overlay-action", action);
    },
    onClosed: (windowId) => {
      windowManager?.broadcast(windowId, "magi-desktop:overlay-closed", null);
    },
  });
  worker = new AutomationWorker({
    entryPath: paths.workerEntry,
    surfaceManager: surfaces,
    uploadRoot: browserUploadRoot,
    onFailure: async (cause) => {
      if (shuttingDown) return;
      workerInvalidatedDesktopConnection = desktopConnectionGeneration !== null;
      browserComponentError = componentError("worker", "browser_worker_failed", cause);
      // Releasing the local Surface flag is not enough: the daemon owns the
      // authoritative Browser Lease. Removing this Desktop registration makes
      // the daemon revoke every lease before the recovered Worker reconnects.
      await unregisterDesktopBrowserConnection();
    },
    onReady: async () => {
      if (shuttingDown || !workerInvalidatedDesktopConnection) return;
      await registerDesktopBrowserConnection();
      workerInvalidatedDesktopConnection = false;
      await windowManager?.restoreAfterDaemonReady();
    },
  });
  automationWorker = worker;
  const manager = new WindowManager({
    desktopEpoch,
    preloadPath: paths.preload,
    agentOrigin: AGENT_ORIGIN,
    surfaceManager: surfaces,
    overlayManager: overlays,
    windows,
    onSnapshot: (snapshot) => {
      try {
        windowManager?.broadcast(snapshot.windowId, "magi-desktop:snapshot", snapshot);
      } catch {
        // Window may have closed between reducer and publication.
      }
    },
  });
  windowManager = manager;
  // Overlay Renderer 会在加载完成后立即发送 ready 握手。IPC 必须先于
  // 创建窗口注册，否则启动阶段的握手会落在“无 handler”窗口期，Overlay
  // 永远保持零尺寸，菜单和标记选择都会表现为可见但无法交互。
  registerIpc();
  control = new DesktopControlServer({
    socketPath: controlSocket,
    token: controlToken,
    surfaceManager: surfaces,
    worker,
    activeWindowId: () => manager.activeWindowId(),
    handshake: () => handshake(worker!),
  });
  controlServer = control;
  await control.start();
  processSupervisor = new ProcessSupervisor({
    daemonPath: paths.daemon,
    agentOrigin: AGENT_ORIGIN,
    onReady: handleDaemonReady,
    environment: {
      ...process.env,
      MAGI_HOST: "127.0.0.1",
      MAGI_PORT: "38123",
      MAGI_OPEN_BROWSER: "0",
      MAGI_DESKTOP_PARENT_PID: String(process.pid),
      MAGI_DESKTOP_EPOCH: desktopEpoch,
      MAGI_DESKTOP_CONTROL_SOCKET: controlSocket,
      MAGI_DESKTOP_CONTROL_TOKEN: controlToken,
      MAGI_STATE_ROOT: stateRoot,
      ...(app.isPackaged
        ? { MAGI_WEB_DIST_ROOT: paths.webDist }
        : { MAGI_WEB_DEV: "1", MAGI_WEB_DEV_ROOT: paths.webRoot }),
    },
  });
  await processSupervisor.start();
  // 先让 daemon、桌面控制端点和前端入口全部就绪，再创建唯一桌面窗口。
  // 如果窗口早于 daemon 加载 Renderer，首次 loadURL 会收到 connection refused；
  // 这会留下一个只有原生外框的空白窗口，并把后续 Browser Surface 恢复带入竞态。
  // 外部 daemon 复用路径同样在这里完成注册，因此不需要提前创建窗口。
  manager.createWindow();
  // daemon、窗口控制端点和 Renderer 生命周期先建立，再启动 Worker。
  // 这样启动失败时不会留下一个脱离桌面宿主的浏览器自动化进程。
  worker.start();
  updateManager = new UpdateManager(PRODUCT_VERSION, (snapshot) => {
    broadcastAll("magi-desktop:update", snapshot);
  });
  startBrowserComponentSnapshots();
  }).catch(async (error) => {
    console.error("Magi Desktop 启动失败", error);
    await shutdown().catch((cleanupError) => {
      console.error("Magi Desktop 启动清理失败", cleanupError);
    });
    app.exit(1);
  });
}

app.on("window-all-closed", () => app.quit());
app.on("before-quit", (event) => {
  if (shuttingDown) return;
  event.preventDefault();
  shuttingDown = true;
  void shutdown().finally(() => app.exit(0));
});

function registerIpc(): void {
  ipcMain.handle("magi-desktop:get-snapshot", (event) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    return manager.snapshot(windowId);
  });
  ipcMain.handle("magi-desktop:layout-intent", (event, value: unknown) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    return manager.submitLayoutIntent(windowId, parseLayoutIntent(value));
  });
  ipcMain.handle("magi-desktop:set-context", (event, value: unknown) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    const request = rejectUnknownFields(
      object(value),
      ["workspaceId", "workspacePath", "sessionId"],
      "context",
    );
    return manager.setRendererContext(windowId, {
      workspaceId: optionalText(request.workspaceId, "workspaceId"),
      workspacePath: optionalText(request.workspacePath, "workspacePath"),
      sessionId: optionalText(request.sessionId, "sessionId"),
    });
  });
  ipcMain.handle("magi-desktop:activate-browser", async (event, value: unknown) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    const request = parseBrowserActivation(value);
    return manager.activateBrowser({ windowId, ...request });
  });
  ipcMain.handle("magi-desktop:activate-panel", (event, value: unknown) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    const request = rejectUnknownFields(object(value), ["kind", "tabId"], "activatePanel");
    return manager.activatePanel(
      windowId,
      parsePanelKind(request.kind),
      typeof request.tabId === "string" ? request.tabId : null,
    );
  });
  ipcMain.handle("magi-desktop:set-browser-viewport", async (event, value: unknown) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    const request = rejectUnknownFields(object(value), ["tabId", "viewport"], "browserViewport");
    return manager.setBrowserViewport(
      windowId,
      text(request.tabId, "tabId"),
      parseViewport(request.viewport),
    );
  });
  ipcMain.handle("magi-desktop:focus-app", (event) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    manager.focusApp(windowId);
  });
  ipcMain.handle("magi-desktop:right-pane-ready", (event) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    manager.handleRightPaneReady(windowId);
  });
  ipcMain.handle("magi-desktop:open-overlay", (event, value: unknown) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    manager.openOverlay(windowId, parseOverlayState(value));
  });
  ipcMain.handle("magi-desktop:close-overlay", (event) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    const role = manager.rendererRoleForWebContents(event.sender.id);
    if (role !== "app" && role !== "overlay") {
      throw new Error("desktop_overlay_close_sender_denied");
    }
    manager.closeOverlay(windowId);
  });
  ipcMain.handle("magi-desktop:set-blocking-overlay", (event, value: unknown) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    const request = rejectUnknownFields(object(value), ["active"], "blockingOverlay");
    if (typeof request.active !== "boolean") throw new Error("desktop_blocking_overlay_invalid");
    return manager.setBlockingOverlay(windowId, request.active);
  });
  ipcMain.handle("magi-desktop:overlay-ready", (event) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    if (manager.rendererRoleForWebContents(event.sender.id) !== "overlay") {
      throw new Error("desktop_overlay_ready_sender_denied");
    }
    manager.handleOverlayReady(windowId);
  });
  ipcMain.handle("magi-desktop:overlay-action", (event, value: unknown) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    if (manager.rendererRoleForWebContents(event.sender.id) !== "overlay") {
      throw new Error("desktop_overlay_action_sender_denied");
    }
    manager.handleOverlayAction(windowId, parseOverlayAction(value));
  });
  ipcMain.handle("magi-desktop:open-external", async (event, value: unknown) => {
    trustedAppSender(event.sender.id);
    if (typeof value !== "string") throw new Error("external_url_invalid");
    const url = new URL(value);
    if (!["http:", "https:"].includes(url.protocol)) throw new Error("external_url_invalid");
    await shell.openExternal(url.href);
  });
  ipcMain.handle("magi-desktop:show-context-menu", async (event, value: unknown) => {
    const { windowId } = trustedAppSender(event.sender.id);
    return showContextMenu(windowId, parseContextMenuItems(value));
  });
  ipcMain.handle("magi-desktop:open-workspace-folder", async (event, value: unknown) => {
    trustedAppSender(event.sender.id);
    await openWorkspaceFolder(text(value, "workspaceRootPathRef"));
  });
  ipcMain.handle("magi-desktop:reveal-workspace-file", async (event, value: unknown) => {
    trustedAppSender(event.sender.id);
    const request = rejectUnknownFields(
      object(value),
      ["targetPathRef", "workspaceRootPathRef"],
      "revealWorkspaceFile",
    );
    await revealWorkspaceFile({
      targetPathRef: text(request.targetPathRef, "targetPathRef"),
      workspaceRootPathRef: text(request.workspaceRootPathRef, "workspaceRootPathRef"),
    });
  });
  ipcMain.handle("magi-desktop:set-appearance", (event, value: unknown) => {
    const { manager, windowId } = trustedAppSender(event.sender.id);
    const request = rejectUnknownFields(
      object(value),
      ["mode", "backgroundColor", "accentColor", "material"],
      "appearance",
    );
    const mode = request.mode;
    const backgroundColor = text(request.backgroundColor, "backgroundColor");
    const accentColor = text(request.accentColor, "accentColor");
    const material = request.material;
    if (mode !== "light" && mode !== "dark") throw new Error("desktop_appearance_mode_invalid");
    if (!isDesktopBackgroundColor(backgroundColor)) {
      throw new Error("desktop_appearance_background_invalid");
    }
    if (!isDesktopBackgroundColor(accentColor)) {
      throw new Error("desktop_appearance_accent_invalid");
    }
    if (material !== "clear" && material !== "translucent" && material !== "immersive") {
      throw new Error("desktop_appearance_material_invalid");
    }
    nativeTheme.themeSource = mode;
    manager.setAppearance(windowId, { mode, backgroundColor, accentColor, material });
  });
  ipcMain.handle("magi-desktop:get-app-version", (event) => {
    trustedAppSender(event.sender.id);
    return PRODUCT_VERSION;
  });
  ipcMain.handle("magi-desktop:get-browser-component-info", (event) => {
    trustedAppSender(event.sender.id);
    return browserComponentSnapshot();
  });
  ipcMain.handle("magi-desktop:restart-browser-automation", async (event) => {
    trustedAppSender(event.sender.id);
    try {
      await automationWorker!.restart();
      browserComponentError = null;
      return browserComponentSnapshot();
    } catch (cause) {
      browserComponentError = componentError("worker", "browser_worker_restart_failed", cause);
      throw cause;
    }
  });
  ipcMain.handle("magi-desktop:clear-browser-data", async (event) => {
    trustedAppSender(event.sender.id);
    await surfaceManager!.clearBrowsingData();
  });
  ipcMain.handle("magi-desktop:check-for-updates", (event) => {
    trustedAppSender(event.sender.id);
    return updateManager!.check();
  });
  ipcMain.handle("magi-desktop:download-update", (event) => {
    trustedAppSender(event.sender.id);
    return updateManager!.download();
  });
  ipcMain.handle("magi-desktop:install-update", (event) => {
    trustedAppSender(event.sender.id);
    return updateManager!.install();
  });
}

type ContextMenuRole = "undo" | "redo" | "cut" | "copy" | "paste" | "selectAll";

type ContextMenuItem =
  | { type: "role"; role: ContextMenuRole }
  | { type: "separator" }
  | { type: "action"; id: string; label: string; enabled: boolean };

function showContextMenu(windowId: string, items: ContextMenuItem[]): Promise<string | null> {
  const window = windows.get(windowId);
  if (!window || window.isDestroyed()) throw new Error("desktop_window_not_found");
  return new Promise((resolveMenu) => {
    let selectedAction: string | null = null;
    const template: MenuItemConstructorOptions[] = items.map((item) => {
      if (item.type === "role") return { role: item.role };
      if (item.type === "separator") return { type: "separator" };
      return {
        label: item.label,
        enabled: item.enabled,
        click: () => {
          selectedAction = item.id;
        },
      };
    });
    const menu = Menu.buildFromTemplate(template);
    menu.popup({
      window,
      callback: () => resolveMenu(selectedAction),
    });
  });
}

function parseContextMenuItems(value: unknown): ContextMenuItem[] {
  const request = rejectUnknownFields(object(value), ["items"], "contextMenu");
  if (!Array.isArray(request.items) || request.items.length === 0 || request.items.length > 32) {
    throw new Error("desktop_context_menu_items_invalid");
  }
  const allowedRoles = new Set<ContextMenuRole>([
    "undo",
    "redo",
    "cut",
    "copy",
    "paste",
    "selectAll",
  ]);
  return request.items.map((rawItem) => {
    const item = object(rawItem);
    if (item.type === "separator") {
      rejectUnknownFields(item, ["type"], "contextMenu.separator");
      return { type: "separator" };
    }
    if (item.type === "role" && allowedRoles.has(item.role as ContextMenuRole)) {
      rejectUnknownFields(item, ["type", "role"], "contextMenu.role");
      return { type: "role", role: item.role as ContextMenuRole };
    }
    if (item.type !== "action") throw new Error("desktop_context_menu_item_invalid");
    rejectUnknownFields(item, ["type", "id", "label", "enabled"], "contextMenu.action");
    const id = text(item.id, "contextMenu.id");
    const label = text(item.label, "contextMenu.label");
    if (!/^[a-z][a-z0-9-]{0,63}$/u.test(id) || label.length > 160) {
      throw new Error("desktop_context_menu_item_invalid");
    }
    return { type: "action", id, label, enabled: item.enabled !== false };
  });
}

function handshake(worker: AutomationWorker): DesktopBrowserHandshake {
  return {
    protocol_version: DESKTOP_BROWSER_PROTOCOL_VERSION,
    desktop_version: PRODUCT_VERSION,
    electron_version: process.versions.electron,
    chromium_version: process.versions.chrome,
    process_id: process.pid,
    desktop_epoch: desktopEpoch,
    worker_epoch: worker.workerEpoch,
  };
}

type BrowserComponentStatus = "starting" | "ready" | "restarting" | "failed" | "stopped";
type BrowserProtocolStatus = BrowserComponentStatus | "incompatible";
type BrowserComponentErrorTarget = "daemon" | "worker" | "protocol" | "version";

interface BrowserComponentError {
  target: BrowserComponentErrorTarget;
  code: string;
  message: string;
}

interface BrowserCapabilityManifest {
  productVersion?: string;
  daemonVersion?: string;
  automationWorkerVersion?: string;
}

function browserComponentSnapshot() {
  const worker = automationWorker;
  const daemon = processSupervisor;
  const daemonStatus = daemon?.status ?? "stopped";
  const workerStatus = worker?.status ?? "stopped";
  const protocolCompatible = daemonStatus === "ready"
    && desktopConnectionGeneration !== null
    && daemonHostProtocolCompatible;
  const protocolStatus = browserProtocolStatus(
    daemonStatus,
    daemonHostStatus,
    protocolCompatible,
    daemonHostErrorCode,
  );
  const versionCompatible = componentVersionsCompatible();
  const error = currentBrowserComponentError(
    daemonStatus,
    workerStatus,
    protocolStatus,
    versionCompatible,
  );
  const runtimeStatus = browserRuntimeStatus(
    daemonStatus,
    workerStatus,
    protocolStatus,
    versionCompatible,
  );
  return {
    product_version: PRODUCT_VERSION,
    electron_version: process.versions.electron,
    chromium_version: process.versions.chrome,
    process_id: process.pid,
    desktop_epoch: desktopEpoch,
    daemon: {
      version: DAEMON_VERSION,
      status: daemonStatus,
      process_id: daemon?.processId ?? null,
    },
    worker: {
      version: AUTOMATION_WORKER_VERSION,
      epoch: worker?.workerEpoch ?? "",
      status: workerStatus,
    },
    protocol: {
      version: DESKTOP_BROWSER_PROTOCOL_VERSION,
      status: protocolStatus,
      compatible: protocolCompatible,
      error: (protocolStatus === "incompatible" || protocolStatus === "failed")
        && error?.target === "protocol" ? error : null,
    },
    runtime: {
      status: runtimeStatus,
      ready: runtimeStatus === "ready",
      version_compatible: versionCompatible,
      error,
    },
    error,
  };
}

function componentVersionsCompatible(): boolean {
  return PRODUCT_VERSION !== "unknown"
    && DAEMON_VERSION !== "unknown"
    && AUTOMATION_WORKER_VERSION !== "unknown"
    && PRODUCT_VERSION === DAEMON_VERSION
    && PRODUCT_VERSION === AUTOMATION_WORKER_VERSION;
}

function browserProtocolStatus(
  daemonStatus: BrowserComponentStatus,
  hostStatus: BrowserComponentStatus | null,
  protocolCompatible: boolean,
  hostErrorCode: string | null,
): BrowserProtocolStatus {
  if (daemonStatus !== "ready") return daemonStatus;
  if (hostErrorCode === "browser_protocol_incompatible") return "incompatible";
  if (hostStatus === "failed") return "failed";
  if (hostStatus === "restarting") return "restarting";
  if (hostStatus === "stopped") return "stopped";
  if (hostStatus === "ready" && protocolCompatible) return "ready";
  return "starting";
}

function browserRuntimeStatus(
  daemonStatus: BrowserComponentStatus,
  workerStatus: BrowserComponentStatus,
  protocolStatus: BrowserProtocolStatus,
  versionCompatible: boolean,
): BrowserComponentStatus {
  if (daemonStatus !== "ready") return daemonStatus;
  if (workerStatus !== "ready") return workerStatus;
  if (!versionCompatible || protocolStatus === "incompatible") return "failed";
  if (protocolStatus === "failed") return "failed";
  if (protocolStatus === "restarting") return "restarting";
  if (protocolStatus === "stopped") return "stopped";
  if (protocolStatus !== "ready") return "starting";
  return "ready";
}

function currentBrowserComponentError(
  daemonStatus: BrowserComponentStatus,
  workerStatus: BrowserComponentStatus,
  protocolStatus: BrowserProtocolStatus,
  versionCompatible: boolean,
): BrowserComponentError | null {
  if (daemonStatus === "failed") {
    return browserComponentError?.target === "daemon"
      ? browserComponentError
      : { target: "daemon", code: "browser_daemon_failed", message: "Browser daemon failed" };
  }
  if (workerStatus === "failed") {
    return browserComponentError?.target === "worker"
      ? browserComponentError
      : { target: "worker", code: "browser_worker_failed", message: "Browser automation worker failed" };
  }
  // starting/restarting/stopped 是生命周期状态，不应被当成错误；界面会
  // 为每个组件单独展示这些状态。
  if (daemonStatus !== "ready" || workerStatus !== "ready") return null;
  if (!versionCompatible) {
    return browserComponentError?.target === "version"
      ? browserComponentError
      : {
        target: "version",
        code: "browser_component_version_mismatch",
        message: `Browser component versions are inconsistent: product=${PRODUCT_VERSION}, daemon=${DAEMON_VERSION}, worker=${AUTOMATION_WORKER_VERSION}`,
      };
  }
  if (protocolStatus === "incompatible") {
    return browserComponentError?.target === "protocol"
      ? browserComponentError
      : { target: "protocol", code: "browser_protocol_incompatible", message: "Browser protocol is incompatible" };
  }
  if (protocolStatus === "failed") {
    return browserComponentError?.target === "protocol"
      ? browserComponentError
      : { target: "protocol", code: "browser_protocol_failed", message: "Browser protocol failed" };
  }
  return null;
}

function componentError(target: BrowserComponentErrorTarget, code: string, cause: unknown): BrowserComponentError {
  return {
    target,
    code,
    message: cause instanceof Error ? cause.message : String(cause),
  };
}

function startBrowserComponentSnapshots(): void {
  if (browserComponentSnapshotTimer) return;
  publishBrowserComponentSnapshot();
  void refreshDaemonHostStatus(true);
  browserComponentSnapshotTimer = setInterval(() => {
    void refreshDaemonHostStatus();
    publishBrowserComponentSnapshot();
  }, 250);
  browserComponentSnapshotTimer.unref();
}

function publishBrowserComponentSnapshot(): void {
  const snapshot = browserComponentSnapshot();
  const serialized = JSON.stringify(snapshot);
  if (serialized === lastBrowserComponentSnapshot) return;
  lastBrowserComponentSnapshot = serialized;
  broadcastAll("magi-desktop:browser-component", snapshot);
}

async function refreshDaemonHostStatus(force = false): Promise<void> {
  const daemon = processSupervisor;
  if (!daemon || daemon.status !== "ready") return;
  const now = Date.now();
  if (!force && now - daemonHostProbeAt < 500) return;
  if (daemonHostProbe) return daemonHostProbe;
  daemonHostProbeAt = now;
  daemonHostProbe = (async () => {
    try {
      const response = await fetch(`${AGENT_ORIGIN}/api/browser/desktop/connection`, {
        cache: "no-store",
      });
      if (!response.ok) throw new Error(`browser_host_status_failed:${response.status}`);
      const payload: unknown = await response.json();
      updateDaemonHostStatus(payload);
      publishBrowserComponentSnapshot();
    } catch {
      // A short polling failure must not turn a healthy component into a
      // false failure. The process supervisor remains the source of daemon
      // process state; keep the last host state until the next successful read.
    } finally {
      daemonHostProbe = null;
    }
  })();
  return daemonHostProbe;
}

function updateDaemonHostStatus(value: unknown): void {
  if (!value || typeof value !== "object") throw new Error("browser_host_status_invalid");
  const record = value as Record<string, unknown>;
  const status = normalizeDaemonHostStatus(record.hostStatus);
  if (!status) throw new Error("browser_host_status_invalid");
  daemonHostStatus = status;
  daemonHostProtocolCompatible = record.hostProtocolCompatible === true;
  daemonHostErrorCode = typeof record.lastErrorCode === "string" ? record.lastErrorCode : null;
}

function normalizeDaemonHostStatus(value: unknown): BrowserComponentStatus | null {
  switch (value) {
    case "stopped": return "stopped";
    case "starting": return "starting";
    case "ready": return "ready";
    case "reconnecting": return "restarting";
    case "failed": return "failed";
    default: return null;
  }
}

function readBrowserCapabilityManifest(): BrowserCapabilityManifest {
  const path = join(process.resourcesPath, "browser-capability-manifest.json");
  if (!existsSync(path)) return {};
  try {
    const value: unknown = JSON.parse(readFileSync(path, "utf8"));
    if (!value || typeof value !== "object") return {};
    const record = value as Record<string, unknown>;
    return {
      ...(typeof record.productVersion === "string" ? { productVersion: record.productVersion } : {}),
      ...(typeof record.daemonVersion === "string" ? { daemonVersion: record.daemonVersion } : {}),
      ...(typeof record.automationWorkerVersion === "string"
        ? { automationWorkerVersion: record.automationWorkerVersion }
        : {}),
    };
  } catch {
    return {};
  }
}

function trustedSender(webContentsId: number): { manager: WindowManager; windowId: string } {
  const manager = windowManager;
  if (!manager) throw new Error("desktop_not_ready");
  const windowId = manager.windowIdForWebContents(webContentsId);
  if (!windowId) throw new Error("desktop_ipc_sender_denied");
  return { manager, windowId };
}

function trustedAppSender(webContentsId: number): { manager: WindowManager; windowId: string } {
  const sender = trustedSender(webContentsId);
  if (sender.manager.rendererRoleForWebContents(webContentsId) !== "app") {
    throw new Error("desktop_app_sender_denied");
  }
  return sender;
}

function publishBrowserEvent(event: BrowserSurfaceEvent): void {
  // Surface 事件属于创建它的窗口。广播给所有窗口会让同一逻辑 Tab 的
  // Secondary Surface 污染另一个窗口的地址、加载状态和 Agent 光标。
  try {
    windowManager?.broadcast(event.binding.window_id, "magi-desktop:browser-event", event);
  } catch {
    // 目标窗口可能刚好关闭，WindowManager 会负责清理它的 Surface。
  }
}

function broadcastAll(channel: string, value: unknown): void {
  const manager = windowManager;
  if (!manager) return;
  for (const windowId of windows.keys()) {
    try {
      manager.broadcast(windowId, channel, value);
    } catch {
      // Closed windows are removed by WindowManager.
    }
  }
}

async function shutdown(): Promise<void> {
  if (browserComponentSnapshotTimer) clearInterval(browserComponentSnapshotTimer);
  browserComponentSnapshotTimer = null;
  lastBrowserComponentSnapshot = "";
  windowManager?.closeAll();
  surfaceManager?.closeAll();
  automationWorker?.stop();
  await processSupervisor?.stop();
  await unregisterDesktopBrowserConnection();
  await controlServer?.close();
}

async function registerDesktopBrowserConnection(): Promise<void> {
  const currentResponse = await fetch(`${AGENT_ORIGIN}/api/browser/desktop/connection`, {
    cache: "no-store",
  });
  if (!currentResponse.ok) {
    throw new Error(`browser_desktop_connection_snapshot_failed:${currentResponse.status}`);
  }
  const currentPayload: unknown = await currentResponse.json();
  const expectedGeneration = browserConnectionGeneration(currentPayload);
  const response = await fetch(`${AGENT_ORIGIN}/api/browser/desktop/connection`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      socketPath: controlSocket,
      authToken: controlToken,
      desktopEpoch,
      parentPid: process.pid,
      expectedGeneration,
    }),
  });
  if (!response.ok) {
    throw new Error(`browser_desktop_connection_register_failed:${response.status}`);
  }
  const payload: unknown = await response.json();
  updateDaemonHostStatus(payload);
  desktopConnectionGeneration = browserConnectionGeneration(payload);
}

async function handleDaemonReady(): Promise<void> {
  try {
    await registerDesktopBrowserConnection();
    browserComponentError = null;
    await windowManager?.restoreAfterDaemonReady();
  } catch (cause) {
    browserComponentError = componentError("daemon", "browser_daemon_registration_failed", cause);
    throw cause;
  }
}

async function unregisterDesktopBrowserConnection(): Promise<void> {
  if (desktopConnectionGeneration === null) return;
  try {
    const response = await fetch(`${AGENT_ORIGIN}/api/browser/desktop/connection`, {
      method: "DELETE",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        desktopEpoch,
        parentPid: process.pid,
        generation: desktopConnectionGeneration,
      }),
    });
    if (!response.ok) {
      throw new Error(`browser_desktop_connection_unregister_failed:${response.status}`);
    }
    desktopConnectionGeneration = null;
    daemonHostStatus = "stopped";
    daemonHostProtocolCompatible = false;
    daemonHostErrorCode = null;
  } catch {
    // daemon 可能已经随桌面端退出；关闭流程不因注册清理失败阻塞。
  }
}

function browserConnectionGeneration(value: unknown): number {
  if (!value || typeof value !== "object") {
    throw new Error("browser_desktop_connection_generation_missing");
  }
  const generation = (value as { desktopConnectionGeneration?: unknown })
    .desktopConnectionGeneration;
  if (typeof generation !== "number" || !Number.isSafeInteger(generation) || generation < 0) {
    throw new Error("browser_desktop_connection_generation_invalid");
  }
  return generation;
}

function resolveRuntimePaths(): {
  daemon: string;
  workerEntry: string;
  preload: string;
  webDist: string;
  webRoot: string;
} {
  const moduleDirectory = fileURLToPath(new URL(".", import.meta.url));
  if (app.isPackaged) {
    return {
      daemon: join(process.resourcesPath, "daemon", process.platform === "win32" ? "magi-daemon-app.exe" : "magi-daemon-app"),
      workerEntry: join(process.resourcesPath, "browser-automation-worker", "index.cjs"),
      preload: join(moduleDirectory, "..", "preload", "index.cjs"),
      webDist: join(process.resourcesPath, "web", "dist"),
      webRoot: join(process.resourcesPath, "web"),
    };
  }
  const root = resolve(moduleDirectory, "../../../..");
  return {
    daemon: join(root, "target", "debug", process.platform === "win32" ? "magi-daemon-app.exe" : "magi-daemon-app"),
    workerEntry: join(root, "browser-automation-worker", "dist", "index.cjs"),
    preload: join(root, "apps", "desktop", "dist", "preload", "index.cjs"),
    webDist: join(root, "web", "dist"),
    webRoot: join(root, "web"),
  };
}

function parseLayoutIntent(value: unknown): WindowLayoutIntent {
  const input = rejectUnknownFields(object(value), ["type", "width", "visible"], "layoutIntent");
  switch (input.type) {
    case "right_pane_width":
      return { type: "right_pane_width", width: finite(input.width, "width") };
    case "right_pane_reset_width":
      return { type: "right_pane_reset_width" };
    case "right_pane_visibility":
      return { type: "right_pane_visibility", visible: input.visible === true };
    default:
      throw new Error("desktop_layout_intent_invalid");
  }
}

function parseBrowserActivation(value: unknown): {
  tabId: string;
  browserSessionId: string;
  url: string;
  navigationRevision: number;
  viewport: BrowserLogicalViewport;
} {
  const input = rejectUnknownFields(
    object(value),
    ["tabId", "browserSessionId", "url", "navigationRevision", "viewport"],
    "browserActivation",
  );
  const tabId = text(input.tabId, "tabId");
  const browserSessionId = text(input.browserSessionId, "browserSessionId");
  const url = text(input.url, "url");
  const navigationRevision = finite(input.navigationRevision, "navigationRevision");
  const viewport = parseViewport(input.viewport);
  return { tabId, browserSessionId, url, navigationRevision, viewport };
}

function parseViewport(value: unknown): BrowserLogicalViewport {
  const viewportInput = object(value);
  if (viewportInput.mode === "auto") {
    rejectUnknownFields(viewportInput, ["mode"], "viewport");
    return { mode: "auto" };
  }
  if (viewportInput.mode !== "fixed") throw new Error("desktop_ipc_viewport_mode_invalid");
  rejectUnknownFields(
    viewportInput,
    ["mode", "width", "height", "deviceScaleFactorMillis", "device_scale_factor_millis", "deviceType", "device_type"],
    "viewport",
  );
  return {
        mode: "fixed",
        width: finite(viewportInput.width, "viewport.width"),
        height: finite(viewportInput.height, "viewport.height"),
        device_scale_factor_millis: finite(
          viewportInput.deviceScaleFactorMillis ?? viewportInput.device_scale_factor_millis ?? 1_000,
          "viewport.deviceScaleFactorMillis",
        ),
        device_type: viewportInput.deviceType === "mobile" || viewportInput.device_type === "mobile"
          ? "mobile"
          : "desktop",
      };
}

function positiveInteger(value: unknown, name: string): number {
  const number = finite(value, name);
  if (!Number.isInteger(number) || number < 1) throw new Error(`${name}_invalid`);
  return number;
}

function nonNegativeInteger(value: unknown, name: string): number {
  const number = finite(value, name);
  if (!Number.isInteger(number) || number < 0) throw new Error(`${name}_invalid`);
  return number;
}

function parsePanelKind(value: unknown): PanelKind {
  return ["agent", "browser", "code", "terminal"].includes(String(value))
    ? value as PanelKind
    : null;
}

function parseOverlayState(value: unknown): import("./desktop-overlay-manager.js").DesktopOverlayState {
  const input = rejectUnknownFields(
    object(value),
    ["overlayId", "kind", "phase", "ownerId", "placement", "anchorBounds", "title", "items", "fields"],
    "overlay",
  );
  const placement = input.placement;
  if (placement !== "right-pane-add" && placement !== "browser-viewport" && placement !== "browser-annotations") {
    throw new Error("desktop_overlay_placement_invalid");
  }
  const kind = input.kind === "annotation" ? "annotation" : input.kind === "menu" ? "menu" : null;
  if (!kind) throw new Error("desktop_overlay_kind_invalid");
  const phase = input.phase === "select" || input.phase === "comment"
    ? input.phase
    : "menu";
  const rawItems = Array.isArray(input.items) ? input.items : [];
  const rawFields = Array.isArray(input.fields) ? input.fields : [];
  if (rawItems.length > 50 || rawFields.length > 4) throw new Error("desktop_overlay_items_invalid");
  const items = rawItems.map((value) => {
    const item = rejectUnknownFields(
      object(value),
      ["id", "label", "type", "icon", "selected", "disabled"],
      "overlay.item",
    );
    const id = text(item.id, "overlay.item.id");
    const label = text(item.label, "overlay.item.label");
    if (id.length > 120 || label.length > 240) throw new Error("desktop_overlay_item_invalid");
    return {
      id,
      label,
      type: item.type === "text" ? "text" : "number",
      icon: typeof item.icon === "string" && item.icon.trim() ? item.icon.trim() : null,
      selected: item.selected === true,
      disabled: item.disabled === true,
    };
  });
  const fields = rawFields.map((value) => {
    const field = rejectUnknownFields(
      object(value),
      ["id", "label", "type", "value", "min", "max"],
      "overlay.field",
    );
    const id = text(field.id, "overlay.field.id");
    const label = text(field.label, "overlay.field.label");
    const fieldValue = typeof field.value === "string" ? field.value : String(field.value ?? "");
    if (id.length > 120 || label.length > 120 || fieldValue.length > 32) throw new Error("desktop_overlay_field_invalid");
    return {
      id,
      label,
      type: field.type === "text" ? ("text" as const) : ("number" as const),
      value: fieldValue,
      min: finiteOrNull(field.min),
      max: finiteOrNull(field.max),
    };
  });
  let anchorBounds: Rectangle | null = null;
  if (input.anchorBounds !== null && input.anchorBounds !== undefined) {
    const bounds = rejectUnknownFields(
      object(input.anchorBounds),
      ["x", "y", "width", "height"],
      "overlay.anchorBounds",
    );
    anchorBounds = {
      x: finite(bounds.x, "overlay.anchorBounds.x"),
      y: finite(bounds.y, "overlay.anchorBounds.y"),
      width: finite(bounds.width, "overlay.anchorBounds.width"),
      height: finite(bounds.height, "overlay.anchorBounds.height"),
    };
    if (anchorBounds.width <= 0 || anchorBounds.height <= 0) {
      throw new Error("desktop_overlay_anchor_invalid");
    }
  }
  if (kind === "menu" && !anchorBounds) throw new Error("desktop_overlay_anchor_required");
  return {
    overlayId: typeof input.overlayId === "string" && input.overlayId.trim()
      ? input.overlayId.trim()
      : `overlay-${randomUUID()}`,
    kind,
    phase,
    ownerId: text(input.ownerId, "overlay.ownerId"),
    placement,
    anchorBounds,
    title: text(input.title, "overlay.title"),
    items,
    fields,
  };
}

function parseOverlayAction(value: unknown): import("./desktop-overlay-manager.js").DesktopOverlayAction {
  const input = rejectUnknownFields(
    object(value),
    ["overlayId", "kind", "ownerId", "interaction", "id", "value"],
    "overlayAction",
  );
  const interaction = input.interaction;
  if (interaction !== "select" && interaction !== "input") throw new Error("desktop_overlay_interaction_invalid");
  const kind = input.kind === "annotation" ? "annotation" : input.kind === "menu" ? "menu" : null;
  if (!kind) throw new Error("desktop_overlay_kind_invalid");
  return {
    overlayId: text(input.overlayId, "overlay.overlayId"),
    kind,
    ownerId: text(input.ownerId, "overlay.ownerId"),
    interaction,
    id: text(input.id, "overlay.action.id"),
    value: typeof input.value === "string" ? input.value : null,
  };
}

function finiteOrNull(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("desktop_ipc_payload_invalid");
  return value as Record<string, unknown>;
}

function isDesktopBackgroundColor(value: string): boolean {
  return /^(?:#[0-9a-f]{6}(?:[0-9a-f]{2})?|rgba?\(\s*(?:\d{1,3}\s*,\s*){2}\d{1,3}(?:\s*,\s*(?:0(?:\.\d+)?|1(?:\.0+)?|0?\.\d+))?\s*\))$/iu.test(value);
}

function rejectUnknownFields(
  input: Record<string, unknown>,
  allowed: readonly string[],
  scope: string,
): Record<string, unknown> {
  const allowedFields = new Set(allowed);
  const unknown = Object.keys(input).find((key) => !allowedFields.has(key));
  if (unknown) throw new Error(`desktop_ipc_${scope}_unknown_field:${unknown}`);
  return input;
}

function text(value: unknown, name: string): string {
  if (typeof value !== "string" || !value.trim()) throw new Error(`desktop_ipc_${name}_invalid`);
  return value.trim();
}

function optionalText(value: unknown, name: string): string {
  if (value === undefined || value === null || value === "") return "";
  if (typeof value !== "string") throw new Error(`desktop_ipc_${name}_invalid`);
  const normalized = value.trim();
  if (normalized.length > 16_384) throw new Error(`desktop_ipc_${name}_invalid`);
  return normalized;
}

function finite(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) throw new Error(`desktop_ipc_${name}_invalid`);
  return value;
}
