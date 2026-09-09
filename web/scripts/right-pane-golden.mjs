import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const read = (path) => readFile(new URL(path, import.meta.url), 'utf8');
globalThis.$state = (value) => value;

await withGoldenViteServer(async (server) => {
  const rightPane = await server.ssrLoadModule('/src/stores/right-pane.svelte.ts');
  rightPane.activateRightPaneSession('', '');
  assert.equal(rightPane.rightPaneState.activeScopeKey, 'personal');

  rightPane.synchronizeBrowserTabs(
    'workspace-golden',
    '/tmp/workspace-golden',
    'session-golden',
    {
      browserSessionId: 'browser-session-golden',
      revision: 1,
      agentOccupied: false,
      activeTabId: 'browser-tab-golden',
      tabs: [{
        tabId: 'browser-tab-golden',
        lifecycle: 'ready',
        url: 'https://example.com/',
        title: 'Example',
        navigationRevision: 1,
      }, {
        tabId: 'browser-tab-second',
        lifecycle: 'ready',
        url: 'https://example.org/',
        title: 'Example Org',
        navigationRevision: 1,
      }],
    },
  );
  const pane = rightPane.getRightPaneState('workspace-golden\u0000session-golden');
  assert.equal(pane.openTabs.length, 2);
  assert.equal(pane.activeTabId, 'browser:browser-session-golden:browser-tab-golden');
  assert.equal((pane.openTabs[0].payload).url, 'https://example.com/');

  rightPane.synchronizeBrowserTabs(
    'workspace-golden',
    '/tmp/workspace-golden',
    'session-golden',
    {
      browserSessionId: 'browser-session-golden',
      revision: 2,
      agentOccupied: false,
      activeTabId: 'browser-tab-second',
      tabs: [{
        tabId: 'browser-tab-golden',
        lifecycle: 'ready',
        url: 'https://example.com/',
        title: 'Example',
        navigationRevision: 1,
      }, {
        tabId: 'browser-tab-second',
        lifecycle: 'ready',
        url: 'https://example.org/',
        title: 'Example Org',
        navigationRevision: 1,
      }],
    },
  );
  assert.equal(pane.activeTabId, 'browser:browser-session-golden:browser-tab-second');

  rightPane.synchronizeBrowserTabs(
    'workspace-golden',
    '/tmp/workspace-golden',
    'session-golden',
    {
      browserSessionId: 'browser-session-golden',
      revision: 0,
      agentOccupied: false,
      activeTabId: 'browser-tab-golden',
      tabs: [{
        tabId: 'browser-tab-golden',
        lifecycle: 'ready',
        url: 'https://stale.example/',
        title: 'Stale',
        navigationRevision: 0,
      }],
    },
  );
  assert.equal((pane.openTabs[0].payload).url, 'https://example.com/');
  assert.equal(pane.activeTabId, 'browser:browser-session-golden:browser-tab-second');

  const [storeSource, shellSource, rightPaneSource, browserSource, terminalSource] = await Promise.all([
    read('../src/stores/right-pane.svelte.ts'),
    read('../src/web/WebWorkbenchShell.svelte'),
    read('../src/web/RightPane.svelte'),
    read('../src/components/tabs/BrowserTabContent.svelte'),
    read('../src/components/tabs/TerminalTabContent.svelte'),
  ]);

  const removedGeometry = /browserContentSlot|rendererGeometry|renderer_geometry|bindContentSurface|browserContentBounds/u;
  for (const [name, source] of Object.entries({ storeSource, shellSource, rightPaneSource, browserSource })) {
    assert.doesNotMatch(source, removedGeometry, `${name} 不得依赖旧浏览器几何协议`);
  }
  assert.match(shellSource, /<RightPaneComponent[\s\S]*desktopSurface=\{true\}/u);
  assert.match(
    storeSource,
    /if \(active\) \{[\s\S]*rememberDesktopPanelIntent\(scopeKey, active\);/u,
    'revealTabId 必须记录 Desktop 激活意图，避免旧 Main 快照覆盖目标 Tab',
  );
  assert.match(shellSource, /desktop-right-pane-column/u);
  assert.match(rightPaneSource, /class="right-pane-tabbar"/u);
  assert.match(rightPaneSource, /class="right-pane-body"/u);
  assert.match(rightPaneSource, /class="right-pane-browser-tab-host"[\s\S]*hidden=\{/u);
  assert.match(rightPaneSource, /class="right-pane-add-menu-row"[\s\S]*popover="auto"[\s\S]*ontoggle=\{handleAddPaneMenuToggle\}/u);
  assert.doesNotMatch(rightPaneSource, /addPaneMenuOpen[\s\S]*window\.addEventListener\('pointerdown'/u);
  assert.match(rightPaneSource, /\.right-pane-add-menu-row \{[\s\S]*position-anchor: --right-pane-add-anchor/u);
  assert.match(rightPaneSource, /\.right-pane-add-tab \{[\s\S]*anchor-name: --right-pane-add-anchor/u);
  assert.match(rightPaneSource, /<BrowserTabContent[\s\S]*desktopSurface=\{desktopSurface\}/u);
  assert.match(rightPaneSource, /<AgentTabContent/u);
  assert.match(rightPaneSource, /<TerminalTabContent/u);
  assert.match(rightPaneSource, /class="right-pane-image-wrap"/u);
  assert.match(terminalSource, /class="terminal-pane"/u);

  assert.match(browserSource, /<webview[\s\S]*src="about:blank"[\s\S]*allowpopups=\{false\}/u);
  assert.match(browserSource, /\.browser-toolbar \{[\s\S]*z-index: 10[\s\S]*isolation: isolate/u);
  assert.match(
    browserSource,
    /\.browser-toolbar-tooltip \{[\s\S]*position-anchor: --browser-tooltip-anchor[\s\S]*top: calc\(anchor\(bottom\) \+ 5px\);[\s\S]*transform: translateX\(-50%\)/u,
    '浏览器工具提示必须由 Renderer Top Layer 锚定在工具栏下方',
  );
  for (const action of [
    'browser\.viewport\.control',
    'browser\.action\.screenshot',
    'browser\.action\.annotate',
    'browser\.annotation\.history',
  ]) {
    assert.match(
      browserSource,
      new RegExp(`data-tooltip=\\{i18n\\.t\\('${action}'\\)\\}`),
      `浏览器工具栏必须为 ${action} 提供统一 tooltip`,
    );
  }
  assert.doesNotMatch(browserSource, /\.toolbar-edge-button\[data-tooltip\]::after/u);
  assert.match(browserSource, /\.browser-surface-slot \{[\s\S]*display: flex;[\s\S]*flex: 1;[\s\S]*overflow: hidden;/u);
  assert.match(browserSource, /\.browser-webview \{[\s\S]*width: 100%;[\s\S]*height: 100%;/u);
  assert.match(browserSource, /\{#if viewportMenuOpen\}[\s\S]*class="viewport-popover"/u);
  assert.match(browserSource, /\{#if annotationMenuOpen\}[\s\S]*class="annotation-history-popover"/u);
  assert.match(browserSource, /VIEWPORT_DEVICE_MODES[\s\S]*id: 'wide'[\s\S]*id: 'narrow'/u);
  assert.match(browserSource, /CUSTOM_VIEWPORT_DEBOUNCE_MILLIS/u);
  assert.match(browserSource, /magi:browserScreenshotCaptured/u);
  assert.match(browserSource, /magi:browserNodeSelected/u);
  assert.doesNotMatch(browserSource, /transform:\s*scale\(|object-fit:\s*(?:fill|cover)/u);

  console.log('right pane golden replay passed');
});
