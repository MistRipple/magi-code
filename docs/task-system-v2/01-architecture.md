# Task System v2 架构

## 1. 系统目标

Task System v2 面向三类工作负载：

- 简单任务：快速问答、解释、一次性工具执行、小范围修改。
- 中等任务：明确目标下的实现、修复、调研、验证，可能需要拆分和多 worker 协作。
- 复杂任务：跨多轮、多阶段、可中断、可恢复、需要长期事实沉淀与人审的 Mission。

v2 的设计目标不是让所有输入都进入任务系统，而是按复杂度逐级增加控制面：

```text
简单任务：SessionTurn
中等任务：ExecutionChain + TaskGraph
复杂任务：Mission + ExecutionChain + TaskGraph + Long-Mission 闸门
```

关键原则：

- 简单任务不能被 Task/Mission 元数据拖慢。
- 中等任务必须有可观察的执行链、可并行推进的代理任务和可回收的同步工具结果。
- 复杂任务必须有 Charter、Plan、Validation、Checkpoint、HumanCheckpoint，不能靠模型口头声明完成。

## 2. 唯一业务模型

v2 只采用以下一套对象关系：

```text
Session
  └─ ExecutionChain?              // 当前任务化执行链，普通 chat/execute 可为空
       └─ TaskGraph               // root task + child tasks
            ├─ Task               // 业务调度节点
            │    ├─ Conversation  // 运行时 Turn/Mailbox 容器
            │    └─ Thread        // task/代理实例持久化消息历史
            └─ SpawnGraph         // TaskId -> TaskId 父子拓扑

Mission?                          // 复杂任务激活
  ├─ MissionCharter
  ├─ Plan
  ├─ MissionWorkspace
  ├─ KnowledgeGraph
  ├─ ValidationReport
  ├─ CheckpointLog
  └─ HumanCheckpointLog
```

对象职责：

| 对象 | 职责 | 不承担 |
|------|------|--------|
| `Session` | 用户交互入口、UI 容器、普通 turn 历史 | 任务拆解和长期验收 |
| `ExecutionChain` | 当前可恢复执行链，连接 UI、root task、代理详情、恢复信息 | 不作为业务工作单元 |
| `Mission` | 复杂目标的长期边界和审计容器 | 不接管简单任务 |
| `Task` | 可调度、可展示、可取消、可验收的业务工作单元 | 不直接保存模型完整对话 |
| `Conversation` | 单个 Task 的 Turn/Mailbox/模型调用边界 | 不作为 SpawnGraph 节点 |
| `Thread` | 单个 task/代理实例的持久化消息历史 | 不混用不同 task 历史 |
| `SpawnGraph` | 用 `TaskId` 表达父子拓扑、回执路由、级联停止 | 不调度任务、不持有模型状态 |

这意味着：`Task` 是业务事实源，`Conversation` 是运行时容器。多代理本质是 TaskGraph 中多个 Task 并发推进，每个 Task 各自拥有 Conversation 和 Thread。

## 3. 简单 / 中等 / 复杂任务推进

### 3.1 简单任务：SessionTurn

适用场景：

- 普通问答、解释、总结。
- 明确的一次性工具调用。
- 小范围修改且不需要任务投影、子任务回执或恢复链。

推进方式：

```text
User input
  └─ classify route = chat / execute
       └─ SessionTurnExecution
            ├─ chat：模型直接回复
            └─ execute：模型调用工具并回写 session timeline
```

约束：

- 不创建 root task。
- 不创建 Mission。
- 不注入 Long-Mission 层。
- 只保留当前 session turn 的必要历史和工具证据。

简单任务的成功标准是低延迟、低摩擦、结果清楚。不要为了统一抽象而强行任务化。

### 3.2 中等任务：ExecutionChain + TaskGraph

适用场景：

- 实现一个明确功能。
- 修复一个 bug 并验证。
- 处理一批 review comments。
- 调研多个路径并落地一个结论。

推进方式：

```text
User input
  └─ classify route = task
       └─ create ExecutionChain
            └─ create root Task(LocalAgent)
                 └─ TaskRunner dispatch
                      ├─ single-worker task：root worker 直接完成
                      └─ coordinated task：主代理通过 agent_spawn 生成 child tasks
                           └─ child terminal outcome -> tool_call_result -> parent same turn
```

中等任务分两种，但仍属于同一设计：

| 形态 | 触发条件 | 推进方式 |
|------|----------|----------|
| Single-worker task | 目标明确、单 worker 可闭环 | root task 直接执行、验证、完成 |
| Coordinated task | 需要拆解、并行、子结果回收 | root task 使用主编排角色，spawn 子 task 并汇总 |

约束：

- `ExecutionChain` 是恢复入口，必须指向 root task。
- `TaskRunner` 只调度 runnable leaf task。
- `SpawnGraph` 使用 `TaskId` 建父子边。
- `agent_spawn` 是同步阻塞工具调用：child task 终态必须作为 `tool_call_result` 回写 parent turn。
- root task 完成必须有输出摘要和必要验证证据。

中等任务的成功标准是：用户能看到当前任务进度、worker 分工、阻塞点、最终验证结果。

### 3.3 复杂任务：Mission

适用场景：

- 多天或多阶段迁移。
- 大规模重构。
- 跨多个执行链持续推进同一目标。
- 涉及高风险操作、长上下文恢复、阶段验收或人审。

推进方式：

```text
User input
  └─ classify route = task + long mission
       └─ create / resume Mission
            ├─ MissionCharter：目标、范围、验收、约束
            ├─ Plan：阶段和步骤
            ├─ ExecutionChain：当前推进链
            ├─ TaskGraph：本轮可调度工作
            ├─ KnowledgeGraph：事实、决策、风险
            ├─ ValidationReport：每个 Plan step 的验证证据
            ├─ CheckpointLog：可恢复快照
            └─ HumanCheckpointLog：人审挂起点
```

复杂任务的硬约束：

- Charter 有生命周期：`draft` 可澄清修改，`frozen` 后修改必须走 HumanCheckpoint。
- Plan step 不能只由模型声明完成，必须绑定 Validation evidence。
- HumanCheckpoint pending 时，runtime 必须阻止新的 dispatch / agent_spawn。
- Checkpoint 必须服务恢复，不只是展示摘要。
- Mission 恢复后必须能定位 active chain、task tree、spawn graph、pending mailbox、thread/turn 游标和 workspace 指针。

复杂任务的成功标准是：即使 context 压缩、进程重启或用户隔天回来，系统仍能解释“目标是什么、做到哪、证据是什么、下一步是什么”。

## 4. 分层架构

四个 Tier 仍然保留，但它们是控制面递增，不是四套系统：

```text
Tier 4 — Long Mission
  MissionCharter / Plan / MissionWorkspace / KnowledgeGraph
  ValidationRunner / Checkpoint / HumanCheckpoint

Tier 3 — Orchestration
  CoordinatorPrompt / TaskPolymorphism / SafetyGate
  TodoLedger / ProjectMemory

Tier 2 — Session & I/O
  ToolRegistry / Permissions / Streaming / SessionStore

Tier 1 — Conversation Runtime
  Conversation / Turn / Mailbox / AgentRole / SpawnGraph
```

激活规则：

| 负载 | 激活控制面 |
|------|------------|
| 简单任务 | Tier 2 的普通 SessionTurn；必要时使用 ToolRegistry / Permissions / Streaming |
| 中等任务 | Tier 1 + Tier 2 + TaskRunner / ExecutionChain；需要协作时激活 Tier 3 |
| 复杂任务 | Tier 1 + Tier 2 + Tier 3 + Tier 4 |

## 5. 核心层定义

### 5.1 Conversation Runtime

**Conversation**

单个 Task 的运行时容器。Conversation 持有 mailbox 和 current turn，保证同一 Task 的模型推进不并发。Conversation 不作为业务拓扑节点，不直接表达父子关系。

**Turn**

单次模型决策周期：构造输入、调用模型、流式输出、收集 tool calls、执行工具、生成结果。Mailbox 只能在 Turn 边界被 drain。

**Mailbox**

运行时输入通道。用户继续输入、父任务消息、系统 followup 都进入 mailbox，在下一次 Turn 边界注入模型上下文。代理 spawn 的终态结果不走 mailbox 旁路，而是作为当前 `agent_spawn` 的 `tool_call_result` 返回。

**AgentRole**

Role 是配置：system prompt、工具可见性、是否 coordinator、默认 task kind 支持。Role 不应该通过运行时代码分支膨胀。

**SpawnGraph**

Task 父子拓扑。节点是 `TaskId`，边代表 parent task spawn child task。SpawnGraph 负责回执路由、级联停止、深度和扇出限制；不负责调度。

### 5.2 Session & I/O

**ToolRegistry**

工具注册与执行入口。任务工具、普通 session 工具、MCP 工具都应通过统一 registry 暴露。

**Permissions**

三维权限：工具、路径、命令。PermissionEngine 输出 allow / deny / needs_approval，调用层必须按 task policy 和 session mode 显式传入模式。

**Streaming**

统一发布模型 token、工具事件、代理 task 状态和任务完成事件。前端 projection 从 stream 派生，不直接拼装运行时私有状态。

**SessionStore**

保存 session、turn、thread、active execution chain、timeline projection 所需的事实。SessionStore 不替代 TaskStore。

### 5.3 Orchestration

**Main Orchestration Prompt**

主编排提示词是 prompt-as-code。模型可以通过 `agent_spawn` 创建一个或多个代理 task 协调工作；runtime 必须限制工具可见性、spawn 深度、人审状态和权限边界。

**TaskPolymorphism**

Task 统一暴露 5 态生命周期：

```rust
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Killed,
}
```

Task 变体用于调度不同执行器：

```rust
pub enum TaskKind {
    LocalAgent,
    LocalWorkflow,
    RemoteAgent,
    MonitorMcp,
    InProcessTeammate,
    Dream,
}
```

实现成熟度必须在代码和文档中标明。当前主线以 `LocalAgent` 为可运行核心；shell 命令不是独立 TaskKind，统一作为 `shell_exec` 工具能力在 LocalAgent / Coordinator 受权限约束调用。其它变体按同一接口逐步补 executor。

**SafetyGate**

SafetyGate 是语义拦截，不是工具白名单。高危命令、破坏性 git 操作、发布操作、跨 mission 删除必须在工具执行前被 runtime 拦截。

**TodoLedger / ProjectMemory**

TodoLedger 是当前 session 内的短期进度锚点。ProjectMemory 是项目维度的长期记忆。二者都不是 Plan 的替代品。

### 5.4 Long Mission

**MissionCharter**

复杂任务的目标契约：目标、范围、非目标、验收标准、关键约束、人审触发点。Charter 生命周期：

```text
draft -> frozen
```

`draft` 阶段允许澄清更新；`frozen` 后只能通过 HumanCheckpoint 修改。

**Plan**

Mission 的阶段和步骤。Plan step 是验收路线，不是普通 todo。Step 状态转换必须受依赖和验证约束。

**MissionWorkspace**

Mission 独占工作目录、artifact、log、memory、checkpoint 指针。复杂任务不得把产物散落到随机临时路径。

**KnowledgeGraph**

Mission 事实表：symbols、decisions、risks。KG 是结构化事实，不是自由文本聊天总结。

**ValidationRunner**

验证记录系统。每个需要交付的 Plan step 必须有对应 validation record；失败记录未消解前不能完成。

**Checkpoint**

恢复快照。Checkpoint 至少要能恢复：

- active execution chain
- task tree / task statuses
- spawn graph open edges
- pending mailbox item counts and payload references
- thread / turn cursor
- plan / KG / validation version
- workspace commit or snapshot pointer

**HumanCheckpoint**

人审挂起点。pending 状态是 runtime 硬阻塞，不是 prompt 建议。

## 6. 关键时序

### 6.1 简单任务

```text
User
  └─ POST /session/turn
       └─ classify: chat / execute
            └─ SessionTurnExecution
                 ├─ build session messages
                 ├─ model stream
                 ├─ execute tools when route=execute
                 └─ write back timeline
```

### 6.2 中等任务

```text
User
  └─ POST /session/turn
       └─ classify: task
            └─ dispatch_submission
                 ├─ ensure Session Mission identity
                 ├─ create root Task(LocalAgent)
                 ├─ create ExecutionChain
                 ├─ create worker Thread
                 └─ TaskRunner.run_cycle(root)
                      ├─ dispatch runnable leaf
                      ├─ Conversation.advance_turn(task)
                      ├─ optional agent_spawn child Task
                      ├─ child terminal outcome -> tool_call_result
                      └─ root completed / failed / killed
```

### 6.3 复杂任务

```text
User
  └─ start / resume long mission
       ├─ load or create Mission
       ├─ ensure Charter and Plan
       ├─ resume latest Checkpoint when present
       ├─ run current ExecutionChain
       ├─ write KG / Validation / Checkpoint during progress
       ├─ stop on HumanCheckpoint pending
       └─ complete only when Plan validation and acceptance pass
```

## 7. 不变式

- **简单任务不任务化**：`chat/execute` 不创建 TaskGraph，除非分类明确为 task。
- **Task 是业务节点**：进度、状态、父子关系、取消、回执、验收都以 Task 为中心。
- **Conversation 单 Task 所有**：一个 task 对应自己的 Conversation runtime，不复用其它 task 的 mailbox。
- **Thread 不串线**：每个 task/代理实例使用独立 thread；历史代理 thread 只做审计，不注入新 task 上下文。
- **Mailbox 只在 Turn 边界消费**：runtime signal 不能插入正在执行的模型 round。
- **Coordinator 不越权**：prompt 可以协调，但所有权限、人审、验证、恢复约束由 runtime enforce。
- **HumanCheckpoint 不可绕过**：pending 即禁止新 dispatch / agent_spawn，直到 operator resolve。
- **Plan 完成必须有证据**：没有 validation evidence 的交付 step 不能进入 completed。
- **Checkpoint 服务恢复**：不能只做展示日志，必须能支持 Mission 继续推进。

## 8. 当前实现对齐要求

当前代码已经采用 `TaskId` 作为 SpawnGraph 节点、task-level Conversation 作为运行容器。文档以这一点为基线。

后续实现优先级：

1. HumanCheckpoint pending 时 runtime 阻止 `agent_spawn` 和新任务派发。
2. Plan step 完成前检查 ValidationStore。
3. MissionCharter 增加 `draft/frozen` 生命周期。
4. Checkpoint 从摘要日志升级为可恢复快照协议。
5. Long Mission 使用显式 `TaskTier::LongMission`，`background_allowed` 只允许作为执行策略字段。
