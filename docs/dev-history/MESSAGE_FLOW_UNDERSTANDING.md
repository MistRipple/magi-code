# 消息流架构理解总结

## ✅ 正确的架构理解

### 主对话区 (Thread 面板)

**显示内容**:
1. ✅ 用户输入
2. ✅ 编排者的规划和总结
3. ✅ 编排者的询问
4. ✅ **Worker 执行状态卡片** (subTaskCard) - 简要信息
   - 当前在做什么
   - 执行进度
   - 文件变更
5. ✅ **Worker 完成总结卡片** (summaryCard)
   - 执行总结
   - 完成状态

**不显示**:
- ❌ Worker 的详细执行过程（详细的思考、逐行输出）
- ❌ Worker 的完整代码输出

### Worker CLI 面板 (Claude/Codex/Gemini)

**显示内容**:
1. ✅ **编排者分配的任务**（需要添加特殊标识 ← 改进点）
2. ✅ Worker 的详细执行输出
3. ✅ Worker 的思考过程
4. ✅ Worker 的完整代码输出
5. ✅ Worker 的工具调用

---

## 🎯 需要改进的地方

### 问题
在 Worker CLI 面板中，**编排者分配的任务**和 **Worker 的执行输出**混在一起，用户无法区分。

### 解决方案
为编排者分配的任务添加**特殊视觉标识**（蓝色徽章），让用户清楚地看到：
- 这是编排者分配的任务
- 任务的具体内容
- 与 Worker 自己的输出区分开

---

## 📋 实现计划

### 1. 后端标记
在 `autonomous-worker.ts` 中，发送消息时添加 `isOrchestratorTask` 标记：

```typescript
const response = await options.adapterFactory.sendMessage(
  this.cliType,
  fullPrompt,
  undefined,
  {
    source: 'worker',
    streamToUI: true,
    adapterRole: 'worker',
    messageMeta: {
      isOrchestratorTask: true,  // ← 新增
      orchestratorTaskInfo: {
        assignmentId: assignment.id,
        todoId: todo.id,
        responsibility: assignment.responsibility,
        todoContent: todo.content,
      },
    },
  }
);
```

### 2. 协议层扩展
在 `message-protocol.ts` 中扩展 `MessageMetadata` 接口：

```typescript
export interface MessageMetadata {
  // ... 现有字段
  isOrchestratorTask?: boolean;
  orchestratorTaskInfo?: {
    assignmentId: string;
    todoId: string;
    responsibility: string;
    todoContent: string;
  };
}
```

### 3. 前端显示
在 `index.html` 中添加徽章渲染逻辑：

```javascript
// 在 renderMessageBlock 函数中
if (message.isOrchestratorTask && message.orchestratorTaskInfo) {
  html += '<span class="orchestrator-task-badge">';
  html += '<svg>...</svg>';
  html += '编排者任务';
  html += '</span>';
}
```

---

## 🎨 预期效果

### Worker CLI 面板显示

```
┌─────────────────────────────────────────┐
│ Claude 面板                              │
├─────────────────────────────────────────┤
│ [→ 编排者任务] 创建登录表单组件         │
│ 职责: 前端组件开发                       │
│ 任务: 创建 React 登录表单...            │
│                                          │
│ [Assistant] 12:05                        │
│ 我将创建一个 React 登录表单组件...      │
│                                          │
│ [思考过程] (可折叠)                      │
│ - 需要用户名和密码输入框                 │
│ - 添加表单验证                           │
│                                          │
│ [Assistant] 12:06                        │
│ ✓ 已创建 LoginForm.tsx                  │
│ ✓ 已添加表单验证逻辑                     │
└─────────────────────────────────────────┘
```

**改进**:
- ✅ 蓝色"编排者任务"徽章清晰可见
- ✅ 任务内容简要显示
- ✅ 与 Worker 输出明确区分

---

## ✅ 关键要点

1. **主对话区**显示 Worker 的**简要状态**（subTaskCard/summaryCard）
2. **Worker CLI 面板**显示 Worker 的**详细执行过程**
3. **编排者任务**需要在 Worker CLI 面板中有**特殊标识**
4. 这样用户可以：
   - 在主对话区看到整体进度
   - 在 Worker 面板看到详细执行
   - 清楚区分任务来源

---

**创建时间**: 2025-01-20
**状态**: ✅ 架构理解正确
**下一步**: 实现编排者任务徽章

