# CLI 到 LLM 重构方案

## 1. 重构目标

将现有的 CLI 模式（依赖外部命令行工具）彻底替换为 LLM API 直接接入模式，解决以下问题：

### 当前问题
- ❌ CLI 工具安装和配置复杂
- ❌ 进程管理复杂（stdin/stdout、超时、中断）
- ❌ 输出格式不统一，解析困难
- ❌ 响应速度慢（进程启动开销）
- ❌ 调试和错误处理困难

### 新模式优势
- ✅ 直接调用 API，无外部依赖
- ✅ 统一的请求/响应格式
- ✅ 更好的流式支持
- ✅ 更快的响应速度
- ✅ 更容易测试和调试
- ✅ 更好的错误处理

---

## 2. 架构设计

### 2.1 新的层次结构

```
┌─────────────────────────────────────┐
│      UI Layer (Webview)             │
│  - 保持不变，使用 StandardMessage   │
└─────────────────┬───────────────────┘
                  │
┌─────────────────▼───────────────────┐
│   Orchestrator Layer                │
│  - IntelligentOrchestrator          │
│  - MissionDrivenEngine              │
│  - 保持现有编排逻辑                  │
└─────────────────┬───────────────────┘
                  │
┌─────────────────▼───────────────────┐
│   LLM Adapter Factory               │
│  - 管理多个 LLM 适配器              │
│  - 事件转发和标准化                 │
│  - Normalizer 集成                  │
└─────────────────┬───────────────────┘
                  │
        ┌─────────┼─────────┐
        │         │         │
┌───────▼──┐ ┌───▼────┐ ┌──▼──────┐
│ Claude   │ │ Gemini │ │   GPT   │
│ Adapter  │ │Adapter │ │ Adapter │
└───────┬──┘ └───┬────┘ └──┬──────┘
        │        │          │
┌───────▼──┐ ┌───▼────┐ ┌──▼──────┐
│ Claude   │ │ Gemini │ │   GPT   │
│ Client   │ │ Client │ │ Client  │
│ (SDK)    │ │ (SDK)  │ │  (SDK)  │
└───────┬──┘ └───┬────┘ └──┬──────┘
        │        │          │
        └────────┼──────────┘
                 │
        ┌────────▼─────────┐
        │  External APIs   │
        │ (Anthropic, etc) │
        └──────────────────┘
```

### 2.2 核心组件

#### LLMClient (新增)
```typescript
interface LLMClient {
  // 基础配置
  provider: 'claude' | 'gemini' | 'gpt';
  model: string;

  // 核心方法
  sendMessage(params: LLMMessageParams): Promise<LLMResponse>;
  streamMessage(params: LLMMessageParams): AsyncIterator<LLMStreamChunk>;

  // 工具调用
  supportsTools(): boolean;

  // 连接管理
  testConnection(): Promise<boolean>;
}
```

#### LLMAdapter (替代 CLIAdapter)
```typescript
interface LLMAdapter {
  type: 'claude' | 'gemini' | 'gpt';
  role: 'orchestrator' | 'worker';

  // 保持与现有接口兼容
  connect(): Promise<void>;
  disconnect(): Promise<void>;
  sendMessage(message: string, images?: string[]): Promise<AdapterResponse>;
  interrupt(): Promise<void>;

  // 状态
  isConnected: boolean;
  isBusy: boolean;
}
```

#### LLMAdapterFactory (替代 CLIAdapterFactory)
```typescript
class LLMAdapterFactory extends EventEmitter {
  // 与 CLIAdapterFactory 相同的接口
  sendMessage(type: LLMType, message: string, images?: string[]): Promise<Response>;
  interrupt(type: LLMType): Promise<void>;

  // 事件保持不变
  on('stream', handler);
  on('standardComplete', handler);
  on('standardMessage', handler);
}
```

#### ContextManager (简化的 SessionManager)
```typescript
class ContextManager {
  // 会话历史管理
  addMessage(role: 'user' | 'assistant', content: string): void;
  getHistory(maxTokens?: number): Message[];
  clearHistory(): void;

  // Token 管理
  estimateTokens(text: string): number;
  getTotalTokens(): number;

  // 上下文窗口
  trimToWindow(maxTokens: number): void;
}
```

---

## 3. 重构计划

### 阶段 1: 基础设施层 (2-3天)

**目标：** 实现 LLM Client 层，支持直接 API 调用

**任务：**
- [ ] 安装依赖包
  ```bash
  npm install @anthropic-ai/sdk @google/generative-ai openai
  ```
- [ ] 创建目录结构
  ```
  src/llm/
    ├── clients/
    │   ├── base-client.ts
    │   ├── claude-client.ts
    │   ├── gemini-client.ts
    │   └── gpt-client.ts
    ├── types.ts
    └── config.ts
  ```
- [ ] 实现 `BaseLLMClient` 抽象类
- [ ] 实现 `ClaudeClient` (优先)
  - 使用 `@anthropic-ai/sdk`
  - 支持流式响应
  - 支持工具调用
- [ ] 实现 `GeminiClient`
  - 使用 `@google/generative-ai`
  - 支持流式响应
  - 支持 Function Calling
- [ ] 实现 `GPTClient`
  - 使用 `openai`
  - 支持流式响应
  - 支持 Function Calling
- [ ] 配置管理
  - API Key 管理
  - 模型选择
  - 参数配置（temperature, max_tokens）

**验收标准：**
- 所有 Client 可以成功调用 API
- 流式响应正常工作
- 工具调用功能正常
- 单元测试通过

---

### 阶段 2: 适配器层 (2-3天)

**目标：** 实现 LLM Adapter，保持与现有接口兼容

**任务：**
- [ ] 创建目录结构
  ```
  src/llm/
    ├── adapters/
    │   ├── base-adapter.ts
    │   ├── claude-adapter.ts
    │   ├── gemini-adapter.ts
    │   └── gpt-adapter.ts
    └── adapter-factory.ts
  ```
- [ ] 实现 `BaseLLMAdapter`
  - 继承 EventEmitter
  - 实现通用逻辑（状态管理、事件发送）
- [ ] 实现具体适配器
  - `ClaudeAdapter`
  - `GeminiAdapter`
  - `GPTAdapter`
- [ ] 角色支持
  - orchestrator 角色配置
  - worker 角色配置
- [ ] 工具调用适配
  - 统一的工具定义格式
  - 工具调用结果处理

**验收标准：**
- 适配器接口与 CLIAdapter 兼容
- 支持 orchestrator 和 worker 角色
- 工具调用正常工作
- 事件系统正常

---

### 阶段 3: 工厂和管理层 (1-2天)

**目标：** 实现 LLMAdapterFactory 和 ContextManager

**任务：**
- [ ] 实现 `LLMAdapterFactory`
  - 与 `CLIAdapterFactory` 相同的接口
  - 适配器实例管理
  - 事件转发
  - Normalizer 集成
- [ ] 实现 `ContextManager`
  - 会话历史管理
  - Token 计数
  - 上下文窗口控制
  - 历史导入（从旧会话）
- [ ] 配置系统
  - VS Code 配置项
  - API Key 安全存储
  - 模型选择 UI

**验收标准：**
- LLMAdapterFactory 可以替换 CLIAdapterFactory
- ContextManager 正常管理会话历史
- 配置系统可用

---

### 阶段 4: 编排器集成 (1-2天)

**目标：** 将编排器切换到 LLM 模式

**任务：**
- [ ] 修改 `IntelligentOrchestrator`
  - 使用 `LLMAdapterFactory` 替代 `CLIAdapterFactory`
  - 保持现有编排逻辑
- [ ] 修改 `MissionDrivenEngine`
  - 适配新的适配器接口
  - 保持任务执行逻辑
- [ ] 修改 `extension.ts`
  - 初始化 LLM 系统
  - 配置加载
- [ ] 兼容性处理
  - 支持配置切换（CLI/LLM）
  - 平滑迁移

**验收标准：**
- 编排器使用 LLM 模式正常工作
- 任务执行流程正常
- 配置切换功能正常

---

### 阶段 5: UI 和测试 (1-2天)

**目标：** 完成 UI 适配和全面测试

**任务：**
- [ ] UI 验证
  - StandardMessage 协议兼容性
  - 流式输出显示
  - 工具调用显示
- [ ] 配置 UI
  - API Key 输入
  - 模型选择
  - 提供商切换
- [ ] 端到端测试
  - 简单对话测试
  - 任务执行测试
  - 工具调用测试
- [ ] 性能测试
  - 响应速度测试
  - Token 使用统计
- [ ] 错误处理测试
  - API 错误处理
  - 网络错误处理
  - 限流处理

**验收标准：**
- UI 显示正常
- 所有功能测试通过
- 性能达标
- 错误处理完善

---

## 4. 目录结构

```
src/
├── llm/                          # 新增 LLM 模块
│   ├── clients/                  # LLM Client 层
│   │   ├── base-client.ts
│   │   ├── claude-client.ts
│   │   ├── gemini-client.ts
│   │   └── gpt-client.ts
│   ├── adapters/                 # LLM Adapter 层
│   │   ├── base-adapter.ts
│   │   ├── claude-adapter.ts
│   │   ├── gemini-adapter.ts
│   │   └── gpt-adapter.ts
│   ├── adapter-factory.ts        # LLM 适配器工厂
│   ├── context-manager.ts        # 上下文管理器
│   ├── types.ts                  # 类型定义
│   └── config.ts                 # 配置管理
├── cli/                          # 保留但标记为 deprecated
│   └── ...
├── normalizer/                   # 保留，简化
│   ├── base-normalizer.ts
│   ├── claude-normalizer.ts
│   └── ...
├── orchestrator/                 # 修改以使用 LLM
│   ├── intelligent-orchestrator.ts
│   └── core/
│       └── mission-driven-engine.ts
└── extension.ts                  # 修改初始化逻辑
```

---

## 5. 配置示例

### VS Code 配置

```json
{
  "multiCLI.mode": "llm",  // "cli" | "llm"
  "multiCLI.llm.orchestrator": {
    "provider": "claude",
    "model": "claude-3-5-sonnet-20241022",
    "apiKey": "${env:ANTHROPIC_API_KEY}",
    "maxTokens": 8192,
    "temperature": 0.7
  },
  "multiCLI.llm.worker": {
    "provider": "claude",
    "model": "claude-3-5-sonnet-20241022",
    "apiKey": "${env:ANTHROPIC_API_KEY}",
    "maxTokens": 4096,
    "temperature": 0.5
  },
  "multiCLI.llm.enablePromptCaching": true,
  "multiCLI.llm.maxRetries": 3
}
```

### 环境变量

```bash
# .env
ANTHROPIC_API_KEY=sk-ant-xxx
GOOGLE_API_KEY=AIzaSyxxx
OPENAI_API_KEY=sk-xxx
```

---

## 6. 接口定义

### LLMMessageParams

```typescript
interface LLMMessageParams {
  messages: Array<{
    role: 'user' | 'assistant' | 'system';
    content: string | Array<ContentBlock>;
  }>;
  model?: string;
  maxTokens?: number;
  temperature?: number;
  tools?: ToolDefinition[];
  stream?: boolean;
}
```

### LLMResponse

```typescript
interface LLMResponse {
  content: string;
  toolCalls?: ToolCall[];
  usage: {
    inputTokens: number;
    outputTokens: number;
  };
  stopReason: 'end_turn' | 'max_tokens' | 'tool_use';
}
```

### LLMStreamChunk

```typescript
interface LLMStreamChunk {
  type: 'content_delta' | 'tool_call_delta' | 'complete';
  delta?: string;
  toolCall?: Partial<ToolCall>;
  usage?: TokenUsage;
}
```

---

## 7. 迁移策略

### 7.1 渐进式迁移

1. **并存期**
   - CLI 和 LLM 模式同时存在
   - 通过配置切换
   - 默认使用 LLM 模式

2. **过渡期**
   - 保留 CLI 模式 3 个月
   - 标记为 deprecated
   - 提供迁移指南

3. **完全迁移**
   - 移除 CLI 相关代码
   - 清理依赖

### 7.2 用户迁移指南

```markdown
# 从 CLI 模式迁移到 LLM 模式

## 1. 安装 API Keys
- 获取 Anthropic API Key
- 配置环境变量或 VS Code 设置

## 2. 更新配置
- 设置 `multiCLI.mode` 为 `"llm"`
- 配置 LLM 提供商和模型

## 3. 测试
- 运行简单对话测试
- 验证工具调用功能

## 4. 卸载 CLI 工具（可选）
- 不再需要 `claude` 命令行工具
```

---

## 8. 风险和缓解

### 8.1 API 成本

**风险：** API 调用成本高于本地 CLI

**缓解措施：**
- 实现 Prompt Caching（Claude 支持）
- Token 使用监控和限制
- 提供成本估算工具
- 支持本地模型作为备选（未来）

### 8.2 网络依赖

**风险：** 依赖网络连接

**缓解措施：**
- 实现重试机制（指数退避）
- 提供离线模式提示
- 缓存常用响应

### 8.3 API 限流

**风险：** API 请求频率限制

**缓解措施：**
- 实现请求队列
- 速率限制
- 优雅降级

### 8.4 兼容性

**风险：** 破坏现有功能

**缓解措施：**
- 保持接口兼容
- 全面测试
- 渐进式迁移

---

## 9. 时间估算

| 阶段 | 任务 | 预计时间 |
|------|------|----------|
| 阶段 1 | 基础设施层 | 2-3 天 |
| 阶段 2 | 适配器层 | 2-3 天 |
| 阶段 3 | 工厂和管理层 | 1-2 天 |
| 阶段 4 | 编排器集成 | 1-2 天 |
| 阶段 5 | UI 和测试 | 1-2 天 |
| **总计** | | **7-12 天** |

---

## 10. 成功标准

### 功能完整性
- ✅ 所有现有功能正常工作
- ✅ 流式输出正常
- ✅ 工具调用正常
- ✅ 编排器逻辑正常

### 性能指标
- ✅ 响应速度 < 5 秒（首次响应）
- ✅ 流式输出延迟 < 100ms
- ✅ Token 使用效率提升 20%

### 质量标准
- ✅ 单元测试覆盖率 > 80%
- ✅ 端到端测试通过
- ✅ 无严重 Bug

### 用户体验
- ✅ 配置简单（< 5 分钟）
- ✅ 错误提示清晰
- ✅ 文档完善

---

## 11. 下一步行动

1. **立即开始：** 阶段 1 - 基础设施层
   - 安装依赖包
   - 创建目录结构
   - 实现 ClaudeClient

2. **准备工作：**
   - 获取测试用 API Keys
   - 设置开发环境
   - 创建测试用例

3. **沟通计划：**
   - 通知用户即将进行重构
   - 提供迁移指南
   - 收集反馈

---

## 12. 参考资料

- [Anthropic API Documentation](https://docs.anthropic.com/claude/reference/getting-started-with-the-api)
- [Google Generative AI SDK](https://ai.google.dev/tutorials/node_quickstart)
- [OpenAI API Documentation](https://platform.openai.com/docs/api-reference)
- [VS Code Extension API](https://code.visualstudio.com/api)
