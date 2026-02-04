# Unified Todo System Architecture Redesign

## Goal
设计并实现一个统一、健壮的 Todo 系统，合并现有的 `UnifiedTaskManager (Task/SubTask)` 和 `WorkerTodo` 两套重复系统，消除功能冗余，建立单一数据源。

## Requirements (Strict)
1. **No compatibility handling** - 不允许任何兼容性处理
2. **High availability first** - 高可用架构设计为第一优先级
3. **No patch solutions** - 不允许补丁方案
4. **Single source of truth** - 不允许多套同样功能的业务路线

## Current Problem Analysis

### Duplicate Systems Identified:

| Feature | UnifiedTaskManager (SubTask) | WorkerTodo |
|---------|------------------------------|------------|
| Status | SubTaskStatus | TodoStatus |
| Dependencies | dependencies[] | dependsOn[] |
| Priority | priority | priority |
| Retry | retryCount, maxRetries | retryCount |
| Timestamps | startedAt, completedAt | startedAt, completedAt |
| Worker | assignedWorker | (via Assignment.workerId) |

### UnifiedTaskManager 优势:
- 持久化 (TaskRepository)
- 优先级队列 (PriorityQueue)
- 超时管理 (TimeoutChecker)
- LRU 缓存
- 事件驱动

### WorkerTodo 优势:
- 契约系统 (requiredContracts, producesContracts)
- 范围检查 (outOfScope, approvalStatus)
- 类型分类 (discovery, design, implementation, etc.)
- 推理说明 (reasoning, expectedOutput)
- Assignment 关联

---

## Phases

### Phase 1: Architecture Design
**Status**: `complete`
**Goal**: 设计统一的 Todo 数据模型和架构

Tasks:
- [x] 定义统一的 `UnifiedTodo` 接口
- [x] 设计新的状态机
- [x] 规划依赖注入结构
- [x] 确定持久化策略

### Phase 2: Core Type Definitions
**Status**: `complete`
**Goal**: 实现核心类型定义

Tasks:
- [x] 创建 `src/todo/types.ts`
- [x] 定义 `UnifiedTodo`, `TodoStatus`, `TodoType`
- [x] 定义事件类型

### Phase 3: Todo Manager Implementation
**Status**: `complete`
**Goal**: 实现统一的 Todo 管理器

Tasks:
- [x] 创建 `src/todo/todo-manager.ts`
- [x] 实现生命周期管理
- [x] 实现持久化层
- [x] 实现优先级队列
- [x] 实现契约依赖检查

### Phase 4: Integration Layer
**Status**: `complete`
**Goal**: 与现有系统集成

Tasks:
- [x] Mission → UnifiedTodo 适配
- [x] Assignment 简化
- [x] 删除 WorkerTodo 相关代码
- [ ] 删除 UnifiedTaskManager 中的 SubTask (deferred - see decision log)

### Phase 5: Cleanup & Verification
**Status**: `complete`
**Goal**: 清理旧代码，验证功能

Tasks:
- [ ] 删除 `src/task/unified-task-manager.ts` 中的 SubTask 逻辑 (deferred)
- [x] 删除 `src/orchestrator/worker/todo-planner.ts`
- [x] 更新所有引用
- [x] 编译验证
- [ ] 功能测试

---

## Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| Missing UnifiedTodo fields in tests | 1 | Added missionId, workerId, progress, retryCount, maxRetries |
| extractDynamicTodos signature mismatch | 1 | Changed to accept Assignment instead of assignmentId |
| TodoStatus naming inconsistency | 2 | Standardized on 'running' instead of 'in_progress' |

---

## Decision Log
| Decision | Rationale | Date |
|----------|-----------|------|
| 合并 SubTask 和 WorkerTodo | 消除功能重复 | 2026-02-04 |
| 保留 Task 层级 | Mission 级别的组织需要 | 2026-02-04 |
| 使用 UnifiedTaskManager 的基础设施 | 已有成熟的持久化、队列、超时机制 | 2026-02-04 |
| 整合 WorkerTodo 的领域特性 | 契约、范围检查是必要的业务逻辑 | 2026-02-04 |
| 保留 SubTask in UnifiedTaskManager | SubTask 用于 UI 任务跟踪，UnifiedTodo 用于 Mission 执行，不同抽象层级 | 2026-02-04 |
| 使用 'running' 而非 'in_progress' | 保持所有状态命名格式统一化 | 2026-02-04 |
