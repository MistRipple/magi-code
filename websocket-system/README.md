# WebSocket 消息推送系统

完整的 WebSocket 实时消息推送系统，支持服务端和客户端实现。

## 快速开始

### 1. 安装依赖
```bash
cd websocket-system
npm install
```

### 2. 启动服务端
```bash
npm start
```
服务器将在 `ws://localhost:8080` 启动

### 3. 运行演示
```bash
npm run demo
```
自动演示完整的消息推送流程

### 4. 浏览器客户端
在浏览器中打开 `client/index.html`，即可使用可视化界面连接服务器

## 功能特性

### 服务端
- ✅ WebSocket 服务启动（基于 ws 库）
- ✅ 连接池管理（增删查、状态追踪）
- ✅ 心跳检测（30秒间隔，35秒超时）
- ✅ 单播消息（发送给指定客户端）
- ✅ 广播消息（发送给所有客户端）
- ✅ 错误处理（连接异常、消息解析失败）
- ✅ 断线清理（自动移除超时连接）

### 客户端
- ✅ 连接建立（原生 WebSocket API）
- ✅ 消息收发（JSON 格式）
- ✅ 自动重连（指数退避，最多 5 次）
- ✅ 事件回调（连接、消息、关闭、错误）
- ✅ 心跳保活（25秒间隔）

## 目录结构

```
websocket-system/
├── package.json              # 项目配置
├── server/                   # 服务端
│   ├── connection-manager.js # 连接池管理器
│   ├── websocket-server.js   # WebSocket 核心服务
│   └── index.js             # 服务端启动入口
├── client/                   # 客户端
│   ├── websocket-client.js   # 客户端封装类
│   └── index.html           # 浏览器演示页面
└── demo/                     # 演示
    └── app.js               # 完整演示程序
```

## 核心 API

### 服务端 API

```javascript
import WebSocketServerCore from './server/websocket-server.js';

const server = new WebSocketServerCore({ port: 8080 });

// 启动服务
await server.start();

// 注册事件
server.onConnection((connectionId, req) => {});
server.onMessage((connectionId, message) => {});

// 发送消息
server.sendToClient(connectionId, data);  // 单播
server.broadcast(data, excludeIds);       // 广播

// 获取统计
server.getStats();

// 关闭服务
await server.close();
```

### 客户端 API

```javascript
const client = new WebSocketClient('ws://localhost:8080');

// 连接
client.connect();

// 注册事件
client.onOpen(() => {});
client.onMessage((message) => {});

// 发送消息
client.send({ type: 'chat', content: 'Hello' });
client.sendChat('Hello');  // 快捷方法

// 关闭连接
client.close();
```

## 消息协议

### 系统消息
```json
{
  "type": "connected",
  "connectionId": "conn_1_1234567890",
  "message": "连接成功",
  "timestamp": 1234567890
}
```

### 聊天消息
```json
{
  "type": "chat",
  "from": "conn_1_1234567890",
  "content": "消息内容",
  "timestamp": 1234567890
}
```

### 心跳消息
```json
// 客户端发送
{ "type": "ping" }

// 服务端响应
{ "type": "pong", "timestamp": 1234567890 }
```

## 注意事项

1. **心跳机制**：客户端 25 秒发送一次 ping，服务端 30 秒检测一次，35 秒超时断开
2. **重连策略**：指数退避算法，延迟为 `3000 * 1.5^(尝试次数-1)` 毫秒
3. **并发处理**：使用 Map 数据结构管理连接池，支持高并发
4. **错误恢复**：所有网络错误和解析错误都有对应的处理逻辑

## 技术栈

- **服务端**：Node.js + ws (WebSocket 库)
- **客户端**：原生 WebSocket API
- **协议**：JSON over WebSocket
