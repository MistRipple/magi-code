# Magi 插件系统：架构、当前合同与交付边界

【现状】更新：2026-11-07。以本工作区源码和本轮运行证据为准；方案目标不得作为已交付证明。本文完整替换旧 QuickJS 方案。作者可依赖的接口见[作者指南](plugin-authoring.md)，字节协议见[扩展协议](extension-protocol-v1.md)。

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
| 目标 | R10 | pi／DSH 兼容子集后期推进；来源开关只负责发现，不自动执行 |
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

【现状·读码推断】iframe 仅 `allow-scripts`；响应 CSP 也包含 `sandbox allow-scripts`、`connect-src 'none'`、`frame-ancestors 'self'` 与 nosniff。桥检查 contentWindow、origin、通道和 instanceId；导航重新签发 instanceId，释放旧订阅，丢弃迟到回复。页面原有的 HTTP(S)/mailto 外链由兼容宿主捕获后交给 Magi 外部链接入口，保留 href、target、rel 和上游链接样式；文档内锚点仍由页面处理。[plugins.rs:166](../crates/magi-api/src/routes/plugins.rs#L166)、[PluginTabContent.svelte:23](../web/src/components/tabs/PluginTabContent.svelte#L23)。实际桥操作包括资源 read/write/subscribe、设置 read/write、受限 route.call、外链、剪贴板和高度上报；不开放任意 daemon API。

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

【现状·已运行验证】model.invoke 经原生 StepModelClient 和用量账本；credential.ref 只返回模型绑定引用，不返回 secret。session.snapshot 仅当前签发会话，最多 10 Turn、每 Turn 20 条文本投影并截断；notify/ask 复用通知与交互事实。task.spawn/tool.call 只在具有原生 Task／租约、策略和工具权限时执行，普通命令没有 Task 会明确拒绝，不伪造任务。[服务:138](../crates/magi-conversation-runtime/src/task_execution_dispatcher/extension_services.rs#L138)。

【现状·已运行验证】`modelSource.set/clear` 向主对话模型目录整体替换或撤销已授权的数据源，只追加、不作默认选择，不被子代理、`model.invoke`、辅助、识图和生图使用；已选用而不可用的会话明确报错，不改用其他模型。选用后主对话内容会发往插件提供的地址，授权预览必须明示。daemon 单测覆盖未声明、非法输入与替换撤销，真实 daemon 端到端用例未单列。[服务:380](../crates/magi-daemon/src/daemon/extension_services.rs#L380)、[作者指南](plugin-authoring.md#提供主对话模型数据源)。

【现状·读码推断】blob 为 JSON 传递：单项 2 MiB、宿主最多 64 项／16 MiB、300 秒过期、归插件所有；不是持久存储或任意文件读取接口。[blob 服务](../crates/magi-daemon/src/daemon/extension_services.rs#L232)。

### 5.8 超时、失败与取消

【现状·已运行验证】guard 默认 5 秒、上限 30 秒；advise 默认 5 秒、上限 15 秒；observe 队列每插件 1024，溢出报告缺口。连续 5 次失败或 60 秒窗口至少 10 次调用且失败过半熔断，30 秒后单探测。宿主指数退避从 1 秒起、上限 60 秒，10 分钟超过 5 次崩溃停止重启；心跳用于回收 event loop 卡死宿主。[breaker](../crates/magi-extension-host/src/breaker.rs)、[supervisor](../crates/magi-extension-host/src/supervisor.rs)。

【目标】guard 异常、超时、熔断、宿主缺失均阻止；advise 失败保留原生行为并提供诊断；observe 不阻塞主流程。扩展不得自降 guard 模式。取消先使租约失效，发送 host.cancel；只读工具取消为 Cancelled，可能写入且结果不确定为 Indeterminate，不自动重试。直接 OS 副作用无法保证回滚。最强反方：故障规则会阻塞工作；必须展示原因及显式停用入口，不能静默放行。

### 5.9 提示、上下文与缓存

【目标】贡献身份包含插件、贡献 ID、包摘要和内容摘要，不含随机时间；扩展不能伪造 system 规则、删除原生系统段或改 canonical 原文。工具配对与预算由 daemon 保护；历史改写需 contextRewrite 授权，进入唯一投影和实际 IR，再计算指纹／缓存。最强反方：改写会增加缓存成本；以单独授权、来源与实际记录约束。

【现状·静态核对】界面消费 `ModelStep.promptFrame.historyProjection.extensionRewrites`，展示来源和记录数；不把估算显示为实际费用。对话与缓存事实仍由 daemon 返回。[缓存合同](prompt-cache-lineage-architecture.md#扩展改写extension_rewrite)、[ConversationTurn:120](../web/src/components/ConversationTurn.svelte#L120)。

### 5.10 包与信任模型

【现状·读码推断】ZIP 路径、大小写冲突、原生载荷、文件类型和 schema 在安装前校验；16 MiB 压缩／32 MiB 展开／512 条目，后台入口上限以包校验代码为准。作者必须构建期打包依赖；产品不安装 npm 依赖，不运行 install 脚本。[package.rs:10](../crates/magi-plugin-system/src/package.rs#L10)。

【目标】信任告知必须说明：可读写文件和凭据、联网、派生进程；Magi 无法限制所有直接副作用。Magi 的 models/storage/tools 授权只控制公开业务服务，不保证恶意完全受信代码遵守它。modelSources 授权还须告知：用户选用该数据源后，对话历史与工具结果会发送到插件提供的地址。

## 6. 阶段 E：翻译器与上游来源

【目标】2026-11-06 合同修订：翻译器是随 Magi 发行的**运行时生态兼容层**。用户安装与升级上游原包；原代码在受信 Node 中执行，服务调用、动态注册、事件、配置与客户端宿主接口在运行时适配到 Magi 的公开合同。不维护插件改写版，不以安装时执行一次 `apply` 或配置排列组合证明贡献完整性。以下早期安装期记录与单插件成功记录只能证明当时的样例路径；不得作为完整兼容声明。改造计划与退出条件见 11.1。

### 6.0 DSH 翻译器的设计合同（2026-11-05）

【现状·已运行验证】用户安装的是上游官方发布的 npm 包本身，不是 Magi 维护的另一份翻译副本。安装在 daemon 内分四步：向 npm registry 取包元数据并下载 tarball；核对 `dist.integrity`（sha512）并安全解包（拒绝路径穿越、符号链接、特殊文件，数量与体积有界，不运行任何脚本）；调用随产品发行的翻译器（随包 Node 加 `compat/cli.mjs prepare`）生成标准 Magi 包；把该包交给与其他来源完全相同的预览、授权和生命周期。翻译是确定的：同一上游版本与同一兼容运行时得到同一摘要，所以预览确认的摘要在安装时重新翻译后核对，服务端不缓存产物，也不维护第二份目录。接口为 `GET /api/plugins/upstream?q=`、`POST /api/plugins/upstream/preview`、`POST /api/plugins/upstream/install`，来源标记为 `upstream`（`生态:包名@精确版本`）。

【目标】`dsh-connect-workbuddy` 只是第一个验收样本，翻译器内不得出现针对它的分支或补丁；样本之外必须有独立夹具覆盖同一机制。

【现状·已运行验证】翻译器按工厂注册（`scripts/compat/core/registry.mjs`）。共享层负责产物扫描、能力报告、清单生成、可复现打包；生态层（`dsh/`、`pi/`）只提供识别、能力对照表和兼容层。上游的 `./client` 界面脚本原样放入 `ui/`，由随产品预构建的页面运行时提供宿主服务；上游没有界面才由设置 schema 生成通用页。

【现状·已运行验证】DSH 兼容层在 Magi 扩展宿主内运行**真实的 Cordis 与 `@deepseek-ai/dsh-llm` 运行时**，不自写 Cordis 子集；共享依赖由 `scripts/compat/package-lock.json` 按精确版本固定，发行时用 `npm ci --omit=dev --include=optional --ignore-scripts` 安装进 `daemon/compat/`。兼容层只补宿主服务（`tools`、`commands`、`settings`、`webServer`、`attachments`），其余行为沿用上游代码，避免仿写与上游漂移。

| 状态 | DSH 服务或 API | 映射 | 约束 |
| --- | --- | --- | --- |
| 目标 | `llm.registerAdapter` / `registerConfigurableProviders` / `registerModelDiscovery` | 经 `api.modelSource.set` 提交主对话数据源 | 适配器的真实上游不必是标准协议：兼容层在宿主内起仅本机监听、带随机 Bearer 的 OpenAI 兼容端点，适配器 `stream()` 输出的 DSH 块在端点转成 OpenAI 流，`apiKey` 为该 Bearer。数据源条目携带上下文窗口、输入模态、推理档位（见 5.7） |
| 现状 | `tools.register` | 清单 `contributions.tools` + `api.tool` | 工具必须在翻译期静态声明：翻译器在受限子进程里**实际执行**一次 `apply` 并记录注册的工具名、描述与参数 schema，写入清单；运行期按名字绑定。运行期才出现、翻译期未记录的工具明确拒绝 |
| 目标 | `commands.register` | 清单 `contributions.commands` + `api.command` | 同上 |
| 目标 | `settings`、`ctx.settings` | `settingsSchema` 与 `api.settings` | Schemastery 在翻译期导出为 JSON Schema；上游有界面时原样挂载，否则由翻译器生成通用设置页并打入 `ui/`，都只用公开视图桥 |
| 目标 | `webServer.register` | 视图桥操作 | 浏览器端界面调用的 HTTP 路由改为经视图桥的受限操作转发给宿主内的同一处理器；不开放任意 daemon API |
| 目标 | 浏览器端 React 界面（`lib/client.js`） | 重新打包为 `ui/` 页面 | 依赖 DSH 客户端槽位的部分明确报告，由兼容层提供最小槽位容器 |
| 目标 | `attachments`、`tokenMeter`、`compaction`、`agents`、`sessionProjections`（`ctx.get`） | 无 | 无对应公开接口：运行期访问明确抛出“宿主服务不可用”，不返回空值；报告列为 dropped |
| 目标 | 包内原生载荷与 `child_process` 加载宿主机器上的程序 | 原生载荷拒绝；运行外部程序属作者代码 | 不随包分发二进制；外部程序是否存在由用户机器决定，报告如实标注 |

【现状·已运行验证】翻译期执行 `apply` 发生在 Node 权限模型的子进程里：默认拒绝网络、子进程、Worker 与原生插件，文件系统只放行翻译运行时与上游目录，环境变量不继承（凭据不进入被记录的上游代码），60 秒超时。Node 官方文档说明权限模型是防误操作的护栏而不是对抗性沙箱，所以这一步不能被表述为"上游代码被隔离"；运行期的上游代码与其他 Magi 扩展一样以用户权限运行（完全信任），安装预览已按完全信任披露。翻译失败的条件：必需能力里有 `planned`、`dropped`、`unknown`；翻译期记录到的注册与静态扫描不一致；产物超出包限制；上游界面声明了槽位却没有任何 Magi 会渲染的槽位。翻译在 daemon 内串行执行，单次上限 180 秒。

【现状·已运行验证】已有静态分析：`core/registry.mjs` 工厂注册表（识别不出或并列明确失败）、`core/scan.mjs` 语法树扫描、`core/report.mjs` 状态报告（mapped／shim／bundled／planned／dropped／unknown）、`dsh/` 对照表；`node scripts/compat/cli.mjs inspect <目录> --require llm`，`npm run test:compat`。

【现状·已运行验证】对 `dsh-connect-workbuddy@5.18.1` npm 产物（早期实测，版本已由上段 5.18.2 取代）（`lib/index.js` 约 66.7 万字符，已构建、未压缩）的实测：16 条 `webServer` 路由、3 个工具（`workbuddy_draw_image`、`workbuddy_draw_video`、`codebuddy_ask`，后两个受配置开关控制）、1 个命令（`codebuddy`）、2 个适配器、1 个目录、1 个模型发现；esbuild 打包（含 `next` 线的 dsh 依赖和 pi-ai 1.5.0）单文件 5.7 MB（压缩前），其中 pi-ai 及其各厂商 SDK 约 4.4 MB。

【现状·已运行验证】2026-11-05 真机验收（macOS，真实 WorkBuddy 登录态，未使用 mock）：早期的离线构建方案（`cli.mjs build`，已由安装期翻译取代并移除）取得上游（核对 npm integrity）、在隔离子进程记录注册、打成 `magi.dsh-connect-workbuddy`（3 个工具、1 个命令、2 个模型数据源、16 条路由明确放弃，入口 2.8 MB 压缩后）。依赖目录按上游对等范围精确固定：cordis 4.1.0、schemastery 3.19.0、dsh-* 0.2.2-rc.1、pi-ai 1.5.0、react 18；`next` 线的 cordis 4.0.6-alpha.2 与 dsh-llm 0.2.2-rc.1 的对等要求（`~4.1.0`）冲突，不能混用。该包经公开 API 预览、安装、授权、启用、激活后运行态为 `running`、崩溃 0 次；WorkBuddy 国内版 16 个模型进入主对话数据源目录；会话选用 `hy3` 完成真实对话（输入 20875／输出 25 Token）；`workbuddy_draw_image` 经 Magi 工具审批（`allow_once`）后真实出图并扣 5.71 积分。未验证：`workbuddy_draw_video`、`codebuddy_ask`、国际版账号、设置页在 Electron 内的渲染、Windows。

【现状·已运行验证】真机暴露的两个问题已修：(1) 包校验把所有贡献说明限在 4096 字节，而真实工具 `workbuddy_draw_image` 的使用指引为 16843 字节，翻译器原先静默截断到 4000 字符，会让模型拿不到完整约束；现把工具说明上限提到 32 KiB（schema、包校验、生成类型同步），其余贡献仍为 4096，翻译器超限时明确失败而不截断。(2) 记录期必须用「开关全开」的配置变体才能发现受开关控制的工具，只用默认配置会得到 0 个工具而「成功」；现由翻译器自动追加该变体（见 6.1）。

【现状·已运行验证】2026-11-05 在本地打包的 Magi.app（`npm run desktop:package -- --dir`，随包 daemon 与 Node，独立状态目录）内复验，仅国内版：`codebuddy_ask` 经审批后调用 WorkBuddy 随包 CLI，返回 `ok=true`、reply `pong`（21088 输入／38 输出 Token）；`workbuddy_draw_video`（`kling-video-v3`、5 秒、720P）真实出片，扣 128.57 积分，文件为 H.264／AAC、1280×720、约 5 秒；工具调用在清单未声明 `timeoutMs` 时按 60 秒默认期限，本次视频任务跑完仍成功，期限对更长任务是否足够未验证。

【现状·已运行验证】复验暴露并已修一处宿主缺陷：上游框架重新挂载工具时先注册新处理器、再释放旧处理器，二者不互相等待；daemon 侧绑定集合以名字为键、没有代次，旧释放会把同名新绑定一并收回，表现为改任意一项设置后 `codebuddy_ask` 从工具目录消失。现在由宿主运行时（`host.mjs`）在发出释放前检查同名新注册是否在途，在途则不通知 daemon；新增集成测试 `rebinding_in_the_same_tick_keeps_the_new_binding`。

【现状·已运行验证·未解决】`/codebuddy` 命令在 Magi 中不可用：上游命令处理器读取 `invocation.agent` 的会话工作目录，而 Magi 的命令上下文只含会话与工作区标识，不含工作区路径；翻译器对 `agent` 明确抛出“不可用”。`codebuddy_ask` 工具不受影响（可传 `working_dir` 或 `allow_unattended`）。若要支持，需要插件系统先决定是否向扩展调用上下文公开会话的工作区目录（待定，见 13.2）；不得为此伪造 DSH Agent 对象。

【现状·已运行验证】WorkBuddy 随包 CLI 只含 `codebuddy-lite-wb` 一个产物，直接以 `-p` 启动会因缺少 `dist/codebuddy` 而 `MODULE_NOT_FOUND`；需要环境变量 `CODEBUDDY_FORCE_LITE_WB_BUNDLE=1`，或安装完整的 `@tencent-ai/codebuddy-code` 后用 `cliBridge.binary` 指向它。这是使用者机器上的环境事实，翻译器不得为其写入特例。

### 6.1 兼容层与翻译运行时

【现状·已运行验证】兼容层只用公开 API；依赖的 Cordis 框架随翻译运行时发行，但不得顺带挂载 DSH 会话／loop／审批／持久化服务成为第二个内核。发行目录 `daemon/compat/` 由 `apps/desktop/scripts/build-support.mjs` 的 `stageCompatRuntime` 生成，进入发行清单的逐文件哈希，`after-pack` 校验其存在；daemon 只在自身同级 `compat/` 查找，开发时用 `MAGI_EXTENSION_COMPAT` 覆盖。翻译与扩展宿主使用同一份 Node 解析结果；缺少随包 Node 或兼容运行时时，上游条目只可浏览，界面显示原因。

【目标】翻译报告应逐项列出 API 清单、依赖图、上游精确版本、平台要求与降级项。现状报告含上游版本与 integrity、工具／命令／数据源、路由、界面槽位处置、静态分析降级项与被省略的文件；依赖图与平台要求尚未输出。发现不支持或无法识别的动态导入／API 直接失败，不生成"成功但少功能"的包。静态分析不能证明任意 JS 的全部运行分支：运行时仍严格拒绝未知 API；未执行到的分支应标为未覆盖。

【现状】受开关控制的工具在记录期可见，靠翻译器自动追加"所有开关打开"的配置变体，不再需要作者提供；是否所有开关型配置都能被这样发现仍属待定。

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
| 现状·已运行验证 | P4 DSH 翻译器 | 安装期动态翻译、多来源搜索、预览确认、升级；真实 npm registry 全链路与发行目录中的翻译运行时验证；不嵌入第二内核 |
| 目标 | P3 pi 翻译器 | 在同一工厂注册表与来源/预览/安装链路上新增 pi 生态；翻译器提供前 pi 条目只可浏览，安装明确拒绝 |

【现状】不创建 pi 空适配器或兼容声明；pi 条目可浏览、不可安装。原生 MCP 不列为插件。发布签名、公钥托管与官方目录内容须由发布者决定，不把本地开发密钥当官方信任根。

### 11.1 运行时兼容层改造计划（2026-11-06）

【目标】优先级固定为 Magi 稳定性、插件系统正确性、生态兼容完整性。DSH 先交付；pi 的规范用于共同协议和工厂边界设计，pi 运行器随后接入。保留现有插件设置页的共享布局、原生 UI、图标与顶部操作、整页滚动、停用后导航和设置页同时消失的交互。MCP 保持 Magi 内置功能。Windows 实机与安装包验收等待可用环境，不能据 macOS 结果宣告完成。

| 状态 | 步骤 | 模块与交付 | 退出条件 |
| --- | --- | --- | --- |
| 目标 | T1 上游合同 | 固定 DSH 与 pi 提交；核对官方开发文档、类型、加载器、生命周期与服务实现；逐项记录原样复用、适配、内核所有权冲突、未实现及验证范围 | 事件顺序、返回值、异步、取消、回收与版本约束有明确证据；不以 API 同名证明兼容 |
| 目标 | T2 原包安装 | `scripts/compat` 工厂、`magi-api/plugins/upstream`、`magi-plugin-system`：稳定且无碰撞的来源身份、依赖/版本校验、不可变上游文件和摘要 | 预览与安装身份一致；不同 npm 名不得互相升级；安装不执行上游业务初始化；不生成维护分叉 |
| 目标 | T3 公共运行协议 | schema、`magi-extension-host`、`magi-plugin-system`、daemon：动态工具/命令/模型目录、配置 schema、实例租约、注册回收 | 晚注册与条件注册正常生效；最终参数沿原生 schema/权限/审批校验；旧实例不得修改新目录；失败与退出清理全部贡献 |
| 目标 | T4 DSH 宿主 | 真实 Cordis 生命周期、effect、配置更新、工具与命令、模型流/附件、事件合同及客户端宿主 | 初始化错误不就绪；缺依赖不假成功；事件不漏执行；多模态声明与执行一致；未支持的必需契约明确拒绝 |
| 目标 | T5 契约回归 | 固定夹具与上游行为对照，Node/Cordis 实测、Rust 注册准入与生命周期测试 | 覆盖初始化失败、异步/动态注册、同名重绑、撤权、崩溃、取消、配置更新、图片、来源碰撞与不支持 API；静态分析测试不能代替运行验证 |
| 目标 | T6 产品验收与清理 | macOS 发行包与真实 WorkBuddy 国内版登录态；更新本文件与公开作者/协议合同，移除已替代的记录器与特例 | 原包搜索、预览、安装、启动、配置、模型选择、实际调用、停用、重新启用及升级闭环；报告实测范围与未覆盖项 |

【目标】动态声明是宿主向 daemon 提交的提案：daemon 校验授权范围、大小、schema、唯一身份和实例代次后维护唯一运行目录；包身份与用户授权仍来自不可变安装事实。适配器不得自行批准工具、写 canonical 日志、替换会话引擎或另建任务存储。

【目标】安装预览说明可动态贡献的能力种类与完全信任风险，不能把尚未启动的零工具目录表述为插件没有工具。运行期发现目录变化，通过既有事件投影刷新工具、命令与模型；不按定时等待完成来推断初始化成功。

【现状·已运行验证】改造前基线：18 项翻译器测试通过，但最小夹具仍复现初始化抛错后成功打包、2 秒异步注册被 1.5 秒记录窗口漏掉、拦截钩子未接线仍打包、npm 名归一化碰撞，以及图片请求 HTTP 400。三份缺陷包通过 Rust 安装包校验；因此早期 P4 成功记录不等于 T1–T6 已完成。

### 11.2 运行时生命周期续推记录（2026-11-06）

【现状·已运行验证】在 `codex/dsh-translator` 工作树继续 T3／T4／T5，修复以下问题：

- 模型发现尚未结束时，适配器已撤销或变化：刷新以代次排除旧结果并补跑期间收到的变化事件，初始化阶段也不漏事件。卸载使未完成的发现失效；daemon 拒绝回写时，本地 HTTP 端口仍被关闭。
- 普通配置按版本串行重载；Volatile 更新保持实例、保留最新原始配置供后续重启，并按上游 loader 的 fiber 范围通知监听者。
- 停用／撤权后重新激活不能使旧实例租约复活；旧实例的迟到写入与回收不能覆盖或清空新模型来源。清理与新实例准入使用同一 manager 锁；移除动态来源声明同步移除其已发布端点。
- 删除没有实例租约的旧 `instance_detached` 回调，回收统一走 `instance_ended(pluginId, lease)`。持久资源与设置保留，运行期目录和模型来源回收。

| 已验证入口 | 结果与范围 |
| --- | --- |
| `npm run test:compat` | 30 项：原 18 项静态／路由／界面槽位测试，加 12 项真实 Cordis 生命周期、工具管线与本机 HTTP 图片桥回归。daemon RPC 在 Cordis 测试中使用有版本的测试替身；图片到达本机模型替身，未调用 WorkBuddy 模型 |
| `cargo test -p magi-plugin-system --test runtime_catalog --test lifecycle` | 4 项新运行目录测试＋20 项生命周期回归：初始化隐藏、原子 CAS、声明边界、重复／超量／非法 schema、旧实例与撤权失效、重启不持久化动态目录 |
| `cargo test -p magi-daemon --lib daemon::extension` | 26 项：含真实 Node 运行后新增／撤销／重绑工具，沿原生工具入口重验入参，动态模型来源撤销，停用和过期实例回收 |
| `cargo test -p magi-extension-host --test host` | 22 项真实 Node 宿主回归：绑定、同名重绑、路由、取消、崩溃、超时与进程回收 |
| `npm run protocol:check` | schema 生成物一致；协议与作者指南补充动态目录、租约及回收合同 |

【现状·静态核对】本机已有 `dsh-connect-workbuddy@5.18.1` 原包按当前安装期必需类别重新检查，仍有三项阻塞：`ctx.sessionProjections`、`sessionProjections` 注入及 `session/event`。未放宽检查或跳过这些功能；需继续核对只读投影、条件注入和事件消费者的精确合同，并沿 daemon 事实接线。不能把本节回归通过解释为 WorkBuddy 已完成改造后验收。

【未运行·截至本节记录】未重新下载或登录 WorkBuddy、未打包新版 Electron、未进行原生设置页交互或 Windows 验收；后续主分支打包与界面验证见 11.3。pi 运行器、上游完整契约分母、依赖安装覆盖及旧安装记录器清理仍在 T1／T2／T4／T6 范围。运行验证使用当前工作树依赖锁及 macOS arm64 Node 26.13.0，不代表其他上游版本或平台通过。

### 11.3 主分支合并与目录包验收（2026-11-06）

【现状·已运行验证】`codex/dsh-translator` 的正式代码已通过 `f459634a` 合入 `main`；另一个 `project-multi-root` 工作树的提交也已包含在主分支。工作树剩余未跟踪文件为历史截图／探测脚本和本地依赖链接，保留原处。合并后的真机验证补充三组修复：

- `1685effa`：补全随包兼容层依赖与资源校验，修复设置默认值和原生服务回归，加入真实 Magi Main 验收入口。
- `9cd4363b`：停用先取消在途租约；即使插件在收到 abort 后返回成功，宿主也拒绝迟到成功回执。此前真实验收中错误完成的 Turn 现在按失败／取消有限结算。
- `8d42a7df`：设置页初始化失败沿现有状态区展示原因，读取成功前禁用保存。验收从只监听主 Renderer 扩展到插件 iframe 与 Electron 控制台日志，覆盖加载撞上生命周期排空的未捕获异常。

【现状·已运行验证】最终目录包为 `target/electron-dist/mac-arm64/Magi.app`，构建身份 `8d42a7df`、`gitDirty=false`，Magi 3.0.62／Electron 45.0.0／Chromium 156.0.8078.48／随包 Node 26.13.0。使用本机已安装的同版本 Electron 重新组装目录包，`after-pack` 对资源摘要、兼容层依赖和随包 Node 的自检通过；包内 daemon 与本次 release 构建的 SHA-256 相同。

| 已验证入口 | 结果与范围 |
| --- | --- |
| `MAGI_PLUGIN_TEST_APP="$PWD/target/electron-dist/mac-arm64/Magi.app" MAGI_PLUGIN_TEST_PORT=38936 npm run test:local-plugins-ui` | 26 项通过：包内 daemon／Node、公开 API、真实 Electron 工作台，覆盖在途停用、重新激活、升级清空授权与旧格式数据、卸载、模型来源撤销后明确失败且不回退；本地 HTTP 模型替身，无用户凭据 |
| `npm run test:electron-plugin-runtime` | 最终包 16 项通过：真实 Magi Main 托管 sidecar、Cordis 晚注册、设置默认值与保存／重载、DSH 模型流式回复、停用与重启、390px 深色视口，以及 iframe／主 Renderer 无运行时异常。通过 CDP 暂停设置读取并注入 409，断言加载及失败时禁止保存、显示错误、配置版本不变，重新加载后恢复；旧包在新增断言上失败，修复包通过 |
| `cargo test --release -p magi-extension-host --test host` | 23 项真实 Node 宿主回归通过，包含收到取消后仍返回成功的处理器；`cargo check --release -p magi-daemon` 通过 |
| `npm run test:compat` | 设置初始化修复后 30 项通过；验收脚本语法及 `git diff --check` 通过 |

【现状·证据】Electron 结果与截图保存在 `target/plugin-acceptance/runtime/` 的 `results.json`、`settings-desktop.png`、`conversation.png`、`inactive-390.png`、`electron.log` 和 `workbuddy-result.json`。设置读取失败用例为主动故障注入；其他生命周期与配置流程通过真实 daemon 执行。390px 为桌面 Electron 视口模拟，不是手机实机。

【现状·截至本节记录】真实 npm `dsh-connect-workbuddy@5.18.1` 原包在当时目录包的预览入口返回 HTTP 400，原因是 `ctx.sessionProjections`、`sessionProjections` 注入及 `session/event` 未支持；当时只有拒绝路径通过，不能解释为 WorkBuddy 登录态或模型调用通过。后续原包在线安装与真实模型调用见 11.4。本轮未进行 Windows、干净 macOS 安装、真实付费模型与全仓测试；pi 运行器和 T1–T6 的其余范围仍未完成。

### 11.4 首款原包在线翻译验收：WorkBuddy 5.20.2（2026-11-06）

【现状·已运行验证】首款验收必须是从 npm 在线安装的 `dsh-connect-workbuddy` 原包，不能用夹具或拒绝路径替代。`24e7a37a` 对齐两项通用上游合同，未修改 WorkBuddy 原代码，也未按包名添加兼容分支：

- Cordis `ctx.inject` 是条件挂载，不等于导出的必需 `inject`。静态分析只在全部使用点均有同一服务的条件注入保护，或同步注册失败有可追踪的 catch 时标注可选；`partial`／`dropped` 状态仍保留并在预览展示原因。未知能力、必需依赖、未捕获注册失败和不能证明的回调逃逸仍拒绝。运行期保留 Cordis 条件 fiber 的 pending 状态，缺少必需服务仍使激活失败。
- 官方 `@deepseek-ai/dsh-app-boot@0.2.2-rc.1` 的 `lib/index.js` 中 `plugin-compatibility` 明确用 `semver.satisfies(..., { includePrerelease: true })` 检查 DSH peer 范围。兼容层对齐该规则；直接依赖和非 DSH peer 仍用普通语义版本校验，超出上游范围的版本仍拒绝。无需更改已固定的 DSH 依赖版本。

【现状·已运行验证】安装预览沿已有设置组件显示降级原因；清单声明的动态工具／命令／模型目录在激活前显示“激活后动态注册，当前尚未加载”，不再误写成“无”。

| 入口 | 结果与证据范围 |
| --- | --- |
| `npm run test:electron-workbuddy` | 11 项通过。macOS arm64 真实目录包 Main／daemon／随包 Node，界面搜索 npm → 原包下载与完整性预览 → 确认安装 `5.20.2` → 授权激活 → 上游设置卡片渲染 → 主对话模型目录 → 使用本机已有 WorkBuddy 国内版登录态调用 `hy3` → 停用回收来源 → 重新激活；主窗口和插件 iframe 无未捕获异常 |
| 真实回复与 canonical Turn | `turn-session-action-1793974719003` 为 `completed`；绑定来源为该插件的 `workbuddy`，模型为 `hy3`，模型步骤记录 20,957 Token。单独检查 `assistant_text` 精确等于 `MAGI_WORKBUDDY_LIVE_OK`，不以用户消息中的同名文本充当回复证据 |
| `npm run test:compat` | 35 项通过；新增条件依赖、捕获／重抛、回调逃逸、真实 Cordis 可选 fiber、DSH peer 预发布准入与越界拒绝回归 |
| `npm run test:electron-plugin-runtime` | 同一产品代码 16 项通过；必需事件拒绝改用不捕获错误的独立夹具，不再把 WorkBuddy 拒绝当作正向验收 |
| Web 定向验证 | `npm --prefix web run check` 0 错误 0 警告；`test:i18n`、`test:plugin-store`、构建与 `git diff --check` 通过 |

【现状·证据与状态】真实安装和会话使用独立 Magi／DSH 状态目录，原包自行读取本机已有桌面登录态；没有修改用户原有 Magi 会话。`target/plugin-acceptance/workbuddy/` 保存 `installed.json`（版本、来源、会话与状态目录）、`results.json`、`install-preview.png`、`settings.png`、`conversation.png` 和 `electron.log`。凭据不写入验收结果文件。该脚本会发出真实模型请求，仅显式运行，不并入默认测试套件。

【现状·兼容边界】`session/event` 自动续发仍由上游捕获不支持错误后关闭；`sessionProjections` 当前工作目录读取分支因服务缺席而不挂载。这是上游明确支持的可选降级，不代表这两项能力已经实现。本轮未验证真实生图／视频、`codebuddy_ask`／`/codebuddy`、国际版登录态、Windows、干净系统安装和全部 DSH／pi 插件兼容性。

### 11.5 插件列表交互与在线升级验收（2026-11-06）

【现状·已修复】原界面缺少搜索与翻译期间的反馈，失败消息位于列表上方，安装操作随详情滚动；搜索又被状态分组与名称排序覆盖，上游包在安装前没有真实图标。`7076b0a2`、`1b406ede` 沿现有 daemon 安装入口和共享设置组件完成三项修复：搜索增加加载、失败重试和过期结果隔离，精确包名优先；上游图标从原包声明读取，翻译进度、取消、错误和确认留在当前详情；安装、确认及在线更新固定在详情底部，长说明和权限在上方滚动。上游升级继续从原 npm 来源下载、翻译、预览并确认，详情不混放文件升级入口。

【现状·已运行验证】在 macOS arm64 的真实 Magi.app 目录包中执行 `npm run test:electron-plugin-interactions`，21 项通过。包内构建标识为 `1b406ede`，随包 Electron 45／daemon／Node；使用独立状态根，不激活本轮安装的插件或调用模型。在线安装与升级均使用真实 npm 原包；取消、乱序和搜索失败用例通过 CDP 暂停或失败请求，未替代安装事实。

| 验收范围 | 结果 |
| --- | --- |
| 搜索与列表 | `dsh-rea` 精确匹配第一，搜索动效、失败重试、连续改词的过期响应隔离通过；未安装 WorkBuddy 的列表与详情真实图标加载成功 |
| 安装反馈 | 重复点击只发起一次预览；取消恢复操作；确认期间禁止重复提交；安装后保留当前插件详情 |
| 固定操作 | 桌面与 390×844 窄视口下，安装、确认安装、在线更新按钮在长内容滚动时保持可见；浅色错误页无横向溢出，深色预览与更新操作可见 |
| 在线升级 | 先从 npm 安装 `dsh-connect-workbuddy@5.20.0` 并读取 daemon 确认旧版，再经界面预览和确认更新至 `5.20.2`；版本显示正确，无文件升级入口 |
| Web 定向检查 | `npm --prefix web run check` 0 错误 0 警告；`test:plugin-store`、`test:i18n` 与 `git diff --check` 通过 |

【现状·证据与限制】结果在 `target/plugin-acceptance/interactions/results.json`；截图包括 `dsh-rea-error.png`、`preview.png`、`update-preview.png`、`install-390.png`、`preview-390.png`、`update-390.png`、`error-390.png`。该脚本保留 dsh-rea 的失败反馈路径，在线兼容成功与真实运行见 11.6。390px 是桌面 Electron 视口模拟；未验证 Android／iOS 真机、触摸、200% 缩放、RTL 或全部插件兼容性。此前 WorkBuddy 真实模型调用证据见 11.4。

### 11.6 DSH 原生页面与 dsh-rea 在线验收（2026-11-07）

【现状·已运行验证】`node scripts/verify-electron-dsh-rea.mjs` 使用真实 macOS arm64 目录包、独立状态根、npm 在线原包 `dsh-rea@1.2.0` 和 `rea-agents@6.5.0`，共 16 项通过：

- 安装前搜索精确匹配第一，npm 原包声明的 SVG 图标在列表和详情可显示；在线预览、确认安装和安装后图标接口均保留原始 SVG 字节，并带 `image/svg+xml`、`nosniff` 与 sandbox CSP。
- 原包业务页面、React 组件、样式、语言包、示例提示、复制按钮、Ghidra 外链和设置表单均由上游页面直接渲染。Ghidra 链接保留原始 href、`target="_blank"` 和链接色；外链点击经宿主外部链接入口打开。
- `rea-agents@6.5.0` 的真实 MCP 握手返回 207 个工具；动态目录超过安装清单的 128 项静态贡献上限后仍能在运行目录进入 daemon，目录总量与单工具 schema 受独立 2 MiB／256 KiB 边界保护。
- Remote 状态页显示真实 REA 版本和工具数量；修改 300000 毫秒超时为 310000 后经 CAS 保存，触发上游 Volatile 配置重载；停用清空动态工具，重新激活恢复工具并保留配置；原窗口和 iframe 无未捕获异常，390px 无横向溢出。

证据保存在 `target/plugin-acceptance/dsh-rea/results.json`、`evidence.json`、`online-svg.png`、`settings.png` 和 `settings-390.png`。本轮仍未覆盖 Android/iOS 真机触摸、Windows、Ghidra 实际安装与原生二进制分析，以及全部 DSH 包。

### 11.7 热门社区 DSH 插件兼容审计（2026-11-07）

【现状·已运行验证】`npm run test:electron-popular-dsh` 在真实 macOS arm64 目录包、独立状态根和 npm 在线来源上审计 9 个近期社区包。脚本对每个包执行精确版本预览、翻译、确认安装、授权、激活和运行状态检查；成功包在结束时停用并卸载，避免污染用户状态。

| 包 | 版本 | 结果 | 原因或证据 |
| --- | --- | --- | --- |
| `dsh-plugin-health` | 3.0.5 | 阻止安装 | 宿主 `@deepseek-ai/cordis@4.1.0` 不满足包声明的 `~4.0.5-alpha.2` |
| `dsh-plugin-ops-bundle` | 0.20.8 | 阻止安装 | 使用未登记的 `ctx.baseUrl` |
| `dsh-plugin-upgrader` | 1.8.0 | 阻止安装 | 事件名动态生成，静态分析无法证明语义 |
| `dsh-plugin-debugging` | 0.7.2 | 阻止安装 | 使用未登记的 `ctx.events`，并发生上下文逃逸 |
| `dsh-plugin-appshot` | 0.4.2 | 阻止安装 | 依赖附件、会话等由 Magi 内核独有的能力 |
| `dsh-plugin-share` | 0.2.5 | 阻止安装 | 使用未登记的 `ctx.connection` 与 `ctx.pluginManager` |
| `dsh-plugin-mgr` | 0.2.12 | 阻止安装 | 使用未登记的 `loader` 服务 |
| `dsh-plugin-release-probe` | 0.0.8 | **通过** | 在线翻译、安装、授权、激活成功，运行状态 `running`，设置视图贡献正常 |
| `@99galaxy/dsh-plugin-market` | 1.2.2 | 阻止安装 | 上游客户端模块格式不符合当前原样页面加载合同 |

这组结果说明兼容门禁正在拦截真实生态中的版本、宿主服务、动态事件、上下文逃逸和客户端格式差异；`dsh-plugin-release-probe` 提供了除 WorkBuddy、dsh-rea 外的第三个真实安装运行样本。审计结果保存在 `target/plugin-acceptance/popular-dsh/results.json`。

### 11.8 功能验收与右栏原生页面补全（2026-11-07）

【目标】用户要求至少 95% 的 DSH 插件正常在线安装并完成核心功能；提供运行页面的插件须能在 Magi 右栏实际使用，原页面代码保持不变。该比例是后续退出条件，当前未达到，也不能用少量样本的启动成功率代替。固定生态包版本与分母、安装、核心行为、页面、升级分别留证；未知和未测不计成功。

【现状·已运行验证】本轮沿原有兼容层和视图桥补全以下通用合同：

- 原生客户端记录等待异步 `apply`；提供 `sidebarRight`／`sidebarRightTabs` 的界面注册与受控导航。页面身份包含槽位名和上游 key/id，同一槽位的多个页面各有独立清单入口，不再只取第一项。页签名称取上游声明；设置页、完整页面、右栏页签继续由未经修改的上游页面渲染。
- 记录器加载与真实页面相同的随包预构建浏览器模块，移除“任意导出都返回 null”的 UI 库替身。REA 原包在 `apply` 中构造真实 `SettingsFormModel` 的路径由此恢复；未知模块仍明确失败，不能以空组件完成准入。
- `PluginRouteCallRequest` 增加可选应用请求头，原生页面自带的鉴权头能到达其 Node／Fetch 处理器；宿主身份、代理和传输头不能覆盖。opaque-origin iframe 的 localStorage／sessionStorage 通过按插件 ID 隔离的界面缓存桥访问，不开放同源权限，不借此存储 daemon 业务事实。
- `api.plugins.list()` 每次读取 daemon 的真实目录；DSH 的 `listBundles`、`registries` 映射到真实包来源、版本和启用状态，不生成第二份 DSH profile。安装、升级和生命周期写操作尚未映射，目录明确标注只读管理边界。删除空 loader／sessions 服务，必需依赖缺失不再假装启动成功；对应兼容报告改为未完成或部分支持。

【现状·已运行验证】macOS arm64 目录包（真实 Electron Main、sidecar、随包 Node／compat）验证：

| 验证 | 本轮结果与边界 |
| --- | --- |
| `npm run test:compat` | 51 项通过，含异步/条件多面板、身份碰撞、页面存储、应用请求头、真实插件目录与必需服务缺失回归 |
| `npm run test:electron-plugin-runtime` | 22 项通过：原有配置／工具／模型链路，加两个独立右栏原生页面、页面跳转、真实后端动作与请求头、跨页面界面缓存、宿主键隔离、390px、停用回收；这是机制夹具，不计为真实生态包兼容率 |
| `npm run test:electron-workbuddy` | 在线原包 `dsh-connect-workbuddy@5.20.2` 11 项通过，含原生设置页、本机真实登录态及 `hy3` 模型调用、停用与重激活 |
| `npm run test:electron-dsh-rea` | 原包 16 项通过，真实 MCP 207 个工具、真实 UI 库构造与原生设置页、Ghidra 外链、CAS 保存／配置重载、390px 和停用／重激活；没有执行 Ghidra 二进制分析 |
| daemon 定向回归 | 插件目录读取与停用拒绝、路由透传／准入／取消各 1 项通过 |
| Web／协议 | Web 类型检查 0 错误 0 警告；right-pane、plugin-store golden 和 protocol:check 通过。仅修改的 Rust 文件格式通过；全仓 fmt 检查仍有本轮未修改文件的既有差异 |

【现状·已运行验证·未达标】9 个固定社区样本重新在线安装并打开真实页面后：7 个安装、启动及页面加载通过，5 个安全后端探测通过，无未捕获页面异常；**没有任何一个样本完成“全部核心业务＋升级”的完整验收**，脚本以退出码 1 报告未达到门槛。不能把 7/9 或页面无异常解释为生态兼容率。

- `dsh-plugin-ops-bundle@0.20.8` 的健康页面及右栏沙箱页面均可显示，沙箱状态读取真实成功；健康扫描仍缺少 `listVersionExemptions` 等管理能力，沙箱启动／安装／修复未通过功能验收。
- `dsh-plugin-share@0.2.5` 已从 daemon 读取真实已安装包，真实导出组合码、解析及二维码生成通过；导入、升级与生命周期写操作仍未完成。
- `dsh-plugin-upgrader@1.8.0` 已显示 daemon 的真实插件目录，消除了 `listBundles is not a function`；仍只读，不能把列表显示或 HTTP 200 计为升级成功。
- `dsh-plugin-health@3.0.5` 页面可显示，但其 DSH profile 文件扫描未映射到 Magi；`dsh-plugin-debugging@0.7.2` 的页面及工具登记可用，工具业务未逐项验证；`dsh-plugin-release-probe@0.0.8` 仅完成安装、启动和设置页显示。
- `@99galaxy/dsh-plugin-market@1.2.2` 的原始完整页面可在右栏打开并读取真实目录；安装与 profile 重载动作未验收。
- `dsh-plugin-mgr@0.2.12` 的必需 loader、`dsh-plugin-appshot@0.4.2` 的必需 sessions 尚未完整实现，本轮明确阻止安装。此前“running”来自空服务，不能沿用为功能通过证据。Appshot 的浏览器存储加载错误已通过通用桥修复，但其会话投递能力仍待接线。

证据：`target/plugin-acceptance/popular-dsh/{results.json,coverage.json,electron.log}` 及各原生页面截图；右栏机制为 `target/plugin-acceptance/runtime/{results.json,panel-first.png,panel-second.png,panel-390.png}`；WorkBuddy 为 `target/plugin-acceptance/workbuddy/`，REA 为 `target/plugin-acceptance/dsh-rea/`。验收脚本现在区分安装、启动、页面、后端探测、完整业务和升级，未完成项保留 unverified；逐包清理沿停用、禁用、卸载顺序，不吞掉清理失败。证据保存在构建目录，未纳入版本库。

Windows、手机触摸、完整生态分母及 95% 目标均未验收。后续须补齐 daemon 原生会话投影、插件管理写操作、profile/loader 语义与业务级回归，不能靠添加空服务或跳过失败提高比例。

### 11.9 插件管理写操作与原包业务验收（2026-11-07）

【目标】继续以生态安装和核心业务完整兼容为验收口径，95% 为此前最低目标，100% 为用户期望目标；当前均未达到。不能把新增管理服务机制或单个原包的部分核心路径通过换算成生态兼容率。

【现状】新增 `PluginOperations`，HTTP、插件中心、上游安装与受信 Node 管理调用使用同一组安装、升级、生命周期和排空实现。服务只捕获原 manager、事件、随包工具链与进程组，监管端口使用弱引用，避免捕获完整 ApiState 造成引用环；不生成 DSH profile，也不让 Node 维护安装事实。

- 新增 `plugin.management` 功能协商、`pluginManagement` 权限及 `install`／`lifecycle` 两个目标。插件详情和安装预览沿共享权限组件展示中英文告知。译包实际使用 pluginManager 时声明该授权；只读目录仍不包含配置或凭据。
- DSH 的 `installBundle`、`setBundleEnabled`、`removeBundle` 映射到原服务。npm 精确版本、标签与范围解析后，最终仍经 daemon 校验 npm 包、完整性、精确版本和翻译摘要。Git／本地来源和构建脚本不伪装支持。
- 新装或升级不会代替用户确认目标包权限；未授权激活明确失败，并保留“已安装、后续未完成”的信息。管理调用不能在自身请求内升级、停止或删除发起插件，避免回执前销毁自己；这类操作仍可沿 Magi 原生在线入口完成。
- 发起插件摘要、激活、权限与实例租约在异步等待后的提交锁内复核；管理写操作要求活动调用租约，钩子不能隐式启动管理动作。已停用目标不重复执行停用，卸载先停止运行宿主再移除事实。启用操作以实际宿主状态确认运行，启动失败不算成功。

【现状·已运行验证】`test:electron-dsh-management` 从新构建的 macOS arm64 目录包启动真实 Electron、sidecar 和随包 Node：11 项通过。在线安装未经修改的 `dsh-plugin-share@0.2.5`，原生页面生成／解析组合码、二维码、点击安装 `dsh-plugin-release-probe@0.0.7`（保持禁用）、导入停用／启用状态并确认目标宿主恢复；共享管理服务机制夹具再验证错误摘要拒绝、真实 npm 原包从 `0.0.7` 在线升级至 `0.0.8`、清空旧版授权、未授权激活拒绝、运行中卸载和自身删除拒绝。后半部分是机制夹具，不称为 Upgrader 原包业务验收。390px 截图确认原页面与宿主设置页可显示。

证据：`target/plugin-acceptance/dsh-management/{results.json,electron.log,share-installed.png,share-390.png}`。前两次验收脚本分别因测试输入缺少 npm 类型和机制夹具缺少视图声明失败，修正夹具后完整重跑通过，未绕过产品准入。

【现状·定向回归】兼容层 53 项通过（含标签／版本范围从本机 registry 解析后固定精确版本、拒绝静默切换安装源）；API 插件路由与管理服务 25 项通过；daemon 服务 9 项通过；扩展宿主 15 项单元与 23 项真实 Node 集成通过；运行目录租约回归 6 项通过。Web 类型检查 0 错误 0 警告，i18n、protocol:check 和差异空白检查通过。未运行全仓测试。

【现状·社区复测仍未达标】同一目录包重跑 9 个固定样本，7 个安装、启动和页面显示通过，5 个只读后端探测通过；脚本仍以退出码 1 报告不达标，完整核心业务／升级均未由该批量脚本验收。Share 的新增业务证据来自单独脚本，不能把它或管理机制夹具拼成 7/9 完整业务通过。Health 仍报告 profile 缺席，Ops 仍缺 `listVersionExemptions`，Upgrader 真实返回 `managed:false`／`not a profile-managed package`。证据为 `target/plugin-acceptance/popular-dsh/{results.json,coverage.json,electron.log}`。

【剩余】DSH 原生 profile 文件、loader 行、健康扫描、版本豁免、Agent／会话及平台原生辅助进程等语义仍须沿对应 Magi 服务继续适配。Upgrader 原包还检查 DSH profile 的托管身份并直接修改其 manifest，新增通用在线管理 RPC 不代表该原包已完成升级业务；Mgr／Appshot 的必需服务仍缺席。Share 已验证 npm 原包安装与启停，Git 导入、完整扫码和升级业务未验收。Windows、手机触摸、生态全集分母及 95%／100% 均未验收。

### 11.10 调用期 profile、委托授权与原包复测（2026-11-07）

【现状】DSH 的 `loader.entries()`、`profileContext` 与 profile 文件读取来自同一次 daemon 插件目录快照。`package.json` 的依赖和 bundles、patch 行及每个包的版本均由原安装事实投影，不创建物理 profile、文件监听器或第二份安装注册表。启停修改只形成调用内提案，回执前通过原生生命周期入口提交；失败、取消及摘要变化不能提交草稿。临时原子写入文件只保留在当前调用。上游 ESM 与 CommonJS 共用同一 fs / fs-promises / child-process 外观；profile 外仍使用原生异步文件 API，保留 opendir 的异步迭代和目录关闭语义。

【现状】新增独立 `pluginManagement:["authorize"]` 委托，安装预览及详情明确告知可为其他插件授予其声明的权限，包括完全信任。用户授权管理插件后，管理插件才能为安装回执中的精确摘要申请授权，再沿原生启用／激活入口等待实际运行。自授权限、未声明权限、过期目标摘要、失效发起者租约均拒绝。仅有 install／lifecycle 的插件仍不能代授。HTTP 与宿主目录共同合并监管器状态，Health 不再把已运行目标显示为 pending。

【现状·已运行验证】macOS arm64 目录包（Electron 45.0.0、随包 Node 26.13.0）：`test:electron-dsh-profile` 11 项通过，未经修改的 Mgr 原包在线安装、原生页面、读取真实行、停用／重启、通过 `dsh plugin add` 在线升级 release-probe 0.0.7→0.0.8 并运行，以及真正卸载均通过；profile 未落盘。`test:electron-dsh-management` 13 项通过，新增 Share 原包获独立委托后安装并运行新包的检查。Share 0.2.5 在自己业务代码中明确拒绝升级已有包，返回 manual-update-required；保留该上游行为，升级链路由 Mgr 验收，未修改原包绕过其决策。

【现状·定向回归】兼容层 58 项、API 管理服务 4 项、daemon 扩展服务 9 项通过；Web 检查 0 错误 0 警告。补充 ESM／CommonJS 文件导入、异步目录扫描、临时原子写入、取消和连续安装／升级摘要回归。构建有既有 WebFolderPicker 静态／动态导入警告；未运行全仓测试。

【剩余】Health 基础文件体检、运行状态和可靠性报告已能读取调用期 profile 快照；DSH 运行版本仍显示未知，不记为全部业务通过。Ops 的文件导入和 CommonJS 准入阻塞已修复，安装、启动与两个原生面板可显示，但健康扫描仍因缺少 `listVersionExemptions` 失败；验收将页面显示的方法缺失判为失败，不用空豁免目录伪造支持。版本豁免的原生持久化／兼容准入、完整包元数据投影、Agent／sessions、Appshot 业务、Upgrader 完整升级／回滚及平台辅助进程仍未完成。95%／100% 生态覆盖均未达到，Windows 与 pi 运行器仍未验收。

证据：`target/plugin-acceptance/profile/`、`target/plugin-acceptance/dsh-management/`、`target/plugin-acceptance/popular-dsh/`，构建目录不纳入版本库。

## 12. 验收与证据边界

### 12.1 已运行验证

| 状态 | 入口 | 本轮证据 |
| --- | --- | --- |
| 现状·已运行验证 | `npm run test:local-plugins` | 本机真实 daemon＋Node＋本地 HTTP 模型，20 项通过：预览／授权、input guard、服务、直接 OS 能力、CAS、用户提问、无 Task 服务拒绝、调度、在途停用、重激活、升级重置、卸载、内置 MCP 管理面 |
| 现状·已运行验证 | `npm run test:local-plugins-ui` | 在同一纵向流程追加真实 Electron Chromium：设置页创建计划、展示目标会话／预算／daemon 回执，桌面与 390px 深色视口无横向溢出；21 项总检查通过 |
| 现状·已运行验证 | `MAGI_PLUGIN_TEST_APP=<Magi.app> npm run test:local-plugins` | 2026-11-03：对 `npm run package --workspace @magi/desktop -- --dir` 生成的 macOS arm64 目录包，直接启动包内 daemon，只使用随包 Node（不设 `MAGI_EXTENSION_NODE`、不读 PATH），同一 20 项纵向流程全部通过；未签名、未生成 dmg，未在干净机器上安装 |
| 现状·已运行验证 | `npm run test:electron-plugins` | 同一目录包经 CDP 驱动真实打包 Electron：工作区导航与 MCP 服务 88 项通过（该脚本不含扩展内容；扩展的界面验收见 `test:local-plugins-ui`）。同一脚本某次运行曾在“桌面设置展示 daemon 的连接统计”处等待超时，重跑通过，按时序不稳定记录，未定位根因 |
| 现状·已运行验证 | `npm run test:compat` | 2026-11-05：静态兼容分析 5 项通过（生态识别、能力状态、必需能力判定、命令行退出码）；只证明分析工具行为，不证明任何上游包可运行 |
| 现状·已运行验证 | `cargo test -p magi-extension-host --all-targets` | 15 个单元＋17 个真实 Node 集成通过，含模型预算、独立取消、异常、guard、卡死回收与长路径端点 |
| 现状·已运行验证 | `cargo test -p magi-plugin-system --all-targets` | 包／生命周期／资源／计划持久化及签名目录定向套件通过 |
| 现状·已运行验证 | API／daemon 定向回归 | Turn 接纳前取消计划收口、models 未授权拒绝、命令清单准入通过 |
| 现状·已运行验证 | Rust／Web／Desktop 基础检查 | 相关 crate check；宿主／插件系统／daemon 库 Clippy（含传递依赖）零警告；Web 0 错误 0 警告；Desktop check、protocol:check、i18n／桥接定向检查、格式和 diff 检查通过 |

### 12.1.1 上游来源验证

| 状态 | 入口 | 证据 |
| --- | --- | --- |
| 现状·已运行验证 | `npm run test:compat` | 翻译器单元与夹具 18 项通过（工厂识别、静态分析、槽位策略含 `settings.section`、图标、路由取消等） |
| 现状·已运行验证 | `cargo test -p magi-api --lib routes::plugins` | 21 项通过，含包名校验、升级状态判定、翻译摘要解析 |
| 现状·已运行验证 | 真实 npm 全链路（开发构建） | 见 6.3：搜索、预览、拒绝路径、安装、激活运行、旧版升级到新版；使用 `target/debug/magi-daemon-app` 与 `apps/desktop/dist/compat`，Node 取开发机的 Node 26 |
| 现状·已运行验证 | 打包版 Magi.app 的 daemon | `npm run desktop:package -- --dir` 生成的 macOS arm64 目录包，直接启动包内 daemon，只使用随包 Node 与包内 `daemon/compat/`（不设 `MAGI_EXTENSION_*`），同一条链路 18 项全部通过；包内 `compat/` 文件数与源目录一致（13547） |
| 现状·已运行验证 | 打包版真实 Electron 界面 | `scripts/lib/plugin-upstream-acceptance.cjs`：设置 → 插件搜索关键词，核对 DSH 标注与发布者提示，预览展示权限与翻译结果，确认安装后列表以 Magi 插件 ID 出现、来源显示为"上游生态"、图标已加载、无重复的临时条目，共 12 项通过；截图在 `target/plugin-acceptance/upstream-*.png` |
| 现状·已运行验证 | `npm --prefix web run test:plugin-store` | 上游条目的归并：临时身份、按已装来源归并（含作用域包名）、不同生态同名不互相归并 |
| 现状·已运行验证 | 上游 5.19.1 | 上游把配置页换成新槽位 `settings.section`；翻译器原先正确拒绝了"没有可渲染槽位"的包，补入该槽位策略后翻译通过。翻译产物逐字节可复现（三次摘要相同） |
| 现状 | 未覆盖 | 打包后的 Magi.app 内的安装、Windows、pi 生态、第三方发布者包的覆盖面（仅验证了一个真实包） |

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
| 现状 | D10 | 不维护第二份翻译产物目录：用户安装上游官方 npm 包，翻译在本机安装期完成并以摘要核对；反方是每次预览都要下载并翻译（约数秒），信任信号（谁发布、是否审核）与摘要核对分开表达，不借 npm 元数据暗示审核 |
| 目标 | D11 | 不恢复工作流／会话引擎插件；反方是扩展面较窄，关键钩子与工具服务承载业务扩展 |
| 目标 | D12 | 全权限扩展不提供强制 OS 只读保证；要求此保证的环境不得组合启用，不能把工具过滤称作隔离 |
| 目标 | D13 | 低信任内容仅声明式包；反方是轻量后台也需完全信任，避免第二执行器 |

### 13.2 待定问题与推荐默认值

| 状态 | 项目 | 推荐默认值与理由 |
| --- | --- | --- |
| 待定 | 官方目录签名密钥、轮换和发布责任人 | 由发布者提供受控公钥；当前不伪造可信官方内容 |
| 待定 | Windows／正式安装包验收环境 | 后置到具备真实环境；能力合同保留，不把缺环境误报为通过 |
| 待定 | 翻译器的必需语义分母与一致性测试集 | 现状只有 `dsh-connect-workbuddy` 一个真实样本和一个夹具；扩大到更多真实包后再定分母，不先承诺生态兼容比例 |
| 待定 | 扩展调用上下文是否公开会话工作区目录 | 推荐只公开已授权的当前工作区路径（只读、随调用签发）；DSH 的 `/codebuddy` 等命令依赖它，现状只能在 `codebuddy_ask` 里显式传 `working_dir`。反方：路径属于会话事实，扩展拿到后可据此直接读写磁盘，但完全信任扩展本来就能读写磁盘，所以不新增权限面 |

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
