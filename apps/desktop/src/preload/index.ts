import { contextBridge, ipcRenderer, webUtils } from "electron";

const SNAPSHOT_EVENT = "magi-desktop:snapshot";
const CONTEXT_EVENT = "magi-desktop:context";
const BROWSER_EVENT = "magi-desktop:browser-event";
const BROWSER_COMPONENT_EVENT = "magi-desktop:browser-component";
const UPDATE_EVENT = "magi-desktop:update";
const OVERLAY_STATE_EVENT = "magi-desktop:overlay-state";
const OVERLAY_CLOSED_EVENT = "magi-desktop:overlay-closed";
const OVERLAY_ACTION_EVENT = "magi-desktop:overlay-action";

type DesktopFileDropEvent =
  | { type: "enter"; paths: string[]; position: { x: number; y: number } }
  | { type: "over"; position: { x: number; y: number } }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" };

const fileDropListeners = new Set<(event: DesktopFileDropEvent) => void>();
let dragDepth = 0;

function readDesktopArgument(prefix: string): string | null {
  const value = process.argv.find((argument) => argument.startsWith(prefix));
  const result = value?.slice(prefix.length).trim() ?? "";
  return result || null;
}

const desktopSurface = readDesktopArgument("--magi-desktop-surface=");
const desktopWindowId = readDesktopArgument("--magi-desktop-window-id=");

function isFileDrag(event: DragEvent): boolean {
  return Array.from(event.dataTransfer?.types ?? []).includes("Files");
}

function droppedPaths(event: DragEvent): string[] {
  const files = Array.from(event.dataTransfer?.files ?? []);
  return [...new Set(files.map((file) => webUtils.getPathForFile(file)).filter(Boolean))];
}

function publishFileDrop(event: DesktopFileDropEvent): void {
  for (const listener of fileDropListeners) listener(event);
}

window.addEventListener("dragenter", (event) => {
  if (!isFileDrag(event)) return;
  event.preventDefault();
  dragDepth += 1;
  publishFileDrop({
    type: "enter",
    paths: droppedPaths(event),
    position: { x: event.clientX, y: event.clientY },
  });
}, true);

window.addEventListener("dragover", (event) => {
  if (!isFileDrag(event)) return;
  event.preventDefault();
  if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
  publishFileDrop({ type: "over", position: { x: event.clientX, y: event.clientY } });
}, true);

window.addEventListener("dragleave", (event) => {
  if (!isFileDrag(event)) return;
  event.preventDefault();
  dragDepth = Math.max(0, dragDepth - 1);
  if (dragDepth === 0) publishFileDrop({ type: "leave" });
}, true);

window.addEventListener("drop", (event) => {
  if (!isFileDrag(event)) return;
  event.preventDefault();
  dragDepth = 0;
  publishFileDrop({
    type: "drop",
    paths: droppedPaths(event),
    position: { x: event.clientX, y: event.clientY },
  });
}, true);

contextBridge.exposeInMainWorld("magiDesktop", {
  runtime: "electron",
  surface: desktopSurface === "app" || desktopSurface === "overlay"
    ? desktopSurface
    : null,
  windowId: desktopWindowId,
  getSnapshot: () => ipcRenderer.invoke("magi-desktop:get-snapshot"),
  setContext: (context: unknown) => ipcRenderer.invoke("magi-desktop:set-context", context),
  submitLayoutIntent: (intent: unknown) => ipcRenderer.invoke("magi-desktop:layout-intent", intent),
  activateBrowser: (request: unknown) => ipcRenderer.invoke("magi-desktop:activate-browser", request),
  activatePanel: (request: unknown) => ipcRenderer.invoke("magi-desktop:activate-panel", request),
  setBrowserViewport: (request: unknown) => ipcRenderer.invoke("magi-desktop:set-browser-viewport", request),
  focusApp: () => ipcRenderer.invoke("magi-desktop:focus-app"),
  readyRightPane: () => ipcRenderer.invoke("magi-desktop:right-pane-ready"),
  openOverlay: (state: unknown) => ipcRenderer.invoke("magi-desktop:open-overlay", state),
  closeOverlay: () => ipcRenderer.invoke("magi-desktop:close-overlay"),
  setBlockingOverlay: (request: unknown) => ipcRenderer.invoke("magi-desktop:set-blocking-overlay", request),
  readyOverlay: () => ipcRenderer.invoke("magi-desktop:overlay-ready"),
  submitOverlayAction: (action: unknown) => ipcRenderer.invoke("magi-desktop:overlay-action", action),
  openExternal: (url: string) => ipcRenderer.invoke("magi-desktop:open-external", url),
  showContextMenu: (request: unknown) => ipcRenderer.invoke("magi-desktop:show-context-menu", request),
  openWorkspaceFolder: (workspaceRootPathRef: string) => ipcRenderer.invoke(
    "magi-desktop:open-workspace-folder",
    workspaceRootPathRef,
  ),
  revealWorkspaceFile: (request: unknown) => ipcRenderer.invoke(
    "magi-desktop:reveal-workspace-file",
    request,
  ),
  setAppearance: (appearance: unknown) => ipcRenderer.invoke("magi-desktop:set-appearance", appearance),
  getAppVersion: () => ipcRenderer.invoke("magi-desktop:get-app-version"),
  getBrowserComponentInfo: () => ipcRenderer.invoke("magi-desktop:get-browser-component-info"),
  restartBrowserAutomation: () => ipcRenderer.invoke("magi-desktop:restart-browser-automation"),
  clearBrowserData: () => ipcRenderer.invoke("magi-desktop:clear-browser-data"),
  checkForUpdates: () => ipcRenderer.invoke("magi-desktop:check-for-updates"),
  downloadUpdate: () => ipcRenderer.invoke("magi-desktop:download-update"),
  installUpdate: () => ipcRenderer.invoke("magi-desktop:install-update"),
  onSnapshot: (listener: (snapshot: unknown) => void) => subscribe(SNAPSHOT_EVENT, listener),
  onContext: (listener: (context: unknown) => void) => subscribe(CONTEXT_EVENT, listener),
  onBrowserEvent: (listener: (event: unknown) => void) => subscribe(BROWSER_EVENT, listener),
  onBrowserComponent: (listener: (snapshot: unknown) => void) => subscribe(BROWSER_COMPONENT_EVENT, listener),
  onOverlayState: (listener: (state: unknown) => void) => subscribe(OVERLAY_STATE_EVENT, listener),
  onOverlayClosed: (listener: () => void) => subscribe(OVERLAY_CLOSED_EVENT, listener),
  onOverlayAction: (listener: (action: unknown) => void) => subscribe(OVERLAY_ACTION_EVENT, listener),
  onUpdate: (listener: (snapshot: unknown) => void) => subscribe(UPDATE_EVENT, listener),
  onFileDrop: (listener: (event: DesktopFileDropEvent) => void) => {
    fileDropListeners.add(listener);
    return () => fileDropListeners.delete(listener);
  },
});

function subscribe(channel: string, listener: (value: unknown) => void): () => void {
  const handler = (_event: Electron.IpcRendererEvent, value: unknown) => listener(value);
  ipcRenderer.on(channel, handler);
  return () => ipcRenderer.off(channel, handler);
}
