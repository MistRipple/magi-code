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
assert.match(
  sources.surface,
  /session\.fromPartition\(record\.partitionId,\s*\{\s*cache:\s*false,?\s*\}\)/u,
);
assert.match(sources.surface, /primaryBindingForTab\(/u);
assert.match(sources.surface, /setWindowOpenHandler\(\(details\) =>/u);
assert.match(sources.surface, /type: "popup_blocked"/u);
assert.doesNotMatch(sources.surface, /new\s+WebContentsView\(/u);
assert.doesNotMatch(sources.surface, /\.setBounds\(/u);
assert.doesNotMatch(sources.surface, /\.addChildView\(/u);

assert.match(sources.surface, /Page\.captureScreenshot/u);
assert.match(sources.surface, /DOM\.getNodeForLocation/u);
assert.match(sources.surface, /Overlay\.highlightNode/u);
assert.match(sources.surface, /contents\.disableDeviceEmulation\(\)/u);
assert.match(sources.surface, /contents\.enableDeviceEmulation\(\{/u);
assert.match(sources.worker, /case\s+['"]screenshot['"][\s\S]*Page\.captureScreenshot/u);
assert.match(sources.worker, /DOM\.getDocument|Accessibility\.getFullAXTree/u);

assert.match(sources.window, /attachWindow\(windowId, appView\.webContents\)/u);
assert.match(sources.window, /setViewBounds\(record\.appView, layout\.appBounds/u);
assert.doesNotMatch(sources.window, /bindContentSurface|browserContentBounds|browserContentSlot|rendererGeometry/u);
assert.match(sources.main, /handleIpc\(\s*"magi-desktop:register-browser-webview"/u);
assert.match(
  sources.preload,
  /registerBrowserWebview:\s*\([^)]*\)\s*=>[\s\S]*?magi-desktop:register-browser-webview/u,
);
assert.match(sources.schema, /magi-desktop:register-browser-webview/u);

assert.match(sources.browserTab, /VIEWPORT_DEVICE_MODES[\s\S]*id: 'wide'[\s\S]*id: 'narrow'/u);
assert.match(sources.browserTab, /useAutomaticViewport\(\)/u);
assert.match(sources.browserTab, /CUSTOM_VIEWPORT_DEBOUNCE_MILLIS/u);
assert.match(sources.browserTab, /webpreferences="focusOnNavigation=no"/u);
assert.match(sources.browserTab, /magi:browserScreenshotCaptured/u);
assert.match(sources.browserTab, /magi:browserNodeSelected/u);
assert.match(sources.browserTab, /annotationMenuNumber|annotation-menu-number/u);

console.log('browser core acceptance passed');
