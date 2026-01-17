# ProfileLoader 架构设计与使用规范

## 设计原则

**单一实例原则**：ProfileLoader 在整个系统中应该只有一个实例。

## 架构设计

### 所有权和生命周期

```
系统启动
  ↓
OrchestratorAgent 构造
  ↓
WorkerPool 构造
  ↓
OrchestratorAgent.initialize()
  ↓
WorkerPool.initialize()
  ├─ WorkerPool.loadProfiles()
  │  └─ 创建 ProfileLoader 实例 ← 唯一创建点
  │     └─ 加载配置文件
  ├─ 创建 Worker (claude, codex, gemini)
  │  └─ 注入画像配置
  └─ 返回
  ↓
OrchestratorAgent.initialize() 继续
  ├─ 从 WorkerPool 获取 ProfileLoader
  ├─ 注入到 CLISelector
  ├─ 注入到 TaskAnalyzer
  └─ 创建 PolicyEngine 并注入
```

### 依赖关系图

```
ProfileLoader (唯一实例)
  ↑
  │ 拥有
  │
WorkerPool
  ↑
  │ 获取
  │
OrchestratorAgent
  │
  ├─→ CLISelector (注入)
  ├─→ TaskAnalyzer (注入)
  ├─→ PolicyEngine (注入)
  └─→ Worker × 3 (通过 WorkerPool)
```

## 使用规范

### ✅ 正确的使用方式

**1. 创建实例（仅在 WorkerPool 中）**

```typescript
// src/orchestrator/worker-pool.ts
class WorkerPool {
  private async loadProfiles(): Promise<void> {
    // ✅ 唯一创建点
    this.profileLoader = new ProfileLoader(this.workspacePath);
    await this.profileLoader.load();
  }
}
```

**2. 获取实例（其他组件）**

```typescript
// src/orchestrator/orchestrator-agent.ts
class OrchestratorAgent {
  async initialize(): Promise<void> {
    await this.workerPool.initialize();

    // ✅ 从 WorkerPool 获取
    const profileLoader = this.workerPool.getProfileLoader();

    // ✅ 注入到需要的组件
    this.cliSelector.setProfileLoader(profileLoader);
    this.taskAnalyzer.setProfileLoader(profileLoader);
    this.policyEngine = new PolicyEngine(profileLoader);
  }
}
```

**3. 使用实例（组件内部）**

```typescript
// src/task/cli-selector.ts
class CLISelector {
  private profileLoader?: ProfileLoader;

  // ✅ 通过 setter 接受注入
  setProfileLoader(loader: ProfileLoader): void {
    this.profileLoader = loader;
  }

  // ✅ 使用注入的实例
  select(analysis: TaskAnalysis): CLISelection {
    if (this.profileLoader) {
      const categoryConfig = this.profileLoader.getCategory(category);
      // ...
    }
  }
}
```

### ❌ 错误的使用方式

**1. 在多个地方创建实例**

```typescript
// ❌ 错误：在 OrchestratorAgent 中创建
class OrchestratorAgent {
  constructor() {
    this.profileLoader = new ProfileLoader(this.workspaceRoot); // ❌
  }
}

// ❌ 错误：在 WorkerPool 中也创建
class WorkerPool {
  async loadProfiles() {
    this.profileLoader = new ProfileLoader(this.workspacePath); // ❌
  }
}
```

**问题**：两个独立的实例，配置可能不一致。

**2. 不等待加载完成**

```typescript
// ❌ 错误：异步加载但不等待
constructor() {
  this.profileLoader = new ProfileLoader(this.workspaceRoot);
  this.profileLoader.load().catch(err => console.warn(err)); // ❌ 不等待

  // ❌ 可能在加载完成前使用
  this.policyEngine = new PolicyEngine(this.profileLoader);
}
```

**问题**：可能在配置加载完成前使用，导致使用默认配置。

**3. 直接创建而不是注入**

```typescript
// ❌ 错误：组件自己创建实例
class CLISelector {
  constructor() {
    this.profileLoader = new ProfileLoader(); // ❌
  }
}
```

**问题**：违反依赖注入原则，难以测试和维护。

## 保护机制

### 1. 实例跟踪和警告

ProfileLoader 内置了实例跟踪机制：

```typescript
// src/orchestrator/profile/profile-loader.ts
export class ProfileLoader {
  private static instanceCount = 0;
  private static instances: WeakRef<ProfileLoader>[] = [];

  constructor() {
    ProfileLoader.instanceCount++;
    this.instanceId = ProfileLoader.instanceCount;

    // 检测多实例
    if (ProfileLoader.instanceCount > 1) {
      console.warn(
        `[ProfileLoader] ⚠️  检测到多个 ProfileLoader 实例！` +
        `\n  当前实例 ID: ${this.instanceId}` +
        `\n  活跃实例数: ${activeInstances}` +
        `\n  建议：只在 WorkerPool 中创建一个实例`
      );
      console.warn('[ProfileLoader] 创建位置堆栈:');
      console.warn(new Error().stack);
    }
  }
}
```

**效果**：
- 第一个实例创建时：正常，无警告
- 第二个实例创建时：立即警告，并打印堆栈跟踪
- 帮助开发者快速定位问题

### 2. 代码文档

关键位置添加了详细的文档注释：

- [src/orchestrator/profile/profile-loader.ts](src/orchestrator/profile/profile-loader.ts): 文件头部说明使用规范
- [src/orchestrator/worker-pool.ts:191](src/orchestrator/worker-pool.ts#L191): loadProfiles() 说明这是唯一创建点
- [src/orchestrator/worker-pool.ts:211](src/orchestrator/worker-pool.ts#L211): getProfileLoader() 说明这是推荐的获取方式

### 3. 自动化测试

创建了专门的测试验证单例保护机制：

```bash
node scripts/test-profileloader-singleton.js
```

**测试覆盖**：
- 第一个实例不应该有警告
- 第二个实例应该触发警告
- 警告信息包含实例 ID、活跃实例数、建议信息、堆栈跟踪
- 实例计数器正确递增

## 为什么选择这个设计

### 方案对比

| 方案 | 优点 | 缺点 | 选择 |
|------|------|------|------|
| **单例模式** | 强制唯一性 | 全局状态，难以测试 | ❌ |
| **依赖注入（当前）** | 易于测试，清晰的依赖关系 | 需要手动管理 | ✅ |
| **构造函数注入** | 依赖明确 | 构造函数不能 async | ❌ |

### 选择依赖注入的原因

1. **易于测试**：可以在测试中注入 mock 对象
2. **清晰的依赖关系**：通过 `setProfileLoader()` 明确依赖
3. **灵活性**：可以在运行时更换实例（如果需要）
4. **符合 SOLID 原则**：依赖倒置原则

### WorkerPool 作为所有者的原因

1. **主要消费者**：WorkerPool 需要为每个 Worker 配置画像
2. **生命周期管理**：WorkerPool 在 initialize() 中同步加载，保证一致性
3. **职责清晰**：WorkerPool 负责 Worker 管理，画像是 Worker 的配置

## 常见问题

### Q1: 为什么不使用单例模式？

**A**: 单例模式引入全局状态，难以测试。依赖注入更灵活，更符合现代软件工程实践。

### Q2: 如果我需要在测试中使用不同的配置怎么办？

**A**: 创建一个测试用的 ProfileLoader 实例，通过 `setProfileLoader()` 注入到被测试的组件中。

```typescript
// 测试代码
const testLoader = new ProfileLoader();
await testLoader.load();
cliSelector.setProfileLoader(testLoader);
```

### Q3: 警告机制会影响性能吗？

**A**: 不会。警告只在创建实例时检查一次，对运行时性能没有影响。

### Q4: 如果我真的需要多个实例怎么办？

**A**:
1. 首先确认是否真的需要多个实例
2. 如果确实需要（如测试场景），可以忽略警告
3. 考虑是否可以通过其他方式（如配置参数）实现需求

## 检查清单

在添加新组件或修改现有代码时，请检查：

- [ ] 是否需要使用 ProfileLoader？
- [ ] 如果需要，是否通过 `WorkerPool.getProfileLoader()` 获取？
- [ ] 是否通过 `setProfileLoader()` 接受注入？
- [ ] 是否在 `initialize()` 完成后才使用？
- [ ] 是否避免了创建新的 ProfileLoader 实例？

## 总结

**核心原则**：
1. **唯一创建点**：只在 WorkerPool.loadProfiles() 中创建
2. **依赖注入**：其他组件通过 getProfileLoader() 获取
3. **同步加载**：使用 await 确保加载完成
4. **保护机制**：实例跟踪 + 警告 + 测试

**如果遇到多实例警告**：
1. 检查堆栈跟踪，找到创建位置
2. 移除重复的创建代码
3. 改为从 WorkerPool 获取实例
4. 运行测试验证修复
