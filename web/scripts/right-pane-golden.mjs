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
  assert.match(
    rightPaneSource,
    /sandbox="allow-scripts allow-forms allow-modals"/,
    'HTML preview must stay in a sandbox without allow-same-origin',
  );
  assert.match(rightPaneSource, /agentNavigationUrl\('\/api\/files\/site-open'/);
  assert.match(rightPaneSource, /htmlPreviewRevisions/);
  assert.match(rightPaneSource, /openHtmlInBrowser/);

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

  console.log('right pane golden replay passed');
});
