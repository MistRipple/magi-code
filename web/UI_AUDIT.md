# Magi 三组 UI 要求检查与修复记录

检查日期：2026-10-24。检查对象是 Magi 的实际产品界面与 UI 运行链路：Web、共用 Svelte 工作台、Electron App Renderer、右栏内容槽、主题服务及独立预览窗口。前端清点覆盖 120 个 Svelte 文件和 4 个共享 CSS 文件；后端核对 UI 涉及的主题和宿主状态所有权。这不等于完成整个 Rust 业务内核的功能测试，也不等于所有平台、所有异常状态均已实测。

## 补充：全仓滑块与分段选择检查

范围包括 `web/src` 共用 Web / Desktop Renderer、`apps` 和 `examples` 内的 UI 源码。开关共 9 个 Toggle 调用点（工具 2、浏览器 4、安全 1、外观 1、MCP 编辑 1），另有 MCP 服务页 5 个原生开关声明；范围滑块共 6 个声明（浅深对比度各 1、焦点水平/垂直、遮光、模糊）。分段选择另核对 MCP 连接方式、GPT Web 方式选择器及工具栏、Markdown 预览模式与知识图谱视图。普通复选框和下划线 Tab 保留各自用途，不套用开关外观。当前模型表单已没有 URL 路径分段选择；未恢复并行工作中移除的控件。

| 检查项 | 发现 / 修复 |
| --- | --- |
| 开关几何 | 原 Toggle 的布局边框在 0.5px / 1px 下影响滑块位置，默认及小号左右端点不对称；改为共享内部描边，36×20px 轨道 / 14px 滑块 / 3px 边距，28×16px 轨道 / 12px 滑块 / 2px 边距，用 transform 切换端点 |
| 重复样式 | MCP 服务页另写 32×18px 开关，继承全局文本框样式的风险未隔离；5 处统一使用 `.control-switch`，清除未使用的 `.toggle-switch` 旧 CSS |
| 可点击、焦点、禁用 | Toggle 隐藏输入原本宽高为 0；现在覆盖至少 24px 高的 label，保留原生 Space 和禁用行为，使用双环阴影；MCP 原生 label 也至少 24px 高 |
| 语义 | 开关使用原生 checkbox + `role=switch`，补齐新增 MCP 开关名称、工具/技能对象名；GPT Web 两处模式按钮增加 `aria-pressed`，文档模式按钮增加 Tab / `aria-selected` |
| 范围滑块 | 实测原来带 8px padding、文本框底色及内部描边；现在 padding 0、透明底、无默认描边，28px 高、键盘焦点可见；百分比 / px 使用 `aria-valuetext`，数值等宽，Grid 中间列可收缩 |
| MCP 分段几何 | 34px 轨道不再装固定 24px 按钮；按钮伸展为 30px，上下各 2px，切换后高度不变 |

真实 daemon 入口复核：浏览器四个禁用开关、安全页小号开关、工具页对象名称、MCP 服务连接/网络/活动开关、MCP 新增分段切换及草稿启用开关。MCP 草稿实测点击关闭 → Space 开启，焦点环可见；没有提交配置或改变服务、网络与安全规则。外观深色预览实测水平焦点 62% → 63%、Home → 0%、End → 100%，恢复 62%；浅色预览实测对比度 52 → 53 → 52，退出恢复活动主题。截图：`range-controls-dark.png`、`range-controls-light.png`、`switch-controls-final.png`，均在 `target/ui-audit-2026-10-24/`。

检查与 build 通过；定向 `test:appearance`、`test:mcp-server`、`test:browser-settings`、`test:conversation-disclosure`、`test:web-model`、`test:right-pane` 通过。本轮验证使用 macOS 当前 Chromium 页面，没有新增 Safari、Windows/Linux 或各触屏设备实测；GPT Web 条件入口与冲突合并选项仅核对源码，未触发真实对话绑定或合并写入。相关文件 `git diff --check` 通过；全仓检查曾报告并行工作文件 `crates/magi-bridge-client/src/tests.rs:889` 末尾空行，未修改该无关文件。

## 补充：中性提示与只读值的背景清理

真实模型设置页复核 `请求协议 → 实际请求`：原先外层 `.protocol-endpoint` 和全局 `code` 各提供一层底色；现在外层无底色/描边，`POST /v1/messages` 也不再套代码标签底色，仍保留等宽字体与 `aria-live`。选择框和输入框仍保留控件底色，错误、警告、阻塞和权限提醒仍保留状态背景。

全项目筛查后同步简化中性只读说明：插件指南 lead、统计图片用量说明、MCP 空参数/空环境变量/空 Header 行、右栏普通文件说明、无差异文件说明均不再伪装成卡片；错误 diff、右栏警告、知识索引状态、隔离提示、Web 会话阻塞和 MCP 连接错误继续保留背景以表达状态。全局 `code` 默认回到透明，仅 Markdown 内联代码恢复代码底色，代码块组件继续自带代码背景。

本项复核：在 daemon 主入口真实打开模型配置、插件开发规范、添加 MCP 服务器，截图确认实际请求端点、指南 lead、空参数与环境变量提示均无独立底色。其他上述低频状态通过源码筛查，未声称逐个触发真实页面。`check` 为 0 errors / 17 warnings（其他控件迁移留下的未使用 CSS），`build` 通过；构建时发现并修复知识图谱表单四处 i18n 字符串缺失结束引号。未运行全仓测试。

真实页面截图：`target/ui-audit-2026-10-24/plain-protocol-hint.png`。`npm --prefix web run check`、`test:appearance`、`test:markdown-image`、`test:file-reference`、`test:right-pane` 通过。尚未在 Safari、Windows/Linux 和高对比度系统主题下实测。

## 第一组：视觉与动效

| 要求 | 检查发现的问题 | 修复与代码位置 |
| --- | --- | --- |
| 微对比层级、双环焦点 | 默认配色偏冷，区域分层依赖描边，输入焦点使用单圈高对比强调 | `crates/magi-appearance/src/builtin.rs` 默认主题改为温灰/炭黑；`src/appearance/runtime.ts` 从当前主题派生不同表面，保留自定义主题权威；`src/styles/global.css`、`InputArea.svelte` 用双环阴影表达焦点 |
| 暗色字重补偿 | 各组件写死字重，切换暗色后视觉粗细不一致 | 共享 `--font-normal/medium/semibold/bold`，浅色 400/500/600/700，暗色 360/460/560/660；组件统一消费 token，独立预览继承当前字重 |
| 水墨流式文字、图标对齐 | 新文字直接出现；图标基线不统一 | `renderers/MdText.svelte` 只对新挂载的流式分词播放 90ms、6px 模糊到清晰动画，缓动 `.215,.61,.355,1`，同批相邻延迟 30ms；批次延迟上限 210ms，避免长回复积压；`Icon.svelte` 使用 `vertical-align:-0.125em` |
| 数字、展开、正文排版 | 数字变化可能带动布局，正文缺少统一避孤字规则，多个详情展开直接跳变 | 全局及统计、计时区域使用 `tabular-nums`，正文 `text-wrap:pretty`；`Disclosure.svelte` 用 Grid 0fr→1fr；对话、思考、工具、代码、计划、知识、终端、通知、设置工具/技能、代理上下文及运行记录接入；收起后卸载且关闭期间设 `inert` |
| 动效性格、反馈自熄 | 侧栏和反馈缺乏区别，复制反馈时间与计时器清理不统一 | 全局定义质量为 1 的 1200/80 阻尼响应与 380/18 微弹响应；侧栏抽屉使用前者，复制对勾使用后者；复制反馈 2340ms 开始淡出，2500ms 清理，重新复制重置计时器；尊重减少动态效果设置 |

精确连续字重取决于系统字体支持。水墨效果用于流式正文叶子文本，不在稳定历史、链接、代码高亮或整段 Markdown 上反复重播。拖动分割条仍直接跟手，不用弹簧延迟指针。

## 第二组：交互容错

| 要求 | 检查发现的问题 | 修复与代码位置 |
| --- | --- | --- |
| 中文输入防误发 | 单独判断组合态无法覆盖组合结束后补发 Enter；仅保护聊天输入会漏掉重命名、弹窗和 portal 菜单 | `InputArea.svelte` 保留组合态、`isComposing`、229 检查，补 500ms 保护；`lib/ime-enter-guard.ts` 在 body 捕获阶段统一拦截。组合中的回车保留原生选字默认行为；Shift/Alt+Enter 继续换行；卸载清理监听 |
| 自动滚动防抢 | 用户滚动意图与程序跟随存在竞争；输出增长时按钮距离可能过期 | `MessageList.svelte` 收到主消息列表滚动意图立即撤销跟随，向下回到距底部 40px 内恢复；超过 120px 显示回到底部；流式增长更新距离与阅读锚点，程序恢复不能重新启用跟随。沿用原滚动协调器及会话视图状态 |
| 单层边框、无抖动描边 | 外层容器与内按钮重复描边；部分搜索框继承了不必要的内部描边 | 主容器、输入框和设置公共卡片使用 inset 阴影，内按钮用底色/文字区分；`--magi-hairline` 在 2dppx 及以上为 0.5px；知识与 Git 搜索避免双重描边，焦点集中在搜索容器；保留必要的键盘可见焦点、状态线和拖放虚线 |
| 同心圆角 | 弹出菜单与内按钮使用同一半径，间距不均匀 | 输入菜单、分支菜单、模型菜单等外半径由内控件半径加 padding 推导；遵循现有布局与尺寸 token |
| 分屏防失控 | 鼠标可能穿透预览，取消路径不完整，重置比例与侧栏占宽混算 | `lib/panel-resize.ts` 统一指针事务：透明 Top Layer 遮罩、捕获、禁选、拖动结束点击抑制；Esc、失焦、pointercancel、失去捕获及卸载恢复起始比例；双击按实际内容轨道等分，受已有最小宽度约束；超过 82% 松手沿既有入口收起右栏。`web/panel-layout.ts` 排除侧栏宽度，Web 与 Desktop 沿各自原有布局入口提交 |
| 文件名后缀保留 | 单一 text-overflow 会把后缀一起截断 | `FileName.svelte`、`lib/file-name.ts` 分离主名和完整后缀，仅让主名省略；覆盖文件树、变更列表、附件引用、文件 Tab、路径、下载及技能文件清单；支持 `.tar.gz`、`.d.ts`、大小写后缀、Windows 路径、点文件和无后缀文件 |

## 真实 UI 验证

使用 `http://127.0.0.1:38123/web.html` 的真实 daemon 入口与实际 Electron 开发窗口，没有另搭展示用聊天页。

| 界面/动作 | 已取得的证据 |
| --- | --- |
| 浅深主题聊天 | 实际切换并截图；正文计算字重分别为 400/360；输入区焦点无 outline，父容器呈现双环阴影；代码高亮可加载 |
| 设置全部分类 | 模型、工具、浏览器、代理、偏好、安全、统计、外观、项目、插件、GPT Web、MCP 服务逐页打开；桌面浅深主题及 390px 深色窄屏均检查；内容区 scrollWidth 等于 clientWidth，页面无水平溢出 |
| Web 分屏 | 实际双击分割条：1066px 内容轨道分为 529px + 8px + 529px；实际拖过 82% 后右栏收起 |
| Electron 分屏 | 实际开启右栏、双击分割条、打开新增面板菜单；重启旧开发宿主后右栏与高亮恢复正常加载 |
| 长文件名 | 打开 `conversation-response-core-architecture-redesign.md`：文件树主名可见宽 220px、实际内容 268px，Tab 主名可见宽 103px、实际内容 318px；两处 `.md` 后缀均保持约 20px 完整可见，文件预览正常 |
| 复制与折叠 | 实际复制代码后出现成功反馈，之后恢复复制状态；实际打开工具明细与技能详情；设置窄屏展开内容无页面溢出 |
| 长流式与阅读 | 在既有 UI 验收会话发送 80 行和 160 行纯文字请求；输出期间上滚显示回到底部，内容继续增长时仍留在历史阅读区，未拉回最新；点击回到底部后实测底部距离为 0 |
| 项目页面 | 打开工作区文件树、文件预览、变更和知识入口，确认真实加载；未执行知识清理、Git 写入或权限变更 |

截图保存在 `target/ui-audit-2026-10-24/`。主要证据：`filename-preview-dark.png`、`chat-mobile-light.png`、`stream-reading-position.png`、`electron-split.png`、`settings-*.png`、`settings-dark-*.png`、`settings-mobile-*.png`。含连接令牌的 MCP 截图不作为交付证据保留。

## 定向验证

通过：

- `npm --prefix web run check`：0 errors、0 warnings。
- `npm --prefix web run build`。
- `npm run check --workspace @magi/desktop`。
- `cargo check -p magi-appearance`。
- Web 定向脚本：`test:appearance`、`test:composer-input`（59 场景）、`test:chat-interaction`、`test:panel-layout`、`test:conversation-disclosure`、`test:history-pagination`、`test:file-reference`、`test:bridge`、`test:right-pane`、`test:right-pane-browser`、`test:message-scroll-input`、`test:message-scroll-coordinator`、`test:desktop-right-pane-intent`、`test:desktop-panel-activation`、`test:skill-detail`、`test:mcp-server`、`test:workspace-onboarding`、`test:active-agent-center`、`test:runtime-state`、`test:agent-run`。
- `git diff --check`。

`test:chat-interaction` 直接执行生产滚动处理函数及 IME/指针事务，覆盖组合态与 500ms 保护、40px 边界、输出增长、程序恢复、文件后缀、82% 几何、Esc/失焦/取消/失去捕获/卸载清理和结束点击抑制。它不是系统输入法或物理 guest 的替代验收。

测试脚本的 Vite 已分配独立临时 cacheDir，避免 SSR 回归改写正在运行的开发页面依赖缓存。曾出现的动态模块加载失败已在 Web 重载和 Electron 宿主重启后复验恢复。

## 尚未覆盖及运行环境发现

- 没有在 Safari、Windows、Linux 实测系统输入法，也没有验收这些平台的发行包；500ms/229/组合事件路径由生产处理器回归覆盖。
- 拖动取消等异常路径有定向回归；物理 Electron guest 内的点击穿透仍需专门 live 验收。此次创建了真实 `<webview>`，但开发环境的 Browser Tab 仍显示“已释放”，导航没有读回目标页，不能据此宣称 guest 验收通过。没有为掩盖该状态而改写 daemon 的 Browser 事实。
- 设置显示的第三方 MCP `computer-use` 本地可执行文件缺失，是现有环境配置错误；未替用户改动配置或授权。
- 未运行全仓测试、发布预检及所有破坏性/权限相关流程；本次没有改变 daemon 会话、任务、权限或持久化协议。

## 运行

从项目根目录执行 `./scripts/dev-daemon.sh`，访问 `http://127.0.0.1:38123/web.html`。Desktop 按 `docs/README.md` 的既有构建、启动入口运行。布局所有权与业务入口保持原设计。

## 第三组：浮层分类与防误操作

全仓再次清点了 `web/src` 的 Modal、popover、alert、toast、drawer、confirm 入口，并对真实 daemon 页面逐项抽查。现在浮层有明确的 Tooltip / Toast / Alert / Popconfirm / Modal 语义；Magi 的长内容和表单按代理新增模式切换到内容区，短确认保留 Modal，高频删除改用 Popconfirm。

| 入口 | 组件分类 | 修复 |
| --- | --- | --- |
| `Modal.svelte` / `SettingsDetailPage.svelte` | Modal / 内容页 | 400/520/720px 短确认；Modal 陷获焦点；长表单在内容区切换，保留导航与返回焦点；通过 `overlay-stack` 协调 Esc |
| 设置 MCP、仓库、Skill 来源、角色、外观编辑 | 内容页 / Form | 左侧导航保留，关闭返回列表并保留草稿，保存成功清理 |
| 插件指南、浏览器批注 | 内容页 / Form | 长文在插件内容区展示；批注自动保留当前编辑草稿 |
| 知识条目、知识图谱关联、插件卸载、审计单条删除 | Popconfirm / Modal | 先确认再写入 daemon；按钮使用“删除此知识条目”“删除关联”“卸载”动作文案 |
| 图片/上下文移除 | Toast + Undo | 直接执行并提供撤销，避免二次确认疲劳 |
| `ToastContainer` | Toast | 支持动作按钮、定时器卸载清理、`role=status` |
| Electron 原生对话框/浏览器 guest JS dialog | 宿主 Alert / Confirm | 继续由 Main 的受控 bridge 管理，不在 Web Renderer 复制第二套事实源 |

未把通知中心、运行态详情、模型菜单和浏览器工具菜单误标为 Modal：它们保持 Popover/Alert 语义；删除、网络暴露、令牌吊销、文件还原等不可逆操作继续使用 Modal 并写明影响。

### 第三组真实 UI 验收

在 `http://127.0.0.1:38123/web.html` 真实 daemon 页面打开设置 → 工具 → 添加 MCP 服务器：表单位于设置内容区，左侧导航可用，按钮文案为“返回列表/保存”，初始焦点落在返回按钮。实测修改未提交服务器名称 → 返回 → 重新添加，名称草稿保留；Esc 只返回工具列表，焦点回到“添加服务器”。凭据草稿仅存 Renderer 内存，刷新/退出清除。

设置 → 插件的来源与指南使用内容页切换。复核发现指南从加载态切换正文时重建内容页，导致 Esc 返回后焦点落到页面根；已将 `PluginGuidePage` 改为稳定的单一页面外壳，真实页面复验 Esc 返回后焦点回到“插件开发规范”。插件来源草稿基线改用 daemon 返回的配置，重新进入时不恢复其他客户端修改之前的旧草稿。知识与附件入口沿主工作台检查。`npm --prefix web run check`（0 errors / 0 warnings）、`build`、`test:overlay-interaction`、`test:confirm-dialog`、`test:appearance`、`test:settings-store`、`test:mcp-config`、`test:skill-detail`、`test:agent-run`、`test:chat-interaction`、`test:composer-input`、`test:panel-layout`、`test:conversation-disclosure` 已通过。

## 第四组：保留下拉方案，区分数据选择与命令菜单

以本轮最新要求为准：现有下拉继续保留，不按 2 项或少于 5 项改成单选；主对话权限保留原自绘面板，主对话模型与推理强度保留原组合面板，设置中的协议和推理级别继续使用原生下拉。清除了本轮曾加入的短选项单选分支、相关样式和临时布局参数。现有开关、Tab 和原本平铺的按钮组维持各自语义。

| 组件 / 入口 | 分类与改法 |
| --- | --- |
| `SelectField`；模型协议、推理级别、语言设置、MCP 审批 / 工作区 / 令牌 / 保留时间、安全规则、插件枚举、知识关系、工作副本模式、上下文单位 | 数据 Select：保留带框下拉；超过 10 项增加搜索，保留原选中值，不因搜索自动选择第一项；受控更新仍等待原拥有者接受 |
| `LocaleSwitcher` | 数据选择：恢复原图标按钮与语言浮层，复用 shell popover 状态和关闭链路 |
| `InputArea` 权限 | 数据选择：恢复原按钮、权限说明与推荐标记；使用 listbox / option 暴露选中值，沿原本偏好与发送入口生效 |
| `InputArea` 模型 / 工作区 | 数据选择：长列表加搜索和无匹配提示，关闭后清空查询；不提交搜索内容、不改会话绑定入口 |
| `EnginePicker` | 数据选择：保留原自绘下拉及模型状态；长列表搜索，修复过滤后的分组，支持方向键、Home / End、Enter / Esc 和活动项滚动；搜索框中的 Home / End 保留编辑用途 |
| `ModelConfigForm` 模型候选 | 可输入模型名及候选列表：保留输入、拉取和 portal 定位，补长列表搜索和无匹配提示 |
| `SettingsMcpServerAudit` | 数据筛选：带框 Select，与清理等操作按钮区分 |
| `GitContextControl` 分支 | 数据选择：保留已有搜索与分支选中态 |
| `WebModelModeChooser` 已保存会话 | 点击后绑定会话的操作列表：保留原展开区域和绑定入口，补长列表搜索与无匹配提示，移除不完整的 listbox 语义 |
| `TurnNavigationRail`、浏览器批注历史 | 导航命令：保留 menu / menuitem，长列表补搜索；不回填命令文字 |
| 添加上下文、右栏新增面板、浏览器更多操作 | 命令 Dropdown：保留图标 / 文字触发与点选执行 |
| 目录根位置、设备预设 | 保留原生下拉；路径与视口数值继续允许直接输入 |

自绘候选 / 命令面板限制在 264px 内并滚动；原生 select 的系统弹出高度由浏览器控制。原弹出方向、定位、portal、Top Layer 和移动端承载方式保留。搜索入口隐藏时清除残留查询，避免列表缩短后留下无法清除的过滤条件。

### 第四组验证

`npm --prefix web run check` 为 0 errors / 0 warnings，生产构建通过。定向 `test:composer-input`（59 场景）、`test:appearance`、`test:context-ring`、`test:mcp-server`、`test:web-model`、`test:settings-store`、`test:overlay-interaction`、`test:turn-navigation`、`test:right-pane-browser` 通过。轮次回归原先硬编码旧布局边框，已更新为检查现行 inset 描边；未为测试恢复旧样式。Web 改动的 `git diff --check` 通过。

真实 daemon 页面验收：主对话权限按钮展开三个候选，当前值保持不变；配置页请求协议与推理级别均为原生下拉。代理引擎共 12 项，未过滤面板实测 260px 且可滚动；搜索“演示引擎 09”只显示对应引擎，搜索无匹配有明确提示；Esc 关闭候选、设置页保持打开、焦点回到“代理引擎”，重开恢复 12 项。主对话模型搜索无匹配时保留原模型与推理级别。未保存或修改模型绑定、权限、令牌等真实配置。截图：`target/ui-audit-2026-10-24/selection-search-final.png`。其他条件入口以源码与上述定向回归核对；未宣称全部状态均已逐一真实操作，也未运行 Windows / Linux 发行包验收。
