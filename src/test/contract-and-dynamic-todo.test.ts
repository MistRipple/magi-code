/**
 * 契约冲突和动态 Todo 集成测试
 *
 * 验证：
 * 1. 契约冲突检测
 * 2. Todo 管理流程
 * 3. Todo 依赖检查
 */

import { ContractManager } from '../orchestrator/mission/contract-manager';
import { AssignmentManager } from '../orchestrator/mission/assignment-manager';
import { ProfileLoader } from '../orchestrator/profile/profile-loader';
import { GuidanceInjector } from '../orchestrator/profile/guidance-injector';
import type {
  Mission,
  Contract,
  Assignment,
  WorkerTodo,
  ContractViolation,
} from '../orchestrator/mission/types';
import type { WorkerSlot } from '../types';

/**
 * 测试结果
 */
interface TestResult {
  name: string;
  passed: boolean;
  error?: string;
  duration: number;
}

/**
 * 创建测试 Mission
 */
function createTestMission(id: string): Mission {
  return {
    id,
    sessionId: 'test-session',
    userPrompt: 'Test mission',
    goal: 'Test goal',
    analysis: 'Test analysis',
    context: '',
    constraints: [],
    acceptanceCriteria: [],
    contracts: [],
    assignments: [],
    riskLevel: 'low',
    riskFactors: [],
    executionPath: 'light',
    status: 'draft',
    phase: 'goal_understanding',
    createdAt: Date.now(),
    updatedAt: Date.now(),
  };
}

/**
 * 创建测试契约
 */
function createTestContract(id: string, missionId: string, producer: WorkerSlot, consumers: WorkerSlot[]): Contract {
  return {
    id,
    missionId,
    type: 'api',
    name: `Test Contract ${id}`,
    description: 'A test contract',
    specification: {
      api: {
        endpoint: '/api/test',
        method: 'POST',
        requestSchema: '{ userId: string }',
        responseSchema: '{ success: boolean }',
      },
    },
    producer,
    consumers,
    status: 'draft',
  };
}

/**
 * 创建测试 Assignment
 */
function createTestAssignment(id: string, missionId: string, workerId: WorkerSlot): Assignment {
  return {
    id,
    missionId,
    workerId,
    assignmentReason: {
      profileMatch: { category: 'test', score: 0.8, matchedKeywords: ['test'] },
      contractRole: 'none',
      explanation: 'Test assignment',
      alternatives: [],
    },
    responsibility: 'Test responsibility',
    shortTitle: '测试任务',
    scope: { includes: ['test'], excludes: [] },
    guidancePrompt: '',
    producerContracts: [],
    consumerContracts: [],
    todos: [],
    planningStatus: 'pending',
    status: 'pending',
    progress: 0,
    createdAt: Date.now(),
  };
}

/**
 * 创建测试 Todo
 */
function createTestTodo(id: string, assignmentId: string, outOfScope = false): WorkerTodo {
  return {
    id,
    missionId: 'test-mission',
    assignmentId,
    content: `Test todo ${id}`,
    reasoning: 'Test reasoning',
    expectedOutput: 'Test output',
    type: 'implementation',
    workerId: 'claude',
    priority: 3,
    outOfScope,
    approvalStatus: outOfScope ? 'pending' : undefined,
    dependsOn: [],
    requiredContracts: [],
    producesContracts: [],
    status: 'pending',
    progress: 0,
    retryCount: 0,
    maxRetries: 3,
    createdAt: Date.now(),
  };
}

/**
 * 运行单个测试
 */
async function runTest<T>(name: string, fn: () => Promise<T>): Promise<TestResult> {
  const start = Date.now();
  try {
    await fn();
    return { name, passed: true, duration: Date.now() - start };
  } catch (error) {
    return {
      name,
      passed: false,
      error: error instanceof Error ? error.message : String(error),
      duration: Date.now() - start,
    };
  }
}

/**
 * 运行所有测试
 */
async function runTests(): Promise<void> {
  console.log('='.repeat(60));
  console.log('契约冲突和动态 Todo 集成测试');
  console.log('='.repeat(60));

  const results: TestResult[] = [];
  const profileLoader = ProfileLoader.getInstance();
  const guidanceInjector = new GuidanceInjector();

  // =========== 契约测试 ===========

  // 测试 1: 契约创建
  results.push(await runTest('契约创建', async () => {
    const contractManager = new ContractManager();
    const mission = createTestMission('mission-1');

    // 创建两个契约
    const contract1 = contractManager.createContract({
      missionId: mission.id,
      type: 'api',
      name: 'User API Contract',
      description: 'User management API',
      producer: 'claude',
      consumers: ['codex'],
    });

    const contract2 = contractManager.createContract({
      missionId: mission.id,
      type: 'data',
      name: 'User Data Contract',
      description: 'User data schema',
      producer: 'codex',
      consumers: ['claude'],
    });

    if (!contract1.id || !contract2.id) {
      throw new Error('契约 ID 未生成');
    }

    if (contract1.status !== 'draft') {
      throw new Error(`契约状态应为 draft，实际为 ${contract1.status}`);
    }

    console.log(`  - 创建了 2 个契约`);
    return [contract1, contract2];
  }));

  // 测试 2: 契约一致性验证 - 缺少生产者分配
  results.push(await runTest('契约一致性验证 - 缺少生产者分配', async () => {
    const contractManager = new ContractManager();
    const mission = createTestMission('mission-2');

    // 创建只有消费者的契约
    const contract: Contract = {
      id: 'contract-orphan',
      missionId: mission.id,
      type: 'api',
      name: 'Orphan Contract',
      description: 'Contract without producer assignment',
      specification: {},
      producer: 'claude',
      consumers: ['codex'],
      status: 'agreed',
    };

    // 只创建消费者的 Assignment，不创建生产者
    const consumerAssignment = createTestAssignment('assign-1', mission.id, 'codex');
    consumerAssignment.consumerContracts = [contract.id];

    mission.contracts = [contract];
    mission.assignments = [consumerAssignment];

    const result = await contractManager.verifyContractConsistency(mission);

    if (result.consistent) {
      throw new Error('应检测到不一致');
    }

    const hasMissingProducer = result.violations.some(
      (v: ContractViolation) => v.type === 'missing_producer'
    );
    if (!hasMissingProducer) {
      throw new Error('应检测到 missing_producer 类型的违规');
    }

    console.log(`  - 检测到 ${result.violations.length} 个违规`);
    return result;
  }));

  // 测试 3: 契约状态转换
  results.push(await runTest('契约状态转换', async () => {
    const contractManager = new ContractManager();

    let contract = createTestContract('contract-3', 'mission-3', 'claude', ['codex']);

    // draft -> proposed
    contract = contractManager.updateContractStatus(contract, 'proposed');
    if (contract.status !== 'proposed') {
      throw new Error(`状态应为 proposed，实际为 ${contract.status}`);
    }

    // proposed -> agreed
    contract = contractManager.updateContractStatus(contract, 'agreed');
    if (contract.status !== 'agreed') {
      throw new Error(`状态应为 agreed，实际为 ${contract.status}`);
    }

    // agreed -> implemented
    contract = contractManager.updateContractStatus(contract, 'implemented');
    if (contract.status !== 'implemented') {
      throw new Error(`状态应为 implemented，实际为 ${contract.status}`);
    }

    console.log('  - 状态转换: draft -> proposed -> agreed -> implemented');
    return contract;
  }));

  // =========== Assignment 和 Todo 测试 ===========

  // 测试 4: 添加 Todo 到 Assignment
  results.push(await runTest('添加 Todo 到 Assignment', async () => {
    const assignmentManager = new AssignmentManager(profileLoader, guidanceInjector);
    let assignment = createTestAssignment('assign-4', 'mission-4', 'claude');

    // 添加初始 Todo
    const todo1 = createTestTodo('todo-1', assignment.id);
    assignment = assignmentManager.addTodo(assignment, todo1);

    // 添加第二个 Todo
    const todo2 = createTestTodo('todo-2', assignment.id);
    assignment = assignmentManager.addTodo(assignment, todo2);

    if (assignment.todos.length !== 2) {
      throw new Error(`Todo 数量应为 2，实际为 ${assignment.todos.length}`);
    }

    console.log('  - 添加 Todo 成功');
    return assignment;
  }));

  // 测试 5: 更新 Todo 状态和进度计算
  results.push(await runTest('更新 Todo 状态和进度计算', async () => {
    const assignmentManager = new AssignmentManager(profileLoader, guidanceInjector);
    let assignment = createTestAssignment('assign-5', 'mission-5', 'claude');

    // 添加 2 个 Todo
    const todo1 = createTestTodo('todo-1', assignment.id);
    const todo2 = createTestTodo('todo-2', assignment.id);
    assignment = assignmentManager.addTodo(assignment, todo1);
    assignment = assignmentManager.addTodo(assignment, todo2);

    // 完成第一个 Todo
    const completedTodo1: WorkerTodo = { ...assignment.todos[0], status: 'completed' };
    assignment = assignmentManager.updateTodo(assignment, completedTodo1);

    if (assignment.progress !== 50) {
      throw new Error(`进度应为 50，实际为 ${assignment.progress}`);
    }

    // 完成第二个 Todo
    const completedTodo2: WorkerTodo = { ...assignment.todos[1], status: 'completed' };
    assignment = assignmentManager.updateTodo(assignment, completedTodo2);

    if (assignment.progress !== 100) {
      throw new Error(`进度应为 100，实际为 ${assignment.progress}`);
    }

    console.log('  - 进度计算正确: 0% -> 50% -> 100%');
    return assignment;
  }));

  // 测试 6: Assignment 状态转换
  results.push(await runTest('Assignment 状态转换', async () => {
    const assignmentManager = new AssignmentManager(profileLoader, guidanceInjector);
    let assignment = createTestAssignment('assign-6', 'mission-6', 'claude');

    // pending -> planning
    assignment = assignmentManager.updateAssignmentStatus(assignment, 'planning');
    if (assignment.status !== 'planning') {
      throw new Error(`状态应为 planning，实际为 ${assignment.status}`);
    }

    // planning -> ready
    assignment = assignmentManager.updateAssignmentStatus(assignment, 'ready');
    if (assignment.status !== 'ready') {
      throw new Error(`状态应为 ready，实际为 ${assignment.status}`);
    }

    // ready -> executing
    assignment = assignmentManager.updateAssignmentStatus(assignment, 'executing');
    if (assignment.status !== 'executing') {
      throw new Error(`状态应为 executing，实际为 ${assignment.status}`);
    }

    if (!assignment.startedAt) {
      throw new Error('执行状态应设置 startedAt');
    }

    // executing -> completed
    assignment = assignmentManager.updateAssignmentStatus(assignment, 'completed');
    if (assignment.status !== 'completed') {
      throw new Error(`状态应为 completed，实际为 ${assignment.status}`);
    }

    if (!assignment.completedAt) {
      throw new Error('完成状态应设置 completedAt');
    }

    console.log('  - 状态转换: pending -> planning -> ready -> executing -> completed');
    return assignment;
  }));

  // 测试 7: Todo 依赖检查
  results.push(await runTest('Todo 依赖检查', async () => {
    const assignmentManager = new AssignmentManager(profileLoader, guidanceInjector);
    let assignment = createTestAssignment('assign-7', 'mission-7', 'claude');

    // 创建依赖关系：todo-2 依赖 todo-1
    const todo1 = createTestTodo('todo-1', assignment.id);
    const todo2 = createTestTodo('todo-2', assignment.id);
    todo2.dependsOn = ['todo-1'];

    assignment = assignmentManager.addTodo(assignment, todo1);
    assignment = assignmentManager.addTodo(assignment, todo2);

    // todo-1 应该先执行
    let next = assignmentManager.getNextExecutableTodo(assignment);
    if (next?.id !== 'todo-1') {
      throw new Error('第一个可执行 Todo 应该是 todo-1');
    }

    // 完成 todo-1
    const completedTodo1: WorkerTodo = { ...assignment.todos[0], status: 'completed' };
    assignment = assignmentManager.updateTodo(assignment, completedTodo1);

    // 现在 todo-2 应该可执行
    next = assignmentManager.getNextExecutableTodo(assignment);
    if (next?.id !== 'todo-2') {
      throw new Error('todo-1 完成后，todo-2 应该可执行');
    }

    console.log('  - Todo 依赖检查通过');
    return assignment;
  }));

  // 测试 8: 超范围 Todo 阻塞
  results.push(await runTest('超范围 Todo 阻塞执行', async () => {
    const assignmentManager = new AssignmentManager(profileLoader, guidanceInjector);
    let assignment = createTestAssignment('assign-8', 'mission-8', 'claude');

    // 添加超范围 Todo（未审批）
    const outOfScopeTodo = createTestTodo('todo-oos', assignment.id, true);
    assignment = assignmentManager.addTodo(assignment, outOfScopeTodo);

    // 超范围 Todo 未审批时不应可执行
    const next = assignmentManager.getNextExecutableTodo(assignment);
    if (next !== null) {
      throw new Error('超范围 Todo 未审批时不应可执行');
    }

    // 手动审批
    const approvedTodo: WorkerTodo = {
      ...assignment.todos[0],
      approvalStatus: 'approved',
    };
    assignment = assignmentManager.updateTodo(assignment, approvedTodo);

    // 审批后应可执行
    const nextAfterApproval = assignmentManager.getNextExecutableTodo(assignment);
    if (nextAfterApproval?.id !== 'todo-oos') {
      throw new Error('审批后 Todo 应可执行');
    }

    console.log('  - 超范围 Todo 阻塞/审批逻辑正确');
    return assignment;
  }));

  // 测试 9: 检查 Assignment 完成
  results.push(await runTest('检查 Assignment 完成', async () => {
    const assignmentManager = new AssignmentManager(profileLoader, guidanceInjector);
    let assignment = createTestAssignment('assign-9', 'mission-9', 'claude');

    // 添加 2 个 Todo
    const todo1 = createTestTodo('todo-1', assignment.id);
    const todo2 = createTestTodo('todo-2', assignment.id);
    assignment = assignmentManager.addTodo(assignment, todo1);
    assignment = assignmentManager.addTodo(assignment, todo2);

    // 未完成时
    if (assignmentManager.isAssignmentComplete(assignment)) {
      throw new Error('未完成 Todo 时，Assignment 不应标记为完成');
    }

    // 完成一个
    const completedTodo1: WorkerTodo = { ...assignment.todos[0], status: 'completed' };
    assignment = assignmentManager.updateTodo(assignment, completedTodo1);

    if (assignmentManager.isAssignmentComplete(assignment)) {
      throw new Error('只完成部分 Todo 时，Assignment 不应标记为完成');
    }

    // 跳过另一个
    const skippedTodo2: WorkerTodo = { ...assignment.todos[1], status: 'skipped' };
    assignment = assignmentManager.updateTodo(assignment, skippedTodo2);

    if (!assignmentManager.isAssignmentComplete(assignment)) {
      throw new Error('所有 Todo 完成或跳过时，Assignment 应标记为完成');
    }

    console.log('  - Assignment 完成检查正确');
    return assignment;
  }));

  // =========== 打印结果 ===========
  console.log('\n' + '='.repeat(60));
  console.log('测试结果汇总');
  console.log('='.repeat(60));

  let passed = 0;
  let failed = 0;

  for (const result of results) {
    const status = result.passed ? '✓' : '✗';
    console.log(`${status} ${result.name} (${result.duration}ms)`);
    if (!result.passed && result.error) {
      console.log(`  错误: ${result.error}`);
    }
    result.passed ? passed++ : failed++;
  }

  console.log('\n' + '-'.repeat(60));
  console.log(`通过: ${passed}/${results.length}`);
  console.log(`失败: ${failed}/${results.length}`);

  if (failed > 0) {
    process.exit(1);
  }
}

runTests().catch((err) => {
  console.error(err);
  process.exit(1);
});
