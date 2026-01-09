/**
 * CLI Arranger 后端业务逻辑测试脚本
 * 运行: node -r ./test/setup-mock.js -r ts-node/register test/run-tests.ts
 * 或者: npx ts-node test/run-tests.ts
 *
 * 注意：此测试脚本只测试不依赖 vscode 模块的核心逻辑
 */

// Mock vscode 模块 - 在 require 之前注入
const Module = require('module');
const originalLoad = Module._load;
Module._load = function(request: string, parent: any, isMain: boolean) {
  if (request === 'vscode') {
    return {
      languages: { getDiagnostics: () => [] },
      DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
      Uri: { file: (p: string) => ({ fsPath: p }) },
      workspace: { workspaceFolders: [] },
    };
  }
  return originalLoad(request, parent, isMain);
};

// 简单测试框架
interface TestResult {
  name: string;
  passed: boolean;
  error?: string;
  duration: number;
}

const results: TestResult[] = [];

async function test(name: string, fn: () => Promise<void> | void): Promise<void> {
  const start = Date.now();
  try {
    await fn();
    results.push({ name, passed: true, duration: Date.now() - start });
    console.log(`  ✅ ${name}`);
  } catch (error: any) {
    results.push({ name, passed: false, error: error.message, duration: Date.now() - start });
    console.log(`  ❌ ${name}: ${error.message}`);
  }
}

function assert(condition: boolean, message: string): void {
  if (!condition) throw new Error(message);
}

function assertEqual<T>(actual: T, expected: T, message?: string): void {
  if (actual !== expected) {
    throw new Error(message || `Expected ${expected}, got ${actual}`);
  }
}

function assertContains(str: string, substr: string, message?: string): void {
  if (!str.includes(substr)) {
    throw new Error(message || `Expected "${str}" to contain "${substr}"`);
  }
}

// ============ 测试模块导入 ============
import { TaskStateManager, TaskState, TaskStatus } from '../src/orchestrator/task-state-manager';
import { VerificationRunner, VerificationResult } from '../src/orchestrator/verification-runner';
import { formatPlanForUser } from '../src/orchestrator/prompts';

// ============ TaskStateManager 测试 ============
async function testTaskStateManager(): Promise<void> {
  console.log('\n📋 TaskStateManager 测试');

  const manager = new TaskStateManager('test-session', process.cwd(), false);

  await test('创建任务', () => {
    const task = manager.createTask({
      id: 'task-1',
      parentTaskId: 'parent-1',
      description: '测试任务',
      assignedCli: 'claude',
    });
    assert(task.id === 'task-1', '任务 ID 应该正确');
    assertEqual(task.status, 'pending', '初始状态应为 pending');
    assertEqual(task.assignedCli, 'claude', 'CLI 应为 claude');
  });

  await test('更新任务状态', () => {
    manager.updateStatus('task-1', 'running');
    const updated = manager.getTask('task-1');
    assertEqual(updated?.status, 'running', '状态应更新为 running');
  });

  await test('完成任务', () => {
    manager.updateStatus('task-1', 'completed');
    const updated = manager.getTask('task-1');
    assertEqual(updated?.status, 'completed', '状态应为 completed');
  });

  await test('获取所有任务', () => {
    const tasks = manager.getAllTasks();
    assert(tasks.length > 0, '应有任务');
  });

  await test('获取待处理任务', () => {
    manager.createTask({
      id: 'task-2',
      parentTaskId: 'parent-1',
      description: '待处理任务',
      assignedCli: 'codex'
    });
    const pending = manager.getPendingTasks();
    assert(pending.length > 0, '应有待处理任务');
    assertEqual(pending[0].status, 'pending', '状态应为 pending');
  });

  await test('任务失败处理', () => {
    manager.updateStatus('task-2', 'failed', '测试错误');
    const task = manager.getTask('task-2');
    assertEqual(task?.status, 'failed', '状态应为 failed');
    assertEqual(task?.error, '测试错误', '应有错误信息');
  });
}

// ============ VerificationRunner 测试 ============
async function testVerificationRunner(): Promise<void> {
  console.log('\n🔍 VerificationRunner 测试');

  const runner = new VerificationRunner(process.cwd());

  await test('创建验证器实例', () => {
    assert(runner !== null, '应成功创建实例');
  });

  await test('运行验证检查', async () => {
    const result = await runner.runVerification('test-task');
    assert(typeof result.success === 'boolean', '应返回 success 布尔值');
    assert(typeof result.summary === 'string', '应返回 summary 字符串');
  });
}

// ============ Prompts 格式化测试 ============
async function testPrompts(): Promise<void> {
  console.log('\n📝 Prompts 格式化测试');

  await test('格式化执行计划', () => {
    const plan = {
      analysis: '任务分析结果',
      needsCollaboration: true,
      subTasks: [
        { id: '1', description: '实现功能A', assignedCli: 'claude', reason: '复杂逻辑' },
        { id: '2', description: '实现功能B', assignedCli: 'codex', reason: '简单修改' },
      ],
      executionMode: 'parallel',
      summary: '执行计划总结',
    };
    const formatted = formatPlanForUser(plan);
    assertContains(formatted, '执行计划', '应包含标题');
    assertContains(formatted, '功能A', '应包含任务描述');
  });
}

// ============ 主测试入口 ============
async function runAllTests(): Promise<void> {
  console.log('🚀 CLI Arranger 后端业务逻辑测试\n');
  console.log('=' .repeat(50));

  try {
    await testTaskStateManager();
    await testVerificationRunner();
    await testPrompts();
  } catch (error: any) {
    console.error('\n💥 测试运行异常:', error.message);
  }

  // 输出测试结果汇总
  console.log('\n' + '='.repeat(50));
  console.log('📊 测试结果汇总\n');

  const passed = results.filter(r => r.passed).length;
  const failed = results.filter(r => !r.passed).length;
  const totalDuration = results.reduce((sum, r) => sum + r.duration, 0);

  console.log(`  总计: ${results.length} 个测试`);
  console.log(`  通过: ${passed} ✅`);
  console.log(`  失败: ${failed} ❌`);
  console.log(`  耗时: ${totalDuration}ms`);

  if (failed > 0) {
    console.log('\n❌ 失败的测试:');
    results.filter(r => !r.passed).forEach(r => {
      console.log(`  - ${r.name}: ${r.error}`);
    });
    process.exit(1);
  } else {
    console.log('\n✅ 所有测试通过!');
    process.exit(0);
  }
}

runAllTests();

