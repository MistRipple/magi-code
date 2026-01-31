/**
 * 真实 LLM E2E 测试启动器
 * 在加载测试前注入 vscode mock
 */

// 在任何其他模块加载前注入 vscode mock
import * as vscodeMock from './vscode-mock';

const Module = require('module');
const originalRequire = Module.prototype.require;

Module.prototype.require = function(id: string) {
  if (id === 'vscode') {
    return vscodeMock;
  }
  return originalRequire.apply(this, arguments);
};

import './real-llm-e2e';
