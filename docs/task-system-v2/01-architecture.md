# Task System v2 架构

## 1. 任务分档

按"会话长度 + 是否多代理 + 是否需跨进程"分为三档。同一个 magi 实例可以同时存在三档任务，三档共享 Runtime 层 primitives。

| 档位 | 典型场景 | 时长 | 代理数 | 进程跨度 | 激活层 |
|------|----------|------|--------|----------|--------|
| A    | "帮我改一下这个 bug"<br>"解释一下这段代码" | 1-30 min | 1 | 单进程，单会话 | T1 + T2（9 层） |
| B    | "调研三个候选方案并落一个"<br>"修这个 PR 的全部 review comments" | 30 min - 8 h | 2-10 | 单进程，多代理 | T1 + T2 + T3（14 层） |
| C    | "把这个 Java 后端用 Python 重构"<br>"接管这个仓库做一周的迭代" | 数日 - 数周 | 多代理 + 多 mission | 跨进程，跨重启，跨 context 压缩 | T1 + T2 + T3 + T4（21 层） |

**关键观察**：A 档不需要 Coordinator Prompt、不需要 Charter、不需要 Checkpoint，它只需要 Runtime 层；C 档需要全部。中间档位按需激活。

## 2. 整体架构

四个 Tier，21 个 Layer。每层是一个独立的关注点，不混合。

```
┌────────────────────────────────────────────────────────────────┐
│  Tier 4 — Long Mission（C 档独占）                              │
│  L15 MissionCharter  L16 Plan        L17 Workspace              │
│  L18 KnowledgeGraph  L19 ValidationRunner                       │
│  L20 Checkpoint      L21 HumanCheckpoint                        │
├────────────────────────────────────────────────────────────────┤
│  Tier 3 — Multi-Agent Orchestration（B/C 档）                   │
│  L10 CoordinatorPrompt  L11 TaskPolymorphism                    │
│  L12 SafetyGate         L13 TodoLedger                          │
│  L14 ProjectMemory                                              │
├────────────────────────────────────────────────────────────────┤
│  Tier 2 — Session & I/O（A 档起）                                │
│  L6  ToolRegistry   L7  Permissions                             │
│  L8  Streaming      L9  SessionStore                            │
├────────────────────────────────────────────────────────────────┤
│  Tier 1 — Conversation Runtime（所有档共享）                     │
│  L1  Conversation   L2  Turn        L3  Mailbox                 │
│  L4  AgentRole      L5  SpawnGraph                              │
└────────────────────────────────────────────────────────────────┘
```

### 2.1 Tier 1 — Conversation Runtime

整套系统的运行时核心，参考 codex 的实现，所有档位共享。

**L1 Conversation**
单条对话流。**一次对话 = 一个 Conversation**，不再用"Task = 对话"的混合抽象。Conversation 持有：自己的消息历史、自己的 Turn 迭代器、自己的 Mailbox 入口、自己的 AgentRole、自己的 SpawnGraph 节点 id。Conversation 可以递归 spawn 子 Conversation（多代理本质就是 Conversation 树）。

**L2 Turn**
单次"模型决策周期"：拼 prompt → 调模型 → 流式 tokens → 收集 tool calls → 执行 tools → 决定是否进入下一 Turn。Turn 是 Conversation 内部的迭代单元。**Turn 是 Mailbox 唯一的消费边界**：Mailbox 中的待处理项只能在 Turn 与 Turn 之间被注入到下一个 Turn 的输入，绝不在 Turn 内部插入。

**L3 Mailbox**
每个 Conversation 一个 Mailbox。所有需要"在对话进行中注入信号"的来源（用户键盘输入、子代理回执 `report_agent_job_result`、外部决策应答、调度器引导、定时唤醒）都从 Mailbox 入栈。Mailbox 项包含 `author`（user / agent / system / parent / child）、`kind`（message / decision / interrupt / agent_result / followup）、`trigger_turn: bool`（是否立刻触发下一轮 Turn）。

**L4 AgentRole**
Agent 的"人格 + 工具集 + 系统提示"的配置层。来自 codex 的 TOML 配置思路。AgentRole 决定：用什么 system prompt、允许使用哪些工具、token budget、subagent 是否允许、是否在 sandbox。Role 是配置，不是代码分支——增加新 Role 不需要改运行时。

**L5 SpawnGraph**
Conversation 间父子关系图。每个 Conversation 是一个节点，每条"父代理 spawn 子代理"是一条带状态的边（`Open` / `Closed`）。SpawnGraph 用于：子代理回执路由（找到 parent 的 Mailbox）、级联停止（停 parent 同时停所有 open 子节点）、防爆破（限制最大深度与扇出）。

### 2.2 Tier 2 — Session & I/O

A 档及以上激活。这是单次会话从启动到落库所需的最小基础设施。

**L6 ToolRegistry**
工具注册表：内置工具（Bash / Read / Edit / Write / Grep / Glob...）+ 动态加载的 MCP 工具。Registry 按 AgentRole 过滤可见集合。

**L7 Permissions**
按工具 + 按目录 + 按命令做三维允许/拒绝判定。Permission 模式：default / acceptAll / acceptEdits / plan / bypassPermissions。同一份 PermissionEngine 服务所有 Conversation。

**L8 Streaming**
模型 token / tool event / system signal 的统一流式管道。前端、子代理 mailbox、磁盘日志都从同一份 stream 派生订阅，不再各自拼装。

**L9 SessionStore**
单会话持久化：消息、工具调用、Mailbox 历史项、SpawnGraph 拓扑。落到 `~/.magi/sessions/{conv_id}/`。A 档结束即结束；B/C 档继续被 Tier 3/4 引用。

### 2.3 Tier 3 — Multi-Agent Orchestration

B/C 档激活。所有"多代理协作"的能力集中在此层，**不下沉到 Runtime**——Runtime 只关心 Conversation 怎么跑，不关心谁该 spawn 谁。

**L10 CoordinatorPrompt**
当一个 Conversation 被标记为 `coordinator` 时，注入一段特殊 system prompt（参考 claude-code/src/coordinator/coordinatorMode.ts 的 369 行模板）。**协调动作不写在代码里**，全部转化为模型对 `Agent` / `SendMessage` / `TaskStop` 三个工具的调用。这是 v2 最重要的设计决策之一：协调器是 Prompt-as-Code，不是 Code-as-Coordinator。

**L11 TaskPolymorphism**
"任务"不只是"一个子 Conversation"。参考 claude-code 的 `TaskType` 设计，magi 的 Task 包含 7 个变体：

- `local_agent`：本进程内启动的子 Conversation（最常见）
- `local_bash`：异步 shell 任务（长跑构建、watch、本地服务）
- `local_workflow`：预定义工作流（例如 /commit / /verify）
- `remote_agent`：跨进程/跨机器代理（远程 sandbox）
- `monitor_mcp`：MCP 长连接任务（订阅 GitHub PR 事件等）
- `in_process_teammate`：与主代理共存于同一进程的"队友"
- `dream`：后台反思任务（agent 自己 spawn 自己做整理）

每个变体只暴露同一个 `Task` 接口（`kill(task_id)`，外加 5 态 `TaskStatus`：pending / running / completed / failed / killed）。这给 Coordinator 一个统一的协调语言，不论目标是 shell 还是子代理。

**L12 SafetyGate**
B/C 档的高危操作拦截层。例如：`git push --force`、`rm -rf`、跨 mission 删除、外部 API 发送。Gate 不是 Permission（Permission 是工具白名单），Gate 是**语义判定**——比如同样调用 `bash`，但目标是 `git push --force-with-lease` 才需要 Gate 弹窗。

**L13 TodoLedger**
session 内的 todo list（参考 claude-code 的 TodoWrite）。Coordinator 在长任务中把"分解 + 进度"写到 ledger，模型在每次 Turn 开始时自动读到。**不是项目管理工具**，是 in-session anchor。

**L14 ProjectMemory**
跨会话的 memory 文件系统（参考 claude-code 的 4 类 memory：user / feedback / project / reference）。落在 `~/.magi/projects/{slug}/memory/MEMORY.md` + 多个 typed 文件。每次 Conversation 启动自动加载 MEMORY.md 索引。

### 2.4 Tier 4 — Long Mission（C 档独占）

C 档场景的关键洞察：**一次对话装不下一个 Mission**。Java→Python 重构会经历几十个 Conversation、跨数次进程重启、跨数次 context 压缩。所以 C 档不再以 Conversation 为最高对象，而是引入 `Mission`。

**L15 MissionCharter**
Mission 的"宪章"：目标、范围、不做什么、验收标准、关键约束（如"必须保持 API 向后兼容"）、人在何处签字。Charter 在 Mission 创建时一次性写定，**不可被 agent 改写**——agent 想修改 Charter 必须通过 HumanCheckpoint。

**L16 Plan**
Charter 之下的可演化计划：阶段 → 任务 → 子任务的树。Plan 状态可以被 agent 改，但每次重大修改触发 Checkpoint 快照。Plan 节点指向具体 Conversation 或 Task。

**L17 Workspace**
Mission 独占的工作目录。包含源码 worktree、artifacts 目录、log 目录、Mission 自己的 `MEMORY.md`、Checkpoint 快照存档点。Workspace 在 Mission 启动时初始化，结束时归档但不删除。

**L18 KnowledgeGraph**
Mission 进程中累积的"知道了什么"：
- 代码符号索引（哪些类对应哪些 Python 类、哪些接口已经迁移）
- 决策记录（"为什么选 SQLAlchemy 不选 Tortoise"）
- 风险登记（"X 处理逻辑依赖 JVM GC 行为，需特殊处理"）

KG 不是 vector store 本身（那是实现细节），是一组**带版本的事实表**。在 Checkpoint 时整体快照，跨 Conversation 共享。

**L19 ValidationRunner**
独立的验证子系统：测试套件 / 类型检查 / 集成 smoke / 性能基准。Runner 是一个 `Task`（L11），由 Coordinator 调度。每次 Plan 节点完成都触发 Runner，结果写回 Plan 节点。**没有 Runner 通过的 Plan 节点不算完成**——避免"模型说做完了"的认知偏差。

**L20 Checkpoint**
Mission 级别的快照点。每次进程重启、context 压缩、阶段切换都生成 Checkpoint。Checkpoint 包含：Plan 当前状态、KG 快照、Workspace 工作树指针（git commit）、open Conversations 的 mailbox 与 turn 游标。

**Checkpoint 让 Mission 跨进程存活**——这是 C 档与 A/B 档最本质的区别。

**L21 HumanCheckpoint**
预设的人审点。Mission 在 Charter 中声明哪些节点必须人审（"DB schema 改动前"、"破坏性 API 修改前"、"超过 X 个文件改动后"）。命中 HumanCheckpoint 时 Mission 进入 `awaiting_human` 状态，不前进，不丢弃 KG，等待用户决定。

## 3. 任务分档与层激活对照

```
A 档（单代理）：    T1(L1-L5) + T2(L6-L9)
                   9 个 Layer

B 档（多代理）：    T1 + T2 + T3(L10-L14)
                   14 个 Layer

C 档（长 Mission）：T1 + T2 + T3 + T4(L15-L21)
                   21 个 Layer
```

A 档代码路径里**完全看不见** Mission / Charter / KG，因为 Tier 4 在 A 档不被注入。B 档同理看不见 Tier 4。

## 4. 核心数据模型

### 4.1 Tier 1 类型骨架（Rust 伪代码）

```rust
// L1 Conversation
pub struct Conversation {
    pub id: ConversationId,
    pub role: AgentRole,                    // L4
    pub mailbox: MailboxHandle,             // L3
    pub turn_cursor: TurnCursor,            // L2
    pub spawn_node: SpawnNodeId,            // L5
    pub session_ref: SessionRef,            // L9
    pub mission_ref: Option<MissionId>,     // L15-L21 仅 C 档
}

// L2 Turn
pub struct Turn {
    pub id: TurnId,
    pub conversation: ConversationId,
    pub status: TurnStatus,                 // Pending / Modeling / ToolCalling / Done / Failed
    pub pending_input: Vec<MailboxItem>,    // 本轮开始时从 Mailbox 抽取
    pub model_stream: StreamHandle,
}

// L3 Mailbox
pub struct MailboxItem {
    pub id: MailboxItemId,
    pub author: Author,                     // User / Agent(id) / System / Parent(id) / Child(id)
    pub kind: MailboxKind,                  // Message / Decision / Interrupt / AgentResult / Followup
    pub trigger_turn: bool,
    pub payload: MailboxPayload,
    pub enqueued_at: SystemTime,
}

// L4 AgentRole
pub struct AgentRole {
    pub name: RoleName,
    pub system_prompt: String,
    pub allowed_tools: BTreeSet<ToolName>,
    pub token_budget: TokenBudget,
    pub spawn_policy: SpawnPolicy,          // 是否允许 subagent / 最大深度
    pub coordinator_mode: bool,             // 是否注入 CoordinatorPrompt (L10)
}

// L5 SpawnGraph
pub struct SpawnEdge {
    pub parent: ConversationId,
    pub child: ConversationId,
    pub status: SpawnEdgeStatus,            // Open / Closed
    pub task_kind: TaskKind,                // L11 polymorphism
    pub created_at: SystemTime,
    pub closed_at: Option<SystemTime>,
}
```

### 4.2 Tier 3 类型骨架

```rust
// L11 TaskPolymorphism
pub enum TaskKind {
    LocalAgent,
    LocalBash,
    LocalWorkflow,
    RemoteAgent,
    MonitorMcp,
    InProcessTeammate,
    Dream,
}

pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}

pub trait Task: Send {
    fn id(&self) -> TaskId;
    fn kind(&self) -> TaskKind;
    fn status(&self) -> TaskStatus;
    async fn kill(&self) -> Result<()>;
}

// L13 TodoLedger
pub struct TodoEntry {
    pub id: TodoId,
    pub content: String,
    pub active_form: String,                // "Running X" present continuous
    pub status: TodoStatus,                 // pending / in_progress / completed
    pub conversation: ConversationId,
}
```

### 4.3 Tier 4 类型骨架

```rust
// L15 MissionCharter
pub struct MissionCharter {
    pub mission: MissionId,
    pub objective: String,
    pub scope_in: Vec<String>,
    pub scope_out: Vec<String>,
    pub acceptance: Vec<AcceptanceCriterion>,
    pub constraints: Vec<Constraint>,
    pub human_checkpoints: Vec<HumanCheckpointTrigger>,
    pub signed_off_by: Option<UserId>,
    pub frozen: bool,                       // true 后 agent 不可改
}

// L16 Plan
pub struct PlanNode {
    pub id: PlanNodeId,
    pub parent: Option<PlanNodeId>,
    pub title: String,
    pub status: PlanNodeStatus,
    pub conversation_refs: Vec<ConversationId>,
    pub task_refs: Vec<TaskId>,
    pub validation: Option<ValidationStatus>,
}

// L20 Checkpoint
pub struct Checkpoint {
    pub id: CheckpointId,
    pub mission: MissionId,
    pub kind: CheckpointKind,               // ProcessRestart / ContextCompaction / PhaseTransition / Manual
    pub created_at: SystemTime,
    pub plan_snapshot: PlanSnapshotRef,
    pub kg_snapshot: KgSnapshotRef,
    pub workspace_commit: GitCommitSha,
    pub open_conversations: Vec<ConversationCheckpoint>,
}

pub struct ConversationCheckpoint {
    pub id: ConversationId,
    pub turn_cursor: TurnCursor,
    pub mailbox_drain: Vec<MailboxItem>,    // 未处理的 mailbox 项
}
```

## 5. 关键时序

### 5.1 A 档：单代理对话（最常见）

```
User → magi-api: POST /sessions/{id}/messages
  └─ SessionStore.append_user_message()
  └─ Mailbox.enqueue({ author: User, kind: Message, trigger_turn: true })
  └─ Conversation.advance_turn()
       └─ Turn.start()
            ├─ pending_input := Mailbox.drain()
            ├─ Build prompt (system + history + pending_input)
            ├─ Model stream → tokens
            ├─ tool_calls? → ToolRegistry.invoke()
            └─ Decide next: more tools | end turn
       └─ Turn.complete()
            └─ Mailbox.has_trigger_turn()? → advance_turn() | wait
```

### 5.2 B 档：Coordinator spawn 子代理并收回执

```
Coordinator Conversation (Turn N):
  Model emits tool_call: Agent({ subagent_type: "worker", prompt: "..." })
    └─ TaskRegistry.spawn(LocalAgent)
         └─ new Conversation child
         └─ SpawnGraph.add_edge(parent, child, Open)
    └─ return task_id to coordinator's tool result

Child Conversation runs to completion:
  └─ Final message
  └─ TaskNotification {
         task_id, status: Completed, summary, result
     }
  └─ Look up parent via SpawnGraph
  └─ parent.Mailbox.enqueue({
         author: Child(child_id), kind: AgentResult,
         trigger_turn: true, payload: <task-notification>...
     })
  └─ SpawnGraph.close_edge(parent, child)

Coordinator Conversation (Turn N+k):
  └─ pending_input := [<task-notification>...]  // 自动消费
  └─ Model 继续协调
```

### 5.3 C 档：Mission 跨进程恢复

```
Process A 启动 Mission "Java→Python 重构":
  └─ MissionCharter.sign_off(user)
  └─ Plan v0 由模型生成 + user 确认
  └─ Workspace 初始化（git worktree）
  └─ Plan node "迁移 UserService" 启动
       └─ Conversation X spawn 多个 worker Conversation
       └─ KG.write(symbol_map: UserService → app/services/user.py)
       └─ Checkpoint.create(kind: ContextCompaction)  // context 满了
       └─ Process A crash / shutdown

Process B 恢复同一 Mission:
  └─ Load Checkpoint
  └─ KG 恢复（带版本号）
  └─ Plan 恢复，定位到上次 in-progress 节点
  └─ Workspace 指向同一 git commit
  └─ open Conversations 按 ConversationCheckpoint 恢复
       └─ turn_cursor 指向最后未完成的 Turn
       └─ mailbox_drain 重新入队
  └─ Coordinator 继续，模型不知道发生过重启
```

## 6. 不变式与契约

- **Mailbox-Turn 边界**：Mailbox 项只能在 Turn 边界被消费，不能进入 Turn 内部。这是确定性的来源。
- **Conversation 单一所有者**：一个 Conversation 同一时刻只能被一个 Turn 占用，不并发。Conversation 间并发通过多 Conversation 实现。
- **Checkpoint 完整性**：Plan / KG / Workspace / Conversations 必须原子快照，否则恢复时会出现"Plan 说做完了但代码没改完"。
- **HumanCheckpoint 不可绕过**：agent 不能通过任何 prompt 技巧让自己跳过 HumanCheckpoint，命中即阻塞。
- **AgentRole 不可在运行时被自己修改**：Role 修改必须通过外部配置变更或 HumanCheckpoint，防止模型 self-modify 出 jailbreak。

## 7. 性能预算

| 操作 | 目标延迟 |
|------|----------|
| Mailbox.enqueue | < 1 ms |
| Turn.start → 第一个 token | < 500 ms（不含模型 TTFT） |
| Subagent spawn → 子 Conversation 就绪 | < 100 ms |
| Checkpoint.create（C 档典型） | < 2 s |
| Mission resume from Checkpoint | < 5 s |

## 8. 代码行数预期

| 模块 | 当前 | v2 目标 |
|------|------|---------|
| task_llm_loop.rs + task_runner.rs + task_store.rs | ~12000 行 | 拆为 conversation_loop + turn_driver + spawn_graph + mailbox + session_store ≈ 4500 行 |
| 任务编排相关 routes / dispatch | ~3000 行 | ≈ 1500 行 |
| Tier 4（新增） | 0 | ≈ 2500 行 |
| **总计** | ~15000 行 | ~8500 行 |

总体减少约 6500 行（-43%），但获得 C 档完整支持。

## 9. 与 codex / claude-code 的对应关系

| v2 Layer | 来源 | 参考路径 |
|----------|------|----------|
| L1 Conversation | codex | `codex-rs/core/src/codex.rs` 的 Conversation |
| L2 Turn | codex | `codex-rs/core/src/state/turn.rs` |
| L3 Mailbox | codex | `codex-rs/core/src/agent/mailbox.rs` |
| L4 AgentRole | codex | `codex-rs/core/src/agent/role.rs` |
| L5 SpawnGraph | codex | `codex-rs/agent-graph-store/` |
| L10 CoordinatorPrompt | claude-code | `src/coordinator/coordinatorMode.ts` |
| L11 TaskPolymorphism | claude-code | `src/Task.ts` + `src/tasks/types.ts` |
| L13 TodoLedger | claude-code | `src/tools/TodoWriteTool/` |
| L14 ProjectMemory | claude-code | `src/memdir/memoryTypes.ts` |
| L15-L21 Tier 4 | magi 原创 | 综合 codex SpawnGraph 持久化 + claude-code Checkpoint 思路 |
