# 消息链路架构设计

## 分层架构总览

```text
┌─────────────────────────────────────────────────────────────────┐
│                    后端 (Extension Host)                         │
├─────────────────────────────────────────────────────────────────┤
│  L1 生产层    Normalizer + Adapter + MessageFactory             │
│  L2 中枢层    MessageHub (= Bus + Pipeline)                     │
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
| L1 | 生产层 | 后端 | Normalizer, Adapter, MessageFactory | 创建 StandardMessage / StreamUpdate，**决定消息的 ID** |
| L2 | 中枢层 | 后端 | MessageHub (= MessageBus + MessagePipeline) | 协议保护：去重、节流、生命周期状态管理、事件分发 |
| L3 | 桥接层 | 后端 | WebviewProvider | 监听 Hub 事件，postMessage 到前端 |
| L4 | 接收层 | 前端 | vscode-bridge | 接收 postMessage，分发给 listeners |
| L5 | 路由层 | 前端 | message-handler, message-router | 所有「是否展示」「展示在哪」的决策在此层完成 |
| L6 | 渲染层 | 前端 | Store, Components | 状态管理（响应式 Store）+ UI 渲染 |

### 核心原则：各层只做本层职责范围内的事，禁止越权

> **设计教训**：Pipeline（L2）曾越权承担「根据消息类型将 ID 映射到占位符 ID」的职责，
> 导致独立消息（INTERACTION、RESULT 等）被错误映射到流式卡片，覆盖已有内容。
> 根因是 L2 做了本应由 L1（ID 分配）决定的事情。已通过移除 `messageIdAliasMap` 修复。

### 层间边界（5 个接口）

| 边界 | 上层 | 下层 | 接口方式 |
| ---- | ---- | ---- | -------- |
| L1→L2 | Adapter | MessageHub | `messageHub.sendMessage(msg)` / `messageHub.sendUpdate(update)` |
| L2→L3 | MessageHub | WebviewProvider | `emit('unified:message/update/complete', msg)` |
| L3→L4 | WebviewProvider | vscode-bridge | `postMessage({ type, message })` |
| L4→L5 | vscode-bridge | message-handler | `listener(message)` 回调 |
| L5→L6 | message-handler | Store | `addThreadMessage(msg)` / `addAgentMessage(msg)` 等 |

---

## 各层详细定义

### L1 生产层（后端）

**组件**：
- `src/normalizer/*.ts` - 各 LLM 的响应解析器
- `src/llm/adapters/*.ts` - 各 LLM 的适配器
- `src/protocol/message-protocol.ts` - MessageFactory（消息创建工厂）

**唯一职责**：创建 StandardMessage / StreamUpdate，**决定消息的 ID**

**流式输出**：Normalizer 接收 LLM 原始 chunk → 解析 → emit MESSAGE(STARTED) / UPDATE / COMPLETE

**ID 分配**：
- 流式消息：`adapter.startStreamWithContext()` 查询占位符 ID → `normalizer.startStream(boundId)` → 流式 chunk 使用占位符 ID
- 独立消息：MessageFactory `createStandardMessage()` 生成随机 ID（INTERACTION、RESULT、PROGRESS 等）

**代码路径**：

```text
LLM API Response
    ↓
Normalizer.startStream(boundId) → emit MESSAGE(id=boundId, STARTED)
Normalizer.processTextDelta()   → emit UPDATE(messageId=boundId)
Normalizer.endStream()          → emit COMPLETE(messageId=boundId)
    ↓ adapter.setupNormalizerEvents()
    ↓ messageHub.sendMessage(message) / messageHub.sendUpdate(update)
进入 L2
```

**禁止行为**：
- **不做过滤或丢弃**
- 不做路由决策（不判断消息应该展示在哪个区域）

---

### L2 中枢层（后端）

**组件**：
- `src/orchestrator/core/message-hub.ts` (MessageBus)
- `src/orchestrator/core/message-pipeline.ts` (MessagePipeline)

**唯一职责**：协议保护——去重、节流、生命周期状态管理、事件分发

**流式输出**：接收 L1 消息 → 状态机管理 (STARTED→STREAMING→COMPLETED) → emit unified:message/update/complete

**提供的服务**：

- 维护 `requestMessageIdMap`（供 L1 adapter 查询占位符 ID）
- 生成 `eventSeq`（会话内全局单调递增）
- 管理 `sealedCards`（封口保护）
- 管理 `messageStates`（生命周期跟踪）

**代码路径**：

```text
messageHub.sendMessage(message)
    ↓
Pipeline.process() → 去重检查 → 状态管理 → 生命周期更新
    ↓ emit('unified:message', message)
messageHub.sendUpdate(update)
    ↓
Pipeline.processUpdate() → 节流(100ms) → 封口检查
    ↓ emit('unified:update', update)
进入 L3
```

**禁止行为**：

- **不修改消息 ID**（不做 ID 映射/重写）
- 不做内容过滤（只做协议层保护）
- 不判断消息类型决定归属
- 不做展示决策

---

### L3 桥接层（后端）

**组件**：
- `src/ui/webview-provider.ts` 的 `setupMessageHubListeners()`

**唯一职责**：监听 L2 事件，封装为 postMessage 格式，发送到前端

**流式输出**：`on('unified:message')` → `postMessage({ type: 'unifiedMessage', message })`

**附加职责**：创建占位符消息（`emitUserAndPlaceholder`）——占位符本身是一条 L1 层级的消息

**代码路径**：

```text
messageHub.on('unified:message', (message) => {
    ↓
this.postMessage({ type: 'unifiedMessage', message })
    ↓ 跨进程
进入 L4
```

**禁止行为**：

- **不做过滤或转换**
- 不修改消息内容或 ID
- 不做路由决策

---

### L4 接收层（前端）

**组件**：
- `src/ui/webview-svelte/src/lib/vscode-bridge.ts`

**唯一职责**：接收 postMessage，分发给注册的 listeners

**代码路径**：

```text
window.addEventListener('message', (event) => {
    ↓
listeners.forEach(listener => listener(message))
    ↓
进入 L5
```

**禁止行为**：

- **不做过滤或转换**
- 纯透传

---

### L5 路由层（前端）

**组件**：
- `src/ui/webview-svelte/src/lib/message-handler.ts`
- `src/ui/webview-svelte/src/lib/message-router.ts`

**唯一职责**：所有「是否展示」「展示在哪」的决策在此层完成

**流式输出**：

- `handleContentMessage` 处理消息归属（占位替换 / 独立卡片）
- `handleStandardUpdate` 路由增量更新
- `handleStandardComplete` 标记完成

**提供的服务**：

- `messageTargetMap`（messageId → DisplayTarget）
- `requestBinding`（requestId → 占位符关联）

**代码路径**：

```text
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

**禁止行为**：

- **不修改消息内容**
- 不做渲染逻辑（只做路由写入 Store）

---

### L6 渲染层（前端）

**组件**：
- `src/ui/webview-svelte/src/stores/messages.svelte.ts`
- `src/ui/webview-svelte/src/components/*.svelte`

**唯一职责**：状态管理（响应式 Store）+ UI 渲染

**流式输出**：Store 中消息数组变化 → Svelte 响应式更新 → 卡片组件增量渲染

**禁止行为**：

- **不做消息过滤**（只做渲染决策，如空内容显示加载态）
- 不做路由决策

---

## 层间数据流（流式输出完整链路）

```text
L1: Normalizer.startStream(boundId) → emit MESSAGE(id=boundId, STARTED)
    Normalizer.processTextDelta()   → emit UPDATE(messageId=boundId)
    Normalizer.endStream()          → emit COMPLETE(messageId=boundId)
      ↓ adapter.setupNormalizerEvents → messageHub.sendMessage / sendUpdate
L2: Pipeline.process(message)       → 状态管理 → emit unified:message
    Pipeline.processUpdate(update)  → 节流/去重 → emit unified:update
      ↓ messageHub events
L3: WebviewProvider.on('unified:*') → postMessage({ type, message/update })
      ↓ 跨进程边界
L4: vscode-bridge                   → listener(message)
      ↓
L5: message-handler                 → 路由决策 → addThreadMessage / addAgentMessage
      ↓
L6: Store → Components              → 渲染卡片
```

---

## 消息过滤规范

各层允许的过滤行为及其归属：

| # | 过滤节点 | 归属层级 | 行为 | 说明 |
| ---- | ---- | ---- | ---- | ---- |
| 1 | MessageHub 去重 | L2 中枢层 | 同 ID + lifecycle 消息只发一次 | 协议层保护 |
| 2 | MessageHub 节流 | L2 中枢层 | 流式更新间隔 100ms | 协议层保护 |
| 3 | 消息类别分发 | L5 路由层 | 按 category 分发到不同处理器 | 路由决策 |
| 4 | `visibility` 可见性 | L5 路由层 | 按 visibility 决定展示去向 | 路由决策 |
| 5 | 路由表缺失保护 | L5 路由层 | update 暂存到队列等待路由建立 | 时序保护 |
| 6 | Complete 类别过滤 | L5 路由层 | 非 CONTENT 的 complete 跳过 | 业务正确性 |

---

## 消息可见性控制

### visibility 字段定义

```typescript
interface StandardMessage {
  // ... 其他字段

  /**
   * 消息可见性
   * - 'user': 用户可见（默认）
   * - 'system': 仅系统日志可见
   * - 'debug': 仅调试模式可见
   */
  visibility: 'user' | 'system' | 'debug';
}
```

### 处理规则

1. 后端在创建消息时设置 `visibility` 字段（默认 `'user'`）。
2. 消息正常流经 L1 → L2 → L3 → L4 → L5，各层不因 visibility 做拦截。
3. L5 路由层根据 `visibility` 决定去向：
   - `user`：正常路由到 thread/worker。
   - `system`：路由到 `location: 'none'`（系统日志区），不展示给用户。
   - `debug`：仅在调试模式下显示。

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
