# Magi 插件系统：架构、当前合同与交付边界

【现状】更新：2026-11-09。以本工作区源码和运行证据为准；方案目标不得作为已交付证明。翻译功能的产品定义与后续开发约束以第 6.0 节为准，历史技术判断和阶段记录不作为功能完成或永久拒绝的依据。本文完整替换旧 QuickJS 方案。作者可依赖的接口见[作者指南](plugin-authoring.md)，字节协议见[扩展协议](extension-protocol-v1.md)。

## 1. 结论、约定与需求

### 1.1 结论摘要

- 【目标】保留 Rust daemon 唯一业务内核；插件使用独立、完全受信的 Node 宿主。
- 【现状·已运行验证】宿主反向服务、工具／命令、原生调度、全局状态与资源链路已实现；本机真实 daemon 的 20 项纵向回归及真实 Chromium 界面追加 1 项通过，见第 11、12 节。
- 【现状·读码推断】MCP 完整内置，监听、令牌、审批和审计不依赖插件；不包装为第一方插件。
- 【目标】插件能直接使用文件、网络、进程；daemon 服务负责 Magi 业务所有权，不是 OS 沙箱。
- 【目标】扩展只能阻止或提出改写；最终权限、审批、日志和结果由 daemon 决定。
- 【目标】子代理保留自定义角色与模型绑定；不允许扩展静默替换模型。
- 【现状】已接入 DSH 上游原包安装与运行时兼容层；WorkBuddy 与 dsh-rea 已完成 macOS 目录包在线安装、原生页面和运行期验收，尚未支持的必需契约仍会阻止安装。pi 运行器尚未提供，只能浏览其搜索结果，安装被明确拒绝。
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
| 现状 | 翻译包 | 安装期由翻译器从上游 npm 包生成的标准 Magi 包：上游业务文件逐字节保留，页面代码、语言包、样式、图片与链接原样运行；兼容层只翻译宿主能力与业务运行时接口，不带第二个 agent 内核 |

### 1.3 连续编号需求与非目标

| 状态 | 编号 | 需求 |
| --- | --- | --- |
| 目标 | R1 | daemon 唯一拥有会话、Turn、任务、审批、日志与持久化 |
| 目标 | R2 | macOS／Windows 功能合同一致，平台验收和安全限制分别披露 |
| 目标 | R3 | 扩展可直接使用文件、网络、进程，以及观察、阻止和受控改写 agent 行为 |
| 目标 | R4 | MCP、Browser、Subagent 等原生功能继续内置；只有公开 API 可独立实现的功能才作为插件 |
| 目标 | R5 | 本地 ZIP、地址与插件中心走统一预览、摘要确认、安装和授权 |
| 目标 | R6 | 支持工具、命令、Skill、设置、视图、资源、browser surface 及主对话模型数据源声明 |
| 目标 | R7 | UI 与扩展共享 daemon 资源／设置事实；CAS 冲突明确，不覆盖新值 |
| 目标 | R8 | 工作流与会话内核保持原生；通过公开钩子和命令扩展业务 |
| 目标 | R9 | 启用、就绪、停用、撤权、升级、崩溃、取消均有可观察状态 |
| 目标 | R10 | 翻译是正式产品功能，覆盖上游插件的发现、在线安装、运行、原生页面、更新与恢复；功能支持以完整业务验收确定，技术检查只作为实现证据 |
| 目标 | R11 | 自定义子代理角色和模型绑定继续生效，不按扩展提案静默换模型 |
| 目标 | R12 | 不支持的契约明确拒绝；无影子实现、兼容补丁或静默兜底 |
| 目标 | R13 | 版本、能力范围、拒绝项与验证报告可核对 |
| 目标 | R14 | 作者只使用公开接口开发、打包、诊断和分发，不需要修改 Magi |

【目标】非目标：替换或 Rust 复刻 DSH 内核；任意上游原包直接兼容；扩展写 canonical 事件或批准审批；任意原生二进制／Node 原生模块；安装时运行 npm／git 的安装脚本或第三方构建脚本（翻译器只在受限子进程里执行一次上游入口以记录注册，不运行 postinstall）；恢复 QuickJS、工作流引擎和数据转换通道。

## 2. 阶段 A：现状审计与缺陷收敛

### 2.1 当前贡献与完整后台 API

| 状态／依据 | 项目 | 当前合同与证据 |
| --- | --- | --- |
| 现状·已运行验证 | 包 | SDK 2，固定单入口 `extension/index.mjs`；SDK 1、backend、未知字段与未接线模式拒绝；[package.rs:192](../crates/magi-plugin-system/src/package.rs#L192) |
| 现状·读码推断 | 贡献全集 | skills、tools、commands、views、resources、browserSurfaces、modelSources（须同时声明 extension）；views 可为 settings、home、main、rightPane、homeCard；[生成清单类型:1600](../crates/magi-app-server-protocol/src/generated.rs#L1600) |
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
| 现状·已运行验证 | modelSources | `modelSource.set/clear` 按数据源 ID 逐项检查授权；清单要求每个数据源有同名权限且包必须声明 extension；连接信息只在 daemon 内存，宿主退出即撤销；[服务:43](../crates/magi-daemon/src/daemon/extension_services.rs#L43)、[清单校验:410](../crates/magi-plugin-system/src/package.rs#L410) |
| 现状·已运行验证 | 已删除权限 | filesRead、filesWrite、network 不再作为装饰性权限；旧 scope 参数与未知清单字段明确拒绝；[包回归](../crates/magi-plugin-system/tests/package.rs) |

### 2.3 视图隔离

【现状·读码推断】iframe 使用 `allow-scripts allow-downloads`；响应 CSP 同样包含 `sandbox allow-scripts allow-downloads`、`connect-src 'none'`、`frame-ancestors 'self'` 与 nosniff。桥检查 contentWindow、origin、通道和 instanceId；导航重新签发 instanceId，释放旧订阅，丢弃迟到回复。页面原有的 HTTP(S)/mailto 外链由兼容宿主捕获后交给 Magi 外部链接入口，保留 href、target、rel 和上游链接样式；文档内锚点仍由页面处理。原包生成的 Blob 文件沿浏览器标准下载保存，格式与字节由原包拥有，保存位置与取消由浏览器／系统交互处理；不增加文件写入桥或另一套下载状态。[plugins.rs:166](../crates/magi-api/src/routes/plugins.rs#L166)、[PluginTabContent.svelte:23](../web/src/components/tabs/PluginTabContent.svelte#L23)。实际桥操作包括资源 read/write/subscribe、设置 read/write、受限 route.call、外链、剪贴板和高度上报；不开放任意 daemon API。

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
| 现状·已运行验证 | G11 | P2 | 历史改写提示已接入；设置界面不再提供计划管理表单（原计划 UI 已移除，daemon 的作业接口与调度保持不变）；[ConversationTurn:120](../web/src/components/ConversationTurn.svelte#L120)；界面验收见第 12 节 |
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

【现状·已运行验证】model.invoke 经原生 StepModelClient 和用量账本；credential.ref 只返回模型绑定引用，不返回 secret。session.snapshot 仅当前签发会话，最多 10 Turn、每 Turn 20 条文本投影并截断；notify/ask 复用通知与交互事实。task.spawn/tool.call 只在具有原生 Task／租约、策略和工具权限时执行；插件命令从接纳起就是无隐式模型调用的 Task Turn，复用同一原生准入与结算，不伪造任务。[服务:138](../crates/magi-conversation-runtime/src/task_execution_dispatcher/extension_services.rs#L138)。

【现状·已运行验证】`modelSource.set/clear` 向主对话模型目录整体替换或撤销已授权的数据源，只追加、不作默认选择，不被子代理、`model.invoke`、辅助、识图和生图使用；已选用而不可用的会话明确报错，不改用其他模型。选用后主对话内容会发往插件提供的地址，授权预览必须明示。daemon 单测覆盖未声明、非法输入与替换撤销，真实 daemon 端到端用例未单列。[服务:380](../crates/magi-daemon/src/daemon/extension_services.rs#L380)、[作者指南](plugin-authoring.md#提供主对话模型数据源)。

【现状·读码推断】blob 为 JSON 传递：单项 2 MiB、宿主最多 64 项／16 MiB、300 秒过期、归插件所有；不是持久存储或任意文件读取接口。[blob 服务](../crates/magi-daemon/src/daemon/extension_services.rs#L232)。

### 5.8 超时、失败与取消

【现状·已运行验证】guard 默认 5 秒、上限 30 秒；advise 默认 5 秒、上限 15 秒；observe 队列每插件 1024，溢出报告缺口。连续 5 次失败或 60 秒窗口至少 10 次调用且失败过半熔断，30 秒后单探测。宿主指数退避从 1 秒起、上限 60 秒，10 分钟超过 5 次崩溃停止重启；心跳用于回收 event loop 卡死宿主。[breaker](../crates/magi-extension-host/src/breaker.rs)、[supervisor](../crates/magi-extension-host/src/supervisor.rs)。

【目标】guard 异常、超时、熔断、宿主缺失均阻止；advise 失败保留原生行为并提供诊断；observe 不阻塞主流程。扩展不得自降 guard 模式。取消先使租约失效，发送 host.cancel；只读工具取消为 Cancelled，可能写入且结果不确定为 Indeterminate，不自动重试。直接 OS 副作用无法保证回滚。最强反方：故障规则会阻塞工作；必须展示原因及显式停用入口，不能静默放行。

### 5.9 提示、上下文与缓存

【目标】贡献身份包含插件、贡献 ID、包摘要和内容摘要，不含随机时间；扩展不能伪造 system 规则、删除原生系统段或改 canonical 原文。工具配对与预算由 daemon 保护；历史改写需 contextRewrite 授权，进入唯一投影和实际 IR，再计算指纹／缓存。最强反方：改写会增加缓存成本；以单独授权、来源与实际记录约束。

【现状·静态核对】界面消费 `ModelStep.promptFrame.historyProjection.extensionRewrites`，展示来源和记录数；不把估算显示为实际费用。对话与缓存事实仍由 daemon 返回。[缓存合同](prompt-cache-lineage-architecture.md#扩展改写extension_rewrite)、[ConversationTurn:120](../web/src/components/ConversationTurn.svelte#L120)。

### 5.10 包与信任模型

【现状·实现】ZIP 路径、大小写冲突、原生载荷、文件类型和 schema 在安装前校验；归档最多 16 MiB、展开最多 32 MiB、4096 个条目。浏览器与 Node 的 JavaScript 模块采用同一 16 MiB 单文件合同，其余资源最多 4 MiB；不再仅为一个后台入口设置较大容量。翻译准备只限制临时复制开销，不维护另一份单文件准入；最终包由 daemon 唯一校验。原生作者在构建期打包依赖，DSH 原包依赖沿在线解析并核对完整性，均不执行 install 脚本。[package.rs:10](../crates/magi-plugin-system/src/package.rs#L10)。

【目标】信任告知必须说明：可读写文件和凭据、联网、派生进程；Magi 无法限制所有直接副作用。Magi 的 models/storage/tools 授权只控制公开业务服务，不保证恶意完全受信代码遵守它。modelSources 授权还须告知：用户选用该数据源后，对话历史与工具结果会发送到插件提供的地址。

## 6. 阶段 E：翻译器与上游来源

【目标】翻译是 Magi 的插件产品能力：用户在 Magi 中获取上游插件，并完成插件提供的实际业务。安装封装、业务接口转换和运行宿主是实现手段。用户操作、生命周期、在线更新、失败恢复和业务结果共同决定功能是否交付。下述早期实现与单插件记录仅证明当时的样例路径；实现状态和未完成项分别记录。

### 6.0 翻译功能的产品合同（2026-11-08）

【目标】用户从 Magi 搜索并安装 DSH 插件，保留插件原有页面和操作，在 Magi 中完成其业务，能够在线更新、停用、重新启用及卸载。用户不需要修改插件代码、替换 Magi 文件、手工配置运行时或理解底层接口映射。首批产品范围为 DSH；pi 尚未提供运行功能的事实继续明确展示。

【目标】后续开发以完整功能为单位。每项实现先明确用户任务、输入与结果、操作入口、状态及数据所有者、授权边界、失败后的可执行恢复方式，再修改公共能力及翻译入口。没有 Magi 对应能力时先完成能力设计；现有接口暂未暴露属于待开发项，不能直接归为产品不支持。确实涉及内核所有权冲突时，说明用户任务及冲突，设计由 Magi 原生服务完成该任务的路径，不能用假对象满足调用。

| 状态 | 用户流程 | 完成要求 |
| --- | --- | --- |
| 目标 | 发现与安装 | 搜索结果按匹配度展示真实名称、图标和来源；安装操作位置固定，执行、取消、失败和重试有即时反馈；用户确认后可从实际结果进入插件 |
| 目标 | 页面与业务 | 页面、组件、样式、语言包、图片和链接由原包展示；设置、主页面及运行面板进入 Magi 对应位置。业务转换须保留输入、结果、异步、取消与错误语义，实际业务结果可核验 |
| 目标 | 持续运行 | 配置变更、页面关闭／重开、切换会话、应用重启和后台工作都有明确生命周期；状态由真实事实驱动，迟到结果、旧实例和重复回调不能修改当前状态 |
| 目标 | 在线更新 | 插件及所需运行依赖有受控的在线版本管理；预览说明实际版本和权限变化，更新结果与当前可用版本一致；失败后给出明确的保留状态和恢复操作，无需代码替换 |
| 目标 | 停用与卸载 | 停用、撤权、退出和卸载统一回收运行贡献、订阅、连接及子进程；持久数据的保留或删除沿 Magi 统一合同，重新启用不复用失效实例 |
| 目标 | 故障处理 | 在用户当前操作位置解释失败阶段、已完成的动作和可用恢复操作；重试边界明确，不能重复已经提交的业务副作用，也不能用重新扫描代替恢复 |

【目标】架构职责固定：daemon 拥有安装、版本、授权、配置、持久数据和业务执行事实；翻译入口负责来源获取、依赖解析及可审查的安装准备；运行宿主负责上游业务到 Magi 正式服务的转换和实例资源回收；页面宿主负责原生页面展示及共享交互。新增能力应同时可供其他插件使用，接入现有唯一入口，不以包名、页面 CSS 类名或特定报错添加功能分支。不维护插件改写分叉、第二份状态源、双写链路或只为旧实现保留的兜底。

【目标】验收以固定版本插件的用户任务为单位，至少覆盖成功结果、失败恢复、取消、配置持久化、在线更新和停用／重启；涉及页面时遵循 [UX/UI 规范](ux-ui-guidelines.md)。例如管理插件须完成查询、启停及在线更新，截图插件须完成触发、附件投递及恢复。扫描通过、安装成功、进程运行和页面出现文字各自只证明对应阶段。完整业务覆盖率保留明确分母，未执行与未解决项不得计为通过；产品目标仍为至少 95%，继续争取完整支持。

【现状·源码与定向回归】插件业务指引通过既有 `prompt.contribute` 接纳。DSH 的 `section`、`variable`、`getSectionOrder` 使用真实上游注册表维护本插件的排序、插值、重复注册与 effect；每轮读取后提交一份带来源的易变参考资料，运行中切换指引在下一模型步骤生效，不把首次读取伪装成稳定前缀。daemon 仍拥有实际模型输入、授权、提示身份与接纳预算，插件不能替换原生完整系统提示或关闭原生上下文。Agent 局部作用域、动态上下文及其余整体装配接口仍待接入，必需使用继续拒绝。命名空间设置句柄的 `update` 沿同一资源 CAS 提交，失效句柄不能写入新注册。独立原包后台的 RPC 替身检查不作为原包安装或完整业务通过。

【现状·源码与定向回归】公共提示预算统一为每片段及每阶段合计 128K Unicode 字符，补丁 JSON 传输上限 1 MiB；运行宿主读取 daemon 当次预算，不另维护上限或截断指引。超过预算整体拒绝，长指引继续计入正常模型上下文与压力处理。原生回归覆盖完整长 Unicode 指引、超限、重复身份与总预算；不会按字符串 UTF-8 字节数误判字符预算。该容量属于所有插件的正式提示合同，不按原包名称设置例外。

【现状·源码与定向回归】插件身份与片段身份按各自正式合同接纳：插件安装及提示贡献共用 `magi-plugin-system::valid_plugin_id` 的 96 字符命名空间校验，片段 ID 仍独立限制为 64 字符。目录包验收发现原接纳点误把两者都限为 64 字符，导致正常翻译包的贡献被丢弃；现清除该重复规则，原生回归覆盖 96 字符插件身份，真实长指引断言保持完整原文与来源要求。

【现状·实现与真实 Node 回归】钩子身份用于调用关联与取消，不是工具／命令的业务租约。SDK 保留钩子取消信号，实例级资源、设置及已授权目录读取交给原 daemon 授权入口，不再携带一份从未签发的业务租约；钩子里的模型、工具、任务和会话快照仍明确拒绝。公共钩子分发记录真实失败种类及原因，避免仅因 advise 继续原生行为而掩盖贡献失败。

【目标】当前实现中的公开能力、契约检查和实际运行证据用于支撑产品设计；代码中使用 `compat` 等历史名称不构成架构完成证明，也不应通过批量改名冒充本次产品改造。上游接口和版本声明仍须精确处理，不能因停止采用兼容补丁而移除正常业务所需的转换或授权校验。

【现状·实现与定向回归】原包的额外脚本、样式和图片使用公共路由响应能力，浏览器与 Node 不另存一份资源事实。原处理器每次请求只执行一次；超过单帧容量的响应仅作为当前宿主内的短期传输载荷，按有界块读完并释放，不扩大 4 MiB RPC 帧或重放业务。daemon 核对插件当前授权、包摘要及同一实例后交付最终响应；停用、升级、取消、失效和总容量不足明确结束交付，用户核对当前状态后显式重试。普通请求正文仍使用现有 2 MiB 合同，响应最多 16 MiB、编码最多 24 MiB。传输绑定 SHA-256、块位置与实例代次，宿主最多暂存 8 项／48 MiB，闲置 30 秒过期。脚本资源加载与消息卡片接入分别验收，不能据大响应成功就认定聊天插件完成。

【目标·原生资源加载】用户打开原包页面后，其额外模块、样式和图片应沿浏览器原生加载机制取得真实字节，保留模块相对依赖、查询、MIME、加载与失败事件。页面宿主只把本插件资源地址映射到 daemon 的有版本及实例绑定的资源入口，不改写组件、脚本正文或样式；资源入口复用同一路由执行、授权、取消及大小合同，不设置第二份文件存储、下载缓存或监听端口。实例的公开关联身份与内部授权租约分开，停用、重启、同版重新启用或升级后，旧地址在业务开始前拒绝；局部崩溃代次不能充当全局身份。原生 ESM 需要的匿名 CORS 只用于此入口，不放宽其他 API 的来源校验、凭据或页面 CSP。聊天与输入挂载及其回传另行验收，不能以模块加载成功冒充原包业务完成。

【现状】上述为开发与验收约束，不作为运行能力已交付的证明。已完成和剩余问题以源码及第 11、12 节的实际证据为准。

【现状·已运行验证】用户安装的是上游官方发布的 npm 包本身，不是 Magi 维护的另一份翻译副本。安装在 daemon 内分四步：向 npm registry 取包元数据并下载 tarball；核对 `dist.integrity`（sha512）并安全解包（拒绝路径穿越、符号链接、特殊文件，数量与体积有界，不运行任何脚本）；调用随产品发行的翻译器（随包 Node 加 `compat/cli.mjs prepare`）生成标准 Magi 包；把该包交给与其他来源完全相同的预览、授权和生命周期。翻译是确定的：同一上游版本与同一兼容运行时得到同一摘要，所以预览确认的摘要在安装时重新翻译后核对，服务端不缓存产物，也不维护第二份目录。接口为 `GET /api/plugins/upstream?q=`、`POST /api/plugins/upstream/preview`、`POST /api/plugins/upstream/install`，来源标记为 `upstream`（`生态:包名@精确版本`）。

【现状·实现】来源获取已扩展到公开 GitHub 仓库。`github:owner/repository#reference` 与 GitHub HTTPS 地址经官方 codeload 归档的首个有界 PAX Git 元数据解析实际提交，再取固定 SHA 归档，沿公共有界下载、归档解包和同一翻译入口准备。删除单独的匿名 REST 提交查询，避免其配额成为公开原包安装的前置依赖。引用归档与 SHA 归档根名可能不同，安装准备只使用固定 SHA 的字节；确认安装同时绑定固定提交、原始引用及翻译摘要。提交缺失、重复、无效或与确认不一致均拒绝；PAX 元数据不生成文件，符号链接与特殊文件仍拒绝。原包名称和语义版本来自仓库的 `package.json`，不伪造 npm 发布元数据。仓库必须已携带可运行的 JavaScript 入口，不执行构建、安装脚本或 Git hooks。

【现状·实现】仓库、引用和提交增量保存在 daemon 原 `PluginSource::Upstream` 内，没有第二份安装目录。DSH 管理器、profile 快照及状态观察共用一个来源投影，保留真实包名／版本和 GitHub 来源。原生插件详情提供沿原引用的在线检查，搜索到的 npm 同名包不能改绑更新来源；同一仓库的提交替换仍走原排空、摘要授权和恢复事务。窄屏插件列表与详情共用剩余高度，分别滚动，预览确认和生命周期操作固定在详情底部。当前来源范围为公开 GitHub，其他 Git 服务、私有仓库、子模块与需构建的源码仓库未验收。

【目标】`dsh-connect-workbuddy` 只是第一个验收样本，翻译器内不得出现针对它的分支或补丁；样本之外必须有独立夹具覆盖同一机制。

【现状·已运行验证】翻译器按工厂注册（`scripts/compat/core/registry.mjs`）。共享层负责产物扫描、能力报告、清单生成、可复现打包；生态层（`dsh/`、`pi/`）只提供识别、能力对照表和兼容层。上游的 `./client` 界面脚本原样放入 `ui/`，由随产品预构建的页面运行时提供宿主服务；上游没有界面时，仍由同一页面宿主加载通用设置组件；不再生成独立 HTML 表单或另一份视图桥。

【现状·已运行验证】DSH 兼容层在 Magi 扩展宿主内运行**真实的 Cordis 与 `@deepseek-ai/dsh-llm` 运行时**，不自写 Cordis 子集；共享依赖由 `scripts/compat/package-lock.json` 按精确版本固定，发行时用 `npm ci --omit=dev --include=optional --ignore-scripts` 安装进 `daemon/compat/`。兼容层只补宿主服务（`tools`、`commands`、`settings`、`webServer`、`attachments`），其余行为沿用上游代码，避免仿写与上游漂移。

| 状态 | DSH 服务或 API | 映射 | 约束 |
| --- | --- | --- | --- |
| 现状 | `llm.registerAdapter` / `registerConfigurableProviders` / `registerModelDiscovery` | 经 `api.modelSource.set` 提交主对话数据源 | 适配器的真实上游不必是标准协议：兼容层在宿主内起仅本机监听、带随机 Bearer 的 OpenAI 兼容端点，适配器 `stream()` 输出的 DSH 块在端点转成 OpenAI 流，`apiKey` 为该 Bearer。数据源条目携带上下文窗口、输入模态、推理档位（见 5.7） |
| 现状 | `tools.register` | daemon runtime catalog + `api.tool` | 原包按实际配置注册工具，目录和处理器绑定同一宿主租约；安装期观察只供预览，不生成静态工具清单 |
| 现状 | `commands.register` | daemon runtime catalog + `api.command` | 同上 |
| 现状 | `settings`、`ctx.settings` | `settingsSchema` 与 `api.settings` | Schemastery 在运行期提供 JSON Schema 投影，校验仍调用原包 schema；原生页面与无页面包的设置组件共用 settingsScope 和公开视图桥 |
| 现状 | `webServer.register` | 视图桥操作 | 浏览器端界面调用的 HTTP 路由改为经视图桥的受限操作转发给宿主内的同一处理器；不开放任意 daemon API |
| 现状 | 浏览器端 React 界面（`lib/client.js`） | 原包页面与静态资源直接加载 | 依赖 DSH 客户端槽位的部分明确报告，由兼容层提供最小槽位容器 |
| 现状 | `attachments` | 原生调用租约与独立后台图片服务 | 图片原字节、身份、持久数据和投递归 daemon；普通调用和后台图片授权分别校验 |
| 待开发 | `tokenMeter`、`compaction`、`agents`、`sessionProjections` | 按具体用户任务设计原生服务 | 不制造 DSH 内核对象；扫描与准入区分必需和已证明的可选分支，不能以缺少公开接口作为永久不支持的理由 |
| 现状 | 包内平台辅助程序与 `child_process` | 当前平台载荷保留，进程树受宿主回收 | 不执行安装脚本；保留归档的执行意图，排除其他平台目录，仍拒绝 `.node`／`.wasm`；外部系统程序的可用性按实际机器验证 |

【现状·已运行验证】翻译期执行 `apply` 发生在 Node 权限模型的子进程里：默认拒绝网络、子进程、Worker 与原生插件，文件系统只放行翻译运行时与上游目录，环境变量不继承（凭据不进入被记录的上游代码），60 秒超时。Node 官方文档说明权限模型是防误操作的护栏而不是对抗性沙箱，所以这一步不能被表述为"上游代码被隔离"；运行期的上游代码与其他 Magi 扩展一样以用户权限运行（完全信任），安装预览已按完全信任披露。翻译失败的条件：必需能力里有 `planned`、`dropped`、`unknown`；翻译期记录到的注册与静态扫描不一致；产物超出包限制；上游界面声明了槽位却没有任何 Magi 会渲染的槽位。翻译在 daemon 内串行执行，单次上限 180 秒。

【现状·已运行验证】已有静态分析：`core/registry.mjs` 工厂注册表（识别不出或并列明确失败）、`core/scan.mjs` 语法树扫描、`core/report.mjs` 状态报告（mapped／shim／bundled／planned／dropped／unknown）、`dsh/` 对照表；`node scripts/compat/cli.mjs inspect <目录> --require llm`，`npm run test:compat`。

【现状·实现】npm 版本解析与原包依赖的元数据／归档共用 `core/registry-options.mjs`；只有 pacote HTTP 层执行至多两次读取重试，单次请求 30 秒、退避 250–1000ms，仍受 daemon 原安装阶段总预算及取消约束。TLS 断连、503 等暂态读取由同一层恢复；不重新执行翻译、安装提交或激活。独立 registry 回归实际制造连接断开和归档 503，校验各两次请求后完整性与运行库隔离仍成立。

【现状】工具说明最多 32 KiB，其他贡献说明最多 4096 字节；超限明确失败，不截断原包指引。默认启动只作为安装准入观察，不通过开关变体推导静态目录。原包验收入口与当前业务范围集中维护于第 11、12 节；旧离线构建方案及其记录由 Git 历史追溯。

### 6.1 兼容层与翻译运行时

【现状·已运行验证】兼容层只用公开 API；依赖的 Cordis 框架随翻译运行时发行，但不得顺带挂载 DSH 会话／loop／审批／持久化服务成为第二个内核。发行目录 `daemon/compat/` 由 `apps/desktop/scripts/build-support.mjs` 的 `stageCompatRuntime` 生成，进入发行清单的逐文件哈希，`after-pack` 校验其存在；daemon 只在自身同级 `compat/` 查找，开发时用 `MAGI_EXTENSION_COMPAT` 覆盖。翻译与扩展宿主使用同一份 Node 解析结果；缺少随包 Node 或兼容运行时时，上游条目只可浏览，界面显示原因。

【目标】翻译报告应逐项列出 API 清单、依赖图、上游精确版本、平台要求与降级项。现状报告含上游版本与 integrity、工具／命令／数据源、路由、界面槽位处置、静态分析降级项与被省略的文件；依赖图与平台要求尚未输出。发现不支持或无法识别的动态导入／API 直接失败，不生成"成功但少功能"的包。静态分析不能证明任意 JS 的全部运行分支：运行时仍严格拒绝未知 API；未执行到的分支应标为未覆盖。

【现状】安装封装集中于 `scripts/compat/install/`，仅保留默认启动与槽位观察。工具、命令、技能、模型来源和配置由 `runtime/` 适配器动态发布至 daemon；不穷举开关、不合并静态注册副本。注册设置授权覆盖本插件私有资源，合法命名空间可在真实配置重载后出现，仍逐次由 daemon 校验资源权限和 CAS。

【现状·实现】运行期注册的技能经 `plugin.skills` 协商进入现有贡献目录，说明与正文分别用于用途发现和实际调用；上游正文不改写。输入框复用已有清单读取和斜杠菜单，模型复用原生 `skill_apply`，不新增技能 HTTP 目录、本地技能配置或执行器。配置替换、停用、撤权与旧租约失效沿同一目录撤销；重启由新实例重新提交。当前只接入全局、用户与模型均可调用的文本技能，其他调用策略、资源根和 agent 作用域未实现时明确拒绝，不称为完整 DSH skill 子系统。

【现状·清理】旧 `translate/` 目录已移除，通用归档工具归 `core/pack.mjs`，运行期 schema 投影归 `runtime/settings-schema.mjs`。已删除无调用的静态贡献 ID／超时转换、开关全开与注册并集合并、安装期 schema 私有字段收集、重复页面格式判断，以及独立 HTML 设置页生成器和桥接。后端条件导出和包元数据读取使用同一入口；无原生页面的包复用同一 `settingsScope`、主题、语言和 iframe 生命周期。发行边界拒绝重新携带旧 `translate/` 资产。原包业务代码与页面仍保留；仅需要跨进程真实回执的业务调用进行适配。

【现状·实现】原生页面固定提供随产品发行的公共浏览器模块目录（含官方 UI primitives 和所需语法模块），不再按直接 `require` 的语法出现与否裁剪 SDK；原包可在用户交互后通过注入函数取用相同模块。每份隔离文档从官方 `ICON_SYMBOLS` 安装原始 SVG 图形，主按钮、遮罩、浮层、圆角与动效沿公共主题令牌映射并随 Magi 主题刷新，页面与官方组件都不改写。归档上传的预览、安装和升级按统一 16 MiB 包上限计算 base64 JSON 请求容量，业务层仍核对实际归档大小与摘要，其他接口不放宽默认请求上限。

【现状·实现】次级文字色统一由 Magi 全局 `--foreground-secondary` 定义，设置页和原包页面消费同一令牌，不在翻译器重算。DSH 可用于反色文字的层级色映射到不透明画布；可读正文及页签文字沿共享次级色。已删除 WorkBuddy 专属按钮／页签 CSS 选择器，公共宿主不依据插件类名修正页面。主题来源仍是 Magi 外观服务，原包样式和组件保持原样。成功、警告、错误及信息的 `--*-text` 文字色在同一外观入口按主题表面及着色背景校准，原生设置组件与插件页面接收相同文字色及不透明状态底色；实心状态填色继续使用原有语义，文字校准不改写按钮和徽标的背景；DSH 状态背景、警告标签和官方连接反馈沿公共语义映射，不再使用上游浅色背景兜底。

【现状·实现】公共主题映射集中于 `runtime/client/theme.mjs`，原页面反色文字、官方 SDK 选中标签、Diff 增删背景、空闲状态、代码背景等消费同一套 Magi 语义。发行构建读取实际随包官方 UI 模块的 JS／CSS，核对所有 `--dsw-alias-*` 引用；出现未映射的语义即失败，不改写插件 CSS 或依赖包。该检查只覆盖随包 SDK 的公共 alias 引用，不声称检查了所有第三方私有变量、字体或静态色板。

【现状·实现】插件设置文档的最小视口由宿主可用高度确定，页面上报的正文高度用于扩展超长内容的整页滚动范围。短正文不能把原包的 `fixed` 浮层和 `vh` 限高缩到正文高度；内容增长、收起及窗口缩放保留同一隔离文档与表单状态。该约束实施于公共 `PluginFrame`，不改写原包提示组件或为特定插件覆盖样式。

【目标·验收合同】版本管理插件完成：从原包页面发起更新，禁止重复并发批次；关闭页面保留实际后台任务，重开恢复真实进度；取消及停用结束准备中的安装并保留目标版本与授权；重新启用后只能由显式操作再次更新。版本切换保留停用意愿，归档写入失败恢复旧版本，故障清除后显式重试；配置及最终安装身份跨应用重启保留，不自动重放旧批次。可用主按钮在八种外观的默认与真实悬停状态均可读。安装替换由 daemon 事务拥有，不要求先制造 DSH 的 `already-installed` 拒绝再进入移除式更新。共同用户流程由 `scripts/lib/dsh-upgrader-acceptance.mjs` 验证，社区批次与管理专项共用一份断言；不再各维护一条 Upgrader 业务验收。

【现状·实现】profile 中的安装清单、配置、node_modules 原包读取及其临时备份继续使用 daemon 调用快照。除此之外的报告、导出和缓存是当前插件的私有业务文件，使用其 `extensions/data/<id>` 下的真实 Node 路径；根目录读取将这些真实文件与只读安装投影合并，不生成物理 package.json、cordis 配置或 node_modules 安装副本。原包返回的报告路径可实际读取，文件副作用遵循普通受信 Node 的语义；它们不变成安装、授权或启停的第二份事实源。静态准入只对同模块明确构造 profile patch 路径并调用文件写入的代码申请 lifecycle/background 委托，普通截图或缓存写入不因此获得管理授权。

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
| 现状·已运行验证 | DSH 客户端页面、Typert Remote、模块加载器 | 原包页面运行 | 页面及资产保留原字节，共享页面宿主提供槽位、公开路由和 Remote 服务；异步业务调用按真实回执转换，未知槽位和未接入服务仍报告，不能据页面有内容推导业务完成 |
| 目标 | 任一来源原生 Node 模块、安装时下载/执行程序 | 不可翻译 | 违反包格式及安装不执行；不能为通过兼容测试而增加静默例外 |

【现状†·文档摘要】上表依据为第 3 节固定版本文档，不是已经实现的适配器。pi 的 input/Turn/工具次序与不重验入参，见[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md)；DSH 对象、事件及生成协议见[Agent](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent/README.md)、[Session](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/subsystems/session.md)、[Gateway](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/api-gateway.md)。

### 6.3 上游来源的发现与安装

【现状·已运行验证】设置 → 插件 的搜索框同时检索三处：Magi 插件中心目录、DSH 生态（npm 关键词 `dsh-plugin`）、pi 生态（npm 关键词 `pi-package`）。上游检索需要关键词，空查询不拉取 npm 全量，只对已安装的上游包检查最新版本以提示升级。不设"兼容来源"开关：搜索即发现，没有第二个入口。

| 状态 | 动作 | 规定行为 |
| --- | --- | --- |
| 现状·已运行验证 | 条目标注 | 每个上游条目显示生态（DSH／pi）、npm 发布者，以及发布者是否属于上游官方作用域（`@deepseek-ai/`、`@earendil-works/`）。官方作用域只说明谁发布，不代表适配 Magi 或经 Magi 审核，界面如实写出 |
| 现状·已运行验证 | 逐个安装 | 点击安装先预览：取包、核对 integrity、翻译，展示权限差异（完全信任）、翻译结果（工具／命令／数据源／不显示的界面位置／降级项／未随包文件）与 integrity；用户确认后才安装。安装只接受预览摘要，缺失或不一致明确拒绝（409） |
| 现状·已运行验证 | 升级 | registry 的语义版本更新于已装版本时标"可更新"；升级沿同一预览与确认，走排空升级，授权不继承 |
| 现状·已运行验证 | pi 条目 | 可搜索、可浏览；翻译器未提供前，预览与安装明确拒绝并显示原因，不伪装成功 |
| 现状 | 卸载 | 已装上游包按普通插件管理，沿用现有卸载流程 |
| 目标 | 信任信号 | 现状没有按包的审核或签名；若要把某些包标为"已验证"，需要 Magi 发布者提供可验证的名单与签名，不得用 npm 评分、下载量或关键词当作信任依据 |

【现状·已运行验证】真实 npm registry 上对 `dsh-connect-workbuddy` 的全链路（2026-11-06，隔离 daemon 加发行目录中的翻译运行时）：搜索得到 40 条 DSH 结果并标注发布者；预览约 7.6 秒；无摘要与错误摘要的安装分别被拒绝；非法包名与 pi 安装被拒绝；安装后来源为 `upstream`，激活后宿主 `running`、崩溃 0 次，工具、命令、两个模型数据源与图标齐全；先装 5.18.2 再升级到 5.19.1，状态依次为"可更新"、升级成功、新版本运行。未验证：Windows、干净机器上的发行包安装、pi 生态。打包时曾发现 electron-builder 会丢弃 `extraResources` 里名为 `node_modules` 的目录，已改为与 Lighthouse 运行时相同的单独复制写法，并由 `after-pack` 校验。

【现状†·文档摘要】不能沿用“pi、DSH 都没有官方目录”的旧结论：pi 的[包文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/packages.md)链接 [pi.dev/packages](https://pi.dev/packages)，DSH [插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md)描述官方按需包。所读资料没有证明第三方全集经过签名审查并保证安全；此项应写“未核实”，不能推导为绝对不存在。

### 6.4 兼容验收与供应链

【目标】每个翻译包记录来源项目、仓库与源码摘要、上游精确版本/提交、适配层版本、构建工具版本、依赖锁摘要、许可证与兼容报告。许可逐包核实；核心项目许可证不能代替第三方扩展分发许可。Magi ID 与作者命名空间绑定，不由上游显示名称决定授权。

【目标】固定一致性测试集覆盖：阻止/改参、异常和取消、并行工具结果顺序、输入来源、最终空闲、流式更新、资源冲突、重启与私有状态、模型归属、UI 缺失、effect 回收、所有不支持 API 的拒绝。能做上游差分的在同一输入下对照；平台差异分别记录。分母为固定样本与必需语义断言，报告“通过/偏离/不支持/未覆盖”，不能只报一个可翻译比例。

【目标】仅当一个包的全部必需行为在两平台通过，且作者/用户确认可接受的显式差异后，才标该固定版本兼容。若必需行为依赖不支持 API，构建即拒绝；不可为求比例忽略该行为。上游或适配层更新生成新产物和新报告，不静默沿用旧兼容结论。

## 7. 沙箱层处置与冻结约束

【现状·静态核对】QuickJS／rquickjs／旧 Worker 已删除；SDK 1 和 backend 包拒绝，不保留兼容解释器。[包回归](../crates/magi-plugin-system/tests/package.rs)。保留安装、资源、schema 校验、隔离 iframe、原生进程组与现有权限基础。原生 Browser Worker、MCP、模型 bridge 不是旧插件 Worker，不因替换删除。

【目标】冻结第二运行时、SDK 1 转换、任意 Node 原生模块、安装时构建、工作流／会话引擎替换、程序化数据转换 VM；不把宿主代发少量白名单冒充全能力插件。

## 8. 界面与数据同步

【现状·已运行验证】插件在设置里的形态已定稿，后续不再调整版面；变更须先改本节并经用户确认。

| 范围 | 规定 | 依据 |
| --- | --- | --- |
| 导航 | 设置左侧导航按“已安装”列出声明了 `settings` 视图的插件，项名为插件名称，图标为清单 `icon`，缺省或加载失败时退回通用插件图标；卸载后消失 | [SettingsPanel](../web/src/components/SettingsPanel.svelte) |
| 页头 | 页头只显示插件名称，不显示描述，不拼接视图标题；原生分类的页头与描述保持不变 | 同上 |
| 顶部栏 | 位于内容列顶部：图标、名称、版本、启用开关、卸载；状态只读全局投影，操作沿现有授权／激活／停用／卸载接口，卸载沿用确认对话框 | [PluginPageHeader](../web/src/components/PluginPageHeader.svelte) |
| 关闭 | 关闭开关即停用：设置页保留，只剩顶部栏与停用提示，不加载插件界面；重新启用在“插件”列表详情里完成 | 同上 |
| 版面 | 内容列宽度、左右留白、与页头的间距、圆角、整页滚动全部沿用设置共享框架（`settings.css` 的 `settings-frame`）；插件界面以透明背景嵌入，高度由页面经 `view.resize` 上报，页面自身不产生滚动条也不自带外层留白 | [PluginTabContent](../web/src/components/tabs/PluginTabContent.svelte)、[settings.css](../web/src/styles/settings.css) |
| 令牌 | 字号、字体、间距、圆角、文字层级与主按钮前后景取宿主下发的令牌，不写死数值；交互控件用次级文字色，实心控件用主按钮前景色 | 同上 |
| 图标 | 清单 `icon` 指向包内 `ui/` 下的 PNG、JPEG、GIF、WebP、AVIF、BMP、ICO 或 SVG，不超过 256 KiB，文件内容必须与类型相符；SVG 只作为图片显示并经过 XML 内容准入；读取走 `GET /api/plugins/{id}/icon`，与插件是否激活无关，仅返回声明的那一个文件 | [package.rs](../crates/magi-plugin-system/src/package.rs)、[plugins.rs](../crates/magi-api/src/routes/plugins.rs)、[magi-media](../crates/magi-media/src/lib.rs) |

【现状·已运行验证】2026-11-06：`node scripts/verify-local-plugins.mjs --ui`（隔离 daemon，真实 Electron 窗口）26 项通过，其中界面部分断言：导航、列表、详情头的图片均为已加载；页头同一行含图标、名称与已启用开关且内容列不重复操作栏；页头与内容列左边距同原生分类一致；关闭后保留页面与停用提示且不渲染 iframe；在列表中重新授权激活后页面恢复；390px 视口无横向溢出。包校验的反例（缺文件、伪造内容、SVG 主动内容、超限、越出 ui/）由 `cargo test -p magi-plugin-system --test package` 覆盖。

【现状·读码推断】设置界面不再提供插件计划管理；daemon 的作业接口（`/plugins/{id}/jobs`）与调度保留，只是没有界面入口。

【现状·读码推断】设置分类以 `SETTINGS_TABS` 为唯一清单，顺序为：模型、角色、能力、浏览器、MCP 服务、插件、偏好、外观、安全防护、统计、项目。MCP 服务是独立的系统设置分类，不属于插件，其监听、令牌和审批不经插件宿主；插件分类只管理扩展包。[settings-tabs.ts:24](../web/src/lib/settings-tabs.ts#L24)。

【现状·已运行验证】UI、HTTP、后台共用资源 CAS 与写事件；SSE 原子获取快照和订阅边界，溢出 reset 后重读，撤权结束订阅。设置变化通过 config.changed 更新宿主快照；不建立前端业务持久化权威。[plugins.rs:107](../crates/magi-api/src/routes/plugins.rs#L107)、[extension_runtime:239](../crates/magi-daemon/src/daemon/extension_runtime.rs#L239)。

## 9. 安装、升级与生命周期

### 9.1 统一安装

【现状·已运行验证】本地、地址和中心安装绑定已预览摘要；缺少摘要拒绝，内容变化冲突；下载限制协议、解析地址与重定向并固定连接地址。安装不激活；启用后授权清单全部权限才能激活。[plugins.rs:305](../crates/magi-api/src/routes/plugins.rs#L305)、[download](../crates/magi-api/src/routes/plugins/download.rs)。

### 9.2 状态与收敛

【现状·已运行验证】全局 enabled／grants／active 是唯一生命周期事实；宿主 starting/running/restarting/failed/stopped 为 daemon 运行投影。停用／撤权／升级先关闭准入并 quiesce，进程停止后提交，注册表事件与周期复核收敛；摘要不符宿主不能执行新授权调用。[extension_runtime:274](../crates/magi-daemon/src/daemon/extension_runtime.rs#L274)、[plugins.rs:460](../crates/magi-api/src/routes/plugins.rs#L460)。

【现状·实现】原包页面可发起自身在线更新。准备仍沿唯一安装任务；daemon 在提交前核对原实例的 install／lifecycle／authorize 委托、候选来源、数据重置与全部候选权限，租约核对和关闭准入在同一锁内完成。只允许在原已批权限范围内为新摘要重新授权；新增权限或数据重置转由 Magi 原生在线升级入口确认。旧宿主排空后，任务继续由 daemon 完成版本提交、授权、激活及新版就绪确认，不再要求旧租约存活。旧 RPC／文档结束不代表更新失败，结果以持久安装回执和实际新版为准。提交前失败保留原安装；已提交但授权或启动未完成时保留新版实际状态，重启不重放旧任务。

### 9.3 数据版本与恢复

【现状·实现】管理调用可以显式声明 `changeId`，将移除与后续安装归入同一恢复边界。daemon 在排空后把原安装记录与归档引用原子转为待结算移除；当前目录如实展示目标已移除，其他调用不能占用该目标。准备失败或调用失败时恢复原摘要、授权、启停来源及意愿，无需重新下载旧包；应用重启按同一记录恢复未结算移除。候选版本提交是替换的提交点，之后保留新版本的实际状态，不撤销已执行业务或回放安装。成功的独立卸载在调用结算时释放恢复引用。记录属于 PluginManager 原安装状态，不增加第二份 profile、安装树或按插件名称猜测的事务。

【现状·已运行验证】升级保留启用意愿，清空授权与激活。dataSchemaVersion 变化且已有数据时预览 `dataReset`，升级清空该插件资源／设置；不提供迁移 VM。重置日志先于数据变化，安装摘要决定恢复旧数据或完成新版本清理；损坏状态明确报错并保留原文件。[upgrade.rs:7](../crates/magi-plugin-system/src/lifecycle/upgrade.rs#L7)。

【目标】不得因插件升级清空 MCP、原生会话、用户源文件或其他插件数据；不得静默恢复旧代码或把未知外部副作用当成回滚成功。完整断电阶段故障注入范围单列，不以正常升级测试代替。

### 9.4 插件作业与调度

【现状·已运行验证】计划由 daemon JobStore 持久化，绑定插件摘要、公开命令、普通用户会话、时间、重复间隔和预算。提交副作用前持久 pendingRequestId；重启使用同一身份，从 canonical Turn 收口，不重复执行。失败／取消不自动重试；活动会话冲突等待；包摘要变化或停用使计划停止；宿主启动中等待就绪。[jobs.rs:11](../crates/magi-plugin-system/src/jobs.rs#L11)、[plugin_jobs.rs:132](../crates/magi-api/src/plugin_jobs.rs#L132)。

【现状·读码推断】预算为最多 0～100 次 model.invoke、每次 1～32768 输出 Token、1000～120000ms；重复间隔至少一分钟。调用次数原子扣减，输出上限进入真实 Provider 请求，超限明确拒绝；Node 直接外调模型不在 Magi 核算内。计划上限为全局 512／每插件 32；当前不提供计划编辑或历史批量删除。[预算强制](../crates/magi-extension-host/src/instance.rs#L461)。

## 10. 工作流、会话引擎与子代理

【目标】工作流／会话引擎继续原生，不重建旧插件工厂。插件命令是独立 Task Turn，纯 Conversation 入口不执行插件业务或准备 Git 资源，不隐式跑主模型；要调用模型必须显式 model.invoke，带活动归属、models 授权并计入用量。命令与模型动作共用 TaskStore、调度、目录准备、取消、完成契约、canonical 结果和资源结算。Task 工具或派发走原生 schema／权限／审批／账本，不另建执行入口。命令停止、失败或重启中断后不提供通用模型 Continue，须核对外部结果后明确重新发起命令；正文参数不触发聊天记忆提取或辅助模型。

【目标】子代理可由用户自定义角色和模型；扩展的 task.spawn 使用原生 agent_spawn 合同，保留角色、目录、策略与绑定解析。显式模型绑定无效时拒绝，不能回落其他模型。最强反方：无法照搬上游任意 model.select；保留产品特色与唯一配置权威优先。

## 11. 当前产品链路与退出条件

已完成的阶段计划、逐次调试经过和重复验收记录由 Git 历史追溯；本节只维护当前职责、可复查的证据入口和未完成项。完整产品流程见第 6.0 节，SDK 合同以[作者指南](plugin-authoring.md)和[扩展协议](extension-protocol-v1.md)为准。

### 11.1 当前公共能力

| 用户任务 | 当前实现与所有权 | 验证入口与范围 |
| --- | --- | --- |
| 搜索、安装与在线更新 | npm 原包的获取、完整性、依赖解析与安装准备进入唯一 `PluginOperations`；动态贡献不冻结在安装期；独立 Cordis peer 按完整版本范围在线解析，不替换 Magi 自己的运行库 | `test:compat`、`test:electron-plugin-runtime`；原包搜索、排序、图标和固定操作区见 `test:electron-plugin-interactions` |
| 更新已装插件的运行组件 | 发行构建为转换源码、锁文件和实际页面产物生成统一内容身份；daemon 与已安装归档比较，在原包版本未变化时也提供固定在线更新入口。当前原包及 GitHub 提交沿原来源重新准备、预览摘要并提交，保留设置及启用意愿；新摘要须用户重新授权激活，不继承旧授权，不增加独立组件安装目录 | `test:electron-plugin-components`：旧目录包在线安装 Health 3.0.5 后，由新目录包接续同一安装状态；本轮结果见第 12 节 |
| 保存插件生成的文件 | 所有插件视图共用浏览器标准下载，iframe 与响应 CSP 同时开放 `allow-downloads`；原包生成 Blob 和文件名，浏览器／系统拥有保存位置和取消。保持原页面、组件及导出字节，不另建文件写入桥或下载状态源 | `test:electron-dsh-management` 的 Share SVG／PNG 保存、取消后重试与二维码识别；`test:electron-dsh-health` 的原页面 JSON 导出，结果见第 12 节 |
| 通过第三方管理页面安装 | `plugins.installBundle/cancelInstall` 把解析、准备、提交与请求的启停纳入同一个 daemon 任务；取消只影响自身未提交的准备，进入提交后明确返回 too-late；去重绑定 owner、摘要、实例和完整输入 | `test:electron-dsh-management`；Share／Upgrader 原包、取消与显式重试，证据 `target/plugin-acceptance/dsh-management/` |
| 重复安装与失败恢复 | 同摘要不排空正在运行的实例；真实版本替换先排空再提交，失败保留旧摘要、授权和启用状态，并恢复旧宿主；不能把已安装但启用失败表述为运行成功 | Upgrader 原包的在线替换、版本不存在与候选 ZIP 写入失败，证据 `target/plugin-acceptance/upgrade-recovery/`、`installations-upgrader/` |
| 管理目录与原包辅助进程 | 当前 Magi 数据目录的原包主宿主与 Node 辅助脚本读取同一 daemon profile 快照；清单、启停及包管理沿唯一提交入口。当前 profile 的 npm／pnpm 单包 install／add／uninstall／remove 也转换为同一安装及移除任务，支持明确目录及其物理别名，拒绝无目标或未提供对应能力的参数；外部目录仍由真实包管理程序拥有。延迟页面操作的包管理在安装准备时声明独立委托，普通子进程不因此获得管理授权。直接管理与 CLI 委托共用修改边界：同调用串行、先结算有效配置提案，再执行管理并重读目录；无效提案在管理副作用前拒绝。显式实例后台管理可在 HTTP 202 后重新取得独立快照；普通调用保留取消边界，辅助进程退出撤销自身委托。快照绑定原实例，目录读取后及配置提交前核对实例与取消状态；管理命令自身终止还会在排队／目标读取后阻止新业务，父请求和显式重试保持可用。重读失败保留实际回执并要求检查状态，不自动重放。外部 `DSH_HOME` 保持独立；没有物理安装树、锁文件和第二套持久 profile | `test:compat` 的修改后读取、并发、取消、实例替换、提案校验及未知回执回归；`test:electron-plugin-runtime` 的原包 npm 安装／失败／卸载与 pnpm 在线更新，真实 Mgr 及其他管理原包结果见第 12 节；外部实例的既有证据不代表本轮全部复跑 |
| 从原页面更新自身 | daemon 接管已准备的安装任务，在原授权范围内完成排空、替换及新版启动；摘要或代次变化后共享页面宿主更换文档。旧实例结束不取消已开始提交的任务，不允许旧租约操作新版 | `test:electron-dsh-health` 的原包页面 Health 3.0.5 → 在线当前版本、新文档、持久回执、设置及重启；Rust 回归核对旧租约、新权限、撤权与归档写入失败 |
| 应用插件目录 | `hmr.runExclusive` 只串行当前实例操作，拒绝嵌套；`reconcileProfile` 提交本次启停意图后等待 daemon 原监管器就绪，不改写授权，不启动停用项，不重启未变化实例 | Market 原页面重载并在新文档继续搜索，证据 `target/plugin-acceptance/market-reload-business/`；不提供文件监听或 Node 模块缓存替换 |
| 查看健康与运行状态 | 目录订阅只发失效通知，消费者重读 daemon；原包元数据来自不可变归档，生命周期和失败原因来自真实监管状态；观察其他插件失败不处置观察者自身 | Health／Mgr 原包及 `test:electron-dsh-profile`；证据 `target/plugin-acceptance/profile-reload-final/` |
| 按诊断停用并恢复插件 | daemon 将停用来源与原因保存在原安装记录，排空后原子提交启停状态；恢复核对原包摘要及当前停用来源，用户显式停用、启用和版本替换会清除来源。上游 profile 的说明仅投影该事实，备份／复制仍属于调用提案，不保留可写安装树；报告和修复隔离文件保存在插件私有目录，真实文件与投影使用相同的系统路径身份 | `test:compat`、插件 API 的来源持久化和过期恢复回归；`test:electron-dsh-health` 的原页面修复、停用与重启恢复 |
| 读取已安装原包文件 | 显式安装元数据读取携带 daemon 当前归档路径与摘要；profile 在当前调用内懒读取并核对原包字节，目录、只读句柄和文件时间复用同一快照。未激活包也可读取安装产物，不借用运行目录，不物化第二份安装树；摘要不一致明确拒绝。目录只投影真实插件及原生 ID，已删除虚构的 `cordis:include` 根和 ID 前缀；同步错误读取与异步状态观察共用同一监管原因 | `test:compat` 的真实归档、二进制、只读与摘要冲突回归；Debugging 原包业务验收结果单列 |
| 管理版本豁免 | daemon 资源 CAS 保存精确包版本与 runtimeId，独立 compatibility 授权及风险确认；豁免不跳过能力、摘要或权限准入，撤销后旧确认不可继续提交 | `test:electron-dsh-compatibility`，Ops 原包扫描、豁免、在线安装、卸载和重启恢复；证据 `target/plugin-acceptance/dsh-compatibility/` |
| 使用原包页面 | 设置、主页面和右栏共用隔离页面宿主；后台就绪后挂载，摘要或代次变化释放旧文档与桥，其他插件变化保留当前文档；页面、资源、SVG 和外链保持上游表达 | `test:electron-dsh-rea`、`test:electron-plugin-runtime`；REA 原页面 Ghidra 链接、设置 CAS、207 个 MCP 工具和 390px 视口 |
| 在会话目录执行业务 | 命令与工具共用 daemon 的实际执行上下文；命令先完成原生资源准备，再调用原包。DSH 仅将本次会话身份与执行根投影为业务读取所需的 `agent.id`、`agent.session.id/header.cwd`；不创建上游内核对象、历史或写入路径，其他成员明确拒绝。投影在调用结束／取消后失效，并发调用相互隔离 | 定向运行时回归覆盖并发、配置替换、取消及跨调用拒绝；Rust 覆盖隔离路径、源目录不可回落及真实 Node 命令上下文。桌面与真实原包业务的本轮结果见第 12 节 |
| 持续接收页面业务 | 共享 EventSource 保留命名事件、UTF-8／CRLF、重连和取消；`contributions.client` 另需 persistent 授权，每窗口每个激活插件一个隔离文档，视图切换复用 | `test:electron-plugin-runtime`；多右栏页签、SSE、页面关闭／重开及停用回收 |
| 执行与取消页面请求 | 普通 HTTP、SSE 控制与插件私有 Typert JSON unary 路由共用调用入口，按独立授权读取 daemon 的会话与安装目录快照，回执前结算注册设置写入。原页面 fetch 的 `init.signal`／`Request.signal` 及 Remote 第四参数沿同一视图桥取消各自后台调用；文档回收结束等待，迟到回执不能覆盖取消，不撤销已提交业务 | 定向合同覆盖 Remote 的当前快照、未 await 的 CAS 写入、失败回执、预先取消及监听释放；真实目录包验证并发取消、显式重试和页面关闭／重开，完整复跑结果见第 12 节。Remote 流、Host 对象查找与原生会话上下文仍未提供 |
| 页面上传与业务文件返回 | Blob、File、FormData 和 Request 正文沿原视图桥传原字节，Node／Fetch 路由共用载荷合同；图片、归档与应用响应头进入同一 Fetch Response。daemon 在业务前校验字节编码及容量，读流响应取消；不新增文件执行器或安装副本 | 机制回归覆盖二进制、multipart、原 PNG、下载文件名、2 MiB 边界、响应头、伪造／冲突及取消；目录发行包结果按本轮证据单列，不计为社区插件核心业务通过 |
| 截图与附件 | 后台图片存储有独立实例授权、分块传输和持久元数据；输入端口等待真实草稿回执，原同步业务调用仅在可证明的 AST 路径上改为等待；图片发送仍由用户决定 | `test:electron-appshot`：0.4.2 原包真实 macOS 截图、关闭设置页后投递、重启后载荷恢复；证据 `target/plugin-acceptance/appshot/`。曾有解码／挂载超时，复跑通过，长期时序稳定性仍待定位 |
| 读取会话目录 | 独立 `sessionRead:["directory"]` 权限，只读 daemon 用户会话元数据与稳定分页；不包含消息、凭据或完整可写 Session，不按上游对象形状伪造事实 | `test:electron-plugin-runtime`、`test:electron-appshot` |
| 发现与调用技能 | 全局文本注册进入已有运行目录，输入框 `/` 与模型 `skill_apply` 共用该事实；原文调用、配置替换、停用撤销和应用重启归原生生命周期；消息保存接纳时名称 | `test:compat`、`test:electron-plugin-runtime`，Debugging 0.7.2 原包技能；其他调用策略、资源根、provider 和 agent 作用域仍未接入 |
| 安装任务中断与回执恢复 | daemon 在同一安装操作中持久记录请求、候选与终态；旧回执跨宿主代次领取，中断只核对当前安装摘要，不重放未知副作用；明确已安装但结算未确认 | `test:electron-plugin-install-recovery`，私有 registry 测试原包与真实目录发行包，解析中、提交后启动等待、终态回执三个断进程位置；不是社区插件业务覆盖率 |
| 将业务结果交给当前会话 | 独立 context／wake 授权，统一 SessionInput、canonical、原生引导及队列；context 不主动启动模型，steer/followup 可唤醒。来源、身份与模型消费回执可审计，子任务及计划预算不能旁路 | 公开合同见[扩展协议](extension-protocol-v1.md#会话消息交付)；`session-messages-product-final/` 与真实 WorkBuddy 自动回注均通过 |
| 交付工具业务结果与结束本轮 | `api.toolResult` 将任意业务输出与失败／完成意图区分；DSH 真实管线的结构化失败、纠正上下文和 `concludeTurn` 进入同一公开合同。上下文交付失败保留已执行结果，不重放业务；完成意图随原 canonical 工具回执恢复，整批未知结果、任务证据、必需工具、Goal 与租约继续由原生执行链裁决 | 运行时回归、真实 Node 宿主及任务恢复回归；目录发行包的成功／失败／取消／原文交付与重启结果见第 12 节 |
| 使用 WorkBuddy 模型与本机 CLI | 原包模型、CLI 及结果回注均沿 daemon 原生服务；CLI 在本次 Task 的真实目录执行，原包可选择仅返回结果或回注后唤醒主模型。来源、模型请求和用量可追踪 | `test:electron-workbuddy`：5.20.32 在线原包、CodeBuddy 2.181.0、国内现有登录态与 hy3，15/15；实际读取文件、自动回注、主模型答复及原页面。证据 `target/plugin-acceptance/session-messages-workbuddy-final/`；未覆盖国际版、视频或外部程序安装管理 |

上述为分项证据，不能把不同批次的通过项拼成一次全部通过。固定社区样本为 Health 3.0.5、Ops 0.20.8、Upgrader 1.8.0、Debugging 0.7.2、Appshot 0.4.2、Share 0.2.5、Mgr 0.2.12、Release Probe 0.0.8 与 Market 1.2.2；WorkBuddy 与 REA 单列。筛选样本不能缩小完整门槛的分母，未验证业务保持 `unverified`。

### 11.2 剩余产品工作

- 完整业务覆盖率仍未达到可证明的 95%／100%。固定社区整批中的 Health 3.0.5 与 Upgrader 1.8.0 已通过各自共同完整业务合同，最近批次 `coreVerified=2/9`；其余样本仍需补齐成功结果、取消、配置持久化、在线更新、失败恢复与停用／重启验收。安装、进程 running、页面有内容和路由 smoke 各自只证明对应阶段。
- 当前固定样本偏重管理与诊断；Release Probe 是无业务处理器的机制包，不能据此证明真实插件业务覆盖。已另取 npm 当次列表的 8 个近期插件进行完整性核对和静态缺口检查，结果见第 12 节；这些未经过安装与真实业务，不计入既有 9 个分母或成功率。凭据引用、Web 搜索／抓取提供方、技能提供方与聊天挂载仍是实质缺口。大型路由响应已接通公共能力及目录包机制验收，不能据此推导相关原包业务通过。
- Mgr 0.2.12 原页面的 Button 类型判断缺陷由上游 0.2.13 修正；本轮通过 Magi 原生在线入口完成 0.2.12 → 0.2.13，再完成原页面更新、取消、卸载与应用重启。最新证据 `viewport-profile-final/`，旧版缺陷证据 `profile-product-current/` 保留；未改写旧包或伪装组件，固定社区批次中的 0.2.12 不因此改计为通过。Mgr 0.2.13 的服务分支先移除再安装，现纳入 daemon 的显式 changeId 恢复边界。准备失败或候选提交前崩溃恢复原安装；候选已提交后保留新版本实际状态。异步跨调用的独立卸载／安装不按时间邻近猜成同一事务，需由发起者显式传递恢复身份。
- Market／Share 的 GitHub 路径已接入并按第 12 节逐项验收；其他 Git 来源及安装提交所有阶段的崩溃恢复仍未完成。安装任务现持久保存请求、提交边界和回执，重启不自动重放，候选摘要由 PluginManager 核对；验收证据见第 12 节。尚不能据部分断进程验证宣称覆盖所有掉电阶段或外部副作用 exactly-once。
- Debugging 的 `dsh_diagnose_plugin_load` 已完成真实工具审批、安装文件核对及运行／停用／故障／恢复验收；另以本机 Harness 0.2.2-rc.1 的真实 CLI 验证独立目录的 `dsh_dump_config` 与 `dsh_check_effect_mode`。Health 已验证原页面临时文件修复、取消、隔离日志及停用／重启恢复。其他诊断工具、宿主重启、其他修复类型及未列为实测通过的原包分支仍待验证，不能据单项通过认定整个插件完成。
- Mgr 0.2.13 的全文展开入口仍是不可聚焦的点击 `span`；当前关闭按钮的焦点与 Enter、此次失败原因在桌面／390px 的全文展开已实测。其他键盘路径及超过原包自身限高的错误仍需处理，不能据此次说明完整可见宣称所有错误页面均已验收。
- 页面运行库封装在安装归档内，已通过运行组件内容身份及同版原包在线更新接通既有安装的更新。运行组件依然由 Magi 发行更新，原包隔离 peer 由在线依赖解析获取；没有独立运行组件发布源。私有／已撤回来源、所有平台及跨多次 Magi 发布的升级组合仍待验收。
- 会话消息已接通当前主会话的工具／命令文本交付，以及 DSH 工具管线的补充上下文与结束本轮；跨会话与后台消息尚无产品入口。session/event、sessionProjections、完整 Agent 对象、provider 技能与作用域等仍按实际能力门槛处理，不能返回假对象或空结果冒充完成。
- Windows、干净系统正式安装包、Android／iOS 真机触摸、屏幕录制权限引导和长期负载仍需各自验收。390px Electron 视口模拟不代表手机真机通过。
- pi 尚无运行器；只允许浏览搜索结果，不创建空适配器或兼容声明。官方签名、公钥托管及目录内容由发布者提供，不把本地开发密钥当官方信任根。

## 12. 验收与证据边界

### 12.1 验证入口与证据

本节每个功能只保留最近一次独立完整运行的证据。此前批次不代表本轮已复跑，不将不同批次的分项合并为一次通过，也不从检查数量推导生态支持率。逐次排错过程及被替代的验收记录由 Git 历史和原证据目录追溯。

本轮在大响应基础上完成原生资源入口，重新构建 macOS arm64 目录包并完整运行公共运行时验收 **160/160**。翻译回归 **131/131**、宿主 **18 单元＋27 集成**、daemon 生命周期回归 **13/13**、daemon 路由定向回归 **1/1**、原生资源 API **1/1**、`cargo check -p magi-daemon`、Web check、协议生成一致性与修改文件格式通过。证据为 `target/plugin-acceptance/native-resource-compat-final.log`、`route-payload-host-final-verified.log`、`native-resource-daemon-final.log`、`native-resource-api-verified.log`、`native-resource-daemon-check.log`；目录包证据见下表。宿主单元中另启动真实 Node，填满 8 项载荷池，核对容量失败、按调用者取消、旧代次读／释放拒绝、其他响应完整交付及业务执行次数；取消在途块读取后释放，迟到分块不影响后续响应。目录包原生资源验收核对 5 MiB 以上 ESM、相对模块查询、CSS、SVG、HTTP 取消、公开实例换代及 6,327,444 字节未修改 GenUI 运行模块注册；聊天槽位及业务回传仍未计入支持率。此前包校验 **25/25**、提示接纳 **8/8** 的证据保留，本次未复跑。全仓测试及此前 Health／社区整批未在新目录包复跑，不合并为本包的全量证明。目录包验收通过 `MAGI_PLUGIN_TEST_APP` 选择发行物，使用独立状态根和随包 daemon／Node；输出目录和可选审计设置见[验证入口](validation.md)。

| 验证范围 | 入口 | 最近证据与边界 |
| --- | --- | --- |
| 此前准入收敛与目录包定向验收 | `test:compat`、标准 macOS 目录包、随包 Node 与真实 Electron | **128/128**，`target/plugin-acceptance/product-admission-compat.log`；目录包及解包自检通过，`product-admission-build.log`。当时目录包独立定向验收 **4/4**，`product-admission-release-final/`：随包 Node 完整输出 Cost Meter 的 **75,265 字节**报告；GenUI 在线预览在提示能力尚未接入时拒绝且无安装产物；Release Probe 在线安装、精确摘要、授权、实际运行及原包页面通过，页面异常 0。该 GenUI 拒绝不代表当前状态，机制包不计入真实业务覆盖率 |
| 此前安装事务与构建检查，本轮未复跑 Rust | `test:compat`、`cargo test -p magi-plugin-system --test lifecycle`、`cargo test -p magi-api --lib routes::plugins::operations`、标准目录包 | **126/126**、**25/25**、**19/19**，`cargo check -p magi-daemon`、修改文件格式与 macOS 目录包解包自检通过；`target/plugin-acceptance/self-update-compat-final.log`、`self-update-lifecycle.log`、`self-update-api-final.log`、`self-update-daemon-final.log`、`self-update-build-final.log`。新增 npm／pnpm 安装、取消与目录边界，原实例结束后的自更新、权限扩张拒绝、撤权、归档失败、相同摘要的租约替换及等待就绪时目标版本被再次替换的回归。公共就绪等待绑定预期摘要，不把另一版本的 running 算成本次成功。未运行全仓测试或 Web check；构建仍输出已有 WebFolderPicker 静态／动态导入混用警告 |
| 本轮 Health 共同完整业务 | `test:electron-dsh-health` 与社区批次共用 `scripts/lib/dsh-health-acceptance.mjs` | macOS arm64 目录包、随包 Node，专项 **33/33**，`target/plugin-acceptance/health-contract-release/`；社区整批独立执行同一合同通过。覆盖原包诊断、导出、临时文件修复预览／取消／隔离、文件变更后旧计划 409 拒绝、停用来源及重启、页面偏好实际持久化及关闭／重开、原页面 3.0.5 → 3.0.7 在线自更新、应用重启、插件自身停用／重新启用及原回执不重放。新实例控制密钥变化，旧密钥 401 拒绝；不用局部崩溃计数推导全局身份。页面异常 0，专项执行的阴影审计均 `cut=0`；非全部修复类型通过 |
| 本轮公共脚本、原生资源、大响应、提示、包管理、主题、页面业务及恢复 | `npm run test:electron-plugin-runtime` | 最新 macOS arm64 目录包、Electron 45、随包 Node，独立完整运行 **160/160**；`target/plugin-acceptance/native-resource-runtime-verified/`，构建及解包自检 `native-resource-release-verified-build.log`。后台和页面脚本均超过 5 MiB，完成真实封装、安装、运行及重启；业务路由完整交付 **5,850,074 字节**脚本，原页面真实执行且处理器仅运行一次；连续十次交付 5 MiB 二进制，累计超过宿主总池仍可继续业务，超过 16 MiB 响应明确整体失败后 PNG／SVG 与后续操作继续通过。原生资源入口真实加载并执行超过 5 MiB 的 ESM，保留相对模块、查询参数、CSS、SVG 和原始 HTTP 取消；同版重新启用后旧 URL 在业务开始前返回 409。未修改的 GenUI 运行模块 **6,327,444 字节**、SHA-256 `c73508cf211535572e26edb7ab98ef6286adacd676880e5d76855a24b2fd3f6a` 通过原生资源入口注册 `dsh-genui-card`，不计聊天槽位业务完成。**85,347 字符、272,415 字节**业务指引完整进入真实模型请求，保留插件身份、参考来源及 user 角色，配置替换后不混入旧指引。npm 在线安装／卸载、pnpm 0.0.7 → 0.0.8 更新、失败保留、同调用文件快照，以及 8 种外观的真实颜色、官方组件、重连、390px 对话框均通过；桌面与 390px 阴影审计均 `cut=0`、`hidden=0`。Remote 取消／迟到回复、工具结果、上下文、消息、技能、Debugging 0.7.2 原包诊断、2 MiB 请求字节、右栏、停用、重启及短／长／收起正文视口通过，Renderer 与 iframe 异常 0。未启用外部 DSH CLI，不代表整个 Debugging 插件或社区完整业务通过 |
| 此前目录修改 Rust 回归，本轮未复跑 | Rust、协议 | 插件领域生命周期 **24/24**、插件管理 API **15/15**、真实 Node 宿主 **25/25**；恢复保留原摘要、授权、停用来源和资源，拒绝过期实例及被篡改归档，新版提交沿原数据重置和授权规则。此前 `cargo check -p magi-daemon` 与协议一致性检查通过；本轮未复跑这些定向检查 |
| 此前工具结果公共检查，未在本轮复跑 | Rust、Web、协议 | 真实 Node 宿主 **25/25**、任务循环 **66/66**、完成结果／恢复定向 **3/3**（与任务循环有重叠），`cargo check -p magi-daemon`、协议一致、修改文件格式通过。Web check 0 错误 0 警告，工具错误与展示回归通过；展示回归仍输出任务外 FileChangeCard／FileSpan 的既有 SSR 嵌套按钮警告。前端删除失败工具重展示时重复写控制台告警的副作用，失败详情仍由原 canonical 展示 |
| 此前在线管理、Upgrader 完整业务及恢复 | `test:electron-dsh-management` | 新 macOS arm64 目录包独立完整运行 **46/46**，`target/plugin-acceptance/sdk-theme-management/`。Share、Upgrader、Market 原包完成 npm／GitHub 安装、更新、授权、失败保留、重启及卸载；Share SVG／PNG 导出、取消后重试与原包扫码核对通过。Upgrader 共同合同覆盖原页面配置、更新、并发拒绝、关闭／重开页面、取消、停用／重新启用、版本切换、归档落盘失败恢复、显式重试与应用重启，不重放旧批次。原包可用主按钮在 8 种外观的默认与真实悬停状态最低约 **7.43:1**；390px GitHub 确认固定可达、无横向溢出且 `cut=0`；页面异常 0。专项通过不替代其他插件完整业务 |
| 此前运行组件在线更新，未在本轮复跑 | `test:electron-plugin-components` | 两版 macOS arm64 目录包接续同一状态根，**17/17**；`target/plugin-acceptance/components/`。旧包在线安装 Health 3.0.5，随后新包完成组件身份发现、390px 固定操作、加载／取消／失败保留、同版预览与摘要切换、重新授权、原页面、设置保留及重启恢复；默认、悬停、预览均 `cut=0`。运行组件随 Magi 发行，不是独立组件发布源 |
| 此前发现与安装交互，未在本轮复跑 | `test:electron-plugin-interactions` | **21/21**；`target/plugin-acceptance/interactions/`。WorkBuddy 在线安装与原包版本更新、REA 精确搜索、真实图标、请求乱序、加载／取消／失败反馈及固定操作区 |
| 此前外部实例诊断，本轮未复跑外部 CLI | `MAGI_DSH_EXTERNAL_CLI=<真实CLI路径> npm run test:electron-plugin-runtime` | 新构建 macOS arm64 目录包 **128/128**；`target/plugin-acceptance/external-dsh-product-final/`。原 125 项页面、2 MiB 字节、multipart、PNG／SVG、8 种外观、右栏、390px、取消、消息、技能及重启均复跑；新增 Debugging 0.7.2 原包经真实模型工具和原生审批调用本机 Harness 0.2.2-rc.1 CLI，对独立目录导出配置、完成 schema 检查、判断配置生效方式，Magi 安装与运行意愿保持不变。公共 canonical 继续隐藏绝对路径，实际参数及原包回执另由原始模型请求核对。外部 CLI 不替代 Magi 内核，不计为整个 Debugging 插件已完成 |
| 此前 Share 原生导出与管理流程，未在本轮复跑 | `test:electron-dsh-management` | macOS arm64 目录包 **33/33**；`target/plugin-acceptance/native-exports-management-final/`。Share 0.2.5 原页面保存 SVG、取消 PNG 后再次保存，落盘 PNG 经原包识别为同一组合码；管理流程覆盖 GitHub／npm 在线安装、升级、来源校验、失败保留、授权、取消、显式重试及应用重启。另实际观察 macOS 保存框并点击取消，回到原页面。默认、悬停、确认与完成状态阴影审计均 `cut=0`。未验证 Windows 或移动设备保存交互 |
| 此前安装中断恢复，未在本轮复跑 | `test:electron-plugin-install-recovery` | 与本轮管理验收相同目录包 **10/10**；`target/plugin-acceptance/removal-install-recovery/`。解析中、版本已提交但 Node 未就绪、终态回执三个断进程位置；同请求不重新下载或替换版本，显式新请求收敛，恢复后真实业务命令返回。私有 registry 仅提供机制原包，未覆盖所有掉电、磁盘和替换阶段 |
| 此前 WorkBuddy 外部服务，未在本轮复跑 | `MAGI_WORKBUDDY_CLI_BINARY=<CLI绝对路径> MAGI_WORKBUDDY_CLI_MODEL=hy3 MAGI_WORKBUDDY_CLI_INJECT_RESULT=1 npm run test:electron-workbuddy` | WorkBuddy 5.20.32 在线原包、CodeBuddy CLI 2.181.0，**15/15**；`target/plugin-acceptance/session-messages-workbuddy-final/`。原页面、CLI 读取验收文件、自动回注 codebuddy-bridge 来源及主模型真实回复；未覆盖国际版、视频或外部程序安装管理 |
| 此前 REA 页面与工具，未在本轮复跑 | `test:electron-dsh-rea` | REA 1.2.0、rea-agents 6.5.0，**16/16**；`target/plugin-acceptance/product-ui-rea/`。SVG、原页面 Ghidra 链接、207 个真实工具、CAS、停用和恢复；早于最后的外观事件分类修正，未验证 Ghidra 实际安装或二进制分析 |
| 此前 Appshot 原生截图及持久化，本轮未复跑 | `MAGI_APPSHOT_LIVE_CAPTURE=1 test:electron-appshot` | 此前视口修复的 macOS arm64 目录包，Appshot 0.4.2 在线原包 **14/14**；`target/plugin-acceptance/viewport-appshot/`。原包配置、原生进程不重复、关闭／重开设置复用客户端、真实 macOS 原生截图经后台事件进入当前输入框、daemon 持久附件原字节核对及应用重启恢复配置／图片均通过。证据 `core=capture-mounted` 仅覆盖截图业务及上述恢复，不替代其他平台、权限引导或完整社区样本门槛 |
| 此前 Ops 兼容豁免与在线原包安装，本轮未复跑 | `test:electron-dsh-compatibility` | 此前视口修复的目录包、Ops 0.20.8 原包 **12/12**；`target/plugin-acceptance/viewport-ops/`。独立 compatibility 授权准入、失败安装撤销刚提交的豁免、daemon 实际豁免登记、真实 Release Probe 在线安装、原页面扫描、撤销豁免、应用重启持久化及卸载通过；不创建第二份物理 profile 或豁免文件，页面异常 0。只覆盖列出的业务，非整个 Ops 工具集通过 |
| 本轮固定社区整批 | `MAGI_DSH_FUNCTIONAL_UPGRADE=1 test:electron-popular-dsh` | 固定 9 个原包在线安装 **9/9**、启动 **9/9**、原页面 **9/9**、路由 smoke **7/7**、页面异常 **0**；Health、Upgrader、Mgr 与 Market 在线更新 **4 项**，`coreVerified=2/9`、`qualified=false`，完整门槛仍退出 1。证据 `target/plugin-acceptance/health-contract-community-verified/`。Health、Upgrader 分别完成与专项共用的完整合同；其余未执行项继续 unverified。Market 原页面搜索及在线安装核对原生预览精确版本与摘要，重载后显示真实结果。registry 留存一次 TLS 建连断开，已有读取重试后成功；不自动重跑整轮业务或拼接专项结果 |
| 此前 Mgr 在线升级、状态主题及原页面流程，本轮未复跑 | `test:electron-dsh-profile` | 此前视口修复的 macOS arm64 目录包，独立完整运行 **54/54**；`target/plugin-acceptance/viewport-profile-final/`。Mgr 0.2.12 经原生在线事务升级为 0.2.13，原页面完成查询、配置恢复、启停、Release Probe 0.0.7 → 0.0.8 在线更新、更新／卸载取消、390px 确认、应用重启及最终卸载；页面异常 0。先移除式更新的解析失败恢复原摘要、授权及实际运行，8 种外观下真实错误与恢复说明的文字对比度最低约 **5.77:1**，状态底色全部不透明；深浅沉浸材质的 390px 错误浮层及关闭操作可达，阴影审计 `cut=0`、`hidden=0`。原包成功更新提示在入场动画结束后核对实际回执、颜色与截图。旧版已移除时终止真实 daemon，翻译工具链退出、重启恢复且不重放安装，再由原页面显式重试成功。新增 1280px 与 390px 原包真实错误全文展开／收起、完整恢复说明与阴影审计；原包关闭按钮取得真实文档焦点后由完整 Enter 按键关闭，恢复版本保持运行。复现前视口被短正文收缩至 240px，扩展后的说明被隐藏；公共最小视口修正后实际视口为 670px／557px，原包组件和样式不改写。证据含 `manager-error-expanded.json` 与焦点记录；不代表不可聚焦的全文展开入口、所有超长错误或所有崩溃提交阶段均已验收 |

本轮失败证据保留：`upgrader-button-before/` 记录旧包深色主题浅底白字，最低约 1.24:1；`sdk-theme-runtime/` 记录初次夹具给静态 Pill 传入未由该组件透传的 data 属性，导致计数失败，后改用真实交互式 Pill 验收，未修改 SDK。`upgrader-business-final-community/` 保留脚本点击加载中禁用搜索按钮而超时的记录；最终批次等待同一原包操作可用后提交，未跳过搜索和安装断言。

本轮自更新原始失败保留于 `profile-package-process-health/`：标准包命令已进入 daemon，但自身替换仍被旧限制拒绝。`self-update-runtime-final/` 保留宽度恢复／窗口失焦后菜单已结束，脚本向不存在的旧项派发 mousedown 的失败。脚本现重取实际焦点、等待当前菜单并发送物理鼠标事件；最终两项独立验收均完整复跑，不过滤页面异常或省略原业务。

验收保持原包组件、异常与成功门槛，不过滤运行错误或整轮自动重试。进程重启前释放旧文档，重新启动后等待工作台初始化；折叠详情需要沿原页面展开后再执行操作。此前失败的脚本和产品记录保留原证据目录，由 Git 历史追溯，不合并不同批次作为一次通过。

本轮视口与验收修正的原始证据保留：`long-error-probe/` 记录短正文视口下全文被裁切；`viewport-profile/` 记录仅发送 keyDown、未核对物理焦点造成的键盘验收失败。`community-current/`、`community-viewport/` 与 `community-viewport-final/` 分别保留 Market 安装旧版假设、把 latest 作为精确版本参数及依赖搜索索引导致的脚本失败；这些未改动原包。最终独立完整批次使用正式预览的省略版本解析语义并核对摘要，仍保留完整业务门槛未通过。

此前恢复试跑的失败证据保留在 `removal-recovery-profile/` 和 `removal-recovery-profile-product/`：前者暴露 daemon 崩溃后旧翻译进程仍可联网重试，后者在恢复成功后未展开原包卡片而等待更新按钮超时。所有者管道回归与真实进程退出检查覆盖前一缺口；后续独立完整运行的结果仅以表中最新证据为准，不代替第 11.2 节剩余产品工作。

本轮另核对 npm `keywords:dsh-plugin` 列表，固定当次发布版本与完整性后静态读取 8 个原包。来源快照、元数据、SHA-512 核对与初次报告位于 `target/plugin-acceptance/popular-source-audit/`。初次检查沿用了旧版仅检查 LLM 的默认范围，不能作为安装准入结论；现已与在线安装统一默认业务范围，并对同一留存原包快照重新检查，证据位于 `target/plugin-acceptance/product-admission-audit/`。未执行上游代码，未安装这些包，下载量只用于选样，不作为业务支持率依据。

| 原包版本 | 本轮发现，均未计为业务通过 |
| --- | --- |
| dshmarket 1.73.16 | `desktopPnpm` 虽位于原包可选桌面分支，仍属未登记契约而阻塞；未把 unknown 改成无条件通过 |
| dsh-pet 0.4.7 | 原包下载超过本轮静态检查 32 MiB 预算，未继续解包；发布元数据 `unpackedSize=78611395`，尚无实际安装结论 |
| dsh-whale-widget 0.4.2 | 必需 `credentials` 尚未接入 |
| @michengai/dsh-skills-manager 1.3.0 | 技能提供方、沙箱策略、会话投影、Agent 生命周期及工具前置事件尚未完整接入 |
| dsh-cost-meter 1.9.60 | 凭据、Web／Typert、流式计量、会话／工具与设置／账号事件等仍阻塞；保留完整报告与全部阻塞项 |
| dsh-free-search 0.9.39 | 本插件片段注册已接通；Web 搜索／抓取提供方、`webStartup`、设置文档事件及其余必需接口仍需补齐 |
| dsh-genui 0.3.3 | 已接通必需的片段注册；未修改的 6,327,444 字节运行模块可经原生资源入口加载并注册组件。`conversation.input.left`／`conversation.chat.node` 仍未接入，聊天渲染槽位及交互回传仍阻塞安装及完整业务，不计为插件业务通过 |
| dsh-image-gen 0.11.1 | 图片限制／校验、凭据解析及变更事件尚未接入 |

检查 Cost Meter 时发现工具链写 stdout 后立即退出，大报告在 65,536 字节处被截断。`cli.mjs` 的 resolve、prepare 和 inspect 共用等待写入完成的输出入口；真实管道回归生成超过 64 KiB 的报告，核对末尾阻塞项和失败退出码。独立检查与安装共用 DSH 翻译器的默认完整业务准入，移除安装入口的第二份范围列表；显式 `--require` 只作分项诊断，文字结论明确区分静态检查与安装／业务验证。回归核对同一必需提示 API 在默认 CLI 和实际安装准备入口均被拒绝，拒绝发生在依赖获取和产物生成之前；分项诊断成功不宣称插件可用。未放宽能力准入或包限制。

此前社区整批因 registry 代理连续 502 而失败的记录保留于 `health-contract-community/`，不计为通过；最新独立完整运行仅以上表 `health-contract-community-verified/` 为准。新包定向验收的首次脚本传入了空 registry 地址，在配置校验阶段失败，记录保留于 `product-admission-release/`；显式使用官方 registry 后完整独立复跑，结果只以上表 `product-admission-release-final/` 为准。

提示贡献改造另使用已核对的 GenUI 0.3.3 原包副本，安装准备保留真实依赖解析与受限启动，失败证据为 `target/plugin-acceptance/prompt-genui-preparation/`。未删改原包页面、降低文件准入或制作替代插件。随后直接运行同一原入口、真实 Cordis 和提示注册表，daemon RPC 使用内存替身，原路由切换 `off`／`mini`／`standard`／`plus` 后分别得到 0／38,475／57,982／73,417 字符，并核对设置句柄实际提交；证据 `prompt-genui-backend/`。它只证明后台接口与原文模式，不能替代目录包安装、页面渲染或交互业务。

公共脚本容量改造后，使用同一来源快照、真实完整性身份与未修改的 GenUI 0.3.3 原包重新准备，6,327,444 字节运行资源的 SHA-256 为 `c73508cf211535572e26edb7ab98ef6286adacd676880e5d76855a24b2fd3f6a`，复制前后字节一致；随后在聊天／输入槽位准入明确拒绝。证据 `target/plugin-acceptance/resource-genui-preparation/source-verified.json`；没有通过补一个空白设置页或伪造挂载结果让安装继续。

容量验收的首次失败保留于 `resource-runtime/`：真实资源已返回 200，但夹具把原业务调用与已插入必要回执等待的正式产物直接比较。后续断言核对交付字节与正式安装产物、未改写的依赖原文和必需的业务转换记录；完整页面动作、在线安装和重启仍全部执行，最终只以 `resource-runtime-final/` 独立批次为准。

提示链路的失败证据保留于 `prompt-runtime/`、`prompt-runtime-final/` 及真实 Node 回归 `prompt-hook-lease-before.log`：前两次目录包运行未能把贡献交付到模型，分别暴露插件／片段身份混用及钩子关联身份被误当业务租约。最终结果只以修正后的独立完整运行 `prompt-lease-runtime/` 为准，没有省略长原文或来源断言。

### 12.2 未运行与不得推导的结论

【现状】没有运行全仓测试、Windows 运行／安装包、干净 macOS 安装包、所有崩溃阶段断电注入、长期负载或其他区域／账号的外部模型服务。UI 类型／源码检查不等于所有语言、触屏、皮肤和实际布局已验收。不同测试有重叠，不以测试数量推导完成百分比。

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
| 现状 | D10 | 不维护第二份翻译产物目录：用户安装上游官方 npm 包，翻译在本机安装期完成并以摘要核对；反方是每次预览仍要读取在线元数据并翻译；同一已验证原包的图标、预览和安装获取由有界进程缓存复用，信任信号（谁发布、是否审核）与摘要核对分开表达，不借 npm 元数据暗示审核 |
| 目标 | D11 | 不恢复工作流／会话引擎插件；反方是扩展面较窄，关键钩子与工具服务承载业务扩展 |
| 目标 | D12 | 全权限扩展不提供强制 OS 只读保证；要求此保证的环境不得组合启用，不能把工具过滤称作隔离 |
| 目标 | D13 | 低信任内容仅声明式包；反方是轻量后台也需完全信任，避免第二执行器 |

### 13.2 待定问题与推荐默认值

| 状态 | 项目 | 推荐默认值与理由 |
| --- | --- | --- |
| 待定 | 官方目录签名密钥、轮换和发布责任人 | 由发布者提供受控公钥；当前不伪造可信官方内容 |
| 待定 | Windows／正式安装包验收环境 | 后置到具备真实环境；能力合同保留，不把缺环境误报为通过 |
| 待定 | 翻译器的必需语义分母与一致性测试集 | 固定社区样本为 9 个，WorkBuddy、REA 及机制夹具单列；安装、启动、页面、核心业务与在线升级分别计数，未验证项不计通过，不以样本安装率代替整个生态的业务支持率 |

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
