# 主对话 Git 上下文与安全分支工作流

## 1. 概念边界

| 概念 | 权威实体 | 含义 | 不是 |
|---|---|---|---|
| Conversation branch | conversation/thread 的 parent/fork 关系 | 对话历史分叉 | Git branch |
| Execution branch | `ActiveExecutionBranch` | 一个任务/worker 在执行链中的分支 | Git ref |
| Git branch | `refs/heads/*` / `refs/remotes/*` | 代码版本分支 | conversation/thread |
| Worktree/workspace path | `GitObservation.worktree_path` / `SessionCodeContext.execution_root` | 工具实际读写目录 | branch 名 |

`magi-workspace::WorktreeAllocation` 是历史执行路径登记模型，不会调用
`git worktree add/remove`。真实 Git worktree 生命周期由 `magi-git::GitService` 管理，
两者不能因为名称相似而混用。

## 2. 当前实现

### 2.1 结构化 Git 服务

`crates/magi-git/src/lib.rs` 是 Git 操作的唯一底层实现。它使用参数数组调用系统 Git，
不拼 shell 字符串，提供：

- repository/worktree identity、branch、HEAD、upstream、origin、ahead/behind、dirty/conflict 状态；
- 本地/远程 branch 列表；
- branch 创建（默认创建后切换）和已有 branch 切换；
- merge preview、merge、结构化 conflict paths；
- 本地 branch 安全删除、`-D` 二次确认、远程删除二次确认；
- detached 只读 worktree、临时 branch 可写 worktree、worktree 安全移除；
- repository mutex；
- `expected_branch`、`expected_head`、`expected_worktree_path` CAS 前置条件。

`GitService` 不表达 conversation fork 或执行树。

### 2.2 SessionCodeContext

`SessionCodeContext` 独立记录：

- `session_id`、`workspace_id`；
- `execution_root` 与 `runtime_workspace_roots`；
- 单调递增的 `context_revision`；
- repository root、git common dir、worktree path/git dir；
- `desired_ref`、`base_head` 与实时 `observed_branch`、`observed_head`；
- upstream、dirty summary、lease generation；
- 子代理 worktree 的 task、worker、mode、base HEAD、branch、path、active 状态。

状态持久化到 `session-git-contexts.json`。每轮执行前重新观测 Git；外部终端改变
branch 或 HEAD 后保留原期望基线，返回 drift/409，不静默接受。用户可以通过
`/workspace/vcs/context/accept` 明确接受新基线。

### 2.3 并发模型

存在两层互补保护：

1. `GitService` repository mutex 串行化 refs、HEAD、index 和 worktree 元数据操作；
2. `WorkspaceGitOperationCoordinator` 在 turn/worker 与 Git mutation 之间建立 lease。

同一 repository 的两个主 session 不并行执行；运行中的任何 session 会阻止结构化
branch/merge/delete/worktree mutation。外部终端不服从进程内 lease，因此持锁后的实时
观测与 CAS 仍是最终保护。每轮开始发现外部漂移后拒绝执行。

主 session 使用已注册 workspace 的 live worktree，并通过独占执行 lease 串行化多个
session；这保留用户当前 dirty 文件。若未来要求多个主 session 同时写入，应升级为
session 专属 branch + worktree，不能允许它们并行共享 live worktree。

### 2.4 子代理

子代理从父 `SessionGitContext.base_head` 派生，不读取“当下可能已变化”的全局 workspace：

- `ReadOnly` 子代理：`git worktree add --detach <path> <base_head>`；
- 可写子代理：从同一 `base_head` 创建唯一 `magi/agent/*` branch 与独立 worktree；
- 工具的 `working_directory` 指向代理 worktree；
- ProjectMemory、MissionMetrics 与 session snapshot 仍以主 workspace identity root 归档；
- 子代理模型调用终止后立刻把分配标为 inactive：干净 worktree 自动安全移除，
  writable branch 保留供主对话 merge；dirty/conflict worktree 保留目录和 context，
  绝不使用 `--force` 丢失代理产物；
- 主模型在任何 agent worktree 仍为 active 时不能执行 Git mutation，必须先等待 worker
  结束，避免 merge 尚在变化的 agent branch。

因此并行代理不会切换主 HEAD、互相覆盖文件，diff 可以按 task/branch/path 归属。

## 3. 结构化能力

所有请求只接受已注册 workspace。携带 `sessionId` 时还校验 session/workspace 归属。

| 能力 | 调用入口 | 关键约束 |
|---|---|---|
| `git_status` | `POST /api/workspace/vcs/status` | 返回 observation、session context、drift |
| `git_context_accept` | `POST /api/workspace/vcs/context/accept` | 用户明确接受外部 branch/HEAD |
| `git_branch_list` | `POST /api/workspace/vcs/branches` | 可选本地+远程、worktree 占用信息 |
| `git_branch_create` | `POST /api/workspace/vcs/branch/create` | 默认创建并切换；dirty/active lease 拒绝 |
| `git_branch_switch` | `POST /api/workspace/vcs/branch/switch` | 仅已有 branch；dirty/active lease 拒绝 |
| `git_pull` | 模型内置工具 | 默认按 upstream 执行 fast-forward-only pull |
| `git_push` | 模型内置工具 | 推送当前分支；force-with-lease 必须二次确认 |
| `git_merge_preview` | `POST /api/workspace/vcs/merge/preview` | 目标 SHA、merge base、FF、commit/path 列表 |
| `git_merge` | `POST /api/workspace/vcs/merge` | 必须 `confirm=true`；冲突结构化返回 |
| `git_branch_delete` | `POST /api/workspace/vcs/branch/delete` | 当前/占用 branch 阻止；force/remote 二次确认 |
| `git_worktree_list` | `POST /api/workspace/vcs/worktree/list` | repository 全部 worktree |
| `git_worktree_create` | `POST /api/workspace/vcs/worktree/create` | 只在 Magi 管理目录生成 |
| `git_worktree_remove` | `POST /api/workspace/vcs/worktree/remove` | 非管理路径拒绝；force 二次确认 |

Mutation 推荐始终携带：

```json
{
  "sessionId": "session-...",
  "workspacePath": "/absolute/workspace",
  "expectedContextRevision": 3,
  "expectedBranch": "main",
  "expectedHead": "<40-hex-sha>",
  "expectedWorktreePath": "/absolute/workspace"
}
```

稳定错误种类包括 `dirty_workspace`、`stale_git_context`、
`stale_context_revision`、`workspace_execution_active`、
`workspace_git_lease_conflict`、`merge_conflict`、`branch_in_use`、
`current_branch` 和 `confirmation_required`。

### 3.1 主模型工具面

同一组能力已注册为主模型可见的内建工具，不再要求模型通过 `shell_exec` 拼 Git 命令：

- `git_status`、`git_branch_list`；
- `git_branch_create`、`git_branch_switch`；
- `git_pull`、`git_push`；
- `git_merge_preview`、`git_merge`；
- `git_branch_delete`；
- `git_worktree_list`、`git_worktree_create`、`git_worktree_remove`。

工具运行时只负责路由，实际 Git 命令仍唯一落到 `magi-git::GitService`。模型工具从
`ToolExecutionContext` 获取 session、workspace 与 working directory，不允许模型自行传入
另一个 workspace。Git 工具只对普通主会话和 coordinator 主线可见，sidechain 子代理既
看不到这些工具，执行层也会再次拒绝带 `worker_id` 的 mutation。

外部 UI mutation 在 turn/worker 运行期间仍被拒绝；主模型在当前 turn 内调用 mutation
时必须已经持有该 repository 的 execution lease，因此不会与其他 session 竞态。mutation
成功后更新 session baseline、持久化 context、发布刷新事件并重建代码索引。

工具调用目录与产品统计是两个独立视图：模型目录继续保留 10 个具体 `git_*` 动作，确保
schema、权限和审批能够逐操作表达；设置页和 `tool_catalog` 的内建能力统计按 `category`
去重，所有 Git 动作统一归入一个 `git` 类型。设置页顶层只展示能力类型，具体动作收在该
类型的可展开明细中，避免新增 Git 动作虚增“内建能力种类”。

`git_context_accept` 不暴露给模型：接受外部终端改变后的新基线必须由用户在 UI 明确点击，
避免模型静默吞掉 branch/HEAD 漂移。

### 3.2 Web 操作面

分支入口提供完整管理流程：本地/远程分支列表、创建并切换、切换、merge preview 与确认、
安全删除、`-D` 二次确认、远程删除二次确认，以及 Magi 管理 worktree 的创建、列表和移除。
外部 worktree 只展示不允许移除；当前 worktree、dirty、drift 和运行中 turn 都在 UI 与后端
两层阻止危险操作。模型或 UI 改变 Git context 后，SSE 事件统一触发文件树、变更视图、
分支状态和 worktree 状态刷新。

## 4. 刷新与缓存

成功接受基线或完成 mutation 后：

- 推进 session `context_revision` 和 Git baseline；
- 发布 `workspace.git.context.changed`，payload 指明 file tree、code index、knowledge、
  context cache 刷新域；
- 重新调度 workspace code index；
- Web 端刷新 branch 状态并广播 `magi:workspaceContentChanged`，驱动文件树和变更视图刷新。
- session snapshot 基线按新 branch/HEAD 重建；Magi 本地 snapshot 账本通过 repository-local
  `.git/info/exclude` 忽略 `/.magi/snapshots/`，不修改用户受版本控制的 `.gitignore`，也不会
  让内部运行态把工作区误标为 dirty。

`context_revision + observed_head + worktree_path` 应作为后续上下文缓存键。禁止只按
`workspace_id` 缓存跨 branch 文件内容。

## 5. Codex 对比

Codex 值得借鉴的边界：

- `thread_data.rs` 分开保存 `forked_from_id` / `parent_thread_id`、`cwd`、`git_info`；
- `TurnContext` 把当轮 cwd/权限/环境作为执行快照，子代理从父 TurnContext 派生；
- `cwd` 与 `runtimeWorkspaceRoots` 分开，执行入口与允许访问根不是同一字段；
- thread 创建时采集 SHA/branch/origin 作为审计快照；
- TUI 异步 branch 查询按 cwd 校验，拒绝把旧目录结果写回新会话。

参考代码：

- `/Users/xie/code/codex/codex-rs/app-server-protocol/src/protocol/v2/thread_data.rs`
- `/Users/xie/code/codex/codex-rs/app-server-protocol/src/protocol/v2/thread.rs`
- `/Users/xie/code/codex/codex-rs/core/src/session/turn_context.rs`
- `/Users/xie/code/codex/codex-rs/core/src/tools/handlers/multi_agents_common.rs`
- `/Users/xie/code/codex/codex-rs/rollout/src/recorder.rs`
- `/Users/xie/code/codex/codex-rs/tui/src/chatwidget/status_surfaces.rs`

不能直接照搬的部分：Codex `gitInfo` 主要是快照，不是 branch ownership；Codex 并未
提供完整 branch CRUD/merge/worktree 生命周期，也不能单靠相同 cwd 的继承解决并行写入。
Magi 因此额外实现了 session baseline、CAS、repository mutex、execution/mutation lease 和
真实 agent worktree。

结论：conversation branch 与 Git branch 必须独立建模；`cwd`、runtime roots、Git 元数据
也必须独立。把其中任意两个压成一个 `branch` 字段会重新引入当前设计要消除的歧义。

## 6. 测试证据

- `crates/magi-git/src/lib.rs`：临时仓库覆盖 dirty、stale HEAD、create/switch、merge、
  merge conflict、删除保护、force/remote 确认、worktree、session drift 和 lease 竞态；
- `crates/magi-api/src/routes/workspace_vcs.rs`：HTTP/session 绑定、自动切换、dirty、drift、
  accept baseline、运行 lease；
- `task_execution_dispatcher.rs`：子代理从父 base HEAD 创建独立 worktree；
- `git_tool_runtime.rs`：模型工具绑定 session Git context、execution lease、持久化与刷新；
- `magi-tool-runtime`：完整 `git_*` schema、权限/风险分级和业务执行器委托；
- `crates/magi-git/tests/rg_retry.rs`：对 `MistRipple/rg-retry` 的独立临时克隆执行完整本地
  branch/merge/delete/worktree 流程，基准 clone 保持只读。
