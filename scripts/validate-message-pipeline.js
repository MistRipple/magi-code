#!/usr/bin/env node

/**
 * 消息链路多层级静态验证
 *
 * 验证目标：覆盖设计文档定义的 L1-L6 各层约束
 * 参考文档：
 *   - docs/architecture/message-flow-design.md
 *   - docs/architecture/streaming-output-unified-design.md
 *
 * 层级划分：
 *   L1 生产层：消息 ID 由生产者分配
 *   L2 中枢层：不修改消息 ID，状态机管理、节流去重、事件分发
 *   L3 桥接层：透传到前端
 *   L4 接收层：纯透传
 *   L5 路由层：路由决策、不修改消息内容
 *   L6 渲染层：不做消息过滤、不做路由决策
 */

const fs = require('fs');
const path = require('path');

const root = process.cwd();

// 文件路径映射
const FILES = {
  // L1 生产层
  messageProtocol: path.join(root, 'src/protocol/message-protocol.ts'),
  baseAdapter: path.join(root, 'src/llm/adapters/base-adapter.ts'),
  // L2 中枢层
  messagePipeline: path.join(root, 'src/orchestrator/core/message-pipeline.ts'),
  messageHub: path.join(root, 'src/orchestrator/core/message-hub.ts'),
  // L3 桥接层
  webviewProvider: path.join(root, 'src/ui/webview-provider.ts'),
  // L4 接收层
  vscodeBridge: path.join(root, 'src/ui/webview-svelte/src/lib/vscode-bridge.ts'),
  // L5 路由层
  messageHandler: path.join(root, 'src/ui/webview-svelte/src/lib/message-handler.ts'),
  messageRouter: path.join(root, 'src/ui/webview-svelte/src/lib/message-router.ts'),
  messageClassifier: path.join(root, 'src/ui/webview-svelte/src/lib/message-classifier.ts'),
  routingTable: path.join(root, 'src/ui/webview-svelte/src/config/routing-table.ts'),
  // L6 渲染层
  threadPanel: path.join(root, 'src/ui/webview-svelte/src/components/ThreadPanel.svelte'),
  messageItem: path.join(root, 'src/ui/webview-svelte/src/components/MessageItem.svelte'),
  messageList: path.join(root, 'src/ui/webview-svelte/src/components/MessageList.svelte'),
};

function readFile(filePath) {
  if (!fs.existsSync(filePath)) {
    throw new Error(`文件不存在: ${filePath}`);
  }
  return fs.readFileSync(filePath, 'utf8');
}

// 加载所有文件
const contents = {};
for (const [key, filePath] of Object.entries(FILES)) {
  contents[key] = readFile(filePath);
}

let passed = 0;
let failed = 0;
const failures = [];

function assertRule(layer, file, regex, description) {
  const content = contents[file];
  if (!content) {
    failures.push({ layer, file, description, reason: '文件内容未加载' });
    failed++;
    return;
  }
  if (regex.test(content)) {
    passed++;
  } else {
    failures.push({ layer, file, description, reason: '正则未匹配' });
    failed++;
  }
}

function assertNotMatch(layer, file, regex, description) {
  const content = contents[file];
  if (!content) {
    failures.push({ layer, file, description, reason: '文件内容未加载' });
    failed++;
    return;
  }
  if (!regex.test(content)) {
    passed++;
  } else {
    failures.push({ layer, file, description, reason: '存在禁止的模式' });
    failed++;
  }
}

// ============================================================================
// L1 生产层验证
// ============================================================================

// L1-1: MessageFactory 支持外部注入 ID 或自动生成
assertRule('L1', 'messageProtocol',
  /id:\s*id\s*\|\|\s*generateMessageId\(\)/,
  'createStandardMessage 必须支持外部注入 ID 或自动生成');

// L1-2: generateMessageId 存在且生成唯一 ID
assertRule('L1', 'messageProtocol',
  /function\s+generateMessageId\(\):\s*string/,
  'generateMessageId 工厂函数必须存在');

// L1-3: startStreamWithContext 查询占位符 ID
assertRule('L1', 'baseAdapter',
  /getRequestMessageId\(requestId\)/,
  'startStreamWithContext 必须查询 requestMessageId 获取占位符 ID');

// L1-4: startStreamWithContext 将占位符 ID 传给 normalizer
assertRule('L1', 'baseAdapter',
  /normalizer\.startStream\([^)]*boundMessageId/,
  'startStreamWithContext 必须将占位符 ID 传给 normalizer');

// L1-5: 默认 visibility 为 'user'
assertRule('L1', 'messageProtocol',
  /visibility:\s*'user'/,
  '默认消息可见性必须为 user');

// ============================================================================
// L2 中枢层验证
// ============================================================================

// L2-1: Pipeline 不存在 messageIdAliasMap（禁止 ID 别名映射）
assertNotMatch('L2', 'messagePipeline',
  /messageIdAliasMap/,
  'Pipeline 禁止存在 messageIdAliasMap（ID 别名映射）');

// L2-2: process() 分配 eventId
assertRule('L2', 'messagePipeline',
  /eventId:\s*message\.eventId\s*\|\|\s*this\.generateEventId/,
  'process() 必须为消息分配 eventId');

// L2-3: process() 分配 eventSeq（通过 resolveEventSeq）
assertRule('L2', 'messagePipeline',
  /eventSeq:\s*this\.resolveEventSeq/,
  'process() 必须通过 resolveEventSeq 分配 eventSeq');

// L2-4: resolveEventSeq 保证单调递增
assertRule('L2', 'messagePipeline',
  /if\s*\(\s*normalized\s*>\s*this\.eventSeqCounter\s*\)/,
  'resolveEventSeq 必须保证 eventSeq 单调递增');

// L2-5: processUpdate 封口检查存在
assertRule('L2', 'messagePipeline',
  /sealedCards\.get\(cardId\)/,
  'processUpdate 必须检查 sealedCards 封口状态');

// L2-6: cardStreamSeq 乱序校验存在
assertRule('L2', 'messagePipeline',
  /out_of_order_update/,
  'Pipeline 必须存在 cardStreamSeq 乱序校验');

// L2-7: requestMessageIdMap 仅在占位消息时写入
assertRule('L2', 'messagePipeline',
  /isPlaceholder\s*===?\s*true\)\s*this\.requestMessageIdMap\.set/,
  'requestMessageIdMap 仅在 isPlaceholder===true 时写入');

// L2-8: Pipeline 明确声明不做 ID 映射（注释约束）
assertRule('L2', 'messagePipeline',
  /Pipeline\s*不做\s*ID\s*映射/,
  'Pipeline 必须声明不做 ID 映射');

// L2-9: MessageHub.sendMessage 委托给 pipeline.process
assertRule('L2', 'messageHub',
  /sendMessage\([^)]*\)[^{]*\{[^}]*pipeline\.process\(/s,
  'MessageHub.sendMessage 必须委托给 pipeline.process');

// L2-10: MessageHub.sendUpdate 委托给 pipeline.processUpdate
assertRule('L2', 'messageHub',
  /sendUpdate\([^)]*\)[^{]*\{[^}]*pipeline\.processUpdate\(/s,
  'MessageHub.sendUpdate 必须委托给 pipeline.processUpdate');

// L2-12: dead letter 记录机制存在
assertRule('L2', 'messagePipeline',
  /recordDeadLetter/,
  'Pipeline 必须有 dead letter 记录机制');

// L2-13: validate 校验 source、agent、category 必填
assertRule('L2', 'messagePipeline',
  /missing source\/agent/,
  'Pipeline validate 必须校验 source/agent 必填');

assertRule('L2', 'messagePipeline',
  /missing category/,
  'Pipeline validate 必须校验 category 必填');

// ============================================================================
// L3 桥接层验证
// ============================================================================

// L3-1: unified:message 直接 postMessage
assertRule('L3', 'webviewProvider',
  /on\(\s*'unified:message'\s*,\s*\(message\)\s*=>\s*\{[^}]*postMessage/s,
  'L3 必须监听 unified:message 并直接 postMessage');

// L3-2: unified:update 直接 postMessage
assertRule('L3', 'webviewProvider',
  /on\(\s*'unified:update'\s*,\s*\(update\)\s*=>\s*\{[^}]*postMessage/s,
  'L3 必须监听 unified:update 并直接 postMessage');

// L3-3: unified:complete 直接 postMessage
assertRule('L3', 'webviewProvider',
  /on\(\s*'unified:complete'\s*,\s*\(message\)\s*=>\s*\{[^}]*postMessage/s,
  'L3 必须监听 unified:complete 并直接 postMessage');

// L3-4: L3 不存在 ID 修改（不允许修改 message.id）
assertNotMatch('L3', 'webviewProvider',
  /message\.id\s*=\s*[^=]/,
  'L3 禁止修改 message.id');

// ============================================================================
// L4 接收层验证
// ============================================================================

// L4-1: vscode-bridge 使用 window.addEventListener('message')
assertRule('L4', 'vscodeBridge',
  /window\.addEventListener\(\s*'message'/,
  'L4 必须监听 window message 事件');

// L4-2: listeners.forEach 透传
assertRule('L4', 'vscodeBridge',
  /listeners\.forEach\(\s*\(listener\)\s*=>/,
  'L4 必须将消息透传给所有 listeners');

// L4-3: sanitizeMessage 只做序列化净化
assertRule('L4', 'vscodeBridge',
  /structuredClone\(message\)/,
  'sanitizeMessage 应使用 structuredClone 序列化');

// L4-4: sanitizeMessage 不做业务逻辑过滤
assertNotMatch('L4', 'vscodeBridge',
  /sanitizeMessage[^}]*\.filter\(/s,
  'sanitizeMessage 禁止做业务逻辑过滤');

// ============================================================================
// L5 路由层验证
// ============================================================================

// L5-1: routeStandardMessage 先检查 visibility
assertRule('L5', 'messageRouter',
  /checkVisibility\(standard\)/,
  'routeStandardMessage 必须先检查 visibility');

// L5-2: visibility='system' 路由到 none
assertRule('L5', 'messageRouter',
  /visibility\s*===\s*'system'[^}]*location:\s*'none'/s,
  'visibility=system 必须路由到 none');

// L5-3: visibility='debug' 需要 isDebugMode
assertRule('L5', 'messageRouter',
  /visibility\s*===\s*'debug'\s*&&\s*!isDebugMode\(\)/,
  'visibility=debug 必须检查 isDebugMode()');

// L5-4: ROUTING_TABLE 中 WORKER_* 路由到 worker
assertRule('L5', 'routingTable',
  /WORKER_INSTRUCTION\]:\s*\{\s*location:\s*'worker'\s*\}/,
  'WORKER_INSTRUCTION 必须路由到 worker');

assertRule('L5', 'routingTable',
  /WORKER_OUTPUT\]:\s*\{\s*location:\s*'worker'\s*\}/,
  'WORKER_OUTPUT 必须路由到 worker');

assertRule('L5', 'routingTable',
  /WORKER_THINKING\]:\s*\{\s*location:\s*'worker'\s*\}/,
  'WORKER_THINKING 必须路由到 worker');

assertRule('L5', 'routingTable',
  /WORKER_TOOL_USE\]:\s*\{\s*location:\s*'worker'\s*\}/,
  'WORKER_TOOL_USE 必须路由到 worker');

assertRule('L5', 'routingTable',
  /WORKER_CODE\]:\s*\{\s*location:\s*'worker'\s*\}/,
  'WORKER_CODE 必须路由到 worker');

assertRule('L5', 'routingTable',
  /WORKER_SUMMARY\]:\s*\{\s*location:\s*'worker'\s*\}/,
  'WORKER_SUMMARY 必须路由到 worker');

// L5-5: handleStandardComplete 过滤非 CONTENT 类别
assertRule('L5', 'messageHandler',
  /standard\.category\s*!==\s*MessageCategory\.CONTENT/,
  'handleStandardComplete 必须过滤非 CONTENT 类别');

// L5-6: Worker source 回退值为 'claude'（核心修复验证）
assertRule('L5', 'messageHandler',
  /resolvedWorker\s*\?\?\s*'claude'\)/,
  'Worker source 回退值必须为 claude（非 orchestrator）');

// L5-7: Worker source 回退链不能出现 fallback 到 'orchestrator'
assertNotMatch('L5', 'messageHandler',
  /resolvedWorker\s*\?\?\s*'orchestrator'\)/,
  'Worker source 禁止回退到 orchestrator');

// L5-8: Worker 安全拦截存在
assertRule('L5', 'messageHandler',
  /standard\.source\s*===\s*'worker'/,
  'L5 必须有 Worker 安全拦截逻辑');

// L5-9: Worker 安全拦截允许 ERROR 和 INTERACTION 写入主对话区
assertRule('L5', 'messageHandler',
  /MessageType\.ERROR.*MessageType\.INTERACTION|MessageType\.INTERACTION.*MessageType\.ERROR/,
  'Worker 安全拦截必须允许 ERROR 和 INTERACTION 写入主对话区');

// L5-10: 占位消息必须校验 requestId
assertRule('L5', 'messageHandler',
  /占位消息缺少\s*requestId/,
  '占位消息处理必须校验 requestId');

// L5-11: 占位消息必须校验 userMessageId
assertRule('L5', 'messageHandler',
  /占位消息缺少\s*userMessageId/,
  '占位消息处理必须校验 userMessageId');

// L5-12: 分类器按 source 区分 orchestrator 和 worker
assertRule('L5', 'messageClassifier',
  /standard\.source\s*===\s*'orchestrator'/,
  '分类器必须区分 orchestrator source');

assertRule('L5', 'messageClassifier',
  /standard\.source\s*===\s*'worker'/,
  '分类器必须区分 worker source');

// L5-13: 分类器 normalizeWorkerSlot 验证合法值
assertRule('L5', 'messageClassifier',
  /WORKER_SLOTS\.has\(lower\)/,
  'normalizeWorkerSlot 必须验证 Worker 槽位合法性');

// L5-14: resolveDisplayTarget 对 worker location 需要 worker 参数
assertRule('L5', 'routingTable',
  /if\s*\(worker\)\s*\{[^}]*return\s*\{\s*location:\s*rule\.location,\s*worker\s*\}/s,
  'resolveDisplayTarget 必须在 worker location 时注入 worker 参数');

// L5-15: 非 CONTENT complete 有调试日志
assertRule('L5', 'messageHandler',
  /跳过非\s*CONTENT\s*类别的\s*complete\s*消息/,
  '非 CONTENT complete 必须有调试日志');

// L5-16: 流式更新暂存队列存在（时序保护）
assertRule('L5', 'messageHandler',
  /queueStreamUpdate/,
  'L5 必须有流式更新暂存队列（时序保护）');

// L5-17: 流式更新回放机制存在
assertRule('L5', 'messageHandler',
  /flushPendingStreamUpdates/,
  'L5 必须有流式更新回放机制');

// ============================================================================
// L6 渲染层验证
// ============================================================================

// L6-1: ThreadPanel 不传 readOnly={true}（核心修复验证）
assertNotMatch('L6', 'threadPanel',
  /MessageList\s[^>]*readOnly=\{true\}/,
  'ThreadPanel 禁止传 readOnly={true} 给 MessageList');

// L6-2: ThreadPanel 正确使用 MessageList 组件
assertRule('L6', 'threadPanel',
  /<MessageList\s+\{messages\}\s*\/>/,
  'ThreadPanel 必须正确使用 MessageList 组件');

// L6-3: MessageItem 不存在死代码（task_card 在 else 分支内的重复检查）
assertNotMatch('L6', 'messageItem',
  /\{:else\}[\s\S]*?\{#if\s+message\.type\s*===\s*'task_card'/,
  'MessageItem 禁止在 {:else} 分支内重复检查 task_card');

// L6-4: MessageItem 使用 BlockRenderer 渲染 blocks
assertRule('L6', 'messageItem',
  /<BlockRenderer/,
  'MessageItem 必须使用 BlockRenderer 渲染内容块');

// L6-5: MessageItem 传递 readOnly 给 BlockRenderer
assertRule('L6', 'messageItem',
  /BlockRenderer\s[^>]*\{readOnly\}/,
  'MessageItem 必须将 readOnly 传递给 BlockRenderer');

// L6-6: MessageList 接受 readOnly prop
assertRule('L6', 'messageList',
  /readOnly/,
  'MessageList 必须接受 readOnly prop');

// ============================================================================
// 跨层约束验证
// ============================================================================

// X-1: 前端 messageHandler 使用 MessageCategory 和 MessageType（与后端一致）
assertRule('X-LAYER', 'messageHandler',
  /import.*MessageCategory.*from.*message-protocol/,
  '前端必须使用后端定义的 MessageCategory');

assertRule('X-LAYER', 'messageHandler',
  /import.*MessageType.*from.*message-protocol/,
  '前端必须使用后端定义的 MessageType');

// X-2: 前端路由器使用后端定义的 StandardMessage 类型
assertRule('X-LAYER', 'messageRouter',
  /import.*StandardMessage.*from.*message-protocol/,
  '前端路由器必须使用后端定义的 StandardMessage 类型');

// ============================================================================
// 结果输出
// ============================================================================

console.log('\n' + '='.repeat(60));
console.log('消息链路多层级静态验证');
console.log('='.repeat(60));
console.log(`\n通过: ${passed}`);
console.log(`失败: ${failed}`);
console.log(`总计: ${passed + failed}`);

if (failures.length > 0) {
  console.log('\n--- 失败项 ---');
  for (const f of failures) {
    console.error(`[${f.layer}] ${f.file}: ${f.description}`);
    console.error(`  原因: ${f.reason}`);
  }
  console.log('');
  process.exit(1);
} else {
  console.log('\n消息链路多层级静态验证全部通过\n');
}
