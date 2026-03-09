#!/usr/bin/env node
/**
 * 工具终止鲁棒性回归脚本
 *
 * 目标：
 * 1) 软失败（blocked/rejected/aborted）不再被透传为模型 hard error
 * 2) 编排/Worker 对 tool_result.is_error 使用硬失败判定
 * 3) Normalizer 与前端状态映射仅把硬失败显示为 error
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function read(relPath) {
  return fs.readFileSync(path.join(ROOT, relPath), 'utf8');
}

function main() {
  const protocolUtils = read('src/llm/protocol/adapters/protocol-utils.ts');
  const orchestratorAdapter = read('src/llm/adapters/orchestrator-adapter.ts');
  const workerAdapter = read('src/llm/adapters/worker-adapter.ts');
  const baseNormalizer = read('src/normalizer/base-normalizer.ts');
  const messageHandler = read('src/ui/webview-svelte/src/lib/message-handler.ts');

  assert(
    protocolUtils.includes("normalizedStatus === 'error'") && protocolUtils.includes("normalizedStatus === 'timeout'") && protocolUtils.includes("normalizedStatus === 'killed'"),
    '协议层缺少硬失败状态判定（error/timeout/killed）'
  );
  assert(
    protocolUtils.includes("normalizedStatus === 'blocked'") && protocolUtils.includes("normalizedStatus === 'rejected'") && protocolUtils.includes("normalizedStatus === 'aborted'"),
    '协议层缺少软失败状态判定（blocked/rejected/aborted）'
  );
  assert(
    orchestratorAdapter.includes('is_error: this.isHardToolFailure(result)'),
    'Orchestrator tool_result 仍未使用硬失败语义'
  );
  assert(
    workerAdapter.includes('is_error: this.isHardToolFailure(result)'),
    'Worker tool_result 仍未使用硬失败语义'
  );
  assert(
    baseNormalizer.includes("status === 'error' || status === 'timeout' || status === 'killed'"),
    'Normalizer 仍将软失败归类为 failed'
  );
  assert(
    messageHandler.includes("case 'blocked':") && messageHandler.includes("case 'rejected':") && messageHandler.includes("case 'aborted':") && messageHandler.includes("return 'success';"),
    '前端工具状态映射仍将软失败展示为 error'
  );

  console.log('\n=== tool termination resilience regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'protocol-hard-vs-soft-failure',
      'orchestrator-hard-error-flag',
      'worker-hard-error-flag',
      'normalizer-hard-failure-only',
      'ui-soft-failure-not-red',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('tool termination resilience 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
}

