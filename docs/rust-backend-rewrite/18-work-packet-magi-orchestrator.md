# Agent 任务单：magi-orchestrator 编排与调度主链

更新时间：2026-04-15

---

## 1. 任务名称

- 名称：`magi-orchestrator` 编排与调度主链任务单
- 编号：`WP-ORCH-001`
- 负责 Agent：Orchestrator Agent

## 2. 写域

- 唯一写域：`crates/magi-orchestrator`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - worker 内部执行循环
  - tool 实际执行
  - session / workspace store 实现
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：Mission / Assignment / Todo、plan ledger、dispatch control plane、replan / summary
- 当前实现位置：
  - `src/orchestrator/core/mission-driven-engine.ts`
  - `src/orchestrator/core/dispatch/dispatch-manager.ts`
  - `src/orchestrator/plan-ledger/**`
- 当前问题：
  - 超级调度器问题严重
  - 请求分类、治理、派发、恢复、汇总混装
  - 状态机边界不显式

## 4. 根本原因

1. 编排主链长期集中在少数大对象中
2. dispatch 和 runtime control plane 没有被真正拆开
3. 如果不先在 crate 级收口，Rust 版只会重演当前巨型 orchestrator

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-orchestrator`
  - 拆出 mission model、plan ledger、dispatch policy、runtime control plane、summary
  - 明确 command enum / command result / control plane object，覆盖 mission 创建、assignment/todo 管理、dispatch、worker report 应用与 resume
- 本任务不做什么：
  - 不实现 worker 执行循环
  - 不实现 builtin tool 执行
  - 不直接承载 API 路由
- 与其他 Agent 的边界：
  - worker runtime 只负责执行 assignment
  - tool runtime 只负责执行工具
  - session/workspace 只提供状态与资源

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-orchestrator`
  - `mission`
  - `plan_ledger`
  - `dispatch`
  - `runtime_control_plane`
  - `summary`
- 新增 schema：
  - 若 task protocol 需要冻结，先补 `schema/task-protocol`
- 更新文档：
  - 回写 `D-003`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删主仓运行代码
  - 但禁止在 Rust 侧复刻 `MissionDrivenEngine` 式超级总控

## 7. 语义约束

- 本任务涉及的真相源：
  - Mission / Assignment / Todo 主模型
  - dispatch policy
  - runtime control plane
- 是否涉及协议变化：
  - 是，task protocol 最终会影响外部 read model
- 是否涉及语义偏差台账登记：
  - 是，必须对齐 `D-003`

## 8. 质量要求

必须遵循
[$cn-engineering-standard](/Users/xie/.codex/skills/cn-engineering-standard/SKILL.md)。

额外要求：

- orchestrator 只负责编排，不负责 worker 内部执行
- 任何状态机都必须显式表达，禁止隐式串联

## 9. 验收标准

- 编译：
  - `magi-orchestrator` 可独立编译
- 最小运行验证：
  - Mission / Assignment / Todo 状态流可闭环
  - command enum / command result / control plane object 可直接描述 mission 创建、assignment/todo 管理、dispatch、worker report 应用与 resume
  - dispatch control plane 语义清晰，不依赖复杂 planner
- 协议验证：
  - task protocol 和 runtime state 输入输出可描述
- 清理验证：
  - crate 内无 worker loop、tool execution 混装

## 10. 输出结论

- 已完成内容：
  - 已建立 mission / assignment / todo 的最小强类型状态骨架
  - 已建立 mission 创建、assignment 挂接、todo 挂接的基础接口
- 已建立 worker report 回写、mission execution overview 与 recovery resume dispatch 主链
- 已建立从 `RecoveryResumeInput` 到 `ResumeDispatchDecision` 再到 `worker execute` 的 recovery consume 入口，并把 session / workspace sidecar export 纳入恢复执行结果
- 已建立 worker skill dispatch 观测消费与 mission execution overview 汇总
- 已建立 assignment / todo 维度 skill dispatch 摘要，与 mission 级摘要共享 builtin / bridge / rejected / failed 计数语义
- 已建立 assignment / todo 维度治理摘要，与 mission 级治理摘要共享 allowed / needs approval / rejected / blocked / repair retry 计数语义
- 已建立显式 `OrchestratorCommand / OrchestratorCommandResult / OrchestratorControlPlane` 命令面，可直接调度 mission 创建、assignment/todo 管理、dispatch、worker report 应用与 resume
- 已建立治理决策接入，`ApplyGovernanceDecision` 可覆盖 allow / needs approval / rejected / blocked / repair retry 路径，并同步更新 mission / assignment / todo 进度
- 已在 mission execution overview 中纳入 worker / governance 摘要，控制面可以直接看到治理阻断与 repair retry 计数，并且可按 assignment / todo 维度继续下钻
  - 已建立从 `DispatchDecision` 生成 `WorkerExecutionIntent` 的最小执行入口，并可通过 orchestrator execution runtime 驱动 `worker loop -> tool/skill -> report -> overview` 闭环
  - 已建立显式 execution request 最终化入口，`derive_execution_request(...) / finalize_execution_profile(...)` 现在会把 request/profile/lifecycle 一起收口，避免继续硬编码 one-shot / 隐式 lease 语义
- 已将 `WorkerRuntime::new()` 的默认执行器切到 `LocalProcessWorkerExecutor::cargo_loopback()`，deterministic 仅作为显式 compare 路径 `WorkerRuntime::new_compare()` 保留，不再承担默认回退
- 已适配 worker 本地子进程执行器：当 worker runtime 切到 `LocalProcessWorkerExecutor` 时，orchestrator execution runtime 不再错误注入内存执行驱动，而是直接走本地子进程执行链
- 已补 local process 路径的 execution runtime 回归测试，验证 `dispatch -> local worker execute -> report -> mission overview` 闭环
- 已补 local process 路径下的 tool summary 收口：当 builtin 真正在子进程内执行时，overview 会从 worker tool observation 回填 `ToolExecutionSummary`，避免父进程 `tool_registry` 统计失真
- 已补 local process executor 预探测：execution runtime 在执行前会先消费 `executor probe`，若 capability 不支持 `execute` 或 health 非 `Healthy`，则直接走统一 `WorkerExecutorUnavailable` 错误出口，不再盲目进入 worker execute
- 已补 local process executor capability / version / supported_step_kinds 的稳定暴露；execution runtime 现在还会在派发前校验 intent 所需 step 是否被 executor 支持，缺少步骤时直接拒绝，不再 silent fallback
- 已补 local process executor 的 `execution_mode / affinity / stage_matrix` 稳定能力层，execution runtime 在派发前会先校验 session/workspace affinity，并继续沿用统一拒绝出口
  - 已补 local process executor 的 `descriptor / execution profile` 前置消费层：
    - dispatch 构造出的 `WorkerExecutionIntent` 现在会稳定附带 `execution_profile`
    - orchestrator 已显式收口 `derive_execution_profile(...) / finalize_execution_profile(...)`
    - `execution_profile` 现在由 orchestrator 统一推导 `reuse_policy / binding_scope / requested_process_model / requested_parallelism`
    - orchestrator 现已显式生成 `WorkerExecutorRequest`，不再只把 intent 当作执行器请求来源
    - local process 路径在派发前不再硬编码 one-shot，而是基于 probe 结果最终化 profile
    - execution runtime 会先校验 `request -> probe -> execute` 主链，显式消费 binding scope / process model / parallelism，而不是从 intent 反推
    - local process 路径现已统一走 `probe_for_request(...)`，观测与前置校验都使用同一份显式 request，不再把“无请求 probe”混入 executor readiness
    - 缺少这层能力时统一返回 `WorkerExecutorUnavailable`，不再 silent fallback
- 已补 execution runtime 回归测试，覆盖本地子进程执行器不健康时的前置拒绝路径
- 已补 execution runtime 回归测试，覆盖本地子进程执行器 capability 不足或 step 不受支持时的拒绝路径
- 已补 execution runtime 回归测试，覆盖本地子进程执行器 affinity 不匹配时的前置拒绝路径
- 已补 execution runtime 对 `execution_profile` 的前置消费，local process 路径现在能显式表达“当前 executor 是否具备 reusable session / process model / parallelism 能力”
- 已建立最小 recovery consume 入口：`RecoveryResumeInput -> ResumeDispatchDecision -> session/workspace sidecar sync -> worker resume -> worker execute -> report -> mission snapshot`，恢复链不再只停在 resumed 标记
- 已补 `MissionContextSummary`，orchestrator 现在可直接吸收 `magi-context-runtime` 产出的 context summary，并把 used knowledge / memory、code source path、memory extraction refs 继续导出到 `mission.execution.overview`
- 已补与 `magi-event-bus` 的系统级回归测试，证明 mission execution overview 中的 context summary 会继续进入 runtime read model
- 删除内容：
  - 无
- 未完成边界：
- 尚未实现更细粒度的治理编排策略
- 当前只适配首个 local-process 外部执行器候选，尚未接入最终真实外部执行器后的更完整执行策略
- 当前 orchestrator 只消费 descriptor / profile 这层切换前能力，不直接管理 persistent process 生命周期或 lease 管理
- execution runtime 默认 dispatch 入口现在已可在配置 `with_context_runtime(...)` 时自动装配 context assembly；但 extraction 自动回写还没有抽成对所有调用方统一生效
- deterministic compare 路径仍保留为测试/对照用途，不作为默认执行器回退
- 后续依赖：
  - `magi-worker-runtime`
  - `magi-tool-runtime`
  - `magi-event-bus`
