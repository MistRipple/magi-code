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
