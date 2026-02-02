# WebSocket 系统 API 接口契约

**契约ID**: contract_1770010232546_5vf7w5pz3  
**版本**: 1.0.0  
**状态**: 已发布  
**提供方**: Claude (Architecture Team)  
**消费方**: 所有集成方  

---

## 📋 契约概述

本契约定义了 WebSocket 实时消息推送系统的完整 API 接口规范，包括：

- ✅ 服务端核心 API
- ✅ 客户端核心 API
- ✅ 频道管理 API
- ✅ 房间管理 API
- ✅ 消息路由 API
- ✅ 认证 API

---

## 🔌 服务端 API 契约

### IWebSocketServer 接口

#### start(): Promise\<void\>

**描述**: 启动 WebSocket 服务器

**契约保证**:
- ✅ 幂等性：多次调用不会重复启动
- ✅ 成功时 resolve，失败时 reject
- ✅ 启动后触发 `ServerEvent.STARTED` 事件

**使用示例**:
```typescript
import { IWebSocketServer } from './types';

const server: IWebSocketServer = createServer({ port: 8080 });

try {
  await server.start();
  console.log('服务器启动成功');
} catch (error) {
  console.error('启动失败:', error);
}
```

**错误处理**:
- 端口已占用 → reject with Error
- 配置无效 → reject with Error

---

#### stop(): Promise\<void\>

**描述**: 停止 WebSocket 服务器

**契约保证**:
- ✅ 关闭所有现有连接
- ✅ 停止心跳检测
- ✅ 清理所有资源
- ✅ 触发 `ServerEvent.STOPPED` 事件

**使用示例**:
```typescript
await server.stop();
console.log('服务器已关闭');
```

---

#### sendToClient(connectionId, message, options?): Promise\<boolean\>

**描述**: 发送消息给指定客户端（单播）

**参数**:
- `connectionId: string` - 连接ID（必需）
- `message: WSMessage | any` - 消息内容（必需）
- `options?: SendMessageOptions` - 发送选项（可选）

**返回值**:
- `true` - 发送成功
- `false` - 发送失败（连接不存在或未就绪）

**契约保证**:
- ✅ 连接不存在时返回 `false`
- ✅ 自动序列化消息为 JSON
- ✅ 如果 `message` 不是 `WSMessage` 格式，自动包装
- ✅ 支持消息确认（如果 `options.requireAck = true`）

**使用示例**:
```typescript
// 发送普通对象（自动包装）
const sent = await server.sendToClient('conn_123', {
  text: 'Hello, World!',
});

// 发送完整的 WSMessage
const sent = await server.sendToClient('conn_123', {
  header: {
    id: 'msg_001',
    type: MessageType.MESSAGE,
    timestamp: Date.now(),
  },
  payload: {
    data: { text: 'Hello!' },
  },
}, {
  requireAck: true,
  ackTimeout: 5000,
});

if (!sent) {
  console.error('发送失败，连接不存在或未就绪');
}
```

**错误处理**:
- 连接不存在 → 返回 `false`
- 连接未就绪 → 返回 `false`
- 序列化失败 → 返回 `false`
- 网络错误 → 返回 `false`

---

#### broadcast(message, options?): Promise\<BatchConnectionOperationResult\>

**描述**: 广播消息给所有或部分客户端

**参数**:
- `message: WSMessage | any` - 消息内容（必需）
- `options?: BroadcastOptions` - 广播选项（可选）

**返回值**:
```typescript
interface BatchConnectionOperationResult {
  successCount: number;      // 成功数量
  failedCount: number;       // 失败数量
  successIds: string[];      // 成功的连接ID列表
  failedIds: string[];       // 失败的连接ID列表
  errors?: Array<{           // 详细错误信息
    connectionId: string;
    error: string;
  }>;
}
```

**契约保证**:
- ✅ 并行发送给所有目标连接
- ✅ 单个连接失败不影响其他连接
- ✅ 返回详细的成功/失败统计

**使用示例**:
```typescript
// 广播给所有连接
const result = await server.broadcast({
  type: 'notification',
  message: '系统维护通知',
});

console.log(`成功: ${result.successCount}, 失败: ${result.failedCount}`);

// 广播给指定频道的订阅者
const result = await server.broadcast({
  type: 'channel_message',
  content: 'Hello Channel!',
}, {
  channel: 'tech-news',
});

// 排除某些连接
const result = await server.broadcast({
  type: 'broadcast',
  data: 'Hello!',
}, {
  excludeIds: ['conn_123', 'conn_456'],
});

// 使用过滤器
const result = await server.broadcast({
  type: 'admin_notice',
  message: 'Admin only',
}, {
  filter: {
    authenticated: true,
    custom: (conn) => conn.authInfo?.roles?.includes('admin'),
  },
});
```

**广播选项**:
```typescript
interface BroadcastOptions {
  excludeIds?: string[];           // 排除的连接ID列表
  channel?: string;                // 只发送给指定频道的订阅者
  roomId?: string;                 // 只发送给指定房间的成员
  filter?: ConnectionFilter;       // 连接过滤器
  requireAck?: boolean;            // 是否需要确认
  compress?: boolean;              // 是否压缩
  priority?: 'low' | 'normal' | 'high'; // 消息优先级
}
```

---

#### getConnection(connectionId): ConnectionInfo | null

**描述**: 获取连接信息

**参数**:
- `connectionId: string` - 连接ID

**返回值**:
- `ConnectionInfo` - 连接信息对象
- `null` - 连接不存在

**契约保证**:
- ✅ 返回不可变的连接信息副本（防止外部修改）
- ✅ 包含完整的连接元数据

**使用示例**:
```typescript
const conn = server.getConnection('conn_123');

if (conn) {
  console.log('连接状态:', conn.state);
  console.log('已认证:', conn.authenticated);
  console.log('订阅频道:', Array.from(conn.subscribedChannels));
  console.log('消息统计:', {
    sent: conn.messagesSent,
    received: conn.messagesReceived,
  });
}
```

---

#### getAllConnections(filter?): ConnectionInfo[]

**描述**: 获取所有连接（支持过滤）

**参数**:
- `filter?: ConnectionFilter` - 过滤条件（可选）

**返回值**:
- `ConnectionInfo[]` - 连接信息数组

**契约保证**:
- ✅ 返回连接信息的副本数组
- ✅ 支持多种过滤条件
- ✅ 过滤条件为空时返回所有连接

**使用示例**:
```typescript
// 获取所有连接
const allConns = server.getAllConnections();

// 获取已认证的连接
const authConns = server.getAllConnections({
  authenticated: true,
});

// 获取订阅了特定频道的连接
const channelConns = server.getAllConnections({
  channel: 'tech-news',
});

// 获取特定房间的成员
const roomConns = server.getAllConnections({
  roomId: 'room_001',
});

// 使用自定义过滤器
const adminConns = server.getAllConnections({
  custom: (conn) => conn.authInfo?.roles?.includes('admin'),
});

// 组合过滤条件
const filteredConns = server.getAllConnections({
  state: ConnectionState.CONNECTED,
  authenticated: true,
  tags: ['premium'],
});
```

**过滤器选项**:
```typescript
interface ConnectionFilter {
  state?: ConnectionState | ConnectionState[];  // 连接状态
  userId?: string;                              // 用户ID
  authenticated?: boolean;                      // 是否已认证
  channel?: string;                             // 订阅的频道
  roomId?: string;                              // 加入的房间
  tags?: string[];                              // 标签
  custom?: (conn: ConnectionInfo) => boolean;   // 自定义过滤函数
}
```

---

#### disconnect(connectionId, code?, reason?): Promise\<boolean\>

**描述**: 断开指定连接

**参数**:
- `connectionId: string` - 连接ID
- `code?: number` - WebSocket 关闭码（可选，默认 1000）
- `reason?: string` - 关闭原因（可选）

**返回值**:
- `true` - 断开成功
- `false` - 连接不存在

**契约保证**:
- ✅ 优雅关闭连接（发送关闭帧）
- ✅ 清理连接相关资源
- ✅ 触发 `ServerEvent.DISCONNECTION` 事件

**使用示例**:
```typescript
// 正常关闭
await server.disconnect('conn_123');

// 指定关闭码和原因
await server.disconnect('conn_123', 1008, '违反使用条款');

// 批量断开
const connsToDisconnect = server.getAllConnections({
  custom: (conn) => Date.now() - conn.lastActiveAt > 3600000, // 1小时无活动
});

for (const conn of connsToDisconnect) {
  await server.disconnect(conn.id, 1000, '长时间未活动');
}
```

**WebSocket 关闭码参考**:
- `1000` - 正常关闭
- `1001` - 端点离开
- `1002` - 协议错误
- `1003` - 不支持的数据类型
- `1008` - 违反策略
- `1011` - 服务器错误

---

#### getStats(): ServerStats

**描述**: 获取服务器统计信息

**返回值**:
```typescript
interface ServerStats {
  startTime: number;                      // 启动时间
  uptime: number;                         // 运行时长（毫秒）
  port: number;                           // 监听端口
  currentConnections: number;             // 当前连接数
  peakConnections: number;                // 峰值连接数
  totalConnections: number;               // 累计连接数
  totalMessagesSent: number;              // 累计发送消息数
  totalMessagesReceived: number;          // 累计接收消息数
  totalErrors: number;                    // 累计错误数
  averageMessageProcessingTime: number;   // 平均处理时间（毫秒）
  memory?: {
    heapUsed?: number;                    // 堆内存使用（字节）
    heapTotal?: number;                   // 堆内存总量（字节）
  };
}
```

**契约保证**:
- ✅ 实时统计信息
- ✅ 轻量级操作（不影响性能）

**使用示例**:
```typescript
const stats = server.getStats();

console.log(`服务器运行时间: ${stats.uptime / 1000 / 60} 分钟`);
console.log(`当前连接数: ${stats.currentConnections}`);
console.log(`峰值连接数: ${stats.peakConnections}`);
console.log(`平均处理时间: ${stats.averageMessageProcessingTime}ms`);

// 定期监控
setInterval(() => {
  const stats = server.getStats();
  if (stats.currentConnections > 1000) {
    console.warn('连接数过高，考虑扩容');
  }
}, 60000); // 每分钟检查一次
```

---

#### on(event, handler): void

**描述**: 注册事件监听器

**参数**:
- `event: ServerEvent | string` - 事件类型
- `handler: Function` - 事件处理函数

**契约保证**:
- ✅ 支持同一事件注册多个处理器
- ✅ 处理器支持异步（返回 Promise）
- ✅ 处理器抛出异常不影响其他处理器

**使用示例**:
```typescript
// 监听连接事件
server.on(ServerEvent.CONNECTION, (connectionId: string, req: any) => {
  console.log('新连接:', connectionId);
  console.log('IP:', req.socket.remoteAddress);
});

// 监听消息事件
server.on(ServerEvent.MESSAGE, async (connectionId: string, message: WSMessage) => {
  console.log('收到消息:', message);
  
  // 处理消息
  await processMessage(connectionId, message);
});

// 监听断开事件
server.on(ServerEvent.DISCONNECTION, (connectionId: string, code: number, reason: string) => {
  console.log(`连接断开: ${connectionId}, 原因: ${reason}`);
});

// 监听错误事件
server.on(ServerEvent.ERROR, (error: Error) => {
  console.error('服务器错误:', error);
});

// 监听心跳超时
server.on(ServerEvent.HEARTBEAT_TIMEOUT, (connectionId: string) => {
  console.warn(`心跳超时: ${connectionId}`);
});
```

**事件类型**:
```typescript
enum ServerEvent {
  STARTED = 'started',
  STOPPED = 'stopped',
  CONNECTION = 'connection',
  DISCONNECTION = 'disconnection',
  MESSAGE = 'message',
  MESSAGE_SENT = 'message_sent',
  ERROR = 'error',
  HEARTBEAT_TIMEOUT = 'heartbeat_timeout',
  AUTHENTICATED = 'authenticated',
  AUTH_FAILED = 'auth_failed',
}
```

---

#### off(event, handler): void

**描述**: 取消事件监听器

**参数**:
- `event: ServerEvent | string` - 事件类型
- `handler: Function` - 要取消的处理函数

**使用示例**:
```typescript
const messageHandler = (connId: string, msg: WSMessage) => {
  console.log('消息:', msg);
};

// 注册
server.on(ServerEvent.MESSAGE, messageHandler);

// 取消
server.off(ServerEvent.MESSAGE, messageHandler);
```

---

### 频道管理 API (ChannelManager)

服务端通过 `server.channels` 访问频道管理器。

#### createChannel(channelName, metadata?): Promise\<boolean\>

**描述**: 创建频道

**参数**:
- `channelName: string` - 频道名称
- `metadata?: Record<string, any>` - 频道元数据（可选）

**返回值**:
- `true` - 创建成功
- `false` - 频道已存在

**使用示例**:
```typescript
const created = await server.channels.createChannel('tech-news', {
  description: '技术新闻频道',
  maxSubscribers: 1000,
});

if (created) {
  console.log('频道创建成功');
}
```

---

#### deleteChannel(channelName): Promise\<boolean\>

**描述**: 删除频道

**参数**:
- `channelName: string` - 频道名称

**返回值**:
- `true` - 删除成功
- `false` - 频道不存在

**契约保证**:
- ✅ 自动取消所有订阅
- ✅ 通知订阅者频道已删除

---

#### subscribe(connectionId, channelName): Promise\<boolean\>

**描述**: 订阅频道

**参数**:
- `connectionId: string` - 连接ID
- `channelName: string` - 频道名称

**返回值**:
- `true` - 订阅成功
- `false` - 失败（连接不存在或频道不存在）

**使用示例**:
```typescript
server.on(ServerEvent.MESSAGE, async (connId, msg) => {
  if (msg.header.type === MessageType.SUBSCRIBE) {
    const { channels } = msg.payload.data as SubscribeData;
    
    for (const channel of channels) {
      const subscribed = await server.channels.subscribe(connId, channel);
      console.log(`订阅 ${channel}: ${subscribed ? '成功' : '失败'}`);
    }
  }
});
```

---

#### unsubscribe(connectionId, channelName): Promise\<boolean\>

**描述**: 取消订阅

**参数**:
- `connectionId: string` - 连接ID
- `channelName: string` - 频道名称

**返回值**:
- `true` - 取消成功
- `false` - 失败

---

#### getSubscribers(channelName): string[]

**描述**: 获取频道订阅者列表

**参数**:
- `channelName: string` - 频道名称

**返回值**:
- `string[]` - 连接ID数组

**使用示例**:
```typescript
const subscribers = server.channels.getSubscribers('tech-news');
console.log(`订阅者数量: ${subscribers.length}`);
```

---

#### getAllChannels(): string[]

**描述**: 获取所有频道列表

**返回值**:
- `string[]` - 频道名称数组

---

#### broadcast(channelName, message, options?): Promise\<BatchConnectionOperationResult\>

**描述**: 向频道广播消息

**参数**:
- `channelName: string` - 频道名称
- `message: WSMessage` - 消息内容
- `options?: SendMessageOptions` - 发送选项

**返回值**:
- `BatchConnectionOperationResult` - 广播结果

**使用示例**:
```typescript
const result = await server.channels.broadcast('tech-news', {
  header: {
    id: 'msg_001',
    type: MessageType.BROADCAST,
    timestamp: Date.now(),
  },
  payload: {
    data: {
      title: '新技术发布',
      content: '...',
    },
  },
});

console.log(`发送给 ${result.successCount} 个订阅者`);
```

---

### 房间管理 API (RoomManager)

服务端通过 `server.rooms` 访问房间管理器。

API 与 `ChannelManager` 类似，主要方法：

- `createRoom(roomId, options?): Promise<boolean>`
- `deleteRoom(roomId): Promise<boolean>`
- `join(connectionId, roomId): Promise<boolean>`
- `leave(connectionId, roomId): Promise<boolean>`
- `getMembers(roomId): string[]`
- `getAllRooms(): string[]`
- `broadcast(roomId, message, options?): Promise<BatchConnectionOperationResult>`

---

### 消息路由 API (MessageRouter)

服务端通过 `server.router` 访问消息路由器。

#### registerHandler(messageType, handler): void

**描述**: 注册消息类型处理器

**参数**:
- `messageType: MessageType | string` - 消息类型
- `handler: MessageHandler` - 处理函数

**处理函数签名**:
```typescript
type MessageHandler = (
  connectionId: string,
  message: WSMessage,
  context: MessageContext
) => void | Promise<void>;

interface MessageContext {
  connection: ConnectionInfo;
  server: IWebSocketServer;
  reply: (data: any) => Promise<boolean>;
  error: (code: number, message: string, details?: any) => Promise<boolean>;
  ack: (status: 'success' | 'partial' | 'failed', info?: any) => Promise<boolean>;
}
```

**使用示例**:
```typescript
// 注册聊天消息处理器
server.router.registerHandler(MessageType.MESSAGE, async (connId, msg, ctx) => {
  const { content } = msg.payload.data;
  
  // 验证权限
  if (!ctx.connection.authenticated) {
    return ctx.error(ErrorCode.UNAUTHORIZED, '请先认证');
  }
  
  // 处理消息
  const saved = await saveMessage(connId, content);
  
  // 广播给其他用户
  await ctx.server.broadcast({
    type: 'chat',
    from: connId,
    content,
  }, {
    excludeIds: [connId],
  });
  
  // 回复确认
  await ctx.ack('success', { messageId: saved.id });
});

// 注册自定义消息类型
server.router.registerHandler('CUSTOM_ACTION', async (connId, msg, ctx) => {
  const result = await processCustomAction(msg.payload.data);
  await ctx.reply({ result });
});
```

---

## 📱 客户端 API 契约

### IWebSocketClient 接口

#### connect(): Promise\<void\>

**描述**: 建立 WebSocket 连接

**契约保证**:
- ✅ 如果已连接，立即返回
- ✅ 连接成功后触发 `ClientEvent.CONNECTED` 事件
- ✅ 连接失败时 reject

**使用示例**:
```typescript
import { IWebSocketClient, createClient } from './client';

const client: IWebSocketClient = createClient({
  url: 'ws://localhost:8080',
  autoReconnect: true,
});

try {
  await client.connect();
  console.log('连接成功，ID:', client.getConnectionId());
} catch (error) {
  console.error('连接失败:', error);
}
```

---

#### disconnect(code?, reason?): Promise\<void\>

**描述**: 断开连接

**参数**:
- `code?: number` - 关闭码（可选）
- `reason?: string` - 关闭原因（可选）

**契约保证**:
- ✅ 停止自动重连
- ✅ 清空消息队列
- ✅ 触发 `ClientEvent.DISCONNECTED` 事件

**使用示例**:
```typescript
await client.disconnect(1000, '用户主动断开');
```

---

#### send(message, options?): Promise\<SendResult\>

**描述**: 发送消息

**参数**:
- `message: any` - 消息内容（自动包装成 WSMessage）
- `options?: object` - 发送选项

**返回值**:
```typescript
interface SendResult {
  success: boolean;
  messageId?: string;
  error?: string;
  timestamp: number;
}
```

**契约保证**:
- ✅ 自动包装消息为 WSMessage 格式
- ✅ 未连接时加入队列（如果启用）
- ✅ 支持消息确认

**使用示例**:
```typescript
// 发送普通消息
const result = await client.send({
  text: 'Hello, Server!',
});

if (result.success) {
  console.log('发送成功，消息ID:', result.messageId);
}

// 指定消息类型
const result = await client.send({
  action: 'update_profile',
  data: { name: 'John' },
}, {
  type: MessageType.MESSAGE,
});

// 需要确认
const result = await client.send({
  important: true,
  content: 'Critical data',
}, {
  requireAck: true,
  ackTimeout: 5000,
});
```

---

#### subscribe(channels, options?): Promise\<boolean\>

**描述**: 订阅频道

**参数**:
- `channels: string[]` - 频道名称数组
- `options?: object` - 订阅选项

**返回值**:
- `true` - 订阅成功
- `false` - 订阅失败

**使用示例**:
```typescript
const subscribed = await client.subscribe(['tech-news', 'sports'], {
  includeHistory: true,
  historyLimit: 10,
});

if (subscribed) {
  console.log('订阅成功');
}
```

---

#### on(event, handler): void

**描述**: 注册事件监听器

**使用示例**:
```typescript
// 监听连接成功
client.on(ClientEvent.CONNECTED, (connectionId: string) => {
  console.log('已连接，ID:', connectionId);
});

// 监听消息
client.on(ClientEvent.MESSAGE, (message: WSMessage) => {
  console.log('收到消息:', message);
});

// 监听重连
client.on(ClientEvent.RECONNECTING, (attempt: number, delay: number) => {
  console.log(`第 ${attempt} 次重连，延迟 ${delay}ms`);
});

// 监听断开
client.on(ClientEvent.DISCONNECTED, (code: number, reason: string) => {
  console.log(`连接断开: ${code} - ${reason}`);
});
```

---

#### onMessage\<T\>(messageType, handler): void

**描述**: 注册特定类型消息的处理器

**参数**:
- `messageType: MessageType | string` - 消息类型
- `handler: MessageEventHandler<T>` - 处理函数

**使用示例**:
```typescript
// 监听聊天消息
client.onMessage<{ from: string; content: string }>(
  MessageType.MESSAGE,
  (message) => {
    const { from, content } = message.payload.data;
    console.log(`${from}: ${content}`);
  }
);

// 监听通知
client.onMessage('notification', (message) => {
  showNotification(message.payload.data);
});
```

---

## 📄 完整使用示例

### 服务端完整示例

```typescript
import {
  IWebSocketServer,
  MessageType,
  ServerEvent,
  ErrorCode,
} from './types';
import { createServer } from './server';

async function main() {
  // 创建服务器
  const server: IWebSocketServer = createServer({
    port: 8080,
    heartbeatInterval: 30000,
    heartbeatTimeout: 35000,
    maxConnections: 10000,
  });

  // 注册事件监听
  server.on(ServerEvent.CONNECTION, (connId, req) => {
    console.log('新连接:', connId, 'IP:', req.socket.remoteAddress);
  });

  server.on(ServerEvent.DISCONNECTION, (connId, code, reason) => {
    console.log('连接断开:', connId, reason);
  });

  // 注册消息处理器
  server.router.registerHandler(MessageType.MESSAGE, async (connId, msg, ctx) => {
    if (!ctx.connection.authenticated) {
      return ctx.error(ErrorCode.UNAUTHORIZED, '请先认证');
    }

    // 广播消息
    await ctx.server.broadcast({
      type: 'chat',
      from: connId,
      content: msg.payload.data,
    }, {
      excludeIds: [connId],
    });

    await ctx.ack('success');
  });

  // 启动服务器
  await server.start();
  console.log('服务器已启动');

  // 定期输出统计
  setInterval(() => {
    const stats = server.getStats();
    console.log(`连接数: ${stats.currentConnections}, 消息数: ${stats.totalMessagesReceived}`);
  }, 60000);
}

main().catch(console.error);
```

### 客户端完整示例

```typescript
import {
  IWebSocketClient,
  ClientEvent,
  MessageType,
} from './types';
import { createClient } from './client';

async function main() {
  // 创建客户端
  const client: IWebSocketClient = createClient({
    url: 'ws://localhost:8080',
    autoReconnect: true,
    reconnect: {
      maxAttempts: 5,
      initialDelay: 1000,
      strategy: 'exponential',
    },
  });

  // 注册事件
  client.on(ClientEvent.CONNECTED, (connId) => {
    console.log('连接成功:', connId);
  });

  client.on(ClientEvent.RECONNECTING, (attempt, delay) => {
    console.log(`重连中... 第${attempt}次，延迟${delay}ms`);
  });

  client.onMessage(MessageType.MESSAGE, (message) => {
    console.log('收到消息:', message.payload.data);
  });

  // 连接
  await client.connect();

  // 订阅频道
  await client.subscribe(['tech-news']);

  // 发送消息
  await client.send({
    text: 'Hello, World!',
  }, {
    type: MessageType.MESSAGE,
  });
}

main().catch(console.error);
```

---

## ✅ 契约验证清单

- [x] 所有 API 方法都有明确的签名
- [x] 参数类型明确定义
- [x] 返回值类型明确定义
- [x] 契约保证明确说明
- [x] 错误处理规范定义
- [x] 使用示例完整
- [x] 与数据结构契约一致

---

**契约状态**: ✅ 已完成  
**最后更新**: 2024  
**维护者**: Architecture Team
