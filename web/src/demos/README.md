# 消息卡片视觉 Demo

启动既有 `./scripts/dev-daemon.sh` 后访问：

http://127.0.0.1:38123/src/demos/message-cards.html

这是独立的开发预览，不改变正式消息渲染组件，不进入发行版入口。HTML 使用明确的演示数据；审批、重试、预览等动作只给出标为演示的反馈，不执行真实业务操作。复制代码是实际剪贴板操作，成功反馈 2.5 秒后消失。

## 皮肤与样式边界

- `message-card-preview.ts` 复用正式 `appearance/runtime.ts`，从 daemon 的现有外观入口读取皮肤列表和当前皮肤，使用正式的颜色、明暗、材质和壁纸投影。
- 选择皮肤只调用 `previewAppearanceTheme`，恢复使用 `restoreActiveAppearance`；没有激活、更新主题或保存设置的写入。
- 卡片底色、边框、文字、悬停和强调色映射到正式主题变量，不把任何一种皮肤的颜色当作产品品牌色。失败、警告、成功和语法高亮使用独立语义色。
- 工具状态沿用 Magi 的语义 token：成功 `--success` 绿点、运行中 `--info` 蓝点、失败 `--error` 红点；只显示圆点，状态通过 title 和 aria-label 提供，折叠后仍可见，运行点轻微呼吸并尊重减少动态效果。
- 卡片共用 10px 外圆角、42px 标题栏、13/14px 内容内边距；代码标题栏为 36px。高分屏轮廓为 0.5px，卡身无外部投影、无装饰彩条。
- 折叠采用 Grid，隐藏内容同步 inert 和 aria-hidden；摘要视图保留需要用户处理的错误卡片。支持键盘和减少动态效果设置。

## 覆盖与正式组件对应

| 演示类型 | 正式入口 |
| --- | --- |
| 连续消息、过程摘要 | ConversationTurn、ConversationPhase、ConversationToolGroup |
| 思考过程 | ThinkingBlock |
| 搜索、MCP、Skill 调用 | ToolCall、ToolCallRenderer |
| 终端输出 | TerminalSessionCard |
| 计划、执行步骤 | PlanCard、ExecutionSteps |
| 代码、JSON、纯文本 | CodeBlock、PlainTextBlock |
| 文件变更、Diff | FileChangeCard、DiffCodeBlock |
| 图表、图像 | DiagramRenderer、GeneratedImageBlock |
| 代理进行中、完成回执 | ConversationAgentGroup、ToolCall 的 agent 变体 |
| 授权、提问 | ToolApprovalAction、UserQuestionPanel |
| 错误、结果未确认、停止、无差异、预览不可用 | ModelFailureCard、ToolCall 及相应产物变体 |

共 24 张卡片，另含纯文本和行内提示。此处用轻量静态结构评审视觉方案，正式实现应回到对应组件和共享样式，不将 Demo 变成另一条消息渲染链路。

## 验证

`npm --prefix web run check`、`build` 通过。浏览器已核验实际皮肤读取、浅色与带壁纸皮肤、折叠/摘要切换、真实复制和演示审批反馈。没有触发 daemon 业务写入。
