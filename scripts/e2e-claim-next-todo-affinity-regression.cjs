#!/usr/bin/env node
/**
 * todo_claim_next 上下文亲和度回归
 *
 * 覆盖目标：
 * 1) findClaimable 必须禁止跨 Worker 认领。
 * 2) todo_claim_next 只能选择与当前上下文有足够亲和度的候选 Todo。
 * 3) 当不存在足够亲和的候选时，应 fail-closed，而不是跨上下文续领。
 */

const fs = require('fs');
const os = require('os');
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
  const todoManager = read('src/todo/todo-manager.ts');
  const dispatchManager = read('src/orchestrator/core/dispatch-manager.ts');
  const orchestrationExecutor = read('src/tools/orchestration-executor.ts');
  const workerPersonas = read('src/orchestrator/profile/builtin/worker-personas.ts');

  assert(
    todoManager.includes('if (workerId && todo.workerId !== workerId) {'),
    'findClaimable 未禁止跨 Worker 认领',
  );
  assert(
    dispatchManager.includes('selectClaimNextTodoCandidate(claimable, {')
      && dispatchManager.includes('禁止 todo_claim_next 跨上下文续领'),
    'DispatchManager 未接入 todo_claim_next 上下文亲和度守卫',
  );
  assert(
    orchestrationExecutor.includes('系统只会自动续领同一 Assignment 或共享目标文件的 Todo'),
    'todo_claim_next 工具描述未声明上下文亲和度约束',
  );
  assert(
    workerPersonas.includes('context-adjacent task')
      && workerPersonas.includes('no context-affine tasks are available'),
    'Worker persona 未同步 todo_claim_next 亲和度约束',
  );
}

async function testWorkerBoundaryAtRuntime() {
  const { TodoManager } = loadCompiledModule(path.join('todo', 'index.js'));
  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-claim-affinity-'));
  const todoManager = new TodoManager(workspaceRoot);
  try {
    await todoManager.initialize();
    const missionId = 'mission-affinity-runtime';
    await todoManager.create({
      sessionId: 'session-affinity-runtime',
      missionId,
      assignmentId: 'assignment-codex',
      content: 'codex task',
      reasoning: 'owned by codex',
      type: 'implementation',
      workerId: 'codex',
      targetFiles: ['src/a.ts'],
    });
    await todoManager.create({
      sessionId: 'session-affinity-runtime',
      missionId,
      assignmentId: 'assignment-claude',
      content: 'claude task',
      reasoning: 'owned by claude',
      type: 'implementation',
      workerId: 'claude',
      targetFiles: ['src/b.ts'],
    });

    const claimable = await todoManager.findClaimable(missionId, 'codex');
    assert(claimable.length === 1, 'findClaimable 应只返回当前 Worker 的 Todo');
    assert(claimable[0].workerId === 'codex', 'findClaimable 返回了跨 Worker Todo');
  } finally {
    todoManager.destroy();
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }
}

function testAffinitySelectionRuntime() {
  const {
    selectClaimNextTodoCandidate,
    evaluateClaimNextTodoAffinity,
  } = loadCompiledModule(path.join('orchestrator', 'core', 'claim-next-todo-affinity.js'));

  const context = {
    currentAssignmentId: 'assignment-1',
    currentTodoId: 'todo-current',
    currentTargetFiles: ['src/auth/login.ts', 'src/auth/session.ts'],
  };

  const sameAssignment = {
    id: 'todo-same-assignment',
    assignmentId: 'assignment-1',
    priority: 4,
    targetFiles: [],
  };
  const sharedTarget = {
    id: 'todo-shared-target',
    assignmentId: 'assignment-2',
    priority: 1,
    targetFiles: ['src/auth/session.ts'],
  };
  const unrelated = {
    id: 'todo-unrelated',
    assignmentId: 'assignment-3',
    priority: 1,
    targetFiles: ['src/other/file.ts'],
  };

  const selectedPreferred = selectClaimNextTodoCandidate(
    [sharedTarget, sameAssignment, unrelated],
    context,
  );
  assert(selectedPreferred.selected && selectedPreferred.selected.id === 'todo-same-assignment', '同 Assignment 候选应优先于仅共享目标文件的候选');
  assert(selectedPreferred.affinity.level === 'same_assignment', '同 Assignment 候选应命中 same_assignment 亲和度');

  const selectedSharedTarget = selectClaimNextTodoCandidate(
    [sharedTarget, unrelated],
    context,
  );
  assert(selectedSharedTarget.selected && selectedSharedTarget.selected.id === 'todo-shared-target', '共享目标文件候选应可被选中');
  assert(selectedSharedTarget.affinity.level === 'shared_target_files', '共享目标文件候选应命中 shared_target_files 亲和度');

  const blocked = selectClaimNextTodoCandidate([unrelated], context);
  assert(blocked.selected === null, '无亲和度候选应 fail-closed');
  assert(blocked.affinity.level === 'none', '无亲和度候选应返回 none');

  const selfAffinity = evaluateClaimNextTodoAffinity({
    id: 'todo-current',
    assignmentId: 'assignment-1',
    targetFiles: ['src/auth/login.ts'],
  }, context);
  assert(selfAffinity.level === 'none', '当前 Todo 自身不应被重复认领');
}

async function main() {
  testSourceGuards();
  await testWorkerBoundaryAtRuntime();
  testAffinitySelectionRuntime();
  console.log('\n=== claim next todo affinity regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'worker-boundary-hard-filter',
      'same-assignment-priority',
      'shared-target-affinity',
      'low-affinity-fail-closed',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('todo_claim_next affinity 回归失败:', error?.stack || error);
  process.exit(1);
});
