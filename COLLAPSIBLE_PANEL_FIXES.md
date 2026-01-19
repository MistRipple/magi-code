# 折叠面板与内容渲染修复总结

## 用户核心需求

**关键原则**: "用户除了AI响应的内容外，不关心其他东西，不要输出一些AI出的包含内容的json串"

**折叠面板使用规则**:
- ✅ **应该使用折叠面板**: 工具调用、思考过程、代码片段、长文档
- ❌ **不应该使用折叠面板**: 普通AI响应、总结内容、计划、澄清请求

## 修复内容总览

### 1. 移除 JSON 自动检测 ✅

**问题**: 前端和后端都在自动检测 JSON 并渲染为代码块，导致用户看到原始 JSON 而不是 AI 的自然语言解释。

**修复位置**:

#### 前端 (`src/ui/webview/index.html` 行 4548-4556)
```javascript
case 'text':
  // 🔧 移除前端 JSON 检测 - 后端已经处理，避免显示意外的 JSON 代码块
  // 如果后端认为是 JSON，会发送 type: 'code' 块
  if (block.isMarkdown) {
    html += '<div class="markdown-rendered">' + renderMarkdown(block.content) + '</div>';
    hasMarkdown = true;
  } else {
    html += formatSimpleContent(block.content);
  }
  break;
```

#### 后端 (`src/utils/content-parser.ts` 行 331-339)
```typescript
// 🔧 只有在纯 JSON 时才作为代码块（整个内容都是 JSON，没有其他文本）
// 如果 JSON 混合在其他文本中，说明是 AI 的解释，应该保持原样
if (jsonInfo.isJson && trimmed === jsonInfo.jsonText) {
  return [{
    type: 'code',
    content: jsonInfo.jsonText,
    language: 'json',
  } as ContentBlock];
}
```

**效果**:
- ✅ AI 的自然语言响应正常显示
- ✅ 不会显示意外的 JSON 代码块
- ✅ 只有纯 JSON 才会被渲染为代码块

---

### 2. 工具面板标签优化 ✅

**问题**: 工具面板使用中文标签"输入"/"输出"/"错误"

**修复位置**: `src/ui/webview/index.html` 行 3731, 3734, 3737

```javascript
if (hasInput) {
  sections.push('<div class="tool-section"><div class="tool-section-title">IN</div>' + renderToolPanelContent(inputContent) + '</div>');
}
if (hasOutput) {
  sections.push('<div class="tool-section"><div class="tool-section-title">OUT</div>' + renderToolPanelContent(outputContent) + '</div>');
}
if (hasError) {
  sections.push('<div class="tool-section"><div class="tool-section-title">ERROR</div>' + renderToolPanelContent(errorContent) + '</div>');
}
```

**效果**:
- ✅ 更简洁的标签显示
- ✅ 符合国际化习惯
- ✅ 节省界面空间

---

### 3. 特殊面板标题无点击事件 ✅

**问题**: 用户不希望特殊面板（计划确认、澄清请求等）的标题有点击事件

**修复原则**:
- 特殊面板保留标题，但设置 `collapsed: false`
- 只有工具调用面板可折叠 (`collapsed: true`)

**修复位置**:

#### `renderPlanPreviewCard` (行 6682, 6687)
```javascript
return renderUnifiedCard({
  type: 'plan',
  variant: 'info',
  icon: getRoleIcon('plan'),
  title: '执行计划预览',
  badges: [{ text: statusText, class: badgeClass }],
  time: m.time || '',
  content: '<div class="plan-content">' + formatPlanHtml(m.content || '') + '</div>',
  footer: footerHtml,
  collapsed: false,  // 🔧 不可折叠，标题无点击事件
  className: 'plan-preview-card ' + statusClass,
  dataAttrs: { 'msg-idx': idx }
});
```

#### `renderPlanConfirmationCard` (行 6690, 6695)
```javascript
return renderUnifiedCard({
  type: 'plan',
  variant: isPending ? 'warning' : (m.confirmed ? 'success' : 'error'),
  icon: getRoleIcon('plan'),
  title: '执行计划确认',
  badges: [{ text: statusText, class: badgeClass }],
  time: m.time || '',
  content: '<div class="plan-content">' + formatPlanHtml(m.content || '') + '</div>',
  footer: footerHtml,
  collapsed: false,  // 🔧 不可折叠，标题无点击事件
  className: 'plan-confirmation-card ' + statusClass,
  dataAttrs: { 'msg-idx': idx }
});
```

#### `renderQuestionCard` (行 6732, 6737)
```javascript
return renderUnifiedCard({
  type: 'question',
  variant: 'warning',
  icon: getRoleIcon('question'),
  title: '问题补充',
  badges: [{ text: '待回答', class: 'badge-pending' }],
  time: m.time || '',
  content: contentHtml,
  footer: footerHtml,
  collapsed: false,  // 🔧 不可折叠，标题无点击事件
  className: 'question-card',
  dataAttrs: { 'msg-idx': idx }
});
```

#### `renderCliQuestionCard` (行 6800, 6805)
```javascript
return renderUnifiedCard({
  type: 'question',
  variant: 'warning',
  icon: getRoleIcon('question'),
  title: 'CLI 询问',
  badges: [{ text: '待回答', class: 'badge-pending' }],
  time: m.time || '',
  content: contentHtml,
  footer: footerHtml,
  collapsed: false,  // 🔧 不可折叠，标题无点击事件
  className: 'cli-question-card',
  dataAttrs: { 'msg-idx': idx }
});
```

**效果**:
- ✅ 特殊面板保留标题，便于识别
- ✅ 标题无点击事件，不会误触折叠
- ✅ 只有工具调用面板可折叠

---

### 4. 移除 summaryCard 特殊处理 ✅

**问题**: 总结内容被包装成特殊卡片，而不是作为普通 AI 响应显示

**修复位置**: `src/normalizer/orchestrator-normalizer.ts`

#### 移除 summaryCard 解析 (行 41-42)
```typescript
// 🔧 移除 summaryCard 特殊处理 - 总结内容应该作为普通消息显示
// const summaryCard = uiMessage.type === 'summary' ? parseSummaryCard(uiMessage.content) : null;
```

#### 所有内容作为普通文本显示 (行 45-52)
```typescript
// 主文本内容 - 所有内容都作为普通文本显示
if (uiMessage.content) {
  const textBlock: TextBlock = {
    type: 'text',
    content: uiMessage.content,
    isMarkdown: uiMessage.type === 'plan_ready' || uiMessage.type === 'summary',  // 总结也用 Markdown
  };
  blocks.push(textBlock);
}
```

#### 移除 metadata 中的 summaryCard (行 98)
```typescript
metadata: {
  taskId: uiMessage.taskId,
  phase: uiMessage.metadata?.phase,
  subTaskId: uiMessage.metadata?.subTaskId,
  // 🔧 移除 summaryCard - 总结内容作为普通消息显示
},
```

**效果**:
- ✅ 总结内容作为普通 Markdown 消息显示
- ✅ 不再被包装成特殊卡片
- ✅ 用户看到的是自然的对话流

---

## 保留的特殊卡片

### subTaskCard ✅ (保留)

**位置**: `src/ui/webview-provider.ts` 行 2528-2544

**内容**:
- 概览: 描述、执行者、耗时
- 错误: 错误信息（如果失败）
- 文件变更: 修改的文件及行数变化
- 验证提醒: 运行测试、手动验证、检查快照

**为什么保留**:
- ✅ 这是技术元数据，不是对话内容
- ✅ 结构化数据（文件变更、统计信息）
- ✅ 验证提醒对用户有价值
- ✅ 已设置 `collapsed: false`（不可折叠）

**渲染位置**: `src/ui/webview/index.html` 行 3673-3699

```javascript
return renderUnifiedCard({
  type: 'summary',
  variant: card.status === 'failed' ? 'error' : 'success',
  title: card.title || '子任务结果',
  badges: [{ text: statusText, class: badgeClass }],
  content: renderSummarySections(sections),
  collapsed: false,  // ✅ 不可折叠
});
```

---

## 折叠面板使用总结

### ✅ 应该使用折叠面板 (`collapsed: true`)

1. **工具调用** (`renderToolCallItem` 行 3749)
   - 输入参数
   - 输出结果
   - 错误信息
   - 最新的工具调用默认展开

### ❌ 不应该使用折叠面板 (`collapsed: false`)

1. **计划预览** (`renderPlanPreviewCard`)
2. **计划确认** (`renderPlanConfirmationCard`)
3. **问题补充** (`renderQuestionCard`)
4. **CLI 询问** (`renderCliQuestionCard`)
5. **子任务结果** (`renderSubTaskSummaryCard`)
6. **普通 AI 响应** (直接渲染为消息内容)

---

## 验证清单

- ✅ AI 的文本响应不包含意外的 JSON 代码块
- ✅ 工具调用的输入/输出使用 IN/OUT/ERROR 标签
- ✅ 特殊面板标题无点击事件
- ✅ 总结内容作为普通消息显示
- ✅ 子任务结果卡片保留（技术元数据）
- ✅ Markdown 渲染正常
- ✅ 代码块渲染正常
- ✅ 编译通过

---

**状态**: ✅ 修复完成
**修复日期**: 2025-01-19
**编译状态**: ✅ 通过
**修改文件**:
- `src/ui/webview/index.html`
- `src/utils/content-parser.ts`
- `src/normalizer/orchestrator-normalizer.ts`
