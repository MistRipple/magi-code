# Changelog

## v0.2.0 (2025-07-14)

> 中期迭代版本。聚焦于**工作流稳定性**、**UI 交互体验**和**LLM 行为一致性**三大方向，共 15 项改进。

### ⚡ 工作流与可靠性

- **dispatch_task 批量派发** — 支持 `tasks` 数组一次性下发多个子任务，减少编排轮次 ([10c975d])
- **LLM 连接重试覆盖补全** — `isRetryableError` 补全 `connection|network|fetch failed|socket hang up`，修复 `APIConnectionError` 绕过重试直接失败 ([ef81c58])
- **SSE 流式解析异常拦截** — 流式迭代改为手动 `AsyncIterator`，拦截 SSE 解析异常避免静默中断 ([e212b9d])
- **工具参数解析统一** — `parseToolArguments` 统一流式响应的工具参数解析并处理异常 ([af3ab64])
- **Token 用量完整统计** — 缓存令牌跟踪 + Token 用量报告改进 ([03307b6])

### 🎨 UI 与交互

- **三点动画位置修复** — Worker 面板处理指示器添加 `order: 9999` 防御，确保始终在消息列表底部 ([8721183])
- **计时器按轮次重置** — Worker 面板 `timerStartTime` 锚定 instruction 消息时间戳，不再每次工具调用后归零 ([8721183])
- **工具卡片文件头可点击** — ToolCall 组件重构，文件路径支持点击跳转 ([02f284a])
- **Webview 切换不丢失状态** — 开启 `retainContextWhenHidden`，防止标签页切换导致 UI 状态重置 ([2cb9a61])
- **自检结果不再暴露到 UI** — Worker 验收检查改用 `sendSilentMessage` 静默调用，`{ "allSatisfied": true, "gaps": [] }` 等内部 JSON 不再显示 ([fcbe11a])

### 🧠 LLM 行为与 Prompt

- **动态语言跟随** — 编排者 + Worker prompt 不再硬编码"用中文回复"，改为跟随用户输入语言。用户全局规则中的语言要求拥有最高优先级 ([a006235])
- **禁止输出推理过程** — 禁止 LLM 在主文本中输出 "Let me..."、"The user wants..." 等内部推理，直接给出结论 ([a006235])

### 🔧 工程改进

- **launch-process 双轨策略** — 工具描述去除"禁止用于读写文件"一刀切限制，允许 `sed/python/node` 脚本批量编辑，单文件精确编辑仍优先 `file_edit` ([409be5c])
- **终端 Split-Brain 消除** — `launchProcess` 前强制落盘脏文档，消除文件系统与编辑器内容不一致 ([20f29a4])
- **Todo/工具网关完善** — `get_todos`、`update_todo` 注册到工具网关白名单；`todo.content` 为 undefined 时防御性处理 ([4273ecd], [130ec44])

### 🏗️ 架构

- **sendSilentMessage 机制** — `IAdapterFactory` 接口 + `WorkerLLMAdapter` 新增静默消息方法，底层 client 非流式调用，不触发 UI 推送，对话历史正常更新。为未来需要"后端内部 LLM 调用不暴露到前端"的场景提供统一基础设施 ([fcbe11a])

---

## v0.1.6 (2025-07-10)

- 编排合同链路完整落地 + 终端空闲超时/kill 修复
- 调度完成队列与 MDE 辅助逻辑拆分

## v0.1.4 (2025-07-06)

- 工具重复调用修复与 UI 排序优化

## v0.1.3 (2025-07-04)

- Worker 记忆修复与端到端验证

## v0.1.1 (2025-07-01)

- L3 统一架构重构 + 编排防阻塞 + 质量门禁修复

