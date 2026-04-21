# M6 统一切换评估包

更新时间：2026-04-18

> 本文档用于统一回答一个问题：
>
> “当前 Rust 后端是否已经可以进入 `M6` 统一切换？”

---

## 1. 当前评估结论

当前评估结论：`可进入前后端正式对接`

最后收口任务树（doc 30）的 4 条终点定义已全部满足：

1. **Extraction 自动回写统一复用** — 已满足。A-1 审计确认生产态仅 3 条真实执行入口（`/session/action`、`/recovery/resume`、`/task/execute`），全部走统一 `ExecutionWritebackPlans`；其余 10 个 task/chain/interaction stub 已全部接入事件总线，不再有纯假数据返回；A-3 调用方级验证（success writeback、failure isolation、no-pipeline error）全部通过
2. **Provider / MCP 最小真实 smoke** — 已满足。env-backed provider smoke 已有 7 条测试覆盖（成功 / 拒绝 / degraded / invalid response / transport failure / MCP fallback-only / MCP unavailable），失败稳定反映到 `/bridges/cutover-smoke.blocking_issues`
3. **TS 契约层覆盖 Rust 稳定出口** — 已满足。`support/frontend-contract` 已冻结 ~80 个 DTO + ~70 个 `RustDaemonClient` 方法，覆盖所有 Rust 后端路由；repo-level smoke 支持 env-driven gate、cutover-url、api-reads、events、session-action、task-execute 六类可选检查
4. **M6 回归给出明确结论** — 已满足。`cargo test --workspace` 420 tests / 0 failures；`svelte-check` 0 errors / 0 warnings；`vite build` 通过；`npm run check`（frontend-contract）通过

补充 live 证据：

- `apps/daemon` 现已支持 env-driven 启动配置（`MAGI_HOST / MAGI_PORT / MAGI_SERVICE_NAME / MAGI_STATE_ROOT`）
- 本机已在真实 `cargo run -p magi-daemon-app` 条件下复现无 provider 配置时的 `model_provider_unavailable`
- 本机已在真实 daemon + 本地 OpenAI-compatible stub 条件下跑通 `npm run smoke:strict`、`npm run smoke:execution`、`npm run smoke:task-execute`
- 因此当前“可进入前后端正式对接”的结论，已经不只依赖 repo 内单测和只读脚手架

进入对接阶段后仍需关注的风险项见 §4。

---

## 2. 已满足的准入条件

- `cargo test --workspace` 已通过
- 15 个 crate 的结构治理已完成
- Runtime Read Model 冻结证据已经文档化
- `M6` 预检清单已经生成
- TS 接线准备清单已经生成
- `IDEA host` 是否进入本轮已明确做出延后决策

---

## 3. 当前阻塞项

| 阻塞项 | 当前状态 | 对切换的影响 |
|---|---|---|
| Knowledge / code index / audit link 上游消费验证 | 部分满足 | knowledge sidecar 已进入 context assembly，并已在配置 `with_context_runtime(...)` 的默认 dispatch 执行入口与 `/session/action` / `/task/execute` shadow 路由下继续进入 mission overview / runtime read model；`/task/execute` 现在也通过统一的 `ExecutionWritebackPlans` 走 `execute_dispatch_for_session_action`；`tasks_interaction.rs` 的全部 15 个端点已不再有纯假数据返回（execute_task 走真实 shadow execution，interrupt/interaction 系列 5 个走事件总线，其余 10 个 stub 也已全部接入事件总线）；A-3 调用方级验证已全部完成（3 tests: success writeback, failure isolation, no-pipeline error）；真实仓库扫描与更多消费者仍未补齐 |
| Memory extraction 上游消费验证 | 部分满足 | memory 提取闭环已在 store 内成立，默认入口与 `/session/action` shadow 路由也已能自动导出 context summary，且 `/session/action` 已在 dispatch 成功后通过 pipeline 级 writeback plans 自动调用 `apply_extraction(...)`，空文本输入也已锁定不会产生伪写回；`/recovery/resume` 现在也已能通过统一 writeback plan 落 recovery extraction，且正文只保留 `diagnostic_summary`、`recovery_id / snapshot_id` 收口到 provenance，并已在 daemon router 三跳闭环中被后续 dispatch 真实消费；`/task/execute` 现在也通过统一的 `ExecutionWritebackPlans` 走 `execute_dispatch_for_session_action`，不再返回 stub；`tasks_interaction.rs` 全部 15 个端点已不再有纯假数据返回；A-3 调用方级验证已全部完成（success writeback、failure isolation、no-pipeline error）；同时 recovery API 的 `Prepared / Consumed / Missing` 错误语义已稳定化，read model 的 `active_recovery_ids` 也不再把 `Consumed` 误算为 active；但 extraction 自动回写仍未抽成对所有 execution runtime 调用方统一生效 |
| 真实 provider / MCP 接线 | 未完成 | model 已到 HTTP smoke path，且 `openai-compatible` 成功响应现在已能保留 `usage / finish_reason / tool_calls`，并补了 tool-call-only / content-parts / structured tool arguments / multi-choice fallback / refusal-only / 空 content + refusal 成功路径测试；MCP manager lifecycle JSON-RPC 已可调用、已有 typed contract，并补了 shared-registry round-trip 回归；API/daemon 现已新增 `/bridges/preflight` 与 `/bridges/cutover-smoke`，前者提供最小 smoke，后者提供 model 成功包与 MCP default-route 的 contract 证据，但模型与扩展生态仍停留在 shadow/prehost 级前置形态 |
| TS 实际接线 | 已满足（C-1） | `support/frontend-contract` TS 契约层已全量冻结（约 80 个 DTO 接口、约 70 个 `RustDaemonClient` 方法，覆盖所有 Rust 后端路由），C-1 终点定义已满足；此前已补齐 bootstrap bridge 字段、runtime query、bridge gate 与 recovery resume 的最小 DTO/client 契约层，并新增且实跑通过 repo-level `npm run smoke` 脚手架；smoke 现已支持 `MAGI_SMOKE_JSON / MAGI_REQUIRE_CUTOVER_READY / MAGI_FAIL_ON_REASON_CODES` 这类 env-driven gate，也支持 `--cutover-url / MAGI_CUTOVER_SMOKE_URL` 直接对独立 cutover 资源做放行判断；`web` settings 刷新链路也已开始显式消费 `cutover-smoke`，且实现为”Rust 根路由优先、`/api` 兼容代理回退”；同时 cutover smoke 已进入共享消息流和前端全局 store，并被 settings、`Header`、`BottomTabs`、`InputArea` 四个真实消费面复用，其中 settings 已开始直接读取顶层聚合计数而不是前端重复统计；前端 `agent-api.ts` 的 104 个 API 函数已全部映射到 Rust 后端路由，并新增 `/api/bridges/cutover-smoke` fallback 路由；`svelte-check` 0 errors / 0 warnings，`vite build` 通过，删除了 3 个死文件并清理了大量未使用导入；新增 `--check-api-reads` smoke 覆盖 5 条前端常用读取路由（settings/bootstrap、workspaces、workspace-sessions、bridges-services、bridges-preflight），B-2 终点定义已满足；但更广 TS 消费面、切换窗口、兼容风险与回滚路径还不能实证 |

---

## 4. 风险清单

1. 若在 knowledge / memory 新主链尚未完成 extraction 自动回写的更广入口收口与更多消费者验证前直接切换，这两块能力会停留在”`/session/action`、`/recovery/resume` 与 `/task/execute` 已能驱动统一 writeback plan，但其他调用方仍不够统一”的状态。
2. 若在真实 provider / MCP 未完成前直接切换，桥接协议虽然稳定，但生态行为仍会偏影子实现。
3. 若在 TS 接线仍只覆盖 repo-level smoke 与 settings cutover banner 的阶段直接切换，真实替换面与回滚路径仍无法通过完整集成级验证。

---

## 5. 回归清单

切换前至少需要再次执行：

1. `cargo test --workspace`
2. `cargo test -p magi-bridge-client`
3. `cargo test -p magi-event-bus`
4. `cargo test -p magi-api`
5. `cargo test -p magi-knowledge-store`
6. `cargo test -p magi-memory-store`
7. `cargo test -p magi-context-runtime`
8. `cargo test -p magi-orchestrator`

---

## 6. 建议执行窗口

建议分三段推进，而不是一步切换：

1. 先把 extraction 自动回写从 `/session/action` / `/task/execute` 默认 shadow 路由继续抽象到更广 execution runtime 调用方，并继续补 knowledge / memory 的更多消费者证据
2. 再做真实 provider / MCP / TS 的接线 smoke
   当前 `openai-compatible` 已具备最小 HTTP smoke path，但还不是完整 provider 适配层
3. 最后重新生成本评估包并决定是否进入 `M6`

---

## 7. 回滚条件

若后续进入真实接线阶段，出现以下任一情况必须回滚到“继续影子运行”：

1. bootstrap contract 与 [07-schema-and-contract-freeze.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/07-schema-and-contract-freeze.md) 不一致
2. SSE 事件面出现双真相源
3. Host / Model / MCP 真实边界需要依赖临时兼容分支才能跑通
4. `cargo test --workspace` 不能稳定通过

---

## 8. 本轮新增进展

- `magi-orchestrator` 默认 dispatch 执行入口在配置了 `with_context_runtime(...)` 时，已能自动调用 `ContextRuntime::assemble_execution_context(...)`
- 这条默认入口现在会自动导出 `MissionContextSummary`，并继续进入 `mission.execution.overview` 与 runtime read model
- `magi-api` / `magi-daemon` 现在已能真实构造 shadow execution pipeline；`/session/action` 路由可直接驱动 shadow dispatch，并把 mission 摘要推到 runtime read model
- `magi-daemon` 现已把 `host / model / mcp` loopback bridge 真正接进 `build_api_state(...)`：内部 `SkillDispatchRuntime` 的 bridge dispatch client 与 `/bridges/services` 的 probe snapshot 现在共用同一套 loopback bindings，daemon 对外 bridge catalog 不再退回空快照
- `magi-api` / `magi-daemon` 现已新增 `/bridges/preflight`，可以直接执行 `host.workspace_roots`、`shadow-model` invoke、`mcp.list_servers` 与 `shadow-mcp.echo.inspect` 这些最小真实 smoke，切换前的 bridge smoke 已经不再只能依赖静态 catalog
- `magi-api` 现已补 `openai-compatible ready` 的 bridge preflight 覆盖：当 model catalog 把该 provider 标成 `ready` 时，preflight provider 与 `/bridges/preflight` route 都会稳定追加对应 smoke 结果
- `magi-daemon` 真实 loopback preflight 现已补条件一致性校验：若 `/bridges/services` 中 `openai-compatible` 为 `ready`，则 `/bridges/preflight` 必须执行这条 provider smoke；若不是 `ready`，则不会误跑该调用
- `magi-api` 现已把 `bridge_services / bridge_preflight` 并入 `/bootstrap`：bridge catalog 与 smoke 不再只停在独立路由，而是进入统一消费者出口
- `magi-api` / `magi-daemon` 现已新增 `/bridges/cutover-smoke`，把 model 成功包契约与 MCP default-route contract 收成独立只读 cutover 证据面
- `/bridges/cutover-smoke` 当前明确保持独立于 `/bootstrap`：它增加的是切换前评估证据，不是 bootstrap 统一消费者出口的继续扩容
- `/bridges/cutover-smoke` 现已补顶层 `overall_ok / blocking_check_count / blocking_services` summary：bridge cutover 现在已有机器可读的 gate snapshot，但这不会改变“真实 provider / MCP / TS 接线仍未完成”的总体结论
- `/bridges/cutover-smoke` 现已补 service-level `service_ok / blocking_check_count / blocking_targets` summary：bridge cutover 的阻塞定位现在已经有稳定的第二层读取面
- `/bridges/cutover-smoke` 的 MCP service 现已补 `mcp_default_route_gate.route_status / route_target / resolved_server / contract_ok`：默认路由 gate 现在已有稳定 machine-readable 诊断面
- `/bridges/cutover-smoke` 现已补顶层 `blocking_issues` 与稳定 `reason_code`：bridge cutover gate 现在已经是“可解释的机器可读快照”，不只是 block 结论
- `/bridges/cutover-smoke` 现已再补顶层 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind`：调用方现在可以直接读取聚合后的 gate 统计，而不必在 TS / CI 侧重复统计 `blocking_issues`
- `MCP default-route` 的 `describe_server` 失败现在会把底层 bridge error 保留回 `blocking_issues[i].error` 与 route 输出；cutover gate 已从“知道失败”进一步收口到“知道失败发生在哪一层、具体错误是什么”
- `MCP default-route` 的 blank-selection 失败现在也已继续细分成 `mcp_blank_selection_invocation_failed` 与 `mcp_blank_selection_response_not_ok`，ready 路径还已稳定暴露 `mcp_default_route_metadata_drift / mcp_default_route_resolved_server_mismatch`；默认路由 gate 已能区分“根本没有成功发起 blank-selection 调用”“调用成功但上游明确返回 `ok=false`”“默认路由元数据漂移”“解析出的 server 与目标不一致”
- `web` 的 cutover gate 共享快照现在已被 `settings`、`Header` 顶栏、`BottomTabs` 底栏与 `InputArea` 输入区四个真实消费面复用；其中 `InputArea` 已真正按 `checking / blocked / error` gate 阻止发送，前端切换前诊断开始进入真实任务入口
- `support/frontend-contract` 现已把 `smoke.ts` 推进成 env-driven gate：支持 `MAGI_SMOKE_JSON / MAGI_REQUIRE_CUTOVER_READY / MAGI_FAIL_ON_REASON_CODES` 与 `--fail-on-reason-codes`，repo 内 cutover smoke 已更接近真实 CI / 放行使用方式
- `support/frontend-contract` 现已新增固定严格入口 `npm run smoke:strict`：repo 内已经存在“默认要求 cutover ready”的最小放行脚本
- `support/frontend-contract` 现已新增可选 `--check-events / MAGI_CHECK_EVENTS`、`npm run smoke:events` 与 `npm run verify:events`：repo 内 smoke 现在可以在保持默认 gate 语义不变的前提下，再补一条最小 `/events` 连通性验证
- `support/frontend-contract` 现已新增可选 `--check-session-action / MAGI_CHECK_SESSION_ACTION` 与 `npm run smoke:execution`：repo 内 smoke 现在可以在保持默认 gate 语义不变的前提下，再补一条最小 `POST /session/action` 执行脚手架
- `support/frontend-contract` 现已再补 `--cutover-url / MAGI_CUTOVER_SMOKE_URL`，独立 `/bridges/cutover-smoke` 资源已经可以脱离整套 daemon 资源做最小 gate smoke
- `magi-daemon` 现已补 env-backed provider / MCP 组合态与失败路径回归：除了 ready 路径，catalog 中已暴露但处于 `degraded` 的 `openai-compatible`、环境配置驱动的 provider 上游拒绝、`HTTP 200` 但 payload 不可桥接、MCP `fallback-only` 默认路由以及 `unavailable / no-route` 分支也都能稳定反映到 `/bridges/cutover-smoke`
- `/bridges/cutover-smoke` 现已把 model/provider 失败从通用 `bridge_invocation_failed` 进一步细分为 `model_provider_unavailable / misconfigured / transport_failed / rejected / invalid_response`，并通过 `BridgeProbeErrorDto.code` 把远端业务码一起导出；但这些仍然属于影子/loopback 级验证，还不等于真实 provider 已全面接线
- `web-client-bridge` 的 `ensureFreshLiveBridge(...)` 现已把 cutover gate 接进 `executeTask / startTask / resumeTask` 的真实执行 preflight，bridge runtime 层不再只依赖 `InputArea` 的局部拦截
- `web` settings 统计面现已开始直接消费顶层 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind`，TS 侧针对 cutover diagnostics 的聚合统计开始摆脱“前端自己重扫 issues”的旧模式
- `/session/action` 默认 shadow 路由现在已会在 dispatch 成功后自动调用 `magi-memory-store::apply_extraction(...)`，并且后续同 session dispatch 已能继续消费这条 route 级写回
- `magi-api` 已把 `/session/action` 的 shadow dispatch / extraction 顺序收口到 `shadow_execution.rs` 单一 helper，默认入口不再维护重复的内联副作用逻辑
- `magi-api` 已新增失败回归测试，锁定 dispatch 失败时不写回 extraction、也不绑定 mission / todo / worker ownership
- `magi-orchestrator` 已新增默认 execution runtime 的 `execute_dispatch_then(...)` success hook 入口，`magi-api` 现已复用这条 runtime 层接缝承接 route 级 extraction 写回
- `magi-orchestrator` 现已把 writeback 能力进一步下沉到 runtime 公共层：`ExecutionWritebackPlans`、`execute_dispatch_with_writebacks(...)` 与 `execute_recovery_with_writebacks(...)` 已可直接被非 API 私有调用方复用
- `magi-daemon` 默认 shadow 执行器现已从 `WorkerRuntime::new_compare()` 切到 `WorkerRuntime::new()` / local-process 主线，默认 API 路径更接近真实执行器
- `magi-orchestrator` 已把 `session.action` 的 extraction payload builder 提升到 runtime 公共层：`DispatchMemoryExtractionInput` 与 `ExecutionWritebackPlans::from_session_action_input(...)` 现在统一负责 skill/deep-task 扩展、timeline provenance 与空文本跳过语义
- `magi-api` 现已直接复用 runtime 公共层的 `ExecutionWritebackPlans` 与 `execute_dispatch_with_writebacks(...)` / `execute_recovery_with_writebacks(...)`；route 行为保持不变，但 writeback 能力不再停留在 API 私有模块
- `magi-daemon` 现已补一条 bootstrap 消费者验证：follow-up `session/action` 后，`/bootstrap.runtime_read_model` 与 `/runtime/read-model` 保持一致，bootstrap 不会丢失 mission context 摘要
- `magi-daemon` 现已再补一条 recovery bootstrap 消费者验证：`session/action -> recovery/resume -> session/action` 之后，`/bootstrap.runtime_read_model` 与 `/runtime/read-model` 仍保持一致，且 follow-up mission 能稳定看到 recovery extraction ref
- `magi-daemon` 现已补一条 bridge bootstrap 消费者验证：`/bootstrap.bridge_services` 与 `/bootstrap.bridge_preflight` 会稳定分别对齐 `/bridges/services` 与 `/bridges/preflight`
- `magi-orchestrator` 已把 recovery 主链补成与 dispatch 同形的 `execute_recovery_then(...)` success hook 接缝，并补了 success / failure 两条回归测试
- `magi-api` 现已把 recovery writeback 真正接到 pipeline：`diagnostic_summary` 进入 memory content，`recovery_id / snapshot_id` 收口到 `source_ref`
- `magi-api` 已新增 `/recovery/resume` 路由，并补 `RECOVERY_NOT_FOUND` / 成功写回两条回归；默认测试态 shadow pipeline 也已接上 recovery support
- `magi-api` 现已把 `/recovery/resume` 的 `Prepared / Consumed` 状态前置收口成稳定 `400 INPUT_INVALID`，并修正 `/runtime/read-model` 的 `active_recovery_ids` 不再把 `Consumed` 计入 active
- `magi-orchestrator` / `magi-api` 已把 recovery resume 的实际执行 worker 对齐成单一真相源：显式 `worker_id` 请求现在会稳定体现在 `decision.worker_id`、session sidecar 与 `/recovery/resume` 响应里
- `magi-api` 已修正 workspace recovery sidecar 与 runtime read model 的合并策略：如果事件聚合侧已有 recovery worker，则不再被 stale ownership worker 覆盖回去
- `magi-workspace` / `magi-orchestrator` 已把 recovery `Ready` 校验下沉到 workspace/runtime 真入口：`build_recovery_resume_input(...)` 与 direct `execute_recovery(...)` 现在都会在 sidecar 写入前拒绝 `Prepared` recovery
- `magi-workspace` / `magi-orchestrator` 现已把 consumed recovery 的 workspace sidecar ownership 对齐到实际恢复结果：`mission_id / todo_id / worker_id / execution_chain_ref` 不再只停留在事件流和 session sidecar
- `magi-api` 现已把 consumed workspace recovery sidecar 在 runtime read model 中收紧成“补洞而非降级”语义：event-sourced recovery outcome 不再被 sidecar 快照覆盖回 `consumed`
- `magi-daemon` 默认 router 现已补通 `session/action -> recovery/resume -> session/action` 三跳闭环，recovery 写回出的 extraction 能在后续 dispatch 中被 context 消费
- `magi-bridge-client` 已补 `JsonRpcMcpManagerClient` 与 typed MCP manager contract，并新增 shared-registry 的完整 lifecycle round-trip 回归；同时继续补强 `openai-compatible` 成功语义测试、structured tool arguments 宽容解析、多 `choices` 回退选择策略，以及 refusal-only / 空 content + refusal 成功包兼容
- `magi-bridge-client` 现已补 `mcp.update_health` 生命周期恢复护栏：health 变回 healthy 后会按 `enabled + health` 统一重算 lifecycle_state，避免状态卡在 `failed / stopped`
- `magi-bridge-client` 现已补 `mcp.update_health` no-op 幂等护栏：当 health 未变化时，响应不会把旧 `lifecycle_event` 误报成当前操作结果
- `magi-bridge-client` 现已补 `enable_server / disable_server` 的 no-op 幂等护栏：重复 enable/disable 不再把历史 `lifecycle_event` 误报成当前操作结果
- `magi-bridge-client` 现已把 `enabled + unavailable` server 收紧为 `non-routable`：空白 selection 不再默认回退到 unavailable server，显式 `mcp.call_tool` 也会稳定返回 `server unavailable`
- `magi-bridge-client` 现已把 MCP manager 的 no-route `default_server` 收紧为真实可空元数据：catalog、blank-selection error 与 default-route 能力字段不再把 `shadow-mcp-manager` 误报成默认 server
- `magi-event-bus` 已修复“无活跃订阅者时 publish 失败”的稳定性问题，默认 API 写路径不再依赖已有 SSE 订阅
- `magi-event-bus` 已补回归，锁定 `mission.execution.overview.context` 的 `knowledge_source_paths / memory_extraction_refs` 在后续不带 context 的 follow-up overview 下仍会稳定保留
- 本轮已通过的验证包括：
  - `cargo test -p magi-context-runtime`
  - `cargo test -p magi-orchestrator`
  - `cargo test -p magi-event-bus`
  - `cargo test -p magi-api`
  - `cargo test -p magi-bridge-client`
  - `cargo test -p magi-daemon`
  - `cargo test --workspace`
  - `cargo test --workspace -- --list | rg '(^test |: test$)' -c` = `420`（从 331 增长）
- `/api/task/execute` 已接入真实 shadow execution pipeline：`tasks_interaction.rs` 的 `execute_task` 不再返回 stub，而是检查 pipeline 存在 -> 构建 `SessionActionRequestDto` -> 解析/创建 session -> 记录 timeline -> 调用 `run_shadow_session_action` -> 发布 `task.execute.accepted` 事件 -> 返回真实 `taskId/sessionId/entryId/eventId/acceptedAt/status/createdSession`；这意味着 `/task/execute` 现在也通过统一的 `ExecutionWritebackPlans` 走 `execute_dispatch_for_session_action`
- `SettingsStore` 已添加 JSON 文件持久化：`crates/magi-api/src/settings_store.rs` 新增 `with_persistence_path`、`load_from_disk`、`save_to_disk` 和 `auto_persist`；写操作（`set`/`set_section`/`upsert_array_entry`/`remove_array_entry`/`remove_section_entry`）后自动原子写入磁盘；daemon 启动时通过 `SettingsStore::with_persistence_path(state_root.join("settings.json"))` 构造并调用 `load_from_disk()` 恢复；新增 4 个单元测试
- 前端 API 端点全覆盖：前端 `agent-api.ts` 中的 104 个 API 函数已全部映射到 Rust 后端路由；新增 `/api/bridges/cutover-smoke` fallback 路由（前端 "Rust 根路由优先、`/api` 兼容代理回退" 策略）
- 前端清理完毕：`svelte-check` 0 errors / 0 warnings；`vite build` 通过；删除了 3 个死文件，清理了大量未使用导入
- A-3 调用方级验证已全部完成：`magi-api` 新增 3 个 task/execute 专属回归（success writeback → runtime read-model 更新、failure isolation 不污染 ownership/memory、no-pipeline 稳定返回 500 INTERNAL_ASSEMBLY_ERROR）
- `tasks_interaction.rs` 剩余 10 个 stub 端点（append/start/resume/delete/clear-all/queued-update/queued-delete/confirm-recovery/chain-resume/chain-abandon）现已全部接入事件总线，通过 `EventEnvelope::domain` 发布各自领域事件，不再有纯假数据返回
- `support/frontend-contract` 现已新增 `TaskExecuteRequestDto / TaskExecuteResponseDto / TaskInterruptResponseDto / InteractionEventBusResponseDto` 四个 DTO，`RustDaemonClient` 补 `executeTask / interruptTask / submitInteractionResponse / submitInteractionClarification / submitWorkerQuestion` 五个 client 方法，并新增 `--check-task-execute` smoke 入口与 `npm run smoke:task-execute`
- `support/frontend-contract` TS 契约层全量冻结：新增约 80 个 DTO 接口与约 70 个 `RustDaemonClient` 方法，覆盖所有 Rust 后端路由（task lifecycle、interaction、chain、sessions、workspaces、settings、knowledge、MCP/skills/repos、changes/files/tunnel），C-1 终点定义已满足
- `magi-daemon` env-backed provider transport failure 回归已补：当端点不可达时稳定报 `model_provider_transport_failed`，B-3 终点定义已满足
- `support/frontend-contract` 新增 `--check-api-reads` smoke 覆盖 5 条前端常用读取路由（settings/bootstrap、workspaces、workspace-sessions、bridges-services、bridges-preflight），B-2 终点定义已满足
- Task 编排升级 Phase 1-2 完成：TaskStore、task_events（17 事件类型）、Task Graph API（6 端点）已落地，`session/action` 已挂接 Task Graph
- Phase 5 直连模式：`HttpModelBridgeClient` 已创建（10 tests），daemon runtime 已支持 env-backed 直连
- 本轮仍未完成的切换前事项：
  - extraction 自动回写虽然已下沉到 runtime 公共层，且 `/task/execute` 也已接入，但新的真实调用方是否全部切到该接缝仍需后续 daemon / bridge / TS 接线继续验证
  - 更广真实 bridge / provider / MCP / TS 接线
