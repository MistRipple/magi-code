/**
 * E2E 测试启动器
 * 在加载测试前注入 vscode mock
 */

// 在任何其他模块加载前注入 vscode mock
import * as vscodeMock from './vscode-mock';

// 使用 Module 的 _cache 来注入 mock
const Module = require('module');
const originalRequire = Module.prototype.require;

Module.prototype.require = function(id: string) {
  if (id === 'vscode') {
    return vscodeMock;
  }
  return originalRequire.apply(this, arguments);
};

// 现在可以安全地导入测试模块
import { runComprehensiveTests } from './comprehensive-e2e';

// 运行测试
runComprehensiveTests().catch(err => {
  console.error('测试运行失败:', err);
  process.exit(1);
});

