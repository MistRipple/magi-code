# Agent 任务单：magi-worker-runtime Worker 执行内核

更新时间：2026-04-15

---

## 1. 任务名称

- 名称：`magi-worker-runtime` Worker 执行内核任务单
- 编号：`WP-WORKER-001`
- 负责 Agent：Worker Agent

## 2. 写域

- 唯一写域：`crates/magi-worker-runtime`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - orchestrator 调度总控
  - tool runtime 内部执行实现
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：worker lifecycle、todo loop、verification、review、repair
- 当前实现位置：
  - `src/orchestrator/worker/autonomous-worker.ts`
  - `src/llm/adapters/worker-adapter.ts`
- 当前问题：
  - Worker 执行循环过重
  - verification / review / repair 与主循环强耦合
  - 状态边界和异常路径不够显式

## 4. 根本原因

1. Worker 执行逻辑长期叠加在单个大类上
2. 执行循环、验证、修复、质量门禁没有独立抽象
3. 如果不先拆 worker runtime，执行内核会继续保持高耦合

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-worker-runtime`
  - 拆分 worker lifecycle、todo execution、verification、review、repair
- 本任务不做什么：
  - 不实现任务全局派发
  - 不实现工具注册
  - 不实现 API 协议出口
- 与其他 Agent 的边界：
  - orchestrator 负责派发 assignment
  - worker runtime 只负责执行 assignment

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-worker-runtime`
  - `lifecycle`
  - `todo_loop`
  - `verification`
  - `review`
  - `repair`
- 新增 schema：
  - 无，当前先立内部执行模型
- 更新文档：
  - 若生命周期语义收敛，回写能力对照表
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删主仓运行代码
  - 但禁止 Rust 侧继续形成单一超级 worker 对象

## 7. 语义约束

- 本任务涉及的真相源：
  - worker lifecycle
  - assignment execution result
  - verification / review state
- 是否涉及协议变化：
  - 间接影响 runtime/read model，但当前先不直接改外部协议
- 是否涉及语义偏差台账登记：
  - 主要对齐 `D-003`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

额外要求：

- worker loop 必须可解释、可诊断、可恢复
- verification / review / repair 不得继续隐式耦合在单循环中

## 9. 验收标准

- 编译：
  - `magi-worker-runtime` 可独立编译
- 最小运行验证：
  - worker lifecycle 可枚举
  - assignment 执行结果可表达
  - verification / review / repair 状态可区分
- 协议验证：
  - 无直接外部协议要求
- 清理验证：
  - crate 内无 orchestrator / API / tool registry 混装

## 10. 输出结论

- 已完成内容：
  - 已建立 worker register / transition / lifecycle 骨架
  - 已建立 `WorkerRecord` 和显式生命周期字段
  - 已建立 tool invocation 观测、execution snapshot 与 recovery resume 接管链
  - 已建立 skill dispatch 观测、skill dispatch snapshot 与统一 worker 观测链
  - 已建立 worker 维度 skill dispatch 汇总，能够表达 builtin / bridge / succeeded / rejected / failed 计数
  - 已建立 `WorkerRuntimeLoop` 作为队列式 control object，支持 `enqueue_action` / `enqueue_plan` / `step` / `run_until_idle`
  - 已建立可按 `execute / review / verify / repair / finish / fail` 前进的 loop action 与 step outcome
  - 已接入 governance decision，worker loop 可显式消费 allow / needs approval / rejected / blocked / repair retry 路径
- 已建立 worker governance 观测与 summary，worker snapshot 可见治理阻断、审批与重试语义
- 已建立 worker governance 观测与 summary，worker snapshot 可见治理阻断、审批与重试语义；上层 orchestrator 已可将这些观测继续汇总到 assignment / todo / mission 级治理摘要
- 已建立 worker loop 的最小单测，覆盖成功循环与无上下文 review 拒绝
- 已将执行器请求从纯隐式校验推进为显式协议层：
  - 已新增 `WorkerExecutorRequest`
  - `intent` 继续描述步骤计划，`request` 专门描述本次执行对 executor 的 `stage / profile / step requirements / request source`
  - `probe / execute / observation` 现在都消费同一份 request 语义，不再各自反推
- 已建立模拟外部执行器：`ShadowWorkerExecutor / DeterministicWorkerExecutor / WorkerExecutionIntent / WorkerExecutionIntentStep`
  - 已建立 `Execute` 主链闭环，能够在 worker-runtime 内依次驱动 builtin tool invocation、skill dispatch、final report，并将结果回写到 tool / skill observation 与 worker report
  - 已建立 Execute 主链单测，覆盖显式注册 intent 与默认 deterministic intent 两种路径
- 已建立本地子进程执行器：`LocalProcessWorkerExecutor`
  - 已冻结统一本地协议：`LocalProcessProtocolRequest / LocalProcessProtocolResponse`
  - 已补 `Probe + Execute` 两类请求边界，不改 `WorkerLoopAction` 顶层语义，也未发明第二套执行协议
  - 已建立 executor 能力/健康探测：`WorkerExecutorProbe / LocalProcessExecutorCapability / LocalProcessExecutorHealth`
  - 已补 executor 标识/版本与受支持 step 集合，`probe` 结果可稳定暴露 `executor_id / executor_version / supported_step_kinds`
  - 已补更接近真实执行器的稳定能力矩阵，`probe` 结果现在还能稳定暴露 `execution_mode / affinity / stage_matrix`，并可描述 `review / verify / repair` 阶段能力
  - 已补 `LocalProcessExecutorDescriptor`，`probe` 结果现在还能稳定暴露：
    - `process_model`
    - `reuse_scope`
    - `parallelism_scope`
    - `max_parallelism`
    - `executor_instance_id`
    - `executor_lease_id`
  - 已补 `WorkerExecutionProfile`，`WorkerExecutionIntent` 不再只描述步骤，还会显式表达：
    - `reuse_policy`
    - `binding_scope`
    - `lease_state`
    - `binding_lifecycle`
    - `process_lifecycle`
    - 请求的 process model
    - 请求的 parallelism
  - 已补 `WorkerExecutorRequest` 到本地子进程协议：
    - `LocalProcessProbeRequest` 现可携带可选 `executor_request`
    - `LocalProcessExecutionRequest` 现固定同时携带 `request + intent`
    - loopback 子进程端在 `probe` 阶段即可显式拒绝 request 不匹配
  - 已补 profile 级能力校验，worker loop 在 `Execute` 前不只会校验 `stage / affinity / step kinds`，还会校验 `execution_profile`；当前可显式拒绝：
    - reusable session / binding scope 不匹配
    - process model 不匹配
    - parallelism 超限
    且不再 silent fallback
  - 已将 `WorkerRuntime::new()` 的默认执行器切到 `LocalProcessWorkerExecutor::cargo_loopback()`，deterministic 仅保留为显式 compare 路径 `WorkerRuntime::new_compare()`
  - 已补 `WorkerExecutorObservation / WorkerExecutorObservationStatus`，worker loop 在 `execute / review / verify / repair` 前会统一记录 `worker.executor.observed`
  - executor 观测现在会稳定暴露：
    - `request_id / request_source`
    - `requested_reuse_policy / requested_binding_scope / requested_lease_state / requested_binding_lifecycle / requested_process_lifecycle / requested_process_model / requested_parallelism`
    - `requested_step_kinds`
    - `executor_kind / executor_id / executor_version`
    - `executor_instance_id / executor_lease_id`
    - `execution_mode / protocol_version / process_model / lease_state / binding_lifecycle / process_lifecycle`
    - `reuse_scope / parallelism_scope / max_parallelism`
    - `strict_session_affinity / strict_workspace_affinity`
    - `supported_step_kinds`
    - `health_status / health_detail`
    - `failure_layer / failure_message`
  - executor failure detail 现在会稳定暴露 `requested_execution_profile / requested_lease_state / requested_binding_lifecycle / requested_process_lifecycle / effective_process_model / effective_lease_state / effective_binding_lifecycle / effective_process_lifecycle / effective_reuse_scope / effective_parallelism_scope`，不再只剩 step 缺口
  - 本地子进程 loopback 在子进程侧也会复用同一套 `execution_profile` 校验，不再出现“父进程校验、子进程绕过”的双轨
  - 本轮已把 request 真正推进到运行态 capability 推导：
    - `probe_for_request(...)` 与子进程 `Probe/Execute` 都会按 request 推导有效 descriptor
    - `persistent-process + session/workspace binding` 现在会导出有效 `executor_lease_id`
    - `Requested -> Active / Requested -> Bound` 已作为合法满足关系进入校验，不再被误判为 lifecycle mismatch
    - `binding_scope=session/workspace` 但缺少对应 `session_id/workspace_id` 时会被显式拒绝
  - 本轮继续把 local-process 候选执行器推进到最小 lease registry 语义：
    - 同一 `session/workspace binding` 的 persistent request 会复用同一 `executor_lease_id`
    - 不同 binding 会分配不同 lease
    - 显式 `Released / Expired` 请求会释放当前 binding lease，并在下一次 acquire 时重新分配
    - 这条 lease 语义当前仍然停留在 worker-runtime 本地 registry，不代表已经具备真实长驻外部执行器
  - `WorkerExecutionSnapshot / TodoExecutionSnapshot / WorkerRuntimeSummary` 已吸收 executor 观测，不再只能看 tool/skill/governance
  - 统一 executor 观测现在已进入 runtime read model 的 `meta.executor`、`details.workers`、diagnostics 与 attention，不再只是 worker 内部快照
  - 已建立失败分层：`transport / protocol / remote business`
  - 已提供 `local_worker_executor` loopback bin，用于通过本地子进程执行最小 `execute` 主链
  - 已建立 `execute_intent_with_shadow_drivers(...)`，确保子进程路径仍然复用 builtin tool / skill dispatch / final report 的既有语义，而不是发明第二套执行协议
  - 已补本地子进程执行回归测试，覆盖 `Execute -> tool observation -> skill dispatch observation -> final report`
  - 已补 probe / health / failure layering 回归测试，覆盖 capability 探测、协议错误、传输错误、远端业务错误、request_id 校验，以及 capability 不足 / step 不受支持时的统一拒绝语义
  - 已补 `max_parallelism` 回归测试，`requested_parallelism` 超限会被 local process executor 显式拒绝
  - 已补 session/workspace affinity 约束与 review / verify / repair 阶段拒绝语义，`Execute` 之外的 loop action 现在也会显式消费 executor stage capability，不再 silent fallback
  - 已补 reusable session 缺失时的显式拒绝语义，并建立对应回归测试
- 已由 orchestrator 的 recovery consume 入口复用现有 `resume_from_dispatch_decision` / `resume_execution` 接管链，未发明第二套恢复协议，并把恢复消费真正推进到 worker execute
- 删除内容：
  - 无
- 未完成边界：
  - 当前本地子进程执行器仍是 shadow loopback，不涉及真实宿主调用
  - 仍未实现真实外部执行器
  - 当前 `LocalProcessWorkerExecutor` 已形成“首个真实外部执行器候选”的能力契约，但默认 loopback 仍不是最终真实 external executor
  - 当前 descriptor / profile 仍是切换前前置能力层，还没有接入真正 persistent process 生命周期与 session lease 管理
  - 仍未补更细的 review / verification / repair 业务策略
- 后续依赖：
  - `magi-orchestrator`
  - `magi-tool-runtime`
  - `magi-governance`
