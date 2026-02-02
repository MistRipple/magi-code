/**
 * WebSocket 服务端启动入口
 * 演示如何使用 WebSocketServerCore
 */

import WebSocketServerCore from './websocket-server.js';

// 创建服务器实例
const server = new WebSocketServerCore({
  port: 8080,
  heartbeatInterval: 30000,
  heartbeatTimeout: 35000
});

// 注册事件处理器
server.onConnection((connectionId, req) => {
  console.log(`✅ 新客户端连接: ${connectionId}`);
  console.log(`   IP: ${req.socket.remoteAddress}`);
  console.log(`   User-Agent: ${req.headers['user-agent']}`);
});

server.onMessage((connectionId, message) => {
  console.log(`📨 收到消息 from ${connectionId}:`, message);
  
  // 根据消息类型处理
  switch (message.type) {
    case 'chat':
      // 广播聊天消息给所有人（排除发送者）
      server.broadcast({
        type: 'chat',
        from: connectionId,
        content: message.content,
        timestamp: Date.now()
      }, [connectionId]);
      break;
      
    case 'private':
      // 私聊消息
      if (message.to) {
        server.sendToClient(message.to, {
          type: 'private',
          from: connectionId,
          content: message.content,
          timestamp: Date.now()
        });
      }
      break;
      
    default:
      // 回显消息
      server.sendToClient(connectionId, {
        type: 'echo',
        original: message,
        timestamp: Date.now()
      });
  }
});

server.onClose((connectionId, code, reason) => {
  console.log(`❌ 客户端断开: ${connectionId}, 代码: ${code}, 原因: ${reason}`);
});

server.onError((connectionId, error) => {
  console.error(`⚠️  连接错误 ${connectionId}:`, error.message);
});

// 启动服务器
async function startServer() {
  try {
    await server.start();
    console.log('🚀 WebSocket 服务器启动成功！');
    console.log(`📡 监听地址: ws://localhost:${server.port}`);
    console.log('💡 使用 Ctrl+C 停止服务器');
    
    // 定期打印统计信息
    setInterval(() => {
      const stats = server.getStats();
      console.log(`📊 连接统计: 总数=${stats.total}, 活跃=${stats.alive}, 非活跃=${stats.inactive}`);
    }, 60000); // 每分钟打印一次
    
  } catch (error) {
    console.error('❌ 服务器启动失败:', error);
    process.exit(1);
  }
}

// 优雅关闭
process.on('SIGINT', async () => {
  console.log('\n📴 收到关闭信号，正在优雅关闭...');
  await server.close();
  process.exit(0);
});

process.on('SIGTERM', async () => {
  console.log('\n📴 收到终止信号，正在优雅关闭...');
  await server.close();
  process.exit(0);
});

// 启动
startServer();
