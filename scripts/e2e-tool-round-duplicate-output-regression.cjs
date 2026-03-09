#!/usr/bin/env node
/**
 * 工具轮重复输出回归脚本
 *
 * 目标：
 * 1) 工具轮文本一旦进入流式管道，必须标记为已交付（finalTextDelivered=true）
 * 2) 循环外 fallback 回灌仍受 !finalTextDelivered 保护
 */

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function main() {
  const filePath = path.join(ROOT, 'src', 'llm', 'adapters', 'orchestrator-adapter.ts');
  const source = fs.readFileSync(filePath, 'utf8');

  assert(
    source.includes('let finalTextDelivered = false;'),
    '缺少 finalTextDelivered 交付状态标记'
  );

  assert(
    source.includes('if (assistantText.trim()) {\n            lastNonEmptyAssistantText = assistantText;\n            // 文本一旦进入当轮流式管道（含 fallback 的 processTextDelta），\n            // 就应视为“已交付”。否则在工具轮触发终止（如 stalled/budget）时，\n            // 循环外 finalText fallback 会把同段文本再次回灌，造成重复输出。\n            finalTextDelivered = true;'),
    '工具轮文本交付标记缺失，可能导致重复回灌'
  );

  assert(
    source.includes('if (!isTransientSystemCall && finalText.trim() && !finalTextDelivered && !this.abortController?.signal.aborted) {'),
    '循环外 fallback 未受 finalTextDelivered 保护'
  );

  console.log('\n=== tool round duplicate output regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'tool-round-text-marked-delivered',
      'fallback-guarded-by-finalTextDelivered',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('tool round duplicate output 回归失败:', error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
}

