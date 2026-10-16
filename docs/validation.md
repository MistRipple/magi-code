# 脚本与验证入口

脚本的执行入口以各 workspace 的 package.json、Cargo target 和 .github/workflows 为准。本文区分离线检查、真实集成验收与发布，不把源码断言当作真实 Chromium 或 Provider 验收。

## 日常开发与构建

| 任务 | 入口 | 条件 |
| --- | --- | --- |
| Web + daemon 开发 | `./scripts/dev-daemon.sh` | daemon 托管 Vite，使用 http://127.0.0.1:38123/web.html；构建缓存清理规则见根 AGENTS.md |
| Web 生产资源 | `npm --prefix web run build` | 使用根 npm lockfile 安装依赖 |
| Desktop 构建/开发 | `npm run build` / `npm run desktop:dev` | 开发宿主连接已有 daemon；见 docs/README.md |
| 安装包 | `npm run desktop:package` | 见 release-process.md；不等于发布 |
| 协议生成与校验 | `npm run protocol:generate` / `npm run protocol:check` | App Server schema 为唯一来源 |
| Browser 工具目录 | `npm run browser-tool-catalog:generate` / `npm run browser-tool-catalog:check` | 从 browser-tool.schema.json 生成 |
| crate 架构图 | `npm run docs:architecture` / `npm run docs:architecture:check` | Cargo metadata 生成 docs/architecture.html；crate 注释和业务流程必须引用实际 workspace 成员；CI 发布预检执行一致性检查 |

## 本地定向回归

- Web：`npm --prefix web run check` 和 web/package.json 中对应的 `test:*`。`test:transport` 包括实时传输、App Server 客户端与滚动协调回归。
- Desktop：`npm run check --workspace @magi/desktop`、`npm run test --workspace @magi/desktop`。
- Worker：`npm run check --workspace @magi/browser-automation-worker`、`npm run test --workspace @magi/browser-automation-worker`。
- Rust：`cargo check -p <package>` 和受影响的测试目标。Git 工作流用 `cargo test -p magi-git --test workflow`，在临时本地仓库验证分支/worktree，不依赖开发者路径或远端仓库。
- Browser 静态边界：`npm run test:browser-core`，同时检查 guest 注册、唯一控制链及下载清理生命周期。
- 协议与发行边界：`npm run release:guard`，包含生成物、Desktop Browser schema 和发行依赖检查。

`npm test` 运行 Desktop、Worker 和 Web 套件，用于明确要求的整体回归或发布预检；日常只运行受影响的 test:*，不默认执行全套。脚本语法可用 `node --check <script.mjs>` / `bash -n <script.sh>` 检查，语法通过不能替代行为验证。

## 真实集成验收

| 入口 | 覆盖范围与前置条件 |
| --- | --- |
| `npm run test:electron-browser-viewport` | 已打包 Electron、独立状态根与本地 Provider；真实 AI 工具验证固定/自适应视口、窄电脑/手机横屏、DPR/触控、缩放点击、光标常驻/导航/折叠恢复、拖动与网页点击接管后自适应、交还、320px 工具栏/菜单边界和后台页面不改布局 |
| `npm run test:electron-browser-actions` | 从当前源码构建 Worker 与 SurfaceManager，在独立临时状态目录启动真实 Electron webview；验证敏感值、快照预算、点击、输入、焦点、遮挡及导航引用失效；需要本机图形环境 |
| `npm run test:electron-cursor-presentation` | 从当前 Main、Worker 和 Svelte 光标源码启动真实 Electron guest；验证移动完成确认先于原生点击和输入框焦点、原地点击、隐藏/减少动态效果，以及移动途中用户接管、取消、导航和迟到确认；使用本地测试页与独立临时状态目录，需要本机图形环境 |
| `npm run test:electron-cursor-desktop-live` | 当前源码的完整开发 Desktop + daemon + 原生 Worker，访问真实 Example/IANA、Wikipedia 和 Rust Book，验证对话触发工具、光标滑行先于原生点击、输入/替换/搜索、右栏折叠恢复、移动中接管和交还；使用独立状态目录和本地模型调度，依赖公网和本机图形环境。先构建 Web 与 Desktop JavaScript，脚本通过 `scripts/dev-daemon.sh` 启动当前 daemon；截图与日志默认保留在临时目录，可用 `MAGI_CURSOR_LIVE_ARTIFACTS` 指定输出目录 |
| `npm run test:electron-browser-network-policy` | 开发依赖 Electron 的网络边界验收；验证 iframe、fetch、重定向和 localhost 预览来源 |
| `npm run test:browser-core-live -- --session-id <id>` | 已运行 daemon 的真实会话/Tab；`--app-renderer` 使用已启动 Desktop 的可信 Renderer，`--lifecycle-regression` 覆盖生命周期。环境变量和可写操作开关见 scripts/verify-browser-core-live.mjs |
| `npm run test:electron-conversation-dom` | 已打包 Electron 与脚本提供的本地 HTTP Provider；验证 DOM、首轮接纳期间草稿保留、排队与会话归属、停止和继续、终态、重启恢复及时序。应用路径、端口及场景通过 MAGI_ELECTRON_DOM_* 设置，见脚本头部 |
| `npm run test:electron-plugins` | 已打包 Electron、独立状态根与本地 Provider；覆盖插件安装、授权激活、设置、隔离视图与资源同步、工具/命令、升级排空与数据转换、插件中心和停用；MCP 覆盖服务生命周期、完整工具目录、询问/开放模式、外部会话排序与审批、拒绝写入、变更账本、审计与内容留存、实际生成的 stdio 配置握手与文件读写、越界及错误令牌拒绝、HTTP 吊销。通过 MAGI_ELECTRON_PLUGINS_* 设置应用路径与端口 |
| `cargo test -p magi-plugin-system --test catalog_tool` | 用 `scripts/plugin-center.mjs` 发布工具签名目录，再由 daemon 的 `PluginCatalog::verify` 校验；需要 Node.js，缺失时跳过 |
| `npm run test:electron-gpt-web` | 已打包 Electron；复制本机已登录的 `persist:magi-web-model` 分区到一次性状态根，向真实 ChatGPT 发送文字和两张图片，验证收发、暂存清理与停用；`MAGI_GPT_WEB_MODE=saved` 验证已保存模式，默认临时模式。`MAGI_GPT_WEB_FRESH=1` 只验证需要登录；`MAGI_GPT_WEB_QUESTION_FIXTURE=1 MAGI_GPT_WEB_LAYOUT_CHECK=1` 验证受控问答与布局；`MAGI_GPT_WEB_REAL_QUESTION=1` 单独验证真账号原生卡片，不能以受控页面替代。未找到登录分区时真实账号场景跳过 |
| `npm run test:electron-browser-permission-matrix` | 打包 macOS Apple Silicon Electron 的 Browser 权限代表场景；需本机图形环境及空闲 daemon/CDP 端口 |
| `npm run test:electron-right-pane-links` | 已打包 Electron、全新状态根与本地 Provider；从右栏未挂载开始，验证链接点击展开、真实页面加载、同 URL 复用、会话绑定、GPT Web 激活时切换、关闭 Tab 和关闭会话后 Browser 资源结算 |

核心集成按改动触发，不接入日常 npm test。Browser actions 负责页面交互和快照，network policy 负责 Chromium 网络边界；DOM 入口负责消息、工具、审批、取消和重启恢复；Browser live 检查当前 Tab 的物理生命周期，权限矩阵验证真实工具授权，MCP 入口验证 HTTP/stdio 对外协议。

DOM 的确定性 Provider 失败场景使用 400，避免混入 5xx 的退避重试预算；并行可写子代理场景使用已提交的 Git 工作区，沿生产隔离规则创建真实工作副本。

DOM 的 recovery、Git preflight、approval expiry、compaction 场景可单独选择（MAGI_ELECTRON_DOM_*）。压缩场景包含历史预填充和 3 个连续 Turn，至少一次 completed；时序原始记录用于诊断。

发布预检仍执行一次 Rust workspace 全量测试；不再紧接着重复执行已覆盖的本机平台测试。CI 的 Windows/macOS 专项任务继续验证对应操作系统差异。

## 性能测量

只在处理性能问题或明确比较收益时采样，先验证终态、幂等、权限、取消和恢复。后端与 Renderer 使用各自时钟，不拼接为总延迟；辅助模型调用、失败、取消、阻塞和恢复单列。

| 阶段 | 观察范围 |
| --- | --- |
| 接纳 | submit_received → accepted_response_sent |
| 本地准备 | accepted_response_sent → provider_request_started |
| Provider | provider_request_started → provider_first_raw_delta；首个可见文本单列 |
| 后端完成 | accepted_response_sent → canonical_terminal_published |
| Renderer | frontend_event_received → reducer_completed → projection_completed → dom_painted |

每条记录关联 session_id、turn_id、request_id、实际执行 profile、场景和 outcome；事件序号只用服务端事实，缺失阶段保持缺失。比较时记录源码、构建、模型参数、输入和历史形态。P50/P95 另定样本量与口径并保留失败样本，少量行为用例不能作为稳定 P95 的证据。本地 Provider 只证明 Magi 行为，上游可用性与延迟须独立测量。

## 文档与脚本维护

- 架构约束保留在对应设计文档；完成后的阶段计划、一次性进度/验收报告从活动文档移除，历史通过 Git 追溯。
- 尚未交付的产品方案、用户场景和协议规则仍需保留，并标明性质。版本发布说明是历史交付记录，不按日期删除。
- 删除脚本时同步处理 package.json、工作流、文档和调用方；无 package 命令不代表无效，真实集成脚本可能由开发者直接运行。
- 发布工作流仍调用 build-legacy-desktop-bridge.mjs 生成旧客户端升级迁移资产；它有现行发行合同，不能当作无人使用的旧宿主脚本删除。
- 替换测试时保留有效行为覆盖，删除重复的源码匹配和依赖个人环境的旧 fixture；不得通过删除失败断言掩盖产品问题。
