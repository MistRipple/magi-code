#!/usr/bin/env node
/**
 * Dispatch 执行协议回归脚本
 *
 * 覆盖目标：
 * 1) ACK 超时触发 fail-fast（ack-timeout）
 * 2) lease 过期触发 fail-fast（lease-expired）
 * 3) NACK 触发 fail-fast（nack:*）
 * 4) heartbeat 可以补 ACK 并续租
 */

const fs = require('fs');
const path = require('path');
const { EventEmitter } = require('events');

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

function createFakeBatch(batchId) {
  const entries = new Map();
  const failed = [];
  let touched = 0;

  return {
    id: batchId,
    register(taskId, worker) {
      entries.set(taskId, {
        taskId,
        status: 'running',
        worker,
        taskContract: {
          taskTitle: `task-${taskId}`,
          category: 'general',
          requirementAnalysis: {
            goal: `goal-${taskId}`,
            analysis: `analysis-${taskId}`,
            needsWorker: true,
            reason: 'dispatch protocol regression fixture',
          },
          context: [],
          scopeHint: [],
          files: [],
          dependsOn: [],
          collaborationContracts: {
            producerContracts: [],
            consumerContracts: [],
            interfaceContracts: [],
            freezeFiles: [],
          },
        },
      });
    },
    getEntry(taskId) {
      return entries.get(taskId);
    },
    markFailed(taskId, result) {
      const entry = entries.get(taskId);
      if (entry) {
        entry.status = 'failed';
      }
      failed.push({ taskId, result });
    },
    touchActivity() {
      touched += 1;
    },
    getFailed() {
      return failed;
    },
    getTouchedCount() {
      return touched;
    },
  };
}

function createManager(DispatchManager) {
  const interrupts = [];
  const subTaskCards = [];
  const workerErrors = [];
  const missionOrchestrator = new EventEmitter();

  const manager = new DispatchManager({
    adapterFactory: {
      interrupt(worker) {
        interrupts.push(worker);
        return Promise.resolve();
      },
    },
    profileLoader: {
      getEnabledProfiles() {
        return new Map();
      },
      getAllCategories() {
        return new Map();
      },
      getAssignmentLoader() {
        return {
          reload() {},
        };
      },
      getWorkerForCategory() {
        return 'codex';
      },
      getCategory() {
        return { name: 'general' };
      },
    },
    messageHub: {
      subTaskCard(payload) {
        subTaskCards.push(payload);
      },
      workerError(worker, message) {
        workerErrors.push({ worker, message });
      },
      notify() {},
    },
    missionOrchestrator,
    workspaceRoot: ROOT,
    getActiveUserPrompt: () => '',
    getActiveImagePaths: () => undefined,
    getCurrentSessionId: () => 'session-dispatch-protocol',
    getMissionIdsBySession: async () => [],
    ensureMissionForDispatch: async () => 'mission-dispatch-protocol',
    getCurrentTurnId: () => 'turn-dispatch-protocol',
    getProjectKnowledgeBase: () => undefined,
    processWorkerWisdom() {},
    recordOrchestratorTokens() {},
    recordWorkerTokenUsage() {},
    getSnapshotManager: () => null,
    getContextManager: () => null,
    getTodoManager: () => null,
    getSupplementaryQueue: () => null,
  });

  return {
    manager,
    interrupts,
    subTaskCards,
    workerErrors,
  };
}

async function testAckTimeout(DispatchManager) {
  const ctx = createManager(DispatchManager);
  const batch = createFakeBatch('batch-ack-timeout');
  batch.register('task-ack-timeout', 'codex');

  ctx.manager.activeBatch = batch;
  const state = ctx.manager.registerProtocolState('task-ack-timeout', batch.id, 'codex');
  state.createdAt = Date.now() - 25_000;
  state.leaseExpireAt = Date.now() + 120_000;

  ctx.manager.checkProtocolLeases();
  await new Promise((resolve) => setTimeout(resolve, 10));

  const failed = batch.getFailed();
  assert(failed.length === 1, `ACK 超时应失败 1 次，实际: ${failed.length}`);
  assert(String(failed[0].result?.summary || '').includes('ack-timeout'), 'ACK 超时原因不正确');
  assert(ctx.interrupts.includes('codex'), 'ACK 超时后应中断 worker');
  assert(!ctx.manager.executionProtocolStates.has('task-ack-timeout'), 'ACK 超时后协议状态应清理');
  ctx.manager.dispose();
}

async function testLeaseExpired(DispatchManager) {
  const ctx = createManager(DispatchManager);
  const batch = createFakeBatch('batch-lease-expired');
  batch.register('task-lease-expired', 'claude');

  ctx.manager.activeBatch = batch;
  const state = ctx.manager.registerProtocolState('task-lease-expired', batch.id, 'claude');
  ctx.manager.markProtocolAck('task-lease-expired', 'claude');
  state.leaseExpireAt = Date.now() - 1;

  ctx.manager.checkProtocolLeases();
  await new Promise((resolve) => setTimeout(resolve, 10));

  const failed = batch.getFailed();
  assert(failed.length === 1, `lease 过期应失败 1 次，实际: ${failed.length}`);
  assert(String(failed[0].result?.summary || '').includes('lease-expired'), 'lease 过期原因不正确');
  assert(ctx.interrupts.includes('claude'), 'lease 过期后应中断 worker');
  ctx.manager.dispose();
}

async function testNack(DispatchManager) {
  const ctx = createManager(DispatchManager);
  const batch = createFakeBatch('batch-nack');
  batch.register('task-nack', 'gemini');

  ctx.manager.activeBatch = batch;
  ctx.manager.registerProtocolState('task-nack', batch.id, 'gemini');
  ctx.manager.markProtocolNack('task-nack', 'simulated-reject');

  ctx.manager.checkProtocolLeases();
  await new Promise((resolve) => setTimeout(resolve, 10));

  const failed = batch.getFailed();
  assert(failed.length === 1, `NACK 应失败 1 次，实际: ${failed.length}`);
  assert(String(failed[0].result?.summary || '').includes('nack:simulated-reject'), 'NACK 原因不正确');
  assert(ctx.interrupts.includes('gemini'), 'NACK 后应中断 worker');
  ctx.manager.dispose();
}

function testHeartbeatRenewsLease(DispatchManager) {
  const ctx = createManager(DispatchManager);
  const batch = createFakeBatch('batch-heartbeat');
  batch.register('task-heartbeat', 'codex');

  ctx.manager.activeBatch = batch;
  const state = ctx.manager.registerProtocolState('task-heartbeat', batch.id, 'codex');
  const heartbeatAt = Date.now();
  ctx.manager.updateProtocolHeartbeat('task-heartbeat', 'codex', heartbeatAt);

  assert(state.ackState === 'acked', `heartbeat 应补 ACK，实际: ${state.ackState}`);
  assert(state.leaseExpireAt > heartbeatAt, `heartbeat 应续租，leaseExpireAt=${state.leaseExpireAt}, heartbeatAt=${heartbeatAt}`);
  assert(batch.getTouchedCount() >= 1, 'heartbeat 应刷新 batch activity');
  ctx.manager.dispose();
}

async function main() {
  const { DispatchManager } = loadCompiledModule(path.join('orchestrator', 'core', 'dispatch-manager.js'));

  await testAckTimeout(DispatchManager);
  await testLeaseExpired(DispatchManager);
  await testNack(DispatchManager);
  testHeartbeatRenewsLease(DispatchManager);

  console.log('\n=== dispatch protocol regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'ack-timeout-fail-fast',
      'lease-expired-fail-fast',
      'nack-fail-fast',
      'heartbeat-renew-lease',
    ],
  }, null, 2));
}

main().catch((error) => {
  console.error('dispatch protocol 回归失败:', error?.stack || error);
  process.exit(1);
});
