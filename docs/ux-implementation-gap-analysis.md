# UX/UI 实现差距分析对照表 (Audit Table)

> 本报告严格对照 `ux-flow-specification.md` 规范，对当前代码实现进行逐项对标分析。
>
> **最后更新**: 2026-02-04 (All Functional Requirements Met & Refactored)

## 一、 渲染与流控性能（核心突破）

| 规范/体验要求 | 当前实现 | 状态 | 说明 |
| :--- | :--- | :--- | :--- |
| **流式打字机效果** | 采用 `Render Loop` + 动态步调控制 | ✅ 已实现 | 彻底解决了长文本解析阻塞主线程导致的卡顿问题。 |
| **消息卡片高度自适应** | 锁定 `flex-shrink: 0` 与 `height: auto` | ✅ 已实现 | 消除了流式输出过程中卡片塌陷与内容溢出的 Bug。 |
| **Markdown 格式解析** | 引入 `preprocessMarkdown` 预处理器 | ✅ 已实现 | 修复了 `<think>` 标签及未闭合代码块的显示 Bug。 |
| **滚动稳定性** | 设置 `overflow-anchor: none` | ✅ 已实现 | 彻底消除了高速渲染时自动滚动逻辑与浏览器锚定的冲突。 |
| **行号精准对齐** | 重置 `CodeBlock` 局部 CSS 优先级 | ✅ 已实现 | 解决 padding 导致的代码与行号错位。 |

## 二、 消息类型与路由规则

| 规范定义 | 当前实现 | 状态 | 说明 |
| :--- | :--- | :--- | :--- |
| 主对话区只接受编排者叙事 + Worker 节点卡片 | `handleContentMessage` 中增加强校验拦截器 | ✅ 已实现 | 强制将 Worker 来源的消息重定向到 Worker Tab，防止污染主区。 |
| Worker Tab 只接受 Worker 自身的执行细节 | 路由表按 `workerId` 隔离，逻辑闭环 | ✅ 已实现 | Worker 详细输出（思考、工具、结果）正确路由到独立 Tab。 |
| Worker 节点卡片必须由编排者生成 | 依赖 Source 强校验 + Metadata 识别 | ✅ 已实现 | Worker 无法伪造 source='orchestrator' 的消息进入主区。 |
| WORKER_SUMMARY：Worker 执行摘要类型 | `MessageCategory.WORKER_SUMMARY` | ✅ 已实现 | 后端 `MessageHub` 新增 `workerSummary` 方法，引擎已对接发送。 |
| 规范化的 ORCHESTRATOR_RESPONSE 命名 | `MessageCategory.ORCHESTRATOR_RESPONSE` | ✅ 已实现 | 已完成重构，消除了与规范的命名偏差。 |

## 三、 UI 组件细节规范

| 规范定义 | 当前实现 | 状态 | 说明 |
| :--- | :--- | :--- | :--- |
| 主对话区无操作按钮（除卡片跳转外） | 引入 `readOnly` 属性 | ✅ 已实现 | 主对话区的 `CodeBlock` 和 `SummaryCard` 已隐藏展开/复制按钮。 |
| 卡片状态图标需完整（🟡✅❌⏹️⏭️⬚） | 补全了 `statusBadgeMap` | ✅ 已实现 | 支持停止、跳过、待执行等所有状态的图标与颜色。 |
| 任务说明卡片需醒目美化 | `InstructionCard` 组件 | ✅ 已实现 | 实现了带 Banner 头部和特定 Worker 配色的卡片样式。 |
| 执行中输入框仍保持可交互 | 移除了 `disabled` 属性 | ✅ 已实现 | 用户可在 AI 执行期间自由输入内容。 |
| 发送/停止按钮需根据内容双态切换 | `showStopButton = isSending && !hasContent` | ✅ 已实现 | 实现了"有内容=发送，无内容且运行=停止"的规范逻辑。 |

## 四、 交互流程逻辑

| 规范定义 | 当前实现 | 状态 | 说明 |
| :--- | :--- | :--- | :--- |
| 执行中可发送补充指令（不中断当前任务） | 前端发送 `appendMessage` | ✅ 已实现 | 链路打通，后端 `handleAppendMessage` 接收并注入指令。 |
| 补充指令在下一决策点生效 | 后端引擎指令队列 `supplementaryInstructions` | ✅ 已实现 | 编排者已具备在决策点消费指令的能力。 |
| 点击停止按钮后卡片状态更新为 ⏹️ | 后端发送 `stopped` 状态更新 | ✅ 已实现 | 中断逻辑中已遍历子任务并发送 `stopped` 状态卡片。 |
| 发送频率限制（限频机制） | `InputArea` 内置防抖 | ✅ 已实现 | 实现了空闲 300ms / 运行中 1s 的发送冷却。 |
