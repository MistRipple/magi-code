# Magi 稳定性修复验收报告

- 验收日期：2026-10-04
- 仓库：`/Users/xie/code/magi-rust-rewrite`
- 验收基线：`486130e6`
- 工作区：`main`，验收前已 `git pull --ff-only origin main`
- 状态：未修改业务代码，未提交；报告文件是本次唯一新增文件
- 内存：16 GiB；本次未发生 OOM
- 运行状态根：`/Users/xie/.magi`（按要求使用现有工程目录，未创建临时项目目录）

## 汇总

| 编号 | 验收项 | 结果 | 关键证据 |
|---|---|---|---|
| 1 | `cargo test -p magi-api` | **失败** | 825 passed、25 failed、2 ignored；无 OOM。指定回归 `reset_stats_counts_only_usage_after_reset_marker` 单独通过。 |
| 2 | `cargo test -p magi-event-bus` | **通过** | 51 passed。 |
| 3 | `cargo test -p magi-daemon` | **通过** | 141 passed；题目列出的两个旧失败本次未重现。 |
| 4 | Desktop check 与测试 | **通过** | `npm run check --workspace @magi/desktop` 通过；测试 122 passed、0 failed。Node 22 使用 `Timeout.prototype.unref` 空操作 preload。 |
| 5 | Web check 与测试 | **通过** | `npx svelte-check --tsconfig ./tsconfig.json`：0 errors、0 warnings；`npm test` 全部 golden 测试通过。 |
| A | 代理页面后台保活（P1-12，`cf70fae8`） | **未能验证** | 真实代理任务在进入多步浏览器操作前因 `model_rate_limited` 失败；daemon 记录 HTTP 429。 |
| B | 自动化不改布局（P3-6） | **未能验证** | 没有成功进入浏览器操作阶段；切到会话 B 时右栏为空状态，未显示 A 页面。Desktop 单测覆盖的布局保护通过。 |
| C | 用户接管与“交还控制”（P0-6，`d1d172f9`） | **未能验证** | 没有可供用户接管的成功运行中的代理页面。 |
| D | 启动失败错误窗口（P0-7/P0-8，`1f8f457d`、`60a436ba`） | **通过** | 人为占用 Vite `3000` 端口后，出现可读错误窗口，显示原因、最近输出、日志路径，并提供“重试 / 打开日志位置 / 退出”。 |
| E1 | 审计/用量账本迁移（P2-4，`a358c00c`） | **通过** | 启动前旧文件存在；启动后旧文件消失并生成 `audit-usage-ledger/segment-*.jsonl`。 |
| E2 | 账本增量写入 | **未能完整验证** | 真实对话被模型限流，未能观察正常对话期间的持续追加。 |
| E3 | 重置执行统计 | **通过** | 设置页归零；账本追加 `usage.stats.reset`（sequence `2101005`）；重启后 `/api/settings/stats` 仍为全零。 |
| F | 会话事件日志检查点（P2-5，`486130e6`） | **未能验证** | 真实状态根没有 `checkpoint-*.json`；最大会话目录只有 56 个事务文件，未达到 128 次阈值。相关自动化测试中的生成、截断、恢复路径通过。 |

## 自动化测试详情

Rust 测试按要求串行执行，环境变量为：

```text
CARGO_BUILD_JOBS=1
CARGO_PROFILE_TEST_DEBUG=0
CARGO_PROFILE_DEV_DEBUG=0
CARGO_INCREMENTAL=0
```

`magi-api` 的 25 个失败集中在持久队列重放和受限写操作审批边界，失败名称如下：

```text
start_turn_replays_persisted_queue_without_enqueuing_again
session_turn_queue_routes_expose_and_remove_persisted_turns
restricted_profile_approval_denial_fails_without_repeating_write_tool
restricted_profile_background_shell_approval_denial_preserves_side_effect_boundary
restricted_profile_background_shell_approval_expiry_preserves_side_effect_boundary
restricted_profile_expired_approval_rejects_without_side_effect
restricted_profile_file_remove_denial_preserves_path_without_retry
restricted_profile_git_branch_create_denial_preserves_branch_state
restricted_profile_git_branch_create_expiry_preserves_branch_state
restricted_profile_git_branch_delete_denial_preserves_branch
restricted_profile_git_branch_delete_expiry_preserves_branch
restricted_profile_git_branch_switch_allow_for_turn_does_not_cross_session
restricted_profile_git_branch_switch_allow_for_turn_does_not_cross_turn
restricted_profile_git_branch_switch_denial_preserves_branch_and_fails_turn
restricted_profile_git_branch_switch_expiry_preserves_branch_without_resolution
restricted_profile_git_merge_denial_preserves_workspace
restricted_profile_git_merge_expiry_preserves_workspace
restricted_profile_git_pull_denial_preserves_workspace
restricted_profile_git_pull_expiry_preserves_workspace
restricted_profile_git_push_denial_preserves_remote
restricted_profile_git_push_expiry_preserves_remote
restricted_profile_git_worktree_create_denial_preserves_worktrees
restricted_profile_git_worktree_create_expiry_preserves_worktrees
restricted_profile_git_worktree_remove_denial_preserves_worktree
restricted_profile_git_worktree_remove_expiry_preserves_worktree
```

## Electron 真实验收详情

### A/B/C：浏览器代理相关场景

最小复现（A）：

1. 启动桌面版。
2. 新建会话，发送多步内置浏览器任务。
3. 切换到另一个会话并等待。
4. 原任务最终显示 `model_rate_limited`，错误码 `-32006`，HTTP 429。

由于代理没有成功运行到浏览器多步阶段，A 的后台保活、B 的布局保持和 C 的用户接管均只能标记为“未能验证”，不能据此判定实现失败。

### D：启动失败窗口

复现方式：先占用 Vite `3000` 端口，再按桌面开发启动方式启动 daemon/桌面版。

实际现象：窗口标题为“**Magi 后台服务无法启动**”，包含失败原因、最近输出和完整日志路径，并提供“重试”“打开日志位置”“退出”操作。完整日志：

`/Users/xie/Library/Logs/@magi/desktop/daemon.log`

## 持久化变更详情

### E：审计/用量账本

- 迁移前 `/Users/xie/.magi/audit-usage-ledger.json` 存在。
- daemon 启动后旧文件消失，生成 `audit-usage-ledger/segment-*.jsonl`（本次观察到两个 segment 文件）。
- 设置页“重置执行统计”后显示 0 调用、0 Token。
- 账本中出现 `usage.stats.reset`，sequence 为 `2101005`。
- daemon 重启后：`GET http://127.0.0.1:38123/api/settings/stats` 返回 totals 全为 0，`items` 和 `models` 为空。
- 因真实模型请求被 HTTP 429 限流，无法完整观察正常对话中的 segment 持续追加及写入频率。

### F：会话事件日志检查点

- 当前真实状态根没有 `checkpoint-*.json`。
- 最大 `session-events/<会话>/` 目录只有 56 个事务文件，未达到“新增提交超过 128 次”的触发条件。
- 自动化测试已覆盖检查点生成、事务截断和重启恢复路径，未报告失败。

## 已知限制与后续复现条件

1. 需要一个可用的模型配置或解除 HTTP 429 限流，才能完成 A/B/C 和 E2 的真实运行验收。
2. 需要选取或构造至少 128 个新增事件提交的历史会话，才能完成 F 的真实检查点生成、截断和重启验证。
3. `magi-api` 的失败并非 OOM；应单独跟进上文列出的 25 个失败测试。

## 验收结束状态

验收结束时 daemon、Vite 和 Electron 进程已停止；`git status` 保持干净（除本报告文件外无业务代码改动），没有创建提交。

---

## 2026-10-05 `d1a30114` 回归诊断

本节是在 `git pull --ff-only origin main` 更新到 `d1a30114` 后追加的诊断记录。没有修改业务代码，也没有创建提交。

测试环境变量：

```text
CARGO_BUILD_JOBS=1
CARGO_PROFILE_TEST_DEBUG=0
CARGO_PROFILE_DEV_DEBUG=0
CARGO_INCREMENTAL=0
```

### 队列测试

| 测试 | 结果 | 关键输出 |
|---|---|---|
| `start_turn_replays_persisted_queue_without_enqueuing_again` | **失败** | `crates/magi-api/src/app_server.rs:2497:9`；`assertion left == right`，`left: Null`，`right: true`。 |
| `session_turn_queue_routes_expose_and_remove_persisted_turns` | **通过** | `1 passed; 0 failed`。 |

### 3 个代表性测试

#### `restricted_profile_approval_denial_fails_without_repeating_write_tool`

命令：

```text
env CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo test -p magi-api restricted_profile_approval_denial_fails_without_repeating_write_tool -- --nocapture
```

完整 panic：

```text
thread 'turn_harness::tests::restricted_profile_approval_denial_fails_without_repeating_write_tool' (14284789) panicked at crates/magi-api/src/turn_harness.rs:9556:9:
assertion `left == right` failed: 拒绝不可重试的写工具后不得再次请求 Provider
  left: 200
 right: 1
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test turn_harness::tests::restricted_profile_approval_denial_fails_without_repeating_write_tool ... FAILED
```

- Turn 最终状态：`Failed`（`turn.status` 断言在请求次数断言前已通过）。
- Task 最终状态：`Failed`（测试已成功等待到 Task 终态，且该失败收口路径写入 `TaskStatus::Failed`）。
- 工具错误：canonical `ToolCall` 的 `shell_exec` 错误包含“拒绝”；审批结果的错误码为 `tool_approval_denied`，文字为“用户拒绝了本次工具操作”。
- 非分类器模型请求次数：**200**；断言期望 **1**。这已达到 `MAX_MODEL_ROUNDS_PER_TURN = 200`，表现为拒绝后仍反复请求 Provider。

#### `restricted_profile_git_branch_switch_expiry_preserves_branch_without_resolution`

命令：

```text
env CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo test -p magi-api restricted_profile_git_branch_switch_expiry_preserves_branch_without_resolution -- --nocapture
```

完整 panic：

```text
thread 'turn_harness::tests::restricted_profile_git_branch_switch_expiry_preserves_branch_without_resolution' (14285126) panicked at crates/magi-api/src/turn_harness.rs:9202:9:
assertion `left == right` failed
  left: Failed
 right: Completed
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test turn_harness::tests::restricted_profile_git_branch_switch_expiry_preserves_branch_without_resolution ... FAILED
```

- Turn 最终状态：实际 `Failed`，期望 `Completed`（失败行 `turn_harness.rs:9202`）。
- Task 最终状态：测试已等待到 Task 终态，但因为 Turn 状态断言先失败，没有把 `task.status` 打印到 stdout；该执行失败收口对应 `TaskStatus::Failed`。
- 失败原因文字：该测试在检查 canonical `ToolCall` 错误前中断；审批过期结果的定义文字为“工具授权请求已过期，原始操作未执行”，错误码 `tool_approval_expired`。实际最终 Turn 错误正文没有被该测试打印。
- 模型请求次数：该测试后面的 `assert_eq!(non_classifier_provider_request_count(&harness), 2)` 没有执行，因此 stdout 没有直接打印计数。结合实际 `Failed` 终态、同一错误路径达到共享 200 轮上限的测试结果，诊断为 **200 次**（期望 **2**）；这是基于执行路径的推断，非该测试后置断言直接输出。

#### `restricted_profile_expired_approval_rejects_without_side_effect`

命令：

```text
env CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo test -p magi-api restricted_profile_expired_approval_rejects_without_side_effect -- --nocapture
```

完整 panic：

```text
thread 'turn_harness::tests::restricted_profile_expired_approval_rejects_without_side_effect' (14285487) panicked at crates/magi-api/src/turn_harness.rs:9769:9:
assertion `left == right` failed
  left: Failed
 right: Completed
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
test turn_harness::tests::restricted_profile_expired_approval_rejects_without_side_effect ... FAILED
```

- Turn 最终状态：实际 `Failed`，期望 `Completed`（失败行 `turn_harness.rs:9769`）。
- Task 最终状态：测试已等待到 Task 终态，但 Turn 断言先中断，stdout 没有直接打印 `task.status`；该失败收口对应 `TaskStatus::Failed`。
- 失败原因文字：过期审批结果定义为错误码 `tool_approval_expired`，文字为“工具授权请求已过期，原始操作未执行”；该测试在检查 canonical `ToolCall` 错误前中断，实际最终 Turn 错误正文未打印。
- 模型请求次数：该测试后面的 `assert_eq!(non_classifier_provider_request_count(&harness), 1)` 没有执行，stdout 没有直接计数；结合相同 200 轮失败路径，诊断为 **200 次**（期望 **1**），同样标注为执行路径推断。

### `cargo test -p magi-api restricted_profile_` 全量结果

本次运行：`41 passed; 23 failed; 0 ignored; 0 measured; 788 filtered out`，耗时 111.19 秒。以下为全部失败测试及 panic 行号：

| 测试 | 失败位置 |
|---|---|
| `restricted_profile_file_remove_denial_preserves_path_without_retry` | `crates/magi-api/src/turn_harness.rs:9896:9` |
| `restricted_profile_approval_denial_fails_without_repeating_write_tool` | `crates/magi-api/src/turn_harness.rs:9556:9` |
| `restricted_profile_expired_approval_rejects_without_side_effect` | `crates/magi-api/src/turn_harness.rs:9769:9` |
| `restricted_profile_git_branch_create_denial_preserves_branch_state` | `crates/magi-api/src/turn_harness.rs:8127:9` |
| `restricted_profile_background_shell_approval_denial_preserves_side_effect_boundary` | `crates/magi-api/src/turn_harness.rs:5373:9` |
| `restricted_profile_background_shell_approval_expiry_preserves_side_effect_boundary` | `crates/magi-api/src/turn_harness.rs:5373:9` |
| `restricted_profile_git_branch_create_expiry_preserves_branch_state` | `crates/magi-api/src/turn_harness.rs:1438:9` |
| `restricted_profile_git_branch_delete_denial_preserves_branch` | `crates/magi-api/src/turn_harness.rs:1438:9` |
| `restricted_profile_git_branch_switch_allow_for_turn_does_not_cross_turn` | `crates/magi-api/src/turn_harness.rs:8839:9` |
| `restricted_profile_git_branch_delete_expiry_preserves_branch` | `crates/magi-api/src/turn_harness.rs:1438:9` |
| `restricted_profile_git_branch_switch_allow_for_turn_does_not_cross_session` | `crates/magi-api/src/turn_harness.rs:1438:9` |
| `restricted_profile_git_branch_switch_denial_preserves_branch_and_fails_turn` | `crates/magi-api/src/turn_harness.rs:9045:9` |
| `restricted_profile_git_branch_switch_expiry_preserves_branch_without_resolution` | `crates/magi-api/src/turn_harness.rs:9202:9` |
| `restricted_profile_git_merge_denial_preserves_workspace` | `crates/magi-api/src/turn_harness.rs:6775:9` |
| `restricted_profile_git_merge_expiry_preserves_workspace` | `crates/magi-api/src/turn_harness.rs:1438:9` |
| `restricted_profile_git_pull_denial_preserves_workspace` | `crates/magi-api/src/turn_harness.rs:6465:9` |
| `restricted_profile_git_pull_expiry_preserves_workspace` | `crates/magi-api/src/turn_harness.rs:1438:9` |
| `restricted_profile_git_push_denial_preserves_remote` | `crates/magi-api/src/turn_harness.rs:6166:9` |
| `restricted_profile_git_push_expiry_preserves_remote` | `crates/magi-api/src/turn_harness.rs:6166:9` |
| `restricted_profile_git_worktree_create_denial_preserves_worktrees` | `crates/magi-api/src/turn_harness.rs:1438:9` |
| `restricted_profile_git_worktree_create_expiry_preserves_worktrees` | `crates/magi-api/src/turn_harness.rs:1438:9` |
| `restricted_profile_git_worktree_remove_denial_preserves_worktree` | `crates/magi-api/src/turn_harness.rs:7469:9` |
| `restricted_profile_git_worktree_remove_expiry_preserves_worktree` | `crates/magi-api/src/turn_harness.rs:7469:9` |

全量失败中，直接显示 `left: 200 / right: 1` 的项目说明审批结果后仍达到 200 次模型请求；`:1438:9` 项目是等待 Turn 在 10 秒测试窗口内进入终态失败；两个后台 shell 项目在 `:5373:9` 没有观察到 `tool.approval.requested` 事件。

---

## 2026-10-05 `570e1fd0` / `d2997939` 验证

已执行 `git pull --ff-only origin main`。当前 HEAD 为 `d2997939`，`570e1fd0` 在当前历史中；未修改业务代码，未提交。测试均使用：

```text
CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
```

### 1. 排队重放测试

```text
cargo test -p magi-api start_turn_replays_persisted_queue_without_enqueuing_again -- --nocapture
```

**通过**：`1 passed; 0 failed; 0 ignored; 0 measured; 799 filtered out`。

### 2. `restricted_profile_` 全量

**通过**：`64 passed; 0 failed; 0 ignored; 0 measured; 736 filtered out`，耗时 11.24 秒。

没有失败项，因此第 3 步没有需要单独重跑的 `restricted_profile_` 测试，也没有新增 panic。

### 4. 完整 `magi-api` 测试

```text
cargo test -p magi-api
```

结果：`778 passed; 20 failed; 2 ignored; 0 measured`，耗时 43.78 秒。失败测试及 panic 行如下：

| 测试 | panic 行 / 关键信息 |
|---|---|
| `mcp_runtime::tests::a_named_tunnel_gives_a_fixed_address_and_is_restored_after_restart` | `crates/magi-api/src/mcp_runtime.rs:1878:9`；`命名隧道开着时重启后应恢复` |
| `git_tool_runtime::tests::agent_apply_brings_finished_agent_changes_into_main_worktree_once` | `crates/magi-api/src/git_tool_runtime.rs:1058:9`；`应用后删除代理分支` |
| `routes::sessions::tests::a_skill_picked_for_a_plain_chat_turn_reaches_the_turn_instead_of_being_dropped` | `crates/magi-api/src/routes/sessions.rs:6259:9`；left `String("execute")`，right `"chat"` |
| `routes::sessions::tests::new_goal_mode_session_requires_goal_creation_before_plan` | `crates/magi-api/src/routes/sessions.rs:9098:9`；left `["get_goal", "create_goal", "update_plan"]`，right `["get_goal", "create_goal", "update_plan", "shell_exec"]` |
| `routes::sessions::tests::session_interrupt_starts_next_queued_turn_and_preserves_fifo_tail` | `crates/magi-api/src/routes/sessions.rs:8031:9`；`普通 Conversation Turn 不应创建 TaskStore root task` |
| `turn_harness::tests::duplicate_request_replays_and_fingerprint_conflict_is_rejected` | `crates/magi-api/src/turn_harness.rs:2421:14`；`InternalAssemblyError("构建任务派发运行时: task_store 未配置")` |
| `turn_harness::tests::empty_provider_response_becomes_a_single_failed_terminal_turn` | `crates/magi-api/src/turn_harness.rs:12557:14`；`InternalAssemblyError("构建任务派发运行时: task_store 未配置")` |
| `turn_harness::tests::provider_failure_becomes_a_single_failed_terminal_turn` | `crates/magi-api/src/turn_harness.rs:11928:14`；`InternalAssemblyError("构建任务派发运行时: task_store 未配置")` |
| `turn_harness::tests::read_only_profile_rejects_explicit_apply_patch_without_side_effect` | `crates/magi-api/src/turn_harness.rs:1755:9`；left `1`，right `0` |
| `turn_harness::tests::read_only_profile_rejects_explicit_file_copy_and_move_without_side_effect` | `crates/magi-api/src/turn_harness.rs:4124:9`；left `1`，right `0` |
| `turn_harness::tests::read_only_profile_rejects_explicit_file_patch_mkdir_and_remove_without_side_effect` | `crates/magi-api/src/turn_harness.rs:1755:9`；left `1`，right `0` |
| `turn_harness::tests::read_only_profile_rejects_explicit_file_write_without_approval_or_side_effect` | `crates/magi-api/src/turn_harness.rs:4012:9`；left `1`，right `0` |
| `turn_harness::tests::read_only_profile_rejects_explicit_image_and_git_writes` | `crates/magi-api/src/turn_harness.rs:1755:9`；left `1`，right `0` |
| `turn_harness::tests::reconnect_snapshot_replays_persisted_turn_events_after_receiver_drop` | `crates/magi-api/src/turn_harness.rs:11351:14`；`InternalAssemblyError("构建任务派发运行时: task_store 未配置")` |
| `turn_harness::tests::sse_reconnect_replays_canonical_turn_snapshot_over_real_router_body` | `crates/magi-api/src/turn_harness.rs:11398:14`；`InternalAssemblyError("构建任务派发运行时: task_store 未配置")` |
| `turn_harness::tests::steer_routes_to_the_active_turn_and_keeps_the_same_turn_identity` | `crates/magi-api/src/turn_harness.rs:11713:14`；`InternalAssemblyError("构建任务派发运行时: task_store 未配置")` |
| `turn_harness::tests::task_profile_spawns_multiple_children_and_waits_for_all_results` | `crates/magi-api/src/turn_harness.rs:11266:9`；left `Failed`，right `Completed` |
| `turn_harness::tests::timeout_provider_failure_retries_before_first_delta_and_completes` | `crates/magi-api/src/turn_harness.rs:12190:14`；`InternalAssemblyError("构建任务派发运行时: task_store 未配置")` |
| `turn_harness::tests::transient_provider_failure_retries_before_first_delta_and_completes` | `crates/magi-api/src/turn_harness.rs:12163:14`；`InternalAssemblyError("构建任务派发运行时: task_store 未配置")` |
| `turn_harness::tests::websocket_reconnect_replays_canonical_turn_over_real_app_server` | `crates/magi-api/src/turn_harness.rs:11486:14`；`InternalAssemblyError("构建任务派发运行时: task_store 未配置")` |

本次完整包测试的失败主要包含 `task_store 未配置` 装配错误、只读 profile 仍进入一次 Provider、路由/目标工具契约断言，以及一个多子代理任务最终状态不符。工作区除本报告文件外没有未提交改动。

---

## 2026-10-06 最新 main 全自动验收

### 基线与范围

- `git pull --ff-only origin main`：Already up to date。
- 当前 HEAD：`8a3b44b37af37865b0ca540549e71611c3e81448`（`8a3b44b3`，H10 收尾；当前 main 没有 2026-10-06 新提交，最新提交时间为 2026-10-04）。
- 未修改业务代码，未提交；工作区唯一未跟踪文件仍是本报告。
- Rust 环境：`CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0`。
- 测试过程中启动的最新 Desktop 使用 build identity `8a3b44b3`，结束时已停止；没有留下 38123 端口监听。

### 自动化测试汇总

| 范围 | 结果 | 证据 |
|---|---|---|
| `cargo test -p magi-api` | **失败** | `765 passed; 22 failed; 2 ignored`，40.31s；完整输出 `/tmp/magi-api-20261006.log`。 |
| `cargo test -p magi-daemon` | **失败** | `116 passed; 5 failed`，18.21s；失败见下表。 |
| 其余 30 个 Rust package（逐 package、串行） | **通过** | 合计 `1935 passed; 0 failed; 1 ignored`；`magi-daemon-app` 无测试。原始汇总 `/tmp/magi-rust-packages-20261006.log`。 |
| `cargo test --workspace --no-fail-fast` | **未完成** | API 测试执行到 `restricted_profile_git_branch_switch_allow_for_turn_does_not_cross_turn` 超过 60s 未结束，手动中断；随后已逐 package 完成除 API 外的覆盖。 |
| `cargo test -p magi-process` | **失败** | `12 passed; 1 failed`，失败为 `production_code_uses_the_shared_process_factory`。 |
| `npm test`（Desktop + Worker + Web） | **通过** | Desktop `128 passed`；Worker `90 passed`；Web golden 全链路退出码 0。 |
| `npm run check` | **通过** | Desktop、Worker、Web 均通过；Svelte `0 errors, 0 warnings`。 |
| `npm run release:guard` | **通过** | Browser catalog、App Server protocol、Desktop Browser contracts、Electron release boundary 均通过。 |
| Desktop build/package | **通过** | `npm run build --workspace @magi/desktop` 与 `npm run package --workspace @magi/desktop -- --dir` 通过；Vite 仅有既存 `@__PURE__`/大 chunk 警告。 |
| Browser/性能/账本静态 golden | **通过** | `test:browser-core`、`test:browser-download-lifecycle`、renderer comparison、performance envelope、context compaction、permission ledger、trajectory ledger 均通过。 |

### `magi-api` 失败清单

| 测试 | panic 行与实际信息 |
|---|---|
| `git_tool_runtime::tests::agent_apply_brings_finished_agent_changes_into_main_worktree_once` | `crates/magi-api/src/git_tool_runtime.rs:1058:9`；`应用后删除代理分支`。 |
| `routes::mcp_skills_repos::tests::install_skill_rejects_wrapped_requests` | `crates/magi-api/src/routes/mcp_skills_repos.rs:3521:13`；返回 `{"error_code":"INPUT_INVALID","message":"skillId 不能为空"}`。 |
| `routes::sessions::tests::a_skill_picked_for_a_plain_chat_turn_reaches_the_turn_instead_of_being_dropped` | `crates/magi-api/src/routes/sessions.rs:6259:9`；left `String("execute")`，right `"chat"`。 |
| `routes::sessions::tests::new_goal_mode_session_requires_goal_creation_before_plan` | `crates/magi-api/src/routes/sessions.rs:9098:9`；left `get_goal/create_goal/update_plan`，right 还期待 `shell_exec`。 |
| `routes::sessions::tests::session_interrupt_starts_next_queued_turn_and_preserves_fifo_tail` | `crates/magi-api/src/routes/sessions.rs:8031:9`；`普通 Conversation Turn 不应创建 TaskStore root task`。 |
| `routes::settings::tests::settings_bootstrap_exposes_mcp_env_values_for_editing` | `crates/magi-api/src/routes/settings.rs:4683:13`；`settings bootstrap must not expose stale MCP scope field workspaceId`。 |
| `sse::tests::event_workspace_filter_uses_payload_workspace_when_context_is_missing` | `crates/magi-api/src/sse.rs:684:9`；workspace payload 过滤断言为 false。 |
| `turn_harness::tests::{duplicate_request_replays_and_fingerprint_conflict_is_rejected,empty_provider_response_becomes_a_single_failed_terminal_turn,provider_failure_becomes_a_single_failed_terminal_turn,reconnect_snapshot_replays_persisted_turn_events_after_receiver_drop,sse_reconnect_replays_canonical_turn_snapshot_over_real_router_body,steer_routes_to_the_active_turn_and_keeps_the_same_turn_identity,timeout_provider_failure_retries_before_first_delta_and_completes,transient_provider_failure_retries_before_first_delta_and_completes,websocket_reconnect_replays_canonical_turn_over_real_app_server}` | 各测试分别在 `turn_harness.rs:2421:14, 12557:14, 11928:14, 11351:14, 11398:14, 11713:14, 12190:14, 12163:14, 11486:14`；共同错误 `InternalAssemblyError("构建任务派发运行时: task_store 未配置")`。 |
| `turn_harness::tests::read_only_profile_rejects_explicit_apply_patch_without_side_effect` | `turn_harness.rs:1755:9`；left `1`，right `0`。 |
| `turn_harness::tests::read_only_profile_rejects_explicit_file_write_without_approval_or_side_effect` | `turn_harness.rs:4012:9`；left `1`，right `0`。 |
| `turn_harness::tests::read_only_profile_rejects_explicit_file_copy_and_move_without_side_effect` | `turn_harness.rs:4124:9`；left `1`，right `0`。 |
| `turn_harness::tests::read_only_profile_rejects_explicit_file_patch_mkdir_and_remove_without_side_effect` | `turn_harness.rs:1755:9`；left `1`，right `0`。 |
| `turn_harness::tests::read_only_profile_rejects_explicit_image_and_git_writes` | `turn_harness.rs:1755:9`；left `1`，right `0`。 |
| `turn_harness::tests::task_profile_spawns_multiple_children_and_waits_for_all_results` | `turn_harness.rs:11266:9`；left `Failed`，right `Completed`。 |

### `magi-daemon` 与 `magi-process` 失败

| package / 测试 | panic 行与实际信息 |
|---|---|
| `magi-daemon::runtime::tests::router_regular_session_turn_uses_daemon_session_turn_dispatcher` | `crates/magi-daemon/src/daemon/runtime.rs:4301:9`；left `String("execute")`，right `"chat"`。 |
| `magi-daemon::tests::conversation_turn_replays_after_daemon_restart_without_task_or_duplicate_acceptance` | `crates/magi-daemon/src/daemon/tests.rs:3008:5`；left `String("task")`，right `"conversation"`。 |
| `magi-daemon::tests::daemon_http_server_restart_replays_conversation_turn_without_task_or_duplicate_acceptance` | `crates/magi-daemon/src/daemon/tests.rs:3316:5`；left `String("task")`，right `"conversation"`。 |
| `magi-daemon::tests::orchestrator_settings_save_stays_global_when_session_scope_is_supplied` | `crates/magi-daemon/src/daemon/tests.rs:3633:5`；HTTP 400，`模型配置不支持字段 config`，断言期望 200。 |
| `magi-daemon::tests::session_turn_live_events_reach_multiple_subscribers` | `crates/magi-daemon/src/daemon/tests.rs:196:21`；等待首个 `session.turn.conversation.accepted` 超时。 |
| `magi-process::tests::production_code_uses_the_shared_process_factory` | `crates/magi-process/src/lib.rs:1428:9`；仍检测到直接 `std::process::Command::new`：`crates/magi-api/src/tunnel_client_install.rs:283`、`crates/magi-web-model/src/tunnel.rs:483,559,694`。 |

### 真实模型与业务链路

本机 `/Users/xie/.magi/settings.json` 在测试前后保持原配置：主模型 `cline-pass/deepseek-v4.1-flash`（OpenAI Responses，`http://localhost:8317/v1`），生图配置为 `gpt-6-luna`（OpenAI Chat，`http://localhost:8317`）。

- 主模型连接测试：**通过**，`POST /api/settings/orchestrator/test` 返回 HTTP 200 `连接测试成功`。
- 模型列表：**通过**，`POST /api/settings/models/fetch` 返回 HTTP 200，返回 42 个模型。
- 真实普通对话：**通过**，真实 daemon 使用已配置主模型完成一轮；日志中出现 `provider_first_delta`，约 1.48s 后 `canonical_terminal_published`，见 `/tmp/magi-real-app-test.log`。
- 生图连接测试：当前配置 `gpt-6-luna` 返回 **HTTP 400**：`图片生成服务暂不可用`。临时用同一服务的图像模型 `gpt-image-1.5` 测试返回 **HTTP 200**，`mediaType=image/png`、`bytes=721964`；随后已恢复 `gpt-6-luna`，没有保留配置变更。结论是当前生图失败由所选模型不是可用图像生成模型导致，不是连接地址或鉴权失败。
- 真实 Provider 性能采样（5 个场景各 1 次）：4 个完成；`personal_short_history` 失败，证据 `/tmp/magi-real-provider-performance-20261006.json`，daemon 日志 `/tmp/magi-real-provider-performance-20261006.json.daemon.log`。失败原因是第二轮复用已有会话时传入模型覆盖，daemon 返回 `InvalidInput("已有会话的模型只能通过会话模型设置操作修改")`，没有发起第二次 Provider 请求。

### 真实 Electron 验收

- `npm run test:electron-conversation-dom`：**失败**。真实 App Renderer 和 daemon 启动成功，首轮真实 Provider Turn 完成；随后在 `scripts/verify-electron-conversation-dom.mjs:2503` 注册 workspace 时，`scripts/verify-electron-conversation-dom.mjs:1101` 抛出 `Error: workspace new session button missing`。这是最小复现：执行该命令即可复现。
- `npm run test:electron-browser-permission-matrix`：**失败**。真实 Browser Tab 导航、snapshot/click/evaluate 链路均启动；在 `scripts/verify-electron-browser-permission-matrix.mjs:964` 的断言中，实际 Turn receipt `executionProfile="task"`，测试期待 `"conversation"`，随后脚本退出。该断言与当前普通消息统一进入 task 主线的行为不一致。
- Desktop 单测已覆盖 P1-12 后台保活、P3-6 不改布局、用户接管光标/控制租约、daemon 启动失败窗口；对应 128 个测试全部通过。但两个真实 Electron harness 仍有上述失败，因此不能把真实 UI 全部判为通过。

### 需要跟进的问题（未修改）

1. `magi-api` 的 22 个失败，集中在 `task_store 未配置` 装配、ReadOnly 仍额外进入一次 Provider、普通消息路由契约、MCP scope 字段、SSE workspace 过滤、agent_apply 分支清理和多子代理终态。
2. `magi-daemon` 的 5 个失败与普通 Turn 的 `execute/task` 路由语义、模型配置 DTO、实时事件订阅有关。
3. `magi-process` 的共享进程工厂静态扫描仍发现 4 处直接创建子进程。
4. 真实 Electron DOM harness 的 workspace 新会话按钮选择器/页面状态不匹配。
5. 生图配置当前选择了文本模型 `gpt-6-luna`；更换为服务提供的图像模型后连接测试通过。
6. 真实 Provider 性能脚本的 `personal_short_history` 场景需要通过会话模型设置 API 复用历史会话，不能在已有会话请求中直接传 `orchestratorSessionConfig`。

### 验收结束状态

- 业务代码没有修改，也没有创建提交。
- `git status --short --branch`：`main...origin/main`，仅有本报告未跟踪文件。

---

## 2026-10-06 修复后复验

### 代码与自动化

当前 `HEAD` 仍为 `8a3b44b37af37865b0ca540549e71611c3e81448`；`git pull --ff-only origin main` 返回 `Already up to date`。本轮在现有工程目录中保留并验证工作区修复，没有提交。

| 验证 | 结果 | 证据 |
|---|---|---|
| `cargo test --workspace --no-fail-fast` | **通过**：合计 `2856 passed; 0 failed; 3 ignored` | `/tmp/magi-cargo-workspace-fixed-20261006.log` |
| `cargo test -p magi-api --no-fail-fast` | **通过**：`787 passed; 0 failed; 2 ignored` | 本轮终端输出；完整 workspace 日志亦包含此结果 |
| `cargo test -p magi-daemon --no-fail-fast` | **通过**：`121 passed; 0 failed` | 本轮终端输出 |
| `cargo test -p magi-process -- --nocapture` | **通过**：`13 passed; 0 failed` | 本轮终端输出 |
| `npm run check` | **通过**：Desktop、Browser Automation Worker、Web；Svelte `0 errors, 0 warnings` | 本轮命令退出码 `0` |
| `npm test` | **通过**：Desktop、Worker、Web 全部测试命令退出码 `0` | 本轮命令退出码 `0` |
| Electron Desktop build/package | **通过**：Magi `3.0.51`、Electron `43.4.0`、Chromium `150.0.7871.224`；解包自检通过 | `/Users/xie/code/magi-rust-rewrite/target/electron-dist/mac-arm64/Magi.app` |
| Release guard、Browser core/download lifecycle、Renderer comparison、性能/压缩/权限/trajectory golden | **全部通过** | 本轮各命令退出码 `0` |

之前记录的 `magi-api` 22 项、`magi-daemon` 5 项及 `magi-process` 1 项失败，在当前工作区修复后均由全 workspace 测试覆盖并通过；全量日志没有失败测试或 panic。

### 真实 Electron 与 Provider

| 场景 | 结果 | 关键证据 |
|---|---|---|
| Electron 对话与 DOM 全链路 | **通过**：184 项断言通过；正常退出，daemon shutdown settlement 收口 1 个活动 Turn；运行期间源码和包指纹稳定 | `/tmp/magi-electron-conversation-dom-fixed-20261006.json`、`.log` |
| Electron Browser 权限矩阵 | **通过**：99 项断言通过，真实 Chromium guest；ReadOnly、Restricted、FullAccess、Workspace 及跨 Session 范围检查完成；Provider 请求 5 次 | `/tmp/magi-electron-browser-permission-matrix-fixed-20261006.json`、`.log` |
| 真实 Provider 性能/正确性场景 | **通过**：6 个场景各 1 次，全部 completed；EventBus 序号单调；源码指纹采样前后相同 | `/tmp/magi-real-provider-performance-fixed2-20261006.json`、`.log` |

先前失败的 `personal_short_history` 已完成。性能脚本现在仅在新建会话时设置模型；复用会话时不再重复覆盖不可变的会话模型。此轮六个场景为 `new_personal_chat`、`personal_short_history`、`personal_long_history`、`workspace_chat`、`workspace_tool`、`subagent_concurrency`。

### 仍需区分的用户配置项

原配置中的生图模型 `gpt-6-luna` 不是该 Provider 当前可用的图像生成模型；之前用它测试仍返回 HTTP 400。已用同一 Provider 的 `gpt-image-1.5` 实测生图成功，返回 PNG（721,964 bytes）。本轮没有更改用户的 `~/.magi/settings.json`；若要让当前设置页的生图测试也通过，需要把生图配置选择为 Provider 实际提供的图像生成模型。此项属于现有模型选择不匹配，不是 Rust、Web 或 Electron 回归。

### 最终状态

- Rust workspace、Desktop/Worker/Web 检查和测试、Release/Browser golden、真实 Electron DOM、真实 Chromium 权限矩阵及真实 Provider 六场景均通过。
- 用户的生图模型选择仍需改为可用图像模型；未擅自改写用户配置。
- 当前工作区仍有本轮修复代码和本验收报告的未提交改动；没有创建提交。

### 最终包复验（格式修正后）

为确保最后一次源码格式修正也进入实际发行物，已重新构建 `Magi.app`，并在该包上再次运行真实验收：

- Browser 权限矩阵：`99` 项断言通过，`status=passed`，证据 `/tmp/magi-electron-browser-permission-matrix-final-20261006.json`。
- Electron 对话/DOM：`184` 项断言通过，`status=passed`，证据 `/tmp/magi-electron-conversation-dom-final-20261006.json`。
- Provider 六场景：`status=passed`，六个场景均 `completed`，证据 `/tmp/magi-real-provider-performance-final-20261006.json`。
- 最后一次 Rust workspace 复跑：`2856 passed; 0 failed; 3 ignored`，日志 `/tmp/magi-cargo-workspace-final-20261006.log`。

修改过的 Rust 文件已通过定向 `rustfmt --check` 与 `git diff --check`。仓库级 `cargo fmt --all -- --check` 仍会报告未修改文件的既有格式差异，因此没有对无关文件做全仓格式化。

### 2026-10-06 后续稳定性复验

在用户要求继续检查后，重新运行全 workspace 测试时首次发现一个测试时序问题：`turn_harness::tests::reconnect_snapshot_replays_persisted_turn_events_after_receiver_drop` 在 `crates/magi-api/src/turn_harness.rs:11395` 读取快照时尚未看到 completed canonical event。`wait_for_terminal` 观察到 SessionStore 终态的时刻早于终态事件发布；终态发布本来就在 durable 状态提交之后，因此这是该测试过早取 EventBus 快照的竞态，不是 Provider/daemon 运行失败。

测试现在会在最多 2 秒内等待 completed canonical event 发布，然后再构造重连快照；没有更改生产业务路径。该用例单独连续运行 3 次通过，随后全量复验通过：

| 验证 | 结果 | 证据 |
|---|---|---|
| `cargo test --workspace --no-fail-fast` | **通过**：75 个测试目标，`2856 passed; 0 failed; 3 ignored` | `/tmp/magi-cargo-workspace-stable-20261006.log` |
| 重连快照用例单测 | **通过**：连续 3 次，每次 `1 passed; 0 failed` | 终端复验输出 |
| `npm run check` | **通过**：Desktop、Browser Automation Worker、Web；Svelte 0 errors / 0 warnings | 命令退出码 0 |
| `npm test` | **通过**：Desktop、Worker、Web 测试链退出码 0 | 命令退出码 0 |
| `npm run release:guard` | **通过**：Browser catalog、App Server protocol、Desktop Browser contracts、Electron release boundary | 命令退出码 0 |
| Rust 定向格式检查与 `git diff --check` | **通过** | 命令退出码 0 |

该阶段没有操作当时已有的 Electron 实例；随后“最终包与当前配置复验”专门退出旧实例并启动重新打包的版本，真实 Electron DOM、Chromium 权限矩阵、Provider 六场景均在新包上完成。

本轮唯一新增的代码差异是上述测试时序等待；没有创建提交。工作区已有的其他未提交修复继续保留。

### 2026-10-06 最终包与当前配置复验

为确认最新工作区重新进入发行物，重新执行了 Desktop build/package，并使用生成的 `Magi.app` 做真实运行验收：

| 验证 | 结果 | 证据 |
|---|---|---|
| Desktop build/package | **通过**：Magi `3.0.51`，Electron `43.4.0`，Chromium `150.0.7871.224`，解包自检通过 | `target/electron-dist/mac-arm64/Magi.app` |
| Electron 对话/DOM | **通过**：`status=passed`，证据 `/tmp/magi-electron-conversation-dom-continuation-20261006.json`；完整日志 `/tmp/magi-electron-conversation-dom-continuation-20261006.log` | 184 项断言通过 |
| Electron Browser 权限矩阵 | **通过**：`status=passed`，99/99 项断言通过，源码与发行物指纹稳定 | `/tmp/magi-electron-browser-permission-matrix-continuation-20261006.json` |
| 真实 Provider 六场景 | **通过**：`new_personal_chat`、`personal_short_history`、`personal_long_history`、`workspace_chat`、`workspace_tool`、`subagent_concurrency` 均 `completed`，EventBus 序号单调 | `/tmp/magi-real-provider-performance-continuation-20261006.json`；日志 `/tmp/magi-real-provider-performance-continuation-20261006.json.daemon.log` |
| 当前生图模型连接测试 | **通过**：HTTP 200，`图片生成测试成功`，`mediaType=image/png`，本次 739510 bytes | 当前 `/Users/xie/.magi/settings.json` 的 `imageGeneration.model=gpt-image-1.5`，请求 `POST /api/settings/image-generation/test` |
| Browser / 性能 / 压缩 / 权限 / trajectory golden | **全部通过** | 相关 `npm run test:*` 命令退出码均为 0 |

为用新包完成真实验证，本节期间退出了原 Desktop 实例并启动了重新打包的版本。当前发行版保持可测试状态，`GET http://127.0.0.1:38123/health` 返回 `status=ok`、`buildIdentity=8a3b44b37af37865b0ca540549e71611c3e81448`。本轮没有改写用户配置、没有新增提交。

### 2026-10-07 用户消息气泡空白行

- 现象：发送后用户消息气泡末尾多出一个空白行。
- 定位到两个会产生空白的地方：隐藏的 `.user-time` 仍保留 22px 高度和上边距；`.user-plain-content` 的 `pre-wrap` 会把 Markdown Renderer 后的结构空白显示出来。发送编辑器也可能带上末尾空块。
- 处理：消息时间/操作行隐藏时不占垂直布局，悬停或聚焦时仍展开；用户 Markdown 容器使用普通空白折叠，由 Markdown Renderer 负责段落和换行；桥接入口去掉提交文本的末尾空白，让 optimistic 气泡和服务端 canonical 内容一致。

| 验证 | 结果 | 证据 |
|---|---|---|
| `npm --prefix web run check` | **通过**：0 errors、0 warnings | 退出码 0 |
| `npm --prefix web run test:bridge` | **通过**：覆盖 contenteditable 末尾空白，本地消息与服务端请求内容一致 | `web client bridge golden replay passed` |
| `npm --prefix web test` | **通过**：Web 全部 golden/test 子命令完成 | 退出码 0 |
| `npm run test --workspace @magi/desktop` | **通过**：128/128 | Node TAP 汇总 `pass 128, fail 0` |
| `npm run package --workspace @magi/desktop -- --dir` | **通过**：Magi 3.0.51，Electron 43.4.0 / Chromium 150.0.7871.224，解包自检通过 | `target/electron-dist/mac-arm64/Magi.app` |
| 打包 Electron 对话回归 | **通过**：真实 Desktop `personal_chat` 场景 16/16 断言通过；空闲 footer 高度 0，末尾空白未显示，悬停 footer 高 22px 且可见；Provider 1 次请求，Turn 正常完成 | `/tmp/magi-electron-user-bubble-20261007-final.json` |
| 当前本地 Desktop | **运行正常**：health 为 `status=ok`，`buildIdentity=8a3b44b37af37865b0ca540549e71611c3e81448` | `GET http://127.0.0.1:38123/health` |

Electron 回归使用隔离的测试状态根和本地 Provider；未改写 `~/.magi` 配置。重新打包的 Magi 当前已启动，未创建提交。构建输出仍有既有的 Rollup chunk 大小和纯注释提示。

### 2026-10-07 GPT Web 的 Magi 项目工具调用

#### 现场证据与原因

- Magi 设置页显示 GPT Web 已登录、OpenAI Tunnel 正在运行；Tunnel 日志显示 MCP stdio 中继启动，且当前 `magi.fs.read` 请求已转发到本机 MCP 服务。
- 真实审计记录 `/Users/xie/.magi/mcp-audit.jsonl` 的结果是 `clientName=GPT Web`、`tool=magi.fs.read`、`outcome=denied`，原因是“令牌绑定的工作区不可用”。审计中的 `workspaceId` 是该会话的 `session-…` ID。
- 当前失败会话 URL 为 `scope=personal`，没有 `workspaceId`。尽管已有活动工作区，个人会话并未绑定该工作区；旧的 Web client 解析逻辑却把 session ID 作为 `project_id` 传给 MCP，工作区根目录查找因此失败。

结论：本次失败发生在工具的工作区归属解析，不是 GPT Web 登录或 Tunnel 未配置。不要把侧栏当前活动工作区隐式套给个人会话；Magi 工具必须使用会话明确绑定的工作区，避免在错误项目里读写文件。

#### 修复

- Web client 只从会话的真实 `workspace_id` 取得工具范围。个人会话不再伪造工作区 ID。
- GPT Web 槽位在没有工作区归属时明确拒绝项目工具调用，并提示用户切到工作区会话。
- 个人 GPT Web 会话保留纯聊天；模型选择器标示“无工具”，输入区常驻提示从左侧工作区新建会话后再选择 GPT Web。

#### 验证

| 验证 | 结果 | 证据 |
|---|---|---|
| GPT Web scope helper 单测 | **通过**：1/1 | `cargo test -p magi-conversation-runtime gpt_web_tool_scope_uses_only_the_session_workspace -- --nocapture` |
| GPT Web 槽位 MCP 回归 | **通过**：9/9 | `cargo test -p magi-api web_slot_mcp::tests -- --nocapture`，覆盖个人会话拒绝、活动工作区归属、工具调用写回与审批 |
| Web 模型状态与提示 golden | **通过** | `npm --prefix web run test:web-model` |
| Web 检查与测试 | **通过**：Svelte 0 errors、0 warnings；全部 Web golden 测试通过 | `npm --prefix web run check`、`npm --prefix web test` |
| Desktop 检查与测试 | **通过**：128/128 | `npm run check --workspace @magi/desktop`、`npm run test --workspace @magi/desktop` |
| Desktop 本地目录包 | **构建通过** | `npm run package --workspace @magi/desktop -- --dir`，产物为 `target/electron-dist/mac-arm64/Magi.app` |

#### 真实桌面复验

真实验收时，本地 Electron/daemon 实例 `http://127.0.0.1:38123` 已运行包含这次 GPT Web 修复的构建，健康检查返回 `status=ok`。在 GPT Web 绑定到 Magi 工作区 `workspace-1783913762044-0` 的真实桌面会话中请求 `magi.changes.list`，界面返回 `0`；审计 `/Users/xie/.magi/mcp-audit.jsonl` 记录 `clientName=GPT Web`、`tool=magi.changes.list`、`workspaceId=workspace-1783913762044-0`、`outcome=succeeded`。随后又重新生成了最新本地目录包。这验证了 GPT Web → OpenAI Tunnel → 本地 MCP → 工作区解析 → Magi 内置工具的真实调用链。

同一会话里对 `README.md` 的读请求失败是因为该工作区根目录 `/Users/xie/code/magi` 没有这个文件；对 `.magi/workspace-identity.json` 的请求被路径策略拒绝（`该位置不允许访问`）。两者的审计记录都带正确的 `workspaceId`，与最初把 `session-…` 当工作区 ID 的错误不同。成功验证使用只读的 `magi.changes.list`，未读取或修改工作区文件。

| 验证 | 结果 | 证据 |
|---|---|---|
| GPT Web 修复后真实工具调用 | **通过**：返回 `0` 项待处理变更 | `/Users/xie/.magi/mcp-audit.jsonl` 最新 `magi.changes.list` 记录；Electron Accessibility UI 的第 4 轮显示 `0` |
| MCP 工作区归属 | **通过**：使用真实 `workspace-1783913762044-0`，工具执行成功 | 审计 `workspaceId` 与当前桌面 URL 的 `workspaceId` 一致 |
| 文件读取边界 | **符合预期**：不存在的 README 报文件缺失；`.magi` 元数据读取被策略拒绝 | 同一审计文件的 `magi.fs.read` 记录；UI 显示“README.md 不存在”及“该位置不允许访问” |
| Desktop 目录包重建 | **通过**：Magi 3.0.51、Electron 43.4.0 / Chromium 150.0.7871.224，解包自检通过 | `npm run package --workspace @magi/desktop -- --dir`，产物 `target/electron-dist/mac-arm64/Magi.app` |

本轮未停止或释放用户的 GPT Web 会话；真实验证使用现有「读取 README 首行」会话，并在历史中保留了三条只读测试轮次及审计记录。没有修改工作区文件，也没有创建提交。

#### 2026-10-07 Desktop 复验：登录、Tunnel 与工具调用

用户要求再次用 Desktop 确认 GPT Web 账号及内置工具配置。浏览器设置页显示：GPT Web「已登录，可用」、账号等级 Plus；项目工具通道「OpenAI Tunnel 通道已就绪」且通道状态「运行中」；Tunnel ID 与被遮罩的运行时 API 密钥均已配置。Magi MCP 审计确认 `magi.changes.list` 通过 `web-slot` 以 GPT Web 身份、绑定工作区 `workspace-1783913762044-0` 调用成功。随后在桌面当前 GPT Web 工作区会话再次发起相同只读检查，UI 返回 `0`，审计再次记录 `outcome=succeeded`。因此当前实际账号已启用可用的 Magi 连接器，工具经 Tunnel 到达 daemon 且工作区归属正确。

「检查 ChatGPT 连接器」按钮需要 GPT Web 页面槽位空闲，并会导航共享 WebView；本次没有为执行这项状态查询而停止/释放用户当前会话。已完成的真实 `magi.changes.list` 调用直接验证了 ChatGPT connector → Tunnel → Magi MCP → 工具运行时链路。权限设置保持用户当前值：工具档为「编辑：写入前需要你确认」，授权方式为「始终授权」（创建/修改类操作自动执行；删除、推送、合并等仍逐次确认），本次没有改写这些安全偏好。

| 检查 | 结果 | 证据 |
|---|---|---|
| GPT Web 账号登录 | **通过**：已登录可用，Plus | Desktop 设置 → 浏览器 → GPT Web |
| Tunnel 与运行时密钥 | **通过**：Tunnel 已就绪、运行中；密钥保持遮罩 | Desktop 设置 → GPT Web「项目工具（高级）」 |
| ChatGPT 连接器实际调用 | **通过**：再次返回 `0` 项待处理变更 | GPT Web 会话第 5 轮；`/Users/xie/.magi/mcp-audit.jsonl` 最新记录 `tool=magi.changes.list`、`workspaceId=workspace-1783913762044-0`、`outcome=succeeded` |
| daemon 可用性 | **通过**：`status=ok` | `GET http://127.0.0.1:38123/health` |

当前 Desktop 会话和 GPT Web 登录态保持原样，没有停止/释放槽位，也没有读取、写入或回退项目文件；没有提交代码。

#### 2026-10-07 GPT Web 槽位释放后真实桌面复测

按用户要求先释放 GPT Web 页面槽位，再重新做真实 Desktop 验收。使用当前本地构建 `Magi.app`（版本 3.0.51，daemon `buildIdentity=8a3b44b37af37865b0ca540549e71611c3e81448`）；`GET http://127.0.0.1:38123/health` 返回 `status=ok`。设置页复核 GPT Web 为“已登录，可用”、账号 Plus；OpenAI Tunnel 通道“已就绪 / 运行中”，运行时密钥仍为遮罩状态，权限档和授权偏好未更改。

真实测试步骤与结果：

1. 在设置 → 浏览器 → GPT Web 点“停止并释放”。页面随即显示“当前没有会话占用 GPT Web”，停止按钮禁用；登录状态仍是“已登录，可用”。
2. 新建当前工作区 GPT Web 临时会话，发送只读请求：调用 `magi.changes.list`，只返回待处理变更数量。
3. 会话 UI 返回 `0`。Canonical turn 事件将 `magi.changes.list` 项从 `running` 更新为 `completed`，结果为 `{"changes":[],"total":0,"truncated":false}`。
4. `/Users/xie/.magi/mcp-audit.jsonl` 对应记录：`atMs=1791335432234`、`clientName=GPT Web`、`tool=magi.changes.list`、`workspaceId=workspace-1783913762044-0`、`outcome=succeeded`、`requiresApproval=false`、`paths=[]`。会话事件证据：`/Users/xie/.magi/session-events/session-1791335424442-1/00000000000000000005-00000000000000000005.json`。这验证 GPT Web → Tunnel/MCP → Magi 工具运行时 → 工作区归属的只读实际调用。
5. 再次点“停止并释放”，页面恢复“当前没有会话占用 GPT Web”；随后在空闲槽位执行“检查 ChatGPT 连接器”，操作完成，未因槽位占用被阻止。

连接器检查显示“已启用（0 个工具）”，与上述成功的内置工具调用不一致。源码中状态展示使用 `toolCount ?? 0`；页面未识别到工具数量时，空值会被显示成 0。因此本次将其记录为**工具数量状态文案不可靠/无法据此判断工具未配置**，而不是连接器不可用。无需重新创建或扩大账号连接器权限：GPT Web 已登录、Tunnel 正常，真实工具调用成功。没有改动账号连接器、安全偏好或工作区文件。

本次实际测试所选工作区为 `magi`，路径 `/Users/xie/code/magi`，workspace ID `workspace-1783913762044-0`；这与本报告所在克隆 `/Users/xie/code/magi-rust-rewrite` 不同，故结果证明的是当前 Desktop 活动工作区的调用链，不将其表述为对报告所在克隆的项目文件操作测试。测试结束后 GPT Web 槽位已释放，daemon 健康。

| 检查 | 结果 | 证据 |
|---|---|---|
| GPT Web 登录与账号状态 | **通过**：已登录、Plus | Desktop 设置 → 浏览器 → GPT Web |
| Tunnel 与密钥配置 | **通过**：Tunnel 就绪且运行中；密钥保持遮罩 | Desktop 设置页 |
| 内置项目工具真实调用 | **通过**：`magi.changes.list` 返回 0，审计成功 | 上述 MCP 审计行与 canonical session event |
| 停止并释放 | **通过**：会话占用归零；登录状态保留 | Desktop 设置 → GPT Web |
| 连接器工具数量提示 | **有问题**：状态显示 0，但实际工具调用成功；空值被 UI 兜底为 0 | `SettingsWebModelSection.svelte` 的 `toolCount ?? 0`；真实调用证据 |
| 仓库改动 / 提交 | **未涉及** | 未修改业务代码；无提交 |
