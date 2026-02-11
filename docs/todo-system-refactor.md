# Todo 系统统一改造方案

## 0. 设计约束（来自编码规范）

| 约束 | 规范条款 | 违规后果 |
| :--- | :------- | :------- |
| 禁止多重实现 | 同一功能严禁出现多种实现方式 | TaskView 二重定义、Todo 创建散布 6 处 |
| 禁止回退逻辑 | 严禁保留兼容性分支 | execution-coordinator else 空洞 |
| 禁止打补丁 | 必须追溯根因 | extractDynamicTodos 正则匹配是打补丁 |
| 效率优先 | 自动化高置信度任务 | Worker 应主动追加子 Todo 而非被动等待 |
| 单一数据源 | Todo 操作必须经 TodoManager | handleAdjustment.addSteps 绕过 TodoManager |

---

## 1. 问题全景图

### 1.1 Todo 系统的三个职责

```text
┌─────────────────────────────────────────────────────────────┐
│                    Todo 系统的三个职责                        │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  职责 A：生命周期管理（TodoManager）                          │
│  ├── CRUD + 状态机（pending→ready→running→completed/failed）│
│  ├── 依赖链（dependsOn）、契约（requiredContracts）           │
│  ├── 优先级队列、超时监控、范围审批                            │
│  └── 持久化（FileTodoRepository）                            │
│                                                             │
│  职责 B：Todo 创建（散布在 6 个地方 ← 核心问题）              │
│  ├── PlanningExecutor.createTodoForAssignment()              │
│  ├── DispatchManager.dispatchTask()                          │
│  ├── AutonomousWorker.extractDynamicTodos()                  │
│  ├── AutonomousWorker.addDynamicTodo()                       │
│  ├── AutonomousWorker.handleAdjustment() addSteps            │
│  └── ExecutionCoordinator.execute() else 分支（空洞 Bug）     │
│                                                             │
│  职责 C：状态映射到 UI（二重定义 ← 核心问题）                 │
│  ├── state-mapper.ts → MissionStateMapper（推送路径）         │
│  └── task-view-adapter.ts → missionToTaskView（拉取路径）     │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### 1.2 类型二重定义清单

| 类型名 | state-mapper.ts（推送路径） | task-view-adapter.ts（拉取路径） | 冲突点 |
| :----- | :------------------------ | :----------------------------- | :----- |
| **TaskView** | `{title, description, phase, subTasks: AssignmentView[]}` | `{prompt, goal, sessionId, subTasks: TodoItemView[], missionId}` | **字段完全不同** |
| **TaskViewStatus** | `pending\|running\|completed\|failed\|cancelled\|paused` | 同名，相同成员 | 导入歧义 |
| **SubTaskViewStatus** | `pending\|planning\|running\|blocked\|completed\|failed` | `pending\|running\|paused\|completed\|failed\|skipped\|blocked\|cancelled` | **成员不同** |

### 1.3 实际使用路径分析

```text
推送路径（state-mapper.ts）：
  mission-driven-engine.ts
    └── MissionStateMapper 实例
         └── mapAssignmentToAssignmentView()  ← 唯一被调用的方法
              └── 产出 AssignmentView → 转换为 SubTaskCardPayload → subTaskCard 消息
         ✗ mapMissionToTaskView()      ← 从未被外部调用（死代码）
         ✗ handleMissionUpdate()       ← 从未被外部调用（死代码）
         ✗ mapMissions()               ← 从未被外部调用（死代码）

拉取路径（task-view-adapter.ts）：
  mission-driven-engine.ts
    └── listTaskViews()
         └── missionToTaskView()  ← 产出 TaskView
              └── webview-provider.ts buildUIState() → 转为前端 Task 类型
```

**结论：state-mapper.ts 的 TaskView 是死代码，但 AssignmentView 和 MissionStateMapper 类被实际使用。**

### 1.4 Todo 创建散布的根因分析

**Why-1**：为什么 DispatchManager 要直接调用 todoManager.create()？
→ 因为 dispatch_task 走的是快速路径，不经过 ExecutionCoordinator

**Why-2**：为什么 ExecutionCoordinator 的 else 分支是空洞？
→ 因为开发者假设"简单任务不需要 Todo"，但 Worker 执行需要 Todo

**Why-3**：为什么 extractDynamicTodos 用正则匹配？
→ 因为缺少 Worker 主动追加子 Todo 的清晰机制，只能被动从 LLM 输出中提取

**Why-4**：为什么 handleAdjustment.addSteps 直接 new Todo 对象？
→ 因为 handleAdjustment 是后加的编排者调整机制，开发时未意识到应走 TodoManager

**Why-5**：为什么会有两套视图类型？
→ 因为 state-mapper.ts 先实现了观察者模式，后来 task-view-adapter.ts 又实现了拉取模式，两套并存

**根本原因：系统经历了从旧架构到新架构的渐进迁移，旧组件未完全清理，新组件未完全收敛。**

---

## 2. 改造目标

```text
改造前：                              改造后：
6 个 Todo 创建点                      2 个 Todo 创建点（编排层 + Worker 层）
2 套 TaskView 定义                    1 套 TaskView 定义
1 个 P0 Bug（else 空洞）              0 个 Bug
正则被动提取                          Worker 通过 addDynamicTodo 主动追加二级 Todo
handleAdjustment 绕过 TodoManager     统一走 addDynamicTodo
parentId 无联动语义                   子 Todo 全完成 → 父 Todo 自动 complete
```

### 2.1 两层 Todo 模型

```text
    ┌──────────────────────────────────────────────────────┐
    │                  两层 Todo 模型                        │
    ├──────────────────────────────────────────────────────┤
    │                                                      │
    │  一级 Todo（编排者创建）                                │
    │  ├── 由 PlanningExecutor 创建                         │
    │  ├── 1 个 Assignment 对应 1 个一级 Todo               │
    │  ├── 粒度 = assignment 级别的工作项                    │
    │  └── 状态：running → (等待子 Todo) → completed        │
    │                                                      │
    │  二级 Todo（Worker 创建）                              │
    │  ├── 由 AutonomousWorker.addDynamicTodo() 创建        │
    │  ├── parentId 指向一级 Todo                           │
    │  ├── 粒度 = Worker 执行中发现的子步骤                   │
    │  └── 全部完成后 → 父 Todo 自动 complete               │
    │                                                      │
    │  规则：                                               │
    │  ├── 一级 Todo 无 parentId（顶层）                     │
    │  ├── 二级 Todo 必须有 parentId（子层）                  │
    │  ├── 不允许三级及以上嵌套                               │
    │  └── 编排者通过 OrchestratorAdjustment 追加二级 Todo   │
    │                                                      │
    └──────────────────────────────────────────────────────┘
```

### 2.2 改造后的架构

```text
               ┌──────────────────────────────┐
               │   TodoManager (增强)          │
               │   CRUD + 状态机 + 持久化       │
               │   + tryCompleteParent（新增）  │
               └──────────┬───────────────────┘
                          │
              ┌───────────┴───────────┐
              │                       │
    ┌─────────┴──────────┐  ┌────────┴──────────────────────┐
    │  PlanningExecutor   │  │ AutonomousWorker               │
    │  (一级 Todo 唯一入口)│  │ (二级 Todo 唯一入口)            │
    │                     │  │                                │
    │  ● createMacroTodo  │  │ ● addDynamicTodo(parentId=一级) │
    │  ● planWithLLM      │  │                                │
    └─────────────────────┘  └────────────────────────────────┘
              │                       ↑
    ┌─────────┴────────────────────┐  │ OrchestratorAdjustment
    │ ExecutionCoordinator          │  │ addSteps → addDynamicTodo
    │ 统一调用: mode='macro'|'plan' │  │ skipSteps → todoManager.skip
    │ （无 if/else 分支）           │  │ priorityChanges → 优先级调整
    └──────────────────────────────┘  │
              │                       │
    ┌─────────┴────────────────────┐  │
    │ DispatchManager               │──┘
    │ 委托 PlanningExecutor         │
    │ （自身不再 todoManager.create）│
    └──────────────────────────────┘

    反馈环路（运行时二级 Todo 动态伸缩）：
    ┌───────────────────────────────────────────────────┐
    │ Worker.executeTodo(一级 Todo)                      │
    │   → 执行中发现子步骤 → addDynamicTodo(二级 Todo)    │
    │   → reportProgress() 向编排者汇报                   │
    │     → 编排者返回 OrchestratorResponse               │
    │       action='adjust':                             │
    │         addSteps → addDynamicTodo(二级 Todo)        │
    │         skipSteps → todoManager.skip()              │
    │       action='abort': 终止执行                      │
    │   → 所有二级 Todo 完成 → 一级 Todo 自动 complete     │
    └───────────────────────────────────────────────────┘

    视图层（统一为 1 套）：
    ┌──────────────────────────────────────┐
    │ task-view-adapter.ts (唯一)           │
    │ ● TaskView (UI 拉取)                 │
    │ ● TodoItemView                       │
    │ ● TaskViewStatus / SubTaskViewStatus  │
    └──────────────────────────────────────┘
    ┌──────────────────────────────────────┐
    │ state-mapper.ts (精简)               │
    │ ● MissionStateMapper (保留)          │
    │ ● AssignmentView (保留)              │
    │ ● TodoView (保留)                    │
    │ ✗ TaskView (删除)                    │
    │ ✗ handleMissionUpdate (删除)         │
    │ ✗ mapMissions (删除)                 │
    └──────────────────────────────────────┘
```

---

## 3. 文件级改动清单

### 阶段一：视图类型统一（消除二重定义）

#### 3.1 state-mapper.ts — 删除死代码

**删除项：**

- `TaskView` 接口（未被外部消费）
- `mapMissionToTaskView()` 方法（未被外部调用）
- `handleMissionUpdate()` 方法（未被外部调用）
- `mapMissions()` 方法（未被外部调用）
- `calculateMissionProgress()` 方法（仅被 mapMissionToTaskView 调用）
- `formatPhase()` 方法（仅被 mapMissionToTaskView 调用）
- `mapMissionStatus()` 方法（仅被 mapMissionToTaskView 调用）
- `subscribe()` / `notify()` / `callbacks` / `dispose()` — 观察者机制（未被使用）
- `StateChangeCallback` 类型

**保留项：**

- `AssignmentView` 接口（被 mission-driven-engine.ts 使用）
- `TodoView` 接口（被 AssignmentView.todos 使用）
- `SubTaskViewStatus` 类型（被 AssignmentView.status 使用，可改为私有）
- `TodoViewStatus` 类型（被 TodoView.status 使用，可改为私有）
- `MissionStateMapper` 类（保留 `mapAssignmentToAssignmentView`、`mapTodoToTodoView`、`mapAssignmentStatus`、`mapTodoStatus`、`generateAssignmentSummary`）
- `globalMissionStateMapper` 实例（可删除，mission-driven-engine 自建实例）

**影响文件：**

- `src/orchestrator/mission/index.ts` — 移除 TaskView、TaskViewStatus、StateChangeCallback 的导出

#### 3.2 mission/index.ts — 精简导出

```typescript
// 改造前：
export {
  MissionStateMapper,
  globalMissionStateMapper,
  type TaskView,           // 删除
  type AssignmentView,
  type TodoView,
  type TaskViewStatus,     // 删除
  type SubTaskViewStatus,  // 删除（state-mapper 内部使用）
  type TodoViewStatus,     // 删除（state-mapper 内部使用）
  type StateChangeCallback,// 删除
} from './state-mapper';

// 改造后：
export {
  MissionStateMapper,
  type AssignmentView,
  type TodoView,
} from './state-mapper';
```

### 阶段二：Todo 创建收敛

#### 3.3 planning-executor.ts — 重写为一级 Todo 统一入口

```typescript
// src/orchestrator/core/executors/planning-executor.ts

export interface PlanningOptions {
  projectContext?: string;
  parallel?: boolean;
  contextManager?: ContextManager | null;
  mode: 'macro' | 'plan';
}

export class PlanningExecutor {
  constructor(
    private todoManager: TodoManager,
    private adapterFactory: IAdapterFactory
  ) {}

  async execute(mission: Mission, options: PlanningOptions): Promise<PlanningResult> {
    for (const assignment of mission.assignments) {
      if (options.mode === 'plan') {
        await this.planWithLLM(mission, assignment);
      } else {
        await this.createMacroTodo(mission, assignment);
      }
    }
    return { success: true, errors: [] };
  }

  // 创建一级 Todo（1 个 Assignment = 1 个一级 Todo）
  async createMacroTodo(mission: Mission, assignment: Assignment): Promise<void> {
    const content = this.buildTodoContent(assignment);
    const todo = await this.todoManager.create({
      missionId: mission.id,
      assignmentId: assignment.id,
      content,
      reasoning: assignment.delegationBriefing || assignment.responsibility,
      type: 'implementation',
      workerId: assignment.workerId,
      targetFiles: assignment.scope?.targetPaths,
      // 注意：一级 Todo 不设置 parentId
    });
    this.applyTodoToAssignment(assignment, [todo]);
  }

  // 规划模式：LLM 拆分为多步骤一级 Todo
  private async planWithLLM(mission: Mission, assignment: Assignment): Promise<void> {
    // ...LLM 调用 + 解析 + 降级为 createMacroTodo
  }

  private buildTodoContent(assignment: Assignment): string {
    const targetPaths = assignment.scope?.targetPaths?.length
      ? assignment.scope.requiresModification
        ? `\n目标文件: ${assignment.scope.targetPaths.join(', ')}。必须使用工具直接编辑并保存。`
        : `\n目标文件: ${assignment.scope.targetPaths.join(', ')}。只需读取/分析，不要修改文件。`
      : '';
    return `${assignment.responsibility}${targetPaths}`;
  }

  private applyTodoToAssignment(assignment: Assignment, todos: UnifiedTodo[]): void {
    assignment.todos = todos;
    assignment.planningStatus = 'planned';
    if (assignment.status === 'pending') {
      assignment.status = 'ready';
    }
  }
}
```

#### 3.4 execution-coordinator.ts — 消除 if/else 空洞

```typescript
// 改造前（P0 Bug）：
if (needsPlanning) {
  await this.planningExecutor.execute(this.mission, { ... });
} else {
  logger.info('跳过规划阶段（简单任务）');
  // 什么都没做 ← Bug
}

// 改造后：
const planningResult = await this.planningExecutor.execute(this.mission, {
  projectContext: options.projectContext,
  parallel: options.parallelPlanning,
  contextManager: this.contextManager,
  mode: needsPlanning ? 'plan' : 'macro',
});
```

#### 3.5 dispatch-manager.ts — 委托 PlanningExecutor

```typescript
// 改造前（越权）：
const todoManager = this.deps.missionOrchestrator.getTodoManager();
const todo = await todoManager.create({ ... });
assignment.todos = [todo];
assignment.planningStatus = 'planned';
assignment.status = 'ready';

// 改造后（委托）：
await this.deps.planningExecutor.createMacroTodo(
  { id: batch?.id || 'dispatch' } as Mission,
  assignment
);
```

**DispatchManagerDeps 接口变更：**

```typescript
export interface DispatchManagerDeps {
  // 删除：不再需要通过 missionOrchestrator 间接获取 TodoManager
  // 新增：
  planningExecutor: PlanningExecutor;
}
```

#### 3.6 autonomous-worker.ts — 删除 extractDynamicTodos + 修复 handleAdjustment

**删除：**

- `extractDynamicTodos()` 方法（~35 行）
- `executeTodo()` 中对 `extractDynamicTodos()` 的调用

**修复 handleAdjustment.addSteps（第 6 个散布点）：**

```typescript
// 改造前（绕过 TodoManager，直接 new 对象）：
if (adjustment.addSteps && adjustment.addSteps.length > 0) {
  for (const stepContent of adjustment.addSteps) {
    const todo: UnifiedTodo = {
      id: `adj-${Date.now()}-${Math.random()...}`,  // 手工 ID
      ...
    };
    assignment.todos.push(todo);  // 直接 push，无持久化
  }
}

// 改造后（统一走 addDynamicTodo → TodoManager，创建二级 Todo）：
if (adjustment.addSteps && adjustment.addSteps.length > 0) {
  // 找到当前一级 Todo 作为 parent
  const parentTodo = assignment.todos.find(t => !t.parentId);
  for (const stepContent of adjustment.addSteps) {
    await this.addDynamicTodo(
      assignment,
      stepContent,
      '编排者调整指令添加',
      'implementation',
      parentTodo?.id  // parentId 指向一级 Todo
    );
  }
}
```

**保留：**

- `addDynamicTodo()` — Worker 层唯一的 TodoManager.create() 调用点，创建二级 Todo

#### 3.7 todo-manager.ts — 新增 tryCompleteParent（父子联动）

```typescript
// src/todo/todo-manager.ts

// complete() 方法末尾追加一行：
async complete(todoId: string, output?: TodoOutput): Promise<void> {
  // ...现有逻辑...

  // 触发依赖此 Todo 的其他 Todos 检查
  await this.triggerDependentTodos(todoId);

  // 新增：如果是二级 Todo 完成，检查是否可以自动 complete 一级 Todo
  if (todo.parentId) {
    await this.tryCompleteParent(todo.parentId);
  }
}

/**
 * 检查父 Todo 的所有子 Todo 是否都已完成
 * 如果是，自动将父 Todo 标记为 completed
 */
private async tryCompleteParent(parentId: string): Promise<void> {
  const parent = await this.get(parentId);
  if (!parent || parent.status === 'completed') return;

  const children = await this.repository.query({ parentId });
  if (children.length === 0) return;

  const allDone = children.every(c =>
    c.status === 'completed' || c.status === 'skipped'
  );
  if (!allDone) return;

  // 所有子 Todo 完成，自动 complete 父 Todo
  parent.status = 'running'; // 临时状态，满足 complete() 的前置条件
  await this.repository.save(parent);
  await this.complete(parentId, {
    success: true,
    summary: `所有 ${children.length} 个子步骤已完成`,
    modifiedFiles: children.flatMap(c => c.output?.modifiedFiles || []),
    duration: Date.now() - (parent.startedAt || parent.createdAt),
  });
}
```

**改造后 Todo 操作权分布：**

| 操作 | 入口 | 底层调用 | Todo 层级 |
| :--- | :--- | :------- | :-------- |
| PlanningExecutor.createMacroTodo | 编排层 | todoManager.create | 一级（顶层） |
| PlanningExecutor.planWithLLM | 编排层 | todoManager.create | 一级（顶层） |
| AutonomousWorker.addDynamicTodo | Worker 层 | todoManager.create | 二级（子层） |
| handleAdjustment.addSteps | 编排者调整 | addDynamicTodo | 二级（子层） |
| handleAdjustment.skipSteps | 编排者调整 | todoManager.skip | 任意层级 |
| TodoManager.tryCompleteParent | 自动触发 | complete(parentId) | 一级（自动） |

**todoManager.create() 仅出现在 2 处：**

1. `PlanningExecutor`（createMacroTodo / planWithLLM）→ 创建一级 Todo
2. `AutonomousWorker.addDynamicTodo()` → 创建二级 Todo（handleAdjustment 也委托此方法）

---

## 4. 数据流对比

### 4.1 改造前

```text
dispatch_task:
  DispatchManager → todoManager.create() [散布点1] → Worker
  Worker → extractDynamicTodos() [散布点4，正则被动]
  Worker → handleAdjustment.addSteps [散布点6，绕过 TodoManager]

plan_mission:
  ExecutionCoordinator →
    needsPlanning=true:  PlanningExecutor.createTodoForAssignment() [散布点2]
    needsPlanning=false: 什么都没做 [散布点3，P0 Bug]
  → Worker → extractDynamicTodos() [散布点4]
```

### 4.2 改造后

```text
dispatch_task:
  DispatchManager → PlanningExecutor.createMacroTodo() → 一级 Todo → Worker
  Worker 执行中 → addDynamicTodo(parentId=一级) → 二级 Todo
  所有二级 Todo 完成 → tryCompleteParent() → 一级 Todo 自动 complete

plan_mission:
  ExecutionCoordinator → PlanningExecutor.execute(mode) → 一级 Todo → Worker
    mode='macro': createMacroTodo() → 1 个一级 Todo
    mode='plan':  planWithLLM()    → 多个一级 Todo（降级为 macro）
  → Worker 执行中：
    → addDynamicTodo(parentId=一级) → 追加二级 Todo
  → 编排者反馈（reportProgress → OrchestratorResponse）：
    → action='adjust': addSteps → addDynamicTodo(二级 Todo)
    → action='adjust': skipSteps → todoManager.skip()
    → action='abort': 终止执行
  → 所有二级 Todo 完成 → 一级 Todo 自动 complete
```

---

## 5. 执行顺序

| 步骤 | 改动 | 影响范围 | 风险 |
| :--- | :--- | :------- | :--- |
| **Step 1** | state-mapper.ts 删除死代码 | 1 文件 + index.ts 导出调整 | 低（删除未使用代码） |
| **Step 2** | mission/index.ts 精简导出 | 1 文件 | 低 |
| **Step 3** | PlanningExecutor 重写 + PlanningOptions.mode | 1 文件 | 中（核心逻辑变更） |
| **Step 4** | ExecutionCoordinator 消除 if/else | 1 文件 ~5 行改动 | 低（P0 修复） |
| **Step 5** | DispatchManager 委托 PlanningExecutor | 1 文件 + deps 接口 | 中（依赖链变更） |
| **Step 6** | MissionOrchestrator 适配 PlanningExecutor 新构造函数 | 1 文件 | 低 |
| **Step 7** | AutonomousWorker: 删除 extractDynamicTodos + 修复 handleAdjustment | 1 文件 | 中 |
| **Step 8** | TodoManager: 新增 tryCompleteParent | 1 文件 ~25 行 | 低 |
| **Step 9** | 编译验证 | 全量 | — |

**建议分三批执行：**

- **批次 A（视图统一）**：Step 1-2 → 编译验证
- **批次 B（创建收敛）**：Step 3-6 → 编译验证
- **批次 C（Worker 层 + 父子联动）**：Step 7-8 → 编译验证

---

## 6. 验收标准

### 6.1 类型统一验证

- [ ] `TaskView` 仅在 `task-view-adapter.ts` 中定义（`grep -r "interface TaskView" src/`）
- [ ] `TaskViewStatus` 仅在 `task-view-adapter.ts` 中定义
- [ ] `state-mapper.ts` 不再导出 `TaskView`、`TaskViewStatus`、`StateChangeCallback`

### 6.2 创建收敛验证

- [ ] `todoManager.create` 仅出现在 `PlanningExecutor` 和 `AutonomousWorker.addDynamicTodo()` 中
- [ ] `assignment.todos =` 仅出现在 `PlanningExecutor.applyTodoToAssignment()` 中
- [ ] `DispatchManager` 中无 `todoManager` 引用
- [ ] `extractDynamicTodos` 已删除
- [ ] `handleAdjustment.addSteps` 走 `addDynamicTodo()` 而非直接 new 对象

### 6.3 P0 Bug 修复验证

- [ ] `ExecutionCoordinator.execute()` 无 if/else 分支，统一调用 `PlanningExecutor.execute()`
- [ ] `needsPlanning=false` 时 Worker 能正常执行（assignment.todos 不为空）

### 6.4 两层 Todo 模型验证

- [ ] 一级 Todo 由 PlanningExecutor 创建，无 parentId
- [ ] 二级 Todo 由 addDynamicTodo 创建，parentId 指向一级 Todo
- [ ] 编排者 `addSteps` 调整走 `addDynamicTodo()` 路径，创建二级 Todo
- [ ] 二级 Todo 全部完成后，一级 Todo 通过 `tryCompleteParent()` 自动 complete
- [ ] 编排者 `skipSteps` 调整走 `todoManager.skip()` 路径

### 6.5 回归验证

- [ ] `npx tsc --noEmit` 通过
- [ ] Svelte 前端编译通过
- [ ] `node esbuild.mjs --production` 通过

---

## 7. 风险点

| 风险 | 影响 | 缓解 |
| :--- | :--- | :--- |
| PlanningExecutor 构造函数签名变更 | MissionOrchestrator 和 DispatchManager 需适配 | 通过 deps 接口隔离 |
| DispatchManager 不再直接持有 TodoManager | dispatch_task 流程可能受影响 | 委托 PlanningExecutor，逻辑等价 |
| state-mapper.ts 删除方法后编译失败 | 可能有未发现的引用 | 编译验证兜底 |
| planWithLLM 的 prompt 输出不稳定 | 解析失败 | 降级为 createMacroTodo |
| tryCompleteParent 递归调用 complete | 循环引用风险 | parent.status !== 'completed' 前置检查 |
| handleAdjustment 改造后行为变化 | addSteps 从内存操作变为持久化 | 逻辑等价，增加了持久化保障 |
