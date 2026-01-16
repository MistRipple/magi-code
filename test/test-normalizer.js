#!/usr/bin/env node
/**
 * 测试消息规范化器
 * 验证各CLI的Normalizer是否正确处理工具调用和思考过程
 */

const path = require('path');

// 加载编译后的模块
const { ClaudeNormalizer } = require('../out/normalizer/claude-normalizer');
const { CodexNormalizer } = require('../out/normalizer/codex-normalizer');
const { GeminiNormalizer } = require('../out/normalizer/gemini-normalizer');

console.log('======================================================================');
console.log('消息规范化器测试');
console.log('======================================================================\n');

// 测试数据 - Claude stream-json 格式
const claudeToolCallData = [
  '{"type":"message_start","message":{"id":"msg_123","role":"assistant"}}',
  '{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"Read","input":{}}}',
  '{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\\"path\\": \\"package.json\\"}"}}',
  '{"type":"content_block_stop","index":0}',
  '{"type":"tool_result","result":{"type":"tool_result","content":"{\\"name\\": \\"multicli\\"}","is_error":false}}',
  '{"type":"message_stop"}',
];

const claudeThinkingData = [
  '{"type":"message_start","message":{"id":"msg_456","role":"assistant"}}',
  '{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}',
  '{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"让我分析一下这个问题..."}}',
  '{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"首先需要检查文件结构"}}',
  '{"type":"content_block_stop","index":0}',
  '{"type":"content_block_start","index":1,"content_block":{"type":"text"}}',
  '{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"根据分析，这是一个Node.js项目。"}}',
  '{"type":"content_block_stop","index":1}',
  '{"type":"message_stop"}',
];

// 测试数据 - Gemini JSON 格式
const geminiToolCallData = [
  '{"type":"tool_call","name":"read_file","id":"gtool_1","args":{"path":"package.json"}}',
  '{"type":"tool_result","result":"{\\"name\\": \\"multicli\\"}"}',
];

function testNormalizer(name, NormalizerClass, testData, description) {
  console.log(`\n--- 测试 ${name}: ${description} ---`);
  
  const normalizer = new NormalizerClass({ debug: false });
  const messages = [];
  const updates = [];
  
  normalizer.on('message', (msg) => {
    messages.push(msg);
    console.log(`[message] id=${msg.id}, lifecycle=${msg.lifecycle}`);
  });
  
  normalizer.on('update', (update) => {
    updates.push(update);
    console.log(`[update] type=${update.updateType}, messageId=${update.messageId}`);
  });
  
  normalizer.on('complete', (messageId, msg) => {
    console.log(`[complete] id=${messageId}, blocks=${msg.blocks.length}`);
    
    // 分析 blocks
    const toolCalls = msg.blocks.filter(b => b.type === 'tool_call');
    const thinking = msg.blocks.filter(b => b.type === 'thinking');
    const text = msg.blocks.filter(b => b.type === 'text');
    
    console.log(`  - 工具调用: ${toolCalls.length} 个`);
    toolCalls.forEach(t => console.log(`    * ${t.toolName} (${t.status})`));
    
    console.log(`  - 思考过程: ${thinking.length} 个`);
    thinking.forEach(t => console.log(`    * ${t.content?.slice(0, 50)}...`));
    
    console.log(`  - 文本块: ${text.length} 个`);
    text.forEach(t => console.log(`    * ${t.content?.slice(0, 50)}...`));
  });
  
  // 开始流
  const messageId = normalizer.startStream('test-trace', 'worker');
  
  // 处理数据
  for (const line of testData) {
    normalizer.processChunk(messageId, line + '\n');
  }
  
  // 结束流
  normalizer.endStream(messageId);
  
  return { messages, updates };
}

// 运行测试
console.log('\n========== Claude Normalizer 测试 ==========');
testNormalizer('Claude', ClaudeNormalizer, claudeToolCallData, '工具调用');
testNormalizer('Claude', ClaudeNormalizer, claudeThinkingData, '思考过程');

console.log('\n========== Gemini Normalizer 测试 ==========');
testNormalizer('Gemini', GeminiNormalizer, geminiToolCallData, '工具调用');

console.log('\n========== Codex Normalizer 测试 ==========');
const codexData = [
  '# 分析结果',
  '',
  '```javascript',
  'const x = 1;',
  '```',
  '',
  '这是一个简单的变量声明。',
];
testNormalizer('Codex', CodexNormalizer, codexData, '代码块');

console.log('\n======================================================================');
console.log('测试完成');
console.log('======================================================================');

