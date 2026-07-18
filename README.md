# Magi

**你的本地 AI 工程团队。**

一个本地优先、可自托管的 AI 工程工作空间：由主线代理统筹目标，多个专业代理按职责协作，在统一的工具、知识和权限边界内持续完成复杂的软件任务。

**Your local AI engineering team.**

A local-first, self-hostable AI engineering workspace where one main agent coordinates the work, specialized agents collaborate by responsibility, and every tool, knowledge source, and permission boundary remains observable and controllable.

![Magi mainline task overview](docs/images/readme/mainline-task-overview.jpg)

> **Turn a single request into a durable, observable, and reviewable engineering workflow.**

[中文](#中文) · [English](#english) · [架构图](docs/architecture.html) · [GitHub](https://github.com/MistRipple/magi-code)

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

Magi 的核心不是增加一个更复杂的聊天窗口，而是把目标、代理、工具、知识、变更和验证组织成一条可以长期运行、持续恢复、全程复核的工程链路。

### 核心能力

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

Magi 支持标准的 OpenAI 兼容接口格式和 Anthropic Messages 接口格式；图片生成使用 OpenAI 兼容的 Images API。

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

### 产品实录

下面的界面截图均来自本地 Chrome 浏览器中的 Magi 真实运行场景，使用 `magi` 工作空间和脱敏演示数据，以完整浏览器窗口采集，当前素材分辨率约为 `1913 × 1212–1263`。主流程截图保留完整工作区、对话区域和输入区；它们展示的是产品实际可操作的工作流，而不是静态概念图。

#### 从目标到结果

主线把目标、执行状态、最终结论和下一次输入放在同一界面中；用户不需要在多个页面之间拼接任务进度。

![Magi 主线任务总览](docs/images/readme/mainline-task-overview.jpg)

#### 从分工到执行

一次任务可以在输入区指定角色和职责，让主线把复杂工作拆成多个相互独立、可验证的工作包。

![Magi 多代理任务编排入口](docs/images/readme/multi-agent-task-entry.jpg)

![Magi 多代理对话与代理结果](docs/images/readme/multi-agent-conversation.jpg)

![Magi 主线执行状态与任务清单](docs/images/readme/mainline-multi-agent.jpg)

#### 从模型到角色

主模型、辅助模型、图片模型和专业代理模型分别管理，模型选择服务于职责，而不是把整个任务锁定在一个引擎上。

![Magi 模型配置](docs/images/readme/model-configuration.jpg)

![Magi 专业代理模型绑定](docs/images/readme/agent-role-model-bindings.jpg)

#### 从工具到知识

工具、MCP、Skills、ADR、FAQ 和工程经验都在工作区内可见，并且只有相关知识才会按需参与任务上下文。

![Magi 工具、MCP 与 Skills 状态](docs/images/readme/tools-mcp-skills.jpg)

![Magi ADR 知识记录](docs/images/readme/knowledge-adr.jpg)

![Magi FAQ 与按需知识说明](docs/images/readme/knowledge-system-complete.jpg)

![Magi FAQ 知识记录](docs/images/readme/knowledge-faq.jpg)

![Magi 工程经验沉淀](docs/images/readme/knowledge-experience.jpg)

#### 从变更到交付

文件归属、增删行数、Diff、工具输出和任务清单保持在同一条可回看的执行记录中，用户可以在确认前复核、批准或还原变更。

![Magi 变更审查](docs/images/readme/changes-review.jpg)

![Magi 文件 Diff 预览](docs/images/readme/file-diff-preview.jpg)

#### 图片与用量

图片模型生成的素材直接写入工作区；统计面板帮助判断模型分工和实际成本。

![Magi 图片生成与预览](docs/images/readme/image-generation.jpg)

![Magi 模型与角色用量统计](docs/images/readme/model-usage-stats.jpg)

![Magi 偏好配置](docs/images/readme/preferences.jpg)

这组截图对应 Magi 的核心产品路径：**提出目标 → 组织分工 → 执行验证 → 复核变更 → 沉淀知识**。

### 为什么选择 Magi

Magi 适合需要长期运行、可复核、可自主管理的软件工程工作流。它把模型能力变成了一套可配置、可观察、可恢复的本地工程系统：

- **模型选择自由**：按主线、辅助、图片和专业角色分别配置模型、供应商与网关。
- **多代理协作有边界**：执行、探索、架构、测试和评审职责清晰，主线统一管理派发、等待和汇总。
- **工程状态完整可见**：Goal、任务清单、代理状态、工具调用、文件变更和验证结果沿同一条链路呈现。
- **知识能够持续积累**：代码索引、ADR、FAQ 和经验记录围绕工作区沉淀，并在需要时参与后续任务。
- **工具治理统一**：文件、Shell、搜索、MCP、Skills 和图片生成遵循统一的工作区、访问模式与权限边界。
- **运行时属于用户**：工作区、会话、模型配置和知识数据保存在本地 Magi 环境中，支持自托管和离线管理。
- **多端共享同一状态**：桌面、浏览器、局域网设备和公网隧道连接同一个 daemon，不需要重复配置或同步任务。
- **适合真实交付**：支持暂停、恢复、停止、失败诊断、变更审查和发布前验证，而不是只返回一段答案。

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

推送与版本号一致的 `v*` 标签后，GitHub Actions 会构建三平台安装包、签名 updater 归档并创建 Release。已安装的桌面端启动后会自动检查 `latest.json`；用户确认后，应用会下载、校验签名、安装新版本并自动重启。更新只替换应用本体，`~/.magi` 中的模型配置、会话、工作区、任务和知识库不会被打包或覆盖。

发布更新需要在 GitHub Repository Secrets 中配置 `TAURI_SIGNING_PRIVATE_KEY`，可选配置 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。私钥只供 GitHub Actions 使用，不应写入仓库或桌面包；更新公钥只保存在 `apps/desktop/tauri.conf.json` 中。

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

Magi supports the standard OpenAI-compatible API format and the Anthropic Messages API format. Image generation uses the OpenAI-compatible Images API.

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

### Product evidence

These are full-window captures from the local Chrome browser using the `magi` workspace and sanitized demonstration data.

![Magi mainline task overview](docs/images/readme/mainline-task-overview.jpg)

![Magi multi-agent conversation](docs/images/readme/multi-agent-conversation.jpg)

![Magi model configuration](docs/images/readme/model-configuration.jpg)

![Magi tools, MCP, and Skills](docs/images/readme/tools-mcp-skills.jpg)

![Magi knowledge overview](docs/images/readme/knowledge-system-complete.jpg)

![Magi change review](docs/images/readme/changes-review.jpg)

![Magi image generation](docs/images/readme/image-generation.jpg)

### Why teams use Magi

Magi is built for software work that must remain understandable and recoverable over time:

- **Bring your own model stack** with independent connections for orchestration, support, image generation, and specialist roles.
- **Coordinate bounded specialists** with explicit responsibilities, controlled fan-out, and one mainline responsible for synthesis.
- **Keep the full execution trail visible**, from the original goal to task progress, tool calls, file changes, validation, and final evidence.
- **Turn project knowledge into a working asset** through code indexing, ADRs, FAQs, and continuously accumulated engineering experience.
- **Apply one governance model everywhere** across files, Shell, search, MCP, Skills, image generation, permissions, and access profiles.
- **Own the runtime and its data** with a local daemon, self-hosted deployment, and user-managed workspace and session state.
- **Continue from any client** because desktop, browser, LAN, and tunnel access share the same authoritative runtime.
- **Support real delivery workflows** with cancellation, recovery, failure diagnostics, change review, and release verification.

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

The Tauri 2 desktop host targets macOS DMG, Linux AppImage/Deb, and Windows NSIS. Pushing a version-matching `v*` tag triggers GitHub Actions to build the installers, signed updater archives, and a Release containing `latest.json`. Installed desktop builds check for updates on startup; after confirmation they download, verify, install, and relaunch the new version. Runtime data under `~/.magi` stays outside the application bundle and is preserved across updates.

Release builds require the `TAURI_SIGNING_PRIVATE_KEY` GitHub Repository Secret and may use `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The private key is used only by GitHub Actions and is never committed or bundled; the public key is stored in `apps/desktop/tauri.conf.json` for client-side verification.

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
