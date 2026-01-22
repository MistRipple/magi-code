# Phase 6.2 完成总结

**完成时间**: 2024年
**状态**: ✅ 完成并验证

---

## 目标

清理 `webview-provider.ts` 中的 CLI 状态系统，使用 LLM 适配器状态替代。

---

## 完成的工作

### 1. 类型系统更新

#### 文件: `src/types.ts`

**新增 WorkerStatus 接口**:
```typescript
/** Worker 状态（基于 LLM 适配器） */
export interface WorkerStatus {
  worker: WorkerSlot;
  available: boolean;
  enabled: boolean;
  model?: string;      // 配置的模型名称
  provider?: string;   // openai 或 anthropic
}
```

**更新 UIState 接口**:
```typescript
export interface UIState {
  currentSessionId?: string;
  sessions?: UIChatSession[];
  currentTask?: Task;
  tasks?: Task[];
  activePlan?: { planId: string; formattedPlan: string; updatedAt: number; review?: { status: 'approved' | 'rejected' | 'skipped'; summary: string } };
  workerStatuses: WorkerStatus[];  // ✅ 使用 WorkerStatus 替代 cliStatuses
  // ❌ 删除 cliStatuses: CLIStatus[]
  // ❌ 删除 degradationStrategy: DegradationStrategy
  pendingChanges: PendingChange[];
  isRunning: boolean;
  logs: LogEntry[];
  interactionMode: InteractionMode;
  orchestratorPhase?: string;
}
```

**更新 WebviewToExtensionMessage**:
```typescript
// 旧代码
| { type: 'saveCurrentSession'; messages: any[]; cliOutputs: Record<string, any[]> }

// 新代码
| { type: 'saveCurrentSession'; messages: any[] }  // ✅ 移除 cliOutputs 参数
```

**更新 ExtensionToWebviewMessage**:
```typescript
// 旧代码
| { type: 'cliStatusChanged'; cli: string; available: boolean; version?: string }

// 新代码
| { type: 'workerStatusChanged'; worker: WorkerSlot; available: boolean; version?: string }
```

### 2. webview-provider.ts 更新

#### 导入清理

**删除的导入**:
```typescript
// ❌ 删除
CLIStatus,
CLIStatusCode,
import { CLI_CAPABILITIES, CLIResponse } from '../cli/types';
```

**新增的导入**:
```typescript
// ✅ 新增
WorkerStatus,
WorkerSlot,
```

#### 状态变量清理

**删除的字段**:
```typescript
// ❌ 删除
private cliStatuses: Map<CLIType, CLIStatus> = new Map();
private cliOutputs: Map<CLIType, string[]> = new Map();
```

**保留的字段**:
```typescript
// ✅ 保留（用于 Worker 选择）
private selectedCli: CLIType | null = null;
```

#### 初始化代码清理

**删除的代码**:
```typescript
// ❌ 删除
this.cliOutputs.set('claude', []);
this.cliOutputs.set('codex', []);
this.cliOutputs.set('gemini', []);
```

#### CLI 可用性检查重构

**旧代码**:
```typescript
// 更新 CLI 状态
const cliTypes: CLIType[] = ['claude', 'codex', 'gemini'];
for (const cli of cliTypes) {
  const status: CLIStatus = {
    type: cli,
    code: availability[cli] ? CLIStatusCode.AVAILABLE : CLIStatusCode.NOT_INSTALLED,
    available: availability[cli],
    path: cli,
    lastChecked: new Date(),
  };
  this.cliStatuses.set(cli, status);
}

// 通知 UI 更新状态
this.sendStateUpdate();

// 发送单独的状态变更通知
for (const cli of cliTypes) {
  this.postMessage({
    type: 'cliStatusChanged',
    cli,
    available: availability[cli],
  });
}
```

**新代码**:
```typescript
// 通知 UI 更新状态
this.sendStateUpdate();

// 发送单独的状态变更通知
const workerSlots: WorkerSlot[] = ['claude', 'codex', 'gemini'];
for (const worker of workerSlots) {
  this.postMessage({
    type: 'workerStatusChanged',
    worker,
    available: availability[worker],
  });
}
```

#### 会话保存方法更新

**方法签名更新**:
```typescript
// 旧代码
private saveCurrentSessionData(messages: any[], cliOutputs: Record<string, any[]>): void

// 新代码
private saveCurrentSessionData(messages: any[]): void
```

**调用处更新**:
```typescript
// 旧代码
this.saveCurrentSessionData(message.messages, message.cliOutputs);

// 新代码
this.saveCurrentSessionData(message.messages);
```

#### UI 状态构建重构

**旧代码**:
```typescript
// 构建 CLI 状态（包含能力信息）
const cliStatuses: CLIStatus[] = Array.from(this.cliStatuses.values()).map(status => ({
  ...status,
  capabilities: CLI_CAPABILITIES[status.type],
}));

return {
  // ...
  cliStatuses,
  degradationStrategy: {
    level: 3,
    availableCLIs: ['claude', 'codex', 'gemini'],
    missingCLIs: [],
    hasOrchestrator: true,
    recommendation: '',
    canProceed: true,
    fallbackMap: {},
  },
  // ...
};
```

**新代码**:
```typescript
// 构建 Worker 状态（基于 LLM 适配器）
const workerSlots: WorkerSlot[] = ['claude', 'codex', 'gemini'];
const workerStatuses: WorkerStatus[] = workerSlots.map(worker => ({
  worker,
  available: this.adapterFactory.isConnected(worker),
  enabled: true,  // TODO: 从配置读取
}));

return {
  // ...
  workerStatuses,  // ✅ 新字段
  // ❌ 删除 cliStatuses
  // ❌ 删除 degradationStrategy
  // ...
};
```

#### 事件处理更新

**旧代码**:
```typescript
globalEventBus.on('cli:statusChanged', (event) => {
  const data = event.data as { cli: string; available: boolean; version?: string };
  this.sendStateUpdate();
  this.postMessage({ type: 'cliStatusChanged', cli: data.cli, available: data.available, version: data.version });
});
```

**新代码**:
```typescript
globalEventBus.on('cli:statusChanged', (event) => {
  const data = event.data as { cli: string; available: boolean; version?: string };
  this.sendStateUpdate();
  this.postMessage({ type: 'workerStatusChanged', worker: data.cli as WorkerSlot, available: data.available, version: data.version });
});
```

#### 其他清理

**删除的代码**:
```typescript
// ❌ 删除
this.cliOutputs.set(targetCli, []);
```

---

## 架构改进

### 1. 简化状态管理

**之前**:
- 维护 `cliStatuses` Map 存储 CLI 状态
- 维护 `cliOutputs` Map 存储 CLI 输出
- 需要手动更新和同步状态

**现在**:
- 直接从 `adapterFactory.isConnected()` 获取状态
- 无需维护额外的状态 Map
- 状态始终是最新的

### 2. 移除降级策略

**原因**:
- LLM 模式不需要降级策略
- 所有 Worker 都是通过 LLM API 调用
- 不存在 CLI 工具未安装的问题

### 3. 统一消息类型

**之前**:
- `cliStatusChanged` 消息类型
- 使用 `cli` 字段

**现在**:
- `workerStatusChanged` 消息类型
- 使用 `worker` 字段（类型为 `WorkerSlot`）

---

## 编译状态

✅ **0 错误** - 所有代码编译通过

---

## 影响范围

### 修改的文件 (2个)

1. **src/types.ts**
   - 新增 `WorkerStatus` 接口
   - 更新 `UIState` 接口
   - 更新 `WebviewToExtensionMessage` 类型
   - 更新 `ExtensionToWebviewMessage` 类型

2. **src/ui/webview-provider.ts**
   - 删除 CLI 相关导入
   - 删除 `cliStatuses` 和 `cliOutputs` 字段
   - 删除 cliOutputs 初始化代码
   - 重构 CLI 可用性检查方法
   - 更新 `saveCurrentSessionData` 方法
   - 重构 `buildUIState` 方法
   - 更新事件处理

### 删除的依赖

- `CLI_CAPABILITIES` from `../cli/types`
- `CLIStatus` interface
- `CLIStatusCode` enum
- `DegradationStrategy` interface

---

## 待办事项

### 前端适配 (如果需要)

前端代码可能需要更新以适配新的状态结构：

1. **更新状态访问**:
   ```typescript
   // 旧代码
   state.cliStatuses.forEach(status => { ... })

   // 新代码
   state.workerStatuses.forEach(status => { ... })
   ```

2. **更新消息处理**:
   ```typescript
   // 旧代码
   case 'cliStatusChanged':
     // 处理 cli 字段

   // 新代码
   case 'workerStatusChanged':
     // 处理 worker 字段
   ```

3. **移除降级策略显示**:
   ```typescript
   // ❌ 删除
   state.degradationStrategy
   ```

---

## 下一步

### Phase 6.3: 删除 CLI 代码

现在可以安全地删除所有 CLI 相关代码：

1. **删除目录**:
   - `src/cli/` (约 7 个文件)

2. **删除测试**:
   - `src/test/message-flow-e2e.test.ts` (旧的 CLI 测试)

3. **清理导入**:
   - 搜索并删除所有 `from '../cli/` 或 `from './cli/` 的导入

4. **验证编译**:
   - 确保删除后编译通过
   - 确保没有残留的 CLI 引用

---

## 验收标准

- [x] 所有 CLI 状态相关代码已删除
- [x] 使用 LLM 适配器状态替代
- [x] 编译通过，0 错误
- [x] UIState 接口已更新
- [x] 消息类型已更新
- [x] 不再依赖 `CLI_CAPABILITIES`
- [x] 不再维护 `cliStatuses` 和 `cliOutputs`
- [x] 事件处理已更新

---

**最后更新**: 2024年
**编译状态**: ✅ 0 错误
**系统可用性**: ✅ 核心功能可用
