# Stage 3.2 完成报告：添加 UnifiedTaskManager 支持

**完成时间**: 2026-01-18 12:30
**状态**: ✅ 完成

---

## 目标

为 OrchestratorAgent 添加 UnifiedTaskManager 支持，为后续替换 TaskStateManager 做准备。

---

## 完成的工作

### 1. 添加导入语句

**文件**: src/orchestrator/orchestrator-agent.ts

**添加的导入**:
```typescript
import { UnifiedTaskManager } from '../task/unified-task-manager';
import { SessionManagerTaskRepository } from '../task/session-manager-task-repository';
```

**位置**: 行 37-38

### 2. 添加属性声明

**添加的属性**:
```typescript
private unifiedTaskManager: UnifiedTaskManager | null = null;
private subTaskIdToTaskIdMap: Map<string, string> = new Map();
```

**位置**: 行 214-215

**说明**:
- `unifiedTaskManager`: UnifiedTaskManager 实例
- `subTaskIdToTaskIdMap`: 维护 subTaskId -> taskId 的映射，用于快速查找

### 3. 修改 ensureContext 方法

**位置**: 行 1399-1420

**添加的代码**:
```typescript
// 初始化 UnifiedTaskManager（替代 TaskStateManager）
if (this.workspaceRoot && this.taskManager) {
  // 从 TaskManager 获取 SessionManager
  const sessionManager = (this.taskManager as any).sessionManager;
  if (sessionManager) {
    const taskRepository = new SessionManagerTaskRepository(sessionManager, sessionId);
    this.unifiedTaskManager = new UnifiedTaskManager(sessionId, taskRepository);
    await this.unifiedTaskManager.initialize();

    // 设置事件监听
    this.setupUnifiedTaskManagerEvents();

    // 初始化 RecoveryHandler（暂时注释，因为 RecoveryHandler 还不支持 UnifiedTaskManager）
    // if (this.snapshotManager && this.strategyConfig.enableRecovery) {
    //   this.recoveryHandler = new RecoveryHandler(
    //     this.cliFactory,
    //     this.snapshotManager,
    //     this.unifiedTaskManager
    //   );
    // }
  }
}
```

**关键点**:
- 通过 `(this.taskManager as any).sessionManager` 获取 SessionManager
- 创建 SessionManagerTaskRepository 适配器
- 初始化 UnifiedTaskManager
- 调用 setupUnifiedTaskManagerEvents() 设置事件监听
- RecoveryHandler 暂时注释，因为它还不支持 UnifiedTaskManager（Stage 4 会处理）

**保留的代码**:
```typescript
// 保留 TaskStateManager 初始化（暂时保留，后续会删除）
if (this.workspaceRoot) {
  this.taskStateManager = new TaskStateManager(sessionId, this.workspaceRoot, true);
  await this.taskStateManager.load();
  this.taskStateManager.onStateChange((taskState) => {
    this.applyTaskStateToTaskManager(taskState);
  });
  this.replayTaskStatesToTaskManager();

  // RecoveryHandler 暂时还使用 TaskStateManager（Stage 4 会更新）
  if (this.snapshotManager && this.strategyConfig.enableRecovery) {
    this.recoveryHandler = new RecoveryHandler(
      this.cliFactory,
      this.snapshotManager,
      this.taskStateManager
    );
  }
}
```

**说明**: 暂时保留 TaskStateManager，确保系统继续正常工作

### 4. 添加事件处理方法

**位置**: 行 2401-2445

**新增方法**:
```typescript
/**
 * 设置 UnifiedTaskManager 事件监听
 */
private setupUnifiedTaskManagerEvents(): void {
  if (!this.unifiedTaskManager) return;

  // 监听子任务开始
  this.unifiedTaskManager.on('subtask:started', (task, subTask) => {
    console.log(`[OrchestratorAgent] SubTask started: ${subTask.id}`);
    // 同步到 TaskManager (old)
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
    console.log(`[OrchestratorAgent] SubTask progress: ${subTask.id} - ${progress}%`);
  });
}
```

**功能**:
- 监听 5 个关键事件
- 将 UnifiedTaskManager 的状态变更同步到 TaskManager (old)
- 添加日志输出便于调试

---

## 代码质量

### TypeScript 编译

```bash
npx tsc --noEmit
```

**结果**: ✅ 无错误

### 代码统计

- 新增导入: 2 行
- 新增属性: 2 行
- 修改方法: 1 个 (ensureContext)
- 新增方法: 1 个 (setupUnifiedTaskManagerEvents)
- 新增代码: ~70 行

---

## 架构说明

### 当前架构（过渡状态）

```
OrchestratorAgent
  ├── TaskManager (old) - 基础任务管理
  ├── TaskStateManager - 执行状态追踪（保留）
  └── UnifiedTaskManager - 统一管理（新增）
```

**说明**:
- 三个管理器并存（过渡状态）
- UnifiedTaskManager 已初始化，但还未使用
- TaskStateManager 继续工作，确保系统稳定
- 通过事件监听器保持状态同步

### 状态同步机制

```
UnifiedTaskManager 事件
  ↓
setupUnifiedTaskManagerEvents()
  ↓
TaskManager (old) 状态更新
```

**目的**: 确保两个管理器的状态一致

---

## 关键决策

### 1. SessionManager 访问方式

**问题**: OrchestratorAgent 没有直接的 SessionManager

**解决方案**:
```typescript
const sessionManager = (this.taskManager as any).sessionManager;
```

**说明**: 
- 使用 `as any` 绕过类型检查
- 这是临时方案，后续可以改进类型定义

### 2. RecoveryHandler 处理

**问题**: RecoveryHandler 还不支持 UnifiedTaskManager

**解决方案**: 
- 暂时注释 UnifiedTaskManager 的 RecoveryHandler 初始化
- 保留 TaskStateManager 的 RecoveryHandler 初始化
- 在 Stage 4 中完整处理

### 3. 保留 TaskStateManager

**原因**:
- 确保系统继续正常工作
- 渐进式迁移，降低风险
- 便于对比和调试

---

## 测试

### 编译测试

```bash
npx tsc --noEmit
```

**结果**: ✅ 通过

### 运行时测试

**待完成**: 需要在实际运行中验证

**测试点**:
1. UnifiedTaskManager 是否正确初始化
2. 事件监听器是否正确触发
3. 状态同步是否正常工作

---

## 下一步

### Stage 3.3: 替换 TaskStateManager 调用

**目标**: 将所有 TaskStateManager 的调用替换为 UnifiedTaskManager

**任务**:
1. 替换创建任务 (3 处)
2. 替换更新状态 (9 处)
3. 替换重试逻辑 (1 处)
4. 替换更新进度 (1 处)
5. 替换获取任务 (4 处)
6. 替换取消任务 (1 处)

**预计时间**: 1-2 小时

---

## 风险评估

### 已缓解的风险

1. ✅ **SessionManager 访问** - 通过 `(this.taskManager as any).sessionManager` 解决
2. ✅ **编译错误** - 所有类型错误已修复
3. ✅ **RecoveryHandler 兼容性** - 暂时保留 TaskStateManager 版本

### 待处理的风险

1. ⚠️ **运行时错误** - 需要实际运行测试
2. ⚠️ **状态同步问题** - 需要验证事件监听器是否正确工作

---

## 总结

✅ **Stage 3.2 成功完成**

- 所有计划功能已实现
- TypeScript 编译通过
- 代码质量良好
- 为下一步替换做好准备

OrchestratorAgent 现在已经具备了 UnifiedTaskManager 支持，可以开始替换 TaskStateManager 的调用。

---

**报告生成时间**: 2026-01-18 12:35
**状态**: ✅ Stage 3.2 完成
