#!/usr/bin/env node
/**
 * Dispatch 幂等账本回归脚本
 *
 * 覆盖目标：
 * 1) 幂等记录可跨实例持久化加载
 * 2) task 状态回写（dispatched -> completed）可落盘
 * 3) 过期记录可被清理
 * 4) 原子 claimOrGet 可防止并发重复占位
 * 5) DispatchManager 已接入原子幂等路径（源码守卫）
 */

const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawn } = require('child_process');

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
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run compile`);
  }
  return require(abs);
}

function testSourceGuardrails() {
  const source = fs.readFileSync(path.join(ROOT, 'src', 'orchestrator', 'core', 'dispatch-manager.ts'), 'utf8');
  assert(source.includes('dispatchIdempotencyStore.claimOrGet'), 'DispatchManager 缺少原子幂等 claim 接入');
  assert(source.includes('dispatchIdempotencyStore.removeByTaskId'), 'DispatchManager 缺少幂等占位回滚接入');
  assert(source.includes('幂等占位失败'), 'DispatchManager 缺少幂等占位失败显式错误处理');
  assert(source.includes('buildDispatchIdempotencyKey('), 'DispatchManager 缺少幂等键构建逻辑');
}

function testStorePersistence(DispatchIdempotencyStore, workspaceRoot) {
  const storeA = new DispatchIdempotencyStore(workspaceRoot, {
    ttlMs: 60_000,
    maxRecords: 100,
  });
  const key = 'session-1::mission-1::provided:abc';
  const first = storeA.claimOrGet({
    key,
    sessionId: 'session-1',
    missionId: 'mission-1',
    taskId: 'task-1',
    worker: 'codex',
    category: 'general',
    taskName: 'task-name',
    routingReason: 'route',
    degraded: false,
    status: 'dispatched',
  });
  assert(first.claimed === true, '首个 claim 应成功');

  const storeB = new DispatchIdempotencyStore(workspaceRoot, {
    ttlMs: 60_000,
    maxRecords: 100,
  });
  const loaded = storeB.resolveByKey(key);
  assert(loaded, '幂等记录未持久化');
  assert(loaded.taskId === 'task-1', `幂等记录 taskId 异常: ${loaded.taskId}`);
  assert(loaded.status === 'dispatched', `幂等记录 status 异常: ${loaded.status}`);

  storeB.updateStatusByTaskId('task-1', 'completed');

  const storeC = new DispatchIdempotencyStore(workspaceRoot, {
    ttlMs: 60_000,
    maxRecords: 100,
  });
  const completed = storeC.resolveByKey(key);
  assert(completed && completed.status === 'completed', `幂等状态回写异常: ${completed?.status}`);
}

function testAtomicClaim(DispatchIdempotencyStore, workspaceRoot) {
  const key = 'session-atomic::mission-atomic::provided:same-key';
  const storeA = new DispatchIdempotencyStore(workspaceRoot, {
    ttlMs: 60_000,
    maxRecords: 100,
  });
  const storeB = new DispatchIdempotencyStore(workspaceRoot, {
    ttlMs: 60_000,
    maxRecords: 100,
  });

  const claimA = storeA.claimOrGet({
    key,
    sessionId: 'session-atomic',
    missionId: 'mission-atomic',
    taskId: 'task-atomic-A',
    worker: 'codex',
    category: 'general',
    taskName: 'atomic-A',
    routingReason: 'route-A',
    degraded: false,
    status: 'dispatched',
  });
  const claimB = storeB.claimOrGet({
    key,
    sessionId: 'session-atomic',
    missionId: 'mission-atomic',
    taskId: 'task-atomic-B',
    worker: 'claude',
    category: 'general',
    taskName: 'atomic-B',
    routingReason: 'route-B',
    degraded: false,
    status: 'dispatched',
  });

  assert(claimA.claimed === true, '第一个 claim 应成功');
  assert(claimB.claimed === false, '第二个 claim 应复用已有记录');
  assert(claimB.record.taskId === 'task-atomic-A', `第二个 claim 应复用 task-atomic-A，实际: ${claimB.record.taskId}`);
}

function testStorePrune(DispatchIdempotencyStore, workspaceRoot) {
  const runtimeDir = path.join(workspaceRoot, '.magi', 'runtime');
  const filePath = path.join(runtimeDir, 'dispatch-idempotency.json');
  fs.mkdirSync(runtimeDir, { recursive: true });
  fs.writeFileSync(filePath, JSON.stringify({
    version: 1,
    records: [{
      key: 'expired-key',
      sessionId: 'session-expired',
      missionId: 'mission-expired',
      taskId: 'task-expired',
      worker: 'claude',
      category: 'general',
      taskName: 'expired',
      routingReason: 'expired',
      degraded: false,
      status: 'dispatched',
      createdAt: Date.now() - 120_000,
      updatedAt: Date.now() - 120_000,
    }],
  }, null, 2), 'utf8');

  const store = new DispatchIdempotencyStore(workspaceRoot, {
    ttlMs: 1_000,
    maxRecords: 100,
  });
  const expired = store.resolveByKey('expired-key');
  assert(expired === null, '过期记录未被清理');
}

function testStaleLockRecovery(DispatchIdempotencyStore, workspaceRoot) {
  const runtimeDir = path.join(workspaceRoot, '.magi', 'runtime');
  const lockPath = path.join(runtimeDir, 'dispatch-idempotency.json.lock');
  fs.mkdirSync(runtimeDir, { recursive: true });
  fs.writeFileSync(lockPath, JSON.stringify({ pid: 999999, acquiredAt: Date.now() - 10_000 }), 'utf8');
  const staleSeconds = (Date.now() - 10_000) / 1000;
  fs.utimesSync(lockPath, staleSeconds, staleSeconds);

  const store = new DispatchIdempotencyStore(workspaceRoot, {
    ttlMs: 60_000,
    maxRecords: 100,
    lockStaleMs: 200,
    lockAcquireTimeoutMs: 1000,
    lockRetryMs: 5,
  });
  const result = store.claimOrGet({
    key: 'stale-lock-key',
    sessionId: 'session-stale',
    missionId: 'mission-stale',
    taskId: 'task-stale',
    worker: 'codex',
    category: 'general',
    taskName: 'stale-lock',
    routingReason: 'stale-lock',
    degraded: false,
    status: 'dispatched',
  });
  assert(result.claimed === true, '陈旧锁回收后 claim 应成功');
}

function testLockTimeout(DispatchIdempotencyStore, workspaceRoot) {
  const runtimeDir = path.join(workspaceRoot, '.magi', 'runtime');
  const lockPath = path.join(runtimeDir, 'dispatch-idempotency.json.lock');
  fs.mkdirSync(runtimeDir, { recursive: true });
  fs.writeFileSync(lockPath, JSON.stringify({ pid: process.pid, acquiredAt: Date.now() }), 'utf8');

  const store = new DispatchIdempotencyStore(workspaceRoot, {
    ttlMs: 60_000,
    maxRecords: 100,
    lockStaleMs: 60_000,
    lockAcquireTimeoutMs: 120,
    lockRetryMs: 10,
  });
  let timeoutError = null;
  try {
    store.claimOrGet({
      key: 'lock-timeout-key',
      sessionId: 'session-timeout',
      missionId: 'mission-timeout',
      taskId: 'task-timeout',
      worker: 'codex',
      category: 'general',
      taskName: 'lock-timeout',
      routingReason: 'lock-timeout',
      degraded: false,
      status: 'dispatched',
    });
  } catch (error) {
    timeoutError = error;
  } finally {
    fs.unlinkSync(lockPath);
  }

  assert(timeoutError, '锁未释放时应超时失败');
  assert(String(timeoutError.message || timeoutError).includes('锁获取超时'), `锁超时报错异常: ${timeoutError?.message || timeoutError}`);
}

function spawnConcurrentClaim({
  modulePath,
  workspaceRoot,
  key,
  taskId,
}) {
  const script = `
    const { DispatchIdempotencyStore } = require(process.env.MAGI_STORE_MODULE);
    const store = new DispatchIdempotencyStore(process.env.MAGI_WORKSPACE_ROOT, {
      ttlMs: 60000,
      maxRecords: 1000,
      lockAcquireTimeoutMs: 5000,
      lockStaleMs: 2000,
      lockRetryMs: 5,
    });
    const result = store.claimOrGet({
      key: process.env.MAGI_CLAIM_KEY,
      sessionId: 'session-concurrent',
      missionId: 'mission-concurrent',
      taskId: process.env.MAGI_TASK_ID,
      worker: 'codex',
      category: 'general',
      taskName: 'concurrent-claim',
      routingReason: 'concurrent-claim',
      degraded: false,
      status: 'dispatched',
    });
    process.stdout.write(JSON.stringify({ claimed: result.claimed, taskId: result.record.taskId }));
  `;
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, ['-e', script], {
      env: {
        ...process.env,
        MAGI_STORE_MODULE: modulePath,
        MAGI_WORKSPACE_ROOT: workspaceRoot,
        MAGI_CLAIM_KEY: key,
        MAGI_TASK_ID: taskId,
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += String(chunk); });
    child.stderr.on('data', (chunk) => { stderr += String(chunk); });
    child.on('error', reject);
    child.on('close', (code) => {
      if (code !== 0) {
        return reject(new Error(`子进程失败(code=${code}): ${stderr || stdout}`));
      }
      try {
        resolve(JSON.parse(stdout.trim()));
      } catch (error) {
        reject(new Error(`子进程输出解析失败: ${stdout}; ${error?.message || error}`));
      }
    });
  });
}

async function testCrossProcessConcurrency(workspaceRoot) {
  const modulePath = path.join(OUT, 'orchestrator', 'core', 'dispatch-idempotency-store.js');
  const key = 'session-concurrent::mission-concurrent::provided:same-key';
  const tasks = Array.from({ length: 16 }, (_, i) => `task-concurrent-${i}`);
  const results = await Promise.all(tasks.map(taskId => spawnConcurrentClaim({
    modulePath,
    workspaceRoot,
    key,
    taskId,
  })));
  const claimed = results.filter(item => item.claimed === true);
  assert(claimed.length === 1, `并发 claim 成功数应为 1，实际: ${claimed.length}`);
  const canonicalTaskId = claimed[0].taskId;
  for (const item of results) {
    assert(item.taskId === canonicalTaskId, `并发 claim 结果不一致: ${item.taskId} vs ${canonicalTaskId}`);
  }
}

async function main() {
  testSourceGuardrails();
  const { DispatchIdempotencyStore } = loadCompiledModule(path.join('orchestrator', 'core', 'dispatch-idempotency-store.js'));
  const workspaceRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'magi-dispatch-idempotency-'));

  try {
    testStorePersistence(DispatchIdempotencyStore, workspaceRoot);
    testStorePrune(DispatchIdempotencyStore, workspaceRoot);
    testAtomicClaim(DispatchIdempotencyStore, workspaceRoot);
    testStaleLockRecovery(DispatchIdempotencyStore, workspaceRoot);
    testLockTimeout(DispatchIdempotencyStore, workspaceRoot);
    await testCrossProcessConcurrency(workspaceRoot);
  } finally {
    fs.rmSync(workspaceRoot, { recursive: true, force: true });
  }

  console.log('\n=== dispatch idempotency regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'source-guardrails',
      'store-persistence',
      'store-status-update',
      'store-prune-expired',
      'store-atomic-claim',
      'store-stale-lock-recovery',
      'store-lock-timeout',
      'store-cross-process-concurrency',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('dispatch idempotency 回归失败:', error?.stack || error);
  process.exit(1);
});
