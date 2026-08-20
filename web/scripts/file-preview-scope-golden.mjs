import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;

await withGoldenViteServer(async (server) => {
  const { resolveFilePreviewScope } = await server.ssrLoadModule('/src/lib/file-preview-scope.ts');
  const workspaceBinding = {
    scope: 'workspace',
    workspaceId: 'workspace-test',
    workspacePath: '/tmp/workspace-test',
    sessionId: 'session-workspace-test',
  };
  const personalBinding = {
    scope: 'personal',
    workspaceId: '',
    workspacePath: '',
    sessionId: 'session-personal-test',
  };
  const pathForId = (workspaceId) => workspaceId ? `/tmp/${workspaceId}` : '';

  assert.deepEqual(
    resolveFilePreviewScope({
      imageDataUrl: 'data:image/png;base64,inline',
      currentBinding: personalBinding,
      selectedWorkspaceId: 'workspace-stale',
      selectedWorkspacePath: '/tmp/workspace-stale',
      workspacePathForId: pathForId,
      activeWorkspaceId: 'workspace-stale',
      activeSessionId: 'session-stale',
    }),
    {
      workspaceId: '',
      workspacePath: '',
      sessionId: 'session-personal-test',
      imageDataUrl: 'data:image/png;base64,inline',
    },
    '个人会话的内存标记图片不得继承旧工作区作用域',
  );

  assert.deepEqual(
    resolveFilePreviewScope({
      imageDataUrl: 'data:image/png;base64,inline',
      currentBinding: workspaceBinding,
      selectedWorkspaceId: 'workspace-stale',
      selectedWorkspacePath: '/tmp/workspace-stale',
      workspacePathForId: pathForId,
      activeWorkspaceId: 'workspace-stale',
      activeSessionId: 'session-stale',
    }),
    {
      workspaceId: 'workspace-test',
      workspacePath: '/tmp/workspace-test',
      sessionId: 'session-workspace-test',
      imageDataUrl: 'data:image/png;base64,inline',
    },
    '工作区会话的内存标记图片必须保持当前工作区作用域',
  );

  console.log('file preview scope golden passed');
});
