# Rust 后端重构验证矩阵与切换就绪清单

更新时间：2026-04-17

> 本文档用于把“Rust 后端重构是否完成”从原则描述落到逐域验证矩阵。
>
> 它既服务于多 Agent 并行开发期间的阶段验收，也服务于 `M6` 的统一切换评估。

---

## 1. 文档目的

本文件用于回答 3 个问题：

1. 每个能力域要验证什么，才算真正被 Rust 侧覆盖
2. 语义偏差台账中的高风险问题如何转化为可验证项
3. 在什么情况下必须一票否决统一切换

本文件不是：

- 具体测试代码清单
- 某个 crate 的单元测试说明
- UI 回归测试文档

---

## 2. 能力覆盖验证矩阵

| 能力域 | 当前实现基线 | Rust 目标能力 | 必测行为 | 最小通过条件 | 当前状态 |
|---|---|---|---|---|---|
| Session 创建 / 切换 / 恢复 | `src/session/unified-session-manager.ts` | `magi-session-store` 提供稳定 session aggregate、timeline、notification、恢复输入 | 创建、切换、删除、hydrate、恢复后仍可查询 | session 真相源与读模型分层明确，execution ownership / recovery_id / status / updated_at 可解释，projection 输入、session index 与 execution sidecar store 查询按稳定字典序输出，runtime read model 可导出 session 维度执行活动摘要；execution sidecar store 已独立为专属子结构，并补齐稳定导出视图；bootstrap 组装层已把 session sidecar export 合并进 `details.sessions` | 开发中 |
| Workspace 注册 / Worktree 分配 / 释放 | `src/agent/service/local-agent-service.ts`<br>`src/workspace/worktree-manager.ts` | `magi-workspace` 提供 registry、路径解析、worktree 生命周期 | 注册、枚举、激活、分配、释放、异常回收 | workspace 与 worktree 语义解耦，无宿主污染，projection 输入、workspaces / worktree_allocations / snapshots / recovery_handles 按稳定字典序输出，runtime read model 可导出 workspace 维度执行活动摘要；recovery sidecar store 已独立为专属子结构，并补齐稳定导出视图；bootstrap 组装层已把 recovery sidecar export 合并进 `details.workspaces` | 开发中 |
| Snapshot / Recovery | `src/snapshot-manager.ts`<br>`src/orchestrator/runtime/*resume*` | `magi-workspace` recovery 子模块提供快照与恢复主链 | 快照生成、恢复、续跑、恢复诊断 | 能明确说明恢复点、执行归属、恢复输入和失败原因；`RecoveryResumeInput` 可被执行主链消费；recovery sidecar store / handle 状态机已显式收紧为 `Prepared -> Ready -> Consumed`，且已禁止已消费句柄再次构建恢复输入；lookup / ready / consume / resume / export 构建已收口到专属 store helper；bootstrap 组装层已将 recovery sidecar export 合并进统一导出面 | 开发中 |
| Mission / Assignment / Todo 生命周期 | `src/orchestrator/core/mission-driven-engine.ts`<br>`src/todo/**` | `magi-orchestrator` 提供显式状态机 | 创建、派发、推进、完成、失败、中断、恢复 | 层级关系、状态转换、关联 ID 均强类型且可追踪；worker report 可直接回写 todo 状态，mission 级 execution overview、assignment / todo 级 skill dispatch 汇总观测与 recovery resume dispatch 可独立生成；三层摘要均已能表达 builtin / bridge / rejected / failed dispatch 计数；已补治理决策接入与 `ApplyGovernanceDecision`，可直接描述 mission 创建、assignment/todo 管理、dispatch、worker report 应用、resume 以及治理回写；mission execution overview 已补 assignment / todo 级治理摘要，可继续下钻 allowed / needs approval / rejected / blocked / repair retry 计数；当前也已补 `MissionContextSummary`，可把 context-runtime 产出的 knowledge / memory / truncation 摘要继续注入 mission execution overview；已补最小 orchestrator execution runtime，可从 dispatch decision 生成 execution intent 并驱动 worker execute 主链 | 开发中 |
| Worker 执行 / Review / Verification | `src/orchestrator/worker/autonomous-worker.ts` | `magi-worker-runtime` 提供 worker 生命周期与质量门禁 | 执行、失败、review、verification、repair | worker 生命周期显式，result / termination / verification 语义完整，异常路径可解释，工具调用与 skill dispatch 均可归属到 worker/todo，worker summary 已能表达 builtin / bridge / rejected / failed dispatch 计数，且支持 resume dispatch 接管；已接入治理决策，worker loop 可显式消费 allow / needs approval / rejected / blocked / repair retry 路径；已补队列式 `WorkerRuntimeLoop`，可通过 `enqueue_action` / `enqueue_plan` / `step` / `run_until_idle` 逐步推进 `execute / review / verify / repair / finish / fail`；已补模拟外部执行器与 execution intent，Execute 主链可在 worker-runtime 内闭环驱动 builtin tool invocation、skill dispatch 和 final report；已补本地子进程执行器能力矩阵，稳定暴露 `execution_mode / affinity / stage_matrix / supported_step_kinds / descriptor / execution_profile`，descriptor/profile 现已显式收口 `reuse_scope / parallelism_scope / executor_instance_id / executor_lease_id / reuse_policy / binding_scope`，并已新增显式 `WorkerExecutorRequest` 协议层，把 `request_id / request_source / requested_stage / requested_execution_profile / required_step_kinds` 固定为 probe/execute/observation 的统一输入；`execute / review / verify / repair` 前会显式拒绝不支持的 stage、step、上下文、binding scope、process model 或 parallelism 要求；`WorkerRuntime::new()` 已默认走 local-process 候选执行器，deterministic 只保留 compare 路径 | 待验证 |
| Builtin Tools 执行与审批 | `src/tools/tool-manager.ts`<br>`src/tools/shell/**` | `magi-tool-runtime` 提供 builtin tool registry 与执行主链 | 文件、搜索、shell、process、diff、审批前置检查 | 工具结果状态、审批要求、风险等级统一，execution context 可关联 worker/todo/session/workspace，且支持按上下文汇总查询；已支持在治理前消费 skill runtime 输出的 `ToolExecutionPolicy`；已补 builtin allow/deny 真实执行语义；已补 file.read、search.text、shell.exec、process.inspect、diff.preview 五类 builtin 的真实执行器骨架，并支持 `ToolExecutionInput.input` 的 JSON/raw 双输入约定；已补 builtin access mode 区分与运行时并发写防护，shell.exec 对同一 `workspace_id` / `todo_id` / `cwd` / 路径 claim 可直接拒绝冲突写入，写冲突沿用统一 `Rejected + SandboxPolicy` 语义；runtime read model 可导出 tool 维度活动摘要 | 待验证 |
| Governance / Sandbox / Risk Gate | `src/governance/**`<br>`src/tools/tool-policy.ts` | `magi-governance` 提供统一治理主链 | 审批、阻断、放行、风险解释、sandbox 决策 | 决策来源清晰，可区分 policy 与执行结果；worker control request / outcome / phase 已可覆盖 allow / needs approval / rejected / blocked / repair retry；worker / mission / assignment / todo 级治理摘要已可按相同计数语义下钻；runtime read model 已能汇总 governance blocked / approval required / rejected 的计数与受影响对象列表；当前已补 `GovernanceDecisionTrace` / `GovernanceTarget`，tool / sandbox / path / worker control 四类请求都可导出统一可序列化决策轨迹，显式暴露 action / outcome / summary | 开发中 |
| Event / Audit / Usage Ledger | `src/usage-authority/authority.ts`<br>`src/events.ts` | `magi-event-bus` 提供 domain/audit/usage/ui 分层事件模型 | 事件发射、审计记录、usage 追加、回放基础 | 事件主链统一，resume/recovery 事件具备稳定 context，且可导出带 recovery 诊断摘要的 runtime read-model input；runtime read model 已可表达 `meta.ledger` 最小账本状态（schema version、audit count、usage count、next sequence、last persist error），并由 event-bus 启动时注入真实账本状态；audit / usage ledger 已具备内存状态、导入/导出与文件落盘骨架，SSE 不再兼任全部职责；bootstrap 组装层仅消费这份 ledger，不再开第二套出口；当前 runtime read model 已可继续吸收 mission 级 context summary，在 `details.missions` / `overview.diagnostics` 下稳定导出 knowledge / memory 系统级消费摘要；`meta.ledger.readiness` 已可直接表达账本在最终切换前是否满足可用性门槛 | 开发中 |
| Knowledge Query | `src/knowledge/project-knowledge-base.ts` | `magi-knowledge-store` 提供存储、索引、查询和治理后输出 | 索引、查询、过滤、tags 匹配、摘要、审计关联 | 结果 envelope 稳定，知识类型可区分，已支持索引词项、评分查询、matched_terms、source_ref 与 governed output 独立生成；当前已补 `CodeIndexIngestion` 入口与 `code_source / audit_link / governance_link` sidecar，可让 query 与 governed output 建立最小 code index / audit / governance 联动；`magi-context-runtime` 已验证这些 sidecar 能继续进入 `selected_knowledge`；`magi-orchestrator` / `magi-event-bus` 已验证这些 sidecar 摘要可继续进入 mission execution overview 与 runtime read model；列表与查询均具备确定性排序 | 待验证 |
| Memory Extraction / Compaction | `src/context/layered-memory-store.ts`<br>`src/orchestrator/session-memory/**` | `magi-memory-store` 提供提取、存储、压缩主链 | 提取、写入、读取、偏好记忆、抽取结果、压缩、来源追踪 | 记忆层级稳定，已拆出 preference memory、extraction results 与 compaction history，来源与压缩规则可解释，compaction summary 可输出；当前已补 `apply_extraction`、`extraction_linkage` 与 `verify_extraction_linkage`，可让 extraction result 与 memory record 的关联闭环直接验证；`magi-context-runtime` 已验证 extraction provenance 能继续进入 `selected_memory`；`magi-orchestrator` / `magi-event-bus` 已验证 extraction provenance 与 extraction refs 可继续进入 mission execution overview 与 runtime read model；查询与历史输出具备确定性排序 | 待验证 |
| Context Assembly / Truncation | `src/context/context-manager.ts`<br>`src/context/context-assembler.ts` | `magi-context-runtime` 提供预算、组装、裁剪主链 | budget、recent turns、file summary、shared context、truncation | 上下文输入来源明确，knowledge/memory/turns/shared/file summary 预算独立，knowledge 路径直接消费 governed output 与 total_matches；已具备 `SharedContextPool / FileSummaryStore / ProjectRecentTurnStore / assemble_from_runtime_sources`，recent turns 已支持 session/project 两级来源，并支持双路限额、去重、显式来源优先级与结构化 recent turn 结果；当前已补 assembly 级测试，证明 code index sidecar 与 extraction provenance 能从 knowledge/memory store 继续穿透到上游结果；当前已补 orchestrator / event-bus 级测试，证明这些 assembly 结果可继续进入 mission execution overview 与 runtime read model；裁剪可解释 | 待验证 |
| Skill 注入 | `src/tools/skills-manager.ts`<br>`src/llm/adapter-factory.ts` | `magi-skill-runtime` 提供 skill registry、注入与 allowlist | skill 加载、注入、自定义工具绑定、allowlist 校验 | skill 不再依附超级对象，已支持 prompt priority、custom tool binding、allowlist/denylist 解析和运行时 resolve，resolved context / prompt injection / binding 边界清晰；已支持输出 `SkillToolRuntimePlan`，并显式区分 builtin 请求、bridge-bound 请求与 denied 请求，同时向 tool runtime 提供统一执行策略，向 bridge client 输出带 `bridge_kind / dispatch_action / bridge_target` 的 `BridgeBindingDispatchPlan`，并由 `SkillDispatchRuntime` 统一分流和产出标准化 dispatch 观测，观测层已显式保留 `error_kind / bridge_error_layer / bridge_error_message`；已补统一 builtin / bridge 错误与结果语义；`magi-bridge-client` 已补本地 JSON-RPC over stdio 的最小 transport client，并补 model bridge loopback server 验证回环 | 待验证 |
| MCP Tool Discovery / Call | `src/tools/mcp-manager.ts`<br>`src/tools/mcp-executor.ts` | `bridges/mcp` 提供 registry、discovery、call | server 注册、tool 枚举、prompt 枚举、调用、重连 | shadow MCP loopback 已具备 manager + 多 server + 多工具目录语义，`bridge.describe_services` 可稳定暴露 server `enabled / health / tool_count`，并进一步稳定暴露 `manager_version / registry_profile / registry_manifest / selection_strategy / default_server / default_server_health / default_server_selection_key / selection_targets / service_health / service_health_reason / default_route_status / default_route_target / server_version / server_manifest / capability_profile / selection_key`；manager 现已支持通过环境变量驱动最小 registry 配置、默认 server 偏好、enable/disable 与 health override；当前默认 server 配错、enable/disable 指向未知 server、health override 语法错误或目标未知时，都会显式进入统一 `service_health / service_health_reason`，并通过 `registry_config_status / config_issue_count` 暴露；disabled server 与“无可用默认 server”都会给出显式远端业务错误；`mcp.list_servers / mcp.describe_server / mcp.enable_server / mcp.disable_server / mcp.register_server / mcp.start_server / mcp.stop_server / mcp.deregister_server / mcp.update_health` 现已能作为真实 JSON-RPC 方法被 transport 直接调用；Core Runtime 不直接持有 MCP manager 状态 | 开发中 |
| Host Bridge 核心操作 | `src/host/runtime-host.ts` | `hosts/vscode` / `hosts/idea` 通过稳定协议提供宿主能力 | open_file、reveal_diff、read_diagnostics、read_symbols、terminal_exec、workspace_roots | shadow host loopback 已覆盖 6 类宿主命令，`bridge.describe_services` 与 host payload 均可稳定暴露 `shell_manifest / shell_profile / command_capability_profiles / session_descriptor / workspace_context / context_resolution_boundary`；当前 `VSCode` 已以前置 `real-prehost` 形态稳定暴露 `implementation_source / capability_profile / workspace_roots_source / service_health / service_health_reason / runtime_mode / terminal_exec_mode`，并已能基于本地文件系统返回真实前置结果；`VSCode prehost` 现已支持通过 `MAGI_VSCODE_PREHOST_WORKSPACE_ROOTS` 显式配置 workspace roots，且当配置 roots 全部无效时会进入 `service_health=unavailable` 而不是静默回退；`TerminalExec` 默认仍拒绝，但在显式开启 `allowlisted` 模式、命中允许命令且 working_directory 落在 workspace roots 内时已可执行受控本地命令；`IDEA` 现已明确收口为 `boundary-placeholder` 未实现边界，`service_health=unavailable` 且所有 `host.call` 都显式拒绝；Core Runtime 零 IDE SDK 依赖 | 开发中 |

---

## 3. 语义偏差验证矩阵

本节直接从 [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md) 反推验证项。

| 偏差编号 | 偏差主题 | 验证重点 | 旧行为处置原则 | 验证通过标准 |
|---|---|---|---|---|
| D-001 | Host / Core Runtime 边界污染 | 核验 core crate 是否仍直接依赖宿主 SDK | 旧行为废弃，必须重定义为 Host Bridge 协议 | Core Runtime 不直接引用任何 IDE SDK，宿主能力仅通过桥接进入 |
| D-002 | Agent API / Runtime 混装 | 核验 API 层是否仍持有业务真相 | 旧行为废弃，按新边界拆开 | `daemon -> api -> services/stores` 结构成立，API 不承载业务状态机 |
| D-003 | Orchestrator / Dispatch 超级调度器 | 核验是否已拆为显式子模块和状态机 | 旧行为废弃，重定义内部语义 | 计划、派发、治理、控制面职责清晰，无超级对象吞装 |
| D-004 | Session 聚合与投影混装 | 核验 durable state、projection、sidecar 是否分离 | 旧行为废弃，重定义数据边界 | session aggregate、read model、execution sidecar store / recovery store 分层明确，`unknown` 透传消失 |
| D-005 | 后端运行时与前端投影耦合 | 核验后端是否只输出稳定 read model 和事件 | 旧行为重定义，保留必要对外契约 | 后端不再依赖 UI 表达层结构，读模型与传输层解耦 |
| D-006 | Tool / MCP / Skill / Host 混装 | 核验工具执行、扩展、宿主桥接是否分层 | 旧行为废弃，重定义执行边界 | builtin、skill、mcp、host_bound 只通过协议拼装 |
| D-007 | LLM 运行时与执行环境混装 | 核验模型调用是否退到 bridge 边界 | 旧行为重定义，保留必要 provider 行为 | core 只持有 bridge client，不持有工具/宿主/技能实现 |
| D-008 | Context / Memory / Knowledge 交织 | 核验三层是否拆为独立能力域 | 旧行为废弃，重定义边界 | knowledge、memory、context 只通过稳定输入输出协作 |
| D-009 | Knowledge Store 职责过重 | 核验索引、存储、查询、治理输出是否拆开 | 旧行为废弃，重定义内部结构 | indexer、store、query、governed output 可独立演进 |
| D-010 | Event / Audit / Usage 主链分散 | 核验事件主链是否统一 | 旧行为废弃，重定义事件分层 | domain/audit/usage/ui projection event 可区分且统一收口 |

### 3.1 旧行为保留 / 重定义 / 废弃的判定依据

`保留`

- 已形成稳定外部契约
- 不损害目标领域模型
- 不会导致双真相源

`重定义`

- 外部契约需要保留，但内部语义明显不合理
- 当前行为与目标模型存在可收敛冲突

`废弃`

- 属于历史补丁
- 直接污染核心边界
- 继续保留会阻断平台化

---

## 4. 切换就绪清单

### 4.1 文档核对

进入 `M6` 前必须确认：

- [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md) 已更新到最新状态
- [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md) 的高风险偏差已完成决策
- [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md) 与实际 crate 结构一致
- [07-schema-and-contract-freeze.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/07-schema-and-contract-freeze.md) 与实现一致
- [08-local-shadow-rust-workspace-bootstrap.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/08-local-shadow-rust-workspace-bootstrap.md) 没有被实现偏离
- [26-m6-precheck-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/26-m6-precheck-checklist.md) 已生成并能逐条映射 `M6` 门槛
- [27-ts-cutover-wiring-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/27-ts-cutover-wiring-checklist.md) 已生成并可直接指导 TS 接线前检查
- [28-m6-cutover-evaluation-package.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/28-m6-cutover-evaluation-package.md) 已生成并明确当前“不放行切换”的结论
- [29-idea-host-defer-decision.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/29-idea-host-defer-decision.md) 已固定 `IDEA host` 的延后决策
- 当前任务单的写域边界未被破坏

### 4.2 行为验证

进入 `M6` 前必须完成：

- session 主链验证
- workspace / worktree / snapshot / recovery 验证
- mission / assignment / todo / worker 主链验证
- builtin tool 与治理链验证
- event / audit / usage 主链验证
- audit / usage ledger 导出 / 导入 / 落盘骨架验证
- knowledge / memory / context 长链验证
- skill / MCP / host bridge 关键边界验证

### 4.3 协议稳定性核验

进入 `M6` 前必须确认：

- API 一级资源稳定
- SSE 一级事件分类稳定
- 统一 envelope 稳定
- Host Bridge 核心命令稳定
- Tool Protocol 主字段稳定
- Runtime Read Model 一级视图稳定
- Runtime Read Model 已具备显式 `contract_version`
- Runtime Read Model 已具备显式 `contract_sections`
- Runtime Read Model 已具备显式 `ordering_strategy`
- Runtime Read Model 已具备显式 `section_ordering_rules`
- Runtime Read Model 已具备显式 `validation`
- Runtime Read Model 已具备显式 `freeze`
- Runtime Read Model 已具备显式 `freeze_gate`
- Runtime Read Model 已具备 `freeze_gate.required/satisfied/pending_validation_refs`
- Runtime Read Model 已具备显式 `freeze_evidence`
- Runtime Read Model 已具备显式 `freeze_report`
- Runtime Read Model 已具备显式 `freeze_consistency`
- Runtime Read Model 已具备显式 `freeze_closure`
- Runtime Read Model 中所有集合型输出已做确定性排序，避免顺序漂移造成伪协议变化
- `RuntimeReadModelInput -> bootstrap.runtime_read_model` 的边界已经固定为“event-bus 维护 contract，api 只做 merge 与导出”

当前已具备的最小验证证据：

- `cargo check` 持续通过
- 影子 daemon 可启动
- `/health`、`/version`、`/bootstrap` 已验证
- `/bridges/services`、`/bridges/preflight`、`/bridges/cutover-smoke` 已验证；其中 `bridge_services / bridge_preflight` 已进入 `/bootstrap`，而 `cutover-smoke` 当前仍保持独立只读资源，并已具备顶层 blocking gate summary、顶层 `blocking_issues + reason_code`、顶层 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind`、service-level blocking summary、provider 专属 `model_provider_*` 原因码与 `BridgeProbeErrorDto.code`，以及 `MCP` 的 `mcp_default_route_gate` 稳定诊断面
- `/events` 已可在首连时回放最近事件
- `sessions.json`、`workspaces.json` 已完成原子写落盘
- `magi-worker-runtime` 已输出带 `result_kind / termination_reason / verification_status` 的 report
- `magi-worker-runtime` 已补 `ShadowWorkerExecutor / DeterministicWorkerExecutor / WorkerExecutionIntent / WorkerExecutionIntentStep`，Execute 主链可在 worker-runtime 内闭环驱动 builtin tool invocation、skill dispatch 与 final report
- `magi-orchestrator` 已可消费 worker report 并推进 todo / assignment / mission 状态
- `magi-orchestrator` 已可消费治理决策并通过 `ApplyGovernanceDecision` 回写 todo 进度
- `magi-orchestrator` 已可在 mission execution overview 中输出 assignment / todo 级治理摘要
- `magi-worker-runtime` 已可消费治理决策并将 allow / needs approval / rejected / blocked / repair retry 反映到 loop outcome，且治理结果可进入 runtime diagnostics / attention
- `magi-worker-runtime` 已补队列式 `WorkerRuntimeLoop`，可排队并 step 推进 `execute / review / verify / repair / finish / fail`
- `magi-worker-runtime` 已补模拟外部执行器，Execute step 会先解析 execution intent，再记录 tool observation / skill dispatch observation 并产出 final report
- `magi-knowledge-store` / `magi-memory-store` / `magi-context-runtime` / `magi-skill-runtime` 已具备可独立编译的核心接口
- `magi-skill-runtime` 已具备 `SkillToolRuntimePlan`，并可向 `magi-bridge-client` 输出带 `bridge_kind / dispatch_action / bridge_target` 的 `BridgeBindingDispatchPlan`
- `magi-skill-runtime` 已能显式区分 builtin 请求、bridge-bound 请求与 denied 请求，避免 custom binding 重新落回 builtin allowlist
- `magi-skill-runtime` 已具备 `SkillDispatchRuntime`，调用方不再需要手工判断 builtin/bridge 分流
- `magi-skill-runtime` 已可产出标准化 `SkillDispatchObservation`
- `magi-bridge-client` 已具备 `BridgeDispatchRuntime`，可安全消费 dispatch plan 并构造 host/model/MCP 三类桥接请求
- `magi-bridge-client` 已补本地 JSON-RPC over stdio 的最小 transport client，并补 model / host / MCP 三类 loopback server 验证回环
- `magi-bridge-client` 已把 model bridge 从单一 shadow provider 推进到 provider registry，并补 `openai-compatible` 的 env-configurable prehost skeleton；`bridge.describe_services` 现可直接暴露 provider alias / service_health / service_health_reason / default_model 等前置信号
- `magi-event-bus` / `magi-api` 的 runtime read model 冻结证据链已经收口到单一路径：`RuntimeReadModelInput -> validation/freeze_gate/freeze_report/freeze_consistency/freeze_closure -> bootstrap.runtime_read_model`
- `magi-context-runtime` 已新增 assembly 级测试，证明 `CodeIndexIngestion` 产出的 `code_source / audit_link / governance_link` 与 `apply_extraction(...)` 产出的 extraction provenance 能被上游结果真实消费
- `magi-orchestrator` / `magi-event-bus` 已新增系统级长链测试，证明 `ContextAssemblyResult` 产出的 knowledge / memory 摘要可继续进入 `mission.execution.overview -> runtime read model`
- `magi-bridge-client` 已补统一 `bridge.handshake / bridge.health` 本地进程协议入口与 `JsonRpcBridgeServerProbeClient`
- `magi-bridge-client` 已对 transport / protocol / remote business 错误进行分层，JSON-RPC 标准协议错误码已归入 protocol 层
- `magi-knowledge-store` 已具备 `KnowledgeIndexer / KnowledgeQueryService / GovernedKnowledgeService`
- `magi-memory-store` 已具备 preference memory、extraction results 与 compaction history 分层
- `magi-context-runtime` 已改为直接消费 governed knowledge output，而不是在 context runtime 内重复构造 knowledge 投影
- `magi-context-runtime` 已具备 `SharedContextPool / FileSummaryStore`，shared context 与 file summary 不再只能由调用方直接塞入
- `magi-context-runtime` 已具备 `ProjectRecentTurnStore`，recent turns 不再只能由调用方直接塞入
- `magi-context-runtime` 已对 recent turns 增加 session/project 双路限额与去重治理
- `magi-tool-runtime` 已支持 execution context，并能把调用关联到 worker / todo / session / workspace
- `magi-worker-runtime` 已支持 tool invocation 观测
- `magi-worker-runtime` 已支持 skill dispatch 观测，并已具备带 builtin / bridge / rejected / failed 分解的 worker summary 与 snapshot
- `magi-context-runtime` 已支持 shared context / file summaries / truncation records
- `magi-worker-runtime` 已支持按 worker / todo 导出 execution snapshot
- `magi-worker-runtime` 已补 `LocalProcessWorkerExecutor` 与本地子进程最小协议，Execute 主链可经由本地子进程执行而不改变 `WorkerLoopAction` 语义
- `LocalProcessWorkerExecutor` 已补统一 `probe / execute` 协议、capability / health 输出，以及 `transport / protocol / remote business` 三层失败分层；orchestrator 在 local process 路径下已先探测 executor，再决定是否进入 execute
- `LocalProcessWorkerExecutor` 已稳定暴露 `executor_id / executor_version / supported_step_kinds`，缺少 intent 所需 step 时会显式拒绝，不再 silent fallback
- `LocalProcessWorkerExecutor` 已稳定暴露 `execution_mode / affinity / stage_matrix`，并可在 `execute / review / verify / repair` 前显式拒绝不支持的 stage、step 或 session/workspace context
- `LocalProcessWorkerExecutor` 已稳定暴露 `descriptor / execution_profile`，并可在执行前显式拒绝不支持 binding scope、process model 或 parallelism 要求的 executor
- `WorkerRuntime::new()` 已默认走 `LocalProcessWorkerExecutor::cargo_loopback()`，deterministic 仅保留 `new_compare()` 对照路径，不再作为默认执行器回退
- `magi-worker-runtime` 现会统一发布 `worker.executor.observed`，把 executor capability / health / readiness 收口成审计事件
- `magi-worker-runtime` 现已将 `request_id / request_source / requested_stage / requested_execution_profile / required_step_kinds` 收口进显式 `WorkerExecutorRequest`
- `meta.executor` 现已可直接表达 executor `executor_instance_id / executor_lease_id / request_id / request_source / requested_reuse_policy / requested_binding_scope / requested_lease_state / requested_binding_lifecycle / requested_process_lifecycle / requested_process_model / requested_parallelism / requested_step_kinds / lease_state / binding_lifecycle / process_lifecycle / reuse_scope / parallelism_scope / blocking_issue_count / blocking_issues / is_cutover_candidate`，不再只能看单一 ready 布尔值
- `LocalProcessWorkerExecutor` 现已按显式 request 推导有效 lifecycle；`persistent-process + session/workspace binding` 只有在 `lease_state=active`、`binding_lifecycle=bound`、`process_lifecycle=persistent` 时，才会进入 cutover candidate 判定
- `LocalProcessWorkerExecutor` 本轮已补最小 lease registry：同一 binding 会复用同一 lease、不同 binding 会分 lease、显式 `Released / Expired` 后下一次 acquire 会重新分配；这条语义当前仍是 worker-runtime 内核里的前置候选，而不是最终真实外部执行器生命周期
- `magi-orchestrator` 已支持 mission 级 execution overview
- `magi-orchestrator` 已可消费 worker skill dispatch 观测并汇总到 mission execution overview，同时已生成 assignment / todo 级 skill dispatch 摘要
- `magi-orchestrator` 已可消费 worker governance 观测并将 assignment / todo 级治理摘要纳入 mission execution overview
- `magi-orchestrator` 已补 recovery consume 执行入口，可将 `RecoveryResumeInput -> ResumeDispatchDecision -> worker resume -> worker execute -> report -> mission snapshot` 主链跑通
- `magi-event-bus` 已可吸收 worker skill dispatch 事件，并输出 runtime read model 的活动总量、mission/todo/worker 维度 skill dispatch 摘要，以及 diagnostics/attention 中的失败与拒绝信号
- `magi-event-bus` / `magi-api` 已将 `worker.executor.observed` 收口进 `runtime_read_model.meta.executor`，并在 `details.workers` 下保留每个 worker 最近一次 executor 观测字段
- `magi-event-bus` / `magi-api` 现已将 degraded / unavailable executor 继续汇总进 `overview.diagnostics` 与 `operations.attention`
- `magi-session-store` 已支持 execution ownership、recovery_id、status、updated_at 的强类型 sidecar，并已拆出 execution sidecar store，且相关 apply 路径与导出路径已收口到专属 store helper；已补 dirty 跟踪与 `flush_execution_sidecars_with(...)`，可对刷新的 sidecar 做细粒度落盘
- `magi-workspace` 已支持 worktree / snapshot / recovery 的 session/mission/execution ownership 与 recovery entry points，并已拆出 recovery sidecar store，且 ready / consume / resume / export 构建已收口到专属 store helper；已补 dirty 跟踪与 `flush_recovery_sidecars_with(...)`，可对刷新的 recovery sidecar 做细粒度落盘
- `magi-session-store` / `magi-workspace` 已继续补齐 flush metadata，可稳定暴露 `last_dirty_reason / last_dirty_at / next_flush_hint / last_flush_at`，为 daemon 自动调度 sidecar flush 提供固定输入
- `magi-core` 已冻结 `ExecutionOwnership / RecoveryResumeInput`
- `magi-orchestrator` / `magi-worker-runtime` 已支持消费 `RecoveryResumeInput`
- `magi-orchestrator` 已支持 `RecoveryResumeInput -> ResumeDispatchDecision`
- `magi-worker-runtime` 已支持 `ResumeDispatchDecision -> worker resumed`
- `magi-event-bus` 已支持 `EventContext`
- resume / recovery / tool invocation 关键事件已区分为 `domain` 或 `audit`
- `magi-event-bus` 已支持以 `meta` 为元信息出口、`overview` 为总览入口、`details` 为统一明细出口、`operations` 为稳定操作切面、并补齐 recovery 诊断摘要的统一 `RuntimeReadModelInput`
- `magi-event-bus` 已将 runtime read model contract 收口为固定 `meta / overview / details / operations / recovery` schema，并把 `contract_version / contract_sections / ordering_strategy / section_ordering_rules / validation / freeze / freeze_gate / freeze_evidence / freeze_report / freeze_consistency / freeze_closure` 提升为统一冻结链路常量与校验入口
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加 `contract_version` 与集合稳定排序
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加 `contract_sections` 与 `ordering_strategy`
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加 `section_ordering_rules`
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加 `validation`，可直接自校验 contract 是否漂移
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加 `freeze`，可直接导出 schema 冻结对照输出
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加 `freeze_gate`，可直接判断是否具备 schema 冻结条件
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加验证矩阵引用联动，可直接输出 required / satisfied / pending validation refs
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加 `freeze_evidence`，可直接导出冻结留痕证据摘要
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加 `freeze_report`，可直接输出统一冻结结论
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加 `freeze_consistency`，可直接校验冻结链路内部是否一致
- `magi-event-bus` 已为统一 `RuntimeReadModelInput` 增加 `freeze_closure`，可直接判断冻结链路是否最终闭环
- `magi-event-bus` 已补 audit / usage ledger 的内存状态、导入/导出与文件落盘骨架，并支持启动恢复与运行中自动刷新；runtime read model 已能表达最小 ledger 状态，并由 event-bus 状态注入
- `magi-tool-runtime` 已在 builtin 执行链中稳定补发 `usage` 事件，`meta.ledger.usage_count` 与 `audit_usage_ledger.usage_count` 已能真实反映运行期用量
- `magi-event-bus` / `magi-api` 已将 `audit_usage_ledger_status -> runtime_ledger_summary -> runtime_read_model.meta.ledger -> bootstrap.audit_usage_ledger` 收口成单真相源，并显式覆盖 `persistence_path / last_persist_error` 一致性
- `magi-event-bus` / `magi-api` 已继续补齐 `is_persist_healthy / last_persisted_at / pending_flush` 等 ledger 运行态信号，且 bootstrap 不会吞掉这些信号
- `magi-event-bus` / `magi-api` 已继续补齐 `cutover_readiness`，可把“运行期可用”与“切换前可用”明确分层
- `bootstrap` 已输出 runtime read model DTO，并包含 `meta`、`overview`、`details`、`operations`、`recovery` 诊断摘要，以及最小 `audit_usage_ledger` 状态出口；`details.sessions` / `details.workspaces` 的 sidecar export 已在 bootstrap 组装层合并
- `magi-session-store` / `magi-workspace` 已将 durable 主状态与 sidecar 状态拆成独立持久化入口，daemon 已支持 `session-sidecars.json`、`workspace-recovery-sidecars.json` 与旧单文件兼容读取
- daemon 启动恢复 ledger 后已显式发布 `system.ledger.ready`，事件载荷稳定暴露 `schema_version / audit_count / usage_count / next_sequence / persistence_path / last_persist_error / is_persist_healthy`
- daemon 已补 runtime maintenance tick，可按 sidecar flush metadata 自动刷 sidecar，并在 ledger `pending_flush=true` 时主动刷新账本并重发 `system.ledger.ready`
- daemon 已补 `DaemonMaintenancePolicyProfile / DaemonMaintenanceMode / DaemonRuntimeStatus`，并会发布统一 `system.runtime.maintenance.status` 运行态事件
- daemon 已将 maintenance profile 继续细化为 `ShadowDefault / AggressiveFlush / PreCutoverDrain`，并可稳定导出 eager flush / unhealthy refresh / never persisted refresh 策略信号
- `meta.ledger` 已补 `readiness`，可直接表达账本在最终切换前是否满足可用性门槛
- `meta.ledger` 已补 `cutover_readiness`，可直接表达账本是否满足最终切换前更严格的门槛
- daemon 已补显式 `ShadowRuntimeMaintenancePolicy / Config / State / Report`，maintenance 结果现可稳定区分 `Skipped / DueAndFlushed / DueAndRefreshed / Failed`
- `magi-event-bus` / `magi-api` 已将 `system.runtime.maintenance.status` 收口进 `runtime_read_model.meta.maintenance`，bootstrap 不再吞掉 maintenance 运行态信号
- `magi-bridge-client` 已补 `bridge.describe_services` 下的 host shell manifest / session descriptor / workspace context，以及 MCP manager / server `enabled / health / tool_count` 描述
- `magi-bridge-client` 已补 `run_vscode_host_shell_server()` 与 `run_mcp_manager_server()`，并通过 `implementation_source / default_server` 等字段把 `VSCode real-prehost` 与 `MCP manager default fallback` 前置到统一协议里

### 4.4 代码质量核验

进入 `M6` 前必须确认：

- 没有双实现长期并存
- 没有回退逻辑
- 没有兼容分支掩盖结构问题
- 超级对象已被拆开
- 核心真相源清晰

---

## 5. 一票否决条件

出现以下任一情况时，禁止进入统一切换：

1. `02` 中任一关键能力域仍无法说明“Rust 是否已覆盖”
2. `03` 中任一高风险偏差仍未决策
3. `07` 中稳定契约与实现不一致
4. Core Runtime 仍直接依赖宿主 SDK 或桥接实现细节
5. 关键主链仍依赖旧 TS 运行逻辑作为运行参考
6. 仍需保留大规模兼容分支或回退逻辑
7. 关键状态仍存在双真相源
8. 无法说明切换后 bootstrap / API / SSE / Host Bridge 的稳定形状

---

## 6. 与里程碑的对应关系

`M1`

- 用本文档补齐“如何判断未来验证完成”的标准

`M2`

- 重点验证基础骨架、协议边界与工作区结构未漂移

`M3-M5`

- 按能力矩阵逐域推进
- 按偏差矩阵逐项收口

`M6`

- 以本文档作为统一切换前的最终核验清单

---

## 7. 当前结论

没有本文档，后续多 Agent 即使把 crate 写出来，也很容易出现：

- 功能看似完成，但没有逐域通过标准
- 语义偏差没有真正关闭
- 最后切换时无法判断哪些风险可以接受，哪些必须阻断

因此本文档是从“文档覆盖完整”走向“可判断是否真的完成”的关键一层。

当前这层判断已经不再只是原则说明：

- `M6` 预检清单已落到 [26-m6-precheck-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/26-m6-precheck-checklist.md)
- TS 接线准备已落到 [27-ts-cutover-wiring-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/27-ts-cutover-wiring-checklist.md)
- 统一切换评估结论已落到 [28-m6-cutover-evaluation-package.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/28-m6-cutover-evaluation-package.md)
