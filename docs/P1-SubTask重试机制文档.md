# P1 文档：SubTask 级别重试机制

**文档日期**: 2026-01-18
**文档级别**: P1 (重要)
**文档作者**: Claude Sonnet 4.5

---

## 概述

SubTask 重试机制是 MultiCLI 任务系统的核心容错能力，负责在子任务执行失败时自动或手动重试。该机制跨越两个主要系统：

- **UnifiedTaskManager** - 业务逻辑层，管理 SubTask 生命周期和重试计数
- **TaskStateManager** - 执行追踪层，追踪 Worker 执行状态和重试尝试
- **ExecutionScheduler** - 执行层，实现具体的重试逻辑和指数退避

**核心特性**:
- ✅ 自动重试机制（带指数退避）
- ✅ 可配置的最大重试次数
- ✅ 智能错误分类和重试决策
- ✅ 恢复策略（原 CLI 修复、升级、回滚）
- ✅ 状态持久化和恢复

---

## 架构设计

### 数据结构

#### SubTask 重试字段

```typescript
export interface SubTask {
  // ... 其他字段 ...
  
  // 重试机制
  retryCount: number;            // 当前重试次数（0 表示首次执行）
  maxRetries: number;            // 最大重试次数（默认 3）
  
  // 执行结果
  error?: string;                // 错误信息
  status: SubTaskStatus;         // 状态：pending | running | retrying | completed | failed | skipped
}
```

#### TaskState 重试字段

```typescript
export interface TaskState {
  // ... 其他字段 ...
  
  // 重试机制
  attempts: number;              // 重试尝试次数
  maxAttempts: number;           // 最大重试次数（默认 3）
  status: TaskStatus;            // 状态：pending | running | retrying | completed | failed | cancelled
}
```

#### ExecutionScheduler 配置

```typescript
export interface SchedulerConfig {
  maxParallel: number;           // 最大并行数
  timeout: number;               // 超时时间（毫秒）
  retryCount: number;            // 重试次数（默认 1）
}
```

### 状态转换图

```
SubTask 状态转换：
pending → running → completed
             ↓
          failed → retrying → running
             ↓
          cancelled

TaskState 状态转换：
pending → running → completed
             ↓
          paused → running
             ↓
          failed → retrying → running
             ↓
          cancelled
```

---

## 核心功能详解

### 1. ExecutionScheduler 中的自动重试

**位置**: `src/task/execution-scheduler.ts:163-200`

**功能**: 在执行层实现自动重试，带指数退避策略

**实现**:

```typescript
/**
 * 执行单个子任务（带重试机制）
 */
private async executeSubTask(task: SubTaskDef, retryCount = 0): Promise<SubTaskResult> {
  const result: SubTaskResult = {
    subTaskId: task.id,
    cli: task.assignedWorker,
    status: 'running',
    startTime: Date.now(),
  };

  this.emit('taskStart', { task, result, retry: retryCount });

  try {
    const response = await this.executeWithTimeout(task);
    result.response = response;
    result.status = response.error ? 'failed' : 'completed';
    if (response.error) result.error = response.error;
  } catch (error) {
    result.status = 'failed';
    result.error = error instanceof Error ? error.message : String(error);
  }

  result.endTime = Date.now();
  result.duration = result.endTime - result.startTime;

  // 重试逻辑
  if (result.status === 'failed' && retryCount < this.config.retryCount) {
    const shouldRetry = this.shouldRetry(result.error);
    if (shouldRetry) {
      this.emit('taskRetry', { 
        task, 
        result, 
        attempt: retryCount + 1, 
        maxRetries: this.config.retryCount 
      });
      await this.delay(this.getRetryDelay(retryCount));
      return this.executeSubTask(task, retryCount + 1);  // 递归重试
    }
  }

  this.results.set(task.id, result);
  this.emit('taskComplete', { task, result, retries: retryCount });

  return result;
}
```

**关键特性**:

1. **递归重试**: 通过递归调用 `executeSubTask(task, retryCount + 1)` 实现
2. **智能错误分类**: `shouldRetry()` 判断是否应该重试
3. **指数退避**: `getRetryDelay()` 计算重试延迟

**重试决策逻辑**:

```typescript
/**
 * 判断是否应该重试
 */
private shouldRetry(error?: string): boolean {
  if (!error) return false;
  
  // 可重试的错误类型
  const retryableErrors = [
    'timeout', '超时', 'ETIMEDOUT', 'ECONNRESET',
    'rate limit', '限流', 'overloaded', '过载',
    'temporary', '临时', 'retry', '重试',
  ];
  
  const lowerError = error.toLowerCase();
  return retryableErrors.some(e => lowerError.includes(e.toLowerCase()));
}
```

**指数退避策略**:

```typescript
/**
 * 计算重试延迟（指数退避）
 */
private getRetryDelay(retryCount: number): number {
  const baseDelay = 1000;        // 1秒
  const maxDelay = 30000;        // 最大30秒
  const delay = Math.min(baseDelay * Math.pow(2, retryCount), maxDelay);
  // 添加随机抖动（0-1秒）
  return delay + Math.random() * 1000;
}
```

**延迟计算示例**:
- 第 1 次重试: 1000ms + 随机 0-1000ms = 1-2秒
- 第 2 次重试: 2000ms + 随机 0-1000ms = 2-3秒
- 第 3 次重试: 4000ms + 随机 0-1000ms = 4-5秒
- 第 4 次重试: 8000ms + 随机 0-1000ms = 8-9秒
- 第 5+ 次重试: 30000ms（上限）

---

### 2. UnifiedTaskManager 中的 SubTask 失败处理

**位置**: `src/task/unified-task-manager.ts:713-743`

**功能**: 管理 SubTask 失败状态和重试决策

**实现**:

```typescript
/**
 * 失败 SubTask
 */
async failSubTask(taskId: string, subTaskId: string, error: string): Promise<void> {
  const task = await this.getTask(taskId);
  if (!task) throw new Error(`Task not found: ${taskId}`);

  const subTask = task.subTasks.find(st => st.id === subTaskId);
  if (!subTask) throw new Error(`SubTask not found: ${subTaskId}`);

  // 更新状态
  subTask.status = 'failed';
  subTask.completedAt = Date.now();
  subTask.error = error;

  // 清理资源
  this.timeoutChecker.remove(subTaskId);
  this.subTaskQueue.remove(subTaskId);

  // 持久化
  await this.repository.saveTask(task);

  // 发送事件
  this.emit('subtask:failed', task, subTask);

  // 检查是否需要重试
  if (subTask.retryCount < subTask.maxRetries) {
    // 可以重试，但不自动重试，等待外部决策
    console.log(`[UnifiedTaskManager] SubTask ${subTaskId} 可以重试 (${subTask.retryCount}/${subTask.maxRetries})`);
  } else {
    // 已达到最大重试次数，标记 Task 为失败
    console.log(`[UnifiedTaskManager] SubTask ${subTaskId} 已达到最大重试次数，标记 Task 为失败`);
    await this.failTask(taskId);
  }
}
```

**关键点**:

1. **不自动重试**: UnifiedTaskManager 不自动重试，而是等待外部决策
2. **重试计数检查**: 检查 `retryCount < maxRetries` 决定是否可以重试
3. **事件通知**: 发送 `subtask:failed` 事件供外部处理
4. **资源清理**: 移除超时监控和队列中的任务

---

### 3. TaskStateManager 中的重试管理

**位置**: `src/orchestrator/task-state-manager.ts:173-190`

**功能**: 追踪 Worker 执行状态和重试尝试

**实现**:

```typescript
/**
 * 检查任务是否可以重试
 */
canRetry(taskId: string): boolean {
  const task = this.tasks.get(taskId);
  if (!task) return false;
  return task.attempts < task.maxAttempts;
}

/**
 * 重置任务为待执行状态（用于重试）
 */
resetForRetry(taskId: string): void {
  const task = this.tasks.get(taskId);
  if (!task) return;

  // 应用状态转换，增加重试次数
  this.applyStatus(task, 'retrying', { 
    force: true, 
    reset: true,           // 重置进度和错误
    incrementAttempt: true // 增加重试次数
  });

  this.notifyChange(task);
  this.autoSaveIfEnabled();
  this.emitStateChanged(task);
}
```

**状态应用逻辑**:

```typescript
private applyStatus(
  task: TaskState,
  status: TaskStatus,
  options: {
    error?: string;
    force?: boolean;
    reset?: boolean;
    incrementAttempt?: boolean;
  } = {}
): boolean {
  const prevStatus = task.status;
  
  // 检查状态转换是否合法
  if (!options.force && !this.isTransitionAllowed(prevStatus, status)) {
    console.warn(`[TaskStateManager] 非法状态流转: ${prevStatus} -> ${status}`);
    return false;
  }
  
  task.status = status;
  
  // 重置选项：清除错误、结果、进度
  if (options.reset) {
    task.error = undefined;
    task.result = undefined;
    task.progress = 0;
    task.startedAt = undefined;
    task.completedAt = undefined;
  }
  
  // 设置错误信息
  if (typeof options.error === 'string') {
    task.error = options.error;
  }
  
  // 记录开始时间
  if (status === 'running' && !task.startedAt) {
    task.startedAt = Date.now();
  }
  
  // 记录完成时间
  if (status === 'completed' || status === 'failed' || status === 'cancelled') {
    task.completedAt = Date.now();
  }
  
  // 增加重试次数
  if (status === 'retrying' && options.incrementAttempt) {
    task.attempts += 1;
  }
  
  // 完成时设置进度为 100%
  if (status === 'completed' && task.progress < 100) {
    task.progress = 100;
  }
  
  return true;
}
```

**状态转换规则**:

```typescript
private isTransitionAllowed(from: TaskStatus, to: TaskStatus): boolean {
  if (from === to) return true;
  
  const allowed: Record<TaskStatus, TaskStatus[]> = {
    pending: ['running', 'retrying', 'paused', 'failed', 'cancelled', 'completed'],
    running: ['completed', 'failed', 'retrying', 'paused', 'cancelled'],
    paused: ['running', 'cancelled'],
    retrying: ['running', 'failed', 'cancelled', 'completed'],
    failed: ['retrying', 'cancelled'],
    completed: [],
    cancelled: [],
  };
  
  return allowed[from].includes(to);
}
```

---

### 4. RecoveryHandler 中的恢复策略

**位置**: `src/orchestrator/recovery-handler.ts:75-197`

**功能**: 基于失败类型选择恢复策略

**恢复策略**:

```typescript
export type RecoveryStrategy =
  | 'retry_same_cli'      // 原 CLI 修复
  | 'retry_with_context'  // 提供更多上下文
  | 'escalate_to_claude'  // 升级到 Claude
  | 'rollback';           // 回滚
```

**恢复计划**:

```typescript
private getRecoveryPlan(failureType: FailureType): RecoveryStrategy[] {
  const plans: Record<FailureType, RecoveryStrategy[]> = {
    tool_failure: ['retry_same_cli', 'retry_with_context', 'rollback'],
    compile_failure: ['retry_with_context', 'escalate_to_claude', 'rollback'],
    test_failure: ['retry_with_context', 'escalate_to_claude', 'rollback'],
    logic_failure: ['escalate_to_claude', 'rollback'],
    dependency_failure: ['escalate_to_claude', 'rollback'],
    unknown: ['retry_with_context', 'escalate_to_claude', 'rollback'],
  };
  return plans[failureType] ?? plans.unknown;
}
```

**原 CLI 重试实现**:

```typescript
private async retrySameCli(
  taskId: string,
  failedTask: TaskState,
  errorDetails: string
): Promise<RecoveryResult> {
  console.log(`[RecoveryHandler] 原 CLI 尝试修复: ${failedTask.assignedWorker}`);

  // 重置任务状态为重试中
  this.taskStateManager.resetForRetry(failedTask.id);
  this.taskStateManager.updateStatus(failedTask.id, 'running');

  // 构建修复提示词
  const fixPrompt = this.buildFixPrompt(failedTask, errorDetails, 'simple');

  try {
    // 发送修复请求
    const response = await this.cliFactory.sendMessage(failedTask.assignedWorker, fixPrompt);

    if (response.error) {
      this.taskStateManager.updateStatus(failedTask.id, 'failed', response.error);
      return {
        success: false,
        strategy: 'retry_same_cli',
        attempts: failedTask.attempts,
        message: `修复失败: ${response.error}`,
      };
    }

    // 修复成功
    this.taskStateManager.updateStatus(failedTask.id, 'completed');
    if (response.content) {
      this.taskStateManager.setResult(failedTask.id, response.content);
    }
    
    return {
      success: true,
      strategy: 'retry_same_cli',
      attempts: failedTask.attempts,
      message: '原 CLI 修复成功',
    };
  } catch (error) {
    this.taskStateManager.updateStatus(failedTask.id, 'failed', String(error));
    return {
      success: false,
      strategy: 'retry_same_cli',
      attempts: failedTask.attempts,
      message: `修复异常: ${error instanceof Error ? error.message : String(error)}`,
    };
  }
}
```

---

## 使用示例

### 示例 1: 基本重试流程

```typescript
import { ExecutionScheduler } from './execution-scheduler';
import { CLIAdapterFactory } from './cli/adapter-factory';

// 创建调度器（配置最多重试 2 次）
const factory = new CLIAdapterFactory();
const scheduler = new ExecutionScheduler(factory, {
  maxParallel: 3,
  timeout: 300000,
  retryCount: 2,  // 最多重试 2 次
});

// 监听重试事件
scheduler.on('taskRetry', ({ task, result, attempt, maxRetries }) => {
  console.log(`任务 ${task.id} 重试 (${attempt}/${maxRetries})`);
  console.log(`错误: ${result.error}`);
});

// 监听完成事件
scheduler.on('taskComplete', ({ task, result, retries }) => {
  if (retries > 0) {
    console.log(`任务 ${task.id} 在 ${retries} 次重试后成功`);
  }
});

// 执行任务
const results = await scheduler.execute(splitResult);
```

### 示例 2: SubTask 失败处理

```typescript
import { UnifiedTaskManager } from './unified-task-manager';

const taskManager = new UnifiedTaskManager(sessionId, repository);

// 监听 SubTask 失败事件
taskManager.on('subtask:failed', async (task, subTask) => {
  console.log(`SubTask ${subTask.id} 失败: ${subTask.error}`);
  
  // 检查是否可以重试
  if (subTask.retryCount < subTask.maxRetries) {
    console.log(`可以重试 (${subTask.retryCount}/${subTask.maxRetries})`);
    // 外部决策是否重试
  } else {
    console.log(`已达到最大重试次数，Task 标记为失败`);
  }
});

// 处理 SubTask 失败
await taskManager.failSubTask(taskId, subTaskId, '执行超时');
```

### 示例 3: 恢复策略

```typescript
import { RecoveryHandler } from './recovery-handler';

const recoveryHandler = new RecoveryHandler(
  cliFactory,
  snapshotManager,
  taskStateManager,
  { maxAttempts: 3, enableRollback: true }
);

// 执行恢复流程
const result = await recoveryHandler.recover(
  taskId,
  failedTask,
  verificationResult,
  errorDetails
);

if (result.success) {
  console.log(`恢复成功，使用策略: ${result.strategy}`);
} else {
  console.log(`恢复失败，已尝试 ${result.attempts} 次`);
}
```

---

## 最佳实践

### 1. 配置合理的重试次数

```typescript
// ✅ 好的做法：根据任务类型配置
const config = {
  // 网络相关任务：多重试
  networkTasks: { retryCount: 3, timeout: 30000 },
  
  // 编译任务：中等重试
  compileTasks: { retryCount: 2, timeout: 60000 },
  
  // 测试任务：少重试
  testTasks: { retryCount: 1, timeout: 120000 },
};

// ❌ 不好的做法：无限重试
const badConfig = { retryCount: 999 };
```

### 2. 监听重试事件进行日志记录

```typescript
// ✅ 好的做法
scheduler.on('taskRetry', ({ task, attempt, maxRetries }) => {
  logger.warn(`Task retry: ${task.id} (${attempt}/${maxRetries})`);
});

// ❌ 不好的做法：忽略重试事件
```

### 3. 区分可重试和不可重试的错误

```typescript
// ✅ 好的做法：智能错误分类
const retryableErrors = [
  'timeout',
  'ECONNRESET',
  'rate limit',
  'temporary',
];

const shouldRetry = (error: string) => {
  return retryableErrors.some(e => error.includes(e));
};

// ❌ 不好的做法：重试所有错误
const shouldRetryAll = () => true;
```

### 4. 使用指数退避避免雪崩

```typescript
// ✅ 好的做法：指数退避
const getRetryDelay = (retryCount: number) => {
  const baseDelay = 1000;
  const maxDelay = 30000;
  return Math.min(baseDelay * Math.pow(2, retryCount), maxDelay);
};

// ❌ 不好的做法：固定延迟
const getFixedDelay = () => 1000;
```

### 5. 持久化重试状态

```typescript
// ✅ 好的做法：自动保存
taskStateManager.resetForRetry(taskId);  // 自动持久化

// ❌ 不好的做法：忘记保存
task.attempts += 1;  // 内存中修改，未持久化
```

---

## 故障排查

### 问题 1: 任务无限重试

**症状**: 任务一直在重试，不会停止

**原因**: 
- `maxRetries` 设置过高或为无穷大
- 错误分类逻辑错误，导致所有错误都被重试

**解决方案**:
```typescript
// 检查配置
console.log(`maxRetries: ${subTask.maxRetries}`);

// 检查错误分类
console.log(`shouldRetry: ${shouldRetry(error)}`);

// 添加日志
if (retryCount > 10) {
  console.error('重试次数过多，可能存在问题');
  throw new Error('重试次数超过限制');
}
```

### 问题 2: 重试延迟过长

**症状**: 重试等待时间太长，影响用户体验

**原因**: 
- 指数退避基数过大
- 最大延迟设置过高

**解决方案**:
```typescript
// 调整参数
const baseDelay = 500;      // 减小基数
const maxDelay = 10000;     // 减小最大延迟

// 或者使用线性退避
const getRetryDelay = (retryCount: number) => {
  return Math.min(1000 * (retryCount + 1), 10000);
};
```

### 问题 3: 重试后仍然失败

**症状**: 重试多次后仍然失败，需要升级处理

**原因**: 
- 原 CLI 无法解决问题
- 需要更多上下文或升级到更强大的 CLI

**解决方案**:
```typescript
// 使用恢复策略
const strategy = recoveryHandler.determineStrategy(attempts, failureType);

if (strategy === 'escalate_to_claude') {
  // 升级到 Claude
  const result = await recoveryHandler.escalateToClaude(taskId, failedTask, errorDetails);
}
```

---

## 性能考虑

### 时间复杂度

| 操作 | 时间复杂度 | 说明 |
|------|-----------|------|
| 单次重试 | O(1) | 只需更新计数器 |
| 重试延迟计算 | O(1) | 指数计算 |
| 状态转换检查 | O(1) | 哈希表查询 |
| 恢复策略选择 | O(1) | 查表操作 |

### 空间复杂度

- 重试状态: O(1) - 只需存储计数器
- 恢复计划: O(F) - F = 失败类型数量（常数）

### 优化建议

1. **缓存恢复计划**: 避免重复计算
2. **异步重试**: 不阻塞主线程
3. **批量重试**: 合并多个失败任务的重试

---

## 相关文档

- [任务系统设计分析报告](./任务系统设计分析报告.md)
- [P0-双重状态管理系统分析报告](./P0-双重状态管理系统分析报告.md)
- [P1-TaskDependencyGraph集成文档](./P1-TaskDependencyGraph集成文档.md)

---

## 总结

SubTask 重试机制是 MultiCLI 的关键容错能力，提供了：

✅ **多层次重试**: ExecutionScheduler（执行层）+ RecoveryHandler（恢复层）
✅ **智能决策**: 错误分类、恢复策略选择
✅ **指数退避**: 避免雪崩，提高成功率
✅ **状态持久化**: 支持恢复和审计
✅ **事件驱动**: 便于监控和日志记录

**使用建议**:
- 根据任务类型配置合理的重试次数
- 监听重试事件进行日志记录
- 区分可重试和不可重试的错误
- 使用指数退避避免雪崩
- 持久化重试状态以支持恢复

---

**文档生成时间**: 2026-01-18 03:30
**文档作者**: Claude Sonnet 4.5
**状态**: ✅ P1 文档完成

