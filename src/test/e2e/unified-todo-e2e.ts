/**
 * 统一 Todo 系统端对端测试
 *
 * 验证 TodoManager 和 UnifiedTodo 的核心功能（使用真实 LLM）：
 * 1. Todo 生命周期管理
 * 2. 契约依赖检查
 * 3. Todo 规划生成
 * 4. 与 Mission/Assignment 集成
 *
 * 运行: npx ts-node src/test/e2e/unified-todo-e2e.ts
 */

import * as path from 'path';
import * as fs from 'fs';
import * as os from 'os';
import { TodoManager } from '../../todo/todo-manager';
import { UnifiedTodo, TodoStatus, CreateTodoParams } from '../../todo/types';
import { UniversalLLMClient } from '../../llm/clients/universal-client';
import { LLMConfigLoader } from '../../llm/config';
import { LLMConfig } from '../../types/agent-types';
import { WorkerSlot } from '../../types';

// ============================================================================
// 类型定义
// ============================================================================

interface TestResult {
  name: string;
  passed: boolean;
  details: string[];
  duration: number;
}

interface TestContext {
  todoManager: TodoManager;
  workerClient: UniversalLLMClient;
  workspaceRoot: string;
}

// ============================================================================
// 测试辅助函数
// ============================================================================

function createTestDir(): string {
  const testDir = path.join(os.tmpdir(), `magi-todo-test-${Date.now()}`);
  fs.mkdirSync(testDir, { recursive: true });
  return testDir;
}

function cleanupTestDir(dir: string): void {
  try {
    fs.rmSync(dir, { recursive: true, force: true });
  } catch (e) {
    // Ignore cleanup errors
  }
}

async function createTestContext(): Promise<TestContext> {
  const workspaceRoot = createTestDir();
  const todoManager = new TodoManager(workspaceRoot);
  await todoManager.initialize();

  const config = LLMConfigLoader.loadFullConfig();

  // 使用第一个可用的 worker
  let workerConfig: LLMConfig | null = null;
  for (const [_, wConfig] of Object.entries(config.workers)) {
    if (wConfig.enabled) {
      workerConfig = { ...wConfig, enabled: true } as LLMConfig;
      break;
    }
  }

  if (!workerConfig) {
    throw new Error('No enabled worker found');
  }

  const workerClient = new UniversalLLMClient(workerConfig);

  return { todoManager, workerClient, workspaceRoot };
}

// ============================================================================
// 测试场景 1: Todo 生命周期管理
// ============================================================================

async function testTodoLifecycle(ctx: TestContext): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 1.1: 创建 Todo
  console.log('\n  📋 测试 1.1: 创建 Todo');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const params: CreateTodoParams = {
      missionId: 'mission-1',
      assignmentId: 'assignment-1',
      content: '实现用户登录 API',
      reasoning: '用户认证是系统的核心功能',
      type: 'implementation',
      workerId: 'claude' as WorkerSlot,
      priority: 1,
      dependsOn: [],
      requiredContracts: [],
      producesContracts: ['user-auth-api'],
    };

    const todo = await ctx.todoManager.create(params);

    details1.push(`Todo ID: ${todo.id}`);
    details1.push(`状态: ${todo.status}`);
    details1.push(`内容: ${todo.content}`);

    if (todo.id && (todo.status === 'pending' || todo.status === 'ready')) {
      passed1 = true;
      details1.push(`✓ Todo 创建成功 (状态: ${todo.status} - 无依赖时自动就绪)`);
    } else {
      details1.push('✗ Todo 创建失败');
    }
  } catch (e) {
    details1.push(`错误: ${e}`);
  }

  results.push({
    name: '创建 Todo',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 1.2: Todo 状态转换
  console.log('\n  📋 测试 1.2: Todo 状态转换');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    const params: CreateTodoParams = {
      missionId: 'mission-1',
      assignmentId: 'assignment-1',
      content: '测试状态转换',
      reasoning: '验证状态机',
      type: 'verification',
      workerId: 'claude' as WorkerSlot,
      priority: 2,
      dependsOn: [],
      requiredContracts: [],
      producesContracts: [],
    };

    const todo = await ctx.todoManager.create(params);
    details2.push(`初始状态: ${todo.status}`);

    // pending -> ready (通过 prepareForExecution)
    await ctx.todoManager.prepareForExecution(todo.id);
    let updated = await ctx.todoManager.get(todo.id);
    details2.push(`prepareForExecution 后: ${updated?.status}`);

    // ready -> running
    await ctx.todoManager.start(todo.id);
    updated = await ctx.todoManager.get(todo.id);
    details2.push(`start 后: ${updated?.status}`);

    // running -> completed
    await ctx.todoManager.complete(todo.id, {
      success: true,
      summary: '测试通过',
      modifiedFiles: [],
      duration: 100,
    });
    updated = await ctx.todoManager.get(todo.id);
    details2.push(`complete 后: ${updated?.status}`);

    if (updated?.status === 'completed') {
      passed2 = true;
      details2.push('✓ 状态转换正确');
    }
  } catch (e) {
    details2.push(`错误: ${e}`);
  }

  results.push({
    name: 'Todo 状态转换',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  // 测试 1.3: Todo 失败与重试
  console.log('\n  📋 测试 1.3: Todo 失败与重试');
  const start3 = Date.now();
  const details3: string[] = [];
  let passed3 = false;

  try {
    const params: CreateTodoParams = {
      missionId: 'mission-1',
      assignmentId: 'assignment-1',
      content: '测试重试机制',
      reasoning: '验证重试逻辑',
      type: 'implementation',
      workerId: 'claude' as WorkerSlot,
      priority: 3,
      dependsOn: [],
      requiredContracts: [],
      producesContracts: [],
      maxRetries: 3,
    };

    const todo = await ctx.todoManager.create(params);
    await ctx.todoManager.prepareForExecution(todo.id);
    await ctx.todoManager.start(todo.id);
    await ctx.todoManager.fail(todo.id, '模拟失败');

    let updated = await ctx.todoManager.get(todo.id);
    details3.push(`失败后状态: ${updated?.status}`);
    details3.push(`重试次数: ${updated?.retryCount}`);

    // 重试
    await ctx.todoManager.retry(todo.id);
    updated = await ctx.todoManager.get(todo.id);
    details3.push(`重试后状态: ${updated?.status}`);
    details3.push(`重试次数: ${updated?.retryCount}`);

    // 无依赖的 Todo 重试后会自动变成 ready
    if ((updated?.status === 'pending' || updated?.status === 'ready') && updated.retryCount === 1) {
      passed3 = true;
      details3.push('✓ 重试机制正常');
    }
  } catch (e) {
    details3.push(`错误: ${e}`);
  }

  results.push({
    name: 'Todo 失败与重试',
    passed: passed3,
    details: details3,
    duration: Date.now() - start3,
  });

  return results;
}

// ============================================================================
// 测试场景 2: 契约依赖检查
// ============================================================================

async function testContractDependency(ctx: TestContext): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 2.1: 契约注册与依赖检查
  console.log('\n  📋 测试 2.1: 契约注册与依赖检查');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    // 创建依赖契约的 Todo
    const dependentTodo = await ctx.todoManager.create({
      missionId: 'mission-2',
      assignmentId: 'assignment-2',
      content: '使用认证 API',
      reasoning: '需要先完成认证',
      type: 'integration',
      workerId: 'claude' as WorkerSlot,
      priority: 2,
      dependsOn: [],
      requiredContracts: ['auth-contract'],
      producesContracts: [],
    });

    details1.push(`依赖 Todo 初始状态: ${dependentTodo.status}`);

    // 尝试 prepareForExecution（应该变成 blocked，因为契约未满足）
    try {
      await ctx.todoManager.prepareForExecution(dependentTodo.id);
      const after = await ctx.todoManager.get(dependentTodo.id);
      details1.push(`prepareForExecution 后状态: ${after?.status}`);
      // 如果契约未满足，应该变成 blocked
      if (after?.status === 'blocked') {
        details1.push('✓ 正确识别契约未满足，状态变为 blocked');
      }
    } catch (e) {
      details1.push(`prepareForExecution 失败: ${(e as Error).message}`);
    }

    // 注册契约
    ctx.todoManager.registerContract('auth-contract');
    details1.push('已注册 auth-contract');

    // 再次尝试 canExecute
    const canExecute = await ctx.todoManager.canExecute(dependentTodo.id);
    details1.push(`依赖检查结果: ${canExecute}`);

    if (canExecute) {
      await ctx.todoManager.prepareForExecution(dependentTodo.id);
      const final = await ctx.todoManager.get(dependentTodo.id);
      details1.push(`最终状态: ${final?.status}`);
      if (final?.status === 'ready') {
        passed1 = true;
        details1.push('✓ 契约满足后可以就绪');
      }
    }
  } catch (e) {
    details1.push(`错误: ${e}`);
  }

  results.push({
    name: '契约注册与依赖检查',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 2.2: Todo 依赖链
  console.log('\n  📋 测试 2.2: Todo 依赖链');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    // 创建被依赖的 Todo
    const firstTodo = await ctx.todoManager.create({
      missionId: 'mission-3',
      assignmentId: 'assignment-3',
      content: '创建数据库表',
      reasoning: '数据库是基础设施',
      type: 'implementation',
      workerId: 'claude' as WorkerSlot,
      priority: 1,
      dependsOn: [],
      requiredContracts: [],
      producesContracts: ['db-schema'],
    });

    // 创建依赖第一个 Todo 的第二个 Todo
    const secondTodo = await ctx.todoManager.create({
      missionId: 'mission-3',
      assignmentId: 'assignment-3',
      content: '实现数据访问层',
      reasoning: '需要数据库表先创建',
      type: 'implementation',
      workerId: 'claude' as WorkerSlot,
      priority: 2,
      dependsOn: [firstTodo.id],
      requiredContracts: [],
      producesContracts: [],
    });

    details2.push(`第一个 Todo: ${firstTodo.id} (${firstTodo.status})`);
    details2.push(`第二个 Todo: ${secondTodo.id} (${secondTodo.status})`);

    // 第二个不应该能直接 ready（依赖未完成）
    const canSecondReady = await ctx.todoManager.canExecute(secondTodo.id);
    details2.push(`第二个能否就绪: ${canSecondReady}`);

    // 完成第一个
    await ctx.todoManager.prepareForExecution(firstTodo.id);
    await ctx.todoManager.start(firstTodo.id);
    await ctx.todoManager.complete(firstTodo.id, {
      success: true,
      summary: '数据库表已创建',
      modifiedFiles: ['schema.sql'],
      duration: 1000,
    });

    // 现在第二个应该可以了
    const canSecondReadyNow = await ctx.todoManager.canExecute(secondTodo.id);
    details2.push(`第一个完成后，第二个能否就绪: ${canSecondReadyNow}`);

    if (!canSecondReady && canSecondReadyNow) {
      passed2 = true;
      details2.push('✓ Todo 依赖链正确');
    }
  } catch (e) {
    details2.push(`错误: ${e}`);
  }

  results.push({
    name: 'Todo 依赖链',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  return results;
}

// ============================================================================
// 测试场景 3: LLM 驱动的 Todo 规划
// ============================================================================

async function testLLMTodoPlanning(ctx: TestContext): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 3.1: 使用 LLM 生成 Todo 规划
  console.log('\n  📋 测试 3.1: LLM 生成 Todo 规划');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    // 调用 LLM 生成任务规划
    const planningPrompt = `你是一个任务规划助手。请为以下任务生成详细的执行计划。

任务：实现一个简单的用户注册功能

请返回 JSON 格式的规划：
{
  "todos": [
    {
      "content": "任务描述",
      "reasoning": "为什么需要这个任务",
      "type": "implementation|verification|design",
      "priority": 1-5,
      "dependsOn": [],
      "requiredContracts": [],
      "producesContracts": ["contract-name"]
    }
  ]
}`;

    const result = await ctx.workerClient.sendMessage({
      messages: [{ role: 'user', content: planningPrompt }],
      systemPrompt: '你是一个专业的软件工程师，擅长任务分解和规划。',
      maxTokens: 2048,
      temperature: 0.3,
    });

    const content = result.content || '';
    details1.push(`LLM 响应长度: ${content.length}`);

    // 解析 JSON
    const jsonMatch = content.match(/\{[\s\S]*\}/);
    if (jsonMatch) {
      try {
        const plan = JSON.parse(jsonMatch[0]);
        if (plan.todos && Array.isArray(plan.todos)) {
          details1.push(`生成 Todo 数量: ${plan.todos.length}`);

          // 创建这些 Todo
          let createdCount = 0;
          for (const todoData of plan.todos) {
            const params: CreateTodoParams = {
              missionId: 'mission-llm-1',
              assignmentId: 'assignment-llm-1',
              content: todoData.content || '未命名任务',
              reasoning: todoData.reasoning || '',
              type: (todoData.type || 'implementation') as any,
              workerId: 'claude' as WorkerSlot,
              priority: todoData.priority || 3,
              dependsOn: todoData.dependsOn || [],
              requiredContracts: todoData.requiredContracts || [],
              producesContracts: todoData.producesContracts || [],
            };

            await ctx.todoManager.create(params);
            createdCount++;
          }

          details1.push(`成功创建 Todo: ${createdCount}`);

          if (createdCount > 0) {
            passed1 = true;
            details1.push('✓ LLM Todo 规划成功');

            // 展示前 3 个
            plan.todos.slice(0, 3).forEach((t: any, i: number) => {
              details1.push(`  [${i + 1}] ${t.content?.substring(0, 40)}...`);
            });
          }
        }
      } catch (e) {
        details1.push(`JSON 解析错误: ${(e as Error).message}`);
      }
    } else {
      details1.push('未找到有效 JSON');
    }
  } catch (e) {
    details1.push(`错误: ${e}`);
  }

  results.push({
    name: 'LLM 生成 Todo 规划',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 3.2: 验证规划的 Todo 质量
  console.log('\n  📋 测试 3.2: 验证 Todo 规划质量');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    const todos = await ctx.todoManager.getByMission('mission-llm-1');
    details2.push(`Mission 中的 Todo 数量: ${todos.length}`);

    let validCount = 0;
    for (const todo of todos) {
      const hasContent = todo.content && todo.content.length > 5;
      const hasReasoning = todo.reasoning && todo.reasoning.length > 0;
      const hasValidPriority = todo.priority >= 1 && todo.priority <= 5;

      if (hasContent && hasReasoning && hasValidPriority) {
        validCount++;
      }
    }

    details2.push(`有效 Todo 数量: ${validCount}`);

    if (validCount > 0 && validCount >= todos.length * 0.5) {
      passed2 = true;
      details2.push('✓ Todo 质量合格');
    } else {
      details2.push('✗ Todo 质量不足');
    }
  } catch (e) {
    details2.push(`错误: ${e}`);
  }

  results.push({
    name: '验证 Todo 规划质量',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  return results;
}

// ============================================================================
// 测试场景 4: 完整执行流程
// ============================================================================

async function testFullExecutionFlow(ctx: TestContext): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 4.1: 完整的 Todo 执行流程
  console.log('\n  📋 测试 4.1: 完整执行流程');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    // 创建一组有依赖关系的 Todo
    const todo1 = await ctx.todoManager.create({
      missionId: 'mission-full-1',
      assignmentId: 'assignment-full-1',
      content: '分析需求',
      reasoning: '第一步是理解需求',
      type: 'discovery',
      workerId: 'claude' as WorkerSlot,
      priority: 1,
      dependsOn: [],
      requiredContracts: [],
      producesContracts: ['requirements-doc'],
    });

    const todo2 = await ctx.todoManager.create({
      missionId: 'mission-full-1',
      assignmentId: 'assignment-full-1',
      content: '设计架构',
      reasoning: '基于需求设计',
      type: 'design',
      workerId: 'claude' as WorkerSlot,
      priority: 2,
      dependsOn: [todo1.id],
      requiredContracts: ['requirements-doc'],
      producesContracts: ['architecture-doc'],
    });

    const todo3 = await ctx.todoManager.create({
      missionId: 'mission-full-1',
      assignmentId: 'assignment-full-1',
      content: '实现功能',
      reasoning: '按设计实现',
      type: 'implementation',
      workerId: 'claude' as WorkerSlot,
      priority: 3,
      dependsOn: [todo2.id],
      requiredContracts: ['architecture-doc'],
      producesContracts: [],
    });

    details1.push('创建了 3 个 Todo：分析需求 -> 设计架构 -> 实现功能');

    // 执行 Todo 1
    await ctx.todoManager.prepareForExecution(todo1.id);
    await ctx.todoManager.start(todo1.id);

    // 调用 LLM 执行
    const result1 = await ctx.workerClient.sendMessage({
      messages: [{ role: 'user', content: `执行任务：${todo1.content}\n\n请简要描述你的分析结果。` }],
      systemPrompt: '你是一个软件工程师助手。',
      maxTokens: 512,
      temperature: 0.3,
    });

    await ctx.todoManager.complete(todo1.id, {
      success: true,
      summary: result1.content?.substring(0, 100) || '分析完成',
      modifiedFiles: [],
      duration: Date.now() - start1,
    });

    details1.push('✓ Todo 1 (分析需求) 完成');

    // Todo 2 现在应该可以执行了
    const canTodo2Ready = await ctx.todoManager.canExecute(todo2.id);
    details1.push(`Todo 2 可执行: ${canTodo2Ready}`);

    if (canTodo2Ready) {
      await ctx.todoManager.prepareForExecution(todo2.id);
      await ctx.todoManager.start(todo2.id);
      await ctx.todoManager.complete(todo2.id, {
        success: true,
        summary: '设计完成',
        modifiedFiles: ['design.md'],
        duration: 500,
      });
      details1.push('✓ Todo 2 (设计架构) 完成');
    }

    // Todo 3
    const canTodo3Ready = await ctx.todoManager.canExecute(todo3.id);
    details1.push(`Todo 3 可执行: ${canTodo3Ready}`);

    if (canTodo3Ready) {
      await ctx.todoManager.prepareForExecution(todo3.id);
      await ctx.todoManager.start(todo3.id);
      await ctx.todoManager.complete(todo3.id, {
        success: true,
        summary: '实现完成',
        modifiedFiles: ['src/index.ts'],
        duration: 1000,
      });
      details1.push('✓ Todo 3 (实现功能) 完成');
    }

    // 验证所有 Todo 都完成了
    const allTodos = await ctx.todoManager.getByMission('mission-full-1');
    const completedCount = allTodos.filter(t => t.status === 'completed').length;

    details1.push(`完成的 Todo: ${completedCount}/${allTodos.length}`);

    if (completedCount === 3) {
      passed1 = true;
      details1.push('✓ 完整执行流程成功');
    }
  } catch (e) {
    details1.push(`错误: ${e}`);
  }

  results.push({
    name: '完整执行流程',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  return results;
}

// ============================================================================
// 主程序
// ============================================================================

function printResults(results: TestResult[]): void {
  for (const result of results) {
    const icon = result.passed ? '✓' : '✗';
    console.log(`    ${icon} ${result.name} (${result.duration}ms)`);
    result.details.forEach(d => {
      console.log(`        ${d}`);
    });
  }
}

async function main() {
  console.log('============================================================');
  console.log('统一 Todo 系统端对端测试');
  console.log('============================================================');
  console.log('');

  const configPath = path.join(os.homedir(), '.magi', 'llm.json');
  if (!fs.existsSync(configPath)) {
    console.error('错误: 未找到 LLM 配置文件 (~/.magi/llm.json)');
    process.exit(1);
  }

  const config = LLMConfigLoader.loadFullConfig();
  console.log(`Workers: ${Object.keys(config.workers).filter(k => (config.workers as any)[k].enabled).join(', ')}`);
  console.log('');

  let ctx: TestContext | null = null;
  const allResults: TestResult[] = [];

  try {
    ctx = await createTestContext();

    // 场景 1: Todo 生命周期管理
    console.log('【场景 1: Todo 生命周期管理】');
    const lifecycleResults = await testTodoLifecycle(ctx);
    allResults.push(...lifecycleResults);
    printResults(lifecycleResults);

    // 场景 2: 契约依赖检查
    console.log('\n【场景 2: 契约依赖检查】');
    const contractResults = await testContractDependency(ctx);
    allResults.push(...contractResults);
    printResults(contractResults);

    // 场景 3: LLM 驱动的 Todo 规划
    console.log('\n【场景 3: LLM 驱动的 Todo 规划】');
    const llmResults = await testLLMTodoPlanning(ctx);
    allResults.push(...llmResults);
    printResults(llmResults);

    // 场景 4: 完整执行流程
    console.log('\n【场景 4: 完整执行流程】');
    const flowResults = await testFullExecutionFlow(ctx);
    allResults.push(...flowResults);
    printResults(flowResults);

  } catch (error) {
    console.error('测试执行错误:', error);
  } finally {
    // 清理
    if (ctx) {
      ctx.todoManager.destroy();
      cleanupTestDir(ctx.workspaceRoot);
    }
  }

  // 汇总
  console.log('\n============================================================');
  console.log('测试汇总');
  console.log('============================================================');

  const passed = allResults.filter(r => r.passed).length;
  const total = allResults.length;
  const passRate = total > 0 ? Math.round((passed / total) * 100) : 0;

  console.log(`通过: ${passed}/${total} (${passRate}%)`);
  console.log(`失败: ${total - passed}/${total}`);

  if (total - passed > 0) {
    console.log('\n失败场景:');
    for (const result of allResults.filter(r => !r.passed)) {
      console.log(`  ✗ ${result.name}`);
    }
  }

  console.log('\n============================================================');
  console.log('验证结论');
  console.log('============================================================');

  if (passRate >= 80) {
    console.log('✅ 统一 Todo 系统验证通过');
    console.log('  - Todo 生命周期管理: 正常');
    console.log('  - 契约依赖检查: 正常');
    console.log('  - LLM 规划集成: 正常');
    console.log('  - 完整执行流程: 正常');
  } else {
    console.log('❌ 存在需要修复的问题');
  }

  process.exit(passRate >= 80 ? 0 : 1);
}

main().catch((err) => {
  console.error('测试运行失败:', err);
  process.exit(1);
});
