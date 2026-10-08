# Magi 消息响应核心架构：目标与验收合同

> 本文是目标架构和验收合同的唯一来源：定义职责边界、持久化/事件合同、场景范围及整体完成条件。
> 性能测量见[性能验证](conversation-performance-validation.md)，可执行验证入口见[脚本与验证入口](validation.md)。历史重构进度与验收记录通过 Git 历史追溯。
>
> 本文中的“必须/不得”是规范性约束，不是当前实现声明；是否满足须核对当前源码、测试、构建和本次真实运行证据。

阅读顺序：先看第 2–5 节理解所有权和运行合同，再看第 6 节验证入口，最后用第 7 节判断是否允许关闭。当前状态、artifact 和统计不要写回本文。

## 1. 目标与范围

Magi 必须把“用户的一轮对话”和“工程执行任务”分开建模，同时保持统一的主对话体验。目标链路只有一个接纳入口，但 Turn 的两种执行 profile 保留各自的事实边界：

```text
Client
  -> TurnService
  -> SessionTurnCoordinator（每个 Session 一个）
     ├─ conversation -> Provider -> CanonicalTurnEventSink -> Canonical Turn Log
     └─ task -> TaskRunSupervisor -> TaskStore
                         └─ TaskCompletionNotifier -> Coordinator -> Sink
Canonical Turn Log + TaskStore -> Read Models -> SSE / App Server notification
```

覆盖：主对话、普通 Chat、工具、Goal、子代理、流式输出、取消、重连、重启恢复以及 Web/Desktop 展示。

不引入远程数据库、消息队列、分布式事务、第二套 Task 日志、完整 CQRS 平台或“新路径失败后回退旧路径”的双轨实现。这里的 Canonical Turn Log 是逻辑事实源，优先复用现有本地持久化载体和 crate 边界；现有边界无法满足本合同前，先触发第 7 节的设计复审。

验收优先级固定为：事实唯一性与终态正确性 → 恢复、权限和真实副作用 → 可观察响应速度。性能优化不得削弱前两层；本文件的目标合同也不意味着当前代码已经满足它。

## 2. 领域对象与执行 profile

| 对象 | 含义 | 唯一事实或生命周期所有者 |
| --- | --- | --- |
| `Session` | 用户看到的会话和消息时间线 | Session projection / Coordinator |
| `Turn` | 用户发起的一轮交互 | `SessionTurnCoordinator` |
| `TaskRun` | 一轮工程执行的运行实例 | `TaskRunSupervisor` |
| `Task` | TaskRun 中可调度的工作节点 | `TaskStore` / `TaskScheduler` |
| `AgentRole` | 子代理的能力、模型和提示快照 | `AgentRoleRegistry` |
| `TurnEvent` | Turn 的持久事实和通知载荷 | `CanonicalTurnEventSink` |
| `Projection` | 从事实源生成的读取模型 | 对应 projection 更新器 |

### 2.1 `conversation`

只用于会话命令（如 `/compact`）和 GPT Web 引擎会话。不得创建 `TaskRun`、root Task、lease、Runner、Git execution context 或 task execution snapshot；可以创建只读的 `context_snapshot_id`，用于固定本轮输入和配置。

`ConversationExecutor` 读取冻结的 Conversation History Projection，把历史版本、模型配置、工具目录、技能/MCP、知识上下文和 profile 适用的权限快照固化为 `context_snapshot_id`，然后调用共享 Provider。它不得准备或记录 Git execution context，也不得另写一份模型历史。

### 2.2 `task`

用于文件、代码、Shell、Git、Goal、计划、工具和子代理。创建 `TaskRun`，在明确的权限和 workspace 上下文中执行，并保留 Task/Agent 审计与恢复信息。

`TaskRunSupervisor` 只负责 task execution snapshot、Git、权限、上下文和工具准备，创建 root Task、调度 Task/Agent 并把结果通知 Coordinator；它不能直接关闭 Turn。

### 2.3 接纳分类

接纳只读取结构化输入，不按用户文本中的关键词推断路由、协作模式、必调工具或完成证据：

- 会话命令、GPT Web 引擎会话：`conversation`。
- 界面“继续”按钮发出的 `resume`：恢复可恢复的执行链，或从用户主动停止的最近一轮检查点继续；两者都没有时拒绝。
- 目标模式开关 `goalMode`：`task`，要求维护计划并开放 Goal 工具。
- 其余所有消息：带完整工具面的 `task` 主线。是否读代码、执行命令、建立计划或派发代理，由模型根据任务自行判断；用户明确要求或禁止使用子代理时由模型遵循，运行时不再设置协作模式。
- 不得在模型输出后切换 profile。

代价：每个会话第一条进入 `task` 的消息会建立工作区快照与 Git 上下文（快照按会话缓存，后续轮次复用）。

Goal、Continue、子代理等待和恢复都是 `TaskRun` 内部行为，不创建第二条普通 Chat 链路。

### 2.4 核心不变量

1. 每个 Session 只有一个 `SessionTurnCoordinator`；HTTP 和 App Server 适配器都进入同一个 `TurnService`。
2. Turn 在接纳时固定 `conversation` 或 `task` profile，执行中不得隐式升级。
3. Turn 的唯一事实源是 Canonical Turn Log，Task 的唯一事实源是 TaskStore；projection 只读。
4. `accepted` 表示可靠接纳，不表示 Provider 已启动或执行成功；accepted、阶段边界、取消和终态必须可恢复，普通 delta 可以批量写回，终态必须强制 flush。
5. 终态 durable status mutation 的 `changed` 结果是唯一 terminal event 发布闸门；并发 finalizer 必须幂等，只有一个 `canonical_terminal_published`，迟到调用只能记录 `canonical_terminal_ignored`。
6. Provider、工具和 Task 回调携带不可变 attempt 身份；迟到结果标为 stale/rejected。权限、Git、幂等、审计和恢复安全不能为降低延迟而跳过。

## 3. 运行时职责与写入边界

### 3.1 `TurnService`

HTTP 和 App Server 适配器必须调用同一个 `TurnService`。它只负责：

- 输入、范围和权限前置校验；
- `request_id + request_fingerprint` 幂等；
- 识别 start、steer、continue、cancel、recover 和 queue；
- 向 Coordinator 发命令并返回 accepted receipt。

App Server 可以在调用前做只读的 canonical Turn/queued Turn replay 查找，用于把已接纳请求映射成协议层的 replay/queued 响应；这不是第二条接纳或写入路径。新的接纳、指纹冲突判断和任何 durable mutation 仍必须落到与 HTTP 相同的 `TurnService` 合同，不能由协议适配器自行创建 Turn。

它不得等待模型采样、完整 Git/task execution snapshot 准备、工具目录构建、上下文压缩或 TaskRun 完成。

### 3.2 `SessionTurnCoordinator`

Coordinator 采用 Actor 或单写者模型，拥有 active Turn、durable queued Turn、command mailbox、execution attempt、steer/continue 队列、取消令牌、终态和下一 Turn 的启动边界。

Coordinator 不得在状态锁内执行外部 IO。模型、工具、Git 和 Task 完成后，必须通过带身份的 command/event 回到 Coordinator；外部 API 不得直接改写 current Turn。

### 3.3 Task 域

- `TaskStore` 保存任务树、租约、checkpoint 和恢复数据；`TaskScheduler` 负责资源调度。
- Task durable commit 完成后，按 `TaskStore -> TaskCompletionNotifier -> Coordinator -> Sink` 通知；不得依赖周期轮询发现完成。
- 主线 Task 执行器遇到失败时，只能先 durable upsert 失败 item；不得在 TaskStore 终态提交前发布 `Turn failed` 或 canonical terminal event。TaskStore 的 Failed/Killed/Completed 提交后，由 finalizer 收口 Coordinator、资源 settlement，再由 Sink 发布唯一终态快照；sidechain 详情 item 不拥有 root Turn 终态，可以即时展示。
- Task mutation 临界区不得写 SessionStore、同步磁盘或调用跨域回调。
- 子代理只有在角色、能力、模型、父任务、容量和 Git 预检全部通过后，才可固化角色快照并原子创建 child task；预检失败不得创建 Task、lease 或 Thread。

### 子代理生命周期

TaskStore 的父子关系是任务树的唯一来源。父任务进入终态时级联终止未结束子树；agent_cancel 只能取消直接派发的代理及其子树。agent_wait 读取终态时生成的结构化回执，执行命令、退出码和文件变更来自工具记录。等待支持 all/any；审批关注信号可以提前返回，已结束的结果在超时返回时也计入已收集。

代理工作区从父工作树的派发时快照建立，包含未提交与未跟踪文件，不改变用户 index 和工作树。产出在代理分支提交后写入回执，通过 agent_apply 合入主线；冲突必须返回明确事实。详见[会话 Git 工作流](session-git-workflow.md)。

容量、角色和能力来自现有准入与角色注册表。只读角色的限制在工具权限层执行；子代理不隐式继承父任务的 Skill，排队代理的补充消息在启动时读取。相关回归位于 task_store、tool_batch、agent_spawn_preflight 和 task_execution_dispatcher。

### 3.4 `CanonicalTurnEventSink`

Sink 是 Turn 事实的唯一写入边界，负责事件顺序、attempt 和状态转移校验、Session 内连续序号、流式内容合并、批量持久化、通知发布和终态完整快照。

跨域通知必须带 `turn_id`、`task_run_id`（如有）和 `causation_id`。Task/Agent 的完整状态以 TaskStore 提交结果为准，不能重复转换出多个 Turn 终态。

### 3.5 Provider transport

Provider client、Tokio runtime 和 HTTP 连接池按进程复用；每次请求只创建 request-scoped stream 和取消句柄。Transport 负责连接、流解析、超时、取消和错误归一化，不负责 Turn 状态、持久化或跨 Turn 的有状态 continuation。

## 4. Turn、事件与持久化合同

### 4.1 最小 durable 记录

```text
TurnRecord {
  turn_id, session_id, turn_seq,
  request_id, request_fingerprint,
  execution_profile,
  status, execution_phase,
  execution_attempt_id,
  task_run_id?, root_task_id?,
  user_item_id, active_item_ids,
  accepted_at, completed_at?, failure?
}

TaskRunRecord {
  task_run_id, turn_id, root_task_id,
  status, execution_snapshot_id
}

TurnEventEnvelope {
  schema_version, event_id,
  session_id, turn_id, event_seq, turn_seq,
  event_kind, occurred_at,
  causation_id?, correlation_id, payload
}
```

### 4.2 不可违反的约束

1. 同一 Session 的 `event_seq` 单调递增且不重复。
2. 一个 `turn_id` 只能有一个终态；终态之后不得出现 delta、Task 或 phase 事件。
3. `task_run_id` 和 `root_task_id` 只在 `execution_profile=task` 时存在。
4. 相同 request ID 和 fingerprint 返回原 Turn receipt；相同 request ID 但 fingerprint 不同必须拒绝，不能创建第二个 Turn。
5. `ThreadChatMessage` 是 projection，不能作为独立事实源写入；Coordinator 不能从多个 projection 表反推事实。
6. 每个 attempt 绑定不可变 `context_snapshot_id`；输入变化后，新 attempt 必须使用新快照。迟到结果只能留下审计记录。

### 4.3 状态与命令

标准状态流：

```text
accepted -> preparing -> running -> streaming -> finalizing -> completed
preparing/running/streaming -> waiting_input | blocked | failed | cancelled
waiting_input -> preparing | running | blocked | failed | cancelled
blocked -> preparing | waiting_input | failed | cancelled
```

`status` 表示 Turn 生命周期，`execution_phase` 表示执行进度；队列用 `status=accepted + execution_phase=queued` 表达。

Coordinator 进入 `running` 后，必须在首个 Provider/tool 回调或流式 item 之前，通过 Sink 将同一 Turn 的 durable 状态推进到 `running`。这样 Renderer 先看到的 item 状态与后续 canonical 快照保持单调，不得出现 `running -> pending` 回退。

状态语义：

| 状态 | 语义 |
| --- | --- |
| `accepted` | 基础校验通过并可靠持久化，可在重启后恢复 |
| `preparing` | 准备上下文、Git、task execution snapshot、工具或 TaskRun |
| `running` | 执行器已启动但尚无可见模型内容 |
| `streaming` | 已收到可见模型 delta |
| `waiting_input` | 等待 steer/continue 等正常交互 |
| `blocked` | 权限、Git、资源、Provider 或恢复条件阻塞 |
| `finalizing` | 写入最终 item、用量和终态 |
| `completed` / `failed` / `cancelled` | 唯一终态 |

至少支持 `StartTurn`、`SteerTurn`、`ContinueTurn`、`CancelTurn`、`RecoverTurn`、`ProviderDelta`、`ProviderCompleted`、`ProviderFailed`、`TaskEvent` 和 `ExecutionFinished`。所有回调都带 attempt ID。

### 4.4 事件与流式写回

核心事件：

```text
turn.accepted / turn.phase_changed
turn.item_started / turn.item_delta / turn.item_completed
task.created / task.queued / task.running / task.blocked / task.completed / task.failed
turn.completed / turn.failed / turn.cancelled
```

`task.*` 的事实来自 TaskStore，`turn.*` 的事实来自 Sink；客户端可以统一订阅，但不能混用事实源。

`turn.item_delta` 至少包含 `turn_id`、`event_seq`、`item_id`、`item_version`、`base_content_length`、`content_length` 和 `delta`。前端按 `item_id + item_version + base_content_length` 应用增量；不匹配时请求 canonical snapshot，不能猜测拼接。

两个长度字段统一使用 Unicode scalar count，并由 Rust 与 TypeScript 合同测试验证；不能混用 Rust UTF-8 字节数或 JavaScript UTF-16 code unit 数。

Provider raw delta（包括只有 tool-call 的首 chunk）和首个可见 content/thinking delta 必须分别记录。事件 schema 使用显式主、次版本：读取端可兼容同主版本新增的可选字段；遇到不支持的主版本时停止该 Session 的 replay 并报告升级。破坏性字段变更须同步迁移说明、恢复和断线测试，不为此新增代码生成系统。

上下文压缩摘要必须为全部交接字段保留足够的输出预算；预算校验失败时要写入结构化压缩失败事实并停止当前 attempt，不得静默截断摘要、伪造完成或重复压缩循环。`running` 只能作为中间事实；要证明压缩链路收口，必须观察同一 Turn 的 `context_compaction=completed` 或明确合法的 `context_compaction=skipped` 终态，不能把 `running` 当成完成。

终态 durable commit 成功后才能发布完成通知；并发终态收口以 durable status mutation 的 `changed` 标志去重，重复回调不得追加错误 item 或发布第二个 terminal。主线 Task 的失败 item 可以先于 TaskStore 终态 durable，但在 finalizer 收口前不得向客户端发布带 terminal status 的快照；失败必须有结构化 `TurnFailure`，至少包含阶段、`error_code`、`retryable`、可选用户动作和公开消息；凭证、完整 Provider URL、堆栈和内部路径只能进入受控日志。

## 5. 恢复、安全与展示边界

### 5.1 持久化、背压与通知

```text
Coordinator 校验状态/幂等
  -> canonical accepted/phase/terminal durable commit
  -> projection checkpoint
  -> EventBus / SSE / WebSocket publish
```

普通 delta 进入有界 `TurnStreamBuffer`，按短周期或字节阈值批量写入；阶段边界、工具边界和终态强制 flush。队列满时只能合并同一 item 的连续 delta；仍无法消费时暂停或取消 attempt，并发布明确事实。

EventBus 只负责实时传输，不作为恢复依据。断线或序号缺口时，客户端按 `afterSequence` 获取事件尾部和 canonical snapshot；如果 daemon 已重启导致内存 EventBus 没有同一 Session 的新事件，HTTP SSE 必须从已恢复的 Canonical Turn Log 生成一次性 recovery envelope（不创建新 Turn、不重发 Provider），App Server WebSocket 必须提供等价的 `events/resyncRequired` canonical snapshot。恢复通知仍携带原 `session_id`、`turn_id` 和终态，不得把全局 `system.*` 事件当作业务 Turn 进展。

### 5.2 重启、取消与失败

启动时从 Canonical Turn Log 和 TaskStore checkpoint 重建 Coordinator：

1. 尚未发出 Provider 请求的 Turn 可以继续 preparation。
2. Provider 请求是否发出不确定时，进入 `blocked(reason=recovery_required)`，不得猜测成功或自动重复请求。
3. 已完成 Task 保留原 ID；无法解析 Turn 的历史任务只保留任务历史。
4. 迁移必须校验终态唯一性、序号连续性和 Turn/Task 关联后原子切换，并可重复执行。

恢复完成后，只有在同一 Session 的 canonical history、作用域和 profile 已恢复并通过校验时，才能接纳该 Session 的下一轮 Turn；下一轮使用恢复后的 history 建立新的 context snapshot。相同 `request_id + request_fingerprint` 的旧 Turn 仍走 replay，返回原 receipt，不得把 replay 误当成新 Turn 或再次调用 Provider。

取消由 Coordinator 发起并传播到 Provider、Task scheduler、工具进程、Browser/Git 资源；一个 Turn 只产生一个 `turn.cancelled`。迟到回调不能修改当前 Turn、下一 Turn 或可见消息。测试宿主关闭必须先停止新接纳，再取消活动 Turn，最后等待真实 settlement promise。`accepted/preparing` 尚未建立 execution chain 时，也必须经同一个 canonical sink 收口，不能因缺少可恢复链而留下非终态 Turn；桌面退出的重入事件不得绕过这段等待。

### 5.3 权限、Git、Browser、MCP 与前端

- profile 在 accepted 事实中冻结；模型输出不能把 `conversation` 变成 `task`。
- ReadOnly、Restricted、FullAccess 的工具策略必须在执行前生效。Browser 使用独立的 `BrowserToolAccess`（Read/Write/Mixed）与 Desktop Host 能力合同；当前产品合同下，工作区 AccessProfile 本身不拦截 Browser 控制（如导航、点击和页面脚本），不得把 ReadOnly 解释为“禁止 Browser 写动作”。两条权限轴都要记录分类、作用域和授权结果；仅在策略要求时记录审批请求与决定。
- workspace 内外、owner/session/Turn 作用域必须独立校验；`allow_for_turn` 不跨 Turn 或 Session 继承。真实外部副作用必须关联 Turn 身份、Provider 请求数、canonical 终态及实际授权依据。
- Git dirty、branch drift、merge conflict、审批取消/过期和重复审批决定必须产生可审计结果。审批过期固定以 `tool_approval_expired` 拒绝工具、保持无副作用并由 canonical Turn 进入失败终态；不得伪造成功文本或 `approval.resolved`。
- Browser/MCP/process 和子代理不能绕过 Turn、权限、审批、Git 或 TaskStore 边界；Host protocol double 不能替代真实 GUI 或外部副作用。
- HTTP `/api/session/turn` 与 App Server `turn/start` 直接进入同一个 `TurnService`。前端只消费统一 Turn notification，不根据 child task、定时器、bootstrap 或 loading 动画猜测终态。
- accepted 立即显示用户消息和准备状态；preparing、streaming、blocked、failed、completed 各呈现其 canonical 状态。原始/摘要视图和 Agent 详情共用事实投影，只允许布局不同。

### 5.4 模型调用失败的重试归属

每一类暂态故障只有一层负责重试，更上层不得再悄悄重跑同一请求（否则一次故障会被放大成成倍的上游请求，退避窗口也无法推算）。

| 层 | 负责的故障 | 策略 |
| --- | --- | --- |
| HTTP 调用层（`magi-bridge-client`） | 5xx、408/409/429/529、断连、文本标明过载 / 暂不可用的业务错误、SSE 内的过载事件、空流 | 只在尚未收到任何正文或思考内容时重试；最多 8 次，退避 0.5s 起指数增长到 30s（约 1.5 分钟，±25% 抖动）；服务商给出 `Retry-After` 时遵守（上限 60s）；响应超时只重试 2 次；额度耗尽类 429、地址配错返回的 HTML 不重试；进度通过 `ModelRetryRuntimeEvent` 展示且可取消 |
| 会话运行时（`conversation_loop` / `session_turn_execution`） | 内容已开始输出后的流中断、模型空响应、上下文超限 | 语义恢复（带已输出片段续写、追加提示、压缩上下文），次数有限；不重放整条请求 |
| 目标 | 目标已创建后的轮次失败；目标起始轮在创建目标之前失败 | 连续 3 次失败才受阻，间隔 5s、10s、20s…；起始轮按失败轮的 `modelFailure.retryable` 判断，退避 5s、10s 后把原请求重新排进会话队列，连同首次最多 3 次 |

鉴权失败、模型不存在、请求被拒、地区限制、上下文超长、工具不支持属于确定性错误，任何一层都不重试。GPT Web 引擎不自动重试，重试会重复向账号发消息。普通（非目标）轮次在 HTTP 调用层放弃后以失败收口，由用户决定是否重发。

## 6. 验证合同

### 6.1 真实入口与轨迹

`MagiTurnHarness` 必须经过真实 `TurnService`、Coordinator、事件投递和 projection；只调用 Sink 内部函数的单测只能证明局部 mutation。检查结果保留请求身份、实际 outcome 和终态；性能问题的观察边界见[性能验证](conversation-performance-validation.md)。

关闭测试宿主必须遵循：

```text
停止新接纳 -> 取消活动 Turn -> 等待 settlement promise -> 关闭 daemon/runtime/GUI
```

固定 sleep、看到进程退出就猜终态、测试清理直接写 canonical terminal 都不是完成证据。测试宿主必须等到真实 settlement；异常结果须记录缺失阶段与原因，不得合成事件。

### 6.2 按风险选择场景

下表用于按改动选择覆盖范围，不要求每次运行全部组合。日常优先运行直接受影响的模块回归；跨进程、恢复、权限或持久化边界改变时，补充对应的核心集成场景。验收结论只覆盖实际运行的路径。

| 维度 | 可选覆盖范围 |
| --- | --- |
| Session | 新建、短历史、长历史、压缩后、个人、工作区 |
| Profile | `conversation`、`task` |
| 工具 | 无工具、只读、写入、审批、Git、Browser、MCP、process |
| Agent | 无子代理、单个、多个、自定义角色、拒绝/失败 |
| 生命周期 | 正常、queued、blocked、failed、cancelled、steer、continue、重试 |
| Provider | 正常流、raw tool-call-only、空流、超时、断线、重试 |
| 载体与恢复 | SSE/WebSocket 重连、daemon 重启、Desktop reload/restart、request replay、fingerprint conflict、打包 Electron |

空流和超时若通过 fault-injection fixture 复现，只能关闭真实 HTTP bridge/daemon 的 transport 异常收口、重试边界和 settlement 切片；Provider 业务可用性仍须由真实上游正常流或明确的上游错误响应单独证明，不能把 fault-injection 结果扩大为完整 Provider 矩阵。

同一风险已有核心集成入口覆盖时，不再维护第二套采样或报告脚本。新增场景应落入所属入口，说明它保护的行为；完整跨平台发布验证仍遵循发布流程。

## 7. 完成定义与设计复审

### 7.1 整体完成定义

只有下列条目逐项具有与本次源码和构建对应的测试或真实运行证据，才允许宣称相关改动完成。本文件不保存某次运行的 artifact 或通过记录：

1. `conversation` 不创建 TaskRun、lease、Runner、Git execution context 或 task execution snapshot；只读 `context_snapshot_id` 不属于 TaskRun 资源。
2. 每个 Session 只有一个 active Coordinator；HTTP 和 App Server 使用同一个 TurnService。
3. Canonical Turn Log 和 TaskStore 分别成为唯一 Turn/Task 事实源，projection 没有独立写入口。
4. accepted、delta、终态、cancel、steer、continue、重连和重启恢复符合合同，终态只产生一次。
5. Task/Agent 完成由 durable commit + notifier 驱动，不依赖周期轮询；迟到结果被拒绝。
6. 普通 delta 使用有界写回，终态、重连和重启恢复内容一致。
7. 权限、Git、MCP、Browser、process 和子代理的声明场景集逐行可复核，包含真实副作用或明确拒绝、审批和终态关联。
8. 受影响模块通过定向回归；涉及跨进程和真实副作用的改动通过对应集成入口。真实 Provider 问题另做上游验证，不把本地 fixture 通过当作上游可用性证明。
9. 只有涉及性能收益的声明才要求同口径 before/after；保留原始数据、源码/构建身份及失败样本。普通行为修复不要求批量性能采样与报告派生。
10. 已证明不可达的旧双轨、同步提交、轮询终态和无效兼容语义已删除；保留的迁移/恢复/协议兼容语义有边界说明。

### 7.2 设计复审触发条件

遇到以下任一情况，应暂停实现并重新评审，而不是继续堆加兼容分支：

- 需要新增数据库、远程队列、跨进程 outbox 或独立 Task Domain Log；
- 会话命令或 GPT Web 的 conversation profile 被要求创建 Task/lease/Runner 才能返回；
- 同一个 Turn 仍由多个 store、Runner 或前端分别决定终态；
- 需要“新路径失败后回退旧路径”；
- 只能通过 sleep、轮询、前端 loading 或 bootstrap 刷新掩盖响应延迟；
- 为了性能删除权限、Git、幂等、恢复或审计事实。

最终判断标准是可观察行为：普通 Chat 能可靠接纳并流式响应，Task/子代理保留安全和恢复能力，Provider 首阶段、前端首渲染、终态通知、权限副作用和重启恢复都能由真实入口复现并审计。
