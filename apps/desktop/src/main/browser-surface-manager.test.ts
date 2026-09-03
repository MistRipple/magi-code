import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { matchesNavigationRevision } from "./browser-navigation-revision.js";

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
const workbenchSource = readFileSync(
  new URL("../../../../web/src/web/WebWorkbenchShell.svelte", import.meta.url),
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
const rightPaneStoreSource = readFileSync(
  new URL("../../../../web/src/stores/right-pane.svelte.ts", import.meta.url),
  "utf8",
);
const browserHostProtocolSource = readFileSync(
  new URL("../../../../crates/magi-browser-authority/src/host_protocol.rs", import.meta.url),
  "utf8",
);
const browserAuthorityDomainSource = readFileSync(
  new URL("../../../../crates/magi-browser-authority/src/domain.rs", import.meta.url),
  "utf8",
);
const browserHostSource = readFileSync(
  new URL("../../../../crates/magi-daemon/src/daemon/browser_host.rs", import.meta.url),
  "utf8",
);
const browserRoutesSource = readFileSync(
  new URL("../../../../crates/magi-api/src/routes/browser.rs", import.meta.url),
  "utf8",
);
const browserContractsSource = readFileSync(
  new URL("../../../../contracts/desktop-browser/src/index.ts", import.meta.url),
  "utf8",
);
const browserToolSchema = JSON.parse(readFileSync(
  new URL("../../../../contracts/desktop-browser/browser-tool.schema.json", import.meta.url),
  "utf8",
)) as {
  "x-magi-browser-tool-catalog": Array<{
    name: string;
    description: string;
    inputSchema: {
      additionalProperties?: boolean;
      properties?: Record<string, unknown>;
    };
  }>;
};

function section(value: string, startMarker: string, endMarker: string): string {
  const start = value.indexOf(startMarker);
  assert.notEqual(start, -1, `missing section start: ${startMarker}`);
  const end = value.indexOf(endMarker, start + startMarker.length);
  assert.notEqual(end, -1, `missing section end: ${endMarker}`);
  return value.slice(start, end);
}

function withoutLineComments(value: string): string {
  return value.replace(/\/\/[^\n]*/gu, "");
}

test("Browser Surface 只接受 Main 布局计算出的内容槽", () => {
  assert.match(source, /bindContentSurface\([\s\S]*?bounds: Rectangle \| null,/u);
  assert.doesNotMatch(source, /clipBounds|intersectRect/u);
  assert.match(source, /const contentRoot = this\.#contentRoots\.get\(input\.windowId\)/u);
  assert.match(source, /contentRoot\.addChildView\(record\.view, 1\)/u);
  assert.match(source, /record\.view\.setBounds\(effectiveBounds\)/u);
  assert.match(source, /this\.#contentRoots\.get\(record\.windowId\)\?\.removeChildView\(record\.view\)/u);
});

test("Browser Tab 在 Chromium 创建前严格禁止网页子 Tab", () => {
  const policy = section(source, "private installSurfacePolicy(", "webContents.on(\"will-navigate\"");
  assert.match(policy, /setWindowOpenHandler\(\(details\) => \{/u);
  assert.doesNotMatch(policy, /did-create-window|childWindow\.close\(\)/u);
  assert.match(policy, /will-attach-webview[\s\S]*?event\.preventDefault\(\)/u);
  assert.match(policy, /return \{ action: "deny" \};/u);
  assert.doesNotMatch(policy, /action:\s*"allow"/u);
  assert.doesNotMatch(policy, /createWindow|new-window/u);
  assert.match(policy, /type: "popup_blocked"/u);
  assert.doesNotMatch(policy, /\.loadURL\(/u);
  assert.doesNotMatch(policy, /new\s+(BrowserWindow|WebContentsView)/u);
  assert.equal(
    [...source.matchAll(/new WebContentsView\(/gu)].length,
    1,
    "BrowserSurfaceManager 只能在 Surface 工厂创建 WebContentsView，窗口打开请求不得创建第二个 View",
  );
  const preferences = section(source, "const view = new WebContentsView({", "const contents = view.webContents;");
  assert.doesNotMatch(withoutLineComments(preferences), /nativeWindowOpen/u);
  assert.match(preferences, /webviewTag:\s*false/u);
  assert.match(preferences, /allowRunningInsecureContent:\s*false/u);
});

test("target=_blank、window.open 和表单 popup 复用当前一级页面，不创建子 Tab", () => {
  const policy = section(source, "webContents.setWindowOpenHandler(", "webContents.on(\"will-navigate\"");
  assert.match(policy, /return \{ action: "deny" \};/u);
  assert.match(policy, /type: "popup_blocked"/u);
  assert.match(policy, /normalizePopupNavigationUrl\(details\.url\)/u);
  assert.match(policy, /void this\.loadPopupInCurrentPage\(record, url, details\)/u);
  assert.doesNotMatch(policy, /\.loadURL\(/u);
  assert.doesNotMatch(policy, /new\s+(BrowserWindow|WebContentsView)/u);
  const popupNavigation = section(source, "private async loadPopupInCurrentPage(", "private unmountSurface(");
  assert.match(popupNavigation, /this\.loadPage\(\s*record,\s*url/u);
  assert.match(popupNavigation, /httpReferrer:\s*details\.referrer/u);
  assert.match(popupNavigation, /postData:\s*postBody\.data/u);
  assert.match(popupNavigation, /Content-Type: \$\{contentType\}/u);
  assert.match(source, /function normalizePopupNavigationUrl\(/u);
  assert.match(source, /url === "about:blank"\) throw/u);
});

test("唯一 CDP 出口强制自动附着排除页面型 Target", () => {
  const cdpSend = section(source, "private async sendSurfaceCdpCommand(", "private async waitForScreenshotReadiness(");
  assert.match(cdpSend, /constrainCdpCommandParams\(method, params\)/u);
  const policy = section(source, "function constrainCdpCommandParams(", "function sameBounds(");
  assert.match(policy, /method !== "Target\.setAutoAttach"/u);
  assert.match(policy, /params\.autoAttach !== true/u);
  assert.match(policy, /flatten: true/u);
  assert.match(policy, /waitForDebuggerOnStart: false/u);
  assert.match(policy, /filter:\s*\[[\s\S]*\{ type: "iframe" \}[\s\S]*\{ type: "worker" \}[\s\S]*\{ type: "service_worker" \}[\s\S]*\{ type: "shared_worker" \}[\s\S]*\]/u);
  assert.doesNotMatch(policy, /filter:[\s\S]*\{\}/u);
  assert.match(source, /Target\.attachedToTarget/u);
  assert.match(source, /isForbiddenChildPageTarget\(targetInfo\)/u);
  assert.match(source, /blockedCdpSessionIds\.add\(eventParams\.sessionId\)/u);
  assert.match(source, /detachCdpTarget\(record, eventParams\.sessionId\)/u);
  assert.match(source, /blockedCdpSessionIds\.has\(sessionId\)/u);
});

test("browser_tabs 只描述右栏一级 Tab，不允许协议出现子 Tab 字段", () => {
  const entry = browserToolSchema["x-magi-browser-tool-catalog"].find((item) => item.name === "browser_tabs");
  assert.ok(entry, "浏览器工具目录缺少 browser_tabs");
  assert.equal(entry.inputSchema.additionalProperties, false);
  assert.match(entry.description, /一级浏览器标签/u);
  assert.match(entry.description, /绝不创建网页子标签/u);
  assert.match(entry.description, /target=_blank/u);
  assert.ok(entry.inputSchema.properties);
  assert.equal(Object.hasOwn(entry.inputSchema.properties, "parent_tab_id"), false);
  assert.equal(Object.hasOwn(entry.inputSchema.properties, "child_tab_id"), false);
});

test("页面 popup 只在 Main 被拒绝，iframe 和 worker Target 仍可供自动化使用", () => {
  const targetBoundary = section(source, "if (method === \"Target.attachedToTarget\"", "const detachListener:");
  assert.match(targetBoundary, /targetInfo/u);
  assert.match(targetBoundary, /isForbiddenChildPageTarget\(targetInfo\)/u);
  assert.match(targetBoundary, /return;/u);
  const targetPolicy = section(source, "function isForbiddenChildPageTarget(", "function constrainCdpCommandParams(");
  assert.match(targetPolicy, /return !isAllowedBrowserChildTarget\(value\)/u);
  assert.match(browserContractsSource, /BROWSER_CHILD_TARGET_TYPES = \[[\s\S]*?"iframe"[\s\S]*?"worker"[\s\S]*?"service_worker"[\s\S]*?"shared_worker"/u);
  assert.match(browserContractsSource, /isAllowedBrowserChildTarget[\s\S]*?return typeof type === "string"[\s\S]*?includes\(type\)/u);
  assert.match(source, /record\.cdpSessionIds\.add\(eventParams\.sessionId\)/u);
  assert.doesNotMatch(targetBoundary, /cdpSessionIds\.add\(eventParams\.sessionId\)[\s\S]*?isForbiddenChildPageTarget/u);
});

test("Main、Browser Host、Authority 和 Renderer 没有子 Tab 或 Target 创建旁路", () => {
  const forbiddenTargetCommands = [
    "createTarget",
    "createBrowserContext",
    "disposeBrowserContext",
    "attachToTarget",
    "closeTarget",
    "activateTarget",
    "getTargets",
  ];
  for (const command of forbiddenTargetCommands) {
    const pattern = new RegExp(`Target\\.${command}`, "u");
    assert.doesNotMatch(source, pattern, `Main 不得暴露 ${command}`);
    assert.doesNotMatch(workerSource, pattern, `Worker 不得调用 ${command}`);
    assert.doesNotMatch(browserHostProtocolSource, pattern, `Host 协议不得暴露 ${command}`);
    assert.doesNotMatch(browserHostSource, pattern, `Browser Host 不得调用 ${command}`);
  }
  assert.doesNotMatch(browserHostProtocolSource, /CreateTarget|CreateBrowserContext|ChildTab|SubTab/u);
  // Authority 的 target_id 是当前一级 Surface 的绑定身份，不是子 Target
  // 创建入口；禁止的是额外的子 Tab 数据模型或创建命令。
  assert.match(browserAuthorityDomainSource, /pub struct BrowserTab\s*\{/u);
  assert.doesNotMatch(browserAuthorityDomainSource, /ChildTab|SubTab|child_tab|sub_tab|create_target|createTarget/u);
  assert.doesNotMatch(browserTabSource, /Target\.|sessionattached|createCDPSession/u);
  assert.doesNotMatch(rightPaneStoreSource, /Target\.|sessionattached|createCDPSession/u);
  assert.match(rightPaneStoreSource, /authorityTabs\.map\(\(tab\) => `browser:\$\{browserSessionId\}:\$\{tab\.tabId\.trim\(\)\}`/u);
  assert.doesNotMatch(rightPaneStoreSource, /target(?:_|[A-Z])id/u);
});

test("右栏布局和原生 Surface 使用同一 Main 事务", () => {
  assert.match(layoutSource, /export function browserContentBounds\(/u);
  assert.match(layoutSource, /browserContentSlot/u);
  assert.match(layoutSource, /renderer_geometry/u);
  assert.match(windowManagerSource, /private applyLayout\(record: DesktopWindowRecord\)/u);
  assert.match(
    windowManagerSource,
    /const browserSurfaceActive = !record\.blockingOverlayActive[\s\S]*?const rendererGeometryForActiveSurface = browserSurfaceActive[\s\S]*?browserContentSlot\?\.tabId === layout\.activeTabId[\s\S]*?const currentBrowserContentBounds = browserSurfaceActive && rendererGeometryForActiveSurface\s*\?\s*browserContentBounds\(layout\)\s*:\s*null/u,
  );
  assert.match(windowManagerSource, /this\.#surfaceManager\.bindContentSurface\(/u);
  const applyLayout = section(windowManagerSource, "private applyLayout(", "private clearRendererGeometry(");
  assert.match(applyLayout, /bindContentSurface\(\s*record\.windowId,\s*layout\.activeTabId,\s*layout\.activeSurfaceId,\s*currentBrowserContentBounds,\s*currentBrowserParentBounds,\s*\)/u);
  assert.doesNotMatch(applyLayout, /x:\s*0,\s*y:\s*0,\s*width:\s*currentBrowserContentBounds\.width/u);
  assert.doesNotMatch(windowManagerSource, /updateBrowserSlot/u);
});

test("首次显示窗口时强制重放 App Renderer bounds，避免隐藏态默认视口卡住", () => {
  const ready = section(windowManagerSource, "handleRightPaneReady(windowId: string)", "setRendererContext(");
  assert.match(
    ready,
    /record\.window\.setContentSize\([\s\S]*?record\.window\.show\(\)[\s\S]*?record\.appView\.setBounds\(record\.layout\.clientBounds\)[\s\S]*?this\.applyLayout\(record\)/u,
  );
});

test("原生合成层级固定为 App < Browser < Overlay", () => {
  assert.doesNotMatch(windowManagerSource, /appLayer/u);
  assert.match(windowManagerSource, /contentView\.addChildView\(appView, 0\)/u);
  assert.doesNotMatch(windowManagerSource, /browserHostLayer|overlayLayer/u);
  assert.match(source, /contentRoot\.addChildView\(record\.view, 1\)/u);
  assert.match(overlayManagerSource, /contentRoot\.addChildView\(record\.view, 2\)/u);
  assert.equal(
    [...windowManagerSource.matchAll(/contentView\.addChildView\(/gu)].length,
    1,
    "窗口只在 createWindow 中固定挂载 App Renderer；Browser 与 Overlay 直接按需作为根节点兄弟视图挂载",
  );
  assert.doesNotMatch(windowManagerSource, /ensureNativeLayerOrder/u);
});

test("Renderer 只发布浏览器内容槽，不参与原生浏览器 Surface 的坐标计算", () => {
  assert.match(browserTabSource, /class="browser-surface-slot"/u);
  assert.match(browserTabSource, /class="browser-native-surface"/u);
  // 菜单可以观察右栏 DOM 以便在面板尺寸变化后重报自身 popupBounds；
  // 该观察器不能成为 Browser Surface 的几何来源。
  assert.doesNotMatch(browserTabSource, /browserSurfaceSlot|updateBrowserSlot|bindContentSurface/u);
  assert.doesNotMatch(browserTabSource, /transform:\s*scale\(|object-fit:\s*(fill|cover)|surfaceWidth|surfaceHeight/u);
});

test("右栏原生 Surface 使用 Renderer 上报的真实内容槽", () => {
  assert.match(rightPaneSource, /\.right-pane-tabbar\s*\{[\s\S]*?box-sizing:\s*border-box/u);
  assert.match(browserTabSource, /\.browser-toolbar\s*\{[\s\S]*?box-sizing:\s*border-box/u);
  assert.match(layoutSource, /browserContentSlot/u);
  assert.match(layoutSource, /renderer_geometry/u);
  assert.doesNotMatch(layoutSource, /rightPaneTabBarHeight|browserToolbarHeight/u);
});

test("原生 Overlay 使用 Renderer 提交的精确菜单矩形或浏览器内容槽", () => {
  assert.match(overlayManagerSource, /const frame = layout\.rendererGeometry;/u);
  // 菜单和标记都只能绑定 Renderer 已确认的矩形，Main 不推导任何浮层尺寸、
  // 锚点偏移或固定菜单高度。
  assert.match(overlayManagerSource, /if \(state\.kind === "menu"\) \{[\s\S]*?const bounds = state\.popupBounds;[\s\S]*?return \{ bounds: \{ \.\.\.bounds \} \};/u);
  assert.match(overlayManagerSource, /function isContainedByLayout\([\s\S]*?containsRectangle\(layout\.rightPaneBounds!, bounds\)/u);
  assert.match(overlayManagerSource, /if \(!state\.ownerId\.startsWith\("browser:"\)\) return null;/u);
  assert.match(overlayManagerSource, /const slot = frame\?\.browserContentSlot;[\s\S]*?slot\.tabId !== browserTabId[\s\S]*?bounds: Rectangle = \{ \.\.\.slot\.bounds \};/u);
  assert.match(overlayManagerSource, /export type DesktopOverlayKind = "menu" \| "annotation";/u);
  assert.match(overlayManagerSource, /export type DesktopOverlayPlacement =[\s\S]*?"right-pane-add"[\s\S]*?"browser-viewport"[\s\S]*?"browser-annotations";/u);
  assert.match(overlayManagerSource, /resolveCurrentOverlayGeometry\(record\.state, layout\)/u);
  assert.match(overlayManagerSource, /record\.geometryAvailable = false;[\s\S]*?clearOverlayBounds\(record\)/u);
  assert.match(overlayManagerSource, /record\.geometryAvailable = true;[\s\S]*?setOverlayBounds\(record, geometry\.bounds\)/u);
});

test("新增面板的外部点击不会关闭其他浏览器 Overlay", () => {
  const outsidePointer = section(rightPaneSource, "const handleOutsidePointer", "const handleEscape");
  assert.match(outsidePointer, /addPaneMenuOpen\s*&&/u);
  assert.match(outsidePointer, /addPaneMenuElement\s*&&/u);
});

test("右栏浮层按运行时收敛为唯一实现", () => {
  // Desktop 的原生 Chromium Surface 由兄弟视图承载，菜单必须走原生 Overlay；
  // Web 模式没有 Browser Surface，继续由同一组件的 DOM 菜单承载。
  assert.match(browserTabSource, /\{#if viewportMenuOpen && !desktopRuntime\}/u);
  assert.match(browserTabSource, /\{#if annotationMenuOpen && !desktopRuntime\}/u);
  assert.match(browserTabSource, /openDesktopViewportMenu\(\)/u);
  assert.match(browserTabSource, /openDesktopAnnotationHistory\(\)/u);
  assert.match(rightPaneSource, /\{#if addPaneMenuOpen && !desktopSurface\}/u);
  assert.match(rightPaneSource, /openDesktopAddPaneMenu\(\)/u);
  assert.match(
    browserTabSource,
    /\.viewport-popover,\s*\.annotation-history-popover \{[^}]*position:\s*absolute/u,
  );

  // 菜单不能撤下页面，否则就是用户看到的“像切换了页面”。阻塞层只保留给
  // 设置和确认框这类 App Renderer 全局弹层。
  assert.doesNotMatch(browserTabSource, /browser-menu:|setDesktopBlockingOverlay\(key, open\)/u);
  assert.doesNotMatch(rightPaneSource, /right-pane-add:|setDesktopBlockingOverlay\(key, addPaneMenuOpen\)/u);

  const addMenuIndex = rightPaneSource.indexOf('class="right-pane-add-menu-row"');
  const rightPaneContentIndex = rightPaneSource.indexOf('<!-- 当前 code tab 的副标题');
  assert.ok(addMenuIndex >= 0 && addMenuIndex < rightPaneContentIndex);
  assert.match(rightPaneSource, /\.right-pane-add-menu-row \{[^}]*position:\s*absolute/u);

  // 普通菜单的锚点几何与硬编码尺寸推导必须彻底移除，不留双实现。
  assert.doesNotMatch(overlayManagerSource, /overlayAnchors|widthLimit|itemHeight/u);
  assert.doesNotMatch(layoutSource, /overlayAnchors/u);
  assert.doesNotMatch(workbenchSource, /overlayAnchors|data-desktop-overlay-anchor/u);
  assert.doesNotMatch(browserTabSource, /data-desktop-overlay-anchor/u);
  assert.doesNotMatch(rightPaneSource, /data-desktop-overlay-anchor/u);
});

test("几何观察器只监听真实尺寸和结构变化", () => {
  assert.match(workbenchSource, /const observeGeometryTargets = \(\) =>/u);
  assert.match(workbenchSource, /geometryMutationObserver\.observe\(workbenchElement, \{\s*subtree:\s*true,\s*childList:\s*true,[\s\S]*?attributes:\s*true/u);
  assert.doesNotMatch(workbenchSource, /attributeFilter:\s*\['data-browser-tab-id'\]/u);
  assert.match(workbenchSource, /geometryMutationObserver\.observe\(workbenchElement, \{\s*subtree:\s*true,\s*childList:\s*true/u);
  assert.doesNotMatch(workbenchSource, /attributeFilter:\s*\['class', 'style'\]/u);
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

test("Surface 创建异步准备首个文档，工具调用才等待调试器握手", () => {
  const createSurface = section(source, "private createSurface(", "bindContentSurface(");
  assert.doesNotMatch(createSurface, /primeInitialDocument|enqueueCdp/u);
  assert.match(source, /private startDebuggerInitialization\([\s\S]*?await this\.attachDebugger\(record\);/u);
  assert.match(source, /private async waitForDebugger\(record: BrowserSurfaceRecord\)/u);
  assert.match(source, /const debuggerReady = record\.debuggerReadyPromise[\s\S]*?debuggerReadyPromise === debuggerReady[\s\S]*?= null/u);
  assert.match(source, /await this\.waitForDebugger\(record\)/u);
});

test("新建 Surface 的激活不等待调试器，且同一 Tab 的最后地址拥有导航权", () => {
  const materialize = section(source, "async materialize(", "private createSurface(");
  assert.doesNotMatch(materialize, /record\.debuggerReadyPromise\)\s*await/u);
  assert.match(materialize, /const load = this\.startLoad\(record, initialUrl\);[\s\S]*?else void load\.catch/u);
  const startLoad = section(source, "private startLoad(", "private publishNavigationFailure(");
  assert.match(startLoad, /this\.loadPage\(record, url\)/u);
  assert.match(startLoad, /record\.loadPromise[\s\S]*?sameNavigationUrl\(record\.navigationOperation\.targetUrl \?\? "about:blank", url\)/u);
  assert.match(materialize, /this\.startDebuggerInitialization\(record\)/u);
  assert.match(source, /startDebuggerInitialization\(record\)[\s\S]*?waitForCurrentNavigation\(record\)[\s\S]*?this\.attachDebugger\(record\)/u);
});

test("Browser Surface 导航不自动抢占 App Renderer 焦点", () => {
  const createSurface = section(source, "private createSurface(", "bindContentSurface(");
  assert.match(createSurface, /focusOnNavigation:\s*false/u);
  const focusApp = withoutLineComments(section(windowManagerSource, "focusApp(windowId: string)", "async setBrowserViewport("));
  assert.match(focusApp, /record\.window\.focus\(\)[\s\S]*?record\.appView\.webContents\.focus\(\)/u);
  assert.doesNotMatch(focusApp, /setImmediate|blurWindow|restoreWindow/u);
});

test("页面导航期间保持原生页面可见，只有渲染进程崩溃才恢复 Surface", () => {
  const surfaceSlot = section(source, "private applySlot(", "private async loadPage(");
  const navigationEvents = section(source, "webContents.on(\"did-start-navigation\"", "webContents.on(\"before-input-event\"");
  const crashEvents = section(source, "webContents.on(\"render-process-gone\"", "private async waitForDebugger(");
  assert.match(surfaceSlot, /record\.view\.setVisible\(true\)/u);
  assert.match(source, /private beginNavigationOperation[\s\S]*?loading_changed/u);
  assert.match(navigationEvents, /did-fail-load[\s\S]*?this\.failNavigationOperation\(/u);
  assert.match(source, /private failNavigationOperation[\s\S]*?this\.applySlot\(\s*record/u);
  assert.doesNotMatch(navigationEvents, /did-fail-load[\s\S]*?this\.unmountSurface\(record/u);
  assert.match(navigationEvents, /did-finish-load[\s\S]*?this\.completeWhenMainFrameSettled\(/u);
  assert.match(source, /private unmountSurface\([\s\S]*?只解绑原生 View，不关闭 WebContents/u);
  assert.match(crashEvents, /render-process-gone[\s\S]*?this\.invalidateAndRecover\(record/u);
  const recovery = section(source, "private invalidateAndRecover(", "private async recover(");
  assert.match(recovery, /this\.unmountSurface\(record[\s\S]*?this\.resetDebuggerSession\(record/u);
  assert.doesNotMatch(navigationEvents, /did-start-navigation[\s\S]*?setBounds\(\{ x: 0, y: 0, width: 0/u);
});

test("每个真实导航事务都会推进 navigationRevision，旧页面身份不可复用", () => {
  const navigation = section(source, "private beginNavigationOperation(", "private nextNavigationEventSequence");
  assert.match(
    navigation,
    /supersedeNavigationOperation\(previous\)[\s\S]*?record\.navigationRevision \+= 1;[\s\S]*?const id = \+\+record\.navigationOperationId/u,
  );
});

test("Surface binding 的导航代次只允许严格匹配或单向前进", () => {
  assert.equal(matchesNavigationRevision(1, 1, false), true);
  assert.equal(matchesNavigationRevision(1, 2, false), false);
  assert.equal(matchesNavigationRevision(2, 1, false), false);
  assert.equal(matchesNavigationRevision(1, 1, true), true);
  assert.equal(matchesNavigationRevision(1, 2, true), true);
  assert.equal(matchesNavigationRevision(2, 1, true), false);
});

test("CDP 命令发起和完成使用不同的导航代次校验语义", () => {
  const cdp = section(source, "async sendCdp(", "private enqueueCdp<T>(");
  const commandStart = cdp.slice(0, cdp.indexOf("const result = await this.sendSurfaceCdpCommand"));
  assert.match(commandStart, /const contents = this\.recordForBinding\(binding\)/u);
  assert.doesNotMatch(commandStart, /this\.recordForBinding\(binding,\s*\{/u);
  assert.match(cdp, /this\.assertRenderableBinding\(binding, \{\s*allowNavigationAdvance: options\.allowNavigationAdvance/u);
  const queuedCommand = cdp.slice(cdp.indexOf("this.enqueueCdp(record"));
  assert.match(queuedCommand, /this\.recordForBinding\(binding\)/u);
});

test("导航响应只等待主文档生命周期，不把 loadURL Promise 作为第二个完成条件", () => {
  const loadPage = section(source, "private async loadPage(", "private startLoad(");
  assert.doesNotMatch(loadPage, /Promise\.all\(/u);
  assert.match(loadPage, /void loadPromise\.then\(/u);
  assert.match(loadPage, /withNavigationTimeout\(navigationWait/u);
  assert.doesNotMatch(loadPage, /await this\.installDialogBridgeInCurrentDocument/u);
});

test("导航完成后读取新的文档代次，不把成功导航误判为旧 Surface 失效", () => {
  const navigate = section(source, "async navigate(", "async setViewport(");
  assert.match(
    navigate,
    /const finalRecord = this\.assertRenderableBinding\(binding, \{ allowNavigationAdvance: true \}\)/u,
  );
  assert.match(navigate, /return this\.pageState\(finalRecord\)/u);
});

test("导航请求不等待同一 Tab 的标记投影，页面事件负责异步重放", () => {
  const navigate = section(browserRoutesSource, "async fn navigate_tab(", "fn validate_navigation_url(");
  assert.doesNotMatch(navigate, /sync_browser_annotations_to_host/u);
  assert.match(
    browserHostSource,
    /BrowserHostEvent::PageUpdated[\s\S]*?schedule_browser_annotation_sync\(state, &binding\.tab_id\)/u,
  );
});

test("Surface 布局和加载回调不隐式抢占 App Renderer 焦点", () => {
  const applySlot = section(source, "private applySlot(", "private async loadPage(");
  assert.doesNotMatch(applySlot, /contents\.focus\(\)/u);
  assert.match(source, /focusOnNavigation:\s*false/u);
  assert.doesNotMatch(windowManagerSource, /focusApp\([\s\S]*?this\.#surfaceManager\.(?:blurWindow|restoreWindow)\(windowId\)/u);
  const focusApp = withoutLineComments(section(windowManagerSource, "focusApp(windowId: string)", "async setBrowserViewport("));
  assert.doesNotMatch(focusApp, /setImmediate/u);
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

test("debugger 初始化失败会清理半连接状态并按单飞链路退避重试", () => {
  const attach = section(source, "private async attachDebugger(", "private assertDebuggerSessionCurrent(");
  assert.match(attach, /this\.resetDebuggerSession\(record\)/u);
  assert.match(attach, /if \(!record\.contents\.isDestroyed\(\) && debuggerApi\.isAttached\(\)\)/u);
  assert.match(attach, /debuggerApi\.detach\(\)/u);

  const reconnect = section(source, "private reconnectDebugger(", "private scheduleDebuggerReconnect(");
  assert.match(reconnect, /if \(record\.debuggerReadyPromise\) return record\.debuggerReadyPromise/u);
  assert.match(reconnect, /const reconnect = this\.enqueueCdp\(record/u);
  assert.match(reconnect, /this\.scheduleDebuggerReconnect\(/u);

  const retry = section(source, "private scheduleDebuggerReconnect(", "private clearDebuggerReconnectTimer(");
  assert.match(retry, /record\.debuggerReconnectTimer/u);
  assert.match(retry, /DEBUGGER_RECONNECT_MAX_DELAY_MS/u);
  assert.match(retry, /void this\.reconnectDebugger\(record/u);
});

test("导航刷新和 debugger detach 不共享恢复 Promise，且旧 session 不能污染新 attach", () => {
  const detach = section(source, 'debuggerApi.on("detach"', "private reconnectDebugger(");
  assert.doesNotMatch(detach, /record\.debuggerReadyPromise\s*=\s*record\.recoveryPromise/u);
  assert.match(source, /debuggerApi\.on\("detach", detachListener\)[\s\S]*?this\.resetDebuggerSession\(record\)/u);
  assert.match(source, /debuggerApi\.on\("detach", detachListener\)[\s\S]*?void this\.reconnectDebugger\(record/u);

  const recovery = section(source, "private invalidateAndRecover(", "private async recover(");
  assert.match(recovery, /this\.resetDebuggerSession\(record\)/u);
  assert.match(recovery, /record\.recoveryPromise = recovery/u);
  assert.doesNotMatch(recovery, /record\.debuggerReadyPromise\s*=\s*recovery/u);

  const sessionGuard = section(source, "private assertDebuggerSessionCurrent(", "private installDebuggerListeners(");
  assert.match(sessionGuard, /record\.debuggerSessionGeneration !== generation/u);
  assert.match(sessionGuard, /!record\.contents\.debugger\.isAttached\(\)/u);

  const reset = section(source, "private resetDebuggerSession(", "private async installDialogBridgeInCurrentDocument(");
  assert.match(reset, /this\.stopInspectForLifecycle\(record, "debugger-detached"\);/u);
  assert.doesNotMatch(reset, /stopInspect\?:|stopInspect\s*===\s*false/u);
});

test("同一窗口的 Browser Tab 只复用相同 browser session 的 WebContents", () => {
  const materialize = section(source, "async materialize(", "private createSurface(");
  assert.match(materialize, /record\.browserSessionId !== input\.browserSessionId/u);
  assert.match(materialize, /this\.closeRecord\(record, false\)/u);
  assert.match(materialize, /record = null/u);
  const close = section(source, "private closeRecord(", "private removeRecordIndexes(");
  assert.match(close, /this\.resetDebuggerSession\(record\)/u);
  assert.match(close, /if \(!record\.contents\.isDestroyed\(\)\) record\.contents\.close\(\)/u);
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
  const binding = section(source, "bindContentSurface(", "bindingForTabInWindow(");
  assert.match(binding, /const target = records\.find\(\(record\) => \([\s\S]*?record\.tabId === tabId && record\.surfaceId === surfaceId[\s\S]*?\) \?\? null/u);
  assert.match(binding, /this\.applySlot\(target, bounds, window\);/u);
  assert.match(binding, /for \(const record of records\) \{[\s\S]*?if \(record !== target\) this\.unmountSurface\(record, window\);/u);
  assert.ok(
    binding.indexOf("this.applySlot(target, bounds, window);")
      < binding.lastIndexOf("if (record !== target) this.unmountSurface(record, window);"),
    "必须先挂载目标 Surface，再卸载旧 Surface，避免切换 Browser Tab 时出现空槽",
  );
  assert.match(source, /private unmountSurface\([\s\S]*?record\.view\.setVisible\(false\)/u);
  assert.match(source, /private detachSurface\([\s\S]*?this\.#contentRoots\.get\(record\.windowId\)\?\.removeChildView\(record\.view\)/u);
  const activateBrowser = section(windowManagerSource, "async activateBrowser(", "activatePanel(");
  assert.doesNotMatch(activateBrowser, /bindContentSurface\(input\.windowId, "", null\)/u);
  assert.match(activateBrowser, /await this\.#surfaceManager\.materialize\([\s\S]*?record\.layout = reduceWindowLayout\([\s\S]*?type: "active_panel"/u);
});

test("Surface 卸载会使内容槽租约失效，迟到的导航回调不得重新挂载", () => {
  const detach = section(source, "private detachSurface(", "private isRenderable(");
  assert.match(detach, /record\.slotVisible = false[\s\S]*?record\.slotBounds = null/u);
  assert.match(detach, /this\.stopInspectForLifecycle\(record, "surface-unmounted"\)/u);
  const emptySlot = section(source, "private applySlot(", "private async loadPage(");
  assert.match(emptySlot, /if \(!effectiveBounds\) \{[\s\S]*?this\.detachSurface\(record\)[\s\S]*?return;/u);
  const lateNavigation = section(source, "private completeNavigationOperation(", "private completeWhenMainFrameSettled(");
  assert.match(lateNavigation, /record\.slotVisible \? record\.slotBounds : null/u);
});

test("内容槽绑定必须同时匹配窗口、Tab 和目标 Surface 身份", () => {
  const binding = section(source, "bindContentSurface(", "bindingForTabInWindow(");
  assert.match(binding, /surfaceId: string \| null/u);
  assert.match(binding, /record\.tabId === tabId && record\.surfaceId === surfaceId/u);
  const applyLayout = section(windowManagerSource, "private applyLayout(", "private reconcileActiveBrowserSurface(");
  assert.match(applyLayout, /layout\.activeTabId,[\s\S]*?layout\.activeSurfaceId,[\s\S]*?currentBrowserContentBounds/u);
});

test("activeSurfaceId 失效时布局立即清除物理 Surface 身份", () => {
  const reconcile = section(windowManagerSource, "private reconcileActiveBrowserSurface(", "private waitForBrowserSurface(");
  assert.match(reconcile, /bindingForSurface\(layout\.activeSurfaceId\)/u);
  assert.match(reconcile, /binding\.window_id === record\.windowId/u);
  assert.match(reconcile, /binding\.tab_id === layout\.activeTabId/u);
  assert.match(reconcile, /type: "active_panel"[\s\S]*?surfaceId: null/u);
  assert.match(windowManagerSource, /snapshot\(windowId: string\)[\s\S]*?reconcileActiveBrowserSurface\(record\)/u);
});

test("布局 revision 过期时同一 Browser Tab 保留最后确认的原生 Surface", () => {
  const applyLayout = section(windowManagerSource, "private applyLayout(", "private reconcileActiveBrowserSurface(");
  assert.match(applyLayout, /const rendererGeometryForActiveSurface = browserSurfaceActive[\s\S]*?browserContentSlot\?\.tabId === layout\.activeTabId/u);
  assert.match(applyLayout, /browserSurfaceActive && rendererGeometryForActiveSurface\s*\?/u);
  assert.match(applyLayout, /layout\.activeSurfaceId,[\s\S]*?currentBrowserContentBounds,[\s\S]*?currentBrowserParentBounds/u);
});

test("Surface 崩溃恢复只重挂载崩溃前仍有效的内容槽租约", () => {
  assert.match(source, /slotLeaseRevision: number/u);
  assert.match(source, /recoverySlot: \{ bounds: Rectangle; leaseRevision: number \} \| null/u);
  const detach = section(source, "private detachSurface(", "private isRenderable(");
  assert.match(detach, /slotLeaseRevision \+= 1/u);
  const recovery = section(source, "private invalidateAndRecover(", "private async recover(");
  assert.match(recovery, /const previousSlot = record\.slotVisible && record\.slotBounds/u);
  assert.match(recovery, /record\.recoverySlot = previousSlot[\s\S]*?leaseRevision: record\.slotLeaseRevision/u);
  const recover = section(source, "private async recover(", "private async applyViewport(");
  assert.match(recover, /const recoverySlot = record\.recoverySlot[\s\S]*?recoverySlot\.leaseRevision === record\.slotLeaseRevision[\s\S]*?this\.applySlot\(record, recoverySlot\.bounds/u);
});

test("加载期 viewport commit 由真实文档完成事件重新唤醒", () => {
  const finish = section(source, 'webContents.on("did-finish-load"', 'webContents.on("page-title-updated"');
  const stop = section(source, 'webContents.on("did-stop-loading"', 'webContents.on("did-fail-load"');
  assert.match(finish, /this\.scheduleViewportCommit\(record\)/u);
  assert.match(stop, /this\.scheduleViewportCommit\(record\)/u);
  const navigationComplete = section(source, "private completeNavigationOperation(", "private completeWhenMainFrameSettled(");
  assert.match(navigationComplete, /this\.#onDocumentReady\?\.\(this\.binding\(record\)\)/u);
  const flush = section(source, "private async flushViewportCommits(", "private isViewportCommitInputCurrent(");
  assert.match(flush, /record\.priming \|\| record\.contents\.isLoadingMainFrame\(\)/u);
  assert.match(source, /private async waitForViewportCommit\([\s\S]*?commit\.promise[\s\S]*?commit\.state === "ready"/u);
  assert.doesNotMatch(withoutLineComments(flush), /executeJavaScript|requestAnimationFrame|browser_compositor_ready/u);
});

test("debugger 重连定时器只清理当前持有的句柄一次", () => {
  const cleanup = section(source, "private clearDebuggerReconnectTimer(", "private invalidateAndRecover(");
  assert.match(cleanup, /if \(!record\.debuggerReconnectTimer\) return;/u);
  assert.match(cleanup, /clearTimeout\(record\.debuggerReconnectTimer\);[\s\S]*?record\.debuggerReconnectTimer = null;/u);
  assert.equal((cleanup.match(/clearTimeout\(/gu) ?? []).length, 1);
});

test("CDP 和 DOM await 返回后必须重新确认当前可渲染内容槽", () => {
  const cdp = section(source, "async sendCdp(", "private enqueueCdp<T>(");
  assert.match(cdp, /await (?:this\.waitForViewportCommit\(record\)|withOptionalAbort\(this\.waitForViewportCommit\(record\), options\.signal\))[\s\S]*?this\.assertRenderableBinding\(binding, options\)/u);
  assert.match(cdp, /const lease = this\.createDebuggerSessionLease\(record, sessionId\)[\s\S]*?const result = await this\.sendSurfaceCdpCommand\([\s\S]*?this\.assertRenderableBinding\(binding, options\)/u);
  assert.doesNotMatch(cdp, /sendCdpCommandWithTimeout|waitForViewportApply/u);
  const cdpCommand = section(source, "private async sendSurfaceCdpCommand(", "private async waitForScreenshotReadiness(");
  assert.match(cdpCommand, /lease\.assertCurrent\(\)[\s\S]*?const result = await sendCdpCommandWithTimeout\([\s\S]*?lease\.assertCurrent\(\)/u);
  const inspect = section(source, "private isInspectGenerationActive(", "private stopInspectForLifecycle(");
  assert.match(inspect, /this\.isRenderable\(record\)/u);
  const inspectDom = section(source, "private handleInspectPointRequested(", "private async readInspectNodeText(");
  assert.match(inspectDom, /await this\.sendSurfaceCdpCommand\([\s\S]*?if \(!this\.isInspectGenerationActive\(record, generation\)\) return;/u);
  const cursor = section(source, "private async setAgentCursor(", "private promote(");
  assert.match(cursor, /await this\.sendSurfaceCdpCommand\([\s\S]*?if \(!this\.isRenderable\(record\)\) return;/u);
  assert.match(cursor, /await new Promise<void>\([\s\S]*?if \(!this\.isRenderable\(record\)\) return;/u);
});

test("Surface 激活使用代次，过期请求不能抢占当前槽", () => {
  assert.match(source, /#activationGenerations/u);
  assert.match(source, /setActivationGeneration\(windowId: string, generation: number\)/u);
  assert.match(source, /assertActivationCurrent\(input\.windowId, input\.activationGeneration\)/u);
  assert.match(source, /browser_surface_activation_stale/u);
  assert.match(source, /created[\s\S]*?closeRecord\(record, false\)/u);
});

test("viewport 的逻辑尺寸与原生内容槽统一，右栏尺寸会触发 Chromium 原生响应式重排", () => {
  assert.match(source, /Emulation\.clearDeviceMetricsOverride/u);
  assert.match(source, /Emulation\.setDeviceMetricsOverride/u);
  const applySlot = section(source, "private applySlot(", "private async loadPage(");
  assert.match(applySlot, /内容槽只管理原生 View 的物理承载范围/u);
  assert.match(applySlot, /不可验证的中间帧必须隐藏[\s\S]*?退出命中树/u);
  assert.match(source, /private unmountSurface\([\s\S]*?只解绑原生 View，不关闭 WebContents/u);
  assert.doesNotMatch(applySlot, /leaseBounds|clipBounds|intersectRect|x:\s*\(leaseBounds/u);
  assert.match(applySlot, /this\.scheduleViewportCommit\(record\)/u);
  assert.doesNotMatch(applySlot, /scheduleViewportApply|viewportApplied|sizeChanged/u);
  const viewportMethods = section(source, "private async applyViewport(", "private async setAgentCursor(");
  assert.match(viewportMethods, /commit\.bounds/u);
  assert.match(viewportMethods, /nativeViewportScale\(/u);
  assert.match(viewportMethods, /scale,/u);
  const autoViewportMethods = viewportMethods.slice(0, viewportMethods.indexOf("const viewport"));
  assert.match(autoViewportMethods, /Emulation\.clearDeviceMetricsOverride/u);
  assert.doesNotMatch(autoViewportMethods, /Emulation\.setDeviceMetricsOverride/u);
  assert.doesNotMatch(autoViewportMethods, /scale: 1/u);
  const cursorMethods = section(source, "private initialAgentCursorPosition(", "async closeTab(");
  assert.match(cursorMethods, /return null/u);
  assert.doesNotMatch(cursorMethods, /record\.viewport\.width|record\.viewport\.height|640|480/u);
  assert.match(
    source,
    /const ALLOWED_WORKER_CDP_METHODS = new Set\(\[[\s\S]*?"Emulation\.setTouchEmulationEnabled"/u,
  );
  assert.doesNotMatch(viewportMethods, /capturePage\(|startScreencast|drawImage\(/u);
  assert.match(source, /navigationGeneration === record\.navigationGeneration/u);
  assert.match(source, /sameLogicalViewport\(current\.viewport, record\.viewport\)/u);
  assert.match(source, /debuggerSessionGeneration === commit\.debuggerSessionGeneration/u);
  assert.match(browserTabSource, /VIEWPORT_DEVICE_MODES = \[[\s\S]*?id: 'wide'[\s\S]*?id: 'narrow'/u);
  assert.match(browserTabSource, /scheduleCustomViewportUpdate\(\)/u);
  assert.match(browserTabSource, /viewport: mode === 'auto'\s*\n?\s*\? \{ mode: 'auto' \}/u);
});

test("截图统一通过同一 WebContents 的 Chromium CDP 页面捕获，root 不依赖 DOM ref", () => {
  assert.match(source, /method === "Page\.captureScreenshot"/u);
  assert.match(source, /private async sendSurfaceCdpCommand\([\s\S]*?sendCdpCommandWithTimeout\([\s\S]*?method/u);
  assert.doesNotMatch(source, /waitForViewportApply/u);
  assert.match(source, /method === "Page\.captureScreenshot"[\s\S]*?fromSurface: true/u);
  assert.doesNotMatch(source, /capturePage\(|capturePageRect|mapBrowserCaptureClipToNativeRect/u);
  assert.match(source, /private enqueueCdp[\s\S]*?record\.cdpLane/u);
  assert.match(browserRuntimeSource, /fromSurface: true/u);
  assert.doesNotMatch(source, /startScreencast|drawImage\(/u);
  assert.match(browserRuntimeSource, /element_ref !== "root"[\s\S]*?else if \(input\.clip\)/u);
  assert.match(browserRuntimeSource, /input\.clip\.width \* viewport\.width/u);
});

test("后台 Browser Surface 必须先恢复到真实内容槽，不能对 0x0 WebContents 做自动化", () => {
  const sendCdp = section(source, "async sendCdp(", "private enqueueCdp<T>(");
  assert.match(sendCdp, /所有需要页面 viewport 的自动化都必须绑定当前右栏内容槽/u);
  assert.match(sendCdp, /!this\.isPrimary\(binding\) \|\| !this\.isRenderable\(record\)/u);
  assert.match(source, /activationInputForTab\(tabId: string\)/u);
  assert.match(source, /isRenderableBinding\(binding: BrowserSurfaceBinding\)/u);
});

test("Worker/daemon 重连后重新执行当前布局事务以恢复 Browser Surface 内容槽", () => {
  const restore = section(windowManagerSource, "async restoreAfterDaemonReady(): Promise<void>", "activeWindowId(): string");
  assert.match(restore, /for \(const record of \[\.\.\.this\.#records\.values\(\)\]\)[\s\S]*?this\.applyLayout\(record\)/u);
  assert.match(restore, /未确认的几何仍保持不可挂载/u);
});

test("桌面 Overlay IPC 只接收显式复制后的纯数据状态和身份", () => {
  assert.match(rightPaneSource, /toDesktopOverlayState\(state\)/u);
  assert.match(rightPaneSource, /const state = toDesktopOverlayState\(\{ \.\.\.layout\.state, popupBounds \}\)/u);
  assert.match(rightPaneSource, /desktop\.closeOverlay\(toDesktopOverlayIdentity\(identity\)\)/u);
  assert.match(browserTabSource, /const safeState = toDesktopOverlayState\(state\)/u);
  assert.match(browserTabSource, /desktop\.openOverlay\(safeState\)/u);
  assert.match(browserTabSource, /const state = toDesktopOverlayState\(\{ \.\.\.layout\.state, popupBounds \}\)/u);
  assert.match(browserTabSource, /desktop\.closeOverlay\(toDesktopOverlayIdentity\(pending\.identity\)\)/u);
});

test("WebContents 销毁时必须清理 Surface 挂载和注册索引", () => {
  const destroyed = section(source, 'contents.once("destroyed"', "this.installSurfacePolicy(record)");
  assert.match(destroyed, /this\.closeRecord\(record\)/u);
  const closeRecord = section(source, "private closeRecord(", "private removeRecordIndexes(");
  assert.match(closeRecord, /record\.lifecycleEpoch \+= 1/u);
  assert.match(closeRecord, /record\.lifecycleAbort\.abort\(\)/u);
  assert.match(closeRecord, /rejectNavigationWaiters\(record/u);
  assert.match(closeRecord, /this\.detachSurface\(record, window\)/u);
  assert.match(closeRecord, /if \(!record\.contents\.isDestroyed\(\)\) record\.contents\.close\(\)/u);
});

test("后台截图复用同一个 WebContents，不创建隐藏窗口或临时挂载 Surface", () => {
  assert.match(source, /Page\.captureScreenshot[\s\S]*?this\.enqueueCdp\(record[\s\S]*?sendSurfaceCdpCommand/u);
  assert.doesNotMatch(source, /captureWindow/u);
});

test("所有工具截图都从真实 WebContents 读取，并通过同一个 Surface CDP lane 串行化", () => {
  const cdpSection = section(source, "async sendCdp(", "private enqueueCdp<T>(");
  assert.match(cdpSection, /await (?:this\.waitForViewportCommit\(record\)|withOptionalAbort\(this\.waitForViewportCommit\(record\), options\.signal\))[\s\S]*?method === "Page\.captureScreenshot"[\s\S]*?waitForScreenshotReadiness\(record\)/u);
  assert.doesNotMatch(cdpSection, /waitForViewportApply/u);
  assert.match(cdpSection, /method === "Page\.captureScreenshot"[\s\S]*?this\.enqueueCdp\(record[\s\S]*?sendSurfaceCdpCommand/u);
  assert.match(cdpSection, /this\.enqueueCdp\(record/u);
  assert.match(source, /private async waitForScreenshotReadiness\(record: BrowserSurfaceRecord\)/u);
  assert.doesNotMatch(source, /screenshotLane|screenshotInFlight|enqueueScreenshot|flushDeferredContentBinding/u);
});

test("截图不锁定右栏原生布局，页面命令统一通过 CDP lane 收口", () => {
  const applySlot = section(source, "private applySlot(", "private async loadPage(");
  assert.doesNotMatch(applySlot, /screenshotInFlight/u);
  const bind = section(source, "bindContentSurface(", "bindingForTabInWindow(");
  assert.doesNotMatch(bind, /screenshotInFlight|deferredContentBindings/u);
  assert.doesNotMatch(source, /waitForScreenshotIdle|deferredContentBindings/u);
});

test("输入动作允许同一 WebContents 在动作内部推进导航 revision", () => {
  assert.match(source, /recordForBinding\([\s\S]*?allowNavigationAdvance\?: boolean/u);
  assert.match(source, /allowNavigationAdvance: options\.allowNavigationAdvance === true \|\| method\.startsWith\("Input\."\)/u);
  assert.match(desktopControlSource, /交互命令在 Worker 内可能由多个 CDP 输入事件组成/u);
});

test("Lighthouse CDP 会话允许导航推进但不放宽 Surface 身份校验", () => {
  assert.match(source, /options: \{ allowNavigationAdvance\?: boolean \} = \{\}/u);
  assert.match(workerSource, /allowNavigationAdvance: message\.allow_navigation_advance === true/u);
  assert.match(browserRuntimeSource, /allowNavigationAdvance: true/u);
  assert.match(browserRuntimeSource, /this\.#cdp\.send\([\s\S]*?\{ allowNavigationAdvance: true \}/u);
  assert.match(readFileSync(new URL("../../../../contracts/desktop-browser/src/index.ts", import.meta.url), "utf8"), /allow_navigation_advance\?: boolean/u);
});

test("交互命令完成后由 Main 返回当前页面状态契约", () => {
  assert.match(desktopControlSource, /isPageStateInteraction\(command\)/u);
  assert.match(desktopControlSource, /currentBinding\.navigation_revision/u);
  assert.match(desktopControlSource, /type: "page_state"/u);
});

test("render-process-gone 才触发页面崩溃恢复", () => {
  const renderProcessGone = section(source, 'webContents.on("render-process-gone"', "private async waitForDebugger(");
  assert.match(renderProcessGone, /type: "page_crashed"/u);
  assert.match(renderProcessGone, /this\.invalidateAndRecover\(record/u);
  const recovery = section(source, "private invalidateAndRecover(", "private async recover(");
  assert.match(recovery, /this\.unmountSurface\(record/u);
  assert.doesNotMatch(renderProcessGone, /debugger-detached/u);
});

test("标记 Overlay 只覆盖当前浏览器内容槽并保持菜单层级", () => {
  assert.match(overlayManagerSource, /const frame = layout\.rendererGeometry[\s\S]*?const slot = frame\?\.browserContentSlot/u);
  assert.match(overlayManagerSource, /const browserTabId = state\.ownerId\.slice\("browser:"\.length\);[\s\S]*?slot\.tabId !== browserTabId/u);
  assert.match(overlayManagerSource, /resolveCurrentOverlayGeometry\(record\.state, layout\)/u);
  assert.match(overlayManagerSource, /private setOverlayBounds\([\s\S]*?record\.view\.setBounds\(bounds\)/u);
  assert.match(overlayManagerSource, /contentRoot\.addChildView\(record\.view, 2\)/u);
  assert.match(overlayManagerSource, /TRANSPARENT_VIEW_BACKGROUND/u);
  assert.match(browserTabSource, /openAnnotationCommentOverlay\(\)/u);
});

test("Overlay Manager 只分发动作，业务 Renderer 完成处理后再关闭", () => {
  const handleAction = section(overlayManagerSource, "handleAction(windowId", "isWebContents(");
  assert.match(handleAction, /this\.#onAction\(windowId, action\);/u);
  assert.doesNotMatch(handleAction, /this\.close\(windowId\)/u);
  assert.match(browserTabSource, /class="viewport-menu"[\s\S]*?role="menuitem"/u);
  assert.match(browserTabSource, /class="annotation-menu"[\s\S]*?role="menu"/u);
  assert.match(browserTabSource, /updateLogicalViewport\('auto'\)/u);
  assert.match(rightPaneSource, /class="right-pane-add-menu"[\s\S]*?chooseAddPane\(item\.kind\)/u);
  // 原生动作通道只保留标记流程；视口切换回归 DOM 事件处理。
  const annotationAction = section(browserTabSource, "if (action.kind === 'annotation')", "unsubscribeOverlayState");
  assert.match(annotationAction, /action\.id === 'selection'[\s\S]*?action\.id === 'save'[\s\S]*?action\.id === 'cancel'/u);
  assert.match(browserTabSource, /action\.kind === 'annotation' && action\.id === 'comment'/u);
  assert.doesNotMatch(browserTabSource, /action\.kind === '(viewport|annotation-history)'/u);
  assert.match(browserTabSource, /useAutomaticViewport\(\)/u);
  assert.match(browserTabSource, /useFixedViewport\(/u);
});

test("原生浏览器页面接管时关闭同一 Browser Tab 的菜单 Overlay", () => {
  assert.match(overlayManagerSource, /closeBrowserOverlay\(windowId: string, tabId: string\)/u);
  assert.match(overlayManagerSource, /record\.state\?\.ownerId !== `browser:\$\{tabId\}`/u);
  assert.match(windowManagerSource, /closeBrowserOverlay\(windowId: string, tabId: string\)[\s\S]*?applyLayout\(record\)/u);
  assert.match(indexSource, /event\.type === "user_takeover"[\s\S]*?closeOverlay\(event\.binding\.window_id\)/u);
});

test("关闭 Overlay 的 IPC 必须返回主进程确认事件", () => {
  const closeOverlayHandler = section(
    indexSource,
    'handleIpc("magi-desktop:close-overlay"',
    'handleIpc("magi-desktop:set-blocking-overlay"',
  );
  assert.match(
    closeOverlayHandler,
    /return manager\.closeOverlay\(windowId, parseOverlayCloseRequest\(value\)\);/u,
    "Renderer 必须收到 DesktopOverlayClosedEvent，才能结束关闭事务并释放交互状态",
  );
});

test("右栏非浏览器面板不依赖 Browser Surface", () => {
  assert.doesNotMatch(rightPaneSource, /activatePanel\(|activateBrowser\(/u);
  assert.doesNotMatch(browserTabSource, /activateBrowser\(/u);
  assert.match(workbenchSource, /currentDesktopPanelTarget\(\)/u);
  assert.match(workbenchSource, /desktopPanelActivationRequest[\s\S]*?activateDesktopPanelTarget\(/u);
  assert.match(workbenchSource, /desktop\.activateBrowser\([\s\S]*?desktop\.activatePanel\(/u);
  assert.match(rightPaneSource, /kind === 'browser'|kind: 'browser'/u);
  assert.match(windowManagerSource, /activatePanel\(windowId: string, kind: PanelKind/u);
  assert.match(layoutSource, /activeSurfaceId: null/u);
  assert.match(windowManagerSource, /const browserSurfaceActive = !record\.blockingOverlayActive/u);
});

test("浏览器动作沿现有统一工具和单 Tab 队列执行", () => {
  assert.match(source, /private primaryBindingForTab|primaryBindingForTab\(/u);
  assert.match(desktopControlSource, /resourceKey: commandTabId\(request\.command\)/u);
  assert.match(desktopControlSource, /private pumpQueue\(resourceKey: string, queue: ResourceQueue\)/u);
  assert.match(desktopControlSource, /private releaseConnection\(connection: DesktopControlConnection\)/u);
  assert.match(indexSource, /registerDesktopBrowserConnection\(\)/u);
  const workerReadyIndex = indexSource.indexOf("await worker.start()");
  const controlStartIndex = indexSource.indexOf("await control.start()");
  const daemonStartIndex = indexSource.indexOf("await processSupervisor.start()");
  assert.ok(
    workerReadyIndex >= 0 && controlStartIndex >= 0 && daemonStartIndex >= 0
      && workerReadyIndex < controlStartIndex && controlStartIndex < daemonStartIndex,
    "daemon 注册 Desktop Control 前必须完成 Worker 握手和 Host 监听，ready 事件不得携带空 worker_epoch",
  );
  assert.match(rightPaneSource, /loadBrowserAuthoritySession\(/u);
});

test("JavaScript 对话框处理命令绕过输入事件队列，避免点击与授权互相等待", () => {
  const cdpSection = section(source, "async sendCdp(", "private enqueueCdp<T>(");
  assert.match(cdpSection, /const isDialogCommand = method === "Page\.handleJavaScriptDialog"/u);
  assert.match(cdpSection, /if \(!isDialogCommand\) \{[\s\S]*?await (?:this\.waitForDebugger\(record\)|withOptionalAbort\(this\.waitForDebugger\(record\), options\.signal\))/u);
  assert.match(cdpSection, /if \(isDialogCommand\) \{[\s\S]*?sendSurfaceCdpCommand\(/u);
  assert.match(cdpSection, /JavaScript 对话框会阻塞触发它的 Input\.dispatchMouseEvent/u);
  assert.match(source, /Runtime\.addBinding/u);
  assert.match(source, /Page\.addScriptToEvaluateOnNewDocument/u);
  assert.match(source, /executeJavaScript\(DIALOG_BRIDGE_SCRIPT, true\)/u);
  assert.match(source, /private async installDialogBridgeInCurrentDocument/u);
  assert.match(source, /__magiBrowserDialogResolve/u);
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
  assert.doesNotMatch(windowManagerSource, /appLayer/u);
  assert.doesNotMatch(windowManagerSource, /browserHostLayer|overlayLayer/u);
  assert.match(windowManagerSource, /window\.contentView\.addChildView\(appView, 0\)/u);
  assert.match(source, /contentRoot\.addChildView\(record\.view, 1\)/u);
  assert.match(overlayManagerSource, /contentRoot\.addChildView\(record\.view, 2\)/u);
  assert.doesNotMatch(windowManagerSource, /private ensureNativeLayerOrder|ensureNativeLayerOrder/u);
  const applyLayout = section(windowManagerSource, "private applyLayout(", "private clearRendererGeometry(");
  assert.doesNotMatch(
    applyLayout,
    /setViewBounds\(record\.overlayLayer/u,
    "OverlayManager 必须独占 Overlay WebContentsView 的 bounds，布局事务不得把它恢复为整窗",
  );
  assert.match(windowManagerSource, /closeOverlay\([\s\S]*?record\.appView\.webContents\.focus\(\)/u);
  assert.match(windowManagerSource, /closeOverlay\([\s\S]*?this\.applyLayout\(record\)/u);
});

test("BaseWindow 根 contentView 与窗口内容区保持同一真实尺寸", () => {
  assert.match(
    windowManagerSource,
    /const contentBounds = window\.getContentBounds\(\)[\s\S]*?setViewBounds\(window\.contentView,[\s\S]*?width: contentBounds\.width,[\s\S]*?height: contentBounds\.height/u,
  );
  const updateBounds = section(windowManagerSource, "const updateBounds = () =>", "window.on(\"resize\"");
  assert.match(updateBounds, /const bounds = window\.getContentBounds\(\)[\s\S]*?setViewBounds\(window\.contentView,[\s\S]*?width: bounds\.width,[\s\S]*?height: bounds\.height/u);
  const ready = section(windowManagerSource, "handleRightPaneReady(windowId: string)", "setRendererContext(");
  assert.match(ready, /setViewBounds\(record\.window\.contentView, record\.layout\.clientBounds\)/u);
});

test("清理浏览数据等待每个活动页面完成刷新", () => {
  const clearData = section(source, "async clearBrowsingData(): Promise<void>", "private surfaceForTab(");
  assert.match(clearData, /await this\.clearDownloads\(\)/u);
  assert.match(clearData, /clearCache\(\)/u);
  assert.match(clearData, /clearStorageData\(\)/u);
  assert.match(clearData, /await Promise\.all\(\[\.\.\.this\.#surfaces\.values\(\)\]/u);
  assert.match(clearData, /this\.runNavigationAction\([\s\S]*?record\.contents\.reloadIgnoringCache\(\)/u);
});

test("下载生命周期只清理 Magi 私有目录，并覆盖启动、退出和活动下载", () => {
  assert.match(source, /browserDownloadRoot\(this\.#downloadUserDataPath\)/u);
  assert.match(source, /#downloadCleanupInProgress/u);
  assert.match(source, /this\.cancelActiveDownloads\(\)/u);
  assert.match(source, /item\.cancel\(\)/u);
  assert.match(indexSource, /const surfaces = new BrowserSurfaceManager\([\s\S]*?await surfaces\.clearDownloads\(\)[\s\S]*?browser-uploads/u);
  const shutdown = section(indexSource, "async function shutdown(): Promise<void>", "async function registerDesktopBrowserConnection");
  assert.match(shutdown, /surfaceManager\?\.closeAll\(\)[\s\S]*?automationWorker\?\.stop\(\)[\s\S]*?await surfaceManager\?\.clearDownloads\(\)/u);
  assert.match(indexSource, /const browserUploadRoot = join\(app\.getPath\("userData"\), "browser-uploads"\)/u);
});

test("节点检查使用真实鼠标坐标、Chromium DOM 命中和原生高亮", () => {
  assert.match(source, /export interface BrowserInspectedNodeContext/u);
  assert.match(source, /async startInspect\(binding: BrowserSurfaceBinding\)/u);
  assert.match(source, /async stopInspect\(binding: BrowserSurfaceBinding\)/u);
  assert.match(source, /Overlay\.enable/u);
  assert.match(source, /DOM\.enable[\s\S]*?Overlay\.enable[\s\S]*?Overlay\.hideHighlight/u);
  assert.match(source, /private scheduleInspectHighlight\(/u);
  assert.match(source, /Overlay\.highlightNode/u);
  const mousePolicy = section(source, 'webContents.on("before-mouse-event"', 'webContents.on("render-process-gone"');
  assert.match(
    mousePolicy,
    /record\.inspectActive/u,
    "Inspect Mode 的真实鼠标事件不得触发 user takeover 并提前关闭节点检查",
  );
  assert.match(
    mousePolicy,
    /record\.inspectGestureActive[\s\S]*?input\.type === "mouseDown"[\s\S]*?event\.preventDefault\(\)[\s\S]*?handleInspectPointRequested[\s\S]*?input\.type === "mouseUp"[\s\S]*?event\.preventDefault\(\)/u,
    "节点选择完成后的迟到 mouseUp 必须仍归属于同一次 Inspect 手势",
  );
  assert.match(source, /DOM\.getNodeForLocation/u);
  assert.doesNotMatch(source, /Overlay\.inspectNodeRequested/u);
  assert.match(source, /DOM\.describeNode/u);
  assert.match(source, /DOM\.getAttributes/u);
  assert.match(source, /DOM\.getOuterHTML/u);
  assert.match(source, /private async readInspectNodeText\(/u);
  assert.match(source, /this\.readInspectNodeText\([\s\S]*?node_value: textExcerpt/u);
  assert.match(source, /typeof this\.textContent === 'string'/u);
  assert.match(source, /DOM\.getBoxModel/u);
  assert.match(source, /Page\.getFrameTree/u);
  assert.match(source, /DOM\.resolveNode/u);
  assert.match(source, /Runtime\.callFunctionOn/u);
  assert.match(source, /Runtime\.releaseObjectGroup/u);
  assert.match(source, /private async resolveInspectFrameId\(/u);
  assert.match(source, /frameId = await this\.resolveInspectFrameId/u);
  assert.match(source, /backendNodeId/u);
  assert.match(source, /normalizeOptionalDomNodeId\(node\.nodeId\)/u);
  assert.match(source, /normalizeOptionalDomNodeId\(hit\.backendNodeId\)/u);
  assert.doesNotMatch(
    section(source, "async startInspect(", "private initialAgentCursorPosition("),
    /Input\.dispatchMouseEvent|capturePage\(/u,
  );
});

test("节点检查命令通过 Desktop Control Server 进入同一 Surface 生命周期", () => {
  const executeCommand = section(desktopControlSource, "private async executeCommand(", "private emit(");
  assert.match(executeCommand, /case "inspect_start":\n\s*case "inspect_stop":/u);
  assert.match(executeCommand, /requireRenderablePrimaryBindingForIdentity\(command\.payload\)/u);
  assert.match(executeCommand, /await this\.#surfaceManager\.startInspect\(binding\)/u);
  assert.match(executeCommand, /await this\.#surfaceManager\.stopInspect\(binding\)/u);
  assert.doesNotMatch(desktopControlSource, /function requirePrimaryBindingForIdentity\(/u);
  assert.match(desktopControlSource, /private async requireRenderablePrimaryBindingForIdentity\(/u);
  assert.match(desktopControlSource, /binding\.surface_id !== identity\.surface_id/u);
  assert.match(desktopControlSource, /binding\.navigation_revision !== identity\.navigation_revision/u);
});

test("节点选择只向 Host 发送当前 Primary 的完整结构化上下文", () => {
  const surfaceEvents = section(desktopControlSource, "handleSurfaceEvent(event: BrowserSurfaceEvent)", "async close(): Promise<void>");
  assert.match(surfaceEvents, /case "node_inspected":/u);
  assert.match(surfaceEvents, /this\.#surfaceManager\.isPrimary\(event\.binding\)/u);
  assert.match(surfaceEvents, /nodeSelectionFromEvent\(event\)/u);
  assert.match(surfaceEvents, /type: "node_selection"/u);
  const conversion = section(desktopControlSource, "function nodeSelectionFromEvent(", "function succeeded(");
  assert.match(conversion, /node\.browser_session_id\.trim\(\)/u);
  assert.match(conversion, /normalizeOptionalDomNodeId\(node\.backend_node_id\)/u);
  assert.match(conversion, /normalizeOptionalDomNodeId\(node\.node_id\)/u);
  assert.match(conversion, /domNodeIdIsValid/u);
  assert.doesNotMatch(conversion, /!node\.frame_id/u);
  assert.doesNotMatch(conversion, /!node\.bounds/u);
  assert.match(conversion, /backend_dom_node_id: backendDomNodeId/u);
  assert.match(conversion, /browser_session_id: node\.browser_session_id/u);
  assert.match(conversion, /outer_html: node\.outer_html/u);
  assert.match(conversion, /outer_html_truncated: node\.outer_html_truncated/u);
  assert.match(conversion, /aria_role:/u);
  assert.match(conversion, /aria_name:/u);
  assert.doesNotMatch(conversion, /surface_revision/u);
});

test("Renderer 节点选择事件只从 canonical payload 读取身份并拒绝跨 Browser Session", () => {
  const binding = section(browserTabSource, "function browserEventBinding(", "function handleDesktopBrowserEvent(");
  assert.match(binding, /event\.type === 'node_selection'[\s\S]*?event\.payload/u);
  assert.match(binding, /: event\.binding/u);
  assert.doesNotMatch(binding, /nestedBinding|payload\?\.binding/u);

  const conversion = section(browserTabSource, "function nodeSelectionFromEvent(", "function fixedPresetSelected(");
  assert.match(conversion, /selectedBrowserSessionId\s*=\s*value\.browser_session_id/u);
  assert.match(conversion, /selectedBrowserSessionId\.trim\(\) !== browserSessionId/u);
  assert.match(conversion, /browserSessionId:\s*selectedBrowserSessionId\.trim\(\)/u);
  assert.match(conversion, /event\.type === 'node_inspected'[\s\S]*?event\.payload/u);
});

test("节点检查在导航、卸载、detach、Primary 切换和销毁时失效", () => {
  const navigation = section(source, 'webContents.on("did-start-navigation"', "private async waitForDebugger(");
  assert.match(source, /private beginNavigationOperation[\s\S]*?this\.stopInspectForLifecycle\(record, "navigation"\)/u);
  const detach = section(source, 'debuggerApi.on("detach"', "private reconnectDebugger(");
  assert.match(detach, /this\.stopInspectForLifecycle\(record, "debugger-detached"\)/u);
  const unmount = section(source, "private detachSurface(", "private isRenderable(");
  assert.match(unmount, /this\.stopInspectForLifecycle\(record, "surface-unmounted"\)/u);
  const close = section(source, "private closeRecord(", "private removeRecordIndexes(");
  assert.match(close, /this\.resetDebuggerSession\(record\)/u);
  assert.match(source, /private resetDebuggerSession\(record: BrowserSurfaceRecord\)[\s\S]*?this\.stopInspectForLifecycle\(record, "debugger-detached"\)/u);
  const promotion = section(source, "private promote(surfaceId: string)", "private promoteReplacement(");
  assert.match(promotion, /this\.stopInspectForLifecycle\(previous, "surface-not-primary"\)/u);
  const generation = section(source, "private isInspectGenerationActive(", "private stopInspectForLifecycle(");
  assert.match(generation, /record\.inspectGeneration === generation/u);
  assert.match(source, /const generation = \+\+record\.inspectGeneration/u);
  assert.match(source, /const generation = \+\+record\.inspectGeneration;\n\s*record\.inspectActive = false/u);
  assert.match(
    section(source, "record.inspectStartPromise = start;", "private isInspectGenerationActive("),
    /this\.assertRenderableBinding\(binding[\s\S]*?record\.inspectActive/u,
  );
  assert.match(source, /const selectionGeneration = \+\+record\.inspectGeneration[\s\S]*?record\.inspectActive = false/u);
  assert.match(source, /this\.stopInspectForLifecycle\(record, "node-selected"\)/u);
});

test("Debugger session reset 在清零资源标记前捕获并保留 Overlay 清理请求", () => {
  const reset = section(source, "private resetDebuggerSession(", "private async installDialogBridgeInCurrentDocument(");
  const stopRequest = reset.indexOf('this.stopInspectForLifecycle(record, "debugger-detached");');
  const clearResources = reset.indexOf("record.inspectResourcesEnabled = false;");
  assert.ok(stopRequest >= 0, "session reset 必须请求 Inspect 清理");
  assert.ok(clearResources >= 0, "session reset 必须清零 Inspect 状态");
  assert.ok(
    stopRequest < clearResources,
    "必须先提交 Overlay 清理，再清零资源标记，避免清理被状态写入短路",
  );

  const stop = section(source, "private stopInspectRecord(", "private initialAgentCursorPosition(");
  assert.match(stop, /const resourcesEnabledAtRequest = record\.inspectResourcesEnabled;/u);
  assert.match(stop, /const resourcesEnabled = resourcesEnabledAtRequest \|\| record\.inspectResourcesEnabled;/u);
  assert.match(stop, /if \(!resourcesEnabledAtRequest && !record\.inspectResourcesEnabled\) return;/u);
});

test("Renderer 清理旧节点检查不会等待 stale stop 请求", () => {
  const clear = section(browserTabSource, "function clearNodeInspection(", "function toggleNodeInspection(");
  assert.match(clear, /\+\+nodeInspectGeneration/u);
  assert.match(clear, /nodeInspectBusy = false/u);
  assert.match(clear, /void desktop\.stopBrowserInspect\(identity\)\.catch\(\(\) => undefined\)/u);
  assert.doesNotMatch(clear, /nodeInspectBusy = true|\.finally\(\(\) => \{/u);
});

test("DOM 节点选择后用户移动鼠标不会撤销对话框上下文", () => {
  const handler = section(
    browserTabSource,
    "function handleDesktopBrowserEvent(value: unknown)",
    "  $effect(() => {",
  );
  const lifecycleEvents = handler.slice(
    handler.indexOf("if (\n      event.type === 'primary_changed'"),
    handler.indexOf("if (!currentIdentity || !tab || binding.surfaceId !== currentIdentity.surfaceId)") ,
  );
  assert.doesNotMatch(lifecycleEvents, /event\.type === 'user_takeover'/u);

  const userTakeover = section(
    handler,
    "if (event.type === 'user_takeover')",
    "if (event.type === 'loading_changed')",
  );
  assert.match(userTakeover, /clearNodeInspection\(true\)/u);
  assert.doesNotMatch(userTakeover, /browserNodeSelectionInvalidated/u);
});
