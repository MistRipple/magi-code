#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

function testSourceGuardrails() {
  const runtimeSource = fs.readFileSync(
    path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'lib', 'worker-panel-state.ts'),
    'utf8',
  );
  const waitCardSource = fs.readFileSync(
    path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'components', 'WaitResultCard.svelte'),
    'utf8',
  );

  assert(
    !runtimeSource.includes('const hasRunningSignal = hasRunningTask || hasStreaming || hasPendingRequest;'),
    'worker runtime 仍把 pendingRequest 直接视为 running',
  );
  assert(
    runtimeSource.includes('} else if (hasPending) {'),
    'worker runtime 未将 pending 与 running 明确分离',
  );
  assert(
    waitCardSource.includes('runtimeStatusLabelKeyMap'),
    'WaitResultCard 缺少运行态状态文案映射',
  );
}

function createTask(worker, status) {
  return {
    id: `task-${worker}-${status}`,
    title: `${worker}-${status}`,
    status: 'running',
    subTasks: [
      {
        id: `sub-${worker}-${status}`,
        title: `${worker}-${status}`,
        assignedWorker: worker,
        status,
        startedAt: 123,
      },
    ],
  };
}

function main() {
  testSourceGuardrails();
  const { deriveWorkerRuntimeState, deriveWorkerMessageContext } = loadCompiledModule(
    path.join('ui', 'webview-svelte', 'src', 'lib', 'worker-panel-state.js'),
  );

  const completedMessages = [
    {
      id: 'instruction-completed',
      type: 'instruction',
      source: 'orchestrator',
      content: 'Phase 1 done',
      timestamp: 1000,
      isStreaming: false,
      metadata: {
        worker: 'gemini',
        requestId: 'req-completed',
        laneTasks: [
          { taskId: 'task-gemini-completed', status: 'completed', taskName: 'frontend fix' },
        ],
      },
    },
    {
      id: 'output-completed',
      type: 'text',
      source: 'gemini',
      content: 'done',
      timestamp: 1100,
      isStreaming: false,
      metadata: {},
    },
  ];
  const completedContext = deriveWorkerMessageContext({
    messages: completedMessages,
    workerName: 'gemini',
    pendingRequestIds: new Set(['req-completed']),
  });
  const completedRuntime = deriveWorkerRuntimeState(
    {
      messages: completedMessages,
      workerName: 'gemini',
      pendingRequestIds: new Set(['req-completed']),
      tasks: [createTask('gemini', 'completed')],
    },
    completedContext,
  );
  assert(completedRuntime.status === 'completed', `完成态 worker 不应继续 running，实际为 ${completedRuntime.status}`);
  assert(completedRuntime.isExecuting === false, '完成态 worker 不应继续 breathing');

  const pendingMessages = [
    {
      id: 'instruction-pending',
      type: 'instruction',
      source: 'orchestrator',
      content: 'wait next phase',
      timestamp: 2000,
      isStreaming: false,
      metadata: {
        worker: 'gemini',
        requestId: 'req-pending',
        laneTasks: [
          { taskId: 'task-gemini-pending', status: 'pending', taskName: 'frontend verify' },
        ],
      },
    },
  ];
  const pendingContext = deriveWorkerMessageContext({
    messages: pendingMessages,
    workerName: 'gemini',
    pendingRequestIds: new Set(['req-pending']),
  });
  const pendingRuntime = deriveWorkerRuntimeState(
    {
      messages: pendingMessages,
      workerName: 'gemini',
      pendingRequestIds: new Set(['req-pending']),
      tasks: [createTask('gemini', 'pending')],
    },
    pendingContext,
  );
  assert(pendingRuntime.status === 'pending', `待执行 worker 不应被提升为 running，实际为 ${pendingRuntime.status}`);
  assert(pendingRuntime.isExecuting === false, 'pending 态 worker 不应继续 breathing');

  console.log('\n=== worker runtime status regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'completed_worker_not_kept_running_by_pending_request',
      'pending_worker_not_promoted_to_running',
      'wait_result_card_uses_runtime_status_labels',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('worker runtime status 回归失败:', error?.stack || error);
  process.exit(1);
}
