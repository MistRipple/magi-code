import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const sessionConfig = await server.ssrLoadModule('/src/shared/orchestrator-session-config.ts');

  assert.equal(
    sessionConfig.resolveOrchestratorReasoningEffort({}, {}),
    'medium',
    '新会话必须具有明确的中等推理强度默认值',
  );

  assert.equal(
    sessionConfig.resolveOrchestratorModel({}, {}, ['', 'model-first', 'model-second']),
    'model-first',
    '未选择主模型时必须使用模型列表中的第一个有效模型',
  );

  assert.equal(
    sessionConfig.resolveOrchestratorModel(
      { model: 'session-model' },
      { model: 'effective-model' },
      ['model-first'],
    ),
    'session-model',
    '用户已经选择的会话模型必须优先于自动默认模型',
  );

  assert.equal(
    sessionConfig.resolveOrchestratorModel({}, {}, []),
    '',
    '模型配置不可用时不得伪造模型名称',
  );

  assert.equal(
    sessionConfig.resolveOrchestratorReasoningEffort(
      { reasoningEffort: 'high' },
      { reasoningEffort: 'medium' },
    ),
    'high',
    '会话级推理强度必须优先于有效配置',
  );

  assert.deepEqual(
    sessionConfig.withOrchestratorReasoningEffort(
      { model: 'model-1', reasoningEffort: 'high' },
      'high',
      { model: 'model-2' },
    ),
    { model: 'model-2', reasoningEffort: 'high' },
    '切换模型必须沿用当前会话的推理强度',
  );

  assert.deepEqual(
    sessionConfig.withOrchestratorReasoningEffort(
      {},
      'medium',
      { model: 'model-1' },
    ),
    { model: 'model-1', reasoningEffort: 'medium' },
    '草稿会话首次选择模型时必须同时固化默认强度',
  );
});

const inputAreaSource = await readFile(new URL('../src/components/InputArea.svelte', import.meta.url), 'utf8');
assert.match(
  inputAreaSource,
  /const orchestratorSessionConfig = resolveTurnOrchestratorSessionConfigPayload\(\);[\s\S]*?if \(!orchestratorSessionConfig\) \{[\s\S]*?return;/,
  '回车发送必须同步读取当前模型快照，模型不可用时不得提交空配置',
);
assert.match(
  inputAreaSource,
  /let pickerLoadPromise: Promise<void> \| null = null;/,
  '模型列表初始化必须复用同一个加载任务，避免首次发送与自动加载竞态',
);
assert.match(
  inputAreaSource,
  /if \(!pickerLoadedOnce && !pickerLoading\) \{[\s\S]*?void loadPickerModels\(\);/,
  '模型快照不可用时必须打开选择器并后台加载模型，不得阻塞发送交互',
);
assert.match(
  inputAreaSource,
  /function handleStoredAccessProfileChange\(event: StorageEvent\)[\s\S]*?readStoredAccessProfile\(\)[\s\S]*?messagesState\.settingsBootstrapSnapshot = latest;/,
  '访问模式变更必须通过 storage 事件同步到其他窗口并刷新当前会话设置快照',
);

console.log('orchestrator session config golden tests passed');
