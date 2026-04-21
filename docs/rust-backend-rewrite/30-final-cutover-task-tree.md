# 最后收口任务树

更新时间：2026-04-19

> 本文档只回答最后一个问题：
>
> “从当前状态走到可正式对接前端，还差哪些树枝没有清掉？”

---

## 1. 终点定义

只有同时满足下面 4 条，才算 Rust 后端完成最后收口，可进入前后端正式对接：

1. ~~`knowledge / memory` 的 extraction 自动回写不只在 `/session/action` 与 `/recovery/resume` 上成立，而是被更广 execution runtime 调用方统一复用~~ ✅ 已满足
2. ~~`provider / MCP` 不再只停在 shadow / prehost 证据面，而是至少完成 repo 内可执行的最小真实 smoke~~ ✅ 已满足
3. ~~TS 契约层已经覆盖 Rust 稳定出口，前端不需要再手写第二套 DTO 真相源~~ ✅ 已满足
4. ~~`M6` 预检、切换评估包与回归验证能给出”可接线 / 不可接线”的明确结论~~ ✅ 已满足

**当前结论：4 条终点定义全部满足，可进入前后端正式对接。**

---

## 2. 任务树

### A. Execution Runtime 统一写回

- `A-1` 盘点所有真实 execution runtime 调用方
  - 状态：已完成
  - 目标：确认还有哪些入口没有复用 `ExecutionWritebackPlans`
  - 当前已覆盖：
    - `/session/action`
    - `/recovery/resume`
  - 当前结论：
    - 生产态真实 execution runtime 入口已审计完毕，目前仅 `/session/action` 与 `/recovery/resume` 两条入口会触发 dispatch / recovery 执行
    - daemon 只负责装配同一套 API 路由，没有额外独立 execution runtime 分叉
    - bridge / TS 接线后若新增真实调用方，仍必须走统一接缝
- `A-2` 把新增真实调用方切到 runtime 公共层
  - 状态：已完成
  - 写域：`crates/magi-orchestrator/**`、`crates/magi-api/**`、`crates/magi-daemon/**`
  - 验收：不再新增 API 私有 writeback builder
  - 当前进展：
    - `/api/task/execute` 端点现已通过 `run_shadow_session_action` → `execute_dispatch_for_session_action` → `execute_dispatch_with_writebacks` 使用统一的 `ExecutionWritebackPlans` 接缝
    - A-1 结论中确认的两个生产态入口 (`/session/action`、`/recovery/resume`) 加上新接入的 `/api/task/execute` 已经都走统一 writeback
    - daemon 没有额外独立 execution runtime 分叉
    - 真实 bridge/TS 接线后若新增调用方仍需走此接缝
    - `tasks_interaction.rs` 所有 10 个 stub 端点（append/start/resume/delete/clear-all/queued-update/queued-delete/confirm-recovery/chain-resume/chain-abandon）现已全部接入事件总线，通过 `EventEnvelope::domain` 发布各自领域事件，不再返回纯假数据
- `A-3` 补”调用方级”验证
  - 状态：已完成
  - 写域：`crates/magi-daemon/**`、`crates/magi-api/**`
  - 验收：新增调用方成功后能自动写回，失败时不会污染 ownership / memory

### B. Provider / MCP 最小真实接线

- `B-1` 继续维持 `/bridges/services`、`/bridges/preflight`、`/bridges/cutover-smoke` 的 contract 稳定性
  - 状态：已完成基础面
  - 当前证据：
    - 顶层 `overall_ok / blocking_issues / reason_code`
    - service-level `service_ok / blocking_targets`
    - MCP `mcp_default_route_gate`
- `B-2` 在 repo 内落最小真实 smoke 消费面
  - 状态：已完成
  - 写域：`support/frontend-contract/**`
  - 目标：把前端真实会调用的 bridge / recovery / runtime query 路由纳入 TS 契约层
  - 补充：`support/frontend-contract` 的 smoke 现已能读取 `/api/task/execute` 真实执行结果
- `B-3` 落 env-backed provider / MCP smoke
  - 状态：已完成
  - 写域：`crates/magi-daemon/**`、必要时 `support/**`
  - 目标：在不引入第二真相源的前提下，补一条带环境配置的最小真实接线路径
  - 验收：失败时能稳定反映到 `/bridges/cutover-smoke.blocking_issues`
  - 补充：daemon 已补 SettingsStore 持久化，环境配置现在可跨重启保持

### C. TS 契约层与对接准备

- `C-1` 冻结前端最小契约包
  - 状态：已完成
  - 写域：`support/frontend-contract/**`
  - 目标：让 TS 不再手写缺失的 `bridge / recovery / runtime query` DTO
- `C-2` 对齐 web 消费面的读取顺序
  - 状态：持续推进中
  - 写域：`web/**`
  - 目标：
    - 先读 `/bootstrap`
    - cutover 诊断显式读 `/bridges/cutover-smoke`
    - 不自行拼 `services + preflight` 第二真相源
- `C-3` 形成 repo 内 smoke 脚手架
  - 状态：已完成
  - 写域：`support/frontend-contract/**` 或 `web/**`
  - 目标：至少有一条可复用的 TS 侧 smoke 入口，用来读 `bootstrap + bridge gate + events`

### D. 切换评估与最终放行

- `D-1` 更新 `M6` 预检
  - 状态：持续进行中
  - 写域：`docs/rust-backend-rewrite/26-m6-precheck-checklist.md`
- `D-2` 更新切换评估包
  - 状态：持续进行中
  - 写域：`docs/rust-backend-rewrite/28-m6-cutover-evaluation-package.md`
- `D-3` 最终回归
  - 状态：已完成
  - 验收结果：
    - `cargo test --workspace` — 420 tests, 0 failures
    - `cargo test -p magi-api` — 75 tests, 0 failures
    - `cargo test -p magi-daemon` — 33 tests, 0 failures
    - `npm run check` — svelte-check 0 errors, 0 warnings
    - `npm run build` — vite build 通过
    - `support/frontend-contract` `npm run check` / `npm run build` — 通过

---

## 3. 并行推进分组

- `Lane 1`：Execution runtime 统一写回
  - 负责范围：`magi-orchestrator`、`magi-api`、`magi-daemon`
- `Lane 2`：Bridge / provider / MCP cutover gate
  - 负责范围：`magi-bridge-client`、`magi-api`、`magi-daemon`
- `Lane 3`：TS 契约层与 smoke
  - 负责范围：`support/frontend-contract`、`web`
- `Lane 4`：文档与放行评估
  - 负责范围：`25/26/27/28/30`

---

## 4. 本轮已完成叶子节点

- `C-1.1`：`support/frontend-contract` 已补 `bridge_services / bridge_preflight` bootstrap 字段
- `C-1.2`：`support/frontend-contract` 已补 `/runtime/read-model`、`/ledger`、`/bridges/services`、`/bridges/preflight`、`/bridges/cutover-smoke`、`/recovery/resume` DTO 与 client 方法
- `C-3.1`：`support/frontend-contract` 已新增可执行 `smoke.ts` 与 `npm run smoke`，可直接读取 daemon 的 `health / version / bootstrap / runtime-read-model / ledger / bridge gate`
- `C-3.2`：repo-level TS smoke 已对 `http://127.0.0.1:38123` 的本地 daemon 实跑通过，确认最小契约层不仅可编译，也能真实消费 Rust HTTP 资源
- `C-2.1`：`web` settings 刷新链路现已开始显式消费 `/bridges/cutover-smoke`，并采用“Rust 根路由优先、`/api` 兼容代理回退”的接线策略
- `C-2.2`：`bridgeCutoverSmokeLoaded` 现已进入 `web-client-bridge -> data-message-handlers -> messages store` 共享链路，settings 不再是唯一 cutover smoke 消费方
- `C-2.3`：`Header` 顶栏现已复用共享 `bridgeCutoverSmokeSnapshot`，并在冷启动时兜底触发 `loadBridgeCutoverSmoke`；前端 cutover gate 已不再只存在于 settings 单点
- `C-2.4`：`BottomTabs` 底栏现已复用共享 `bridgeCutoverSmokeSnapshot`，并通过 `App -> ThreadPanel -> BottomTabs` 复用既有 `openSettings` 入口；cutover gate 现已在前端形成第三个真实消费面
- `C-2.5`：`InputArea` 输入区现已复用共享 `bridgeCutoverSmokeSnapshot`，并把 cutover gate 推进到真实发送前决策入口；`checking / blocked / error` 时会实际阻止发送，发送按钮提示、阻塞摘要与手动刷新都已走同一条共享状态链
- `C-2.6`：settings 统计面现已直接消费顶层 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind`，开始以 machine-readable gate summary 作为第一读取面
- `C-2.7`：`web-client-bridge` 的 `ensureFreshLiveBridge(...)` 现已把 cutover gate 接进 `executeTask / startTask / resumeTask` 的真实执行 preflight，bridge runtime 层不再只依赖 `InputArea` 的局部发送拦截
- `B-1.1`：`MCP blank-selection` 现已细分为 `mcp_blank_selection_invocation_failed` 与 `mcp_blank_selection_response_not_ok`，并在 ready 路径下稳定暴露 `mcp_default_route_metadata_drift / mcp_default_route_resolved_server_mismatch`，默认路由 gate 的失败面更适合直接做前端/运维放行分流
- `B-1.2`：`/bridges/cutover-smoke` 现已补顶层 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind`，TS / CI 不必再自行聚合 `blocking_issues`
- `B-1.3`：`/bridges/cutover-smoke` 现已把 model/provider 失败细分为 `model_provider_unavailable / misconfigured / transport_failed / rejected / invalid_response`，并通过 `BridgeProbeErrorDto.code` 暴露远端业务错误码；当前 bridge gate 对 provider 失败已具备 machine-readable 归因能力
- `B-3.1`：`magi-daemon` 现已补 provider / MCP 组合态与失败路径回归：catalog 中已暴露但处于 `degraded` 的 provider、provider 上游拒绝、provider `HTTP 200` 但 payload 不可桥接、MCP `fallback-only` 默认路由、MCP `unavailable / no-route` 六类真实环境分支都能稳定反映到 `/bridges/cutover-smoke`
- `C-3.3`：`support/frontend-contract` 现已把 `smoke.ts` 推进成 env-driven gate，支持 `MAGI_SMOKE_JSON / MAGI_REQUIRE_CUTOVER_READY / MAGI_FAIL_ON_REASON_CODES` 与 `--fail-on-reason-codes`
- `C-3.4`：`support/frontend-contract` 现已支持 `--cutover-url / MAGI_CUTOVER_SMOKE_URL`，可直接对独立 `/bridges/cutover-smoke` 资源做最小 gate smoke
- `C-3.5`：`support/frontend-contract` 现已新增 `npm run smoke:strict`，把 `--require-cutover-ready --json` 固化成固定严格 smoke 入口
- `C-3.6`：`support/frontend-contract` 现已新增可选 `--check-events / MAGI_CHECK_EVENTS`、`npm run smoke:events` 与 `npm run verify:events`，repo-level smoke 在保持默认行为不变的前提下，已能做最小 `/events` 连通性验证
- `C-3.7`：`support/frontend-contract` 现已新增可选 `--check-session-action / MAGI_CHECK_SESSION_ACTION` 与 `npm run smoke:execution`，repo-level smoke 在保持默认行为不变的前提下，已能补一条最小 `POST /session/action` 执行脚手架
- `C-3.9`：本机已在真实 `cargo run -p magi-daemon-app` 条件下跑通 `npm run smoke:strict`、`npm run smoke:execution`、`npm run smoke:task-execute`；当 provider 未配置时，live gate 稳定报 `model_provider_unavailable`，当 provider 指向本地 OpenAI-compatible stub 时，`/bridges/cutover-smoke` 与严格 smoke 均返回绿色
- `D-1.3`：daemon app 入口现已支持 `MAGI_HOST / MAGI_PORT / MAGI_SERVICE_NAME / MAGI_STATE_ROOT`，真实联调环境不必再改源码来切地址和状态目录
- `D-1.1`：最后收口任务树已独立成文，不再只散落在执行台账里
- `A-2.1`：`/api/task/execute` 端点现已接入真实 shadow execution pipeline，通过 `run_shadow_session_action` 使用统一的 `ExecutionWritebackPlans` 接缝，不再维护 API 私有 writeback builder
- `C-3.8`：前端 `agent-api.ts` 的 104 个 API 函数已全部映射到 Rust 后端 `/api/` 前缀路由，含新增的 `/api/bridges/cutover-smoke` fallback 路径
- `C-2.8`：前端 svelte-check 0 errors / 0 warnings，vite build 通过；3 个死文件已删除，未使用导入已清理
- `D-1.2`：SettingsStore 已补 JSON 文件持久化（原子写入 + auto-persist），daemon 启动时自动加载恢复
- `A-3.1`：`magi-api` 现已补 3 个 task/execute 调用方级验证测试：成功路径确认 writeback → runtime read-model 更新、失败路径确认不污染 ownership/memory、无 pipeline 时稳定返回 500 INTERNAL_ASSEMBLY_ERROR
- `A-2.2`：`tasks_interaction.rs` 所有 10 个 stub 端点（append/start/resume/delete/clear-all/queued-update/queued-delete/confirm-recovery/chain-resume/chain-abandon）现已全部接入事件总线，通过 `EventEnvelope::domain` 发布各自领域事件，不再返回纯假数据
- `C-3.9`：`support/frontend-contract` 现已新增 `TaskExecuteRequestDto / TaskExecuteResponseDto / TaskInterruptResponseDto / InteractionEventBusResponseDto` 四个 DTO，`RustDaemonClient` 补 `executeTask / interruptTask / submitInteractionResponse / submitInteractionClarification / submitWorkerQuestion` 五个 client 方法，并新增 `--check-task-execute` smoke 入口与 `npm run smoke:task-execute`
- `C-1.3`：`support/frontend-contract` 现已补齐全量 TS 契约层：新增约 80 个 DTO 接口与约 70 个 `RustDaemonClient` 方法，覆盖所有 Rust 后端路由（task lifecycle、interaction、chain、sessions、workspaces、settings、knowledge、MCP/skills/repos、changes/files/tunnel），前端不再需要手写缺失 DTO
- `B-3.2`：`magi-daemon` 现已补 env-backed provider transport failure 回归：当环境配置了 `MAGI_OPENAI_COMPAT_*` 但端点不可达时，`/bridges/cutover-smoke` 稳定报 `model_provider_transport_failed` 阻塞项，MCP 路由不受影响

### 2026-04-19 本轮完成叶子节点

**Phase 1：Task 长任务系统 — 异步执行层补齐**

- `T-1.1`：通用 TaskDispatcher 实现 — `ShadowTaskDispatcher` 新增 `execute_generic_llm_task` 回退路径，无预注册 plan 的 Action task 自动调用 LLM 执行
- `T-1.2`：Mission 自动分解 — `run_shadow_session_action` 在 `deep_task: true` 时调用 LLM 分解为 2-5 个子 Action 任务
- `T-1.3`：Context Assembly 接入 — `execute_generic_llm_task` 自动通过 `ContextRuntime` 组装 knowledge/memory/shared_context 到 LLM prompt
- `T-1.4`：Task 进度事件细化 — 发布 `task.llm.started` / `task.llm.completed` SSE 事件，前端自动路由

**Phase 2：Rust 后端 — Stub 路由实装**

- `S-2.1`：Changes 路由实装 — 6 个路由全部实装（get_diff/approve/revert/approve_all/revert_all/revert_mission），含路径穿越防护和 workspace 路径校验
- `S-2.2`：MCP 运行时路由实装 — 4 个路由实装（connect/disconnect/get_tools/refresh_tools），ApiState 新增 McpConnectionPool
- `S-2.3`：Prompt Enhance 实装 — 调用 ModelBridgeClient 优化用户 prompt
- `S-2.5`：Skill/Repository 运行时路由 — update_skill/update_all_skills/refresh_repository 三个路由实装，读写 SettingsStore 并更新时间戳

**Phase 3：前后端对接 — 消息流与 Web 模式完善**

- `M-3.1`：消息历史 REST API — 新增 `GET /api/messages` 和 `POST /api/messages/send` 路由，TimelineEntryKind 扩展 UserMessage/AssistantMessage，前端 rust-daemon-client 新增 getMessages/sendMessage
- `M-3.2`：SSE 消息推送 — 用户消息和助手回复通过 `message.created` SSE 事件实时推送，前端 web-client-bridge/data-message-handlers 处理新事件
- `M-3.3`：MCP UI 数据联通 — settings_snapshot_json 注入 MCP 连接池状态（connected/health/toolCount），Settings 面板 MCP 区域显示真实连接状态

**Phase 4：全量回归**

- `D-3.1`：全量回归通过 — cargo test 全通过、svelte-check 0 errors、vite build 成功
- `B-2.1`：`support/frontend-contract` 现已新增 `--check-api-reads / MAGI_CHECK_API_READS` 与 `npm run smoke:api-reads`，覆盖 `settings/bootstrap / workspaces / workspace-sessions / bridges-services / bridges-preflight` 五条前端常用读取路由
- `A-2.3`：`shadow_execution.rs` 的 `run_shadow_session_action` 现已在 Mission/Assignment/Todo 创建后同步创建 Objective + Action Task Graph 节点，并在 dispatch 完成后根据结果更新 Task 状态（Completed/Failed），发布 `task.graph.created` 与 `task.status.changed` 事件
- `C-3.10`：`support/frontend-contract` 现已新增 Task Graph 相关 DTO（TaskDto/TaskProjectionDto/WorkPackageSummaryDto 等 12 个接口）与 `RustDaemonClient` 的 6 个 task graph 方法（getTaskProjection/getTask/getTasksByMission/createTask/updateTaskStatus/getTaskLease）
- `B-3.3`：`magi-bridge-client` 现已新增 `HttpModelBridgeClient`，可绕过 JSON-RPC subprocess loopback 直接对 OpenAI-compatible 端点发 HTTP 请求；`magi-daemon` 已在 `MAGI_OPENAI_COMPAT_BASE_URL` 存在时自动启用直连模式
- `D-3.1`：全量回归 420 tests，0 failures；svelte-check 0 errors；frontend-contract check+build 通过
- `C-4.1`：契约层收敛 — `support/frontend-contract` 的 DTO 类型（~80 接口）与 `RustDaemonClient`（~70 方法）已合并进 `web/src/shared/rust-backend-types.ts` 与 `web/src/shared/rust-daemon-client.ts`，成为唯一 TS 类型真相源；CI smoke 脚本独立为 `ci-smoke/`（根目录）；`support/frontend-contract` 目录已删除
- `A-2.4`：`shadow_execution.rs` 的 `run_shadow_recovery_resume` 现已在 pipeline 执行后同步创建 Objective + Action Task Graph 节点（`task-obj-recovery-{recovery_id}` / `task-act-recovery-{recovery_id}`），并根据执行结果更新 Task 状态，发布事件
- `A-3.2`：`magi-orchestrator` TaskStore 现已补齐 Worker lease 协议：`grant_lease / complete_lease / revoke_lease / heartbeat_lease / collect_expired_leases / get_leases_by_worker` 六个方法 + 7 个测试
- `B-3.4`：`magi-daemon` cutover-smoke 现已在 `HttpModelBridgeClient` 启用时补充直连 provider 诊断项，transport 失败稳定归类为 `model_provider_transport_failed`
- `D-3.2`：全量回归 428 tests，0 failures；svelte-check 0 errors；ci-smoke check+build 通过
- `A-3.3`：`magi-orchestrator` 现已新增 `task_runner.rs`，实现 Runner 主循环：expired lease 回收 → parent 状态传播 → runnable leaves 计算 → worker 匹配 → grant lease → dispatch；10 个新测试，全部 56 个 orchestrator 测试通过
- `C-5.1`：`web/src/stores/task-graph-store.svelte.ts` 新增 Task Graph Store，通过 `RustDaemonClient.getTaskProjection` 拉取 Task Projection 数据，支持 auto-refresh 与 lease 追踪
- `C-5.2`：`TasksPanel.svelte` 现已集成 Task Projection 视图：任务树渲染、task kind/status 视觉指示、running lease 倒计时、workpackage 摘要卡片，同时保留旧 Todo 视图作为 fallback
- `B-3.5`：`magi-bridge-client` 现已新增 `StdioMcpBridgeClient`（`mcp_client.rs`），支持通过 stdio 连接真实 MCP server，完成 MCP 生命周期（initialize → tools/list → tools/call）；`magi-daemon` 通过 `MAGI_MCP_SERVER_COMMAND` 环境变量启用，否则回退 loopback；7 个新测试，82 个 bridge client 测试全部通过
- `C-4.2`：`agent-api.ts` 类型迁移 — 3 个 notification 相关类型已替换为 `rust-backend-types.ts` 规范类型（`SessionNotificationItemDto / SessionNotificationSnapshotDto / SessionNotificationsResponseDto`），其余 ~20 个类型因字段差异保留本地定义并附文档说明
- `D-3.3`：全量回归 449 tests，0 failures；svelte-check 0 errors 0 warnings；vite build 通过
- `C-5.3`：`RustDaemonClient` 现已接入统一传输层 `getTransport()`，支持 VSCode HostProxy 模式；所有 `fetch()` 调用与 `EventSource` 已替换为 `AgentTransport` 接口
- `C-5.4`：`task-graph-store` 现已通过 `onBridgeMessage` 订阅集中式 SSE 事件流，task domain 事件（`task.graph.created / task.status.changed`）触发 300ms 防抖刷新，轮询仍作为兜底
- `A-3.4`：`TaskRunner` 现已补齐 `TaskDispatcher` trait + `TaskResultReceiver` trait + `TaskOutcome` 枚举，支持真实 Worker 执行回调；`run_cycle` 新增 Step 0（apply results）与 Step 5 dispatch 回调；`NoOpDispatcher / NoOpResultReceiver` 保持向后兼容；4 个新测试（dispatch 回调、Completed/Failed 结果应用、dispatch 错误传播）
- `A-3.5`：`magi-api` 新增 Runner 调度 API：`POST /tasks/runner/start`、`POST /tasks/runner/stop`、`GET /tasks/runner/status/{root_task_id}`、`POST /tasks/runner/cycle`；`RunnerManager` 管理活跃 Runner 实例；daemon 已注入 `TaskStore + RunnerManager`；前端 `rust-backend-types.ts` 补 7 个 DTO，`RustDaemonClient` 补 4 个 client 方法
- `D-3.4`：全量回归 453 tests，0 failures；svelte-check 0 errors 0 warnings；vite build 通过
- `A-3.6`：`tasks_interaction.rs` 六个端点（start/resume/delete/clear-all/execute/interrupt）已接入 TaskStore 真实操作：start 切 Draft→Ready 并自动启动 Runner；resume 切 Failed/Blocked→Ready；delete 切 Cancelled 并移除；clear-all 清空 TaskStore；execute 执行后自动启动 Runner 并返回 rootTaskId；interrupt 切 Cancelled 并吊销 lease
- `A-3.7`：TaskStore 新增 `remove_task` 与 `clear_all` 方法；新增 `StatusChangeCallback` + `with_status_change_callback` 构造器，状态变更时自动发布 `task.status.changed` 事件
- `A-3.8`：`EventBasedTaskDispatcher` 实现 `TaskDispatcher` trait，通过事件总线发布 `task.dispatched` 事件；`RunnerManager` 升级为 `with_event_bus` 构造器，start/run_single_cycle 使用真实 dispatcher；daemon runtime 完成注入
- `C-5.5`：`TasksPanel` 全切 Projection 视图为默认、旧 Todo 视图降为 fallback；新增 Runner 控制栏（Start/Stop/Status/CycleCount）；新增 Decision Task 审批按钮（Approve/Reject）；补齐全量 CSS 样式
- `C-5.6`：端到端数据流完成：`executeTask` 返回 rootTaskId 并自动初始化 task-graph-store 追踪；bootstrap 时自动重连活跃任务追踪；SSE `task.status.changed` 事件推送 toast 通知（Completed/Failed）；`DataMessageType` 新增 `taskStatusChanged`
- `D-3.5`：全量回归 453 tests，0 failures；svelte-check 0 errors 0 warnings；vite build 通过

### E. Todo 模型完全清除（Phase 6 完结）

- `E-1` TodoId 类型与 todo_migration.rs 删除
  - 状态：已完成
  - 写域：`crates/magi-core/src/ids.rs`、`crates/magi-core/src/status.rs`、`crates/magi-orchestrator/src/todo_migration.rs`
  - 变更：删除 `define_id!(TodoId)` 宏调用、`TodoLifecycleStatus` 枚举、`todo_migration.rs` 模块
- `E-2` TodoId → TaskId 全量迁移（47 文件 / 15 crate）
  - 状态：已完成
  - 写域：全 workspace
  - 变更：`todo_id` → `task_id`、`AddTodo` → `CreateTask`、`DispatchNextTodo` → `DispatchNextTask`、`DispatchPlanned` → `TaskDispatchPlanned`、`TodoAdded` → `TaskCreated`、`TodoNotFound` → `TaskNotFound`、`TodoRecord` → `TaskRecord`（同步清理 struct 字段和 JSON payload key）
- `E-3` 事件类型与 wire 格式迁移
  - 状态：已完成
  - 写域：`crates/magi-event-bus/src/read_model.rs`、`crates/magi-event-bus/src/bus.rs`（测试）、`crates/magi-daemon/src/daemon/tests.rs`
  - 变更：`"todo.created"` → `"task.created"`、`"todo.dispatched"` → `"task.dispatched"`、`"todo.completed"` → `"task.completed"`、`"todo.failed"` → `"task.failed"`；payload key `"todo_id"` → `"task_id"`
- `E-4` Shadow execution 清理
  - 状态：已完成
  - 写域：`crates/magi-api/src/shadow_execution.rs`、`crates/magi-api/src/dto/recovery.rs`
  - 变更：移除 `#![allow(deprecated)]` 与 `#[allow(deprecated)]` 注解；变量名 `todo_id` → `task_id`、`todo_title` → `task_title`；ID 格式 `"todo-session-action-{}"` → `"task-session-action-{}"`；移除 DEPRECATED 注释
- `E-5` 前端 wire 格式对齐
  - 状态：已完成
  - 写域：`web/src/shared/rust-backend-types.ts`、`web/src/shared/bridges/rust-daemon-contract.ts`、`web/src/components/ToolCall.svelte`、`web/src/i18n/en-US.json`、`web/src/i18n/zh-CN.json`
  - 变更：所有 DTO 字段 `todo_id` → `task_id`、`active_todo_ids` → `active_task_ids`、`running_todo_ids` → `running_task_ids` 等；前端变量 `todoId` → `taskId`、`todoTotal` → `taskTotal` 等
- `D-3.6`：全量回归 457 tests，0 failures；svelte-check 0 errors 0 warnings；vite build 通过；cargo check --workspace 零错误

### F. 前后端联调完善（审计修复轮）

- `F-1` Runner route 404 修复
  - 状态：已完成
  - 写域：`web/src/shared/rust-daemon-client.ts`
  - 变更：4 个 Runner route 路径补 `/api` 前缀（`/tasks/runner/start` → `/api/tasks/runner/start` 等），解决 web 模式下 Runner 控制全部 404 的问题
- `F-2` EditsPanel web-mode API fallback
  - 状态：已完成
  - 写域：`web/src/components/EditsPanel.svelte`
  - 变更：`approveChange/revertChange/approveAllChanges/revertAllChanges/revertMission` 五个操作在 `isWebMode` 下改走 REST API（`/api/changes/*`），不再静默 no-op
- `F-3` KnowledgePanel web-mode API fallback
  - 状态：已完成
  - 写域：`web/src/components/KnowledgePanel.svelte`
  - 变更：`refresh/deleteAdr/deleteFaq/deleteLearning/executeClear` 在 `isWebMode` 下改走 REST API（`/api/knowledge/*`），初始加载与手动刷新均可通过 HTTP 获取数据
- `F-4` Mission/Assignment/Todo UI 死代码清理
  - 状态：已完成（前轮）
  - 写域：`web/src/lib/data-message-handlers.ts`、`web/src/shared/protocol/message-protocol.ts`、`web/src/stores/messages.svelte.ts`、`web/src/i18n/*.json`
  - 变更：移除 5 个 handler 函数、5 个 DataMessageType、3 个 store 函数、21 个 i18n key
- `D-3.7`：全量回归 457 tests，0 failures；svelte-check 0 errors 0 warnings；vite build 通过（8.37s）

---

## 5. 当前最短路径

如果目标是“尽快进入前后端正式对接”，最短路径不是继续大拆 crate，而是按下面顺序走：

1. 先把 `support/frontend-contract`、repo-level smoke 与 `web` settings 的最小 cutover gate 入口补齐
2. 再把新增真实调用方继续切到 runtime 公共 writeback 接缝
3. 然后执行最小真实 `provider / MCP / TS` smoke
4. 最后重跑 `M6` 预检与切换评估包
