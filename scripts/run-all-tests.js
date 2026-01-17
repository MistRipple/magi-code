/**
 * MultiCLI 测试运行器
 *
 * 支持多种测试模式:
 * - quick: 快速单元测试 (< 1秒)
 * - full: 完整测试套件 (包括 E2E, < 30秒)
 * - unit: 仅单元测试
 * - e2e: 仅 E2E 测试
 *
 * 使用方式:
 *   npm test              # 运行 quick 模式
 *   npm run test:quick    # 运行 quick 模式
 *   npm run test:full     # 运行完整测试
 */

const { spawn } = require('child_process');
const { TestRunner } = require('./test-utils');

/**
 * 测试套件配置
 */
const TEST_SUITES = {
  // 快速测试 (纯单元测试, < 1秒)
  quick: [
    'test-orchestrator-workers-e2e.js',      // 画像系统单元测试
    'test-architecture-optimization.js',     // 架构优化综合测试
  ],

  // 完整测试 (包括 E2E, < 30秒)
  full: [
    'test-orchestrator-workers-e2e.js',
    'test-architecture-optimization.js',
    'e2e-architecture-test.js',              // E2E 架构测试 (需要真实 CLI)
    // 'test-profile-e2e.js',                // 画像端到端测试 (可选)
  ],

  // 仅单元测试
  unit: [
    'test-orchestrator-workers-e2e.js',
    'test-architecture-optimization.js',
  ],

  // 仅 E2E 测试
  e2e: [
    'e2e-architecture-test.js',
  ],
};

/**
 * 运行单个测试脚本
 * @param {string} script - 脚本文件名
 * @returns {Promise<{name: string, passed: boolean, duration: number, exitCode: number}>}
 */
function runTest(script) {
  return new Promise((resolve) => {
    const startTime = Date.now();
    const proc = spawn('node', [`scripts/${script}`], {
      stdio: 'inherit',
      cwd: process.cwd(),
    });

    proc.on('close', (code) => {
      const duration = Date.now() - startTime;
      resolve({
        name: script,
        passed: code === 0,
        duration,
        exitCode: code,
      });
    });

    proc.on('error', (error) => {
      const duration = Date.now() - startTime;
      console.error(`\n❌ 执行失败: ${script}`);
      console.error(error);
      resolve({
        name: script,
        passed: false,
        duration,
        exitCode: 1,
        error: error.message,
      });
    });
  });
}

/**
 * 主测试流程
 */
async function main() {
  const mode = process.argv[2] || 'quick';
  const tests = TEST_SUITES[mode];

  if (!tests) {
    console.error(`\n❌ 未知测试模式: "${mode}"`);
    console.error('\n可用模式:');
    Object.keys(TEST_SUITES).forEach(m => {
      console.error(`  - ${m}: ${TEST_SUITES[m].length} 个测试`);
    });
    process.exit(1);
  }

  const runner = new TestRunner(`MultiCLI 测试套件 (${mode} 模式)`);

  runner.log(`\n即将运行 ${tests.length} 个测试脚本:\n`, 'cyan');
  tests.forEach((test, i) => {
    runner.log(`  ${i + 1}. ${test}`, 'blue');
  });
  console.log('');

  // 依次运行每个测试
  for (const script of tests) {
    runner.logSection(`运行: ${script}`);

    const result = await runTest(script);

    runner.logTest(
      script,
      result.passed,
      `耗时: ${(result.duration / 1000).toFixed(2)}s, 退出码: ${result.exitCode}${result.error ? ', 错误: ' + result.error : ''}`
    );

    // 如果测试失败且是 E2E 测试,给出提示
    if (!result.passed && script.includes('e2e')) {
      runner.log('   💡 提示: E2E 测试需要真实 CLI 环境,确保已安装 claude/codex/gemini', 'yellow');
    }
  }

  // 输出汇总
  process.exit(runner.finish());
}

// 运行测试
main().catch(error => {
  console.error('\n❌ 测试运行器失败:');
  console.error(error);
  process.exit(1);
});
