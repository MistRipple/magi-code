/**
 * 验证 WorkerAgent 画像注入业务代码
 */

// Mock vscode
const Module = require('module');
const originalRequire = Module.prototype.require;
Module.prototype.require = function(id) {
  if (id === 'vscode') {
    return {
      languages: { getDiagnostics: () => [] },
      DiagnosticSeverity: { Error: 0 },
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

const { WorkerAgent } = require('../out/orchestrator/worker-agent');
const { ProfileLoader } = require('../out/orchestrator/profile');
const { CLIAdapterFactory } = require('../out/cli/adapter-factory');

async function test() {
  console.log('🔍 验证 WorkerAgent 画像注入业务代码\n');
  
  // 1. 加载画像
  const profileLoader = new ProfileLoader('.');
  await profileLoader.load();
  const claudeProfile = profileLoader.getProfile('claude');
  console.log('✅ 画像加载成功');
  console.log('   guidance.role:', claudeProfile.guidance?.role?.substring(0, 50) + '...');
  
  // 2. 创建 WorkerAgent
  const factory = new CLIAdapterFactory({ cwd: '.' });
  const worker = new WorkerAgent({
    type: 'claude',
    cliFactory: factory,
    profile: claudeProfile,
  });
  console.log('✅ WorkerAgent 创建成功');
  console.log('   profile 已设置:', !!worker.getProfile());
  
  // 3. 测试 prompt 构建 (通过子类访问 protected 方法)
  class TestWorker extends WorkerAgent {
    testBuildPrompt(subTask, context) {
      return this.buildExecutionPrompt(subTask, context);
    }
    testBuildGuidance(subTask) {
      return this.buildGuidanceHint(subTask);
    }
  }
  
  const testWorker = new TestWorker({
    type: 'claude',
    cliFactory: factory,
    profile: claudeProfile,
  });
  
  const subTask = {
    id: 'test-1',
    taskId: 'task-1',
    description: '测试任务',
    prompt: '请完成这个测试任务：分析代码结构',  // 实际执行时使用 prompt 字段
    assignedWorker: 'claude',
    targetFiles: ['test.ts'],
    kind: 'implementation',
  };

  const guidanceHint = testWorker.testBuildGuidance(subTask);
  const fullPrompt = testWorker.testBuildPrompt(subTask, '');

  console.log('\n📝 画像引导 Prompt:');
  console.log('   长度:', guidanceHint.length, '字符');
  console.log('   包含 ## 角色定位:', guidanceHint.includes('## 角色定位') ? '✅' : '❌');
  console.log('   包含 ## 专注领域:', guidanceHint.includes('## 专注领域') ? '✅' : '❌');

  console.log('\n📝 完整执行 Prompt:');
  console.log('   长度:', fullPrompt.length, '字符');
  console.log('   包含画像引导:', fullPrompt.includes('## 角色定位') ? '✅' : '❌');
  console.log('   包含任务 prompt:', fullPrompt.includes('请完成这个测试任务') ? '✅' : '❌');
  console.log('   包含目标文件:', fullPrompt.includes('test.ts') ? '✅' : '❌');
  
  // 验证结果
  console.log('\n' + '='.repeat(60));
  if (fullPrompt.includes('## 角色定位')) {
    console.log('🎉 业务代码验证通过！画像引导已正确注入到 Worker Prompt');
  } else {
    console.log('⚠️  需要检查：画像引导可能未正确注入');
    console.log('   Prompt 预览:', fullPrompt.substring(0, 300));
  }
}

test().catch(console.error);

