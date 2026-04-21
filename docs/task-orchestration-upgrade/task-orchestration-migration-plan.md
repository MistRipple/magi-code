# Task Orchestration Migration Plan

更新时间：2026-04-16

## 1. 文档目标

本文档定义从现有 Mission / Assignment / Todo 驱动方式，迁移到
Mission + Task Graph + Worker Binding 统一编排内核的实施计划。

目标不是长期兼容双系统，而是用阶段化迁移完成单一真相源切换。

---

## 2. 迁移原则

- 新系统的唯一主工作对象是 `Task`
- `Mission` 保留为上下文容器
- `Worker` 保留为执行主体
- `Assignment` 语义迁移到 `executor_binding + AssignmentLease`
- `Todo` 只允许作为历史兼容输入，不允许成为新系统正式对象
- 任何兼容层都必须有明确删除时点

---

## 3. 新旧模型映射

### 旧模型

- `Mission`
- `Assignment`
- `Todo`

### 新模型

- `Mission`：保留，升级为上下文容器
- `Task(Objective / Phase / WorkPackage / Action / Validation)`
- `Task(Repair / Decision)`：运行时生成
- `executor_binding`：承接原 Assignment 的角色/能力分派语义
- `AssignmentLease`：承接原 Assignment 的运行时领取语义

映射规则：

- 旧 `todo_id` -> 新 `task_id`
- 旧 `assignment.worker_role` -> 新 `executor_binding.target_role`
- 旧 `assignment` 运行态 -> 新 `AssignmentLease`

---

## 4. 迁移阶段

### Phase 0：冻结目标模型

完成以下文档拍板：

- `unified-task-orchestration-kernel-upgrade-architecture.md`
- `task-schema-and-contract.md`
- `runner-runtime-and-escalation-protocol.md`

出口条件：不再新增 Todo 领域能力。

### Phase 1：引入新存储与新事件

新增：

- `task_store`
- `task_projection`
- `assignment_lease_store`
- `decision_evidence_store`

事件统一转为以 `task_id` 为主键。

出口条件：新事件流可以完整表示 Task 生命周期。

### Phase 2：在现有入口后挂接 Task Graph

保留现有用户入口，但把入口后的真实工作对象改为 `Task Graph`。

要求：

- 新请求必须先生成 `Objective Task`
- 旧 Todo 入口只作为输入适配层
- 所有新执行证据必须挂到 Task 上

出口条件：新进入系统的任务都能在 Task Projection 中看到。

### Phase 3：迁移 Worker 领取协议

把 Worker 侧逻辑从“领取 Assignment/Todo”迁移到“领取 Task lease”。

要求：

- Worker 只认 `executor_binding + AssignmentLease`
- 旧 Assignment API 只做短期转发
- 新并发控制与写范围冲突只在新协议中生效

出口条件：至少一条真实 Worker 执行链路已完全跑在新协议上。

### Phase 4：切换 Projection 与 UI

把 UI 的主视图切到 `Task Projection`。

要求：

- 不再展示 Todo 作为正式对象
- 历史 Todo 页面只读
- 新任务、运行中任务、决策阻塞都来自 Task Projection

出口条件：主 UI 不再依赖旧 Todo 投影。

### Phase 5：切换 Runner 与 Escalation

要求：

- Runner 只调度 `Task`
- checkpoint / resume 只基于新结构
- escalation / decision 只认 `Decision Task`

出口条件：长任务连续执行与恢复闭环建立完成。

### Phase 6：删除旧模型

删除：

- `Todo` 领域对象
- `TodoStore / Todo API / TodoEvent`
- 旧 Assignment 运行态语义

保留：

- 必要的数据迁移映射表
- 历史记录查询兼容层（只读）

出口条件：Task 成为唯一真相源。

---

## 5. API 与事件迁移要求

- 所有新 API 响应必须返回 `task_id`
- 所有事件必须以 `task_id` 和 `mission_id` 为主键关联
- 旧 Todo API 若保留，只能转发到 Task API，不得新增逻辑
- 旧 Assignment API 若保留，只能包装 lease 查询，不得继续承载新语义

---

## 6. 数据迁移要求

- 旧 Todo 必须一次性映射为对应 Task
- 已关闭历史 Todo 可映射为 `Completed / Failed / Cancelled / Skipped`
- 历史 Assignment 执行记录需迁移为 evidence 或 lease history
- 迁移必须可审计、可回放、可校验总量一致性

---

## 7. 兼容层边界

允许存在的短期兼容：

- Todo 输入适配层
- Assignment 查询适配层
- 历史页面只读兼容

明确禁止：

- 新功能继续写到 Todo 模型
- 新调度逻辑继续跑在旧 Assignment/Todo 语义上
- 长期并存两套状态机、两套事件流、两套投影

---

## 8. 切换完成判据

满足以下条件时，迁移视为完成：

- 新任务全部以 Task Graph 创建
- Worker 全部通过 lease 领取任务
- UI 主视图全部来自 Task Projection
- escalation 全部通过 `Decision Task` 闭环
- checkpoint / resume 全部基于新结构
- 仓库内不再存在正式 `Todo` 领域对象
