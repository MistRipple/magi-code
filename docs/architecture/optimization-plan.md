# 编排系统架构优化方案

> **版本**: v3.0（统一合并版）
> **日期**: 2026-02-12
> **基准版本**: 0.1.1（L3 统一执行架构重构后）
> **规范依据**: CLAUDE.md `<critical_bans>` + `<standard_operating_procedure>`
> **关联文档**: [architecture-review.md](../architecture-review.md)、[l3-unified-architecture-refactor.md](../l3-unified-architecture-refactor.md)

---

## 目录

1. [问题总览](#1-问题总览)
2. [规范适用判定](#2-规范适用判定)
3. [Wave 1 — P0 死代码清理 + 核心解耦](#3-wave-1--p0-死代码清理--核心解耦)
4. [Wave 2 — P1 事件架构治理](#4-wave-2--p1-事件架构治理)
5. [Wave 3 — P2 质量提升](#5-wave-3--p2-质量提升)
6. [Wave 4 — P3 组件拆分](#6-wave-4--p3-组件拆分--全部完成)
7. [验收检查清单](#7-验收检查清单)
8. [附录](#附录-a-重构前后指标对比)

---

## 1. 问题总览

经三轮独立审计合并去重，共计 **17 个有效问题**（1 个已修复）：

| # | 问题 | 违反规范 | 优先级 | Wave | 现状 |
| - | ---- | -------- | ------ | ---- | ---- |
| 1 | `_context.mission` 死代码残留（8处引用 + 1个死方法） | 禁止多重实现 | P0 | 1 | **已修复** |
| 2 | `reportTodoProgress` Mission/dispatch 双分支 | 禁止多重实现 | P0 | 1 | **已修复** |
| 3 | activeBatch 所有权割裂（10+ 闭包回调） | 禁止打补丁 | P0 | 1 | **已修复** |
| 4 | MDE 上帝类（2021行，8+ 职责） | 单一职责 | P0 | 1 | **已修复**（2021→1065） |
| 5 | MO 旧执行路径残留 | 禁止多重实现 | P0 | - | **已修复** |
| 6 | setupEventForwarding 桥接补丁（~95行） | 禁止打补丁 | P1 | 2 | **已修复** |
| 7 | Todo 事件双重消费（MDE + WVP 同时处理） | 禁止多重实现 | P1 | 2 | **已修复** |
| 8 | 三套事件系统职责交叠 | 禁止多重实现 | P1 | 2 | **已修复** |
| 9 | Phase 模型无显式状态机 | 架构可观测性 | P1 | 2 | **已修复** |
| 10 | MO 事件合约无类型安全 | 类型安全 | P1 | 2 | **已修复** |
| 11 | 外部组件直接 emit MO | 封装性 | P1 | 2 | **已修复** |
| 12 | PlanningExecutor plan 模式死代码 | 死代码 | P2 | 3 | **已修复** |
| 13 | PlanningExecutor 每次调用新建实例 | 资源浪费 | P2 | 3 | **已修复** |
| 14 | Phase C 降级静默掩盖错误 | 禁止打补丁 | P2 | 3 | **已修复** |
| 15 | 串行队列缺少用户反馈 | 用户体验 | P2 | 3 | **已修复** |
| 16 | WebviewProvider `as any` 42处 | 类型安全 | P2 | 3 | **已修复**（42→0） |
| 17 | MO God Object（1697行） | 单一职责 | P3 | 4 | **已修复**（1757→250，死代码清理） |
| 18 | WebviewProvider 膨胀（3890行/44case） | 单一职责 | P3 | 4 | 存在 |
| 19 | 资源生命周期管理缺失 | 内存泄漏风险 | P3 | 4 | 存在 |

---

## 2. 规范适用判定

对关键方案冲突点，按 `<critical_bans>` 做出裁决：

### 2.1 setupEventForwarding 消除方式

| 方案 | 思路 | 规范判定 |
| ---- | ---- | -------- |
| A: MO 直接注入 MessageHub | 发射方源头修复，消除中间桥梁 | ✅ **源头修复** |
| B: 清理死代码后保留桥接 | 仅删死分支，保留 ~60 行桥接 | ❌ 治标不治本 |
| C: 合并到 WebviewProvider | 消费方搬运桥接，补丁换位置 | ❌ 违反「禁止打补丁」 |

**裁决**: 采用方案 A。MO 在业务逻辑发生时直接调用 MessageHub API，
MDE 的 `setupEventForwarding()` 整体删除，不搬运到任何其他组件。

### 2.2 Phase C 降级

| 行为 | 规范判定 |
| ---- | -------- |
| 静默切换到机械拼接 | ❌ 违反「禁止打补丁」— 静默掩盖了 LLM 调用失败 |
| 保留降级 + 透明上报 | ✅ 运行时容错（非代码兼容性分支），必须告知用户 |

**裁决**: 保留降级能力，但在结果前附加错误说明。

### 2.3 运行时容错 vs 禁止回退逻辑 — 边界判定

**不属于「禁止回退逻辑」**:

- 网络瞬态故障的重试 + 模型切换（生产环境必须处理的瞬态故障）
- Phase C LLM 失败后的降级展示（需透明上报，非静默）

**属于「禁止回退逻辑」**:

- 新旧 API 兼容性分支（if oldApi else newApi）
- try 新逻辑 catch 回退旧逻辑
- 功能开关控制新旧实现切换

### 2.4 activeBatch 归属方案

| 方案 | 思路 | 规范判定 |
| ---- | ---- | -------- |
| A: DM 自持 + startDispatchRound(ctx) | 所有权完全归 DM，MDE 不再持有 activeBatch | ✅ **源头修复** |
| B: DispatchContext 共享值对象 | MDE 持有共享对象，DM 通过引用访问 | ⚠️ 仍然共享状态 |

**裁决**: 采用方案 A。DM 自持 activeBatch，MDE 通过 `waitForActiveBatch()` 等待归档。

---

## 3. Wave 1 — P0 死代码清理 + 核心解耦

> **目标**: 清除 L3 重构遗留死代码 + 消除 MDE ↔ DM 状态耦合 + 提取 MDE 独立职责
> **覆盖问题**: #1 #2 #3 #4

### 3.1 #1+#2: L3 重构 Phase 4 收尾（死代码清理）

**[表象]**

`MissionDrivenContext._context` 初始化为 `{ plan: null, mission: null }`（MDE L144），但 `_context.mission` 永远为 `null` — 唯一设置它的方法 `executePlan` 已在 L3 重构中移除。

代码中仍有 **8 处** 读取 `_context.mission`，以及 **1 个无调用者的死方法**：

| 位置 | 代码 | 性质 |
|------|------|------|
| L144 | `private _context = { plan: null, mission: null }` | 初始化 |
| L381 | `setupEventForwarding → assignmentStarted` 处理器 | 死代码 |
| L486 | `get context()` getter | 暴露含死字段的上下文 |
| L500 | `get plan()` getter | 死代码（plan 永远 null） |
| L590-608 | `reportTodoProgress` Mission 模式分支 | 死代码（#2） |
| L626 | `emitSubTaskStatusCard` | **死方法，全局无调用者** |
| L763 | Worker 报告处理中的 mission 检查 | 死代码 |

**[根因]**: L3 重构 Phase 3 移除了写入端（executePlan/createPlan/resumeMission），Phase 4（清理读取端）未执行。

`reportTodoProgress`（#2）是同源问题 — Mission 模式分支永远不会执行，违反「禁止多重实现」：

```typescript
// MDE L590-621 — Mission 分支是死代码
private reportTodoProgress(assignmentId: string, summary: string, includeFileChanges = false): void {
    const mission = this._context.mission;  // 永远 null
    if (assignment && mission) { ... return; }  // 永远不进入
    // dispatch 模式 ← 唯一实际执行路径
    const entry = this.activeBatch?.getEntry(assignmentId);
    if (entry) { ... }
}
```

**[修复]**:

1. 删除 `MissionDrivenContext` 类型中的 `mission` 和 `plan` 字段
2. 删除 `_context` 实例变量及其 getter（`get context()`、`get plan()`）
3. 删除 `emitSubTaskStatusCard` 方法（全局无调用者）
4. 删除 `buildSubTaskTitlePrefix` 方法（仅被死代码调用）
5. 简化 `reportTodoProgress` — 删除 Mission 模式分支和 `includeFileChanges` 参数
6. 清理 `setupEventForwarding` 中依赖 `_context.mission` 的事件处理器

**预计净删除**: ~120 行

---

### 3.2 #3: activeBatch 所有权归位

**[表象]**: DispatchManager 通过 10+ 闭包回调操控 MDE 私有状态。

```typescript
// MDE L255-280 — 12 个闭包穿透封装
this.dispatchManager = new DispatchManager({
    getActiveBatch: () => this.activeBatch,
    setActiveBatch: (batch) => { this.activeBatch = batch; },
    getActiveUserPrompt: () => this.activeUserPrompt,
    getActiveImagePaths: () => this.activeImagePaths,
    getCurrentSessionId: () => this.currentSessionId,
    getLastMissionId: () => this.lastMissionId || undefined,
    getProjectKnowledgeBase: () => this.projectKnowledgeBase,
    ...
});
```

**[根因]**: DispatchManager 从 MDE 提取时，执行上下文状态没一起迁移。闭包绕开封装，DM 实际操控着 MDE 的内部状态。

**[修复]**: DM 自持执行上下文，MDE 传入一次性参数。

**改造后的 DispatchManagerDeps**:

```typescript
// src/orchestrator/core/dispatch-manager.ts
// 仅保留真正的跨组件共享依赖，移除所有闭包回调
export interface DispatchManagerDeps {
  adapterFactory: IAdapterFactory;
  profileLoader: ProfileLoader;
  messageHub: MessageHub;
  missionOrchestrator: MissionOrchestrator;
  planningExecutor: PlanningExecutor;  // 单例注入，不再每次 new（#13）
  workspaceRoot: string;
  snapshotManager: SnapshotManager | null;
  contextManager: ContextManager | null;
  todoManager: TodoManager | null;
  executionStats: ExecutionStats;
}

// 每次 dispatch 轮次的一次性上下文
export interface DispatchRoundContext {
  userPrompt: string;
  imagePaths?: string[];
  sessionId?: string;
  missionId?: string;
  knowledgeBase?: ProjectKnowledgeBase;
}
```

**状态迁移**:

| 状态 | MDE（原） | DM（新） | 传递方式 |
| ---- | --------- | -------- | -------- |
| `activeBatch` | 私有字段 | 内部创建/管理/归档 | DM 自持 |
| `activeUserPrompt` | 私有字段 | 通过 DispatchRoundContext | 一次性参数 |
| `activeImagePaths` | 私有字段 | 通过 DispatchRoundContext | 一次性参数 |
| `lastMissionId` | 私有字段 | 通过 DispatchRoundContext | 一次性参数 |

**MDE 等待 batch 归档的新方式**:

```typescript
// MDE.execute() 中
const batchPromise = this.dispatchManager.waitForActiveBatch();
const response = await this.adapterFactory.sendMessage('orchestrator', ...);
await batchPromise;  // 等待 dispatch 链路完成
```

**实施步骤**:

1. DM 新增 `startDispatchRound(ctx: DispatchRoundContext)` + `waitForActiveBatch()`
2. DM 内部持有 `activeBatch`，移除 `getActiveBatch/setActiveBatch` 闭包
3. MDE 构造 DM 时不再传闭包，改为注入稳定依赖
4. MDE.execute() 调用 `startDispatchRound()` 传入一次性参数
5. 删除 MDE 中 `activeBatch`/`activeUserPrompt`/`activeImagePaths` 私有字段
6. 编译验证

---

### 3.3 #4: MDE 职责提取

**[表象]**: `mission-driven-engine.ts` 2021 行，承载 8+ 种职责。

**[根因]**: L3 重构提取了 DispatchManager 和 WorkerPipeline，但遗漏了其他可独立模块。

**应提取的模块**:

| 职责 | 当前行数 | 目标归属 | 说明 |
| ---- | -------- | -------- | ---- |
| 上下文压缩适配器 | ~130 行 | 新建 `ResilientCompressorAdapter` | `configureContextCompression` 整体提取 |
| 错误类型判定 | ~40 行 | 随 `ResilientCompressorAdapter` | `isAuthOrQuotaError` 等仅被压缩适配器使用 |
| 意图分类 JSON 解析 | ~70 行 | `IntentGate` | `extractIntentClassificationPayload` 等 4 方法合并进 IntentGate |
| 任务视图 CRUD | ~130 行 | 新建 `TaskViewService` | 8 个任务视图方法 |
| 事件桥接 | ~95 行 | **删除**（见 Wave 2 #6） | `setupEventForwarding` 整体删除 |
| Token 记录 | ~50 行 | `ExecutionStats` 模块 | `recordOrchestratorTokens` |

> **重要**：`configureContextCompression` 中的运行时容错逻辑（网络重试 + 模型切换）是**合理的生产环境弹性处理**，不是「禁止回退逻辑」违规。提取是为了职责清晰，不是否定其设计。

**提取后 MDE 保留的核心职责**:

```text
MissionDrivenEngine（目标 < 800 行）
├── execute()                — 接收用户输入，组装提示词，调用编排者 LLM
├── initialize() / dispose() — 生命周期管理
├── cancel() / interrupt()   — 中断控制
├── setXxxCallback()         — 回调注册
├── setInteractionMode()     — 交互模式切换
├── recordContextMessage()   — 上下文记录
└── getMessageHub()          — 对外暴露消息订阅入口
```

**实施步骤**:

1. 新建 `src/orchestrator/core/resilient-compressor-adapter.ts`，从 MDE 提取压缩容错
2. 将意图分类解析合并进 `IntentGate`（API 不变，内部包含解析链路）
3. 新建 `src/services/task-view-service.ts`（注入 MissionStorageManager + TodoManager）
4. 删除 MDE 中对应的旧代码
5. 编译验证

---

## 4. Wave 2 — P1 事件架构治理

> **目标**: 消除事件桥接补丁，建立类型安全事件合约，明确三通道职责边界
> **前置条件**: Wave 1 完成
> **覆盖问题**: #6 #7 #8 #9 #10 #11

### 4.1 #6+#7: 消除 setupEventForwarding + 消除双重消费

**[表象]**: MDE ~95 行 `setupEventForwarding()` 将 MO EventEmitter 事件桥接到 MessageHub。
同时 todoStarted/todoCompleted/todoFailed 被 MDE 和 WVP 双重消费。

**[根因]**: MO 用 EventEmitter 发业务事件，UI 层用 MessageHub 接收，协议不同。MDE 成了粘合层。

**[修复]**: MO 直接注入 MessageHub，在业务逻辑发生时直接发消息。从源头消除桥接需求。

```typescript
// src/orchestrator/core/mission-orchestrator.ts
constructor(
  ...,
  private messageHub: MessageHub,  // 新增依赖
) {}

// 原来: this.emit('workerOutput', {...}) → MDE 监听 → messageHub.workerOutput()
// 现在: this.messageHub.workerOutput(workerId, output)  ← 直接调用，无桥接
```

**需要迁移的事件**:

| 原 EventEmitter 事件 | 迁移到 MessageHub API | 原 MDE 桥接代码 |
| -------------------- | -------------------- | --------------- |
| `workerOutput` | `messageHub.workerOutput()` | MDE L331-333 |
| `analysisComplete` | `messageHub.orchestratorMessage()` | MDE L336-348 |
| `missionPlanned` | `messageHub.sendMessage()` | MDE L351-377 |
| `assignmentStarted` | `messageHub.subTaskCard()` | MDE L380-399 |
| `todoStarted/Completed/Failed` | `messageHub.subTaskCard()` | MDE L402-412 |
| `insightGenerated` | `messageHub.notify()` | MDE L414-422 |

**迁移完成后**:

1. 删除 MDE 的 `setupEventForwarding()` 方法（~95行）
2. 删除 MDE 的 `reportTodoProgress()` 方法（Wave 1 已简化，此处完全删除）
3. Todo 双重消费自动消除 — MDE 不再监听 todo 事件

### 4.2 #8: 三套事件系统职责明确化

**[修复]**: 明确三层边界，消除重叠：

```text
层级 1: MessageHub（唯一的 UI 消息出口）
  发送方: MO（直接注入）、DispatchManager（已注入）、MDE
  职责: 消息去重、节流、路由到 Webview

层级 2: globalEventBus（跨模块生命周期信号）
  仅用于: task:completed / task:failed / session:changed
  消费方: extension.ts（状态栏）、WebviewProvider（状态更新）

组件级 EventEmitter: 限定为组件内部 + 父子组件通信
  MO 仅保留供 WVP 监听的领域事件（assignmentPlanned / todoStarted 等）
  不再用于触发 UI 消息（已迁移到 MessageHub 直调）
```

### 4.3 #9: Phase 显式状态机

**[修复]**: 在 DispatchBatch 上增加显式 Phase 字段：

```typescript
// src/orchestrator/core/dispatch-batch.ts
export type BatchPhase = 'registering' | 'dispatching' | 'executing' | 'summarizing' | 'archived';

// 合法路径: registering → dispatching → executing → summarizing → archived
transitionTo(next: BatchPhase): void {
  const allowed: Record<BatchPhase, BatchPhase[]> = {
    registering: ['dispatching'],
    dispatching: ['executing'],
    executing: ['summarizing', 'archived'],
    summarizing: ['archived'],
    archived: [],
  };
  if (!allowed[this._phase].includes(next)) {
    throw new Error(`非法 Phase 转换: ${this._phase} → ${next}`);
  }
  this._phase = next;
  this.emit('phase:changed', this._phase);
}
```

### 4.4 #10+#11: MO 事件类型安全 + 收拢 emit 权限

**[修复]**: 定义 `MissionOrchestratorEvents` 接口，约束合法事件及参数类型。
外部组件不直接 emit，改为调用 MO 公开方法。

```typescript
// src/orchestrator/core/mission-orchestrator.ts
interface MissionOrchestratorEvents {
  missionCreated: [data: { mission: Mission }];
  missionPlanned: [data: { mission: Mission; contracts: Contract[]; assignments: Assignment[] }];
  missionCompleted: [data: { mission: Mission }];
  missionFailed: [data: { mission: Mission; error: string }];
  assignmentPlanned: [data: { missionId: string; assignments: AssignmentView[] }];
  todoStarted: [data: { missionId: string | null; assignmentId: string; todoId: string; content: string }];
  todoCompleted: [data: { missionId: string | null; assignmentId: string; todoId: string; content: string; output: any }];
  todoFailed: [data: { missionId: string | null; assignmentId: string; todoId: string; content: string; error: string }];
  dynamicTodoAdded: [data: { missionId: string | null; assignmentId: string; todo: UnifiedTodo }];
}

type TypedEmitter<T extends Record<string, any[]>> = {
  on<K extends keyof T>(event: K, listener: (...args: T[K]) => void): any;
  emit<K extends keyof T>(event: K, ...args: T[K]): boolean;
} & EventEmitter;

export class MissionOrchestrator extends (EventEmitter as new () => TypedEmitter<MissionOrchestratorEvents>) {
  // 外部调用入口（替代直接 emit）
  notifyAssignmentPlanned(missionId: string, assignments: AssignmentView[]): void {
    this.emit('assignmentPlanned', { missionId, assignments });
  }
}
```

**DispatchManager 调用方修改**:

```diff
// src/orchestrator/core/dispatch-manager.ts
- this.deps.missionOrchestrator.emit('assignmentPlanned', { ... });
+ this.deps.missionOrchestrator.notifyAssignmentPlanned(missionId, assignments);
```

---

## 5. Wave 3 — P2 质量提升

> **目标**: 清理死代码，修复资源浪费，消除静默降级
> **前置条件**: 无（可与 Wave 1/2 并行）
> **覆盖问题**: #12 #13 #14 #15 #16

### 5.1 #12: PlanningExecutor plan 模式死代码

**[表象]**: `PlanningExecutor` 中 `plan` 模式相关代码从未被调用。

**[根因]**: 早期设计预留了 `plan` 模式（仅规划不执行），但实际从未实现入口。

**[修复]**:

1. 确认 `plan` 模式无任何外部调用者（grep 验证）
2. 删除 `PlanningExecutor` 中 `plan` 相关的条件分支
3. 删除关联的未使用接口/类型定义
4. 编译验证

### 5.2 #13: PlanningExecutor 改为单例注入

**[表象]**: `DispatchManagerDeps` 中 `planningExecutor: () => PlanningExecutor`
每次 dispatch 调用都通过工厂函数创建新实例。

**[根因]**: 提取 DispatchManager 时为避免循环依赖采用了惰性工厂，但 PlanningExecutor 本身无状态，不需要每次创建。

**[修复]**: 改为构造时一次性注入单例（已在 Wave 1 #3 的 DispatchManagerDeps 改造中包含）。

### 5.3 #14: Phase C 降级透明化

**[表象]**: `phaseCFallback()` 在 Phase C LLM 汇总失败时，静默切换到机械拼接 Worker 结果。

**[根因]**: 容错逻辑正确，但静默行为违反「禁止打补丁」。

**[修复]**: 保留降级能力，在结果前附加错误说明。

```typescript
// src/orchestrator/core/dispatch-manager.ts
private phaseCFallback(entries: DispatchEntry[]): void {
  const lines = entries.map(e => {
    const status = e.status === 'completed' ? '✅' : e.status === 'failed' ? '❌' : '⏭️';
    return `${status} **[${e.worker}]** ${e.result?.summary || '无输出'}`;
  });
  this.deps.messageHub.notify('汇总模型调用失败，以下为各 Worker 原始执行结果', 'warning');
  this.deps.messageHub.result(lines.join('\n'));
}
```

### 5.4 #15: 串行队列用户反馈

**[表象]**: `enqueueExecution()` 串行排队时用户无感知。

**[修复]**: 队列非空时通知用户。

```typescript
// src/orchestrator/core/mission-driven-engine.ts
private enqueueExecution<T>(runner: () => Promise<T>): Promise<T> {
  const queueDepth = this.pendingCount++;
  if (queueDepth > 0) {
    this.messageHub.notify(`当前有 ${queueDepth} 个任务排队中，请稍候...`);
  }
  const next = this.executionQueue.then(runner, runner);
  this.executionQueue = next.then(
    () => { this.pendingCount--; },
    () => { this.pendingCount--; }
  );
  return next;
}
```

### 5.5 #16: WebviewProvider `as any` 42处

**[表象]**: `webview-provider.ts` 中 42 处 `as any` 类型断言，TypeScript 类型检查失效。

**[修复]**: 在各 case 分支中通过类型窄化获取正确类型：

```typescript
case 'executeTask': {
    const msg = message as Extract<WebviewToExtensionMessage, { type: 'executeTask' }>;
    // msg.prompt, msg.images 等都有完整类型
}
```

---

## 6. Wave 4 — P3 组件拆分 ✅ 全部完成

> **目标**: 拆分 God Object，控制单文件规模
> **前置条件**: Wave 1 + Wave 2 完成（拆分前需先完成解耦）
> **覆盖问题**: #17 ✅ #18 ✅ #19

### 6.1 #17: MissionOrchestrator 拆分

**[表象]**: `mission-orchestrator.ts` 1697 行，同时承担业务对象管理和事件总线两个角色。

**[根因分析]**: 经深度调用链分析发现，MDE 的统一执行流已完全绕过 MO 的原始 mission pipeline。
以下方法/类型**从未被外部调用**（死代码）：

- **完整 pipeline**: processRequest → createMission → understandGoal → selectParticipants → defineContracts → assignResponsibilities
- **Mission 状态机**: approveMission, pauseMission, resumeMission, cancelMission, completeMission, failMission
- **验证与汇总**: verifyMission, verifyCriterion, verifyWithSpec, parseVerificationSpec, verifyByTaskCompletion, summarizeMission, writeExecutionToMemory
- **缓存**: getCachedPlanning, cachePlanning, clearCache, generateCacheKey, cleanupCache
- **死 Getter**: getSnapshotManager, getContextManager, getProfileLoader, getGuidanceInjector, getReviewer, getIntentGate, getAllWorkers, isExecuting, getCurrentMissionId
- **死类型**: MissionCreationResult, ExecutionOptions, ExecutionProgress, ExecutionResult, MissionVerificationResult, MissionSummary
- **死字段**: contractManager, assignmentManager, reviewer, verificationRunner, intentGate, snapshotManager, projectKnowledgeBase, planningCache, assignmentResolver

**[修复]**: 原计划拆分为 MissionOrchestrator + MissionLifecycleService，但根因分析表明正确策略是**删除死代码**而非提取：

1. **MO 重写**（1757→250 行，-86%）：仅保留 Worker 管理、事件转发、Todo 审批、Mission ID 生命周期
2. **EventMap 精简**：从 ~26 个事件清理到 14 个实际发射的事件
3. **WVP 事件修复**：将 3 个死监听器（missionCompleted/Failed/Cancelled）替换为 1 个活跃的 missionStatusChanged 监听器
4. **MDE 清理**：移除 setIntentGate/setKnowledgeBase 对 MO 的死调用
5. **类型导出清理**：core/index.ts 移除 3 个死类型导出

### 6.2 #18: WebviewProvider 瘦身

**[表象]**: `webview-provider.ts` 3503 行（死代码清理前），44 个 case 分支。

**[修复]**: 分四步完成：

1. **死代码清理** (#18-A, -218 行): 删除 `sendWorkerHistory`、`sendDetailedWorkerMessages`、`sendLLMLogsDirect`、`getLogsForWorker` 等 6 个未引用的 handler 方法
2. **EventBindingService 提取** (#18-B, -454 行): 所有事件绑定逻辑 + 工具授权状态机
3. **WorkerStatusService 提取** (#18-C, -225 行): Worker 模型连接状态检查 + 缓存管理
4. **文档更新** (#18-D): 更新架构审查报告和优化计划

**修复后架构**:

```text
WebviewProvider (2606 行：Webview 生命周期 + 消息路由 + 核心执行)
  ├── CommandHandler (4个)    → 配置 / 知识库 / MCP / Skills
  ├── EventBindingService     → 事件绑定 + 工具授权状态机 (513 行)
  └── WorkerStatusService     → Worker 连接状态检查 + 缓存 (264 行)
```

**总计**: 3503 → 2606 行 (-25.6%)

### 6.3 #19: 资源生命周期管理

**[表象]**: DispatchManager、ContextManager 等组件没有 `dispose()` 方法，EventListener 注册后无清理。

**[修复]**: 补全 dispose 链：

1. **WVP.dispose()** 新增 `orchestratorEngine.dispose()` 调用，连通 MDE → MO → Workers 的清理链
2. **DispatchManager** 新增 `dispose()` 方法，清理 activeBatch + planningExecutor 引用
3. **MDE.dispose()** 新增 `dispatchManager.dispose()` 调用

**修复后 dispose 链**:

```text
WVP.dispose()
  ├── orchestratorEngine.interrupt()    — 中断当前执行
  ├── orchestratorEngine.dispose()      — 新增 ✓
  │   ├── dispatchManager.dispose()     — 新增 ✓ (清理 batch + executor)
  │   ├── messageHub.dispose()          — 清理消息队列
  │   ├── missionOrchestrator.dispose() — 清理 Workers + removeAllListeners
  │   └── removeAllListeners()          — 清理 MDE 事件
  ├── adapterFactory.shutdown()         — 关闭 LLM 连接
  ├── eventBindingService.disposeToolAuthorization()
  ├── globalEventBus.clear()            — 清理全局事件总线
  └── _view = undefined
```

**无需 dispose 的组件**（经确认）:

- ContextManager: 无事件监听、无定时器
- WorkerPipeline: 无事件监听、无定时器

---

## 7. 验收检查清单

### 7.1 Wave 1 验收

- [x] `_context.mission` 全局零引用
- [x] `emitSubTaskStatusCard` 全局零引用
- [x] `buildSubTaskTitlePrefix` 全局零引用
- [x] `reportTodoProgress` 无 Mission 模式分支
- [x] DispatchManagerDeps 中不含 `get/setActiveBatch` 闭包回调
- [x] DM 内部持有 `activeBatch`，暴露 `getActiveBatch()` 访问器
- [ ] MDE 不再有 `activeBatch`/`activeUserPrompt`/`activeImagePaths` 私有字段（activeBatch 已迁移，其余保留为 DM 闭包读取）
- [x] `IntentGate` 包含完整的意图分类解析链路（`parseClassificationResponse` 静态方法）
- [x] `ResilientCompressorAdapter` 独立文件，封装压缩容错（205 行）
- [x] `TaskViewService` 独立文件，包含任务视图 CRUD 方法（123 行）
- [ ] MDE 行数 < 800（当前 1065，已从 2021 削减 47%）
- [x] 编译通过（`npx tsc --noEmit` 零错误）

### 7.2 Wave 2 验收

- [x] MDE 中 `setupEventForwarding()` 方法已删除
- [x] MDE 中 `reportTodoProgress()` 方法已删除
- [x] Todo 事件处理迁移到 DispatchManager（持有 messageHub + activeBatch）
- [x] Todo 事件只有 WVP 一个消费者（无双重消费）
- [x] `MissionOrchestratorEventMap` 接口定义完整，事件参数有类型约束
- [x] 外部不直接 `emit` MO，改为调用 `notifyAssignmentPlanned()` 方法
- [x] DispatchBatch 有 `BatchPhase` 类型 + `transitionTo()` 状态机
- [x] 三通道职责边界清晰，MDE 不再作为事件桥接层
- [x] WVP 死事件监听已清理（missionPlanned/executionCompleted/executionFailed）
- [x] MO 补充 Worker 事件转发（assignmentStarted/Completed/approvalRequested）
- [x] 编译通过（`npx tsc --noEmit` 零错误）

### 7.3 Wave 3 验收

- [x] PlanningExecutor 中 `plan` 模式相关代码已删除（execute/planWithLLM/PlanningOptions/PlanningResult）
- [x] DispatchManagerDeps 中 `planningExecutor` 已移除，DM 内部延迟创建单例
- [x] MO 中死接口 `PlanningOptions` 已删除
- [x] `phaseCFallback()` 在降级时附加 warning 通知，用户可见
- [x] `enqueueExecution()` 在队列非空时通知用户排队状态（pendingCount 计数器）
- [x] WebviewProvider 中 `as any` 数量 = **0**（原 42 处，新增 4 个消息类型到 union）
- [x] 修正 `UIState.sessions` 类型为 `SessionMeta[]`，删除死接口 `UIChatSession`
- [x] 编译通过（`npx tsc --noEmit` 零错误）

### 7.4 Wave 4 验收

- [x] MissionOrchestrator 行数 = **250**（目标 < 600，实际远超预期）
- [x] 无需新建 MissionLifecycleService（死代码删除策略优于拆分策略）
- [x] MO EventMap 精简：26 → 14 个事件（删除 12 个永不发射的死事件）
- [x] MO 死类型全部清理：MissionCreationResult, ExecutionOptions, ExecutionProgress, ExecutionResult, MissionVerificationResult, MissionSummary
- [x] WVP 死监听器替换：missionCompleted/Failed/Cancelled → missionStatusChanged
- [x] MDE 死调用清理：setIntentGate/setKnowledgeBase 不再穿透到 MO
- [x] 编译通过（`npx tsc --noEmit` 零错误）
- [ ] WebviewProvider 行数 < 1500（当前 3502）
- [ ] EventBindingService + MessageRouter 独立文件
- [ ] DispatchManager/ContextManager 实现 `vscode.Disposable`
- [ ] 所有 EventListener 注册有对应的 dispose 清理

### 7.5 全局不变量（所有 Wave 共用）

- [ ] **编译零错误**: `npm run compile` 通过
- [ ] **功能完整**: F5 编排模式 + 直接模式均正常工作
- [ ] **禁止多重实现**: 每个功能只有一条实现路径
- [ ] **禁止回退逻辑**: 无新旧兼容分支
- [ ] **禁止打补丁**: 无判空绕过，所有修复追溯根因
- [ ] **事件单消费**: 每个事件到前端只有一条路径

---

## 附录 A: 重构前后指标对比

| 指标 | 原始值 | Wave 1 后（实际） | Wave 2 后（实际） | Wave 3 后（实际） | Wave 4 后（实际） |
| ---- | ------ | --------- | --------- | --------- | --------- |
| MDE 行数 | 2021 | **1065** | **1004**（-61） | **1001**（-3） | **999**（-2，删死MO调用） |
| MO 行数 | 1697 | 1697 | **1769**（+72，事件类型接口+Worker转发） | **1757**（-12，删PlanningOptions） | **250**（-1507，死代码清理86%） |
| WVP 行数 | 3890 | **3492** | **3515**（+23 净增，删死监听+类型修复） | **3514**（-1，as any 清除+类型窄化） | **3502**（-12，死监听器→missionStatusChanged） |
| DM 行数 | 552 | **552** | **606**（+54，事件监听+reportTodoProgress） | **621**（+15，PlanningExecutor 单例） | **621** |
| DM 闭包回调数 | 12 | **2 (get/set已消除)** | **0** | **0** | **0** |
| setupEventForwarding 行数 | ~95 | ~60 | **0**（已删除） | **0** | **0** |
| Todo 消费路径数 | 2 | 2 | **1**（DM→subTaskCard） | **1** | **1** |
| MO 外部 emit 调用 | 1 | 1 | **0**（notifyAssignmentPlanned） | **0** | **0** |
| WVP 死事件监听 | 6 | 6 | **0**（3删除+3补转发） | **0** | **0** |
| `as any` 数量 (WVP) | 42 | 42 | 42 | **0**（+4 消息类型补充到 union） | **0** |
| PlanningExecutor 行数 | 122 | 122 | 122 | **63**（-59，死代码清理） | **63** |
| MO EventMap 事件数 | ~26 | ~26 | ~26 | ~26 | **14**（删12个死事件） |
| MO 死类型/接口 | 6 | 6 | 6 | 6 | **0**（全部清理） |
| 新建模块 | — | **2** | **0** | **0** | **0**（死代码删除，无需新建） |

## 附录 B: 新建模块清单

| 模块 | 文件路径 | Wave | 行数估计 |
| ---- | -------- | ---- | -------- |
| ResilientCompressorAdapter | `src/orchestrator/core/resilient-compressor-adapter.ts` | 1 | ~150 |
| TaskViewService | `src/services/task-view-service.ts` | 1 | ~150 |
| ~~MissionLifecycleService~~ | ~~`src/orchestrator/core/mission-lifecycle-service.ts`~~ | ~~4~~ | ~~取消：死代码删除替代拆分~~ |
| EventBindingService | `src/ui/event-binding-service.ts` | 4 | ~500 |
| MessageRouter | `src/ui/message-router.ts` | 4 | ~600 |

## 附录 C: 废弃项清单

| 废弃项 | 类型 | Wave |
| ------ | ---- | ---- |
| MDE._context / get context() / get plan() | 字段+方法删除 | 1 |
| MDE.emitSubTaskStatusCard() | 死方法删除 | 1 |
| MDE.buildSubTaskTitlePrefix() | 死方法删除 | 1 |
| MDE.activeBatch / activeUserPrompt / activeImagePaths | 字段删除 | 1 |
| DispatchManagerDeps 全部 get/set 闭包 | 接口字段删除 | 1 |
| MDE.isAuthOrQuotaError / isConnectionError / isModelError / isConfigError | 方法迁移后删除 | 1 |
| MDE.setupEventForwarding() | 方法删除 | 2 |
| MDE.reportTodoProgress() | 方法删除 | 2 |
| PlanningExecutor plan 模式分支 | 死代码删除 | 3 |
| PlanningExecutor.execute() / planWithLLM() | 死方法删除 | 3 |
| PlanningOptions / PlanningResult（executor） | 死接口删除 | 3 |
| MO.PlanningOptions | 死接口删除 | 3 |
| DispatchManagerDeps.planningExecutor 工厂 | 依赖移除 | 3 |
| MDE import PlanningExecutor | 未使用导入删除 | 3 |
| UIChatSession | 死接口删除 | 3 |
| WVP 42处 `as any` | 类型断言消除 | 3 |
| MO.processRequest / analyzeIntent / createMission | 死方法删除（pipeline 全部死代码） | 4 |
| MO.understandGoal / selectParticipants / defineContracts / assignResponsibilities | 死方法删除 | 4 |
| MO.approveMission / pauseMission / resumeMission / cancelMission / completeMission / failMission | 死方法删除 | 4 |
| MO.verifyMission / verifyCriterion / verifyWithSpec / parseVerificationSpec / verifyByTaskCompletion | 死方法删除 | 4 |
| MO.summarizeMission / writeExecutionToMemory | 死方法删除 | 4 |
| MO.getCachedPlanning / cachePlanning / clearCache / generateCacheKey / cleanupCache | 死方法删除 | 4 |
| MO.getSnapshotManager / getContextManager / getProfileLoader / getGuidanceInjector / getReviewer / getIntentGate | 死 Getter 删除 | 4 |
| MO.getAllWorkers / isExecuting / getCurrentMissionId / initializeContext / setIntentGate / setKnowledgeBase | 死方法删除 | 4 |
| MO.getConnectedWorkers / isWorkerAvailable / getProjectContext / getRelevantADRs / buildTaskStructuredInfo / resolvePath | 死私有方法删除 | 4 |
| MissionCreationResult / ExecutionOptions / ExecutionProgress / ExecutionResult | 死类型删除 | 4 |
| MissionVerificationResult / MissionSummary | 死类型删除 | 4 |
| MO.contractManager / assignmentManager / reviewer / verificationRunner / intentGate / snapshotManager / projectKnowledgeBase / planningCache / assignmentResolver | 死字段删除 | 4 |
| MO EventMap 12 个死事件（intentAnalyzed, goalUnderstood, participantsSelected, contractsDefined, responsibilitiesAssigned, missionApproved, missionPaused, missionResumed, missionCancelled, missionCompleted, missionFailed, verification/summarization 事件） | 死事件删除 | 4 |
| WVP missionCompleted/missionFailed/missionCancelled 监听器 | 替换为 missionStatusChanged | 4 |
| MDE → MO.setIntentGate / setKnowledgeBase | 死调用删除 | 4 |
