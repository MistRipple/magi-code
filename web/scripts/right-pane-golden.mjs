import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;

await withGoldenViteServer(async (server) => {
  const rightPane = await server.ssrLoadModule('/src/stores/right-pane.svelte.ts');
  const filePreview = await server.ssrLoadModule('/src/lib/file-preview-utils.ts');

  assert.equal(filePreview.isHtmlFile('design/index.html'), true);
  assert.equal(filePreview.isHtmlFile('design/index.HTM'), true);
  assert.equal(filePreview.isHtmlFile('design/index.ts'), false);

  rightPane.activateRightPaneSession('', '');
  assert.equal(
    rightPane.rightPaneState.activeScopeKey,
    'personal',
    '个人草稿必须使用稳定的 personal 右栏作用域，即使尚未打开文件或会话',
  );
  const personalPane = rightPane.getRightPaneState('personal');
  assert.equal(personalPane.collapsed, true, '个人草稿右栏初始应为折叠态');
  rightPane.setRightPaneCollapsed('personal', false);
  assert.equal(personalPane.collapsed, false, '个人草稿右栏应可在冷启动时直接展开');
  rightPane.setRightPaneCollapsed('personal', true);

  const rightPaneSource = await readFile(
    new URL('../src/web/RightPane.svelte', import.meta.url),
    'utf8',
  );
  const browserPaneSource = await readFile(
    new URL('../src/components/tabs/BrowserTabContent.svelte', import.meta.url),
    'utf8',
  );
  const rightPaneStoreSource = await readFile(
    new URL('../src/stores/right-pane.svelte.ts', import.meta.url),
    'utf8',
  );
  const appSource = await readFile(
    new URL('../src/App.svelte', import.meta.url),
    'utf8',
  );
  const modalSource = await readFile(
    new URL('../src/components/Modal.svelte', import.meta.url),
    'utf8',
  );
  const overlayShellSource = await readFile(
    new URL('../src/DesktopOverlayShell.svelte', import.meta.url),
    'utf8',
  );
  const overlayContractSource = await readFile(
    new URL('../src/shared/desktop-overlay-contract.ts', import.meta.url),
    'utf8',
  );
  const workbenchSource = await readFile(
    new URL('../src/web/WebWorkbenchShell.svelte', import.meta.url),
    'utf8',
  );
  const overlayManagerSource = await readFile(
    new URL('../../apps/desktop/src/main/desktop-overlay-manager.ts', import.meta.url),
    'utf8',
  );
  const windowManagerSource = await readFile(
    new URL('../../apps/desktop/src/main/window-manager.ts', import.meta.url),
    'utf8',
  );
  await assert.rejects(
    readFile(new URL('../src/lib/native-browser.ts', import.meta.url), 'utf8'),
    { code: 'ENOENT' },
    'retired Tauri/CEF native browser bridge must be deleted',
  );
  const terminalPaneSource = await readFile(
    new URL('../src/components/tabs/TerminalTabContent.svelte', import.meta.url),
    'utf8',
  );
  const markdownLinkSource = await readFile(
    new URL('../src/components/renderers/MdLink.svelte', import.meta.url),
    'utf8',
  );
  assert.doesNotMatch(rightPaneSource, /<iframe\b/, 'HTML preview must not retain the old iframe path');
  assert.match(rightPaneSource, /createBrowserTab\(/);
  assert.match(rightPaneSource, /materializeSession\(/);
  assert.match(
    appSource,
    /async function synchronizeCurrentBrowserAuthority\(revealTabId = ''\): Promise<void> \{\s*if \(!messagesState\.bootstrapped\) return;/,
    'BrowserAuthority 首轮同步必须等待消息状态完成 bootstrap，避免启动阶段把工作区会话误判为 personal',
  );
  assert.match(
    appSource,
    /setDesktopBlockingOverlay\('app-settings', settingsOpen\)/,
    'Settings 必须通过统一阻塞覆盖层契约撤下当前 Browser Surface，而不是依赖局部 z-index',
  );
  assert.match(
    modalSource,
    /setDesktopBlockingOverlay\(overlayId, true\)[\s\S]*?setDesktopBlockingOverlay\(overlayId, false\)/,
    '所有通用 DOM Modal 必须自动加入 Browser Surface 阻塞生命周期',
  );
  assert.match(
    overlayContractSource,
    /activeOverlayIds = new Set[\s\S]*?wasVisible[\s\S]*?notify\(\)[\s\S]*?onDesktopBlockingOverlayChange/,
    '阻塞覆盖层必须使用共享引用集合，支持嵌套弹窗而不提前恢复 Browser Surface',
  );
  assert.match(
    browserPaneSource,
    /onDesktopBlockingOverlayChange\([\s\S]*?browserSurfaceHiddenByOverlay = visible/,
    'App Renderer 必须接收统一阻塞状态，原生 Surface 的显示生命周期由 Main 管理',
  );
  assert.match(
    overlayContractSource,
    /desktop\.setBlockingOverlay\(\{ active: visible \}\)/,
    '阻塞状态必须同步到 Electron Main，不能只修改 Renderer 本地状态',
  );
  assert.match(
    overlayShellSource,
    /window\.focus\(\)[\s\S]*?key !== 'Escape'[\s\S]*?close\(\)/,
    'Desktop Overlay 必须在打开后取得焦点并确定性处理 Escape 关闭',
  );
  assert.match(overlayShellSource, /data-desktop-overlay-root/);
  const browserCapabilityDeclaration = rightPaneSource.slice(
    rightPaneSource.indexOf('const canCreateBrowserPane'),
    rightPaneSource.indexOf('const canCreateTerminalPane'),
  );
  assert.match(
    rightPaneSource,
    /materializeSession\([\s\S]*?navigateSession\([\s\S]*?waitForSessionNavigation\([\s\S]*?createBrowserSession\(/,
    '草稿态创建浏览器必须先实体化 Magi session，再导航并创建 Browser Session',
  );
  assert.doesNotMatch(
    browserCapabilityDeclaration,
    /rightPaneState\.activeSessionId/,
    '浏览器入口不能把已存在 session 作为草稿态可用性的前置条件',
  );
  assert.doesNotMatch(
    rightPaneSource,
    /initialBrowserViewport|automaticBrowserViewport|rightPaneBodyElement/,
    'new BrowserSurfaces must derive their automatic viewport from Electron Main instead of Renderer layout state',
  );
  assert.match(rightPaneSource, /agentUrl\('\/api\/files\/site-open'/);
  assert.match(rightPaneSource, /openHtmlInMagiBrowser/);
  assert.match(rightPaneSource, /class="right-pane-add-tab"/);
  assert.match(rightPaneSource, /class="right-pane-add-menu"/);
  assert.match(rightPaneSource, /addablePaneKinds/);
  assert.match(
    rightPaneSource,
    /activeTabId[\s\S]*?dataset\.tabId === activeTabId[\s\S]*?strip\.scrollLeft/,
    'newly activated panes must scroll their tab into the visible strip',
  );
  assert.match(rightPaneSource, /rightPane\.addPanelBrowser/);
  assert.match(rightPaneSource, /rightPane\.addPanelTerminal/);
  assert.match(
    browserCapabilityDeclaration,
    /desktopSurface[\s\S]*?window\.magiDesktop[\s\S]*?browserCapabilities\?\.inAppBrowserEnabled/,
    'browser pane availability must depend on Electron ownership and the user setting',
  );
  assert.doesNotMatch(
    browserCapabilityDeclaration,
    /nativeBrowserAvailable|hostStatus|runtimeStatus/,
    'BrowserSurface creation must not depend on the retired runtime installer or Worker readiness',
  );
  assert.match(
    rightPaneSource,
    /activeTab\.kind === 'terminal'[\s\S]*?<TerminalTabContent/,
    'terminal panes must render their own command surface rather than reuse browser content',
  );
  assert.doesNotMatch(
    rightPaneSource,
    /bodyActiveTab/,
    'the body must reuse the canonical active-tab projection instead of keeping another cached selection',
  );
  assert.match(
    rightPaneSource,
    /const activeTab = \$derived\.by<[\s\S]*?rightPaneState\.perSession\[scopeKey\][\s\S]*?activeTabId/,
    'the active tab must resolve directly from the root pane state so the tab strip and body cannot diverge',
  );
  assert.match(
    terminalPaneSource,
    /@xterm\/xterm[\s\S]*?FitAddon[\s\S]*?new WebSocket\(terminalChannelUrl/,
    'terminal panes must use xterm with the daemon PTY websocket channel',
  );
  assert.match(
    terminalPaneSource,
    /terminal\.onData[\s\S]*?terminal\.onResize/,
    'terminal panes must forward interactive input and viewport size to the PTY',
  );
  assert.doesNotMatch(
    terminalPaneSource,
    /runTerminalCommand|readTerminalProcess|commandPlaceholder|<textarea/,
    'the retired command-card terminal implementation must not remain in the UI',
  );
assert.doesNotMatch(
  rightPaneSource,
  /onclick=\{\(\) => void createBrowserPane\(\)\}/,
  'the top-level add button must open the extensible pane chooser',
);
assert.doesNotMatch(
  rightPaneSource,
  /if \(!canOpenAddPaneMenu \|\| creatingBrowserPane\) return;/,
  'a pending browser surface must not disable the shared right-pane chooser',
);
assert.match(
  rightPaneSource,
  /disabled=\{!canOpenAddPaneMenu\}[\s\S]*?aria-busy=\{creatingBrowserPane\}/,
  'browser creation may show a local busy indicator without blocking other panel types',
);
assert.match(
  rightPaneSource,
  /function registerBrowserPane\([\s\S]*?openBrowserTab\([\s\S]*?reconcileCreatedBrowserPane\(/,
  'every newly created browser tab must reconcile through the authority snapshot so it becomes the selected tab',
);
assert.doesNotMatch(
  rightPaneSource,
  /if \(!lifecycle \|\| lifecycle === 'creating'\) \{[\s\S]*?reconcileCreatedBrowserPane/,
  'ready browser creation responses must not skip the reveal/reconciliation path',
);
assert.match(
  rightPaneSource,
  /\{#if addPaneMenuOpen && !desktopSurface\}[\s\S]*?right-pane-add-menu[\s\S]*?chooseAddPane\(item\.kind\)/,
  'ordinary Web must keep the extensible pane chooser in the unified App Renderer DOM',
);
assert.match(
  rightPaneSource,
  /onOverlayAction\([\s\S]*?openOverlay\([\s\S]*?placement:\s*'right-pane-add'/,
  'Desktop pane chooser must use the native overlay only for the popup that can cross the Browser Surface',
);
assert.match(
  workbenchSource,
  /class="desktop-right-pane-resize-handle"[\s\S]*?onpointerdown=\{startDesktopRightPaneResize\}[\s\S]*?ondblclick=/,
  'desktop right-pane keeps drag and double-click reset in the unified workbench renderer',
);
assert.match(
  workbenchSource,
  /\.desktop-right-pane-resize-handle\s*\{[\s\S]*?touch-action:\s*none;/,
  'right-pane resize hit area must span the whole divider instead of being limited to the tab bar',
);
assert.match(
  workbenchSource,
  /function startDesktopRightPaneResize\([\s\S]*?window\.addEventListener\('pointermove', move\)[\s\S]*?window\.addEventListener\('pointerup', stop\)/,
  'right-pane resize must keep the pointer stream at window level after leaving the divider hit area',
);
assert.match(
  workbenchSource,
  /\.desktop-right-pane-column :global\(\.right-pane\)\s*\{[\s\S]*?box-sizing:\s*border-box;/,
  'right-pane content must include its border in the allocated track and cannot overflow the outer gutter',
);
assert.match(
  workbenchSource,
  /desktop-right-pane-column[\s\S]*?desktopSurface=\{true\}/,
  'desktop right-pane UI must live in the same renderer as the left and middle panes',
);
assert.match(
  workbenchSource,
  /desktopSnapshot\?\.layout\.rightPaneWidth[\s\S]*?--desktop-right-pane-width/,
  'the unified renderer must consume the right-pane width without measuring the browser surface',
);
assert.match(
  workbenchSource,
  /const effectivePreviewPanelWidth = \$derived\([\s\S]*?desktopSnapshot\?\.layout\.rightPaneWidth[\s\S]*?resolvePanelLayout/,
  'desktop panel coexistence must use the authoritative right-pane width',
);
assert.match(
  workbenchSource,
  /resolvePanelVisibility\([\s\S]*?rightPaneOpen:\s*rightPaneVisible/,
  'desktop right-pane expansion must participate in sidebar coexistence instead of over-constraining the middle pane',
);
assert.doesNotMatch(
  workbenchSource,
  /workbenchBodyElement|getBoundingClientRect\(\)[\s\S]{0,700}?startDesktopRightPaneResize/,
  'right-pane drag bounds must come from the shared panel layout model rather than a second DOM measurement path',
);
assert.doesNotMatch(
  workbenchSource,
  /desktopWorkbenchWidth|bodyResizeObserver|const desktopRightPaneOverlay = \$derived\([\s\S]{0,500}getBoundingClientRect/,
  'desktop overlay mode must come from the Main layout snapshot, not a Renderer feedback loop based on body measurement',
);
assert.match(
  workbenchSource,
  /\.web-workbench-shell--desktop-right-pane-visible \.workbench-body[\s\S]*?var\(--desktop-right-pane-width, 480px\)/,
  'desktop right-pane grid must consume the Main-provided content-track width directly',
);
assert.match(
  workbenchSource,
  /\.web-workbench-shell--desktop-right-pane-visible \.workbench-body[\s\S]*?grid-template-columns:[\s\S]*?minmax\(var\(--workbench-min-content-width, 448px\),\s*1fr\)[\s\S]*?var\(--desktop-right-pane-width, 480px\)/,
  'desktop right-pane grid must preserve the conversation minimum and consume the Main-provided right track',
);
assert.doesNotMatch(
  workbenchSource,
  /minmax\(var\(--workbench-min-content-width, 448px\),\s*minmax\(0,\s*1fr\)\)/,
  'desktop grid tracks must use valid CSS Grid grammar instead of nested minmax expressions',
);
assert.match(
  workbenchSource,
  /desktopSnapshot\?\.layout\.rightPaneMode === 'overlay'[\s\S]*?web-workbench-shell--desktop-preview-overlay[\s\S]*?desktop-right-pane-column--overlay/,
  'desktop overlay mode must be consumed from the authoritative Main layout snapshot',
);
assert.doesNotMatch(
  workbenchSource,
  /--desktop-right-pane-width, 480px\)[\s\S]*?--shell-padding/,
  'desktop right-pane width must not subtract the shell inset a second time',
);
assert.doesNotMatch(
  workbenchSource,
  /\.web-workbench-shell--desktop-right-pane-visible\s*\{\s*padding-right:\s*0;/,
  'desktop layout must keep the shell right outer inset instead of moving the pane to the window edge',
);
assert.match(
  workbenchSource,
  /\.desktop-right-pane-column\s*\{[\s\S]*?width:\s*100%;[\s\S]*?min-width:\s*0;/,
  'right-pane column must fill its grid track without a second padding-based geometry system',
);
assert.match(
  workbenchSource,
  /\.web-workbench-shell--desktop \.sidebar[\s\S]*?background:\s*transparent;[\s\S]*?\.web-workbench-shell--desktop \.workbench-app-pane[\s\S]*?background:\s*transparent;/,
  'desktop three-column surfaces must share the App shell background instead of stacking independent translucent panels',
);
assert.match(
  overlayManagerSource,
  /updateLayout\([\s\S]*?!layout\.rightPaneVisible[\s\S]*?this\.close\(windowId\);[\s\S]*?this\.mountOnLayer\(record\);[\s\S]*?record\.view\.setBounds\(/,
  'overlay layout updates must keep the fixed OverlayLayer while updating bounds',
);
assert.match(
  overlayManagerSource,
  /private mountOnLayer\([\s\S]*?record\.layer\.addChildView\(record\.view\)[\s\S]*?record\.mounted = true/,
  'overlay mounting must add the live WebContentsView only to the fixed OverlayLayer',
);
assert.doesNotMatch(
  overlayManagerSource,
  /contentView\.addChildView|mountOnTop/,
  'overlay updates must not reparent the view to the window root or use the retired z-order workaround',
);
assert.match(
  windowManagerSource,
  /const appLayer = new View\(\);[\s\S]*?const browserLayer = new View\(\);[\s\S]*?const overlayLayer = new View\(\);[\s\S]*?contentView\.addChildView\(appLayer\)[\s\S]*?contentView\.addChildView\(browserLayer\)[\s\S]*?contentView\.addChildView\(overlayLayer\)/,
  'WindowManager must create the App, Browser, and Overlay layers once in stable native order',
);
assert.match(
  windowManagerSource,
  /attachWindow\(windowId, window, browserLayer\)[\s\S]*?overlayManager\.create\(windowId, window, overlayLayer\)/,
  'Browser Surface and Overlay must receive their dedicated native layer instead of sharing contentView',
);
  assert.doesNotMatch(
    browserPaneSource,
    /setBrowserUserControl|toggleControl|control-button/,
    'browser control ownership must be automatic rather than a user-facing mode switch',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /class="browser-tabs"/,
    'a browser pane must not render a nested browser tab strip',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /class="browser-status"/,
    'browser connection state must stay in the toolbar instead of consuming a separate row',
  );
  assert.match(
    browserPaneSource,
    /<form class="address-form"[\s\S]*?<button type="submit" class="address-submit"/,
    'browser address navigation must expose an explicit submit control while retaining Enter submission',
  );
  assert.match(
    browserPaneSource,
    /onkeydown=\{\(event\) => \{[\s\S]*?event\.key !== 'Enter'[\s\S]*?navigate\('url'\)/,
    'browser address navigation must handle Enter without relying on implicit form submission',
  );
  assert.match(
    browserPaneSource,
    /async function refreshSession\(initialLoad = false\)[\s\S]*?if \(initialLoad\) loading = true;/,
    'background authority polling must not project as a foreground loading state',
  );
  assert.match(
    browserPaneSource,
    /const expectedSessionId = browserSessionId\.trim\(\);[\s\S]*?const expectedTabId = tabId\.trim\(\);[\s\S]*?void refreshSession\(true\)/,
    'browser toolbar lifecycle must track only the authority session and tab identity',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /activateBrowserTab/,
    'browser content must not mutate the authority active tab while mounting or polling',
  );
assert.match(
  rightPaneSource,
  /const activationIdentity = `\$\{payload\.browserSessionId\}\\u0000\$\{payload\.tabId\}`;[\s\S]*?const activationKey = `\$\{activationIdentity\}\\u0000\$\{payload\.lifecycle\}`;[\s\S]*?activateBrowserTab\(payload\.tabId\)/,
  'the selected top-level pane must be the single browser activation source while lifecycle remains a state transition, not an identity',
);
assert.doesNotMatch(
  rightPaneSource,
  /nativeSurfaceAlreadyActive|if \(nativeSurfaceAlreadyActive/,
  'Browser activation must not skip Authority activation merely because Main already has a Surface',
);
assert.match(
  workbenchSource,
  /desktopSnapshot\?\.layout\.activePanelKind !== 'browser'[\s\S]*?BrowserTabPayload[\s\S]*?\.tabId === logicalTabId[\s\S]*?setActiveRightPaneTab/,
  'each desktop window must restore its right-pane Browser Tab from its own Main snapshot',
);
assert.doesNotMatch(
  rightPaneStoreSource,
  /activeTabId:\s*snapshot\.activeTabId|revealActiveTab/,
  'BrowserAuthority projection must not expose a global UI active Tab',
);
assert.match(
  rightPaneSource,
  /waitForBrowserTabReady\([\s\S]*?synchronizeBrowserSessionSnapshot\(/,
  'browser creation must reconcile through an authority snapshot instead of depending on one event',
);
assert.match(
  rightPaneSource,
  /function registerBrowserPane\([\s\S]*?openBrowserTab\([\s\S]*?reconcileCreatedBrowserPane\(/,
  'every browser creation entry point must use the same authority reconciliation path',
);
assert.doesNotMatch(
  rightPaneSource,
  /if \(!lifecycle \|\| lifecycle === 'creating'\)[\s\S]*?reconcileCreatedBrowserPane\(/,
  'ready browser creation responses must not skip authority reconciliation',
);
assert.match(
  rightPaneSource,
  /lifecycle: 'crashed'[\s\S]*?权威状态收敛失败/,
  'browser creation timeout must leave an explicit crashed state instead of permanent connecting',
);
assert.match(
  browserPaneSource,
  /synchronizeBrowserSessionSnapshot\(next, workspacePath,\s*\{[\s\S]*?workspaceId[\s\S]*?sessionId/,
  'the active browser tab must project its fetched authority snapshot back into the right-pane lifecycle',
);
assert.match(
  browserPaneSource,
  /const identityKey = `\$\{expectedSessionId\}\\u0000\$\{expectedTabId\}`;[\s\S]*?identityKey === activeBrowserIdentityKey[\s\S]*?activeBrowserIdentityKey = identityKey/,
  'browser metadata refresh must run once per logical browser identity instead of once per projected payload object',
);
assert.match(
  rightPaneStoreSource,
  /function sameBrowserTabPayload\([\s\S]*?left\.browserSessionId === right\.browserSessionId[\s\S]*?left\.sessionId === right\.sessionId/,
  'authority projection must preserve an unchanged browser payload object',
);
assert.match(
  rightPaneStoreSource,
  /const retainedTabs = pane\.openTabs\.filter\([\s\S]*?retainedTabs\.length !== pane\.openTabs\.length[\s\S]*?pane\.openTabs = retainedTabs/,
  'authority projection must preserve the open tab array when no browser tab was removed',
);
assert.match(
  rightPaneSource,
  /if \(payload\.lifecycle === 'creating'\) \{[\s\S]*?activeBrowserActivationRequest \+= 1;[\s\S]*?activeBrowserActivationKey = '';[\s\S]*?return;[\s\S]*?\}/,
  'a logical Browser Tab must finish Host materialization before the UI asks the authority to activate it',
);
assert.match(
  rightPaneSource,
  /activeBrowserActivationKey = `\$\{activationIdentity\}\\u0000\$\{tab\.lifecycle\}`;[\s\S]*?synchronizeBrowserSessionSnapshot\(authority, payload\.workspacePath,[\s\S]*?desktop\.activateBrowser/,
  'the activation response must project the authoritative ready lifecycle before the native surface publishes its content slot',
);
assert.match(
  browserPaneSource,
  /const lifecycleFailure = \$derived\([\s\S]*?activeTab\?\.lifecycle === 'crashed'[\s\S]*?browser\.status\.unavailable/,
  'a crashed browser lifecycle must render an explicit failure state instead of permanent connecting',
);
  assert.match(
    rightPaneSource,
    /activeTab\.kind === 'browser'[\s\S]*?<BrowserTabContent[\s\S]*?browserPayload\.tabId/,
    'the selected top-level browser tab must render exactly one BrowserTabContent slot',
  );
  assert.match(
    rightPaneSource,
    /<BrowserTabContent[\s\S]*?lifecycle=\{browserPayload\.lifecycle\}/,
    'BrowserTabContent must receive the canonical BrowserAuthority lifecycle projection',
  );
assert.match(
  rightPaneSource,
  /\{:else if activeTab\.kind === 'browser'\}[\s\S]*?<BrowserTabContent[\s\S]*?\{:else if activeTab\.kind === 'terminal'\}/,
  'switching to code, image, terminal, or agent tabs must replace only the right-pane body content',
);
  assert.doesNotMatch(
    browserPaneSource,
    /new WebSocket|browserChannelUrl|renderedFrame|queuedFrame|frameImage|drawImage\(|getContext\(['"]2d['"]\)/,
    'browser toolbar must not retain a websocket, bitmap frame, or canvas projection path',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /setBrowserSurfaceBounds|set-browser-surface-bounds/,
    'Browser content must not use the retired independent overlay-bounds bridge',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /@tauri-apps|\binvoke\s*\(|native_browser_|\{\s*x:\s*0,\s*y:\s*0,\s*width:\s*1,\s*height:\s*1\s*\}/,
    'Renderer must not retain Tauri commands, CEF bridge commands, or the retired 1x1 hide path',
  );
assert.match(
  browserPaneSource,
  /class="browser-surface-slot"[\s\S]*?class="browser-native-surface"[\s\S]*?browser\.status\.recordOnly/,
  'Desktop must expose a native Chromium content slot while ordinary Web keeps a read-only Browser record state',
);
assert.match(
  browserPaneSource,
  /\.status-light\s*\{[\s\S]*?background:\s*var\(--foreground-muted\)/,
  'Browser connection status must use a defined theme token in every state',
);
assert.match(
  rightPaneSource,
  /\.browser-control-status\s*\{[\s\S]*?background:\s*var\(--foreground-muted\)/,
  'Browser Tab ownership status must remain visible when the agent has released the tab',
);
  assert.match(
    browserPaneSource,
    /desktopRuntime && !browserReady[\s\S]*?browser-native-surface/,
    'the native Browser Surface must be presented only after the logical tab is ready',
  );
  assert.match(
    browserPaneSource,
    /const browserSurfaceAvailable = \$derived\([\s\S]*?browserSlotHostAvailable[\s\S]*?desktopSnapshot\?\.layout\.activeSurfaceId[\s\S]*?const browserReady = \$derived\(browserSurfaceAvailable && browserSlotPublished\)/,
    'BrowserSurface availability and content-slot mounting must remain separate states',
  );
  const browserSlotPublicationSource = browserPaneSource.slice(
    browserPaneSource.indexOf('function publishBrowserSlotBounds()'),
    browserPaneSource.indexOf('function scheduleBrowserSlotBounds()'),
  );
  assert.ok(
    browserSlotPublicationSource.length > 0,
    'BrowserTabContent must keep an explicit content-slot publication function',
  );
  assert.match(
    browserSlotPublicationSource,
    /if \(!browserSlotHostAvailable \|\| !desktop \|\| !slot \|\| !currentTab \|\| currentTab\.tabId !== tabId\) return;/,
    'initial slot geometry publication must be gated by the active DOM slot host, not by a materialized Chromium Surface',
  );
  const browserSlotInitialPublicationSource = browserSlotPublicationSource.slice(
    0,
    browserSlotPublicationSource.indexOf('      .then((next) => {'),
  );
  assert.doesNotMatch(
    browserSlotInitialPublicationSource,
    /browserSurfaceAvailable|activeSurfaceId/,
    'initial slot geometry publication must not wait for activeSurfaceId; Main receives bounds before showing the Surface',
  );
  const browserSlotHostAvailabilitySource = browserPaneSource.slice(
    browserPaneSource.indexOf('const browserSlotHostAvailable'),
    browserPaneSource.indexOf('const browserSurfaceAvailable'),
  );
  const browserSlotHostExpressionSource = browserSlotHostAvailabilitySource.replace(/\/\/.*$/gm, '');
  assert.doesNotMatch(
    browserSlotHostExpressionSource,
    /activeSurfaceId/,
    'the DOM content-slot host availability contract must remain valid while activeSurfaceId is still null',
  );
  assert.match(
    browserPaneSource,
    /function clearPublishedBrowserSlot\([\s\S]*?slotRevision: revision[\s\S]*?bounds: null/,
    'switching or unmounting a Browser Tab must explicitly clear the previous content slot',
  );
assert.match(
  browserPaneSource,
  /const accepted = next\.layout\.activePanelKind === 'browser'[\s\S]*?next\.layout\.rightPaneVisible[\s\S]*?browserSlotPublished = accepted/,
  'a stale slot response must not make an old Browser Surface visible again',
);
assert.match(
  browserPaneSource,
  /let slotLifecycleGeneration = 0;[\s\S]*?let slotOwnerTabId = '';[\s\S]*?let componentDisposed = false;/,
  'Browser content-slot publication must have an explicit lifecycle generation and disposal state',
);
assert.match(
  browserPaneSource,
  /componentDisposed[\s\S]*?publicationGeneration !== slotLifecycleGeneration[\s\S]*?publicationTabId !== tabId/,
  'a late slot response must be ignored after Browser Tab replacement or component disposal',
);
assert.match(
  browserPaneSource,
  /componentDisposed = true;[\s\S]*?ownedSlotTabId = slotOwnerTabId \|\| publishedSlotTabId[\s\S]*?invalidateBrowserSlotPublication\(\)/,
  'unmount must invalidate pending slot requests even before an acknowledgement populated publishedSlotTabId',
);
  assert.match(
    browserPaneSource,
    /desktopRuntime && !browserReady[\s\S]*?aria-live="polite"[\s\S]*?desktopRuntime && Boolean\(error\)[\s\S]*?role="alert"[\s\S]*?desktopRuntime && browserReady[\s\S]*?browser-native-surface/,
    'the content slot must expose loading and error states before yielding to the native Surface',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /src="about:blank"/,
    'the Browser Tab content slot must not hard-code an about:blank Guest source',
  );
  assert.match(
    browserPaneSource,
    /\.browser-pane\s*\{[\s\S]*?flex:\s*1 1 auto;[\s\S]*?width:\s*100%;[\s\S]*?min-width:\s*0;/,
    'BrowserTabContent must stretch as a normal DOM flex item instead of retaining the webview intrinsic width after right-pane resize',
  );
  assert.match(
    rightPaneSource,
    /\.right-pane-body--browser\s*\{[\s\S]*?display:\s*flex;[\s\S]*?width:\s*100%;/,
    'Browser body must provide a full-width flex layout for its DOM content slot',
  );
  assert.match(
    rightPaneSource,
    /\.right-pane-body--browser\s*\{[\s\S]*?display:\s*flex;[\s\S]*?width:\s*100%;[\s\S]*?overflow:\s*hidden;/,
    'The Browser Tab body must fill and clip only the selected content slot when the right pane is resized',
  );
  assert.match(
    browserPaneSource,
    /class="viewport-menu"[\s\S]*?class="viewport-custom"[\s\S]*?scheduleCustomViewportUpdate\(\)/,
    'browser viewport controls must stay in the current Browser Tab toolbar DOM and update dynamically',
  );
  assert.match(
    browserPaneSource,
    /class="annotation-menu"[\s\S]*?selectSavedAnnotation\(annotation\)/,
    'browser annotation history must stay in the current Browser Tab toolbar DOM',
  );
  assert.match(
    browserPaneSource,
    /openOverlay\([\s\S]*?placement:\s*'browser-viewport'[\s\S]*?openOverlay\([\s\S]*?placement:\s*'browser-annotations'/,
    'Desktop browser toolbar menus must use the native overlay above Chromium content',
  );
  const overlayCalls = [
    ...rightPaneSource.matchAll(/openOverlay\(\{[\s\S]*?\}\)\.catch/g),
    ...browserPaneSource.matchAll(/openOverlay\(\{[\s\S]*?\}\)\.catch/g),
  ].map((match) => match[0]);
  assert.equal(
    overlayCalls.length,
    5,
    'Web 侧必须只有右栏新增、浏览器视口、标记历史、标记选择和标记备注五个 Overlay 调用',
  );
  assert.ok(
    overlayCalls.every((call) => /anchorBounds:\s*(?:null|\{)/.test(call)),
    '每个 openOverlay 调用都必须显式声明 anchorBounds，禁止遗漏位置契约',
  );
  const menuOverlayCalls = overlayCalls.filter((call) => /kind:\s*'menu'/.test(call));
  assert.equal(menuOverlayCalls.length, 3, '三个菜单 Overlay 必须全部经过 DOM 锚点定位');
  assert.ok(
    menuOverlayCalls.every((call) => /anchorBounds:\s*\{[\s\S]*?x:\s*anchor\.left[\s\S]*?y:\s*anchor\.top[\s\S]*?width:\s*anchor\.width[\s\S]*?height:\s*anchor\.height/.test(call)),
    '菜单 Overlay 必须把真实 DOM getBoundingClientRect 结果原样转换为 anchorBounds',
  );
  assert.match(
    rightPaneSource,
    /const anchor = addPaneButtonElement\?\.getBoundingClientRect\(\);[\s\S]*?openOverlay\(\{[\s\S]*?placement:\s*'right-pane-add'[\s\S]*?anchorBounds:\s*\{[\s\S]*?x:\s*anchor\.left[\s\S]*?y:\s*anchor\.top[\s\S]*?width:\s*anchor\.width[\s\S]*?height:\s*anchor\.height/,
    '右栏新增菜单必须锚定顶级右栏新增按钮，不能用固定坐标或浏览器内容槽',
  );
  assert.match(
    browserPaneSource,
    /const anchor = viewportMenuButton\?\.getBoundingClientRect\(\);[\s\S]*?openOverlay\(\{[\s\S]*?placement:\s*'browser-viewport'[\s\S]*?anchorBounds:\s*\{[\s\S]*?x:\s*anchor\.left[\s\S]*?y:\s*anchor\.top[\s\S]*?width:\s*anchor\.width[\s\S]*?height:\s*anchor\.height/,
    '浏览器视口菜单必须锚定当前 Browser Tab 的视口按钮',
  );
  assert.match(
    browserPaneSource,
    /const anchor = annotationHistoryButton\?\.getBoundingClientRect\(\);[\s\S]*?openOverlay\(\{[\s\S]*?placement:\s*'browser-annotations'[\s\S]*?anchorBounds:\s*\{[\s\S]*?x:\s*anchor\.left[\s\S]*?y:\s*anchor\.top[\s\S]*?width:\s*anchor\.width[\s\S]*?height:\s*anchor\.height/,
    '浏览器标记历史菜单必须锚定当前 Browser Tab 的标记按钮',
  );
  const annotationOverlayCalls = overlayCalls.filter((call) => /kind:\s*'annotation'/.test(call));
  assert.equal(annotationOverlayCalls.length, 2, '标记选择和标记备注必须复用同一 Annotation Overlay');
  assert.ok(
    annotationOverlayCalls.every((call) => /placement:\s*'browser-annotations'[\s\S]*?anchorBounds:\s*null/.test(call)),
    '标记 select/note 不得锚定工具栏按钮，必须交给完整浏览器内容槽定位',
  );
  assert.match(
    browserPaneSource,
    /bind:this=\{browserSurfaceSlot\}/,
    '浏览器必须提供唯一的 DOM 内容槽作为原生 Chromium Surface 的宿主区域',
  );
  assert.match(
    browserPaneSource,
    /function publishBrowserSlotBounds\([\s\S]*?const slot = browserSurfaceSlot[\s\S]*?slot\.getBoundingClientRect\(\)[\s\S]*?updateBrowserSlot\([\s\S]*?bounds/,
    '标记 Overlay 的内容槽必须来自当前 Browser Tab 的真实 DOM 槽位边界',
  );
  assert.match(
    overlayManagerSource,
    /if \(state\.kind === "annotation"\)\s*\{[\s\S]*?if \(!browserContentBounds\) throw new Error\("desktop_overlay_browser_content_unavailable"\);[\s\S]*?return \{ \.\.\.browserContentBounds \};/,
    'Main 必须将标记 select/note 的 Overlay 直接铺满完整浏览器内容槽，保持选择和截图坐标系一致',
  );
  assert.match(
    browserPaneSource,
    /onOverlayAction\([\s\S]*?onOverlayClosed\(/,
    'Desktop browser toolbar menus must receive actions and close notifications from the overlay surface',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /placement:\s*'right-pane-add'/,
    'browser toolbar overlays must not take ownership of the unified right-pane chooser',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /\{#if !desktopRuntime && viewportMenuOpen/,
    'ordinary Web entry must not expose Desktop viewport controls or a no-op viewport menu',
  );
  assert.match(
    rightPaneSource,
    /platformCapabilities\.desktopBrowserSurface === true/,
    'browser creation must be gated by the explicit platform capability directory',
  );
  assert.match(
    rightPaneSource,
    /browser\.error\.desktopRequired/,
    'ordinary Web entry must explain that real browser interaction requires Magi Desktop',
  );
  assert.match(
    rightPaneSource,
    /if \(desktopSurface\)[\s\S]*?createBrowserPane\(request\.url\)[\s\S]*?openExternalWebUrl\(externalUrl\)/,
    'ordinary Web links must remain usable by opening in the external browser when no BrowserSurface exists',
  );
  assert.match(
    rightPaneSource,
    /\.\.\.\(canCreateBrowserPane \? \[\{[\s\S]*?kind: 'browser'/,
    'unsupported Web clients must not expose a no-op browser item in the add-pane chooser',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /transform:\s*scale\(|object-fit:\s*(fill|cover)|surfaceWidth|surfaceHeight/,
    'the frontend must not scale, crop, or maintain a second projected browser surface',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /window\.magiDesktop\.(?:activateBrowser|activatePanel|focusBrowser|submitLayoutIntent)/,
    'BrowserTabContent must leave BrowserSurface activation, focus, and layout ownership to RightPane and Electron Main',
  );
  assert.match(
    browserPaneSource,
    /navigateBrowserTab\(tab\.tabId, action,[\s\S]*?await refreshSession\(\)/,
    'browser toolbar navigation must use the authority API that controls the same Electron page',
  );
  assert.match(
    browserPaneSource,
    /const browserSurfaceAvailable = \$derived\([\s\S]*?browserSlotHostAvailable[\s\S]*?desktopSnapshot\?\.layout\.activeSurfaceId[\s\S]*?const browserReady = \$derived\(browserSurfaceAvailable && browserSlotPublished\)/,
    'browser readiness must use the current Main Surface and content-slot state',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /const browserSurfaceAvailable = \$derived\([\s\S]*?lifecycle === 'ready'/,
    'an Authority lifecycle value must not gate the Desktop Surface before the Main snapshot converges',
  );
  assert.match(
    browserPaneSource,
    /const lifecycleFailure = \$derived\(lifecycle === 'crashed' \|\| activeTab\?\.lifecycle === 'crashed'\)/,
    'Authority lifecycle failures must remain visible as an explicit crashed state',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /browserLoading \? i18n\.t\('browser\.status\.connecting'\)/,
    'page loading must not be presented as a BrowserSurface connection failure',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /if \(loading \|\| !browserReady\) return 'connecting'/,
    'metadata refresh must not block an already active BrowserSurface',
  );
  assert.match(
    browserPaneSource,
    /finally \{[\s\S]*?if \(desktopRuntime\) \{[\s\S]*?synchronizeDesktopSurface\(\)/,
    'authority refresh completion must deterministically reconcile the active desktop surface without waiting for stale lifecycle metadata',
  );
  assert.match(
    browserPaneSource,
    /let addressEditing = \$state\(false\)[\s\S]*?if \(initialLoad \|\| !addressEditing\) address = nextUrl/,
    'the address bar must synchronize authority URLs unless the user is actively editing',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /address === lastObservedUrl/,
    'address synchronization must not infer edit state from stale URL equality',
  );
  assert.match(
    browserPaneSource,
    /window\.magiDesktop\?\.onBrowserEvent\(handleDesktopBrowserEvent\)/,
    'browser toolbar must observe Electron page navigation without owning the physical surface',
  );
  assert.match(
    browserPaneSource,
    /const VIEWPORT_DEVICE_MODES = \[[\s\S]*?id: 'wide'[\s\S]*?id: 'narrow'/,
    'browser device emulation must expose only wide and narrow modes',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /id: 'laptop'|id: 'tablet'|viewport-custom-apply|applyCustomViewport/,
    'viewport controls must not retain redundant device presets or a manual apply path',
  );
  assert.match(
    browserPaneSource,
    /CUSTOM_VIEWPORT_DEBOUNCE_MILLIS[\s\S]*?scheduleCustomViewportUpdate\(\)/,
    'custom viewport dimensions must update dynamically through one debounced path',
  );
  assert.match(
    browserPaneSource,
    /pendingViewport = \{ width, height, deviceType: width <= 600 \? 'mobile' : 'desktop' \}/,
    'custom viewport width must determine the canonical wide or narrow device mode',
  );
  assert.match(
    browserPaneSource,
    /desktop\.setBrowserViewport\(\{[\s\S]*?mode === 'auto'[\s\S]*?mode: 'fixed'[\s\S]*?deviceScaleFactorMillis: 1_000/,
    'logical viewport controls must target the current Electron Surface through Main IPC',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /setBrowserTabViewport|action: 'sync'|viewportControllerId|releaseBrowserTabViewportController|setBrowserTabViewportSlot|localStorage|sessionStorage/,
    'viewport UI must not persist through BrowserAuthority or synchronize a physical slot',
  );
  assert.match(
    browserPaneSource,
    /function selectSavedAnnotation[\s\S]*?magi:browserAnnotationCreated/,
    'browser annotations must expose one composer reference event',
  );
  assert.match(
    browserPaneSource,
    /openDesktopAnnotationCreation[\s\S]*?kind: 'annotation'[\s\S]*?phase: 'select'/,
    'Browser Tab 必须提供可达的创建标记入口，并进入通用 annotation selection overlay',
  );
  assert.match(
    browserPaneSource,
    /parseAnnotationSelection[\s\S]*?navigationRevision[\s\S]*?openAnnotationCommentOverlay\(\)/,
    '标记选择必须绑定当前导航版本并进入备注阶段',
  );
  assert.match(
    browserPaneSource,
    /createBrowserAnnotation\(tab\.tabId, selection, comment\)[\s\S]*?magi:browserAnnotationCreated/,
    '标记备注保存必须 POST 权威 annotation，并把结果送入消息引用流程',
  );
  assert.match(
    browserPaneSource,
    /openDesktopAnnotationCreation[\s\S]*?browser\.action\.annotate/,
    '添加标记按钮必须在 Browser Tab 工具栏可见且可操作',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /setNativeBrowserAnnotationMode|listenNativeBrowserAnnotations|createBrowserElementAnnotation|createBrowserRegionAnnotation/,
    'browser toolbar must not retain the retired native annotation bridge or create a second annotation input surface',
  );
  assert.match(
    browserPaneSource,
    /savedAnnotations[\s\S]*?class="annotation-menu"[\s\S]*?selectSavedAnnotation\(annotation\)/,
    'persisted annotation history must remain available to the composer flow',
  );
  assert.match(
    markdownLinkSource,
    /requestOpenUrlInBrowser\(webTarget\)/,
    'conversation web links must prefer the built-in browser',
  );
  assert.match(
    markdownLinkSource,
    /handleOpenExternal[\s\S]*?openExternalWebUrl\(webTarget\)/,
    'conversation web links must retain an explicit external-browser action',
  );
  assert.match(markdownLinkSource, /class="md-link-external"[\s\S]*?onclick=\{handleOpenExternal\}/);
  assert.match(
    rightPaneSource,
    /OPEN_URL_IN_BROWSER_EVENT[\s\S]*?createBrowserPane\(request\.url\)/,
    'built-in link navigation must create one authority-backed browser pane',
  );
  assert.match(
    browserPaneSource,
    /openCurrentPageExternally[\s\S]*?openExternalWebUrl\(externalUrl\)[\s\S]*?browser\.action\.openExternal/,
    'the browser toolbar must expose the current page to the external browser',
  );

  rightPane.activateRightPaneSession('workspace-browser', 'session-browser');
  for (let index = 0; index < 8; index += 1) {
    rightPane.openBrowserTab('browser-session', `browser-tab-${index}`, {
      workspaceId: 'workspace-browser',
      workspacePath: '/tmp/workspace-browser',
      sessionId: 'session-browser',
      label: `网页 ${index + 1}`,
    });
  }
  const browserPane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  assert.deepEqual(
    browserPane.openTabs.map((tab) => tab.id),
    Array.from({ length: 8 }, (_, index) => `browser:browser-session:browser-tab-${index}`),
    'each BrowserTabId must map to one top-level right-pane tab without LRU eviction',
  );
  assert.equal(browserPane.activeTabId, 'browser:browser-session:browser-tab-7');

  rightPane.activateRightPaneSession('workspace-browser-lifecycle', 'session-browser-lifecycle');
  rightPane.openBrowserTab('browser-session-lifecycle', 'browser-tab-lifecycle', {
    workspaceId: 'workspace-browser-lifecycle',
    workspacePath: '/tmp/workspace-browser-lifecycle',
    sessionId: 'session-browser-lifecycle',
    lifecycle: 'ready',
  });
  const lifecyclePane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  assert.equal(
    lifecyclePane.openTabs[0]?.payload.lifecycle,
    'ready',
    '创建响应中的 authority ready 必须直接进入右栏生命周期投影',
  );
  rightPane.openBrowserTab('browser-session-lifecycle', 'browser-tab-lifecycle', {
    workspaceId: 'workspace-browser-lifecycle',
    workspacePath: '/tmp/workspace-browser-lifecycle',
    sessionId: 'session-browser-lifecycle',
  });
  assert.equal(
    lifecyclePane.openTabs[0]?.payload.lifecycle,
    'ready',
    '本地 creating 注册不能覆盖已经到达的 authority ready',
  );
  rightPane.synchronizeBrowserSessionSnapshot({
    browserSessionId: 'browser-session-lifecycle',
    workspaceId: 'authority-workspace-id',
    sessionId: 'session-browser-lifecycle',
    agentOccupied: false,
    tabs: [{
      tabId: 'browser-tab-lifecycle',
      lifecycle: 'ready',
      url: 'https://example.com/lifecycle',
      title: 'Lifecycle page',
    }],
  }, '/tmp/workspace-browser-lifecycle', {
    workspaceId: 'workspace-browser-lifecycle',
    sessionId: 'session-browser-lifecycle',
  });
  assert.equal(
    lifecyclePane.openTabs[0]?.label,
    'Lifecycle page',
    '完整 authority 快照必须同时更新 Browser Tab 标签和生命周期',
  );

  rightPane.activateRightPaneSession('', 'session-personal-browser');
  rightPane.openBrowserTab('browser-session-personal', 'browser-tab-personal', {
    sessionId: 'session-personal-browser',
    label: '个人浏览器',
  });
  assert.equal(
    rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey).openTabs[0]?.id,
    'browser:browser-session-personal:browser-tab-personal',
    '个人会话浏览器 Tab 不应依赖 workspaceId',
  );

  rightPane.activateRightPaneSession('workspace-terminal', 'session-terminal');
  const firstTerminalId = rightPane.openTerminalTab({
    workspaceId: 'workspace-terminal',
    workspacePath: '/tmp/workspace-terminal',
    sessionId: 'session-terminal',
  });
  const secondTerminalId = rightPane.openTerminalTab({
    workspaceId: 'workspace-terminal',
    workspacePath: '/tmp/workspace-terminal',
    sessionId: 'session-terminal',
  });
  assert.ok(firstTerminalId && secondTerminalId && firstTerminalId !== secondTerminalId);
  const terminalScope = rightPane.rightPaneState.activeScopeKey;
  const terminalPane = rightPane.getRightPaneState(terminalScope);
  assert.deepEqual(
    terminalPane.openTabs.map((tab) => tab.id),
    [`terminal:${firstTerminalId}`, `terminal:${secondTerminalId}`],
    'each user-created terminal must keep an independent top-level PTY pane',
  );
  assert.equal(terminalPane.activeTabId, `terminal:${secondTerminalId}`);
  assert.deepEqual(
    terminalPane.openTabs.map((tab) => tab.label),
    ['Terminal', 'Terminal'],
    'terminal tabs must use a stable product label instead of the workspace name',
  );
  rightPane.setActiveRightPaneTab(terminalScope, `terminal:${firstTerminalId}`);
  assert.equal(terminalPane.activeTabId, `terminal:${firstTerminalId}`);
  rightPane.closeTab(terminalScope, `terminal:${firstTerminalId}`);
  assert.deepEqual(
    terminalPane.openTabs.map((tab) => tab.id),
    [`terminal:${secondTerminalId}`],
    'closing one terminal must not affect another terminal tab',
  );

  rightPane.activateRightPaneSession('workspace-browser-sync', 'session-browser-sync');
  rightPane.openCodeTab('session-browser-sync', 'index.html', {
    workspaceId: 'workspace-browser-sync',
    workspacePath: '/tmp/workspace-browser-sync',
    sessionId: 'session-browser-sync',
  });
  const browserSyncScope = rightPane.rightPaneState.activeScopeKey;
  rightPane.synchronizeBrowserTabs(
    'workspace-browser-sync',
    '/tmp/workspace-browser-sync',
    'session-browser-sync',
    {
      browserSessionId: 'browser-session-sync',
      agentOccupied: false,
      tabs: [
        {
          tabId: 'browser-tab-sync-1',
          lifecycle: 'ready',
          url: 'https://example.com/one',
          title: '页面一',
        },
      ],
    },
    { newTabLabel: '新标签页' },
  );
  const browserSyncPane = rightPane.getRightPaneState(browserSyncScope);
  assert.deepEqual(
    browserSyncPane.openTabs.map((tab) => tab.id),
    ['code:index.html', 'browser:browser-session-sync:browser-tab-sync-1'],
    'authority recovery must register one top-level pane per BrowserTabId',
  );
  assert.equal(
    browserSyncPane.activeTabId,
    'code:index.html',
    'background authority recovery must not steal focus from a code pane',
  );

  rightPane.synchronizeBrowserTabs(
    'workspace-browser-sync',
    '/tmp/workspace-browser-sync',
    'session-browser-sync',
    {
      browserSessionId: 'browser-session-sync',
      agentOccupied: true,
      tabs: [
        {
          tabId: 'browser-tab-sync-1',
          lifecycle: 'ready',
          url: 'https://example.com/one',
          title: '页面一',
        },
        {
          tabId: 'browser-tab-sync-2',
          lifecycle: 'ready',
          url: 'about:blank',
          title: '',
        },
      ],
    },
    { revealTabId: 'browser-tab-sync-2', newTabLabel: '新标签页' },
  );
  assert.equal(
    browserSyncPane.activeTabId,
    'browser:browser-session-sync:browser-tab-sync-2',
    'an explicitly created Page must reveal its matching top-level pane',
  );
  assert.equal(
    browserSyncPane.openTabs.find((tab) => tab.id.endsWith('browser-tab-sync-2'))?.label,
    '新标签页',
  );
  assert.equal(
    browserSyncPane.openTabs.find((tab) => tab.id.endsWith('browser-tab-sync-2'))?.payload.agentOccupied,
    true,
    'authority occupancy must be projected into every browser pane without persistence',
  );

  rightPane.synchronizeBrowserTabs(
    'workspace-browser-sync',
    '/tmp/workspace-browser-sync',
    'session-browser-sync',
    {
      browserSessionId: 'browser-session-sync',
      agentOccupied: false,
      tabs: [
        {
          tabId: 'browser-tab-sync-1',
          lifecycle: 'ready',
          url: 'https://example.com/one',
          title: '页面一',
        },
      ],
    },
    { newTabLabel: '新标签页' },
  );
  assert.deepEqual(
    browserSyncPane.openTabs.map((tab) => tab.id),
    ['code:index.html', 'browser:browser-session-sync:browser-tab-sync-1'],
    'a closed authority Page must remove exactly its matching top-level pane',
  );
  assert.equal(
    browserSyncPane.activeTabId,
    'browser:browser-session-sync:browser-tab-sync-1',
    'closing the local active Page must choose a remaining local pane',
  );
  assert.equal(
    browserSyncPane.openTabs.find((tab) => tab.id.endsWith('browser-tab-sync-1'))?.payload.agentOccupied,
    false,
    'a released authority snapshot must clear stale browser occupancy',
  );

  rightPane.activateRightPaneSession('workspace-active', 'session-active');
  rightPane.openCodeTab('session-stale', 'README.md', {
    workspaceId: 'workspace-active',
    workspacePath: '/tmp/workspace-active',
    sessionId: '',
  });

  assert.equal(
    rightPane.rightPaneState.activeScopeKey,
    'workspace-active\u0000session-active',
    'file preview should join the active session pane instead of replacing it with a workspace-only pane',
  );
  const workspacePane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  const workspaceTab = workspacePane.openTabs.find((tab) => tab.kind === 'code');
  assert.equal(
    workspaceTab?.payload.sessionId,
    'session-active',
    'file preview opened from the project tree should inherit the active session scope',
  );

  rightPane.openAgentTab('session-active', 'task-active-agent', {
    workspaceId: 'workspace-active',
    workspacePath: '/tmp/workspace-active',
    label: '当前会话代理',
  });
  const unifiedPane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  assert.deepEqual(
    unifiedPane.openTabs.map((tab) => tab.id),
    ['code:README.md', 'agent:task-active-agent'],
    'agent preview must append to the existing file-preview tab strip',
  );

  rightPane.activateRightPaneSession('workspace-draft', null);
  rightPane.openCodeTab(null, 'draft.md', {
    workspaceId: 'workspace-draft',
    workspacePath: '/tmp/workspace-draft',
    sessionId: '',
  });
  rightPane.activateRightPaneSession('workspace-draft', 'session-created');
  rightPane.openAgentTab('session-created', 'task-created-agent', {
    workspaceId: 'workspace-draft',
    workspacePath: '/tmp/workspace-draft',
    label: '新会话代理',
  });
  const migratedDraftPane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  assert.deepEqual(
    migratedDraftPane.openTabs.map((tab) => tab.id),
    ['code:draft.md', 'agent:task-created-agent'],
    'workspace draft tabs must migrate into the created session before agent tabs append',
  );

  rightPane.openCodeTab('session-edit', 'src/lib.rs', {
    workspaceId: 'workspace-tree',
    workspacePath: '/tmp/workspace-tree',
  });
  assert.equal(
    rightPane.rightPaneState.activeScopeKey,
    'workspace-tree\u0000session-edit',
    'session-bound edit preview must still use workspace/session scope',
  );
  const sessionPane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  const sessionTab = sessionPane.openTabs.find((tab) => tab.kind === 'code');
  assert.equal(sessionTab?.payload.sessionId, 'session-edit');

  rightPane.openAgentTab('session-agent', 'task-agent-1', {
    workspaceId: 'workspace-agent',
    workspacePath: '/tmp/workspace-agent',
    label: '执行代理',
  });
  assert.equal(
    rightPane.rightPaneState.activeScopeKey,
    'workspace-agent\u0000session-agent',
    'agent tab must use explicit workspace/session scope',
  );
  const agentPane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  const agentTab = agentPane.openTabs.find((tab) => tab.kind === 'agent');
  assert.equal(agentTab?.payload.sessionId, 'session-agent');
  assert.equal(agentTab?.payload.workspaceId, 'workspace-agent');
  assert.equal(agentTab?.payload.workspacePath, '/tmp/workspace-agent');

  rightPane.openAgentTab('session-agent', 'task-agent-2', {
    workspaceId: 'workspace-agent',
    workspacePath: '/tmp/workspace-agent',
    label: '审查代理',
    accentToken: '#10b981',
  });
  const parallelAgentPane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  assert.deepEqual(
    parallelAgentPane.openTabs
      .filter((tab) => tab.kind === 'agent')
      .map((tab) => tab.id),
    ['agent:task-agent-1', 'agent:task-agent-2'],
    'parallel agent cards must open incremental taskId tabs in the same session scope',
  );
  assert.equal(
    parallelAgentPane.activeTabId,
    'agent:task-agent-2',
    'clicking the second agent should activate it without replacing the first tab',
  );
  const secondAgentTab = parallelAgentPane.openTabs.find((tab) => tab.id === 'agent:task-agent-2');
  assert.equal(
    secondAgentTab?.accentToken,
    '#10b981',
    'agent tab should preserve the visual accent passed by the spawn card',
  );

  rightPane.openAgentTab('session-agent', 'task-agent-1', {
    workspaceId: 'workspace-other',
    label: '另一个工作区代理',
  });
  assert.equal(
    rightPane.rightPaneState.activeScopeKey,
    'workspace-other\u0000session-agent',
    'same task id in another workspace must stay in a separate right-pane scope',
  );
  const otherAgentPane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  const otherAgentTab = otherAgentPane.openTabs.find((tab) => tab.kind === 'agent');
  assert.equal(otherAgentTab?.payload.workspaceId, 'workspace-other');

  rightPane.activateRightPaneSession('workspace-collapse', 'session-collapse');
  rightPane.openCodeTab('session-collapse', 'README.md', {
    workspaceId: 'workspace-collapse',
    workspacePath: '/tmp/workspace-collapse',
  });
  const collapseScope = rightPane.rightPaneState.activeScopeKey;
  const collapsePane = rightPane.getRightPaneState(collapseScope);
  const collapseTabId = collapsePane.activeTabId;
  rightPane.setRightPaneCollapsed(collapseScope, true);
  assert.equal(
    rightPane.getRightPaneState(collapseScope),
    collapsePane,
    'right-pane session state must keep a stable reactive reference for shell visibility consumers',
  );
  assert.equal(collapsePane.collapsed, true, 'explicit collapse must close the surface');
  assert.equal(collapsePane.openTabs.length, 1, 'explicit collapse must preserve open tabs');
  rightPane.setRightPaneCollapsed(collapseScope, false);
  assert.equal(collapsePane.collapsed, false, 'explicit expand must restore the preserved surface');
  assert.equal(collapsePane.activeTabId, collapseTabId, 'explicit expand must preserve the active tab');
  rightPane.closeTab(collapseScope, collapseTabId);
  assert.equal(collapsePane.openTabs.length, 0, 'closing the final tab must empty the pane');
  assert.equal(collapsePane.collapsed, true, 'closing the final tab must collapse the pane');

  rightPane.activateRightPaneSession('workspace-changes', 'session-changes');
  const alphaRevision = rightPane.changeDiffRevision({
    filePath: 'alpha.txt',
    snapshotId: 'snapshot-alpha',
    updatedAt: 1,
  });
  rightPane.openCodeTab('session-changes', 'alpha.txt', {
    workspaceId: 'workspace-changes',
    sessionId: 'session-changes',
    isChangeDiff: true,
    changeRevision: alphaRevision,
    diff: '@@ -1 +1 @@\n-alpha-v1\n+alpha-v2',
    originalContent: 'alpha-v1\n',
    currentContent: 'alpha-v2\n',
  });
  rightPane.openCodeTab('session-changes', 'beta.txt', {
    workspaceId: 'workspace-changes',
    sessionId: 'session-changes',
    isChangeDiff: true,
    changeRevision: rightPane.changeDiffRevision({
      filePath: 'beta.txt',
      snapshotId: 'snapshot-beta',
      updatedAt: 1,
    }),
  });
  rightPane.openCodeTab('session-changes', 'README.md', {
    workspaceId: 'workspace-changes',
    sessionId: 'session-changes',
    content: 'README',
  });
  rightPane.synchronizeChangeDiffTabs('workspace-changes', 'session-changes', [
    {
      filePath: 'alpha.txt',
      snapshotId: 'snapshot-alpha',
      updatedAt: 2,
      contentKind: 'text',
      size: 9,
    },
  ]);
  const synchronizedPane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  assert.deepEqual(
    synchronizedPane.openTabs.map((tab) => tab.id),
    ['code:alpha.txt', 'code:README.md'],
    '变更投影移除文件后必须关闭对应 diff tab，同时保留普通文件预览',
  );
  const synchronizedAlpha = synchronizedPane.openTabs.find((tab) => tab.id === 'code:alpha.txt');
  assert.equal(
    synchronizedAlpha?.payload.changeRevision,
    'snapshot-alpha:2',
    '仍在变更集中的 diff tab 必须更新到最新投影版本',
  );
  assert.equal(
    Object.prototype.hasOwnProperty.call(synchronizedAlpha?.payload ?? {}, 'diff'),
    false,
    '投影版本变化后必须清除旧 diff 内容并重新读取',
  );
  assert.equal(
    synchronizedPane.openTabs.find((tab) => tab.id === 'code:README.md')?.payload.content,
    'README',
    '普通文件预览不得被变更投影同步影响',
  );

  console.log('right pane golden replay passed');
});
