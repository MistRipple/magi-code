/**
 * 消息格式端对端验证测试
 *
 * 目标：验证从 CLI 响应 -> Normalizer -> 前端的数据格式是否正确
 *
 * 测试内容：
 * 1. StandardMessage 结构完整性
 * 2. ContentBlock 各种类型的数据格式
 * 3. 流式更新的数据格式
 * 4. 交互消息的数据格式
 */

const path = require('path');

// 颜色输出
const colors = {
  reset: '\x1b[0m',
  bright: '\x1b[1m',
  red: '\x1b[31m',
  green: '\x1b[32m',
  yellow: '\x1b[33m',
  blue: '\x1b[34m',
  cyan: '\x1b[36m',
};

function log(message, color = colors.reset) {
  console.log(`${color}${message}${colors.reset}`);
}

function logSection(title) {
  console.log('\n' + '='.repeat(80));
  log(`  ${title}`, colors.cyan + colors.bright);
  console.log('='.repeat(80) + '\n');
}

function logTest(name, passed, details = null) {
  const status = passed ? 'PASS' : 'FAIL';
  const icon = passed ? '[OK]' : '[X]';
  log(`${icon} ${name}`, passed ? colors.green : colors.red);
  if (details && !passed) {
    log(`    Details: ${details}`, colors.yellow);
  }
}

// 测试统计
const stats = { total: 0, passed: 0, failed: 0 };

function recordTest(passed) {
  stats.total++;
  if (passed) stats.passed++;
  else stats.failed++;
}

// 模拟前端期望的数据结构验证函数
function validateStandardMessage(message) {
  const errors = [];

  // 必需字段检查
  if (!message.id || typeof message.id !== 'string') {
    errors.push('id: 必须是非空字符串');
  }
  if (!message.traceId || typeof message.traceId !== 'string') {
    errors.push('traceId: 必须是非空字符串');
  }
  if (!message.type || typeof message.type !== 'string') {
    errors.push('type: 必须是非空字符串');
  }
  if (!message.source || !['orchestrator', 'worker'].includes(message.source)) {
    errors.push('source: 必须是 orchestrator 或 worker');
  }
  if (!message.cli || typeof message.cli !== 'string') {
    errors.push('cli: 必须是非空字符串');
  }
  if (!message.lifecycle || typeof message.lifecycle !== 'string') {
    errors.push('lifecycle: 必须是非空字符串');
  }
  if (!Array.isArray(message.blocks)) {
    errors.push('blocks: 必须是数组');
  }
  if (typeof message.timestamp !== 'number') {
    errors.push('timestamp: 必须是数字');
  }
  if (typeof message.updatedAt !== 'number') {
    errors.push('updatedAt: 必须是数字');
  }
  if (!message.metadata || typeof message.metadata !== 'object') {
    errors.push('metadata: 必须是对象');
  }

  return { valid: errors.length === 0, errors };
}

// 验证各种 ContentBlock 类型
function validateContentBlock(block) {
  const errors = [];

  if (!block.type) {
    errors.push('type: 必须存在');
    return { valid: false, errors };
  }

  switch (block.type) {
    case 'text':
      if (typeof block.content !== 'string') {
        errors.push('TextBlock.content: 必须是字符串');
      }
      // isMarkdown 是可选的
      break;

    case 'code':
      if (typeof block.content !== 'string') {
        errors.push('CodeBlock.content: 必须是字符串');
      }
      if (typeof block.language !== 'string') {
        errors.push('CodeBlock.language: 必须是字符串');
      }
      // filename, highlightLines, isEmbedded 是可选的
      break;

    case 'thinking':
      if (typeof block.content !== 'string') {
        errors.push('ThinkingBlock.content: 必须是字符串');
      }
      // summary, blockId 是可选的
      break;

    case 'tool_call':
      if (typeof block.toolName !== 'string') {
        errors.push('ToolCallBlock.toolName: 必须是字符串');
      }
      if (typeof block.toolId !== 'string') {
        errors.push('ToolCallBlock.toolId: 必须是字符串');
      }
      if (!['pending', 'running', 'completed', 'failed'].includes(block.status)) {
        errors.push('ToolCallBlock.status: 必须是有效状态');
      }
      // input, output, error, duration 是可选的
      break;

    case 'file_change':
      if (typeof block.filePath !== 'string') {
        errors.push('FileChangeBlock.filePath: 必须是字符串');
      }
      if (!['create', 'modify', 'delete'].includes(block.changeType)) {
        errors.push('FileChangeBlock.changeType: 必须是有效类型');
      }
      break;

    default:
      errors.push(`未知的 block 类型: ${block.type}`);
  }

  return { valid: errors.length === 0, errors };
}

// 验证 StreamUpdate 结构
function validateStreamUpdate(update) {
  const errors = [];

  if (!update.messageId || typeof update.messageId !== 'string') {
    errors.push('messageId: 必须是非空字符串');
  }
  if (!['append', 'replace', 'block_update', 'lifecycle_change'].includes(update.updateType)) {
    errors.push('updateType: 必须是有效类型');
  }
  if (typeof update.timestamp !== 'number') {
    errors.push('timestamp: 必须是数字');
  }

  // 根据 updateType 验证特定字段
  if (update.updateType === 'append' && typeof update.appendText !== 'string') {
    errors.push('append 类型必须有 appendText 字符串');
  }
  if ((update.updateType === 'replace' || update.updateType === 'block_update') && !Array.isArray(update.blocks)) {
    errors.push('replace/block_update 类型必须有 blocks 数组');
  }
  if (update.updateType === 'lifecycle_change' && typeof update.lifecycle !== 'string') {
    errors.push('lifecycle_change 类型必须有 lifecycle 字符串');
  }

  return { valid: errors.length === 0, errors };
}

// 验证 InteractionRequest 结构
function validateInteractionRequest(interaction) {
  const errors = [];

  if (!interaction.type || typeof interaction.type !== 'string') {
    errors.push('type: 必须是非空字符串');
  }
  if (!interaction.requestId || typeof interaction.requestId !== 'string') {
    errors.push('requestId: 必须是非空字符串');
  }
  if (!interaction.prompt || typeof interaction.prompt !== 'string') {
    errors.push('prompt: 必须是非空字符串');
  }
  if (typeof interaction.required !== 'boolean') {
    errors.push('required: 必须是布尔值');
  }

  // 验证 options 数组结构
  if (interaction.options) {
    if (!Array.isArray(interaction.options)) {
      errors.push('options: 必须是数组');
    } else {
      interaction.options.forEach((opt, idx) => {
        if (typeof opt.value !== 'string') {
          errors.push(`options[${idx}].value: 必须是字符串`);
        }
        if (typeof opt.label !== 'string') {
          errors.push(`options[${idx}].label: 必须是字符串`);
        }
      });
    }
  }

  return { valid: errors.length === 0, errors };
}

// 测试 1: 基础消息结构验证
async function testBasicMessageStructure() {
  logSection('测试 1: 基础消息结构验证');

  try {
    const { createStandardMessage, createTextMessage, createStreamingMessage, createErrorMessage, MessageType, MessageLifecycle } = require('../out/protocol/message-protocol');

    // 测试 createStandardMessage
    const msg1 = createStandardMessage({
      traceId: 'trace-1',
      type: MessageType.TEXT,
      source: 'worker',
      cli: 'claude',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [{ type: 'text', content: 'Hello', isMarkdown: true }],
      metadata: {},
    });

    let result = validateStandardMessage(msg1);
    logTest('createStandardMessage 生成有效结构', result.valid, result.errors.join(', '));
    recordTest(result.valid);

    // 测试 createTextMessage
    const msg2 = createTextMessage('Test text', 'worker', 'claude', 'trace-2');
    result = validateStandardMessage(msg2);
    logTest('createTextMessage 生成有效结构', result.valid, result.errors.join(', '));
    recordTest(result.valid);

    // 测试 createStreamingMessage
    const msg3 = createStreamingMessage('orchestrator', 'codex', 'trace-3');
    result = validateStandardMessage(msg3);
    logTest('createStreamingMessage 生成有效结构', result.valid, result.errors.join(', '));
    recordTest(result.valid);

    // 验证流式消息的 lifecycle 状态
    const lifecycleCorrect = msg3.lifecycle === MessageLifecycle.STARTED;
    logTest('流式消息初始 lifecycle 为 STARTED', lifecycleCorrect);
    recordTest(lifecycleCorrect);

    // 测试 createErrorMessage
    const msg4 = createErrorMessage('Error occurred', 'worker', 'claude', 'trace-4');
    result = validateStandardMessage(msg4);
    logTest('createErrorMessage 生成有效结构', result.valid, result.errors.join(', '));
    recordTest(result.valid);

    // 验证错误消息的 lifecycle 状态
    const errorLifecycleCorrect = msg4.lifecycle === MessageLifecycle.FAILED;
    logTest('错误消息 lifecycle 为 FAILED', errorLifecycleCorrect);
    recordTest(errorLifecycleCorrect);

  } catch (error) {
    logTest('基础消息结构测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 2: ContentBlock 各类型验证
async function testContentBlockTypes() {
  logSection('测试 2: ContentBlock 各类型验证');

  // TextBlock
  const textBlock = { type: 'text', content: 'Hello World', isMarkdown: true };
  let result = validateContentBlock(textBlock);
  logTest('TextBlock 结构有效', result.valid, result.errors.join(', '));
  recordTest(result.valid);

  // CodeBlock
  const codeBlock = {
    type: 'code',
    content: 'console.log("hello")',
    language: 'javascript',
    filename: 'test.js',
  };
  result = validateContentBlock(codeBlock);
  logTest('CodeBlock 结构有效', result.valid, result.errors.join(', '));
  recordTest(result.valid);

  // ThinkingBlock
  const thinkingBlock = {
    type: 'thinking',
    content: 'Let me think about this...',
    summary: 'Thinking...',
    blockId: 'thinking-1',
  };
  result = validateContentBlock(thinkingBlock);
  logTest('ThinkingBlock 结构有效', result.valid, result.errors.join(', '));
  recordTest(result.valid);

  // ToolCallBlock
  const toolCallBlock = {
    type: 'tool_call',
    toolName: 'read_file',
    toolId: 'tool-1',
    status: 'completed',
    input: { path: '/test/file.ts' },
    output: 'file content',
  };
  result = validateContentBlock(toolCallBlock);
  logTest('ToolCallBlock 结构有效', result.valid, result.errors.join(', '));
  recordTest(result.valid);

  // FileChangeBlock
  const fileChangeBlock = {
    type: 'file_change',
    filePath: '/test/file.ts',
    changeType: 'modify',
    additions: 10,
    deletions: 5,
  };
  result = validateContentBlock(fileChangeBlock);
  logTest('FileChangeBlock 结构有效', result.valid, result.errors.join(', '));
  recordTest(result.valid);

  // 无效的 block
  const invalidBlock = { type: 'text' }; // 缺少 content
  result = validateContentBlock(invalidBlock);
  logTest('无效 TextBlock 被正确检测', !result.valid);
  recordTest(!result.valid);
}

// 测试 3: Normalizer 输出验证
async function testNormalizerOutput() {
  logSection('测试 3: Normalizer 输出验证');

  try {
    const { ClaudeNormalizer } = require('../out/normalizer/claude-normalizer');
    const normalizer = new ClaudeNormalizer();

    // 模拟 Claude 流式响应
    const traceId = 'test-trace-normalizer';
    const messageId = normalizer.startStream(traceId);

    // 验证开始消息
    let startMessageReceived = false;
    normalizer.on('message', (msg) => {
      startMessageReceived = true;
      const result = validateStandardMessage(msg);
      logTest('Normalizer 发出的开始消息结构有效', result.valid, result.errors.join(', '));
      recordTest(result.valid);
    });

    // 模拟处理 chunk
    const testChunks = [
      '{"type":"message_start","message":{"id":"msg-1"}}',
      '{"type":"content_block_start","index":0,"content_block":{"type":"text"}}',
      '{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hello "}}',
      '{"type":"content_block_delta","delta":{"type":"text_delta","text":"World!"}}',
      '{"type":"content_block_stop","index":0}',
      '{"type":"message_stop"}',
    ];

    // 验证流式更新
    let updateCount = 0;
    normalizer.on('update', (update) => {
      updateCount++;
      const result = validateStreamUpdate(update);
      if (!result.valid) {
        logTest(`Normalizer 流式更新 #${updateCount} 结构无效`, false, result.errors.join(', '));
        recordTest(false);
      }
    });

    // 处理所有 chunks
    for (const chunk of testChunks) {
      normalizer.processChunk(messageId, chunk + '\n');
    }

    // 结束流
    const finalMessage = normalizer.endStream(messageId);

    // 验证最终消息
    const finalResult = validateStandardMessage(finalMessage);
    logTest('Normalizer 最终消息结构有效', finalResult.valid, finalResult.errors.join(', '));
    recordTest(finalResult.valid);

    // 验证 blocks 内容
    const hasTextBlock = finalMessage.blocks.some(b => b.type === 'text');
    logTest('最终消息包含 TextBlock', hasTextBlock);
    recordTest(hasTextBlock);

    // 验证文本内容
    const textContent = finalMessage.blocks
      .filter(b => b.type === 'text')
      .map(b => b.content)
      .join('');
    const contentCorrect = textContent.includes('Hello') && textContent.includes('World');
    logTest('TextBlock 内容正确', contentCorrect, `实际内容: ${textContent}`);
    recordTest(contentCorrect);

    log(`  流式更新数量: ${updateCount}`, colors.blue);

  } catch (error) {
    logTest('Normalizer 输出测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 4: 工具调用消息验证
async function testToolCallMessages() {
  logSection('测试 4: 工具调用消息验证');

  try {
    const { ClaudeNormalizer } = require('../out/normalizer/claude-normalizer');
    const normalizer = new ClaudeNormalizer();

    const traceId = 'test-trace-tool';
    const messageId = normalizer.startStream(traceId);

    // 模拟工具调用流
    const toolChunks = [
      '{"type":"message_start"}',
      '{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-123","name":"read_file","input":{}}}',
      '{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\\"path\\":"}}',
      '{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"\\"/test.ts\\"}"}}',
      '{"type":"content_block_stop"}',
      '{"type":"tool_result","result":{"content":"file content here"}}',
      '{"type":"message_stop"}',
    ];

    for (const chunk of toolChunks) {
      normalizer.processChunk(messageId, chunk + '\n');
    }

    const finalMessage = normalizer.endStream(messageId);

    // 验证消息类型
    const isToolCallType = finalMessage.type === 'tool_call';
    logTest('消息类型为 tool_call', isToolCallType, `实际类型: ${finalMessage.type}`);
    recordTest(isToolCallType);

    // 验证 ToolCallBlock
    const toolBlocks = finalMessage.blocks.filter(b => b.type === 'tool_call');
    logTest('包含 ToolCallBlock', toolBlocks.length > 0, `工具调用块数量: ${toolBlocks.length}`);
    recordTest(toolBlocks.length > 0);

    if (toolBlocks.length > 0) {
      const toolBlock = toolBlocks[0];
      const result = validateContentBlock(toolBlock);
      logTest('ToolCallBlock 结构有效', result.valid, result.errors.join(', '));
      recordTest(result.valid);

      // 验证工具名称
      logTest('工具名称正确', toolBlock.toolName === 'read_file', `实际名称: ${toolBlock.toolName}`);
      recordTest(toolBlock.toolName === 'read_file');
    }

  } catch (error) {
    logTest('工具调用消息测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 5: 思考过程消息验证
async function testThinkingMessages() {
  logSection('测试 5: 思考过程消息验证');

  try {
    const { ClaudeNormalizer } = require('../out/normalizer/claude-normalizer');
    const normalizer = new ClaudeNormalizer();

    const traceId = 'test-trace-thinking';
    const messageId = normalizer.startStream(traceId);

    // 模拟思考过程流
    const thinkingChunks = [
      '{"type":"message_start"}',
      '{"type":"content_block_start","index":0,"content_block":{"type":"thinking"}}',
      '{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"Let me analyze "}}',
      '{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"this problem..."}}',
      '{"type":"content_block_stop"}',
      '{"type":"content_block_start","index":1,"content_block":{"type":"text"}}',
      '{"type":"content_block_delta","delta":{"type":"text_delta","text":"Here is my response."}}',
      '{"type":"content_block_stop"}',
      '{"type":"message_stop"}',
    ];

    for (const chunk of thinkingChunks) {
      normalizer.processChunk(messageId, chunk + '\n');
    }

    const finalMessage = normalizer.endStream(messageId);

    // 验证 ThinkingBlock
    const thinkingBlocks = finalMessage.blocks.filter(b => b.type === 'thinking');
    logTest('包含 ThinkingBlock', thinkingBlocks.length > 0, `思考块数量: ${thinkingBlocks.length}`);
    recordTest(thinkingBlocks.length > 0);

    if (thinkingBlocks.length > 0) {
      const thinkingBlock = thinkingBlocks[0];
      const result = validateContentBlock(thinkingBlock);
      logTest('ThinkingBlock 结构有效', result.valid, result.errors.join(', '));
      recordTest(result.valid);

      // 验证思考内容
      const contentCorrect = thinkingBlock.content.includes('analyze') && thinkingBlock.content.includes('problem');
      logTest('思考内容正确', contentCorrect, `实际内容: ${thinkingBlock.content}`);
      recordTest(contentCorrect);
    }

    // 验证同时包含 TextBlock
    const textBlocks = finalMessage.blocks.filter(b => b.type === 'text');
    logTest('同时包含 TextBlock', textBlocks.length > 0);
    recordTest(textBlocks.length > 0);

  } catch (error) {
    logTest('思考过程消息测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 6: 交互消息验证
async function testInteractionMessages() {
  logSection('测试 6: 交互消息验证');

  try {
    const { createInteractionMessage, InteractionType, generateMessageId } = require('../out/protocol/message-protocol');

    // 创建权限请求
    const permissionRequest = {
      type: InteractionType.PERMISSION,
      requestId: generateMessageId(),
      prompt: 'Allow file modification?',
      options: [
        { value: 'yes', label: 'Yes', isDefault: true },
        { value: 'no', label: 'No' },
      ],
      required: true,
    };

    let result = validateInteractionRequest(permissionRequest);
    logTest('权限请求结构有效', result.valid, result.errors.join(', '));
    recordTest(result.valid);

    // 创建交互消息
    const interactionMsg = createInteractionMessage(
      permissionRequest,
      'worker',
      'claude',
      'trace-interaction'
    );

    result = validateStandardMessage(interactionMsg);
    logTest('交互消息结构有效', result.valid, result.errors.join(', '));
    recordTest(result.valid);

    // 验证消息类型
    const typeCorrect = interactionMsg.type === 'interaction';
    logTest('消息类型为 interaction', typeCorrect, `实际类型: ${interactionMsg.type}`);
    recordTest(typeCorrect);

    // 验证 interaction 字段存在
    const hasInteraction = !!interactionMsg.interaction;
    logTest('包含 interaction 字段', hasInteraction);
    recordTest(hasInteraction);

    if (hasInteraction) {
      result = validateInteractionRequest(interactionMsg.interaction);
      logTest('内嵌 interaction 结构有效', result.valid, result.errors.join(', '));
      recordTest(result.valid);
    }

  } catch (error) {
    logTest('交互消息测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 7: 内容解析器验证
async function testContentParser() {
  logSection('测试 7: 内容解析器验证');

  try {
    const { parseContentToBlocks } = require('../out/utils/content-parser');

    // 测试纯文本
    const textBlocks = parseContentToBlocks('Hello, this is plain text.');
    logTest('纯文本解析', textBlocks.length > 0 && textBlocks[0].type === 'text');
    recordTest(textBlocks.length > 0);

    // 测试 Markdown
    const mdBlocks = parseContentToBlocks('# Title\n\nThis is **bold** text.');
    const mdHasMarkdown = mdBlocks.some(b => b.type === 'text' && b.isMarkdown);
    logTest('Markdown 解析 (isMarkdown=true)', mdHasMarkdown);
    recordTest(mdHasMarkdown);

    // 测试代码块
    const codeContent = '```javascript\nconsole.log("hello");\n```';
    const codeBlocks = parseContentToBlocks(codeContent);
    const hasCodeBlock = codeBlocks.some(b => b.type === 'code' && b.language === 'javascript');
    logTest('代码块解析', hasCodeBlock);
    recordTest(hasCodeBlock);

    if (hasCodeBlock) {
      const codeBlock = codeBlocks.find(b => b.type === 'code');
      const result = validateContentBlock(codeBlock);
      logTest('代码块结构有效', result.valid, result.errors.join(', '));
      recordTest(result.valid);
    }

    // 测试混合内容
    const mixedContent = `
Here is some text.

\`\`\`typescript
const x = 1;
\`\`\`

And more text after code.
`;
    const mixedBlocks = parseContentToBlocks(mixedContent);
    const hasText = mixedBlocks.some(b => b.type === 'text');
    const hasCode = mixedBlocks.some(b => b.type === 'code');
    logTest('混合内容解析 (文本+代码)', hasText && hasCode,
      `blocks: ${mixedBlocks.map(b => b.type).join(', ')}`);
    recordTest(hasText && hasCode);

    // 验证所有 blocks 结构
    let allValid = true;
    for (const block of mixedBlocks) {
      const result = validateContentBlock(block);
      if (!result.valid) {
        allValid = false;
        log(`  无效 block: ${result.errors.join(', ')}`, colors.red);
      }
    }
    logTest('所有解析的 blocks 结构有效', allValid);
    recordTest(allValid);

  } catch (error) {
    logTest('内容解析器测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 8: 前端数据转换验证
async function testFrontendDataTransform() {
  logSection('测试 8: 前端数据转换验证 (模拟)');

  // 模拟前端的 standardToWebviewMessage 逻辑
  function standardToWebviewMessage(message) {
    const blocks = message.blocks || [];

    // 提取文本内容
    const textContent = blocks
      .filter(b => b.type === 'text')
      .map(b => b.content)
      .join('\n');

    // 提取思考内容
    const thinking = blocks
      .filter(b => b.type === 'thinking')
      .map(b => ({ content: b.content, summary: b.summary }));

    // 提取工具调用
    const toolCalls = blocks
      .filter(b => b.type === 'tool_call')
      .map(b => ({
        id: b.toolId,
        name: b.toolName,
        status: b.status,
        input: b.input,
        output: b.output,
        error: b.error,
      }));

    // 提取代码块
    const codeBlocks = blocks
      .filter(b => b.type === 'code')
      .map(b => ({
        language: b.language || 'text',
        content: b.content,
        filename: b.filename,
      }));

    return {
      role: message.source === 'user' ? 'user' : 'assistant',
      content: textContent,
      timestamp: message.timestamp,
      streaming: message.lifecycle === 'streaming' || message.lifecycle === 'started',
      source: message.source,
      cli: message.cli,
      thinking,
      toolCalls,
      codeBlocks,
      parsedBlocks: blocks,
      standardMessageId: message.id,
      traceId: message.traceId,
      lifecycle: message.lifecycle,
      messageType: message.type,
    };
  }

  try {
    const { createStandardMessage, MessageType, MessageLifecycle } = require('../out/protocol/message-protocol');

    // 创建一个复杂消息
    const complexMessage = createStandardMessage({
      traceId: 'trace-complex',
      type: MessageType.TEXT,
      source: 'worker',
      cli: 'claude',
      lifecycle: MessageLifecycle.COMPLETED,
      blocks: [
        { type: 'thinking', content: 'Analyzing...', summary: 'Thinking' },
        { type: 'text', content: 'Here is my response.', isMarkdown: true },
        { type: 'code', content: 'const x = 1;', language: 'typescript' },
        { type: 'tool_call', toolName: 'read_file', toolId: 'tool-1', status: 'completed', output: 'file content' },
      ],
      metadata: { taskId: 'task-1' },
    });

    // 转换为前端格式
    const webviewMsg = standardToWebviewMessage(complexMessage);

    // 验证转换结果
    logTest('role 字段正确', webviewMsg.role === 'assistant');
    recordTest(webviewMsg.role === 'assistant');

    logTest('content 提取正确', webviewMsg.content.includes('Here is my response'));
    recordTest(webviewMsg.content.includes('Here is my response'));

    logTest('thinking 数组提取正确', webviewMsg.thinking.length === 1);
    recordTest(webviewMsg.thinking.length === 1);

    logTest('toolCalls 数组提取正确', webviewMsg.toolCalls.length === 1);
    recordTest(webviewMsg.toolCalls.length === 1);

    logTest('codeBlocks 数组提取正确', webviewMsg.codeBlocks.length === 1);
    recordTest(webviewMsg.codeBlocks.length === 1);

    logTest('parsedBlocks 保留原始 blocks', webviewMsg.parsedBlocks.length === 4);
    recordTest(webviewMsg.parsedBlocks.length === 4);

    logTest('standardMessageId 正确映射', webviewMsg.standardMessageId === complexMessage.id);
    recordTest(webviewMsg.standardMessageId === complexMessage.id);

    // 验证工具调用细节
    if (webviewMsg.toolCalls.length > 0) {
      const tc = webviewMsg.toolCalls[0];
      logTest('工具调用 id 正确', tc.id === 'tool-1');
      recordTest(tc.id === 'tool-1');

      logTest('工具调用 name 正确', tc.name === 'read_file');
      recordTest(tc.name === 'read_file');

      logTest('工具调用 status 正确', tc.status === 'completed');
      recordTest(tc.status === 'completed');
    }

  } catch (error) {
    logTest('前端数据转换测试失败', false, error.message);
    recordTest(false);
  }
}

// 打印测试报告
function printTestReport() {
  logSection('测试报告');

  log(`总计: ${stats.total}`, colors.blue);
  log(`通过: ${stats.passed}`, colors.green);
  log(`失败: ${stats.failed}`, stats.failed > 0 ? colors.red : colors.green);
  log(`成功率: ${((stats.passed / stats.total) * 100).toFixed(1)}%`,
      stats.failed === 0 ? colors.green : colors.yellow);

  console.log('\n' + '='.repeat(80));
  if (stats.failed === 0) {
    log('[SUCCESS] 所有消息格式测试通过！', colors.green + colors.bright);
  } else {
    log('[FAILURE] 部分测试失败，需要修复', colors.red + colors.bright);
  }
  console.log('='.repeat(80) + '\n');

  return stats.failed === 0;
}

// 主测试流程
async function runTests() {
  log('\n' + '='.repeat(80), colors.cyan);
  log('  MultiCLI 消息格式端对端验证测试', colors.cyan + colors.bright);
  log('='.repeat(80) + '\n', colors.cyan);

  try {
    await testBasicMessageStructure();
    await testContentBlockTypes();
    await testNormalizerOutput();
    await testToolCallMessages();
    await testThinkingMessages();
    await testInteractionMessages();
    await testContentParser();
    await testFrontendDataTransform();

    const success = printTestReport();
    process.exit(success ? 0 : 1);

  } catch (error) {
    log('\n[ERROR] 测试执行失败:', colors.red);
    log(error.stack, colors.red);
    process.exit(1);
  }
}

// 运行测试
runTests();
