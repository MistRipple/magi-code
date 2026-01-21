# Phase 6.2: 清理 UI CLI 引用

**目标**: 清理 `webview-provider.ts` 中的 CLI 状态系统，使用 LLM 适配器状态替代

**状态**: 🔄 进行中

---

## 需要清理的内容

### 1. 导入清理
**文件**: `src/ui/webview-provider.ts`

**删除的导入**:
```typescript
// Line 12-15
CLIType,        // ✅ 保留（作为 WorkerSlot 的别名）
CLIStatus,      // ❌ 删除
CLIStatusCode,  // ❌ 删除

// Line 39
import { CLI_CAPABILITIES, CLIResponse } from '../cli/types';  // ❌ 删除
```

### 2. 状态变量清理
**文件**: `src/ui/webview-provider.ts`

**删除的字段** (Lines 71-72):
```typescript
private cliStatuses: Map<CLIType, CLIStatus> = new Map();  // ❌ 删除
private cliOutputs: Map<CLIType, string[]> = new Map();    // ❌ 删除
```

**保留的字段**:
```typescript
private selectedCli: CLIType | null = null;  // ✅ 保留（用于 Worker 选择）
```

### 3. 初始化代码清理
**文件**: `src/ui/webview-provider.ts`

**删除** (Lines 191-193):
```typescript
this.cliOutputs.set('claude', []);
this.cliOutputs.set('codex', []);
this.cliOutputs.set('gemini', []);
```

### 4. CLI 可用性检查重构
**文件**: `src/ui/webview-provider.ts`

**当前代码** (Lines 1254-1277):
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
    type: 'workerStatusChanged',  // ✅ 改名
    worker,                        // ✅ 改字段名
    available: availability[worker],
  });
}
```

### 5. 会话保存方法清理
**文件**: `src/ui/webview-provider.ts`

**当前代码** (Line 2731):
```typescript
private saveCurrentSessionData(messages: any[], cliOutputs: Record<string, any[]>): void {
  // ...
}
```

**新代码**:
```typescript
private saveCurrentSessionData(messages: any[]): void {
  // ❌ 移除 cliOutputs 参数
  // ...
}
```

**调用处更新** (Line 1388):
```typescript
// 旧代码
this.saveCurrentSessionData(message.messages, message.cliOutputs);

// 新代码
this.saveCurrentSessionData(message.messages);
```

### 6. UI 状态构建重构
**文件**: `src/ui/webview-provider.ts`

**当前代码** (Lines 2767-2790):
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
const workerStatuses = workerSlots.map(worker => ({
  worker,
  available: this.adapterFactory.isConnected(worker),
  enabled: true,  // 从配置读取
}));

return {
  // ...
  workerStatuses,  // ✅ 新字段
  // ❌ 删除 cliStatuses
  // ❌ 删除 degradationStrategy（LLM 模式不需要降级策略）
  // ...
};
```

### 7. 其他清理
**文件**: `src/ui/webview-provider.ts`

**删除** (Line 2608):
```typescript
this.cliOutputs.set(targetCli, []);  // ❌ 删除
```

---

## 类型系统更新

### 1. UIState 接口更新
**文件**: `src/types.ts`

**当前定义** (Lines 387-402):
```typescript
export interface UIState {
  currentSessionId?: string;
  sessions?: UIChatSession[];
  currentTask?: Task;
  tasks?: Task[];
  activePlan?: { planId: string; formattedPlan: string; updatedAt: number; review?: { status: 'approved' | 'rejected' | 'skipped'; summary: string } };
  cliStatuses: CLIStatus[];           // ❌ 删除
  degradationStrategy: DegradationStrategy;  // ❌ 删除
  pendingChanges: PendingChange[];
  isRunning: boolean;
  logs: LogEntry[];
  interactionMode: InteractionMode;
  orchestratorPhase?: string;
}
```

**新定义**:
```typescript
export interface UIState {
  currentSessionId?: string;
  sessions?: UIChatSession[];
  currentTask?: Task;
  tasks?: Task[];
  activePlan?: { planId: string; formattedPlan: string; updatedAt: number; review?: { status: 'approved' | 'rejected' | 'skipped'; summary: string } };
  workerStatuses: WorkerStatus[];     // ✅ 新增
  pendingChanges: PendingChange[];
  isRunning: boolean;
  logs: LogEntry[];
  interactionMode: InteractionMode;
  orchestratorPhase?: string;
}

// ✅ 新增 WorkerStatus 接口
export interface WorkerStatus {
  worker: WorkerSlot;
  available: boolean;
  enabled: boolean;
  model?: string;      // 配置的模型名称
  provider?: string;   // openai 或 anthropic
}
```

### 2. WebviewToExtensionMessage 更新
**文件**: `src/types.ts`

**当前定义** (Line 432):
```typescript
| { type: 'saveCurrentSession'; messages: any[]; cliOutputs: Record<string, any[]> }
```

**新定义**:
```typescript
| { type: 'saveCurrentSession'; messages: any[] }  // ❌ 移除 cliOutputs
```

### 3. ExtensionToWebviewMessage 更新
**文件**: `src/types.ts`

**需要查找并更新**:
```typescript
// 旧消息类型
| { type: 'cliStatusChanged'; cli: CLIType; available: boolean }

// 新消息类型
| { type: 'workerStatusChanged'; worker: WorkerSlot; available: boolean }
```

---

## 实施步骤

### Step 1: 更新类型定义 ✅
- [ ] 在 `src/types.ts` 中添加 `WorkerStatus` 接口
- [ ] 更新 `UIState` 接口
- [ ] 更新 `WebviewToExtensionMessage` 类型
- [ ] 查找并更新 `ExtensionToWebviewMessage` 类型

### Step 2: 更新 webview-provider.ts ✅
- [ ] 删除 CLI 相关导入
- [ ] 删除 `cliStatuses` 和 `cliOutputs` 字段
- [ ] 删除 cliOutputs 初始化代码
- [ ] 重构 CLI 可用性检查方法
- [ ] 更新 `saveCurrentSessionData` 方法签名
- [ ] 更新 `saveCurrentSessionData` 调用处
- [ ] 重构 `buildUIState` 方法
- [ ] 删除其他 cliOutputs 引用

### Step 3: 编译验证 ✅
- [ ] 运行 `npm run compile`
- [ ] 修复所有编译错误
- [ ] 确保 0 错误

### Step 4: 前端适配（如果需要）⚠️
- [ ] 检查前端是否使用 `cliStatuses`
- [ ] 更新为使用 `workerStatuses`
- [ ] 更新消息类型处理

---

## 验收标准

- [ ] 所有 CLI 状态相关代码已删除
- [ ] 使用 LLM 适配器状态替代
- [ ] 编译通过，0 错误
- [ ] UIState 接口已更新
- [ ] 消息类型已更新
- [ ] 不再依赖 `src/cli/types.ts`

---

## 风险和注意事项

1. **前端兼容性**: 前端可能依赖 `cliStatuses` 字段，需要同步更新
2. **消息类型**: `cliStatusChanged` 消息类型需要改为 `workerStatusChanged`
3. **降级策略**: LLM 模式不需要降级策略，可以直接删除

---

**最后更新**: 2024年
**状态**: 准备开始实施
