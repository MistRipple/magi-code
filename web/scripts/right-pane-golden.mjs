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

  const rightPaneSource = await readFile(
    new URL('../src/web/RightPane.svelte', import.meta.url),
    'utf8',
  );
  const browserPaneSource = await readFile(
    new URL('../src/components/tabs/BrowserTabContent.svelte', import.meta.url),
    'utf8',
  );
  const markdownLinkSource = await readFile(
    new URL('../src/components/renderers/MdLink.svelte', import.meta.url),
    'utf8',
  );
  assert.doesNotMatch(rightPaneSource, /<iframe\b/, 'HTML preview must not retain the old iframe path');
  assert.match(rightPaneSource, /createBrowserTab\(/);
  assert.match(rightPaneSource, /agentUrl\('\/api\/files\/site-open'/);
  assert.match(rightPaneSource, /openHtmlInMagiBrowser/);
  assert.match(rightPaneSource, /class="right-pane-add-tab"/);
  assert.match(rightPaneSource, /class="right-pane-add-menu"/);
  assert.match(rightPaneSource, /addablePaneKinds/);
  assert.match(rightPaneSource, /rightPane\.addPanelBrowser/);
  assert.doesNotMatch(
    rightPaneSource,
    /onclick=\{\(\) => void createBrowserPane\(\)\}/,
    'the top-level add button must open the extensible pane chooser',
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
    /nextSnapshot\.lifecycle !== 'ready' \|\| nextTab\?\.lifecycle !== 'ready'[\s\S]*?disconnectChannel\(\)/,
    'the frame channel must only exist while both session and tab authority are ready',
  );
  assert.match(
    browserPaneSource,
    /canvas\.width = image\.width;[\s\S]*?canvas\.height = image\.height;/,
    'the canvas backing store must preserve the encoded high-DPI frame dimensions',
  );
  assert.match(
    browserPaneSource,
    /queuedFrame = \{ bytes, metadata \};[\s\S]*?if \(!frameDecoderActive\) void drainFrameQueue/,
    'high-DPI frame decoding must coalesce pending frames instead of creating parallel decoders',
  );
  assert.match(
    browserPaneSource,
    /while \(generation === frameDecoderGeneration\)[\s\S]*?const frame = queuedFrame;[\s\S]*?queuedFrame = null;/,
    'the frame decoder must consume only the latest queued frame for the active channel generation',
  );
  assert.match(
    browserPaneSource,
    /const scale = Math\.min\(\s*1,/,
    'a stale browser frame must never be enlarged beyond its native CSS viewport',
  );
  assert.match(
    browserPaneSource,
    /document\.visibilityState === 'visible' && document\.hasFocus\(\)/,
    'only the foreground Magi window may control a shared BrowserTab viewport',
  );
  assert.match(
    browserPaneSource,
    /controllerId: viewportControllerId,[\s\S]*?claim,/,
    'panel viewport sync must carry a stable controller identity and explicit takeover flag',
  );
  assert.match(
    browserPaneSource,
    /magi:browserViewportControllerClaim/,
    'manual panel resizing must reclaim the single viewport writer',
  );
  assert.match(
    browserPaneSource,
    /window\.addEventListener\('focus', reclaimViewportControl\)[\s\S]*?document\.addEventListener\('visibilitychange', reclaimViewportControl\)/,
    'a Magi window must reclaim its BrowserTab viewport when it becomes active',
  );
  assert.match(
    browserPaneSource,
    /Math\.min\(frame\.width,[\s\S]*?frame\.width \/ rect\.width/,
    'browser input coordinates must remain in CSS viewport units when the canvas backing store is high-DPI',
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
    /CUSTOM_VIEWPORT_DEBOUNCE_MILLIS[\s\S]*?oninput=\{scheduleCustomViewportUpdate\}/,
    'custom viewport dimensions must update dynamically through one debounced path',
  );
  assert.match(
    browserPaneSource,
    /function deviceTypeForWidth\(width: number\)[\s\S]*?width <= 600 \? 'mobile' : 'desktop'[\s\S]*?customViewportDeviceType = deviceType/,
    'custom viewport width must determine the canonical wide or narrow device mode',
  );
  assert.doesNotMatch(
    browserPaneSource,
    /annotations: candidate\.annotations\.map[\s\S]*?status: 'stale'/,
    'resizing the browser pane must not invalidate annotations on the same document',
  );
  assert.match(
    browserPaneSource,
    /function selectSavedAnnotation[\s\S]*?magi:browserAnnotationCreated[\s\S]*?onclick=\{\(\) => selectSavedAnnotation\(annotation\)\}/,
    'clicking a saved annotation must select it for the composer instead of silently resolving it',
  );
  assert.match(
    browserPaneSource,
    /\.browser-viewport\.marking \.annotation-marker \{ pointer-events: none; \}/,
    'saved annotations must not intercept region selection while annotation mode is active',
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
    /openCurrentPageExternally[\s\S]*?openExternalWebUrl\(url\)[\s\S]*?browser\.action\.openExternal/,
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
      activeTabId: 'browser-tab-sync-1',
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
      activeTabId: 'browser-tab-sync-2',
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
    { revealActiveTab: true, newTabLabel: '新标签页' },
  );
  assert.equal(
    browserSyncPane.activeTabId,
    'browser:browser-session-sync:browser-tab-sync-2',
    'a newly created authority Page must reveal its matching top-level pane',
  );
  assert.equal(
    browserSyncPane.openTabs.find((tab) => tab.id.endsWith('browser-tab-sync-2'))?.label,
    '新标签页',
  );

  rightPane.synchronizeBrowserTabs(
    'workspace-browser-sync',
    '/tmp/workspace-browser-sync',
    'session-browser-sync',
    {
      browserSessionId: 'browser-session-sync',
      activeTabId: 'browser-tab-sync-1',
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
    'closing the active Page must follow BrowserAuthority activeTabId',
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
