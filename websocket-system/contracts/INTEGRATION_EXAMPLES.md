# WebSocket 系统集成示例

本文档提供完整的集成使用示例，帮助开发者快速理解如何使用 WebSocket 系统的 API 和数据结构。

---

## 📋 目录

1. [服务端完整示例](#服务端完整示例)
2. [客户端完整示例](#客户端完整示例)
3. [聊天室应用示例](#聊天室应用示例)
4. [实时通知系统示例](#实时通知系统示例)
5. [多人协作编辑器示例](#多人协作编辑器示例)
6. [错误处理最佳实践](#错误处理最佳实践)
7. [性能优化技巧](#性能优化技巧)

---

## 🖥️ 服务端完整示例

### 基础服务端

```typescript
import {
  IWebSocketServer,
  ServerConfig,
  ServerEvent,
  MessageType,
  ErrorCode,
  WSMessage,
} from './types';
import { createServer } from './server';

async function createBasicServer() {
  // 1. 创建服务器配置
  const config: ServerConfig = {
    port: 8080,
    host: '0.0.0.0',
    heartbeatInterval: 30000,      // 30秒心跳
    heartbeatTimeout: 35000,        // 35秒超时
    maxConnections: 10000,          // 最大连接数
    maxMessageSize: 1024 * 1024,   // 1MB 消息大小限制
    compression: true,              // 启用压缩
    rateLimit: {
      window: 60000,                // 1分钟窗口
      maxMessages: 100,             // 最多100条消息
    },
    auth: {
      required: false,              // 不强制认证
      timeout: 10000,
    },
    logging: {
      level: 'info',
      logMessages: false,
    },
  };

  // 2. 创建服务器实例
  const server: IWebSocketServer = createServer(config);

  // 3. 注册事件监听器
  
  // 服务器启动事件
  server.on(ServerEvent.STARTED, () => {
    console.log('✅ WebSocket 服务器已启动');
    console.log(`🌐 监听端口: ${config.port}`);
  });

  // 新连接事件
  server.on(ServerEvent.CONNECTION, (connectionId: string, req: any) => {
    console.log('🔗 新连接:', connectionId);
    console.log('📍 IP:', req.socket.remoteAddress);
    
    // 发送欢迎消息
    server.sendToClient(connectionId, {
      type: 'welcome',
      message: '欢迎连接到 WebSocket 服务器',
      connectionId,
      timestamp: Date.now(),
    });
  });

  // 断开连接事件
  server.on(ServerEvent.DISCONNECTION, (connectionId: string, code: number, reason: string) => {
    console.log('❌ 连接断开:', connectionId);
    console.log('📝 原因:', reason || '未知');
    console.log('🔢 代码:', code);
  });

  // 消息接收事件
  server.on(ServerEvent.MESSAGE, async (connectionId: string, message: WSMessage) => {
    console.log('📨 收到消息:', {
      from: connectionId,
      type: message.header.type,
      timestamp: message.header.timestamp,
    });
  });

  // 错误事件
  server.on(ServerEvent.ERROR, (error: Error) => {
    console.error('❌ 服务器错误:', error);
  });

  // 心跳超时事件
  server.on(ServerEvent.HEARTBEAT_TIMEOUT, (connectionId: string) => {
    console.warn('⚠️ 心跳超时:', connectionId);
    // 自动断开超时连接
    server.disconnect(connectionId, 1000, '心跳超时');
  });

  // 4. 启动服务器
  await server.start();

  // 5. 定期输出统计信息
  setInterval(() => {
    const stats = server.getStats();
    console.log('📊 服务器统计:', {
      运行时长: `${Math.floor(stats.uptime / 1000 / 60)} 分钟`,
      当前连接数: stats.currentConnections,
      峰值连接数: stats.peakConnections,
      总消息数: stats.totalMessagesReceived + stats.totalMessagesSent,
      平均处理时间: `${stats.averageMessageProcessingTime.toFixed(2)}ms`,
    });
  }, 60000); // 每分钟输出一次

  // 6. 优雅关闭
  process.on('SIGINT', async () => {
    console.log('\n⏳ 正在关闭服务器...');
    await server.stop();
    console.log('✅ 服务器已关闭');
    process.exit(0);
  });

  return server;
}

// 启动服务器
createBasicServer().catch(console.error);
```

---

### 高级服务端 - 消息路由与处理

```typescript
import {
  IWebSocketServer,
  MessageType,
  ServerEvent,
  ErrorCode,
} from './types';
import { createServer } from './server';

async function createAdvancedServer() {
  const server = createServer({ port: 8080 });

  // 1. 注册消息类型处理器
  
  // 处理认证消息
  server.router.registerHandler(MessageType.AUTHENTICATE, async (connId, msg, ctx) => {
    const authData = msg.payload.data;
    
    try {
      // 验证 Token
      const authInfo = await validateToken(authData.token);
      
      // 更新连接信息（假设有这个方法）
      // ctx.connection.authenticated = true;
      // ctx.connection.userId = authInfo.userId;
      // ctx.connection.authInfo = authInfo;
      
      // 回复认证成功
      await ctx.reply({
        success: true,
        userId: authInfo.userId,
        roles: authInfo.roles,
      });
      
      console.log('✅ 用户认证成功:', authInfo.userId);
    } catch (error) {
      // 回复认证失败
      await ctx.error(ErrorCode.AUTH_FAILED, '认证失败', {
        reason: error.message,
      });
      
      console.error('❌ 认证失败:', error);
    }
  });

  // 处理普通消息
  server.router.registerHandler(MessageType.MESSAGE, async (connId, msg, ctx) => {
    // 检查认证
    if (!ctx.connection.authenticated) {
      return ctx.error(ErrorCode.UNAUTHORIZED, '请先认证');
    }

    const { content, channel } = msg.payload.data;

    // 保存消息到数据库
    const savedMessage = await saveMessageToDatabase({
      userId: ctx.connection.userId,
      content,
      channel,
      timestamp: Date.now(),
    });

    // 广播消息给其他用户
    if (channel) {
      // 发送到频道
      await server.channels.broadcast(channel, {
        header: {
          id: generateMessageId(),
          type: MessageType.BROADCAST,
          timestamp: Date.now(),
        },
        payload: {
          data: {
            messageId: savedMessage.id,
            userId: ctx.connection.userId,
            content,
            timestamp: savedMessage.timestamp,
          },
          metadata: {
            channel,
          },
        },
      });
    } else {
      // 广播给所有人（排除发送者）
      await ctx.server.broadcast({
        type: 'chat',
        from: ctx.connection.userId,
        content,
      }, {
        excludeIds: [connId],
      });
    }

    // 确认消息已处理
    await ctx.ack('success', {
      messageId: savedMessage.id,
    });
  });

  // 处理订阅消息
  server.router.registerHandler(MessageType.SUBSCRIBE, async (connId, msg, ctx) => {
    const { channels } = msg.payload.data;
    
    const results = [];
    
    for (const channel of channels) {
      const subscribed = await server.channels.subscribe(connId, channel);
      results.push({ channel, subscribed });
    }

    await ctx.reply({
      subscribed: results,
    });
  });

  // 处理自定义消息类型
  server.router.registerHandler('TYPING_INDICATOR', async (connId, msg, ctx) => {
    const { channel, isTyping } = msg.payload.data;
    
    // 广播打字指示器
    await server.channels.broadcast(channel, {
      header: {
        id: generateMessageId(),
        type: 'TYPING_INDICATOR',
        timestamp: Date.now(),
      },
      payload: {
        data: {
          userId: ctx.connection.userId,
          isTyping,
        },
      },
    });
  });

  await server.start();
  return server;
}

// 辅助函数
async function validateToken(token: string) {
  // 实现 Token 验证逻辑
  return {
    userId: 'user_123',
    roles: ['user'],
    permissions: ['read', 'write'],
  };
}

async function saveMessageToDatabase(data: any) {
  // 实现消息保存逻辑
  return {
    id: 'msg_' + Date.now(),
    ...data,
  };
}

function generateMessageId() {
  return 'msg_' + Date.now() + '_' + Math.random().toString(36).substr(2, 9);
}

createAdvancedServer().catch(console.error);
```

---

## 📱 客户端完整示例

### 基础客户端

```typescript
import {
  IWebSocketClient,
  ClientConfig,
  ClientEvent,
  MessageType,
  WSMessage,
} from './types';
import { createClient } from './client';

async function createBasicClient() {
  // 1. 创建客户端配置
  const config: ClientConfig = {
    url: 'ws://localhost:8080',
    autoReconnect: true,
    reconnect: {
      maxAttempts: 5,           // 最多重连5次
      initialDelay: 1000,       // 初始延迟1秒
      maxDelay: 30000,          // 最大延迟30秒
      strategy: 'exponential',  // 指数退避策略
      factor: 2,                // 每次延迟翻倍
    },
    heartbeatInterval: 30000,   // 30秒心跳
    heartbeatTimeout: 5000,     // 5秒超时
    connectionTimeout: 10000,   // 10秒连接超时
    maxQueueSize: 100,          // 消息队列大小
    enableQueue: true,          // 启用离线消息队列
    logging: {
      level: 'info',
      logMessages: false,
    },
  };

  // 2. 创建客户端实例
  const client: IWebSocketClient = createClient(config);

  // 3. 注册事件监听器

  // 连接中事件
  client.on(ClientEvent.CONNECTING, () => {
    console.log('🔄 正在连接到服务器...');
  });

  // 连接成功事件
  client.on(ClientEvent.CONNECTED, (connectionId: string) => {
    console.log('✅ 连接成功!');
    console.log('🆔 连接ID:', connectionId);
  });

  // 断开连接事件
  client.on(ClientEvent.DISCONNECTED, (code: number, reason: string) => {
    console.log('❌ 连接断开');
    console.log('📝 原因:', reason || '未知');
    console.log('🔢 代码:', code);
  });

  // 重连中事件
  client.on(ClientEvent.RECONNECTING, (attempt: number, delay: number) => {
    console.log(`🔄 正在重连... 第${attempt}次尝试，延迟${delay}ms`);
  });

  // 重连成功事件
  client.on(ClientEvent.RECONNECTED, (connectionId: string) => {
    console.log('✅ 重连成功!');
    console.log('🆔 新连接ID:', connectionId);
  });

  // 消息接收事件
  client.on(ClientEvent.MESSAGE, (message: WSMessage) => {
    console.log('📨 收到消息:', {
      type: message.header.type,
      data: message.payload.data,
    });
  });

  // 错误事件
  client.on(ClientEvent.ERROR, (error: Error) => {
    console.error('❌ 客户端错误:', error);
  });

  // 心跳事件
  client.on(ClientEvent.HEARTBEAT, () => {
    console.log('💓 心跳');
  });

  // 4. 连接到服务器
  try {
    await client.connect();
    console.log('🎉 已成功连接到服务器');
  } catch (error) {
    console.error('❌ 连接失败:', error);
    return;
  }

  // 5. 发送消息示例
  await client.send({
    text: 'Hello, Server!',
  }, {
    type: MessageType.MESSAGE,
  });

  return client;
}

createBasicClient().catch(console.error);
```

---

### 高级客户端 - 带认证和频道订阅

```typescript
import {
  IWebSocketClient,
  ClientEvent,
  MessageType,
} from './types';
import { createClient } from './client';

async function createAuthenticatedClient(token: string) {
  const client = createClient({
    url: 'ws://localhost:8080',
    autoReconnect: true,
    auth: {
      method: 'token',
      token,
      autoAuth: true, // 连接后自动认证
    },
  });

  // 监听认证成功
  client.on(ClientEvent.AUTHENTICATED, (authInfo: any) => {
    console.log('✅ 认证成功:', authInfo);
    
    // 认证后订阅频道
    client.subscribe(['tech-news', 'sports'], {
      includeHistory: true,
      historyLimit: 10,
    }).then((subscribed) => {
      if (subscribed) {
        console.log('✅ 频道订阅成功');
      }
    });
  });

  // 监听特定类型的消息
  client.onMessage(MessageType.BROADCAST, (message) => {
    const { content, channel } = message.payload.data;
    console.log(`📢 [${channel}] ${content}`);
  });

  client.onMessage('TYPING_INDICATOR', (message) => {
    const { userId, isTyping } = message.payload.data;
    if (isTyping) {
      console.log(`✍️ ${userId} 正在输入...`);
    }
  });

  // 连接
  await client.connect();

  // 发送聊天消息
  const sendChatMessage = async (channel: string, content: string) => {
    const result = await client.send({
      channel,
      content,
    }, {
      type: MessageType.MESSAGE,
      requireAck: true,
      ackTimeout: 5000,
    });

    if (result.success) {
      console.log('✅ 消息发送成功');
    } else {
      console.error('❌ 消息发送失败:', result.error);
    }
  };

  // 发送打字指示器
  const sendTypingIndicator = async (channel: string, isTyping: boolean) => {
    await client.send({
      channel,
      isTyping,
    }, {
      type: 'TYPING_INDICATOR',
    });
  };

  return {
    client,
    sendChatMessage,
    sendTypingIndicator,
  };
}

// 使用示例
const token = 'eyJhbGciOiJIUzI1NiIs...';
createAuthenticatedClient(token).then(({ sendChatMessage, sendTypingIndicator }) => {
  // 发送消息
  sendChatMessage('tech-news', 'Hello, everyone!');
  
  // 发送打字指示
  sendTypingIndicator('tech-news', true);
  
  setTimeout(() => {
    sendTypingIndicator('tech-news', false);
  }, 3000);
});
```

---

## 💬 聊天室应用示例

### 服务端 - 聊天室

```typescript
import { IWebSocketServer, MessageType } from './types';
import { createServer } from './server';

class ChatRoomServer {
  private server: IWebSocketServer;
  private userMap: Map<string, string> = new Map(); // connectionId -> username

  constructor(port: number) {
    this.server = createServer({ port });
    this.setupHandlers();
  }

  private setupHandlers() {
    // 用户加入聊天室
    this.server.router.registerHandler('JOIN_CHAT', async (connId, msg, ctx) => {
      const { username } = msg.payload.data;
      
      // 保存用户名映射
      this.userMap.set(connId, username);
      
      // 通知其他用户
      await ctx.server.broadcast({
        type: 'user_joined',
        username,
        timestamp: Date.now(),
      }, {
        excludeIds: [connId],
      });
      
      // 回复当前在线用户列表
      const onlineUsers = Array.from(this.userMap.values());
      await ctx.reply({
        success: true,
        onlineUsers,
      });
      
      console.log(`✅ ${username} 加入聊天室`);
    });

    // 处理聊天消息
    this.server.router.registerHandler(MessageType.MESSAGE, async (connId, msg, ctx) => {
      const username = this.userMap.get(connId);
      
      if (!username) {
        return ctx.error(2000, '请先加入聊天室');
      }

      const { content } = msg.payload.data;
      
      // 广播消息
      await ctx.server.broadcast({
        type: 'chat_message',
        from: username,
        content,
        timestamp: Date.now(),
      }, {
        excludeIds: [connId], // 不发送给自己
      });
      
      // 确认
      await ctx.ack('success');
    });

    // 用户断开连接
    this.server.on('disconnection', (connId) => {
      const username = this.userMap.get(connId);
      
      if (username) {
        // 通知其他用户
        this.server.broadcast({
          type: 'user_left',
          username,
          timestamp: Date.now(),
        });
        
        this.userMap.delete(connId);
        console.log(`❌ ${username} 离开聊天室`);
      }
    });
  }

  async start() {
    await this.server.start();
    console.log('🎯 聊天室服务器已启动');
  }
}

// 启动聊天室服务器
new ChatRoomServer(8080).start();
```

### 客户端 - 聊天室

```typescript
import { IWebSocketClient, ClientEvent } from './types';
import { createClient } from './client';

class ChatRoomClient {
  private client: IWebSocketClient;
  private username: string;
  private onlineUsers: string[] = [];

  constructor(serverUrl: string, username: string) {
    this.username = username;
    this.client = createClient({ url: serverUrl });
    this.setupHandlers();
  }

  private setupHandlers() {
    // 连接成功后加入聊天室
    this.client.on(ClientEvent.CONNECTED, async () => {
      const result = await this.client.send({
        username: this.username,
      }, {
        type: 'JOIN_CHAT',
      });
      
      if (result.success) {
        console.log('✅ 已加入聊天室');
      }
    });

    // 监听用户加入
    this.client.onMessage('user_joined', (msg) => {
      const { username } = msg.payload.data;
      this.onlineUsers.push(username);
      console.log(`👤 ${username} 加入了聊天室`);
    });

    // 监听用户离开
    this.client.onMessage('user_left', (msg) => {
      const { username } = msg.payload.data;
      this.onlineUsers = this.onlineUsers.filter(u => u !== username);
      console.log(`👋 ${username} 离开了聊天室`);
    });

    // 监听聊天消息
    this.client.onMessage('chat_message', (msg) => {
      const { from, content, timestamp } = msg.payload.data;
      const time = new Date(timestamp).toLocaleTimeString();
      console.log(`💬 [${time}] ${from}: ${content}`);
    });
  }

  async connect() {
    await this.client.connect();
  }

  async sendMessage(content: string) {
    await this.client.send({ content }, { type: 'MESSAGE' });
  }

  getOnlineUsers() {
    return this.onlineUsers;
  }
}

// 使用示例
const chatClient = new ChatRoomClient('ws://localhost:8080', 'Alice');
await chatClient.connect();
await chatClient.sendMessage('Hello, everyone!');
```

---

## 🔔 实时通知系统示例

```typescript
import { IWebSocketServer, MessageType } from './types';
import { createServer } from './server';

class NotificationServer {
  private server: IWebSocketServer;
  private userSubscriptions: Map<string, Set<string>> = new Map();

  constructor(port: number) {
    this.server = createServer({ port });
    this.setupHandlers();
  }

  private setupHandlers() {
    // 订阅通知类型
    this.server.router.registerHandler('SUBSCRIBE_NOTIFICATIONS', async (connId, msg, ctx) => {
      const { types } = msg.payload.data; // ['order', 'payment', 'system']
      
      if (!this.userSubscriptions.has(connId)) {
        this.userSubscriptions.set(connId, new Set());
      }
      
      const subs = this.userSubscriptions.get(connId)!;
      types.forEach((type: string) => subs.add(type));
      
      await ctx.reply({
        success: true,
        subscribed: Array.from(subs),
      });
    });

    // 断开时清理订阅
    this.server.on('disconnection', (connId) => {
      this.userSubscriptions.delete(connId);
    });
  }

  // 发送通知
  async sendNotification(type: string, data: any) {
    const targetConnections: string[] = [];
    
    // 找到订阅了该类型通知的所有连接
    for (const [connId, types] of this.userSubscriptions.entries()) {
      if (types.has(type)) {
        targetConnections.push(connId);
      }
    }
    
    // 批量发送
    const results = await Promise.all(
      targetConnections.map(connId =>
        this.server.sendToClient(connId, {
          type: 'notification',
          notificationType: type,
          data,
          timestamp: Date.now(),
        })
      )
    );
    
    const successCount = results.filter(r => r).length;
    console.log(`📢 通知已发送给 ${successCount}/${targetConnections.length} 个用户`);
  }

  async start() {
    await this.server.start();
    console.log('🔔 通知服务器已启动');
  }
}

// 使用示例
const notificationServer = new NotificationServer(8080);
await notificationServer.start();

// 模拟发送订单通知
setInterval(() => {
  notificationServer.sendNotification('order', {
    orderId: 'ORD_' + Date.now(),
    status: 'shipped',
    message: '您的订单已发货',
  });
}, 10000); // 每10秒发送一次
```

---

## 📝 多人协作编辑器示例

```typescript
import { IWebSocketServer } from './types';
import { createServer } from './server';

class CollaborativeEditor {
  private server: IWebSocketServer;
  private documents: Map<string, {
    content: string;
    version: number;
    editors: Set<string>; // connectionId
  }> = new Map();

  constructor(port: number) {
    this.server = createServer({ port });
    this.setupHandlers();
  }

  private setupHandlers() {
    // 加入文档编辑
    this.server.router.registerHandler('JOIN_DOCUMENT', async (connId, msg, ctx) => {
      const { documentId } = msg.payload.data;
      
      if (!this.documents.has(documentId)) {
        this.documents.set(documentId, {
          content: '',
          version: 0,
          editors: new Set(),
        });
      }
      
      const doc = this.documents.get(documentId)!;
      doc.editors.add(connId);
      
      // 返回当前文档内容
      await ctx.reply({
        content: doc.content,
        version: doc.version,
        editors: Array.from(doc.editors),
      });
      
      // 通知其他编辑者
      await this.broadcastToDocument(documentId, {
        type: 'editor_joined',
        connectionId: connId,
      }, [connId]);
    });

    // 处理编辑操作
    this.server.router.registerHandler('EDIT', async (connId, msg, ctx) => {
      const { documentId, operation, version } = msg.payload.data;
      const doc = this.documents.get(documentId);
      
      if (!doc) {
        return ctx.error(5001, '文档不存在');
      }
      
      // 检查版本冲突
      if (version !== doc.version) {
        return ctx.error(4003, '版本冲突', {
          currentVersion: doc.version,
        });
      }
      
      // 应用操作
      doc.content = this.applyOperation(doc.content, operation);
      doc.version++;
      
      // 广播操作给其他编辑者
      await this.broadcastToDocument(documentId, {
        type: 'operation',
        operation,
        version: doc.version,
        from: connId,
      }, [connId]);
      
      await ctx.ack('success', { version: doc.version });
    });

    // 离开文档
    this.server.on('disconnection', (connId) => {
      for (const [docId, doc] of this.documents.entries()) {
        if (doc.editors.has(connId)) {
          doc.editors.delete(connId);
          
          // 通知其他编辑者
          this.broadcastToDocument(docId, {
            type: 'editor_left',
            connectionId: connId,
          });
        }
      }
    });
  }

  private async broadcastToDocument(
    documentId: string,
    message: any,
    excludeIds: string[] = []
  ) {
    const doc = this.documents.get(documentId);
    if (!doc) return;
    
    const targetIds = Array.from(doc.editors).filter(
      id => !excludeIds.includes(id)
    );
    
    for (const connId of targetIds) {
      await this.server.sendToClient(connId, message);
    }
  }

  private applyOperation(content: string, operation: any): string {
    // 简化的操作应用逻辑
    const { type, position, text } = operation;
    
    if (type === 'insert') {
      return content.slice(0, position) + text + content.slice(position);
    } else if (type === 'delete') {
      return content.slice(0, position) + content.slice(position + text.length);
    }
    
    return content;
  }

  async start() {
    await this.server.start();
    console.log('📝 协作编辑服务器已启动');
  }
}

new CollaborativeEditor(8080).start();
```

---

## ⚠️ 错误处理最佳实践

```typescript
import { IWebSocketClient, ClientEvent, ErrorCode } from './types';
import { createClient } from './client';

class RobustClient {
  private client: IWebSocketClient;
  private reconnectAttempts = 0;
  private maxReconnectAttempts = 5;

  constructor(url: string) {
    this.client = createClient({
      url,
      autoReconnect: true,
    });
    
    this.setupErrorHandling();
  }

  private setupErrorHandling() {
    // 处理连接错误
    this.client.on(ClientEvent.ERROR, (error: Error) => {
      console.error('❌ 客户端错误:', error.message);
      
      // 根据错误类型采取不同的处理策略
      if (error.message.includes('ECONNREFUSED')) {
        console.log('🔧 服务器未响应，请检查服务器状态');
      } else if (error.message.includes('ETIMEDOUT')) {
        console.log('⏱️ 连接超时，请检查网络');
      }
    });

    // 处理消息中的错误
    this.client.on(ClientEvent.MESSAGE, (message) => {
      if (message.payload.error) {
        const { code, message: errorMsg, details } = message.payload.error;
        
        switch (code) {
          case ErrorCode.UNAUTHORIZED:
            console.error('🔒 未授权，请重新登录');
            this.handleUnauthorized();
            break;
            
          case ErrorCode.TOKEN_EXPIRED:
            console.error('⏰ Token 已过期，正在刷新...');
            this.refreshToken();
            break;
            
          case ErrorCode.RATE_LIMIT_EXCEEDED:
            console.warn('⚠️ 请求过于频繁，请稍后再试');
            break;
            
          case ErrorCode.MESSAGE_TOO_LARGE:
            console.error('📦 消息过大，请减小消息大小');
            break;
            
          default:
            console.error(`❌ 错误 ${code}: ${errorMsg}`, details);
        }
      }
    });

    // 处理重连
    this.client.on(ClientEvent.RECONNECTING, (attempt) => {
      this.reconnectAttempts = attempt;
      
      if (attempt >= this.maxReconnectAttempts) {
        console.error('❌ 重连失败次数过多，停止重连');
        this.client.disconnect();
      }
    });

    // 重连成功后重置计数
    this.client.on(ClientEvent.RECONNECTED, () => {
      this.reconnectAttempts = 0;
      console.log('✅ 重连成功，重新订阅频道');
      this.resubscribeChannels();
    });
  }

  private handleUnauthorized() {
    // 跳转到登录页或刷新 token
    console.log('🔄 重定向到登录页...');
  }

  private async refreshToken() {
    try {
      // 调用刷新 token API
      const newToken = await fetch('/api/refresh-token').then(r => r.json());
      
      // 重新认证
      await this.client.send({
        method: 'token',
        token: newToken.token,
      }, {
        type: 'AUTHENTICATE',
      });
    } catch (error) {
      console.error('❌ Token 刷新失败:', error);
      this.handleUnauthorized();
    }
  }

  private async resubscribeChannels() {
    // 重新订阅之前订阅的频道
    const channels = this.getSavedChannels();
    if (channels.length > 0) {
      await this.client.subscribe(channels);
    }
  }

  private getSavedChannels(): string[] {
    // 从本地存储获取之前订阅的频道
    return JSON.parse(localStorage.getItem('subscribed_channels') || '[]');
  }

  async connect() {
    await this.client.connect();
  }
}
```

---

## 🚀 性能优化技巧

### 1. 批量消息处理

```typescript
import { IWebSocketServer } from './types';

class OptimizedServer {
  private server: IWebSocketServer;
  private messageBuffer: Map<string, any[]> = new Map();
  private flushInterval = 100; // 100ms 刷新一次

  constructor(port: number) {
    this.server = createServer({ port });
    this.startBufferFlusher();
  }

  private startBufferFlusher() {
    setInterval(() => {
      this.flushBuffers();
    }, this.flushInterval);
  }

  private flushBuffers() {
    for (const [connId, messages] of this.messageBuffer.entries()) {
      if (messages.length > 0) {
        // 批量发送
        this.server.sendToClient(connId, {
          type: 'batch',
          messages,
          count: messages.length,
        });
        
        // 清空缓冲区
        messages.length = 0;
      }
    }
  }

  async bufferMessage(connId: string, message: any) {
    if (!this.messageBuffer.has(connId)) {
      this.messageBuffer.set(connId, []);
    }
    
    this.messageBuffer.get(connId)!.push(message);
    
    // 如果缓冲区满了，立即刷新
    if (this.messageBuffer.get(connId)!.length >= 10) {
      this.flushBuffers();
    }
  }
}
```

### 2. 消息压缩

```typescript
import { IWebSocketClient } from './types';
import pako from 'pako';

class CompressedClient {
  private client: IWebSocketClient;

  async sendLargeData(data: any) {
    // 序列化
    const json = JSON.stringify(data);
    
    // 压缩
    const compressed = pako.deflate(json);
    
    // 发送
    await this.client.send({
      compressed: true,
      data: Array.from(compressed), // 转为数组
    }, {
      type: 'LARGE_DATA',
    });
  }
}
```

### 3. 连接池管理

```typescript
class ConnectionPool {
  private connections: Map<string, IWebSocketClient> = new Map();
  private maxConnections = 10;

  async getConnection(userId: string): Promise<IWebSocketClient> {
    if (this.connections.has(userId)) {
      return this.connections.get(userId)!;
    }
    
    if (this.connections.size >= this.maxConnections) {
      // 移除最旧的连接
      const oldestKey = this.connections.keys().next().value;
      const oldestConn = this.connections.get(oldestKey)!;
      await oldestConn.disconnect();
      this.connections.delete(oldestKey);
    }
    
    // 创建新连接
    const client = createClient({ url: 'ws://localhost:8080' });
    await client.connect();
    this.connections.set(userId, client);
    
    return client;
  }
}
```

---

**文档状态**: ✅ 已完成  
**最后更新**: 2024
