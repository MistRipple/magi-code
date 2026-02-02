# WebSocket 系统数据结构契约

**契约ID**: contract_1770010232546_dzyfec7i4  
**版本**: 1.0.0  
**状态**: 已发布  
**提供方**: Claude (Architecture Team)  
**消费方**: 所有集成方  

---

## 📋 契约概述

本契约定义了 WebSocket 实时消息推送系统的完整数据结构规范，包括：

- ✅ 消息协议格式
- ✅ 连接管理数据结构
- ✅ 认证相关数据结构
- ✅ 频道和房间数据结构
- ✅ 错误码定义
- ✅ 配置选项数据结构

**重要说明**: 本契约定义的所有数据结构都是不可变的（immutable）契约，消费方必须严格遵守类型定义，不得擅自修改字段类型或语义。

---

## 📨 消息协议数据结构

### WSMessage（核心消息格式）

**描述**: WebSocket 消息的统一格式，所有通信必须遵循此结构

**契约保证**:
- ✅ 所有字段类型固定，不可变
- ✅ `header` 和 `payload` 必须存在
- ✅ 自动序列化为 JSON 字符串传输

**TypeScript 定义**:
```typescript
interface WSMessage {
  /** 消息头（必需） */
  header: MessageHeader;
  
  /** 消息载荷（必需） */
  payload: MessagePayload;
}
```

**JSON 示例**:
```json
{
  "header": {
    "id": "msg_1770010232546_abc123",
    "type": "MESSAGE",
    "timestamp": 1770010232546,
    "version": "1.0.0",
    "priority": "normal"
  },
  "payload": {
    "data": {
      "text": "Hello, World!"
    },
    "metadata": {
      "channel": "tech-news"
    }
  }
}
```

**使用场景**:
- ✅ 客户端到服务端的所有消息
- ✅ 服务端到客户端的所有消息
- ✅ 广播消息
- ✅ 频道/房间消息

---

### MessageHeader（消息头）

**描述**: 消息的元信息，用于路由、追踪和处理优先级

**TypeScript 定义**:
```typescript
interface MessageHeader {
  /** 消息ID（必需，唯一） */
  id: string;
  
  /** 消息类型（必需） */
  type: MessageType | string;
  
  /** 时间戳（必需，Unix毫秒） */
  timestamp: number;
  
  /** 协议版本（可选，默认 "1.0.0"） */
  version?: string;
  
  /** 关联ID（可选，用于请求-响应关联） */
  correlationId?: string;
  
  /** 优先级（可选，默认 "normal"） */
  priority?: 'low' | 'normal' | 'high' | 'critical';
  
  /** 是否需要确认（可选，默认 false） */
  requireAck?: boolean;
  
  /** 确认超时（可选，毫秒） */
  ackTimeout?: number;
  
  /** 发送者ID（可选，服务端自动填充） */
  senderId?: string;
  
  /** 接收者ID（可选，用于点对点消息） */
  receiverId?: string;
  
  /** 自定义元数据（可选） */
  metadata?: Record<string, any>;
}
```

**字段说明**:

| 字段 | 类型 | 必需 | 说明 | 示例 |
|------|------|------|------|------|
| id | string | ✅ | 全局唯一消息ID | "msg_1770010232546_abc123" |
| type | MessageType\|string | ✅ | 消息类型 | "MESSAGE", "SUBSCRIBE" |
| timestamp | number | ✅ | Unix时间戳（毫秒） | 1770010232546 |
| version | string | ❌ | 协议版本 | "1.0.0" |
| correlationId | string | ❌ | 关联请求的ID | "msg_original_123" |
| priority | string | ❌ | 消息优先级 | "high" |
| requireAck | boolean | ❌ | 是否需要确认 | true |
| ackTimeout | number | ❌ | 确认超时（毫秒） | 5000 |
| senderId | string | ❌ | 发送者连接ID | "conn_abc123" |
| receiverId | string | ❌ | 接收者连接ID | "conn_def456" |
| metadata | object | ❌ | 自定义元数据 | { "source": "mobile" } |

**使用示例**:
```typescript
const header: MessageHeader = {
  id: generateMessageId(),
  type: MessageType.MESSAGE,
  timestamp: Date.now(),
  priority: 'high',
  requireAck: true,
  ackTimeout: 5000,
};
```

---

### MessagePayload（消息载荷）

**描述**: 消息的实际数据内容

**TypeScript 定义**:
```typescript
interface MessagePayload {
  /** 消息数据（必需） */
  data: any;
  
  /** 元数据（可选） */
  metadata?: {
    /** 频道名称 */
    channel?: string;
    
    /** 房间ID */
    roomId?: string;
    
    /** 是否加密 */
    encrypted?: boolean;
    
    /** 压缩算法 */
    compression?: 'gzip' | 'deflate' | 'brotli';
    
    /** 内容类型 */
    contentType?: 'text' | 'json' | 'binary' | 'file';
    
    /** 文件信息（如果是文件类型） */
    fileInfo?: {
      name: string;
      size: number;
      mimeType: string;
      url?: string;
    };
    
    /** 自定义元数据 */
    custom?: Record<string, any>;
  };
  
  /** 错误信息（仅在错误响应时） */
  error?: {
    code: ErrorCode | number;
    message: string;
    details?: any;
    stack?: string;
  };
}
```

**使用示例**:
```typescript
// 文本消息
const textPayload: MessagePayload = {
  data: {
    text: 'Hello, World!',
  },
};

// 频道消息
const channelPayload: MessagePayload = {
  data: {
    content: 'Channel message',
  },
  metadata: {
    channel: 'tech-news',
    contentType: 'text',
  },
};

// 文件消息
const filePayload: MessagePayload = {
  data: {
    fileId: 'file_123',
  },
  metadata: {
    contentType: 'file',
    fileInfo: {
      name: 'document.pdf',
      size: 1024000,
      mimeType: 'application/pdf',
      url: 'https://cdn.example.com/file_123',
    },
  },
};

// 错误响应
const errorPayload: MessagePayload = {
  data: null,
  error: {
    code: ErrorCode.UNAUTHORIZED,
    message: '未授权',
    details: {
      requiredPermissions: ['admin'],
    },
  },
};
```

---

### MessageType（消息类型枚举）

**描述**: 预定义的消息类型，用于消息路由和处理

**TypeScript 定义**:
```typescript
enum MessageType {
  /** 握手消息 */
  HANDSHAKE = 'HANDSHAKE',
  
  /** 心跳消息 */
  HEARTBEAT = 'HEARTBEAT',
  
  /** 普通消息 */
  MESSAGE = 'MESSAGE',
  
  /** 广播消息 */
  BROADCAST = 'BROADCAST',
  
  /** 订阅请求 */
  SUBSCRIBE = 'SUBSCRIBE',
  
  /** 取消订阅 */
  UNSUBSCRIBE = 'UNSUBSCRIBE',
  
  /** 认证请求 */
  AUTHENTICATE = 'AUTHENTICATE',
  
  /** 加入房间 */
  JOIN_ROOM = 'JOIN_ROOM',
  
  /** 离开房间 */
  LEAVE_ROOM = 'LEAVE_ROOM',
  
  /** 确认消息 */
  ACK = 'ACK',
  
  /** 错误消息 */
  ERROR = 'ERROR',
  
  /** 系统消息 */
  SYSTEM = 'SYSTEM',
  
  /** 通知消息 */
  NOTIFICATION = 'NOTIFICATION',
  
  /** 命令消息 */
  COMMAND = 'COMMAND',
  
  /** 查询消息 */
  QUERY = 'QUERY',
  
  /** 响应消息 */
  RESPONSE = 'RESPONSE',
}
```

**类型说明表**:

| 类型 | 值 | 方向 | 说明 | 示例场景 |
|------|-------|------|------|----------|
| HANDSHAKE | "HANDSHAKE" | 双向 | 连接建立后的握手 | 协议版本协商 |
| HEARTBEAT | "HEARTBEAT" | 双向 | 心跳保活 | 每30秒发送一次 |
| MESSAGE | "MESSAGE" | 双向 | 普通消息 | 聊天消息 |
| BROADCAST | "BROADCAST" | 服务端→客户端 | 广播消息 | 系统公告 |
| SUBSCRIBE | "SUBSCRIBE" | 客户端→服务端 | 订阅频道 | 订阅技术新闻频道 |
| UNSUBSCRIBE | "UNSUBSCRIBE" | 客户端→服务端 | 取消订阅 | 取消订阅 |
| AUTHENTICATE | "AUTHENTICATE" | 客户端→服务端 | 认证 | 用户登录认证 |
| JOIN_ROOM | "JOIN_ROOM" | 客户端→服务端 | 加入房间 | 加入聊天室 |
| LEAVE_ROOM | "LEAVE_ROOM" | 客户端→服务端 | 离开房间 | 退出聊天室 |
| ACK | "ACK" | 双向 | 确认消息 | 消息送达确认 |
| ERROR | "ERROR" | 双向 | 错误响应 | 权限不足错误 |
| SYSTEM | "SYSTEM" | 服务端→客户端 | 系统消息 | 服务器维护通知 |
| NOTIFICATION | "NOTIFICATION" | 服务端→客户端 | 通知 | 新消息通知 |
| COMMAND | "COMMAND" | 双向 | 命令 | 执行特定操作 |
| QUERY | "QUERY" | 客户端→服务端 | 查询请求 | 查询在线用户 |
| RESPONSE | "RESPONSE" | 服务端→客户端 | 查询响应 | 返回查询结果 |

**扩展说明**:
- ✅ 支持自定义类型（使用字符串）
- ✅ 建议自定义类型使用大写，避免与预定义类型冲突
- ✅ 自定义类型示例: `"CUSTOM_ACTION"`, `"GAME_MOVE"`

---

### 特定消息类型的数据结构

#### SubscribeData（订阅数据）

```typescript
interface SubscribeData {
  /** 要订阅的频道列表 */
  channels: string[];
  
  /** 是否包含历史消息 */
  includeHistory?: boolean;
  
  /** 历史消息数量限制 */
  historyLimit?: number;
  
  /** 订阅选项 */
  options?: {
    /** 消息过滤器 */
    filter?: any;
    
    /** 自定义参数 */
    custom?: Record<string, any>;
  };
}
```

**使用示例**:
```typescript
const subscribeMsg: WSMessage = {
  header: {
    id: generateMessageId(),
    type: MessageType.SUBSCRIBE,
    timestamp: Date.now(),
  },
  payload: {
    data: {
      channels: ['tech-news', 'sports'],
      includeHistory: true,
      historyLimit: 10,
    } as SubscribeData,
  },
};
```

---

#### AuthData（认证数据）

```typescript
interface AuthData {
  /** 认证方式 */
  method: 'token' | 'credentials' | 'oauth' | 'custom';
  
  /** Token（如果使用token认证） */
  token?: string;
  
  /** 用户名（如果使用凭证认证） */
  username?: string;
  
  /** 密码（如果使用凭证认证） */
  password?: string;
  
  /** OAuth提供商 */
  provider?: string;
  
  /** OAuth访问令牌 */
  accessToken?: string;
  
  /** 自定义认证参数 */
  custom?: Record<string, any>;
}
```

**使用示例**:
```typescript
// Token认证
const authMsg: WSMessage = {
  header: {
    id: generateMessageId(),
    type: MessageType.AUTHENTICATE,
    timestamp: Date.now(),
  },
  payload: {
    data: {
      method: 'token',
      token: 'eyJhbGciOiJIUzI1NiIs...',
    } as AuthData,
  },
};

// 用户名密码认证
const authMsg: WSMessage = {
  header: {
    id: generateMessageId(),
    type: MessageType.AUTHENTICATE,
    timestamp: Date.now(),
  },
  payload: {
    data: {
      method: 'credentials',
      username: 'john@example.com',
      password: 'password123',
    } as AuthData,
  },
};
```

---

#### JoinRoomData（加入房间数据）

```typescript
interface JoinRoomData {
  /** 房间ID */
  roomId: string;
  
  /** 加入选项 */
  options?: {
    /** 用户显示名称 */
    displayName?: string;
    
    /** 用户角色 */
    role?: string;
    
    /** 自定义参数 */
    custom?: Record<string, any>;
  };
}
```

---

#### AckData（确认数据）

```typescript
interface AckData {
  /** 被确认的消息ID */
  messageId: string;
  
  /** 确认状态 */
  status: 'success' | 'partial' | 'failed';
  
  /** 确认信息 */
  info?: any;
  
  /** 时间戳 */
  timestamp: number;
}
```

**使用示例**:
```typescript
const ackMsg: WSMessage = {
  header: {
    id: generateMessageId(),
    type: MessageType.ACK,
    timestamp: Date.now(),
    correlationId: 'msg_original_123', // 关联原始消息
  },
  payload: {
    data: {
      messageId: 'msg_original_123',
      status: 'success',
      timestamp: Date.now(),
    } as AckData,
  },
};
```

---

## 🔌 连接管理数据结构

### ConnectionInfo（连接信息）

**描述**: 单个 WebSocket 连接的完整信息

**TypeScript 定义**:
```typescript
interface ConnectionInfo {
  /** 连接ID（唯一） */
  id: string;
  
  /** 连接状态 */
  state: ConnectionState;
  
  /** 建立时间 */
  connectedAt: number;
  
  /** 最后活跃时间 */
  lastActiveAt: number;
  
  /** 最后心跳时间 */
  lastHeartbeatAt: number;
  
  /** 是否已认证 */
  authenticated: boolean;
  
  /** 用户ID（认证后） */
  userId?: string;
  
  /** 认证信息 */
  authInfo?: {
    method: string;
    roles?: string[];
    permissions?: string[];
    custom?: Record<string, any>;
  };
  
  /** 订阅的频道 */
  subscribedChannels: Set<string>;
  
  /** 加入的房间 */
  joinedRooms: Set<string>;
  
  /** 发送的消息数 */
  messagesSent: number;
  
  /** 接收的消息数 */
  messagesReceived: number;
  
  /** 连接元数据 */
  metadata: {
    /** IP地址 */
    ip?: string;
    
    /** User-Agent */
    userAgent?: string;
    
    /** 客户端类型 */
    clientType?: 'web' | 'mobile' | 'desktop' | 'iot' | 'other';
    
    /** 客户端版本 */
    clientVersion?: string;
    
    /** 协议版本 */
    protocolVersion?: string;
    
    /** 自定义元数据 */
    custom?: Record<string, any>;
  };
  
  /** 连接标签（用于分组） */
  tags?: Set<string>;
  
  /** 自定义数据 */
  custom?: Record<string, any>;
}
```

**JSON 示例**（导出时 Set 转为数组）:
```json
{
  "id": "conn_1770010232546_abc123",
  "state": "CONNECTED",
  "connectedAt": 1770010232546,
  "lastActiveAt": 1770010262546,
  "lastHeartbeatAt": 1770010260000,
  "authenticated": true,
  "userId": "user_12345",
  "authInfo": {
    "method": "token",
    "roles": ["user", "premium"],
    "permissions": ["read", "write"]
  },
  "subscribedChannels": ["tech-news", "sports"],
  "joinedRooms": ["room_general"],
  "messagesSent": 150,
  "messagesReceived": 200,
  "metadata": {
    "ip": "192.168.1.100",
    "userAgent": "Mozilla/5.0...",
    "clientType": "web",
    "clientVersion": "2.1.0",
    "protocolVersion": "1.0.0"
  },
  "tags": ["premium", "verified"]
}
```

---

### ConnectionState（连接状态枚举）

**TypeScript 定义**:
```typescript
enum ConnectionState {
  /** 连接中 */
  CONNECTING = 'CONNECTING',
  
  /** 已连接 */
  CONNECTED = 'CONNECTED',
  
  /** 认证中 */
  AUTHENTICATING = 'AUTHENTICATING',
  
  /** 已认证 */
  AUTHENTICATED = 'AUTHENTICATED',
  
  /** 断开中 */
  DISCONNECTING = 'DISCONNECTING',
  
  /** 已断开 */
  DISCONNECTED = 'DISCONNECTED',
  
  /** 重连中 */
  RECONNECTING = 'RECONNECTING',
  
  /** 错误 */
  ERROR = 'ERROR',
}
```

**状态转换图**:
```
CONNECTING → CONNECTED → AUTHENTICATING → AUTHENTICATED
     ↓           ↓              ↓                ↓
ERROR ← ──────────────────────────────────────────
     ↓
DISCONNECTING → DISCONNECTED → RECONNECTING → CONNECTING
```

---

### ConnectionFilter（连接过滤器）

**描述**: 用于筛选连接的条件

**TypeScript 定义**:
```typescript
interface ConnectionFilter {
  /** 连接状态 */
  state?: ConnectionState | ConnectionState[];
  
  /** 用户ID */
  userId?: string;
  
  /** 是否已认证 */
  authenticated?: boolean;
  
  /** 订阅的频道 */
  channel?: string;
  
  /** 加入的房间 */
  roomId?: string;
  
  /** 标签 */
  tags?: string[];
  
  /** 自定义过滤函数 */
  custom?: (conn: ConnectionInfo) => boolean;
}
```

**使用示例**:
```typescript
// 获取所有已认证的连接
const authConns = server.getAllConnections({
  authenticated: true,
});

// 获取订阅了特定频道的连接
const techConns = server.getAllConnections({
  channel: 'tech-news',
});

// 使用自定义过滤器
const premiumConns = server.getAllConnections({
  custom: (conn) => conn.tags?.has('premium'),
});

// 组合条件
const filteredConns = server.getAllConnections({
  state: ConnectionState.AUTHENTICATED,
  authenticated: true,
  tags: ['premium'],
  custom: (conn) => conn.messagesSent > 100,
});
```

---

## 📊 统计数据结构

### ServerStats（服务器统计）

**TypeScript 定义**:
```typescript
interface ServerStats {
  /** 启动时间 */
  startTime: number;
  
  /** 运行时长（毫秒） */
  uptime: number;
  
  /** 监听端口 */
  port: number;
  
  /** 当前连接数 */
  currentConnections: number;
  
  /** 峰值连接数 */
  peakConnections: number;
  
  /** 累计连接数 */
  totalConnections: number;
  
  /** 累计发送消息数 */
  totalMessagesSent: number;
  
  /** 累计接收消息数 */
  totalMessagesReceived: number;
  
  /** 累计错误数 */
  totalErrors: number;
  
  /** 平均消息处理时间（毫秒） */
  averageMessageProcessingTime: number;
  
  /** 内存使用情况 */
  memory?: {
    heapUsed?: number;
    heapTotal?: number;
    external?: number;
    rss?: number;
  };
  
  /** 自定义统计 */
  custom?: Record<string, any>;
}
```

---

### BatchConnectionOperationResult（批量操作结果）

**描述**: 批量连接操作（如广播）的结果

**TypeScript 定义**:
```typescript
interface BatchConnectionOperationResult {
  /** 成功数量 */
  successCount: number;
  
  /** 失败数量 */
  failedCount: number;
  
  /** 成功的连接ID列表 */
  successIds: string[];
  
  /** 失败的连接ID列表 */
  failedIds: string[];
  
  /** 详细错误信息 */
  errors?: Array<{
    connectionId: string;
    error: string;
  }>;
}
```

---

## ⚠️ 错误码定义

### ErrorCode（错误码枚举）

**TypeScript 定义**:
```typescript
enum ErrorCode {
  // 1xxx - 通用错误
  /** 未知错误 */
  UNKNOWN = 1000,
  
  /** 无效的消息格式 */
  INVALID_MESSAGE_FORMAT = 1001,
  
  /** 无效的参数 */
  INVALID_PARAMETER = 1002,
  
  /** 服务器内部错误 */
  INTERNAL_ERROR = 1003,
  
  /** 超时 */
  TIMEOUT = 1004,
  
  /** 未实现 */
  NOT_IMPLEMENTED = 1005,
  
  // 2xxx - 认证/授权错误
  /** 未授权 */
  UNAUTHORIZED = 2000,
  
  /** 认证失败 */
  AUTH_FAILED = 2001,
  
  /** Token无效 */
  INVALID_TOKEN = 2002,
  
  /** Token过期 */
  TOKEN_EXPIRED = 2003,
  
  /** 权限不足 */
  PERMISSION_DENIED = 2004,
  
  // 3xxx - 连接错误
  /** 连接未建立 */
  NOT_CONNECTED = 3000,
  
  /** 连接已关闭 */
  CONNECTION_CLOSED = 3001,
  
  /** 连接超时 */
  CONNECTION_TIMEOUT = 3002,
  
  /** 连接数达到上限 */
  MAX_CONNECTIONS_REACHED = 3003,
  
  /** 心跳超时 */
  HEARTBEAT_TIMEOUT = 3004,
  
  // 4xxx - 消息错误
  /** 消息过大 */
  MESSAGE_TOO_LARGE = 4000,
  
  /** 消息发送失败 */
  SEND_FAILED = 4001,
  
  /** 消息确认超时 */
  ACK_TIMEOUT = 4002,
  
  /** 无效的消息类型 */
  INVALID_MESSAGE_TYPE = 4003,
  
  /** 消息队列已满 */
  QUEUE_FULL = 4004,
  
  // 5xxx - 频道/房间错误
  /** 频道不存在 */
  CHANNEL_NOT_FOUND = 5000,
  
  /** 房间不存在 */
  ROOM_NOT_FOUND = 5001,
  
  /** 已订阅 */
  ALREADY_SUBSCRIBED = 5002,
  
  /** 未订阅 */
  NOT_SUBSCRIBED = 5003,
  
  /** 房间已满 */
  ROOM_FULL = 5004,
  
  /** 已加入房间 */
  ALREADY_IN_ROOM = 5005,
  
  // 6xxx - 速率限制错误
  /** 请求过于频繁 */
  RATE_LIMIT_EXCEEDED = 6000,
  
  /** 消息发送速率超限 */
  MESSAGE_RATE_LIMIT = 6001,
}
```

**错误码说明表**:

| 代码 | 名称 | HTTP等价 | 说明 | 客户端处理建议 |
|------|------|----------|------|----------------|
| 1000 | UNKNOWN | 500 | 未知错误 | 提示用户稍后重试 |
| 1001 | INVALID_MESSAGE_FORMAT | 400 | 消息格式无效 | 检查消息格式 |
| 1002 | INVALID_PARAMETER | 400 | 参数无效 | 检查参数 |
| 1003 | INTERNAL_ERROR | 500 | 服务器内部错误 | 提示用户稍后重试 |
| 1004 | TIMEOUT | 408 | 超时 | 重试请求 |
| 2000 | UNAUTHORIZED | 401 | 未授权 | 跳转到登录页 |
| 2001 | AUTH_FAILED | 401 | 认证失败 | 提示用户重新登录 |
| 2002 | INVALID_TOKEN | 401 | Token无效 | 刷新Token |
| 2003 | TOKEN_EXPIRED | 401 | Token过期 | 刷新Token |
| 2004 | PERMISSION_DENIED | 403 | 权限不足 | 提示用户无权限 |
| 3000 | NOT_CONNECTED | - | 未连接 | 尝试重连 |
| 3001 | CONNECTION_CLOSED | - | 连接已关闭 | 尝试重连 |
| 3002 | CONNECTION_TIMEOUT | - | 连接超时 | 尝试重连 |
| 3003 | MAX_CONNECTIONS_REACHED | 503 | 连接数达到上限 | 提示稍后重试 |
| 3004 | HEARTBEAT_TIMEOUT | - | 心跳超时 | 尝试重连 |
| 4000 | MESSAGE_TOO_LARGE | 413 | 消息过大 | 减小消息大小 |
| 4001 | SEND_FAILED | - | 发送失败 | 重试发送 |
| 4002 | ACK_TIMEOUT | 408 | 确认超时 | 重试发送 |
| 5000 | CHANNEL_NOT_FOUND | 404 | 频道不存在 | 检查频道名称 |
| 5001 | ROOM_NOT_FOUND | 404 | 房间不存在 | 检查房间ID |
| 6000 | RATE_LIMIT_EXCEEDED | 429 | 请求过于频繁 | 降低请求频率 |

---

## ⚙️ 配置选项数据结构

### ServerConfig（服务端配置）

**TypeScript 定义**:
```typescript
interface ServerConfig {
  /** 监听端口（必需） */
  port: number;
  
  /** 监听主机（可选，默认 '0.0.0.0'） */
  host?: string;
  
  /** 心跳间隔（毫秒，可选，默认 30000） */
  heartbeatInterval?: number;
  
  /** 心跳超时（毫秒，可选，默认 35000） */
  heartbeatTimeout?: number;
  
  /** 最大连接数（可选，默认 10000） */
  maxConnections?: number;
  
  /** 最大消息大小（字节，可选，默认 1MB） */
  maxMessageSize?: number;
  
  /** 消息队列大小（可选，默认 1000） */
  messageQueueSize?: number;
  
  /** 是否启用压缩（可选，默认 false） */
  compression?: boolean;
  
  /** 速率限制配置（可选） */
  rateLimit?: {
    /** 时间窗口（毫秒） */
    window: number;
    
    /** 最大消息数 */
    maxMessages: number;
  };
  
  /** 认证配置（可选） */
  auth?: {
    /** 是否必需认证 */
    required: boolean;
    
    /** 认证超时（毫秒） */
    timeout?: number;
    
    /** 自定义认证处理器 */
    handler?: (data: AuthData) => Promise<AuthResult>;
  };
  
  /** TLS配置（可选） */
  tls?: {
    /** 证书 */
    cert: string;
    
    /** 私钥 */
    key: string;
    
    /** CA证书 */
    ca?: string;
  };
  
  /** 日志配置（可选） */
  logging?: {
    /** 日志级别 */
    level: 'debug' | 'info' | 'warn' | 'error';
    
    /** 是否记录消息内容 */
    logMessages?: boolean;
  };
  
  /** 自定义配置 */
  custom?: Record<string, any>;
}
```

**使用示例**:
```typescript
const config: ServerConfig = {
  port: 8080,
  host: '0.0.0.0',
  heartbeatInterval: 30000,
  heartbeatTimeout: 35000,
  maxConnections: 10000,
  maxMessageSize: 1024 * 1024, // 1MB
  compression: true,
  rateLimit: {
    window: 60000, // 1分钟
    maxMessages: 100,
  },
  auth: {
    required: true,
    timeout: 10000,
  },
  logging: {
    level: 'info',
    logMessages: false,
  },
};
```

---

### ClientConfig（客户端配置）

**TypeScript 定义**:
```typescript
interface ClientConfig {
  /** WebSocket URL（必需） */
  url: string;
  
  /** 协议列表（可选） */
  protocols?: string | string[];
  
  /** 是否自动重连（可选，默认 true） */
  autoReconnect?: boolean;
  
  /** 重连配置（可选） */
  reconnect?: {
    /** 最大重连次数（默认 5） */
    maxAttempts?: number;
    
    /** 初始延迟（毫秒，默认 1000） */
    initialDelay?: number;
    
    /** 最大延迟（毫秒，默认 30000） */
    maxDelay?: number;
    
    /** 重连策略（默认 'exponential'） */
    strategy?: 'linear' | 'exponential' | 'fixed';
    
    /** 延迟因子（指数策略时使用，默认 2） */
    factor?: number;
  };
  
  /** 心跳间隔（毫秒，可选，默认 30000） */
  heartbeatInterval?: number;
  
  /** 心跳超时（毫秒，可选，默认 5000） */
  heartbeatTimeout?: number;
  
  /** 连接超时（毫秒，可选，默认 10000） */
  connectionTimeout?: number;
  
  /** 最大消息队列大小（可选，默认 100） */
  maxQueueSize?: number;
  
  /** 是否启用消息队列（可选，默认 true） */
  enableQueue?: boolean;
  
  /** 认证配置（可选） */
  auth?: {
    /** 认证方式 */
    method: 'token' | 'credentials' | 'oauth' | 'custom';
    
    /** Token */
    token?: string;
    
    /** 用户名 */
    username?: string;
    
    /** 密码 */
    password?: string;
    
    /** 自动认证（连接后立即认证） */
    autoAuth?: boolean;
  };
  
  /** 日志配置（可选） */
  logging?: {
    level: 'debug' | 'info' | 'warn' | 'error';
    logMessages?: boolean;
  };
  
  /** 自定义配置 */
  custom?: Record<string, any>;
}
```

**使用示例**:
```typescript
const config: ClientConfig = {
  url: 'ws://localhost:8080',
  autoReconnect: true,
  reconnect: {
    maxAttempts: 5,
    initialDelay: 1000,
    maxDelay: 30000,
    strategy: 'exponential',
    factor: 2,
  },
  heartbeatInterval: 30000,
  connectionTimeout: 10000,
  auth: {
    method: 'token',
    token: 'eyJhbGciOiJIUzI1NiIs...',
    autoAuth: true,
  },
  logging: {
    level: 'info',
    logMessages: false,
  },
};
```

---

### ReconnectStrategy（重连策略）

**描述**: 定义重连的延迟计算方式

**策略说明**:

| 策略 | 值 | 延迟计算公式 | 示例（初始1s，因子2） |
|------|---------|--------------|---------------------|
| fixed | "fixed" | 固定延迟 | 1s, 1s, 1s, 1s, ... |
| linear | "linear" | delay * attempt | 1s, 2s, 3s, 4s, ... |
| exponential | "exponential" | delay * (factor ^ attempt) | 1s, 2s, 4s, 8s, ... |

**使用示例**:
```typescript
// 固定延迟（每次都等待1秒）
const fixedConfig: ClientConfig = {
  url: 'ws://localhost:8080',
  reconnect: {
    strategy: 'fixed',
    initialDelay: 1000,
  },
};

// 线性延迟（1秒、2秒、3秒...）
const linearConfig: ClientConfig = {
  url: 'ws://localhost:8080',
  reconnect: {
    strategy: 'linear',
    initialDelay: 1000,
  },
};

// 指数延迟（1秒、2秒、4秒、8秒...）
const exponentialConfig: ClientConfig = {
  url: 'ws://localhost:8080',
  reconnect: {
    strategy: 'exponential',
    initialDelay: 1000,
    factor: 2,
    maxDelay: 30000, // 最多等待30秒
  },
};
```

---

## 🔄 事件数据结构

### ServerEvent（服务端事件）

**TypeScript 定义**:
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

**事件参数表**:

| 事件 | 参数 | 参数类型 | 说明 |
|------|------|----------|------|
| STARTED | - | - | 服务器启动 |
| STOPPED | - | - | 服务器停止 |
| CONNECTION | connectionId, req | string, any | 新连接建立 |
| DISCONNECTION | connectionId, code, reason | string, number, string | 连接断开 |
| MESSAGE | connectionId, message | string, WSMessage | 收到消息 |
| MESSAGE_SENT | connectionId, message | string, WSMessage | 消息已发送 |
| ERROR | error | Error | 发生错误 |
| HEARTBEAT_TIMEOUT | connectionId | string | 心跳超时 |
| AUTHENTICATED | connectionId, authInfo | string, object | 认证成功 |
| AUTH_FAILED | connectionId, error | string, Error | 认证失败 |

---

### ClientEvent（客户端事件）

**TypeScript 定义**:
```typescript
enum ClientEvent {
  CONNECTING = 'connecting',
  CONNECTED = 'connected',
  DISCONNECTED = 'disconnected',
  RECONNECTING = 'reconnecting',
  RECONNECTED = 'reconnected',
  MESSAGE = 'message',
  ERROR = 'error',
  HEARTBEAT = 'heartbeat',
  AUTHENTICATED = 'authenticated',
}
```

**事件参数表**:

| 事件 | 参数 | 参数类型 | 说明 |
|------|------|----------|------|
| CONNECTING | - | - | 正在连接 |
| CONNECTED | connectionId | string | 连接成功 |
| DISCONNECTED | code, reason | number, string | 连接断开 |
| RECONNECTING | attempt, delay | number, number | 正在重连 |
| RECONNECTED | connectionId | string | 重连成功 |
| MESSAGE | message | WSMessage | 收到消息 |
| ERROR | error | Error | 发生错误 |
| HEARTBEAT | - | - | 心跳 |
| AUTHENTICATED | authInfo | object | 认证成功 |

---

## ✅ 契约验证清单

- [x] 所有数据结构都有明确的类型定义
- [x] 字段类型明确（必需/可选）
- [x] 提供 TypeScript 类型定义
- [x] 提供 JSON 示例
- [x] 提供使用示例
- [x] 枚举值明确定义
- [x] 错误码完整定义
- [x] 与 API 契约一致

---

## 📚 类型导出索引

**建议的导出结构**:

```typescript
// types/index.ts

// 消息相关
export * from './message';
export { WSMessage, MessageHeader, MessagePayload, MessageType } from './message';
export { SubscribeData, AuthData, JoinRoomData, AckData } from './message-data';

// 连接相关
export * from './connection';
export { ConnectionInfo, ConnectionState, ConnectionFilter } from './connection';

// 统计相关
export * from './stats';
export { ServerStats, BatchConnectionOperationResult } from './stats';

// 错误相关
export * from './error';
export { ErrorCode } from './error';

// 配置相关
export * from './config';
export { ServerConfig, ClientConfig, ReconnectStrategy } from './config';

// 事件相关
export * from './events';
export { ServerEvent, ClientEvent } from './events';
```

---

**契约状态**: ✅ 已完成  
**最后更新**: 2024  
**维护者**: Architecture Team
