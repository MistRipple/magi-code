import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const mcpConfig = await server.ssrLoadModule('/src/shared/mcp-config.ts');

  assert.deepEqual(
    mcpConfig.normalizeMcpServerDraft('web-search-prime', {
      type: 'http',
      url: ' https://example.test/mcp ',
      headers: { Authorization: 'Bearer test' },
      command: 'must-not-leak',
    }),
    {
      ok: true,
      server: {
        id: 'web-search-prime',
        name: 'web-search-prime',
        type: 'streamable-http',
        url: 'https://example.test/mcp',
        headers: { Authorization: 'Bearer test' },
        enabled: true,
      },
    },
    'HTTP MCP configuration must require url instead of command',
  );

  assert.equal(
    mcpConfig.normalizeMcpServerDraft('remote', { type: 'streamable-http' }).error,
    'missingUrl',
  );
  assert.equal(
    mcpConfig.normalizeMcpServerDraft('remote', { type: 'http', url: 'file:///tmp/mcp' }).error,
    'invalidUrl',
  );
  assert.equal(
    mcpConfig.normalizeMcpServerDraft('local', { type: 'stdio' }).error,
    'missingCommand',
  );

  const httpForm = mcpConfig.createMcpFormDraft('web-search-prime', {
    type: 'http',
    url: 'https://example.test/mcp',
    headers: { Authorization: 'Bearer test' },
    requestTimeoutMs: 45_000,
    enabled: false,
  });
  assert.deepEqual(httpForm, {
    name: 'web-search-prime',
    type: 'streamable-http',
    enabled: false,
    command: '',
    args: [],
    env: [],
    url: 'https://example.test/mcp',
    headers: [{ key: 'Authorization', value: 'Bearer test' }],
    requestTimeoutSeconds: '45',
  });
  assert.deepEqual(mcpConfig.convertMcpFormDraft(httpForm), {
    ok: true,
    name: 'web-search-prime',
    config: {
      type: 'streamable-http',
      url: 'https://example.test/mcp',
      headers: { Authorization: 'Bearer test' },
      enabled: false,
      requestTimeoutMs: 45_000,
    },
  });

  const stdioForm = mcpConfig.createMcpFormDraft('filesystem', {
    command: 'npx',
    args: ['-y', '@modelcontextprotocol/server-filesystem'],
    env: { ROOT: '/tmp' },
  });
  assert.deepEqual(mcpConfig.convertMcpFormDraft(stdioForm), {
    ok: true,
    name: 'filesystem',
    config: {
      type: 'stdio',
      command: 'npx',
      args: ['-y', '@modelcontextprotocol/server-filesystem'],
      env: { ROOT: '/tmp' },
      enabled: true,
    },
  });

  assert.equal(
    mcpConfig.convertMcpFormDraft({ ...stdioForm, requestTimeoutSeconds: '0' }).error,
    'invalidTimeout',
  );
  assert.equal(
    mcpConfig.convertMcpFormDraft({
      ...httpForm,
      headers: [{ key: 'Authorization', value: 'a' }, { key: 'Authorization', value: 'b' }],
    }).error,
    'duplicateKey',
  );

  console.log('MCP config golden replay passed');
});
