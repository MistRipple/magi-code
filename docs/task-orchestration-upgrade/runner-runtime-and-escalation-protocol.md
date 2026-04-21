# Runner Runtime and Escalation Protocol

更新时间：2026-04-16

## 1. 文档目标

本文档定义统一 Task 编排内核的运行时协议，补足：

- Runner 如何调度 Task
- Worker 如何领取并执行任务
- graph reflection / replanning 如何触发
- checkpoint / resume 如何保证长任务连续性
- escalation / decision 如何阻塞与恢复下游

本文件依赖：

- `unified-task-orchestration-kernel-upgrade-architecture.md`
- `task-schema-and-contract.md`

---

## 2. 运行时对象

运行时只承认以下对象：

- `Mission`
- `Task Graph`
- `Worker`
- `AssignmentLease`
- `Checkpoint`
- `DecisionResult`

其中：

- `Task` 是工作对象
- `Worker` 是执行主体
- `AssignmentLease` 是任务执行租约，不是第二套任务模型

---

## 3. Runner 主循环

Runner 的标准循环固定为：

1. 加载 `Mission`、`Task Graph`、`Checkpoint`
2. 重建父节点聚合状态
3. 计算所有可运行叶子任务
4. 过滤出有匹配 Worker 的任务
5. 做并发冲突裁决
6. 为任务创建 `AssignmentLease`
7. 派发到匹配 Worker 或系统处理器
8. 接收结果并写入 `output_refs / evidence_refs`
9. 推进任务状态
10. 触发图反思或重规划（如需要）
11. 写入 checkpoint
12. 继续下一轮，直到无任务可推进或进入暂停/终止

停止条件：

- 所有任务进入终态
- 命中 `AwaitingApproval`
- 用户主动 `pause / cancel`
- 当前没有可推进任务且无自动重规划机会

---

## 4. Worker 领取与租约协议

`AssignmentLease` 至少包含：

- `lease_id`
- `task_id`
- `worker_id`
- `role`
- `granted_at`
- `expires_at`
- `heartbeat_at`
- `lease_status`

规则：

- 只有 `Action / Validation / Repair` 可以被 Worker 领取
- `Decision` 不创建 Worker lease，只等待人工输入
- Runner 派发前必须校验 `executor_binding` 与 Worker 能力匹配
- 同一任务在同一时刻只允许一个活动 lease
- lease 过期或 Worker 心跳失效后，Runner 必须回收 lease 并重算任务状态
- 回收后任务不得被默认为成功，必须根据已持久化证据决定回到 `Ready`、`Blocked` 或进入 `Repairing`

---

## 5. 并发与冲突协议

Runner 在派发前必须统一裁决：

- `write_scope` 冲突
- `exclusive_scope` 冲突
- `parallelism_group` 上限
- Worker 自身 `parallelism_limit`

规则：

- 写范围冲突任务不得并行
- 同一独占作用域只允许一个活动 lease
- 同一并发组超上限时，低优先级任务延迟派发
- Validation 默认可与只读 Action 并行，但不得与冲突写任务并行

---

## 6. 图反思与重规划协议

图反思触发点固定为：

- 初始图合成完成后
- 任一 `WorkPackage` 完成后
- Worker 返回“粒度不对 / 角色不匹配 / 上下文不足”
- 连续 repair 后仍失败
- `Decision` 修改策略或边界

图反思结果只允许：

- 接受当前图
- 细化剩余子树
- 收缩剩余子树
- 重规划剩余图
- 在同一 `Mission` 下派生新的 `Objective`

重规划约束：

- 保留 `mission_id / root_task_id / Objective.task_id`
- 已完成任务只能追加证据，不能重写
- 已运行任务只能在终态后影响后续子树
- `Repair / Decision` 不能被回填为“初始规划早已知道”的节点

---

## 7. Checkpoint / Resume 协议

每个 `Checkpoint` 必须保存：

- 当前图结构与版本
- 任务状态与聚合结果
- `policy_snapshot`
- `input_refs / output_refs / evidence_refs`
- `retry_count / repair_count`
- 活动或待完成的 `Decision`
- 活动 lease 摘要与回收所需信息

写 checkpoint 的时机：

- 任一状态迁移后
- 图反思/重规划后
- `Decision` 完成后
- Runner 退出前
- 后台挂起前

恢复规则：

- 先恢复图，再恢复聚合状态，再恢复调度
- 崩溃前的活动 lease 一律视为失效，需要重新校验
- 崩溃前处于 `Running / Verifying / Repairing` 的任务不得假定成功
- 恢复永远只相信已持久化证据

---

## 8. Escalation / Decision 协议

触发 escalation 的条件：

- 需要超出授权路径或工具
- 发现两个以上同等合理方案
- repair / retry 预算耗尽
- 验证结果与用户规则冲突
- 关键上下文缺失导致无法继续

`DecisionResult` 至少包含：

- `task_id`
- `selected_option`
- `user_input`
- `policy_changes`
- `decision_evidence`

闭环规则：

- 用户输入被接受后，`Decision Task` 必须进入 `Completed`
- 直接依赖它的下游任务必须重新计算依赖满足性
- 如果用户修改了路径、工具、预算或阶段边界，必须先重规划剩余图
- 如果用户做出终止性决策，受影响子树必须进入 `Cancelled` 或 `Skipped`

---

## 9. 控制命令协议

Runner 必须支持：

- `start`
- `pause`
- `resume`
- `cancel`
- `replan`
- `rerun_validation`

约束：

- `pause` 不取消现有证据，只停止新派发
- `resume` 必须从最新 checkpoint 恢复
- `cancel` 必须把未完成子树显式推入终态
- `replan` 只作用于剩余图

---

## 10. 运行时不变量

- 不存在无 Worker 匹配却被派发的可执行任务
- 不存在无 checkpoint 的后台长任务推进
- 不存在未写 decision evidence 就解除 gate 的情况
- 不存在通过引入 `Todo`、匿名 Node 或隐藏内部任务来补结构的问题
