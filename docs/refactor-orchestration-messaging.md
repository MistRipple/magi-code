# 编排消息架构重构设计文档

> 版本: 1.1
> 日期: 2026-02-05
> 状态: 已完成

## 一、问题背景

### 1.1 用户反馈问题

通过实际运行截图和 UX 规范对照，发现以下核心问题：

| 问题编号 | 问题描述 | 严重程度 |
|---------|---------|---------|
| P1 | 主对话区 Worker 卡片显示完整 delegationBriefing，而非简短标题 | 高 |
| P2 | 缺少任务分配宣告消息（"我将安排 X 个 Worker..."） | 高 |
| P3 | Worker Tab 缺少明确的"任务说明"卡片标识 | 中 |
| P4 | 多个 Worker 卡片内容重复 | 中 |
| P5 | Worker Tab 缺少 thinking/tool_call/output 内容 | 高 |

### 1.2 根本原因分析

#### 问题链路一：SubTaskCard 标题错误

```
state-mapper.ts:175
  ↓ title = assignment.delegationBriefing || assignment.responsibility
mission-driven-engine.ts:774
  ↓ 使用 mapped.title（完整的 delegationBriefing）
message-hub.ts:473
  ↓ subTaskCard 用 title 构造卡片内容
UI 显示
  → Worker 卡片显示完整委托说明，而非简短标题
```

**根因**：Assignment 数据模型缺少 `shortTitle` 字段，导致 title 被错误地设置为详细的 delegationBriefing。

#### 问题链路二：任务分配宣告缺失

```
mission-driven-engine.ts:1172
  ↓ 循环调用 sendWorkerDispatchMessage()
mission-driven-engine.ts:561-568
  ↓ 只发送到 Worker Tab（dispatchToWorker: true）
主对话区
  → 没有"我将安排 X 个 Worker..."的汇总消息
```

**根因**：消息发送逻辑缺少向主对话区发送任务分配宣告的步骤。

#### 问题链路三：Worker Tab 内容缺失

```
Worker 执行时
  ↓ adapter 的 streamToUI 流式输出
  ↓ 但 source='worker' 的消息
  → 路由正确但内容未正常产生/传递
```

**根因**：需要进一步排查 Worker 适配器的消息产生逻辑。

---

## 二、UX 规范要求

### 2.1 消息类型完整列表

> 来源：`docs/ux-flow-specification.md` 第 596-610 行

#### 协议层 MessageType 枚举（12 种）

| MessageType | 说明 | 适用场景 |
|-------------|------|----------|
| `TEXT` | 普通文本消息 | 编排者回复、Worker 输出 |
| `PLAN` | 执行计划 | 编排者任务规划 |
| `PROGRESS` | 进度更新 | 阶段进度、执行状态 |
| `RESULT` | 执行结果 | 编排者汇总、Worker 摘要 |
| `ERROR` | 错误消息 | 系统错误、执行失败 |
| `INTERACTION` | 用户交互 | 确认、提问、权限请求 |
| `SYSTEM` | 系统通知 | 系统级通知 |
| `TOOL_CALL` | 工具调用 | 编排者/Worker 工具调用 |
| `THINKING` | 思考过程 | 编排者/Worker 思考 |
| **`USER_INPUT`** | 用户输入 | 用户消息（方案 B 新增） |
| **`TASK_CARD`** | 任务状态卡片 | Worker 执行状态摘要（方案 B 新增） |
| **`INSTRUCTION`** | 任务说明 | 编排者派发给 Worker 的任务（方案 B 新增） |

#### UI 层 MessageCategory 枚举（20 种）

| 消息类型 | MessageType | source | 显示位置 | 用途 |
|----------|-------------|--------|----------|------|
| `USER_INPUT` | USER_INPUT | - | 主对话区 | 用户输入 |
| `ORCHESTRATOR_THINKING` | THINKING | orchestrator | 主对话区 | 编排者思考（可折叠） |
| `ORCHESTRATOR_PLAN` | PLAN | orchestrator | 主对话区 | 任务规划说明 |
| `ORCHESTRATOR_RESPONSE` | TEXT | orchestrator | 主对话区 | 编排者回复/提问 |
| `ORCHESTRATOR_TOOL_USE` | TOOL_CALL | orchestrator | 主对话区 | 编排者工具调用 |
| `ORCHESTRATOR_SUMMARY` | RESULT | orchestrator | 主对话区 | 最终汇总 |
| `TASK_SUMMARY_CARD` | TASK_CARD | orchestrator | 主对话区 | Worker 状态卡片（可点击跳转） |
| `WORKER_INSTRUCTION` | INSTRUCTION | orchestrator | Worker Tab | 任务说明 |
| `WORKER_THINKING` | THINKING | worker | Worker Tab | 思考过程 |
| `WORKER_TOOL_USE` | TOOL_CALL | worker | Worker Tab | Worker 工具调用 |
| `WORKER_CODE` | TEXT | worker | Worker Tab | 代码输出（含 CodeBlock） |
| `WORKER_OUTPUT` | TEXT | worker | Worker Tab | 内容输出 |
| `WORKER_SUMMARY` | RESULT | worker | Worker Tab | 执行摘要 |

### 2.2 双区域职责划分

| 区域           | 职责                   | 允许内容                                                       | 禁止内容                                   |
|----------------|------------------------|----------------------------------------------------------------|--------------------------------------------|
| **主对话区**   | 用户交互 + 编排者叙事  | 用户输入、规划说明、编排者工具调用、Worker 状态卡片、最终汇总  | Worker 思考、Worker 工具调用、Worker 输出  |
| **Worker Tab** | Worker 执行详情        | 任务说明、思考过程、工具调用、内容输出、执行摘要               | 编排者执行细节                             |

> **注意**：编排者具备完整的工具调用能力（读取文件、搜索、分析等），仅不参与具体编码任务。编排者的工具调用显示在主对话区，Worker 的工具调用显示在对应 Worker Tab。

### 2.3 Worker 状态卡片格式

> 来源：`docs/ux-flow-specification.md` 第 63-65 行、219-235 行

```
🟡 Claude [1/3]
分析现有代码结构
执行中...   → 点击跳转
```

组成部分：
- **状态图标**: 🟡（执行中）/ ✅（完成）/ ❌（失败）/ ⏹️（已停止）
- **Worker 名称**: Claude / Gemini / Codex
- **序号（可选）**: `[1/3]` 表示依赖链顺序
- **短标题**: "分析现有代码结构"（简短描述，**非 delegationBriefing**）
- **状态文本**: "执行中..." / "完成 · 发现 3 个问题" / "等待确认..."
- **跳转指示**: → 点击跳转

### 2.4 场景清单

| 场景 | 触发条件 | 关键消息 |
|------|---------|---------|
| 基础流程 | 单 Worker 线性执行 | 全部 11 种消息类型 |
| 场景1 | 多 Worker 并行执行 | 多个 STATUS_CARD + 任务分配宣告 |
| 场景2 | Worker 依赖链执行 | STATUS_CARD 带序号 `[1/3]` |
| 场景3 | Worker 提问 | STATUS_CARD 状态"等待确认" |
| 场景4 | 错误与恢复 | STATUS_CARD 状态"失败" + RESPONSE 列出选项 |
| 场景5 | 用户中断（停止） | STATUS_CARD 状态"已停止" |
| 场景6 | Todo 动态变更 | STATUS_CARD 动态更新 statusText |

---

## 三、统一架构设计

### 3.1 数据模型层

#### Assignment 扩展

```typescript
interface Assignment {
  id: string;
  workerId: WorkerSlot;

  // === 标题与说明（分离职责）===
  shortTitle: string;           // 【新增】简短标题，用于 Worker 卡片（如"分析依赖"）
  responsibility: string;       // 职责描述（内部逻辑用）
  delegationBriefing: string;   // 详细委托说明，显示在 Worker Tab 任务说明区

  // === 状态 ===
  status: AssignmentStatus;
  progress: number;

  // === 其他现有字段 ===
  dependencies: string[];
  todos: WorkerTodo[];
  // ...
}
```

#### SubTaskView 规范化

```typescript
interface SubTaskView {
  id: string;
  worker: WorkerSlot;

  // === 显示内容 ===
  shortTitle: string;           // 简短标题（如"分析依赖"）
  sequenceLabel?: string;       // "[1/3]" 或 null（单任务不显示）

  // === 状态 ===
  status: 'pending' | 'running' | 'waiting_confirmation' | 'completed' | 'failed' | 'stopped' | 'skipped';
  statusText: string;           // "执行中..." / "完成 · 发现 3 个问题" / "等待确认..."

  // === 执行结果 ===
  summary?: string;
  modifiedFiles?: string[];
  createdFiles?: string[];
  duration?: number;
}
```

### 3.2 消息发送层

#### MessageHub API 扩展

| API | 用途 | 目标位置 | 当前状态 |
|-----|------|----------|----------|
| `taskAssignment(workers, briefings)` | **任务分配宣告** | 主对话区 | ❌ 需新增 |
| `workerInstruction(worker, content)` | **任务说明下发** | Worker Tab | ❌ 需新增 |
| `subTaskCard(card)` | Worker 状态卡片 | 主对话区 | ⚠️ 需修正 title |
| `orchestratorMessage()` | 编排者回复/提问 | 主对话区 | ✅ 已有 |
| `result()` | 编排者总结 | 主对话区 | ✅ 已有 |
| `workerOutput()` | Worker 内容输出 | Worker Tab | ✅ 已有 |
| `workerSummary()` | Worker 执行摘要 | Worker Tab | ✅ 已有 |

#### 新增 API 定义

```typescript
/**
 * 发送任务分配宣告（主对话区）
 *
 * 示例输出：
 * "我将安排 2 个 Worker 协作完成：
 *   • Claude: 分析依赖
 *   • Gemini: 优化性能"
 */
taskAssignment(assignments: Array<{
  worker: WorkerSlot;
  shortTitle: string;
}>): void;

/**
 * 发送任务说明到 Worker Tab
 *
 * 显示为带"📋 任务说明"标识的卡片
 */
workerInstruction(worker: WorkerSlot, content: string, metadata?: {
  assignmentId?: string;
  missionId?: string;
}): void;
```

### 3.3 Prompt 生成层

#### 规划 Prompt 输出格式调整

```json
{
  "subTasks": [
    {
      "id": "1",
      "shortTitle": "分析依赖",
      "description": "分析项目依赖结构",
      "assignedWorker": "claude",
      "reason": "复杂分析任务适合 Claude",
      "targetFiles": ["package.json", "tsconfig.json"],
      "dependencies": [],
      "delegationBriefing": "请分析项目的依赖结构，关注 package.json 中的依赖版本兼容性，识别循环依赖问题，并给出优化建议。"
    }
  ]
}
```

**关键变化**：新增 `shortTitle` 字段，与 `delegationBriefing` 分离。

### 3.4 状态映射层

#### state-mapper.ts 修正

```typescript
// 修改前
title: assignment.delegationBriefing || assignment.responsibility,

// 修改后
title: assignment.shortTitle || assignment.responsibility,
```

### 3.5 消息流程层

#### 基础流程消息序列

```
1. 用户输入 → USER_INPUT（已有）

2. 编排者分析 → ORCHESTRATOR_THINKING / ORCHESTRATOR_PLAN
   - streamToUI 流式输出思考过程
   - missionPlanned 事件发送 PlanBlock

3. 任务分配宣告 → 【新增】taskAssignment()
   - 内容："我将安排 X 个 Worker 协作完成：..."
   - 时机：planCollaborationWithLLM 完成后、执行前

4. Worker 开始 → subTaskCard({ status: 'running' })
   - assignmentStarted 事件触发
   - 使用 shortTitle 而非 delegationBriefing

5. 任务说明下发 → 【新增】workerInstruction()
   - 内容：delegationBriefing
   - 显示在 Worker Tab，带"📋 任务说明"标识

6. Worker 执行 → WORKER_THINKING, WORKER_TOOL_USE, WORKER_OUTPUT
   - source: 'worker'
   - 路由到对应 Worker Tab

7. Worker 完成 → subTaskCard({ status: 'completed' })
   - 完成/失败汇报事件触发
   - statusText 显示摘要

8. 编排者总结 → result()
   - 汇总所有 Worker 执行结果
```

---

## 四、具体修改清单

### 第一层：数据模型

| 文件 | 修改内容 | 优先级 |
|------|----------|--------|
| `src/orchestrator/mission/types.ts` | Assignment 新增 `shortTitle: string` 字段 | P0 |

### 第二层：Prompt 生成

| 文件 | 修改内容 | 优先级 |
|------|----------|--------|
| `src/orchestrator/prompts/orchestrator-prompts.ts` | `buildOrchestratorAnalysisPrompt` JSON 格式新增 `shortTitle` | P0 |

### 第三层：状态映射

| 文件 | 修改内容 | 优先级 |
|------|----------|--------|
| `src/orchestrator/mission/state-mapper.ts` | `mapAssignmentToSubTaskView` 使用 `shortTitle` | P0 |

### 第四层：消息发送

| 文件 | 修改内容 | 优先级 |
|------|----------|--------|
| `src/orchestrator/core/message-hub.ts` | 1. 新增 `taskAssignment()` API<br>2. 新增 `workerInstruction()` API<br>3. SubTaskView 接口增加 `statusText` | P0 |
| `src/orchestrator/core/mission-driven-engine.ts` | 1. 执行前调用 `taskAssignment()`<br>2. `sendWorkerDispatchMessage` 改用 `workerInstruction()`<br>3. `emitSubTaskCard` 使用新的 SubTaskView 结构 | P0 |

### 第五层：前端渲染

| 文件 | 修改内容 | 优先级 |
|------|----------|--------|
| `src/ui/webview-svelte/src/lib/message-classifier.ts` | 识别 `WORKER_INSTRUCTION` 类型消息 | P1 |
| Worker Tab 组件 | 渲染"📋 任务说明"卡片样式 | P1 |

---

## 五、场景验证

### 5.1 场景 1: 多 Worker 并行执行

**期望消息流**：

```
用户: 帮我分析前后端依赖问题

编排者: 我将安排两个 Worker 并行分析：      ← taskAssignment() 【新增】
  • Claude 分析前端依赖
  • Gemini 分析后端 API

[🟡 Claude | 分析前端依赖 | 执行中...]       ← subTaskCard() 【修正 title】
[🟡 Gemini | 分析后端 API | 执行中...]

// Worker Tab (Claude)
[📋 任务说明]                                  ← workerInstruction() 【新增】
分析前端项目的依赖关系，找出循环依赖...

[💭 思考] 我需要先读取 package.json...
[🔧 read_file] path: "package.json" ✅
[📝 分析结果] 发现 3 个循环依赖...

[✅ Claude | 分析前端依赖 | 完成 · 发现 3 个循环依赖]
[✅ Gemini | 分析后端 API | 完成 · 5 个端点缺少错误处理]

编排者: 分析完成，共发现以下问题...           ← result()
```

### 5.2 场景 2: Worker 依赖链执行

**期望消息流**：

```
编排者: 这个任务需要分步执行：               ← taskAssignment()
  1. Claude 先分析现有代码
  2. Gemini 基于分析结果设计重构方案
  3. Claude 执行重构

[🟡 Claude [1/3] | 分析现有代码结构 | 执行中...]
        ↓ (完成后自动触发下一步)
[✅ Claude [1/3] | 分析现有代码结构 | 完成 · 识别出 5 个模块]

[🟡 Gemini [2/3] | 设计重构方案 | 执行中...]
        ↓
[✅ Gemini [2/3] | 设计重构方案 | 完成 · 生成 3 种方案]

[🟡 Claude [3/3] | 执行重构 | 执行中...]
```

### 5.3 场景 3: Worker 提问

**期望消息流**：

```
[🟡 Claude | 重构用户模块 | 等待确认...]     ← subTaskCard({ status: 'waiting_confirmation' })

编排者: Claude 遇到决策点：                   ← orchestratorMessage()
  发现两种重构方案：
  A. Repository 模式 - 解耦清晰
  B. Service 直连 - 简单直接
  请告诉我你倾向哪种方案

用户: 用 Repository 模式

[🟡 Claude | 重构用户模块 | 继续执行...]     ← subTaskCard({ status: 'running' })
```

### 5.4 场景 4: 错误恢复

**期望消息流**：

```
[❌ Claude | 修改配置文件 | 失败 · 权限不足]  ← subTaskCard({ status: 'failed' })

编排者: Claude 在修改 config.json 时遇到权限问题。
  你可以：
  • 说"重试"让我再次尝试
  • 说"跳过"继续执行后续任务
  • 说"回滚"撤销已完成的更改
  • 说"停止"终止整个任务

用户: 跳过这个，继续后面的

编排者: 好的，已跳过配置文件修改，继续执行后续任务。

[🟡 Claude | 更新类型定义 | 执行中...]
```

### 5.5 场景 5: 用户中断

**期望消息流**：

```
[🟡 Claude | 重构用户模块 | 执行中...]
[🟡 Gemini | 更新 API 文档 | 执行中...]

// 用户点击停止按钮

[⏹️ Claude | 重构用户模块 | 已停止]          ← subTaskCard({ status: 'stopped' })
[⏹️ Gemini | 更新 API 文档 | 已停止]

编排者: 已停止所有正在执行的任务。
  • Claude: 重构用户模块 - 已完成 2/5 步骤
  • Gemini: 更新 API 文档 - 已完成 1/3 步骤
```

---

## 六、实施计划

### 阶段一：数据模型与 Prompt（P0）

1. 修改 `types.ts` 新增 `shortTitle` 字段
2. 修改 `orchestrator-prompts.ts` Prompt 格式
3. 修改 `state-mapper.ts` 映射逻辑

**验收标准**：Assignment 能正确解析和存储 shortTitle

### 阶段二：消息发送（P0）

1. 修改 `message-hub.ts` 新增 API
2. 修改 `mission-driven-engine.ts` 消息发送逻辑

**验收标准**：
- 主对话区显示任务分配宣告
- Worker 卡片显示 shortTitle
- Worker Tab 显示任务说明

### 阶段三：前端渲染（P1）

1. 修改 `message-classifier.ts` 识别新消息类型
2. 修改 Worker Tab 组件渲染任务说明卡片

**验收标准**：Worker Tab 顶部显示带"📋 任务说明"标识的卡片

### 阶段四：场景测试（P1）

1. 编写 E2E 测试覆盖 6 个场景
2. 手动测试验证 UX 规范符合度

**验收标准**：所有场景的消息流符合 UX 规范

---

## 七、风险与注意事项

### 7.1 向后兼容

- `shortTitle` 字段可设为可选，降级使用 `responsibility`
- 存量 Mission 数据需要兼容处理

### 7.2 LLM 输出稳定性

- Prompt 需要明确要求 `shortTitle` 简短（建议 ≤20 字）
- 需要后处理截断过长的 shortTitle

### 7.3 Worker Tab 内容缺失

- 本次重构主要解决消息架构问题
- Worker 执行内容缺失需单独排查 Worker 适配器

---

## 八、附录

### 8.1 相关文件索引

| 文件路径 | 用途 |
|---------|------|
| `docs/ux-flow-specification.md` | UX 规范定义 |
| `src/orchestrator/mission/types.ts` | Assignment 类型定义 |
| `src/orchestrator/mission/state-mapper.ts` | 状态映射 |
| `src/orchestrator/core/message-hub.ts` | 消息发送 API |
| `src/orchestrator/core/mission-driven-engine.ts` | 编排引擎 |
| `src/orchestrator/prompts/orchestrator-prompts.ts` | Prompt 模板 |
| `src/protocol/message-protocol.ts` | 消息协议定义 |

### 8.2 消息路由规则

```typescript
// 主对话区消息
source === 'orchestrator' && !metadata.dispatchToWorker

// Worker Tab 消息
source === 'worker' || metadata.dispatchToWorker === true
```

### 8.3 状态图标映射

| 状态 | 图标 | 颜色 |
|------|------|------|
| pending | ⬚ | 灰色 |
| running | 🟡 | 黄色 |
| waiting_confirmation | 🟡 | 黄色（带提示） |
| completed | ✅ | 绿色 |
| failed | ❌ | 红色 |
| stopped | ⏹️ | 灰色 |
| skipped | ⏭️ | 灰色 |
