import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const read = (path) => readFile(new URL(path, import.meta.url), 'utf8');
const [store, shell, rightPane, browserTab, surfaceManager, windowManager, preload, main, schema] = await Promise.all([
  read('../src/stores/right-pane.svelte.ts'),
  read('../src/web/WebWorkbenchShell.svelte'),
  read('../src/web/RightPane.svelte'),
  read('../src/components/tabs/BrowserTabContent.svelte'),
  read('../../apps/desktop/src/main/browser-surface-manager.ts'),
  read('../../apps/desktop/src/main/window-manager.ts'),
  read('../../apps/desktop/src/preload/index.ts'),
  read('../../apps/desktop/src/main/index.ts'),
  read('../../contracts/desktop-browser/desktop-ipc.schema.json'),
]);

const removed = /rendererGeometry|renderer_geometry|browserContentSlot|bindContentSurface|contentBoundsForTab/u;
for (const [name, source] of Object.entries({ store, shell, rightPane, browserTab, surfaceManager, windowManager, preload, main, schema })) {
  assert.doesNotMatch(source, removed, `${name} 不得保留已删除的浏览器几何通道`);
}

assert.match(store, /upsertTab\(/u);
assert.match(shell, /readyRightPane\(\)/u);
assert.match(shell, /desktopPanelActivationRequest/u);
assert.match(shell, /decideDesktopPanelActivation\(/u);
const chooseAddPane = rightPane.match(/async function chooseAddPane\([\s\S]*?\n  \}/u)?.[0] ?? '';
assert.match(chooseAddPane, /kind === 'browser'/u);
assert.match(chooseAddPane, /createBrowserPane\(\)/u);
assert.match(chooseAddPane, /createTerminalPane\(\)/u);
assert.match(rightPane, /class="right-pane-browser-tab-host"/u);
assert.match(rightPane, /class="right-pane-body"/u);
assert.match(browserTab, /<webview[\s\S]*allowpopups=\{false\}/u);
assert.match(browserTab, /webpreferences="focusOnNavigation=no"/u);
assert.match(browserTab, /function toggleViewportMenu\(\)/u);
assert.match(browserTab, /function toggleAnnotationMenu\(\)/u);
assert.match(browserTab, /\{#if viewportMenuOpen\}/u);
assert.match(browserTab, /\{#if annotationMenuOpen\}/u);
assert.match(browserTab, /data-tooltip=/u);
assert.match(browserTab, /\.browser-toolbar \{[\s\S]*z-index: 10[\s\S]*isolation: isolate/u);
assert.match(browserTab, /\.browser-toolbar-tooltip \{[\s\S]*position-anchor: --browser-tooltip-anchor[\s\S]*top: calc\(anchor\(bottom\) \+ 5px\);[\s\S]*transform: translateX\(-50%\)/u);
assert.doesNotMatch(browserTab, /\.icon-button\[data-tooltip\]::after/u);

assert.match(surfaceManager, /registerEmbeddedWebview\(/u);
assert.match(surfaceManager, /webContents\.fromId\(input\.webContentsId\)/u);
assert.match(surfaceManager, /guest\.getType\(\) !== "webview"/u);
assert.match(surfaceManager, /guest\.hostWebContents !== host/u);
assert.match(
  surfaceManager,
  /session\.fromPartition\(record\.partitionId,\s*\{\s*cache:\s*false,?\s*\}\)/u,
);
assert.match(surfaceManager, /setWindowOpenHandler\(\(details\) =>/u);
assert.match(surfaceManager, /type: "popup_blocked"/u);
assert.match(surfaceManager, /method === "Page\.captureScreenshot"/u);
assert.match(surfaceManager, /DOM\.getNodeForLocation/u);
assert.match(surfaceManager, /Overlay\.highlightNode/u);
assert.match(surfaceManager, /contents\.disableDeviceEmulation\(\)/u);
assert.match(surfaceManager, /contents\.enableDeviceEmulation\(\{/u);
assert.doesNotMatch(surfaceManager, /new\s+WebContentsView\(/u);
assert.doesNotMatch(surfaceManager, /\.setBounds\(/u);
assert.doesNotMatch(surfaceManager, /\.addChildView\(/u);

assert.match(windowManager, /this\.#surfaceManager\.attachWindow\(windowId, appView\.webContents\)/u);
assert.match(windowManager, /setViewBounds\(record\.appView, layout\.appBounds/u);
assert.doesNotMatch(windowManager, /bindContentSurface|browserContentBounds|browserContentSlot|rendererGeometry/u);
assert.match(
  preload,
  /registerBrowserWebview:\s*\([^)]*\)\s*=>[\s\S]*?magi-desktop:register-browser-webview/u,
);
assert.match(main, /handleIpc\(\s*"magi-desktop:register-browser-webview"/u);
assert.match(schema, /magi-desktop:register-browser-webview/u);
assert.doesNotMatch(schema, /open-overlay|close-overlay|overlay-ready|overlay-action|set-blocking-overlay/u);

console.log('desktop right-pane intent golden passed');
