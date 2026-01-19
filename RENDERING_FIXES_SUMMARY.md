# 消息渲染问题修复总结

## 问题背景

用户报告前端消息渲染存在以下问题：
1. 用户看到原始 JSON 串而不是 AI 的自然语言响应
2. 工具面板标签不够简洁
3. 特殊面板标题有点击事件（不应该可折叠）
4. 总结内容被错误地包装成折叠面板

## 核心原则

**用户需求**: "用户除了AI响应的内容外，不关心其他东西，不要输出一些AI出的包含内容的json串"

**折叠面板使用规则**:
- ✅ **应该折叠**: 工具调用、思考过程、代码片段、长文档
- ❌ **不应该折叠**: 普通AI响应、总结内容、计划、澄清请求

## 修复详情

### 修复 1: 移除 JSON 自动检测

**文件**:
- `src/ui/webview/index.html` (行 4548-4556)
- `src/utils/content-parser.ts` (行 331-339)

**问题**: 前端和后端都在自动检测 JSON 并渲染为代码块

**解决方案**:
- 前端: 移除 `isJsonText()` 检查，完全信任后端分类
- 后端: 只有纯 JSON（整个内容都是 JSON）才作为代码块

**效果**: AI 的自然语言响应正常显示，不会出现意外的 JSON 代码块

---

### 修复 2: 工具面板标签优化

**文件**: `src/ui/webview/index.html` (行 3731, 3734, 3737)

**修改**: 将"输入"/"输出"/"错误"改为 "IN"/"OUT"/"ERROR"

**效果**: 更简洁、国际化的标签显示

---

### 修复 3: 特殊面板标题无点击事件

**文件**: `src/ui/webview/index.html`

**修改位置**:
- `renderPlanPreviewCard` (行 6682, 6687)
- `renderPlanConfirmationCard` (行 6690, 6695)
- `renderQuestionCard` (行 6732, 6737)
- `renderCliQuestionCard` (行 6800, 6805)

**解决方案**: 设置 `collapsed: false`，保留标题但无点击事件

**效果**: 特殊面板不可折叠，标题仅用于识别

---

### 修复 4: 移除 summaryCard 特殊处理

**文件**: `src/normalizer/orchestrator-normalizer.ts`

**修改位置**:
- 行 41-42: 移除 `parseSummaryCard()` 调用
- 行 45-52: 所有内容作为普通文本块显示
- 行 98: 从 metadata 中移除 `summaryCard`

**效果**: 总结内容作为普通 Markdown 消息显示，不再被包装成特殊卡片

---

## 保留的特殊卡片

### subTaskCard (保留 ✅)

**位置**:
- 生成: `src/ui/webview-provider.ts` (行 2528-2544)
- 渲染: `src/ui/webview/index.html` (行 3673-3699)

**内容**:
- 概览: 描述、执行者、耗时
- 文件变更: 修改的文件及行数变化
- 验证提醒: 运行测试、手动验证、检查快照
- 错误信息（如果失败）

**为什么保留**:
- 这是技术元数据，不是对话内容
- 结构化数据（文件变更、统计信息）
- 验证提醒对用户有价值
- 已设置 `collapsed: false`（不可折叠）

---

## 折叠面板使用规范

### ✅ 应该使用折叠面板 (`collapsed: true`)

| 类型 | 函数 | 原因 |
|------|------|------|
| 工具调用 | `renderToolCallItem` | 技术细节，可选查看 |

### ❌ 不应该使用折叠面板 (`collapsed: false`)

| 类型 | 函数 | 原因 |
|------|------|------|
| 计划预览 | `renderPlanPreviewCard` | 用户需要直接看到 |
| 计划确认 | `renderPlanConfirmationCard` | 需要用户确认 |
| 问题补充 | `renderQuestionCard` | 需要用户回答 |
| CLI 询问 | `renderCliQuestionCard` | 需要用户回答 |
| 子任务结果 | `renderSubTaskSummaryCard` | 重要的执行结果 |
| 普通 AI 响应 | 直接渲染 | 对话内容 |

---

## 内容渲染流程

```
CLI 原始输出
  ↓
后端解析 (content-parser.ts)
  ├─ sanitizeCliOutput() - 清理 ANSI、零宽字符
  ├─ extractCodeBlocks() - 提取代码块
  └─ parseTextContent() - 解析文本
      ├─ 纯 JSON → code block
      ├─ Markdown 语法 → text block (isMarkdown: true)
      └─ 普通文本 → text block (isMarkdown: false)
  ↓
StandardMessage.blocks[]
  ↓
前端渲染 (index.html)
  ├─ renderParsedBlocks()
  │   ├─ text + isMarkdown → renderMarkdown()
  │   ├─ text + plain → formatSimpleContent()
  │   ├─ code → renderCodeBlock()
  │   ├─ thinking → (头部单独渲染)
  │   └─ tool_call → (底部单独渲染)
  └─ renderSpecialMessage()
      ├─ subTaskCard → renderSubTaskSummaryCard()
      ├─ plan_confirmation → renderPlanConfirmationCard()
      └─ ...
```

---

## 验证清单

- ✅ AI 的文本响应不包含意外的 JSON 代码块
- ✅ 工具调用的输入/输出使用 IN/OUT/ERROR 标签
- ✅ 特殊面板标题无点击事件（不可折叠）
- ✅ 总结内容作为普通消息显示
- ✅ 子任务结果卡片保留（技术元数据）
- ✅ Markdown 渲染正常
- ✅ 代码块渲染正常
- ✅ 工具调用面板可折叠
- ✅ 编译通过

---

## 相关文档

- `SPECIAL_PANELS_AND_RENDERING_ANALYSIS.md` - 特殊面板与内容渲染完整分析
- `FRONTEND_RENDER_CHECK.md` - 前端消息渲染完整检查报告
- `MESSAGE_RENDER_ISSUE.md` - 消息渲染问题深度分析与修复
- `COLLAPSIBLE_PANEL_FIXES.md` - 折叠面板与内容渲染修复总结

---

**状态**: ✅ 修复完成
**修复日期**: 2025-01-19
**编译状态**: ✅ 通过
**修改文件**:
1. `src/ui/webview/index.html` - 前端渲染逻辑
2. `src/utils/content-parser.ts` - 后端内容解析
3. `src/normalizer/orchestrator-normalizer.ts` - Orchestrator 消息标准化

**测试建议**:
1. 发送包含 JSON 的 AI 响应，确认显示为自然语言
2. 执行工具调用，确认 IN/OUT/ERROR 标签显示
3. 触发计划确认，确认标题无点击事件
4. 完成子任务，确认总结内容正常显示
5. 检查子任务结果卡片显示文件变更和验证提醒

---

## 🆕 修复 5: 移除裸露的 JSON 对象

**问题**: AI 响应中包含未用代码块包裹的 JSON 对象，用户看到原始 JSON 而不是自然语言描述

**示例**:
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

**解决方案**: 
- 新增 `extractEmbeddedJson()` 函数，提取内容中的裸露 JSON
- 新增 `tryExtractJsonAt()` 辅助函数，使用括号匹配算法
- 修改 `parseContentToBlocks()` 在解析前移除裸露的 JSON

**效果**: 用户只看到文字说明，裸露的 JSON 被自动移除

**详细文档**: `EMBEDDED_JSON_FIX.md`

