import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { matchesNavigationRevision } from "./browser-navigation-revision.js";
import {
  resolveBrowserPageTitle,
  resolveMaterializedPageUrl,
} from "./browser-surface-url.js";

const source = readFileSync(
  new URL("./browser-surface-manager.ts", import.meta.url),
  "utf8",
);
const desktopControlServerSource = readFileSync(
  new URL("./desktop-control-server.ts", import.meta.url),
  "utf8",
);
const windowManagerSource = readFileSync(
  new URL("./window-manager.ts", import.meta.url),
  "utf8",
);
const indexSource = readFileSync(
  new URL("./index.ts", import.meta.url),
  "utf8",
);
const preloadSource = readFileSync(
  new URL("../preload/index.ts", import.meta.url),
  "utf8",
);
const schemaSource = readFileSync(
  new URL(
    "../../../../contracts/desktop-browser/desktop-ipc.schema.json",
    import.meta.url,
  ),
  "utf8",
);
const browserTabSource = readFileSync(
  new URL(
    "../../../../web/src/components/tabs/BrowserTabContent.svelte",
    import.meta.url,
  ),
  "utf8",
);
const rightPaneSource = readFileSync(
  new URL("../../../../web/src/web/RightPane.svelte", import.meta.url),
  "utf8",
);

function normalizeSourceWhitespace(value: string): string {
  return value.replace(/\s+/gu, " ");
}

function findNormalizedSource(pattern: RegExp, fromIndex = 0): number {
  const match = pattern.exec(normalizedSource.slice(fromIndex));
  return match ? fromIndex + match.index : -1;
}

const normalizedSource = normalizeSourceWhitespace(source);
const normalizedWindowManagerSource =
  normalizeSourceWhitespace(windowManagerSource);
const normalizedPreloadSource = normalizeSourceWhitespace(preloadSource);

test("导航 revision 只允许同一代次或明确的前进", () => {
  assert.equal(matchesNavigationRevision(4, 4, false), true);
  assert.equal(matchesNavigationRevision(3, 4, false), false);
  assert.equal(matchesNavigationRevision(5, 4, false), false);
  assert.equal(matchesNavigationRevision(4, 5, true), true);
});

test("已有 Surface 不会被旧投影的 about:blank 覆盖", () => {
  assert.equal(
    resolveMaterializedPageUrl(
      "about:blank",
      false,
      "https://example.com/",
      "about:blank",
    ),
    "https://example.com/",
  );
  assert.equal(
    resolveMaterializedPageUrl(
      "about:blank",
      false,
      "https://example.com/",
      "https://example.org/",
    ),
    "https://example.org/",
  );
  assert.equal(
    resolveMaterializedPageUrl(
      "about:blank",
      true,
      "https://example.com/",
      "about:blank",
    ),
    "about:blank",
  );
  assert.equal(
    resolveMaterializedPageUrl(
      "https://example.org/",
      false,
      "https://example.com/",
      "about:blank",
    ),
    "https://example.org/",
  );
});

test("真实 URL 下的 about:blank 标题不能覆盖已确认的页面标题", () => {
  assert.equal(
    resolveBrowserPageTitle(
      "https://example.com/",
      "Example Domain",
      "about:blank",
    ),
    "Example Domain",
  );
  assert.equal(
    resolveBrowserPageTitle("https://example.com/", "", "Example Domain"),
    "Example Domain",
  );
  assert.equal(
    resolveBrowserPageTitle("about:blank", "", "about:blank"),
    "about:blank",
  );
  assert.equal(
    resolveBrowserPageTitle("https://example.com/", "", "about:blank"),
    "",
  );
});

test("浏览器显示由 Renderer webview 承载，Main 不创建或调整浏览器 View", () => {
  assert.match(browserTabSource, /<webview[\s\S]*class="browser-webview"/u);
  assert.match(browserTabSource, /getWebContentsId\(\)/u);
  assert.match(browserTabSource, /registerBrowserWebview\(/u);
  assert.doesNotMatch(source, /new\s+WebContentsView\(/u);
  assert.doesNotMatch(source, /\.setBounds\(/u);
  assert.doesNotMatch(source, /\.addChildView\(/u);
  assert.doesNotMatch(
    source,
    /rendererGeometry|browserContentSlot|bindContentSurface/u,
  );
  assert.doesNotMatch(
    windowManagerSource,
    /bindContentSurface|browserContentSlot|rendererGeometry/u,
  );
  assert.match(browserTabSource, /\.browser-webview\s*\{[^}]*display:\s*flex/u);
});

test("Webview 注册严格绑定当前窗口、Browser Session、导航代次和 partition", () => {
  assert.match(indexSource, /parseEmbeddedBrowserWebview\(value\)/u);
  assert.match(
    indexSource,
    /manager\.registerEmbeddedWebview\(windowId, request\)/u,
  );
  assert.match(source, /guest\.getType\(\) !== "webview"/u);
  assert.match(source, /guest\.hostWebContents !== host/u);
  assert.match(source, /guest\.session !== expectedSession/u);
  assert.match(
    source,
    /record\.navigationRevision !== input\.navigationRevision/u,
  );
  assert.match(source, /record\.contents === guest/u);
  assert.match(source, /record\.contents !== guest/u);
  assert.match(
    normalizedPreloadSource,
    /registerBrowserWebview:.*magi-desktop:register-browser-webview/u,
  );
  assert.match(schemaSource, /magi-desktop:register-browser-webview/u);
  assert.doesNotMatch(
    source,
    /contentSize|updateEmbeddedWebviewContentSize|contentWidth|contentHeight/u,
  );
  assert.doesNotMatch(
    preloadSource,
    /updateBrowserContentSize|update-browser-content-size/u,
  );
  assert.doesNotMatch(
    schemaSource,
    /updateBrowserContentSize|update-browser-content-size/u,
  );
  assert.doesNotMatch(
    schemaSource,
    /renderer_geometry|begin-right-pane-resize|end-right-pane-resize/u,
  );
});

test("重新激活 Browser Tab 使用 Main Surface 的当前导航代次", () => {
  assert.match(
    normalizedWindowManagerSource,
    /activeBrowserNavigationRevision:\s*this\s*\.\s*#surfaceManager\s*\.\s*navigationRevisionForSurface\(\s*layout\s*\.\s*activeSurfaceId,?\s*\)/u,
  );
  assert.match(
    browserTabSource,
    /const navigationRevision = desktopSnapshot\?\.activeBrowserNavigationRevision/u,
  );
  const identityStart = browserTabSource.indexOf(
    "const activeBrowserIdentity =",
  );
  const identityEnd = browserTabSource.indexOf("const error =", identityStart);
  assert.ok(identityStart >= 0 && identityEnd > identityStart);
  const identity = browserTabSource.slice(identityStart, identityEnd);
  assert.match(identity, /navigationRevision,\s*\n\s*\};/u);
  assert.doesNotMatch(
    identity,
    /navigationRevision:\s*tab\.navigationRevision/u,
  );
});

test("重绑已有 guest 后重新发布真实页面标题，不能保留 about:blank 元数据", () => {
  assert.match(
    normalizedSource,
    /private publishCurrentPage\(record: BrowserSurfaceRecord\): void[\s\S]*?this\.#onEvent\(\{\s*type: "page_updated"/u,
  );
  assert.match(
    normalizedSource,
    /this\.startDebuggerInitialization\(record\);[\s\S]*?if \(record\.primary\) \{[\s\S]*?this\.publishPrimaryChanged\(record\);[\s\S]*?\}[\s\S]*?this\.publishCurrentPage\(record\);[\s\S]*?return this\.binding\(record\);/u,
  );
  assert.match(
    normalizedSource,
    /record\.priming = false;[\s\S]*?this\.publishPageForNavigation\(record, operation, true\);/u,
  );
  assert.match(
    normalizedSource,
    /private publishPrimaryChanged\(record: BrowserSurfaceRecord\): void[\s\S]*?this\.publishCurrentPage\(record\);/u,
  );
  assert.match(
    normalizedSource,
    /private async syncPageTitleThroughDebugger\(\s*record: BrowserSurfaceRecord, lease: DebuggerSessionLease,?\s*\): Promise<void>[\s\S]*?"Runtime\.evaluate"[\s\S]*?expression: "document\.title"[\s\S]*?type: "page_updated"/u,
  );
  assert.doesNotMatch(
    normalizedSource,
    /executeJavaScript\("document\.title", true\)/u,
  );
  assert.match(
    normalizedSource,
    /record\.debuggerSessionInitialized = true;[\s\S]*?if \(record\.primary\) \{[\s\S]*?syncPageTitleThroughDebugger\(record, lease\)/u,
  );
  assert.match(
    normalizedSource,
    /this\.publishPageForNavigation\(record, operation, true\);\s*this\.syncPageTitleFromContent\(record\);/u,
  );
  assert.match(
    normalizedSource,
    /webContents\.on\("page-title-updated", \(_event, title\) => \{[\s\S]*?phase: "title",\s*allowSettled: true,[\s\S]*?if \(title\.trim\(\)\) record\.pageTitle = title\.trim\(\);[\s\S]*?isChromiumErrorPageUrl\(webContents\.getURL\(\) \|\| ""\)[\s\S]*?return;/u,
  );
  assert.match(
    normalizedSource,
    /private\s+completeNavigationOperation\([\s\S]*?this\.publishPageForNavigation\(record, operation, true\);/u,
  );
  assert.match(
    normalizedSource,
    /private publishRendererOwnedSettledPage\(record: BrowserSurfaceRecord\): void[\s\S]*?this\.syncPageTitleFromContent\(record\)[\s\S]*?this\.#onDocumentReady\?\.\(this\.binding\(record\)\)/u,
  );
  assert.match(
    normalizedSource,
    /webContents\.on\("did-finish-load", \(\) => \{[\s\S]*?if \(!record\.navigationOperation\) \{[\s\S]*?this\.publishRendererOwnedSettledPage\(record\)/u,
  );
  assert.match(
    normalizedSource,
    /webContents\.on\("did-stop-loading", \(\) => \{[\s\S]*?if \(!record\.navigationOperation\) \{[\s\S]*?this\.publishRendererOwnedSettledPage\(record\)/u,
  );
  assert.match(
    normalizedSource,
    /operation\.kind === "document"[\s\S]*?url === "about:blank"[\s\S]*?operation\.targetUrl !== "about:blank"[\s\S]*?return null;/u,
  );
});

test("真实 guest 绑定内容槽后即可就绪，CDP 不阻塞页面激活", () => {
  assert.match(
    normalizedWindowManagerSource,
    /resolveBrowserSurfaceReadinessForBinding\(\s*binding: BrowserSurfaceBinding,?\s*\)/u,
  );
  assert.match(
    normalizeSourceWhitespace(indexSource),
    /event\.type === "primary_changed"[\s\S]*?resolveBrowserSurfaceReadinessForBinding\(\s*event\.binding\s*,?\s*\)/u,
  );
  assert.match(
    normalizedWindowManagerSource,
    /isContentSlotBoundBinding\(binding\)/u,
  );
  const readinessStart = normalizedSource.search(
    /private\s+isContentSlotBound\(/u,
  );
  const readinessEnd = normalizedSource.search(
    /private\s+configurePartition\(/u,
  );
  assert.ok(readinessStart >= 0 && readinessEnd > readinessStart);
  const readiness = normalizedSource.slice(readinessStart, readinessEnd);
  assert.doesNotMatch(readiness, /debuggerSessionInitialized/u);
});

test("浏览器下载由 Main 私有目录和真实生命周期事件驱动", () => {
  const downloadStart = source.indexOf("private handleDownload(");
  const downloadEnd = source.indexOf(
    "private async performDownloadCleanup()",
    downloadStart,
  );
  const download = normalizeSourceWhitespace(
    source.slice(downloadStart, downloadEnd),
  );
  assert.ok(downloadStart >= 0 && downloadEnd > downloadStart);
  assert.match(download, /event\.preventDefault\(\)/u);
  assert.match(source, /browserDownloadRoot\(/u);
  assert.match(source, /#downloadRoot = [\s\S]*?browserDownloadRoot\(/u);
  assert.match(
    download,
    /setSavePath\(\s*join\(directory, `\$\{Date\.now\(\)\}-\$\{randomUUID\(\)\}-\$\{filename\}`\)\s*,?\s*\)/u,
  );
  assert.match(download, /state: "started"/u);
  assert.match(
    download,
    /item\.on\("updated"[\s\S]*?state === "progressing" \? "progressing" : "interrupted"/u,
  );
  assert.match(download, /item\.once\("done"[\s\S]*?state,/u);
  assert.match(download, /item\.getReceivedBytes\(\)/u);
  assert.match(download, /nonNegativeTotalBytes\(item\.getTotalBytes\(\)\)/u);
  assert.match(download, /record\.downloads\.set\(downloadId, \{[\s\S]*?item,[\s\S]*?state: "started"/u);
  assert.match(download, /download\.state = state === "progressing" \? "progressing" : "interrupted"/u);
  assert.match(download, /record\.downloads\.delete\(downloadId\)/u);
  assert.match(
    normalizedSource,
    /state: download\.state,[\s\S]*?receivedBytes: download\.item\.getReceivedBytes\(\)/u,
  );
  assert.doesNotMatch(download, /setSavePath\(.*Downloads/u);
  assert.match(source, /cancelDownloadsForRecord\(record\)/u);
  assert.match(
    source,
    /browserDownloadSnapshotsForWindow\(\s*windowId:\s*string\s*,?\s*\)/u,
  );
  assert.match(
    source,
    /cancelBrowserDownload\(\s*windowId:\s*string\s*,\s*tabId:\s*string\s*,\s*downloadId:\s*string\s*,?\s*\)/u,
  );
});

test("任务控制释放只释放 Lease，不关闭 Browser Tab 或当前页面", () => {
  const releaseStart = source.indexOf(
    "private releaseAgentControlForUserInput(",
  );
  const releaseEnd = source.indexOf(
    "private async waitForDebugger(",
    releaseStart,
  );
  assert.ok(releaseStart >= 0 && releaseEnd > releaseStart);
  const release = source.slice(releaseStart, releaseEnd);
  assert.match(release, /record\.agentControlled = false/u);
  assert.match(release, /this\.setAgentCursor\(/u);
  assert.match(release, /type: "user_takeover"/u);
  assert.doesNotMatch(release, /closeTab|closeRecord|contents\.close/u);

  const disconnectedStart = source.indexOf(
    "releaseDisconnectedHostControl(): void",
  );
  const disconnectedEnd = source.indexOf(
    "closeWindow(windowId: string): void",
    disconnectedStart,
  );
  assert.ok(disconnectedStart >= 0 && disconnectedEnd > disconnectedStart);
  const disconnected = source.slice(disconnectedStart, disconnectedEnd);
  assert.match(disconnected, /record\.agentControlled = false/u);
  assert.match(disconnected, /this\.setAgentCursor\(/u);
  assert.doesNotMatch(disconnected, /closeTab|closeRecord|contents\.close/u);

  const executeStart = desktopControlServerSource.indexOf(
    "case \"update_control\":",
  );
  const executeEnd = desktopControlServerSource.indexOf(
    "case \"shutdown\":",
    executeStart,
  );
  assert.ok(executeStart >= 0 && executeEnd > executeStart);
  assert.doesNotMatch(
    desktopControlServerSource.slice(executeStart, executeEnd),
    /closeTab|close_page/u,
  );
});

test("Renderer 消费当前 Tab 的真实下载事件并提供取消入口", () => {
  assert.match(browserTabSource, /if \(event\.type === 'download'\)/u);
  assert.match(browserTabSource, /function applyDownloadEvent/u);
  assert.match(browserTabSource, /activeBrowserDownloads/u);
  assert.match(browserTabSource, /cancelBrowserDownload\(/u);
  assert.match(browserTabSource, /browser-download-panel/u);
  assert.match(browserTabSource, /formatDownloadBytes/u);
  assert.match(indexSource, /magi-desktop:cancel-browser-download/u);
  assert.match(preloadSource, /cancelBrowserDownload:/u);
  assert.match(schemaSource, /magi-desktop:cancel-browser-download/u);
});

test("旧 WebContents 的迟到事件不会污染当前 Surface 生命周期", () => {
  assert.match(
    normalizedSource,
    /const isCurrentGuest = \(\): boolean => !record\.closed && record\.contents === webContents;/u,
  );
  const policyStart = source.indexOf("private installSurfacePolicy(");
  const policy = source.slice(policyStart);
  assert.match(
    policy,
    /webContents\.on\("render-process-gone",[\s\S]*?if \(!isCurrentGuest\(\)\) return;/u,
  );
  assert.match(
    policy,
    /setImmediate\(\(\) => \{\s*if \(!isCurrentGuest\(\)\) return;/u,
  );
});

test("普通鼠标输入不会重复广播 AI 接管事件", () => {
  const policyStart = source.indexOf('webContents.on("before-input-event"');
  const policyEnd = source.indexOf(
    'webContents.on("render-process-gone"',
    policyStart,
  );
  assert.ok(policyStart >= 0 && policyEnd > policyStart);
  const policy = source.slice(policyStart, policyEnd);
  assert.equal((policy.match(/type: "user_takeover"/gu) ?? []).length, 0);
  assert.equal(
    (policy.match(/this\.releaseAgentControlForUserInput\(record\)/gu) ?? [])
      .length,
    2,
  );

  const releaseStart = source.indexOf(
    "private releaseAgentControlForUserInput",
  );
  const releaseEnd = source.indexOf(
    "private async waitForDebugger",
    releaseStart,
  );
  assert.ok(releaseStart >= 0 && releaseEnd > releaseStart);
  const release = source.slice(releaseStart, releaseEnd);
  assert.match(
    release,
    /if \(record\.closed \|\| !record\.agentControlled\) return/u,
  );
  assert.match(release, /record\.agentControlled = false/u);
  assert.match(release, /type: "user_takeover"/u);
});

test("关闭 Surface 等待 Renderer release ACK，物理 guest 由 webview DOM 自然销毁", () => {
  const closeStart = source.indexOf("private closeRecord(");
  const closeEnd = source.indexOf("private removeRecordIndexes", closeStart);
  assert.ok(closeStart >= 0 && closeEnd > closeStart);
  const closeRecord = source.slice(closeStart, closeEnd);
  assert.match(closeRecord, /record\.closed = true/u);
  assert.match(closeRecord, /record\.lifecycleAbort\.abort\(\)/u);
  assert.match(closeRecord, /#closingSurfaces\.add\(record\)/u);
  assert.doesNotMatch(closeRecord, /record\.contents = null/u);
  assert.match(source, /releaseEmbeddedWebview\(/u);
  assert.match(source, /detachEmbeddedGuest\(active\)/u);
  assert.doesNotMatch(source, /guest\.close\(\)/u);
  assert.doesNotMatch(source, /releasePhysicalGuest/u);
  assert.match(source, /Authority 的 close_page/u);
  const replacementStart = normalizedSource.search(
    /if \( record\.contents && !record\.contents\.isDestroyed\(\) && record\.contents !== guest \)/u,
  );
  const replacementEnd = normalizedSource.indexOf(
    "if (record.contents === guest)",
    replacementStart,
  );
  assert.ok(replacementStart >= 0 && replacementEnd > replacementStart);
  assert.doesNotMatch(
    normalizedSource.slice(replacementStart, replacementEnd),
    /\.close\(\)/u,
  );
});

test("物理 guest 终止后只解除绑定，不进入调试器无限重连", () => {
  const destroyedStart = source.indexOf(
    'guest.once("destroyed"',
    source.indexOf("registerEmbeddedWebview"),
  );
  const destroyedEnd = source.indexOf("});", destroyedStart);
  assert.ok(destroyedStart >= 0 && destroyedEnd > destroyedStart);
  const destroyedHandler = source.slice(destroyedStart, destroyedEnd);
  assert.match(
    destroyedHandler,
    /record\.closed\) this\.finalizeClosingSurface\(record\)/u,
  );
  assert.match(destroyedHandler, /else this\.detachEmbeddedGuest\(record\)/u);
  assert.doesNotMatch(destroyedHandler, /else this\.closeRecord\(record\)/u);

  const detachStart = source.indexOf(
    "const detachListener: DebuggerDetachListener",
  );
  const detachEnd = source.indexOf('debuggerApi.on("message"', detachStart);
  assert.ok(detachStart >= 0 && detachEnd > detachStart);
  const detachHandler = source.slice(detachStart, detachEnd);
  assert.match(detachHandler, /isBrowserTargetClosedError\(reason\)/u);
  assert.match(detachHandler, /this\.detachEmbeddedGuest\(record\)/u);
  assert.match(detachHandler, /return;/u);
  assert.match(detachHandler, /this\.reconnectDebugger\(record/u);

  const laneStart = normalizedSource.indexOf("const lane = previous");
  const laneEnd = normalizedSource.indexOf("record.cdpLane =", laneStart);
  assert.ok(laneStart >= 0 && laneEnd > laneStart);
  assert.match(
    normalizedSource.slice(laneStart, laneEnd),
    /isBrowserTargetClosedError\(error\)/u,
  );
  assert.match(
    normalizedSource.slice(laneStart, laneEnd),
    /this\.detachEmbeddedGuest\(record\)/u,
  );
  assert.match(
    source,
    /function isBrowserTargetClosedError\(value: unknown\)/u,
  );
});

test("右栏切换只隐藏非活动 Browser Tab，不卸载其 Chromium guest", () => {
  assert.match(
    rightPaneSource,
    /\{#each openTabs as tab \(tab\.id\)\}[\s\S]*?tab\.kind === 'browser'[\s\S]*?BrowserTabContent/u,
  );
  assert.match(
    rightPaneSource,
    /hidden=\{tab\.id !== paneState\.activeTabId \|\| activeTab\?\.kind !== 'browser'\}/u,
  );
  assert.match(
    rightPaneSource,
    /\.right-pane-browser-tab-host\s*\{[\s\S]*?position:\s*absolute/u,
  );
  assert.match(
    rightPaneSource,
    /\.right-pane-body\s*\{[\s\S]*?position:\s*relative/u,
  );
});

test("浏览器 popup 永远复用当前一级 Browser Tab", () => {
  const policy = source.slice(source.indexOf("private installSurfacePolicy("));
  assert.match(policy, /setWindowOpenHandler\(\(details\) => \{/u);
  assert.match(policy, /return \{ action: "deny" \};/u);
  assert.match(policy, /type: "popup_blocked"/u);
  assert.doesNotMatch(policy, /new\s+(BrowserWindow|WebContentsView)\(/u);
});

test("失败导航由当前 Browser 内容槽的 Top Layer 呈现，不让 webview 白屏覆盖错误事实", () => {
  assert.match(browserTabSource, /pageErrorElement/u);
  assert.match(
    browserTabSource,
    /pageError && browserReady && !browserLoading/u,
  );
  assert.match(
    browserTabSource,
    /class="browser-page-error"[\s\S]*popover="manual"/u,
  );
  assert.match(browserTabSource, /browser\.error\.pageLoadFailed/u);
  assert.match(browserTabSource, /navigate\('reload'\)/u);
  assert.match(
    browserTabSource,
    /\.browser-page-error\s*\{[^}]*position:\s*fixed;[^}]*position-anchor:\s*--browser-surface-anchor/u,
  );
  assert.match(source, /type: "page_failed"[\s\S]*reason/u);
  const failureHandlerStart = normalizedSource.indexOf(
    'webContents.on( "did-fail-load"',
  );
  assert.ok(failureHandlerStart >= 0);
  const failureHandler = normalizedSource.slice(
    failureHandlerStart,
    normalizedSource.indexOf(
      'webContents.on( "before-input-event"',
      failureHandlerStart,
    ),
  );
  assert.match(
    failureHandler,
    /currentOperation\.settled[\s\S]*?currentOperation\.completed = false;[\s\S]*?failNavigationOperation/u,
  );
  const frameNavigateStart = normalizedSource.search(
    /webContents\.on\(\s*"did-frame-navigate"/u,
  );
  assert.ok(frameNavigateStart >= 0);
  const frameNavigateHandler = normalizedSource.slice(
    frameNavigateStart,
    findNormalizedSource(/webContents\.on\(\s*"did-navigate"/u, frameNavigateStart),
  );
  assert.match(
    frameNavigateHandler,
    /phase: "error-page"[\s\S]*runtimeErrorPage = true/u,
  );
  assert.match(
    frameNavigateHandler,
    /phase: "error-page"[\s\S]*return;[\s\S]*committedUrl = url/u,
  );
  const didNavigateStart = normalizedSource.search(
    /webContents\.on\(\s*"did-navigate"/u,
  );
  assert.ok(didNavigateStart >= 0);
  const didNavigateHandler = normalizedSource.slice(
    didNavigateStart,
    findNormalizedSource(
      /webContents\.on\(\s*"did-navigate-in-page"/u,
      didNavigateStart,
    ),
  );
  assert.match(didNavigateHandler, /isChromiumErrorPageUrl\(url\)\) return/u);
  assert.match(
    didNavigateHandler,
    /isChromiumErrorPageUrl\(url\)\) return;[\s\S]*committedUrl = url/u,
  );
  assert.doesNotMatch(source, /operationFailedForChromiumErrorPage/u);
  const pageUpdatedStart = browserTabSource.indexOf(
    "if (event.type !== 'page_updated' || !event.page) return;",
  );
  assert.ok(pageUpdatedStart >= 0);
  const pageUpdatedHandler = browserTabSource.slice(
    pageUpdatedStart,
    browserTabSource.indexOf("$effect(() =>", pageUpdatedStart),
  );
  assert.doesNotMatch(pageUpdatedHandler, /pageError\s*=\s*''/u);
  assert.match(
    source,
    /isChromiumErrorPageUrl\(webContents\.getURL\(\)[\s\S]*?operation\.runtimeErrorPage = true;[\s\S]*?return;/u,
  );
  const stopLoadingStart = source.indexOf('webContents.on("did-stop-loading"');
  assert.ok(stopLoadingStart >= 0);
  const stopLoadingHandler = source.slice(
    stopLoadingStart,
    source.indexOf('webContents.on("did-fail-load"', stopLoadingStart),
  );
  assert.match(
    stopLoadingHandler,
    /isChromiumErrorPageUrl\(webContents\.getURL\(\)[\s\S]*?operation\.runtimeErrorPage = true;[\s\S]*?return;/u,
  );
  assert.match(source, /runtimeErrorPage:\s*false/u);
  assert.match(
    source,
    /completeNavigationOperation\([\s\S]*?operation\.runtimeErrorPage/u,
  );
  assert.match(
    source,
    /completeWhenMainFrameSettled\([\s\S]*?operation\.runtimeErrorPage/u,
  );
  assert.match(
    source,
    /completeNavigationFromSettledEvent\([\s\S]*?operation\.runtimeErrorPage = true/u,
  );
  assert.match(
    source,
    /publishPageForNavigation\([\s\S]*?operation\.runtimeErrorPage/u,
  );
  assert.match(
    source,
    /isChromiumErrorPageUrl\(runtimeUrl\)\) return;[\s\S]*?page: this\.pageState\(record\)/u,
  );
  const loadPromiseStart = source.search(/void\s+loadPromise\.then\(/u);
  assert.ok(loadPromiseStart >= 0);
  const loadPromiseHandler = source.slice(
    source.indexOf("(error) => {", loadPromiseStart),
    source.indexOf("const navigationWait =", loadPromiseStart),
  );
  assert.doesNotMatch(
    loadPromiseHandler,
    /!this\.isCurrentNavigationOperation\(record, operation\)[\s\S]*?operation\.settled[\s\S]*?isNavigationAbortError\(error\)/u,
    "settled 检查不能在 loadURL 迟到失败转换前拦截 chrome-error 事务",
  );
  assert.match(loadPromiseHandler, /failCompletedNavigationOperation/u);
  assert.match(loadPromiseHandler, /failNavigationOperation/u);
  assert.match(
    source,
    /operation\.settled\s*&&\s*operation\.completed\s*&&\s*isChromiumErrorPageUrl\(\s*contents\.getURL\(\)\s*\|\|\s*""\s*\)[\s\S]*?failCompletedNavigationOperation/u,
    "loadURL 迟到失败必须能把已误完成的 chrome-error 事务转换为失败终态",
  );
  assert.match(
    source,
    /method\s*===\s*"Network\.loadingFailed"[\s\S]*?eventParams\.type\s*===\s*"Document"[\s\S]*?networkFailureRequestId\s*=\s*eventParams\.requestId[\s\S]*?networkFailureError\s*=\s*normalizeNetworkFailureError\(\s*eventParams\.errorText\s*,?\s*\)/u,
    "主文档 Network 失败必须记录到当前导航事务，供错误页提交时还原真实错误码",
  );
  assert.match(
    source,
    /method\s*===\s*"Page\.frameNavigated"[\s\S]*?isChromiumErrorPageFrame\(\s*eventParams\s*\)[\s\S]*?navigationFailureReason\(\s*eventParams\s*,\s*operation\s*\)[\s\S]*?this\.failNavigationOperation\(/u,
    "CDP 错误页提交必须作为同一导航事务的权威失败事实",
  );
  assert.match(
    source,
    /loaderId === operation\.networkFailureRequestId[\s\S]*operation\.networkFailureError/u,
    "失败原因必须优先使用同一 loader 的 Network.loadingFailed 错误码",
  );
  const normalizeNetworkFailureStart = source.indexOf(
    "function normalizeNetworkFailureError",
  );
  assert.ok(normalizeNetworkFailureStart >= 0);
  const normalizeNetworkFailure = source.slice(normalizeNetworkFailureStart);
  assert.match(
    normalizeNetworkFailure,
    /replace\(\/\^net::\/u, ""\)/u,
    "网络错误码必须移除 Chromium 的 net:: 前缀，不能改写成固定 DNS 错误",
  );
  assert.doesNotMatch(source, /ERR_NAME_NOT_RESOLVED\s+\$\{unreachableUrl/u);
  assert.match(
    source,
    /did-fail-load[\s\S]*?currentOperation\.completed = false;[\s\S]*?currentOperation\.stopEventSequence = null;[\s\S]*?failNavigationOperation/u,
  );
  assert.match(
    source,
    /expectation\.phase\s*!==\s*"start-loading"\s*&&\s*expectation\.phase\s*!==\s*"error-page"\s*&&\s*expectation\.phase\s*!==\s*"fail"\s*&&\s*expectation\.phase\s*!==\s*"title"\s*&&\s*operation\.startEventSequence\s*===\s*null/u,
    "did-fail-load 早于 did-start-navigation 时也必须作为权威失败事实收口当前导航",
  );
  assert.match(
    source,
    /isStaleInitialAboutBlankCommit\(operation, expectation\.url\)\) return null/u,
  );
  assert.match(
    source,
    /isStaleInitialAboutBlankCommit\(operation, url\)\) return null/u,
  );
  assert.match(
    source,
    /function isStaleInitialAboutBlankCommit\([\s\S]*?operation\.targetUrl !== "about:blank"/u,
  );
  assert.match(
    source,
    /const\s+currentUrlIsErrorPage\s*=\s*isChromiumErrorPageUrl\(\s*currentUrl\s*\);[\s\S]*?currentUrl\s*!==\s*"about:blank"\s*&&\s*!currentUrlIsErrorPage\s*&&\s*!guest\.isLoadingMainFrame\(\)/u,
    "重启重绑 chrome-error guest 时不能把旧成功页面事实重新发布为当前页面",
  );
  assert.match(
    source,
    /if \(currentUrlIsErrorPage\) \{[\s\S]*record\.pageTitle = ""[\s\S]*publishRestoredErrorPage\(record\)/u,
    "重启重绑 chrome-error guest 时必须发布可重试失败投影，不能留下目标地址加白屏",
  );
});

test("Auto 视口使用 webview 自然尺寸，固定视口只使用 Chromium 设备指标", () => {
  const autoSection = source.slice(
    source.indexOf('if (commit.viewport.mode === "auto")'),
  );
  assert.match(autoSection, /contents\.disableDeviceEmulation\(\)/u);
  assert.match(autoSection, /<webview> 的真实 DOM bounds 就是页面/u);
  assert.match(autoSection, /contents\.enableDeviceEmulation\(\{/u);
  assert.match(autoSection, /const scale = 1/u);
  assert.doesNotMatch(
    source,
    /contentSize|contentWidth|contentHeight|browserDeviceEmulationScale/u,
  );
  assert.match(browserTabSource, /mode === 'auto'[\s\S]*?\{ mode: 'auto' \}/u);
  assert.doesNotMatch(
    browserTabSource,
    /updateBrowserContentSize|new ResizeObserver\(/u,
  );
});

test("截图和 DOM 选择都来自同一真实 Chromium WebContents", () => {
  assert.match(source, /method === "Page\.captureScreenshot"/u);
  assert.match(
    source,
    /this\.enqueueCdp\(record[\s\S]*?sendSurfaceCdpCommand/u,
  );
  assert.match(source, /DOM\.getNodeForLocation/u);
  assert.match(source, /Overlay\.highlightNode/u);
  assert.doesNotMatch(source, /capturePage\(|startScreencast|drawImage\(/u);
  assert.match(browserTabSource, /magi:browserScreenshotCaptured/u);
  assert.match(browserTabSource, /magi:browserNodeSelected/u);
});
