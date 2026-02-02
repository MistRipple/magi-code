# 统一消息通道架构设计规范

> **状态**: 设计定稿 (标准路径)
> **关联文档**: `docs/orchestration-unified-design.md` (编排引擎设计)

---

## 1. 架构核心：单一 MessageHub 出口

MultiCLI 采用 **单一消息出口架构**。所有从后端发送到 UI 的通信流（包括 LLM 响应、任务状态、系统通知、控制指令等）必须通过 `MessageHub` 统一分发。

### 1.1 拓扑结构

```text
┌─────────────────────────────────────────────────────────────────┐
│                    MessageHub (唯一消息出口)                      │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  语义 API (业务层调用):                                           │
│    - progress(phase, content)      进度消息                       │
│    - result(content)               结果消息                       │
│    - workerOutput(worker, content) Worker 输出                    │
│    - subTaskCard(subTask)          子任务卡片                     │
│    - systemNotice(content)         系统通知                       │
│                                                                 │
│  控制 API (状态同步调用):                                         │
│    - sendControl(type, payload)    底层控制消息                   │
│    - phaseChange(phase, isRunning) 编排阶段变化                   │
│    - taskAccepted(requestId)       任务确认                       │
│                                                                 │
│  传输能力 (内置中间件):                                           │
│    - 消息去重（基于内容哈希与 ID）                                 │
│    - 流式节流（固定 100ms 最小更新间隔）                           │
│    - 状态追踪（ProcessingState 权威源）                           │
│                                                                 │
│  物理出口:                                                       │
│    emit('unifiedMessage') ──► WebviewProvider ──► postMessage()  │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## 2. 消息协议规范

### 2.1 消息大类 (MessageCategory)

所有通过通道传输的消息必须显式归属于以下四个大类之一：

| 分类 | 描述 | 承载字段 |
| :--- | :--- | :--- |
| **CONTENT** | 业务内容（LLM 响应、对话文本、任务总结、错误详情） | `blocks[]` |
| **CONTROL** | 系统控制（阶段切换、任务确认/拒绝、Worker 连通性状态） | `control{}` |
| **NOTIFY** | 瞬时通知（UI Toast、气泡提醒） | `notify{}` |
| **DATA** | 数据同步（Session 列表更新、配置文件下发、执行统计更新） | `data{}` |

### 2.2 StandardMessage 扩展协议

```typescript
export interface StandardMessage {
  id: string;           // 全局唯一消息 ID
  traceId: string;      // 关联一次完整请求的追踪 ID
  category: MessageCategory; // 🔧 必填：消息大类
  type: MessageType;    // 具体展示类型（text/plan/progress 等）
  source: MessageSource; // 来源标识（orchestrator/worker/system）
  agent: AgentType;     // 具体执行者槽位
  lifecycle: MessageLifecycle; // 生命周期（STARTED/STREAMING/COMPLETED/FAILED）
  blocks: ContentBlock[]; // 内容载体
  metadata: MessageMetadata; // 扩展元数据
  
  // 类别专属字段（互斥使用）
  control?: ControlPayload; 
  notify?: NotifyPayload;
  data?: DataPayload;
}
```

---

## 3. 通信指令集

### 3.1 控制指令 (CONTROL)

| 指令 (controlType) | 载荷参数 | 说明 |
| :--- | :--- | :--- |
| `PHASE_CHANGED` | `phase`, `isRunning`, `taskId` | 驱动 UI 进度条与状态灯 |
| `TASK_ACCEPTED` | `requestId` | 确认后端已接管用户请求，清除前端 Pending 状态 |
| `TASK_REJECTED` | `requestId`, `message` | 拒绝请求（如模型不可用），清除状态并提示 |
| `WORKER_STATUS` | `worker`, `available` | 更新 Worker 槽位的在线状态 |

### 3.2 通知指令 (NOTIFY)

| 级别 (level) | 说明 |
| :--- | :--- |
| `info` | 常规系统提示 |
| `success` | 操作成功反馈 |
| `warning` | 风险警告 |
| `error` | 阻塞性错误提醒 |

---

## 4. 后端状态权威源

`MessageHub` 是系统处理状态 (`ProcessingState`) 的唯一权威来源。

### 4.1 ProcessingState 模型

```typescript
interface ProcessingState {
  isProcessing: boolean; // 是否处于忙碌状态
  source: MessageSource | null; // 触发忙碌的来源
  agent: AgentType | null; // 正在工作的 Agent
  startedAt: number | null; // 开始时间
}
```

任何业务逻辑触发的异步操作，必须通过 `MessageHub` 更新此状态，从而确保 UI 层的 Loading 指示器与后端逻辑完全同步。

---

## 5. 传输约束

1.  **强类型校验**：严禁发送不含 `category` 字段的 StandardMessage。
2.  **原子更新**：流式更新 (`StreamUpdate`) 必须引用有效的 `messageId`，且不得跨越 `traceId` 边界。
3.  **零残留保障**：任何任务终态消息（`COMPLETED/FAILED`）必须自动触发 `ProcessingState` 的清理逻辑。

---

## 9. 实施对齐清单（前后端统一通道核对）

本清单用于确认 **唯一通道** 已在前后端完整落地，无旧事件残留。

### 9.1 后端出口（Extension → Webview）

- **唯一事件监听**：仅监听 `MessageHub` 的 `unified:message / unified:update / unified:complete`。
- **唯一投递**：所有消息通过 `WebviewProvider.postMessage` 发送到 Webview。
- **数据/通知统一**：`sendData` → `MessageHub.data` → `unifiedMessage(category=DATA)`；`sendToast` → `MessageHub.notify` → `unifiedMessage(category=NOTIFY)`。
- **处理状态同步**：`MessageHub` 变更 `ProcessingState`，由 `processingStateChanged` DATA 消息更新 UI。

### 9.2 前端入口（Webview → Svelte）

- **唯一接收**：`message-handler.ts` 仅处理 `unifiedMessage / unifiedUpdate / unifiedComplete`。
- **统一路由**：`message-classifier` → `routing-table` → `message-router` 决定显示位置。
- **统一渲染**：`MessageList` 使用去重后的 `safeMessages`，避免重复 id 导致渲染异常。
- **subTaskCard 归类**：仅依据 `metadata.subTaskCard` 归类为任务卡片，避免 source 依赖。
- **路由缓存清理**：消息完成后清理 `messageTargetMap`，防止长会话内存增长。

### 9.3 禁止项（必须为 0）

- **旧事件**：`orchestrator:message / worker:output / system:notice` 等事件 **不得存在**。
- **双通道**：不允许同一消息同时走旧通道与统一通道。
- **兼容分支**：不允许保留旧路径兜底（if/else 兼容逻辑）。
