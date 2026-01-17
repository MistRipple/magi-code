/**
 * 使用 MessageBridge 模拟用户输入 -> CLI 输出 -> 标准消息流
 * 不依赖 VSCode 面板，直接观察标准消息是否重复
 */

const { EventEmitter } = require('events');
const { MessageBridge } = require('../out/normalizer/message-bridge.js');

function extractTextFromBlocks(blocks) {
  if (!Array.isArray(blocks)) return '';
  return blocks
    .filter(b => b && b.type === 'text' && typeof b.content === 'string')
    .map(b => b.content)
    .join('\n');
}

function normalizeBlock(block) {
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
  blocks.forEach((block, idx) => {
    const key = normalizeBlock(block);
    if (seen.has(key)) {
      duplicates.push({ block, firstIndex: seen.get(key), dupIndex: idx });
    } else {
      seen.set(key, idx);
    }
  });
  return duplicates;
}

function run() {
  const factory = new EventEmitter();
  const bridge = new MessageBridge(factory, { debug: false });

  const standardMessages = [];
  const updates = [];
  const completes = [];

  bridge.on('message', (msg) => {
    standardMessages.push(msg);
  });
  bridge.on('update', (update) => {
    updates.push(update);
  });
  bridge.on('complete', (msg) => {
    completes.push(msg);
  });

  const userInput = '你可以做什么';
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

  console.log('=== 模拟用户输入 ===');
  console.log(userInput);
  console.log('');

  // 模拟流式开始
  factory.emit('streamStart', { type: 'claude', source: 'orchestrator', adapterRole: 'orchestrator' });

  // 模拟流式输出
  factory.emit('output', { type: 'claude', chunk: summaryText, source: 'orchestrator', adapterRole: 'orchestrator' });

  // 模拟响应完成
  factory.emit('response', { type: 'claude', response: { content: summaryText }, source: 'orchestrator', adapterRole: 'orchestrator' });

  console.log(`标准消息数量: ${standardMessages.length}`);
  console.log(`更新数量: ${updates.length}`);
  console.log(`完成数量: ${completes.length}`);
  console.log('');

  if (completes.length > 0) {
    const final = completes[completes.length - 1];
    const content = extractTextFromBlocks(final.blocks);
    console.log('=== 最终消息内容（节选） ===');
    console.log(content.slice(0, 400));
    console.log('');

    const dups = detectDuplicateBlocks(content);
    if (!dups.length) {
      console.log('未检测到重复段落');
    } else {
      console.log(`检测到重复段落: ${dups.length} 处`);
      dups.slice(0, 3).forEach((d, idx) => {
        console.log(`${idx + 1}. 重复段落（首次索引: ${d.firstIndex + 1} -> 重复索引: ${d.dupIndex + 1}）`);
        console.log(d.block.split('\n')[0] + '...');
      });
    }
  }
}

run();
