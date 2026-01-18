# TaskManager → UnifiedTaskManager 迁移实施计划

**创建时间**: 2026-01-18 15:30
**状态**: 进行中
**目标**: 完全删除 TaskManager，统一使用 UnifiedTaskManager

---

## 项目概述

### 目标

删除 TaskManager，统一使用 UnifiedTaskManager，并完整清理所有废弃内容。

### 成功标准

- [ ] TaskManager 完全删除
- [ ] 所有调用迁移到 UnifiedTaskManager
- [ ] 删除 4 个状态同步监听器
- [ ] 所有测试通过 (38/38)
- [ ] 性能提升 30-50%
- [ ] 无废弃代码遗留

---

## Stage 1: Plan 管理功能迁移

**目标**: 将 TaskManager 独有的 Plan 管理功能添加到 UnifiedTaskManager

**状态**: 🔄 进行中

### 1.1 添加 Plan 管理方法到 UnifiedTaskManager

**文件**: `src/task/unified-task-manager.ts`

**需要添加的方法**:

```typescript
/**
 * 更新 Task 的执行计划信息
 */
async updateTaskPlan(
  taskId: string,
  planInfo: {
    planId: string;
    planSummary?: string;
    status?: Task['planStatus'];
  }
): Promise<void> {
  const task = this.taskCache.get(taskId);
  if (!task) {
    throw new Error(`Task not found: ${taskId}`);
  }

  // 更新 Plan 信息
  task.planId = planInfo.planId;
  task.planSummary = planInfo.planSummary;
  task.planStatus = planInfo.status ?? 'ready';
  task.planCreatedAt = Date.now();
  task.planUpdatedAt = Date.now();

  // 持久化
  await this.repository.saveTask(task);

  // 更新缓存
  this.taskCache.set(taskId, task);

  // 发布事件
  this.emit('task:plan-updated', task);
}

/**
 * 更新 Task 的执行计划状态
 */
async updateTaskPlanStatus(
  taskId: string,
  status: Task['planStatus']
): Promise<void> {
  const task = this.taskCache.get(taskId);
  if (!task) {
    throw new Error(`Task not found: ${taskId}`);
  }

  // 更新状态
  task.planStatus = status;
  task.planUpdatedAt = Date.now();

  // 持久化
  await this.repository.saveTask(task);

  // 更新缓存
  this.taskCache.set(taskId, task);

  // 发布事件
  this.emit('task:plan-status-updated', task);
}
```

**新增事件类型**:
```typescript
export interface TaskManagerEvents {
  // ... 现有事件 ...
  'task:plan-updated': (task: Task) => void;
  'task:plan-status-updated': (task: Task) => void;
}
```

### 1.2 测试 Plan 管理功能

**测试文件**: 创建或更新测试

**测试用例**:
- [ ] updateTaskPlan() 正确更新 Plan 信息
- [ ] updateTaskPlanStatus() 正确更新状态
- [ ] Plan 信息正确持久化
- [ ] 事件正确触发

### 1.3 验证

- [ ] 编译通过
- [ ] 测试通过
- [ ] Plan 管理功能正常工作

---

## Stage 2: OrchestratorAgent 迁移

**目标**: 替换 OrchestratorAgent 中所有 TaskManager 调用

**状态**: ⏳ 待开始

### 2.1 分析 TaskManager 使用情况

**文件**: `src/orchestrator/orchestrator-agent.ts`

**TaskManager 调用位置** (10 处):
```bash
# 需要运行命令找出具体位置
grep -n "this.taskManager\." src/orchestrator/orchestrator-agent.ts
```

### 2.2 逐个替换调用

**替换策略**:

| TaskManager 方法 | UnifiedTaskManager 方法 | 注意事项 |
|-----------------|------------------------|---------|
| `createTask()` | `createTask()` | 同步 → 异步 |
| `updateTaskStatus()` | `startTask()`, `completeTask()`, `failTask()` | 根据状态选择 |
| `addSubTask()` | `createSubTask()` | 同步 → 异步 |
| `updateSubTaskStatus()` | `startSubTask()`, `completeSubTask()`, `failSubTask()` | 根据状态选择 |
| `updateSubTaskFiles()` | `updateSubTaskFiles()` | 同步 → 异步 |
| `updateTaskPlan()` | `updateTaskPlan()` | 新增方法 |
| `updateTaskPlanStatus()` | `updateTaskPlanStatus()` | 新增方法 |

### 2.3 处理同步 → 异步转换

**策略**:
- 在调用方添加 `await`
- 如果调用方不是 async，改为 async
- 或者使用 `.then()` 处理

### 2.4 删除 TaskManager 实例

**修改**:
```typescript
// 删除
private taskManager: TaskManager | null = null;

// 删除构造函数参数
constructor(config: {
  // taskManager?: TaskManager;  // 删除
  // ...
})

// 删除初始化
// this.taskManager = config.taskManager ?? null;  // 删除
```

### 2.5 验证

- [ ] 编译通过
- [ ] 所有测试通过
- [ ] 功能正常工作

---

## Stage 3: 其他文件迁移

**目标**: 迁移其他依赖 TaskManager 的文件

**状态**: ⏳ 待开始

### 3.1 IntelligentOrchestrator

**文件**: `src/orchestrator/intelligent-orchestrator.ts`

**修改**:
```typescript
// 删除 TaskManager 创建
// const taskManager = new TaskManager(this.sessionManager);

// 删除 TaskManager 传递
const orchestratorAgent = new OrchestratorAgent({
  // taskManager,  // 删除
  // ...
});
```

### 3.2 webview-provider.ts

**文件**: `src/ui/webview-provider.ts`

**分析**: 检查是否直接使用 TaskManager
**修改**: 如果使用，替换为 UnifiedTaskManager

### 3.3 orchestrator.ts

**文件**: `src/orchestrator.ts`

**分析**: 检查是否直接使用 TaskManager
**修改**: 如果使用，替换为 UnifiedTaskManager

### 3.4 测试文件

**文件**: `src/test/real-orchestrator-e2e.ts`

**修改**: 更新测试以使用 UnifiedTaskManager

### 3.5 验证

每个文件修改后：
- [ ] 编译通过
- [ ] 相关测试通过
- [ ] 功能正常工作

---

## Stage 4: 删除状态同步代码

**目标**: 删除 UnifiedTaskManager 事件监听器中的 TaskManager 同步代码

**状态**: ⏳ 待开始

### 4.1 删除状态同步监听器

**文件**: `src/orchestrator/orchestrator-agent.ts:2357-2390`

**删除代码**:
```typescript
// 删除整个监听器或监听器中的 TaskManager 同步部分
this.unifiedTaskManager.on('subtask:started', (task, subTask) => {
  // 删除: this.taskManager?.updateSubTaskStatus(task.id, subTask.id, 'running');
  // 保留其他逻辑（如果有）
});

this.unifiedTaskManager.on('subtask:completed', (task, subTask) => {
  // 删除: this.taskManager?.updateSubTaskStatus(task.id, subTask.id, 'completed');
  // 删除: this.taskManager?.updateSubTaskFiles(task.id, subTask.id, subTask.modifiedFiles || []);
  // 保留其他逻辑（如果有）
});

this.unifiedTaskManager.on('subtask:failed', (task, subTask) => {
  // 删除: this.taskManager?.updateSubTaskStatus(task.id, subTask.id, 'failed');
  // 保留其他逻辑（如果有）
});

this.unifiedTaskManager.on('subtask:skipped', (task, subTask) => {
  // 删除: this.taskManager?.updateSubTaskStatus(task.id, subTask.id, 'skipped');
  // 保留其他逻辑（如果有）
});
```

### 4.2 验证

- [ ] 编译通过
- [ ] 所有测试通过
- [ ] 无状态同步代码遗留

---

## Stage 5: 完整清理和文档

**目标**: 标记废弃、删除文件、更新文档、完整验证

**状态**: ⏳ 待开始

### 5.1 标记 TaskManager 为 @deprecated

**文件**: `src/task-manager.ts`

**添加注释**:
```typescript
/**
 * Task 管理器
 *
 * @deprecated 自 v0.8.0 起废弃，请使用 UnifiedTaskManager
 * @deprecated-since v0.8.0
 * @deprecated-reason 功能已完全迁移到 UnifiedTaskManager，该类将在下一个大版本中删除
 *
 * 迁移指南:
 * - 使用 UnifiedTaskManager 替代 TaskManager
 * - createTask() → unifiedTaskManager.createTask()
 * - updateTaskStatus() → unifiedTaskManager.startTask() / completeTask() / failTask()
 * - addSubTask() → unifiedTaskManager.createSubTask()
 * - updateSubTaskStatus() → unifiedTaskManager.startSubTask() / completeSubTask() / failSubTask()
 * - updateTaskPlan() → unifiedTaskManager.updateTaskPlan()
 * - updateTaskPlanStatus() → unifiedTaskManager.updateTaskPlanStatus()
 *
 * @see UnifiedTaskManager
 */
export class TaskManager {
  // ...
}
```

### 5.2 更新导出（保持向后兼容）

**文件**: `src/index.ts` 或相关导出文件

**确保 TaskManager 仍可导入**:
```typescript
// 保留导出以保持向后兼容
export { TaskManager } from './task-manager';
```

### 5.3 删除 TaskManager 导入

**检查所有文件**:
```bash
grep -rn "import.*TaskManager" src/ --include="*.ts"
```

**删除所有导入**（除了导出文件）

### 5.4 完整验证

#### 编译检查
```bash
npx tsc --noEmit
```
**预期**: 0 个错误

#### 测试检查
```bash
npm test
```
**预期**: 38/38 测试通过

#### 代码清洁度检查
```bash
# 检查 TaskManager 使用（应该只在 task-manager.ts 和导出文件中）
grep -rn "this.taskManager" src/ --include="*.ts"

# 检查状态同步代码
grep -rn "同步到 TaskManager" src/ --include="*.ts"

# 检查临时标记
grep -rn "TODO.*TaskManager\|FIXME.*TaskManager" src/ --include="*.ts"
```
**预期**: 无遗留使用

#### 性能验证
- [ ] 运行性能测试
- [ ] 对比迁移前后性能
- [ ] 确认性能提升 30-50%

### 5.5 更新文档

**需要更新的文档**:
- [ ] README.md - 更新 API 说明
- [ ] 迁移指南 - 添加 TaskManager 迁移说明
- [ ] API 文档 - 标记 TaskManager 为废弃
- [ ] 完成报告 - 创建 TaskManager 迁移完成报告

### 5.6 创建完成报告

**文件**: `docs/TaskManager迁移完成报告.md`

**内容**:
- 迁移概述
- 修改的文件列表
- 删除的代码统计
- 性能对比
- 测试结果
- 遗留问题（如果有）

---

## 验证清单

### 编译和类型检查
- [ ] TypeScript 编译无错误
- [ ] 无类型断言滥用
- [ ] 所有导入路径正确

### 测试覆盖
- [ ] 所有单元测试通过
- [ ] 所有集成测试通过
- [ ] 测试覆盖率保持或提升

### 代码清洁度
- [ ] 无 TaskManager 实例使用（除了文件本身）
- [ ] 无状态同步代码
- [ ] 无临时标记
- [ ] 无被注释的重要代码

### 架构验证
- [ ] OrchestratorAgent 仅使用 UnifiedTaskManager
- [ ] 无双重状态管理
- [ ] 无状态同步逻辑
- [ ] Plan 管理功能正常工作

### 功能验证
- [ ] Task 创建正常
- [ ] Task 状态更新正常
- [ ] SubTask 创建正常
- [ ] SubTask 状态更新正常
- [ ] Plan 管理正常
- [ ] 所有事件正常触发

### 性能验证
- [ ] 无重复持久化
- [ ] 磁盘 I/O 减少 50%
- [ ] 整体性能提升 30-50%

---

## 风险管理

### 已识别风险

| 风险 | 等级 | 缓解措施 | 状态 |
|------|------|---------|------|
| Plan 管理功能迁移失败 | 中 | 先添加到 UnifiedTaskManager，充分测试 | ⏳ |
| 同步→异步转换导致错误 | 中 | 逐步迁移，每步测试 | ⏳ |
| 破坏现有功能 | 低 | 每个阶段都运行完整测试 | ⏳ |
| 性能下降 | 低 | 性能测试验证 | ⏳ |

### 回滚计划

如果迁移失败：
1. 使用 git 回滚到迁移前的提交
2. 分析失败原因
3. 调整策略后重新开始

---

## 进度跟踪

### Stage 1: Plan 管理功能迁移
- [ ] 1.1 添加方法到 UnifiedTaskManager
- [ ] 1.2 测试 Plan 管理功能
- [ ] 1.3 验证

### Stage 2: OrchestratorAgent 迁移
- [ ] 2.1 分析使用情况
- [ ] 2.2 逐个替换调用
- [ ] 2.3 处理同步→异步转换
- [ ] 2.4 删除 TaskManager 实例
- [ ] 2.5 验证

### Stage 3: 其他文件迁移
- [ ] 3.1 IntelligentOrchestrator
- [ ] 3.2 webview-provider.ts
- [ ] 3.3 orchestrator.ts
- [ ] 3.4 测试文件
- [ ] 3.5 验证

### Stage 4: 删除状态同步代码
- [ ] 4.1 删除状态同步监听器
- [ ] 4.2 验证

### Stage 5: 完整清理和文档
- [ ] 5.1 标记 @deprecated
- [ ] 5.2 更新导出
- [ ] 5.3 删除导入
- [ ] 5.4 完整验证
- [ ] 5.5 更新文档
- [ ] 5.6 创建完成报告

---

## 预期结果

### 代码统计
- **删除代码**: ~300 行
- **删除文件**: 0 个（TaskManager 标记为 @deprecated）
- **修改文件**: 5-7 个
- **新增代码**: ~100 行（Plan 管理功能）

### 性能提升
- **磁盘 I/O**: 减少 50%
- **管理器调用**: 减少 50%
- **整体性能**: 提升 30-50%

### 架构改进
- **单一状态管理**: ✅
- **无状态同步**: ✅
- **清晰的职责**: ✅
- **易于维护**: ✅

---

## 参考文档

1. **TaskManager-vs-UnifiedTaskManager-对比分析.md** - 功能对比
2. **双重系统架构问题图解.md** - 架构图
3. **根本原因分析-为什么UnifiedTaskManager没有完全替代TaskManager.md** - 问题分析
4. **Stage3-完成总结.md** - TaskStateManager 迁移经验

---

**计划版本**: v1.0
**创建时间**: 2026-01-18 15:30
**预计完成时间**: 2026-01-18 18:00
**状态**: 🔄 进行中
