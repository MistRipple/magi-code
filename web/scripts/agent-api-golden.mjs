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
  assert.equal(overridePreviewQuery.get('sessionId'), 'session-override');

  const workspaceOnlyOverridePreviewQuery = new URLSearchParams(
    agentApi.buildFilePreviewQuery('README.md', {
      workspaceId: 'workspace-override',
      workspacePath: '/tmp/workspace-override',
      sessionId: '',
    }),
  );
  assert.equal(
    workspaceOnlyOverridePreviewQuery.has('sessionId'),
    false,
    'explicit empty file preview session should keep workspace-only scope',
  );

  const originalWindow = globalThis.window;
  try {
    globalThis.window = {
      location: {
        href: 'http://127.0.0.1:38123/web.html?workspacePath=%2Ftmp%2Fworkspace-from-url',
      },
    };
    binding.setAgentBindingContext({
      workspaceId: 'workspace-stale-runtime',
      workspacePath: '/tmp/workspace-stale-runtime',
      sessionId: 'session-stale-runtime',
    });
    const explicitUrlBinding = binding.resolveAgentBindingContext();
    assert.equal(explicitUrlBinding.workspacePath, '/tmp/workspace-from-url');
    assert.equal(explicitUrlBinding.workspaceId, '');
    assert.equal(
      explicitUrlBinding.sessionId,
      '',
      'explicit URL workspace without session must clear stale runtime session binding',
    );
    const explicitUrlPreviewQuery = new URLSearchParams(
      agentApi.buildFilePreviewQuery('README.md'),
    );
    assert.equal(
      explicitUrlPreviewQuery.get('workspacePath'),
      '/tmp/workspace-from-url',
      'explicit URL workspace must win over stale runtime binding for shared API queries',
    );
    assert.equal(
      explicitUrlPreviewQuery.has('sessionId'),
      false,
      'workspace-only URL must not leak stale runtime sessionId into API queries',
    );
  } finally {
    if (originalWindow === undefined) {
      delete globalThis.window;
    } else {
      globalThis.window = originalWindow;
    }
  }

  console.log('agent api golden replay passed');
} finally {
  await server.close();
}
