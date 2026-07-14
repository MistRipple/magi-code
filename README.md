# Magi

**本地优先的 AI 编程工作空间，面向长任务、多代理协作与可审计工具执行。**  
**A local-first AI coding workspace for long-running goals, multi-agent collaboration, and auditable tool execution.**

[中文](#中文) | [English](#english) | [架构图 / Architecture](docs/architecture.html) | [Releases](https://github.com/MistRipple/magi-code/releases)

---

## 中文

### 项目简介

Magi 将主对话、目标推进、多个专业子代理、工作区工具、Skills、MCP、知识库和模型配置整合到同一个本地运行时中。项目采用 Rust daemon 作为唯一业务内核，并提供 Web UI 与 Windows、Linux、macOS 桌面客户端。

桌面端启动后会同时启动 Magi 服务。桌面窗口、浏览器、局域网设备和公网隧道共享同一个运行实例；关闭主窗口时默认隐藏到系统托盘，从托盘退出才会停止服务。

### 核心能力

- **多代理协作**：主线可以按角色并行创建多个子代理，分别使用不同模型执行探索、实现、测试和评审任务。
- **目标模式**：使用会话级 Goal 和 TodoLedger 持续推进长任务，保留状态、用量与终止原因。
- **统一工具运行时**：文件、搜索、Shell、进程、变更、知识库、图片生成等能力使用同一套目录、权限和执行记录。
- **模型与角色分离**：主模型连接、辅助模型、图片生成模型和代理角色模型分别配置。
- **图片生成**：支持 CPA/OpenAI 兼容的 `/v1/images/generations` 接口，生成结果写入当前工作区并可直接预览。
- **Skills 与 MCP**：支持安装指令型 Skill、绑定工具以及连接外部 MCP 服务。
- **项目上下文**：本地代码索引、知识库、项目记忆、会话历史和上下文压缩共同提供持续上下文。
- **流式交互**：主对话与子代理均支持流式消息、工具卡片、排队消息和运行中引导。
- **访问控制**：只读、受限和完全访问模式共同约束主代理、子代理与工具副作用。
- **多端访问**：同一 daemon 支持桌面 WebView、本机浏览器、局域网访问与可选公网隧道。

### 运行架构

```text
Windows / Linux / macOS Desktop
Browser / LAN / Public Tunnel
                │
                ▼
        Magi Daemon + HTTP/SSE
                │
      ┌─────────┼─────────┐
      ▼         ▼         ▼
 Conversation  Agent     Tool Runtime
 Runtime       Runtime   Skills / MCP
      │         │         │
      └─────────┼─────────┘
                ▼
 Sessions / Goals / Knowledge / Memory / Snapshots
```

详细模块关系见 [docs/architecture.html](docs/architecture.html)。

### 环境要求

- Rust stable，支持 Rust 2024 edition
- Node.js 22 或更高版本
- npm
- 桌面打包所需的 Tauri 2 平台依赖

### 快速开始

```bash
git clone https://github.com/MistRipple/magi-code.git
cd magi-code
npm --prefix web ci
./scripts/dev-daemon.sh
```

打开：

```text
http://127.0.0.1:38123/web.html
```

开发模式下只需启动 daemon。daemon 会在固定端口自动启动或复用 Vite 热加载服务，API、SSE 与前端资源始终从同一个 `38123` 入口访问。

### 桌面端运行

```bash
npm --prefix web run build
cargo run -p magi-desktop
```

桌面端使用 Tauri 2，支持：

- macOS DMG
- Linux AppImage 与 Deb
- Windows NSIS 安装器

推送 `v*` 标签会触发 GitHub Actions 构建三平台安装包并创建 Release。

### 模型配置

首次运行后，在“设置 -> 模型”中配置：

1. **主模型连接**：主对话和代理协调使用的接口地址与密钥，具体模型按会话选择。
2. **辅助模型**：用于标题生成、知识抽取、项目记忆和上下文压缩。
3. **图片生成模型**：用于 `image_generate`，支持 CPA/OpenAI 兼容图片接口。
4. **角色模型**：为执行、探索、架构、测试和评审等代理角色分别绑定模型。

模型配置保存在本机 Magi 状态目录，不应提交到代码仓库。

### 常用验证命令

```bash
cargo fmt --all -- --check
cargo check -p magi-daemon
cargo test --workspace
npm --prefix web test
npm --prefix web run check
npm --prefix web run build
```

桌面宿主验证：

```bash
cargo test -p magi-desktop --all-targets
```

### 项目结构

```text
apps/
  daemon/             无头开发与服务入口
  desktop/            Tauri 桌面宿主
crates/
  magi-api/           HTTP、SSE 与公开 API
  magi-daemon/        业务运行时组装
  magi-conversation-runtime/
                      主对话、上下文与任务派发
  magi-tool-runtime/  内置工具、权限与工具目录
  magi-orchestrator/  任务编排与代理执行策略
  magi-knowledge-store/
                      本地代码索引与知识检索
  ...                 会话、目标、记忆、用量、快照等模块
web/                  Svelte 5 Web UI
docs/                 架构说明与可视化结构图
scripts/              开发、缓存清理与架构图生成脚本
```

### 工程原则

- daemon 是唯一业务内核，桌面宿主不复制业务逻辑。
- 后端状态和协议是前端展示的权威来源。
- 同一能力只保留一条正式实现路径，不保留双实现或兼容性兜底。
- 工具执行必须经过访问模式、路径边界、权限和治理检查。
- 子代理不能继续创建子代理；只有主线负责代理拓扑。
- 生成文件和工具写入必须限制在当前工作区或用户明确授权的路径内。

### 仓库

- GitHub: [MistRipple/magi-code](https://github.com/MistRipple/magi-code)
- Issues: [问题反馈](https://github.com/MistRipple/magi-code/issues)
- Releases: [版本下载](https://github.com/MistRipple/magi-code/releases)

### 许可证

当前仓库尚未包含开源许可证文件。在许可证明确之前，请勿假定代码可以被复制、修改或再分发。

---

## English

### Overview

Magi combines the main conversation, long-running goals, multiple specialized subagents, workspace tools, Skills, MCP, project knowledge, and model configuration in one local runtime. A Rust daemon is the single business kernel, with both a Web UI and desktop clients for Windows, Linux, and macOS.

Starting the desktop application also starts the Magi service. The desktop window, browsers, LAN devices, and public tunnel share the same runtime. Closing the main window hides it to the system tray by default; quitting from the tray stops the service.

### Highlights

- **Multi-agent collaboration**: the mainline can run multiple role-based subagents in parallel and assign different models to exploration, implementation, testing, and review.
- **Goal mode**: session-scoped Goals and TodoLedger state support long-running work with durable progress, usage accounting, and explicit terminal states.
- **Unified tool runtime**: file operations, search, shell, processes, changes, knowledge, and image generation share one catalog, permission model, and execution record.
- **Separated model responsibilities**: main connection, auxiliary model, image model, and role-specific agent models are configured independently.
- **Image generation**: supports CPA/OpenAI-compatible `/v1/images/generations`, writes results into the active workspace, and previews them directly in the conversation and right pane.
- **Skills and MCP**: install instruction Skills, bind tools, and connect external MCP services.
- **Project context**: local code indexing, knowledge, project memory, session history, and context compaction provide continuous context.
- **Streaming interaction**: mainline and subagent responses support streaming text, tool cards, queued messages, and active-turn guidance.
- **Access control**: read-only, restricted, and full-access profiles consistently constrain the main agent, subagents, and tool side effects.
- **Multiple clients**: one daemon serves the desktop WebView, local browsers, LAN access, and an optional public tunnel.

### Runtime Architecture

```text
Windows / Linux / macOS Desktop
Browser / LAN / Public Tunnel
                │
                ▼
        Magi Daemon + HTTP/SSE
                │
      ┌─────────┼─────────┐
      ▼         ▼         ▼
 Conversation  Agent     Tool Runtime
 Runtime       Runtime   Skills / MCP
      │         │         │
      └─────────┼─────────┘
                ▼
 Sessions / Goals / Knowledge / Memory / Snapshots
```

See [docs/architecture.html](docs/architecture.html) for the generated module graph.

### Requirements

- Stable Rust with Rust 2024 edition support
- Node.js 22 or newer
- npm
- Tauri 2 platform dependencies when building the desktop application

### Quick Start

```bash
git clone https://github.com/MistRipple/magi-code.git
cd magi-code
npm --prefix web ci
./scripts/dev-daemon.sh
```

Open:

```text
http://127.0.0.1:38123/web.html
```

For normal development, start only the daemon. It starts or reuses the fixed-port Vite development server, while the UI, API, and SSE remain available through the single `38123` origin.

### Desktop Development

```bash
npm --prefix web run build
cargo run -p magi-desktop
```

The Tauri 2 desktop host produces:

- macOS DMG
- Linux AppImage and Deb
- Windows NSIS installer

Pushing a `v*` tag triggers GitHub Actions to build all three platforms and publish a GitHub Release.

### Model Configuration

After the first launch, open **Settings -> Models** and configure:

1. **Main model connection**: endpoint and API key for the main conversation and agent coordination; each session selects its concrete model.
2. **Auxiliary model**: title generation, knowledge extraction, project memory, and context compaction.
3. **Image generation model**: the CPA/OpenAI-compatible backend used by `image_generate`.
4. **Role models**: independent model bindings for executor, explorer, architect, tester, reviewer, and other agent roles.

Model settings are stored in the local Magi state directory and should never be committed to the repository.

### Verification

```bash
cargo fmt --all -- --check
cargo check -p magi-daemon
cargo test --workspace
npm --prefix web test
npm --prefix web run check
npm --prefix web run build
```

Desktop host verification:

```bash
cargo test -p magi-desktop --all-targets
```

### Repository Layout

```text
apps/
  daemon/             Headless development and service entry point
  desktop/            Tauri desktop host
crates/
  magi-api/           HTTP, SSE, and public APIs
  magi-daemon/        Runtime assembly
  magi-conversation-runtime/
                      Main conversation, context, and task dispatch
  magi-tool-runtime/  Built-in tools, permissions, and tool catalog
  magi-orchestrator/  Task orchestration and agent execution policy
  magi-knowledge-store/
                      Local code index and knowledge retrieval
  ...                 Sessions, goals, memory, usage, snapshots, and more
web/                  Svelte 5 Web UI
docs/                 Architecture documentation and generated graph
scripts/              Development, cache cleanup, and graph generation
```

### Engineering Principles

- The daemon is the only business kernel; the desktop host does not duplicate business logic.
- Backend state and protocols are authoritative for frontend presentation.
- Each capability has one production path, without parallel implementations or compatibility fallbacks.
- Tool execution must pass access-profile, path-boundary, permission, and governance checks.
- Subagents cannot create more subagents; the mainline owns the agent topology.
- Generated files and tool writes stay inside the active workspace or explicitly authorized paths.

### Repository

- GitHub: [MistRipple/magi-code](https://github.com/MistRipple/magi-code)
- Issues: [Report an issue](https://github.com/MistRipple/magi-code/issues)
- Releases: [Download releases](https://github.com/MistRipple/magi-code/releases)

### License

This repository does not currently include an open-source license. Do not assume permission to copy, modify, or redistribute the code until a license is provided.
