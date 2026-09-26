# Electron Desktop 规则

本文件补充仓库根目录 [AGENTS.md](../../AGENTS.md)。Electron 边界的权威说明是 [工程约束与运行入口](../../docs/README.md) 和 [Browser Runtime 设计](../../docs/browser-runtime-design.md)。修改前核对源码和 `contracts/desktop-browser` schema。

## 所有权

- `magi-daemon` 拥有 Session、Turn、任务、权限、Browser 逻辑 Tab 和持久化事实。Electron Main 负责原生窗口、单实例、宿主生命周期、受控 daemon sidecar，以及 Chromium guest 的安全策略、WebContents 控制和恢复；不要把业务状态复制进 Main。
- 正式 Desktop 由 Electron Main 托管 daemon sidecar。开发运行时先通过 `./scripts/dev-daemon.sh` 启动 daemon，再按 [开发入口说明](../../docs/README.md) 构建并运行 `npm run desktop:dev`；不要因此启动第二个 daemon。
- 主窗口关闭默认隐藏到系统托盘，daemon 继续运行；退出事务遵循现有 Main 生命周期，不能把隐藏、正常退出和崩溃处理混为一谈。

## App Renderer 与 Browser guest

- 唯一 `BrowserWindow` 直接承载可信 App Renderer。Renderer 管工作台 DOM、右栏布局和 Tab 内容槽；当前 Browser Tab 只在内容槽中渲染一个真实 Electron `<webview>`。
- Main 管 guest 的安全配置、隔离 partition、导航策略、下载、权限、popup、受控 CDP 和恢复；`<webview>` 元素及其 DOM 附着/释放由 App Renderer 生命周期管理。Main 不管理右栏布局，不读取 DOM 坐标，不创建网页显示用 `WebContentsView`，不调用网页 `setBounds()`。
- Browser 工具物理命令与页面算法遵循 `docs/browser-runtime-design.md` 的唯一链路和角色划分。不要在 Main、Worker、Renderer 中实现第二条浏览器控制链，也不要增加独立 BrowserWindow、子 Tab、截图投影或原生 Overlay 显示路径。
- guest 注册和命令必须校验当前 Desktop/Window/Surface/Tab/导航身份。失效身份、旧导航或旧 Surface 的迟到事件不得更新当前 Browser 状态；已开始且无底层取消能力的写操作不得伪装成成功或自动重放。
- Desktop IPC 的类型和消息以 `contracts/desktop-browser` schema 为准。不要在 preload、Main、Renderer 或 Worker 单独发明同形状 DTO、频道或授权判断。
- `displaySize` 只表示当前内容槽的瞬时宽高，用于 fixed viewport 的 Chromium 显示比例；不承载坐标，不写回窗口布局或持久化状态。

## 验证

- 基础检查：`npm run check --workspace @magi/desktop`。
- Main、IPC、窗口、guest 生命周期行为变化时运行 `npm run test --workspace @magi/desktop` 或对应测试；schema 变化还需执行 `npm --workspace @magi/desktop-browser-contracts run check`。
- 涉及真实 Electron/Chromium 页面显示、导航、安全、恢复或控制行为时，需按 Browser 设计执行真实运行验收；单靠 TypeScript 检查或源码测试不能代替物理 guest 验收。
