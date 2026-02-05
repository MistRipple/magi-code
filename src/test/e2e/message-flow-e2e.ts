/**
 * 消息流端对端测试
 *
 * 验证：
 * 1. ASK 模式：意图分类和路由决策不流式输出到 UI，只输出实质性回答
 * 2. TASK 模式：目标分析可以流式输出，任务分配、Worker 输出正确路由
 * 3. 消息结构：所有消息符合 StandardMessage 规范
 */

import { MessageHub, SubTaskView } from '../../orchestrator/core/message-hub';
import { MessageType, MessageCategory, MessageLifecycle, createStandardMessage, type StandardMessage } from '../../protocol/message-protocol';
import { classifyMessage } from '../../ui/webview-svelte/src/lib/message-classifier';
import { resolveDisplayTarget, ROUTING_TABLE } from '../../ui/webview-svelte/src/config/routing-table';
import { MessageCategory as UIMessageCategory } from '../../ui/webview-svelte/src/types/message-routing';

// 测试颜色
const GREEN = '\x1b[32m';
const RED = '\x1b[31m';
const YELLOW = '\x1b[33m';
const BLUE = '\x1b[34m';
const RESET = '\x1b[0m';
const BOLD = '\x1b[1m';

function log(msg: string, color = RESET) {
  console.log(`${color}${msg}${RESET}`);
}

function success(msg: string) {
  log(`✓ ${msg}`, GREEN);
}

function fail(msg: string) {
  log(`✗ ${msg}`, RED);
}

function info(msg: string) {
  log(`ℹ ${msg}`, BLUE);
}

function section(title: string) {
  console.log();
  log(`${BOLD}═══ ${title} ═══${RESET}`, YELLOW);
}

interface TestResult {
  name: string;
  passed: boolean;
  error?: string;
}

const results: TestResult[] = [];

function assert(condition: boolean, name: string, errorMsg?: string): boolean {
  if (condition) {
    success(name);
    results.push({ name, passed: true });
    return true;
  } else {
    fail(`${name}${errorMsg ? ': ' + errorMsg : ''}`);
    results.push({ name, passed: false, error: errorMsg });
    return false;
  }
}

// ============================================================================
// 测试 1: MessageHub 消息发送
// ============================================================================

function testMessageHubBasic() {
  section('测试 1: MessageHub 基础消息发送');

  const hub = new MessageHub();
  const collectedMessages: StandardMessage[] = [];

  // 订阅消息
  hub.on('unified:message', (msg: StandardMessage) => {
    collectedMessages.push(msg);
  });

  // 测试 1.1: orchestratorMessage
  hub.orchestratorMessage('这是一条编排者消息', {
    metadata: { phase: 'test' }
  });

  assert(
    collectedMessages.length === 1,
    'orchestratorMessage 发送成功',
    `期望 1 条消息，实际 ${collectedMessages.length}`
  );

  const msg1 = collectedMessages[0];
  assert(
    msg1.source === 'orchestrator',
    'orchestratorMessage.source = orchestrator'
  );
  assert(
    msg1.agent === 'orchestrator',
    'orchestratorMessage.agent = orchestrator'
  );
  assert(
    msg1.type === MessageType.TEXT,
    `orchestratorMessage.type = TEXT (实际: ${msg1.type})`
  );
  assert(
    msg1.category === MessageCategory.CONTENT,
    `orchestratorMessage.category = CONTENT`
  );

  // 测试 1.2: workerOutput
  hub.workerOutput('claude', 'Worker 输出内容');

  const msg2 = collectedMessages[1];
  assert(
    msg2.source === 'worker',
    'workerOutput.source = worker'
  );
  assert(
    msg2.agent === 'claude',
    'workerOutput.agent = claude'
  );

  // 测试 1.3: result
  hub.result('最终结果内容', { success: true });

  const msg3 = collectedMessages[2];
  assert(
    msg3.type === MessageType.RESULT,
    `result.type = RESULT (实际: ${msg3.type})`
  );

  // 测试 1.4: subTaskCard
  hub.subTaskCard({
    id: 'task-1',
    title: '测试任务',
    status: 'running',
    worker: 'claude'
  });

  const msg4 = collectedMessages[3];
  assert(
    msg4.type === MessageType.TASK_CARD,
    `subTaskCard.type = TASK_CARD (实际: ${msg4.type})`
  );

  // 测试 1.5: workerInstruction
  hub.workerInstruction('gemini', '这是任务说明');

  const msg5 = collectedMessages[4];
  assert(
    msg5.type === MessageType.INSTRUCTION,
    `workerInstruction.type = INSTRUCTION (实际: ${msg5.type})`
  );
  assert(
    msg5.agent === 'gemini',
    'workerInstruction.agent = gemini'
  );

  hub.dispose();
}

// ============================================================================
// 测试 2: 消息分类器
// ============================================================================

function testMessageClassifier() {
  section('测试 2: 消息分类器 (message-classifier)');

  // 测试 2.1: USER_INPUT
  const userInput = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.USER_INPUT,
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', content: '你是谁' }],
    metadata: {}
  });

  const userResult = classifyMessage(userInput);
  assert(
    userResult.category === UIMessageCategory.USER_INPUT,
    'USER_INPUT 正确分类'
  );

  // 测试 2.2: TASK_CARD
  const taskCard = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.TASK_CARD,
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', content: 'Claude 正在处理：分析依赖' }],
    metadata: { worker: 'claude' }
  });

  const taskCardResult = classifyMessage(taskCard);
  assert(
    taskCardResult.category === UIMessageCategory.TASK_SUMMARY_CARD,
    'TASK_CARD 正确分类为 TASK_SUMMARY_CARD'
  );
  assert(
    taskCardResult.worker === 'claude',
    'TASK_CARD worker 正确解析'
  );

  // 测试 2.3: INSTRUCTION
  const instruction = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.INSTRUCTION,
    source: 'orchestrator',
    agent: 'gemini',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', content: '请分析这个文件' }],
    metadata: { worker: 'gemini' }
  });

  const instructionResult = classifyMessage(instruction);
  assert(
    instructionResult.category === UIMessageCategory.WORKER_INSTRUCTION,
    'INSTRUCTION 正确分类为 WORKER_INSTRUCTION'
  );

  // 测试 2.4: Worker TEXT (无代码块)
  const workerText = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.TEXT,
    source: 'worker',
    agent: 'claude',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', content: '分析完成' }],
    metadata: {}
  });

  const workerTextResult = classifyMessage(workerText);
  assert(
    workerTextResult.category === UIMessageCategory.WORKER_OUTPUT,
    'Worker TEXT 正确分类为 WORKER_OUTPUT'
  );

  // 测试 2.5: Worker TEXT (有代码块)
  const workerCode = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.TEXT,
    source: 'worker',
    agent: 'codex',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [
      { type: 'text', content: '修改如下：' },
      { type: 'code', content: 'const x = 1;', language: 'typescript' }
    ],
    metadata: {}
  });

  const workerCodeResult = classifyMessage(workerCode);
  assert(
    workerCodeResult.category === UIMessageCategory.WORKER_CODE,
    'Worker TEXT+CODE 正确分类为 WORKER_CODE'
  );

  // 测试 2.6: Worker THINKING
  const workerThinking = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.THINKING,
    source: 'worker',
    agent: 'claude',
    lifecycle: MessageLifecycle.STREAMING,
    blocks: [{ type: 'thinking', content: '让我分析一下...' }],
    metadata: {}
  });

  const workerThinkingResult = classifyMessage(workerThinking);
  assert(
    workerThinkingResult.category === UIMessageCategory.WORKER_THINKING,
    'Worker THINKING 正确分类为 WORKER_THINKING'
  );

  // 测试 2.7: Worker TOOL_CALL
  const workerToolCall = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.TOOL_CALL,
    source: 'worker',
    agent: 'claude',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'tool_call', toolName: 'readFile', toolId: 'call-1', status: 'pending' }],
    metadata: {}
  });

  const workerToolCallResult = classifyMessage(workerToolCall);
  assert(
    workerToolCallResult.category === UIMessageCategory.WORKER_TOOL_USE,
    'Worker TOOL_CALL 正确分类为 WORKER_TOOL_USE'
  );

  // 测试 2.8: Worker ERROR
  const workerError = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.ERROR,
    source: 'worker',
    agent: 'gemini',
    lifecycle: MessageLifecycle.FAILED,
    blocks: [{ type: 'text', content: '执行失败' }],
    metadata: {}
  });

  const workerErrorResult = classifyMessage(workerError);
  assert(
    workerErrorResult.category === UIMessageCategory.SYSTEM_ERROR,
    'Worker ERROR 正确分类为 SYSTEM_ERROR (显示在主对话区)'
  );

  // 测试 2.9: Orchestrator PLAN
  const orchestratorPlan = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.PLAN,
    source: 'orchestrator',
    agent: 'orchestrator',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'plan', goal: '执行计划...' }],
    metadata: {}
  });

  const orchestratorPlanResult = classifyMessage(orchestratorPlan);
  assert(
    orchestratorPlanResult.category === UIMessageCategory.ORCHESTRATOR_PLAN,
    'Orchestrator PLAN 正确分类为 ORCHESTRATOR_PLAN'
  );

  // 测试 2.10: Worker RESULT
  const workerResult = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.RESULT,
    source: 'worker',
    agent: 'claude',
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', content: '任务完成' }],
    metadata: {}
  });

  const workerResultClassified = classifyMessage(workerResult);
  assert(
    workerResultClassified.category === UIMessageCategory.WORKER_SUMMARY,
    'Worker RESULT 正确分类为 WORKER_SUMMARY'
  );
}

// ============================================================================
// 测试 3: 路由表
// ============================================================================

function testRoutingTable() {
  section('测试 3: 路由表 (routing-table)');

  // 测试 3.1: 主对话区消息
  const threadCategories = [
    UIMessageCategory.USER_INPUT,
    UIMessageCategory.ORCHESTRATOR_RESPONSE,
    UIMessageCategory.ORCHESTRATOR_PLAN,
    UIMessageCategory.ORCHESTRATOR_SUMMARY,
    UIMessageCategory.SYSTEM_NOTICE,
    UIMessageCategory.SYSTEM_ERROR,
    UIMessageCategory.INTERACTION_CONFIRMATION,
    UIMessageCategory.INTERACTION_QUESTION,
    UIMessageCategory.TASK_SUMMARY_CARD,
  ];

  for (const category of threadCategories) {
    const target = resolveDisplayTarget(category);
    assert(
      target.location === 'thread',
      `${category} → thread`
    );
  }

  // 测试 3.2: Worker 面板消息（需要 worker 参数）
  const workerCategories = [
    UIMessageCategory.WORKER_INSTRUCTION,
    UIMessageCategory.WORKER_THINKING,
    UIMessageCategory.WORKER_OUTPUT,
    UIMessageCategory.WORKER_TOOL_USE,
    UIMessageCategory.WORKER_CODE,
    UIMessageCategory.WORKER_SUMMARY,
  ];

  for (const category of workerCategories) {
    // 无 worker 参数时，应该 fallback 到 thread
    const targetNoWorker = resolveDisplayTarget(category);
    assert(
      targetNoWorker.location === 'thread',
      `${category} (无worker) → thread`
    );

    // 有 worker 参数时，应该路由到 worker
    const targetWithWorker = resolveDisplayTarget(category, 'claude');
    assert(
      targetWithWorker.location === 'worker' && (targetWithWorker as any).worker === 'claude',
      `${category} (worker=claude) → worker:claude`
    );
  }

  // 测试 3.3: SYSTEM_PHASE 应该不显示
  const phaseTarget = resolveDisplayTarget(UIMessageCategory.SYSTEM_PHASE);
  assert(
    phaseTarget.location === 'none',
    'SYSTEM_PHASE → none (不显示)'
  );
}

// ============================================================================
// 测试 4: 完整消息流程（模拟）
// ============================================================================

function testMessageFlow() {
  section('测试 4: 完整消息流程模拟');

  const hub = new MessageHub();
  const messages: StandardMessage[] = [];

  // 收集消息
  hub.on('unified:message', (msg: StandardMessage) => {
    messages.push(msg);
  });

  // 模拟 ASK 模式流程
  info('模拟 ASK 模式：用户问"你是谁"');

  // Step 1: 编排者直接回答（不应该有意图分类输出）
  hub.result('我是 MultiCLI 智能编排者（Orchestrator），一个 VSCode 插件中的 AI 助手核心。我可以调度多个专业 Worker 协作完成任务，使用系统工具，并负责任务编排。', {
    metadata: { intent: 'ask' }
  });

  assert(
    messages.length === 1,
    'ASK 模式只输出一条实质性回答'
  );

  const askResponse = messages[0];
  assert(
    askResponse.type === MessageType.RESULT,
    'ASK 模式响应类型为 RESULT'
  );
  assert(
    askResponse.source === 'orchestrator',
    'ASK 模式响应来源为 orchestrator'
  );

  // 验证分类结果
  const askClassified = classifyMessage(askResponse);
  assert(
    askClassified.category === UIMessageCategory.ORCHESTRATOR_SUMMARY,
    'ASK 模式响应分类为 ORCHESTRATOR_SUMMARY'
  );

  // 验证路由
  const askTarget = resolveDisplayTarget(askClassified.category);
  assert(
    askTarget.location === 'thread',
    'ASK 模式响应路由到主对话区'
  );

  // 清空消息
  messages.length = 0;

  // 模拟 TASK 模式流程
  info('模拟 TASK 模式：用户请求"帮我分析这个文件"');

  // Step 1: 任务分配宣告
  hub.taskAssignment([
    { worker: 'claude', shortTitle: '分析文件结构' }
  ]);

  // Step 2: 发送任务说明到 Worker
  hub.workerInstruction('claude', '请分析以下文件的结构和内容...');

  // Step 3: 子任务卡片 (running)
  hub.subTaskCard({
    id: 'task-1',
    title: '分析文件结构',
    status: 'running',
    worker: 'claude'
  });

  // Step 4: Worker 输出
  hub.workerOutput('claude', '正在读取文件内容...');

  // Step 5: 子任务卡片 (completed)
  hub.subTaskCard({
    id: 'task-1',
    title: '分析文件结构',
    status: 'completed',
    worker: 'claude',
    summary: '文件分析完成，发现 3 个函数'
  });

  // Step 6: 最终结果
  hub.result('文件分析已完成。该文件包含 3 个函数，分别是...', {
    success: true
  });

  assert(
    messages.length === 6,
    `TASK 模式产生 6 条消息 (实际: ${messages.length})`
  );

  // 验证消息类型分布
  const messageTypes = messages.map(m => m.type);
  info(`消息类型: ${messageTypes.join(', ')}`);

  // 验证路由分布
  for (const msg of messages) {
    const classified = classifyMessage(msg);
    const target = resolveDisplayTarget(classified.category, classified.worker);
    info(`[${msg.type}] source=${msg.source}, agent=${msg.agent} → ${target.location}${(target as any).worker ? ':' + (target as any).worker : ''}`);
  }

  hub.dispose();
}

// ============================================================================
// 测试 5: 边界情况
// ============================================================================

function testEdgeCases() {
  section('测试 5: 边界情况');

  const hub = new MessageHub();
  const messages: StandardMessage[] = [];

  hub.on('unified:message', (msg: StandardMessage) => {
    messages.push(msg);
  });

  // 测试 5.1: 空内容消息应该被过滤
  hub.progress('test', '');  // 空内容
  hub.result('');            // 空内容
  hub.systemNotice('');      // 空内容

  assert(
    messages.length === 0,
    '空内容消息被正确过滤'
  );

  // 测试 5.2: workerOutput 允许空内容（流式占位）
  hub.workerOutput('claude', '');
  assert(
    messages.length === 1,
    'workerOutput 允许空内容（用于流式占位）'
  );

  messages.length = 0;

  // 测试 5.3: 无效 worker 的 INSTRUCTION
  const invalidWorkerInstruction = createStandardMessage({
    traceId: 'test',
    category: MessageCategory.CONTENT,
    type: MessageType.INSTRUCTION,
    source: 'orchestrator',
    agent: 'orchestrator',  // 无效的 worker
    lifecycle: MessageLifecycle.COMPLETED,
    blocks: [{ type: 'text', content: '任务说明' }],
    metadata: { worker: 'invalid_worker' }  // 无效
  });

  const invalidResult = classifyMessage(invalidWorkerInstruction);
  assert(
    invalidResult.worker === undefined,
    '无效 worker 被正确处理 (worker=undefined)'
  );

  hub.dispose();
}

// ============================================================================
// 主函数
// ============================================================================

async function main() {
  console.log();
  log(`${BOLD}======================================${RESET}`);
  log(`${BOLD}  消息流端对端测试${RESET}`);
  log(`${BOLD}======================================${RESET}`);

  try {
    testMessageHubBasic();
    testMessageClassifier();
    testRoutingTable();
    testMessageFlow();
    testEdgeCases();
  } catch (error) {
    console.error('测试执行出错:', error);
    process.exit(1);
  }

  // 汇总结果
  section('测试结果汇总');

  const passed = results.filter(r => r.passed).length;
  const failed = results.filter(r => !r.passed).length;
  const total = results.length;

  console.log();
  log(`总计: ${total} 个测试`, BOLD);
  log(`通过: ${passed}`, GREEN);
  if (failed > 0) {
    log(`失败: ${failed}`, RED);
    console.log();
    log('失败的测试:', RED);
    for (const r of results.filter(r => !r.passed)) {
      log(`  - ${r.name}: ${r.error || '未知错误'}`, RED);
    }
    process.exit(1);
  } else {
    console.log();
    log('✓ 所有测试通过!', GREEN + BOLD);
    process.exit(0);
  }
}

main().catch(console.error);
