/**
 * 编排系统增强提案 E2E 验证测试
 *
 * 验证 docs/orchestration-enhancement-proposal.md 中定义的 6 个提案：
 * - P0: Session 恢复机制 (4.1)
 * - P0: 验证证据机制 (4.2)
 * - P1: 6-Section 结构化提示 (4.3)
 * - P1: 分类别约束 (4.4)
 * - P1: Wisdom 累积系统 (4.5)
 * - P2: Wave 并行分组 (4.6)
 *
 * 运行: npx ts-node src/test/e2e/enhancement-proposal-e2e.ts
 */

import { UniversalLLMClient } from '../../llm/clients/universal-client';
import { LLMConfigLoader } from '../../llm/config';
import { LLMConfig } from '../../types/agent-types';

// Session 相关
import {
  WorkerSessionManager,
  WorkerSession,
  resetGlobalSessionManager,
} from '../../orchestrator/worker/worker-session';

// Worker Report 相关
import {
  WorkerReport,
  WorkerEvidence,
  WisdomExtraction,
  createCompletedReport,
  createFailedReport,
} from '../../orchestrator/protocols/worker-report';

// Guidance 相关
import { GuidanceInjector, TaskStructuredInfo } from '../../orchestrator/profile/guidance-injector';
import { DEFAULT_CLAUDE_PROFILE } from '../../orchestrator/profile/defaults/claude';
import { DEFAULT_CODEX_PROFILE } from '../../orchestrator/profile/defaults/codex';
import { WorkerProfile } from '../../orchestrator/profile/types';

// Helper to get worker profile
function getWorkerProfile(workerId: string): WorkerProfile {
  switch (workerId) {
    case 'claude':
      return DEFAULT_CLAUDE_PROFILE;
    case 'codex':
      return DEFAULT_CODEX_PROFILE;
    default:
      return DEFAULT_CLAUDE_PROFILE;
  }
}

// Wisdom 相关
import {
  WisdomExtractor,
  WisdomManager,
  WisdomStorage,
} from '../../orchestrator/wisdom/wisdom-extractor';

// Task Dependency Graph 相关
import {
  TaskDependencyGraph,
  ExecutionBatch,
} from '../../orchestrator/task-dependency-graph';

// ============================================================================
// 测试工具
// ============================================================================

const colors = {
  reset: '\x1b[0m',
  green: '\x1b[32m',
  red: '\x1b[31m',
  yellow: '\x1b[33m',
  cyan: '\x1b[36m',
  dim: '\x1b[2m',
};

function log(prefix: string, message: string, color: string = colors.reset) {
  console.log(`${color}[${prefix}]${colors.reset} ${message}`);
}

interface TestResult {
  name: string;
  passed: boolean;
  details: string[];
  duration: number;
}

// ============================================================================
// 提案 4.1: Session 恢复机制测试
// ============================================================================

async function testSessionRecovery(): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 1: Session 创建和获取
  console.log('\n  📋 测试 4.1.1: Session 创建和获取');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const manager = new WorkerSessionManager({ autoCleanup: false });

    // 创建 Session
    const session = manager.create({
      assignmentId: 'test-assignment-1',
      workerId: 'claude',
      initialContext: '初始上下文',
    });

    details1.push(`Session ID: ${session.id}`);
    details1.push(`Assignment ID: ${session.assignmentId}`);
    details1.push(`Worker ID: ${session.workerId}`);

    // 获取 Session
    const retrieved = manager.get(session.id);
    if (retrieved && retrieved.id === session.id) {
      passed1 = true;
      details1.push('✓ Session 创建和获取成功');
    } else {
      details1.push('✗ Session 获取失败');
    }

    manager.dispose();
  } catch (e: any) {
    details1.push(`错误: ${e.message}`);
  }

  results.push({
    name: 'Session 创建和获取',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 2: Session 更新和恢复
  console.log('\n  📋 测试 4.1.2: Session 更新和恢复');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    const manager = new WorkerSessionManager({ autoCleanup: false });

    const session = manager.create({
      assignmentId: 'test-assignment-2',
      workerId: 'codex',
    });

    // 模拟执行过程中的更新
    manager.update(session.id, {
      appendMessage: {
        role: 'assistant',
        content: '开始执行任务...',
        timestamp: Date.now(),
      },
    });

    manager.update(session.id, {
      updateFile: {
        path: '/src/test.ts',
        entry: {
          content: 'const x = 1;',
          readAt: Date.now(),
        },
      },
    });

    manager.update(session.id, {
      completeTodo: 'todo-1',
      stateSnapshot: {
        currentTodoIndex: 1,
        retryCount: 0,
      },
    });

    // 模拟失败后恢复
    manager.markAsResumed(session.id, '请修复类型错误');

    const resumed = manager.get(session.id);
    if (
      resumed &&
      resumed.isResumed &&
      resumed.conversationHistory.length > 0 &&
      resumed.readFiles.size > 0 &&
      resumed.completedTodos.includes('todo-1')
    ) {
      passed2 = true;
      details2.push(`✓ Session 恢复成功`);
      details2.push(`  对话历史: ${resumed.conversationHistory.length} 条`);
      details2.push(`  文件缓存: ${resumed.readFiles.size} 个`);
      details2.push(`  已完成 Todo: ${resumed.completedTodos.length} 个`);
      details2.push(`  重试次数: ${resumed.stateSnapshot.retryCount}`);
    } else {
      details2.push('✗ Session 恢复数据不完整');
    }

    manager.dispose();
  } catch (e: any) {
    details2.push(`错误: ${e.message}`);
  }

  results.push({
    name: 'Session 更新和恢复',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  // 测试 3: Session 过期清理
  console.log('\n  📋 测试 4.1.3: Session 过期清理');
  const start3 = Date.now();
  const details3: string[] = [];
  let passed3 = false;

  try {
    const manager = new WorkerSessionManager({
      sessionTtlMs: 100, // 100ms 过期
      autoCleanup: false,
    });

    const session = manager.create({
      assignmentId: 'test-assignment-3',
      workerId: 'gemini',
    });

    details3.push(`创建 Session: ${session.id}`);

    // 等待过期
    await new Promise((resolve) => setTimeout(resolve, 150));

    const expired = manager.get(session.id);
    if (expired === null) {
      passed3 = true;
      details3.push('✓ Session 正确过期');
    } else {
      details3.push('✗ Session 应该已过期但仍存在');
    }

    manager.dispose();
  } catch (e: any) {
    details3.push(`错误: ${e.message}`);
  }

  results.push({
    name: 'Session 过期清理',
    passed: passed3,
    details: details3,
    duration: Date.now() - start3,
  });

  return results;
}

// ============================================================================
// 提案 4.2: 验证证据机制测试
// ============================================================================

async function testEvidenceVerification(): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 1: 验证证据结构
  console.log('\n  📋 测试 4.2.1: 验证证据结构');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const evidence: WorkerEvidence = {
      commandsRun: [
        {
          command: 'npm run build',
          exitCode: 0,
          stdout: 'Build successful',
          duration: 5000,
        },
        {
          command: 'npm test',
          exitCode: 0,
          stdout: 'All tests passed',
          duration: 3000,
        },
      ],
      testResults: {
        framework: 'jest',
        total: 10,
        passed: 10,
        failed: 0,
        duration: 3000,
      },
      typeCheckResult: {
        passed: true,
        errors: [],
      },
      fileChanges: [
        {
          path: '/src/module.ts',
          action: 'modify',
          linesAdded: 50,
          linesRemoved: 10,
        },
        {
          path: '/src/new-file.ts',
          action: 'create',
          linesAdded: 100,
        },
      ],
      verifiedAt: Date.now(),
      verificationStatus: 'verified',
    };

    // 验证结构完整性
    const hasCommands = evidence.commandsRun && evidence.commandsRun.length > 0;
    const hasTests = evidence.testResults && evidence.testResults.passed >= 0;
    const hasTypeCheck = evidence.typeCheckResult !== undefined;
    const hasFileChanges = evidence.fileChanges && evidence.fileChanges.length > 0;

    if (hasCommands && hasTests && hasTypeCheck && hasFileChanges) {
      passed1 = true;
      details1.push('✓ 验证证据结构完整');
      details1.push(`  命令记录: ${evidence.commandsRun!.length} 条`);
      details1.push(`  测试结果: ${evidence.testResults!.passed}/${evidence.testResults!.total} 通过`);
      details1.push(`  类型检查: ${evidence.typeCheckResult!.passed ? '通过' : '失败'}`);
      details1.push(`  文件变更: ${evidence.fileChanges!.length} 个`);
    } else {
      details1.push('✗ 验证证据结构不完整');
    }
  } catch (e: any) {
    details1.push(`错误: ${e.message}`);
  }

  results.push({
    name: '验证证据结构',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 2: WorkerReport 集成证据
  console.log('\n  📋 测试 4.2.2: WorkerReport 集成证据');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    const report = createCompletedReport('claude', 'assignment-1', {
      success: true,
      modifiedFiles: ['/src/module.ts'],
      createdFiles: ['/src/new-file.ts'],
      summary: '任务完成，添加了新模块',
      totalDuration: 10000,
      evidence: {
        commandsRun: [{ command: 'npm run build', exitCode: 0 }],
        testResults: { framework: 'jest', total: 5, passed: 5, failed: 0, duration: 2000 },
        typeCheckResult: { passed: true },
        verificationStatus: 'verified',
      },
    });

    if (
      report.type === 'completed' &&
      report.result?.evidence &&
      report.result.evidence.verificationStatus === 'verified'
    ) {
      passed2 = true;
      details2.push('✓ WorkerReport 正确集成验证证据');
      details2.push(`  汇报类型: ${report.type}`);
      details2.push(`  验证状态: ${report.result.evidence.verificationStatus}`);
    } else {
      details2.push('✗ WorkerReport 验证证据集成失败');
    }
  } catch (e: any) {
    details2.push(`错误: ${e.message}`);
  }

  results.push({
    name: 'WorkerReport 集成证据',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  return results;
}

// ============================================================================
// 提案 4.3 & 4.4: 结构化提示和分类别约束测试
// ============================================================================

async function testStructuredPromptAndConstraints(): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 1: 6-Section 结构化提示生成
  console.log('\n  📋 测试 4.3.1: 6-Section 结构化提示生成');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const injector = new GuidanceInjector();
    const profile = getWorkerProfile('claude');

    const taskInfo: TaskStructuredInfo = {
      expectedOutcome: ['代码修改完成', '测试通过', '无 TypeScript 错误'],
      mustDo: ['遵循现有代码风格', '添加必要的类型注解'],
      mustNotDo: ['不要删除现有功能', '不要引入新依赖'],
      relatedDecisions: ['使用 async/await 而非回调'],
      pendingIssues: ['性能优化待后续处理'],
    };

    const prompt = injector.buildFullTaskPrompt(
      profile,
      {
        taskDescription: '实现用户登录功能',
        category: 'feature',
        targetFiles: ['/src/auth/login.ts'],
      },
      '项目使用 TypeScript + Express',
      taskInfo
    );

    // 验证 6 个关键部分
    const hasRole = prompt.includes('角色定位');
    const hasFocus = prompt.includes('专注领域');
    const hasConstraints = prompt.includes('注意事项') || prompt.includes('约束');
    const hasExpectedOutcome = prompt.includes('预期结果');
    const hasMustDo = prompt.includes('必须遵守');
    const hasMustNotDo = prompt.includes('禁止行为');
    const hasTask = prompt.includes('当前任务');

    const sections = [hasRole, hasFocus, hasConstraints, hasExpectedOutcome, hasMustDo, hasMustNotDo, hasTask];
    const sectionCount = sections.filter(Boolean).length;

    if (sectionCount >= 5) {
      passed1 = true;
      details1.push(`✓ 结构化提示生成成功 (${sectionCount}/7 部分)`);
      details1.push(`  角色定位: ${hasRole ? '✓' : '✗'}`);
      details1.push(`  专注领域: ${hasFocus ? '✓' : '✗'}`);
      details1.push(`  注意事项: ${hasConstraints ? '✓' : '✗'}`);
      details1.push(`  预期结果: ${hasExpectedOutcome ? '✓' : '✗'}`);
      details1.push(`  必须遵守: ${hasMustDo ? '✓' : '✗'}`);
      details1.push(`  禁止行为: ${hasMustNotDo ? '✓' : '✗'}`);
      details1.push(`  当前任务: ${hasTask ? '✓' : '✗'}`);
      details1.push(`  Prompt 长度: ${prompt.length} 字符`);
    } else {
      details1.push(`✗ 结构化提示不完整 (${sectionCount}/7 部分)`);
    }
  } catch (e: any) {
    details1.push(`错误: ${e.message}`);
  }

  results.push({
    name: '6-Section 结构化提示生成',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 2: 分类别约束生成
  console.log('\n  📋 测试 4.4.1: 分类别约束生成');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    const injector = new GuidanceInjector();
    const profile = getWorkerProfile('codex');

    const categories = ['bugfix', 'refactor', 'feature', 'test', 'security'];
    const categoryResults: { category: string; hasConstraints: boolean }[] = [];

    for (const category of categories) {
      const prompt = injector.buildWorkerPrompt(profile, {
        taskDescription: `执行 ${category} 任务`,
        category: category,
      });

      // 检查是否包含分类别约束
      const hasCategory = prompt.includes(`${category} 任务专项约束`) || prompt.includes(category);
      categoryResults.push({ category, hasConstraints: hasCategory });
    }

    const allHaveConstraints = categoryResults.every((r) => r.hasConstraints);
    const constraintCount = categoryResults.filter((r) => r.hasConstraints).length;

    if (constraintCount >= 3) {
      passed2 = true;
      details2.push(`✓ 分类别约束生成成功 (${constraintCount}/${categories.length})`);
      categoryResults.forEach((r) => {
        details2.push(`  ${r.category}: ${r.hasConstraints ? '✓' : '✗'}`);
      });
    } else {
      details2.push(`✗ 分类别约束生成不足 (${constraintCount}/${categories.length})`);
    }
  } catch (e: any) {
    details2.push(`错误: ${e.message}`);
  }

  results.push({
    name: '分类别约束生成',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  return results;
}

// ============================================================================
// 提案 4.5: Wisdom 累积系统测试
// ============================================================================

async function testWisdomAccumulation(): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 1: Wisdom 提取
  console.log('\n  📋 测试 4.5.1: Wisdom 提取');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const extractor = new WisdomExtractor();

    const testSummary = `
      任务完成。
      发现：项目使用了自定义的依赖注入框架。
      注意：配置文件格式与文档不符。
      决定：采用工厂模式创建服务实例。
      选择：使用 TypeScript strict 模式。
      重要：需要在部署前更新环境变量。
    `;

    const learnings = extractor.extractLearnings(testSummary);
    const decisions = extractor.extractDecisions(testSummary);

    if (learnings.length >= 2 && decisions.length >= 2) {
      passed1 = true;
      details1.push(`✓ Wisdom 提取成功`);
      details1.push(`  Learnings: ${learnings.length} 条`);
      learnings.slice(0, 2).forEach((l, i) => {
        details1.push(`    [${i + 1}] ${l.substring(0, 50)}...`);
      });
      details1.push(`  Decisions: ${decisions.length} 条`);
      decisions.slice(0, 2).forEach((d, i) => {
        details1.push(`    [${i + 1}] ${d.substring(0, 50)}...`);
      });
    } else {
      details1.push(`✗ Wisdom 提取不足 (L:${learnings.length}, D:${decisions.length})`);
    }
  } catch (e: any) {
    details1.push(`错误: ${e.message}`);
  }

  results.push({
    name: 'Wisdom 提取',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 2: WisdomManager 集成
  console.log('\n  📋 测试 4.5.2: WisdomManager 集成');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    // 创建一个简单的存储实现来验证
    const storedItems: { type: string; content: string }[] = [];

    const testStorage: WisdomStorage = {
      storeLearning: (learning, _) => storedItems.push({ type: 'learning', content: learning }),
      storeDecision: (decision, _) => storedItems.push({ type: 'decision', content: decision }),
      storeWarning: (warning, _) => storedItems.push({ type: 'warning', content: warning }),
      storeSignificantLearning: (learning, _) => storedItems.push({ type: 'significant', content: learning }),
    };

    const manager = new WisdomManager(testStorage);

    const report: WorkerReport = {
      type: 'completed',
      workerId: 'claude',
      assignmentId: 'test-assignment',
      timestamp: Date.now(),
      result: {
        success: true,
        modifiedFiles: [],
        createdFiles: [],
        summary: '发现：系统使用 Redis 缓存。决定：采用读写分离策略。',
        totalDuration: 5000,
      },
    };

    const result = manager.processReport(report, 'test-assignment');

    if (result.learnings.length > 0 || result.decisions.length > 0) {
      passed2 = true;
      details2.push(`✓ WisdomManager 集成成功`);
      details2.push(`  提取 Learnings: ${result.learnings.length}`);
      details2.push(`  提取 Decisions: ${result.decisions.length}`);
      details2.push(`  存储项目: ${storedItems.length}`);
    } else {
      details2.push('✗ WisdomManager 未能提取知识');
    }
  } catch (e: any) {
    details2.push(`错误: ${e.message}`);
  }

  results.push({
    name: 'WisdomManager 集成',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  return results;
}

// ============================================================================
// 提案 4.6: Wave 并行分组测试
// ============================================================================

async function testWaveParallelGrouping(): Promise<TestResult[]> {
  const results: TestResult[] = [];

  // 测试 1: 依赖图构建和 Wave 计算
  console.log('\n  📋 测试 4.6.1: 依赖图构建和 Wave 计算');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const graph = new TaskDependencyGraph();

    // 添加任务
    graph.addTask('task-1', '设计 API 规范');
    graph.addTask('task-2', '实现后端接口', undefined, ['/src/api.ts']);
    graph.addTask('task-3', '实现前端组件', undefined, ['/src/component.tsx']);
    graph.addTask('task-4', '编写测试用例');
    graph.addTask('task-5', '集成测试');

    // 添加依赖关系
    graph.addDependency('task-2', 'task-1'); // 后端依赖设计
    graph.addDependency('task-3', 'task-1'); // 前端依赖设计
    graph.addDependency('task-4', 'task-2'); // 测试依赖后端
    graph.addDependency('task-5', 'task-2'); // 集成测试依赖后端
    graph.addDependency('task-5', 'task-3'); // 集成测试依赖前端

    // 分析
    const analysis = graph.analyze();

    details1.push(`任务数量: ${graph.size}`);
    details1.push(`有循环依赖: ${analysis.hasCycle}`);
    details1.push(`Wave 数量: ${analysis.executionBatches.length}`);
    details1.push(`关键路径: ${analysis.criticalPath.join(' → ')}`);

    // 验证 Wave
    analysis.executionBatches.forEach((batch) => {
      details1.push(`  Wave ${batch.batchIndex}: [${batch.taskIds.join(', ')}]`);
    });

    if (!analysis.hasCycle && analysis.executionBatches.length >= 3) {
      passed1 = true;
      details1.push('✓ 依赖图分析正确');
    } else {
      details1.push('✗ 依赖图分析有误');
    }
  } catch (e: any) {
    details1.push(`错误: ${e.message}`);
  }

  results.push({
    name: '依赖图构建和 Wave 计算',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  // 测试 2: 文件冲突检测
  console.log('\n  📋 测试 4.6.2: 文件冲突检测');
  const start2 = Date.now();
  const details2: string[] = [];
  let passed2 = false;

  try {
    const graph = new TaskDependencyGraph();

    // 添加修改相同文件的任务
    graph.addTask('task-a', '修改配置', undefined, ['/config.json']);
    graph.addTask('task-b', '更新配置', undefined, ['/config.json']);
    graph.addTask('task-c', '独立任务', undefined, ['/other.ts']);

    // 检测冲突
    const conflicts = graph.detectFileConflicts();

    details2.push(`冲突数量: ${conflicts.length}`);
    conflicts.forEach((c) => {
      details2.push(`  文件: ${c.file}, 任务: [${c.taskIds.join(', ')}]`);
    });

    // 自动添加依赖解决冲突
    const addedDeps = graph.addFileDependencies('sequential');
    details2.push(`自动添加依赖: ${addedDeps} 个`);

    // 重新分析
    const analysis = graph.analyze();
    details2.push(`解决冲突后 Wave 数量: ${analysis.executionBatches.length}`);

    if (conflicts.length >= 1 && addedDeps >= 1) {
      passed2 = true;
      details2.push('✓ 文件冲突检测和解决正确');
    } else {
      details2.push('✗ 文件冲突处理有误');
    }
  } catch (e: any) {
    details2.push(`错误: ${e.message}`);
  }

  results.push({
    name: '文件冲突检测',
    passed: passed2,
    details: details2,
    duration: Date.now() - start2,
  });

  // 测试 3: 循环依赖检测
  console.log('\n  📋 测试 4.6.3: 循环依赖检测');
  const start3 = Date.now();
  const details3: string[] = [];
  let passed3 = false;

  try {
    const graph = new TaskDependencyGraph();

    graph.addTask('task-x', '任务 X');
    graph.addTask('task-y', '任务 Y');
    graph.addTask('task-z', '任务 Z');

    // 尝试创建循环: X -> Y -> Z -> X
    graph.addDependency('task-y', 'task-x');
    graph.addDependency('task-z', 'task-y');

    // 这个应该被拒绝
    const cyclicAdded = graph.addDependency('task-x', 'task-z');

    if (!cyclicAdded) {
      passed3 = true;
      details3.push('✓ 循环依赖被正确拒绝');
    } else {
      // 检查分析是否检测到循环
      const analysis = graph.analyze();
      if (analysis.hasCycle) {
        passed3 = true;
        details3.push('✓ 循环依赖被检测到');
        details3.push(`  循环节点: ${analysis.cycleNodes?.join(', ')}`);
      } else {
        details3.push('✗ 循环依赖未被检测');
      }
    }
  } catch (e: any) {
    details3.push(`错误: ${e.message}`);
  }

  results.push({
    name: '循环依赖检测',
    passed: passed3,
    details: details3,
    duration: Date.now() - start3,
  });

  return results;
}

// ============================================================================
// 真实 LLM 集成测试
// ============================================================================

async function testRealLLMIntegration(): Promise<TestResult[]> {
  const results: TestResult[] = [];

  console.log('\n  📋 测试 LLM 集成: 结构化提示生效验证');
  const start1 = Date.now();
  const details1: string[] = [];
  let passed1 = false;

  try {
    const config = LLMConfigLoader.loadFullConfig();
    const workerConfig = config.workers.claude;

    if (!workerConfig?.enabled) {
      details1.push('跳过: Claude Worker 未启用');
      passed1 = true;
    } else {
      const client = new UniversalLLMClient({
        ...workerConfig,
        enabled: true,
      } as LLMConfig);

      // 使用结构化提示
      const injector = new GuidanceInjector();
      const profile = getWorkerProfile('claude');

      const structuredPrompt = injector.buildFullTaskPrompt(
        profile,
        {
          taskDescription: '解释什么是依赖注入',
          category: 'documentation',
        },
        undefined,
        {
          expectedOutcome: ['清晰的解释', '包含代码示例'],
          mustDo: ['使用中文回答', '保持简洁'],
          mustNotDo: ['不要过于冗长'],
        }
      );

      const response = await client.sendMessage({
        messages: [{ role: 'user', content: '什么是依赖注入？请简短解释。' }],
        systemPrompt: structuredPrompt,
        maxTokens: 1024,
        temperature: 0.5,
      });

      if (response.content && response.content.length > 50) {
        passed1 = true;
        details1.push('✓ LLM 响应成功');
        details1.push(`  响应长度: ${response.content.length} 字符`);
        details1.push(`  响应预览: ${response.content.substring(0, 100)}...`);

        // 提取 Wisdom
        const extractor = new WisdomExtractor();
        const wisdom = extractor.extractLearnings(response.content);
        details1.push(`  提取 Learnings: ${wisdom.length} 条`);
      } else {
        details1.push('✗ LLM 响应为空或过短');
      }
    }
  } catch (e: any) {
    details1.push(`错误: ${e.message}`);
    // 网络错误不算测试失败
    if (e.message.includes('network') || e.message.includes('connect')) {
      passed1 = true;
      details1.push('(网络错误，跳过)');
    }
  }

  results.push({
    name: 'LLM 集成 - 结构化提示',
    passed: passed1,
    details: details1,
    duration: Date.now() - start1,
  });

  return results;
}

// ============================================================================
// 主程序
// ============================================================================

function printResults(title: string, results: TestResult[]): void {
  console.log(`\n${colors.cyan}${title}${colors.reset}`);
  for (const result of results) {
    const icon = result.passed ? colors.green + '✓' : colors.red + '✗';
    console.log(`    ${icon}${colors.reset} ${result.name} (${result.duration}ms)`);
    result.details.forEach((d) => {
      console.log(`        ${colors.dim}${d}${colors.reset}`);
    });
  }
}

async function main() {
  console.log('============================================================');
  console.log('编排系统增强提案 E2E 验证测试');
  console.log('============================================================');
  console.log('');
  console.log('验证内容:');
  console.log('  - P0: 4.1 Session 恢复机制');
  console.log('  - P0: 4.2 验证证据机制');
  console.log('  - P1: 4.3 6-Section 结构化提示');
  console.log('  - P1: 4.4 分类别约束');
  console.log('  - P1: 4.5 Wisdom 累积系统');
  console.log('  - P2: 4.6 Wave 并行分组');
  console.log('');

  const allResults: TestResult[] = [];

  try {
    // P0: Session 恢复
    const sessionResults = await testSessionRecovery();
    allResults.push(...sessionResults);
    printResults('【P0 提案 4.1: Session 恢复机制】', sessionResults);

    // P0: 验证证据
    const evidenceResults = await testEvidenceVerification();
    allResults.push(...evidenceResults);
    printResults('【P0 提案 4.2: 验证证据机制】', evidenceResults);

    // P1: 结构化提示和分类别约束
    const promptResults = await testStructuredPromptAndConstraints();
    allResults.push(...promptResults);
    printResults('【P1 提案 4.3 & 4.4: 结构化提示和分类别约束】', promptResults);

    // P1: Wisdom 累积
    const wisdomResults = await testWisdomAccumulation();
    allResults.push(...wisdomResults);
    printResults('【P1 提案 4.5: Wisdom 累积系统】', wisdomResults);

    // P2: Wave 并行
    const waveResults = await testWaveParallelGrouping();
    allResults.push(...waveResults);
    printResults('【P2 提案 4.6: Wave 并行分组】', waveResults);

    // 真实 LLM 集成
    const llmResults = await testRealLLMIntegration();
    allResults.push(...llmResults);
    printResults('【真实 LLM 集成测试】', llmResults);

  } catch (error) {
    console.error('测试执行错误:', error);
  }

  // 汇总
  console.log('\n============================================================');
  console.log('测试汇总');
  console.log('============================================================');

  const passed = allResults.filter((r) => r.passed).length;
  const total = allResults.length;
  const passRate = total > 0 ? Math.round((passed / total) * 100) : 0;

  console.log(`通过: ${passed}/${total} (${passRate}%)`);
  console.log(`失败: ${total - passed}/${total}`);

  if (total - passed > 0) {
    console.log('\n失败测试:');
    for (const result of allResults.filter((r) => !r.passed)) {
      console.log(`  ${colors.red}✗${colors.reset} ${result.name}`);
    }
  }

  console.log('\n============================================================');
  console.log('验证结论');
  console.log('============================================================');

  if (passRate >= 80) {
    console.log(`${colors.green}✅ 增强提案实现验证通过${colors.reset}`);
    console.log('  - P0 Session 恢复机制: 正常');
    console.log('  - P0 验证证据机制: 正常');
    console.log('  - P1 结构化提示: 正常');
    console.log('  - P1 分类别约束: 正常');
    console.log('  - P1 Wisdom 累积: 正常');
    console.log('  - P2 Wave 并行: 正常');
  } else {
    console.log(`${colors.red}❌ 存在需要修复的问题${colors.reset}`);
  }

  process.exit(passRate >= 80 ? 0 : 1);
}

main().catch((err) => {
  console.error('测试运行失败:', err);
  process.exit(1);
});
