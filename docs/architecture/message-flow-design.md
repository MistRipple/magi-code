# 消息链路架构设计 v1.0

## 分层架构总览

```
┌─────────────────────────────────────────────────────────────────┐
│                    后端 (Extension Host)                         │
├─────────────────────────────────────────────────────────────────┤
│  L1 生产层    Normalizer → Adapter                              │
│  L2 中枢层    MessageHub                                        │
│  L3 桥接层    WebviewProvider                                   │
├─────────────────────────────────────────────────────────────────┤
│                    ↓ postMessage (跨进程边界) ↓                  │
├─────────────────────────────────────────────────────────────────┤
│                    前端 (Webview)                                │
├─────────────────────────────────────────────────────────────────┤
│  L4 接收层    vscode-bridge                                     │
│  L5 路由层    message-handler + message-router                  │
│  L6 渲染层    Svelte Store + UI Components                      │
└─────────────────────────────────────────────────────────────────┘
```

### 层级定义（6 层）

| 层级 | 名称 | 位置 | 核心组件 | 唯一职责 |
| ---- | ---- | ---- | -------- | -------- |
| L1 | 生产层 | 后端 | Normalizer, Adapter | 生产 StandardMessage |
| L2 | 中枢层 | 后端 | MessageHub | 去重、节流、生命周期、事件发射 |
| L3 | 桥接层 | 后端 | WebviewProvider | 监听 Hub 事件，postMessage 到前端 |
| L4 | 接收层 | 前端 | vscode-bridge | 接收 postMessage，分发给 listeners |
| L5 | 路由层 | 前端 | message-handler, message-router | 按类别/可见性决定消息去向 |
| L6 | 渲染层 | 前端 | Store, Components | 状态管理，UI 渲染 |

### 层间边界（5 个接口）

| 边界 | 上层 | 下层 | 接口方式 |
| ---- | ---- | ---- | -------- |
| L1→L2 | Adapter | MessageHub | `messageHub.sendMessage(msg)` |
| L2→L3 | MessageHub | WebviewProvider | `emit('unified:message', msg)` |
| L3→L4 | WebviewProvider | vscode-bridge | `postMessage({ type, message })` |
| L4→L5 | vscode-bridge | message-handler | `listener(message)` 回调 |
| L5→L6 | message-handler | Store | `addThreadMessage(msg)` 等 |

---

## 各层详细定义

### L1 生产层（后端）

**组件**：
- `src/normalizer/*.ts` - 各 LLM 的响应解析器
- `src/llm/adapters/*.ts` - 各 LLM 的适配器

**职责**：
- 接收 LLM API 原始响应
- 解析为统一的 StandardMessage 格式
- 转发到 MessageHub

**代码路径**：
```
LLM API Response
    ↓
Normalizer.processChunk()
    ↓ emit(MESSAGE_EVENTS.MESSAGE)
Adapter.setupNormalizerEvents()
    ↓ messageHub.sendMessage(message)
进入 L2
```

**禁止行为**：不做任何过滤或丢弃

---

### L2 中枢层（后端）

**组件**：
- `src/orchestrator/core/message-hub.ts`

**职责**：
- 消息去重（同 ID + lifecycle 只发一次）
- 流式节流（100ms 间隔）
- 生命周期管理（started → streaming → completed）
- 事件发射（unified:message/update/complete）

**代码路径**：
```
messageHub.sendMessage(message)
    ↓
去重检查 → 节流检查 → 生命周期更新
    ↓ emit('unified:message', message)
进入 L3
```

**禁止行为**：不做内容过滤（只做协议层保护）

---

### L3 桥接层（后端）

**组件**：
- `src/ui/webview-provider.ts` 的 `setupMessageHubListeners()`

**职责**：
- 监听 MessageHub 事件
- 封装为 postMessage 格式
- 发送到前端 Webview

**代码路径**：
```
messageHub.on('unified:message', (message) => {
    ↓
this.postMessage({ type: 'unifiedMessage', message })
    ↓ 跨进程
进入 L4
```

**禁止行为**：不做任何过滤或转换

---

### L4 接收层（前端）

**组件**：
- `src/ui/webview-svelte/src/lib/vscode-bridge.ts`

**职责**：
- 监听 window message 事件
- 分发给注册的 listeners

**代码路径**：
```
window.addEventListener('message', (event) => {
    ↓
listeners.forEach(listener => listener(message))
    ↓
进入 L5
```

**禁止行为**：不做任何过滤或转换

---

### L5 路由层（前端）

**组件**：
- `src/ui/webview-svelte/src/lib/message-handler.ts`
- `src/ui/webview-svelte/src/lib/message-router.ts`

**职责**：
- 按消息类别分发（CONTENT/CONTROL/NOTIFY/DATA）
- 按可见性决定去向（user→thread/worker, system→none）
- 建立路由表（messageId → DisplayTarget）

**代码路径**：
```
handleMessage(message)
    ↓ switch(type)
handleUnifiedMessage() → handleContentMessage()
    ↓
routeStandardMessage() → 建立路由
    ↓
addThreadMessage() / addAgentMessage()
    ↓
进入 L6
```

**关键决策点**：所有「是否展示」的决策在此层完成

---

### L6 渲染层（前端）

**组件**：
- `src/ui/webview-svelte/src/stores/messages.svelte.ts`
- `src/ui/webview-svelte/src/components/*.svelte`

**职责**：
- 状态管理（响应式 Store）
- UI 渲染（消息列表、Worker 面板等）
- 空内容处理（显示加载态）

**禁止行为**：不做消息过滤（只做渲染决策）

---

## 现有过滤节点与层级归属

| # | 过滤节点 | 代码位置 | 当前层级 | 行为 | 状态 |
| ---- | ---- | ---- | ---- | ---- | ---- |
| 1 | `streamToUI=false` | base-adapter.ts:186,193,206 | L1 生产层 | 消息完全不发送 | 🔴 问题 |
| 2 | MessageHub 去重 | message-hub.ts:739-754 | L2 中枢层 | 同 ID 消息只发一次 | ✅ 正常 |
| 3 | MessageHub 节流 | message-hub.ts:1059-1103 | L2 中枢层 | 流式更新间隔 100ms | ✅ 正常 |
| 4 | 消息类别分发 | message-handler.ts:319-335 | L5 路由层 | 按 category 分发 | ✅ 正常 |
| 5 | `hasRenderableContent` | message-handler.ts:527,584 | L5 路由层 | 无内容消息被丢弃 | 🔴 问题 |
| 6 | 路由表缺失保护 | message-handler.ts:690-693 | L5 路由层 | update 暂存到队列 | ✅ 正常 |
| 7 | Complete 类别过滤 | message-handler.ts:711-713 | L5 路由层 | 非 CONTENT 跳过 | ✅ 正常 |

### 问题节点分析

**#1 streamToUI（L1 生产层）**：消息在最上游被拦截，L2-L6 完全无感知，链路断裂。

**#5 hasRenderableContent（L5 路由层）**：消息到达前端后被静默丢弃，无日志无追踪。

---

## 长期方案：过滤职责归一到 L5

| 原节点 | 新设计 | 归属层级 |
| ---- | ---- | ---- |
| `streamToUI=false` | 改为 `visibility: 'system'`，消息正常流经全链路 | L5 路由层决策 |
| `hasRenderableContent` | 移除，空内容消息由 L6 渲染层决定如何展示 | L6 渲染层 |
| MessageHub 去重/节流 | 保留，协议层保护 | L2 中枢层 |
| 消息类别分发 | 保留，正常分流 | L5 路由层 |
| Complete 类别过滤 | 保留，业务正确性 | L5 路由层 |

---

## 消息可见性控制（新设计）

### 问题：如何处理「不应展示给用户」的内部调用？

**旧方案（废弃）**：`streamToUI: false` 静默模式

- 问题：消息链路断裂，无法追踪

**新方案**：`visibility` 字段 + L5 路由层决策

```typescript
interface StandardMessage {
  // ... 现有字段

  /**
   * 消息可见性
   * - 'user': 用户可见（默认）
   * - 'system': 仅系统日志可见
   * - 'debug': 仅调试模式可见
   */
  visibility: 'user' | 'system' | 'debug';
}
```

**处理流程**：

1. 后端设置 `visibility` 字段
2. 消息正常流经 L1 → L2 → L3 → L4 → L5
3. L5 路由层根据 `visibility` 决定去向：
   - `user`: 正常路由到 thread/worker
   - `system`: 路由到 `location: 'none'`（系统日志区）
   - `debug`: 仅在调试模式下显示

---

## 迁移计划

### Phase 1: 引入 visibility 字段（向后兼容）

1. 在 `message-protocol.ts` 中添加 `visibility` 字段（默认 `'user'`）
2. 更新 `MessageHub.createMessage()` 支持 `visibility` 参数
3. L5 路由层根据 `visibility` 决定去向

### Phase 2: 迁移内部调用

1. 将 `streamToUI: false` 调用改为 `visibility: 'system'`
2. 保留消息完整流经链路，仅改变前端展示

### Phase 3: 移除 streamToUI

1. 移除 `BaseAdapter._streamToUI` 字段
2. 移除 `AdapterFactory.sendMessage()` 中的 `streamToUI` 处理
3. 移除 `AdapterOutputScope.streamToUI` 选项

### Phase 4: 清理前端过滤逻辑

1. 移除 `hasRenderableContent` 过滤
2. 空内容消息由 L6 UI 组件处理（显示加载态或占位符）

---

## 调试与可观测性

### 消息追踪 ID

每条消息携带完整追踪信息：

```typescript
interface MessageTracing {
  traceId: string;      // 请求级追踪 ID
  messageId: string;    // 消息唯一 ID
  parentId?: string;    // 父消息 ID（用于消息链）
  source: string;       // 来源组件
  timestamp: number;    // 创建时间
}
```

### 链路日志

每个节点在处理消息时记录日志：

```typescript
// L1 生产层
logger.debug('Adapter.send', { messageId, lifecycle });

// L2 中枢层
logger.debug('MessageHub.send', { messageId, lifecycle, visibility });

// L3 桥接层
logger.debug('WebviewProvider.post', { messageId, type });

// L4 接收层
console.log('[vscode-bridge]', messageId, type);

// L5 路由层
console.log('[message-handler]', messageId, category, location);
```

---

## 长期演进方向

1. **消息持久化**：所有消息存储到本地数据库，支持历史回放
2. **消息订阅**：前端可选择性订阅特定类型的消息
3. **消息压缩**：对长对话进行消息合并和摘要
4. **消息加密**：敏感信息端到端加密

