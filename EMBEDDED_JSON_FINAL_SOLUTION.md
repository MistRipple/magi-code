# 嵌入式 JSON 代码块处理方案

## 问题描述

用户报告：AI 响应中显示了 JSON 代码块，这些 JSON 包含内部数据（如约束条件、需求分析），用户不需要看到这些技术细节。

**示例**：
```
根据项目历史记录和当前状态，该登录功能已经完整实现并通过集成测试。以下是需求分析：

```json
{
  "constraints": [
    "后端必须使用 Python (已使用 FastAPI 框架)",
    "前端必须使用 Vue (已使用 Vue 3 + Vite)"
  ]
}
```

后续文字...
```

用户希望只看到文字说明，不看到中间的 JSON 代码块。

---

## 参考：Augment 的处理方式

通过分析 augment 插件的截图，发现它的处理方式是：
- ✅ 将结构化数据转换为表格或列表展示
- ✅ 代码块正常显示（带语言标识和折叠按钮）
- ✅ **不显示原始 JSON**

---

## 我们的解决方案

### 核心思路

**智能标记 + 前端隐藏**

1. **后端识别**：检测消息是否以代码块开头
2. **标记嵌入式**：如果以文本开头，中间的 JSON 代码块标记为 `isEmbedded: true`
3. **前端隐藏**：前端检测到 `isEmbedded: true`，完全跳过渲染

### 处理规则

| 场景 | 处理方式 | 原因 |
|------|----------|------|
| 消息以 JSON 代码块开头 | 正常显示 | 这是主要内容 |
| 消息以文本开头，中间有 JSON 代码块 | 标记并隐藏 | 这是内部数据 |
| 消息以文本开头，中间有其他代码块（bash, python 等） | 正常显示 | 这是示例代码 |
| 裸露的 JSON（没有 ``` 包裹） | 移除 | 已有逻辑处理 |

---

## 实现细节

### 1. 类型定义修改

**文件**: `src/protocol/message-protocol.ts`

```typescript
export interface CodeBlock {
  type: 'code';
  language: string;
  content: string;
  filename?: string;
  highlightLines?: number[];
  /** 是否为嵌入式代码块（在文本中间的代码块，通常是内部数据，不需要显示给用户） */
  isEmbedded?: boolean;  // 新增字段
}
```

### 2. 后端解析逻辑

**文件**: `src/utils/content-parser.ts`

**关键代码**（行 383-424）：

```typescript
// 3. 提取代码块
const codeBlocks = extractCodeBlocks(content);

// 🔧 新增：检查内容是否以代码块开头
const startsWithCodeBlock = codeBlocks.length > 0 && codeBlocks[0].startIndex === 0;

console.log('[content-parser] 代码块检查:', {
  codeBlocksCount: codeBlocks.length,
  startsWithCodeBlock,
  firstCodeBlockLang: codeBlocks[0]?.lang,
});

if (codeBlocks.length > 0) {
  let lastIndex = 0;

  for (const codeBlock of codeBlocks) {
    // 代码块之前的文本
    if (codeBlock.startIndex > lastIndex) {
      const textBefore = content.slice(lastIndex, codeBlock.startIndex).trim();
      if (textBefore) {
        blocks.push(...parseTextContent(textBefore));
      }
    }

    // 🔧 新增：如果不是以代码块开头，且当前代码块是 JSON，则标记为嵌入式
    if (!startsWithCodeBlock && codeBlock.lang === 'json') {
      console.log('[content-parser] 标记嵌入式 JSON 代码块:', {
        startIndex: codeBlock.startIndex,
        length: codeBlock.code.length,
      });
      // 添加 isEmbedded 标记，前端会隐藏这个代码块
      blocks.push({
        type: 'code',
        content: codeBlock.code,
        language: codeBlock.lang,
        filename: codeBlock.filepath,
        isEmbedded: true,  // 标记为嵌入式，前端不渲染
      } as ContentBlock);
      lastIndex = codeBlock.endIndex;
      continue;
    }

    // 代码块本身（正常显示）
    const lang = codeBlock.lang || 'text';
    blocks.push({
      type: 'code',
      content: codeBlock.code,
      language: lang,
      filename: codeBlock.filepath,
    } as ContentBlock);

    lastIndex = codeBlock.endIndex;
  }
}
```

### 3. 前端渲染逻辑

**文件**: `src/ui/webview/index.html`

**关键代码**（行 4487-4501）：

```javascript
case 'code':
  // 🔧 新增：跳过嵌入式代码块（通常是内部 JSON 数据）
  if (block.isEmbedded) {
    console.log('[DEBUG-LAYER-3] 跳过嵌入式代码块:', {
      language: block.language,
      contentLength: block.content?.length || 0,
    });
    break;
  }

  const lang = block.language || 'text';
  const filename = block.filename || '';
  html += renderCodeBlock(block.content, lang, filename);
  hasMarkdown = true;
  break;
```

---

## 测试步骤

### 1. 重启扩展
```
Cmd+Shift+P → "Reload Window"
```

### 2. 打开开发者工具
```
Cmd+Option+I
```

### 3. 发送测试消息
```
"做一个登录功能，包含前后端的，使用python和vue"
```

### 4. 观察日志

**预期日志**：
```
[DEBUG-WORKER] buildFinalMessage 解析 pendingText: { ... }
[content-parser] 代码块检查: { codeBlocksCount: 2, startsWithCodeBlock: false, firstCodeBlockLang: 'json' }
[content-parser] 标记嵌入式 JSON 代码块: { startIndex: ..., length: 1225 }
[DEBUG-WORKER] parseContentToBlocks 返回: { blocksCount: 4, blockTypes: ['text', 'code', 'code', 'text'] }
[DEBUG-LAYER-3] 前端接收 standardMessage: { blocksCount: 4, blockTypes: ['text', 'code', 'code', 'text'] }
[DEBUG-LAYER-3] renderParsedBlocks 开始渲染: { blocksCount: 4, blockTypes: ['text', 'code', 'code', 'text'] }
[DEBUG-LAYER-3] 渲染 block: { type: 'text', ... }
[DEBUG-LAYER-3] 跳过嵌入式代码块: { language: 'json', contentLength: 1225 }
[DEBUG-LAYER-3] 渲染 block: { type: 'code', language: 'bash', ... }
[DEBUG-LAYER-3] 渲染 block: { type: 'text', ... }
```

### 5. 验证结果

**预期效果**：
- ✅ 消息以文本开头："根据项目历史记录..."
- ✅ 中间的 JSON 代码块被隐藏（不显示）
- ✅ bash 代码块正常显示
- ✅ 后续文本正常显示

---

## 效果对比

### 修复前

```
根据项目历史记录和当前状态，该登录功能已经完整实现并通过集成测试。以下是需求分析：

JSON  {
  "constraints": [
    "后端必须使用 Python (已使用 FastAPI 框架)",
    "前端必须使用 Vue (已使用 Vue 3 + Vite)",
    ...
  ]
}

后续文字...

BASH  npm run dev
```

❌ 用户看到了不需要的 JSON 代码块

### 修复后

```
根据项目历史记录和当前状态，该登录功能已经完整实现并通过集成测试。以下是需求分析：

后续文字...

BASH  npm run dev
```

✅ 用户只看到有用的信息，JSON 被隐藏

---

## 边界情况处理

### 场景 1：消息以 JSON 代码块开头

**输入**：
```
```json
{
  "config": {...}
}
```

这是配置说明...
```

**处理**：
- `startsWithCodeBlock: true`
- JSON 代码块**正常显示**（因为这是主要内容）

### 场景 2：消息以文本开头，中间有 JSON

**输入**：
```
这是说明文字。

```json
{
  "data": {...}
}
```

后续文字。
```

**处理**：
- `startsWithCodeBlock: false`
- JSON 代码块**标记为嵌入式**，前端隐藏

### 场景 3：消息以文本开头，中间有 bash 代码

**输入**：
```
运行以下命令：

```bash
npm run dev
```

完成。
```

**处理**：
- `startsWithCodeBlock: false`
- bash 代码块**正常显示**（不是 JSON，保留）

### 场景 4：裸露的 JSON（没有 ``` 包裹）

**输入**：
```
说明文字

{
  "data": {...}
}

后续文字
```

**处理**：
- `extractEmbeddedJson()` 检测到裸露 JSON
- **直接移除**（已有逻辑）

---

## 优点

1. ✅ **用户体验好**：用户只看到有用的信息，不看到技术细节
2. ✅ **信息不丢失**：JSON 数据仍然在 blocks 中，只是不渲染
3. ✅ **保留有用代码**：bash、python 等示例代码正常显示
4. ✅ **实现简单**：只需添加一个标记字段
5. ✅ **灵活可控**：前端可以根据需要调整显示方式
6. ✅ **参考业界实践**：与 augment 的处理方式一致

---

## 相关文档

- `MESSAGE_RENDERING_FLOW_COMPLETE.md` - 消息渲染完整流程
- `RENDERING_FIXES_SUMMARY.md` - 所有渲染修复总结
- `EMBEDDED_JSON_FIX.md` - 裸露 JSON 修复（移除逻辑）
- `DEBUG_DATA_FLOW.md` - 三层数据流调试方案
- `DEBUG_TEST_GUIDE.md` - 调试测试指南

---

## 编译状态

✅ 编译通过
✅ 类型定义已更新
✅ 后端逻辑已实现
✅ 前端逻辑已实现
✅ 调试日志已添加

---

**状态**: ✅ 实现完成，等待用户测试
**修复日期**: 2025-01-19
**修改文件**:
1. `src/protocol/message-protocol.ts` - 添加 `isEmbedded` 字段
2. `src/utils/content-parser.ts` - 标记嵌入式 JSON 代码块
3. `src/ui/webview/index.html` - 跳过嵌入式代码块渲染

**下一步**: 用户测试并提供反馈
