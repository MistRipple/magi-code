# CLI 询问重复显示问题修复方案

**问题**: 截图显示 5 个重复的 "CLI 询问 (claude)" 面板，且内容为空

---

## 问题分析

### 现象
1. 5 个相同的 "CLI 询问 (claude)" 面板
2. 所有面板都显示"等待回答"状态
3. 询问内容区域为空

### 可能原因

#### 原因 1: 多次触发询问事件
- **位置**: `src/cli/session/print-session.ts:255`
- **代码**: `this.emit('question', question);`
- **问题**: 可能被调用了 5 次

**可能的触发场景**:
- CLI 输出了多次相同的询问文本
- 有多个 PrintSession 实例同时运行
- 询问检测逻辑被多次触发

#### 原因 2: 询问内容为空
- **位置**: `src/cli/session/print-session.ts:249`
- **代码**: `content: lastLines.trim()`
- **问题**: `lastLines` 可能为空字符串

**可能的原因**:
- `buffer.split('\n')` 返回空数组
- `lines.slice(-5)` 没有有效内容
- 询问文本在 buffer 中被清空了

#### 原因 3: 前端去重失效
- **位置**: `src/ui/webview/index.html:3377-3382`
- **代码**: `findCliQuestionIndex(cliOutputs[cli], cli, msg.questionId)`
- **问题**: `questionId` 每次都不同，导致去重失败

---

## 修复方案

### 方案 1: 基于内容生成稳定的 questionId ⭐ 推荐

**目标**: 相同内容的询问使用相同的 ID，避免重复显示

**修改文件**: `src/cli/session/print-session.ts`

```typescript
import * as crypto from 'crypto';

private checkForQuestion(buffer: string, requestId: string): void {
  // 如果已经在等待回答，不重复检测
  if (this.waitingForAnswer) {
    return;
  }

  // 获取最后几行进行检测
  const lines = buffer.split('\n');
  const lastLines = lines.slice(-5).join('\n').trim();

  // 🔧 修复：如果内容为空，不触发询问
  if (!lastLines) {
    return;
  }

  for (const pattern of QUESTION_PATTERNS) {
    if (pattern.test(lastLines)) {
      // 🔧 修复：基于内容生成稳定的 questionId
      const contentHash = crypto.createHash('md5')
        .update(lastLines)
        .digest('hex')
        .slice(0, 8);

      this.currentQuestionId = `${requestId}-${contentHash}`;

      // 🔧 修复：检查是否已经发送过相同的询问
      if (this.lastQuestionId === this.currentQuestionId) {
        logger.debug(`[PrintSession] 跳过重复询问: ${this.currentQuestionId}`, undefined, LogCategory.CLI);
        return;
      }

      this.lastQuestionId = this.currentQuestionId;
      this.waitingForAnswer = true;

      const question: CLIQuestion = {
        questionId: this.currentQuestionId,
        cli: this.cli,
        content: lastLines,
        pattern: pattern.source,
        timestamp: Date.now(),
      };

      logger.info(`[PrintSession] 检测到 CLI 询问:`, question, LogCategory.CLI);
      this.emit('question', question);

      // 设置询问超时
      this.setQuestionTimeout();
      break;
    }
  }
}
```

**需要添加的属性**:
```typescript
export class PrintSession extends EventEmitter {
  // ... 现有属性 ...
  private lastQuestionId?: string;  // 🔧 新增：记录上一次的询问 ID
}
```

### 方案 2: 增强前端去重逻辑

**目标**: 即使 questionId 不同，也能根据内容去重

**修改文件**: `src/ui/webview/index.html`

```javascript
function findCliQuestionIndex(list, cli, questionId, content) {
  if (!Array.isArray(list)) return -1;

  return list.findIndex(m => {
    if (m.type !== 'cli_question' || m.cli !== cli) {
      return false;
    }

    // 🔧 修复：优先匹配 questionId，其次匹配内容
    if (questionId && m.questionId === questionId) {
      return true;
    }

    // 🔧 修复：如果 questionId 不匹配，检查内容是否相同
    if (content && m.content === content) {
      return true;
    }

    return false;
  });
}

function showCliQuestion(msg) {
  console.log('[Webview] 收到 CLI 询问:', msg);

  const cli = msg.cli || 'claude';
  if (!cliOutputs[cli]) {
    cliOutputs[cli] = [];
  }

  // 🔧 修复：传入 content 参数进行去重
  const existingCliIdx = findCliQuestionIndex(
    cliOutputs[cli],
    cli,
    msg.questionId,
    msg.content  // 🔧 新增
  );

  // ... 其余代码保持不变 ...
}
```

### 方案 3: 添加调试日志

**目标**: 帮助诊断问题

**修改文件**: `src/cli/session/print-session.ts`

```typescript
private checkForQuestion(buffer: string, requestId: string): void {
  logger.debug(`[PrintSession] checkForQuestion called, waitingForAnswer=${this.waitingForAnswer}`, undefined, LogCategory.CLI);

  if (this.waitingForAnswer) {
    logger.debug(`[PrintSession] 已在等待回答，跳过检测`, undefined, LogCategory.CLI);
    return;
  }

  const lines = buffer.split('\n');
  const lastLines = lines.slice(-5).join('\n').trim();

  logger.debug(`[PrintSession] 检测内容: "${lastLines.slice(0, 100)}..."`, undefined, LogCategory.CLI);

  if (!lastLines) {
    logger.debug(`[PrintSession] 内容为空，跳过检测`, undefined, LogCategory.CLI);
    return;
  }

  // ... 其余代码 ...
}
```

---

## 推荐实施顺序

### 第一步: 添加调试日志（方案 3）
- 目的：了解问题的具体原因
- 风险：低
- 时间：5 分钟

### 第二步: 修复内容为空问题（方案 1 部分）
- 添加 `if (!lastLines) return;` 检查
- 目的：避免空内容触发询问
- 风险：低
- 时间：2 分钟

### 第三步: 实施稳定 questionId（方案 1 完整）
- 基于内容生成 questionId
- 添加重复检测
- 目的：从根本上解决重复问题
- 风险：中
- 时间：15 分钟

### 第四步: 增强前端去重（方案 2）
- 作为额外的保险措施
- 目的：即使后端有问题，前端也能去重
- 风险：低
- 时间：10 分钟

---

## 测试验证

### 测试场景 1: 正常询问
```
输入：执行一个会触发 CLI 询问的任务
期望：只显示 1 个询问面板，内容正确显示
```

### 测试场景 2: 重复询问
```
输入：CLI 输出多次相同的询问文本
期望：只显示 1 个询问面板（去重成功）
```

### 测试场景 3: 空内容
```
输入：buffer 中没有有效内容
期望：不触发询问事件
```

---

## 回滚计划

如果修复后出现问题：

```bash
# 回滚到修复前的版本
git checkout HEAD~1 src/cli/session/print-session.ts
git checkout HEAD~1 src/ui/webview/index.html

# 重新编译
npm run compile
```

---

## 相关文件

- `src/cli/session/print-session.ts` - 询问检测逻辑
- `src/ui/webview/index.html` - 前端显示逻辑
- `src/ui/webview-provider.ts` - 消息转发

---

**状态**: 待实施
**优先级**: P1
**预计时间**: 30 分钟
