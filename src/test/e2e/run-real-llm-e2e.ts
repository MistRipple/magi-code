/**
 * 真实 LLM E2E 测试启动器
 * 在加载测试前注入 vscode mock
 */

// 在任何其他模块加载前注入 vscode mock
import * as vscodeMock from './vscode-mock';

// 使用 require.cache 和 Module._resolveFilename 拦截
const Module = require('module');

// 保存原始的 _resolveFilename
const originalResolveFilename = Module._resolveFilename;

// 拦截模块解析
Module._resolveFilename = function(request: string, parent: any, isMain: boolean, options?: any) {
  if (request === 'vscode') {
    // 返回 mock 模块的路径
    return require.resolve('./vscode-mock');
  }
  return originalResolveFilename.call(this, request, parent, isMain, options);
};

// 预先将 mock 放入缓存
try {
  require.cache['vscode'] = {
    id: 'vscode',
    filename: 'vscode',
    loaded: true,
    exports: vscodeMock,
  } as any;
} catch {
  // ignore
}

import './real-llm-e2e';
