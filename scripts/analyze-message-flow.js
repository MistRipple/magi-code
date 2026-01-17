/**
 * 分析 message-flow.jsonl，定位重复汇总/重复消息来源
 *
 * 用法:
 *   MULTICLI_MESSAGE_FLOW_LOG=1 启动 VSCode 插件
 *   node scripts/analyze-message-flow.js .multicli/logs/message-flow.jsonl
 */

const fs = require('fs');
const path = require('path');

const inputPath = process.argv[2];
if (!inputPath) {
  console.error('Usage: node scripts/analyze-message-flow.js <path-to-message-flow.jsonl>');
  process.exit(1);
}

const resolvedPath = path.resolve(process.cwd(), inputPath);
if (!fs.existsSync(resolvedPath)) {
  console.error(`File not found: ${resolvedPath}`);
  process.exit(1);
}

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

const lines = fs.readFileSync(resolvedPath, 'utf-8').split(/\r?\n/).filter(Boolean);
const events = lines.map(line => JSON.parse(line));

const messageStates = new Map();
const completedMessages = [];

for (const event of events) {
  const { eventType, payload } = event;
  if (!payload) continue;

  if (eventType === 'standardMessage') {
    const msg = payload;
    const existing = messageStates.get(msg.id) || { updates: '' };
    existing.message = msg;
    messageStates.set(msg.id, existing);
  }

  if (eventType === 'standardUpdate') {
    const update = payload;
    const existing = messageStates.get(update.messageId) || { updates: '' };
    if (update.updateType === 'append' && typeof update.appendText === 'string') {
      existing.updates += update.appendText;
    }
    messageStates.set(update.messageId, existing);
  }

  if (eventType === 'standardComplete') {
    const msg = payload;
    const existing = messageStates.get(msg.id) || { updates: '' };
    existing.complete = msg;
    messageStates.set(msg.id, existing);
    completedMessages.push(msg);
  }
}

console.log(`Loaded events: ${events.length}`);
console.log(`Completed messages: ${completedMessages.length}`);
console.log('');

// 检测单条消息内部重复
let internalDupCount = 0;
for (const msg of completedMessages) {
  const content = extractTextFromBlocks(msg.blocks) || '';
  if (!content) continue;
  const dups = detectDuplicateBlocks(content);
  if (dups.length > 0) {
    internalDupCount += 1;
    console.log(`--- 内部重复: messageId=${msg.id} type=${msg.type}`);
    dups.slice(0, 3).forEach((d, idx) => {
      console.log(`  ${idx + 1}. 重复段落（首次索引: ${d.firstIndex + 1} -> 重复索引: ${d.dupIndex + 1}）`);
      console.log(`     ${d.block.split('\n')[0]}...`);
    });
    console.log('');
  }
}

// 检测跨消息重复（摘要重复）
const summaryBuckets = new Map();
for (const msg of completedMessages) {
  const content = extractTextFromBlocks(msg.blocks) || '';
  if (!content) continue;
  const key = normalizeBlock(content);
  if (!summaryBuckets.has(key)) {
    summaryBuckets.set(key, []);
  }
  summaryBuckets.get(key).push(msg);
}

let crossDupCount = 0;
for (const [key, msgs] of summaryBuckets.entries()) {
  if (msgs.length < 2) continue;
  crossDupCount += 1;
  console.log(`--- 跨消息重复: count=${msgs.length}`);
  msgs.slice(0, 5).forEach((m) => {
    console.log(`  messageId=${m.id} type=${m.type} source=${m.source} cli=${m.cli}`);
  });
  console.log(`  content-head: ${key.slice(0, 120)}...`);
  console.log('');
}

console.log(`内部重复消息数: ${internalDupCount}`);
console.log(`跨消息重复组数: ${crossDupCount}`);
