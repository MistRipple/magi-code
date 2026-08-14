import { randomBytes, randomUUID } from "node:crypto";
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
} from "electron";
import {
  DESKTOP_BROWSER_PROTOCOL_VERSION,
  DESKTOP_RIGHT_PANE_INTENT_VERSION,
  type DesktopRightPaneIntentEnvelope,
  type DesktopRightPaneTabIntent,
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
let shuttingDown = false;

const singleInstance = app.requestSingleInstanceLock();
if (!singleInstance) app.quit();

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
    windows,
    onEvent: (event) => {
      control?.handleSurfaceEvent(event);
      publishBrowserEvent(event);
    },
  });
  surfaceManager = surfaces;
  const overlays = new DesktopOverlayManager({
    preloadPath: paths.preload,
    agentOrigin: AGENT_ORIGIN,
    windows,
    onAction: (windowId, action) => {
      windowManager?.broadcast(windowId, "magi-desktop:overlay-action", action);
    },
    onClosed: (windowId) => {
      windowManager?.broadcast(windowId, "magi-desktop:overlay-closed", null);
    },
  });
  worker = new AutomationWorker({ entryPath: paths.workerEntry, surfaceManager: surfaces });
  worker.start();
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
  updateManager = new UpdateManager(app.getVersion(), (snapshot) => {
    broadcastAll("magi-desktop:update", snapshot);
  });
  registerIpc();
  manager.createWindow();
}).catch((error) => {
  console.error("Magi Desktop 启动失败", error);
  app.exit(1);
});

app.on("window-all-closed", () => app.quit());
app.on("before-quit", (event) => {
  if (shuttingDown) return;
  event.preventDefault();
  shuttingDown = true;
  void shutdown().finally(() => app.exit(0));
});

function registerIpc(): void {
  ipcMain.handle("magi-desktop:get-snapshot", (event) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    return manager.snapshot(windowId);
  });
  ipcMain.handle("magi-desktop:layout-intent", (event, value: unknown) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    return manager.submitLayoutIntent(windowId, parseLayoutIntent(value));
  });
  ipcMain.handle("magi-desktop:set-context", (event, value: unknown) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    if (manager.rendererRoleForWebContents(event.sender.id) !== "app") {
      throw new Error("desktop_context_sender_denied");
    }
    const request = object(value);
    return manager.setRendererContext(windowId, {
      workspaceId: optionalText(request.workspaceId, "workspaceId"),
      workspacePath: optionalText(request.workspacePath, "workspacePath"),
      sessionId: optionalText(request.sessionId, "sessionId"),
    });
  });
  ipcMain.handle("magi-desktop:activate-browser", async (event, value: unknown) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    const request = parseBrowserActivation(value);
    return manager.activateBrowser({ windowId, ...request });
  });
  ipcMain.handle("magi-desktop:activate-panel", (event, value: unknown) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    const request = object(value);
    return manager.activatePanel(
      windowId,
      parsePanelKind(request.kind),
      typeof request.tabId === "string" ? request.tabId : null,
    );
  });
  ipcMain.handle("magi-desktop:set-browser-viewport", async (event, value: unknown) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    const request = object(value);
    return manager.setBrowserViewport(
      windowId,
      text(request.tabId, "tabId"),
      parseViewport(request.viewport),
    );
  });
  ipcMain.handle("magi-desktop:open-right-pane-tab", (event, value: unknown) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    if (manager.rendererRoleForWebContents(event.sender.id) !== "app") {
      throw new Error("desktop_right_pane_intent_sender_denied");
    }
    return manager.openRightPaneTab(windowId, parseRightPaneIntent(value));
  });
  ipcMain.handle("magi-desktop:right-pane-ready", (event) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    if (manager.rendererRoleForWebContents(event.sender.id) !== "right-pane") {
      throw new Error("desktop_right_pane_ready_sender_denied");
    }
    manager.handleRightPaneReady(windowId);
  });
  ipcMain.handle("magi-desktop:open-overlay", (event, value: unknown) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    if (manager.rendererRoleForWebContents(event.sender.id) !== "right-pane") {
      throw new Error("desktop_overlay_sender_denied");
    }
    manager.openOverlay(windowId, parseOverlayState(value));
  });
  ipcMain.handle("magi-desktop:close-overlay", (event) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    manager.closeOverlay(windowId);
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
  ipcMain.handle("magi-desktop:focus-browser", (event, surfaceId: unknown) => {
    trustedSender(event.sender.id);
    if (typeof surfaceId !== "string" || !surfaceId.trim()) throw new Error("browser_surface_id_invalid");
    surfaceManager!.focus(surfaceId.trim());
  });
  ipcMain.handle("magi-desktop:open-external", async (event, value: unknown) => {
    trustedSender(event.sender.id);
    if (typeof value !== "string") throw new Error("external_url_invalid");
    const url = new URL(value);
    if (!["http:", "https:"].includes(url.protocol)) throw new Error("external_url_invalid");
    await shell.openExternal(url.href);
  });
  ipcMain.handle("magi-desktop:show-context-menu", async (event, value: unknown) => {
    const { windowId } = trustedSender(event.sender.id);
    return showContextMenu(windowId, parseContextMenuItems(value));
  });
  ipcMain.handle("magi-desktop:open-workspace-folder", async (event, value: unknown) => {
    trustedSender(event.sender.id);
    await openWorkspaceFolder(text(value, "workspaceRootPathRef"));
  });
  ipcMain.handle("magi-desktop:reveal-workspace-file", async (event, value: unknown) => {
    trustedSender(event.sender.id);
    const request = object(value);
    await revealWorkspaceFile({
      targetPathRef: text(request.targetPathRef, "targetPathRef"),
      workspaceRootPathRef: text(request.workspaceRootPathRef, "workspaceRootPathRef"),
    });
  });
  ipcMain.handle("magi-desktop:set-appearance", (event, value: unknown) => {
    const { manager, windowId } = trustedSender(event.sender.id);
    const request = object(value);
    const mode = request.mode;
    const backgroundColor = text(request.backgroundColor, "backgroundColor");
    if (mode !== "light" && mode !== "dark") throw new Error("desktop_appearance_mode_invalid");
    if (!/^#[0-9a-f]{6}$/iu.test(backgroundColor)) {
      throw new Error("desktop_appearance_background_invalid");
    }
    nativeTheme.themeSource = mode;
    manager.setAppearance(windowId, backgroundColor);
  });
  ipcMain.handle("magi-desktop:get-app-version", (event) => {
    trustedSender(event.sender.id);
    return app.getVersion();
  });
  ipcMain.handle("magi-desktop:get-browser-component-info", (event) => {
    trustedSender(event.sender.id);
    return browserComponentInfo();
  });
  ipcMain.handle("magi-desktop:restart-browser-automation", (event) => {
    trustedSender(event.sender.id);
    automationWorker!.restart();
    return browserComponentInfo();
  });
  ipcMain.handle("magi-desktop:clear-browser-data", async (event) => {
    trustedSender(event.sender.id);
    await surfaceManager!.clearBrowsingData();
  });
  ipcMain.handle("magi-desktop:check-for-updates", (event) => {
    trustedSender(event.sender.id);
    return updateManager!.check();
  });
  ipcMain.handle("magi-desktop:download-update", (event) => {
    trustedSender(event.sender.id);
    return updateManager!.download();
  });
  ipcMain.handle("magi-desktop:install-update", (event) => {
    trustedSender(event.sender.id);
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
  const request = object(value);
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
    if (item.type === "separator") return { type: "separator" };
    if (item.type === "role" && allowedRoles.has(item.role as ContextMenuRole)) {
      return { type: "role", role: item.role as ContextMenuRole };
    }
    if (item.type !== "action") throw new Error("desktop_context_menu_item_invalid");
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
    desktop_version: app.getVersion(),
    electron_version: process.versions.electron,
    chromium_version: process.versions.chrome,
    process_id: process.pid,
    desktop_epoch: desktopEpoch,
    worker_epoch: worker.workerEpoch,
  };
}

function browserComponentInfo() {
  const worker = automationWorker!;
  const daemon = processSupervisor!;
  return {
    ...handshake(worker),
    daemon_version: app.getVersion(),
    daemon_status: daemon.status,
    daemon_process_id: daemon.processId,
    automation_worker_version: app.getVersion(),
    automation_worker_status: worker.status,
    protocol_compatible: true,
  };
}

function trustedSender(webContentsId: number): { manager: WindowManager; windowId: string } {
  const manager = windowManager;
  if (!manager) throw new Error("desktop_not_ready");
  const windowId = manager.windowIdForWebContents(webContentsId);
  if (!windowId) throw new Error("desktop_ipc_sender_denied");
  return { manager, windowId };
}

function publishBrowserEvent(event: BrowserSurfaceEvent): void {
  broadcastAll("magi-desktop:browser-event", event);
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
  windowManager?.closeAll();
  surfaceManager?.closeAll();
  automationWorker?.stop();
  await processSupervisor?.stop();
  await controlServer?.close();
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
  const input = object(value);
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
  const input = object(value);
  const tabId = text(input.tabId, "tabId");
  const browserSessionId = text(input.browserSessionId, "browserSessionId");
  const url = text(input.url, "url");
  const navigationRevision = finite(input.navigationRevision, "navigationRevision");
  const viewport = parseViewport(input.viewport);
  return { tabId, browserSessionId, url, navigationRevision, viewport };
}

function parseRightPaneIntent(value: unknown): DesktopRightPaneIntentEnvelope {
  const input = object(value);
  if (input.version !== DESKTOP_RIGHT_PANE_INTENT_VERSION) {
    throw new Error("desktop_right_pane_intent_version_invalid");
  }
  const raw = object(input.intent);
  const kind = raw.kind;
  if (kind === "agent") {
    return {
      version: DESKTOP_RIGHT_PANE_INTENT_VERSION,
      intent: stripUndefined({
        kind: "agent",
        agentRunId: boundedText(raw.agentRunId, "rightPane.agentRunId", 512),
        sessionId: boundedOptionalText(raw.sessionId, "rightPane.sessionId", 512),
        workspaceId: boundedOptionalText(raw.workspaceId, "rightPane.workspaceId", 512),
        workspacePath: boundedOptionalText(raw.workspacePath, "rightPane.workspacePath", 16_384),
        label: boundedOptionalTextOrUndefined(raw.label, "rightPane.label", 512),
        accentToken: boundedNullableText(raw.accentToken, "rightPane.accentToken", 128),
      }) as unknown as DesktopRightPaneTabIntent,
    };
  }
  if (kind === "code") {
    const contentKind = raw.contentKind;
    if (contentKind !== undefined
      && contentKind !== "text"
      && contentKind !== "binary"
      && contentKind !== "large_text"
      && contentKind !== "symlink"
      && contentKind !== "special") {
      throw new Error("desktop_right_pane_content_kind_invalid");
    }
    return {
      version: DESKTOP_RIGHT_PANE_INTENT_VERSION,
      intent: stripUndefined({
        kind: "code",
        filepath: boundedText(raw.filepath, "rightPane.filepath", 16_384),
        sessionId: boundedOptionalText(raw.sessionId, "rightPane.sessionId", 512),
        workspaceId: boundedOptionalText(raw.workspaceId, "rightPane.workspaceId", 512),
        workspacePath: boundedOptionalText(raw.workspacePath, "rightPane.workspacePath", 16_384),
        label: boundedOptionalTextOrUndefined(raw.label, "rightPane.label", 512),
        displayPath: boundedOptionalTextOrUndefined(raw.displayPath, "rightPane.displayPath", 16_384),
        diff: boundedNullableText(raw.diff, "rightPane.diff", 2_000_000),
        originalContent: boundedNullableText(raw.originalContent, "rightPane.originalContent", 2_000_000),
        currentContent: boundedNullableText(raw.currentContent, "rightPane.currentContent", 2_000_000),
        isChangeDiff: raw.isChangeDiff === true ? true : raw.isChangeDiff === false ? false : undefined,
        changeRevision: boundedNullableText(raw.changeRevision, "rightPane.changeRevision", 512),
        content: boundedNullableText(raw.content, "rightPane.content", 2_000_000),
        language: boundedNullableText(raw.language, "rightPane.language", 128),
        contentKind,
        size: nullableFinite(raw.size, "rightPane.size"),
        mime: boundedNullableText(raw.mime, "rightPane.mime", 256),
        symlinkTarget: boundedNullableText(raw.symlinkTarget, "rightPane.symlinkTarget", 16_384),
        headSummary: boundedNullableText(raw.headSummary, "rightPane.headSummary", 512_000),
        tailSummary: boundedNullableText(raw.tailSummary, "rightPane.tailSummary", 512_000),
        imageDataUrl: boundedNullableText(raw.imageDataUrl, "rightPane.imageDataUrl", 8_000_000),
      }) as unknown as DesktopRightPaneTabIntent,
    };
  }
  if (kind === "terminal") {
    return {
      version: DESKTOP_RIGHT_PANE_INTENT_VERSION,
      intent: {
        kind,
        terminalTabId: boundedText(raw.terminalTabId, "rightPane.terminalTabId", 512),
        sessionId: boundedOptionalText(raw.sessionId, "rightPane.sessionId", 512),
        workspaceId: boundedOptionalText(raw.workspaceId, "rightPane.workspaceId", 512),
        workspacePath: boundedOptionalText(raw.workspacePath, "rightPane.workspacePath", 16_384),
      },
    };
  }
  throw new Error("desktop_right_pane_intent_kind_invalid");
}

function parseViewport(value: unknown): BrowserLogicalViewport {
  const viewportInput = object(value);
  return viewportInput.mode === "fixed"
    ? {
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
      }
    : { mode: "auto" };
}

function parsePanelKind(value: unknown): PanelKind {
  return ["agent", "browser", "code", "terminal"].includes(String(value))
    ? value as PanelKind
    : null;
}

function parseOverlayState(value: unknown): import("./desktop-overlay-manager.js").DesktopOverlayState {
  const input = object(value);
  const placement = input.placement;
  if (placement !== "right-pane-add" && placement !== "browser-viewport" && placement !== "browser-annotations" && placement !== "browser-content") {
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
    const item = object(value);
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
    const field = object(value);
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
  return {
    overlayId: typeof input.overlayId === "string" && input.overlayId.trim()
      ? input.overlayId.trim()
      : `overlay-${randomUUID()}`,
    kind,
    phase,
    ownerId: text(input.ownerId, "overlay.ownerId"),
    placement,
    title: text(input.title, "overlay.title"),
    items,
    fields,
  };
}

function parseOverlayAction(value: unknown): import("./desktop-overlay-manager.js").DesktopOverlayAction {
  const input = object(value);
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

function boundedText(value: unknown, name: string, maxLength: number): string {
  const normalized = text(value, name);
  if (normalized.length > maxLength) throw new Error(`desktop_ipc_${name}_invalid`);
  return normalized;
}

function boundedOptionalText(value: unknown, name: string, maxLength: number): string {
  const normalized = optionalText(value, name);
  if (normalized.length > maxLength) throw new Error(`desktop_ipc_${name}_invalid`);
  return normalized;
}

function boundedOptionalTextOrUndefined(value: unknown, name: string, maxLength: number): string | undefined {
  if (value === undefined || value === null || value === "") return undefined;
  return boundedText(value, name, maxLength);
}

function boundedNullableText(value: unknown, name: string, maxLength: number): string | null | undefined {
  if (value === undefined) return undefined;
  if (value === null || value === "") return null;
  return boundedText(value, name, maxLength);
}

function nullableFinite(value: unknown, name: string): number | null | undefined {
  if (value === undefined) return undefined;
  if (value === null) return null;
  const normalized = finite(value, name);
  if (normalized < 0) throw new Error(`desktop_ipc_${name}_invalid`);
  return normalized;
}

function stripUndefined(value: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(value).filter(([, entry]) => entry !== undefined),
  );
}

function finite(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) throw new Error(`desktop_ipc_${name}_invalid`);
  return value;
}
