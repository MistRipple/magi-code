# WebSocket 系统契约总览

**版本**: 1.0.0  
**状态**: 已发布  
**最后更新**: 2024  

---

## 📋 契约文件索引

### 核心契约文件

1. **API 接口契约** (`API_CONTRACT.md`)
   - **契约ID**: contract_1770010232546_5vf7w5pz3
   - **内容**: 定义所有 API 接口规范
   - **状态**: ✅ 已完成

2. **数据结构契约** (`DATA_STRUCTURE_CONTRACT.md`)
   - **契约ID**: contract_1770010232546_dzyfec7i4
   - **内容**: 定义所有数据结构规范
   - **状态**: ✅ 已完成

3. **集成示例** (`INTEGRATION_EXAMPLES.md`)
   - **内容**: 完整的集成使用示例
   - **状态**: ✅ 已完成

---

## 🎯 契约使用指南

### 对于消费方（集成者）

#### 1. 了解契约结构

本系统提供两个核心契约：

- **API 契约** - 告诉你"能做什么"（接口方法）
- **数据结构契约** - 告诉你"如何传递数据"（类型定义）

#### 2. 集成步骤

**步骤 1**: 阅读契约文档
```bash
# 了解 API 接口
cat contracts/API_CONTRACT.md

# 了解数据结构
cat contracts/DATA_STRUCTURE_CONTRACT.md

# 查看集成示例
cat contracts/INTEGRATION_EXAMPLES.md
```

**步骤 2**: 安装类型定义
```bash
# 如果是 TypeScript 项目
npm install --save-dev @types/websocket-system

# 或者直接复制类型文件
cp types/*.ts your-project/types/
```

**步骤 3**: 导入类型
```typescript
import {
  IWebSocketServer,
  IWebSocketClient,
  WSMessage,
  MessageType,
  ConnectionInfo,
  ErrorCode,
  ServerConfig,
  ClientConfig,
} from './types';
```

**步骤 4**: 使用 API
```typescript
// 服务端
const server: IWebSocketServer = createServer({
  port: 8080,
});

await server.start();

// 客户端
const client: IWebSocketClient = createClient({
  url: 'ws://localhost:8080',
});

await client.connect();
```

---

### 对于提供方（维护者）

#### 契约变更流程

1. **提议变更**
   - 在 GitHub Issue 中描述变更需求
   - 说明变更原因和影响范围

2. **版本控制**
   - 破坏性变更：主版本号 +1（如 1.0.0 → 2.0.0）
   - 新增功能：次版本号 +1（如 1.0.0 → 1.1.0）
   - Bug 修复：补丁版本号 +1（如 1.0.0 → 1.0.1）

3. **更新文档**
   - 更新契约文档
   - 更新 CHANGELOG.md
   - 更新集成示例

4. **通知消费方**
   - 发布变更公告
   - 提供迁移指南（如果有破坏性变更）

---

## 📊 契约兼容性矩阵

### API 契约版本兼容性

| API 版本 | 数据结构版本 | 兼容性 | 说明 |
|---------|------------|--------|------|
| 1.0.0 | 1.0.0 | ✅ 完全兼容 | 当前版本 |
| 1.1.0 | 1.0.0 | ✅ 向后兼容 | 新增 API，不影响现有代码 |
| 2.0.0 | 2.0.0 | ❌ 破坏性变更 | 需要迁移 |

### 协议版本兼容性

| 协议版本 | 服务端版本 | 客户端版本 | 兼容性 |
|---------|-----------|-----------|--------|
| 1.0.0 | 1.x.x | 1.x.x | ✅ 完全兼容 |
| 1.0.0 | 2.x.x | 1.x.x | ⚠️ 功能受限 |
| 2.0.0 | 2.x.x | 2.x.x | ✅ 完全兼容 |

---

## 🔍 快速查找

### 常见场景快速索引

#### 场景 1: 创建服务端

**相关契约**:
- API 契约 → `IWebSocketServer` 接口
- 数据结构契约 → `ServerConfig` 类型

**示例**:
```typescript
import { IWebSocketServer, ServerConfig } from './types';
import { createServer } from './server';

const config: ServerConfig = {
  port: 8080,
  heartbeatInterval: 30000,
};

const server: IWebSocketServer = createServer(config);
await server.start();
```

---

#### 场景 2: 发送消息

**相关契约**:
- API 契约 → `sendToClient()` 方法
- 数据结构契约 → `WSMessage` 类型

**示例**:
```typescript
const sent = await server.sendToClient('conn_123', {
  header: {
    id: 'msg_001',
    type: MessageType.MESSAGE,
    timestamp: Date.now(),
  },
  payload: {
    data: { text: 'Hello!' },
  },
});
```

---

#### 场景 3: 广播消息

**相关契约**:
- API 契约 → `broadcast()` 方法
- 数据结构契约 → `BroadcastOptions`, `BatchConnectionOperationResult`

**示例**:
```typescript
const result = await server.broadcast({
  type: 'notification',
  message: '系统通知',
}, {
  excludeIds: ['conn_123'],
});

console.log(`成功: ${result.successCount}`);
```

---

#### 场景 4: 订阅频道

**相关契约**:
- API 契约 → `ChannelManager.subscribe()` 方法
- 数据结构契约 → `SubscribeData` 类型

**示例**:
```typescript
// 服务端
await server.channels.subscribe('conn_123', 'tech-news');

// 客户端
await client.subscribe(['tech-news', 'sports'], {
  includeHistory: true,
  historyLimit: 10,
});
```

---

#### 场景 5: 认证

**相关契约**:
- API 契约 → 认证事件
- 数据结构契约 → `AuthData` 类型

**示例**:
```typescript
// 客户端发送认证消息
await client.send({
  method: 'token',
  token: 'eyJhbGciOiJIUzI1NiIs...',
}, {
  type: MessageType.AUTHENTICATE,
});

// 服务端处理认证
server.on(ServerEvent.AUTHENTICATED, (connId, authInfo) => {
  console.log('用户认证成功:', authInfo);
});
```

---

#### 场景 6: 错误处理

**相关契约**:
- API 契约 → 错误处理方法
- 数据结构契约 → `ErrorCode` 枚举

**示例**:
```typescript
// 服务端返回错误
server.router.registerHandler(MessageType.MESSAGE, async (connId, msg, ctx) => {
  if (!ctx.connection.authenticated) {
    return ctx.error(ErrorCode.UNAUTHORIZED, '请先认证');
  }
});

// 客户端处理错误
client.on(ClientEvent.MESSAGE, (message: WSMessage) => {
  if (message.payload.error) {
    const { code, message: errorMsg } = message.payload.error;
    console.error(`错误 ${code}: ${errorMsg}`);
  }
});
```

---

## 📝 契约变更日志

### v1.0.0 (2024-01-01)

**初始版本**

- ✅ 定义核心 API 接口
- ✅ 定义消息协议格式
- ✅ 定义连接管理数据结构
- ✅ 定义错误码体系
- ✅ 提供完整的集成示例

---

## 🤝 集成支持

### 遇到问题？

1. **查看集成示例**: `contracts/INTEGRATION_EXAMPLES.md`
2. **查看 FAQ**: `docs/FAQ.md`
3. **提交 Issue**: GitHub Issues
4. **联系维护者**: [contact info]

### 贡献契约改进

我们欢迎对契约的改进建议：

1. Fork 项目
2. 创建特性分支 (`git checkout -b feature/contract-improvement`)
3. 提交变更 (`git commit -am 'Improve contract docs'`)
4. 推送分支 (`git push origin feature/contract-improvement`)
5. 创建 Pull Request

---

## 📚 延伸阅读

- [架构设计文档](../docs/ARCHITECTURE.md)
- [类型定义](../types/README.md)
- [服务端实现指南](../docs/SERVER_IMPLEMENTATION.md)
- [客户端实现指南](../docs/CLIENT_IMPLEMENTATION.md)
- [性能优化指南](../docs/PERFORMANCE.md)
- [安全最佳实践](../docs/SECURITY.md)

---

**契约维护团队**: Architecture Team  
**联系方式**: architecture-team@example.com  
**最后更新**: 2024
