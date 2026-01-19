# OrchestratorAgent 重构方案：Mission-Driven Architecture

> **版本**: v1.1
> **日期**: 2026-01-19
> **状态**: 待审核

---

## 零、重构原则（强制）

### 0.1 零兼容原则

> **核心立场**：这是一次**完整的架构重构**，不允许任何兼容性妥协。

**禁止事项**：
- ❌ 禁止保留兼容层（LegacyAdapter、CompatibilityWrapper 等）
- ❌ 禁止使用废弃标记（@deprecated）代替删除
- ❌ 禁止保留旧接口"以防万一"
- ❌ 禁止为旧代码做特殊处理
- ❌ 禁止在新架构中引用旧类型或旧逻辑

**强制事项**：
- ✅ 所有调用方必须同步升级到新架构
- ✅ 不兼容新架构的模块必须**同步重构**，而非架构妥协
- ✅ UI 层、存储层、测试代码必须同步适配
- ✅ 配置文件、数据格式必须同步迁移

### 0.2 零残留原则

> **核心立场**：重构完成且测试通过后，必须**完全清理**所有残留内容。

**必须删除**：
- 🗑️ 旧的 `orchestrator-agent.ts` 文件
- 🗑️ 旧的 `worker-agent.ts` 文件
- 🗑️ 旧的 `worker-pool.ts` 文件
- 🗑️ 旧的类型定义（ExecutionPlan、SubTask 旧版）
- 🗑️ 旧的 Prompt 模板
- 🗑️ 所有 `.backup`、`.old`、`.deprecated` 文件
- 🗑️ 注释掉的旧代码
- 🗑️ 无用的 import 语句
- 🗑️ 孤立的工具函数

**代码质量要求**：
- 📖 保持高度可读性，新开发者能快速理解
- 📖 文件职责单一，每个文件 < 500 行
- 📖 类型定义完整，无 `any` 类型逃逸
- 📖 注释只保留"为什么"，删除"是什么"

### 0.3 同步升级清单

以下模块必须在本次重构中**同步升级**：

| 模块 | 升级内容 | 优先级 |
|------|----------|--------|
| **WebviewProvider** | 适配 Mission/Assignment/Todo 事件 | P0 |
| **UnifiedSessionManager** | 适配 MissionStorage | P0 |
| **InteractiveSession** | 适配新消息协议 | P0 |
| **CLI 适配器** | 适配 AutonomousWorker 调用 | P0 |
| **测试文件** | 全部重写，匹配新架构 | P1 |
| **Prompt 模板** | 迁移到新目录结构 | P1 |
| **配置文件** | WorkerProfile 新增评审字段 | P1 |
| **文档** | 更新 API 文档 | P2 |

---

## 一、重构背景

### 1.1 现有架构的局限性

当前 `OrchestratorAgent` 采用 **SubTask 预规划模式**：

```
用户输入 → 编排者分析 → 生成 SubTask 列表 → 分发给 Worker → 执行 → 汇总
```

**核心问题**：

| 问题 | 影响 |
|------|------|
| 编排者无法预知执行细节 | 任务粒度可能不合适 |
| Worker 发现新需求时无法灵活处理 | 只能失败重试 |
| 多 Worker 协作靠隐式依赖 | 契约不明确，易冲突 |
| 评审机制单一 | 单次评审，无修订闭环 |
| 画像系统未充分利用 | 分配决策不透明 |

### 1.2 重构目标

1. **从"任务分发"转向"目标驱动"**：编排者定义目标和约束，Worker 自主规划
2. **显式化协作契约**：多 Worker 之间有明确的接口约定
3. **Worker 自主性**：Worker 自己规划 Todo，灵活应对执行中的发现
4. **编排者变协调者**：监控、协调、审批，而非微观管理
5. **充分利用画像系统**：职责分配、引导注入、验证都基于画像

---

## 二、新架构概述

### 2.1 核心概念

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Mission（任务使命）                          │
│  用户目标 + 约束 + 验收标准                                          │
└─────────────────────────────────────────────────────────────────────┘
                                    │
                    ┌───────────────┼───────────────┐
                    ▼               ▼               ▼
            ┌─────────────┐ ┌─────────────┐ ┌─────────────┐
            │  Contract   │ │  Contract   │ │  Contract   │
            │  API 契约   │ │  数据契约   │ │  文件契约   │
            └─────────────┘ └─────────────┘ └─────────────┘
                    │               │               │
                    ▼               ▼               ▼
            ┌─────────────┐ ┌─────────────┐ ┌─────────────┐
            │ Assignment  │ │ Assignment  │ │ Assignment  │
            │ Claude 职责 │ │ Codex 职责  │ │ Gemini 职责 │
            └──────┬──────┘ └──────┬──────┘ └──────┬──────┘
                   │               │               │
                   ▼               ▼               ▼
            ┌─────────────┐ ┌─────────────┐ ┌─────────────┐
            │    Todo     │ │    Todo     │ │    Todo     │
            │ Worker 自主 │ │ Worker 自主 │ │ Worker 自主 │
            │   规划执行  │ │   规划执行  │ │   规划执行  │
            └─────────────┘ └─────────────┘ └─────────────┘
```

### 2.2 概念定义

| 概念 | 定义 | 所有者 | 对应现有概念 |
|------|------|--------|-------------|
| **Mission** | 用户的目标 + 约束 + 验收标准 | Orchestrator | ExecutionPlan |
| **Contract** | Worker 之间的接口约定 | Orchestrator | featureContract（增强） |
| **Assignment** | 分配给 Worker 的职责范围 | Orchestrator | SubTask（弱化） |
| **Todo** | Worker 自己规划的具体工作 | Worker | 新增 |

### 2.3 工作流程对比

**现有流程**：
```
用户输入 → 任务分析 → 生成 SubTask 列表 → 用户确认 → 分发执行 → 汇总
```

**新流程**：
```
用户输入 → 理解目标 → 确定参与者 → 定义契约 → 分配职责
    → Worker 自主规划 → 编排者审查 → 执行 → 验收
```

---

## 三、数据模型设计

### 3.1 Mission（任务使命）

```typescript
/**
 * Mission - 任务使命
 * 替代现有的 ExecutionPlan，但更关注"目标"而非"任务列表"
 */
export interface Mission {
  id: string;
  sessionId: string;

  // ===== 目标定义 =====
  /** 用户原始输入 */
  userPrompt: string;
  /** 提炼后的目标（一句话描述） */
  goal: string;
  /** 目标分析（为什么需要这样做） */
  analysis: string;
  /** 项目上下文 */
  context: string;

  // ===== 约束与验收 =====
  /** 约束条件（必须遵守） */
  constraints: Constraint[];
  /** 验收标准（如何判断完成） */
  acceptanceCriteria: AcceptanceCriterion[];

  // ===== 协作定义 =====
  /** 契约列表（Worker 之间的约定） */
  contracts: Contract[];
  /** 职责分配列表 */
  assignments: Assignment[];

  // ===== 风险评估 =====
  /** 风险等级 */
  riskLevel: 'low' | 'medium' | 'high';
  /** 风险因素 */
  riskFactors: string[];
  /** 执行路径 */
  executionPath: 'light' | 'standard' | 'full';

  // ===== 状态管理 =====
  status: MissionStatus;
  /** 当前阶段 */
  phase: MissionPhase;

  // ===== 时间戳 =====
  createdAt: number;
  updatedAt: number;
  startedAt?: number;
  completedAt?: number;
}

export type MissionStatus =
  | 'draft'           // 草稿
  | 'planning'        // 规划中
  | 'pending_review'  // 等待审查
  | 'pending_approval'// 等待用户确认
  | 'executing'       // 执行中
  | 'paused'          // 暂停
  | 'reviewing'       // 验收中
  | 'completed'       // 完成
  | 'failed'          // 失败
  | 'cancelled';      // 取消

export type MissionPhase =
  | 'goal_understanding'    // 理解目标
  | 'participant_selection' // 确定参与者
  | 'contract_definition'   // 定义契约
  | 'responsibility_assignment' // 分配职责
  | 'worker_planning'       // Worker 规划
  | 'plan_review'           // 规划审查
  | 'execution'             // 执行
  | 'verification'          // 验收
  | 'summary';              // 总结

export interface Constraint {
  id: string;
  type: 'must' | 'should' | 'should_not' | 'must_not';
  description: string;
  source: 'user' | 'system' | 'profile';
}

export interface AcceptanceCriterion {
  id: string;
  description: string;
  verifiable: boolean;
  verificationMethod?: 'auto' | 'manual' | 'test';
  status: 'pending' | 'passed' | 'failed';
}
```

### 3.2 Contract（协作契约）

```typescript
/**
 * Contract - 协作契约
 * Worker 之间的显式约定，替代隐式的依赖关系
 */
export interface Contract {
  id: string;
  missionId: string;

  // ===== 契约类型 =====
  type: ContractType;
  /** 契约名称 */
  name: string;
  /** 契约描述 */
  description: string;

  // ===== 契约内容 =====
  specification: ContractSpecification;

  // ===== 参与方 =====
  /** 提供方（谁定义/实现这个契约） */
  producer: CLIType;
  /** 消费方（谁使用这个契约） */
  consumers: CLIType[];

  // ===== 状态 =====
  status: ContractStatus;

  // ===== 验证 =====
  /** 验证方式 */
  verificationMethod?: 'type_check' | 'test' | 'manual';
  /** 验证结果 */
  verificationResult?: {
    passed: boolean;
    issues: string[];
    verifiedAt: number;
  };
}

export type ContractType =
  | 'api'       // API 接口契约
  | 'data'      // 数据结构契约
  | 'event'     // 事件契约
  | 'file'      // 文件/目录契约
  | 'style'     // 样式/规范契约
  | 'dependency'; // 依赖契约

export type ContractStatus =
  | 'draft'       // 草稿
  | 'proposed'    // 已提议
  | 'agreed'      // 已同意
  | 'implemented' // 已实现
  | 'verified'    // 已验证
  | 'violated';   // 已违反

export interface ContractSpecification {
  // API 契约
  api?: {
    endpoint?: string;
    method?: string;
    requestSchema?: string;   // JSON Schema 或 TypeScript 类型
    responseSchema?: string;
    errorCodes?: Record<string, string>;
  };

  // 数据契约
  data?: {
    schema: string;           // TypeScript interface 定义
    examples?: string[];
    validationRules?: string[];
  };

  // 事件契约
  event?: {
    eventName: string;
    payload: string;
    trigger: string;
  };

  // 文件契约
  file?: {
    patterns: string[];       // 文件路径模式
    namingConvention?: string;
    structure?: string;
  };
}
```

### 3.3 Assignment（职责分配）

```typescript
/**
 * Assignment - 职责分配
 * 告诉 Worker "你负责什么"，而不是"你要做什么"
 * 替代现有的 SubTask，但更抽象
 */
export interface Assignment {
  id: string;
  missionId: string;

  // ===== Worker 分配 =====
  workerId: CLIType;
  /** 分配原因（基于画像的决策记录） */
  assignmentReason: AssignmentReason;

  // ===== 职责定义 =====
  /** 职责描述 */
  responsibility: string;
  /** 职责范围 */
  scope: AssignmentScope;
  /** 引导 Prompt（从画像生成） */
  guidancePrompt: string;

  // ===== 契约关联 =====
  /** 作为提供方的契约 */
  producerContracts: string[];
  /** 作为消费方的契约 */
  consumerContracts: string[];

  // ===== Worker 规划 =====
  /** Worker 自主规划的 Todo 列表 */
  todos: WorkerTodo[];
  /** 规划状态 */
  planningStatus: 'pending' | 'planning' | 'planned' | 'approved' | 'rejected';
  /** 规划审查结果 */
  planReview?: {
    status: 'approved' | 'needs_revision' | 'rejected';
    feedback: string;
    reviewedAt: number;
  };

  // ===== 状态 =====
  status: AssignmentStatus;
  progress: number;           // 0-100，基于 Todo 完成度自动计算

  // ===== 时间戳 =====
  createdAt: number;
  startedAt?: number;
  completedAt?: number;
}

export type AssignmentStatus =
  | 'pending'     // 等待 Worker 规划
  | 'planning'    // Worker 正在规划
  | 'ready'       // 规划完成，等待执行
  | 'executing'   // 执行中
  | 'blocked'     // 被阻塞（等待其他 Assignment 或审批）
  | 'completed'   // 完成
  | 'failed';     // 失败

export interface AssignmentScope {
  /** 职责范围内（应该做的） */
  includes: string[];
  /** 职责范围外（不应该做的） */
  excludes: string[];
  /** 目标文件/目录 */
  targetPaths?: string[];
}

export interface AssignmentReason {
  /** 匹配的画像偏好 */
  profileMatch: {
    category: string;
    score: number;
    matchedKeywords: string[];
  };
  /** 契约角色 */
  contractRole: 'producer' | 'consumer' | 'both' | 'none';
  /** 决策说明 */
  explanation: string;
  /** 备选方案 */
  alternatives: Array<{
    workerId: CLIType;
    score: number;
    reason: string;
  }>;
}
```

### 3.4 WorkerTodo（Worker 自主规划）

```typescript
/**
 * WorkerTodo - Worker 自主规划的工作项
 * 这是新架构的核心：Worker 自己决定怎么完成职责
 */
export interface WorkerTodo {
  id: string;
  assignmentId: string;

  // ===== 内容 =====
  /** 工作内容 */
  content: string;
  /** 为什么需要这个 Todo */
  reasoning: string;
  /** 预期产出 */
  expectedOutput: string;

  // ===== 分类 =====
  type: TodoType;
  /** 优先级 */
  priority: number;

  // ===== 范围检查 =====
  /** 是否超出职责范围 */
  outOfScope: boolean;
  /** 超范围审批状态 */
  approvalStatus?: 'pending' | 'approved' | 'rejected';
  /** 审批说明 */
  approvalNote?: string;

  // ===== 依赖 =====
  /** 依赖的 Todo（同 Assignment 内） */
  dependsOn: string[];
  /** 依赖的契约 */
  requiredContracts: string[];
  /** 被阻塞原因 */
  blockedReason?: string;

  // ===== 状态 =====
  status: TodoStatus;

  // ===== 执行结果 =====
  output?: TodoOutput;

  // ===== 时间戳 =====
  createdAt: number;
  startedAt?: number;
  completedAt?: number;
}

export type TodoType =
  | 'discovery'       // 探索/调研
  | 'design'          // 设计/规划
  | 'implementation'  // 实现
  | 'verification'    // 验证/测试
  | 'integration'     // 集成
  | 'fix'             // 修复
  | 'refactor';       // 重构

export type TodoStatus =
  | 'pending'         // 等待执行
  | 'blocked'         // 被阻塞
  | 'in_progress'     // 执行中
  | 'completed'       // 完成
  | 'failed'          // 失败
  | 'skipped';        // 跳过

export interface TodoOutput {
  /** 是否成功 */
  success: boolean;
  /** 输出摘要 */
  summary: string;
  /** 修改的文件 */
  modifiedFiles: string[];
  /** 产生的新契约（如果有） */
  newContracts?: Partial<Contract>[];
  /** 发现的问题 */
  issues?: string[];
  /** 错误信息 */
  error?: string;
  /** 执行时长 */
  duration: number;
  /** Token 使用 */
  tokenUsage?: {
    input: number;
    output: number;
  };
}
```

---

## 四、核心组件设计

### 4.1 组件架构

```
┌─────────────────────────────────────────────────────────────────────┐
│                      MissionOrchestrator                            │
│                    （替代 OrchestratorAgent）                        │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐                 │
│  │ GoalParser  │  │ Collaborator│  │ MissionFlow │                 │
│  │  目标理解   │  │  协作规划   │  │  流程控制   │                 │
│  └─────────────┘  └─────────────┘  └─────────────┘                 │
│                                                                     │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐                 │
│  │ Contract    │  │ Assignment  │  │ Verification│                 │
│  │ Manager     │  │ Manager     │  │ Runner      │                 │
│  │ 契约管理    │  │ 职责分配    │  │ 验收执行    │                 │
│  └─────────────┘  └─────────────┘  └─────────────┘                 │
│                                                                     │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│                     ┌─────────────────────┐                         │
│                     │   WorkerCoordinator │                         │
│                     │     Worker 协调器   │                         │
│                     └──────────┬──────────┘                         │
│                                │                                    │
│           ┌────────────────────┼────────────────────┐              │
│           ▼                    ▼                    ▼              │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐    │
│  │ AutonomousWorker│  │ AutonomousWorker│  │ AutonomousWorker│    │
│  │     Claude      │  │     Codex       │  │     Gemini      │    │
│  └─────────────────┘  └─────────────────┘  └─────────────────┘    │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### 4.2 MissionOrchestrator（核心编排器）

```typescript
/**
 * MissionOrchestrator - 任务使命编排器
 * 替代现有的 OrchestratorAgent
 */
export class MissionOrchestrator extends EventEmitter {
  // ===== 依赖 =====
  private cliFactory: CLIAdapterFactory;
  private profileLoader: ProfileLoader;
  private workerCoordinator: WorkerCoordinator;
  private contractManager: ContractManager;
  private assignmentManager: AssignmentManager;
  private verificationRunner: VerificationRunner;
  private missionStorage: MissionStorage;

  // ===== 状态 =====
  private currentMission: Mission | null = null;

  /**
   * 主入口：执行用户请求
   */
  async execute(userPrompt: string, sessionId: string): Promise<string> {
    // Phase 1: 意图门控（复用现有 IntentGate）
    const intent = await this.intentGate.process(userPrompt);
    if (intent.skipTaskAnalysis) {
      return await this.handleDirectIntent(userPrompt, intent);
    }

    // Phase 2: 理解目标
    const mission = await this.understandGoal(userPrompt, sessionId);
    this.currentMission = mission;
    this.emit('missionCreated', mission);

    // Phase 3: 确定参与者和契约
    await this.planCollaboration(mission);
    this.emit('collaborationPlanned', mission);

    // Phase 4: Worker 自主规划
    await this.letWorkersPlan(mission);
    this.emit('workersPlanned', mission);

    // Phase 5: 编排者审查规划
    const reviewResult = await this.reviewPlanning(mission);
    if (!reviewResult.approved) {
      // 处理审查未通过
      await this.handleReviewFailure(mission, reviewResult);
    }

    // Phase 6: 用户确认（根据风险等级）
    if (this.requiresUserConfirmation(mission)) {
      const confirmed = await this.waitForUserConfirmation(mission);
      if (!confirmed) {
        return '任务已取消。';
      }
    }

    // Phase 7: 执行
    await this.executeMission(mission);

    // Phase 8: 验收
    const verificationResult = await this.verifyMission(mission);

    // Phase 9: 总结
    return await this.summarizeMission(mission, verificationResult);
  }

  /**
   * Phase 2: 理解目标
   */
  private async understandGoal(userPrompt: string, sessionId: string): Promise<Mission> {
    const goalParser = new GoalParser(this.cliFactory, this.profileLoader);

    const parsed = await goalParser.parse(userPrompt, {
      projectContext: await this.getProjectContext(),
    });

    const mission: Mission = {
      id: `mission_${Date.now()}`,
      sessionId,
      userPrompt,
      goal: parsed.goal,
      analysis: parsed.analysis,
      context: parsed.context,
      constraints: parsed.constraints,
      acceptanceCriteria: parsed.acceptanceCriteria,
      contracts: [],
      assignments: [],
      riskLevel: parsed.riskLevel,
      riskFactors: parsed.riskFactors,
      executionPath: this.determineExecutionPath(parsed.riskLevel),
      status: 'planning',
      phase: 'goal_understanding',
      createdAt: Date.now(),
      updatedAt: Date.now(),
    };

    await this.missionStorage.save(mission);
    return mission;
  }

  /**
   * Phase 3: 规划协作
   */
  private async planCollaboration(mission: Mission): Promise<void> {
    mission.phase = 'participant_selection';

    // 3.1 确定需要哪些 Worker 参与
    const participants = await this.identifyParticipants(mission);

    // 3.2 定义契约
    mission.phase = 'contract_definition';
    const contracts = await this.contractManager.defineContracts(mission, participants);
    mission.contracts = contracts;

    // 3.3 分配职责
    mission.phase = 'responsibility_assignment';
    const assignments = await this.assignmentManager.createAssignments(
      mission,
      participants,
      contracts
    );
    mission.assignments = assignments;

    mission.updatedAt = Date.now();
    await this.missionStorage.save(mission);
  }

  /**
   * Phase 4: Worker 自主规划
   */
  private async letWorkersPlan(mission: Mission): Promise<void> {
    mission.phase = 'worker_planning';

    const planningResults = await Promise.all(
      mission.assignments.map(async (assignment) => {
        assignment.status = 'planning';
        assignment.planningStatus = 'planning';

        const worker = this.workerCoordinator.getWorker(assignment.workerId);
        const todos = await worker.planWork({
          assignment,
          contracts: this.getRelevantContracts(mission, assignment),
          missionContext: mission.context,
        });

        // 检查超范围的 Todo
        for (const todo of todos) {
          if (this.isOutOfScope(todo, assignment.scope)) {
            todo.outOfScope = true;
            todo.approvalStatus = 'pending';
          }
        }

        assignment.todos = todos;
        assignment.planningStatus = 'planned';
        assignment.status = 'ready';

        return { assignmentId: assignment.id, todos };
      })
    );

    mission.updatedAt = Date.now();
    await this.missionStorage.save(mission);
  }

  /**
   * Phase 7: 执行
   */
  private async executeMission(mission: Mission): Promise<void> {
    mission.status = 'executing';
    mission.phase = 'execution';
    mission.startedAt = Date.now();

    await this.workerCoordinator.executeAssignments(mission, {
      onTodoStarted: (assignmentId, todoId) => {
        this.emit('todoStarted', { missionId: mission.id, assignmentId, todoId });
      },
      onTodoCompleted: (assignmentId, todoId, output) => {
        this.emit('todoCompleted', { missionId: mission.id, assignmentId, todoId, output });
        this.updateProgress(mission, assignmentId);
      },
      onTodoFailed: (assignmentId, todoId, error) => {
        this.emit('todoFailed', { missionId: mission.id, assignmentId, todoId, error });
      },
      onDynamicTodoAdded: async (assignmentId, todo) => {
        if (todo.outOfScope) {
          // 超范围 Todo 需要审批
          const approved = await this.handleOutOfScopeApproval(mission, assignmentId, todo);
          todo.approvalStatus = approved ? 'approved' : 'rejected';
        }
        this.emit('dynamicTodoAdded', { missionId: mission.id, assignmentId, todo });
      },
      onContractViolation: async (contractId, violation) => {
        await this.handleContractViolation(mission, contractId, violation);
      },
    });

    mission.updatedAt = Date.now();
    await this.missionStorage.save(mission);
  }
}
```

### 4.3 AutonomousWorker（自主 Worker）

```typescript
/**
 * AutonomousWorker - 自主 Worker
 * 替代现有的 WorkerAgent，增加自主规划能力
 */
export class AutonomousWorker {
  private workerId: CLIType;
  private cliAdapter: CLIAdapter;
  private profile: WorkerProfile;
  private guidanceInjector: GuidanceInjector;

  /**
   * 自主规划工作
   */
  async planWork(context: {
    assignment: Assignment;
    contracts: Contract[];
    missionContext: string;
  }): Promise<WorkerTodo[]> {
    const { assignment, contracts, missionContext } = context;

    // 构建规划 Prompt
    const planningPrompt = this.buildPlanningPrompt(assignment, contracts, missionContext);

    // 调用 LLM 生成 Todo 列表
    const response = await this.cliAdapter.sendMessage(planningPrompt, {
      systemPrompt: this.buildPlanningSystemPrompt(),
    });

    // 解析 Todo 列表
    const todos = this.parseTodos(response.content, assignment.id);

    // 验证 Todo 合理性
    const validatedTodos = await this.validateTodos(todos, assignment);

    return validatedTodos;
  }

  private buildPlanningPrompt(
    assignment: Assignment,
    contracts: Contract[],
    missionContext: string
  ): string {
    return `
## 你的职责
${assignment.responsibility}

## 职责范围
**应该做的**：
${assignment.scope.includes.map(i => `- ${i}`).join('\n')}

**不应该做的**：
${assignment.scope.excludes.map(e => `- ${e}`).join('\n')}

## 需要遵守的契约
${contracts.map(c => this.formatContract(c)).join('\n\n')}

## 项目上下文
${missionContext}

## 任务
请规划你需要完成的具体工作项（Todo）。

对于每个 Todo，请说明：
1. **content**: 要做什么（具体、可执行）
2. **reasoning**: 为什么需要做
3. **type**: 类型（discovery/design/implementation/verification/integration/fix/refactor）
4. **expectedOutput**: 预期产出
5. **priority**: 优先级（1-5，1 最高）
6. **dependsOn**: 依赖的其他 Todo ID（如果有）
7. **requiredContracts**: 依赖的契约 ID（如果有）

请输出 JSON 格式的 Todo 列表。
`;
  }

  /**
   * 执行单个 Todo
   */
  async executeTodo(
    todo: WorkerTodo,
    context: {
      assignment: Assignment;
      contracts: Contract[];
      onProgress?: (progress: string) => void;
    }
  ): Promise<TodoOutput> {
    const { assignment, contracts, onProgress } = context;

    // 构建执行 Prompt
    const executionPrompt = this.buildExecutionPrompt(todo, assignment, contracts);

    // 注入画像引导
    const guidancePrompt = this.guidanceInjector.buildWorkerPrompt(this.profile, {
      taskDescription: todo.content,
      targetFiles: assignment.scope.targetPaths,
      category: this.inferCategory(todo),
      featureContract: this.formatContracts(contracts),
    });

    const fullPrompt = `${guidancePrompt}\n\n---\n\n${executionPrompt}`;

    const startTime = Date.now();

    try {
      const response = await this.cliAdapter.sendMessage(fullPrompt, {
        onProgress,
      });

      return {
        success: true,
        summary: this.extractSummary(response.content),
        modifiedFiles: this.extractModifiedFiles(response),
        duration: Date.now() - startTime,
        tokenUsage: response.tokenUsage,
      };
    } catch (error) {
      return {
        success: false,
        summary: '',
        modifiedFiles: [],
        error: error instanceof Error ? error.message : String(error),
        duration: Date.now() - startTime,
      };
    }
  }

  /**
   * 执行中发现新需求，动态添加 Todo
   */
  async addDynamicTodo(
    currentTodo: WorkerTodo,
    reason: string,
    assignment: Assignment
  ): Promise<WorkerTodo> {
    const newTodo: WorkerTodo = {
      id: `todo_${Date.now()}`,
      assignmentId: assignment.id,
      content: reason,
      reasoning: `执行 "${currentTodo.content}" 时发现需要先完成此项`,
      expectedOutput: '',
      type: 'discovery',
      priority: currentTodo.priority,
      outOfScope: false,
      dependsOn: [],
      requiredContracts: [],
      status: 'pending',
      createdAt: Date.now(),
    };

    // 检查是否超出职责范围
    newTodo.outOfScope = this.isOutOfScope(newTodo, assignment.scope);
    if (newTodo.outOfScope) {
      newTodo.approvalStatus = 'pending';
    }

    return newTodo;
  }

  /**
   * 规划失败恢复
   */
  async planRecovery(
    failedTodo: WorkerTodo,
    error: Error,
    assignment: Assignment
  ): Promise<WorkerTodo[]> {
    const recoveryPrompt = `
## 失败的工作项
${failedTodo.content}

## 错误信息
${error.message}

## 任务
请分析失败原因，并规划恢复步骤。
输出新的 Todo 列表（JSON 格式）。
`;

    const response = await this.cliAdapter.sendMessage(recoveryPrompt);
    return this.parseTodos(response.content, assignment.id);
  }
}
```

### 4.4 ContractManager（契约管理器）

```typescript
/**
 * ContractManager - 契约管理器
 * 负责契约的定义、验证、冲突检测
 */
export class ContractManager {
  /**
   * 定义契约
   */
  async defineContracts(
    mission: Mission,
    participants: CLIType[]
  ): Promise<Contract[]> {
    // 如果只有一个参与者，不需要契约
    if (participants.length <= 1) {
      return [];
    }

    // 分析任务，识别需要的契约类型
    const contractTypes = await this.identifyContractTypes(mission);

    // 生成契约
    const contracts: Contract[] = [];
    for (const type of contractTypes) {
      const contract = await this.generateContract(mission, type, participants);
      contracts.push(contract);
    }

    return contracts;
  }

  /**
   * 验证契约一致性
   */
  async verifyContractConsistency(mission: Mission): Promise<{
    consistent: boolean;
    violations: ContractViolation[];
  }> {
    const violations: ContractViolation[] = [];

    for (const contract of mission.contracts) {
      // 检查提供方是否已实现
      const producerAssignment = mission.assignments.find(
        a => a.workerId === contract.producer
      );
      if (!producerAssignment) {
        violations.push({
          contractId: contract.id,
          type: 'missing_producer',
          message: `契约 ${contract.name} 的提供方 ${contract.producer} 未分配任务`,
        });
      }

      // 检查消费方是否依赖
      for (const consumer of contract.consumers) {
        const consumerAssignment = mission.assignments.find(
          a => a.workerId === consumer
        );
        if (consumerAssignment) {
          const hasDependency = consumerAssignment.todos.some(
            t => t.requiredContracts.includes(contract.id)
          );
          if (!hasDependency) {
            violations.push({
              contractId: contract.id,
              type: 'unused_contract',
              message: `消费方 ${consumer} 未声明对契约 ${contract.name} 的依赖`,
              severity: 'warning',
            });
          }
        }
      }
    }

    return {
      consistent: violations.filter(v => v.severity !== 'warning').length === 0,
      violations,
    };
  }

  /**
   * 处理契约违反
   */
  async handleViolation(
    contract: Contract,
    violation: ContractViolation
  ): Promise<ContractResolution> {
    // 根据违反类型决定处理方式
    switch (violation.type) {
      case 'schema_mismatch':
        return await this.resolveSchemaConflict(contract, violation);
      case 'missing_implementation':
        return { action: 'block_consumer', message: '等待提供方实现' };
      case 'breaking_change':
        return { action: 'notify_consumers', message: '契约变更，请消费方确认' };
      default:
        return { action: 'log', message: violation.message };
    }
  }
}
```

---

## 五、与现有系统的映射

### 5.1 组件映射

| 现有组件 | 新组件 | 迁移策略 |
|----------|--------|----------|
| `OrchestratorAgent` | `MissionOrchestrator` | 重写，保留 IntentGate |
| `ExecutionPlan` | `Mission` | 重新设计 |
| `SubTask` | `Assignment` + `WorkerTodo` | 拆分 |
| `WorkerAgent` | `AutonomousWorker` | 增强 |
| `WorkerPool` | `WorkerCoordinator` | 重构 |
| `PolicyEngine` | `ContractManager` + `AssignmentManager` | 拆分 |
| `ProfileLoader` | `ProfileLoader` | 保留，增强使用 |
| `GuidanceInjector` | `GuidanceInjector` | 保留 |
| `RiskPolicy` | 整合到 `MissionOrchestrator` | 整合 |
| `VerificationRunner` | `VerificationRunner` | 保留 |

### 5.2 数据迁移

```typescript
// 现有 ExecutionPlan → 新 Mission
function migratePlanToMission(plan: ExecutionPlan): Mission {
  return {
    id: plan.id,
    goal: plan.summary || plan.analysis,
    analysis: plan.analysis,
    constraints: plan.acceptanceCriteria?.map(c => ({
      id: generateId(),
      type: 'should',
      description: c,
      source: 'user',
    })) || [],
    acceptanceCriteria: plan.acceptanceCriteria?.map(c => ({
      id: generateId(),
      description: c,
      verifiable: true,
      status: 'pending',
    })) || [],
    // ... 其他字段映射
  };
}

// 现有 SubTask → 新 Assignment + WorkerTodo
function migrateSubTaskToAssignment(subTask: SubTask): {
  assignment: Assignment;
  todos: WorkerTodo[];
} {
  const assignment: Assignment = {
    id: subTask.id,
    workerId: subTask.assignedWorker,
    responsibility: subTask.description,
    scope: {
      includes: [subTask.description],
      excludes: [],
      targetPaths: subTask.targetFiles,
    },
    // ...
  };

  const todo: WorkerTodo = {
    id: `${subTask.id}_todo_1`,
    assignmentId: subTask.id,
    content: subTask.prompt || subTask.description,
    reasoning: subTask.reason || '',
    type: inferTodoType(subTask),
    status: mapStatus(subTask.status),
    // ...
  };

  return { assignment, todos: [todo] };
}
```

---

## 六、实施计划

### 6.1 阶段划分

```
Phase 1: 基础设施（2 周）
├── 定义新数据模型（Mission, Contract, Assignment, WorkerTodo）
├── 实现 MissionStorage
├── 实现 ContractManager 基础功能
└── 实现 AssignmentManager 基础功能

Phase 2: Worker 自主性（2 周）
├── 重构 WorkerAgent → AutonomousWorker
├── 实现 Worker 自主规划（planWork）
├── 实现动态 Todo 添加
└── 实现规划审查机制

Phase 3: 编排器重构（2 周）
├── 实现 MissionOrchestrator
├── 整合 GoalParser
├── 整合 WorkerCoordinator
└── 实现新的执行流程

Phase 4: 契约系统（1 周）
├── 完善 ContractManager
├── 实现契约验证
├── 实现契约冲突检测
└── 实现契约违反处理

Phase 5: 集成与迁移（1 周）
├── UI 层适配
├── 数据迁移工具
├── 兼容层（可选）
└── 端到端测试

Phase 6: 优化与稳定（持续）
├── 性能优化
├── 错误处理完善
├── 用户反馈迭代
└── 文档完善
```

### 6.2 里程碑

| 里程碑 | 目标 | 验收标准 |
|--------|------|----------|
| M1 | 数据模型可用 | 能创建 Mission、Assignment、WorkerTodo |
| M2 | Worker 自主规划 | Worker 能自己生成 Todo 列表 |
| M3 | 端到端流程 | 能完成一个完整的多 Worker 协作任务 |
| M4 | 契约系统 | 契约验证和冲突检测工作正常 |
| M5 | 生产就绪 | 通过所有测试，性能达标 |

### 6.3 风险与缓解

| 风险 | 概率 | 影响 | 缓解措施 |
|------|------|------|----------|
| Worker 规划质量不稳定 | 高 | 高 | 增加规划审查，提供模板和示例 |
| 契约定义复杂度高 | 中 | 中 | 提供常见契约模板，简化定义流程 |
| 迁移期间功能降级 | 中 | 高 | 保留兼容层，渐进式迁移 |
| 性能下降（多轮交互） | 中 | 中 | 优化 Prompt，缓存规划结果 |

---

## 七、兼容性考虑

### 7.1 API 兼容层

```typescript
/**
 * 兼容层：让现有调用方无感迁移
 */
export class LegacyOrchestratorAdapter {
  private missionOrchestrator: MissionOrchestrator;

  /**
   * 兼容现有的 execute 方法
   */
  async execute(userPrompt: string, taskId: string, sessionId?: string): Promise<string> {
    return await this.missionOrchestrator.execute(userPrompt, sessionId || taskId);
  }

  /**
   * 兼容现有的 createPlan 方法
   */
  async createPlan(userPrompt: string, taskId: string, sessionId?: string): Promise<PlanRecord> {
    const mission = await this.missionOrchestrator.createMission(userPrompt, sessionId || taskId);
    return this.convertMissionToPlanRecord(mission);
  }

  /**
   * 兼容现有的 executePlan 方法
   */
  async executePlan(plan: ExecutionPlan, taskId: string, sessionId?: string): Promise<string> {
    const mission = this.convertPlanToMission(plan);
    return await this.missionOrchestrator.executeMission(mission);
  }
}
```

### 7.2 渐进式迁移

```typescript
/**
 * 功能开关：控制新旧架构切换
 */
export interface MigrationConfig {
  /** 是否启用新架构 */
  enableMissionArchitecture: boolean;
  /** 启用新架构的任务类型（渐进式） */
  enabledTaskTypes: string[];
  /** 是否记录对比数据 */
  enableComparison: boolean;
}

export class OrchestratorFactory {
  static create(config: MigrationConfig): IOrchestraor {
    if (config.enableMissionArchitecture) {
      return new MissionOrchestrator();
    }
    return new OrchestratorAgent();  // 现有实现
  }
}
```

---

## 八、画像驱动评审系统（增强）

> **来源**：结合 orchestrator-refactoring-analysis.md 分析补充

### 8.1 核心问题

当前评审机制与画像系统**脱节**：

```typescript
// 问题1: Plan Review 未利用画像信息
private async reviewPlan(plan): Promise<PlanReview> {
  // ❌ 未检查子任务分配是否符合 Worker 的 strengths
  // ❌ 未利用 CategoryConfig.riskLevel 调整评审严格度
}

// 问题2: 评审者选择太简单
private selectPeerReviewer(subTask: SubTask): CLIType {
  // ❌ 仅排除执行者后取第一个
  // ❌ 应该选择 strengths 匹配当前任务分类的 Worker
}
```

### 8.2 ProfileAwareReviewer（画像感知评审器）

```typescript
/**
 * 画像感知评审器
 * 让评审决策基于 Worker 画像
 */
export class ProfileAwareReviewer {
  constructor(
    private profileLoader: ProfileLoader,
    private policyEngine: PolicyEngine
  ) {}

  /**
   * 计划评审：检查任务分配是否符合 Worker 能力
   */
  async reviewPlan(plan: ExecutionPlan): Promise<PlanReviewResult> {
    const issues: PlanIssue[] = [];

    for (const task of plan.subTasks) {
      const profile = this.profileLoader.getProfile(task.assignedWorker);
      const category = this.inferCategory(task);

      // 1. 检查是否分配给了擅长该分类的 Worker
      if (!profile.preferences.preferredCategories.includes(category)) {
        issues.push({
          type: 'suboptimal_assignment',
          taskId: task.id,
          message: `任务分类 "${category}" 不在 ${task.assignedWorker} 的擅长领域`,
          suggestion: this.findBetterWorker(category),
        });
      }

      // 2. 检查任务是否涉及 Worker 的弱项
      const weaknessHit = profile.profile.weaknesses.find(w =>
        task.description.toLowerCase().includes(w.toLowerCase())
      );
      if (weaknessHit) {
        issues.push({
          type: 'weakness_match',
          taskId: task.id,
          message: `任务涉及 ${task.assignedWorker} 的弱项: "${weaknessHit}"`,
          reviewLevel: 'strict', // 需要更严格的评审
        });
      }
    }

    return { issues, approved: issues.filter(i => i.type === 'critical').length === 0 };
  }

  /**
   * 互检评审者选择：基于能力画像匹配
   */
  selectPeerReviewer(task: SubTask, executor: CLIType): CLIType {
    const category = this.inferCategory(task);
    const allProfiles = this.profileLoader.getAllProfiles();

    // 选择擅长该分类且不是执行者的 Worker
    const candidates = [...allProfiles.entries()]
      .filter(([cli]) => cli !== executor)
      .filter(([_, profile]) =>
        profile.preferences.preferredCategories.includes(category)
      )
      .sort((a, b) => {
        // 优先选择该分类是第一优先的 Worker
        const aIndex = a[1].preferences.preferredCategories.indexOf(category);
        const bIndex = b[1].preferences.preferredCategories.indexOf(category);
        return aIndex - bIndex;
      });

    return candidates[0]?.[0] || (executor === 'claude' ? 'codex' : 'claude');
  }

  /**
   * 评审严格度：基于分类风险 + Worker 弱项
   */
  determineReviewLevel(task: SubTask, executor: CLIType): ReviewLevel {
    const category = this.profileLoader.getCategory(this.inferCategory(task));
    const profile = this.profileLoader.getProfile(executor);

    // 基础严格度来自分类风险
    let level: ReviewLevel = category?.riskLevel === 'high' ? 'strict'
                           : category?.riskLevel === 'medium' ? 'standard'
                           : 'light';

    // 如果任务涉及 Worker 弱项，提升严格度
    const involvesWeakness = profile.profile.weaknesses.some(w =>
      task.description.toLowerCase().includes(w.toLowerCase())
    );
    if (involvesWeakness && level !== 'strict') {
      level = level === 'light' ? 'standard' : 'strict';
    }

    return level;
  }
}
```

### 8.3 GuidanceInjector 扩展

```typescript
/**
 * 扩展 GuidanceInjector，支持评审上下文
 */
export class EnhancedGuidanceInjector extends GuidanceInjector {
  /**
   * 构建自检引导 Prompt
   * 基于 Worker 弱项定制检查清单
   */
  buildSelfCheckGuidance(profile: WorkerProfile, task: SubTask): string {
    const sections: string[] = [];

    // 1. 基于 Worker 弱项的重点检查
    if (profile.profile.weaknesses.length > 0) {
      sections.push(`## 重点自检（你的相对弱项）`);
      sections.push(`请特别检查以下方面：`);
      profile.profile.weaknesses.forEach(w => {
        sections.push(`- ${w}`);
      });
    }

    // 2. 基于协作规则的输出检查
    sections.push(`## 协作规范检查`);
    profile.collaboration.asCollaborator.forEach(rule => {
      sections.push(`- [ ] ${rule}`);
    });

    return sections.join('\n');
  }

  /**
   * 构建互检引导 Prompt
   * 利用评审者专长视角
   */
  buildPeerReviewGuidance(
    reviewerProfile: WorkerProfile,
    executorProfile: WorkerProfile,
    task: SubTask
  ): string {
    const sections: string[] = [];

    // 1. 利用评审者的专长
    sections.push(`## 你的专长检查视角`);
    sections.push(`作为 ${reviewerProfile.name}，请重点从以下专长领域审查：`);
    reviewerProfile.profile.strengths.forEach(s => {
      sections.push(`- ${s}`);
    });

    // 2. 针对执行者弱项的检查
    sections.push(`\n## 执行者弱项重点审查`);
    sections.push(`执行者 ${executorProfile.name} 在以下方面相对较弱，请重点检查：`);
    executorProfile.profile.weaknesses.forEach(w => {
      sections.push(`- ${w}`);
    });

    return sections.join('\n');
  }
}
```

### 8.4 类型扩展

**WorkerProfile 评审扩展**：

```typescript
// src/orchestrator/profile/types.ts 扩展
interface WorkerProfile {
  // ... 现有字段 ...

  // 🆕 评审相关配置
  review?: {
    /** 作为被评审者时需要重点检查的方面 */
    focusAreasWhenReviewed: string[];
    /** 作为评审者时的专长视角 */
    reviewStrengths: string[];
    /** 需要更严格评审的任务类型 */
    strictReviewCategories: string[];
  };
}
```

**CategoryConfig 评审扩展**：

```typescript
// src/orchestrator/profile/types.ts 扩展
interface CategoryConfig {
  // ... 现有字段 ...

  // 🆕 评审策略
  reviewPolicy?: {
    /** 是否强制互检 */
    requirePeerReview: boolean;
    /** 推荐的评审 Worker */
    preferredReviewer?: CLIType;
    /** 评审重点 */
    reviewFocus: string[];
  };
}
```

### 8.5 画像感知失败恢复

```typescript
/**
 * 画像感知的失败恢复
 * 失败与 Worker 弱项相关时，考虑换 Worker 重试
 */
class ProfileAwareRecoveryHandler {
  async handleReviewFailure(
    task: SubTask,
    failure: ReviewFailure,
    executor: CLIType
  ): Promise<RecoveryAction> {
    const profile = this.profileLoader.getProfile(executor);

    // 检查失败是否与 Worker 弱项相关
    const isWeaknessRelated = profile.profile.weaknesses.some(w =>
      failure.message.toLowerCase().includes(w.toLowerCase()) ||
      task.description.toLowerCase().includes(w.toLowerCase())
    );

    if (isWeaknessRelated) {
      // 弱项相关失败：换 Worker 重试
      const betterWorker = this.findWorkerWithStrength(category);
      if (betterWorker && betterWorker !== executor) {
        return {
          action: 'reassign',
          newWorker: betterWorker,
          reason: `任务涉及 ${executor} 的弱项，转交给更擅长的 ${betterWorker}`,
        };
      }
    }

    // 非弱项相关失败：使用原有恢复逻辑
    return this.recoveryHandler.handleFailure(task, failure);
  }
}
```

---

## 九、附录

### A. 术语表

| 术语 | 定义 |
|------|------|
| Mission | 用户的目标 + 约束 + 验收标准，是整个任务的顶层抽象 |
| Contract | Worker 之间的协作约定，包括 API、数据结构、事件等 |
| Assignment | 分配给某个 Worker 的职责范围，不是具体任务 |
| WorkerTodo | Worker 自己规划的具体工作项 |
| Goal | 用户想要达成的结果 |
| Constraint | 必须遵守的约束条件 |
| AcceptanceCriterion | 验收标准，用于判断任务是否完成 |

### B. 参考资料

- [oh-my-opencode Sisyphus 架构](https://github.com/code-yeongyu/oh-my-opencode)
- [现有 MultiCLI 架构文档](./Orchestrator-Agent-架构分析报告.md)

### C. 变更历史

| 版本 | 日期 | 变更内容 |
|------|------|----------|
| 1.0 | 2026-01-19 | 初始版本 |

---

**文档结束**
