# Magi 消息响应核心架构：目标与验收合同

> 本文是目标架构和验收合同的唯一来源：定义职责边界、持久化/事件合同、场景范围及整体完成条件。
> 性能测量与可执行入口见[脚本与验证入口](validation.md)。历史重构进度与验收记录通过 Git 历史追溯。
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
- 界面“继续”按钮发出的 `resume`：恢复可恢复的执行链，或从最近失败、中断、用户主动停止的主线轮次检查点继续；缺少原 Task/Thread 时拒绝。控制输入保留在 canonical 中，通过已有 `visibility.renderable=false` 隐藏用户气泡；用户手动发送的文字“继续”仍作为正常消息展示。入口统一在输入框主按钮：无草稿时显示播放图标，有新文字、附件或命令时恢复发送图标；不在历史消息旁提供恢复操作，也不额外添加输入区提示。
- 目标模式开关 `goalMode`：`task`，要求维护计划并开放 Goal 工具。
- 其余所有消息：带完整工具面的 `task` 主线。是否读代码、执行命令、建立计划或派发代理，由模型根据任务自行判断；用户明确要求或禁止使用子代理时由模型遵循，运行时不再设置协作模式。
- 不得在模型输出后切换 profile。

原生主线与子代理使用同一个 `conversation_loop`；`conversation` 执行器只承载命令和 GPT Web 的独立上下文合同。

资源按实际工具调用准备：主线的纯文本、文件读取/检索、网络与 Browser 工具不因绑定 workspace 而建立完整快照或取得 Git 执行租约。首次写入、Shell、结构化 Git 或代理派发前，经 daemon 注入的准备入口检查工作区争用、按条件创建隔离副本、建立变更账本、校验 Git 基线并取得租约；失败先记录未执行回执并重建线程投影，再沿原 Task/Turn 收口。Continue 与工作流共用这一路径。子代理在首次本地工具前才建立隔离 worktree；纯文本和网络调用不提前分配。目录改变时同步投影路径权限、刷新 workspace 提示并清除旧读取缓存，引用旧绝对路径的调用明确拒绝执行。子代理分支产出不记入主目录变更账本；自动隔离后的派发继承当前副本目录。

工作流在结算待决子任务前和外部核心决策返回后，都重新检查原 Task/lease、Turn 与执行绑定。canonical Turn 已关闭而 Task 租约尚未撤销时，不能领取待决回执、重新排队，也不能接受迟到的完成、检查点或用户等待动作；取消宽限期只用于结算原调用，不授予提交新动作的资格。

工作流模型目录和工具执行共用当前执行计划中的 Skill 激活投影，缺少后续激活时沿用 Task 的初始绑定。`skill_apply` 与原生循环共用限制持久化入口，提交成功后才发布工具成功并进入下一步；Skill 工具面按当前内容重新解析，不以同名缓存代表最新授权。

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

HTTP 和 App Server 适配器必须调用同一个 `TurnService`。当前提交业务实现位于 `crates/magi-api/src/turn_service.rs`；HTTP 提交路由只补充可信 Desktop 来源标记并传入 DTO，App Server 也调用该服务。队列推进、Goal 自动续轮与任务终态回调引用同一份服务实现。

服务依赖按职责归位：`scope_binding` 统一校验会话/工作区归属并解析 Turn 所属会话；`task_dispatch` 负责 Task Turn 的接纳、准备、调度及恢复；`model_config` 负责会话模型绑定、Web 转换门禁、持久化与配置更新事件；浏览器标记上下文及 artifact 边界由 `browser_tool_runtime` 从 BrowserAuthority 读取。输入转换和接纳时间由 `TurnService` 管理，接纳时间以原子更新保证并发唯一、递增。上述业务模块不反向引用路由，路由直接调用对应实现，不保留辅助逻辑的旧转导出。中断、生命周期取消和显式继续执行也由 Turn 服务统一负责；HTTP、浏览器用户接管、GPT Web 停止和代理继续动作直接调用服务，不能通过构造 HTTP extractor 或调用路由函数执行业务。角色模板、引擎与角色绑定的读取和写入统一归 `model_config`，`ApiState` 不依赖设置路由。

它负责：

- 输入、范围和权限前置校验；
- `request_id + request_fingerprint` 幂等；
- 识别 start、steer、continue、cancel、recover 和 queue；
- 向 Coordinator 发命令并返回 accepted receipt。

App Server 可以在调用前做只读的 canonical Turn/queued Turn replay 查找，用于把已接纳请求映射成协议层的 replay/queued 响应；这不是第二条接纳或写入路径。新的接纳、指纹冲突判断和任何 durable mutation 仍必须落到与 HTTP 相同的 `TurnService` 合同，不能由协议适配器自行创建 Turn。

它不得等待模型采样、完整 Git/task execution snapshot 准备、工具目录构建、上下文压缩或 TaskRun 完成。

### 3.2 `SessionTurnCoordinator`

Coordinator 使用会话单写者边界决定 active Turn、输入投递、command mailbox、execution attempt、steer/continue FIFO、取消、终态及下一轮启动。持久输入由 SessionStore 拥有，队列只读取 Pending 输入，不再由 ApiState 单独保存。

Coordinator 不得在状态锁内执行外部 IO。模型、工具、Git 和 Task 完成后，必须通过带身份的 command/event 回到 Coordinator；外部 API 不得直接改写 current Turn。

文字与图片排队消息可通过既有 steer 入口引导当前 `conversation` 或主线 `task` Turn。能力由 Coordinator 的活动输入边界给出：先提交 canonical 用户 item，再进入同一 Turn 的 FIFO；主线 Task 在模型轮次之间消费，子代理不消费会话引导。模型准备结束时必须原子取走待处理引导或关闭边界；关闭后拒绝迟到输入，不得静默丢弃，也不得另建 Turn/Task。队列读取、领取、删除及转引导共用会话提交锁。用户 item 与 Input 的 Attached 转移写入同一个 canonical event segment；写盘成功后才更新内存并发布 Inbox 通知，不再存在独立的出队确认写入。

队列快照使用 `guide.status` 和 `guide.targetTurnId` 描述补充能力及不可用原因。转为补充时，客户端必须提交用户看到的 `expectedTurnId`，不能在服务端自动改投刚开始的另一轮。目标变化、边界关闭或输入形态不支持时保留原排队项。快照同时返回 `cursor.epoch`、`cursor.throughSequence` 和条目 `revision`；Web 的删除与改投递携带 revision，服务端在同一提交边界重新校验。旧 `canGuide` 已删除。`session.inbox.changed` 在输入事务提交后统一发布，UI 不靠终态事件猜测队列是否改变。

“停止并改做”使用独立的 `interruptTurnId`，与编辑后重发的 `replaceTurnId` 区分。服务端先在 Input 记录原 Turn、原 Task 和待结算意图，再请求取消并等待 Task/dispatcher、工具进程和 Browser 租约收口；结算回执持久提交后才原子绑定新的 Turn。Pending 替代输入阻止普通输入或 Goal 自动续跑抢跑，并优先于原 FIFO 项；原 FIFO 继续保留。取消失败、未退出或回执写盘失败均保留输入，队列投影展示错误。`/session/queue/retry` 以 revision 重新驱动同一输入，不生成新身份。删除待处理替代输入也必须先完成同一结算；失败保留屏障，成功删除后唤醒原 FIFO。重启从 canonical 日志恢复屏障与回执，不从旧终态猜测资源已结算。state layout 16 拒绝旧 daemon 覆盖此事实。

App Server `turn/interrupt` 要求 `sessionId / expectedTurnId`，在同一提交锁内校验并调用现有取消/结算入口；初始化用 `turnInterrupt` 广告支持。已进入停止操作后，RPC 超时、取消或断开不能 abort 资源结算。HTTP 与 App Server 的停止响应复用 schema 生成类型。GUI/Web 的普通发送、引导、停止、继续与替代均使用 daemon 权威，不增加客户端队列或恢复引擎。

队列的 HTTP 与 App Server 适配器共用 `input_service`；`session/queue/read / remove / guide / retry` 通过 `sessionQueue` 声明支持，快照由 schema 生成。App Server 队列变更要求明确输入 ID/revision，转补充还要求目标 Turn；重试只驱动同一个待结算替代输入。HTTP 保留已有 revision 可选的兼容入口，业务转换与提交锁仍只有一份。App Server 适配器按协议要求检查显式正数 revision；共享 `input_service` 同样拒绝零版本，并拥有 Pending/CAS 和已绑定输入的重试核验。GUI 的删除、补充与重试提交点击时的版本，桥接层不能读取更新后的快照代填。补充重试只在原 canonical 用户 item 的 `route=steer`、目标 Turn 和绑定前版本匹配时读回成功回执；首次消费后仍校验同一版本，普通下一轮领取不能冒充补充成功。

待审批/提问与 Goal 控制分别通过 `sessionInteractions / goalControl` 声明，HTTP 与 App Server 复用 `interaction_service / goal_service`。重启后先处理持久交互，再显式 Continue；提前 Continue 必须在修改 canonical/Coordinator 之前拒绝。暂停目标只结算当前归其拥有的 Turn/任务，不能取消同会话的其他显式工作。用量仅用于统计；旧 `budget_limited / usage_limited` 只在持久化读取边界映射为普通暂停，不作为现行状态写出。

Input 的生命周期是 Pending → Attached → Consumed，尚未领取的输入可转 Cancelled。Attached 表示绑定到唯一 Turn/item；Consumed 表示输入已纳入某个冻结的模型请求。两条原生模型入口共用 `StepModelClient`，在同一 Session 事务中记录 canonical Turn 的 `modelSteps` 与输入首次消费；后来接纳的 steer 留给下一 Step。每个 Step 固定 Task/thread、上下文检查点代际、输入集合、请求指纹与规模；HTTP 重试有独立 attempt，改变请求正文的恢复创建新 Step。响应指纹提交后才允许工具循环使用结果，失败则停止交付。`Started` 在重启后按结果未知处理，`ResponseReceived` 不代表工具执行完成；禁止从消费记录盲目重发副作用。Step 在发送前绑定 Goal；响应用量和该 Goal 的累计 token 水位与响应指纹一起提交。Goal 的 tokensUsed 从持久水位重建，重复恢复不重复累加；模型请求次数从 Step/attempt journal 派生。用量仅用于统计，不设置任务预算或估算预留。用户暂停在新 Step/transport attempt 发送前阻止调用；已发送请求的迟到响应仍可结算用量，已关闭 Turn 不允许新 Step/attempt，也不向工具循环交付迟到结果。恢复沿用 Goal/Plan revision，不要求增额。压缩等必要的单次输出上限仍固定在 Step 身份中。共用每次 activation 1000 次模型循环安全上限，canonical Agent 执行树另有累计 10000 次 transport 尝试安全上限，触发明确失败并保留持久进展。state layout 8 读取旧预算字段时仅做丢弃迁移，旧预算受限目标转为普通暂停，显式恢复后继续；其他未知持久字段仍拒绝。

补充输入的回执直接投影 canonical `modelSteps.inputRequestIds` 与 Step 状态，不另存一份 UI 消费事实。SessionStore 的同一个 Step 提交通知同时驱动实时 `session.turn.step` 与 Goal 用量刷新，替代原 Goal 专用观察入口；事件只带单个 Step 和不含正文的原 Turn 基线，以支持客户端尚未读到独立代理恢复 Turn 的情况。历史与实时均保留这些 Step，旧 item 快照不能撤销已结算回执；压缩 Step 不代表补充已用于主执行。时间线区分等待下一步、已进入请求、已收到响应、结果待核对和本次未记录使用；任务终态不能把在途请求推断成成功。移除没有事实生产方的 `isSupplementary` 悬停标签、样式和元数据字段。

旧 `session-turn-queue.json` 只在启动迁移时读取，按 request/input 身份去重并匹配 canonical 用户 item；完成后改名保留迁移备份。state layout 7 阻止旧 daemon 写入包含 Input/Step/Goal 用量水位的新版数据（2–6 升级保留原标记），工作区可移植 projection 导入同时保留 Input 记录与 revision。

后台 Task 完成通知来自普通线程时，后续工作使用进程级常驻异步运行时，不能为 finalizer 创建临时运行时并在其返回时取消已派生的队列或 Goal 工作。

当前 Turn 结算后的选择顺序是用户明确排队输入优先、Goal 自动续跑次之。续跑退避结束后在同一 session 提交锁内复查队列和会话状态，避免自动续跑抢过退避期间到达的输入。

### 3.3 Task 域

- `TaskStore` 保存任务树、租约、checkpoint 和恢复数据；`TaskScheduler` 负责资源调度。
- Task durable commit 完成后，按 `TaskStore -> TaskCompletionNotifier -> Coordinator -> Sink` 通知；不得依赖周期轮询发现完成。
- Dispatcher 构造时必须绑定完成交付目标；生产环境的 Worker 结果直接进入同一 TaskCompletionNotifier，来源绑定与结果交付不能指向不同通知器。不保留可选通知目标、内存结果缓冲队列或 Runner 测试专用轮询提交路径；迟到 lease 和重复终态仍由 TaskStore 与通知器核对。
- 主线 Task 执行器遇到失败时，只能先 durable upsert 失败 item；不得在 TaskStore 终态提交前发布 `Turn failed` 或 canonical terminal event。TaskStore 的 Failed/Killed/Completed 提交后，由 finalizer 收口 Coordinator、资源 settlement，再由 Sink 发布唯一终态快照；sidechain 详情 item 不拥有 root Turn 终态，可以即时展示。
- Task mutation 临界区不得写 SessionStore、同步磁盘或调用跨域回调。
- 子代理只有在角色、能力、模型、父任务、容量和 Git 预检全部通过后，才可固化角色快照并原子创建 child task；预检失败不得创建 Task、lease 或 Thread。

### 子代理生命周期

daemon 重启先从 TaskStore 的持久 activation、lease 和 Worker checkpoint 结算易失执行态，按当前 execution scope 而非完整审计树选择范围；之后逐 Session 核对并结算独立 Agent canonical Turn，再处理主线中断。主线已结束、归档或切换不影响原转交 Agent 的持久交互定位；问题/审批答复需要有效 Session/Goal 拥有方及子树内的中断父链，只写回原调用，不创建新 Step 或重新开放已关闭 Turn。未知工具副作用保持 outcome_unknown，在途 Step 只能结算原用量。原主线 Continue 的分支选择、旧终态补交和路径恢复不能接管已经转交的执行范围。

子代理选择规则作为每轮动态 `CurrentAgentMode` developer 片段注入，不能混入 `TaskContext` 的任务事实或写入 transcript。主线与获授嵌套委派的代理仅在本轮实际 tools 提供 `agent_spawn` 时收到派发规则与角色清单；Skill、策略、执行器或当前阶段隐藏该工具时说明当前不可派发，下一轮依据实际定义刷新。初始估算、最终工具筛选和压缩重建复用同一入口；片段替换不追加旧能力。模型判断委派价值，执行预检与 TaskStore 继续拥有授权和注册事实，不从提示文本授予权限。详见[当前唤起方案](subagent-delegation-design.md)。

创建能力还必须具备完整生命周期：有效 Task/Skill 工具范围须同时允许 `agent_wait` 和 `agent_cancel`，缺一则关闭 `agent_spawn`；保留仍允许的已有代理管理工具，不替策略授予缺失工具。初始与动态 Skill 工具面在阶段临时筛选前检查，创建前预检复核同一权限事实，拒绝不得创建 Task、Thread 或注册资源。Goal/必调合同暂时只展示创建工具，不等于后续等待和取消被禁止。详见[完整方案与验收](subagent-delegation-design.md)。

动态激活的代理 Skill 归原 Agent canonical Turn 保存；同一 Task 恢复从该 Turn 重建限制，工具/终态投影不得删除。激活须携带原执行 Turn，先持久保存限制再保存成功回执，失败即停止后续模型调用；限制已提交而回执失败时仍保留限制和未知结果。主线分支 Skill 在会话提交锁内核对 Turn 并精确更新，不能使用旧执行链快照覆盖当前主线。详见[子代理限制与恢复合同](subagent-delegation-design.md)。

LocalAgent 的标题、目标和自然语言阶段名称不选择执行实现。主线与子代理统一经过模型循环、持久 Step 和结构化完成合同；`no_tools` 仅关闭工具。规划及验证没有关键词触发的固定答复或自动“通过”路径，依赖中的工具调用成功也不能替代业务验收。

Worker 的 `finish` 阶段只描述执行进度，不拥有 Task 成功事实。Continue 不得把阶段快照、有效租约和中间 `output_refs` 拼成最终回复并提交 Completed；成功仍须从原执行结果通过统一完成门提交。恢复资格统一读取 TaskStore 的实际状态、mission/root 和当前执行拥有方，HTTP Continue、代理列表与运行投影共用同一判定。Failed 不能被陈旧 `finish` 阶段遮蔽，Completed/Killed 也不能因投影不一致而重新开放。

TaskStore 的父子关系是任务树的唯一来源。父任务进入终态时级联终止仍归其拥有的未结束子树；agent_cancel 只能取消直接派发的代理及其子树。子树取消和 Runner 异常失败通过 TaskStore 的单次 mutation/checkpoint 同时结算 Task 与当前租约，删除调用方逐项写入的路径；状态通知只在整体提交后发布，写盘失败不释放未结算的准入预占。普通任务 Turn 停止按当前执行拥有方范围取消工具/Browser 资源，已转交的 Session/Goal 子树继续由自己的拥有方控制；Session 关闭/删除才跨完整审计树收口。agent_wait 读取终态时生成的结构化回执，执行命令、退出码和文件变更来自工具记录。等待支持 all/any；审批关注信号可以提前返回，已结束的结果在超时返回时也计入已收集。

agent_cancel 复用同一终态回执入口返回 `results[]`；取消晚于完成时返回 `already_terminal` 和原结果，保留原 Task 输出与待采用分支。主线依据实际交付的终态结果判断已收集；旧 `already_terminal` 没有结果时仍须等待收集。公开错误描述事实，是否重新委派由共享恢复规则决定，不能把取消或失败当作自动再次唤起的依据。详见[子代理设计 §5.1](subagent-delegation-design.md)。

取消还须区分 Task 终态与执行退出：宿主经原资源入口取消指定子树，Dispatcher 等待这些 Task 的全部执行退出，再核对工具/进程活动；不等待调用者或兄弟任务。`resources_settled=true` 才确认资源退出，结算失败保留原回执并返回 `cancel_unsettled`；轻量装配未提供结算能力时返回 false。恢复后显式再次取消重新查询原 Task，不复用旧结算结果，不放宽其他副作用重放保护。取消能力本身不授予转交权；转交须经下述独立入口，见[子代理生命周期合同](subagent-delegation-design.md)。

`TaskExecutorBinding.agent_activation` 保存 Agent 的稳定身份、Session/Goal 生命周期、generation 和前序 Task。显式保留身份的已完成子代理可用 `agent_spawn(resume_task_id)` 创建新 activation，使用新 Task、Thread、租约和当前父任务授权。TaskStore 原子检查最新来源和代际；历史输出/证据/transcript 仅按该 Agent 的前序链检索，不继承旧成功结果作为本次执行证据，未知副作用护栏保留。运行中子树只有在显式转交事实和新的 Session/Goal 拥有方都成立时才脱离原父 Turn；身份保留本身不等于转交。执行入口通过 `TaskExecutionGuard` 固定原 Task/lease/Turn，读取 TaskStore 活跃父链和 SessionStore 的 canonical 准入；模型、工具及缓存结果、等待恢复、工作流和资源准备共用该边界。父级终态、会话关闭、Goal 暂停或执行代际更换会撤销对应拥有方资格，不等待错误范围的级联清理。canonical 取消后退出的 worker 用明确 Cancelled 回执按原 lease 结算 Killed，不能把用户取消抢先记成普通失败；迟到响应仍结算原 Step 用量。

原生 Task/Goal 主线与子代理直接进入同一个模型/工具 Step 循环，不再包裹内存 `Conversation`、`TurnDriver` 或第二份 `TurnState`。单次派发由 Runner 准入与 TaskStore 原子租约保证；`ConversationRegistry` 仅投递输入和交互，不保存生命周期。子代理初始 assignment 与后续消息共用按任务 FIFO，注册 Task 前完成投递，模型/工具 IO 不持有输入锁；正常、失败、取消和 panic 退出都会关闭通道，尚未派发就进入终态的 Task 通过原资源结算入口清理。Session conversation profile 保持不创建 Task 的边界，两种 profile 共用 Step 请求/attempt 日志。

Session/Goal Agent 的 Step、工具、审批与提问属于独立 canonical Turn，主会话当前输入指针保持主线 Turn。父 Turn 重启中断与其未结束子轮在同一个 canonical 事务结算，交互仍绑定原 Task/Turn。显式 Continue 为同一 Task/Thread 创建新的执行 Turn，以 `agentExecution.previousTurnId` 关联旧中断记录；旧终态不可重新打开，重载校验完整父链与来源。恢复先运行 Pending 父 Agent，再允许其 Pending 子任务派发，避免双方互等；终态写回携带具体 Turn，迟到通知不能结束后续执行。持久布局当前为 18，2–17 升级保留原标记。

转交准入通过只读端口查询 TaskStore 的原转交根 Task，SessionStore 核对 activation、canonical 来源父链和当前 Session/Goal 控制版本，再用于 Step、工具/Job 与交互写回；查询失败不能当作未转交，已有 Step 用量归属不改写。canonical 不复制拥有方。HTTP/App Server 和 Web 通过同一 `agent_service` 显式提交转交和独立继续。继续只接受原转交子树根及预期 Turn/activation，先处理原交互，再原子接纳该范围的新 Turn，保留 Skill、目录和审批来源；operation 重试只读 canonical 回执。终态投影与恢复提交共用原执行范围的 restart 锁，避免校验后旧回调清理新执行资源；主线与独立代理重新执行时都递增 TaskStore 原 retry_count，前后终态即使同毫秒也能拒绝旧代际；模型请求前从 canonical 重建原线程历史，不依赖主线恢复指针。具体限制见[当前实现边界](subagent-delegation-design.md#10-当前实现边界)。

嵌套代理首次本地执行时，从直接父代理活动 worktree 的当前文件建立快照，核对父 Task/lease 后分配自己的目录。`agent_apply` 只采用已完成的直接子代理分支差异；受委派代理合入自己的活动可写副本，主线采用其直接子代理，不能越级绕过中间代理。缺失 TaskStore 或调用任务不能采用产出。嵌套身份仅由同一父 Agent 的 activation 继续，主线保留身份目录也按所有权过滤；取消根任务结算当前拥有的子树、后台 Job 和 worktree；Session 销毁覆盖已转交分支。布局 14 阻止旧 daemon 覆盖委派字段，2–13 升级保留旧 marker。

后台 Shell 的 Job 事实存在原 canonical 工具 item 的 `backgroundJob` metadata，启动前提交 Starting，输出 artifact 与进程退出后结算分别记录。Job 更新保留原 Turn/item 来源，并通过原 `session.turn.item` 事件投影；不会生成新 Turn 或复制任务状态。`job_id` 支持跨轮/重启读取，临时 terminal_id 不能跨 daemon 使用。子代理进程权限限定本 Task/activation；重启未结算 Job 标为结果未知，不恢复旧 PID，也不自动重放。Job 随原 Task 的当前执行拥有方取消与释放；等待 Job 使用事件唤醒并释放模型名额，返回前重新准入。Job 本身不另设独立转交入口。

取消必须区分物理进程退出与 Job 终态提交。终态写盘失败时保留原 Job 的结算记录，停止调用返回失败，资源查询仍显示未结算；重试同一个 kill 只重试提交，不重新启动命令或向已回收 PID 发送信号。原 Job 已为 Completed/Failed/Cancelled 且资源已结算时，重复 kill 读取同一 canonical Job 回执；不能因临时 terminal_id 已移除而误报不存在，也不能改写原退出码、完成时间或 revision。OutcomeUnknown 仍要求核对原资源，不能假报停止成功。启动与停止共用原在途操作登记，移走进程句柄做 IO 的窗口也不能被观察为资源已释放。未提交取消的记录不进入已退出进程的数量淘汰。正常退出结算先回收原进程树，再等待 stdout/stderr 读取线程排空、同步输出文件，最后提交原 Job 的完成或失败事实；启动期早退与异步退出共用该入口。后代仍持有继承管道时，不能先等待管道关闭，再把该等待超时误记为结果未知。进程回收、输出读取或同步失败仍保留结果未知及未结算资源边界。

代理工作区从首次本地执行时的父工作树快照建立，包含未提交与未跟踪文件，不改变用户 index 和工作树。产出在代理分支提交后写入回执，通过 agent_apply 合入主线；冲突必须返回明确事实。详见[会话 Git 工作流](session-git-workflow.md)。

容量、角色和能力来自现有准入与角色注册表。子代理的访问模式与父任务（主对话）完全一致，不因角色收紧；只读角色（explorer、reviewer）的只读职责由角色提示词约束，不再把访问模式强制降为只读；普通 Skill 流程说明不隐式继承；父 Skill 明确限制工具范围时，预检与注册要求 inherit_skill=true，排队代理的补充消息在启动时读取。相关回归位于 task_store、tool_batch、agent_spawn_preflight 和 task_execution_dispatcher。

内置工具的模型调用范围由 `BuiltinToolName::model_call_scope` 定义，工具目录、模型工具定义过滤和调用入口复用这一决策，实际权限与资源绑定仍分别预检。子代理可通过 `ask_user_question` 创建带 task/Turn/tool-call 身份的用户交互，答复只回到原调用；共享 SessionPlan 与项目记忆仍由主线控制；派发工具允许根协调器和显式获授 allow_delegation 的直接子代理使用，最多两层。嵌套任务不能绑定共享计划项，TaskStore 注册和持久恢复核对真实父链与相同授权，缓存按委派权区分工具面。封闭 Skill 的标准工具范围在进入任何 Builtin/MCP/插件/编排执行器前统一校验；Goal 模式仍保留既有四个生命周期工具，自定义 handler 独立校验绑定。Skill 移除后范围为空，旧 schema 不授权执行。Browser/MCP/插件刷新复用同一策略，禁止失效配置回退为全部工具。

执行期的 `ToolExecutionPolicy.allowed_tool_names` 用 `None` 表示继承、`Some([])` 表示禁止全部，Skill 来源只用于审计。Task 的历史空白名单仍按原合同表示继承；非空白名单与当前 Skill 范围取交集，空交集不得重新放开，显式拒绝始终优先。内置执行和 `tool_catalog` 共用这一合并结果，不再另装配一份遗漏 Task 工具限制的目录策略。此对象仅在运行期构建，不改变持久 TaskPolicy、Skill 配置或 App Server wire 格式。

MCP 与插件按同一个副作用/访问档判定组装工具面、目录和执行回执：声明只读的工具允许使用；有副作用的工具在 ReadOnly 下拒绝、Restricted 下等待原调用审批、FullAccess 下放行。Task/Skill 的显式禁用与白名单范围仍独立生效，批准不能解除它们，等待期间撤销 Skill 也须阻止执行。目录可展示被拒绝或缺执行器的条目及原因；schema 加载不等于授权，缺 MCP 执行器时不向模型发布该工具定义。插件的目录和执行器沿既有成对注册入口接入。

人工提问以原 canonical 工具 item 的 `userQuestion` 为权威：请求/等待原子提交，答复或跳过先持久化再唤醒等待者，取消状态不可回退。等待注册表仅承载进程内通道。原生 Task 重启后的未答问题保留在原 interrupted Turn；先回答/跳过再从既有 Continue 恢复，恢复领取与答复提交使用同一个 canonical Session 锁。已答内容从 canonical metadata 投影到工具历史，工具线程尚未来得及写结果不导致答案丢失。迟到进度/快照不能删除交互或覆盖已确认答复。GPT Web 答复也先持久化，但重启后的网页步骤不自动重放。

原生工具审批以同一工具 item 的 `toolApproval` 为权威，包含参数指纹、执行目录、权限快照、决定及单次执行领取。重启后的待审批仍由原审批 ID 处理；批准本身不执行工具，先处理交互再显式 Continue。恢复只领取同一 Task 恢复链、相同参数/真实目录/权限快照的未执行批准，来源领取与目标 running 原子提交；写盘失败阻止执行。取消、过期、拒绝和已领取批准不能放行副作用。实际工具执行仍重查 Skill、任务状态和租约；已领取后结果未知时不自动重放。state layout 12 保护交互事实；外部 MCP 合成调用继续沿用活跃客户端合同，不承诺重启重放。

`shell_exec(action=wait, job_id)` 通过 Job 提交后的通知等待终态，读取输出仍以原 canonical item 与 daemon artifact 为准；超时返回当前状态，失败、取消与结果未知不报告为 Job 成功。`read/wait/list` 是进程观察，执行范围校验仍有效，但不扫描工作区或准备 worktree。同步读取与 Job 等待共用作用域取消入口。等待代理、Job、审批、用户答复、父代理上下文或工作区资源时释放模型执行名额，返回模型前按当前容量重新准入；guard 析构不再无条件恢复 Running。进程仍可能写入时不擅自释放仓库租约。

`agent_wait` 与进程 `read/wait/list` 在恢复时重新读取当前状态，既不按未知副作用阻止，也不复用旧超时或已完成回执。此分类共用内置工具的进程观察判断；启动、stdin 写入、终止与文件写入仍按实际副作用证据决定是否可恢复。


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

执行 sidecar checkpoint 按 Session 确认实际提交成功的版本。工作区暂未注册时，其脏版本继续保留，其他会话仍可正常落盘；重新注册后写回原工作区路径，不创建全局备用投影。快照构造期间产生的新版本、未提交项和写盘失败都不能被本次确认清除。移除工作区通过原 API 先撤销 MCP 准入、提交检查点，再清除属于该工作区的当前导航选择并移除注册；会话历史保留。

EventBus 只负责实时传输，不作为恢复依据。断线或序号缺口时，客户端按 `afterSequence` 获取事件尾部和 canonical snapshot；如果 daemon 已重启导致内存 EventBus 没有同一 Session 的新事件，HTTP SSE 必须从已恢复的 Canonical Turn Log 生成一次性 recovery envelope（不创建新 Turn、不重发 Provider），App Server WebSocket 必须提供等价的 `events/resyncRequired` canonical snapshot。恢复通知仍携带原 `session_id`、`turn_id` 和终态，不得把全局 `system.*` 事件当作业务 Turn 进展。

侧栏会话目录独立于当前导航：Web 启动、手动刷新和连接恢复时读取所有已注册工作区的目录。SSE 与 App Server 对未订阅会话的目录相关变化投影为 `session.directory.changed`，只携带作用域、工作区和会话身份，保留原序号，不传递正文、工具结果或任务上下文，也不改写 EventBus 原事件。客户端按工作区合并通知并重读权威目录，以运行时代际与事件游标拒绝旧快照；后台刷新不能改变当前会话绑定或覆盖前台目录投影。

### 5.2 重启、取消与失败

启动时从 Canonical Turn Log 和 TaskStore checkpoint 重建 Coordinator：

1. 尚未发出 Provider 请求的 Turn 可以继续 preparation。
2. Provider 请求是否发出不确定时，进入 `blocked(reason=recovery_required)`，不得猜测成功或自动重复请求。
3. 已完成 Task 保留原 ID；无法解析 Turn 的历史任务只保留任务历史。
4. 迁移必须校验终态唯一性、序号连续性和 Turn/Task 关联后原子切换，并可重复执行。

恢复完成后，只有在同一 Session 的 canonical history、作用域和 profile 已恢复并通过校验时，才能接纳该 Session 的下一轮 Turn；下一轮使用恢复后的 history 建立新的 context snapshot。相同 `request_id + request_fingerprint` 的旧 Turn 仍走 replay，返回原 receipt，不得把 replay 误当成新 Turn 或再次调用 Provider。

原生 Task 恢复的工具执行裁决和完成证据直接读取 canonical 工具 item；Thread transcript 与上下文压缩只用于组装模型请求。连续 Continue 沿 TaskRecoveryCheckpoint 读取完整来源链，缺失来源、越出 session/mission 或循环时拒绝执行。相同规范化参数的未知副作用不能被较早的成功结果掩盖；等待授权或明确尚未执行的调用可以重新申请。工具 item 身份由 task、Step 与 Provider call ID 共同确定，Provider 复用 call ID 不能覆盖旧事实。审批通过后的 running 事实必须先提交，提交失败阻止执行；审批等待期间 Skill 撤销在继续前复查。

子代理上下文补充、访问回执和 revision 由同一个 TaskStore 事务接纳，锁内核对父任务作用域与接纳状态。Runner 获得执行租约后读取最新任务，再向 Dispatcher 传递启动上下文，不能使用准入扫描时的旧副本。

取消由 Coordinator 发起并传播到 Provider、Task scheduler、工具进程、Browser/Git 资源；一个 Turn 只产生一个 `turn.cancelled`。迟到回调不能修改当前 Turn、下一 Turn 或可见消息。测试宿主关闭必须先停止新接纳，再取消活动 Turn，最后等待真实 settlement promise。`accepted/preparing` 尚未建立 execution chain 时，也必须经同一个 canonical sink 收口，不能因缺少可恢复链而留下非终态 Turn；桌面退出的重入事件不得绕过这段等待。

### 5.3 权限、Git、Browser、MCP 与前端

- profile 在 accepted 事实中冻结；模型输出不能把 `conversation` 变成 `task`。
- ReadOnly、Restricted、FullAccess 的工具策略必须在执行前生效。Browser 的能力目录使用 `BrowserToolAccess`（Read/Write/Mixed）与 Desktop Host 合同，网页动作另由同一 PermissionEngine 按工具名和参数判定：只读档允许导航和观察，拒绝点击、输入等写动作；受限档的脚本、存储写入及 WebMCP execute 需要审批。具体矩阵见 [Browser 访问档位与网页写动作](browser-runtime-design.md#57-访问档位与网页写动作)。能力、作用域和授权结果分别记录，不能从目录的粗粒度标签推断实际动作已获授权。
- workspace 内外、owner/session/Turn 作用域必须独立校验；`allow_for_turn` 不跨 Turn 或 Session 继承。真实外部副作用必须关联 Turn 身份、Provider 请求数、canonical 终态及实际授权依据。
- Git dirty、branch drift、merge conflict、审批取消/过期和重复审批决定必须产生可审计结果。审批过期固定以 `tool_approval_expired` 拒绝工具、保持无副作用并由 canonical Turn 进入失败终态；不得伪造成功文本或 `approval.resolved`。
- Browser/MCP/process 和子代理不能绕过 Turn、权限、审批、Git 或 TaskStore 边界；Host protocol double 不能替代真实 GUI 或外部副作用。
- HTTP `/api/session/turn` 与 App Server `turn/start` 直接进入同一个 `TurnService`。前端只消费统一 Turn notification，不根据 child task、定时器、bootstrap 或 loading 动画猜测终态。
- HTTP 历史与接纳响应、SSE 和 App Server 实时/恢复通知共用公开 canonical 投影；同一 itemVersion 的脱敏结果必须一致。增量帧的 occurred_at 使用存储 item.updated_at，前端据此还原更新时刻；合并历史窗口后重建 turn/item 索引，后续事件更新原实体。
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

`MagiTurnHarness` 必须经过真实 `TurnService`、Coordinator、事件投递和 projection；只调用 Sink 内部函数的单测只能证明局部 mutation。检查结果保留请求身份、实际 outcome 和终态；性能问题的观察边界见[验证入口的性能测量](validation.md#性能测量)。

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

### Executor 能力边界

当前只维护原生 Provider、现有插件和 GPT Web。外部 Executor 仅预留通用接口和历史证据读取，没有具体适配器或配置入口。

`ExecutorCapabilities` 来自可信宿主适配器，复用 App Server schema。原生 Provider、通用文本插件和 GPT Web 分别声明输入、工具、引导、恢复、供应商输出上限与上下文所有权。`StepModelClient` 在写入 Step/消费输入前校验请求；能力及 `execution / context_compaction` 用途与请求一同冻结，辅助压缩不能改变主执行器的引导能力。响应指纹与用量先持久化，合同外的原生工具不向工具循环交付。

通用插件不再截取最后一条消息：完整文字历史以带角色的 JSON prompt 传递；图片、原生工具及输出上限不能静默忽略。无 checkpoint 恢复能力的空响应/中断不会进入原生自动补发；Magi Continue 拒绝恢复该任务，即使后来已切换为原生模型，仍以来源 Step 的能力为准。GPT Web 的远端会话续接保持其原有专用合同，不等于支持 Magi 的工具检查点重放。

排队转引导与直接引导共用服务端能力检查；不支持时返回 `unsupported_executor` 原因并保留 Pending 输入。运行中优先读取冻结的主线 execution Step，未发送前对现有插件/网页配置保守关闭引导。state layout 13 保护新增 Step 能力及用途，2–12 自动升级并保留原 marker；旧历史字段缺失保留未知。
