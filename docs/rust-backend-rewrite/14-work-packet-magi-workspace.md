# Agent 任务单：magi-workspace 工作区与恢复边界

更新时间：2026-04-16

---

## 1. 任务名称

- 名称：`magi-workspace` 工作区与恢复边界任务单
- 编号：`WP-WORKSPACE-001`
- 负责 Agent：Workspace Agent

## 2. 写域

- 唯一写域：`crates/magi-workspace`
- 禁止修改范围：
  - 现有 `src/**` 运行代码
  - API 路由层
  - session aggregate
  - orchestrator / worker 状态机
- 依赖的上游文档：
  - [02-capability-matrix.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/02-capability-matrix.md)
  - [03-semantic-deviation-ledger.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/03-semantic-deviation-ledger.md)
  - [04-module-mapping-and-target-crates.md](/Users/xie/code/magi-rust-rewrite/docs/rust-backend-rewrite/04-module-mapping-and-target-crates.md)

## 3. 背景

- 当前能力域：workspace registry / roots / worktree / snapshot / recovery 关联资源
- 当前实现位置：
  - `src/workspace/worktree-manager.ts`
  - `src/workspace/workspace-roots.ts`
  - `src/snapshot-manager.ts`
  - `src/agent/service/local-agent-service.ts` 中 workspace registry 部分
- 当前问题：
  - workspace registry、worktree、snapshot、恢复关联资源分散
  - workspace 级与 session 级边界在运行时服务中混装
  - worktree/snapshot/recovery 的长期归属不够清晰

## 4. 根本原因

1. 旧实现先有运行服务，再把 workspace 能力逐步塞进去
2. workspace 作为平台级隔离边界，没有被单独收口
3. snapshot / recovery 既和 workspace 绑定，又被编排链直接透传

## 5. 目标

- 本任务要完成的 Rust 目标结构：
  - 建立 `magi-workspace`
  - 收口 workspace registry、workspace roots、worktree、snapshot、recovery 关联资源
  - 形成“工作区级边界”而不是“运行服务里顺手管理”
  - 建立 recovery sidecar store / 独立子结构，避免恢复句柄散落在聚合边上
- 本任务不做什么：
  - 不实现 session timeline
  - 不实现 orchestrator 任务状态机
  - 不实现 UI / host 行为
- 与其他 Agent 的边界：
  - workspace 只负责工作区级真相
  - session / orchestrator / worker 通过明确接口使用 workspace 能力

## 6. 产出物

- 新增 crate / module：
  - `crates/magi-workspace`
  - `registry`
  - `roots`
  - `worktree`
  - `snapshot`
  - `recovery_scope`
- 新增 schema：
  - 当前阶段无必须新增对外 schema
- 更新文档：
  - 若恢复边界收敛，回写 `D-002`、`D-004`
- 必须删除的旧实现或冗余结构：
  - 当前阶段不删除主仓运行代码
  - 但禁止在 Rust workspace crate 中继续混入 session 与 API 逻辑

## 7. 语义约束

- 本任务涉及的真相源：
  - Workspace registry
  - Worktree allocation
  - Snapshot metadata
  - Recovery 关联资源边界
- 是否涉及协议变化：
  - 当前阶段不直接改外部 API
- 是否涉及语义偏差台账登记：
  - 是，需对齐 `D-002`、`D-004`

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

- workspace crate 不得承担 session 或 API 职责
- snapshot / recovery 语义必须可解释、可恢复、可审计

## 9. 验收标准

- 编译：
  - `magi-workspace` 可独立编译
- 最小运行验证：
  - workspace register / resolve
  - worktree allocate / merge / release
  - snapshot create / restore 基础语义
- 协议验证：
  - workspace 对外暴露的接口能被 API / runtime 清晰消费
- 清理验证：
  - crate 内无 session / API / UI 混装逻辑

## 10. 输出结论

- 已完成内容：
  - 已建立 `WorkspaceStoreState`、`WorkspaceProjectionInput`
  - 已支持 register / activate / assign worktree root / release worktree root
  - 已建立 snapshot metadata 与 recovery handle 骨架
  - 已提供 `export_state` / `from_state` 供 daemon 层持久化
  - 已补齐 worktree / snapshot / recovery 的 session / mission / execution ownership
  - 已建立 recovery sidecar store 独立子结构，并提供 `recovery_sidecar_store_state` 与稳定导出视图查询入口；`recovery_sidecar_exports` 现可作为 bootstrap / runtime 统一导出面的输入
  - 已建立 recovery entry point 查询入口与诊断摘要字段
  - 已将 recovery sidecar 的 lookup / ready / consume / resume / export 构建收口到专属 store helper，registry 不再直接散落操作 handle 向量
- 已支持恢复消费后的 sidecar 状态同步，`consume_recovery` 会把 `current_status / last_update / recovery_ref / execution_chain_ref` 保持在 recovery sidecar 导出面上
- 已支持上层 recovery consume 入口把 `build_recovery_resume_input -> resume decision -> worker execute` 串起来，并在消费后保持 workspace recovery 导出一致性
- 已拆出 `WorkspaceDurableState` 作为 registry/worktree/snapshot 的独立持久化状态，并保留 `WorkspaceRecoverySidecarStoreState` 作为 recovery sidecar 专属持久化子结构；daemon 现可将两者分别落到 `workspaces.json` 与 `workspace-recovery-sidecars.json`
- 已支持从 `WorkspaceDurableState + WorkspaceRecoverySidecarStoreState` 重建 `WorkspaceStore`，并对旧单文件 `workspaces.json` 中内嵌 `recovery_handles` 的布局保持兼容读取
- 已补 `flush_recovery_sidecars_with(...)` 显式 flush hook，只对刷新的 recovery sidecar 文件落盘；`prepare / ready / consume` 现在都会统一标记 dirty
- 已补显式 `WorkspaceRecoveryFlushMetadata`，包含 `last_dirty_reason / last_dirty_at / next_flush_hint / last_flush_at`，为 daemon 后续自动调度提供稳定输入，但不改 recovery sidecar 顶层 export 语义
- 已把 recovery consume 后的 `current_status / last_update / execution_chain_ref / recovery_ref` 写回时机收口到同一套 sidecar dirty/flush 链，不再依赖上层全量保存
- 已让 `projection_input`、`workspaces`、`snapshots`、`recovery_handles`、`worktree_allocations` 按稳定字典序输出
  - 已补齐 `mark_recovery_ready` / `consume_recovery` 的显式状态守卫
  - 已支持 `active_recovery_handles` 与 `build_recovery_resume_input`
  - 已支持从 recovery entry 构建统一 `RecoveryResumeInput`
- 删除内容：
  - 无
- 未完成边界：
  - 尚未接入真实 worktree 分配与释放
  - 尚未扩展更细的 workspace 恢复编排策略
  - 尚未把 flush metadata 接到更长生命周期的 daemon 自动调度策略
- 后续依赖：
  - `magi-session-store`
  - `magi-orchestrator`
  - `magi-governance`
