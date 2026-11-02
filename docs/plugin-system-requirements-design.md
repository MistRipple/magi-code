# Magi 插件系统：架构评估与目标设计

【目标】文档日期：2026-11-02。本文是终局设计，受信宿主、公开扩展协议和翻译器均未交付，不能当作可用接口手册；源码核对基于当日工作区（含未提交内容）。工程约束以 [AGENTS.md](../AGENTS.md) 为入口。

## 1. 结论、约定与需求

### 1.1 结论摘要

- 【目标】保留 Rust daemon 内核，复用现有安装、资源、授权与恢复基础；直接用受信 Node 宿主替换 QuickJS，统一一种可执行插件运行时。
- 【现状·读码推断】QuickJS SDK 只有资源、设置四个操作，无法独立满足文件、网络、进程及 agent 拦截需求，依据见第 2 节。
- 【现状·用户确认】QuickJS 插件目前尚未实际使用，不承担已投产插件兼容；这项使用现状由用户确认，源码接线不等于已有生产用户。
- 【目标】删除 QuickJS 执行、旧 SDK 兼容和双运行时路线；插件直接使用 Node API，不把宿主白名单代发包装成同等级扩展能力。
- 【目标】daemon 继续拥有会话、Turn、任务、审批和持久化；扩展仅通过公开接口提交业务提案。
- 【目标】上述所有权是协议合同，不能被描述为针对同用户全权限恶意代码的安全隔离。
- 【目标】每个启用的可执行插件独占一个 Node 进程，不提供共享进程模式；共享进程无法强制终止单个插件，依据与代价见 5.4。
- 【目标】支持 pi、DSH 的明确子集翻译；不得承诺原包通用或把上游会话内核带入 Magi。
- 【现状·文档摘要】pi 有官方包展示页，DSH 有官方按需目录；发现入口不等于第三方包经过安全认证，依据见第 3 节。
- 【目标】兼容来源开关只显示目录，逐个预检、安装、信任确认；关闭不隐式删除已安装包。
- 【目标】本文是终局设计：不设置兼容层、过渡运行时或临时补丁；实施阶段只表达依赖顺序，每阶段交付终局合同的完整纵向切片（第 11 节）。

### 1.2 状态、证据与规范用语

【目标】全文使用以下标记；一个标记约束其所在段、列表项或表格行的全部陈述。目标描述已定的规范合同，不得反向证明现有实现；本文不保留悬而未决的设计项，实施中必须变更合同时，直接修订本文相应章节与第 13 节决策记录。

| 标记 | 含义 |
| --- | --- |
| 现状 | 已重新核对的源码、文件或外部资料；另列证据类型 |
| 现状† | 沿用未复核，不得作为验收完成依据 |
| 目标 | 本文确定的设计约束，尚未实现的部分仍须实施与验收 |

【目标】审计证据分四类：**已运行验证**必须说明实际运行对象；**读码推断**只证明静态控制流与声明；**文档摘要**只描述所读资料；**静态核对**指行数、文件内容、链接与格式检查，只证明文档与源码一致，不证明运行行为。用户明确确认的使用现状另标“用户确认”，作为产品前提，不冒充读码或运行证据。没有行为缺陷被升级为“已运行验证”：daemon、Worker、Electron、pi 与 DSH 均未启动过。

【目标】“必须”“不得”是硬约束；“应”是默认做法，偏离须记录原因；“可”是允许但非必需。源码引用带文件行号，外部事实引用完整 URL。行号对应审计时的工作区，后续改动须重新定位。

| 状态 | 术语 | 定义 |
| --- | --- | --- |
| 目标 | 插件包 | 统一分发、身份、授权与升级单元，可组合不同贡献 |
| 现状·读码推断 | 待替换运行时 | 源码中的 QuickJS 受管 Worker，能力由旧 SDK 限定；现状和移除范围见第 7 节 |
| 目标 | 受信扩展 | 以用户权限运行的 Node 代码，拥有直接文件、网络、进程能力 |
| 目标 | 扩展宿主 | 执行受信代码的进程；生命周期归 daemon，进程拓扑见 5.4 |
| 目标 | ExtensionBus | daemon 内唯一扩展分发与裁决入口，不是第二个业务事件事实源 |
| 目标 | 提案 | 扩展返回的业务变更意图，daemon 校验并提交后才成为 Magi 事实 |
| 目标 | 操作系统直接副作用 | 扩展直接调用 Node API 的行为；Magi 无法保证完整拦截、审计或撤销 |
| 目标 | 翻译包 | 上游扩展、适配层及已构建依赖组成的 Magi 包，不包含第二个 agent 内核 |

### 1.3 产品需求与非目标

| 状态 | 编号 | 需求与可观察结果 |
| --- | --- | --- |
| 目标 | R1 | daemon 是会话、Turn、任务、审批、canonical 日志及持久化的唯一事实源 |
| 目标 | R2 | macOS、Windows 扩展功能一致；运行时随产品交付，隔离差异如实披露 |
| 目标 | R3 | 受信扩展能直接使用文件、网络、进程，并订阅、阻止和按合同改写 agent 行为 |
| 目标 | R4 | 原生 Skill、MCP、Subagent、Browser 保留原生执行；仅靠公开 API 可实现的功能才列为插件 |
| 目标 | R5 | 支持中心、地址、本地发布包和兼容来源；个人插件不必先上架 |
| 目标 | R6 | 支持工具、命令、设置、主页入口、页面、右栏面板、资源和可组合的声明式贡献 |
| 目标 | R7 | 页面与模型操作共享权威资源服务，数据持续同步，不等待整轮结束 |
| 目标 | R8 | 允许为新运行选择工作流核心，daemon 保留执行、授权、恢复与终态裁决 |
| 目标 | R9 | 安装、授权、启用、就绪、排空、取消、升级、卸载和崩溃有可验证状态 |
| 目标 | R10 | pi、DSH 翻译器纳入路线；设置提供“pi 兼容来源”“DSH 兼容来源” |
| 目标 | R11 | 保留用户自定义子代理角色及其模型绑定；扩展不得静默改换绑定模型 |
| 目标 | R12 | 无兼容补丁、影子实现、静默兜底；不支持的契约明确拒绝 |
| 目标 | R13 | API 可版本化演进；每项兼容声明对应固定上游版本、能力矩阵和一致性报告 |
| 目标 | R14 | 作者能在无私有接口的条件下开发、调试、诊断并分发真实扩展；可执行后台统一 Node，直接替换未投入使用的 QuickJS，不提供 SDK 1 兼容执行路径 |

【目标】非目标：替换 Magi 内核或用 Rust 复刻 DSH；直接加载未经适配的上游原包；开放任意 canonical 事件类型、审批结论、TaskStore 写入或内部对象；插件市场收费、云端执行与多人协同；包内原生可执行程序、动态库和 Node 原生模块；安装时运行第三方构建或安装脚本。产品自带 Node 运行时不属于插件携带原生程序。

## 2. 阶段 A：现状审计

### 2.1 清单、SDK 与贡献链路

| 状态与证据类型 | 核对项 | 实际支持与边界 | 证据 |
| --- | --- | --- | --- |
| 现状·读码推断 | 清单 | SDK 1；贡献为 skills、tools、views、browserSurfaces、commands、workflows、engines、resources；没有 hooks、mcpServers 或 provider 贡献 | [package.rs:201](../crates/magi-plugin-system/src/package.rs#L201)、[package.rs:280](../crates/magi-plugin-system/src/package.rs#L280) |
| 现状·读码推断 | 包入口 | 必须存在 plugin.mjs，即使仅贡献 Skill；上限 512KiB；视图必须引用 ui/ 下 HTML | [package.rs:109](../crates/magi-plugin-system/src/package.rs#L109) |
| 现状·读码推断 | SDK 全集 | resource.read、resource.write、settings.read、settings.write；其他操作拒绝。无文件、网络、进程、模型或工具调用 API | [plugin_bridge.rs:274](../crates/magi-daemon/src/daemon/plugin_bridge.rs#L274) |
| 现状·读码推断 | 工具 | 目录和执行共享 tools 权限判断；参数按 inputSchema 校验，再申请租约并调用 Worker | [lifecycle.rs:700](../crates/magi-plugin-system/src/lifecycle.rs#L700)、[plugin_bridge.rs:113](../crates/magi-daemon/src/daemon/plugin_bridge.rs#L113) |
| 现状·读码推断 | Skill | 不只存在清单字段；已将激活清单中的 Skill 投影进原生 SkillRegistry，无隐式工具提权 | [state.rs:5609](../crates/magi-api/src/state.rs#L5609) |
| 现状·读码推断 | 命令 | 进入命令执行器，结果只能是非空有界的 {content:string} | [command.rs:70](../crates/magi-plugin-system/src/command.rs#L70)、[command.rs:136](../crates/magi-plugin-system/src/command.rs#L136) |
| 现状·读码推断 | 工作流 | 类型化动作和独立核心工厂已存在；摘要不匹配拒绝恢复；不代表可替换 daemon 会话内核 | [workflow.rs:13](../crates/magi-plugin-system/src/workflow.rs#L13)、[workflow_factory.rs:94](../crates/magi-plugin-system/src/workflow_factory.rs#L94) |
| 现状·读码推断 | 会话引擎 | 激活、贡献存在性与 models 权限准入；仍无通用模型 SDK | [session_engine.rs:193](../crates/magi-plugin-system/src/session_engine.rs#L193) |
| 现状·读码推断 | Browser surface | 清单要求 browser 权限；激活只接受应用级、持久分区、保持挂载、关闭隐藏这一组合。未验证实际 guest 挂载链 | [package.rs:320](../crates/magi-plugin-system/src/package.rs#L320)、[lifecycle.rs:894](../crates/magi-plugin-system/src/lifecycle.rs#L894) |
| 现状·读码推断 | 资源同步 | SSE 在管理锁内获取快照并注册订阅边界；溢出发 reset；撤销通过周期复核结束订阅 | [plugins.rs:175](../crates/magi-api/src/routes/plugins.rs#L175)、[plugins.rs:219](../crates/magi-api/src/routes/plugins.rs#L219) |
| 现状·静态核对 | 官方内容 | plugins.json 为 []，catalog.json 的 plugins 为空；只有 board 示例，无可用官方扩展样本集 | [plugins.json:1](../plugin-center/plugins.json#L1)、[catalog.json:1](../plugin-center/catalog.json#L1)、[board 清单:1](../examples/plugins/board/manifest.json#L1) |
| 现状·读码推断 | 第一方插件 | 仅 magi.mcp-server；后台原样返回 input，监听、令牌、隧道、审计被注明为原生能力，不能作为公开 SDK 充足的证明 | [bundled_plugins.rs:9](../crates/magi-api/src/bundled_plugins.rs#L9)、[bundled_plugins.rs:23](../crates/magi-api/src/bundled_plugins.rs#L23) |

### 2.2 每种权限的实际强制点

【现状·读码推断】权限枚举共七种，见 [generated.rs:1518](../crates/magi-app-server-protocol/src/generated.rs#L1518)。通用“清单已授权”检查不等于存在相应能力的执行检查；以下“无”指本次检索未发现对应通用插件强制点，不代表插件已经能绕过 QuickJS 获得该能力。

| 状态与证据类型 | 种类 | 强制点与结论 |
| --- | --- | --- |
| 现状·读码推断 | storage | 有；资源及设置读写检查激活、声明和授权，[lifecycle.rs:296](../crates/magi-plugin-system/src/lifecycle.rs#L296)、[lifecycle.rs:370](../crates/magi-plugin-system/src/lifecycle.rs#L370) |
| 现状·读码推断 | tools | 有；本插件贡献的目录和执行准入，[lifecycle.rs:700](../crates/magi-plugin-system/src/lifecycle.rs#L700) |
| 现状·读码推断 | models | 有限；会话引擎准入，不是模型调用授权，[session_engine.rs:205](../crates/magi-plugin-system/src/session_engine.rs#L205) |
| 现状·读码推断 | browser | 有限；清单关联、激活授权及支持组合限制；不应宣传完整自动化能力，[package.rs:326](../crates/magi-plugin-system/src/package.rs#L326)、[lifecycle.rs:541](../crates/magi-plugin-system/src/lifecycle.rs#L541) |
| 现状·读码推断 | filesRead | 无；枚举存在，SDK 无文件入口；清单未拒绝该种类，[package.rs:244](../crates/magi-plugin-system/src/package.rs#L244)、[plugin_bridge.rs:290](../crates/magi-daemon/src/daemon/plugin_bridge.rs#L290) |
| 现状·读码推断 | filesWrite | 无；同上，不得把申请成功表述为可写文件 |
| 现状·读码推断 | network | 通用插件无；原生 MCP 壳声明了它，但没有对应 SDK 或网络目标执行检查，[bundled_plugins.rs:33](../crates/magi-api/src/bundled_plugins.rs#L33) |

### 2.3 视图与响应头

【现状·读码推断】iframe 使用 sandbox="allow-scripts"，无 allow-same-origin；宿主按精确 contentWindow、origin（资源 origin 或 null）、通道、instanceId、操作校验。允许六种消息：resource.read/write/subscribe、settings.read/write、context.select；不存在页面直接调用后台工具或命令的消息。实例 ID 在组件创建时生成，并非每次导航重新握手，不能宣称已经实现完整的导航代次隔离。[PluginTabContent.svelte:23](../web/src/components/tabs/PluginTabContent.svelte#L23)、[PluginTabContent.svelte:91](../web/src/components/tabs/PluginTabContent.svelte#L91)、[PluginTabContent.svelte:148](../web/src/components/tabs/PluginTabContent.svelte#L148)。

【现状·读码推断】资源响应 CSP 为 default-src 'none'；script/style 允许 self 和 unsafe-inline；img/font 允许 self 和 data:；object、base、form、connect 为 none；frame-ancestors 为 self，另有 nosniff。响应头**没有 sandbox**。直接导航到资源 URL 不受 iframe 属性保护，存在同源脚本风险；本次未进行浏览器利用验证。[plugins.rs:238](../crates/magi-api/src/routes/plugins.rs#L238)。

### 2.4 缺陷清单与处理顺序

【目标】严重度 P1 表示交付前应处理的安全或核心合同缺陷，P2 表示可用性、演进与诊断缺口，P3 表示表达和维护问题；不据静态审计声称已经发生攻击。此处优先级与第 11 节实施阶段编号是不同维度。

| 状态与依据类型 | 编号 | 严重度 | 位置与缺陷 | 建议顺序 |
| --- | --- | --- | --- | --- |
| 现状·读码推断 | G1 | P1 | [host.rs:173](../crates/magi-plugin-runtime/src/host.rs#L173) 的能力取消结算受 [host.rs:210](../crates/magi-plugin-runtime/src/host.rs#L210) 外层 biased select 抢先返回；处理器 future 可先被丢弃，100ms 通知结算路径未获保证。当前 SDK 主要同步存储，不能据此认定已存在脱离的网络副作用 | 随旧宿主替换消除该路径；新宿主必须回归处理器收到取消及结算顺序，不先扩建 QuickJS |
| 现状·已运行验证（响应头） | G2 | P1 | [plugins.rs:272](../crates/magi-api/src/routes/plugins.rs#L272) 缺响应级 sandbox，直接导航可能获得 daemon 同源身份 | 响应头已补 `sandbox allow-scripts`，单元回归 `view_resources_carry_a_response_level_sandbox` 通过；真实浏览器直接导航验证仍待做，在此之前不标为已验证修复 |
| 现状·已运行验证（单元回归） | G3 | P1 | 已修：协议枚举删除 `filesRead`/`filesWrite`/`network`，清单声明它们在校验阶段被拒绝（`permissions_without_an_enforcement_point_are_rejected`）；作者指南已同步。受信宿主的 Magi 服务授权仍待新清单版本定案 | 已落地；旧已安装包若声明这些权限将无法加载，SDK 1 未投产，按设计不保留转换层 |
| 现状·已运行验证（单元回归，部分） | G4 | P2 | 已修：权限与 Browser surface 的 `scope` 枚举只剩 `application`，示例 board 清单改为 application。**未完成**：生命周期中按 scope 字符串索引的 `enabled_scopes`/`active_scopes`、资源键、排空通配符与 API `scope` 参数仍在（约两百处，涉及持久状态与线协议），并入 P1 的新清单与状态 schema 一次性替换，不在 QuickJS 生命周期上单独改 | 状态与 API 的 scope 参数收敛随 Node 宿主批次 |
| 现状·已运行验证（单元回归） | G5 | P1 | 已修：中心安装必须携带目录条目的 `expectedSha256`，目录变化返回 409；普通安装与升级必须携带预览摘要（缺失返回 400）；`magi-api` 路由测试覆盖。内联包与本地包同样要求先预览 | 已落地；Electron 验收脚本已改为先预览再安装，未在本轮运行 |
| 现状·已运行验证（单元回归） | G6 | P1 | 已修：唯一出站入口 [download.rs](../crates/magi-api/src/routes/plugins/download.rs)——仅 HTTPS（回环地址可 HTTP）、解析出的每个地址须为公网或回环（拒绝私网、链路本地含元数据地址、共享地址、ULA）、重定向逐跳手动跟随并重新校验、禁止 HTTPS 降级、解析结果固定到连接以防 DNS 重绑定。内网来源改用本地文件安装 | 已落地；未在真实公网 DNS 重绑定场景验证 |
| 现状·读码推断 | G7 | P2 | [host.rs:74](../crates/magi-plugin-runtime/src/host.rs#L74)、[host.rs:102](../crates/magi-plugin-runtime/src/host.rs#L102) 单宿主共用 4 个名额，排队计入超时，无每插件公平性或独立决策通道 | 基础设施批次，先验证饥饿场景 |
| 现状·读码推断 | G8 | P2 | [worker.rs:107](../crates/magi-plugin-runtime/src/worker.rs#L107)、[host.rs:116](../crates/magi-plugin-runtime/src/host.rs#L116) 脚本异常折叠、stderr 丢弃；作者无法得到栈与源码位置 | 基础设施批次，日志有界、按插件隔离并脱敏 |
| 现状·已运行验证（单元回归，部分） | G9 | P1 | 已修：目录支持 `sequence`（防回滚，按地址持久化已见最高序号）、`expiresAt`（过期拒绝）、`revoked`（撤销后不展示也不可按版本安装）；`verified` 只来自受信公钥验签，官方地址不再自动视为已验证；发布脚本支持 `--expires-in-days`、`--revoked`。**未完成**：官方发布公钥尚未提供（`OFFICIAL_CENTER_KEYS` 为空，官方目录当前按未验证对待），release 资产发布流程待建 | 需发布者生成密钥并写入常量后，才能把官方目录升级为签名信封 |
| 现状·读码推断 | G10 | P2 | [plugins.rs:243](../crates/magi-api/src/routes/plugins.rs#L243)、[plugins.rs:752](../crates/magi-api/src/routes/plugins.rs#L752)、[plugin_bridge.rs:276](../crates/magi-daemon/src/daemon/plugin_bridge.rs#L276) 安装事实、同步存储、SDK、资源克隆共用同步管理锁 | 基础设施批次，缩短持锁与每插件串行化；未测量为性能瓶颈 |
| 现状·已运行验证（单元回归） | G11 | P2 | 已修：删除 `bundled_plugins`、`/plugins/bundled*` 路由与前端内置插件列表；MCP 服务是原生功能，只由自己的开关控制，设置里的 MCP 分类始终可见；`BUILTIN_PLUGIN_IDS` 清空，旧版遗留的内置记录在打开时退役（备份后移除），退役前未激活的则关闭持久开关，不让升级悄悄开启监听 | 已落地；Electron 验收脚本已改，未在本轮运行 |
| 现状·读码推断 | G12 | P2 | [plugin_bridge.rs:139](../crates/magi-daemon/src/daemon/plugin_bridge.rs#L139) 工具身份的 turn_id/attempt_id 为空、instance_id 是固定 scope；[PluginTabContent.svelte:23](../web/src/components/tabs/PluginTabContent.svelte#L23) 无导航换代；现有取消和实例标识不能直接充当受信宿主完整调用身份 | 受信协议前置批次，补真实代次与执行身份 |
| 现状·读码推断 | G13 | P3 | [plugin-authoring.md:23](plugin-authoring.md#L23) 把 Skill 一并写成“不支持声明”；实际已有 [state.rs:5609](../crates/magi-api/src/state.rs#L5609) 接线 | 作者指南顶部已有状态说明；正文随 Node 公开 API 发布时整体重写（D1） |
| 现状·读码推断 | G14 | P1 | [plugin_bridge.rs:128](../crates/magi-daemon/src/daemon/plugin_bridge.rs#L128) 为插件工具新建独立取消令牌；[registry.rs:106](../crates/magi-tool-runtime/src/registry.rs#L106) 的调用未传入原任务取消，且 [blocking.rs:19](../crates/magi-plugin-system/src/blocking.rs#L19) 同步等待线程结束。原任务停止不会经此链通知 Worker；宿主总时限及 [drain.rs:252](../crates/magi-plugin-system/src/lifecycle/drain.rs#L252) 的停用取消仍存在，不能说调用永不结束 | 第一批，与 G1 分别回归任务取消传播、停止后 SDK 写入准入和有界结算 |

【现状·读码推断】Windows Worker 的操作系统沙箱缺失是当前明确取舍，不伪装成已解决缺陷；见 7.3。受信宿主尚不存在，其进程拓扑、失败策略和替换接线是**目标设计缺口**，不混入已实现缺陷清单。

## 3. 阶段 B：上游对标与自有体系判断

### 3.1 上游机制

| 状态与证据类型 | 维度 | pi | DSH |
| --- | --- | --- | --- |
| 现状·文档摘要 | 加载 | TypeScript/JavaScript 默认工厂，jiti 加载，支持异步工厂、显式路径及受信项目发现，[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md) | Cordis 插件经 profile、bundle、patch 组合；注入服务，注册是可撤销 effect，[架构](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/architecture.md)、[Cordis](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/cordis-primer.md) |
| 现状·文档摘要 | 信任 | 扩展与 pi 同进程、同 OS 权限，明确可读凭据和历史；项目本地动态内容先过项目信任，[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md) | 安装的 Host 代码在进程内、工作区沙箱外执行；安装脚本另有批准机制；安全声明不承诺隔离或生产安全，[插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md)、[安全说明](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/SAFETY.md) |
| 现状·文档摘要 | 事件 | 生命周期、输入、提示、上下文、压缩、模型传输、工具调用/结果；tool_call 改参后明确**不重新校验**；tool_call 异常阻止调用，[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md#tool-events) | Session 持久事件、agent 运行事件、能力事件分域；waterfall 是带 next 的环绕中间件，不等同普通结果折叠，[架构](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/architecture.md)、[Cordis](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/cordis-primer.md) |
| 现状·文档摘要 | 注册与 UI | 工具、命令、provider、虚拟模型、MCP、渲染器、快捷键、状态与 TUI 自定义组件；RPC 只支持部分 UI，[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md) | 服务、工具、命令、设置、模型、客户端模块；Remote 声明和生成物依赖 Typert Host/Client 组合，不是一个通用可挂载网页协议，[架构](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/architecture.md)、[API Gateway](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/api-gateway.md) |
| 现状·文档摘要 | 分发 | npm/git/local；宿主提供特定模块实例；官方 [Package Catalog](https://pi.dev/packages) 展示 npm 包，[包文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/packages.md) | npm/pnpm、git、路径与 bundle；有随安装交付的官方按需清单，官方目标随 DSH 版本；第三方可借 [dsh-plugin topic](https://github.com/topics/dsh-plugin) 发现，[插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md)、[README](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/README.md) |
| 现状·文档摘要 | 稳定性 | 所读扩展/包文档没有独立的扩展 ABI 长期兼容承诺；不能从它推导“永不破坏” | README 明示 developer preview 和破坏性变更；同时已有 engines.dsh、peer 要求及精确版本豁免。存在版本检查不等于长期稳定承诺，[README](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/README.md)、[插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md) |

【现状·文档摘要】DSH AgentRegistry 与 agent loop 分离，Session 是可扩展事件日志及消息投影的事实源；依赖这些对象、纯消息投影或持久化解释器的插件，不是换几个事件名称即可移植。[Agent](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent/README.md)、[Agent loop](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent-loop/README.md)、[Session](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/subsystems/session.md)。

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
| 目标 | 能否直接运行 DSH 插件 | 在现有 QuickJS 中不能。Node 只解决语言执行，还须相同 Cordis 服务实例、事件模式、包导出、生命周期、Agent/Session 对象及版本。仅依赖少数服务的插件可在适配宿主执行，但这已是兼容子集。反方：嵌入完整 DSH 更快得到上游行为；代价是第二个内核及事实源，违反 R1 与非目标 |
| 目标 | 工具/命令/设置兼容是否要改内核 | 不必替换内核；公开注册、表单与执行适配足够，但需实现实际 API、schema 和 effect 回收。反方：第三方包可能间接依赖其他服务，不能只按名称认定可翻译 |
| 目标 | 事件拦截兼容是否要改内核 | 需要在 Magi 现有调用链增加公开钩子，不必更换事实源。顺序、阻断、环绕、重入、并发不同必须报告；无法保持必要语义就拒绝。反方：极复杂 waterfall 可能比重用上游难，故只承诺验证过的子集 |
| 目标 | 替换 loop/SessionPersistence 是否要改内核 | 原样支持需要把上游驱动、日志和对象语义带进来，超出边界；只可把业务决策重写为 Magi 工作流提案。反方：失去“一切皆插件”的完整性；这是保留自有内核的明确代价 |
| 目标 | 照 DSH 设计重写能否通用 | 不能保证。设计相似不等于依赖同一份实现、模块实例、异常/取消/微任务、事件对象、持久格式及版本；Rust 重写还需 JS ABI 和行为适配。反方：相同概念能减少映射成本，但不能消除合同验证 |

### 4.2 路径成本、放弃项与可回退性

【现状·静态核对】统计 src/**/*.rs，包含空行和内联测试，不含 tests/：magi-plugin-system 15 文件、5,037 行；magi-plugin-runtime 6 文件、931 行；插件主路由 1,457 行、中心 242 行、daemon 桥 403 行。受影响核心区域的量级是 conversation-runtime 59 文件、87,274 行，session-store 23 文件、30,083 行，orchestrator 5 文件、8,695 行。这些是现有模块规模，**不是预计改动行数**。依据：[插件系统目录](../crates/magi-plugin-system/src)、[运行时目录](../crates/magi-plugin-runtime/src)、[对话目录](../crates/magi-conversation-runtime/src)、[存储目录](../crates/magi-session-store/src)、[编排目录](../crates/magi-orchestrator/src)。

| 状态 | 路径 | 模块与量级依据 | 放弃项、风险与可回退性 | 最强反方理由 |
| --- | --- | --- | --- | --- |
| 目标 | 仅扩 QuickJS 白名单，否决 | 主要修改现有约 6 千行插件模块与桥 | 放弃直接 OS 能力，不满足 R3；代码易停用，产品目标无法验收 | 复用已有受限执行边界，避免增加 Node 交付依赖 |
| 目标 | Node 直接替换＋自有公开 API＋适配层，已确定 | 重构上述运行时与桥；新增 Node 协议、SDK、适配构建工具；同批移除 QuickJS 依赖、调用及发行接线，第 7 节逐项列出；接入第 5.6 节核心入口 | 放弃 SDK 1 兼容、原包全兼容和任意内核替换；新模块行数未核实、不估造。受信后台停用后原生功能仍可运行；不撤销外部副作用，不以复活 QuickJS 作为故障回退 | Node 分发和适配维护仍有成本；不再承担两种 Magi 后台的长期维护 |
| 目标 | 完整 DSH 与 Magi 并存，否决 | 新增完整 Node 应用树及 Magi↔DSH 会话/任务/UI 桥；会触及上述三个核心域，不是小插件运行时 | 出现双事实源、授权与恢复对账；迁入历史后难回退，违反 R1 | 最大限度重用真实上游代码 |
| 目标 | Rust 复刻 DSH，否决 | 要重做上述核心域，并补 Cordis/JS 对象语义、持久日志解释器与客户端合同 | 放弃现有架构和稳定性，仍无法天然通用；需整体状态迁移及备份恢复 | 有机会统一概念，但与本任务保留内核的硬约束相冲突 |
| 目标 | 独立 DSH 外部执行器，不属于本设计 | 使用预留执行器合同，另带 DSH 进程与历史证据适配 | 可提供另一种 agent 运行，不能宣称是 Magi 插件兼容；外部执行器当前冻结，[演进方案第 10 节](magi-agent-runtime-evolution-plan.md#10-插件guiweb-和预留接口的边界) | 需要完整 DSH 行为的用户可能更适合此路；须另立产品决策 |

### 4.3 什么算真正可用

【目标】以下全部通过才可宣称受信插件可用，不以“能加载 JS”或“能回显”代替：

1. 作者仅用公开 API 和 Node API，实现工具/命令、文件或远端集成、进程执行及实际 agent 拦截，不修改 Magi 源码。
2. 同一插件能读取真实工具调用，阻止危险调用，改写参数后让 daemon 对最终参数重新校验；异常不会静默跨过已启用的阻断规则。
3. 数据型插件能由 UI 和模型修改同一资源，持续同步、明确版本冲突；无 UI 时仍可返回结构化结果。
4. 子代理扩展按已配置角色进行路由，模型解析沿原生绑定，显式绑定不可用时明确失败。
5. 干净 macOS、Windows 正式安装包不依赖用户安装 Node/npm，即可安装、启用、诊断、取消、停用和升级样本。
6. 死循环、同步阻塞、崩溃、超时、迟到响应、升级中断和撤权回归通过；Magi 不自动重放未知副作用。
7. 至少一个独立作者或独立作者视角的样本使用相同公开包和接口；第一方不能以私有桥冒充成功。
8. 翻译包给出固定版本、完整能力差异与测试报告；未支持功能明确拒绝，不能静默空实现。

## 5. 阶段 D：目标架构

### 5.1 daemon 与 Node 宿主的职责分层

| 状态 | 维度 | daemon 业务权威层 | Node 扩展执行层 |
| --- | --- | --- | --- |
| 目标 | 执行 | 原生 Rust 业务服务、插件管理与 ExtensionBus | 随产品提供的 Node 进程，按插件激活保活；唯一可执行插件运行时 |
| 目标 | 能力 | 工具、模型、子任务、审批、资源及类型化工作流动作的公开服务 | 直接 Node 文件、网络、进程；公开钩子、注册和业务提案 |
| 目标 | 裁决 | 校验最终参数、授权、持久事实与恢复 | 执行扩展代码、生成提案；不能经 API 写入权威事实 |
| 目标 | 信任 | 管理服务与注册准入，不能限制同用户 Node 的全部 OS 行为 | 全权限，不沙箱；独立进程用于故障回收 |
| 目标 | 声明式包 | 直接处理 Skill/MCP 等声明，不创建空后台；MCP 连接沿原生确认 | 无后台的包不启动 Node、不要求后台完全信任；一旦启用可执行后台则明确确认信任 |

【目标】结构为“统一插件管理服务 → ExtensionBus／公开业务服务 ↔ Node 宿主”。两层分别负责业务权威与扩展代码执行；后台统一 Node，不保留 QuickJS 分支或运行时选择开关。ExtensionBus 通过注入的公开端口接入 conversation-runtime 等领域模块，业务模块不得反向依赖 daemon 装配模块。

### 5.2 所有者表

| 状态 | 状态或行为 | 唯一所有者 |
| --- | --- | --- |
| 目标 | 包、来源、版本、摘要、启用意愿、授权、就绪 | daemon 插件管理服务 |
| 目标 | 宿主进程、注册租约、调用代次、超时、熔断 | daemon，统一经 ExtensionBus |
| 目标 | Session、Turn、任务树、审批、canonical 事件、用量与恢复 | 现有 daemon 领域服务及原有存储边界 |
| 目标 | 下一步编排决策 | 当前运行固定的工作流实现；daemon 执行动作 |
| 目标 | 角色和模型绑定、目录授权与执行映射 | 原生角色/模型配置、TaskStore 与项目目录服务；扩展不可自报 |
| 目标 | 插件资源、设置、会话内私有记录 | daemon 命名空间存储；插件定义值的语义，不另建会话日志 |
| 目标 | 文件/远端业务数据 | 指定原始系统；插件资源缓存明确为派生值 |
| 目标 | 逻辑 Browser 与物理 guest | BrowserAuthority、Electron Main |
| 目标 | 布局、焦点、活动面板 | 现有工作台；视图仅拥有滚动、选中等展示状态 |
| 目标 | OS 直接副作用 | 受信插件承担行为责任；daemon 只能记录经公开接口观察到的事实，不保证掌握全部行为 |

### 5.3 不变量

| 状态 | 编号 | 合同 |
| --- | --- | --- |
| 目标 | I1 | 公开 API 不允许扩展写 canonical 日志、TaskStore、工具成功事实或审批结果；daemon 提交事实 |
| 目标 | I2 | 扩展能阻止，不能批准；不能把 deny 改成 allow，也不能代替用户答复审批 |
| 目标 | I3 | 所有改写完成后，对最终 schema、工具可见性、目录映射、并发分类、权限和审批重新裁决；不得复用原参数批准凭据 |
| 目标 | I4 | 钩子只经 ExtensionBus；一个状态一种写入入口；不能把观察回调变成隐藏的第二个编排器 |
| 目标 | I5 | 清单声明必须有路由/注册强制点；不保留无法执行的装饰性文件或网络权限 |
| 目标 | I6 | 第一方、第三方、兼容层使用同一公开 API；原生功能不得借占位包列为插件 |
| 目标 | I7 | Magi 安装不执行第三方代码、不运行 npm install 或安装后脚本；激活代码执行与安装分开 |
| 目标 | I8 | 每个扩展有可失效的实例代次与租约；取消后拒绝新动作和迟到交付，结算与审计仍进行 |
| 目标 | I9 | 扩展故障不得使 daemon 等待无界；阻断规则失效时停止受影响动作，不能默认当作无扩展继续 |
| 目标 | I10 | 同一插件 ID 同时只激活一个后台版本；旧运行不交给新版本或默认核心猜测恢复 |
| 目标 | I11 | 权限撤销优先于排空租约；已发生副作用不被声称已撤销，结果未知明确标记 |
| 目标 | I12 | 角色先选、模型按绑定解析；扩展不能通过钩子偷偷覆盖用户的角色模型选择 |
| 目标 | I13 | 提示贡献走同一来源、IR、预算和缓存谱系；不删除原生必需规则，不伪造用户或 system 权威 |
| 目标 | I14 | 不支持的 API/协议明确拒绝；SDK 1 包不进入 Node 执行，不自动回退 QuickJS、默认工作流或另一套内核 |
| 目标 | I15 | 上述合同约束 Magi 管理的接口。受信 Node 直接 OS 调用不能被伪称为均已审批、均可取消、均被审计；恶意同用户代码不在安全保证内。有全权限后台运行时不得承诺强制 OS 只读；仅不向某 Task 分发钩子也不能隔离已激活后台，模式组合见 D12 |

### 5.4 扩展宿主、通信与运行时交付

| 状态 | 项 | 设计与最强反方理由 |
| --- | --- | --- |
| 目标 | 进程拓扑 | 每个已启用的可执行插件独占一个 Node 进程，其适配层与它同进程；不提供多插件共享进程或“信任组”模式。独立进程使 daemon 能强制终止单个阻塞插件，但不能阻止全权限代码伤害其他进程。激活按清单声明的触发条件（启动、钩子、命令、工具、计划作业）按需拉起，没有常驻理由的插件空闲回收；声明了 guard 型钩子或订阅的插件保持常驻。反方：多进程带来内存与启动成本，用按需激活与空闲回收控制，不以共享进程换取 |
| 目标 | 生命周期 | daemon 拥有启动、握手、就绪、停止、重启意愿与进程组；沿 ManagedProcessGroup 的 Unix 进程组/Windows Job Object 接线。[lib.rs:208](../crates/magi-process/src/lib.rs#L208)、[lib.rs:235](../crates/magi-process/src/lib.rs#L235) 是现有可复用基线，不是新宿主已实现 |
| 目标 | Node 交付 | daemon 直接启动产品随包的固定版本 Node，Desktop 与无头入口使用同一合同；更新、安全修复、许可证与签名随产品发布，不依赖用户 PATH。插件宿主不使用 Electron utilityProcess：[automation-worker.ts:116](../apps/desktop/src/main/automation-worker.ts#L116) 的机制由 Main 管理生命周期，不在 daemon 的进程组内，会形成第二套监管 |
| 目标 | 通信 | Unix 域套接字／Windows 命名管道，用户访问控制、一次性握手凭据、协议版本与实例代次；限制帧、队列、挂起请求和输出。参考 [local_socket.rs:290](../crates/magi-mcp-server/src/local_socket.rs#L290)、[local_socket.rs:338](../crates/magi-mcp-server/src/local_socket.rs#L338)，不得直接继承 MCP 令牌权限 |
| 目标 | 故障与资源 | daemon 使用独立计时与有界队列，不依赖 Node event loop 自报超时；每插件并发/队列/输出上限及全局公平调度。超时先取消，有限结算后终止所属宿主；心跳、重启次数与退避有界。内存监测与平台硬限制须分别记录，不能把 V8 堆上限称为进程总内存限制 |
| 目标 | 资源回收 | 合作式注册、订阅、UI、文件监听、子进程均登记 disposer；生命周期结束失效 daemon 句柄并回收受管树。全权限插件可绕过登记或派生脱离进程，不能保证技术性彻底清理 |

### 5.5 公开协议与版本演进

【目标】Extension Protocol 与扩展 API、包格式、资源 schema 分开版本化；线协议的帧、握手、方法与逐钩子 schema 见[扩展协议 v1](extension-protocol-v1.md)。握手声明 protocolVersion、extensionApi、宿主运行时能力与必需功能；不支持的主契约或必需功能拒绝加载。兼容的小版本新增只能在双方声明支持时使用；不靠缺字段猜旧协议，不维护第二条执行实现。版本规则：manifestVersion（包格式）、extensionApi、protocol、数据 schema 是四条独立版本轴，各为主.次两段；主版本不同即拒绝，次版本只增不删且须双方声明才可使用。宿主只接受其发布时的当前主版本，其余明确拒绝，作者按新主版本重新发布。后台入口固定为包内 extension/index.mjs，入口产物至多 8MiB，仍受 32MiB 展开总量约束；这些是宿主策略上限，不由清单自抬。

【目标】daemon 签发插件 ID、精确版本、摘要、实例代次、调用 ID、hook ID、deadline、配置快照；有执行归属时附 Session、Turn、Task、Step、attempt、目录绑定与父调用。缺少归属的后台任务要先取得原生任务或插件作业身份，不冒充用户输入。

【目标】每次分发冻结扩展集合与顺序。ExtensionBus 向独立插件逐项调用、逐项校验并保存可解释的裁决轨迹，不要求所有扩展折成一条不可归因结果。阻止结果不能被后续插件撤销；变更依次作用于最新候选，最终仍须原生校验。观察通知使用有界异步队列，不阻塞 token 流；丢失时提供可重读事实的游标或明确缺口，不伪造完整送达。

【目标】调用顺序由用户在插件管理中配置的有序列表决定，未排序的插件按插件 ID 稳定排序；清单不能自抬优先级。每 Step 冻结配置与调用集合，新注册影响下一边界；撤销立即生效；工作流版本按整个运行固定。同一钩子点的总期限取各插件 deadline 之和与该类上限中的较小者，超限后的插件按其模式处理（guard 阻止，advise 与 observe 跳过并记录）。重入深度上限为 4 层，超过或检测到环路即拒绝该请求。

【目标】扩展在钩子中调用模型、工具或派发子任务时，仍进入原生服务并携带父调用。协议必须定义可重入操作、递归深度和循环检测；禁止等待当前被阻塞动作的完成，避免“工具前钩子等该工具结束”的死锁。状态型命令使用幂等请求 ID；超时不自动重试外部写入。

### 5.6 钩子目录与实际调用链

【现状·读码推断】核对的是可接入的原生链路，**没有发现已公开的 ExtensionBus 或下表插件钩子**。落点的函数名代表拟插入位置，不是已存在的扩展 API。核对范围为 Magi 原生主线、task/worker、工作流、外部工具路径；没有把预留外部执行器内部事件当成可拦截事件。

| 状态 | 候选钩子 | 类别与结果语义 | 已核对链路与拟接线边界 | 缓存影响 |
| --- | --- | --- | --- | --- |
| 目标 | session.created / session.deleted | 观察，不能否决已提交事实 | [sessions.rs:263](../crates/magi-api/src/routes/sessions.rs#L263)、[sessions.rs:572](../crates/magi-api/src/routes/sessions.rs#L572) 在成功创建/删除后发目录事件；不能把删除等同 pi 的 session_shutdown。后台恢复创建和 attach/detach 的统一通知仍需补覆盖 | 无；观察后提交的上下文按贡献规则计入 |
| 目标 | turn.accepted / turn.end | 观察已接纳／已结算事实，不追加隐藏续跑 | [TurnService:74](../crates/magi-api/src/turn_service.rs#L74) → [task_dispatch:446](../crates/magi-api/src/task_dispatch.rs#L446) → [accept_dispatch_submission:1667](../crates/magi-conversation-runtime/src/dispatch_submission.rs#L1667) 的 canonical sink；终态走 [session_turn_finalize:925](../crates/magi-conversation-runtime/src/session_turn_finalize.rs#L925)。纯对话终态另在 session_turn_execution，接线须覆盖两类 profile | 无 |
| 目标 | input | 拦截：继续、转换文本/附件、明确接管 | [turn_service.rs:85](../crates/magi-api/src/turn_service.rs#L85) 统一接纳，校验/身份/指纹从 101 行起生成；原始输入先保留，扩展转换另外记录，最终引用重新校验。队列、steer、重放必须共用一次处理身份，不在 HTTP 私加第二个入口 | 转换后输入追加；不能改写旧用户事实 |
| 现状·部分实现 | prompt.contribute | 贡献：带来源、稳定性和预算的提示片段（任务主线已接入 `start`/`round` 两阶段与唯一接纳点，见[扩展协议](extension-protocol-v1.md)；纯会话入口未接） | task：dispatcher 产出 → [conversation_loop:825](../crates/magi-conversation-runtime/src/conversation_loop.rs#L825) 装静态片段；conversation：[build_session_turn_messages_with_runtime:490](../crates/magi-conversation-runtime/src/session_turn_execution.rs#L490)。两入口注入同一贡献解析服务，再进入实际请求 IR | 稳定片段改变使前缀失效；易变放尾部 |
| 目标 | context.assemble | 拦截：仅在扩展可控上下文段增删重排；不可破坏原生约束和工具配对 | [ContextAuthority::prepare:357](../crates/magi-conversation-runtime/src/context_authority.rs#L357) 被 [conversation_loop:1136](../crates/magi-conversation-runtime/src/conversation_loop.rs#L1136)、[session_turn_execution:752](../crates/magi-conversation-runtime/src/session_turn_execution.rs#L752) 使用；预算前接纳贡献，最终装配后、IR 前复核，无绕过预算的消息体 | 历史改写需显式原因与代际；不得只标稳定就绕过 |
| 目标 | model.request | 观察＋受限参数提案，不换角色绑定或裸 provider body | task：[conversation_loop:1761](../crates/magi-conversation-runtime/src/conversation_loop.rs#L1761) → StepModelClient；conversation：[session_turn_execution:1942](../crates/magi-conversation-runtime/src/session_turn_execution.rs#L1942) → 同一 [invoke_recorded:195](../crates/magi-conversation-runtime/src/model_step.rs#L195)。最终提案必须在 cache_prompt_ir（241 行）、指纹与 begin_model_step（390 行）前校验/冻结 | 从最终实际请求计算，不能在指纹后再改 |
| 目标 | model.response | 观察已结算结果；不提供正文替换 | [model_step:421](../crates/magi-conversation-runtime/src/model_step.rs#L421) invoke → settle_model_step → 迟到响应检查 → response 校验；流式正文可能早已交付，不能靠末端改写假装未展示 | 观察无 |
| 目标 | tool.call | 拦截：继续、阻止、改写参数；不改审批结论 | [conversation_loop:3178](../crates/magi-conversation-runtime/src/conversation_loop.rs#L3178) → [execute_task_tool_call_batch:210](../crates/magi-conversation-runtime/src/tool_batch.rs#L210) → ledger（284 行）→ 并发分组（363 行）→ 单工具 preflight（2800 行）→ registry（3211 行）；**改参须在账本指纹、路径和并发分类之前**，最终再过权限与审批。工作流经 [workflow_execution:597](../crates/magi-conversation-runtime/src/task_execution_dispatcher/workflow_execution.rs#L597)，外部工具经 [external_tool:248](../crates/magi-conversation-runtime/src/external_tool.rs#L248)，统一接同一管线 | 改参/阻止产生新执行回执，不回写旧模型调用 |
| 目标 | tool.result | 拦截：只改模型可见投影，不改执行状态、审批、实际用量 | 原始执行在 [tool_batch:404](../crates/magi-conversation-runtime/src/tool_batch.rs#L404) 返回；主循环 [conversation_loop:3225](../crates/magi-conversation-runtime/src/conversation_loop.rs#L3225) 消费。必须先固定原始回执，后接投影；工作流、外部调用应显式选择是否需要模型投影，不能覆盖审计结果 | 追加结果通常保前缀；修改已发结果按前缀改写处理 |
| 目标 | compaction.before | 拦截：继续、取消、自定义摘要提案；不能自定压缩范围或升为 system | [ContextAuthority::prepare](../crates/magi-conversation-runtime/src/context_authority.rs#L357) → [compact_one_pass:950](../crates/magi-conversation-runtime/src/context_authority.rs#L950) → build_compaction_summary（1065 行）→ validate；daemon 已决定范围、配对与预算后请求摘要，再由原检查点路径提交。manual/threshold/overflow 共用该权威 | 历史代次与 epoch 变化；取消后超预算则停住，不强行继续 |
| 目标 | subagent.spawn | 拦截：选择允许角色、收窄已授权参数或拒绝 | [tool_batch::execute_agent_spawn:755](../crates/magi-conversation-runtime/src/tool_batch.rs#L755) → [preflight_agent_spawn:167](../crates/magi-conversation-runtime/src/agent_spawn_preflight.rs#L167) → role 校验（238 行）→ 模型绑定（389 行）→ TaskExecutionRegistry；提案在完整 preflight 前，之后重新预检，不独立创建 Task | 角色/工具面影响子分支前缀；worker 使用自身 lineage |
| 目标 | model.resolved | 观察最终模型身份，不覆盖用户绑定 | [model_config::configured_role_engine_model_config:650](../crates/magi-conversation-runtime/src/model_config.rs#L650) → [preflight_agent_model:776](../crates/magi-conversation-runtime/src/agent_spawn_preflight.rs#L776)；主线配置经 resolve_orchestrator_model_config，StepModelClient 冻结绑定。没有核实一个覆盖所有设置写入的 model.select 事件，因此不伪造该现状 | 不同模型/提供方按实际缓存合同断代 |
| 目标 | usage.update | 观察已提交用量，不能改账单事实 | [model_step:442](../crates/magi-conversation-runtime/src/model_step.rs#L442) 结算、[usage_recording:481](../crates/magi-conversation-runtime/src/usage_recording.rs#L481) 的原生发布入口；先提交后观察，不从插件自报文本伪造供应商 usage | 无 |

【目标】每个钩子的协议评审还必须列：接纳前后顺序、输入/输出 schema、执行归属、取消点、最大大小、总期限、重入规则、失败语义、审计保存位置、恢复时是否重发以及各调用方覆盖。表中落点不替代这些实现验收；尚未统一的 attach、队列或外部工具接线须标为缺口，不能宣称全部链路已打通。

### 5.7 注册、服务与提案裁决

【目标】清单给出允许贡献；激活时通过公开 API 绑定处理器，注册进入现有工具、命令、Skill、模型或 MCP 治理入口。不得动态注册未声明的工具或覆盖原生同名身份。动态远端目录必须有独立、可强制的注册范围和用户确认合同，未定案前拒绝，不用通配声明绕开差异授权。

【目标】服务包含会话只读投影、带归属的模型调用、原生工具调用、子任务派发、通知/用户问题、插件资源/设置和凭据引用。模型、工具、子任务服务不是受信扩展访问 OS 的唯一途径；其价值是复用 Magi 的任务、审计、用量和取消，而非装饰性权限。当前设置和资源不是秘密存储，目标凭据服务不得把 secret 写进视图或日志；同用户受信代码仍可能直接读取凭据。

【目标】裁决链为：冻结调用与扩展身份 → 逐项提案及 schema 校验 → 原生策略校验 → 固定最终参数与摘要 → 用户审批（如需）→ 持久记录执行意图 → 真执行 → 固定原始结果 → 模型投影。已 deny 的动作终止，不允许修改成另一组参数后复用 deny 前的批准；新的意图须新接纳。所有重入使用最终参数重新检查，严格保持既有撤销约束。

【目标】输入接管、自定义摘要、结果替换均有来源标记；原始用户输入和原始工具事实保留。提案与裁决由原生持久化边界记录，不由扩展写日志。记录有界、脱敏；敏感正文不复制进第二份审计数据库。

【目标】后台工作（计划任务、事件触发、长时命令）必须持有 daemon 签发的插件作业身份：作业记录归属的插件与摘要、触发来源、用户授权的用量预算、取消句柄与结果回执，由 daemon 持久化。作业内的模型调用经原生服务并计入该作业的用量；没有作业或 Turn 身份的模型调用一律拒绝。持久定时由 daemon 的原生调度服务唯一拥有，插件只声明计划。Node 直接访问外部模型接口产生的费用在 Magi 核算之外，界面必须如实说明。

### 5.8 超时、失败与取消

【目标】每个钩子注册在清单中声明模式：observe（只读通知，失败只记录）、advise（可提出提案，失败时保留原生行为并提示）、guard（必经规则，失败即阻止）。模式随授权展示；升级改变模式或新增 guard 视为新增能力，须重新授权。

| 状态 | 类别 | 默认行为 |
| --- | --- | --- |
| 目标 | 观察通知 | 失败记录并继续；队列满报告缺口，必要时读权威快照 |
| 目标 | 可选增强贡献 | 无有效提案时保留原生结果，并可见提示；不把已发生扩展 OS 副作用视作未执行 |
| 目标 | guard 型钩子（工具/输入/策略） | 用户启用即为必经规则：异常、超时、熔断、宿主缺失或协议错误都阻止受影响动作并给出可见原因；熔断打开意味着拒绝而非放行，直到修复或用户显式停用该插件。扩展不能在运行时把 guard 降级为 advise；advise 型钩子失败时保留原生行为并提示 |
| 目标 | 选中的工作流/引擎 | 失败或恢复阻塞，不切换默认核心；可用性不等于必须让该轮继续 |
| 目标 | 宿主崩溃 | 撤销该插件注册和句柄；不影响其他插件进程的控制通道。观察增强可降级，依赖必经规则或选定实现的运行阻塞 |
| 目标 | 取消 | daemon 关闭新调用，发送取消并有限等待，必要时回收受管进程；迟到结果只作原调用结算，不写下一轮；OS 直接副作用无法保证中止或回滚 |

【目标】宿主策略默认值（不由清单提高，可由产品配置调整，验收时以压力测试校准，偏离须修订本段）：guard 单次 deadline 默认 5 秒、上限 30 秒；advise 默认 5 秒、上限 15 秒；observe 走有界异步队列，每插件 1024 条，满则丢弃最旧并留下可见缺口标记。熔断：连续 5 次失败，或最近 60 秒内至少 10 次调用且失败率过半，则打开；冷却 30 秒后半开，只放行 1 次探测，成功则关闭；guard 型熔断期间受影响动作被拒绝。宿主崩溃后按 1 秒起、上限 60 秒的指数退避重启；10 分钟内崩溃超过 5 次则停止自动重启并标记失败，等待用户手动重启。最强反方：guard 失败会阻塞工作；因此阻塞原因、受影响动作与停用入口必须始终可见，用户显式停用后新动作才按变更后的策略接纳，Magi 不自动越过。

### 5.9 提示、上下文与缓存

【现状·文档摘要】现有缓存设计只将工具历史降级和文件事实刷新视为声明式前缀改写，未声明改写为 history_rewritten 并告警；不能把扩展任意改写接入后仅显示一条成本提示就算合规。[缓存架构:180](prompt-cache-lineage-architecture.md#声明式前缀改写)、[model_step.rs:238](../crates/magi-conversation-runtime/src/model_step.rs#L238)。

【目标】稳定贡献使用插件、贡献 ID、包摘要、配置快照及内容摘要确定身份，不能夹入时间戳或随机数；易变内容放 RoundContext 尾部。最终请求贡献先入来源标记与预算，再由实际 Bridge 的 cache_prompt_ir 计算；不能在该步骤后改 provider 请求体。插件不生成 cache key，不维护另一份 transcript。

【目标】扩展可经 context.assemble 与 compaction.before 提出对已装配历史的受控改写，作为现有声明式前缀改写合同的新增原因 ExtensionRewrite：每次改写必须记录插件与贡献身份、改写范围、原文引用（canonical 日志不变，只改投影）和触发的缓存代际变化，并在界面显示“扩展改写了历史，缓存已重建”；原生系统段与工具配对约束不可改写；未声明并获授权 contextRewrite 的插件提交此类提案一律拒绝。该合同已在[缓存架构](prompt-cache-lineage-architecture.md#扩展改写extension_rewrite)中落为正式失效原因（含记录合同、唯一投影和归因回归），并已接入主线任务循环；`contextRewrite` 授权项已进入清单与授权面板；历史改写不持久化替换内容，由扩展保证稳定、成本可见。仍缺界面提示与纯会话 profile 的接入。最强反方：改写会破坏前缀缓存、推高成本；以显式授权、可见代价和代际记录约束，而不是整体禁止。

### 5.10 包、清单、权限与信任

【现状·读码推断】包为 ZIP，16MiB 压缩、32MiB 展开、512 条目、单文件 4MiB；校验路径、大小写冲突、文件模式、文件类型和原生程序魔数；SHA-256 标识内容，不标识作者。[package.rs:10](../crates/magi-plugin-system/src/package.rs#L10)、[package.rs:54](../crates/magi-plugin-system/src/package.rs#L54)。

【现状·部分实现】清单已有 `extension`（`hooks: [{name, mode, deadlineMs?}]`）与权限种类 `extension:["full-trust"]`：声明 extension 的包不得同时有 `backend`/`plugin.mjs`，必须带固定入口 `extension/index.mjs`（单文件 ≤4MiB，随现有包上限），钩子须在协议目录内且模式合法，暂不允许工具/命令/工作流/引擎/数据迁移贡献。这是 SDK 1 清单上的过渡增量，完整的新清单版本（manifestVersion、extensionApi、迁移声明等）仍属目标。

【目标】统一 manifestVersion、插件身份、语义版本、说明、可选 Node 后台入口、扩展 API 版本、贡献、设置 schema、数据 schema 和迁移声明。Node ESM 是作者已构建的纯 JS 产物，依赖打包并固定版本，入口和大小属于 D2。仅 Skill/MCP 声明的包允许无后台；不生成空 UI，不要求不必要的完全信任。SDK 1 清单必须在校验阶段明确拒绝，不沿用旧版本号解释 Node 代码，也不自动转换旧包。

【目标】删除 filesRead/filesWrite/network 形式的装饰性 OS 权限；storage、tools、models、browser 等服务权限仅在具备真实强制点时纳入新合同。启用可执行后台先确认“此扩展以您的用户权限运行，可读文件与凭据、联网和启动进程，Magi 无法限制”，再展示 hook/注册清单。后者只强制**Magi 路由与注册范围**，不强制限制 Node OS 权限。钩子注册须声明 mode、deadline 与触发条件；contextRewrite、externalMigration 作为需要显式授权的能力项出现在清单与授权面板。授权绑定包摘要、来源身份和 API 契约；升级新内容不得静默继承执行信任。

【目标】安装和启用仍全局一份，不重开工作区安装体系；删除 scope 常量穿透的伪多作用域集合、资源键与参数。运行请求保留宿主签发的 workspaceId、sourceFolderId、cwd 与执行映射；这些限制 Magi 服务，不是对受信 Node 的文件沙箱。按项目筛选钩子适用范围可作为后续公开合同，不能伪装成 OS 权限。

【目标】安装前展示实际来源、版本、摘要、执行类型、兼容报告及新增能力，区分来源标签与签名保证；供应链审计、签名、版本固定可降低或追踪部分风险，但不能保证作者或依赖没有恶意。插件不能自称“已验证”；每个验证标签须说明验证对象与范围。一键停用和故障入口在启用受信代码时同时提供。

## 6. 阶段 E：翻译器与兼容来源

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

【现状·文档摘要】上表依据为第 3 节固定版本文档，不是已经实现的适配器。pi 的 input/Turn/工具次序与不重验入参，见[扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md)；DSH 对象、事件及生成协议见[Agent](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent/README.md)、[Session](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/subsystems/session.md)、[Gateway](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/api-gateway.md)。

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

【现状·文档摘要】不能沿用“pi、DSH 都没有官方目录”的旧结论：pi 的[包文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/packages.md)链接 [pi.dev/packages](https://pi.dev/packages)，DSH [插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md)描述官方按需包。所读资料没有证明第三方全集经过签名审查并保证安全；此项应写“未核实”，不能推导为绝对不存在。

### 6.4 兼容验收与供应链

【目标】每个翻译包记录来源项目、仓库与源码摘要、上游精确版本/提交、适配层版本、构建工具版本、依赖锁摘要、许可证与兼容报告。许可逐包核实；核心项目许可证不能代替第三方扩展分发许可。Magi ID 与作者命名空间绑定，不由上游显示名称决定授权。

【目标】固定一致性测试集覆盖：阻止/改参、异常和取消、并行工具结果顺序、输入来源、最终空闲、流式更新、资源冲突、重启与私有状态、模型归属、UI 缺失、effect 回收、所有不支持 API 的拒绝。能做上游差分的在同一输入下对照；平台差异分别记录。分母为固定样本与必需语义断言，报告“通过/偏离/不支持/未覆盖”，不能只报一个可翻译比例。

【目标】仅当一个包的全部必需行为在两平台通过，且作者/用户确认可接受的显式差异后，才标该固定版本兼容。若必需行为依赖不支持 API，构建即拒绝；不可为求比例忽略该行为。上游或适配层更新生成新产物和新报告，不静默沿用旧兼容结论。

## 7. QuickJS 直接替换与删除边界

### 7.1 已确定的替换策略

【现状·用户确认】用户确认 QuickJS 尚未实际投入使用，并要求直接替换。该确认用于确定兼容责任；未扫描用户机器或统计已安装插件。源码中已存在的工具、工作流、引擎和发行接线仍须逐项替换，不能由使用现状推导为没有集成成本。

【目标】直接以 Node 替换 QuickJS。停止扩建 SDK 1，不安排旧包逐个迁移、兼容解释器、双后台过渡版本或故障时启用旧引擎。替换交付必须同时移除旧执行入口及其依赖，不能只增加 Node 分支后让删除无限延期。最强反方：一次更换所有接线扩大回归面；因此以完整纵向交付和有界验收控制，不用双实现分摊风险。

【现状·读码推断】当前 Worker 读取报文前施加限制，每次调用创建 QuickJS Runtime/Context；无模块 loader、Node、文件、网络、环境或进程接口；sdk.call 同步收发，身份对象深冻结。[worker.rs:14](../crates/magi-plugin-runtime/src/worker.rs#L14)、[worker.rs:29](../crates/magi-plugin-runtime/src/worker.rs#L29)、[worker.rs:58](../crates/magi-plugin-runtime/src/worker.rs#L58)。这些是待删除实现的审计快照，不是目标保留的沙箱层。

### 7.2 保留基础与同批删除清单

| 状态 | 范围与当前依据 | 替换交付要求 |
| --- | --- | --- |
| 目标 | 包、管理器、资源与设置，见第 2 节 | 保留有效的校验、摘要、安装事实、资源 CAS、设置 schema 和单一管理入口；重定新包合同，SDK 1 在预检阶段明确拒绝 |
| 目标 | [运行时依赖与二进制](../crates/magi-plugin-runtime/Cargo.toml#L8)、[QuickJS 执行器](../crates/magi-plugin-runtime/src/worker.rs#L29) | 将 Rust 运行时职责收敛为 Node 进程监管与协议；删除 rquickjs 依赖、QuickJS 执行器、旧 sandbox 实现与 magi-plugin-worker 的原执行/自检入口；不得另留受限转换 VM |
| 目标 | [daemon 装配与旧 Worker 寻址](../crates/magi-daemon/src/daemon/plugin_bridge.rs#L249)、[命令](../crates/magi-plugin-system/src/command.rs#L70)、[工作流工厂](../crates/magi-plugin-system/src/workflow_factory.rs#L94)、[引擎工厂](../crates/magi-plugin-system/src/session_engine.rs#L193) | 工具、命令、工作流、引擎全部接同一 Node 宿主协议；复用类型化业务合同，删除同步 sdk.call 和旧 source/Invocation 执行桥，不维护 SDK 1 shim |
| 目标 | [数据转换 Worker](../crates/magi-plugin-system/src/data_migration.rs#L134) | 同批移除 QuickJS 数据转换调用；Node 插件的数据升级按 9.4 的迁移会话执行，不为升级保留第二种代码运行时 |
| 目标 | [开发构建](../scripts/dev-daemon.sh#L67)、[发布预检](../scripts/release-preflight.mjs#L26)、[Desktop 构建](../apps/desktop/scripts/build-support.mjs#L103)、[打包资源及签名](../apps/desktop/electron-builder.yml#L23) | 替换为产品自带 Node 与宿主入口的构建、复制、签名及真实执行预检；正式包不携带旧 Worker，开发与正式路径均不得依赖用户 Node |
| 目标 | [旧平台自检](../apps/desktop/scripts/plugin-worker-preflight.mjs#L21)、[旧许可打包](../apps/desktop/scripts/build-support.mjs#L249) | 删除 QuickJS 专属自检、许可收集及相关 SBOM 条目，加入 Node 的对应交付材料；新版预检证明 Node、协议、取消和回收，不再要求旧沙箱测试通过 |
| 目标 | [示例](../examples/plugins/board/manifest.json#L1)、[运行时测试](../crates/magi-plugin-runtime/tests/execution.rs#L1)、[作者指南](plugin-authoring.md#L3) | 实施时重写为新 API 样本、Node 回归及公开作者手册；仅留下历史审计说明，旧样本不得经兼容层继续运行 |

【目标】界面隔离视图、原生进程组和业务恢复基础继续使用；删除目标只针对旧插件运行时，不误删 Browser Automation Worker、模型 bridge 或原生任务能力。原生 MCP 壳移出插件列表，但其真实用户配置、令牌与审计沿原生入口保留；“无 QuickJS 插件存量”不能作为清空这些数据的理由。

【目标】本轮替换不承担 SDK 1 插件包、授权或检查点的自动转换。遇到旧记录明确报告不支持，不执行、不猜测恢复、不自动清空；显式清理仅能作用于已确认可丢弃的插件试验数据。未来 Node 包的版本升级与备份恢复仍需第 9 节合同，但不得恢复成 QuickJS 运行时。

### 7.3 平台与交付边界

【现状·读码推断】当前 QuickJS Worker 在 macOS 使用 sandbox_init，Linux 使用 no_new_privs/seccomp；Windows apply 返回 Ok，ENFORCED=false。[sandbox.rs:9](../crates/magi-plugin-runtime/src/sandbox.rs#L9)、[sandbox.rs:40](../crates/magi-plugin-runtime/src/sandbox.rs#L40)、[sandbox.rs:187](../crates/magi-plugin-runtime/src/sandbox.rs#L187)。这组差异只描述待替换源码，本次未运行跨平台自检。

【目标】Node 后台在 macOS、Windows 均按用户权限运行，提供相同插件能力，不承诺旧 QuickJS 的 OS 沙箱。Unix 进程组、Windows Job Object、IPC 访问控制和签名交付分别验收；不得把故障回收或 IPC 身份认证等同文件/网络隔离。视图 iframe 的限制独立保留，不以后台全权限为由放宽 Renderer 边界。

## 8. 界面扩展与数据同步

### 8.1 工作台与视图

【目标】设置沿共享 schema 表单，复杂 UI 使用隔离 iframe；主页入口打开主内容区页面，右侧面板复用公共 Tab、关闭/折叠/焦点和自适应布局。原生 Browser surface 继续由 BrowserAuthority/Main 托管；受信后台不因此获得主 Renderer DOM、preload 或第二套 guest 管理器。

【目标】视图响应必须补 sandbox allow-scripts，结合 connect-src none、nosniff 等防线；不能只靠父 iframe。消息桥按精确 frame、每次导航新代次、一次性握手、schema、请求和大小限制绑定，页面不能自报插件或任务身份。新增后台操作经公开工具/命令入口，由 daemon 区分用户动作、模型动作和插件后台动作；消息本身不构成用户审批。

【目标】复用 [UX/UI 规范](ux-ui-guidelines.md) 的状态、共享控件、多语言、键盘、触摸与窄屏要求。实现时验证宽窄布局、200% 缩放、长文案、RTL、触屏与键盘；本文仅改设计，不宣称 UI 已完成这些验收。

### 8.2 单一资源权威

【目标】每个资源明确权威为 daemon 插件资源、文件或远端系统之一。全局存储键为 pluginId＋resourceId；项目相关值使用显式项目身份或业务对象键，不恢复多套启用 scope。会话内私有记录是 daemon 命名空间中的 session 级资源，键含 sessionId；分支（Thread）创建时复制父会话记录并从此独立，它们不属于 canonical 日志。插件 UI、工具、命令进入同一资源操作服务和 CAS，配置保存经同一 schema 校验。

【目标】打开视图获取原子快照/订阅边界；增量缺口重读权威快照；并发冲突明确返回，旧视图不补写最新值。长计算携输入版本，过期不覆盖。文件监听、远端推送属于资源服务输入；远端无版本时本地观察序号不冒充远端事务版本。大数据用有界授权句柄按需读，不塞入聊天事件。

【目标】选中对象可提交插件、资源、版本、对象 ID 和摘要，经原生上下文预算引用；不自动注入整个页面。关闭面板只释放视图订阅，不结束原生任务。后台监听、定时任务和模型调用必须有显式保活与用量归属；持久定时由 daemon 的原生调度服务唯一拥有（5.7），扩展宿主内不得另建持久调度器。

## 9. 安装、生命周期与升级

### 9.1 统一安装与来源

【目标】统一流程：解析来源 → 获取不可变产物 → 校验包/契约/平台/摘要 → 预览能力与差异 → 确认信任 → 原子记录安装 → 用户启用 → 准备并统一发布贡献。安装不执行，预览不激活。每插件管理操作串行，独立操作 ID 用于取消、重连和恢复。

【目标】预览凭据绑定插件 ID、版本、包摘要、来源身份、执行类型、声明、数据格式和契约；安装重新获取内容不一致即 409，不让用户确认 A 后安装 B。更换来源不继承执行授权；无静默自动升级。

【目标】默认远端使用 HTTPS；下载器统一限制大小/超时、重定向每一跳、IP 和最终连接目标。用户明确配置的本地/私网目录可单独授权其端点范围，不能全局禁私网后破坏个人源，也不能因初始地址获准就允许跳到任意内网。HTTP 例外必须明确显示来源不安全，不能自动降级。

### 9.2 目录信任

【现状·读码推断】目录已有 Ed25519 信封验签和包 ID/版本/SHA-256 一致性校验；配置公钥时拒绝未签名目录，当前官方地址额外映为 trusted。[catalog.rs:88](../crates/magi-plugin-system/src/catalog.rs#L88)、[catalog.rs:175](../crates/magi-plugin-system/src/catalog.rs#L175)、[center.rs:79](../crates/magi-api/src/routes/plugins/center.rs#L79)。

【目标】官方目录一律签名：Magi 发布公钥（Ed25519）随产品交付，目录为签名信封，含单调递增的 sequence、issuedAt、expiresAt 与撤销列表。客户端持久记录已见最大 sequence，拒绝回退与过期目录；撤销条目使已安装插件进入停用并提示用户；密钥轮换由旧密钥签发的密钥集变更完成。用户自配目录可选配自己的公钥，未配置则作为“指定地址”来源呈现，不显示为已验证。来源、签名、兼容测试是不同属性，签名不代替包授权；发现指针可更新，实际安装钉死用户预览的产物。

【目标】官方目录以 release 资产发布，指针地址稳定，新鲜度由签名、sequence 与 expiresAt 保证；`main` 分支文件不再作为信任根。兼容来源的 Magi 目录使用同一信封格式与同一验证路径。

### 9.3 状态与退出

| 状态 | 对象或操作 | 合同 |
| --- | --- | --- |
| 目标 | 包状态 | 未安装、安装中、已安装、更新中、移除中、操作失败；操作结果与代码清理结果分别表达 |
| 目标 | 实例状态 | 禁用、激活中、就绪、排空中、停止、失败/崩溃；启用意愿不等于可用 |
| 目标 | 停用/禁用 | 先关闭新运行、命令、独立写入与后台活动准入；已有租约有限排空，再取消/回收；结算完成才显示停用。撤销启用意愿与停止实例均在同一管理操作中排序 |
| 目标 | 排空期间 | 原租约范围内继续；状态读取、取消、待处理审批/答复仍可用。权限撤销立即覆盖租约 |
| 目标 | 卸载 | 有活动调用、资源租约或待恢复精确版本引用时明确阻止；不删除后再让恢复静默换核心 |
| 目标 | 崩溃 | 句柄与注册代次失效；保留真实结果和未知副作用；不自动重放 |
| 目标 | 数据 | 卸载默认保留配置与资源，清除数据单独显式操作；删除失败不得报“完整清理成功” |

【现状·读码推断】当前排空默认 30 秒，再取消并等待 10 秒，超时返回冲突；这些值已在源码，不是沿用未复核。[lifecycle.rs:40](../crates/magi-plugin-system/src/lifecycle.rs#L40)、[drain.rs:178](../crates/magi-plugin-system/src/lifecycle/drain.rs#L178)。受信宿主是否沿用须按任务/钩子类型定案，不能把原 Worker 时限无条件套入。

### 9.4 升级、数据迁移与恢复

【现状·读码推断】当前升级关闭准入且要求无运行租约，写新包，清空 grants/active_scopes，commit 状态后应用暂存资源；重启按 to_digest 判断继续应用或丢弃暂存。现状不是“提交后新版已经就绪”。[upgrade.rs:33](../crates/magi-plugin-system/src/lifecycle/upgrade.rs#L33)、[upgrade.rs:199](../crates/magi-plugin-system/src/lifecycle/upgrade.rs#L199)、[upgrade.rs:261](../crates/magi-plugin-system/src/lifecycle/upgrade.rs#L261)。

【目标】升级分准备和切换：候选包校验/预览 → 差异与执行信任确认 → 关闭准入/排空旧写入者 → 准备并校验数据版本 → 准备激活描述（不执行有副作用的 Node 入口）→ 原子提交版本/摘要/数据/配置/授权绑定 → 激活并发布就绪。激活后才标成功；失败留在明确不可用状态，不自动恢复旧实现。

【目标】旧实例已停止后，提交前失败可在显式恢复动作中重启原绑定，但不能声称“旧激活从未改变”；Node import 本身能产生副作用，不属于无副作用预检。提交后应用或激活失败以操作记录恢复，不运行新代码读取旧格式数据。

【目标】Node 包的数据迁移是程序化的：新版本在专用迁移会话中运行其迁移入口，经协议迭代读取旧数据快照，并把结果写入 daemon 管理的暂存区；暂存结果经 schema、大小与版本核对后，随版本切换原子提交。迁移入口以用户权限运行，不是沙箱纯函数，文档与界面不得如此表述；它直接触达的外部文件或远端 schema 不在 daemon 原子事务内，插件须声明 externalMigration，并在升级预览中向用户披露恢复办法。迁移前 daemon 必须先生成完整备份。不存在第二种迁移执行方式。

【目标】每次升级在提交前生成并保留上一版本的完整备份集（代码包、数据快照、配置与授权记录），保留到下一次升级成功后回收。旧版本恢复是显式的管理操作：校验备份完整性与授权，经排空后原子还原，并明确提示插件自己管理的外部状态不会被还原；它不是故障时的自动回退，也不允许新旧双写。存在引用旧版本的待恢复运行时，升级前须由用户处理这些运行。

## 10. 工作流核心、会话引擎与子代理

### 10.1 职责与现状

【现状·读码推断】工作流已有 ModelRequest、ToolCall、DispatchTask、WaitUser、SaveStage、Complete 六类动作；宿主签发 action_id，返回必须匹配。[workflow.rs:13](../crates/magi-plugin-system/src/workflow.rs#L13)、[workflow.rs:88](../crates/magi-plugin-system/src/workflow.rs#L88)。执行器在模型/工具动作前保存检查点；恢复依赖既有事实。[workflow_execution.rs:313](../crates/magi-conversation-runtime/src/task_execution_dispatcher/workflow_execution.rs#L313)、[workflow_execution.rs:585](../crates/magi-conversation-runtime/src/task_execution_dispatcher/workflow_execution.rs#L585)。

【目标】工作流决定下一步，daemon 负责实际模型/工具/派发、权限、取消、压缩、检查点与终态。默认和插件实现遵守同一公开动作合同；观察/拦截插件不能变成第二个调度器。Node 能直接执行 OS 副作用不代表这些副作用进入工作流动作账本；需要原生恢复保证的操作必须走公开服务。

【目标】会话引擎负责请求与响应传递，不允许把自带 agent loop、任意工具执行或第二套存储藏进 conversation profile。模型 provider 负责模型协议，与工作流编排分别版本化。Node 执行扩展决策处理器，daemon 的执行与恢复权威不变。

### 10.2 固定、恢复和替换验收

【目标】本次显式选择 → 用户默认 → 内置默认；显式选项失效拒绝启动。运行固定插件/贡献 ID、精确版本、摘要、配置和检查点格式；更改默认只影响新运行。核心失败不改用默认核心。

【目标】每个动作先持久化意图与身份，后执行并记录回执；恢复查看原生动作状态。未知副作用进入恢复阻塞，只有证明未执行或具备幂等证据才能自动重试。不能把 attempt 的新 ID 当作重新执行旧动作的理由，不宣称 exactly-once。

【目标】工作流和会话引擎的宿主调用随 QuickJS 一并替换，不设置逐包迁移阶段。现有类型化动作、取消、原始回执、模型能力声明和崩溃恢复合同必须在 Node 路径回归通过；尚未具备的增强能力保持未支持并明确拒绝，不夸大成已提供流式或恢复能力。旧 QuickJS 检查点不交给 Node 或默认核心猜测恢复。最强反方：同批替换扩大回归面；没有投产插件兼容负担时，以单路径完整验收控制风险更符合 D1。

### 10.3 自定义角色与模型

【现状·读码推断】原生派发先校验角色，再解析该角色配置的模型绑定；预检明确不接受调用级模型覆盖。[agent_spawn_preflight.rs:238](../crates/magi-conversation-runtime/src/agent_spawn_preflight.rs#L238)、[agent_spawn_preflight.rs:389](../crates/magi-conversation-runtime/src/agent_spawn_preflight.rs#L389)、[agent_spawn_preflight.rs:774](../crates/magi-conversation-runtime/src/agent_spawn_preflight.rs#L774)。

【目标】扩展可按任务类型建议允许角色、提示配置角色与模型或提交显式配置变更，但不直接把 role 选定后的绑定换成任意 model。不提供按次模型覆盖：角色绑定的模型是用户对产品的显式选择，扩展只能选择角色，或提交需用户确认的角色与绑定变更提案。角色名称、模型选择和委派授权是三种事实，不能互相替代。

## 11. 实施路线、退出条件与冻结项

【现状·文档摘要】[演进方案:3](magi-agent-runtime-evolution-plan.md#1-核心判断) 明示存量收敛、暂停新增能力；[第 10 节](magi-agent-runtime-evolution-plan.md#10-插件guiweb-和预留接口的边界) 要求改参重验、撤销优先和资源结算。

【目标】实施路线只表达依赖顺序：每个阶段交付终局合同的完整纵向切片，不含临时兼容层、过渡运行时或可由后续补丁替换的接口。演进方案的“暂停新增能力”表述须先按 13.2 修订。

| 状态 | 阶段 | 依赖与交付范围 | 退出条件 |
| --- | --- | --- | --- |
| 目标 | P0 审计收敛与协议 | 优先修保留基础中的 G2/G3/G5/G6/G9；将 G1/G7/G8/G12/G14 纳入新宿主验收；收敛 scope、原生 MCP 壳与诊断。定案 Node 单路径协议、API、信任、失败、取消和第 7 节删除清单 | 共享基础缺陷有定向回归方案；每钩子有真实落点；必需设计决策关闭。不先扩建或为兼容而修补 QuickJS |
| 目标 | P1 Node 替换纵向交付 | Node 宿主、ExtensionBus、版本握手、工具/命令、现有工作流/引擎合同、迁移会话与升级备份；tool.call/tool.result（含 guard/advise 模式与熔断）、有限 prompt.contribute；完全信任、停用、诊断、构建打包及 QuickJS 删除同批完成 | 一个真实扩展仅公开 API＋Node 完成文件/网络/进程及工具拦截；现有后台贡献的保留合同通过回归；两平台干净安装版通过；QuickJS、旧 Worker、旧 SDK 桥及其依赖不再进入产品。阻断规则不可因故障被越过 |
| 目标 | P2 数据与 UI、上下文合同 | 资源与 UI→后台共同入口；通知/问题；模型观察；受控 context/compaction；原生子代理角色路由；插件作业与原生调度；ExtensionRewrite | 同步冲突/订阅重连/视图隔离；模型用量有归属；角色模型不漂移；缓存失效可解释；每新增钩子有取消及重放验收 |
| 目标 | P3 pi 翻译器 | 固定 pi API 子集、构建工具、一致性集、“pi 兼容来源”及产物报告；只基于 P1/P2 的 Node 公开 API | 至少一项真实上游扩展的全部必需断言两平台通过；不支持 API 构建失败；准确报告偏离与未覆盖，不预设生态比例 |
| 目标 | P4 DSH 子集 | 按固定 DSH 版本提供服务/effect 子集与“DSH 兼容来源”；不新增第二个 Magi 后台或 DSH 内核 | 至少一项真实 DSH 包完整依赖审计与行为验收；无 DSH 内核/日志写入；上游破坏性更新不会静默加载旧报告 |

【目标】删除项：QuickJS 后台、SDK 1 兼容执行器、旧 Worker 专属构建/打包/自检与隐藏的数据转换 VM。冻结项：任意插件自带原生二进制、任意应用页面模块、绕过原生上下文的裸消息/请求体、全量 pi/DSH 兼容承诺、默认本机 npm/git 翻译、另建持久调度器、具体外部执行器、收费市场。动态 provider 全面覆盖、跨插件共享内核对象及任何第二运行时均须真实需求和独立评审，不用“大而全 P3”打包推进。

【目标】诊断与完全信任 UI 属于启用受信代码的前置条件，不延期到最后分发阶段。macOS 与 Windows 是 R2 的必验平台；Linux 沿用现有行为维护，不在本设计验收范围内，本评审未验证。

## 12. 验收与证据边界

### 12.1 必须证明的目标合同

| 状态 | 验收面 | 必须证明 |
| --- | --- | --- |
| 目标 | 安装与来源 | 所有来源走同一服务；预览包不变；不运行安装脚本；未授权不激活；目录来源与签名分开 |
| 目标 | 声明与信任 | 未声明 hook/注册被拒，后台完全信任明确；无装饰性 OS 权限；只声明 Skill/MCP 可无后台；SDK 1 在执行前明确拒绝 |
| 目标 | 工具提案 | 最终改参在账本、路径/并发规划、权限和审批前生效；用户批准真实最终内容；deny 不被翻转 |
| 目标 | 观察与重入 | 观察不阻塞流式、队列有界且缺口可见；递归和等待依赖不会死锁 |
| 目标 | 故障与取消 | 故障只阻塞依赖它的动作；独立插件仍可操作；必经规则不失效放行；迟到响应不污染新代次 |
| 目标 | 原始结果 | 原始回执、工具状态和 usage 不受模型投影改写；未知副作用不报成功或已撤销 |
| 目标 | 生命周期与升级 | 排空边界原子；撤权即时；不同时激活两版；提交各阶段崩溃不会版本/数据错配；保留待恢复引用 |
| 目标 | UI 与资源 | 直接导航资源 URL 不获 daemon 同源能力；导航换代、过期 frame、CAS 冲突、漏增量重取均受控；关闭视图不结束任务 |
| 目标 | 工作流与模型 | 精确版本恢复、类型化动作、唯一编排负责人；角色先选再解析模型，绑定失效不换默认 |
| 目标 | 缓存 | 最终请求的 IR、指纹、实际 wire 一致；稳定/易变分段与已声明前缀改写可解释，不用估算伪造实际费用 |
| 目标 | 翻译 | 固定版本差分、必需断言、拒绝项、来源与许可证完整；动态未知 API 在运行时仍拒绝 |
| 目标 | 发布 | 两平台无开发工具可用；系统命令不存在和平台不支持明确报告；Node 与宿主版本匹配；依赖、构建、安装资源与实际执行不再包含 QuickJS 或旧 Worker 路径 |
| 目标 | 钩子模式 | guard 在异常、超时、熔断、宿主缺失时阻止；advise 失败保留原生行为并提示；模式变更触发重新授权 |
| 目标 | 作业与用量 | 无作业或 Turn 身份的模型调用被拒绝；作业取消与预算超限结算可见；计划任务由 daemon 唯一调度 |
| 目标 | 目录与撤销 | 回退或过期目录被拒绝；撤销条目使已安装插件停用并提示；官方目录验签失败不降级为未验证展示 |
| 目标 | 迁移与恢复 | 迁移只经暂存区提交；提交前失败不改变旧版本与数据；备份完整性经校验；显式恢复不触碰外部状态并明确披露 |
| 目标 | 历史改写 | 未授权 contextRewrite 的提案被拒；已授权改写在日志与界面可见，原文保留，缓存代际变化被记录 |

### 12.2 证据边界

【目标】证据分层见 1.2。静态核对（行数统计、链接与编号检查、git diff --check）只证明文档与源码的一致性，不证明运行行为；行为结论必须以真实运行验证取代“读码推断”后才能升级。G1–G14 在修复并取得运行验证前不得标为已修复，已有测试的名称不是运行结果。

【目标】纯文档变更按 [UX/UI 规范第 10.2 节](ux-ui-guidelines.md#102-验证入口)只校验内容、相对链接与 diff；Rust、Web、Desktop、协议生成物与真实运行验收按各自目录规则单独记录，不互相替代。

## 13. 设计决策记录与前置文档修订

### 13.1 决策记录

【目标】下列决策已选定，不是待办事项。每项给出理由与最强反方理由；实施中发现必须变更时，直接修订本表与对应章节，不另立过渡方案。

| 状态 | 编号 | 决策 | 理由／最强反方理由 |
| --- | --- | --- | --- |
| 目标 | D1 | 终局合同为 Node 单路径，无过渡运行时、兼容层或补丁接口；本设计是产品能力而非边缘新增，演进方案的“暂停新增能力”表述按 13.2 修订；实施阶段按第 11 节依赖顺序，每阶段交付完整纵向切片 | 双实现会让故障面和维护面翻倍；反方：回归面大，以单路径、完整切片和第 12 节验收控制 |
| 目标 | D2 | 四条独立版本轴（manifestVersion、extensionApi、protocol、数据 schema），各为主.次两段；主版本不同拒绝，次版本只增；入口为 extension/index.mjs，至多 8MiB；SDK 1 清单明确拒绝 | 让包、API、协议分别演进而不连锁破坏；反方：作者需随主版本重发，换取宿主不背负多版本解释器 |
| 目标 | D3 | 每插件独立 Node 进程；按声明的触发条件按需激活，空闲回收；调用顺序由用户有序列表加插件 ID 决定；重入深度 4；产品随包 Node 由 daemon 直接托管，不用 Electron utilityProcess | 可强制终止单个插件，且 Desktop 与无头同一合同；反方：内存与启动成本更高，以按需激活控制 |
| 目标 | D4 | 钩子模式 observe、advise、guard；guard 失败即阻止，熔断等于拒绝；扩展不能运行时降级模式 | 必经规则不得被故障静默绕过；反方：扩展故障会阻塞工作，以可见原因与停用入口控制 |
| 目标 | D5 | 允许受控历史改写，作为 ExtensionRewrite 失效原因；须授权 contextRewrite、原文保留、代价可见、原生系统段不可改写；缓存架构先落合同再开放钩子 | 满足 pi 类扩展的核心用法而不破坏事实源；反方：缓存成本上升，以显式授权与记录约束 |
| 目标 | D6 | 不提供按次模型覆盖；钩子只选角色；角色或绑定变更提案须用户确认 | 保住“角色绑定模型”这一产品特色；反方：上游任意 model.select 无法原样翻译，在 6.2 明确拒绝 |
| 目标 | D7 | 数据迁移为程序化迁移会话，经暂存协议提交；会话内私有记录为 session 级资源，分支复制后独立 | 复杂迁移需要代码；反方：迁移入口全权限，故必须备份、暂存校验并如实披露，不称沙箱 |
| 目标 | D8 | 升级前生成完整备份集；旧版本恢复是显式管理操作，不自动回退，不双写 | 满足可恢复与禁止静默兜底；反方：备份占用存储，外部状态无法还原，须披露 |
| 目标 | D9 | 后台工作持有插件作业身份；持久定时由 daemon 原生调度服务拥有；无作业或 Turn 身份的模型调用拒绝 | 用量、取消与归属有唯一所有者；反方：Node 直接外调模型仍在核算之外，如实披露 |
| 目标 | D10 | 官方目录一律签名，含 sequence、expiresAt 与撤销列表，以 release 资产发布；兼容来源的 Magi 目录同格式；不提供本机一键翻译 | 目录新鲜度、回滚与撤销必须由协议保证，而非固定 URL；反方：Magi 要承担发布密钥与目录维护 |
| 目标 | D11 | 现有工作流与会话引擎合同整体迁入 Node 协议，不扩张能力；流式、provider 增强经协商能力位独立版本化 | QuickJS 一次退出；反方：P1 接线范围大，须完整回归 |
| 目标 | D12 | 要求强制 OS 只读的环境拒绝激活受信层，不以单 Task 的钩子过滤代替隔离 | 与受限模式的真实隔离约束一致；反方：严格环境下插件不可用，若要并存须另立真实 OS 隔离设计 |
| 目标 | D13 | 不保留任何低信任后台路径：需要后台逻辑的插件一律完全信任；低信任形态只有无后台的声明式包 | 避免第二种执行器及其兼容负担；反方：轻量计算类插件也需完全信任，以明确告知和声明式包承载不需后台的内容 |

### 13.2 相关文档的对应描述

【目标】本设计的各部分在下列文档中各有唯一描述位置，修订任一处时同步检查其余，使全仓只有一套说法：

| 状态 | 文档 | 承载内容 | 来源决策 |
| --- | --- | --- | --- |
| 现状 | [演进方案](magi-agent-runtime-evolution-plan.md#1-核心判断) 第 1、3.5、8.4、10、11 节 | 插件系统作为独立交付线、不受“暂停新增能力”约束；ExtensionBus、钩子模式与插件作业；受信层与严格只读不并存；与 P3 统一 Step Loop 的接线顺序 | D1、D4、D9、D12 |
| 现状 | [缓存架构](prompt-cache-lineage-architecture.md#扩展改写extension_rewrite) | `extension_rewrite` 失效原因、记录合同与唯一投影（已实现并有回归），已接入 `context.assemble` / `compaction.before`；剩余缺口：界面提示与纯会话 profile | D5 |
| 现状 | [项目架构文档](README.md) | 扩展宿主进程与进程拓扑、随包 Node、文档索引 | D3 |
| 现状 | [验证入口](validation.md) | 第 12 节验收面的登记处；当前没有对应命令 | 第 12 节 |
| 目标 | [扩展协议 v1](extension-protocol-v1.md) | 线协议规范：传输与帧、握手与版本、调用身份、方法目录、注册、逐钩子合同、重入、迁移会话、失败与错误码、协议层验收（尚未实现） | 5.5～5.8、D1、D4 |
| 目标 | [作者指南](plugin-authoring.md) | Node 公开 API 发布时整体重写；SDK 1 指南随 QuickJS 一并删除 | D1、D2 |

### 13.3 未核实事项与局限

| 状态 | 项 | 局限 |
| --- | --- | --- |
| 现状·读码推断 | 本地行为 | 未执行 G1 取消时序、G2 浏览器利用、下载私网访问、管理锁压力或饥饿用例；严重度基于静态链路 |
| 现状·读码推断 | 跨平台 | 未在 Windows/macOS 干净安装环境验证；当前开发主机读取不能代替平台行为与安全边界证明 |
| 现状·文档摘要 | 上游实现 | 全读指定文档，不等于全读其源码；未运行上游扩展、未审查全部依赖或发布包。文档中的实现自述仍是文档摘要 |
| 现状·文档摘要 | pi 样本统计 | 未抽样执行真实生态包，不能给可翻译比例、TUI/原生依赖比例或推广承诺；统计需固定样本、源码与依赖图 |
| 现状·文档摘要 | 注册表信任 | 已确认 pi gallery 与 DSH 官方按需目录存在；未核实第三方全集的签名、审核或发布者担保；未用星数/包数量作架构论据 |
| 现状·读码推断 | Browser surface | 核对至清单与激活准入，未证明真实宿主所有 surface 声明均有挂载/授权强制点 |
| 现状·读码推断 | 默认核心统一 | 插件动作合同已核对；内置默认循环是否完整实现同一 WorkflowCore 合同未核实，不写成已完成 |
| 现状† | 沙箱历史发行验收 | 旧版本“已纳入发行并验收”陈述不沿用为当前证据，须重跑目标平台安装版 |

## 14. 参考资料与读取范围

### 14.1 本地依据

【现状·文档摘要】读取范围：根 AGENTS、docs/README、作者指南、演进方案第 10 节及冻结说明、Turn/任务架构、缓存谱系、UX/UI 规范；插件系统与运行时源码、插件路由、daemon 桥及工具/模型/上下文/子代理实际调用方。正文逐项链接是对应结论的证据，不把其他计划文档视为交付事实。

【现状·文档摘要】架构约束入口：[整体架构](README.md)、[演进方案](magi-agent-runtime-evolution-plan.md)、[Turn 与任务](conversation-response-core-architecture-redesign.md)、[缓存谱系](prompt-cache-lineage-architecture.md)、[上下文压力](context-pressure-compaction-architecture.md)、[Browser](browser-runtime-design.md)、[UX/UI](ux-ui-guidelines.md)、[验证入口](validation.md)。未在本文复核的 Browser 物理宿主细节只作约束引用。

### 14.2 外部资料

【现状·静态核对】查阅日期均为 2026-11-02。先获取用户指定 raw 地址，再核实仓库提交并重取，下载字节相同。pi 原 badlogic/pi-mono 地址重定向到 earendil-works/pi；固定快照为 c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8。DSH 固定快照为 ce7073d51254c26cd63b22a60835b8f948deb447。下列行数是实际下载文本静态统计，不是 API 稳定性证据。

| 状态与证据类型 | 资料 | 读取范围 |
| --- | --- | --- |
| 现状·文档摘要 | [pi 扩展文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/extensions.md)；[用户指定 raw 地址](https://raw.githubusercontent.com/badlogic/pi-mono/main/packages/coding-agent/docs/extensions.md) | 全部 3,347 行、156,365 UTF-8 字节；分段读至最后 with-deps 示例，包含后半部 tool exposure、嵌套工具、MCP、UI、错误与运行模式；不再沿用“只读前十万字符”的限制 |
| 现状·文档摘要 | [pi 包文档](https://github.com/earendil-works/pi/blob/c9f4c7b9fcd0f3e6b5bbc35c7c9b6cda35580eb8/packages/coding-agent/docs/packages.md)、[官方展示页](https://pi.dev/packages) | 包文档全部 153 行；展示页核对包目录与 npm 安装说明，未审查全部包、未采用页面统计数字 |
| 现状·文档摘要 | [DSH README](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/README.md) | 全部 77 行，含 preview、安装、topic 与许可入口 |
| 现状·文档摘要 | [DSH 架构](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/architecture.md) | 全部 169 行，含 profile、Desktop、事件域、Turn 流、日志与 capability seam |
| 现状·文档摘要 | [Cordis 入门](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/cordis-primer.md) | 全部 45 行，含 dispatch mode、waterfall、inject/effect 与 loader |
| 现状·文档摘要 | [DSH Session](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/subsystems/session.md) | 分段全部 1,427 行，含消息投影、append、fork、插件记录与生成 API/事件目录 |
| 现状·文档摘要 | [DSH API Gateway](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/docs/api-gateway.md) | 全部 172 行，含 Remote、Host/Client 生成、身份解析、取消、源码开发模式与边界 |
| 现状·文档摘要 | [DSH Agent](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent/README.md) | 全部 219 行，含模型绑定、inbox、scope、工厂与 teardown |
| 现状·文档摘要 | [DSH Agent loop](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/core/agent-loop/README.md) | 全部 230 行；读取交付文本全部内容，其中 Turn/step flow 有一行原文本身含“line truncated to 2000 chars”提示，该省略部分未核实，不作详细实现依据 |
| 现状·文档摘要 | [DSH 插件管理器](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/packages/boot/plugin-manager/README.md) | 分段全部 207 行，含官方按需目录、npm 源、脚本授权、版本准入/豁免、故障及更新恢复 |
| 现状·文档摘要 | [DSH 安全说明](https://github.com/deepseek-ai/deepseek-harness/blob/ce7073d51254c26cd63b22a60835b8f948deb447/SAFETY.md) | 全部 27 行；不将上游沙箱或审批表述为插件代码的可靠隔离 |

【目标】不沿用未复查的其他产品比较作为本设计证据。后续引用上游新版本须重新记录读取范围、提交和一致性结果，不能让默认分支更新自动扩大兼容承诺。
