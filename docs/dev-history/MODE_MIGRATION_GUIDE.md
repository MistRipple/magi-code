# 交互模式迁移指南

## 📅 更新日期：2025-01-22
## 🎯 变更：从 3 种模式简化为 2 种模式

---

## 🔄 变更概述

### 之前（v0.3.x）
- **ask**: 仅对话，不执行任何操作
- **agent**: 可以执行操作，需要确认
- **auto**: 自动执行，不需要确认

### 现在（v0.4.0+）
- **ask**: 可以调用工具，但每次都需要用户授权
- **auto**: 完全自动执行，不需要任何确认

---

## 📋 用户迁移

### 如果你之前使用 **ask 模式**

**变化**：
- ✅ 现在可以调用工具了（需要每次授权）
- ✅ 更灵活，可以在对话中执行操作

**建议**：
- 如果你只想对话，在工具授权对话框中选择"拒绝"
- 如果你想执行操作，在授权对话框中选择"允许"

### 如果你之前使用 **agent 模式**

**变化**：
- ⚠️ agent 模式已被移除
- ✅ 系统会自动切换到 **auto 模式**

**建议**：
- 如果你想要每次确认工具使用，请切换到 **ask 模式**
- 如果你想要自动执行，保持 **auto 模式**

### 如果你之前使用 **auto 模式**

**变化**：
- ✅ 无变化，行为保持不变

**建议**：
- 继续使用 auto 模式即可

---

## 🎯 新的使用场景

### Ask 模式 💬

**适用场景**：
- 探索性任务
- 需要逐步确认的操作
- 学习和理解工具的使用

**工作流程**：
```
用户：帮我创建一个新文件 test.ts
LLM：好的，我需要使用 Write 工具
系统：[弹出授权对话框]
      工具：Write
      参数：{ file_path: "test.ts", content: "..." }
      是否允许？ [允许] [拒绝]
用户：[点击允许]
LLM：[执行 Write 工具，创建文件]
```

### Auto 模式 🚀

**适用场景**：
- 明确的任务
- 自动化工作流
- 批量操作

**工作流程**：
```
用户：帮我创建一个新文件 test.ts
LLM：好的，我需要使用 Write 工具
系统：[自动执行，无需确认]
LLM：[执行 Write 工具，创建文件]
```

---

## 🔧 开发者迁移

### 类型定义

**之前**：
```typescript
type InteractionMode = 'ask' | 'agent' | 'auto';
```

**现在**：
```typescript
type InteractionMode = 'ask' | 'auto';
```

### 配置更新

**之前**：
```typescript
INTERACTION_MODE_CONFIGS = {
  ask: { allowFileModification: false, allowCommandExecution: false },
  agent: { allowFileModification: true, allowCommandExecution: true },
  auto: { allowFileModification: true, allowCommandExecution: true },
}
```

**现在**：
```typescript
INTERACTION_MODE_CONFIGS = {
  ask: {
    allowFileModification: true,
    allowCommandExecution: true,
    requireToolAuthorization: true  // ✅ 新增
  },
  auto: {
    allowFileModification: true,
    allowCommandExecution: true,
    requireToolAuthorization: false  // ✅ 新增
  },
}
```

### 代码更新

如果你的代码中有对 `agent` 模式的引用，需要更新：

**之前**：
```typescript
if (mode === 'agent') {
  // agent 模式逻辑
}
```

**现在**：
```typescript
// 根据需求选择 ask 或 auto
if (mode === 'ask') {
  // 需要授权的逻辑
} else {
  // 自动执行的逻辑
}
```

---

## 🆕 新功能：工具授权

### 前端需要实现

1. **监听授权请求消息**：
```typescript
case 'toolAuthorizationRequest':
  // 显示授权对话框
  showToolAuthorizationDialog(message.toolName, message.toolArgs);
  break;
```

2. **发送授权响应**：
```typescript
function handleUserDecision(allowed: boolean) {
  vscode.postMessage({
    type: 'toolAuthorizationResponse',
    allowed: allowed
  });
}
```

3. **UI 组件示例**：
```jsx
<Dialog>
  <DialogTitle>工具授权请求</DialogTitle>
  <DialogContent>
    <p>工具名称：{toolName}</p>
    <p>参数：{JSON.stringify(toolArgs, null, 2)}</p>
  </DialogContent>
  <DialogActions>
    <Button onClick={() => handleUserDecision(false)}>拒绝</Button>
    <Button onClick={() => handleUserDecision(true)}>允许</Button>
  </DialogActions>
</Dialog>
```

---

## ❓ 常见问题

### Q: 为什么要移除 agent 模式？

**A**: 简化用户理解难度。之前 3 种模式让用户困惑，不知道什么时候用哪个。现在只有 2 种：
- **ask** = 需要确认
- **auto** = 全自动

### Q: 我之前的 agent 模式配置会怎样？

**A**: 系统会自动迁移到 auto 模式。如果你需要每次确认，请手动切换到 ask 模式。

### Q: Ask 模式现在可以执行操作了，会不会不安全？

**A**: 不会。每次工具调用都需要你的明确授权，你可以看到工具名称和参数，然后决定是否允许。

### Q: 我可以记住授权选择吗？

**A**: 当前版本每次都需要授权。未来版本可能会添加"总是允许"选项。

### Q: 如何在代码中切换模式？

**A**:
```typescript
orchestrator.setInteractionMode('ask');  // 或 'auto'
```

---

## 📚 相关文档

- [实施计划](./MODE_SIMPLIFICATION_IMPLEMENTATION.md)
- [完成报告](./MODE_SIMPLIFICATION_COMPLETED.md)
- [原始提案](./MODE_SIMPLIFICATION_PROPOSAL.md)

---

**更新日期**: 2025-01-22
**版本**: v0.4.0
**状态**: ✅ 完全实现（前端 + 后端）
