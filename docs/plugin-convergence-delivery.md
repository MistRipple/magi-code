# 插件系统收敛交付（2026-11-10）

本文件是用户于 2026-11-10 确认的本次有限交付范围，覆盖旧持续目标的完成判定；产品长期愿景仍见[产品合同](plugin-system-requirements-design.md#60-翻译功能的产品合同2026-11-08)。不以整个 DSH 生态的 95%／100% 支持作为本次退出条件。新增发现进入本文后续事项，仅直接阻挡下表任务或破坏原生对话的缺陷进入当前整改。

历史现场已按用户要求清理。本文早期批次中“保留”的路径只描述当时状态，当前实际保留范围以[过期现场清理](#过期现场清理2026-11-10)及清理回执为准；已删除的旧目录不再作为当前可复查证据。

## 本轮边界修正（2026-11-10）

用户确认控制插件对 Magi 的干预范围，不新增可切换的 DSH 主对话面板。本轮只闭环三个问题：

1. 安装期与运行期拒绝 DSH 原生消息／输入区替换，明确说明实际冲突，保留设置、业务页、右栏及并列输入贡献。
2. 清除将替换注册自动改为附加视图的映射，以及专属消息投影和草稿提交转换；旧安装不能借已有授权或重启恢复绕过，数据与管理操作保留。
3. 在当前目录包以 GenUI 0.3.3 验证拒绝，用 WorkBuddy 5.21.2 的真实模型业务、原生详细／摘要模式及草稿确认宿主未受影响，再回归 REA 正常设置页面。

此前 GenUI 24/24 是旧合同的运行记录，不是当前支持结果；下方旧批次证据保留以供审计。本轮不继续追求生态全集支持率，也不扩展新 UI 位置、执行内核或通用配额框架。

本轮已闭环，使用重新构建的 macOS arm64 / Magi 3.0.62 / Electron 45 目录包与随包 Node／daemon：

| 当前验证范围 | 结果与证据 |
| --- | --- |
| GenUI 0.3.3 在线原包准入、WorkBuddy 5.21.2 实际模型及原生界面 | `node scripts/verify-electron-genui.mjs` 独立完整 **10/10**，`target/plugin-acceptance/ui-boundary-genui-final/`。安装前明确消息替换冲突、无确认候选、安装与旧实例不变；真实 `deepseek-v4.1-flash` 在拒绝前后均完成回复；原生详细／摘要、正文及样式、未发送草稿和完整重启通过，Renderer 异常 0。GenUI 计为边界拒绝，不能计入业务支持率。 |
| REA 1.2.0 正常原页面与配置生命周期 | `node scripts/verify-electron-dsh-rea.mjs` 独立完整 **22/22**，`target/plugin-acceptance/ui-boundary-rea-final/`。在线原包 SVG、原表单、Ghidra 外链、真实 rea-agents 6.5.0 的 **207** 项工具目录、配置 CAS 与 MCP 重载、390px、页面保活、停用及重新启用通过；Renderer／iframe 异常 0。本批未运行应用分析或 Ghidra，不是 REA 核心分析业务通过。 |
| 安装与运行转换 | `npm run test:compat` **152/152**，`target/plugin-ui-boundary-compat.log`。删除旧转换专属的两项测试，以“不改写替换组件的业务代码”及安装／延迟注册拒绝回归替代；未降低剩余业务断言。 |
| daemon 权威准入与旧安装恢复 | `cargo test -p magi-plugin-system --test lifecycle` **26/26**，`target/plugin-ui-boundary-lifecycle.log`。同一宿主能力裁决阻止旧翻译消息替换，原生附加视图继续允许；重启后配置、摘要与授权记录保留。`cargo check -p magi-daemon` 通过，`target/plugin-ui-boundary-daemon-check.log`。 |
| UI 与发行物 | Web check **0 错误、0 警告**，插件商店回归通过；目录包及解包自检通过。日志 `target/plugin-ui-boundary-web-check.log`、`plugin-ui-boundary-plugin-store.log`、`plugin-ui-boundary-build.log`。安装错误、原生摘要和 REA 窄屏截图已人工核对。 |

真实回归同时修复共用设置文档的隐藏测量缺陷：隐藏文档上报零高度曾使阅读位置从 **120** 夹断为 **16.5**，表单与文档身份未丢失。共享 PluginFrame 在隐藏时保留最后有效高度，不向隐藏页下发无效尺寸上下文；恢复仍沿原 ResizeObserver。最终同一原包文档、未保存 `320000` 表单、daemon 已保存 `310000` 配置和滚动 **120** 均保持，证据 `settings-cache-return.json`。没有保存另一份表单或增加轮询／强制刷新。

删除 DSH 专用 `conversation-view.mjs`、助手结点伪投影、消息派生提交身份和连续 `setDraft`／`submit` 转换；Magi 原生 `chatAssistant` 与公共 composer 文本入口保留。新增运行阻断复用 PluginManager 的正式激活及能力裁决，不新增权限或协议字段；全局业务指引与已授权 guard 合同保持既有语义。

首次回归发现 WorkBuddy 的并列积分入口被未接入槽位检查误拒，已让左右输入贡献共用已有区域并重验。GenUI 首次检查被后端静态缺口遮蔽，已将原包 UI 意图检查前置，最终提示的是实际消息替换冲突。首次 REA 的滚动失败与诊断现场保留在 `ui-boundary-rea/`、`ui-boundary-rea-diagnostic/`；不拼接失败运行。

本轮未验证 Windows／Linux、所有原包业务、旧 GenUI 真实安装归档的 Electron 重启或全部升级场景；旧产物恢复由上述生命周期回归验证。下一步仅按具体新插件的用户任务评估公共能力，不再为 GenUI 恢复消息替换或另建主对话面板。



## 基线与一次审计

- 当前 checkout 为 `/Users/xie/code/magi-rust-rewrite`，`main` 基线 `2cefb9f3`，相对 `origin/main` 超前 64 个提交。DSH worktree 的 `codex/dsh-translator` 在 `fab8e63a`，已是 main 祖先，仅有未跟踪探针；`project-multi-root` 在 `5cedb6ab` 且干净。保留其他 worktree 和探针，不据名称迁移或清理其他任务。
- 主目录存在本次之前的未提交修改，包含原生 UI 隔离、插件页面模型用量、动态工具目录、管理恢复、Free Search 接入及旧路径删除。按现有源码核对，未提交不等于未实现，历史验收也不等于当前发行物已通过。
- 上轮同一 macOS arm64 目录包的 Free Search 0.9.41 完整流程 `free-search-lifecycle-verified/` 为 12/12，原生搜索、实际公网内容抓取、停用恢复及应用重启均通过。其余四包须在最终发行物复验。
- 已核对准备入口、真实 Cordis 启动、共享依赖解析、profile 投影及包管理转换、daemon 安装与授权入口、模型账本、联网提供方选择，以及 InputArea／MessageItem／PluginFrame 的展示链。旧 pi 静态预研和多生态工厂已从工作目录移除；当前消费方不引用旧模块，最终发行清单与相关回归已核验。
- 发现现代设置 `describe()` 已公开 revision，`mutate()` 却忽略 expectedRevision。旧表单可覆盖新设置，是本轮明确的数据一致性缺陷；应在同一 daemon 设置 CAS 提交前核对版本，保留原 DSH 冲突语义。
- WorkBuddy 启动回退目录中的 hy3 随真实在线目录更新被撤下，旧验收过早选择会造成 UNKNOWN_MODEL。验收应固定当前真实可用的模型并等目录稳定，不静默换模型；产品不恢复被上游撤下的模型。

## 各层所有权

| 事实或行为 | 唯一所有者与正式入口 | 其他层职责 |
| --- | --- | --- |
| 安装、版本、授权、启停、更新及恢复 | daemon PluginManager／PluginOperations、安装事务及原生资源 CAS | 翻译准备输出候选；profile、文件、命令转换只投影事实、提交同一动作 |
| 会话、Turn、Task、审批及目录 | daemon 原生会话／任务服务与工具管线 | 插件携带真实调用身份；UI 不决定终态，Node 不建立会话内核 |
| 模型调用及用量 | 原生模型选择／传输及统一审计用量账本 | 上游适配器承载协议；页面调用归插件，原会话调用保留原归属 |
| 全局联网提供方选择 | PluginManager 的持久配置与版本 CAS | WebRuntime 注册业务；原生 web 工具沿唯一插件工具执行入口调用 |
| 原包业务、报告和缓存含义 | 原包业务代码 | daemon 仅管理正式宿主状态及提交机制，不迁入插件全部业务 |
| 工作台、正文、输入与审批 UI | Magi 共享组件 | 原包在隔离文档按需展示；聊天附加视图默认收起，原生正文保持可读 |

插件默认应用全局安装、配置、授权和启停，不增加会话／工作区二次启用。调用身份与服务权限仍按对应合同裁决。

## 冻结问题及完成清单

| ID | 本轮问题／工作单元 | 交付证据与退出条件 | 状态 |
| --- | --- | --- | --- |
| C1 | 完成旧离线预研、工厂和重复管理验收链清退审查 | 旧符号全局检索、发行物检查；保留实际 DSH 运行库和单一管理事务，不新增替代链 | 已核验，见下方记录 |
| C2 | 现代设置版本合同不完整 | 旧 revision 写入被拒且当前配置不变；同一 daemon CAS 仍是唯一提交点，运行回归及原页面保存通过 | 已完成；运行回归、Rust CAS 和最终目录包 REA／Free Search 原页面保存均通过 |
| C3 | 当前固定原包缺少同一发行物的业务证据 | 下表五项独立完整运行，逐项记录版本、摘要、任务及失败恢复；不能拼接失败运行 | 已完成；五项均使用最终目录包，实际业务、相关恢复及 UI 边界有完整证据 |
| C4 | 验收目录等待重复且依赖已撤下模型／移动 latest | 使用一个共用验收助手取得固定模型；安装版本冻结；不新增产品模型选择链 | 已完成；四个模型专项取得固定在线模型，五包安装及 Mgr 目标升级版本与冻结清单一致 |
| C5 | 旧目标与历史记录没有有限交付终点 | 文档明确本次范围、支持矩阵、清理结果和剩余缺口，完成审计逐项核对后结束目标 | 已完成；以下矩阵及完成审计为本次退出依据，生态扩展与后续事项另行立项 |

2026-11-10 用户截图确认的 UI 回归纳入 C3：WorkBuddy 将同一 `WorkBuddyCard` 注册为设置与 main，旧准备逻辑生成两个编辑入口。配置必须只在设置页的独立插件 Tab 展示；按真实组件引用在共用记录入口剔除 main 别名，保留独立业务组件。该修复后重新构建，五包均以新目录包验收；上一目录包的 WorkBuddy 13/13 留作过程证据，不能替代最终 UI 结果。

## 此前批次的冻结业务矩阵（历史证据）

平台固定为 macOS arm64、Electron 45 目录包及随包 Node／daemon。模型固定 WorkBuddy 5.21.2 的 `deepseek-v4.1-flash`，保留真实在线目录证据；模型不存在时明确失败。REA 依赖本机 rea-agents 6.5.0。下面列出本次完整任务，而非插件所有功能。

| 样本及版本 | 冻结用户任务与适用恢复 | 验收入口 | 当前结果 |
| --- | --- | --- | --- |
| WorkBuddy 5.21.2 | 在线预览／安装、原页面、真实模型答复、停用撤销数据源、重新启用与应用重启恢复配置及模型目录 | test:electron-workbuddy | 最终目录包独立完整 15/15，`convergence-workbuddy-visible/installed.json`、`results.json`、`settings.png`；配置只在设置 Tab，截图已核对实际绘制内容 |
| REA 1.2.0 / rea-agents 6.5.0 | 搜索相关度与 SVG／Ghidra 链接、原页面设置 CAS、390px、启停恢复工具；新个人会话经原生审批执行 JS 应用分析并核对 IPC、桥 API 与原文件摘要 | MAGI_REA_BUSINESS=1 test:electron-dsh-rea | 最终目录包独立完整 20/20，`convergence-rea-delivery/analysis-input.json`、`analysis-turn.json`、`analysis-task.json`、`analysis-tool-receipt.json`、`analysis-recoveries.json`；真实模型完成单次分析及同证据追踪，最终字面值与原始回执及源文件摘要一致 |
| Free Search 0.9.41 | 原页面保存 Bing／实际搜索；两个新会话的原生搜索及公网 DNS JSON 抓取；旧 0.9.39 候选真实失败保留现有实例；停用回原生、重启恢复 | test:electron-free-search | 最终目录包独立完整 12/12，`convergence-free-search-delivery/evidence.json`、`results.json`；真实搜索／公网抓取、停用与重启通过 |
| GenUI 0.3.3（历史合同） | 原生安装／全局指引告知；模式保存与390px菜单、真实模型原卡片及继续对话；正文不变、草稿冲突／显式恢复、重复点击、收起回收、停用及重启去重 | node scripts/verify-electron-genui.mjs | **已被本轮边界合同取代，当前因消息替换而拒绝。** 旧目录包独立完整 24/24，`convergence-genui-verified/identity.json`、`results.json`；原生正文／草稿／重启去重通过，16 份裁切回执均 cut=0、hidden=0 |
| Mgr 0.2.12 → 0.2.13 | 自身在线更新预览／取消／提交／原生授权；目标包更新、未授权真实回执与恢复、解析失败及 daemon 中断恢复、启停、390px 与原页面、重启和卸载 | test:electron-dsh-profile | 最终目录包独立完整 60/60，`convergence-manager-delivery/manager-business.json`、`results.json` 及宽窄屏／八种外观截图；5 份裁切回执均 cut=0、hidden=0，在线更新、取消、实际 daemon 中断恢复及卸载通过 |

所有相关静态／协议检查和失败回归须通过。截图核对实际原页面及原生正文；不以 checks 数量代替结果。业务专项串行运行，避免多个 Electron 实例互相抢占物理焦点。最终生产修改后重新构建目录包；只改验收驱动不要求重复构建。需要修复同一清单缺陷时复跑受影响业务。

## 本次实施与证据

- C1：源码全局检索 `TranslatorRegistry`、`createRegistry`、`createPiTranslator`、`PI_FLOW`、`PI_RULES`、`PI_IMPLEMENTED` 和旧模块路径均无消费者；`scripts/compat/translate/`、`pi/`、旧工厂入口已移除。`cli.mjs` 直接使用 DSH 检查与同一安装准备入口，保留真实原包运行时。Mgr 专项与社区入口共用 `verifyDshManager`，Share／Market／Upgrader 的专项与共用验收链此前已统一，不另建产品管理事务。
- C2：新增运行回归先复现旧 revision 写入没有被拒绝；现代 `mutate`、`update`、`replace` 现在在读取 daemon 最新版本后核对调用方版本，并保留上游 `SettingsConflictError`、`SETTINGS_CONFLICT`、expected／actual。最终提交继续调用同一 `api.settings.write` CAS；无写入时不发布更新，成功事件携带实际版本。旧版本三种写法拒绝且配置不变、重新读取后的保存已通过；`node --test --test-name-pattern='现代设置' scripts/compat/runtime.test.mjs` 为 2/2，相关 Rust 资源合同 4/4。
- C4：四个需要真实模型的专项共用 `waitForPluginModel`，固定 `deepseek-v4.1-flash` 并保存在线目录；等待仅在验收驱动，产品选模未增加路径。原生 UI 专项通过 CDP 只固定提交给真实 daemon 的预览版本参数，不替换任何网络响应、原包或业务结果；确认安装仍使用真实预览摘要。WorkBuddy 增加原目标要求的完整应用重启及配置、授权和模型目录恢复验证。
- 相关检查：UI 入口修复后的 `test:compat` 151/151（`convergence-compat-final.log`），现代设置 2/2、Rust 资源合同 4/4、daemon 共用设置／资源服务回归 1/1。Web／Desktop 检查、`protocol:check`，以及 Web plugin-store／plugin-composer／settings-store／session-stats 回归通过。Web 首次检查暴露的旧 VS Code 条件已由工作区其他改动移除，复检通过；保留这些已有改动，没有扩大本次插件修复范围。
- UI 入口修复阶段的目录包：`convergence-build-ui.log` 的标准构建和解包自检通过，`convergence-build-ui-identity.json` 保存其身份。这些记录作为过程证据保留；当前交付以完成审计列出的最终目录包及五项独立完整运行替代。
- REA 首次完整运行 `convergence-rea/` 的模型最终给出正确 IPC／API，但中途分别提交了 schema 不接纳的 history 包装及存在歧义的模块路径选择，被原生／原包拒绝后重试。该次仍因“全部证据工具成功”断言退出 1，不计通过；后续驱动明确保留证据引用的原生形状、每个新工具先读 schema，以及先取得唯一模块节点再选择的指引。保留原断言、失败记录和同一真实业务任务，完整重跑而不拼接结果。
- 第二次 `convergence-rea-ui/` 仍因模型的枚举参数和证据类型选错而失败，但真实 Task 最终完成、只分析一次并给出全部原证据名称。原“每次中间调用均成功”的断言把正常参数拒绝及显式纠正也判成整任务失败，未对应本轮成功／失败恢复合同。替代验收逐项检查：分析器必须唯一且实际成功；中途仅允许原生准入的 `tool_arguments_invalid`／rejected 或原包明确 `invalid_request`／invalid_input／retryable 输入拒绝；每次拒绝后必须在同一原证据上取得后续成功回执；派发前拒绝没有审批，实际进入原包的调用仍核对原生审批数量。拒绝与恢复关联单独落盘，未知错误、超时、权限失败、重复分析、最终答案缺失及文件变化继续失败。随后以新门槛完整重跑；此前两次不追认通过，不变更产品权限或原包。
- 第三次 `convergence-rea-final/` 分析器及多个证据读取成功，但模型构造了未提供的 `magi_plugin_plugin_…` 名称，最终 Task 明确失败，驱动也拒绝非指定工具。核对目录返回的是实际 canonical `model_tool_name`，传输 codec 按当前工具面维护唯一可逆映射，未发现可据此修改 codec 的证据；不能加别名猜测来接纳未知调用。该完整流程仍未交付，下一步沿同一原包、固定模型和新会话核对模型实际所见的目录／工具面及任务指引，不重做安装、设置或 UI 架构。

目录投影修复前，冻结业务曾为 4/5，REA 未通过；上述失败记录保留，不追认通过。当前五项业务均已在最终目录包独立完整通过，以下完成审计为本轮结论，不表示生态支持率。

2026-11-10 继续核对 REA 失败：`convergence-rea-final/analysis-turn.json` 中定向请求的目录回执达 28 KB，单个 schema 约 21 KB，超过模型单条结果的 12 KiB 展示预算。原始回执已用于加载完整工具定义，但目录正文被通用首尾预览截断，名称、准入和加载事实不再以完整 JSON 交付。修复归属模型历史唯一投影：超预算目录输出结构化元数据，保留工具身份、可用性及 schema 加载标记；完整 schema 继续由原回执进入下一步 tools，不在目录正文重复。描述优先让位于身份与准入；极大目录只能按完整条目省略并明确数量及现有 query/tool_names 收窄方式。canonical 回执、准入、加载和执行链不变，无插件名分叉。该缺口纳入 C3，已用同一冻结 REA 原包在最终发行物完成业务复验，没有用投影回归冒充业务通过。

`convergence-rea-catalog/` 的完整 Task 已完成，单次分析并从同一保留证据报告正确 IPC、API 和成员；但驱动因一次中途未知名称而退出 1，仍不计通过。原回执明确是 `tool_not_available`、`tool_call_validation`、`rejected`、`expected_input_schema:null`，未进入插件执行，后续同一证据的正式工具调用成功。这属于可审计的原生准入拒绝及恢复，原“任何目录外名称出现均失败”断言过度约束模型输出，不能代表有目录外工具被执行。驱动现逐次核对上述拒绝字段、真实父证据及后续正式成功回执，单独落盘；目录外实际执行、不能关联的拒绝、权限失败、未知错误、副作用和最终任务未完成仍失败。不修改 codec、不新增别名，也不追认此前运行；以下一完整新运行证明业务和恢复。

## 完成审计与退出

- 该次汇总路径为 `target/plugin-acceptance/convergence-completion-audit.json`，记录最终目录包摘要、五个原包版本与独立完整结果，以及 21 份裁切回执。当前工作区未保留此构建产物；下述数字是该次验收记录，不能作为本轮重新验收的证据。
- 最终构建：`convergence-build-final.log` 标准目录包构建及解包自检退出 0；`convergence-build-final-identity.json` 记录 Magi **3.0.62**、Electron **45.0.0**、37,547 个资源条目，manifest SHA-256 为 `ea6077c971763c8a48bdcf417d2b4616a995d69b89d49e233f378327c79947b1`。五项业务串行使用该包，验收后再次核对摘要相同；旧离线目录、pi 预研及旧工厂均未进入发行物。
- 五项冻结任务均取得上表单次完整运行证据，版本固定；不拼接中途失败的检查数。REA 最终 Task completed，19 个真实模型步骤，仅调用原生目录及指定 REA 工具，没有未知工具名；一次原包 `invalid_request` 输入拒绝在同一父证据上由后续真实成功调用恢复，记录于 `analysis-recoveries.json`。分析器只执行一次，最终答案包含实际 IPC `rea-audit:read-version`、API `sampleApi` 及成员 `readVersion`，源文件原字节与 SHA-256 保持一致。
- GenUI 首次终版运行 `convergence-genui-delivery/` 在重复点击的命中位置等待阶段超时，失败截图处于设置页，未计业务通过。增加的仅是驱动几何／可见性失败取证；独立完整复跑 `convergence-genui-verified/` 通过全部 24 项及裁切审计，未复现该超时，不宣称已修复一个未确认的产品根因。
- WorkBuddy 首次终版截图在 DOM 检查完成后保存了尚未完成绘制的空白内容区，不能作为视觉通过证据。截图驱动改为等待两次浏览器绘制回调后取图；同一包独立完整复跑 `convergence-workbuddy-visible/` 15/15，人工核对 `settings.png` 的原包内容、页头和独立设置导航均实际可见。没有增加产品等待、轮询或 UI 补丁。
- 相关验证：翻译回归 151/151，现代设置冲突 2/2、Rust 资源合同 4/4、daemon 设置／资源服务 1/1；本轮目录投影与恢复相关 Rust 回归 **25 + 4 + 7 = 36** 项通过。大 schema 仍完整进入真实工具定义，停用／拒绝／只读策略重查以及原生代理恢复身份均有断言。`cargo check -p magi-daemon`、Web／Desktop 检查、`protocol:check`、涉及驱动的语法检查及 `git diff --check` 通过。
- 产品边界维持：插件默认全局，配置只在设置页独立插件 Tab；原生会话正文、输入、审批和管理入口由 Magi 拥有。独立业务页及会话交互保留正式位置与受控动作。daemon 仍是安装、版本、授权、启停、会话、任务及提交的唯一所有者；Node 执行业务和资源生命周期，翻译器适配公共服务，UI 消费事实。
- 新增复杂度仅为同一历史投影中的有界目录展示及相关回归／取证，没有新的状态源、执行器、插件分叉或别名兼容层。保留必要 DSH 运行库与 profile 只读投影；已清退的离线改包、pi 预研、旧工厂和重复管理验收路径没有恢复。
- C1—C5 均有对应代码、清理、验证及本文结果。本轮有限交付完成并停止；只承诺上述平台、版本和任务范围。主目录仍在 `main`，保留用户及其他任务的已有未提交修改；没有发布或清理其他 worktree。

## 插件设置页切换回归（2026-11-10）

- 用户反馈的重复加载来自设置分类仅挂载当前插件页，切换时销毁隔离文档。设置面板现在只在首次访问时挂载，按视图身份保留已访问页面；隐藏页面退出布局和焦点导航，停用／卸载释放文档，版本和运行实例变化仍由既有页面宿主失效。没有新增配置存储或改变插件全局启停规则。
- 当前源码经 macOS arm64 的开发 daemon 与 Electron 窗口验证，在线原包 `dsh-rea@1.2.0` 的设置页在三次分类切换及返回对话后，保留同一文档、真实表单未保存输入和设置页滚动位置；隐藏时停用释放文档，重新启用创建新文档。证据为 `target/plugin-acceptance/settings-retention/rea-results.json` 与 `rea-retained.png`。这是设置页生命周期验收；该隔离环境未安装 REA CLI，不计分析业务通过，也不替代上文发行物业务验收。
- 共享本地插件 UI 回归已覆盖按需挂载、草稿与整页滚动保留、隐藏隔离及重新启用失效；原包 REA 专项同样保留对应断言。Web 检查、设置 store 回归与涉及驱动的语法检查通过。本次未重新构建发行目录包。

## 多类型原包抽验（2026-11-10）

用户要求再安装不同类型插件验证实际使用。本轮以当前源码重新执行标准目录包构建，macOS arm64、Magi 3.0.62、Electron 45.0.0；包含设置页保活修复，构建与解包自检退出 0。构建身份、各次独立运行及截图保留在 `target/plugin-acceptance/multitype-20261110/`，汇总见其中 `audit.json`。

| 原包版本 | 实际用户任务 | 本次结果及证据目录 |
| --- | --- | --- |
| `dsh-free-search@0.9.41` | 原页面保存引擎、实际搜索；真实模型经 Magi 原生 `web_search`／`web_fetch` 取得搜索来源和公网 DNS 内容；停用与重启恢复 | 通过，独立完整运行 `free-search/`；模型连接使用在线原包 WorkBuddy 5.21.2 与实际 `deepseek-v4.1-flash` |
| `dsh-plugin-appshot@0.4.2` | 真实本机窗口截图经原生辅助进程进入输入框，核对持久附件字节与摘要；配置、停用和重启恢复 | 通过，独立完整运行 `appshot/`，`core:capture-mounted`；只截图验收专用窗口 |
| `dsh-plugin-share@0.2.5` | 原页面导出／解析组合码，取消与重试在线导入，原生授权恢复，SVG／PNG 二维码保存和识别，GitHub 来源更新及完整重启 | 通过，独立完整复跑 `share-verified/`；导入的 Release Probe／Health 仅为该业务的真实目标包，不另计为它们的核心业务通过 |
| `dsh-genui@0.3.3` | 原生安装授权、输入区提示模式及交互卡片 | 未通过完整流程，`genui/`：宽屏模式选择成功，390px 菜单悬停后物理位置漂移，固定坐标不再命中选项；后续模型卡片业务尚未执行，不计通过 |

Share 首次运行 `share/` 停在下一份组合码的解析：页面保活后，原包按设计保留了授权失败的暂停队列，原生授权不应清空该队列。旧驱动漏掉原页面的“跳过并继续”，此前页面重建掩盖了该遗漏。共用验收现在明确执行此操作，再检查安装账本未变化、失败历史未清除及下一份组合码可解析；未改产品、原包或既有授权规则。`share-verified/` 独立完整通过全部 25 项，首次失败仍保留，不拼接两次结果。

本轮四项抽验中三项完成核心业务，一项被窄屏交互阻塞；这不是生态覆盖率。GenUI 现场包含几何记录、原包文档和失败截图：左侧尺寸补偿持续增长，需沿共享浮层尺寸链路单独修复，不能用原包 CSS 补丁或重试点击掩盖。本轮未扩大为插件架构改造，未修改用户既有安装、主对话布局或发行版本。

## WorkBuddy 用量与输出速度核对（2026-11-10）

以下为此前“精确正文速度”口径的历史验收；用户随后要求平均速度不能因缺少思考拆分而为空。当前主指标已改为整体响应吞吐，见本文“整体输出速度的产品口径修正”；精确正文速度仍独立保留在性能明细。

用户反馈原生统计不及时、缓存有值而输出速度不更新。本轮限定同一 `dsh-connect-workbuddy@5.21.2`、在线 `deepseek-v4.1-flash` 的真实主对话，追踪原包适配器 → 仅本机 Chat 流 → HttpModelBridgeClient → Step/用量账本 → usage 事件 → SessionStats。

根因已复现：DSH TokenUsage 的未缓存输入和缓存读写互斥，桥却把 `inputTokens` 直接作为 Chat 完整 `prompt_tokens`。修复前长回复回执为输入 **494**、缓存读取 **21,120**，被用量权威判为不可能的缓存观测并排除，第二次命中样本和缓存比例因而没有变化。事件及 UI 自动刷新本身正常，证据 `target/plugin-acceptance/workbuddy-stats-before/stats-counts.json`、`stats-refresh.json` 和 `target/workbuddy-stats-before.log`。

公共模型桥现按互斥输入三类之和生成完整 Chat 输入；保留真实 total、读写缓存及推理 token 的零／缺席。Chat 的共用流式／非流式 parser 接收明确的缓存写入字段，在同一 LlmUsage 合同中拆出写入量，后续仍使用原用量权威、审计账本和现有统计事件。没有按包名分支、第二套统计、UI 补算、轮询或自动重放，也不重写历史错误回执。

当前 WorkBuddy 适配器的最终 usage 未交付 `reasoningTokens`；其底层 pi-ai 映射将推理并入 output 后未提供精确拆分。Magi 收到了真实首末正文计时，但无法证明全部 output token 都属于正文，因此按现有性能合同继续把 TPS 标为未知。不能用字符估算、填零推理量或把包含思考的输出除以正文区间来伪造速度。若上游明确提供拆分（包括零），桥原样透传并由同一 HTTP 接收计时与用量权威计算；对应真实本机 HTTP 测试已覆盖。运行中继续展示已结算统计，usage 结算后自动更新，不以定时动画冒充 token 计量。

验证使用重新构建的 macOS arm64 / Magi 3.0.62 / Electron 45 目录包，随包 Node／daemon：

- `MAGI_WORKBUDDY_STATS=1 ... node scripts/verify-electron-workbuddy.mjs` 独立完整 **22/22**，证据 `target/plugin-acceptance/workbuddy-stats-final/`、`target/workbuddy-stats-final.log`。在线安装及原生设置、真实短／长模型回复、保持统计面板打开后的自动刷新、停用／重启恢复均通过，页面异常 0。与旧失败运行不拼接。
- 最终长回复完整输入 **21,615**、缓存读取 **21,120**、未缓存输入 **495**、输出 **2,595**；两次累计缓存样本 **2/2**、读取 **25,344**、完整输入 **43,070**、命中 **58.8%**、累计输出 **2,607**。usage 事件后 **152ms** 内自动取得新版统计；实际面板显示 **2.6K** 输出、**2** 次首 token 样本和 **58.8%**。正文接收区间 **4,321ms**，由于未提供精确推理拆分，TPS 保持未知。数据、计时、UI 和完整重启后的统计分别保留在 `stats-counts.json`、`stats-long-turn.json`、`stats-refresh.json`、`stats-ui.json`、`stats-restored.json`；`stats-updated.png` 已核对。
- `npm run test:compat` **154/154**，`target/workbuddy-stats-compat.log`；`cargo test -p magi-bridge-client --lib` **232/232**，`target/workbuddy-stats-http-tests.log`；`cargo check -p magi-bridge-client` 通过。覆盖互斥缓存转换、零与缺失、真实本机 HTTP 正文即时交付、最终推理拆分透传、共用 parser 与现有计时／账本回归。目录包和解包自检通过，`target/workbuddy-stats-build.log`。

本轮没有更改主对话布局或统计口径。真实正文 TPS 在上游缺失拆分时仍未提供；未验证 WorkBuddy 其他模型、其他平台和用户既有安装的在线组件更新，历史错误统计保持原始证据。


## 用量遗漏复核（2026-11-10）

本批只复核上述 WorkBuddy 统计流程的转换、异常结算、取消、恢复和展示，不扩展插件能力或主对话 UI。确认两类遗漏并完成修复：

- 公共模型桥将正文前收到的用量暂存至首段正文，上游失败时会丢掉这些观测；异常抛出没有 SSE 错误，缺少 finish 的 EOF 仍生成 DONE，提前发送成功 finish 也可能遮蔽后续异常。真实本机 HTTP 的五种失败场景均在修改前失败，`target/workbuddy-stats-audit-before.log`。现即时透传用量，流中失败明确发送错误，正常 finish 等迭代器退出后才提交；异常退出使用同一迭代协议释放上游资源。已有 HTTP recorder、canonical Step、账本与失败裁决继续拥有事实，没有第二条结算路径。
- 设置统计只对图片模型区分用量缺失与零，文本模型的总计、模型、角色和交叉明细仍可能显示假零。现全部使用 daemon 的 `tokenObservedCallCount`，未知显示“未返回”，实际零保持零，部分缺失显示已报告合计和覆盖数；删除图片专属判断及过时说明。UI 只聚合已有回执，未新增持久状态或推断 token。

修改后的 macOS arm64 / Magi 3.0.62 / Electron 45 目录包经重新构建、解包自检，并取得以下独立证据：

- 同一在线原包 WorkBuddy **5.21.2**、`deepseek-v4.1-flash`，独立完整 **22/22**，`target/plugin-acceptance/workbuddy-stats-audit/`、`target/workbuddy-stats-audit-live.log`。真实短／长回复、保持面板打开后的统计自动更新、缓存累计、停用及完整重启恢复通过；两次调用累计输出 **1,085 Token**，缓存读取 **25,344 / 43,080 Token**，无重复计数、未捕获页面异常。
- 真实 Electron/daemon 加本机受控 HTTP，`target/plugin-acceptance/stats-usage-presence/evidence.json`：主模型与子代理统计、完整重启、未知用量 **0/1** 与明确零值后的覆盖 **1/2**、模型／角色两种视角和保持统计页打开后的自动更新通过。`evidence.json.png` 已核对，设置页裁切审计 `clip-audit.json` 为 **cut=0**。该夹具只证明核算及 UI 合同，不冒充 WorkBuddy 业务。首次脚本把已展开面板再次点击收起，以及错误地仅在最后一条动态上下文查找夹具标记，分别保留失败日志及 `diagnostic-known-usage.json`；只修正测试驱动，没有修改产品等待、事件或准入规则。
- 兼容层完整回归 **159/159**，`target/workbuddy-stats-audit-compat.log`；随后新增的无用量拒绝、finish 后最终用量和取消释放连同五种失败情形的定向回归 **8/8**，`target/workbuddy-stats-audit-bridge.log`。Rust 实际 HTTP 失败保留用量 **1/1**、取消释放 **2/2**、用量权威 **37/37**、canonical 恢复 **5/5**、辅助／插件恢复 **4/4**、用量记录 **20/20**；相关日志为 `target/workbuddy-stats-audit-{failure,cancel,authority,recovery,auxiliary,recording}.log`。Web check 为 0 错误、0 警告，设置、会话统计及 i18n 回归通过。

按用户确认的验收归责，WorkBuddy 未提供精确推理拆分时 TPS 显示未知，是翻译器正确保留和展示缺失状态，不列为缺陷、待办或退出阻塞；翻译器仍须完整传递实际收到的内容与计数。本批真实原包只覆盖上述模型的成功与停用／重启；错误和请求中途取消由本机 HTTP 回归验证，未宣称完成 WorkBuddy 所有远端失败阶段或其他模型、平台的实测。第三方插件和服务自身的问题不由本任务代修。

## 翻译器自身遗漏收尾（2026-11-10）

本次按用户最新归责完成翻译器自身的消息转换和宿主接入，不代修第三方业务，不扩展 Magi 主对话干预权。原包缺少的计量继续显示未知，不再当作翻译器待办；不能用默认零值、字符估算或假终态补齐。

- 原页面模型业务现在沿同一原生模型调用传递图片、工具定义、工具调用历史及结果、生成参数、推理续写和实际用量。工具函数属于插件业务，不因此获得 Magi 原生工具执行权。默认模型目录只投影实际选择，不再次发布为模型来源。
- 同一调用的正文／推理增量在最终响应前交付原包运行时；消费者退出中止同一请求。流中失败、正文改写或最终结果与已交付片段矛盾明确失败，不补发成功、不自动重试。进度复用现有宿主连接、代次及路由租约，已确认结果仍由 daemon 的持久操作账本拥有。
- 修正附件读取的归属：持久图片的真实媒体类型、尺寸及字节数由 daemon 回执决定，模型图片处理使用该回执；调用者提供的元数据不能覆盖权威值。请求临时图片仍按本次请求身份校验，没有本地持久副本或另一套图片存储。
- 补充原包回归发现 Appshot 0.4.2 在线预览被误拒：读取 `ctx.webServer` 命中了“未支持服务器成员”。在现有能力目录中单独声明已提供的服务读取，保留未接入的 host、port、外壳注入和未知成员的直接使用拒绝。没有开放端口、添加原包分支或修改页面。

| 验证范围 | 本次独立结果与证据 |
| --- | --- |
| Ops 0.20.8，WorkBuddy 5.21.2 / deepseek-v4.1-flash | **15/15**，`target/plugin-acceptance/translator-completion-ops/`：原页面诊断答复、知识沉淀、取消、独立插件用量、完整重启不重放、统计恢复及重置；原生 canonical 未变、页面异常 0，原页面及统计截图已核对。不把原包对 DSH 环境的扫描结论当作 Magi 的真实安装裁决。 |
| WorkBuddy 5.21.2 / deepseek-v4.1-flash | **22/22**，`target/plugin-acceptance/translator-completion-workbuddy/`：真实短／长回复、即时统计刷新、缓存与完整输入、首 token 及分段接收计时、启停和重启。截图显示累计输出 **907 Token**、缓存 **58.8%**；上游未提供精确正文拆分，TPS 正确显示未知。 |
| REA 1.2.0 / rea-agents 6.5.0 | **22/22**，`target/plugin-acceptance/translator-completion-rea/`：SVG、原表单／Ghidra 链接、**207** 项工具、设置 CAS／重载、390px、页面保活、草稿／滚动、停用回收及重新启用；本批不计 REA 分析业务通过。 |
| Appshot 0.4.2 | 修复后重新构建的最终目录包 **14/14**，`target/plugin-acceptance/translator-completion-appshot-final/`：真实原生辅助进程、专用窗口截图进入输入框、daemon 持久附件字节及摘要一致、停用回收和完整重启恢复，`core=capture-mounted`、页面异常 0。首轮准入失败保留于 `translator-completion-appshot/`，不拼接通过数。 |
| 转换与公共协议 | 翻译完整回归 **165/165**，`target/translator-completion-final-compat.log`；真实 HTTP 图片／工具／控制及持久身份回归 **2/2**，`translator-page-model-tests.log`；真实 Node 宿主 **28/28**，`translator-completion-host.log`；模型桥 **233/233**，`translator-completion-model-bridge.log`；daemon 服务与当前实例授权 **12/12**，`translator-completion-daemon-service.log`。覆盖最终结果前交付、消费者退出、失败／改写、工具历史、图片及实际参数，不以接口字段存在代替流式证明。 |
| 构建与检查 | 最终 macOS arm64 / Magi 3.0.62 / Electron 45 目录包构建和解包自检通过，`target/translator-completion-final-build.log`；daemon check、Web check（0 错误／0 警告）、protocol check 及差异检查通过。全仓格式检查仍有其他目录既有格式差异，未修改这些无关文件；本次 Rust 文件未报告格式差异。 |

Ops、WorkBuddy、REA 使用本次模型链修复后的目录包；最终包仅追加上述 Appshot 服务读取准入修正，并回到同一 Appshot 原包完成全流程，未将不同运行拼为一批生态支持率。协议、作者指南、能力说明及验收入口同步到实际字段与限制，旧“工具／图片／生成参数未接入”的当前状态已更新。

本次确认的上述翻译器自身缺陷已闭环。没有新增第二套会话、审批、调度、配置、安装或用量提交链；没有调整原生主对话布局。普通页面 fetch 仍遵守既有整段响应合同，本次正文进度指原包运行时的模型迭代器，工具参数在最终确认后交付，不冒充浏览器逐块 HTTP 或逐字符工具参数。工具／图片的真实协议合同由受控 HTTP 与真实 Node 验证，第三方真实业务实测范围以本表为准；其他平台、全部原包及任意上游扩展不在本次完成声明内。代码保留在 main 工作目录，本地验证包已重建，未发布线上版本。

## 整体输出速度的产品口径修正（2026-11-10）

用户要求正常模型调用不能仅因未拆分思考 Token 而不显示平均输出速度。当前会话统计主指标统一使用 **同次真实总输出 Token ÷ 完整成功请求接收耗时**，包含思考、首段等待和尾段接收；多次调用按总输出／总耗时聚合，不把整体速度与正文速度混合平均。精确正文速度仍以原正文 Token 和正文接收区间计算，在性能明细独立展示，缺少拆分时该明细保持“未提供”。

同一 HTTP 传输记录器在原 `ModelResponseTiming` 增量记录可选 `responseDurationMs`，与最终实际输出配对，计时使用网络接收时间，不由 UI 或 SSE 逐条解析重新计时。用量权威在原聚合器增加整体样本、输出、时长和最近速度；UI 只投影同一账本，沿原 usage 事件自动刷新。失败、取消和未知用量不计入成功速度；真实零值保持零。旧回执没有完整请求耗时时不补造样本，不把历史正文计时重解释为整体计时；重启不重放模型或重复计数。

以重新构建的 macOS arm64 / Magi 3.0.62 / Electron 45 目录包，在线原包 WorkBuddy **5.21.2** 与固定 **deepseek-v4.1-flash** 完成独立 **25/25**。证据 `target/plugin-acceptance/response-speed-workbuddy/`：两次实际输出合计 **2477 Token**，完整请求合计 **24235ms**，平均 **102.2 tok/s**；最近一次 **2465 Token / 21955ms = 112.3 tok/s**。正文拆分仍缺席，精确正文样本 **0**，主指标已有真实值。usage 事件后 **153ms** 自动刷新；输入区与面板数值一致，390px 展开明细无横向裁切，完整应用重启后统计完全相同。宽窄屏截图 `stats-updated.png`、`stats-narrow.png` 已核对；审计计算见 `response-speed-audit.json`。

相关回归：模型桥 **233/233**，用量权威 **37/37**，canonical 恢复 **5/5**；Web check **0 错误／0 警告**，会话统计投影、i18n、protocol check 及目录包解包自检通过，日志 `target/response-speed-*.log`。零值回归首次把后续未知请求的“最近速度”也断言为先前零速度，失败保留在 `response-speed-authority.log`；已改为分别验证已确认零请求的零值，以及后续未知请求不继承旧速度，最终 **37/37** 见 `response-speed-authority-final.log`。没有修改产品或放宽未知值规则来满足旧断言。

本轮修改仅涉及真实计时、既有用量聚合与统计展示，未改变插件或主对话执行合同。旧数据仍保留原始证据；当前完整计时由新请求产生，不为填满界面捏造历史速度。本地目录包已重建，未发布线上版本。

## 翻译层与原生模型合同收敛（2026-11-10）

本批围绕用户明确的边界完成一个工作单元：插件页面通过既有公共模型服务执行业务，DSH 的接口形状只存在于翻译器中。固定业务样本为 Ops 0.20.8 的原页面诊断／知识沉淀及 WorkBuddy 5.21.2／GLM-5.2 模型来源，显式选择 none 以覆盖关闭推理的转换，不新增插件内置功能、UI 插槽、执行器或服务方法。

- **问题与证据**：`plugin_models.rs` 的 `model.selection` 直接生成 DSH 的 `efforts:[{id,name}]`，还将 Magi 的 `none` 改为 `off`；`native-model.mjs` 再原样交给 DSH。原生插件因此只能看到上游生态格式，职责方向相反。模型选择、身份校验、调用与账本的唯一所有者继续为 daemon。
- **最小修改及消费者**：原生服务直接序列化已有 `ReasoningCapability`，返回 `supportedEfforts/defaultEffort/transport/source`，不添加新的业务事实。翻译器的共享推理转换为真实 DSH LlmRuntime 生成 `efforts/defaultEffort`；原始 selection 不改写，调用时仍按 daemon 快照校验，只有所选档位在翻译层转换。已核对 SDK 仅透传快照、现有原生插件和已确认结果读取没有另一套字段变换；原包消费者只有该适配器。删除原生服务中的 DSH 档位闭包，不保留旧形状分支。
- **验收门槛**：原生插件看到原生能力且没有凭据或新 Session；DSH 原包仍看到自身 off 等档位；实际调用回传的 selection 与原始快照相同。继续覆盖实际页面答复、知识结果、取消、账本与重启不重放。回归夹具不替代原包业务。
- **已核对且保留的边界**：常规会话、模型配置与主对话 UI 没有按 WorkBuddy／Our Free Model 等包名启用的功能分支。兼容运行库装配、上游安装来源及版本豁免属于插件接入入口；旧翻译产物的助手正文替换拒绝仍保护已有安装的重启／激活，不能当作重复业务代码直接删除。原生 UI 只消费已激活的公共贡献，主正文和输入框继续由 Magi 拥有。

验证完成：

- 原生模型服务回归 **3/3**：使用普通 Node 插件身份读取完整原生 ReasoningCapability，关闭推理仍为 none；既有真实 HTTP 的图片、工具、采样、持久结果读取及禁止重放回归通过。日志 `/tmp/magi-native-boundary-model-complete.log`。
- 翻译运行回归 **55/55**：真实 DSH LlmRuntime 看到 off／low／high，调用回传的 selection 与原生快照逐项相同，关闭档位回传 none；未知能力及无效默认档位拒绝。日志 `/tmp/magi-native-boundary-runtime.log`。
- macOS arm64 / Magi 3.0.62 / Electron 45 当前目录包构建及解包自检通过，随包 native-model／reasoning 模块与源码一致。真实 Ops 原包完整 **15/15**，证据 `target/plugin-acceptance/native-model-boundary-ops/`：GLM-5.2 真实主对话及原页面诊断、知识沉淀、取消、独立账本、重启不重放和统计恢复；原生 canonical 对话未被页面业务追加或替换，Renderer／原包异常 0。`native-selections.json` 保存同一批原生账本中的三次模型选择，均为 supportedEfforts + defaultEffort:none，未包含 DSH efforts；`plugin-model-usage.json` 两次成功及一次取消均保留原生 none。截图已核对原页面展示于独立插件设置 tab；原包自身的 DSH 环境扫描建议仍是其业务内容，未作为 Magi 的安装裁决。

本批只收敛已定位的模型合同耦合，不将其他原包、平台、插件全部功能或全仓历史改动计为已验证。

## 安装影响、授权与停用流程收尾（2026-11-10）

本批只闭环用户从安装预览查看影响、确认安装、明确授权、使用原包、停用及重新启用的流程，
不恢复全生态覆盖目标，不新增 DSH 主对话模式或内置第三方业务。固定原包为 WorkBuddy 5.21.2，
边界拒绝回归为 GenUI 0.3.3，设置页面回归为 REA 1.2.0。安装／授权事实由 daemon PluginManager
拥有，Node 管实例资源，原生管理页与设置页只提交操作并消费同一注册表。

核对发现设置页顶部开关直接调用“授权全部权限再激活”，可以绕过管理详情的影响说明；
原生 HTTP 授权及启停也未使用公共管理服务已经支持的目标摘要校验，旧页面可操作后来安装的版本。
现在未授权设置页提供“查看权限并启用”，定位同一插件的管理详情；已授权开关只启用／激活，
不重新授权。所有这些原生请求携带当前显示的安装摘要，daemon 在准入和排空后的提交处检查。
安装预览、更新和已安装详情共用已有影响说明，不增加新的授权状态或提交机制。

验收门槛：同一真实原包在线安装、原设置 tab、模型答复；未授权页没有可执行文档；
管理页说明与请求摘要一致；停用清除模型来源、隐藏文档与旧地址准入；重新启用不重复授权、
创建新实例，重启保持配置和授权。GenUI 拒绝不影响原生详细／摘要模式及草稿，REA 原页面继续可用。

本批已完成。最终 macOS arm64 / Magi 3.0.62 / Electron 45 目录包的构建与解包自检见
`target/plugin-admission-layout-build.log`，发行物摘要见 `target/plugin-admission-build-identity.json`。
以下三项分别为同一最终目录包上的独立完整运行，不拼接失败或历史检查数：

| 实际范围 | 本批结果与证据 |
| --- | --- |
| WorkBuddy 5.21.2 / deepseek-v4.1-flash | **35/35**，`target/plugin-acceptance/admission-workbuddy-verified/`。在线预览说明、设置页授权跳转、绑定安装摘要、真实模型答复、即时统计与整体速度、停用回收模型及保活页面、旧页面地址拒绝、已有授权开关恢复新实例、撤权及显式重新授权、完整重启与配置／统计不重复。`lifecycle-requests.json` 保留原生操作实际发送的摘要；页面异常 0。 |
| REA 1.2.0 / rea-agents 6.5.0 | **22/22**，`target/plugin-acceptance/admission-rea/`。原 SVG、设置表单与 Ghidra 外链、207 项工具目录、设置 CAS 与 MCP 重载、390px、页面保活及未保存草稿／滚动、停用回收和重新启用通过；页面异常 0。本批未执行应用分析或 Ghidra，不能计为 REA 全部业务通过。 |
| GenUI 0.3.3，WorkBuddy 5.21.2 | **10/10**，`target/plugin-acceptance/admission-genui-final/`。安装前明确拒绝消息替换、无可确认候选、现有实例保持；拒绝前后真实模型业务完成，原生正文与样式、草稿、摘要模式及重启保持。GenUI 计为边界拒绝，不计为支持。 |

实际截图复核另定位一个设置页头缺陷：新增权限入口与长插件名在窄屏争抢一行，按钮几何检查通过，
标题却被裁到视口外。此前 `admission-workbuddy-final/` 的 **35/35** 仅保留为过程记录，不能证明视觉通过。
修复共用设置页头的标题换行和操作组换行，删除强制单行约束；没有按包名修改原页面。
验收增加标题边界及标题／按钮交叠检查。最终 1280px、390px 中文及加长英文按钮的三份回执
`authorization-*.json` 均完整可见、无重叠、`cut=0`、`hidden=0`，截图已人工核对；REA 390px 也已核对。
授权跳转使用可读插件名称过滤列表，不向搜索框暴露内部散列 ID。

相关验证：原生升级／授权 HTTP 回归 **1/1**，同一摘要授权后激活成功、旧摘要五类动作均拒绝且不改状态；
公共管理及排空／撤权回归 **11/11**。Web check **0 错误／0 警告**，plugin-store、i18n、panel-layout
及 protocol check 通过，日志为 `target/plugin-admission-*.log`。沿原 PluginOperations 裁决与提交，
删除设置开关的隐式授权调用及 Web 重复请求封装；没有新增执行内核、持久授权状态或主对话 UI。

本批有限范围已收尾，不以生态支持率为退出条件。未验证 Windows／Linux、全部插件业务或所有升级阶段；
既有无摘要 HTTP 调用保持协议原合同，当前原生界面统一发送摘要。修改留在 main 工作目录，
本地目录包已重建，未发布线上版本；其他任务的已有改动保留。

## 后续事项（不阻挡本轮退出）

### Our Free Model 原包安装与模型调用修复（2026-11-10）

本批以 `dsh-our-free-model 2.1.0`（npm 已 deprecated）及同作者 `@ebony-vinyl/dsh-our-free-model 2.8.3` 为固定原包，修复安装误拦并回到真实业务。初始两个预览都为 HTTP 400；历史拒绝回执保留于 `target/plugin-acceptance/our-free-model-2.1.0-api/`、`our-free-model-2.8.3-api/`，不再代表当前结果。

根因及正式修复：

- 原包只硬依赖 llm，多账号 channel-pack 位于 `ctx.inject(['credentials', 'commands', 'llm'], ...)` 的动态导入内。此前扫描无条件追进 pack，把其凭据和 Agent 事件需求算成整包必需。现在按上下文数据流核对直接条件导入：仅在宿主明确不发布依赖服务、且原包没有显式发布该服务证据时保留待就绪分支；静态／其他可达入口、未知依赖和逃逸闭包仍完整检查。报告明示未执行模块，不假装账号功能已实现。credentials 保持真实缺席。
- 真实 Cordis registry 读取、真实 LlmRuntime 读取及 emitAdaptersUpdated 补入准入目录。registry 仅拥有当前宿主实例内的 fiber，不提供 DSH 模块缓存加载器，不取代 daemon 安装、摘要、授权与升级。
- WASM 标准资源此前按原生 ABI 插件误拒；2.8.3 还携带上游平台运行时分片，31 MiB 发布包超过旧容量。原包资源保持原路径，受信 Node 扩展沿公共 full-trust 合同携带 WASM 和二进制辅助资源，通用包校验不包含 DSH 身份分支；归档、展开统一最多 64 MiB，单脚本仍 16 MiB、其他文件 4 MiB、4096 条目、路径及执行位检查继续有效。没有按包名、资源文件名或特定 CSS 放行。
- 推理档位 disabled、light／balanced／deep 导致运行启动失败。目录和请求共用转换，Magi 的 none／low／medium／high 在发送时还原为该模型实际声明 ID；未知或歧义仍明确拒绝。移除过去把所有 none 固定写成 off 的反向规则。
- 原生派发将 preparing 提交为 canonical Running，后续装配仍按旧状态字符串判断，致使真实请求尚未进入上游就失败。装配、启动恢复和重启调度统一消费 canonical 投影，同时继续要求根 Task 为 Pending，避免重放已执行请求；没有为插件新增另一条会话执行链。工作区另有一个已有借用编译错误，以 canonical transaction 的值传递 branch 修复，不改变业务语义。

实际结果：

| 原包与业务 | 本批证据与结论 |
| --- | --- |
| `@ebony-vinyl/dsh-our-free-model 2.8.3` / `deepseek-v4.1-flash-free` | 当前 macOS arm64、Magi 3.0.62 / Electron 45 目录包及随包 Node／daemon，独立状态根，一次完整 **13/13**：在线预览安装、原生授权、原设置 tab、真实路由与配置保存、模型 HTTP 200 并返回 `MAGI_OFM_READY`、原生助手正文展示、执行中原生取消、停用清除来源、重新启用新实例、完整重启及配置／正文／取消终态保留；Renderer 与原包异常 0。输入 27,379、输出 9、缓存读取 1,664 Token 为本次上游实际用量。证据 `target/plugin-acceptance/our-free-model-acceptance-2.8.3/`，驱动 `scripts/verify-electron-our-free-model.mjs`。|
| `dsh-our-free-model 2.1.0` / `mimo-v2.6-flash-free` | 同一目录包独立安装、原页面、配置、停用／重新启用及重启通过；真实模型返回上游 HTTP 429：`Free usage limit reached. Free models work in OpenCode ... subscribe to OpenCode Go ...`，原生界面保留失败，不计模型业务通过。证据 `target/plugin-acceptance/our-free-model-final-2.1.0/`，包括 `model-turn.json` 与 `before-restart.log`。|

验收使用 npm 完整性核对的隔离 **Bun 1.4.4**，通过原包已有 `OUR_FREE_MODEL_BUN` 入口提供给其匿名通道；该工具未安装到用户全局目录、未随 Magi 发行。记录位于 `environment.json`；缺少 Bun 的机器仍需满足原包外部运行条件，不能将本次结果表述为 Magi 已提供通用 Bun 在线管理。多账号登录／凭据中心、EAC、模型工作台、插件自更新和其他未执行功能均不计通过；不为其扩张主对话 UI 或 Magi 内核控制权。主设置仍为独立插件 tab，原包页面没有改写。

定向验证：兼容静态回归 51 项、运行回归 54 项、合同回归 30 项、包准入 28 项、dispatch 回归 21 项及 daemon 接纳重启回归 1 项通过；目录包构建与解包检查通过。旧 32 MiB 总量断言因统一容量合同变更更新，并以超过 64 MiB 的实际展开拒绝回归替代；未放宽单文件、路径、未授权二进制和未知档位断言。有限样本不代表整个生态支持率。

- 全生态覆盖率、Windows／Linux、全部插件功能及近期新包；所有原包逐项自身升级与全部取消阶段。
- Free Search 所有引擎、凭据中心、HTML／浏览器抓取、fake-IP 代理环境下原包 SSRF 的网络适配；保留真实拒绝，不修改原包防护来制造通过。
- WorkBuddy 外部 CLI 全功能、REA Ghidra／Java 与原生二进制反编译及其他原包自更新；GenUI 消息替换按当前边界拒绝，不继续追求原样接管主对话。
- 原页面独立模型的更多真实图片／工具业务抽验、普通页面 fetch 的逐块 HTTP 交付及工具参数逐字符进度；pi 不在本次 DSH 交付范围。
- 其他 worktree 的临时探针、与本轮无关的业务及数据整理。
- GenUI 一次未复现的测试点击等待超时及模型偶发名称／参数拒绝的长期可靠性观察；不把单次完整通过外推为所有交互或模型均无错误。

完成本轮表示上述五项冻结任务、C1—C5 和相应失败／恢复合同有当前证据，不代表整个生态或所有历史产品愿景完成。

## 输入区空插件入口占位（2026-11-11）

用户截图中的加号与工作区按钮之间留白来自通用插件入口容器：原包组件返回空内容时，
外层仍固定占用 96px，工具栏还为它保留间距。该问题属于 Magi 页面宿主布局，不是原包业务失败。

输入区现在移除重复的外层占位，公共 PluginFrame 将既有、已校验的 `view.resize` 高度交给入口组件。
零高度入口退出工具栏布局并禁用交互，同时保留可测量的隔离文档；异步内容出现后恢复原入口，
不销毁重建页面、不改写原组件。新实例或失败状态清除旧文档的空内容标记，停用仍释放文档。
原包内容与 daemon 授权事实没有增加第二份存储，也不按 WorkBuddy 名称处理。

macOS arm64、真实 Chrome 与独立 daemon、当前源码 Web 构建，安装用户当前 WorkBuddy **5.21.2**
原归档（摘要 `5d9974442b37464b75ac6e9e369be621b8b796365aea522fe9b7c02c66f706fa`）复现原页面空 root。
1400px 与390px 下，加号与工作区按钮间距均为正常 **4px**，无横向溢出；同一隔离文档加入／移除受控
几何内容时可恢复／收起，停用释放文档，Renderer 异常 0。几何内容仅验证尺寸生命周期，不冒充原包模型业务。
证据 `target/plugin-acceptance/composer-empty-gap/`。Web check 0 错误／0 警告、构建、plugin-composer
及 composer-input **61** 场景通过。操作栏裁切审计 `cut=0`；整输入框审计仍有外壳底部阴影裁切，
与恢复旧占位后的同一基线一致，未将它计为整输入框视觉通过，也未扩大此次修改去调整外壳。
未重新打包或重启用户正在使用的 Electron，当前客户端须更新构建后生效。

## 过期现场清理（2026-11-10）

用户明确授权清理过期 worktree 和历史验收材料后，核对 DSH worktree 的 HEAD
`fab8e63a` 已是 main 祖先、没有未合并提交或受跟踪文件改动，剩余未跟踪项均为旧截图探针或依赖链接。
已删除这些探针、旧依赖目录、构建缓存和产物；worktree 从约 **1.5 GB** 缩至 **40 MB**，Git 状态干净。
checkout 本体及分支仍保留：Codex 归档工具仅接受当前对话附件，接管该 worktree 返回
`This worktree is owned by another task`；它归属“评审插件系统设计”对话，须从原对话归档。
其他任务的 `project-multi-root` worktree 保持原状。

已删除 **401** 个被替代的验收目录及旧日志、快照、下载源码等材料；合计删除 **1,093** 个清理条目，
按删除前磁盘占用估算释放约 **2.56 GiB**。`target/plugin-acceptance` 从约 **1.3 GB** 缩至 **204 MB**。
保留 **29** 个有效证据／验收依赖目录：包括最新 WorkBuddy **35/35**、REA **22/22**、GenUI 边界拒绝
**10/10**、其他不同业务的最终验收、必要的失败边界证据和 REA 工具依赖；保留当前客户端构建和相关检查日志。
原始删除路径、占用、旧结果文件摘要与检查通过数、保留目录清单见 `target/plugin-acceptance/cleanup-20261110.json`。
清理不重解释旧批次结果，不合并多轮计数；有效结果文件和 REA 可执行依赖已复核可读。

此次只清理现场并同步记录，未修改产品代码、当前客户端或用户数据，未重新运行产品测试。
