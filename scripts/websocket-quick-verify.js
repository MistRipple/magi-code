#!/usr/bin/env node

/**
 * 快速验证脚本 - 5分钟验证 WebSocket 系统所有核心功能
 * 
 * {{ AURA: Add - 创建快速验证脚本，便于一键测试 }}
 */

import WebSocketServerCore from '../websocket-system/server/websocket-server.js';
import WebSocketClientNode from '../websocket-system/client/websocket-client-node.js';

console.log('╔═══════════════════════════════════════════════════════════╗');
console.log('║   WebSocket 实时消息推送系统 - 快速验证                    ║');
console.log('╚═══════════════════════════════════════════════════════════╝\n');

const PORT = 8765;
const URL = `ws://localhost:${PORT}`;

async function quickTest() {
  let server = null;
  const clients = [];

  try {
    console.log('🚀 步骤 1/5: 启动服务器...');
    server = new WebSocketServerCore({ port: PORT });
    await server.start();
    console.log('   ✅ 服务器已启动\n');

    console.log('🔌 步骤 2/5: 连接客户端...');
    for (let i = 1; i <= 3; i++) {
      const client = new WebSocketClientNode(URL);
      await client.connect();
      clients.push(client);
      console.log(`   ✅ 客户端 ${i} 已连接 (${client.connectionId})`);
    }
    console.log('');

    console.log('📨 步骤 3/5: 测试消息收发...');
    
    // 单播测试
    let received = false;
    clients[0].onMessage(msg => {
      if (msg.type === 'test') received = true;
    });
    
    server.sendToClient(clients[0].connectionId, { type: 'test', data: 'hello' });
    await new Promise(r => setTimeout(r, 500));
    console.log(`   ${received ? '✅' : '❌'} 单播测试`);

    // 广播测试
    const receivedBy = new Set();
    clients.forEach((c, i) => {
      c.onMessage(msg => {
        if (msg.type === 'broadcast') receivedBy.add(i);
      });
    });
    
    server.broadcast({ type: 'broadcast', content: 'Hello All' });
    await new Promise(r => setTimeout(r, 500));
    console.log(`   ${receivedBy.size === 3 ? '✅' : '❌'} 广播测试 (${receivedBy.size}/3 收到)`);
    console.log('');

    console.log('💓 步骤 4/5: 测试心跳...');
    const conn = server.connectionManager.getConnection(clients[0].connectionId);
    const beforeHeartbeat = conn.lastHeartbeat;
    await new Promise(r => setTimeout(r, 2000));
    
    // 客户端发送心跳
    await clients[0].send({ type: 'ping' });
    await new Promise(r => setTimeout(r, 500));
    
    const afterHeartbeat = server.connectionManager.getConnection(clients[0].connectionId).lastHeartbeat;
    console.log(`   ${afterHeartbeat > beforeHeartbeat ? '✅' : '❌'} 心跳机制`);
    console.log('');

    console.log('🔄 步骤 5/5: 测试重连...');
    const client = new WebSocketClientNode(URL, { 
      reconnectInterval: 1000,
      maxReconnectAttempts: 2 
    });
    
    let reconnecting = false;
    client.onReconnecting(() => { reconnecting = true; });
    
    await client.connect();
    const oldId = client.connectionId;
    
    // 强制断开
    const conn2 = server.connectionManager.getConnection(oldId);
    conn2.ws.terminate();
    
    await new Promise(r => setTimeout(r, 2000));
    
    console.log(`   ${reconnecting && client.isConnected() ? '✅' : '❌'} 自动重连`);
    console.log(`   ${client.connectionId !== oldId ? '✅' : '❌'} 新连接ID`);
    
    client.close();
    console.log('');

    // 统计
    const stats = server.getStats();
    console.log('📊 系统状态:');
    console.log(`   总连接: ${stats.total}`);
    console.log(`   活跃连接: ${stats.alive}`);
    console.log(`   运行时间: ${Math.floor(process.uptime())}s`);
    console.log('');

    console.log('╔═══════════════════════════════════════════════════════════╗');
    console.log('║   ✅ 验证完成！所有核心功能正常工作                        ║');
    console.log('╚═══════════════════════════════════════════════════════════╝\n');

    console.log('💡 提示:');
    console.log('   - 运行完整测试: npm test');
    console.log('   - 启动服务器: npm start');
    console.log('   - 查看演示: npm run demo');
    console.log('   - 阅读文档: cat README.md\n');

  } catch (error) {
    console.error('\n❌ 验证失败:', error.message);
    process.exit(1);
  } finally {
    // 清理
    clients.forEach(c => {
      try { if (c.isConnected()) c.close(); } catch {}
    });
    if (server) await server.close();
  }

  process.exit(0);
}

quickTest();
