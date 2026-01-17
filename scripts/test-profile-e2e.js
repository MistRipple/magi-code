/**
 * 画像系统端到端测试
 *
 * 验证画像系统是否真正影响了 CLI 调用：
 * 1. 直接测试 WorkerAgent 的画像注入
 * 2. 验证不同 Worker 类型收到不同的引导 Prompt
 * 3. 验证 CLISelector 的任务分配逻辑
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

const { CLIAdapterFactory } = require('../out/cli/adapter-factory');
const { WorkerAgent } = require('../out/orchestrator/worker-agent');
const { ProfileLoader, GuidanceInjector } = require('../out/orchestrator/profile');
const { CLISelector } = require('../out/task/cli-selector');
const { globalMessageBus } = require('../out/orchestrator/message-bus');

const workspaceRoot = process.cwd();

// 颜色输出
const colors = {
  reset: '\x1b[0m',
  green: '\x1b[32m',
  red: '\x1b[31m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  cyan: '\x1b[36m',
  magenta: '\x1b[35m',
};

function log(msg, color = 'reset') {
  console.log(`${colors[color]}${msg}${colors.reset}`);
}

function logSection(title) {
  console.log('\n' + '='.repeat(70));
  log(`  ${title}`, 'cyan');
  console.log('='.repeat(70));
}

// 记录 CLI 调用
const cliCallLog = [];

/**
 * 创建带拦截的 CLI 适配器工厂
 * 拦截适配器层的 sendMessage 方法
 */
function createInterceptedFactory() {
  const factory = new CLIAdapterFactory({ cwd: workspaceRoot });

  // 保存原始 getOrCreate 方法
  const originalGetOrCreate = factory.getOrCreate.bind(factory);

  // 拦截 getOrCreate 方法，对返回的适配器进行包装
  factory.getOrCreate = function(type) {
    const adapter = originalGetOrCreate(type);

    // 如果已经拦截过，直接返回
    if (adapter._intercepted) {
      return adapter;
    }

    // 保存原始 sendMessage 方法
    const originalSendMessage = adapter.sendMessage.bind(adapter);

    // 拦截 sendMessage 方法
    adapter.sendMessage = async function(message, imagePaths, meta) {
      // 记录调用信息
      const callInfo = {
        timestamp: new Date().toISOString(),
        workerType: type,
        messageLength: message.length,
        messagePreview: message.substring(0, 1000),
        hasGuidancePrompt: message.includes('## 角色定位'),
        hasRoleSection: message.includes('角色定位'),
        hasFocusSection: message.includes('专注领域'),
        hasNotesSection: message.includes('注意事项'),
        hasCurrentTask: message.includes('## 当前任务'),
      };

      cliCallLog.push(callInfo);

      log(`\n📡 [CLI 调用拦截 - 适配器层]`, 'magenta');
      log(`   Worker: ${type}`, 'yellow');
      log(`   消息长度: ${message.length} 字符`, 'blue');
      log(`   包含引导 Prompt: ${callInfo.hasGuidancePrompt ? '✅ 是' : '❌ 否'}`, callInfo.hasGuidancePrompt ? 'green' : 'red');

      if (callInfo.hasGuidancePrompt) {
        // 提取角色定位部分
        const roleMatch = message.match(/## 角色定位\n([^\n]+)/);
        if (roleMatch) {
          log(`   角色: ${roleMatch[1].substring(0, 60)}...`, 'blue');
        }
      }

      // 显示消息预览
      log(`   消息预览:`, 'blue');
      const lines = message.split('\n').slice(0, 8);
      lines.forEach(line => log(`     ${line.substring(0, 70)}`, 'blue'));
      if (message.split('\n').length > 8) {
        log(`     ... (共 ${message.split('\n').length} 行)`, 'blue');
      }

      // 调用原始方法
      return originalSendMessage(message, imagePaths, meta);
    };

    adapter._intercepted = true;
    return adapter;
  };

  return factory;
}

/**
 * 端到端测试用例 - 验证画像注入和 CLI 响应
 *
 * 每个测试用例包含：
 * - 任务描述和分类
 * - 期望的角色定位
 * - 期望 CLI 响应中体现的关键词（验证画像是否影响了 CLI 行为）
 */
const E2E_TEST_CASES = [
  {
    name: 'Claude Worker (架构设计)',
    workerType: 'claude',
    subTask: {
      id: 'test-1',
      description: '请简要说明你作为什么角色来处理这个任务，以及你会关注哪些方面？',
      prompt: '请简要说明你作为什么角色来处理这个任务，以及你会关注哪些方面？只需要 3-5 句话。',
      kind: 'architecture',
      targetFiles: [],
      status: 'pending',
    },
    expectedRole: '资深软件架构师',
    // 期望 CLI 响应中体现的关键词（验证画像影响）
    expectedResponseKeywords: ['架构', '设计', '可维护', '扩展', '质量'],
  },
  {
    name: 'Codex Worker (Bug 修复)',
    workerType: 'codex',
    subTask: {
      id: 'test-2',
      description: '请简要说明你作为什么角色来处理这个任务，以及你会关注哪些方面？',
      prompt: '请简要说明你作为什么角色来处理这个任务，以及你会关注哪些方面？只需要 3-5 句话。',
      kind: 'bugfix',
      targetFiles: [],
      status: 'pending',
    },
    expectedRole: '高效的代码执行者',
    expectedResponseKeywords: ['快速', '精准', '修复', '最小', '问题'],
  },
  {
    name: 'Gemini Worker (前端开发)',
    workerType: 'gemini',
    subTask: {
      id: 'test-3',
      description: '请简要说明你作为什么角色来处理这个任务，以及你会关注哪些方面？',
      prompt: '请简要说明你作为什么角色来处理这个任务，以及你会关注哪些方面？只需要 3-5 句话。',
      kind: 'frontend',
      targetFiles: [],
      status: 'pending',
    },
    expectedRole: '前端专家和文档专家',
    expectedResponseKeywords: ['前端', '用户', '界面', 'UI', '体验'],
  },
];

/**
 * 运行单个 Worker 画像注入测试
 * 验证：1. 画像是否注入 2. CLI 响应是否体现画像影响
 */
async function runWorkerProfileTest(factory, profileLoader, testCase, testIndex) {
  log(`\n📋 测试 ${testIndex + 1}: ${testCase.name}`, 'cyan');
  log(`   任务: ${testCase.subTask.description.substring(0, 40)}...`, 'blue');
  log(`   分类: ${testCase.subTask.kind}`, 'blue');
  log(`   期望角色: ${testCase.expectedRole}`, 'blue');

  // 清空调用日志
  cliCallLog.length = 0;

  // 获取 Worker 画像
  const profile = profileLoader.getProfile(testCase.workerType);
  if (!profile) {
    log(`   ❌ 未找到 ${testCase.workerType} 的画像`, 'red');
    return { success: false, reason: '画像未找到' };
  }

  const roleText = profile.guidance?.role || '未定义';
  log(`   画像: ${profile.displayName} (${roleText.substring(0, 40)}...)`, 'green');

  // 创建 WorkerAgent
  const worker = new WorkerAgent({
    type: testCase.workerType,
    cliFactory: factory,
    messageBus: globalMessageBus,
    orchestratorId: 'test-orchestrator',
    profile: profile,
  });

  // 连接 CLI
  await factory.create(testCase.workerType).connect();

  try {
    // 执行子任务
    log(`\n   🚀 执行子任务...`, 'yellow');
    const result = await worker.executeTask('test-task-' + testIndex, testCase.subTask);

    // 调试：显示完整的 result 对象
    log(`   [DEBUG] result.success: ${result.success}`, 'magenta');
    log(`   [DEBUG] result.result 长度: ${(result.result || '').length}`, 'magenta');
    log(`   [DEBUG] result.error: ${result.error || '无'}`, 'magenta');

    // 检查调用日志
    if (cliCallLog.length === 0) {
      log(`   ❌ 没有捕获到 CLI 调用`, 'red');
      return { success: false, reason: '没有 CLI 调用' };
    }

    const lastCall = cliCallLog[cliCallLog.length - 1];
    const workerMatch = lastCall.workerType === testCase.workerType;
    const hasGuidance = lastCall.hasGuidancePrompt;
    const hasExpectedRole = lastCall.messagePreview.includes(testCase.expectedRole);

    // 🆕 验证 CLI 响应是否体现画像影响
    const cliResponse = result.result || '';
    const responseKeywordsMatched = testCase.expectedResponseKeywords.filter(
      keyword => cliResponse.includes(keyword)
    );

    // 响应验证：至少匹配 2 个关键词
    const responseReflectsProfile = responseKeywordsMatched.length >= 2;

    log(`\n   📊 调用结果:`, 'yellow');
    log(`   Worker 类型: ${lastCall.workerType} ${workerMatch ? '✅' : '❌'}`, workerMatch ? 'green' : 'red');
    log(`   引导 Prompt: ${hasGuidance ? '✅ 已注入' : '❌ 未注入'}`, hasGuidance ? 'green' : 'red');
    log(`   角色匹配: ${hasExpectedRole ? '✅' : '❌'} (期望: ${testCase.expectedRole})`, hasExpectedRole ? 'green' : 'red');

    if (hasGuidance) {
      log(`   - 角色定位: ${lastCall.hasRoleSection ? '✅' : '❌'}`, lastCall.hasRoleSection ? 'green' : 'red');
      log(`   - 专注领域: ${lastCall.hasFocusSection ? '✅' : '❌'}`, lastCall.hasFocusSection ? 'green' : 'red');
    }

    // 🆕 显示 CLI 响应验证结果
    log(`\n   📝 CLI 响应验证:`, 'yellow');
    log(`   响应长度: ${cliResponse.length} 字符`, 'blue');
    log(`   期望关键词: ${testCase.expectedResponseKeywords.join(', ')}`, 'blue');
    log(`   匹配关键词: ${responseKeywordsMatched.length > 0 ? responseKeywordsMatched.join(', ') : '无'}`,
        responseKeywordsMatched.length >= 2 ? 'green' : 'red');
    log(`   响应体现画像: ${responseReflectsProfile ? '✅ 是' : '❌ 否'}`, responseReflectsProfile ? 'green' : 'red');

    // 显示响应预览
    if (cliResponse) {
      log(`\n   响应预览:`, 'blue');
      const lines = cliResponse.split('\n').slice(0, 6);
      lines.forEach(line => log(`     ${line.substring(0, 70)}`, 'blue'));
      if (cliResponse.split('\n').length > 6) {
        log(`     ... (共 ${cliResponse.split('\n').length} 行)`, 'blue');
      }
    }

    return {
      // 画像系统验证成功条件：Worker 匹配 + 引导注入 + 角色匹配 + 响应体现画像
      success: workerMatch && hasGuidance && hasExpectedRole && responseReflectsProfile,
      workerMatch,
      hasGuidance,
      hasExpectedRole,
      responseReflectsProfile,
      responseKeywordsMatched,
      actualWorker: lastCall.workerType,
      cliResponse: cliResponse.substring(0, 200),
    };

  } catch (error) {
    log(`   ❌ 执行失败: ${error.message}`, 'red');
    return { success: false, reason: error.message };
  }
}

/**
 * 运行单个端到端测试
 */
async function runE2ETest(orchestrator, testCase, testIndex) {
  log(`\n📋 测试 ${testIndex + 1}: ${testCase.name}`, 'cyan');
  log(`   Prompt: "${testCase.prompt.substring(0, 50)}..."`, 'blue');
  log(`   期望 Worker: ${testCase.expectedWorker}`, 'blue');

  // 清空调用日志
  cliCallLog.length = 0;

  try {
    // 执行任务
    const result = await orchestrator.execute(testCase.prompt);

    // 检查调用日志
    if (cliCallLog.length === 0) {
      log(`   ❌ 没有捕获到 CLI 调用`, 'red');
      return { success: false, reason: '没有 CLI 调用' };
    }

    const lastCall = cliCallLog[cliCallLog.length - 1];
    const workerMatch = lastCall.workerType === testCase.expectedWorker;
    const hasGuidance = lastCall.hasGuidancePrompt;

    log(`\n   📊 调用结果:`, 'yellow');
    log(`   实际 Worker: ${lastCall.workerType} ${workerMatch ? '✅' : '❌'}`, workerMatch ? 'green' : 'red');
    log(`   引导 Prompt: ${hasGuidance ? '✅ 已注入' : '❌ 未注入'}`, hasGuidance ? 'green' : 'red');

    if (hasGuidance) {
      log(`   - 角色定位: ${lastCall.hasRoleSection ? '✅' : '❌'}`, lastCall.hasRoleSection ? 'green' : 'red');
      log(`   - 专注领域: ${lastCall.hasFocusSection ? '✅' : '❌'}`, lastCall.hasFocusSection ? 'green' : 'red');
      log(`   - 注意事项: ${lastCall.hasNotesSection ? '✅' : '❌'}`, lastCall.hasNotesSection ? 'green' : 'red');
    }

    // 显示响应预览
    if (result) {
      const content = typeof result === 'string' ? result : result.content;
      if (content) {
        log(`   响应预览: ${content.substring(0, 100)}...`, 'blue');
      }
    }

    return {
      success: workerMatch && hasGuidance,
      workerMatch,
      hasGuidance,
      actualWorker: lastCall.workerType,
    };

  } catch (error) {
    log(`   ❌ 执行失败: ${error.message}`, 'red');
    return { success: false, reason: error.message };
  }
}

/**
 * 主测试函数
 */
async function runE2ETests() {
  log('\n🚀 画像系统端到端测试 (WorkerAgent 直接测试)\n', 'cyan');
  log('本测试将验证画像系统是否真正影响了 CLI 调用：', 'yellow');
  log('1. 直接创建 WorkerAgent 并设置画像', 'yellow');
  log('2. 执行子任务，验证引导 Prompt 是否被注入', 'yellow');
  log('3. 验证不同 Worker 收到不同的角色定位\n', 'yellow');

  const startTime = Date.now();

  // 初始化
  logSection('1. 初始化测试环境');

  // 加载画像
  const profileLoader = new ProfileLoader(workspaceRoot);
  await profileLoader.load();
  log('✅ 已加载 Worker 画像', 'green');

  // 显示画像信息
  const profiles = profileLoader.getAllProfiles();
  for (const [type, profile] of profiles) {
    const rolePreview = profile.guidance?.role ? profile.guidance.role.substring(0, 30) : '未定义';
    log(`   - ${type}: ${profile.displayName} (${rolePreview}...)`, 'blue');
  }

  // 创建带拦截的工厂
  const factory = createInterceptedFactory();
  log('✅ 已创建带拦截的 CLI 适配器工厂', 'green');

  // 运行测试
  logSection('2. 执行 WorkerAgent 画像注入测试');

  const results = [];
  for (let i = 0; i < E2E_TEST_CASES.length; i++) {
    const result = await runWorkerProfileTest(factory, profileLoader, E2E_TEST_CASES[i], i);
    results.push({ ...E2E_TEST_CASES[i], ...result });

    // 等待一下，避免 CLI 调用过快
    await new Promise(resolve => setTimeout(resolve, 1000));
  }

  // 汇总结果
  logSection('3. 测试结果汇总');

  const passed = results.filter(r => r.success).length;
  const failed = results.length - passed;

  console.log('\n┌───────────────────────────┬──────────┬──────────┬──────────┬──────────┬──────────┐');
  console.log('│ 测试用例                  │ Worker   │ 引导注入 │ 角色匹配 │ 响应验证 │ 结果     │');
  console.log('├───────────────────────────┼──────────┼──────────┼──────────┼──────────┼──────────┤');

  for (const r of results) {
    const name = r.name.padEnd(23);
    const worker = r.workerMatch ? '✅' : '❌';
    const guidance = r.hasGuidance ? '✅' : '❌';
    const role = r.hasExpectedRole ? '✅' : '❌';
    const response = r.responseReflectsProfile ? '✅' : '❌';
    const status = r.success ? '✅ 通过' : '❌ 失败';
    console.log(`│ ${name} │ ${worker.padEnd(6)}   │ ${guidance.padEnd(6)}   │ ${role.padEnd(6)}   │ ${response.padEnd(6)}   │ ${status.padEnd(6)} │`);
  }

  console.log('└───────────────────────────┴──────────┴──────────┴──────────┴──────────┴──────────┘');

  const duration = ((Date.now() - startTime) / 1000).toFixed(2);

  log(`\n✅ 通过: ${passed}/${results.length}`, passed === results.length ? 'green' : 'yellow');
  log(`❌ 失败: ${failed}/${results.length}`, failed > 0 ? 'red' : 'green');
  log(`⏱️  耗时: ${duration}s`, 'blue');

  // 详细分析
  if (failed > 0) {
    logSection('失败分析');
    for (const r of results.filter(r => !r.success)) {
      log(`❌ ${r.name}:`, 'red');
      if (!r.workerMatch) {
        log(`   Worker 不匹配: 期望 ${r.workerType}, 实际 ${r.actualWorker}`, 'red');
      }
      if (!r.hasGuidance) {
        log(`   引导 Prompt 未注入`, 'red');
      }
      if (!r.hasExpectedRole) {
        log(`   角色不匹配: 期望包含 "${r.expectedRole}"`, 'red');
      }
      if (!r.responseReflectsProfile) {
        log(`   CLI 响应未体现画像影响`, 'red');
        log(`   期望关键词: ${r.expectedResponseKeywords?.join(', ')}`, 'red');
        log(`   匹配关键词: ${r.responseKeywordsMatched?.join(', ') || '无'}`, 'red');
      }
      if (r.reason) {
        log(`   原因: ${r.reason}`, 'red');
      }
    }
  }

  // 🆕 显示画像影响验证总结
  logSection('画像影响验证总结');

  const responsePassedCount = results.filter(r => r.responseReflectsProfile).length;

  log('验证项目:', 'cyan');
  log(`  1. ${results.every(r => r.workerMatch) ? '✅' : '❌'} 画像是否正确加载`, results.every(r => r.workerMatch) ? 'green' : 'red');
  log(`  2. ${results.every(r => r.hasGuidance) ? '✅' : '❌'} 引导 Prompt 是否注入到 CLI 调用`, results.every(r => r.hasGuidance) ? 'green' : 'red');
  log(`  3. ${results.every(r => r.hasExpectedRole) ? '✅' : '❌'} 不同 Worker 是否收到不同的角色定位`, results.every(r => r.hasExpectedRole) ? 'green' : 'red');
  log(`  4. ${responsePassedCount === results.length ? '✅' : '❌'} CLI 响应是否体现画像影响 (${responsePassedCount}/${results.length} 通过)`,
      responsePassedCount === results.length ? 'green' : 'red');

  log('\n结论:', 'cyan');
  if (passed === results.length) {
    log('  🎉 画像系统端到端验证通过！', 'green');
    log('  - 画像引导 Prompt 已成功注入到真实 CLI 调用', 'green');
    log('  - CLI 响应体现了画像定义的角色和关注点', 'green');
  } else {
    log('  ⚠️ 部分测试未通过，请检查失败分析', 'yellow');
  }

  process.exit(failed > 0 ? 1 : 0);
}

// 运行测试
runE2ETests().catch(console.error);

