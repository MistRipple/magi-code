/**
 * 会话管理系统测试
 * 测试 Phase 1 实现的会话总结生成、切换、注入等功能
 */

const path = require('path');
const fs = require('fs');

// 测试配置
const TEST_WORKSPACE = path.join(__dirname, '..', '.test-session-management');
const TEST_SESSION_1 = 'test-session-001';
const TEST_SESSION_2 = 'test-session-002';

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
    console.log(`   堆栈: ${error.stack}`);
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
    console.log(`   堆栈: ${error.stack}`);
    failed++;
  }
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message || 'Assertion failed');
  }
}

async function runTests() {
  console.log('\n🧪 开始测试会话管理系统 (Phase 1)\n');
  console.log('='.repeat(80));

  // 清理之前的测试数据
  cleanupTestDir();

  // 动态导入模块（从编译后的 out 目录）
  const { UnifiedSessionManager } = require('../out/session/unified-session-manager.js');
  const { ContextManager } = require('../out/context/context-manager.js');
  const { TruncationUtils } = require('../out/context/truncation-utils.js');

  let sessionManager;
  let contextManager;

  // ========================================
  // 1. 测试 UnifiedSessionManager 基础功能
  // ========================================
  console.log('\n📋 1. UnifiedSessionManager 基础功能测试\n');

  await asyncTest('创建 UnifiedSessionManager', async () => {
    const storagePath = path.join(TEST_WORKSPACE, '.multicli/sessions');
    sessionManager = new UnifiedSessionManager(storagePath);
    assert(sessionManager !== null, 'SessionManager 应该被创建');
  });

  await asyncTest('创建新会话', async () => {
    const session = await sessionManager.createSession('测试会话 1');
    assert(session !== null, '会话应该被创建');
    assert(session.name === '测试会话 1', '会话名称应该匹配');
    assert(session.messages.length === 0, '新会话应该没有消息');
    assert(session.tasks.length === 0, '新会话应该没有任务');
  });

  await asyncTest('添加消息到会话', async () => {
    sessionManager.addMessage(
      'user',
      '请帮我实现用户认证功能'
    );

    sessionManager.addMessage(
      'assistant',
      '好的，我决定使用 JWT 认证方案。我会创建以下文件：\n1. src/auth/jwt.ts\n2. src/middleware/auth.ts'
    );

    const session = sessionManager.getCurrentSession();
    assert(session.messages.length === 2, '应该有2条消息');
  });

  await asyncTest('添加任务到会话', async () => {
    const session = sessionManager.getCurrentSession();

    sessionManager.addTask(session.id, {
      id: 'task-1',
      sessionId: session.id,
      prompt: '实现 JWT token 生成',
      status: 'completed',
      priority: 1,
      subTasks: [],
      createdAt: Date.now(),
      retryCount: 0,
      maxRetries: 3
    });

    sessionManager.addTask(session.id, {
      id: 'task-2',
      sessionId: session.id,
      prompt: '实现认证中间件',
      status: 'running',
      priority: 1,
      subTasks: [],
      createdAt: Date.now(),
      retryCount: 0,
      maxRetries: 3
    });

    const updatedSession = sessionManager.getCurrentSession();
    assert(updatedSession.tasks.length === 2, '应该有2个任务');
  });

  await asyncTest('添加快照到会话', async () => {
    const session = sessionManager.getCurrentSession();

    sessionManager.addSnapshot(session.id, {
      id: 'snapshot-1',
      filePath: 'src/auth/jwt.ts',
      lastModifiedBy: 'claude',
      lastModifiedAt: Date.now(),
      subTaskId: 'task-1',
      priority: 1
    });

    sessionManager.addSnapshot(session.id, {
      id: 'snapshot-2',
      filePath: 'src/middleware/auth.ts',
      lastModifiedBy: 'claude',
      lastModifiedAt: Date.now(),
      subTaskId: 'task-2',
      priority: 1
    });

    const updatedSession = sessionManager.getCurrentSession();
    assert(updatedSession.snapshots.length === 2, '应该有2个快照');
  });

  // ========================================
  // 2. 测试会话总结生成 (Task 1.1)
  // ========================================
  console.log('\n📊 2. 会话总结生成测试 (Task 1.1)\n');

  let summary;

  await asyncTest('生成会话总结', async () => {
    summary = sessionManager.getSessionSummary();
    assert(summary !== null, '应该生成会话总结');
    assert(summary.sessionId !== null, '总结应该有 sessionId');
    assert(summary.title === '测试会话 1', '总结标题应该匹配');
  });

  test('总结包含已完成任务', () => {
    assert(summary.completedTasks.length === 1, '应该有1个已完成任务');
    assert(summary.completedTasks[0].includes('JWT token'), '应该包含任务描述');
  });

  test('总结包含进行中任务', () => {
    assert(summary.inProgressTasks.length === 1, '应该有1个进行中任务');
    assert(summary.inProgressTasks[0].includes('认证中间件'), '应该包含任务描述');
  });

  test('总结包含代码变更', () => {
    assert(summary.codeChanges.length === 2, '应该有2个代码变更');
    assert(summary.codeChanges[0].includes('jwt.ts'), '应该包含文件路径');
  });

  test('总结包含关键决策', () => {
    // 应该提取到 "决定使用 JWT 认证方案"
    assert(summary.keyDecisions.length >= 1, '应该至少有1个关键决策');
    const hasJwtDecision = summary.keyDecisions.some(d => d.includes('JWT'));
    assert(hasJwtDecision, '应该包含 JWT 相关决策');
  });

  test('总结包含消息数量', () => {
    assert(summary.messageCount === 2, '消息数量应该是2');
  });

  test('格式化会话总结', () => {
    const formatted = sessionManager.formatSessionSummary(summary);
    assert(typeof formatted === 'string', '格式化结果应该是字符串');
    assert(formatted.includes('会话总结'), '应该包含标题');
    assert(formatted.includes('已完成任务'), '应该包含已完成任务部分');
    assert(formatted.includes('代码变更'), '应该包含代码变更部分');
  });

  // ========================================
  // 3. 测试会话切换和元数据 (Task 1.2 & 1.3)
  // ========================================
  console.log('\n🔄 3. 会话切换和元数据测试 (Task 1.2 & 1.3)\n');

  await asyncTest('创建第二个会话', async () => {
    const session2 = await sessionManager.createSession('测试会话 2');
    assert(session2 !== null, '第二个会话应该被创建');
    assert(session2.name === '测试会话 2', '会话名称应该匹配');
  });

  await asyncTest('获取会话元数据列表', async () => {
    const metas = sessionManager.getSessionMetas();
    console.log('   调试: 会话元数据列表:', JSON.stringify(metas.map(m => ({ id: m.id, name: m.name, messageCount: m.messageCount })), null, 2));
    assert(metas.length === 2, '应该有2个会话');
    // 注意：会话可能按时间倒序排列（最新的在前）
    const hasSession1 = metas.some(m => m.name === '测试会话 1');
    const hasSession2 = metas.some(m => m.name === '测试会话 2');
    assert(hasSession1, '应该包含测试会话 1');
    assert(hasSession2, '应该包含测试会话 2');
    assert(typeof metas[0].messageCount === 'number', '应该有消息数量');
    assert(typeof metas[0].preview === 'string', '应该有预览文本');
  });

  await asyncTest('切换到第一个会话', async () => {
    const metas = sessionManager.getSessionMetas();
    // 找到"测试会话 1"
    const session1Meta = metas.find(m => m.name === '测试会话 1');
    assert(session1Meta !== undefined, '应该找到测试会话 1');

    const session = await sessionManager.switchSession(session1Meta.id);
    console.log('   调试: 切换后的会话:', { id: session?.id, name: session?.name, messageCount: session?.messages.length });
    assert(session !== null, '应该成功切换会话');
    assert(session.name === '测试会话 1', '应该切换到第一个会话');
    assert(session.messages.length === 2, '应该加载历史消息');
  });

  await asyncTest('重命名会话', async () => {
    const currentId = sessionManager.getCurrentSession().id;
    await sessionManager.renameSession(currentId, '用户认证功能实现');
    const session = sessionManager.getCurrentSession();
    assert(session.name === '用户认证功能实现', '会话名称应该被更新');
  });

  // ========================================
  // 4. 测试会话总结注入到上下文 (Task 1.4)
  // ========================================
  console.log('\n💉 4. 会话总结注入到上下文测试 (Task 1.4)\n');

  await asyncTest('创建 ContextManager', async () => {
    const storagePath = path.join(TEST_WORKSPACE, '.multicli/sessions');
    const truncationUtils = new TruncationUtils();
    contextManager = new ContextManager(
      TEST_SESSION_1,
      'Test Session',
      storagePath,
      truncationUtils
    );
    assert(contextManager !== null, 'ContextManager 应该被创建');
  });

  test('设置 SessionManager', () => {
    contextManager.setSessionManager(sessionManager);
    contextManager.setCurrentSessionId(sessionManager.getCurrentSession().id);
    // 不抛出异常即为成功
    assert(true, '应该成功设置 SessionManager');
  });

  test('获取包含会话总结的上下文切片', () => {
    const contextSlice = contextManager.getContextSlice({
      maxTokens: 8000,
      memoryRatio: 0.3,
      memorySummary: {
        includeCurrentTasks: true,
        includeCompletedTasks: 5,
        includeKeyDecisions: 3,
        includeCodeChanges: 10,
        includeImportantContext: true,
        includePendingIssues: true
      }
    });

    assert(typeof contextSlice === 'string', '上下文切片应该是字符串');
    assert(contextSlice.length > 0, '上下文切片不应该为空');

    // 验证包含会话总结
    assert(contextSlice.includes('会话总结') || contextSlice.includes('Session Summary'),
      '应该包含会话总结标题');
    assert(contextSlice.includes('用户认证功能实现'), '应该包含会话名称');
  });

  test('验证 Token 预算分配', () => {
    const contextSlice = contextManager.getContextSlice({
      maxTokens: 4000,
      memoryRatio: 0.3,
      memorySummary: {
        includeCurrentTasks: true,
        includeCompletedTasks: 5,
        includeKeyDecisions: 3,
        includeCodeChanges: 10
      }
    });

    // 粗略估算：中文约 1.5 字符/token，英文约 4 字符/token
    const estimatedTokens = contextSlice.length / 2.5;
    assert(estimatedTokens <= 4000, `上下文应该在 token 预算内 (估算: ${estimatedTokens.toFixed(0)} tokens)`);
  });

  // ========================================
  // 5. 测试边界情况
  // ========================================
  console.log('\n🔍 5. 边界情况测试\n');

  await asyncTest('空会话的总结', async () => {
    const emptySession = await sessionManager.createSession('空会话');
    const emptySummary = sessionManager.getSessionSummary();
    assert(emptySummary !== null, '空会话应该也能生成总结');
    assert(emptySummary.completedTasks.length === 0, '空会话应该没有已完成任务');
    assert(emptySummary.messageCount === 0, '空会话应该没有消息');
  });

  await asyncTest('大量任务的总结（测试截断）', async () => {
    const session = sessionManager.getCurrentSession();
    // 添加超过限制的任务
    for (let i = 0; i < 15; i++) {
      sessionManager.addTask(session.id, {
        id: `task-bulk-${i}`,
        sessionId: session.id,
        prompt: `测试任务 ${i}`,
        status: 'completed',
        priority: 1,
        subTasks: [],
        createdAt: Date.now(),
        retryCount: 0,
        maxRetries: 3
      });
    }

    const largeSummary = sessionManager.getSessionSummary();
    assert(largeSummary.completedTasks.length <= 10, '已完成任务应该被限制在10个以内');
  });

  await asyncTest('大量代码变更的总结（测试截断）', async () => {
    const session = sessionManager.getCurrentSession();
    // 添加超过限制的快照
    for (let i = 0; i < 25; i++) {
      sessionManager.addSnapshot(session.id, {
        id: `snapshot-bulk-${i}`,
        filePath: `src/test/file-${i}.ts`,
        lastModifiedBy: 'claude',
        lastModifiedAt: Date.now(),
        subTaskId: 'task-bulk-0',
        priority: 1
      });
    }

    const largeSummary = sessionManager.getSessionSummary();
    assert(largeSummary.codeChanges.length <= 20, '代码变更应该被限制在20个以内');
  });

  test('没有 SessionManager 时的上下文切片', () => {
    const newContextManager = new ContextManager(
      'test-session',
      'Test',
      TEST_WORKSPACE,
      new TruncationUtils()
    );

    // 不设置 SessionManager
    const contextSlice = newContextManager.getContextSlice({
      maxTokens: 4000,
      memoryRatio: 0.3
    });

    assert(typeof contextSlice === 'string', '应该返回字符串');
    // 不应该包含会话总结
    assert(!contextSlice.includes('会话总结'), '不应该包含会话总结');
  });

  // ========================================
  // 6. 测试会话删除
  // ========================================
  console.log('\n🗑️  6. 会话删除测试\n');

  await asyncTest('删除会话', async () => {
    const metas = sessionManager.getSessionMetas();
    const sessionToDelete = metas[metas.length - 1].id;

    await sessionManager.deleteSession(sessionToDelete);

    const newMetas = sessionManager.getSessionMetas();
    assert(newMetas.length === metas.length - 1, '会话数量应该减少1');

    const stillExists = newMetas.some(m => m.id === sessionToDelete);
    assert(!stillExists, '被删除的会话不应该存在');
  });

  // ========================================
  // 测试总结
  // ========================================
  console.log('\n' + '='.repeat(80));
  console.log('\n📊 测试结果统计\n');
  console.log(`✅ 通过: ${passed}`);
  console.log(`❌ 失败: ${failed}`);
  console.log(`📈 通过率: ${((passed / (passed + failed)) * 100).toFixed(1)}%`);

  if (failed === 0) {
    console.log('\n🎉 所有测试通过！Phase 1 会话管理功能验证成功！\n');
  } else {
    console.log('\n⚠️  部分测试失败，请检查上述错误信息\n');
  }

  // 清理测试数据
  cleanupTestDir();
}

// 运行测试
runTests().catch(error => {
  console.error('测试执行失败:', error);
  process.exit(1);
});
