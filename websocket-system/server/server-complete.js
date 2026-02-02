/**
 * WebSocket 服务端完整启动入口
 * 演示所有功能：连接管理、心跳检测、消息广播、私聊、房间订阅
 */

import WebSocketServerComplete from './websocket-server-complete.js';

// 创建服务器实例
const server = new WebSocketServerComplete({
  port: process.env.WS_PORT || 8080,
  host: process.env.WS_HOST || '0.0.0.0',
  heartbeatInterval: 30000,  // 30秒心跳间隔
  heartbeatTimeout: 35000,   // 35秒心跳超时
  logLevel: process.env.LOG_LEVEL || 'info'
});

// ==================== 事件监听 ====================

// 服务器启动事件
server.on('onServerStart', ({ port, host }) => {
  console.log('');
  console.log('╔═══════════════════════════════════════════════════╗');
  console.log('║     WebSocket 实时消息推送系统已启动              ║');
  console.log('╠═══════════════════════════════════════════════════╣');
  console.log(`║  🌐 监听地址: ws://${host}:${port}                `);
  console.log('║  💡 测试连接: 打开 client/index.html             ║');
  console.log('║  📊 服务状态: http://localhost:' + port + '         ║');
  console.log('║  🛑 停止服务: Ctrl+C                              ║');
  console.log('╚═══════════════════════════════════════════════════╝');
  console.log('');
});

// 新连接事件
server.on('onConnection', ({ connectionId, userId }) => {
  console.log(`\n🎯 [连接] 用户 ${userId} 已连接 (${connectionId})`);
});

// 断开连接事件
server.on('onDisconnect', ({ connectionId, userId, code }) => {
  console.log(`\n👋 [断开] 用户 ${userId} 已断开 (${connectionId}), 代码: ${code}`);
});

// 消息事件
server.on('onMessage', ({ userId, message }) => {
  const typeEmoji = {
    'ping': '💓',
    'join_room': '🚪',
    'leave_room': '🚶',
    'create_room': '🏠',
    'send_message': '💬',
    'broadcast': '📢',
    'get_room_members': '👥',
    'get_user_rooms': '📋'
  };
  
  const emoji = typeEmoji[message.type] || '📨';
  console.log(`${emoji} [${message.type}] 来自用户 ${userId}`);
});

// 错误事件
server.on('onError', ({ type, error }) => {
  console.error(`\n⚠️  [错误] ${type}:`, error.message);
});

// 服务器停止事件
server.on('onServerStop', () => {
  console.log('\n✅ 服务器已完全关闭');
});

// ==================== 扩展功能演示 ====================

// 自定义消息处理器示例：欢迎消息
server.getMessageRouter().registerHandler('get_welcome', (message, connectionId, userId) => {
  const router = server.getMessageRouter();
  router.sendToConnection(connectionId, {
    type: 'welcome_message',
    payload: {
      message: `欢迎 ${userId}！`,
      tips: [
        '使用 join_room 加入房间',
        '使用 send_message 发送消息',
        '使用 broadcast 广播消息',
        '使用 get_room_members 查看房间成员'
      ],
      serverTime: Date.now()
    },
    replyTo: message.id,
    timestamp: Date.now()
  });
});

// ==================== 定时任务 ====================

// 每分钟输出统计信息
setInterval(() => {
  const stats = server.getStats();
  console.log('\n' + '='.repeat(60));
  console.log('📊 服务器统计信息');
  console.log('='.repeat(60));
  console.log(`⏰ 运行时长: ${Math.floor(stats.server.uptime / 60)} 分钟`);
  console.log(`👥 总连接数: ${stats.connections.total}`);
  console.log(`✅ 活跃连接: ${stats.connections.alive}`);
  console.log(`❌ 非活跃连接: ${stats.connections.inactive}`);
  console.log(`🏠 房间总数: ${stats.rooms.totalRooms}`);
  console.log(`👤 房间用户数: ${stats.rooms.totalUsersInRooms}`);
  
  if (stats.rooms.rooms.length > 0) {
    console.log('\n房间列表:');
    stats.rooms.rooms.forEach(room => {
      console.log(`  - ${room.name || room.roomId} (${room.memberCount} 人)`);
    });
  }
  console.log('='.repeat(60) + '\n');
}, 60000);

// ==================== 启动服务器 ====================

async function startServer() {
  try {
    await server.start();
    
    // 可选：创建一些默认房间
    const roomManager = server.getRoomManager();
    roomManager.createRoom('lobby', {
      name: '大厅',
      description: '公共聊天大厅',
      maxMembers: 1000
    });
    roomManager.createRoom('tech', {
      name: '技术讨论',
      description: '技术交流房间',
      maxMembers: 100
    });
    roomManager.createRoom('game', {
      name: '游戏频道',
      description: '游戏玩家聊天',
      maxMembers: 50
    });
    
    console.log('📦 已创建默认房间: lobby, tech, game\n');
    
  } catch (error) {
    console.error('❌ 服务器启动失败:', error);
    process.exit(1);
  }
}

// ==================== 优雅退出 ====================

async function gracefulShutdown(signal) {
  console.log(`\n\n📴 收到 ${signal} 信号，正在优雅关闭...`);
  
  try {
    await server.stop();
    console.log('✅ 服务器已安全关闭');
    process.exit(0);
  } catch (error) {
    console.error('❌ 关闭过程中发生错误:', error);
    process.exit(1);
  }
}

process.on('SIGINT', () => gracefulShutdown('SIGINT'));
process.on('SIGTERM', () => gracefulShutdown('SIGTERM'));

// 未捕获异常处理
process.on('uncaughtException', (error) => {
  console.error('❌ 未捕获异常:', error);
  gracefulShutdown('UNCAUGHT_EXCEPTION');
});

process.on('unhandledRejection', (reason, promise) => {
  console.error('❌ 未处理的 Promise 拒绝:', reason);
  gracefulShutdown('UNHANDLED_REJECTION');
});

// ==================== 启动 ====================

startServer();

// 导出服务器实例（用于测试）
export default server;
