# DSH 会话网关设计

> 状态：设计稿，2026-11-19。是 [DSH 对齐设计](dsh-seam-alignment.md) 第 10 节阶段 2 的子设计，不代表任何一项已交付。依据是对锁定 SDK（DSH 0.2.2-rc.3）源码与 Magi 现有代码的核对，条目标明"已核对"或"待核对"。其中需要用户确认的取舍集中在第 11 节。

## 1. 目标与范围

**目标。** 让依赖会话的 DSH 原包完整运行：

- 后端：`ctx.sessions`、`ctx.agents`、`sessionQuery`、`sessionProjections` 与会话事件（样本：Better Sidebar、Message History、Mission Control、Notify）。
- 前端：全局与会话作用域的标准套件（`useSessions`、`useSession`、`useSessionRetainInfo`、`useProjection`、`useAccess` 等），右栏与设置页里依赖会话的组件。

**非目标。**
- 不在页面宿主里手写外观（已核对：`useSession` 等的快照合约含打开状态、待提交输入、历史加载状态等，手写必然漂移）。
- 不替换 Magi 的会话内核与存储（决策 4）：DSH 的会话日志是接口，Magi 的会话事实仍由 daemon 唯一拥有。
- 不做 DSH 应用外壳（决策 7）：`ui-layout`、`ui-workspace`、对话渲染不挂载。
- 不在本设计里决定弹层、语言归属（见 `dsh-seam-alignment.md` 8.6）。

## 2. 已核对的事实

### 2.1 DSH 合约面

客户端 `@deepseek-ai/dsh-api-session-controller/client` 的启动依赖 `connection`、`fileUpload`、`typert`、`remote`、`remote.commands`、`remote.session`、`remote.subagents`。它**实际调用**的服务端方法只有：

| 命名空间 | 方法 |
| --- | --- |
| `session` | `list`、`control`（流）、`follow`（流）、`page`、`projections`、`create`、`prompt`、`cancel`、`rename`、`updateQueue`、`attachment`、`fork`、`search` |
| `commands` | `list`、`execute` |
| `subagents` | `prompt`、`interruptByUser` |

服务端合约另有 `state`、`observeState`、`wait`、`observeEvent`、`observeProjections`、`selectModel`、`modelCatalog`、`openWorkspacePath` 等，客户端会话链不调用，随对话与 SDK 场景需要时再对应。服务端事件 `api-session/added|removed|status|activity|error` 经网关 `$events` 流推给客户端（已有测试证明同类事件如 `settings/document-updated` 可经真实网关到达客户端）。

关键合约形状（均已核对）：
- `SessionSummary`（列表项）：`agentAvailable`、`sessionId`、`updatedAt`、`running`、`blank`，可选 `formatStatus`、`parentSessionId`、`origin`、`cwd`、`projections`。`SessionListValue` 只有 `items`，**没有分页游标返回值**。
- `follow` 帧：`snapshot`（含 `header`、`cursor`、`records`、`hasMore`、`projections`、可选 `assistantStream` 基线）、事件条目、`assistant-stream` 增量（修订号必须连续，缺一即流失败）。
- 日志事件 `SessionWireEvent`：`type`、`seq`、`time`、`data`，可选 `ignorable`、`surfaceOp`、`sourceEventSeqs`；客户端对每条事件运行 `validateSessionEventData`，不合规整条流失败。
- `control` 流：基线 `{projections}` 加投影增量；与列表无关。
- 提示内容：`text`、带数据的 `image`、`file`（上传回执）；`mode` 为 `queue | steer`；队列动作为 `edit | remove | steer`。

### 2.2 Magi 现有面

- **App Server 协议**（冻结的外部协议，`contracts/app-server`）已有：`session/list`、`session/read`、`turn/start`（含 `steerCurrentTurn`、`requestId` 幂等回放、首轮创建会话）、`turn/interrupt`、队列的读取、更新、移除、重试、引导，工具审批与用户提问的读取和答复，目标读写，`events/subscribe`（带序号与 `resyncRequired`）。列表项事实：`sessionId`、`workspaceId`、`title`、`status`（Active/Archived）、`createdAt`、`updatedAt`、`messageCount`、`isRunning`、`runningTaskCount`、`hasUnreadCompletion`。
- HTTP 路由另有 `session/materialize`（创建空会话）、`session/rename`、`session/delete`、`session/viewed`；工作区列表、注册、移除。
- 会话领域事件：`session.created`、`session.deleted`、`session.title.updated`、`session.viewed`、`session.closed`、`session.turn.*`（接纳、排队、转向、完成、失败、中断、取代）。
- 规范轮次事件带会话内持久序号 `event_seq`；共享宿主已能从提交事务重放出 `user/message`、`turn/start`、`turn/end`、`tool/call`、`tool/result`（`host.session.history` 与实时通知共用投影函数）。
- 宿主到 daemon 的请求经一条委托链处理：`PluginServicesBridge`（`host.llm.invoke`、`host.userQuestions.ask`、`host.settings.*`，`plugin_services.rs`）、`pluginManager.*`（`plugin_management.rs`）、`RuntimeEventSink`（`host.session.history`，`native_plugin_platform.rs`）。`host.session.*` 新方法接在这条链上，不新开通道。
- **Magi 未发现对应能力**（按代码检索，不排除别处实现）：用户会话的分叉（只有代理上下文分叉）、会话内容搜索、会话置顶；子代理不是独立会话（是同一会话里的 Thread/Task，无 `parentSessionId`）；标题变更没有日志序号。

### 2.3 2a 进度（2026-11-19）

- **已交付，有测试，未做真机验收**：
  - daemon 事实面：`host.session.list/get/changed`（`magi-daemon::native_plugin_platform::session_facts`、`magi-api::plugin_session_facts`），协议见 [共享宿主协议](plugin-host-protocol.md#会话事实面)。目录项与 App Server 同源，上限 5000 整体失败，通知按会话合并并带单调序号，总线落后发 `resync`。
  - 宿主 `session` 服务（`runtime/dsh/session-gateway.mjs`）：`list`、`control`（空投影基线）与 `api-session/added|removed|status|activity` 增量事件，经真实网关的 HTTP 调用、`$events` 与 `control` 流测试；序号缺口、重复与并发通知、读取失败重试都有用例。
- **2b 已开始（2026-11-19）**：写端口 `magi_api::plugin_session_control::PluginSessionControl` 与 `host.session.cancel`、宿主 `session.cancel` 已交付：端口由 magi-api 以 `ApiState` 实现，daemon 装配后交给插件平台（依赖倒置，方向与 `plugin_session_message_admitter` 一致），`cancel` 调用与界面共用的 `turn_service::interrupt_session_turn`，作用域取自会话记录，调用方不能指定工作区；只作用于用户会话。网关的参数按合约形参名匹配（`list` 为 `_request`，`cancel` 为 `request`），已用真实网关的参数校验测试覆盖。`create` 已交付：`session.create` 只接受可选的 `workspaceId`（已注册工作区为项目会话，否则个人会话），创建目录总是该作用域的目录；`cwd`、预分配的 `sessionId`、`agentPreset` 在 Magi 里没有对应物，明确拒绝而不是忽略。创建走新的 `SessionStore::create_unselected_session_with_working_directory_at`（不改写 `current_session_id`，其余与界面创建相同，仍写时间线），并把界面 `materialize_session` 里的创建、持久化、回滚、`session.created` 事件抽成 `session_lifecycle::create_session`，HTTP 路由与端口共用（`CreationSelection` 区分是否选中；调用方必须持有导航锁，函数以锁守卫参数证明这一点）；未选中会话的持久化失败回滚不带替代当前指针（删除非当前会话带替代指针会被存储拒绝）。`prompt` 与 `updateQueue` 也已交付：
  - `prompt`：`session.prompt` 以用户的名义走与界面、App Server 相同的 `TurnService::submit`，作用域取自会话记录，同一 `requestId` 按轮次提交的回放语义处理（同一业务请求串行，重复提交不重复接纳）；`mode: 'steer'` 绑定提交时观察到的进行中轮次，没有则失败，不降级成普通输入；`queue` 即普通输入（空闲开始、进行中排队）。内容块转换：文本块按顺序以空行拼接，图片随轮次提交，**文本与图片的相对位置不保留**（Magi 的轮次是一段文本加图片列表）；`file` 块需要上传回执，Magi 没有对应物，明确拒绝；`clientTimeZone` 没有 Magi 对应事实，不转发。
  - `updateQueue`：`edit`、`remove`、`steer` 走共用的 `input_service`（提交锁、修订校验、回放语义不变）。排队输入的身份是队列自己的 `queueId`（DSH 的 `itemId`）；`edit` 只替换文本，图片、引用、命令、目标模式、访问档位沿用原输入；Magi 的"重试"没有 DSH 对应项，不暴露。`steer` 的成功路径需要进行中的轮次，目前只测了失败路径（无进行中轮次）。
  - `rename`（2026-11-19 交付，决议 5 落地）：标题变更是会话级日志事件 `session_title_changed`（无 Turn：`turnId` 为空、`turnSeq` 为 0，带会话内 `eventSeq` 与完整标题），**日志为权威、会话记录是投影**。`SessionStore::rename_session_with_persistence` 在持锁区间内先向 `SessionEventWriter::append_session_title` 提交事件（提交点），再改记录与时间线，最后写 durable projection；日志拒绝则什么都不改。事件提交后 projection 写入失败只把错误返回，**不回滚内存标题**（回滚会让记录落后于日志，下次重放又改回来），落盘记录在该会话下次 projection 写入或重启重放时追上日志。投影重放出的最后一个标题在加载时覆盖落盘记录（快照路径与仅事件路径都处理），检查点携带最后标题，释放回合不影响它。标题没有变化的重命名同样提交事件：序号对应被接受的那一刻。界面重命名与插件 `rename` 共用 `session_lifecycle::rename_session`（只有标题实际变化才发布 `session.title.updated`），LLM 自动命名走同一入口；`session.rename` 返回 `{title, seq}`，`seq` 就是该事件序号，没有事件日志时拒绝，不用水位代替。没有标题事件的旧会话在首次改名前沿用记录标题。事件的 DSH 观察形态（`session/title`）随 2c 的 `page/follow` 一起定，这一批不发布观察。
- **`commands` 暂缓（2026-11-19 核对）**：`commands.list`、`commands.execute` 的合约按**代理（agent）**限定作用域，参数经 typert 的 `agent` 查找解析为活的 Agent 对象，执行还要向会话日志追加 `command/run`、`command/done`。它依赖 `agents` 句柄与日志事件，在 `agents`（2d）与历史（2c）之前无法按合约提供，不用忽略 agent 作用域的近似实现代替。Magi 现有的插件命令执行入口（界面发起，`host.commands.execute`）不受影响。
- **2c 的前置缺口（2026-11-19 核对）**：`follow`/`page`/`projections` 要返回通过 DSH 事件校验的日志，现在能给出的只有第 6 节表里"已有"的几类；2026-11-19 更新：助手消息（文本、工具调用、用量）与步骤边界已能从现有事实推导并发布（第 6 节表），`page`、`follow`、`projections` 已交付；仍缺的是提供方流记录（助手消息的 `stream`，以及依赖它的 `assistant/attempt`）、系统提示与请求头（`system/message`、`request/header`）——这些是模型调用层尚未持久化的事实，需要独立的内核改动，不在页面宿主里合成。插件消息另有现成入口：`plugin_session_message_admitter`（后端句柄向会话注入消息，复用 Input 收件箱），与页面代用户发起的 `prompt`（走 `TurnService::submit`）是两条不同语义，不合并。
- **标题**：列表项在会话有真实标题时带 `title`（记录标题，即日志里最后一次标题变更的投影）；新建时的占位标题（"新会话"）不算标题，列表项不带，插件看到 DSH 的回退展示（项目目录名，再回退会话身份）。标题变更触发 `host.session.changed`。
- **`api-session/error`** 没有事实来源，不发。
- **未完成**：客户端挂载真实 `api-session-controller/client` 与 `ui-session`、删除 `session-facts.mjs`；样本验收。

### 2.4 后端 `sessions` 的同步读取（2026-11-19 核对，用户已确认：移到 2c）

DSH 核心的 `ctx.sessions.get(id)`、`list()` 是**同步**返回 `Session` 的，`Session` 的 `eventAt`、`snapshotEvents`、`seq` 也是同步读取（DSH 把所有会话的日志常驻内存）。Magi 的会话可达数千个，历史在磁盘上，需要异步读取（`host.session.history`）。因此 `list()` 不能为每个会话预取历史，惰性读取又违背同步合约。DSH 对齐设计 3.2 要求的"宿主持有 daemon 推送的只读快照"在这里意味着：句柄的事实成员（`id`、`header`）可以同步给出，**事件成员只能对宿主已加载历史的会话同步可读**。建议：2c 随历史一起交付，规则为——`list()` 返回全部会话的句柄（头部事实同步可读），事件成员对已加载的会话同步可读，对未加载的会话抛出带明确原因的错误，并提供让插件触发加载的合约内途径（`sessionQuery` 与调用期作用域，沿现有的受理调用读取）；宿主保留有界的已加载集合。在此之前后端 `sessions` 保持"不提供"（Better Sidebar 的"等待 `sessions`"是合约规定的结果，不是缺陷）。**已确认（2026-11-19）：后端 `sessions` 移到 2c，随历史一起交付。**

### 2.5 剩余项与前置条件（2026-11-20 收口记录）

已交付并有测试（含真实 `magi-daemon-app` 与真实 Electron 45 + 沙箱 `PluginFrame` iframe 端到端验收；2026-11-20 在重新打包的目录包上重跑并留存结果，见[阶段 2 收尾验收](plugin-validation/phase-2/closeout-2026-11-20/README.md)）：
- **会话事实面与 Typert 网关**：`host.session.list/get/changed/history/committed/assistantStream/appendCommandEvent` 与控制端口 `create/prompt/cancel/updateQueue/rename`；Typert `session` 的 `list`、`control`（`changes` 数组严格遵循 `Array<{ key, val }>` 合约，含 `title`、`subagentCatalog`、`subagent`、`subagentTiming` 实时推送）、`create`、`prompt`、`updateQueue`、`cancel`、`rename`、`page`、`projections`（父会话含 `title` 与 `subagentCatalog`，子代理含 `subagent` 与 `subagentTiming`）、`follow`（含主线与只读子代理 `SessionAddress.subagent`，及 `assistantStream: true` 实时流帧）、`attachment`（经宿主私有图片存储服务读取 durable image 并映射 `AttachmentError` → `session/attachment-invalid`）；Typert `commands.list/execute`（经只读 `agent` lookup 绑定已确认的用户会话，`command/run`/`command/done` 持久化入会话事件日志并在检查点与重放中一致）。
- **观察事件与持久化**：`user/message`、`turn/start|end`、`tool/call|result`、`assistant/message`（文本、工具调用、`usage`、`stream`）、`assistant/attempt`（失败与取消尝试的流记录，在线与重放一致）、`step/start|end`、`request/header`、`request/context`、`system/message`（按摘要去重、凭据脱敏、4 MiB 上限、外部执行器步骤跳过、在线重启播种与重放一致）、`subagent/catalog`、`subagent/descriptor`、`command/run`、`command/done`、`session/title`（标题变更落盘后即时推送在线观察并在历史重放一致）。
- **正式后端会话服务（§2.4 落地，合并清退路由专用版）**：`ctx.sessions`（`Sessions`，路由调用期保持严格隔离与吊销契约，全局后端同步返回全部用户会话只读句柄，`id`/`header` 同步可读、已加载历史同步可读、未加载历史明确报错并指引 `ctx.sessionQuery`）、只读 `ctx.sessionPersistence`（`MagiSessionPersistence`，`list`/`stat`/`open('read')` 代理到 daemon 并维护 16 会话有界缓存，写入口抛 `SessionReadOnlyError`）、正式 `ctx.sessionQuery`（`MagiSessionQuery extends SessionQueryEngine`，直接继承锁定官方 `@deepseek-ai/dsh-session-query`，13 个查询/折算方法自动向 daemon 同步最新水位；`searchSessions`/`searchEvents` 按官方标准码返回 `SESSION_QUERY_SEARCH_DISABLED`）。
- **页面宿主迁移与真实 Electron 45 验收**：页面宿主挂载锁定原包 `@deepseek-ai/dsh-client-file-upload/client`、`@deepseek-ai/dsh-api-session-controller/client` 与 `@deepseek-ai/dsh-client-ui-session/client`，通过受准入保护的 `/api/plugins/{id}/mux` WebSocket 隧道与 `PluginFrame.svelte` / `transport.mjs` `createWebSocket` 桥接复用官方 `RemoteStreamMuxClient`；`client-host/index.mjs` 等待权威 `sessions.list` 到达后再 `retain`（带防重入守卫）并由 `root-shell.tsx` 通过官方 `<SessionProvider session={mainSessionRef}>` 向严格 `'session'` 作用域（如 `sidebar.right.pane.tab`）注入真实标准属性；彻底删除 `client-host/session-facts.mjs`、`test/session-facts.test.mjs` 与手写双链路。已在真实 Electron 45 `BrowserWindow` + 真实 `magi-daemon-app` + 真实右侧栏沙箱 iframe 中完成全链路验收（含插件探测发现、`sidebar.right.pane.tab` 严格会话作用域注入、`retain().ready` 经真实 WebSocket `session/follow` 握手、`session/rename` 触发沙箱 iframe DOM 无刷新实时更新为新标题，0 运行时错误）。

**`page`/`follow`/`projections` 的合约与实现（2026-11-20，依据原包源码 `history.ts`、`history/controller.ts`、`web/controller.ts` 与 `@deepseek-ai/dsh-subagent`）**：
- `page` 在宿主已读取的**稠密日志**（从 0 起连续的 DSH 序号，由 `session-reader` 从 `host.session.history` 构造）上做按消息对齐的向后分页：从截断点向前数追加型 `user/message`、`assistant/message`，达到 `maxMessages`（默认 50）即在该消息所属分组的起点切开；`beforeSeq` 是开区间上界，`throughSeq` 是闭区间截断（省略为日志末尾，`-1` 为空，超过末尾拒绝）；`turnWindow` 在两个下限都满足后于 `turn/start` 处停止。参数校验与规则逐条对齐原包，并有测试固定。返回 `{cursor, records, hasMore}`。
- `follow` 先订阅再读日志（读取期间提交的事件进入有界队列，4096 条，溢出以 `gateway/overflow` 结束该流让客户端重开，不跳过），再发开场帧 `snapshot`（`header`、`cursor`、`records`、`hasMore`、`projections`、请求时的 `assistantStream` 基线），随后把水位之后提交的每条观察按稠密序号续在日志后面；daemon 水位以内的观察丢弃。队列由 `host.session.committed` 通知经 `observeCommitted` 喂入；`assistant-stream` 增量帧由 `host.session.assistantStream` 经 `observeAssistantStream` 喂入。
- `projections`：父会话从事件日志折算 `title` 与 `subagentCatalog` 投影（`{kind: 'sequenced', asOfSeq: <日志末尾>, values: { title, subagentCatalog }}`）；子代理只读会话折算 `subagent` 与 `subagentTiming` 投影并经 `control` 流（`changes: [{ key, val }]`）实时广播更新；未知会话返回 `null`。
- 只读子代理映射：从父会话 canonical event log 中带 `metadata.agentExecution`（`AgentTurnBinding`）的独立子代理 `CanonicalTurn` 投影：`Main` 作用域在首个子代理 Turn 发出 `subagent/catalog`，`Subagent { task_id }` 作用域发出 `subagent/descriptor`（v3, `mode: 'one-shot'`）与该子代理的完整只读事件流；`page`/`follow` 支持 `{ kind: 'subagent', parentSessionId, childSessionId, mode: 'one-shot' | 'unknown' }`，拒绝 `continuable`（`subagent/unauthorized`）与未知子代理（`subagent/not-found`）；遵守决议 4，不提供 `subagents.prompt` / `interruptByUser`；`activate` 不适用（Magi 没有"激活后台 Agent"这一概念）。
- 验证：每种宿主产生的事件（含 `subagent/catalog`、`subagent/descriptor`、`command/run`、`command/done`、`session/title`）都通过客户端自己的 `assertSessionWireEvent` 规则（信封、`validateSurfaceMetadata`、`validateSessionEventData`）并能被 DSH 的 `foldSurface` 回放；经真实网关（HTTP 与 WebSocket mux）与真实 Electron 45 沙箱 iframe 验证了 `page`、`projections`、`follow`、`control`、`attachment`、`commands/list`、`commands/execute`、`ctx.sessions`、`ctx.sessionPersistence`、`ctx.sessionQuery`；并挂载真实锁定 `@deepseek-ai/dsh-api-session-controller/client` 与 `@deepseek-ai/dsh-client-ui-session/client` 验证了 `useSessions`、`useSession`、`useSessionStatus`、`useSessionRetainInfo`、`useSessionAccess`、`useProjection` 与 `useAccess`。

当前状态汇总：

| 项 | 状态与说明 |
| --- | --- |
| P1 流记录、P2 系统提示/请求头、P3 实时助手流 | **已交付并验证**（见 [model-call-record-persistence-design.md](model-call-record-persistence-design.md) 与 [live-assistant-stream-design.md](live-assistant-stream-design.md)）。 |
| 子代理只读映射、`commands.list/execute`、页面宿主迁移 | **已交付并经真实 `magi-daemon-app` 与真实 Electron 45 沙箱 iframe 端到端验证**。 |
| `session/attachment`、`session/title` 观察与 `title` 投影 | **已交付并经单元测试、真实 daemon 8 步与真实 Electron 45 7 步端到端验证**。 |
| 正式后端 `ctx.sessions`、只读 `ctx.sessionPersistence`、官方 `ctx.sessionQuery` | **已交付并经单元测试与真实 daemon 端到端验证**（合并清退原路由专用版 `session-reader`）。 |
| 打包 `.app` 内固定原包（Better Sidebar / Message History / Mission Control / Notify）全套发布态验收 | 待后续重打 `target/electron-dist/mac-arm64/Magi.app`（当前本地磁盘余量偏紧，本轮未触发全量 `electron-builder` 打包，已用最新 `magi-daemon-app` + `web/dist` + Electron 45 验证真实沙箱 iframe 全链路）。 |
| `subagents.prompt/interruptByUser` | **已确认不提供**（保持 Magi 自己的 Thread/Task 模型，插件只读映射，不允许插件干预内核）。 |
| `session.search`、`session.fork`、`selectModel`、`modelCatalog`、`openWorkspacePath` | **另立项**（按设计文档第 8 节，需先定义 Magi 产品语义；`sessionQuery.searchSessions`/`searchEvents` 按官方错误码返回 `SESSION_QUERY_SEARCH_DISABLED`）。 |

## 3. 架构

```
DSH 原包 ──ctx.sessions / agents / sessionQuery──┐
页面组件 ──真实 ui-session + 会话控制器客户端──┐   │
                                              ▼   ▼
                         共享 Cordis 宿主（Node）：会话网关
                         ├ Typert 服务 session / commands / subagents
                         ├ 后端服务 sessions / agents / sessionQuery（句柄，3.2）
                         └ 事件桥（api-session/*、session/event）
                                   │ host.session.*（请求）  host.session.changed / committed（通知）
                                   ▼
                         daemon：会话事实面 + 会话控制端口
                         └ 与 App Server 共用同一组服务函数，不另设写入口
```

### 3.1 daemon：会话事实面（只读）

单一所有者，向宿主提供：
- `host.session.list`：用户会话目录（排除外部工具会话），含 `running`、`blank`、`cwd`、`updatedAt`、`status`、`hasUnreadCompletion`。
- `host.session.get`：单个会话头部（创建时间、初始目录）与状态，用于后端句柄与 `follow` 的 `header`。
- `host.session.history`（已有）：规范序号分页，固定水位。
- 通知 `host.session.changed`：`added`、`removed`、`status`、`activity`、`title`，带本宿主代次内单调的 `factsSequence`。通知可丢失（队列有界），宿主检测到序号缺口或收到 `resync` 时重读目录并对已发布状态求差，补发缺失的增量。**不持久化镜像**：目录始终是 daemon 的权威读取，宿主只保存已发布状态用于求差。

### 3.2 daemon：会话控制端口（写）

宿主把网关写操作翻译为 daemon 的 `host.session.*` 请求。**daemon 侧不新增业务逻辑，与 App Server 共用同一组服务函数。** 2026-11-19 核对后范围比原先估计的小：轮次提交（`TurnService::submit`）、停止（`turn_service::interrupt_session_turn_with_expected`）、队列的读取、更新、移除、引导、重试（`input_service`）已经是 App Server 与 HTTP 路由共用的服务层，网关直接调用；真正只存在于 HTTP 路由处理函数里的是会话创建（`materialize_session`：导航锁、作用域解析、持久化事务与回滚、`session.created` 事件）和重命名（`rename_session`：生命周期锁、持久化、`session.title.updated` 事件），两者还把结果组装成界面用的 `BootstrapDto`。2b 只需把这两处的领域动作抽成不带界面 DTO 的服务函数，HTTP 路由改为调用它再组装 DTO。

**调用方向**：写操作需要 `ApiState`，而 daemon 的插件平台由 `ApiState` 持有（`NativePluginPlatform`），不能反向依赖。用依赖倒置：在 `magi_api::native_plugins` 定义 `SessionControlPort`（`prompt`、`cancel`、`update_queue`、`create`、`rename`），由 magi-api 以 `ApiState` 实现，在装配时交给 daemon 的协调器；协调器的 `host.session.*` 请求只调用该端口。

每个写请求带：来源 `plugin-page`（页面调用）或 `plugin-backend`（插件后端句柄）、发起插件身份与激活代次、用户授权判定的结果（页面已由 daemon 准入，对等方是所有者），并写入审计事实，使 canonical 记录能标注"由哪个插件发起"。

### 3.3 宿主：Typert 服务

在共享宿主里实现 `session`、`commands`、`subagents` 三个 `TypertRemoteService`（与现有 `SettingsControllerService`、`PluginInventoryService` 同一模式），方法名与锁定 SDK 的合约一致。能力有限的方法不暴露（不做返回占位值的空实现）；调用未暴露的方法按网关的 `gateway/bad-request` 失败，并在能力清单里列出。

### 3.4 宿主：后端服务

`ctx.sessions`（列表、按身份取句柄）、`ctx.agents`（句柄）、`sessionQuery`、`sessionProjections` 由同一事实面支撑，句柄遵循 DSH 对齐设计 3.2：`(类型, id, revision, 实例代次)`，宿主重启、插件升级、重新启用后旧句柄失效；写入与裁决异步重读并校验修订。对象业务行为只开放登记清单里的项（v1 仅"消息投影转换"）。

### 3.5 客户端

挂载锁定 SDK 的真实 `api-session-controller/client`、`client-file-upload` 与 `ui-session`，由它们提供 `ctx.sessions`、`useSessions`、`useSessionRetainInfo`、`useSession`、会话作用域适配器，并**删除** `client-host/session-facts.mjs` 与标准套件里对应的手写源。`useSessionStatus`、`useWorkspaces` 的包依赖外壳或未发布，仍按"事实齐备前不提供"处理（见 `dsh-seam-alignment.md` 8.6）。

## 4. 方法映射

分期见第 8 节。"缺口"列是 Magi 当前没有的事实或能力，没有它就不实现该方法，不用近似值代替。

| DSH 方法 | Magi 来源 | 缺口 | 期 |
| --- | --- | --- | --- |
| `session.list` | `host.session.list`（`session/list`） | 无分页返回值，目录规模见第 11 节 | 2a |
| `api-session/*` 事件 | `host.session.changed` | 无 | 2a |
| `session.control` | 投影基线 + 实时 `projection` 增量数组（`title`、`subagentCatalog`、`subagent`、`subagentTiming`） | 插件自定义投影属 `sessionProjections` | 2a/2c/2d 已交付 |
| `session.create` | 共享服务层的会话创建（HTTP `session/materialize`） | `agentPreset`：Magi 的角色不是预设，不实现该字段 | 2b 已交付 |
| `session.prompt` | `turn/start`（`requestId`、`steerCurrentTurn`、图片与上下文引用） | `file` 内容需宿主附件回执转换；图片按附件服务取数据 | 2b 已交付 |
| `session.cancel` | `turn/interrupt` | 无 | 2b 已交付 |
| `session.updateQueue` | 队列更新、移除、引导 | DSH 的 `edit` 仅限纯文本；Magi 的"重试"没有对应项，不暴露 | 2b 已交付 |
| `session.rename` | 共用的 `session_lifecycle::rename_session`（HTTP `session/rename` 同入口） | `seq` 是标题事件的会话内日志序号；无事件日志时拒绝 | 2b 已交付 |
| `session.page`、`follow`、`projections` | `host.session.history` 投影与 `host.session.committed` / `assistantStream` 通知 | 含主线与只读子代理、实时助手流、系统提示与请求头、`title` / `subagentCatalog` 投影 | 2c/2d 已交付 |
| `session.attachment` | 宿主私有图片存储服务（`attachments.readImage`，`AttachmentError` → `session/attachment-invalid`） | 无 | 2c 已交付 |
| `session.search` | 无 | Magi 没有会话内容搜索 | 另立 |
| `session.fork` | 无（只有代理上下文分叉） | 用户会话分叉语义未定义 | 另立 |
| `commands.list`、`execute` | 插件命令与只读 `'agent'` lookup + 持久化 `command/run` / `command/done` | 无 | 2b/2d 已交付 |
| `subagents.prompt`、`interruptByUser` | 不提供（用户确认保持 Magi Thread/Task 模型，插件仅只读映射，不干预内核） | 不适用 | 已排除 |
| `selectModel`、`modelCatalog` | 模型设置 | 随对话需要 | 另立 |

## 5. 一致性与失败

- **列表与事件**：客户端先调用 `list`，再接收事件；服务端保证事件相对该次列表可重放（重复的 `added`、对未知会话的 `status` 不应造成状态错误；客户端对这些乱序的实际处理待 2a 用真实客户端包逐项核对）。`factsSequence` 缺口、宿主重启或队列溢出后，宿主重读并补差。
- **代次**：网关流、句柄、通知都带宿主进程代次（DSH 对齐设计 3.6）；宿主重启时在途流以载体错误结束，客户端按其重连逻辑重新 `list`。
- **有界**：每条流有界队列；`follow` 的 `assistant-stream` 修订必须连续，无法保证时结束该流而不是跳号。
- **写操作幂等**：`prompt` 以 `requestId` 为幂等键，沿 `turn/start` 的回放语义；重复请求不重复接纳。
- **取消**：网关调用的取消信号传给 daemon 请求，daemon 对已接纳的轮次不回滚，只报告当前事实。
- **授权失效**：插件停用或撤权后，其句柄与在途网关调用立即拒绝新请求，进行中的写请求按已接纳事实结算，不改写已提交历史（对应 Q5）。

## 6. 日志保真

`follow`/`page` 返回 DSH 事件日志，**每条必须通过 `validateSessionEventData`**，所以只能返回有完整事实的事件类型，不合成。

| DSH 事件 | Magi 事实 | 状态 |
| --- | --- | --- |
| `user/message` | 输入绑定（消息身份、文本） | 已有（共享宿主已发布） |
| `turn/start`、`turn/end` | 提交的主线 Turn 与终态（含用户中断与结构化失败） | 已有 |
| `tool/call`、`tool/result` | 已确认的主线工具活动 | 已有（范围见 `plugin-host-protocol.md`） |
| `assistant/message` | 主线助手文本条目 + 其模型步骤（编号、模型绑定）+ 已记录的工具调用 | **部分已有**：在线观察与历史重放共用同一推导，发布文本与工具调用块；`stream` 取该步骤已提交的尝试流记录（HTTP 提供方的成功尝试；没有记录则为空数组）；`usage` 取该步骤最后一次**成功**传输回执（回执在响应条目提交前已落盘），口径与用量统计的 `normalize_usage_delta` 相同、也就是 DSH 的互不重叠计数（输入只含未命中缓存部分，缓存读写单列），没有成功回执或回执无用量时不带，不估算；思考内容是独立条目、不并入；缺步骤、模型绑定或工具调用记录不完整时不发布，不用占位补 |
| `step/start`、`step/end` | 主线 `Execution` 模型步骤的首次提交（发送前冻结），与步骤的完成（响应终态落盘且请求的每个工具调用都有已提交结果） | **已有**：在线与历史重放共用同一推导；失败、取消或结果未知而未自然完成的步骤在回合结束时、`turn/end` 之前收口；一个 canonical 事件可产生多条观察（共享 `sourceSequence`），历史分页只在事件边界切页 |
| `request/header`、`request/context`、`system/message` | 主线 `Execution` 步骤首次提交时记录的 `ModelRequestRecorded`（系统提示、模型配置与规范工具表，写入前凭据脱敏、按 SHA-256 去重、上限 4 MiB 超限放弃并告警、外部执行器步骤跳过） | **已有**：在线观察（重启后由检查点最新摘要播种）与历史重放共用同一推导；`request/header` 按相邻差异产生 `initial`/`change`，`request/context` 在路由或上下文窗口变化时发出，`system/message` 以 `surfaceOp: 'append'`（`source.kind: 'system-prompt'`）发出 |
| `assistant/attempt` | 失败/取消的模型调用尝试流记录（HTTP 提供方） | **已有**：在线观察与历史重放共用同一推导；失败/取消流不进待配对，直接作为非 surface 事件发布，先于同一步骤的后续尝试 |
| `subagent/catalog`、`subagent/descriptor` | 父会话 canonical event log 中携带 `metadata.agentExecution`（`AgentTurnBinding`）的独立子代理 `CanonicalTurn` | **已有**：父会话 `Main` 作用域在首个子代理 Turn 发出 `subagent/catalog`（折算 `subagentCatalog` 投影）；子代理只读作用域发出 `subagent/descriptor`（v3, `mode: 'one-shot'`）及独立事件流（折算 `subagent` / `subagentTiming` 投影） |
| `command/run`、`command/done` | 会话级非 Turn 日志事件 `CommandEventRecorded`（经 `host.session.appendCommandEvent` 落盘，无 Turn 包装、带连续 `eventSeq`） | **已有**：在线观察与历史重放共用同一推导；`commands/execute` 先落盘 `command/run`（失败则不进入 handler），结算后再落盘 `command/done` |
| 标题变更（`session/title`） | 会话级标题事件 `SessionTitleChanged`（带 `eventSeq`，落盘后即时推送在线观察） | **已有**：在线观察与历史重放共用同一推导；在 `Main` 作用域发出 `session/title`（子代理作用域过滤），折算 `title` 投影并通过 `control` 流（`changes: [{ key: 'title', val }]`）实时推送 |

结论（2026-11-19 更新）：拿到锁定的 `dsh-api-session-controller` 0.2.2-rc.3 原包后核对，`page`、`follow`、`projections` 的合约与分页规则都已明确，且 `request/header`、`system/message` 只供请求检视与系统提示视图、对话渲染不依赖，所以已按现有事件集交付这三个方法（第 2.5 节）；缺这些事实的视图（请求检视、系统提示、流式回放、`assistant/attempt`）不提供，随内核事实补齐。需要补的内核事实沿用 `dsh-seam-alignment.md` 阶段 2 的清单：会话内事件序号（已有）、条目所属模型步骤与步骤边界、助手消息的流记录与用量、工具结果的消息身份（已有）、子代理与主会话的归属。

## 7. 安全与写入单一路径

- 页面调用：daemon 先按页面准入（安装摘要、实例身份、会话）接纳，网关对等方是所有者（已用真实网关测试）；后端句柄调用：daemon 校验插件激活代次。
- 写操作只经会话控制端口进入 daemon 的服务层，不绕过现有的审批、访问档位、目标与队列规则；插件发起的轮次在 canonical 记录里带来源标注，用户界面可见。
- 速率与并发：每个插件的写请求有界并发；超限明确失败。
- 外部工具会话（`SessionKind::ExternalTool`）不进入网关目录与句柄。

## 8. 分期、样本与退出条件

每期都在真实 Electron 与 daemon 中验收，固定样本与版本在期初锁定（沿 `docs/plugin-validation` 清单）。

**2a 会话事实面与列表。** daemon 事实面（`host.session.list/get/changed`）；宿主 `session` 服务的 `list`、`control`、事件；客户端挂载真实 `api-session-controller/client`、`ui-session`，删除 `session-facts.mjs`。**后端 `sessions` 句柄不在 2a**（原计划在此，见下方"后端 `sessions` 的同步读取"，移到 2c）。
- 样本：Notify 0.4.9、Mission Control 0.2.1 的列表与状态相关业务；Better Sidebar 仍等待后端 `sessions`，随 2c 验收。
- 退出：两者通过通用业务任务；`useSessions`、`useSessionRetainInfo`、`usePanelInfo` 的值与 daemon 事实一致（对照测试：创建、重命名、删除、运行状态变化后列表在秒级内对齐）；宿主重启、事件队列溢出后列表对齐；允许不提供：`useSession` 及会话作用域的快照类成员。

**2b 会话控制。** 共享服务层抽出；`create`、`prompt`、`cancel`、`updateQueue`、`rename`、`commands.list/execute`。
- 样本：使用 `inputActions`、命令的原包（期初从样本中锁定）；Message History 的输入写回。
- 退出：写操作经同一服务层，审计可见来源；幂等、取消、撤权用例通过；不绕过审批与访问档位（反例用例）。

**2c 历史读取。** `page`、`follow`、`projections`、`attachment`，以及 `sessionQuery`。前置：第 6 节的内核事实补齐到客户端最小事件集。
- 样本：Message History 1.0.6 历史补读（`sessionQuery.listSessions()` 返回项形状以锁定 SDK 为准，已知与该原包读取方式的差异保留为记录）、Better Sidebar 历史。
- 退出：每条返回事件通过 `validateSessionEventData`；修订连续性与断线续读用例通过。

**2d 子代理。** 子代理 Thread/Task 到 `SessionAddress.subagent` 的映射，`subagents.prompt/interruptByUser`。前置：子代理事实模型确认。
- 退出：Mission Control 子代理场景。

**另立。** `search`、`fork`、`selectModel`、`modelCatalog`、`openWorkspacePath`。每项先写 Magi 侧的产品定义，再实现。

## 9. 删除清单

- **已删除**：`client-host/session-facts.mjs`、`test/session-facts.test.mjs`、`standard-kit.mjs` 里会话列表与保留信息的手写源，以及 `PluginFrame.svelte` 中的手写会话同步与仅限 `$events` 的流桥接（已替换为 `/api/plugins/{id}/mux` + `createWebSocket` + 锁定原包 `@deepseek-ai/dsh-api-session-controller/client` 与 `@deepseek-ai/dsh-client-ui-session/client`）。
- **已合并清退**：原宿主里仅供路由调用期间一次性读取的专用 `session-reader.mjs` 实现（已升级合并为正式后端 `ctx.sessions` + 只读 `ctx.sessionPersistence` + 继承官方 `@deepseek-ai/dsh-session-query` 的 `ctx.sessionQuery` 单一实现）。

## 10. 验证方式

- daemon：事实面与控制端口的单元与集成测试（含与 App Server 共用服务层的回归）。
- 宿主：用真实网关流测试每个 Typert 方法（沿 `services.test.mjs` 的方式），含代次、断线与溢出。
- 客户端：用真实客户端包在 Node 中挂载（沿 `client-access.test.mjs` 的方式）核对 hook 值。
- 真机：固定样本的通用业务任务，证据存入 `docs/plugin-validation/phase-2/`。

## 11. 已确认的取舍（2026-11-19，用户按建议确认）

1. **写入口**：先把 `app_server.rs` 与 HTTP 会话路由的私有处理函数抽成共享服务层，再接 `host.session.*`；不让宿主作为 App Server 客户端接入。这会改动现有路由实现（行为不变，需回归），属于 2b 的前置。
2. **目录规模**：`session.list` 整批返回，硬上限 5000，超限整体失败并提示，不静默截断。
3. **事件集先行**：事件集不全不暴露 `follow`/`page`。2026-11-19 核对原包后，对话渲染所需的事件集已齐（缺的 `request/header`、`system/message` 只供检视视图），`page`、`follow`、`projections` 已按此交付；`useSession` 与会话作用域成员仍不提供。
4. **子代理**：2d 以只读映射开始，不伪造 `parentSessionId` 与子会话目录项。
5. **`rename` 的 `seq`**：标题变更作为内核事实进入日志并有序号，不返回当前水位代替。

2a 只含只读事实面，不依赖第 1、5 项；第 2 项的上限在 2a 落地。

**第 6 项（2b 核对新增，用户已确认 2026-11-19）：插件创建的会话不抢占用户当前选中的会话。** 现有的用户会话创建会同时把 `current_session_id` 指向新会话（`create_session_with_working_directory_at`）；外部工具会话明确不这样做（`create_external_tool_session` 的注释写明"不抢占用户当前选中的会话"）。DSH 的 `session.create` 只创建、由调用方决定是否打开，若沿用现有创建路径，插件会静默切走用户正在看的会话。建议新增不写 current 指针的用户会话创建变体，`session.create` 使用它。`create` 按此实现；`rename` 随第 5 项交付（见 2.3）。
