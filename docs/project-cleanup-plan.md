# Magi 全项目整理计划

状态：主体清理及 2026-11-11 审计发现问题的修复已完成；验证范围见追加记录，不代表全仓所有行为均已验收。开始日期：2026-11-10。对应用户目标：列出完整计划，以目标模式完成全项目整理。

本文件是这次整理的唯一计划和进度入口；长期架构约束仍归各领域设计文档。每个批次完成后更新本文件，不能把计划、历史通过记录或扫描结果当成当前交付证据。

## 目标与完成定义

完成一次覆盖全部正式源码、工作区依赖、协议、测试入口、构建发布脚本和维护文档的整理。优先消除无效实现、重复事实源、重复业务写入路径、已经撤下的产品入口和为旧实现服务的补偿逻辑。代码量下降是结果，不设机械删行指标。

完成须同时满足：

1. 下方所有区域都有审查记录，区分已修复、确认保留与具体未验收事项；不能只整理 Web 后宣称全仓完成。
2. 已确认的废弃路径连同专属类型、配置、翻译文案、测试和文档一并清退；引用、生成物和正式入口保持一致。
3. 行为、状态、持久化和权限有明确所有者；修改沿原入口完成，不增加双写、影子状态、自动切换业务实现或为通过测试而伪造成功。
4. 每个行为改动有相应定向回归；收尾完成 workspace 编译、Web/Desktop/Worker 检查及受影响的协议/架构生成一致性检查。真实进程和 UI 行为按实际改动选择现有验收入口。
5. 保留所有既有无关改动；最终报告只把本次确实执行的验证算作证据，并列明外部服务、平台和现场操作的覆盖范围。

不把“全项目整理”扩大为无需求的全仓重写。多种 Provider、MCP 传输、插件翻译器、必要的数据版本迁移是正式能力，不能仅因存在多个 adapter 就删除。真实用户数据、开发者环境及发布历史不按“旧文件”批量清空。

## 起始基线

- 当前 checkout 已有 **366** 条修改/删除/未跟踪路径，来自此前多轮工作；本任务沿当前 checkout 推进，不重置或覆盖它们。
- Cargo workspace 为 35 个成员（含 daemon app）；JS workspace 为 Web、Desktop、Browser Worker、Desktop Browser contracts。
- 初步行数统计包含测试与文档，不用于比较纯业务代码大小。较大区域为 Web、API、Conversation Runtime、Tool Runtime、Session Store、daemon；大文件先区分实际业务与内嵌测试，再决定是否整理职责。
- 此前已完成的主线状态、代理投影与 Turn 恢复资格收敛是当前基线。后续不能恢复已删除的 IDE 宿主代理、loopback 模拟服务或独立运行状态消息入口。

## 执行顺序

| 阶段 | 状态 | 范围与具体产出 | 退出验证 |
| --- | --- | --- | --- |
| P0 全仓盘点 | 完成 | 已核对 35 个 Cargo 成员、4 个 JS workspace、目录覆盖、Rust 直接依赖引用及顶层脚本引用；具体语义审查在后续阶段继续 | Cargo metadata 成功；全部顶层脚本有仓内引用，名称含 legacy 的发行迁移脚本不能按名称删除 |
| P1 Web 运行态与展示 | 完成 | 已清退旧诊断结构、恢复面板无生产方分支、无消费者的知识审计副本、重复时间线参数；已核对 store/bridge 与全仓导出消费者，清退无调用包装，并修复共享文件/工具卡操作嵌套 | Web check；对应 canonical、runtime、transport、store、i18n 回归；涉及产品显示时补真实或组件渲染验收 |
| P2 会话与执行内核 | 完成 | API → TurnService → Conversation Runtime → SessionStore/TaskStore → daemon 持久化；检查接纳、取消、终态、继续、重启和独立代理的唯一入口与代际校验 | 受影响 Rust package check 和相应生命周期/恢复回归 |
| P3 工具、模型与数据域 | 完成 | 工具目录/授权/执行、MCP、模型协议、上下文/用量、知识/记忆、项目/Git/隔离/快照；清退未接线服务、重复聚合及无效依赖 | 领域回归；工具目录与协议一致性；必要本地集成 |
| P4 插件与翻译边界 | 完成 | 插件包/生命周期/Node 宿主/翻译安装/页面服务/模型与用量；检查同一业务是否存在旁路、旧注册方式或仅样本有效的补丁 | 相关 Rust 宿主/插件测试与 compat 回归；不把第三方业务自身问题当成本项目缺陷 |
| P5 Desktop 与 Browser | 完成 | Main、可信 Renderer、物理 guest、逻辑 Browser Authority 与 Worker；检查旧宿主接口、重复状态、资源释放和失效边界 | Desktop/Worker check 与相关测试；Browser core；物理行为变化再做 Electron 验收 |
| P6 工程与文档 | 完成 | 清理孤立脚本、过期入口、无效依赖、测试空壳和过时“当前状态”描述；合并重复说明，保留实际合同及必要迁移/发布信息 | 脚本/链接引用、manifest、生成物与文档架构检查 |
| P7 总体验收与交付 | 完成 | 复核各区域覆盖、最终差异和残留；运行整体静态检查，汇总真实验证范围 | 所有阶段闭环；最终结果、保留理由与验证限制可追溯 |

每阶段采用“源码及调用方证据 → 最小完整修改 → 定向验证 → 清退旧路径 → 更新记录”。允许交叉解决已确认的依赖，但不启动新的平行业务实现。不因一个模块尚需审查就阻塞其他独立整理。

## 全仓覆盖表

| 区域 | 结果 | 调用链、保留理由与证据 |
| --- | --- | --- |
| `web/` | 已清理并验收改动面 | 无生产方诊断、API 包装、旧工具和 i18n 清退；store/bridge 仍消费 daemon；文件/工具卡共用独立操作与样式，Web check、定向回归及真实组件浏览器检查通过 |
| `magi-api`、`magi-conversation-runtime`、`magi-daemon`、`apps/daemon` | 已清理并核对 | TurnService 接纳 → 任务派发 → canonical 写回 → daemon 事件落盘保持原链；模型配置失败不切换客户端。Conversation Runtime 733 项通过，daemon/开发 app 全目标编译通过 |
| `magi-session-store`、`magi-orchestrator`、`magi-event-bus`、`magi-runtime-state`、`magi-plan` | 已清理并核对 | SessionStore 198、Orchestrator 65、RuntimeState 3 项通过；canonical/TaskStore/计划各有唯一 owner。清退旧客户端租约、恢复读包装、无调用接口。低层事务测试仍通过真实 Store 入口构造状态，不以测试放行生产条件 |
| `magi-tool-runtime`、`magi-mcp-server`、`magi-process`、`magi-safety-gate`、`magi-permissions` | 已清理并核对 | 工具风险沿 ToolRegistry → GovernanceService，目录/命令沿 policy/permissions，进程由 ManagedProcessGroup 托管；MCP stdio/HTTP 是两种正式传输。撤下旧治理分支、无调用权限和进程包装；对应治理、权限、安全回归通过 |
| `magi-bridge-client`、`magi-context-runtime`、`magi-usage-authority` | 已清理并核对 | Provider adapter 和 tool_choice 能力协商仍服务真实请求；并发分批只有 `partition_tool_calls_with_inputs` 执行链。ContextAuthority 保有模型上下文压缩。用量统一从事实账本聚合，删除第二套内存重置及旧聚合包装；UsageAuthority 35 项与正式统计重置回归通过 |
| `magi-knowledge-store`、`magi-memory-store`、`magi-project-memory`、`magi-governance` | 已清理并核对 | 知识图谱/检索索引、会话抽取、项目 Markdown 记忆用途不同，均有正式消费者；删无调用索引接口及未接线偏好/压缩状态，保留 apply_extraction 与项目 memory_write。记忆/上下文/编排回归通过；废弃治理 trace 连专属测试删除 |
| `magi-workspace`、`magi-git`、`magi-snapshot`、`magi-session-isolation` | 已核对并保留正式能力 | 项目及源目录身份、隔离映射、Git/worktree 和文件快照沿各自 registry；多目录的简化读取拒绝歧义，不选第一项。源目录旧数据只作单向迁移；快照旧压缩格式和内容哈希仍有读取合同。派发隔离、目录投影、恢复清理回归通过 |
| `magi-core`、`magi-media`、`magi-agent-role`、`magi-skill-runtime`、`magi-settings-store`、`magi-appearance` | 已清理并核对 | 清退角色旧提示词组合/能力包装、无调用 Core getter；媒体共用原始字节准入，角色/Skill/设置/外观由 daemon 装配并被正式请求消费。未把辅助模型、图片处理或主题运行库误判成重复内核 |
| `magi-plugin-system`、`magi-extension-host`、`scripts/compat`、`plugin-center` | 已核对与定向验证 | HTTP/宿主共用 PluginOperations，安装/版本/授权归 PluginManager，执行经 ExtensionBus；翻译器保留原业务并映射公共宿主能力。移除无调用排序 setter；插件/宿主 Rust 137 项、compat 169 项与中心生成检查通过 |
| `apps/desktop`、`magi-browser-authority`、`browser-automation-worker` | 已清理并验证 | 物理 guest/Main、页面算法/Worker、逻辑状态/Authority 的控制链保留；撤下 Authority 旧恢复、旧 snapshot 校验及无调用 lease 释放入口，测试改走当前恢复/验证/撤销接口。Authority 68、Desktop 154、Worker 76 项与 browser-core 通过 |
| `contracts`、`magi-app-server-protocol` | 已核对一致性 | App Server、Browser 工具与网络规则仍以 schema/目录为源；生成检查、Desktop contracts 类型检查通过。本轮没有手改生成协议来配合 UI |
| 根 manifests、`scripts`、`.github`、`docs`、README、`examples`、agent 配置 | 已清理并核对 | 无调用直接依赖已撤下；235 个维护脚本语法通过；本地 Markdown 文件链接无断链；Gemini 均只导入 AGENTS。发布门禁允许明确的旧更新器签名工具安装，撤下字符串规避及重复扫描规则；保留仍被发行流程使用的升级迁移包生成脚本 |

## 已确认候选与处理边界

| 候选 | 当前证据 | 处理 |
| --- | --- | --- |
| 旧运行态诊断类型 | executionGroup/plan/governance/state diffs 保留声明；当前 `deriveOpsView` 未提供对应事实 | 清退旧投影和专属展示分支，正式 Goal/执行步骤能力沿原入口保留 |
| 恢复面板历史字段 | 恢复摘要生产方只写继续说明、时间和计数，面板仍包含旧快照/评审/重规划分支 | 按真实生产字段收缩类型、渲染、格式化和文案 |
| Web 知识审计副本 | `deriveKnowledgeAudit` 的结果目前只有测试消费，没有产品视图读取 | 清退前端冗余聚合；daemon 知识审计事实与正式检索能力保留 |
| 时间线双起点 | RuntimeStatePanel 对两个起始参数传入同一 canonical startedAt | 合为一个明确起点，删除旧回退参数并验证过滤与去重 |
| 无数据来源的固定指标 | 进度、验收、阻塞 UI 读取已无生产方的运行态字段 | 先核对正式 Goal/执行步骤/Task 信息职责；撤下废弃的旧指标合同，不造默认数字或平行计数器 |
| 超大源文件 | API/Conversation/Store 包含大量内嵌测试；行数不等于废弃业务 | 只在责任或验证边界有明确收益时拆分；不为行数换文件位置 |

## 批次记录

### 2026-11-10：目标建立

- 已建立持续目标，完成初步目录、workspace 和规模盘点。
- 当前尚未宣称任何全仓阶段完成；前几轮通过记录保留在会话中，本轮后续检查单独记录。
- 无发布、Tag、PR 合并或用户状态目录清理动作。

### 2026-11-10：P1 诊断残留与 P6 依赖首批

- 清退 Web 无生产方的进度/评审/阻塞卡片、旧计划/执行组/治理/状态差异类型、恢复快照/重规划/评审分支、无消费者的知识审计聚合与专属测试。保留真实上下文指标、恢复摘要、代理和关键运行记录。
- 上下文卡片主数值改为实际占用率，不再展示无数据的耗时。未知值仍显示未知；移除 49 组中英文废弃文案。时间线筛选只保留一个 canonical 起始时间。
- 固定属性测试首次发现插件夹具缺少当前必填的 `upstream: null`，按 `StoreItem` 合同修正夹具，并删除该夹具过期的组件参数；没有为测试增加产品兜底。
- 按所有 Rust 源码、manifests 和正式调用核对，移除 daemon app 的 `tracing`、permissions 的 `serde`、context runtime 的 `serde_json` 三项未使用直接依赖。workspace_vcs 未调用的序列化辅助函数移除；正式路由已有同类型序列化调用与业务测试。
- 本批验证：Web check 0 错误/0 警告；runtime-state、context-ring、fixed-properties（修正夹具后）、i18n 通过；`cargo check -p magi-daemon-app -p magi-permissions -p magi-context-runtime --all-targets` 通过（包含业务依赖编译）。
- 当前 `38123` daemon 未运行，尚未做本批真实 Electron/浏览器布局验收；组件状态渲染和固定属性回归已执行，不能替代真实窗口验证。

### 2026-11-11：P1 API 包装与卡片操作收尾

- 承接上一批清退无调用 Web API 包装、旧宿主模式检测与仅测试消费的展示工具；复核全仓引用后再移除 `AgentWorkspacePickResult`、`GoalAllowedActionsDto` 两个遗留声明。内部仍被调用的导出不能按“没有外部引用”机械删除。
- 文件变更与普通工具卡存在跨组件的按钮嵌套。原组件将文件预览和展开操作改成并列原生按钮；移除整行伪按钮和重复键盘处理，沿共享 tool-card 样式提供 28px 桌面/44px 触摸热区，保留原来的目录身份、右栏入口及展开状态。未增加业务状态或新组件框架。
- 验证：Web check 0 错误/0 警告；agent-api、file-preview-scope、tool-call-display、conversation-disclosure、right-pane、image-generation-preview 均通过。渲染回归覆盖创建/编辑/删除/重命名、详细/摘要、内部/外部会话及不同工具状态，防止再次嵌套按钮。
- 使用独立临时 state root，按 `scripts/dev-daemon.sh` 启动 daemon，经 `http://127.0.0.1:38123/web.html` 在真实 headless Chromium 挂载原组件验收：1000px 中文浅色、375px 英文深色触摸模拟；鼠标预览不改变展开、Space 展开/收起、Enter 预览、sourceFolderId 保留、无横向溢出及热区通过。主题使用正式 appearance runtime 应用。截图为 `/tmp/magi-cleanup-cards-1000.png`、`/tmp/magi-cleanup-cards-375.png`，已目视检查。属于真实组件浏览器验收，不代表整个 Electron/手机真机流程通过。

### 2026-11-11：P2 模型配置错误不再触发替代执行

- 子代理模型预检原来在配置解析失败或 HTTP client 构造失败时，可退回注入客户端；同时存在仅为拓扑测试保留的“没有模型也通过”分支。两者均已移除。
- 主对话、子代理和扩展模型准入共用配置存在性判定；损坏的来源目录不再被当作“未配置”。只有没有目录及来源选择时，才允许明确注入的模型客户端。来源已撤销、模型选择无效、角色绑定失效按原失败入口返回，不自动切换来源。
- 核对 TurnService 接纳、SessionStore canonical 写入、会话 writeback、Continue 资格与 daemon 事件恢复入口：canonical 日志仍为事实源；sidecar/Thread 为投影，转交子树不能被原主线重新打开。没有添加第二条接纳或恢复链。
- 验证：agent_spawn 定向 24 项、tool_batch 101 项通过；新增 model_preflight 1 项覆盖错误目录、撤销来源、失效模型与无模型；resolve_target_for_role 3 项、main_model_sources 6 项通过。旧的扁平 orchestrator 夹具曾将错误结构当作未配置，已改为断言拒绝，未放宽正式 schema。

### 2026-11-11：P3 无调用接口、治理空壳和记忆残留

- 通过全仓源码、测试、脚本和文档交叉引用，清退 17 个文件中的 27 个无调用方法：旧 MCP builder 包装、Task getter、EventBus 恢复包装、账本校验包装、扩展排序 setter、API 无作用域快照包装、重复“真实模型”注入入口及标记、未接线快照枚举/索引状态接口、上下文旧 key/anchor 包装、进程 stdin 包装、角色提示词旧组合入口与权限旧适配器。编译暴露的私有 `exclusive_scope()` 孤儿也一并清退。
- `magi-governance` 的 Worker 控制、路径/sandbox 判断及独立 trace 仅有测试调用，已连同请求类型、trace 模块和专属测试撤下。保留 ToolRegistry 正在使用的工具风险/显式审批判定及正式输出类型；真实目录/命令/代理权限仍由原 policy、permissions、preflight 负责。移除不可配置却伪装成配置的 thresholds 对象和不可达审批分支。
- `magi-memory-store` 的偏好存储和独立压缩历史从未接入生产，已删除模块、类型、内存状态及专属测试。移除只被该压缩器写入的 `compacted` 标记。正式记忆抽取与 ContextAuthority 的上下文压缩继续各司其职；记忆抽取写入统一为 `apply_extraction`，删除只供测试独立写抽取记录的公开接口，损坏数据测试在测试模块中显式构造。
- 验证：一次 workspace 全目标编译通过并指出删除造成的私有 getter 孤儿（已修复，后续收尾重跑）；governance 1、permissions 31、safety-gate 19、ToolRuntime governance 集成 1 项通过；memory-store 3、orchestrator 65、context-runtime memory 2 项通过。只存在于测试中的只读检查接口不能直接等同废弃业务；后续候选仍须按调用和用途判断。

### 2026-11-11：P6 旧客户端发现机制

- RuntimeState 中旧客户端租约的写入/读取/过期扫描只被自身测试使用，已撤下类型、目录状态、方法与专属测试；删除无调用的 PID 读取与默认地址猜测包装。保留正式 daemon 的 runtime.json、PID 写入/退出删除以及进程存活检查。
- 更新状态根注释，明确项目数据归全局 projects 布局，源目录旧 `.magi` 只作迁移输入；启动日志不再称当前 daemon 为“影子后端”。没有删除任何用户状态文件。
- RuntimeState 3 项回归及 Desktop/Worker/contracts、协议/目录/网络规则/架构生成一致性检查随后全部通过，最终记录见 P7。

### 2026-11-11：P3 用量、P4 插件和 P5 Browser 收尾

- 用量重置的正式事实是 `usage.stats.reset` 账本标记。旧 `UsageAuthority::reset_session/reset_workspace` 只被测试调用，会另造内存 reset 事件，已连同旧事件分支、只供旧 session 聚合的包装和专属测试清退。所有聚合测试直接验证正在使用的 reducer；真实账本、重置标记及插件/会话归属不变。
- Browser Authority 的 `begin_runtime_recovery` 无生产调用，另做一遍恢复状态变换；已移除，恢复回归改走正式 durable restore。旧 `apply_host_snapshot_revision` 与现行 `validate_snapshot_result` 重复且缺少导航代次，已移除；无调用的 lease release 包装亦撤下，回归直接验证正式 revoke 入口。
- 插件与翻译器保持产品合同。逐层核对 PluginOperations、PluginManager、HostService/ExtensionBus 和安装翻译入口，没有因名称为 compat、重试或版本转换就删除正式适配。
- 验证：UsageAuthority 35 项、API 正式统计重置 1 项；Browser Authority 68 项；插件系统/扩展宿主 Rust 137 项；compat 169 项；Desktop 154 项、Worker 76 项；browser-core 均通过。外部社区插件的真实模型/账号业务未在本轮重跑。

### 2026-11-11：P2/P6/P7 最终验收

- 扩大派发回归时发现既有隔离目录测试没有 canonical Turn，因而停在正式输入校验。补齐同一 Session 的 canonical Turn 与 TaskExecutionPlan 后仍验证真实隔离目录准备和执行器 scope 拒绝，不增加产品兜底。派发 72 项通过；随后 Conversation Runtime **733**、SessionStore **198** 项完整库测试通过。
- 发布工作流原先用 `printf 't''auri-cli'` 规避过宽的废弃宿主扫描。改为显式安装固定版本签名 CLI；唯一发行门禁只允许 release.yml 中这一精确命令，仍拒绝旧宿主构建、其他流程安装和无版本安装。CI golden 调用同一个门禁，不维护第二份相冲突规则。CI workflow、update manifest 和发行边界检查通过；没有执行签名或发布。
- 一处文档链接指向已不存在的 target 验收产物，改成明确的历史路径与证据限制，没有伪造或补写历史通过报告。全部当前 Markdown 本地文件链接检查通过。
- 最终 `cargo check --workspace --all-targets` 通过，**0 警告**。Web check 0 错误/0 警告；Desktop、Worker、Desktop contracts 检查通过；App Server、Browser tool catalog、网络策略、crate 架构图与插件中心一致性检查通过。235 个维护脚本（含未跟踪的新脚本）语法检查通过，`git diff --check` 通过。
- 已关闭本轮临时 daemon；保留用户既有修改与构建缓存，没有提交、发布、Tag 操作或清空用户状态目录。

### 2026-11-11：提交后审计与修复

- 修复当前 Turn 的图片在输入消费后丢失：每次装配都从 canonical 用户 item 为保留在历史中的当前输入恢复图片，不把消费标记当作附件寿命。覆盖首轮、空响应恢复、工具续轮，以及消费后序列化重启；线程历史不保存第二份图片。
- 文件事实刷新只处理 `succeeded` 且有非空路径/内容哈希的读取回执；失败、拒绝、取消、未执行及身份不完整的回执保持原错误和恢复指引。子代理切换执行目录时的 `workspace_execution_root_changed` 不再被覆盖。
- 三类过期测试同步到当前入口：四处 MCP 夹具按精确名称取得 schema；恢复夹具显式绑定 Session 恢复引用；Web 隔离回归验证正式列表请求，不再要求已删除的单会话包装。没有恢复旧执行或恢复链路。
- 删除只被测试使用的无参数并发包装，测试直接覆盖正式带参数分批入口。MemoryStore 的直接写入种子收进 `test-support`，生产继续使用 `apply_extraction`。
- 完整回归额外暴露两处既有问题并按根因修复：SSR golden 不需要客户端依赖预构建，现已在测试环境配置入口关闭，删除临时目录清理重试；快照的单活动工具只有在未声明路径时才沿用原全树归属规则，已声明路径必须匹配，防止并行工具结束后认领其他文件。快照新增确定性回归在修复前失败、修复后通过。
- 验证：API 完整库测试 **1002 通过 / 1 忽略**；BridgeClient **233**、ContextRuntime **21**、MemoryStore **3**、daemon **161** 项通过。首次组合回归的快照归属失败已按上一项修复，最终 Conversation Runtime **734**、Snapshot 库 **26**、Snapshot lifecycle **32** 项通过。完整 Web 测试通过，Web check **0 错误 / 0 警告**；生产 `cargo check -p magi-daemon`、脚本语法及差异检查通过。日志为 `/tmp/magi-fix-*.log`，属于本机临时证据。
- 原始输入写入 Durable 记忆仍有 ContextRuntime 的正式消费者，不是无调用残留；本次保留既有记忆策略。重新定义哪些内容应进入长期记忆属于产品语义调整，不能随接口剪枝静默删除。未重跑真实 Electron、第三方远程模型/账号业务、全 workspace 测试或全量 Clippy；没有发布或修改用户状态数据。

### 2026-11-11：本地目录包验收

- 当前工作区通过 `CSC_IDENTITY_AUTO_DISCOVERY=false npm run desktop:package -- --dir` 构建 macOS arm64 的 Magi 3.0.62，目录包位于 `target/electron-dist/mac-arm64/Magi.app`（约 970MiB）。Web、Worker、release daemon、Electron 与随包运行时完成组装，解包资源哈希及随包 Node 执行自检通过；`npm run release:guard` 通过。
- 真实 Electron 对话验收暴露测试夹具残留：读取最后一条 user 消息时误取运行时上下文，工具流未提供用量回执，审批查询仍使用旧按钮类名。夹具按现行上下文封装、流式 usage 和共享按钮类修正，移除旧 Task 文本包装解析；保留全部业务断言，没有修改生产链路或增加兜底。
- 使用独立临时状态根、独立端口及本地受控 Provider 运行 `test:electron-conversation-dom`，最终 **221 项检查全部通过**、进程退出码 0。覆盖个人/项目对话、排队/停止/继续、工具、Goal 成功与失败、子代理、统计、权限拒绝、Git dirty/drift/conflict、Git 分支创建审批、审批允许/拒绝/取消、daemon 重启及宿主退出结算。验收期间源码与包指纹稳定，无 Desktop IPC 协议拒绝。
- 本机日志为 `/tmp/magi-local-package.log`、`/tmp/magi-local-package-guard.log`、`/tmp/magi-local-package-dom.log`，结构化结果为 `/tmp/magi-local-package-dom.json`。本轮未生成 DMG/ZIP、未执行正式发布完整预检、跨平台安装和第三方远程模型/账号业务；目录包可用于本地测试，不代表正式发布门禁已完成。

## 交付范围与保留边界

本轮完成覆盖 35 个 Cargo 成员、4 个 JS workspace 以及协议、工程脚本、文档的审查和上述清理。逐项调用证据、修复和回归见覆盖表及批次记录；不以净删行数作为验收条件。

明确保留：正在使用的插件翻译器、Provider/MCP 适配、来源/版本/租约校验、单向数据迁移、旧更新器签名桥、当前正式事务的底层接口和测试所需的事实查询。大文件中的真实业务与回归不因行数多而搬家或删去；扫描出的公开方法、测试夹具和 re-export 不是自动删除清单。

本轮未执行全 workspace 的所有测试、全量 Clippy/依赖安全审计、安装包重建与发布预检；未重跑第三方插件账号/远程模型业务，未做 Windows/Linux、手机真机或打包 Electron guest 的完整现场验收。相关合同与既有证据仍在领域文档中，本轮只认上述实际执行的结果。后续新增产品需求和新发现的问题沿原 owner 与入口修复，不恢复本轮删除的独立写入、旧恢复或配置错误兜底。
