# 工具卡片验收

视觉与交互要求以[UX/UI 规范](../docs/ux-ui-guidelines.md)、[Web 设计规范](DESIGN_SYSTEM.md)和[对话展示设计](../docs/conversation-display-modes.md)为准。本文只维护组件覆盖和复验入口；历史截图、修复过程和测试次数通过 Git 查询。

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

## 运行入口与场景

使用 `./scripts/dev-daemon.sh`，访问 `http://127.0.0.1:38123/src/acceptance/tool-cards.html`。该页直接挂载正式 `MessageItem` / `ConversationTurn`，只用于开发验收，不进入产品构建。

- 检查浅色、深色和自定义皮肤，以及详细/摘要模式下的宽栏和窄栏；长文件名、代码、错误和按钮不能撑破内容区。
- 展开工具后追加输出并结算，确认展开选择、状态、耗时和阅读位置保持；折叠与减少动态效果遵循共享 Disclosure。
- 使用 `?focus=code` 检查代码栏、行号、长 JSON、复制回执和滚动；真实图表须实际渲染。
- 工作区图片需要传入 `workspaceId`、`workspacePath`、`sessionId`，并把同目录 `card-preview.svg` 放到一次性工作区根目录；不指定工作区时不请求图片。
- 固定夹具中的审批只验证展示。授权、文件读取、Shell、失败恢复和持久化应通过隔离状态下的真实 daemon 会话验收。

## 自动检查与覆盖说明

运行 `npm --prefix web run check`，按改动选择 `test:tool-call-display`、`test:conversation-disclosure`、`test:file-reference`、`test:turn-navigation`、`test:appearance` 和 `test:mermaid-source`。发布构建使用 `npm --prefix web run build`。

记录本次实际执行的构建与场景；SSR 不能替代真实浏览器，Web 验收不能代表 Electron 或 Windows/Linux 发行包。不要将历史通过记录写成当前结果。
