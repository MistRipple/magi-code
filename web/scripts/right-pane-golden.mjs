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

  rightPane.activateRightPaneSession('workspace-restore-golden', 'session-restore-golden');
  rightPane.synchronizeBrowserTabs(
    'workspace-restore-golden',
    '/tmp/workspace-restore-golden',
    'session-restore-golden',
    {
      browserSessionId: 'browser-session-restore-golden',
      agentOccupied: false,
      tabs: [{
        tabId: 'browser-tab-restore-golden',
        lifecycle: 'ready',
        url: 'https://example.com/',
        title: 'Example',
        navigationRevision: 1,
      }],
    },
  );
  const restoredPane = rightPane.getRightPaneState('workspace-restore-golden\u0000session-restore-golden');
  assert.equal(restoredPane.collapsed, false, '首次恢复权威 Browser Tab 必须展开右栏');
  assert.equal(restoredPane.openTabs.length, 1, '首次恢复权威 Browser Tab 必须投影到右栏');
  assert.equal(restoredPane.activeTabId, restoredPane.openTabs[0].id, '首次恢复必须激活权威 Browser Tab');

  const [rightPaneSource, browserPaneSource, appSource, modalSource, overlayContractSource,
    overlayShellSource, overlayManagerSource, windowManagerSource, surfaceManagerSource,
    layoutSource, desktopTypesSource, terminalPaneSource, workbenchShellSource] = await Promise.all([
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
    read('../src/web/WebWorkbenchShell.svelte'),
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
  // publishes native bounds, so there is no browser-surface ResizeObserver ->
  // IPC feedback loop. The local ResizeObserver used for menu reflow is a
  // separate overlay concern and must remain allowed.
  assert.match(browserPaneSource, /class="browser-surface-slot"/u);
  assert.match(browserPaneSource, /class="browser-native-surface"/u);
  assert.doesNotMatch(browserPaneSource, /browserSurfaceSlot|updateBrowserSlot|bindContentSurface/u);
  const browserPaneResizeObserverBlock = browserPaneSource.match(
    /new ResizeObserver\([\s\S]*?(?=\n\s*const windowResize)/u,
  )?.[0] ?? '';
  assert.doesNotMatch(
    browserPaneResizeObserverBlock,
    /browserSurface|browserSlot|rendererGeometry|renderer_geometry/u,
    'Browser Tab 的 ResizeObserver 不得参与原生 Surface 几何或 Renderer 几何上报',
  );
  assert.doesNotMatch(browserPaneSource, /transform:\s*scale\(|object-fit:\s*(fill|cover)|surfaceWidth|surfaceHeight/u);
  assert.match(browserPaneSource, /desktop\.setBrowserViewport\(/u);
  assert.match(browserPaneSource, /const browserSurfaceAvailable = \$derived\([\s\S]*?activeSurfaceId[\s\S]*?browserContentSlot\?\.tabId === tabId/u);
  assert.doesNotMatch(browserPaneSource, /lifecycle === 'ready'/u);
  assert.match(browserPaneSource, /VIEWPORT_DEVICE_MODES = \[[\s\S]*?id: 'wide'[\s\S]*?id: 'narrow'/u);
  assert.match(browserPaneSource, /scheduleCustomViewportUpdate\(\)/u);
  assert.match(browserPaneSource, /customViewportWidthInput = \$state\('390'\)[\s\S]*?customViewportHeightInput = \$state\('844'\)/u);
  assert.match(browserPaneSource, /customViewportWidthEditing = \$state\(false\)[\s\S]*?customViewportHeightEditing = \$state\(false\)[\s\S]*?customViewportInputDirty = \$state\(false\)/u);
  assert.match(browserPaneSource, /function parseViewportDimension\([\s\S]*?raw\.trim\(\)[\s\S]*?\/\^\\d\+\$\/u[\s\S]*?Number\(normalized\)/u);
  // 空值和逐字符输入只保留原始字符串；只有完整且合法的宽高对才会提交。
  assert.match(browserPaneSource, /function parsedCustomViewport\([\s\S]*?parseViewportDimension\(customViewportWidthInput[\s\S]*?parseViewportDimension\(customViewportHeightInput[\s\S]*?if \(width === null \|\| height === null\) return null;/u);
  // 非法范围必须被解析器拒绝，不能把 0 或部分输入发给 Desktop IPC。
  assert.match(browserPaneSource, /VIEWPORT_DIMENSION_LIMITS = \{[\s\S]*?width: \{ min: 320, max: 7680 \}[\s\S]*?height: \{ min: 240, max: 4320 \}/u);
  assert.match(browserPaneSource, /function normalizeCustomViewportInputs\([\s\S]*?\?\? localViewport\.width[\s\S]*?\?\? localViewport\.height[\s\S]*?customViewportWidthInput = String\(width\)[\s\S]*?customViewportHeightInput = String\(height\)/u);
  assert.match(browserPaneSource, /function handleCustomViewportInput\([\s\S]*?input\.value[\s\S]*?scheduleCustomViewportUpdate\(\)/u);
  assert.doesNotMatch(browserPaneSource, /oninput=\{\([^)]*\) => \{[\s\S]*?Number\(\(event\.currentTarget as HTMLInputElement\)\.value\)/u);
  assert.match(browserPaneSource, /onfocus=\{\(\) => \{ customViewportWidthEditing = true; \}\}[\s\S]*?oninput=\{\(event\) => handleCustomViewportInput\('width', event\)\}[\s\S]*?onblur=\{handleCustomViewportBlur\}/u);
  assert.match(browserPaneSource, /onfocus=\{\(\) => \{ customViewportHeightEditing = true; \}\}[\s\S]*?oninput=\{\(event\) => handleCustomViewportInput\('height', event\)\}[\s\S]*?onblur=\{handleCustomViewportBlur\}/u);
  assert.match(browserPaneSource, /function handleCustomViewportBlur\(event: FocusEvent\)[\s\S]*?relatedTarget[\s\S]*?viewportMenuElement\?\.contains[\s\S]*?\+\+viewportMutationGeneration[\s\S]*?normalizeCustomViewportInputs\(\)[\s\S]*?scheduleCustomViewportCommit\(viewport, 0\)/u);
  assert.match(browserPaneSource, /function useAutomaticViewport\(\)[\s\S]*?cancelPendingCustomViewport\(\)[\s\S]*?normalizeCustomViewportInputs\(\)[\s\S]*?updateLogicalViewport\('auto'\)/u);
  assert.match(browserPaneSource, /function useFixedViewport\([\s\S]*?cancelPendingCustomViewport\(\)[\s\S]*?customViewportWidthInput = String\(width\)[\s\S]*?customViewportHeightInput = String\(height\)/u);
  assert.match(browserPaneSource, /let viewportMutationGeneration = 0[\s\S]*?const mutationGeneration = \+\+viewportMutationGeneration[\s\S]*?if \(mutationGeneration !== viewportMutationGeneration\) return;/u);
  assert.match(browserPaneSource, /action\.id === 'selection'[\s\S]*?openAnnotationCommentOverlay\(\)/u);
  assert.match(browserPaneSource, /onOverlayAction\([\s\S]*?onOverlayClosed\(/u);
  // Browser Overlay 的关闭身份是不可拆分的事务键。旧 Tab/session 的迟到
  // 事件不能仅凭 ownerId 重新激活或清理新浮层。
  assert.match(browserPaneSource, /interface BrowserOverlayIdentity[\s\S]*?overlayId: string;[\s\S]*?ownerId: string;/u);
  assert.match(browserPaneSource, /let desktopOverlayIdentity = \$state<BrowserOverlayIdentity \| null>\(null\)/u);
  assert.match(browserPaneSource, /function nextBrowserOverlayId\([\s\S]*?crypto\.randomUUID\(\)[\s\S]*?return `browser-\$\{kind\}-\$\{token\}`/u);
  assert.doesNotMatch(
    browserPaneSource,
    /\b(?:let|const|var)\s+desktopOverlayId\s*=/u,
    'Browser Tab 不得保留只按 overlayId 管理的旧状态',
  );
  assert.match(browserPaneSource, /function enqueueDesktopOverlayOperation\([\s\S]*?desktopOverlayOperations = next\.catch\(\(\) => undefined\)/u);
  assert.match(browserPaneSource, /function closeDesktopOverlay\([\s\S]*?expected: BrowserOverlayIdentity \| null = desktopOverlayIdentity[\s\S]*?desktop\.closeOverlay\(toDesktopOverlayIdentity\(pending\.identity\)\)[\s\S]*?await pending\.confirmation/u);
  assert.match(browserPaneSource, /function confirmDesktopOverlayClosed\([\s\S]*?sameOverlayIdentity\(pending\.identity, event\)[\s\S]*?pending\.resolve\(\)/u);
  assert.match(browserPaneSource, /function openDesktopOverlay\([\s\S]*?const previousUi = captureOverlayUi\(\)[\s\S]*?await pendingClose\.confirmation[\s\S]*?await desktop\.openOverlay\(state\)/u);
  assert.match(browserPaneSource, /if \(waitsForClose && closeConfirmed\) clearDesktopOverlayUi\(nextIdentity\)[\s\S]*?else restoreOverlayUi\(previousUi\)/u);
  assert.match(browserPaneSource, /const previousOverlay = desktopOverlayIdentity[\s\S]*?closeDesktopOverlay\(previousOverlay, \{ silent: true \}\)[\s\S]*?clearDesktopOverlayUi\(previousOverlay\)/u);
  assert.match(browserPaneSource, /desktopOverlayClosing[\s\S]*?action\.overlayId !== desktopOverlayIdentity\.overlayId[\s\S]*?action\.ownerId !== desktopOverlayIdentity\.ownerId/u);
  assert.match(browserPaneSource, /state\.overlayId !== desktopOverlayIdentity\.overlayId[\s\S]*?state\.ownerId !== desktopOverlayIdentity\.ownerId/u);
  assert.match(browserPaneSource, /event\.preventDefault\(\)[\s\S]*?event\.stopPropagation\(\)[\s\S]*?window\.addEventListener\('keydown', keyboard, true\)/u);
  assert.match(browserPaneSource, /window\.removeEventListener\('keydown', keyboard, true\)/u);
  assert.match(
    browserPaneSource,
    /\{#if annotationMenuOpen && !desktopRuntime\}[\s\S]*?class="annotation-history-popover"/u,
    '标记记录必须是浏览器容器内的浮层，不能占用内容排版空间',
  );
  assert.match(
    browserPaneSource,
    /\.viewport-popover,\s*\.annotation-history-popover \{[^}]*position:\s*absolute;[^}]*pointer-events:\s*auto/u,
    '标记记录浮层必须脱离文档流并保留自身命中区域',
  );
  assert.match(
    browserPaneSource,
    /viewportMenuElement[\s\S]*?annotationMenuElement[\s\S]*?viewportMenuButton[\s\S]*?annotationHistoryButton/u,
    '浮层打开状态下，工具栏按钮自身不能被 outside-pointer 逻辑误关后立即重开',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /class="browser-tool-menu-row" data-menu="annotations"/u,
    '标记记录不能继续使用会撑开浏览器内容的文档流菜单行',
  );

  // Main owns the only layout and native geometry transaction.
  assert.match(layoutSource, /export function browserContentBounds\(/u);
  assert.match(layoutSource, /browserContentSlot/u);
  assert.match(layoutSource, /rendererGeometry: RendererGeometryFrame \| null/u);
  assert.match(windowManagerSource, /private applyLayout\(record: DesktopWindowRecord\)/u);
  assert.match(
    windowManagerSource,
    /const browserSurfaceActive = !record\.blockingOverlayActive[\s\S]*?const rendererGeometryForActiveSurface = browserSurfaceActive[\s\S]*?layout\.rendererGeometry\?\.browserContentSlot\?\.tabId === layout\.activeTabId[\s\S]*?const currentBrowserContentBounds = browserSurfaceActive && rendererGeometryForActiveSurface[\s\S]*?browserContentBounds\(layout\)/u,
    '原生浏览器只能绑定到当前 Browser Tab/Surface 已确认的 Renderer 内容槽',
  );
  assert.match(windowManagerSource, /this\.#surfaceManager\.bindContentSurface\(/u);
  assert.match(windowManagerSource, /setViewBounds\(record\.appView, layout\.appBounds/u);
  assert.doesNotMatch(windowManagerSource, /updateBrowserSlot/u);
  assert.match(workbenchShellSource, /getBoundingClientRect\(\)/u);
  assert.match(workbenchShellSource, /type: 'renderer_geometry'/u);
  assert.match(workbenchShellSource, /new ResizeObserver/u);
  assert.doesNotMatch(workbenchShellSource, /rightPaneTabBarHeight|browserToolbarHeight/u);
  assert.match(surfaceManagerSource, /contentRoot\.addChildView\(record\.view, 1\)/u);
  assert.match(
    surfaceManagerSource,
    /bindContentSurface\([\s\S]*?bounds: Rectangle \| null,[\s\S]*?\): void \{/u,
    '原生 Surface 绑定只接受右栏内容槽，不再接受 clipBounds 兼容参数',
  );
  assert.doesNotMatch(surfaceManagerSource, /clipBounds|intersectRect|leaseBounds/u);
  assert.match(surfaceManagerSource, /record\.view\.setBounds\(effectiveBounds\)/u);
  assert.match(surfaceManagerSource, /record\.view\.setVisible\(true\)/u);
  assert.match(surfaceManagerSource, /did-fail-load[\s\S]*?this\.applySlot\(/u);
  const failedLoadHandler = surfaceManagerSource.match(
    /webContents\.on\("did-fail-load"[\s\S]*?\n\s*\}\);\n\s*webContents\.on\("before-input-event"/u,
  )?.[0];
  assert.ok(failedLoadHandler, 'did-fail-load handler should remain explicit');
  assert.doesNotMatch(failedLoadHandler, /this\.unmountSurface\(/u);
  assert.match(surfaceManagerSource, /private unmountSurface\([\s\S]*?只解绑原生 View，不关闭 WebContents/u);
  assert.match(surfaceManagerSource, /private async waitForDebugger\(/u);
  assert.match(surfaceManagerSource, /debugger-detached:[\s\S]*?reconnectDebugger/u);
  assert.match(surfaceManagerSource, /method === "Page\.captureScreenshot"/u);
  assert.match(
    surfaceManagerSource,
    /method === "Page\.captureScreenshot"[\s\S]*?sendSurfaceCdpCommand\([\s\S]*?sendCdpCommandWithTimeout\([\s\S]*?method/u,
    '截图必须沿用当前 Browser Surface 的 CDP lane，并由真实 WebContents 执行',
  );
  assert.match(surfaceManagerSource, /method === "Page\.captureScreenshot"[\s\S]*?fromSurface: true/u);
  assert.doesNotMatch(surfaceManagerSource, /capturePage\(|capturePageRect|mapBrowserCaptureClipToNativeRect/u);
  assert.doesNotMatch(surfaceManagerSource, /startScreencast|drawImage\(/u);

  // 右栏激活只有 Workbench Shell 一个所有者。RightPane 只记录用户 Tab
  // 意图，BrowserTabContent 只渲染工具栏和原生内容槽，不能再并发写 Main IPC。
  assert.doesNotMatch(rightPaneSource, /activatePanel\(|activateBrowser\(/u);
  assert.doesNotMatch(browserPaneSource, /activateBrowser\(/u);
  // 新增面板菜单 Web 模式是 DOM 浮层；Desktop 模式进入原生 Overlay。
  assert.match(rightPaneSource, /let domAddPaneMenuOpen = \$state\(false\)/u);
  assert.match(rightPaneSource, /const addPaneMenuOpen = \$derived\(domAddPaneMenuOpen\)/u);
  assert.match(rightPaneSource, /window\.addEventListener\('pointerdown', handleOutsidePointer, true\)/u);
  assert.match(rightPaneSource, /window\.addEventListener\('keydown', handleEscape, true\)/u);
  assert.match(rightPaneSource, /event\.preventDefault\(\)[\s\S]*?event\.stopPropagation\(\)/u);
  assert.match(rightPaneSource, /openDesktopAddPaneMenu\(\)/u);
  assert.match(rightPaneSource, /closeDesktopAddPaneOverlay\(\)/u);
  assert.match(workbenchShellSource, /currentDesktopPanelTarget\(\)[\s\S]*?kind: 'browser'/u);
  assert.match(workbenchShellSource, /desktopPanelActivationRequest[\s\S]*?activateDesktopPanelTarget\(/u);
  assert.match(workbenchShellSource, /desktop\.activateBrowser\([\s\S]*?desktop\.activatePanel\(/u);
  assert.match(
    workbenchShellSource,
    /decideDesktopPanelActivation\([\s\S]*?desktopPanelActivationRequest !== null[\s\S]*?activationDecision === 'wait_for_in_flight'/u,
  );
  assert.match(rightPaneSource, /kind === 'browser'|kind: 'browser'/u);
  assert.match(terminalPaneSource, /class="terminal-pane"|class='terminal-pane'/u);
  assert.match(desktopTypesSource, /activatePanel\(request: \{ kind: MagiDesktopPanelKind/u);
  assert.doesNotMatch(desktopTypesSource, /updateBrowserSlot|browserSlot/u);

  // All browser popups that sit above Chromium use the same bounded overlay
  // contract on Desktop. The non-Desktop path is the absolute DOM equivalent.
  assert.match(overlayManagerSource, /private setOverlayBounds\([\s\S]*?record\.view\.setBounds\(bounds\)/u);
  assert.match(overlayManagerSource, /TRANSPARENT_VIEW_BACKGROUND/u);
  assert.match(overlayManagerSource, /const geometry = resolveCurrentOverlayGeometry\(state, layout\)/u);
  assert.match(overlayManagerSource, /resolveCurrentOverlayGeometry\(record\.state, layout\)/u);
  assert.match(overlayManagerSource, /const frame = layout\.rendererGeometry[\s\S]*?const parent = frame\?\.rightPaneBounds/u);
  assert.match(
    windowManagerSource,
    /const currentBrowserParentBounds = browserSurfaceActive && rendererGeometryForActiveSurface[\s\S]*?rendererGeometryForActiveSurface\.rightPaneBounds/u,
  );
  assert.match(overlayManagerSource, /record\.geometryAvailable = false;[\s\S]*?clearOverlayBounds\(record\)/u);
  assert.match(workbenchShellSource, /getBoundingClientRect\(\)/u);
  assert.match(workbenchShellSource, /new ResizeObserver/u);
  // Desktop 的所有浏览器内浮层都收敛到同一个 Overlay Renderer；Web 没有
  // 原生 Browser Surface，继续走 DOM 浮层。
  assert.match(rightPaneSource, /\{#if addPaneMenuOpen && !desktopSurface\}/u);
  assert.match(browserPaneSource, /\{#if viewportMenuOpen && !desktopRuntime\}[\s\S]*?class="viewport-popover"/u);
  assert.match(overlayManagerSource, /export type DesktopOverlayKind = "menu" \| "annotation";/u);
  assert.doesNotMatch(overlayManagerSource, /overlayAnchors|widthLimit|itemHeight/u);
  assert.doesNotMatch(browserPaneSource, /setDesktopBlockingOverlay\(key, open\)/u);
  assert.doesNotMatch(rightPaneSource, /setDesktopBlockingOverlay\(key, addPaneMenuOpen\)/u);

  assert.doesNotMatch(
    `${rightPaneSource}\n${browserPaneSource}\n${windowManagerSource}\n${surfaceManagerSource}`,
    /@tauri-apps|native_browser_|setNativeBrowserAnnotationMode|renderedFrame|frameImage/u,
  );

  console.log('right pane golden replay passed');
});
