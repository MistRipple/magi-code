# Plans 完整能力实现方案（产品级）

## 1. 背景与目标

当前 `plans` 目录仅有路径预留，缺少真实存取和执行链路绑定，导致：

1. 计划无法跨会话恢复与审计。
2. 计划确认（ask/auto）无法形成可追踪记录。
3. 计划与 Mission/Assignment/Todo 之间缺少一致性映射。
4. 无法回答“计划是怎么改的、为何改、执行到哪一步”。

本方案目标是把 `plans` 从“目录占位”升级为“会话级计划账本（Plan Ledger）”，满足产品级可用、可追溯、可验证。

---

## 2. 产品定位与设计原则

1. 单一事实源：计划状态以 `Plan Ledger` 为准，不在多处重复维护真相。
2. 会话隔离：计划天然按 `sessionId` 归档，禁止跨会话污染。
3. 版本可追溯：计划修订采用版本链，不覆盖历史。
4. 执行强绑定：计划项必须可映射到 `mission/assignment/todo`。
5. 模式一致：Standard（功能级）与 Deep（项目级）共享同一数据结构，仅规则不同。

---

## 3. 能力边界（In / Out）

### In Scope

1. 计划数据模型、存储、状态机、事件流。
2. 计划确认（ask/auto）落账与恢复。
3. 计划与 Mission/Assignment/Todo 的映射与进度计算。
4. UI 任务面板中的“当前计划 + 历史计划 + 修订记录”展示。
5. 全链路验证与回归脚本。

### Out of Scope（后续阶段）

1. 跨会话计划复用推荐（推荐引擎）。
2. 计划质量打分模型（智能评分）。
3. 多人协同审批流（多人评审）。

---

## 4. 目标架构

新增核心模块：`PlanLedgerService`

1. 负责计划的创建、修订、确认、执行态推进、归档。
2. 对外提供统一接口，供 Orchestrator、WebviewProvider、TaskViewService、UI 使用。
3. 持久化存储在 `.magi/sessions/{sessionId}/plans/`。

### 4.1 目录结构

1. `.magi/sessions/{sessionId}/plans/index.json`
2. `.magi/sessions/{sessionId}/plans/{planId}.json`
3. `.magi/sessions/{sessionId}/plans/{planId}.events.jsonl`（可选，建议保留）

说明：

1. `index.json` 用于快速检索和列表。
2. `planId.json` 存完整计划记录。
3. `events.jsonl` 存状态变更事件，便于回放与审计。

---

## 5. 数据模型（建议）

```ts
type PlanMode = 'standard' | 'deep';
type PlanStatus =
  | 'draft'
  | 'awaiting_confirmation'
  | 'approved'
  | 'rejected'
  | 'executing'
  | 'partially_completed'
  | 'completed'
  | 'failed'
  | 'cancelled';

interface PlanRecord {
  planId: string;
  sessionId: string;
  missionId: string;
  turnId: string;
  version: number;
  parentPlanId?: string;
  mode: PlanMode;
  status: PlanStatus;
  source: 'orchestrator';
  promptDigest: string;
  analysis?: string;
  acceptanceCriteria: string[];
  constraints: string[];
  riskLevel?: 'low' | 'medium' | 'high' | 'critical';
  summary?: string;
  review?: {
    status: 'approved' | 'rejected' | 'skipped';
    reviewer?: string;
    reason?: string;
    reviewedAt: number;
  };
  items: PlanItem[];
  links: {
    assignmentIds: string[];
    todoIds: string[];
  };
  createdAt: number;
  updatedAt: number;
}

interface PlanItem {
  itemId: string;
  title: string;
  owner: 'orchestrator' | 'claude' | 'codex' | 'gemini';
  category?: string;
  dependsOn: string[];
  scopeHints?: string[];
  targetFiles?: string[];
  requiresModification?: boolean;
  status: 'pending' | 'running' | 'completed' | 'failed' | 'skipped';
  progress: number; // 0-100
  assignmentId?: string;
  todoIds: string[];
}
```

---

## 6. 状态机与不变量

### 6.1 状态流转

1. `draft -> awaiting_confirmation`
2. `awaiting_confirmation -> approved/rejected`
3. `approved -> executing`
4. `executing -> partially_completed/completed/failed/cancelled`
5. `partially_completed -> completed/failed/cancelled`
6. 计划修订：任何非终态可生成 `version+1` 新计划，旧计划转 `superseded`（可通过 `events` 表示）。

### 6.2 核心不变量

1. 同一 `sessionId + turnId` 只能有一个“active 计划版本”。
2. 进入 `dispatch` 执行流后，`approved/executing` 计划必须绑定 `missionId`（无 dispatch 的纯问答轮次可无 mission）。
3. 计划项 `assignmentId/todoIds` 一旦绑定，不可指向其他 session。
4. 终态计划禁止再写入执行进度（只能通过修订新版本）。

---

## 7. 主链路接入点

### 7.1 Phase A（计划生成）

1. 在 Orchestrator 完成计划生成后，调用 `PlanLedgerService.createDraft(...)`。
2. 计划项以 `dispatch_task` 注册事件为事实源写入 `PlanRecord.items`，不依赖对 LLM 文本 plan block 的反解析。
3. 下发 UI `confirmationRequest` 时附带 `planId/version`。

### 7.2 计划确认（ask/auto）

1. Ask 模式：用户确认后 `approve(planId)`。
2. Auto 模式：自动确认也必须落账 `review.status='approved'`，`reviewer='system:auto'`。
3. 拒绝时记录 `rejected` 与理由，不进入执行。

### 7.3 执行阶段（dispatch + worker）

1. `dispatch_task` 成功后，将 `assignmentId` 回填到对应 `PlanItem`。
2. `todo created/started/completed/failed` 事件实时更新 `PlanItem.progress/status`。
3. Mission 完成时，触发计划终态归档。

### 7.4 会话恢复

1. 切换到 session 时加载 `active plan + recent plan history`。
2. 若存在 `executing` 且 Mission 已终态，执行 reconcile 自动纠正状态。
3. UI 面板展示：当前计划、历史版本、修订原因、执行达成率。

---

## 8. 与现有模块协同

1. `UnifiedSessionManager`：只提供路径与会话元信息，不承载计划业务。
2. `MissionStorageManager`：仍是任务执行事实源，Plan 通过 `missionId` 关联。
3. `TodoManager`：通过 `todoId/sessionId` 回写计划项进度。
4. `MessageHub + message-handler`：新增 `planLedgerUpdated` 数据消息用于 UI 同步。
5. `TaskViewService`：聚合任务时可附带当前计划摘要。

---

## 9. 迁移策略

### 9.1 线上/本地兼容

1. 对已存在 session：若无 plans 数据，按“无历史计划”处理。
2. 不迁移历史残留 `out` 目录的旧 plan 结构。
3. 自新版本起按新模型记录。

### 9.2 可选灰度开关（按需）

1. `magi.planLedger.enabled`（默认 true）可作为紧急回退预案，但不是当前版本上线前置条件。
2. `magi.planLedger.strictReconcile`（默认 true）仅在出现对账性能/稳定性争议时再引入。
3. 产品优先级：先保证链路正确与可追溯，再决定是否增加运维开关复杂度。

---

## 10. 验证方案（验收基线）

### 10.1 P0 功能验证

1. 新会话生成计划后，`plans/{planId}.json` 必须落盘。
2. Ask/Auto 两模式确认后，`review` 字段必须准确。
3. 执行中 `PlanItem` 进度可随 Todo 状态动态变化。
4. 会话切换后，计划视图不丢失、不串 session。
5. Mission 完成后，计划状态进入终态并可追溯。

### 10.2 P0 一致性验证

1. `PlanRecord.sessionId === Mission.sessionId === Todo.sessionId`。
2. `PlanItem.todoIds` 必须都存在且属于同一 session。
3. 同一 `session+turn` 无多个 active 版本。

### 10.3 P1 稳定性验证

1. 中断恢复：执行中断后恢复，计划状态和进度正确续接。
2. 并发调度：同会话多 worker 并发，计划项映射不丢不乱序。
3. 异常回放：模拟写盘失败/事件乱序，reconcile 后状态正确。

### 10.4 回归脚本建议

1. `scripts/e2e-plan-ledger-lifecycle.cjs`
2. `scripts/e2e-plan-ledger-session-isolation.cjs`
3. `scripts/e2e-plan-ledger-reconcile.cjs`

---

## 11. 交付拆分（建议）

### 阶段 A：后端账本能力

1. `PlanLedgerService`、模型、存储、状态机、reconcile。
2. 打通 Orchestrator 生成/确认/执行终态写入。

### 阶段 B：前端展示与恢复

1. plans 面板与任务面板联动。
2. 会话切换时加载计划版本和进度。

### 阶段 C：验证与上线门禁

1. E2E 回归脚本。
2. 故障注入与性能基线。

---

## 12. 上线门禁（Go/No-Go）

满足以下条件才可认为“完整版可上线”：

1. P0 验证项全部通过。
2. 三组 E2E 回归全部稳定通过。
3. 关键不变量巡检无失败（session/mission/todo/plan 一致性）。
4. 会话切换、任务恢复、并发调度场景无计划污染。

---

## 13. 风险与对策

1. 风险：计划与实际执行偏离。  
对策：`reconcile` 以 Mission/Todo 事实源纠正 Plan 状态。

2. 风险：并发事件导致计划项错序。  
对策：事件写入使用单会话串行队列 + 版本号。

3. 风险：UI 缓存显示旧计划。  
对策：计划更新统一走 `planLedgerUpdated`，切 session 强制重载。

---

## 14. 结论

`plans` 应作为产品核心治理能力实现，而不是“展示文本”。  
本方案实现后，Magi 将具备可审计、可恢复、可验证的计划执行闭环，能够支撑 Standard/Deep 双模式下的工程级协作。
