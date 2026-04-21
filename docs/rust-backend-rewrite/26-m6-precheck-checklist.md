# M6 预检清单

更新时间：2026-04-18

> 本文档用于把进入 `M6` 统一切换评估前必须先确认的事项，收口成一份可执行清单。
>
> 它不是最终放行结论；它负责回答“还差什么、证据在哪、谁负责继续推进”。

---

## 1. 当前结论

当前 `M6` 预检结论是：

- 文档与协议冻结台账已补齐
- `cargo test --workspace` 已通过
- 统一切换仍不放行

当前阻塞集中在：

1. `knowledge / memory` 的新主链已经进一步推进到”daemon -> api `/session/action` / `/recovery/resume` / `/task/execute` shadow 执行入口 -> mission.execution.overview -> runtime read model”级别：`ApiState` 与 daemon router 已能真实构造带 recovery support 的 shadow execution pipeline；配置了 `with_context_runtime(...)` 的默认 dispatch 执行入口会自动调用 `assemble_execution_context(...)`，而 `/session/action`、`/recovery/resume` 与 `/task/execute` 这三条默认 shadow 路由现在都已能通过 runtime 公共层的 `ExecutionWritebackPlans` 自动调用 `apply_extraction(...)`；但 extraction 自动回写是否已被所有后续真实调用方复用，仍需在 daemon / bridge / TS 接线阶段继续验证
2. `SettingsStore` 已完成 JSON 文件持久化：daemon 启动时通过 `with_persistence_path` / `load_from_disk()` 恢复，写操作后自动原子写入磁盘
3. 前端 API 端点全覆盖（`agent-api.ts` 104 个函数已全部映射到 Rust 后端路由），所有 `task/chain/interaction` stub 端点已接入事件总线不再返回纯假数据，C-1 TS 契约层全量冻结已完成（`support/frontend-contract` 新增约 80 个 DTO 与约 70 个 client 方法，覆盖所有 Rust 后端路由），前端 `svelte-check` / TS check/build 均已清零错误
4. 真实 provider / MCP / TS 接线尚未执行

---

## 2. 预检清单

| 预检项 | 当前状态 | 证据入口 | 责任 crate / 文档 | 当前阻塞 |
|---|---|---|---|---|
| 文档真相源一致性 | 已满足 | `02 / 05 / 07 / 09 / 25 / 26 / 27 / 28 / 29` | `docs/rust-backend-rewrite/**` | 无 |
| Runtime Read Model 冻结证据 | 已满足 | `07-schema-and-contract-freeze.md`、`crates/magi-event-bus/src/read_model/contract.rs`、`crates/magi-api/src/dto/bootstrap.rs` | `magi-event-bus` + `magi-api` | 无 |
| Session / Workspace / Recovery 长链 | 已满足 | `cargo test -p magi-session-store`、`cargo test -p magi-workspace`、`cargo test -p magi-daemon` | `magi-session-store` + `magi-workspace` + `magi-daemon` | 无 |
| Mission / Worker / Tool 长链 | 已满足 | `cargo test -p magi-orchestrator`、`cargo test -p magi-worker-runtime`、`cargo test -p magi-tool-runtime` | `magi-orchestrator` + `magi-worker-runtime` + `magi-tool-runtime` | 无 |
| Bridge 边界最小冻结 | 部分满足 | `cargo test -p magi-bridge-client`、`cargo test -p magi-api`、`cargo test -p magi-daemon`、`07-schema-and-contract-freeze.md`、`29-idea-host-defer-decision.md` | `magi-bridge-client` + `magi-api` + `magi-daemon` | `openai-compatible` 已能保留 `usage / finish_reason / tool_calls` 的结构化成功响应，能宽容接受结构化 `tool_call.arguments`，并能在多 `choices` 成功响应里回退到第一条可桥接 choice；对 refusal-only 以及”空 content + refusal”成功包也已有稳定 fallback；MCP manager lifecycle 不仅已有 typed JSON-RPC contract、shared-registry round-trip 与 health 恢复护栏，还补了 no-op `update_health` 不回放 stale lifecycle event 的幂等语义；API/daemon 现已新增 `/bridges/preflight` 与 `/bridges/cutover-smoke`，其中前者提供最小 smoke，后者提供 model 成功包与 MCP default-route 的 cutover contract 证据，并进一步把 blank-selection 原因细分为 `invocation_failed / response_not_ok`，同时锁定 `metadata drift / resolved server mismatch` 的 ready-path 归因；`cutover-smoke` 顶层还已稳定导出 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind`；B-3 env-backed provider smoke 路径已确认完整：`magi-daemon` 已有 6+1 条 env-backed provider/MCP 回归测试，新增 transport failure 场景，证明环境配置可稳定反映到 `/bridges/cutover-smoke`；但真实 provider / MCP 生命周期仍未完成，且 `cutover-smoke` 仍保持独立资源、未进入 bootstrap |
| Knowledge 主链接入 | 部分满足 | `cargo test -p magi-knowledge-store`、`cargo test -p magi-context-runtime`、`cargo test -p magi-orchestrator`、`cargo test -p magi-event-bus`、`cargo test -p magi-api`、`20-work-packet-magi-knowledge-store.md` | `magi-knowledge-store` + `magi-context-runtime` + `magi-orchestrator` + `magi-event-bus` + `magi-api` | 已完成”配置 `with_context_runtime(...)` 的默认 dispatch 执行入口 + `/session/action` / `/task/execute` shadow 路由 -> mission.execution.overview -> runtime read model”级消费验证；A-3 调用方级验证已完成：`task/execute` 的成功路径（writeback → runtime read-model）、失败路径（不污染 ownership/memory）、无 pipeline 路径均有测试覆盖；真实仓库扫描与更多消费者仍待补齐 |
| Memory 主链接入 | 部分满足 | `cargo test -p magi-memory-store`、`cargo test -p magi-context-runtime`、`cargo test -p magi-orchestrator`、`cargo test -p magi-event-bus`、`cargo test -p magi-api`、`cargo test -p magi-daemon`、`21-work-packet-magi-memory-store.md` | `magi-memory-store` + `magi-context-runtime` + `magi-orchestrator` + `magi-event-bus` + `magi-api` + `magi-daemon` | 已完成”配置 `with_context_runtime(...)` 的默认 dispatch 执行入口 + `/session/action` / `/recovery/resume` / `/task/execute` shadow 路由 -> mission.execution.overview -> runtime read model”级 context summary 验证；A-3 调用方级验证已完成：`task/execute` 的成功路径（writeback → runtime read-model）、失败路径（不污染 ownership/memory）、无 pipeline 路径均有测试覆盖；`/session/action` 已按 runtime 公共层 `DispatchMemoryExtractionInput + ExecutionWritebackPlans` 统一生成写回计划并在 dispatch 成功后自动 `apply_extraction(...)`，空文本输入也已锁定不会生成伪写回；`/recovery/resume` 现在也已能通过统一 writeback plan 落 recovery extraction，且正文只保留 `diagnostic_summary`、`recovery_id / snapshot_id` 收口到 provenance，并已在 daemon router 三跳闭环里被后续 dispatch 真实消费；`/task/execute` 现在也通过统一的 `ExecutionWritebackPlans` 走 `execute_dispatch_for_session_action`，不再返回 stub；仍未完成对所有 execution runtime 调用方统一生效的 extraction 写回 |
| TS 接线准备 | 已满足 | `27-ts-cutover-wiring-checklist.md`、`support/frontend-contract/**`、`npm run smoke -- --base-url http://127.0.0.1:38123 --json`、`cd web && npm run check` | `docs` + `support/frontend-contract` + `web` | 最小契约层现已覆盖 `bootstrap bridge fields / runtime-read-model / ledger / bridges/services / bridges/preflight / bridges/cutover-smoke / recovery/resume`，并已进一步补 `task/execute`、`task/interrupt`、`interaction` 三类端点的 DTO 与 client 方法；C-1 TS 契约层全量冻结已完成：`support/frontend-contract` 新增约 80 个 DTO 与约 70 个 client 方法，覆盖所有 Rust 后端路由（task/interaction/chain/sessions/workspaces/settings/knowledge/MCP/changes），前端不再需要手写缺失 DTO；repo-level smoke 也已能真实读取本地 daemon；`support/frontend-contract` 的 smoke 还已支持 `MAGI_SMOKE_JSON / MAGI_REQUIRE_CUTOVER_READY / MAGI_FAIL_ON_REASON_CODES` 这类 env-driven gate，以及 `--cutover-url / MAGI_CUTOVER_SMOKE_URL` 这类独立 cutover gate 入口；B-2 repo-level smoke 已扩展 `--check-api-reads` 覆盖 5 条前端常用读取路由；`web` settings 刷新链路、`Header` 顶栏、`BottomTabs` 底栏与 `InputArea` 输入区都已开始消费共享 `cutover-smoke` 快照，其中 settings 统计面已开始直接消费顶层 `reason_code / server_kind` 聚合计数；前端 `agent-api.ts` 的 104 个 API 函数已全部映射到 Rust 后端路由，并新增 `/api/bridges/cutover-smoke` fallback 路由；所有 `task/chain/interaction` stub 端点已接入事件总线；`svelte-check` 0 errors / 0 warnings，TS check/build 全部通过；但更广 TS 消费面仍未完成 |
| 统一切换评估包 | 已满足 | `28-m6-cutover-evaluation-package.md` | `docs` | 评估结论仍是不放行 |

---

## 3. 进入 M6 前必须再次执行的检查

1. 运行 `cargo test --workspace`
2. 复核 [07-schema-and-contract-freeze.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/07-schema-and-contract-freeze.md) 与当前实现是否一致
3. 复核 [09-validation-matrix-and-readiness-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/09-validation-matrix-and-readiness-checklist.md) 中所有“待验证”项是否已经补证
4. 复核 [27-ts-cutover-wiring-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/27-ts-cutover-wiring-checklist.md) 的替换点是否仍与 TS 现状一致
5. 确认 knowledge / memory 新主链已完成上游消费验证，否则禁止进入统一切换

---

## 4. 当前建议

当前最合理的推进顺序是：

1. 先把 extraction 自动回写从 `/session/action` / `/task/execute` 默认 shadow 路由继续抽象到更广 execution runtime 调用方，并继续扩展 knowledge / memory 的更多消费方证据
2. 再启动真实 provider / MCP / TS 接线验证
3. 最后重新执行一次本清单与 [28-m6-cutover-evaluation-package.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/28-m6-cutover-evaluation-package.md)

---

## 5. 本轮新增进展

- `magi-orchestrator` 默认 dispatch 执行入口在配置了 `with_context_runtime(...)` 时，已能自动调用 `ContextRuntime::assemble_execution_context(...)`
- 自动装配出来的 `MissionContextSummary` 已会进入 `mission.execution.overview`，并继续被 runtime read model 吸收
- `magi-api` / `magi-daemon` 现在已能真实构造 shadow execution pipeline；`/session/action` 路由可直接驱动 shadow dispatch，并把 mission 摘要推到 runtime read model
- `magi-daemon` 现已把 `host / model / mcp` loopback bridge 真正接进 `build_api_state(...)`：`SkillDispatchRuntime` 的 bridge dispatch client 与 `/bridges/services` 的 probe snapshot 现在共用同一套 loopback bindings，不再出现 daemon 对外 catalog 为空而内部 runtime 仍各自为政的 split-brain
- `magi-api` / `magi-daemon` 现已新增 `/bridges/preflight`：在 `/bridges/services` 保留静态 probe snapshot 的同时，API 现在还可以直接执行 `host.workspace_roots`、`shadow-model` invoke、`mcp.list_servers` 与 `shadow-mcp.echo.inspect` 这些最小真实 smoke
- `magi-api` 现已补 `openai-compatible ready` 的 bridge preflight 覆盖：只要 model catalog 把该 provider 标成 `ready`，`BridgePreflightSnapshotProvider` 与 `/bridges/preflight` route 都会稳定追加对应 smoke 结果
- `magi-daemon` 真实 loopback preflight 现已补条件一致性校验：若 `/bridges/services` 中 `openai-compatible` 为 `ready`，则 `/bridges/preflight` 必须执行该 smoke；若不是 `ready`，则 preflight 不会误跑这条 provider 调用
- `magi-api` 现已把 `bridge_services / bridge_preflight` 正式并入 `/bootstrap`：bridge catalog 与 smoke 不再只停在独立路由，而是进入统一消费者出口
- `magi-api` / `magi-daemon` 现已新增 `/bridges/cutover-smoke`：它会把 model 成功包契约与 MCP default-route contract 收口成独立只读 cutover 证据面
- `/bridges/cutover-smoke` 当前明确不并入 `/bootstrap`：`bridge_services / bridge_preflight` 继续走 bootstrap 统一导出，cutover smoke 则保持切换前辅助资源，需由消费者显式调用
- `/bridges/cutover-smoke` 现已补顶层 `overall_ok / blocking_check_count / blocking_services` summary：bridge 预检现在不只可读，而且能直接给出 blocking gate 结论
- `/bridges/cutover-smoke` 现已补 service-level `service_ok / blocking_check_count / blocking_targets` summary：调用方现在可以稳定定位到具体 bridge kind 的阻塞面，而不需要自己重扫 checks
- `/bridges/cutover-smoke` 现已补 `mcp_default_route_gate.route_status / route_target / resolved_server / contract_ok`：MCP 默认路由现在已有稳定第二层 gate，调用方不必再依赖 `checks[0].mcp_contract`
- `/bridges/cutover-smoke` 现已补顶层 `blocking_issues` 与稳定 `reason_code`：预检阶段现在可以直接汇总阻塞项并做原因分类，而不必只依赖 `blocking_check_count`
- `/bridges/cutover-smoke` 现已继续把 `MCP blank-selection` 细分成 `mcp_blank_selection_invocation_failed` 与 `mcp_blank_selection_response_not_ok`，并在 ready 路径下稳定区分 `mcp_default_route_metadata_drift / mcp_default_route_resolved_server_mismatch`；调用方对默认路由失败面的判断不再需要把这些情形混成一类
- `web` 的共享 cutover 快照现在已经不只被 settings 消费，`Header` 顶栏、`BottomTabs` 底栏与 `InputArea` 输入区也都已接入同一条消息流与全局 store；其中 `InputArea` 现已真正按 `checking / blocked / error` gate 阻止发送，切换前诊断已经开始进入真实执行入口
- `support/frontend-contract` 现已把 `smoke.ts` 推进成 env-driven cutover gate：支持 `MAGI_SMOKE_JSON / MAGI_REQUIRE_CUTOVER_READY / MAGI_FAIL_ON_REASON_CODES` 与 `--fail-on-reason-codes`，并新增固定严格入口 `npm run smoke:strict`
- `support/frontend-contract` 现已再补 `--cutover-url / MAGI_CUTOVER_SMOKE_URL`：repo-level smoke 现在可直接读取独立 `/bridges/cutover-smoke` 资源，不必总是拉整套 daemon 资源
- `support/frontend-contract` 现已新增可选 `--check-events / MAGI_CHECK_EVENTS`、`npm run smoke:events` 与 `npm run verify:events`：在不改变默认 cutover gate 行为的前提下，repo-level smoke 现在也能补一条最小 `/events` 连通性验证
- `magi-daemon` 现已补 env-backed provider / MCP 组合态与失败路径回归：除了 ready 路径，catalog 中已暴露但处于 `degraded` 的 `openai-compatible`、环境配置驱动的 provider 上游拒绝、`HTTP 200` 但 payload 不可桥接、MCP `fallback-only` 默认路由以及 `unavailable / no-route` 分支也都能稳定反映到 `/bridges/cutover-smoke`
- `/bridges/cutover-smoke` 现已不再把 provider 失败统一压平成 `bridge_invocation_failed`：`model_provider_unavailable / misconfigured / transport_failed / rejected / invalid_response` 已可稳定导出，`BridgeProbeErrorDto` 也已补 `error.code`
- `support/frontend-contract` 现已新增可选 `--check-session-action / MAGI_CHECK_SESSION_ACTION` 与 `npm run smoke:execution`，repo-level smoke 已能在需要时补一条最小 `POST /session/action` 执行脚手架；本机已在真实 daemon 上实跑通过 `smoke:execution` 与 `smoke:task-execute`
- `apps/daemon` 入口现已支持 `MAGI_HOST / MAGI_PORT / MAGI_SERVICE_NAME / MAGI_STATE_ROOT` 覆盖默认配置，真实联调环境不再需要修改源码才能切换监听地址或状态目录
- `support/frontend-contract` 的 smoke 默认 `MAGI_BASE_URL` 现已对齐到 daemon app 默认地址 `http://127.0.0.1:38123`，不再与 app 默认端口错位
- `/session/action` 默认 shadow 路由现在已会在 dispatch 成功后自动调用 `magi-memory-store::apply_extraction(...)`，并且后续同 session dispatch 已能继续消费这条 route 级写回
- `magi-api` 已把 `/session/action` 的 shadow dispatch / extraction 顺序收口到 `shadow_execution.rs` 单一 helper，默认入口不再维护重复的内联副作用逻辑
- `magi-api` 已新增失败回归测试，锁定 dispatch 失败时不写回 extraction、也不绑定 mission / todo / worker ownership
- `magi-orchestrator` 已新增默认 execution runtime 的 `execute_dispatch_then(...)` success hook 入口，`magi-api` 现已复用这条 runtime 层接缝承接 route 级 extraction 写回
- `magi-orchestrator` 现已把 writeback 能力进一步下沉到 runtime 公共层：`ExecutionWritebackPlans`、`execute_dispatch_with_writebacks(...)` 与 `execute_recovery_with_writebacks(...)` 已可直接被非 API 私有调用方复用
- `magi-daemon` 默认 shadow 执行器现已从 `WorkerRuntime::new_compare()` 切到 `WorkerRuntime::new()` / local-process 主线，默认 API 路径更接近真实执行器
- `magi-orchestrator` 已把 `session.action` 的 extraction payload builder 提升到 runtime 公共层：`DispatchMemoryExtractionInput` 与 `ExecutionWritebackPlans::from_session_action_input(...)` 现在统一负责 skill/deep-task 扩展、timeline provenance 与空文本跳过语义
- `magi-api` 现已直接复用 runtime 公共层的 `ExecutionWritebackPlans` 与 `execute_dispatch_with_writebacks(...)` / `execute_recovery_with_writebacks(...)`；route 行为保持不变，但 writeback 能力不再停留在 API 私有模块
- `magi-orchestrator` 已把 recovery 主链补成与 dispatch 同形的 `execute_recovery_then(...)` success hook 接缝，并补了 success / failure 两条回归测试
- `magi-api` 现已把 recovery writeback 真正接到 pipeline：`diagnostic_summary` 进入 memory content，`recovery_id / snapshot_id` 收口到 `source_ref`
- `magi-api` 已新增 `/recovery/resume` 路由，并补 `RECOVERY_NOT_FOUND` / 成功写回两条回归；默认测试态 shadow pipeline 也已接上 recovery support
- `magi-api` 现已把 `/recovery/resume` 的 `Prepared / Consumed` 状态前置收口成稳定 `400 INPUT_INVALID`，并修正 `/runtime/read-model` 的 `active_recovery_ids` 不再把 `Consumed` 计入 active
- `magi-orchestrator` / `magi-api` 已把 recovery resume 的实际执行 worker 对齐成单一真相源：显式 `worker_id` 请求现在会稳定体现在 `decision.worker_id`、session sidecar 与 `/recovery/resume` 响应里
- `magi-api` 已修正 workspace recovery sidecar 与 runtime read model 的合并策略：如果事件聚合侧已有 recovery worker，则不再被 stale ownership worker 覆盖回去
- `magi-workspace` / `magi-orchestrator` 已把 recovery `Ready` 校验下沉到 workspace/runtime 真入口：`build_recovery_resume_input(...)` 与 direct `execute_recovery(...)` 现在都会在 sidecar 写入前拒绝 `Prepared` recovery
- `magi-workspace` / `magi-orchestrator` 现已把 consumed recovery 的 workspace sidecar ownership 对齐到实际恢复结果：`mission_id / todo_id / worker_id / execution_chain_ref` 不再只停留在事件流和 session sidecar
- `magi-api` 现已把 consumed workspace recovery sidecar 在 runtime read model 中收紧成“补洞而非降级”语义：event-sourced `mission_resumed / worker_resumed` 不再被 sidecar 快照覆盖回 `consumed`
- `magi-daemon` 默认 router 现已补通 `session/action -> recovery/resume -> session/action` 三跳闭环，recovery 写回出的 extraction 能在后续 dispatch 中被 context 消费
- `magi-daemon` 现已补一条 bootstrap 消费者验证：follow-up `session/action` 后，`/bootstrap.runtime_read_model` 与 `/runtime/read-model` 保持一致，bootstrap 不会丢失 mission context 摘要
- `magi-daemon` 现已再补一条 recovery bootstrap 消费者验证：`session/action -> recovery/resume -> session/action` 之后，`/bootstrap.runtime_read_model` 与 `/runtime/read-model` 仍保持一致，且 follow-up mission 能稳定看到 recovery extraction ref
- `magi-daemon` 现已补一条 bridge bootstrap 消费者验证：`/bootstrap.bridge_services` 与 `/bootstrap.bridge_preflight` 会稳定分别对齐 `/bridges/services` 与 `/bridges/preflight`
- `magi-bridge-client` 已补 `JsonRpcMcpManagerClient` 与 typed MCP manager contract，并新增 shared-registry 的完整 lifecycle round-trip 回归；`openai-compatible` 也已补 tool-call-only、content-parts、structured tool arguments、多 choice 回退、refusal-only、空 content + refusal 成功路径测试
- `magi-bridge-client` 现已补 `mcp.update_health` 生命周期恢复护栏：health 变回 healthy 后会按 `enabled + health` 统一重算 lifecycle_state，避免状态卡在 `failed / stopped`
- `magi-bridge-client` 现已补 `mcp.update_health` no-op 幂等护栏：当 health 未变化时，响应不会把旧 `lifecycle_event` 误报成当前操作结果
- `magi-bridge-client` 现已补 `enable_server / disable_server` 的 no-op 幂等护栏：重复 enable/disable 不再把历史 `lifecycle_event` 误报成当前操作结果
- `magi-bridge-client` 现已把 `enabled + unavailable` server 收紧为 `non-routable`：空白 selection 不再默认回退到 unavailable server，显式 `mcp.call_tool` 也会稳定返回 `server unavailable`
- `magi-bridge-client` 现已把 MCP manager 的 `default_server` 收紧为真实可空元数据：当没有 routable default route 时，catalog 与 blank-selection error 不再把 `shadow-mcp-manager` 误报成默认 server，而是稳定导出 `default_server = null` / `default_server:<none>` 与 `default_route_target = <none>`
- `magi-event-bus` 已修复“无活跃订阅者时 publish 失败”的稳定性问题，默认 API 写路径不再依赖已有 SSE 订阅
- `magi-event-bus` 已补回归，锁定 `mission.execution.overview.context` 的 `knowledge_source_paths / memory_extraction_refs` 在后续不带 context 的 follow-up overview 下仍会稳定保留
- 本轮验证已通过：
  - `cargo test -p magi-context-runtime`
  - `cargo test -p magi-orchestrator`
  - `cargo test -p magi-event-bus`
  - `cargo test -p magi-api`
  - `cargo test -p magi-bridge-client`
  - `cargo test -p magi-daemon`
  - `cargo test --workspace`（测试数量从 331 增长到 420）
- `/api/task/execute` 已接入真实 shadow execution pipeline：`tasks_interaction.rs` 的 `execute_task` 不再返回 stub，而是检查 pipeline 存在 -> 构建 `SessionActionRequestDto` -> 解析/创建 session -> 记录 timeline -> 调用 `run_shadow_session_action` -> 发布 `task.execute.accepted` 事件 -> 返回真实 `taskId/sessionId/entryId/eventId/acceptedAt/status/createdSession`；这意味着 `/task/execute` 现在也通过统一的 `ExecutionWritebackPlans` 走 `execute_dispatch_for_session_action`
- `SettingsStore` 已添加 JSON 文件持久化：`crates/magi-api/src/settings_store.rs` 新增 `with_persistence_path`、`load_from_disk`、`save_to_disk` 和 `auto_persist`；写操作（`set`/`set_section`/`upsert_array_entry`/`remove_array_entry`/`remove_section_entry`）后自动原子写入磁盘；daemon 启动时通过 `SettingsStore::with_persistence_path(state_root.join("settings.json"))` 构造并调用 `load_from_disk()` 恢复；新增 4 个单元测试
- 前端 API 端点全覆盖：前端 `agent-api.ts` 中的 104 个 API 函数已全部映射到 Rust 后端路由；新增 `/api/bridges/cutover-smoke` fallback 路由（前端 "Rust 根路由优先、`/api` 兼容代理回退" 策略）
- 前端清理完毕：`svelte-check` 0 errors / 0 warnings；TS check/build 全部通过；删除了 3 个死文件，清理了大量未使用导入
- A-3 调用方级验证已完成：`task/execute` 的成功路径（writeback → runtime read-model）、失败路径（不污染 ownership/memory）、无 pipeline 路径均有测试覆盖
- 所有 `task/chain/interaction` stub 端点已接入事件总线，不再返回纯假数据
- `support/frontend-contract` TS 契约层已补 `task/execute`、`task/interrupt`、`interaction` 三类端点的 DTO 与 client 方法
- C-1 TS 契约层全量冻结已完成：`support/frontend-contract` 新增约 80 个 DTO 与约 70 个 client 方法，覆盖所有 Rust 后端路由（task/interaction/chain/sessions/workspaces/settings/knowledge/MCP/changes），前端不再需要手写缺失 DTO
- B-3 env-backed provider smoke 路径已确认完整：`magi-daemon` 已有 6+1 条 env-backed provider/MCP 回归测试，新增 transport failure 场景
- B-2 repo-level smoke 已扩展 `--check-api-reads` 覆盖 5 条前端常用读取路由
- Task Graph 基础设施已落地（TaskStore/task_events/Task Graph API）
- HttpModelBridgeClient 已创建，支持直连 OpenAI-compatible 端点
- 当前测试总数：420（workspace 全量），`svelte-check` 0 error / 0 warning，TS check/build 全部通过
- 本轮未完成项仍然明确保留：
  - extraction 自动回写虽然已下沉到 runtime 公共层，且 `/task/execute` 也已接入，但新的真实调用方是否全部切到该接缝仍需后续 daemon / bridge / TS 接线继续验证
  - 更广真实 bridge / provider / MCP / TS 接线尚未完成
