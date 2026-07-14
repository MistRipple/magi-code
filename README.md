# Magi

**你的本地 AI 工程团队。**

一个本地优先、可自托管的 AI 工程工作空间：由一个主线代理统筹目标，多个专业代理并行工作，在统一的工具、知识和权限边界内持续完成复杂的软件任务。

**Your local AI engineering team.**

A local-first, self-hostable AI engineering workspace where one main agent coordinates the goal, specialized agents work in parallel, and every tool, knowledge source, and permission boundary stays observable and controllable.

[中文](#中文) · [English](#english) · [架构图](docs/architecture.html) · [GitHub](https://github.com/MistRipple/magi-code)

> Magi 借鉴了 Codex 在 Goal、子代理和工具交互上的优秀体验，但产品定位不同：Magi 面向希望自托管、多模型编排、多端访问和可视化工程治理的用户。
>
> Magi takes inspiration from Codex's Goal, subagent, and tool workflows, while serving a different audience: users who want self-hosting, multi-model orchestration, multi-client access, and visible engineering governance.

## 中文

### Magi 解决什么问题

复杂软件任务很少只需要一次问答。它们通常需要理解整个项目、拆解目标、并行调查、修改代码、运行验证、处理失败、持续追踪上下文，最后给出可以复核的结果。

Magi 把这条链路组织成一个可持续运行的工程工作流：

~~~text
提出目标
   ↓
主线代理理解约束并拆解工作
   ↓
多个专业代理使用各自模型并行执行
   ↓
文件、Shell、搜索、知识库、MCP、Skills 等工具统一治理
   ↓
实时流式输出、任务状态、变更和代理结果持续可见
   ↓
主线等待、汇总、验证并继续推进目标
~~~

Magi 的重点不是把聊天窗口做得更复杂，而是让 AI 真正像一支可以观察、配置和管理的工程团队一样工作。

### 核心产品能力

#### 一条主线，多个专业代理

主线代理负责理解用户目标、分配任务、等待结果和最终汇总。专业代理按职责执行独立工作，当前内置角色包括：

- 执行代理：负责代码实现和实际修改。
- 探索代理：负责阅读代码、定位调用链和梳理影响范围。
- 架构代理：负责设计判断、模块边界和技术取舍。
- 测试代理：负责验证、回归检查和测试缺口分析。
- 评审代理：负责从缺陷、风险和可维护性角度复核结果。

每个角色可以绑定独立模型，并可在同一轮任务中创建多个代理实例。子代理只负责自己的工作，不继续创建更深层代理，代理拓扑由主线统一掌控，避免递归扩散和上下文失控。

#### Goal 模式驱动长任务

Goal 不是一次性的计划文本，而是可持续推进的任务状态。它会保存目标、约束、验收标准、任务清单、当前进度、暂停状态和终止原因。

用户可以在任务运行期间继续补充要求，也可以暂停、恢复、编辑或清除目标。主线会在需要时继续同一目标，而不是每轮重新猜测任务背景。

#### 模型按职责分工，而不是只有一个模型下拉框

Magi 支持按职责配置模型：

- 主模型：负责主线对话和任务编排。
- 辅助模型：负责标题、知识抽取、项目记忆和上下文压缩。
- 图片模型：负责图片生成。
- 角色模型：为执行、探索、架构、测试、评审等代理分别绑定模型。

Magi 支持标准的 OpenAI 兼容接口格式和 Anthropic Messages 接口格式。CPA、其他本地网关或远程服务都只是可以接入的服务端实现，不是 Magi 自定义的接口协议，也不存在“CPA 格式接口”。图片生成使用 OpenAI 兼容的 Images API。

#### 统一工具运行时

文件读写、目录操作、补丁、搜索、Shell、进程、变更预览、知识查询、图片生成、Skills 和 MCP 工具通过同一套工具目录与执行策略工作。

工具调用会经过：

1. 当前工作区和会话边界检查。
2. 访问模式检查：只读、受限访问或完全访问。
3. 工具自身的读写与审批策略检查。
4. 执行状态、流式结果和最终摘要写回同一条运行链路。

这使得主线和子代理遵循同一套工具规则，避免出现主线能用、子代理不能用，或前端展示与真实执行状态不一致的问题。

#### 真实上下文，而不是每轮重新开始

Magi 将以下信息组合成当前任务的上下文：

- 当前会话和历史消息。
- 当前工作区代码索引与文件摘要。
- 项目知识库中的 ADR、FAQ 和经验记录。
- 目标、任务清单、代理运行状态和工具执行记录。
- 用户主动引用的文件、文件夹、Skill 和其他上下文。

上下文由后端运行时统一组装，前端只负责展示和交互。这样可以保证桌面端、浏览器、手机端和公网隧道看到的是同一份任务状态。

#### 多端访问，共享一个运行实例

桌面端启动时会同时启动 Magi daemon。桌面窗口、本机浏览器、局域网设备和公网隧道都连接到同一个服务实例：

- 关闭桌面窗口默认隐藏到系统托盘。
- 从系统托盘退出才会停止服务。
- 浏览器或手机端可以继续访问同一个会话状态。
- 服务退出后，所有访问入口同时停止。

支持 Windows、Linux 和 macOS 桌面端，同时保留 Web、局域网和公网隧道访问方式。

#### 可审计的工程界面

Magi 不把运行细节藏在黑盒里。用户可以直接看到：

- 主线和每个子代理的实时输出。
- 代理的运行、等待、完成和失败状态。
- 文件、变更和工具调用卡片。
- Goal 与任务清单的进度。
- 上下文用量和运行诊断。
- 右侧文件、代理和知识内容面板。
- 系统异常告警，而不是把普通保存、切换和发送消息都变成通知噪声。

### 为什么选择 Magi

如果你只是需要一个代码问答窗口，Magi 可能不是必要选择。如果你需要的是一套可以长期运行的软件工程工作台，Magi 的价值在于把以下能力放在同一个可控运行时中：

- 自己选择模型、服务端和供应商，不被单一模型锁定。
- 通过 UI 管理不同角色的模型绑定，而不是只维护一份全局模型配置。
- 让多个代理并行工作，同时保留每个代理的独立结果和运行轨迹。
- 让 Goal、任务、知识、变更和工具调用围绕同一个工作区协同。
- 让桌面、浏览器和移动设备共享同一套本地服务状态。
- 在局域网或公网隧道场景下继续访问正在运行的工程任务。

### Magi 与 Codex Desktop 的定位差异

这不是“谁更强”的简单排名，而是两种产品取向的区别。Codex 是 OpenAI 的一体化编码代理产品；Magi 是可自托管、可接入多种模型服务的工程协作运行时。以下对比基于当前公开能力，具体能力会随产品版本变化。

| 维度 | Magi | Codex Desktop |
| --- | --- | --- |
| 产品定位 | 本地优先的多代理工程工作空间 | OpenAI Codex 一体化编码代理产品 |
| 代理模型配置 | 在 UI 中分别配置主模型、辅助模型、图片模型和角色模型 | 支持模型选择，也支持通过自定义代理配置模型和推理强度 |
| 模型接口 | OpenAI 兼容格式、Anthropic Messages 格式 | 以 Codex/OpenAI 模型体验为核心，也支持配置兼容模型供应商 |
| 服务部署 | 用户自己的 daemon，可运行桌面、浏览器、局域网和公网隧道 | 官方桌面、CLI、IDE 与云端产品体系 |
| 代理组织 | 内置执行、探索、架构、测试、评审角色；同一角色可运行多个实例 | 内置代理并支持自定义代理与并行子代理工作流 |
| 长任务 | Goal、任务清单、暂停/恢复/编辑/清除、代理结果和运行诊断统一展示 | 支持 Goal、子代理和长任务工作流 |
| 项目知识 | 独立代码索引、ADR、FAQ、经验记录和知识面板 | 依托项目、对话上下文、Skills、Memory 和工具体系 |
| 工具治理 | 内置工具、MCP、Skills、访问模式和执行记录集中到一个运行时 | Sandbox、审批、MCP、Skills 和插件体系 |
| 数据与状态 | 工作区、会话、知识和模型配置由用户本地环境管理 | 取决于 Codex 使用方式、账户和连接的服务 |

Magi 的差异化不在于声称 Codex 没有子代理或模型配置，而在于：将多模型角色编排、项目知识、工具治理和多端服务合并为一个用户可管理的本地产品。

相关参考：[Codex Manual](https://developers.openai.com/codex/codex-manual.md)。

### 适用场景

- 大型代码库的架构梳理和模块级审查。
- 需要探索、实现、测试和评审并行推进的重构任务。
- 需要多个模型分别承担不同职责的团队工作流。
- 需要持续数小时甚至更久的目标型开发任务。
- 不希望把项目上下文和运行服务交给单一云端产品的本地开发者。
- 需要通过浏览器、手机或局域网设备查看同一任务进度的工作环境。

### 快速开始

环境要求：Rust stable、Node.js 22 或更高版本、npm，以及桌面端对应的 Tauri 2 平台依赖。

~~~bash
git clone https://github.com/MistRipple/magi-code.git
cd magi-code
npm --prefix web ci
./scripts/dev-daemon.sh
~~~

打开：

~~~text
http://127.0.0.1:38123/web.html
~~~

开发时只启动 daemon。它会自动启动或复用固定端口的 Vite 服务，并从同一个 38123 入口提供前端、API 和 SSE。

### 桌面端

~~~bash
npm --prefix web run build
cargo run -p magi-desktop
~~~

桌面端基于 Tauri 2，目标平台包括：

- macOS DMG
- Linux AppImage 与 Deb
- Windows NSIS 安装器

推送 v* 标签后，GitHub Actions 会构建三平台安装包并创建 Release。

### 配置模型与角色

启动后进入“设置 -> 模型”：

1. 配置主模型连接和默认模型。
2. 配置辅助模型，用于标题、知识抽取、记忆和上下文压缩。
3. 配置图片生成模型，连接 OpenAI 兼容 Images API。
4. 为执行、探索、架构、测试和评审角色绑定独立模型。
5. 按任务需要选择只读、受限访问或完全访问模式。

模型配置保存在本机 Magi 状态目录，不应提交到代码仓库。

### 项目结构

~~~text
apps/
  daemon/                         无头开发与服务入口
  desktop/                        Tauri 桌面宿主
crates/
  magi-api/                       HTTP、SSE 与公开 API
  magi-conversation-runtime/      主对话、上下文与任务派发
  magi-agent-role/                代理角色定义与注册表
  magi-tool-runtime/              内置工具、权限与工具目录
  magi-knowledge-store/           代码索引与项目知识
  magi-context-runtime/           上下文来源选择与组装
  ...                             会话、目标、任务、记忆、用量和快照
web/                              Svelte Web UI
docs/                             架构说明与可视化结构图
scripts/                          开发、缓存清理与架构图生成脚本
~~~

### 验证命令

~~~bash
cargo fmt --all -- --check
cargo check -p magi-daemon
cargo test --workspace
npm --prefix web test
npm --prefix web run check
npm --prefix web run build
~~~

### 工程原则

- daemon 是唯一业务内核，桌面宿主不复制业务逻辑。
- 后端状态和协议是前端展示的权威来源。
- 每项能力只保留一条正式实现路径，不保留双实现、兼容分支或临时兜底。
- 工具执行必须经过工作区、访问模式、路径边界、权限和治理检查。
- 子代理不能继续创建子代理，只有主线负责代理拓扑。
- 模型配置、知识记录和运行数据默认属于本地用户环境。

### 仓库与许可证

- GitHub：[MistRipple/magi-code](https://github.com/MistRipple/magi-code)
- Issues：[问题反馈](https://github.com/MistRipple/magi-code/issues)
- Releases：[版本下载](https://github.com/MistRipple/magi-code/releases)

当前仓库尚未包含开源许可证文件。在许可证明确之前，请勿假定代码可以被复制、修改或再分发。

---

## English

### What Magi is for

Complex software work is rarely a single prompt. It requires understanding a codebase, decomposing an outcome, investigating in parallel, editing files, running checks, recovering from failures, preserving context, and producing results that can be reviewed.

Magi turns that full chain into a durable engineering workflow. The main agent owns the goal and coordination, specialized agents work on bounded responsibilities, and the runtime keeps tools, knowledge, permissions, and results connected to the same workspace.

### Product capabilities

#### One mainline, many specialists

The main agent understands the request, assigns work, waits for results, and produces the final synthesis. Built-in specialist roles include executor, explorer, architect, tester, and reviewer.

Each role can use its own model, and one role can run multiple agent instances in the same task. Subagents do not spawn deeper subagents; the mainline owns the topology so fan-out remains deliberate, visible, and bounded.

#### Goal mode for long-running work

Goal mode is durable task state, not a one-off planning message. It keeps the outcome, constraints, acceptance criteria, task ledger, progress, pause state, and terminal reason together.

Users can steer a running goal, pause it, resume it, edit it, or clear it. The mainline continues the same goal with its existing context instead of reconstructing the task from scratch on every turn.

#### Models assigned by responsibility

Magi separates model responsibilities instead of forcing the whole product through one model selector:

- Main model for the conversation and orchestration.
- Auxiliary model for titles, knowledge extraction, memory, and context compaction.
- Image model for image generation.
- Role models for executor, explorer, architect, tester, reviewer, and other agents.

Magi supports the standard OpenAI-compatible API format and the Anthropic Messages API format. CPA, a local gateway, or a remote service is an upstream implementation that can be connected to Magi; CPA is not a Magi protocol and there is no separate “CPA format API”. Image generation uses the OpenAI-compatible Images API.

#### One governed tool runtime

File operations, patches, search, shell, processes, change previews, knowledge queries, image generation, Skills, and MCP tools share one catalog and execution policy.

Every call is evaluated against workspace and session scope, access profile, tool read/write policy, and execution governance. Streaming results, tool cards, final summaries, and runtime state are written back through the same event path for the mainline and subagents.

#### Context that stays connected

Magi assembles the active task context from the current conversation, workspace code index, project knowledge, goals, task ledger, agent runs, tool records, and user-selected references. The backend owns this assembly so the desktop app, browser, mobile client, and public tunnel see the same state.

#### One service, many clients

The desktop application starts the Magi daemon. The desktop window, local browser, LAN devices, and public tunnel connect to the same runtime. Closing the window hides it in the system tray by default; quitting from the tray stops the service and all access paths.

Magi targets Windows, Linux, and macOS while retaining Web, LAN, and optional public-tunnel access.

#### Visible engineering operations

Magi keeps the important runtime state visible: streaming output from the mainline and each subagent, agent lifecycle, tool cards, file previews, changes, Goal progress, task status, context usage, knowledge access, and runtime diagnostics.

### Why Magi

Magi is designed for users who need a durable engineering workbench, not only a code-answer window:

- Choose models, providers, and gateways by responsibility.
- Configure role-specific engines from the UI.
- Run multiple specialists in parallel while keeping each result inspectable.
- Keep Goals, tasks, knowledge, changes, and tools around one workspace.
- Share the same local runtime across desktop, browser, and mobile access.
- Continue viewing a running engineering task over LAN or a public tunnel.

### Magi compared with Codex Desktop

This is a product-positioning comparison, not a claim that one product wins every workflow. Codex is OpenAI's integrated coding-agent product. Magi is a self-hostable orchestration runtime that can connect to different model services. Capabilities may change as both products evolve.

| Dimension | Magi | Codex Desktop |
| --- | --- | --- |
| Product focus | Local-first multi-agent engineering workspace | Integrated OpenAI Codex coding-agent product |
| Agent model setup | UI-based bindings for main, auxiliary, image, and role models | Model picker plus custom-agent configuration for model and reasoning settings |
| Model protocols | OpenAI-compatible and Anthropic Messages formats | Centered on the Codex/OpenAI model experience, with compatible provider configuration available |
| Deployment | User-owned daemon serving desktop, browser, LAN, and public tunnel clients | Official desktop, CLI, IDE, and cloud product surfaces |
| Agent organization | Built-in executor, explorer, architect, tester, and reviewer roles; multiple instances per role | Built-in agents plus custom agents and parallel subagent workflows |
| Long-running work | Goal, task ledger, pause/resume/edit/clear, agent results, and runtime diagnostics in one UI | Goal, subagent, and long-running task workflows |
| Project knowledge | Dedicated code index, ADR, FAQ, learning records, and knowledge panel | Projects, conversation context, Skills, Memory, and tool ecosystem |
| Tool governance | Built-in tools, MCP, Skills, access profiles, and execution records in one runtime | Sandbox, approvals, MCP, Skills, and plugin ecosystem |
| State ownership | Workspace, session, knowledge, and model state stay in the user's local environment | Depends on the local, cloud, and account surface being used |

Magi's differentiation is not claiming that Codex lacks subagents or model configuration. It is the combination of multi-provider role orchestration, project knowledge, visible tool governance, and a user-managed multi-client daemon.

See the [Codex Manual](https://developers.openai.com/codex/codex-manual.md) for the current public Codex documentation.

### Use cases

- Architecture analysis and module-level review of large codebases.
- Refactors that benefit from parallel exploration, implementation, testing, and review.
- Team workflows where different models serve different responsibilities.
- Development goals that run for hours or longer.
- Local developers who want control over project context and runtime deployment.
- Environments where the same task must be inspected from desktop, browser, phone, or LAN devices.

### Quick start

Requirements: stable Rust, Node.js 22 or newer, npm, and the Tauri 2 platform dependencies needed for desktop builds.

~~~bash
git clone https://github.com/MistRipple/magi-code.git
cd magi-code
npm --prefix web ci
./scripts/dev-daemon.sh
~~~

Open http://127.0.0.1:38123/web.html.

For development, start only the daemon. It starts or reuses the fixed-port Vite server and serves the UI, API, and SSE through the same 38123 origin.

### Desktop build

~~~bash
npm --prefix web run build
cargo run -p magi-desktop
~~~

The Tauri 2 desktop host targets macOS DMG, Linux AppImage/Deb, and Windows NSIS. Pushing a v* tag triggers GitHub Actions to build all three platforms and publish a Release.

### Configure models and roles

After launch, open **Settings -> Models** to configure the main connection, auxiliary model, OpenAI-compatible image model, and independent role bindings. Choose the read-only, restricted, or full-access profile that matches the task.

Model settings are stored in the local Magi state directory and should never be committed to the repository.

### Repository layout

~~~text
apps/daemon/                         Headless service entry point
apps/desktop/                        Tauri desktop host
crates/magi-api/                     HTTP, SSE, and public APIs
crates/magi-conversation-runtime/   Conversation, context, and task dispatch
crates/magi-agent-role/              Agent role definitions and registry
crates/magi-tool-runtime/            Built-in tools, permissions, and catalog
crates/magi-knowledge-store/         Code index and project knowledge
crates/magi-context-runtime/         Context source selection and assembly
crates/...                           Sessions, goals, tasks, memory, usage, snapshots
web/                                  Svelte Web UI
docs/                                 Architecture documentation and graph
scripts/                              Development and graph-generation scripts
~~~

### Verification

~~~bash
cargo fmt --all -- --check
cargo check -p magi-daemon
cargo test --workspace
npm --prefix web test
npm --prefix web run check
npm --prefix web run build
~~~

### Engineering principles

- The daemon is the single business kernel; the desktop host does not duplicate business logic.
- Backend state and protocol are authoritative for frontend presentation.
- Each capability has one production path, without duplicate implementations or compatibility fallbacks.
- Tool execution must pass workspace, access-profile, path-boundary, permission, and governance checks.
- Subagents cannot create more subagents; the mainline owns the agent topology.
- Model settings, knowledge records, and runtime data belong to the local user environment by default.

### Repository and license

- GitHub: [MistRipple/magi-code](https://github.com/MistRipple/magi-code)
- Issues: [Report an issue](https://github.com/MistRipple/magi-code/issues)
- Releases: [Download releases](https://github.com/MistRipple/magi-code/releases)

This repository does not currently include an open-source license. Do not assume permission to copy, modify, or redistribute the code until a license is provided.
