# MultiCLI 编排系统增强提案

> 版本: 1.1
> 日期: 2025-01-31
> 状态: 提案
> 参考: oh-my-opencode 项目 (https://github.com/code-yeongyu/oh-my-opencode)

---

## 目录

1. [背景与目标](#1-背景与目标)
   - [1.1 背景](#11-背景)
   - [1.2 目标](#12-目标)
   - [1.3 非目标](#13-非目标)
   - [1.4 重构原则：单一正确路径](#14-重构原则单一正确路径)
2. [参考项目分析](#2-参考项目分析)
3. [当前架构评估](#3-当前架构评估)
4. [增强提案详解](#4-增强提案详解)
   - [4.1 Session 恢复机制](#41-提案-1-session-恢复机制)
   - [4.2 验证证据机制](#42-提案-2-验证证据机制)
   - [4.3 6-Section 结构化委托提示（增强现有 GuidanceInjector）](#43-提案-3-6-section-结构化委托提示增强现有-guidanceinjector)
   - [4.4 强化分类别约束](#44-提案-4-强化分类别约束)
   - [4.5 Wisdom 累积系统（复用现有知识库）](#45-提案-5-wisdom-累积系统复用现有知识库)
   - [4.6 Wave 并行分组（利用现有 TaskDependencyGraph）](#46-提案-6-wave-并行分组利用现有-taskdependencygraph)
5. [实施路线图](#5-实施路线图)
6. [验收标准](#6-验收标准)

---

## 1. 背景与目标

### 1.1 背景

MultiCLI 已完成 `orchestration-unified-design.md` 中定义的核心架构重构，包括：

- 单路径执行（ASK 直答；需要 Worker 进入 MissionOrchestrator）
- 层级简化（6 层 → 3 层）
- Worker 汇报协议
- 统一消息出口（MessageHub）
- 状态统一（Mission 唯一源）

通过对 oh-my-opencode 项目的分析，发现了一些可借鉴的设计模式，可作为增量优化方向。

### 1.2 目标

在不改变核心架构的前提下，增强以下维度：

| 维度 | 当前状态 | 目标状态 | 优先级 |
|------|----------|----------|--------|
| 失败恢复 | 重建上下文 | Session 复用 | P0 |
| 验证机制 | 隐式验证 | 显式证据 | P0 |
| 委托提示 | 自由文本 | 6-Section 结构化（增强现有 GuidanceInjector） | P1 |
| Prompt 约束 | 通用约束 | 分类别强化约束 | P1 |
| 知识积累 | 无 | Wisdom 累积（复用现有 MemoryDocument/ProjectKnowledgeBase） | P1 |
| 并行控制 | 粗粒度 | Wave 分组（利用现有 TaskDependencyGraph） | P2 |

### 1.3 非目标

- 不改变现有架构层级
- 不改变 IntentGate 分流逻辑
- 不改变 MessageHub 消息协议
- 不引入新的状态管理系统

### 1.4 重构原则：单一正确路径

> **本设计是唯一正确流程，不存在"兼容旧逻辑"的选项。**

在实施过程中，必须严格遵循以下原则：

#### 1.4.1 删除而非兼容

发现旧逻辑不符合新设计时，直接删除重构，不保留旧代码。

```typescript
// ❌ 错误：保留旧逻辑做兼容
function execute(options: ExecutionOptions) {
  if (options.useLegacy) {
    return this.legacyExecute(options);  // 不要保留
  }
  return this.newExecute(options);
}

// ✅ 正确：只保留新逻辑
function execute(options: ExecutionOptions) {
  return this.newExecute(options);
}
// 删除 legacyExecute 方法
```

#### 1.4.2 单一代码路径

同一功能只有一套实现，禁止 if/else 分支兼容。

```typescript
// ❌ 错误：多个代码路径
class Orchestrator {
  execute(mission: Mission) {
    if (this.useNewFlow) {
      return this.executeNewFlow(mission);
    } else if (this.useExperimentalFlow) {
      return this.executeExperimentalFlow(mission);
    } else {
      return this.executeLegacyFlow(mission);
    }
  }
}

// ✅ 正确：单一代码路径
class Orchestrator {
  execute(mission: Mission) {
    return this.executeFlow(mission);  // 唯一实现
  }
}
```

#### 1.4.3 不留技术债

不创建 Facade、Adapter、兼容层等过渡代码。

```typescript
// ❌ 错误：创建兼容层
class LegacyWorkerAdapter implements Worker {
  constructor(private legacyWorker: OldWorker) {}

  execute(assignment: Assignment) {
    // 转换参数以兼容旧 Worker
    const oldParams = this.convertToLegacyParams(assignment);
    return this.legacyWorker.run(oldParams);
  }
}

// ✅ 正确：直接使用新实现
class AutonomousWorker implements Worker {
  execute(assignment: Assignment) {
    // 新实现，不兼容旧接口
    return this.doExecute(assignment);
  }
}
// 删除 OldWorker 和 LegacyWorkerAdapter
```

#### 1.4.4 测试驱动验证

用测试保证新逻辑正确，而非依赖旧代码兜底。

```typescript
// ✅ 正确：测试驱动
describe('AutonomousWorker', () => {
  it('should execute assignment with structured guidance', async () => {
    const worker = new AutonomousWorker(/*...*/);
    const assignment = createTestAssignment();

    const result = await worker.execute(assignment);

    // 验证新逻辑的行为
    expect(result.success).toBe(true);
    expect(result.sessionId).toBeDefined();
    expect(result.evidence).toBeDefined();
  });

  it('should resume from session on failure', async () => {
    // 测试 Session 恢复功能
    const worker = new AutonomousWorker(/*...*/);

    // 第一次执行失败
    const result1 = await worker.execute(assignment, {
      simulateFailure: true
    });
    expect(result1.success).toBe(false);
    expect(result1.sessionId).toBeDefined();

    // 使用 sessionId 恢复执行
    const result2 = await worker.execute(assignment, {
      sessionId: result1.sessionId,
      resumePrompt: 'Fix the error',
    });
    expect(result2.success).toBe(true);
  });
});
```

#### 1.4.5 清理检查清单

每个 Phase 完成后，执行以下检查：

| 检查项 | 通过标准 |
|--------|----------|
| 旧代码删除 | `git diff` 中删除的行数 >= 旧实现的 90% |
| 无兼容分支 | 无 `if (legacy)` / `if (useOld)` 等条件判断 |
| 无过渡层 | 无 `*Adapter` / `*Facade` / `*Compat*` 类 |
| 测试覆盖 | 新功能测试覆盖率 >= 80% |
| 编译通过 | `npm run compile` 无错误 |
| 类型安全 | 无 `any` 类型（除非必要） |

---

## 2. 参考项目分析

### 2.1 oh-my-opencode 架构概览

```
┌─────────────────────────────────────────────────────────────────┐
│  Prometheus (规划者)                                             │
│  ├─ 只做规划，生成 .sisyphus/plans/*.md                          │
│  ├─ 禁止写代码文件（系统级约束）                                   │
│  └─ 采访式需求收集 → 结构化工作计划                               │
├─────────────────────────────────────────────────────────────────┤
│  Sisyphus (编排者)                                               │
│  ├─ 接收计划，执行 delegate_task                                  │
│  ├─ 意图分类 (Trivial/Explicit/Exploratory/Open-ended/Ambiguous) │
│  ├─ Category + Skills 组合派发                                   │
│  └─ Trust But Verify 原则                                        │
├─────────────────────────────────────────────────────────────────┤
│  Junior Workers (执行者)                                         │
│  ├─ 接收 6-Section 结构化任务                                     │
│  ├─ 执行并汇报                                                   │
│  └─ Session 恢复支持                                              │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 核心设计亮点

#### 2.2.1 6-Section Prompt 结构

oh-my-opencode 强制要求每次 `delegate_task` 必须包含完整的 6 个部分：

```
1. TASK: 原子化、具体的目标（单一职责）
2. EXPECTED OUTCOME: 具体的交付物和成功标准
3. REQUIRED TOOLS: 显式工具白名单（防止工具滥用）
4. MUST DO: 详尽的必做事项（不留隐式假设）
5. MUST NOT DO: 预判并阻止危险行为
6. CONTEXT: 文件路径、现有模式、约束条件
```

**示例**：
```
TASK: 为 UserService 类添加 getUserById 方法

EXPECTED OUTCOME:
- 新增 getUserById(id: string): Promise<User | null> 方法
- 方法遵循现有 getUsers() 的错误处理模式
- 包含 JSDoc 注释

REQUIRED TOOLS:
- read_file (读取现有代码)
- edit_file (修改代码)
- file_search (查找相关类型定义)

MUST DO:
- 复用现有的 this.repository.findOne() 方法
- 返回 null 而非抛异常（当用户不存在时）
- 添加参数验证（id 非空检查）

MUST NOT DO:
- 不要修改其他方法
- 不要添加新的依赖
- 不要修改 User 类型定义
- 不要添加缓存逻辑（后续任务）

CONTEXT:
- 目标文件: src/services/user-service.ts
- 类型定义: src/types/user.ts
- 现有模式参考: getUsers() 方法（第 45-60 行）
- 约束: 保持与现有代码风格一致
```

#### 2.2.2 Session 恢复机制

```typescript
// 首次执行
const result = await delegate_task({
  category: "quick",
  prompt: "Implement getUserById..."
});
// 返回: { sessionId: "ses_abc123", success: false, error: "Type error" }

// 失败后恢复（保留完整上下文）
const retryResult = await delegate_task({
  session_id: "ses_abc123",  // 复用 session
  prompt: "Fix: Type error on line 42"  // 只需描述修复
});
// Worker 保留之前的文件读取、探索结果、决策历史
```

**收益**：
- 失败重试节省 70%+ token
- Worker 不需要重新读取文件
- 保留之前的推理过程

#### 2.2.3 Category + Skills 组合

```typescript
// oh-my-opencode 的灵活派发
delegate_task({
  category: "visual-engineering",  // 类别决定基础能力
  load_skills: ["playwright", "browser"],  // 技能注入额外能力
  prompt: "..."
});

// 可用类别
// - visual-engineering: 前端/UI
// - ultrabrain: 复杂推理
// - artistry: 创意设计
// - quick: 简单任务
// - writing: 文档编写
```

#### 2.2.4 Trust But Verify

```typescript
// Sisyphus 的验证流程
const result = await delegate_task(...);

// 验证 Worker 声明
// - 文件是否真的被修改？
// - 测试是否真的通过？
// - 输出是否符合预期？

if (!verifyWorkerClaims(result)) {
  // 使用 session_id 恢复并修复
  await delegate_task({
    session_id: result.sessionId,
    prompt: "Verification failed: {具体问题}. Fix."
  });
}
```

### 2.3 与 MultiCLI 对比

| 维度 | oh-my-opencode | MultiCLI 当前 | 差距分析 |
|------|----------------|---------------|----------|
| Prompt 结构 | 6-Section 强制 | 自由文本 | 需增强 |
| Session 恢复 | 原生支持 | 无 | 需新增 |
| 工具控制 | 白名单 | 全量暴露 | 需增强 |
| 验证机制 | 显式证据 | 隐式 | 需增强 |
| 类别派发 | Category + Skills | Worker 直接指定 | 可借鉴 |
| 并行控制 | Wave 分组 | 粗粒度 | 可增强 |

---

## 3. 当前架构评估

### 3.1 已完成的优秀设计

根据 `orchestration-unified-design.md`，MultiCLI 已具备：

#### 3.1.1 双轨制执行

```
IntentGate
    │
    ├── ASK → 编排者直答
    ├── DIRECT/EXPLORE → LLM 决策（需要 Worker 则进入 MissionOrchestrator）
    │
    └── TASK → MissionOrchestrator (完整路径)
```

#### 3.1.2 Worker 汇报协议

```typescript
interface WorkerReport {
  type: 'progress' | 'question' | 'completed' | 'failed';
  progress?: { currentStep, completedSteps, remainingSteps, percentage };
  result?: { modifiedFiles, createdFiles, summary };
  question?: { content, options, blocking };
  error?: string;
}

interface OrchestratorResponse {
  action: 'continue' | 'adjust' | 'abort' | 'answer';
  adjustment?: { newInstructions, skipSteps, addSteps };
  answer?: string;
}
```

#### 3.1.3 降级机制

```
通用/后端任务: Claude → Codex → Gemini → 报告失败
前端/UI任务:   Gemini → Claude → Codex → 报告失败
简单/批量任务: Codex → Claude → Gemini → 报告失败
```

#### 3.1.4 状态统一

```
Mission (唯一真实源)
  ├── id, goal, status, phase
  ├── Assignments[]
  │     ├── id, workerId, responsibility
  │     ├── status, progress
  │     └── WorkerTodos[]
  │           ├── id, content, status
  │           └── output
  └── Contracts[]
```

### 3.2 可增强的维度

| 维度 | 当前实现 | 增强方向 |
|------|----------|----------|
| `Assignment.guidancePrompt` | 自由文本 | 6-Section 结构化 |
| `AutonomousWorker` 执行 | 无状态 | Session 持久化 |
| 工具调用 | 无限制 | 白名单/黑名单 |
| `WorkerReport.result` | 简单摘要 | 验证证据 |
| `WorkerTodo` 执行 | 依赖数组 | Wave 并行分组 |

---

## 4. 增强提案详解

### 4.1 提案 1: Session 恢复机制

> **优先级: P0** - 解决失败重试时 token 浪费的核心痛点

#### 4.1.1 问题描述

当前 `AutonomousWorker` 每次执行是独立的：

```typescript
// 当前流程
const result1 = await worker.execute(assignment);  // 失败
// result1.success = false, error = "Type error"

// 重试需要完全重新开始
const result2 = await worker.execute(assignment);  // 重建上下文，浪费 token
```

**问题**：
- 失败重试时需要重新读取所有文件
- 之前的探索结果丢失
- 推理过程无法复用
- Token 消耗高

#### 4.1.2 解决方案

为 Worker 添加 Session 持久化能力：

```typescript
// 新增: src/orchestrator/worker/worker-session.ts

/**
 * Worker Session 管理
 *
 * 用于保存和恢复 Worker 执行上下文
 */
export interface WorkerSession {
  /** Session ID */
  id: string;

  /** 关联的 Assignment ID */
  assignmentId: string;

  /** Worker 类型 */
  workerId: WorkerSlot;

  /** 对话历史（用于 LLM 上下文恢复） */
  conversationHistory: Array<{
    role: 'user' | 'assistant' | 'system';
    content: string;
  }>;

  /** 已读取的文件缓存 */
  readFiles: Map<string, {
    content: string;
    readAt: number;
  }>;

  /** 已完成的 Todo IDs */
  completedTodos: string[];

  /** 执行状态快照 */
  stateSnapshot: {
    currentTodoIndex: number;
    lastError?: string;
    retryCount: number;
  };

  /** 创建时间 */
  createdAt: number;

  /** 最后更新时间 */
  updatedAt: number;
}

/**
 * Session 存储管理器
 */
export class WorkerSessionManager {
  private sessions: Map<string, WorkerSession> = new Map();
  private readonly SESSION_TTL_MS = 30 * 60 * 1000;  // 30 分钟过期

  create(assignmentId: string, workerId: WorkerSlot): WorkerSession { /* ... */ }
  get(sessionId: string): WorkerSession | null { /* ... */ }
  update(sessionId: string, updates: Partial<WorkerSession>): void { /* ... */ }
  delete(sessionId: string): void { /* ... */ }
  cleanup(): void { /* 清理过期 Session */ }
}
```

#### 4.1.3 AutonomousWorker 改造

```typescript
// 改造: src/orchestrator/worker/autonomous-worker.ts

export interface ExecutionOptions {
  /** Session ID（用于恢复执行） */
  sessionId?: string;
  /** 恢复时的额外指令 */
  resumePrompt?: string;
}

export interface AutonomousExecutionResult {
  success: boolean;
  sessionId: string;  // 返回 sessionId 以便后续恢复
  error?: string;
}

export class AutonomousWorker {
  async execute(
    assignment: Assignment,
    options: ExecutionOptions
  ): Promise<AutonomousExecutionResult> {
    // 恢复或创建 Session
    const session = options.sessionId
      ? this.sessionManager.get(options.sessionId) || this.sessionManager.create(...)
      : this.sessionManager.create(...);

    try {
      // 从上次位置继续执行
      const startIndex = session.stateSnapshot.currentTodoIndex;
      // ... 执行逻辑

      return { success: true, sessionId: session.id };
    } catch (error) {
      // 保留 Session 以便恢复
      return { success: false, sessionId: session.id, error: error.message };
    }
  }
}
```

#### 4.1.4 验收标准

- [ ] `WorkerSession` 接口定义完成
- [ ] `WorkerSessionManager` 实现完成
- [ ] `AutonomousWorker.execute()` 支持 `sessionId` 参数
- [ ] 执行结果包含 `sessionId`
- [ ] Session 30 分钟过期清理
- [ ] 恢复执行时对话历史正确恢复
- [ ] 文件缓存复用验证

---

### 4.2 提案 2: 验证证据机制

> **优先级: P0** - 实现 "Trust But Verify" 原则

#### 4.2.1 问题描述

当前 `WorkerReport.result` 只包含简单摘要：

```typescript
result?: {
  modifiedFiles: string[];
  createdFiles: string[];
  summary: string;
};
```

**问题**：
- 编排者无法验证 Worker 的声明
- "Trust But Verify" 无法落地
- 执行结果可信度低

#### 4.2.2 解决方案

扩展 `WorkerReport.result` 包含验证证据：

```typescript
// 改造: src/orchestrator/protocols/worker-report.ts

export interface WorkerReport {
  result?: {
    modifiedFiles: string[];
    createdFiles: string[];
    summary: string;

    /** 验证证据（新增） */
    evidence?: {
      /** 执行的命令及输出 */
      commandsRun?: Array<{
        command: string;
        exitCode: number;
        stdout?: string;
        stderr?: string;
      }>;

      /** 测试结果 */
      testResults?: {
        framework: string;
        total: number;
        passed: number;
        failed: number;
        duration: number;
      };

      /** 类型检查结果 */
      typeCheckResult?: {
        passed: boolean;
        errors?: string[];
      };

      /** 文件变更证据 */
      fileChanges?: Array<{
        path: string;
        action: 'create' | 'modify' | 'delete';
        linesAdded?: number;
        linesRemoved?: number;
      }>;
    };
  };
}
```

#### 4.2.3 验证逻辑

```typescript
// 改造: src/orchestrator/core/mission-orchestrator.ts

private async verifyWorkerReport(report: WorkerReport): Promise<{
  verified: boolean;
  issues: string[];
}> {
  const issues: string[] = [];
  const evidence = report.result?.evidence;

  if (!evidence) {
    return { verified: false, issues: ['缺少验证证据'] };
  }

  // 验证文件变更
  for (const file of report.result?.modifiedFiles || []) {
    if (!await this.fileExists(file)) {
      issues.push(`声明修改的文件不存在: ${file}`);
    }
  }

  // 验证测试结果
  if (evidence.testResults?.failed > 0) {
    issues.push(`测试失败: ${evidence.testResults.failed}/${evidence.testResults.total}`);
  }

  return { verified: issues.length === 0, issues };
}
```

#### 4.2.4 验收标准

- [ ] `WorkerReport.result.evidence` 字段定义
- [ ] `AutonomousWorker.collectEvidence()` 实现
- [ ] 命令执行记录收集
- [ ] 文件变更证据收集
- [ ] 测试结果解析
- [ ] `MissionOrchestrator.verifyWorkerReport()` 实现

---

### 4.3 提案 3: 6-Section 结构化委托提示

> **优先级: P1** - 增强现有 GuidanceInjector，标准化任务委托格式

#### 4.3.1 问题描述

当前 `Assignment.guidancePrompt` 为自由文本格式，存在以下问题：

- 任务描述可能遗漏关键信息
- 没有明确的成功标准
- 缺乏禁止行为的预防
- Worker 容易自作主张（AI Slop）

#### 4.3.2 现有机制分析

**⚠️ 重要发现：现有 `GuidanceInjector` 已有完整的 6-section 结构！**

```typescript
// 现有: src/orchestrator/profile/guidance-injector.ts
export class GuidanceInjector {
  buildWorkerPrompt(profile: WorkerProfile, context: InjectionContext): string {
    // 已有 6 个 section：
    // 1. buildRoleSection      - 角色定位
    // 2. buildFocusSection     - 专注领域
    // 3. buildConstraintsSection - 行为约束
    // 4. buildCollaborationSection - 协作规则
    // 5. buildContractSection  - 功能契约
    // 6. buildOutputSection    - 输出要求
  }
}
```

因此，本提案**不新建 `StructuredPromptBuilder`**，而是**增强现有 `GuidanceInjector`**。

#### 4.3.3 增强方案

在现有 6-section 基础上，增加任务导向的结构化信息：

```typescript
// 增强: src/orchestrator/profile/guidance-injector.ts

export class GuidanceInjector {
  /**
   * 构建完整的任务 Prompt（增强版）
   * 组合：引导 Prompt + 任务结构化信息 + 上下文
   */
  buildFullTaskPrompt(
    profile: WorkerProfile,
    context: InjectionContext,
    taskInfo?: TaskStructuredInfo
  ): string {
    const sections: string[] = [];

    // 1. 现有引导 Prompt（6 sections）
    sections.push(this.buildWorkerPrompt(profile, context));

    // 2. 新增：任务结构化信息
    if (taskInfo) {
      sections.push(this.buildTaskStructuredSection(taskInfo));
    }

    // 3. 项目上下文（如果有）
    if (context.additionalContext) {
      sections.push(`## 项目上下文\n${context.additionalContext}`);
    }

    return sections.join('\n\n');
  }

  /**
   * 新增：构建任务结构化信息
   * 包含 EXPECTED OUTCOME、MUST DO、MUST NOT DO
   */
  private buildTaskStructuredSection(taskInfo: TaskStructuredInfo): string {
    const parts: string[] = [];

    // 预期结果
    if (taskInfo.expectedOutcome) {
      parts.push(`## 预期结果\n${taskInfo.expectedOutcome.map(o => `- ${o}`).join('\n')}`);
    }

    // 必须做
    if (taskInfo.mustDo?.length) {
      parts.push(`## 必须遵守\n${taskInfo.mustDo.map(m => `- ${m}`).join('\n')}`);
    }

    // 禁止做
    if (taskInfo.mustNotDo?.length) {
      parts.push(`## 禁止行为\n${taskInfo.mustNotDo.map(m => `- ${m}`).join('\n')}`);
    }

    return parts.join('\n\n');
  }
}

interface TaskStructuredInfo {
  expectedOutcome?: string[];
  mustDo?: string[];
  mustNotDo?: string[];
  relatedDecisions?: string[];  // 从 MemoryDocument 获取
  pendingIssues?: string[];     // 从 MemoryDocument 获取
}
```

#### 4.3.4 与现有知识库集成

从 `ContextManager` 获取上下文信息注入到 `TaskStructuredInfo`：

```typescript
// 在 ExecutionCoordinator 中
async prepareTaskInfo(assignment: Assignment): Promise<TaskStructuredInfo> {
  // 从现有 ContextManager 获取上下文
  const context = await this.contextManager.getRecentContext(2000);

  // 从现有 MemoryDocument 获取决策和问题
  const memoryDoc = this.contextManager.getMemoryDocument();

  return {
    expectedOutcome: this.inferOutcome(assignment.category),
    mustDo: this.inferMustDo(assignment),
    mustNotDo: this.inferMustNotDo(assignment.category),
    relatedDecisions: memoryDoc?.decisions.slice(-3).map(d => d.description) || [],
    pendingIssues: memoryDoc?.pendingIssues.slice(-2).map(i => i.description) || [],
  };
}
```

#### 4.3.5 示例输出

```markdown
## 角色定位
你是一个资深软件架构师，专注于系统设计、代码质量和可维护性。

## 专注领域
- 优先考虑代码的可维护性和扩展性
- 在修改前先分析影响范围和依赖关系

## 注意事项
- 不要进行不必要的重构
- 避免引入新的依赖，除非必要

## 预期结果
- 代码修改完成，无语法错误
- 相关测试通过（如有）

## 必须遵守
- 完成所有列出的 Todo 项
- 保持与现有代码风格一致
- 遵循决策：返回 null 而非抛异常

## 禁止行为
- 不要修改与任务无关的代码
- 不要添加未要求的功能

## 当前任务
为 UserService 类添加 getUserById 方法
```

#### 4.3.6 验收标准

- [ ] `GuidanceInjector.buildTaskStructuredSection()` 实现
- [ ] `TaskStructuredInfo` 接口定义
- [ ] 与现有 `ContextManager` 集成
- [ ] 从 `MemoryDocument` 获取 decisions 和 pendingIssues
- [ ] 单元测试覆盖
- [ ] 生成的 Prompt 长度合理（< 2000 tokens）

---

### 4.4 提案 4: 强化分类别约束

> **优先级: P1** - 增强现有 GuidanceInjector 的约束能力

#### 4.4.1 设计原则

遵循现有设计哲学：**"引导而非限制：通过 Prompt 注入引导 Worker 行为，不限制工具权限"**

现有 `GuidanceInjector` 已有 6 个 section，本提案聚焦于**强化 `buildConstraintsSection`** 的内容生成。

#### 4.4.2 现有机制

```typescript
// 现有: src/orchestrator/profile/types.ts
interface WorkerProfile {
  guidance: {
    constraints: string[];  // 建议性行为约束
  };
}

// 现有约束示例（claude.ts）
constraints: [
  "不要进行不必要的重构",
  "避免引入新的依赖，除非必要",
  "大规模修改前先与编排者确认"
]
```

#### 4.4.3 增强方案

根据任务分类（CategoryConfig）生成更具针对性的约束：

```typescript
// 改造: src/orchestrator/profile/guidance-injector.ts

private buildConstraintsSection(
  assignment: Assignment,
  category: CategoryType
): string {
  const baseConstraints = this.profile.guidance.constraints;
  const categoryConstraints = this.getCategoryConstraints(category);

  return `
## 注意事项

### 通用约束
${baseConstraints.map(c => `- ${c}`).join('\n')}

### ${category} 任务专项约束
${categoryConstraints.map(c => `- ${c}`).join('\n')}
  `;
}

private getCategoryConstraints(category: CategoryType): string[] {
  const presets: Record<CategoryType, string[]> = {
    'bugfix': [
      '只修复指定的 bug，不要顺便重构',
      '不要添加新功能',
      '保持代码风格一致',
    ],
    'refactor': [
      '不要改变外部行为',
      '每次只重构一个模块',
      '重构前确保测试覆盖',
    ],
    'review': [
      '只分析代码，不要修改',
      '关注逻辑问题而非风格问题',
    ],
    // ... 其他分类
  };
  return presets[category] || [];
}
```

#### 4.4.4 与工具白名单的关系

本提案明确 **不采用硬性工具白名单**。  
遵循产品定位：尽量减少用户操作，让 AI 自主完成任务。  
因此仅使用**提示词层面的引导策略**（分类别约束）来提升行为一致性。

**补充理由（结合产品定位）**

1. **降低用户介入成本**  
   多数用户不愿意频繁授权/选择工具，硬性白名单会引入额外操作与中断，违背“AI 自主完成任务”的定位。

2. **保持工作流连续性**  
   任务链路中断会显著降低完成率。引导策略允许模型在同一流程内自我纠偏，不因权限门槛而停摆。

3. **支持复杂任务的弹性探索**  
   编排系统强调多 Worker 协作与动态分工。硬性权限容易导致“可用工具不足 → 任务阻塞”，引导策略更适配灵活探索与并行协作。

4. **与现有架构一致**  
   当前体系强调 MessageHub 统一出口、Mission 单一状态源。引导策略只影响提示词层，无需引入新的权限系统，避免破坏“单一逻辑路径”。

#### 4.4.5 最终建议（策略选择）

基于 MultiCLI 的产品定位与现有架构，**更合适的策略是“强引导 + 软约束”**：

- **强引导**：分类别约束 + 结构化提示（6-Section）持续强化行为边界。  
- **软约束**：对极少数高风险操作（如删除、覆盖关键文件）采用“轻量提示/二次确认”而非系统级拒绝。

该策略在“高自主性 + 高完成率 + 单一路径”之间取得最佳平衡，更符合当前产品目标。

#### 4.4.6 验收标准

- [ ] `getCategoryConstraints()` 实现
- [ ] 12 个任务分类都有专项约束
- [ ] 约束内容与 CategoryConfig 对应
- [ ] 单元测试覆盖各分类

---

### 4.5 提案 5: Wisdom 累积系统（复用现有知识库）

> **优先级: P1** - 跨任务知识积累，避免重复探索，提升后续任务执行质量

#### 4.5.1 问题描述

当前 Mission 执行过程中产生的知识没有被有效保存和复用：

- Worker 探索的代码模式、发现的问题、做出的决策没有记录
- 后续 Assignment 执行时需要重新探索
- 同类任务的经验无法传递
- 失败的尝试可能被重复

#### 4.5.2 现有知识库分析

**⚠️ 重要发现：系统已有完整的三层知识库架构！**

| 层级 | 组件 | 存储内容 | 生命周期 |
|------|------|----------|----------|
| Layer 1 | `ContextManager` 即时上下文 | 最近几轮对话 | 对话级 |
| Layer 2 | `MemoryDocument` | decisions, pendingIssues, codeChanges | 会话级 |
| Layer 3 | `ProjectKnowledgeBase` | FAQs, ADRs, 代码索引 | 项目级 |

```typescript
// 现有: src/context/memory-document.ts
interface MemoryDocument {
  decisions: DecisionEntry[];      // ✅ 可存储 Wisdom.decisions
  pendingIssues: IssueEntry[];     // ✅ 可存储 Wisdom.issues
  codeChanges: CodeChangeEntry[];  // 代码变更记录
  context: string;                 // ✅ 可存储 Wisdom.learnings
}

// 现有: src/knowledge/project-knowledge-base.ts
class ProjectKnowledgeBase {
  addFAQ(question: string, answer: string): void  // ✅ 跨会话经验
  addADR(adr: ADREntry): void                     // 架构决策记录
}
```

因此，本提案**不新建 `MissionWisdomManager`**，而是**复用现有知识库系统**。

#### 4.5.3 Wisdom 映射方案

| Wisdom 概念 | 映射目标 | 说明 |
|-------------|----------|------|
| `learnings` | `MemoryDocument.context` | 会话级知识，通过追加方式积累 |
| `decisions` | `MemoryDocument.addDecision()` | 直接复用现有方法 |
| `issues` | `MemoryDocument.pendingIssues` | 作为待解决问题记录 |
| 跨会话经验 | `ProjectKnowledgeBase.addFAQ()` | 重要经验持久化为 FAQ |

#### 4.5.4 Wisdom 提取流水线

从 Worker 执行结果中提取知识，存入现有系统：

```typescript
// 增强: src/orchestrator/core/mission-orchestrator.ts

/**
 * Wisdom 提取流水线
 * 从 WorkerReport 提取知识，存入现有知识库
 */
private async extractWisdomFromResult(
  assignmentId: string,
  report: WorkerReport
): Promise<void> {
  const contextManager = this.contextManager;
  const memoryDoc = contextManager.getMemoryDocument();

  if (!memoryDoc) return;

  // 1. 从成功结果中提取 learnings
  if (report.result?.success && report.result.summary) {
    // 追加到 MemoryDocument.context
    const learningContext = this.extractLearnings(report.result.summary);
    if (learningContext) {
      memoryDoc.appendContext(learningContext);
    }
  }

  // 2. 从决策中提取 decisions
  const decisions = this.extractDecisions(report.result?.summary || '');
  for (const decision of decisions) {
    memoryDoc.addDecision({
      description: decision,
      sourceAssignmentId: assignmentId,
      createdAt: Date.now(),
    });
  }

  // 3. 从错误中提取 issues
  if (report.error) {
    memoryDoc.addPendingIssue({
      description: `执行失败: ${report.error}`,
      sourceAssignmentId: assignmentId,
      priority: 'high',
      createdAt: Date.now(),
    });
  }

  // 4. 重要经验持久化到 ProjectKnowledgeBase
  if (report.result?.significantLearning) {
    const knowledgeBase = contextManager.getProjectKnowledgeBase();
    knowledgeBase?.addFAQ(
      `关于 ${report.context}`,
      report.result.significantLearning
    );
  }
}

/**
 * 从 Worker 输出中提取 learnings
 */
private extractLearnings(summary: string): string | null {
  // 检测特定模式
  const patterns = [
    /发现[：:]\s*(.+)/g,
    /注意[：:]\s*(.+)/g,
    /了解到[：:]\s*(.+)/g,
  ];

  const learnings: string[] = [];
  for (const pattern of patterns) {
    let match;
    while ((match = pattern.exec(summary)) !== null) {
      learnings.push(match[1].trim());
    }
  }

  return learnings.length > 0
    ? `\n### 执行中学习到:\n${learnings.map(l => `- ${l}`).join('\n')}`
    : null;
}

/**
 * 从 Worker 输出中提取 decisions
 */
private extractDecisions(summary: string): string[] {
  const patterns = [
    /决定[：:]\s*(.+)/g,
    /选择[：:]\s*(.+)/g,
    /采用[：:]\s*(.+)/g,
  ];

  const decisions: string[] = [];
  for (const pattern of patterns) {
    let match;
    while ((match = pattern.exec(summary)) !== null) {
      decisions.push(match[1].trim());
    }
  }

  return decisions;
}
```

#### 4.5.5 Wisdom 注入

通过现有 `ContextManager` 获取上下文并注入：

```typescript
// 增强: src/orchestrator/profile/guidance-injector.ts

/**
 * 从现有知识库获取 Wisdom 并注入到 Prompt
 */
buildFullTaskPrompt(
  profile: WorkerProfile,
  context: InjectionContext,
  contextManager?: ContextManager
): string {
  const sections: string[] = [];

  // 1. 现有引导 Prompt
  sections.push(this.buildWorkerPrompt(profile, context));

  // 2. 从 ContextManager 获取 Wisdom
  if (contextManager) {
    const wisdom = this.getWisdomFromContext(contextManager);
    if (wisdom) {
      sections.push(wisdom);
    }
  }

  // 3. 当前任务
  sections.push(`## 当前任务\n${context.taskDescription}`);

  return sections.join('\n\n');
}

/**
 * 从现有 ContextManager 提取 Wisdom
 */
private getWisdomFromContext(contextManager: ContextManager): string | null {
  const memoryDoc = contextManager.getMemoryDocument();
  if (!memoryDoc) return null;

  const parts: string[] = [];

  // 最近决策
  if (memoryDoc.decisions.length > 0) {
    parts.push('## 已做决策');
    parts.push(...memoryDoc.decisions.slice(-5).map(d => `- ${d.description}`));
  }

  // 待解决问题（需要避免）
  if (memoryDoc.pendingIssues.length > 0) {
    parts.push('## 需要注意');
    parts.push(...memoryDoc.pendingIssues.slice(-3).map(i => `- ${i.description}`));
  }

  return parts.length > 0 ? parts.join('\n') : null;
}
```

#### 4.5.6 扩展 WorkerReport

增加结构化的 Wisdom 提取字段：

```typescript
// 扩展: src/orchestrator/protocols/worker-report.ts

interface WorkerResult {
  // ... 现有字段

  /**
   * 结构化知识提取（可选）
   * Worker 可主动标注重要发现
   */
  wisdomExtraction?: {
    learnings?: string[];      // 学习到的信息
    decisions?: string[];      // 做出的决策
    warnings?: string[];       // 需要注意的问题
    significantLearning?: string; // 值得跨会话保存的重要经验
  };
}
```

#### 4.5.7 验收标准

- [ ] `extractWisdomFromResult()` 方法实现
- [ ] Wisdom 提取规则（learnings、decisions、issues）
- [ ] 与现有 `MemoryDocument` 集成
- [ ] 与现有 `ContextManager.getRecentContext()` 集成
- [ ] 重要经验持久化到 `ProjectKnowledgeBase.addFAQ()`
- [ ] `WorkerResult.wisdomExtraction` 字段扩展
- [ ] 单元测试覆盖

---

### 4.6 提案 6: Wave 并行分组（利用现有 TaskDependencyGraph）

> **优先级: P2** - 优化并行执行效率

#### 4.6.1 问题描述

当前 `WorkerTodo` 的并行控制粒度较粗：

```typescript
interface WorkerTodo {
  // ...
  dependencies: string[];  // 依赖的 Todo IDs
}

// 执行时只有 parallel: true/false
```

**问题**：

- 无法表达复杂的并行依赖
- 无法可视化执行波次
- 并行效率不是最优

#### 4.6.2 现有机制分析

**⚠️ 重要发现：系统已有完整的任务依赖图和 Wave 调度能力！**

```typescript
// 现有: src/orchestrator/task-dependency-graph.ts
export class TaskDependencyGraph {
  addTask(id: string, name: string, data?: unknown, targetFiles?: string[]): void
  addDependency(taskId: string, dependsOn: string): boolean

  // ✅ 已有拓扑排序和并行批次计算！
  analyze(): DependencyAnalysis {
    return {
      hasCycle: boolean,
      topologicalOrder: string[],
      executionBatches: ExecutionBatch[],  // ✅ 这就是 Wave！
      criticalPath: string[],
      fileConflicts: FileConflict[],
    };
  }
}

// 现有: ExecutionBatch 就是 Wave
export interface ExecutionBatch {
  batchIndex: number;           // Wave 编号
  taskIds: string[];            // 该 Wave 可并行执行的任务
}
```

```typescript
// 现有: src/orchestrator/core/executors/execution-coordinator.ts
private groupByDependencies(): Assignment[][] {
  // ✅ 已有按依赖分组的逻辑！
}
```

因此，本提案**不新建 `WaveScheduler`**，而是**利用现有 `TaskDependencyGraph`**。

#### 4.6.3 增强方案

在 `ExecutionCoordinator` 中集成 `TaskDependencyGraph`：

```typescript
// 增强: src/orchestrator/core/executors/execution-coordinator.ts

import { TaskDependencyGraph, DependencyAnalysis } from '../../task-dependency-graph';

export class ExecutionCoordinator extends EventEmitter {
  private dependencyGraph: TaskDependencyGraph;

  constructor(...) {
    // ...
    this.dependencyGraph = new TaskDependencyGraph();
  }

  /**
   * 执行 Mission（增强版）
   */
  async execute(options: ExecutionOptions): Promise<ExecutionResult> {
    // 1. 构建依赖图
    this.buildDependencyGraph();

    // 2. 分析依赖图，获取执行批次（Wave）
    const analysis = this.dependencyGraph.analyze();

    // 3. 检测循环依赖
    if (analysis.hasCycle) {
      logger.warn('检测到循环依赖', { cycleNodes: analysis.cycleNodes }, LogCategory.ORCHESTRATOR);
    }

    // 4. 发送执行计划到 UI
    this.emit('planGenerated', {
      totalWaves: analysis.executionBatches.length,
      waves: analysis.executionBatches,
      criticalPath: analysis.criticalPath,
      fileConflicts: analysis.fileConflicts,
    });

    // 5. 按 Wave 执行
    return await this.executeByWaves(analysis.executionBatches, options);
  }

  /**
   * 构建依赖图
   */
  private buildDependencyGraph(): void {
    // 清空旧图
    this.dependencyGraph = new TaskDependencyGraph();

    // 添加所有 Assignment
    for (const assignment of this.mission.assignments) {
      this.dependencyGraph.addTask(
        assignment.id,
        assignment.responsibility,
        assignment,
        assignment.targetFiles
      );
    }

    // 添加依赖关系（基于 Contract 依赖）
    for (const assignment of this.mission.assignments) {
      for (const contractId of assignment.consumerContracts) {
        const producer = this.mission.assignments.find(
          a => a.producerContracts.includes(contractId)
        );
        if (producer) {
          this.dependencyGraph.addDependency(assignment.id, producer.id);
        }
      }
    }
  }

  /**
   * 按 Wave 执行
   */
  private async executeByWaves(
    waves: ExecutionBatch[],
    options: ExecutionOptions
  ): Promise<ExecutionResult> {
    const errors: string[] = [];
    let completedCount = 0;

    for (const wave of waves) {
      this.emit('waveStarted', {
        wave: wave.batchIndex,
        taskCount: wave.taskIds.length
      });

      const assignments = wave.taskIds
        .map(id => this.mission.assignments.find(a => a.id === id)!)
        .filter(Boolean);

      if (options.parallel && assignments.length > 1) {
        // 并行执行同一 Wave 的所有 Assignment
        const results = await Promise.all(
          assignments.map(assignment => this.executeAssignment(assignment, options))
        );
        results.forEach(r => {
          if (r.success) completedCount++;
          else errors.push(...r.errors);
        });
      } else {
        // 串行执行
        for (const assignment of assignments) {
          const result = await this.executeAssignment(assignment, options);
          if (result.success) completedCount++;
          else errors.push(...result.errors);
        }
      }

      this.emit('waveCompleted', { wave: wave.batchIndex });
    }

    return this.buildResult(errors.length === 0, errors, completedCount);
  }
}
```

#### 4.6.4 可选扩展：Todo 级别的 Wave 字段

如果需要更细粒度的 Todo 级并行控制，可以扩展 `WorkerTodo`：

```typescript
// 可选扩展: src/orchestrator/mission/types.ts

export interface WorkerTodo {
  // ... 现有字段

  /**
   * 执行波次（并行分组）- 可选
   * 由 TaskDependencyGraph.analyze() 自动计算
   */
  wave?: number;
}
```

#### 4.6.5 验收标准

- [ ] `ExecutionCoordinator` 集成 `TaskDependencyGraph`
- [ ] `buildDependencyGraph()` 实现
- [ ] `executeByWaves()` 实现
- [ ] Wave 事件（`waveStarted`、`waveCompleted`）发送
- [ ] UI 显示执行波次
- [ ] 循环依赖检测和警告
- [ ] 单元测试覆盖

---


## 5. 实施路线图

### 5.1 阶段划分

```text
Phase A: Session 恢复 (P0)
    │   优先级: 最高
    │   复杂度: 中
    │
    │   • 实现 WorkerSessionManager
    │   • 改造 AutonomousWorker
    │   • 集成到 MissionOrchestrator
    │   • 恢复流程测试
    ↓
Phase B: 验证证据 (P0)
    │   优先级: 最高
    │   复杂度: 中
    │
    │   • 扩展 WorkerReport
    │   • 实现证据收集
    │   • 实现验证逻辑
    ↓
Phase C: 6-Section 结构化提示 + 强化分类别约束 (P1)
    │   优先级: 高
    │   复杂度: 低（增强现有 GuidanceInjector）
    │
    │   • 增强 GuidanceInjector.buildTaskStructuredSection
    │   • 增强 GuidanceInjector.buildConstraintsSection
    │   • 与现有 ContextManager 集成
    │   • 测试验证
    ↓
Phase D: Wisdom 累积系统 (P1)
    │   优先级: 高
    │   复杂度: 低（复用现有知识库）
    │
    │   • 实现 extractWisdomFromResult 提取流水线
    │   • 存入现有 MemoryDocument/ProjectKnowledgeBase
    │   • 通过 ContextManager 注入
    ↓
Phase E: Wave 并行 (P2)
    │   优先级: 中
    │   复杂度: 低（利用现有 TaskDependencyGraph）
    │
    │   • 在 ExecutionCoordinator 集成 TaskDependencyGraph
    │   • Wave 事件发送
    │   • UI 展示
    ↓
Phase F: 集成测试
        • 端到端场景验证
        • 性能回归测试
        • 文档更新
```

### 5.2 文件变更清单

| 文件 | 变更类型 | Phase |
|------|----------|-------|
| `src/orchestrator/worker/worker-session.ts` | 新增 | A |
| `src/orchestrator/worker/autonomous-worker.ts` | 重构 | A, B |
| `src/orchestrator/core/mission-orchestrator.ts` | 重构 | A, B, D |
| `src/orchestrator/protocols/worker-report.ts` | 扩展 | B, D |
| `src/orchestrator/profile/guidance-injector.ts` | 增强 | C, D |
| `src/orchestrator/profile/types.ts` | 扩展 | C |
| `src/context/memory-document.ts` | 扩展 | D |
| `src/orchestrator/core/executors/execution-coordinator.ts` | 增强 | E |

### 5.3 依赖关系

```text
Phase A (Session 恢复) [独立，最高优先级]
    │
    └──→ Phase B (验证证据) [可并行]

Phase C (6-Section 结构化提示 + 强化分类别约束) [独立]
    │
    └──→ Phase D (Wisdom 累积系统) [依赖 C 的 Context 注入机制]

Phase E (Wave 并行) [独立]
```

### 5.4 实施约束（强制）

> 基于 1.4 节"单一正确路径"原则，实施过程中必须遵守以下约束：

| 约束                 | 说明                                                         | 违规后果               |
| -------------------- | ------------------------------------------------------------ | ---------------------- |
| **禁止临时兼容代码** | 即使 Phase D 依赖 Phase C，也不允许创建"等待 C 完成"的临时分支 | 回滚并重新实施         |
| **禁止特性开关**     | 不允许使用 `if (enableNewFeature)` 控制新旧逻辑切换          | 删除开关，保留单一路径 |
| **同步删除旧代码**   | 新代码上线时，旧代码必须在同一 PR 中删除                     | PR 不予合并            |
| **测试先行**         | 新功能必须先有测试，测试通过后才能删除旧实现                 | 阻止 CI 通过           |

**示例**：Phase C（强化分类别约束）可独立实施

```typescript
// ✅ 正确：直接增强现有 buildConstraintsSection
private buildConstraintsSection(
  assignment: Assignment,
  category: CategoryType
): string {
  const baseConstraints = this.profile.guidance.constraints;
  const categoryConstraints = this.getCategoryConstraints(category);

  return `
## 注意事项

### 通用约束
${baseConstraints.map(c => `- ${c}`).join('\n')}

### ${category} 任务专项约束
${categoryConstraints.map(c => `- ${c}`).join('\n')}
  `;
}
```

---

## 6. 验收标准

### 6.1 功能验收

| 验收项              | 标准                                             | 验证方法 |
| ------------------- | ------------------------------------------------ | -------- |
| Session 恢复        | 失败后使用 sessionId 可恢复执行                  | 集成测试 |
| 上下文复用          | 恢复时对话历史和文件缓存正确恢复                 | 日志检查 |
| 验证证据            | WorkerReport 包含命令执行记录和文件变更证据      | 日志检查 |
| 验证逻辑            | 编排者能检测 Worker 声明与实际不符               | 单元测试 |
| 6-Section 结构化    | 委托提示包含完整的 6 个部分                      | 单元测试 |
| 分类别约束          | 各类别生成正确的专项约束                         | 单元测试 |
| Wisdom 累积         | 后续 Assignment 能获取之前的知识摘要             | 集成测试 |
| Wave 调度           | 同 wave 的 Todo 并行执行                         | 性能测试 |

### 6.2 性能验收

| 指标                 | 基准值 | 目标值                    |
| -------------------- | ------ | ------------------------- |
| 失败重试 token 消耗  | 100%   | <30% (Session 复用)       |
| 并行任务执行时间     | N * T  | < N * T / 2 (Wave 调度)   |

### 6.3 回归测试

确保以下场景不受影响：

- [ ] ASK 模式正常响应
- [ ] DIRECT 模式快速执行
- [ ] TASK 模式完整流程
- [ ] Worker 汇报机制正常
- [ ] 降级流程正常
- [ ] UI 消息显示正确

---

## 附录

### A. 参考资料

- [oh-my-opencode](https://github.com/code-yeongyu/oh-my-opencode)
  - `src/agents/prometheus-prompt.ts` - 规划者 Prompt
  - `src/agents/sisyphus.ts` - 编排者实现
  - `src/tools/delegate-task/tools.ts` - 任务委托机制
  - `docs/guide/understanding-orchestration-system.md` - 架构文档

### B. 术语表

| 术语              | 含义                                                                                     |
| ----------------- | ---------------------------------------------------------------------------------------- |
| 6-Section Prompt  | 包含 TASK/EXPECTED OUTCOME/REQUIRED TOOLS/MUST DO/MUST NOT DO/CONTEXT 的结构化任务描述  |
| Session 恢复      | 使用 sessionId 恢复之前的执行上下文，避免重建                                            |
| Wave              | 执行波次，同 wave 的任务可并行执行（利用现有 TaskDependencyGraph 计算）                  |
| Trust But Verify  | 验证 Worker 的声明，而非盲信                                                             |
| AI Slop           | AI 过度优化、自作主张的行为                                                              |
| Wisdom            | Mission 执行过程中积累的知识（学习、决策、问题），存入现有 MemoryDocument/ProjectKnowledgeBase |

### C. 变更历史

| 版本 | 日期       | 变更内容                                                                 |
| ---- | ---------- | ------------------------------------------------------------------------ |
| 1.0  | 2025-01-31 | 初始版本                                                                 |
| 1.1  | 2025-01-31 | 增加 6-Section 结构化委托提示、Wisdom 累积系统；重组提案编号和实施路线图 |
| 1.2  | 2025-02-01 | 优化提案 3/5/6，复用现有 GuidanceInjector、ContextManager、TaskDependencyGraph |

---

> 本文档作为增强提案，实施前需评审确认优先级和可行性。
