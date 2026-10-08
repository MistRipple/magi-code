import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const read = (path) => readFile(join(root, path), 'utf8');
const files = {
  surface: 'apps/desktop/src/main/browser-surface-manager.ts',
  window: 'apps/desktop/src/main/window-manager.ts',
  layout: 'apps/desktop/src/main/window-layout.ts',
  main: 'apps/desktop/src/main/index.ts',
  preload: 'apps/desktop/src/preload/index.ts',
  browserTab: 'web/src/components/tabs/BrowserTabContent.svelte',
  rightPane: 'web/src/web/RightPane.svelte',
  workbench: 'web/src/web/WebWorkbenchShell.svelte',
  schema: 'contracts/desktop-browser/desktop-ipc.schema.json',
  worker: 'browser-automation-worker/src/runtime.ts',
  downloadStorage: 'apps/desktop/src/main/browser-download-storage.ts',
};
const sources = Object.fromEntries(
  await Promise.all(Object.entries(files).map(async ([name, path]) => [name, await read(path)])),
);

const removedGeometry = /browserContentSlot|rendererGeometry|renderer_geometry|bindContentSurface|browserContentBounds/u;
for (const name of ['surface', 'window', 'layout', 'main', 'preload', 'browserTab', 'rightPane', 'workbench', 'schema']) {
  assert.doesNotMatch(sources[name], removedGeometry, `${files[name]} 仍包含已删除的浏览器几何协议`);
}

assert.match(sources.browserTab, /<webview[\s\S]*class="browser-webview"/u);
assert.match(sources.browserTab, /getWebContentsId\(\)/u);
assert.match(sources.browserTab, /registerBrowserWebview\(/u);
assert.match(sources.browserTab, /partition=\{browserPartitionForSession\(browserSessionId\)\}/u);
assert.match(sources.rightPane, /class="right-pane-browser-tab-host"[\s\S]*hidden=\{/u);
assert.match(sources.rightPane, /<BrowserTabContent[\s\S]*desktopSurface=\{desktopSurface\}/u);
assert.match(sources.workbench, /desktop-right-pane-column/u);
assert.match(sources.workbench, /grid-template-columns:[\s\S]*desktop-right-pane-width/u);

assert.match(sources.surface, /registerEmbeddedWebview\(/u);
assert.match(sources.surface, /webContents\.fromId\(input\.webContentsId\)/u);
assert.match(sources.surface, /guest\.getType\(\) !== "webview"/u);
assert.match(sources.surface, /guest\.hostWebContents !== host/u);
// Partition creation is centralized so the application-level persistent GPT Web
// partition does not accidentally get reopened as an in-memory session. Ordinary
// browser tabs may still use cache:false; the helper is the contract we verify.
assert.match(sources.surface, /browserSessionForPartition\(record\.partitionId\)/u);
assert.match(
  sources.surface,
  /function\s+browserSessionForPartition\(partitionId:\s*string\)[\s\S]*?partitionId\.startsWith\("persist:"\)[\s\S]*?session\.fromPartition\(partitionId\)/u,
);
assert.match(sources.surface, /primaryBindingForTab\(/u);
assert.match(sources.surface, /setWindowOpenHandler\(\(details\) =>/u);
assert.match(sources.surface, /type: "popup_blocked"/u);
assert.match(sources.surface, /decideBrowserPopup\(details\)/u);
assert.match(sources.surface, /reason: decision\.reason/u);
assert.doesNotMatch(sources.surface, /new\s+WebContentsView\(/u);
assert.doesNotMatch(sources.surface, /\.setBounds\(/u);
assert.doesNotMatch(sources.surface, /\.addChildView\(/u);

assert.match(sources.surface, /Page\.captureScreenshot/u);
assert.match(sources.surface, /DOM\.getNodeForLocation/u);
assert.match(sources.surface, /Overlay\.highlightNode/u);
assert.match(sources.surface, /"Emulation\.clearDeviceMetricsOverride"/u);
assert.match(sources.surface, /"Emulation\.setDeviceMetricsOverride"/u);
assert.doesNotMatch(sources.surface, /\.enableDeviceEmulation\(|\.disableDeviceEmulation\(/u);
assert.match(sources.worker, /case\s+['"]screenshot['"][\s\S]*Page\.captureScreenshot/u);
assert.match(sources.worker, /DOM\.getDocument|Accessibility\.getFullAXTree/u);

assert.match(sources.window, /new\s+BrowserWindow\(/u);
assert.match(sources.window, /attachWindow\(windowId, window\.webContents\)/u);
assert.doesNotMatch(sources.window, /\bBaseWindow\b|\bWebContentsView\b|\bappView\b|\.addChildView\(/u);
assert.doesNotMatch(sources.window, /bindContentSurface|browserContentBounds|browserContentSlot|rendererGeometry/u);
assert.match(sources.main, /handleIpc\(\s*"magi-desktop:register-browser-webview"/u);
assert.match(sources.main, /handleIpc\(\s*"magi-desktop:update-browser-display-size"/u);
assert.match(sources.main, /configureAppRendererAuthentication\(controlToken\)/u);
assert.match(sources.main, /X-Magi-Desktop-Renderer-Token/u);
assert.doesNotMatch(sources.preload, /Desktop-Renderer-Token|controlToken/u);
assert.match(
  sources.preload,
  /registerBrowserWebview:\s*\([^)]*\)\s*=>[\s\S]*?magi-desktop:register-browser-webview/u,
);
assert.match(sources.schema, /magi-desktop:register-browser-webview/u);
assert.match(sources.schema, /magi-desktop:update-browser-display-size/u);

assert.match(sources.browserTab, /VIEWPORT_DEVICE_MODES[\s\S]*id: 'wide'[\s\S]*id: 'narrow'/u);
assert.match(sources.browserTab, /useAutomaticViewport\(\)/u);
assert.match(sources.browserTab, /CUSTOM_VIEWPORT_DEBOUNCE_MILLIS/u);
assert.match(sources.browserTab, /new ResizeObserver\(scheduleBrowserDisplaySizeSync\)/u);
assert.match(sources.browserTab, /updateBrowserDisplaySize\(/u);
assert.match(sources.browserTab, /webpreferences="focusOnNavigation=no"/u);
assert.match(sources.browserTab, /magi:browserScreenshotCaptured/u);
assert.match(sources.browserTab, /magi:browserNodeSelected/u);
assert.match(sources.browserTab, /annotationMenuNumber|annotation-menu-number/u);

function ordered(source, markers, label) {
  let cursor = -1;
  for (const marker of markers) {
    const next = source.indexOf(marker, cursor + 1);
    assert.notEqual(next, -1, `${label} 缺少或顺序错误: ${marker}`);
    cursor = next;
  }
}

assert.match(sources.downloadStorage, /export const BROWSER_DOWNLOAD_DIRECTORY = "browser-downloads"/u);
assert.match(sources.downloadStorage, /lstat\(root\)/u);
assert.match(sources.downloadStorage, /rootStat\.isSymbolicLink\(\)/u);
assert.match(sources.downloadStorage, /rm\(join\(root, entry\)/u);
assert.doesNotMatch(sources.downloadStorage, /browser-uploads/u);

ordered(
  sources.main,
  [
    "const surfaces = new BrowserSurfaceManager({",
    "await surfaces.clearDownloads();",
    "worker = new AutomationWorker({",
  ],
  "启动下载清理",
);
const shutdownStart = sources.main.indexOf("async function shutdown(): Promise<void>");
assert.notEqual(shutdownStart, -1, "退出清理缺少 shutdown");
const shutdownSource = sources.main.slice(shutdownStart);
ordered(
  shutdownSource,
  ["surfaceManager?.closeAll();", "automationWorker?.stop();", "await surfaceManager?.clearDownloads();"],
  "退出下载清理",
);

assert.match(sources.surface, /async clearBrowsingData\(\): Promise<void> \{\s*await this\.clearDownloads\(\)/u);
assert.match(sources.surface, /#downloadCleanupInProgress/u);
assert.match(sources.surface, /this\.cancelActiveDownloads\(\)/u);
assert.match(sources.surface, /item\.cancel\(\)/u);
assert.match(sources.surface, /browser_download_cleanup_in_progress/u);

console.log('browser core acceptance passed');
