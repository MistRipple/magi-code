import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./browser-surface-manager.ts", import.meta.url), "utf8");
const windowManagerSource = readFileSync(new URL("./window-manager.ts", import.meta.url), "utf8");
const overlayManagerSource = readFileSync(new URL("./desktop-overlay-manager.ts", import.meta.url), "utf8");
const indexSource = readFileSync(new URL("./index.ts", import.meta.url), "utf8");
const workerSource = readFileSync(new URL("./automation-worker.ts", import.meta.url), "utf8");
const rightPaneSource = readFileSync(
  new URL("../../../../web/src/web/RightPane.svelte", import.meta.url),
  "utf8",
);
const browserTabSource = readFileSync(
  new URL("../../../../web/src/components/tabs/BrowserTabContent.svelte", import.meta.url),
  "utf8",
);

test("Browser Surface 无内容槽时也必须零尺寸挂入宿主", () => {
  assert.doesNotMatch(source, /initialHiddenSurfaceBounds/u);
  assert.doesNotMatch(source, /view\.setBounds\(initial/u);
  assert.doesNotMatch(source, /#browserSlotBounds/u);
  assert.match(source, /this\.applySlot\(record, null, window\)/u);
  assert.match(source, /零尺寸、不可见状态存在/u);
  assert.match(
    source,
    /private unmountSurface\([\s\S]*?setVisible\(false\)[\s\S]*?保留最后有效的非零 bounds/u,
  );
  assert.match(source, /private detachSurface\([\s\S]*?removeChildView\(record\.view\)[\s\S]*?record\.mounted = false/u);
});

test("Surface materialize 统一首个导航，激活路径可异步等待网络", () => {
  const materialize = source.slice(
    source.indexOf("async materialize("),
    source.indexOf("private async createSurface("),
  );
  assert.match(materialize, /const load = this\.startLoad\(record, initialUrl\)/u);
  assert.match(materialize, /if \(input\.awaitPageLoad !== false\) await load/u);
  assert.doesNotMatch(materialize, /void record\.contents\.loadURL/u);
  assert.match(windowManagerSource, /awaitPageLoad: false/u);
});

test("控制权更新不得等待依赖文档树的光标 CDP", () => {
  const updateControl = source.slice(
    source.indexOf("async updateControl("),
    source.indexOf("async closeTab(", source.indexOf("async updateControl(")),
  );
  assert.match(updateControl, /void this\.setAgentCursor\(/u);
  assert.match(source, /record\.priming \|\| record\.contents\.isLoadingMainFrame\(\) \|\| !record\.contents\.getURL\(\)/u);
  assert.match(source, /CURSOR_CDP_COMMAND_TIMEOUT_MS/u);
});

test("截图必须保留 Page.captureScreenshot 的 CDP 语义", () => {
  assert.doesNotMatch(source, /capturePage\(/u);
  assert.doesNotMatch(source, /captureNativeScreenshot/u);
  assert.match(source, /method === "Page\.captureScreenshot"/u);
  assert.match(source, /browser_surface_no_content_slot/u);
  assert.match(source, /sendCdpCommandWithTimeout\(/u);
  assert.match(source, /params,/u);
});

test("没有内容槽时截图在进入 CDP 队列前结构化失败", () => {
  const check = source.indexOf("browser_surface_no_content_slot");
  const debuggerCheck = source.indexOf("browser_debugger_detached");
  assert.notEqual(check, -1);
  assert.notEqual(debuggerCheck, -1);
  assert.ok(check < debuggerCheck, "截图槽位检查必须先于 debugger 状态检查");
  assert.match(source, /!record\.slotVisible/u);
  assert.match(source, /!record\.slotBounds/u);
  assert.match(source, /!record\.mounted/u);
});

test("视觉激活不得抢占 Primary Surface", () => {
  assert.match(source, /if \(!this\.#surfaces\.primaryForTab\(record\.tabId\)\) this\.promote\(record\.surfaceId\);/u);
  assert.doesNotMatch(source, /focus\(surfaceId: string\)/u);
  assert.doesNotMatch(source, /webContents\.on\("focus"[\s\S]*?this\.promote/u);
  assert.match(source, /before-input-event[\s\S]*?this\.promote\(record\.surfaceId\)/u);
  assert.match(source, /before-mouse-event[\s\S]*?this\.promote\(record\.surfaceId\)/u);
});

test("不同窗口的 Surface 使用逻辑 Tab 全局单调 revision", () => {
  assert.match(source, /surfaceRevision: this\.#surfaces\.nextRevision\(input\.tabId\)/u);
  assert.match(source, /record\.surfaceRevision = this\.#surfaces\.nextRevision\(record\.tabId\)/u);
  assert.doesNotMatch(source, /surfaceRevision \+= 1/u);
});

test("重新选择 Browser Tab 不得重置该 Surface 的临时 viewport", () => {
  const materialize = source.slice(
    source.indexOf("async materialize("),
    source.indexOf("private async createSurface("),
  );
  assert.doesNotMatch(materialize, /record\.viewport\s*=/u);
  assert.doesNotMatch(materialize, /record\.viewportApplied\s*=\s*false/u);
});

test("窗口布局快照变化必须重新发布当前 Browser 内容槽", () => {
  const source = readFileSync(
    new URL("../../../../web/src/components/tabs/BrowserTabContent.svelte", import.meta.url),
    "utf8",
  );
  assert.match(
    source,
    /desktop\?\.onSnapshot\(\(next\) => \{[\s\S]*?scheduleBrowserSlotBounds\(\);/u,
  );
  assert.match(source, /visualViewport\?\.addEventListener\('resize', scheduleBrowserSlotBounds\)/u);
  assert.match(source, /new ResizeObserver\(scheduleBrowserSlotBounds\)[\s\S]*?observer\.observe\(slot\)/u);
  assert.match(source, /const available = browserSlotHostAvailable;[\s\S]*?void tick\(\)\.then\(scheduleBrowserSlotBounds\)/u);
});

test("Desktop Browser Surface 不得被过期的 Authority lifecycle 阻塞", () => {
  const browserSlotHostAvailable = browserTabSource.slice(
    browserTabSource.indexOf("const browserSlotHostAvailable"),
    browserTabSource.indexOf("const browserSurfaceAvailable", browserTabSource.indexOf("const browserSlotHostAvailable")),
  );
  assert.match(browserSlotHostAvailable, /desktopSnapshot\?\.layout\.activePanelKind === 'browser'[\s\S]*?desktopSnapshot\.layout\.activeTabId === tabId/u);
  assert.doesNotMatch(browserSlotHostAvailable, /lifecycle\s*===\s*'ready'/u);
  const browserSurfaceAvailable = browserTabSource.slice(
    browserTabSource.indexOf("const browserSurfaceAvailable"),
    browserTabSource.indexOf("const browserReady", browserTabSource.indexOf("const browserSurfaceAvailable")),
  );
  assert.match(browserSurfaceAvailable, /browserSlotHostAvailable[\s\S]*?desktopSnapshot\?\.layout\.activeSurfaceId/u);
  assert.match(browserTabSource, /desktopSnapshot = next;/u);

  const desktopSyncEffect = browserTabSource.slice(
    browserTabSource.indexOf("$effect(() => {\n    if (!desktopRuntime"),
    browserTabSource.indexOf("$effect(() => {\n    const expectedSessionId"),
  );
  assert.notEqual(desktopSyncEffect.indexOf("void synchronizeDesktopSurface();"), -1);
  assert.doesNotMatch(desktopSyncEffect, /lifecycle\s*!==\s*'ready'/u);
});

test("窄右栏下浏览器工具栏和内容槽允许完整收缩", () => {
  assert.match(browserTabSource, /\.browser-toolbar \{[^}]*width: 100%;[^}]*min-width: 0;[^}]*overflow: hidden;/u);
  assert.match(browserTabSource, /\.address-form \{[^}]*flex: 1 1 0;[^}]*min-width: 0;/u);
  assert.match(rightPaneSource, /\.right-pane-body \{[^}]*min-width: 0;[^}]*min-height: 0;/u);
});

test("浏览器首帧前保持原生 Surface 隐藏并报告加载失败", () => {
  assert.match(source, /type: "page_failed"/u);
  assert.match(source, /did-fail-load[\s\S]*?this\.unmountSurface\(record,/u);
  assert.match(source, /if \(record\.loadFailed\)[\s\S]*?record\.view\.setVisible\(false\)[\s\S]*?return;/u);
  assert.match(source, /did-finish-load[\s\S]*?this\.applySlot\(/u);
  assert.match(source, /const visible = bounds !== null && !record\.priming && !record\.loadFailed/u);
});

test("浏览器 Tab 切换先卸载旧 Surface，再异步物化新页面", () => {
  const activationStart = windowManagerSource.indexOf("async activateBrowser(");
  const activation = windowManagerSource.slice(
    activationStart,
    windowManagerSource.indexOf("\n  updateBrowserSlot(", activationStart),
  );
  const retireOldSurface = activation.indexOf('updateBrowserSlot(input.windowId, "", null)');
  const materializeNewSurface = activation.indexOf("await this.#surfaceManager.materialize");
  assert.notEqual(retireOldSurface, -1);
  assert.notEqual(materializeNewSurface, -1);
  assert.ok(retireOldSurface < materializeNewSurface);
  assert.match(activation, /activationGeneration: activationRevision/u);
  assert.match(activation, /const browserSlot = readBrowserSlot\(record\);[\s\S]*?this\.applyLayout\(record\)[\s\S]*?browserSlot\.bounds/u);
  assert.match(windowManagerSource, /normalizeBrowserSlotBoundsWithinPane\([\s\S]*?normalizeBrowserSlotBounds\(bounds\),[\s\S]*?snapshotWindowLayout\(record\.layout\)\.rightPaneBounds/u);
});

test("已有 ready Surface 切回后必须用保留的内容槽重新挂载并显示", () => {
  assert.match(source, /if \(record\.tabId === tabId\) \{[\s\S]*?this\.applySlot\(record, bounds, window\)/u);
  assert.match(source, /else if \(bounds\) \{[\s\S]*?this\.unmountSurface\(record, window\)/u);
  assert.match(source, /const visible = bounds !== null && !record\.priming && !record\.loadFailed/u);
  assert.match(
    windowManagerSource,
    /const browserSlot = readBrowserSlot\(record\);[\s\S]*?activeSurfaceId[\s\S]*?updateBrowserSlot\([\s\S]*?browserSlot\.bounds/u,
  );
});

test("真实 Browser Surface 首次挂载后获得输入焦点，后续 resize 不抢焦点", () => {
  assert.match(
    source,
    /const wasMounted = record\.mounted;[\s\S]*?const visible = bounds !== null && !record\.priming && !record\.loadFailed;[\s\S]*?if \(!wasMounted && visible\) record\.contents\.focus\(\)/u,
  );
});

test("过期激活代次不得挂载 Surface，并回收无主新 Surface", () => {
  assert.match(source, /#activationGenerations/u);
  assert.match(source, /setActivationGeneration\(windowId: string, generation: number\)/u);
  assert.match(source, /assertActivationCurrent\(input\.windowId, input\.activationGeneration\)/u);
  assert.match(source, /browser_surface_activation_stale/u);
  assert.match(
    source,
    /created[\s\S]*?record\.activationGeneration === input\.activationGeneration[\s\S]*?this\.closeRecord\(record, false\)/u,
  );
});

test("清理非当前 Tab 的空槽位不得卸载当前 Surface", () => {
  const update = source.slice(
    source.indexOf("updateBrowserSlot(windowId: string, tabId: string, bounds"),
    source.indexOf("bindingForTabInWindow", source.indexOf("updateBrowserSlot(windowId: string, tabId: string, bounds")),
  );
  assert.match(update, /if \(!tabId\) \{[\s\S]*?this\.applySlot\(record, null, window\)/u);
  assert.match(update, /if \(record\.tabId === tabId\) \{[\s\S]*?this\.applySlot\(record, bounds, window\)/u);
  assert.match(update, /else if \(bounds\) \{[\s\S]*?this\.unmountSurface\(record, window\)/u);
  assert.doesNotMatch(update, /const visible = record\.tabId === tabId && bounds !== null/u);
  assert.match(windowManagerSource, /if \(!record\.layout\.rightPaneVisible\)[\s\S]*?updateBrowserSlot\(windowId, "", null\)/u);
  assert.match(windowManagerSource, /record\.layout\.activePanelKind !== "browser"[\s\S]*?if \(!bounds\) this\.#surfaceManager\.updateBrowserSlot\(windowId, tabId, null\)/u);
});

test("失效或恢复中的 Surface 只隐藏并归零，关闭时才从原生子视图树卸载", () => {
  assert.match(source, /did-fail-load[\s\S]*?record\.priming = true[\s\S]*?this\.unmountSurface\(record,/u);
  assert.match(source, /invalidateAndRecover[\s\S]*?record\.priming = true[\s\S]*?this\.unmountSurface\(record,/u);
  assert.match(source, /private unmountSurface[\s\S]*?setVisible\(false\)[\s\S]*?保留最后有效的非零 bounds/u);
  assert.match(source, /private detachSurface[\s\S]*?removeChildView\(record\.view\)/u);
  assert.match(windowManagerSource, /updateBrowserSlot\(input\.windowId, "", null\)/u);
});

test("daemon 重启时主 Renderer 只恢复可信失败页面并撤下旧 Browser Surface", () => {
  assert.match(windowManagerSource, /rendererLoadFailed: boolean/u);
  assert.match(
    windowManagerSource,
    /did-fail-load[\s\S]*?record\.rendererLoadFailed = true[\s\S]*?updateBrowserSlot\(record\.windowId, "", null\)/u,
  );
  assert.match(
    windowManagerSource,
    /restoreAfterDaemonReady\(\)[\s\S]*?record\.rendererLoadFailed[\s\S]*?this\.loadAppRenderer/u,
  );
  assert.match(
    windowManagerSource,
    /render-process-gone[\s\S]*?record\.browserSlot = null[\s\S]*?updateBrowserSlot\(record\.windowId, "", null\)[\s\S]*?this\.loadAppRenderer/u,
  );
  assert.match(
    windowManagerSource,
    /url\.origin === this\.#agentOrigin[\s\S]*?url\.pathname === "\/web\.html"[\s\S]*?desktopWindowId/u,
  );
  assert.match(
    windowManagerSource,
    /handleDisplayMetricsChanged[\s\S]*?updateBounds\(\)/u,
  );
});

test("daemon 重启时原生 Overlay Renderer 也会从失败状态恢复", () => {
  assert.match(overlayManagerSource, /loadFailed: boolean/u);
  assert.match(
    overlayManagerSource,
    /did-fail-load[\s\S]*?record\.loadFailed = true[\s\S]*?this\.syncVisibility\(record\)/u,
  );
  assert.match(
    overlayManagerSource,
    /restoreAfterDaemonReady\(\)[\s\S]*?record\.loadFailed[\s\S]*?this\.loadRenderer/u,
  );
});

test("OverlayLayer 与子 View 的可见性只能由统一生命周期同步", () => {
  const syncStart = overlayManagerSource.indexOf("  private syncVisibility(record: OverlayRecord): void");
  assert.notEqual(syncStart, -1);
  const syncEnd = overlayManagerSource.indexOf("\n  private mountOnLayer(", syncStart);
  assert.notEqual(syncEnd, -1);
  const syncSource = overlayManagerSource.slice(syncStart, syncEnd);
  const outsideSyncSource = overlayManagerSource.slice(0, syncStart) + overlayManagerSource.slice(syncEnd);

  assert.match(syncSource, /record\.layer\.setVisible\(visible\)/u);
  assert.match(syncSource, /record\.view\.setVisible\(visible\)/u);
  assert.doesNotMatch(outsideSyncSource, /record\.(?:layer|view)\.setVisible\(/u);
  assert.match(
    overlayManagerSource,
    /this\.#records\.set\(windowId, record\);[\s\S]*?this\.syncVisibility\(record\);/u,
  );
  assert.match(
    overlayManagerSource,
    /render-process-gone[\s\S]*?record\.ready = false;[\s\S]*?this\.syncVisibility\(record\)/u,
  );
  assert.match(
    overlayManagerSource,
    /catch[\s\S]*?record\.loadFailed = true;[\s\S]*?this\.syncVisibility\(record\)/u,
  );
  assert.match(
    overlayManagerSource,
    /close\(windowId: string\): void[\s\S]*?record\.state = null;[\s\S]*?this\.syncVisibility\(record\)/u,
  );
  assert.match(
    overlayManagerSource,
    /closeWindow\(windowId: string\): void[\s\S]*?record\.state = null;[\s\S]*?this\.syncVisibility\(record\)/u,
  );
});

test("Overlay 在 Renderer 完成加载和握手前不得显示或拦截右栏输入", () => {
  assert.match(
    overlayManagerSource,
    /const visible = \([\s\S]*?!record\.loadFailed[\s\S]*?record\.view\.setVisible\(visible\)/u,
  );
  assert.match(overlayManagerSource, /open\([\s\S]*?this\.syncVisibility\(record\)/u);
  assert.match(overlayManagerSource, /handleReady\([\s\S]*?this\.syncVisibility\(record\)/u);
  const handleReady = overlayManagerSource.slice(
    overlayManagerSource.indexOf("  handleReady(windowId: string): void"),
    overlayManagerSource.indexOf("\n  close(windowId: string): void"),
  );
  assert.doesNotMatch(handleReady, /!record\.loaded/u);
  assert.match(overlayManagerSource, /loadPromise: Promise<void> \| null/u);
});

test("标记选择层必须覆盖当前 Browser 内容槽，而不是只占用顶部菜单高度", () => {
  assert.match(
    overlayManagerSource,
    /state\.kind === "annotation"[\s\S]*?if \(!browserContentBounds\) throw new Error\("desktop_overlay_browser_content_unavailable"\)[\s\S]*?return \{ \.\.\.browserContentBounds \}/u,
  );
  assert.match(windowManagerSource, /record\.browserSlot\?\.bounds \?\? null/u);
  assert.match(windowManagerSource, /updateLayout\(windowId, snapshot\.layout, normalizedBounds\)/u);
});

test("每个 Desktop 窗口只建立一次固定的 App、Browser、Overlay 三层", () => {
  const createWindow = windowManagerSource.slice(
    windowManagerSource.indexOf("  createWindow(): string"),
    windowManagerSource.indexOf("\n  async restoreAfterDaemonReady()", windowManagerSource.indexOf("  createWindow(): string")),
  );
  assert.match(
    createWindow,
    /const appLayer = new View\(\);\s*const browserLayer = new View\(\);\s*const overlayLayer = new View\(\);/u,
  );
  assert.match(
    createWindow,
    /window\.contentView\.addChildView\(appLayer\);\s*window\.contentView\.addChildView\(browserLayer\);\s*window\.contentView\.addChildView\(overlayLayer\);/u,
  );
  assert.equal(
    [...createWindow.matchAll(/window\.contentView\.addChildView\(/gu)].length,
    3,
  );
  assert.match(
    createWindow,
    /appLayer\.addChildView\(appView\);\s*this\.#surfaceManager\.attachWindow\(windowId, window, browserLayer\);\s*this\.#overlayManager\.create\(windowId, window, overlayLayer\);/u,
  );
});

test("BrowserSurfaceManager 只能挂载到 WindowManager 提供的 BrowserLayer", () => {
  assert.doesNotMatch(source, /contentView\.(?:addChildView|removeChildView)\(/u);
  assert.match(source, /attachWindow\(windowId: string, window: BaseWindow, layer: View\)/u);
  assert.match(source, /this\.#layers\.set\(windowId, layer\)/u);
  assert.match(source, /attachWindow\([\s\S]*?layer\.setVisible\(false\)/u);
  assert.match(source, /private syncLayerVisibility\([\s\S]*?layer\.setVisible\(visible\)/u);
  assert.match(source, /updateBrowserSlot\([\s\S]*?this\.syncLayerVisibility\(windowId\)/u);
  assert.doesNotMatch(windowManagerSource, /browserLayer\.setVisible\((?!false\))/u);
  assert.match(
    source,
    /record\.layer\.addChildView\(record\.view\);[\s\S]*?record\.mounted = true/u,
  );
  assert.match(source, /record\.layer\.removeChildView\(record\.view\)/u);
});

test("OverlayManager 只挂载到固定 OverlayLayer，不重建原生视图", () => {
  assert.doesNotMatch(overlayManagerSource, /contentView\.removeChildView\(/u);
  assert.match(overlayManagerSource, /create\(windowId: string, window: BaseWindow, layer: View\)/u);
  assert.match(
    overlayManagerSource,
    /private mountOnLayer\([\s\S]*?if \(record\.window\.isDestroyed\(\)\) return;[\s\S]*?record\.layer\.addChildView\(record\.view\);/u,
  );
  assert.doesNotMatch(overlayManagerSource, /contentView\.addChildView\(/u);
  assert.match(overlayManagerSource, /create\([\s\S]*?this\.syncVisibility\(record\)/u);
  assert.match(overlayManagerSource, /private syncVisibility\([\s\S]*?record\.layer\.setVisible\(visible\)/u);
  const open = overlayManagerSource.slice(
    overlayManagerSource.indexOf("  open("),
    overlayManagerSource.indexOf("\n  handleReady(", overlayManagerSource.indexOf("  open(")),
  );
  const updateLayout = overlayManagerSource.slice(
    overlayManagerSource.indexOf("  updateLayout("),
    overlayManagerSource.indexOf("\n  handleAction(", overlayManagerSource.indexOf("  updateLayout(")),
  );
  assert.match(open, /this\.mountOnLayer\(record\)/u);
  assert.match(updateLayout, /this\.mountOnLayer\(record\)/u);
});

test("Overlay 菜单锚点必须来自主 Renderer 的真实 DOM 控件边界", () => {
  assert.match(
    rightPaneSource,
    /const anchor = addPaneButtonElement\?\.getBoundingClientRect\(\);[\s\S]*?anchorBounds: \{[\s\S]*?x: anchor\.left,[\s\S]*?y: anchor\.top,[\s\S]*?width: anchor\.width,[\s\S]*?height: anchor\.height,/u,
  );
  assert.match(
    browserTabSource,
    /const anchor = viewportMenuButton\?\.getBoundingClientRect\(\);[\s\S]*?placement: 'browser-viewport',[\s\S]*?anchorBounds: \{[\s\S]*?x: anchor\.left,[\s\S]*?y: anchor\.top,/u,
  );
  assert.match(
    browserTabSource,
    /const anchor = annotationHistoryButton\?\.getBoundingClientRect\(\);[\s\S]*?placement: 'browser-annotations',[\s\S]*?anchorBounds: \{[\s\S]*?x: anchor\.left,[\s\S]*?y: anchor\.top,/u,
  );
  assert.match(indexSource, /if \(kind === "menu" && !anchorBounds\) throw new Error\("desktop_overlay_anchor_required"\)/u);
  assert.match(overlayManagerSource, /state\.kind === "annotation"[\s\S]*?if \(!browserContentBounds\) throw new Error\("desktop_overlay_browser_content_unavailable"\)[\s\S]*?return \{ \.\.\.browserContentBounds \}/u);
});

test("清理浏览器数据必须覆盖重启后尚未物化的持久 partition", () => {
  assert.match(source, /#knownPartitions/u);
  assert.match(source, /partitionRegistryPath\?: string/u);
  assert.match(source, /new Set\(\[\.\.\.this\.#knownPartitions, \.\.\.this\.#configuredPartitions\]\)/u);
  assert.match(source, /persistPartitionRegistry\(this\.#partitionRegistryPath, this\.#knownPartitions\)/u);
});

test("可信 Renderer 禁止导航到外部页面，并且 IPC 按 Renderer 角色隔离", () => {
  assert.match(windowManagerSource, /setWindowOpenHandler\(\(\) => \(\{ action: "deny" \}\)\)/u);
  assert.match(windowManagerSource, /will-navigate[\s\S]*?isTrustedAppRendererUrl[\s\S]*?preventDefault/u);
  assert.match(overlayManagerSource, /setWindowOpenHandler\(\(\) => \(\{ action: "deny" \}\)\)/u);
  assert.match(overlayManagerSource, /will-navigate[\s\S]*?isTrustedRendererUrl[\s\S]*?preventDefault/u);
  assert.match(indexSource, /function trustedAppSender\(/u);
  assert.match(indexSource, /rendererRoleForWebContents\(webContentsId\) !== "app"/u);
});

test("过期的空内容槽事件不能清除更新后的 Browser Surface", () => {
  const update = windowManagerSource.slice(
    windowManagerSource.indexOf("  updateBrowserSlot("),
    windowManagerSource.indexOf("\n  activatePanel(", windowManagerSource.indexOf("  updateBrowserSlot(")),
  );
  const revisionGuard = update.indexOf("if (slotRevision <= record.browserSlotRevision)");
  const clearSlot = update.indexOf("record.browserSlot = null");
  assert.ok(revisionGuard >= 0 && clearSlot >= 0 && revisionGuard < clearSlot);
});

test("非单实例进程不得进入 Electron 主启动链路", () => {
  assert.match(indexSource, /if \(singleInstance\) \{[\s\S]*?app\.whenReady\(\)/u);
});

test("全局 blocking overlay 统一隐藏 Browser Surface 并按最新槽位恢复", () => {
  assert.match(windowManagerSource, /blockingOverlayActive: boolean/u);
  assert.match(windowManagerSource, /setBlockingOverlay\(windowId: string, active: boolean\)/u);
  assert.match(windowManagerSource, /record\.appView\.webContents\.focus\(\)/u);
  assert.match(windowManagerSource, /const showBrowserSurface = !record\.blockingOverlayActive/u);
  assert.match(windowManagerSource, /if \(!showBrowserSurface\) \{[\s\S]*?updateBrowserSlot\(record\.windowId, "", null\)/u);
  assert.match(windowManagerSource, /else if \(browserSlot && browserSlotMatchesLayout\) \{[\s\S]*?browserSlot\.bounds/u);
  assert.match(indexSource, /magi-desktop:set-blocking-overlay/u);
});

test("重启 Automation Worker 必须等待新的 worker_ready 握手", () => {
  assert.match(workerSource, /async restart\(\): Promise<void>/u);
  assert.match(workerSource, /this\.start\(\);[\s\S]*?await this\.waitUntilReady\(\);/u);
  assert.match(indexSource, /magi-desktop:restart-browser-automation[\s\S]*?await automationWorker!\.restart\(\)/u);
});

test("每个 Tab 命令队列的尾部始终收敛，不吞掉后续命令", () => {
  assert.match(
    readFileSync(new URL("./desktop-control-server.ts", import.meta.url), "utf8"),
    /const tail = next\.then\([\s\S]*?this\.#queues\.set\(queueKey, tail\)[\s\S]*?this\.#queues\.get\(queueKey\) === tail/u,
  );
});
