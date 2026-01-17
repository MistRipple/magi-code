/**
 * 文件冲突检测集成测试
 *
 * 验证 WorkerPool 层面的文件冲突检测是否正确启用
 *
 * 测试场景:
 * 1. 并行任务修改同一文件应自动串行化
 * 2. 文件冲突检测日志应正确输出
 * 3. 任务执行顺序符合依赖关系
 */

// Mock vscode 模块
const Module = require('module');
const originalRequire = Module.prototype.require;
Module.prototype.require = function(id) {
  if (id === 'vscode') {
    return {
      languages: { getDiagnostics: () => [] },
      DiagnosticSeverity: { Error: 0, Warning: 1, Information: 2, Hint: 3 },
      Uri: { file: (p) => ({ fsPath: p, path: p }), parse: (s) => ({ fsPath: s, path: s }) },
      workspace: {
        workspaceFolders: [],
        getConfiguration: () => ({ get: () => undefined, update: () => Promise.resolve() }),
        onDidChangeConfiguration: () => ({ dispose: () => {} }),
      },
      window: {
        showInformationMessage: () => Promise.resolve(),
        showWarningMessage: () => Promise.resolve(),
        showErrorMessage: () => Promise.resolve(),
        createOutputChannel: () => ({ appendLine: () => {}, show: () => {}, dispose: () => {} }),
      },
      commands: { registerCommand: () => ({ dispose: () => {} }), executeCommand: () => Promise.resolve() },
      EventEmitter: class { event = () => {}; fire() {} dispose() {} },
    };
  }
  return originalRequire.apply(this, arguments);
};

const { TaskDependencyGraph } = require('../out/orchestrator/task-dependency-graph');
const { TestRunner } = require('./test-utils');

const runner = new TestRunner('文件冲突检测集成测试');

// ============================================================================
// 测试: 文件冲突检测机制
// ============================================================================

async function testFileConflictDetection() {
  runner.logSection('文件冲突检测机制测试');

  // 测试 1: 模拟 WorkerPool 的调用模式
  try {
    const graph = new TaskDependencyGraph();

    const subTasks = [
      {
        id: 'task-1',
        description: '优化类型定义',
        targetFiles: ['src/types.ts'],
        dependencies: []
      },
      {
        id: 'task-2',
        description: '优化性能',
        targetFiles: ['src/types.ts'],
        dependencies: []
      },
      {
        id: 'task-3',
        description: '添加新功能',
        targetFiles: ['src/feature.ts'],
        dependencies: []
      }
    ];

    // 模拟 WorkerPool 的调用方式 (修复后的版本)
    for (const subTask of subTasks) {
      graph.addTask(subTask.id, subTask.description, subTask, subTask.targetFiles || []);
    }

    // 添加显式依赖
    for (const subTask of subTasks) {
      if (subTask.dependencies && subTask.dependencies.length > 0) {
        graph.addDependencies(subTask.id, subTask.dependencies);
      }
    }

    // ✅ 关键修复: 调用文件冲突检测
    const addedDeps = graph.addFileDependencies('sequential');

    runner.logTest(
      '文件冲突自动检测',
      addedDeps > 0,
      `检测到文件冲突，自动添加 ${addedDeps} 个依赖关系`
    );

    // 验证依赖关系是否正确添加
    const task2 = graph.getTask('task-2');
    const hasDependency = task2.dependencies.includes('task-1');

    runner.logTest(
      'task-2 依赖 task-1 (文件冲突串行化)',
      hasDependency,
      `task-2 的依赖: ${task2.dependencies.join(', ')}`
    );

    // 验证分析结果
    const analysis = graph.analyze();

    runner.logTest(
      '依赖图分析包含文件冲突信息',
      analysis.fileConflicts && analysis.fileConflicts.length > 0,
      `文件冲突数: ${analysis.fileConflicts?.length || 0}`
    );

    // 验证执行批次 (冲突任务应在不同批次)
    const task1Batch = analysis.executionBatches.find(b => b.taskIds.includes('task-1'));
    const task2Batch = analysis.executionBatches.find(b => b.taskIds.includes('task-2'));

    runner.logTest(
      '冲突任务在不同批次执行',
      task1Batch && task2Batch && task1Batch.batchIndex < task2Batch.batchIndex,
      `task-1 在批次 ${task1Batch?.batchIndex}, task-2 在批次 ${task2Batch?.batchIndex}`
    );

  } catch (error) {
    runner.logTest('文件冲突检测集成', false, error.message);
  }

  // 测试 2: 多个文件的复杂冲突
  try {
    const graph = new TaskDependencyGraph();

    const subTasks = [
      {
        id: 'task-A',
        description: '修改 A 和 B',
        targetFiles: ['src/a.ts', 'src/b.ts'],
        dependencies: []
      },
      {
        id: 'task-B',
        description: '修改 B 和 C',
        targetFiles: ['src/b.ts', 'src/c.ts'],
        dependencies: []
      },
      {
        id: 'task-C',
        description: '修改 C',
        targetFiles: ['src/c.ts'],
        dependencies: []
      }
    ];

    for (const subTask of subTasks) {
      graph.addTask(subTask.id, subTask.description, subTask, subTask.targetFiles || []);
    }

    const addedDeps = graph.addFileDependencies('sequential');

    runner.logTest(
      '多文件冲突检测',
      addedDeps > 0,
      `复杂场景下添加 ${addedDeps} 个依赖关系`
    );

    // 验证没有循环依赖
    const analysis = graph.analyze();

    runner.logTest(
      '文件冲突不引入循环依赖',
      !analysis.hasCycle,
      analysis.hasCycle ? `检测到循环: ${analysis.cycleNodes?.join(' -> ')}` : '无循环依赖'
    );

  } catch (error) {
    runner.logTest('多文件冲突检测', false, error.message);
  }

  // 测试 3: 无冲突场景
  try {
    const graph = new TaskDependencyGraph();

    const subTasks = [
      {
        id: 'task-X',
        description: '修改 X',
        targetFiles: ['src/x.ts'],
        dependencies: []
      },
      {
        id: 'task-Y',
        description: '修改 Y',
        targetFiles: ['src/y.ts'],
        dependencies: []
      },
      {
        id: 'task-Z',
        description: '修改 Z',
        targetFiles: ['src/z.ts'],
        dependencies: []
      }
    ];

    for (const subTask of subTasks) {
      graph.addTask(subTask.id, subTask.description, subTask, subTask.targetFiles || []);
    }

    const addedDeps = graph.addFileDependencies('sequential');

    runner.logTest(
      '无冲突场景不添加额外依赖',
      addedDeps === 0,
      `无冲突时添加依赖数: ${addedDeps}`
    );

    // 验证可以并行执行
    const analysis = graph.analyze();

    runner.logTest(
      '无冲突任务可并行执行',
      analysis.executionBatches.length === 1 && analysis.executionBatches[0].taskIds.length === 3,
      `批次数: ${analysis.executionBatches.length}, 第一批次任务数: ${analysis.executionBatches[0]?.taskIds.length}`
    );

  } catch (error) {
    runner.logTest('无冲突场景', false, error.message);
  }
}

// ============================================================================
// 运行测试
// ============================================================================

async function main() {
  try {
    await testFileConflictDetection();

    // 使用 finish() 方法而不是 printSummary()
    const exitCode = runner.finish();

    // 如果有测试失败,返回错误码
    process.exit(exitCode);

  } catch (error) {
    console.error('测试执行失败:', error);
    process.exit(1);
  }
}

main();
