# 数据流三层检查方案

## 问题定位策略

按照用户要求，需要检查三个环节：
1. **CLI 返回的数据格式** - Orchestrator 生成的原始消息
2. **后端统一处理数据格式** - normalizeOrchestratorMessage + parseContentToBlocks
3. **前端解析** - renderParsedBlocks

---

## 第一层：CLI 返回的原始数据

### 数据源头
- **事件**: `orchestrator:ui_message`
- **位置**: `/Users/xie/code/MultiCLI/src/ui/webview-provider.ts:673`
- **数据类型**: `OrchestratorUIMessage`

### 数据结构
```typescript
interface OrchestratorUIMessage {
  type: OrchestratorMessageType;
  taskId: string;
  timestamp: number;
  content: string;  // ⚠️ 这里是原始内容，可能包含裸露的 JSON
  metadata?: {
    phase?: OrchestratorState;
    formattedPlan?: string;
    // ...
  };
}
```

### 检查点 1.1：添加日志查看原始内容

**文件**: `src/ui/webview-provider.ts`
**位置**: 行 673-690

**需要添加的日志**:
```typescript
globalEventBus.on('orchestrator:ui_message', (event) => {
  const data = event.data as any;
  if (!data?.content) return;

  // 🔍 检查点 1.1：记录原始 CLI 数据
  console.log('[DEBUG-LAYER-1] 原始 Orchestrator 消息:', {
    type: data.type,
    contentLength: data.content.length,
    contentPreview: data.content.substring(0, 200),
    hasJson: /\{[\s\S]*"[^"]+"\s*:/.test(data.content),
  });

  // 过滤内部状态消息
  if (isInternalStateMessage(data)) {
    return;
  }

  // 转换为标准消息格式
  const standardMessage = normalizeOrchestratorMessage(data, event.taskId);

  // ... 后续代码
});
```

### 预期结果
- 如果原始内容包含裸露 JSON，日志会显示 `hasJson: true`
- 可以看到完整的原始内容（前 200 字符）

---

## 第二层：后端统一处理数据格式

### 处理流程
```
OrchestratorUIMessage
  ↓
normalizeOrchestratorMessage()  [orchestrator-normalizer.ts:33]
  ↓
parseContentToBlocks(content)  [orchestrator-normalizer.ts:47]
  ↓
extractEmbeddedJson(content)  [content-parser.ts:264]
  ↓
移除裸露 JSON  [content-parser.ts:276-283]
  ↓
StandardMessage { blocks: ContentBlock[] }
```

### 检查点 2.1：normalizeOrchestratorMessage 入口

**文件**: `src/normalizer/orchestrator-normalizer.ts`
**位置**: 行 33-56

**需要添加的日志**:
```typescript
export function normalizeOrchestratorMessage(
  uiMessage: OrchestratorUIMessage,
  traceId?: string
): StandardMessage {
  const messageId = `msg-orch-${uuidv4().substring(0, 8)}`;

  // 🔍 检查点 2.1：记录进入 normalizer
  console.log('[DEBUG-LAYER-2] normalizeOrchestratorMessage 入口:', {
    messageId,
    type: uiMessage.type,
    contentLength: uiMessage.content?.length || 0,
    contentPreview: uiMessage.content?.substring(0, 100),
  });

  // 构建内容块
  let blocks: ContentBlock[] = [];

  // 🔧 使用 parseContentToBlocks 处理内容（会自动移除裸露的 JSON）
  if (uiMessage.content) {
    blocks = parseContentToBlocks(uiMessage.content, { source: 'orchestrator' });

    // 🔍 检查点 2.2：记录解析后的 blocks
    console.log('[DEBUG-LAYER-2] parseContentToBlocks 返回:', {
      messageId,
      blocksCount: blocks.length,
      blockTypes: blocks.map(b => b.type),
      firstBlockPreview: blocks[0] ? {
        type: blocks[0].type,
        contentLength: blocks[0].content?.length || 0,
        contentPreview: blocks[0].content?.substring(0, 100),
      } : null,
    });

    // ... 后续代码
  }

  // ... 返回 StandardMessage
}
```

### 检查点 2.3：parseContentToBlocks 已有日志

**文件**: `src/utils/content-parser.ts`
**位置**: 行 263-286

**现有日志**（已实现）:
```typescript
// 2. 🔧 移除裸露的 JSON 对象（用户不需要看到原始 JSON）
const embeddedJsons = extractEmbeddedJson(content);
if (embeddedJsons.length > 0) {
  console.log('[content-parser] 发现裸露 JSON:', embeddedJsons.length, '个');
  embeddedJsons.forEach((json, idx) => {
    console.log(`[content-parser] JSON ${idx + 1}:`, {
      startIndex: json.startIndex,
      endIndex: json.endIndex,
      length: json.jsonText.length,
      preview: json.jsonText.substring(0, 100) + '...'
    });
  });

  // 从后往前移除，避免索引变化
  for (let i = embeddedJsons.length - 1; i >= 0; i--) {
    const json = embeddedJsons[i];
    const before = content.substring(0, json.startIndex).trimEnd();
    const after = content.substring(json.endIndex).trimStart();
    content = before + (before && after ? '\n\n' : '') + after;
  }

  console.log('[content-parser] 移除 JSON 后的内容长度:', content.length);
}
```

### 预期结果
- 如果有裸露 JSON，会看到 `[content-parser] 发现裸露 JSON: X 个`
- 可以看到 JSON 的位置和预览
- 可以看到移除后的内容长度

---

## 第三层：前端解析

### 处理流程
```
WebviewProvider.postMessage({ type: 'standardMessage', message: StandardMessage })
  ↓
前端 index.html 接收
  ↓
handleStandardMessage()
  ↓
renderMessages()
  ↓
renderMessageBlock()
  ↓
renderParsedBlocks(message.parsedBlocks)
  ↓
遍历 blocks，根据 type 渲染
```

### 检查点 3.1：前端接收消息

**文件**: `src/ui/webview/index.html`
**位置**: 需要在 message 事件监听器中添加

**需要添加的日志**:
```javascript
window.addEventListener('message', event => {
  const message = event.data;

  if (message.type === 'standardMessage') {
    // 🔍 检查点 3.1：记录前端接收到的消息
    console.log('[DEBUG-LAYER-3] 前端接收 standardMessage:', {
      messageId: message.message?.id,
      source: message.message?.source,
      blocksCount: message.message?.blocks?.length || 0,
      blockTypes: message.message?.blocks?.map(b => b.type) || [],
      firstBlockPreview: message.message?.blocks?.[0] ? {
        type: message.message.blocks[0].type,
        contentLength: message.message.blocks[0].content?.length || 0,
        contentPreview: message.message.blocks[0].content?.substring(0, 100),
      } : null,
    });
  }

  // ... 后续处理
});
```

### 检查点 3.2：renderParsedBlocks 渲染

**文件**: `src/ui/webview/index.html`
**位置**: renderParsedBlocks 函数（约行 4440）

**需要添加的日志**:
```javascript
function renderParsedBlocks(blocks) {
  // 🔍 检查点 3.2：记录渲染的 blocks
  console.log('[DEBUG-LAYER-3] renderParsedBlocks 开始渲染:', {
    blocksCount: blocks?.length || 0,
    blockTypes: blocks?.map(b => b.type) || [],
  });

  if (!blocks || blocks.length === 0) {
    return { html: '', isMarkdown: false };
  }

  let html = '';
  let hasMarkdown = false;

  for (const block of blocks) {
    // 🔍 检查点 3.3：记录每个 block 的渲染
    console.log('[DEBUG-LAYER-3] 渲染 block:', {
      type: block.type,
      contentLength: block.content?.length || 0,
      isMarkdown: block.isMarkdown,
      language: block.language,
    });

    switch (block.type) {
      case 'text':
        // ... 渲染逻辑
        break;
      case 'code':
        // ... 渲染逻辑
        break;
      // ...
    }
  }

  return { html, isMarkdown: hasMarkdown };
}
```

### 预期结果
- 可以看到前端接收到的 blocks 数量和类型
- 可以看到每个 block 的渲染过程
- 如果 JSON 还在显示，可以确定是哪个 block 包含了 JSON

---

## 诊断流程

### 场景 A：没有任何日志
**结论**: parseContentToBlocks 没有被调用
**原因**: normalizeOrchestratorMessage 可能没有执行到 parseContentToBlocks
**解决**: 检查 orchestrator-normalizer.ts 的逻辑

### 场景 B：有 Layer-1 和 Layer-2 日志，但没有 `[content-parser]` 日志
**结论**: extractEmbeddedJson 没有找到 JSON
**原因**:
- JSON 格式不符合预期
- JSON 在代码块中（被跳过）
- JSON 不是有效的 JSON
**解决**: 检查原始内容的 JSON 格式

### 场景 C：有 `[content-parser]` 日志显示找到 JSON，但前端还显示 JSON
**结论**: JSON 移除逻辑有问题，或者前端渲染了错误的内容
**原因**:
- 移除逻辑的索引计算错误
- 前端渲染了旧的 content 而不是 blocks
**解决**: 检查移除逻辑和前端渲染逻辑

### 场景 D：前端 Layer-3 日志显示 blocks 中有 code 类型的 JSON
**结论**: 后端将 JSON 识别为 code block 而不是移除
**原因**: parseTextContent 的逻辑将其识别为纯 JSON
**解决**: 检查 parseTextContent 的判断逻辑

---

## 实施步骤

### 步骤 1：添加后端日志
```bash
# 编辑 webview-provider.ts
# 在 orchestrator:ui_message 事件处理中添加 Layer-1 日志

# 编辑 orchestrator-normalizer.ts
# 在 normalizeOrchestratorMessage 函数中添加 Layer-2 日志
```

### 步骤 2：添加前端日志
```bash
# 编辑 index.html
# 在 message 事件监听器中添加 Layer-3 日志
# 在 renderParsedBlocks 函数中添加详细日志
```

### 步骤 3：编译并测试
```bash
npm run compile
# 重启 VSCode 扩展
# 发送测试消息
# 查看控制台日志
```

### 步骤 4：分析日志
根据上述诊断流程，确定问题出在哪一层

---

## 预期日志输出

### 正常情况（JSON 被移除）
```
[DEBUG-LAYER-1] 原始 Orchestrator 消息: { type: 'direct_response', contentLength: 500, hasJson: true, ... }
[DEBUG-LAYER-2] normalizeOrchestratorMessage 入口: { messageId: 'msg-orch-xxx', contentLength: 500, ... }
[content-parser] 发现裸露 JSON: 1 个
[content-parser] JSON 1: { startIndex: 100, endIndex: 300, length: 200, preview: '{"constraints":...' }
[content-parser] 移除 JSON 后的内容长度: 300
[DEBUG-LAYER-2] parseContentToBlocks 返回: { blocksCount: 1, blockTypes: ['text'], ... }
[DEBUG-LAYER-3] 前端接收 standardMessage: { blocksCount: 1, blockTypes: ['text'], ... }
[DEBUG-LAYER-3] renderParsedBlocks 开始渲染: { blocksCount: 1, blockTypes: ['text'] }
[DEBUG-LAYER-3] 渲染 block: { type: 'text', contentLength: 300, isMarkdown: false }
```

### 异常情况（JSON 没有被移除）
```
[DEBUG-LAYER-1] 原始 Orchestrator 消息: { type: 'direct_response', contentLength: 500, hasJson: true, ... }
[DEBUG-LAYER-2] normalizeOrchestratorMessage 入口: { messageId: 'msg-orch-xxx', contentLength: 500, ... }
# ⚠️ 没有 [content-parser] 日志 - 说明 extractEmbeddedJson 没有找到 JSON
[DEBUG-LAYER-2] parseContentToBlocks 返回: { blocksCount: 2, blockTypes: ['text', 'code'], ... }
[DEBUG-LAYER-3] 前端接收 standardMessage: { blocksCount: 2, blockTypes: ['text', 'code'], ... }
[DEBUG-LAYER-3] renderParsedBlocks 开始渲染: { blocksCount: 2, blockTypes: ['text', 'code'] }
[DEBUG-LAYER-3] 渲染 block: { type: 'text', contentLength: 100, isMarkdown: false }
[DEBUG-LAYER-3] 渲染 block: { type: 'code', contentLength: 200, language: 'json' }
# ⚠️ 这里显示了 code 类型的 JSON block
```

---

## 下一步

用户需要：
1. 确认是否要添加这些调试日志
2. 或者直接测试现有的代码，查看 `[content-parser]` 日志
3. 提供实际的日志输出，以便精确定位问题

---

**状态**: 等待用户确认调试方案
**建议**: 先测试现有代码，查看是否有 `[content-parser]` 日志，再决定是否需要添加更多日志
