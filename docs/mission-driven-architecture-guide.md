# Mission-Driven Architecture 使用指南

> **版本**: 1.0.0
> **更新日期**: 2026-01-19
> **前置阅读**: [API 文档](./mission-driven-architecture-api.md)

---

## 快速开始

### 1. 基本使用

```typescript
import { IntelligentOrchestrator } from './orchestrator';

// 创建编排器（默认使用 MissionDrivenEngine）
const orchestrator = new IntelligentOrchestrator({
  workspaceRoot: '/path/to/project',
  // ... 其他配置
});

// 执行任务
const result = await orchestrator.execute(
  '重构用户认证模块',
  context,
  {
    onConfirmation: async (plan) => true,
    onClarification: async (questions) => ({ /* answers */ }),
  }
);
```

### 2. 理解执行流程

新架构采用 9 阶段执行流程：

```
┌─────────────────────────────────────────────────────────────┐
│                    Mission 执行流程                          │
├─────────────────────────────────────────────────────────────┤
│  Phase 1: 意图识别 (IntentGate)                             │
│     ↓                                                       │
│  Phase 2: 目标理解 (GoalParser)                             │
│     ↓                                                       │
│  Phase 3: 协作规划 (Contract + Assignment)                  │
│     ↓                                                       │
│  Phase 4: Worker 自主规划 (WorkerTodo 生成)                 │
│     ↓                                                       │
│  Phase 5: 规划审查 (ProfileAwareReviewer)                   │
│     ↓                                                       │
│  Phase 6: 用户确认                                          │
│     ↓                                                       │
│  Phase 7: 任务执行 (MissionExecutor)                        │
│     ↓                                                       │
│  Phase 8: 结果验证 (VerificationRunner)                     │
│     ↓                                                       │
│  Phase 9: 总结汇报                                          │
└─────────────────────────────────────────────────────────────┘
```

---

## 核心概念详解

### Mission（任务）

Mission 是最高层抽象，代表用户的完整意图。

```typescript
const mission = await orchestrator.createMission({
  prompt: '添加用户登录功能',
  sessionId: 'session-123',
  context: { /* 上下文信息 */ },
});

// Mission 包含：
// - goal: 目标描述
// - constraints: 约束条件（如技术栈、时间限制）
// - acceptanceCriteria: 验收标准
// - contracts: Worker 间的协作契约
// - assignments: 职责分配
```

### Contract（契约）

契约定义 Worker 之间的协作接口。

```typescript
// 契约类型
type ContractType = 'api' | 'data' | 'file' | 'event';

// 示例：API 契约
const contract: Contract = {
  id: 'auth-api-contract',
  type: 'api',
  name: '认证 API 接口',
  specification: {
    endpoint: '/api/auth/login',
    method: 'POST',
    request: { username: 'string', password: 'string' },
    response: { token: 'string', expiresIn: 'number' },
  },
  producer: 'backend-worker',
  consumers: ['frontend-worker'],
  status: 'draft',
};
```

**契约生命周期**:
```
draft → proposed → agreed → implemented → verified
```

### Assignment（职责分配）

Assignment 定义 Worker 的职责范围。

```typescript
const assignment: Assignment = {
  id: 'frontend-assignment',
  missionId: mission.id,
  workerId: 'claude',
  role: '前端开发',
  scope: '登录页面 UI 和表单验证',
  providesContracts: [],          // 提供的契约
  consumesContracts: ['auth-api'], // 消费的契约
  todos: [],                       // 自主规划的执行项
  status: 'pending',
};
```

### WorkerTodo（执行项）

Worker 自主规划的具体执行步骤。

```typescript
const todo: WorkerTodo = {
  id: 'todo-1',
  assignmentId: assignment.id,
  description: '创建登录表单组件',
  estimatedComplexity: 'medium',
  status: 'pending',
  dependencies: [],
  isOutOfScope: false,
  output: null,
};
```

---

## 常见场景

### 场景 1：单 Worker 任务

```typescript
// 简单任务，单个 Worker 即可完成
const result = await orchestrator.execute(
  '修复登录按钮样式问题',
  context
);

// 流程：
// 1. 识别为简单任务 → 分配给单个 Worker
// 2. Worker 自主规划 Todo 列表
// 3. 依次执行 Todo
// 4. 验证结果
```

### 场景 2：多 Worker 协作

```typescript
// 复杂任务，需要多个 Worker 协作
const result = await orchestrator.execute(
  '实现完整的用户认证系统，包括前后端',
  context
);

// 流程：
// 1. 识别需要多 Worker → 选择 frontend、backend Worker
// 2. 定义契约（API 接口规范）
// 3. 分配职责
// 4. 各 Worker 自主规划
// 5. 并行执行（契约依赖自动处理）
// 6. 验证契约一致性
```

### 场景 3：处理动态 Todo

```typescript
// Worker 执行中发现需要额外工作
const assignment = mission.assignments[0];

// 添加动态 Todo
const { added, blocked } = await worker.addDynamicTodo(
  assignment,
  {
    description: '添加密码强度检测',
    reason: '用户需求变更',
  }
);

if (blocked) {
  // 超出范围，需要审批
  console.log('需要审批:', blocked.blockingReason);
}
```

### 场景 4：失败恢复

```typescript
// 配置失败恢复策略
const result = await orchestrator.execute(prompt, context, {
  recoveryOptions: {
    maxRetries: 3,
    retryDelay: 1000,
    shouldRetry: (error) => !error.isFatal,
  },
  onRecoveryConfirmation: async (strategy) => {
    console.log('恢复策略:', strategy);
    return true; // 确认恢复
  },
});
```

---

## 事件监听

### 可用事件

```typescript
// Mission 级别事件
orchestrator.on('missionCreated', ({ mission }) => { });
orchestrator.on('missionPhaseChanged', ({ missionId, phase }) => { });
orchestrator.on('missionCompleted', ({ mission, summary }) => { });
orchestrator.on('missionFailed', ({ mission, error }) => { });

// 执行进度事件
executor.on('progress', (progress: ExecutionProgress) => {
  console.log(`进度: ${progress.completedTodos}/${progress.totalTodos}`);
  console.log(`当前: ${progress.currentTodo?.description}`);
});

// Todo 级别事件
executor.on('todoStarted', ({ todo }) => { });
executor.on('todoCompleted', ({ todo, output }) => { });
executor.on('todoFailed', ({ todo, error }) => { });
```

### UI 集成示例

```typescript
// WebviewProvider 事件适配
orchestrator.on('missionPhaseChanged', ({ phase }) => {
  webview.postMessage({
    type: 'phase-update',
    phase: phase,
  });
});

executor.on('progress', (progress) => {
  webview.postMessage({
    type: 'progress-update',
    current: progress.completedTodos,
    total: progress.totalTodos,
    percentage: progress.progressPercentage,
  });
});
```

---

## 画像驱动特性

### Worker 能力匹配

```typescript
// Worker 画像包含能力信息
const profile: WorkerProfile = {
  id: 'claude',
  name: 'Claude',
  strengths: ['architecture', 'refactoring', 'testing'],
  weaknesses: ['ui-design'],
  review: {
    focusAreasWhenReviewed: ['edge-cases', 'error-handling'],
    reviewStrengths: ['code-quality', 'best-practices'],
  },
};

// 编排器根据画像选择最合适的 Worker
// 架构任务 → Claude（strengths 包含 architecture）
// UI 任务 → 其他更适合的 Worker
```

### 智能评审

```typescript
// 基于画像的评审
const reviewer = new ProfileAwareReviewer(profileLoader);

// 选择最佳评审者
const bestReviewer = reviewer.selectPeerReviewer(
  workerProfile,      // 被评审者画像
  ['testing'],        // 任务类别
  availableWorkers    // 可用评审者列表
);

// 确定评审严格度
const level = reviewer.determineReviewLevel(
  workerProfile,
  assignment,
  riskLevel
);
// 返回: 'thorough' | 'standard' | 'quick'
```

---

## 缓存和性能

### 规划缓存

```typescript
// 相同 prompt + sessionId 会使用缓存
const cached = orchestrator.getCachedPlanning(prompt, sessionId);
if (cached) {
  console.log('使用缓存的规划结果');
  return cached;
}

// 缓存新规划
orchestrator.cachePlanning(prompt, sessionId, mission);

// 清理缓存
orchestrator.clearCache();
```

### 并行执行

```typescript
// MissionExecutor 自动并行执行无依赖的 Todo
const result = await executor.execute(mission, {
  parallelExecution: true,  // 启用并行
  maxParallel: 3,           // 最大并行数
});
```

---

## 迁移指南

### 从旧架构迁移

```typescript
// 旧架构（已废弃）
const agent = new OrchestratorAgent(config);
const plan = await agent.createPlan(prompt, context);
const result = await agent.executePlan(plan);

// 新架构（MissionDrivenEngine 是唯一实现）
const orchestrator = new IntelligentOrchestrator({
  // 配置选项
});

// API 保持兼容
const result = await orchestrator.execute(prompt, context);
```

> **注意**: 旧架构文件（OrchestratorAgent, WorkerAgent, WorkerPool）已完全删除。
> MissionDrivenEngine 现在是唯一的编排引擎实现。

---

## 故障排查

### 常见问题

**1. 契约验证失败**
```typescript
// 检查契约状态
const violations = contractManager.verifyContractConsistency(
  mission.contracts,
  mission.assignments
);

if (violations.length > 0) {
  console.log('契约问题:', violations);
  // 处理违规：重新协商或修改分配
}
```

**2. Worker 规划超出范围**
```typescript
// 启用严格的范围检查
const worker = new AutonomousWorker({
  strictScopeCheck: true,
  onOutOfScope: async (todo) => {
    // 自动阻塞并请求审批
    return { approved: false, reason: '超出范围' };
  },
});
```

**3. 执行超时**
```typescript
// 配置超时
const result = await executor.execute(mission, {
  timeout: 5 * 60 * 1000,  // 5 分钟超时
  onTimeout: async () => {
    // 保存当前进度
    await missionStorage.save(mission);
    throw new Error('执行超时');
  },
});
```

---

## 最佳实践

1. **明确定义契约** - 在多 Worker 协作前，确保契约规范清晰
2. **使用画像匹配** - 让编排器根据 Worker 画像自动选择最佳执行者
3. **启用事件监听** - 实时跟踪执行进度，及时发现问题
4. **配置恢复策略** - 为长时间任务配置合理的失败恢复策略
5. **利用缓存** - 对于重复的规划请求，利用缓存减少开销

---

## 相关文档

- [API 文档](./mission-driven-architecture-api.md)
- [重构计划](./orchestrator-refactoring-plan.md)
- [检查清单](./orchestrator-refactoring-checklist.md)
