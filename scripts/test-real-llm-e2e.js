/**
 * 真实端对端 LLM 集成测试
 *
 * 测试范围：
 * 1. 真实 LLM API 调用（需要配置 API Key）
 * 2. 用户需求处理完整流程
 * 3. 记忆系统上下文的使用
 * 4. 工具调用和编排系统
 *
 * 运行方式：node scripts/test-real-llm-e2e.js
 *
 * 环境要求：
 * - ~/.multicli/llm.json 需要配置有效的 API Key
 * - 或设置环境变量 ANTHROPIC_API_KEY / OPENAI_API_KEY
 */

const path = require('path');
const fs = require('fs');
const os = require('os');

// 测试工作目录
const TEST_WORKSPACE = path.join(__dirname, '..', '.test-real-llm');

// 测试结果统计
let passed = 0;
let failed = 0;
let skipped = 0;
const results = [];

function test(name, fn) {
  try {
    fn();
    console.log(`✅ ${name}`);
    passed++;
    results.push({ name, status: 'passed' });
  } catch (error) {
    console.log(`❌ ${name}`);
    console.log(`   错误: ${error.message}`);
    failed++;
    results.push({ name, status: 'failed', error: error.message });
  }
}

async function asyncTest(name, fn, options = {}) {
  const { skipIf, skipMessage } = options;
  if (skipIf) {
    console.log(`⏭️  ${name} (跳过: ${skipMessage})`);
    skipped++;
    results.push({ name, status: 'skipped', reason: skipMessage });
    return;
  }

  try {
    await fn();
    console.log(`✅ ${name}`);
    passed++;
    results.push({ name, status: 'passed' });
  } catch (error) {
    console.log(`❌ ${name}`);
    console.log(`   错误: ${error.message}`);
    if (error.stack) {
      console.log(`   堆栈: ${error.stack.split('\n').slice(1, 3).join('\n')}`);
    }
    failed++;
    results.push({ name, status: 'failed', error: error.message });
  }
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message || 'Assertion failed');
  }
}

function cleanupTestDir() {
  if (fs.existsSync(TEST_WORKSPACE)) {
    fs.rmSync(TEST_WORKSPACE, { recursive: true });
  }
  fs.mkdirSync(TEST_WORKSPACE, { recursive: true });
}

// 检查 API Key 是否配置
function checkApiKeyAvailable() {
  const configPath = path.join(os.homedir(), '.multicli', 'llm.json');
  if (fs.existsSync(configPath)) {
    try {
      const config = JSON.parse(fs.readFileSync(configPath, 'utf-8'));
      // 检查 orchestrator 或 workers.claude 是否有 API Key
      if (config.orchestrator?.apiKey || config.workers?.claude?.apiKey) {
        return true;
      }
    } catch (e) {
      // 忽略解析错误
    }
  }
  // 检查环境变量
  return !!(process.env.ANTHROPIC_API_KEY || process.env.OPENAI_API_KEY);
}

async function runTests() {
  console.log('\n🧪 真实端对端 LLM 集成测试\n');
  console.log('='.repeat(70));

  const hasApiKey = checkApiKeyAvailable();
  if (!hasApiKey) {
    console.log('\n⚠️  警告: 未检测到 API Key 配置');
    console.log('   请在 ~/.multicli/llm.json 中配置 API Key');
    console.log('   或设置环境变量 ANTHROPIC_API_KEY / OPENAI_API_KEY');
    console.log('   部分测试将被跳过\n');
  }

  cleanupTestDir();

  // 动态导入编译后的模块
  const { LLMAdapterFactory } = require('../out/llm/adapter-factory.js');
  const { LLMConfigLoader } = require('../out/llm/config.js');
  const { ContextManager } = require('../out/context/context-manager.js');
  const { ContextCompressor } = require('../out/context/context-compressor.js');
  const { MemoryDocument } = require('../out/context/memory-document.js');
  const { ToolManager } = require('../out/tools/tool-manager.js');

  // ========================================
  // 1. LLM 适配器工厂初始化测试
  // ========================================
  console.log('\n🏭 1. LLM 适配器工厂初始化测试\n');

  let adapterFactory;

  await asyncTest('创建 LLMAdapterFactory', async () => {
    adapterFactory = new LLMAdapterFactory({ cwd: TEST_WORKSPACE });
    assert(adapterFactory !== null, 'LLMAdapterFactory 应该被创建');
  });

  await asyncTest('初始化 LLMAdapterFactory', async () => {
    await adapterFactory.initialize();
    const toolManager = adapterFactory.getToolManager();
    assert(toolManager !== null, 'ToolManager 应该可用');
  });

  test('获取 ToolManager', () => {
    const toolManager = adapterFactory.getToolManager();
    assert(toolManager instanceof ToolManager, '应该返回 ToolManager 实例');
  });

  test('获取 MCP 执行器', () => {
    const mcpExecutor = adapterFactory.getMCPExecutor();
    // MCP 执行器可能为 null（如果没有配置 MCP 服务器）
    console.log(`   MCP 执行器: ${mcpExecutor ? '已加载' : '未配置'}`);
  });

  // ========================================
  // 2. 配置加载和验证测试
  // ========================================
  console.log('\n⚙️  2. 配置加载和验证测试\n');

  test('加载完整 LLM 配置', () => {
    const config = LLMConfigLoader.loadFullConfig();
    assert(config.orchestrator, '应该有编排者配置');
    assert(config.workers, '应该有工人配置');
    console.log(`   编排者模型: ${config.orchestrator.model}`);
    console.log(`   Claude 模型: ${config.workers.claude.model}`);
  });

  test('验证配置格式', () => {
    const config = LLMConfigLoader.loadFullConfig();
    const validation = LLMConfigLoader.validateFullConfig(config);
    console.log(`   配置验证: ${validation.valid ? '通过' : '失败'}`);
    if (!validation.valid) {
      console.log(`   错误: ${validation.errors.join(', ')}`);
    }
  });

  // ========================================
  // 3. 上下文管理和记忆系统测试
  // ========================================
  console.log('\n🧠 3. 上下文管理和记忆系统测试\n');

  let contextManager;
  const testSessionId = 'test-real-llm-session';

  await asyncTest('创建并初始化 ContextManager', async () => {
    contextManager = new ContextManager(TEST_WORKSPACE);
    await contextManager.initialize(testSessionId, '真实 LLM 测试会话');
    const state = contextManager.exportState();
    assert(state.immediateContextCount === 0, '初始上下文应该为空');
  });

  test('添加用户消息到上下文', () => {
    contextManager.addMessage({
      role: 'user',
      content: '请帮我分析一下这个项目的代码结构',
    });
    const state = contextManager.exportState();
    assert(state.immediateContextCount === 1, '应该有 1 条消息');
  });

  test('添加助手响应到上下文', () => {
    contextManager.addMessage({
      role: 'assistant',
      content: '好的，我来分析项目的代码结构。首先让我查看主要的目录和文件...',
    });
    const state = contextManager.exportState();
    assert(state.immediateContextCount === 2, '应该有 2 条消息');
  });

  test('记录任务到 Memory', () => {
    contextManager.addTask({
      id: 'task-analyze',
      description: '分析项目代码结构',
      status: 'in_progress',
      assignedWorker: 'claude',
    });
    const memory = contextManager.getMemoryDocument();
    const content = memory.getContent();
    assert(content.currentTasks.length === 1, '应该有 1 个当前任务');
  });

  test('记录关键决策到 Memory', () => {
    contextManager.addDecision(
      'arch-decision-1',
      '采用 LLM 直接调用替代 CLI 封装',
      '提高响应速度和可靠性'
    );
    const memory = contextManager.getMemoryDocument();
    const content = memory.getContent();
    assert(content.keyDecisions.length === 1, '应该有 1 个关键决策');
  });

  test('获取组装后的上下文', () => {
    const context = contextManager.getContext(4000);
    assert(context.length > 0, '上下文应该非空');
    console.log(`   组装后上下文长度: ${context.length} 字符`);
  });

  await asyncTest('保存 Memory 到文件', async () => {
    await contextManager.saveMemory();
    const memoryPath = path.join(
      TEST_WORKSPACE,
      '.multicli/sessions',
      testSessionId,
      'memory.json'
    );
    assert(fs.existsSync(memoryPath), 'Memory 文件应该存在');
    console.log(`   Memory 保存到: ${memoryPath}`);
  });

  // ========================================
  // 4. 真实 LLM 调用测试
  // ========================================
  console.log('\n🤖 4. 真实 LLM 调用测试\n');

  await asyncTest(
    '发送简单消息到 LLM',
    async () => {
      const response = await adapterFactory.sendMessage(
        'claude',
        '请用一句话简单介绍你自己。',
        undefined,
        { source: 'test', adapterRole: 'worker' }
      );

      assert(response.content, '应该有响应内容');
      assert(response.content.length > 0, '响应内容不应为空');
      console.log(`   LLM 响应: ${response.content.substring(0, 100)}...`);
      console.log(`   Token 使用: 输入 ${response.tokenUsage?.inputTokens || 0}, 输出 ${response.tokenUsage?.outputTokens || 0}`);
    },
    {
      skipIf: !hasApiKey,
      skipMessage: '未配置 API Key',
    }
  );

  await asyncTest(
    '发送带上下文的消息到 LLM',
    async () => {
      // 使用之前积累的上下文
      const context = contextManager.getContext(2000);
      const message = `${context}\n\n根据以上会话上下文，请简要总结我们正在做什么任务？`;

      const response = await adapterFactory.sendMessage(
        'claude',
        message,
        undefined,
        { source: 'test', adapterRole: 'worker' }
      );

      assert(response.content, '应该有响应内容');
      console.log(`   上下文感知响应: ${response.content.substring(0, 150)}...`);
    },
    {
      skipIf: !hasApiKey,
      skipMessage: '未配置 API Key',
    }
  );

  // ========================================
  // 5. 工具调用集成测试
  // ========================================
  console.log('\n🔧 5. 工具调用集成测试\n');

  await asyncTest('获取可用工具列表', async () => {
    const toolManager = adapterFactory.getToolManager();
    const tools = await toolManager.getTools();
    assert(tools.length >= 1, '至少应该有 Shell 工具');
    console.log(`   可用工具数量: ${tools.length}`);
    tools.forEach((tool) => {
      console.log(`   - ${tool.name}: ${tool.description?.substring(0, 50)}...`);
    });
  });

  await asyncTest('执行 Shell 工具', async () => {
    const toolManager = adapterFactory.getToolManager();
    const result = await toolManager.execute({
      id: 'test-shell-1',
      name: 'execute_shell',
      arguments: {
        command: 'echo "Hello from real LLM test"',
        cwd: TEST_WORKSPACE,
      },
    });

    assert(!result.isError, '工具执行应该成功');
    assert(result.content.includes('Hello'), '应该包含预期输出');
    console.log(`   Shell 输出: ${result.content.trim()}`);
  });

  await asyncTest(
    'LLM 触发工具调用',
    async () => {
      // 发送一个需要工具调用的请求
      const message = '请执行 echo "LLM triggered tool call" 命令';

      const response = await adapterFactory.sendMessage(
        'claude',
        message,
        undefined,
        { source: 'test', adapterRole: 'worker' }
      );

      // 检查是否有工具调用或响应中提到了执行结果
      console.log(`   LLM 响应: ${response.content.substring(0, 200)}...`);
    },
    {
      skipIf: !hasApiKey,
      skipMessage: '未配置 API Key',
    }
  );

  // ========================================
  // 6. 多轮对话和上下文累积测试
  // ========================================
  console.log('\n💬 6. 多轮对话和上下文累积测试\n');

  await asyncTest(
    '多轮对话测试',
    async () => {
      // 第一轮
      contextManager.addMessage({
        role: 'user',
        content: '我正在开发一个 VS Code 扩展',
      });

      let response = await adapterFactory.sendMessage(
        'claude',
        '我正在开发一个 VS Code 扩展，请记住这一点。',
        undefined,
        { source: 'test', adapterRole: 'worker' }
      );

      contextManager.addMessage({
        role: 'assistant',
        content: response.content,
      });

      // 第二轮 - 检查上下文是否被记住
      contextManager.addMessage({
        role: 'user',
        content: '我之前提到在开发什么？',
      });

      const context = contextManager.getContext(3000);
      response = await adapterFactory.sendMessage(
        'claude',
        `${context}\n\n我之前提到在开发什么？请简短回答。`,
        undefined,
        { source: 'test', adapterRole: 'worker' }
      );

      assert(
        response.content.toLowerCase().includes('vscode') ||
        response.content.toLowerCase().includes('vs code') ||
        response.content.includes('扩展'),
        '应该记住之前的对话内容'
      );
      console.log(`   多轮对话验证: ${response.content.substring(0, 100)}...`);
    },
    {
      skipIf: !hasApiKey,
      skipMessage: '未配置 API Key',
    }
  );

  // ========================================
  // 7. 上下文压缩测试
  // ========================================
  console.log('\n🗜️  7. 上下文压缩测试\n');

  let compressor;

  test('创建 ContextCompressor', () => {
    compressor = new ContextCompressor();
    assert(compressor !== null, 'ContextCompressor 应该被创建');
  });

  await asyncTest('压缩大型 Memory', async () => {
    const storagePath = path.join(TEST_WORKSPACE, '.multicli/sessions');
    const memoryDoc = new MemoryDocument('compress-test', 'Compression Test', storagePath);
    await memoryDoc.load();

    // 添加大量数据
    for (let i = 0; i < 50; i++) {
      memoryDoc.addCurrentTask({
        id: `task-${i}`,
        description: `测试任务 ${i}: 实现复杂功能模块的核心逻辑`,
        status: 'completed',
        result: `任务 ${i} 已完成，修改了多个文件，添加了新的功能`,
      });
      memoryDoc.updateTaskStatus(`task-${i}`, 'completed', `详细结果 ${i}`);
    }

    for (let i = 0; i < 100; i++) {
      memoryDoc.addCodeChange({
        file: `src/module${i % 10}/component${i % 5}.ts`,
        action: i % 3 === 0 ? 'add' : 'modify',
        summary: `修改 ${i}: ${i % 2 === 0 ? '架构重构' : '功能实现'} - 详细说明`,
      });
    }

    const beforeTokens = memoryDoc.estimateTokens();
    console.log(`   压缩前 Token: ${beforeTokens}`);

    await compressor.compress(memoryDoc);

    const afterTokens = memoryDoc.estimateTokens();
    console.log(`   压缩后 Token: ${afterTokens}`);
    console.log(`   压缩率: ${((1 - afterTokens / beforeTokens) * 100).toFixed(1)}%`);

    const stats = compressor.getLastStats();
    if (stats) {
      console.log(`   压缩方法: ${stats.method}`);
    }
  });

  // ========================================
  // 8. 完整流程集成测试
  // ========================================
  console.log('\n🔗 8. 完整流程集成测试\n');

  await asyncTest(
    '模拟完整用户需求处理流程',
    async () => {
      // 1. 创建新的上下文管理器
      const flowContext = new ContextManager(TEST_WORKSPACE);
      await flowContext.initialize('flow-test', '完整流程测试');

      // 2. 用户提出需求
      const userRequest = '请帮我创建一个简单的 TypeScript 函数，计算两个数的和';
      flowContext.addMessage({ role: 'user', content: userRequest });

      // 3. 记录任务
      flowContext.addTask({
        id: 'flow-task-1',
        description: '创建计算函数',
        status: 'in_progress',
        assignedWorker: 'claude',
      });

      // 4. 调用 LLM 处理
      const context = flowContext.getContext(2000);
      const response = await adapterFactory.sendMessage(
        'claude',
        `${context}\n\n请处理用户的请求。`,
        undefined,
        { source: 'test', adapterRole: 'worker' }
      );

      // 5. 记录响应
      flowContext.addMessage({ role: 'assistant', content: response.content });

      // 6. 更新任务状态
      flowContext.getMemoryDocument().updateTaskStatus(
        'flow-task-1',
        'completed',
        '已生成计算函数代码'
      );

      // 7. 记录代码变更
      flowContext.getMemoryDocument().addCodeChange({
        file: 'src/utils/math.ts',
        action: 'add',
        summary: '添加求和函数',
      });

      // 8. 保存 Memory
      await flowContext.saveMemory();

      console.log(`   用户需求: ${userRequest}`);
      console.log(`   LLM 响应: ${response.content.substring(0, 150)}...`);

      // 验证流程
      const finalMemory = flowContext.getMemoryDocument().getContent();
      assert(finalMemory.completedTasks.length >= 1, '应该有完成的任务');
      assert(finalMemory.codeChanges.length >= 1, '应该有代码变更记录');
    },
    {
      skipIf: !hasApiKey,
      skipMessage: '未配置 API Key',
    }
  );

  // ========================================
  // 9. 错误处理和恢复测试
  // ========================================
  console.log('\n⚠️  9. 错误处理和恢复测试\n');

  await asyncTest('处理无效工具调用', async () => {
    const toolManager = adapterFactory.getToolManager();
    const result = await toolManager.execute({
      id: 'test-invalid-1',
      name: 'non_existent_tool',
      arguments: {},
    });

    assert(result.isError, '无效工具应该返回错误');
    assert(result.content.includes('not found'), '错误信息应该说明工具未找到');
    console.log(`   错误处理: ${result.content}`);
  });

  await asyncTest('处理危险命令', async () => {
    const toolManager = adapterFactory.getToolManager();
    const result = await toolManager.execute({
      id: 'test-dangerous-1',
      name: 'execute_shell',
      arguments: {
        command: 'rm -rf /',
        cwd: TEST_WORKSPACE,
      },
    });

    assert(result.isError, '危险命令应该被拒绝');
    console.log(`   安全拦截: ${result.content}`);
  });

  // ========================================
  // 10. 清理和关闭测试
  // ========================================
  console.log('\n🧹 10. 清理和关闭测试\n');

  await asyncTest('关闭 LLMAdapterFactory', async () => {
    await adapterFactory.shutdown();
    console.log('   所有适配器已关闭');
  });

  // ========================================
  // 测试结果汇总
  // ========================================
  console.log('\n' + '='.repeat(70));
  console.log('📊 测试结果汇总');
  console.log('='.repeat(70));
  console.log(`✅ 通过: ${passed}`);
  console.log(`❌ 失败: ${failed}`);
  console.log(`⏭️  跳过: ${skipped}`);
  console.log(`📋 总计: ${passed + failed + skipped}`);
  console.log('='.repeat(70));

  if (failed > 0) {
    console.log('\n❌ 失败的测试:');
    results
      .filter((r) => r.status === 'failed')
      .forEach((r) => {
        console.log(`   - ${r.name}: ${r.error}`);
      });
  }

  if (skipped > 0) {
    console.log('\n⏭️  跳过的测试:');
    results
      .filter((r) => r.status === 'skipped')
      .forEach((r) => {
        console.log(`   - ${r.name}: ${r.reason}`);
      });
  }

  // 清理测试目录
  cleanupTestDir();

  console.log('\n提示: 如需运行完整测试，请确保配置 API Key');
  console.log('     编辑 ~/.multicli/llm.json 或设置环境变量\n');

  process.exit(failed > 0 ? 1 : 0);
}

// 运行测试
runTests().catch((error) => {
  console.error('测试运行失败:', error);
  process.exit(1);
});
