# 裸露 JSON 问题修复

## 问题描述

用户报告在 Orchestrator 的响应中看到了裸露的 JSON 代码块：

```
根据项目历史记录和当前状态，该登录功能已经完整实现并通过集成测试。以下是详细分析：

{
  "constraints": [
    "后端必须使用 Python (已使用 FastAPI 框架) ",
    "前端必须使用 Vue (已使用 Vue 3 + Vite) ",
    ...
  ]
}
```

**问题**：这个 JSON 对象没有用 ``` 包裹，直接显示在响应中，用户看到的是原始 JSON 而不是自然语言描述。

## 根本原因

1. **AI 直接输出 JSON**：Claude API 有时会在响应中直接输出 JSON 对象（不用代码块包裹）
2. **现有解析器无法处理**：
   - `extractCodeBlocks()` 只提取 ``` 包裹的代码块
   - `parseTextContent()` 只检查整个内容是否是纯 JSON
   - 对于"文字 + 裸露 JSON"的混合内容，无法正确处理

## 解决方案

### 新增功能：提取和移除裸露 JSON

**文件**: `src/utils/content-parser.ts`

#### 1. 新增 `extractEmbeddedJson()` 函数

```typescript
/**
 * 提取内容中的裸露 JSON 对象（不在代码块中的 JSON）
 * 🔧 新增：处理 AI 响应中混合的 JSON 对象
 */
export function extractEmbeddedJson(content: string): Array<{
  jsonText: string;
  startIndex: number;
  endIndex: number;
}> {
  const results: Array<{ jsonText: string; startIndex: number; endIndex: number }> = [];

  let i = 0;
  while (i < content.length) {
    const char = content[i];

    // 跳过代码块中的内容
    if (content.substring(i, i + 3) === '```') {
      const endIndex = content.indexOf('```', i + 3);
      if (endIndex !== -1) {
        i = endIndex + 3;
        continue;
      }
    }

    if (char === '{' || char === '[') {
      // 尝试提取 JSON
      const extracted = tryExtractJsonAt(content, i);
      if (extracted) {
        results.push(extracted);
        i = extracted.endIndex;
        continue;
      }
    }
    i++;
  }

  return results;
}
```

**功能**：
- 扫描内容，查找 `{` 或 `[` 开头的 JSON 对象/数组
- 跳过代码块中的内容（避免误判）
- 使用括号匹配算法提取完整的 JSON
- 验证提取的内容是否是有效的 JSON

#### 2. 新增 `tryExtractJsonAt()` 辅助函数

```typescript
/**
 * 尝试从指定位置提取 JSON
 */
function tryExtractJsonAt(content: string, startIndex: number): { jsonText: string; startIndex: number; endIndex: number } | null {
  const startChar = content[startIndex];
  const endChar = startChar === '{' ? '}' : ']';
  let depth = 0;
  let inString = false;
  let escapeNext = false;

  for (let i = startIndex; i < content.length; i++) {
    const char = content[i];

    if (escapeNext) {
      escapeNext = false;
      continue;
    }

    if (char === '\\') {
      escapeNext = true;
      continue;
    }

    if (char === '"') {
      inString = !inString;
      continue;
    }

    if (inString) continue;

    if (char === startChar) {
      depth++;
    } else if (char === endChar) {
      depth--;
      if (depth === 0) {
        // 找到匹配的结束符
        const jsonText = content.substring(startIndex, i + 1);
        try {
          JSON.parse(jsonText);
          return {
            jsonText,
            startIndex,
            endIndex: i + 1,
          };
        } catch {
          return null;
        }
      }
    }
  }

  return null;
}
```

**功能**：
- 使用深度计数器匹配括号
- 正确处理字符串中的括号（不计入深度）
- 处理转义字符
- 验证提取的内容是否是有效的 JSON

#### 3. 修改 `parseContentToBlocks()` 函数

```typescript
export function parseContentToBlocks(
  rawContent: string,
  options?: {
    toolCalls?: Array<{ name: string; input: unknown; status?: string }>;
    source?: string;
  }
): ContentBlock[] {
  if (!rawContent) return [];

  // 1. 预处理：清理 ANSI、零宽字符等
  const sanitized = sanitizeCliOutput(rawContent);
  let content = collapseExtraBlankLines(sanitized);
  const trimmed = content.trim();

  if (!trimmed) return [];

  const blocks: ContentBlock[] = [];

  // 2. 🔧 移除裸露的 JSON 对象（用户不需要看到原始 JSON）
  const embeddedJsons = extractEmbeddedJson(content);
  if (embeddedJsons.length > 0) {
    // 从后往前移除，避免索引变化
    for (let i = embeddedJsons.length - 1; i >= 0; i--) {
      const json = embeddedJsons[i];
      // 移除 JSON 及其前后的空行
      const before = content.substring(0, json.startIndex).trimEnd();
      const after = content.substring(json.endIndex).trimStart();
      content = before + (before && after ? '\n\n' : '') + after;
    }
  }

  // 3. 提取代码块（原有逻辑）
  // ...
}
```

**修改**：
- 在提取代码块之前，先移除裸露的 JSON
- 从后往前移除，避免索引变化
- 保留 JSON 前后的文字说明

## 效果对比

### 修复前

```
根据项目历史记录和当前状态，该登录功能已经完整实现并通过集成测试。以下是详细分析：

JSON  {
  "constraints": [
    "后端必须使用 Python (已使用 FastAPI 框架) ",
    "前端必须使用 Vue (已使用 Vue 3 + Vite) ",
    ...
  ]
}
```

用户看到一个 JSON 代码块，不友好。

### 修复后

```
根据项目历史记录和当前状态，该登录功能已经完整实现并通过集成测试。以下是详细分析：

这些约束条件都已经满足。
```

用户只看到自然语言描述，JSON 被自动移除。

## 测试验证

```javascript
// 测试内容
const content = `根据项目历史记录和当前状态，该登录功能已经完整实现并通过集成测试。以下是详细分析：

{
  "constraints": [
    "后端必须使用 Python (已使用 FastAPI 框架) ",
    "前端必须使用 Vue (已使用 Vue 3 + Vite) ",
    "需要实现前后端分离架构"
  ]
}

这些约束条件都已经满足。`;

// 提取裸露 JSON
const embeddedJsons = extractEmbeddedJson(content);
console.log('找到的 JSON 数量:', embeddedJsons.length); // 1

// 移除 JSON
let cleaned = content;
for (let i = embeddedJsons.length - 1; i >= 0; i--) {
  const json = embeddedJsons[i];
  const before = cleaned.substring(0, json.startIndex).trimEnd();
  const after = cleaned.substring(json.endIndex).trimStart();
  cleaned = before + (before && after ? '\n\n' : '') + after;
}

console.log(cleaned);
// 输出：
// 根据项目历史记录和当前状态，该登录功能已经完整实现并通过集成测试。以下是详细分析：
//
// 这些约束条件都已经满足。
```

✅ 测试通过

## 边界情况处理

### 1. 代码块中的 JSON（不移除）

```
这是说明文字。

\`\`\`json
{
  "key": "value"
}
\`\`\`

这是后续文字。
```

✅ 代码块中的 JSON 不会被移除（`extractEmbeddedJson` 会跳过代码块）

### 2. 字符串中的括号（不误判）

```
{
  "message": "这里有 { 和 } 括号"
}
```

✅ 字符串中的括号不会影响括号匹配

### 3. 多个裸露 JSON

```
第一个 JSON:
{"a": 1}

第二个 JSON:
{"b": 2}
```

✅ 所有裸露的 JSON 都会被移除

### 4. 无效的 JSON（不移除）

```
这不是有效的 JSON: {invalid}
```

✅ 无效的 JSON 不会被移除（`JSON.parse` 会失败）

## 相关修复

这个修复是之前 JSON 检测修复的补充：

1. **之前的修复**（已完成）：
   - 移除前端 JSON 检测
   - 后端只检测纯 JSON

2. **本次修复**（新增）：
   - 移除裸露的 JSON 对象
   - 只保留文字说明

## 编译状态

✅ 编译通过

```bash
npm run compile
```

---

**状态**: ✅ 修复完成
**修复日期**: 2025-01-19
**修改文件**: `src/utils/content-parser.ts`
**新增函数**: `extractEmbeddedJson()`, `tryExtractJsonAt()`
**修改函数**: `parseContentToBlocks()`
