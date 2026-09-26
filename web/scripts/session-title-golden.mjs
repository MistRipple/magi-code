import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

const appSource = await readFile(new URL('../src/App.svelte', import.meta.url), 'utf8');
assert.match(
  appSource,
  /document\.title\s*=\s*currentSessionTitle\s*\|\|\s*i18n\.t\('app\.documentTitle'\)/,
  '应用文档标题必须由当前会话标题统一驱动',
);

await withGoldenViteServer(async (server) => {
  const module = await server.ssrLoadModule('/src/lib/session-title.ts');
  const workspaceSessions = [{ id: 'workspace-session', name: '工作区会话' }];
  const personalSessions = [{ id: 'personal-session', name: '个人会话' }];

  assert.equal(
    module.resolveSessionTitle('workspace-session', 'workspace-1', workspaceSessions, personalSessions),
    '工作区会话',
  );
  assert.equal(
    module.resolveSessionTitle('personal-session', '', workspaceSessions, personalSessions),
    '个人会话',
  );
  assert.equal(
    module.resolveSessionTitle('missing-session', 'workspace-1', workspaceSessions, personalSessions),
    '',
  );
  assert.equal(
    module.resolveSessionTitle('', '', workspaceSessions, personalSessions),
    '',
  );
  assert.equal(
    module.resolveCurrentSessionTitle({
      sessionId: 'workspace-session',
      workspaceId: 'workspace-1',
      workspaceSessions: { workspaceId: 'workspace-old', sessions: [{ id: 'stale', name: '旧投影' }] },
      workspaceSessionProjections: {
        'workspace-1': { workspaceId: 'workspace-1', sessions: workspaceSessions },
      },
      personalSessions,
    }),
    '工作区会话',
  );
  assert.equal(
    module.resolveCurrentSessionTitle({
      sessionId: 'workspace-session',
      workspaceId: 'workspace-1',
      workspaceSessions: { workspaceId: 'workspace-old', sessions: workspaceSessions },
      personalSessions,
    }),
    '',
  );
  assert.equal(
    module.resolveCurrentSessionTitle({
      sessionId: 'personal-session',
      workspaceId: '',
      workspaceSessions: { sessions: workspaceSessions },
      personalSessions,
    }),
    '个人会话',
  );
  assert.equal(
    module.resolveCurrentSessionTitle({
      sessionId: 'workspace-session',
      workspaceId: '',
      workspacePath: '/workspace/project',
      workspaceSessions: { sessions: workspaceSessions },
      personalSessions,
    }),
    '工作区会话',
  );
});

console.log('session title golden checks passed');
