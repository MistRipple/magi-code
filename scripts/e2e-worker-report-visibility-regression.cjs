#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function main() {
  const waitResultCardSource = fs.readFileSync(
    path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'components', 'WaitResultCard.svelte'),
    'utf8',
  );
  const subTaskSummaryCardSource = fs.readFileSync(
    path.join(ROOT, 'src', 'ui', 'webview-svelte', 'src', 'components', 'SubTaskSummaryCard.svelte'),
    'utf8',
  );

  assert(
    !waitResultCardSource.includes('{#if result.summary}'),
    'WaitResultCard 仍在用户层渲染单个 Worker 完成上报摘要',
  );
  assert(
    !waitResultCardSource.includes('class="item-summary"'),
    'WaitResultCard 仍保留 Worker 完成上报摘要 DOM',
  );
  assert(
    !subTaskSummaryCardSource.includes('{#if result.summary}'),
    'SubTaskSummaryCard 仍在用户层渲染单个 Worker 完成上报摘要',
  );
  assert(
    !subTaskSummaryCardSource.includes('class="result-summary"'),
    'SubTaskSummaryCard 仍保留 Worker 完成上报摘要 DOM',
  );

  console.log('\n=== worker report visibility regression ===');
  console.log(JSON.stringify({
    pass: true,
    checks: [
      'wait_result_card_hides_worker_completion_summary',
      'subtask_summary_card_hides_worker_completion_summary',
    ],
  }, null, 2));
}

try {
  main();
} catch (error) {
  console.error('worker report visibility 回归失败:', error?.stack || error);
  process.exit(1);
}
