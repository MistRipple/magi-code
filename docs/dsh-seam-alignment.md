# DSH 对齐：Magi 插件系统的当前设计

> 状态：里程碑改造的权威设计，2026-11-17。**本文取代此前的插件设计文档**（`plugin-system-requirements-design.md` 及其附属的剪除审核、交付记录），旧文档已过时，不再修改，仅作历史。作者指南 `plugin-authoring.md` 与 `extension-protocol-v1.md` 在本文的接口章节写成之前仍是现行接口参考，之后由本文取代。
>
> 本文不代表任何一项已交付。依据：DSH 仓库 `docs/capability-seams.md` 的全部 113 个 `ctx.*` 服务（master，2026-11-17 读取，不锁版本）；Magi 源码与现有翻译层规则；对 100 个真实 DSH 插件的静态扫描（第 4 节）。表中分类是文档与名称层面的判断，不是逐项代码审计。

## 1. 决策（用户）

1. **Magi 的插件接口按 DSH 对齐：DSH 支持的，Magi 同样支持。** DSH 对插件放行的事项（审批决策、系统提示整体改写、模型请求拦截、替换执行循环和会话存储后端等），Magi 不另设例外。此前提出的“事实类是否放开”“工作流是否恢复”“新建类是否都做”均按此处理，不再单独待决；先做哪个，只由实际依赖决定（第 7 节）。
2. **会话能力组合与 DSH 一致。** 插件按应用全局安装与启停；会话启用哪些能力，由预设从已启用插件中组合。
3. **不锁定 DSH 版本，跟随上游。** DSH 业务线已基本稳定；上游更新后重新读取接缝清单核对本表。
4. **Magi 内核实现保持自有，只在对外接口上对齐 DSH 的接缝。** 不要安装期翻译与改写这类中间层：对齐后安装只做校验与放置，运行在单层宿主里。
5. **旧设计文档不再修改。** 本文是替代。

## 2. 对齐原则与“完全运行”的定义

- **完全运行** = 只使用 DSH 公开接缝的插件，其用户目标在 Magi 中完整可用。
- 接口对齐分三层：签名；语义（顺序、`next` 环绕、错误、取消、时机）；对象身份。前两层对齐。**对象身份无法跨进程对齐**：插件直接改内核活对象（例如把方法补丁到 Session 实例上）时，只能识别其目的，把它落成等价的公共端口。
- 给 DSH 宿主安装文件打补丁的功能（如 dsh-purge 的 86 条宿主补丁）在 Magi 中没有宿主可打，标“不适用”，不显示为失败。
- DSH 专属名字与逻辑只放在 Node 侧宿主服务层，Rust 内核不出现 DSH 分支。
- 每对齐一个接缝，同步清退它对应的翻译代码，不保留双份。

## 3. 处置分类

| 处置 | 含义 |
| --- | --- |
| 已有 | Magi 已有对应公共能力或翻译层已接，补语义细节 |
| 补端口 | Magi 内部有该能力，要新增或放宽对插件的端口 |
| 新建 | Magi 没有对应能力，需先设计 |
| 页面宿主 | DSH 的宿主、客户端或 API 外壳；Magi 自有，由页面宿主为依赖它的插件页面提供最小服务外观 |
| 无需 | 遥测、账号、实验、调试类；DSH 插件以条件注入使用，缺席时自行降级，Magi 保持缺席 |

## 4. 真实插件的依赖实测

**方法。** 取 npm 上关键字 `dsh-plugin` 搜索结果前 100 个包（2026-11-17），排除 4 个 PDF 原生二进制子包，其余 95 个用 `npm pack --ignore-scripts` 下载解压（不执行任何包代码），用仓库内 `scripts/compat` 的静态扫描器分析。

**覆盖。** 95 个中 79 个被识别为 DSH 插件并完成扫描；16 个不是插件（目录、命令行工具、规范文档）或没有可扫描源码。**这是静态扫描，不是业务验收；样本按搜索相关度取得，不代表整个生态。** 按当前翻译器的准入规则：50 个无阻塞，29 个被阻塞。

**使用最多的服务**（使用该服务的插件数，79 个中）：`tools` 34，`webServer` 28，`settings` 14，`commands` 14，`connection` 14，`llm` 9，`skills` 9，`credentials` 9，`agents` 8，`sessions` 6，`pluginManager` 5，`systemPrompt` 4，`storageDomain` 4，`workspaceRegistry` 4。使用事件最多的是 `session/event` 7、`agent/created` 5、`agent/pre-step` 3、`tools/pre-execute` 3；`system-prompt/assemble` 仅 1 个，`llm/stream` 在该样本中为 0。

**29 个被阻塞的原因**（受影响插件数）：`session/event` 逐条事件 6；`ctx.agents` 5；`skills.registerProvider`（含 `dsh-skill` 导入）5；`dsh-session` 导入 4；`agent/pre-step` 3；`agent/created` 3；`storageDomain` 3；`tools.guard` 3；`credentials` 约 3；`sessionQuery` 2；`tools/result`、`pre-execute`、`post-execute` 各 2；`workspaceRegistry` 2。

**结论。**
- 绝大多数插件依赖的是工具、页面路由、设置、命令，这些已经接通。
- 阻塞集中在**读取会话与 Agent、会话与 Agent 事件、工具执行管线事件、技能提供方、凭据、存储域**。这些都是“补端口”或“新建”，而且以只读或观察为主。
- **要求替换执行循环或会话存储的插件，在样本里没有出现**（`agentLoop` 0；`sessionPersistence` 有 3 个插件注入它，但不构成阻塞原因，需要核对具体用法）。系统提示整体改写与模型请求拦截在样本中各只有 1 个或 0 个插件（dsh-purge 在样本之外单独分析）。

**两个指定样本。**
- **dsh-rea 1.2.0**：只读取 `tools`（`tools.schemas`），14 项全部已接，无阻塞。它本质是把上游 REA 的 MCP 服务注册为 `mcp__rea__*` 工具。
- **dsh-purge-community 1.7.18**：148 项，7 项已映射、8 项 shim、115 项随包、5 项部分、1 项丢弃、12 项未登记。它需要：`agent/pre-step` 环绕（过滤步骤消息）、`llm/stream` 环绕（拒答检测后重试）、`system-prompt/assemble` 与 `systemPrompt.ctx`、`session/created` 与 `session/event`（读取 `header.cwd`）、`agent-preset/selected`、`agent/inbox/claimed`，以及 `agents.get`、`sessions.all/list`、`connection.trustedHosts`。扫描器把它挂在 `ctx` 上的 8 个私有标记（`ctx.__dshPurge*`）也报成“未登记”，这是扫描器对插件私有属性的误报。它还直接修改会话实例（给 `deriveMessages` 打补丁），按第 2 节只能识别目的，落成“消息投影变换”端口。它的宿主补丁、备份回滚、DSH 自更新等功能不适用。

## 5. 对照表

按 DSH `capability-seams.md` 的 113 个服务逐项列出。最后一列是第 4 节样本（79 个已扫描插件）中使用该服务的插件数，0 表示样本中没有用到。

汇总：已有 9，补端口 38，新建 19，页面宿主 28，无需 19（共 113 项）。

### 一、代理运行时与会话（决定插件能不能改变代理的行为）

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.agentLoop` | bundle | 补端口 | Turn 执行器；外部执行器接口（ExecutorCapabilities） | DSH 中循环本身是一个可替换插件。落点是 Magi 已预留的外部执行器接口，见第 6 节 | 0 |
| `ctx.agents` | core | 补端口 | Task / Thread / 角色 | DSH 是活 Agent 句柄与 create/resume 工厂；Magi 以句柄代理加提案表达，不暴露活对象 | 8 |
| `ctx.agentPresets` | core | 新建 | 角色（agent-role）与能力单元 | 按决议：插件全局安装启停，会话能力由预设从全局已启用插件中组合；需先做能力单元注册表 | 1 |
| `ctx.agentDefaultModel` | core | 已有 | 主对话默认模型选择 | 翻译层已接（只读无凭据投影） | 3 |
| `ctx.agentTeams` | core | 新建 | 子代理派发与计划 | 共享任务 DAG 无对应，需设计 | 0 |
| `ctx.subagents` | seam | 补端口 | agent_spawn 与角色绑定 | DSH 有 6 种实现（进程内 spawn/fork 等）；现仅有只能阻止的 guard 钩子 | 1 |
| `ctx.subagentModelSelection` | core | 补端口 | 角色模型绑定 | DSH 默认关闭的设置命名空间；Magi 绑定在角色上 | 0 |
| `ctx.sessions` | core | 补端口 | 会话存储（session-store） | 现仅有调用期 list/get 投影；需完整只读 API 与提案式写入 | 6 |
| `ctx.sessionPersistence` | seam | 补端口 | 会话存储（session-store） | DSH 的合同是存储后端（create/open/stat/list/flush，句柄追加与读取，单写者）。落点见第 6 节 | 3 |
| `ctx.sessionProjections` | core | 补端口 | canonical 投影 | 只读投影接口 | 2 |
| `ctx.sessionProjectionCache` | core | 补端口 | canonical 投影缓存 | 随 sessionProjections | 0 |
| `ctx.sessionQuery` | seam | 补端口 | 会话检索 | seam，后端可替换 | 3 |
| `ctx.sessionReferenceResolver` | core | 补端口 | 会话引用 | 把当前界面的会话快照作为上下文 | 0 |
| `ctx.sessionTitle` | seam | 补端口 | 会话标题生成 | seam，实现可替换 | 1 |
| `ctx.planMode` | core | 新建 | 计划（magi-plan） | 已核对：Magi 的 PlanStore 是用户可见的阶段计划（最多 12 项），没有“计划模式”及其退出流程 | 0 |
| `ctx.goals` | core | 补端口 | 目标（Goal） | Magi 原生拥有；现被明确拒绝，需放开读写 | 1 |
| `ctx.schedule` | core | 补端口 | 插件作业调度 | Magi 现仅调度插件命令；DSH 是定时向目标会话排入消息 | 0 |
| `ctx.userQuestions` | seam | 已有 | ask_user_question | seam | 1 |
| `ctx.commands` | core | 已有 | 命令贡献 | 翻译层已接 | 14 |
| `ctx.skills` | seam | 补端口 | 技能（skill-runtime） | 现仅接入全局文本技能；其他调用策略、资源根、provider、agent 作用域未接 | 9 |
| `ctx.compaction` | seam | 补端口 | 上下文压缩（context-runtime） | seam，后端可替换；Magi 现为单一权威，放开需设计 | 0 |
| `ctx.tokenMeter` | core | 补端口 | 用量权威（usage-authority） | 现只能观察已提交用量 | 0 |
| `ctx.toolResultPruner` | core | 补端口 | 工具结果投影 | 对应 tool.result 的 advise | 0 |
| `ctx.systemPrompt` | core | 补端口 | 提示装配 | 现只能追加；按决议开放整体改写，并与缓存谱系对接 | 4 |
| `ctx.tools` | core | 补端口 | 工具注册与执行 | 注册已有；pre-execute 的 allow/ask、execute 环绕、restrict 需补 | 34 |
| `ctx.approval` | seam | 补端口 | 审批与安全规则（safety-gate） | 按决议完全放行：插件可决定审批，审批事实仍由 daemon 记录来源 | 0 |
| `ctx.permissionPresets` | core | 补端口 | 访问档位（Restricted / FullAccess） | 对应 workspace-write / danger-full-access 预设 | 0 |
| `ctx.llm` | seam | 已有 | 模型数据源（modelSources） | 翻译层已接；stream / prepareCall 仍是 partial | 9 |
| `ctx.deepseekLlmApiExtensions` | seam | 补端口 | 模型请求拦截 | 请求体扩展需要模型请求前的拦截端口 | 0 |
| `ctx.sessionLogDeepSeek` | core | 无需 | — | DeepSeek 会话日志上传策略，绑定其账号体系 | 0 |

### 二、执行环境（工具背后的可替换后端）

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.fs` | seam | 补端口 | 文件工具 | seam，local/sandbox/ssh 后端可替换 | 2 |
| `ctx.shell` | seam | 补端口 | shell_exec | seam | 0 |
| `ctx.shellEnv` | core | 补端口 | shell 环境变量事实 | 插件声明 DSH_* 环境事实 | 0 |
| `ctx.subprocess` | seam | 补端口 | 进程管理（magi-process） | seam | 1 |
| `ctx.terminals` | seam | 补端口 | 用户终端（PTY，terminal_runtime）与进程工具 | 已核对：Magi 有会话作用域的 PTY 终端；seam 的后端提供方未向插件开放 | 0 |
| `ctx.sandbox` | seam | 补端口 | 隔离与安全规则 | seam | 0 |
| `ctx.sandboxPolicy` | core | 补端口 | 隔离策略 | 随 sandbox | 0 |
| `ctx.jobs` | seam | 补端口 | 后台进程与作业 | seam | 0 |
| `ctx.ptcRuntime` | seam | 新建 | — | PTC（代码执行式工具调用）模式，Magi 无 | 0 |
| `ctx.ssh` | core | 新建 | — | Magi 无 SSH 后端 | 0 |
| `ctx.lsp` | seam | 新建 | — | Magi 无 LSP 工具 | 0 |
| `ctx.web` | seam | 已有 | web_search / web_fetch | 翻译层已接联网提供方选择 | 0 |
| `ctx.spillStore` | seam | 新建 | 工具结果预算与历史压缩 | 已核对：Magi 用预算与历史压缩处理超长输出，没有“落盘并返回引用”的机制 | 0 |
| `ctx.workingDirectory` | seam | 补端口 | 会话工作目录 | 现仅有调用期 cwd 投影 | 1 |
| `ctx.worktrees` | seam | 补端口 | Git 工作树（magi-git）与任务租约 | seam | 0 |
| `ctx.attachments` | seam | 已有 | 私有图片服务 | 翻译层已接图片读写；其余附件语义待补 | 3 |
| `ctx.fileReferences` | seam | 补端口 | 上下文文件引用 | seam | 0 |
| `ctx.workflowEngine` | seam | 新建 | — | DSH：模型编写的 JS 脚本编排并发子代理。Magi 有子代理派发与计划，没有脚本编排引擎；本样本中 0 个插件使用 | 0 |

### 三、外部能力（供应方可替换）

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.mcpResources` | seam | 已有 | MCP 客户端 | 翻译层打入上游 MCP 客户端，调用经 Magi 工具入口 | 0 |
| `ctx.mcpManager` | core | 补端口 | MCP 服务器配置与状态 | Magi 的 MCP 管理在设置中，未向插件开放 | 0 |
| `ctx.browserUse` | seam | 补端口 | 内置浏览器（browser-authority） | DSH 是 provider 注册；Magi 浏览器由权威状态拥有 | 0 |
| `ctx.computerUse` | seam | 新建 | — | Magi 无 computer use | 0 |
| `ctx.cuaAccess` | core | 新建 | — | 随 computerUse | 0 |
| `ctx.cuaPrograms` | core | 新建 | — | 随 computerUse | 0 |
| `ctx.officeKit` | core | 新建 | — | Office 渲染、转换与重算 | 0 |
| `ctx.speechToText` | seam | 新建 | — | 语音转写 | 1 |
| `ctx.speechController` | core | 新建 | — | 随 speechToText | 0 |
| `ctx.forge` | seam | 新建 | — | 代码托管变更请求；Magi 有 Git 能力和 Super-agent 设计稿，尚无对应服务 | 0 |
| `ctx.forgeSession` | service | 新建 | — | 随 forge | 0 |
| `ctx.forgePanel` | service | 页面宿主 | — | 随 forge 的界面远程服务 | 0 |
| `ctx.webhookRuntime` | core | 新建 | — | 外部事件投递 | 0 |

### 四、凭据、设置与存储

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.credentials` | seam | 新建 | — | DSH 凭据引用 seam；翻译层目前保持缺席，必需凭据的业务拒绝安装 | 9 |
| `ctx.authorization` | seam | 新建 | — | 凭据获取流程注册，随 credentials | 0 |
| `ctx.deepseekAccount` | service | 无需 | — | 绑定 DeepSeek 账号体系 | 0 |
| `ctx.settings` | core | 已有 | 插件设置（CAS） | 翻译层已接 | 14 |
| `ctx.configEditor` | core | 已有 | 插件设置写入 | 随 settings | 0 |
| `ctx.storage` | seam | 补端口 | 插件资源存储 | Magi 有带版本的资源；DSH 是通用后端 seam（JSON / SQLite） | 0 |
| `ctx.storageDomain` | core | 补端口 | 插件资源存储 | 随 storage | 4 |
| `ctx.workspaceRegistry` | core | 补端口 | 工作区（magi-workspace） | 只读投影 | 4 |

### 五、宿主、客户端与 API 控制器（Magi 自有界面，由页面宿主提供最小外观）

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.connection` | core | 页面宿主 | daemon 已认证路由 | 翻译层已 shim | 14 |
| `ctx.connection.authentication` | seam | 页面宿主 | — | 宿主认证，Magi 自有 | 0 |
| `ctx.webServer` | core | 页面宿主 | 插件私有路由（api.route） | 翻译层已接；监听器与外壳相关成员无意义 | 28 |
| `ctx.webApp` | core | 页面宿主 | — | DSH Web GUI 地址 | 0 |
| `ctx.clientModules` | core | 页面宿主 | 视图加载 | 插件客户端模块打包与服务 | 0 |
| `ctx.typert` | core | 页面宿主 | 插件私有 Remote 路由 | 翻译层已接 JSON unary；流与 Host 对象查找未接 | 1 |
| `ctx.typertGateway` | core | 页面宿主 | 同上 | 随 typert | 0 |
| `ctx.remoteAccess` | core | 页面宿主 | — | 远程访问，Magi 自有 | 0 |
| `ctx.hmr` | core | 页面宿主 | daemon 运行目录收敛 | 翻译层已 shim（runExclusive、reconcileProfile） | 1 |
| `ctx.pluginManager` | core | 页面宿主 | 插件管理入口 | 翻译层已 shim；写操作沿 Magi 生命周期 | 5 |
| `ctx.pluginMarket` | core | 页面宿主 | 插件中心 | Magi 自有 | 0 |
| `ctx.pluginRegistryProbe` | core | 页面宿主 | — | DSH 插件管理界面使用 | 0 |
| `ctx.profileContext` | core | 页面宿主 | 调用期 profile 身份 | 翻译层已 shim | 4 |
| `ctx.headlessStartup` | core | 无需 | — | DSH 无头运行选项 | 0 |
| `ctx.sessionController` | core | 页面宿主 | Magi API | 会话命令与冷读，Magi 自有 API | 2 |
| `ctx.sessionStatus` | core | 页面宿主 | Magi 界面 | 已读与未读标记 | 0 |
| `ctx.sessionFileReferences` | core | 页面宿主 | Magi API | 文件引用远程适配 | 0 |
| `ctx.sessionSkillCatalog` | core | 页面宿主 | Magi API | 技能目录远程适配 | 0 |
| `ctx.jobController` | core | 页面宿主 | Magi API | 后台作业观察流 | 0 |
| `ctx.credentialsController` | core | 页面宿主 | — | 随 credentials | 0 |
| `ctx.settingsController` | core | 页面宿主 | Magi 设置 API | 设置远程适配 | 0 |
| `ctx.workspaceFiles` | core | 页面宿主 | Magi API | 工作区文件读取与变更流 | 0 |
| `ctx.workspaceChanges` | core | 页面宿主 | Magi 变更视图 | 工作区变更摘要 | 0 |
| `ctx.terminalController` | core | 页面宿主 | Magi 终端面板 | 用户终端的远程适配，Magi 自有 | 0 |
| `ctx.workspaceController` | core | 页面宿主 | Magi API | 工作区命令 | 0 |
| `ctx.directoryPicker` | seam | 页面宿主 | 桌面目录选择 | seam，Magi 由 Electron 宿主承担 | 0 |
| `ctx.directoryPickerController` | core | 页面宿主 | 桌面目录选择 | 随 directoryPicker | 0 |
| `ctx.fileUploads` | core | 页面宿主 | — | 客户端文件上传 | 0 |

### 六、遥测、反馈与实验服务（DSH 插件以条件注入使用，缺席时自行降级）

| ctx 服务 | DSH 角色 | 处置 | Magi 对应 | 说明 | 样本使用 |
| --- | --- | --- | --- | --- | --- |
| `ctx.otel` | service | 无需 | — | 遥测 | 0 |
| `ctx.productAnalytics` | service | 无需 | — | 产品统计 | 0 |
| `ctx.productTelemetry` | service | 无需 | — | 产品遥测 | 0 |
| `ctx.sessionTelemetry` | seam | 无需 | — | 会话遥测 seam | 0 |
| `ctx.messageFeedback` | core | 无需 | — | 消息反馈，写入 DSH canonical 日志 | 0 |
| `ctx.sessionFeedback` | core | 无需 | — | 会话级反馈 | 0 |
| `ctx.actionCards` | service | 无需 | — | 实验：行动卡片 | 0 |
| `ctx.pluginRecommendations` | service | 无需 | — | 实验：插件推荐 | 0 |
| `ctx.deepseekChatImport` | service | 无需 | — | 导入 DeepSeek 聊天导出 | 0 |
| `ctx.cotTranslation` | service | 无需 | — | 实验：推理翻译 | 0 |
| `ctx.translator` | service | 无需 | — | 实验：文本翻译 | 0 |
| `ctx.liveshareShares` | service | 无需 | — | 实验：实时共享 | 0 |
| `ctx.claudeCodeMods` | core | 无需 | — | 实验：Claude Code 模组 | 0 |
| `ctx.inspector` | core | 无需 | — | DSH 调试观察 | 0 |
| `ctx.dynamicCordisRunner` | core | 无需 | — | DSH 开发调试 | 0 |
| `ctx.cordisInspect` | core | 无需 | — | DSH 开发调试 | 0 |

## 6. 执行循环与会话存储的对齐方式

两者在 DSH 中都是可替换的接缝。

**`ctx.agentLoop` ↔ Magi 外部执行器接口。** DSH 的 agent-loop 是“默认的代理驱动”，负责创建或恢复 Agent、驱动模型请求、流式响应、工具执行与会话历史。Magi 已有一个预留的外部执行器接口：执行器声明能力（`ExecutorCapabilities`：输入模态、steer、resume、工具模式、审批模式、取消方式、**上下文归谁**、后台、观察），协议里已有 `magi_owned` 与 `executor_owned` 两种上下文所有权。对齐方式是把插件提供的循环作为一个声明了能力的执行器接入，由 daemon 继续提供租约、取消、用量与回执入口。待审计：现有协议里 `executor_owned` 的完成条件、历史重放与审批交互是否足以承载 DSH 循环的语义。

**`ctx.sessionPersistence` ↔ 会话存储后端。** DSH 的合同是**存储后端**，不是会话语义：`create`、`open(id,'write'|'read')`、`stat`、`list`、`flush`（持久化屏障），写句柄单写者所有权，追加式连续事件日志，崩溃后返回物理有效的日志、语义修复由读取方负责。因此对齐的难点不在“放权”，而在 **Magi 的 session-store 能否表达为这份事件日志合同**。本项待审：读 `magi-session-store` 的持久化模型。可选的落地形态有三种：主存储（Node 后端承担持久化与读取）、镜像（Magi 主存储不变，插件后端接收事件副本）、无。样本中没有插件因它而被阻塞，建议先做镜像形态，是否做主存储形态等有真实需求再定。

## 7. 推进顺序

以第 4 节的实测为依据，从被阻塞最多的接缝开始，每项独立设计、验证、交付，并同步清退对应的翻译代码：

1. **会话与 Agent 的读取与事件**：`sessions`、`agents`（句柄代理）、`session/event`、`agent/created`、`sessionProjections`、`sessionQuery`、`workspaceRegistry`。样本里阻塞插件最多。
2. **工具执行管线**：`tools/pre-execute`、`post-execute`、`result`、`tools.guard`，以及 `execute` 环绕。
3. **技能提供方、凭据、存储域**：`skills.registerProvider`、`credentials` 与 `authorization`、`storage` 与 `storageDomain`。
4. **提示与模型拦截**（dsh-purge 依赖）：`system-prompt/assemble` 整体改写、`llm/stream` 环绕、`agent/pre-step` 环绕、消息投影变换、`agent-preset/selected`。
5. **能力单元注册表与预设**（`agentPresets`）：影响面最大，放在读取与管线端口稳定之后。
6. **执行循环与会话存储后端**：按第 6 节，先审计，再做镜像形态。
7. **“新建”类**（computerUse、officeKit、speechToText、forge、webhook、ssh、lsp、ptcRuntime、workflowEngine、planMode、spillStore）：样本中使用很少，按插件出现的实际需求逐个排期。

## 8. 与项目指令的关系，及未验证的边界

- 项目根目录 `AGENTS.md` 的“功能开发”一节仍指向旧设计文档的两个锚点（翻译功能的产品合同、每轮推进与完成门槛）。本文替代它们，但项目指令文件需要你确认后再改，我没有动。
- 对既有不变量的影响：按决策 1，DSH 放行的事项不再保留“扩展只能阻止、不能批准”“提示只能追加”这类限制；以下仍沿用：扩展不直接写 canonical 事实、一个事实一个写入口、调用与实例可失效、超时与队列有界、撤权优先、未支持的契约明确拒绝。具体字句由后续接口章节定稿。
- **未验证**：本表未逐项审计 Magi 源码（已核对 4 项：`planMode`、`terminals`、`spillStore`、`terminalController`）；样本是搜索前 100 个、静态扫描，不代表生态整体，也没有任何业务验收；扫描器的“未登记”有一部分只是规则表没有收录，不等于 DSH 不支持。
