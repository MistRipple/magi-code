/**
 * 前端消息过滤诊断
 * 检测消息是否会被 UI 过滤为"内部 JSON"
 */

const path = require('path');

// 模拟 isInternalJsonMessage 函数（来自 UI webview）
function isInternalJsonMessage(content) {
  if (!content || typeof content !== 'string') return false;
  const trimmed = content.trim();
  if (!trimmed.startsWith('{') || !trimmed.endsWith('}')) return false;
  try {
    const parsed = JSON.parse(trimmed);
    if (!parsed || typeof parsed !== 'object') return false;
    const hasAmbiguity = Object.prototype.hasOwnProperty.call(parsed, 'score')
      && Object.prototype.hasOwnProperty.call(parsed, 'isAmbiguous');
    const hasReview = Object.prototype.hasOwnProperty.call(parsed, 'status')
      && Object.prototype.hasOwnProperty.call(parsed, 'summary');
    const hasMessageEnvelope = typeof parsed.type === 'string'
      && parsed.message && (parsed.message.role || parsed.message.content);
    const hasToolTrace = Array.isArray(parsed.message?.content)
      && parsed.message.content.some(item =>
        item && typeof item === 'object' &&
        (item.type === 'tool_result' || item.type === 'tool_use' || item.tool_use_id)
      );
    const hasToolUseRefs = Object.prototype.hasOwnProperty.call(parsed, 'tool_use_id')
      || Object.prototype.hasOwnProperty.call(parsed, 'parent_tool_use_id');
    return hasAmbiguity || hasReview || hasMessageEnvelope || hasToolTrace || hasToolUseRefs;
  } catch {
    return false;
  }
}

async function run() {
  const outDir = path.join(__dirname, '../out');
  const { CLIAdapterFactory } = require(path.join(outDir, 'cli/adapter-factory'));

  const cwd = process.cwd();
  const prompt = process.argv.slice(2).join(' ') || '你好';

  console.log('=== 前端消息过滤诊断 ===');
  console.log(`测试消息: ${prompt}`);
  console.log('');

  const factory = new CLIAdapterFactory({ cwd });
  const messagesReceived = [];
  const filteredMessages = [];

  factory.on('standardMessage', (message) => {
    const textBlocks = (message.blocks || []).filter(b => b.type === 'text');
    const textContent = textBlocks.map(b => b.content || '').join('\n');

    const wouldBeFiltered = message.source === 'orchestrator' &&
                           isInternalJsonMessage(textContent) &&
                           message.type !== 'plan';

    const info = {
      id: message.id,
      source: message.source,
      type: message.type,
      lifecycle: message.lifecycle,
      blocksCount: message.blocks?.length || 0,
      textContentLength: textContent.length,
      textContentPreview: textContent.slice(0, 100),
      wouldBeFiltered,
    };

    messagesReceived.push(info);

    if (wouldBeFiltered) {
      filteredMessages.push({
        ...info,
        fullContent: textContent.slice(0, 300),
        filterReason: getFilterReason(textContent),
      });
    }

    console.log(`[${wouldBeFiltered ? '❌ FILTERED' : '✅ PASSED'}] ${message.id}`);
    console.log(`  source=${message.source}, type=${message.type}`);
    console.log(`  content preview: ${textContent.slice(0, 80)}`);
    console.log('');
  });

  factory.on('standardComplete', (message) => {
    const textBlocks = (message.blocks || []).filter(b => b.type === 'text');
    const textContent = textBlocks.map(b => b.content || '').join('\n');

    console.log(`[COMPLETE] ${message.id}`);
    console.log(`  source=${message.source}, blocks=${message.blocks?.length || 0}`);
    console.log(`  content length: ${textContent.length}`);
  });

  // 检查可用性
  const availability = await factory.checkAllAvailability();
  if (!availability.claude) {
    console.error('Claude 不可用');
    process.exit(1);
  }

  // 创建适配器
  const adapter = factory.create('claude');
  await adapter.connect();

  console.log('--- 发送消息 ---\n');

  try {
    const response = await adapter.sendMessage(prompt);
    console.log('\n--- 响应 ---');
    console.log(`长度: ${response.content.length}`);
    console.log(`预览: ${response.content.slice(0, 200)}`);
  } catch (err) {
    console.error('错误:', err.message);
  }

  await new Promise(r => setTimeout(r, 500));

  console.log('\n=== 统计 ===');
  console.log(`接收消息: ${messagesReceived.length}`);
  console.log(`被过滤消息: ${filteredMessages.length}`);

  if (filteredMessages.length > 0) {
    console.log('\n=== 被过滤的消息详情 ===');
    for (const msg of filteredMessages) {
      console.log(`ID: ${msg.id}`);
      console.log(`原因: ${msg.filterReason}`);
      console.log(`内容: ${msg.fullContent}`);
      console.log('---');
    }
  }

  // 检查是否有消息被错误过滤
  const workerMessages = messagesReceived.filter(m => m.source === 'worker');
  const orchestratorMessages = messagesReceived.filter(m => m.source === 'orchestrator');

  console.log('\n=== 消息来源分布 ===');
  console.log(`Worker 消息: ${workerMessages.length}`);
  console.log(`Orchestrator 消息: ${orchestratorMessages.length}`);

  // Worker 消息不应该被过滤
  const workerFiltered = workerMessages.filter(m => isInternalJsonMessage(m.textContentPreview + '...'));
  if (workerFiltered.length > 0) {
    console.log('\n⚠️ Worker 消息内容看起来像 JSON，但不会被过滤（因为不是 orchestrator）');
  }

  await factory.disconnectAll().catch(() => {});
  process.exit(0);
}

function getFilterReason(content) {
  try {
    const parsed = JSON.parse(content.trim());
    if (parsed.score !== undefined && parsed.isAmbiguous !== undefined) return 'hasAmbiguity';
    if (parsed.status !== undefined && parsed.summary !== undefined) return 'hasReview';
    if (typeof parsed.type === 'string' && parsed.message) return 'hasMessageEnvelope';
    if (parsed.tool_use_id || parsed.parent_tool_use_id) return 'hasToolUseRefs';
    return 'unknown';
  } catch {
    return 'invalid JSON';
  }
}

run().catch(err => {
  console.error('诊断失败:', err);
  process.exit(1);
});
