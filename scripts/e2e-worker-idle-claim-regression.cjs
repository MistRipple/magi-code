#!/usr/bin/env node
/**
 * Worker 自治驻留回归（运行时优先）
 *
 * 覆盖目标：
 * 1) AutonomousWorker 空闲时可通过 findClaimable + tryClaim 认领同 assignment 新 Todo。
 * 2) 认领范围必须限制在当前 assignment，禁止跨 assignment 抢占。
 * 3) idle claim 开关可关闭。
 * 4) DispatchManager lane 驻留轮询入口与配置项仍存在。
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const OUT = path.join(ROOT, 'out');
const DISPATCH_FILE = path.join(ROOT, 'src', 'orchestrator', 'core', 'dispatch-manager.ts');

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

function createWorker(todoManager) {
  const { AutonomousWorker } = loadCompiledModule(path.join('orchestrator', 'worker', 'autonomous-worker.js'));
  const profileLoader = {
    getProfile() {
      return {
        strengths: [],
        weaknesses: [],
        preferredTaskTypes: [],
        avoidTaskTypes: [],
        collaborationStyle: 'balanced',
      };
    },
  };
  const guidanceInjector = {};
  const sharedContextDeps = {
    contextAssembler: {},
    fileSummaryCache: {},
    sharedContextPool: {},
  };
  return new AutonomousWorker(
    'codex',
    profileLoader,
    guidanceInjector,
    todoManager,
    sharedContextDeps,
  );
}

async function testIdleClaimSuccess() {
  let findClaimableCalls = 0;
  let tryClaimCalls = 0;
  const claimedTodo = {
    id: 'todo-claim-1',
    assignmentId: 'assignment-1',
  };
  const todoManager = {
    async findClaimable() {
      findClaimableCalls += 1;
      return [claimedTodo];
    },
    async tryClaim(todoId) {
      tryClaimCalls += 1;
      return todoId === claimedTodo.id ? claimedTodo : null;
    },
  };
  const worker = createWorker(todoManager);
  try {
    process.env.MAGI_WORKER_IDLE_CLAIM_ENABLE = '1';
    process.env.MAGI_WORKER_IDLE_CLAIM_TIMEOUT_MS = '80';
    process.env.MAGI_WORKER_IDLE_CLAIM_POLL_INTERVAL_MS = '10';

    const result = await worker.pollAndClaimTodoWhileIdle(
      { id: 'assignment-1', missionId: 'mission-1', workerId: 'codex' },
      {},
    );
    assert(result && result.id === claimedTodo.id, 'idle claim 未成功认领当前 assignment todo');
    assert(findClaimableCalls >= 1, 'idle claim 未调用 findClaimable');
    assert(tryClaimCalls >= 1, 'idle claim 未调用 tryClaim');
  } finally {
    worker.dispose();
  }
}

async function testAssignmentScopeGuard() {
  let tryClaimCalls = 0;
  const todoManager = {
    async findClaimable() {
      return [
        { id: 'todo-foreign-1', assignmentId: 'assignment-foreign' },
      ];
    },
    async tryClaim() {
      tryClaimCalls += 1;
      return null;
    },
  };
  const worker = createWorker(todoManager);
  try {
    process.env.MAGI_WORKER_IDLE_CLAIM_ENABLE = '1';
    process.env.MAGI_WORKER_IDLE_CLAIM_TIMEOUT_MS = '40';
    process.env.MAGI_WORKER_IDLE_CLAIM_POLL_INTERVAL_MS = '10';

    const result = await worker.pollAndClaimTodoWhileIdle(
      { id: 'assignment-1', missionId: 'mission-1', workerId: 'codex' },
      {},
    );
    assert(result === null, '跨 assignment todo 不应被认领');
    assert(tryClaimCalls === 0, '跨 assignment 候选不应触发 tryClaim');
  } finally {
    worker.dispose();
  }
}

async function testIdleClaimSwitchOff() {
  let findClaimableCalls = 0;
  const todoManager = {
    async findClaimable() {
      findClaimableCalls += 1;
      return [];
    },
    async tryClaim() {
      return null;
    },
  };
  const worker = createWorker(todoManager);
  try {
    process.env.MAGI_WORKER_IDLE_CLAIM_ENABLE = '0';
    process.env.MAGI_WORKER_IDLE_CLAIM_TIMEOUT_MS = '100';
    process.env.MAGI_WORKER_IDLE_CLAIM_POLL_INTERVAL_MS = '10';

    const result = await worker.pollAndClaimTodoWhileIdle(
      { id: 'assignment-1', missionId: 'mission-1', workerId: 'codex' },
      {},
    );
    assert(result === null, '关闭 idle claim 时应直接返回 null');
    assert(findClaimableCalls === 0, '关闭 idle claim 时不应进入轮询');
  } finally {
    worker.dispose();
  }
}

function testDispatchResidentSourceGuard() {
  const dispatchSource = fs.readFileSync(DISPATCH_FILE, 'utf8');
  assert(
    dispatchSource.includes('waitForReadyTaskWhileResident(batch, worker'),
    'DispatchManager 缺少 worker lane 驻留轮询入口',
  );
  assert(
    dispatchSource.includes('MAGI_WORKER_LANE_RESIDENT_TIMEOUT_MS'),
    'DispatchManager 缺少驻留 timeout 配置',
  );
  assert(
    dispatchSource.includes('MAGI_WORKER_LANE_RESIDENT_POLL_INTERVAL_MS'),
    'DispatchManager 缺少驻留 poll interval 配置',
  );
}

async function main() {
  const originalEnv = {
    enable: process.env.MAGI_WORKER_IDLE_CLAIM_ENABLE,
    timeout: process.env.MAGI_WORKER_IDLE_CLAIM_TIMEOUT_MS,
    interval: process.env.MAGI_WORKER_IDLE_CLAIM_POLL_INTERVAL_MS,
  };

  try {
    await testIdleClaimSuccess();
    await testAssignmentScopeGuard();
    await testIdleClaimSwitchOff();
    testDispatchResidentSourceGuard();

    console.log('\n=== worker idle claim regression ===');
    console.log(JSON.stringify({
      pass: true,
      checks: [
        'idle_loop_runtime_claim_success',
        'assignment_scoped_claim_runtime_guard',
        'idle_claim_switch_runtime_guard',
        'lane_resident_poll_hooked',
        'lane_resident_timeout_configurable',
        'lane_resident_poll_interval_configurable',
      ],
    }, null, 2));
  } finally {
    if (typeof originalEnv.enable === 'undefined') {
      delete process.env.MAGI_WORKER_IDLE_CLAIM_ENABLE;
    } else {
      process.env.MAGI_WORKER_IDLE_CLAIM_ENABLE = originalEnv.enable;
    }
    if (typeof originalEnv.timeout === 'undefined') {
      delete process.env.MAGI_WORKER_IDLE_CLAIM_TIMEOUT_MS;
    } else {
      process.env.MAGI_WORKER_IDLE_CLAIM_TIMEOUT_MS = originalEnv.timeout;
    }
    if (typeof originalEnv.interval === 'undefined') {
      delete process.env.MAGI_WORKER_IDLE_CLAIM_POLL_INTERVAL_MS;
    } else {
      process.env.MAGI_WORKER_IDLE_CLAIM_POLL_INTERVAL_MS = originalEnv.interval;
    }
  }
}

main().catch((error) => {
  console.error('worker idle claim 回归失败:', error?.stack || error);
  process.exit(1);
});
