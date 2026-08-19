import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./browser-surface-manager.ts", import.meta.url), "utf8");
const windowManagerSource = readFileSync(new URL("./window-manager.ts", import.meta.url), "utf8");
const layoutSource = readFileSync(new URL("./window-layout.ts", import.meta.url), "utf8");
const overlayManagerSource = readFileSync(new URL("./desktop-overlay-manager.ts", import.meta.url), "utf8");
const desktopControlSource = readFileSync(new URL("./desktop-control-server.ts", import.meta.url), "utf8");
const indexSource = readFileSync(new URL("./index.ts", import.meta.url), "utf8");
const browserTabSource = readFileSync(
  new URL("../../../../web/src/components/tabs/BrowserTabContent.svelte", import.meta.url),
  "utf8",
);
const rightPaneSource = readFileSync(
  new URL("../../../../web/src/web/RightPane.svelte", import.meta.url),
  "utf8",
);
const workerSource = readFileSync(new URL("./automation-worker.ts", import.meta.url), "utf8");
const browserRuntimeSource = readFileSync(
  new URL("../../../../browser-automation-worker/src/runtime.ts", import.meta.url),
  "utf8",
);

function section(value: string, startMarker: string, endMarker: string): string {
  const start = value.indexOf(startMarker);
  assert.notEqual(start, -1, `missing section start: ${startMarker}`);
  const end = value.indexOf(endMarker, start + startMarker.length);
  assert.notEqual(end, -1, `missing section end: ${endMarker}`);
  return value.slice(start, end);
}

test("Browser Surface 只接受 Main 布局计算出的内容槽", () => {
  assert.match(source, /bindContentSurface\(windowId: string, tabId: string, bounds: Rectangle \| null\)/u);
  assert.match(source, /record\.host\.addChildView\(record\.view, 1\)/u);
  assert.match(source, /record\.view\.setBounds\(bounds\)/u);
  assert.match(source, /record\.host\.removeChildView\(record\.view\)/u);
});

test("右栏布局和原生 Surface 使用同一 Main 事务", () => {
  assert.match(layoutSource, /export function browserContentBounds\(/u);
  assert.match(layoutSource, /rightPaneTabBarHeight/u);
  assert.match(layoutSource, /browserToolbarHeight/u);
  assert.match(windowManagerSource, /private applyLayout\(record: DesktopWindowRecord\)/u);
  assert.match(windowManagerSource, /const currentBrowserContentBounds = showBrowserSurface \? browserContentBounds\(layout\) : null/u);
  assert.match(windowManagerSource, /this\.#surfaceManager\.bindContentSurface\(/u);
  assert.doesNotMatch(windowManagerSource, /updateBrowserSlot/u);
});

test("Renderer 不再发布原生内容槽坐标", () => {
  assert.match(browserTabSource, /class="browser-surface-slot"/u);
  assert.match(browserTabSource, /class="browser-native-surface"/u);
  assert.doesNotMatch(browserTabSource, /ResizeObserver|browserSurfaceSlot|updateBrowserSlot|bindContentSurface/u);
  assert.doesNotMatch(browserTabSource, /transform:\s*scale\(|object-fit:\s*(fill|cover)|surfaceWidth|surfaceHeight/u);
});

test("物化只创建一次 Surface，导航默认脱离激活关键路径", () => {
  const materialize = section(source, "async materialize(", "private createSurface(");
  assert.equal([...materialize.matchAll(/this\.createSurface\(input\)/gu)].length, 1);
  assert.match(materialize, /let record = this\.surfaceForTab\(input\.tabId, input\.windowId\)/u);
  assert.match(materialize, /if \(input\.awaitPageLoad === true\) await load/u);
  assert.match(materialize, /else void load\.catch\(\(\) => undefined\)/u);
  assert.match(windowManagerSource, /awaitPageLoad: false/u);
  assert.doesNotMatch(windowManagerSource, /async activateBrowser\([\s\S]*?registerDesktopBrowserConnection/u);
});

test("Surface 创建不等待调试器握手，工具调用才等待", () => {
  const createSurface = section(source, "private createSurface(", "bindContentSurface(");
  assert.doesNotMatch(createSurface, /await this\.attachDebugger\(/u);
  assert.match(createSurface, /const debuggerReady = this\.attachDebugger\(record\)/u);
  assert.match(createSurface, /debuggerReady\.then\([\s\S]*?debuggerReadyPromise === debuggerReady[\s\S]*?= null/u);
  assert.match(source, /private async waitForDebugger\(record: BrowserSurfaceRecord\)/u);
  assert.match(source, /const debuggerReady = record\.debuggerReadyPromise[\s\S]*?debuggerReadyPromise === debuggerReady[\s\S]*?= null/u);
  assert.match(source, /await this\.waitForDebugger\(record\)/u);
});

test("页面导航期间保持原生页面可见，只有明确失败才撤下", () => {
  assert.match(source, /const visible = bounds !== null && !record\.loadFailed/u);
  assert.match(source, /did-start-navigation[\s\S]*?loading_changed/u);
  assert.match(source, /did-fail-load[\s\S]*?record\.loadFailed = true[\s\S]*?this\.unmountSurface\(record/u);
  assert.match(source, /did-finish-load[\s\S]*?this\.applySlot\(\s*record/u);
  assert.match(source, /private unmountSurface\([\s\S]*?只解绑原生 View，不关闭 WebContents/u);
  assert.doesNotMatch(source, /did-start-navigation[\s\S]*?setBounds\(\{ x: 0, y: 0, width: 0/u);
});

test("Surface 布局和加载回调不隐式抢占 App Renderer 焦点", () => {
  const applySlot = section(source, "private applySlot(", "private async loadPage(");
  assert.doesNotMatch(applySlot, /contents\.focus\(\)/u);
  assert.match(windowManagerSource, /activatePanel\([\s\S]*?record\.appView\.webContents\.focus\(\)/u);
  assert.doesNotMatch(readFileSync(new URL("../../../../web/src/App.svelte", import.meta.url), "utf8"), /restoreAppFocus|document\.addEventListener\('pointerdown'/u);
});

test("debugger detach 只后台重连，不把页面当成崩溃刷新", () => {
  assert.match(source, /debugger-detached:[\s\S]*?reconnectDebugger/u);
  assert.match(source, /private reconnectDebugger\(/u);
  const debuggerDetach = section(source, 'debuggerApi.on("detach"', "private reconnectDebugger(");
  assert.doesNotMatch(debuggerDetach, /reloadAndWait|invalidateAndRecover|setVisible\(false\)/u);
});

test("切换 Browser Tab 保留各自 WebContents，非当前 Surface 不参与命中测试", () => {
  assert.match(source, /record\.tabId === tabId[\s\S]*?this\.applySlot\(record, bounds, window\)/u);
  assert.match(source, /else if \(bounds\) \{[\s\S]*?this\.unmountSurface\(record, window\)/u);
  assert.match(source, /private unmountSurface\([\s\S]*?record\.view\.setVisible\(false\)/u);
  assert.match(source, /private detachSurface\([\s\S]*?record\.host\.removeChildView\(record\.view\)/u);
  assert.match(windowManagerSource, /activateBrowser\([\s\S]*?bindContentSurface\(input\.windowId, "", null\)/u);
});

test("Surface 激活使用代次，过期请求不能抢占当前槽", () => {
  assert.match(source, /#activationGenerations/u);
  assert.match(source, /setActivationGeneration\(windowId: string, generation: number\)/u);
  assert.match(source, /assertActivationCurrent\(input\.windowId, input\.activationGeneration\)/u);
  assert.match(source, /browser_surface_activation_stale/u);
  assert.match(source, /created[\s\S]*?closeRecord\(record, false\)/u);
});

test("viewport 只作用于当前 Surface，不改变右栏或 DOM 尺寸", () => {
  assert.match(source, /Emulation\.clearDeviceMetricsOverride/u);
  assert.match(source, /Emulation\.setDeviceMetricsOverride/u);
  assert.match(source, /Emulation\.setTouchEmulationEnabled/u);
  assert.doesNotMatch(source, /capturePage\(|startScreencast|drawImage\(/u);
  assert.match(browserTabSource, /VIEWPORT_DEVICE_MODES = \[[\s\S]*?id: 'wide'[\s\S]*?id: 'narrow'/u);
  assert.match(browserTabSource, /scheduleCustomViewportUpdate\(\)/u);
});

test("截图使用 CDP Page.captureScreenshot，root 不依赖 DOM ref", () => {
  assert.match(source, /method === "Page\.captureScreenshot"/u);
  assert.match(source, /sendCdpCommandWithTimeout\(/u);
  assert.doesNotMatch(source, /capturePage\(|startScreencast|drawImage\(/u);
  assert.match(browserRuntimeSource, /element_ref !== "root"[\s\S]*?else if \(input\.clip\)/u);
  assert.match(browserRuntimeSource, /input\.clip\.width \* viewport\.width/u);
});

test("标记 Overlay 只覆盖当前浏览器内容槽并保持菜单层级", () => {
  assert.match(overlayManagerSource, /state\.kind === "annotation"[\s\S]*?browserContentBounds/u);
  assert.match(overlayManagerSource, /if \(!browserContentBounds\) throw new Error\("desktop_overlay_browser_content_unavailable"\)/u);
  assert.match(overlayManagerSource, /private setOverlayBounds\([\s\S]*?record\.layer\.setBounds\(bounds\)/u);
  assert.match(overlayManagerSource, /TRANSPARENT_VIEW_BACKGROUND/u);
  assert.match(browserTabSource, /openAnnotationCommentOverlay\(\)/u);
});

test("右栏非浏览器面板不依赖 Browser Surface", () => {
  assert.match(rightPaneSource, /activatePanel\(/u);
  assert.match(rightPaneSource, /kind === 'browser'|kind: 'browser'/u);
  assert.match(windowManagerSource, /activatePanel\(windowId: string, kind: PanelKind/u);
  assert.match(layoutSource, /activeSurfaceId: null/u);
  assert.match(windowManagerSource, /const showBrowserSurface = !record\.blockingOverlayActive/u);
});

test("浏览器动作沿现有统一工具和单 Tab 队列执行", () => {
  assert.match(source, /private primaryBindingForTab|primaryBindingForTab\(/u);
  assert.match(desktopControlSource, /const queueKey = commandTabId\(request\.command\)/u);
  assert.match(desktopControlSource, /this\.#queues\.set\(queueKey, tail\)/u);
  assert.match(indexSource, /registerDesktopBrowserConnection\(\)/u);
  assert.match(rightPaneSource, /getBrowserSession\(/u);
});

test("权限和安全边界不读取外部浏览器资料", () => {
  assert.match(source, /setPermissionCheckHandler\(\(\) => false\)/u);
  assert.match(source, /setPermissionRequestHandler\([\s\S]*?callback\(false\)/u);
  assert.match(source, /nodeIntegration: false/u);
  assert.match(source, /contextIsolation: true/u);
  assert.match(source, /sandbox: true/u);
  assert.doesNotMatch(source, /chrome-user-data|Default\/Cookies|app\.getPath\("userData"\).*Chrome/u);
});

test("原生 View 层级固定，浏览器与 App、Overlay 同级", () => {
  assert.match(windowManagerSource, /const appLayer = new View\(\);[\s\S]*?const overlayLayer = new View\(\)/u);
  assert.match(windowManagerSource, /window\.contentView\.addChildView\(appLayer\)[\s\S]*?window\.contentView\.addChildView\(overlayLayer\)/u);
  assert.match(source, /record\.host\.addChildView\(record\.view, 1\)/u);
  assert.match(windowManagerSource, /closeOverlay\([\s\S]*?record\.appView\.webContents\.focus\(\)/u);
});
