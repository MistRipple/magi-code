# Magi

**你的本地 AI 工程团队。**

Magi 是一个本地优先、可自托管的 AI 工程工作空间。它把目标、分工、上下文、工具、变更、内置浏览器和验证放进同一条可观察的工作流，让复杂软件任务不只是“问一句、答一段”，而是真正可以持续推进、随时接手、逐项复核和交付。

![Magi 主线任务总览](docs/images/readme/mainline-task-overview.jpg)

> **把一次请求变成一条可持续、可观察、可复核的工程工作流。**

[English](README.en.md) · [架构图](docs/architecture.html) · [许可证](LICENSE) · [GitHub](https://github.com/MistRipple/magi-code)

## 中文

### 复杂工程，不该靠重复提问来推进

一段代码改动背后通常是一整条决策链：目标是什么、从哪里开始、谁去调查、用什么模型、改哪些文件、怎样验证、失败后从哪里恢复。Magi 把这条链组织成一个长期运行的本地工程团队：

- **主线优先与专业分工**：主线代理负责理解目标、推进实现；需要时派发执行、探索、架构、测试与评审专业代理，每个角色可使用独立模型。
- **内置 Chromium 浏览器与多功能右栏**：直接嵌入真实 Electron `<webview>`，支持 Agent 驱动的网页自动化操作、DOM 解析、页面标记与实时审查。
- **开放插件体系与标准 MCP 服务**：支持一键安装扩展插件、沙箱工作流；不仅能消费外部 MCP，还对外提供标准 MCP 服务端。
- **全生命周期可控**：支持输入排队、动态引导（Steering）与随时“停止并改做”；上下文压力由单一权威自动连续压缩，拒绝历史断层。
- **严格的安全与治理**：文件、Shell、进程、Git 工作树、网络、MCP 和 Skills 统一走隔离沙箱与权限规则；拒绝未经批准的破坏性副作用。

Magi 的核心不是增加一个更复杂的聊天窗口，而是让每一次请求都进入一条可以长期运行、失败恢复、全程复核的工程流水线。

---

### 从请求到交付

~~~text
提出目标 / 任务输入
   ↓
会话协调器接纳、排队并支持动态引导 (Steering)
   ↓
主线代理理解约束并推进工作（默认主线直接完成）
   ↓
按需通过严格决策合同派发专业代理（并行实现 / 独立复核 / 上下文隔离）
   ↓
文件、Shell、内置 Chromium、搜索、知识库、MCP 与插件沙箱受控执行
   ↓
上下文压力权威实时计算，触发连续历史无损前缀压缩
   ↓
实时流式输出、任务状态、代码 Diff 与交互审批持续可见
   ↓
主线汇总、验证并完成目标交付
~~~

---

### 核心能力

#### 1. 主线优先，结构化专业代理

主线代理像工程负责人一样理解目标、推进任务、汇总成果。在面临复杂隔离工作时，可按职责调度专业代理：

- **执行代理**：负责代码实现与工程落地。
- **探索代理**：负责跨目录代码阅读、调用链分析与影响面梳理。
- **架构代理**：负责设计方案取舍、系统边界与技术决策。
- **测试代理**：负责测试用例设计、自动化验证与回归缺口分析。
- **评审代理**：负责从安全性、规范与可维护性角度进行独立复核。

**严格的委派合同与拓扑约束**：
- **默认主线直接执行**：定位函数、单文件修改、测试运行等普通任务一律由主线直接完成，拒绝无收益的代理滥用。
- **决策合同约束**：唤起代理必须提供严格的 `delegation` 决策凭证（仅限 `user_requested`、`parallel_work`、`context_isolation`、`independent_review` 四种目的；并行任务必须显式声明父任务同时推进的工作）。
- **受控嵌套与资源结算**：支持最多两层代理（父 $\to$ 子 $\to$ 孙，默认禁止深层派发）；任务取消执行确定性的底层资源结算与产物收集，杜绝进程泄漏与副作用遗漏。

#### 2. 内置 Chromium 浏览器与多功能右栏

Magi 将通用多功能面板与真实 Chromium 浏览器原生融合，彻底摒弃传统截图流或虚假坐标映射：

- **通用内容容器 (`RightPane`)**：右栏统一承载源码查看、Diff 差异比对、终端（xterm.js）、Agent 详情以及浏览器视图。
- **真实 Chromium `<webview>`**：直接将 Electron 原生 `<webview>` 嵌入 Svelte 内容槽，保证用户手动交互与 Agent CDP 自动化作用于同一物理页面。
- **逻辑权威与自动化 Worker**：`magi-browser-authority` 掌握 Tab、Surface 逻辑租约；独立 Node.js 自动化 Worker 提供精确的 DOM 元素解析、页面高亮标记、截图与自动化动作。

#### 3. 开放插件系统与官方插件中心

用户无需修改 Magi 源码或重新编译，即可为 Magi 扩充工具、界面和工作流：

- **统一生命周期**：由 `PluginManager` 提供安装、校验、版本化资源、权限授权、激活与安全停用支持。
- **独立运行沙箱**：插件在 QuickJS Worker 隔离环境中运行，保障宿主稳定性。
- **深度界面融合**：插件可贡献设置页、主面板入口，或在右栏以隔离 iframe 形式无缝渲染自定义数据面板与交互界面。
- **官方插件中心**：内置官方插件中心（`plugin-center`），一键发现并安装扩展；首发内置第一方 GPT Web 插件壳。

#### 4. 双向 MCP 生态（客户端 + 标准服务端）

Magi 在 MCP（Model Context Protocol）协议上提供完整的双向互通能力：

- **MCP 客户端**：无缝连接本地与远程 MCP 服务，将第三方工具与数据源融入上下文。
- **Magi MCP 服务端**：系统内置标准 MCP 服务端（`magi-mcp-server`），支持 **stdio**、**本地 HTTP 回环**以及 **Cloudflare Quick Tunnel** 远程隧道模式。外部客户端可安全接入 Magi 当前工作区的文件、代码搜索、Git 状态与变更账本。

#### 5. 上下文压力权威与连续无损压缩

告别生硬的“头尾保留拼接”或粗暴截断，Magi 实现了业界严谨的上下文统一架构：

- **单一权威计量**：`magi-usage-authority` 统一负责 Token 计量，物理分离“当前窗口预测”与“累计计费”。
- **连续前缀摘要**：在接近模型窗口阈值前主动压缩，选择完整的时间连续历史前缀生成摘要。
- **结构安全保护**：**绝对不可拆分工具调用与对应的工具结果**；大型工具输出预先确定性裁剪，超限自动触发有界重试恢复。

#### 6. 输入队列、动态引导与安全停止

- **单写者保证**：每个 Session 独占一个 `SessionTurnCoordinator`，确保状态机流转严格有序。
- **FIFO 输入排队**：在任务执行期间发送的新消息会自动可靠入队，无需等待整轮结束。
- **动态引导 (Steering)**：在任务运行途中注入纠偏指示，模型能在下一个执行步实时感知并调整策略。
- **停止并改做 (Stop & Redirect)**：支持瞬时中断当前执行流，并立即转入全新的目标任务。

#### 7. Goal 模式与工程级持久化恢复

- **Goal 持续推进**：目标、约束、验收标准、任务清单与进度全程持久化；支持无预算持续执行、暂停、编辑与恢复。
- **崩溃安全与防重放**：交互审批（Tool Approvals）与用户提问（User Questions）原生绑定在工具 Item 上；系统重启后先处理待办交互再显式 Continue，绝不擅自在重启后重复触发外部副作用。

#### 8. 模型按职责分工，自由配置

支持标准的 OpenAI 兼容格式与 Anthropic Messages 协议：

- **主模型**：主线对话、任务拆解与全局编排。
- **辅助模型**：会话标题、知识抽取、项目记忆与上下文连续压缩。
- **图片模型**：集成兼容 Images API，生成素材直接落地工作区。
- **角色模型**：为执行、探索、架构、测试、评审等角色绑定最适合的模型。

#### 9. 统一安全与治理工具运行时

文件读写、补丁应用、代码搜索、Shell 命令、长期进程、Git 状态、知识检索与图片生成，全部纳入同一运行时管辖：
- 支持工作区边界约束与目录争用检测。
- 细粒度权限控制（只读、受限访问、完全访问）与破坏性操作原生审批流。
- 变更账本（Change Ledger）记录每一处增删改，提交前随时复核、还原或分支合并。

#### 10. 一个实例，多端共享，深度美化

- **单实例运行**：桌面端启动时托管启动 `magi-daemon`，关闭窗口默认最小化至系统托盘，后台持续推进长任务。
- **全平台支持**：支持 Windows、Linux 与 macOS 原生桌面端，同时提供局域网与移动端响应式 Web 访问。
- **统一外观系统**：内置跟随系统、浅色、深色、深林、星夜雪山、青岚绘境、量子矩阵、海岸晨光和赤砂远境等主题，支持壁纸、通透材质与主题导入导出。

---

### 产品实录

这些截图全部来自本地 daemon 托管入口中的 Magi 真实运行场景，使用 `magi` 和 `magi-rust-rewrite` 工作空间；主线、模型与外观截图使用脱敏演示数据，变更审查展示拍摄时工作区中的真实未提交改动。它们不是概念图，也不是组件拼贴，而是统一使用 `1913 × 1263` 画布的完整应用窗口，展示产品实际可操作的工作流。

#### 从目标到结果

主线把目标、执行状态、最终结论和下一次输入放在同一界面中；用户不需要在多个页面之间拼接任务进度。

![星夜雪山皮肤下的 Magi 主线任务总览](docs/images/readme/mainline-task-overview.jpg)

#### 从分工到执行

一次任务可以在输入区指定角色和职责，让主线把复杂工作拆成多个相互独立、可验证的工作包。

![Magi 多代理任务编排入口](docs/images/readme/multi-agent-task-entry.jpg)

![Magi 多代理对话与代理结果](docs/images/readme/multi-agent-conversation.jpg)

![Magi 主线执行状态与任务清单](docs/images/readme/mainline-multi-agent.jpg)

#### 从模型到角色

主模型、辅助模型、图片模型和专业代理模型分别管理，模型选择服务于职责，而不是把整个任务锁定在一个引擎上。

![星夜雪山皮肤下的 Magi 模型与角色配置](docs/images/readme/model-configuration.jpg)

![Magi 专业代理模型绑定](docs/images/readme/agent-role-model-bindings.jpg)

#### 主题与皮肤

主题库把配色、壁纸和工作台材质统一管理。用户可以直接预览内置皮肤，在跟随系统、浅色、深色、通透和沉浸式工作台之间切换，也可以基于内置主题创建自己的主题。

![Magi 主题库与皮肤预览](docs/images/readme/appearance-themes.jpg)

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

![Magi 偏好与安全防护](docs/images/readme/preferences.jpg)

这组截图对应 Magi 的核心产品路径：**提出目标 → 组织分工 → 执行验证 → 复核变更 → 沉淀知识**。

---

### 为什么选 Magi

Magi 的目标很直接：把模型能力变成一套能够长期协作、可恢复、可复核的本地工程系统，而不是一个只会生成答案的聊天窗口。

- **模型选择自由**：按主线、辅助、图片和专业角色分别配置模型、供应商与网关，不必被一个引擎限制。
- **多代理协作有边界**：坚持主线优先，具备严格的决策合同与两层拓扑限制，杜绝失控调用。
- **工程状态完整可见**：Goal、任务清单、代理状态、工具调用、文件变更和验证结果沿同一条链路呈现。
- **知识能够持续积累**：代码索引、ADR、FAQ 和经验记录围绕工作区沉淀，并在需要时参与后续任务。
- **工具与浏览器治理统一**：文件、Shell、搜索、内置 Chromium 网页自动化、MCP、Skills 遵循统一工作区与权限边界。
- **双向扩展与插件生态**：支持独立沙箱插件，同时开放标准 MCP 服务端，无缝接轨现代 AI 工具链。
- **运行时属于用户**：工作区、会话、模型配置和知识数据保存在本地 Magi 环境中，支持自托管和离线管理。
- **多端共享同一状态**：桌面、浏览器、局域网设备和公网隧道连接同一个 daemon，不需要重复配置或同步任务。
- **适合真实工程交付**：支持暂停、恢复、停止、失败诊断、变更审查和发布前验证，而不是只返回一段答案。

---

### 快速开始

#### 环境要求
- **Rust**：由 [`rust-toolchain.toml`](rust-toolchain.toml) 固定版本（支持 Rust 2024 edition）。
- **Node.js**：24.22.1 或更高版本，以及 npm。
- **系统打包依赖**：构建 Electron 桌面安装包所需的操作系统对应构建工具链。

#### 1. 克隆与开发启动

~~~bash
git clone https://github.com/MistRipple/magi-code.git
cd magi-code
npm ci
./scripts/dev-daemon.sh
~~~

启动完成后，在浏览器打开：

~~~text
http://127.0.0.1:38123/web.html
~~~

`dev-daemon.sh` 会自动启动或复用 Vite 模块热加载服务，并统一从 `38123` 端口提供前端界面、API、SSE 和 WebSocket App Server。

#### 2. 构建与运行桌面端

~~~bash
# 构建前端工作台与桌面依赖
npm --prefix web run build
npm run build

# 启动桌面开发版（会自动连接已经启动的 daemon）
npm run desktop:dev

# 或打包本地绿色版目录
npm run desktop:package -- --dir
~~~

桌面发行版支持以下目标平台：
- **macOS**：DMG（Apple Silicon 与 Intel 架构）
- **Linux**：AppImage 与 Deb 安装包
- **Windows**：NSIS 可执行安装器

---

### 项目代码全景

Magi 采用微内核架构，由多个功能高度收敛的 Rust Crate 与前端/Worker 工作区组成：

~~~text
apps/
  daemon/                         magi-daemon-app 无头开发与服务独立可执行入口
  desktop/                        Electron 桌面端宿主（窗口生命周期、安全沙箱、托盘、Sidecar 托管）
browser-automation-worker/        Node.js 浏览器自动化 Worker（CDP 控制、页面标记、截图算法）
contracts/
  app-server/                     App Server 协议 JSON Schema 及自动生成物
  desktop-browser/                桌面浏览器控制与网络策略契约
crates/
  magi-daemon/                    业务核心装配、生命周期与持久化调度
  magi-api/                       统一业务应用服务（Turn, Input, Interaction, Goal, Agent）与 HTTP 路由
  magi-conversation-runtime/      会话响应核心、SessionTurnCoordinator、执行大循环与任务分发
  magi-session-store/             本地 SQLite/Sidecar 事务持久化（Turns, Steps, Inputs, Approvals, Goals）
  magi-tool-runtime/              内置工具实现（Shell, Files, Process, Search, Web, Delegation）与策略
  magi-browser-authority/         内置浏览器逻辑 Tab、Surface 权威状态、租约与失效判定
  magi-usage-authority/           Token 计量、上下文压力预测与历史连续压缩
  magi-orchestrator/              任务调度器、TaskStore 依赖图与 TaskRunSupervisor
  magi-mcp-server/                对外标准 MCP 服务端（支持 stdio, 本地 HTTP, Cloudflare 隧道）
  magi-plugin-system/             插件系统管理器、QuickJS 沙箱、版本化资源与扩展加载
  magi-permissions/               工作区路径与工具操作权限判定
  magi-session-isolation/         工作区争用检查与 Git Worktree 隔离工作副本
  magi-agent-role/                子代理角色模板与注册表
  magi-skill-runtime/             Skill 规范加载、验证与运行
  magi-knowledge-store/           代码索引与项目知识存储 (ADR, FAQ, 经验)
  ...                             （共 35 个独立细分 crate）
web/                              Svelte 5 响应式 Web 工作台（Runes 驱动、消息流、RightPane 多功能面板）
plugin-center/                    官方插件中心市场元数据与打包
scripts/                          研发辅助、协议生成、代码生成与端到端验收脚本
~~~

---

### 分层验证规范

根据全仓工程规范，日常开发采用分层定向检查，避免盲目运行全量耗时套件：

~~~bash
# 1. 基础编译与类型检查
cargo check -p magi-daemon
npm --prefix web run check
npm run check --workspace @magi/desktop
npm run check --workspace @magi/browser-automation-worker

# 2. 协议与工具 Catalog 一致性门禁
npm run protocol:check
npm run browser-tool-catalog:check
npm run browser-network-policy:check

# 3. 核心端到端与集成验收
npm run test:daemon-controls     # Daemon 控制流与恢复验收
npm run test:daemon-input-queue  # 输入队列、引导与目标推进真实验收
npm run test:browser-core        # 浏览器自动化核心验收
~~~

---

### 工程原则

- **daemon 是唯一业务内核**：Electron Main 仅负责物理桌面资源与沙箱，不复制业务逻辑；前端仅消费服务端事实。
- **单一事实源与无影子实现**：每项能力只保留一条正式实现路径，严禁双轨兼容分支、静默兜底或重复状态维护。
- **默认主线直接完成**：子代理仅作为特定隔离任务的辅助手段，严格遵守四类委派目的与两层嵌套上限。
- **确定性副作用与恢复安全**：所有破坏性操作与外部交互必须经过审批；崩溃恢复优先核对既有产物，严禁自动盲目重放。
- **本地用户主权**：模型配置、工作区数据、会话历史与沉淀知识默认完全保存在本地用户环境。

---

### 仓库与许可证

- GitHub：[MistRipple/magi-code](https://github.com/MistRipple/magi-code)
- Issues：[问题反馈](https://github.com/MistRipple/magi-code/issues)
- Releases：[版本下载](https://github.com/MistRipple/magi-code/releases)

Magi 核心代码采用 [Apache License 2.0](LICENSE) 开源授权。

### 友情链接

- [Linux.do](https://linux.do)

---

**Magi 的诞生离不开早期支持者的帮助。**

##### 赞助用户

<table>
  <tr>
    <td align="center">
      <a href="https://github.com/Poonwai">
        <img src="https://images.weserv.nl/?url=https://github.com/Poonwai.png&mask=circle&w=80" width="80" alt="Poonwai"><br>
        <sub><b>Poonwai</b></sub>
      </a>
    </td>
    <td align="center">
      <a href="https://github.com/agassiz">
        <img src="https://images.weserv.nl/?url=https://github.com/agassiz.png&mask=circle&w=80" width="80" alt="agassiz"><br>
        <sub><b>agassiz</b></sub>
      </a>
    </td>
    <td align="center">
      <a href="https://github.com/StoneFancyX">
        <img src="https://images.weserv.nl/?url=https://github.com/StoneFancyX.png&mask=circle&w=80" width="80" alt="StoneFancyX"><br>
        <sub><b>StoneFancyX</b></sub>
      </a>
    </td>
  </tr>
</table>

##### 赞助站

**Token 支持**：[BinCode 中转站](https://stonefancyx.com/)

### 联系

无论是功能建议、Bug 反馈还是商务合作，欢迎随时交流。

<p align="left">
  <img src="docs/images/wechat.png" height="180" alt="个人微信二维码">
  &nbsp;&nbsp;
  <img src="docs/images/group.png" height="180" alt="Magi 测试群二维码">
</p>

> [!NOTE]
> **左侧**：个人微信（商务合作/问题反馈） | **右侧**：Magi 测试群二维码

本项目采用 [Apache License 2.0](LICENSE)。你可以在遵守许可证条款的前提下自由使用、修改和分发 Magi。
