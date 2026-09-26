# 协议与 Schema 规则

本文件补充仓库根目录 [AGENTS.md](../AGENTS.md)。协议语义和兼容边界以各自 schema、迁移文档及 [Browser Runtime 设计](../docs/browser-runtime-design.md) 为准。

## 唯一来源与消费者

- `contracts/app-server/app-server.schema.json` 是 App Server wire DTO 的唯一 schema 来源。生成的 Rust/TypeScript 文件禁止手改；按 [`App Server 迁移说明`](app-server/MIGRATION.md) 修改 schema、生成代码并验证。
- `contracts/desktop-browser/` 是 Desktop Browser IPC/Desktop Control/Worker IPC 的 schema 来源。浏览器协议类型由 schema 生成或校验；Browser 工具目录的生成物以 `browser-tool.schema.json` 和现有生成脚本为准。
- 协议改动要追踪 daemon/API、Electron Main/preload/Renderer、Automation Worker、Web 及持久化/replay 等实际消费者。不同传输可有不同实现，但不能各自定义同形状 wire DTO 或旁路事件。
- Magi 协议保持冻结和收敛。只按对应协议已有兼容规则做增量修改；不要随 UI 需求扩字段、重复字段或创建第二份合同。涉及破坏性持久化或 wire 语义时先阅读迁移文档和调用方恢复逻辑。
- Desktop Browser 初始化和能力协商、Desktop 可信身份、Web/Mobile 降级与权限检查遵循 Browser 设计；客户端自报平台不能提升服务端权限。

## 更新与验证

- App Server schema：修改后运行 `npm run protocol:generate` 和 `npm run protocol:check`，检查生成的 Rust、TypeScript diff 及全部消费者。
- Browser 工具 catalog：按 `scripts/generate-browser-tool-catalog.mjs` 的源文件关系修改 schema/脚本/生成物，再运行 `npm run browser-tool-catalog:generate` 和 `npm run browser-tool-catalog:check`。
- Desktop Browser 其他 schema：运行 `npm --workspace @magi/desktop-browser-contracts run check`，并运行受影响的 Desktop/Worker/daemon 定向检查。
- 只改生成物时，先确认它与源 schema 一致；不得以手改生成物替代 schema 变更。
