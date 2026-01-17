/**
 * 真实 CLI 适配器 + Normalizer 流测试
 * 目标：在不打开 GUI 的情况下，模拟真实环境并捕获标准消息
 *
 * 用法:
 *   node scripts/test-real-cli-flow.js "你可以做什么"
 *
 * 注意：
 * - 需要本机安装对应 CLI（claude/codex/gemini）
 * - 默认使用 claude orchestrator
 */

const path = require('path');
const { CLIAdapterFactory } = require('../out/cli/adapter-factory');

const input = process.argv.slice(2).join(' ') || '你可以做什么';
const cwd = process.cwd();

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
    if (!key || key.length < 30) return;
    if (/^[-*]{3,}$/.test(key)) return;
    if (/^`{3,}/.test(key)) return;
    if (seen.has(key)) {
      duplicates.push({ block, firstIndex: seen.get(key), dupIndex: idx });
    } else {
      seen.set(key, idx);
    }
  });
  return duplicates;
}

async function run() {
  const factory = new CLIAdapterFactory({ cwd });
  const rawChunks = [];
  if (factory.sessionManager && typeof factory.sessionManager.on === 'function') {
    factory.sessionManager.on('output', ({ cli, role, chunk }) => {
      rawChunks.push({ cli, role, chunk });
    });
    factory.sessionManager.on('sessionEvent', (event) => {
      rawChunks.push({ event });
    });
  }

  const availability = await factory.checkAllAvailability();
  if (!availability.claude) {
    console.error('Claude CLI 未安装或不可用，无法运行真实适配器测试。');
    process.exit(1);
  }

  const standardMessages = [];
  const standardUpdates = [];
  const standardCompletes = [];

  factory.on('standardMessage', (msg) => standardMessages.push(msg));
  factory.on('standardUpdate', (update) => standardUpdates.push(update));
  factory.on('standardComplete', (msg) => standardCompletes.push(msg));

  console.log('=== 真实 CLI 测试 ===');
  console.log(`输入: ${input}`);
  console.log('');

  const done = new Promise((resolve) => {
    factory.on('standardComplete', () => resolve('complete'));
  });

  const sendPromise = factory.sendMessage('claude', input, undefined, {
    source: 'orchestrator',
    streamToUI: true,
    adapterRole: 'orchestrator',
    messageMeta: { intent: 'ask' },
  }).catch((error) => {
    console.error('CLI 调用失败:', error);
  });

  const timeoutMs = 45000;
  const timeout = new Promise((resolve) => {
    setTimeout(() => resolve('timeout'), timeoutMs);
  });

  const reason = await Promise.race([done, timeout]);
  if (reason === 'timeout') {
    console.warn(`等待超时(${timeoutMs}ms)，继续输出已有结果。`);
  }

  await sendPromise;
  await factory.disconnectAll().catch(() => {});

  console.log(`standardMessage: ${standardMessages.length}`);
  console.log(`standardUpdate: ${standardUpdates.length}`);
  console.log(`standardComplete: ${standardCompletes.length}`);
  console.log('');

  const final = standardCompletes[standardCompletes.length - 1];
  if (!final) {
    console.log('未收到 complete 消息');
    process.exit(0);
  }

  const content = extractTextFromBlocks(final.blocks);
  console.log('=== 最终内容（前 800 字） ===');
  console.log(content.slice(0, 800));
  console.log('');
  console.log('=== Final Message Debug ===');
  console.log(JSON.stringify({
    id: final.id,
    type: final.type,
    source: final.source,
    cli: final.cli,
    lifecycle: final.lifecycle,
    blocksCount: Array.isArray(final.blocks) ? final.blocks.length : 0,
    blockTypes: Array.isArray(final.blocks) ? final.blocks.map(b => b.type) : [],
    metadata: final.metadata,
  }, null, 2));
  console.log('');
  if (!content && rawChunks.length > 0) {
    console.log('=== Raw CLI Output (last 5 chunks) ===');
    rawChunks.slice(-5).forEach((entry, idx) => {
      const label = entry.event ? 'event' : `${entry.cli}/${entry.role}`;
      const snippet = entry.event ? JSON.stringify(entry.event) : String(entry.chunk);
      console.log(`${idx + 1}. [${label}] ${snippet.slice(0, 200)}`);
    });
    console.log('');
  }

  const dups = detectDuplicateBlocks(content);
  if (!dups.length) {
    console.log('未检测到重复段落');
  } else {
    console.log(`检测到重复段落: ${dups.length} 处`);
    dups.slice(0, 5).forEach((d, idx) => {
      console.log(`${idx + 1}. 重复段落（首次索引: ${d.firstIndex + 1} -> 重复索引: ${d.dupIndex + 1}）`);
      console.log(d.block.split('\n')[0] + '...');
    });
  }
}

run();
