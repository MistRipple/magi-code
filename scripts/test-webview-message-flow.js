/**
 * 模拟用户 -> CLI -> 编排者 -> Webview 的消息流
 * 重点验证：总结内容是否重复，以及 Webview 侧是否产生重复消息
 */

const { parseContentToBlocks } = require('../out/utils/content-parser.js');

const now = Date.now();

function buildStandardMessage(params) {
  return {
    id: params.id,
    traceId: params.traceId || `trace-${Date.now()}`,
    type: params.type || 'text',
    source: params.source || 'orchestrator',
    cli: params.cli || 'claude',
    timestamp: params.timestamp || Date.now(),
    updatedAt: Date.now(),
    blocks: params.blocks || [],
    lifecycle: params.lifecycle || 'streaming',
    metadata: params.metadata || {},
  };
}

function extractTextFromBlocks(blocks) {
  return blocks
    .filter(b => b.type === 'text')
    .map(b => b.content)
    .join('\n');
}

function standardToWebviewMessage(message) {
  const blocks = message.blocks || [];
  return {
    role: message.source === 'user' ? 'user' : 'assistant',
    content: extractTextFromBlocks(blocks),
    time: new Date(message.timestamp).toLocaleTimeString().slice(0, 5),
    timestamp: message.timestamp,
    streaming: message.lifecycle === 'streaming',
    source: message.source,
    cli: message.cli,
    parsedBlocks: blocks,
    standardMessageId: message.id,
    lifecycle: message.lifecycle,
    messageType: message.type,
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
  }
}

function normalizeBlockKey(block) {
  return block
    .replace(/^#{1,6}\s*/gm, '')
    .replace(/\*\*/g, '')
    .replace(/\s+/g, ' ')
    .trim();
}

function detectDuplicateBlocks(content) {
  const blocks = content.split(/\n{2,}/).map(b => b.trim()).filter(Boolean);
  const seen = new Map();
  const duplicates = [];
  for (const block of blocks) {
    const key = normalizeBlockKey(block);
    if (seen.has(key)) {
      duplicates.push({ block, firstIndex: seen.get(key) });
    } else {
      seen.set(key, blocks.indexOf(block));
    }
  }
  return duplicates;
}

function runSimulation() {
  const threadMessages = [];
  const standardMessages = new Map();

  // 模拟用户输入
  const userInput = '你可以做什么';
  console.log('=== 用户输入 ===');
  console.log(userInput);
  console.log('');

  // 模拟编排者流式开始
  const messageId = `msg-${now}-1`;
  const started = buildStandardMessage({
    id: messageId,
    lifecycle: 'streaming',
    blocks: [],
  });
  standardMessages.set(messageId, started);
  threadMessages.push(standardToWebviewMessage(started));

  // 模拟 CLI 流式输出
  const summaryText = [
    '执行总结',
    '任务状态: 成功完成',
    '完成工作: 回答了关于编排模式能力的咨询问题，详细说明了编排模式的工作机制和适用场景。',
    '主要内容:',
    '- 解释了编排模式的 5 个核心步骤：需求分析、计划制定、Worker 分配、执行、结果汇总',
    '- 说明了三种 Worker 的专长分工：Claude（复杂架构）、Codex（后端开发）、Gemini（前端 UI）',
    '- 列举了典型应用场景：前后端协作、复杂架构设计、多模块并行开发',
    '',
    '## 执行总结',
    '任务状态: 成功完成',
    '完成工作: 回答了关于编排模式能力的咨询问题，详细说明了编排模式的工作机制和适用场景。',
    '主要内容:',
    '- 解释了编排模式的 5 个核心步骤：需求分析、计划制定、Worker 分配、执行、结果汇总',
    '- 说明了三种 Worker 的专长分工：Claude（复杂架构）、Codex（后端开发）、Gemini（前端 UI）',
    '- 列举了典型应用场景：前后端协作、复杂架构设计、多模块并行开发',
  ].join('\n');

  const update = { messageId, updateType: 'append', appendText: summaryText };
  const message = standardMessages.get(messageId);
  applyUpdateToStandardMessage(message, update);

  // 模拟流式完成（后端重新解析 blocks）
  const parsedBlocks = parseContentToBlocks(summaryText);
  const completed = buildStandardMessage({
    id: messageId,
    lifecycle: 'completed',
    blocks: parsedBlocks,
  });
  standardMessages.set(messageId, completed);

  const finalMsg = standardToWebviewMessage(completed);
  threadMessages[0] = finalMsg;

  console.log('=== CLI 原始响应 ===');
  console.log(summaryText);
  console.log('');

  console.log('=== Webview 渲染前的内容 ===');
  console.log(finalMsg.content);
  console.log('');

  const duplicates = detectDuplicateBlocks(finalMsg.content);
  console.log('=== 重复段落检测 ===');
  if (!duplicates.length) {
    console.log('未检测到重复段落');
  } else {
    duplicates.forEach((d, idx) => {
      console.log(`${idx + 1}. 重复段落（首次位置: ${d.firstIndex + 1}）`);
      console.log(d.block);
      console.log('');
    });
  }
}

runSimulation();
