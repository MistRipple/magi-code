# Agent 任务单：magi-event-bus 事件、审计与用量主链

更新时间：2026-04-16

---

## 1. 任务名称

- 名称：`magi-event-bus` 事件、审计与用量主链任务单
- 编号：`WP-EVENT-001`
- 负责 Agent：Event Agent

## 2. 写域

- 唯一写域：`crates/magi-event-bus`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - UI 渲染逻辑
  - Host 专属桥接实现
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：domain events、audit、usage、rollout、diagnostics
- 当前实现位置：
  - `src/events.ts`
  - `src/usage-authority/**`
  - `src/orchestrator/observability/**`
- 当前问题：
  - 事件主链分散
  - usage、audit、runtime rollout 没有统一模型
  - 部分事件既承担前端通知又承担审计

## 4. 根本原因

1. 旧实现是按功能增长而不是按事件主链增长
2. 没有先定义 domain event / audit event / usage event 的边界
3. 如果不先统一事件模型，后续 read model 与切换都会失控

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-event-bus`
  - 收口 domain events、audit events、usage ledger、rollout recorder 入口
- 本任务不做什么：
  - 不负责 UI 呈现
  - 不直接承载任务状态机
- 与其他 Agent 的边界：
  - 只提供统一事件主链和审计账本
  - 由其他模块消费事件，不反向把 UI 结构塞回来

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-event-bus`
  - `domain_events`
  - `audit_events`
  - `usage_ledger`
  - `publisher`
  - `read_model_input`
- 新增 schema：
  - 若需要稳定对外事件结构，先补 `schema/events`
- 更新文档：
  - 回写 `D-010`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删主仓运行代码
  - 但禁止在 Rust 侧把 UI projection event 直接当成 domain event

## 7. 语义约束

- 本任务涉及的真相源：
  - domain event
  - audit event
  - usage ledger
- 是否涉及协议变化：
  - 是，事件 schema 需要前置冻结
- 是否涉及语义偏差台账登记：
  - 是，必须对齐 `D-010`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

额外要求：

- 不允许把前端通知结构直接当成审计事件
- 事件模型必须支持回放和切换评估

## 9. 验收标准

- 编译：
  - `magi-event-bus` 可独立编译
- 最小运行验证：
  - 事件发布、审计记录、usage 账本链路可成立
- 协议验证：
  - SSE 事件输入结构可稳定描述
- 清理验证：
  - event / audit / usage 边界清晰，无混装

## 10. 输出结论

- 已完成内容：
  - 已建立统一 `EventEnvelope`
  - 已建立 `EventCategory` 分层和 `EventStreamSnapshot`
  - 已支持最近事件回放与实时订阅的统一事件主链
  - 已建立 `EventContext`
  - 已将 recovery / resume / tool invocation 关键事件收口到 `domain` / `audit` 分类
  - 已支持从事件总线导出以 `meta` 为元信息出口、`overview` 为总览入口、`details` 为统一明细出口、`operations` 为稳定操作切面、并补齐按 recovery_id 聚合诊断摘要的统一 `RuntimeReadModelInput`
  - 已将 runtime read model contract 收口为固定 `meta / overview / details / operations / recovery` schema，并把 `contract_version / contract_sections / ordering_strategy / section_ordering_rules / validation / freeze / freeze_gate / freeze_evidence / freeze_report / freeze_consistency / freeze_closure` 提升为统一冻结链路常量与校验入口
  - runtime read model 已补 `meta.ledger` 最小账本状态出口，可表达 schema_version、audit_count、usage_count、next_sequence、last_persist_error，并进一步补齐 `is_persist_healthy`、`last_persisted_at`、`pending_flush`、`readiness`、`cutover_readiness` 等运行态/切换前门槛信号；由 event-bus 启动时注入真实账本状态，sidecar export 不新开顶层 contract，统一由 API 组装层合并进 details 视图
  - 已为统一 `RuntimeReadModelInput` 增加 `contract_version`、`contract_sections`、`ordering_strategy`、`section_ordering_rules`、`validation`、`freeze`、`freeze_gate`、`freeze_evidence`、`freeze_report`、`freeze_consistency`、`freeze_closure` 和显式集合稳定排序，且 `freeze_gate` 已包含 required / satisfied / pending validation refs，避免 schema 冻结前出现字段或顺序漂移
  - 已吸收 `worker.skill_dispatch.observed` / `worker.skill_dispatch.applied` 事件，并在 runtime read model 中输出活动总量、mission/todo/worker 维度 skill dispatch 摘要，以及 diagnostics/attention 中的失败与拒绝信号
  - 已将 governance blocked / approval required / rejected 的最小汇总收入口径加入 runtime read model 的 diagnostics 与 attention
  - 已补 audit / usage ledger 的内存状态、导入/导出、文件落盘骨架，并可从事件流重建 ledger snapshot
  - 已补 ledger 状态查询、启动恢复和发布后自动刷新骨架，bootstrap 可见最小 ledger 状态
  - 已稳定吃到 builtin tool runtime 发出的 `usage` 事件，usage 账本能够按事件流累积 `tool_name / status / risk_level` 等最小运行期用量信息；`meta.ledger` 继续是唯一 ledger 出口，不新增第二套导出
  - 已补 `runtime_ledger_summary()` 与 `runtime_read_model_input()` 的统一收口，`last_persist_error` 在账本持久化失败时会稳定进入 `meta.ledger`，并有 event-bus 回归测试覆盖账本状态、read model 和运行时导出的一致性
  - 已将 `persistence_path` 一并收口进 `meta.ledger`，并补齐 `is_persist_healthy`、`last_persisted_at`、`pending_flush`、`readiness`、`cutover_readiness` 等运行态与切换前门槛信号，使 `audit_usage_ledger_status -> runtime_ledger_summary -> runtime_read_model.meta.ledger` 在 schema_version、count、next_sequence、persistence_path、last_persist_error 上保持单真相源，同时可消费最近一次成功持久化、待刷新状态和最终切换前可用性结论
  - 已将 `system.runtime.maintenance.status` 吸收进统一 runtime read model，并在 `meta.maintenance` 下稳定导出 `maintenance_mode / policy_profile / mode_reason / last_tick_at / sidecar&ledger outcome / tick_interval / policy flags`
  - 已将 `worker.executor.observed` 吸收进统一 runtime read model，并在 `meta.executor` 下稳定导出最近一次 executor capability / health / readiness 快照，同时在 `details.workers` 下补出每个 worker 最近一次 executor 观测字段
  - `meta.executor` 现已进一步补齐：
    - `executor_instance_id / executor_lease_id`
    - `request_id / request_source`
    - `requested_reuse_policy / requested_binding_scope / requested_lease_state / requested_binding_lifecycle / requested_process_lifecycle / requested_process_model / requested_parallelism`
    - `requested_step_kinds`
    - `lease_state / binding_lifecycle / process_lifecycle`
    - `reuse_scope / parallelism_scope`
    - `blocking_issue_count / blocking_issues / is_cutover_candidate`
  - degraded / unavailable executor 信号已继续汇总进 `overview.diagnostics` 与 `operations.attention`
  - `is_cutover_candidate` 现在不再只看 `local-process`，还要求：
    - `process_model == persistent-process`
    - `process_lifecycle == persistent`
    - `executor_instance_id / executor_lease_id` 完整
    - `lease_state == active`
    - `binding_lifecycle == bound`
    - `reuse_scope` 不是 `none`
- 删除内容：
  - 无
- 未完成边界：
  - 尚未补齐更多 domain / audit / usage 事件构造器
  - 尚未把 ledger 持久化接入切换链路
- 后续依赖：
  - `magi-api`
  - `magi-orchestrator`
  - `magi-worker-runtime`
