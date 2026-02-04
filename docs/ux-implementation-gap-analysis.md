# UX/UI 实现差距分析对照表

> 基于 `ux-flow-specification.md` 规范，对照当前代码实现，梳理需要完善的功能点

## 一、消息类型与路由

### 1.1 消息类型枚举对照

| 规范定义 | 当前实现 | 状态 | 说明 |
|---------|---------|------|------|
| `USER_INPUT` | `MessageCategory.USER_INPUT` | ✅ 已实现 | 路由到 thread |
| `ORCHESTRATOR_THINKING` | `MessageCategory.ORCHESTRATOR_THINKING` | ✅ 已实现 | 路由到 thread |
| `ORCHESTRATOR_PLAN` | `MessageCategory.ORCHESTRATOR_PLAN` | ✅ 已实现 | 路由到 thread |
| `ORCHESTRATOR_RESPONSE` | `MessageCategory.ORCHESTRATOR_ANALYSIS` | ⚠️ 命名不一致 | 功能等价 |
| `ORCHESTRATOR_SUMMARY` | `MessageCategory.ORCHESTRATOR_SUMMARY` | ✅ 已实现 | 路由到 thread |
| `WORKER_STATUS_CARD` | `MessageCategory.TASK_SUMMARY_CARD` | ⚠️ 命名不一致 | 功能等价，使用 SubTaskSummaryCard |
| `WORKER_INSTRUCTION` | `MessageCategory.WORKER_INSTRUCTION` | ✅ 已实现 | 路由到 worker |
| `WORKER_THINKING` | `MessageCategory.WORKER_THINKING` | ✅ 已实现 | 路由到 worker |
| `WORKER_TOOL_USE` | `MessageCategory.WORKER_TOOL_USE` | ✅ 已实现 | 路由到 worker |
| `WORKER_OUTPUT` | `MessageCategory.WORKER_OUTPUT` | ✅ 已实现 | 路由到 worker |
| `WORKER_SUMMARY` | - | ❌ 缺失 | 需要新增，Worker 执行摘要 |

### 1.2 路由规则对照

| 规范规则 | 当前实现 | 状态 |
|---------|---------|------|
| 主对话区只接受编排者叙事 + Worker 节点卡片 | `routing-table.ts` 已配置 | ✅ 已实现 |
| Worker Tab 只接受 Worker 执行细节 | `routing-table.ts` 已配置 | ✅ 已实现 |
| Worker 节点卡片必须由编排者生成 | `hasSummaryCard` 检测 + 路由 | ✅ 已实现 |

---

## 二、UI 组件实现

### 2.1 主对话区组件

| 规范要求 | 当前组件 | 状态 | 说明 |
|---------|---------|------|------|
| Worker 状态卡片（可点击跳转） | `SubTaskSummaryCard.svelte` | ✅ 已实现 | 点击跳转到对应 Tab |
| 卡片状态图标（🟡✅❌⏹️） | `SubTaskSummaryCard` status 渲染 | ⚠️ 部分实现 | 需核实所有状态图标 |
| 每个 Worker 独立卡片上下布局 | MessageList 渲染 | ✅ 已实现 | 按消息顺序渲染 |

### 2.2 Worker Tab 组件

| 规范要求 | 当前组件 | 状态 | 说明 |
|---------|---------|------|------|
| 任务说明卡片 | - | ⚠️ 样式待优化 | 有 WORKER_INSTRUCTION 分类，但无专用样式 |
| 思考过程（可折叠） | `BlockRenderer` + `ThinkingBlock` | ✅ 已实现 | |
| 工具调用卡片 | `ToolCallBlock` | ✅ 已实现 | 可展开查看详情 |
| 内容输出 | `MarkdownContent` | ✅ 已实现 | |
| 执行摘要 | - | ❌ 缺失 | 需要新增 WORKER_SUMMARY 渲染 |

### 2.3 输入区域

| 规范要求 | 当前实现 | 状态 | 说明 |
|---------|---------|------|------|
| 发送按钮 | `InputArea.svelte` 发送按钮 | ✅ 已实现 | |
| 停止按钮（执行中） | `InputArea.svelte` stop 按钮 | ✅ 已实现 | `isSending` 切换 |
| 执行中可输入 | textarea `disabled={isSending}` | ❌ 不符合 | 规范要求执行中仍可输入 |
| 按钮双态切换 | 当前：isSending 切换 | ⚠️ 逻辑不符合 | 规范：有内容=发送，无内容=停止 |
| 限频机制 | - | ❌ 缺失 | 需实现 1s/条（执行中）、300ms/条（空闲） |

---

## 三、交互流程

### 3.1 补充指令（执行中发送消息）

| 规范要求 | 当前实现 | 状态 | 说明 |
|---------|---------|------|------|
| 执行中可发送补充指令 | 当前禁用输入 | ❌ 不符合 | 需修改 textarea disabled 逻辑 |
| 补充指令不中断当前任务 | - | ❌ 未实现 | 需后端支持 appendMessage |
| 在下一决策点生效 | - | ❌ 未实现 | 需编排者逻辑支持 |
| 按钮双态 | - | ❌ 未实现 | 需根据输入内容切换 |

### 3.2 停止功能

| 规范要求 | 当前实现 | 状态 | 说明 |
|---------|---------|------|------|
| 点击停止立即停止所有 Worker | `interruptTask` 消息 | ✅ 已实现 | |
| 卡片状态更新为 ⏹️ | - | ⚠️ 待验证 | 需核实状态更新逻辑 |
| 编排者汇报进度 | `interruptCurrentTask` 发送消息 | ✅ 已实现 | |

### 3.3 限频机制

| 规范要求 | 当前实现 | 状态 | 说明 |
|---------|---------|------|------|
| 空闲状态 300ms/条 | - | ❌ 缺失 | 需实现 |
| 执行中 1s/条 | - | ❌ 缺失 | 需实现 |
| 超频提示 | - | ❌ 缺失 | 需实现 |

---

## 四、Worker 状态管理

### 4.1 执行状态

| 规范状态 | 当前实现 | 状态 |
|---------|---------|------|
| 待执行 ⬚ | `workerExecutionStatus: 'idle'` | ⚠️ 图标待核实 |
| 执行中 🟡 | `workerExecutionStatus: 'executing'` | ✅ 已实现 |
| 已完成 ✅ | `workerExecutionStatus: 'completed'` | ✅ 已实现 |
| 已跳过 ⏭️ | - | ❌ 缺失 |
| 失败 ❌ | `workerExecutionStatus: 'failed'` | ✅ 已实现 |
| 已停止 ⏹️ | - | ❌ 缺失 | 需新增 'stopped' 状态 |

### 4.2 状态卡片更新

| 规范要求 | 当前实现 | 状态 |
|---------|---------|------|
| 编排者统一控制卡片状态 | `SubTaskSummaryCard` 接收状态 | ✅ 已实现 |
| Worker 只上报结果 | Worker 发送 RESULT | ✅ 已实现 |
| 状态实时更新 | - | ⚠️ 待验证 | 需核实状态同步机制 |

---

## 五、优先级排序

### P0 - 必须修复（阻塞核心流程）

| 编号 | 问题 | 涉及文件 |
|------|------|---------|
| P0-1 | 执行中输入框被禁用，不符合"持续可交互"原则 | `InputArea.svelte` |
| P0-2 | 按钮双态逻辑不符合规范 | `InputArea.svelte` |

### P1 - 重要优化

| 编号 | 问题 | 涉及文件 |
|------|------|---------|
| P1-1 | 限频机制未实现 | `InputArea.svelte` |
| P1-2 | WORKER_SUMMARY 消息类型缺失 | `message-routing.ts`, `routing-table.ts` |
| P1-3 | 补充指令后端处理逻辑 | `webview-provider.ts` |

### P2 - 体验增强

| 编号 | 问题 | 涉及文件 |
|------|------|---------|
| P2-1 | Worker 状态缺少 'stopped'/'skipped' | `messages.svelte.ts` |
| P2-2 | 任务说明卡片样式优化 | 新增组件或样式 |
| P2-3 | 状态图标完整性 | `SubTaskSummaryCard.svelte` |

---

## 六、实现计划

### 阶段 1：核心交互修复（P0）

1. 修改 `InputArea.svelte`：
   - 移除 `disabled={isSending}` 限制
   - 实现按钮双态：有内容=发送，无内容=停止
   - 执行中发送消息调用 `appendMessage`

### 阶段 2：限频与消息类型（P1）

2. 实现限频机制（InputArea 层面）
3. 新增 `WORKER_SUMMARY` 消息类型
4. 完善 `appendMessage` 后端处理

### 阶段 3：体验优化（P2）

5. 新增 Worker 状态类型
6. 优化任务说明卡片样式
7. 完善状态图标

