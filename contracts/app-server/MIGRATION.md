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
