import { svelte } from '@sveltejs/vite-plugin-svelte';
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

  console.log('tool display golden replay passed');
});

const zhCN = JSON.parse(await readFile(new URL('../src/i18n/zh-CN.json', import.meta.url), 'utf8'));
const enUS = JSON.parse(await readFile(new URL('../src/i18n/en-US.json', import.meta.url), 'utf8'));
const builtinCatalogSource = await readFile(
  new URL('../../crates/magi-tool-runtime/src/builtin_catalog.rs', import.meta.url),
  'utf8',
);
const builtinNamesSource = builtinCatalogSource.match(
  /pub fn as_str\(&self\)[\s\S]*?pub fn category\(&self\)/,
)?.[0] || '';
const builtinToolNames = Array.from(
  builtinNamesSource.matchAll(/Self::\w+\s*=>\s*"([a-z0-9_]+)"/g),
  (match) => match[1],
);

assert.ok(builtinToolNames.length > 0, 'golden test must read the Rust built-in tool catalog');
assert.equal(
  new Set(builtinToolNames).size,
  builtinToolNames.length,
  'Rust built-in tool names must remain unique',
);
for (const name of builtinToolNames) {
  const suffix = name.replace(/_([a-z0-9])/g, (_, char) => char.toUpperCase());
  const key = `settings.tools.builtin.${suffix}`;
  assert.equal(typeof zhCN[key], 'string', `${key} must have a Chinese label`);
  assert.ok(zhCN[key].trim(), `${key} must have a non-empty Chinese label`);
  assert.equal(typeof enUS[key], 'string', `${key} must have an English label`);
  assert.ok(enUS[key].trim(), `${key} must have a non-empty English label`);
}

assert.equal(zhCN['settings.tools.builtinCategory.browser'], '浏览器');
assert.equal(enUS['settings.tools.builtinCategory.browser'], 'Browser');

const settingsToolsSource = await readFile(
  new URL('../src/components/SettingsToolsTab.svelte', import.meta.url),
  'utf8',
);
assert.match(
  settingsToolsSource,
  /function getBuiltinToolLabel[\s\S]*?return formatBuiltinToolFallbackLabel\(name\);/,
  'unknown built-in tools must preserve a readable tool identity',
);
assert.doesNotMatch(
  settingsToolsSource,
  /getBuiltinToolLabel[\s\S]*?settings\.tools\.builtin\.unknown/,
  'Chinese built-in tool labels must not collapse unknown names into one generic label',
);
assert.match(
  settingsToolsSource,
  /builtinToolGroups[\s\S]*?new Map<string, typeof builtinTools>[\s\S]*?tool\.category[\s\S]*?class="builtin-category-group"/,
  'built-in capability statistics and top-level rows must group concrete actions by category',
);
assert.match(
  settingsToolsSource,
  /builtinReadyCount[\s\S]*?builtinToolGroups\.filter[\s\S]*?total: builtinToolGroups\.length/,
  'built-in readiness statistics must count capability categories instead of concrete actions',
);
assert.match(
  settingsToolsSource,
  /class="builtin-tool-identity"[\s\S]*?class="builtin-tool-badges"/,
  'built-in tool cards must keep identity and status in separate stable columns',
);
assert.match(
  settingsToolsSource,
  /\.builtin-category-list\s*\{[\s\S]*?display:\s*flex;[\s\S]*?border:\s*var\(--magi-hairline\) solid var\(--border\);[\s\S]*?\.builtin-category-group \+ \.builtin-category-group\s*\{[\s\S]*?border-top:\s*var\(--magi-hairline\) solid var\(--border\);/,
  'built-in capability categories must use one compact list with separators instead of nested cards',
);
assert.match(
  settingsToolsSource,
  /\.builtin-category-group > \.builtin-tool-list\s*\{[\s\S]*?padding:\s*3px 10px 5px 45px;[\s\S]*?\.builtin-tool-list\s*\{[\s\S]*?display:\s*flex;[\s\S]*?flex-direction:\s*column;[\s\S]*?\.builtin-tool-row\s*\{[\s\S]*?grid-template-columns:\s*24px minmax\(0, 1fr\) auto;[\s\S]*?border-bottom:/,
  'expanded built-in actions must render as an indented dense sub-list aligned with MCP and Skill rows',
);
assert.match(
  settingsToolsSource,
  /@container tools-tab \(max-width:\s*560px\)[\s\S]*?\.builtin-category-group > \.builtin-tool-list\s*\{[\s\S]*?padding-left:\s*12px;[\s\S]*?\.builtin-tool-row\s*\{[\s\S]*?grid-template-columns:\s*24px minmax\(0, 1fr\);[\s\S]*?\.builtin-tool-badges\s*\{[\s\S]*?grid-column:\s*2;/,
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
  /\.capability-dependency-strip\s*\{[\s\S]*?border:\s*var\(--magi-hairline\) solid var\(--border\);[\s\S]*?\.command-environment-panel\s*\{[\s\S]*?width:\s*100%;[\s\S]*?border:\s*var\(--magi-hairline\) solid var\(--border\);/,
  'built-in tool dependency strip and command environment panel must share the same card material',
);
assert.doesNotMatch(
  settingsToolsSource,
  /\.command-environment-command\.command-unavailable\s*\{[^}]*text-decoration:\s*line-through;/,
  'unavailable commands should use a muted status indicator instead of destructive strikethrough styling',
);
// MCP rendering is covered below against the actual Svelte section, replacing the old popup-source assertions.
for (const selector of ['builtin-category-list', 'skill-detail-body']) {
  const body = settingsToolsSource.match(new RegExp(`\\.${selector}\\s*\\{([^}]+)\\}`))?.[1];
  assert.ok(body, `${selector} must keep its content layout`);
  assert.doesNotMatch(body, /(?:max-)?height\s*:|overflow(?:-y)?\s*:\s*(?:auto|scroll)/,
    `${selector} must use the settings page scroll area instead of a nested height cap`);
}

await withGoldenViteServer(async server => {
  const { render } = await server.ssrLoadModule('svelte/server');
  const { default: Section } = await server.ssrLoadModule('/src/components/SettingsMcpToolsSection.svelte');
  const props = {
    servers: [{ id: 'files', name: 'Files', type: 'stdio', enabled: true, connected: true, health: 'connected', toolCount: 1 }],
    hydrated: true, loading: false, listError: '', retryServers() {}, expandedServer: 'files', tools: { files: [{ name: 'read_file', description: 'Read project files.' }] },
    refreshing: new Set(), errors: {}, changing: new Set(), openEditor() {}, toggleExpanded() {},
    healthLabel: item => item.connected ? 'Connected' : 'Disconnected', toggleServer() {}, deleteServer() {}, refreshTools() {},
  };
  const html = render(Section, { props }).body;
  assert.match(html, /read_file/);
  assert.match(html, /Read project files\./);
  assert.equal((html.match(/1 个工具/g) ?? []).length, 1, 'tool count belongs only in the service row');
  assert.doesNotMatch(html, /inputSchema|参数|工具目录<|role="tooltip"/, 'directory is capability information, without invocation details or popup layers');
  const failed = render(Section, { props: { ...props, errors: { files: 'Read failed' } } }).body;
  assert.match(failed, /Read failed/);
  assert.match(failed, /read_file/, 'failed refresh preserves the previous directory');
  const unavailable = render(Section, { props: { ...props, tools: {}, errors: { files: 'Read failed' } } }).body;
  assert.match(unavailable, /Read failed/);
  assert.doesNotMatch(unavailable, /此服务未提供工具/, 'read failure is not an empty directory');
  const empty = render(Section, { props: { ...props, tools: { files: [] } } }).body;
  assert.match(empty, /此服务未提供工具/, 'a successful empty directory has its own state');
}, { plugins: [svelte()], ssr: { noExternal: ['svelte'] } });
