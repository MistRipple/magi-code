# Stage 3: OrchestratorAgent 中 TaskStateManager 使用分析

**分析时间**: 2026-01-18 11:00
**文件**: src/orchestrator/orchestrator-agent.ts

---

## TaskStateManager 使用点汇总

通过代码分析，发现 OrchestratorAgent 中 TaskStateManager 的使用点如下：

### 1. 导入和声明

**行号**: 36, 211
```typescript
import { TaskStateManager, TaskState } from './task-state-manager';

private taskStateManager: TaskStateManager | null = null;
```

### 2. 初始化

**行号**: 1395-1405
```typescript
this.taskStateManager = new TaskStateManager(sessionId, this.workspaceRoot, true);
await this.taskStateManager.load();
this.taskStateManager.onStateChange((taskState) => {
  // 状态变更回调
  this.taskStateManager
});
```

**用途**: 
- 创建 TaskStateManager 实例
- 加载持久化状态
- 注册状态变更回调

### 3. 取消所有任务

**行号**: 1459-1464
```typescript
if (this.taskStateManager) {
  for (const task of this.taskStateManager.getAllTasks()) {
    // 取消任务
    this.taskStateManager.updateStatus(task.id, 'cancelled');
  }
}
```

**用途**: 在清理时取消所有任务

### 4. 更新任务状态为 running

**行号**: 2277
```typescript
this.taskStateManager?.updateStatus(task.id, 'running');
```

**用途**: 标记任务开始执行

### 5. 获取所有任务状态

**行号**: 2373-2374
```typescript
if (!this.taskStateManager) return;
for (const taskState of this.taskStateManager.getAllTasks()) {
  // 处理任务状态
}
```

**用途**: 遍历所有任务状态

### 6. 创建任务状态 (3 处)

#### 6.1 集成子任务
**行号**: 3753-3754
```typescript
if (this.taskStateManager && !this.taskStateManager.getTask(integrationSubTask.id)) {
  this.taskStateManager.createTask({
    id: integrationSubTask.id,
    parentTaskId: task.id,
    description: integrationSubTask.description,
    assignedWorker: integrationSubTask.assignedWorker,
    status: 'pending',
    progress: 0,
    attempts: 0,
    maxAttempts: 3,
  });
}
```

#### 6.2 修复任务
**行号**: 3831-3832
```typescript
if (this.taskStateManager && !this.taskStateManager.getTask(repairTask.id)) {
  this.taskStateManager.createTask({
    id: repairTask.id,
    parentTaskId: task.id,
    description: repairTask.description,
    assignedWorker: repairTask.assignedWorker,
    status: 'pending',
    progress: 0,
    attempts: 0,
    maxAttempts: 3,
  });
}
```

#### 6.3 普通子任务
**行号**: 3868-3869
```typescript
if (this.taskStateManager && !this.taskStateManager.getTask(subTask.id)) {
  this.taskStateManager.createTask({
    id: subTask.id,
    parentTaskId: task.id,
    description: subTask.description,
    assignedWorker: subTask.assignedWorker,
    status: 'pending',
    progress: 0,
    attempts: 0,
    maxAttempts: 3,
  });
}
```

**用途**: 为不同类型的子任务创建状态追踪

### 7. 更新任务状态和结果

**行号**: 4650-4657
```typescript
if (this.taskStateManager) {
  this.taskStateManager.updateStatus(
    result.subTaskId,
    result.success ? 'completed' : 'failed',
    result.error
  );
  if (result.success) {
    this.taskStateManager.setResult(result.subTaskId, result.result, result.modifiedFiles);
  }
}
```

**用途**: 根据执行结果更新状态

### 8. 获取待重试任务

**行号**: 4720-4721
```typescript
if (this.taskStateManager) {
  const tasks = this.taskStateManager.getAllTasks().filter(task =>
    task.status === 'failed' && task.attempts < task.maxAttempts
  );
}
```

**用途**: 查找可以重试的失败任务

### 9. 重试逻辑

**行号**: 4765-4773
```typescript
if (this.taskStateManager) {
  if (canRetry) {
    this.taskStateManager.resetForRetry(subTaskId);
  } else {
    this.taskStateManager.updateStatus(subTaskId, 'failed', error);
  }
}

if (!canRetry && this.strategyConfig.enableRecovery && this.recoveryHandler && this.taskStateManager) {
  const failedTask = this.taskStateManager.getTask(subTaskId);
  // 恢复处理
}
```

**用途**: 
- 重置任务为重试状态
- 标记任务失败
- 触发恢复机制

### 10. 回滚时更新状态

**行号**: 4818
```typescript
this.taskStateManager?.updateStatus(failedTask.id, 'cancelled', '已回滚');
```

**用途**: 标记任务已回滚

### 11. 恢复时更新状态

**行号**: 4828, 4847
```typescript
this.taskStateManager?.updateStatus(subTaskId, 'running');
// ...
this.taskStateManager?.updateStatus(subTaskId, 'failed', msg);
```

**用途**: 在恢复过程中更新状态

### 12. 更新进度

**行号**: 4854
```typescript
this.taskStateManager?.updateProgress(subTaskId, progress);
```

**用途**: 更新任务执行进度

---

## 方法调用统计

| 方法 | 调用次数 | 用途 |
|------|---------|------|
| `new TaskStateManager()` | 1 | 初始化 |
| `load()` | 1 | 加载持久化状态 |
| `onStateChange()` | 1 | 注册回调 |
| `getAllTasks()` | 3 | 获取所有任务 |
| `getTask()` | 4 | 获取单个任务 |
| `createTask()` | 3 | 创建任务状态 |
| `updateStatus()` | 9 | 更新状态 |
| `setResult()` | 1 | 设置结果 |
| `resetForRetry()` | 1 | 重置重试 |
| `updateProgress()` | 1 | 更新进度 |

**总计**: 25 个调用点

---

## 替换方案

### 1. 初始化 (行 1395-1405)

**当前**:
```typescript
this.taskStateManager = new TaskStateManager(sessionId, this.workspaceRoot, true);
await this.taskStateManager.load();
this.taskStateManager.onStateChange((taskState) => {
  // 回调
});
```

**替换为**:
```typescript
// TaskStateManager 已移除，UnifiedTaskManager 已在构造函数中初始化
// 状态变更通过 UnifiedTaskManager 的事件系统处理
this.taskManager.on('subtask:started', (task, subTask) => {
  // 处理状态变更
});
this.taskManager.on('subtask:completed', (task, subTask) => {
  // 处理状态变更
});
this.taskManager.on('subtask:failed', (task, subTask) => {
  // 处理状态变更
});
```

### 2. 创建任务 (行 3753, 3831, 3868)

**当前**:
```typescript
if (this.taskStateManager && !this.taskStateManager.getTask(subTask.id)) {
  this.taskStateManager.createTask({
    id: subTask.id,
    parentTaskId: task.id,
    description: subTask.description,
    assignedWorker: subTask.assignedWorker,
    status: 'pending',
    progress: 0,
    attempts: 0,
    maxAttempts: 3,
  });
}
```

**替换为**:
```typescript
// SubTask 已经通过 taskManager.createSubTask() 创建
// 不需要额外的状态追踪
// 移除此代码块
```

### 3. 更新状态 (行 2277, 4650, 4818, 4828, 4847)

**当前**:
```typescript
this.taskStateManager?.updateStatus(subTaskId, 'running');
this.taskStateManager?.updateStatus(subTaskId, 'completed');
this.taskStateManager?.updateStatus(subTaskId, 'failed', error);
```

**替换为**:
```typescript
await this.taskManager.startSubTask(taskId, subTaskId);
await this.taskManager.completeSubTask(taskId, subTaskId, result);
await this.taskManager.failSubTask(taskId, subTaskId, error);
```

### 4. 重试逻辑 (行 4765-4773)

**当前**:
```typescript
if (this.taskStateManager) {
  if (canRetry) {
    this.taskStateManager.resetForRetry(subTaskId);
  } else {
    this.taskStateManager.updateStatus(subTaskId, 'failed', error);
  }
}

if (!canRetry && this.recoveryHandler && this.taskStateManager) {
  const failedTask = this.taskStateManager.getTask(subTaskId);
  // 恢复处理
}
```

**替换为**:
```typescript
if (canRetry) {
  await this.taskManager.resetSubTaskForRetry(taskId, subTaskId);
} else {
  await this.taskManager.failSubTask(taskId, subTaskId, error);
}

if (!canRetry && this.recoveryHandler) {
  const task = await this.taskManager.getTask(taskId);
  const failedSubTask = task?.subTasks.find(st => st.id === subTaskId);
  // 恢复处理
}
```

### 5. 获取任务 (行 4720-4721)

**当前**:
```typescript
if (this.taskStateManager) {
  const tasks = this.taskStateManager.getAllTasks().filter(task =>
    task.status === 'failed' && task.attempts < task.maxAttempts
  );
}
```

**替换为**:
```typescript
const task = await this.taskManager.getTask(taskId);
if (task) {
  const failedSubTasks = task.subTasks.filter(st =>
    st.status === 'failed' && st.retryCount < st.maxRetries
  );
}
```

### 6. 更新进度 (行 4854)

**当前**:
```typescript
this.taskStateManager?.updateProgress(subTaskId, progress);
```

**替换为**:
```typescript
await this.taskManager.updateSubTaskProgress(taskId, subTaskId, progress);
```

### 7. 设置结果 (行 4657)

**当前**:
```typescript
this.taskStateManager.setResult(subTaskId, result, modifiedFiles);
```

**替换为**:
```typescript
// 结果已经通过 completeSubTask() 传递
// 不需要单独设置
```

### 8. 取消任务 (行 1459-1464)

**当前**:
```typescript
if (this.taskStateManager) {
  for (const task of this.taskStateManager.getAllTasks()) {
    this.taskStateManager.updateStatus(task.id, 'cancelled');
  }
}
```

**替换为**:
```typescript
const tasks = await this.taskManager.getAllTasks();
for (const task of tasks) {
  await this.taskManager.cancelTask(task.id);
}
```

---

## 状态映射逻辑

需要移除的状态映射函数（如果存在）：

1. `applyTaskStateToTaskManager()` - 将 TaskState 同步到 TaskManager
2. `mapTaskStateStatus()` - 状态类型转换
3. `replayTaskStatesToTaskManager()` - 恢复时重放状态

这些函数在合并后不再需要，因为只有一个状态源。

---

## 依赖关系

### TaskStateManager 依赖

- `TaskState` 接口 → 替换为 `SubTask` 接口
- `TaskStatus` 类型 → 使用统一的 `SubTaskStatus` 类型

### RecoveryHandler 依赖

RecoveryHandler 也依赖 TaskStateManager，需要在 Stage 4 中一起更新。

---

## 迁移步骤

### Step 1: 移除 TaskStateManager 实例化
- 删除 `private taskStateManager: TaskStateManager | null = null;`
- 删除初始化代码

### Step 2: 替换所有方法调用
- 使用上述替换方案逐一替换
- 确保 taskId 和 subTaskId 都正确传递

### Step 3: 移除状态映射逻辑
- 删除所有状态同步函数
- 删除状态类型转换函数

### Step 4: 更新事件处理
- 使用 UnifiedTaskManager 的事件系统
- 替换 `onStateChange` 回调

### Step 5: 测试
- 运行所有测试
- 验证任务创建、执行、重试、恢复流程

---

## 风险评估

### 高风险 🔴
- **状态同步错误**: 确保所有状态更新都正确调用 UnifiedTaskManager
- **taskId 缺失**: 很多地方只有 subTaskId，需要找到对应的 taskId

### 中风险 🟡
- **事件处理变更**: 从单一回调改为多个事件监听器
- **异步调用**: UnifiedTaskManager 的方法都是异步的，需要 await

### 低风险 🟢
- **类型兼容**: SubTask 接口已包含所有 TaskState 字段
- **功能完整**: UnifiedTaskManager 已实现所有需要的功能

---

## 下一步

1. 开始替换代码
2. 处理 taskId 查找问题
3. 更新事件处理
4. 运行测试验证

---

**分析完成时间**: 2026-01-18 11:15
**状态**: ✅ 分析完成，准备开始替换
