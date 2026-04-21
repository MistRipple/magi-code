# Agent 任务单：magi-daemon 进程骨架

更新时间：2026-04-16

---

## 1. 任务名称

- 名称：`magi-daemon` 进程骨架任务单
- 编号：`WP-DAEMON-001`
- 负责 Agent：Daemon Agent

## 2. 写域

- 唯一写域：`crates/magi-daemon`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - API 路由实现细节
  - session / workspace / orchestrator 业务状态
- 依赖的上游文档：
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)
  - [05-milestones-and-cutover-gates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/05-milestones-and-cutover-gates.md)

## 3. 背景

- 当前能力域：本地常驻进程入口
- 当前实现位置：
  - `src/agent/main.ts`
  - `src/agent/config.ts`
  - `src/agent/runtime-state.ts`
- 当前问题：
  - 当前 daemon 入口极薄，但能力边界都沉到 `LocalAgentService`
  - 进程入口、配置加载、服务初始化顺序尚未形成稳定后端骨架

## 4. 根本原因

1. 旧实现以 Node 单进程服务为中心，daemon 与 API 服务没有明确分层
2. 没有把“常驻进程生命周期”作为独立能力建模
3. 如果不先立 daemon，后续 API 和 runtime 仍会混在一个启动对象里

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-daemon` crate
  - 定义配置加载、日志初始化、signal/shutdown、服务启动顺序
- 本任务不做什么：
  - 不实现 HTTP 路由
  - 不实现业务状态机
  - 不直接依赖宿主能力
- 与其他 Agent 的边界：
  - `magi-daemon` 只负责启动和托管
  - API / store / runtime 由其他 crate 提供

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-daemon`
  - `bootstrap`
  - `config`
  - `lifecycle`
  - `shutdown`
- 新增 schema：
  - 无
- 更新文档：
  - 如初始化顺序影响 crate 依赖，回写 `04-module-mapping-and-target-crates.md`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删除主仓运行代码
  - 但禁止在 Rust 侧把 API 路由偷偷塞进 daemon crate

## 7. 语义约束

- 本任务涉及的真相源：
  - daemon 生命周期
  - 配置加载顺序
- 是否涉及协议变化：
  - 否
- 是否涉及语义偏差台账登记：
  - 如发现 `LocalAgentService` 启动语义需重定义，回写 `D-002`

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

- `magi-daemon` 不得承担业务状态机
- 不得引入宿主 SDK

## 9. 验收标准

- 编译：
  - `magi-daemon` 可独立编译
- 最小运行验证：
  - daemon 可启动并优雅关闭
- 协议验证：
  - 无直接协议要求
- 清理验证：
  - crate 内无 API 路由与业务状态混装

## 10. 输出结论

- 已完成内容：
  - 已建立 `magi-daemon` crate 和 `apps/daemon` 入口
  - 已完成配置、监听地址、服务装配、状态目录接入
  - 已接入 `SessionStore`、`WorkspaceStore`、`GovernanceService`、`EventBus` 的启动装配
  - 已支持启动期状态加载与原子写回
- 已接入 audit / usage ledger 的启动恢复与运行期自动刷新装配，且保持当前恢复/刷新语义不变；usage 事件仍由 event-bus 统一落入同一账本，daemon 侧不新增第二套 ledger 恢复语义
- 已在 daemon 启动恢复后显式消费 ledger 状态，并发布统一 `system.ledger.ready` 系统事件；事件载荷固定暴露 `schema_version / audit_count / usage_count / next_sequence / persistence_path / last_persist_error / is_persist_healthy`
- 已补 runtime maintenance tick：daemon 现可按 session/workspace sidecar 的 flush metadata 自动决定是否刷 sidecar，并在 ledger `pending_flush=true` 时主动刷新账本后重发 `system.ledger.ready`；当前不引入第二套调度协议
- 已将 session/workspace 的恢复 sidecar 持久化入口拆成独立文件：`session-sidecars.json`、`workspace-recovery-sidecars.json`，并保持 aggregate 主状态仍落在 `sessions.json`、`workspaces.json`
- 已对旧单文件布局保持兼容读取：若独立 sidecar 文件尚不存在，daemon 会从旧的 `sessions.json` / `workspaces.json` 中提取 sidecar 子结构完成迁移式加载
- 已补 `ShadowRuntimeSidecarPersistence`，daemon 现有显式 sidecar 刷新入口，可只刷变脏的 `session-sidecars.json` / `workspace-recovery-sidecars.json`
- 已支持恢复消费后的细粒度 sidecar 写回：session/workspace sidecar 变更后可通过统一 flush hook 增量落盘，而不是重新全量直写 aggregate 主状态
- 已补更明确的运行策略结构：`ShadowRuntimeMaintenancePolicy / ShadowRuntimeMaintenanceConfig / ShadowRuntimeMaintenanceState / RuntimeMaintenanceReport`
- 已将 maintenance tick 的结果收口为可区分三态：
  - `skipped`
  - `due-and-flushed` / `due-and-refreshed`
  - `failed`
- 已让 runtime maintenance 报告显式承接 policy/config/state 信息，并继续只消费既有 sidecar flush metadata 与 ledger runtime signals，没有引入第二套持久化协议
- 已将 maintenance policy 继续前置为可区分的 profile：`ShadowDefault / AggressiveFlush / PreCutoverDrain`，并固定由 policy profile 推导 tick interval、dirty sidecar eager flush、ledger unhealthy/never-persisted refresh 与强制刷新策略
- 已补 maintenance mode 与 graceful shutdown 前置语义：
  - `Active`
  - `AggressiveFlush`
  - `CutoverPrep`
  - `ShutdownRequested`
  - `ShutdownComplete`
- 已补 `DaemonRuntimeStatus` 运行态导出，固定暴露：
  - `maintenance_mode`
  - `policy_profile`
  - `mode_reason`
  - `shutdown_requested_at`
  - `shutdown_completed_at`
  - `last_tick_at`
  - `last_sidecar_outcome`
  - `last_ledger_outcome`
  - `tick_interval_millis`
  - `sidecar_flush_enabled`
  - `ledger_refresh_enabled`
  - `eager_flush_dirty_sidecars`
  - `refresh_ledger_when_unhealthy`
  - `refresh_ledger_when_never_persisted`
- 已补统一 `system.runtime.maintenance.status` 系统事件，daemon 现在会在启动和每次 maintenance tick 后发布 runtime status，而不另起第二套导出协议
- 已支持 graceful shutdown 前的最终维护 tick：shutdown request 会强制执行一次 sidecar flush / ledger refresh 收口，然后把运行态推进到 `ShutdownComplete`
- 删除内容：
  - 无
- 未完成边界：
  - 尚未接入真实 signal/shutdown 触发源，当前 graceful shutdown 仍停留在 daemon 内部策略层
  - 尚未接入更多 runtime crate
  - runtime maintenance tick 仍属于 shadow 运行策略，不代表已经进入真实切换编排或外部调度器接管
- 后续依赖：
  - `magi-api`
