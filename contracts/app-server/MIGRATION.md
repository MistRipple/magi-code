# App Server 协议生成约定

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
