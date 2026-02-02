# MessageHub API 文档

MessageHub 是系统的统一消息与事件中心，负责协调编排者(Orchestrator)、工作单元(Worker)和系统(System)之间的消息流转。
它实现了消息的去重、节流和统一路由，确保 UI 呈现的一致性。

## 核心职责
1. **统一出口**: 所有 UI 消息统一通过 MessageHub 发送
2. **主从分离**: 主对话区只承载编排者叙事；Worker 输出在各自 Tab 显示
3. **智能流控**: 内置消息去重（ID/内容）和流式节流（默认 100ms）

## API 概览

### 1. 生命周期管理
- `newTrace()`: 生成新 Trace ID，开启新会话
- `setTraceId(id)`: 设置当前 Trace ID
- `getTraceId()`: 获取当前 Trace ID

### 2. 编排者叙事 (主对话区)
- `progress(phase, content, options?)`: 汇报当前阶段进度
- `result(content, options?)`: 汇报最终执行结果
- `orchestratorMessage(content, options?)`: 发送分析/规划类消息
- `subTaskCard(subTask)`: 展示/更新子任务卡片状态

### 3. Worker 交互 (Worker Tab)
- `workerOutput(worker, content, options?)`: 发送 Worker 执行日志

### 4. 系统与错误
- `systemNotice(content, metadata?)`: 发送系统级通知
- `error(err, options?)`: 上报错误信息

### 5. 全局通信
- `broadcast(msg, options?)`: 向所有组件广播消息

## 详细 API 说明

### `newTrace()`
生成一个新的 Trace ID，标志着一次新的完整交互（Session）的开始。后续的所有消息都将自动关联到此 Trace ID。
- **返回**: `string` - 新生成的 Trace ID

### `progress(phase, content, options?)`
在主对话区显示进度更新，通常用于展示编排者当前的阶段。
- **参数**:
  - `phase`: `string` - 阶段名称 (e.g., "Planning", "Execution")
  - `content`: `string` - 进度描述文本
  - `options`: `{ percentage?: number, metadata?: object }` (可选)

### `result(content, options?)`
在主对话区显示最终结果，标志着当前任务或阶段的结束。
- **参数**:
  - `content`: `string` - 结果描述文本
  - `options`: `{ success?: boolean, metadata?: object }` (可选)

### `workerOutput(worker, content, options?)`
发送 Worker 的执行日志或中间产出。这些消息**不会**显示在主对话区，而是路由到对应 Worker 的独立 Tab 中。
- **参数**:
  - `worker`: `WorkerSlot` - Worker 标识 (e.g., 'codex', 'claude')
  - `content`: `string` - 输出内容
  - `options`: `{ blocks?: ContentBlock[], metadata?: object }` (可选)

### `subTaskCard(subTask)`
在主对话区显示或更新子任务卡片。用于让用户感知任务的拆分和执行状态。
- **参数**:
  - `subTask`: `SubTaskView`
    - `id`: `string` - 任务 ID
    - `title`: `string` - 任务标题
    - `status`: `'pending' | 'running' | 'completed' | 'failed'`
    - `worker`: `WorkerSlot` - 负责的 Worker
    - `summary`: `string` (可选) - 任务摘要或结果

## 使用示例

```typescript
import { MessageHub } from './message-hub';

const hub = new MessageHub();
hub.newTrace(); // 开始新会话

// 1. 阶段汇报
hub.progress('Planning', '正在制定执行计划...');

// 2. 下发任务 (显示子任务卡片)
hub.subTaskCard({
  id: 'task-01',
  title: '分析依赖',
  status: 'running',
  worker: 'claude'
});

// 3. Worker 执行 (内容流向独立 Tab)
hub.workerOutput('claude', '读取 package.json...');
hub.workerOutput('claude', '发现 15 个依赖项...');

// 4. 任务完成 (更新卡片状态)
hub.subTaskCard({
  id: 'task-01',
  title: '分析依赖',
  status: 'completed',
  worker: 'claude',
  summary: '分析完成，发现 3 个潜在问题'
});

// 5. 最终结果
hub.result('依赖分析已完成，准备进行优化。');
```
