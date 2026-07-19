import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { aggregateUsageStatsForDisplay } from '../src/lib/usage-stats-aggregation.ts';
import { resolveModelApiProtocol } from '../src/shared/model-governance.ts';

assert.equal(
  resolveModelApiProtocol({
    baseUrl: 'https://api.deepseek.com/anthropic',
    urlMode: 'standard',
    model: 'deepseek-chat',
  }),
  'anthropic_messages',
  '显式 /anthropic 路径必须优先使用 Anthropic Messages，不能被模型名误判为 OpenAI Chat',
);

const settingsStoreSource = await readFile(
  new URL('../src/stores/settings-store.svelte.ts', import.meta.url),
  'utf8',
);
const settingsStatsTabSource = await readFile(
  new URL('../src/components/SettingsStatsTab.svelte', import.meta.url),
  'utf8',
);
const settingsPanelSource = await readFile(
  new URL('../src/components/SettingsPanel.svelte', import.meta.url),
  'utf8',
);
const settingsToolsTabSource = await readFile(
  new URL('../src/components/SettingsToolsTab.svelte', import.meta.url),
  'utf8',
);
const usageAggregationSource = await readFile(
  new URL('../src/lib/usage-stats-aggregation.ts', import.meta.url),
  'utf8',
);

assert.match(
  settingsStoreSource,
  /async function toggleSkill\(skillId: string, enabled: boolean\)[\s\S]*?await toggleAgentSkill\(skillId, enabled\)[\s\S]*?await hydrateSkillInventory\(\)/,
  'Skill 启停必须写入后端并重新读取权威 Skill 清单',
);
assert.match(
  settingsToolsTabSource,
  /checked=\{skill\.enabled !== false\}[\s\S]*?onchange=\{\(enabled\) => toggleSkill\(skill\.skillId, enabled\)\}/,
  '已安装的 instruction Skill 必须提供受控启停开关',
);
assert.match(
  settingsToolsTabSource,
  /skill\.lastCheckedAt[\s\S]*?settings\.tools\.skillCheckedAt[\s\S]*?: i18n\.t\('settings\.tools\.skillNeverChecked'\)/,
  'Skill 从未检查时必须直接显示“尚未检查”，不能拼接成“检查于 尚未检查”',
);

assert.match(
  settingsStoreSource,
  /import\s*\{[^}]*\bgetModelStatus\b[^}]*\bmessagesState\b[^}]*\bsetModelStatus\b[^}]*\}\s*from\s*["']\.\.\/stores\/messages\.svelte["'];/s,
  '设置面板必须使用 messages store 的可追踪状态与显式模型状态访问器',
);

assert.match(
  settingsStoreSource,
  /const modelStatuses = \$derived\(getModelStatus\(\)\);/,
  '模型连接状态必须从可追踪 getter 派生',
);
assert.doesNotMatch(
  settingsStoreSource,
  /\$derived\(appState\.modelStatus\)/,
  '模型连接状态不能通过不可追踪的 getState() 返回对象派生',
);

const modelDraftMarker = settingsStoreSource.indexOf(
  '// 主模型 / 辅助模型 / 引擎草稿统一派生自单一事实源',
);
assert.notEqual(modelDraftMarker, -1, '必须保留模型表单草稿的单一事实源说明');
const modelDraftSection = settingsStoreSource.slice(modelDraftMarker);
const modelDraftEffect = modelDraftSection.match(
  /\$effect\(\(\)\s*=>\s*\{[\s\S]*?applyImageGenerationConfig\(snapshot\.imageGenerationConfig\);[\s\S]*?\}\);/,
);
assert.ok(modelDraftEffect, '必须保留从 settings bootstrap 派生模型表单草稿的唯一响应式入口');
assert.match(
  modelDraftEffect[0],
  /messagesState\.settingsBootstrapSnapshot/,
  '模型表单草稿必须直接追踪 settingsBootstrapSnapshot 的真实响应式状态',
);
assert.doesNotMatch(
  modelDraftEffect[0],
  /appState\.settingsBootstrapSnapshot/,
  '模型表单草稿不能通过不可追踪的 getState() 返回对象读取 snapshot',
);

assert.match(
  settingsStoreSource,
  /executionModelStats\s*=\s*payload\.models\.map/,
  '设置统计必须消费后端按模型身份聚合的 models 数据',
);
assert.match(
  settingsStoreSource,
  /resetAgentExecutionStats\(\);[\s\S]*?loadExecutionStats\(\)/,
  '重置统计必须统一清理累计统计，并从后端重新读取权威结果',
);
assert.match(
  settingsStoreSource,
  /totalInputTokens\s*=\s*toSafeTokenCount\(payload\.totals\.netInputTokens\)/,
  '统计总量必须使用权威 totals，不能把角色和模型两个维度重复相加',
);
assert.match(
  settingsStoreSource,
  /if\s*\(v\s*===\s*["']stats["']\)\s*\{[\s\S]*?loadExecutionStats\(\)/,
  '每次进入统计页都必须重新加载累计权威统计',
);
assert.match(
  settingsPanelSource,
  /bindingUsageStats=\{store\.executionStats\}[\s\S]*?modelUsageStats=\{store\.executionModelStats\}/,
  '设置面板必须把角色绑定和模型统计传给统计视图',
);
assert.doesNotMatch(settingsPanelSource, /statsScope|statsSession|sessionUsageStats/, '统计视图不能再暴露范围与会话切换');
const statsDisplayKeysSource = settingsStoreSource.match(
  /function getStatsDisplayKeys\(\): string\[\] \{[\s\S]*?return Array\.from\(keys\);\n  \}/,
);
assert.ok(statsDisplayKeysSource, '必须保留统计角色集合的唯一派生入口');
assert.match(
  statsDisplayKeysSource[0],
  /const orderedBuiltIns = \[[\s\S]*?orchestrator[\s\S]*?auxiliary[\s\S]*?imageGeneration[\s\S]*?executor[\s\S]*?explorer[\s\S]*?reviewer[\s\S]*?tester[\s\S]*?architect[\s\S]*?\]/,
  '角色统计必须包含固定顺序的主模型、辅助模型、图片模型和五类内置角色',
);
assert.doesNotMatch(
  statsDisplayKeysSource[0],
  /keys\.add\("orchestrator"\)|keys\.add\("auxiliary"\)/,
  '内置角色必须由固定顺序集合统一管理，不能在历史循环中重复追加',
);
assert.doesNotMatch(
  statsDisplayKeysSource[0],
  /item\.role === ["']orchestrator["']|item\.role === ["']auxiliary["']/,
  '主模型和辅助模型不能依赖历史事件循环决定是否展示',
);
assert.match(
  settingsStatsTabSource,
  /for\s*\(const model of \[\.\.\.modelUsageStats\][\s\S]*?model\.totals\.llmCallCount/,
  '模型视角必须从后端模型身份统计构建，不得再从角色当前配置反向推导',
);
assert.match(
  settingsStatsTabSource,
  /const key = resolvedModel\.toLocaleLowerCase\(\)/,
  '产品模型视角必须只按模型名称聚合',
);
assert.match(
  settingsStatsTabSource,
  /bucket\.identityKeys\.push\(model\.modelIdentityKey\)/,
  '模型聚合必须保留底层连接身份，以便准确关联来源与连接状态',
);
assert.doesNotMatch(
  settingsStatsTabSource,
  /for\s*\(const atom of roleAtoms\)[\s\S]*?buckets\.set/,
  '模型视角不能再按角色当前模型分桶，否则切换模型会丢失历史模型',
);
assert.match(
  settingsStatsTabSource,
  /type Perspective = ['"]role['"] \| ['"]engine['"]/,
  '统计面板只保留模型和角色两个维度',
);
assert.doesNotMatch(
  settingsStatsTabSource,
  /scopeSession|scopeWorkspace|perspectiveBySession|sessionUsageStats/,
  '统计面板不能再展示会话、工作区或轮次范围',
);
assert.match(
  usageAggregationSource,
  /resolvedModels:\s*string\[\][\s\S]*?resolvedModels,/,
  '角色聚合必须保留实际使用过的全部模型，不能只返回最后一个模型',
);
assert.doesNotMatch(
  settingsStatsTabSource,
  /settings\.stats\.configuredModel/,
  '角色累计统计不能用当前配置模型冒充历史使用模型',
);
assert.match(
  settingsStatsTabSource,
  /function aggregateUsageBreakdown\([\s\S]*?bucket\.totalIn \+= binding\.netInputTokens[\s\S]*?bucket\.successCount \+= binding\.successCount/s,
  '右侧交叉明细必须从角色绑定历史用量聚合调用、Token 和成功数',
);
assert.match(
  settingsStatsTabSource,
  /const selectedBreakdown = \$derived\.by[\s\S]*?identityKeys\.has\(binding\.modelIdentityKey as string\)/s,
  '点击模型时必须按模型身份筛选并展示角色明细',
);
assert.match(
  settingsStatsTabSource,
  /const roleKey = selectedRow\.roleAtom\?\.worker \|\| selectedRow\.key[\s\S]*?bindingRoleKey\(binding\) === roleKey/s,
  '点击角色时必须按角色身份筛选并展示模型明细',
);

const roleStats = aggregateUsageStatsForDisplay([
  {
    templateId: 'reviewer',
    engineId: 'shared-engine',
    role: 'worker',
    llmCallCount: 1,
    assignmentCount: 1,
    successCount: 1,
    failureCount: 0,
    totalTokens: 15,
    netInputTokens: 10,
    netOutputTokens: 5,
    resolvedModel: 'model-a',
  },
  {
    templateId: 'reviewer',
    engineId: 'replacement-engine',
    role: 'worker',
    llmCallCount: 1,
    assignmentCount: 1,
    successCount: 1,
    failureCount: 0,
    totalTokens: 28,
    netInputTokens: 20,
    netOutputTokens: 8,
    resolvedModel: 'model-b',
  },
  {
    templateId: 'implementer',
    engineId: 'shared-engine',
    role: 'worker',
    llmCallCount: 1,
    assignmentCount: 1,
    successCount: 1,
    failureCount: 0,
    totalTokens: 9,
    netInputTokens: 6,
    netOutputTokens: 3,
    resolvedModel: 'model-a',
  },
], 'reviewer');
assert.equal(roleStats?.totalExecutions, 2, '角色换引擎后必须继续聚合到同一个角色');
assert.equal(roleStats?.totalTokens, 43, '共享引擎的其他角色不能混入当前角色统计');
assert.deepEqual(roleStats?.resolvedModels.sort(), ['model-a', 'model-b']);

const imageStats = aggregateUsageStatsForDisplay([
  {
    templateId: 'imageGeneration',
    engineId: 'imageGeneration',
    role: 'image_generation',
    llmCallCount: 3,
    assignmentCount: 0,
    successCount: 2,
    failureCount: 1,
    totalTokens: 0,
    netInputTokens: 0,
    netOutputTokens: 0,
    resolvedModel: 'gpt-image-test',
  },
], 'imageGeneration');
assert.equal(imageStats?.totalExecutions, 3, '图片模型必须按生成调用次数统计');
assert.equal(imageStats?.successCount, 2, '图片模型成功与失败调用必须进入统一统计');
assert.equal(imageStats?.totalTokens, 0, '图片接口未返回 usage 时不能伪造 Token');
assert.deepEqual(imageStats?.resolvedModels, ['gpt-image-test']);

assert.match(
  settingsStatsTabSource,
  /binding\.role === ['"]image_generation['"][\s\S]*?return ['"]imageGeneration['"]/,
  '图片模型账本角色必须映射到产品角色标识',
);
assert.match(
  settingsStatsTabSource,
  /settings\.stats\.imageUsageMetricHint/,
  '图片接口未返回 usage 时必须解释调用次数与 Token 的统计口径',
);

console.log('settings store golden tests passed');
