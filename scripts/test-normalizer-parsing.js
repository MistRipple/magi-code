/**
 * 测试 ClaudeNormalizer 是否正确解析 stream-json
 */

const path = require('path');

async function run() {
  const outDir = path.join(__dirname, '../out');
  const { ClaudeNormalizer } = require(path.join(outDir, 'normalizer/claude-normalizer'));

  console.log('=== ClaudeNormalizer 解析测试 ===\n');

  const normalizer = new ClaudeNormalizer({ cli: 'claude', defaultSource: 'worker' });

  // 模拟 Claude CLI 的 stream-json 输出
  const testChunks = [
    '{"type":"message_start","message":{"id":"msg_123"}}\n',
    '{"type":"content_block_start","index":0,"content_block":{"type":"text"}}\n',
    '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}\n',
    '{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"！"}}\n',
    '{"type":"content_block_stop","index":0}\n',
    '{"type":"message_stop"}\n',
  ];

  const messages = [];
  const updates = [];
  const completes = [];

  normalizer.on('message', (msg) => {
    messages.push(msg);
    console.log('[message]', msg.id, msg.lifecycle);
  });

  normalizer.on('update', (update) => {
    updates.push(update);
    console.log('[update]', update.updateType, update.appendText?.slice(0, 20));
  });

  normalizer.on('complete', (messageId, msg) => {
    completes.push(msg);
    console.log('[complete]', messageId, 'blocks:', msg.blocks.length);
    msg.blocks.forEach(b => {
      console.log('  -', b.type, ':', (b.content || '').slice(0, 50));
    });
  });

  // 开始流
  const messageId = normalizer.startStream('test-trace-1', 'worker');
  console.log('Started stream:', messageId, '\n');

  // 处理每个块
  for (const chunk of testChunks) {
    normalizer.processChunk(messageId, chunk);
  }

  // 结束流
  const finalMessage = normalizer.endStream(messageId);

  console.log('\n=== 结果 ===');
  console.log('Messages:', messages.length);
  console.log('Updates:', updates.length);
  console.log('Completes:', completes.length);
  console.log('\n最终消息:');
  console.log('  ID:', finalMessage.id);
  console.log('  Blocks:', finalMessage.blocks.length);
  console.log('  Content:', finalMessage.blocks.map(b => b.content).join(''));

  // 测试2：完整的 assistant 消息（一次性）
  console.log('\n\n=== 测试2：完整 assistant 消息 ===\n');

  const normalizer2 = new ClaudeNormalizer({ cli: 'claude', defaultSource: 'worker' });
  const messages2 = [];
  const completes2 = [];

  normalizer2.on('message', (msg) => messages2.push(msg));
  normalizer2.on('complete', (_, msg) => completes2.push(msg));

  const messageId2 = normalizer2.startStream('test-trace-2', 'worker');

  // 模拟一次性收到完整的 assistant 消息
  const fullJson = '{"type":"assistant","message":{"content":[{"type":"text","text":"这是完整的回复"}]}}\n';
  normalizer2.processChunk(messageId2, fullJson);
  const finalMessage2 = normalizer2.endStream(messageId2);

  console.log('最终消息:');
  console.log('  Blocks:', finalMessage2.blocks.length);
  console.log('  Content:', finalMessage2.blocks.map(b => b.content).join(''));

  // 测试3：原始 JSON 字符串（不是 stream-json 格式）
  console.log('\n\n=== 测试3：原始 JSON 字符串 ===\n');

  const normalizer3 = new ClaudeNormalizer({ cli: 'claude', defaultSource: 'worker' });
  const completes3 = [];

  normalizer3.on('complete', (_, msg) => completes3.push(msg));

  const messageId3 = normalizer3.startStream('test-trace-3', 'worker');

  // 这是问题场景：整个 JSON 作为文本
  const rawJsonText = '{"type":"assistant","message":{"content":[{"type":"text","text":"内容"}]}}';
  normalizer3.processChunk(messageId3, rawJsonText);
  const finalMessage3 = normalizer3.endStream(messageId3);

  console.log('最终消息:');
  console.log('  Blocks:', finalMessage3.blocks.length);
  if (finalMessage3.blocks.length > 0) {
    console.log('  Block[0] type:', finalMessage3.blocks[0].type);
    console.log('  Block[0] content:', finalMessage3.blocks[0].content?.slice(0, 100));
  }

  console.log('\n=== 诊断结论 ===');
  if (finalMessage3.blocks.length > 0 && finalMessage3.blocks[0].content?.includes('{"type":"assistant"')) {
    console.log('❌ Normalizer 将原始 JSON 作为文本处理（这就是问题所在）');
    console.log('   前端的 isInternalJsonMessage 会过滤这种内容');
  } else {
    console.log('✅ Normalizer 正确解析了 JSON');
  }
}

run().catch(err => {
  console.error('测试失败:', err);
  process.exit(1);
});
