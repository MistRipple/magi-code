# P7 合规收敛待办

## 背景
P1-P6d 已落地并 commit 到 main，但在 P1 / P2 / P6a / P6b / P6c / P6d 中引入了若干过渡实现，违反 `cn-engineering-standard` 禁止"双轨路径 / 兼容层 / 过渡实现"的硬性规定。P7 一次性收敛全部违规，不再分阶段留过渡。

推荐参考上一次对话（2026-05-13）中产出的"P7 一次性收敛架构蓝图"与"违规审计"，如果会话已归档可直接依据本文件重新规划。

## 违规清单（全部需要一次性修复）

1. `ActiveExecutionTurnItem.thread_visible / worker_visible` 双布尔与 `source_thread_id` 并存 —— 同一职责（可见性路由）两套实现
2. `CanonicalTurnVisibility.thread_visible / worker_visible / worker_tab_ids` 前后端镜像双轨
3. `ExecutionThread.mission_id: Option<MissionId>` —— P6c 为了让 orchestrator session 级 thread 能存在引入的让步
4. `ActiveExecutionTurnItem.source_thread_id: Option<ThreadId>` —— user_message 找不到归属 thread 的让步
5. `ActiveExecutionTurnLane.thread_id: Option<ThreadId>` —— P6a 引入的兼容字段
6. `web/src/shared/timeline-worker-lifecycle.ts::resolveTimelineWorkerId(_options: { fallbacks })` —— 过渡形参，注释已写"下个阶段清理"
7. `web/src/types/message.ts::DispatchGroupLane.worker` —— P1.5 将兜底去掉但字段保留，实际已沦为显示名来源冗余

## 修复方案（必须一次性完成，不得再留过渡）

### A. 入口（mission 前移）
- 新增 `magi-orchestrator::ensure_session_mission(session_id)`：返回当前有效 `mission_id`；若 session 无 mission 则创建新 mission 并 spawn `role = ORCHESTRATOR_ROLE_ID` 的 ExecutionThread（mission_id 必填）
- `routes/mod.rs::session_action_route / session_task_route / session_turn_route` 在接收 user 提交的第一步调用 `ensure_session_mission`
- `SessionStore::create_session` 不再 spawn orchestrator thread（P6c 原地删除那段代码）

### B. 后端可见性单一路由
- `ActiveExecutionTurnItem` 删 `thread_visible / worker_visible`，把 `source_thread_id` 改为 `ThreadId`（非 Option）
- `CanonicalTurnVisibility` 仅保留 `renderable: bool`，删 `thread_visible / worker_visible / worker_tab_ids`
- `CanonicalTurnItem` 增 `source_thread_id: ThreadId`（非 Option）
- 所有写入点（session_turn_writeback / task_llm_loop / dispatch_execution / routes / execution_chain_recovery / session_turn_execution）改为设 `source_thread_id` 单一字段
- 投影/可见性判定改为 `source_thread_id == orchestrator_thread_id` → 主线可见；`thread_registry[source_thread_id].role_id` 非 orchestrator → 对应 worker drawer 可见

### C. Option 全删
- `ExecutionThread.mission_id: MissionId`（删 Option）
- `ActiveExecutionTurnTurnLane.thread_id: ThreadId`（删 Option）
- `ActiveExecutionTurnItem.source_thread_id: ThreadId`（删 Option）
- 所有 fixture、测试、recovery 路径同步；user_message 归属当前 mission 的 orchestrator thread

### D. 前端 P1 兵底清理
- `timeline-worker-lifecycle.ts::resolveTimelineWorkerId` 删 `_options: { fallbacks }` 形参，所有调用点（messages.svelte.ts、message-utils.ts）同步
- `types/message.ts::DispatchGroupLane.worker` 字段删除（`jumpTarget.workerTabId` 是唯一身份）

### E. 前端可见性路由
- `CanonicalTurnVisibility` TS 定义仅保留 `renderable`
- 投入 `messagesState.executionThreadSnapshot: ExecutionThread[]`（类似 settingsRegistrySnapshot 机制），bootstrap / state_update 走 data-message-handlers 透传
- `turn-projection.ts::buildCanonicalTimelineProjection` 根据 `sourceThreadId` + thread_registry 决定分流：等于 orchestrator thread_id 进 `threadRenderEntries`，否则按 thread 的 role_id 进 `workerRenderEntries[roleId]`

### F. Fixture + Golden 全量重写
- `web/scripts/canonical-turn-golden.mjs` 所有 fixture 的 `visibility` 结构改为 `{ renderable: bool }`；每条 item 新增 `sourceThreadId`；新增 `threadRegistry` mock 并作为 `buildCanonicalTimelineProjection` 第二参数传入
- cargo 测试里 ~30 处 `ActiveExecutionTurnItem` fixture 同步
- `magi-daemon/src/daemon/tests.rs` fixture 同步

## 验证（全部绿才算完成）
- `cargo test -p magi-api --lib`（预期有 3 个 pre-existing sandbox 失败，与 P7 无关）
- `cargo test -p magi-session-store --lib`（38/38）
- `cargo test -p magi-daemon --lib`
- `cd web && npm run check`（0 error 0 warning）
- `cd web && node scripts/canonical-turn-golden.mjs`（pass）
- 用户重启 daemon 做真实派发验证

## 工作量估计
- ~1500-2000 行改动，~30 文件
- 预计 300+ 工具调用；建议**不**试图一次会话做完，每次会话内完成一个子范围后 commit，下次接着做

## 规范硬性约束
- 不得再留"下一阶段清理"式过渡注释
- 不得再引入任何 `Option<>` 兼容字段
- 每次 commit 必须是独立合规变更（一次收敛一个职责，不要"先加再清"的双轨中间态）

## 提交策略
按职责一次性收敛，每次 commit message 明确：

- `refactor: ensure_session_mission 前移，纯聊天也有 mission（P7.A）`
- `refactor: 删除 thread_visible/worker_visible 双布尔，可见性统一走 source_thread_id（P7.B）`
- `refactor: mission_id / source_thread_id / thread_id 全改非 Option（P7.C）`
- `refactor: 前端 P1 兜底参数和 DispatchGroupLane.worker 彻底清理（P7.D）`

如果分多次 commit，**每次 commit 结束时代码必须是干净的唯一实现**（不是"先加新字段旧字段留着"），否则仍是过渡违规。

## 上下文入口
- 本仓库规范：`cn-engineering-standard`
- 前一轮规划对话：2026-05-13（含完整蓝图与违规审计）
- 上一次 commit：`ff03297 refactor: 收口 lane 概念为 thread per-turn 快照（P6d）`
