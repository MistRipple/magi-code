# MessageHub API 使用说明

MessageHub 是系统的**统一消息与事件中心**，负责协调编排者 (Orchestrator)、工作单元 (Worker) 和系统 (System) 之间的消息流转。它确保所有 UI 消息通过唯一出口发送，并提供消息去重、节流和统一路由功能，以保证用户界面的一致性和响应性。

## 核心职责

1.  **统一出口**: 所有的 UI 交互消息必须通过 MessageHub 发送。
2.  **主从分离**: 主对话区仅展示编排者的宏观叙事和关键里程碑；具体的 Worker 输出（如代码生成过程）在各自独立的 Tab 中显示。
3.  **智能流控**: 内置 ID/内容去重和流式消息节流（默认 100ms），防止 UI 消息过载。

## 核心 API 列表

### 1. 生命周期管理

*   **`newTrace(): string`**
    *   **描述**: 生成一个新的 Trace ID，标志着一次新的完整交互（Session）的开始。后续消息将自动关联到此 ID。
    *   **返回**: 新生成的 Trace ID。

*   **`setTraceId(traceId: string): void`**
    *   **描述**: 手动设置当前的 Trace ID。
    *   **参数**: `traceId` - 目标 Trace ID。

*   **`getTraceId(): string`**
    *   **描述**: 获取当前的 Trace ID。

### 2. 编排者叙事 (主对话区)

这些 API 发送的消息会显示在主聊天界面，代表系统的主线进展。

*   **`progress(phase: string, content: string, options?: { percentage?: number; metadata?: MessageMetadata }): void`**
    *   **描述**: 汇报当前任务阶段和进度。
    *   **参数**:
        *   `phase`: 阶段名称 (例如 "Planning", "Execution")。
        *   `content`: 进度描述文本。
        *   `options`: 可选参数，包含进度百分比等元数据。

*   **`result(content: string, options?: { success?: boolean; metadata?: MessageMetadata }): void`**
    *   **描述**: 汇报任务或阶段的最终结果。
    *   **参数**:
        *   `content`: 结果描述文本。
        *   `options`: 可选参数，包含成功标志等元数据。

*   **`orchestratorMessage(content: string, options?: { type?: MessageType; metadata?: MessageMetadata }): void`**
    *   **描述**: 发送通用的编排者消息，用于分析、规划或一般性对话。
    *   **参数**:
        *   `content`: 消息内容。
        *   `options`: 可选参数，指定消息类型等。

*   **`subTaskCard(subTask: SubTaskView): void`**
    *   **描述**: 在主对话区显示或更新一个子任务卡片，用于可视化任务分解和状态。
    *   **参数**:
        *   `subTask`: 子任务对象，包含 `id`, `title`, `status` ('pending' | 'running' | 'completed' | 'failed'), `worker` 等信息。

### 3. Worker 交互 (Worker Tab)

这些 API 发送的消息**不**显示在主对话区，而是路由到对应 Worker 的独立面板。

*   **`workerOutput(worker: WorkerSlot, content: string, options?: { blocks?: ContentBlock[]; metadata?: MessageMetadata }): void`**
    *   **描述**: 发送 Worker 的执行日志、中间产出或代码生成内容。
    *   **参数**:
        *   `worker`: Worker 标识 (例如 'codex', 'claude')。
        *   `content`: 输出内容的文本表示。
        *   `options`: 可选参数，可包含结构化的内容块 (`blocks`)。

### 4. 系统与错误

*   **`systemNotice(content: string, metadata?: MessageMetadata): void`**
    *   **描述**: 发送系统级通知。
    *   **参数**:
        *   `content`: 通知内容。

*   **`error(error: string, options?: { details?: Record<string, unknown>; recoverable?: boolean }): void`**
    *   **描述**: 上报错误信息。
    *   **参数**:
        *   `error`: 错误描述文本。
        *   `options`: 可选参数，包含错误详情和是否可恢复标志。

### 5. 全局通信

*   **`broadcast(message: string | StandardMessage, options?: { target?: string; metadata?: MessageMetadata }): void`**
    *   **描述**: 向所有订阅者广播消息，用于跨组件通信。
    *   **参数**:
        *   `message`: 消息内容字符串或标准消息对象。
        *   `options`: 可选参数，指定目标等。

## 使用示例

```typescript
import { MessageHub } from './core/message-hub';

// 1. 初始化
const hub = new MessageHub();
const traceId = hub.newTrace(); // 开始新会话
console.log(`Session started: ${traceId}`);

// 2. 编排者规划阶段 (主对话区)
hub.progress('Planning', '正在分析用户需求...');
hub.orchestratorMessage('我需要先检查一下当前的项目结构。');

// 3. 分配任务 (显示子任务卡片)
hub.subTaskCard({
  id: 'task-001',
  title: '扫描文件系统',
  status: 'running',
  worker: 'claude'
});

// 4. Worker 执行 (Worker Tab - 主对话区不可见)
// 这些日志会出现在 "Claude" 标签页下
hub.workerOutput('claude', '读取 package.json...');
hub.workerOutput('claude', '发现 25 个依赖项。');

// 5. 任务完成更新
hub.subTaskCard({
  id: 'task-001',
  title: '扫描文件系统',
  status: 'completed',
  worker: 'claude',
  summary: '扫描完成，发现 React 项目结构'
});

// 6. 最终结果 (主对话区)
hub.result('分析完成，这是一个基于 React 的前端项目。');
```

## 事件订阅

MessageHub 继承自 `EventEmitter`，允许订阅各类消息事件：

```typescript
// 监听所有标准消息
hub.on('unified:message', (message) => {
  console.log('收到消息:', message);
});

// 监听处理状态变化 (忙碌/空闲)
hub.on('processingStateChanged', (state) => {
  console.log('系统状态:', state.isProcessing ? '忙碌' : '空闲');
});
```
