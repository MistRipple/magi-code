# Web 工作台规则

本文件补充根目录 [AGENTS.md](../AGENTS.md)。产品与架构事实以 [工程约束和运行入口](../docs/README.md)、[Browser 设计](../docs/browser-runtime-design.md)及源码为准。

## 运行与验收

- Web、Rust daemon 的日常开发和真实浏览器验收都通过 `./scripts/dev-daemon.sh`。主入口是 `http://127.0.0.1:38123/web.html`；API、SSE、会话、任务和设置请求应回到同一个 daemon。
- daemon 开启 `MAGI_WEB_DEV=1` 后会复用或启动 Vite。Vite 默认监听 `0.0.0.0:3000`，但它只是 daemon 托管的模块热加载后端，不是产品或浏览器验收入口。
- 除了孤立调试 Vite 配置，不要直接运行 `npm --prefix web run dev`。该入口当前会失败退出。不要让 Vite 改用 `3001` 等漂移端口；先确认已有 daemon/Vite，再处理占用。
- 可通过 `MAGI_WEB_DEV_HOST`、`MAGI_WEB_DEV_PORT`、`MAGI_WEB_DEV_ROOT` 调整 Vite；通过 `MAGI_HOST`、`MAGI_PORT`、`MAGI_STATE_ROOT` 调整 daemon。端口调整从 `./scripts/dev-daemon.sh` 启动。
- 真实验收按需确认 `curl -I http://127.0.0.1:38123/web.html` 和 `curl http://127.0.0.1:38123/health`，然后在 daemon 主入口检查用户流程。
- 生产近似验证使用静态模式：先 `npm --prefix web run build`，再运行 `cargo run -p magi-daemon-app`。

## 状态与协议

- Web 是 daemon 业务事实的展示与交互层。Session、Turn、任务、权限和 Browser 逻辑状态应通过既有 API、SSE 或 App Server 契约读取和提交；不要在 Web 建立第二套持久化权威。
- Browser Tab 和 Browser 记录的权威归 `magi-daemon` 的 Browser Authority。Web 的 `localStorage` 或 Desktop 的 `sessionStorage` 只保存设计允许的视图恢复资料，不得成为 Browser 实体或并行状态源。
- 不从 UI 推断任务终态、后台状态或授权成功；消费 daemon 的事件和响应事实。跨层字段变更先读对应 schema、DTO 和调用方。
- UI 文案沿用现有 i18n 字典和组件模式；不要绕过当前本地化方式硬编码产品文案。

## 设置页一致性

所有设置分类共用 `src/styles/settings.css` 的同一套结构，新增或修改设置页时只用这些共享类，不要在分类内另起一套标题、卡片、行或按钮：

- 页面框架：分类根元素使用 `.apple-scroller-proxy`（或 `.settings-frame`），内容列居中、最宽 880px，不要在分类里再写自己的左右留白。
- 分区：`.settings-section` + `.settings-section-header`（`.settings-section-title` 14px 加粗，其下 `.settings-section-desc` 12px 灰色；操作放在 `.settings-section-actions`）。分区之间只有一条细分隔线；每个分区最多一个主按钮。
- 内容：卡片用 `.settings-card`，列表用 `.settings-rows` / `.settings-row`（`.settings-row-copy` 内放 `.settings-row-title` 与 `.settings-row-desc`），标签用 `.settings-chip`，状态点用 `.settings-dot`，提示用 `.settings-notice`，空状态用 `.settings-empty`（一行说明，不用大图标）。
- 控件：按钮用全局 `.btn`（设置页一律 `.btn--sm`，主操作 `.btn--primary`，其余 `.btn--secondary`，破坏性 `.btn--error`），输入框用 `.form-input`，开关用 `Toggle`。
- 主从布局（插件页）：整体是一块带边框面板，左侧按状态分组、可独立滚动的列表，右侧在同一页面展示所选项的详情与全部操作，默认选中第一项，不用弹窗；安装/更新这类首要动作用 `.btn--lg`，让「未安装」「有更新」一眼可辨。
- 分类内的二级标签用 `.settings-subtabs` / `.settings-subtab`（下划线样式），不使用胶囊分段控件。
- 内容面板不填充背景色、不加阴影、不用渐变，只用 1px `var(--border)` 边框与 10px 圆角；背景色只留给控件（输入框、分段控件、选中项）。

## 验证

- 类型与 Svelte 检查：`npm --prefix web run check`。
- 行为变化时运行 `web/package.json` 中对应的 `test:*` 脚本。仅改 Web 组件时不默认运行整套前端测试。
- 改动 App Server 协议或其生成类型时，再执行 `npm run protocol:check`；Browser 工具目录/schema 变更按 [`contracts/AGENTS.md`](../contracts/AGENTS.md) 执行对应验证。
- 涉及跨层 Browser 或 Turn 流程时，按设计文档列出的职责和失效场景验收；只通过 UI 静态检查不代表跨进程行为已验收。
