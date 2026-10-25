# 工具卡片标准化验收（2026-10-24）

本次修改位于 `codex/tool-card-standardization` worktree；保留主检出及其未提交改动。工作台继续消费 daemon 的工具、任务与权限事实；新组件 `ToolCardMeta` 只负责展示，不提交业务请求、不推断终态。

## 统一规则

- 标题栏从右向左：展开箭头、状态点、耗时及来源/统计。状态与箭头仅在存在时分配空间，无状态或无展开能力时不保留空列；已有控件保持垂直居中与 8px 间距。代码复制、图表操作放在状态列左侧。
- 摘要模式的整轮「已处理 / 处理中 + 耗时」提示独立左对齐，箭头紧随文本，按钮随内容宽度；不使用工具卡片的右侧尾栏。
- 成功绿点、运行蓝点、失败红点，待定/结果未确认黄点、取消中性点；用 `title` 和无障碍名称补充含义，不增加重复的可见状态文字。
- 主题、明暗、字重、语义色沿用正式外观运行时。共用 10px 圆角、内部细轮廓、中性表面；没有装饰色条、外部投影或套娃边框。
- 工具标题栏最小高度 42px，代码栏 36px；横向留白 13px。窄内容列通过 container query 隐藏耗时，为名称、后缀和状态让位。
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
| `ConversationTurn` / `ConversationToolGroup` | — | 整轮、阶段与工具组共用右侧列；保留原有分组、稳定 key 和手动展开逻辑 |

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
