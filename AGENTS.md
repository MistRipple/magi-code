# Magi Agent Guide

本文件只写跨目录、且无法从代码直接推断的规则。当轮用户明确要求优先；更近目录的 `AGENTS.md` 只补充该区域的差异。项目指令以本文件为唯一入口，其他 agent 配置只导入，不复制。

## 项目定位

- Magi 的业务内核是 `magi-daemon`。`apps/daemon` 的 `magi-daemon-app` 是开发和无头入口；Electron 正式发行版由 Main 通过受控 sidecar 托管 daemon。
- UI、Worker 和宿主不复制 daemon 的会话、任务、权限或持久化事实；UI 只消费和展示服务端事实，不反向发明业务协议，也不绕开授权入口。
- 产品范围：只提供 Electron 桌面 GUI 与 Web 工作台（手机和平板走同一 Web 工作台的响应式布局），没有 Magi CLI 或独立移动端；外部 Agent Executor 只预留通用扩展接口；插件由 daemon 托管的 Node 扩展宿主承载，QuickJS 与 SDK 1 已删除。不要按这些已排除的方向新增实现。
- 协议字段、DTO 结构、事件信封与宿主桥接命令处于冻结与收敛状态，只做对应协议兼容规则内的增量修改。
- 工程约束、运行入口和整体架构见 [docs/README.md](docs/README.md)。

## 通用工作规则

- 默认使用中文沟通和说明；代码风格遵循所在模块的现有实现、格式化器与 lint。
- 修改前先看 `git status`、相关源码、调用方和架构文档。工作区可能有与任务无关的已有改动：保留它们，不恢复、不覆盖、不清理。
- 新增或修改状态、持久化、权限、行为前，先在源码里找到现有拥有者，沿它的入口做最小完整修改。找不到唯一拥有者时先向用户确认，不新建第二条写入路径。
- 外部 DTO、事件、生成协议和宿主命令遵循各自 schema 与兼容策略；改动要追踪全部实际消费者。
- 方案、计划、进度文档可能描述尚未完成的目标。实施前核对当前源码和真实运行链路，不把计划状态当成已交付事实。

## 兼容与兜底

禁止长期双实现、影子实现、回退逻辑、补丁式兼容分支和仅为旧实现服务的兜底；替换实现时在同一轮清退旧代码、无效分支和冗余配置。

确需短期过渡实现（例如分步迁移）时，先向用户说明具体失败模式、触发概率、增加的复杂度和清退时点，得到同意后再做，并在同一任务内记录清退条件；不要静默扩大范围，也不要为推测性的竞态或故障预先加兜底。已发布的持久化数据与 wire 协议的兼容，按对应 schema 与迁移文档执行。

## 功能开发

- 先明确用户任务、完整操作流程、状态归属、失败恢复和验收结果，再实现公共能力；不要用单样本补丁或临时兼容代替正式功能。
- 插件翻译功能遵循[产品合同](docs/plugin-system-requirements-design.md#60-翻译功能的产品合同2026-11-08)及[每轮推进与完成门槛](docs/plugin-system-requirements-design.md#每轮推进与完成门槛2026-11-09)，以真实业务完成验收。
- 涉及界面设计、文案、布局或交互的新增、优化与修复，先读并遵循 [UX/UI 设计与开发规范](docs/ux-ui-guidelines.md)，沿共享组件实施并完成对应验收；规范要求不等于已经实现。

## 生成物

下列文件由 schema 或脚本生成，不手改；改源头后重新生成，并检查全部消费者：

| 生成物 | 源头 | 命令 |
| --- | --- | --- |
| App Server 的 Rust/TypeScript 类型 | `contracts/app-server/app-server.schema.json` | `npm run protocol:generate` |
| Browser 工具目录 | `contracts/desktop-browser/browser-tool.schema.json` | `npm run browser-tool-catalog:generate` |
| Browser 网络边界 | `contracts/desktop-browser/network-policy.json` | `npm run browser-network-policy:generate` |
| `docs/architecture.html` | Cargo metadata | `npm run docs:architecture` |

各生成流程的细节见 [`contracts/AGENTS.md`](contracts/AGENTS.md)。

## 本地环境须知

- Web 与 daemon 的日常开发和真实浏览器验收使用 `./scripts/dev-daemon.sh`，入口为 `http://127.0.0.1:38123/web.html`；代理、端口和静态模式见 [`web/AGENTS.md`](web/AGENTS.md)。
- 该脚本在 `target/` 超过默认 `8GiB` 时会执行 `cargo clean`，会丢掉本地构建缓存。需要保留时，先检查缓存大小，再通过 `MAGI_TARGET_PRUNE_GIB` 调整阈值。

## 验证

- 只运行与改动相关的检查和回归；不因本指南默认运行完整测试套件。行为改动要覆盖相关回归路径。
- 静态检查、源码断言和模拟宿主不能代替真实 Chromium、Electron 或 Provider 验收；涉及真实行为时按局部规则做运行验收。
- 如实报告：未运行的验证、已有失败、无法验证的部分都要明确说明，不宣称未实际运行的检查通过。

| 改动面 | 基础检查 |
| --- | --- |
| `web/` | `npm --prefix web run check` |
| `apps/desktop/` | `npm run check --workspace @magi/desktop` |
| `browser-automation-worker/` | `npm run check --workspace @magi/browser-automation-worker` |
| `crates/` | `cargo check -p <Cargo.toml 中的 package name>`；daemon 使用 `cargo check -p magi-daemon` |
| App Server schema/生成物 | `npm run protocol:check` |
| Browser 工具 schema/catalog/生成物 | `npm run browser-tool-catalog:check` |
| Browser 工具执行语义 | `npm run test:browser-core`；涉及真实 Electron/Chromium 行为时按局部规则做运行验收 |
| 插件包、生命周期、扩展宿主 | `cargo test -p magi-plugin-system -p magi-extension-host --all-targets --locked`；官方插件中心改动加 `npm run plugin-center:check`；真实业务验收按[每轮推进与完成门槛](docs/plugin-system-requirements-design.md#每轮推进与完成门槛2026-11-09) |
| 内置代理提示词、工具定义 | `cargo test -p magi-agent-role`、`cargo test -p magi-conversation-runtime --test prompt_layout_snapshots` 及受影响的提示词断言测试；见下方“提示词与工具定义” |
| 发布或创建/移动版本 Tag | 严格遵循 [发布流程](docs/release-process.md) |

更多脚本与验证入口见 [docs/validation.md](docs/validation.md)。发布预检（`npm run release:preflight`）会执行 `clippy --workspace -D warnings` 和 Rust 全量测试；改动 Rust 后，至少让自己改动的文件不引入新的 clippy 告警。

## 提示词与工具定义

- 角色提示词、能力提示词、运行时注入规则和工具描述分别只有一个事实源：委派规则在 `AGENT_DELEGATION_GUIDANCE` 与 `agent_spawn` 等工具定义，角色文件不重复，也不授予或禁止委派；设计合同见 [子代理唤起设计](docs/subagent-delegation-design.md)。
- 渲染后的完整前缀会被模型缓存，工具名、描述和 schema 的任何改动都会影响复用，见 [提示词缓存优化](docs/prompt-cache-optimization.md)。改动前确认确有行为收益，不为命中率填充无用文字。
- 角色提示和能力不授予权限，也不因角色名形成硬只读边界；权限以运行时 TaskPolicy 为准（[自定义 Worker 角色设计](docs/custom-worker-role-design.md)）。
- 改动提示词措辞时同步更新断言它的测试和上述设计文档，不要只改其中一处。

## 按改动位置读取局部规则

开始修改下列区域前，先读该目录的 `AGENTS.md`：

| 区域 | 局部规则 | 主要职责 |
| --- | --- | --- |
| `web/` | [`web/AGENTS.md`](web/AGENTS.md) | Svelte 工作台、daemon 托管开发与 UI 状态投影 |
| `apps/desktop/` | [`apps/desktop/AGENTS.md`](apps/desktop/AGENTS.md) | Electron Main、可信 App Renderer、窗口与物理 Browser guest |
| `crates/magi-browser-authority/` | [`crates/magi-browser-authority/AGENTS.md`](crates/magi-browser-authority/AGENTS.md) | 逻辑 Browser Tab、Surface 权威状态、租约和失效边界 |
| `browser-automation-worker/` | [`browser-automation-worker/AGENTS.md`](browser-automation-worker/AGENTS.md) | Browser Automation Worker 与页面自动化 |
| `contracts/` | [`contracts/AGENTS.md`](contracts/AGENTS.md) | App Server、Desktop Browser 等 wire schema 与生成类型 |

只有子目录有稳定、独有且易误改的约定时，才新增局部 `AGENTS.md`；局部文件只写与本文件的差异，并链接事实文档。

## 权威文档

- [Browser 显示、状态所有权、安全和恢复设计](docs/browser-runtime-design.md)
- [Turn、事件事实与对话执行架构](docs/conversation-response-core-architecture-redesign.md)
- [上下文压力与压缩架构](docs/context-pressure-compaction-architecture.md)
- [GitHub 新版本发布流程](docs/release-process.md)

## agent 配置互操作

`AGENTS.md` 是项目指令的唯一事实来源。Gemini CLI 入口 `GEMINI.md` 只导入本文件；若添加项目级 `CLAUDE.md`，必须使用 `@AGENTS.md` 导入。局部 agent 配置不得复制本指南或定义冲突规则。
