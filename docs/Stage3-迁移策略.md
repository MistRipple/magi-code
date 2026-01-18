# Stage 3: OrchestratorAgent 迁移策略

**创建时间**: 2026-01-18 11:30
**状态**: 规划中

---

## 问题分析

通过代码分析，发现了一个关键架构问题：

### 当前架构

```
IntelligentOrchestrator
  ├── TaskManager (old, src/task-manager.ts)
  └── OrchestratorAgent
      ├── TaskManager (old, passed from IntelligentOrchestrator)
      └── TaskStateManager (独立的状态追踪)
```

### 目标架构

```
IntelligentOrchestrator
  ├── UnifiedTaskManager (new, src/task/unified-task-manager.ts)
  └── OrchestratorAgent
      └── UnifiedTaskManager (passed from IntelligentOrchestrator)
```

---

## 关键发现

### 1. 双重 TaskManager 问题

**发现**: 系统中存在两个不同的 TaskManager：

1. **TaskManager** (src/task-manager.ts)
   - 旧的任务管理器
   - 被 IntelligentOrchestrator 和 OrchestratorAgent 使用
   - 功能相对简单

2. **UnifiedTaskManager** (src/task/unified-task-manager.ts)
   - 新的统一任务管理器
   - 功能完整，包含优先级调度、超时管理等
   - 目前未被 OrchestratorAgent 使用

### 2. 三重状态管理问题

**发现**: 实际上存在三个状态管理系统：

1. **TaskManager** - 基础任务管理
2. **TaskStateManager** - 执行状态追踪
3. **UnifiedTaskManager** - 统一任务管理（未使用）

这比我们之前分析的"双重状态管理"更复杂！

---

## 迁移策略

### 方案 A: 渐进式迁移（推荐）

**阶段 1**: 先迁移 TaskStateManager 到 UnifiedTaskManager
- 保持 TaskManager (old) 不变
- 移除 TaskStateManager
- OrchestratorAgent 使用 UnifiedTaskManager 替代 TaskStateManager
- 保持与 TaskManager (old) 的兼容

**阶段 2**: 再迁移 TaskManager (old) 到 UnifiedTaskManager
- 修改 IntelligentOrchestrator 使用 UnifiedTaskManager
- 移除 TaskManager (old)
- 完全统一到 UnifiedTaskManager

**优势**:
- ✅ 风险可控，分步验证
- ✅ 每个阶段都可以独立测试
- ✅ 出问题可以快速回滚

**劣势**:
- ❌ 需要更多时间
- ❌ 中间状态仍有两个管理器

### 方案 B: 一次性迁移

**直接替换**: 同时替换 TaskManager 和 TaskStateManager 为 UnifiedTaskManager

**优势**:
- ✅ 一步到位
- ✅ 最终架构清晰

**劣势**:
- ❌ 风险高，改动大
- ❌ 难以定位问题
- ❌ 回滚困难

---

## 推荐方案：方案 A（渐进式迁移）

### Stage 3.1: 移除 TaskStateManager

**目标**: 让 OrchestratorAgent 使用 UnifiedTaskManager 替代 TaskStateManager

**关键问题**: 
- OrchestratorAgent 当前接收的是 TaskManager (old)，不是 UnifiedTaskManager
- 需要决定：是传入 UnifiedTaskManager，还是在内部创建？

**解决方案**: 
在 OrchestratorAgent 内部创建 UnifiedTaskManager 实例，与 TaskManager (old) 并存

```typescript
// OrchestratorAgent 构造函数
constructor(
  cliFactory: CLIAdapterFactory,
  config?: Partial<OrchestratorConfig>,
  workspaceRoot?: string,
  snapshotManager?: SnapshotManager,
  taskManager?: TaskManager  // 保持旧的 TaskManager
) {
  // ... 现有代码 ...
  
  // 新增：创建 UnifiedTaskManager
  if (this.workspaceRoot && this.sessionManager) {
    const taskRepository = new SessionManagerTaskRepository(this.sessionManager);
    this.unifiedTaskManager = new UnifiedTaskManager(sessionId, taskRepository);
  }
}
```

**迁移步骤**:

1. **添加 UnifiedTaskManager 实例**
   - 在 OrchestratorAgent 中添加 `private unifiedTaskManager: UnifiedTaskManager | null = null;`
   - 在 `ensureContext()` 中初始化 UnifiedTaskManager

2. **替换 TaskStateManager 调用**
   - 移除 `private taskStateManager: TaskStateManager | null = null;`
   - 移除 TaskStateManager 初始化代码
   - 替换所有 `this.taskStateManager` 调用为 `this.unifiedTaskManager`

3. **移除状态映射逻辑**
   - 删除 `applyTaskStateToTaskManager()`
   - 删除 `mapTaskStateStatus()`
   - 删除 `replayTaskStatesToTaskManager()`

4. **更新事件处理**
   - 移除 `taskStateManager.onStateChange()` 回调
   - 使用 UnifiedTaskManager 的事件系统

5. **处理 taskId 查找问题**
   - 很多地方只有 subTaskId，需要找到对应的 taskId
   - 可能需要维护一个 subTaskId -> taskId 的映射

### Stage 3.2: 统一到 UnifiedTaskManager（后续）

**目标**: 完全移除 TaskManager (old)，只使用 UnifiedTaskManager

**这个阶段暂时不做**，先完成 Stage 3.1

---

## Stage 3.1 详细实施计划

### Step 1: 添加 UnifiedTaskManager 支持

**文件**: src/orchestrator/orchestrator-agent.ts

**修改**:

1. 添加导入:
```typescript
import { UnifiedTaskManager } from '../task/unified-task-manager';
import { SessionManagerTaskRepository } from '../task/session-manager-task-repository';
```

2. 添加属性:
```typescript
private unifiedTaskManager: UnifiedTaskManager | null = null;
private subTaskIdToTaskIdMap: Map<string, string> = new Map();
```

3. 在 `ensureContext()` 中初始化:
```typescript
if (this.contextSessionId !== sessionId) {
  // ... 现有代码 ...
  
  // 初始化 UnifiedTaskManager（替代 TaskStateManager）
  if (this.workspaceRoot && this.sessionManager) {
    const taskRepository = new SessionManagerTaskRepository(this.sessionManager);
    this.unifiedTaskManager = new UnifiedTaskManager(sessionId, taskRepository);
    await this.unifiedTaskManager.initialize();
    
    // 设置事件监听
    this.setupUnifiedTaskManagerEvents();
  }
  
  // 移除 TaskStateManager 初始化
  // this.taskStateManager = new TaskStateManager(...);
}
```

### Step 2: 添加事件处理

**新增方法**:
```typescript
private setupUnifiedTaskManagerEvents(): void {
  if (!this.unifiedTaskManager) return;
  
  this.unifiedTaskManager.on('subtask:started', (task, subTask) => {
    // 处理子任务开始
  });
  
  this.unifiedTaskManager.on('subtask:completed', (task, subTask) => {
    // 处理子任务完成
  });
  
  this.unifiedTaskManager.on('subtask:failed', (task, subTask) => {
    // 处理子任务失败
  });
  
  this.unifiedTaskManager.on('subtask:retrying', (task, subTask) => {
    // 处理子任务重试
  });
  
  this.unifiedTaskManager.on('subtask:progress', (task, subTask, progress) => {
    // 处理进度更新
  });
}
```

### Step 3: 替换方法调用

**需要替换的调用点**（共 25 处）:

#### 3.1 创建任务 (3 处)

**位置**: 行 3753, 3831, 3868

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
// SubTask 已经通过其他方式创建，这里只需要记录映射
this.subTaskIdToTaskIdMap.set(subTask.id, task.id);
```

#### 3.2 更新状态 (9 处)

**位置**: 行 2277, 4650, 4818, 4828, 4847

**当前**:
```typescript
this.taskStateManager?.updateStatus(subTaskId, 'running');
```

**替换为**:
```typescript
const taskId = this.subTaskIdToTaskIdMap.get(subTaskId);
if (taskId && this.unifiedTaskManager) {
  await this.unifiedTaskManager.startSubTask(taskId, subTaskId);
}
```

#### 3.3 重试逻辑 (1 处)

**位置**: 行 4765-4773

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
const taskId = this.subTaskIdToTaskIdMap.get(subTaskId);
if (taskId && this.unifiedTaskManager) {
  if (canRetry) {
    await this.unifiedTaskManager.resetSubTaskForRetry(taskId, subTaskId);
  } else {
    await this.unifiedTaskManager.failSubTask(taskId, subTaskId, error);
  }
}

if (!canRetry && this.recoveryHandler && this.unifiedTaskManager) {
  const task = await this.unifiedTaskManager.getTask(taskId);
  const failedSubTask = task?.subTasks.find(st => st.id === subTaskId);
  // 恢复处理
}
```

#### 3.4 更新进度 (1 处)

**位置**: 行 4854

**当前**:
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

#### 3.5 设置结果 (1 处)

**位置**: 行 4657

**当前**:
```typescript
this.taskStateManager.setResult(subTaskId, result, modifiedFiles);
```

**替换为**:
```typescript
// 结果已经通过 completeSubTask() 传递，不需要单独设置
```

#### 3.6 获取所有任务 (3 处)

**位置**: 行 1460, 2374, 4721

**当前**:
```typescript
for (const task of this.taskStateManager.getAllTasks()) {
  // ...
}
```

**替换为**:
```typescript
// 需要遍历所有 Task 的所有 SubTask
const tasks = await this.unifiedTaskManager.getAllTasks();
for (const task of tasks) {
  for (const subTask of task.subTasks) {
    // ...
  }
}
```

#### 3.7 获取单个任务 (4 处)

**位置**: 行 3753, 3831, 3868, 4773

**当前**:
```typescript
const task = this.taskStateManager.getTask(subTaskId);
```

**替换为**:
```typescript
const taskId = this.subTaskIdToTaskIdMap.get(subTaskId);
const task = taskId ? await this.unifiedTaskManager.getTask(taskId) : null;
const subTask = task?.subTasks.find(st => st.id === subTaskId);
```

### Step 4: 移除状态映射逻辑

**删除方法**:
- `mapTaskStateStatus()` (行 2321-2336)
- `applyTaskStateToTaskManager()` (行 2338-2370)
- `replayTaskStatesToTaskManager()` (行 2372-2377)

**删除调用**:
- 行 1398: `this.applyTaskStateToTaskManager(taskState);`
- 行 1400: `this.replayTaskStatesToTaskManager();`

### Step 5: 更新 RecoveryHandler

**位置**: 行 1402-1407

**当前**:
```typescript
if (this.snapshotManager && this.strategyConfig.enableRecovery) {
  this.recoveryHandler = new RecoveryHandler(
    this.cliFactory,
    this.snapshotManager,
    this.taskStateManager
  );
}
```

**替换为**:
```typescript
if (this.snapshotManager && this.strategyConfig.enableRecovery && this.unifiedTaskManager) {
  this.recoveryHandler = new RecoveryHandler(
    this.cliFactory,
    this.snapshotManager,
    this.unifiedTaskManager
  );
}
```

**注意**: RecoveryHandler 也需要修改以接受 UnifiedTaskManager（这是 Stage 4 的工作）

### Step 6: 移除 TaskStateManager 导入和声明

**删除**:
- 行 35: `import { TaskStateManager, TaskState } from './task-state-manager';`
- 行 211: `private taskStateManager: TaskStateManager | null = null;`

---

## 关键挑战

### 1. taskId 查找问题

**问题**: 很多地方只有 subTaskId，但 UnifiedTaskManager 的方法需要 taskId

**解决方案**: 
- 维护 `subTaskIdToTaskIdMap: Map<string, string>`
- 在创建 SubTask 时记录映射
- 在查询时使用映射查找 taskId

### 2. 异步调用

**问题**: UnifiedTaskManager 的方法都是异步的，需要 await

**解决方案**: 
- 确保所有调用点都在 async 函数中
- 添加 await 关键字
- 处理可能的异步错误

### 3. SessionManager 依赖

**问题**: UnifiedTaskManager 需要 SessionManager，但 OrchestratorAgent 可能没有

**解决方案**: 
- 检查 OrchestratorAgent 是否有 SessionManager
- 如果没有，需要添加或传入

### 4. 事件处理变更

**问题**: 从单一回调改为多个事件监听器

**解决方案**: 
- 创建 `setupUnifiedTaskManagerEvents()` 方法
- 监听所有相关事件
- 保持原有的业务逻辑

---

## 测试计划

### 单元测试

1. 测试 UnifiedTaskManager 初始化
2. 测试 subTaskId -> taskId 映射
3. 测试状态更新
4. 测试重试逻辑
5. 测试进度更新

### 集成测试

1. 测试完整的任务创建流程
2. 测试任务执行流程
3. 测试重试流程
4. 测试恢复流程
5. 测试取消流程

### E2E 测试

1. 运行实际的编排任务
2. 验证状态持久化
3. 验证恢复机制
4. 验证 UI 显示

---

## 风险评估

### 高风险 🔴

1. **SessionManager 缺失**
   - 风险: OrchestratorAgent 可能没有 SessionManager
   - 缓解: 检查并添加 SessionManager 支持

2. **taskId 查找失败**
   - 风险: 映射不完整导致找不到 taskId
   - 缓解: 完善映射逻辑，添加错误处理

3. **异步调用错误**
   - 风险: 忘记 await 导致状态不一致
   - 缓解: 仔细检查所有调用点

### 中风险 🟡

1. **事件处理遗漏**
   - 风险: 某些事件没有正确处理
   - 缓解: 对比原有回调逻辑，确保完整

2. **RecoveryHandler 兼容性**
   - 风险: RecoveryHandler 还依赖 TaskStateManager
   - 缓解: 同步修改 RecoveryHandler（Stage 4）

### 低风险 🟢

1. **性能影响**
   - 风险: UnifiedTaskManager 可能更慢
   - 缓解: 性能测试

---

## 回滚计划

如果出现严重问题：

1. 恢复 TaskStateManager 的使用
2. 恢复状态映射逻辑
3. 回滚代码到上一个稳定版本

---

## 下一步

1. 检查 OrchestratorAgent 是否有 SessionManager
2. 如果没有，添加 SessionManager 支持
3. 开始实施 Step 1: 添加 UnifiedTaskManager 支持

---

**文档创建时间**: 2026-01-18 11:45
**状态**: ✅ 策略规划完成，等待实施
