/**
 * LLM 压缩器集成测试
 * 验证 ContextCompressor 使用 LLM 进行智能压缩
 */

const path = require('path');
const fs = require('fs');

// 测试配置
const TEST_WORKSPACE = path.join(__dirname, '..', '.test-llm-compression');
const TEST_SESSION_ID = 'llm-compression-test';

// 清理测试目录
function cleanupTestDir() {
  if (fs.existsSync(TEST_WORKSPACE)) {
    fs.rmSync(TEST_WORKSPACE, { recursive: true });
  }
}

// 测试结果统计
let passed = 0;
let failed = 0;

async function asyncTest(name, fn) {
  try {
    await fn();
    console.log(`✅ ${name}`);
    passed++;
  } catch (error) {
    console.log(`❌ ${name}`);
    console.log(`   错误: ${error.message}`);
    if (error.stack) {
      console.log(`   堆栈: ${error.stack.split('\n').slice(0, 3).join('\n')}`);
    }
    failed++;
  }
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message || 'Assertion failed');
  }
}

async function runTests() {
  console.log('\n🧪 LLM 压缩器集成测试\n');
  console.log('='.repeat(60));

  // 清理之前的测试数据
  cleanupTestDir();

  // 动态导入模块
  const { MemoryDocument } = require('../out/context/memory-document.js');
  const { ContextCompressor } = require('../out/context/context-compressor.js');

  // ========================================
  // 1. 测试 LLM 适配器注入
  // ========================================
  console.log('\n📝 1. LLM 适配器注入测试\n');

  let compressor;
  let mockCallCount = 0;

  await asyncTest('创建带 Mock LLM 适配器的 ContextCompressor', async () => {
    const mockAdapter = {
      sendMessage: async (message) => {
        mockCallCount++;
        console.log(`   [Mock LLM] 收到压缩请求 (调用 #${mockCallCount})`);

        // 模拟 LLM 响应：返回压缩后的 JSON
        return `\`\`\`json
{
  "currentTasks": [],
  "completedTasks": [
    {
      "id": "task-summary",
      "description": "已完成10个任务（任务0-9）",
      "status": "completed",
      "result": "所有任务成功完成"
    }
  ],
  "keyDecisions": [],
  "codeChanges": [
    {
      "file": "src/file0.ts",
      "action": "modify",
      "summary": "合并5次修改"
    }
  ],
  "importantContext": [],
  "pendingIssues": []
}
\`\`\``;
      }
    };

    // 使用低阈值配置以触发 LLM 压缩
    compressor = new ContextCompressor(mockAdapter, {
      tokenLimit: 500,  // 低阈值确保触发压缩
      lineLimit: 50,
      compressionRatio: 0.5,
      retentionPriority: ['currentTasks', 'keyDecisions', 'importantContext', 'codeChanges', 'completedTasks', 'pendingIssues']
    });
    assert(compressor !== null, 'ContextCompressor 应该被创建');
  });

  // ========================================
  // 2. 测试 LLM 智能压缩
  // ========================================
  console.log('\n📦 2. LLM 智能压缩测试\n');

  await asyncTest('LLM 压缩大量 Memory 数据', async () => {
    const storagePath = path.join(TEST_WORKSPACE, '.multicli/sessions');
    const testMemory = new MemoryDocument(TEST_SESSION_ID, 'LLM Compression Test', storagePath);
    await testMemory.load();

    // 添加大量数据以触发压缩
    for (let i = 0; i < 10; i++) {
      testMemory.addCurrentTask({
        id: `task-${i}`,
        description: `测试任务 ${i} - 这是一个包含详细信息的长描述`,
        status: 'completed',
        result: `任务 ${i} 成功完成，产出了大量结果数据和详细报告`
      });
      testMemory.updateTaskStatus(`task-${i}`, 'completed', `详细结果 ${i}`);
    }

    for (let i = 0; i < 15; i++) {
      testMemory.addCodeChange({
        file: `src/file${i % 5}.ts`,
        action: 'modify',
        summary: `第 ${i} 次修改：添加了新功能，重构了部分代码，优化了性能`
      });
    }

    const beforeTokens = testMemory.estimateTokens();
    const beforeTasks = testMemory.getContent().completedTasks.length;
    const beforeChanges = testMemory.getContent().codeChanges.length;

    console.log(`   压缩前: ${beforeTokens} tokens, ${beforeTasks} 任务, ${beforeChanges} 代码变更`);

    // 执行压缩
    mockCallCount = 0;
    const success = await compressor.compress(testMemory);

    assert(success, '压缩应该成功');
    assert(mockCallCount > 0, 'LLM 适配器应该被调用');

    const afterTokens = testMemory.estimateTokens();
    const afterTasks = testMemory.getContent().completedTasks.length;
    const afterChanges = testMemory.getContent().codeChanges.length;

    console.log(`   压缩后: ${afterTokens} tokens, ${afterTasks} 任务, ${afterChanges} 代码变更`);
    console.log(`   LLM 调用次数: ${mockCallCount}`);

    assert(afterTokens < beforeTokens, '压缩后 Token 应该减少');

    // 获取压缩统计
    const stats = compressor.getLastStats();
    assert(stats !== null, '应该有压缩统计信息');
    assert(stats.method === 'llm', '应该使用 LLM 压缩方法');
    console.log(`   压缩方法: ${stats.method}`);
    console.log(`   压缩比: ${(stats.compressionRatio * 100).toFixed(1)}%`);
  });

  // ========================================
  // 3. 测试降级到简单压缩
  // ========================================
  console.log('\n⚠️  3. LLM 失败降级测试\n');

  await asyncTest('LLM 失败时降级到简单压缩', async () => {
    // 创建一个会失败的 LLM 适配器
    const failingAdapter = {
      sendMessage: async (message) => {
        console.log('   [Mock LLM] 模拟 LLM 失败');
        throw new Error('LLM 服务不可用');
      }
    };

    // 使用低阈值配置以触发压缩
    const failoverCompressor = new ContextCompressor(failingAdapter, {
      tokenLimit: 200,  // 低阈值确保触发压缩
      lineLimit: 30,
      compressionRatio: 0.5,
      retentionPriority: ['currentTasks', 'keyDecisions', 'importantContext', 'codeChanges', 'completedTasks', 'pendingIssues']
    });

    const storagePath = path.join(TEST_WORKSPACE, '.multicli/sessions');
    const testMemory = new MemoryDocument('failover-test', 'Failover Test', storagePath);
    await testMemory.load();

    // 添加数据
    for (let i = 0; i < 10; i++) {
      testMemory.addCurrentTask({
        id: `task-${i}`,
        description: `任务 ${i}`,
        status: 'completed'
      });
      testMemory.updateTaskStatus(`task-${i}`, 'completed');
    }

    const beforeTokens = testMemory.estimateTokens();
    console.log(`   压缩前: ${beforeTokens} tokens`);

    // LLM 失败，应该降级到简单压缩
    const success = await failoverCompressor.compress(testMemory);

    assert(success, '降级压缩应该成功');

    const afterTokens = testMemory.estimateTokens();
    console.log(`   压缩后: ${afterTokens} tokens`);

    const stats = failoverCompressor.getLastStats();
    assert(stats !== null, '应该有压缩统计信息');
    // LLM失败后会降级到 simple 或 aggressive
    assert(stats.method === 'simple' || stats.method === 'aggressive', '应该使用降级压缩方法');
    console.log(`   降级方法: ${stats.method}`);
  });

  // ========================================
  // 测试结果汇总
  // ========================================
  console.log('\n' + '='.repeat(60));
  console.log('📊 测试结果汇总');
  console.log('='.repeat(60));
  console.log(`通过: ${passed} 个`);
  console.log(`失败: ${failed} 个`);
  console.log(`成功率: ${(passed / (passed + failed) * 100).toFixed(1)}%`);
  console.log('='.repeat(60));

  // 清理测试目录
  cleanupTestDir();

  // 返回退出码
  if (failed > 0) {
    process.exit(1);
  }
}

// 运行测试
runTests().catch(error => {
  console.error('测试执行失败:', error);
  process.exit(1);
});
