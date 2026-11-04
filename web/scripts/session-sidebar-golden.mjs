import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
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
  const parallelChats = mergeExternalSessionsIntoSidebar([], ['A', 'B'].map((title, index) => ({
    tokenId: 'shared-client', sessionId: `external-tool-chat-${title}`, title,
    clientName: 'Web Agent', workspaceId: 'workspace-1', lastActivityAtMs: index, running: 0,
  })), 'workspace-1');
  assert.equal(parallelChats.length, 2, '同一客户端和目录中的多个聊天不能合并');
  assert.deepEqual(parallelChats.map(chat => chat.name), ['B', 'A'], '外部聊天标题作为主名称，客户端来源不再挤占标题');

});

console.log('session sidebar golden checks passed');

// 无法加载的会话必须能被用户删除：按钮常显（关键补救操作不依赖 hover），
// 点击经同一个确认框和 deleteSession 桥接，两处列表（项目分组与无项目对话）都要接线。
const unavailableList = readFileSync(new URL('../src/components/UnavailableSessionList.svelte', import.meta.url), 'utf8');
const shell = readFileSync(new URL('../src/web/WebWorkbenchShell.svelte', import.meta.url), 'utf8');
assert.match(unavailableList, /onclick=\{\(\) => onDelete\(session\)\}/, '不可用会话行必须提供删除入口');
assert.doesNotMatch(unavailableList, /unavailable-session-delete[^}]*opacity:\s*0/, '删除按钮不能只在悬停时出现');
assert.equal(
  (shell.match(/<UnavailableSessionList[\s\S]*?onDelete=\{/g) ?? []).length,
  2,
  '项目分组与无项目对话两处不可用列表都必须接入删除',
);
assert.match(shell, /unavailable: true,/, '不可用会话删除使用专门的确认文案');
console.log('session sidebar unavailable-delete golden checks passed');
