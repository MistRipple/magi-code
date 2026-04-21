# TS 链路接线准备清单

更新时间：2026-04-17

> 本文档用于在真正开始 TS -> Rust 接线前，先把替换点与依赖关系收口清楚。
>
> 它的目标不是今天就接线，而是避免接线时再反向定义 Rust 后端语义。

---

## 1. 当前结论

当前 TS 接线准备已经具备文档化入口，但还没有进入真实替换执行。

接线前提：

1. Runtime Read Model 冻结证据已成立
2. `knowledge / memory` 主链接入完成
3. 真实 provider / MCP 的最小可用边界明确

---

## 2. API 替换点

需要优先复核的 TS 入口：

- `src/agent/main.ts`
- `src/agent/service/local-agent-service.ts`
- `src/agent/service/agent-runtime-service.ts`

接线目标：

- `health / version / bootstrap / bridges/services / bridges/preflight / bridges/cutover-smoke / events` 先对齐 Rust 侧稳定出口
- 其余一级资源在真正切换前不要提前假定字段形状
- `support/frontend-contract` 现已覆盖 `runtime/read-model / ledger / bridges/services / bridges/preflight / bridges/cutover-smoke / recovery/resume`；TS 新接线应优先复用这层最小契约包，而不是再手写 DTO
- `support/frontend-contract` 现已新增 `npm run smoke`；在继续扩大 `web` 消费面前，可先用这条 repo-level smoke 验证 daemon 的 bridge gate 与 runtime query 是否可读
- `support/frontend-contract` 现已新增 `npm run smoke:strict`；若 TS / CI 需要固定“cutover 必须 ready”的严格入口，应优先复用这条脚本，而不是手拼 `--require-cutover-ready --json`
- repo-level smoke 现已对本地 daemon 实跑通过：`npm run smoke -- --base-url http://127.0.0.1:38123 --json`
- `npm run smoke:strict`、`npm run smoke:execution`、`npm run smoke:task-execute` 现已在真实本地 daemon + env-backed provider stub 条件下实跑通过；TS 接线前的最小严格 gate 与最小执行主线都已有 live 证据
- `apps/daemon` 入口现已支持 `MAGI_HOST / MAGI_PORT / MAGI_SERVICE_NAME / MAGI_STATE_ROOT`，若 TS 联调环境需要切端口或状态目录，不必再改 app 源码
- 若 TS / CI 只想对独立 bridge gate 做放行判断，可优先复用 `support/frontend-contract` 的 `--cutover-url / MAGI_CUTOVER_SMOKE_URL`，不要为 `/bridges/cutover-smoke` 再写一套单独脚本

---

## 3. Bootstrap / Runtime Query 替换点

需要优先复核的 TS 入口：

- `src/shared/session-bootstrap.ts`
- `src/orchestrator/runtime/orchestration-runtime-query-service.ts`
- `src/agent/service/agent-runtime-service.ts`

接线原则：

- 以 Rust 侧的 `meta / overview / details / operations / recovery` 为唯一 query contract
- 不允许 TS 再拼装第二套 bootstrap 真相源
- `details.sessions / details.workspaces` 的 sidecar export 继续只从 bootstrap 统一导出面进入
- bridge catalog / smoke 现在也已能从 `bootstrap.bridge_services / bootstrap.bridge_preflight` 统一导出；TS 不需要再为了 bridge readiness 额外拼一套 bootstrap 旁路
- `/bridges/cutover-smoke` 当前不在 `bootstrap` 内；需要 cutover gating 的 TS 调用方必须显式请求独立路由，不能假定存在 `bootstrap.bridge_cutover_smoke`
- TS 侧消费 `/bridges/cutover-smoke` 时应先读取顶层 `overall_ok` 与 `blocking_issues`，再按 `reason_code` 做诊断分流，不要从一开始就自己重扫全部 payload
- TS 侧若只需要快速做 gate 统计或告警聚合，应优先消费顶层 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind`，不要在前端重复统计 `blocking_issues`
- TS 侧进入 service 诊断时应优先消费 `service_ok / blocking_check_count / blocking_targets`，只有在需要更深排障时再继续读取逐项 checks
- TS 侧进入 `MCP` service 诊断时应优先消费 `mcp_default_route_gate.route_status / route_target / resolved_server / contract_ok`，不要默认直接下钻 `checks[0].mcp_contract`
- 当前 `web` 已把 `/bridges/cutover-smoke` 接进共享消息流：优先由 `web-client-bridge` 派发 `bridgeCutoverSmokeLoaded` 并落入全局 store，settings 只是消费方之一；后续 TS 新入口应继续复用这份共享快照，而不是每个面板各自直拉一次
- `web-client-bridge` 的 `ensureFreshLiveBridge(...)` 现已把 cutover gate 接进 `executeTask / startTask / resumeTask` 的真实执行 preflight；后续 TS 新入口若会直接触发执行，也应优先复用这条 bridge runtime 级 gate，而不是只依赖局部 UI 判断
- `web` settings 统计面现已开始直接消费顶层 `blocking_issue_counts_by_reason_code / blocking_issue_counts_by_server_kind`，后续 TS 新入口若只是做聚合统计，也应优先沿用这条路径，而不是重扫 `blocking_issues`
- `Header` 顶栏现已成为第二个真实 cutover gate 消费面；后续 TS 新入口应继续沿“共享快照 + 全局可见状态”扩展，而不是把 cutover 诊断退回 settings 私有路径
- `BottomTabs` 底栏现已成为第三个真实 cutover gate 消费面；后续 TS 新入口应继续沿“共享快照 + 多入口复用”扩展，而不是为局部 UI 再造私有拉取链
- `InputArea` 输入区现已成为第四个真实 cutover gate 消费面；后续 TS 若继续扩大消费面，应优先进入真实决策入口，而不是只增加只读展示位
- `InputArea` 输入区现在不只展示 gate，而是会在 `checking / blocked / error` 时实际阻止发送；后续 TS 若继续做 cutover 决策，应优先复用这条真实入口语义，而不是另外造一套发送前判断

---

## 4. SSE 替换点

需要优先复核的 TS 入口：

- `src/events.ts`
- `src/agent/service/agent-runtime-service.ts`

接线原则：

- Rust 侧统一 SSE envelope 与事件分类先对齐
- 不在接线阶段扩充事件主语义
- 事件回放、ledger readiness、maintenance status 必须沿同一事件面消费

---

## 5. Host Bridge 替换点

需要优先复核的 TS 入口：

- `src/host/runtime-host.ts`
- `src/extension.ts`
- `src/ui/webview-provider.ts`

接线原则：

- 只把 `VSCode real-prehost` 视为本轮有效宿主前置实现
- `IDEA host` 当前固定按 [29-idea-host-defer-decision.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/29-idea-host-defer-decision.md) 延后处理
- `bridge.handshake / bridge.health / bridge.describe_services` 是最先接线的协议入口
- Rust API 现已可通过 `/bridges/services` 导出这三类协议入口的快照，也可通过 `/bootstrap` 统一带出 `bridge_services`
- Rust API 现已可通过 `/bridges/preflight` 导出 `host / model / mcp` 最小 smoke 结果，也可通过 `/bootstrap` 统一带出 `bridge_preflight`
- Rust API 现已可通过 `/bridges/cutover-smoke` 导出切换前桥接 contract 视图，但这条资源当前保持独立，不经由 `/bootstrap` 下发
- `support/frontend-contract` 现已直接导出 bridge catalog / preflight / cutover-smoke DTO 与 client 方法；TS 侧不需要自己再从 Rust struct 逆推出前端类型
- `support/frontend-contract` 现已把 smoke CLI 推进成 env-driven gate：除 `--base-url` 外，还支持 `MAGI_SMOKE_JSON / MAGI_REQUIRE_CUTOVER_READY / MAGI_FAIL_ON_REASON_CODES` 与 `--fail-on-reason-codes`，TS cutover CI / smoke 应优先复用这条入口
- `support/frontend-contract` 现已新增 `npm run smoke:strict` 作为固定严格 smoke 入口；若 TS cutover CI 的目标只是“必须 ready 才放行”，应优先从这条脚本起步
- `support/frontend-contract` 现已新增可选 `--check-events / MAGI_CHECK_EVENTS`、`npm run smoke:events` 与 `npm run verify:events`；若 TS cutover smoke 需要顺手验证 `/events` 连通性，应优先复用这条 opt-in 能力，而不是另写一套 SSE 探针
- `support/frontend-contract` 现已新增可选 `--check-session-action / MAGI_CHECK_SESSION_ACTION` 与 `npm run smoke:execution`；若 TS cutover smoke 需要补一条最小真实执行请求，应优先复用这条 opt-in 入口，而不是另写一套 `POST /session/action` 脚手架
- `support/frontend-contract` 现已再补 `--cutover-url / MAGI_CUTOVER_SMOKE_URL`，TS 侧若只想验证 bridge cutover gate，不必强依赖完整 daemon 资源全部可读
- `web` settings 刷新链路现已开始消费 `/bridges/cutover-smoke`：实现策略是“优先直连 Rust daemon 根路由，若当前环境仍保留兼容 `/api` 代理则自动回退”；后续 TS 消费面应沿用这套读取顺序，而不是重新猜代理层
- `/bridges/cutover-smoke` 现已具备顶层 blocking summary，可直接作为 TS cutover preflight 的 gate snapshot 使用
- `/bridges/cutover-smoke` 现已具备 service-level blocking summary，可直接作为 TS bridge diagnostics 的第一层读取面
- `/bridges/cutover-smoke` 的 MCP service 现已具备稳定 default-route gate，可直接作为 TS MCP cutover diagnostics 的第二层读取面
- `/bridges/cutover-smoke` 现已具备顶层 `blocking_issues` 与稳定 `reason_code`，TS 侧应优先按原因码路由诊断，而不是只依赖 `blocking_services`
- model/provider 失败当前已进一步细分为 `model_provider_unavailable / model_provider_misconfigured / model_provider_transport_failed / model_provider_rejected / model_provider_invalid_response`，TS 侧若要做 provider 放行建议或错误归因，应优先消费这些原因码与 `error.code`，而不是继续把 provider 失败视为单一 `bridge_invocation_failed`
- `MCP blank-selection` 当前已进一步细分出 `mcp_blank_selection_invocation_failed` 与 `mcp_blank_selection_response_not_ok`，ready 路径也已稳定区分 `mcp_default_route_metadata_drift / mcp_default_route_resolved_server_mismatch`；TS 侧若要做默认路由恢复建议，应优先消费这些原因码，而不是把默认路由失败视为单一分支

---

## 6. Model / MCP 替换点

需要优先复核的 TS 入口：

- `src/llm/adapter-factory.ts`
- `src/llm/clients/universal-client.ts`
- `src/tools/mcp-manager.ts`
- `src/tools/mcp-executor.ts`

接线原则：

- 当前只能把 `openai-compatible` 视为 provider HTTP smoke path，不应误判为已完成真实 provider 接线；streaming 仍未收口，但 `tool calls / usage / finish_reason` 的结构化成功响应已在 Rust 侧 bridge 语义中成立，TS cutover preflight 应优先读取 `/bridges/cutover-smoke` 的 model contract，而不是自行重建 provider 判定
- MCP 当前可以先消费 registry / health / default route 语义；`mcp.list_servers / mcp.describe_server / mcp.enable_server / mcp.disable_server / mcp.register_server / mcp.start_server / mcp.stop_server / mcp.deregister_server / mcp.update_health` 也已可调用，但仍不应假设真实外部 server lifecycle 已完成；TS cutover preflight 应优先读取 `/bridges/cutover-smoke` 的 default-route contract，而不是手工拼 `/bridges/services + /bridges/preflight`

---

## 7. 真正开始接线前的最后检查

1. 重新运行 `cargo test --workspace`
2. 复核 [07-schema-and-contract-freeze.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/07-schema-and-contract-freeze.md)
3. 复核 [26-m6-precheck-checklist.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/26-m6-precheck-checklist.md)
4. 复核 [28-m6-cutover-evaluation-package.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/28-m6-cutover-evaluation-package.md)
5. 确认 `T-301 / T-302` 已关闭
