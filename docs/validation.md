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
| `npm run test:electron-browser-actions` | 从当前源码构建 Worker 与 SurfaceManager，在独立临时状态目录启动真实 Electron webview；验证敏感值、快照预算、点击、输入、焦点、遮挡及导航引用失效；需要本机图形环境 |
| `npm run test:electron-browser-network-policy` | 开发依赖 Electron 的网络边界验收；验证 iframe、fetch、重定向和 localhost 预览来源 |
| `npm run test:browser-core-live -- --session-id <id>` | 已运行 daemon 的真实会话/Tab；`--app-renderer` 使用已启动 Desktop 的可信 Renderer，`--lifecycle-regression` 覆盖生命周期。环境变量和可写操作开关见 scripts/verify-browser-core-live.mjs |
| `npm run test:electron-conversation-dom` | 已打包 Electron 与脚本提供的本地 HTTP Provider；验证 DOM、终态、取消、恢复和时序。应用路径、端口及场景通过 MAGI_ELECTRON_DOM_* 设置，见脚本头部 |
| `npm run test:electron-plugins` | 已打包 Electron、独立状态根与脚本提供的本地 Provider；用示例插件走完安装、授权激活（插件页为左右分栏：左侧按「有更新/已安装/可安装」分组的可滚动列表，右侧在同一页面展示所选插件的详情与全部操作，默认选中第一项、不弹窗；含分页、搜索、过滤与「打开设置」快捷入口）、设置表单、主页入口与主内容区页面、隔离视图资源订阅、视图选中引用、模型发起的插件工具调用、命令、右栏视图与添加菜单（插件视图项、管理插件直达）、停用清理、MCP 服务插件壳（未激活拒绝、激活监听、停用释放、重新激活恢复）、外部会话（外部 agent 经 MCP 的读取、越界拒绝、待确认写入：外部会话是对应工作区下的真正会话，在主对话区只读打开，调用在 canonical 时间线里按工具卡片展示，待确认出现在会话自己的授权托盘并批准，窗口不在前台时发系统通知、停在其他标签时点外部会话回到「对话」，关闭「保存调用内容」后读到的内容不落库）、主页卡片、升级候选包预览与确认面板及升级排空、数据格式升级（新版入口在 Worker 内转换已有数据）、插件中心（发布工具签名目录 → daemon 验签 → 安装）。应用路径与端口通过 MAGI_ELECTRON_PLUGINS_* 设置 |
| `cargo test -p magi-plugin-system --test catalog_tool` | 用 `scripts/plugin-center.mjs` 发布工具签名目录，再由 daemon 的 `PluginCatalog::verify` 校验；需要 Node.js，缺失时跳过 |
| `npm run test:electron-gpt-web` | 已打包 Electron；把本机已登录的 `persist:magi-web-model` 分区复制到一次性状态根，验证第一方 GPT Web 插件安装激活、首次使用物化主页、探测、临时对话收发和停用释放，会向真实 ChatGPT 账号发送一条消息。`MAGI_GPT_WEB_FRESH=1` 只验证全新环境到“需要登录”；未找到登录分区时跳过。通过 MAGI_GPT_WEB_* 设置 |
| `npm run test:electron-browser-permission-matrix` | 打包 macOS Apple Silicon Electron 的 Browser 权限代表场景；需本机图形环境及空闲 daemon/CDP 端口 |
| `node scripts/verify-mcp-server.mjs --base <daemon-url> --mcp-bin <magi-mcp-path>` | 在独立状态根运行的 daemon 与已构建 magi-mcp；通过真实 HTTP/stdio 检查令牌、工具、审批与吊销；会注册工作区、安装并激活 MCP 服务插件并修改测试实例设置 |

核心集成按改动触发，不接入日常 npm test。Browser actions 负责页面交互和快照，network policy 负责 Chromium 网络边界；DOM 入口负责消息、工具、审批、取消和重启恢复；Browser live 检查当前 Tab 的物理生命周期，权限矩阵验证真实工具授权，MCP 入口验证 HTTP/stdio 对外协议。

DOM 的 recovery、Git preflight、approval expiry、compaction 场景仍可单独选择（MAGI_ELECTRON_DOM_*）。压缩场景保留历史预填充和 3 个连续 Turn，至少一次 completed；移除只为统计时延而重复执行的 timing 采样模式。时序原始记录保留用于诊断，性能分析原则见[对话性能验证](conversation-performance-validation.md)。

发布预检仍执行一次 Rust workspace 全量测试；不再紧接着重复执行已覆盖的本机平台测试。CI 的 Windows/macOS 专项任务继续验证对应操作系统差异。

## 文档与脚本维护

- 架构约束保留在对应设计文档；完成后的阶段计划、一次性进度/验收报告从活动文档移除，历史通过 Git 追溯。
- 尚未交付的产品方案、用户场景和协议规则仍需保留，并标明性质。版本发布说明是历史交付记录，不按日期删除。
- 删除脚本时同步处理 package.json、工作流、文档和调用方；无 package 命令不代表无效，真实集成脚本可能由开发者直接运行。
- 发布工作流仍调用 build-legacy-desktop-bridge.mjs 生成旧客户端升级迁移资产；它有现行发行合同，不能当作无人使用的旧宿主脚本删除。
- 替换测试时保留有效行为覆盖，删除重复的源码匹配和依赖个人环境的旧 fixture；不得通过删除失败断言掩盖产品问题。
