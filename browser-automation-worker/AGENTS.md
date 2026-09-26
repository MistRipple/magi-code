# Browser Automation Worker 规则

本文件补充仓库根目录 [AGENTS.md](../AGENTS.md)。Worker 与 Electron/Authority 的职责以 [Browser Runtime 设计](../docs/browser-runtime-design.md) 为准。

## 职责与调用

- Worker 负责页面快照、DOM/节点引用、自动化交互、诊断和截图算法；Electron Main 拥有 guest、WebContents、CDP 连接及物理生命周期命令；Browser Authority 拥有逻辑 Tab、Surface、租约和控制权。
- 沿用唯一受控链路：`BrowserAuthority -> BrowserHostClient -> Desktop Control Server -> Automation Worker -> Main cdp_request -> BrowserSurfaceManager -> guest webContents.debugger`。不要让 Worker 绕过 Desktop Control 直接创建 guest，或把 Main 的物理生命周期职责复制到 Worker。
- 每个请求、结果和异步事件遵循当前 binding 的 Desktop/Worker epoch、Tab、Surface 和 `navigationRevision`。文档导航、Surface 替换或 Worker 重启后，旧 node/frame/CDP 引用和 pending result 不得作用于新页面。
- 命令只等待自己的完成条件，不将所有操作统一等待网络空闲。取消、超时和断连按实际底层能力收口；已开始的写操作没有可靠取消句柄时不得假报取消后安全重试。
- 所有权、安全认证和持久化规则以 Authority、Main 与 daemon 的实现和 schema 为准；Worker 不授权远端 Web/Mobile 会话借用本地 Desktop Host。

## 验证

- 基础检查：`npm run check --workspace @magi/browser-automation-worker`。
- 行为变化时运行 `npm run test --workspace @magi/browser-automation-worker` 或对应测试；涉及完整工具通路时运行 `npm run test:browser-core`。
- 涉及真实 Chromium 操作、取消、导航或截图时，按 Browser 设计中相应场景做运行验收；Worker 单测不能证明 Electron guest 与 daemon binding 协作正确。
