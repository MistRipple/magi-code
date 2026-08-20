import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const read = (path) => readFile(new URL(path, import.meta.url), 'utf8');
globalThis.$state = (value) => value;

await withGoldenViteServer(async (server) => {
  const rightPane = await server.ssrLoadModule('/src/stores/right-pane.svelte.ts');
  const filePreview = await server.ssrLoadModule('/src/lib/file-preview-utils.ts');

  assert.equal(filePreview.isHtmlFile('design/index.html'), true);
  assert.equal(filePreview.isHtmlFile('design/index.HTM'), true);
  assert.equal(filePreview.isHtmlFile('design/index.ts'), false);

  rightPane.activateRightPaneSession('', '');
  assert.equal(rightPane.rightPaneState.activeScopeKey, 'personal');
  const personalPane = rightPane.getRightPaneState('personal');
  assert.equal(personalPane.collapsed, true);
  rightPane.setRightPaneCollapsed('personal', false);
  assert.equal(personalPane.collapsed, false);
  rightPane.setRightPaneCollapsed('personal', true);

  const [rightPaneSource, browserPaneSource, appSource, modalSource, overlayContractSource,
    overlayShellSource, overlayManagerSource, windowManagerSource, surfaceManagerSource,
    layoutSource, desktopTypesSource, terminalPaneSource] = await Promise.all([
    read('../src/web/RightPane.svelte'),
    read('../src/components/tabs/BrowserTabContent.svelte'),
    read('../src/App.svelte'),
    read('../src/components/Modal.svelte'),
    read('../src/shared/desktop-overlay-contract.ts'),
    read('../src/DesktopOverlayShell.svelte'),
    read('../../apps/desktop/src/main/desktop-overlay-manager.ts'),
    read('../../apps/desktop/src/main/window-manager.ts'),
    read('../../apps/desktop/src/main/browser-surface-manager.ts'),
    read('../../apps/desktop/src/main/window-layout.ts'),
    read('../../web/src/types/magi-desktop.d.ts'),
    read('../src/components/tabs/TerminalTabContent.svelte'),
  ]);

  assert.doesNotMatch(rightPaneSource, /<iframe\b/u);
  assert.match(rightPaneSource, /createBrowserTab\(/u);
  assert.match(rightPaneSource, /materializeSession\(/u);
  assert.match(appSource, /setDesktopBlockingOverlay\('app-settings', settingsOpen\)/u);
  assert.match(modalSource, /setDesktopBlockingOverlay\(overlayId, true\)[\s\S]*?setDesktopBlockingOverlay\(overlayId, false\)/u);
  assert.match(overlayContractSource, /activeOverlayIds = new Set[\s\S]*?onDesktopBlockingOverlayChange/u);
  assert.match(overlayContractSource, /desktop\.setBlockingOverlay\(\{ active: visible \}\)/u);
  assert.match(overlayShellSource, /data-desktop-overlay-root/u);

  // Browser Tab is only the DOM chrome/placeholder. It never measures or
  // publishes native bounds, so there is no ResizeObserver -> IPC feedback loop.
  assert.match(browserPaneSource, /class="browser-surface-slot"/u);
  assert.match(browserPaneSource, /class="browser-native-surface"/u);
  assert.doesNotMatch(browserPaneSource, /ResizeObserver|browserSurfaceSlot|updateBrowserSlot|bindContentSurface/u);
  assert.doesNotMatch(browserPaneSource, /transform:\s*scale\(|object-fit:\s*(fill|cover)|surfaceWidth|surfaceHeight/u);
  assert.match(browserPaneSource, /desktop\.setBrowserViewport\(/u);
  assert.match(browserPaneSource, /const browserSurfaceAvailable = \$derived\([\s\S]*?activeSurfaceId/u);
  assert.doesNotMatch(browserPaneSource, /lifecycle === 'ready'/u);
  assert.match(browserPaneSource, /VIEWPORT_DEVICE_MODES = \[[\s\S]*?id: 'wide'[\s\S]*?id: 'narrow'/u);
  assert.match(browserPaneSource, /scheduleCustomViewportUpdate\(\)/u);
  assert.match(browserPaneSource, /action\.id === 'selection'[\s\S]*?openAnnotationCommentOverlay\(\)/u);
  assert.match(browserPaneSource, /onOverlayAction\([\s\S]*?onOverlayClosed\(/u);

  // Main owns the only layout and native geometry transaction.
  assert.match(layoutSource, /export function browserContentBounds\(/u);
  assert.match(layoutSource, /rightPaneTabBarHeight/u);
  assert.match(layoutSource, /browserToolbarHeight/u);
  assert.match(windowManagerSource, /private applyLayout\(record: DesktopWindowRecord\)/u);
  assert.match(windowManagerSource, /const currentBrowserContentBounds = showBrowserSurface \? browserContentBounds\(layout\) : null/u);
  assert.match(windowManagerSource, /this\.#surfaceManager\.bindContentSurface\(/u);
  assert.match(windowManagerSource, /record\.appView\.setBounds\(layout\.appBounds/u);
  assert.doesNotMatch(windowManagerSource, /updateBrowserSlot/u);
  assert.match(surfaceManagerSource, /record\.host\.addChildView\(record\.view, 0\)/u);
  assert.match(surfaceManagerSource, /bindContentSurface\(windowId: string, tabId: string, bounds: Rectangle \| null\)/u);
  assert.match(surfaceManagerSource, /const localBounds = \{ x: 0, y: 0, width: bounds\.width, height: bounds\.height \}/u);
  assert.match(surfaceManagerSource, /record\.view\.setBounds\(localBounds\)/u);
  assert.match(surfaceManagerSource, /record\.view\.setVisible\(true\)/u);
  assert.match(surfaceManagerSource, /did-fail-load[\s\S]*?this\.applySlot\(/u);
  const failedLoadHandler = surfaceManagerSource.match(
    /webContents\.on\("did-fail-load"[\s\S]*?\n\s*\}\);\n\s*webContents\.on\("did-finish-load"/u,
  )?.[0];
  assert.ok(failedLoadHandler, 'did-fail-load handler should remain explicit');
  assert.doesNotMatch(failedLoadHandler, /this\.unmountSurface\(/u);
  assert.match(surfaceManagerSource, /private unmountSurface\([\s\S]*?只解绑原生 View，不关闭 WebContents/u);
  assert.match(surfaceManagerSource, /private async waitForDebugger\(/u);
  assert.match(surfaceManagerSource, /debugger-detached:[\s\S]*?reconnectDebugger/u);
  assert.match(surfaceManagerSource, /method === "Page\.captureScreenshot"/u);
  assert.match(surfaceManagerSource, /sendCdpCommandWithTimeout\([\s\S]*?Page\.captureScreenshot/u);
  assert.doesNotMatch(surfaceManagerSource, /record\.contents\.capturePage\(|stayHidden:\s*true/u);
  assert.doesNotMatch(surfaceManagerSource, /startScreencast|drawImage\(/u);

  // The browser remains a peer panel: terminal and future right-pane tabs
  // keep the same shared body and do not depend on browser activation.
  assert.match(rightPaneSource, /activatePanel\(/u);
  assert.match(rightPaneSource, /kind === 'browser'|kind: 'browser'/u);
  assert.match(terminalPaneSource, /class="terminal-pane"|class='terminal-pane'/u);
  assert.match(desktopTypesSource, /activatePanel\(request: \{ kind: MagiDesktopPanelKind/u);
  assert.doesNotMatch(desktopTypesSource, /updateBrowserSlot|browserSlot/u);

  // Native overlay menus remain above Chromium, while ordinary right-pane DOM
  // remains the owner of the toolbar and panel frame.
  assert.match(overlayManagerSource, /private setOverlayBounds\([\s\S]*?record\.layer\.setBounds\(bounds\)/u);
  assert.match(overlayManagerSource, /TRANSPARENT_VIEW_BACKGROUND/u);
  assert.match(overlayManagerSource, /state\.kind === "annotation"[\s\S]*?browserContentBounds/u);
  assert.match(rightPaneSource, /getBoundingClientRect\(\)/u);
  assert.match(browserPaneSource, /getBoundingClientRect\(\)/u);

  assert.doesNotMatch(
    `${rightPaneSource}\n${browserPaneSource}\n${windowManagerSource}\n${surfaceManagerSource}`,
    /@tauri-apps|native_browser_|setNativeBrowserAnnotationMode|renderedFrame|frameImage/u,
  );

  console.log('right pane golden replay passed');
});
