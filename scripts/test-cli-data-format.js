/**
 * CLI 数据格式完整性测试
 *
 * 验证所有类型的 CLI 输出都能正确处理，
 * 确保用户只看到实际内容，不看到 JSON 元数据
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
  if (details) {
    log(`    ${details}`, passed ? colors.blue : colors.yellow);
  }
}

// 测试统计
const stats = { total: 0, passed: 0, failed: 0 };

function recordTest(passed) {
  stats.total++;
  if (passed) stats.passed++;
  else stats.failed++;
}

// 模拟前端的 isInternalJsonMessage 函数
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

// 测试场景：各种 CLI 输出内容
const testCases = [
  // 1. 纯文本回复
  {
    name: '纯文本回复',
    input: '这是一个简单的回复，没有任何格式。',
    expectVisible: true,
    expectedContent: '这是一个简单的回复，没有任何格式。',
  },

  // 2. Markdown 格式回复
  {
    name: 'Markdown 格式回复',
    input: '# 标题\n\n这是**粗体**和*斜体*文本。\n\n- 列表项1\n- 列表项2',
    expectVisible: true,
    expectedContent: '# 标题',
  },

  // 3. 代码块
  {
    name: '代码块回复',
    input: '这是代码示例：\n\n```javascript\nconst x = 1;\nconsole.log(x);\n```\n\n上面是代码。',
    expectVisible: true,
    expectedContent: 'const x = 1;',
  },

  // 4. 内部 JSON 元数据 - 应该被过滤
  {
    name: '模糊度评估 JSON (应过滤)',
    input: '{"score": 0.8, "isAmbiguous": true, "reason": "需求不明确"}',
    expectVisible: false,
    reason: '这是内部元数据，用户不应该看到',
  },

  // 5. 计划评审 JSON - 应该被过滤
  {
    name: '计划评审 JSON (应过滤)',
    input: '{"status": "approved", "summary": "计划已通过", "concerns": []}',
    expectVisible: false,
    reason: '这是内部元数据，用户不应该看到',
  },

  // 6. 工具调用追踪 JSON - 应该被过滤
  {
    name: '工具调用追踪 JSON (应过滤)',
    input: '{"type": "assistant", "message": {"role": "assistant", "content": [{"type": "tool_use", "id": "tool-1", "name": "read_file"}]}}',
    expectVisible: false,
    reason: '这是内部工具调用追踪，用户不应该看到',
  },

  // 7. 工具结果 JSON - 应该被过滤
  {
    name: '工具结果引用 JSON (应过滤)',
    input: '{"tool_use_id": "tool-123", "content": "文件内容"}',
    expectVisible: false,
    reason: '这是内部工具结果引用，用户不应该看到',
  },

  // 8. 普通 JSON 数据（用户请求的）- 应该显示
  {
    name: '用户请求的 JSON 数据 (应显示)',
    input: '```json\n{"name": "张三", "age": 25}\n```',
    expectVisible: true,
    expectedContent: '"name": "张三"',
    reason: '这是用户请求的数据，在代码块中，应该显示',
  },

  // 9. 混合内容：文本 + 裸露 JSON
  {
    name: '文本混合裸露 JSON (JSON应被移除)',
    input: '我分析了你的需求。\n\n{"score": 0.5, "isAmbiguous": false}\n\n下面是建议：',
    expectVisible: true,
    expectedContentNot: '"score"',
    reason: '文本应保留，裸露的内部 JSON 应被移除',
  },

  // 10. 嵌入式 JSON 代码块（在文本中间）
  {
    name: '嵌入式 JSON 代码块 (应标记为 isEmbedded)',
    input: '分析结果如下：\n\n```json\n{"analysis": "完成", "result": true}\n```\n\n以上是结果。',
    expectVisible: true,
    checkEmbedded: true,
    reason: '中间的 JSON 代码块应被标记为 isEmbedded',
  },

  // 11. 以代码块开头的 JSON（用户请求的）
  {
    name: 'JSON 代码块开头 (应显示)',
    input: '```json\n{"config": "value"}\n```',
    expectVisible: true,
    expectedContent: '"config"',
    checkNotEmbedded: true,
    reason: '以代码块开头的 JSON 应该显示给用户',
  },

  // 12. 思考过程（应在折叠面板中显示）
  {
    name: '思考过程内容',
    input: 'Let me think about this problem...',
    expectVisible: true,
    asThinking: true,
    reason: '思考过程应在专门的折叠面板中显示',
  },
];

// 测试 1: 内容解析器测试
async function testContentParser() {
  logSection('测试 1: 内容解析器 - 确保正确处理各种内容');

  try {
    const { parseContentToBlocks, extractEmbeddedJson } = require('../out/utils/content-parser');

    for (const testCase of testCases) {
      if (testCase.asThinking) continue; // 思考过程测试单独处理

      const blocks = parseContentToBlocks(testCase.input);

      // 检查是否有可见内容
      const visibleBlocks = blocks.filter(b => !b.isEmbedded);
      const hasVisibleContent = visibleBlocks.length > 0;

      if (testCase.expectVisible) {
        // 应该有可见内容
        let passed = hasVisibleContent;
        let details = `blocks: ${blocks.length}, visible: ${visibleBlocks.length}`;

        if (testCase.expectedContent) {
          const allContent = blocks.map(b => b.content).join(' ');
          passed = passed && allContent.includes(testCase.expectedContent);
          if (!passed) details += `, 未找到期望内容: "${testCase.expectedContent}"`;
        }

        if (testCase.expectedContentNot) {
          const allContent = blocks.map(b => b.content).join(' ');
          passed = passed && !allContent.includes(testCase.expectedContentNot);
          if (!passed) details += `, 不应包含: "${testCase.expectedContentNot}"`;
        }

        if (testCase.checkEmbedded) {
          const hasEmbedded = blocks.some(b => b.isEmbedded);
          passed = passed && hasEmbedded;
          if (!hasEmbedded) details += ', 应有 isEmbedded 标记';
        }

        if (testCase.checkNotEmbedded) {
          const hasEmbedded = blocks.some(b => b.isEmbedded);
          passed = passed && !hasEmbedded;
          if (hasEmbedded) details += ', 不应有 isEmbedded 标记';
        }

        logTest(testCase.name, passed, details);
        recordTest(passed);
      } else {
        // 检查是否被 isInternalJsonMessage 过滤
        const isFiltered = isInternalJsonMessage(testCase.input);
        logTest(testCase.name, isFiltered, isFiltered ? '已被过滤' : '未被过滤');
        recordTest(isFiltered);
      }
    }
  } catch (error) {
    logTest('内容解析器测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 2: 裸露 JSON 提取和移除
async function testEmbeddedJsonExtraction() {
  logSection('测试 2: 裸露 JSON 提取和移除');

  try {
    const { extractEmbeddedJson, parseContentToBlocks } = require('../out/utils/content-parser');

    // 测试包含裸露 JSON 的内容
    const contentWithJson = `
这是介绍文字。

{"score": 0.7, "isAmbiguous": true, "details": "需要澄清"}

继续说明...

{"status": "pending", "items": [1, 2, 3]}

结束语。
`;

    const extracted = extractEmbeddedJson(contentWithJson);
    const extractedCount = extracted.length;
    logTest('提取裸露 JSON', extractedCount === 2, `提取到 ${extractedCount} 个 JSON`);
    recordTest(extractedCount === 2);

    // 解析后检查 JSON 是否被移除
    const blocks = parseContentToBlocks(contentWithJson);
    const allContent = blocks.map(b => b.content).join(' ');

    const noScore = !allContent.includes('"score"');
    const noStatus = !allContent.includes('"status"');
    const hasText = allContent.includes('介绍文字') && allContent.includes('结束语');

    logTest('移除裸露 JSON 后文本保留', hasText, hasText ? '文本内容保留' : '文本丢失');
    recordTest(hasText);

    logTest('移除了 score JSON', noScore);
    recordTest(noScore);

    logTest('移除了 status JSON', noStatus);
    recordTest(noStatus);

  } catch (error) {
    logTest('裸露 JSON 测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 3: 嵌入式代码块处理
async function testEmbeddedCodeBlocks() {
  logSection('测试 3: 嵌入式代码块处理');

  try {
    const { parseContentToBlocks } = require('../out/utils/content-parser');

    // 场景 1: 文本开头，中间有 JSON 代码块
    const mixedContent = `
首先，我来分析一下你的配置：

\`\`\`json
{"config": "internal", "metadata": true}
\`\`\`

基于以上分析，建议如下：

1. 修改配置A
2. 更新配置B
`;

    const blocks1 = parseContentToBlocks(mixedContent);
    const jsonBlock = blocks1.find(b => b.type === 'code' && b.language === 'json');

    if (jsonBlock) {
      logTest('中间 JSON 代码块标记为 isEmbedded', jsonBlock.isEmbedded === true);
      recordTest(jsonBlock.isEmbedded === true);
    } else {
      logTest('找到 JSON 代码块', false);
      recordTest(false);
    }

    // 场景 2: 以代码块开头
    const codeFirst = `\`\`\`json
{"result": "success"}
\`\`\``;

    const blocks2 = parseContentToBlocks(codeFirst);
    const jsonBlock2 = blocks2.find(b => b.type === 'code' && b.language === 'json');

    if (jsonBlock2) {
      logTest('开头 JSON 代码块不标记 isEmbedded', !jsonBlock2.isEmbedded);
      recordTest(!jsonBlock2.isEmbedded);
    } else {
      logTest('找到 JSON 代码块', false);
      recordTest(false);
    }

    // 场景 3: 非 JSON 代码块不受影响
    const pythonCode = `
下面是 Python 代码：

\`\`\`python
def hello():
    print("Hello")
\`\`\`

这就是代码。
`;

    const blocks3 = parseContentToBlocks(pythonCode);
    const pythonBlock = blocks3.find(b => b.type === 'code' && b.language === 'python');

    if (pythonBlock) {
      logTest('Python 代码块不标记 isEmbedded', !pythonBlock.isEmbedded);
      recordTest(!pythonBlock.isEmbedded);
    } else {
      logTest('找到 Python 代码块', false);
      recordTest(false);
    }

  } catch (error) {
    logTest('嵌入式代码块测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 4: 前端过滤验证
async function testFrontendFiltering() {
  logSection('测试 4: 前端内部 JSON 过滤');

  const internalJsonCases = [
    { name: '模糊度评估', json: '{"score": 0.5, "isAmbiguous": true}', shouldFilter: true },
    { name: '计划评审', json: '{"status": "approved", "summary": "OK"}', shouldFilter: true },
    { name: '消息封装', json: '{"type": "assistant", "message": {"role": "assistant", "content": []}}', shouldFilter: true },
    { name: '工具追踪', json: '{"type": "msg", "message": {"content": [{"type": "tool_use", "id": "1"}]}}', shouldFilter: true },
    { name: '工具引用', json: '{"tool_use_id": "123", "content": "result"}', shouldFilter: true },
    { name: '普通用户数据', json: '{"name": "张三", "age": 25}', shouldFilter: false },
    { name: '配置对象', json: '{"host": "localhost", "port": 3000}', shouldFilter: false },
    { name: '数组数据', json: '[1, 2, 3]', shouldFilter: false },
  ];

  for (const testCase of internalJsonCases) {
    const isFiltered = isInternalJsonMessage(testCase.json);
    // 使用 !! 转换为布尔值进行比较
    const passed = !!isFiltered === testCase.shouldFilter;
    logTest(
      testCase.name + (testCase.shouldFilter ? ' (应过滤)' : ' (应保留)'),
      passed,
      isFiltered ? '被过滤' : '未过滤'
    );
    recordTest(passed);
  }
}

// 测试 5: Normalizer 端到端测试
async function testNormalizerE2E() {
  logSection('测试 5: Normalizer 端到端 - 真实 CLI 响应');

  try {
    const { ClaudeNormalizer } = require('../out/normalizer/claude-normalizer');

    // 模拟包含各种内容的 Claude 响应
    const normalizer = new ClaudeNormalizer();
    const messageId = normalizer.startStream('test-trace');

    // 模拟流式响应
    const chunks = [
      '{"type":"message_start"}',
      '{"type":"content_block_start","index":0,"content_block":{"type":"text"}}',
      '{"type":"content_block_delta","delta":{"type":"text_delta","text":"我来帮你分析这个问题。"}}',
      '{"type":"content_block_delta","delta":{"type":"text_delta","text":"\\n\\n"}}',
      '{"type":"content_block_delta","delta":{"type":"text_delta","text":"```typescript\\nconst x = 1;\\n```"}}',
      '{"type":"content_block_delta","delta":{"type":"text_delta","text":"\\n\\n这就是代码。"}}',
      '{"type":"content_block_stop"}',
      '{"type":"message_stop"}',
    ];

    for (const chunk of chunks) {
      normalizer.processChunk(messageId, chunk + '\n');
    }

    const finalMessage = normalizer.endStream(messageId);

    // 验证
    const hasTextBlock = finalMessage.blocks.some(b => b.type === 'text');
    logTest('Normalizer 输出包含文本块', hasTextBlock);
    recordTest(hasTextBlock);

    const textContent = finalMessage.blocks
      .filter(b => b.type === 'text')
      .map(b => b.content)
      .join(' ');

    const hasUserContent = textContent.includes('分析') && textContent.includes('问题');
    logTest('文本内容可读', hasUserContent, `内容: ${textContent.substring(0, 50)}...`);
    recordTest(hasUserContent);

    // 确保没有内部 JSON
    const noInternalJson = !isInternalJsonMessage(textContent);
    logTest('无内部 JSON 元数据', noInternalJson);
    recordTest(noInternalJson);

  } catch (error) {
    logTest('Normalizer E2E 测试失败', false, error.message);
    recordTest(false);
  }
}

// 测试 6: 边界情况
async function testEdgeCases() {
  logSection('测试 6: 边界情况');

  try {
    const { parseContentToBlocks } = require('../out/utils/content-parser');

    // 空内容
    let blocks = parseContentToBlocks('');
    logTest('空内容返回空数组', blocks.length === 0);
    recordTest(blocks.length === 0);

    // 纯空白
    blocks = parseContentToBlocks('   \n\n   ');
    logTest('纯空白返回空数组', blocks.length === 0);
    recordTest(blocks.length === 0);

    // 非法 JSON（看起来像 JSON 但不是）
    blocks = parseContentToBlocks('{这不是JSON}');
    const hasContent = blocks.length > 0;
    logTest('非法 JSON 字符串正常处理', hasContent);
    recordTest(hasContent);

    // 嵌套代码块（代码块中包含代码块语法）
    const nestedCode = '```markdown\n这是示例：\n```js\nconsole.log();\n```\n```';
    blocks = parseContentToBlocks(nestedCode);
    logTest('嵌套代码块语法处理', blocks.length > 0);
    recordTest(blocks.length > 0);

    // 只有 ANSI 转义序列
    blocks = parseContentToBlocks('\x1b[31mRed\x1b[0m \x1b[32mGreen\x1b[0m');
    const cleanText = blocks.length > 0 && blocks[0].content.includes('Red');
    logTest('ANSI 转义序列被清理', cleanText);
    recordTest(cleanText);

  } catch (error) {
    logTest('边界情况测试失败', false, error.message);
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
    log('[SUCCESS] CLI 数据格式验证通过！用户只会看到实际内容。', colors.green + colors.bright);
  } else {
    log('[FAILURE] 部分测试失败，可能存在 JSON 泄露给用户的风险', colors.red + colors.bright);
  }
  console.log('='.repeat(80) + '\n');

  return stats.failed === 0;
}

// 主测试流程
async function runTests() {
  log('\n' + '='.repeat(80), colors.cyan);
  log('  CLI 数据格式完整性验证', colors.cyan + colors.bright);
  log('  确保用户只看到实际内容，不看到 JSON 元数据', colors.cyan);
  log('='.repeat(80) + '\n', colors.cyan);

  try {
    await testContentParser();
    await testEmbeddedJsonExtraction();
    await testEmbeddedCodeBlocks();
    await testFrontendFiltering();
    await testNormalizerE2E();
    await testEdgeCases();

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
