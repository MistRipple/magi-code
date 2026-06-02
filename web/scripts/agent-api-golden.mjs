import assert from 'node:assert/strict';
import { createServer } from 'vite';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => value;

const server = await createServer({
  root: process.cwd(),
  configFile: false,
  logLevel: 'silent',
  server: { middlewareMode: true },
});

try {
  const binding = await server.ssrLoadModule('/src/web/agent-binding-context.ts');
  const agentApi = await server.ssrLoadModule('/src/web/agent-api.ts');

  binding.setAgentBindingContext({
    workspaceId: 'workspace-query-golden',
    workspacePath: '/tmp/workspace-query-golden',
    sessionId: 'session-query-golden',
  });

  const defaultPreviewQuery = new URLSearchParams(
    agentApi.buildFilePreviewQuery('src/main.ts'),
  );
  assert.equal(defaultPreviewQuery.get('workspaceId'), 'workspace-query-golden');
  assert.equal(defaultPreviewQuery.get('workspacePath'), '/tmp/workspace-query-golden');
  assert.equal(defaultPreviewQuery.get('filePath'), 'src/main.ts');
  assert.equal(
    defaultPreviewQuery.has('sessionId'),
    false,
    'file preview must default to workspace scope to avoid stale session binding failures',
  );

  const sessionPreviewQuery = new URLSearchParams(
    agentApi.buildFilePreviewQuery('src/main.ts', { includeSession: true }),
  );
  assert.equal(sessionPreviewQuery.get('sessionId'), 'session-query-golden');

  const overridePreviewQuery = new URLSearchParams(
    agentApi.buildFilePreviewQuery('README.md', {
      workspaceId: 'workspace-override',
      workspacePath: '/tmp/workspace-override',
      sessionId: 'session-override',
    }),
  );
  assert.equal(overridePreviewQuery.get('workspaceId'), 'workspace-override');
  assert.equal(overridePreviewQuery.get('workspacePath'), '/tmp/workspace-override');
  assert.equal(overridePreviewQuery.get('filePath'), 'README.md');
  assert.equal(
    overridePreviewQuery.has('sessionId'),
    false,
    'explicit file preview scope should still omit session unless includeSession is true',
  );

  console.log('agent api golden replay passed');
} finally {
  await server.close();
}
