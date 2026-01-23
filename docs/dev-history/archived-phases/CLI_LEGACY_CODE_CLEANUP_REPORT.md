# CLI 遗留代码清理报告

## 📅 检查日期
**日期**: 2025-01-22
**目的**: 检查代码中是否还存在 CLI 相关遗留内容

---

## 🔍 检查结果总结

### ✅ 已完成迁移的部分

1. **核心类型系统** ✅
   - `CLIType` → `AgentType` (已完成)
   - `WorkerSlot` 统一使用
   - `SessionMessage.agent` 使用 `AgentType`

2. **适配器层** ✅
   - CLI 适配器文件已删除
   - 只保留 `adapter-factory-interface.ts`
   - LLM 适配器完全替代

3. **会话管理** ✅
   - `SessionMessage` 使用 `agent: AgentType`
   - `addMessage()` 方法使用 `agent` 参数
   - 快照元数据使用 `workerId`

---

## ⚠️ 发现的 CLI 遗留内容

### 1. 变量命名中的 "cli" 字样

#### 1.1 WebviewProvider 中的参数命名
**位置**: `src/ui/webview-provider.ts`

**问题**:
```typescript
// Line 4365
private saveMessageToSession(
  userPrompt: string,
  assistantResponse: string,
  cli?: WorkerSlot,  // ❌ 应改为 agent 或 worker
  source?: MessageSource
): void

// Line 4373
this.sessionManager.addMessage('assistant', assistantResponse, cli, source);
```

**建议修改**:
```typescript
private saveMessageToSession(
  userPrompt: string,
  assistantResponse: string,
  agent?: WorkerSlot,  // ✅ 改为 agent
  source?: MessageSource
): void {
  // ...
  this.sessionManager.addMessage('assistant', assistantResponse, agent, source);
}
```

**影响**: 低（仅命名问题，不影响功能）

---

#### 1.2 事件数据中的 cli 字段
**位置**: `src/ui/webview-provider.ts`

**问题**:
```typescript
// Line 563
const data = event.data as { cli?: string; description?: string; ... };
cli: data.cli || 'system',

// Line 588
const data = event.data as { success?: boolean; cli?: string; cliType?: string; ... };
cli: data?.cli || data?.cliType,

// Line 615
const data = event.data as { error?: string | object; cli?: string; cliType?: string; ... };
cli: data?.cli || data?.cliType,
```

**建议修改**:
```typescript
// 统一使用 agent 或 worker
const data = event.data as { agent?: string; description?: string; ... };
agent: data.agent || 'system',

const data = event.data as { success?: boolean; agent?: string; ... };
agent: data?.agent,

const data = event.data as { error?: string | object; agent?: string; ... };
agent: data?.agent,
```

**影响**: 中（需要同步修改前端代码）

---

#### 1.3 日志和调试信息中的 cli 字段
**位置**: 多处

**问题**:
```typescript
// src/ui/webview-provider.ts:4222
logger.info('界面.执行.模式.直接', { cli: targetCli }, LogCategory.UI);

// src/orchestrator/recovery-handler.ts:160
logger.info('编排器.恢复.重试.原始_CLI', { cli: failedTask.assignedWorker }, LogCategory.ORCHESTRATOR);

// src/orchestrator/recovery-handler.ts:215
logger.info('编排器.恢复.提供_上下文', { cli: failedTask.assignedWorker }, LogCategory.ORCHESTRATOR);
```

**建议修改**:
```typescript
logger.info('界面.执行.模式.直接', { agent: targetAgent }, LogCategory.UI);
logger.info('编排器.恢复.重试.原始_Worker', { agent: failedTask.assignedWorker }, LogCategory.ORCHESTRATOR);
logger.info('编排器.恢复.提供_上下文', { agent: failedTask.assignedWorker }, LogCategory.ORCHESTRATOR);
```

**影响**: 低（仅日志字段名）

---

### 2. 日志系统中的 cli 字段

#### 2.1 UnifiedLogger 接口
**位置**: `src/logging/unified-logger.ts`

**问题**:
```typescript
// Line 63
cli: string;

// Line 99-135
cli: {
  type: 'string',
  description: 'CLI 类型 (claude/codex/gemini)',
  example: 'claude'
}
```

**建议修改**:
```typescript
agent: string;

agent: {
  type: 'string',
  description: 'Agent 类型 (claude/codex/gemini)',
  example: 'claude'
}
```

**影响**: 中（需要更新所有日志调用）

---

#### 2.2 日志结构中的 cli 字段
**位置**: `src/logging/unified-logger.ts`

**问题**:
```typescript
// Line 408, 451
cli: string;

// Line 685
cli: log.cli,
```

**建议修改**:
```typescript
agent: string;
agent: log.agent,
```

**影响**: 中（需要更新日志格式）

---

### 3. 任务结果聚合器中的 cli 字段

**位置**: `src/task/result-aggregator.ts`

**问题**:
```typescript
// Line 52
cli: AgentType;  // ✅ 使用 AgentType

// Line 137
cli: d.source,
```

**建议修改**:
```typescript
agent: AgentType;
agent: d.source,
```

**影响**: 低（已使用正确类型，仅字段名问题）

---

### 4. 测试代码中的 cli 字段

**位置**: `src/test/` 目录

**问题**:
```typescript
// src/test/test-unified-logger.ts:24
cli: 'claude',

// src/test/e2e/orchestrator-e2e.ts:67
cli: string;  // 目标 CLI

// src/test/e2e/orchestrator-e2e.ts:149
private questionCallback?: (cli: string, question: string) => Promise<string>;

// src/test/e2e/orchestrator-e2e.ts:152
registerResponse(cli: string, trigger: string, response: string, options?: Partial<MockResponse>): void

// src/test/e2e/orchestrator-e2e.ts:165
onQuestion(callback: (cli: string, question: string) => Promise<string>): void

// src/test/e2e/orchestrator-e2e.ts:170
async sendMessage(cli: string, prompt: string): Promise<{ content: string; error?: string }>
```

**建议修改**:
```typescript
agent: 'claude',
agent: string;
private questionCallback?: (agent: string, question: string) => Promise<string>;
registerResponse(agent: string, trigger: string, response: string, ...)
onQuestion(callback: (agent: string, question: string) => Promise<string>)
async sendMessage(agent: string, prompt: string)
```

**影响**: 低（测试代码）

---

### 5. 注释和文档中的 CLI 字样

**位置**: 多处

**问题**:
```typescript
// src/ui/webview-provider.ts:4
 * 负责：对话面板、任务视图、变更视图、CLI 输出

// src/ui/webview-provider.ts:469
  /** 设置所有 CLI 适配器事件监听 */

// src/ui/webview-provider.ts:1234
  /** 检测所有 CLI 的可用性并更新状态 */

// src/types/agent-types.ts:4
 * 用于替代原有的 CLIType，支持 LLM 模式
```

**建议修改**:
```typescript
 * 负责：对话面板、任务视图、变更视图、Agent 输出
  /** 设置所有 Agent 适配器事件监听 */
  /** 检测所有 Agent 的可用性并更新状态 */
 * 用于替代原有的 CLIType，现已完全迁移到 LLM 模式
```

**影响**: 低（仅文档）

---

### 6. DI 容器中的符号

**位置**: `src/di/types.ts`

**问题**:
```typescript
// Line 36
CLIAdapterFactory: Symbol.for('CLIAdapterFactory'),
```

**建议修改**:
```typescript
LLMAdapterFactory: Symbol.for('LLMAdapterFactory'),
// 或者直接删除（如果未使用）
```

**影响**: 低（如果未使用可直接删除）

---

### 7. 消息追踪器中的类型

**位置**: `src/tracing/message-tracer.ts`

**问题**:
```typescript
// Line 16
  | 'cli-adapter'
  | 'cli-process';
```

**建议修改**:
```typescript
  | 'llm-adapter'
  | 'llm-client';
```

**影响**: 低（追踪类型）

---

## 📊 清理优先级

### 🔴 高优先级（影响功能或理解）

1. **WebviewProvider 事件数据中的 cli 字段**
   - 需要同步修改前端代码
   - 影响前后端通信

2. **日志系统中的 cli 字段**
   - 影响日志查询和分析
   - 需要更新所有日志调用

### 🟡 中优先级（影响代码一致性）

1. **参数命名中的 cli**
   - `saveMessageToSession(cli?: WorkerSlot)` → `agent?: WorkerSlot`
   - 影响代码可读性

2. **任务结果聚合器中的 cli 字段**
   - 已使用正确类型，仅字段名问题

### 🟢 低优先级（不影响功能）

1. **注释和文档中的 CLI 字样**
   - 仅影响文档准确性

2. **测试代码中的 cli 字段**
   - 测试代码，影响较小

3. **DI 容器中的旧符号**
   - 如果未使用可直接删除

---

## 🎯 清理建议

### 阶段 1: 核心接口和类型（1-2 天）

1. **统一字段命名**
   ```typescript
   // 全局替换规则
   cli?: WorkerSlot → agent?: WorkerSlot
   cli: AgentType → agent: AgentType
   cli: string → agent: string
   ```

2. **更新日志系统**
   - 修改 `UnifiedLogger` 接口
   - 更新所有日志调用
   - 更新日志查询工具

3. **更新事件数据结构**
   - 修改 WebviewProvider 事件处理
   - 同步更新前端代码

### 阶段 2: 文档和注释（半天）

1. **更新注释**
   - CLI → Agent
   - CLI 输出 → Agent 输出
   - CLI 适配器 → LLM 适配器

2. **更新文档**
   - 更新架构文档
   - 更新 API 文档

### 阶段 3: 测试和验证（半天）

1. **更新测试代码**
   - 修改测试中的 cli 字段
   - 更新 Mock 对象

2. **清理未使用的符号**
   - 删除 DI 容器中的 `CLIAdapterFactory`
   - 删除消息追踪器中的 `cli-adapter`、`cli-process`

---

## 📝 清理脚本

### 自动化替换脚本

```bash
#!/bin/bash

# 1. 替换参数名
find src -name "*.ts" -exec sed -i '' 's/cli?: WorkerSlot/agent?: WorkerSlot/g' {} \;
find src -name "*.ts" -exec sed -i '' 's/cli: AgentType/agent: AgentType/g' {} \;

# 2. 替换日志字段
find src -name "*.ts" -exec sed -i '' 's/{ cli:/{ agent:/g' {} \;
find src -name "*.ts" -exec sed -i '' 's/, cli:/, agent:/g' {} \;

# 3. 替换注释
find src -name "*.ts" -exec sed -i '' 's/CLI 输出/Agent 输出/g' {} \;
find src -name "*.ts" -exec sed -i '' 's/CLI 适配器/LLM 适配器/g' {} \;
find src -name "*.ts" -exec sed -i '' 's/CLI 类型/Agent 类型/g' {} \;

# 4. 验证
echo "清理完成，请运行测试验证："
echo "npm run compile"
echo "npm run test"
```

---

## ✅ 验证清单

清理完成后，验证以下内容：

- [ ] TypeScript 编译通过
- [ ] 所有测试通过
- [ ] 前端界面正常工作
- [ ] 日志输出正确
- [ ] 事件通信正常
- [ ] 文档更新完成

---

## 📊 统计数据

| 类别 | 数量 | 优先级 |
|------|------|--------|
| 参数命名 | ~10 处 | 🟡 中 |
| 事件数据字段 | ~5 处 | 🔴 高 |
| 日志字段 | ~15 处 | 🔴 高 |
| 注释文档 | ~10 处 | 🟢 低 |
| 测试代码 | ~8 处 | 🟢 低 |
| DI 符号 | 2 处 | 🟢 低 |
| **总计** | **~50 处** | - |

---

## 🎯 总结

### 当前状态
- ✅ **核心架构已完成迁移**（CLIType → AgentType）
- ✅ **适配器层已完全替换**（CLI → LLM）
- ⚠️ **变量命名存在遗留**（cli 字段名）
- ⚠️ **注释文档需要更新**

### 影响评估
- **功能影响**: 无（仅命名问题）
- **代码一致性**: 中等影响
- **文档准确性**: 中等影响

### 建议
**优先清理高优先级项目**（事件数据和日志系统），然后逐步清理其他部分。

**预计工作量**: 2-3 天

---

**检查人**: AI Assistant
**检查日期**: 2025-01-22
**文档版本**: 1.0
