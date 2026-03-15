#!/usr/bin/env node
/**
 * Worktree 运行保障回归脚本
 *
 * 覆盖目标：
 * 1) merge 冲突必须返回结构化解释（summary + hints）
 * 2) 孤儿 worktree/分支必须可对账清理
 */

const fs = require('fs');
const os = require('os');
const path = require('path');
const { execSync } = require('child_process');

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

function run(cmd, cwd) {
  return execSync(cmd, {
    cwd,
    encoding: 'utf-8',
    stdio: ['pipe', 'pipe', 'pipe'],
  }).trim();
}

function setupTempRepo() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-worktree-guardrails-'));
  run('git init', dir);
  run('git config user.name "Magi Regression"', dir);
  run('git config user.email "magi-regression@example.com"', dir);
  fs.writeFileSync(path.join(dir, 'app.txt'), 'base-line\n', 'utf8');
  run('git add app.txt', dir);
  run('git commit -m "init"', dir);
  return dir;
}

function testMergeConflictGuidance(repoDir, WorktreeManager) {
  const manager = new WorktreeManager(repoDir);
  const taskId = 'merge-conflict-case';
  const allocation = manager.acquire(taskId);
  assert(allocation && allocation.worktreePath, 'worktree acquire 失败');

  fs.writeFileSync(path.join(allocation.worktreePath, 'app.txt'), 'worker-line\n', 'utf8');
  fs.writeFileSync(path.join(repoDir, 'app.txt'), 'main-line\n', 'utf8');
  run('git add app.txt', repoDir);
  run('git commit -m "main-side-change"', repoDir);

  const mergeResult = manager.merge(taskId);
  assert(mergeResult.hasConflicts === true, '预期 merge 冲突未触发');
  assert(typeof mergeResult.conflictSummary === 'string' && mergeResult.conflictSummary.length > 0, '缺少冲突摘要');
  assert(Array.isArray(mergeResult.conflictHints) && mergeResult.conflictHints.length > 0, '缺少冲突修复建议');

  manager.release(taskId);
}

function testOrphanReconcile(repoDir, WorktreeManager) {
  const manager = new WorktreeManager(repoDir);
  const orphanId = 'orphan_clean_case';
  const orphanDir = path.join(repoDir, '.magi', 'worktrees', `task-${orphanId}`);
  fs.mkdirSync(orphanDir, { recursive: true });
  fs.writeFileSync(path.join(orphanDir, 'dummy.txt'), 'orphan\n', 'utf8');
  run(`git branch magi/worker/${orphanId}`, repoDir);

  const reconcile = manager.reconcileOrphanedWorktrees();
  assert(!fs.existsSync(orphanDir), '孤儿 worktree 目录未被清理');
  const branchExists = run(`git branch --list magi/worker/${orphanId}`, repoDir);
  assert(!branchExists, '孤儿 worktree 分支未被清理');
  assert(
    reconcile.removedWorktrees.length > 0 || reconcile.removedBranches.length > 0,
    '对账清理未产生任何结果',
  );
}

function cleanup(repoDir) {
  if (!repoDir || !fs.existsSync(repoDir)) {
    return;
  }
  fs.rmSync(repoDir, { recursive: true, force: true });
}

async function testWriteTaskNonGitWorkspaceSerialFallback(WorkerPipeline) {
  const pipeline = new WorkerPipeline();
  let workerExecuted = false;
  let receivedWorkingDirectory = null;
  let acquireCalled = false;

  const assignment = {
    id: 'write-task-non-git-fallback',
    missionId: 'mission-non-git-fallback',
    workerId: 'codex',
    responsibility: 'write guarded file',
    scope: {
      requiresModification: true,
      targetPaths: ['app.txt'],
    },
    todos: [],
  };

  const result = await pipeline.execute({
    assignment,
    workerInstance: {
      async executeAssignment(_assignment, options) {
        workerExecuted = true;
        receivedWorkingDirectory = options.workingDirectory;
        return {
          assignment,
          success: true,
          completedTodos: [],
          failedTodos: [],
          skippedTodos: [],
          dynamicTodos: [],
          recoveredTodos: [],
          totalDuration: 0,
          errors: [],
          recoveryAttempts: 0,
          summary: 'workspace serial fallback executed',
          verification: {
            attempted: false,
            degraded: false,
            warnings: [],
            rounds: 0,
          },
        };
      },
    },
    adapterFactory: {
      getToolManager() {
        return {
          setSnapshotContext() {},
          clearSnapshotContext() {},
        };
      },
    },
    workspaceRoot: process.cwd(),
    enableSnapshot: false,
    enableLSP: false,
    enableTargetEnforce: false,
    enableContextUpdate: false,
    worktreeManager: {
      acquire() {
        acquireCalled = true;
        return null;
      },
      isGitRepository() { return false; },
    },
  });

  assert(result.executionResult.success === true, '非 Git 工作区写任务应自动降级为串行执行');
  assert(workerExecuted === true, '非 Git 工作区降级后仍应执行 worker');
  assert(acquireCalled === false, '非 Git 工作区不应尝试申请 worktree');
  assert(receivedWorkingDirectory === process.cwd(), `非 Git 工作区应直接使用主工作区执行，实际: ${receivedWorkingDirectory}`);
  assert(result.worktreeMerge === undefined, '非 Git 工作区降级执行不应产生 worktree merge 结果');
}

async function main() {
  const { WorktreeManager } = loadCompiledModule(path.join('workspace', 'worktree-manager.js'));
  const { WorkerPipeline } = loadCompiledModule(path.join('orchestrator', 'core', 'worker-pipeline.js'));
  const repoDir = setupTempRepo();
  try {
    testMergeConflictGuidance(repoDir, WorktreeManager);
    testOrphanReconcile(repoDir, WorktreeManager);
    await testWriteTaskNonGitWorkspaceSerialFallback(WorkerPipeline);
    console.log('\n=== worktree runtime guardrails regression ===');
    console.log(JSON.stringify({
      pass: true,
      checks: [
        'merge-conflict-guidance',
        'orphan-reconcile',
        'write-task-non-git-workspace-serial-fallback',
      ],
    }, null, 2));
  } finally {
    cleanup(repoDir);
  }
}

main().catch((error) => {
  console.error('worktree runtime guardrails 回归失败:', error?.stack || error);
  process.exit(1);
});
