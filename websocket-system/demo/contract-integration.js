/**
 * 契约集成演示
 * 展示如何使用契约适配器集成 WebSocket 服务
 */

import WebSocketServiceAdapter from '../contracts/service-adapter.js';
import { validateMethodCall, validateMessageFormat } from '../contracts/api-contract.js';
import { validateFileOrganization, getFilePathMapping } from '../contracts/file-organization.js';

console.log('='.repeat(60));
console.log('📋 WebSocket 契约集成演示');
console.log('='.repeat(60));

/**
 * 主演示函数
 */
async function runContractIntegrationDemo() {
  console.log('\n🔍 步骤 1: 验证文件组织契约\n');
  
  // 模拟项目结构（实际应该扫描文件系统）
  const projectStructure = {
    server: true,
    client: true,
    contracts: true,
    demo: true,
    files: [
      'server/websocket-server.js',
      'server/connection-manager.js',
      'client/websocket-client.js',
      'contracts/service-adapter.js'
    ]
  };

  const orgValidation = validateFileOrganization(projectStructure);
  console.log('文件组织验证结果:', orgValidation);

  const pathMapping = getFilePathMapping();
  console.log('\n文件路径映射:');
  Object.entries(pathMapping).forEach(([path, description]) => {
    console.log(`  ${path} -> ${description}`);
  });

  console.log('\n✅ 文件组织契约验证完成\n');

  // ===== 使用契约适配器 =====
  console.log('🚀 步骤 2: 初始化服务（通过契约适配器）\n');

  const service = new WebSocketServiceAdapter();

  // 验证 initialize 方法调用
  const initValidation = validateMethodCall('initialize', [{ port: 8080 }]);
  console.log('API 契约验证 (initialize):', initValidation);

  // 注册事件监听器
  service
    .onConnection((clientId, metadata) => {
      console.log(`\n👤 [契约事件] 客户端连接: ${clientId}`);
      console.log(`   元数据:`, metadata);
    })
    .onMessage((clientId, message) => {
      console.log(`\n📨 [契约事件] 收到消息 from ${clientId}:`, message);
      
      // 验证消息格式
      const msgValidation = validateMessageFormat(message);
      if (!msgValidation.valid) {
        console.warn('   ⚠️  消息格式验证失败:', msgValidation.violations);
      }
    })
    .onDisconnect((clientId, reason) => {
      console.log(`\n❌ [契约事件] 客户端断开: ${clientId}`, reason);
    })
    .onError((clientId, error) => {
      console.error(`\n⚠️  [契约事件] 错误 ${clientId}:`, error.message);
    });

  // 初始化服务
  try {
    await service.initialize({
      port: 8081, // 使用不同端口避免冲突
      heartbeatInterval: 30000,
      heartbeatTimeout: 35000
    });
    console.log('✅ 服务初始化成功\n');
  } catch (error) {
    console.error('❌ 服务初始化失败:', error);
    process.exit(1);
  }

  // ===== 演示 API 调用 =====
  console.log('📤 步骤 3: 演示契约定义的 API 调用\n');

  // 等待一段时间让客户端连接（在实际场景中）
  await new Promise(resolve => setTimeout(resolve, 2000));

  // 3.1 获取服务状态
  console.log('   3.1 获取服务状态:');
  const statusValidation = validateMethodCall('getStatus', []);
  console.log('   API 契约验证 (getStatus):', statusValidation);

  const status = await service.getStatus();
  console.log('   服务状态:', status);

  // 3.2 获取连接的客户端
  console.log('\n   3.2 获取连接的客户端:');
  const clients = service.getConnectedClients();
  console.log(`   当前连接数: ${clients.length}`);
  clients.forEach(client => {
    console.log(`   - ${client.id}: 活跃=${client.isAlive}, 连接时间=${client.connectedAt}`);
  });

  // 3.3 广播消息（验证契约）
  console.log('\n   3.3 广播消息（符合契约）:');
  
  const broadcastMessage = {
    type: 'announcement',
    content: '这是一条通过契约适配器发送的广播消息',
    timestamp: Date.now()
  };

  // 验证消息格式
  const broadcastMsgValidation = validateMessageFormat(broadcastMessage);
  console.log('   消息格式验证:', broadcastMsgValidation);

  // 验证方法调用
  const broadcastValidation = validateMethodCall('broadcast', [broadcastMessage, {}]);
  console.log('   API 契约验证 (broadcast):', broadcastValidation);

  const broadcastResult = await service.broadcast(broadcastMessage, {
    exclude: [] // 不排除任何客户端
  });
  console.log('   广播结果:', broadcastResult);

  // 3.4 单播消息（如果有连接的客户端）
  if (clients.length > 0) {
    console.log('\n   3.4 单播消息:');
    
    const targetClient = clients[0];
    const unicastMessage = {
      type: 'notification',
      content: '这是一条单播消息',
      timestamp: Date.now()
    };

    const sendValidation = validateMethodCall('sendToClient', [targetClient.id, unicastMessage]);
    console.log('   API 契约验证 (sendToClient):', sendValidation);

    const sendResult = await service.sendToClient(targetClient.id, unicastMessage);
    console.log(`   发送结果: ${sendResult ? '成功' : '失败'}`);
  }

  // ===== 定期状态检查 =====
  console.log('\n💓 步骤 4: 定期状态检查（每 10 秒）\n');
  
  const statusInterval = setInterval(async () => {
    const currentStatus = await service.getStatus();
    console.log(`📊 [${new Date().toLocaleTimeString()}] 连接数: ${currentStatus.connections.total}, 活跃: ${currentStatus.connections.alive}`);
  }, 10000);

  // ===== 优雅关闭 =====
  console.log('💡 提示: 按 Ctrl+C 停止演示\n');

  // 监听关闭信号
  const gracefulShutdown = async () => {
    console.log('\n\n📴 正在优雅关闭...');
    
    clearInterval(statusInterval);

    // 验证 shutdown 方法调用
    const shutdownValidation = validateMethodCall('shutdown', []);
    console.log('API 契约验证 (shutdown):', shutdownValidation);

    await service.shutdown();
    
    console.log('✅ 契约集成演示结束\n');
    process.exit(0);
  };

  process.on('SIGINT', gracefulShutdown);
  process.on('SIGTERM', gracefulShutdown);

  // 演示运行 30 秒后自动关闭
  setTimeout(async () => {
    console.log('\n⏰ 演示时间结束，自动关闭...');
    await gracefulShutdown();
  }, 30000);
}

// 错误处理
process.on('unhandledRejection', (error) => {
  console.error('❌ 未处理的 Promise 拒绝:', error);
  process.exit(1);
});

// 启动演示
runContractIntegrationDemo().catch(error => {
  console.error('❌ 契约集成演示失败:', error);
  process.exit(1);
});
