/**
 * WebSocket 系统核心功能测试脚本
 * 测试：服务端启动、客户端连接、消息收发、广播、心跳、重连
 * 
 * {{ AURA: Add - 创建集成测试验证所有核心功能 }}
 */

import WebSocketServerCore from '../websocket-system/server/websocket-server.js';
import WebSocketClientNode from '../websocket-system/client/websocket-client-node.js';

console.log('═'.repeat(70));
console.log('🧪 WebSocket 系统核心功能测试');
console.log('═'.repeat(70));

// 测试配置
const TEST_PORT = 8888;
const SERVER_URL = `ws://localhost:${TEST_PORT}`;

// 测试状态
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
      return false;
    }
  };
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message || '断言失败');
  }
}

function wait(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}

/**
 * 主测试流程
 */
async function runTests() {
  let server = null;
  const clients = [];

  try {
    // ==================== 服务端测试 ====================
    console.log('\n' + '─'.repeat(70));
    console.log('📡 第一部分：服务端功能测试');
    console.log('─'.repeat(70));

    // 测试 1: 服务端启动
    await test('服务端启动', async () => {
      server = new WebSocketServerCore({
        port: TEST_PORT,
        heartbeatInterval: 5000,
        heartbeatTimeout: 8000
      });

      await server.start();
      assert(server.wss, '服务器实例应该存在');
      console.log(`   └─ 服务器已启动，端口: ${TEST_PORT}`);
    })();

    // 测试 2: 事件注册
    await test('事件处理器注册', async () => {
      let connectionCalled = false;
      let messageCalled = false;

      server.onConnection(() => { connectionCalled = true; });
      server.onMessage(() => { messageCalled = true; });

      // 创建临时客户端触发事件
      const tempClient = new WebSocketClientNode(SERVER_URL);
      await tempClient.connect();
      await tempClient.send({ type: 'test', data: 'hello' });
      await wait(500);
      tempClient.close();

      assert(connectionCalled, '连接回调应该被调用');
      assert(messageCalled, '消息回调应该被调用');
      console.log('   └─ 事件处理器正常工作');
    })();

    // ==================== 客户端测试 ====================
    console.log('\n' + '─'.repeat(70));
    console.log('🔌 第二部分：客户端功能测试');
    console.log('─'.repeat(70));

    // 测试 3: 客户端连接
    await test('客户端连接', async () => {
      const client = new WebSocketClientNode(SERVER_URL, {
        reconnectInterval: 2000,
        maxReconnectAttempts: 3
      });

      await client.connect();
      assert(client.isConnected(), '客户端应该已连接');
      assert(client.connectionId, '应该获得连接ID');
      
      clients.push(client);
      console.log(`   └─ 客户端已连接，ID: ${client.connectionId}`);
    })();

    // 测试 4: 消息发送和接收
    await test('消息发送和接收', async () => {
      const client = clients[0];
      let receivedMessage = null;

      client.onMessage(msg => {
        if (msg.type === 'echo') {
          receivedMessage = msg;
        }
      });

      // 发送消息
      const sent = await client.send({
        type: 'echo',
        data: { test: '测试数据' }
      });

      assert(sent, '消息应该发送成功');
      
      // 等待响应
      await wait(1000);
      assert(receivedMessage, '应该收到回显消息');
      console.log('   └─ 消息收发正常');
    })();

    // 测试 5: 多客户端连接
    await test('多客户端连接', async () => {
      for (let i = 2; i <= 3; i++) {
        const client = new WebSocketClientNode(SERVER_URL);
        await client.connect();
        clients.push(client);
        console.log(`   └─ 客户端 ${i} 已连接`);
      }

      const stats = server.getStats();
      assert(stats.total >= 3, `应该有至少 3 个连接，实际: ${stats.total}`);
    })();

    // ==================== 消息路由测试 ====================
    console.log('\n' + '─'.repeat(70));
    console.log('📨 第三部分：消息路由测试');
    console.log('─'.repeat(70));

    // 测试 6: 单播消息
    await test('单播消息 (sendToClient)', async () => {
      const client = clients[0];
      let receivedNotification = false;

      client.onMessage(msg => {
        if (msg.type === 'notification') {
          receivedNotification = true;
        }
      });

      const sent = server.sendToClient(client.connectionId, {
        type: 'notification',
        content: '这是单播消息'
      });

      assert(sent, '单播应该发送成功');
      await wait(500);
      assert(receivedNotification, '客户端应该收到通知');
      console.log('   └─ 单播消息正常');
    })();

    // 测试 7: 广播消息
    await test('广播消息 (broadcast)', async () => {
      const receivedClients = new Set();

      clients.forEach((client, index) => {
        client.onMessage(msg => {
          if (msg.type === 'announcement') {
            receivedClients.add(index);
          }
        });
      });

      const stats = server.broadcast({
        type: 'announcement',
        content: '系统公告'
      });

      assert(stats.success >= 3, `应该成功发送给至少 3 个客户端，实际: ${stats.success}`);
      
      await wait(500);
      assert(receivedClients.size >= 3, `至少 3 个客户端应该收到广播，实际: ${receivedClients.size}`);
      console.log(`   └─ 广播成功发送给 ${stats.success} 个客户端`);
    })();

    // 测试 8: 广播排除功能
    await test('广播排除特定客户端', async () => {
      const excludeClient = clients[0];
      let excludedReceived = false;
      let othersReceived = 0;

      excludeClient.onMessage(msg => {
        if (msg.type === 'limited_broadcast') {
          excludedReceived = true;
        }
      });

      clients.slice(1).forEach(client => {
        client.onMessage(msg => {
          if (msg.type === 'limited_broadcast') {
            othersReceived++;
          }
        });
      });

      const stats = server.broadcast(
        { type: 'limited_broadcast', content: '限定广播' },
        [excludeClient.connectionId]
      );

      await wait(500);
      assert(!excludedReceived, '被排除的客户端不应该收到消息');
      assert(othersReceived >= 2, '其他客户端应该收到消息');
      console.log('   └─ 排除功能正常工作');
    })();

    // ==================== 心跳和连接管理测试 ====================
    console.log('\n' + '─'.repeat(70));
    console.log('💓 第四部分：心跳和连接管理测试');
    console.log('─'.repeat(70));

    // 测试 9: 心跳机制
    await test('心跳机制', async () => {
      const client = clients[0];
      const initialHeartbeat = server.connectionManager.getConnection(client.connectionId).lastHeartbeat;
      
      console.log('   └─ 等待心跳更新...');
      await wait(6000); // 等待心跳间隔
      
      const updatedHeartbeat = server.connectionManager.getConnection(client.connectionId).lastHeartbeat;
      assert(updatedHeartbeat > initialHeartbeat, '心跳时间应该更新');
      console.log('   └─ 心跳正常更新');
    })();

    // 测试 10: 连接断开和清理
    await test('连接断开和清理', async () => {
      const client = clients[0];
      const connId = client.connectionId;
      
      client.close();
      await wait(500);

      const conn = server.connectionManager.getConnection(connId);
      assert(!conn, '连接应该被清理');
      console.log('   └─ 连接断开后正确清理');
    })();

    // ==================== 重连测试 ====================
    console.log('\n' + '─'.repeat(70));
    console.log('🔄 第五部分：自动重连测试');
    console.log('─'.repeat(70));

    // 测试 11: 自动重连
    await test('自动重连机制', async () => {
      const client = new WebSocketClientNode(SERVER_URL, {
        reconnectInterval: 1000,
        maxReconnectAttempts: 3
      });

      let reconnectingCalled = false;
      client.onReconnecting(() => {
        reconnectingCalled = true;
      });

      await client.connect();
      const firstId = client.connectionId;
      
      // 模拟服务器断开
      const conn = server.connectionManager.getConnection(firstId);
      if (conn) {
        conn.ws.terminate();
      }
      
      console.log('   └─ 模拟连接断开，等待重连...');
      await wait(3000);

      assert(reconnectingCalled, '应该触发重连回调');
      assert(client.isConnected(), '应该重连成功');
      assert(client.connectionId !== firstId, '应该获得新的连接ID');
      console.log(`   └─ 重连成功，新ID: ${client.connectionId}`);
      
      client.close();
    })();

    // ==================== 错误处理测试 ====================
    console.log('\n' + '─'.repeat(70));
    console.log('⚠️  第六部分：错误处理测试');
    console.log('─'.repeat(70));

    // 测试 12: 无效消息处理
    await test('无效消息格式处理', async () => {
      const client = new WebSocketClientNode(SERVER_URL);
      await client.connect();

      let errorHandled = false;
      client.onMessage(msg => {
        if (msg.type === 'error') {
          errorHandled = true;
        }
      });

      // 发送无效 JSON (服务器会返回错误)
      if (client.ws) {
        client.ws.send('invalid json {');
      }
      
      await wait(500);
      assert(errorHandled, '应该收到错误响应');
      console.log('   └─ 错误处理正常');
      
      client.close();
    })();

    // 测试 13: 向不存在的连接发送消息
    await test('向不存在的连接发送消息', async () => {
      const result = server.sendToClient('non-existent-id', { test: 'data' });
      assert(result === false, '向不存在的连接发送应该返回 false');
      console.log('   └─ 正确处理无效连接ID');
    })();

    // 测试 14: 连接超时处理
    await test('连接超时处理', async () => {
      const client = new WebSocketClientNode('ws://localhost:9999', {
        connectionTimeout: 2000,
        maxReconnectAttempts: 1
      });

      let connectFailed = false;
      try {
        await client.connect();
      } catch (error) {
        connectFailed = true;
      }

      assert(connectFailed, '连接到无效服务器应该失败');
      console.log('   └─ 连接超时处理正常');
    })();

    // ==================== 性能测试 ====================
    console.log('\n' + '─'.repeat(70));
    console.log('⚡ 第七部分：性能测试');
    console.log('─'.repeat(70));

    // 测试 15: 批量消息发送
    await test('批量消息发送性能', async () => {
      const client = new WebSocketClientNode(SERVER_URL);
      await client.connect();

      const messageCount = 100;
      const startTime = Date.now();

      for (let i = 0; i < messageCount; i++) {
        await client.send({
          type: 'test',
          index: i,
          data: `Message ${i}`
        });
      }

      const duration = Date.now() - startTime;
      const throughput = (messageCount / duration * 1000).toFixed(2);
      
      console.log(`   └─ 发送 ${messageCount} 条消息耗时: ${duration}ms`);
      console.log(`   └─ 吞吐量: ${throughput} 条/秒`);
      
      client.close();
      assert(duration < 5000, `批量发送不应超过 5 秒，实际: ${duration}ms`);
    })();

  } finally {
    // ==================== 清理资源 ====================
    console.log('\n' + '─'.repeat(70));
    console.log('🧹 清理测试资源...');
    console.log('─'.repeat(70));

    // 关闭所有客户端
    for (const client of clients) {
      try {
        if (client.isConnected()) {
          client.close();
        }
      } catch (error) {
        console.error('   ⚠️  关闭客户端失败:', error.message);
      }
    }

    // 关闭服务器
    if (server) {
      await server.close();
      console.log('   ✅ 服务器已关闭');
    }

    console.log('   ✅ 所有资源已清理\n');
  }

  // ==================== 测试结果汇总 ====================
  console.log('═'.repeat(70));
  console.log('📊 测试结果汇总');
  console.log('═'.repeat(70));
  console.log(`总计: ${testResults.total} 个测试`);
  console.log(`✅ 通过: ${testResults.passed} 个`);
  console.log(`❌ 失败: ${testResults.failed} 个`);
  console.log(`成功率: ${((testResults.passed / testResults.total) * 100).toFixed(2)}%`);

  if (testResults.errors.length > 0) {
    console.log('\n❌ 失败的测试:');
    testResults.errors.forEach((err, index) => {
      console.log(`   ${index + 1}. ${err.name}`);
      console.log(`      错误: ${err.error}`);
    });
  }

  console.log('═'.repeat(70));
  
  // 退出码
  process.exit(testResults.failed > 0 ? 1 : 0);
}

// 错误捕获
process.on('unhandledRejection', (error) => {
  console.error('\n❌ 未捕获的 Promise 错误:', error);
  process.exit(1);
});

process.on('uncaughtException', (error) => {
  console.error('\n❌ 未捕获的异常:', error);
  process.exit(1);
});

// 运行测试
runTests().catch(error => {
  console.error('\n❌ 测试运行失败:', error);
  process.exit(1);
});
