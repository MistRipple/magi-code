# 共享 Cordis 宿主内部控制协议

本协议实现 DSH 对齐设计第 3.4、3.6 节的共享进程生命周期，供 daemon 与 Node 宿主使用。开发线的 daemon 通过 `magi-extension-host::shared` 运行共享宿主。文中较早的开发记录描述对应批次的范围。应用和原包的验收进展见[阶段 1 收尾记录](plugin-validation/phase-1/closeout-2026-11-18/README.md)。

## 传输与身份

复用 `magi-extension-host` 的本地 socket／命名管道、4 字节大端 JSON 帧、Magi 传输版本 1.0 和有界 `Peer`。JSON-RPC 信封为 `jsonrpc: "2.0"`；`protocol: {major: 1, minor: 0}` 是 Magi 传输版本。daemon 托管进程树，创建一次性随机令牌并签发宿主代次。Node 在加载原包前删除令牌环境变量。

Node 发出 `host.hello`，包含 `token`、`protocol` 和 `features: ["runtime.shared"]`。daemon 验证令牌及 shared 特性后响应同一特性、`hostGeneration`、`limits: {frameBytes, pending}`；Node 发出 `host.ready`。共享协议不协商 SDK 2 的 `extensionApi`、清单或单插件身份，缺少 shared 特性直接失败，不尝试旧宿主。

### 会话观察

`host.session.committed` 是 daemon 发往当前宿主的单向通知。信封包含 `hostGeneration, sessionId, occurredAt, observation`，日志事件另带 canonical `eventSeq`。同会话沿持久提交次序入队。目录事务首次保存用户会话时发布 `session/created`。首轮接纳首次落盘的会话在同一接纳事务成功后、发布轮次事件前通知。已落盘目录或已接纳过的会话会在后续写入去重，失败写入不会发布。创建事件独立于轮次日志序号。

普通文本的消息身份来自本事务的 Input 绑定。轮次边界来自已提交的主线 Turn，引用 canonical 排序中的位置。轮次终态要求发生实际状态转换。用户中止来自已提交的中断来源，失败取主线已落盘的结构化诊断或失败消息。Node 按锁定 DSH 的结构发布 `user/message`、`turn/start`、`turn/end`。创建和轮次观察当前均携带只读会话身份，完整读句柄仍待交付。监听器抛错或拒绝只记录告警。

主线模型工具有对应的执行 Step 时，首次提交运行条目后发布 `tool/call`，参数取同一 Step 已接纳的模型消息。执行器写回已知结果时在原 canonical 条目提交 `toolResultCommitted`。宿主用条目 ID 作为结果消息身份，沿同一 `callId` 发布 `tool/result`。进度、回合终态收口和结果未知均不产生工具结果事件。

主线模型响应条目首次到达终态时发布 `assistant/message`：步骤编号取该主线执行步骤的序号，模型身份取步骤绑定，内容为文本与该条目记录的工具调用（`tool-call` 块，参数为原始 JSON 字符串）。`stream` 是该步骤已提交的尝试流记录（DSH 紧凑流记录；没有记录时为空数组，不伪造）；`usage` 取该步骤最后一次成功传输回执，换算成 DSH 的互不重叠计数（`inputTokens` 只含未命中缓存部分，`cacheReadTokens`、`cacheWriteTokens` 单列且仅在回执给出时出现），没有成功回执或回执无用量时不带，不估算；思考条目不并入。失败或取消的主线模型尝试在提交 `ModelAttemptStreamRecorded`（其 `finish.reason.kind` 为 `error` 或 `aborted`，未关闭块不写 `block-end`）时直接发布非 surface 的 `assistant/attempt`（`data: {turn, step, stream}`，无 `surfaceOp`），先于同一步骤之后的下一次尝试；在线通知与历史重放共用同一推导。

主线执行步骤首次提交时发布 `step/start`（编号同上）；非外部执行器步骤紧接写入 `ModelRequestRecorded`（按 SHA-256 摘要去重、写入前脱敏凭据、单条上限 4 MiB 超限放弃并告警），在请求头、路由/上下文窗口或系统提示首次出现及发生变化时依次发布 `request/header`（`initial`/`change`）、`request/context` 与 `system/message`（`surfaceOp: 'append'`、`source.kind: 'system-prompt'`），重启后在线观察由检查点最新摘要播种以与全量重放一致。步骤完成时发布 `step/end`：响应条目终态落盘且它请求的每个工具调用都有已提交结果（没有工具调用的响应在落盘时完成）。失败、取消或结果未知而未自然完成的步骤，在回合结束事件里先发 `step/end` 再发 `turn/end`。一个 canonical 事件可以产生多条观察（例如助手消息与它收口的步骤），它们共享 `sourceSequence`；`host.session.history` 分页只在事件边界切页，`nextAfter` 是已完整返回的最后一个事件序号，宿主按观察次序重新编号 DSH 序号。未关联步骤、缺模型绑定、工具调用记录不完整、错误结果文本和子代理线程的条目均不发布。本批覆盖具有实际开始条目、成功或失败文本结果的主线调用。订阅只接收在线提交，历史与恢复读取继续由会话读取合同交付。

`host.session.assistantStream` 是 daemon 发往当前宿主的非持久化实时助手流单向通知：`{hostGeneration, sessionId, frame}`，`frame` 为 `start`（`attemptId, turn, step, afterEventSeq`）、`chunk`（`attemptId, index, time, chunk`）、`abandon`（`attemptId, index`）或 `overflow`（`attemptId`）。仅主线 `Execution` 步骤的 HTTP 流式尝试发送，与 `host.session.committed` 共用同一有界 `Peer::notify` 队列以保证因果序；队列满或超帧时标记当前尝试降级并停止发送跳号块，同时尝试发送 `overflow`。Node 宿主为每会话维护严格连续 `revision` 与 `AssistantStreamAccumulator` 重连基线，并在匹配持久事件入队后紧接发出带精确稠密 `seq` 的 `end(committed)`，遇 `overflow` 或序号断链则以 `gateway/overflow` 结束受影响的 `follow` 流。详情见 [P3 实时助手流设计](live-assistant-stream-design.md)。

收到通知的宿主按协商代次核对。投递受共享发送队列和帧上限约束，队列满或载荷超限时记录诊断，原会话提交仍有效。重启不会重播这类实时通知，历史由后续会话读取合同提供。[原包业务回执](plugin-validation/phase-2/live-input-2026-11-19/README.md)

### 会话事实面

用户会话目录的只读事实，供宿主的 `session` 网关服务（[会话网关设计](dsh-session-gateway-design.md)）使用。目录始终从会话存储读取，宿主只保存已发布状态用于求差，不持久化镜像。

| 方法 | 输入 | 回执 |
| --- | --- | --- |
| `host.session.list` | `{includeArchived?: boolean}`，未知字段拒绝 | `{factsSequence, sessions}`；默认只含 `Active` 的用户会话，外部工具会话不在其中；超过 5000 个整体失败，不截断 |
| `host.session.get` | `{sessionId}` | `{factsSequence, session}`；`session` 为空表示不存在或不是用户会话，已归档会话照常返回 |
| `host.session.rename` | `{sessionId, title}`，未知字段拒绝 | `{title, seq}`：被接受（去空白后）的标题，与它在会话事件日志里的序号；标题进日志是提交点并即时发布在线观察 `sessionTitle`（映射为 DSH `session/title` 与 `title` 投影变更），没有事件日志时拒绝 |
| `host.session.appendCommandEvent` | `{sessionId, event}`（`event` 为 `run` 或 `done`） | `{seq}`：把会话级命令生命周期事件（`CommandEventRecorded`）写入会话事件日志并即时发布在线观察 `command/run` / `command/done` |

目录项为 `SessionFact`：`sessionId`、`workspaceId`、`title`（仍是新建占位时缺席）、`status`、`createdAt`、`updatedAt`、`messageCount`、`isRunning`、`hasUnreadCompletion`，以及创建时选定目录 `cwd`（没有则缺席，不用工作区根目录代替）。事实与 App Server 的 `SessionSummary` 同源，运行态用同一判定。

`host.session.changed` 是 daemon 发往当前宿主的单向通知：`{hostGeneration, factsSequence, resync, changes: [{kind: "added"|"changed"|"removed", sessionId}]}`。总线上的会话生命周期、标题、查看、轮次事件在 100 ms 窗口内按会话合并，`removed` 优先于 `added` 优先于 `changed`；总线落后时 `resync` 为 `true`，宿主重读整个目录。`factsSequence` 在 daemon 进程内单调递增，读取方法先读序号再读目录，所以读到的序号之前发出的通知已被该次读取覆盖；通知只说明哪个会话变了，宿主再用 `host.session.get` 取权威事实。序号缺口、变更数超过 256 或读取失败时宿主重读整个目录，失败后 1 秒重试一次。

内部读动作 `host.session.history` 供共享宿主构造会话读投影。请求携带 `sessionId, after, limit` 及可选 `subagentTaskId`，首次返回 `through` 作为持久水位，续页带上原水位和 `nextAfter`。游标使用 canonical 序号。每页至多 256 条，并受传输字节限制。仅接受 daemon 中可用的用户会话，并在返回前复核。封存和增量文件在提交锁内选定及打开，锁外解析，读取期间的检查点轮转沿同一水位继续。旧检查点已截断原始前缀时返回错误及最早保留序号。提供 `subagentTaskId` 时只重放该子代理任务的只读事件视图（首条为 `subagent/descriptor`），与父会话主线观察隔离；在线 `host.session.committed` 对触及子代理的事务额外附带 `subagentTaskId` 推送子代理观察。

历史读取用 canonical 重放恢复每笔事务的前后事实，与实时通知共用投影函数。当前覆盖输入绑定、轮次边界、已确认的主线/子代理工具活动、助手消息与流记录、请求头与系统提示、子代理目录与描述符、标题变更（`session/title`）以及命令生命周期事件（`command/run`、`command/done`）。返回记录携带零起点连续的投影 `seq`，保留 `sourceSequence` 供回溯。续页会重放前缀以核算投影位置。实时通知仍携带原 canonical 序号。[历史同构回执](plugin-validation/phase-2/shared-observations-2026-11-19/README.md)

插件会话页面的路由调用携带已打开页面的安装摘要、运行实例身份和会话 ID。daemon 用已提交的会话目录和原生执行根解析创建目录、当前工作目录与创建时间，经内部网关传给 Node。客户端提供的同名内部头会被清除。Node 在路由处理期间开放只读 `sessions.get().snapshotEvents()`、`eventAt()`、`header` 和 `workingDirectory.get()`，只接受本次绑定的会话。读句柄在返回、取消或实例撤销时失效。受信 Node 原包的文件业务依然由插件执行。Magi 工具权限与审批有各自的原生入口。

隔离文档路径包含安装摘要和运行实例。以文档完整地址为基址发起的请求剥离挂载前缀后进入原注册路由。浏览器原生加载的 `index.html/<route>` 资源使用同一网关，按 URL 中的安装和实例身份接纳 GET／HEAD，沿用资源响应头过滤和页面隔离。需要会话身份的路由通过页面消息桥绑定该次调用。

沙箱插件页面的流式连接通过可信外壳（`PluginFrame.svelte`）以受准入保护的 WebSocket 升级路由 `GET /api/plugins/{id}/mux?expectedDigest=...&expectedInstanceId=...` 经 `PluginWebGateway::forward` 直连共享宿主私有网关 `/api/remote.mux`（租约随插件实例停用/升级或宿主代次自动吊销），并在 `magi-plugin-v1` 消息桥上向 `client-host/transport.mjs` 暴露标准 `ConnectionWebSocket`（`createWebSocket`），由官方 `RemoteStreamMuxClient` 统一复用 `$events`、`remoteAccess/follow`、`session/control`、`session/follow` 等全部流式端点。

原生命令与 Typert `commands.list/execute` 共用 `MagiCommandRuntime`：Typert 侧经只读 `agent` lookup（每次向 daemon `host.session.get` 核验用户会话存在）绑定受控作用域；每次执行先校验命令所有者与激活租约，再经 `host.session.appendCommandEvent` 持久化 `command/run`（失败则不进入 handler），结算后再落盘 `command/done`。正式后端服务已合并入单一 `session-reader.mjs` 实现：`ctx.sessions` 在路由调用期维持严格隔离与吊销契约，在全局后端同步暴露全部用户会话的只读句柄（`id`/`header` 与已加载历史同步可读，未加载历史抛出明确指引 `ctx.sessionQuery` 的错误）；`ctx.sessionPersistence` 提供只读代理并拒绝写操作（`SessionReadOnlyError`）；`ctx.sessionQuery` 继承官方 `@deepseek-ai/dsh-session-query::SessionQueryEngine` 自动同步最新水位，`searchSessions`/`searchEvents` 返回官方标准码 `SESSION_QUERY_SEARCH_DISABLED`。

## 控制动作

| 方法 | 输入 | 回执 |
| --- | --- | --- |
| `package.inspect` | `projectDirectory, directory, name, version, manifestSha256` | 原包身份、真实后端入口、锁定的 DSH 版本、客户端声明是否存在、实际共享 SDK 模块地址 |
| `configuration.parse` | `name, content` | 原始内容 SHA-256 与按锁定 Include 方言解析的条目；不求值、不接纳修订、不加载 |
| `configuration.prepare` | `sourceDirectory, projectDirectory, sources, base, overlays, packages, previous`；可信准备可显式给出 `memoryRoot` | `sourceRoot, sourceNames, sourcesSha256, preparation, graph, executables`；根树、静态来源闭包与原包入口准备，未执行图 |
| `configuration.edit.prepare` | 同准备输入，增加来源 `operation`，见下方整组编辑合同 | `baseSourcesSha256, edits, plan, result`；只编译来源写回，不写文件、不变更运行条目 |
| `configuration.bundle.prepare` | `action, sources, packages, previousPackageDir?, targetName?, enabled?` | 无状态编译 `bundle.patch.yml` / `cordis.patch.yml` 补丁、行声明与运行时版本／平台准入；不写文件、不拉包 |
| `host.pluginManager.open` | 空对象 | 在根 Context 注册公开 `ctx.pluginManager`（`TypertRemoteService`），所有读写委托给 daemon `pluginManager.*` |
| `configuration.apply` | `token, configuration, activations`，运行期动作附带 `instanceAction: { kind, entryId?, targets, activateTargets? }` | 原样返回 `token` 及配置实例的真实状态 `instances`（冲突刷新返回 `refreshConflict: true`），允许 `starting`；应用配置不等于插件就绪或业务成功 |
| `plugin.activate` | `hostGeneration, pluginId, instanceId, activationGeneration, version, entry, config` | Cordis 真实生命周期快照；独立生命周期验证入口，正式配置执行使用 `configuration.apply` |
| `plugin.deactivate` | `hostGeneration, pluginId, instanceId, activationGeneration` | 清理完成后空对象；清理失败明确报错 |
| `plugin.snapshot` | 同上 | 当前生命周期快照，不合成安装或启用事实 |
| `host.ping` | 空对象 | 空对象，仅证明事件循环响应 |
| `host.shutdown` | 空对象 | 释放插件资源后响应并退出 |

`entry` 为安装所有者验证过的不可变原包绝对入口路径。Node 原样导入模块，将其默认导出或命名导出的 Cordis 插件交给同一 Context，不生成包装包或改写代码。`config` 必须是 daemon 已提交的设置快照；Node 不持久化配置或决定启停意愿。

包准备由 `magi-plugin-system::npm::NativePackage` 校验归档完整性、包身份及声明结构。随后 `package.inspect` 核对准备时的原始清单摘要，在真实加载项目的 `node_modules` 中按 Node ESM 导入条件解析入口并验证普通文件；不会执行插件，也不切换到 `require` 条件。原包声明的 Cordis 与 DSH SDK 依赖必须解析到宿主使用的同一物理模块，防止相同版本的第二份模块形成另一组对象身份。版本准入采用 DSH app-boot 的规则：`engines.dsh` 优先，否则检查 DSH peers；包含预发布版本；仅 peer 的 `workspace:^/~/*` 表示当前版本。没有按包、版本的豁免。共享宿主由 Rust 显式传入 `--experimental-import-meta-resolve`，以便使用 Node 原生的父地址解析，不生成解析包装包。

`magi-api::fetch_npm_archive` 复用现有 registry 下载路径，按包名与精确版本核对元数据，并取得 SHA-512 验证后的原包字节。`NpmArchive::stage` 在调用者拥有的空目录展开原包，随后由同一个 `PluginManager` 接纳安装项目。下载及解包未进入配置提交，预览、图标及安装共用校验后的归档缓存。固定 Mgr 两个版本已经走通真实 registry 到加载修订的回归，测试由调用者预先准备共享 SDK 及其锁定依赖。见 `plugin-validation/phase-1/online-archives-2026-11-18/`。

`magi-daemon::plugin_dependencies` 为同一安装所有者准备候选项目。Rust 验证原包后交给受管 npm 工具链，先生成依赖锁，验证归档与 SDK 绑定，再按锁安装且禁用安装脚本。普通依赖由 npm 按原包声明处理。DSH 和 Cordis 从已锁定 SDK 项目共享物理模块，普通依赖保持各自的 npm 解析结果。安装后比对原包文件并验证真实入口。准备期间取消会结束进程树，暂存目录由调用者回收。当前开发回归显式传入 SDK 项目和 npm CLI，发行装配与安装任务入口待接入。见 `plugin-validation/phase-1/dependency-preparation-2026-11-18/`。

`configuration.parse` 使用发行锁定的 `@deepseek-ai/cordis-plugin-include` 的 `entryListSchema`。`!!js` 保留为表达式数据，不执行代码；非法、半写入、循环引用及超出结构或传输限制的来源明确失败。

`configuration.prepare` 只读取 daemon 捕获的来源，使用 Include 的正式补丁合成规则。根来源、补丁层顺序、静态 include 与 group 形成一份未执行的图；必需来源未捕获、循环引用、重复行 ID 或缺少实际包绑定均失败，不补假数据。匿名行沿已接纳图的 `previous` 保持身份，准备后续修订时协调器必须传入这份图。原包业务配置仍是原始数据，表达式在此不求值；动态 include 需要运行上下文时不能猜一个路径。普通模块通过 `package.inspect` 验证，图节点引用唯一的 `artifacts` 元数据；`sourceDirectory` 是可编辑来源，`projectDirectory` 是已准备的执行依赖项目，两者不能混淆。

`PluginManager::prepare_loading_configuration` 捕获来源与当前修订；`accept_loading_configuration` 在 Node 校验后核对来源根、内容摘要和期望修订，以原 `state.json` 原子提交快照与计划。该领域只提交意愿，不声称插件已运行。宿主代次和加载操作序号由同一所有者持久分配；应用回执必须匹配当前修订、宿主和操作。来源已变更、旧代次、旧操作或重复回执不能提交成功。运行投影不持久化为进程仍存活的事实，重启后必须重新应用。

安装所有者通过 `LoadingRuntime::submit_installation` 提交候选执行项目和对已有来源的修改。它按文件摘要在内存生成候选正文，使用新安装项目编译整张图，然后复用原来源编辑日志和配置 CAS。确认接纳前的取消不写文件。已记录意图但还未完成时，候选依赖项目仍由安装事务引用，需保留以供恢复。普通文件通知和插件配置编辑继续使用已接纳项目。见 [安装和来源联合提交](plugin-validation/phase-1/installation-source-commit-2026-11-18/README.md)。

`configuration.apply` 只执行 `PluginManager` 已接纳的图。`token` 包含 `revision, hostGeneration, sequence`；每个 `activation` 包含完整 `executable`、宿主代次、配置实例激活代次与持久激活顺序。相同包的不同配置行拥有不同 `instanceId`；普通配置更新沿原 Cordis fiber 的更新合同，模块／执行上下文替换或重新启用分配新的激活代次。Node 核对图与全部实例绑定后收敛，不读取当前文件来启动额外模块。内部 Loader 和 Include 仅供已接纳图执行，并未向原包公开 `ctx.loader`。

父级 Include／Group 缺少注入服务时，尚未创建 fiber 的子实例按实际父级依赖呈现 `waiting`；服务恢复后使用真实 Cordis 生命周期创建和重新挂接 fiber，不分配假实例。父级挂起使旧调用租约失效，重新挂接不能使旧租约复活。显式自停用仍呈现终止，不冒充等待依赖。

`LoadingRuntime` 序列化同一协调器的图应用，检查回执身份，并在调用取消、结果无法确认或宿主断连时撤销该宿主的配置实例绑定。该运行投影不会改变已提交的期望配置；旧宿主和旧实例通知不能撤销新实例。`Applied` 仅表示该配置完成了一次收敛，消费者仍须读取每个实例的 `starting/waiting/running/failed/stopped` 状态，不能把它解释为全部业务可用。

图应用等待模块导入及 Include／Group 的结构建立，不等待所有业务启动回调。插件可能在启动中修改来源、启用自己依赖的服务；若控制回执等待该插件就绪，下一张已接纳图就无法执行，形成等待环。尚未完成启动的真实 fiber 返回 `starting`，继续沿原 `plugin.status` 发布就绪或失败；`starting` 实例不能取得运行调用租约。显式调用 Loader 的 `await()` 等待全部生命周期与图应用回执是两个含义，不能混为一谈。导入尚未完成时取消图应用，仍属于无法确认的控制动作，须回收宿主；业务处于 `starting` 不表示控制动作仍未返回。

来源接纳在同一串行区间捕获文件和前一张已接纳图，由共享 Node 静态准备，再由 `PluginManager` 核对来源、摘要、期望修订及安装输入，提交后才执行。受信安装所有者通过 `LoadingRuntime::reconcile_sources` 提交不可变安装项目和包绑定，随同一配置修订保存。普通来源编辑和显式刷新从已接纳修订取安装输入。来源与安装输入均未变化且图已经收敛时，回执没有新的 `application`，也不重复执行失败的启动。

`LoadingSourceMonitor` 监视真实来源目录，首次注册后立即重新捕获来源。通知按最新输入合并，OS 要求重扫时重新捕获。接纳错误保留来源并发布 `Rejected` 诊断，等待实际改动后再次校验。`Reconciled` 表示对应配置修订已接纳并应用，插件是否就绪取运行事实。监视器首次接纳使用受信初始化输入，之后每次在接纳锁内重读已提交修订。更换安装项目时继续使用同一个监视器，旧事件也按新修订重新捕获。停止监视器会等待当前接纳结束，直接丢弃时的未知应用结果由宿主失效合同处理。

### 按文件引用确定来源范围

基础文件与补丁层由安装所有者固定。`configuration.prepare` 和 `configuration.edit.prepare` 沿同一编译器解析有效 Include 引用。遇到尚未捕获的来源时，内部回包以 `detail.requiredSources` 列出所需相对路径。daemon 校验路径、类型和大小后补充读取，并保持已经捕获的原文。编译器返回实际使用的 `sourceNames` 和对应摘要。daemon 在原 CAS 入口重核所有候选文件，并将实际来源、图和执行实例描述提交为同一修订。

监视器在尝试读取前登记新路径。文件暂时缺失或内容不合法时保留上一张已接纳图，来源变更通知再次触发读取。撤去引用时对应该文件退出已接纳来源和监视范围，磁盘文件保留。程序化增删引用使用相同来源捕获和提交入口。若编辑写入父文件后中断，原 `pendingEdit` 一并保存新增依赖的读取原文，恢复时核对完整已知前后状态。新增文件在此期间被外部修改会明确冲突，正文保持原样。

配置可以增删指向实际文件的 Include 引用。对缺失文件，`configuration.prepare` 和 `configuration.edit.prepare` 返回所需来源及该 Include 声明的 `initial` 正文候选。daemon 先捕获现有文件。候选文件仍缺失时，在内存校验完整来源和包依赖，再沿来源日志和配置 CAS 提交引用及新文件。创建必须原子排斥同名文件。恢复时仅接受缺失状态或已经写入的预期正文，保留冲突的外部正文。无有效初始配置的缺失文件仍由来源监视器报告并等待实际文件。表达式路径和公开 `ctx.loader` 仍待接入。已有路径监视回归见 `plugin-validation/phase-1/dynamic-sources-2026-11-18/`。

`configuration.edit.prepare` 使用锁定 Include 的实际 journal／patch 所有权算法。官方公开的 TypeScript 源码导出在构建时按原语义编译为 `include-journal.generated.mjs`，只保留纯数据写回规则，不注册另一套服务或执行内核，也不修改任何插件原包。生成和核对命令为该目录的 `include:generate`、`include:check`。配置源的匿名行在补丁应用之前获得稳定 ID，补丁才能正确引用它们；写回时保存对应身份。

`LoadingRuntime::edit_sources` 捕获已接纳来源，拒绝旧修订和未接纳的外部文件变更，调用上述纯准备端口后由 `PluginManager` 保存 `pendingEdit` 意图。候选文件按来源所有权原子替换；全部正文与编译计划摘要相符后，仍由原 `accept_loading_configuration` 提交配置，并在同一提交清除编辑意图，再应用运行图。跨 Include 移动可涉及多个来源，单个文件保存不是配置生效回执。无变更编辑不创建文件事务。

来源接纳与图应用分别串行，锁之间不形成嵌套等待。内部 `submit_source_edit` 只返回已落盘的来源编辑结果及其配置修订，不等待业务执行；将来公开 Loader 的保存回调须使用这份接纳语义，不能等待正在执行该回调的生命周期完成。原生 `edit_sources` 在同一提交后再等待图应用，分别返回本次 `accepted_revision` 和最终收敛修订。新提交唤醒既有来源监视器，不另建执行 worker；在途图应用收到合法旧回执时，不用它结算新修订，而是串行应用最新已接纳图。监视器和应用循环都只读取同一份 `PluginManager` 配置，不重放来源编辑或插件业务。没有来源监视器且没有在途应用时，单独提交仅保存意愿，调用者需要显式发起图应用；公开 Loader 尚未注册，不对插件暴露无人接管的提交入口。

已核对锁定 Loader 1.1.0、Include 1.1.0 和 Cordis 4.1.0 的等待合同：`EntryTree.create/update` 先修改树再同步调用 `commit`，`remove` 返回 `void`；`Fiber.update` 同步发出保存通知，随后保留并运行候选；Include `flush` 等待读写和树同步，其源码明确避免在持久化循环中等待 fiber。因而不能把已有 `commit` 监听器接到远端后就声称准入已完成，也不能把保存回执定义成等待所有插件就绪。公开 Entry／Group 的变更与 Fiber 保存仍须在执行前接到正式接纳入口；本轮没有把内部执行 Loader 直接注册成公开服务。

部分来源已写、部分未写时，普通来源接纳只能提交与已记录完整计划完全相同的图，不能把部分文件组合当成另一张有效图。`finish_loading_edit`／`resume_source_edit` 根据持久化意图恢复已知的修改前／后正文；未知的外部正文保持原样并报告冲突。恢复的是配置文件和配置提交，不重放插件业务调用。当前只实现了内部来源创建／更新／移除及其恢复，还不等于公开 Loader 对象、Entry／Group 方法、Fiber 保存与 Include `flush` 已交付。

### 整组来源编辑与组合顺序

来源操作现有四种，未知字段明确拒绝：`create` 使用 `options, parent?, position?`；`update` 使用 `id, options, parent?, targetTreeId?, position?, expectedConfig?`；`remove` 使用 `id, expectedConfig?`；`reconcile` 使用 `entries, parent?, expectedEntries?`。每种都包含 `kind, treeId`。`reconcile` 是完整成员数组的业务动作，回执 `result` 为该组直属成员的完整 ID 数组；它不是多次独立提交的 RPC 批处理。

整组候选在同一静态准备中计算增删、字段更新、父组变化和顺序。保留条目的身份，匿名新条目分配稳定 ID；重复身份、组外身份冲突、过期数组或未准入模块使整个候选失败。字段写回沿原来源所有权，已删除条目的专属补丁清退；父配置覆盖拥有的组数组写回实际覆盖层。最终序列化的来源会再次编译，完整有效成员数组必须与提案相符，不能仅验证某一行或在部分来源写入后声称成功。

位置按组合后的组计算，包括补丁插入的行。上游 `routeJournal` 不保存同父组内的位置变化，原 Magi 内部准备器也因此会返回成功却没有重排；本实现补齐源布局动作，没有改锁定上游 helper 或原包。跨 append 层的顺序可能要求转移条目的来源所有权，保留其有效值及稳定身份；在可表达目标顺序的前提下保留最多的原有行所有者。布局只改捕获来源候选，由原编辑意图与 CAS 一次接纳，再沿同一应用入口执行。

组排序一次索引和处理整组，不逐行重建所有来源。现有来源诊断由最终计划保留，布局索引不吞掉它们。真实 Node／领域回归已覆盖两个来源同时提交后再更新、启动和回收成员；无变更组不推进修订或重放业务。原包专项与配置准备规模测量见 `plugin-validation/phase-1/group-edit-2026-11-18/`，不代表公开 Loader／Group 对象已经提供。

整组替换使用严格来源比较，保留显式 `null`，不能以运行时认为 nullable 等价为由漏写字段。`update` 的空值清除与整组删除字段会清除实际来源中同一字段的贡献定义，防止只删除最后一个覆盖而重新露出旧值。覆盖来源即使已无成员也必须写回空数组；其来源身份不能随最后一条覆盖一起丢弃。新建／转换组收纳旧成员时，在候选中先撤掉旧位置，再将完整有效子树交给新数组所有者；整体配置替换同步清退其失效的子项覆盖。以上只修改候选来源，文件与图仍沿原事务一次接纳；证据见 `plugin-validation/phase-1/group-values-2026-11-18/`。

以上是开发中的领域与内部端口，目前尚无发行 daemon 的正式协调器调用，也未注册公开 `ctx.loader`。完整公开 Loader/Include 变更、原生设置接线、动态来源及 `flush` 仍未交付，内部编辑与执行回归不能代替这些能力或原包业务验收。根树已按下节接入同一准入与恢复入口，不能把其动作重定向到 Include 子树；公开注册前仍须完整覆盖 Entry／Group 的对象与选项合同。真实 Mgr 原包已分别由文件通知、单行、整组和根树编辑驱动接纳、撤除与重建配置实例，但仍缺少 `loader` 和 `webServer`，业务未通过。

### Loader 根树与文件树

开发中的内部图已统一为版本 2：`graph.root.entries` 是真实 Loader 根的条目，Include 节点以 `source, patches, children` 表示实际捕获来源、有效补丁和子树；`graph.sources` 记录本次来源装配的 `base, overlays`。根条目不再由执行器固定创建。首次准备时，应用创建指向 `base` 的普通 Include 根条目；有已接纳图后，准备器从该图取根意愿，空根也原样保留，不能恢复成默认 Include。这里没有旧开发图的转换或回退分支；它不是已发行数据迁移或 App Server wire 版本变更。

内部操作用 `treeId: ""` 寻址内存根，其他树 ID 是实际 Include 条目的完整 ID。根条目 ID 不加前缀，文件树条目仍为 `<treeId>:<localId>`；组内成员沿 Cordis 的同树身份规则。根的创建、更新、移除、整组替换，以及根与文件树之间移动，都经原准备、配置 CAS 和串行图应用。移动到自身子树、重复身份、过期修订及未准入模块在执行前拒绝。源文件与根候选共用结构、JSON 数据和容量校验。

根配置保存在同一 `PluginManager` 已接纳图中，不生成影子文件或另建根配置管理器。纯根变更可以有 `edits: []`，但图已改变；调用方仍须提交 CAS，不能仅凭文件摘要认为无变更。根与文件同时修改时，原 `pendingEdit` 记录完整计划，文件写后未提交的中断只按已知前后正文恢复；普通通知不能抢先接纳缺少根变更的计划。重开管理器不恢复旧运行投影，新宿主需重新签发代次、准备和应用；恢复配置不重放插件业务调用。

`base` 的文件补丁层先合成，Include 自有的 `config.patches` 随后合成。有效补丁随已接纳节点交给执行树；来源编辑仍区分每个补丁的真实所有者。文件拥有的覆盖写回对应文件；Include 自有补丁回到其父配置，父级是内存根时仅提交根图。原包根 fiber 的配置保存也使用同一 `loading.edit.submit`，不再要求它必须属于文件树。真实 Node、领域恢复、宿主重建及原包证据见 `plugin-validation/phase-1/memory-root-2026-11-18/`。本节没有提供公开 `ctx.loader`、Include `flush` 或动态来源。

每个配置实例的 `activationGeneration` 必须由 daemon 签发并递增，旧身份不可停用或读取新实例。服务消失导致 Cordis 挂起时，当次调用租约立即失效；依赖服务恢复也不会复活旧租约。插件状态、诊断用 `plugin.status`、`host.diagnostic` 通知回传，均带宿主代次及已知的包、配置实例和激活身份。内部状态是运行投影，安装与配置仍由 daemon 管理。

## 宿主发起的配置保存

共享连接现已支持有界双向请求。Node 请求 ID 使用 `h` 前缀，与 daemon 的 `d` 前缀分别关联；回复、超时和断连均释放请求槽位，迟到回复不重新激活调用。来源保存使用内部方法 `loading.edit.submit`，输入是 `caller, expectedRevision, operation`，回执是 `revision, result`，只证明配置已持久接纳。处理器的依赖项目与来源闭包由安装所有者固定提供；请求不能传入安装目录、配置正文或替换来源根。

`caller` 包含 `hostGeneration, pluginId, instanceId, activationGeneration, version, configurationEpoch`。前五项对应 daemon 当前签发的配置实例；`configurationEpoch` 是 Cordis 运行作用域的进程内租约代次，不是另一套安装或激活事实。Node 在作用域失效时撤销租约，重新进入启动／运行状态使用新代次；daemon 从同一连接的实际状态通知维护只读投影，接纳前和静态准备后都复核实例、作用域与来源修订。旧代次通知不能恢复已失效的写入权。投影不持久化，宿主或实例失效后不能据此写入配置。

`LoadingHostServices` 在宿主启动前构造，加载任何图前一次绑定 `Arc<LoadingRuntime>`；使用弱引用避免宿主、服务处理器与运行协调器形成资源环。插件状态在交给展示消费者前进入同一运行协调器；其余方法仍由原服务处理器负责，没有第二个配置写入处理器。协调器退出后明确返回 `HostGone`，不会重绑另一宿主或回到本地保存。

正式配置实例根 fiber 的 `update(config)` 现在先解析候选并按原 Config `simplify` 规则提取来源数据，再经这条请求进入既有来源编辑事务。接纳前不就地修改 Entry 的来源配置、运行 raw/effective config 或调用旧 `internal/commit` 写回；接纳后由既有监视器／串行图应用更新实际 fiber。保存可在插件启动期间调用，回执不等待自身重启。多个显式保存串行并使用最新已确认修订，队列沿传输的 pending 上限约束；失败只结算本次调用，不重放它。来源所有者缺失时明确失败，不能在 Node 本地先应用。

Fiber 提案同时携带调用时的 `expectedConfig`；静态编辑器在当前捕获来源中比较基础配置，防止其他调用的修订回执已到达、但该 fiber 尚未消费新配置时覆盖它。相同执行作用域的连续保存可以引用本作用域刚刚确认的保存意愿；这只是校验提案的瞬时记录，不是第二份配置所有者，不改变对外运行对象。修订或基础配置不符均明确冲突，不自动重试。回执还必须匹配原来源条目身份。

`update(config, true)` 明确表示不保存来源，仍按 Cordis 的实例内运行更新执行；它不改加载来源、安装／启停意愿、已接纳配置或权限。普通子插件的私有配置也不被当成所属原包根配置持久化。此区别保留 DSH 业务语义，不能将实例内状态显示成全局配置已保存。

本节完成的是内部 Fiber 保存与运行领域接线。公开 Loader／Entry／Group 编辑、Include `flush`、动态来源与发行 daemon 协调器尚未完成，仍不提供半成品 `ctx.loader`。本轮实际证据见 `plugin-validation/phase-1/fiber-save-2026-11-18/`。

### 配置保存与树同步

公开对象的内部访问现在与 Fiber 保存共用 `ConfigurationAccess`：一个有界队列串行处理已明确发起的变更及同步。普通提案在调用时复制；Fiber 的连续保存仍在同一队列中引用本作用域上一次已确认的配置。失败回给原调用者并发布诊断，后续显式动作可以继续，不重发旧动作。回执须为有效修订且不早于所请求修订；原始保存与同步各自保留成功含义，队列不维护安装或配置事实。

新增宿主请求 `loading.sources.synchronize`，输入为 `caller, minimumRevision, refresh`，回执为 `revision`。`minimumRevision` 是读取屏障，允许同步更新的已接纳修订，不是覆盖配置的 CAS。处理器核对真实调用实例与运行作用域、最低修订及未完成编辑事务；`refresh: true` 才重捕获实际来源，仍复用同一准备和配置 CAS，并在准备前及提交前复核调用者。`refresh: false` 只收敛已接纳图，不以此声称外部文件已经重读。未完成文件事务须按原恢复入口处理，不能靠同步清除。

同步回执在既有串行图应用确认后返回，不等待每个业务 fiber 启动完成；`starting` 和缺依赖状态仍由真实生命周期呈现。Node 还核对本地已收到相应图。调用作用域失效会结束同步等待，排队时也能立即向调用者结算；在途远端动作依旧按原身份、接纳与执行合同完成，不能宣称撤销了已经保存的配置。迟到同步回执不能恢复旧作用域或推进其可用修订。保存回执仍表示持久接纳，即使其来源变化随后导致实例卸载，也不能将有效保存回执伪装成回滚。

访问绑定从真实 Cordis Context 找到已签发实例；普通子插件可沿所属实例使用合同，其他 Context 不获得该实例的配置写入权。完整对象服务尚未注册，以上不是公开 `Include.flush/refresh` 的交付声明。真实宿主、刷新失败保留及同版原包回归见 `plugin-validation/phase-1/source-sync-2026-11-18/`。

## 加载对象的内部接线

`CommittedTree.objects(context)` 提供实际执行树的内部管理句柄，不向原包注册 `ctx.loader`。Loader 访问入口在读取时取当前修订；Entry、Group 和树视图绑定取得时的修订，来源变更后必须重新解析。句柄同时受实际调用 Context 的运行租约约束；条目被移除、运行代次变化或调用实例失效后不能写入。

来源选项与成员数组是不可变数据；目标条目的 Context 投影保留调用者的资源归属，不能取得目标插件的生命周期所有权。变更发送既有 `loading.edit.submit`，不先修改本地 Entry；需要运行图同步时调用原 `loading.sources.synchronize`。无返回值的编辑也在同一访问队列中执行，失败通过后续同步报告；已报告的失败不会永久污染刷新，也不会被自动重发。Loader 的实时读取与各修订视图共享这些待完成动作。

首次根 Include 声明使用来源文件的绝对文件 URL。它指向真正参与接纳与加载的文档，不能依赖 Node 工作目录，也不生成 profile 投影。固定 Mgr 原包的 helper 已通过这些读取定位并编辑真实候选文件；文件写入回执不代表 Magi 已接纳或插件已停用。对象读写的 Node 回归使用显式注入的测试接纳处理器；正式领域 CAS、恢复和双向请求仍由原 Rust／真实 Node 回归验证。本批没有据此声称原包已通过公开服务完成业务。

Include 的运行文件名和基准 URL 来自已接纳图中的来源路径。路径解释只发生在接纳准备环节，运行时沿该来源注册条目。含 `#`、`%` 或空格的绝对路径因此保持实际文件名。固定 Mgr 原包的启停函数现在也驱动跨进程来源监视测试，文件通知触发接纳，旧实例身份被撤除，再启用时分配新身份。

`Entry.init()` 和 `Entry.refresh()` 可向 daemon 请求重新应用已结束的启用条目。内部消息 `loading.instances.change` 核对调用方配置作用域、已应用修订以及全部目标代次。激活动作签发新代次，再走同一图应用机制。回执表示图应用已完成，实例就绪继续以生命周期消息为准。条目自停保留已保存的启用配置、回收实例调用，并使旧条目句柄失效。其他来源变化不会重放失败条目。目标自己的配置或执行身份有已接纳的变化时，可以为该目标签发新代次。见 [条目恢复回归](plugin-validation/phase-1/entry-reactivation-2026-11-18/README.md)。

`EntryGroup.remove(id, true)`、`EntryGroup.stop()` 与 `Include.stop()` 使用同一命令的运行期终止动作。目标来自已接纳图中相应节点及其后代，批次全量校验后撤销调用租约，再交给 Cordis 回收资源。宿主拒绝在终止批次中更改配置图或激活代次，结束的实例保留可读配置行和已保存的启用意愿。普通的 `remove(id)` 仍由来源事务管理。见 [资源回收回归](plugin-validation/phase-1/loader-disposal-2026-11-18/README.md)。

可执行条目的 `Entry.update(options, create, true)` 先走正常来源事务。发生配置变更时只应用已接纳的新修订。无变更时，使用同一条实例命令刷新当前条目，校验修订与代次后执行锁定 Cordis 的强制更新语义。活动实例保持 fiber 与调用租约，结束实例使用已存在的激活事务签发新代次。已禁用条目保持停用。见 [强制更新回归](plugin-validation/phase-1/loader-forced-update-2026-11-18/README.md)。

## 加载对象与公开 `ctx.loader` 服务

`CommittedTree` 在共享宿主的公开服务隔离域注册正式 `ctx.loader`（继承 Cordis `Service`，隔离于内部 `ExecutionLoader`），通过 `CommittedTree.objects(context)` 为每个调用方 Context 提供受运行租约约束的只读投影与来源编辑提案；同一 fiber 的 `configurationEpoch` 更新时自动续签新作用域句柄，旧代次租约保持失效。支持 `Loader.Intercept`（`inject: { loader: { await: true } }`）挂起与任务清空唤醒，以及 `!!js` 动态 `disabled` 表达式求值为真时结算为 `stopped` 而非 `failed`。

结构载体（`cordis:group` 与 `cordis:include`）的 `Entry.update(options, create, true)` 在无配置变更时按上游边界刷新：`cordis:group` 保持活动后代 fiber 与调用租约，对同树已结束的组内实例签发新激活代次并重新启动，不越界重跑子 `Include` 的独立文件树；`cordis:include` 仅刷新载体自身。`EntryGroup.unlink()` 明确拒绝未提交的内存组摘除，指引使用 `group.remove` 或 `tree.update`。`ctx.loader.exit()` 在同一配置访问队列中排在已提交编辑之后发送 `loading.host.restart`，由监管器按受控代次切换重启共享宿主，不计入异常崩溃计数。

daemon 侧 `PluginRuntime` 实现第 3.6 节的共享宿主监管：周期性发送 `host.ping` 探测事件循环响应与 `rssBytes` 内存上限；异常退出或探测超时按指数退避重启并跨代次切换 `PluginWebGateway`（旧代次页面与在途流立即失效返回 409）；5 分钟窗口内崩溃达到 3 次停止自动重启并进入 `StoppedByCrashLimit`，记录崩溃时的激活中／运行中插件供人工处置后调用 `restart()` 恢复；若崩溃唯一归因于激活期插件（`activating: true`），在下一代宿主执行图前通过正式来源编辑事务自动写入 `disabled: true` 并发出诊断。固定样本 `dsh-plugin-mgr@0.2.12` 与 `0.2.13` 已在真实共享宿主与网关下进入 `running` 状态。

## 插件私有 HTTP 承载

共享宿主的私有 HTTP 承载复用锁定的 `@deepseek-ai/dsh-host-webserver`，由上游处理路由匹配、HTTP 请求与响应对象、资源流和协议升级。宿主在路由注册处绑定实际 Cordis fiber 和插件激活身份。内部请求须携带承载凭据、目标实例和激活代次，校验通过后移除传输头并进入插件处理器。路由注册、活动连接和未返回的资源响应跟随注册作用域回收，迟到的资源正文取消读取。

所有者在网关拥有实际 HTTP(S) origin 后，经认证控制连接发送 `host.webRoutes.open`。Node 绑定仅本机可达的端口，返回端口并在本代宿主提供 `ctx.webServer`。同一 origin 重复打开使用同一承载，冲突的 origin 拒绝。Rust 和 Node 从本次控制握手凭据分别派生私有 HTTP 凭据，应用侧的 origin 与内部端口分开。实例 ID 以 UTF-8 的 Base64 URL 编码传递。控制连接关闭后停止签发请求头，宿主关闭清理 HTTP 连接与监听端口。

插件可注册命名路由、升级路由、独占回退和文档转换。进程内资源读取使用实际调用实例，只能访问该实例的注册。插件文档的结构化注入和 HTML 转换使用锁定上游服务。发行 daemon 仍需接入页面准入、监听入口和宿主管理。目前由跨进程回归入口显式打开服务。固定 Mgr 原包在该入口已解析 `webServer`，仍缺 `loader`。请参见 `plugin-validation/phase-1/web-routes-2026-11-18/control-result.json`。

`PluginManager` 在同一份运行实例绑定里保存从 Cordis 观测到的配置与调用作用域。daemon 的插件页面绑定本次运行实例和调用代次。转发前，`LoadingWebRoutes` 复核绑定并签发可撤销的 `PluginRouteLease`。调用代次随每段连续可运行的生命周期生成，依赖恢复可以保留激活身份，同时必须重新绑定页面。私有 HTTP 请求还携带调用代次，由 Node 在处理器准入处再次核对。

停用或替换配置提交时，撤除原实例绑定即撤销租约。Node 的依赖挂起及宿主断连也会撤销租约。daemon 私有转发在连接及等待响应时监听撤销，收到响应头及读取正文时继续检查。升级连接属于页面调用，两端任一关闭或租约失效即回收双向连接。真实 TCP 回归证实停用提交后、Node 仍报告旧实例运行时，普通流和升级连接已关闭，旧页面的新请求返回 409。依赖恢复后的旧代次拒绝由领域回归覆盖。参见 `plugin-validation/phase-1/web-routes-2026-11-18/gateway-result.json` 和 `plugin-validation/phase-1/web-routes-2026-11-18/admission-result.json`。

## 失败与交付范围

`host.tools.execute`、`host.commands.execute`、`host.web.search` 与 `host.web.fetch` 支持调用取消。daemon 派发前为 `request.cancel` 通知预留队列空间，调用超时或等待被取消时发送原请求 ID。当前代宿主只中止该 ID 的调用，并将 `AbortSignal` 传给实际选定的提供方。正常响应释放预留空间；连接断开或宿主关闭时中止该代的在途调用。daemon 决定业务终态，迟到响应不参与提交。

首次向 daemon 的安装入口提供宿主前，来源监视器须完成第一轮检查。首轮配置被拒绝时，保留监视器和拒绝原因，用户修复同一来源后可以继续接纳。

必需服务缺失呈现 `waiting` 及 `missingServices`；不伪造服务使原包启动。激活异常撤销该包贡献与资源，子插件异常不能留下假运行状态；清理失败使宿主拒绝后续业务，须由监管回收。控制连接断开后排空有界时间，再结束进程；Rust 进程所有者回收整个进程树。

激活／停用控制请求超时或调用方放弃等待时，该控制结果无法确认，Rust 失效连接并回收共享进程，禁止迟到的导入或激活继续注册。原包模块导入（含顶层 await）不能由 Cordis effect 中断，此故障影响整个共享宿主；不是工具或模型业务调用取消的默认策略。其他插件的进行中调用随宿主失效结算，后续由监管决定重新激活，不能自动重放未确定的业务。Node 不因处理失败请求而改用旧宿主。

daemon 的共享宿主装配先完成控制握手和 `webServer` 网关绑定，再开始监视、接纳和应用加载来源。重启时，来源监视在首轮接纳前恢复可确认的编辑事务。如果外部草稿无效，装配仍运行已接纳的修订，来源监视报告拒绝并保留原文。重复通知和修正后的相同正文不会重新应用图。

该装配与崩溃监管已用包内工具链、跨代次监管回归和固定 Mgr 原包验证。候选加载项目视图（在 `loading-source` 原子物化 `package.json`、`.npmrc` 与指向不可变包目录的 `node_modules` 符号链接）、未变更原包的跨候选目录物理入口保持、公开 `ctx.pluginManager`（含 `inspect`、`listVersions`、`listPlugins`、`listBundles`、`readBundleDetail`、`setPluginEnabled`、`setBundleEnabled`、`installBundle`、`waitForInstall`、`cancelInstall`、`removeBundle` 自卸载拒绝边界与原生单步原子 `upgrade_bundle`）已接入 `PluginRuntime`，并通过真实 `dsh-plugin-mgr@0.2.12` → `0.2.13` 跨进程网关 HTTP 业务回归（列表、更新发现、启停收敛、目标两步更新、自更新拒绝、卸载）。当前正式 daemon 启动进入共享宿主，根 Include 的绝对文件 URL 指向真实的加载来源。随包 SDK 需要的独立数据目录由 daemon 分配。配置源由现有修订机制提交，启动过程不再创建 profile 别名。整阶段业务结果见[阶段 1 证据](plugin-validation/phase-1/retirement-2026-11-18/README.md)。

## 阶段 3–9 共享宿主运行时钩子与权威端口（2026-11-20）

- **daemon → Node 运行时钩子与执行循环 RPC（均可取消）**：
  - `host.tools.preExecute`：`tools/pre-execute`（含改写 `arguments` 透传）+ `ctx.approval`（超过内置 UI 监听基线时）+ 单调 `tools.guard`；
  - `host.tools.postExecute`：`ctx.toolResultPruner` + `tools/post-execute` + `tools/result`；
  - `host.tools.filterVisible`：冷会话按需通过 `withSessionAgent` 激活 `agent/created` 并执行 `tools.restrict` 作用域单调裁剪；
  - `host.systemPrompt.assemble`：官方 `SystemPrompt.assemble` + `renderPrompt` / `renderContextSections`；
  - `host.approval.ask`：官方 `ApprovalService.decide`（冷会话先经 `sessionQuery.readSession` 预加载历史）；
  - `host.compaction.before`：`compaction/before` 瀑布流裁决；
  - `host.session.projectMessages` / `host.llm.extendRequest` / `host.llm.interceptStream`：消息投影（接入 `RequestHistoryProjection::view`）、`agent/pre-step`、`deepseekLlmApiExtensions` 与 `llm/stream` 环绕（接入 `model_step::invoke_recorded`，支持正文改写与有界重试及 `ModelStreamingDelta.replace`）；
  - `host.agentLoop.{run,cancel,resume}`：由 daemon（`LlmTaskDispatcher::execute_plugin_agent_loop`）在显式选定非 `magi-native` 执行循环时接管主线 Turn（不可用或失败按 §3.1 明确报错，绝不静默回退）。
- **Node → daemon 能力与状态广播通知**：
  - `host.hooks.capabilities` 在监听器/守卫/限制/系统提示段/`agent/created`/`llm/stream` 监听变化时推送布尔快照，daemon 侧 `SharedDshExtensionHooks`（挂载于 `LlmTaskDispatcher`）在对应能力为 `false` 时 O(1) 跳过跨进程 IPC；
  - `host.agentLoop.changed` 在插件注册/注销/切换执行循环时同步给 daemon `PluginServicesBridge::agent_loop_snapshot()`。
- **Node → daemon 权威读写入口新增**：
  - 写入口：`host.session.setAccessProfile`（切换活动目标访问档位）、`host.goals.{list,get,create,update,delete}`（经 `PluginSessionControl` 复用 `SessionStore` 唯一写入口与检查点）、`host.terminals.{list,create,write,resize,read,close}`（直连原生 Rust PTY `TerminalSessionManager`）、`host.worktrees.{create,remove}`（直连原生 `magi-git::GitService`）；
  - 只读桥接：`host.worktrees.list`（读 `state.session_code_contexts`）、`host.mcp.list` / `host.mcp.resources`（读 `settings_store("mcpServers")` 与 `mcp_connections`）、`host.browser.snapshot`（读 `BrowserAuthority::snapshot()`）、`host.subagentModelSelection.get`（读 `agent_role_registry` 与 `settings_store("agentRoles")`）。
