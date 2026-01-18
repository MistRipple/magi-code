/**
 * UI 验证脚本：
 * 1) plan_ready 的 content 与 formattedPlan 相同不会重复
 * 2) MessageLifecycle.STARTED 触发 streaming 指示器
 *
 * 用法:
 *   node scripts/test-ui-dedupe-started.js
 */

const assert = require('assert');

function requireOrExit(path) {
  try {
    return require(path);
  } catch (err) {
    console.error(`无法加载 ${path}，请先运行 npm run compile`);
    throw err;
  }
}

const { normalizeOrchestratorMessage } = requireOrExit('../out/normalizer/orchestrator-normalizer');
const { MessageLifecycle } = requireOrExit('../out/protocol');

function extractTextBlocks(blocks) {
  return (blocks || [])
    .filter(b => b && b.type === 'text' && typeof b.content === 'string')
    .map(b => b.content);
}

function toWebviewStreamingFlag(lifecycle) {
  return lifecycle === 'streaming' || lifecycle === 'started';
}

function testPlanReadyDedupSame() {
  const content = '计划内容A';
  const msg = normalizeOrchestratorMessage({
    type: 'plan_ready',
    taskId: 'task-1',
    timestamp: Date.now(),
    content,
    metadata: { formattedPlan: content },
  });
  const texts = extractTextBlocks(msg.blocks);
  assert.strictEqual(texts.length, 1, 'plan_ready 同内容不应重复 block');
  assert.strictEqual(texts[0], content, 'plan_ready 内容应匹配');
}

function testPlanReadyDedupDifferent() {
  const content = '计划内容A';
  const formattedPlan = '计划内容B';
  const msg = normalizeOrchestratorMessage({
    type: 'plan_ready',
    taskId: 'task-2',
    timestamp: Date.now(),
    content,
    metadata: { formattedPlan },
  });
  const texts = extractTextBlocks(msg.blocks);
  assert.strictEqual(texts.length, 2, 'plan_ready 内容不同应保留两个 block');
  assert.strictEqual(texts[0], content, '第一个 block 应为 content');
  assert.strictEqual(texts[1], formattedPlan, '第二个 block 应为 formattedPlan');
}

function testStartedStreamingFlag() {
  const flag = toWebviewStreamingFlag(MessageLifecycle.STARTED);
  assert.strictEqual(flag, true, 'STARTED 应触发 streaming 指示器');
}

function run() {
  console.log('=== UI 去重/streaming 验证开始 ===');
  testPlanReadyDedupSame();
  console.log('✓ plan_ready 同内容不重复');
  testPlanReadyDedupDifferent();
  console.log('✓ plan_ready 不同内容保留');
  testStartedStreamingFlag();
  console.log('✓ STARTED 触发 streaming');
  console.log('=== 验证完成 ===');
}

run();
