import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

const settingsStoreSource = await readFile(
  new URL('../src/stores/settings-store.svelte.ts', import.meta.url),
  'utf8',
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

console.log('settings store golden tests passed');
