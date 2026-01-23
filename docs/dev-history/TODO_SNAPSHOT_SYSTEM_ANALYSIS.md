# TODO 系统和快照系统全面分析

## 📋 执行日期

**分析日期**: 2025-01-22
**分析目标**: 检查 todo 系统和快照系统是否完全支持 LLM 模式的编排

---

## 🎯 当前架构理解

### 1. 编排架构

```
用户输入
    ↓
IntelligentOrchestrator (智能编排器)
    ↓
MissionDrivenEngine (任务驱动引擎)
    ↓
MissionOrchestrator (任务编排器) + MissionExecutor (任务执行器)
    ↓
AutonomousWorker (自主 Worker)
    ↓
LLM Adapter (Claude/Codex/Gemini)
```

### 2. 数据存储架构

```
.multicli/
├── sessions/
│   └── {sessionId}/
│       ├── session.json          # 会话元数据
│       ├── messages.json         # 消息历史
│       ├── snapshots/            # 文件快照
│       │   └── {snapshotId}.snapshot
│       ├── plans/                # 执行计划 TODO
│       │   └── {planId}.md
│       └── missions/             # Mission 记录
│           └── {missionId}.json
└── knowledge/                    # 项目知识库
    ├── code-index.json
    ├── adrs/
    └── faqs/
```

---

## 🔍 问题分析

### 问题 1: PlanTodoManager 与 LLM 模式不兼容

**文件**: `src/orchestrator/plan-todo.ts`

**当前实现**:
```typescript
export class PlanTodoManager {
  ensurePlanFile(record: PlanRecord): void {
    // 基于 PlanRecord 生成 TODO 文件
    // PlanRecord 来自旧的 OrchestratorAgent
  }

  updateSubTaskStatus(sessionId: string, planId: string, subTaskId: string, status: 'completed' | 'failed'): void {
    // 更新 TODO 文件中的任务状态
  }
}
```

**问题**:
1. ❌ **依赖 PlanRecord**: PlanRecord 是旧架构的产物，新的 MissionDrivenEngine 使用 Mission 对象
2. ❌ **没有与 Mission 集成**: 无法从 Mission 对象生成 TODO 文件
3. ❌ **没有与 MissionExecutor 集成**: 任务状态更新不会同步到 TODO 文件
4. ❌ **文件路径硬编码**: 使用 `plans/{planId}.md`，但 Mission 使用 `missions/{missionId}.json`

**影响**:
- 用户看不到当前任务的 TODO 列表
- 任务进度无法通过 TODO 文件追踪
- Mission 执行与 TODO 系统脱节

---

### 问题 2: SnapshotManager 与 Mission 集成不完整

**文件**: `src/snapshot-manager.ts`

**当前实现**:
```typescript
export class SnapshotManager {
  clearSnapshotsForFiles(filePaths: string[], keepSubTaskId?: string): number {
    // 清理指定文件的历史快照
    // 使用 subTaskId 作为标识
  }
}
```

**问题**:
1. ❌ **使用 subTaskId**: Mission 架构中使用 `assignmentId` 和 `todoId`，不是 `subTaskId`
2. ❌ **快照元数据不完整**: FileSnapshotMeta 缺少 Mission 相关字段
3. ❌ **没有 Mission 级别的快照管理**: 无法按 Mission 清理或查询快照

**影响**:
- 快照清理逻辑可能失效
- 无法追踪快照属于哪个 Mission
- 快照恢复时缺少上下文信息

---

### 问题 3: UnifiedSessionManager 的快照元数据过时

**文件**: `src/session/unified-session-manager.ts`

**当前 FileSnapshotMeta**:
```typescript
export interface FileSnapshotMeta {
  id: string;
  filePath: string;
  timestamp: number;
  taskId?: string;
  subTaskId?: string;  // ❌ 旧架构字段
  agentType?: AgentType;
  reason?: string;
}
```

**问题**:
1. ❌ **缺少 Mission 字段**: 没有 `missionId`、`assignmentId`、`todoId`
2. ❌ **使用 subTaskId**: 应该使用 Mission 架构的标识符
3. ❌ **缺少 Worker 信息**: 没有记录是哪个 Worker 创建的快照

**影响**:
- 快照无法关联到 Mission
- 无法按 Mission 查询或恢复快照
- 快照元数据不完整，影响调试和恢复

---

### 问题 4: MissionExecutor 没有更新 TODO 系统

**文件**: `src/orchestrator/core/mission-executor.ts`

**当前实现**:
- ✅ 执行 Mission 中的 Assignment
- ✅ 调用 AutonomousWorker 执行任务
- ❌ **没有生成 TODO 文件**
- ❌ **没有更新 TODO 状态**

**问题**:
1. ❌ **缺少 TODO 集成**: MissionExecutor 不调用 PlanTodoManager
2. ❌ **任务进度不可见**: 用户无法通过 TODO 文件查看进度
3. ❌ **与旧系统不一致**: 旧的 OrchestratorAgent 会生成 TODO 文件

**影响**:
- 用户体验下降（看不到 TODO 列表）
- 无法通过文件系统追踪任务进度
- 与文档中描述的功能不符

---

### 问题 5: AutonomousWorker 的快照创建使用旧标识符

**文件**: `src/orchestrator/worker/autonomous-worker.ts`

**当前实现**:
```typescript
// 创建快照时使用 subTaskId
await this.snapshotManager.createSnapshot(
  filePath,
  taskId,
  subTaskId,  // ❌ 应该使用 assignmentId 或 todoId
  this.cliType,
  reason
);
```

**问题**:
1. ❌ **使用 subTaskId**: Mission 架构中应该使用 `assignmentId` 和 `todoId`
2. ❌ **缺少 Mission 上下文**: 快照不知道属于哪个 Mission
3. ❌ **标识符不一致**: 与 Mission 架构的命名不一致

**影响**:
- 快照无法正确关联到 Mission
- 快照清理和恢复逻辑可能失效
- 代码可读性和维护性下降

---

## 🎯 修复方案

### 方案 1: 重构 PlanTodoManager 支持 Mission

**目标**: 让 PlanTodoManager 能够从 Mission 对象生成和更新 TODO 文件

**实现步骤**:

1. **添加 Mission 支持**:
```typescript
export class PlanTodoManager {
  // 新增：从 Mission 生成 TODO 文件
  ensureMissionTodoFile(mission: Mission, sessionId: string): void {
    this.ensureDir(sessionId);
    const todoPath = this.getTodoPath(sessionId, mission.id);
    if (fs.existsSync(todoPath)) {
      return;
    }

    const lines: string[] = [];
    lines.push(`# Mission: ${mission.id}`);
    lines.push('');
    lines.push(`Goal: ${mission.goal}`);
    lines.push(`Status: ${mission.status}`);
    lines.push(`Created: ${new Date(mission.createdAt).toISOString()}`);
    lines.push('');
    lines.push('## Assignments');

    for (const assignment of mission.assignments || []) {
      lines.push(`### ${assignment.responsibility} [${assignment.worker}]`);
      lines.push('');
      for (const todo of assignment.todos || []) {
        const marker = todo.status === 'completed' ? 'x' : ' ';
        lines.push(`- [${marker}] (${todo.id}) ${todo.content}`);
      }
      lines.push('');
    }

    fs.writeFileSync(todoPath, lines.join('\n'), 'utf-8');
  }

  // 新增：更新 Mission TODO 状态
  updateMissionTodoStatus(
    sessionId: string,
    missionId: string,
    todoId: string,
    status: 'completed' | 'failed'
  ): void {
    const todoPath = this.getTodoPath(sessionId, missionId);
    if (!fs.existsSync(todoPath)) {
      return;
    }

    const content = fs.readFileSync(todoPath, 'utf-8');
    const lines = content.split('\n');
    const nextLines = lines.map(line => {
      if (!line.startsWith('- [')) {
        return line;
      }
      if (!line.includes(`(${todoId})`)) {
        return line;
      }
      const marker = status === 'completed' ? 'x' : '!';
      const stripped = line.replace(/^- \[[ x!]?\]\s*/, '');
      const suffix = status === 'failed' && !stripped.includes('[FAILED]') ? ' [FAILED]' : '';
      return `- [${marker}] ${stripped}${suffix}`;
    });

    fs.writeFileSync(todoPath, nextLines.join('\n'), 'utf-8');
  }

  // 保留旧方法以兼容 PlanRecord（逐步废弃）
  ensurePlanFile(record: PlanRecord): void {
    // ... 保持不变
  }
}
```

2. **在 MissionExecutor 中集成**:
```typescript
export class MissionExecutor extends EventEmitter {
  private todoManager: PlanTodoManager;

  constructor(...) {
    // ...
    this.todoManager = new PlanTodoManager(workspaceRoot);
  }

  async executeMission(mission: Mission): Promise<void> {
    // 生成 TODO 文件
    const sessionId = this.sessionManager.getCurrentSession()?.id;
    if (sessionId) {
      this.todoManager.ensureMissionTodoFile(mission, sessionId);
    }

    // 执行任务...
    for (const assignment of mission.assignments) {
      for (const todo of assignment.todos) {
        try {
          await this.executeTodo(assignment, todo);

          // 更新 TODO 状态
          if (sessionId) {
            this.todoManager.updateMissionTodoStatus(
              sessionId,
              mission.id,
              todo.id,
              'completed'
            );
          }
        } catch (error) {
          // 更新失败状态
          if (sessionId) {
            this.todoManager.updateMissionTodoStatus(
              sessionId,
              mission.id,
              todo.id,
              'failed'
            );
          }
        }
      }
    }
  }
}
```

---

### 方案 2: 扩展 FileSnapshotMeta 支持 Mission

**目标**: 让快照元数据包含完整的 Mission 上下文

**实现步骤**:

1. **扩展 FileSnapshotMeta 接口**:
```typescript
export interface FileSnapshotMeta {
  id: string;
  filePath: string;
  timestamp: number;

  // 旧架构字段（保留以兼容）
  taskId?: string;
  subTaskId?: string;

  // 新增：Mission 架构字段
  missionId?: string;
  assignmentId?: string;
  todoId?: string;
  workerId?: string;  // Worker 标识（claude/codex/gemini）

  agentType?: AgentType;
  reason?: string;
}
```

2. **更新 SnapshotManager 方法**:
```typescript
export class SnapshotManager {
  // 新增：创建快照（Mission 版本）
  async createSnapshotForMission(
    filePath: string,
    missionId: string,
    assignmentId: string,
    todoId: string,
    workerId: string,
    reason?: string
  ): Promise<string> {
    const session = this.sessionManager.getCurrentSession();
    if (!session) {
      throw new Error('No active session');
    }

    const snapshotId = generateId();
    const meta: FileSnapshotMeta = {
      id: snapshotId,
      filePath,
      timestamp: Date.now(),
      missionId,
      assignmentId,
      todoId,
      workerId,
      reason,
    };

    // ... 创建快照逻辑
    return snapshotId;
  }

  // 新增：按 Mission 清理快照
  clearSnapshotsForMission(missionId: string): number {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return 0;

    let removed = 0;
    for (const snapshot of [...session.snapshots]) {
      if (snapshot.missionId !== missionId) continue;

      const snapshotFile = path.join(
        this.getSnapshotDir(session.id),
        `${snapshot.id}.snapshot`
      );
      if (fs.existsSync(snapshotFile)) {
        try {
          fs.unlinkSync(snapshotFile);
          this.invalidateSnapshotCache(snapshotFile);
        } catch (error) {
          logger.error('快照.清理.失败', { snapshotId: snapshot.id, error }, LogCategory.RECOVERY);
        }
      }
      this.sessionManager.removeSnapshot(session.id, snapshot.filePath);
      removed++;
    }

    return removed;
  }

  // 新增：按 Assignment 清理快照
  clearSnapshotsForAssignment(assignmentId: string): number {
    const session = this.sessionManager.getCurrentSession();
    if (!session) return 0;

    let removed = 0;
    for (const snapshot of [...session.snapshots]) {
      if (snapshot.assignmentId !== assignmentId) continue;

      const snapshotFile = path.join(
        this.getSnapshotDir(session.id),
        `${snapshot.id}.snapshot`
      );
      if (fs.existsSync(snapshotFile)) {
        try {
          fs.unlinkSync(snapshotFile);
          this.invalidateSnapshotCache(snapshotFile);
        } catch (error) {
          logger.error('快照.清理.失败', { snapshotId: snapshot.id, error }, LogCategory.RECOVERY);
        }
      }
      this.sessionManager.removeSnapshot(session.id, snapshot.filePath);
      removed++;
    }

    return removed;
  }

  // 保留旧方法以兼容（逐步废弃）
  clearSnapshotsForFiles(filePaths: string[], keepSubTaskId?: string): number {
    // ... 保持不变，但添加 deprecated 注释
  }
}
```

3. **更新 AutonomousWorker 调用**:
```typescript
export class AutonomousWorker {
  async execute(assignment: Assignment, todo: Todo, options: WorkerOptions): Promise<void> {
    // 创建快照时使用 Mission 标识符
    await this.snapshotManager.createSnapshotForMission(
      filePath,
      options.missionId,
      assignment.id,
      todo.id,
      this.cliType,
      reason
    );
  }
}
```

---

### 方案 3: 添加 Mission 到快照的双向关联

**目标**: 让 Mission 对象也能追踪它创建的快照

**实现步骤**:

1. **扩展 Mission 接口**:
```typescript
export interface Mission {
  id: string;
  goal: string;
  status: MissionStatus;
  assignments: Assignment[];
  createdAt: number;
  updatedAt: number;

  // 新增：快照追踪
  snapshots?: string[];  // 快照 ID 列表
}
```

2. **在创建快照时更新 Mission**:
```typescript
export class SnapshotManager {
  async createSnapshotForMission(
    filePath: string,
    missionId: string,
    assignmentId: string,
    todoId: string,
    workerId: string,
    reason?: string
  ): Promise<string> {
    // ... 创建快照

    // 更新 Mission 的快照列表
    const mission = await this.missionStorage.getMission(missionId);
    if (mission) {
      if (!mission.snapshots) {
        mission.snapshots = [];
      }
      mission.snapshots.push(snapshotId);
      await this.missionStorage.updateMission(mission);
    }

    return snapshotId;
  }
}
```

---

## 📊 修复优先级

| 问题 | 优先级 | 影响范围 | 修复难度 |
|------|--------|---------|---------|
| PlanTodoManager 不支持 Mission | 🔴 高 | 用户体验 | 中 |
| FileSnapshotMeta 缺少 Mission 字段 | 🔴 高 | 数据完整性 | 低 |
| SnapshotManager 使用旧标识符 | 🟡 中 | 功能正确性 | 中 |
| MissionExecutor 没有 TODO 集成 | 🔴 高 | 用户体验 | 中 |
| AutonomousWorker 使用旧标识符 | 🟡 中 | 代码一致性 | 低 |

---

## 🎯 实施计划

### Phase 1: 扩展数据结构（不破坏兼容性）

1. ✅ 扩展 FileSnapshotMeta 接口（添加 Mission 字段，保留旧字段）
2. ✅ 扩展 Mission 接口（添加 snapshots 字段）
3. ✅ 添加新的 SnapshotManager 方法（保留旧方法）

### Phase 2: 集成 TODO 系统

1. ✅ 添加 PlanTodoManager 的 Mission 支持方法
2. ✅ 在 MissionExecutor 中集成 TODO 生成和更新
3. ✅ 测试 TODO 文件生成和状态更新

### Phase 3: 更新调用点

1. ✅ 更新 AutonomousWorker 使用新的快照方法
2. ✅ 更新 MissionExecutor 使用新的快照方法
3. ✅ 添加 Mission 到快照的双向关联

### Phase 4: 测试和验证

1. ✅ 单元测试：PlanTodoManager Mission 方法
2. ✅ 单元测试：SnapshotManager Mission 方法
3. ✅ 集成测试：完整的 Mission 执行流程
4. ✅ 端到端测试：在 VSCode 中验证

### Phase 5: 清理和文档

1. ✅ 标记旧方法为 @deprecated
2. ✅ 更新文档说明新的架构
3. ✅ 创建迁移指南

---

## ✅ 验证清单

### 功能验证

- [ ] Mission 执行时生成 TODO 文件
- [ ] TODO 文件包含所有 Assignment 和 Todo
- [ ] Todo 状态更新正确反映在文件中
- [ ] 快照包含完整的 Mission 上下文
- [ ] 快照可以按 Mission 查询和清理
- [ ] 快照可以按 Assignment 查询和清理
- [ ] Mission 对象追踪它创建的快照

### 兼容性验证

- [ ] 旧的 PlanRecord 仍然可以生成 TODO
- [ ] 旧的快照方法仍然可用
- [ ] 旧的快照元数据仍然可以读取
- [ ] 不破坏现有的会话数据

### 性能验证

- [ ] TODO 文件生成不影响执行速度
- [ ] 快照创建不影响执行速度
- [ ] 快照查询性能可接受
- [ ] 内存使用在合理范围内

---

**文档版本**: 1.0
**创建日期**: 2025-01-22
**作者**: AI Assistant
**状态**: 📋 分析完成，待实施
