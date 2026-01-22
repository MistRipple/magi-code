# 会话管理实现方案

## 📋 执行摘要

基于你提出的 **"单次会话历史 + 项目级知识库 + 会话 ID 恢复"** 模式，当前架构已经有了非常好的基础：

- ✅ **UnifiedSessionManager** 已实现（会话级管理）
- ✅ **UI 会话选择器** 已实现（前端界面）
- ⚠️ **项目级知识库** 未实现（需要添加）
- ⚠️ **前后端集成** 需要完善

---

## 🏗️ 当前架构分析

### ✅ 已实现的功能

#### 1. **UnifiedSessionManager**（后端）
位置：`src/session/unified-session-manager.ts`

**核心功能**：
- ✅ 创建新会话 `createSession()`
- ✅ 切换会话 `switchSession()`
- ✅ 获取所有会话 `getAllSessions()`
- ✅ 会话元数据 `getSessionMetas()`
- ✅ 消息管理 `addMessage()`, `getRecentMessages()`
- ✅ 任务管理 `addTask()`, `updateTask()`
- ✅ 快照管理 `addSnapshot()`, `getSnapshot()`
- ✅ 会话删除 `deleteSession()`
- ✅ 会话重命名 `renameSession()`
- ✅ 自动保存 `saveSession()`
- ✅ 内存管理（自动驱逐旧会话）
- ✅ Token 预算管理 `getRecentMessagesWithinTokenBudget()`

**数据结构**：
```typescript
interface UnifiedSession {
  id: string;                    // session-{timestamp}-{random}
  name?: string;                 // 会话标题（自动生成或用户设置）
  status: 'active' | 'completed';
  createdAt: number;
  updatedAt: number;
  messages: SessionMessage[];    // 对话历史
  tasks: Task[];                 // 任务列表
  snapshots: FileSnapshotMeta[]; // 快照元数据
}
```

**存储结构**：
```
.multicli/sessions/{sessionId}/
├── session.json          # 会话主数据
├── plans/                # 计划文件
├── tasks.json            # 子任务状态
├── snapshots/            # 快照文件
├── missions/             # Mission 数据
└── execution-state.json  # 执行状态
```

#### 2. **UI 会话选择器**（前端）
位置：`src/ui/webview/index.html`

**UI 组件**：
- ✅ 会话选择器按钮（顶部）
- ✅ 会话下拉菜单
- ✅ 新建会话按钮
- ✅ 会话列表显示

**事件处理**：
位置：`src/ui/webview/js/ui/event-handlers.js`
- ✅ `handleSessionSelect()` - 切换会话
- ✅ `handleNewSession()` - 新建会话
- ✅ `handleRenameSession()` - 重命名会话
- ✅ `handleDeleteSession()` - 删除会话
- ✅ `handleExportSession()` - 导出会话

---

## ⚠️ 需要完善的部分

### 1. **项目级知识库**（Layer 3）

**目标**：跨会话共享的项目知识

**需要实现**：
```typescript
class ProjectKnowledgeBase {
  // 代码结构索引（自动更新）
  private codeIndex: CodeIndex;

  // 项目配置（自动检测）
  private config: ProjectConfig;

  // 架构决策记录（用户主动保存）
  private adr: ADRStore;

  // 常见问题（用户主动保存）
  private faq: FAQStore;

  // 获取项目知识摘要
  getSummary(): string;

  // 保存架构决策
  saveDecision(decision: Decision): void;

  // 保存常见问题
  saveFAQ(question: string, answer: string): void;
}
```

**存储结构**：
```
.multicli/project-knowledge/
├── code-index.json       # 代码结构索引
├── config.json           # 项目配置
├── adr/                  # 架构决策记录
│   ├── 001-session-management.md
│   └── 002-memory-architecture.md
└── faq.json              # 常见问题
```

---

### 2. **前后端集成完善**

#### 问题 1：UI 会话列表渲染
**当前状态**：UI 有会话选择器，但没有看到渲染逻辑

**需要添加**：
```javascript
// src/ui/webview/js/ui/message-renderer.js
export function renderSessionList() {
  const sessionList = document.getElementById('session-list');
  const sessionEmpty = document.getElementById('session-empty');

  if (sessions.length === 0) {
    sessionList.style.display = 'none';
    sessionEmpty.style.display = 'flex';
    return;
  }

  sessionList.style.display = 'block';
  sessionEmpty.style.display = 'none';

  sessionList.innerHTML = sessions.map(session => `
    <div class="session-item ${session.id === currentSessionId ? 'active' : ''}"
         onclick="handleSessionSelect('${session.id}')">
      <div class="session-item-header">
        <span class="session-item-name">${escapeHtml(session.name || '未命名会话')}</span>
        <span class="session-item-time">${formatTimestamp(session.updatedAt)}</span>
      </div>
      <div class="session-item-preview">${escapeHtml(session.preview)}</div>
      <div class="session-item-meta">
        <span>💬 ${session.messageCount} 条消息</span>
      </div>
      <div class="session-item-actions">
        <button onclick="event.stopPropagation(); handleRenameSession('${session.id}')" title="重命名">
          <svg>...</svg>
        </button>
        <button onclick="event.stopPropagation(); handleDeleteSession('${session.id}')" title="删除">
          <svg>...</svg>
        </button>
      </div>
    </div>
  `).join('');
}
```

#### 问题 2：WebviewProvider 消息处理
**需要添加**：处理前端发送的会话管理消息

```typescript
// src/ui/webview-provider.ts
private handleMessage(message: any) {
  switch (message.type) {
    case 'newSession':
      this.handleNewSession();
      break;
    case 'switchSession':
      this.handleSwitchSession(message.sessionId);
      break;
    case 'renameSession':
      this.handleRenameSession(message.sessionId, message.name);
      break;
    case 'deleteSession':
      this.handleDeleteSession(message.sessionId);
      break;
    case 'listSessions':
      this.handleListSessions();
      break;
  }
}

private handleNewSession() {
  const session = this.sessionManager.createSession();
  this.sendMessage({
    type: 'sessionCreated',
    session: {
      id: session.id,
      name: session.name,
      createdAt: session.createdAt
    }
  });
  this.sendSessionList();
}

private handleSwitchSession(sessionId: string) {
  const session = this.sessionManager.switchSession(sessionId);
  if (session) {
    this.sendMessage({
      type: 'sessionSwitched',
      sessionId: session.id,
      messages: session.messages
    });
  }
}

private sendSessionList() {
  const metas = this.sessionManager.getSessionMetas();
  this.sendMessage({
    type: 'sessionList',
    sessions: metas
  });
}
```

---

### 3. **ContextManager 集成**

**问题**：ContextManager 需要同时访问会话数据和项目知识库

**解决方案**：
```typescript
// src/context/context-manager.ts
class ContextManager {
  constructor(
    private sessionManager: UnifiedSessionManager,
    private projectKnowledge: ProjectKnowledgeBase
  ) {}

  getContext(maxTokens: number): Context {
    const session = this.sessionManager.getCurrentSession();

    return {
      // Layer 3: 项目知识（只读，跨会话共享）
      projectKnowledge: this.projectKnowledge.getSummary(),

      // Layer 2: 会话 Memory（会话独立）
      memory: this.buildMemoryFromSession(session),

      // Layer 1: 即时上下文（会话独立）
      immediateContext: session?.messages.slice(-10) || []
    };
  }

  private buildMemoryFromSession(session: UnifiedSession | null): MemoryDocument {
    if (!session) return this.createEmptyMemory();

    return {
      currentTasks: session.tasks.filter(t => t.status !== 'completed'),
      completedTasks: session.tasks.filter(t => t.status === 'completed'),
      keyDecisions: this.extractDecisions(session.messages),
      codeChanges: this.extractCodeChanges(session.snapshots),
      // ...
    };
  }
}
```

---

## 🎯 实施计划

### **Phase 1: 前后端集成完善**（优先级：高）

**目标**：让现有的会话管理功能完全可用

#### Task 1.1: 完善 UI 会话列表渲染
- [ ] 实现 `renderSessionList()` 函数
- [ ] 添加会话列表样式（CSS）
- [ ] 实现会话切换动画
- [ ] 测试会话列表显示

#### Task 1.2: 完善 WebviewProvider 消息处理
- [ ] 添加 `newSession` 消息处理
- [ ] 添加 `switchSession` 消息处理
- [ ] 添加 `renameSession` 消息处理
- [ ] 添加 `deleteSession` 消息处理
- [ ] 添加 `listSessions` 消息处理
- [ ] 实现会话列表推送

#### Task 1.3: 会话状态同步
- [ ] 前端状态管理（`state.js`）
- [ ] 会话切换时清空当前消息
- [ ] 会话切换时加载历史消息
- [ ] 会话创建时更新 UI

**成功标准**：
- ✅ 可以通过 UI 创建新会话
- ✅ 可以通过 UI 切换会话
- ✅ 可以通过 UI 重命名会话
- ✅ 可以通过 UI 删除会话
- ✅ 会话列表实时更新

**预计时间**：2-3 天

---

### **Phase 2: 项目级知识库**（优先级：中）

**目标**：实现跨会话的项目知识共享

#### Task 2.1: 实现 ProjectKnowledgeBase
- [ ] 创建 `src/knowledge/project-knowledge-base.ts`
- [ ] 实现代码索引（简单版：文件列表）
- [ ] 实现项目配置检测
- [ ] 实现 ADR 存储
- [ ] 实现 FAQ 存储

#### Task 2.2: 集成到 ContextManager
- [ ] 修改 ContextManager 构造函数
- [ ] 在 `getContext()` 中包含项目知识
- [ ] 测试项目知识注入

#### Task 2.3: UI 支持（可选）
- [ ] 添加"保存到项目记忆"按钮
- [ ] 添加项目知识查看面板
- [ ] 添加 ADR 列表显示

**成功标准**：
- ✅ 项目知识库可以存储和读取
- ✅ 新会话可以访问项目知识
- ✅ 用户可以主动保存重要信息

**预计时间**：3-5 天

---

### **Phase 3: ContextManager 与 SessionManager 深度集成**（优先级：中）

**目标**：让 ContextManager 使用 UnifiedSessionManager 的数据

#### Task 3.1: 重构 ContextManager
- [ ] 移除独立的消息存储
- [ ] 使用 `sessionManager.addMessage()`
- [ ] 使用 `sessionManager.getRecentMessages()`
- [ ] 使用 `sessionManager.getRecentMessagesWithinTokenBudget()`

#### Task 3.2: 统一 Memory 构建
- [ ] 从 Session 数据构建 MemoryDocument
- [ ] 自动提取任务、决策、代码变更
- [ ] 保持与现有 Memory 格式兼容

#### Task 3.3: Worker 历史管理集成
- [ ] WorkerLLMAdapter 使用 SessionManager
- [ ] 移除独立的 `conversationHistory`
- [ ] 统一上下文管理

**成功标准**：
- ✅ ContextManager 不再独立存储消息
- ✅ 所有消息通过 SessionManager 管理
- ✅ Worker 和 Orchestrator 使用统一的会话数据

**预计时间**：3-5 天

---

### **Phase 4: 优化和增强**（优先级：低）

#### Task 4.1: 会话搜索
- [ ] 按标题搜索
- [ ] 按内容搜索
- [ ] 按时间过滤

#### Task 4.2: 会话导出
- [ ] 导出为 JSON
- [ ] 导出为 Markdown
- [ ] 导出为 PDF（可选）

#### Task 4.3: 会话统计
- [ ] Token 使用统计
- [ ] 消息数统计
- [ ] 时间统计

#### Task 4.4: 自动归档
- [ ] 归档 7 天前的会话
- [ ] 压缩归档文件
- [ ] 清理 30 天前的归档

**预计时间**：1-2 周

---

## 🎨 UI 设计细节

### 会话选择器（已有基础）

```html
<!-- 当前 UI 结构 -->
<div class="session-selector" id="session-selector">
  <button class="session-selector-btn" id="session-selector-btn">
    <svg class="session-selector-icon">...</svg>
    <span class="session-selector-name" id="current-session-name">新会话</span>
    <svg class="session-selector-chevron">...</svg>
  </button>

  <div class="session-dropdown" id="session-dropdown">
    <div class="session-dropdown-header">
      <span class="session-dropdown-title">会话历史</span>
      <button class="icon-btn-sm" id="new-session-dropdown-btn" title="新建会话">
        <svg>...</svg>
      </button>
    </div>

    <!-- 需要渲染的会话列表 -->
    <div class="session-list" id="session-list"></div>

    <div class="session-dropdown-empty" id="session-empty">
      <svg>...</svg>
      <span>暂无会话历史</span>
    </div>
  </div>
</div>
```

### 会话列表项设计

```html
<div class="session-item active">
  <div class="session-item-header">
    <span class="session-item-name">添加用户认证功能</span>
    <span class="session-item-time">2小时前</span>
  </div>
  <div class="session-item-preview">帮我实现 JWT 认证...</div>
  <div class="session-item-meta">
    <span>💬 45 条消息</span>
    <span>📊 12.5K tokens</span>
  </div>
  <div class="session-item-actions">
    <button title="重命名">✏️</button>
    <button title="删除">🗑️</button>
  </div>
</div>
```

### CSS 样式（需要添加）

```css
/* 会话列表 */
.session-list {
  max-height: 400px;
  overflow-y: auto;
}

.session-item {
  padding: 12px;
  border-bottom: 1px solid var(--border-color);
  cursor: pointer;
  transition: background-color 0.2s;
}

.session-item:hover {
  background-color: var(--hover-bg);
}

.session-item.active {
  background-color: var(--active-bg);
  border-left: 3px solid var(--primary-color);
}

.session-item-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 4px;
}

.session-item-name {
  font-weight: 500;
  font-size: 14px;
}

.session-item-time {
  font-size: 12px;
  color: var(--text-secondary);
}

.session-item-preview {
  font-size: 12px;
  color: var(--text-secondary);
  margin-bottom: 8px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.session-item-meta {
  display: flex;
  gap: 12px;
  font-size: 11px;
  color: var(--text-tertiary);
}

.session-item-actions {
  display: none;
  gap: 4px;
  margin-top: 8px;
}

.session-item:hover .session-item-actions {
  display: flex;
}
```

---

## 📊 数据流设计

### 创建新会话

```
用户点击"新建会话"
  ↓
前端: handleNewSession()
  ↓
发送消息: { type: 'newSession' }
  ↓
后端: WebviewProvider.handleNewSession()
  ↓
调用: sessionManager.createSession()
  ↓
创建会话数据并保存到磁盘
  ↓
发送消息: { type: 'sessionCreated', session: {...} }
  ↓
前端: 更新 currentSessionId
  ↓
前端: 清空当前消息列表
  ↓
前端: 更新 UI 显示
```

### 切换会话

```
用户点击会话列表项
  ↓
前端: handleSessionSelect(sessionId)
  ↓
发送消息: { type: 'switchSession', sessionId }
  ↓
后端: WebviewProvider.handleSwitchSession()
  ↓
调用: sessionManager.switchSession(sessionId)
  ↓
加载会话数据
  ↓
发送消息: { type: 'sessionSwitched', sessionId, messages: [...] }
  ↓
前端: 更新 currentSessionId
  ↓
前端: 替换消息列表
  ↓
前端: 重新渲染对话内容
  ↓
前端: 关闭下拉菜单
```

---

## 🔧 技术细节

### 会话 ID 生成

```typescript
function generateSessionId(): string {
  const timestamp = Date.now();
  const random = Math.random().toString(36).substring(2, 9);
  return `session-${timestamp}-${random}`;
}

// 示例: session-1737532800000-a3f9k2x
```

### 会话标题自动生成

```typescript
private generateSessionTitle(firstMessage: string): string {
  let text = firstMessage.trim()
    .replace(/\n+/g, ' ')
    .replace(/\s+/g, ' ');

  // 移除冗余前缀
  const prefixes = [
    /^(请|帮我|帮忙|能不能|可以|麻烦|我想|我要|我需要)/,
    /^(please|can you|could you|help me)/i
  ];
  for (const p of prefixes) {
    text = text.replace(p, '').trim();
  }

  // 移除末尾语气词
  const suffixes = [/(吗|呢|吧|啊|谢谢|thanks)[\s。？?！!]*$/i];
  for (const s of suffixes) {
    text = text.replace(s, '').trim();
  }

  return text.length <= 100 ? text : text.substring(0, 100) + '...';
}

// 示例:
// 输入: "请帮我添加用户认证功能，谢谢"
// 输出: "添加用户认证功能"
```

### Token 预算管理

```typescript
// 获取在 token 预算内的最近消息
const messages = sessionManager.getRecentMessagesWithinTokenBudget(8000);

// 估算规则：
// - 英文: ~4 字符/token
// - 中文: ~1.5 字符/token
// - 平均: ~3 字符/token
// - 元数据开销: 20 tokens/message
```

---

## 🎯 成功标准

### Phase 1 完成标准
- [ ] 可以通过 UI 创建新会话
- [ ] 可以通过 UI 切换会话
- [ ] 可以通过 UI 重命名会话
- [ ] 可以通过 UI 删除会话
- [ ] 会话列表实时更新
- [ ] 会话切换时正确加载历史消息
- [ ] 会话数据持久化到磁盘

### Phase 2 完成标准
- [ ] 项目知识库可以存储和读取
- [ ] 新会话可以访问项目知识
- [ ] 用户可以主动保存重要信息到项目记忆
- [ ] 项目知识在上下文中正确注入

### Phase 3 完成标准
- [ ] ContextManager 不再独立存储消息
- [ ] 所有消息通过 SessionManager 管理
- [ ] Worker 和 Orchestrator 使用统一的会话数据
- [ ] Memory 从 Session 数据自动构建

---

## 📝 下一步行动

### 立即开始（本周）
1. ✅ 完成本文档
2. 🔧 实现 UI 会话列表渲染
3. 🔧 完善 WebviewProvider 消息处理
4. 🧪 测试会话切换功能

### 短期目标（本月）
1. 🔧 完成 Phase 1 所有任务
2. 📊 收集用户反馈
3. 🐛 修复发现的问题

### 中期目标（下季度）
1. 🚀 完成 Phase 2 项目知识库
2. 🔗 完成 Phase 3 深度集成
3. 📢 发布新版本

---

## 🤔 需要确认的问题

1. **是否立即开始 Phase 1？**
   - 优先完善现有功能，让会话管理完全可用

2. **项目知识库的范围？**
   - 先实现简单版本（文件索引 + ADR + FAQ）
   - 后续再考虑语义检索（已有 Augment ACE）

3. **是否需要会话导出功能？**
   - 可以放到 Phase 4，不是核心功能

4. **会话归档策略？**
   - 建议：7 天后自动归档，30 天后清理
   - 用户可以手动标记"重要会话"不归档

---

**文档版本**: 1.0
**最后更新**: 2025-01-22
**作者**: AI Assistant
**状态**: 待审阅
