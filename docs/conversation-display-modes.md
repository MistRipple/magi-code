# 对话展示：完整模式与摘要模式

本文从当前代码整理对话区的展示设计，描述的是已经实现的行为与必须保持的约束。代码是唯一事实源；与本文冲突时以代码为准并同步修订本文。

## 两种模式

| 模式 | 设置值 | 呈现 | 适用 |
| --- | --- | --- | --- |
| 完整 | `original` | 按时间线逐条展示每个条目：思考、文字、每一次工具调用卡片 | 排查过程、核对每一步工具输入输出 |
| 摘要 | `summary` | 每轮折叠成「过程 + 最终回答」：过程按阶段和工具组收敛成可展开的标题，最终回答始终可见 | 日常使用，只关心结论与关键动作 |

两种模式只是**布局不同**，共用同一份事实投影：同一条目在两种模式里的状态、内容、工具结果、产物完全一致，切换模式不重新请求数据，也不改变任何条目的身份（key）。

模式是全局运行时偏好：

- 唯一写入口 `POST /api/settings/update`，只接受 `{ key: "locale" | "conversationDisplayMode", value }`，值分别限定为 `zh-CN`/`en-US` 与 `original`/`summary`，其他键或取值一律拒绝。
- 存储为设置快照顶层的 `conversationDisplayMode`，`runtime_settings_from_snapshot` 只读这一个位置，缺省为 `original`。

## 数据链路

```
Rust canonical turn/item（itemVersion 单调递增）
  → SSE / App Server 事件（turn_item_upsert，流式文字为 stream 增量）
  → turn-reducer：按 itemVersion 合并，增量按码点追加
  → turn-projection：每个 item 投影为一个渲染产物（artifact），写入 renderRevision 与呈现角色
  → timeline-render-items：按源消息对象缓存渲染副本
  → MessageList（完整模式时间线） / ConversationTurn（摘要模式）
```

关键约束：

- **身份稳定**：产物 ID 由 turn 与 item 身份决定；追加新条目不能让已显示条目消失或换 key，否则阶段会重挂载、用户的展开状态会丢失。
- **增量开销与已输出长度无关**：reducer 对流式增量只替换变化的 turn，未变化 turn 的 item 索引直接复用；内容码点长度按 item 对象缓存并由上一版推算，不在每个增量上重新扫描整段内容。
- **投影复用**：只重建发生变化的 turn；`renderRevision` 未变的产物复用同一对象。结构化工具结果的展示文本按结果对象缓存，不在每个增量上重新序列化。
- **终态不可回退**：item 的 `completed`、`failed`、`cancelled`、`indeterminate` 都是终态，迟到的非终态更新不能覆盖它们。

## 呈现角色

投影为每条助手消息写入 `conversationPresentationRole`，两种模式共用：

| 角色 | 判定 | 摘要模式中的位置 |
| --- | --- | --- |
| `final` | 后端标记 `assistantOutputKind` 为 `final` 或 `error` | 最终区，始终可见 |
| `attention` | 待用户处理的审批 | 过程流中单独成块，始终可见 |
| `delegation` | `agent_spawn` 调用 | 合并为代理组 |
| `artifact` | 用户可见产物（文件变更、生成的图片等） | 单独成块，两种模式都默认展开 |
| `process` | 其余文字、思考、工具调用 | 过程流 |

最终回答**只**依据后端的 `assistantOutputKind`。流式中的文字属于过程（模型之后可能继续调用工具），前端不能凭位置、耗时等元数据提前把它当成最终回答，否则它会先出现在最终区再被收回。

## 摘要模式布局

一轮的折叠规则：

- 进行中自动展开过程；结束时，有最终回答才自动收起。
- 没有最终回答的轮次（例如被中断或失败）结束后保持展开，否则收起后什么也看不到。
- 用户手动展开或收起之后，流式更新不再改变这个选择。

过程流（`buildConversationStreamEntries`）：

- 连续的工具调用合并为一个工具组；委派合并为一个代理组；产物和待处理交互单独成块；其余文字成为事件。
- 结果只依赖每个条目之前的内容，所以流式追加只会在末尾增加条目。

阶段（`buildConversationDisclosureBlocks`）：

- 一段文字过程及其后的工具组收敛为一个阶段；新的文字输出意味着下一阶段开始。工具调用不单独提升为阶段。
- 阶段标题是一行预览：Markdown 去掉语法标记，超长截断。能被一行完整表达的短文字展开后不重复；含换行、代码、表格、列表、思考块或超长内容的必须展开后按完整正文渲染。

工具组标签（`resolveConversationToolGroupLabel`）：

- 单个工具：动作 + 目标，例如「读取 a.ts」；缺少目标时用不带占位符的独立文案。
- 浏览器工具按命名空间归类（`browser_read` 是浏览器操作，不是读文件），单个时显示展示名 + 对人有意义的目标：网址、输入内容、检索词、运行时报告的实际点击元素，例如「浏览器点击 提交订单」；不显示 `e:3:12` 这类只对模型有意义的元素引用。
- 多个工具：同类合并计数（「完成了 3 项浏览器操作」），并附加失败数与结果未确认数。

## 工具卡片

卡片状态：

| 状态 | 来源 | 呈现 |
| --- | --- | --- |
| `pending` / `running` | item 未终结 | 进行中 |
| `success` | item `completed` | 成功 |
| `error` | item `failed` / `cancelled` | 失败，展示结构化错误诊断 |
| `unconfirmed` | item `indeterminate` | 结果未确认：写操作已发出但无法确认是否生效（例如 Desktop 连接在动作中断开）。既不是成功也不等同失败，用独立文案与配色提示用户核对页面 |

「结果未确认」是端到端的正式状态：Rust 工具运行时返回 `ExecutionResultStatus::Indeterminate` → canonical item `indeterminate`（终态）→ 前端 `unconfirmed`。前端不从结果文本里嗅探状态。

浏览器卡片：

- 标题旁摘要见上文「工具组标签」；点击、输入、悬停在结果到达后显示运行时报告的实际元素（`target.name`，没有名称时用角色），输入显示为「“内容” → 元素」。
- `browser_read` 展示读到的正文或检索命中的上下文，不展示整段 JSON。
- `browser_screenshot` 展开后直接预览截图。截图保存在会话的浏览器 artifact 目录，通过 `GET /api/browser/artifacts/{sessionId}/{fileName}` 读取（只接受已存在会话、单段文件名，且规范化路径必须落在该会话目录内），个人会话同样可用；点击图片在右栏打开。

## 流式渲染稳定性

- Markdown 正文分为稳定区与流式区，稳定区在流式期间不重新解析，避免已渲染内容闪烁或重挂载。
- 时间线按 turn 身份分组，组的 key 不随条目追加变化。
- 事件流断线或出现缺口时重新同步 bootstrap 快照，reducer 不猜测缺失片段。

## 验证

- `npm --prefix web run test:canonical`：reducer/投影的版本合并、流式增量（含代理对与重叠续传）、索引一致性、`indeterminate` 投影为 `unconfirmed`。
- `npm --prefix web run test:conversation-disclosure`：过程流 key 稳定、阶段与工具组标签、未确认与失败分开计数、浏览器目标。
- `npm --prefix web run test:tool-call-display`：浏览器摘要、截图预览解析、`browser_read` 输出格式化。
