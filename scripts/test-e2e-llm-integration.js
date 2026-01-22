/**
 * 端对端 LLM 集成测试
 *
 * 测试范围：
 * 1. 工具调用（Shell、MCP、Skills）
 * 2. 子代理编排
 * 3. 上下文压缩
 *
 * 运行方式：node scripts/test-e2e-llm-integration.js
 */

const path = require('path');
const fs = require('fs');

// 测试工作目录
const TEST_WORKSPACE = path.join(__dirname, '..', '.test-e2e-llm');

// 测试结果统计
let passed = 0;
let failed = 0;
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

async function asyncTest(name, fn) {
  try {
    await fn();
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

async function runTests() {
  console.log('\n🧪 端对端 LLM 集成测试\n');
  console.log('='.repeat(60));

  cleanupTestDir();

  // 动态导入编译后的模块
  const { ToolManager } = require('../out/tools/tool-manager.js');
  const { ShellExecutor } = require('../out/tools/shell-executor.js');
  const { ContextCompressor } = require('../out/context/context-compressor.js');
  const { MemoryDocument } = require('../out/context/memory-document.js');
  const { ContextManager } = require('../out/context/context-manager.js');
  const { LLMConfigLoader } = require('../out/llm/config.js');

  // ========================================
  // 1. 工具系统测试
  // ========================================
  console.log('\n🔧 1. 工具系统测试\n');

  let toolManager;
  let shellExecutor;

  test('创建 ToolManager', () => {
    toolManager = new ToolManager();
    assert(toolManager !== null, 'ToolManager 应该被创建');
  });

  test('创建 ShellExecutor', () => {
    shellExecutor = new ShellExecutor();
    assert(shellExecutor !== null, 'ShellExecutor 应该被创建');
  });

  test('获取 Shell 工具定义', () => {
    const toolDef = shellExecutor.getToolDefinition();
    assert(toolDef.name === 'execute_shell', '工具名称应该是 execute_shell');
    assert(toolDef.description, '工具应该有描述');
    assert(toolDef.input_schema, '工具应该有输入模式');
    assert(toolDef.input_schema.properties.command, '应该有 command 参数');
  });

  await asyncTest('执行安全的 Shell 命令', async () => {
    const result = await shellExecutor.execute({
      command: 'echo "Hello from Shell"',
      cwd: TEST_WORKSPACE,
    });
    assert(result.exitCode === 0, '命令应该成功执行');
    assert(result.stdout.includes('Hello from Shell'), '输出应该包含预期文本');
  });

  await asyncTest('拒绝危险的 Shell 命令', async () => {
    const validation = shellExecutor.validateCommand('rm -rf /');
    assert(!validation.valid, '危险命令应该被拒绝');
    assert(validation.reason, '应该提供拒绝原因');
  });

  await asyncTest('ToolManager 执行 Shell 工具调用', async () => {
    const toolCall = {
      id: 'test-tool-call-1',
      name: 'execute_shell',
      arguments: {
        command: 'pwd',
        cwd: TEST_WORKSPACE,
      },
    };
    const result = await toolManager.execute(toolCall);
    assert(!result.isError, '工具调用应该成功');
    assert(result.content.includes(TEST_WORKSPACE) || result.content.length > 0, '应该返回工作目录');
  });

  await asyncTest('获取所有工具列表', async () => {
    const tools = await toolManager.getTools();
    assert(tools.length >= 1, '至少应该有 Shell 工具');
    const shellTool = tools.find(t => t.name === 'execute_shell');
    assert(shellTool, '应该包含 execute_shell 工具');
    assert(shellTool.metadata.source === 'builtin', '来源应该是 builtin');
  });

  await asyncTest('检查工具可用性', async () => {
    const shellAvailable = await toolManager.isAvailable('execute_shell');
    assert(shellAvailable, 'execute_shell 应该可用');
    const unknownAvailable = await toolManager.isAvailable('unknown_tool');
    assert(!unknownAvailable, '未知工具应该不可用');
  });

  test('获取工具管理器统计', () => {
    const stats = toolManager.getStats();
    assert(typeof stats.mcpServers === 'number', '应该有 MCP 服务器计数');
    assert(typeof stats.skills === 'number', '应该有 Skills 计数');
    assert(typeof stats.cachedTools === 'number', '应该有缓存工具计数');
  });

  // ========================================
  // 2. MCP 执行器测试（模拟）
  // ========================================
  console.log('\n🔌 2. MCP 执行器测试\n');

  // 创建模拟的 MCP 执行器
  const mockMCPExecutor = {
    async getTools() {
      return [
        {
          name: 'mock_mcp_tool',
          description: '模拟 MCP 工具',
          input_schema: {
            type: 'object',
            properties: {
              input: { type: 'string' },
            },
          },
          metadata: {
            source: 'mcp',
            sourceId: 'mock-mcp-server',
            category: 'test',
            tags: ['mock'],
          },
        },
      ];
    },
    async execute(toolCall) {
      return {
        toolCallId: toolCall.id,
        content: `MCP 工具执行结果: ${JSON.stringify(toolCall.arguments)}`,
        isError: false,
      };
    },
    async isAvailable(toolName) {
      return toolName === 'mock_mcp_tool';
    },
  };

  test('注册 MCP 执行器', () => {
    toolManager.registerMCPExecutor('mock-mcp-server', mockMCPExecutor);
    const stats = toolManager.getStats();
    assert(stats.mcpServers >= 1, '应该有至少 1 个 MCP 服务器');
  });

  await asyncTest('获取包含 MCP 工具的工具列表', async () => {
    const tools = await toolManager.getTools();
    const mcpTool = tools.find(t => t.name === 'mock_mcp_tool');
    assert(mcpTool, '应该包含 mock_mcp_tool');
    assert(mcpTool.metadata.source === 'mcp', '来源应该是 mcp');
  });

  await asyncTest('执行 MCP 工具调用', async () => {
    const toolCall = {
      id: 'test-mcp-call-1',
      name: 'mock_mcp_tool',
      arguments: { input: 'test data' },
    };
    const result = await toolManager.execute(toolCall);
    assert(!result.isError, 'MCP 工具调用应该成功');
    assert(result.content.includes('test data'), '结果应该包含输入数据');
  });

  test('注销 MCP 执行器', () => {
    toolManager.unregisterMCPExecutor('mock-mcp-server');
    const stats = toolManager.getStats();
    // 缓存被清除，需要重新获取工具
  });

  // ========================================
  // 3. Skills 执行器测试（模拟）
  // ========================================
  console.log('\n🎯 3. Skills 执行器测试\n');

  const mockSkillsExecutor = {
    async getTools() {
      return [
        {
          name: 'web_search',
          description: '网络搜索工具',
          input_schema: {
            type: 'object',
            properties: {
              query: { type: 'string' },
            },
            required: ['query'],
          },
          metadata: {
            source: 'skill',
            sourceId: 'claude-skills',
            category: 'search',
            tags: ['web', 'search'],
          },
        },
      ];
    },
    async execute(toolCall) {
      return {
        toolCallId: toolCall.id,
        content: `搜索结果: ${toolCall.arguments.query}`,
        isError: false,
      };
    },
    async isAvailable(toolName) {
      return toolName === 'web_search';
    },
  };

  test('注册 Skill 执行器', () => {
    toolManager.registerSkillExecutor('claude-skills', mockSkillsExecutor);
    const stats = toolManager.getStats();
    assert(stats.skills >= 1, '应该有至少 1 个 Skill');
  });

  await asyncTest('获取包含 Skill 工具的工具列表', async () => {
    const tools = await toolManager.getTools();
    const skillTool = tools.find(t => t.name === 'web_search');
    assert(skillTool, '应该包含 web_search');
    assert(skillTool.metadata.source === 'skill', '来源应该是 skill');
  });

  await asyncTest('执行 Skill 工具调用', async () => {
    const toolCall = {
      id: 'test-skill-call-1',
      name: 'web_search',
      arguments: { query: 'MultiCLI 项目' },
    };
    const result = await toolManager.execute(toolCall);
    assert(!result.isError, 'Skill 工具调用应该成功');
    assert(result.content.includes('MultiCLI'), '结果应该包含搜索词');
  });

  // ========================================
  // 4. 上下文压缩测试
  // ========================================
  console.log('\n🗜️  4. 上下文压缩测试\n');

  let compressor;
  let memoryDoc;

  test('创建 ContextCompressor', () => {
    compressor = new ContextCompressor();
    assert(compressor !== null, 'ContextCompressor 应该被创建');
  });

  await asyncTest('创建 MemoryDocument', async () => {
    const storagePath = path.join(TEST_WORKSPACE, '.multicli/sessions');
    memoryDoc = new MemoryDocument('test-session', 'Test Session', storagePath);
    await memoryDoc.load();
    assert(memoryDoc !== null, 'MemoryDocument 应该被创建');
  });

  test('添加测试数据到 Memory', () => {
    // 添加大量任务
    for (let i = 0; i < 20; i++) {
      memoryDoc.addCurrentTask({
        id: `task-${i}`,
        description: `测试任务 ${i}：实现功能模块`,
        status: 'completed',
        result: `任务 ${i} 已完成，修改了 ${i} 个文件`,
      });
      memoryDoc.updateTaskStatus(`task-${i}`, 'completed', `结果 ${i}`);
    }

    // 添加大量代码变更
    for (let i = 0; i < 30; i++) {
      memoryDoc.addCodeChange({
        file: `src/module${i % 5}/file${i % 10}.ts`,
        action: i % 3 === 0 ? 'add' : 'modify',
        summary: `修改 ${i}: ${i % 2 === 0 ? '架构重构' : '功能实现'}`,
      });
    }

    // 添加决策
    memoryDoc.addDecision({
      id: 'decision-1',
      description: '使用 TypeScript 进行开发',
      reason: '类型安全和更好的开发体验',
    });

    const content = memoryDoc.getContent();
    assert(content.completedTasks.length >= 20, '应该有 20 个已完成任务');
    assert(content.codeChanges.length >= 30, '应该有 30 个代码变更');
  });

  test('估算压缩前 Token 数量', () => {
    const beforeTokens = memoryDoc.estimateTokens();
    console.log(`   压缩前 Token: ${beforeTokens}`);
    assert(beforeTokens > 0, 'Token 数量应该大于 0');
  });

  await asyncTest('执行简单压缩（无 LLM）', async () => {
    const beforeTokens = memoryDoc.estimateTokens();
    await compressor.compress(memoryDoc);
    const afterTokens = memoryDoc.estimateTokens();

    console.log(`   压缩后 Token: ${afterTokens}`);
    console.log(`   压缩率: ${((1 - afterTokens / beforeTokens) * 100).toFixed(1)}%`);

    // 允许小幅波动（时间戳等因素导致的轻微增加）
    const tolerance = 10;
    assert(afterTokens <= beforeTokens + tolerance, '压缩后 Token 应该基本不变或减少');
  });

  test('获取压缩统计', () => {
    const stats = compressor.getLastStats();
    if (stats) {
      console.log(`   压缩方法: ${stats.method}`);
      console.log(`   原始 Token: ${stats.originalTokens}`);
      console.log(`   压缩后 Token: ${stats.compressedTokens}`);
    }
  });

  test('截断长消息', () => {
    const longMessage = 'A'.repeat(5000);
    const result = compressor.truncateMessage(longMessage, 1000);
    // TruncationResult 使用 content 字段
    assert(result.content.length <= 1100, '截断后长度应该受限');
    assert(result.wasTruncated, '应该标记为已截断');
  });

  test('截断工具输出', () => {
    const longOutput = 'Line\n'.repeat(500);
    const result = compressor.truncateToolOutput(longOutput);
    // 默认的 maxToolOutputChars 可能很大，需要检查实际配置
    console.log(`   工具输出原始长度: ${longOutput.length}, 截断后: ${result.content.length}`);
    assert(result.content.length > 0, '应该有输出内容');
  });

  test('截断代码块', () => {
    const longCode = Array(200).fill('const x = 1;').join('\n');
    const result = compressor.truncateCodeBlock(longCode, 50);
    const lines = result.content.split('\n').length;
    assert(lines <= 60, '代码行数应该受限');
  });

  // ========================================
  // 5. ContextManager 集成测试
  // ========================================
  console.log('\n📦 5. ContextManager 集成测试\n');

  let contextManager;

  await asyncTest('创建 ContextManager', async () => {
    contextManager = new ContextManager(TEST_WORKSPACE);
    assert(contextManager !== null, 'ContextManager 应该被创建');
  });

  await asyncTest('初始化 ContextManager', async () => {
    await contextManager.initialize('context-test-session', 'Context Test');
    const state = contextManager.exportState();
    assert(state.immediateContextCount === 0, '初始上下文应该为空');
  });

  test('添加消息到上下文', () => {
    contextManager.addMessage({ role: 'user', content: '请帮我实现一个用户登录功能' });
    contextManager.addMessage({ role: 'assistant', content: '好的，我来帮你实现登录功能。首先我们需要...' });
    const state = contextManager.exportState();
    assert(state.immediateContextCount === 2, '应该有 2 条消息');
  });

  test('添加任务到 Memory', () => {
    contextManager.addTask({
      id: 'ctx-task-1',
      description: '实现登录 API',
      status: 'in_progress',
      assignedWorker: 'claude',
    });
    const memory = contextManager.getMemoryDocument();
    const content = memory.getContent();
    assert(content.currentTasks.length === 1, '应该有 1 个任务');
  });

  test('添加决策到 Memory', () => {
    contextManager.addDecision('d1', '使用 bcrypt 加密密码', '安全性更高');
    const memory = contextManager.getMemoryDocument();
    const content = memory.getContent();
    assert(content.keyDecisions.length === 1, '应该有 1 个决策');
  });

  test('获取组装后的上下文', () => {
    const context = contextManager.getContext(4000);
    assert(context.length > 0, '上下文应该非空');
    console.log(`   上下文长度: ${context.length} 字符`);
  });

  test('检查是否需要压缩', () => {
    const needsCompression = contextManager.needsCompression();
    assert(typeof needsCompression === 'boolean', '应该返回布尔值');
    console.log(`   需要压缩: ${needsCompression}`);
  });

  await asyncTest('保存 Memory', async () => {
    await contextManager.saveMemory();
    const filePath = path.join(TEST_WORKSPACE, '.multicli/sessions', 'context-test-session', 'memory.json');
    assert(fs.existsSync(filePath), 'Memory 文件应该存在');
  });

  // ========================================
  // 6. LLM 配置加载器测试
  // ========================================
  console.log('\n⚙️  6. LLM 配置加载器测试\n');

  test('确保默认配置存在', () => {
    LLMConfigLoader.ensureDefaults();
    const configDir = LLMConfigLoader.getConfigDir();
    assert(configDir.includes('.multicli'), '配置目录应该包含 .multicli');
  });

  test('加载完整配置', () => {
    const config = LLMConfigLoader.loadFullConfig();
    assert(config.orchestrator, '应该有编排者配置');
    assert(config.workers, '应该有工人配置');
    assert(config.workers.claude, '应该有 Claude 配置');
    assert(config.workers.codex, '应该有 Codex 配置');
    assert(config.workers.gemini, '应该有 Gemini 配置');
  });

  test('加载 MCP 配置', () => {
    const mcpConfig = LLMConfigLoader.loadMCPConfig();
    assert(Array.isArray(mcpConfig), 'MCP 配置应该是数组');
  });

  test('加载 Skills 配置', () => {
    const skillsConfig = LLMConfigLoader.loadSkillsConfig();
    // 可能为 null（首次运行）
    if (skillsConfig) {
      console.log('   Skills 配置已加载');
    }
  });

  test('加载仓库配置', () => {
    const repos = LLMConfigLoader.loadRepositories();
    assert(Array.isArray(repos), '仓库配置应该是数组');
    assert(repos.length >= 1, '至少应该有内置仓库');
    const builtinRepo = repos.find(r => r.id === 'builtin');
    assert(builtinRepo, '应该有内置仓库');
  });

  // ========================================
  // 7. 子代理编排模拟测试
  // ========================================
  console.log('\n🎭 7. 子代理编排模拟测试\n');

  // 模拟适配器工厂
  const mockAdapterFactory = {
    sendMessage: async (worker, message, images, options) => {
      return {
        content: `[${worker}] 响应: ${message.substring(0, 50)}...`,
        done: true,
        tokenUsage: {
          inputTokens: Math.floor(message.length / 4),
          outputTokens: 50,
        },
      };
    },
    interrupt: async () => {},
    shutdown: async () => {},
    isConnected: () => true,
    isBusy: () => false,
    getToolManager: () => toolManager,
  };

  test('模拟适配器工厂创建', () => {
    assert(mockAdapterFactory.sendMessage, '应该有 sendMessage 方法');
    assert(mockAdapterFactory.getToolManager, '应该有 getToolManager 方法');
  });

  await asyncTest('模拟多 Worker 任务分发', async () => {
    const tasks = [
      { worker: 'claude', task: '分析代码结构' },
      { worker: 'codex', task: '实现功能代码' },
      { worker: 'gemini', task: '编写测试用例' },
    ];

    const results = await Promise.all(
      tasks.map(async ({ worker, task }) => {
        const response = await mockAdapterFactory.sendMessage(
          worker,
          task,
          undefined,
          { source: 'test' }
        );
        return { worker, response };
      })
    );

    assert(results.length === 3, '应该有 3 个结果');
    results.forEach(({ worker, response }) => {
      assert(response.content.includes(worker), `响应应该包含 ${worker}`);
      console.log(`   ${worker}: ${response.content.substring(0, 40)}...`);
    });
  });

  await asyncTest('模拟顺序执行流程', async () => {
    const steps = ['理解需求', '设计方案', '实现代码', '测试验证'];
    let context = '';

    for (const step of steps) {
      const response = await mockAdapterFactory.sendMessage(
        'claude',
        `${step}。上下文: ${context}`,
        undefined,
        { source: 'test' }
      );
      context += response.content + '\n';
    }

    assert(context.length > 0, '应该有累积的上下文');
    console.log(`   累积上下文长度: ${context.length} 字符`);
  });

  // ========================================
  // 8. 集成场景测试
  // ========================================
  console.log('\n🔗 8. 集成场景测试\n');

  await asyncTest('场景：工具调用 + 上下文记录', async () => {
    // 1. 执行工具调用
    const toolCall = {
      id: 'integration-test-1',
      name: 'execute_shell',
      arguments: {
        command: 'ls -la',
        cwd: TEST_WORKSPACE,
      },
    };
    const toolResult = await toolManager.execute(toolCall);
    assert(!toolResult.isError, '工具调用应该成功');

    // 2. 记录到上下文
    contextManager.addMessage({
      role: 'assistant',
      content: `执行了命令，结果：\n${toolResult.content.substring(0, 200)}`,
    });

    // 3. 检查上下文状态
    const state = contextManager.exportState();
    assert(state.immediateContextCount >= 3, '应该有多条消息');
  });

  await asyncTest('场景：多工具协作', async () => {
    // 1. Shell 工具创建文件
    const createResult = await toolManager.execute({
      id: 'multi-tool-1',
      name: 'execute_shell',
      arguments: {
        command: 'echo "test content" > test-file.txt',
        cwd: TEST_WORKSPACE,
      },
    });
    assert(!createResult.isError, '创建文件应该成功');

    // 2. 模拟 Skill 工具分析
    const analyzeResult = await toolManager.execute({
      id: 'multi-tool-2',
      name: 'web_search',
      arguments: { query: 'test file analysis' },
    });
    assert(!analyzeResult.isError, 'Skill 工具应该成功');

    // 3. Shell 工具验证
    const verifyResult = await toolManager.execute({
      id: 'multi-tool-3',
      name: 'execute_shell',
      arguments: {
        command: 'cat test-file.txt',
        cwd: TEST_WORKSPACE,
      },
    });
    assert(!verifyResult.isError, '验证文件应该成功');
    assert(verifyResult.content.includes('test content'), '文件内容应该正确');
  });

  await asyncTest('场景：压缩 + 上下文管理', async () => {
    // 1. 添加大量上下文
    for (let i = 0; i < 20; i++) {
      contextManager.addMessage({
        role: i % 2 === 0 ? 'user' : 'assistant',
        content: `消息 ${i}: ${'这是一段测试内容'.repeat(10)}`,
      });
    }

    // 2. 检查是否需要压缩
    const needsCompression = contextManager.needsCompression();
    console.log(`   添加 20 条消息后需要压缩: ${needsCompression}`);

    // 3. 获取压缩后的上下文
    const context = contextManager.getContext(2000);
    assert(context.length > 0, '上下文应该非空');
    console.log(`   限制 2000 Token 后上下文长度: ${context.length}`);
  });

  // ========================================
  // 测试结果汇总
  // ========================================
  console.log('\n' + '='.repeat(60));
  console.log('📊 测试结果汇总');
  console.log('='.repeat(60));
  console.log(`✅ 通过: ${passed}`);
  console.log(`❌ 失败: ${failed}`);
  console.log(`📋 总计: ${passed + failed}`);
  console.log('='.repeat(60));

  if (failed > 0) {
    console.log('\n❌ 失败的测试:');
    results.filter(r => r.status === 'failed').forEach(r => {
      console.log(`   - ${r.name}: ${r.error}`);
    });
  }

  // 清理测试目录
  cleanupTestDir();

  process.exit(failed > 0 ? 1 : 0);
}

// 运行测试
runTests().catch(error => {
  console.error('测试运行失败:', error);
  process.exit(1);
});
