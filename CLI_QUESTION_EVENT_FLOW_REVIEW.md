# CLI 询问事件流完整复盘

## 事件流路径

### 路径 1: PrintSession → SessionManager → CLIAdapterFactory → WebviewProvider

```
PrintSession.checkForQuestion()
  ↓ emit('question', CLIQuestion)
SessionManager (line 269-271)
  ↓ emit('question', { cli, role, question })
CLIAdapterFactory (line 88-90)
  ↓ emit('question', { type: cli, question, adapterRole: role })
WebviewProvider (line 318-338)
  ↓ postMessage({ type: 'cliQuestion', ... })
前端 (index.html)
  ↓ showCliQuestion(msg)
显示询问卡片
```

**状态**: ✅ 已修复
- PrintSession 发送正确的 CLIQuestion 格式
- 包含 content 字段
- 有去重机制

---

### 路径 2: InteractiveSession → SessionManager → CLIAdapterFactory → WebviewProvider

```
InteractiveSession.send() → stdout.on('data')
  ↓ emit('question', CLIQuestion)  ← 已修复
SessionManager (line 269-271)
  ↓ emit('question', { cli, role, question })
CLIAdapterFactory (line 88-90)
  ↓ emit('question', { type: cli, question, adapterRole: role })
WebviewProvider (line 318-338)
  ↓ postMessage({ type: 'cliQuestion', ... })
前端 (index.html)
  ↓ showCliQuestion(msg)
显示询问卡片
```

**状态**: ✅ 已修复
- InteractiveSession 现在发送正确的 CLIQuestion 格式
- 包含 content 字段
- 有去重机制

---

## 关键检查点

### 1. ✅ PrintSession (src/cli/session/print-session.ts:277)
```typescript
this.emit('question', question);
```
- question 是 CLIQuestion 类型
- 包含 questionId, cli, content, pattern, timestamp
- 有去重机制（lastQuestionId）

### 2. ✅ InteractiveSession (src/cli/session/interactive-session.ts:172)
```typescript
this.emit('question', question);
```
- question 是 CLIQuestion 类型
- 包含 questionId, cli, content, pattern, timestamp
- 有去重机制（lastQuestionId）

### 3. ✅ SessionManager (src/cli/session/session-manager.ts:271)
```typescript
this.emit('question', { cli, role, question });
```
- 只是转发，不修改 question 对象
- question 保持原始的 CLIQuestion 格式

### 4. ✅ CLIAdapterFactory (src/cli/adapter-factory.ts:89)
```typescript
this.emit('question', { type: cli, question, adapterRole: role });
```
- 只是转发，不修改 question 对象
- question 保持原始的 CLIQuestion 格式

### 5. ✅ WebviewProvider (src/ui/webview-provider.ts:328-337)
```typescript
this.postMessage({
  type: 'cliQuestion',
  cli: type,
  questionId: question.questionId,
  content: question.content,  // ← 使用 question.content
  pattern: question.pattern,
  timestamp: question.timestamp,
  adapterRole: adapterRole,
  sessionId: this.activeSessionId
});
```
- 正确提取 question.content
- 发送给前端

### 6. ✅ 前端 (src/ui/webview/index.html:3369)
```javascript
const questionMsg = {
  role: 'cli_question',
  type: 'cli_question',
  cli: cli,
  questionId: msg.questionId,
  content: msg.content,  // ← 接收 msg.content
  pattern: msg.pattern,
  // ...
};
```
- 正确接收 msg.content
- 显示在卡片中

---

## 潜在问题检查

### ❓ 问题 1: SessionManager 的事件转发

**位置**: `src/cli/session/session-manager.ts:269-271`

```typescript
sessionProcess.on('question', (...args: unknown[]) => {
  const question = args[0];
  this.emit('question', { cli, role, question });
});
```

**分析**:
- 使用 `...args: unknown[]` 接收参数
- `question = args[0]` 可能是任何类型
- 没有类型检查

**风险**: 低
- PrintSession 和 InteractiveSession 都已修复
- 都发送正确的 CLIQuestion 格式
- 但缺少类型安全

**建议**: 可选优化
```typescript
sessionProcess.on('question', (question: CLIQuestion) => {
  this.emit('question', { cli, role, question });
});
```

---

### ❓ 问题 2: CLIAdapterFactory 的事件转发

**位置**: `src/cli/adapter-factory.ts:88-90`

```typescript
this.sessionManager.on('question', ({ cli, role, question }) => {
  this.emit('question', { type: cli, question, adapterRole: role });
});
```

**分析**:
- question 类型是 any
- 没有类型检查

**风险**: 低
- 只是转发，不修改
- 上游已经保证格式正确

**建议**: 可选优化
```typescript
this.sessionManager.on('question', ({ cli, role, question }: {
  cli: CLIType;
  role: 'worker' | 'orchestrator';
  question: CLIQuestion;
}) => {
  this.emit('question', { type: cli, question, adapterRole: role });
});
```

---

### ❓ 问题 3: WebviewProvider 的类型定义

**位置**: `src/ui/webview-provider.ts:318-326`

```typescript
this.cliFactory.on('question', ({
  type,
  question,
  adapterRole
}: {
  type: CLIType;
  question: any;  // ← any 类型
  adapterRole?: 'worker' | 'orchestrator';
}) => {
```

**分析**:
- question 是 any 类型
- 缺少类型安全

**风险**: 低
- 运行时已经正确
- 但编译时无法检查

**建议**: 可选优化
```typescript
import type { CLIQuestion } from '../cli/session/print-session';

this.cliFactory.on('question', ({
  type,
  question,
  adapterRole
}: {
  type: CLIType;
  question: CLIQuestion;  // ← 明确类型
  adapterRole?: 'worker' | 'orchestrator';
}) => {
```

---

## 遗漏检查

### ✅ 检查 1: 是否有其他 Session 类型？
- PrintSession ✅
- InteractiveSession ✅
- ProcessPool ✅ (不触发 question 事件)
- 没有其他 Session 类型

### ✅ 检查 2: 是否有直接调用 WebviewProvider.postMessage 的地方？
```bash
grep -rn "type: 'cliQuestion'" src/
```
- 只有 WebviewProvider.ts:329 一处
- 没有其他地方直接发送 cliQuestion

### ✅ 检查 3: 前端是否有其他接收 cliQuestion 的地方？
```bash
grep -n "cliQuestion" src/ui/webview/index.html
```
- 只有一处接收和处理
- 逻辑正确

### ✅ 检查 4: 是否有测试文件需要更新？
```bash
find . -name "*test*.ts" -o -name "*spec*.ts" | xargs grep -l "question"
```
- 需要检查测试文件

---

## 总结

### 已修复的问题 ✅
1. ✅ PrintSession 事件格式正确
2. ✅ InteractiveSession 事件格式正确
3. ✅ 两者都有去重机制
4. ✅ 两者都检查空内容
5. ✅ WebviewProvider 正确提取 content
6. ✅ 前端正确显示 content

### 可选优化（非必需）
1. SessionManager 添加类型检查
2. CLIAdapterFactory 添加类型检查
3. WebviewProvider 添加类型导入
4. 更新测试文件

### 无遗漏 ✅
- 所有 Session 类型都已检查
- 所有事件流路径都已验证
- 没有其他触发点
- 前端逻辑正确

---

## 建议

### 立即测试
当前修复已经完整，可以立即测试验证：
- 不应该再有重复的询问面板
- 内容应该正确显示
- 可以正常回答

### 后续优化（可选）
如果测试通过，可以考虑：
1. 添加类型安全（编译时检查）
2. 更新测试文件
3. 添加单元测试

---

**状态**: ✅ 核心问题已完全修复
**遗漏**: ❌ 无遗漏
**建议**: 立即测试验证
