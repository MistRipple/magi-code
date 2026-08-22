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
