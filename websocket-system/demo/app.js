/**
 * WebSocket 消息推送系统完整演示
 * 展示服务端启动、客户端连接、消息推送全流程
 */

import WebSocketServerCore from '../server/websocket-server.js';
import WebSocket from 'ws';

// 全局 WebSocket 对象（用于 Node.js 环境）
global.WebSocket = WebSocket;

console.log('='.repeat(60));
console.log('🚀 WebSocket 消息推送系统演示');
console.log('='.repeat(60));

// 创建服务器
const server = new WebSocketServerCore({
  port: 8080,
  heartbeatInterval: 30000,
  heartbeatTimeout: 35000
});

// 模拟客户端管理
const mockClients = new Map();

// 启动服务器
async function startDemo() {
  try {
    // 1. 启动服务器
    console.log('\n📡 步骤 1: 启动 WebSocket 服务器...');
    await server.start();
    console.log('✅ 服务器启动成功\n');

    // 2. 注册服务器事件
    console.log('📋 步骤 2: 注册事件处理器...');
    
    server.onConnection((connectionId, req) => {
      console.log(`\n👤 新客户端连接: ${connectionId}`);
      console.log(`   └─ IP: ${req.socket.remoteAddress}`);
    });

    server.onMessage((connectionId, message) => {
      console.log(`\n📨 收到消息 [${connectionId}]:`, message);
      
      // 处理不同类型的消息
      switch (message.type) {
        case 'chat':
          console.log('   └─ 类型: 聊天消息，准备广播...');
          server.broadcast({
            type: 'chat',
            from: connectionId,
            content: message.content,
            timestamp: Date.now()
          }, [connectionId]);
          break;
          
        case 'echo':
          console.log('   └─ 类型: 回显请求');
          server.sendToClient(connectionId, {
            type: 'echo',
            original: message,
            timestamp: Date.now()
          });
          break;
      }
    });

    server.onClose((connectionId, code, reason) => {
      console.log(`\n❌ 客户端断开: ${connectionId}`);
      console.log(`   └─ 代码: ${code}, 原因: ${reason}`);
    });

    console.log('✅ 事件处理器注册完成\n');

    // 3. 模拟多个客户端连接
    console.log('🔗 步骤 3: 模拟客户端连接...');
    await simulateClients();

    // 4. 演示消息推送
    console.log('\n📤 步骤 4: 演示消息推送...');
    await demonstrateMessaging();

    // 5. 演示心跳和断线处理
    console.log('\n💓 步骤 5: 心跳检测运行中...');
    console.log('   (服务器会自动处理心跳和超时连接)\n');

    // 6. 定期显示统计信息
    setInterval(() => {
      const stats = server.getStats();
      console.log(`\n📊 连接统计: 总数=${stats.total}, 活跃=${stats.alive}, 非活跃=${stats.inactive}`);
    }, 15000);

    // 7. 等待一段时间后演示断开
    setTimeout(async () => {
      console.log('\n🔌 步骤 6: 演示客户端断开...');
      await demonstrateDisconnection();
    }, 10000);

    // 8. 保持运行
    console.log('💡 提示: 按 Ctrl+C 停止演示\n');

  } catch (error) {
    console.error('❌ 演示失败:', error);
    process.exit(1);
  }
}

/**
 * 模拟多个客户端连接
 */
async function simulateClients() {
  const clientConfigs = [
    { id: 'client-1', name: '客户端A' },
    { id: 'client-2', name: '客户端B' },
    { id: 'client-3', name: '客户端C' }
  ];

  for (const config of clientConfigs) {
    await new Promise(resolve => setTimeout(resolve, 500)); // 延迟连接
    
    console.log(`   连接 ${config.name}...`);
    const ws = new WebSocket('ws://localhost:8080');
    
    ws.on('open', () => {
      console.log(`   ✅ ${config.name} 已连接`);
    });

    ws.on('message', (data) => {
      try {
        const message = JSON.parse(data.toString());
        
        // 保存连接ID
        if (message.type === 'connected') {
          mockClients.set(config.id, {
            ws,
            connectionId: message.connectionId,
            name: config.name
          });
        }
        
        // 显示收到的消息（排除连接消息和pong）
        if (message.type !== 'connected' && message.type !== 'pong') {
          console.log(`   📥 ${config.name} 收到:`, message.type);
        }
      } catch (error) {
        console.error(`   ⚠️  ${config.name} 消息解析错误:`, error);
      }
    });

    ws.on('close', () => {
      console.log(`   ❌ ${config.name} 已断开`);
    });

    ws.on('error', (error) => {
      console.error(`   ⚠️  ${config.name} 错误:`, error.message);
    });
  }

  // 等待所有连接建立
  await new Promise(resolve => setTimeout(resolve, 1000));
  console.log(`✅ 模拟了 ${mockClients.size} 个客户端连接\n`);
}

/**
 * 演示消息推送
 */
async function demonstrateMessaging() {
  // 1. 单播消息
  console.log('   1️⃣  测试单播消息:');
  const client1 = mockClients.get('client-1');
  if (client1) {
    const success = server.sendToClient(client1.connectionId, {
      type: 'notification',
      title: '系统通知',
      content: '这是一条单播消息，只有你能看到',
      timestamp: Date.now()
    });
    console.log(`      └─ 发送到 ${client1.name}: ${success ? '成功' : '失败'}`);
  }

  await new Promise(resolve => setTimeout(resolve, 1000));

  // 2. 广播消息
  console.log('\n   2️⃣  测试广播消息:');
  const stats = server.broadcast({
    type: 'announcement',
    title: '系统公告',
    content: '这是一条广播消息，所有人都能收到',
    timestamp: Date.now()
  });
  console.log(`      └─ 广播结果: 成功 ${stats.success} 个, 失败 ${stats.failed} 个`);

  await new Promise(resolve => setTimeout(resolve, 1000));

  // 3. 客户端发送消息（模拟聊天）
  console.log('\n   3️⃣  测试客户端发送消息:');
  const client2 = mockClients.get('client-2');
  if (client2) {
    client2.ws.send(JSON.stringify({
      type: 'chat',
      content: '大家好，这是来自客户端B的消息！'
    }));
    console.log(`      └─ ${client2.name} 发送聊天消息`);
  }

  await new Promise(resolve => setTimeout(resolve, 1000));

  // 4. 回显测试
  console.log('\n   4️⃣  测试回显功能:');
  const client3 = mockClients.get('client-3');
  if (client3) {
    client3.ws.send(JSON.stringify({
      type: 'echo',
      data: { test: '回显测试数据' }
    }));
    console.log(`      └─ ${client3.name} 发送回显请求`);
  }

  console.log('\n✅ 消息推送演示完成');
}

/**
 * 演示断开连接
 */
async function demonstrateDisconnection() {
  const client1 = mockClients.get('client-1');
  if (client1) {
    console.log(`   断开 ${client1.name}...`);
    client1.ws.close(1000, '演示断开');
    mockClients.delete('client-1');
  }

  await new Promise(resolve => setTimeout(resolve, 1000));

  // 显示剩余连接
  const stats = server.getStats();
  console.log(`✅ 断开演示完成，剩余连接: ${stats.total} 个\n`);
}

// 优雅关闭
async function gracefulShutdown() {
  console.log('\n\n📴 开始优雅关闭...');
  
  // 关闭所有模拟客户端
  console.log('   关闭模拟客户端...');
  for (const [id, client] of mockClients) {
    try {
      client.ws.close();
    } catch (error) {
      console.error(`   ⚠️  关闭 ${client.name} 失败:`, error.message);
    }
  }
  
  // 关闭服务器
  console.log('   关闭服务器...');
  await server.close();
  
  console.log('✅ 所有资源已清理');
  console.log('👋 演示结束，再见！\n');
  process.exit(0);
}

process.on('SIGINT', gracefulShutdown);
process.on('SIGTERM', gracefulShutdown);

// 启动演示
startDemo().catch(error => {
  console.error('❌ 演示启动失败:', error);
  process.exit(1);
});
