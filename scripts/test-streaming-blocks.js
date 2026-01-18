/**
 * 验证流式消息 blocks 一致性：
 * - append 更新会写入 text block
 * - block_update 会合并 tool_call
 * - complete 阶段使用后端解析 blocks
 *
 * 用法:
 *   node scripts/test-streaming-blocks.js
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

function buildStandardMessage(params) {
  return {
    id: params.id,
    traceId: params.traceId || `trace-${Date.now()}`,
    type: params.type || 'text',
    source: params.source || 'worker',
    cli: params.cli || 'claude',
    timestamp: params.timestamp || Date.now(),
    updatedAt: Date.now(),
    blocks: params.blocks || [],
    lifecycle: params.lifecycle || 'streaming',
    metadata: params.metadata || {},
  };
}

function extractTextFromBlocks(blocks) {
  return (blocks || [])
    .filter(b => b.type === 'text')
    .map(b => b.content)
    .join('\n');
}

function standardToWebviewMessage(message) {
  const blocks = message.blocks || [];
  return {
    content: extractTextFromBlocks(blocks),
    parsedBlocks: blocks,
    toolCalls: (blocks || []).filter(b => b.type === 'tool_call').map(b => ({
      id: b.toolId,
      name: b.toolName,
      status: b.status,
      input: b.input,
      output: b.output,
      error: b.error,
    })),
    streaming: message.lifecycle === 'streaming' || message.lifecycle === 'started',
  };
}

function applyUpdateToStandardMessage(message, update) {
  if (update.updateType === 'append' && update.appendText) {
    let textBlock = message.blocks.find(b => b.type === 'text');
    if (!textBlock) {
      textBlock = { type: 'text', content: '', isMarkdown: true };
      message.blocks.push(textBlock);
    }
    textBlock.content += update.appendText;
  } else if (update.updateType === 'replace' && update.replaceText !== undefined) {
    let textBlock = message.blocks.find(b => b.type === 'text');
    if (!textBlock) {
      textBlock = { type: 'text', content: '', isMarkdown: true };
      message.blocks.push(textBlock);
    }
    textBlock.content = update.replaceText;
  } else if (update.updateType === 'block_update' && Array.isArray(update.blocks)) {
    for (const newBlock of update.blocks) {
      if (newBlock.type === 'tool_call') {
        const existing = message.blocks.find(
          b => b.type === 'tool_call' && b.toolId === newBlock.toolId
        );
        if (existing) {
          Object.assign(existing, newBlock);
        } else {
          message.blocks.push(newBlock);
        }
      } else {
        message.blocks.push(newBlock);
      }
    }
  }
}

function run() {
  console.log('=== 流式 blocks 一致性验证开始 ===');

  const messageId = `msg-${Date.now()}-1`;
  const started = buildStandardMessage({ id: messageId, lifecycle: 'streaming', blocks: [] });

  // append: 写入 text block
  const append1 = 'Hello world\n';
  applyUpdateToStandardMessage(started, { messageId, updateType: 'append', appendText: append1 });
  assert.strictEqual(started.blocks.length, 1, 'append 后应有 1 个 text block');
  assert.strictEqual(started.blocks[0].content, append1, 'text block 内容应匹配');

  // block_update: tool_call 合并
  applyUpdateToStandardMessage(started, {
    messageId,
    updateType: 'block_update',
    blocks: [{
      type: 'tool_call',
      toolId: 'tool-1',
      toolName: 'search_context',
      status: 'running',
      input: { q: 'test' },
    }],
  });
  applyUpdateToStandardMessage(started, {
    messageId,
    updateType: 'block_update',
    blocks: [{
      type: 'tool_call',
      toolId: 'tool-1',
      toolName: 'search_context',
      status: 'completed',
      output: 'ok',
    }],
  });
  const toolBlocks = started.blocks.filter(b => b.type === 'tool_call');
  assert.strictEqual(toolBlocks.length, 1, '同一 toolId 只能保留一个 tool_call');
  assert.strictEqual(toolBlocks[0].status, 'completed', 'tool_call 状态应更新');

  // 完成阶段：后端解析 blocks
  const finalContent = [
    'Result:',
    '```js',
    'console.log("ok");',
    '```',
  ].join('\n');
  const parsedBlocks = parseContentToBlocks(finalContent);
  const completed = buildStandardMessage({
    id: messageId,
    lifecycle: 'completed',
    blocks: parsedBlocks,
  });
  const webviewMsg = standardToWebviewMessage(completed);
  const codeBlocks = webviewMsg.parsedBlocks.filter(b => b.type === 'code');
  assert.ok(codeBlocks.length >= 1, '完成态应包含 code block');
  assert.ok(webviewMsg.content.includes('Result:'), '完成态 text content 应保留');

  console.log('✓ append -> text block');
  console.log('✓ block_update -> tool_call 合并');
  console.log('✓ complete -> 解析 blocks');
  console.log('=== 验证完成 ===');
}

run();
