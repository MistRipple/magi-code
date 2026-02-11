# L3 统一执行架构重构方案

> 状态：实施中
> 日期：2026-02-11
> 目标：消除 dispatch_task / plan_mission 双路径分裂，建立单一 L3 执行管道

---

## 1. 问题诊断

### 1.1 现状：两条并列的 L3 执行路径

| 维度 | dispatch_task 路径 | plan_mission 路径 |
|------|-------------------|-------------------|
| 入口工具 | `dispatch_task` | `plan_mission` |
| 调度器 | `DispatchManager` | `ExecutionCoordinator` |
| Worker 执行 | 直接调 `Worker.executeAssignment` | `AssignmentExecutor` 包装（含 LSP/快照/变更检测） |
| 阻塞性 | 非阻塞（立即返回 task_id） | 阻塞（等待全部完成） |
| 事件链 | 仅 `subTaskCard` | 完整 Mission 事件链（missionPlanned → assignmentPlanned → todo*） |
| 治理能力 | 无（无 LSP、无快照、无 Review、无契约验证） | 完整（LSP 预检/后检 + 快照 + 目标变更检测 + Review + 契约验证） |
| Report 处理 | `handleDispatchWorkerReport`（透传进度 + Phase B+ 中间 LLM） | `handleWorkerReport`（Wisdom 提取 + 用户提问回调 + 补充指令注入） |
| 汇总 | Phase C LLM 汇总 | `verifyMission` + `summarizeMission` |
| 使用频率 | ~99% | ~1% |

### 1.2 问题

1. **能力断裂**：99% 的任务走 dispatch 却缺少治理能力（LSP/快照/Review）
2. **代码冗余**：两套并行的调度、执行、报告、汇总逻辑
3. **事件链不统一**：前端 TasksPanel 需要兼容两种事件格式
4. **维护困难**：任何 Worker 执行逻辑的修改都要在两个地方同步

---

## 2. 目标架构

### 2.1 架构全景

```
用户请求 → MissionDrivenEngine.execute()
  → 编排者 LLM（ReAct tool loop）
    → L1: 直接回答
    → L2: 工具调用
    → L3: dispatch_task(worker, task, files?, dependsOn?, governance?)
      │
      ▼
    DispatchManager（唯一 L3 调度器）
      → DispatchBatch.register（拓扑排序 / 文件冲突检测 / 深度校验）
      → Worker 隔离策略调度（同类型串行、不同类型并行）
      → WorkerPipeline.execute（统一执行管道）
          ├─ [auto] Snapshot 创建       ← 有 files 时自动启用
          ├─ [auto] LSP 预检            ← 有 files 时自动启用
          ├─ Todo 创建（PlanningExecutor.createMacroTodo）
          ├─ Worker.executeAssignment   ← 核心执行
          ├─ [auto] 目标变更检测 + 强制重试
          ├─ [auto] LSP 后检
          └─ [auto] Context 更新
      → 状态更新 + 统一事件链（assignmentPlanned + subTaskCard + todo*）
      → (全部完成) Phase C 汇总
  → 等待 Batch 归档 → 返回结果
```

### 2.2 核心组件

| 组件 | 职责 | 变更 |
|------|------|------|
| **WorkerPipeline** | 围绕 Worker.executeAssignment 的统一治理包装 | **新建** |
| **DispatchManager** | L3 唯一调度器（注册/调度/汇总） | **改造** |
| **DispatchBatch** | 任务批次管理（拓扑/隔离/取消） | **不变** |
| **PlanningExecutor** | 一级 Todo 创建入口 | **不变** |
| **MissionDrivenEngine** | 编排入口（ReAct loop） | **精简** |
| **MissionOrchestrator** | Worker 管理 + Mission CRUD | **精简** |

---

## 3. 详细设计

### 3.1 WorkerPipeline（新建）

从 `AssignmentExecutor` 提取核心逻辑，不依赖 Mission 对象，所有治理步骤 opt-in：

```typescript
// src/orchestrator/core/worker-pipeline.ts

export interface PipelineConfig {
  // 基本信息（必选）
  assignment: Assignment;
  workerInstance: AutonomousWorker;
  adapterFactory: IAdapterFactory;
  workspaceRoot: string;
  projectContext?: string;

  // 治理开关（由 governance + files 自动计算）
  enableSnapshot: boolean;     // 有 files + requiresModification 时 auto=true
  enableLSP: boolean;          // 有 files 时 auto=true
  enableTargetEnforce: boolean; // 有 files + requiresModification 时 auto=true
  enableContextUpdate: boolean; // 有 contextManager 时 auto=true

  // 外部依赖（可选注入）
  snapshotManager?: SnapshotManager | null;
  contextManager?: ContextManager | null;

  // 执行选项
  onReport?: ReportCallback;
  cancellationToken?: CancellationToken;
  imagePaths?: string[];
  missionId?: string;
}

export interface PipelineResult {
  executionResult: AutonomousExecutionResult;
  lspNewErrors?: string[];
  targetChangeDetected?: boolean;
}
```

**关键设计**：`governance` 参数到治理开关的映射逻辑在 `DispatchManager.launchDispatchWorker` 中完成，不在 WorkerPipeline 内部：

```
governance = 'auto'（默认）:
  - enableSnapshot = files.length > 0 && snapshotManager != null
  - enableLSP = files.length > 0
  - enableTargetEnforce = files.length > 0
  - enableContextUpdate = contextManager != null

governance = 'full':
  - 全部 = true
```

### 3.2 dispatch_task 工具扩展

```typescript
// 新增可选参数
interface DispatchTaskParams {
  worker: WorkerSlot;            // 必选
  task: string;                  // 必选
  files?: string[];              // 可选
  depends_on?: string[];         // 可选
  governance?: 'auto' | 'full';  // 可选，默认 'auto'
}
```

### 3.3 统一事件链

DispatchManager 在 `launchDispatchWorker` 中发射完整事件链：

```
1. Todo 创建后      → emit assignmentPlanned（前端展示 Todo 列表）
2. Worker 开始      → subTaskCard(running)
3. Worker 执行中    → todoStarted / todoCompleted / dynamicTodoAdded（Worker 内部）
4. Worker 完成      → subTaskCard(completed/failed) + emit assignmentCompleted
5. Batch 全部完成   → Phase C 汇总 → result
```

### 3.4 Report 处理统一

合并 `handleDispatchWorkerReport` 和 `handleWorkerReport` 的能力：

- **progress** → subTaskCard 更新 + 补充指令注入
- **question** → Phase B+ 中间 LLM 调用（频率限制）
- **completed/failed** → Wisdom 提取 + Context 更新

---

## 4. 变更矩阵

### 4.1 新建文件

| 文件 | 说明 |
|------|------|
| `src/orchestrator/core/worker-pipeline.ts` | 统一 Worker 执行管道 |

### 4.2 核心改造文件

| 文件 | 变更点 |
|------|--------|
| `src/orchestrator/core/dispatch-manager.ts` | `launchDispatchWorker` 使用 WorkerPipeline；移除 `plan` handler；发射 assignmentPlanned 事件；合并 Report 处理能力 |
| `src/tools/orchestration-executor.ts` | 移除 `plan_mission` 工具定义和执行逻辑；dispatch_task 新增 governance 参数；`TOOL_NAMES` 改为 `['dispatch_task', 'send_worker_message']`；移除 `PlanMissionHandler` 类型 |
| `src/orchestrator/core/mission-driven-engine.ts` | 移除方法：`executePlan`、`executePlanRecord`、`createPlan`、`resumeMission`、`handleWorkerReport`、`planCollaborationWithLLM`、`analyzeRequirement`、`shouldUseOrchestratorToolingPath`、`missionToPlan`、`planToMission`、`formatPlanForUser`、`getActivePlanForSession`、`getLatestPlanForSession`、`getPlanById`；移除 `PlanStorage` import 和实例化 |
| `src/orchestrator/core/mission-orchestrator.ts` | 移除方法：`execute`、`planMission`；移除 import：`ExecutionCoordinator`、`TaskPreAnalyzer`、`OrchestratorResponder`、`BlockedItem` |
| `src/ui/webview-provider.ts` | 移除方法：`executePlanOnly`、`executeStartWork`；移除 `/plan` `/start-work` 命令解析；移除 `resumeMission` 调用；移除 plan 相关 UI 状态 |
| `src/orchestrator/core/executors/index.ts` | 仅保留 `PlanningExecutor` 相关导出 |
| `src/orchestrator/core/index.ts` | 移除 `ExecutionOptions`/`ExecutionProgress`/`ExecutionResult` 从 MissionOrchestrator 的重导出；移除 `BlockedItem` 等阻塞类型导出 |
| `src/orchestrator/index.ts` | 清理废弃模块的导出 |
| `src/orchestrator/prompts/orchestrator-prompts.ts` | 移除 plan_mission 相关提示词；更新 L3 指导说明 |

### 4.3 废弃文件（删除）

| 文件 | 原因 |
|------|------|
| `src/orchestrator/core/executors/execution-coordinator.ts` | 调度能力由 DispatchBatch 覆盖，治理能力由 WorkerPipeline 替代 |
| `src/orchestrator/core/executors/assignment-executor.ts` | 核心逻辑提取到 WorkerPipeline |
| `src/orchestrator/core/executors/task-pre-analyzer.ts` | 治理由 governance 参数决定，不需要 LLM 预分析 |
| `src/orchestrator/core/executors/orchestrator-responder.ts` | Report 处理统一到 DispatchManager |
| `src/orchestrator/core/executors/progress-reporter.ts` | 进度通过 subTaskCard 追踪 |
| `src/orchestrator/core/executors/blocking-manager.ts` | DispatchBatch 的依赖管理已覆盖阻塞场景 |
| `src/orchestrator/core/executors/review-executor.ts` | Review 能力由 Phase C 汇总中编排者 LLM 判断 |
| `src/orchestrator/core/executors/contract-verifier.ts` | 契约验证由 dependsOn + 拓扑排序替代 |
| `src/orchestrator/plan-storage.ts` | 仅服务于已废弃的 createPlan/executePlan 路径 |

### 4.4 MissionDrivenEngine 废弃方法完整清单

| 方法 | 行号 | 原因 |
|------|------|------|
| `handleWorkerReport` | ~705 | 统一使用 DispatchManager 的 Report 处理 |
| `createPlan` | ~1393 | plan_mission 路径废弃 |
| `executePlan` | ~1475 | plan_mission 路径废弃 |
| `resumeMission` | ~1548 | 不再有 Mission 级别恢复概念 |
| `executePlanRecord` | ~1635 | plan_mission 路径废弃 |
| `getActivePlanForSession` | ~1648 | plan_mission 路径废弃 |
| `getLatestPlanForSession` | ~1656 | plan_mission 路径废弃 |
| `getPlanById` | ~1663 | plan_mission 路径废弃 |
| `planCollaborationWithLLM` | ~2190 | plan_mission 路径废弃 |
| `analyzeRequirement` | ~2466 | plan_mission 路径废弃 |
| `shouldUseOrchestratorToolingPath` | ~2578 | plan_mission 路径废弃 |
| `missionToPlan` | ~2601 | plan_mission 路径废弃 |
| `planToMission` | ~2634 | plan_mission 路径废弃 |
| `formatPlanForUser` | ~2698 | plan_mission 路径废弃 |

### 4.5 MissionOrchestrator 废弃方法清单

| 方法 | 行号 | 原因 |
|------|------|------|
| `execute` | ~1691 | 执行链路由 DispatchManager 接管 |
| `planMission` | ~782 | 仅被 createPlan 间接调用 |

### 4.6 WebviewProvider 废弃方法清单

| 方法/逻辑 | 行号 | 原因 |
|-----------|------|------|
| `executePlanOnly` | ~3384 | `/plan` 命令废弃 |
| `executeStartWork` | ~3411 | `/start-work` 命令废弃 |
| `parseOrchestrationCommand`（plan/start-work 分支） | ~3370 | 命令解析废弃 |
| `resumeMission` 调用 | ~1390 | Mission 恢复路径废弃 |

---

## 5. 实施步骤

### Phase 1: 创建 WorkerPipeline + 改造 DispatchManager

1. 新建 `worker-pipeline.ts`，从 `assignment-executor.ts` 提取核心逻辑
2. 改造 `dispatch-manager.ts` 的 `launchDispatchWorker` 使用 WorkerPipeline
3. 在 `launchDispatchWorker` 中发射 `assignmentPlanned` 事件
4. 合并 `handleWorkerReport` 的 Wisdom 提取和补充指令能力到 `handleDispatchWorkerReport`

### Phase 2: 清理 plan_mission 工具

1. `orchestration-executor.ts`：移除 plan_mission 工具定义、`PlanMissionHandler` 类型、`executePlanMission` 方法
2. `dispatch-manager.ts`：移除 `plan` handler
3. `orchestrator-prompts.ts`：移除 plan_mission 提示词

### Phase 3: 清理 MissionDrivenEngine 废弃方法

1. 移除 14 个废弃方法（见 4.4 清单）
2. 移除 `PlanStorage` import 和实例化
3. 移除 `handleWorkerReport` 及其辅助方法

### Phase 4: 清理 MissionOrchestrator + executors 目录

1. 移除 `MissionOrchestrator.execute()` 和 `planMission()`
2. 移除相关 import（ExecutionCoordinator、TaskPreAnalyzer、OrchestratorResponder）
3. 删除 executors 目录中 8 个废弃文件
4. 更新 `executors/index.ts` 仅保留 PlanningExecutor

### Phase 5: 清理 WebviewProvider + 前端 + 导出

1. 移除 `/plan` 和 `/start-work` 命令相关逻辑
2. 移除 `resumeMission` 调用
3. 更新 `core/index.ts` 和 `orchestrator/index.ts` 导出
4. 清理前端事件处理中的 plan_mission 路径特有逻辑

---

## 6. 验证检查清单

### 6.1 编译验证
- [ ] `npm run compile` 零错误
- [ ] 无未使用的 import 警告

### 6.2 路径完整性验证
- [ ] 所有 `plan_mission` 字符串引用已清除
- [ ] 所有 `ExecutionCoordinator` 引用已清除
- [ ] 所有 `AssignmentExecutor` 引用已清除
- [ ] 所有废弃方法调用点已清除
- [ ] `executors/` 目录仅保留 `planning-executor.ts` 和 `index.ts`

### 6.3 功能验证
- [ ] dispatch_task 工具正常注册和执行
- [ ] WorkerPipeline 治理开关按 governance 参数正确启用
- [ ] assignmentPlanned 事件正确发射
- [ ] subTaskCard + todoStarted/todoCompleted 事件链完整
- [ ] Phase C 汇总正常触发
- [ ] Worker 隔离策略（同类型串行、不同类型并行）正常工作
- [ ] 前端 TasksPanel 正确显示任务和 Todo

### 6.4 无回归验证
- [ ] 编排者 LLM ReAct loop 正常（L1/L2/L3 决策）
- [ ] 多 Worker 并行任务正常（dependsOn 依赖）
- [ ] Worker Report 处理正常（progress/question/completed/failed）
- [ ] Phase B+ 中间 LLM 调用正常
- [ ] 取消机制正常（CancellationToken）
- [ ] send_worker_message 工具正常

---

## 7. 新旧架构对比

| 维度 | 旧架构（双路径） | 新架构（统一） |
|------|------------------|----------------|
| 入口工具 | dispatch_task + plan_mission | dispatch_task（唯一） |
| 调度器 | DispatchManager + ExecutionCoordinator | DispatchManager（唯一） |
| Worker 执行 | 直接调 Worker vs AssignmentExecutor 包装 | WorkerPipeline（唯一，可配置治理） |
| 治理能力 | 仅 plan_mission 路径有 | 按需启用（governance: auto/full） |
| 事件链 | subTaskCard vs Mission 事件链 | 统一事件链 |
| Report 处理 | 两套独立处理器 | 统一处理器 |
| 前端展示 | 两种事件格式 | 统一格式 |
| executors 文件数 | 10 个 | 2 个（planning-executor + index） |
| MissionDrivenEngine 方法数 | ~30+ | ~15（移除 14 个废弃方法） |
