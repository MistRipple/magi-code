# DSH 翻译器剪除审核

本文为分批审核的历史记录。2026-11-10 已按用户要求清理被后续验收替代的原始现场；下文旧目录的“保留”描述不再代表当前磁盘状态。当前证据及删除回执见[过期现场清理](plugin-convergence-delivery.md#过期现场清理2026-11-10)，当前产品边界以产品合同和最新交付记录为准。

2026-11-09。用户授权先做减法；全量功能目标保持暂停，本批不新增插件能力或内核接口。

## 范围与所有权

用户仍应能在线取得 DSH 原包、显示原页面、通过原生服务使用工具与模型。安装、版本、权限、会话和持久化事实继续归 daemon。先清退未交付的预研和重复展示代码，再收窄已确认过宽的默认声明。

| 候选 | 处置与依据 | 影响及验收 |
| --- | --- | --- |
| pi 静态翻译预研 | 删除 `pi/`、`PI_FLOW` 及其专属入口解析、规则和测试。产品从未提供 pi 安装／运行链路 | 开发诊断 CLI 不再检查 pi；产品目录搜索与不可安装事实保留。保留 DSH 数据流与同名变量回归 |
| 多生态工厂 | 删除 `index.mjs`、`core/registry.mjs`，安装和诊断直接使用同一个 DSH 检查入口；同步发行打包和运行组件身份输入 | 不增加替代工厂或第二条安装路径；原包准备、可复现归档及打包应用实测 |
| 重复权限说明 | 安装预览和已安装详情共用现有 `plugin-store.ts` 中的说明函数 | 继续使用原 i18n 与共享样式；Web check、商店与 i18n 回归 |
| DSH 管理插件默认代授权 | 从准备产物的管理权限中删除 `authorize`，保留安装、生命周期及其真实回执 | 无目标授权时明确返回已安装但未激活，用户沿 Magi 原生入口授权；验证真实 Mgr 原包预览与 daemon 既有拒绝回归 |

## 本批保留及原因

- 原包获取、依赖完整性、可复现封装、原页面及其资源入口、动态工具／模型目录与生命周期仍为实际业务所需。
- `@earendil-works/pi-ai` 是 DSH 模型运行库的真实依赖；删除 pi 静态翻译预研不删除该依赖。
- `tools:*`、`modelSources:*` 同时承担动态贡献准入。直接去掉会损坏运行目录，不能以削减声明冒充权限问题已解决。
- Magi 原生插件的正式 API 与委托授权机制保留；本批收窄的是 DSH 准备产物的默认声明。
- 既有安装摘要、权限和运行状态不直接改写。正式在线更新通过运行组件内容身份提示新产物，再按原生生命周期提交。

## 已确定规则与待收敛项

- 提示贡献按用户已确定的产品规则默认全局，激活时明示全局指引能力；实施与验收见第六批。
- 默认 `sessionWrite:["context","wake"]` 的按业务声明已在下方第二批收窄；逐操作的权限粒度仍未处理，不增加无 Task 的后台投递。
- 动态贡献与调用原生工具权限的边界；本批不新建自动权限推断系统。
- 上述项目未全部处理前，不宣称完成最小权限或完整产品边界整改。

## 验证记录

- 相对本批剪除前现场，代码与测试新增 104 行、删除 508 行，净减 **404 行**；不含文档，也不混入此前正文隔离等未提交工作。明细为 `target/plugin-acceptance/pruning-code-size.json`。删除 4 个模块，保留 DSH 数据流回归，没有按测试数量或行数认定产品完成。
- 翻译回归 **135/135**；Web check 为 0 错误 0 警告，商店与 i18n 回归、Desktop check、协议生成一致性和差异检查通过。既有 daemon 定向回归确认无代授权委托时返回已安装但未激活及真实授权错误，未改写该执行链路。
- macOS arm64 目录包构建、解包自检通过，`target/plugin-acceptance/pruning-build.log`；发行暂存与本批源码核对一致，不含 pi 预研或已删除工厂入口。
- 同一目录包真实 Electron 验收 **19/19**，`target/plugin-acceptance/pruning-genui/`：Mgr 0.2.13 在线原包预览保留安装与生命周期能力且无 `authorize`；GenUI 0.3.3 在线安装、WorkBuddy 5.21.2／hy3 真实回复、原卡片操作、草稿冲突与显式恢复、停用／重启后模式和去重均通过。无效尺寸请求整体拒绝，宿主高度保持 41px，原生正文不变；16 份菜单及悬停阴影审计均 `cut=0`、`hidden=0`，页面异常 0。
- 同一目录包 REA 1.2.0 原包在线流程 **16/16**，`target/plugin-acceptance/pruning-rea/`：真实 SVG、原 React 设置、Ghidra 链接、207 项 MCP 工具、CAS 配置保存及重载、390px、停用与恢复通过，页面异常 0。只覆盖列出的业务，不证明 Ghidra 安装或二进制分析完成。
- 本批未验证 Windows、完整社区业务或收窄授权后的 Mgr／Upgrader／Market 全部管理流程；此前依赖默认代授权的完整管理验收不能沿用为当前通过证据。新目标包需用户到 Magi 原生入口授权，既有包与旧授权未被直接改动。第一批当时未完成的会话写入声明和全局指引告知分别接续于第二批、第六批；全局作用范围已由用户确定。

## 第二批：收窄会话写入声明

本轮只处理安装准备时对所有 DSH 包统一声明 `sessionWrite:["context","wake"]` 的问题，不恢复全量功能目标。

- 真实用户任务：WorkBuddy 原包的 CodeBuddy 工具／命令完成后，把结果交回当前 Magi 会话；设置、管理或模型连接本身不应附带这项权限。旧证据见 `target/plugin-acceptance/session-messages-workbuddy/`，其命令回注实际使用 `Agent.steer`；工具路径还可使用 `Agent.inject` 与 `deferContext`。本轮需回到在线原包复验，旧结果不作为改动后的通过证据。
- 失败证据：`prepare.mjs` 无条件声明会话写入，连没有工具／命令业务的页面包也会在预览中要求该权限。
- 预期结果：只有既有扫描或启动记录显示使用后端 tools／commands 命名空间、或观察到真实工具／命令注册的包才声明该权限。保留对可选注入、配置后注册和服务句柄读取的准入，不用默认配置的空目录否认后续业务。页面提交继续使用独立 composer 授权。
- 唯一所有者：daemon 的原生消息入口继续校验当前根 Task、会话、Turn、租约、作业预算、插件激活及用户授权，并沿原 Input 队列持久化和去重。准备产物仅申请权限，不替用户授权；不新增业务入口或推断引擎。
- 范围限制：这是按后端业务命名空间收窄声明，仍不是逐操作的最小权限推断，也不是操作系统沙箱。直接系统访问、全局提示和工具调用边界沿既有未决项处理。既有包和授权不直接改写，通过正式在线更新生效。

### 第二批验证

- `npm run test:compat` **135/135**，覆盖无工具／命令业务不声明会话写入、可选与配置后注册、服务句柄读取及启动期真实注册；已有工具上下文交付失败仍保留实际业务结果，取消及失效投影回归通过。日志：`target/plugin-acceptance/session-permissions-tests.log`。
- macOS arm64 目录包构建与 Electron 解包自检通过；随包 `prepare.mjs` 与本轮源码逐字节一致，运行组件身份已记录。证据：`session-permissions-build.log`、`session-permissions-identity.json`。
- 同一目录包 WorkBuddy **5.21.2** 在线原包 **16/16**：真实设置和 hy3 回复、停用／重新激活、原生 Task 调度 CodeBuddy CLI 读取验收文件、结果回注及真实主模型接续完成。回注沿 `steer` 进入原 Input 队列，保留原插件、源 Turn、源 Task；文件保持原内容，页面异常 0。证据：`target/plugin-acceptance/session-permissions-workbuddy/`。
- 同一目录包 GenUI **0.3.3** 原包流程 **21/21**：Mgr **0.2.13** 在线预览和 GenUI 安装产物都没有 `sessionWrite`；GenUI 仍通过独立 composer 授权完成原卡片后续对话。草稿冲突、重复点击、停用／重新激活、完整重启和稳定去重通过，原生正文与样式保持原样，页面异常 0。证据：`target/plugin-acceptance/session-permissions-genui/`。
- 本轮生产代码新增 7 行、删除 1 行，净增 **6 行**，未增加内核接口、第二条消息路径或权限推断模块；含验证净增 **24 行**，与第一批合计仍净减 **380 行**。明细：`target/plugin-acceptance/session-permissions-code-size.json`。未复跑 REA、完整社区业务、Windows 或既有安装的同版在线更新，不将上述结果外推为生态覆盖率或完整最小权限整改。

## 第三批前置审核：提示贡献作用范围

原包实测确认了提示贡献的全局作用。用户随后明确选择“激活插件时明确授权全局指引”，并补充“插件就是全局的，默认全局”。因此跨会话作用是预期产品行为，需补齐激活前的全局能力告知。

- 原包与任务：GenUI **0.3.3**，用户在会话 A 的原包输入按钮切换指引模式；会话 B 没有打开插件页面或显式采用指引，仍用 WorkBuddy **5.21.2 / hy3** 完成真实模型调用。独立状态不修改用户原有安装和会话。
- 实测证据：会话 A 用原组件将模式从 `off` 切为 `plus` 后，会话 B 下一次实际模型请求的 `promptFrame.tailBytes` 从 **769** 增至 **104159**，增加 **103390 字节**。两次调用均 completed，Renderer 与原包文档异常 0。证据与复现脚本：`target/plugin-acceptance/prompt-scope-audit/`。用户已明确这属于预期全局行为；该证据只证明跨会话作用路径，不代表原包完整业务通过。
- 所有权与现状：原包的模式和 `systemPrompt.section` 由同一全局注册表维护；`ExtensionBusHooks::prompt_contribute` 传入当前会话身份，`ExtensionBus::invoke` 对所有已注册插件派发。`authorizeAndActivatePlugin` 一次授权清单全部权限并全局激活；第六批补齐安装预览与激活前的全局能力告知。
- 现有接纳合同：最终接纳将指引渲染成有来源、受预算的 user 角色参考资料；原生系统段、canonical 正文、权限及审批保持原有所有权。用户确定的是插件默认全局生效，沿现有全局运行链路实现。

用户已确定的实施路径：插件默认按应用全局生效；安装预览及授权激活前明确展示全局指引能力，复用既有插件摘要授权与全局激活入口。原包的模式设置继续由原包拥有。此前实测用于证明全局作用真实存在，后续验证告知与授权流程；全量产品合同与完成门槛继续保留。

## 第四批：管理原包更新后的授权恢复反馈

本批处理独立的管理流程反馈缺口；提示贡献的产品裁决与实施见第六批。

- 原包与任务：Mgr **0.2.13** 原页面在线更新 release-probe **0.0.7 → 0.0.8**；管理器没有代授权权限，用户应能辨认新版已安装、到 Magi 原生入口授权并继续使用，无需重复安装。
- 真实失败：新版已提交且 grants 为空、active 为 false，原页面却只显示“更新失败：插件声明的权限尚未全部授权”，没有已安装版本和恢复入口说明。证据：`target/plugin-acceptance/manager-authorization-before/attempt2/`。此前一次驱动缺少展开原包详情的操作而超时，单独保留于同目录父级，不计产品失败。
- 预期与所有者：daemon 的既有安装失败回执同时说明已完成的安装、未完成的激活及原生恢复操作；保留原错误码、阶段与真实安装事实。原包组件原样显示该回执，用户通过既有 Magi 授权／激活入口恢复；不新增权限、改变生命周期或让管理器代授。

### 第四批验证

- `cargo check -p magi-api` 通过；安装操作相关 Rust 回归 **7/7**，覆盖已提交但未授权、已获委托、后台实例准入、自更新权限与恢复。修改文件 rustfmt 一致，差异检查通过。证据：`target/plugin-acceptance/manager-authorization-rust-tests.log`。
- macOS arm64 目录包构建、Electron 解包自检通过，`target/plugin-acceptance/manager-authorization-build.log`。
- 同一目录包完整 Mgr 原包流程 **60/60**，`target/plugin-acceptance/manager-authorization-profile-verified/`：Mgr **0.2.12 → 0.2.13** 原生在线升级预览、取消和重新授权；两代原包对 release-probe **0.0.7 → 0.0.8** 的实际在线更新；新版本未代授、错误说明已安装版本与恢复入口；用户从 Magi 原生入口授权后同一摘要实际运行，候选解析次数不增加；原包启停、启动失败及恢复、卸载确认取消、完整重启和实际卸载通过。候选提交前断进程后恢复旧摘要与授权，不重新下载或重放安装。页面异常 0，5 份阴影审计均 `cut=0`、`hidden=0`，390px 展开后原因与恢复说明全文可读。
- 截图复核纠正了一处旧验收缺口：`innerText.includes('0.0.8')` 也会匹配 registry 的最新版本。原包的错误分支不自动重读列表，现沿其原有“刷新”操作后精确核对安装版本字段；原生授权后重新打开页面再次核对该字段。此前 **58/58** 的结果保留在 `manager-authorization-profile/`，不将其中弱版本断言算作通过证据，以整次 **60/60** 运行和对应截图为准。原页面行为未被改写。
- 本轮生产实现净增 **12 行**，仍在同一 daemon 安装回执函数；含验证净增 **56 行**，明细 `target/plugin-acceptance/manager-authorization-code-size.json`。未在本轮重跑 WorkBuddy、REA、Market／Upgrader 全流程或 Windows；提示贡献的产品裁决与实施见第六批，不据此宣称完整产品合同已完成。

## 第五批：安装确认操作的尺寸与长文案

- 用户任务与原包：用户搜索 WorkBuddy、取得官方原包预览并确认安装；安装及在线更新由现有 daemon 入口拥有，当前只修改安装页的展示与交互。
- 真实失败：打包 Electron 在线预览后，确认安装按钮实际 **28px × 72px**，低于安装主操作的共享 **40px** 尺寸，源码误用了 `btn--sm`。这违反 `web/AGENTS.md` 对安装、确认安装及更新使用大号主按钮的要求。证据：`target/plugin-acceptance/installer-actions-before/confirmation-size.json` 与同目录原包预览截图。
- 预期：确认与安装使用同一共享大号按钮；底部操作栏保留固定位置，长文案按可用宽度换行；忙碌期间沿原按钮展示加载并禁用重复提交，取消与失败恢复沿现有入口。只沿当前共享样式处理，不修改授权、安装协议、业务事实或插件页面。

### 第五批验证

- Web 检查 **0 错误、0 警告**；macOS arm64 目录包构建及解包自检通过，`target/plugin-acceptance/installer-actions-build.log`。
- 打包 Electron 安装交互 **30/30**，`target/plugin-acceptance/installer-actions-verified/`：确认按钮实测 **40px × 100px**，与共享主操作尺寸一致；390px 加长文案下主按钮按内容增高至 **55px**，确认及取消操作都在视口内。真实 WorkBuddy **5.21.2** 原包在线预览、安装及 **5.20.0 → 5.21.2** 在线更新完成；搜索相关度、加载动画、真实图标、取消、重复点击、注入网络失败与原位恢复、旧搜索请求失效均通过。8 份阴影审计全部 `cut=0`、`hidden=0`。这些记录验证安装交互，不替代 WorkBuddy 模型业务证据。
- 生产代码只替换 2 行、净增 **0 行**；验证净增 **25 行**，`target/plugin-acceptance/installer-actions-code-size.json`。长文案用例只临时替换验收 DOM 的按钮标签，产品 i18n 和原包内容不被改写。

## 第六批：激活前告知全局业务指引

- 用户决定：插件默认全局生效，激活时明确授权全局指引。授权继续绑定插件包摘要及其清单，沿现有“授权并激活”操作完成。
- 原包与事实：GenUI **0.3.3** 已经通过真实 WorkBuddy / hy3 双会话调用证明全局指引生效；现有安装预览和权限区仅显示通用完全信任说明，没有说明全局指引会影响所有会话。
- 实施：Web 直接读取 daemon 提供的清单中 `prompt.contribute` 声明，在安装／更新预览及已安装详情展示同一条全局指引告知；沿现有插件授权与全局激活流程提交。指引、设置和运行生命周期继续由原有服务拥有，页面宿主只展示已声明的能力。
- 验收：从真实原包安装预览开始核对告知，确认安装后仍未授权／激活，再通过 Magi 原生“授权并激活”执行，回到同一原包验证提示开关、卡片业务和停用／重启。

### 第六批验证

- Web 检查 **0 错误、0 警告**，i18n 检查通过（3270 keys、306 sources）。macOS arm64 目录包构建和解包自检通过，`target/plugin-acceptance/global-prompt-notice-build.log`。
- 同一目录包 GenUI **0.3.3** 原包流程 **24/24**，`target/plugin-acceptance/global-prompt-notice-genui/`：通过原生搜索、预览、确认安装及“授权并激活”真实操作验证全局告知；确认安装后 grants 为空且未激活，激活前保留相同告知。WorkBuddy / hy3 真实回复、原卡片继续对话、模式设置、草稿冲突、重复提交、停用／重新启用和完整应用重启均通过；原生正文与样式保持现有展示。页面异常 0，16 份阴影审计全部 `cut=0`、`hidden=0`。
- 生产代码净增 **8 行**（两处现有页面告知及中英文文案），验证净增 **24 行**，`target/plugin-acceptance/global-prompt-notice-code-size.json`。复用 daemon 清单和原生全局授权／激活事实，未增加会话筛选或第二套授权状态。Windows、完整社区业务覆盖和完整产品合同仍未在本轮证明。

## 第七批：已有授权范围内的在线自更新

- 原包与用户任务：Health **3.0.5 → 3.0.8**，用户从原页面检查更新并立即更新，保留设置、已批权限和启用意愿；更新后的页面与实际运行版本一致。
- 源码阻塞：DSH 新产物已不再申请替其他包授权的 `authorize` 委托，自更新却仍要求这项委托。daemon 已逐项确认候选权限不超出原授权，却在此之前拒绝请求。
- 实施与所有者：删除自更新对 `authorize` 的额外要求，继续由 daemon 的同一安装任务排空、提交、续用已批范围及等待新摘要就绪；安装／生命周期权限、旧实例租约、权限扩张和用户撤权检查保留。新增权限在提交前拒绝，说明当前版本保留，并指向“设置 → 插件”的在线更新及授权激活操作。其他包的代授权规则继续沿原入口执行。
- 独立 macOS arm64 目录包、Electron 45、随包 Node 原包业务 **29/29**，页面异常 **0**；`target/plugin-acceptance/self-update-authorization-health/`。两代 Health 均只获 `install`、`lifecycle`、`background` 管理权限。诊断、原页面导出、修复取消与确认、文件变化拒绝、停用来源、配置和页面偏好、真实在线自更新、新页面、完整应用重启、停用／重新启用及原持久回执不重放通过；最终回执为 `applied`，候选摘要与实际安装一致。检查脚本提前读取同一候选的持久终态，失败时保留原回执。
- 首次原包运行在进入自更新前遭 npm `/latest` 的 HTTP **429**，**19/20**，保留 `self-update-authorization-before/`；不是自更新权限拒绝的实测证据。上游恢复后独立完整重跑取得上述 **29/29**，没有替换原包页面、修改更新服务或拼接结果。
- `cargo check -p magi-api`、安装操作 Rust 回归 **7/7**、修改文件格式／脚本语法及差异检查通过；覆盖无代授权的自身更新、缺少安装／生命周期权限、权限扩张在排空前拒绝、撤权、归档失败及另一摘要不能冒充成功。标准目录包构建与解包自检通过；日志 `self-update-authorization-{check,rust,build}.log`，构建仍有既有 WebFolderPicker 导入方式警告。
- 生产执行路径只删除一项多余准入要求并补充恢复说明；未增加协议、UI 状态或第二条安装路径。未重新验证整个社区批次、Windows 或真实新增权限包的原生恢复流程，本轮结果不代表完整生态覆盖率或产品合同完成；本次未启用可选阴影审计。

## 第八批：Upgrader 在线更新后的原生授权恢复

- 原包与用户任务：Upgrader **1.8.0** 原页面将 release-probe **0.0.7 → 0.0.8**，用户沿 Magi 原生入口授权已提交的新版，继续版本切换、失败恢复及停用／重启。安装、授权、版本与任务事实继续由同一 daemon 拥有。
- 实际差距：新版已安装、未激活且 grants 为空，原页面完整显示版本、`authorization-required` 和无需重复安装的恢复说明；旧共用验收却等待管理器自动授权后的 running 而超时。原证据 `target/plugin-acceptance/upgrader-authorization-before/`，保留原页面全文及 overview。产品回执已符合当前授权规则，本轮补齐真实操作验收，不增加生产接口或权限。
- 共用验收改为检查原始失败回执和安装事实，再实际点击原生“授权并激活”；三次恢复都核对同一摘要运行，安装任务文件完全不变。保留原包任务失败结果，不能用用户随后授权冒充该原始任务成功。原包文档身份在更新期间、切换设置分类前核对；原生授权返回原页面后另核对原任务与回执保持不变。
- 当前 macOS arm64 目录包、Electron 45、随包 Node 的独立专项 **19/19**：原页面配置、8 种外观默认／真实悬停、并发拒绝、关闭／重开、取消、停用／重新启用、真实在线更新及原生授权恢复、版本切换、版本不存在时保留旧启停与授权、归档落盘失败恢复、显式重试和完整应用重启通过。原包业务 `completed=true`、`core=passed`、`upgrade=passed`，页面异常与 registry 异常均为 **0**；证据 `target/plugin-acceptance/upgrader-authorization-native-verified/`，日志同名 `.log`。语法与差异检查通过。
- 首次补齐流程的 `upgrader-authorization-native/` 在主动切换设置分类后检查旧文档标记，属于验收断言位置错误；已将原有身份断言移至切换前，并增加返回后原任务回执断言，未删弱该条件。以上 **19/19** 为后续独立完整运行，不拼接此前分项。
- 本次只选固定 **9** 个样本中的 Upgrader，输出 `packages=1`、`expectedPackages=9`、`coreVerified=1`、`qualified=false`，社区脚本因此仍退出 **1**；不是完整社区门槛通过。未模拟断网、未验证 Windows／移动真机／新增权限包，本轮不沿用此前 Share／Market 的代授权通过结论。

## 第九批：Share 原包导入、恢复与共用验收

- 原包与用户任务：Share **0.2.5** 用原页面导入 npm／GitHub 组合码，取消和显式重试，保存并识别原二维码，按 Magi 原生入口授权已安装包，再完成导入包的在线更新、失败保留及重启。版本、任务与权限仍归同一 daemon；Share 的逐项结果仅是原包明确标注的浏览器历史，不作为当前安装事实。
- 原始差距：原接口返回 **409** 和 `authorization-required`，候选已安装但未激活；旧社区验收直接要求 HTTP 200／running。证据 `target/plugin-acceptance/share-authorization-before/`。新共用合同核对原始失败回执，再实际原生授权同一摘要；授权前后安装任务文件完全相同，旧回执和历史不改写为成功。
- 实际宿主缺陷：首次完整流程在页面停用时产生 `Plugin UI storage failed: AbortError: 插件页面已关闭`，证据 `share-product-native/`。缓存写入的等待随文档销毁正常取消，原回调却一律报持久化错误。页面宿主现读取同一 transport 的销毁事实；存活页面的真实存储失败仍沿原错误反馈，销毁后的等待结束不再误报。未改写原包、扩大授权、增加存储事实或协议。
- 清理：两处 Share 业务验证合为 `scripts/lib/dsh-share-acceptance.mjs`，专项和固定社区调用同一完整流程；删除原来的重复片段。Share、Upgrader、Mgr 的原生授权操作合为 `plugin-native-acceptance.mjs`。专项中独立的管理 API 摘要、拒绝授权和移除回归保留；本轮未运行整个 Share／Market／Upgrader 混合专项。
- 当前 macOS arm64 目录包、Electron **45**、随包 Node，独立完整流程 **24/24**，`target/plugin-acceptance/share-product-receipts/`：原页面队列取消／重试、后台文档保持、npm 导入及原生授权、原包启停导入、上游明确要求手动更新、SVG／PNG 保存及保存取消后重试、原包扫码还原同一 GitHub 组合码、GitHub 固定提交安装、来源保护、**Health 3.0.5 → 3.0.6** 原生在线更新预览／取消／提交／授权、固定标签再次在线检查、无效引用保留、完整应用重启、安装中停用、重新启用及卸载均通过。Share 自身版本保持 **0.2.5**，不将被导入插件的升级称作 Share 自更新。
- 页面及 registry 异常 **0**，`completed=true`、`core=passed`、`upgrade=passed`；390px 确认固定可达，预览阴影 `cut=0`、`hidden=0`。结果历史实际非空并跨原生授权、完整重启和停用恢复保留；Share 未提供自定义配置字段，原生 settings 相等只作状态一致性检查，不冒充配置编辑业务。构建／解包自检、翻译回归 **135/135**、修改脚本语法和差异检查通过，日志 `share-storage-{build,compat}.log`；构建仍有既有 WebFolderPicker 导入方式警告。
- 验收驱动的中间失败分别保留于 `share-product-native/`（再次预览未重开来源页）、`share-product-verified/`（重启后未等待工作台入口）、`share-product-complete/` 和 `share-product-final/`（混用公开导出与内部条目的字段）。已按原包真实公开字段校验精确来源、版本及启用状态。交付审计发现 `share-product-release/` 的宽泛“项完成”也会匹配“0 项完成”，因而可能在原包生成成功历史前离开；已改为核对精确 GitHub 成功回执及宿主浏览器存储中的同一记录。最终 `share-product-receipts/` 独立整次 **24/24**，保留取消、待授权失败及 GitHub 成功三条真实历史，后续原生授权、重启和停用恢复均保持；不拼接分项、不删减业务结果或页面异常门槛。
- 本次仍只执行固定 **9** 包中的 Share，`packages=1`、`expectedPackages=9`、`coreVerified=1`、`qualified=false`，社区命令因此退出 **1**。未将分批专项合为新的整批覆盖率，未验证 Windows、移动真机、全部安装掉电阶段或完整 Market 流程；整个产品合同与 95% 目标继续未完成。


## 第十批：Market 原包安装、更新与原生授权恢复

- 用户规则继续明确为应用全局：插件统一安装、配置、授权和启停，贡献覆盖全部工作区及会话；业务指引随激活一次授权，不增加会话／工作区的二次开关。产品合同第 6.0 节单列此规则；既有全局运行链路不变。
- 原包与失败：Market **1.2.2** 安装 release-probe **0.0.7** 后返回 HTTP **500**，日志已说明“已安装、未完成启用、无需重复安装”；旧验收误要求管理器代授权后立即 running。原证据 `target/plugin-acceptance/market-authorization-before/` 保留。daemon 继续拥有安装、授权及运行事实，本轮不扩张管理插件的权限、不修改原包页面或业务代码。
- 收敛：删除专项与社区中的重复 Market 片段，改为共用 `scripts/lib/dsh-market-acceptance.mjs`，复用现有原生授权驱动。逐次核对未授权的新摘要、原始失败回执、原页面完整日志以及授权前后完全相同的安装任务文件；用户授权后的运行不改记为原任务成功。
- 独立完整流程 **17/17**，`target/plugin-acceptance/market-native-verified/`：npm **0.0.7 → 0.0.8**、GitHub／HTTPS Git Health **3.0.5 → 3.0.6**、不存在版本与 Git 引用失败保留、原目录版本未知语义、原页面搜索并安装 Mgr **0.2.13**、原生授权恢复、原失败日志保留、显式刷新、页面实际重载、桥与未变化宿主保持、目标停用意愿、Market 全局停用／重新启用、完整应用重启和卸载通过。真实截图核对恢复日志与重启后的已安装状态；页面及 registry 异常均 **0**。
- 原包列表沿自己的快照刷新合同：普通查询可先返回旧快照并后台刷新，原安装失败分支保留旧列表。初次共用验收的即时快照断言和随后普通搜索等待分别失败，保留 `market-native-recovery/`、`market-native-complete/`；最终验收通过原有“强制重新获取”显式刷新后核对真实安装，不新增重试安装、自动刷新或页面改写。
- 本批仅改产品合同与验收代码，沿用已构建目录包，未重新构建或运行无关 Rust／Web 全套检查。修改脚本语法和差异检查通过。社区固定分母仍为 **9**，本次只执行 Market，`coreVerified=1`、`qualified=false`，命令退出 **1**；未宣称完整社区、Market 自身版本升级、安装取消、Windows 或 95% 门槛通过。
- 共用入口连续回归独立完成 **66/66**，`target/plugin-acceptance/management-native-verified/`：同一运行覆盖 Share、Market、Upgrader 原包和保留的原生管理服务回归，退出 **0**，Renderer／原包异常 **0**。Market 共用流程仍为 **17/17**，包含真实 npm／Git 安装、更新及原生授权恢复，未将其他原包检查合并成 Market 检查数。首次连续回归 `management-native-shared/` 暴露验收入口传入安装时快照，比较尚不存在的运行代次而报错；已改为从 daemon 读取当前运行基线，再完整复跑连续流程，不沿用分段结果。


## 第十一批：Appshot 真实截图业务与社区共用验收

- 用户任务与事实：Appshot **0.4.2** 用户以原生快捷键截图，将真实图片投递到 Magi 当前输入框，并在停用／重启后恢复配置和持久图片。历史记录注明挂载曾超时；当前目录包首先独立执行旧脚本 **14/14**，`target/plugin-acceptance/appshot-current-business/`，未复现该问题，不能认定已修复。实际缺口是社区入口仅检查页面与配置路由，尚未调用既有真实截图合同。
- 所有权不变：原包后台拥有截图业务和原生辅助进程；Magi 原 InputArea 接纳图片，daemon 的资源及图片存储拥有持久配置、附件元数据和载荷。验收只读取与核对这些事实，不增加生产接口或图片事实源。
- 收敛：将原专项截图流程提取为 `scripts/lib/dsh-appshot-acceptance.mjs`，专项与社区调用同一流程，移除专项中的原业务正文。两入口均使用各自同一 daemon 和独立状态根；实际读取当前运行状态，不从页面有字判断业务成功。显式关闭真实截图验收时 `liveCapture` 保留 `unverified`，不计核心通过；在线升级始终单列未验证。
- 共享专项 **13/13**，`target/plugin-acceptance/appshot-shared-native/`；随后最终社区入口独立完整运行 **13/13**，`target/plugin-acceptance/appshot-community-native/`，`completed=true`。原生截图 **1440 × 904、86,200 字节**进入当前输入框，实际图片与 daemon 元数据、SHA-256 和文件原字节相同，真实截图及设置恢复截图已查看。原配置保存、文档常驻与复用、相同摘要重复确认不重启原生进程、停用／重新启用、完整应用重启后的配置及图片元数据／载荷均通过，页面异常 **0**。旧脚本的退出回收与重启恢复合并为重启后旧 PID 不存在且仅有一个新 PID 的同一检查，保留原回收要求。
- 社区固定分母仍为 **9**，本次仅执行 Appshot，`coreVerified=1`、`upgradeVerified=0`、`qualified=false`，命令退出 **1**；不把不同批次拼为完整社区支持率。原包自身在线升级、取消／系统授权引导、其他平台和历史长期超时仍未验证。本轮仅改验收和文档，未重新构建或复跑无关 Rust／Web 全套；修改脚本语法及差异检查通过，继续保留完整产品目标。


## 第十二批：Appshot 在线候选失败诊断与原实例保持

- 本轮原用户任务为 Appshot 原包自身在线版本升级，配置、原生辅助进程和截图业务须在更新后继续可用。npm 实际 `latest` 为 **0.4.2**，选定 **0.4.1 → 0.4.2** 作为验证路径；旧版在线预览即被拒，原始证据 `target/plugin-acceptance/appshot-online-before/`，未进入可运行旧版基线，不能记作自身升级通过。
- 原包核对：0.4.1 使用 `addImages`、`sessions.binding`、`createDraftImages` 等图像／会话接口；0.4.2 官方变更记录明确包含“mount screenshots on Harness 0.1.7 composer APIs”。0.4.1 原 npm 归档 **36,461,653 字节**，已核对官方 SHA-512 integrity；来源及 SHA-256 记录 `target/plugin-acceptance/appshot-041-source.json`。这些证据说明本次是旧版所需接口尚未接入，不能称为现有安全分析误拒，也不能据此认定整个原生在线升级机制失败。
- 实际修复：公共业务分析器对可静态识别的未支持输入成员明确报告真实名称，并提示尝试在线更新插件；动态访问和对象逃逸仍按原规则拒绝。原先泛化的“逃逸或动态／未支持”无法让用户辨认具体缺口。本轮生产实现新增 **1 行**诊断，不按包名添加分支，未改变原包、准入结论或增加另一套输入实现。
- 验收仍以当前可运行 **0.4.2** 为基线：先保存配置并确认唯一原生进程，实际请求 0.4.1 在线候选，核对 `INPUT_INVALID` 内具体 `addImages` 名称及在线更新建议，并确认当前摘要、授权、PID、文档和配置不变；随后以真实快捷键截图投递并验证持久字节、停用及完整重启。新目录包社区入口独立 **14/14**，`target/plugin-acceptance/appshot-diagnostic-native/`，`completed=true`、页面异常 **0**；未把候选拒绝改记为成功，也未把失败后仍可截图冒充自身升级成功。
- 既有业务转换回归 **27/27**、修改脚本语法及差异检查通过；标准 macOS arm64 目录包与解包自检通过，证据 `appshot-input-diagnostic-{conformance,build}.log`。构建仍有原有 WebFolderPicker 静态／动态导入混用提示。本轮未重跑无关 Rust／Web 全套。
- 社区仍只执行固定 **9** 包中的 Appshot，`coreVerified=1`、`upgradeVerified=0`、`qualified=false`，命令退出 **1**。Appshot 自身版本升级、旧接口的正式产品裁决、系统授权引导、Windows 和完整社区门槛继续未完成；完整目标保持，不用本次诊断改进替代缺失能力。

## 第十三批：Ops 原页面启停与授权恢复

- 用户规则仍为应用全局安装、配置、授权及启停，贡献覆盖所有工作区和会话，不增加二次范围开关。本轮真实任务为 Ops **0.20.8** 原页面在线安装 Release Probe **0.0.8**，经 Magi 原生授权恢复后，回原页面显式重试、停用和重新启用目标；daemon 继续拥有安装、授权、生命周期和持久化事实。
- 初始失败 `ops-authorization-before/` 来自旧验收要求管理器默认代授权。补齐原生恢复后，`ops-native-recovery/` 与 `ops-lifecycle-diagnostic/` 复现产品缺陷：原页面禁用成功，随后点击启用返回成功，但 daemon 目标仍停用。诊断批次保留完整原页面和插件快照。原包停用写入不带说明的 `disabled:true` 行，启用删除该行；公共 profile 转换却只把删除带 `disabledBy` 的项认作恢复，遗漏了无说明的正常停用。
- 修复公共提交规则：删除当前调用快照中的停用项即提出恢复启用，停用说明继续可选；保留 daemon 的同一 `setEnabled` 入口、摘要及预期状态／停用来源核对。生产代码只替换一个条件并纠正注释，净增 **0 行**，没有包名分支、原页面改写、新接口或第二条状态写入路径。新增回归覆盖无说明的停用／删除恢复、其他停用项保持及来源变化拒绝。
- 最终 macOS arm64 标准目录包、Electron **45**、随包 Node，独立完整流程 **17/17**，`target/plugin-acceptance/ops-lifecycle-final/`，退出 **0**、页面异常 **0**。覆盖原按钮安装、原始待授权回执、原生授权同一摘要且安装任务不重放、原包显式重试保持原实例、原按钮全局启停并恢复为新实例且保留摘要和授权、精确版本豁免、真实扫描、原页面重载、应用重启后的豁免持久化、撤销及卸载；没有物理 profile 或第二份豁免文件。
- 翻译回归 **136/136**（含 profile **27/27**），`ops-lifecycle-compat.log`；既有 Rust 停用来源持久化／过期恢复回归 **1/1**，`ops-lifecycle-source-rust.log`；标准构建与解包自检 `ops-lifecycle-build-final.log` 通过，随包 `profile.mjs` 与最终源码逐字节相同，脚本语法及差异检查通过。构建仍有既有 WebFolderPicker 静态／动态导入提示。
- 补强验收的 `ops-lifecycle-verified/` 错把局部运行代次要求为递增；源码既有合同允许同版重新启用后代次重复，唯一实例 ID 必须改变。已依据该合同核对唯一实例 ID、原摘要和授权，并在上述最终批次完整重跑，未拼接中间结果。此前 `ops-lifecycle-native/` 的 **17/17** 只作初次复验，不取代最终证据。
- 未验证 Ops 诊断对话、知识库、隔离 canary、取消、自身在线更新、其他平台和完整社区；不将该专项加入整个 Ops 完整业务通过数或宣称 95% 门槛完成。下一步继续核对其诊断对话的真实模型调用与结果，不能以本轮管理操作代替该业务。

## 第十四批：Ops 诊断对话实测与原生模型归属设计

- 上一轮属于有效进展：修复无说明停用项的恢复并完成同一原包 **17/17** 管理专项。本轮继续 Ops **0.20.8** 原页面诊断，预期使用 Magi 的真实默认模型，在原页面返回诊断结果，且不修改主对话内容或另配 DSH 密钥。
- 当前目录包独立实测 `target/plugin-acceptance/ops-chat-configured-failure/`：在线安装 WorkBuddy **5.21.2**、选择 **hy3**，同一 daemon 真实模型回复 `MAGI_OPS_MODEL_READY`，canonical 状态 `completed` 且有模型步骤。随后以 Ops 原输入框发送诊断问题，实际 HTTP **200**，业务回执为 `ok:false`、`reply:""`，错误 `no model channel available ... set DSH_OPS_LLM_API_KEY to override`；脚本退出 **1**。原生用户会话的 canonical Turn 在该操作前后保持原样。原始回执、全文、真实模型 Turn、截图和运行日志全部保留；这是产品失败，不能按 HTTP 200 或设置页面可见认作核心业务通过。
- 已核对的调用链：原包 `resolveChannel` 读取 `llm` 与 `agentDefaultModel.currentSelection()`；后者在 Magi DSH 宿主中缺席。原生 `model.invoke` 又要求活动 Turn／Task，普通页面路由签发的 `owner` 为空，不能借主对话绕过。现有 `invoke_auxiliary_model_with_usage`／`auxiliary_usage` 已有无 Turn 的传输事实持久化，但其当前持久用量归属仍要求用户 session，不能直接填插件 ID 或无账本调用。上述是待补齐的公共产品能力，不归为 Ops 固有不支持，也不以新增错误文案冒充修复。
- 产品合同新增“原页面中的独立模型业务（待实现）”：原生模型选择与授权、插件操作归属、消息角色及原业务、取消和失效、未知结果不重放、真实模型与原页面闭环。实现先复用既有辅助模型／用量入口并补正式插件归属，不新建模型执行器或虚构会话，不改主对话 UI。WorkBuddy 等已选来源的消费必须设计为公共能力，保留既有子代理角色模型规则；具体 DTO 和协议仍待沿唯一类型及消费者实施。本轮没有改生产服务或扩大授权。
- 新增 `MAGI_DSH_OPS_CHAT=1` 独立验收分支，记录原页面实际响应并核对非空答复和原组件展示；不设置该开关仍执行原管理专项。中间 `ops-chat-before/` 是输入元素类型选错；`ops-chat-native-model-before/` 与 `ops-chat-native-model-current/` 选到了同类容器中的升级预演输入，未发出诊断请求；`ops-chat-native-model-confirmed/` 被既有验收端口占用检查拒绝。已按原“发送”按钮定位其同一输入，并通过真实输入事件发送，再取得上述配置有效时的真实失败；这些驱动失败不计为额外产品故障，不拼接通过数。
- 原页面另报告 WorkBuddy 的 `schemastery` 重复实例为 fatal，而同一目录包的隔离宿主已完成真实回复。该扫描解释是否适用于 Magi 的独立宿主仍待核对；不删除原包警告、不伪造 peer 元数据，也不把本轮结构化扫描回执称作诊断准确率通过。
- 脚本语法与差异检查通过。本轮只改验收和产品设计，未重新构建或复跑无关整套检查。诊断对话及知识沉淀尚未修复，完整目标继续 active；下一轮沿同一主流程补原生插件模型业务与用量归属，再回到该真实失败验收，不能继续用管理专项替代。

## 第十五批：Ops 原页面真实模型业务与插件用量归属

- 用户规则：插件的安装、配置、授权和启停默认全局，不增加工作区／会话范围开关。本轮只接续第十四批的 Ops **0.20.8** 诊断对话失败；目标是在原页面使用用户已选的 WorkBuddy **5.21.2 / hy3**，返回真实答复并通过原按钮沉淀知识，不修改主对话 UI 或借用主对话 Turn。
- 唯一所有者与实现：daemon 从同一设置源解析默认模型，以插件、摘要、实例和稳定操作身份接纳；原 `model.invoke` 增加正式页面业务，协商 `model.operations`。DSH 宿主在真实 `LlmRuntime` 中登记原生消费适配器，提供 `agentDefaultModel` 的无凭据投影，保留 system、角色、顺序、推理续写数据、输出上限、结束原因和实际用量；消费适配器不再次发布为模型来源。旧工具／命令和子代理的模型规则未被放宽。
- 收敛：原辅助模型与页面业务共用 `invoke_observed_model_with_usage` 的 Bridge 传输记录、落盘和结算入口；扩展原 `auxiliary_usage` 事实的插件归属，插件 ID 不填入 sessionId。同一审计／用量账本保存操作指纹及已确认结果；在途重复、改参和未知结果拒绝自动重放，已确认结果在来源不可用时仍可读取。取消覆盖父路由、原包 signal、停用及宿主失效；接纳前先到的取消以有界记录保留，避免 spawn_blocking 顺序导致漏取消。没有第二个模型执行器、隐藏用户会话或 DSH Session／Agent。
- 真实失败保留：`ops-native-model-business/` 在主模型真实可用时取得页面 HTTP **502**，底层实际为模型桥拒绝非流式请求的 **400**；使用现有 Bridge 流式入口后，`ops-native-model-stream-business/` 与诊断批次确认底层已 **200** 且有真实用量，但原生服务仍因推理续写数据拒绝交付。随后保留该数据并按原生 `ModelResponse` 的 camelCase 字段传输，不删原包警告、不改问题或降低原页面非空答复门槛。
- macOS arm64 标准目录包、Electron **45**、随包 Node，独立完整原包业务 **10/10**，`target/plugin-acceptance/ops-native-model-verified/`：主对话真实模型对照、原页面诊断答复、原按钮知识沉淀、两次成功的插件用量归属、页面通道取消同一第三次模型请求、完整重启后知识持久化及模型操作不重放、主对话 canonical 保持原样、页面异常 **0**。页面通道取消是原 fetch 的真实 AbortSignal 验证，不虚构原包没有提供的取消按钮。原始输入、回执、账本与截图保留；该批不代表整个 Ops 核心业务完成。此前 `ops-native-model-product/` 的独立 10/10 保留，不与最新批次数量拼接；原诊断答复截图及重启后的真实知识条目截图已查看，后者另按条目身份核对整个原组件在视口内可见，记录 `knowledge-visual-receipt.json`，没有改写组件或正文。
- 同一目录包原 Ops 管理完整回归 **17/17**，`target/plugin-acceptance/ops-native-model-management/`：没有配置默认模型时仍能打开原页面，在线安装待授权回执、原生授权恢复、显式重试、全局启停、精确版本豁免、扫描、重启恢复及卸载全部保持；页面异常 **0**，没有第二份物理 profile。
- 翻译回归 **137/137**；原辅助用量恢复 **4/4**、原生插件页面模型与持久归属／同请求读取／撤去来源后读取／接纳前取消回归 **1/1**、独立模型授权回归 **1/1**、真实 Node 宿主回归 **27/27** 通过。标准构建与解包自检为 `ops-native-model-product-build.log`，脚本语法及差异检查通过；保留既有 WebFolderPicker 静态／动态导入提示。新增两个生产模块合计 **400 行**，分别装配原生页面模型业务和 DSH 适配，传输与用量记录器复用既有实现；不以行数或测试数证明产品完成。
- 待完成：插件独立用量事实已持久化，但原执行统计页仍未汇总该类插件事件；页面流目前在原生调用完成后转换为 DSH 块，逐块远程透传、工具／多模态／采样控制、并发压力及模型来源变化实机矩阵尚未验收。Ops 的 schemastery 扫描结论在独立宿主下是否准确、canary、自身在线更新、Windows 和完整社区仍未完成，社区完整业务通过数不增加，不宣称 95% 或完整产品合同达成。后续继续从这些具体原包业务缺口推进。


## 第十六批：原包模型调用的统一用量统计

- 用户任务：插件默认按应用全局生效。本轮接续 Ops **0.20.8** 原页面诊断与知识沉淀；第十五批虽已落盘实际插件调用，执行统计只读取 `model.usage.recorded`，原插件专用投影未被汇总，用户无法从统计页查看其消耗。
- 唯一事实与收敛：传输事实继续由 daemon 同一持久审计／用量账本拥有；会话和插件共用 `UsageCallRecordInput`、`model.usage.recorded`、传输投影和聚合器，删除插件专用结算分支。互斥所有者保留真实会话或插件 ID、摘要、实例、操作及名称快照；插件不制造用户会话。内存投影按真实所有者分桶，公共聚合不携带虚构会话身份；共享模型的插件、原生辅助业务和不同插件分别保留来源，模型总量仍正常合并。重置与重启使用原账本合同。
- 统计展示沿原设置页：统一既有展示分组函数，清退重复的角色分类条件；插件名称与实际调用进入模型、角色两种视角。单会话筛选排除不属于该会话的全局插件调用，筛选只影响统计视图，不改变插件全局启用规则。未增加设置范围开关、插件专用统计页面或数据存储，也未改主对话 UI。
- macOS arm64、标准目录包 **3.0.62 / Electron 45**、随包 Node，Ops **0.20.8** 与在线 WorkBuddy **5.21.2 / hy3** 独立完整实测 **15/15**，`target/plugin-acceptance/ops-usage-unified/`，退出 **0**、页面异常 **0**。原页面返回真实诊断并通过原按钮沉淀知识，插件两次成功共 **5710 Token**；取消第三次真实请求后仅结算一次，共 **3** 次调用、**2** 次成功、**1** 次取消，成功用量保持不变。全局统计包含原生对照的 **1** 次和插件的 **3** 次调用，单会话无插件记录。完整重启后插件统计逐字段相同且模型操作未重放；全局重置后再次重启仍为 **0**。主对话 canonical 保持原样，原包知识持久化恢复。
- 证据：`stats-{completed,cancelled,restored}.json`、`stats-reset-restored.json`、`plugin-model-usage.json`、原诊断与知识回执、两次重启日志及 `results.json`。已查看实际统计截图 `ops-plugin-stats.png`，中文深色、2880 × 1920 图片中插件名称、3 次调用与实际用量均可见；其他语言、窄屏和其他平台本轮未复验。
- 定向回归：用量聚合 **37/37**，统计接口 **11/11**，辅助／插件用量恢复 **5/5**，canonical 用量恢复 **5/5**，插件页面持久归属与不重放 **1/1**；Web 检查 **0 错误 / 0 警告**，既有 settings-store 与会话统计回归通过，daemon 检查、脚本语法及差异检查通过。构建与解包自检 `ops-usage-build.log` 通过，保留既有 WebFolderPicker 静态／动态导入提示。新恢复夹具最初使用了不符合正式规则的插件 ID，已改用合法身份后完整复跑；未放宽产品身份校验。
- 待完成：逐块远程透传、工具／多模态／采样控制、并发与来源变化实机矩阵、Ops 扫描解释准确性、canary、自身在线更新及完整社区合同。该单流程不增加完整社区业务通过数，不宣称达到生态 95%；本轮未复跑无关管理专项或全仓测试。下一步仍应从具体原包业务阻塞推进。


## 第十七批：升级器自身在线更新和原包完整业务

- 上一轮统一统计属于已完成的具体进展。本轮先核对 Ops 原包的实际诊断：原有 **0.20.8** 安装归档内解析到 `dsh-plugin-ops-core@0.20.9`，npm 最新核心 **0.20.9** 也仍使用 `host !== null && internal !== null` 即报告双实例的规则。以此前真实 daemon 安装归档生成只读调用快照、运行原核心解析和判断函数，`target/plugin-acceptance/ops-scan-source/peer-identity.json` 显示两端规范路径完全相同，均为 WorkBuddy 包内 **schemastery**；原函数仍产生 fatal。该结果支持这是上游扫描将同一副本重复计算的误报，不能据此认定 Magi 的 WorkBuddy 真实业务失败。诊断只证明这一规则，不证明全部扫描正确；原页面和模型诊断仍展示其原始结论，未删改原包、隐藏告警、伪造依赖元数据或宣称问题已修复。后续仍需正式处理运行拓扑及诊断语义，不能添加包名补丁。
- 实际主流程选择在线更新：既有 Upgrader **1.8.0** 只验证它管理其他插件，尚未验证自身旧版到新版的持续使用；社区入口还要额外设置功能开关才执行真实业务。先在当前目录包重跑原完整合同 **19/19**，`upgrader-unified-functional/`，确认既有业务并无新的生产阻塞。未设置旧开关的 `upgrader-unified/` 只有安装／页面／smoke，核心仍未验证，未计通过。
- 本轮仅增强既有共用验收：社区和管理专项先安装 Upgrader **1.7.5**，旧版原页面保存实际来源配置；复用 `plugin-native-acceptance.mjs` 的原生操作驱动，通过既有搜索、在线更新预览、取消、重新预览、确认与授权入口更新到 **1.8.0**。daemon 继续唯一拥有安装、授权、生命周期及配置事实。核对取消后旧摘要／授权／实例、新版提交的待授权事实、新摘要与新实例、实际旧版配置版本和值保持一致，再执行新版原页面完整业务；没有新增生产能力、第二份配置或上游页面修改。社区去除 `MAGI_DSH_FUNCTIONAL_UPGRADE` 条件，选中 Upgrader 就完整执行，原固定 **9** 包分母保持不变。
- 最终 macOS arm64 标准目录包 **3.0.62 / Electron 45**、随包 Node，独立完整 **23/23**，`target/plugin-acceptance/upgrader-online-self-update-final/`，`completed=true`、Renderer／原包异常 **0**、registry 异常 **0**。自身 **1.7.5 → 1.8.0** 更新后保留旧版非空配置；新版对 release-probe 的 **0.0.7 → 0.0.8** 原按钮更新、原始待授权回执、原生恢复不重放安装、并发拒绝、关页／重开、原按钮取消与旧版保留、停用回收、重新启用、版本切换、不存在版本保留、真实归档写入故障恢复及显式重试、完整应用重启全部通过。8 种外观的默认与真实悬停文字对比度检查继续通过；已查看重启原页面截图，实际显示 Upgrader **v1.8.0** 和 target **0.0.7 → 0.0.8** 的在线候选。
- 证据为 `upgrader-business.json` 中的旧／新摘要、`selfUpdateSettings` 的前后真实配置、目标版本和实例，`upgrader-button-contrast.json`、原页面截图、应用重启日志、`results.json` 及 `coverage.json`。此前 `upgrader-online-self-update/` **22/22** 是补充跨自身更新配置校验前的独立批次，不与最终批次拼接。社区本次仅执行 **1/9**，`coreVerified=1`、`upgradeVerified=1`、`qualified=false`，命令退出 **1** 为固定整批门槛未满足，非本次升级流程失败。
- 脚本语法、相对引用及 `git diff --check` 通过；本轮只改验收及文档，使用上一轮最终标准目录包，未重复构建或运行无关 Rust／Web 检查。管理专项调用同一共用合同，但其 Share／Market 前置流程本轮未重跑。Ops 误报、canary、自身更新、其余社区插件业务、独立运行组件发布源、Windows 等完整产品合同仍未完成，目标保持 active；不以本轮局部实测宣称生态 95%。

## 第十八批：全局工具目录与 REA 实际分析

- 用户规则：插件默认按应用全局安装、配置、授权及启停，贡献覆盖全部工作区和会话；不新增会话／工作区的启用、授权或范围设置。原生目录发现只是模型请求的装配状态，由同一 daemon 的当前目录、权限和实例继续裁决。
- 真实任务与失败：在线安装 REA **1.2.0**、使用真实 **rea-agents 6.5.0**，在新的个人会话通过 WorkBuddy **5.21.2 / hy3** 分析本地 Electron JavaScript 样本。此前 **207** 项 REA 工具被无条件展开，首轮共 **245** 项工具、固定提示 **886,443 字节**，上游明确返回 `prompt is too long`，分析尚未执行。原始失败为 `target/plugin-acceptance/rea-analysis-model/analysis-turn.json`，不能算作业务通过。
- 收敛：插件工具复用现有 MCP 工具目录与逐步刷新入口，删除全量加载布尔状态；`tool_catalog` 默认给元数据，以 `query` 检索、`tool_names` 精确取得完整 schema，只装配已取得且仍可用的定义。查询按词匹配，精确身份选择优先；针对某项工具的查询不再混入无关内置工具而被模型单条结果预算截断。目录会在同一任务中变化，不再复用只读查询的旧结果，避免启动期间的空目录长期遮挡后续注册。不新建执行器、授权存储或插件作用范围。
- 定向回归：目录 **24/24**、实时工具面 **4/4**、提示 **15/15**、只读复用分类 **1/1**；daemon 检查及 macOS arm64 标准目录包构建通过。证据为 `rea-discovery-{catalog,runtime,prompt,idempotent}-tests.log`、`rea-product-daemon-check.log` 和 `rea-product-build.log`。公共入口继续校验只读限制、Task／Skill 策略、执行器、撤销和当前目录；没有根据包名修改准入。
- 同一最终目录包公共运行回归独立完整 **169/169**，`target/plugin-acceptance/rea-product-runtime/`，退出 **0**。包括实际目录发现与工具调用、原生审批、结构化成功／失败／结束、取消、消息和配置持久化、原包 Debugging 诊断、停用与完整应用重启、标准 npm／pnpm 在线管理及同摘要原生授权恢复，主 Renderer 与原包 iframe 异常 **0**。未启用可选外部 DSH CLI、原始额外资源和阴影审计，不把该机制批次计入社区业务覆盖率。
- 验收脚本同时清除旧假设：模拟模型从真实目录元数据取得工具身份后再请求 schema，按业务工具的真实 `modelStepId` 核对完成后是否再调模型，不把目录发现步骤当作业务执行；npm／pnpm 的默认声明不再期待代授权，保留已提交待授权的原回执，由现有 Magi 原生 UI 恢复，不重放安装。此前目录截断、读取封装和旧步数／代授权断言的失败独立保留在 `rea-discovery-runtime*`，不拼接通过数量。
- REA 已在真实模型、Task 和原生审批后执行一次分析及保留证据的追踪，原始结果含真实 IPC、桥 API、成员和源文件 SHA-256。`rea-product-analysis/` 的独立尝试首轮 **38** 项工具、固定提示 **87,235 字节**，后续只装配所取的两项 REA 定义；上游模型请求均返回真实结果。该尝试只允许一次特征追踪，模型诚实报告结果裁剪后仍未读到 API／成员字面值，完整业务断言失败，未计通过。后续移除验收驱动对同份证据只读追踪的额外限制，接续同一原包与相同任务；原始业务结果、审批和模型结论继续分别核对。
- 最终同一目录包 REA 独立完整 **20/20**，`target/plugin-acceptance/rea-product-evidence/`，退出 **0**，页面异常 **0**。在线原包、原设置、SVG、Ghidra 链接、CAS 保存与重载、207 项工具注册、390px、停用／重激活保持；之后在新会话直接使用已全局激活的插件。真实 WorkBuddy / hy3 在同一原生 Task 中经过目录发现、**1** 次分析、**4** 次同份保留证据的特征追踪，**6** 个模型步骤全部实际 HTTP **200**；最终明确给出 `rea-audit:read-version`、`sampleApi` 和 `readVersion`，并与原始结果及两个源文件 SHA-256 核对。首轮 **38** 项工具、固定提示 **87,464 字节**，后续仅所取两项 REA 定义进入工具面，共 **40** 项；没有重放完整分析、运行样本或读取源码替代 REA 结果。源文件原字节、包摘要及既有实例均保持。已查看 `analysis-conversation.png` 的实际结论，`analysis-task.json` 与 canonical Turn 均 completed；验收依据包括业务字面值和原始回执，不只看终态。
- Windows、Ghidra／Java 安装和原生二进制反编译、REA 自身在线升级、完整社区业务以及 95% 产品门槛仍未证明；不以安装、工具目录、原始数据存在或 Task completed 替代用户所需的最终分析结论。

## 第十九批：跨环境市场原包的安装准入与业务验证

- 用户任务与原包：在线安装 **dshmarket 1.73.16**，保留原页面，使用其市场业务管理 Magi 中的插件。插件安装、授权和运行继续默认全局，事实及实际变更由 daemon 的既有插件服务拥有。
- 失败证据：`target/plugin-acceptance/product-admission-audit/dshmarket.json` 将 `ctx.desktopPnpm` 和 `desktopPnpm` 两项已识别为条件注入的接口列为 unknown，直接阻止安装。已读取原包入口及其引用的第三方 Desktop [固定版本服务合同](https://github.com/anywhere-labs/deepseek-harness-desktop/blob/4f68147091e585aaa1d815f99d30a657b3842d7c/dsh-plugin-desktop/docs/plugin-services.md#L190-L243)：这些服务专属该桌面宿主，缺席时原包走普通 DSH 分支，使用已有 `profileContext.packageManager`。Magi 不应伪造第三方桌面身份或为不执行的分支新增第二条包管理链。
- 预期与收敛：登记上述有明确合同的宿主专属服务为 dropped，仍完整展示缺口；仅沿现有条件注入规则允许服务缺席，顶层必需依赖和直接调用继续拒绝，未知接口仍阻止安装。没有按原包名称分支，也没有新增服务、修改原包或放宽默认检查范围。
- 当前仅完成源码准入及必需／可选分支定向回归；同一原包静态复查无阻塞。在线安装、原页面实际业务及失败恢复仍待本批打包 Electron 验证，不能计为核心业务通过或生态支持率。
- 首次新目录包在线预览的真实失败保留于 `target/plugin-acceptance/dshmarket-online/`：原包静态导入 `@deepseek-ai/schemastery`，依赖解析器却把该可选 peer 当成宿主未提供，启动记录返回 `ERR_MODULE_NOT_FOUND`。随包已携带 **3.19.0**，满足原包 `^3.18.1`；安装与执行共用的宿主依赖入口补齐该 Schema 运行库身份。版本满足时共享真实实例，不改写原包或吞掉导入异常；不满足时沿原依赖图安装并解析精确隔离副本，见下方 WorkBuddy 回归发现与收敛。
- 第二次目录包在线预览 `dshmarket-schema-online/` 已越过 Schema 导入，受限记录在原包查询 `existsSync(process.argv0)` 时拒绝读取正在运行的 Node 可执行文件。安装期记录器补充精确的当前可执行文件只读授权，不放开其目录、用户文件、网络或子进程；独立子进程回归核对上述拒绝仍有效。未更改运行期权限或模拟检查成功。
- 第三次目录包原包预览 `dshmarket-node-online/` 越过前述失败，随后明确拒绝启动时实际注册的 `agent/created`、`agent/disposed` 和 `agent/status`。原包的 `DrainCoordinator` 无条件订阅这些事件，以 `agents.list()` 判断活动任务、通过活 Agent 的 `whenIdle()`／`runMaintenance()` 取得维护期间的排他持有；Magi 当前没有这套公开合同，也没有 DSH Agent。不能把监听注册伪装成功、永久不发事件或将空列表冒充 daemon 空闲。该原包仍未完成安装及业务，本轮不增加通过数。后续须先设计由 daemon 原生任务／安装租约提供的可观察维护合同，再决定上游结构投影；不为安装率复制 DSH Agent 或开放内核替换。
- WorkBuddy 的新目录包回归 `dshmarket-schema-workbuddy/` 发现其依赖精确要求 Schema **3.18.4**，不能强制替换为宿主 **3.19.0**。沿既有 Cordis 隔离解析支持 Schema 库的完整依赖范围：满足范围才共享，不满足时依原包范围取得真实归档；主入口、子路径及 CJS 查找均优先同一原包隔离库，不改变宿主实例。回归通过独立 registry 和原包目录实际模块导入核对两套版本互不覆盖；没有按 WorkBuddy 包名添加分支。
- 最终翻译回归 **140/140**、macOS arm64 标准目录包及解包自检通过，证据 `dshmarket-peer-final-compat.log`、`dshmarket-peer-build.log`；五个生产文件与随包源码逐字节一致，组件身份为 `dshmarket-peer-identity.json`。最终同一目录包重新在线取得 **dshmarket 1.73.16**，`dshmarket-peer-online/` 确认仍明确拒绝缺失的三个 Agent 事件，未生成可安装产物；每次原包失败独立保留，不合并为成功。
- 同一最终目录包 WorkBuddy **5.21.3 / hy3** 独立真机 **12/12**，`dshmarket-peer-workbuddy/`，退出 **0**：真实在线搜索、原包预览、确认安装、Cordis 激活、原设置卡片、真实模型回复、停用回收及重新激活通过，页面异常 **0**。已查看原设置和对话截图，实际版本与安装回执一致；本批未开启 CodeBuddy CLI、完整应用重启或 WorkBuddy 自身更新，不能沿用旧版证据补齐这些流程。未增加内核接口或生产模块；dshmarket 的完整业务、社区整批和 95% 产品门槛仍未完成。

## 第二十批：原包明确接纳的不可用事件订阅

- 接续同一 **dshmarket 1.73.16** 安装失败。核对 native 排空、Task 准入及原包 `DrainCoordinator` 后，修正上批“安装必需维护锁”的判断：原包以 `host.on?.(...) ?? null` 返回订阅不可用，协调器收到 null 时返回 `supported:false / no-agent-events`；`agentsLookup` 缺席也明确返回 `no-agents-service`。它为没有 Agent 维护能力的宿主保留普通管理路径，不能据三个监听注册便推断整个插件必须控制主对话调度。
- 预期与唯一所有者：安装准备和运行宿主都沿原包显式的 null 分支，展示能力不可用，不返回空 disposer 冒充订阅成功；daemon 原插件管理、实例排空与授权继续拥有实际安装事实，不新增 DSH Agent、全局 Task 维护锁或核心调度入口。该分支不证明维护业务已支持，须在原页面核对明确不可用及普通管理真实结果。
- 修正扫描遗漏：不可变局部上下文别名参与跨文件传播；局部参数、块级变量及普通同名对象遮蔽外层绑定，避免既漏掉必需调用又误判普通业务对象。明确的可选调用加 `?? null` 才登记 nullable 订阅；普通可选调用、不同回退、未知事件或同名必需使用仍拒绝。`internal/plugin` 已在实际随包 Cordis 源码核对，由真实 Cordis 提供。
- 安装记录和运行期共用同一 nullable 订阅入口，返回 null 且不留下监听；已有 catch 分支继续收到真实注册错误，删除旧 `optionalEvents` 的静默成功路径。尚待新目录包与同一原包实测，不能将源码或定向回归视为原包业务完成。
- 第一次目录包 `dshmarket-nullable-online/` 已完成同一原包在线预览及实际安装，激活明确失败于 `scope.watch is not a function`。原包设置模块调用完整的命名空间句柄，安装记录器此前只提供 `get()` 的简化对象，漏报了这个真实业务接口。已核对发布的 **@deepseek-ai/dsh-settings 0.1.0-rc.7** 源码和 `SettingsScope` 类型（留存 `settings-contract-rc7/`）：观察在提交后按每个回调的顺序异步执行；解绑后跳过排队回调；停止等待已开始观察结束；值由 schema 默认、组合 base 和用户层合成。
- 同一 daemon 资源／CAS 入口补齐句柄 `watch`、`replace` 与 base 继承，用户层不保存派生默认值；更新只在实际提交后通知，失败写入不发布。记录器改为复用同一设置实现，持久层只用其现有临时观察状态，不新增产品存储或第二条配置写入。观察器异常沿已有诊断日志处理，不回滚已提交事实；生命周期释放句柄、订阅和排队通知。定向回归已核对顺序、失败、base 恢复及停止清理，待同一原包重新激活与原页面实测。
- 最终目录包的完整翻译回归 **144/144**（`dshmarket-settings-compat.log`），标准 macOS arm64 构建及解包自检通过（`dshmarket-settings-build.log`）；10 个生产文件与随包文件逐字节一致，身份为 `dshmarket-settings-identity.json`。事件入口最初捕获到绑定 root 的 Cordis 方法，导致 Volatile 作用域回归失败；已改为保留当前 traceable receiver 调用真实 `EventsService`，相关事件及 Volatile 回归和最终整批均通过，失败日志单独保留在 `dshmarket-nullable-compat.log`。
- 同一最终目录包重新在线取得并安装 **dshmarket 1.73.16**，真实 daemon 激活成功；原包 `/dsh-market/status` 返回实际安装版本、`settingsNamespace:"registered"`、`agentDrain:false` 及 `agentGuardAvailable:false`，没有伪造 DSH Agent 或控制主对话。`dshmarket-settings-online/` 选择的是原包设置摘要卡片（默认折叠），驱动要求长正文而超时；这是选错视图的验收缺口，不据此声称市场页面失败或通过。
- 随后独立完整 `dshmarket-page-online/` 选择实际 `settings.section / market` 页面，保留原组件的真实失败：`ctx.theme` 缺席，`getTheme()` 抛异常，原包主动展示“插件市场无法显示”恢复页。Renderer 收集到实际异常及原包恢复日志，命令退出 **1**；原包搜索、从页面安装、恢复、取消、在线更新和重启业务仍未完成，不计为完整业务通过。下一步须按真实 **@deepseek-ai/dsh-client-ui-theme 0.2.2-rc.1** 合同（`client-theme-contract/`）补齐页面主题服务及原生所有权设计，再回到同一原包；不能返回假主题对象、修改原组件或让插件初始化改写 Magi 主对话外观。
- 收敛扫描器中 catch／解构及循环绑定的词法作用域后，最终标准包重新完成翻译回归 **144/144**、构建及解包自检，日志 `dshmarket-event-final-{compat,build}.log`；源码与包身份记录 `dshmarket-event-final-identity.json`。同一最终包独立 WorkBuddy **5.21.3 / hy3** 真机 **12/12**，`dshmarket-event-final-workbuddy/`，退出 **0**，原包在线安装、原设置、真实回复、停用与重新激活、页面异常 **0**；本批未启用 CLI 或重启业务。最终独立 dshmarket `dshmarket-event-final-online/` 再次实际安装、激活并返回明确不可用的维护状态，市场原页面仍重现相同 `getTheme()` 错误，退出 **1**。不拼接各次运行的通过数，固定社区覆盖率及全量目标继续未完成。

## 第二十一批：原页面外观服务与同一市场安装流程

- 用户规则仍为插件默认应用全局生效，不增加工作区／会话的范围开关。本轮用户任务是 **dshmarket 1.73.16** 原市场页面读取现有外观、搜索并安装原始 WorkBuddy 卡片；不改写原组件、样式或目录数据。
- 原始失败为上一批原页面的 `ctx.theme.getTheme()` 异常。上游 **@deepseek-ai/dsh-client-ui-theme 0.2.2-rc.1** 的读取和 `theme/change` 合同要求稳定、不可变的快照；仅写入 CSS 变量不能满足原组件的订阅。页面服务投影原生外观的 `preference`、`active` 和 `themes`，复用原有语义色映射，原包订阅由同一文档 effect 回收。DSH `revision` 只计文档快照变化，写入仍使用原生目录 revision。预览不改写持久偏好，系统模式由 Magi 原生外观解析，自定义主题身份不与 `light/dark/system` 冲突。
- 主题选择沿现有 `/api/appearance/activate` 的 revision 校验和原生应用流程；daemon 保有目录、偏好及持久化唯一事实，页面不保存第二份偏好，不在原包初始化时自动选择主题。异步提交失败可见，实际提交成功清理先前错误，回执不冒充新的主题快照。本批只接入读取、通知及选择既有主题；DSH 主题／图标注册、覆盖层、字体读写尚未接入，不称为完整 ThemeRuntime 支持。
- 同一原页面越过主题错误后，实际目录请求把 `document.baseURI` 中的 Magi `ui-v/...` 目录带给后台，显示“后端路由未被宿主绑定”；证据 `target/plugin-acceptance/dshmarket-theme-online/page.png`。资源与业务共用的地址恢复入口补齐精确文档目录恢复，仍保留查询、原包路径及跨 origin 拒绝，不改原包 URL 构造方式或后端路由权限。
- 原市场卡片点击和确认安装后，实际出现“DSH 管理命令参数不受支持”，证据 `dshmarket-original-install/`。原包正常追加 `--reporter=ndjson`；重复的 DSH 参数解析无条件拒绝附加参数。DSH 管理命令与 npm/pnpm 现在共用已有 profile 单目标解析和同一 daemon 安装入口，允许原有公共参数在目标前后出现；多目标、未知参数和冲突 reporter 仍明确拒绝。NDJSON 输出为真实 `magi:plugin-management` 回执，不伪造 pnpm 阶段或完成进度；安装取消、授权及恢复沿既有服务执行。

### 第二十一批验证

- 最终翻译回归 **146/146**，`target/plugin-acceptance/dshmarket-theme-compat-verified.log`；Web check **0 错误、0 警告**，`dshmarket-theme-web-check.log`。profile 定向回归 **27/27**，包含共享参数解析、真实回执、拒绝、取消及恢复；没有新增 Magi 内核接口、权限种类或持久化事实源。
- macOS arm64 目录包构建与解包自检通过，`dshmarket-theme-build-verified.log`；6 个改动的生产运行模块与随包副本逐字节一致，`dshmarket-theme-source-identity.json`。源码与发行暂存目录中的 esbuild 产物因构建路径可不同，不用产物字节相等替代输入及真实运行核对。
- 同一最终目录包独立 **dshmarket 1.73.16** 原包 **11/11**，`dshmarket-theme-verified/`：在线安装和激活、初始化不改写偏好、原市场深浅主题选择及真实通知、实际目录加载、原始 WorkBuddy 搜索过滤、完整应用重启后的持久偏好及原包“使用中”状态均通过，页面异常 **0**。两种主题原页面阴影审计均 `cut=0`、`hidden=0`；截图已查看。不把搜索后出现其他 WorkBuddy 相关条目视为 Magi 原生搜索排序的验收，原市场筛选仍由原包拥有。
- 随后原市场真实卡片点击和“确认安装” **3/3**，`dshmarket-original-install-verified/`，实际提交 **dsh-connect-workbuddy 5.21.3**，未授予管理器代授权。另一次完整应用启动后，原生授权与返回原市场 **4/4**，`dshmarket-activation-final/`：待授权安装持久存在，用户沿既有原生入口授权，同一摘要实际运行，原市场读取同一 daemon 的 **5.21.3**。首次恢复驱动在工作台初始化前点击入口，失败保留于 `dshmarket-activation-verified/`；等待真实原生入口后完整复跑，不计为产品修复。
- 同一最终目录包独立 WorkBuddy **5.21.3 / hy3** **12/12**，`dshmarket-theme-workbuddy/`，真实原设置、模型回复、停用回收与重新激活通过，页面异常 **0**；没有启用 CLI 子流程。市场自身在线升级、取消／排空、原市场从其他源码安装、全部主题 API 及 Windows 尚未在本轮验证，固定社区覆盖率和完整产品合同继续未完成。

## 第二十二批：原市场取消自身在线更新

- 本轮主流程是原市场 **dshmarket 1.73.15** 发起自身在线更新，等待实际解析后从原页面取消，再显式重试并使用新版。用户默认全局规则不变。首次观察到原包直接完成 **1.73.15 → 1.73.16**；验收驱动误将版本号加入只按包名匹配的 registry 拦截条件，未形成取消现场，不能算取消通过。另一次驱动未打开原包任务面板，单独保留于 `dshmarket-self-update-cancel/`，不算产品失败。
- 真实失败证据 `target/plugin-acceptance/dshmarket-self-update-menu/`：已等待同一 daemon 安装解析连接，再实际打开原包任务面板并点击“取消”，连接仍不结束，页面保持“正在处理”。原包 `killTree` 在 `child.pid` 缺席时直接返回；Magi 的管理命令返回虚拟 EventEmitter 子进程且 PID 为 undefined。最终退出应用才取消真实安装，不是页面取消成功。该次官方 latest 已成为 **1.73.17**，真实安装请求身份保留在 daemon 的安装日志，不把移动的 latest 当成固定 1.73.16。
- 预期及唯一所有者：Magi 既有受管 Node 辅助进程提供真实 OS 句柄和进程生命周期，沿原有私有委托把当前管理操作交给同一个 daemon 安装任务；退出回收委托并只取消该进程拥有的任务。调用期 profile 提案与更新恢复身份继续由原调用持有，不能因进程化而新建第二份安装事务。删除虚拟 ChildProcess 链路，不伪造 PID，不替换原包进程控制代码，也不全局改写 process.kill。
- 实施：管理命令复用 `createProfileProcesses` 和既有私有包管理脚本，真实进程提供 PID、stdio 及 OS 退出状态；`child.kill()` 成功投递终止信号时立即撤销所属委托，防止已就绪的目录读取在 OS exit 事件到达前迟到发起安装，外部终止则沿真实退出回收。命令业务用 AsyncResource 保留父调用的 profile 与恢复身份，仍调用同一个 PluginManager 和 daemon 安装入口。实际 profile 不物化为子进程目录，管理辅助进程使用既有私有数据根作为工作目录。删除原 EventEmitter／PassThrough 模拟和单独取消链路；NDJSON 仍只交付真实完成回执。

### 第二十二批验证及剩余问题

- 翻译回归 **146/146**，`target/plugin-acceptance/dshmarket-managed-process-compat.log`；profile 定向 **27/27**，`dshmarket-managed-process-profile.log`，随后完整翻译回归增加了真实管理进程参与先移除后安装失败的恢复身份断言。回归同时验证真实 PID／SIGTERM、取消只结束所属安装、迟到目录读取不提交、新调用可显式重试、无物理安装副本及外部 DSH 实例不被接管。
- macOS arm64 目录包构建与解包自检通过，`dshmarket-managed-process-build.log`；3 个生产运行模块与随包副本逐字节一致，`dshmarket-managed-process-identity.json`。本轮未改 Rust／Web 产品源码，不额外运行其全套检查；未在 Windows 验证物理终止语义。
- 同一最终目录包 `dshmarket-managed-process-business/` 完成 **12 项业务断言**：在线安装 **1.73.15**，原页面查到新版，真实解析期间从任务面板取消；daemon 安装日志明确 `application:cancelled`，旧摘要、授权及实例保持。原页面显式重试实际请求并提交 **1.73.17**，沿既有自身更新合同保留相同授权并更换运行实例，原页面重挂载读到新版；完整重启、停用释放文档及重新激活新实例通过，安装日志不重放，所有页面异常 **0**。原始请求、真实结果及版本分别记录在 `updates.json`、`cancelled-job.json`、`updated-job.json`、`updated.json`。此前 `dshmarket-managed-process-live/` 观察到原包重试选择 **1.73.16**，驱动错误等待独立查询的 **1.73.17**；新专项改为核对原包实际委托与提交版本，保留该差异而不改原包选择逻辑。
- **整体专项仍退出 1**：原任务面板右侧原生 box-shadow 在隔离文档边缘被裁 **48px**，`cancel-menu-clip.json` 为 `cut=1`、`hidden=0`，截图已核对。这是实际 UX/UI 缺口，不能用上述业务结果宣称界面完全通过。验收先取得业务终态再保留原视觉门槛；不移除阴影、不按原包类名修改样式或跳过失败。下一阻塞是共享设置视图的物理余量，须沿统一布局设计处理，不能为此扩大主对话展示权。
- 同一最终目录包 Mgr **0.2.12 → 0.2.13** 独立回归 **55/55**，`dshmarket-managed-process-mgr-verified/`，退出 **0**：Release Probe 更新、原生授权恢复、先移除后候选解析失败恢复、daemon 中断恢复、显式重试、取消卸载及最终真实移除通过，原包异常 **0**，仍无第二份物理 profile。此前两次驱动在视口切换后仅调用 element.focus，未取得隔离文档物理焦点；失败保留于 `dshmarket-managed-process-mgr/` 与 `-mgr-sequential/`。补充真实点击 toast 内边距后再执行原有键盘门槛并完整复跑；未放宽焦点断言。本次未设置阴影审计输入，不能沿用旧批次的完整阴影结果。
- 新专项入口为 `npm run test:electron-dshmarket`，合同和环境参数见 `docs/validation.md`。本轮真实包和版本分别记录，不拼接成社区整体支持率；原市场完整管理能力、所有主题 API 及完整产品目标仍未完成。

## 第二十三批：设置页隔离文档的物理显示余量

- 用户任务仍是 **dshmarket 1.73.15** 原任务面板显示更新进度并取消；上一批 `dshmarket-managed-process-business/cancel-menu-clip.json` 证明原生 52px 模糊阴影在隔离文档右边缘裁切 48px。预期原任务面板默认、悬停、展开及关闭后的状态均可用，宽屏与 390px 无横向溢出，取消、重试、更新及重启不回退。
- 所有者与设计：共享设置框架继续拥有内容列宽与留白；PluginFrame 将框架现有水平留白纳入隔离文档的物理视口，再由客户端宿主的 `#root` 容器承接同样留白，原包内容位置和宽屏内容列保持一致。窄屏原包绝对定位浮层的原生阴影不足时，宿主容器只补充所需的水平余量；打开期间余量只增不减，关闭后恢复，不复制、移动或改写原组件及其样式。布局几何是临时展示状态，安装、授权和任务事实仍归 daemon；不增加第二条渲染路径，不让页面覆盖 Magi 原生区域。

### 第二十三批验证

- 首次目录包 `dshmarket-viewport-verified/` 完成取消、更新与生命周期，但整体 **12/14**、退出 **1**：原包内部及主窗口都没有横向溢出，外层设置滚动容器却多出宽屏 **40px**／390px **16px**。扩展文档宽度时只抵消了起始侧留白，结束侧仍进入滚动范围；现对称抵消两侧外边距，保留设置框架原留白。原失败、几何与截图全部保留，没有删除外层滚动检查或隐藏溢出来通过。
- 最终 macOS arm64 目录包构建与 Electron 解包自检通过，`dshmarket-viewport-build-final.log`；随包客户端宿主与源码逐字节一致。Web 检查 **0 错误、0 警告**，`dshmarket-viewport-web-check-final.log`；此前同批运行模块的翻译回归 **146/146** 为 `dshmarket-viewport-compat.log`，随后仅补齐 Web 外边距及实际验收脚本，没有重复执行无关 Rust 检查。
- 同一最终目录包独立完整原市场 **22/22**，`target/plugin-acceptance/dshmarket-viewport-final/`，退出 **0**：在线安装 **1.73.15**，1280px 与 390px 的默认、任务展开、真实悬停及收起共 **7** 份审计均 `cut=0`、`hidden=0`，截图已查看；原包文档、设置滚动容器和主窗口全部无横向溢出。缩放不重建原文档，取消操作可达，关闭恢复原内容留白；390px 原按钮经真实鼠标取消 daemon 安装，保留旧摘要／授权／实例。随后原页面显式重试实际完成 **1.73.15 → 1.73.17**、新文档、完整应用重启、停用／重新激活及安装日志不重放，页面异常 **0**。没有修改原包页面、组件或样式，没有新增内核能力、权限种类或持久化状态。
- 同一最终目录包 Mgr **0.2.12 → 0.2.13** 独立完整回归 **60/60**，`target/plugin-acceptance/dshmarket-viewport-mgr-final/`，退出 **0**。原包管理、真实在线更新与取消、待授权恢复、先移除后失败及 daemon 中断恢复、显式重试、完整重启与实际卸载通过，页面异常 **0**。深浅色长错误、1280px／390px 展开错误及待授权说明共 **5** 份阴影审计全部 `cut=0`、`hidden=0`，展开截图已核对。插件默认全局规则保持现有实现；本批不恢复暂停的全量目标，不宣称原市场全部管理能力、全部主题接口、Windows 或整体社区合同已通过。

## 第二十四批：Mgr 专项与社区共用真实业务验收

- 本轮按已恢复的持续目标推进，插件默认全局规则不变。用户任务仍是在线安装 Mgr **0.2.12**、沿原生入口在线更新至 **0.2.13**，再通过原页面管理其他原包，包含取消、失败恢复、授权、持久化和应用重启。安装、授权、生命周期与恢复事实均由同一 daemon 拥有。
- 失败证据：`target/plugin-acceptance/mgr-community-before/`。社区入口的旧路由验收仍假定 Mgr 可以代授新版权限，在原包真实返回 HTTP **500**、明确说明 Release Probe **0.0.8** 已安装但未完成启用时断言失败。Mgr 本身仍 running；该结果是旧验收与产品授权流程不一致，不能冒充新的安装失败或跳过授权。保留原始回执、固定分母 **9**、`coreVerified=0`、`upgradeVerified=0`。
- 收敛：专项和社区共用 `scripts/lib/dsh-manager-acceptance.mjs` 的完整原包合同，删除社区里独立的路由流程与 `runtimeFailureDisplay:unverified` 占位。Mgr 自身升级复用现有 `upgradePluginFromNative`，保留预览版本校验，增强取消时原授权及实例保持、新摘要提交后待授权及原生恢复的断言。候选解析失败、等待、请求计数统一使用已有 registry gate，删除专项自建的第二套代理。只调整验收组织，不新增产品能力、权限、运行状态或业务持久化。
- 当前目录包与随包 Node 的最终专项 **60/60**、退出 **0**，`target/plugin-acceptance/mgr-shared-specialist-final/`；最终社区 Mgr 分项同样完整 **60/60**，`target/plugin-acceptance/mgr-community-final/manager-business.json` 的 `completed=true`。两次均实际完成自身 **0.2.12 → 0.2.13**、Release Probe **0.0.7 → 0.0.8**、原生授权恢复且不重新下载安装、先移除后解析失败及 daemon 崩溃恢复、原包页面操作、8 种外观、390px、配置恢复、完整重启与真实卸载；每次各 **5** 份阴影审计均 `cut=0`、`hidden=0`，页面异常 **0**。抽取后首次社区运行也完整通过，保留于 `mgr-community-shared/`；最终结果只以上述最终目录为准。
- 社区本轮只执行 **1** 包，`expectedPackages=9`、`coreVerified=1`、`upgradeVerified=1`、`qualified=false`，命令按完整门槛退出 **1**。不拼接历史批次，不外推整体支持率；未重跑完整社区、Windows、Web 或 Rust 检查。本轮改动文件语法与差异检查通过，使用第二十三批目录包，产品源码及发行物没有变化。
- 社区 `registry-errors.json` 另保留 **3** 次官方 `/dsh-plugin-release-probe/latest` HTTP **406**，不是测试注入的解析 404，也未从日志中滤除；同次完整业务仍实际完成预览、提交、恢复及最终卸载。随后直接读取该官方入口，以 JSON 和 npm 精简元数据两种 Accept 都返回 **200**，不足以确定此前 406 的原因，不宣称 registry 全路径零错误或已修复外部服务。
