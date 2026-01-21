/**
 * 简化版端到端架构验证测试
 *
 * 专注于测试核心重构模块，跳过需要 VSCode 环境的部分
 */

const path = require('path');
const fs = require('fs');

// 测试配置
const TEST_CONFIG = {
  workspaceRoot: path.join(__dirname, '..', '.tmp', 'e2e-test-simple'),
  timeout: 30000,
};

// 颜色输出
const colors = {
  reset: '\x1b[0m',
  bright: '\x1b[1m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  cyan: '\x1b[36m',
};

function log(message, color = colors.reset) {
  console.log(`${color}${message}${colors.reset}`);
}

function logSection(title) {
  console.log('\n' + '='.repeat(80));
  log(`  ${title}`, colors.cyan + colors.bright);
  console.log('='.repeat(80) + '\n');
}

function logTest(name, passed, details = '') {
  const status = passed ? '✅' : '❌';
  const detailsStr = details ? ` - ${details}` : '';
  log(`${status} ${name}${detailsStr}`, passed ? colors.green : colors.red);
}

// 测试统计
const stats = {
  total: 0,
  passed: 0,
  failed: 0,
  startTime: Date.now(),
};

function recordTest(passed) {
  stats.total++;
  if (passed) {
    stats.passed++;
  } else {
    stats.failed++;
  }
}

// 准备测试环境
function setupTestEnvironment() {
  logSection('准备测试环境');

  if (!fs.existsSync(TEST_CONFIG.workspaceRoot)) {
    fs.mkdirSync(TEST_CONFIG.workspaceRoot, { recursive: true });
  }

  // 创建测试文件
  const testFiles = [
    'src/index.ts',
    'src/utils/helper.ts',
  ];

  testFiles.forEach(file => {
    const filePath = path.join(TEST_CONFIG.workspaceRoot, file);
    const dir = path.dirname(filePath);
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
    }
    fs.writeFileSync(filePath, `// Test file: ${file}\nexport const test = 'hello';\n`);
  });

  log('✅ 测试环境准备完成', colors.green);
}

// 清理测试环境
function cleanupTestEnvironment() {
  logSection('清理测试环境');

  if (fs.existsSync(TEST_CONFIG.workspaceRoot)) {
    fs.rmSync(TEST_CONFIG.workspaceRoot, { recursive: true, force: true });
    log('✅ 测试环境清理完成', colors.green);
  }
}

// 测试 1: 核心模块导入
async function testCoreModuleImports() {
  logSection('测试 1: 核心模块导入验证');

  const modules = [
    { name: 'IDGenerator', path: '../out/utils/id-generator' },
    { name: 'TokenCounter', path: '../out/utils/token-counter' },
    { name: 'FileLock', path: '../out/utils/file-lock' },
    { name: 'SnapshotCache', path: '../out/snapshot/snapshot-cache' },
    { name: 'SnapshotValidator', path: '../out/snapshot/snapshot-validator' },
    { name: 'SnapshotCleaner', path: '../out/snapshot/snapshot-cleaner' },
    { name: 'AtomicOperations', path: '../out/snapshot/atomic-operations' },
    { name: 'ConfigResolver', path: '../out/orchestrator/config-resolver' },
    { name: 'TaskContextManager', path: '../out/orchestrator/task-context-manager' },
    { name: 'InteractionModeManager', path: '../out/orchestrator/interaction-mode-manager' },
  ];

  for (const module of modules) {
    try {
      const startTime = Date.now();
      const loaded = require(module.path);
      const duration = Date.now() - startTime;

      const hasExports = Object.keys(loaded).length > 0;
      logTest(`导入 ${module.name}`, hasExports, `${duration}ms, ${Object.keys(loaded).length} exports`);
      recordTest(hasExports);
    } catch (error) {
      logTest(`导入 ${module.name}`, false, error.message);
      recordTest(false);
    }
  }
}

// 测试 2: IDGenerator 功能
async function testIDGenerator() {
  logSection('测试 2: IDGenerator 功能验证');

  try {
    const { IDGenerator } = require('../out/utils/id-generator');

    // 测试生成唯一 ID
    const id1 = IDGenerator.generate('test');
    const id2 = IDGenerator.generate('test');
    const unique = id1 !== id2;
    logTest('生成唯一 ID', unique, `${id1} !== ${id2}`);
    recordTest(unique);

    // 测试前缀
    const hasPrefix = id1.startsWith('test_');
    logTest('ID 包含正确前缀', hasPrefix, id1);
    recordTest(hasPrefix);

    // 测试性能
    const count = 1000;
    const start = Date.now();
    for (let i = 0; i < count; i++) {
      IDGenerator.generate('perf');
    }
    const duration = Date.now() - start;
    const avgDuration = duration / count;

    const performanceOk = avgDuration < 1;
    logTest('ID 生成性能', performanceOk, `${count} 次耗时 ${duration}ms (平均 ${avgDuration.toFixed(3)}ms/次)`);
    recordTest(performanceOk);

  } catch (error) {
    logTest('IDGenerator 测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 3: SnapshotCache 功能
async function testSnapshotCache() {
  logSection('测试 3: SnapshotCache 功能验证');

  try {
    const { SnapshotCache } = require('../out/snapshot/snapshot-cache');
    const cache = new SnapshotCache(10);

    // 测试文件缓存
    const testFile = path.join(TEST_CONFIG.workspaceRoot, 'src/index.ts');
    const content1 = cache.readFileWithCache(testFile);
    const content2 = cache.readFileWithCache(testFile);

    const cacheWorks = content1 === content2 && content1.includes('Test file');
    logTest('文件缓存功能', cacheWorks, `内容长度: ${content1.length}`);
    recordTest(cacheWorks);

    // 测试缓存统计
    const stats = cache.getStats();
    const statsValid = stats.fileCache.size === 1 && stats.fileCache.maxSize === 10;
    logTest('缓存统计信息', statsValid, `size: ${stats.fileCache.size}, max: ${stats.fileCache.maxSize}`);
    recordTest(statsValid);

    // 测试缓存失效
    cache.invalidateFileCache(testFile);
    const statsAfter = cache.getStats();
    const invalidateWorks = statsAfter.fileCache.size === 0;
    logTest('缓存失效功能', invalidateWorks, `失效后 size: ${statsAfter.fileCache.size}`);
    recordTest(invalidateWorks);

    // 测试 LRU 策略
    cache.clearAll();
    for (let i = 0; i < 12; i++) {
      cache.addToCache(cache['fileContentCache'], `file-${i}`, `content-${i}`);
    }
    const lruWorks = cache.getStats().fileCache.size === 10;
    logTest('LRU 缓存淘汰', lruWorks, `添加 12 个，保留 ${cache.getStats().fileCache.size} 个`);
    recordTest(lruWorks);

  } catch (error) {
    logTest('SnapshotCache 测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 4: SnapshotValidator 功能
async function testSnapshotValidator() {
  logSection('测试 4: SnapshotValidator 功能验证');

  try {
    const { SnapshotValidator } = require('../out/snapshot/snapshot-validator');
    const { UnifiedSessionManager } = require('../out/session');

    const sessionManager = new UnifiedSessionManager(TEST_CONFIG.workspaceRoot);
    const validator = new SnapshotValidator(sessionManager, TEST_CONFIG.workspaceRoot);

    // 测试有效快照
    const validSnapshot = {
      id: 'snap-123',
      sessionId: 'session-456',
      filePath: 'src/index.ts',
      originalContent: 'test content',
      lastModifiedAt: Date.now(),
      lastModifiedBy: 'claude',
      subTaskId: 'task-789',
    };

    const result1 = validator.validateSnapshot(validSnapshot);
    logTest('验证有效快照', result1.valid, `errors: ${result1.errors.length}, warnings: ${result1.warnings.length}`);
    recordTest(result1.valid);

    // 测试无效快照
    const invalidSnapshot = {
      id: '',
      sessionId: '',
      filePath: '',
      originalContent: '',
      lastModifiedAt: 0,
      lastModifiedBy: '',
      subTaskId: '',
    };

    const result2 = validator.validateSnapshot(invalidSnapshot);
    logTest('检测无效快照', !result2.valid && result2.errors.length > 0, `检测到 ${result2.errors.length} 个错误`);
    recordTest(!result2.valid && result2.errors.length > 0);

  } catch (error) {
    logTest('SnapshotValidator 测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 5: ConfigResolver 功能
async function testConfigResolver() {
  logSection('测试 5: ConfigResolver 功能验证');

  try {
    const { ConfigResolver } = require('../out/orchestrator/config-resolver');

    // 测试配置解析
    const config = ConfigResolver.resolveConfig({
      timeout: 60000,
      maxRetries: 5,
    });

    const configOk = config.timeout === 60000 && config.maxRetries === 5;
    logTest('配置解析', configOk, `timeout: ${config.timeout}, maxRetries: ${config.maxRetries}`);
    recordTest(configOk);

    // 测试默认配置
    const defaultConfig = ConfigResolver.getDefaultConfig();
    const hasDefaults = defaultConfig.timeout > 0 && defaultConfig.maxRetries > 0;
    logTest('默认配置', hasDefaults, `timeout: ${defaultConfig.timeout}, maxRetries: ${defaultConfig.maxRetries}`);
    recordTest(hasDefaults);

    // 测试权限解析
    const permissions = ConfigResolver.resolvePermissions(config);
    const permsOk = typeof permissions.allowEdit === 'boolean';
    logTest('权限解析', permsOk, `allowEdit: ${permissions.allowEdit}, allowBash: ${permissions.allowBash}`);
    recordTest(permsOk);

    // 测试策略解析
    const strategy = ConfigResolver.resolveStrategyConfig(config);
    const strategyOk = typeof strategy.enableVerification === 'boolean';
    logTest('策略解析', strategyOk, `enableVerification: ${strategy.enableVerification}`);
    recordTest(strategyOk);

  } catch (error) {
    logTest('ConfigResolver 测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 6: InteractionModeManager 功能
async function testInteractionModeManager() {
  logSection('测试 6: InteractionModeManager 功能验证');

  try {
    const { InteractionModeManager } = require('../out/orchestrator/interaction-mode-manager');

    const strategyConfig = {
      enableVerification: true,
      enableRecovery: true,
      autoRollbackOnFailure: false,
    };

    const manager = new InteractionModeManager(strategyConfig);

    // 测试模式设置
    manager.setInteractionMode('agent');
    const mode = manager.getInteractionMode();
    logTest('模式设置', mode === 'agent', `当前模式: ${mode}`);
    recordTest(mode === 'agent');

    // 测试 ask 模式判断
    const testCases = [
      { prompt: '什么是 TypeScript?', expected: true, desc: '问题' },
      { prompt: '实现用户登录功能', expected: false, desc: '任务' },
      { prompt: '如何优化性能?', expected: true, desc: '咨询' },
      { prompt: '创建一个新组件', expected: false, desc: '创建任务' },
    ];

    let judgeCorrect = 0;
    testCases.forEach(({ prompt, expected, desc }) => {
      manager.setInteractionMode('auto'); // 重置为 auto 模式
      const result = manager.shouldUseAskMode(prompt);
      if (result === expected) {
        judgeCorrect++;
      }
      log(`  ${result === expected ? '✓' : '✗'} "${desc}": ${result} (期望: ${expected})`,
          result === expected ? colors.green : colors.yellow);
    });

    const judgeOk = judgeCorrect >= 3; // 至少 3/4 正确
    logTest('模式判断准确性', judgeOk, `${judgeCorrect}/${testCases.length} 正确`);
    recordTest(judgeOk);

  } catch (error) {
    logTest('InteractionModeManager 测试失败', false, error.message);
    recordTest(false);
  }
}

// 打印测试报告
function printTestReport() {
  const totalDuration = Date.now() - stats.startTime;

  logSection('测试报告');

  log('功能测试:', colors.bright);
  log(`  总计: ${stats.total}`, colors.blue);
  log(`  通过: ${stats.passed}`, colors.green);
  log(`  失败: ${stats.failed}`, stats.failed > 0 ? colors.red : colors.green);
  log(`  成功率: ${((stats.passed / stats.total) * 100).toFixed(1)}%`,
      stats.failed === 0 ? colors.green : colors.yellow);

  log(`\n总耗时: ${(totalDuration / 1000).toFixed(2)}s`, colors.blue);

  // 最终结论
  console.log('\n' + '='.repeat(80));
  if (stats.failed === 0) {
    log('✅ 所有测试通过！架构验证成功！', colors.green + colors.bright);
  } else if (stats.passed / stats.total >= 0.8) {
    log('⚠️  大部分测试通过，少数测试失败', colors.yellow + colors.bright);
  } else {
    log('❌ 多个测试失败，需要修复', colors.red + colors.bright);
  }
  console.log('='.repeat(80) + '\n');

  return stats.failed === 0;
}

// 主测试流程
async function runTests() {
  log('\n╔══════════════════════════════════════════════════════════════════════════════╗', colors.cyan);
  log('║           MultiCLI 核心架构验证测试 (简化版)                                  ║', colors.cyan);
  log('╚══════════════════════════════════════════════════════════════════════════════╝\n', colors.cyan);

  try {
    setupTestEnvironment();

    await testCoreModuleImports();
    await testIDGenerator();
    await testSnapshotCache();
    await testSnapshotValidator();
    await testConfigResolver();
    await testInteractionModeManager();

    const success = printTestReport();

    cleanupTestEnvironment();

    process.exit(success ? 0 : 1);

  } catch (error) {
    log('\n❌ 测试执行失败:', colors.red);
    log(error.stack, colors.red);
    cleanupTestEnvironment();
    process.exit(1);
  }
}

// 运行测试
runTests();
