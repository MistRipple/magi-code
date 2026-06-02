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

  const workspaceIdOnlyOverridePreviewQuery = new URLSearchParams(
    agentApi.buildFilePreviewQuery('README.md', {
      workspaceId: 'workspace-id-only-override',
    }),
  );
  assert.equal(workspaceIdOnlyOverridePreviewQuery.get('workspaceId'), 'workspace-id-only-override');
  assert.equal(
    workspaceIdOnlyOverridePreviewQuery.has('workspacePath'),
    false,
    'workspaceId-only override must not inherit stale runtime workspacePath',
  );
  assert.equal(
    workspaceIdOnlyOverridePreviewQuery.has('sessionId'),
    false,
    'workspaceId-only override must not inherit stale runtime sessionId',
  );

  const workspacePathOnlyOverridePreviewQuery = new URLSearchParams(
    agentApi.buildFilePreviewQuery('README.md', {
      workspacePath: '/tmp/workspace-path-only-override',
    }),
  );
  assert.equal(workspacePathOnlyOverridePreviewQuery.get('workspacePath'), '/tmp/workspace-path-only-override');
  assert.equal(
    workspacePathOnlyOverridePreviewQuery.has('workspaceId'),
    false,
    'workspacePath-only override must not inherit stale runtime workspaceId',
  );
  assert.equal(
    workspacePathOnlyOverridePreviewQuery.has('sessionId'),
    false,
    'workspacePath-only override must not inherit stale runtime sessionId',
  );

  const originalFetch = globalThis.fetch;
  try {
    let capturedKnowledgeUrl = '';
    globalThis.fetch = async (url) => {
      capturedKnowledgeUrl = String(url);
      return new Response(JSON.stringify({
        workspaceId: 'workspace-knowledge-override',
        workspacePath: '/tmp/workspace-knowledge-override',
        items: [],
        codeIndex: null,
        codeIndexStatus: { status: 'empty', reasonCode: 'no_indexable_files' },
      }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    };
    await agentApi.getAgentProjectKnowledge({
      workspaceId: 'workspace-knowledge-override',
      workspacePath: '/tmp/workspace-knowledge-override',
      sessionId: 'session-must-not-leak',
    });
    const capturedQuery = new URL(capturedKnowledgeUrl).searchParams;
    assert.equal(capturedQuery.get('workspaceId'), 'workspace-knowledge-override');
    assert.equal(capturedQuery.get('workspacePath'), '/tmp/workspace-knowledge-override');
    assert.equal(
      capturedQuery.has('sessionId'),
      false,
      'project knowledge must stay workspace-scoped even when an explicit session is present',
    );
  } finally {
    if (originalFetch === undefined) {
      delete globalThis.fetch;
    } else {
      globalThis.fetch = originalFetch;
    }
  }

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

  try {
    let capturedToolCatalogUrl = '';
    globalThis.fetch = async (url) => {
      capturedToolCatalogUrl = String(url);
      return new Response(JSON.stringify({
        catalog_access_mode: 'read_only',
        current_access_profile: 'full_access',
        tools: [
          {
            name: 'shell_exec',
            public: true,
            risk_level: 'high',
            approval_requirement: 'required',
            effective_approval_policy: 'ordinary_approval_skipped',
            access_profile_behavior: 'full_access_skips_ordinary_approval',
            access_mode: 'explicit_write',
            policy_scope: 'input_sensitive',
            input_sensitive_policy: true,
            policy_summary: '按输入判定',
            runtime_internal: false,
            runtime_status: 'ready',
            runtime_warnings: ['raw dependency detail must become marker'],
            schema_status: 'ok',
            schema_warnings: [],
          },
          {
            name: 'file_read',
            public: true,
            riskLevel: 'low',
            approvalRequirement: 'none',
            effectiveApprovalPolicy: 'none',
            accessProfileBehavior: 'restricted_allowed',
            accessMode: 'read_only',
            policyScope: 'fixed',
            inputSensitivePolicy: false,
            policySummary: '默认策略',
            runtimeInternal: false,
            runtimeStatus: 'ready',
            runtimeWarnings: [],
            schemaStatus: 'ok',
            schemaWarnings: [],
          },
        ],
        runtime_dependencies: [],
      }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    };
    const diagnostics = await agentApi.loadAgentToolCatalogDiagnostics();
    assert.ok(
      capturedToolCatalogUrl.includes('/api/tools/catalog'),
      'tool catalog diagnostics must call the backend catalog endpoint',
    );
    const shell = diagnostics.builtinTools.find((tool) => tool.name === 'shell_exec');
    assert.ok(shell, 'shell_exec should be normalized from tool catalog response');
    assert.equal(shell.effectiveApprovalPolicy, 'ordinary_approval_skipped');
    assert.equal(shell.accessProfileBehavior, 'full_access_skips_ordinary_approval');
    assert.deepEqual(
      shell.runtimeWarnings,
      ['runtime_warning'],
      'runtime warning details must stay marker-only after normalization',
    );
    const fileRead = diagnostics.builtinTools.find((tool) => tool.name === 'file_read');
    assert.ok(fileRead, 'file_read should be normalized from camelCase response');
    assert.equal(fileRead.effectiveApprovalPolicy, 'none');
    assert.equal(fileRead.accessProfileBehavior, 'restricted_allowed');
  } finally {
    if (originalFetch === undefined) {
      delete globalThis.fetch;
    } else {
      globalThis.fetch = originalFetch;
    }
  }

  console.log('agent api golden replay passed');
} finally {
  await server.close();
}
