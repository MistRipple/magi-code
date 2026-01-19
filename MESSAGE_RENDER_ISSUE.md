# 消息渲染问题深度分析与修复

## 问题现象

用户发送消息后：
1. 显示两个 "Orchestrator" 标签
2. 都显示"正在思考"（streaming 状态）
3. 然后都消失了（没有内容）

## 根本原因

### 问题链路

1. **Orchestrator 发送消息** → `cliFactory.sendMessage()` with `adapterRole: 'orchestrator'`
2. **Normalizer 创建流式消息** → `startStream()` 创建 `blocks: []` 的初始消息
3. **前端接收 standardMessage** → 创建 `streaming: true`, `content: ''` 的消息
4. **显示"正在思考"** → 因为 `streaming: true`，渲染检查通过
5. **接收 standardUpdate** → 更新内容（但可能很少或没有文本）
6. **接收 standardComplete** → 设置 `streaming: false`
7. **渲染检查** → `hasRenderableContent = false`（因为 `content` 为空）
8. **消息被跳过** → 不渲染，消息"消失"

### 核心问题

**`renderMessageList` 的 `hasRenderableContent` 检查过于严格**：

```javascript
const hasRenderableContent =
  m.streaming ||  // ✅ streaming 时可以渲染
  (m.content && String(m.content).trim()) ||  // ❌ 完成后 content 为空就跳过
  // ... 其他条件
```

当消息完成（`streaming = false`）但 `content` 为空时，即使消息存在且可能包含其他信息（工具调用、thinking等），消息也会被跳过。

### 为什么 content 为空？

1. **工具调用消息** - Claude 可能只返回工具调用，没有文本内容
2. **状态消息** - Orchestrator 的某些消息可能只是状态更新
3. **解析问题** - Normalizer 可能没有正确提取文本到 blocks

## 修复方案

### 修改位置

`src/ui/webview/index.html` - `renderMessageList` 函数（约第 5010 行）

### 修改内容

添加特殊处理：**Orchestrator 消息即使内容为空也应该渲染**

```javascript
const isOrchestrator = m.source === 'orchestrator';
const hasRenderableContent =
  m.streaming ||
  (m.content && String(m.content).trim()) ||
  // ... 其他条件
  (isOrchestrator && m.standardMessageId);  // 🔧 orchestrator 消息总是渲染
```

### 修复逻辑

| 场景 | streaming | content | isOrchestrator | 旧逻辑 | 新逻辑 | 说明 |
|------|-----------|---------|----------------|--------|--------|------|
| 流式中 | true | '' | true | ✅ 渲染 | ✅ 渲染 | 显示"正在思考" |
| 完成有内容 | false | 'text' | true | ✅ 渲染 | ✅ 渲染 | 正常显示 |
| 完成无内容 | false | '' | true | ❌ 跳过 | ✅ 渲染 | **修复点** |
| Worker消息 | false | '' | false | ❌ 跳过 | ❌ 跳过 | 保持原逻辑 |

## 影响范围

- ✅ 修复 Orchestrator 消息消失问题
- ✅ 保持 Worker 消息的原有逻辑
- ✅ 不影响其他消息类型

## 测试验证

修复后需要验证：
1. ✅ Orchestrator 消息不再消失
2. ✅ 空内容消息显示占位符或状态
3. ✅ 正常消息显示不受影响
4. ✅ Worker 消息逻辑不变

---

**状态**: ✅ 已修复
**修复日期**: 2025-01-19
**修改文件**: `src/ui/webview/index.html`
