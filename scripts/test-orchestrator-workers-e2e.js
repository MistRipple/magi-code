/**
 * 画像系统单元测试
 * 验证画像系统的核心功能（不实际调用 CLI）
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

const path = require('path');
const { ProfileLoader, GuidanceInjector } = require('../out/orchestrator/profile');
const { TaskAnalyzer } = require('../out/task/task-analyzer');
const { WorkerSelector } = require('../out/task/worker-selector');
const { TestRunner } = require('./test-utils');

const workspaceRoot = path.resolve(__dirname, '..');

async function main() {
  const runner = new TestRunner('画像系统单元测试');

  try {
    // 1. 测试 ProfileLoader
    runner.logSection('1. ProfileLoader 测试');
    const profileLoader = new ProfileLoader(workspaceRoot);
    await profileLoader.load();

    const profiles = profileLoader.getAllProfiles();
    runner.logTest(
      '加载画像数量',
      profiles.size >= 3,
      `加载了 ${profiles.size} 个画像`
    );

    for (const [type, profile] of profiles) {
      // 画像使用 guidance.role 结构，strengths 可能为空（用户配置覆盖）
      const hasRole = !!profile.guidance?.role;
      const hasGuidance = !!profile.guidance;
      runner.logTest(
        `${type} 画像结构`,
        hasRole,
        `guidance=${hasGuidance ? '✅' : '❌'}, guidance.role=${hasRole ? '✅' : '❌'}`
      );
    }

    // 2. 测试 GuidanceInjector
    runner.logSection('2. GuidanceInjector 测试');
    const claudeProfile = profileLoader.getProfile('claude');
    if (claudeProfile) {
      runner.log(`Claude 画像结构: ${JSON.stringify(Object.keys(claudeProfile))}`, 'blue');

      // 检查画像是否有 guidance 结构
      if (claudeProfile.guidance) {
        const injector = new GuidanceInjector();
        const prompt = injector.buildWorkerPrompt(claudeProfile, { collaborators: [] });

        runner.log(`Prompt 长度: ${prompt.length} 字符`, 'blue');
        runner.logTest(
          '角色定位存在',
          prompt.includes('## 角色定位')
        );
        runner.logTest(
          '专注领域存在',
          prompt.includes('## 专注领域') || claudeProfile.guidance.focus?.length === 0
        );
      } else {
        runner.log('画像使用简化结构（无 guidance 字段）', 'yellow');
        // 简化结构验证
        const hasRole = !!claudeProfile.role;
        const hasStrengths = claudeProfile.strengths?.length > 0;
        runner.logTest('role 字段存在', hasRole);
        runner.logTest('strengths 字段存在', hasStrengths);
      }
    } else {
      runner.logTest('获取 Claude 画像', false, '无法获取 Claude 画像');
      runner.logTest('GuidanceInjector 功能', false, '前置条件失败');
    }

    // 3. 测试 TaskAnalyzer
    runner.logSection('3. TaskAnalyzer 测试');
    const taskAnalyzer = new TaskAnalyzer();
    taskAnalyzer.setProfileLoader(profileLoader);

    const tasks = [
      ['分析 src/orchestrator 目录的代码结构', ['architecture', 'review', 'general', 'debug', 'simple']],
      ['创建一个新的 TypeScript 工具函数', ['implement', 'general', 'simple']],
    ];
    for (const [task, expectedCategories] of tasks) {
      const analysis = taskAnalyzer.analyze(task);
      const ok = expectedCategories.includes(analysis.category);
      runner.logTest(
        `任务分析: "${task.substring(0, 30)}..."`,
        ok,
        `类型: ${analysis.category}, 复杂度: ${analysis.complexity}`
      );
    }

    // 4. 测试 WorkerSelector
    runner.logSection('4. WorkerSelector 测试');
    const workerSelector = new WorkerSelector();
    workerSelector.setProfileLoader(profileLoader);
    workerSelector.setAvailableWorkers(['claude', 'codex', 'gemini']);

    // 测试选择
    const testAnalysis = { category: 'implement', complexity: 2 };
    const selection = workerSelector.select(testAnalysis);
    const ok = !!selection?.worker;
    runner.logTest(
      'Worker 选择 (implement 任务)',
      ok,
      `选择: ${selection?.worker || '无'}, 原因: ${selection?.reason || '无'}`
    );

    // 使用 TestRunner 的统一输出
    process.exit(runner.finish());

  } catch (error) {
    runner.log(`\n❌ 测试失败: ${error.message}`, 'red');
    console.error(error);
    process.exit(1);
  }
}

main().catch(console.error);
