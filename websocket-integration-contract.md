# WebSocket 实时消息推送系统 - 集成契约文档

**契约ID**: contract_1770008190053_06v7ehbw0  
**版本**: v1.0.0  
**状态**: 🟢 已发布  
**创建时间**: 2024  
**负责人**: Claude (Architecture Team)

---

## 📋 契约概述

### 目的
定义 WebSocket 实时消息推送系统中**服务端**与**客户端**、以及**各模块间**的接口契约，确保：
- ✅ Codex 团队（服务端实现）与 Gemini 团队（客户端实现）能够并行开发
- ✅ 接口定义清晰、无歧义
- ✅ 集成测试有明确验收标准

### 适用范围
- 服务端核心模块实现（Codex）
- 客户端实现（Gemini）
- 集成测试（两团队共同验收）

### 技术栈约定
- **服务端**: Node.js + ws 库
- **客户端**: 原生 JavaScript（支持浏览器和 Node.js 环境）
- **协议**: WebSocket (ws:// 或 wss://)
- **数据格式**: JSON

---

## 🔌 服务端接口契约

### 1. WebSocketServer 主类

#### 接口签名

```typescript
class WebSocketServer {
  /**
   * 构造函数
   * @param {ServerConfig} config - 服务器配置
   */
  constructor(config: ServerConfig);
  
  /**
   * 启动服务器
   * @param {number} port - 端口号（可选，默认使用构造函数配置）
   * @param {string} host - 主机地址（可选，默认 '0.0.0.0'）
   * @returns {Promise<void>}
   * @throws {Error} 端口已被占用或启动失败
   */
  async start(port?: number, host?: string): Promise<void>;
  
  /**
   * 停止服务器
   * @returns {Promise<void>}
   */
  async stop(): Promise<void>;
  
  /**
   * 获取服务器状态
   * @returns {ServerStatus}
   */
  getStatus(): ServerStatus;
  
  /**
   * 获取连接管理器实例
   * @returns {ConnectionManager}
   */
  getConnectionManager(): ConnectionManager;
  
  /**
   * 获取消息路由器实例
   * @returns {MessageRouter}
   */
  getMessageRouter(): MessageRouter;
  
  /**
   * 获取房间管理器实例
   * @returns {RoomManager}
   */
  getRoomManager(): RoomManager;
  
  /**
   * 获取事件分发器实例
   * @returns {EventDispatcher}
   */
  getEventDispatcher(): EventDispatcher;
  
  /**
   * 广播消息给所有连接
   * @param {ServerMessage} message - 消息对象
   * @param {string[]} excludeConnectionIds - 排除的连接ID列表
   * @returns {Promise<number>} 成功发送的数量
   */
  async broadcast(message: ServerMessage, excludeConnectionIds?: string[]): Promise<number>;
  
  /**
   * 监听事件
   * @param {string} eventType - 事件类型
   * @param {Function} handler - 事件处理器
   */
  on(eventType: string, handler: Function): void;
}
```

#### 配置对象

```typescript
interface ServerConfig {
  port?: number;                     // 默认: 8080
  host?: string;                     // 默认: '0.0.0.0'
  heartbeatInterval?: number;        // 心跳间隔（毫秒），默认: 30000
  heartbeatTimeout?: number;         // 心跳超时（毫秒），默认: 60000
  maxConnections?: number;           // 最大连接数，默认: 10000
  enableCompression?: boolean;       // 启用压缩，默认: false
  maxPayloadLength?: number;         // 最大消息长度（字节），默认: 1MB
}

interface ServerStatus {
  isRunning: boolean;                // 是否运行中
  port?: number;                     // 监听端口
  host?: string;                     // 监听地址
  startedAt?: number;                // 启动时间戳
  totalConnections: number;          // 当前连接数
  totalRooms: number;                // 当前房间数
}
```

#### 契约要求

| 要求 | 描述 | 优先级 |
|------|------|--------|
| **单例模式** | 每个进程只应有一个 WebSocketServer 实例 | P0 |
| **优雅关闭** | stop() 必须等待所有连接正常关闭后才返回 | P0 |
| **错误处理** | 所有异步方法必须正确处理错误并抛出有意义的异常 | P0 |
| **事件触发** | 必须在适当时机触发系统事件（如 server:started） | P1 |

---

### 2. ConnectionManager (连接管理器)

#### 接口签名

```typescript
class ConnectionManager {
  /**
   * 构造函数
   * @param {EventDispatcher} eventDispatcher - 事件分发器
   */
  constructor(eventDispatcher: EventDispatcher);
  
  /**
   * 注册新连接
   * @param {string} connectionId - 连接ID（必须唯一）
   * @param {WebSocket} ws - WebSocket 实例
   * @param {string} userId - 用户ID（可选，认证后设置）
   * @returns {void}
   * @throws {Error} 连接ID已存在
   */
  registerConnection(connectionId: string, ws: WebSocket, userId?: string): void;
  
  /**
   * 注销连接
   * @param {string} connectionId - 连接ID
   * @returns {boolean} 是否成功注销
   */
  unregisterConnection(connectionId: string): boolean;
  
  /**
   * 获取连接实例
   * @param {string} connectionId - 连接ID
   * @returns {WebSocket | undefined}
   */
  getConnection(connectionId: string): WebSocket | undefined;
  
  /**
   * 根据用户ID获取所有连接ID
   * @param {string} userId - 用户ID
   * @returns {string[]} 连接ID列表
   */
  getConnectionsByUserId(userId: string): string[];
  
  /**
   * 获取连接元数据
   * @param {string} connectionId - 连接ID
   * @returns {ConnectionMetadata | undefined}
   */
  getConnectionMetadata(connectionId: string): ConnectionMetadata | undefined;
  
  /**
   * 更新连接元数据
   * @param {string} connectionId - 连接ID
   * @param {Partial<ConnectionMetadata>} metadata - 部分元数据
   * @returns {boolean} 是否成功更新
   */
  updateConnectionMetadata(connectionId: string, metadata: Partial<ConnectionMetadata>): boolean;
  
  /**
   * 获取所有活跃连接
   * @returns {Map<string, WebSocket>}
   */
  getAllConnections(): Map<string, WebSocket>;
  
  /**
   * 启动心跳检测
   * @param {number} interval - 检测间隔（毫秒）
   * @returns {void}
   */
  startHeartbeat(interval?: number): void;
  
  /**
   * 停止心跳检测
   * @returns {void}
   */
  stopHeartbeat(): void;
  
  /**
   * 检查连接是否存活
   * @param {string} connectionId - 连接ID
   * @returns {boolean}
   */
  isAlive(connectionId: string): boolean;
  
  /**
   * 处理 PING 消息（更新最后心跳时间）
   * @param {string} connectionId - 连接ID
   * @returns {void}
   */
  handlePing(connectionId: string): void;
}
```

#### 数据结构

```typescript
interface ConnectionMetadata {
  connectionId: string;              // 连接ID
  userId?: string;                   // 用户ID（认证后）
  connectedAt: number;               // 连接时间戳
  lastHeartbeat: number;             // 最后心跳时间戳
  isAlive: boolean;                  // 是否存活
  userAgent?: string;                // 客户端 User-Agent
  ipAddress?: string;                // IP 地址
  customData?: Record<string, any>;  // 自定义数据
}
```

#### 契约要求

| 要求 | 描述 | 优先级 |
|------|------|--------|
| **线程安全** | 支持并发调用（使用适当的锁机制） | P0 |
| **自动清理** | 心跳超时的连接必须自动断开并清理 | P0 |
| **事件触发** | 连接注册/注销时必须触发对应事件 | P0 |
| **内存管理** | 断开连接后必须立即释放相关资源 | P1 |

---

### 3. MessageRouter (消息路由器)

#### 接口签名

```typescript
class MessageRouter {
  /**
   * 构造函数
   * @param {ConnectionManager} connectionManager - 连接管理器
   * @param {RoomManager} roomManager - 房间管理器
   * @param {EventDispatcher} eventDispatcher - 事件分发器
   */
  constructor(
    connectionManager: ConnectionManager,
    roomManager: RoomManager,
    eventDispatcher: EventDispatcher
  );
  
  /**
   * 路由消息到对应处理器
   * @param {ClientMessage} message - 客户端消息
   * @param {string} connectionId - 来源连接ID
   * @returns {Promise<void>}
   * @throws {Error} 消息格式错误或处理器未找到
   */
  async route(message: ClientMessage, connectionId: string): Promise<void>;
  
  /**
   * 广播消息给所有连接
   * @param {ServerMessage} message - 服务端消息
   * @param {string[]} excludeConnectionIds - 排除的连接ID
   * @returns {Promise<number>} 成功发送的数量
   */
  async broadcast(message: ServerMessage, excludeConnectionIds?: string[]): Promise<number>;
  
  /**
   * 发送点对点消息
   * @param {string} userId - 目标用户ID
   * @param {ServerMessage} message - 服务端消息
   * @returns {Promise<number>} 成功发送的数量（用户可能多端登录）
   * @throws {Error} 用户不存在或未连接
   */
  async sendToUser(userId: string, message: ServerMessage): Promise<number>;
  
  /**
   * 发送消息到指定连接
   * @param {string} connectionId - 连接ID
   * @param {ServerMessage} message - 服务端消息
   * @returns {Promise<boolean>} 是否成功发送
   */
  async sendToConnection(connectionId: string, message: ServerMessage): Promise<boolean>;
  
  /**
   * 发送消息到房间
   * @param {string} roomId - 房间ID
   * @param {ServerMessage} message - 服务端消息
   * @param {string[]} excludeConnectionIds - 排除的连接ID
   * @returns {Promise<number>} 成功发送的数量
   */
  async sendToRoom(roomId: string, message: ServerMessage, excludeConnectionIds?: string[]): Promise<number>;
  
  /**
   * 注册消息处理器
   * @param {string} messageType - 消息类型
   * @param {MessageHandler} handler - 处理器函数
   * @returns {void}
   */
  registerHandler(messageType: string, handler: MessageHandler): void;
  
  /**
   * 注销消息处理器
   * @param {string} messageType - 消息类型
   * @returns {boolean} 是否成功注销
   */
  unregisterHandler(messageType: string): boolean;
}
```

#### 类型定义

```typescript
type MessageHandler = (message: ClientMessage, connectionId: string) => Promise<void> | void;
```

#### 契约要求

| 要求 | 描述 | 优先级 |
|------|------|--------|
| **消息验证** | route() 必须验证消息格式，无效消息返回错误 | P0 |
| **错误隔离** | 单个发送失败不应影响其他发送 | P0 |
| **处理器注册** | 默认注册所有标准消息类型的处理器 | P0 |
| **性能优化** | broadcast/sendToRoom 应批量发送，避免阻塞 | P1 |

---

### 4. RoomManager (房间管理器)

#### 接口签名

```typescript
class RoomManager {
  /**
   * 构造函数
   * @param {ConnectionManager} connectionManager - 连接管理器
   * @param {EventDispatcher} eventDispatcher - 事件分发器
   */
  constructor(
    connectionManager: ConnectionManager,
    eventDispatcher: EventDispatcher
  );
  
  /**
   * 创建房间
   * @param {string} roomId - 房间ID
   * @param {RoomMetadata} metadata - 房间元数据（可选）
   * @returns {void}
   * @throws {Error} 房间已存在
   */
  createRoom(roomId: string, metadata?: RoomMetadata): void;
  
  /**
   * 删除房间
   * @param {string} roomId - 房间ID
   * @returns {boolean} 是否成功删除
   */
  deleteRoom(roomId: string): boolean;
  
  /**
   * 用户加入房间
   * @param {string} roomId - 房间ID
   * @param {string} userId - 用户ID
   * @param {string} connectionId - 连接ID
   * @returns {boolean} 是否成功加入
   * @throws {Error} 房间不存在或已满
   */
  joinRoom(roomId: string, userId: string, connectionId: string): boolean;
  
  /**
   * 用户离开房间
   * @param {string} roomId - 房间ID
   * @param {string} userId - 用户ID
   * @param {string} connectionId - 连接ID（可选，不指定则离开所有连接）
   * @returns {boolean} 是否成功离开
   */
  leaveRoom(roomId: string, userId: string, connectionId?: string): boolean;
  
  /**
   * 获取房间成员列表
   * @param {string} roomId - 房间ID
   * @returns {RoomMember[]} 成员列表
   */
  getRoomMembers(roomId: string): RoomMember[];
  
  /**
   * 获取房间元数据
   * @param {string} roomId - 房间ID
   * @returns {RoomMetadata | undefined}
   */
  getRoomMetadata(roomId: string): RoomMetadata | undefined;
  
  /**
   * 更新房间元数据
   * @param {string} roomId - 房间ID
   * @param {Partial<RoomMetadata>} metadata - 部分元数据
   * @returns {boolean} 是否成功更新
   */
  updateRoomMetadata(roomId: string, metadata: Partial<RoomMetadata>): boolean;
  
  /**
   * 获取用户加入的所有房间
   * @param {string} userId - 用户ID
   * @returns {string[]} 房间ID列表
   */
  getUserRooms(userId: string): string[];
  
  /**
   * 检查房间是否存在
   * @param {string} roomId - 房间ID
   * @returns {boolean}
   */
  roomExists(roomId: string): boolean;
  
  /**
   * 检查用户是否在房间中
   * @param {string} roomId - 房间ID
   * @param {string} userId - 用户ID
   * @returns {boolean}
   */
  isUserInRoom(roomId: string, userId: string): boolean;
  
  /**
   * 获取所有房间
   * @returns {Map<string, Room>}
   */
  getAllRooms(): Map<string, Room>;
}
```

#### 数据结构

```typescript
interface RoomMetadata {
  roomId: string;                    // 房间ID
  name?: string;                     // 房间名称
  description?: string;              // 房间描述
  createdAt: number;                 // 创建时间戳
  createdBy?: string;                // 创建者用户ID
  maxMembers?: number;               // 最大成员数
  isPrivate?: boolean;               // 是否私密
  customData?: Record<string, any>;  // 自定义数据
}

interface RoomMember {
  userId: string;                    // 用户ID
  connectionIds: string[];           // 连接ID列表（支持多端）
  joinedAt: number;                  // 加入时间戳
  role?: 'owner' | 'admin' | 'member'; // 角色
}

interface Room {
  metadata: RoomMetadata;            // 房间元数据
  members: Map<string, RoomMember>;  // 成员映射
}
```

#### 契约要求

| 要求 | 描述 | 优先级 |
|------|------|--------|
| **房间容量** | 必须检查 maxMembers 限制 | P0 |
| **自动清理** | 房间无成员时自动删除（可配置） | P1 |
| **事件触发** | 加入/离开房间时触发对应事件 | P0 |
| **多端支持** | 支持同一用户多个连接在同一房间 | P0 |

---

### 5. EventDispatcher (事件分发器)

#### 接口签名

```typescript
class EventDispatcher {
  /**
   * 注册事件监听器
   * @param {string} eventType - 事件类型
   * @param {EventListener} listener - 监听器函数
   * @returns {void}
   */
  on(eventType: string, listener: EventListener): void;
  
  /**
   * 注册一次性事件监听器
   * @param {string} eventType - 事件类型
   * @param {EventListener} listener - 监听器函数
   * @returns {void}
   */
  once(eventType: string, listener: EventListener): void;
  
  /**
   * 注销事件监听器
   * @param {string} eventType - 事件类型
   * @param {EventListener} listener - 监听器函数（可选，不指定则移除所有）
   * @returns {void}
   */
  off(eventType: string, listener?: EventListener): void;
  
  /**
   * 触发事件
   * @param {string} eventType - 事件类型
   * @param {any} data - 事件数据
   * @returns {void}
   */
  emit(eventType: string, data: any): void;
  
  /**
   * 获取事件监听器数量
   * @param {string} eventType - 事件类型
   * @returns {number}
   */
  listenerCount(eventType: string): number;
}
```

#### 类型定义

```typescript
type EventListener = (data: any) => void | Promise<void>;
```

#### 系统事件类型

```typescript
const SystemEventType = {
  // 连接事件
  CONNECTION_OPENED: 'connection:opened',
  CONNECTION_CLOSED: 'connection:closed',
  CONNECTION_ERROR: 'connection:error',
  HEARTBEAT_TIMEOUT: 'heartbeat:timeout',
  
  // 消息事件
  MESSAGE_RECEIVED: 'message:received',
  MESSAGE_SENT: 'message:sent',
  MESSAGE_ERROR: 'message:error',
  
  // 房间事件
  ROOM_CREATED: 'room:created',
  ROOM_DELETED: 'room:deleted',
  USER_JOINED_ROOM: 'user:joined_room',
  USER_LEFT_ROOM: 'user:left_room',
  
  // 系统事件
  SERVER_STARTED: 'server:started',
  SERVER_STOPPED: 'server:stopped',
  SERVER_ERROR: 'server:error',
};
```

#### 契约要求

| 要求 | 描述 | 优先级 |
|------|------|--------|
| **异步支持** | 支持异步监听器（返回 Promise） | P0 |
| **错误隔离** | 单个监听器错误不应影响其他监听器 | P0 |
| **执行顺序** | 监听器按注册顺序执行 | P1 |

---

## 🌐 客户端接口契约

### WebSocketClient 类

#### 接口签名

```typescript
class WebSocketClient {
  /**
   * 构造函数
   * @param {ClientOptions} options - 客户端配置
   */
  constructor(options?: ClientOptions);
  
  /**
   * 连接服务器
   * @param {string} url - WebSocket URL（如 ws://localhost:8080）
   * @returns {Promise<void>}
   * @throws {Error} 连接失败
   */
  async connect(url: string): Promise<void>;
  
  /**
   * 断开连接
   * @returns {void}
   */
  disconnect(): void;
  
  /**
   * 发送消息
   * @param {ClientMessage} message - 客户端消息
   * @returns {Promise<void>}
   * @throws {Error} 未连接或发送失败
   */
  async send(message: ClientMessage): Promise<void>;
  
  /**
   * 监听消息
   * @param {string} messageType - 消息类型或事件（如 'open', 'close', 'error'）
   * @param {Function} handler - 处理器函数
   * @returns {void}
   */
  on(messageType: string, handler: Function): void;
  
  /**
   * 取消监听
   * @param {string} messageType - 消息类型或事件
   * @param {Function} handler - 处理器函数（可选）
   * @returns {void}
   */
  off(messageType: string, handler?: Function): void;
  
  /**
   * 认证
   * @param {AuthPayload} payload - 认证信息
   * @returns {Promise<AuthSuccessPayload>} 认证成功响应
   * @throws {Error} 认证失败
   */
  async auth(payload: AuthPayload): Promise<AuthSuccessPayload>;
  
  /**
   * 加入房间
   * @param {string} roomId - 房间ID
   * @param {string} password - 房间密码（可选）
   * @returns {Promise<RoomJoinedPayload>} 加入成功响应
   * @throws {Error} 加入失败
   */
  async joinRoom(roomId: string, password?: string): Promise<RoomJoinedPayload>;
  
  /**
   * 离开房间
   * @param {string} roomId - 房间ID
   * @returns {Promise<void>}
   */
  async leaveRoom(roomId: string): Promise<void>;
  
  /**
   * 发送消息给用户
   * @param {string} targetUserId - 目标用户ID
   * @param {any} content - 消息内容
   * @returns {Promise<void>}
   */
  async sendToUser(targetUserId: string, content: any): Promise<void>;
  
  /**
   * 发送消息给房间
   * @param {string} roomId - 房间ID
   * @param {any} content - 消息内容
   * @returns {Promise<void>}
   */
  async sendToRoom(roomId: string, content: any): Promise<void>;
  
  /**
   * 发送 PING
   * @returns {Promise<void>}
   */
  async ping(): Promise<void>;
  
  /**
   * 获取连接状态
   * @returns {boolean} 是否已连接
   */
  isConnected(): boolean;
}
```

#### 配置对象

```typescript
interface ClientOptions {
  autoReconnect?: boolean;           // 自动重连，默认: true
  reconnectInterval?: number;        // 重连间隔（毫秒），默认: 1000
  maxReconnectAttempts?: number;     // 最大重连次数，默认: 5
  reconnectBackoff?: boolean;        // 指数退避，默认: true
  heartbeatInterval?: number;        // 心跳间隔（毫秒），默认: 30000
  timeout?: number;                  // 超时时间（毫秒），默认: 10000
}
```

#### 契约要求

| 要求 | 描述 | 优先级 |
|------|------|--------|
| **自动重连** | 断线后自动重连（可配置） | P0 |
| **指数退避** | 重连间隔指数递增，避免服务器压力 | P0 |
| **心跳保活** | 定期发送 PING 保持连接 | P0 |
| **事件监听** | 支持监听 open/close/error/message 事件 | P0 |
| **Promise 封装** | 关键方法返回 Promise 便于异步处理 | P0 |

---

## 📡 消息协议契约

### 消息基础格式

#### 客户端消息 (Client → Server)

```typescript
interface ClientMessage {
  id: string;                        // 消息ID（UUID v4）
  type: ClientMessageType;           // 消息类型
  timestamp: number;                 // 时间戳（Unix 毫秒）
  payload: any;                      // 消息负载
  version?: string;                  // 协议版本（默认 "1.0.0"）
  metadata?: Record<string, any>;    // 元数据（可选）
}
```

#### 服务端消息 (Server → Client)

```typescript
interface ServerMessage {
  id: string;                        // 消息ID（UUID v4）
  type: ServerMessageType;           // 消息类型
  timestamp: number;                 // 时间戳（Unix 毫秒）
  payload: any;                      // 消息负载
  replyTo?: string;                  // 关联的客户端消息ID
  version?: string;                  // 协议版本（默认 "1.0.0"）
  metadata?: Record<string, any>;    // 元数据（可选）
}
```

### 消息类型枚举

#### 客户端消息类型

```typescript
enum ClientMessageType {
  PING = 'ping',
  AUTH = 'auth',
  JOIN_ROOM = 'join_room',
  LEAVE_ROOM = 'leave_room',
  CREATE_ROOM = 'create_room',
  SEND_MESSAGE = 'send_message',
  BROADCAST = 'broadcast',
  GET_ROOM_MEMBERS = 'get_room_members',
  GET_USER_ROOMS = 'get_user_rooms',
  CUSTOM = 'custom',
}
```

#### 服务端消息类型

```typescript
enum ServerMessageType {
  PONG = 'pong',
  AUTH_SUCCESS = 'auth_success',
  AUTH_FAILURE = 'auth_failure',
  ROOM_JOINED = 'room_joined',
  ROOM_LEFT = 'room_left',
  ROOM_CREATED = 'room_created',
  USER_JOINED = 'user_joined',
  USER_LEFT = 'user_left',
  MESSAGE = 'message',
  BROADCAST = 'broadcast',
  ROOM_MEMBERS = 'room_members',
  USER_ROOMS = 'user_rooms',
  ERROR = 'error',
  NOTIFICATION = 'notification',
  CUSTOM = 'custom',
}
```

### Payload 定义契约

#### AUTH (认证)

```typescript
// 客户端请求
interface AuthPayload {
  token?: string;                    // JWT Token
  username?: string;                 // 用户名
  password?: string;                 // 密码
  credentials?: any;                 // 其他认证信息
}

// 服务端成功响应
interface AuthSuccessPayload {
  userId: string;                    // 用户ID
  connectionId: string;              // 连接ID
  sessionData?: any;                 // 会话数据
}

// 服务端失败响应
interface AuthFailurePayload {
  reason: string;                    // 失败原因
  code?: string;                     // 错误码
}
```

#### JOIN_ROOM (加入房间)

```typescript
// 客户端请求
interface JoinRoomPayload {
  roomId: string;                    // 房间ID
  password?: string;                 // 房间密码（私密房间）
}

// 服务端响应
interface RoomJoinedPayload {
  roomId: string;                    // 房间ID
  members: RoomMember[];             // 成员列表
  metadata?: RoomMetadata;           // 房间元数据
}
```

#### SEND_MESSAGE (发送消息)

```typescript
// 客户端请求
interface SendMessagePayload {
  targetType: 'user' | 'room';       // 目标类型
  targetId: string;                  // 目标ID
  content: any;                      // 消息内容
  contentType?: string;              // 内容类型（text/json/image等）
}

// 服务端转发（MESSAGE）
interface MessagePayload {
  messageId: string;                 // 消息ID
  fromUserId: string;                // 发送者用户ID
  targetType: 'user' | 'room';       // 目标类型
  targetId: string;                  // 目标ID
  content: any;                      // 消息内容
  contentType?: string;              // 内容类型
  sentAt: number;                    // 发送时间戳
}
```

#### ERROR (错误)

```typescript
interface ErrorPayload {
  code: string;                      // 错误码（见错误码表）
  message: string;                   // 错误描述
  details?: any;                     // 错误详情
}
```

### 错误码契约

```typescript
const ErrorCodes = {
  // 通用错误 (1000-1999)
  UNKNOWN_ERROR: '1000',
  INVALID_MESSAGE_FORMAT: '1001',
  UNSUPPORTED_MESSAGE_TYPE: '1002',
  PROTOCOL_VERSION_MISMATCH: '1003',
  
  // 认证错误 (2000-2999)
  AUTH_REQUIRED: '2000',
  AUTH_FAILED: '2001',
  INVALID_TOKEN: '2002',
  TOKEN_EXPIRED: '2003',
  PERMISSION_DENIED: '2004',
  
  // 连接错误 (3000-3999)
  CONNECTION_CLOSED: '3000',
  CONNECTION_TIMEOUT: '3001',
  HEARTBEAT_TIMEOUT: '3002',
  MAX_CONNECTIONS_REACHED: '3003',
  
  // 房间错误 (4000-4999)
  ROOM_NOT_FOUND: '4000',
  ROOM_ALREADY_EXISTS: '4001',
  ROOM_FULL: '4002',
  ROOM_PASSWORD_REQUIRED: '4003',
  ROOM_PASSWORD_INCORRECT: '4004',
  NOT_IN_ROOM: '4005',
  
  // 消息错误 (5000-5999)
  USER_NOT_FOUND: '5000',
  USER_NOT_CONNECTED: '5001',
  MESSAGE_TOO_LARGE: '5002',
  RATE_LIMIT_EXCEEDED: '5003',
};
```

---

## ✅ 集成检查清单

### 服务端（Codex 团队）

#### 模块完整性

- [ ] `WebSocketServer` 类已实现
- [ ] `ConnectionManager` 类已实现
- [ ] `MessageRouter` 类已实现
- [ ] `RoomManager` 类已实现
- [ ] `EventDispatcher` 类已实现

#### 接口遵循

- [ ] 所有方法签名与契约一致
- [ ] 所有必需参数已实现
- [ ] 返回类型正确
- [ ] 异常处理符合契约

#### 功能验证

- [ ] 服务器能够启动并监听端口
- [ ] 接受 WebSocket 连接
- [ ] 正确解析和验证 JSON 消息
- [ ] 心跳检测正常工作
- [ ] 房间创建/加入/离开功能正常
- [ ] 广播消息功能正常
- [ ] 点对点消息功能正常
- [ ] 房间消息功能正常
- [ ] 连接断开时正确清理资源

#### 事件触发

- [ ] `connection:opened` 事件触发
- [ ] `connection:closed` 事件触发
- [ ] `message:received` 事件触发
- [ ] `room:created` 事件触发
- [ ] `user:joined_room` 事件触发
- [ ] `user:left_room` 事件触发

### 客户端（Gemini 团队）

#### 模块完整性

- [ ] `WebSocketClient` 类已实现
- [ ] `ReconnectionManager` 辅助类已实现（可选）

#### 接口遵循

- [ ] 所有方法签名与契约一致
- [ ] 支持 Promise 异步操作
- [ ] 事件监听机制已实现

#### 功能验证

- [ ] 能够连接到服务器
- [ ] 发送和接收消息
- [ ] 认证功能正常
- [ ] 加入/离开房间功能正常
- [ ] 发送点对点消息功能正常
- [ ] 发送房间消息功能正常
- [ ] 心跳 PING 自动发送
- [ ] 断线自动重连（指数退避）
- [ ] 优雅断开连接

#### 事件监听

- [ ] 支持 `open` 事件
- [ ] 支持 `close` 事件
- [ ] 支持 `error` 事件
- [ ] 支持所有服务端消息类型的监听

### 集成测试（两团队共同）

#### 连接测试

- [ ] 客户端能够成功连接服务端
- [ ] 多个客户端能够同时连接
- [ ] 连接断开后客户端能够重连

#### 消息测试

- [ ] 客户端发送 PING，服务端返回 PONG
- [ ] 客户端发送认证消息，服务端正确响应
- [ ] 客户端发送消息，其他客户端能够接收

#### 房间测试

- [ ] 客户端能够创建房间
- [ ] 客户端能够加入房间
- [ ] 房间内消息广播正常
- [ ] 客户端离开房间后不再收到消息
- [ ] 其他成员能够收到用户加入/离开通知

#### 广播测试

- [ ] 服务端广播消息，所有客户端都能收到
- [ ] 排除列表功能正常

#### 错误处理测试

- [ ] 发送无效消息格式，服务端返回错误
- [ ] 未认证访问受保护资源，服务端拒绝
- [ ] 加入不存在的房间，服务端返回错误
- [ ] 发送消息给不存在的用户，服务端返回错误

#### 性能测试

- [ ] 1000 并发连接测试通过
- [ ] 消息延迟 < 100ms (P99)
- [ ] 无内存泄漏

---

## 🧪 测试契约

### 单元测试要求

#### 服务端

每个模块必须包含单元测试，覆盖率 ≥ 80%：

```javascript
// 示例：ConnectionManager 单元测试
describe('ConnectionManager', () => {
  it('应该能够注册新连接', () => {
    // 测试代码
  });
  
  it('应该能够注销连接', () => {
    // 测试代码
  });
  
  it('应该在心跳超时后自动断开连接', async () => {
    // 测试代码
  });
});
```

#### 客户端

```javascript
// 示例：WebSocketClient 单元测试
describe('WebSocketClient', () => {
  it('应该能够连接到服务器', async () => {
    // 测试代码
  });
  
  it('应该能够发送和接收消息', async () => {
    // 测试代码
  });
  
  it('应该在断线后自动重连', async () => {
    // 测试代码
  });
});
```

### 集成测试要求

#### 端到端流程测试

```javascript
describe('WebSocket 端到端测试', () => {
  it('完整的聊天室流程', async () => {
    // 1. 启动服务器
    // 2. 客户端A连接并认证
    // 3. 客户端B连接并认证
    // 4. 客户端A创建房间
    // 5. 客户端B加入房间
    // 6. 客户端A发送消息
    // 7. 验证客户端B收到消息
    // 8. 客户端B离开房间
    // 9. 验证客户端B不再收到消息
    // 10. 客户端断开连接
    // 11. 停止服务器
  });
});
```

---

## 📊 性能基准契约

### 服务端性能要求

| 指标 | 目标值 | 测试方法 |
|------|--------|----------|
| **并发连接数** | ≥ 10,000 | 使用 ws-benchmark 压测 |
| **消息吞吐** | ≥ 50,000 msg/s | 批量发送测试 |
| **消息延迟 (P99)** | < 50ms | 发送-接收时间戳对比 |
| **内存使用** | < 1GB (10,000 连接) | 监控进程内存 |
| **CPU 使用** | < 50% (单核) | 监控进程 CPU |

### 客户端性能要求

| 指标 | 目标值 | 测试方法 |
|------|--------|----------|
| **连接建立时间** | < 500ms | 计时测试 |
| **重连时间** | < 2s (首次) | 断线后重连计时 |
| **内存占用** | < 10MB | 浏览器内存监控 |
| **事件处理延迟** | < 10ms | 事件触发-处理时间 |

---

## 🔄 版本兼容性契约

### 协议版本

当前版本：`1.0.0`

### 版本协商

1. **客户端**在连接后发送的第一条消息（通常是 AUTH）中包含 `version` 字段
2. **服务端**验证版本兼容性
3. 如果不兼容，服务端返回 ERROR 消息（错误码 `1003`）

### 向后兼容

- **主版本号**（1.x.x）：破坏性变更，不保证兼容
- **次版本号**（x.1.x）：新增功能，向后兼容
- **修订号**（x.x.1）：Bug 修复，向后兼容

---

## 📝 文档交付契约

### 服务端文档（Codex）

必须提供：
- [ ] `README.md` - 项目说明、安装、使用
- [ ] `API.md` - API 文档（可选，如果有 HTTP API）
- [ ] `EXAMPLES.md` - 使用示例
- [ ] 代码注释（JSDoc 或 TypeScript 类型定义）

### 客户端文档（Gemini）

必须提供：
- [ ] `README.md` - 项目说明、安装、使用
- [ ] `API.md` - 客户端 API 文档
- [ ] `EXAMPLES.md` - 使用示例（浏览器和 Node.js）
- [ ] 代码注释

---

## 🤝 协作流程

### 开发阶段

1. **Phase 1 - 接口定义确认** (已完成)
   - Architecture Team 发布接口契约
   - Codex 和 Gemini 团队确认接口

2. **Phase 2 - 并行开发** (当前)
   - Codex: 实现服务端模块
   - Gemini: 实现客户端模块
   - 定期同步进度

3. **Phase 3 - 集成测试**
   - 联合测试服务端和客户端
   - 修复集成问题

4. **Phase 4 - 优化和文档**
   - 性能优化
   - 完善文档

### 沟通机制

- **契约变更**：必须通知所有团队并达成一致
- **问题反馈**：通过 Issue 跟踪
- **进度同步**：每日 Standup（可选）

---

## ❌ 不允许的行为

### 服务端（Codex）

- ❌ 修改已定义的接口签名（参数、返回值）
- ❌ 修改消息协议格式
- ❌ 跳过错误处理
- ❌ 使用非标准的消息类型（必须在契约中定义）

### 客户端（Gemini）

- ❌ 假设服务端行为（必须严格遵循契约）
- ❌ 跳过自动重连实现
- ❌ 不处理错误消息
- ❌ 发送不符合协议的消息

---

## ✨ 验收标准

### 最终验收条件

服务端和客户端必须**全部通过**以下测试才能验收：

1. ✅ 所有单元测试通过（覆盖率 ≥ 80%）
2. ✅ 所有集成测试通过
3. ✅ 性能基准达标
4. ✅ 无内存泄漏
5. ✅ 文档完整
6. ✅ 代码符合规范（ESLint/Prettier）

---

## 📞 联系方式

- **架构师（Claude）**: 接口契约问题、设计变更
- **Codex 团队**: 服务端实现问题
- **Gemini 团队**: 客户端实现问题

---

## 📄 附录

### A. 完整消息协议示例

#### 客户端认证请求

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "type": "auth",
  "timestamp": 1704067200000,
  "version": "1.0.0",
  "payload": {
    "username": "alice",
    "password": "secret123"
  }
}
```

#### 服务端认证成功响应

```json
{
  "id": "660e8400-e29b-41d4-a716-446655440001",
  "type": "auth_success",
  "timestamp": 1704067200100,
  "version": "1.0.0",
  "replyTo": "550e8400-e29b-41d4-a716-446655440000",
  "payload": {
    "userId": "user-001",
    "connectionId": "conn-12345",
    "sessionData": {
      "token": "jwt-token-here"
    }
  }
}
```

### B. 快速参考

#### 常用命令

```bash
# 启动服务端
node server.js

# 运行测试
npm test

# 运行集成测试
npm run test:integration

# 性能测试
npm run test:performance
```

#### 常见问题

**Q: 如果需要添加新的消息类型怎么办？**  
A: 必须在契约中添加定义，并通知所有团队。

**Q: 性能测试未达标怎么办？**  
A: 联系架构师讨论优化方案或调整性能目标。

---

**契约版本**: v1.0.0  
**最后更新**: 2024  
**状态**: 🟢 已发布

**本契约是服务端和客户端开发的唯一依据，任何变更必须经过架构师审核并更新版本号。**

---

**祝开发顺利！** 🚀
