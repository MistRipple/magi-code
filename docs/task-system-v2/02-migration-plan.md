# Task System v2 推进计划

更新时间：2026-05-17
状态：按单方案推进

## 1. 推进原则

v2 只保留一个设计主线：

```text
简单任务：SessionTurn
中等任务：ExecutionChain + TaskGraph
复杂任务：Mission + ExecutionChain + TaskGraph + Long-Mission 闸门
```

所有后续实现都围绕这条主线推进，不引入兼容路径或双实现叙事。

落地原则：

- **单事实源**：任务状态以 Task/TaskGraph 为事实源，session timeline 只是投影。
- **单拓扑**：SpawnGraph 使用 `TaskId` 建图，不再引入独立 Conversation 拓扑。
- **单运行容器**：Conversation 只属于单个 Task，负责 Turn/Mailbox。
- **单恢复入口**：ExecutionChain 是当前任务化执行的恢复入口。
- **单复杂任务边界**：Mission 是长周期任务的唯一业务边界。

## 2. 当前基线

已经进入主线的能力：

- `chat / execute / task / continue` 分类入口。
- `dispatch_submission` 创建 root task、execution chain、worker thread。
- TaskRunner 推进 pending/running/terminal 状态、lease 和结果回收。
- task-level Conversation 承载 Turn/Mailbox。
- `agent_spawn / send_message / task_stop` 进入 coordinator 工具层。
- SpawnGraph 使用 `TaskId` 做父子拓扑。
- Tier 4 stores 已有文件化记录、工具写入和 prompt 注入。

需要收敛的点：

- HumanCheckpoint 还需要 runtime 硬阻塞。
- Validation 还需要接入 Plan 完成门槛。
- Checkpoint 还需要从摘要日志升级为恢复协议。
- MissionCharter 还需要生命周期。
- Long Mission 激活条件还需要显式业务语义。

## 3. 推进顺序

### P1 — 文档与概念收敛

目标：文档、代码注释、类型命名使用同一业务模型。

工作：

- 明确 `Task` 是业务节点。
- 明确 `Conversation` 是运行容器。
- 明确 `ExecutionChain` 是恢复入口。
- 明确 `Mission` 是复杂任务边界。
- 删除或改写与当前业务模型不一致的旧表述。

验收：

- `docs/task-system-v2` 只保留当前业务模型。
- 架构图和时序图与当前代码主路径一致。

### P2 — HumanCheckpoint runtime 硬阻塞

目标：人审 pending 时，系统不能继续派发新工作。

工作：

- 在 coordinator 工具层拦截 `agent_spawn`。
- 在 TaskRunner 或 dispatch 层阻止 long mission 新 leaf dispatch。
- pending 时允许读取、汇报、请求用户决策，但不允许产生新副作用工作。
- resolve 后恢复执行链。

验收：

- 有 pending HumanCheckpoint 时，`agent_spawn` 返回 rejected。
- 已存在 running task 的处理策略明确：可完成当前无副作用汇报，但不能继续 spawn。
- resolve 后同一 Mission 可以继续推进。

### P3 — Plan completion validation gate

目标：Plan step 不能靠模型口头标记完成。

工作：

- `plan_write` 接收 completed step 时查询 ValidationStore。
- 没有 pass 记录或存在未消解 fail 时拒绝 completed。
- skipped 必须带 reason，并只允许非交付型 step 使用。
- validation evidence 写入 Plan prompt 视图。

验收：

- 没有 validation pass 的交付 step 不能 completed。
- 有 fail 记录时不能 completed。
- 补写 pass 后可以 completed。

### P4 — MissionCharter 生命周期

目标：Charter 既能支持早期澄清，也能冻结为长期契约。

工作：

- 增加 `draft / frozen` 状态。
- Mission 初始化和澄清阶段允许 draft 更新。
- frozen 后 `mission_charter_write` 必须要求 HumanCheckpoint approval。
- prompt 中显示 Charter 状态。

验收：

- draft 可更新。
- frozen 后直接更新被拒绝。
- 通过 HumanCheckpoint resolve 后可产生受审计的修改。

### P5 — Checkpoint 恢复协议

目标：Checkpoint 能让复杂任务跨进程继续，不只是展示最近记录。

工作：

- 定义 checkpoint 最小恢复集：
  - active execution chain
  - task tree / statuses
  - spawn graph open edges
  - mailbox pending payload references
  - thread / turn cursor
  - plan / KG / validation versions
  - workspace commit or snapshot pointer
- checkpoint_create 写入恢复指针。
- resume 路径校验恢复集完整性。

验收：

- 进程重启后能定位 in-progress task。
- parent/child 回执路由不丢。
- pending mailbox 能进入下一次 Turn。
- checkpoint 损坏时给出明确恢复错误，不静默继续。

### P6 — 显式 Long Mission 激活

目标：复杂任务进入 Mission 由业务 tier 决定，而不是借用 `background_allowed`。

工作：

- 在任务分类或 dispatch request 中显式表达 task tier / mission mode。
- long mission 只在明确复杂任务时启用 Tier 4。
- 中等任务可使用 coordinator，但不自动启用 Charter/Plan/Checkpoint。

验收：

- 中等 coordinated task 不注入 Tier 4。
- 复杂 mission task 必定有 Charter/Plan/Checkpoint/HumanCheckpoint 能力。
- UI projection 能区分 `execution_chain` 与 `long_mission`。

### P7 — TaskKind executor 成熟度收敛

目标：TaskKind 的文档能力和真实 executor 能力一致。

工作：

- 标注每个 TaskKind 的实现状态。
- `LocalAgent` 作为当前可运行核心；shell 命令通过 `shell_exec` 工具执行，不再保留独立命令任务变体。
- 其它变体在 executor 未完成前保持不可调度或明确 rejected。
- 避免接口看起来支持但运行时悄悄退化成普通 local agent。

验收：

- 不支持的 TaskKind 不能进入 running。
- UI 和日志能说明 unsupported reason。
- 新 executor 接入必须补调度、取消、结果回收和验证。

## 4. 验证策略

每个推进点都要覆盖三类验证：

- 单元测试：状态转换、工具拦截、store round-trip。
- 集成测试：session turn / dispatch / task runner / mailbox 回执链路。
- 手工验收：通过 daemon `/web.html` 验证用户可见进度和恢复行为。

不要求每个推进点都跑完整前端验收；涉及 UI projection、daemon 托管、HumanCheckpoint 交互时必须跑。

## 5. 完成定义

Task System v2 的完成不是“21 层都有 crate”，而是满足以下业务能力：

- 简单任务可以快速完成，不创建多余任务结构。
- 中等任务可以任务化执行、展示进度、回收子任务、给出验证。
- 复杂任务可以创建/恢复 Mission，保持目标、计划、事实、验证和人审状态。
- 任何 completed 状态都有证据。
- 任何需要人审的 pending 状态都不能被 prompt 绕过。
