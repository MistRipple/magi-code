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

  console.log('MCP config golden replay passed');
});
