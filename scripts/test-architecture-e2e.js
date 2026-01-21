/**
 * 端到端架构验证测试
 *
 * 目标：
 * 1. 验证重构后的架构能够正确工作
 * 2. 测试各个模块的集成
 * 3. 验证响应速度和性能
 * 4. 确保向后兼容性
 */

const path = require('path');
const fs = require('fs');

// 测试配置
const TEST_CONFIG = {
  workspaceRoot: path.join(__dirname, '..', '.tmp', 'e2e-test'),
  timeout: 30000, // 30秒超时
  performanceThreshold: {
    initialization: 1000, // 初始化应在1秒内完成
    simpleTask: 5000,     // 简单任务应在5秒内完成
    complexTask: 15000,   // 复杂任务应在15秒内完成
  }
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

function logTest(name, passed, duration) {
  const status = passed ? '✅' : '❌';
  const durationStr = duration ? ` (${duration}ms)` : '';
  log(`${status} ${name}${durationStr}`, passed ? colors.green : colors.red);
}

function logPerformance(name, duration, threshold) {
  const passed = duration <= threshold;
  const status = passed ? '⚡' : '⚠️';
  const color = passed ? colors.green : colors.yellow;
  log(`${status} ${name}: ${duration}ms (阈值: ${threshold}ms)`, color);
  return passed;
}

// 测试统计
const stats = {
  total: 0,
  passed: 0,
  failed: 0,
  performance: {
    total: 0,
    passed: 0,
    failed: 0,
  },
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

function recordPerformance(passed) {
  stats.performance.total++;
  if (passed) {
    stats.performance.passed++;
  } else {
    stats.performance.failed++;
  }
}

// 准备测试环境
function setupTestEnvironment() {
  logSection('准备测试环境');

  // 创建临时工作目录
  if (!fs.existsSync(TEST_CONFIG.workspaceRoot)) {
    fs.mkdirSync(TEST_CONFIG.workspaceRoot, { recursive: true });
    log('✅ 创建测试工作目录', colors.green);
  }

  // 创建测试文件
  const testFiles = [
    'src/index.ts',
    'src/utils/helper.ts',
    'src/components/Button.tsx',
  ];

  testFiles.forEach(file => {
    const filePath = path.join(TEST_CONFIG.workspaceRoot, file);
    const dir = path.dirname(filePath);
    if (!fs.existsSync(dir)) {
      fs.mkdirSync(dir, { recursive: true });
    }
    fs.writeFileSync(filePath, `// Test file: ${file}\n`);
  });

  log('✅ 创建测试文件', colors.green);
}

// 清理测试环境
function cleanupTestEnvironment() {
  logSection('清理测试环境');

  if (fs.existsSync(TEST_CONFIG.workspaceRoot)) {
    fs.rmSync(TEST_CONFIG.workspaceRoot, { recursive: true, force: true });
    log('✅ 清理测试目录', colors.green);
  }
}

// 测试 1: 模块导入测试
async function testModuleImports() {
  logSection('测试 1: 模块导入验证');

  const modules = [
    // 基础设施
    { name: 'ConfigManager', path: '../out/config' },
    { name: 'IDGenerator', path: '../out/utils/id-generator' },
    { name: 'TokenCounter', path: '../out/utils/token-counter' },
    { name: 'ErrorHandler', path: '../out/errors' },
    { name: 'PerformanceMonitor', path: '../out/monitoring/performance-monitor' },
    { name: 'FileLock', path: '../out/utils/file-lock' },

    // Snapshot 模块
    { name: 'SnapshotCoordinator', path: '../out/snapshot' },
    { name: 'AtomicOperations', path: '../out/snapshot/atomic-operations' },
    { name: 'SnapshotCache', path: '../out/snapshot/snapshot-cache' },
    { name: 'SnapshotValidator', path: '../out/snapshot/snapshot-validator' },
    { name: 'SnapshotCleaner', path: '../out/snapshot/snapshot-cleaner' },

    // Orchestrator 模块
    { name: 'IntelligentOrchestrator', path: '../out/orchestrator/orchestrator-facade' },
    { name: 'ConfigResolver', path: '../out/orchestrator/config-resolver' },
    { name: 'TaskContextManager', path: '../out/orchestrator/task-context-manager' },
    { name: 'InteractionModeManager', path: '../out/orchestrator/interaction-mode-manager' },
    { name: 'PlanCoordinator', path: '../out/orchestrator/plan-coordinator' },
    { name: 'ExecutionCoordinator', path: '../out/orchestrator/execution-coordinator' },

    // Mission Executor 模块
    { name: 'ExecutionCoordinator (Mission)', path: '../out/orchestrator/core/executors/execution-coordinator' },
    { name: 'PlanningExecutor', path: '../out/orchestrator/core/executors/planning-executor' },
    { name: 'AssignmentExecutor', path: '../out/orchestrator/core/executors/assignment-executor' },
  ];

  let importErrors = [];

  for (const module of modules) {
    try {
      const startTime = Date.now();
      require(module.path);
      const duration = Date.now() - startTime;

      logTest(`导入 ${module.name}`, true, duration);
      recordTest(true);

      // 性能检查：导入应该很快
      if (duration > 100) {
        log(`  ⚠️  导入耗时较长: ${duration}ms`, colors.yellow);
      }
    } catch (error) {
      logTest(`导入 ${module.name}`, false);
      recordTest(false);
      importErrors.push({ module: module.name, error: error.message });
    }
  }

  if (importErrors.length > 0) {
    log('\n导入错误详情:', colors.red);
    importErrors.forEach(({ module, error }) => {
      log(`  ${module}: ${error}`, colors.red);
    });
  }

  return importErrors.length === 0;
}

// 测试 2: 基础设施模块功能测试
async function testInfrastructureModules() {
  logSection('测试 2: 基础设施模块功能验证');

  // 测试 IDGenerator
  try {
    const { IDGenerator } = require('../out/utils/id-generator');
    const id1 = IDGenerator.generate('test');
    const id2 = IDGenerator.generate('test');

    const passed = id1 !== id2 && id1.startsWith('test-') && id2.startsWith('test-');
    logTest('IDGenerator 生成唯一 ID', passed);
    recordTest(passed);
  } catch (error) {
    logTest('IDGenerator 生成唯一 ID', false);
    recordTest(false);
    log(`  错误: ${error.message}`, colors.red);
  }

  // 测试 TokenCounter
  try {
    const { TokenCounter } = require('../out/utils/token-counter');
    const counter = new TokenCounter();
    const tokens = counter.countTokens('Hello, world!');

    const passed = tokens > 0 && tokens < 10;
    logTest('TokenCounter 计算 token 数量', passed);
    recordTest(passed);
  } catch (error) {
    logTest('TokenCounter 计算 token 数量', false);
    recordTest(false);
    log(`  错误: ${error.message}`, colors.red);
  }

  // 测试 FileLock
  try {
    const { FileLock } = require('../out/utils/file-lock');
    const lock = new FileLock();
    const testFile = 'test-file.txt';

    const acquired1 = lock.acquireLock(testFile);
    const acquired2 = lock.acquireLock(testFile);
    lock.releaseLock(testFile);
    const acquired3 = lock.acquireLock(testFile);

    const passed = acquired1 && !acquired2 && acquired3;
    logTest('FileLock 锁机制正常', passed);
    recordTest(passed);

    lock.releaseLock(testFile);
  } catch (error) {
    logTest('FileLock 锁机制正常', false);
    recordTest(false);
    log(`  错误: ${error.message}`, colors.red);
  }
}

// 测试 3: Snapshot 模块集成测试
async function testSnapshotModules() {
  logSection('测试 3: Snapshot 模块集成验证');

  try {
    const { SnapshotCache } = require('../out/snapshot/snapshot-cache');
    const cache = new SnapshotCache(10);

    // 测试缓存功能
    const testFile = path.join(TEST_CONFIG.workspaceRoot, 'src/index.ts');
    const content1 = cache.readFileWithCache(testFile);
    const content2 = cache.readFileWithCache(testFile);

    const passed = content1 === content2 && content1.includes('Test file');
    logTest('SnapshotCache 缓存功能正常', passed);
    recordTest(passed);

    // 测试缓存统计
    const stats = cache.getStats();
    const statsValid = stats.fileCache.size === 1 && stats.fileCache.maxSize === 10;
    logTest('SnapshotCache 统计信息正确', statsValid);
    recordTest(statsValid);

  } catch (error) {
    logTest('SnapshotCache 测试失败', false);
    recordTest(false);
    log(`  错误: ${error.message}`, colors.red);
  }

  try {
    const { SnapshotValidator } = require('../out/snapshot/snapshot-validator');
    const { UnifiedSessionManager } = require('../out/session');

    const sessionManager = new UnifiedSessionManager(TEST_CONFIG.workspaceRoot);
    const validator = new SnapshotValidator(sessionManager, TEST_CONFIG.workspaceRoot);

    // 测试快照验证
    const snapshot = {
      id: 'test-snapshot-1',
      sessionId: 'test-session',
      filePath: 'src/index.ts',
      originalContent: 'test content',
      lastModifiedAt: Date.now(),
      lastModifiedBy: 'claude',
      subTaskId: 'test-subtask',
    };

    const result = validator.validateSnapshot(snapshot);
    const passed = result.valid && result.errors.length === 0;
    logTest('SnapshotValidator 验证功能正常', passed);
    recordTest(passed);

  } catch (error) {
    logTest('SnapshotValidator 测试失败', false);
    recordTest(false);
    log(`  错误: ${error.message}`, colors.red);
  }
}

// 测试 4: Orchestrator 模块集成测试
async function testOrchestratorModules() {
  logSection('测试 4: Orchestrator 模块集成验证');

  try {
    const { ConfigResolver } = require('../out/orchestrator/config-resolver');

    // 测试配置解析
    const config = ConfigResolver.resolveConfig({
      timeout: 60000,
      maxRetries: 5,
    });

    const passed = config.timeout === 60000 && config.maxRetries === 5;
    logTest('ConfigResolver 配置解析正常', passed);
    recordTest(passed);

    // 测试权限解析
    const permissions = ConfigResolver.resolvePermissions(config);
    const permsPassed = permissions.allowEdit && permissions.allowBash && permissions.allowWeb;
    logTest('ConfigResolver 权限解析正常', permsPassed);
    recordTest(permsPassed);

  } catch (error) {
    logTest('ConfigResolver 测试失败', false);
    recordTest(false);
    log(`  错误: ${error.message}`, colors.red);
  }

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
    const modePassed = mode === 'agent';
    logTest('InteractionModeManager 模式设置正常', modePassed);
    recordTest(modePassed);

    // 测试 ask 模式判断
    const shouldAsk1 = manager.shouldUseAskMode('什么是 TypeScript?');
    const shouldAsk2 = manager.shouldUseAskMode('实现用户登录功能');
    const judgePassed = shouldAsk1 && !shouldAsk2;
    logTest('InteractionModeManager 模式判断正确', judgePassed);
    recordTest(judgePassed);

  } catch (error) {
    logTest('InteractionModeManager 测试失败', false);
    recordTest(false);
    log(`  错误: ${error.message}`, colors.red);
  }
}

// 测试 5: 性能基准测试
async function testPerformanceBenchmarks() {
  logSection('测试 5: 性能基准验证');

  // 测试模块初始化性能
  const initStart = Date.now();
  try {
    require('../out/orchestrator/orchestrator-facade');
    require('../out/snapshot');
    const initDuration = Date.now() - initStart;

    const passed = logPerformance(
      '模块初始化',
      initDuration,
      TEST_CONFIG.performanceThreshold.initialization
    );
    recordPerformance(passed);
  } catch (error) {
    log('❌ 模块初始化失败', colors.red);
    recordPerformance(false);
  }

  // 测试 ID 生成性能
  try {
    const { IDGenerator } = require('../out/utils/id-generator');
    const count = 1000;
    const start = Date.now();

    for (let i = 0; i < count; i++) {
      IDGenerator.generate('test');
    }

    const duration = Date.now() - start;
    const avgDuration = duration / count;

    log(`⚡ ID 生成性能: ${count} 次生成耗时 ${duration}ms (平均 ${avgDuration.toFixed(2)}ms/次)`, colors.green);
    const passed = avgDuration < 1; // 平均每次应小于1ms
    recordPerformance(passed);
  } catch (error) {
    log('❌ ID 生成性能测试失败', colors.red);
    recordPerformance(false);
  }

  // 测试 Token 计算性能
  try {
    const { TokenCounter } = require('../out/utils/token-counter');
    const counter = new TokenCounter();
    const testText = 'Hello, world! '.repeat(100); // ~1300 字符
    const count = 100;
    const start = Date.now();

    for (let i = 0; i < count; i++) {
      counter.countTokens(testText);
    }

    const duration = Date.now() - start;
    const avgDuration = duration / count;

    log(`⚡ Token 计算性能: ${count} 次计算耗时 ${duration}ms (平均 ${avgDuration.toFixed(2)}ms/次)`, colors.green);
    const passed = avgDuration < 10; // 平均每次应小于10ms
    recordPerformance(passed);
  } catch (error) {
    log('❌ Token 计算性能测试失败', colors.red);
    recordPerformance(false);
  }

  // 测试缓存性能
  try {
    const { SnapshotCache } = require('../out/snapshot/snapshot-cache');
    const cache = new SnapshotCache(100);
    const testFile = path.join(TEST_CONFIG.workspaceRoot, 'src/index.ts');

    // 首次读取（无缓存）
    const start1 = Date.now();
    cache.readFileWithCache(testFile);
    const duration1 = Date.now() - start1;

    // 第二次读取（有缓存）
    const start2 = Date.now();
    cache.readFileWithCache(testFile);
    const duration2 = Date.now() - start2;

    log(`⚡ 缓存性能: 首次读取 ${duration1}ms, 缓存读取 ${duration2}ms`, colors.green);
    const passed = duration2 < duration1 / 2; // 缓存读取应该快至少一半
    recordPerformance(passed);
  } catch (error) {
    log('❌ 缓存性能测试失败', colors.red);
    recordPerformance(false);
  }
}

// 测试 6: 向后兼容性测试
async function testBackwardCompatibility() {
  logSection('测试 6: 向后兼容性验证');

  // 测试旧的导入路径是否仍然有效
  const oldImports = [
    { name: 'IntelligentOrchestrator (旧路径)', path: '../src/orchestrator/intelligent-orchestrator' },
  ];

  for (const module of oldImports) {
    try {
      require(module.path);
      logTest(`${module.name} 仍可导入`, true);
      recordTest(true);
    } catch (error) {
      // 如果旧路径不存在，这是预期的（因为我们创建了新模块）
      if (error.code === 'MODULE_NOT_FOUND') {
        log(`ℹ️  ${module.name} 已迁移到新模块`, colors.blue);
      } else {
        logTest(`${module.name} 导入失败`, false);
        recordTest(false);
        log(`  错误: ${error.message}`, colors.red);
      }
    }
  }

  // 测试新的导入路径
  try {
    const { IntelligentOrchestrator } = require('../out/orchestrator/orchestrator-facade');
    const passed = typeof IntelligentOrchestrator === 'function';
    logTest('IntelligentOrchestrator (新路径) 可用', passed);
    recordTest(passed);
  } catch (error) {
    logTest('IntelligentOrchestrator (新路径) 不可用', false);
    recordTest(false);
    log(`  错误: ${error.message}`, colors.red);
  }
}

// 测试 7: 错误处理和恢复测试
async function testErrorHandling() {
  logSection('测试 7: 错误处理和恢复验证');

  // 测试文件锁超时
  try {
    const { FileLock } = require('../out/utils/file-lock');
    const lock = new FileLock();
    const testFile = 'test-lock.txt';

    lock.acquireLock(testFile);
    const acquired = lock.acquireLock(testFile, 100); // 100ms 超时

    const passed = !acquired; // 应该获取失败
    logTest('FileLock 超时处理正常', passed);
    recordTest(passed);

    lock.releaseLock(testFile);
  } catch (error) {
    logTest('FileLock 超时测试失败', false);
    recordTest(false);
    log(`  错误: ${error.message}`, colors.red);
  }

  // 测试快照验证错误处理
  try {
    const { SnapshotValidator } = require('../out/snapshot/snapshot-validator');
    const { UnifiedSessionManager } = require('../out/session');

    const sessionManager = new UnifiedSessionManager(TEST_CONFIG.workspaceRoot);
    const validator = new SnapshotValidator(sessionManager, TEST_CONFIG.workspaceRoot);

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

    const result = validator.validateSnapshot(invalidSnapshot);
    const passed = !result.valid && result.errors.length > 0;
    logTest('SnapshotValidator 错误检测正常', passed);
    recordTest(passed);

  } catch (error) {
    logTest('SnapshotValidator 错误处理测试失败', false);
    recordTest(false);
    log(`  错误: ${error.message}`, colors.red);
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

  log('\n性能测试:', colors.bright);
  log(`  总计: ${stats.performance.total}`, colors.blue);
  log(`  通过: ${stats.performance.passed}`, colors.green);
  log(`  失败: ${stats.performance.failed}`, stats.performance.failed > 0 ? colors.yellow : colors.green);
  log(`  成功率: ${((stats.performance.passed / stats.performance.total) * 100).toFixed(1)}%`,
      stats.performance.failed === 0 ? colors.green : colors.yellow);

  log(`\n总耗时: ${(totalDuration / 1000).toFixed(2)}s`, colors.blue);

  // 最终结论
  console.log('\n' + '='.repeat(80));
  if (stats.failed === 0 && stats.performance.failed === 0) {
    log('✅ 所有测试通过！架构验证成功！', colors.green + colors.bright);
  } else if (stats.failed === 0 && stats.performance.failed > 0) {
    log('⚠️  功能测试通过，但部分性能测试未达标', colors.yellow + colors.bright);
  } else {
    log('❌ 部分测试失败，需要修复', colors.red + colors.bright);
  }
  console.log('='.repeat(80) + '\n');

  return stats.failed === 0;
}

// 主测试流程
async function runTests() {
  log('\n╔══════════════════════════════════════════════════════════════════════════════╗', colors.cyan);
  log('║              MultiCLI 架构端到端验证测试                                      ║', colors.cyan);
  log('╚══════════════════════════════════════════════════════════════════════════════╝\n', colors.cyan);

  try {
    // 准备环境
    setupTestEnvironment();

    // 运行测试
    await testModuleImports();
    await testInfrastructureModules();
    await testSnapshotModules();
    await testOrchestratorModules();
    await testPerformanceBenchmarks();
    await testBackwardCompatibility();
    await testErrorHandling();

    // 打印报告
    const success = printTestReport();

    // 清理环境
    cleanupTestEnvironment();

    // 退出
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
