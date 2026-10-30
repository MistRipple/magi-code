import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';
globalThis.$state = value => value;
globalThis.$derived = value => value;
await withGoldenViteServer(async server => {
  const composer = await server.ssrLoadModule('/src/stores/composer-workspace.svelte.ts');
  composer.syncComposerWorkspaces([{ workspaceId: 'project', rootPath: '/tmp/project', name: 'Project', isActive: true }]);
  assert.equal(composer.resolveComposerWorkspace('', ''), null, '个人草稿不能继承活动项目');
  assert.equal(composer.resolveComposerWorkspace('project', '/tmp/project').workspaceId, 'project');
  const { recentSessionList } = await server.ssrLoadModule('/src/lib/recent-sessions.ts');
  const personal = { id: 'personal', createdAt: 1, updatedAt: 3 };
  const project = { id: 'project-chat', workspaceId: 'project', createdAt: 1, updatedAt: 4 };
  const rows = recentSessionList([personal], { project: [project] });
  assert.deepEqual(rows, [project, personal]);
  assert.equal(rows[1].workspaceId, undefined);
  assert.deepEqual(recentSessionList([personal], {}), [personal], '移除项目不会改变个人会话归属');
  const binding = await server.ssrLoadModule('/src/web/agent-binding-context.ts');
  const api = await server.ssrLoadModule('/src/web/agent-api.ts');
  binding.setAgentBindingContext({ scope: 'workspace', workspaceId: 'project', workspacePath: '/tmp/project', sessionId: 'other' });
  const query = new URLSearchParams(api.buildFilePreviewQuery('report.md', { workspaceId: '', workspacePath: '', sessionId: 'personal' }));
  assert.equal(query.get('sessionId'), 'personal');
  assert.equal(query.has('workspaceId'), false, '个人文件预览不能借用当前项目');
  composer.syncComposerWorkspaces([{
    workspaceId: 'project', rootPath: '/tmp/primary', name: 'Project', isActive: true,
    primarySourceFolderId: 'a', sourceFolders: [
      { sourceFolderId: 'a', path: '/tmp/primary', isPrimary: true },
      { sourceFolderId: 'b', path: '/tmp/secondary', pathRef: 'mhp1:u:L3RtcC9zZWNvbmRhcnk', isPrimary: false },
    ],
  }]);
  for (const path of ['/tmp/secondary', 'mhp1:u:L3RtcC9zZWNvbmRhcnk']) {
    const project = composer.resolveComposerWorkspace('', path);
    assert.equal(project?.workspaceId, 'project', '次目录路径和引用均应识别同一项目');
    assert.equal(project.rootPath, '/tmp/primary', '导航路径不能更改主要执行目录');
  }
  composer.selectComposerSourceFolder('project', 'chat-a', 'b');
  assert.equal(composer.selectedComposerSourceFolderId('project', 'chat-a'), 'b');
  assert.equal(composer.selectedComposerSourceFolderId('project', 'chat-b'), 'a', '目录选择按会话隔离');
  composer.syncComposerWorkspaces([{
    workspaceId: 'project', rootPath: '/tmp/primary', name: 'Project', isActive: true,
    primarySourceFolderId: 'a', sourceFolders: [{ sourceFolderId: 'a', path: '/tmp/primary', isPrimary: true }],
  }]);
  assert.equal(composer.selectedComposerSourceFolderId('project', 'chat-a'), 'a', '移除已选目录后，新输入恢复有效主要目录');
  assert.deepEqual(composer.composerWorkspaceState.sourceFolderSelections, {}, '已撤销目录选择不滞留页面状态');
  composer.selectComposerSourceFolder('project', 'chat-a', 'a');
  composer.syncComposerWorkspaces([]);
  assert.equal(composer.selectedComposerSourceFolderId('project', 'chat-a'), '');
  assert.deepEqual(composer.composerWorkspaceState.sourceFolderSelections, {}, '移除项目同步清理其页面级目录选择');
});
console.log('personal session golden passed');
