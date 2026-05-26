# Task System v2 风险与验收标准

更新时间：2026-05-17
状态：单方案风险清单（已收敛）

架构已经确定：简单任务走 SessionTurn，中等任务走 ExecutionChain + TaskGraph，复杂任务走 Mission + Long-Mission 闸门。运行时不变式（含 Plan completion gate 与 MissionCharter draft/frozen 生命周期）已全部落地。

## 1. 已定规则

### 1.1 Mailbox

Mailbox 保持 push 模型。外部信号进入 mailbox，Conversation 只在 Turn 边界 drain。

规则：

- 不提供模型主动 peek/pull mailbox 的工具。
- mailbox 项不得插入正在进行的模型 round。
- `agent_spawn` 初始任务消息进入 child Conversation mailbox；代理终态结果由 `agent_wait` 从 TaskStore 收集。

风险：

- 长跑 task 只能在下一次 Turn 看到用户继续输入或系统 followup。

验收：

- 下一次 task turn 能看到该 runtime signal。
- `agent_spawn` 返回 `child_task_id` 后，父 turn 能通过 `agent_wait` 拿到结构化代理结果。

### 1.2 主编排

主编排使用 Prompt-as-Code 协调，但 runtime 执行边界必须硬约束。

规则：

- 主代理可以调用 `agent_spawn` 创建一个或多个代理 task。
- 主代理不能绕过 tool visibility、permissions、SafetyGate、HumanCheckpoint、Validation gate。
- 弱模型不允许自动降级成“无约束编排器”；如果无法满足编排能力，应拒绝或退回 single-task 推进。

风险：

- 模型理解主编排 prompt 失败，导致拆解差或重复 spawn。

验收：

- SpawnGraph 深度和扇出限制生效。
- 工具不可见时调用被 rejected。
- HumanCheckpoint pending 时 spawn 被 rejected。

### 1.3 Validation

Validation 是完成判定的一部分，不是完成后的附加日志。

规则：

- 交付型 Plan step 进入 completed 前必须有 pass 记录。
- fail 未消解时不能 completed。
- skipped 必须有原因，且不能用于关键交付 step。

风险：

- 验证命令过慢，拖慢复杂 Mission。

验收：

- Plan completion gate 有自动测试。
- validation report 能渲染进 long mission prompt。

落地状态：

- `magi-plan::apply_plan_update` 强制要求新置 completed 的 step 必须满足
  `ValidationReport.step_is_passing`，否则返回
  `PlanError::ValidationEvidenceMissing`。已覆盖单元测试。

### 1.4 Checkpoint

Checkpoint 服务恢复，不只是展示摘要。

规则：

- checkpoint 必须记录恢复当前 Mission 所需的最小恢复集。
- 恢复集不完整时必须显式失败。
- checkpoint 不负责回滚用户文件；workspace 指针只用于定位和校验。

风险：

- 只保存摘要会导致“看似恢复，实际丢失 active chain 或 mailbox”。

验收：

- 进程重启后能恢复 active execution chain。
- child 终态结果仍能通过 `agent_wait` 回写 parent turn。

落地状态：

- `magi-checkpoint::Checkpoint::recovery_set_status` 把恢复集校验写进类型层：
  `ProcessRestart / ContextCompaction / PhaseTransition` 三种恢复 kind 必须携带
  非空 `workspace_commit`，且 `open_conversations` 每一项都至少有一条
  `recovery_ref` 或 `execution_chain_ref` 指针。
- `append_checkpoint` 返回 `Result<u32, CheckpointError>`，恢复集不完整时直接
  以 `CheckpointError::IncompleteRecoverySet { kind, reason }` 拒绝落盘，避免
  "看似已 checkpoint，实际无法恢复"。
- `parse_checkpoint_create_arguments` 与 `magi-tool-runtime` 暴露给模型的
  `checkpoint_create` schema 同步收敛到 `session_id` + `recovery_ref` +
  `execution_chain_ref`，强制模型在工具层提供恢复指针。
- **读端聚合恢复入口**：新增 `magi-mission` crate 提供 `MissionAggregate` /
  `resume_mission` / `enumerate_resumable_missions`，把 7 个 Tier 4 store 的
  load 收口到单一聚合根。`resume_mission` 缺 Charter / Plan / CheckpointLog
  即拒绝；最近 Checkpoint 通过 `recovery_set_status` 校验，缺料返回
  `MissionResumeError::LatestCheckpointIncomplete { reason }`——与写端
  `IncompleteRecoverySet` 共用同一个 `MissingRecoverySetReason` 枚举，**读端拒
  绝条件 = 写端拒绝条件**，单源契约。
- **写读契约对称性**：`magi-mission` 的集成测试 `contract_round_trip.rs` 走真
  实写端 `append_checkpoint` 落盘 → 走真实读端 `resume_mission` 读回，断言
  recovery 指针 round-trip 无损；并对称测试写端 / 读端在 workspace_commit
  缺失、conversation pointer 缺失两种破口下都拒绝。把 `CheckpointLog::latest`
  从孤儿 API 升级为 `resume_mission` 的关键依赖。
- 配套收敛：把 8 个 store crate（7 个 Tier 4 + magi-project-memory）各自重复实现的
  `workspace_slug` 收敛到 `magi-core::paths::workspace_slug` / `mission_dir` /
  `missions_root` / `project_memory_root`，消除"同一函数并存多种实现"的违规。
  统一实现采用 char-by-char 处理：`/` 和 `\` 都视作分隔符替换为 `-`，非
  `[a-zA-Z0-9-_.]` 字符替换为 `_`，覆盖 Windows 路径与特殊字符。这是聚合恢复
  能可靠扫描 missions 目录、且与 project-memory 共享 `<magi_home>/projects/<slug>/`
  前缀对齐的前置。
- 单测覆盖：完整恢复集顺序追加、recovery kind 缺 workspace_commit 拒绝、
  conversation 缺指针拒绝、空 `open_conversations` 在 commit 存在时允许、
  parse 校验 session_id 必填与指针互斥规则、序列化往返保留指针；
  `magi-mission` 12 个单测覆盖聚合根的 happy/sad path + 枚举扫描；4 个
  集成测试覆盖 §1.4 写读契约对称。
- 范围说明：Phase A 收口契约层（写端 ✅ / 读端 ✅ / 写读对称 ✅）。**Phase B**
  已落地（daemon bootstrap 自动恢复）：`magi-daemon::daemon::mission_recovery::
  recover_missions_at_bootstrap` 在 `DaemonRuntime::restore` 内被调用，扫描每个
  workspace 下 `<state_root>/projects/<slug>/missions/`，对每个 mission 执行
  `enumerate_resumable_missions` → `resume_mission`，把 `head_checkpoint.
  open_conversations[i].recovery_ref` 通过 `attach_recovery_ref` 回灌到对应
  `SessionRuntimeSidecar`（不存在则 warn 跳过，不伪造），并发布
  `mission.resumed.from_recovery` 事件携带 `EventContext { workspace_id, session_id,
  mission_id }` 与 payload `{ recovery_id, execution_chain_ref, checkpoint_sequence,
  workspace_commit, source: "daemon_bootstrap" }`。下一次 conversation turn 走
  `apply_chain_recovery_if_needed` 时即可消费该 recovery_ref 完成 active chain
  回放——读端契约 → 运行期消费的闭环。daemon 单测覆盖：空 home / 完整 sidecar
  存在 → 事件发布 + recovery_ref 回灌 / sidecar 缺失 → 事件发布但不伪造 sidecar /
  workspace 无 missions 目录静默跳过。

### 1.5 HumanCheckpoint

HumanCheckpoint pending 是 runtime 级阻塞。

规则：

- pending 时禁止新的 agent_spawn。
- pending 时禁止 long mission 派发新的副作用工作。
- resolve 只能从 pending 到 approved/rejected 一次。

风险：

- 如果只写 prompt，不做 runtime 拦截，模型可以误继续执行。

验收：

- pending 状态下 coordinator spawn 被 rejected。
- approved 后任务可继续。
- rejected 后 coordinator 必须回到 Plan 调整。

### 1.6 MissionCharter

Charter 是复杂任务契约，有生命周期。

规则：

- `draft` 阶段允许澄清和补充。
- `frozen` 后直接修改被拒绝。
- `frozen` 后修改必须绑定 HumanCheckpoint approval；每次修改消费一个严格递增的
  `approval_sequence`，避免单次 approval 反复授权。

风险：

- 没有生命周期会在”不可改”和”需要澄清”之间摇摆。

验收：

- draft 更新、freeze、frozen 拒绝、审批后修改都有测试。

落地状态：

- `MissionCharter` 增加 `state: CharterState` + `last_approval_sequence` 字段，
  frontmatter 兼容历史 charter.md（缺省默认为 draft）。
- `mission_charter_write` 工具支持 `freeze` 与 `approval_sequence` 入参；frozen
  写入路径在 `execute_mission_charter_write_tool` 内对 HumanCheckpointStore 做
  status==Approved 校验，再交给 `apply_charter_update` 做序号递增校验。
- 测试覆盖 draft 更新 / freeze 转换 / frozen 直接修改拒绝 / 引用未批准条目拒绝 /
  审批后修改成功 / 同序号复用拒绝 / 历史 charter 读取为 Draft。

## 2. 实现风险

| 风险 | 严重性 | 处理方式 |
|------|--------|----------|
| HumanCheckpoint 只靠 prompt | 高 | ✅ P2 runtime 硬阻塞已落地 |
| Plan completed 无验证证据 | 高 | ✅ P3 Validation gate 已落地（`apply_plan_update`） |
| Checkpoint 无法恢复 active chain | 高 | ✅ §1.4 端到端落地：写端 `IncompleteRecoverySet` 拒绝写入 + 读端 `magi-mission::resume_mission` 拒绝消费缺料 + daemon bootstrap 自动扫描回灌 recovery_ref 并发布 `mission.resumed.from_recovery` |
| Long Mission 激活条件被执行策略字段污染 | 中 | 已收敛为 `TaskTier::LongMission` 显式业务 tier |
| TaskKind 看似支持但 executor 不完整 | 中 | P7 标注成熟度并拒绝 unsupported |
| Coordinator 过度 spawn | 中 | SpawnGraph 深度/扇出 + role visibility |
| 简单任务被过度任务化 | 中 | classifier 和 route 规则持续验证 |
| MissionCharter frozen 后无运行时拦截 | 中 | ✅ §1.6 已落地：CharterState + approval_sequence 递增 |

## 3. 端到端验收场景

### 3.1 简单任务

输入：解释一段代码或运行一次只读工具。

预期：

- route 为 `chat` 或 `execute`。
- 不创建 TaskGraph。
- 不创建 Mission。
- session timeline 有清晰结果和必要工具证据。

### 3.2 中等 single-agent task

输入：修复一个明确 bug 并运行相关验证。

预期：

- route 为 `task`。
- 创建 root task 和 execution chain。
- 不启用 Long-Mission 层。
- root task 完成时有输出摘要和验证记录。

### 3.3 中等 coordinated task

输入：处理多个独立 review comments 或调研多个方案后落地。

预期：

- root task 使用主编排角色。
- 主代理 spawn 子 task。
- child 终态结果通过 `agent_wait` 回写父 turn。
- parent 汇总后完成。
- SpawnGraph open edge 关闭。

### 3.4 复杂 Mission

输入：跨多阶段重构或迁移。

预期：

- 创建或恢复 Mission。
- 有 Charter、Plan、Workspace、KG、Validation、Checkpoint、HumanCheckpoint。
- Plan step 完成受 Validation gate 约束。
- pending HumanCheckpoint 阻止新派发。
- 进程重启后可以从 checkpoint 恢复。

## 4. 完成标准

v2 达标必须同时满足：

- 简单任务路径保持轻量。
- 中等任务路径具备可观察执行链、子任务回执和验证。
- 复杂任务路径具备目标契约、计划、事实、验证、人审和恢复。
- 文档、代码、UI projection 对同一业务对象使用同一语义。
- 不保留并列设计方案或双实现路径。
