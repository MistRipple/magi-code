# Research Findings - Unified Todo Architecture

## 1. Current Architecture Analysis

### 1.1 UnifiedTaskManager (src/task/unified-task-manager.ts)

**Data Model:**
```typescript
Task {
  id, sessionId, prompt, missionId
  status: TaskStatus (pending|running|paused|completed|failed|cancelled|retrying)
  priority, subTasks[], timeout, retryCount, maxRetries
  planId?, planSummary?, planStatus?
}

SubTask {
  id, taskId, description, title
  assignmentId, assignedWorker, reason, prompt
  targetFiles[], modifiedFiles[], dependencies[]
  priority, kind, background
  status: SubTaskStatus (pending|running|paused|completed|failed|skipped|cancelled|retrying)
  progress, output[], result?, error?
  timeout, retryCount, maxRetries
}
```

**Infrastructure:**
- `TaskRepository` - 持久化层
- `PriorityQueue` - 优先级调度
- `TimeoutChecker` - 超时管理
- LRU Cache - 内存缓存
- EventEmitter - 事件通知

### 1.2 WorkerTodo (src/orchestrator/mission/types.ts)

**Data Model:**
```typescript
WorkerTodo {
  id, assignmentId
  content, reasoning, expectedOutput
  type: TodoType (discovery|design|implementation|verification|integration|fix|refactor)
  priority, outOfScope
  approvalStatus?: pending|approved|rejected
  approvalNote?, dependsOn[], blockedReason?
  requiredContracts[], producesContracts[]
  status: TodoStatus (pending|blocked|in_progress|completed|failed|skipped)
  output?: TodoOutput
  retryCount?, createdAt, startedAt?, completedAt?
}

TodoOutput {
  success, summary, modifiedFiles[]
  newContracts?, issues?, error?, duration, tokenUsage?
}
```

**Features:**
- 契约依赖 (requiredContracts, producesContracts)
- 范围检查 (outOfScope, approvalStatus)
- 推理说明 (reasoning, expectedOutput)
- 类型分类 (TodoType)

### 1.3 TodoPlanner (src/orchestrator/worker/todo-planner.ts)

**功能:**
- `planTodos()` - 为 Assignment 生成 Todo 规划
- `createTodo()` - 创建单个 Todo
- `addDynamicTodo()` - 动态添加 Todo
- `updateTodoStatus()` - 更新状态
- `revisePlan()` - 修订规划
- `validatePlan()` - 验证规划完整性
- `detectCyclicDependencies()` - 检测循环依赖

---

## 2. Feature Comparison Matrix

| Feature | SubTask | WorkerTodo | Unified (Proposed) |
|---------|---------|------------|-------------------|
| ID | id | id | id |
| Parent Link | taskId | assignmentId | missionId, assignmentId |
| Content | description, prompt | content, reasoning | content, reasoning |
| Worker | assignedWorker | (via Assignment) | workerId |
| Type | kind (string) | type (TodoType) | type (TodoType) |
| Priority | priority (1-10) | priority (1-5) | priority (1-5) |
| Status | SubTaskStatus (8) | TodoStatus (6) | UnifiedTodoStatus (7) |
| Dependencies | dependencies[] | dependsOn[] | dependsOn[] |
| **Contracts** | ❌ | requiredContracts[], producesContracts[] | ✅ |
| **Scope Check** | ❌ | outOfScope, approvalStatus | ✅ |
| **Timeout** | timeout, timeoutAt | ❌ | ✅ |
| **Retry** | retryCount, maxRetries | retryCount | ✅ |
| **Output** | output[], result | output: TodoOutput | ✅ |
| **Progress** | progress (0-100) | ❌ | progress (0-100) |
| **Persistence** | TaskRepository | ❌ (in-memory) | TodoRepository |
| **Queue** | PriorityQueue | ❌ | PriorityQueue |

---

## 3. Proposed Unified Architecture

### 3.1 New Data Model

```typescript
// src/todo/types.ts

export type UnifiedTodoType =
  | 'discovery'      // 探索/调研
  | 'design'         // 设计/规划
  | 'implementation' // 实现
  | 'verification'   // 验证/测试
  | 'integration'    // 集成
  | 'fix'            // 修复
  | 'refactor';      // 重构

export type UnifiedTodoStatus =
  | 'pending'     // 等待执行
  | 'blocked'     // 被阻塞（依赖/契约）
  | 'ready'       // 就绪（可执行）
  | 'running'     // 执行中
  | 'completed'   // 完成
  | 'failed'      // 失败
  | 'skipped';    // 跳过

export interface UnifiedTodo {
  // === 标识 ===
  id: string;
  missionId: string;
  assignmentId: string;

  // === 内容 ===
  content: string;           // 任务描述
  reasoning: string;         // 推理说明
  expectedOutput?: string;   // 预期产出
  prompt?: string;           // Worker 执行 prompt

  // === 分类 ===
  type: UnifiedTodoType;
  workerId: WorkerSlot;
  priority: number;          // 1-5, 1 最高

  // === 依赖管理 ===
  dependsOn: string[];       // 依赖的 Todo ID
  requiredContracts: string[]; // 依赖的契约
  producesContracts: string[]; // 产生的契约
  blockedReason?: string;

  // === 范围检查 ===
  outOfScope: boolean;
  approvalStatus?: 'pending' | 'approved' | 'rejected';
  approvalNote?: string;

  // === 状态 ===
  status: UnifiedTodoStatus;
  progress: number;          // 0-100

  // === 超时与重试 ===
  timeout?: number;
  timeoutAt?: number;
  retryCount: number;
  maxRetries: number;

  // === 执行结果 ===
  output?: TodoOutput;
  error?: string;
  modifiedFiles?: string[];

  // === 时间戳 ===
  createdAt: number;
  startedAt?: number;
  completedAt?: number;
}
```

### 3.2 Architecture Layers

```
┌─────────────────────────────────────────────────┐
│                  Mission Layer                   │
│  (Mission → Assignment → UnifiedTodo)           │
└─────────────────────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────┐
│              TodoManager (New)                   │
│  - CRUD operations                              │
│  - Status transitions                           │
│  - Contract dependency resolution               │
│  - Scope validation                             │
└─────────────────────────────────────────────────┘
                        │
        ┌───────────────┼───────────────┐
        ▼               ▼               ▼
┌─────────────┐ ┌─────────────┐ ┌─────────────┐
│ TodoQueue   │ │ TodoRepo    │ │ TimeoutMgr  │
│ (Priority)  │ │ (Persist)   │ │ (Monitor)   │
└─────────────┘ └─────────────┘ └─────────────┘
```

### 3.3 Key Changes

1. **删除 SubTask** - 用 UnifiedTodo 替代
2. **删除 WorkerTodo** - 合并到 UnifiedTodo
3. **删除 TodoPlanner** - 功能集成到 TodoManager
4. **Task 保留** - 作为 Mission 的外部跟踪记录
5. **Assignment 简化** - 只保留职责定义，不再包含 todos[]

---

## 4. Migration Strategy

### 删除的文件:
- `src/orchestrator/worker/todo-planner.ts`
- `src/task/types.ts` 中的 SubTask 相关类型

### 新增的文件:
- `src/todo/types.ts` - 统一类型定义
- `src/todo/todo-manager.ts` - 统一管理器
- `src/todo/todo-repository.ts` - 持久化层
- `src/todo/todo-queue.ts` - 优先级队列

### 修改的文件:
- `src/orchestrator/mission/types.ts` - 删除 WorkerTodo
- `src/task/unified-task-manager.ts` - 删除 SubTask 逻辑
- `src/orchestrator/core/mission-driven-engine.ts` - 使用新 TodoManager

---

## 5. Open Questions

1. **Task 层级是否保留?**
   - 决定：保留，用于 Mission 级别的外部跟踪

2. **契约验证时机?**
   - 决定：在 status 从 pending → ready 转换时检查

3. **持久化粒度?**
   - 决定：每个 Todo 独立持久化，不依赖 Mission
