/**
 * 上下文管理系统测试
 * 测试 MemoryDocument、ContextManager、ContextCompressor 的完整流程
 */

const path = require('path');
const fs = require('fs');

// 测试配置
const TEST_WORKSPACE = path.join(__dirname, '..', '.test-context');
const TEST_SESSION_ID = 'test-session-001';

// 清理测试目录
function cleanupTestDir() {
  if (fs.existsSync(TEST_WORKSPACE)) {
    fs.rmSync(TEST_WORKSPACE, { recursive: true });
  }
}

// 测试结果统计
let passed = 0;
let failed = 0;

function test(name, fn) {
  try {
    fn();
    console.log(`✅ ${name}`);
    passed++;
  } catch (error) {
    console.log(`❌ ${name}`);
    console.log(`   错误: ${error.message}`);
    failed++;
  }
}

async function asyncTest(name, fn) {
  try {
    await fn();
    console.log(`✅ ${name}`);
    passed++;
  } catch (error) {
    console.log(`❌ ${name}`);
    console.log(`   错误: ${error.message}`);
    failed++;
  }
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message || 'Assertion failed');
  }
}

async function runTests() {
  console.log('\n🧪 开始测试上下文管理系统\n');
  console.log('='.repeat(60));

  // 清理之前的测试数据
  cleanupTestDir();

  // 动态导入模块（从编译后的 out 目录）
  const { MemoryDocument } = require('../out/context/memory-document.js');
  const { ContextManager } = require('../out/context/context-manager.js');
  const { ContextCompressor } = require('../out/context/context-compressor.js');
  const { createEmptyMemoryContent } = require('../out/context/types.js');

  // ========================================
  // 1. 测试 MemoryDocument
  // ========================================
  console.log('\n📄 1. MemoryDocument 测试\n');

  let memoryDoc;
  
  await asyncTest('创建 MemoryDocument', async () => {
    const storagePath = path.join(TEST_WORKSPACE, '.cli-arranger/sessions');
    memoryDoc = new MemoryDocument(TEST_SESSION_ID, 'Test Session', storagePath);
    assert(memoryDoc !== null, 'MemoryDocument 应该被创建');
  });

  await asyncTest('加载/保存 Memory', async () => {
    await memoryDoc.load();
    const content = memoryDoc.getContent();
    assert(content.sessionId === TEST_SESSION_ID, 'sessionId 应该匹配');
    assert(content.sessionName === 'Test Session', 'sessionName 应该匹配');
  });

  test('添加当前任务', () => {
    memoryDoc.addCurrentTask({
      id: 'task-1',
      description: '实现用户登录功能',
      status: 'in_progress',
      assignedWorker: 'claude'
    });
    const content = memoryDoc.getContent();
    assert(content.currentTasks.length === 1, '应该有1个当前任务');
    assert(content.currentTasks[0].id === 'task-1', '任务ID应该匹配');
  });

  test('更新任务状态', () => {
    memoryDoc.updateTaskStatus('task-1', 'completed', '登录功能已实现');
    const content = memoryDoc.getContent();
    assert(content.currentTasks.length === 0, '当前任务应该为空');
    assert(content.completedTasks.length === 1, '已完成任务应该有1个');
    assert(content.completedTasks[0].result === '登录功能已实现', '结果应该匹配');
  });

  test('添加关键决策', () => {
    memoryDoc.addDecision({
      id: 'decision-1',
      description: '使用 JWT 进行身份验证',
      reason: 'JWT 无状态，适合分布式系统'
    });
    const content = memoryDoc.getContent();
    assert(content.keyDecisions.length === 1, '应该有1个关键决策');
  });

  test('添加代码变更', () => {
    memoryDoc.addCodeChange({
      file: 'src/auth/login.ts',
      action: 'add',
      summary: '添加登录接口'
    });
    memoryDoc.addCodeChange({
      file: 'src/auth/login.ts',
      action: 'modify',
      summary: '修复登录验证逻辑'
    });
    const content = memoryDoc.getContent();
    assert(content.codeChanges.length === 2, '应该有2个代码变更');
  });

  test('添加重要上下文', () => {
    memoryDoc.addImportantContext('项目使用 TypeScript + Express');
    memoryDoc.addImportantContext('数据库使用 PostgreSQL');
    const content = memoryDoc.getContent();
    assert(content.importantContext.length === 2, '应该有2个重要上下文');
  });

  test('转换为 Markdown', () => {
    const markdown = memoryDoc.toMarkdown();
    assert(markdown.includes('Test Session'), 'Markdown 应该包含会话名称');
    assert(markdown.includes('已完成任务'), 'Markdown 应该包含已完成任务');
    assert(markdown.includes('关键决策'), 'Markdown 应该包含关键决策');
  });

  test('估算 Token 数量', () => {
    const tokens = memoryDoc.estimateTokens();
    assert(tokens > 0, 'Token 数量应该大于0');
    console.log(`   Token 估算: ${tokens}`);
  });

  await asyncTest('保存 Memory 到文件', async () => {
    await memoryDoc.save();
    const filePath = path.join(TEST_WORKSPACE, '.cli-arranger/sessions', TEST_SESSION_ID, 'memory.json');
    assert(fs.existsSync(filePath), 'Memory 文件应该存在');
  });

  // ========================================
  // 2. 测试 ContextManager
  // ========================================
  console.log('\n📦 2. ContextManager 测试\n');

  let contextManager;

  await asyncTest('创建 ContextManager', async () => {
    contextManager = new ContextManager(TEST_WORKSPACE);
    assert(contextManager !== null, 'ContextManager 应该被创建');
  });

  await asyncTest('初始化 ContextManager', async () => {
    await contextManager.initialize('session-002', 'Context Test Session');
    const state = contextManager.exportState();
    assert(state.immediateContextCount === 0, '初始即时上下文应该为空');
  });

  test('添加消息到即时上下文', () => {
    contextManager.addMessage({ role: 'user', content: '请帮我实现一个登录功能' });
    contextManager.addMessage({ role: 'assistant', content: '好的，我来帮你实现登录功能...' });
    const state = contextManager.exportState();
    assert(state.immediateContextCount === 2, '应该有2条消息');
  });

  test('添加任务到 Memory', () => {
    contextManager.addTask({
      id: 'ctx-task-1',
      description: '实现登录 API',
      status: 'in_progress',
      assignedWorker: 'codex'
    });
    const memory = contextManager.getMemoryDocument();
    const content = memory.getContent();
    assert(content.currentTasks.length === 1, '应该有1个任务');
  });

  test('添加决策到 Memory', () => {
    contextManager.addDecision('d1', '使用 bcrypt 加密密码', '安全性更高');
    const memory = contextManager.getMemoryDocument();
    const content = memory.getContent();
    assert(content.keyDecisions.length === 1, '应该有1个决策');
  });

  test('获取组装后的上下文', () => {
    const context = contextManager.getContext(4000);
    assert(context.includes('会话上下文') || context.includes('最近对话'), '应该包含上下文内容');
    console.log(`   上下文长度: ${context.length} 字符`);
  });

  test('检查是否需要压缩', () => {
    const needsCompression = contextManager.needsCompression();
    assert(typeof needsCompression === 'boolean', '应该返回布尔值');
    console.log(`   需要压缩: ${needsCompression}`);
  });

  await asyncTest('保存 Memory', async () => {
    await contextManager.saveMemory();
    const filePath = path.join(TEST_WORKSPACE, '.cli-arranger/sessions', 'session-002', 'memory.json');
    assert(fs.existsSync(filePath), 'Memory 文件应该存在');
  });

  // ========================================
  // 3. 测试 ContextCompressor
  // ========================================
  console.log('\n🗜️  3. ContextCompressor 测试\n');

  let compressor;

  test('创建 ContextCompressor', () => {
    compressor = new ContextCompressor();
    assert(compressor !== null, 'ContextCompressor 应该被创建');
  });

  await asyncTest('简单压缩（不需要 LLM）', async () => {
    // 创建一个有大量数据的 Memory
    const storagePath = path.join(TEST_WORKSPACE, '.cli-arranger/sessions');
    const testMemory = new MemoryDocument('compress-test', 'Compress Test', storagePath);
    await testMemory.load();

    // 添加大量已完成任务
    for (let i = 0; i < 20; i++) {
      testMemory.addCurrentTask({
        id: `task-${i}`,
        description: `测试任务 ${i}`,
        status: 'completed',
        result: `任务 ${i} 完成`
      });
      testMemory.updateTaskStatus(`task-${i}`, 'completed', `结果 ${i}`);
    }

    // 添加大量代码变更
    for (let i = 0; i < 30; i++) {
      testMemory.addCodeChange({
        file: `src/file${i % 5}.ts`,
        action: 'modify',
        summary: `修改 ${i}`
      });
    }

    const beforeTokens = testMemory.estimateTokens();
    console.log(`   压缩前 Token: ${beforeTokens}`);

    await compressor.compress(testMemory);

    const afterTokens = testMemory.estimateTokens();
    console.log(`   压缩后 Token: ${afterTokens}`);

    assert(afterTokens <= beforeTokens, '压缩后 Token 应该减少或不变');
  });

  // ========================================
  // 测试结果汇总
  // ========================================
  console.log('\n' + '='.repeat(60));
  console.log('📊 测试结果汇总');
  console.log('='.repeat(60));
  console.log(`✅ 通过: ${passed}`);
  console.log(`❌ 失败: ${failed}`);
  console.log('='.repeat(60));

  // 清理测试目录
  cleanupTestDir();

  process.exit(failed > 0 ? 1 : 0);
}

// 运行测试
runTests().catch(error => {
  console.error('测试运行失败:', error);
  process.exit(1);
});

