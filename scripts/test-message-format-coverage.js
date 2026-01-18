/**
 * 消息格式覆盖测试：
 * - Markdown / JSON / Diff / 代码块 / 混合内容
 * - 纯文本 + 特殊行号输出
 * - 工具调用块注入
 * - 未闭合 code fence 的容错
 *
 * 用法:
 *   node scripts/test-message-format-coverage.js
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

const { parseContentToBlocks } = requireOrExit('../out/utils/content-parser.js');

function pickTypes(blocks) {
  return (blocks || []).map(b => b.type);
}

function findFirst(blocks, type) {
  return (blocks || []).find(b => b.type === type);
}

function run() {
  console.log('=== 消息格式覆盖测试开始 ===');

  // Markdown
  const md = '# Title\n- item';
  const mdBlocks = parseContentToBlocks(md);
  assert.strictEqual(mdBlocks.length, 1, 'Markdown 应生成 1 个块');
  assert.strictEqual(mdBlocks[0].type, 'text', 'Markdown 应为 text 块');
  assert.strictEqual(mdBlocks[0].isMarkdown, true, 'Markdown text 块应标记 isMarkdown');

  // JSON
  const json = '{"a":1,"b":2}';
  const jsonBlocks = parseContentToBlocks(json);
  assert.strictEqual(jsonBlocks.length, 1, 'JSON 应生成 1 个块');
  assert.strictEqual(jsonBlocks[0].type, 'code', 'JSON 应为 code 块');
  assert.strictEqual(jsonBlocks[0].language, 'json', 'JSON code 块语言应为 json');

  // Diff code fence
  const diff = '```diff\n- old\n+ new\n```';
  const diffBlocks = parseContentToBlocks(diff);
  const diffCode = findFirst(diffBlocks, 'code');
  assert.ok(diffCode, 'diff 应生成 code 块');
  assert.strictEqual(diffCode.language, 'diff', 'diff code 块语言应为 diff');

  // Mixed text + code
  const mixed = 'Intro\n```js\nconsole.log(1)\n```\nOutro';
  const mixedBlocks = parseContentToBlocks(mixed);
  assert.deepStrictEqual(pickTypes(mixedBlocks), ['text', 'code', 'text'], '混合内容应拆为 text/code/text');

  // Numbered code output
  const numbered = ['1→ foo', '2→ bar', '3→ baz', '4→ qux', '5→ quux'].join('\n');
  const numberedBlocks = parseContentToBlocks(numbered);
  assert.strictEqual(numberedBlocks.length, 1, '带行号输出应生成 1 个块');
  assert.strictEqual(numberedBlocks[0].type, 'code', '带行号输出应为 code 块');

  // Unclosed code fence -> markdown text
  const unclosed = '```js\nconst x = 1';
  const unclosedBlocks = parseContentToBlocks(unclosed);
  assert.strictEqual(unclosedBlocks.length, 1, '未闭合 fence 应生成 1 个块');
  assert.strictEqual(unclosedBlocks[0].type, 'text', '未闭合 fence 应为 text 块');
  assert.strictEqual(unclosedBlocks[0].isMarkdown, true, '未闭合 fence 应标记为 markdown');

  // Tool call injection
  const toolBlocks = parseContentToBlocks('hi', {
    toolCalls: [{ name: 'search_context', input: { q: 'test' }, status: 'completed' }],
  });
  const toolCall = findFirst(toolBlocks, 'tool_call');
  assert.ok(toolCall, 'toolCalls 注入应生成 tool_call 块');
  assert.strictEqual(toolCall.toolName, 'search_context', 'tool_call 名称应匹配');
  assert.strictEqual(toolCall.status, 'completed', 'tool_call 状态应匹配');

  console.log('✓ Markdown');
  console.log('✓ JSON');
  console.log('✓ Diff code fence');
  console.log('✓ Mixed text + code');
  console.log('✓ Numbered code output');
  console.log('✓ Unclosed code fence');
  console.log('✓ Tool call injection');
  console.log('=== 测试完成 ===');
}

run();
