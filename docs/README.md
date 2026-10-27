# 项目架构文档

本目录承载 `Magi` 的架构说明与工程基线。

## 项目定位

- Magi 只提供 Electron 桌面 GUI 与 Web 工作台，不提供 Magi CLI 或独立移动端应用；手机和平板通过同一 Web 工作台的响应式布局访问。
- 外部 Agent Executor 当前只预留通用扩展接口，不提供具体执行器、设置入口或试点交付；现有原生模型、GPT Web 与插件运行能力继续维护。
- Rust 后端已完成重构与协议冻结，当前处于正式运行阶段
- 前端基于现有 `magi` 产品方案迁移完成，持续完善产品体验
- 后端重构历史文档已归档清理，当前以代码和真实运行链路为唯一事实源

## 工程约束

- 协议字段、DTO 结构、事件信封与宿主桥接命令保持冻结与收敛
- 前端以现有产品方案为基线，不发散成新的 UI 产品方向
- UI 层可以继续完善产品表达，但不能反向定义后端协议
- 禁止长期双实现、回退逻辑与补丁式兼容分支
- HTTP 模型连接统一填写 API 基础地址，由显式 `apiProtocol` 生成标准端点；仅提供 OpenAI Responses（新配置默认）、OpenAI Chat Completions 与 Anthropic Messages。已有配置保留所选协议，启动时移除废弃的路径模式字段。GPT Web 与插件引擎继续使用各自的非 HTTP 入口。
- JS 构建要求 Node.js >=24.22.1。Electron 45、Desktop Main 和 Browser Automation Worker 均以 Electron 内置 Node 24 为运行目标；Web 使用 Vite 8/Rolldown 与 Oxc；TypeScript 统一使用 6.x，因为当前 svelte-check 4.x 的正式检查合同仍不支持直接切换 TypeScript 7。
- Rust HTTP 使用 reqwest 0.13 默认的 Rustls、系统证书验证与系统代理。摘要和快照依赖升级保持已有 SHA-256 十六进制标识及 zstd 存储格式兼容，不要求迁移用户数据。

## 运行入口

- `apps/desktop` 是 Windows、Linux、macOS 的 Electron/Chromium 桌面产品入口，负责原生窗口、单实例、布局和进程生命周期。主 Renderer 管理右栏 DOM；当前 Browser Tab 在右栏内容槽中渲染唯一的真实 Electron `<webview>`，Main 只负责 guest WebContents 生命周期与自动化控制，不参与浏览器内容几何映射。
- `magi-daemon` 是唯一业务内核。桌面端通过受控 sidecar 启动它，Magi UI 与内置浏览器共用同一 Chromium Desktop Host；浏览器自动化通过私有 Desktop Control 协议接管当前 Browser WebContents。
- 主窗口关闭后默认隐藏到系统托盘，daemon 继续运行；只有托盘中的“退出 Magi”或操作系统显式退出动作会触发优雅关闭。
- `magi-daemon-app` 承载开发、无头部署及桌面包中的 daemon sidecar 入口；业务实现复用 `magi-daemon`，不复制 Electron 宿主逻辑。
- 正式 Desktop 通过专属 stdin 管道托管 daemon（`MAGI_DAEMON_OWNER_PIPE=1`）：退出先关闭管道，daemon 收到 EOF 后沿原关闭入口结算 Turn、工具和持久化，宿主等待退出，超时才强制终止。开发/无头入口不设置此标记，stdin EOF 不影响其生命周期；系统信号仍走原关闭入口。

开发模式仍使用 daemon 托管入口：

```bash
./scripts/dev-daemon.sh
```

桌面宿主开发运行前先构建前端和 Electron 入口：

```bash
npm --prefix web run build
npm run build
npm run desktop:dev
```

`desktop:dev` 只连接已经由 `scripts/dev-daemon.sh` 启动的 daemon，不会再启动第二个 daemon；正式发行包才由 Electron Main 自己托管 daemon sidecar。

发布统一由 Electron Builder 生成 macOS Apple Silicon/Intel DMG、Linux AppImage/Deb 与 Windows NSIS 安装器，旧手工包装脚本已移除。

Electron 45 桌面宿主要求 macOS 13 及以上；macOS 12 停留在 Electron 43 的历史发行版。Windows 继续提供 x64 安装器，Linux 继续提供 x64 AppImage/Deb。

在线更新由 Electron Main 拥有状态。macOS 未签名发行包使用 ZIP 下载、SHA-512 校验、`ditto` 解包及安装助手替换；下载按连续无数据 60 秒超时，断流最多尝试三次，保留部分 ZIP 并通过 HTTP Range 续传。不支持 Range 时覆盖重下。界面在版本与可执行入口验证通过后才展示可重启，之前进度最多 99%。安装助手启动失败保留当前应用和可重试状态，暂存包丢失时重新下载准备；替换后系统拒绝打开新包则恢复旧包。Windows/Linux 沿用 electron-updater，安装错误须返回发起安装的 IPC 调用。

## 发布流程

- [GitHub 新版本发布流程](./release-process.md)：发布前完整预检、main 与 CI 门禁、Tag 顺序及发布后交付物核对。

## 目标与代理运行设计

- [子代理唤起设计与改造](subagent-delegation-design.md)：默认主线执行、四种委派目的、派发前决策合同、唯一预检和验收边界。
- [运行时改造收敛审计](runtime-consolidation-audit.md)：当前变更范围、移除项、冻结项和继续修改门槛。

- [Magi 产品与 Agent 运行时演进方案](./magi-agent-runtime-evolution-plan.md)：2026-10-16 起分阶段实施，基于当前源码与 Claude Code、Codex CLI、DeepSeek Harness、ZCode 的一手资料，涵盖输入/队列/引导、主子代理工具权限、统一运行时和分阶段迁移。方案保留目标与退出条件；当前保留、移除和未验收范围见收敛审计。

- 简单事走会话流和工具证据，不生成额外抽屉
- 多步骤工作进入目标模式，目标抽屉承载目标内容、状态与操作
- 执行计划来自会话级 `SessionPlan`，通过稳定 ID、revision 与真实执行任务绑定，只在存在计划项时展示
- 代理协作保留内部 root task 作为编排锚点，用户主视图通过代理运行抽屉展示实际代理任务
- 跨轮恢复由 canonical Turn 日志、TaskStore 检查点和变更账本承载；原生审批和提问/答复持久化到原工具 item，重启后先处理原交互再显式 Continue；批准领取须匹配恢复链、参数、目录与权限并先提交 running，重启不自动重放副作用
- 已转交的独立代理从详情页显式继续；daemon 只重开原转交子树，沿原 Task/Thread 接纳新 Turn，保持原 Skill、执行目录与交互来源。主线继续不代替独立代理继续；重试读取原回执，旧终态回调不能释放新执行的资源。

后续设计说明从当前代码和产品形态反向整理。

## 架构与维护文档

- [模型推理能力与选择交互](model-reasoning-capabilities.md)：daemon 能力解析、服务商声明、子代理按次强度与恢复快照、实际传输参数和模型选择器验收。

- [无项目会话与最近对话](personal-session-architecture.md)：个人归属、独立执行目录、项目能力边界、文件生命周期和历史兼容。

- [Magi 插件系统：需求与方案设计](./plugin-system-requirements-design.md)：统一安装、授权、生命周期、版本化资源、插件工具与工作流、会话引擎路由、隔离视图、第一方 GPT Web/MCP 服务插件壳及插件中心已接入；平台验证、取消结算与通用 Browser surface 的限制见 §10.1。作者合同见[插件作者指南](./plugin-authoring.md)。
- [Magi 内置浏览器完整设计](./browser-runtime-design.md)：Browser Authority、Browser Automation Worker、Goal/子代理租约、右侧预览、页面标记与跨平台发布基线。
- [内置工具的失败合同](./builtin-tool-failure-contract.md)：工具失败时必须告诉模型什么、暂态重试的唯一负责层、结构化错误字段，以及文件、shell 与后台进程、web、搜索、Git、update_plan 等内置工具的具体约定。
- [对话展示：详细模式与摘要模式](./conversation-display-modes.md)：两种显示模式共用的事实投影、摘要模式折叠与分组规则、工具卡片状态（含结果未确认）与流式稳定性约束。
- [Magi 上下文压力与压缩统一架构](./context-pressure-compaction-architecture.md)：token 语义、压力快照、连续压缩、检查点、超限恢复、模型隔离与测试门禁。
- [知识图谱设计与维护](./knowledge-graph-design.md)：图谱事实源、候选审阅、检索预算、索引维护与定向验证。
- [用户自定义子代理角色](./custom-worker-role-design.md)：角色定义、持久化、注册表热更新、导入导出与调度合同。
- [Magi GPT Web 最终开发文档](./web-model-browser-development.md)：**当前唯一开发基线**；单宿主、单槽位、临时 / 已保存对话、单向 Web → 本地、工具能力通过 Magi MCP 服务接入、状态所有权和验收口径。
- [Magi MCP 服务设计](./magi-mcp-server-design.md)：Magi 对外提供的标准 MCP 服务端，任何 MCP 客户端可在已注册工作区使用文件、搜索、只读 git、变更账本等工具；令牌与授权方式、审批、路径限制、审计，本机 stdio / 回环 HTTP 与 Cloudflare Quick Tunnel 网络模式（**均已实现**）；动态网关目录含内置工具、下游 MCP 与 Skill handler；§12 记录 GPT Web 槽位端点的最终接线（无令牌、槽位表派生身份与归属、OpenAI Tunnel 通道、代码地图）。

- [Turn、事件事实与对话执行架构](./conversation-response-core-architecture-redesign.md)：统一接纳、任务调度、终态、恢复和权限边界。
- [持久日志的追加写入与压缩](./durable-log-compaction-design.md)：用量账本分段、canonical 事件检查点和崩溃恢复。
- [会话 Git 工作流](./session-git-workflow.md)与[会话隔离工作副本](./session-isolation.md)：工作目录、执行租约、子代理产出与合并。
- [Rust crate 依赖图](./architecture.html)：由 Cargo workspace 生成；维护命令见[脚本与验证入口](./validation.md)。
- [脚本与验证入口](./validation.md)：构建、定向回归、真实集成验收、性能测量和文档维护规则。

## 用户场景与产品要求

- [主对话图片理解补偿能力](./main-chat-image-understanding-user-scenarios.md)：描述主模型不支持图片时的用户问题、期望行为、异常边界和产品验收标准，不限定内部实现方案。
