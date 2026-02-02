# WebSocket 实时消息推送系统 - 架构设计文档

**版本**: 1.0.0  
**作者**: Architecture Team  
**日期**: 2024  
**状态**: 已完成设计阶段

---

## 📋 目录

1. [架构概览](#架构概览)
2. [消息协议设计](#消息协议设计)
3. [模块划分](#模块划分)
4. [接口契约](#接口契约)
5. [连接管理](#连接管理)
6. [心跳机制](#心跳机制)
7. [断线重连策略](#断线重连策略)
8. [扩展性设计](#扩展性设计)
9. [安全性考虑](#安全性考虑)
10. [性能优化](#性能优化)
11. [部署建议](#部署建议)

---

## 🏗️ 架构概览

### 设计目标

本 WebSocket 系统采用**分层架构**和**事件驱动模型**，旨在提供：

- ✅ **高可用性**: 自动心跳检测和断线重连
- ✅ **可扩展性**: 模块化设计，支持插件扩展
- ✅ **类型安全**: 完整的 TypeScript 类型定义
- ✅ **易用性**: 清晰的 API 接口和事件模型
- ✅ **高性能**: 优化的连接池和消息路由

### 整体架构图

```
┌─────────────────────────────────────────────────────────────┐
│                       应用层 (Application)                   │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │  业务逻辑    │  │  消息路由    │  │  认证授权    │         │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
└─────────────────────────────────────────────────────────────┘
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                     服务层 (Service)                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │  频道管理    │  │  房间管理    │  │  消息确认    │         │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
└─────────────────────────────────────────────────────────────┘
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    核心层 (Core)                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │  连接管理    │  │  心跳检测    │  │  消息队列    │         │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
└─────────────────────────────────────────────────────────────┘
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                    协议层 (Protocol)                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │  消息编解码  │  │  错误处理    │  │  类型定义    │         │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
└─────────────────────────────────────────────────────────────┘
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                   传输层 (Transport)                         │
│            WebSocket (ws library / Native API)              │
└─────────────────────────────────────────────────────────────┘
```

---

## 📨 消息协议设计

### 协议版本

**当前版本**: `1.0.0`

协议版本遵循语义化版本规范，用于协议升级和兼容性管理。

### 消息结构

所有消息遵循统一的 `WSMessage` 结构：

```typescript
interface WSMessage<T = any> {
  header: MessageHeader;      // 消息头（元数据）
  payload: MessagePayload<T>; // 消息体（业务数据）
}
```

#### 消息头 (Header)

```typescript
interface MessageHeader {
  id: string;              // 消息唯一ID（UUID）
  type: MessageType;       // 消息类型
  timestamp: number;       // 时间戳（毫秒）
  version?: string;        // 协议版本
  from?: string;           // 发送者ID
  to?: string;             // 接收者ID（单播）
  correlationId?: string;  // 关联消息ID（用于请求-响应）
}
```

**设计理念**:
- `id`: 用于消息追踪、去重和确认机制
- `correlationId`: 支持请求-响应模式，客户端可关联响应和原始请求
- `timestamp`: 支持消息排序和延迟计算

#### 消息类型 (MessageType)

消息类型分为三大类：

**1. 系统消息** (System Messages)
- `CONNECT`: 连接建立
- `DISCONNECT`: 连接断开
- `HEARTBEAT`: 心跳请求
- `HEARTBEAT_ACK`: 心跳响应
- `AUTH`: 认证请求
- `AUTH_ACK`: 认证响应

**2. 业务消息** (Business Messages)
- `MESSAGE`: 单播消息
- `BROADCAST`: 广播消息
- `SUBSCRIBE`: 订阅频道
- `UNSUBSCRIBE`: 取消订阅
- `JOIN_ROOM`: 加入房间
- `LEAVE_ROOM`: 离开房间

**3. 响应消息** (Response Messages)
- `ACK`: 操作成功确认
- `ERROR`: 错误响应

### 错误码体系

错误码采用 **分类编码**，便于快速定位问题：

| 范围      | 分类           | 示例                              |
|-----------|----------------|-----------------------------------|
| 1000-1999 | 消息格式错误   | 1001: 无效消息格式                |
| 2000-2999 | 认证和权限错误 | 2001: 认证失败                    |
| 3000-3999 | 连接相关错误   | 3003: 心跳超时                    |
| 4000-4999 | 业务逻辑错误   | 4001: 频道不存在                  |
| 5000-5999 | 服务器错误     | 5000: 服务器内部错误              |

### 消息示例

#### 连接建立

**客户端 → 服务端**:
```json
{
  "header": {
    "id": "msg_001",
    "type": "CONNECT",
    "timestamp": 1704067200000,
    "version": "1.0.0"
  },
  "payload": {
    "data": {
      "clientId": "client_123",
      "token": "auth_token_xxx",
      "clientInfo": {
        "platform": "web",
        "version": "1.0.0"
      }
    }
  }
}
```

**服务端 → 客户端**:
```json
{
  "header": {
    "id": "msg_002",
    "type": "CONNECT",
    "timestamp": 1704067200100,
    "correlationId": "msg_001"
  },
  "payload": {
    "data": {
      "connectionId": "conn_456",
      "serverConfig": {
        "heartbeatInterval": 30000,
        "heartbeatTimeout": 35000,
        "maxMessageSize": 1048576
      }
    }
  }
}
```

#### 心跳检测

**客户端 → 服务端**:
```json
{
  "header": {
    "id": "msg_003",
    "type": "HEARTBEAT",
    "timestamp": 1704067230000
  },
  "payload": {
    "data": {
      "clientTimestamp": 1704067230000,
      "sequence": 1
    }
  }
}
```

**服务端 → 客户端**:
```json
{
  "header": {
    "id": "msg_004",
    "type": "HEARTBEAT_ACK",
    "timestamp": 1704067230050,
    "correlationId": "msg_003"
  },
  "payload": {
    "data": {
      "serverTimestamp": 1704067230050,
      "clientTimestamp": 1704067230000
    }
  }
}
```

#### 错误响应

```json
{
  "header": {
    "id": "msg_005",
    "type": "ERROR",
    "timestamp": 1704067240000,
    "correlationId": "msg_failed"
  },
  "payload": {
    "code": 4001,
    "message": "频道不存在",
    "details": {
      "channelName": "non-existent-channel"
    }
  }
}
```

---

## 🧩 模块划分

系统采用**模块化设计**，各模块职责清晰，低耦合高内聚。

### 核心模块

#### 1. 协议模块 (Protocol)

**职责**: 定义消息格式、类型、常量

**文件**: `types/protocol.types.ts`

**主要内容**:
- 消息类型枚举 (`MessageType`)
- 错误码枚举 (`ErrorCode`)
- 消息结构定义 (`WSMessage`, `MessageHeader`, `MessagePayload`)
- 协议常量 (`PROTOCOL_VERSION`, `MAX_MESSAGE_SIZE`)
- 类型守卫函数

#### 2. 连接管理模块 (Connection Manager)

**职责**: 管理 WebSocket 连接的生命周期

**文件**: `types/connection.types.ts`, `server/connection-manager.ts`

**主要功能**:
- 连接池管理（增删查改）
- 连接状态追踪
- 连接元数据存储
- 连接过滤和查询
- 连接统计信息

**核心接口**:
```typescript
interface ConnectionInfo {
  id: string;
  ws: WebSocket;
  state: ConnectionState;
  connectedAt: number;
  lastActiveAt: number;
  lastHeartbeat: number;
  metadata: ConnectionMetadata;
  subscribedChannels: Set<string>;
  joinedRooms: Set<string>;
  // ...
}
```

#### 3. 服务端模块 (Server)

**职责**: 提供 WebSocket 服务端核心功能

**文件**: `types/server.types.ts`, `server/websocket-server.ts`

**主要功能**:
- WebSocket 服务启动/停止
- 消息发送（单播/广播）
- 事件管理（连接/断开/消息）
- 频道管理
- 房间管理
- 消息路由

**核心接口**:
```typescript
interface IWebSocketServer {
  start(): Promise<void>;
  stop(): Promise<void>;
  sendToClient(connectionId: string, message: WSMessage): Promise<boolean>;
  broadcast(message: WSMessage, options?: BroadcastOptions): Promise<Result>;
  on(event: ServerEvent, handler: Function): void;
  // ...
}
```

#### 4. 客户端模块 (Client)

**职责**: 提供 WebSocket 客户端核心功能

**文件**: `types/client.types.ts`, `client/websocket-client.ts`

**主要功能**:
- 连接建立/断开
- 消息发送/接收
- 自动重连
- 心跳保活
- 消息队列（离线缓存）
- 事件管理

**核心接口**:
```typescript
interface IWebSocketClient {
  connect(): Promise<void>;
  disconnect(): Promise<void>;
  send(message: any): Promise<SendResult>;
  subscribe(channels: string[]): Promise<boolean>;
  on(event: ClientEvent, handler: Function): void;
  // ...
}
```

#### 5. 频道管理模块 (Channel Manager)

**职责**: 管理频道订阅和消息分发

**主要功能**:
- 创建/删除频道
- 订阅/取消订阅
- 获取订阅者列表
- 频道消息广播

**核心接口**:
```typescript
interface ChannelManager {
  createChannel(name: string, metadata?: any): Promise<boolean>;
  subscribe(connectionId: string, channelName: string): Promise<boolean>;
  unsubscribe(connectionId: string, channelName: string): Promise<boolean>;
  broadcast(channelName: string, message: WSMessage): Promise<Result>;
}
```

#### 6. 房间管理模块 (Room Manager)

**职责**: 管理虚拟房间和成员

**主要功能**:
- 创建/删除房间
- 加入/离开房间
- 获取房间成员
- 房间消息广播

**核心接口**:
```typescript
interface RoomManager {
  createRoom(roomId: string, options?: any): Promise<boolean>;
  join(connectionId: string, roomId: string): Promise<boolean>;
  leave(connectionId: string, roomId: string): Promise<boolean>;
  broadcast(roomId: string, message: WSMessage): Promise<Result>;
}
```

#### 7. 消息路由模块 (Message Router)

**职责**: 根据消息类型分发到不同的处理器

**主要功能**:
- 注册消息处理器
- 消息路由和分发
- 处理器链管理

**核心接口**:
```typescript
interface MessageRouter {
  registerHandler(type: MessageType, handler: MessageHandler): void;
  unregisterHandler(type: MessageType): void;
  route(connectionId: string, message: WSMessage): Promise<void>;
}

type MessageHandler = (
  connectionId: string,
  message: WSMessage,
  context: MessageContext
) => void | Promise<void>;
```

#### 8. 认证模块 (Authentication)

**职责**: 处理连接认证和授权

**核心接口**:
```typescript
interface AuthHandler {
  authenticate(token: string, metadata?: any): Promise<{
    success: boolean;
    userId?: string;
    roles?: string[];
    permissions?: string[];
    error?: string;
  }>;
  refreshToken?(oldToken: string): Promise<string | null>;
  revokeToken?(token: string): Promise<boolean>;
}
```

### 模块依赖关系

```
┌──────────────────┐
│   Application    │  (业务层)
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  Channel/Room    │  (服务层)
│  Message Router  │
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  WebSocket       │  (核心层)
│  Server/Client   │
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  Connection      │  (基础层)
│  Manager         │
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  Protocol Types  │  (协议层)
└──────────────────┘
```

---

## 📜 接口契约

### 数据结构契约

所有模块共享以下核心数据结构：

#### WSMessage 契约

```typescript
interface WSMessage<T = any> {
  header: MessageHeader;
  payload: MessagePayload<T> | ErrorPayload | AckPayload;
}
```

**契约要求**:
1. 所有跨模块传递的消息必须符合 `WSMessage` 格式
2. `header.id` 必须唯一（建议使用 UUID）
3. `header.timestamp` 必须为 Unix 时间戳（毫秒）
4. `payload` 类型由 `header.type` 决定

#### ConnectionInfo 契约

```typescript
interface ConnectionInfo {
  id: string;
  state: ConnectionState;
  connectedAt: number;
  lastActiveAt: number;
  lastHeartbeat: number;
  // ...
}
```

**契约要求**:
1. `id` 在服务端全局唯一
2. `state` 必须为枚举 `ConnectionState` 的有效值
3. 时间戳必须为 Unix 时间戳（毫秒）

### API 接口契约

#### 服务端核心 API

```typescript
interface IWebSocketServer {
  // 生命周期管理
  start(): Promise<void>;
  stop(): Promise<void>;

  // 消息发送
  sendToClient(connectionId: string, message: WSMessage): Promise<boolean>;
  broadcast(message: WSMessage, options?: BroadcastOptions): Promise<Result>;

  // 连接管理
  getConnection(connectionId: string): ConnectionInfo | null;
  getAllConnections(filter?: ConnectionFilter): ConnectionInfo[];
  disconnect(connectionId: string, code?: number, reason?: string): Promise<boolean>;

  // 事件管理
  on(event: ServerEvent, handler: Function): void;
  off(event: ServerEvent, handler: Function): void;

  // 统计信息
  getStats(): ServerStats;

  // 子模块访问
  readonly channels: ChannelManager;
  readonly rooms: RoomManager;
  readonly router: MessageRouter;
}
```

**契约保证**:
1. `start()` 必须是幂等的（多次调用不会重复启动）
2. `sendToClient()` 在连接不存在时返回 `false`
3. `broadcast()` 返回成功和失败的统计信息
4. 所有事件处理器支持异步（返回 `Promise`）

#### 客户端核心 API

```typescript
interface IWebSocketClient {
  // 连接管理
  connect(): Promise<void>;
  disconnect(code?: number, reason?: string): Promise<void>;
  reconnect(): Promise<void>;

  // 消息发送
  send(message: any, options?: SendOptions): Promise<SendResult>;
  sendMessage(message: WSMessage, options?: SendOptions): Promise<SendResult>;

  // 频道/房间
  subscribe(channels: string[]): Promise<boolean>;
  unsubscribe(channels: string[]): Promise<boolean>;
  joinRoom(roomId: string): Promise<boolean>;
  leaveRoom(roomId: string): Promise<boolean>;

  // 状态查询
  getStatus(): ClientStatus;
  getConnectionId(): string | null;
  isConnected(): boolean;

  // 事件管理
  on(event: ClientEvent, handler: Function): void;
  off(event: ClientEvent, handler: Function): void;
  once(event: ClientEvent, handler: Function): void;
  onMessage(messageType: MessageType, handler: MessageEventHandler): void;
}
```

**契约保证**:
1. `connect()` 在已连接时立即返回
2. `send()` 在未连接时将消息加入队列（如果启用了队列）
3. `disconnect()` 会停止自动重连
4. 所有 `Promise` 在超时时会 reject

---

## 🔗 连接管理

### 连接生命周期

```
┌──────────────┐
│  CONNECTING  │  初始状态
└──────┬───────┘
       │ 连接成功
       ▼
┌──────────────┐
│  CONNECTED   │  已建立连接
└──────┬───────┘
       │ 网络异常 / 主动断开
       ▼
┌──────────────┐
│ DISCONNECTING│  正在断开
└──────┬───────┘
       │
       ▼
┌──────────────┐
│ DISCONNECTED │  已断开
└──────┬───────┘
       │ 启用自动重连
       ▼
┌──────────────┐
│ RECONNECTING │  重连中
└──────┬───────┘
       │ 重连成功
       └──────────► CONNECTED
       │ 重连失败
       └──────────► FAILED
```

### 连接池管理

#### 服务端连接池

**数据结构**:
```typescript
Map<connectionId: string, ConnectionInfo>
```

**操作**:
- `addConnection(ws, metadata)`: 添加新连接
- `removeConnection(connectionId)`: 移除连接
- `getConnection(connectionId)`: 获取连接信息
- `getAllConnections(filter?)`: 查询连接列表
- `updateHeartbeat(connectionId)`: 更新心跳时间
- `updateMetadata(connectionId, metadata)`: 更新元数据

**清理策略**:
1. 心跳超时自动清理（默认 35 秒）
2. 连接关闭时立即清理
3. 定期清理空闲连接（可选）

#### 客户端连接管理

**状态追踪**:
- 连接状态 (`ConnectionState`)
- 连接ID（服务端分配）
- 重连次数
- 消息队列

**消息队列**:
- 离线时缓存待发送消息
- 重连成功后自动发送
- 队列满时丢弃旧消息或拒绝新消息

---

## 💓 心跳机制

### 设计目标

1. **及时检测断线**: 快速发现网络中断
2. **保持连接活跃**: 防止代理/防火墙超时关闭
3. **计算网络延迟**: 提供性能监控数据

### 心跳流程

#### 服务端心跳

```
每 30 秒（heartbeatInterval）:
1. 遍历所有连接
2. 检查上次心跳时间
3. 如果超过 35 秒（heartbeatTimeout）→ 断开连接
4. 否则发送 WebSocket PING 帧
5. 接收到 PONG 帧 → 更新 lastHeartbeat
```

**实现**:
```typescript
setInterval(() => {
  connections.forEach(conn => {
    if (Date.now() - conn.lastHeartbeat > heartbeatTimeout) {
      conn.ws.terminate(); // 强制关闭
      connectionManager.removeConnection(conn.id);
    } else {
      conn.ws.ping(); // 发送 PING
    }
  });
}, heartbeatInterval);

ws.on('pong', () => {
  connectionManager.updateHeartbeat(connectionId);
});
```

#### 客户端心跳

```
每 25 秒（clientHeartbeatInterval）:
1. 发送心跳消息（MessageType.HEARTBEAT）
2. 记录发送时间戳
3. 等待服务端响应（HEARTBEAT_ACK）
4. 计算延迟 = 响应时间 - 发送时间
```

**实现**:
```typescript
setInterval(async () => {
  const startTime = Date.now();
  
  await client.send({
    type: MessageType.HEARTBEAT,
    data: {
      clientTimestamp: startTime,
      sequence: heartbeatSequence++,
    },
  });

  // 等待 HEARTBEAT_ACK 响应
  // latency = ackTimestamp - startTime
}, clientHeartbeatInterval);
```

### 心跳配置

| 参数                     | 默认值  | 说明                     |
|--------------------------|---------|--------------------------|
| `clientHeartbeatInterval`| 25000ms | 客户端发送间隔           |
| `serverHeartbeatInterval`| 30000ms | 服务端检测间隔           |
| `heartbeatTimeout`       | 35000ms | 超时阈值                 |

**配置建议**:
- 客户端间隔 < 服务端间隔 < 超时阈值
- 移动网络环境建议缩短间隔（20s/25s/30s）
- 弱网环境建议延长超时（45s）

---

## 🔄 断线重连策略

### 重连触发条件

1. 连接意外断开（非主动断开）
2. 心跳超时
3. 网络错误
4. 服务端强制断开（非业务原因）

**不触发重连**:
- 调用 `disconnect()` 主动断开
- 服务端返回 `FORBIDDEN` 错误
- 超过最大重连次数

### 重连策略

#### 指数退避 (Exponential Backoff)

**默认策略**，延迟呈指数增长：

```typescript
delay = min(
  initialDelay * (backoffFactor ^ (attempt - 1)),
  maxDelay
)
```

**示例** (initialDelay=1000ms, backoffFactor=1.5, maxDelay=30000ms):

| 尝试次数 | 延迟计算            | 实际延迟 |
|----------|---------------------|----------|
| 1        | 1000 * 1.5^0        | 1000ms   |
| 2        | 1000 * 1.5^1        | 1500ms   |
| 3        | 1000 * 1.5^2        | 2250ms   |
| 4        | 1000 * 1.5^3        | 3375ms   |
| 5        | 1000 * 1.5^4        | 5063ms   |
| ...      | ...                 | ...      |
| 10       | 1000 * 1.5^9        | 30000ms  |

#### 线性退避 (Linear Backoff)

延迟线性增长：

```typescript
delay = min(
  initialDelay + (attempt - 1) * increment,
  maxDelay
)
```

#### 固定延迟 (Fixed Delay)

所有重连使用相同延迟：

```typescript
delay = fixedDelay
```

### 重连流程

```
┌─────────────────┐
│  连接断开        │
└────────┬─────────┘
         │
         ▼
    ┌─────────────┐
    │ 是否启用重连? │ ──No──► 结束
    └────────┬──────┘
             │ Yes
             ▼
    ┌─────────────┐
    │达到最大次数?  │ ──Yes──► 触发 RECONNECT_FAILED
    └────────┬──────┘
             │ No
             ▼
    ┌─────────────┐
    │ 计算延迟时间 │
    └────────┬──────┘
             │
             ▼
    ┌─────────────┐
    │ 等待延迟     │
    └────────┬──────┘
             │
             ▼
    ┌─────────────┐
    │ 尝试重新连接 │
    └────────┬──────┘
             │
         ┌───┴───┐
         │       │
       成功     失败
         │       │
         ▼       └──► 增加尝试次数 ──► 返回"达到最大次数?"
    ┌─────────────┐
    │重置重连次数  │
    │触发RECONNECTED│
    └──────────────┘
```

### 重连配置

```typescript
interface ReconnectOptions {
  enabled: boolean;           // 是否启用
  maxAttempts: number;        // 最大次数 (0=无限)
  initialDelay: number;       // 初始延迟 (ms)
  maxDelay: number;           // 最大延迟 (ms)
  backoffFactor: number;      // 增长因子
  strategy: 'exponential' | 'linear' | 'fixed';
  reconnectOnNetworkRestore: boolean; // 网络恢复时立即重连
}
```

**默认配置**:
```typescript
{
  enabled: true,
  maxAttempts: 5,
  initialDelay: 1000,
  maxDelay: 30000,
  backoffFactor: 1.5,
  strategy: 'exponential',
  reconnectOnNetworkRestore: true,
}
```

### 重连事件

```typescript
client.on(ClientEvent.RECONNECTING, (attempt, delay) => {
  console.log(`第 ${attempt} 次重连，延迟 ${delay}ms`);
});

client.on(ClientEvent.RECONNECTED, (connectionId) => {
  console.log(`重连成功，新连接ID: ${connectionId}`);
});

client.on(ClientEvent.RECONNECT_FAILED, (attempts) => {
  console.log(`重连失败，已尝试 ${attempts} 次`);
});
```

---

## 🔌 扩展性设计

### 插件系统（未来扩展）

支持通过插件扩展功能：

```typescript
interface Plugin {
  name: string;
  version: string;
  install(server: IWebSocketServer): void;
  uninstall?(): void;
}

// 使用示例
server.use(new LoggingPlugin());
server.use(new MetricsPlugin());
server.use(new CompressionPlugin());
```

### 自定义消息类型

可注册自定义消息类型和处理器：

```typescript
// 定义自定义消息类型
const CUSTOM_MESSAGE_TYPE = 'CUSTOM_ACTION';

// 注册处理器
server.router.registerHandler(CUSTOM_MESSAGE_TYPE, async (connId, msg, ctx) => {
  // 处理自定义消息
  const result = await processCustomAction(msg.payload.data);
  
  // 回复客户端
  await ctx.reply({ result });
});
```

### 中间件支持（未来扩展）

消息处理支持中间件链：

```typescript
server.use(authMiddleware);
server.use(loggingMiddleware);
server.use(validationMiddleware);
```

---

## 🔐 安全性考虑

### 认证和授权

#### 1. Token 认证

```typescript
// 连接时提供 Token
client.connect({
  token: 'Bearer eyJhbGciOiJIUzI1NiIs...',
});

// 服务端验证
server.setAuthHandler({
  async authenticate(token, metadata) {
    const user = await verifyJWT(token);
    return {
      success: true,
      userId: user.id,
      roles: user.roles,
      permissions: user.permissions,
    };
  },
});
```

#### 2. 权限控制

```typescript
// 检查权限
function checkPermission(conn: ConnectionInfo, action: string): boolean {
  return conn.authInfo?.permissions?.includes(action) ?? false;
}

// 使用
server.router.registerHandler('ADMIN_ACTION', async (connId, msg, ctx) => {
  if (!checkPermission(ctx.connection, 'admin')) {
    return ctx.error(ErrorCode.FORBIDDEN, '权限不足');
  }
  // ...
});
```

### 输入验证

1. **消息大小限制**: 防止大消息攻击
   ```typescript
   if (message.length > MAX_MESSAGE_SIZE) {
     throw new Error('消息过大');
   }
   ```

2. **消息格式验证**: 使用 JSON Schema 或 Zod
   ```typescript
   const schema = z.object({
     header: z.object({
       id: z.string().uuid(),
       type: z.nativeEnum(MessageType),
       timestamp: z.number(),
     }),
     payload: z.any(),
   });
   
   const validated = schema.parse(rawMessage);
   ```

3. **频率限制**: 防止消息洪水攻击
   ```typescript
   // 限制每个连接的消息频率
   if (conn.messagesSent > 100 && Date.now() - conn.connectedAt < 60000) {
     conn.ws.close(1008, '消息发送过于频繁');
   }
   ```

### TLS/SSL 加密

生产环境必须使用 WSS (WebSocket Secure):

```typescript
const server = new WebSocketServer({
  port: 443,
  ssl: {
    cert: fs.readFileSync('/path/to/cert.pem'),
    key: fs.readFileSync('/path/to/key.pem'),
  },
});
```

---

## ⚡ 性能优化

### 连接池优化

1. **使用 Map 而非数组**: O(1) 查找
2. **连接限制**: 防止资源耗尽
3. **定期清理**: 移除僵尸连接

### 消息处理优化

1. **批量处理**: 合并小消息
2. **异步处理**: 避免阻塞事件循环
3. **消息压缩**: 大消息启用压缩

```typescript
server.broadcast(message, {
  compress: message.length > 1024, // 大于 1KB 启用压缩
});
```

### 广播优化

1. **并行发送**: 使用 `Promise.all()`
2. **失败快速跳过**: 不等待失败的连接
3. **分批发送**: 大量连接时分批处理

```typescript
async broadcast(message: WSMessage, options: BroadcastOptions) {
  const connections = this.getFilteredConnections(options.filter);
  const batchSize = 100;
  
  for (let i = 0; i < connections.length; i += batchSize) {
    const batch = connections.slice(i, i + batchSize);
    await Promise.all(batch.map(conn => this.sendToClient(conn.id, message)));
  }
}
```

### 内存优化

1. **及时清理断开的连接**
2. **限制消息队列大小**
3. **避免消息体深拷贝**

---

## 🚀 部署建议

### 开发环境

```bash
# 启动服务端
npm run dev:server

# 启动客户端测试
npm run dev:client
```

### 生产环境

#### 1. 负载均衡

使用 Nginx 或 HAProxy 进行负载均衡：

```nginx
upstream websocket {
  ip_hash; # 会话保持
  server ws1.example.com:8080;
  server ws2.example.com:8080;
}

server {
  listen 443 ssl;
  server_name ws.example.com;

  location / {
    proxy_pass http://websocket;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
  }
}
```

#### 2. 集群部署

使用 Redis Pub/Sub 实现跨服务器消息广播：

```typescript
// 服务器 A 发送消息
redis.publish('websocket:broadcast', JSON.stringify(message));

// 所有服务器订阅
redis.subscribe('websocket:broadcast', (channel, message) => {
  const msg = JSON.parse(message);
  localServer.broadcast(msg);
});
```

#### 3. 监控和日志

- **连接数监控**: Prometheus + Grafana
- **消息延迟监控**: 记录消息处理时间
- **错误率监控**: 统计各类错误
- **日志聚合**: ELK 或 Loki

#### 4. 容量规划

| 并发连接 | CPU核心 | 内存  | 建议配置        |
|----------|---------|-------|-----------------|
| 1000     | 2       | 1GB   | t3.small        |
| 10000    | 4       | 4GB   | t3.large        |
| 50000    | 8       | 16GB  | c5.2xlarge      |
| 100000+  | 集群    | 集群  | 多服务器 + Redis |

---

## 📚 类型定义文件清单

### 已创建文件

1. **`types/protocol.types.ts`**
   - 消息协议定义
   - 消息类型枚举
   - 错误码枚举
   - 协议常量

2. **`types/connection.types.ts`**
   - 连接信息结构
   - 连接管理配置
   - 连接事件定义

3. **`types/server.types.ts`**
   - 服务端接口契约
   - 频道管理接口
   - 房间管理接口
   - 消息路由接口

4. **`types/client.types.ts`**
   - 客户端接口契约
   - 重连策略配置
   - 客户端事件定义

5. **`types/index.ts`**
   - 统一导出入口

---

## ✅ 设计验证清单

- [x] 消息协议格式已定义（类型、结构、错误码）
- [x] 连接管理模块已设计（连接池、状态管理）
- [x] 心跳机制已设计（检测间隔、超时策略）
- [x] 断线重连策略已设计（指数退避、配置项）
- [x] 模块划分清晰（协议、连接、服务端、客户端）
- [x] 接口契约已定义（服务端、客户端、子模块）
- [x] TypeScript 类型定义完整
- [x] 架构文档完整
- [x] 扩展性设计考虑
- [x] 安全性设计考虑
- [x] 性能优化建议

---

## 📖 后续实现步骤

1. **实现连接管理模块** (TypeScript)
   - ConnectionManager 类实现
   - 连接池管理逻辑

2. **实现服务端核心** (TypeScript)
   - WebSocketServer 类实现
   - 心跳检测实现
   - 消息路由实现

3. **实现客户端核心** (TypeScript)
   - WebSocketClient 类实现
   - 断线重连实现
   - 消息队列实现

4. **实现频道和房间管理** (TypeScript)
   - ChannelManager 实现
   - RoomManager 实现

5. **编写单元测试**
   - 测试消息协议
   - 测试连接管理
   - 测试心跳机制
   - 测试重连逻辑

6. **编写集成测试**
   - 测试端到端通信
   - 测试并发场景
   - 测试异常恢复

7. **性能测试和优化**
   - 压力测试
   - 延迟测试
   - 内存泄漏检测

---

## 📞 联系方式

如有疑问或建议，请联系架构团队。

---

**文档版本**: 1.0.0  
**最后更新**: 2024
