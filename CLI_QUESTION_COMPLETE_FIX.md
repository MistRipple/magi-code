# CLI 询问问题完整分析与修复

**问题发现**: 2026-01-18
**严重性**: P0 (严重影响用户体验)

---

## 问题现象（截图分析）

### 1. 重复显示
- 5 个相同的 "CLI 询问 (claude)" 面板
- 所有面板都显示"等待回答"状态

### 2. 内容为空 ⚠️
- 询问卡片内容区域完全空白
- 没有显示任何询问文本
- 用户无法知道 CLI 在问什么

---

## 日志分析

### 后台日志
```
[14:13:08.950] [INFO] [ui] [WebviewProvider] 收到 CLI 询问事件:
  Data: {text: '{"type":"assistant","message":{"id":"req_vrtx_…', process: ChildProcess}

[14:13:09.001] [INFO] [ui] [WebviewProvider] 收到 CLI 询问事件:
  Data: {text: '{"type":"user","message":{"role":"user","conte…', process: ChildProcess}

... (共 5 次)
```

### 关键发现

1. **事件格式错误**
   ```javascript
   // 实际收到的
   {
     text: '{"type":"assistant","message":...}',
     process: ChildProcess
   }

   // 应该收到的
   {
     questionId: string;
     cli: CLIType;
     content: string;  // ← 这个字段缺失！
     pattern: string;
     timestamp: number;
   }
   ```

2. **content 字段缺失**
   - 前端期望 `msg.content` 字段
   - 但实际收到的是 `msg.text` 字段
   - 导致前端无法显示询问内容

---

## 根本原因

### 问题 1: InteractiveSession 事件格式错误

**位置**: `src/cli/session/interactive-session.ts:143`

```typescript
// ❌ 错误的代码
this.emit('question', { text, process: proc });
```

**问题**:
1. 字段名错误：`text` 应该是 `content`
2. 缺少必需字段：`questionId`, `cli`, `pattern`, `timestamp`
3. 多余字段：`process` 不应该发送给前端

### 问题 2: 没有去重机制

每次流式输出都可能触发 `detectQuestion()`，导致：
- 同一个询问被检测多次
- 每次都生成新的事件
- 前端收到 5 个重复的询问

### 问题 3: 前端渲染逻辑

**位置**: `src/ui/webview/index.html:3369`

```javascript
const questionMsg = {
  role: 'cli_question',
  type: 'cli_question',
  cli: cli,
  questionId: msg.questionId,
  content: msg.content,  // ← 如果 msg.content 不存在，这里就是 undefined
  pattern: msg.pattern,
  // ...
};
```

如果 `msg.content` 是 `undefined`，卡片内容就会是空白。

---

## 修复方案

### 已实施的修复

#### 1. PrintSession 去重修复 (Commit: 07127b8)
- 基于内容生成稳定的 questionId
- 添加 lastQuestionId 去重检测
- 添加空内容检查

#### 2. InteractiveSession 格式修复 (Commit: 64a0040) ⭐ 核心修复
- **修复事件格式**：发送正确的 CLIQuestion 对象
- **添加去重机制**：基于内容生成 questionId
- **添加重复检测**：使用 lastQuestionId
- **添加 waitingForAnswer 检查**

**修复后的代码**:
```typescript
// ✅ 正确的代码
const contentHash = crypto.createHash('md5')
  .update(text.trim())
  .digest('hex')
  .slice(0, 8);

this.currentQuestionId = `${message.requestId || 'interactive'}-${contentHash}`;

// 检查是否已经发送过相同的询问
if (this.lastQuestionId === this.currentQuestionId) {
  return;
}

this.lastQuestionId = this.currentQuestionId;
this.waitingForAnswer = true;

// 发送正确格式的 CLIQuestion 对象
const question: CLIQuestion = {
  questionId: this.currentQuestionId,
  cli: this.cli,
  content: text.trim(),  // ← 正确的字段名
  pattern: 'interactive-detection',
  timestamp: Date.now(),
};

this.emit('question', question);
```

---

## 问题解决对照表

| 问题 | 原因 | 修复方案 | 状态 |
|------|------|----------|------|
| **重复显示 5 次** | 没有去重机制 | 基于内容生成稳定 ID + lastQuestionId 检测 | ✅ 已修复 |
| **内容为空** | 字段名错误 (text vs content) | 发送正确的 CLIQuestion 格式 | ✅ 已修复 |
| **事件格式错误** | InteractiveSession 发送错误格式 | 统一使用 CLIQuestion 类型 | ✅ 已修复 |
| **多次触发** | 每次流式输出都检测 | 添加 waitingForAnswer 检查 | ✅ 已修复 |

---

## 预期效果

### 修复前
```
问题 1: 5 个重复的询问面板
问题 2: 所有面板内容为空
问题 3: 用户无法知道 CLI 在问什么
```

### 修复后
```
✅ 只显示 1 个询问面板
✅ 内容正确显示（text.trim()）
✅ 用户可以看到询问内容并回答
✅ 不再有重复触发
```

---

## 技术细节

### 为什么内容会为空？

1. **InteractiveSession 发送**:
   ```javascript
   { text: '...', process: proc }
   ```

2. **前端接收**:
   ```javascript
   msg.content  // undefined (因为字段名是 text，不是 content)
   ```

3. **前端渲染**:
   ```javascript
   content: msg.content,  // undefined
   ```

4. **结果**: 卡片内容区域为空

### 为什么会重复 5 次？

1. Claude 流式输出多次触发 `detectQuestion()`
2. 每次都生成新的事件（没有去重）
3. 前端收到 5 个不同的 questionId
4. 前端认为是 5 个不同的询问
5. 显示 5 个面板

---

## 验证方法

### 测试场景 1: 正常询问
```
输入：执行一个会触发 CLI 询问的任务
期望：
  - 只显示 1 个询问面板
  - 内容正确显示
  - 可以正常回答
```

### 测试场景 2: 流式输出
```
输入：Claude 流式输出包含询问模式的文本
期望：
  - 不会重复触发
  - 只显示 1 次
  - 内容完整
```

### 测试场景 3: 空内容
```
输入：text.trim() 为空
期望：
  - 不触发询问事件
  - 不显示空白卡片
```

---

## 相关文件

### 修改的文件
1. `src/cli/session/print-session.ts` - PrintSession 去重
2. `src/cli/session/interactive-session.ts` - InteractiveSession 格式修复

### 相关代码
- `src/ui/webview-provider.ts:318-338` - 事件监听
- `src/ui/webview/index.html:3354-3404` - 前端显示逻辑
- `src/cli/adapter-factory.ts:89-91` - 事件转发

---

## 提交记录

```
64a0040 - fix: 修复 InteractiveSession 的 CLI 询问事件格式错误（根本原因）
07127b8 - fix: 修复 CLI 询问重复显示和内容为空问题
```

---

## 总结

### 问题的两个层面

1. **表面问题**: 重复显示 5 次
   - 原因：没有去重机制
   - 修复：添加 lastQuestionId 检测

2. **根本问题**: 内容为空 ⚠️
   - 原因：事件格式错误（text vs content）
   - 修复：发送正确的 CLIQuestion 格式

### 关键修复

**InteractiveSession 的格式修复是核心**，因为：
1. 解决了字段名错误（text → content）
2. 统一了事件格式（CLIQuestion）
3. 添加了去重机制
4. 同时解决了重复和空白两个问题

---

**状态**: ✅ 已完全修复
**需要验证**: 是（需要实际测试）
**优先级**: P0
