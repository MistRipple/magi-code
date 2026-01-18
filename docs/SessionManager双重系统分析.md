# SessionManager 双重系统分析

**分析时间**: 2026-01-18 16:00
**问题**: 为什么存在两个 SessionManager？

---

## 执行摘要

**发现**: 存在两个完全不同职责的 SessionManager：
1. **旧 SessionManager** (`src/cli/session/session-manager.ts`) - CLI 进程会话管理
2. **新 UnifiedSessionManager** (`src/session/unified-session-manager.ts`) - 任务数据持久化管理

**结论**: 这**不是**双重系统问题！两者职责完全不同，应该共存。

---

## 两个 SessionManager 对比

| 特征 | CLI SessionManager | UnifiedSessionManager |
|------|-------------------|----------------------|
| **文件路径** | `src/cli/session/session-manager.ts` | `src/session/unified-session-manager.ts` |
| **职责** | 管理 CLI 进程生命周期 | 管理任务数据持久化 |
| **管理对象** | CLI 进程（claude/codex/gemini） | Session/Task/SubTask 数据 |
| **核心功能** | 启动/停止进程、消息队列、健康监控 | 创建/更新/查询 Session 数据 |
| **持久化** | 不持久化（内存管理） | 持久化到 `.multicli-sessions/` |
| **事件** | output, question, sessionEvent | 无（纯数据管理） |
| **依赖** | PrintSession, InteractiveSession | 文件系统 |
| **使用者** | CLIAdapterFactory, PersistentSessionAdapter | TaskManager, UnifiedTaskManager |

---

## CLI SessionManager 详细分析

### 职责
管理 CLI 进程的生命周期和通信：
- 启动/停止 CLI 进程（claude/codex/gemini）
- 管理消息队列和请求/响应
- 健康监控和自动重启
- 超时管理和中断处理
- 会话模式选择（interactive vs oneshot）

### 核心方法
```typescript
class SessionManager extends EventEmitter {
  // 进程管理
  async startSession(cli: CLIType, role: 'worker' | 'orchestrator'): Promise<void>
  async stopSession(cli: CLIType, role: 'worker' | 'orchestrator'): Promise<void>
  async stopAll(): Promise<void>

  // 消息通信
  async send(cli: CLIType, role: 'worker' | 'orchestrator', message: SessionMessage): Promise<SessionResponse>
  async interrupt(cli: CLIType, role: 'worker' | 'orchestrator', reason?: string): Promise<void>
  writeInput(cli: CLIType, role: 'worker' | 'orchestrator', text: string): boolean

  // 状态查询
  isSessionAlive(cli: CLIType, role: 'worker' | 'orchestrator'): boolean
  isWaitingForAnswer(cli: CLIType, role: 'worker' | 'orchestrator'): boolean
  getSessionMode(role: 'worker' | 'orchestrator'): string
}
```

### 使用场景
```typescript
// CLIAdapterFactory 创建 SessionManager
this.sessionManager = new SessionManager({
  cwd: config.cwd,
  idleTimeoutMs: config.idleTimeout,
  heartbeatMs: 15000,
  commandOverrides: config.cliPaths,
  env: config.env,
  contextManager: this.contextManager,
});

// PersistentSessionAdapter 使用 SessionManager
await this.sessionManager.startSession(this.type, this.role);
const response = await this.sessionManager.send(this.type, this.role, payload);
```

---

## UnifiedSessionManager 详细分析

### 职责
管理任务数据的持久化和查询：
- 创建/更新/查询 Session 数据
- 管理 Task 和 SubTask 数据
- 持久化到文件系统（`.multicli-sessions/`）
- 提供数据访问接口

### 核心方法
```typescript
class UnifiedSessionManager {
  // Session 管理
  createSession(prompt: string, metadata?: Record<string, unknown>): Session
  getCurrentSession(): Session | null
  getSession(sessionId: string): Session | null
  updateSession(sessionId: string, updates: Partial<Session>): void

  // Task 管理
  addTask(sessionId: string, task: Task): void
  updateTask(sessionId: string, taskId: string, updates: Partial<Task>): void
  getTask(sessionId: string, taskId: string): Task | null

  // SubTask 管理
  addSubTask(sessionId: string, taskId: string, subTask: SubTask): void
  updateSubTask(sessionId: string, taskId: string, subTaskId: string, updates: Partial<SubTask>): void

  // 持久化
  private async saveSession(session: Session): Promise<void>
  private async loadSession(sessionId: string): Promise<Session | null>
}
```

### 使用场景
```typescript
// TaskManager 使用 UnifiedSessionManager
constructor(sessionManager: UnifiedSessionManager) {
  this.sessionManager = sessionManager;
}

// UnifiedTaskManager 通过 TaskRepository 使用
const taskRepository = new SessionManagerTaskRepository(sessionManager, sessionId);
this.unifiedTaskManager = new UnifiedTaskManager(sessionId, taskRepository);
```

---

## 架构关系图

```
┌─────────────────────────────────────────────────────────────────┐
│                        应用层                                    │
├─────────────────────────────────────────────────────────────────┤
│                                                                   │
│  OrchestratorAgent                                               │
│         │                                                         │
│         ├─────────────────┬─────────────────┐                   │
│         │                 │                 │                   │
│         ▼                 ▼                 ▼                   │
│  CLIAdapterFactory  UnifiedTaskManager  UnifiedSessionManager   │
│         │                 │                 │                   │
│         │                 │                 │                   │
├─────────┼─────────────────┼─────────────────┼───────────────────┤
│         │                 │                 │                   │
│         ▼                 ▼                 ▼                   │
│  CLI SessionManager  TaskRepository   文件系统持久化            │
│  (进程管理)          (适配器)        (.multicli-sessions/)      │
│         │                                                         │
│         ▼                                                         │
│  CLI 进程                                                         │
│  (claude/codex/gemini)                                           │
│                                                                   │
└─────────────────────────────────────────────────────────────────┘

职责分离：
- CLI SessionManager: 管理 CLI 进程生命周期（横向）
- UnifiedSessionManager: 管理任务数据持久化（纵向）
```

---

## 命名冲突分析

### 问题根源
两个类都叫 `SessionManager`，但职责完全不同：
- CLI SessionManager: 管理 CLI **进程会话**
- UnifiedSessionManager: 管理 **任务数据会话**

### 为什么会混淆？
1. **名称相似**: 都包含 "Session" 和 "Manager"
2. **概念重叠**: "Session" 在两个上下文中含义不同
   - CLI 上下文: 一个 CLI 进程的生命周期
   - 任务上下文: 一次用户交互的所有任务数据

### 实际上不冲突
```typescript
// 不同的导入路径
import { SessionManager } from './cli/session/session-manager';           // CLI 进程管理
import { UnifiedSessionManager } from './session/unified-session-manager'; // 任务数据管理

// 不同的使用场景
const cliSessionManager = new SessionManager({ cwd, ... });     // 管理 CLI 进程
const dataSessionManager = new UnifiedSessionManager(baseDir);  // 管理任务数据
```

---

## 是否需要迁移？

### 答案：**不需要**

理由：
1. ✅ **职责完全不同**: 一个管理进程，一个管理数据
2. ✅ **无功能重叠**: 没有重复的功能
3. ✅ **无双重调用**: 不存在同时调用两者做同一件事
4. ✅ **无状态同步**: 不需要在两者之间同步状态
5. ✅ **架构清晰**: 各司其职，边界明确

### 与 TaskManager 问题的对比

| 特征 | TaskManager 问题 | SessionManager "问题" |
|------|-----------------|---------------------|
| 功能重叠 | 83% | 0% |
| 双重调用 | ✅ 存在 | ❌ 不存在 |
| 状态同步 | ✅ 存在 | ❌ 不存在 |
| 职责冲突 | ✅ 冲突 | ❌ 不冲突 |
| 需要迁移 | ✅ 需要 | ❌ 不需要 |

---

## 可选改进：重命名以避免混淆

如果要改进，唯一的问题是**命名混淆**，可以考虑重命名：

### 方案 1: 重命名 CLI SessionManager
```typescript
// 旧名称
class SessionManager extends EventEmitter { ... }

// 新名称（更明确）
class CLIProcessManager extends EventEmitter { ... }
// 或
class CLISessionManager extends EventEmitter { ... }
```

### 方案 2: 重命名 UnifiedSessionManager
```typescript
// 旧名称
class UnifiedSessionManager { ... }

// 新名称（更明确）
class TaskDataManager { ... }
// 或
class SessionDataStore { ... }
```

### 推荐：方案 1（重命名 CLI SessionManager）
理由：
- UnifiedSessionManager 已经在多处使用（50+ 引用）
- CLI SessionManager 使用范围较小（3 个文件）
- "CLIProcessManager" 更准确描述其职责

---

## 结论

### 核心发现
**这不是双重系统问题**，而是**命名混淆问题**。

两个 SessionManager：
1. **CLI SessionManager**: 管理 CLI 进程生命周期（进程管理器）
2. **UnifiedSessionManager**: 管理任务数据持久化（数据管理器）

### 建议行动

#### 选项 A: 保持现状（推荐）
- ✅ 架构清晰，职责分离
- ✅ 无功能重叠，无需迁移
- ✅ 通过不同的导入路径区分
- ⚠️ 名称可能造成混淆

#### 选项 B: 重命名 CLI SessionManager
- ✅ 消除命名混淆
- ✅ 更准确描述职责
- ⚠️ 需要修改 3 个文件
- ⚠️ 可能影响外部 API

### 最终建议
**保持现状**，因为：
1. 架构本身没有问题
2. 职责清晰，边界明确
3. 重命名收益不大，风险不值得

---

**文档版本**: v1.0
**创建时间**: 2026-01-18 16:00
**状态**: 分析完成 - 无需迁移
