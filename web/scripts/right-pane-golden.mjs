import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;

await withGoldenViteServer(async (server) => {
  const rightPane = await server.ssrLoadModule('/src/stores/right-pane.svelte.ts');

  rightPane.activateRightPaneSession('workspace-active', 'session-active');
  rightPane.openCodeTab('session-stale', 'README.md', {
    workspaceId: 'workspace-tree',
    workspacePath: '/tmp/workspace-tree',
    sessionId: '',
  });

  assert.equal(
    rightPane.rightPaneState.activeScopeKey,
    'workspace:workspace-tree',
    'explicit empty session must keep project-file preview in workspace scope',
  );
  const workspacePane = rightPane.getRightPaneState(rightPane.rightPaneState.activeScopeKey);
  const workspaceTab = workspacePane.openTabs.find((tab) => tab.kind === 'code');
  assert.equal(
    workspaceTab?.payload.sessionId,
    undefined,
    'workspace-scoped file preview tab must not persist a stale session id',
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

  console.log('right pane golden replay passed');
});
