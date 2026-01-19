# Mission-Driven Architecture API 文档

> **版本**: 1.0.0
> **更新日期**: 2026-01-19

---

## 概述

Mission-Driven Architecture 是 MultiCLI 的新编排架构，采用 Mission/Contract/Assignment/WorkerTodo 层次结构来管理复杂的多 Worker 协作任务。

### 核心概念

| 概念 | 说明 |
|------|------|
| **Mission** | 任务的最高层抽象，包含目标、约束、验收标准等 |
| **Contract** | Worker 之间的协作契约，定义 API/数据接口规范 |
| **Assignment** | 分配给特定 Worker 的职责范围 |
| **WorkerTodo** | Worker 自主规划的具体执行项 |

---

## 核心组件

### 1. MissionOrchestrator

任务编排核心，负责 Mission 生命周期管理。

```typescript
import { MissionOrchestrator } from './orchestrator/core';

const orchestrator = new MissionOrchestrator(
  profileLoader,
  guidanceInjector,
  missionStorage,
  workspaceRoot
);
```

#### 主要方法

| 方法 | 说明 |
|------|------|
| `createMission(params)` | 创建新 Mission |
| `understandGoal(mission, analysis)` | 理解并设置 Mission 目标 |
| `selectParticipants(mission)` | 选择参与的 Worker |
| `defineContracts(mission, participants)` | 定义 Worker 间契约 |
| `assignResponsibilities(mission, participants)` | 分配职责 |
| `approveMission(missionId)` | 批准 Mission 开始执行 |
| `verifyMission(missionId)` | 验证 Mission 执行结果 |
| `summarizeMission(missionId)` | 生成 Mission 总结 |
| `getCachedPlanning(prompt, sessionId)` | 获取缓存的规划结果 |
| `cachePlanning(prompt, sessionId, mission)` | 缓存规划结果 |

#### 事件

| 事件 | 数据 | 说明 |
|------|------|------|
| `missionCreated` | `{ mission }` | Mission 创建完成 |
| `missionPhaseChanged` | `{ missionId, phase }` | Mission 阶段变更 |
| `missionApproved` | `{ missionId }` | Mission 被批准 |
| `missionCompleted` | `{ missionId, summary }` | Mission 完成 |
| `contractDefined` | `{ missionId, contractId }` | 契约定义完成 |
| `assignmentCreated` | `{ missionId, assignmentId }` | 职责分配完成 |

---

### 2. MissionExecutor

任务执行器，负责执行 Mission 中的所有 Assignment。

```typescript
import { MissionExecutor } from './orchestrator/core';

const executor = new MissionExecutor(
  missionOrchestrator,
  profileLoader,
  guidanceInjector
);

const result = await executor.execute(mission, {
  workingDirectory: '/path/to/workspace',
  timeout: 300000,
  onProgress: (progress) => console.log(progress),
  onOutput: (workerId, output) => console.log(workerId, output),
});
```

#### 执行选项

```typescript
interface ExecutionOptions {
  workingDirectory: string;
  timeout?: number;
  onProgress?: (progress: ExecutionProgress) => void;
  onOutput?: (workerId: CLIType, output: string) => void;
}
```

#### 事件

| 事件 | 数据 | 说明 |
|------|------|------|
| `progress` | `ExecutionProgress` | 执行进度更新 |
| `workerOutput` | `{ workerId, output }` | Worker 输出 |
| `assignmentStarted` | `{ missionId, assignmentId }` | Assignment 开始执行 |
| `assignmentCompleted` | `{ missionId, assignmentId }` | Assignment 执行完成 |
| `todoCompleted` | `{ missionId, assignmentId, todoId }` | Todo 完成 |
| `contractVerified` | `{ missionId, contractId, passed }` | 契约验证结果 |

---

### 3. MissionDrivenEngine

适配层，提供与 OrchestratorAgent 兼容的接口。

```typescript
import { MissionDrivenEngine } from './orchestrator/core';

const engine = new MissionDrivenEngine(
  cliFactory,
  config,
  workspaceRoot,
  snapshotManager,
  sessionManager
);

await engine.initialize();
const result = await engine.execute(userPrompt, taskId, sessionId);
```

#### 主要方法

| 方法 | 说明 |
|------|------|
| `execute(userPrompt, taskId, sessionId?)` | 执行完整任务流程 |
| `createPlan(userPrompt, taskId, sessionId?)` | 仅创建执行计划 |
| `executePlan(plan, taskId, sessionId?, prompt?)` | 执行已有计划 |
| `cancel()` | 取消当前任务 |
| `getActivePlanForSession(sessionId)` | 获取会话活跃计划 |

#### 回调设置

```typescript
engine.setConfirmationCallback(async (plan, formatted) => {
  // 用户确认逻辑
  return true;
});

engine.setClarificationCallback(async (questions, context, score, prompt) => {
  // 需求澄清逻辑
  return { answers: { q1: 'answer1' } };
});

engine.setWorkerQuestionCallback(async (workerId, question, context) => {
  // Worker 提问回调
  return 'user answer';
});
```

---

### 4. AutonomousWorker

自主 Worker，负责规划和执行具体任务。

```typescript
import { AutonomousWorker } from './orchestrator/worker';

const worker = new AutonomousWorker(
  workerId,
  profileLoader,
  guidanceInjector,
  cliFactory,
  snapshotManager
);

const todos = await worker.planWork(assignment, context);
const output = await worker.executeTodo(todo, options);
```

#### 主要方法

| 方法 | 说明 |
|------|------|
| `planWork(assignment, context)` | 规划 Todo 列表 |
| `executeTodo(todo, options)` | 执行单个 Todo |
| `addDynamicTodo(todo)` | 动态添加 Todo（需审批） |
| `planRecovery(failedTodo, error)` | 规划失败恢复 |

---

### 5. ContractManager

契约管理器，负责契约的创建、验证和状态管理。

```typescript
import { ContractManager } from './orchestrator/mission';

const manager = new ContractManager();

const contract = manager.createContract({
  missionId: 'mission-1',
  type: 'api',
  name: 'User API Contract',
  description: 'User management API',
  producer: 'claude',
  consumers: ['codex'],
});

const consistency = await manager.verifyContractConsistency(mission);
```

#### 契约类型

| 类型 | 说明 |
|------|------|
| `api` | API 接口契约 |
| `data` | 数据模型契约 |
| `file` | 文件输出契约 |
| `event` | 事件契约 |

---

### 6. AssignmentManager

职责分配管理器。

```typescript
import { AssignmentManager } from './orchestrator/mission';

const manager = new AssignmentManager(profileLoader, guidanceInjector);

const assignments = await manager.createAssignments(mission, participants, contracts);
assignment = manager.addTodo(assignment, todo);
assignment = manager.updateTodo(assignment, updatedTodo);
const nextTodo = manager.getNextExecutableTodo(assignment);
```

---

## 数据模型

### Mission

```typescript
interface Mission {
  id: string;
  sessionId: string;
  userPrompt: string;
  goal: string;
  analysis: string;
  context: string;
  constraints: Constraint[];
  acceptanceCriteria: AcceptanceCriterion[];
  contracts: Contract[];
  assignments: Assignment[];
  riskLevel: 'low' | 'medium' | 'high';
  riskFactors: string[];
  executionPath: 'light' | 'standard' | 'comprehensive';
  status: MissionStatus;
  phase: MissionPhase;
  createdAt: number;
  updatedAt: number;
}
```

### Contract

```typescript
interface Contract {
  id: string;
  missionId: string;
  type: 'api' | 'data' | 'file' | 'event';
  name: string;
  description: string;
  specification: ContractSpecification;
  producer: CLIType;
  consumers: CLIType[];
  status: ContractStatus;
}
```

### Assignment

```typescript
interface Assignment {
  id: string;
  missionId: string;
  workerId: CLIType;
  assignmentReason: AssignmentReason;
  responsibility: string;
  scope: AssignmentScope;
  guidancePrompt: string;
  producerContracts: string[];
  consumerContracts: string[];
  todos: WorkerTodo[];
  planningStatus: PlanningStatus;
  status: AssignmentStatus;
  progress: number;
  createdAt: number;
}
```

### WorkerTodo

```typescript
interface WorkerTodo {
  id: string;
  assignmentId: string;
  content: string;
  reasoning: string;
  expectedOutput: string;
  type: TodoType;
  priority: number;
  outOfScope: boolean;
  approvalStatus?: 'pending' | 'approved' | 'rejected';
  dependsOn: string[];
  requiredContracts: string[];
  producesContracts: string[];
  status: TodoStatus;
  createdAt: number;
  completedAt?: number;
}
```

---

## 状态机

### Mission 状态

```
draft → pending_approval → approved → executing → completed
                                   ↘ failed
```

### Mission 阶段

```
goal_understanding → collaboration_planning → worker_planning →
plan_review → execution → verification → summary
```

### Assignment 状态

```
pending → planning → ready → executing → completed
                          ↘ blocked → executing
                          ↘ failed → pending (重试)
```

### Todo 状态

```
pending → in_progress → completed
                     ↘ failed → pending (重试)
                     ↘ skipped
```

---

## 使用示例

### 基本使用

```typescript
import { IntelligentOrchestrator } from './orchestrator';

const orchestrator = new IntelligentOrchestrator(
  cliFactory,
  {
    timeout: 300000,
  },
  workspaceRoot,
  sessionManager,
  snapshotManager
);

await orchestrator.initialize();

orchestrator.setConfirmationCallback(async (plan, formatted) => {
  console.log('执行计划:', formatted);
  return true;
});

const result = await orchestrator.execute(
  '实现用户认证功能',
  'task-123',
  'session-456'
);

console.log('执行结果:', result);
```

### 仅规划

```typescript
const planRecord = await orchestrator.createPlan(
  '重构数据库访问层',
  'task-124',
  'session-456'
);

console.log('计划详情:', planRecord.formattedPlan);

// 稍后执行
const result = await orchestrator.executePlan(
  planRecord,
  'task-124',
  'session-456'
);
```

---

## 配置选项

```typescript
interface OrchestratorConfig {
  timeout?: number;           // 执行超时（毫秒）
  maxRetries?: number;        // 最大重试次数
  review?: {
    enabled?: boolean;
    requireApproval?: string[];
  };
  planReview?: {
    enabled?: boolean;
    reviewer?: CLIType;
  };
  verification?: VerificationConfig;
  strategy?: StrategyConfig;
}
```

---

## 错误处理

新架构提供以下错误恢复机制：

1. **ProfileAwareRecoveryHandler**: 基于 Worker 画像的智能恢复
2. **MissionExecutor.executeWithRetry()**: 自动重试失败的 Todo
3. **ContractManager.verifyContractConsistency()**: 契约一致性验证

---

## 迁移指南

从旧架构迁移到新架构：

1. 旧架构文件已完全删除，MissionDrivenEngine 是唯一实现
2. 无需修改回调函数，接口保持兼容
3. 新架构会自动转换 Mission 为兼容的 ExecutionPlan

---

## 变更记录

| 版本 | 日期 | 变更 |
|------|------|------|
| 1.0.0 | 2026-01-19 | 初始版本，包含完整的 Mission-Driven Architecture |
