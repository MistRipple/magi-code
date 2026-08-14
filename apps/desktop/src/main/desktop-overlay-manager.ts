import { randomUUID } from "node:crypto";
import type { BaseWindow, Rectangle, WebContentsView } from "electron";
import { WebContentsView as ElectronWebContentsView } from "electron";
import type { WindowLayoutSnapshot } from "./window-layout.js";

export type DesktopOverlayPlacement =
  | "right-pane-add"
  | "browser-viewport"
  | "browser-annotations"
  | "browser-content";

export interface DesktopOverlayItem {
  id: string;
  label: string;
  icon: string | null;
  selected: boolean;
  disabled: boolean;
}

export interface DesktopOverlayField {
  id: string;
  label: string;
  type: "number" | "text";
  value: string;
  min: number | null;
  max: number | null;
}

export interface DesktopOverlayState {
  overlayId: string;
  kind: "menu" | "annotation";
  phase: "menu" | "select" | "comment";
  ownerId: string;
  placement: DesktopOverlayPlacement;
  title: string;
  items: DesktopOverlayItem[];
  fields: DesktopOverlayField[];
}

export interface DesktopOverlayAction {
  overlayId: string;
  kind: "menu" | "annotation";
  ownerId: string;
  interaction: "select" | "input";
  id: string;
  value: string | null;
}

interface OverlayRecord {
  windowId: string;
  window: BaseWindow;
  view: WebContentsView;
  state: DesktopOverlayState | null;
  visible: boolean;
  loaded: boolean;
  ready: boolean;
}

const OVERLAY_WIDTH: Record<DesktopOverlayPlacement, number> = {
  "right-pane-add": 184,
  "browser-viewport": 264,
  "browser-annotations": 320,
  "browser-content": 1,
};

export class DesktopOverlayManager {
  readonly #preloadPath: string;
  readonly #agentOrigin: string;
  readonly #windows: Map<string, BaseWindow>;
  readonly #records = new Map<string, OverlayRecord>();
  readonly #onAction: (windowId: string, action: DesktopOverlayAction) => void;
  readonly #onClosed: (windowId: string) => void;

  constructor(input: {
    preloadPath: string;
    agentOrigin: string;
    windows: Map<string, BaseWindow>;
    onAction: (windowId: string, action: DesktopOverlayAction) => void;
    onClosed: (windowId: string) => void;
  }) {
    this.#preloadPath = input.preloadPath;
    this.#agentOrigin = input.agentOrigin;
    this.#windows = input.windows;
    this.#onAction = input.onAction;
    this.#onClosed = input.onClosed;
  }

  create(windowId: string, window: BaseWindow): void {
    if (this.#records.has(windowId)) return;
    const view = new ElectronWebContentsView({
      webPreferences: {
        preload: this.#preloadPath,
        additionalArguments: [
          "--magi-desktop-surface=overlay",
          `--magi-desktop-window-id=${windowId}`,
        ],
        partition: "persist:magi-app",
        nodeIntegration: false,
        contextIsolation: true,
        sandbox: true,
        webSecurity: true,
      },
    });
    const record: OverlayRecord = {
      windowId,
      window,
      view,
      state: null,
      visible: false,
      loaded: false,
      ready: false,
    };
    this.#records.set(windowId, record);
    window.contentView.addChildView(view);
    view.setVisible(false);
    view.webContents.on("did-finish-load", () => {
      record.loaded = true;
      if (record.state && record.visible) this.publishState(record);
    });
    view.webContents.on("render-process-gone", () => {
      record.ready = false;
      if (record.visible && !window.isDestroyed()) {
        try {
          view.webContents.reload();
        } catch {
          // 进程退出竞态由下一次打开 Overlay 时重新加载处理。
        }
      }
    });
    void view.webContents.loadURL(this.rendererUrl(windowId)).catch(() => undefined);
  }

  open(windowId: string, state: DesktopOverlayState, layout: WindowLayoutSnapshot): void {
    const record = this.requireRecord(windowId);
    const bounds = overlayBounds(layout, state);
    record.state = state;
    record.visible = true;
    record.window.contentView.addChildView(record.view);
    record.view.setBounds(bounds);
    record.view.setVisible(true);
    if (record.loaded && record.ready) this.publishState(record);
  }

  handleReady(windowId: string): void {
    const record = this.#records.get(windowId);
    if (!record || !record.loaded || record.view.webContents.isDestroyed()) return;
    record.ready = true;
    if (record.visible && record.state) this.publishState(record);
  }

  close(windowId: string): void {
    const record = this.#records.get(windowId);
    if (!record) return;
    record.visible = false;
    record.state = null;
    record.view.setVisible(false);
    this.#onClosed(windowId);
  }

  updateLayout(windowId: string, layout: WindowLayoutSnapshot): void {
    const record = this.#records.get(windowId);
    if (!record || !record.visible || !record.state) return;
    if (!layout.rightPaneBounds) {
      this.close(windowId);
      return;
    }
    record.view.setBounds(overlayBounds(layout, record.state));
  }

  handleAction(windowId: string, action: DesktopOverlayAction): void {
    const record = this.#records.get(windowId);
    const state = record?.state;
    if (!record || !record.visible || !state) return;
    const knownAction = (
      state.items.some((item) => item.id === action.id && !item.disabled)
      || state.fields.some((field) => field.id === action.id)
      || (state.kind === "annotation" && ["selection", "save", "cancel"].includes(action.id))
    );
    if (
      action.overlayId !== state.overlayId
      || action.kind !== state.kind
      || action.ownerId !== state.ownerId
      || !knownAction
    ) {
      return;
    }
    this.#onAction(windowId, action);
    if (action.interaction === "select" && !(state.kind === "annotation" && action.id === "selection")) {
      this.close(windowId);
    }
  }

  isWebContents(webContentsId: number): boolean {
    return [...this.#records.values()].some((record) => record.view.webContents.id === webContentsId);
  }

  windowIdForWebContents(webContentsId: number): string | null {
    for (const record of this.#records.values()) {
      if (record.view.webContents.id === webContentsId) return record.windowId;
    }
    return null;
  }

  closeWindow(windowId: string): void {
    const record = this.#records.get(windowId);
    if (!record) return;
    record.visible = false;
    if (!record.view.webContents.isDestroyed()) record.view.webContents.close();
    if (!record.window.isDestroyed()) record.window.contentView.removeChildView(record.view);
    this.#records.delete(windowId);
  }

  closeAll(): void {
    for (const windowId of [...this.#records.keys()]) this.closeWindow(windowId);
  }

  private publishState(record: OverlayRecord): void {
    if (!record.state || record.view.webContents.isDestroyed()) return;
    record.view.webContents.send("magi-desktop:overlay-state", record.state);
  }

  private rendererUrl(windowId: string): string {
    const url = new URL("/web.html", this.#agentOrigin);
    url.searchParams.set("desktopSurface", "overlay");
    url.searchParams.set("desktopWindowId", windowId);
    return url.href;
  }

  private requireRecord(windowId: string): OverlayRecord {
    const record = this.#records.get(windowId);
    if (!record || record.window.isDestroyed()) throw new Error("desktop_overlay_not_found");
    return record;
  }
}

function overlayBounds(layout: WindowLayoutSnapshot, state: DesktopOverlayState): Rectangle {
  if (state.placement === "browser-content") {
    if (!layout.browserSurfaceBounds) throw new Error("desktop_overlay_browser_surface_unavailable");
    return { ...layout.browserSurfaceBounds };
  }
  const pane = layout.rightPaneBounds;
  if (!pane) throw new Error("desktop_overlay_right_pane_unavailable");
  const width = Math.min(OVERLAY_WIDTH[state.placement], Math.max(160, pane.width - 12));
  const itemHeight = 32;
  const fieldHeight = 58;
  const contentHeight = 12
    + state.items.length * itemHeight
    + state.fields.length * fieldHeight
    + (state.items.length > 0 && state.fields.length > 0 ? 8 : 0);
  const height = Math.min(420, Math.max(42, contentHeight));
  const x = pane.x + pane.width - width - 4;
  const y = state.placement === "right-pane-add"
    ? pane.y + 38
    : pane.y + 1 + 38 + 36;
  return { x, y, width, height };
}
