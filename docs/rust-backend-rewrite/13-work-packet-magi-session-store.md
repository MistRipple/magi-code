# Agent 任务单：magi-session-store 会话真相源

更新时间：2026-04-16

---

## 1. 任务名称

- 名称：`magi-session-store` 会话真相源任务单
- 编号：`WP-SESSION-001`
- 负责 Agent：Session Agent

## 2. 写域

- 唯一写域：`crates/magi-session-store`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - orchestrator 业务逻辑
  - workspace / snapshot 资源管理
  - UI 投影视图实现
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：session / timeline / notifications / projection 基础输入
- 当前实现位置：
  - `src/session/unified-session-manager.ts`
  - `src/session/timeline-record*.ts`
  - `src/session/session-timeline-projection.ts`
- 当前问题：
  - `UnifiedSession` 同时混有 durable state、projection、runtime sidecar
  - `executionChains?: unknown`、`resumeSnapshots?: unknown` 透传结构过弱
  - timeline / notifications / projection 层次边界不清晰

## 4. 根本原因

1. session 聚合在历史演进中承担了越来越多非 session 本体的状态
2. projection 被直接塞入 durable aggregate
3. 恢复链与执行链 sidecar 没有独立建模

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-session-store`
  - 拆分 session aggregate、timeline store、notification store、projection input
  - 为 execution/recovery sidecar 留出明确 sidecar store / 独立子结构
- 本任务不做什么：
  - 不实现 orchestrator 状态机
  - 不实现 workspace snapshot
  - 不实现 UI projection render
- 与其他 Agent 的边界：
  - session 只负责 durable 会话真相
  - projection read model 可由其他模块消费，但不反向污染 session

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-session-store`
  - `aggregate`
  - `timeline`
  - `notifications`
  - `index`
  - `hydration`
- 新增 schema：
  - 当前阶段无必须新增对外 schema
- 更新文档：
  - 若 session 语义收敛，回写 `D-004`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删除主仓运行代码
  - 但禁止在 Rust session aggregate 中继续混入 projection 与 `unknown` sidecar

## 7. 语义约束

- 本任务涉及的真相源：
  - Session aggregate
  - Timeline durable state
  - Notifications durable state
- 是否涉及协议变化：
  - 部分影响 bootstrap / projection，当前先不直接改对外协议
- 是否涉及语义偏差台账登记：
  - 是，必须对齐 `D-004`

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

- projection 不是 session aggregate 的持久化真相源
- 禁止继续使用 `unknown` 透传关键运行态 sidecar

## 9. 验收标准

- 编译：
  - `magi-session-store` 可独立编译
- 最小运行验证：
  - session create / switch / rename / delete 语义完整
  - timeline / notifications 可持久化与恢复
- 协议验证：
  - bootstrap 所需输入结构可被清晰导出
- 清理验证：
  - session aggregate 不再混入 UI 投影视图与 runtime sidecar 透传

## 10. 输出结论

- 已完成内容：
  - 已建立 `SessionStoreState`、`SessionProjectionInput`
  - 已拆出 session aggregate、timeline、notification 三层 durable state
  - 已支持 create / rename / archive / switch / delete / append timeline / append notification
  - 已提供 `export_state` / `from_state` 供 daemon 层持久化
  - 已建立 execution ownership、`recovery_id`、`status`、`updated_at` 的强类型 sidecar
  - 已建立 execution sidecar store 独立子结构，并提供 `execution_sidecar_store_state`、`execution_ownership`、`recovery_ref`、`recovery_id`、`runtime_sidecars` 与稳定导出视图查询入口；`execution_sidecar_exports` 现可作为 bootstrap / runtime 统一导出面的输入
  - 已将 sidecar 读写与导出收口到专属 store helper，`apply_recovery_resume_input`、`apply_resume_dispatch_decision`、`attach_recovery_id`、`clear_execution_ownership`、`execution_sidecar_exports` 统一经由 sidecar store 处理
- 已支持恢复消费后的 sidecar 状态同步，`apply_resume_dispatch_decision` 会把 `current_status / last_update / execution_chain_ref / recovery_ref` 一次性收口回 session sidecar
- 已支持上层 recovery consume 入口直接消费 `RecoveryResumeInput`，并在恢复执行时继续推进到 worker execute
- 已拆出 `SessionDurableState` 作为 aggregate/timeline/notification 的独立持久化状态，并保留 `SessionExecutionSidecarStoreState` 作为 sidecar 专属持久化子结构；daemon 现可将两者分别落到 `sessions.json` 与 `session-sidecars.json`
- 已支持从 `SessionDurableState + SessionExecutionSidecarStoreState` 重建 `SessionStore`，并对旧单文件 `sessions.json` 中内嵌 sidecar 的布局保持兼容读取
- 已补 `flush_execution_sidecars_with(...)` 显式 flush hook，只对刷新的 sidecar 文件落盘；sidecar 变更现在会统一标记 dirty，而不是由 daemon 每次全量直写
- 已把 `bind_execution_ownership`、`apply_recovery_resume_input`、`apply_resume_dispatch_decision`、`attach_recovery_ref`、`clear_execution_ownership` 全部接入同一套 sidecar dirty 追踪，恢复消费后的细粒度写回时机已经稳定
- 已补显式 `SessionSidecarFlushMetadata`，包含 `last_dirty_reason / last_dirty_at / next_flush_hint / last_flush_at`，为 daemon 后续自动调度提供稳定输入，但不改 sidecar 顶层 export 语义
- 已让 `projection_input`、`sessions`、`timeline`、`notifications`、`runtime_sidecars` 按稳定字典序输出
  - 已让 `apply_recovery_resume_input`、`apply_resume_dispatch_decision`、`attach_recovery_id` 只在 recovery / execution chain 语义一致时合并
  - 已支持消费 `RecoveryResumeInput`
- 删除内容：
  - 已移除 Rust 侧 `unknown` 风格的运行态透传
- 未完成边界：
  - 尚未扩展更细的 session 恢复编排策略
  - 尚未把 flush metadata 接到更长生命周期的 daemon 自动调度策略
- 后续依赖：
  - `magi-workspace`
  - `magi-orchestrator`
  - `magi-event-bus`
