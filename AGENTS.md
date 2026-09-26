# Magi Agent Guide

本文件定义 Magi 全仓共同规则；当轮用户明确要求优先，更近目录的 `AGENTS.md` 只补充该区域的规则。仓库规则以本文件为唯一入口，其他 agent 配置只导入，不复制维护第二套内容。

## 全仓约定

- 默认使用中文沟通和说明；代码风格遵循所在模块的现有实现、格式化器与 lint。
- 修改前检查 `git status`、相关源码、调用方和架构文档。保留所有与任务无关的已有改动，不恢复、覆盖或清理它们。
- 先确定行为、状态、持久化和权限分别由谁拥有，再沿现有入口做最小完整修改。避免重复事实源、第二条写入路径、影子实现和仅为旧实现服务的兜底。
- Magi 的业务内核是 `magi-daemon`。`apps/daemon` 中的 `magi-daemon-app` 是开发和无头入口；Electron 正式发行版由 Main 通过受控 sidecar 托管 daemon。UI、Worker 和宿主不得复制 daemon 的会话、任务、权限或持久化事实。
- 外部 DTO、事件、生成协议和宿主命令遵循其 schema 与现有兼容策略。UI 只能消费和展示服务端事实，不能反向发明业务协议或绕开授权入口。
- 架构文档描述设计和当前约束；方案、计划及进度文档可能描述尚未完成的目标。实施前核对当前源码和真实运行链路，不把计划状态当成已交付事实。
- 只运行与改动相关的检查和回归；不因全仓指南默认运行完整测试套件。未运行的验证及已有失败要如实说明。
- 根指南只放跨目录规则。只有当子目录有稳定、独有且易误改的约定时才增加局部 `AGENTS.md`；局部文件只写差异并链接事实文档。

## 按改动位置读取规则

开始修改下列区域时，先读该目录的局部 `AGENTS.md`；它会指向权威设计文档和对应验证方式：

| 区域 | 局部规则 | 主要职责 |
| --- | --- | --- |
| `web/` | [`web/AGENTS.md`](web/AGENTS.md) | Svelte 工作台、daemon 托管开发与 UI 状态投影 |
| `apps/desktop/` | [`apps/desktop/AGENTS.md`](apps/desktop/AGENTS.md) | Electron Main、可信 App Renderer、窗口与物理 Browser guest |
| `crates/magi-browser-authority/` | [`crates/magi-browser-authority/AGENTS.md`](crates/magi-browser-authority/AGENTS.md) | 逻辑 Browser Tab、Surface 权威状态、租约和失效边界 |
| `browser-automation-worker/` | [`browser-automation-worker/AGENTS.md`](browser-automation-worker/AGENTS.md) | Browser Automation Worker 与页面自动化 |
| `contracts/` | [`contracts/AGENTS.md`](contracts/AGENTS.md) | App Server、Desktop Browser 等 wire schema 与生成类型 |

## 权威文档入口

- [工程约束、运行入口和整体架构](docs/README.md)
- [Browser 显示、状态所有权、安全和恢复设计](docs/browser-runtime-design.md)
- [Turn、事件事实与对话执行架构](docs/conversation-response-core-architecture-redesign.md)
- [上下文压力与压缩架构](docs/context-pressure-compaction-architecture.md)
- [GitHub 新版本发布流程](docs/release-process.md)

日常 Web/daemon 联调使用 `./scripts/dev-daemon.sh`，真实浏览器入口为 `http://127.0.0.1:38123/web.html`；具体代理、端口和静态模式见 [`web/AGENTS.md`](web/AGENTS.md)。该启动脚本在 `target/` 超过默认 `8GiB` 阈值时会执行 `cargo clean`；需要时先检查本地构建缓存，并通过 `MAGI_TARGET_PRUNE_GIB` 调整阈值。

## 验证选择

按改动面运行最小充分验证；行为改动还应覆盖相关回归路径。详细命令由局部规则和对应 workspace/package 定义。

| 改动面 | 基础检查 |
| --- | --- |
| `web/` | `npm --prefix web run check` |
| `apps/desktop/` | `npm run check --workspace @magi/desktop` |
| `browser-automation-worker/` | `npm run check --workspace @magi/browser-automation-worker` |
| `crates/` | `cargo check -p <Cargo.toml 中的 package name>`；daemon 使用 `cargo check -p magi-daemon` |
| App Server schema/生成物 | `npm run protocol:check` |
| Browser 工具 schema/catalog/生成物 | `npm run browser-tool-catalog:check` |
| Browser 工具执行语义 | `npm run test:browser-core`；涉及真实 Electron/Chromium 行为时按局部规则做运行验收 |
| 发布或创建/移动版本 Tag | 严格遵循 [发布流程](docs/release-process.md) |

## agent 配置互操作

`AGENTS.md` 是项目指令的唯一事实来源。Gemini CLI 入口只导入对应 `AGENTS.md`；未来若添加项目级 `CLAUDE.md`，必须使用 `@AGENTS.md` 导入根规则。局部 agent 配置不得复制本指南或定义冲突规则。
