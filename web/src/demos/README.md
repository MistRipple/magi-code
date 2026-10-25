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

## 完整工作台提案

最终效果入口：<http://127.0.0.1:38123/src/demos/product-preview.html>。

`ProductWorkbench.svelte` 将卡片放回工作区、会话和预览的完整结构；
`WorkbenchCard.svelte` / `PreviewStatus.svelte` 负责演示卡片；
`WorkbenchSettings.svelte` 展示代理、MCP 的内容区切换。
`workbench-fixtures.ts` 是独立的固定演示数据。

- 场景：完整会话、交付结果、执行过程、等待确认。原有 24 卡片清单独立保留。
- 皮肤：读取正式外观运行时，支持当前安装皮肤的临时预览和恢复；无固定品牌色，无主题保存调用。
- 状态：成功绿点、失败红点、运行蓝点，待确认黄点、取消中性点。工具状态不附加重复文字。
- 卡片标题栏：右侧固定为「耗时/其他信息 → 状态圆点 → 展开/折叠箭头」，各列垂直居中；箭头始终最右，状态点不受标题或摘要长度影响。状态点和箭头只在存在时渲染，不预留空列；其余信息按 8px 间距向右收拢，箭头点击区域保持完整。
- 布局：桌面双面板可拖动、双击均分、Esc 取消、82% 吸附关闭；窄窗在内容区切换预览，手机导航整页切换。
- 配置：代理与 MCP 在内容区新增和编辑，不使用抽屉。单选自然排布，提示信息不附带背景；可恢复删除就近撤销，新增演示草稿保存在 sessionStorage。
- 交互：消息只在本地模拟，30ms 逐字输出、90ms 晕染；中文组合输入和结束后的伴生 Enter 受保护；手动离开底部后停止跟随，超过 120px 显示跳底。
- 预览：文件、演示 Diff、终端与代理回执切换；代码和路径使用真实剪贴板，复制回执 2.5 秒自熄。

此提案不接入 daemon 的会话、代理、MCP 或授权写入，不是另一套正式业务实现。
正式构建入口仍为 `web.html`，该 Demo 仅由现有开发服务托管。

参考：[Desktop Agent UI](https://github.com/Ludi-Herry/desktop-agent-ui) 的交互与视觉约束；
保留 Magi 的既有布局、主题事实源和组件入口。

## 统一图标库

入口：<http://127.0.0.1:38123/src/demos/icon-library.html>，工作台 Demo 顶部也有链接。

正式界面与 Demo 共用 `Icon.svelte` / `icons.ts` 的 Lucide 线性图形，不再各自维护 SVG。
预览列出全部独立图形与同义名称，并展示 14 / 16 / 20px 尺寸；皮肤选择只调用正式外观运行时的临时预览入口。
工具图标映射集中在 `tool-icons.ts`，状态圆点继续独立展示。

图标收尾：静态卡片页的普通 SVG symbol 直接读取共享图标目录和绘制属性；具名文件结果挂载正式 `FileTypeIcon`，完整工作台的附件与文件 Tab 也使用同一组件。图表与图片示例保持内容图形。统一图标检查命令：`npm --prefix web run test:icon-library`。

图标改造清理：删除无调用的 `circleOutline` / `taskRunning` 旧别名，以及静态卡片页已废弃的皮肤切换按钮、字段标签和未使用文本样式；预览统一读取现有皮肤选择器。完整工作台、卡片清单和图标库仍各有评审用途，保留三个入口。
