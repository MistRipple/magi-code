#!/usr/bin/env node
/**
 * Todo 分层与来源治理回归
 *
 * 覆盖目标：
 * 1) Todo 来源进入统一数据模型，并在关键创建入口落值。
 * 2) todo_split 具备结构化子任务约束，避免机械切碎任务。
 * 3) Todo 来源可透传到任务视图，供用户识别任务增长原因。
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

function testSourceModelAndEntrypoints() {
  const todoTypes = read('src/todo/types.ts');
  const todoManager = read('src/todo/todo-manager.ts');
  const planningExecutor = read('src/orchestrator/core/executors/planning-executor.ts');
  const dispatchManager = read('src/orchestrator/core/dispatch-manager.ts');
  const worker = read('src/orchestrator/worker/autonomous-worker.ts');
  const workerPipeline = read('src/orchestrator/core/worker-pipeline.ts');

  assert(
    todoTypes.includes("export type TodoSource =")
      && todoTypes.includes("source: TodoSource;"),
    'UnifiedTodo 未建模 Todo 来源字段',
  );
  assert(
    todoManager.includes("source: params.source ?? 'planner_macro'"),
    'TodoManager.create 未提供统一来源落值',
  );
  assert(
    planningExecutor.includes("source: 'planner_macro'"),
    '一级 Todo 创建入口未标记 planner_macro',
  );
  assert(
    dispatchManager.includes("source: 'worker_split'")
      && dispatchManager.includes('expectedOutput: subtask.expectedOutput'),
    'todo_split 子 Todo 未标记 worker_split 或未携带 expectedOutput',
  );
  assert(
    worker.includes("source: 'review_fix'")
      && worker.includes("'orchestrator_adjustment'"),
    '运行期 fix/adjustment Todo 未标记来源',
  );
  assert(
    workerPipeline.includes("source: 'system_repair'"),
    '系统修复 Todo 未标记 system_repair',
  );
}

function testSplitGuardrails() {
  const orchestrationExecutor = read('src/tools/orchestration-executor.ts');

  assert(
    orchestrationExecutor.includes('maxItems: 8')
      && orchestrationExecutor.includes('expected_output')
      && orchestrationExecutor.includes('target_files'),
    'todo_split schema 未限制规模或缺少结构化字段',
  );
  assert(
    orchestrationExecutor.includes('子步骤 content 不可重复')
      && orchestrationExecutor.includes('每个子步骤必须有非空的 expected_output'),
    'todo_split 运行时校验未覆盖重复子项或 expected_output',
  );
}

function testUiProjection() {
  const taskViewAdapter = read('src/task/task-view-adapter.ts');
  const tasksPanel = read('src/ui/webview-svelte/src/components/TasksPanel.svelte');
  const messageTypes = read('src/ui/webview-svelte/src/types/message.ts');
  const messageHandler = read('src/ui/webview-svelte/src/lib/message-handler.ts');

  assert(
    taskViewAdapter.includes('source: UnifiedTodo[\'source\']')
      && taskViewAdapter.includes('source: todo.source'),
    '任务视图未透传 Todo 来源',
  );
  assert(
    messageTypes.includes('source?: string;')
      && messageHandler.includes('source: typeof st.source === \'string\' ? st.source : undefined'),
    '前端消息模型未接入 Todo 来源字段',
  );
  assert(
    tasksPanel.includes('getTodoSourceLabel')
      && tasksPanel.includes('todo-source'),
    '任务面板未展示 Todo 来源标识',
  );
}

function testRuntimeProjection() {
  const { todoToTodoItemView } = loadCompiledModule(path.join('task', 'task-view-adapter.js'));
  const view = todoToTodoItemView({
    id: 'todo-1',
    sessionId: 'session-1',
    missionId: 'mission-1',
    assignmentId: 'assignment-1',
    source: 'worker_split',
    content: '拆分后的实现子任务',
    reasoning: '需要先完成独立子目标',
    expectedOutput: '提交子模块改动并通过验证',
    type: 'implementation',
    workerId: 'codex',
    priority: 2,
    dependsOn: [],
    requiredContracts: [],
    producesContracts: [],
    outOfScope: false,
    status: 'pending',
    progress: 0,
    retryCount: 0,
    maxRetries: 3,
    createdAt: Date.now(),
  }, 'mission-1');

  assert(view.source === 'worker_split', 'TodoItemView 运行时未保留 source');
  assert(view.description === '拆分后的实现子任务', 'TodoItemView 运行时映射异常');
}

function main() {
  testSourceModelAndEntrypoints();
  testSplitGuardrails();
  testUiProjection();
  testRuntimeProjection();
  console.log('\n=== todo structure guardrails regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'todo-source-model',
      'split-todo-structure-guardrails',
      'ui-source-projection',
      'runtime-source-projection',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('todo structure guardrails 回归失败:', error?.stack || error);
  process.exit(1);
}
