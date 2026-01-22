# MultiCLI 架构分析：CLI 编排 → LLM 编排迁移状态

## 📋 概述

当前项目正在进行重大架构重构：**从 CLI 编排模式迁移到 LLM 编排模式**。

- **原架构**：通过 PTY 启动多个 CLI 进程（claude-cli, codex-cli, gemini-cli），通过标准输入输出与它们交互
- **新架构**：直接使用 LLM API（Anthropic, OpenAI, Google），通过 HTTP 调用与 LLM 交互
- **当前状态**：框架改造完成，但前后端交互层存在遗留的 CLI 交互代码未适配

---

## 🏗️ 当前架构状态

### ✅ 已完成的改造

#### 1. **LLM 适配器层** (`src/llm/`)
- ✅ `LLMAdapterFactory`: 统一的适配器工厂，替代原 CLI 适配器工厂
- ✅ `OrchestratorLLMAdapter`: 编排者 LLM 适配器（负责任务分解和协调）
- ✅ `WorkerLLMAdapter`: Worker LLM 适配器（负责执行具体任务）
- ✅ `LLMClient`: 统一的 LLM 客户端接口（支持 Anthropic, OpenAI, Google）
- ✅ `LLMConfigLoader`: 配置管理（从 `.multicli/llm-config.json` 加载）

#### 2. **Mission-Driven 编排引擎** (`src/orchestrator/core/`)
- ✅ `MissionDrivenEngine`: 新的编排引擎，替代原 `OrchestratorAgent`
- ✅ `MissionOrchestrator`: 任务分解和规划（使用 Orchestrator LLM）
- ✅ `MissionExecutor`: 任务执行协调（管理多个 Worker LLM）
- ✅ `IntelligentOrchestrator`: 高层编排器，封装 MissionDrivenEngine

#### 3. **统一接口** (`src/adapters/adapter-factory-interface.ts`)
- ✅ `IAdapterFactory`: 统一的适配器工厂接口
  - `sendMessage()`: 发送消息到指定代理
  - `interrupt()`: 中断代理操作
  - `shutdown()`: 关闭所有适配器
  - `isConnected()`: 检查连接状态
  - `isBusy()`: 检查忙碌状态

#### 4. **消息规范化** (`src/normalizer/`)
- ✅ `UnifiedMessageBus`: 统一消息总线
- ✅ `BaseNormalizer`: 消息规范化基类
- ✅ 支持流式输出、工具调用、错误处理

---

### ❌ 未完成的改造

#### 1. **前后端交互层遗留问题**

**问题位置**: `src/ui/webview-provider.ts` 第 469-513 行

```typescript
private handleCliQuestionAnswer(
  cli: CLIType,
  questionId: string,
  answer: string,
  adapterRole?: 'worker' | 'orchestrator'
): void {
  logger.info('界面.CLI.提问.回答', { cli, questionId, answer, role: adapterRole || 'worker' }, LogCategory.UI);

  const role = adapterRole || 'worker';
  // TODO: LLM mode doesn't support writeInput - this needs to be refactored
  const success = false; // this.adapterFactory.writeInput(cli, answer, role);

  if (success) {
    // ... 发送成功消息
  } else {
    // ... 发送失败消息
  }
}
```

**问题分析**：
1. ❌ 原 CLI 模式通过 `writeInput()` 向 PTY 进程写入用户输入
2. ❌ LLM 模式没有 PTY 进程，不支持 `writeInput()`
3. ❌ 前端仍然发送 `answerCliQuestion` 消息，但后端无法处理
4. ❌ 导致用户无法回答 LLM 的问题（如需要澄清需求、确认操作等）

---

## 🔍 CLI 模式 vs LLM 模式对比

### CLI 模式（旧架构）

```
┌─────────────┐
│   前端 UI    │
└──────┬──────┘
       │ answerCliQuestion
       ▼
┌─────────────────────┐
│  WebviewProvider    │
│  handleCliQuestion  │
└──────┬──────────────┘
       │ writeInput()
       ▼
┌─────────────────────┐
│  CLIAdapterFactory  │
│  (PTY 管理)         │
└──────┬──────────────┘
       │ stdin.write()
       ▼
┌─────────────────────┐
│  claude-cli 进程    │
│  (PTY)              │
└─────────────────────┘
```

**特点**：
- 通过 PTY 启动独立的 CLI 进程
- 通过标准输入输出与 CLI 交互
- CLI 可以主动提问（通过 stdout）
- 用户回答通过 stdin 发送

### LLM 模式（新架构）

```
┌─────────────┐
│   前端 UI    │
└──────┬──────┘
       │ ??? (需要设计)
       ▼
┌─────────────────────┐
│  WebviewProvider    │
│  ??? (需要实现)     │
└──────┬──────────────┘
       │ ???
       ▼
┌─────────────────────┐
│  LLMAdapterFactory  │
│  (HTTP 客户端)      │
└──────┬──────────────┘
       │ HTTP POST
       ▼
┌─────────────────────┐
│  LLM API            │
│  (Anthropic/OpenAI) │
└─────────────────────┘
```

**特点**：
- 通过 HTTP API 与 LLM 交互
- 无状态的请求-响应模式
- LLM 不能"主动提问"，只能在响应中包含问题
- 需要将问题作为消息内容返回给前端

---

## 🎯 核心问题

### 问题 1: LLM 如何"提问"？

**CLI 模式**：
- CLI 进程可以输出 `[QUESTION]` 标记
- 后端监听 stdout，识别问题标记
- 暂停执行，等待用户输入
- 用户回答后通过 stdin 发送

**LLM 模式**：
- ❌ LLM API 是无状态的，不能"暂停"等待输入
- ✅ 需要在 LLM 响应中识别问题（通过特定格式或工具调用）
- ✅ 将问题作为消息发送到前端
- ✅ 用户回答后，将答案作为新的消息发送给 LLM

### 问题 2: 如何处理用户回答？

**CLI 模式**：
```typescript
// 直接写入 PTY 进程的 stdin
this.adapterFactory.writeInput(cli, answer, role);
```

**LLM 模式**：
```typescript
// 需要将答案作为新的消息发送给 LLM
await this.adapterFactory.sendMessage(
  agent,
  `User's answer to your question: ${answer}`,
  undefined,
  { adapterRole: role }
);
```

### 问题 3: 如何维护对话上下文？

**CLI 模式**：
- PTY 进程维护自己的状态
- 后端只需要转发输入输出

**LLM 模式**：
- 需要在适配器中维护对话历史（`conversationHistory`）
- 每次调用都需要发送完整的对话历史
- 需要管理 token 限制（可能需要压缩历史）

---

## 📊 当前代码中的交互点

### 前端发送的消息类型（与 CLI 交互相关）

从 `src/ui/webview/index.html` 中识别：

1. ✅ **已适配**：
   - `executeTask`: 执行任务（已通过 `IntelligentOrchestrator` 处理）
   - `interruptTask`: 中断任务（已通过 `interrupt()` 处理）
   - `confirmPlan`: 确认执行计划（已通过回调处理）
   - `answerQuestions`: 回答编排器问题（已通过回调处理）
   - `answerClarification`: 回答需求澄清（已通过回调处理）
   - `answerWorkerQuestion`: 回答 Worker 问题（已通过回调处理）

2. ❌ **未适配**：
   - `answerCliQuestion`: 回答 CLI 提问（**核心问题**）
     - 前端代码：`vscode.postMessage({ type: 'answerCliQuestion', cli, questionId, answer, adapterRole })`
     - 后端处理：`handleCliQuestionAnswer()` - 当前返回失败

### 后端发送的消息类型（与 CLI 交互相关）

从 `src/ui/webview-provider.ts` 中识别：

1. ✅ **已适配**：
   - `standardMessage`: 标准消息（通过 `UnifiedMessageBus` 处理）
   - `stream`: 流式更新（通过 normalizer 处理）
   - `phaseChanged`: 阶段变化（通过事件总线处理）

2. ❌ **可能需要适配**：
   - `cliQuestionAsked`: CLI 提问（需要从 LLM 响应中识别）
   - `cliQuestionAnswered`: CLI 回答确认（需要实现）

---

## 🛠️ 解决方案设计

### 方案 1: 工具调用方式（推荐）

**原理**：将"提问"作为一个工具（Tool），LLM 通过工具调用来提问。

#### 实现步骤：

1. **定义 `ask_user` 工具**：
```typescript
{
  name: 'ask_user',
  description: 'Ask the user a question when you need clarification or additional information',
  input_schema: {
    type: 'object',
    properties: {
      question: {
        type: 'string',
        description: 'The question to ask the user'
      },
      context: {
        type: 'string',
        description: 'Context about why you are asking this question'
      },
      options: {
        type: 'array',
        items: { type: 'string' },
        description: 'Optional: Suggested answers for the user to choose from'
      }
    },
    required: ['question']
  }
}
```

2. **在 Worker 适配器中处理工具调用**：
```typescript
// src/llm/adapters/worker-adapter.ts
if (toolCall.name === 'ask_user') {
  // 暂停执行，等待用户回答
  const answer = await this.waitForUserAnswer(
    toolCall.arguments.question,
    toolCall.arguments.context,
    toolCall.arguments.options
  );

  // 返回工具结果
  return {
    toolCallId: toolCall.id,
    content: answer,
    isError: false
  };
}
```

3. **实现等待机制**：
```typescript
private pendingQuestion: {
  resolve: (answer: string) => void;
  reject: (error: Error) => void;
} | null = null;

async waitForUserAnswer(question: string, context: string, options?: string[]): Promise<string> {
  // 发送问题到前端
  this.emit('userQuestionNeeded', {
    question,
    context,
    options,
    workerId: this.workerSlot
  });

  // 等待用户回答
  return new Promise((resolve, reject) => {
    this.pendingQuestion = { resolve, reject };

    // 设置超时
    setTimeout(() => {
      if (this.pendingQuestion) {
        this.pendingQuestion.reject(new Error('User answer timeout'));
        this.pendingQuestion = null;
      }
    }, 5 * 60 * 1000); // 5分钟超时
  });
}

// 接收用户回答
answerUserQuestion(answer: string): void {
  if (this.pendingQuestion) {
    this.pendingQuestion.resolve(answer);
    this.pendingQuestion = null;
  }
}
```

4. **前端处理**：
```typescript
// 监听 userQuestionNeeded 事件
this.adapterFactory.on('userQuestionNeeded', (data) => {
  this.postMessage({
    type: 'workerQuestionAsked',
    workerId: data.workerId,
    question: data.question,
    context: data.context,
    options: data.options
  });
});

// 处理用户回答
case 'answerWorkerQuestion':
  const adapter = this.adapterFactory.getAdapter(message.workerId);
  if (adapter instanceof WorkerLLMAdapter) {
    adapter.answerUserQuestion(message.answer);
  }
  break;
```

**优点**：
- ✅ 符合 LLM 工具调用的标准模式
- ✅ 可以在工具定义中明确问题格式
- ✅ 支持多轮对话（工具调用可以嵌套）
- ✅ 易于扩展（可以添加更多交互工具）

**缺点**：
- ⚠️ 需要 LLM 支持工具调用（Anthropic Claude 支持）
- ⚠️ 增加了一次 LLM 调用（工具调用 → 工具结果 → 继续生成）

---

### 方案 2: 消息格式识别方式

**原理**：在 LLM 响应中识别特定格式的问题标记。

#### 实现步骤：

1. **在系统提示中定义问题格式**：
```typescript
const systemPrompt = `
When you need to ask the user a question, use this format:
[ASK_USER]
Question: <your question>
Context: <why you are asking>
Options: <optional comma-separated options>
[/ASK_USER]
`;
```

2. **在 normalizer 中识别问题**：
```typescript
// src/normalizer/base-normalizer.ts
processChunk(messageId: string, chunk: string): void {
  // 检测问题标记
  if (chunk.includes('[ASK_USER]')) {
    this.handleUserQuestion(messageId, chunk);
    return;
  }

  // 正常处理
  this.emit('stream', { messageId, content: chunk });
}

private handleUserQuestion(messageId: string, content: string): void {
  const match = content.match(/\[ASK_USER\](.*?)\[\/ASK_USER\]/s);
  if (match) {
    const questionBlock = match[1];
    const question = this.extractField(questionBlock, 'Question');
    const context = this.extractField(questionBlock, 'Context');
    const options = this.extractField(questionBlock, 'Options')?.split(',');

    this.emit('userQuestionNeeded', {
      question,
      context,
      options
    });
  }
}
```

**优点**：
- ✅ 不依赖工具调用功能
- ✅ 可以在任何 LLM 上使用
- ✅ 实现相对简单

**缺点**：
- ❌ 依赖 LLM 遵守格式约定（可能不可靠）
- ❌ 难以处理复杂的交互场景
- ❌ 可能与正常输出混淆

---

### 方案 3: 混合方式（最佳实践）

**原理**：优先使用工具调用，降级到消息格式识别。

```typescript
// 1. 注册 ask_user 工具
this.toolManager.registerTool({
  name: 'ask_user',
  // ...
});

// 2. 在系统提示中说明两种方式
const systemPrompt = `
You can ask the user questions in two ways:
1. Use the ask_user tool (preferred)
2. Use [ASK_USER] tags in your response (fallback)
`;

// 3. 在适配器中同时处理两种方式
if (toolCall.name === 'ask_user') {
  // 工具调用方式
} else if (content.includes('[ASK_USER]')) {
  // 格式识别方式
}
```

---

## 📝 实施计划

### Phase 1: 核心功能实现（优先级：高）

1. **实现 `ask_user` 工具**
   - [ ] 在 `ToolManager` 中注册工具
   - [ ] 在 `WorkerLLMAdapter` 中处理工具调用
   - [ ] 实现等待用户回答的机制

2. **适配前后端交互**
   - [ ] 修改 `handleCliQuestionAnswer` 为 `handleWorkerQuestionAnswer`
   - [ ] 实现 `answerUserQuestion` 方法
   - [ ] 更新前端消息类型

3. **测试验证**
   - [ ] 单元测试：工具调用流程
   - [ ] 集成测试：完整的问答流程
   - [ ] E2E 测试：前端到后端的完整交互

### Phase 2: 增强功能（优先级：中）

1. **支持多种问题类型**
   - [ ] 是/否问题
   - [ ] 单选问题
   - [ ] 多选问题
   - [ ] 自由文本输入

2. **超时和错误处理**
   - [ ] 用户回答超时处理
   - [ ] 用户取消回答处理
   - [ ] 网络错误重试

3. **对话历史管理**
   - [ ] 压缩长对话历史
   - [ ] 保存问答记录
   - [ ] 恢复中断的对话

### Phase 3: 优化和清理（优先级：低）

1. **清理遗留代码**
   - [ ] 删除 CLI 相关的代码
   - [ ] 更新文档和注释
   - [ ] 统一命名规范

2. **性能优化**
   - [ ] 减少不必要的 LLM 调用
   - [ ] 优化对话历史存储
   - [ ] 实现缓存机制

---

## 🔗 相关文件清单

### 需要修改的文件

1. **后端核心**：
   - `src/ui/webview-provider.ts` - 前后端交互入口
   - `src/llm/adapters/worker-adapter.ts` - Worker 适配器
   - `src/tools/tool-manager.ts` - 工具管理器

2. **前端**：
   - `src/ui/webview/index.html` - 前端 UI 和消息处理

3. **类型定义**：
   - `src/types.ts` - 消息类型定义
   - `src/llm/types.ts` - LLM 相关类型

### 可以参考的文件

1. **已实现的回调机制**：
   - `src/orchestrator/intelligent-orchestrator.ts` - 确认回调、澄清回调
   - `src/orchestrator/core/mission-driven-engine.ts` - Worker 问题回调

2. **工具调用示例**：
   - `src/tools/shell-executor.ts` - Shell 工具实现
   - `src/tools/mcp-manager.ts` - MCP 工具实现

---

## 🎓 关键概念

### 1. 适配器模式（Adapter Pattern）

```
IAdapterFactory (接口)
    ↑
    ├── CLIAdapterFactory (旧实现，已废弃)
    └── LLMAdapterFactory (新实现)
            ↓
        BaseLLMAdapter (基类)
            ↑
            ├── OrchestratorLLMAdapter
            └── WorkerLLMAdapter
```

### 2. 事件驱动架构（Event-Driven Architecture）

```
LLMAdapter
    ↓ emit('userQuestionNeeded')
AdapterFactory
    ↓ emit('userQuestionNeeded')
WebviewProvider
    ↓ postMessage({ type: 'workerQuestionAsked' })
Frontend UI
    ↓ vscode.postMessage({ type: 'answerWorkerQuestion' })
WebviewProvider
    ↓ adapter.answerUserQuestion()
LLMAdapter
    ↓ resolve(pendingQuestion)
Continue execution
```

### 3. 工具调用流程（Tool Calling Flow）

```
1. LLM 决定调用工具
   ↓
2. 返回 tool_use 块
   ↓
3. 适配器执行工具
   ↓
4. 返回 tool_result 块
   ↓
5. LLM 继续生成响应
```

---

## 🚨 注意事项

1. **向后兼容性**：
   - 保留 `IAdapterFactory` 接口不变
   - 确保现有的消息类型仍然有效
   - 逐步迁移，不要一次性删除所有 CLI 代码

2. **错误处理**：
   - 用户可能不回答问题（超时）
   - 用户可能取消操作
   - 网络可能中断
   - LLM 可能不遵守工具调用约定

3. **性能考虑**：
   - 每次工具调用都会增加一次 LLM 请求
   - 对话历史会随着交互增长
   - 需要考虑 token 限制

4. **用户体验**：
   - 问题应该清晰明确
   - 提供合理的默认选项
   - 显示问题的上下文
   - 允许用户取消或跳过

---

## 📚 参考资料

1. **Anthropic Claude API**：
   - Tool Use: https://docs.anthropic.com/claude/docs/tool-use
   - Streaming: https://docs.anthropic.com/claude/docs/streaming

2. **设计模式**：
   - Adapter Pattern
   - Observer Pattern
   - Promise Pattern

3. **相关项目**：
   - Claude Code (官方 CLI)
   - Cursor (AI IDE)
   - Augment Code (AI 编程助手)

---

## 🎯 下一步行动

1. **立即行动**：
   - 实现 `ask_user` 工具
   - 修改 `handleCliQuestionAnswer` 方法
   - 更新前端消息处理

2. **短期目标**（1-2 天）：
   - 完成基本的问答功能
   - 通过集成测试
   - 更新文档

3. **中期目标**（1 周）：
   - 支持多种问题类型
   - 完善错误处理
   - 优化用户体验

4. **长期目标**（2-4 周）：
   - 清理所有 CLI 遗留代码
   - 完善文档和示例
   - 发布新版本

---

**最后更新**: 2025-01-22
**文档版本**: 1.0
**作者**: AI Assistant
