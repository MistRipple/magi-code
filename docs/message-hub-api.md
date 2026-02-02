# MessageHub API 使用文档

MessageHub 是 MultiCLI 系统的统一消息与事件中心，负责协调编排者(Orchestrator)、工作单元(Worker)和系统(System)之间的消息流转。它实现了消息的去重、节流和统一路由，确保 UI 呈现的一致性。

## 核心职责

1. **统一出口**: 所有 UI 消息统一通过 MessageHub 发送
2. **主从分离**: 主对话区只承载编排者叙事；Worker 输出在各自 Tab 显示
3. **智能流控**: 内置消息去重（ID/内容）和流式节流（默认 100ms）

## 主要 API 列表

### 1. 生命周期管理

| 方法签名 | 描述 |
| :--- | :--- |
| `newTrace(): string` | 生成新的 Trace ID，开启新会话 |
| `setTraceId(id: string): void` | 设置当前 Trace ID |
| `getTraceId(): string` | 获取当前 Trace ID |

### 2. 编排者叙事 (主对话区)

这些方法发送的消息会显示在主对话区，代表编排者的主要叙事线。

| 方法签名 | 参数说明 | 描述 |
| :--- | :--- | :--- |
| `progress(phase, content, options?)` | `phase`: 阶段名称<br>`content`: 进度内容<br>`options`: `{ percentage?: number; metadata?: MessageMetadata }` | 汇报当前阶段进度 |
| `result(content, options?)` | `content`: 结果内容<br>`options`: `{ success?: boolean; metadata?: MessageMetadata }` | 汇报最终执行结果 |
| `orchestratorMessage(content, options?)` | `content`: 消息内容<br>`options`: `{ type?: MessageType; metadata?: MessageMetadata }` | 发送通用的编排者分析/规划类消息 |
| `subTaskCard(subTask)` | `subTask`: `SubTaskView` 对象 | 展示或更新子任务卡片状态 |

**SubTaskView 类型定义:**
```typescript
interface SubTaskView {
  id: string;
  title: string;
  status: 'pending' | 'running' | 'completed' | 'failed';
  worker: WorkerSlot;
  summary?: string;
  modifiedFiles?: string[];
  createdFiles?: string[];
  duration?: number;
}
```

### 3. Worker 交互 (Worker Tab)

这些方法发送的消息会被路由到对应的 Worker Tab，不会干扰主对话区。

| 方法签名 | 参数说明 | 描述 |
| :--- | :--- | :--- |
| `workerOutput(worker, content, options?)` | `worker`: Worker 标识 (如 'claude')<br>`content`: 输出内容<br>`options`: `{ blocks?: ContentBlock[]; metadata?: MessageMetadata }` | 发送 Worker 的详细执行日志 |

### 4. 系统与错误

| 方法签名 | 参数说明 | 描述 |
| :--- | :--- | :--- |
| `systemNotice(content, metadata?)` | `content`: 通知内容<br>`metadata`: 元数据 | 发送系统级通知（显示在主对话区） |
| `error(err, options?)` | `err`: 错误信息字符串<br>`options`: `{ details?: Record<string, unknown>; recoverable?: boolean }` | 上报错误信息 |

### 5. 全局通信

| 方法签名 | 参数说明 | 描述 |
| :--- | :--- | :--- |
| `broadcast(msg, options?)` | `msg`: 字符串或 `StandardMessage` 对象<br>`options`: `{ target?: string; metadata?: MessageMetadata }` | 向所有组件广播消息，用于跨组件通信 |

## 典型使用示例

### 初始化与会话管理
```typescript
import { MessageHub } from './core/message-hub';

const hub = new MessageHub();
const traceId = hub.newTrace(); // 开始新会话
console.log(`Current Trace ID: ${traceId}`);
```

### 编排任务流程
```typescript
// 1. 阶段汇报
hub.progress('Planning', '正在制定执行计划...', { percentage: 10 });

// 2. 下发任务 (显示子任务卡片)
hub.subTaskCard({
  id: 'task-01',
  title: '分析项目依赖',
  status: 'running',
  worker: 'claude'
});

// 3. Worker 执行细节 (在独立 Tab 中显示)
hub.workerOutput('claude', '读取 package.json...');
hub.workerOutput('claude', '发现 3 个过时依赖');

// 4. 任务完成更新
hub.subTaskCard({
  id: 'task-01',
  title: '分析项目依赖',
  status: 'completed',
  worker: 'claude',
  summary: '分析完成，发现 3 个问题'
});

// 5. 最终结果
hub.result('依赖分析已完成，准备进行优化。');
```

### 事件订阅
```typescript
// 监听所有标准消息
hub.on('unified:message', (msg) => {
  console.log(`Received message from ${msg.source}: ${msg.type}`);
});

// 监听处理状态变化（忙碌/空闲）
hub.on('processingStateChanged', (state) => {
  console.log(`System is processing: ${state.isProcessing}`);
});
```

## 注意事项
- 所有的 `content` 参数都支持 Markdown 格式。
- `progress` 和 `result` 方法会自动过滤空内容。
- `broadcast` 方法不仅会发送消息，还会触发特定的 `broadcast` 事件供特定监听者使用。
