# App Server 协议生成约定

`PluginInstalled.disabledBy` 为 daemon 持久化的停用来源（插件 ID、名称与原因），不是
客户端自报的运行状态。管理者在 `PluginSetEnabledRequest` 中可提交停用原因；来源身份由
daemon 根据已授权调用者补充，排空后与 enabled/active 一次提交。启用、用户显式停用及
包版本替换会清除该来源。可选 `expectedState` 使用 `PluginLifecycleState`，与
`expectedDigest` 共同核对启停意愿及停用来源，写入前、排空后都须一致；防止旧诊断页
恢复已被用户或其他管理操作接管的停用。原安装状态的增量 `disabled_by` 缺省为 None，
无需另建日志或第二份 profile 状态。

`PluginInstalled.translationRuntime` 是上游安装归档与当前随产品发行的运行组件的只读比较。
`current` 表示完整组件身份相同，`updateAvailable` 包含缺少组件身份的既有归档，
`unavailable` 表示当前组件不能读取并携带恢复原因；不从上游版本号推断该状态。
身份覆盖业务转换源码、锁文件及实际页面产物，由发行构建生成，不新增持久化目录。
`PluginUpstreamPreview.translationRuntime` 在更新预览中返回同一比较，安装继续使用原
`PluginUpstreamRequest`、精确来源、候选摘要和生命周期入口；同版但摘要变化仍是升级。

`app-server.schema.json` 是 App Server 控制协议的唯一 wire schema 来源。运行
`npm run protocol:generate` 会生成：

- `crates/magi-app-server-protocol/src/generated.rs`
- `web/src/shared/app-server-protocol.generated.ts`

生成文件禁止手工修改。协议变更必须先修改 schema，再生成并执行
`npm run protocol:check`。

Rust 的 `magi-app-server-protocol` 只在 `lib.rs` 保留协议行为层：版本兼容性、
请求 ID 行为、错误构造和消息分类。所有 envelope、初始化、能力、浏览器参数和
错误 wire DTO 都从 `generated.rs` re-export，业务代码通过生成类型反序列化，
再转换为领域类型（例如 `AccessProfile`）。

TypeScript 侧的 App Server 客户端必须从 `app-server-protocol.generated.ts`
导入 envelope 和方法参数类型。HTTP、SSE、Desktop WebSocket 和 Renderer
Bridge 可以拥有不同传输实现，但不得重新定义同形状的 wire DTO；事件 payload
也必须沿用 canonical session/turn/item 语义。

插件系统的 `Plugin*` 清单与 DTO 也定义于本 schema，复用同一生成入口。清单类型
定义本身不增加可调用的 App Server method；能力准入仍由 daemon 的插件安装、授权
与激活状态决定。`magi-plugin-system` 使用生成类型并对包资源与作用域补充语义校验；不得
在插件 SDK、HTTP 适配器或 UI 手写另一份清单结构。

`PluginContributions.client` 与 `client:["persistent"]` 为常驻客户端的增量合同。
入口必须引用已声明视图；启动仍须满足 daemon 的激活与全部授权事实。旧包不声明该
贡献，继续按页面生命周期挂载。新增权限须在升级预览中展示并重新授权，不自动授予。
常驻客户端只在当前工作台窗口存活，视图切换复用同一隔离文档；不改变 daemon 的
Session、Turn 或草稿事实，也不允许未授权的后台会话写入。

`attachments:["images"]` 与 `PluginImageAttachment*` 是独立授权的私有图片存储增量合同。
通过 Extension Protocol 的 `images.store` 协商使用，不新增 App Server 方法。旧插件
无此权限，不能使用实例级后台图片存储；普通 blob 与会话调用的租约保持不变。图片
暂存按实例失效，已提交图片由原资源元数据与载荷文件共同恢复，升级数据重置沿原事务处理。

`sessionRead:["directory"]` 与 `PluginSessionDirectoryRequest/Entry/Directory` 是独立授权的只读增量合同。
通过 Extension Protocol 的协商特性 `session.directory` 使用，不新增 App Server 方法。
旧插件无此权限，不能读取跨会话目录；已授权插件每次读取当前 daemon 用户会话元数据，
最多 200 条并以稳定 sessionId 分页，不包含消息、执行配置、外部工具会话、文件路径或凭据。

`PluginDshCompatibilityProjection` 与 `PluginDshVersionExemptionRequest` 是新增的插件兼容策略 DTO，
复用资源 CAS 持久化。runtimeId 绑定当前随包锁文件，豁免只对精确包版本和当前运行时生效；
管理写操作需要独立 compatibility 授权和风险确认，不改变已有安装摘要、能力或权限准入。

`PluginReconcileRequest` 为扩展服务的增量输入 DTO，经 `plugin.reconciliation` 协商使用，不新增 App Server 方法。
显式选择至多 512 个唯一已安装 ID，只核对已提交的激活意图并等待原监管器就绪，返回既有插件目录。
它不修改启停、授权或安装事实；需要 lifecycle 授权，后台调用另需 background，期间版本／状态变化明确拒绝。

`PluginListRequest.includeUpstreamManifest` 与 `PluginInstalled.upstreamManifest` 是只读增量字段：
旧请求默认省略原包元数据；显式请求时仅返回 daemon 当前不可变安装归档内通过包名/版本校验的
package.json，不包含插件可写配置或凭据。旧包缺失该文件时保持未知，不从 Magi 清单补造。

同一显式请求还提供 `PluginInstalled.upstreamArchivePath`：daemon 当前不可变安装归档的
绝对路径，受信 Node 宿主读取前须核对同一投影的 digest。它只用于调用期间的原包只读
文件快照，不生成第二份安装树，不表示该包已激活或已物化。默认列表不返回该路径；
目录读取、实例租约和管理权限仍沿原入口，路径不允许作为安装或写入提案。

`PluginRouteCallRequest.headers` 是可选的增量字段，用于插件页面向其私有后端携带应用头。
旧请求省略时保持原行为；Magi HTTP 身份头不随该字段转交。DSH 路由适配器校验头名、
值、数量和大小，并拒绝覆盖宿主身份及传输控制头。

`PluginRouteCallRequest/Result.bodyBase64` 为原始正文的字节表示，与非空 `body` 互斥；
`PluginRouteCallResult.headers` 提供应用响应头，`PluginRouteMethod` 增加 HEAD／OPTIONS。
所有载荷共用原路由执行、实例授权和取消入口。解码后字节最多 2 MiB，JSON 消息最多
3 MiB 加 64 KiB；daemon 在分发前校验，Node／页面读取流时也有界并响应取消。响应不携带
Set-Cookie、宿主身份或传输控制头。未使用字节表示的原生 SDK 调用继续使用结构化 body，
不是另一个执行器或文件存储。图片附件的分块服务和权限保持独立。

`ModelAttempt.transport` 是可选的逐次 HTTP 证据，包含实际请求摘要、HTTP 状态、
已报告的 Token usage 和 transport outcome。旧记录或非 HTTP 执行器缺失时保持
未知。它不替代 Step/attempt 的逻辑状态；HTTP 已返回不表示模型输出已被接受。
用量事件以 attempt ID 去重，通过 `parentCallId` 关联逻辑调用；统计调用数按传输
尝试计数，不再把重试的已报告用量合并或遗漏为一次最终响应。

`ModelPromptFrame.historyProjection` 是可选的增量字段：请求视图对已有工具结果的改写量
（`demotedToolResults` 降级条数、`staleFileFacts` 失效文件事实条数），由 daemon 在发送前冻结。
`CacheInvalidationReason` 因此增加 `tool_history_demoted` 与 `file_facts_refreshed`：已发送历史
的改写有对应计数增长时按它们归因，没有任何声明的改写仍为 `history_rewritten`。旧记录缺失该
字段时按未声明处理，读取方不得据此推断“未改写”。

`HistoryProjection.extensionRewrites` 是可选的增量字段：当前生效的受信扩展改写记录
（`pluginId`、`contributionId`、`hook`、被替换区间 `[from, to)`、原文与替换内容的 SHA-256）。
`CacheInvalidationReason` 因此增加 `extension_rewrite`：生效记录集合相对上一请求发生新增、
替换或撤销时按它归因（优先于其他声明原因）。缺失该字段的旧记录等同于没有扩展改写。

网页模型专属配置 DTO、发现/对话绑定字段和浏览器内部命令已从 schema 与生成物中移除；相关端点不再注册。标准 MCP 令牌、外部会话与审批合同保持独立，本地 stdio 中继始终进行令牌握手。

`ModelTransportRecord.responseTiming` 为可选的正文流式计时事实：`firstContentMs` 从实际 HTTP
发送到首段正文网络接收，`contentDurationMs` 为首末正文网络接收间隔，`textOutputTokens` 只在 Provider
输出能扣除推理且不混入工具参数时提供。计时在 HTTP I/O 线程采集，不计本地排队、持久化或 UI
回调耗时。缺失字段表示未知；旧记录与非流式调用不补零。逐次回执沿 canonical Step / 辅助调用
账本持久化，恢复投影使用原 timing，不能从历史时间戳反推速度。
同一批 HTTP 字节内解析出的 SSE 事件共享接收时间，不按解析 CPU 时长制造速度。该值为客户端
观测值，受网络和上游缓冲影响。计时与最终用量配对后冻结；在此之前 Provider usage 快照允许
修正计数或撤回中间缓存明细，HTTP outcome 冻结后保持整份回执不可变。

`ModelTransportRecord.usageObserved` 为可选的 Provider 用量存在性证据。新 HTTP 记录起始为
false，只有解析到实际 Token 数字段时才变为 true；缺失或空 usage 不得由 adapter 的零默认值
补造。旧记录缺少此字段且计数全零时保持未知，不能事后当成已确认零用量。

`PluginRuntimeCatalog.skills` 与 `PluginRuntimeContributionKind.skills` 是运行期技能的增量合同，
经 Extension Protocol 的 `plugin.skills` 协商提交。技能仍复用 `PluginSkillContribution`，
沿同一目录的就绪、CAS、实例租约和撤销规则，不新增 HTTP 目录或本地设置写入。

`sessionWrite:["context"|"wake"]`、`PluginSessionMessage*` 和队列的可选 `pluginMessage` 是会话业务结果的增量合同，协商特性为 `session.messages`，不新增 App Server 提交方法。旧插件未声明此授权不能调用；DSH 重新安装或在线更新时展示新增权限并重新授权。插件消息复用 SessionInput 与 canonical 事务，UI 不能把它解释为用户输入；旧数据无该元数据时保留原语义。状态布局推进到 20，阻止不认识不唤醒上下文的旧 daemon 消费新版队列。

`PluginGithubSource` 与安装请求、预览、来源投影的 `github` 为 GitHub 原包来源增量合同。
省略 `github` 时 `package` 仍为必需的 npm 包名；指定仓库时 `package` 可选，提供即核对
原包名称。`reference` 保留用户选择的分支／标签，`commit` 是解析后固定的提交。确认安装
必须携带预览提交和摘要，不能重新解析浮动分支。来源和提交保存在同一 `PluginSource::Upstream`
安装事实内；不同仓库和 npm 同名包不能互相冒用升级。管理插件仍使用同一安装任务、授权
和中断回执，不新增包管理执行器、安装状态文件或 App Server 方法。
