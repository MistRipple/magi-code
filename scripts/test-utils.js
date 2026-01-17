/**
 * MultiCLI 测试工具库
 * 提供统一的日志、断言、结果追踪功能
 *
 * 使用示例:
 * ```javascript
 * const { TestRunner } = require('./test-utils');
 *
 * async function main() {
 *   const runner = new TestRunner('My Test Suite');
 *
 *   runner.logSection('Phase 1');
 *   runner.logTest('Test case 1', true, 'Success');
 *   runner.logTest('Test case 2', false, 'Failed');
 *
 *   process.exit(runner.finish());
 * }
 * ```
 */

/** 颜色常量 */
const colors = {
  reset: '\x1b[0m',
  green: '\x1b[32m',
  red: '\x1b[31m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  cyan: '\x1b[36m',
  magenta: '\x1b[35m',
};

/**
 * 测试运行器
 * 提供统一的测试结果追踪和输出格式
 */
class TestRunner {
  /**
   * 创建测试运行器
   * @param {string} name - 测试套件名称
   */
  constructor(name) {
    this.name = name;
    this.results = {
      passed: 0,
      failed: 0,
      tests: []
    };
    this.startTime = Date.now();
  }

  /**
   * 记录测试结果
   * @param {string} name - 测试名称
   * @param {boolean} passed - 是否通过
   * @param {string} details - 详细信息
   */
  logTest(name, passed, details = '') {
    const symbol = passed ? '✅' : '❌';
    const color = passed ? 'green' : 'red';
    this.log(`${symbol} ${name}`, color);

    if (details) {
      console.log(`   ${details}`);
    }

    this.results.tests.push({ name, passed, details });
    passed ? this.results.passed++ : this.results.failed++;
  }

  /**
   * 彩色日志输出
   * @param {string} msg - 消息内容
   * @param {string} color - 颜色名称
   */
  log(msg, color = 'reset') {
    console.log(`${colors[color]}${msg}${colors.reset}`);
  }

  /**
   * 输出章节标题
   * @param {string} title - 章节标题
   */
  logSection(title) {
    console.log('\n' + '='.repeat(80));
    this.log(`  ${title}`, 'cyan');
    console.log('='.repeat(80));
  }

  /**
   * 完成测试并输出汇总
   * @returns {number} 退出码 (0=成功, 1=失败)
   */
  finish() {
    const duration = ((Date.now() - this.startTime) / 1000).toFixed(2);
    const total = this.results.passed + this.results.failed;

    console.log('\n' + '━'.repeat(80));
    this.log('  测试结果汇总', 'magenta');
    console.log('━'.repeat(80));

    this.log(`⏱️  耗时: ${duration}s`, 'blue');
    this.log(`✅ 通过: ${this.results.passed}/${total} (${((this.results.passed/total)*100).toFixed(1)}%)`,
              this.results.failed === 0 ? 'green' : 'yellow');

    if (this.results.failed > 0) {
      this.log(`❌ 失败: ${this.results.failed}/${total}`, 'red');

      console.log('');
      this.log('失败的测试:', 'red');
      this.results.tests.filter(t => !t.passed).forEach(t => {
        this.log(`  ❌ ${t.name}`, 'red');
        if (t.details) {
          console.log(`     ${t.details}`);
        }
      });
    }

    console.log('━'.repeat(80));
    console.log('');

    return this.results.failed === 0 ? 0 : 1;
  }

  /**
   * 获取测试结果
   * @returns {Object} 测试结果对象
   */
  getResults() {
    return this.results;
  }
}

/**
 * 辅助函数: 等待条件满足或超时
 * @param {Function} conditionFn - 返回布尔值的条件函数
 * @param {number} timeout - 超时时间(毫秒)
 * @param {number} checkInterval - 检查间隔(毫秒)
 * @returns {Promise<boolean>} 是否在超时前满足条件
 */
async function waitFor(conditionFn, timeout = 5000, checkInterval = 100) {
  const startTime = Date.now();
  while (Date.now() - startTime < timeout) {
    if (await conditionFn()) {
      return true;
    }
    await new Promise(resolve => setTimeout(resolve, checkInterval));
  }
  return false;
}

/** 导出 */
module.exports = {
  colors,
  TestRunner,
  waitFor,
};
