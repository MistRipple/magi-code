# Magi Rust 后端重构文档目录

更新时间：2026-04-17

> 本目录用于集中管理 `Magi` Rust 后端重构的全部文档。
>
> 当前采用的策略不是“立即接管现有前后端”，而是：
>
> 1. 先在本地影子重构 Rust 后端
> 2. 重构期间不接现有运行链路
> 3. 以能力覆盖、语义校准、代码质量达标为目标
> 4. 待后端整体重构完成后，再统一评估切换

---

## 1. 目录定位

本目录的职责是：

- 统一管理 Rust 后端重构相关的架构、治理、范围、任务和验收文档
- 作为后续多 Agent 并行重构时的文档真相源
- 约束“本地影子重构、不干扰现有结构、最终统一切换”的执行方式

本目录不承担：

- 当前主线前后端需求追踪
- UI 细节设计文档
- 临时排障记录

---

## 2. 文档使用顺序

建议阅读与维护顺序如下：

1. [magi-rust-platformization-master-plan.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/magi-rust-platformization-master-plan.md)
2. [01-governance-and-rules.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/01-governance-and-rules.md)
3. [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
4. [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
5. [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)
6. [05-milestones-and-cutover-gates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/05-milestones-and-cutover-gates.md)
7. [07-schema-and-contract-freeze.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/07-schema-and-contract-freeze.md)
8. [08-local-shadow-rust-workspace-bootstrap.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/08-local-shadow-rust-workspace-bootstrap.md)
9. [09-validation-matrix-and-readiness-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/09-validation-matrix-and-readiness-checklist.md)
10. [26-m6-precheck-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/26-m6-precheck-checklist.md)
11. [27-ts-cutover-wiring-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/27-ts-cutover-wiring-checklist.md)
12. [28-m6-cutover-evaluation-package.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/28-m6-cutover-evaluation-package.md)
13. [29-idea-host-defer-decision.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/29-idea-host-defer-decision.md)
14. [06-agent-work-packet-template.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/06-agent-work-packet-template.md)
15. [25-complete-task-list-and-team-mode-execution.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/25-complete-task-list-and-team-mode-execution.md)

总方案文档已收口到本目录：

- [magi-rust-platformization-master-plan.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/magi-rust-platformization-master-plan.md)

其中：

- 总方案文档负责全局目标、平台边界、能力域映射与整体迁移策略
- 本目录下的其他文档负责后续实际重构过程中的治理、追踪、派工与切换门槛

---

## 3. 文档清单

### 01-governance-and-rules.md

定义本地影子重构的治理原则：

- 当前只做后端重构
- 不接入现有前后端运行链路
- 允许根因级重构
- 强制遵循 `cn-engineering-standard`

### 02-capability-matrix.md

维护“当前 Magi 后端能力清单 vs Rust 重构覆盖度”。

用途：

- 防止重构遗漏能力域
- 防止最后切换时才发现大面积缺口

### 03-semantic-deviation-ledger.md

维护“旧实现语义问题 vs 新后端目标语义”的偏差台账。

用途：

- 防止把不合理旧实现当成迁移标准
- 防止并行重构时因语义漂移失控

### 04-module-mapping-and-target-crates.md

维护现有 `src/` 结构到 Rust crate 的映射关系。

用途：

- 控制重构范围
- 明确后续 crate 所有权和职责边界

### 05-milestones-and-cutover-gates.md

定义重构阶段、阶段出口、统一切换前提。

用途：

- 控制“什么时候能切换”
- 防止在能力覆盖不完整时提前替换

### 07-schema-and-contract-freeze.md

冻结并行重构期间唯一有效的外部协议与对外读模型边界。

用途：

- 防止多个 Agent 各自发明 API / SSE / Host Bridge / Tool Protocol
- 防止旧实现细节反向定义新后端的稳定契约

### 08-local-shadow-rust-workspace-bootstrap.md

定义本地影子 Rust 工作区的默认组织方式与初始化规则。

用途：

- 防止多个 Agent 各自起盘、各自定义 workspace 结构
- 统一 crate 顺序、依赖方向与基础设施约定

### 09-validation-matrix-and-readiness-checklist.md

定义能力覆盖、语义偏差收口与统一切换前的验证矩阵。

用途：

- 防止“文档看起来完整，但没有逐域验证标准”
- 为 `M6` 前切换评估提供一票否决依据

### 06-agent-work-packet-template.md

为后续多 Agent 派工提供统一模板。

用途：

- 明确写域、依赖、验收、清理要求
- 强制所有 Agent 遵循统一工程规范

### 25-complete-task-list-and-team-mode-execution.md

把能力矩阵、验证矩阵、work packet 与实际代码缺口收口成一份完整任务列表。

用途：

- 为“团队模式”下的并行推进提供统一作战图
- 明确当前轮次优先级、依赖关系与建议分组
- 避免只看单篇 work packet 而缺少全局排期

### 26-m6-precheck-checklist.md

生成进入 `M6` 统一切换评估前的可执行预检清单。

用途：

- 为每项切换门槛补齐证据入口、责任 crate 与当前阻塞
- 让 `09` 的验证矩阵可以直接映射到执行动作

### 27-ts-cutover-wiring-checklist.md

收口 TS 链路接线前的替换点清单。

用途：

- 提前明确 API / SSE / bootstrap / host bridge 的替换面
- 避免在真正接线时再反向定义后端语义

### 28-m6-cutover-evaluation-package.md

生成统一切换评估包。

用途：

- 汇总准入条件、风险、回归清单、执行窗口与回滚条件
- 明确当前是否允许进入 `M6`

### 29-idea-host-defer-decision.md

明确 `IDEA host` 在本轮 Rust 重构中的决策。

用途：

- 固定“延后到切换后阶段”的边界，不让 placeholder 继续伪装成可切换能力
- 为未来独立 `IDEA` work packet 留出清晰入口

---

## 4. 维护要求

本目录下文档必须遵循以下约束：

1. 使用中文
2. 不写空泛路线图，必须可执行
3. 发现原有实现不合理时，必须体现在能力矩阵或语义偏差台账中
4. 任何重构任务启动前，必须先对齐能力清单与语义偏差
5. 从 `M1` 进入 `M2` 前，必须先补齐 `07/08/09`
6. 任何准备切换的结论，都必须有明确阶段门槛和验证依据

---

## 5. 当前结论

当前推荐路线是：

> 并行重构 Rust 后端，运行上与现有系统完全隔离，待 Rust 后端整体完成后，再统一切换。

这是当前对 `Magi` 来说更合理、更安全的路线。

当前实际进度已经明显超出“文档准备期”：

- `M1-M2` 已完成
- `M3-M4` 已进入实质覆盖
- `M5` 已进入集中收口阶段

目前最有代表性的收口结果包括：

- 影子 Rust workspace 已通过 workspace 级 `cargo check`
- 影子 Rust workspace 已通过 `cargo test --workspace`
- runtime query contract 已收口为稳定的 `meta / overview / details / operations / recovery`
- knowledge / memory / context / skill runtime 已形成清晰边界，不再只是占位骨架
- skill / tool / bridge 三层分流已经进入统一运行时入口
- `magi-bridge-client` 已补本地 JSON-RPC over stdio 的最小 transport client，并补 model bridge loopback server 验证回环
- `magi-bridge-client` 已把 model bridge 从单一 `shadow-model` 推进到 provider registry 形态，并把 env-configurable `openai-compatible` 推进到最小 HTTP smoke path：可构建真实 `POST /chat/completions` 请求、解析文本响应并稳定映射上游错误边界
- `magi-bridge-client` 已补 model / host / MCP 三类 loopback server，transport / protocol / remote business 错误边界已可端到端验证
- `magi-bridge-client` 已补统一 `bridge.handshake / bridge.health` 本地进程协议入口，可对 model / host / MCP loopback 做一致探测
- `magi-bridge-client` 已补统一 `bridge.describe_services` 本地进程协议入口，并为 model / host / MCP 三类 loopback 暴露最小 service catalog / service shim
- `magi-api` 已补 `/bridges/services` 只读出口，可稳定导出 model / host / MCP 的 `handshake / health / service catalog` 快照，方便 TS 接线前先消费统一桥接快照而不是各自探测
- `magi-bridge-client` 已把 host loopback 的 `shell_manifest / session_descriptor / workspace_context` 收成稳定元信息，并把 MCP loopback 推进到最小 manager + 多 server + `enabled / health / tool_count` 目录语义
- `magi-bridge-client` 已把 `VSCode real-prehost` 推进到最小真实前置实现，`WorkspaceRoots / OpenFile / RevealDiff / ReadDiagnostics / ReadSymbols` 现在会基于本地工作区与文件系统返回真实前置结果
- `VSCode real-prehost` 本轮已把 `TerminalExec` 从“永远拒绝”推进到“默认拒绝 + 显式 allowlisted 开启后可执行受控本地命令”，并强制 `working_directory` 落在当前 workspace roots 内
- `VSCode real-prehost` 本轮继续补齐了 `service_health / service_health_reason / runtime_mode / terminal_exec_mode`，并支持通过 `MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS` 显式配置 workspace roots；当显式配置 roots 全部无效时，prehost 会稳定进入 `unavailable`，不再静默回退
- `IDEA` host shell 现已明确收口为 `boundary-placeholder` 未实现边界：`service_health=unavailable`、`runtime_mode=boundary-only`，不再和 `VSCode real-prehost` 看起来对等可用
- `M6` 预检清单、TS 接线准备清单、统一切换评估包与 `IDEA host` 延后决策文档已经补齐，切换前缺口和阻塞项不再只存在于口头判断里
- `magi-bridge-client` 已继续把 host loopback 收成 `shell_profile / command_capability_profiles / context_resolution_boundary`，并把 MCP loopback 收成 `manager_version / registry_profile / selection_strategy / default_server / server_version / server_manifest / capability_profile / selection_key`
- `magi-bridge-client` 本轮继续把 MCP manager 从“静态目录”推进到“env-configurable registry + default route readiness”前置形态，稳定导出 `service_health / service_health_reason / default_route_status / default_route_target`
- MCP manager 当前已不再静默吞掉坏配置：默认 server、enable/disable、health override 的错误会直接进入统一 `degraded / unavailable` 与 config issue 计数
- `magi-bridge-client` 现已把 MCP manager 的 registry / lifecycle 能力接成真实 JSON-RPC 方法：`mcp.list_servers / mcp.describe_server / mcp.enable_server / mcp.disable_server / mcp.register_server / mcp.start_server / mcp.stop_server / mcp.deregister_server / mcp.update_health` 均可直接通过 transport 调用
- `magi-bridge-client` 已把 host loopback 扩到 `WorkspaceRoots / OpenFile / RevealDiff / ReadDiagnostics / ReadSymbols / TerminalExec`，并把 MCP 从单工具推进到最小 registry / 多工具目录
- `magi-orchestrator` 已新增 `MissionContextSummary`，可直接吸收 `magi-context-runtime` 产出的 knowledge / memory / truncation 摘要，并把这些系统级消费证据继续导出到 `mission.execution.overview`
- `magi-event-bus` 的 runtime read model 现已可继续吸收 mission 级 context summary，在 `details.missions` / `overview.diagnostics` 下稳定导出 used knowledge / memory、code source path、memory extraction refs 等摘要
- builtin tool 五类真实执行器骨架已补齐
- builtin tool 已补 access mode 与并发写防护
- builtin 执行链已稳定补发 `usage` 事件，ledger 最小状态可真实反映运行期用量
- worker 队列式执行循环骨架已补齐
- worker 已补模拟外部执行器与 execution intent
- worker 已进一步补本地子进程执行器，`execute` 主链可经由本地子进程执行而不改变现有 loop 语义
- 本地子进程执行器已补 `probe / execute` 协议、capability / health 输出与 `transport / protocol / remote business` 三层失败分层
- 本地子进程执行器已稳定暴露 `executor_id / executor_version / supported_step_kinds`，缺少步骤能力时会显式拒绝
- 本地子进程执行器已进一步稳定暴露 `execution_mode / affinity / stage_matrix`，`execute / review / verify / repair` 都会做显式能力校验，不再 silent fallback
- 本地子进程执行器已继续补齐 `descriptor / execution_profile`，并把执行器候选契约收口到 `reuse_scope / parallelism_scope / executor_instance_id / executor_lease_id / reuse_policy / binding_scope`；当前可在执行前显式拒绝 binding scope、process model 或 parallelism 不匹配的 executor
- 本地子进程执行器本轮已进一步显式化 `WorkerExecutorRequest`，`request_id / request_source / requested_stage / requested_execution_profile / required_step_kinds` 已成为 probe / execute / observation 的统一请求协议，不再由 loop 内部隐式反推
- worker loop 与 orchestrator control plane 已接入治理闭环
- orchestrator control plane 命令面已补齐
- orchestrator 已补最小 execution runtime，可把 dispatch decision 推进到 worker execute 主链
- orchestrator 已补 recovery consume 执行入口，可把 `RecoveryResumeInput -> ResumeDispatchDecision -> worker resume -> worker execute -> report -> mission snapshot` 主链跑通
- audit / usage ledger 已具备导入、导出、文件落盘与上层接线
- daemon 启动恢复 ledger 后已显式发布 `system.ledger.ready`，并稳定暴露 `persistence_path / last_persist_error / is_persist_healthy`
- `meta.ledger` 已继续补齐 `is_persist_healthy / last_persisted_at / pending_flush`，daemon 现已补 runtime maintenance tick，会在 ledger 待刷新时主动刷盘并重发 ledger 状态
- daemon maintenance 已进一步收口为显式 `Policy / Config / State / Report` 结构，sidecar flush 与 ledger refresh 结果可稳定区分 `Skipped / DueAndFlushed / DueAndRefreshed / Failed`
- daemon maintenance 现已进入统一 runtime read model，`bootstrap.runtime_read_model.meta.maintenance` 可直接看到当前 maintenance mode、策略档位、最近 tick 与 sidecar/ledger 结果
- daemon 已继续补出 `DaemonMaintenancePolicyProfile / DaemonMaintenanceMode / DaemonRuntimeStatus`，并会稳定发布 `system.runtime.maintenance.status`
- `meta.ledger` 已补 `readiness`，可直接表达账本在最终切换前是否满足可用性门槛
- `worker runtime` 现已统一发布 `worker.executor.observed`，`bootstrap.runtime_read_model.meta.executor` 可直接看到最近一次 local-process executor 的 capability / health / readiness 快照
- `meta.executor` 现已继续补齐 `executor_instance_id / executor_lease_id / reuse_scope / parallelism_scope / blocking_issue_count / blocking_issues / is_cutover_candidate`，并把 degraded / unavailable executor 信号收进统一 diagnostics / attention
- `meta.executor` 现已继续补齐 `request_id / request_source / requested_reuse_policy / requested_binding_scope / requested_lease_state / requested_binding_lifecycle / requested_process_lifecycle / requested_process_model / requested_parallelism / requested_step_kinds`，并同步导出 `lease_state / binding_lifecycle / process_lifecycle`，可以直接看到最近一次执行器请求与执行器有效能力之间的关系
- `LocalProcessWorkerExecutor` 本轮继续从“静态 lifecycle 候选”推进到“最小 lease registry”前置形态：同一 session/workspace binding 会复用同一 lease，不同 binding 会分 lease，显式 release 后下一次 acquire 会重新分配
- `LocalProcessWorkerExecutor` 本轮已把显式 request 推进到有效 capability 推导：`persistent-process + session/workspace binding` 现在会生成运行态 `executor_lease_id`，并把 `Requested -> Active / Requested -> Bound` 收口成合法满足关系；缺少 `session_id/workspace_id` 的 binding request 会被直接拒绝
- `LocalProcessWorkerExecutor` 已进入“首个真实外部执行器候选”的契约收口阶段，但默认 local-process loopback 仍不是最终真实 external executor
- session / workspace 已补 sidecar store 与稳定导出视图，并已拆成独立 sidecar 持久化入口
- session / workspace sidecar 已补 dirty 跟踪与统一 flush hook，可对刷新的 sidecar 做细粒度增量落盘
- session / workspace sidecar 已继续补齐 flush metadata，可稳定暴露 `last_dirty_reason / last_dirty_at / next_flush_hint / last_flush_at`，供 daemon 自动调度消费

但当前仍未进入统一切换阶段，原因是：

- host / model / MCP 已具备最小 transport client，且 model / host / MCP 已具备 loopback server 验证回环与 service catalog，但真实 host / MCP 服务端、provider 适配与宿主壳仍未实现
- builtin tool 的并发写防护虽已落地，但更广的执行主链集成仍未完成
- worker 执行循环与 orchestrator control plane 虽已形成治理闭环，且已具备带 probe/health/capability 的本地子进程执行器，但真实外部执行器仍未完全收口
- audit / usage ledger 已进入上层接线，且 bootstrap/runtime ledger 已收口成单真相源，并已具备 `system.ledger.ready` 运行期信号，但尚未进入最终切换链路
- session / workspace 的恢复消费主链已跑通，且 sidecar 持久化已独立拆分并支持增量 flush 与自动调度前置元数据，但更完整切换链路仍未补完
- 现有 TS 链路仍未进入接线或替换阶段

因此，本目录当前的用途已经从“如何起盘”转为：

1. 持续维护 Rust 影子后端的能力覆盖度
2. 维护 M5 收口与 M6 前评估的判断依据
3. 作为后续集中推进和集成收口的唯一文档真相源
