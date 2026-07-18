import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
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

const zhCN = JSON.parse(await readFile(new URL('../src/i18n/zh-CN.json', import.meta.url), 'utf8'));
const enUS = JSON.parse(await readFile(new URL('../src/i18n/en-US.json', import.meta.url), 'utf8'));
const requiredBuiltinToolLabels = {
  imageGenerate: ['生成图片', 'Image Generation'],
  getGoal: ['查看目标', 'Get Goal'],
  createGoal: ['创建目标', 'Create Goal'],
  updateGoal: ['更新目标', 'Update Goal'],
};

for (const [suffix, [expectedZh, expectedEn]] of Object.entries(requiredBuiltinToolLabels)) {
  const key = `settings.tools.builtin.${suffix}`;
  assert.equal(zhCN[key], expectedZh, `${key} must have a complete Chinese label`);
  assert.equal(enUS[key], expectedEn, `${key} must have a complete English label`);
}

const settingsToolsSource = await readFile(
  new URL('../src/components/SettingsToolsTab.svelte', import.meta.url),
  'utf8',
);
assert.match(
  settingsToolsSource,
  /class="builtin-tool-identity"[\s\S]*?class="builtin-tool-badges"/,
  'built-in tool cards must keep identity and status in separate stable columns',
);
assert.match(
  settingsToolsSource,
  /\.builtin-tool-list\s*\{[\s\S]*?gap:\s*6px;[\s\S]*?\.builtin-tool-row\s*\{[\s\S]*?display:\s*grid;[\s\S]*?grid-template-columns:\s*26px minmax\(0, 1fr\) auto;[\s\S]*?min-height:\s*46px;[\s\S]*?padding:\s*5px 8px;/,
  'wide built-in tool cards must use a compact icon, identity, and status grid',
);
assert.match(
  settingsToolsSource,
  /@container tools-tab \(max-width:\s*560px\)[\s\S]*?\.builtin-tool-row\s*\{[\s\S]*?grid-template-columns:\s*26px minmax\(0, 1fr\);[\s\S]*?\.builtin-tool-badges\s*\{[\s\S]*?grid-column:\s*2;/,
  'only genuinely narrow layouts should move status badges below the tool identity',
);
assert.match(
  settingsToolsSource,
  /<!-- 命令环境 -->[\s\S]*?class="settings-section tools-section command-environment-section"[\s\S]*?class="command-environment-panel"/,
  'command environment must be a peer settings section instead of a nested card inside built-in tools',
);
assert.match(
  settingsToolsSource,
  /title=\{i18n\.t\('settings\.tools\.refreshBuiltinTools'\)\}[\s\S]*?refreshBuiltinToolCatalog\(\)[\s\S]*?title=\{i18n\.t\('settings\.tools\.refreshCommandEnvironment'\)\}[\s\S]*?refreshCommandEnvironment\(\)/,
  'built-in tools and command environment must keep independent refresh actions',
);
assert.match(
  settingsToolsSource,
  /\.builtin-summary\s*\{[\s\S]*?width:\s*100%;[\s\S]*?\.command-environment-panel\s*\{[\s\S]*?width:\s*100%;/,
  'built-in tools and command environment panels must share the same full-width alignment',
);
assert.doesNotMatch(
  settingsToolsSource,
  /\.command-environment-command\.command-unavailable\s*\{[^}]*text-decoration:\s*line-through;/,
  'unavailable commands should use a muted status indicator instead of destructive strikethrough styling',
);
assert.match(
  settingsToolsSource,
  /class="mcp-server-list"[\s\S]*?class="mcp-server-item"[\s\S]*?class="mcp-server-row"/,
  'MCP servers must use a compact management list instead of repeated dashboard cards',
);
assert.doesNotMatch(
  settingsToolsSource,
  /class="apple-tile mcp-server-item"/,
  'MCP server default rows must not reuse the oversized generic tile presentation',
);
assert.match(
  settingsToolsSource,
  /class="mcp-server-actions"[\s\S]*?<Toggle[\s\S]*?openMCPDialog\(server\)[\s\S]*?deleteMCPServer\(server\.id\)/,
  'MCP list rows must keep enable, edit, and delete actions together',
);
assert.match(
  settingsToolsSource,
  /\{#if mcpExpandedServer === server\.id\}[\s\S]*?class="mcp-tools-popover"[\s\S]*?class="mcp-tools-list"/,
  'MCP server rows must preserve the existing expanded tool-list presentation',
);
assert.match(
  settingsToolsSource,
  /class="mcp-tools-heading"[\s\S]*?class="mcp-tools-count"[\s\S]*?class="mcp-tool-identity"[\s\S]*?class="mcp-tool-description"/,
  'expanded MCP tool lists must expose a compact count and readable tool descriptions',
);
assert.match(
  settingsToolsSource,
  /\.mcp-tools-list\s*\{[\s\S]*?max-height:\s*min\(280px, 42vh\);[\s\S]*?overflow-y:\s*auto;[\s\S]*?overscroll-behavior:\s*contain;/,
  'expanded MCP tool lists must scroll independently when a server exposes many tools',
);
