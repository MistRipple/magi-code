import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const sessionConfig = await server.ssrLoadModule('/src/shared/orchestrator-session-config.ts');

  assert.equal(
    sessionConfig.resolveOrchestratorReasoningEffort({}, {}),
    'medium',
    '新会话必须具有明确的中等推理强度默认值',
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

console.log('orchestrator session config golden tests passed');
