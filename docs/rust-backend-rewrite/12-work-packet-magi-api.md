# Agent 任务单：magi-api 路由与协议出口

更新时间：2026-04-16

---

## 1. 任务名称

- 名称：`magi-api` 路由与协议出口任务单
- 编号：`WP-API-001`
- 负责 Agent：API Agent

## 2. 写域

- 唯一写域：`crates/magi-api`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - daemon 生命周期
  - session / workspace / orchestrator 内部状态实现
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)
  - [05-milestones-and-cutover-gates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/05-milestones-and-cutover-gates.md)

## 3. 背景

- 当前能力域：HTTP API 与 SSE 事件出口
- 当前实现位置：
  - `src/agent/service/local-agent-service.ts`
  - `src/shared/session-bootstrap.ts`
  - `src/shared/settings-bootstrap.ts`
- 当前问题：
  - 当前 API 路由和业务状态严重混装
  - bootstrap、workspace/session、knowledge、changes、stats 等入口都挤在同一服务对象内

## 4. 根本原因

1. 旧后端以单服务对象承接所有 API
2. API 层没有被严格限制为“入口协调层”
3. 如果不单独建立 `magi-api`，后续 Rust 后端会继续在 API 层堆业务逻辑

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-api` crate
  - 建立健康检查、bootstrap、workspace/session 基础路由框架
  - 建立 SSE 出口和统一错误映射
- 本任务不做什么：
  - 不承载业务真相源
  - 不实现 orchestrator / worker 执行逻辑
  - 不实现 session / workspace 持久化细节
- 与其他 Agent 的边界：
  - `magi-api` 只做应用层装配和协议出口
  - 业务状态由 store/runtime crates 提供

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-api`
  - `routes`
  - `dto`
  - `errors`
  - `sse`
  - `app_services`
- 新增 schema：
  - 如 DTO 需要冻结，应先在 `schema/` 中登记
- 更新文档：
  - 如路由切分影响切换门槛，回写 `05-milestones-and-cutover-gates.md`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删除主仓运行代码
  - 但禁止在 `magi-api` 内继续形成 `LocalAgentService` 式超级服务

## 7. 语义约束

- 本任务涉及的真相源：
  - 无，`magi-api` 不是核心真相源
- 是否涉及协议变化：
  - 是，任何对外结构冻结都应先走 `schema/`
- 是否涉及语义偏差台账登记：
  - 如发现 API 与 runtime 混装边界需要进一步修订，回写 `D-002`、`D-005`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)，并满足：

- 中文沟通
- 根因导向
- 禁止补丁式修复
- 禁止回退逻辑
- 禁止双实现并存
- 完成后清理废弃代码
- 完成“发现-修复-清理-测试-验证”闭环

额外要求：

- `magi-api` 只做协议出口，不做业务真相源
- 错误模型和 DTO 必须统一

## 9. 验收标准

- 编译：
  - `magi-api` 可独立编译
- 最小运行验证：
  - `/health`
  - `/bootstrap`
  - SSE 基础出口可挂载
- 协议验证：
  - API DTO 形状可冻结
- 清理验证：
  - 无业务状态机混入 route handlers

## 10. 输出结论

- 已完成内容：
  - 已建立 `magi-api` crate
  - 已完成 `/health`、`/version`、`/bootstrap`、`/runtime/read-model`、`/ledger`、`/bridges/services`、`/events` 基础出口
  - `bootstrap` 已改为从 session/workspace/event snapshot 组装
  - `bootstrap` 已接入以 `meta` 为元信息出口、`overview` 为总览入口、`details` 为统一明细出口、`operations` 为稳定操作切面、并补齐 recovery 诊断摘要的统一 runtime read model DTO；runtime read model 的 `details.sessions` / `details.workspaces` 已在 bootstrap 组装层合并 sidecar export 的 `current_status`、`last_update`、`execution_chain_ref`、`recovery_ref`
  - runtime read model DTO 已补 `meta.ledger` 最小账本状态出口，可表达 schema_version、audit_count、usage_count、next_sequence、last_persist_error，并进一步承接 `is_persist_healthy`、`last_persisted_at`、`pending_flush`、`readiness`、`cutover_readiness` 等运行态/切换前门槛信号；与 event-bus 侧保持单真相源对齐，不在 bootstrap 组装层拆出第二套 ledger 视图
  - runtime read model DTO 已补 `meta.maintenance` 出口，可稳定表达 daemon maintenance 的 `maintenance_mode / policy_profile / mode_reason / last_tick_at / sidecar&ledger outcome / tick_interval / policy flags`
  - runtime read model DTO 已补 `meta.executor` 出口，可稳定表达最近一次 worker executor capability / health / readiness 快照；`details.workers` 也已补每个 worker 最近一次 executor 观测字段
  - runtime read model DTO 已继续补 `meta.executor.blocking_issue_count / blocking_issues / is_cutover_candidate`，并同步暴露：
    - `executor_instance_id / executor_lease_id`
    - `request_id / request_source`
    - `requested_reuse_policy / requested_binding_scope / requested_lease_state / requested_binding_lifecycle / requested_process_lifecycle / requested_process_model / requested_parallelism`
    - `requested_step_kinds`
    - `lease_state / binding_lifecycle / process_lifecycle`
    - `reuse_scope / parallelism_scope`
    - degraded / unavailable executor 的 diagnostics 与 attention 字段
  - `runtime read model DTO` 已与 event-bus 侧稳定 schema 完全对齐，包含 `meta / overview / details / operations / recovery` 顶层结构，以及 `contract_version / contract_sections / ordering_strategy / section_ordering_rules / validation / freeze / freeze_gate / freeze_evidence / freeze_report / freeze_consistency / freeze_closure`
  - runtime read model DTO 已补 mission/todo/worker 维度的 skill dispatch 摘要，以及 diagnostics/attention 中的 skill dispatch 失败与拒绝信号
  - runtime read model DTO 已补 governance blocked / approval required / rejected 的最小汇总到 diagnostics 与 attention
  - runtime read model DTO 已补 `meta.contract_version`、`meta.contract_sections`、`meta.ordering_strategy`、`meta.section_ordering_rules`、`meta.validation`、`meta.freeze`、`meta.freeze_gate`、`meta.freeze_evidence`、`meta.freeze_report`、`meta.freeze_consistency`、`meta.freeze_closure`，并已包含 required / satisfied / pending validation refs，与内核侧显式稳定排序、自校验和统一冻结结论输出保持一致
  - `bootstrap` 已补最小 `audit_usage_ledger` 状态出口，可暴露 schema_version、next_sequence、audit_count、usage_count、persistence_path、last_persist_error；同时已新增独立只读 `/runtime/read-model` 与 `/ledger` 路由，但两者继续复用同一套组装路径，不在 API 层形成第二真相源
  - `bootstrap` 组装层现在会强制把 `runtime_read_model.meta.ledger` 与同一份 `audit_usage_ledger` 状态在 schema_version、count、next_sequence、persistence_path、last_persist_error 上保持一致，同时保留 `runtime_read_model` 自身携带的 `is_persist_healthy`、`last_persisted_at`、`pending_flush`、`readiness`、`cutover_readiness` 运行态/门槛信号；已补一致性回归测试
  - 已新增 `bridges/services` 只读桥接快照出口，可稳定导出 `model / host / mcp` 的 `handshake / health / service catalog`，并支持通过 probe transport 或 snapshot provider 注入；API 继续只做只读装配，不成为桥接业务真相源
  - `/bridges/cutover-smoke` 现已补顶层 `blocking_issues` 与稳定 `reason_code`：桥接 cutover gate 现在不仅能判定是否阻塞，还能直接解释阻塞原因；调用方可先读 `overall_ok`，再读 `blocking_issues`，最后才按需下钻到 `services / checks`
- `/events` 已支持最近事件回放后再进入实时订阅
- 删除内容：
  - 无
- 未完成边界：
  - 尚未补齐其他一级资源路由
  - 尚未建立统一错误码映射
- 后续依赖：
  - `magi-session-store`
  - `magi-workspace`
  - `magi-event-bus`
