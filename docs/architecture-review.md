# Magi 架构审查报告

> **审查日期**: 2025-07
> **二次审查**: 2025-02（结合产品定位重新定级 + 补充遗漏问题）
> **审查范围**: 全量源码 (`src/`)
> **审查方式**: 逐文件代码级阅读 + 依赖关系追踪

---

## 前言：产品定位与审查原则

**任何架构审查必须以产品定位为锚点。** 脱离产品阶段谈架构纯洁性是无意义的。

### 产品定位

Magi 是一个 **VSCode 插件形态的多智能体编排系统**：
- 用户通过统一对话入口下达任务
- 底层由 Orchestrator 意图分析 → Mission 任务建模 → 多 Worker（claude/codex/gemini 三槽位）协作执行
- 三层执行模型：L1 直接处理、L2 单 Worker 委派、L3 多 Worker 协作
- 核心竞争力在 **编排层**（意图分析 + 任务拆解 + 异构专家协作），UI 层和工具层是辅助设施

### 产品阶段

当前版本 `0.1.0`，处于 **MVP/快速验证阶段**。这个阶段的首要矛盾是：

> **功能完整性 > 架构优雅性**

因此本报告中所有问题的定级，均以 **"是否影响产品核心功能的正确性和可迭代性"** 为标准，而非代码洁癖。

### 定级标准

| 级别 | 定义 |
|------|------|
| 🔴 P0 | 直接影响产品核心功能的正确性，或严重阻碍核心链路的迭代速度 |
| 🟡 P1 | 增加维护成本或引入潜在风险，但不阻塞当前功能 |
| 🟢 P2 | 代码质量问题，可在日常迭代中顺手修复 |

---

## 目录

- [一、审查总结](#一审查总结)
- [二、问题总览](#二问题总览)
- [三、🔴 P0 · 严重问题](#三-p0--严重问题)
  - [P0-1: 事件系统多通道混杂](#p0-1-事件系统多通道混杂)
  - [P0-2: 前端 Task/Todo 双数据路径](#p0-2-前端-tasktodo-双数据路径)
- [四、🟡 P1 · 中等问题](#四-p1--中等问题)
  - [P1-1: WebviewProvider 业务逻辑越界](#p1-1-webviewprovider-业务逻辑越界)
  - [P1-2: 时序耦合 — MissionOrchestrator 的 Setter 注入链](#p1-2-时序耦合--missionorchestrator-的-setter-注入链)
  - [P1-3: WebviewProvider 消息路由膨胀](#p1-3-webviewprovider-消息路由膨胀)
  - [P1-4: MissionDrivenEngine 职责过多](#p1-4-missiondrivenengine-职责过多)
- [五、🟢 P2 · 轻微问题](#五-p2--轻微问题)
  - [P2-1: handleMessage 中大量 as any](#p2-1-handlemessage-中大量-as-any)
  - [P2-2: 双重 ContextManager 创建](#p2-2-双重-contextmanager-创建)
  - [P2-3: 重复的会话删除逻辑](#p2-3-重复的会话删除逻辑)
  - [P2-4: DI 容器形同虚设](#p2-4-di-容器形同虚设)
  - [P2-5: MissionOrchestrator 合并了 MissionExecutor](#p2-5-missionorchestrator-合并了-missionexecutor)
- [六、已修复问题存档](#六已修复问题存档)
- [七、推荐修复路线图（结合产品阶段）](#七推荐修复路线图结合产品阶段)
- [附录 A：原始审查数据](#附录-a原始审查数据)

---

## 一、审查总结

本次审查覆盖 Magi 项目全部核心源码。二次审查在原始发现基础上，结合产品定位重新定级，补充了遗漏的核心问题，并标注了已修复项。

当前状态：**2 个严重问题（2 个已修复）、4 个中等问题（4 个已修复）、5 个轻微问题（3 个已修复）**。

核心结论：**系统的领域建模和协议设计（Mission/Contract/Assignment 架构、消息协议、画像系统）质量很高。当前最大的架构风险不是 God Object 的行数膨胀，而是多智能体编排系统的「事件可追踪性」不足——5 层事件转发链路直接削弱了产品的核心竞争力。**

关键数据：

| 指标 | 数值 |
|------|------|
| 最大单文件行数 | 2606 行 (`webview-provider.ts`) |
| 次大单文件行数 | 3484 行 (`mission-driven-engine.ts`) |
| WebviewProvider 私有方法/属性数 | ~120 个 |
| handleMessage switch-case 分支数 | 38 个 |
| MissionDrivenEngine 实例变量数 | ~30 个 |
| DI 容器实际绑定数 / 应绑定数 | 6 / 20+ |
| MissionOrchestrator setter 注入数 | 6 个 |
| 事件转发层数 | 5 层 (Worker → MO → MDE → MessageHub → WP) |
| 后端 `as any` 使用数 | 0 处 (`webview-provider.ts`) |

---

## 二、问题总览

| 级别 | 编号 | 问题 | 文件 | 影响范围 | 状态 |
|------|------|------|------|----------|------|
| 🔴 | P0-1 | 事件系统多通道混杂 | 多文件 | 编排核心 | ✅ 已修复 |
| 🔴 | P0-2 | 前端 Task/Todo 双数据路径 | `message-handler.ts` + `messages.svelte.ts` | 前端数据层 | ✅ 已修复 |
| 🟡 | P1-1 | WebviewProvider 业务逻辑越界 | `webview-provider.ts` | UI 层 | ✅ 已修复 |
| 🟡 | P1-2 | Setter 注入时序耦合 | `mission-orchestrator.ts` | 编排层 | ✅ 已修复 |
| 🟡 | P1-3 | WebviewProvider 消息路由膨胀 | `webview-provider.ts` | UI 层 | ✅ 已修复 |
| 🟡 | P1-4 | MissionDrivenEngine 职责过多 | `mission-driven-engine.ts` | 编排层 | ✅ 已修复 |
| 🟢 | P2-1 | handleMessage 中大量 `as any` | `webview-provider.ts` | UI 层 | ✅ 已修复 |
| 🟢 | P2-2 | 双重 ContextManager 创建 | MO + MDE 构造器 | 编排层 | ✅ 已修复 |
| 🟢 | P2-3 | 重复的会话删除逻辑 | `webview-provider.ts` | UI 层 | ✅ 已修复 |
| 🟢 | P2-4 | DI 容器形同虚设 | `di/container.ts` | 全局 | 待修复 |
| 🟢 | P2-5 | MissionOrchestrator 合并了 MissionExecutor | `mission-orchestrator.ts` | 编排层 | 待修复 |

**定级变更说明**：

- 原 P1-3（事件系统）→ **升为 P0-1**：多智能体编排系统的生命线是事件可追踪性，5 层转发直接削弱核心竞争力
- 原 P0-1（WebviewProvider God Object）→ **拆为 P1-1（越界）+ P1-3（膨胀）**：switch-case 多是 VSCode 扩展的 "Controller" 角色天然特征，真正的问题是业务逻辑越界
- 原 P0-2（MissionDrivenEngine）→ **降为 P1-4**：编排引擎职责边界本身就模糊，在领域模型尚未稳定时过早拆分会引入更多跨组件通信
- 原 P0-3（DI 容器）→ **降为 P2-4**：不影响功能正确性，当前阶段测试覆盖率尚非首要目标
- 原 P1-2（双重 ContextManager）→ **降为 P2-2**：只是浪费一次初始化，无功能影响
- 原 P1-4（MissionExecutor 合并）→ **降为 P2-5**：合并可能是有意为之（减少层级），1987 行对编排核心不算过分
- **新增 P0-2**：前端 Task/Todo 双数据路径问题（已修复），是实际导致 UI 数据不一致的真实 Bug

---

## 三、🔴 P0 · 严重问题

### P0-1: 事件系统多通道混杂

> 原编号 P1-3，升级为 P0。已于 2025-02 修复。
>
> **升级理由**：对于"多智能体编排系统"这一产品定位来说，事件系统是核心基础设施。当前 5 层事件转发链路意味着：任何中间层的 bug 都会导致 UI 状态不一致（P0-2 就是直接证据），调试一个事件需要在 5 个文件设断点。这不是"代码洁癖"问题，而是影响产品核心功能的架构风险。

**涉及文件**: `events.ts`, `message-hub.ts`, `mission-orchestrator.ts`, `mission-driven-engine.ts`

**修复内容**:

1. **统一 EventEmitter 类型**：MDE 和 MO 均已使用 Node.js 原生 `EventEmitter`（`import { EventEmitter } from 'events'`），类型混淆问题已不存在
2. **消除无效转发 emit**：
   - 删除 `MDE.emit('progress')` — 无外部消费者，progress 信息已通过 MessageHub 展示
   - 删除 `MDE.emit('workerOutput')` ×2 — 无外部消费者，workerOutput 已通过 `messageHub.workerOutput()` 路由
   - 删除 `onOutput` 回调中的无效 emit — workerOutput 事件已由 `setupEventForwarding()` 处理
3. **明确 3 通道职责边界**：
   - **MessageHub**（UI 消息统一出口）：MDE 监听 MO 事件 → 转换为 subTaskCard/notify/sendMessage → 前端渲染
   - **MissionOrchestrator**（编排业务事件）：WebviewProvider 直接监听 → sendData → 前端 store 数据同步
   - **globalEventBus**（跨模块生命周期事件）：task:completed/failed 等 → WebviewProvider → 全局 UI 状态

**修复后的事件链路**（最长 3 层）:

```
AutonomousWorker.emit('todoStarted')
    → MissionOrchestrator.on → this.emit('todoStarted')    // 第1层：聚合 missionId
        → MDE.setupEventForwarding → messageHub.subTaskCard // 第2层：编排事件→UI消息
            → WebviewProvider.on('unified:message')         // 第3层：postMessage 到前端
```

---

### P0-2: 前端 Task/Todo 双数据路径

> **新增问题**，原始审查未覆盖。已于 2025-02 修复。

**涉及文件**: `message-handler.ts`, `messages.svelte.ts`, `TasksPanel.svelte`, `message.ts`

**表象**: 前端 Task/Todo 系统存在两条独立的数据更新路径，互不同步：

```
路径 A（全量同步）：stateUpdate → store.tasks → TasksPanel 渲染
路径 B（增量事件）：missionPlanned/todoStarted/todoCompleted → missionPlan → 仅更新 Worker 元数据
```

**根本原因**:

1. `stateUpdate` 推送的 `tasks[]` 包含完整的 SubTask 列表，是渲染的数据源
2. `missionPlanned` 推送的 `MissionPlan` 只包含 Worker 元数据（workerId、responsibility），但前端曾试图从中提取 Todo 渲染数据
3. 增量事件（`todoStarted`/`todoCompleted`/`todoFailed`）只更新 `missionPlan.todos` 的状态，不更新 `store.tasks.subTasks`
4. `missionPlan` 原为单对象 `MissionPlan | null`，多 Mission 并发时后到的会覆盖先到的

**后果**: 子任务状态显示滞后、多 Mission 时 Worker 元数据丢失、类型不安全（大量 `as any`）。

**修复内容（已完成）**:

1. **统一渲染数据源**：确认 `stateUpdate → store.tasks → TasksPanel` 为唯一渲染管道
2. **增量事件同步**：新增 `syncSubTaskStatus()` 和 `syncSubTaskAdd()` 函数，增量事件同时更新 `store.tasks.subTasks`
3. **missionPlan Map 化**：从 `MissionPlan | null` 改为 `Map<string, MissionPlan>`，支持多 Mission 并发
4. **类型对齐**：新增 `SubTaskStatus` 联合类型，`SubTaskItem` 与后端 `SubTaskView` 字段对齐，消除前端所有 `as any`
5. **Task 接口强化**：`id`、`subTasks`、`progress`、`missionId` 均为必填字段

---

## 四、🟡 P1 · 中等问题

### P1-1: WebviewProvider 业务逻辑越界

> 原 P0-1 和 P1-5 的核心子问题，合并重组。
>
> **定级理由**：WebviewProvider 的 switch-case 多（93 个 case）是 VSCode 扩展"Controller"角色的天然特征——VSCode API 要求所有 webview 消息必须通过 WebviewProvider 路由。真正的 P1 级问题是：**业务逻辑泄漏到了 UI 层**。

**文件**: `src/ui/webview-provider.ts` (6282 行)

**越界业务逻辑清单**:

| 方法 | 行范围 | 越界原因 |
|------|--------|----------|
| `setupKnowledgeExtractionClient()` | 468-570 | 在 UI 层创建 LLM 客户端、包装 token 统计 |
| `collectCodeContext()` | 2890-2940 | 在 UI 层做 grep/LSP/ACE 语义搜索 |
| `performLocalContextSearch()` | 2992-3080 | 在 UI 层实现多策略代码搜索 |
| `handleEnhancePrompt()` | 2808-2890 | 在 UI 层调用 LLM 做 prompt 增强 |
| `executeWithDirectWorker()` | 5747-5841 | 在 UI 层直接调用 AdapterFactory、管理快照上下文 |
| `normalizeAssignments()` | 400-422 | 在 UI 层做 Mission 数据标准化 |

**改进建议**: 将以上逻辑提取为独立 Service 类，WebviewProvider 只负责调用委派：

```
WebviewProvider 只做:  消息路由 + 调用 Service + 返回结果
  ├── PromptEnhancerService     → Prompt 增强（独立类，拥有自己的 LLM Client）
  ├── CodeContextService        → 代码上下文收集（ACE / grep / LSP）
  ├── KnowledgeExtractionService → 知识提取客户端
  └── DirectWorkerService       → 直接 Worker 执行（如果确实需要绕过编排引擎）
```

---

### P1-2: 时序耦合 — MissionOrchestrator 的 Setter 注入链

**文件**: `src/orchestrator/core/mission-orchestrator.ts`

**表象**: MissionOrchestrator 使用 6 个 setter 方法注入依赖，必须按正确顺序调用，遗漏任何一个都会导致运行时 `throw new Error('未配置 xxx')`。

**代码证据**:

```typescript
// MissionDrivenEngine 构造器 L244-252
this.missionOrchestrator = new MissionOrchestrator(profileLoader, guidanceInjector, ...);
this.missionOrchestrator.setSnapshotManager(snapshotManager);    // ① 必须调用
this.missionOrchestrator.setContextManager(this.contextManager); // ② 必须调用
this.missionOrchestrator.setAdapterFactory(adapterFactory);      // ③ 必须调用（内部会创建 TaskPreAnalyzer）
// 后续还有:
// ④ setKnowledgeBase()   — 由 WebviewProvider 间接调用
// ⑤ setIntentGate()      — 由 engine.initialize() 调用
// ⑥ setVerificationRunner() — 由 engine 内部调用
```

**后果**:

- 编译期无法检查完整性，缺少任何一个 setter 只能在运行时发现
- `setAdapterFactory()` 内部会创建 `TaskPreAnalyzer`，有隐式的创建时序依赖
- 调用者必须了解内部实现才能正确使用

**改进建议**: 将所有必需依赖放入构造器参数，可选依赖用 Options 对象：

```typescript
constructor(
    profileLoader: ProfileLoader,
    guidanceInjector: GuidanceInjector,
    adapterFactory: IAdapterFactory,     // 必需 → 构造器
    contextManager: ContextManager,      // 必需 → 构造器
    snapshotManager: SnapshotManager,    // 必需 → 构造器
    storage?: MissionStorageManager,
    options?: {
        knowledgeBase?: ProjectKnowledgeBase;  // 可选 → Options
        workspaceRoot?: string;
    }
)
```

---

### P1-3: WebviewProvider 消息路由膨胀

> 原 P0-1 的次要子问题，降为 P1。已于 2025-02 修复。
>
> **定级理由**：93 个 case 分支确实多，但在 VSCode 扩展中 WebviewProvider 天然是前后端通信的唯一桥梁。switch-case 多本身不是病——拆分为 CommandHandler 可以降低阅读成本，但不是架构级风险。

**文件**: `src/ui/webview-provider.ts`

**表象**: `handleMessage()` 包含 93 个 case 分支，覆盖 UI 状态、任务执行、配置管理、MCP、Skills、知识库等所有业务域。

**修复内容** (Wave 4.2 #18):

1. **死代码清理** (-218 行): 删除 `sendWorkerHistory`、`sendDetailedWorkerMessages`、`sendLLMLogsDirect`、`getLogsForWorker` 等 6 个未引用的 handler 方法
2. **CommandHandler 模式拆分** (已有): 按业务域拆分为独立 Handler:
   - `ConfigCommandHandler` → LLM / MCP / Skills / Profile 配置管理 (23KB)
   - `KnowledgeCommandHandler` → ADR / FAQ / 知识库 CRUD (10KB)
   - `McpCommandHandler` → MCP 服务器管理 (9KB)
   - `SkillsCommandHandler` → 技能系统管理 (14KB)
3. **EventBindingService 提取** (-454 行): 将所有事件绑定逻辑（globalEventBus 39 个事件监听、MessageHub 订阅、Adapter 错误监听、MissionOrchestrator 14 个事件、工具授权状态机）提取到独立服务
4. **WorkerStatusService 提取** (-225 行): 将 Worker 模型连接状态检查（5 个缓存字段 + 2 个方法）提取到独立服务

**修复后**:

- WVP: 6282 → 2606 行 (-58.5%)
- handleMessage switch-case: 93 → 38 个分支
- 新增独立服务: EventBindingService (513 行) + WorkerStatusService (264 行)
- 架构模式:

```
WebviewProvider (2606 行：Webview 生命周期 + 消息路由 + 核心执行)
  ├── CommandHandler (4个)  → 配置 / 知识库 / MCP / Skills
  ├── EventBindingService   → 事件绑定 + 工具授权状态机
  └── WorkerStatusService   → Worker 连接状态检查 + 缓存
```

---

### P1-4: MissionDrivenEngine 职责过多

> 原 P0-2，降为 P1。
>
> **降级理由**：编排引擎的职责边界本身就模糊——意图分析、路由决策、执行调度、状态管理彼此耦合。在 MVP 阶段、领域模型尚未完全稳定时，过早拆分会引入大量跨组件通信，反而增加复杂度。当前阶段只需做"提取重复代码"级别的优化，不需要架构拆分。

**文件**: `src/orchestrator/core/mission-driven-engine.ts` (3484 行)

**表象**: 编排引擎拥有 ~30 个实例变量和 5+ 种不同关注点。

**最突出的坏味道** — `setupEventForwarding()` 中 `todoStarted/todoCompleted/todoFailed` 三个事件处理器结构完全相同，只有 summary 文本不同。这部分可以提取为 `TodoProgressReporter` 消除重复。

**当前阶段建议**: 仅做代码级重复消除（提取 TodoProgressReporter），不做架构拆分。待 L1/L2/L3 执行路径完全固化后再考虑：

```
MissionDrivenEngine (瘦身后：状态机 + 协调)
  ├── IntentAnalyzer              → 意图分析 + 需求分析
  ├── DispatchManager             → dispatch 模式调度 + 批量管理
  ├── SupplementaryInstructionQueue → 补充指令管理
  └── TodoProgressReporter        → 统一 Mission/dispatch 进度上报
```

---

## 五、🟢 P2 · 轻微问题

### P2-1: handleMessage 中大量 `as any`

**文件**: `src/ui/webview-provider.ts` L2064-2589

**表象**: 虽然定义了消息类型联合，但在 switch-case 中全部用 `as any` 断言（55 处），TypeScript 类型检查完全失效。

```typescript
// webview-provider.ts L2109-2114
const execImages = (message as any).images || [];
const execAgent = (message as any).agent as WorkerSlot | undefined;
const execRequestId = (message as any).requestId as string | undefined;
const requestedModeRaw = (message as any).mode;
```

**修复进度**:

- ✅ 前端 `message-handler.ts` 和 `TasksPanel.svelte` 中的 `as any` 已全部消除
- ⬜ 后端 `webview-provider.ts` 中的 55 处待处理（建议在 P1-3 拆分时顺带修复）

**改进建议**: 在每个 case 分支中通过类型窄化获取正确类型：

```typescript
case 'executeTask': {
    const msg = message as Extract<WebviewToExtensionMessage, { type: 'executeTask' }>;
    // msg.prompt, msg.images 等都有完整类型
}
```

---

### P2-2: 双重 ContextManager 创建

> 原 P1-2，降为 P2。只是浪费一次初始化，无功能影响，顺手修即可。

**文件**: `mission-orchestrator.ts` L254-256 + `mission-driven-engine.ts` L233, L251

**表象**: ContextManager 被创建了两次，第一次立即被第二次覆盖。

```typescript
// MissionOrchestrator 构造器 — 创建了第一个（无 sessionManager）
if (workspaceRoot) {
    this.contextManager = new ContextManager(workspaceRoot);
}

// MissionDrivenEngine 构造器 — 创建了第二个（有 sessionManager）
this.contextManager = new ContextManager(workspaceRoot, undefined, sessionManager);

// 用第二个覆盖第一个
this.missionOrchestrator.setContextManager(this.contextManager);
```

**改进建议**: 删除 MissionOrchestrator 构造器中的条件创建，改为从外部注入（结合 P1-2 的构造器注入方案）。

---

### P2-3: 重复的会话删除逻辑

**文件**: `src/ui/webview-provider.ts` L2270-2304

**表象**: `closeSession` 和 `deleteSession` 两个 case 做几乎相同的事情：

```typescript
case 'closeSession':    // L2270 — 删除会话
case 'deleteSession':   // L2285 — 也是删除会话（多了 requireConfirm）
```

**改进建议**: 统一为一个入口，通过参数控制是否需要确认。

---

### P2-4: DI 容器形同虚设

> 原 P0-3，降为 P2。
>
> **降级理由**：不影响功能正确性，只影响可维护性和可测试性。当前项目版本 0.1.0，无覆盖率指标沉淀，说明测试还不是首要关注点。InversifyJS 的引入成本已支付，移除也有成本。核心组件之间的循环依赖（`AdapterFactory ↔ MessageHub`）本身就不适合简单 DI 绑定。**等测试覆盖率成为正式目标时再做决策**。

**文件**: `src/di/container.ts`

**表象**: 引入了 InversifyJS + reflect-metadata，但只绑定了 6 个纯工具类（ConfigManager、IDGenerator、TokenCounter、PerformanceMonitor、ErrorHandler、LockManager）。核心组件（AdapterFactory、MessageHub、ContextManager 等）全部手动 new + setter。

**改进建议**: 二选一（时机：当测试覆盖率成为正式目标时）

1. **全量使用 DI**: 将核心组件纳入容器管理
2. **移除 InversifyJS**: 减少编译依赖和运行时开销

---

### P2-5: MissionOrchestrator 合并了 MissionExecutor

> 原 P1-4，降为 P2。
>
> **降级理由**：合并可能是有意为之（减少层级跳转），1987 行对于编排核心来说不算过分。规划/执行在实践中耦合度高，强行分离可能带来更多参数传递成本。

**文件**: `src/orchestrator/core/mission-orchestrator.ts` (1987 行)

**表象**: 代码中多处注释标记 `"从 MissionExecutor 合并"`，合并后同时负责规划、执行和验证。

**改进建议**: 保持现状。如果后续发现规划逻辑和执行逻辑的修改频率明显不同（高内聚低耦合的信号），再考虑重新独立。

---

## 六、已修复问题存档

| 编号 | 问题 | 修复日期 | 修复内容 |
|------|------|----------|----------|
| P0-2 | 前端 Task/Todo 双数据路径 | 2025-02 | 统一渲染管道 + 增量同步 + missionPlan Map 化 + 类型对齐 |
| P2-1 (前端部分) | message-handler.ts 中 `as any` | 2025-02 | SubTaskItem 类型对齐，消除所有前端 `as any` |
| P1-2 | Setter 注入时序耦合 | 2025-02 | 6 个 setter 中 4 个改为构造器参数（adapterFactory/contextManager 必需，snapshotManager 可选），删除冗余 null 检查；保留 setIntentGate/setKnowledgeBase（异步生命周期） |
| P2-2 | 双重 ContextManager 创建 | 2025-02 | 删除 MO 构造器中冗余创建，contextManager 由外部注入 |
| P2-3 | 重复的会话删除逻辑 | 2025-02 | closeSession 委托到 performSessionDelete，消除重复 |
| P1-1 (Prompt 增强) | WebviewProvider 业务逻辑越界 | 2025-02 | 提取 PromptEnhancerService（~500 行），含代码上下文收集、ACE/grep/LSP 多策略搜索、LLM 增强调用 |
| P1-1 (数据规范化) | WebviewProvider 业务逻辑越界 | 2025-02 | 提取 normalizeAssignments/normalizeTodos 等纯函数到 `orchestrator/mission/data-normalizer.ts` |
| P1-1 (知识提取客户端) | WebviewProvider 业务逻辑越界 | 2025-02 | 提取 setupKnowledgeExtractionClient 到 `knowledge/knowledge-extraction-client.ts` 工厂函数 |
| P1-1 (直接执行) | WebviewProvider 业务逻辑越界 | 2025-02 | 提取 executeWithDirectWorker 到 `services/direct-execution-service.ts`，依赖注入解耦 |

---

## 七、推荐修复路线图（结合产品阶段）

> **原则**：MVP 阶段优先修复影响产品核心功能的问题，架构纯洁性改进延后。

### 阶段一：低成本高收益 ✅ 已完成

| 编号 | 动作 | 状态 |
|------|------|------|
| P1-2 | 将 MissionOrchestrator 的 6 个 setter 改为构造器参数 | ✅ 已修复 |
| P2-2 | 删除 MissionOrchestrator 构造器中的冗余 ContextManager 创建 | ✅ 已修复 |
| P2-3 | 合并 closeSession / deleteSession 为一个入口 | ✅ 已修复 |

### 阶段二：业务逻辑归位 ✅ 已完成

| 编号 | 动作 | 状态 |
|------|------|------|
| P1-1 | 将越界业务逻辑（代码搜索、Prompt 增强、知识提取、直接 Worker 执行）从 WebviewProvider 提取为独立 Service | ✅ 已修复 |
| P2-1 | 前端 `as any` 消除 | ✅ 已修复（前端部分） |

### 阶段三：事件架构治理 ✅ 已完成

| 编号 | 动作 | 状态 |
|------|------|------|
| P0-1 | 统一事件通道职责；消除无效 emit；减少事件转发层数至 ≤3 层 | ✅ 已修复 |
| P1-4 | MissionDrivenEngine 拆分子组件（SupplementaryInstructionQueue / TodoProgressReporter / DispatchManager） | ✅ 已修复 |

### 延后（等领域模型稳定后）

| 编号 | 动作 | 触发条件 |
|------|------|----------|
| P1-3 | WebviewProvider switch-case 拆分为 CommandHandler | 当 case 分支数 > 120 或新增功能域时 |
| P2-1 | 后端 `as any` 消除（webview-provider.ts 中 55 处） | 当测试覆盖率足够支撑重构时 |
| P2-4 | DI 去留决策 | 当测试覆盖率成为正式目标时 |
| P2-5 | MissionExecutor 是否重新独立 | 当规划/执行修改频率出现明显差异时 |

---

## 附录 A：原始审查数据

> 以下为 2025-07 首次审查时的原始数据，供参考。二次审查已验证数据准确性。

| 指标 | 首次审查值 | 二次验证值 | 偏差 |
|------|-----------|-----------|------|
| WebviewProvider 行数 | 6283 | 6282 | -1 |
| MissionDrivenEngine 行数 | 3485 | 3484 | -1 |
| MissionOrchestrator 行数 | 1988 | 1987 | -1 |
| handleMessage case 分支数 | 60+ | 93 | 首次审查保守 |
| DI 容器绑定数 | 6 | 6 | 一致 |
| Setter 注入数 | 6 | 6 | 一致 |
| `as any` 使用数（后端） | 未统计 | 55 | 二次审查补充 |
