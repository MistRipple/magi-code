import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

globalThis.$state = (value) => value;
globalThis.$derived = (value) => value;

await withGoldenViteServer(async (server) => {
  const binding = await server.ssrLoadModule('/src/web/agent-binding-context.ts');
  const agentApi = await server.ssrLoadModule('/src/web/agent-api.ts');

  binding.setAgentBindingContext({
    workspaceId: 'workspace-query-golden',
    workspacePath: '/tmp/workspace-query-golden',
    sessionId: 'session-query-golden',
  });

  binding.setAgentBindingContext({
    workspaceId: 'workspace-path-ref-golden',
    workspacePath: 'mhp1:u:L3RtcC93b3Jrc3BhY2UtcGF0aC1yZWYtZ29sZGVu',
    sessionId: 'session-path-ref-golden',
  });
  assert.equal(
    agentApi.settingsBootstrapMatchesCurrentWorkspace({
      workspaceId: 'workspace-path-ref-golden',
      workspacePath: '/tmp/workspace-path-ref-golden',
      sessionId: 'session-path-ref-golden',
    }),
    true,
    'workspace identity must not reject bootstrap data when the binding uses an opaque path ref and the response uses a display path',
  );
  assert.equal(
    agentApi.settingsBootstrapMatchesCurrentWorkspace({
      workspaceId: 'workspace-other',
      workspacePath: '/tmp/workspace-path-ref-golden',
      sessionId: 'session-path-ref-golden',
    }),
    false,
    'workspaceId mismatch must remain authoritative even when display paths match',
  );

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
    const capturedKnowledgePosts = [];
    globalThis.fetch = async (url, init) => {
      capturedKnowledgeUrl = String(url);
      if (init?.method === 'POST') {
        capturedKnowledgePosts.push({
          url: String(url),
          body: JSON.parse(String(init.body)),
        });
      }
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

    capturedKnowledgeUrl = '';
    await agentApi.getAgentProjectKnowledge({
      workspaceId: '',
      workspacePath: '',
    });
    const emptyOverrideQuery = new URL(capturedKnowledgeUrl).searchParams;
    assert.equal(
      emptyOverrideQuery.get('workspaceId'),
      'workspace-query-golden',
      'empty knowledge workspace override must fall back to the active workspace binding',
    );
    assert.equal(
      emptyOverrideQuery.get('workspacePath'),
      '/tmp/workspace-query-golden',
      'empty knowledge workspace override must not erase the active workspace path',
    );

    await agentApi.reindexAgentProjectKnowledge({
      workspaceId: 'workspace-knowledge-override',
      workspacePath: '/tmp/workspace-knowledge-override',
      sessionId: 'session-must-not-leak',
    });
    await agentApi.clearAgentProjectKnowledge({
      workspaceId: 'workspace-knowledge-override',
      workspacePath: '/tmp/workspace-knowledge-override',
      sessionId: 'session-must-not-leak',
    });
    assert.deepEqual(
      capturedKnowledgePosts,
      [
        {
          url: 'http://127.0.0.1:38123/api/knowledge/reindex',
          body: {
            workspaceId: 'workspace-knowledge-override',
            workspacePath: '/tmp/workspace-knowledge-override',
          },
        },
        {
          url: 'http://127.0.0.1:38123/api/knowledge/clear',
          body: {
            workspaceId: 'workspace-knowledge-override',
            workspacePath: '/tmp/workspace-knowledge-override',
          },
        },
      ],
      'workspace-scoped knowledge mutations must never send a session binding',
    );

    capturedKnowledgeUrl = '';
    await agentApi.getAgentPendingChanges({
      workspaceId: 'workspace-knowledge-override',
      workspacePath: '/tmp/workspace-knowledge-override',
      sessionId: 'session-changes-refresh',
    });
    const changesUrl = new URL(capturedKnowledgeUrl);
    assert.equal(changesUrl.pathname, '/api/changes');
    assert.equal(changesUrl.searchParams.get('workspaceId'), 'workspace-knowledge-override');
    assert.equal(changesUrl.searchParams.get('workspacePath'), '/tmp/workspace-knowledge-override');
    assert.equal(changesUrl.searchParams.get('sessionId'), 'session-changes-refresh');
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

    binding.setAgentBindingContext({
      workspaceId: 'workspace-authoritative',
      workspacePath: '/tmp/workspace-authoritative',
      sessionId: '',
    }, {
      authoritative: true,
    });
    globalThis.window.location.href = 'http://127.0.0.1:38123/web.html?workspaceId=workspace-authoritative&workspacePath=%2Ftmp%2Fworkspace-authoritative&sessionId=session-stale-url';
    const authoritativeBinding = binding.resolveAgentBindingContext();
    assert.equal(authoritativeBinding.workspaceId, 'workspace-authoritative');
    assert.equal(authoritativeBinding.workspacePath, '/tmp/workspace-authoritative');
    assert.equal(
      authoritativeBinding.sessionId,
      '',
      'authoritative backend binding must ignore stale URL sessionId after bootstrap clears current session',
    );
    const authoritativePreviewQuery = new URLSearchParams(
      agentApi.buildFilePreviewQuery('README.md', { includeSession: true }),
    );
    assert.equal(
      authoritativePreviewQuery.has('sessionId'),
      false,
      'shared API queries must not revive stale URL sessionId after backend binding becomes authoritative',
    );
  } finally {
    if (originalWindow === undefined) {
      delete globalThis.window;
    } else {
      globalThis.window = originalWindow;
    }
  }

  try {
    let capturedSettingsBootstrapUrl = '';
    binding.setAgentBindingContext({
      workspaceId: 'workspace-query-golden',
      workspacePath: '/tmp/workspace-query-golden',
      sessionId: 'session-query-golden',
    });
    globalThis.fetch = async (url) => {
      capturedSettingsBootstrapUrl = String(url);
      return new Response(JSON.stringify({
        workspaceId: 'workspace-query-golden',
        workspacePath: '/tmp/workspace-query-golden',
        sessionId: 'session-query-golden',
        workerConfigs: {},
        orchestratorConfig: {},
        orchestratorSessionConfig: {},
        effectiveOrchestratorConfig: {},
        auxiliaryConfig: {},
        userRulesConfig: {},
        skillsConfig: {},
        safeguardConfig: {},
        repositories: [],
        mcpServers: [],
        builtinTools: [],
        capabilityDependencies: [],
        workerStatuses: {},
        runtimeSettings: { locale: 'zh-CN' },
        roleTemplates: [],
        registryEngines: [],
        registryAgents: [],
      }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    };
    const settingsBootstrap = await agentApi.getAgentSettingsBootstrap({ scope: 'core', accessProfile: 'read_only' });
    const settingsBootstrapQuery = new URL(capturedSettingsBootstrapUrl).searchParams;
    assert.equal(settingsBootstrapQuery.get('scope'), 'core');
    assert.equal(
      settingsBootstrapQuery.get('accessProfile'),
      'read_only',
      'settings bootstrap must request tool diagnostics under the requested access profile',
    );
    assert.deepEqual(
      settingsBootstrap.imageGenerationConfig,
      {},
      'settings bootstrap must expose a normalized image generation config section',
    );
  } finally {
    if (originalFetch === undefined) {
      delete globalThis.fetch;
    } else {
      globalThis.fetch = originalFetch;
    }
  }

  try {
    const imageGenerationRequests = [];
    globalThis.fetch = async (url, init = {}) => {
      imageGenerationRequests.push({
        url: String(url),
        method: init.method || 'GET',
        body: init.body ? JSON.parse(String(init.body)) : null,
      });
      return new Response(JSON.stringify({ success: true }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    };

    const imageConfig = {
      baseUrl: 'https://cpa.example.com/v1',
      apiKey: 'sk-image',
      model: 'gpt-image-1',
      urlMode: 'standard',
    };
    await agentApi.saveAgentImageGenerationConfig(imageConfig);
    await agentApi.testAgentImageGenerationConnection(imageConfig);

    assert.deepEqual(imageGenerationRequests, [
      {
        url: 'http://127.0.0.1:38123/api/settings/image-generation/save',
        method: 'POST',
        body: {
          ...imageConfig,
          workspaceId: 'workspace-query-golden',
          workspacePath: '/tmp/workspace-query-golden',
        },
      },
      {
        url: 'http://127.0.0.1:38123/api/settings/image-generation/test',
        method: 'POST',
        body: {
          ...imageConfig,
          workspaceId: 'workspace-query-golden',
          workspacePath: '/tmp/workspace-query-golden',
        },
      },
    ]);
  } finally {
    if (originalFetch === undefined) {
      delete globalThis.fetch;
    } else {
      globalThis.fetch = originalFetch;
    }
  }

  try {
    let capturedToolCatalogUrl = '';
    globalThis.fetch = async (url) => {
      capturedToolCatalogUrl = String(url);
      return new Response(JSON.stringify({
        catalogAccessMode: 'read_only',
        currentAccessProfile: 'full_access',
        tools: [
          {
            name: 'shell_exec',
            public: true,
            riskLevel: 'high',
            approvalRequirement: 'required',
            effectiveApprovalPolicy: 'ordinary_approval_skipped',
            accessProfileBehavior: 'full_access_skips_ordinary_approval',
            accessMode: 'explicit_write',
            policyScope: 'input_sensitive',
            inputSensitivePolicy: true,
            policySummary: '按输入判定',
            runtimeInternal: false,
            runtimeStatus: 'ready',
            runtimeWarnings: ['raw dependency detail must become marker'],
            schemaStatus: 'ok',
            schemaWarnings: [],
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
        runtimeDependencies: [
          {
            name: 'mcp_servers',
            status: 'not_ready',
            requiredBy: ['mcp custom tools'],
            configuredCount: 1,
            enabledCount: 1,
            readyCount: 0,
            enabledToolCount: 7,
            readyToolCount: 0,
            toolCount: 0,
          },
        ],
      }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    };
    const diagnostics = await agentApi.loadAgentToolCatalogDiagnostics({ accessProfile: 'full_access' });
    assert.ok(
      capturedToolCatalogUrl.includes('/api/tools/catalog'),
      'tool catalog diagnostics must call the backend catalog endpoint',
    );
    const toolCatalogQuery = new URL(capturedToolCatalogUrl).searchParams;
    assert.equal(
      toolCatalogQuery.get('accessProfile'),
      'full_access',
      'tool catalog diagnostics must use the requested access profile',
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
    const mcpDependency = diagnostics.capabilityDependencies.find((dependency) => dependency.name === 'mcp_servers');
    assert.ok(mcpDependency, 'MCP dependency should be normalized from tool catalog diagnostics');
    assert.equal(mcpDependency.enabledToolCount, 7);
    assert.equal(mcpDependency.readyToolCount, 0);
    assert.equal(
      mcpDependency.toolCount,
      0,
      'normalized MCP toolCount must mean currently usable tools',
    );
  } finally {
    if (originalFetch === undefined) {
      delete globalThis.fetch;
    } else {
      globalThis.fetch = originalFetch;
    }
  }

  try {
    const notificationRequests = [];
    globalThis.fetch = async (url, init = {}) => {
      notificationRequests.push({
        url: String(url),
        method: init.method || 'GET',
        body: init.body ? JSON.parse(String(init.body)) : null,
      });
      return new Response(JSON.stringify({
        workspaceId: 'workspace-query-golden',
        sessionId: null,
        notifications: { lastUpdatedAt: 1, records: [] },
      }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    };

    await agentApi.getAgentNotifications({
      workspaceId: 'workspace-query-golden',
      workspacePath: '/tmp/workspace-query-golden',
    });
    await agentApi.reportAgentIncident({
      scope: 'workspace',
      level: 'error',
      message: '索引服务失败',
      source: 'knowledge-runtime',
    }, {
      workspaceId: 'workspace-query-golden',
      workspacePath: '/tmp/workspace-query-golden',
    });

    const loadRequest = notificationRequests[0];
    const loadUrl = new URL(loadRequest.url);
    assert.equal(loadUrl.pathname, '/api/notifications');
    assert.equal(loadUrl.searchParams.get('workspaceId'), 'workspace-query-golden');
    assert.equal(loadUrl.searchParams.has('sessionId'), false);

    const reportRequest = notificationRequests[1];
    assert.equal(new URL(reportRequest.url).pathname, '/api/notifications/report');
    assert.equal(reportRequest.method, 'POST');
    assert.equal(reportRequest.body.scope, 'workspace');
    assert.equal(reportRequest.body.message, '索引服务失败');
    assert.equal(reportRequest.body.sessionId, undefined);
  } finally {
    if (originalFetch === undefined) {
      delete globalThis.fetch;
    } else {
      globalThis.fetch = originalFetch;
    }
  }

  binding.setAgentBindingContext({
    workspaceId: '',
    workspacePath: '',
    sessionId: '',
  });
  const fetchBeforeEmptyStats = globalThis.fetch;
  let emptyStatsRequestCount = 0;
  globalThis.fetch = async () => {
    emptyStatsRequestCount += 1;
    throw new Error('empty workspace stats must not reach transport');
  };
  try {
    const emptyStats = await agentApi.getAgentExecutionStats();
    assert.equal(emptyStatsRequestCount, 0);
    assert.equal(emptyStats.workspaceId, '');
    assert.deepEqual(emptyStats.items, []);
    assert.deepEqual(emptyStats.totals, {
      llmCallCount: 0,
      assignmentCount: 0,
      turnCount: 0,
      totalTokens: 0,
      netInputTokens: 0,
      netOutputTokens: 0,
      successCount: 0,
      failureCount: 0,
    });
  } finally {
    if (fetchBeforeEmptyStats === undefined) {
      delete globalThis.fetch;
    } else {
      globalThis.fetch = fetchBeforeEmptyStats;
    }
  }

  console.log('agent api golden replay passed');
});
