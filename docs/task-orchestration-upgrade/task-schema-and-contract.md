# Task Schema and Contract

更新时间：2026-04-16

## 1. 文档目标

本文档定义统一编排内核的协议级对象，作为
`unified-task-orchestration-kernel-upgrade-architecture.md` 的下游契约。

本文只回答“对象长什么样、字段是什么意思、哪些约束必须执行”，不展开运行时算法。

---

## 2. 统一约束

- `Task` 是唯一主工作对象
- `Mission` 是上下文容器，不是第二套任务模型
- `Worker` 是执行主体，不是任务节点
- 所有可执行任务都必须通过 `executor_binding` 绑定角色/能力约束
- `Repair / Decision` 属于运行时任务类型，禁止出现在初始图中
- `Task Projection` 是视图契约，不是另一套存储模型

---

## 3. 通用标识与引用

- `task_id`：任务唯一 ID
- `mission_id`：Mission 唯一 ID
- `worker_id`：Worker 唯一 ID
- `root_task_id`：Objective 根任务 ID
- `ref_id`：上下文、知识、输入、输出、证据等引用 ID
- 所有 ID 必须全局唯一、不可复用、不可重写语义

---

## 4. Mission Contract

`Mission` 至少包含：

- `mission_id`
- `session_id`
- `title`
- `goal_summary`
- `context_refs`
- `knowledge_refs`
- `global_policy_template`
- `objective_ids`
- `created_at / updated_at`

约束：

- 一个 `Mission` 可拥有多个 `Objective Task`
- 同一 `Mission` 下的多个 Objective 共享上下文，不共享状态机
- Objective 间的数据传递必须通过显式 `output_refs -> input_refs`

---

## 5. Worker Contract

`WorkerCapabilityProfile` 至少包含：

- `worker_id`
- `role`，如 `frontend-dev / backend-dev / architect`
- `capabilities`
- `allowed_tools`
- `preferred_workspaces`
- `parallelism_limit`
- `exclusive_scopes`
- `availability`

约束：

- Worker 能力声明必须可被 Planner 和 Runner 消费
- 没有匹配 Worker 的可执行任务不得进入实际调度

---

## 6. Task Contract

`Task` 至少包含：

- `task_id`
- `mission_id`
- `root_task_id`
- `parent_task_id`
- `kind`
- `title`
- `goal`
- `status`
- `dependency_ids`
- `required_children`
- `policy_snapshot`
- `executor_binding`
- `context_refs / knowledge_refs`
- `workspace_scope / write_scope`
- `input_refs / output_refs / evidence_refs`
- `retry_count / repair_count`
- `aggregate_status`
- `created_at / updated_at`

约束：

- 每个 `root_task_id` 只允许一个 `Objective`
- 只有叶子任务可被 Runner 调度
- `required_children` 默认包含所有直接子任务，除非显式标为可选
- `aggregate_status` 是内部聚合状态，不等于 UI 显示状态

---

## 7. TaskKind Contract

### 7.1 计划型节点

- `Objective`
- `Phase`
- `WorkPackage`
- `Action`
- `Validation`

### 7.2 运行时节点

- `Repair`
- `Decision`

约束：

- `Repair / Decision` 只能由 Runner 在执行期创建
- 简单任务允许 `Objective -> Action`
- 需要独立汇报、独立验收、独立授权时，应创建 `WorkPackage`

---

## 8. TaskStatus Contract

合法状态：

- `Draft`
- `Ready`
- `Running`
- `Blocked`
- `AwaitingApproval`
- `Verifying`
- `Repairing`
- `Completed`
- `Failed`
- `Cancelled`
- `Skipped`

约束：

- 终态固定为 `Completed / Failed / Cancelled / Skipped`
- `Decision` 在用户输入完成后进入 `Completed`
- 需要验收的任务不得从 `Running` 直接进入 `Completed`

---

## 9. ExecutorBinding Contract

`executor_binding` 至少包含：

- `target_role`
- `capability_requirements`
- `parallelism_group`
- `exclusive_scope`
- `worker_selector`

约束：

- 只有 `Action / Validation / Repair` 必须携带 `executor_binding`
- `Decision` 不绑定 Worker，只绑定人工输入协议
- 调度前必须同时校验任务可运行性与 Worker 可匹配性

---

## 10. TaskPolicy Contract

`TaskPolicy` 至少包含：

- `autonomy_level`
- `approval_mode`
- `allowed_tools / denied_tools`
- `allowed_paths / denied_paths`
- `network_mode / command_mode`
- `retry_limit / repair_limit`
- `validation_profile`
- `checkpoint_mode`
- `background_allowed`
- `escalation_conditions`

约束：

- `Objective.policy` 是策略模板，不等于所有子任务快照
- `policy_snapshot` 在 `Draft -> Ready` 时冻结
- 已进入 `Ready` 及之后状态的任务不得因父策略变化被静默改写

---

## 11. TaskProjection Contract

`TaskProjection` 至少包含：

- `root_task`
- `current_phase`
- `running_tasks`
- `blocked_tasks`
- `pending_decisions`
- `workpackage_summaries`
- `validation_summary`
- `progress_summary`
- `aggregate_status`
- `display_status`

约束：

- `aggregate_status` 面向运行时
- `display_status` 面向用户表达，可显示“部分完成/待修复”等产品语义
- Projection 不能引入 `Todo` 作为额外对象

---

## 12. DecisionTaskPayload Contract

`Decision Task` 至少包含：

- `decision_context`
- `blocked_reason`
- `options`
- `risk_notes`
- `recommended_option`
- `required_user_input`
- `decision_evidence`

约束：

- 用户输入被接受后，Decision 必须进入 `Completed`
- 下游任务必须重新计算依赖满足性与策略适用性
