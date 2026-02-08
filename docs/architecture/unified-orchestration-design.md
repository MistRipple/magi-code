# MultiCLI 统一编排架构设计方案

## 一、产品定位

**MultiCLI** 是一个 VSCode 插件，编排多个专业 AI（Claude / Codex / Gemini）协作完成复杂开发任务。

**核心价值**：用户像和一个全能编程助手对话，背后由最合适的 AI 自动执行。

**体验目标**：

- 用户发一句话 → 立即开始流式响应
- 简单问题 1 次 LLM 调用完成
- 复杂任务由 orchestrator 自主决定是否拆分和分配
- 全过程可追踪，不存在不可见的执行环节

---

## 二、核心原则

1. **一次调用即响应**：第一次 LLM 调用就开始产出用户可见内容
2. **工具即能力**：所有能力（包括分配 Worker）通过工具定义暴露给 LLM
3. **LLM 自主判断**：Layer 2（dispatch_task 路径）的意图判断融入系统提示词，由 orchestrator LLM 自主通过工具选择表达决策；Layer 3（plan_mission 路径）使用独立的 IntentGate 前置分类器决定处理模式（ASK/DIRECT/EXPLORE/TASK/DEMO/CLARIFY），两条路径并行存在
4. **按需升级**：简单任务直接完成，复杂任务自然升级为 Worker 协作
5. **全过程可追踪**：Worker 推理过程在 Worker 面板实时展示，关键节点回传主对话区，不存在不可见的任务
6. **统一汇总**：只要使用了 Worker，全部完成后 orchestrator 自动生成最终结论

---

## 三、三层执行模型

```
用户输入
  → orchestrator LLM（统一系统提示词 + 全量工具 + 上下文）
  │
  ├── Layer 1：orchestrator 自主完成（不调用编排工具）
  │   → 直接回答 / 调用内置工具（读文件、搜索、执行命令、网络搜索等）→ 流式输出 → 完成
  │   适用：问答、文件操作、搜索、命令执行等 orchestrator 自身能处理的任务
  │   LLM 调用：1 次（可含多轮工具循环）
  │
  ├── Layer 2：单 Worker 委派（调用 dispatch_task）
  │   → Worker 独立执行（Worker 面板实时展示） → 进度回传 → Worker 完成
  │   → orchestrator 汇总 LLM 调用 → 最终结论
  │   适用：单领域多步修改
  │   orchestrator LLM 调用：1 次决策 + 1 次汇总（+ 可能的中间调用，见下文）
  │   不需要 Mission
  │
  └── Layer 3：多 Worker 协作（调用 plan_mission 或多次 dispatch_task）
      → Worker(s) 独立执行（各自 Worker 面板实时展示） → 全部完成
      → orchestrator 汇总 LLM 调用 → 整合各 Worker 结果 → 最终结论
      适用：跨领域/大规模/高风险任务
      orchestrator LLM 调用：1 次决策 + 1 次汇总（+ 可能的中间调用）
      plan_mission 场景额外包含 MissionOrchestrator 内部的规划/验证 LLM 调用
      plan_mission 场景需要 Mission + Contract + 用户确认
```

**三层的区分标准是是否使用了编排工具（dispatch_task / plan_mission），而非是否使用了工具。** Layer 1 中 orchestrator 可以自由使用所有内置工具，只要不涉及 Worker 委派就属于 Layer 1。

**动态升级**：Layer 之间不是静态分配，orchestrator 可以根据执行过程中的实际情况动态升级：

- **Layer 1 → 2/3**：orchestrator 在工具循环中发现任务超出自身处理范围（如执行命令后发现大量错误），自主判断是否需要 Worker 介入。若用户原始意图已隐含修改需求（如"修复这个 bug"），orchestrator 可直接调用 dispatch_task 升级；若用户意图不明确（如"帮我跑一下测试"），则先向用户反馈发现并建议交由 Worker 处理
- **Layer 2 → 3**：单 Worker 执行过程中发现任务涉及其他领域（如修后端时发现前端也需要改），立即上报 orchestrator，orchestrator 马上追加 dispatch 其他 Worker 并行推进，不等当前 Worker 完成。DispatchBatch 支持动态追加，只要 batch 中还有未完成的 Worker，Phase C 汇总就不触发

**统一规则：只要使用了 Worker，全部 Worker 完成后 orchestrator 都会触发一次汇总 LLM 调用，生成面向用户的最终结论。** 不区分单/多 Worker，保证用户体验一致。

### 场景示例

| 场景 | 执行层 | orchestrator LLM 调用次数 |
|:---|:---|:---|
| "你能做什么" | Layer 1 | 1 次直接回答 |
| "帮我读 src/types.ts" | Layer 1 | 1 次 + 工具循环（read_file） |
| "搜索项目中所有 TODO 注释" | Layer 1 | 1 次 + 工具循环（search） |
| "运行 npm test 看看结果" | Layer 1 | 1 次 + 工具循环（execute_command） |
| "运行 npm test" → 发现大量失败 | Layer 1 → 2 | 1 次（执行+发现+反馈用户） + 1 次（用户确认后 dispatch） + 1 次汇总 = 3 次 |
| "修复这个 bug"（orchestrator 直接 dispatch） | Layer 1 → 2 | 1 次（决策+dispatch） + 1 次汇总 = 2 次 |
| "修复这个 bug" → Worker 发现前端也要改 | Layer 2 → 3 | 1 次决策 + 1 次中间调用（追加 dispatch） + 1 次汇总 = 3 次 |
| "修复这个 bug" | Layer 2 | 1 次决策 + 1 次汇总 = 2 次 |
| "实现用户登录功能（前后端）" | Layer 3 | 1 次决策 + 1 次汇总 = 2 次（+ Worker 内部 LLM） |

**LLM 调用次数说明**：上表仅计入 orchestrator 的 LLM 调用。Worker 内部的 LLM 调用（推理循环）不计入此统计。当 Worker 上报触发中间 LLM 调用时，每次上报计 1 次。plan_mission 路径还包含 MissionOrchestrator 内部的规划/契约/验证 LLM 调用，不在此表统计。

---

## 四、三个编排工具

### dispatch_task

将子任务分配给专业 Worker 执行。适用于需要多步代码操作、多文件修改或专业领域知识的任务。

| 参数 | 类型 | 必填 | 说明 |
|:---|:---|:---|:---|
| worker | string（动态枚举） | 是 | 目标 Worker。可选值和能力描述从 Worker 分工配置（ProfileLoader + WorkerAssignments）动态获取，不写死 |
| task | string | 是 | 清晰、完整的任务描述 |
| files | string[] | 否 | 涉及的关键文件路径 |
| depends_on | string[] | 否 | 依赖的前序任务 ID 列表。设置后该 Worker 将等待所有前序 Worker 完成并获取其产出后再启动 |

**返回值**：dispatch_task 立即返回（非阻塞），返回结构：

| 字段 | 类型 | 说明 |
|:---|:---|:---|
| task_id | string | 唯一任务 ID，可用于其他 dispatch_task 的 depends_on 引用 |
| status | string | 固定为 "dispatched" |
| worker | string | 实际分配的 Worker |

**worker 参数动态生成**：dispatch_task 的 worker enum 值和描述文案由系统在注册工具时从 ProfileLoader 读取当前生效的分工配置动态构建。用户通过 `~/.multicli/worker-assignments.json` 调整分工后，工具描述自动更新，无需修改代码。

### plan_mission

为复杂的多步骤任务创建协作执行计划。适用于需要多个 Worker 协作、涉及架构变更、或需要用户审批的重大任务。

| 参数 | 类型 | 必填 | 说明 |
|:---|:---|:---|:---|
| goal | string | 是 | 任务目标的完整描述 |
| constraints | string[] | 否 | 约束条件 |
| workers | string[]（动态枚举） | 否 | 建议参与的 Worker，可选值从当前可用 Worker 配置动态获取 |

plan_mission 内部调用 MissionOrchestrator 完整流程：创建 Mission → 规划 → 定义契约 → 分配职责 → 用户确认 → 执行 → 验证。完全复用 Mission 系统，零功能损失。

### send_worker_message

向指定 Worker 的面板发送消息。用于传递补充上下文、调整指令或协作信息。

| 参数 | 类型 | 必填 | 说明 |
|:---|:---|:---|:---|
| worker | string（动态枚举） | 是 | 目标 Worker，可选值从当前可用 Worker 配置动态获取 |
| message | string | 是 | 要发送的消息内容 |

### 工具选择决策规则

| 优先级 | 场景 | 行动 |
|:---|:---|:---|
| 1 | 能直接回答的问题（问候、知识问答、代码解释） | 直接回答，不调用工具 |
| 2 | 能用工具一步解决的（读文件、搜索、运行命令） | 调用对应内置工具 |
| 3 | 多步操作但单一领域（修复 bug、添加函数） | dispatch_task |
| 4 | 跨领域/大规模/高风险（完整功能、重构模块） | plan_mission |

**判断标准**：

- 涉及 1-2 个文件 → dispatch_task
- 涉及 3+ 个文件或多模块 → 评估 plan_mission
- 涉及架构变更或多技术栈 → plan_mission
- 用户明确要求"规划"或"制定计划" → plan_mission

---

## 五、dispatch_task 非阻塞执行模型

### 三段式执行

```
Phase A: orchestrator 决策（1 次 LLM 调用）
  → 流式输出分析说明（用户立即看到思考过程）
  → 调用 dispatch_task({worker: "codex", task: "..."})
  → dispatch_task 立即返回 "已分配给 codex Worker"
  → orchestrator 输出过渡文本 "正在执行..."
  → orchestrator LLM 本轮结束

Phase B: Worker 独立执行（全过程可追踪）
  → Worker 在对应的 Worker 面板中实时展示推理过程（thinking / 工具调用 / 输出）
  → 关键节点自动回传主对话区的 subTaskCard（文件变更、步骤完成、遇到问题）
  → 前端 TasksPanel 实时显示子任务卡片和 Todo 进度
  → Worker 完成后，主动将结果摘要发送到主对话区（Worker 身份）

Phase C: orchestrator 汇总（1 次 LLM 调用）
  → 全部 Worker 完成后自动触发
  → 输入：用户原始需求 + 各 Worker 的执行结果（成功/失败、摘要、修改文件列表）
  → orchestrator LLM 生成面向用户的最终结论
  → 流式输出到主对话区（orchestrator 身份）
```

### 多 Worker 协作场景

**并行（无依赖）**：

```
[Phase A] dispatch_task(codex, "修复后端 API 错误处理") → 立即启动
[Phase A] dispatch_task(gemini, "修复前端表单校验") → 立即启动
→ orchestrator 输出 "已安排 codex 和 gemini 并行处理..."

[Phase B] codex 独立执行（Worker 面板可见） ─┐
[Phase B] gemini 独立执行（Worker 面板可见） ─┤→ 全部完成
                                              │
[Phase C] orchestrator 汇总 LLM 调用 → 最终结论
```

**串行（有依赖）**：

```text
[Phase A] dispatch_task(codex, "实现 /api/auth 接口") → task-1，立即启动
[Phase A] dispatch_task(gemini, "基于 API 实现登录页面", depends_on: "task-1") → 等待
→ orchestrator 输出 "已安排 codex 先实现 API，gemini 随后基于 API 做前端..."

[Phase B] codex 执行 → 完成 → 产出写入 SharedContextPool
[Phase B] gemini 自动启动（读取 codex 产出）→ 执行 → 完成

[Phase C] 全部完成 → orchestrator 汇总 LLM 调用 → 最终结论
```

### 为什么需要 Phase C（统一汇总）？

- **Worker summary 质量不可控**：直接执行模式返回 LLM 原始长文，Todo 模式返回"完成 N 个任务"这种无语义统计
- **对话角色一致性**：用户和 orchestrator 对话，最终结论应该从 orchestrator 出来
- **多 Worker 整合需求**：各 Worker 各自汇报结果，需要有人做整合归纳
- **统一规则，实现简单**：不区分单/多 Worker，流程一致

### DispatchBatch 追踪机制

**生命周期**：

1. **创建**：orchestrator 在 Phase A 中首次调用 dispatch_task 时，按需创建 DispatchBatch（Layer 1 不创建，避免空对象）
2. **注册**：每次 dispatch_task 调用时，将任务（task_id、worker、depends_on、Promise）注册到当前 batch
3. **动态追加**：Phase B+ 中间调用产出的新 dispatch 追加到同一 batch
4. **完成检测**：每个 Worker 完成/失败时更新对应任务状态，检查后续依赖
5. **触发汇总**：batch 中所有任务都已完成或失败时，自动触发 Phase C
6. **归档**：Phase C 完成后，DispatchBatch 标记为 archived，释放 Promise 引用

**依赖链与失败处理**：

- 每个 Worker 完成时，标记对应任务完成，检查是否有依赖它的后续 Worker 需要启动
- **前序 Worker 失败**：DispatchBatch 自动将依赖它的所有后续 Worker 标记为 `skipped`（级联跳过），不启动这些 Worker。Phase C 汇总中明确报告失败链路和被跳过的任务
- **部分失败**：只要 batch 中还有正在执行的 Worker，Phase C 不触发。所有 Worker 均已完成或失败或跳过时，触发 Phase C
- **全部失败**：Phase C 仍然触发，汇总 LLM 报告所有失败原因，用户可感知

### Worker 并行/串行执行策略

orchestrator 通过 dispatch_task 的 `depends_on` 参数声明任务间的依赖关系，DispatchBatch 据此自动编排执行顺序：

- **无 depends_on**：立即启动，与其他 Worker 并行执行
- **有 depends_on**：等待前序 Worker 完成，获取其产出（通过 SharedContextPool）后再启动

orchestrator 在 Phase A 决策时自主判断是否存在依赖：

| 场景 | 依赖关系 | 执行方式 |
|:---|:---|:---|
| codex 修后端 + gemini 做前端（互不依赖） | 无 | 并行 |
| codex 先实现 API → gemini 基于 API 做页面 | gemini depends_on codex | 串行 |
| claude 做架构设计 → codex + gemini 分别实现 | codex、gemini 均 depends_on claude | 先串行后并行 |
| Worker A 执行中发现需要 Worker B 协助 | 无（动态追加） | 并行 |

### 汇总 LLM 调用设计

- 系统提示词：精简的汇总指令（"根据 Worker 执行结果，为用户生成简洁的任务完成总结"）
- 输入：用户原始需求 + Worker 结果结构化数据（成功/失败、摘要、修改文件列表）
- 输出：1-3 句话概括完成情况 + 关键修改内容 + 文件列表 + 失败原因（如有）
- 预估开销：单 Worker ~800 tokens，多 Worker ~1200 tokens

---

## 六、跨面板通信设计

### 通道 1：orchestrator ↔ Worker（双向通信）

**orchestrator → Worker**：orchestrator 通过 send_worker_message 工具向 Worker 面板发送消息（上下文补充、指令调整、协调指令等）。消息在 Worker 面板中以编排者指令的特殊样式展示，区别于 Worker 自己的输出。

**Worker → orchestrator**：Worker 在执行过程中可以通过 `workerReport` 事件主动向 orchestrator 发送上报消息，用于：

- 上报阻塞问题（需要 orchestrator 决策或协调其他 Worker）
- 请求补充上下文（缺少必要信息无法继续）
- 汇报关键发现（影响整体任务方向的信息）

**上报实现机制**：Worker 通过 AutonomousWorker 内置的 `reportToOrchestrator(type, content)` 方法触发上报。`type` 分为 `question`（阻塞/请求上下文，暂停等待 orchestrator 响应）和 `progress`（进度信息，不暂停）。上报消息通过 `onReport` 回调传递给 MissionDrivenEngine，由引擎决定是否触发中间 LLM 调用。Worker 完成/失败时通过 `completed` / `failed` 类型自动上报，由 DispatchBatch 状态机处理。

Worker 发给 orchestrator 的上报消息会触发 orchestrator 的中间响应处理（见下文 Phase B+ 机制），orchestrator 可以据此通过 send_worker_message 下发调整指令，或通过主对话区告知用户。

**Phase B+ 中间响应机制**：Phase A 结束后 orchestrator LLM 已退出本轮调用。当 Worker 通过 `workerReport` 事件上报时，MissionDrivenEngine 触发一次新的 orchestrator LLM 调用（中间调用，记为 Phase B+）。Phase B+ 不属于 Phase A 也不属于 Phase C，是 Phase B 期间的附加响应步骤。

中间调用的设计：

- **触发条件**：仅 `question` 类型触发中间 LLM 调用（对应需要 orchestrator 决策的阻塞问题或上下文请求）；`progress` 类型仅更新 subTaskCard UI，不触发 LLM；`completed` / `failed` 类型由 DispatchBatch 状态机处理
- **频率限制**：同一 DispatchBatch 内中间调用最小间隔 30 秒，防止频繁上报导致 LLM 调用风暴
- **上下文**：精简上下文 = 原始用户需求 + Worker 上报内容 + 当前 DispatchBatch 状态（各 Worker 进度），预估 ~600-800 tokens
- **产出**：orchestrator 据此决策：追加 dispatch 新 Worker、通过 send_worker_message 下发调整指令、或在主对话区向用户反馈
- **DispatchBatch 联动**：中间调用产出的新 dispatch 同样注册到当前 DispatchBatch，不影响最终 Phase C 汇总触发条件
- **并发上报**：多个 Worker 同时上报时排队串行处理，避免竞态

### 通道 2：Worker → 主对话区（关键节点回传）

Worker 执行过程中，以下关键节点的摘要自动同步到主对话区：

| 节点 | 消息内容 | 展示方式 |
|:---|:---|:---|
| 开始执行 | "开始处理: {任务摘要}" | subTaskCard 更新 |
| 文件变更 | "已修改: {filePath}" | subTaskCard 的 changes 字段更新 |
| 步骤完成 | "完成第 N 步: {摘要}" | subTaskCard 更新 |
| 遇到问题 | "遇到问题: {描述}" | subTaskCard 更新 + 状态变更 |
| 任务完成 | 结果摘要 + 修改文件列表 | **新消息**发送到主对话区（Worker 身份） |

复用现有的 subTaskCard 更新机制，不需要新增消息类型。

### 通道 3：Worker → Worker（间接协作）

Worker 之间**不直接发送消息**，所有协作通过以下两种机制完成：

- **前置约定（Contract）**：plan_mission 阶段通过契约预先定义好各 Worker 的接口边界和产出格式，避免运行时协调
- **共享上下文池（SharedContextPool）**：先完成的 Worker 将关键产出写入共享池，后续 Worker 启动时读取。复用 insightGenerated 事件机制

**SharedContextPool 数据流**：

- **写入主体**：Worker 在 `executeAssignment` 完成后，由 AutonomousWorker 自动调用 `writeInsights()` 将关键产出写入池
- **写入内容**：修改的文件路径列表、新增的接口定义、关键数据结构、执行摘要。每条记录带 task_id 和时间戳
- **读取时机**：DispatchBatch 检测到前序 Worker 完成后启动后续 Worker 时，自动调用 `assembleSharedContext(dependsOnTaskId)` 组装前序 Worker 的产出，注入到后续 Worker 的执行上下文
- **筛选机制**：按 `depends_on` 指定的 task_id 精确筛选前序 Worker 的产出，不读取无关 Worker 的数据
- **并发安全**：当前 Node.js 单线程模型下，SharedContextPool 的 Map 操作是原子的，无竞态风险。若未来引入 Worker 子进程，需要增加写入锁

**为什么不支持 Worker 间直接通信？**

- Worker 需要感知其他 Worker 的存在和状态，增加系统复杂度
- Worker 的 LLM 上下文需要注入对方信息，增加 Token 消耗
- 用户在 UI 上难以追踪谁在跟谁通信
- 违反"orchestrator 统一调度"原则，容易产生不可控的分布式协商

**运行时需要协调怎么办？** Worker 通过 `workerReport` 事件向 orchestrator 上报问题（`blocker` 类型），触发 Phase B+ 中间调用。orchestrator 决策后通过 send_worker_message 向相关 Worker 下发调整指令。协调决策始终由 orchestrator 做出，Worker 只负责执行。

### 通信全景图

```
主对话区 (Thread)                    Worker 面板
┌─────────────────────┐             ┌──────────────────────┐
│ 用户: "修复这个 bug"  │             │                      │
│                     │             │                      │
│ orchestrator:       │             │                      │
│ "分析后分配给 Claude" │──dispatch──→│ [编排者指令]          │
│                     │             │ "修复 auth 模块的..."  │
│                     │──补充指令──→│ [编排者追加上下文]    │
│ ┌─ SubTaskCard ───┐ │             │                      │
│ │ Worker: claude   │ │←─进度回传──│ [Claude thinking...] │
│ │ 正在执行:修复认证 │ │             │ [工具调用: 读文件]    │
│ │ ✓ 已修改 auth.ts │ │←─文件变更──│ [工具调用: 修改文件]  │
│ │ ✓ 已修改 test.ts │ │←─文件变更──│ [工具调用: 运行测试]  │
│ │ 状态: 完成 ✅     │ │←─完成回传──│ [完成] 修改了 2 文件  │
│ └─────────────────┘ │             │                      │
│                     │←─上报问题──│ "发现依赖冲突..."     │
│ orchestrator:       │──协调指令──→│ [编排者指令]          │
│ "已协调，继续执行"   │             │ "忽略该依赖，使用..." │
│                     │             │                      │
│ [Claude] 已修复...   │←Worker结果──│                      │
│                     │             │                      │
│ orchestrator:       │             │                      │
│ "任务完成。登录bug   │             │                      │
│  是因为token过期检查  │             │                      │
│  使用了错误的时间戳   │             │                      │
│  格式，已修正并补充   │             │                      │
│  了单元测试。"       │             │                      │
└─────────────────────┘             └──────────────────────┘
```

**流程说明**：

1. orchestrator 决策后 dispatch，输出过渡文本
2. orchestrator 可随时通过 send_worker_message 向 Worker 补充上下文或调整指令
3. Worker 独立执行，推理过程在 Worker 面板实时展示，关键节点通过 subTaskCard 回传主对话区
4. Worker 遇到阻塞问题时主动上报 orchestrator，orchestrator 决策后下发协调指令
5. Worker 完成后发送结果消息到主对话区（Worker 身份）
6. orchestrator 汇总 LLM 调用，根据用户原始需求 + Worker 执行结果生成最终结论（orchestrator 身份）

---

## 七、系统提示词设计

系统提示词是统一编排架构的核心，将意图判断和需求分析融入一次 LLM 调用。

**内容组成**：

- **角色定义**：编排者身份、VSCode 插件环境、能力范围
- **可用 Worker 描述**：从 Worker 分工配置（ProfileLoader）动态获取，包括每个 Worker 的画像（WorkerPersona：baseRole、strengths）和当前分工（WorkerAssignments：assignedCategories + CategoryDefinition），不写死任何 Worker 的能力描述
- **决策原则**：四级优先级判断（直接回答 → 内置工具 → dispatch_task → plan_mission）
- **动态升级指导**：当执行过程中发现任务复杂度超出预期时，指导 orchestrator 自主判断是否升级到更高层级（Layer 1→2/3、Layer 2→3），明确升级的触发条件和决策标准
- **项目上下文**：动态注入的项目信息、会话历史、ADR

**Worker 能力描述动态构建流程**：

1. ProfileLoader 从内置画像（WorkerPersona）+ 用户可配置的分工文件（`~/.multicli/worker-assignments.json`）组合生成每个 Worker 的完整画像（WorkerProfile）
2. 系统提示词构建器遍历所有可用 Worker 的 WorkerProfile，动态生成 Worker 能力描述段落
3. 用户调整分工配置后，下次 LLM 调用时系统提示词自动更新，无需重启

**Token 估算**：约 800-1200 tokens。

---

## 八、关键约束与应对

### 约束 1：dispatch_task 必须非阻塞

Worker 执行可能需要分钟级时间。如果同步阻塞在 orchestrator 的工具调用循环中：

- orchestrator 的 LLM 连接空闲等待 → 超时风险
- 用户看到 orchestrator "卡住" → 体验问题
- orchestrator 的工具循环深度有限 → 深度不够

**应对**：dispatch_task 立即返回，Worker 独立执行（Worker 面板实时展示推理过程），完成后通过 DispatchBatch 机制触发汇总。

### 约束 2：Mission 系统不可替代

dispatch_task 支持多次调用实现多 Worker 协作，但以下场景必须使用 Mission（通过 plan_mission 触发）：

- 高风险操作：需要用户确认计划 + 快照回滚
- 大规模架构变更：需要 Assignment 结构化分解 + 验收标准
- 需要 Worker 间严格接口契约：Contract 定义接口边界和数据格式
- 质量控制：需要自动验证 + 验收标准检查

dispatch_task 适合轻量的多步任务和动态升级场景，plan_mission 适合需要前置规划和用户审批的重大任务。两者必须共存。

### 约束 3：Worker UI 反馈不可丢失

Worker 执行期间的所有 UI 交互机制必须保留：

- TasksPanel 子任务卡片（状态实时更新）
- Worker 面板流式输出（thinking / message / 工具调用）
- 动态 Todo 添加
- 进度汇报

### 约束 4：Worker 执行必须可取消和可超时

- **取消信号链**：用户取消操作时，cancel 信号必须通过 CancellationToken 传递到所有活跃 Worker。Worker 的 `executeAssignment` while 循环每次迭代前检查取消信号，LLM 请求通过 AbortController 中断。DispatchBatch 将被取消的 Worker 标记为 `cancelled`，不触发 Phase C
- **全局超时**：每个 dispatch_task 有 Worker 级超时（默认 10 分钟），通过 `Promise.race([workerPromise, timeoutPromise])` 实现。超时后 Worker 标记为 `timeout`，按失败处理
- **资源清理**：Worker 取消/超时/崩溃后，必须调用 `clearAllSessions()` 清理内部状态，避免脏状态泄漏到后续任务

### 约束 5：Worker 执行上下文必须隔离

同一 Worker 类型（如 claude）被多次 dispatch 时，每次 dispatch 必须使用独立的执行上下文，不共享 AutonomousWorker 实例的 `currentSession` 和 `currentMissionId` 等状态。实现方式：为每个 dispatch_task 创建独立的 Worker 执行上下文，或引入 Worker 级任务队列确保同类型串行执行。

### 约束 6：并行 Worker 文件操作必须无冲突

- 多个并行 Worker（无 depends_on）声明了重叠的 `files` 参数时，DispatchBatch 应自动检测冲突并将冲突的 dispatch 转为串行（自动添加 depends_on）
- 未声明 files 参数的并行 Worker，由各自 Worker 的工具层保证文件写入的原子性

### 约束 7：依赖链必须有深度上限和环检测

- **深度上限**：依赖链最大深度 5 层，超过时拒绝 dispatch 并向 orchestrator 返回错误
- **环检测**：DispatchBatch 在注册新任务时进行拓扑排序，检测循环依赖（A→B→A），发现环时拒绝注册并返回具体的环路描述

---

## 九、功能清单

| 功能 | 实现方式 |
|:---|:---|
| 意图判断 | Layer 2：融入系统提示词自主判断；Layer 3：IntentGate 独立分类 |
| 需求分析 | orchestrator LLM 在第一次调用中自然完成 |
| Worker 协作规划 | plan_mission → MissionOrchestrator.planMission |
| 契约系统（Contract） | plan_mission 内部调用 ContractManager |
| 用户确认（Confirmation） | plan_mission 中通过 confirmationCallback 实现 |
| Worker 自主执行循环 | AutonomousWorker 接收 Assignment 执行 |
| Worker UI 反馈 | TasksPanel 子任务卡片 + Worker 面板流式输出 |
| 快照 / 回滚 | SnapshotManager |
| 验证 / 验收标准 | plan_mission 中通过 verifyMission 实现 |
| 进度汇报 | Worker 面板实时展示 + 主对话区 subTaskCard + 完成时独立结果消息 |
| 任务汇总 | Worker 全部完成后 orchestrator 自动触发汇总 LLM 调用，生成最终结论 |
| 动态层级升级 | orchestrator 在执行过程中自主判断是否需要升级（Layer 1→2/3、Layer 2→3） |
| Worker→orchestrator 上报 | Worker 上报触发 orchestrator 中间 LLM 调用，实时响应和协调 |
| 并行/串行执行 | dispatch_task 的 depends_on 参数 + DispatchBatch 依赖链自动编排 |
| Worker 间协作 | SharedContextPool + insightGenerated 事件 |
| 失败恢复 | ProfileAwareRecoveryHandler |
| 动态 Todo | Worker 的 dynamicTodoAdded 事件 |
| 取消与超时 | CancellationToken 信号链 + Worker 级超时（Promise.race） |
| Worker 执行隔离 | 每个 dispatch 独立执行上下文，避免同类型 Worker 状态覆盖 |
| 文件冲突检测 | DispatchBatch 基于 files 参数自动检测并转串行 |
| 依赖链安全 | 拓扑排序 + 环检测 + 深度上限 5 层 |
| 消息管道 | MessageHub（门面层）+ MessagePipeline（校验/去重/节流）+ MessageBus（事件发射）+ MessageFactory（语义化构造） |

---

## 十、风险与应对

| 风险 | 应对 | 严重程度 |
|:---|:---|:---|
| LLM 误判任务复杂度 | 系统提示词中明确判断标准；dispatch_task 执行器检测到多文件修改时建议升级 | 中 |
| Worker 执行失败 | Worker 内部 ProfileAwareRecoveryHandler 处理；失败状态回传主对话区；DispatchBatch 级联跳过依赖的后续 Worker | 中 |
| 系统提示词 token 开销增长 | 工具列表使用精简描述（name + 一句话），约 30-50 tokens/工具 | 低 |
| 工具循环深度不够 | dispatch_task 和 plan_mission 是非阻塞的，不占用循环深度 | 低 |
| Worker 回传消息过多 | 回传消息聚合到 subTaskCard 内部，仅关键节点更新；完成摘要为独立消息 | 低 |
| Worker 间上下文池数据过时 | SharedContextPool 带时间戳，后续 Worker 启动时校验数据时效性 | 低 |
| orchestrator 中间 LLM 调用成本 | 仅 blocker/request 类型触发；最小间隔 30 秒；精简上下文 ~600-800 tokens | 中 |
| 依赖链中前序 Worker 失败 | DispatchBatch 自动级联跳过后续 Worker，Phase C 汇总中报告失败链路 | 中 |
| Worker 后台执行无取消信号 | CancellationToken 机制；cancel 主动终止所有活跃 Worker 的 LLM 调用 | 高 |
| Worker 执行超时无全局监控 | Worker 级超时 10 分钟，Promise.race 包裹；超时按失败处理 | 高 |
| 同 Worker 类型并发 dispatch 状态覆盖 | 每个 dispatch 使用独立执行上下文或 Worker 任务队列串行执行 | 高 |
| Phase C 汇总失败但文件已修改 | 降级展示 Worker 原始结果摘要；汇总 LLM 自动重试 1-2 次；dispatch 模式可考虑轻量级文件快照 | 中 |
| 用户在 Worker 执行中发送新消息 | UI 明确提示当前执行状态；区分补充指令和新请求 | 中 |
| 并行 Worker 同时修改同一文件 | DispatchBatch 基于 files 参数检测冲突，冲突的 dispatch 自动转为串行 | 中 |
| 循环依赖和过深依赖链 | DispatchBatch 注册时拓扑排序 + 环检测；深度上限 5 层 | 中 |
| plan_mission 用户拒绝计划 | orchestrator 在主对话区告知用户拒绝结果，可重新规划或降级为 dispatch_task | 低 |
