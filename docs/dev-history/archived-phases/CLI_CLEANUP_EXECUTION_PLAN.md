# CLI 遗留代码清理执行计划

## 📅 执行日期
**日期**: 2025-01-22
**目的**: 系统性清理所有 CLI 相关命名，统一使用 Agent/Worker 术语

---

## 🎯 清理范围

### 1. 核心接口和类型（高优先级）

#### 1.1 WebviewProvider 参数命名
**文件**: `src/ui/webview-provider.ts`

**变更**:
- Line 4365: `cli?: WorkerSlot` → `agent?: WorkerSlot`
- Line 4373: `cli` → `agent`
- Line 4397: `cli: m.cli` → `agent: m.agent`

#### 1.2 WebviewProvider 事件数据字段
**文件**: `src/ui/webview-provider.ts`

**变更**:
- Line 553: `{ cli?: string; ... }` → `{ agent?: string; ... }`
- Line 563: `cli: data.cli` → `agent: data.agent`
- Line 570: `if (data.cli)` → `if (data.agent)`
- Line 573: `worker: data.cli` → `worker: data.agent`
- Line 588: `{ cli?: string; cliType?: string; ... }` → `{ agent?: string; ... }`
- Line 591: `cli: data?.cli || data?.cliType` → `agent: data?.agent`
- Line 604: `cli: data?.cli || data?.cliType` → `agent: data?.agent`
- Line 615: `{ cli?: string; cliType?: string; ... }` → `{ agent?: string; ... }`
- Line 629: `cli: data?.cli || data?.cliType` → `agent: data?.agent`
- Line 642: `cli: data?.cli || data?.cliType` → `agent: data?.agent`
- Line 4171: `{ cli?: string; ... }` → `{ agent?: string; ... }`
- Line 4174: `data.cli` → `data.agent`

#### 1.3 日志系统
**文件**: `src/logging/unified-logger.ts`

**变更**:
- Line 31: `CLI = 'cli'` → `AGENT = 'agent'`
- Line 63: `cli: string` → `agent: string`
- Line 99-135: `cli: { ... }` → `agent: { ... }`
- Line 211-213: `cli: { ... }` → `agent: { ... }`
- Line 245: `MULTICLI_LOG_CLI_MESSAGES` → `MULTICLI_LOG_AGENT_MESSAGES`
- Line 248: `MULTICLI_LOG_CLI_RESPONSES` → `MULTICLI_LOG_AGENT_RESPONSES`
- Line 273-278: `this.config.cli` → `this.config.agent`
- Line 408: `cli: string` → `agent: string`
- Line 422: `this.config.cli.logMessages` → `this.config.agent.logMessages`
- Line 428-430: `this.config.cli.maxLength` → `this.config.agent.maxLength`
- Line 445: `'cli-message'` → `'agent-message'`
- Line 451: `cli: string` → `agent: string`
- Line 466: `this.config.cli.logResponses` → `this.config.agent.logResponses`
- Line 472-474: `this.config.cli.maxLength` → `this.config.agent.maxLength`
- Line 489: `'cli-response'` → `'agent-response'`
- Line 547: `CLI: ${log.cli}` → `Agent: ${log.agent}`
- Line 677-678: `this.config.cli.maxLengthFile` → `this.config.agent.maxLengthFile`
- Line 683: `type: 'cli-message'` → `type: 'agent-message'`
- Line 685: `cli: log.cli` → `agent: log.agent`
- Line 697: `this.config.cli.maxLengthFile` → `this.config.agent.maxLengthFile`

#### 1.4 日志调用
**文件**: `src/ui/webview-provider.ts`
- Line 4222: `{ cli: targetCli }` → `{ agent: targetAgent }`

**文件**: `src/orchestrator/recovery-handler.ts`
- Line 160: `{ cli: failedTask.assignedWorker }` → `{ agent: failedTask.assignedWorker }`
- Line 215: `{ cli: failedTask.assignedWorker }` → `{ agent: failedTask.assignedWorker }`

#### 1.5 任务结果聚合器
**文件**: `src/task/result-aggregator.ts`
- Line 52: `cli: AgentType` → `agent: AgentType`
- Line 137: `cli: d.source` → `agent: d.source`

---

### 2. DI 容器和追踪系统（中优先级）

#### 2.1 DI 类型符号
**文件**: `src/di/types.ts`
- Line 35-36: 删除 `CLIAdapterFactory: Symbol.for('CLIAdapterFactory')`
- 添加注释说明已迁移到 LLMAdapterFactory

#### 2.2 消息追踪器
**文件**: `src/tracing/message-tracer.ts`
- Line 16: `'cli-adapter'` → `'llm-adapter'`
- Line 18: `'cli-process'` → `'llm-client'`

---

### 3. 测试代码（低优先级）

#### 3.1 测试文件
**文件**: `src/test/test-unified-logger.ts`
- Line 24: `cli: 'claude'` → `agent: 'claude'`

**文件**: `src/test/e2e/orchestrator-e2e.ts`
- Line 67: `cli: string` → `agent: string`
- Line 149: `(cli: string, question: string)` → `(agent: string, question: string)`
- Line 152: `registerResponse(cli: string, ...)` → `registerResponse(agent: string, ...)`
- Line 165: `onQuestion(callback: (cli: string, ...))` → `onQuestion(callback: (agent: string, ...))`
- Line 170: `async sendMessage(cli: string, ...)` → `async sendMessage(agent: string, ...)`

---

### 4. 注释和文档（低优先级）

**文件**: `src/ui/webview-provider.ts`
- Line 4: `CLI 输出` → `Agent 输出`
- Line 469: `CLI 适配器` → `Agent 适配器`
- Line 1234: `CLI 的可用性` → `Agent 的可用性`

**文件**: `src/types/agent-types.ts`
- Line 4: 更新注释说明迁移已完成

---

## 📝 执行步骤

### Phase 1: 核心接口和类型（当前）
1. ✅ 更新 WebviewProvider 参数和事件数据
2. ✅ 更新日志系统接口和实现
3. ✅ 更新所有日志调用点
4. ✅ 更新任务结果聚合器
5. ✅ 编译验证

### Phase 2: DI 和追踪系统
1. 清理 DI 容器中的旧符号
2. 更新消息追踪器类型
3. 编译验证

### Phase 3: 测试和文档
1. 更新测试代码
2. 更新注释和文档
3. 运行测试验证
4. 最终编译验证

---

## ⚠️ 注意事项

1. **前端同步**: WebviewProvider 的事件数据字段变更需要同步更新前端代码
2. **环境变量**: 日志系统的环境变量名称变更需要更新文档
3. **向后兼容**: 不保留任何兼容性代码，完全清理
4. **测试覆盖**: 每个 Phase 完成后都要编译验证

---

## ✅ 验证清单

- [ ] TypeScript 编译通过
- [ ] 所有测试通过
- [ ] 前端界面正常工作
- [ ] 日志输出正确
- [ ] 事件通信正常
- [ ] 无 CLI 相关命名残留（除项目名称）

---

**执行人**: AI Assistant
**执行日期**: 2025-01-22
**文档版本**: 1.0
