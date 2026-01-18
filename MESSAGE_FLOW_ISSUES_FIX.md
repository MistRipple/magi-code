# 消息流问题完整修复方案

**日期**: 2026-01-18
**状态**: 待实施

---

## 发现的问题

### 🔴 问题 1: interaction 类型消息未被前端处理（严重）

**影响**: CLI 询问无法正确显示为交互式卡片

**事件流**:
```
PrintSession/InteractiveSession → emit('question', CLIQuestion)
    ↓
SessionManager → emit('question', { cli, role, question })
    ↓
CLIAdapterFactory → createInteractionMessage() → emit('standardMessage')
    ↓
WebviewProvider → postMessage({ type: 'standardMessage', message })
    ↓
Frontend handleStandardMessage(message)
    ↓ ❌ 没有检查 message.type === 'interaction'
standardToWebviewMessage() → 当作普通消息渲染
```

### 🟡 问题 2: 前端存在死代码

**死代码列表**:
- `msg.type === 'cliQuestion'` 处理分支 (line 3006)
- `msg.type === 'cliQuestionTimeout'` 处理分支 (line 3015)
- `msg.type === 'cliQuestionAnswered'` 处理分支 (line 3024)
- `showCliQuestion()` 函数 (line 3349)
- `handleCliQuestionTimeout()` 函数
- `findCliQuestionIndex()` 函数

### 🟡 问题 3: 类型安全缺失

**位置**:
- `session-manager.ts:269` - `question` 类型是 `unknown`
- `adapter-factory.ts:93` - `question` 类型是隐式 `any`

---

## 修复方案

### 修复 1: 前端添加 interaction 消息处理

**文件**: `src/ui/webview/index.html`

**修改点 1**: 在 `handleStandardMessage` 中添加 interaction 检查

```javascript
function handleStandardMessage(message) {
  if (!message || !message.id) {
    console.warn('[Webview] 收到无效的标准消息:', message);
    return;
  }

  console.log('[Webview] 收到标准消息:', message.id, message.type, message.lifecycle);

  // 🆕 处理交互消息（CLI 询问）
  if (message.type === 'interaction' && message.interaction) {
    handleInteractionMessage(message);
    return;
  }

  // 过滤编排者内部 JSON 分析输出
  if (message.source === 'orchestrator') {
    const textContent = extractTextFromBlocks(message.blocks || []);
    if (isInternalJsonMessage(textContent) && message.type !== 'plan') {
      return;
    }
  }

  // ... 其余代码保持不变 ...
}
```

**修改点 2**: 添加新的 `handleInteractionMessage` 函数

```javascript
/**
 * 处理交互消息（CLI 询问）
 */
function handleInteractionMessage(message) {
  const interaction = message.interaction;
  const cli = message.cli || 'claude';

  console.log('[Webview] 收到交互消息:', interaction.type, interaction.requestId);

  // 只处理 QUESTION 类型的交互
  if (interaction.type !== 'question') {
    console.warn('[Webview] 不支持的交互类型:', interaction.type);
    return;
  }

  // 确保 CLI 输出数组存在
  if (!cliOutputs[cli]) {
    cliOutputs[cli] = [];
  }

  // 创建询问消息
  const questionMsg = {
    role: 'cli_question',
    type: 'cli_question',
    cli: cli,
    questionId: interaction.requestId,
    content: interaction.prompt,
    pattern: message.metadata?.questionPattern || 'interaction',
    time: new Date(message.timestamp).toLocaleTimeString().slice(0, 5),
    timestamp: message.timestamp,
    isPending: true,
    adapterRole: message.metadata?.adapterRole,
    standardMessageId: message.id,
    traceId: message.traceId
  };

  // 检查是否已存在（去重）
  const existingIdx = cliOutputs[cli].findIndex(m =>
    m.type === 'cli_question' && m.questionId === interaction.requestId
  );

  if (existingIdx !== -1) {
    // 更新现有消息
    cliOutputs[cli][existingIdx] = { ...cliOutputs[cli][existingIdx], ...questionMsg };
  } else {
    // 添加新消息
    cliOutputs[cli].push(questionMsg);
  }

  // 如果是 orchestrator 角色，也在 Thread 面板显示
  if (message.metadata?.adapterRole === 'orchestrator') {
    const threadQuestionMsg = {
      ...questionMsg,
      source: 'orchestrator'
    };

    const existingThreadIdx = threadMessages.findIndex(m =>
      m.type === 'cli_question' && m.questionId === interaction.requestId
    );

    if (existingThreadIdx !== -1) {
      threadMessages[existingThreadIdx] = { ...threadMessages[existingThreadIdx], ...threadQuestionMsg };
    } else {
      threadMessages.push(threadQuestionMsg);
    }
  }

  saveWebviewState();
  renderMainContent();
  smoothScrollToBottom();
}
```

### 修复 2: 删除死代码

**文件**: `src/ui/webview/index.html`

**删除以下代码块**:

1. **删除 cliQuestion 事件处理** (约 line 3006-3013):
```javascript
// 删除整个 else if 块
else if (msg.type === 'cliQuestion') {
  if (msg.sessionId && msg.sessionId !== currentSessionId) {
    console.log('[Webview] 忽略非当前会话的 cliQuestion');
    return;
  }
  showCliQuestion(msg);
}
```

2. **删除 cliQuestionTimeout 事件处理** (约 line 3015-3022):
```javascript
// 删除整个 else if 块
else if (msg.type === 'cliQuestionTimeout') {
  if (msg.sessionId && msg.sessionId !== currentSessionId) {
    console.log('[Webview] 忽略非当前会话的 cliQuestionTimeout');
    return;
  }
  handleCliQuestionTimeout(msg);
}
```

3. **删除 cliQuestionAnswered 事件处理** (约 line 3024-3031):
```javascript
// 删除整个 else if 块
else if (msg.type === 'cliQuestionAnswered') {
  if (msg.sessionId && msg.sessionId !== currentSessionId) {
    console.log('[Webview] 忽略非当前会话的 cliQuestionAnswered');
    return;
  }
  handleCliQuestionAnswered(msg);
}
```

4. **删除 showCliQuestion 函数** (约 line 3349-3400):
```javascript
// 删除整个函数
function showCliQuestion(msg) {
  // ... 整个函数体 ...
}
```

5. **删除 handleCliQuestionTimeout 函数**:
```javascript
// 删除整个函数
function handleCliQuestionTimeout(msg) {
  // ... 整个函数体 ...
}
```

6. **删除 findCliQuestionIndex 函数**:
```javascript
// 删除整个函数
function findCliQuestionIndex(list, cli, questionId) {
  // ... 整个函数体 ...
}
```

### 修复 3: 添加类型安全

**文件 1**: `src/cli/session/session-manager.ts`

**修改** (line 269-271):
```typescript
// ❌ 修改前
sessionProcess.on('question', (...args: unknown[]) => {
  const question = args[0];
  this.emit('question', { cli, role, question });
});

// ✅ 修改后
import type { CLIQuestion } from './print-session';

sessionProcess.on('question', (question: CLIQuestion) => {
  this.emit('question', { cli, role, question });
});
```

**文件 2**: `src/cli/adapter-factory.ts`

**修改** (line 93):
```typescript
// ❌ 修改前
this.sessionManager.on('question', ({ cli, role, question }) => {
  const message = createInteractionMessage(
    // ...
  );
});

// ✅ 修改后
import type { CLIQuestion } from './session/print-session';

this.sessionManager.on('question', ({
  cli,
  role,
  question
}: {
  cli: CLIType;
  role: 'worker' | 'orchestrator';
  question: CLIQuestion;
}) => {
  const message = createInteractionMessage(
    // ...
  );
});
```

---

## 实施步骤

### 步骤 1: 修复前端 interaction 处理
1. 在 `handleStandardMessage` 中添加 interaction 检查
2. 添加 `handleInteractionMessage` 函数
3. 测试 CLI 询问是否正确显示

### 步骤 2: 删除死代码
1. 删除 `cliQuestion` 相关事件处理
2. 删除 `showCliQuestion` 等函数
3. 验证没有其他地方引用这些函数

### 步骤 3: 添加类型安全
1. 修改 SessionManager 的事件监听
2. 修改 CLIAdapterFactory 的事件监听
3. 运行 TypeScript 编译验证

### 步骤 4: 测试验证
1. 触发 CLI 询问
2. 验证询问卡片正确显示
3. 验证可以正常回答
4. 验证超时处理正常

---

## 预期效果

### 修复前
- ❌ CLI 询问显示为普通文本消息
- ❌ 无法交互回答
- ❌ 存在大量死代码
- ❌ 类型安全缺失

### 修复后
- ✅ CLI 询问显示为交互式卡片
- ✅ 可以正常回答
- ✅ 代码简洁，无死代码
- ✅ 类型安全完整

---

## 风险评估

### 低风险
- 删除死代码：这些代码已经不被调用
- 添加类型：不影响运行时行为

### 中风险
- 修改 interaction 处理：需要充分测试

### 缓解措施
- 分步实施，每步测试
- 保留 Git 提交记录，便于回滚
- 先在开发环境测试

---

**状态**: 待实施
**优先级**: P0（严重影响功能）
