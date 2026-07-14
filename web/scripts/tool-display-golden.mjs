import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const display = await server.ssrLoadModule('/src/shared/tool-display.ts');

  assert.equal(
    display.getBuiltinToolFallbackLabel('custom_tool_name'),
    'custom tool name',
    'unmapped built-in tools must expose a readable name instead of an unknown label',
  );

  assert.equal(
    display.getCapabilityDependencyFallbackLabel('image_generation_model'),
    'image generation model',
    'dependency identifiers must remain understandable when a translation is missing',
  );

  assert.deepEqual(
    display.summarizeMcpServers([], false, false),
    { kind: 'checking' },
    'MCP status must show a checking state while its snapshot is still loading',
  );

  assert.deepEqual(
    display.summarizeMcpServers([
      { enabled: false, health: 'disabled' },
      { enabled: false, health: 'disabled' },
    ], true, false),
    { kind: 'disabled' },
    'MCP status must distinguish configured-but-disabled servers',
  );

  assert.deepEqual(
    display.summarizeMcpServers([
      { enabled: true, connected: true, health: 'connected' },
      { enabled: true, connected: false, health: 'disconnected', error: 'connection_issue' },
    ], true, false),
    { kind: 'partial', connected: 1, enabled: 2 },
    'MCP status must expose partial availability instead of a generic not-ready state',
  );

  console.log('tool display golden replay passed');
});
