# 消息渲染完整流程梳理

## 1. 后端消息生成流程

### Orchestrator 消息流程

```
用户输入
  ↓
OrchestratorAgent.execute()
  ↓
emitUIMessage(content, type)
  ↓
normalizeOrchestratorMessage()  [orchestrator-normalizer.ts]
  ↓
parseContentToBlocks(content)  [content-parser.ts]
  ↓
  ├─ extractEmbeddedJson() - 提取裸露 JSON
  ├─ 移除裸露 JSON
  ├─ extractCodeBlocks() - 提取代码块
  └─ parseTextContent() - 解析文本
  ↓
StandardMessage { blocks: ContentBlock[] }
  ↓
发送到前端
```

### Worker 消息流程

```
Worker 执行
  ↓
CLI 输出
  ↓
BaseNormalizer.parseChunk()
  ↓
buildFinalMessage()
  ↓
parseContentToBlocks(pendingText)  [content-parser.ts]
  ↓
StandardMessage { blocks: ContentBlock[] }
  ↓
发送到前端
```

## 2. ContentBlock 类型

```typescript
type ContentBlock =
  | TextBlock      // 文本内容
  | CodeBlock      // 代码块
  | ThinkingBlock  // 思考过程
  | ToolCallBlock  // 工具调用

interface TextBlock {
  type: 'text';
  content: string;
  isMarkdown?: boolean;  // 是否用 Markdown 渲染
  isJson?: boolean;      // 是否是 JSON（已废弃，不再使用）
}

interface CodeBlock {
  type: 'code';
  content: string;
  language: string;
  filename?: string;
}
```

## 3. 前端渲染流程

### 消息接收

```
WebviewProvider 接收消息
  ↓
postMessage({ type: 'standardMessage', message: StandardMessage })
  ↓
前端 index.html 接收
  ↓
handleStandardMessage()
  ↓
更新 messages 数组
  ↓
renderMessages()
```

### 渲染逻辑

```
renderMessages()
  ↓
renderMessageList()
  ↓
遍历 messages
  ↓
hasRenderableContent 检查
  ├─ streaming: true → 渲染
  ├─ content 有内容 → 渲染
  ├─ parsedBlocks 有内容 → 渲染
  ├─ toolCalls 有内容 → 渲染
  └─ isOrchestrator && standardMessageId → 渲染
  ↓
renderMessageBlock()
  ↓
  ├─ 渲染消息头部（角色、时间）
  ├─ 渲染 Thinking 块（如果有）
  ├─ 渲染消息内容
  │   ↓
  │   renderMessageContentSmart()
  │   ↓
  │   renderParsedBlocks(message.parsedBlocks)
  │   ↓
  │   遍历 blocks
  │   ↓
  │   switch (block.type)
  │     ├─ 'text' →
  │     │   ├─ isMarkdown → renderMarkdown()
  │     │   └─ plain → formatSimpleContent()
  │     ├─ 'code' → renderCodeBlock()
  │     ├─ 'thinking' → 跳过（已在头部渲染）
  │     └─ 'tool_call' → 跳过（在底部渲染）
  │
  ├─ 渲染工具调用（如果有）
  └─ 渲染流式动画（如果 streaming）
```

## 4. 关键检查点

### ✅ 检查点 1: Orchestrator 消息是否调用 parseContentToBlocks

**位置**: `src/normalizer/orchestrator-normalizer.ts` 行 45-46

```typescript
if (uiMessage.content) {
  blocks = parseContentToBlocks(uiMessage.content, { source: 'orchestrator' });
}
```

**状态**: ✅ 已修复

---

### ✅ 检查点 2: parseContentToBlocks 是否移除裸露 JSON

**位置**: `src/utils/content-parser.ts` 行 263-286

```typescript
// 2. 🔧 移除裸露的 JSON 对象（用户不需要看到原始 JSON）
const embeddedJsons = extractEmbeddedJson(content);
if (embeddedJsons.length > 0) {
  console.log('[content-parser] 发现裸露 JSON:', embeddedJsons.length, '个');
  // ... 移除逻辑
}
```

**状态**: ✅ 已实现

---

### ✅ 检查点 3: 前端是否移除了 JSON 检测

**位置**: `src/ui/webview/index.html` 行 4449-4458

```javascript
case 'text':
  // 🔧 移除前端 JSON 检测 - 后端已经处理
  if (block.isMarkdown) {
    html += '<div class="markdown-rendered">' + renderMarkdown(block.content) + '</div>';
  } else {
    html += formatSimpleContent(block.content);
  }
  break;
```

**状态**: ✅ 已修复

---

### ✅ 检查点 4: 特殊面板是否设置 collapsed: false

**位置**: `src/ui/webview/index.html`

- renderPlanPreviewCard (行 6610)
- renderPlanConfirmationCard (行 6642)
- renderQuestionCard (行 6684)
- renderCliQuestionCard (行 6752)
- renderSubTaskSummaryCard (行 3675)

**状态**: ✅ 已修复

---

### ✅ 检查点 5: 工具面板标签

**位置**: `src/ui/webview/index.html` 行 3731, 3734, 3737

```javascript
'IN' / 'OUT' / 'ERROR'
```

**状态**: ✅ 已修复

---

## 5. 潜在问题排查

### 问题 A: 为什么 JSON 还在显示？

**可能原因**:

1. **parseContentToBlocks 没有被调用**
   - 检查: Orchestrator 是否调用了 parseContentToBlocks
   - 状态: ✅ 已修复（行 46）

2. **extractEmbeddedJson 没有找到 JSON**
   - 检查: JSON 格式是否正确
   - 检查: 是否在代码块中（会被跳过）
   - 调试: 查看控制台日志

3. **JSON 被识别为代码块**
   - 检查: 是否有 ``` 包裹
   - 如果有 ``` 包裹，这是正常的代码块，应该显示

### 问题 B: JSON 右侧有多余内容

**可能原因**:

1. **JSON 提取不完整**
   - tryExtractJsonAt 的括号匹配可能有问题
   - 字符串中的括号可能导致提前结束

2. **JSON 格式问题**
   - JSON 中可能有特殊字符
   - JSON 可能不是有效的

**调试方法**:
- 查看控制台日志中的 JSON 预览
- 检查 startIndex 和 endIndex 是否正确

---

## 6. 测试验证步骤

### 步骤 1: 重启扩展
```
Cmd+Shift+P → "Reload Window"
```

### 步骤 2: 打开开发者工具
```
帮助 → 切换开发人员工具
```

### 步骤 3: 发送测试消息
```
"做一个登录功能，包含前后端的，使用python和vue"
```

### 步骤 4: 查看控制台日志

**期望看到**:
```
[content-parser] 发现裸露 JSON: 1 个
[content-parser] JSON 1: { startIndex: ..., endIndex: ..., length: ..., preview: ... }
[content-parser] 移除 JSON 后的内容长度: ...
```

**如果没有日志**:
- parseContentToBlocks 没有被调用
- 或者没有找到裸露的 JSON

**如果有日志但 JSON 还在显示**:
- 检查 startIndex 和 endIndex 是否正确
- 检查移除逻辑是否正确执行

---

## 7. 数据流完整性检查

### Orchestrator 消息

```typescript
// 1. 生成消息
OrchestratorAgent.emitUIMessage({
  content: "根据项目历史记录...\n\n{JSON对象}\n\n后续文字",
  type: 'direct_response'
})

// 2. 标准化
normalizeOrchestratorMessage(uiMessage)
  ↓
  blocks = parseContentToBlocks(content)
    ↓
    extractEmbeddedJson(content)  // 找到 JSON
    ↓
    移除 JSON
    ↓
    content = "根据项目历史记录...\n\n后续文字"
    ↓
    parseTextContent(content)
    ↓
    return [{ type: 'text', content: ..., isMarkdown: false }]

// 3. 发送到前端
StandardMessage {
  blocks: [
    { type: 'text', content: "根据项目历史记录...\n\n后续文字", isMarkdown: false }
  ]
}

// 4. 前端渲染
renderParsedBlocks(blocks)
  ↓
  case 'text':
    formatSimpleContent(block.content)
    ↓
    显示: "根据项目历史记录...\n\n后续文字"
```

### 预期结果

- ✅ 用户只看到文字说明
- ✅ JSON 被自动移除
- ✅ 不显示代码块

---

## 8. 下一步行动

1. **用户测试**: 重启扩展，发送消息，查看控制台日志
2. **提供日志**: 将控制台输出截图或复制给我
3. **分析问题**: 根据日志确定问题所在
4. **针对性修复**: 如果 JSON 提取有问题，修复 tryExtractJsonAt

---

**状态**: 等待用户测试反馈
**关键日志**: `[content-parser]` 开头的所有输出
