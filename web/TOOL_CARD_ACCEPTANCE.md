# 工具卡片标准化验收（2026-10-24）

工具卡片标准化已合入 main；原 `codex/tool-card-standardization` worktree 已归档。工作台继续消费 daemon 的工具、任务与权限事实；新组件 `ToolCardMeta` 只负责展示，不提交业务请求、不推断终态。

## 统一规则

- 标题栏从右向左：展开箭头、状态点、耗时及来源/统计。状态与箭头仅在存在时渲染；有状态但无箭头时保留 22px 外侧操作间距，状态灯与相邻行对齐；无状态不保留中间空列；已有控件保持垂直居中与 8px 间距。代码复制、图表操作放在状态列左侧。
- 摘要模式的整轮「已处理 / 处理中 + 耗时」提示独立左对齐，箭头紧随文本，按钮随内容宽度；不使用工具卡片的右侧尾栏。
- 成功绿点、运行蓝点、失败红点，待定/结果未确认黄点、取消中性点；用 `title` 和无障碍名称补充含义，不增加重复的可见状态文字。
- 主题、明暗、字重、语义色沿用正式外观运行时。共用 10px 圆角、内部细轮廓、中性表面；没有装饰色条、外部投影或套娃边框。
- 工具标题栏最小高度 36px，代码栏 36px；横向留白 13px。窄内容列通过 container query 隐藏耗时，为名称、后缀和状态让位。
- 折叠复用既有 `Disclosure`：CSS Grid 过渡，折叠内容 inert，过渡后释放昂贵内容；保留键盘操作与减少动态效果。
- 所有复制回执 2.5 秒恢复，按钮不因文字变化改变宽度。

## 组件清单与模式区别

| 入口 | 详细模式 | 摘要模式 |
| --- | --- | --- |
| `ToolCall`：搜索、MCP、Skill、HTTP、Browser、文件读取 | 独立工具卡，原始输入、输出、错误按需展开 | 复用同一组件，以过程行置于原有分组中；状态点仍可见 |
| `TerminalSessionCard` | 中性卡片、主题化日志区，默认折叠 | 紧凑过程行，展开仍能查看完整日志与退出码 |
| `ThinkingBlock` / `ConversationPhase` | 思考卡片 | 阶段标题与正文，不再内嵌详细思考卡 |
| `ToolCall` agent 变体 / `ConversationAgentGroup` | 角色与任务卡，点击打开原任务 | 紧凑任务行，保留任务身份、失败诊断和同一个详情入口 |
| `FileChangeCard` / `DiffCodeBlock` | 产物默认可见；文本、二进制、无差异、不可预览均有对应内容 | 产物保持可见，单独展开/折叠；不随过程组隐藏 |
| `PlanCard` / `ExecutionSteps` | 共用卡片几何，步骤证据按需展开 | 步骤沿现有投影保持可见；普通计划按原有过程归类 |
| `CodeBlock` / `PlainTextBlock` | 代码/JSON 带轻量操作栏；纯文本保留轻容器 | 最终回答中的格式化内容保持可读，不随过程收起 |
| `DiagramRenderer` / `GeneratedImageBlock` | 图表、生成中/已生成图片使用统一表面和状态栏 | 产物保持可见；图表仍使用原渲染器、预览与复制入口 |
| `ModelFailureCard` | 错误诊断使用单层卡身，保留恢复动作 | 最终错误仍可见，不藏入已完成的过程 |
| `ToolApprovalAction` / 提问历史 | 保留原授权/提问入口和动作语义 | 待处理授权保持可见，提问历史为独立交互行 |
| `ConversationTurn` / `ConversationToolGroup` | — | 整轮处理提示左对齐；阶段与工具组共用右侧尾栏；保留原有分组、稳定 key 和手动展开逻辑 |

## 本次发现并修复

1. 只读工具失败时原来无法展开诊断：失败/结果未确认现在保留展开能力。
2. 步骤卡片绕过消息内边距而产生右侧错位：统一步骤壳与消息列的横向基准。
3. 摘要模式的阶段、工具组和最终回答的右侧位置不同：统一留白和尾部列。
4. `FileSpan` 在标题栏内形成有底色的第二层控件：标题栏内恢复中性，只在悬停时反馈。
5. 文件差异采用较重红绿底和左侧彩条：降低差异底色，使用加减号保留非颜色语义。
6. 图片生成使用渐变、玻璃胶囊和多组三点：收敛为同一状态栏与轻量占位，减少动态效果时停止动画。
7. 模型错误可能同时拥有消息外壳与错误卡身：只保留错误卡身。
8. 实际图表验收发现 `elkjs/lib/elk.bundled.js` 的 CommonJS 依赖未预打包：补入现有 Vite 预打包清单，真实 Mermaid 渲染恢复。

## 运行入口

正式组件验收页：`/src/acceptance/tool-cards.html`，仅由开发服务托管，不进入正式构建入口。它直接挂载正式 `MessageItem` / `ConversationTurn`，没有复制卡片 UI。

工作区图片验收可为该页传入 `workspaceId`、`workspacePath` 和 `sessionId`，并把同目录的 `card-preview.svg` 放入一次性工作区根目录；未指定工作区时不请求该图片。固定授权数据仅用于展示；实际授权回归在下面的真实会话中完成。

独立环境使用 daemon `38124`、worktree Vite `3002`、状态目录 `/tmp/magi-tool-card-validation-state`，没有使用主 daemon 的设置或会话。

## 已完成验证

- `npm --prefix web run check`：0 errors / 0 warnings。
- `npm --prefix web run build`：成功。
- 相关回归通过：`test:tool-call-display`、`test:conversation-disclosure`、`test:image-generation-preview`、`test:chat-interaction`、`test:appearance`、`test:tool-display`、`test:web-model`、`test:mermaid-source`、`test:user-question`、`test:file-reference`、`test:tool-error`。
- 浏览器正式组件矩阵：8 个皮肤 × 详细/摘要 × 820/420/320px，共 48 组。检查内容列与标题栏溢出、状态点/箭头垂直中心、尾栏相对卡片外缘 13px 的基准、真实图片加载，结果均通过。
- 手动展开后追加运行工具、再结束输出：已有工具组和搜索卡保持展开；新工具组由蓝点转为绿点。
- Mermaid 真实渲染成功；已生成图片通过 daemon 文件读取入口加载成功；长文件名保留 `.svelte` 后缀。
- macOS 真机运行 worktree Electron App，连接独立 daemon；通过正式设置页切换详细/摘要模式；在一次性工作区实际读取不存在文件、处理单次 Shell 授权、执行 `printf` 并得到 `CARD_ACCEPTANCE_OK` 与退出码 0。
- 真实会话中的错误诊断和终端输出在两种模式均可展开。点击复制后粘贴到输入框，内容与 TypeScript 代码一致；反馈随后恢复为复制图标，未发送粘贴内容。

本次真实链路使用本地受控模型响应来稳定覆盖状态，文件读取、Shell、权限接纳和消息持久化均由真实 daemon 执行。没有调用外部模型服务。没有执行全仓测试、重新打包发行版或验证 Windows/Linux 宿主；本次没有修改这些宿主或 wire schema。

本机截图与测量记录保存在当前聊天的 `tool-card-standardization` 可视化目录：`matrix.json`、`electron-detailed.png`、`electron-summary.png` 及组件清单截图。

## 合入 main 的复核（2026-10-25）

整合 main 的统一图标、主题层级和容器自适应改动；文件身份完整保留 main 的 59 类目录、566 后缀和 65 个特殊文件名，`FileTypeIcon` 继续使用主题字重。代码卡片同时保留共享线性图标、右侧折叠箭头和固定尺寸复制回执。摘要整轮提示仍独立左对齐；没有改变 daemon 事实、授权入口或用户手动展开的所有权。

合并后的类型检查为 0 errors / 1 warning（main 已有的 MCP 服务页 `.card h4` 未使用选择器）；生产构建通过。Web 全量 `npm --prefix web test` 及工具展示、过程折叠、图像预览、聊天交互、文件引用、Mermaid、图标库和文件类型定向回归通过，`git diff --check` 通过。此前真实浏览器和 Electron 验收属于上文对应版本，本次合并未重复执行真实 UI 验收。

## 工具行密度与状态列复核（2026-10-25）

有状态但无折叠箭头的行在尾部保留 22px 操作间距，状态圆点与可折叠工具共用竖直基准；无状态代码栏不保留状态空列。工具行改为 36px，收起与只读行使用前景 1.8% 轻底、4% 轮廓，摘要过程行继续透明。文件名与目录间隔 8px；360px 以下卡片优先显示文件名和完整后缀，目录仍可通过完整路径 title 查看。消息滚动容器采用自适应居中阅读列（上限约 1040px），保留原滚动所有权及轮次轨道。

真实 daemon 38123 的正式组件验收页检查浅色、深色与原壁纸皮肤；820px / 320px 验收容器中 15 个工具标题的状态圆心分别为 x=795 / x=545，行内垂直偏差均为 0，工具标题均无横向溢出。展开搜索后输入、输出可读，收起后状态不移动；摘要模式的过程卡片计算背景透明、阴影 none。窄栏文件后缀保留 `.svelte` / `.md`，目录正确让位。另在主会话页面确认原消息滚动容器加载和窄分屏安全边距，未修改业务配置或发送消息。未进行宽屏视口模拟或重新启动 Electron；1040px 上限由源码和构建核对。截图：`target/ui-tool-rows-2026-10-25/light.png`、`narrow.png`。

验证：类型检查 0 errors / 0 warnings；生产构建通过；`test:tool-call-display`（新增有状态无箭头及无状态代码栏回归）、`test:conversation-disclosure`、`test:turn-navigation`、`test:file-reference`、`test:chat-interaction`、`test:icon-library`、`test:panel-layout` 通过。原有 MCP 未提交改动保留。

## 代码输出面板统一（2026-10-25）

移除代码正文、行号栏与标题栏之间重复叠加的表面，整个代码块沿同一主题卡片底色；行号背景和竖线取消，外轮廓放到正文上层，标题栏使用内部细分隔线，不由 border 撑高。代码标题栏实测 36px，行号与代码均为 12px / 19.8px 行距且首行顶边相同。纯文本与 Diff 使用相同字体/行距 token；Markdown 的通用 pre/code 规则避开专用面板，删除旧行号/标题底色及无调用的代码边框 token。没有修改高亮、复制内容、文件预览、Diff 数据或折叠状态入口。

验收页 `?focus=code` 直接挂载正式消息组件，补充与用户截图相同的三行 JavaScript 和长 JSON。真实 daemon 38123 检查浅深色、820px / 320px 两种容器：代码面板标题高 36px、正文与行号背景透明，行号首行与代码偏差为 0；窄栏长 TypeScript / JSON 仅在 pre 内滚动，卡片和页面横向溢出均为 0。JavaScript 折叠/重开正常，复制后显示成功回执并自动恢复；主工作台已有历史会话的三个 JavaScript 面板也确认使用同一几何与表面。未发送消息或执行业务写入；未重新验收 Electron/Windows/Linux 包。

本次验证：`check` 0 errors / 0 warnings、生产 `build`、`test:appearance`、`test:right-pane`、`test:conversation-disclosure`、`test:tool-call-display`、`test:file-reference`、`test:desktop-context-menu`、`test:mermaid-source`、`test:icon-library` 及 `git diff --check` 通过。截图：`target/ui-code-panels-2026-10-25/dark.png`。本轮改动未提交。

## 消息列左侧对齐（2026-10-25）

移除助手纯消息和执行步骤的重复横向内边距，摘要轮次标题从共享内容基准起排；列表编号预留至少 4.5ch，三位数编号不再伸到卡片外侧。保留卡片内部留白和嵌套列表缩进。

真实 daemon 38123 的现有聊天会话实测正文、代码卡片与完成耗时左边界均为 x=378；正式组件验收页在常规宽度下正文、工具、代码、耗时均为 x=69，320px 容器下均为 x=319 且没有横向溢出。摘要模式标题、产物工具与正文也均为 x=319。未发送新消息或修改会话数据；未重新验收 Electron。`check` 0 errors / 0 warnings，`test:turn-navigation`、`test:conversation-disclosure`、`test:tool-call-display` 通过。截图：`target/ui-message-alignment-2026-10-25/main.png`。改动位于 main 工作区，尚未提交。

## 历史输出加载入口（2026-10-25）

`ToolCallRenderer` 不再在卡片外渲染历史截断提示，通过共享 snippet 把入口放入 ToolCall / TerminalSessionCard 的输出详情内。成功紧凑文件读取继续直接打开文件，不展示无效的原始输出加载按钮；可展开工具加载完整内容后保持展开，失败可重试。文案改为“历史预览 / 未加载”，避免将历史分页误称为折叠。

真实 daemon 38123 静态构建验证：已有“项目现状分析”会话的 49 个工具卡片收起时，外露历史提示由 21 行降为 0。展开 Cargo.toml 的历史工具，详情内显示另有 378 字符未加载；点击后完整输出显示在原详情内，保持展开，收起重开仍保留已加载内容。新增 SSR 回归覆盖文件、终端、通用工具收起和产物默认展开；未验证网络失败的真实交互或重新运行 Electron。截图：`target/ui-tool-output-2026-10-25/collapsed.png`。

## 完成耗时与时间国际化（2026-10-25）

完成回执跟随界面语言：中文为“总耗时：1分8秒 2026年10月10日 6时20分”，英文为“Total time: 1min8sec 2026-10-10 06:20”。完成日期使用完整本地日期；摘要模式的完成耗时复用同一单位格式。运行计时与用户消息原有的时间规则保留。回执在窄栏按耗时和日期两段换行，保留等宽数字。

`check` 0 errors / 0 warnings；生产构建、`test:relative-time`、`test:tool-call-display`、`test:conversation-disclosure` 通过。时间回归覆盖中英文、零秒、不足一秒、小时、凌晨和非法时间。真实 daemon 38123 现有会话切换中英文后，完成回执即时更新且无横向溢出；已恢复中文设置。截图：`target/ui-runtime-i18n-2026-10-25/zh.png`、`en.png`。尚未提交。

补充：桌面完成时间默认隐藏，仅悬停助手消息/底部回执或键盘聚焦后显示在总耗时后；总耗时保持常显。用 opacity 保留几何，淡入淡出不改变消息高度，减少动态效果偏好下取消动画。无悬停能力的触屏直接显示时间，保留查看途径。真实主会话验证默认 opacity=0、悬停和纯键盘聚焦为 1、移出并失焦恢复 0；回执高度始终为 22.796875px。截图：`target/ui-hover-time-2026-10-25/default.png`、`focus.png`。

## 用户消息与回复间距（2026-10-25）

用户时间与编辑/复制操作行改为在消息内部固定占位，悬停只切换可见性；操作行可换行，无悬停能力的设备直接展示。助手正文的负间距仅用于相邻助手消息，用户消息后的首条回复保留正常间距。

`?focus=spacing` 使用正式 MessageItem / ConversationTurn 复现截图中的短用户消息与后续回复。独立 daemon 38124（状态目录 `/tmp/magi-message-spacing-validation`）的真实浏览器检查覆盖详细/摘要模式及 820/420/320px 内容列：操作行始终为 static 布局，没有与回复重叠或页面横向溢出；常规宽度悬停前后回复顶边均为 376.875px，操作栏显示后与回复相隔 16px。主 daemon 38123 保持运行，生产构建已更新；本次未重验 Electron 宿主。
