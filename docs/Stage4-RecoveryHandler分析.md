# Stage 4: RecoveryHandler 迁移分析

**分析时间**: 2026-01-18 11:15
**目标**: 将 RecoveryHandler 从 TaskStateManager 迁移到 UnifiedTaskManager

---

## 当前状态分析

### 依赖关系

```typescript
import { TaskStateManager, TaskState } from './task-state-manager';
```

**使用位置**:
1. 构造函数参数: `taskStateManager: TaskStateManager`
2. 实例属性: `private taskStateManager: TaskStateManager`
3. 方法参数: `failedTask: TaskState` (多处)
4. 方法内部调用: TaskStateManager 的各种方法

### TaskStateManager 调用统计

| 方法 | 调用次数 | 位置 |
|------|---------|------|
| `resetForRetry()` | 3 | lines 160, 209, 258 |
| `updateStatus()` | 12 | lines 161, 169, 178, 189, 210, 218, 227, 238, 259, 267, 276, 287, 324, 334 |
| `setResult()` | 3 | lines 180, 229, 278 |
| `shouldContinueRecovery()` | 1 | line 439 (使用 task.attempts) |
| `getRecoveryStats()` | 1 | line 446 (使用 tasks 数组) |

**总计**: 20 个 TaskStateManager 调用

### TaskState 类型使用

| 位置 | 用途 |
|------|------|
| Line 77 | `recover()` 方法参数 |
| Line 155 | `retrySameCli()` 方法参数 |
| Line 204 | `retryWithContext()` 方法参数 |
| Line 253 | `escalateToClaude()` 方法参数 |
| Line 302 | `performRollback()` 方法参数 |
| Line 369 | `buildFixPrompt()` 方法参数 |
| Line 413 | `buildEscalatePrompt()` 方法参数 |
| Line 439 | `shouldContinueRecovery()` 方法参数 |
| Line 446 | `getRecoveryStats()` 方法参数 |

**总计**: 9 处 TaskState 类型引用

---

## 迁移策略

### 1. 类型映射

| TaskState 字段 | SubTask 字段 | 说明 |
|---------------|-------------|------|
| `id` | `id` | ✅ 相同 |
| `description` | `description` | ✅ 相同 |
| `assignedWorker` | `assignedWorker` | ✅ 相同 |
| `status` | `status` | ✅ 相同 (已统一) |
| `attempts` | `retryCount` | ⚠️ 字段名不同 |
| `maxAttempts` | `maxRetries` | ⚠️ 字段名不同 |
| `modifiedFiles` | `modifiedFiles` | ✅ 相同 |
| `result` | `output` | ⚠️ 字段名不同，类型不同 (string vs string[]) |
| `error` | `error` | ✅ 相同 |

### 2. 方法映射

| TaskStateManager 方法 | UnifiedTaskManager 方法 | 说明 |
|---------------------|------------------------|------|
| `resetForRetry(taskId)` | `resetSubTaskForRetry(taskId, subTaskId)` | ✅ 已实现 |
| `updateStatus(taskId, status, error?)` | `startSubTask()` / `completeSubTask()` / `failSubTask()` | ⚠️ 需要根据状态选择方法 |
| `setResult(taskId, result, files?)` | `completeSubTask(taskId, subTaskId, result)` | ✅ 包含在 completeSubTask 中 |

### 3. 构造函数变更

**之前**:
```typescript
constructor(
  cliFactory: CLIAdapterFactory,
  snapshotManager: SnapshotManager,
  taskStateManager: TaskStateManager,
  config?: Partial<RecoveryConfig>
)
```

**之后**:
```typescript
constructor(
  cliFactory: CLIAdapterFactory,
  snapshotManager: SnapshotManager,
  unifiedTaskManager: UnifiedTaskManager,
  config?: Partial<RecoveryConfig>
)
```

---

## 实施方案

### Phase 1: 更新导入和类型

1. **删除 TaskStateManager 导入**
   ```typescript
   // 删除
   import { TaskStateManager, TaskState } from './task-state-manager';
   ```

2. **添加 UnifiedTaskManager 导入**
   ```typescript
   // 添加
   import { UnifiedTaskManager } from '../task/unified-task-manager';
   import { SubTask } from '../task/types';
   ```

3. **替换所有 TaskState 为 SubTask**
   - 9 处方法参数
   - 所有内部引用

### Phase 2: 更新构造函数和属性

1. **更新构造函数参数**
   ```typescript
   constructor(
     cliFactory: CLIAdapterFactory,
     snapshotManager: SnapshotManager,
     unifiedTaskManager: UnifiedTaskManager,  // 改变
     config?: Partial<RecoveryConfig>
   )
   ```

2. **更新实例属性**
   ```typescript
   private unifiedTaskManager: UnifiedTaskManager;  // 改变
   ```

### Phase 3: 更新方法调用

#### 3.1 `recover()` 方法

**需要添加 taskId 参数**（因为 UnifiedTaskManager 需要 taskId）

**之前**:
```typescript
async recover(
  taskId: string,
  failedTask: TaskState,
  verificationResult: VerificationResult,
  errorDetails: string
): Promise<RecoveryResult>
```

**之后**:
```typescript
async recover(
  taskId: string,
  subTaskId: string,  // 新增
  failedTask: SubTask,
  verificationResult: VerificationResult,
  errorDetails: string
): Promise<RecoveryResult>
```

#### 3.2 `retrySameCli()` 方法

**之前**:
```typescript
this.taskStateManager.resetForRetry(failedTask.id);
this.taskStateManager.updateStatus(failedTask.id, 'running');
// ... 执行修复 ...
this.taskStateManager.updateStatus(failedTask.id, 'failed', response.error);
// 或
this.taskStateManager.updateStatus(failedTask.id, 'completed');
this.taskStateManager.setResult(failedTask.id, response.content);
```

**之后**:
```typescript
await this.unifiedTaskManager.resetSubTaskForRetry(taskId, failedTask.id);
await this.unifiedTaskManager.startSubTask(taskId, failedTask.id);
// ... 执行修复 ...
await this.unifiedTaskManager.failSubTask(taskId, failedTask.id, response.error);
// 或
await this.unifiedTaskManager.completeSubTask(taskId, failedTask.id, {
  cliType: failedTask.assignedWorker,
  success: true,
  output: response.content,
  modifiedFiles: [],
  duration: 0,
  timestamp: new Date(),
});
```

#### 3.3 字段名映射

需要在代码中将 `attempts` 替换为 `retryCount`:
- Line 81: `const attempts = failedTask.attempts;` → `const attempts = failedTask.retryCount;`
- Line 173: `attempts: failedTask.attempts` → `attempts: failedTask.retryCount`
- 等等...

### Phase 4: 更新辅助方法

#### 4.1 `shouldContinueRecovery()`

**之前**:
```typescript
shouldContinueRecovery(task: TaskState): boolean {
  return task.attempts < this.config.maxAttempts;
}
```

**之后**:
```typescript
shouldContinueRecovery(task: SubTask): boolean {
  return task.retryCount < task.maxRetries;
}
```

#### 4.2 `getRecoveryStats()`

**之前**:
```typescript
getRecoveryStats(tasks: TaskState[]): {...} {
  const recoveredTasks = tasks.filter(t => t.attempts > 0);
  // ...
}
```

**之后**:
```typescript
getRecoveryStats(tasks: SubTask[]): {...} {
  const recoveredTasks = tasks.filter(t => t.retryCount > 0);
  // ...
}
```

---

## 挑战和解决方案

### 挑战 1: taskId 参数传递

**问题**: UnifiedTaskManager 的所有方法都需要 `taskId` 和 `subTaskId`，但 RecoveryHandler 的很多方法只接收 `failedTask`。

**解决方案**:
- 在 `recover()` 方法中添加 `subTaskId` 参数
- 将 `taskId` 和 `subTaskId` 传递给所有内部方法

### 挑战 2: 异步调用

**问题**: UnifiedTaskManager 的方法都是异步的，需要 `await`。

**解决方案**:
- 所有调用 UnifiedTaskManager 的地方添加 `await`
- 添加错误处理 `.catch()`

### 挑战 3: result 字段类型不同

**问题**: TaskState.result 是 string，SubTask.output 是 string[]。

**解决方案**:
- 在 `completeSubTask()` 时，将 string 转换为 string[]
- 或者使用 WorkerResult 的 output 字段

### 挑战 4: OrchestratorAgent 的调用

**问题**: OrchestratorAgent 中调用 RecoveryHandler 的地方需要更新。

**解决方案**:
- 在 OrchestratorAgent 中更新 RecoveryHandler 的初始化
- 更新调用 `recover()` 方法的地方，传递正确的参数

---

## 测试计划

### 单元测试

1. **测试 `shouldContinueRecovery()`**
   - 测试 retryCount < maxRetries 返回 true
   - 测试 retryCount >= maxRetries 返回 false

2. **测试 `getRecoveryStats()`**
   - 测试统计逻辑正确

### 集成测试

1. **测试 `retrySameCli()`**
   - 测试成功修复流程
   - 测试失败流程
   - 验证 UnifiedTaskManager 状态更新

2. **测试 `retryWithContext()`**
   - 测试带上下文的修复流程

3. **测试 `escalateToClaude()`**
   - 测试升级到 Claude 的流程

4. **测试 `performRollback()`**
   - 测试回滚流程
   - 验证文件恢复

---

## 风险评估

### 高风险 🔴

1. **OrchestratorAgent 调用更新**
   - 风险: 调用参数不匹配
   - 缓解: 仔细检查所有调用点

### 中风险 🟡

1. **异步调用错误处理**
   - 风险: 未捕获的 Promise 错误
   - 缓解: 所有调用添加 .catch()

2. **字段名映射错误**
   - 风险: attempts vs retryCount 混淆
   - 缓解: 全局搜索替换，仔细检查

### 低风险 🟢

1. **类型检查**
   - 风险: TypeScript 编译错误
   - 缓解: 编译器会捕获所有类型错误

---

## 实施步骤

1. ✅ 分析 RecoveryHandler 代码
2. ⏳ 更新导入和类型定义
3. ⏳ 更新构造函数和属性
4. ⏳ 更新 `recover()` 方法
5. ⏳ 更新 `retrySameCli()` 方法
6. ⏳ 更新 `retryWithContext()` 方法
7. ⏳ 更新 `escalateToClaude()` 方法
8. ⏳ 更新 `performRollback()` 方法
9. ⏳ 更新辅助方法
10. ⏳ 更新 OrchestratorAgent 调用
11. ⏳ 编译检查
12. ⏳ 创建完成报告

---

**预计时间**: 2-3 小时
**复杂度**: 中等
**影响范围**: RecoveryHandler + OrchestratorAgent

---

**分析完成时间**: 2026-01-18 11:15
