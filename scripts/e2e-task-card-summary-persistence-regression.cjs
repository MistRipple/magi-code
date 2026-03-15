#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function loadCompiledModule(relPath) {
  const ROOT = path.resolve(__dirname, '..');
  const OUT = path.join(ROOT, 'out');
  const abs = path.join(OUT, relPath);
  if (!fs.existsSync(abs)) {
    throw new Error(`缺少编译产物: ${abs}，请先执行 npm run -s compile`);
  }
  return require(abs);
}

function assertSourceUsesSharedRuntime() {
  const ROOT = path.resolve(__dirname, '..');
  const messageItemSource = fs.readFileSync(
    path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'components', 'MessageItem.svelte'),
    'utf8',
  );
  const messageHandlerSource = fs.readFileSync(
    path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'lib', 'message-handler.ts'),
    'utf8',
  );
  assert(
    messageItemSource.includes('buildWaitResultFromTaskCardMessage(message)?.result || null'),
    'MessageItem 仍未将 task_card 持久化总结作为恢复兜底渲染源',
  );
  assert(
    messageItemSource.includes('resolveTaskCardKeyFromMetadata('),
    'MessageItem 仍未复用统一 task card key 解析逻辑',
  );
  assert(
    messageHandlerSource.includes('buildWaitResultFromTaskCardMessage(message)'),
    'message-handler 仍未复用统一 task card 完成态重建逻辑',
  );
}

function main() {
  assertSourceUsesSharedRuntime();

  const { buildWaitResultFromTaskCardMessage, resolveTaskCardKeyFromMetadata } = loadCompiledModule(
    path.join('ui', 'webview-svelte', 'src', 'lib', 'task-card-runtime.js'),
  );

  const metadata = {
    missionId: 'mission-123',
    subTaskCard: {
      id: 'assignment-42',
      worker: 'claude',
      status: 'completed',
      summary: '健康检查模块架构增强方案已完成',
      modifiedFiles: ['src/health/check.ts', 'src/health/plan.ts'],
    },
  };

  const cardKey = resolveTaskCardKeyFromMetadata(metadata);
  assert(cardKey === 'assign:assignment-42@mission-123', `task card key 恢复错误: ${cardKey}`);

  const rebuilt = buildWaitResultFromTaskCardMessage({
    id: 'subtask-card-assignment-42-mission-123',
    type: 'task_card',
    timestamp: 1710000000000,
    metadata,
  });

  assert(rebuilt, '未能从 task_card 消息重建完成总结');
  assert(
    rebuilt.cardKey === 'assign:assignment-42@mission-123',
    `重建 cardKey 错误: ${rebuilt.cardKey}`,
  );
  assert(rebuilt.result.results.length === 1, '重建结果数量错误');
  assert(
    rebuilt.result.results[0].summary === '健康检查模块架构增强方案已完成',
    `重建 summary 错误: ${rebuilt.result.results[0].summary}`,
  );
  assert(
    rebuilt.result.results[0].modified_files.length === 2,
    `重建 modified_files 错误: ${JSON.stringify(rebuilt.result.results[0].modified_files)}`,
  );
  assert(rebuilt.result.wait_status === 'completed', `wait_status 错误: ${rebuilt.result.wait_status}`);

  console.log(JSON.stringify({
    pass: true,
    checks: [
      'task_card_key_restores_from_subtask_card_id',
      'task_card_summary_rebuilds_from_persisted_message',
      'message_item_uses_persisted_task_card_summary_fallback',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('task card summary persistence 回归失败:', error?.stack || error);
  process.exit(1);
}
