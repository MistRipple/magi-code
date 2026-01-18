# Stage 3: OrchestratorAgent 迁移实施方案（最终版）

**创建时间**: 2026-01-18 12:00
**状态**: 准备实施

---

## 架构理解

### 当前架构

```
WebviewProvider
  ├── UnifiedSessionManager
  ├── TaskManager (old) = new TaskManager(sessionManager)
  └── IntelligentOrchestrator
      ├── TaskManager (old, passed from WebviewProvider)
      └── OrchestratorAgent
          ├── TaskManager (old, passed from IntelligentOrchestrator)
          └── TaskStateManager (独立创建)
```

### 关键发现

1. **TaskManager (old) 有 SessionManager**
   - TaskManager 构造函数接收 UnifiedSessionManager
   - 所以可以通过 TaskManager 访问 SessionManager

2. **OrchestratorAgent 没有直接的 SessionManager**
   - 但它接收了 TaskManager
   - 可以通过 TaskManager 获取 SessionManager

3. **三重状态管理**
   - TaskManager (old) - 基础任务管理
   - TaskStateManager - 执行状态追踪
   - UnifiedTaskManager - 统一管理（未使用）

---

## 实施方案

### 方案：在 OrchestratorAgent 内部创建 UnifiedTaskManager

**核心思路**:
- 保持 OrchestratorAgent 接收 TaskManager (old) 不变
- 在内部通过 TaskManager 获取 SessionManager
- 使用 SessionManager 创建 UnifiedTaskManager
- 用 UnifiedTaskManager 替代 TaskStateManager

**优势**:
- ✅ 不需要修改 IntelligentOrchestrator 和 WebviewProvider
- ✅ 改动最小，风险可控
- ✅ 可以逐步迁移

---

## 详细实施步骤

### Step 1: 添加 UnifiedTaskManager 支持

#### 1.1 添加导入

**文件**: src/orchestrator/orchestrator-agent.ts

**在文件顶部添加**:
```typescript
import { UnifiedTaskManager } from '../task/unified-task-manager';
import { SessionManagerTaskRepository } from '../task/session-manager-task-repository';
```

#### 1.2 添加属性

**在类中添加**:
```typescript
private unifiedTaskManager: UnifiedTaskManager | null = null;
private subTaskIdToTaskIdMap: Map<string, string> = new Map();
```

**位置**: 在 `private taskStateManager: TaskStateManager | null = null;` 之后

#### 1.3 修改 ensureContext 方法

**位置**: 行 1390-1409

**当前代码**:
```typescript
if (this.contextSessionId !== sessionId) {
  await this.contextManager.initialize(sessionId, `session-${sessionId}`);
  this.contextManager.clearImmediateContext();
  this.contextSessionId = sessionId;
  if (this.workspaceRoot) {
    this.taskStateManager = new TaskStateManager(sessionId, this.workspaceRoot, true);
    await this.taskStateManager.load();
    this.taskStateManager.onStateChange((taskState) => {
      this.applyTaskStateToTaskManager(taskState);
    });
    this.replayTaskStatesToTaskManager();
    if (this.snapshotManager && this.strategyConfig.enableRecovery) {
      this.recoveryHandler = new RecoveryHandler(
        this.cliFactory,
        this.snapshotManager,
        this.taskStateManager
      );
    }
  }
}
```

**修改为**:
```typescript
if (this.contextSessionId !== sessionId) {
  await this.contextManager.initialize(sessionId, `session-${sessionId}`);
  this.contextManager.clearImmediateContext();
  this.contextSessionId = sessionId;
  
  // 初始化 UnifiedTaskManager（替代 TaskStateManager）
  if (this.workspaceRoot && this.taskManager) {
    // 从 TaskManager 获取 SessionManager
    const sessionManager = (this.taskManager as any).sessionManager;
    if (sessionManager) {
      const taskRepository = new SessionManagerTaskRepository(sessionManager);
      this.unifiedTaskManager = new UnifiedTaskManager(sessionId, taskRepository);
      await this.unifiedTaskManager.initialize();
      
      // 设置事件监听
      this.setupUnifiedTaskManagerEvents();
      
      // 初始化 RecoveryHandler
      if (this.snapshotManager && this.strategyConfig.enableRecovery) {
        this.recoveryHandler = new RecoveryHandler(
          this.cliFactory,
          this.snapshotManager,
          this.unifiedTaskManager
        );
      }
    }
  }
}
```

### Step 2: 添加事件处理方法

**在类中添加新方法**:

```typescript
/**
 * 设置 UnifiedTaskManager 事件监听
 */
private setupUnifiedTaskManagerEvents(): void {
  if (!this.unifiedTaskManager) return;
  
  // 监听子任务开始
  this.unifiedTaskManager.on('subtask:started', (task, subTask) => {
    console.log(`[OrchestratorAgent] SubTask started: ${subTask.id}`);
    // 如果需要同步到 TaskManager (old)，在这里处理
    if (this.taskManager) {
      this.taskManager.updateSubTaskStatus(task.id, subTask.id, 'running');
    }
  });
  
  // 监听子任务完成
  this.unifiedTaskManager.on('subtask:completed', (task, subTask) => {
    console.log(`[OrchestratorAgent] SubTask completed: ${subTask.id}`);
    if (this.taskManager) {
      this.taskManager.updateSubTaskStatus(task.id, subTask.id, 'completed');
    }
  });
  
  // 监听子任务失败
  this.unifiedTaskManager.on('subtask:failed', (task, subTask) => {
    console.log(`[OrchestratorAgent] SubTask failed: ${subTask.id}`);
    if (this.taskManager) {
      this.taskManager.updateSubTaskStatus(task.id, subTask.id, 'failed');
    }
  });
  
  // 监听子任务重试
  this.unifiedTaskManager.on('subtask:retrying', (task, subTask) => {
    console.log(`[OrchestratorAgent] SubTask retrying: ${subTask.id} (${subTask.retryCount}/${subTask.maxRetries})`);
    if (this.taskManager) {
      this.taskManager.updateSubTaskStatus(task.id, subTask.id, 'running');
    }
  });
  
  // 监听进度更新
  this.unifiedTaskManager.on('subtask:progress', (task, subTask, progress) => {
    // 进度更新可以选择性处理
  });
}
```

### Step 3: 替换 TaskStateManager 调用

#### 3.1 创建任务 (3 处)

**位置**: 行 3753, 3831, 3868

**查找**:
```typescript
if (this.taskStateManager && !this.taskStateManager.getTask(integrationSubTask.id)) {
  this.taskStateManager.createTask({
```

**替换为**:
```typescript
// 记录 subTaskId -> taskId 映射
this.subTaskIdToTaskIdMap.set(integrationSubTask.id, task.id);

// SubTask 已经通过 taskManager.addExistingSubTask() 创建
// 不需要在 UnifiedTaskManager 中重复创建
```

**说明**: 
- 移除所有 `taskStateManager.createTask()` 调用
- 只保留 `subTaskIdToTaskIdMap.set()` 来记录映射

#### 3.2 更新状态为 running (2 处)

**位置**: 行 2277, 4828

**查找**:
```typescript
this.taskStateManager?.updateStatus(task.id, 'running');
```

**替换为**:
```typescript
const taskId = this.subTaskIdToTaskIdMap.get(task.id);
if (taskId && this.unifiedTaskManager) {
  await this.unifiedTaskManager.startSubTask(taskId, task.id);
}
```

#### 3.3 更新状态和结果 (1 处)

**位置**: 行 4650-4657

**查找**:
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

**替换为**:
```typescript
const taskId = this.subTaskIdToTaskIdMap.get(result.subTaskId);
if (taskId && this.unifiedTaskManager) {
  if (result.success) {
    await this.unifiedTaskManager.completeSubTask(taskId, result.subTaskId, {
      cliType: result.cliType,
      success: true,
      output: result.result,
      modifiedFiles: result.modifiedFiles,
      duration: result.duration,
      timestamp: new Date(),
    });
  } else {
    await this.unifiedTaskManager.failSubTask(taskId, result.subTaskId, result.error || 'Unknown error');
  }
}
```

#### 3.4 重试逻辑 (1 处)

**位置**: 行 4765-4773

**查找**:
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
```

**替换为**:
```typescript
const taskId = this.subTaskIdToTaskIdMap.get(subTaskId);
if (taskId && this.unifiedTaskManager) {
  if (canRetry) {
    await this.unifiedTaskManager.resetSubTaskForRetry(taskId, subTaskId);
  } else {
    await this.unifiedTaskManager.failSubTask(taskId, subTaskId, error);
  }
}

if (!canRetry && this.strategyConfig.enableRecovery && this.recoveryHandler && this.unifiedTaskManager) {
  const task = await this.unifiedTaskManager.getTask(taskId);
  const failedSubTask = task?.subTasks.find(st => st.id === subTaskId);
  if (failedSubTask) {
    // 恢复处理逻辑
```

#### 3.5 更新状态为 cancelled (1 处)

**位置**: 行 4818

**查找**:
```typescript
this.taskStateManager?.updateStatus(failedTask.id, 'cancelled', '已回滚');
```

**替换为**:
```typescript
const taskId = this.subTaskIdToTaskIdMap.get(failedTask.id);
if (taskId && this.unifiedTaskManager) {
  await this.unifiedTaskManager.cancelSubTask(taskId, failedTask.id);
}
```

**注意**: UnifiedTaskManager 可能没有 `cancelSubTask` 方法，需要检查并可能使用 `skipSubTask` 代替。

#### 3.6 更新状态为 failed (1 处)

**位置**: 行 4847

**查找**:
```typescript
this.taskStateManager?.updateStatus(subTaskId, 'failed', msg);
```

**替换为**:
```typescript
const taskId = this.subTaskIdToTaskIdMap.get(subTaskId);
if (taskId && this.unifiedTaskManager) {
  await this.unifiedTaskManager.failSubTask(taskId, subTaskId, msg);
}
```

#### 3.7 更新进度 (1 处)

**位置**: 行 4854

**查找**:
```typescript
this.taskStateManager?.updateProgress(subTaskId, progress);
```

**替换为**:
```typescript
const taskId = this.subTaskIdToTaskIdMap.get(subTaskId);
if (taskId && this.unifiedTaskManager) {
  await this.unifiedTaskManager.updateSubTaskProgress(taskId, subTaskId, progress);
}
```

#### 3.8 取消所有任务 (1 处)

**位置**: 行 1459-1466

**查找**:
```typescript
if (this.taskStateManager) {
  for (const task of this.taskStateManager.getAllTasks()) {
    if (task.status === 'completed' || task.status === 'failed' || task.status === 'cancelled') {
      continue;
    }
    this.taskStateManager.updateStatus(task.id, 'cancelled');
  }
}
```

**替换为**:
```typescript
if (this.unifiedTaskManager) {
  const tasks = await this.unifiedTaskManager.getAllTasks();
  for (const task of tasks) {
    for (const subTask of task.subTasks) {
      if (subTask.status === 'completed' || subTask.status === 'failed' || subTask.status === 'skipped') {
        continue;
      }
      await this.unifiedTaskManager.skipSubTask(task.id, subTask.id);
    }
  }
}
```

#### 3.9 获取所有任务 (1 处)

**位置**: 行 2373-2377

**查找**:
```typescript
if (!this.taskStateManager) return;
for (const taskState of this.taskStateManager.getAllTasks()) {
  this.applyTaskStateToTaskManager(taskState);
}
```

**替换为**:
```typescript
// 这个方法 (replayTaskStatesToTaskManager) 将被完全删除
// 因为不再需要状态映射
```

#### 3.10 获取待重试任务 (1 处)

**位置**: 行 4720-4721

**查找**:
```typescript
if (this.taskStateManager) {
  const tasks = this.taskStateManager.getAllTasks().filter(task =>
    task.status === 'failed' && task.attempts < task.maxAttempts
  );
}
```

**替换为**:
```typescript
if (this.unifiedTaskManager) {
  const allTasks = await this.unifiedTaskManager.getAllTasks();
  const failedSubTasks: Array<{ task: Task; subTask: SubTask }> = [];
  
  for (const task of allTasks) {
    for (const subTask of task.subTasks) {
      if (subTask.status === 'failed' && subTask.retryCount < subTask.maxRetries) {
        failedSubTasks.push({ task, subTask });
      }
    }
  }
  
  // 使用 failedSubTasks 进行后续处理
}
```

### Step 4: 删除状态映射逻辑

#### 4.1 删除方法

**删除以下方法**:
- `mapTaskStateStatus()` (行 2321-2336)
- `applyTaskStateToTaskManager()` (行 2338-2370)
- `replayTaskStatesToTaskManager()` (行 2372-2377)

#### 4.2 删除调用

**在 ensureContext 中删除**:
- 行 1398: `this.applyTaskStateToTaskManager(taskState);`
- 行 1400: `this.replayTaskStatesToTaskManager();`

### Step 5: 删除 TaskStateManager

#### 5.1 删除导入

**删除**:
```typescript
import { TaskStateManager, TaskState } from './task-state-manager';
```

#### 5.2 删除属性

**删除**:
```typescript
private taskStateManager: TaskStateManager | null = null;
```

### Step 6: 处理 RecoveryHandler

**注意**: RecoveryHandler 的修改是 Stage 4 的工作

**临时方案**: 
- 如果 RecoveryHandler 还需要 TaskStateManager，暂时保留 TaskStateManager
- 或者暂时禁用 RecoveryHandler

**推荐**: 
- 先完成 OrchestratorAgent 的迁移
- 然后立即进行 Stage 4: 更新 RecoveryHandler

---

## 关键问题处理

### 问题 1: 如何获取 SessionManager？

**解决方案**:
```typescript
const sessionManager = (this.taskManager as any).sessionManager;
```

**说明**: 
- TaskManager 有 sessionManager 属性
- 使用 `as any` 绕过类型检查
- 这是临时方案，后续可以改进类型定义

### 问题 2: subTaskId -> taskId 映射何时建立？

**解决方案**:
在创建 SubTask 时立即建立映射：

```typescript
// 在 addExistingSubTask 之后
this.subTaskIdToTaskIdMap.set(subTask.id, task.id);
```

**位置**: 
- 行 3752: 集成子任务
- 行 3830: 修复任务
- 行 3867: 普通子任务

### 问题 3: 异步调用如何处理？

**解决方案**:
- 确保所有调用点都在 async 函数中
- 添加 await 关键字
- 使用 try-catch 处理错误

**示例**:
```typescript
try {
  const taskId = this.subTaskIdToTaskIdMap.get(subTaskId);
  if (taskId && this.unifiedTaskManager) {
    await this.unifiedTaskManager.startSubTask(taskId, subTaskId);
  }
} catch (error) {
  console.error(`[OrchestratorAgent] Failed to start subtask: ${error.message}`);
}
```

### 问题 4: UnifiedTaskManager 没有 cancelSubTask 方法？

**解决方案**:
使用 `skipSubTask` 代替：

```typescript
await this.unifiedTaskManager.skipSubTask(taskId, subTaskId);
```

### 问题 5: 如何同步到 TaskManager (old)？

**解决方案**:
在 UnifiedTaskManager 事件监听器中同步：

```typescript
this.unifiedTaskManager.on('subtask:completed', (task, subTask) => {
  if (this.taskManager) {
    this.taskManager.updateSubTaskStatus(task.id, subTask.id, 'completed');
  }
});
```

**说明**: 
- 保持 TaskManager (old) 和 UnifiedTaskManager 同步
- 这是临时方案，最终会移除 TaskManager (old)

---

## 测试计划

### 单元测试

创建测试文件: `src/test/unit/test-orchestrator-unified-task-manager.js`

**测试用例**:
1. ✅ UnifiedTaskManager 初始化成功
2. ✅ subTaskId -> taskId 映射正确建立
3. ✅ 状态更新正确调用 UnifiedTaskManager
4. ✅ 重试逻辑正确工作
5. ✅ 事件监听器正确触发

### 集成测试

**测试场景**:
1. 创建任务并执行
2. 任务失败后重试
3. 任务取消
4. 进度更新
5. 恢复机制（如果 RecoveryHandler 已更新）

### E2E 测试

**测试流程**:
1. 运行实际的编排任务
2. 验证状态持久化
3. 验证 UI 显示
4. 验证恢复机制

---

## 风险评估

### 高风险 🔴

1. **SessionManager 访问失败**
   - 风险: `(this.taskManager as any).sessionManager` 可能为 undefined
   - 缓解: 添加检查和错误处理

2. **taskId 映射缺失**
   - 风险: 某些 SubTask 没有建立映射
   - 缓解: 在所有创建 SubTask 的地方添加映射

3. **异步调用错误**
   - 风险: 忘记 await 导致状态不一致
   - 缓解: 仔细检查所有调用点，添加 try-catch

### 中风险 🟡

1. **RecoveryHandler 兼容性**
   - 风险: RecoveryHandler 还依赖 TaskStateManager
   - 缓解: 暂时禁用或立即进行 Stage 4

2. **事件处理遗漏**
   - 风险: 某些事件没有正确处理
   - 缓解: 对比原有回调逻辑，确保完整

### 低风险 🟢

1. **性能影响**
   - 风险: UnifiedTaskManager 可能更慢
   - 缓解: 性能测试

---

## 回滚计划

如果出现严重问题：

1. 恢复 TaskStateManager 的使用
2. 恢复状态映射逻辑
3. 移除 UnifiedTaskManager 相关代码
4. 回滚到上一个稳定版本

---

## 实施检查清单

### 准备阶段
- [ ] 备份当前代码
- [ ] 创建新分支
- [ ] 编译确保无错误

### 实施阶段
- [ ] Step 1: 添加 UnifiedTaskManager 支持
- [ ] Step 2: 添加事件处理方法
- [ ] Step 3: 替换 TaskStateManager 调用（25 处）
- [ ] Step 4: 删除状态映射逻辑
- [ ] Step 5: 删除 TaskStateManager
- [ ] Step 6: 处理 RecoveryHandler

### 测试阶段
- [ ] 编译通过
- [ ] 单元测试通过
- [ ] 集成测试通过
- [ ] E2E 测试通过

### 完成阶段
- [ ] 更新文档
- [ ] 创建完成报告
- [ ] 提交代码

---

## 下一步

开始实施 Step 1: 添加 UnifiedTaskManager 支持

---

**文档创建时间**: 2026-01-18 12:15
**状态**: ✅ 方案完成，准备实施
