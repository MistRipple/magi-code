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
assert.match(store, /isDesktopRenderer\(\) \? window\.sessionStorage : window\.localStorage/u);
assert.match(shell, /readyRightPane\(\)/u);
assert.match(shell, /desktopPanelActivationRequest/u);
assert.match(
  shell,
  /function abandonSupersededDesktopPanelActivation\([\s\S]*?sameDesktopPanelTarget\(currentDesktopPanelTarget\(\), request\)[\s\S]*?desktopPanelActivationRequest = null;[\s\S]*?desktopPanelActivationEpoch \+= 1;/u,
  '过期右栏激活请求必须只收口自己的单飞槽',
);
assert.match(
  shell,
  /await loadBrowserAuthorityTab\([\s\S]*?if \(abandonSupersededDesktopPanelActivation\(request\)\) return;[\s\S]*?await desktop\.activateBrowser/u,
  '读取 Authority 后必须在物化 Surface 前淘汰过期激活请求',
);
assert.match(
  shell,
  /\(authoritySnapshot\) => \{[\s\S]*?if \(!sameDesktopPanelTarget\(currentDesktopPanelTarget\(\), request\)\) return;[\s\S]*?revealTabId: browser\.tabId/u,
  '旧 Authority 回调不得用 revealTabId 覆盖更新的 Tab 意图',
);
assert.match(shell, /type HtmlBrowserOpenRequest = \{[\s\S]*workspaceId: string;[\s\S]*workspacePath: string;[\s\S]*sessionId: string;/u);
assert.match(rightPane, /openHtmlInMagiBrowser\(request\)/u);
assert.match(shell, /decideDesktopPanelActivation\(/u);
const chooseAddPane = rightPane.match(/async function chooseAddPane\([\s\S]*?\n  \}/u)?.[0] ?? '';
assert.match(chooseAddPane, /kind === 'browser'/u);
assert.match(chooseAddPane, /createBrowserPane\(\)/u);
assert.match(chooseAddPane, /createTerminalPane\(\)/u);
assert.match(rightPane, /class="right-pane-browser-tab-host"/u);
assert.match(rightPane, /class="right-pane-body"/u);
assert.match(rightPane, /\.right-pane-add-menu-row \{[\s\S]*left: calc\(anchor\(right\) - 6px\);[\s\S]*transform: translateX\(-100%\)/u);
assert.match(browserTab, /<webview[\s\S]*allowpopups=\{false\}/u);
assert.match(browserTab, /webpreferences="focusOnNavigation=no"/u);
assert.match(browserTab, /function toggleViewportMenu\(\)/u);
assert.match(browserTab, /function toggleAnnotationMenu\(\)/u);
assert.match(browserTab, /event\.type === 'popup_blocked'[\s\S]*popupBlockedMessage/u);
assert.match(browserTab, /class="browser-action-error"[\s\S]*popover="manual"/u);
assert.match(browserTab, /\{#if viewportMenuOpen\}/u);
assert.match(browserTab, /\{#if annotationMenuOpen\}/u);
assert.match(browserTab, /data-tooltip=/u);
assert.match(browserTab, /\.browser-toolbar \{[\s\S]*z-index: 10[\s\S]*isolation: isolate/u);
assert.match(browserTab, /class="browser-toolbar-tooltip"[\s\S]*style:position-anchor=\{tooltipAnchorName\}/u);
assert.match(browserTab, /\.browser-toolbar-tooltip \{[\s\S]*top: calc\(anchor\(bottom\) \+ 5px\);[\s\S]*left: anchor\(right\);[\s\S]*transform: translateX\(-100%\)/u);
assert.match(browserTab, /`\$\{tooltipAnchorPreviousValue\}, \$\{tooltipAnchorName\}`/u);
assert.match(browserTab, /tooltipAnchorElement\.style\.setProperty\([\s\S]*?tooltipAnchorPreviousValue,[\s\S]*?tooltipAnchorPreviousPriority/u);
assert.match(browserTab, /style:position-anchor=\{viewportAnchorName\}/u);
assert.match(browserTab, /style:position-anchor=\{annotationHistoryAnchorName\}/u);
assert.match(browserTab, /\.annotation-history-popover \{[\s\S]*?left: anchor\(right\);[\s\S]*?transform: translateX\(-100%\)/u);
assert.doesNotMatch(browserTab, /right:\s*calc\(100vw\s*-\s*anchor\(right\)\)/u);
assert.match(browserTab, /\.annotation-editor \{[\s\S]*?top: calc\(anchor\(bottom\) - 12px\);[\s\S]*?transform: translateY\(-100%\)/u);
assert.doesNotMatch(browserTab, /100v[hw]\s*-\s*anchor\(/u);
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
assert.match(surfaceManager, /"Emulation\.clearDeviceMetricsOverride"/u);
assert.match(surfaceManager, /"Emulation\.setDeviceMetricsOverride"/u);
assert.doesNotMatch(surfaceManager, /\.enableDeviceEmulation\(|\.disableDeviceEmulation\(/u);
assert.doesNotMatch(surfaceManager, /new\s+WebContentsView\(/u);
assert.doesNotMatch(surfaceManager, /\.setBounds\(/u);
assert.doesNotMatch(surfaceManager, /\.addChildView\(/u);

assert.match(windowManager, /new\s+BrowserWindow\(/u);
assert.match(windowManager, /this\.#surfaceManager\.attachWindow\(windowId, window\.webContents\)/u);
assert.doesNotMatch(windowManager, /\bBaseWindow\b|\bWebContentsView\b|\bappView\b|\.addChildView\(/u);
assert.doesNotMatch(windowManager, /bindContentSurface|browserContentBounds|browserContentSlot|rendererGeometry/u);
assert.match(
  preload,
  /registerBrowserWebview:\s*\([^)]*\)\s*=>[\s\S]*?magi-desktop:register-browser-webview/u,
);
assert.match(main, /handleIpc\(\s*"magi-desktop:register-browser-webview"/u);
assert.match(main, /handleIpc\(\s*"magi-desktop:update-browser-display-size"/u);
assert.match(schema, /magi-desktop:register-browser-webview/u);
assert.match(schema, /magi-desktop:update-browser-display-size/u);
assert.doesNotMatch(schema, /open-overlay|close-overlay|overlay-ready|overlay-action|set-blocking-overlay/u);

console.log('desktop right-pane intent golden passed');
