# MultiCLI 消息流架构分析与改进方案

## 📊 当前架构分析

### 1. 消息流向

```
用户输入
    ↓
IntelligentOrchestrator (编排者)
    ↓
分解任务 → Assignment (任务分配)
    ↓
AutonomousWorker (自主 Worker)
    ↓
CLIAdapterFactory.sendMessage()
    ↓
CLI 执行 (Claude/Codex/Gemini)
    ↓
输出 → Normalizer → StandardMessage
    ↓
WebviewProvider → 前端显示
```

### 2. 当前显示逻辑

#### 主对话区 (Thread 面板)
- **显示内容**:
  - ✅ 用户输入
  - ✅ 编排者的规划和总结
  - ✅ 编排者的询问 (adapterRole === 'orchestrator')
  - ✅ **Worker 执行状态卡片** (subTaskCard)
    - 当前在做什么
    - 执行进度
    - 文件变更
    - 验证提醒
  - ✅ **Worker 完成总结卡片** (summaryCard)
    - 执行总结
    - 完成状态

- **不显示**:
  - ❌ Worker 的详细执行过程（详细的思考、逐行输出）
  - ❌ Worker 的完整输出内容

#### Worker CLI 面板 (Claude/Codex/Gemini)
- **显示内容**:
  - ✅ Worker 的详细执行输出
  - ✅ Worker 的思考过程
  - ✅ Worker 的询问和回答
  - ✅ Worker 的完整输出

- **当前问题**:
  - ❌ **没有特殊标识**显示"这是来自编排者的任务"
  - ❌ 编排者分配的任务和 Worker 自己的输出混在一起
  - ❌ 用户无法区分哪些是编排者下发的任务，哪些是 Worker 的执行结果

### 3. 代码追踪

#### 后端 - 任务分配流程

**文件**: `src/orchestrator/worker/autonomous-worker.ts`

```typescript
// 行 401-411: 通过 CLIAdapterFactory 发送消息
const response = await options.adapterFactory.sendMessage(
  this.cliType,
  fullPrompt,  // 包含任务内容
  undefined,
  {
    source: 'worker',
    streamToUI: true,
    adapterRole: 'worker',  // ← 标记为 worker 角色
    ...options.adapterScope,
  }
);
```

**问题**:
- `adapterRole: 'worker'` 表示这是 worker 的消息
- 但实际上这是**编排者分配给 worker 的任务**
- 前端无法区分这是"编排者的任务分配"还是"worker 的自主输出"

#### 前端 - 消息渲染

**文件**: `src/ui/webview/index.html`

```javascript
// 行 5198-5253: renderMessageBlock 函数
function renderMessageBlock(message, idx, options) {
  const source = options.source || 'worker';

  // 行 5243-5245: 显示角色徽章
  if (!isUser && roleName && !message.isClarification && !message.isWorkerQuestion) {
    html += '<span class="message-role-badge ' + badgeClass + '">' + roleName + '</span>';
  }
}
```

**问题**:
- 只有 `isClarification` 和 `isWorkerQuestion` 有特殊徽章
- **没有** `isOrchestratorTask` 或类似的标识
- 编排者的任务分配没有特殊视觉标识

---

## 🎯 改进方案

### 方案概述

在 Worker CLI 面板中，为**来自编排者的任务分配**添加特殊的视觉标识，让用户清楚地看到：
1. 这是编排者分配的任务
2. 任务的具体内容
3. 与 Worker 自己的输出区分开

### 实现步骤

#### 步骤 1: 后端标记编排者任务

**文件**: `src/orchestrator/worker/autonomous-worker.ts`

**修改位置**: 行 401-411

```typescript
// 修改前
const response = await options.adapterFactory.sendMessage(
  this.cliType,
  fullPrompt,
  undefined,
  {
    source: 'worker',
    streamToUI: true,
    adapterRole: 'worker',
    ...options.adapterScope,
  }
);

// 修改后
const response = await options.adapterFactory.sendMessage(
  this.cliType,
  fullPrompt,
  undefined,
  {
    source: 'worker',
    streamToUI: true,
    adapterRole: 'worker',
    messageMeta: {
      ...options.adapterScope?.messageMeta,
      isOrchestratorTask: true,  // ← 新增标记
      orchestratorTaskInfo: {
        assignmentId: assignment.id,
        todoId: todo.id,
        responsibility: assignment.responsibility,
        todoContent: todo.content,
      },
    },
    ...options.adapterScope,
  }
);
```

#### 步骤 2: 协议层传递标记

**文件**: `src/protocol/message-protocol.ts`

**修改位置**: MessageMetadata 接口

```typescript
export interface MessageMetadata {
  /** 任务 ID */
  taskId?: string;
  /** 子任务 ID */
  subTaskId?: string;
  /** 阶段 */
  phase?: string;
  /** 持续时间（毫秒） */
  duration?: number;
  /** 错误信息 */
  error?: string;
  /** CLI 询问唯一 ID */
  questionId?: string;
  /** CLI 询问匹配模式 */
  questionPattern?: string;
  /** CLI 询问时间戳 */
  questionTimestamp?: number;
  /** 适配器角色 */
  adapterRole?: 'worker' | 'orchestrator';

  // 🆕 新增：编排者任务标记
  isOrchestratorTask?: boolean;
  orchestratorTaskInfo?: {
    assignmentId: string;
    todoId: string;
    responsibility: string;
    todoContent: string;
  };

  /** 扩展数据 */
  extra?: Record<string, unknown>;
}
```

#### 步骤 3: 前端显示特殊徽章

**文件**: `src/ui/webview/index.html`

**修改位置 1**: standardToWebviewMessage 函数（转换消息时保留标记）

```javascript
function standardToWebviewMessage(message) {
  return {
    // ... 其他字段
    isOrchestratorTask: message.metadata?.isOrchestratorTask || false,
    orchestratorTaskInfo: message.metadata?.orchestratorTaskInfo,
    // ... 其他字段
  };
}
```

**修改位置 2**: renderMessageBlock 函数（显示特殊徽章）

```javascript
// 行 5233 之后添加
html += '<div class="message-header">';

// 🆕 编排者任务显示特殊徽章
if (message.isOrchestratorTask && message.orchestratorTaskInfo) {
  html += '<span class="orchestrator-task-badge">';
  html += '<svg viewBox="0 0 16 16" width="12" height="12" fill="currentColor">';
  html += '<path d="M8 0a8 8 0 1 1 0 16A8 8 0 0 1 8 0zM1.5 8a6.5 6.5 0 1 0 13 0 6.5 6.5 0 0 0-13 0z"/>';
  html += '<path d="M9.05 4.05a.5.5 0 0 1 .707 0l2.5 2.5a.5.5 0 0 1 0 .707l-2.5 2.5a.5.5 0 1 1-.707-.707L10.793 7.5H3.5a.5.5 0 0 1 0-1h7.293L9.05 4.757a.5.5 0 0 1 0-.707z"/>';
  html += '</svg>';
  html += '编排者任务';
  html += '</span>';

  // 可选：显示任务简要信息
  html += '<span class="orchestrator-task-hint" title="' + escapeHtml(message.orchestratorTaskInfo.responsibility) + '">';
  html += escapeHtml(message.orchestratorTaskInfo.todoContent.slice(0, 30));
  if (message.orchestratorTaskInfo.todoContent.length > 30) {
    html += '...';
  }
  html += '</span>';
}

// 🆕 澄清消息显示特殊徽章
if (message.isClarification) {
  // ... 现有代码
}
```

**修改位置 3**: CSS 样式

```css
/* 编排者任务徽章 */
.orchestrator-task-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 8px;
  background: var(--vscode-charts-blue);
  color: var(--vscode-editor-background);
  border-radius: 3px;
  font-size: 11px;
  font-weight: 500;
  margin-right: 8px;
}

.orchestrator-task-badge svg {
  flex-shrink: 0;
}

.orchestrator-task-hint {
  color: var(--vscode-descriptionForeground);
  font-size: 11px;
  font-style: italic;
  margin-left: 4px;
}
```

---

## 🎨 视觉效果示例

### 修改前

```
┌─────────────────────────────────────────┐
│ Claude 面板                              │
├─────────────────────────────────────────┤
│ [Assistant] 12:30                        │
│ 我将开始实现用户登录功能...              │
│                                          │
│ [Assistant] 12:31                        │
│ 已创建 login.ts 文件...                  │
└─────────────────────────────────────────┘
```

**问题**: 无法区分哪个是编排者的任务，哪个是 Worker 的输出

### 修改后

```
┌─────────────────────────────────────────┐
│ Claude 面板                              │
├─────────────────────────────────────────┤
│ [→ 编排者任务] 实现用户登录功能...       │
│ [Assistant] 12:30                        │
│ 我将开始实现用户登录功能...              │
│                                          │
│ [Assistant] 12:31                        │
│ 已创建 login.ts 文件...                  │
└─────────────────────────────────────────┘
```

**改进**:
- ✅ 清楚地看到"编排者任务"徽章
- ✅ 任务内容简要显示
- ✅ 与 Worker 输出明确区分

---

## 📋 完整的消息类型标识

修改后，Worker CLI 面板将支持以下消息类型标识：

1. **[→ 编排者任务]** - 蓝色徽章
   - 来自编排者的任务分配
   - 显示任务简要内容
   - 可悬停查看完整职责

2. **[? 需求澄清]** - 黄色徽章（已有）
   - 编排者请求用户澄清需求
   - 显示在主对话区

3. **[? Worker 提问]** - 橙色徽章（已有）
   - Worker 向用户提问
   - 显示在主对话区

4. **[CLI 询问]** - 警告徽章（已有）
   - CLI 工具的交互式询问
   - 需要用户回答 y/n

5. **[Assistant]** - 普通徽章
   - Worker 的正常输出
   - 思考过程、执行结果等

---

## 🔄 消息流完整示例

### 场景：用户请求"实现用户登录功能"

#### 1. 主对话区 (Thread 面板)

```
[User] 12:00
实现用户登录功能

[Orchestrator] 12:01
我将把任务分解为以下部分：
1. 创建登录表单组件
2. 实现登录 API
3. 添加身份验证逻辑

[? 需求澄清] 12:02
是否需要支持第三方登录（如 Google、GitHub）？

[User] 12:03
不需要，只需要用户名密码登录

[Orchestrator] 12:04
好的，我将分配任务给 Claude 和 Codex...

┌─────────────────────────────────────────┐
│ [子任务执行中] Claude                    │
│ 创建登录表单组件                         │
│ 状态: 执行中                             │
│ 进度: 50%                                │
└─────────────────────────────────────────┘

┌─────────────────────────────────────────┐
│ [子任务完成] Claude                      │
│ 创建登录表单组件                         │
│ 状态: 完成                               │
│ 文件变更:                                │
│ - src/components/LoginForm.tsx          │
│ - src/components/LoginForm.test.tsx     │
│ 耗时: 45s                                │
└─────────────────────────────────────────┘

┌─────────────────────────────────────────┐
│ [子任务完成] Codex                       │
│ 实现登录 API                             │
│ 状态: 完成                               │
│ 文件变更:                                │
│ - src/api/auth.ts                        │
│ - src/api/auth.test.ts                   │
│ 耗时: 38s                                │
└─────────────────────────────────────────┘

[Orchestrator] 12:08
所有任务已完成！用户登录功能已实现。
```

**说明**:
- ✅ 显示用户输入和编排者的规划
- ✅ 显示 Worker 执行状态卡片（subTaskCard）
- ✅ 显示 Worker 完成总结卡片（summaryCard）
- ❌ **不显示** Worker 的详细执行过程

#### 2. Claude 面板（Worker 详细输出）

```
[→ 编排者任务] 创建登录表单组件
职责: 前端组件开发
任务: 创建一个 React 登录表单组件，包含用户名和密码输入框
预期产出: LoginForm.tsx 组件文件

[Assistant] 12:05
我将创建一个 React 登录表单组件...

[思考过程] (可折叠)
- 需要用户名和密码输入框
- 添加表单验证
- 处理提交事件
- 使用 React Hook Form 管理表单状态
- 添加错误提示

[Assistant] 12:06
正在创建 LoginForm.tsx...

```typescript
import React from 'react';
import { useForm } from 'react-hook-form';

export const LoginForm: React.FC = () => {
  const { register, handleSubmit, formState: { errors } } = useForm();

  const onSubmit = async (data) => {
    // 调用登录 API
  };

  return (
    <form onSubmit={handleSubmit(onSubmit)}>
      <input {...register('username', { required: true })} />
      <input type="password" {...register('password', { required: true })} />
      <button type="submit">登录</button>
    </form>
  );
};
```

[Assistant] 12:07
✓ 已创建 LoginForm.tsx
✓ 已添加表单验证逻辑
✓ 已创建单元测试文件

[工具调用] (可折叠)
- Write: src/components/LoginForm.tsx
- Write: src/components/LoginForm.test.tsx
```

**说明**:
- ✅ **显示编排者任务徽章**（这是我们要添加的）
- ✅ 显示 Worker 的详细思考过程
- ✅ 显示 Worker 的完整代码输出
- ✅ 显示 Worker 的工具调用

#### 3. Codex 面板（Worker 详细输出）

```
[→ 编排者任务] 实现登录 API
职责: 后端 API 开发
任务: 实现后端登录 API，包含 JWT token 生成
预期产出: auth.ts API 文件

[Assistant] 12:05
我将实现后端登录 API...

[思考过程] (可折叠)
- 创建 /api/auth/login 端点
- 验证用户名和密码
- 生成 JWT token
- 返回 token 和用户信息

[Assistant] 12:06
正在实现登录 API...

```typescript
import jwt from 'jsonwebtoken';

export async function login(username: string, password: string) {
  // 验证用户
  const user = await validateUser(username, password);

  // 生成 token
  const token = jwt.sign({ userId: user.id }, process.env.JWT_SECRET);

  return { token, user };
}
```

[Assistant] 12:07
✓ 已创建 /api/auth/login 端点
✓ 已添加 JWT token 生成逻辑
✓ 已创建 API 测试
```

**说明**:
- ✅ **显示编排者任务徽章**（这是我们要添加的）
- ✅ 显示 Worker 的详细执行过程
- ✅ 显示 Worker 的完整代码输出

---

## ✅ 改进效果

### 用户体验提升

1. **清晰的任务来源**
   - ✅ 用户一眼就能看出哪些是编排者分配的任务
   - ✅ 哪些是 Worker 的执行输出

2. **更好的任务追踪**
   - ✅ 可以看到每个 Worker 收到了什么任务
   - ✅ 可以追踪任务的执行进度

3. **减少混淆**
   - ✅ 编排者的任务和 Worker 的输出明确分离
   - ✅ 不会误以为 Worker 自己决定做什么

4. **完整的上下文**
   - ✅ 悬停徽章可以看到完整的职责描述
   - ✅ 了解任务的背景和目标

### 技术优势

1. **零兼容性负担**
   - ✅ 新增字段，不影响现有功能
   - ✅ 向后兼容，旧消息不显示徽章

2. **统一的标识系统**
   - ✅ 与现有的 `isClarification`、`isWorkerQuestion` 一致
   - ✅ 可扩展，未来可添加更多消息类型

3. **完整的数据流**
   - ✅ 后端 → 协议层 → 前端，完整传递
   - ✅ 所有信息都有据可查

---

## 📝 实现检查清单

### 后端修改
- [ ] 修改 `autonomous-worker.ts` - 添加 `isOrchestratorTask` 标记
- [ ] 修改 `message-protocol.ts` - 扩展 `MessageMetadata` 接口
- [ ] 编译验证 - `npx tsc`

### 前端修改
- [ ] 修改 `standardToWebviewMessage` - 保留标记字段
- [ ] 修改 `renderMessageBlock` - 添加徽章渲染逻辑
- [ ] 添加 CSS 样式 - `.orchestrator-task-badge`
- [ ] 测试显示效果

### 测试验证
- [ ] 创建测试场景 - 编排者分配任务
- [ ] 验证徽章显示 - Worker 面板中显示蓝色徽章
- [ ] 验证悬停提示 - 显示完整职责描述
- [ ] 验证主对话区 - 不显示 Worker 任务详情

---

## 🎯 总结

### 当前问题
- ❌ Worker 面板中，编排者的任务和 Worker 的输出混在一起
- ❌ 用户无法区分消息来源
- ❌ 缺少视觉标识

### 解决方案
- ✅ 后端添加 `isOrchestratorTask` 标记
- ✅ 前端显示蓝色"编排者任务"徽章
- ✅ 清晰区分任务来源和执行输出

### 预期效果
- ✅ 用户体验大幅提升
- ✅ 任务追踪更加清晰
- ✅ 符合用户的心智模型

---

**文档创建时间**: 2025-01-20
**状态**: 📋 设计完成，待实现
**优先级**: 🔥 高（用户体验关键改进）

