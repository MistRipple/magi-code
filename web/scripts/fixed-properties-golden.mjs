import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { parse } from 'svelte/compiler';
import { withGoldenViteServer } from './golden-vite.mjs';

// Fixed fields may be behind the user's disclosure, never behind data availability.
const runtime = await readFile(new URL('../src/components/RuntimeStatePanel.svelte', import.meta.url), 'utf8');
const sessionStats = await readFile(new URL('../src/components/SessionStats.svelte', import.meta.url), 'utf8');
assert.match(sessionStats, /\.metric-value--status\s*\{[\s\S]*?font-size:\s*15px[\s\S]*?font-weight:\s*var\(--font-normal\)/, 'unobserved status stays slightly above body text without becoming a headline');
assert.match(sessionStats, /\.metric-value\s*\{[\s\S]*?font-size:\s*24px/, 'observed metric values use a restrained size');
assert.match(sessionStats, /\.metric-label\s*\{[\s\S]*?font-size:\s*12px/, 'metric labels remain readable at compact size');
const metrics = [];
function inspect(node, conditions = []) {
  if (!node || typeof node !== 'object') return;
  if (node.type === 'IfBlock') conditions = [...conditions, runtime.slice(node.expression.start, node.expression.end)];
  const className = node.attributes?.find(attribute => attribute.name === 'class')?.value?.[0]?.data;
  if (className === 'metric-card') metrics.push(conditions);
  for (const value of Object.values(node)) {
    for (const child of Array.isArray(value) ? value : [value]) inspect(child, conditions);
  }
}
inspect(parse(runtime).html);
assert.equal(metrics.length, 4);
for (const conditions of metrics) assert.deepEqual(conditions, ['panelVisible', 'isPanelExpanded']);

for (const [file, className, allowed] of [
  ['KnowledgePanel.svelte', 'kp-stats-row', ["currentTab === 'overview'"]],
  ['PluginDetailPane.svelte', 'pane-facts', []],
  ['SettingsPanel.svelte', 'skill-library-meta', null],
  ['SettingsToolsTab.svelte', 'mcp-tools-count', null],
]) {
  const source = await readFile(new URL(`../src/components/${file}`, import.meta.url), 'utf8');
  const sections = [];
  function visit(node, conditions = []) {
    if (!node || typeof node !== 'object') return;
    if (node.type === 'IfBlock') conditions = [...conditions, source.slice(node.expression.start, node.expression.end)];
    if (node.attributes?.some(attribute => attribute.name === 'class' && attribute.value?.[0]?.data === className)) sections.push(conditions);
    for (const value of Object.values(node)) for (const child of Array.isArray(value) ? value : [value]) visit(child, conditions);
  }
  visit(parse(source).html);
  assert.equal(sections.length, 1, `${file}: fixed section exists`);
  if (allowed) assert.deepEqual(sections[0], allowed, `${file}: loading and empty data cannot hide fixed fields`);
  else assert.ok(sections[0].every(condition => !/skill\.(author|version|category)|mcpServerTools/.test(condition)), `${file}: metadata values cannot hide fixed labels`);
}

await withGoldenViteServer(async server => {
  const { render } = await server.ssrLoadModule('svelte/server');
  const { i18n } = await server.ssrLoadModule('/src/stores/i18n.svelte.ts');
  const { default: Context } = await server.ssrLoadModule('/src/components/ContextUsageRing.svelte');
  const { default: Model } = await server.ssrLoadModule('/src/components/ModelConfigForm.svelte');
  const { default: Plugin } = await server.ssrLoadModule('/src/components/PluginDetailPane.svelte');
  const { default: Agent } = await server.ssrLoadModule('/src/components/tabs/AgentTabContent.svelte');
  const missing = () => i18n.t('common.notProvided');
  const noop = () => {};
  const text = html => html.replace(/<[^>]+>/g, '').replaceAll('&amp;', '&').replaceAll('&lt;', '<').replaceAll('&gt;', '>').replaceAll('&quot;', '"').replaceAll('&#39;', "'").replace(/\s+/g, ' ');
  const fixedContext = ['usage', 'remaining', 'limit', 'breakdown', 'budget', 'outputReserve', 'recoveryBuffer', 'compactionThreshold', 'configuredWindow'];
  const categories = ['messages', 'images', 'system', 'project', 'skills', 'references', 'builtinTools', 'mcpTools', 'skillTools', 'otherTools'];
  const zeroBreakdown = Object.fromEntries(['conversationTokens', 'imageTokens', 'systemInstructionTokens', 'projectContextTokens', 'skillTokens', 'contextReferenceTokens', 'builtinToolTokens', 'mcpToolTokens', 'skillToolTokens', 'otherToolTokens'].map(key => [key, 0]));
  for (const locale of ['zh-CN', 'en-US']) {
    i18n.setLocale(locale);
    for (const props of [{}, { tokenLimit: 128000 }, { tokenUsed: 0, tokenLimit: 128000, remainingTokens: 128000, contextBreakdown: zeroBreakdown, responseReserveTokens: 0 }]) {
      const html = render(Context, { props }).body;
      const content = text(html);
      for (const key of fixedContext) {
        const label = i18n.t(`input.contextRing.${key}`, { value: '' }).trim();
        assert.ok(content.includes(label), `${locale}: missing fixed context field ${key}`);
      }
      for (const category of categories) assert.ok(content.includes(i18n.t(`input.contextRing.category.${category}`)));
      assert.equal((html.match(/class="context-breakdown-row /g) ?? []).length, 10);
      assert.doesNotMatch(content, /NaN|Infinity|undefined/);
      assert.ok(!content.includes(i18n.t('input.contextRing.compaction', { reason: '', before: '', after: '' })), 'absent compaction remains dynamic');
      if (props.tokenUsed === 0) assert.match(content, /0%/);
    }

    const model = text(render(Model, { props: {
      formType: 'worker', statusKey: 'fixed-fields', config: {}, baselineConfig: {}, keyVisible: {},
      saveStatus: {}, testStatus: {}, fetchingModels: {}, modelListOpen: {}, modelListPosition: {}, modelLists: {}, modelDisplayNames: {},
      getBaseUrlPlaceholder: () => '', openModelList: noop, closeModelList: noop, fetchModelList: noop, selectModel: noop, saveModelConfig: noop, testModelConnection: noop,
    } }).body);
    assert.ok(model.includes(i18n.t('settings.model.reasoning.workerDefault')));
    assert.ok(model.includes(i18n.t('settings.status.notConfigured')));
    assert.ok(!model.includes(i18n.t('settings.model.reasoning.loading')), 'no model is unconfigured, not perpetually loading');

    const item = { id: 'fixture', name: 'Fixture', description: '', version: '1.0', installed: null, bundled: null, catalog: null, status: 'available', updatable: false };
    const plugin = text(render(Plugin, { props: { item, now: 0, busy: false, onInstall: noop, onUpdateFromCenter: noop, onUpgradeFile: noop, onUninstall: noop, onActivate: noop, onDeactivate: noop, onDisable: noop, onOpenSettings: noop } }).body);
    for (const key of ['type', 'version', 'source', 'digest', 'size']) assert.ok(plugin.includes(i18n.t(`settings.plugins.detail.${key}`)));
    assert.ok(plugin.includes(missing()));

    const agent = text(render(Agent, { props: { agentRunId: 'missing-snapshot' } }).body);
    for (const key of ['model', 'level']) assert.ok(agent.includes(i18n.t(`settings.model.field.${key}`)));
    assert.ok(agent.includes(missing()));
    assert.ok(!agent.includes(i18n.t('input.mainModelPicker.reasoning.providerDefault')), 'missing historical snapshot cannot imply provider default');
  }
  console.log('fixed properties golden checks passed (empty, partial, zero; Chinese and English)');
}, { configFile: './vite.web.config.ts' });
