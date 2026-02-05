/**
 * 契约集成验证测试
 * 验证 WebSocket 系统是否符合 API 接口契约和数据结构契约
 * 
 * {{ AURA: Add - 创建契约集成验证测试 }}
 * 
 * 契约依赖：
 * - contract_1770010232546_5vf7w5pz3 (API 接口契约)
 * - contract_1770010232546_dzyfec7i4 (数据结构契约)
 */

import WebSocketServiceAdapter from '../websocket-system/contracts/service-adapter.js';
import WebSocketClientNode from '../websocket-system/client/websocket-client-node.js';

console.log('╔═══════════════════════════════════════════════════════════╗');
console.log('║   WebSocket 系统契约集成验证测试                           ║');
console.log('╚═══════════════════════════════════════════════════════════╝\n');

const TEST_PORT = 9999;
const SERVER_URL = `ws://localhost:${TEST_PORT}`;

// 测试结果
const testResults = {
  total: 0,
  passed: 0,
  failed: 0,
  errors: []
};

/**
 * 测试工具函数
 */
function test(name, fn) {
  return async () => {
    testResults.total++;
    console.log(`\n🔬 测试 ${testResults.total}: ${name}`);
    try {
      await fn();
      testResults.passed++;
      console.log(`   ✅ 通过`);
      return true;
    } catch (error) {
      testResults.failed++;
      testResults.errors.push({ name, error: error.message });
      console.error(`   ❌ 失败: ${error.message}`);
      if (error.stack) {
        console.error(`   堆栈: ${error.stack.split('\n')[1]}`);
      }
      return false;
    }
  };
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message || '断言失败');
  }
}

function sleep(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

/**
 * 主测试函数
 */
async function runTests() {
  let adapter = null;
  const clients = [];

  try {
    console.log('\n📦 第一部分：API 接口契约验证\n');
    console.log('=' .repeat(60));

    // ========== 测试 1: 契约接口初始化 ==========
    await test('契约接口初始化', async () => {
      adapter = new WebSocketServiceAdapter();
      
      // 验证契约方法存在
      assert(typeof adapter.initialize === 'function', '缺少 initialize 方法');
      assert(typeof adapter.sendToClient === 'function', '缺少 sendToClient 方法');
      assert(typeof adapter.broadcast === 'function', '缺少 broadcast 方法');
      assert(typeof adapter.getStatus === 'function', '缺少 getStatus 方法');
      assert(typeof adapter.shutdown === 'function', '缺少 shutdown 方法');
      
      // 初始化服务
      await adapter.initialize({
        port: TEST_PORT,
        heartbeatInterval: 30000,
        heartbeatTimeout: 35000
      });
      
      assert(adapter.isInitialized === true, '服务初始化状态错误');
    })();

    // ========== 测试 2: 获取服务状态 (契约方法) ==========
    await test('getStatus() 契约方法', async () => {
      const status = await adapter.getStatus();
      
      // 验证契约返回格式
      assert(status.initialized === true, 'initialized 字段错误');
      assert(status.running === true, 'running 字段错误');
      assert(status.port === TEST_PORT, 'port 字段错误');
      assert(typeof status.connections === 'object', 'connections 应为对象');
      assert(typeof status.connections.total === 'number', 'connections.total 应为数字');
      assert(typeof status.uptime === 'number', 'uptime 应为数字');
      
      console.log(`   服务状态: 已初始化=${status.initialized}, 运行中=${status.running}`);
    })();

    // ========== 测试 3: 客户端连接 ==========
    await test('客户端连接', async () => {
      const client = new WebSocketClientNode(SERVER_URL);
      await client.connect();
      clients.push(client);
      
      await sleep(500); // 等待连接建立
      
      const status = await adapter.getStatus();
      assert(status.connections.total >= 1, '连接数应至少为 1');
      
      console.log(`   连接数: ${status.connections.total}`);
    })();

    // ========== 测试 4: sendToClient() 契约方法 ==========
    await test('sendToClient() 单播消息', async () => {
      const client = clients[0];
      
      let receivedMessage = null;
      client.onMessage(msg => {
        receivedMessage = msg;
      });
      
      // 契约方法：发送消息给指定客户端
      const success = await adapter.sendToClient(client.connectionId, {
        type: 'test',
        content: 'Hello from contract',
        data: { key: 'value' }
      });
      
      assert(success === true, 'sendToClient 应返回 true');
      
      await sleep(500);
      
      assert(receivedMessage !== null, '客户端应收到消息');
      assert(receivedMessage.type === 'test', '消息类型不匹配');
      assert(receivedMessage.content === 'Hello from contract', '消息内容不匹配');
      assert(typeof receivedMessage.timestamp === 'number', '消息应包含时间戳');
      
      console.log(`   收到消息:`, receivedMessage);
    })();

    // ========== 测试 5: broadcast() 契约方法 ==========
    await test('broadcast() 广播消息', async () => {
      // 连接第二个客户端
      const client2 = new WebSocketClientNode(SERVER_URL);
      await client2.connect();
      clients.push(client2);
      
      await sleep(500);
      
      const receivedMessages = [];
      clients.forEach((c, index) => {
        c.onMessage(msg => {
          if (msg.type === 'broadcast_test') {
            receivedMessages.push({ index, msg });
          }
        });
      });
      
      // 契约方法：广播消息
      const result = await adapter.broadcast({
        type: 'broadcast_test',
        announcement: '这是广播消息'
      });
      
      // 验证契约返回格式
      assert(result.success === true, 'broadcast 应返回 success=true');
      assert(typeof result.sentCount === 'number', 'sentCount 应为数字');
      assert(typeof result.failedCount === 'number', 'failedCount 应为数字');
      assert(typeof result.totalCount === 'number', 'totalCount 应为数字');
      
      await sleep(500);
      
      assert(receivedMessages.length === 2, '两个客户端都应收到广播');
      
      console.log(`   广播结果: 成功=${result.sentCount}, 失败=${result.failedCount}`);
    })();

    // ========== 测试 6: broadcast() 排除选项 ==========
    await test('broadcast() 排除特定客户端', async () => {
      const excludeId = clients[0].connectionId;
      
      const receivedBy = [];
      clients.forEach((c, index) => {
        c.onMessage(msg => {
          if (msg.type === 'exclude_test') {
            receivedBy.push(index);
          }
        });
      });
      
      // 契约方法：广播并排除
      const result = await adapter.broadcast({
        type: 'exclude_test',
        data: 'test'
      }, {
        exclude: [excludeId]
      });
      
      await sleep(500);
      
      assert(receivedBy.length === 1, '只有一个客户端应收到消息');
      assert(receivedBy[0] === 1, '应该是第二个客户端收到消息');
      
      console.log(`   排除客户端: ${excludeId.substring(0, 20)}...`);
    })();

    // ========== 测试 7: 事件监听器注册 ==========
    await test('事件监听器注册', async () => {
      let connectionEvent = false;
      let messageEvent = false;
      
      // 注册事件监听器
      adapter.onConnection((clientId, metadata) => {
        connectionEvent = true;
        console.log(`   [事件] 新连接: ${clientId.substring(0, 20)}...`);
      });
      
      adapter.onMessage((clientId, message) => {
        messageEvent = true;
        console.log(`   [事件] 收到消息: type=${message.type}`);
      });
      
      // 创建新连接触发事件
      const client3 = new WebSocketClientNode(SERVER_URL);
      await client3.connect();
      clients.push(client3);
      
      await sleep(500);
      
      // 发送消息触发事件
      await client3.send({ type: 'event_test', data: 'test' });
      
      await sleep(500);
      
      assert(connectionEvent === true, 'onConnection 事件应被触发');
      assert(messageEvent === true, 'onMessage 事件应被触发');
    })();

    // ========== 测试 8: getConnectedClients() ==========
    await test('获取连接客户端列表', async () => {
      const connectedClients = adapter.getConnectedClients();
      
      assert(Array.isArray(connectedClients), '应返回数组');
      assert(connectedClients.length >= 3, '至少应有 3 个连接');
      
      // 验证返回格式
      const firstClient = connectedClients[0];
      assert(typeof firstClient.id === 'string', 'id 应为字符串');
      assert(typeof firstClient.connectedAt === 'string', 'connectedAt 应为字符串');
      assert(firstClient.lastHeartbeat instanceof Date, 'lastHeartbeat 应为 Date');
      
      console.log(`   连接数: ${connectedClients.length}`);
    })();

    // ========== 测试 9: disconnectClient() ==========
    await test('断开指定客户端', async () => {
      const targetClient = clients[clients.length - 1];
      const targetId = targetClient.connectionId;
      
      let disconnected = false;
      targetClient.onClose(() => {
        disconnected = true;
      });
      
      // 断开客户端
      const success = adapter.disconnectClient(targetId, '测试断开');
      
      assert(success === true, 'disconnectClient 应返回 true');
      
      await sleep(500);
      
      assert(disconnected === true, '客户端应收到断开事件');
      
      // 移除已断开的客户端
      clients.pop();
      
      console.log(`   已断开: ${targetId.substring(0, 20)}...`);
    })();

    console.log('\n' + '='.repeat(60));
    console.log('\n📝 第二部分：数据结构契约验证\n');
    console.log('=' .repeat(60));

    // ========== 测试 10: WSMessage 消息格式契约 ==========
    await test('WSMessage 消息格式契约', async () => {
      const client = clients[0];
      
      let receivedMessage = null;
      client.onMessage(msg => {
        if (msg.type === 'ws_message_format_test') {
          receivedMessage = msg;
        }
      });
      
      // 发送符合契约的消息格式
      const contractMessage = {
        type: 'ws_message_format_test',
        header: {
          id: `msg_${Date.now()}`,
          type: 'MESSAGE',
          timestamp: Date.now(),
          version: '1.0.0',
          priority: 'normal'
        },
        payload: {
          data: {
            text: 'Contract format message'
          },
          metadata: {
            channel: 'test-channel'
          }
        }
      };
      
      await adapter.sendToClient(client.connectionId, contractMessage);
      
      await sleep(500);
      
      assert(receivedMessage !== null, '应收到消息');
      assert(receivedMessage.type === 'ws_message_format_test', '消息类型匹配');
      
      // 验证消息结构符合契约
      if (receivedMessage.header) {
        assert(typeof receivedMessage.header.id === 'string', 'header.id 应为字符串');
        assert(typeof receivedMessage.header.timestamp === 'number', 'header.timestamp 应为数字');
      }
      
      console.log(`   消息格式验证通过`);
    })();

    // ========== 测试 11: 消息时间戳自动添加 ==========
    await test('消息时间戳自动添加', async () => {
      const client = clients[0];
      
      let receivedMessage = null;
      client.onMessage(msg => {
        if (msg.type === 'timestamp_test') {
          receivedMessage = msg;
        }
      });
      
      // 发送不带时间戳的消息
      await adapter.sendToClient(client.connectionId, {
        type: 'timestamp_test',
        data: 'test'
      });
      
      await sleep(500);
      
      assert(receivedMessage !== null, '应收到消息');
      assert(typeof receivedMessage.timestamp === 'number', '应自动添加时间戳');
      assert(receivedMessage.timestamp > 0, '时间戳应大于 0');
      
      console.log(`   自动添加时间戳: ${receivedMessage.timestamp}`);
    })();

    // ========== 测试 12: 错误处理契约 ==========
    await test('错误处理契约', async () => {
      // 向不存在的连接发送消息
      const success = await adapter.sendToClient('non_existent_id', {
        type: 'test'
      });
      
      assert(success === false, '向不存在的连接发送应返回 false');
      
      // 断开不存在的连接
      const disconnectSuccess = adapter.disconnectClient('non_existent_id');
      
      assert(disconnectSuccess === false, '断开不存在的连接应返回 false');
      
      console.log(`   错误处理正确`);
    })();

    console.log('\n' + '='.repeat(60));
    console.log('\n🔧 第三部分：契约边界测试\n');
    console.log('=' .repeat(60));

    // ========== 测试 13: 重复初始化保护 ==========
    await test('重复初始化保护', async () => {
      let errorThrown = false;
      
      try {
        await adapter.initialize({ port: TEST_PORT });
      } catch (error) {
        errorThrown = true;
        assert(error.message.includes('已初始化'), '错误消息应包含"已初始化"');
      }
      
      assert(errorThrown === true, '重复初始化应抛出错误');
      
      console.log(`   重复初始化被正确阻止`);
    })();

    // ========== 测试 14: 未初始化状态检查 ==========
    await test('未初始化状态检查', async () => {
      const newAdapter = new WebSocketServiceAdapter();
      
      const status = await newAdapter.getStatus();
      
      assert(status.initialized === false, '未初始化时 initialized 应为 false');
      assert(status.running === false, '未初始化时 running 应为 false');
      assert(status.connections.total === 0, '未初始化时连接数应为 0');
      
      console.log(`   未初始化状态检查通过`);
    })();

    // ========== 测试 15: 广播过滤器功能 ==========
    await test('广播过滤器功能', async () => {
      const receivedBy = [];
      
      clients.forEach((c, index) => {
        c.onMessage(msg => {
          if (msg.type === 'filter_test') {
            receivedBy.push(index);
          }
        });
      });
      
      // 使用过滤器（只发送给第一个客户端）
      const result = await adapter.broadcast({
        type: 'filter_test',
        data: 'filtered'
      }, {
        filter: (conn) => conn.id === clients[0].connectionId
      });
      
      await sleep(500);
      
      assert(receivedBy.length === 1, '只有一个客户端应收到消息');
      assert(result.sentCount === 1, 'sentCount 应为 1');
      
      console.log(`   过滤器功能正常`);
    })();

    // ========== 测试 16: shutdown() 契约方法 ==========
    await test('shutdown() 关闭服务', async () => {
      // 关闭所有客户端
      for (const client of clients) {
        if (client.isConnected()) {
          client.close();
        }
      }
      
      await sleep(500);
      
      // 关闭服务
      await adapter.shutdown();
      
      assert(adapter.isInitialized === false, '关闭后 isInitialized 应为 false');
      
      const status = await adapter.getStatus();
      assert(status.initialized === false, '状态应显示未初始化');
      assert(status.running === false, '状态应显示未运行');
      
      console.log(`   服务已成功关闭`);
    })();

  } catch (error) {
    console.error('\n❌ 测试过程中发生未捕获错误:', error);
  } finally {
    // 清理资源
    for (const client of clients) {
      try {
        if (client.isConnected()) client.close();
      } catch {}
    }
    
    if (adapter && adapter.isInitialized) {
      try {
        await adapter.shutdown();
      } catch {}
    }
  }

  // 打印测试结果
  console.log('\n' + '═'.repeat(60));
  console.log('📊 测试结果汇总');
  console.log('═'.repeat(60));
  console.log(`总计: ${testResults.total} 个测试`);
  console.log(`✅ 通过: ${testResults.passed} 个`);
  console.log(`❌ 失败: ${testResults.failed} 个`);
  console.log(`成功率: ${((testResults.passed / testResults.total) * 100).toFixed(2)}%`);

  if (testResults.errors.length > 0) {
    console.log('\n失败的测试:');
    testResults.errors.forEach((err, i) => {
      console.log(`  ${i + 1}. ${err.name}: ${err.error}`);
    });
  }

  console.log('\n' + '═'.repeat(60));
  
  if (testResults.failed === 0) {
    console.log('🎉 所有契约验证测试通过！');
    console.log('✅ 系统完全符合 API 接口契约 (contract_1770010232546_5vf7w5pz3)');
    console.log('✅ 系统完全符合数据结构契约 (contract_1770010232546_dzyfec7i4)');
  } else {
    console.log('⚠️  部分测试失败，请检查契约实现');
  }
  
  console.log('═'.repeat(60) + '\n');

  process.exit(testResults.failed === 0 ? 0 : 1);
}

// 运行测试
runTests();
