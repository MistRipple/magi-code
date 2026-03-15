#!/usr/bin/env node
/**
 * Worker Attention Guard 回归
 *
 * 覆盖目标：
 * 1) 连续多轮只读探索/无实质输出时，会注入当前 todo 聚焦提醒。
 * 2) 出现写入推进或 todo 边界动作后，提醒计数会重置。
 * 3) 运行时链路仍通过 AutonomousWorker -> decisionHook -> WorkerAdapter 传递。
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function read(relPath) {
  return fs.readFileSync(path.join(ROOT, relPath), 'utf8');
}

function loadCompiledModule(relPath) {
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

function testSourceGuards() {
  const workerAdapter = read('src/llm/adapters/worker-adapter.ts');
  const autonomousWorker = read('src/orchestrator/worker/autonomous-worker.ts');
  const attentionGuard = read('src/orchestrator/worker/todo-attention-guard.ts');

  assert(
    workerAdapter.includes("toolNames: toolCalls.map((toolCall) => toolCall.name)")
      && workerAdapter.includes('allReadOnly: allReadOnlyRound')
      && workerAdapter.includes('noSubstantiveOutput: noSubstantiveOutputRound'),
    'WorkerAdapter 未向 decisionHook 透传工具轮治理信号',
  );
  assert(
    autonomousWorker.includes('createTodoAttentionGuard({')
      && autonomousWorker.includes('const decisionHook = (event: DecisionHookEvent) => {'),
    'AutonomousWorker 未接入 todo attention guard',
  );
  assert(
    attentionGuard.includes('SOFT_REMINDER_THRESHOLD')
      && attentionGuard.includes('todo_claim_next')
      && attentionGuard.includes('todo_split'),
    'Todo attention guard 缺少核心阈值或边界工具约束',
  );
}

function testRuntimeBehavior() {
  const { createTodoAttentionGuard } = loadCompiledModule(path.join('orchestrator', 'worker', 'todo-attention-guard.js'));
  const guard = createTodoAttentionGuard({
    todoContent: '实现用户登录流程',
    expectedOutput: '交付登录接口、表单校验和基础测试',
    targetPaths: ['src/auth/login.ts', 'src/auth/login.test.ts'],
    allowSplitTodo: true,
  });

  const readOnlyRound = {
    type: 'tool_result',
    toolNames: ['file_view'],
    allReadOnly: true,
    hadWriteTool: false,
    noSubstantiveOutput: true,
  };

  for (let i = 0; i < 3; i += 1) {
    const reminders = guard(readOnlyRound);
    assert(reminders.length === 0, `第 ${i + 1} 轮不应提前注入提醒`);
  }

  const softReminders = guard(readOnlyRound);
  assert(softReminders.length === 1, '第 4 轮应注入一级聚焦提醒');
  assert(softReminders[0].includes('实现用户登录流程'), '一级提醒缺少当前 todo');
  assert(softReminders[0].includes('交付登录接口、表单校验和基础测试'), '一级提醒缺少预期输出');

  const fifth = guard(readOnlyRound);
  assert(fifth.length === 0, '同级提醒不应重复注入');

  const hardReminders = guard(readOnlyRound);
  assert(hardReminders.length === 1, '第 6 轮应注入二级聚焦提醒');
  assert(hardReminders[0].includes('todo_split'), '二级提醒应建议 todo_split');
  assert(hardReminders[0].includes('todo_claim_next'), '二级提醒应提及 todo_claim_next 边界');

  const resetByWrite = guard({
    type: 'tool_result',
    toolNames: ['file_edit'],
    allReadOnly: false,
    hadWriteTool: true,
    noSubstantiveOutput: false,
  });
  assert(resetByWrite.length === 0, '写入推进轮不应继续提醒');

  for (let i = 0; i < 4; i += 1) {
    const reminders = guard(readOnlyRound);
    if (i < 3) {
      assert(reminders.length === 0, '重置后前 3 轮不应提醒');
    } else {
      assert(reminders.length === 1, '重置后应可再次触发一级提醒');
    }
  }

  const resetByBoundary = guard({
    type: 'tool_result',
    toolNames: ['todo_claim_next'],
    allReadOnly: false,
    hadWriteTool: false,
    noSubstantiveOutput: false,
  });
  assert(resetByBoundary.length === 0, 'todo 边界动作不应继续提醒');
}

function main() {
  testSourceGuards();
  testRuntimeBehavior();
  console.log('\n=== worker attention guard regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'decision-hook-runtime-signals',
      'todo-focus-reminder-thresholds',
      'write-and-boundary-reset',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('worker attention guard 回归失败:', error?.stack || error);
  process.exit(1);
}
