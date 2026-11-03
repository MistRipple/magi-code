# Magi 插件系统：架构、当前合同与交付边界

【现状】更新：2026-11-03。以本工作区源码和本轮运行证据为准；方案目标不得作为已交付证明。本文完整替换旧 QuickJS 方案。作者可依赖的接口见[作者指南](plugin-authoring.md)，字节协议见[扩展协议](extension-protocol-v1.md)。

## 1. 结论、约定与需求

### 1.1 结论摘要

- 【目标】保留 Rust daemon 唯一业务内核；插件使用独立、完全受信的 Node 宿主。
- 【现状·已运行验证】宿主反向服务、工具／命令、原生调度、全局状态与资源链路已实现；本机真实 daemon 的 20 项纵向回归及真实 Chromium 界面追加 1 项通过，见第 11、12 节。
- 【现状·读码推断】MCP 完整内置，监听、令牌、审批和审计不依赖插件；不包装为第一方插件。
- 【目标】插件能直接使用文件、网络、进程；daemon 服务负责 Magi 业务所有权，不是 OS 沙箱。
- 【目标】扩展只能阻止或提出改写；最终权限、审批、日志和结果由 daemon 决定。
- 【目标】子代理保留自定义角色与模型绑定；不允许扩展静默替换模型。
- 【目标】pi／DSH 翻译器及兼容来源开关推迟到本地系统稳定之后；当前不提供占位功能。
- 【现状】Windows demo、Windows 实机与正式安装包验收后置；本机开发验收不等于这些平台已通过。
- 【目标】工作流／会话引擎替换、数据转换 VM、SDK 1 兼容均不恢复。

### 1.2 状态、证据与术语

【目标】“必须／不得”为硬约束，“应”为默认要求，“可”为允许选项。状态标记覆盖其所在段、列表项或表格行；未实现的合同必须标为目标或待定。

| 标记 | 含义 |
| --- | --- |
| 现状 | 本轮复核；再标已运行验证、读码推断或静态核对 |
| 现状† | 沿用未复核；不能作本轮验收完成依据 |
| 目标 | 已确定的规范，不能据此声称实现完成 |
| 待定 | 需后续产品或发布决定，不做静默默认 |

| 状态 | 术语 | 定义 |
| --- | --- | --- |
| 目标 | 插件包 | 安装、摘要、授权、升级的单元，包含声明式贡献及可选 Node 入口 |
| 目标 | 受信扩展 | 用户权限运行的代码，可直接使用 OS 能力；不是受限脚本 |
| 目标 | 扩展宿主 | 每插件独立 Node 进程，由 daemon 启动、监管和回收 |
| 目标 | ExtensionBus | 钩子分发与提案裁决入口，不是第二个业务事件日志 |
| 目标 | canonical 事实 | 原有 Session／Turn／Task／审批／用量存储提交的唯一业务事实 |
| 目标 | 插件作业 | daemon 计划触发的命令 Turn，携带计划、执行身份和预算 |
| 目标 | 翻译包 | 固定上游版本、适配层和已构建依赖组成的 Magi 包；不带第二个 agent 内核 |

### 1.3 连续编号需求与非目标

| 状态 | 编号 | 需求 |
| --- | --- | --- |
| 目标 | R1 | daemon 唯一拥有会话、Turn、任务、审批、日志与持久化 |
| 目标 | R2 | macOS／Windows 功能合同一致，平台验收和安全限制分别披露 |
| 目标 | R3 | 扩展可直接使用文件、网络、进程，以及观察、阻止和受控改写 agent 行为 |
| 目标 | R4 | MCP、Browser、Subagent 等原生功能继续内置；只有公开 API 可独立实现的功能才作为插件 |
| 目标 | R5 | 本地 ZIP、地址与插件中心走统一预览、摘要确认、安装和授权 |
| 目标 | R6 | 支持工具、命令、Skill、设置、视图、资源及 browser surface 声明 |
| 目标 | R7 | UI 与扩展共享 daemon 资源／设置事实；CAS 冲突明确，不覆盖新值 |
| 目标 | R8 | 工作流与会话内核保持原生；通过公开钩子和命令扩展业务 |
| 目标 | R9 | 启用、就绪、停用、撤权、升级、崩溃、取消均有可观察状态 |
| 目标 | R10 | pi／DSH 兼容子集后期推进；来源开关只负责发现，不自动执行 |
| 目标 | R11 | 自定义子代理角色和模型绑定继续生效，不按扩展提案静默换模型 |
| 目标 | R12 | 不支持的契约明确拒绝；无影子实现、兼容补丁或静默兜底 |
| 目标 | R13 | 版本、能力范围、拒绝项与验证报告可核对 |
| 目标 | R14 | 作者只使用公开接口开发、打包、诊断和分发，不需要修改 Magi |

【目标】非目标：替换或 Rust 复刻 DSH 内核；任意上游原包直接兼容；扩展写 canonical 事件或批准审批；任意原生二进制／Node 原生模块；安装时运行 npm／git 构建或第三方脚本；恢复 QuickJS、工作流引擎和数据转换通道。

## 2. 阶段 A：现状审计与缺陷收敛

### 2.1 当前贡献与完整后台 API

| 状态／依据 | 项目 | 当前合同与证据 |
| --- | --- | --- |
| 现状·已运行验证 | 包 | SDK 2，固定单入口 `extension/index.mjs`；SDK 1、backend、未知字段与未接线模式拒绝；[package.rs:192](../crates/magi-plugin-system/src/package.rs#L192) |
| 现状·读码推断 | 贡献全集 | skills、tools、commands、views、resources、browserSurfaces；views 可为 settings、home、main、rightPane、homeCard；[生成清单类型:1600](../crates/magi-app-server-protocol/src/generated.rs#L1600) |
| 现状·已运行验证 | 资源与设置 | `resource.read/write`、`settings.read/write`，统一 CAS、schema 校验与事件发布；[lifecycle.rs:266](../crates/magi-plugin-system/src/lifecycle.rs#L266) |
| 现状·已运行验证 | 业务服务 | `model.invoke`、`task.spawn`、`tools.call`、`notify.user`、`ask.user`、`session.snapshot`、`credential.ref`、`blob.put/get`；wire 工具服务名为 `tool.call`；[host.mjs:248](../crates/magi-extension-host/runtime/host.mjs#L248)、[原生服务:138](../crates/magi-conversation-runtime/src/task_execution_dispatcher/extension_services.rs#L138) |
| 现状·已运行验证 | 注册与诊断 | `api.on`、`api.tool`、`api.command`、`api.log`，只绑定获批贡献；只读 `api.config/configRevision`；[host.mjs:208](../crates/magi-extension-host/runtime/host.mjs#L208) |
| 现状·已运行验证 | 作业 | daemon 持久计划触发 `job.run`，复用命令处理器、TurnService、Coordinator 和 canonical Turn；[plugin_jobs.rs:112](../crates/magi-api/src/plugin_jobs.rs#L112) |
| 现状·静态核对 | 默认中心 | [plugins.json](../plugin-center/plugins.json)、[catalog.json](../plugin-center/catalog.json) 当前为空；不虚构官方可用插件数量 |
| 现状·已运行验证 | 作者样例 | [示例笔记](../examples/plugins/README.md)使用公开资源接口与 Node 工具，可用打包脚本生成 ZIP；不依赖内置 MCP 身份 |
| 现状·已运行验证 | 原生 MCP | 移除插件激活依赖；无插件时管理面仍返回成功；[state.rs:3758](../crates/magi-api/src/state.rs#L3758)、[mcp_runtime.rs](../crates/magi-api/src/mcp_runtime.rs) |

### 2.2 权限强制点

| 状态 | 权限 | 强制点与边界 |
| --- | --- | --- |
| 现状·已运行验证 | storage | 每次资源／设置访问检查声明、授权、激活和排空；[lifecycle.rs:245](../crates/magi-plugin-system/src/lifecycle.rs#L245) |
| 现状·已运行验证 | tools | 插件工具目录与执行共用准入、inputSchema 和原生审批；[plugin_bridge.rs:245](../crates/magi-daemon/src/daemon/plugin_bridge.rs#L245) |
| 现状·已运行验证 | models | `model.invoke/credential.ref` 按 `default` 或明确 roleId 强制授权；[extension_services.rs:39](../crates/magi-daemon/src/daemon/extension_services.rs#L39) |
| 现状·读码推断 | browser | 清单关联和支持的 surface 策略准入；不代表所有物理 guest 路径已实体验收；[package.rs:389](../crates/magi-plugin-system/src/package.rs#L389) |
| 现状·已运行验证 | extension | 可执行包必须声明 `full-trust` 且获批后才激活；不限制直接 OS 调用；[package.rs:192](../crates/magi-plugin-system/src/package.rs#L192) |
| 现状·已运行验证 | contextRewrite | context／compaction 钩子与 `history` 授权关联；未经授权的改写拒绝；[extension_hooks.rs:136](../crates/magi-conversation-runtime/src/extension_hooks.rs#L136) |
| 现状·已运行验证 | 已删除权限 | filesRead、filesWrite、network 不再作为装饰性权限；旧 scope 参数与未知清单字段明确拒绝；[包回归](../crates/magi-plugin-system/tests/package.rs) |

### 2.3 视图隔离

【现状·读码推断】iframe 仅 `allow-scripts`；响应 CSP 也包含 `sandbox allow-scripts`、`connect-src 'none'`、`frame-ancestors 'self'` 与 nosniff。桥检查 contentWindow、origin、通道和 instanceId；导航重新签发 instanceId，释放旧订阅，丢弃迟到回复。[plugins.rs:166](../crates/magi-api/src/routes/plugins.rs#L166)、[PluginTabContent.svelte:23](../web/src/components/tabs/PluginTabContent.svelte#L23)。实际可调用操作只有资源 read/write/subscribe、设置 read/write；不开放任意 daemon API。

### 2.4 缺陷清单与处理顺序

【目标】P1 为业务／安全合同，P2 为稳定性／可用性，P3 为表达；严重度不代表已经发生攻击。已关闭项保留验证范围，不把平台验收写成已完成。

| 状态／依据 | 编号 | 严重度 | 位置、处理与顺序 |
| --- | --- | --- | --- |
| 现状·已运行验证 | G1 | P1 | 服务原先只能单向调用；现已接入反向服务与签发租约；先做归属再开放业务服务；[instance.rs:430](../crates/magi-extension-host/src/instance.rs#L430) |
| 现状·已运行验证 | G2 | P1 | 视图响应缺 sandbox 已补，路由回归覆盖；导航代次与迟到回复另做静态核对；[plugins.rs:196](../crates/magi-api/src/routes/plugins.rs#L196) |
| 现状·已运行验证 | G3 | P1 | models 原先无服务强制点，现按 default／roleId 拒绝未授权目标；[服务:39](../crates/magi-daemon/src/daemon/extension_services.rs#L39) |
| 现状·已运行验证 | G4 | P2 | enabled_scopes／active_scopes、资源 scope 键、API scope 已清理；全局状态 v2、资源 v3；旧状态版本拒绝，不静默转换；[状态:54](../crates/magi-plugin-system/src/lifecycle.rs#L54)、[资源:6](../crates/magi-plugin-system/src/resource.rs#L6) |
| 现状·已运行验证 | G5 | P1 | 取消、超时、失联时可能写入工具结算为 Indeterminate；不当作确定失败重放；[桥:345](../crates/magi-daemon/src/daemon/plugin_bridge.rs#L345) |
| 现状·已运行验证 | G6 | P1 | 同调用身份并发取消与卡死宿主回收有真实 Node 回归；[host.rs](../crates/magi-extension-host/tests/host.rs) |
| 现状·已运行验证 | G7 | P1 | 停用／升级先关闭准入、quiesce 再提交；真实 daemon 验证在途命令停止和升级重置；[plugins.rs:460](../crates/magi-api/src/routes/plugins.rs#L460) |
| 现状·已运行验证 | G8 | P2 | 长状态路径超过 Unix socket 上限已修，短端点与 0600 回归通过；[transport.rs:10](../crates/magi-extension-host/src/transport.rs#L10) |
| 现状·已运行验证 | G9 | P2 | 作业取消发生于 Turn 接纳前会遗留 pending；现显式结算，并有回归；[plugin_jobs.rs:151](../crates/magi-api/src/plugin_jobs.rs#L151) |
| 现状·已运行验证 | G10 | P1 | 默认凭据引用错误读取全局模型字段；现复用会话／角色模型解析，真实 HTTP 模型调用通过；[原生服务:184](../crates/magi-conversation-runtime/src/task_execution_dispatcher/extension_services.rs#L184) |
| 现状·静态核对 | G11 | P2 | 历史改写提示和计划管理 UI 已接入，展示 daemon 事实、来源、目标会话与预算；[ConversationTurn:120](../web/src/components/ConversationTurn.svelte#L120)、[PluginJobs](../web/src/components/PluginJobs.svelte)；真实布局验收范围见第 12 节 |
| 待定 | G12 | P2 | 官方签名公钥及发布资产由发布者提供；当前空目录不得显示为验签通过；[center.rs](../crates/magi-api/src/routes/plugins/center.rs) |

## 3. 阶段 B：上游对标与自有体系判断

### 3.1 上游机制

| 状态与证据类型 | 维度 | pi | DSH |
| --- | --- | --- | --- |
| 现状†·文档摘要 | 加载 | TypeScript/JavaScript 默认工厂，jiti 加载，支持异步工厂、显式路径及受信项目发现，[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md) | Cordis 插件经 profile、bundle、patch 组合；注入服务，注册是可撤销 effect，[架构](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/architecture.md)、[Cordis](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/cordis-primer.md) |
| 现状†·文档摘要 | 信任 | 扩展与 pi 同进程、同 OS 权限，明确可读凭据和历史；项目本地动态内容先过项目信任，[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md) | 安装的 Host 代码在进程内、工作区沙箱外执行；安装脚本另有批准机制；安全声明不承诺隔离或生产安全，[插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md)、[安全说明](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/SAFETY.md) |
| 现状†·文档摘要 | 事件 | 生命周期、输入、提示、上下文、压缩、模型传输、工具调用/结果；tool_call 改参后明确**不重新校验**；tool_call 异常阻止调用，[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md#tool-events) | Session 持久事件、agent 运行事件、能力事件分域；waterfall 是带 next 的环绕中间件，不等同普通结果折叠，[架构](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/architecture.md)、[Cordis](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/cordis-primer.md) |
| 现状†·文档摘要 | 注册与 UI | 工具、命令、provider、虚拟模型、MCP、渲染器、快捷键、状态与 TUI 自定义组件；RPC 只支持部分 UI，[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md) | 服务、工具、命令、设置、模型、客户端模块；Remote 声明和生成物依赖 Typert Host/Client 组合，不是一个通用可挂载网页协议，[架构](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/architecture.md)、[API Gateway](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/api-gateway.md) |
| 现状†·文档摘要 | 分发 | npm/git/local；宿主提供特定模块实例；官方 [Package Catalog](https://pi.dev/packages) 展示 npm 包，[包文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/packages.md) | npm/pnpm、git、路径与 bundle；有随安装交付的官方按需清单，官方目标随 DSH 版本；第三方可借 [dsh-plugin topic](https://github.com/topics/dsh-plugin) 发现，[插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md)、[README](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/README.md) |
| 现状†·文档摘要 | 稳定性 | 所读扩展/包文档没有独立的扩展 ABI 长期兼容承诺；不能从它推导“永不破坏” | README 明示 developer preview 和破坏性变更；同时已有 engines.dsh、peer 要求及精确版本豁免。存在版本检查不等于长期稳定承诺，[README](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/README.md)、[插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md) |

【现状†·文档摘要】DSH AgentRegistry 与 agent loop 分离，Session 是可扩展事件日志及消息投影的事实源；依赖这些对象、纯消息投影或持久化解释器的插件，不是换几个事件名称即可移植。[Agent](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent/README.md)、[Agent loop](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent-loop/README.md)、[Session](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/subsystems/session.md)。

### 3.2 自成体系是否合理

| 状态 | 角度 | 判断 | 最强反方理由与约束 |
| --- | --- | --- | --- |
| 目标 | 信任模型 | 合理；统一受信 Node 满足直接 OS 能力；纯声明式贡献不启动后台 | 放弃 QuickJS 的受限代码执行选项；完全信任门槛更高。以明确告知和声明式包承载不需后台的内容，不再引入第二种执行器 |
| 目标 | 宿主语言 | Rust 内核与 Node 扩展宿主合理；JS 生态不要求会话核心也使用 JS | Node 分发、进程与 RPC 有维护成本；P1 必须在无开发工具的 macOS/Windows 安装包证明可运行 |
| 目标 | 所有权 | 合理；以提案和类型化动作适配 daemon，避免第二套会话和任务存储 | 接口越窄越难适配上游；必须用真实插件证明充分性，同时明确拒绝依赖 DSH 内核的部分 |

## 4. 阶段 C：战略选项与可用标准

### 4.1 逐项回答

| 状态 | 问题 | 回答、前提与最强反方理由 |
| --- | --- | --- |
| 目标 | 能否直接运行 DSH 插件 | 当前 Node 宿主不能原样运行 DSH 包。Node 只解决语言执行，还须相同 Cordis 服务实例、事件模式、包导出、生命周期、Agent/Session 对象及版本。仅依赖少数服务的插件可在适配宿主执行，但这已是兼容子集。反方：嵌入完整 DSH 更快得到上游行为；代价是第二个内核及事实源，违反 R1 与非目标 |
| 目标 | 工具/命令/设置兼容是否要改内核 | 不必替换内核；公开注册、表单与执行适配足够，但需实现实际 API、schema 和 effect 回收。反方：第三方包可能间接依赖其他服务，不能只按名称认定可翻译 |
| 目标 | 事件拦截兼容是否要改内核 | 需要在 Magi 现有调用链增加公开钩子，不必更换事实源。顺序、阻断、环绕、重入、并发不同必须报告；无法保持必要语义就拒绝。反方：极复杂 waterfall 可能比重用上游难，故只承诺验证过的子集 |
| 目标 | 替换 loop/SessionPersistence 是否要改内核 | 原样支持需要把上游驱动、日志和对象语义带进来，超出边界；只可把业务决策重写为公开钩子／工具／命令提案。反方：失去“一切皆插件”的完整性；这是保留自有内核的明确代价 |
| 目标 | 照 DSH 设计重写能否通用 | 不能保证。设计相似不等于依赖同一份实现、模块实例、异常/取消/微任务、事件对象、持久格式及版本；Rust 重写还需 JS ABI 和行为适配。反方：相同概念能减少映射成本，但不能消除合同验证 |

### 4.2 路径规模、风险与可回退性

【现状·静态核对】本轮主要模块：插件系统顶层 Rust 文件约 2,418 行，扩展宿主 Rust 加 Node 约 3,176 行，API 调度约 308 行，daemon 反向服务约 667 行；含空行和内联测试，不是预计改动量或工期。具体范围见[插件模块](../crates/magi-plugin-system/src)、[宿主模块](../crates/magi-extension-host/src)、[调度](../crates/magi-api/src/plugin_jobs.rs)。

| 状态 | 路径 | 放弃项／风险／回退 |
| --- | --- | --- |
| 目标 | 保留自有体系＋Node | 已确定；修改插件、宿主、API／daemon 桥与原生接线。放弃原包全兼容；可停用扩展恢复原生行为，不能撤销外部副作用。最强反方：多进程、协议和运行时分发需持续维护 |
| 目标 | 只扩受限 SDK | 否决；局部改动较少，但不能满足直接 OS 能力。最强反方：易保留限制；与 R3 冲突 |
| 目标 | 完整嵌入 DSH | 否决；新增完整 Node 内核及会话／权限／恢复桥，涉及存储与调度，不只是宿主。最强反方：重用上游最多；双事实源不可接受，数据进入后难撤回 |
| 目标 | Rust 照 DSH 重写 | 否决；需改核心、事件对象与日志语义，还需 JS 行为适配。概念相同不等于依赖同份代码；最强反方是减少概念映射，但不能保证 ABI／行为通用 |

### 4.3 真正可用的判定

【目标】公开包无需私有桥即可注册工具／命令和关键钩子、直接访问 OS、共享资源和设置；最终改参经过权限重验；角色模型不漂移；用量、取消、失败与恢复归属可查；扩展故障不拖垮 daemon。平台完成声明还必须有干净安装环境证据。翻译兼容是后续独立验收，不能作为本地功能的替代。

## 5. 阶段 D：两层架构与规范合同

### 5.1 两层模型

| 状态 | 层 | 职责 |
| --- | --- | --- |
| 目标 | daemon 业务权威层 | 原生服务、PluginManager、ExtensionBus、协议准入与裁决；声明式包不启动后台 |
| 目标 | Node 扩展执行层 | 每个激活可执行插件一个进程，执行作者代码、直接 OS 调用、绑定处理器、生成提案 |

【目标】Rust 领域模块通过注入端口调用扩展，不能反向依赖 daemon 装配；不得复活 QuickJS 或在 UI／宿主复制业务内核。最强反方：边界让复杂上游对象难适配；以真实公开样例验证接口充分性，超出范围明确拒绝。

### 5.2 所有者表

| 状态 | 对象 | 唯一所有者 |
| --- | --- | --- |
| 目标 | 包、摘要、启用、授权、资源、设置 | daemon PluginManager |
| 目标 | 宿主进程、代次、调用租约、熔断 | daemon 扩展运行时／宿主监管 |
| 目标 | Session、Turn、Task、审批、canonical 日志、用量 | 既有原生领域服务和存储 |
| 目标 | 插件计划 | daemon JobStore；执行只引用 canonical Turn，不复制正文和用量 |
| 目标 | 角色、模型和目录绑定 | 原生配置、Task 策略与目录服务 |
| 目标 | Browser 实体／guest | Browser Authority／Electron Main |
| 目标 | 布局、焦点和局部展示草稿 | 工作台；不得推断业务成功 |
| 目标 | 扩展直接 OS 副作用 | 作者代码；Magi 无法完整审计或撤销 |

### 5.3 不变量

| 状态 | 编号 | 必须保持的合同 |
| --- | --- | --- |
| 目标 | I1 | 扩展不得写 canonical 日志、TaskStore、审批和用量事实 |
| 目标 | I2 | 扩展只能阻止，不能批准、翻转 deny 或代用户回答审批 |
| 目标 | I3 | 最终改写参数必须重新校验 schema、目录、权限、审批和并发类别 |
| 目标 | I4 | 一个事实一个写入口；钩子统一 ExtensionBus |
| 目标 | I5 | 每个权限都有强制点；无法强制的声明必须拒绝或删除 |
| 目标 | I6 | 第一方、第三方和未来适配层使用同一公开 API；MCP 不冒充插件 |
| 目标 | I7 | 安装只校验产物；不得执行安装脚本或在线构建 |
| 目标 | I8 | 调用和实例可失效；迟到响应不得污染下一轮／代次 |
| 目标 | I9 | 超时、崩溃、卡死和队列有界；guard 失败必须阻止 |
| 目标 | I10 | 同一插件只运行一版；不把旧执行交给新版猜测恢复 |
| 目标 | I11 | 撤权优先；未知副作用标记未确定，不自动重放 |
| 目标 | I12 | 先选角色、按原生绑定解析模型；不得按扩展参数直接替换模型 |
| 目标 | I13 | 提示进入统一来源、预算、IR 与缓存谱系；原文保留 |
| 目标 | I14 | 未支持契约明确拒绝，无 SDK 1、默认引擎或第二内核兜底 |

### 5.4 宿主、通信与交付

【现状·读码推断】daemon 直接监管 Node，正式包按固定版本及摘要携带运行时，不读用户 PATH；开发通过 `MAGI_EXTENSION_NODE` 指定。Unix socket 为 0600、目录 0700；长状态路径使用短私有端点。Windows 命名管道拒绝远程客户端，身份仍由随机一次性令牌证明；**未实现或未验证的当前用户专属 DACL 不得宣传为已经存在**。[运行时:30](../crates/magi-daemon/src/daemon/extension_runtime.rs#L30)、[transport.rs:10](../crates/magi-extension-host/src/transport.rs#L10)、[运行时打包](../apps/desktop/scripts/node-runtime.mjs)。

【目标】两平台功能一致，但 IPC 访问控制细节与进程树终止能力按实际实现披露。独立进程用于故障隔离，不抵御同用户全权限恶意代码；包摘要只标识内容，不证明作者可信。最强反方：完全信任门槛高；低信任内容使用无后台的声明式包，不增加伪沙箱。

### 5.5 协议与版本

【现状·读码推断】当前 `sdkVersion=2`、protocol/API 1.0、正整数 dataSchemaVersion；固定 ESM 单文件入口，构建期打包依赖。4 字节大端长度＋JSON-RPC，单帧 4 MiB，每方向挂起 256、发送队列有界。握手、令牌、代次和注册在宿主连接准入检查。[instance.rs:556](../crates/magi-extension-host/src/instance.rs#L556)、[peer.rs:30](../crates/magi-extension-host/src/peer.rs#L30)。

【目标】未知主版本、方法、字段和未接线钩子模式必须拒绝；扩展 API、传输、包格式与数据版本分别演进。不得把尚不存在的 manifestVersion 或协商能力写成当前要求。

### 5.6 钩子目录与调用链

【现状·读码推断】下表为当前已接线模式；声明其他组合在安装时拒绝。[目录:199](../crates/magi-extension-host/src/protocol.rs#L199)。观察无业务裁决；guard 可阻止；advise 可提出合同内改写。工具相关钩子作用于有工具的 Task 管线，不能声称纯 conversation 也会调用工具。

| 状态 | 事件 | 模式／结果 | 已核对落点 | 缓存影响 |
| --- | --- | --- | --- | --- |
| 现状·读码推断 | session.created/deleted | observe | 原生事件 → [project_event](../crates/magi-daemon/src/daemon/extension_bridge.rs#L506) | 无 |
| 现状·读码推断 | turn.accepted/end | observe | 接纳／canonical 终态事件 → 同一转发器 | 无 |
| 现状·已运行验证 | input | guard：继续／阻止；不支持改写 | TurnService 新请求准入 → [InputGuard:79](../crates/magi-daemon/src/daemon/extension_bridge.rs#L79)；幂等重放先返回 | 不改变原文 |
| 现状·已运行验证 | prompt.contribute | advise：stable／volatile 片段 | [extension_hooks:329](../crates/magi-conversation-runtime/src/extension_hooks.rs#L329)、[纯会话:1139](../crates/magi-conversation-runtime/src/session_turn_execution.rs#L1139) | 稳定段冻结，易变段在尾部 |
| 现状·已运行验证 | context.assemble | advise：受控历史投影改写 | [唯一投影入口:136](../crates/magi-conversation-runtime/src/extension_hooks.rs#L136)、[纯会话:901](../crates/magi-conversation-runtime/src/session_turn_execution.rs#L901) | extension_rewrite |
| 现状·读码推断 | model.request/response/resolved、usage.update | observe；不支持参数改写 | [StepModelClient:402](../crates/magi-conversation-runtime/src/model_step.rs#L402) → ExtensionBusHooks | 不改变 wire |
| 现状·已运行验证 | tool.call | guard／advise：阻止或改参 | [桥:378](../crates/magi-daemon/src/daemon/extension_bridge.rs#L378) → prepare_tool_calls → 原生 tool_batch；最终参数先于账本／权限规划 | 形成新执行意图 |
| 现状·已运行验证 | tool.result | guard／advise：模型投影 | [桥:421](../crates/magi-daemon/src/daemon/extension_bridge.rs#L421)，原始回执不变 | 只影响后续模型可见内容 |
| 现状·已运行验证 | compaction.before | advise：取消或质量门禁内摘要 | [ContextAuthority:1011](../crates/magi-conversation-runtime/src/context_authority.rs#L1011) → 同一检查点链 | 原生压缩代际 |
| 现状·已运行验证 | subagent.spawn | guard：阻止／继续；不支持改角色或模型 | [prepare_tool_calls:510](../crates/magi-conversation-runtime/src/extension_hooks.rs#L510) → 原生派发预检 | 按原生角色／任务 |

### 5.7 注册、服务与提案裁决

【现状·读码推断】register.hook/tool/command 只能绑定清单获批项。resource/settings 可在 activate 期间访问；其余业务服务需要仍活动的工具／命令调用租约。Node 使用 AsyncLocalStorage 携带 invocationId，daemon 按连接租约注入 owner，不相信插件自报 `_context`。钩子内启动业务动作当前明确拒绝，避免同一阻塞动作重入。[instance.rs:430](../crates/magi-extension-host/src/instance.rs#L430)。

【现状·已运行验证】model.invoke 经原生 StepModelClient 和用量账本；credential.ref 只返回模型绑定引用，不返回 secret。session.snapshot 仅当前签发会话，最多 10 Turn、每 Turn 20 条文本投影并截断；notify/ask 复用通知与交互事实。task.spawn/tool.call 只在具有原生 Task／租约、策略和工具权限时执行，普通命令没有 Task 会明确拒绝，不伪造任务。[服务:138](../crates/magi-conversation-runtime/src/task_execution_dispatcher/extension_services.rs#L138)。

【现状·读码推断】blob 为 JSON 传递：单项 2 MiB、宿主最多 64 项／16 MiB、300 秒过期、归插件所有；不是持久存储或任意文件读取接口。[blob 服务](../crates/magi-daemon/src/daemon/extension_services.rs#L232)。

### 5.8 超时、失败与取消

【现状·已运行验证】guard 默认 5 秒、上限 30 秒；advise 默认 5 秒、上限 15 秒；observe 队列每插件 1024，溢出报告缺口。连续 5 次失败或 60 秒窗口至少 10 次调用且失败过半熔断，30 秒后单探测。宿主指数退避从 1 秒起、上限 60 秒，10 分钟超过 5 次崩溃停止重启；心跳用于回收 event loop 卡死宿主。[breaker](../crates/magi-extension-host/src/breaker.rs)、[supervisor](../crates/magi-extension-host/src/supervisor.rs)。

【目标】guard 异常、超时、熔断、宿主缺失均阻止；advise 失败保留原生行为并提供诊断；observe 不阻塞主流程。扩展不得自降 guard 模式。取消先使租约失效，发送 host.cancel；只读工具取消为 Cancelled，可能写入且结果不确定为 Indeterminate，不自动重试。直接 OS 副作用无法保证回滚。最强反方：故障规则会阻塞工作；必须展示原因及显式停用入口，不能静默放行。

### 5.9 提示、上下文与缓存

【目标】贡献身份包含插件、贡献 ID、包摘要和内容摘要，不含随机时间；扩展不能伪造 system 规则、删除原生系统段或改 canonical 原文。工具配对与预算由 daemon 保护；历史改写需 contextRewrite 授权，进入唯一投影和实际 IR，再计算指纹／缓存。最强反方：改写会增加缓存成本；以单独授权、来源与实际记录约束。

【现状·静态核对】界面消费 `ModelStep.promptFrame.historyProjection.extensionRewrites`，展示来源和记录数；不把估算显示为实际费用。对话与缓存事实仍由 daemon 返回。[缓存合同](prompt-cache-lineage-architecture.md#扩展改写extension_rewrite)、[ConversationTurn:120](../web/src/components/ConversationTurn.svelte#L120)。

### 5.10 包与信任模型

【现状·读码推断】ZIP 路径、大小写冲突、原生载荷、文件类型和 schema 在安装前校验；16 MiB 压缩／32 MiB 展开／512 条目，后台入口上限以包校验代码为准。作者必须构建期打包依赖；产品不安装 npm 依赖，不运行 install 脚本。[package.rs:10](../crates/magi-plugin-system/src/package.rs#L10)。

【目标】信任告知必须说明：可读写文件和凭据、联网、派生进程；Magi 无法限制所有直接副作用。Magi 的 models/storage/tools 授权只控制公开业务服务，不保证恶意完全受信代码遵守它。

## 6. 阶段 E：翻译器与兼容来源

### 6.0 当前进度与工厂结构（2026-11-03）

【现状·已运行验证】翻译器从“静态兼容分析”起步，入口 `scripts/compat/`：`core/registry.mjs` 是翻译器工厂注册表（`register(kind, factory)`、`create(kind)`、`detect(dir)`；识别不出或并列时明确失败，不猜），每种上游生态只在 `index.mjs` 注册一个工厂；`core/scan.mjs` 用语法树扫描导入、`ctx.*` 调用链与 `inject` 服务，不执行上游代码；`core/report.mjs` 规定每项能力必须落在 mapped／shim／bundled／planned／dropped／unknown 之一，必需能力里出现 planned／dropped／unknown 即阻塞翻译；`dsh/` 是首个翻译器，`dsh/catalog.mjs` 是 DSH→Magi 对照表。运行：`node scripts/compat/cli.mjs inspect <插件目录> --require llm`，测试：`npm run test:compat`。pi 翻译器之后只需新增 `pi/` 并注册工厂；DSH 与 pi 共用的 pi-ai 适配层放在共享层，不在两边各写一份。

【现状·已运行验证】对 dsh-connect-workbuddy 5.18.0（95 个源文件、约 5.7 万行）的分析结论：模型提供者注册（`ctx.llm.registerAdapter`／`registerConfigurableProviders`／`registerModelDiscovery`，依赖 pi-ai 适配层）是核心，对应 Magi **尚未交付**的“插件向主对话追加模型数据源”（planned）；浏览器端 React 界面、HTTP 路由、会话投影、槽位与多语言共 18 项 dropped；工具、命令、`systemPrompt` 可映射但需构建期静态声明（planned）；日志、生命周期、服务查找、设置由兼容层 shim。**尚未实现**：兼容层运行时（Cordis 子集）、构建打包、产物与兼容报告随包、Magi 侧插件模型数据源。


> **状态：推迟（2026-11-02 决定）。** 本节及路线表的 P3（pi）、P4（DSH）是后期工作，当前不实现；先让本地插件系统稳定可用。以下是启动时的设计输入，不是已排期的交付。

### 6.1 适配层加构建期打包

【目标】提供 @magi/compat-pi、@magi/compat-dsh 与离线发布构建工具；重定向宿主导入到适配层，将上游扩展、固定依赖、许可证与适配层一起构建为 Magi 包。适配层只用公开 API；依赖的 Cordis 框架可以随适配层打包，但不得顺带挂载 DSH 会话/loop/审批/持久化服务成为第二个内核。

【目标】构建工具生成逐项 API 清单、依赖图、上游和适配层精确版本、源码及产物摘要、平台要求和兼容报告。发现不支持或无法识别的动态导入/API 直接失败，不生成“成功但少功能”的包。静态分析不能证明任意 JS 的全部运行分支：运行时仍严格拒绝未知 API；不得以静态构建成功冒充兼容证明。未执行到的分支在报告标为未覆盖。

【目标】构建在作者或 Magi 维护的隔离构建环境进行；本机安装只接收产物，不运行 npm/git 构建脚本。Magi 不提供本机一键翻译任意上游源码：翻译只发生在作者或 Magi 维护的构建环境，作者用发布工具生成并分发自己的翻译包。

### 6.2 翻译范围

| 状态 | 来源与 API | 范围 | 原因与必要映射 |
| --- | --- | --- | --- |
| 目标 | pi registerTool/registerCommand、普通 JSON schema | 可翻译 | 经公开注册；schema coercion、错误、并发、流式更新和结构化结果需一致性验证；不声称名称相同即等价 |
| 目标 | pi tool_call/tool_result、输入、生命周期 | 需映射 | pi Turn 是一次请求加工具，更接近 Magi Step；agent run、最终 settled 和 Magi Turn 不一一相等；改参后 Magi 重新校验，失败策略也有差异 |
| 目标 | pi before_agent_start/context/context_with_system、provider 裸 body/header | 需映射／部分不可 | 追加扩展段可映射；移除原生规则、任意重写旧历史或最终传输体与 I3/I13 冲突，必须拒绝所需语义，不用近似替代 |
| 目标 | pi session_before_compact | 需映射 | 摘要可贡献；范围、工具配对、预算、取消和检查点由 Magi 裁决 |
| 目标 | pi appendEntry、branch/tree、sessionManager、边界续跑 | 需映射／部分不可 | 私有数据可进 daemon 命名空间；分支沿原生 Thread/lineage。不能伪造 pi JSONL/任意日志写入，不能把 getEntries 返回为不完整的假日志；精确 tree/context_edit 等未支持就拒绝 |
| 目标 | pi select/confirm/input/notify/status、字符串 widget | 可翻译（服务就绪后） | 映射宿主共享交互和状态；扩展确认不是原生工具批准；无 UI 返回明确不可用，不默认为“批准” |
| 目标 | pi ctx.ui.custom、TUI renderer/editor、终端输入 | 不可翻译 | 终端组件、布局和同步渲染语义不能直接在 iframe/Web 工作台执行；作者可另写 Magi 视图，不算原样翻译 |
| 目标 | pi provider/virtual model、MCP 注册、exec | 需映射 | provider 需先落地公开模型能力；MCP 远端声明可映射，包内进程/原生依赖不支持；exec 可用 Node，但系统命令可用性、Windows 参数与取消需报告，不能偷偷改命令 |
| 目标 | pi 内置工具同名覆盖、project_trust 决策、CLI flags | 不可翻译或重新声明 | 不覆盖 Magi 原生身份/用户授权，不自动信任工作区，不新增 Magi CLI；可由作者改为独立工具、显式配置后重新评估 |
| 目标 | pi 子代理、插件间共享事件 | 需映射 | 走 Magi 子任务服务和显式通道；保留角色模型绑定，不能由 pi.exec 另起一个不受 Magi 管理的 agent 再声称原生代理兼容 |
| 目标 | DSH 工具/命令/设置、独立业务服务 | 可翻译（子集） | 提供 Cordis 服务外观、schema、effect/disposer；核对 inject 的全部传递依赖 |
| 目标 | DSH agent/pre-step、tools/* waterfall、模型选择 | 需映射 | next 环绕、串并行、作用域和时序必须对照；可用 Magi 提案表达的才接纳。模型选择回到原生角色绑定，不照搬 live Agent 对象 |
| 目标 | DSH ctx.agents/inbox、AgentHandle、initiator scope | 需映射／部分不可 | 原生 Task/Thread/Turn 与持有句柄不同；跨进程显式身份不能靠 AsyncLocalStorage 冒充；不能暴露假的可写 Session |
| 目标 | DSH 扩展 SessionEventMap、消息投影解释器、SessionPersistence | 不可翻译 | 绑定 DSH 的日志、重放和格式解释器，违反唯一事实源 |
| 目标 | DSH 替换 AgentFactory/agent-loop/审批或存储提供方 | 不可翻译 | 会引入另一执行与事实路径；业务编排意图可由作者改写成 Magi 工作流，但不是直接兼容 |
| 目标 | DSH 客户端页面、Typert Remote 生成物、模块加载器 | 不可直接翻译 | 依赖 DSH Host/Client 组合、身份解析与应用模块；可显式重做为隔离视图和公开服务 |
| 目标 | 任一来源原生 Node 模块、安装时下载/执行程序 | 不可翻译 | 违反包格式及安装不执行；不能为通过兼容测试而增加静默例外 |

【现状†·文档摘要】上表依据为第 3 节固定版本文档，不是已经实现的适配器。pi 的 input/Turn/工具次序与不重验入参，见[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md)；DSH 对象、事件及生成协议见[Agent](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent/README.md)、[Session](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/subsystems/session.md)、[Gateway](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/api-gateway.md)。

### 6.3 兼容来源开关

| 状态 | 动作 | 规定行为 |
| --- | --- | --- |
| 目标 | 显示入口 | 设置 → 插件 → 兼容来源；“pi 兼容来源”“DSH 兼容来源”各一开关，默认关闭，复用现有设置控件与 [UX/UI 规范](ux-ui-guidelines.md) |
| 目标 | 打开 | 只启用该来源的目录浏览，不安装、不启动 Node、不执行远端代码；不能把开关当批量授权 |
| 目标 | 默认目录 | Magi 维护的已翻译产物目录，条目带固定上游版本、兼容报告与摘要，经统一插件中心分发；界面准确标“来自 pi/DSH（翻译）” |
| 目标 | 上游发现 | pi 官方 gallery/npm 与 DSH 官方包/topic 只能提供候选线索，不能等同 Magi 已验证目录；未翻译条目不显示为可直接运行 |
| 目标 | 逐个安装 | 预览产物与报告 → 完全信任确认（有受信后台时）→ 安装 → 用户启用；预览凭据绑定实际产物和来源 |
| 目标 | 关闭 | 隐藏目录/入口，已装包按普通插件继续管理；可显式选择同时停用，默认不联动删除、不撤销用户数据 |
| 目标 | DSH 状态 | “DSH 兼容来源”开关在首个通过全部必需断言的 DSH 包产出兼容报告之前不出现在界面；达标后与 pi 项同等呈现，不设“实验”中间态。DSH 已有版本准入但仍声明破坏性变更，故以固定版本与报告为准 |

【目标】三条具体交互修正：开关只负责发现；安装页显示该包真实差异与信任；关闭来源和停用插件分开提交。共享设置、表单、状态及右栏组件承载这些交互，不增加自制审批界面。最强反方：相比“一开即用”步骤多；但目录内容可变且插件全权限，批量自动执行无法表达逐包知情信任。

【现状†·文档摘要】不能沿用“pi、DSH 都没有官方目录”的旧结论：pi 的[包文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/packages.md)链接 [pi.dev/packages](https://pi.dev/packages)，DSH [插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md)描述官方按需包。所读资料没有证明第三方全集经过签名审查并保证安全；此项应写“未核实”，不能推导为绝对不存在。

### 6.4 兼容验收与供应链

【目标】每个翻译包记录来源项目、仓库与源码摘要、上游精确版本/提交、适配层版本、构建工具版本、依赖锁摘要、许可证与兼容报告。许可逐包核实；核心项目许可证不能代替第三方扩展分发许可。Magi ID 与作者命名空间绑定，不由上游显示名称决定授权。

【目标】固定一致性测试集覆盖：阻止/改参、异常和取消、并行工具结果顺序、输入来源、最终空闲、流式更新、资源冲突、重启与私有状态、模型归属、UI 缺失、effect 回收、所有不支持 API 的拒绝。能做上游差分的在同一输入下对照；平台差异分别记录。分母为固定样本与必需语义断言，报告“通过/偏离/不支持/未覆盖”，不能只报一个可翻译比例。

【目标】仅当一个包的全部必需行为在两平台通过，且作者/用户确认可接受的显式差异后，才标该固定版本兼容。若必需行为依赖不支持 API，构建即拒绝；不可为求比例忽略该行为。上游或适配层更新生成新产物和新报告，不静默沿用旧兼容结论。

## 7. 沙箱层处置与冻结约束

【现状·静态核对】QuickJS／rquickjs／旧 Worker 已删除；SDK 1 和 backend 包拒绝，不保留兼容解释器。[包回归](../crates/magi-plugin-system/tests/package.rs)。保留安装、资源、schema 校验、隔离 iframe、原生进程组与现有权限基础。原生 Browser Worker、MCP、模型 bridge 不是旧插件 Worker，不因替换删除。

【目标】冻结第二运行时、SDK 1 转换、任意 Node 原生模块、安装时构建、工作流／会话引擎替换、程序化数据转换 VM；不把宿主代发少量白名单冒充全能力插件。

## 8. 界面与数据同步

【现状·读码推断】插件页面沿工作台共享布局和 i18n；设置提供安装预览、授权、运行状态、停用、升级和计划管理。计划显示目标会话、模型调用次数、单次输出和期限，结果取 daemon 回执；关闭视图不结束任务。[PluginDetailPane](../web/src/components/PluginDetailPane.svelte)、[PluginJobs](../web/src/components/PluginJobs.svelte)。

【现状·已运行验证】UI、HTTP、后台共用资源 CAS 与写事件；SSE 原子获取快照和订阅边界，溢出 reset 后重读，撤权结束订阅。设置变化通过 config.changed 更新宿主快照；不建立前端业务持久化权威。[plugins.rs:107](../crates/magi-api/src/routes/plugins.rs#L107)、[extension_runtime:239](../crates/magi-daemon/src/daemon/extension_runtime.rs#L239)。

## 9. 安装、升级与生命周期

### 9.1 统一安装

【现状·已运行验证】本地、地址和中心安装绑定已预览摘要；缺少摘要拒绝，内容变化冲突；下载限制协议、解析地址与重定向并固定连接地址。安装不激活；启用后授权清单全部权限才能激活。[plugins.rs:305](../crates/magi-api/src/routes/plugins.rs#L305)、[download](../crates/magi-api/src/routes/plugins/download.rs)。

### 9.2 状态与收敛

【现状·已运行验证】全局 enabled／grants／active 是唯一生命周期事实；宿主 starting/running/restarting/failed/stopped 为 daemon 运行投影。停用／撤权／升级先关闭准入并 quiesce，进程停止后提交，注册表事件与周期复核收敛；摘要不符宿主不能执行新授权调用。[extension_runtime:274](../crates/magi-daemon/src/daemon/extension_runtime.rs#L274)、[plugins.rs:460](../crates/magi-api/src/routes/plugins.rs#L460)。

### 9.3 数据版本与恢复

【现状·已运行验证】升级保留启用意愿，清空授权与激活。dataSchemaVersion 变化且已有数据时预览 `dataReset`，升级清空该插件资源／设置；不提供迁移 VM。重置日志先于数据变化，安装摘要决定恢复旧数据或完成新版本清理；损坏状态明确报错并保留原文件。[upgrade.rs:7](../crates/magi-plugin-system/src/lifecycle/upgrade.rs#L7)。

【目标】不得因插件升级清空 MCP、原生会话、用户源文件或其他插件数据；不得静默恢复旧代码或把未知外部副作用当成回滚成功。完整断电阶段故障注入范围单列，不以正常升级测试代替。

### 9.4 插件作业与调度

【现状·已运行验证】计划由 daemon JobStore 持久化，绑定插件摘要、公开命令、普通用户会话、时间、重复间隔和预算。提交副作用前持久 pendingRequestId；重启使用同一身份，从 canonical Turn 收口，不重复执行。失败／取消不自动重试；活动会话冲突等待；包摘要变化或停用使计划停止；宿主启动中等待就绪。[jobs.rs:11](../crates/magi-plugin-system/src/jobs.rs#L11)、[plugin_jobs.rs:132](../crates/magi-api/src/plugin_jobs.rs#L132)。

【现状·读码推断】预算为最多 0～100 次 model.invoke、每次 1～32768 输出 Token、1000～120000ms；重复间隔至少一分钟。调用次数原子扣减，输出上限进入真实 Provider 请求，超限明确拒绝；Node 直接外调模型不在 Magi 核算内。计划上限为全局 512／每插件 32；当前不提供计划编辑或历史批量删除。[预算强制](../crates/magi-extension-host/src/instance.rs#L461)。

## 10. 工作流、会话引擎与子代理

【目标】工作流／会话引擎继续原生，不重建旧插件工厂。命令是独立 Conversation Turn，不隐式跑主模型；要调用模型必须显式 model.invoke，带活动归属、models 授权并计入用量。Task 工具或派发走原生 schema／权限／审批／账本，不另建执行入口。

【目标】子代理可由用户自定义角色和模型；扩展的 task.spawn 使用原生 agent_spawn 合同，保留角色、目录、策略与绑定解析。显式模型绑定无效时拒绝，不能回落其他模型。最强反方：无法照搬上游任意 model.select；保留产品特色与唯一配置权威优先。

## 11. 实施路线、退出条件与冻结项

| 状态 | 阶段 | 交付／退出条件 |
| --- | --- | --- |
| 现状·已运行验证 | P0 本地收敛 | 包、权限、全局状态、资源、端点与诊断通过相关回归 |
| 现状·已运行验证 | P1 Node 宿主 | 独立进程、钩子／工具／命令、取消、熔断、崩溃和卡死回收；真实 Node 集成验证 |
| 现状·已运行验证 | P2 原生服务与调度 | 反向服务、模型归属、用量／预算、原生计划、停用升级闭环；真实 daemon 20 项验证 |
| 目标 | 平台交付验收 | Windows demo、实机与正式安装包独立验收；不以本机开发结果代替 |
| 目标（推迟） | P3 pi 翻译器 | 本地系统稳定后另启；固定版本／API 子集／一致性报告，再开放来源开关 |
| 目标（推迟） | P4 DSH 翻译器 | P3 适配基础后推进；完整依赖审计和必需语义报告，不嵌入第二内核 |

【目标】当前不创建 pi／DSH 来源开关、空适配器或兼容声明。原生 MCP 不列为插件。发布签名、公钥托管与官方目录内容须由发布者决定，不把本地开发密钥当官方信任根。

## 12. 验收与证据边界

### 12.1 已运行验证

| 状态 | 入口 | 本轮证据 |
| --- | --- | --- |
| 现状·已运行验证 | `npm run test:local-plugins` | 本机真实 daemon＋Node＋本地 HTTP 模型，20 项通过：预览／授权、input guard、服务、直接 OS 能力、CAS、用户提问、无 Task 服务拒绝、调度、在途停用、重激活、升级重置、卸载、内置 MCP 管理面 |
| 现状·已运行验证 | `npm run test:local-plugins-ui` | 在同一纵向流程追加真实 Electron Chromium：设置页创建计划、展示目标会话／预算／daemon 回执，桌面与 390px 深色视口无横向溢出；21 项总检查通过 |
| 现状·已运行验证 | `MAGI_PLUGIN_TEST_APP=<Magi.app> npm run test:local-plugins` | 2026-11-03：对 `npm run package --workspace @magi/desktop -- --dir` 生成的 macOS arm64 目录包，直接启动包内 daemon，只使用随包 Node（不设 `MAGI_EXTENSION_NODE`、不读 PATH），同一 20 项纵向流程全部通过；未签名、未生成 dmg，未在干净机器上安装 |
| 现状·已运行验证 | `npm run test:electron-plugins` | 同一目录包经 CDP 驱动真实打包 Electron：工作区导航与 MCP 服务 88 项通过（该脚本不含扩展内容；扩展的界面验收见 `test:local-plugins-ui`）。同一脚本某次运行曾在“桌面设置展示 daemon 的连接统计”处等待超时，重跑通过，按时序不稳定记录，未定位根因 |
| 现状·已运行验证 | `cargo test -p magi-extension-host --all-targets` | 15 个单元＋17 个真实 Node 集成通过，含模型预算、独立取消、异常、guard、卡死回收与长路径端点 |
| 现状·已运行验证 | `cargo test -p magi-plugin-system --all-targets` | 包／生命周期／资源／计划持久化及签名目录定向套件通过 |
| 现状·已运行验证 | API／daemon 定向回归 | Turn 接纳前取消计划收口、models 未授权拒绝、命令清单准入通过 |
| 现状·已运行验证 | Rust／Web／Desktop 基础检查 | 相关 crate check；宿主／插件系统／daemon 库 Clippy（含传递依赖）零警告；Web 0 错误 0 警告；Desktop check、protocol:check、i18n／桥接定向检查、格式和 diff 检查通过 |

### 12.2 未运行与不得推导的结论

【现状】没有运行全仓测试、Windows 运行／安装包、干净 macOS 安装包、所有崩溃阶段断电注入、长期负载或真实付费第三方模型。UI 类型／源码检查不等于所有语言、触屏、皮肤和实际布局已验收。不同测试有重叠，不以测试数量推导完成百分比。

【目标】平台发布前仍须核对无开发工具安装、固定 Node、视图直接导航隔离、旧 frame 迟到回复、全部所需 browser surface 物理挂载、停用／撤权／升级竞态、未知副作用不重放、角色模型不漂移及实际模型账本。未覆盖项必须明确保留，不编造成功记录。

## 13. 决策、冲突与局限

### 13.1 已确定决策

| 状态 | 编号 | 决策与最强反方理由 |
| --- | --- | --- |
| 目标 | D1 | 保留自有 daemon，Node 单路径；反方是协议维护成本，以单实现与公开测试控制 |
| 目标 | D2 | SDK 2／单 ESM／构建期依赖；反方是作者打包负担，以可复现打包脚本支持 |
| 目标 | D3 | 每插件独立进程、随包 Node；反方是内存和启动成本，不以共享进程牺牲可终止性 |
| 目标 | D4 | guard 失败即阻止；反方是可用性下降，提供明确原因和停用入口 |
| 目标 | D5 | contextRewrite 单独授权、原文保留、来源与缓存代价可见；反方是缓存成本上升 |
| 目标 | D6 | 模型只经原生角色／会话绑定；反方是上游动态模型 API 不能原样兼容 |
| 目标 | D7 | 不做转换 VM；格式变化显式 dataReset；反方是丢失插件数据，必须在预览提示 |
| 目标 | D8 | 不自动回退旧代码或外部副作用；反方是恢复需显式操作，优先事实一致性 |
| 目标 | D9 | 作业由 daemon 调度并绑定 canonical Turn；反方是 Node 直接外调无法完整核算，必须披露 |
| 目标 | D10 | 兼容目录只收已构建产物，签名与来源分开；反方是维护负担，后期与翻译器一起评审 |
| 目标 | D11 | 不恢复工作流／会话引擎插件；反方是扩展面较窄，关键钩子与工具服务承载业务扩展 |
| 目标 | D12 | 全权限扩展不提供强制 OS 只读保证；要求此保证的环境不得组合启用，不能把工具过滤称作隔离 |
| 目标 | D13 | 低信任内容仅声明式包；反方是轻量后台也需完全信任，避免第二执行器 |

### 13.2 待定问题与推荐默认值

| 状态 | 项目 | 推荐默认值与理由 |
| --- | --- | --- |
| 待定 | 官方目录签名密钥、轮换和发布责任人 | 由发布者提供受控公钥；当前不伪造可信官方内容 |
| 待定 | Windows／正式安装包验收环境 | 后置到具备真实环境；能力合同保留，不把缺环境误报为通过 |
| 待定 | 未来翻译器样本和必需语义分母 | 固定上游提交和真实样本；本地系统稳定后再决定，不先承诺生态兼容比例 |

### 13.3 与现有架构的一致性

【现状·文档摘要】[演进方案第 10 节](magi-agent-runtime-evolution-plan.md#10-插件guiweb-和预留接口的边界)允许插件宿主独立交付，同时要求改参重验、撤销优先和资源结算；本文保持这些约束。[Turn 架构](conversation-response-core-architecture-redesign.md)与[缓存谱系](prompt-cache-lineage-architecture.md)仍为各自唯一权威。旧文档中的插件 Workflow／迁移会话目标不再适用当前产品决定，相关描述应同步收敛，不能据其重新开放功能。

【现状】上游文档沿用 2026-11-02 阅读记录（现状†）；本轮未重抓或运行上游插件。未审查全部依赖／市场条目，不使用星数、包数量或估算比例作为论据。受信代码可读取同用户文件与凭据，公开 API 所有权不是防恶意代码的安全边界。

## 14. 参考资料与读取范围

### 14.1 本地依据

【现状†·文档摘要】读取范围：根 AGENTS、docs/README、作者指南、演进方案第 10 节及冻结说明、Turn/任务架构、缓存谱系、UX/UI 规范；插件系统与运行时源码、插件路由、daemon 桥及工具/模型/上下文/子代理实际调用方。正文逐项链接是对应结论的证据，不把其他计划文档视为交付事实。

【现状†·文档摘要】架构约束入口：[整体架构](README.md)、[演进方案](magi-agent-runtime-evolution-plan.md)、[Turn 与任务](conversation-response-core-architecture-redesign.md)、[缓存谱系](prompt-cache-lineage-architecture.md)、[上下文压力](context-pressure-compaction-architecture.md)、[Browser](browser-runtime-design.md)、[UX/UI](ux-ui-guidelines.md)、[验证入口](validation.md)。未在本文复核的 Browser 物理宿主细节只作约束引用。

### 14.2 外部资料

【现状†·静态核对】查阅日期均为 2026-11-02。先获取用户指定 raw 地址，再核实仓库提交并重取，下载字节相同。pi 原 badlogic/pi-mono 地址重定向到 earendil-works/pi；固定快照为 c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8。DSH 固定快照为 ce7073d51254c26cd63b22a60835b8f948deb447。下列行数是实际下载文本静态统计，不是 API 稳定性证据。

| 状态与证据类型 | 资料 | 读取范围 |
| --- | --- | --- |
| 现状†·文档摘要 | [pi 扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md)；[用户指定 raw 地址](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/extensions.md) | 全部 3,347 行、156,365 UTF-8 字节；分段读至最后 with-deps 示例，包含后半部 tool exposure、嵌套工具、MCP、UI、错误与运行模式；不再沿用“只读前十万字符”的限制 |
| 现状†·文档摘要 | [pi 包文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/packages.md)、[官方展示页](https://pi.dev/packages) | 包文档全部 153 行；展示页核对包目录与 npm 安装说明，未审查全部包、未采用页面统计数字 |
| 现状†·文档摘要 | [DSH README](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/README.md) | 全部 77 行，含 preview、安装、topic 与许可入口 |
| 现状†·文档摘要 | [DSH 架构](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/architecture.md) | 全部 169 行，含 profile、Desktop、事件域、Turn 流、日志与 capability seam |
| 现状†·文档摘要 | [Cordis 入门](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/cordis-primer.md) | 全部 45 行，含 dispatch mode、waterfall、inject/effect 与 loader |
| 现状†·文档摘要 | [DSH Session](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/subsystems/session.md) | 分段全部 1,427 行，含消息投影、append、fork、插件记录与生成 API/事件目录 |
| 现状†·文档摘要 | [DSH API Gateway](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/api-gateway.md) | 全部 172 行，含 Remote、Host/Client 生成、身份解析、取消、源码开发模式与边界 |
| 现状†·文档摘要 | [DSH Agent](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent/README.md) | 全部 219 行，含模型绑定、inbox、scope、工厂与 teardown |
| 现状†·文档摘要 | [DSH Agent loop](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent-loop/README.md) | 全部 230 行；读取交付文本全部内容，其中 Turn/step flow 有一行原文本身含“line truncated to 2000 chars”提示，该省略部分未核实，不作详细实现依据 |
| 现状†·文档摘要 | [DSH 插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md) | 分段全部 207 行，含官方按需目录、npm 源、脚本授权、版本准入/豁免、故障及更新恢复 |
| 现状†·文档摘要 | [DSH 安全说明](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/SAFETY.md) | 全部 27 行；不将上游沙箱或审批表述为插件代码的可靠隔离 |

【目标】不沿用未复查的其他产品比较作为本设计证据。后续引用上游新版本须重新记录读取范围、提交和一致性结果，不能让默认分支更新自动扩大兼容承诺。
