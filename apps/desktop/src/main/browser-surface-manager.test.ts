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
const settingsBrowserSource = readFileSync(
  new URL("../../../../web/src/components/SettingsBrowserTab.svelte", import.meta.url),
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
  assert.match(source, /record\.host\.addChildView\(record\.view, 0\)/u);
  assert.match(source, /const localBounds = \{ x: 0, y: 0, width: bounds\.width, height: bounds\.height \}/u);
  assert.match(source, /record\.view\.setBounds\(localBounds\)/u);
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
  assert.match(createSurface, /const debuggerReady = this\.enqueueCdp\(record, \(\) => this\.attachDebugger\(record\)\)/u);
  assert.match(createSurface, /debuggerReady\.then\([\s\S]*?debuggerReadyPromise === debuggerReady[\s\S]*?= null/u);
  assert.match(source, /private async waitForDebugger\(record: BrowserSurfaceRecord\)/u);
  assert.match(source, /const debuggerReady = record\.debuggerReadyPromise[\s\S]*?debuggerReadyPromise === debuggerReady[\s\S]*?= null/u);
  assert.match(source, /await this\.waitForDebugger\(record\)/u);
});

test("页面导航期间保持原生页面可见，只有渲染进程崩溃才恢复 Surface", () => {
  const surfaceSlot = section(source, "private applySlot(", "private async loadPage(");
  const navigationEvents = section(source, "webContents.on(\"did-start-navigation\"", "webContents.on(\"before-input-event\"");
  const crashEvents = section(source, "webContents.on(\"render-process-gone\"", "private async waitForDebugger(");
  assert.match(surfaceSlot, /record\.view\.setVisible\(true\)/u);
  assert.match(navigationEvents, /did-start-navigation[\s\S]*?loading_changed/u);
  assert.match(navigationEvents, /did-fail-load[\s\S]*?this\.applySlot\(\s*record/u);
  assert.doesNotMatch(navigationEvents, /did-fail-load[\s\S]*?this\.unmountSurface\(record/u);
  assert.match(navigationEvents, /did-finish-load[\s\S]*?this\.applySlot\(\s*record/u);
  assert.match(source, /private unmountSurface\([\s\S]*?只解绑原生 View，不关闭 WebContents/u);
  assert.match(crashEvents, /render-process-gone[\s\S]*?this\.unmountSurface\(record[\s\S]*?invalidateAndRecover/u);
  assert.doesNotMatch(navigationEvents, /did-start-navigation[\s\S]*?setBounds\(\{ x: 0, y: 0, width: 0/u);
});

test("Surface 布局和加载回调不隐式抢占 App Renderer 焦点", () => {
  const applySlot = section(source, "private applySlot(", "private async loadPage(");
  assert.doesNotMatch(applySlot, /contents\.focus\(\)/u);
  assert.match(windowManagerSource, /activatePanel\([\s\S]*?record\.appView\.webContents\.focus\(\)/u);
  assert.match(readFileSync(new URL("../../../../web/src/App.svelte", import.meta.url), "utf8"), /document\.addEventListener\('pointerdown', focusAppRenderer, true\)/u);
  assert.match(readFileSync(new URL("../../../../web/src/App.svelte", import.meta.url), "utf8"), /document\.addEventListener\('focusin', focusAppRenderer, true\)/u);
  assert.match(windowManagerSource, /focusApp\(windowId: string\)[\s\S]*?record\.window\.focus\(\)[\s\S]*?record\.appView\.webContents\.focus\(\)/u);
});

test("debugger detach 只后台重连，不把页面当成崩溃刷新", () => {
  assert.match(source, /debugger-detached:[\s\S]*?reconnectDebugger/u);
  assert.match(source, /private reconnectDebugger\(/u);
  const debuggerDetach = section(source, 'debuggerApi.on("detach"', "private reconnectDebugger(");
  assert.doesNotMatch(debuggerDetach, /reloadAndWait|invalidateAndRecover|setVisible\(false\)/u);
  assert.doesNotMatch(debuggerDetach, /type:\s*["']page_crashed["']/u);
});

test("浏览器组件状态只有完整运行链路和统一版本一致时才显示就绪", () => {
  assert.match(settingsBrowserSource, /function browserRuntimeReady\(\): boolean/u);
  assert.match(settingsBrowserSource, /desktopInfo\?\.runtime\.ready === true/u);
  assert.match(settingsBrowserSource, /capabilitySnapshot\?\.hostStatus === 'ready'/u);
  assert.match(settingsBrowserSource, /capabilitySnapshot\.hostProtocolCompatible/u);
  assert.match(settingsBrowserSource, /function hostComponentStatus\(\): string/u);
});

test("下载只写入 Magi 私有目录，文件选择器保持 Chromium 原生行为", () => {
  assert.match(source, /browserSession\.on\("will-download"/u);
  assert.match(source, /item\.setSavePath\(join\(directory/u);
  assert.match(source, /browser_download_storage_unavailable/u);
  assert.doesNotMatch(source, /setInterceptFileChooserDialog|fileChooserInterceptionConfigured|Page\.fileChooserOpened/u);
  assert.match(source, /type: "download"/u);
});

test("Surface 重启恢复会让 Worker 清理旧页面运行态", () => {
  assert.match(browserRuntimeSource, /const resetRuntimeState = navigationChanged \|\| lifecycleChanged/u);
  assert.match(browserRuntimeSource, /traceActive: resetRuntimeState \? false/u);
  assert.match(browserRuntimeSource, /profilerActive: resetRuntimeState \? false/u);
  assert.match(browserRuntimeSource, /coverageActive: resetRuntimeState \? false/u);
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
  const viewportMethods = section(source, "private async applyViewport(", "private scheduleViewportApply(");
  assert.doesNotMatch(viewportMethods, /capturePage\(|startScreencast|drawImage\(/u);
  assert.match(browserTabSource, /VIEWPORT_DEVICE_MODES = \[[\s\S]*?id: 'wide'[\s\S]*?id: 'narrow'/u);
  assert.match(browserTabSource, /scheduleCustomViewportUpdate\(\)/u);
});

test("截图统一通过同一 WebContents 的 Chromium CDP，root 不依赖 DOM ref", () => {
  assert.match(source, /method === "Page\.captureScreenshot"/u);
  assert.match(source, /sendCdpCommandWithTimeout\([\s\S]*?method/u);
  assert.doesNotMatch(source, /record\.contents\.capturePage\(/u);
  assert.doesNotMatch(source, /captureScreenshot\(/u);
  assert.match(source, /private enqueueCdp[\s\S]*?record\.cdpLane/u);
  assert.doesNotMatch(source, /fromSurface: false/u);
  assert.doesNotMatch(source, /startScreencast|drawImage\(/u);
  assert.match(browserRuntimeSource, /element_ref !== "root"[\s\S]*?else if \(input\.clip\)/u);
  assert.match(browserRuntimeSource, /input\.clip\.width \* viewport\.width/u);
});

test("后台 Browser Surface 仍可被自动化，内容槽只决定可见性", () => {
  assert.doesNotMatch(source, /Page\.captureScreenshot[\s\S]*?browser_surface_no_content_slot/u);
  assert.match(source, /自动化面向逻辑 Browser Tab 的真实 WebContents/u);
});

test("WebContents 销毁时必须清理 Surface 挂载和注册索引", () => {
  const destroyed = section(source, 'contents.once("destroyed"', "this.installSurfacePolicy(record)");
  assert.match(destroyed, /this\.closeRecord\(record\)/u);
  const closeRecord = section(source, "private closeRecord(", "private removeRecordIndexes(");
  assert.match(closeRecord, /const wasClosed = record\.closed/u);
  assert.match(closeRecord, /this\.detachSurface\(record, window\)/u);
  assert.match(closeRecord, /if \(!wasClosed && !record\.contents\.isDestroyed\(\)\) record\.contents\.close\(\)/u);
});

test("后台截图复用同一个 WebContents 的 CDP，不创建隐藏窗口或临时挂载 Surface", () => {
  assert.doesNotMatch(source, /capturePage\(|captureWindow/u);
  assert.match(source, /Page\.captureScreenshot/u);
});

test("所有工具截图都从真实 WebContents 读取，并通过 Surface CDP lane 串行化", () => {
  const cdpSection = section(source, "async sendCdp(", "private enqueueCdp<T>(");
  assert.doesNotMatch(cdpSection, /capturePage\(/u);
  assert.match(cdpSection, /this\.enqueueCdp\(record/u);
});

test("render-process-gone 才触发页面崩溃恢复", () => {
  const renderProcessGone = section(source, 'webContents.on("render-process-gone"', "private async waitForDebugger(");
  assert.match(renderProcessGone, /this\.unmountSurface\(record/u);
  assert.match(renderProcessGone, /type: "page_crashed"/u);
  assert.match(renderProcessGone, /this\.invalidateAndRecover\(record/u);
  assert.doesNotMatch(renderProcessGone, /debugger-detached/u);
});

test("标记 Overlay 只覆盖当前浏览器内容槽并保持菜单层级", () => {
  assert.match(overlayManagerSource, /state\.kind === "annotation"[\s\S]*?browserContentBounds/u);
  assert.match(overlayManagerSource, /if \(!browserContentBounds\) throw new Error\("desktop_overlay_browser_content_unavailable"\)/u);
  assert.match(overlayManagerSource, /private setOverlayBounds\([\s\S]*?record\.layer\.setBounds\(bounds\)/u);
  assert.match(overlayManagerSource, /TRANSPARENT_VIEW_BACKGROUND/u);
  assert.match(browserTabSource, /openAnnotationCommentOverlay\(\)/u);
});

test("Overlay 菜单先卸载再分发选择，避免新建浏览器 Tab 时残留遮挡层", () => {
  const handleAction = section(overlayManagerSource, "handleAction(windowId", "isWebContents(");
  assert.match(handleAction, /if \(shouldCloseBeforeDispatch\) this\.close\(windowId\);[\s\S]*?this\.#onAction\(windowId, action\)/u);
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
  assert.match(windowManagerSource, /window\.contentView\.addChildView\(appLayer\)[\s\S]*?window\.contentView\.addChildView\(appView\)[\s\S]*?window\.contentView\.addChildView\(browserLayer\)[\s\S]*?window\.contentView\.addChildView\(overlayLayer\)/u);
  assert.match(source, /record\.host\.addChildView\(record\.view, 0\)/u);
  assert.match(windowManagerSource, /closeOverlay\([\s\S]*?record\.appView\.webContents\.focus\(\)/u);
});

test("清理浏览数据等待每个活动页面完成刷新", () => {
  const clearData = section(source, "async clearBrowsingData(): Promise<void>", "private surfaceForTab(");
  assert.match(clearData, /clearCache\(\)/u);
  assert.match(clearData, /clearStorageData\(\)/u);
  assert.match(clearData, /await Promise\.all\(\[\.\.\.this\.#surfaces\.values\(\)\]/u);
  assert.match(clearData, /reloadAndWait\(record\.contents, true\)/u);
});
