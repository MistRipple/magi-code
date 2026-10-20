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

跨平台证据须核对命令日志：[首次 CI #480](https://github.com/MistRipple/magi-code/actions/runs/39285862787) 虽为 success，Windows 步骤实际包含测试与编译失败，PowerShell 中间命令的非零退出码被后续成功覆盖，不能作为门禁通过证据。现已启用原生命令失败即终止，并修复包清理用字符串比较 `/` 与 `\`、误删当前已安装版本的生产缺陷；重启和升级共用的原清理入口改用宿主 Path 比较。进程夹具改为读取退出信号后，新的失败门禁进一步复现了生产结算缺口：Job 活动计数清零仍早于进程对象 signaled。原 Job 终止入口现先取得成员等待句柄，提交终止后共用原 2.5 秒期限核对活动计数和成员退出信号；枚举失败仍提交终止，但不能宣称结算成功。默认 Shell 的 cwd 夹具另从旧 cmd 语法改为 PowerShell。复用原升级/重载、所有者隔离与 cwd 回归，没有新增业务状态或测试套件。本机插件单元 18 项及集成 54 项、进程 15 项、cwd 定向回归和 Windows 目标进程全目标 Clippy 通过；[后续 CI #484](https://github.com/MistRipple/magi-code/actions/runs/39342539875) 已实际通过 Windows 进程 12 项、插件 Worker 12 项、插件单元 18 项和集成 54 项及 Shell cwd；继续执行时暴露 API 测试无条件引用 UnixStream 的编译失败。现只将原 Unix socket 夹具限定到 Unix，跨平台槽位身份/归属测试继续保留；一并按实际使用平台限定 TaskStore 的 File 引用、Unix 进程扫描函数和工具测试 MCP 导入，清理 Windows 条件编译告警。API 本机槽位 9 项、工具 MCP 9 项及 API/daemon all-targets check 通过；本机 Windows API 交叉检查因缺少 MSVC C 头文件在原生依赖构建阶段失败，不能计为通过。最终 Windows/全 CI 仍须核对，不能把前半段通过外推到整条流水线。

[Windows 后续回归](https://github.com/MistRipple/magi-code/actions/runs/39344122888) 已越过 UnixStream 编译错误，但路径引用 7 项中有 4 项失败：既有 `HostPath` 合同会在安全时简化 Windows 的 `\\?\` 前缀，旧断言却直接比较 `std::canonicalize` 的 verbatim 路径。修正工作区、文件列表/解析和同类文件定位断言，分别检查原生路径与 displayPath，保留路径引用与工作区越界校验；Windows CI 复用原文件定位用例补充执行，不增加测试套件。另按实际 Unix 用途限定 MCP 夹具的令牌字段。本机路径引用 7 项与文件定位 3 项回归通过；修复后的 Windows 原生结果仍待核对。

本次补齐工作流工具开关合同：派发时的 `use_tools` 与持久 TaskPolicy 的 `no_tools` 共同约束工具调用、子任务派发及模型工具子集，拒绝发生在写入新动作意图前；保存检查点和纯文本完成仍可执行。工具关闭状态沿原派发与恢复负载持久化，不引入新的权限字段或执行路径。当前对话运行时 658 项、API 959 项（2 项忽略）、SessionStore 177 项及工具运行时 311 项（1 项网络 smoke 忽略）通过；fmt、受影响运行时/API/daemon 全目标 Clippy、daemon 等五个 crate 的 check、Web check（零错误零警告）及 release:guard 通过。当前源码重新构建的 `test:daemon-controls`、`test:daemon-input-queue` 和 `test:daemon-approval-recovery` 分别通过 102、24、3 次本地 Provider 请求，覆盖工作流、原代理继续、转交、Job 结算、队列编辑/引导及审批后一次执行。工具开关拒绝由定向运行时夹具覆盖，不把普通控制验收描述成同一拒绝场景的跨进程证明；Windows 原生和更大真实上游模型矩阵门禁仍保留。

随后完整 daemon 回归首次出现 7 项失败：6 项会话/记忆/恢复夹具选择了未注册的占位 Skill；另 1 项仍要求失败后归档具有可恢复分支的执行链。已删除通用 daemon 请求中无关的假 Skill 选择，并让原恢复夹具分别核对 canonical 失败终态、Coordinator 运行占位释放、原失败分支可继续和下一条消息正常完成；不修改生产的失效 Skill 拒绝或失败分支保留规则。修正后 daemon 全部 134 项、daemon 全目标 Clippy、fmt 与 diff 检查通过，没有增加测试数量、持久状态或恢复入口。

本次工作流正向回归补齐授权引用、工具子集与结构化结果的真实传递，并核对发送时原待决动作已保存完整请求和 Step 身份；Provider 失败后的 TaskStore projection 写盘/重载保持原意图，再次运行不询问核心或重发模型。移除已排序去重后重复检查工具名称的分支，工作流夹具临时状态目录改由自身 `TempDir` 回收。随后取消复审先复现两处执行资格缺口：Turn 已关闭但 Task 租约未撤销时，待决子任务结算会把工作流重新排队，核心返回的迟到完成也会被接受。现在结算前和决策返回后都复用原 `TaskExecutionGuard` 核对 Task/lease、canonical Turn 与绑定；正常工作流、已完成子任务回执及转交归属保持，迟到的完成、保存检查点和等待用户不产生新事实。工作流 26 项、对话运行时全部 657 项、插件单元 18 项、调度 65 项、Step 5 项、daemon check 和运行时全目标 Clippy 通过，协议及 Web check（零错误、零警告）通过。当前源码的 `test:daemon-controls` 再次通过，共 98 次本地 Provider 请求；真实插件 Worker 的授权工具子集、结果回灌、Step、升级、停用和 SIGKILL 重启后原回执保持，未重复调用模型。取消交错由定向夹具覆盖，不将普通控制验收描述为真实 Worker 的同一竞态复现。同步修正作者指南、插件状态及验证文档的旧拒绝描述；未新增状态、客户端、Executor 或任务预算。以上是本地 Provider 的跨进程证据，不关闭 Windows 或真实上游模型矩阵门禁。

本轮 P5 能力入口复审复现并修复了 JSON-RPC 请求未按文本能力拒绝图片/工具、仅转发 `prompt` 却静默丢弃 `messages`，以及模型接口带输出上限的默认实现静默降级为无上限调用。JSON-RPC 发送前和 BridgeDispatchRuntime 发送/交付边界复用原能力校验；探测端仍保留可解码的异常响应供原 cutover 诊断，但对无法转发的历史在 transport 前明确拒绝。插件空完成结果也按同一响应合同拒绝。输出上限必须由适配器显式覆盖，否则返回 `executor_output_limit_unsupported`；压缩夹具显式接收上限，撤掉对默认降级的依赖。bridge 265 项、对话运行时 655 项、插件单元 18 项、插件集成 54 项、Skill 20 项、API 958 项（2 项忽略）和 daemon 134 项通过，受影响 crate 全目标 Clippy 通过。当前源码重建后的 `test:daemon-controls` 通过 93 次本地 Provider 请求；一次中间源码的重启编译失败不计通过，最终重跑覆盖工作流、Job、交互恢复、独立 Agent Continue、转交子树及无预算 Goal。该切片不新增 Executor、客户端或任务预算，Windows 原生运行与真实模型扩展矩阵仍未关闭。

随后复审 Skill 热更新边界：工具目录原有的 daemon 级缓存会在同名 Skill 收紧或移除后继续发布旧清单，已删除该重复缓存路径，目录每次从当前注册状态和能力快照生成；原生执行在选定 Skill 缺失、歧义或运行时不可用时明确失败，不降级为无 Skill 的全工具面。新增同名收紧/移除、失效 Skill 不请求 Provider 的回归；对话运行时 658 项、API 959 项（2 项忽略）、daemon 134 项、相关 Clippy 及当前源码 `test:daemon-controls`（99 次本地 Provider 请求）通过。该修复未增加持久状态、协议字段或客户端；Windows 原生运行和真实上游模型扩展矩阵仍未关闭。

继续复审发现 Skill 重载原先按 `clear` 后逐项注册，且 `skill_apply` 在解析与读取之间遇到撤销会 panic；现在采用单写锁原子替换，撤销返回正常失败回执。损坏或不可读的 `config.json` 会记录警告并跳过 Skill，不再静默继承无限制工具面。并发撤销回归、Skill 20 项、加载器 7 项、运行时 656 项、API 959 项（2 项忽略）、相关 Clippy 及当前源码 daemon 控制验收（102 次本地 Provider 请求）通过。没有新增持久状态、协议字段或客户端；Windows 原生运行和真实上游模型扩展矩阵仍未关闭。

继续核对原生 Step 后，发现 Turn 内仍只刷新 Browser/MCP，普通内置与 Skill 自定义定义会沿用上一步快照。现在主线和子代理的新 Step 都调用原派发目录解析器，删除激活后单独过滤工具列表的实现及三项旧专用测试；Goal 生命周期工具保留宿主归属，Task 显式禁止仍生效。真实 daemon 验收覆盖同一 Turn 内 Skill 范围收紧、放宽和清空，冻结请求后的撤权阻止实际调用，后续 Step 使用新定义；完整控制验收通过 103 次本地 Provider 请求，队列与审批恢复分别再次通过 24、3 次请求。运行时 655 项、API 959 项（2 项忽略）及相关 Clippy 通过。API 中间回归暴露启动清理脚本按通用前缀删除其他测试证据；独立复现后移除整个跨任务临时目录清理分支，验证旧文件和工作区均保留，再次全量 API 通过。没有新增状态、协议字段或调度器；以上仍不关闭 Windows 实机和真实上游模型矩阵门禁。

本轮继续复核 GPT Web 适配器：其发送与返回现在也经过同一能力响应校验。空正文、空 thinking、无工具调用的完成结果在适配器边界明确拒绝；已经提交的网页消息不重复提交，Web slot 和远端上下文保持占用，交由上层按“结果未确认”处理。`magi-web-model` 62 项回归、bridge 265 项回归和相关 Clippy 通过。该修复不改变 GPT Web 的“网页拥有上下文、Magi 只发送本轮原文”合同。

本轮同时收敛了测试夹具的临时资源所有权：移除按宽泛文件名前缀扫描并删除其他验收证据的全局清理；Turn harness 的 runtime、job-output、worktree 和 Git fixture 统一放入该 harness 独占的 `TempDir`，最后一个 clone 释放后自动清理。删除 25 处重复的手动工作区删除，新增 clone 生命周期回归；turn harness 169 项（1 项性能用例忽略）和 API Clippy 通过。该清理只影响测试资源，不改变 daemon 持久化或产品执行路径。

恢复事实源复审发现并复现了旧 `finalize_terminal_worker_branches` 路径：有活跃租约且仅持有中间 `output_refs` 的 Running Task，因 Worker 阶段为 `finish` 被 Continue 直接改成 Completed。现已删除该完成推断入口、专用 lease 测试和 stage/snapshot 终态判定；真实成功仍只由执行结果进入原完成门。Continue、Agent 列表、终态保留与运行投影共用 TaskStore 状态/mission/root/当前拥有方的恢复判定，旧 `finish` 快照不再屏蔽 Failed，也不把 Running 变为完成。复用原恢复和运行投影夹具核对状态边界，不新增持久字段、备用写入路径或预算。 当前恢复 4 项、运行投影 16 项、API 958 项（2 项忽略）、对话运行时 655 项、daemon 134 项及三者全目标 Clippy 通过，真实 daemon/App Server 控制验收通过 93 次本地 Provider 请求。首轮完整 API 回归发现原转交夹具在主线 durable 终态到归档之间提前取所有权快照；现改为订阅并等待主线终态事件后再记录基线，保留原代理完成前后归属完全一致的断言，修正后全量回归通过。该修正只调整测试观察边界，不修改生产终态或资源释放语义。

本轮 Job 停止收敛先复现了顺序重试和旧 Running 快照竞争；真实 daemon 验收又暴露模型预检仍强制 `terminal_id`，在修复后 `test:daemon-controls` 通过 93 次本地 Provider 请求。复用原夹具核对 Completed/Failed/Cancelled 各启动一次、停止两次，SIGKILL 后原 Job 与停止回执不变且无模型重放。SessionStore 全部 177 项（含 Job 9 项）、API Job 8 项、模型工具校验 11 项及 Shell 31 项通过；工具运行时和 SessionStore 的全目标 Clippy 通过，清理了测试冗余克隆及测试模块后的生产函数位置。该证据来自 macOS，不关闭 Windows 实机门禁。

本轮 P5 生命周期复审还复现了“撤权只发送取消、随后卸载提前删除插件包”的窗口。`PluginManager::uninstall` 现在在包状态提交前检查该插件全部运行租约；未结算时保留状态和包并返回冲突，租约 Drop 后才允许卸载。该回归与插件系统全量生命周期测试通过。

本次工作流复审在原接纳夹具补齐写盘/重载、升级排空期间幂等读取及请求/任务摘要冲突回归；随后真实 daemon 验收先后暴露 coordinator 未声明 LocalWorkflow 导致无法调度、完成摘要只留在临时 attempt 而未进入持久结果两项产品缺陷，现均沿原角色与任务结果入口修复。当前 `test:daemon-controls` 通过 93 次本地 Provider 请求，新增真实插件 Worker 保存检查点、完成摘要、停用/升级/重启后原回执与包摘要保持场景（这些工作流不请求模型）；其余原 Agent、Job、审批和 Goal 场景一并通过。API 派发 8 项、会话路由 102 项、工作流 24 项、接纳/装配 19 项与插件系统单元 18 项、集成 54 项通过；对话运行时 661 项、API turn harness 167 项（1 项性能用例忽略）、orchestrator 65 项、daemon 134 项通过。清理测试夹具无谓 clone、嵌套条件和测试模块布局后，`cargo clippy --workspace --all-targets -- -D warnings` 已通过。Windows 原生运行和真实上游模型扩展矩阵仍需单独验收。本次复用原验收脚本，没有新增执行路径或客户端。

以下按产品合同汇总，替代早期逐轮记录。验证覆盖对应切片；本轮修复后台检查点与会话管理的提交顺序并清理旧刷盘路径，重跑相关持久化和 daemon 产品合同，其他模块的历史结果不能当作修改后的全仓回归。

本次继续从真实 daemon + Chromium 产品入口复核：`test:daemon-controls` 通过 80 次本地 Provider 请求，`test:daemon-input-queue` 通过 24 次请求，`test:daemon-approval-recovery` 通过 3 次请求。三组均使用当前源码构建；除构建阶段既有 PURE 注释和大 chunk 提示外没有失败。它们重新确认队列/引导、跨 Turn Agent Continue、转交与资源归属、审批重启和无预算 Goal 的当前行为。

2026-10-19 存量复审重新执行当前 API、对话运行时、SessionStore、工具运行时、调度、插件和进程测试：API 958 项通过、2 项环境/性能用例忽略；对话运行时 660 项、SessionStore 176 项、工具运行时 309 项（网络 smoke 忽略 1 项）、调度 65 项、插件系统 17 项及集成 54 项、进程 14 项通过。随后路径绑定编辑缺陷先复现 400，再沿原作用域解析入口修复；修复后的 3 项队列编辑回归通过。该轮审查沿持续 Agent 的原 Turn、转交范围、恢复代际和 Task/lease 调用链核对，没有新增调度器、客户端或执行器；测试通过不等于完成所有存量代码的逐行评审。

本次继续复跑当前源码的 Rust 产品边界：`magi-api` 958 项通过（2 项忽略）、`magi-conversation-runtime` 660 项、`magi-session-store` 176 项、`magi-tool-runtime` 309 项（1 项忽略）、`magi-daemon` 134 项及 `magi-plugin-system` 17 项均通过；此前存在的转交竞态定向测试也连续通过 8 次。

桌面与 Browser 入口也使用当前源码复核通过：`test:browser-core`、`test:electron-browser-actions` 及 `test:electron-conversation-dom` 均通过；后者覆盖真实 Electron/Chromium 的队列、引导、恢复、代理卡片、权限、Git 漂移、审批、重启和终态投影。

删除/队列竞争修复后，会话路由 102 项、App Server 控制 1 项、API/进程定向 Clippy 通过；当前 daemon + Chromium 队列夹具再次通过 24 次本地 Provider 请求，覆盖原引导、FIFO、编辑竞争、停止并改做、无预算 Goal 以及 SIGKILL 后从 Web 继续原代理。进程后代回收夹具也分别通过 macOS 与 Windows 目标 Clippy，撤下了此前为 Windows 特定分支加入的 lint 豁免；原根进程先退出、Job 内后代仍运行的断言保留。以上仍不构成 Windows 原生运行或全部存量代码评审的证明。

本次继续核对 HTTP/App Server 的队列控制：共享服务收敛版本有效性及绑定后的补充重试，移除桥接层从最新快照代填版本的路径。 会话路由 102 项、App Server 控制 1 项、API check/Clippy、Web check、Bridge/agent API 回归及协议一致性检查通过。修复后真实 daemon + Chromium 原夹具再次通过，仍为 24 次本地 Provider 请求；复用该夹具增加两种传输的零版本/过期版本拒绝、App Server 缺省版本拒绝，以及绑定、消费、页面重载后的同次引导幂等回执核对。此前一次验收在重启时因本轮中间代码的类型不匹配中止，修复并重新执行后通过；该中止不计作产品通过证据。

本次继续收敛后重新执行了当前源码的协议/发行边界、Web 与受影响 Rust 合同：`npm run release:guard`、`npm --prefix web run check`、`cargo check -p magi-api/magi-conversation-runtime/magi-session-store/magi-bridge-client --locked` 均通过；委派决策 3 项、工具目录 23 项、spawn 预检 4 项、模型 Step 11 项、输入 CAS 2 项、队列引导原子消费 1 项及代理生命周期/API 45 项定向回归通过。此次复核还清理了目标方案中已完成但仍写成“待改造”的引导、Goal FIFO、Git 子代理范围和嵌套派发描述；没有新增客户端、预算或外部 Executor 实现。

随后对完整 Rust 产品边界复跑并修正了两类存量验收漂移：API 全量回归中旧引导 DTO 断言已改为检查当前结构化能力合同；文件变更测试允许 watcher 与 reconcile 的合法竞速归因，但明确禁止把外部变更标成可撤销的工具写入。修正后 `magi-api` 958 项（2 项忽略）、`magi-daemon` 134 项、`magi-conversation-runtime` 661 项、`magi-session-store` 177 项、`magi-tool-runtime` 309 项及 `magi-snapshot` 21 项单测 + 30 项生命周期测试通过；`magi-api`/`magi-snapshot` 目标 Clippy 通过。此次没有引入新的事实源或生产队列路径。

本次 P4 Job 结算复审复用了原进程树夹具，扩展为启动期/异步退出、继承/重定向管道、忽略 SIGTERM、退出码 0/7 的 12 种组合。修复前真实命令返回 `job_output_unconfirmed`，修复后全部组合按真实退出码提交 Completed/Failed，stdout/stderr 保留且释放后无后代写入。SessionStore 的 Job 8 项、API Job 8 项、委派进程隔离 1 项及工具/SessionStore 定向 Clippy 均通过；这组为 macOS 原生进程证据，Windows 原生运行门禁继续保留。

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
| 打包 Desktop 运行链路 | Shell 动作分派修复和 workspace checkpoint 修复后，重建的 macOS Apple Silicon 包再次通过完整 Electron DOM 验收（203 项、54 次本地 Provider 请求）及恢复专项 44 项；此前 Git 漂移/冲突专项 37 项通过，相同检查也包含在完整 DOM 回归 | 普通对话不受 Git 异常阻断，写入失败无副作用且持久化明确未执行回执；代理卡片核对真实任务 ID、详情结果及成功/失败状态；工作区移除后运行 sidecar 不再周期性报未注册归属。不能外推为 Windows 或真实上游模型通过 |
| 真实模型与 Shell 分派 | `gpt-6-astra / low`、`deepseek-v4-flash-0731`、`glm-5.3` 与可用的 `kiro-claude-sonnet-4-5` 经实际 Provider + daemon/App Server 完成小修、缺陷修复、附件诱导、明确禁止和不可用角色接管；四模型均在明确要求双代理时创建并收集两个真实 Task（Claude 代理汇总 17、25 为 42）。独立模块与短复核场景允许主线直接完成，未把“可委派”误当成“必须委派”；GLM 修复 nullable 计划身份后复测成功且零代理。四模型随后各运行一次受控的真实子代理失败场景：每次只创建一个代理，收集 `model_invocation_failed` 终态后均由主线读取原文件并完成 42 的验收，未重复派发 | 首轮实测发现 `action=run` 携带占位 `job_id` 被误分派为 Job 查询；已按动作选择入口，31 项 Shell 回归及 2 项真实 Job 等待/取消回归通过。`kiro-claude-sonnet-4-5` 的基础矩阵、选择质量矩阵和失败接管均通过，证据分别保留在 `/tmp/magi-model-matrix-kiro-claude-sonnet-4-5-evidence.json`、`/tmp/magi-model-choice-kiro-claude-sonnet-4-5-evidence.json` 与 `/tmp/magi-failed-child-kiro-claude-sonnet-4-5-evidence.json`。Provider 当前不提供 `claude-sonnet-5-5`，直接探测返回 `model_not_found`；更大矩阵及重复评测仍未完成；非 Git 单写者限制仍保留 |
| Step、压缩与恢复 | 此前运行时回归保留同文输入、未消费图片、压缩/重启保护、Step/attempt、未知副作用和审批持久化断言；本轮新增 SessionStore 执行树安全上限回归，原主线审批 SIGKILL + Chromium 验收再次通过，3 次本地 Provider 请求且原删除操作只执行一次 | SessionStore 176 项通过；本次新增跨父子 Turn 上限测试；`magi-api` 当前全量单测 958 项通过、2 项忽略（本地性能基准与真实通道压缩包），此前磁盘耗尽的验证限制已解除 |
| 会话管理、工作区移除与 checkpoint | 本轮 SessionStore 176 项、daemon 134 项（含持久化 35 项）及 daemon check 通过；先复现旧后台快照撤销已保存重命名，再验证会话选择、重命名、删除和并发工具结果在重载后保持。原工作区离线、健康会话更新、重新注册后补交回归仍通过；此前工作区 API 19 项通过 | 投影捕获到提交共用持久化锁，canonical/state 锁在快照构造后释放；后台切片不写全局 current。移除旧全量刷盘 API 和专属阻塞测试，原有效断言改走生产增量入口；脏标记强制归属 Session，删除空切片回退全量保存的危险路径。写盘失败和并发新版本不被确认，离线工作区不写全局备用目录 |
| 取消与资源结算 | 本轮先用受控输出线程复现“停止在输出读取结束前返回成功”，修复后工具运行时 311 项通过（网络 smoke 忽略 1 项）、SessionStore Job 9 项、API Job 8 项与两 crate 全目标 Clippy 通过；Job 原进程树夹具扩为 14 种完成/失败/取消组合。真实 daemon/App Server 控制验收在修复后再次通过 96 次本地 Provider 请求 | 输出线程由原 ManagedProcess 持有；移除 watcher 独占读取句柄和单独排空路径，启动期、自然退出和显式停止共同先回收后代、再等待输出。超时保留句柄和原资源屏障，读写失败及重复停止保持未知结果。夹具分别等待 canonical 终态与原 journal 返回后的资源屏障，不把一次瞬时读取当作泄漏证据。没有新协议或持久字段；未验收 Windows 原生进程 |
| 现有执行器兼容性 | 本次插件系统单元 17 项、生命周期 33 项及其余集成 21 项、bridge 265 项、GPT Web 62 项通过；重新构建当前源码的 macOS 目录包后，完整插件验收 167/167 项通过，包含 stdio 握手、工具执行、审批、取消、吊销、工作区移除/恢复和 sidecar 刷盘无归属错误。此前失败的网卡连接现已重试通过：经非回环 IPv4 访问真实 MCP 监听器，网络令牌返回 200、本机专用令牌返回 401。证据保留于 `/tmp/magi-runtime-plugin-recheck-evidence` | 只维护原生模型、GPT Web 和现有插件；旧 `codex_exec` 配置明确拒绝，不回退到 HTTP 模型。本次网络检查来自同机非回环网卡，验证了服务端网络鉴权，不外推公网路由或跨设备部署通过 |
| 静态检查与清理 | 本次 Web check（0 错误、0 警告）、Desktop check 与 146 项单测、daemon check、进程 crate 14 项、协议一致性、架构图和 diff 检查通过；workspace `cargo test --workspace --all-targets -- --test-threads=16` 全量通过，workspace Clippy 及本轮受影响 crate Clippy 也通过，并清理运行时、API、orchestrator、daemon 测试夹具中的冗余 clone、嵌套条件和测试模块布局告警；CLI、具体外部 Executor 和独立移动端入口扫描只剩历史说明及配置拒绝用例 | 本次 Web 构建仍有原 PURE 注释和 bundle 大小提示；Desktop 包为本机目录包，未签名，不等于已发布安装器 |

## 尚未关闭的验收

- 真实 `gpt-6-astra / low`、`deepseek-v4-flash-0731`、`glm-5.3` 与 `kiro-claude-sonnet-4-5` 已完成基础选择矩阵，并各完成一次受控失败接管验收：失败回执先经 `agent_wait` 收集，主线随后直接接管，未另建代理；四次最终结果和文件证据均通过。明确禁止和附件诱导均为零派发，明确双代理均创建两个真实 Task。DeepSeek/GLM/Claude 的短模块与复核样本由主线完成，符合“可委派不等于必须委派”；GLM 的计划 nullable 输入兼容修复后复测完成。Claude 可用模型的基础矩阵和选择质量矩阵均重复运行两次，明确禁止、附件诱导、并行模块、独立复核和不可用角色接管均遵守规则；证据保留在 `/tmp/magi-model-matrix-kiro-claude-sonnet-4-5-evidence-run{1,2}.json` 与 `/tmp/magi-model-choice-kiro-claude-sonnet-4-5-evidence-run{1,2}.json`。Provider 当前不提供 `claude-sonnet-5-5`，直接探测返回 `model_not_found`；更大规模重复评测仍未完成。
- 本轮 Job 输出回收修复后从真实 daemon 产品入口通过 `test:daemon-controls`（96 次本地 Provider 请求），覆盖工作流检查点、Job 重复停止、交互恢复、双客户端幂等、独立 Agent Continue、两层子树、转交后取消/恢复、失败后代保留和 Goal 控制；本轮切片前再次通过 `test:daemon-input-queue`（24 次请求），覆盖编辑/CAS、引导消费、FIFO、重载、无预算 Goal 和停止并改做。它们补强了 P4 的本地跨进程证据，但仍不替代 Windows 和真实上游模型验收。
- Windows 原生门禁：首次 CI 的 success 被原始日志否定，见上文；修复包清理和两项夹具后需重新核对 Windows 2022 的逐命令结果。Windows 目标进程 Clippy 通过只证明可编译；发行包和 GUI 行为仍需独立验收，macOS 打包证据不能外推。
- 大规模存量改动尚未全部逐组审查，不能把当前定向回归称为全部五万行已通过评审，也不能把 P4/P5 全部标为完成。后续只处理存量评审发现的状态/持久化缺陷及 Windows 和 Provider 恢复后的模型矩阵验收缺口，不新增客户端、预算系统或具体外部 Executor。

首次收敛移除了 CLI、重复内存驱动与具体外部执行器，并合并滚动文档；现阶段继续复用已有夹具和事实所有者，不以新增代码行数或测试数量衡量进度。
