# CLI 消息日志增强说明

## 概述

增强了 CLI 消息日志系统，确保对话过程的完整记录，包括：
1. **完整的原始消息**（文件日志不截断）
2. **格式处理后的消息**（用于纠错对比）
3. **对话上下文追踪**（会话、任务、消息序号）
4. **控制台友好显示**（避免刷屏）

---

## 新增功能

### 1. 完整内容保存

**控制台显示**：截断到 500 字符（可配置）
**文件日志**：保存完整内容（默认不限制）

```typescript
// 配置
{
  cli: {
    maxLength: 500,        // 控制台显示最大长度
    maxLengthFile: 0,      // 文件日志最大长度（0 = 不限制）
  }
}
```

### 2. 格式处理追踪

记录消息处理前后的内容，方便对比和纠错：

```typescript
logger.logCLIMessage({
  cli: 'claude',
  role: 'worker',
  requestId: 'req-123',
  message: originalMessage,           // 原始消息
  processedMessage: formattedMessage, // 格式处理后的消息
  metadata: { taskId: 'task-456' }
});
```

### 3. 对话上下文

追踪完整的对话上下文：

```typescript
logger.logCLIMessage({
  cli: 'claude',
  role: 'worker',
  requestId: 'req-123',
  message: prompt,
  conversationContext: {
    sessionId: 'session-abc',
    taskId: 'task-456',
    subTaskId: 'subtask-789',
    messageIndex: 2,      // 第 3 条消息（从 0 开始）
    totalMessages: 5      // 总共 5 条消息
  }
});
```

---

## 使用示例

### 基本用法（向后兼容）

```typescript
import { logger } from '../logging';

// 发送消息
logger.logCLIMessage({
  cli: 'claude',
  role: 'worker',
  requestId: 'req-123',
  message: prompt,
  metadata: { taskId: 'task-456' }
});

// 接收响应
logger.logCLIResponse({
  cli: 'claude',
  role: 'worker',
  requestId: 'req-123',
  response: result.content,
  duration: Date.now() - startTime,
  metadata: { taskId: 'task-456' }
});
```

### 增强用法（推荐）

```typescript
import { logger } from '../logging';

// 1. 准备消息
const originalPrompt = getUserInput();
const formattedPrompt = formatPromptForCLI(originalPrompt);

// 2. 记录发送（包含原始和处理后的内容）
logger.logCLIMessage({
  cli: 'claude',
  role: 'worker',
  requestId: requestId,
  message: originalPrompt,              // 原始消息
  processedMessage: formattedPrompt,    // 格式处理后
  conversationContext: {
    sessionId: currentSession.id,
    taskId: task.id,
    subTaskId: subtask?.id,
    messageIndex: conversationIndex,
    totalMessages: estimatedTotal
  },
  metadata: {
    taskId: task.id,
    subTaskId: subtask?.id,
    promptLength: originalPrompt.length,
    formattedLength: formattedPrompt.length
  }
});

// 3. 发送到 CLI
const startTime = Date.now();
const response = await sendToCLI(formattedPrompt);

// 4. 处理响应
const processedResponse = parseResponse(response.content);

// 5. 记录接收（包含原始和处理后的内容）
logger.logCLIResponse({
  cli: 'claude',
  role: 'worker',
  requestId: requestId,
  response: response.content,           // 原始响应
  processedResponse: processedResponse, // 处理后的响应
  duration: Date.now() - startTime,
  conversationContext: {
    sessionId: currentSession.id,
    taskId: task.id,
    subTaskId: subtask?.id,
    messageIndex: conversationIndex,
    totalMessages: estimatedTotal
  },
  metadata: {
    taskId: task.id,
    subTaskId: subtask?.id,
    responseLength: response.content.length,
    processedLength: processedResponse.length,
    success: true
  }
});
```

---

## 控制台输出示例

### 发送消息

```
━━━ CLI 发送 ━━━
  时间: 12:34:56.789
  CLI: claude (worker)
  Request ID: req-123
  Session: session-abc
  Task: task-456 / subtask-789
  Message: 3/5
  ┌─────────────────────────────────────────────────────────────┐
  │ 请帮我分析这段代码的性能问题...                              │
  │ ... (截断，总长度: 1234)                                     │
  │ (已格式化处理，详见文件日志)                                 │
  └─────────────────────────────────────────────────────────────┘
```

### 接收响应

```
━━━ CLI 接收 ━━━
  时间: 12:34:58.123
  CLI: claude (worker)
  Request ID: req-123
  Session: session-abc
  Task: task-456 / subtask-789
  Message: 3/5
  Duration: 1.33s
  ┌─────────────────────────────────────────────────────────────┐
  │ 这段代码存在以下性能问题：                                   │
  │ 1. 循环中重复创建对象...                                     │
  │ ... (截断，总长度: 2345)                                     │
  │ (已格式化处理，详见文件日志)                                 │
  └─────────────────────────────────────────────────────────────┘
```

---

## 文件日志格式

文件日志以 JSON Lines 格式保存，每行一个 JSON 对象：

```json
{
  "timestamp": "2026-01-18T12:34:56.789Z",
  "type": "cli-message",
  "direction": "send",
  "cli": "claude",
  "role": "worker",
  "requestId": "req-123",
  "content": "完整的原始消息内容...",
  "contentLength": 1234,
  "processedContent": "格式处理后的消息内容...",
  "conversationContext": {
    "sessionId": "session-abc",
    "taskId": "task-456",
    "subTaskId": "subtask-789",
    "messageIndex": 2,
    "totalMessages": 5
  },
  "duration": null,
  "metadata": {
    "taskId": "task-456",
    "subTaskId": "subtask-789",
    "promptLength": 1234,
    "formattedLength": 1456
  },
  "truncatedInFile": false
}
```

---

## 配置选项

### 环境变量

```bash
# 启用文件日志
export MULTICLI_LOG_FILE=.multicli/logs/app.log

# 控制台显示长度（默认 500）
export MULTICLI_CLI_MAX_LENGTH=500

# 文件日志长度（0 = 不限制，默认 0）
export MULTICLI_CLI_MAX_LENGTH_FILE=0

# 启用 CLI 日志（需要 DEBUG 级别）
export MULTICLI_LOG_CLI=DEBUG
```

### 配置文件

创建 `.multicli/logging.json`：

```json
{
  "level": "INFO",
  "categories": {
    "CLI": "DEBUG"
  },
  "file": {
    "enabled": true,
    "path": ".multicli/logs",
    "maxSize": 10485760,
    "maxFiles": 5
  },
  "cli": {
    "logMessages": true,
    "logResponses": true,
    "maxLength": 500,
    "maxLengthFile": 0
  }
}
```

---

## 实际应用场景

### 场景 1: Worker 执行任务

```typescript
class WorkerExecutor {
  private conversationIndex = 0;

  async executeSubTask(subtask: SubTask): Promise<void> {
    const sessionId = this.session.id;
    const taskId = subtask.parentTaskId;
    const subTaskId = subtask.id;

    // 准备 prompt
    const originalPrompt = subtask.prompt;
    const guidancePrompt = this.injectGuidance(originalPrompt);

    // 记录发送
    logger.logCLIMessage({
      cli: this.cli,
      role: 'worker',
      requestId: `${subTaskId}-${Date.now()}`,
      message: originalPrompt,
      processedMessage: guidancePrompt,
      conversationContext: {
        sessionId,
        taskId,
        subTaskId,
        messageIndex: this.conversationIndex++,
        totalMessages: this.estimateTotal()
      },
      metadata: {
        taskId,
        subTaskId,
        hasGuidance: true,
        guidanceLength: guidancePrompt.length - originalPrompt.length
      }
    });

    // 发送并接收
    const startTime = Date.now();
    const response = await this.sendMessage(guidancePrompt);

    // 处理响应
    const parsedResult = this.parseResponse(response.content);

    // 记录接收
    logger.logCLIResponse({
      cli: this.cli,
      role: 'worker',
      requestId: `${subTaskId}-${Date.now()}`,
      response: response.content,
      processedResponse: JSON.stringify(parsedResult),
      duration: Date.now() - startTime,
      conversationContext: {
        sessionId,
        taskId,
        subTaskId,
        messageIndex: this.conversationIndex++,
        totalMessages: this.estimateTotal()
      },
      metadata: {
        taskId,
        subTaskId,
        success: parsedResult.success,
        hasToolCalls: parsedResult.toolCalls?.length > 0
      }
    });
  }
}
```

### 场景 2: Orchestrator 澄清需求

```typescript
class OrchestratorAgent {
  async clarifyRequirement(userPrompt: string): Promise<void> {
    const sessionId = this.session.id;
    const requestId = `clarify-${Date.now()}`;

    // 构建澄清问题
    const originalQuestion = this.generateClarificationQuestion(userPrompt);
    const formattedQuestion = this.formatForUser(originalQuestion);

    // 记录发送给用户
    logger.logCLIMessage({
      cli: 'user',
      role: 'orchestrator',
      requestId,
      message: originalQuestion,
      processedMessage: formattedQuestion,
      conversationContext: {
        sessionId,
        messageIndex: this.messageIndex++,
        totalMessages: this.estimateTotal()
      },
      metadata: {
        type: 'clarification',
        reason: 'vague_requirement'
      }
    });

    // 等待用户回答
    const userAnswer = await this.waitForUserInput();

    // 记录用户回答
    logger.logCLIResponse({
      cli: 'user',
      role: 'orchestrator',
      requestId,
      response: userAnswer,
      duration: Date.now() - startTime,
      conversationContext: {
        sessionId,
        messageIndex: this.messageIndex++,
        totalMessages: this.estimateTotal()
      },
      metadata: {
        type: 'clarification_response',
        satisfied: this.isSatisfied(userAnswer)
      }
    });
  }
}
```

---

## 日志分析和纠错

### 查找特定对话

```bash
# 查找特定会话的所有消息
grep '"sessionId":"session-abc"' .multicli/logs/*.log

# 查找特定任务的消息
grep '"taskId":"task-456"' .multicli/logs/*.log

# 查找特定请求的消息对
grep '"requestId":"req-123"' .multicli/logs/*.log
```

### 对比原始和处理后的内容

```bash
# 提取原始内容
jq -r 'select(.requestId=="req-123") | .content' .multicli/logs/*.log

# 提取处理后的内容
jq -r 'select(.requestId=="req-123") | .processedContent' .multicli/logs/*.log

# 对比差异
diff <(jq -r 'select(.requestId=="req-123" and .direction=="send") | .content' .multicli/logs/*.log) \
     <(jq -r 'select(.requestId=="req-123" and .direction=="send") | .processedContent' .multicli/logs/*.log)
```

### 追踪对话流程

```bash
# 按时间顺序查看会话的所有消息
jq -r 'select(.sessionId=="session-abc") | "\(.timestamp) [\(.direction)] \(.cli): \(.content[0:100])"' \
  .multicli/logs/*.log | sort
```

### 统计分析

```bash
# 统计每个 CLI 的消息数量
jq -r 'select(.type=="cli-message") | .cli' .multicli/logs/*.log | sort | uniq -c

# 统计平均响应时间
jq -r 'select(.direction=="receive") | .duration' .multicli/logs/*.log | \
  awk '{sum+=$1; count++} END {print sum/count}'

# 查找长消息（可能被截断）
jq -r 'select(.contentLength > 5000) | "\(.timestamp) \(.cli) \(.contentLength)"' \
  .multicli/logs/*.log
```

---

## 最佳实践

### 1. 始终记录对话上下文

```typescript
// ✅ 推荐
logger.logCLIMessage({
  cli: 'claude',
  role: 'worker',
  requestId: requestId,
  message: prompt,
  conversationContext: {
    sessionId: session.id,
    taskId: task.id,
    messageIndex: index
  }
});

// ❌ 不推荐
logger.logCLIMessage({
  cli: 'claude',
  role: 'worker',
  requestId: requestId,
  message: prompt
});
```

### 2. 记录格式处理

```typescript
// ✅ 推荐 - 记录处理前后
const formatted = formatPrompt(original);
logger.logCLIMessage({
  message: original,
  processedMessage: formatted,
  // ...
});

// ❌ 不推荐 - 只记录处理后的
logger.logCLIMessage({
  message: formatted,
  // ...
});
```

### 3. 使用有意义的 requestId

```typescript
// ✅ 推荐 - 包含上下文信息
const requestId = `${taskId}-${subTaskId}-${Date.now()}`;

// ❌ 不推荐 - 无意义的随机 ID
const requestId = Math.random().toString();
```

### 4. 启用文件日志

```typescript
// ✅ 推荐 - 生产环境启用文件日志
export MULTICLI_LOG_FILE=.multicli/logs/app.log
export MULTICLI_LOG_CLI=DEBUG

// ❌ 不推荐 - 只依赖控制台输出
```

---

## 故障排查

### 问题：文件日志没有保存完整内容

**检查**：
```bash
# 检查配置
echo $MULTICLI_CLI_MAX_LENGTH_FILE

# 应该是 0（不限制）或足够大的值
```

**解决**：
```bash
export MULTICLI_CLI_MAX_LENGTH_FILE=0  # 不限制
```

### 问题：看不到 CLI 消息日志

**检查**：
```bash
# 检查 CLI 分类的日志级别
echo $MULTICLI_LOG_CLI

# 应该是 DEBUG
```

**解决**：
```bash
export MULTICLI_LOG_CLI=DEBUG
```

### 问题：日志文件太大

**解决**：
```bash
# 调整文件大小和数量
export MULTICLI_LOG_FILE_MAX_SIZE=10485760  # 10MB
export MULTICLI_LOG_FILE_MAX_FILES=5        # 保留 5 个文件
```

---

## 总结

增强后的 CLI 消息日志系统提供了：

✅ **完整性** - 文件日志保存完整的原始内容
✅ **可追溯** - 对话上下文完整记录
✅ **可对比** - 记录处理前后的内容
✅ **易调试** - 控制台友好显示
✅ **可分析** - JSON 格式便于查询和统计

这为后续的问题排查、纠错和系统优化提供了坚实的基础。
