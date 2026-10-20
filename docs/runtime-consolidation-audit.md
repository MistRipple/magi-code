# 运行时改造收敛审计

更新：2026-10-20。本文记录本次存量审计和范围收敛决定，不是新的运行时事实源。业务状态仍由 `magi-daemon`、`SessionStore` 和 `TaskStore` 拥有。

## 结论

最初审计时，工作树相对基线的新增约 52,834 行，其中约 27,644 行位于测试/验收代码，约 3,640 行是协议 schema/生成物，约 19,795 行是实现与配置，约 1,755 行是文档。这是范围清理前的统计，不代表当前行数。这个规模不能直接判定为全部无效，但已经超过“只修排队、引导和子代理唤起”的合理变更边界。

本轮冻结新增能力，先完成以下收敛：

- 废弃 Magi CLI，移除新增 `apps/cli` 产品入口、Cargo workspace 成员、CLI 文档、专属验收脚本和 package scripts，不再保留后续客户端规划。原交互/恢复/Goal 核心验收迁至 `test:daemon-controls`，直接调用 App Server；命令解析与 watch 子进程用例随 CLI 撤下。App Server 继续作为 GUI/Web 共用合同。
- 保留输入、交互、Goal、Agent Turn、Step、取消结算和资源拥有方的应用服务。它们被 HTTP 与 App Server 共用，是排队残留、引导不可用、恢复重复执行和子代理生命周期问题的共同事实入口。
- 保留测试中的故障注入和恢复断言，但不把测试行数当成交付能力；重复或只验证旧捷径的用例应在后续逐组删除。
- 保留 `ExecutorCapabilities` 和执行范围绑定等通用接口，因为引导、恢复和插件/模型包装器使用它；具体 Codex 外部执行器、配置/UI 和专属验收代码已撤下。历史执行证据只按来源读取，不进入工具重放；当前不交付任何具体外部 Executor。
- 撤下独立移动端与 Connect 产品规划。Magi 只做 Desktop GUI 与 Web；手机和平板通过同一 Web 工作台访问，响应式和触控适配继续维护。

## 范围判断

| 范围 | 当前决定 | 退出条件 |
| --- | --- | --- |
| P0–P1 输入、队列、引导 | 保留 | 真实 daemon + Web 验证消费后不残留、目标竞争和重连收敛 |
| P2 工具面、Skill、子代理权限 | 保留 | 可见性、预检、执行和拒绝回执共用事实；没有角色隐式提权 |
| P3 原生 Step/attempt、用量、按需资源 | 保留 | 主线/子代理复用同一边界；用量不成为执行门禁 |
| P4 Agent 身份、Job、交互恢复、取消和转交 | 保留但冻结  | 原生独立代理的显式继续已从 App Server 与 Web 验收；父子取消、未知副作用与平台资源结算仍按各自门禁核对 |
| Magi CLI | 已废弃 | 无后续交付项；控制合同由 daemon/App Server 验证 |
| 外部 Executor | 只预留通用接口与历史证据读取 | 具体实现不属于当前目标，是否接入后续另议 |
| 独立移动端/Connect 规划 | 已撤下 | 手机和平板沿用 Web 兼容，不开发独立客户端 |
| 现有插件兼容性 | 保留 | 不支持能力明确拒绝；禁用/升级正确结算 |

## 继续修改的门槛

每个后续切片必须只解决一个用户可观察的故障或一个生命周期不变量，明确替换的旧路径、删除的重复事实源和最小回归入口。不能以新增类型、schema 字段或定向测试的数量代替产品验收；不能在 P4 尚未完成退出条件时继续展开 P5。

本轮已核对以下事实链；这不是全部五万行的逐行正确性证明：

| 事实 | 唯一所有者与入口 | 审计判断 |
| --- | --- | --- |
| 输入接纳、绑定、消费 | `SessionStore` 的 Session 事务；`state.rs` 从 Inbox 投影队列；时间线读取原 Step 消费事实；Step 只记录实际冻结消息中的 request 身份 | 旧 queue 文件只读迁移；补充接纳、请求在途、响应确认和未知结果分别展示；压缩/重启不会把未消费输入摘要掉或误记为消费；保留 |
| 队列编辑/删除/引导 | `input_service.rs`，HTTP 与 App Server 共用提交锁及原 Input 事务 | 编辑只更新 pending Input 的内容和 revision，保留创建指纹、消息身份、时间和 FIFO；移除点击编辑即删除再提交的旧流程。同内容重试只读回执，领取或其他编辑先提交时返回冲突，UI 保留草稿并在编辑栏显示原因。编辑保存会按注册工作区解析真实归属，兼容原输入只带 workspacePath、投影带回 workspaceId 的情况 |
| 队列 Web 投影及工作流提交 | `messages.svelte.ts` 保留服务端 Input 元数据；`web-client-bridge.ts` 透传选择的工作流 | 本轮逐组审查复现并修复 normalizer 丢弃 revision、停止并改做来源/错误、workflow/plugin 元数据及执行器拒绝原因；无正文工作流也沿原 Turn 入口提交，没有新增业务状态或队列路径 |
| 审批和提问 | canonical 原工具 item；`interaction_service.rs` | 内存注册表交付活跃调用，恢复读取原事实；保留取消/权限边界 |
| 会话删除与提问等待 | `ConversationRegistry::remove_session` 与 `UserQuestionRegistry::remove_session` | 删除会话时先取消并丢弃该会话所有进程内提问等待，断开发送端；其他会话的提问不受影响，避免删除后仍保留孤儿交互 |
| 删除与队列读取竞争 | `input_service.rs` 各队列入口与 `ApiState` 会话生命周期锁 | 复现队列读取/删除已经在 Turn 提交锁前排队、会话随后删除的窗口；原实现拿到锁后直接从不存在的 Session 构造快照而 panic。现在所有读取、编辑、删除、重试和引导入口在获得提交锁后重新核对 Session 归属，返回 `SessionNotFound` 或作用域冲突，不再访问已删除事实；新增回归覆盖读取与删除两种等待路径 |
| 队列版本与补充重试 | `input_service.rs` 的版本/CAS 与原 canonical 用户 item | App Server 保留协议规定的正数版本检查，共享服务补齐 HTTP 零版本拒绝并拥有编辑竞争和绑定后的重试核验；保留 schema 规定的旧 HTTP 可选版本兼容。GUI 删除、补充和重试必须传入点击时的版本，补充不再从最新快照代填。绑定/首次消费的版本变化与原 `route=steer` 同时核对，拒绝编辑前旧版本、普通下一轮已领取输入伪装成功；不新增状态或持久字段 |
| Goal 控制 | `goal_service.rs` 与 SessionStore 原 Goal/control revision | 两种传输共用，不另设目标状态；预算已移除 |
| 派发理由与能力 | `agent_delegation.rs`、共享动态提示、原预检/注册；工具资源按 caller scope 解析 | Schema 与直接入口重复检查同一解析器属于边界防绕过；可继续委派的子代理仍按自身 Task 隔离进程。运行期允许范围明确区分继承与封闭空集合，目录和内置执行共用 Task/Skill 的交集及显式拒绝，不由来源标签推断权限；MCP/插件共用副作用和访问档判定，缺执行器不能发布 MCP 模型定义 |
| activation/转交 | TaskStore；`agent_service.rs` | canonical 绑定来源、不复制转交所有者；终态通知优先解析 Agent 原 Turn，不由当前主线覆盖；跨轮事件使用 activation 的会话归属 |
| 独立代理继续 | 原 canonical Agent Turn + TaskStore 原转交子树；`agent_service.rs` | 处理原交互后显式继续；同 Task/Thread 新 Turn，保留 Skill、目录、审批领取和旧中断记录；只重开 canonical Turn 为 `Interrupted` 的任务，子树中已经 `Completed`/`Failed`/`Cancelled` 收口的后代只保留原失败证据，不挡住其他中断部分，也不重派旧任务；operation 重试只读回执；终态回调与恢复提交共用原 scope 的 restart 锁 |
| Continue 恢复代际 | TaskStore 的 retry_count 与原 checkpoint 事务 | 主线恢复现在和独立代理继续一样递增持久代际；不以毫秒时间代替执行身份。先用同毫秒终态回归复现旧通知误领新上下文，再修正原恢复入口；写盘失败不推进状态或代际 |
| 计划工具 nullable 身份 | magi-plan 的原参数归一化与 Plan/Goal CAS | 真实 GLM 暴露字符串 `"null"` 导致首次计划失败；只把服务端生成的 Plan/Goal 身份空标记归为缺省，调用方可定义的 itemId 保持原合同。现有计划、Goal 绑定及 revision 校验继续拒绝错误更新；修正旧失败文案中“不要传 planId”与 schema 要求显式 null 的冲突 |
| 原生模型调用 | 共同 `StepModelClient` 冻结请求和 attempt | Task/Goal 主线与子代理走同一任务模型/工具循环；Session conversation profile 走独立的输入协调循环；两者不再额外包一层内存 Conversation/Turn 状态 |
| Job | 原 canonical item 加输出 artifact | 没有第二个 Job 调度器；启动、取消和完成写盘失败保留原句柄及完整待提交终态，重试只补交原结果；在途结算共用操作登记，已停止进程清除旧 PID 句柄；早退、异步完成和显式停止均先回收受管进程树并等待 stdout/stderr 读取线程，读取未结束时保留原句柄和资源屏障；输出写出失败保留未知结果，不把重试误报成成功。不能从 Shell 退出或管道关闭推断派生进程已释放 |
| Windows 进程结算 | `magi-process` 原 Windows Job Object | `TerminateJobObject` 返回成功只代表接纳终止请求。现在先取得原 Job 成员的等待句柄，终止后共用原 2.5 秒期限核对活动计数归零和进程对象退出信号；查询失败或超时返回原错误，不提前释放写入边界。复用原后代回收和所有者隔离回归，补上 Windows 根进程先退出、后代仍运行及同步/异步结算断言，没有新增进程登记表 |
| 根进程回收后的资源归属 | `magi-process` 原受管句柄与 owner 登记 | 真实 Unix Shell 回归先复现根进程经 Tokio `wait` 回收后 PID 消失、显式终止却遗漏仍存活后代。异步句柄现在保留启动时的原进程组身份；同步/异步 `wait` 和 `try_wait` 只观察根进程，不提前注销 owner，显式结算成功才注销，失败返回时仍保留原登记；已成功结算的 Unix 句柄不重复向旧进程组发信号。新增一项真实进程回归覆盖显式终止、Drop 和 owner 取消，复用同步已退出根进程回归核对原登记；没有新增任务状态或进程调度器 |
| Job 重复停止与终态读取 | `process.rs` 按 canonical Job 终态区分已结算与 `OutcomeUnknown`；模型预检遵循已发布的 Job/terminal 合同 | 已结算 Job 的重复 kill 读取原回执并保持 revision/退出码/完成时间；读取 Running 快照或匹配句柄后被另一操作结算时，句柄缺失会重读原 journal，不把竞争误报为进程失联。结果未知或未提交仍拒绝伪造 `resources_settled`。修复模型预检错误强制 terminal_id、拒绝合法 job_id 的漂移，同时移除省略 action 时按身份猜动作的旧校验分支；未引入新状态或写入路径 |
| Job 终态与继承管道 | `process.rs` 的原进程树回收及共享输出结算入口；原 canonical Job item | 先复现退出码 0 被误记 `outcome_unknown`：后代继承管道导致读取线程无法结束，而旧路径先等待输出后才回收后代。现将启动期早退与异步退出合并为先回收进程树、再排空并同步输出、最后提交原终态；实际回收/读写失败仍按未知处理，没有新增持久状态 |
| Executor 能力与证据 | 适配器声明、Step 冻结、原回执投影 | 保留现有插件/GPT Web 能力检查与历史读取；具体外部执行器已撤下。本轮修正 JSON-RPC 本地探测桥误报 native tools/checkpoint 的能力声明，探测桥现在明确发布 text-only、无工具执行合同，并在发送前拒绝工具请求和无法转发的对话历史，避免把仅有 `prompt` 的探测请求误当成完整会话调用 |
| 插件工作流接纳、恢复和最终输出 | 原 TaskRuntimePayload 固定包摘要；原 pendingDispatch、TaskStore 与 PluginManager 共同核对；TurnService 原请求重放 | 接纳与实际调度都检查 LocalWorkflow 类型，主线 coordinator 保留所有权；摘要随原任务 output_refs 保存，移除仅保存阶段标记的旧结果。接纳重试先读原事实，排空/停用不阻止读回执；新请求仍检查当前准入。磁盘恢复后包摘要不变，请求与任务负载不一致时在创建 Thread 前拒绝；旧检查点不得交给新包 |
| 工作流模型请求意图与正向能力 | `workflow_execution` 的唯一宿主模型入口；TaskRuntimePayload 的原 `pending_action` | `model_request` 只能使用宿主当前模型；已授权 `input_refs` 与当前工具目录的显式子集实际进入模型请求，模型结果结构化回灌，工具调用仍须后续宿主动作。发送前待决动作保存完整 request、工具定义、引用和 Step 身份；失败/重载保留请求未知证据且不自动重发。任意模型选择和引用正文自动装配仍未交付 |

本轮另补齐运行保护边界：transport 安全上限现在按 canonical Agent 执行树累计，而不是按当前物理 Turn 计数。子代理独立 Turn、显式 Continue、重启恢复和 transport retry 都沿不可变父 Turn 来源链归入同一执行树；新的顶层用户 Turn 仍拥有独立上限。这样恢复或拆分代理不会绕过安全上限，也没有引入用户任务预算。

本轮同时修正了能力目录与执行预检的边界漂移：taskless 主线的目录现在按同一 `model_call_scope.allows` 规则识别根工具，Git 查询和需要审批的 Git 操作不再被错误标成不可用；`agent_spawn`、`memory_write` 等仍明确返回范围拒绝。新增目录回归覆盖该主线与委派差异。

委派决策合同现在还拒绝在非 `parallel_work` 目的中携带 `parent_work`；只有确实说明父任务同时推进的独立工作，才允许声明并行收益。这样角色名称、长任务或“需要复核”不会借结构化字段伪装成并行理由。

本轮移除了 Task worker 仅用于包裹整段 IO 的内存 `Conversation`、`TurnDriver` 和 `Turn` 三个重复状态层；运行时信号改由 `ConversationRegistry` 的按任务 FIFO 统一承载，TaskStore/SessionStore 继续拥有生命周期事实。后续等待边界审查先复现输入关闭后 `context_request` 空转至超时并返回成功，再让原等待接口明确区分关闭与开放空队列；任务关闭、会话删除都返回 `task_interrupted`，父任务结束且没有已投递答复时返回 `parent_not_running`，正常补充交付与唯一名额重新准入保持。撤下只在测试中调用的 `take_task_signals_or_close` 包装，关闭仍走生产 `take_task_inputs_or_close`。复用原夹具覆盖正常答复、任务关闭、会话删除、父任务结束及答复先到后父任务结束五种交错；对话运行时 655 项、Registry 15 项及全目标 Clippy 通过，没有新增持久状态、协议字段或调度器。真实 daemon/App Server 控制验收在通道关闭修复后再次通过 93 次本地 Provider 请求；父任务结束分支由上述定向回归覆盖。随后当前源码的 daemon + Chromium 队列验收通过 24 次本地 Provider 请求，重新确认引导可点击、队列消费无残留、编辑竞争及重启后原代理继续；Web check 零错误零警告、协议一致性和 diff 检查通过。剩余逐组评审只处理状态组合和夹具复用，不推进新客户端或 Executor 功能。

完成结果中转也已收敛：移除 `EventBasedResultReceiver` 的缓冲、可变 Sink、重复去重和 panic 换绑逻辑，以及 Runner 测试专用结果轮询。Dispatcher 必须在构造时绑定完成交付目标，daemon 与真实 API 夹具直接交给原 TaskCompletionNotifier；绑定通知器同时固定来源与结果交付。旧六项 receiver 专用测试随废弃层撤下，迟到租约、写盘失败、Runner 异常与终态幂等仍验证实际提交入口。receiver 实现与专属测试删除约 450 行，未增加业务状态或恢复路径。完整运行时回归发现旧取消归档夹具未标明 root 归属，已同时验证有归属可归档、无归属保留当前链，不放宽迟到子代理通知的隔离条件。清理后 API 958 项（2 项忽略）、对话运行时 655 项、daemon 134 项及三者全目标 Clippy 通过，真实 daemon/App Server 控制验收再次通过 93 次本地 Provider 请求。插件版本固定、缓存引擎准入与排空撤权链路也已复核，插件系统 18 项单元和 54 项集成测试、进程 15 项及工具运行时 309 项回归通过；这些是本机证据，不关闭 Windows 原生门禁。

初始 assignment 与后续 agent_send 走同一 FIFO，注册失败不保留消息；执行栈在成功、失败、取消或 panic 时关闭输入，尚未派发就结束的代理由既有终态资源回调清理。运行中补充在上下文与 FIFO 提交成功后，也须通知 TaskStore 的原等待边界：仅通知 FIFO 会让阻塞于 agent_wait 的接收方等到子任务变化或超时，无法及时处理补充。本次复用原发送夹具先复现缺失唤醒，再沿现有 notify_runtime_change 修复，写盘失败仍不提交补充或唤醒；工具批次 95 项、对话运行时全目标 Clippy 及真实 daemon/App Server 控制验收（93 次本地 Provider 请求）通过。单次派发继续由原准入和 TaskStore 原子租约保证，不增加新调度器或状态表。移除旧内存驱动的切片净减少约 700 行代码与旧专用测试。

本轮追加的存量审查修复了一个转交代理与主线换轮之间的竞态：子代理终态通知沿父 root task 关联收口时，若主线已经启动新的 Turn，旧通知不能再归档新 Turn 的 active execution chain。归档前现在核对当前 canonical Turn 中的 task 身份；只有仍属于被收口 root 的 Turn 才能释放会话执行归属。`transferred_agent_completion_preserves_the_next_mainline_turn` 定向重复 8 次及 API 958 项全量回归均通过。

## 当前验证证据

跨平台证据须核对命令日志：[首次 CI #480](https://github.com/MistRipple/magi-code/actions/runs/39285862787) 虽为 success，Windows 步骤实际包含测试与编译失败，PowerShell 中间命令的非零退出码被后续成功覆盖，不能作为门禁通过证据。现已启用原生命令失败即终止，并修复包清理用字符串比较 `/` 与 `\`、误删当前已安装版本的生产缺陷；重启和升级共用的原清理入口改用宿主 Path 比较。进程夹具改为读取退出信号后，新的失败门禁进一步复现了生产结算缺口：Job 活动计数清零仍早于进程对象 signaled。原 Job 终止入口现先取得成员等待句柄，提交终止后共用原 2.5 秒期限核对活动计数和成员退出信号；枚举失败仍提交终止，但不能宣称结算成功。默认 Shell 的 cwd 夹具另从旧 cmd 语法改为 PowerShell。复用原升级/重载、所有者隔离与 cwd 回归，没有新增业务状态或测试套件。本机插件单元 18 项及集成 54 项、进程 15 项、cwd 定向回归和 Windows 目标进程全目标 Clippy 通过；[后续 CI #484](https://github.com/MistRipple/magi-code/actions/runs/39342539875) 已实际通过 Windows 进程 12 项、插件 Worker 12 项、插件单元 18 项和集成 54 项及 Shell cwd；继续执行时暴露 API 测试无条件引用 UnixStream 的编译失败。现只将原 Unix socket 夹具限定到 Unix，跨平台槽位身份/归属测试继续保留；一并按实际使用平台限定 TaskStore 的 File 引用、Unix 进程扫描函数和工具测试 MCP 导入，清理 Windows 条件编译告警。API 本机槽位 9 项、工具 MCP 9 项及 API/daemon all-targets check 通过；本机 Windows API 交叉检查因缺少 MSVC C 头文件在原生依赖构建阶段失败，不能计为通过。[当前 Windows CI](https://github.com/MistRipple/magi-code/actions/runs/39348779366) 已实际通过路径引用 7 项、文件定位 2 项、进程 12 项、插件 Worker 12 项、插件单元 18 项及集成 54 项、Shell cwd、Worker preflight 和 `magi-daemon --all-targets`；Linux 发布前检查也已通过，当前三平台 CI 全部成功。

[Windows 后续回归](https://github.com/MistRipple/magi-code/actions/runs/39348779366) 已越过 UnixStream 编译错误，并通过路径引用 7 项、文件定位 2 项和 daemon 全目标检查。前一轮失败来自既有 `HostPath` 合同会在安全时简化 Windows 的 `\\?\` 前缀，旧断言却直接比较 `std::canonicalize` 的 verbatim 路径；修正工作区、文件列表/解析和文件定位断言后分别检查原生路径与 displayPath，保留路径引用与工作区越界校验。另按实际 Unix 用途限定 MCP 夹具的令牌字段。本机路径引用 7 项与文件定位 3 项回归通过。

[Windows Job API 回归](https://github.com/MistRipple/magi-code/actions/runs/39348779366) 已从原始日志核对 8 项通过：原 6 项 Unix Job 夹具及停止并改做夹具改用宿主原生 Shell，连同原公开脱敏测试复用执行，测试用例总数未增加。真实退出码 0/7、等待无模型轮询、启动期取消、写盘失败重试、跨轮输出/重载及停止后写入均有断言；输出夹具等待实际输出与自然退出，避免瞬时 Provider 提前取消尚未退出的 Shell。复审另先复现 PowerShell 静态 .NET 调用被误报为 PATH 依赖，再沿原扫描器排除静态成员表达式；本机 Shell 54 项和工具运行时/API 全目标 Clippy 通过。该证据来自 API 进程内的生产装配与真实 PowerShell 进程，不冒充打包 GUI 或跨 daemon 网络入口验收。

以下按产品合同汇总已取得的证据；旧的逐轮测试计数与中间失败记录归入 Git 历史，不再重复追加。测试只覆盖对应源码切片，不能从某次局部通过推断所有存量改动已评审。当前仍保留以下实现边界：

- 工作流的 `use_tools` 与持久 TaskPolicy 的 `no_tools` 共用工具门禁；模型请求、工具子集和授权引用在发送前保存到原 pending action，失败/重载不自动重发。结算子任务前与核心决策返回后复核原 Task/lease/Turn，迟到完成、检查点和等待用户不能产生新事实。真实 daemon + Worker 已覆盖检查点、结构化结果、停用/升级/重启后的原回执保持；取消竞态由定向夹具覆盖。
- Skill 工具目录删除同名缓存，注册表原子替换；失效、歧义、撤销和损坏配置都明确拒绝。主线/子代理每个新 Step 重新解析当前内置、Skill、Browser 和 MCP 定义，冻结请求后的撤权仍在执行前拒绝。真实 daemon 控制验收已覆盖同一 Turn 内 Skill 收紧、放宽与清空。
- JSON-RPC 探测桥拒绝无法转发的历史、图片与工具请求；显式输出上限不支持时返回 `executor_output_limit_unsupported`。GPT Web 与插件返回空完成结果时在同一能力响应边界拒绝；已提交网页消息不重发，原槽位和远端上下文保留为待核对。
- Continue 不再从 Worker `finish` 阶段、中间输出或租约拼出 Completed。真实执行结果通过原完成门提交，恢复与运行投影共用 TaskStore 的状态和归属；后台会话快照不覆盖已保存的管理操作，删除全量刷盘回退和重复写入入口。
- 插件卸载在提交包状态前检查全部运行租约；取消请求未结算时保留状态和包并返回冲突，租约释放后才允许删除。停用/升级仍通过原 PluginManager 排空，不让缓存引擎重新接纳工作。
- 测试资源只由对应 harness 的 `TempDir` 持有，已移除按全局前缀扫描删除其他验收证据的分支和重复手工删除。daemon 夹具不再选择未注册的占位 Skill，也不把仍可恢复的失败链强制归档；生产的失效 Skill 拒绝与失败分支保留规则不变。

修复队列版本/CAS 后，真实 daemon + Chromium 的队列验收通过 24 次本地 Provider 请求，含引导请求的 expectedRevision/expectedTurnId、绑定/消费/重载后的幂等回执。Job 输出回收修复后的控制验收通过 96 次本地 Provider 请求，审批重启验收通过 3 次请求。打包 Desktop 与真实模型结果分别见下表；本地 Provider 不替代真实上游模型，macOS 不替代 Windows。

| 合同 | 已取得的证据 | 覆盖边界 |
| --- | --- | --- |
| 排队、引导、输入消费和 Goal 公平性 | 本轮检查点修复和旧刷盘入口清理后 `test:daemon-input-queue` 再次通过，真实 daemon + Chromium 使用 24 次本地 Provider 请求；覆盖引导可点击、重载保留输入、原 Turn 下一步消费、FIFO、发送后队列无残留、无重复用户消息、停止并改做、无预算 Goal；新增排队消息保存编辑、原 Input 身份/FIFO 保持、领取竞争 409 冲突和草稿保留 | 主线等待/冻结/响应确认回执均有断言；Task 完成与 canonical Turn 收口分别核对；编辑使用 SessionStore revision/CAS，点击编辑不再先删除原输入 |
| Web 队列元数据完整性 | 本轮先用 Bridge 回归复现 `unsupported_executor` 与 revision 被丢弃，再修复同一投影入口；Bridge、agent API、composer policy 与 Web check 通过。修复后的 `test:daemon-input-queue` 再次通过（24 次请求），并从 Chromium 实际请求中断言引导携带有效 expectedRevision/expectedTurnId | Bridge 核对 workflow/plugin 选择、停止来源/错误和 revision 完整保留，以及无正文工作流提交；真实 UI 核对原引导、重连、队列消费及独立代理继续。未将插件工作流完整执行声称为本次验收范围 |
| 队列原子编辑 | API 的 3 项编辑回归及 SessionStore 的 2 项输入事务回归通过，覆盖同内容双客户端重试、过期修改、非法执行身份、领取/撤销竞争、路径/ID 作用域归一化及写盘失败回滚；删除也要求 GUI 提交点击时的 Input revision，Bridge 回归先复现旧版本可被最新快照替代的问题后已修复；Web check、Bridge、composer policy、协议检查通过；真实 daemon + Chromium 原队列夹具通过（24 次本地 Provider 请求） | 保存实际收到 200，原消息被后台领取后的保存收到 409；旧版本编辑或删除都不能覆盖新内容，冲突原因直接留在编辑栏且草稿仍在，不转为新 Turn。原始创建请求重试不覆盖编辑后的内容；没有新增队列事实源或专用调度器 |
| 会话删除后的交互回收 | 本轮用持久提问夹具复现主记录删除后进程内注册项仍残留；沿原 Registry 删除入口补齐清理后，API 会话删除 6 项、提问 6 项、运行时提问 7 项及 Registry 15 项通过 | 有 Task 与无 Task 的等待方均断开，其他会话仍能答复；不新增取消状态或恢复队列。清理旧强制派发代理测试夹具后，daemon 134 项及 daemon check 通过；通用任务完成断言更名，避免被误读为真实双代理覆盖 |
| 工具权限与实际调用一致 | 本轮复现 Task 白名单/明确禁止未过滤 Skill 自定义工具定义、`no_tools` 任务在 Browser 能力刷新后重新收到工具；初始定义与每轮刷新后共用 `constrain_task_tool_definitions`，复用原 `ToolExecutionPolicy` 的 Task 范围解析。对话运行时 655 项通过，目标 crate Clippy 通过；真实 daemon 控制验收再次通过 96 次本地 Provider 请求 | 执行预检原本会拒绝这些调用，本轮修复的是定义与执行不一致，没有放宽授权。主线/子代理 × MCP/插件 × 只读拒绝、受限批准、用户拒绝、完全访问、审批期间撤销 Skill，及 Task 白名单/显式禁用、封闭空集合和缺 MCP 执行器不发布定义继续保留回归；`no_tools` 不因宿主重连或动态定义刷新重新提供工具 |
| MCP 跨进程调用与审批范围 | 本次 API 回归覆盖真实 stdio/HTTP MCP Turn、只读读取与跨 Turn/Session 的审批取消 | 本地 Provider；不能据此推断第三方工具注解真实或 OS 沙箱完备 |
| 独立代理继续与终态来源 | 真实 daemon/App Server 夹具先复现“已有失败后代挡住整个子树继续”，沿原 `continuation_sources` 修复后通过 96 次本地 Provider 请求；此前 API 959 项通过（2 项忽略）、API 全目标 Clippy 通过。本轮再次从 daemon + Chromium 验收 24 次请求（在 Job 输出回收切片前）。覆盖显式转交、父轮停止/换轮、SIGKILL、原问题/审批、两层子树、双客户端 operation 幂等，以及 GUI 的继续投影、失败 Task/Turn/恢复代际保持不变 | 只重开中断部分，不重派已失败后代或改写旧证据。原夹具的完成等待也改为最新主线 canonical Turn 已完成，避免文本先到、终态后到时提前取快照；没有新增协议、持久状态或恢复路径 |
| 失败代理部分结果与接管 | 新增 canonical 工具事实投影：失败/取消的 `agent_wait`、`agent_cancel` 与工作流回执从原 Agent Thread 读取文件写入、命令退出码和 `outcome_unknown`，按 Session/Task/Thread 过滤；真实 daemon 控制验收新增“子代理写入后 Provider 失败、等待/取消/重启均保留同一回执且不重放”，82 次本地 Provider 请求通过 | Task 终态只拥有失败原因，canonical Turn 仍是工具事实唯一来源；兄弟 Task、其他 Thread/Session 不会混入；完成态不重复叠加 canonical 活动 |
| 同毫秒恢复与计划输入修复 | 本轮计划 23 项、TaskStore 恢复 2 项、终态通知 3 项通过；定向 clippy、daemon check、协议与 Web check 通过。修复后的 `test:daemon-controls` 再次通过（75 次本地 Provider 请求），GLM 同一短复核复测完成 | 原旧回调的同毫秒碰撞在修复前明确失败，修复后拒绝；checkpoint 失败回滚代际与状态。计划空身份不能覆盖已有计划或绕过活跃 Goal；GLM 复测未再次选择计划工具，不能替代参数回归证明该输入分支 |
| 打包 Desktop 运行链路 | 当前源码重建的 macOS Apple Silicon 目录包通过完整 Electron DOM 验收（207 项、57 次本地 Provider 请求），新增真实后台 Job 启动、事件等待、持久终态与文件结果核对；此前恢复专项 44 项通过；此前 Git 漂移/冲突专项 37 项通过，相同检查也包含在完整 DOM 回归 | 普通对话不受 Git 异常阻断，写入失败无副作用且持久化明确未执行回执；代理卡片核对真实任务 ID、详情结果及成功/失败状态；工作区移除后运行 sidecar 不再周期性报未注册归属。不能外推为 Windows 或真实上游模型通过 |
| 真实模型与 Shell 分派 | `gpt-6-astra / low`、`deepseek-v4-flash-0731`、`glm-5.3` 与可用的 `kiro-claude-sonnet-4-5` 经实际 Provider + daemon/App Server 完成小修、缺陷修复、附件诱导、明确禁止和不可用角色接管；四模型均在明确要求双代理时创建并收集两个真实 Task（Claude 代理汇总 17、25 为 42）。独立模块与短复核场景允许主线直接完成，未把“可委派”误当成“必须委派”；GLM 修复 nullable 计划身份后复测成功且零代理。四模型随后各运行一次受控的真实子代理失败场景：每次只创建一个代理，收集 `model_invocation_failed` 终态后均由主线读取原文件并完成 42 的验收，未重复派发 | 首轮实测发现 `action=run` 携带占位 `job_id` 被误分派为 Job 查询；已按动作选择入口，31 项 Shell 回归及 2 项真实 Job 等待/取消回归通过。`kiro-claude-sonnet-4-5` 的基础矩阵、选择质量矩阵和失败接管均通过，证据分别保留在 `/tmp/magi-model-matrix-kiro-claude-sonnet-4-5-evidence.json`、`/tmp/magi-model-choice-kiro-claude-sonnet-4-5-evidence.json` 与 `/tmp/magi-failed-child-kiro-claude-sonnet-4-5-evidence.json`。Provider 当前不提供 `claude-sonnet-5-5`，直接探测返回 `model_not_found`；更大矩阵及重复评测仍未完成；非 Git 单写者限制仍保留 |
| Step、压缩与恢复 | 此前运行时回归保留同文输入、未消费图片、压缩/重启保护、Step/attempt、未知副作用和审批持久化断言；本轮新增 SessionStore 执行树安全上限回归，原主线审批 SIGKILL + Chromium 验收再次通过，3 次本地 Provider 请求且原删除操作只执行一次 | SessionStore 176 项通过；本次新增跨父子 Turn 上限测试；`magi-api` 当前全量单测 958 项通过、2 项忽略（本地性能基准与真实通道压缩包），此前磁盘耗尽的验证限制已解除 |
| 会话管理、工作区移除与 checkpoint | 本轮 SessionStore 176 项、daemon 134 项（含持久化 35 项）及 daemon check 通过；先复现旧后台快照撤销已保存重命名，再验证会话选择、重命名、删除和并发工具结果在重载后保持。原工作区离线、健康会话更新、重新注册后补交回归仍通过；此前工作区 API 19 项通过 | 投影捕获到提交共用持久化锁，canonical/state 锁在快照构造后释放；后台切片不写全局 current。移除旧全量刷盘 API 和专属阻塞测试，原有效断言改走生产增量入口；脏标记强制归属 Session，删除空切片回退全量保存的危险路径。写盘失败和并发新版本不被确认，离线工作区不写全局备用目录 |
| 取消与资源结算 | 本轮先用受控输出线程复现“停止在输出读取结束前返回成功”，修复后工具运行时 311 项通过（网络 smoke 忽略 1 项）、SessionStore Job 9 项、API Job 8 项与两 crate 全目标 Clippy 通过；Job 原进程树夹具扩为 14 种完成/失败/取消组合。真实 daemon/App Server 控制验收在修复后再次通过 96 次本地 Provider 请求 | 输出线程由原 ManagedProcess 持有；移除 watcher 独占读取句柄和单独排空路径，启动期、自然退出和显式停止共同先回收后代、再等待输出。超时保留句柄和原资源屏障，读写失败及重复停止保持未知结果。夹具分别等待 canonical 终态与原 journal 返回后的资源屏障，不把一次瞬时读取当作泄漏证据。没有新协议或持久字段；上述 Job 输出组合来自 macOS，Windows 通用进程结算和原 Job API 8 项已由 CI 原生运行通过，仍不替代打包 Windows GUI/Job 端到端验收 |
| 现有执行器兼容性 | 本次插件系统单元 17 项、生命周期 33 项及其余集成 21 项、bridge 265 项、GPT Web 62 项通过；重新构建当前源码的 macOS 目录包后，完整插件验收 167/167 项通过，包含 stdio 握手、工具执行、审批、取消、吊销、工作区移除/恢复和 sidecar 刷盘无归属错误。此前失败的网卡连接现已重试通过：经非回环 IPv4 访问真实 MCP 监听器，网络令牌返回 200、本机专用令牌返回 401。证据保留于 `/tmp/magi-runtime-plugin-recheck-evidence` | 只维护原生模型、GPT Web 和现有插件；旧 `codex_exec` 配置明确拒绝，不回退到 HTTP 模型。本次网络检查来自同机非回环网卡，验证了服务端网络鉴权，不外推公网路由或跨设备部署通过 |
| 静态检查与清理 | 本次 Web check（0 错误、0 警告）、Desktop check 与 146 项单测、daemon check、进程 crate 14 项、协议一致性、架构图和 diff 检查通过；workspace `cargo test --workspace --all-targets -- --test-threads=16` 全量通过，workspace Clippy 及本轮受影响 crate Clippy 也通过，并清理运行时、API、orchestrator、daemon 测试夹具中的冗余 clone、嵌套条件和测试模块布局告警；CLI、具体外部 Executor 和独立移动端入口扫描只剩历史说明及配置拒绝用例 | 本次 Web 构建仍有原 PURE 注释和 bundle 大小提示；Windows/macOS CI 平台专项已通过，Desktop 包为本机目录包，未签名，不等于已发布安装器 |

## 尚未关闭的验收

- 目标方案 §9.1 的历史边界 fork 快照仍是未交付实现项。当前 `AgentContextPackage`、`agent_spawn` Schema 与 `parse_agent_context_package` 只提供结构化摘要、约束、交付要求和按需读取的引用；后续 activation 可以检索自己的前序记录，但尚无从父历史合法边界创建冻结快照的入口及权限/来源核验。完成目标前必须补齐该合同与真实执行验证，不能仅凭现有 fresh 上下文和恢复回归将此项记为完成。
- 当前可用的 `gpt-6-astra`、`glm-5.3`、`kiro-claude-sonnet-4-5` 和 `deepseek-v4.1-flash-1017` 在同一 release daemon/App Server、独立 Git 工作区和 `low` 参数下，各重复执行六项固定任务两次，共 48/48 项通过、234 次持久 transport attempt。小配置、耦合缺陷、明确禁止和附件诱导共 32 项没有任何 `agent_spawn` 调用；明确双代理的 8 项各创建并经 `agent_wait` 收集两个真实 Task，最终结果为 42。独立模块 8 项均通过文件及 Node 断言，其中 Claude 一次选择两个代理，其余主线完成；未把可委派误设为必须委派。原 `deepseek-v4-flash-0731` 的 12 次失败保留，直接 Provider 探测确认 HTTP 400 `model_not_found`，不计为通过；当前模型列表提供新的 DeepSeek 名称后另跑了完整 12 项。原始 canonical 快照、工作区和证据分别保留于 `/tmp/magi-runtime-model-repeat-evidence.json`、`/tmp/magi-runtime-model-repeat-deepseek-evidence.json`，汇总含 daemon SHA-256 的 `/tmp/magi-runtime-model-repeat-summary.json`。该轮尚未覆盖大量日志研究、必要独立复核、受控失败接管的重复样本和统计性能比较；不能据此关闭全部选择质量验收。
- 本轮 Job 输出回收修复后从真实 daemon 产品入口通过 `test:daemon-controls`（96 次本地 Provider 请求），覆盖工作流检查点、Job 重复停止、交互恢复、双客户端幂等、独立 Agent Continue、两层子树、转交后取消/恢复、失败后代保留和 Goal 控制；本轮切片前再次通过 `test:daemon-input-queue`（24 次请求），覆盖编辑/CAS、引导消费、FIFO、重载、无预算 Goal 和停止并改做。它们补强了 P4 的本地跨进程证据，但仍不替代打包 Windows GUI/Job 端到端和真实上游模型验收。
- Windows 的自动化进程/插件/路径、共享 Job API 与 daemon 编译门禁已通过，并已核对 Windows 2022 原始日志；该部分不再列为待验收。现已将原完整 Electron DOM 夹具接入 Windows CI：构建真实目录包，沿 App Renderer 验收对话、队列、子代理卡片、审批、后台 Job、daemon/应用重启与优雅退出，并保留 JSON/原始日志。该夹具共用现有 CDP 驱动，移除 macOS 固定包路径、Unix 写入命令及 AppleScript 退出依赖，未增加产品 IPC 或第二套验收。Windows 目录包现已成功构建并推进至 Git dirty 场景；此前 ANSI 日志解析缺口已修复。本轮原始日志显示最终文本先于新 Session 的 Git 查询完成，立即取到空 DOM；夹具现等待同工作区新绑定的真实脏状态，而非把 Provider 文本到达当成 Git 查询完成。本机完整 207 项通过，Windows 修复后的原生结果仍待核对，不能外推。安装器安装/升级仍不在目录包验收范围。
- 大规模存量改动尚未全部逐组审查，不能把当前定向回归称为全部五万行已通过评审，也不能把 P4/P5 全部标为完成。后续只处理存量评审发现的状态/持久化缺陷及 Windows 和 Provider 恢复后的模型矩阵验收缺口，不新增客户端、预算系统或具体外部 Executor。

首次收敛移除了 CLI、重复内存驱动与具体外部执行器，并合并滚动文档；现阶段继续复用已有夹具和事实所有者，不以新增代码行数或测试数量衡量进度。
