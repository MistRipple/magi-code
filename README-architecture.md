# WebSocket 实时消息推送系统 - 架构设计交付物

## 📦 交付文件清单

本次架构设计共交付以下文件：

1. **websocket-architecture-design.md** - 完整的架构设计文档
2. **websocket-types.ts** - TypeScript 核心接口和类型定义
3. **README-architecture.md** - 本文件（使用指南）

---

## 🎯 设计概览

### 核心设计理念

- **模块化**: 清晰的模块划分，职责单一
- **接口契约**: 严格的接口定义，便于团队协作
- **可扩展**: 预留扩展点，支持水平扩展
- **可测试**: 依赖注入，便于单元测试和 Mock

### 系统架构

```
客户端 (Browser/Native)
        ↓ WebSocket
服务端架构：
┌─────────────────────────┐
│  消息协议层 (Protocol)   │ ← 消息解析、验证、序列化
├─────────────────────────┤
│  核心服务层 (Services)   │
│  ├─ ConnectionManager   │ ← 连接管理、心跳检测
│  ├─ MessageRouter       │ ← 消息路由、分发
│  ├─ RoomManager         │ ← 房间管理、订阅
│  └─ EventDispatcher     │ ← 事件监听、触发
├─────────────────────────┤
│  存储层 (Storage)       │ ← 内存 Map/Set（可扩展至 Redis）
└─────────────────────────┘
```

---

## 📋 核心模块职责

### 1. ConnectionManager (连接管理器)
**职责**: 管理 WebSocket 连接生命周期、心跳检测

**核心方法**:
- `registerConnection()` - 注册新连接
- `unregisterConnection()` - 注销连接
- `getConnection()` - 获取连接实例
- `getConnectionsByUserId()` - 获取用户的所有连接
- `startHeartbeat()` - 启动心跳检测

**依赖**: EventDispatcher

---

### 2. MessageRouter (消息路由器)
**职责**: 根据消息类型路由到处理器，实现广播/点对点/房间消息

**核心方法**:
- `route()` - 路由消息
- `broadcast()` - 广播给所有连接
- `sendToUser()` - 发送给指定用户
- `sendToRoom()` - 发送给房间
- `registerHandler()` - 注册消息处理器

**依赖**: ConnectionManager, RoomManager, EventDispatcher

---

### 3. RoomManager (房间管理器)
**职责**: 管理房间的创建、销毁和成员订阅

**核心方法**:
- `createRoom()` - 创建房间
- `deleteRoom()` - 删除房间
- `joinRoom()` - 加入房间
- `leaveRoom()` - 离开房间
- `getRoomMembers()` - 获取房间成员

**依赖**: ConnectionManager, EventDispatcher

---

### 4. EventDispatcher (事件分发器)
**职责**: 提供事件监听和触发机制，解耦模块间通信

**核心方法**:
- `on()` - 注册事件监听
- `off()` - 注销事件监听
- `emit()` - 触发事件

**依赖**: 无（基础模块）

---

## 📡 消息协议

### 消息基础结构

所有消息均为 JSON 格式，包含以下核心字段：

```typescript
{
  id: string;           // 消息唯一ID
  type: MessageType;    // 消息类型
  timestamp: number;    // 时间戳
  payload: any;         // 消息负载
  version?: string;     // 协议版本
  metadata?: object;    // 元数据
}
```

### 客户端消息类型 (ClientMessageType)

| 类型 | 说明 | Payload |
|-----|------|---------|
| `ping` | 心跳 Ping | 空或 echo 数据 |
| `auth` | 身份认证 | `{ token, userId }` |
| `join_room` | 加入房间 | `{ roomId, password? }` |
| `leave_room` | 离开房间 | `{ roomId }` |
| `create_room` | 创建房间 | `{ roomId, name, ... }` |
| `send_message` | 发送消息 | `{ targetType, targetId, content }` |
| `get_room_members` | 查询房间成员 | `{ roomId }` |
| `get_user_rooms` | 查询用户房间 | `{ userId? }` |

### 服务端消息类型 (ServerMessageType)

| 类型 | 说明 | Payload |
|-----|------|---------|
| `pong` | 心跳 Pong | `{ timestamp }` |
| `auth_success` | 认证成功 | `{ userId, connectionId }` |
| `auth_failure` | 认证失败 | `{ reason, code }` |
| `room_joined` | 已加入房间 | `{ roomId, members }` |
| `user_joined` | 用户加入（广播） | `{ roomId, userId }` |
| `message` | 接收消息 | `{ fromUserId, content, ... }` |
| `error` | 错误消息 | `{ code, message }` |
| `notification` | 系统通知 | `{ level, message }` |

---

## 🔄 关键流程

### 1. 连接建立流程

```
1. 客户端发起 WebSocket 连接
2. 服务端 ConnectionManager 注册连接
3. 客户端发送 AUTH 消息进行认证
4. 服务端验证并返回 AUTH_SUCCESS
5. 客户端开始发送心跳 PING
```

### 2. 房间订阅流程

```
1. 客户端发送 JOIN_ROOM 消息
2. 服务端 RoomManager 添加成员
3. 返回 ROOM_JOINED（包含成员列表）
4. 向房间内其他成员广播 USER_JOINED
```

### 3. 消息发送流程

```
1. 客户端发送 SEND_MESSAGE
2. MessageRouter 根据 targetType 路由
3. 如果是 room，获取房间成员并群发
4. 如果是 user，获取用户连接并发送
```

### 4. 心跳检测流程

```
1. 客户端每 30s 发送 PING
2. 服务端返回 PONG 并更新 lastHeartbeat
3. 服务端定时检查 lastHeartbeat
4. 超过 60s 无心跳则关闭连接
```

---

## 🛠️ 实现指南

### 给 Codex 团队（服务端实现）

#### 实现优先级

**Phase 1: 基础设施（必须）**
- [ ] `EventDispatcher` - 事件系统（无依赖，先实现）
- [ ] `ConnectionManager` - 连接管理
- [ ] `MessageRouter` - 基础路由（支持广播和点对点）
- [ ] 消息解析和验证 (`message-parser.ts`)
- [ ] 基础错误处理和日志

**Phase 2: 房间功能（核心）**
- [ ] `RoomManager` - 房间管理
- [ ] 房间消息处理器 (`room-handler.ts`)
- [ ] 房间订阅/退订逻辑

**Phase 3: 增强功能（优化）**
- [ ] 认证机制 (`auth-handler.ts`)
- [ ] 心跳检测优化
- [ ] 错误处理增强

**Phase 4: 生产就绪（测试）**
- [ ] 单元测试（每个模块）
- [ ] 集成测试（端到端流程）
- [ ] 性能测试（并发连接）

#### 实现建议

1. **严格遵循接口契约**: 所有类必须实现 `websocket-types.ts` 中定义的接口
2. **依赖注入**: 使用构造函数注入依赖，便于测试
3. **类型安全**: 充分利用 TypeScript 类型系统，避免 `any`
4. **错误处理**: 使用 `try-catch`，抛出有意义的错误
5. **日志记录**: 关键操作记录日志（连接、断开、错误）

#### 示例代码框架

```typescript
import { IConnectionManager, ConnectionMetadata } from './websocket-types';

export class ConnectionManager implements IConnectionManager {
  private connections: Map<string, WebSocket>;
  private metadata: Map<string, ConnectionMetadata>;
  private userConnections: Map<string, Set<string>>;
  
  constructor(
    private eventDispatcher: IEventDispatcher,
    private logger: Logger
  ) {
    this.connections = new Map();
    this.metadata = new Map();
    this.userConnections = new Map();
  }
  
  registerConnection(connectionId: string, ws: WebSocket, userId?: string): void {
    // 实现逻辑...
    this.connections.set(connectionId, ws);
    this.logger.info(`Connection registered: ${connectionId}`);
    this.eventDispatcher.emit(SystemEventType.CONNECTION_OPENED, { connectionId, userId });
  }
  
  // ... 其他方法实现
}
```

---

### 给 Gemini 团队（客户端实现）

#### 实现优先级

**Phase 1: 基础连接（必须）**
- [ ] `WebSocketClient` 类基础结构
- [ ] `connect()` - 建立连接
- [ ] `disconnect()` - 断开连接
- [ ] `send()` - 发送消息
- [ ] `on()/off()` - 事件监听

**Phase 2: 消息处理（核心）**
- [ ] 消息解析和分发
- [ ] 便捷方法实现（`auth()`, `joinRoom()`, `sendMessage()`）
- [ ] 错误处理和回调

**Phase 3: 可靠性（优化）**
- [ ] 自动重连逻辑
- [ ] 心跳发送
- [ ] 请求超时处理
- [ ] 状态管理

**Phase 4: 用户体验（增强）**
- [ ] 示例页面 (`index.html`)
- [ ] 演示代码 (`demo.js`)
- [ ] 使用文档

#### 实现建议

1. **用户友好**: 提供简洁的 API，隐藏复杂性
2. **健壮性**: 处理所有异常情况（网络中断、服务器错误）
3. **自动化**: 自动重连、自动心跳
4. **调试友好**: 提供详细的日志和错误信息

#### 示例代码框架

```javascript
class WebSocketClient {
  constructor() {
    this.ws = null;
    this.status = {
      isConnected: false,
      reconnectAttempts: 0
    };
    this.eventHandlers = new Map();
    this.pendingRequests = new Map();
  }
  
  async connect(url, options = {}) {
    this.options = { autoReconnect: true, ...options };
    
    return new Promise((resolve, reject) => {
      this.ws = new WebSocket(url);
      
      this.ws.onopen = () => {
        this.status.isConnected = true;
        this.emit('open');
        this.startHeartbeat();
        resolve();
      };
      
      this.ws.onmessage = (event) => {
        const message = JSON.parse(event.data);
        this.handleMessage(message);
      };
      
      this.ws.onerror = (error) => {
        this.emit('error', error);
        reject(error);
      };
      
      this.ws.onclose = () => {
        this.status.isConnected = false;
        this.emit('close');
        if (this.options.autoReconnect) {
          this.reconnect();
        }
      };
    });
  }
  
  // ... 其他方法实现
}
```

---

## 🧪 测试指南

### 单元测试示例

```typescript
// connection-manager.test.ts
import { ConnectionManager } from './connection-manager';
import { EventDispatcher } from './event-dispatcher';

describe('ConnectionManager', () => {
  let connectionManager: ConnectionManager;
  let eventDispatcher: EventDispatcher;
  
  beforeEach(() => {
    eventDispatcher = new EventDispatcher();
    connectionManager = new ConnectionManager(eventDispatcher);
  });
  
  test('should register connection', () => {
    const ws = {} as WebSocket;
    connectionManager.registerConnection('conn-1', ws, 'user-1');
    
    expect(connectionManager.getConnection('conn-1')).toBe(ws);
    expect(connectionManager.getConnectionsByUserId('user-1')).toContain('conn-1');
  });
  
  test('should unregister connection', () => {
    const ws = {} as WebSocket;
    connectionManager.registerConnection('conn-1', ws);
    connectionManager.unregisterConnection('conn-1');
    
    expect(connectionManager.getConnection('conn-1')).toBeUndefined();
  });
});
```

### 集成测试示例

```typescript
// integration.test.ts
import { WebSocketServer } from './server';

describe('WebSocket Integration Tests', () => {
  let server: WebSocketServer;
  let client: WebSocket;
  
  beforeAll(async () => {
    server = new WebSocketServer();
    await server.start(8080);
  });
  
  afterAll(async () => {
    await server.stop();
  });
  
  test('should connect and authenticate', async () => {
    client = new WebSocket('ws://localhost:8080');
    
    await new Promise(resolve => client.onopen = resolve);
    
    client.send(JSON.stringify({
      id: 'msg-1',
      type: 'auth',
      timestamp: Date.now(),
      payload: { userId: 'user-1', token: 'valid-token' }
    }));
    
    const response = await new Promise(resolve => {
      client.onmessage = (event) => resolve(JSON.parse(event.data));
    });
    
    expect(response.type).toBe('auth_success');
  });
});
```

---

## 📊 性能基准

### 目标指标

| 指标 | 目标值 | 说明 |
|-----|-------|------|
| 并发连接数 | ≥ 10,000 | 单服务器支持的最大连接数 |
| 消息延迟 | < 50ms | 端到端消息传输延迟（P95） |
| 消息吞吐量 | ≥ 100,000/s | 每秒处理的消息数 |
| 内存占用 | < 100MB | 10,000 连接时的内存占用 |
| CPU 占用 | < 50% | 正常负载下的 CPU 使用率 |

### 性能测试工具

- **Artillery**: WebSocket 负载测试
- **autocannon**: HTTP/WebSocket 基准测试
- **k6**: 现代化负载测试工具

---

## 🔧 开发环境设置

### 服务端（Node.js + TypeScript）

```bash
# 初始化项目
npm init -y

# 安装依赖
npm install ws
npm install --save-dev typescript @types/node @types/ws

# 安装测试工具
npm install --save-dev jest @types/jest ts-jest

# 创建 tsconfig.json
npx tsc --init
```

### 客户端（原生 JavaScript）

```html
<!DOCTYPE html>
<html>
<head>
  <title>WebSocket Client Demo</title>
</head>
<body>
  <script src="websocket-client.js"></script>
  <script src="demo.js"></script>
</body>
</html>
```

---

## 📚 参考资源

### WebSocket 相关
- [WebSocket 协议规范 (RFC 6455)](https://tools.ietf.org/html/rfc6455)
- [ws 库文档](https://github.com/websockets/ws)
- [MDN WebSocket API](https://developer.mozilla.org/en-US/docs/Web/API/WebSocket)

### 架构设计
- [微服务架构模式](https://microservices.io/patterns/index.html)
- [领域驱动设计 (DDD)](https://martinfowler.com/bliki/DomainDrivenDesign.html)
- [SOLID 原则](https://en.wikipedia.org/wiki/SOLID)

---

## 🤝 团队协作

### 分工建议

| 团队 | 负责模块 | 交付物 |
|-----|---------|--------|
| **Codex** | 服务端实现 | `src/` 目录下所有 TypeScript 文件 |
| **Gemini** | 客户端实现 | `client/` 目录下所有 JavaScript/HTML 文件 |
| **Architect** | 接口契约维护 | `websocket-types.ts` 更新和版本管理 |

### 协作流程

1. **接口冻结**: 在开始实现前，团队共同评审并冻结接口契约
2. **并行开发**: 基于冻结的接口，服务端和客户端团队并行开发
3. **集成测试**: 每周进行一次集成测试，验证互操作性
4. **问题反馈**: 发现接口问题时，提交 Issue 给架构师评审
5. **版本管理**: 使用语义化版本号管理协议版本

### 沟通渠道

- **接口变更**: 通过 PR 提交，必须经过架构师评审
- **实现问题**: 通过 Issue 跟踪，标记优先级
- **技术讨论**: 通过周会或设计评审会议

---

## ✅ 验收标准

### 功能验收

- [x] 支持 WebSocket 连接建立和断开
- [x] 支持广播消息（一对多）
- [x] 支持点对点消息（一对一）
- [x] 支持房间/频道订阅机制
- [x] 支持心跳检测
- [x] 支持客户端自动重连
- [x] 符合消息协议定义

### 质量验收

- [ ] 单元测试覆盖率 ≥ 80%
- [ ] 集成测试通过率 100%
- [ ] 性能指标达标（见性能基准）
- [ ] 无严重内存泄漏
- [ ] 代码符合 ESLint/Prettier 规范

### 文档验收

- [ ] API 文档完整（JSDoc/TSDoc）
- [ ] 示例代码可运行
- [ ] README 使用指南清晰
- [ ] 部署文档完整

---

## 📅 时间规划建议

### 两周冲刺（Sprint）

| 周次 | 阶段 | 交付物 |
|-----|------|--------|
| Week 1 | Phase 1-2 实现 | 基础功能 + 房间功能 |
| Week 2 | Phase 3-4 实现 | 增强功能 + 测试完善 |

### 详细时间线

```
Day 1-2:   环境搭建、基础架构实现
Day 3-4:   ConnectionManager + MessageRouter
Day 5-6:   RoomManager + 消息处理器
Day 7-8:   客户端基础实现
Day 9-10:  集成测试 + Bug 修复
Day 11-12: 性能优化 + 文档完善
Day 13-14: 最终测试 + 交付准备
```

---

## 🎯 下一步行动

### 架构师（当前）
- [x] 完成架构设计文档
- [x] 完成接口契约定义
- [x] 交付给实现团队

### Codex 团队（服务端）
- [ ] 评审架构设计文档
- [ ] 确认接口契约
- [ ] 搭建开发环境
- [ ] 开始 Phase 1 实现

### Gemini 团队（客户端）
- [ ] 评审架构设计文档
- [ ] 确认接口契约
- [ ] 搭建开发环境
- [ ] 开始客户端基础实现

---

## 📞 联系方式

如有任何疑问或建议，请通过以下方式联系架构师：

- **架构师**: Antigravity (Google Deepmind)
- **文档版本**: v1.0.0
- **最后更新**: 2024

---

**祝开发顺利！** 🚀
