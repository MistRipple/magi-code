import assert from "node:assert/strict";
import test from "node:test";
import type { BaseWindow, Rectangle, View, WebContentsView } from "electron";
import {
  DesktopOverlayManager,
  type DesktopOverlayClosedEvent,
  type DesktopOverlayState,
} from "./desktop-overlay-manager.js";
import {
  browserContentBounds,
  createWindowLayoutState,
  reduceWindowLayout,
  snapshotWindowLayout,
  type WindowLayoutSnapshot,
} from "./window-layout.js";

const ZERO_BOUNDS: Rectangle = { x: 0, y: 0, width: 0, height: 0 };

type Listener = (...args: unknown[]) => void;

class FakeWebContents {
  static #nextId = 701;
  readonly id = FakeWebContents.#nextId++;
  readonly loadedUrls: string[] = [];
  readonly sent: Array<{ channel: string; payload: unknown }> = [];
  closed = false;
  closeRequested = false;
  destroyOnClose = true;
  #listeners = new Map<string, Listener[]>();

  setWindowOpenHandler(_handler: unknown): void {}

  on(channel: string, listener: Listener): this {
    const listeners = this.#listeners.get(channel) ?? [];
    listeners.push(listener);
    this.#listeners.set(channel, listeners);
    return this;
  }

  once(channel: string, listener: Listener): this {
    const onceListener: Listener = (...args) => {
      const listeners = this.#listeners.get(channel) ?? [];
      this.#listeners.set(channel, listeners.filter((candidate) => candidate !== onceListener));
      listener(...args);
    };
    return this.on(channel, onceListener);
  }

  emit(channel: string, ...args: unknown[]): void {
    for (const listener of this.#listeners.get(channel) ?? []) listener(...args);
  }

  async loadURL(url: string): Promise<void> {
    this.loadedUrls.push(url);
  }

  isDestroyed(): boolean {
    return this.closed;
  }

  send(channel: string, payload: unknown): void {
    this.sent.push({ channel, payload });
  }

  close(): void {
    this.closeRequested = true;
    if (this.destroyOnClose) this.destroy();
  }

  destroy(): void {
    if (this.closed) return;
    this.closed = true;
    this.emit("destroyed");
  }
}

class FakeView {
  bounds: Rectangle = { ...ZERO_BOUNDS };
  visible = false;
  backgroundColor: string | null = null;

  setBounds(bounds: Rectangle): void {
    this.bounds = { ...bounds };
  }

  getBounds(): Rectangle {
    return { ...this.bounds };
  }

  setVisible(visible: boolean): void {
    this.visible = visible;
  }

  getVisible(): boolean {
    return this.visible;
  }

  setBackgroundColor(color: string): void {
    this.backgroundColor = color;
  }
}

class FakeLayer extends FakeView {
  readonly children: FakeView[] = [];
  addCount = 0;
  removeCount = 0;

  addChildView(view: FakeView, index?: number): void {
    if (this.children.includes(view)) return;
    const targetIndex = index === undefined
      ? this.children.length
      : Math.max(0, Math.min(index, this.children.length));
    this.children.splice(targetIndex, 0, view);
    this.addCount += 1;
  }

  removeChildView(view: FakeView): void {
    const index = this.children.indexOf(view);
    if (index < 0) throw new Error("view_not_mounted");
    this.children.splice(index, 1);
    this.removeCount += 1;
  }
}

class FakeOverlayView extends FakeView {
  readonly webContents = new FakeWebContents();
}

class FakeWindow {
  destroyed = false;

  isDestroyed(): boolean {
    return this.destroyed;
  }
}

interface Harness {
  manager: DesktopOverlayManager;
  window: FakeWindow;
  layer: FakeLayer;
  view: FakeOverlayView;
  views: FakeOverlayView[];
  closed: DesktopOverlayClosedEvent[];
}

function createHarness(): Harness {
  const window = new FakeWindow();
  const layer = new FakeLayer();
  const views: FakeOverlayView[] = [];
  const closed: DesktopOverlayClosedEvent[] = [];
  const manager = new DesktopOverlayManager({
    preloadPath: "/tmp/magi-overlay-preload.js",
    agentOrigin: "http://127.0.0.1:38123",
    desktopEpoch: "desktop-test",
    createView: () => {
      const view = new FakeOverlayView();
      views.push(view);
      return view as unknown as WebContentsView;
    },
    onAction: () => undefined,
    onClosed: (_windowId, event) => closed.push(event),
  });
  manager.create("window-1", window as unknown as BaseWindow, layer as unknown as View);
  const view = views[0];
  assert.ok(view);
  return { manager, window, layer, view, views, closed };
}

function layoutWithGeometry(
  tabId = "tab-1",
  surfaceId = "surface-1",
  rightPaneWidth: number | null = null,
): { layout: WindowLayoutSnapshot; content: Rectangle } {
  let state = createWindowLayoutState({
    desktopEpoch: "desktop-test",
    windowId: "window-1",
    clientBounds: { x: 0, y: 0, width: 1440, height: 900 },
  });
  state = reduceWindowLayout(state, { type: "right_pane_visibility", visible: true });
  if (rightPaneWidth !== null) {
    state = reduceWindowLayout(state, { type: "right_pane_width", width: rightPaneWidth });
  }
  state = reduceWindowLayout(state, {
    type: "active_panel",
    kind: "browser",
    tabId,
    surfaceId,
  });
  const rightPaneX = snapshotWindowLayout(state).rightPaneBounds?.x
    ?? state.clientBounds.width - (rightPaneWidth ?? state.rightPaneWidth);
  const rightPane = snapshotWindowLayout(state).rightPaneBounds ?? {
    x: rightPaneX,
    y: 0,
    width: rightPaneWidth ?? 480,
    height: 900,
  };
  const revision = (state.rendererGeometry?.revision ?? -1) + 1;
  state = reduceWindowLayout(state, {
    type: "renderer_geometry",
    frame: {
      revision,
      layoutRevision: state.layoutRevision,
      coordinateSpace: "window-content-css-px",
      rightPaneBounds: rightPane,
      browserContentSlot: {
        tabId,
        bounds: {
          x: rightPane.x + 1,
          y: 74,
          width: rightPane.width - 2,
          height: rightPane.height - 74,
        },
      },
    },
  });
  const layout = snapshotWindowLayout(state);
  const content = browserContentBounds(layout);
  assert.ok(content);
  return { layout, content };
}

function annotationState(
  overlayId: string,
  tabId = "tab-1",
): DesktopOverlayState {
  return {
    overlayId,
    kind: "annotation",
    phase: "comment",
    ownerId: `browser:${tabId}`,
    placement: "browser-annotations",
    popupBounds: null,
    title: "Annotation",
    items: [],
    fields: [{
      id: "comment",
      label: "Comment",
      type: "text",
      value: "",
      min: null,
      max: 4000,
    }],
  };
}

function menuState(
  overlayId: string,
  popupBounds: Rectangle | null = { x: 1134, y: 73, width: 300, height: 162 },
): DesktopOverlayState {
  return {
    overlayId,
    kind: "menu",
    phase: "menu",
    ownerId: "browser:tab-1",
    placement: "browser-viewport",
    popupBounds,
    title: "Viewport",
    items: [],
    fields: [],
  };
}

async function settleLoad(): Promise<void> {
  await new Promise<void>((resolve) => setImmediate(resolve));
}

async function openReadyOverlay(
  harness: Harness,
  overlayId = "overlay-1",
  layoutData = layoutWithGeometry(),
): Promise<{ layout: WindowLayoutSnapshot; content: Rectangle }> {
  await settleLoad();
  harness.manager.open(
    "window-1",
    annotationState(overlayId),
    layoutData.layout,
  );
  harness.manager.handleReady("window-1");
  return layoutData;
}

test("创建 Overlay 时一次性挂载自身并保持透明、零 bounds 和隐藏状态", async () => {
  const harness = createHarness();
  await settleLoad();

  assert.deepEqual(harness.view.bounds, ZERO_BOUNDS);
  assert.equal(harness.view.visible, false);
  assert.equal(harness.view.backgroundColor, "rgba(0, 0, 0, 0)");
  assert.equal(harness.layer.children.length, 1);
  assert.equal(harness.layer.addCount, 1);
  assert.deepEqual(harness.view.webContents.loadedUrls, [
    "http://127.0.0.1:38123/web.html?desktopSurface=overlay&desktopWindowId=window-1&desktopEpoch=desktop-test",
  ]);
});

test("缺少当前几何时拒绝打开且不替换现有 Overlay", async () => {
  const harness = createHarness();
  await settleLoad();
  const data = layoutWithGeometry();
  const invalidLayout: WindowLayoutSnapshot = {
    ...data.layout,
    rendererGeometry: null,
    rightPaneBounds: null,
  };
  assert.throws(
    () => harness.manager.open("window-1", annotationState("overlay-1"), invalidLayout),
    /desktop_overlay_browser_content_unavailable/u,
  );
  assert.equal(harness.layer.children.length, 1);
  assert.equal(harness.closed.length, 0);

  await openReadyOverlay(harness);
  const visibleBounds = { ...harness.view.bounds };
  assert.throws(
    () => harness.manager.open("window-1", annotationState("overlay-2", "tab-2"), data.layout),
    /desktop_overlay_browser_content_unavailable/u,
  );
  assert.deepEqual(harness.view.bounds, visibleBounds);
  assert.equal(harness.view.visible, true);
  assert.equal(harness.closed.length, 0);
});

test("菜单只绑定 Renderer 提交的最终弹窗矩形，不从内容槽推导位置", async () => {
  const harness = createHarness();
  await settleLoad();
  const data = layoutWithGeometry();
  const state = menuState("menu-1");
  harness.manager.open("window-1", state, data.layout);
  harness.manager.handleReady("window-1");

  assert.deepEqual(harness.view.bounds, state.popupBounds);
  assert.notDeepEqual(harness.view.bounds, data.content);

  const invalid = menuState("menu-2", null);
  assert.throws(
    () => harness.manager.open("window-1", invalid, data.layout),
    /desktop_overlay_browser_content_unavailable/u,
  );
  assert.deepEqual(harness.view.bounds, state.popupBounds);
});

test("几何提交短暂缺失时保留命中租约，有效新租约到达后复用同一个 View", async () => {
  const harness = createHarness();
  const first = await openReadyOverlay(harness);
  const resized = layoutWithGeometry("tab-1", "surface-1", 560);
  const invalidLayout: WindowLayoutSnapshot = {
    ...first.layout,
    rendererGeometry: {
      ...first.layout.rendererGeometry!,
      rightPaneBounds: null,
      browserContentSlot: null,
    },
    rightPaneBounds: null,
  };

  harness.manager.updateLayout("window-1", invalidLayout);
  // 非法几何帧必须让原生浮层退出命中树，但不能销毁或重建其
  // WebContents。下一份完整 Renderer frame 到达后继续复用同一 View。
  assert.equal(harness.view.visible, false);
  assert.deepEqual(harness.view.bounds, first.content);
  assert.equal(harness.layer.children.length, 1);
  assert.equal(harness.closed.length, 0);

  harness.manager.updateLayout("window-1", resized.layout);
  assert.equal(harness.view.visible, true);
  // 原生 Overlay 的几何完全等于 Renderer 已确认的浏览器内容槽，
  // Main 不再推导任何浮层尺寸。
  assert.deepEqual(harness.view.bounds, resized.content);
  assert.equal(harness.layer.children.length, 1);
  assert.equal(harness.layer.addCount, 1);
  assert.deepEqual(first.content, { x: 961, y: 74, width: 478, height: 826 });
});

test("close 只接受当前 owner，过期请求不能关闭或清空当前 View", async () => {
  const harness = createHarness();
  const data = await openReadyOverlay(harness);
  const stale = harness.manager.close("window-1", {
    overlayId: "stale-overlay",
    ownerId: "browser:tab-1",
  });
  assert.equal(stale, null);
  assert.equal(harness.view.visible, true);
  assert.equal(harness.closed.length, 0);
  assert.equal(harness.manager.close("window-1", {
    overlayId: "overlay-1",
    ownerId: "browser:tab-2",
  }), null);
  assert.equal(harness.closed.length, 0);

  const closed = harness.manager.close("window-1", {
    overlayId: "overlay-1",
    ownerId: "browser:tab-1",
  });
  assert.equal(closed?.overlayId, "overlay-1");
  assert.equal(closed?.reason, "closed");
  assert.equal(harness.view.visible, false);
  assert.deepEqual(harness.view.bounds, data.content);
  assert.equal(harness.layer.children.length, 1);
  assert.equal(harness.layer.removeCount, 0);
  assert.equal(harness.manager.close("window-1"), null);
});

test("关闭 Overlay 只隐藏并复用同一个 Renderer，避免重建合成树", async () => {
  const harness = createHarness();
  const data = await openReadyOverlay(harness);
  const oldView = harness.views[0];
  assert.ok(oldView);

  const closed = harness.manager.close("window-1", {
    overlayId: "overlay-1",
    ownerId: "browser:tab-1",
  });

  assert.equal(closed?.reason, "closed");
  assert.equal(oldView.webContents.closed, false);
  assert.equal(harness.views.length, 1);
  assert.equal(harness.layer.children.length, 1);
  assert.deepEqual(oldView.bounds, data.content);
  assert.equal(harness.layer.removeCount, 0);

  harness.manager.open("window-1", annotationState("overlay-2"), data.layout);
  harness.manager.handleReady("window-1");
  assert.equal(harness.views.length, 1);
  assert.equal(harness.manager.isWebContents(oldView.webContents.id), true);
  assert.equal(oldView.visible, true);
  assert.equal(harness.layer.children.length, 1);
});

test("窗口销毁才关闭并移除 Overlay Renderer", async () => {
  const harness = createHarness();
  await openReadyOverlay(harness);
  const oldView = harness.views[0];
  assert.ok(oldView);
  oldView.webContents.destroyOnClose = false;

  harness.manager.closeWindow("window-1");
  assert.equal(oldView.webContents.closeRequested, true);
  assert.equal(oldView.webContents.closed, false);
  assert.equal(harness.views.length, 1);
  assert.equal(harness.layer.children.length, 0);
  assert.equal(harness.layer.removeCount, 1);

  oldView.webContents.destroy();
  assert.equal(harness.manager.isWebContents(oldView.webContents.id), false);
});

test("同一 View 替换 Overlay，旧 owner 收到 replacement，子 View 不重复挂载", async () => {
  const harness = createHarness();
  const data = await openReadyOverlay(harness);
  const originalView = harness.view;
  harness.manager.open(
    "window-1",
    annotationState("overlay-2"),
    data.layout,
  );

  assert.equal(harness.layer.children.length, 1);
  assert.equal(harness.layer.addCount, 1);
  assert.equal(harness.layer.removeCount, 0);
  assert.equal(harness.view, originalView);
  assert.deepEqual(harness.closed, [{
    kind: "annotation",
    overlayId: "overlay-1",
    ownerId: "browser:tab-1",
    reason: "replaced",
    replacement: { overlayId: "overlay-2", ownerId: "browser:tab-1" },
  }]);
  assert.equal(harness.view.visible, true);
});

test("closeWindow 收敛零 bounds、移除子 View、关闭 WebContents，并保持幂等", async () => {
  const harness = createHarness();
  await openReadyOverlay(harness);
  harness.manager.closeWindow("window-1");
  harness.manager.closeWindow("window-1");

  assert.equal(harness.view.webContents.closed, true);
  assert.deepEqual(harness.view.bounds, ZERO_BOUNDS);
  assert.equal(harness.view.visible, false);
  assert.equal(harness.layer.children.length, 0);
  assert.equal(harness.layer.removeCount, 1);
  assert.equal(harness.closed.length, 0);
});
