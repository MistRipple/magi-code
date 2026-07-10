import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const panel = await server.ssrLoadModule('/src/lib/runtime-state-panel.ts');

  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'idle', isProcessing: false, assignmentCount: 0 }),
    false,
    'idle sessions without active assignments must not reserve homepage space',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'running', isProcessing: false, assignmentCount: 0 }),
    true,
    'active runtime state must remain visible',
  );
  assert.equal(
    panel.shouldShowRuntimePanel({ status: 'idle', isProcessing: false, assignmentCount: 2 }),
    true,
    'active assignments keep the runtime panel visible even if the aggregate status is stale',
  );

  assert.equal(panel.shouldShowRuntimePhase('running', 'running'), false);
  assert.equal(panel.shouldShowRuntimePhase('idle', 'idle'), false);
  assert.equal(panel.shouldShowRuntimePhase('running', 'verify'), true);

  assert.equal(panel.shouldShowRuntimeBudget('normal'), false);
  assert.equal(panel.shouldShowRuntimeBudget(undefined), false);
  assert.equal(panel.shouldShowRuntimeBudget('notice'), true);
  assert.equal(panel.shouldShowRuntimeBudget('warning'), true);
  assert.equal(panel.shouldShowRuntimeBudget('danger'), true);

  assert.equal(panel.shouldShowRuntimeCache('healthy'), false);
  assert.equal(panel.shouldShowRuntimeCache('cold'), false);
  assert.equal(panel.shouldShowRuntimeCache('degraded'), true);

  assert.deepEqual(
    panel.resolveRuntimeTaskProgress({
      requiredTotal: 5,
      failedRequired: 1,
      runningOrPendingRequired: 2,
    }),
    { completed: 2, failed: 1, running: 2, total: 5, percent: 40 },
  );
  assert.equal(panel.resolveRuntimeTaskProgress(undefined), null);

  console.log('runtime state panel golden passed');
});
