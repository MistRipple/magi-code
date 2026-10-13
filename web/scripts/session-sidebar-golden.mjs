import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const { mergeExternalSessionsIntoSidebar } = await server.ssrLoadModule('/src/lib/session-sidebar.ts');
  const sessions = mergeExternalSessionsIntoSidebar(
    [
      { id: 'normal-old', createdAt: 10, updatedAt: 100, name: '旧会话' },
      { id: 'normal-new', createdAt: 20, updatedAt: 300, name: '新会话' },
    ],
    [
      {
        tokenId: 'token-external',
        sessionId: 'external-tool-token-external',
        clientName: 'Claude Code',
        workspaceId: 'workspace-1',
        lastActivityAtMs: 200,
        running: 0,
      },
    ],
    'workspace-1',
  );

  assert.deepEqual(
    sessions.map((session) => session.id),
    ['normal-new', 'external-tool-token-external', 'normal-old'],
    '外部会话应按最近活动时间插入普通会话列表，而不是固定在底部',
  );
});

console.log('session sidebar golden checks passed');
