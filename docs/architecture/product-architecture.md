# MAGI (MultiCLI) 产品架构现状文档

> **版本**: v2.0（合并统一版）
> **日期**: 2025-07-15
> **基于源码审计生成**

---

## 目录

1. [产品概述](#一产品概述)
2. [系统全局架构](#二系统全局架构)
3. [核心子系统详解](#三核心子系统详解)
   - 3.1 编排引擎
   - 3.2 Worker 子系统
   - 3.3 消息系统
   - 3.4 三通道通信架构
   - 3.5 Mission/Todo 任务管理
   - 3.6 工具系统
   - 3.7 LLM 适配层
   - 3.8 上下文管理系统
   - 3.9 UI/Webview 子系统
   - 3.10 辅助子系统
4. [数据流全景](#四数据流全景)
5. [数据模型与实体关系](#五数据模型与实体关系)
6. [关键设计决策](#六关键设计决策)
7. [文件索引](#七文件索引)

<!-- markdownlint-disable MD051 MD060 MD040 MD012 -->

---

## 一、产品概述

MAGI 是一个 VSCode 扩展，实现了**多模型 AI 编排系统**。其核心理念是通过一个 **Orchestrator（编排者）** 接收用户的复杂任务请求，将其分解为多个子任务，分派给不同的 **Worker（工作单元）** 并行执行，最终汇总结果。系统支持 Claude、Gemini、Codex 等多个 LLM 后端。

### 核心定位

- **编排者-工人模式**：Orchestrator 只规划，不直接动手；Worker 负责具体执行
- **多 Worker 并行**：不同 Worker 可同时处理不同文件/模块
- **统一消息协议**：所有 UI 消息通过标准协议流转，确保一致性
- **Mission 驱动架构**：以 Mission（使命）为最高级别任务抽象

### 核心设计原则

| 原则 | 实现 |
|------|------|
| **单一入口** | `MissionDrivenEngine.execute()` 是编排模式的唯一执行入口 |
| **单一 Todo 创建源** | `PlanningExecutor.createMacroTodo()` 是一级 Todo 的唯一创建入口 |
| **三通道隔离** | UI 消息、编排业务事件、跨模块生命周期事件各走独立通道 |
| **非阻塞调度** | `dispatch_task` 工具立即返回 `task_id`，Worker 后台异步执行 |
| **治理可配置** | WorkerPipeline 的 Snapshot/LSP/TargetEnforce 通过开关控制 |

---

## 二、系统全局架构

```
┌─────────────────────────────────────────────────────────────────┐
│                        VSCode Extension Host                     │
│                                                                  │
│  ┌──────────┐    ┌───────────────────────────────────────────┐  │
│  │ extension │───▶│         MissionDrivenEngine               │  │
│  │   .ts     │    │  (核心编排引擎 — 系统入口)                 │  │
│  └──────────┘    └──────┬──────────────┬──────────────────────┘  │
│                         │              │                         │
│              ┌──────────▼──────┐  ┌────▼─────────────────┐      │
│              │  IntentGate     │  │  MissionOrchestrator  │      │
│              │ (意图门控)       │  │ (Mission 生命周期)    │      │
│              └─────────────────┘  └────┬─────────────────┘      │
│                                        │                         │
│  ┌─────────────┐   ┌──────────────────▼──────────────────────┐  │
│  │ Planning-   │   │          DispatchManager                │  │
│  │ Executor    │──▶│  (派发管理器 — Phase A/B/B+/C)          │  │
│  │ (规划执行器) │   └──────────────────┬──────────────────────┘  │
│  └─────────────┘                      │                         │
│                            ┌──────────▼──────────┐              │
│                            │   DispatchBatch      │              │
│                            │ (批次生命周期追踪)    │              │
│                            └──────────┬──────────┘              │
│                                       │                         │
│              ┌────────────────────────┼────────────────────┐    │
│              │                        │                    │    │
│    ┌─────────▼────┐  ┌───────────────▼──┐  ┌─────────────▼─┐  │
│    │ Worker:Claude │  │ Worker:Codex     │  │ Worker:Gemini │  │
│    │ (Autonomous-  │  │ (Autonomous-     │  │ (Autonomous-  │  │
│    │  Worker)      │  │  Worker)         │  │  Worker)      │  │
│    └───────┬───────┘  └────────┬─────────┘  └──────┬────────┘  │
│            │                   │                    │            │
│    ┌───────▼───────────────────▼────────────────────▼────────┐  │
│    │              WorkerPipeline (重试 + 质量门禁)             │  │
│    └───────────────────────────┬─────────────────────────────┘  │
│                                │                                │
│    ┌───────────────────────────▼─────────────────────────────┐  │
│    │              LLM Adapter Layer                           │  │
│    │  (WorkerAdapter / OrchestratorAdapter)                   │  │
│    └───────────────────────────┬─────────────────────────────┘  │
│                                │                                │
│    ┌───────────────────────────▼─────────────────────────────┐  │
│    │              UniversalLLMClient                          │  │
│    │  (Anthropic / OpenAI / Gemini — 多协议适配)              │  │
│    └─────────────────────────────────────────────────────────┘  │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────────┐│
│  │                    横切关注点                                ││
│  │  MessageHub (消息)  |  TodoManager (任务)  |  ToolManager   ││
│  │  ContextManager     |  SnapshotManager     |  SessionManager││
│  └─────────────────────────────────────────────────────────────┘│
└──────────────────────────────────┬──────────────────────────────┘
                                   │ postMessage
                    ┌──────────────▼────────────────┐
                    │      Webview (Svelte 5)        │
                    │  MessageList | TasksPanel |    │
                    │  ThreadPanel | InputArea       │
                    └───────────────────────────────┘
```

### 架构分层

```
┌─────────────────────────────────────────────────────────────────────┐
│                          用户界面层 (UI Layer)                        │
│   WebviewProvider ─── Svelte 前端 ─── message-handler.ts             │
├─────────────────────────────────────────────────────────────────────┤
│                       通信层 (Communication Layer)                    │
│   通道1: MessageHub    通道2: MissionOrchestrator    通道3: globalEventBus │
├─────────────────────────────────────────────────────────────────────┤
│                       编排层 (Orchestration Layer)                    │
│   MissionDrivenEngine ─── DispatchManager ─── PlanningExecutor       │
├─────────────────────────────────────────────────────────────────────┤
│                       执行层 (Execution Layer)                        │
│   WorkerPipeline ─── AutonomousWorker ─── DispatchBatch              │
├─────────────────────────────────────────────────────────────────────┤
│                       领域模型层 (Domain Model Layer)                  │
│   Mission ─── Assignment ─── UnifiedTodo ─── Contract                │
├─────────────────────────────────────────────────────────────────────┤
│                       基础设施层 (Infrastructure Layer)                │
│   ContextManager ─── TodoManager ─── SessionManager ─── SnapshotManager │
│   ProfileLoader ─── AdapterFactory ─── ToolManager ─── LLM Clients   │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 三、核心子系统详解

### 3.1 编排引擎 (Orchestrator Core)

**文件位置**: `src/orchestrator/core/`

#### 3.1.1 MissionDrivenEngine — 系统入口

**文件**: `src/orchestrator/core/mission-driven-engine.ts`
**职责**: 系统的最高级别控制器，接收用户输入并协调所有子系统。

**核心方法**:

| 方法 | 签名 | 职责 |
|------|------|------|
| `execute` | `(prompt, options?) → Promise<string>` | 主入口，处理用户请求的完整生命周期 |
| `setupEventForwarding()` | `() → void` | 将 MissionOrchestrator 事件桥接到 MessageHub |
| `prepareContext()` | `(sessionId, prompt) → Promise<string>` | 组装 ContextManager 上下文 |
| `reportTodoProgress()` | `(data) → void` | 统一 Todo 进度报告 |
| `recordOrchestratorTokens()` | `(response) → void` | 记录编排器 Token 使用 |

**`execute()` 完整流程**:

```
用户输入 prompt
    │
    ▼
1. newTrace() — 生成会话追踪 ID
    │
    ▼
2. IntentGate.classify(prompt) — 意图分类
    │
    ├─── ASK/EXPLORE → handleDirectMode() → 直接回答
    ├─── DIRECT → DirectExecutionService → 单 Worker 执行
    ├─── CLARIFY → 向用户提问 → 重新分类
    └─── TASK/DEMO → handleOrchestrationMode() ───▶ 进入编排
                │
                ▼
            3. Mission.create() — 创建任务使命
                │
                ▼
            4. 组装上下文 + 构建统一系统提示词
                │
                ▼
            5. 单次 LLM 调用（自动包含工具循环，LLM 可调用 dispatch_task）
                │
                ▼
            6. 等待 DispatchBatch 归档（Worker 异步执行 + Phase C 汇总）
                │
                ▼
            7. finally: 更新 Mission 状态 + 清理 currentMissionId
```

#### 3.1.2 IntentGate — 意图门控

**文件**: `src/orchestrator/intent-gate.ts`
**职责**: 用户输入的前置分类器，决定走哪条处理路径。

**意图类型**:

| 意图 | 处理模式 | 描述 |
|------|---------|------|
| `question` | ASK | 简单问答，直接回答 |
| `trivial` | DIRECT | 简单操作，单 Worker 执行 |
| `exploratory` | EXPLORE | 代码分析探索 |
| `task` | TASK | 复杂任务，完整编排流程 |
| `demo` | DEMO | 演示模式 |
| `ambiguous` | CLARIFY | 需要澄清 |

**分类方式**: 完全由 AI 决策。调用 LLM 并解析返回的 JSON 结构化决策。

#### 3.1.3 DispatchManager — 派发管理器

**文件**: `src/orchestrator/core/dispatch-manager.ts`
**职责**: 管理 Orchestrator LLM 与 Worker 之间的任务派发和协调。

**Phase 模型**:

```
Phase A — Orchestrator LLM 调用（规划+工具选择）
    │
    ├── dispatch_task 工具调用 → 注册到 DispatchBatch
    │                              │
    │                              ▼
    │                         Phase B — Worker 并行执行
    │                              │
    │                              ├── Worker 上报 progress → 刷新 SubTaskCard
    │                              ├── Worker 上报 question → Phase B+（中间 LLM 调用）
    │                              └── Worker 上报 completed/failed → Batch 状态更新
    │                              │
    │                              ▼
    │                      全部完成 → batch:allCompleted 事件
    │
    ▼
Phase C — 汇总 LLM 调用
    │
    ├── 生成执行报告
    ├── 检查是否需要追加新 Batch（迭代编排）
    └── 归档 Batch → archive()
```

**Phase 执行阶段**:

| Phase | 触发时机 | 行为 |
|-------|---------|------|
| Phase A | `dispatch_task` 工具调用 | 注册任务、拓扑排序、启动 Worker |
| Phase B | Worker 执行中 | Worker 自主执行 Todo，通过 WorkerPipeline 治理 |
| Phase B+ | Worker 上报 question | Orchestrator 中间 LLM 调用，给 Worker 补充指令 |
| Phase C | `batch:allCompleted` 事件 | Orchestrator 汇总 LLM 调用，生成最终总结 |

**`dispatch_task` 工具处理流程**:

```
1. 校验 Worker 是否已启用（LLMConfigLoader）
2. 生成唯一 task_id
3. 创建/复用 DispatchBatch（使用 Mission ID 作为 Batch ID）
4. 注册到 DispatchBatch（含拓扑排序、环检测、文件冲突解决）
5. 发送 subTaskCard 到 UI
6. 隔离策略决定是否立即启动
7. 返回 { task_id, status: 'dispatched' }（非阻塞）
```

**`launchDispatchWorker()` 执行流程**:

```
1. 标记任务 Running + 更新 subTaskCard
2. 发送任务指令到 Worker Tab（messageHub.workerInstruction）
3. ensureWorkerForDispatch → 获取 Worker 实例
4. 构建轻量 Assignment 对象
5. PlanningExecutor.createMacroTodo → 创建一级 Todo
6. missionOrchestrator.emit('assignmentPlanned') → 前端 Todo 面板更新
7. WorkerPipeline.execute → 执行治理管道 + Worker 执行
8. 完成后更新 subTaskCard + DispatchBatch 状态
```

#### 3.1.4 DispatchBatch — 批次生命周期追踪

**文件**: `src/orchestrator/core/dispatch-batch.ts`
**职责**: 追踪一次编排调用中所有 `dispatch_task` 的状态和依赖关系。

**核心数据结构**:

```typescript
interface DispatchEntry {
  taskId: string;           // 任务标识
  worker: WorkerSlot;       // 分配的 Worker 类型
  task: string;             // 任务描述
  files: string[];          // 相关文件
  dependsOn: string[];      // 依赖的前序任务
  status: DispatchStatus;   // pending → running → completed/failed
  result?: DispatchResult;  // 执行结果
}
```

**关键能力**:

| 能力 | 方法 | 说明 |
|------|------|------|
| 依赖管理 | `canExecute()` / `checkDependents()` | 前序完成后自动就绪后续任务 |
| 文件冲突检测 | `detectFileConflicts()` / `resolveFileConflicts()` | 自动将冲突的并行任务转为串行 |
| 拓扑排序 | `topologicalSort()` | Kahn 算法，环检测 + 执行顺序 |
| Worker 隔离 | `getReadyTasksIsolated()` | 同 Worker 串行，不同 Worker 并行 |
| 取消链 | `cancelAll()` → `CancellationToken` | 统一取消信号传递 |
| Token 成本追踪 | `getTokenConsumption()` | 累计 Batch 内所有任务的 Token 用量 |

**防阻塞机制**:

- **idle 超时**: `waitForArchive()` 每 30s 检查 `_lastActivityAt`，超过 5 分钟无活动自动 `cancelAll()`
- **活动刷新**: Worker 上报进度、LLM chunk 到达都会调用 `touchActivity()` 刷新时间戳
- **Phase C 超时**: DispatchManager 层还有 2 分钟绝对超时保护

**状态机**: `active → all_completed → archived`

**事件**:

| 事件 | 触发时机 | 消费者 |
|------|---------|--------|
| `task:statusChanged` | 任务状态变化 | DispatchManager（重新调度） |
| `task:ready` | 依赖满足可执行 | DispatchManager（启动 Worker）|
| `batch:allCompleted` | 全部任务完成 | DispatchManager（触发 Phase C）|
| `batch:cancelled` | Batch 被取消 | DispatchManager（通知用户）|

#### 3.1.5 MissionOrchestrator — Mission 生命周期

**文件**: `src/orchestrator/core/mission-orchestrator.ts`
**职责**: Mission 的创建、状态流转、Assignment 管理、Worker 池管理和持久化。

**核心 API**:

| 方法 | 用途 |
|------|------|
| `createMission(prompt, goal, analysis)` | 创建新 Mission |
| `transitionStatus(missionId, newStatus)` | 状态流转 |
| `addAssignment(missionId, assignment)` | 添加职责分配 |
| `ensureWorkerForDispatch(slot)` | 供 DispatchManager 创建/获取 Worker |
| `setCurrentMissionId(id)` | 同步当前 Mission ID |
| `getTodoManager()` | 获取共享 TodoManager 实例 |
| `summarizeMission(missionId)` | 生成 Mission 总结报告 |

**Worker 生命周期管理**:

```
ensureWorker(slot)
├── 检查 workers Map 是否已有实例
├── 初始化 TodoManager（如需要）
├── 创建 AutonomousWorker（注入共享上下文依赖）
├── 绑定事件转发：
│   ├── worker.on('todoStarted')      → this.emit('todoStarted', { ...data, missionId })
│   ├── worker.on('todoCompleted')    → this.emit('todoCompleted', { ...data, missionId })
│   ├── worker.on('todoFailed')       → this.emit('todoFailed', { ...data, missionId })
│   ├── worker.on('dynamicTodoAdded') → this.emit('dynamicTodoAdded', { ...data, missionId })
│   └── worker.on('insightGenerated') → this.emit('insightGenerated', { ...data, missionId })
└── 缓存到 workers Map
```

#### 3.1.6 PlanningExecutor — 规划执行器

**文件**: `src/orchestrator/core/executors/planning-executor.ts`
**职责**: 一级 Todo 的唯一创建入口。

**设计原则**: 1 个 Assignment = 1 个一级 Todo。一级 Todo 由编排层创建（无 `parentId`），Worker 执行过程中通过 `addDynamicTodo` 创建二级 Todo（`parentId` 指向一级）。

**`createMacroTodo()` 流程**:

```
1. 构建 Todo 内容（assignment.responsibility + targetPaths）
2. todoManager.create({ missionId, assignmentId, content, workerId, ... })
3. 将 Todo 挂载到 Assignment（assignment.todos = [todo]）
4. 更新 Assignment 状态（planningStatus = 'planned', status = 'ready'）
```

#### 3.1.7 WorkerPipeline — 统一执行管道

**文件**: `src/orchestrator/core/worker-pipeline.ts`
**职责**: Worker 执行的外层包装，负责重试、质量门禁和状态同步。

**执行步骤**:

| 步骤 | 名称 | 触发条件 | 作用 |
| ---- | ---- | -------- | ---- |
| 1 | Snapshot 创建 | `enableSnapshot` | 文件快照，支持失败回滚 |
| 2 | 快照上下文设置 | 始终执行 | ToolManager 记录文件变更归属 |
| 3 | 上下文快照 | `enableContextUpdate` | 为 Worker 注入共享上下文 |
| 4 | 目标文件预快照 | `enableTargetEnforce` | 捕获执行前文件内容 |
| 5 | LSP 预检 | `enableLSP` | 记录执行前编译诊断 |
| 6 | **Worker 执行** | 始终执行 | 核心执行步骤 |
| 7 | 目标变更检测 | `enableTargetEnforce` | 检测文件是否被修改，未修改则重试 |
| 8 | LSP 后检 | `enableLSP` | 检测新增编译错误 |
| 9 | Context 更新 | `enableContextUpdate` | 更新 ContextManager |

**质量门禁机制**:

| 门禁 | 检测内容 | 处理方式 |
|------|---------|---------|
| 目标变更检测 | 要求修改文件但实际未变化 | 强制重试，注入变更引导 |
| LSP 编译检查 | 执行后是否引入新编译错误 | 记录质量缺陷 |
| 上下文完整性 | 是否写入 Shared Facts | 质量门禁失败 → 重试 |

---

### 3.2 Worker 子系统

**文件位置**: `src/orchestrator/worker/` + `src/llm/adapters/`

#### 3.2.1 AutonomousWorker — 自主工作单元

**文件**: `src/orchestrator/worker/autonomous-worker.ts`
**职责**: 接收 Assignment 后自主规划和执行 Todo 列表，具备自我修复能力。

**执行流程**:

```
接收 Assignment
    │
    ▼
1. 上下文组装 (ContextAssembler)
   ├── 项目知识 (按 8k token 预算)
   ├── 共享上下文 (其他 Worker 的产出)
   ├── 协作契约
   └── 长期记忆
    │
    ▼
2. Session 恢复或创建
    │
    ▼
3. Todo 执行循环 (executeAssignment)
   ├── getNextExecutableTodo() — 获取下一个可执行 Todo
   ├── 构建 Prompt (注入目标文件摘要 + 自检引导)
   ├── executeWithWorker() — 多轮 LLM 交互 + 工具调用
   ├── reportProgress() — 向 Orchestrator 汇报
   └── 处理 OrchestratorAdjustment (动态调整)
    │
    ▼
4. 质量门禁 (applyQualityGate)
    │
    ▼
5. 生成 WorkerResult + Worker Insight
```

**动态调整能力**:

- 编排者可在 Worker 执行中下发 `OrchestratorAdjustment` 指令
- 支持：跳过步骤、新增 Todo、调整优先级、终止任务
- Worker Insight 自动写入共享上下文池，供其他 Worker 复用

**事件发射**:

| 事件 | 数据 | 消费者 |
| ---- | ---- | ------ |
| `todoStarted` | `{ assignmentId, todoId, content }` | MO → MDE → UI |
| `todoCompleted` | `{ assignmentId, todoId, content, output }` | MO → MDE → UI |
| `todoFailed` | `{ assignmentId, todoId, content, error }` | MO → MDE → UI |
| `dynamicTodoAdded` | `{ assignmentId, todo }` | MO → UI |
| `insightGenerated` | `{ workerId, type, content, importance }` | MO → MDE |

#### 3.2.2 WorkerLLMAdapter — Worker 适配器

**文件**: `src/llm/adapters/worker-adapter.ts`
**职责**: Worker 与 LLM 的交互适配，实现工具调用循环和智能空转检测。

| 能力 | 说明 |
|------|------|
| 多轮工具调用 | LLM 输出 tool_call → 执行 → 结果回传 → LLM 继续 |
| 独立 Stream | 每轮 LLM 交互启动独立的流式通道 |
| 空转检测 | 连续只读操作 → 多级警告 → 强制终止 |
| 无实质输出检测 | 检测 Worker 是否只在空转不产出 |
| 编排者写入限制 | Orchestrator 最多修改 3 个文件 |

---

### 3.3 消息系统

**文件位置**: `src/orchestrator/core/message-*.ts` + `src/protocol/`

#### 架构分层（门面模式）

```
┌────────────────────────────────────────────┐
│              MessageHub (门面层)            │
│  语义化 API: progress(), result(),         │
│  workerOutput(), error(), broadcast()      │
└─────────────────────┬──────────────────────┘
                      │
┌─────────────────────▼──────────────────────┐
│            MessageFactory (业务层)          │
│  将业务意图 → StandardMessage 对象          │
│  自动填充 TraceId, RequestId, Timestamp     │
└─────────────────────┬──────────────────────┘
                      │
┌─────────────────────▼──────────────────────┐
│           MessagePipeline (协议层)          │
│  校验 → 去重 → 节流 → 封缄 → 统计         │
└─────────────────────┬──────────────────────┘
                      │
┌─────────────────────▼──────────────────────┐
│             MessageBus (传输层)             │
│  EventEmitter — 事件广播                    │
└────────────────────────────────────────────┘
```

#### StandardMessage 核心结构

```typescript
interface StandardMessage {
  id: string;                    // 消息唯一标识
  traceId: string;               // 会话追踪 ID
  category: MessageCategory;     // CONTENT | CONTROL | NOTIFY | DATA
  type: MessageType;             // TEXT | PLAN | TASK_CARD | INSTRUCTION | ...
  source: MessageSource;         // 'orchestrator' | 'worker'
  agent: AgentType;              // 'claude' | 'codex' | 'gemini'
  lifecycle: MessageLifecycle;   // STARTED | STREAMING | COMPLETED | FAILED
  blocks: ContentBlock[];        // 内容块 (Text, Code, ToolCall, Plan)
  metadata: MessageMetadata;     // requestId, cardId, worker 等
}
```

#### 语义化 API

| API | 区域 | 用途 |
| --- | ---- | ---- |
| `progress(phase, content)` | 主对话区 | 阶段进度 |
| `result(content)` | 主对话区 | 最终结果 |
| `orchestratorMessage(content)` | 主对话区 | 分析/规划消息 |
| `subTaskCard(payload)` | 主对话区 | 子任务卡片状态 |
| `workerOutput(worker, content)` | Worker Tab | Worker 执行日志 |
| `workerInstruction(worker, content)` | Worker Tab | 任务说明 |
| `workerError(worker, error)` | Worker Tab | Worker 错误 |
| `error(err)` | 系统 | 错误信息 |
| `notify(msg, level)` | 系统 | 通知消息 |
| `broadcast(msg)` | 全局 | 全局广播 |

#### 消息路由规则

| 类别 | 用途 | 路由目标 |
|------|------|---------|
| `CONTENT` | 对话、代码、工具调用 | 主对话区 or Worker Tab |
| `CONTROL` | 状态机驱动 | UI 状态更新 |
| `NOTIFY` | Toast 短暂提示 | 通知栏 |
| `DATA` | 静默数据同步 | 后台处理 |

- `source: 'orchestrator'` → 主对话区
- `source: 'worker'` + `agent: [WorkerName]` → 对应 Worker Tab
- `metadata.dispatchToWorker: true` → Worker Tab

#### 流控机制

| 机制 | 实现 | 说明 |
|------|------|------|
| 节流 | `minStreamInterval` | 流式消息最小发送间隔 |
| 封缄卡片 | `sealedCards` | 已完结消息不接受后续更新 |
| 流式序号 | `cardStreamSeq` | 乱序包自动丢弃 |
| 死信队列 | `deadLetters` | 记录被拒绝的异常消息 |
| 防重入 | `processingMessageIds` | 同一消息不重复处理 |

---

### 3.4 三通道通信架构

MultiCLI 采用三通道隔离的通信架构：

```text
通道1 ── MessageHub（UI 消息路由）
  │  职责：所有 UI 消息的统一出口
  │  方向：编排层 → UI（单向）
  │  监听者：WebviewProvider（unified:message / unified:update / unified:complete / processingStateChanged）
  │
通道2 ── MissionOrchestrator（编排业务事件）
  │  职责：Mission/Assignment/Todo 生命周期事件
  │  方向：Worker → MissionOrchestrator → WebviewProvider/MDE
  │  监听者：WebviewProvider.bindMissionEvents() + MDE.setupEventForwarding()
  │
通道3 ── globalEventBus（跨模块生命周期事件）
     职责：task:completed/failed/cancelled 等顶层生命周期
     方向：MDE → globalEventBus → extension.ts / WebviewProvider
     监听者：extension.ts（状态栏）、WebviewProvider（状态更新）
```

#### 通道2 事件清单（MissionOrchestrator）

| 事件 | 发射者 | 消费者 | 用途 |
| ---- | ------ | ------ | ---- |
| `workerOutput` | Worker | MDE → MessageHub | Worker 执行日志 |
| `analysisComplete` | MO | MDE → MessageHub | 分析结果 |
| `missionPlanned` | MO | MDE → MessageHub | 执行计划卡片 |
| `assignmentStarted` | MO | MDE + WVP | 任务开始 |
| `assignmentPlanned` | DispatchManager | WVP | Todo 面板更新 |
| `assignmentCompleted` | MO | WVP | 任务完成 |
| `todoStarted` | Worker → MO | MDE + WVP | Todo 开始 |
| `todoCompleted` | Worker → MO | MDE + WVP | Todo 完成 |
| `todoFailed` | Worker → MO | MDE + WVP | Todo 失败 |
| `dynamicTodoAdded` | Worker → MO | WVP | 动态 Todo |
| `insightGenerated` | Worker → MO | MDE → MessageHub | Worker 洞察 |
| `executionCompleted` | MO | WVP | 执行完成 |
| `executionFailed` | MO | WVP | 执行失败 |
| `missionCancelled` | MO | WVP | 任务取消 |
| `approvalRequested` | Worker → MO | WVP | 审批请求 |

#### 通道3 事件清单（globalEventBus）

| 事件 | 发射者 | 消费者 |
| ---- | ------ | ------ |
| `task:completed` | MDE.execute() | extension.ts、WVP |
| `task:failed` | MDE.execute() | extension.ts、WVP |
| `task:cancelled` | extension.ts | WVP |
| `snapshot:created` | SnapshotManager | WVP |
| `snapshot:reverted` | SnapshotManager | WVP |
| `worker:statusChanged` | — | WVP |
| `tool:authorization_request` | — | WVP |
| `execution:stats_updated` | — | WVP |
| `orchestrator:phase_changed` | — | WVP |

---

### 3.5 Mission/Todo 任务管理系统

**文件位置**: `src/orchestrator/mission/` + `src/todo/`

#### 实体关系

```text
Mission (使命 — 最高级别)
    │
    ├── Assignment (职责分配 — 连接 Worker)
    │       │
    │       └── UnifiedTodo (统一待办 — 最小执行单元)
    │               │
    │               └── UnifiedTodo (子 Todo — 支持多级嵌套)
    │
    └── Contract (协作契约 — Worker 间的接口约定)
```

#### Mission 状态机

```text
draft ──▶ planning ──▶ pending_review ──▶ pending_approval
                                              │
                                              ▼
                                          executing ◀──▶ paused
                                              │
                                    ┌─────────┼─────────┐
                                    ▼         ▼         ▼
                               completed    failed   cancelled
```

#### Todo 状态机

```text
pending ──┬──▶ blocked ──▶ ready
          │       ▲          │
          │       │          ▼
          └──────────────▶ running ──┬──▶ completed
                              │      │
                              │      └──▶ failed ──▶ pending (retry)
                              │
                              └──▶ skipped
```

#### TodoManager 核心能力

| API | 说明 |
|-----|------|
| `create()` | 创建 Todo，自动进入依赖检查 |
| `start(todoId)` | 标记开始执行，启动超时计时器 |
| `complete(todoId, output)` | 完成，注册产出契约，触发后续依赖 |
| `fail(todoId, error)` | 失败 |
| `retry(todoId)` | 重试（受 maxRetries 限制） |
| `resetToPending(todoId)` | 外部治理重置，不增加 retryCount |
| `prepareForExecution(todoId)` | 检查依赖并更新状态 (pending → ready) |
| `checkMissionCompletion(missionId)` | 检查整个 Mission 是否完成 |
| `revisePlan(missionId, feedback)` | 应用规划修订（增删改 Todo） |

**持久化**: FileTodoRepository — 基于文件系统存储，内存缓存（最大 500 条）

**父子联动**: 当所有子 Todo 完成时，自动将父 Todo 标记为 completed

---

### 3.6 工具系统

**文件位置**: `src/tools/`

#### ToolManager 架构

```text
ToolManager
├── 工具注册表（按角色分类）
│   ├── Orchestrator 工具：dispatch_task, ask_user, complete_orchestration
│   ├── Worker 工具：read_file, write_file, search, run_command, ...
│   └── 共享工具：list_files, grep_search
│
├── OrchestrationExecutor（编排专用执行器）
│   ├── dispatch_task → DispatchManager.handleDispatchTask()
│   ├── ask_user → SupplementaryInstructionQueue
│   └── complete_orchestration → 结束标记
│
└── 权限控制
    ├── Orchestrator 默认禁止写操作（最多修改 3 个文件）
    ├── Worker 拥有完整读写权限
    └── 快照上下文追踪文件变更归属
```

#### 核心工具清单

| 工具 | 角色 | 用途 |
| ---- | ---- | ---- |
| `dispatch_task` | Orchestrator | 派发子任务给 Worker |
| `ask_user` | Orchestrator | 向用户提问 |
| `complete_orchestration` | Orchestrator | 标记编排完成 |
| `read_file` | Worker | 读取文件 |
| `write_file` | Worker | 写入文件 |
| `search` / `grep_search` | 共享 | 搜索代码 |
| `run_command` | Worker | 执行命令行 |
| `list_files` | 共享 | 列出文件 |
| `add_dynamic_todo` | Worker | 动态添加子 Todo |
| `report_progress` | Worker | 向编排者汇报进度 |

---

### 3.7 LLM 适配层

**文件位置**: `src/llm/`

#### 多模型支持

| 模型 | 协议 | 适配器 |
| ---- | ---- | ------ |
| Claude (Anthropic) | Messages API | AnthropicClient |
| GPT-4 / Codex (OpenAI) | Chat Completions | OpenAIClient |
| Gemini (Google) | Generative AI | GeminiClient |

#### 适配器分层

```text
┌─────────────────────────────────┐
│    OrchestratorAdapter          │  ← Orchestrator LLM 调用
│    WorkerAdapter                │  ← Worker LLM 调用
├─────────────────────────────────┤
│    UniversalLLMClient           │  ← 统一接口
├─────────────────────────────────┤
│  AnthropicClient | OpenAIClient │  ← 协议适配
│  GeminiClient                   │
└─────────────────────────────────┘
```

#### 稳健性策略

| 策略 | 说明 |
| ---- | ---- |
| 流式传输 | 所有 LLM 调用均为流式，实时反馈 |
| 超时保护 | 单次调用超时自动中断 |
| 空转检测 | 连续只读操作超过阈值告警 |
| Token 计量 | 每次调用记录 input/output token 数 |
| 错误重试 | 网络错误自动重试（指数退避） |

---

### 3.8 上下文管理系统

**文件位置**: `src/context/`

#### ContextManager 三层架构

```text
Layer 1 ── 即时上下文（最近几轮对话）
  │  策略：Augment 风格预防性截断
  │  存储：内存（immediateContext[]）
  │
Layer 2 ── 会话 Memory（结构化任务记录）
  │  策略：LLM 智能压缩
  │  存储：MemoryDocument
  │  内容：currentTasks, codeChanges, decisions, pendingIssues
  │
Layer 3 ── 项目知识库（跨会话知识）
     策略：持久化索引
     存储：ProjectKnowledgeBase
     内容：ADR, 项目上下文, 搜索索引
```

#### 跨 Worker 共享上下文

```text
SharedContextPool
├── 按 Mission 隔离（不同 Mission 互不污染）
├── 条目类型：decision | contract | file_summary | risk | constraint | insight
├── 自动去重（内容相似度 > 90% 时合并来源）
├── 重要性分级：critical > high > medium > low
└── Token 预算限制

FileSummaryCache
├── 文件摘要缓存
├── hash 校验自动失效
└── 减少重复文件读取

ContextAssembler
├── 组装最终上下文（多来源合并）
├── Token 预算控制
├── 按 ContextPartType 排除
└── 支持 localTurns 配置
```

#### 上下文在编排中的流动

```text
MDE.execute()
├── prepareContext(sessionId, prompt)
│     └── ContextManager.getAssembledContextText(options)
│           ├── Layer 1: 最近对话
│           ├── Layer 2: Memory 文档
│           └── Layer 3: 项目知识
│
├── buildUnifiedSystemPrompt({ sessionSummary: context, projectContext, relevantADRs })
│     └── 注入到 Orchestrator LLM 系统提示词
│
└── Worker 执行时：
      WorkerPipeline
        ├── generateContextSnapshot(missionId, workerId, contextManager)
        │     └── 注入到 Worker LLM messageMeta.contextSnapshot
        └── updateContextManager(assignment, result, contextManager)
              └── 执行完成后更新 ContextManager
```

---

### 3.9 UI/Webview 子系统

**文件位置**: `src/ui/` + `src/ui/webview-svelte/`

#### 通信架构

```text
Extension Host                          Webview (Svelte 5)
┌──────────────────┐                   ┌──────────────────────┐
│  WebviewProvider  │ ──postMessage──▶ │  message-handler.ts   │
│                   │                   │                       │
│  - 监听 MessageBus│                   │  - 解析 StandardMsg   │
│  - 监听 MO events │ ◀──postMessage── │  - 路由到对应 Store   │
│  - 监听 globalBus │                   │  - 触发 UI 渲染       │
│  - 处理用户命令   │                   │                       │
└──────────────────┘                   └──────────────────────┘
```

#### WebviewProvider 主要职责

| 职责 | 说明 |
| ---- | ---- |
| 消息桥接 | 监听 MessageBus 事件，转发到 Webview |
| 编排事件绑定 | `bindMissionEvents()` 监听 MO 事件，更新 Todo 面板 |
| 全局事件绑定 | `bindGlobalEvents()` 监听 globalEventBus，同步状态 |
| 用户命令处理 | 接收 Webview 发来的用户输入、快照操作、配置更改等 |
| 状态同步 | `sendStateUpdate()` 推送全局状态到 Webview |

#### 前端组件结构

```text
App.svelte
├── MessageList        — 主对话区（Orchestrator 消息流）
├── ThreadPanel        — Worker Tab 面板（按 Worker 分组）
├── TasksPanel         — 任务/Todo 面板
├── InputArea          — 用户输入区
├── SnapshotIndicator  — 快照状态指示器
└── StatusBar          — 状态栏
```

#### 前端状态管理

- 使用 Svelte 5 Runes（`$state`、`$derived`、`$effect`）管理响应式状态
- `message-handler.ts` 统一处理所有 postMessage，按 `category` 和 `type` 路由

---

### 3.10 辅助子系统

#### 3.10.1 SupplementaryInstructionQueue（补充指令队列）

**文件**: `src/orchestrator/core/supplementary-instruction-queue.ts`

- Orchestrator 调用 `ask_user` 工具时，问题进入队列
- 用户在 Webview 回答后，答案注入到 Orchestrator 的下一轮上下文
- 支持超时自动跳过

#### 3.10.2 知识系统

```text
ProjectKnowledgeBase
├── ADR（架构决策记录）管理
├── 项目上下文持久化
├── 全文搜索索引
└── 跨会话知识复用

WisdomExtractor
├── 从执行结果中提取经验
├── 写入 ProjectKnowledgeBase
└── 未来执行可复用
```

#### 3.10.3 PromptEnhancer（提示词增强）

- 画像系统（`src/orchestrator/profile/`）为不同 Worker 注入角色提示
- `ProfileLoader` 加载 Worker 画像配置
- `GuidanceInjector` 向 Worker 注入任务引导
- `PromptBuilder` 构建最终提示词

#### 3.10.4 DirectExecutionService（直接执行服务）

**文件**: `src/services/direct-execution-service.ts`

- 处理 `DIRECT` 意图的简单任务
- 跳过完整编排流程，直接创建单个 Worker 执行
- 无 Mission/Assignment 开销

---

## 四、数据流全景

### 编排模式完整数据流

```text
用户输入 prompt
    │
    ▼
MDE.execute(prompt)
    ├── IntentGate.classify(prompt) → TASK
    ├── MO.createMission(prompt) → missionId
    ├── prepareContext() → contextText
    ├── buildUnifiedSystemPrompt() → systemPrompt
    │
    ▼
Orchestrator LLM 调用（含工具循环）
    ├── LLM 决定调用 dispatch_task(worker, task, files, dependsOn)
    │   └── DM.handleDispatchTask()
    │       ├── DispatchBatch.register(entry) ─── 拓扑排序 + 文件冲突
    │       ├── messageHub.subTaskCard()                               ── 通道1
    │       └── DM.launchDispatchWorker(entry)
    │             ├── PlanningExecutor.createMacroTodo()
    │             ├── MO.emit('assignmentPlanned')                     ── 通道2
    │             └── WorkerPipeline.execute()
    │                   ├── Snapshot 创建
    │                   ├── 上下文快照注入
    │                   ├── AutonomousWorker.executeAssignment()
    │                   │     ├── Todo 循环：start → execute → complete
    │                   │     │     ├── worker.emit('todoStarted')      ── 通道2
    │                   │     │     ├── WorkerAdapter.executeWithTools()
    │                   │     │     │     ├── LLM Stream → messageHub.workerOutput() ── 通道1
    │                   │     │     │     └── 工具调用 → 结果回传
    │                   │     │     └── worker.emit('todoCompleted')    ── 通道2
    │                   │     │
    │                   │     ├── reportProgress() → DispatchManager
    │                   │     │     └── messageHub.subTaskCard(update)  ── 通道1
    │                   │     │
    │                   │     └── (若有问题) reportQuestion() → Phase B+
    │                   │           └── DM.triggerPhaseBPlusLLM()
    │                   │
    │                   ├── 目标变更检测 → 可能重试
    │                   ├── LSP 后检 → 质量报告
    │                   └── Context 更新
    │
    └── (所有 dispatch_task 完成)
          │
          ▼

DispatchManager.launchDispatchWorker
    ├── batch.markCompleted(taskId)
    │     └── batch.emit('task:statusChanged')
    │           └── DispatchManager.dispatchReadyTasksWithIsolation()
    │
    └── (当所有任务完成) batch.emit('batch:allCompleted')
          └── DispatchManager.triggerPhaseCSummary()
                ├── LLM 汇总调用
                ├── messageHub.result(content)                         ── 通道1
                └── batch.archive()
                      └── MDE.execute() 中 waitForArchive() 解除

[MDE.execute() finally]
  ├── missionOrchestrator.setCurrentMissionId(null)
  ├── completeTaskById(missionId) / failTaskById / cancelTaskById
  └── globalEventBus.emitEvent('task:completed' / 'task:failed')      ── 通道3
        ├── extension.ts → 状态栏更新
        └── WebviewProvider → sendStateUpdate()
```

### 调度策略

**隔离策略**: 同类型 Worker 串行、不同类型 Worker 并行

```text
dispatch_task(worker=claude, task=A)    ─── 立即启动
dispatch_task(worker=gemini, task=B)    ─── 立即启动（不同类型，并行）
dispatch_task(worker=claude, task=C)    ─── 等待 A 完成后启动（同类型，串行）
dispatch_task(worker=codex, task=D, dependsOn=[A]) ─── 等待 A 完成（依赖关系）
```

**安全机制**:

| 机制 | 实现 |
| ---- | ---- |
| 环检测 | `topologicalSort()` 拓扑排序检测 |
| 深度上限 | `validateDepthLimit()` 验证 |
| 文件冲突 | `resolveFileConflicts()` 自动添加依赖转串行 |
| 取消传播 | `CancellationToken` 共享，中断所有 Worker |
| Phase B+ 频率限制 | 同一 Batch 内最小间隔 30 秒 |
| Phase C 超时 | 2 分钟超时 + 降级展示 |

---

## 五、数据模型与实体关系

### 核心数据结构

```typescript
// Mission — 最高级别任务抽象
interface Mission {
  id: string;
  status: MissionStatus;
  prompt: string;
  goal: string;
  analysis: MissionAnalysis;
  assignments: Assignment[];
  contracts: Contract[];
  createdAt: Date;
  completedAt?: Date;
}

// Assignment — 职责分配
interface Assignment {
  id: string;
  missionId: string;
  worker: WorkerSlot;
  responsibility: string;
  targetPaths: string[];
  readPaths: string[];
  dependsOn: string[];
  todos: UnifiedTodo[];
  status: AssignmentStatus;
  planningStatus: 'unplanned' | 'planning' | 'planned';
  result?: WorkerResult;
}

// UnifiedTodo — 统一待办
interface UnifiedTodo {
  id: string;
  missionId: string;
  assignmentId: string;
  workerId: string;
  content: string;
  status: TodoStatus;
  parentId?: string;       // 父 Todo（支持多级嵌套）
  dependsOn?: string[];    // 依赖的其他 Todo
  retryCount: number;
  maxRetries: number;
  output?: string;
  error?: string;
}

// Contract — 协作契约
interface Contract {
  id: string;
  missionId: string;
  producerAssignmentId: string;
  consumerAssignmentId: string;
  type: 'file' | 'api' | 'data';
  specification: string;
  status: 'pending' | 'fulfilled' | 'broken';
}
```

---

## 六、关键设计决策

| # | 决策 | 选择 | 理由 |
| - | ---- | ---- | ---- |
| 1 | 编排模式 | ReAct（推理+行动循环） | 单次 LLM 调用 + 工具循环，天然支持多步规划 |
| 2 | Worker 调度 | 非阻塞 `dispatch_task` | Orchestrator 不等待 Worker，避免 LLM 超时 |
| 3 | 消息协议 | `StandardMessage` 统一格式 | 一致的 UI 渲染体验，避免特殊处理分支 |
| 4 | 任务管理 | Mission → Assignment → Todo 三级 | 天然映射到编排者-工人-步骤的层次结构 |
| 5 | 上下文隔离 | 按 Mission 隔离 SharedContextPool | 不同任务互不污染 |
| 6 | Worker 隔离 | 同类型串行、不同类型并行 | 避免文件写入冲突，最大化并行度 |
| 7 | 门面模式 | MessageHub 封装消息系统 | 上层只需调用语义化 API，无需了解消息协议细节 |
| 8 | 画像系统 | ProfileLoader + GuidanceInjector | Worker 可定制角色特性，提高任务精度 |

---

## 七、文件索引

```text
src/
├── orchestrator/                    # 编排系统
│   ├── core/                        # 核心组件
│   │   ├── mission-driven-engine.ts # MDE — 核心编排引擎
│   │   ├── mission-orchestrator.ts  # MissionOrchestrator — Worker 池 + Mission 规划
│   │   ├── dispatch-manager.ts      # DispatchManager — 统一调度
│   │   ├── dispatch-batch.ts        # DispatchBatch — 批次/依赖管理
│   │   ├── worker-pipeline.ts       # WorkerPipeline — 统一执行管道
│   │   ├── message-hub.ts           # MessageHub — 统一消息中心（门面）
│   │   ├── message-factory.ts       # MessageFactory — 消息构造
│   │   ├── message-pipeline.ts      # MessagePipeline — 去重/节流
│   │   ├── message-bus.ts           # MessageBus — 事件发射
│   │   ├── supplementary-instruction-queue.ts  # 补充指令队列
│   │   └── executors/
│   │       └── planning-executor.ts # PlanningExecutor — 一级 Todo 创建
│   │
│   ├── worker/                      # Worker 层
│   │   ├── autonomous-worker.ts     # AutonomousWorker — 自主 Worker
│   │   └── worker-session.ts        # WorkerSessionManager — Session 管理
│   │
│   ├── mission/                     # Mission 领域模型
│   │   ├── types.ts                 # 类型定义
│   │   ├── mission-storage.ts       # Mission 持久化
│   │   ├── assignment-manager.ts    # Assignment 管理
│   │   ├── contract-manager.ts      # Contract 管理
│   │   ├── state-mapper.ts          # 状态映射
│   │   └── data-normalizer.ts       # 数据规范化
│   │
│   ├── profile/                     # Worker 画像系统
│   │   ├── profile-loader.ts        # 加载配置
│   │   ├── guidance-injector.ts     # 任务引导注入
│   │   ├── assignment-resolver.ts   # 任务分配解析
│   │   ├── category-resolver.ts     # 分类解析
│   │   └── prompt-builder.ts        # 提示词构建
│   │
│   ├── prompts/                     # 提示词模板
│   │   ├── orchestrator-prompts.ts  # 编排器统一系统提示词
│   │   └── intent-classification.ts # 意图分类提示词
│   │
│   ├── protocols/                   # 通信协议
│   │   ├── worker-report.ts         # Worker 汇报协议
│   │   └── types.ts                 # 协议类型
│   │
│   ├── recovery/                    # 恢复机制
│   │   └── profile-aware-recovery-handler.ts
│   ├── review/                      # 审查机制
│   │   └── profile-aware-reviewer.ts
│   ├── wisdom/                      # 经验积累
│   │   └── wisdom-extractor.ts
│   ├── lsp/                         # LSP 集成
│   │   └── lsp-enforcer.ts
│   │
│   ├── intent-gate.ts               # 意图门控
│   ├── verification-runner.ts       # 验证执行器
│   └── execution-stats.ts           # 执行统计
│
├── context/                         # 上下文管理系统
│   ├── context-manager.ts           # 三层上下文管理器
│   ├── context-assembler.ts         # 上下文组装器
│   ├── shared-context-pool.ts       # 跨 Worker 共享上下文池
│   ├── file-summary-cache.ts        # 文件摘要缓存
│   ├── memory-document.ts           # Memory 文档
│   ├── context-compressor.ts        # 上下文压缩
│   └── truncation-utils.ts          # 截断工具
│
├── todo/                            # Todo 管理系统
│   ├── todo-manager.ts              # 统一 Todo 管理器
│   ├── todo-repository.ts           # Todo 持久化
│   └── types.ts                     # Todo 类型定义
│
├── services/                        # 业务服务
│   └── direct-execution-service.ts  # 直接 Worker 执行服务
│
├── ui/                              # UI 层
│   ├── webview-provider.ts          # WebviewProvider — UI 桥梁
│   └── webview-svelte/              # Svelte 前端
│       └── src/lib/message-handler.ts
│
├── events.ts                        # 全局事件系统（globalEventBus）
├── protocol/                        # 消息协议
│   └── message-protocol.ts          # StandardMessage 协议定义
├── knowledge/                       # 项目知识库
│   └── project-knowledge-base.ts
├── session/                         # 会话管理
│   └── unified-session-manager.ts
├── snapshot-manager.ts              # SnapshotManager
└── extension.ts                     # VSCode 扩展入口
```

---

> **文档结束** — 本文档基于源码审计生成，反映 MAGI (MultiCLI) 编排系统的当前架构状态。与 `docs/architecture/optimization-plan.md` 配合阅读，了解已知问题和优化方向。
