# 知识库数据来源和注入机制说明

## 概述

MultiCLI 的知识库包含三类数据，每类数据的来源和注入方式都不同。

---

## 1. 数据来源详解

### 1.1 代码索引（Code Index）- 🤖 完全自动

**数据来源：** 项目文件系统扫描

**生成时机：**
- ✅ 扩展激活时自动初始化知识库
- ✅ 首次访问知识库 Tab 时自动索引
- ✅ 点击刷新按钮时重新索引

**包含内容：**
```typescript
{
  files: FileEntry[];           // 所有源代码文件
  directories: DirectoryEntry[]; // 目录结构
  techStack: string[];          // 自动检测的技术栈
  dependencies: Record<string, string>; // package.json 中的依赖
  entryPoints: string[];        // 识别的入口文件
  lastIndexed: number;          // 最后索引时间
}
```

**自动检测逻辑：**
- 扫描项目所有文件（排除 node_modules、.git 等）
- 根据文件扩展名分类（.ts, .js, .vue, .css 等）
- 读取 package.json 检测技术栈
- 识别入口文件（index.ts, main.ts, app.ts 等）
- 统计文件数、代码行数

**存储位置：** `.multicli/knowledge/code-index.json` (239KB)

**用户操作：** 无需任何操作，完全自动

---

### 1.2 架构决策记录（ADR）- 🔄 半自动 + 手动

**当前状态：** ⚠️ 仅有测试数据，无真实项目决策

**数据来源：**

#### 方式 1：从会话中自动提取（已实现，未启用）

**实现位置：** `src/knowledge/project-knowledge-base.ts:615-643`

```typescript
async extractADRFromSession(messages: Array<{ role: string; content: string }>): Promise<ADRRecord[]> {
  // 使用 LLM 分析会话内容
  // 识别关键决策（包含关键词：决定、选择、采用、使用、方案、架构等）
  // 提取决策的背景、内容、影响、替代方案
  // 返回 ADR 记录
}
```

**触发机制：** ❌ 目前没有自动触发点

**问题：**
- 后端方法已实现
- 但没有 UI 入口调用
- 没有自动触发逻辑（如会话结束时）

#### 方式 2：手动添加（已实现后端，缺少 UI）

**实现位置：** `src/ui/webview-provider.ts:3196-3224`

```typescript
case 'addADR':
  await this.handleAddADR(message.adr);
  break;
```

**问题：**
- 后端接口已实现
- 但前端没有添加 ADR 的表单 UI
- 用户无法通过界面添加

**当前数据：**
```json
[
  {
    "id": "adr-001",
    "title": "使用 TypeScript 开发",
    "status": "deprecated",
    "context": "需要类型安全和更好的 IDE 支持",
    "decision": "采用 TypeScript 作为主要开发语言",
    "consequences": "需要编译步骤，但提高了代码质量和可维护性"
  }
]
```

这些是**测试数据**，不是真实的项目决策。

---

### 1.3 常见问题（FAQ）- 🔄 半自动 + 手动

**当前状态：** ⚠️ 仅有测试数据，无真实项目问题

**数据来源：**

#### 方式 1：从会话中自动提取（已实现，未启用）

**实现位置：** `src/knowledge/project-knowledge-base.ts:649-677`

```typescript
async extractFAQFromSession(messages: Array<{ role: string; content: string }>): Promise<FAQRecord[]> {
  // 使用 LLM 分析会话内容
  // 识别常见问题（包含关键词：如何、怎么、为什么、问题、错误等）
  // 提取问题和答案
  // 返回 FAQ 记录
}
```

**触发机制：** ❌ 目前没有自动触发点

#### 方式 2：手动添加（已实现后端，缺少 UI）

**实现位置：** `src/ui/webview-provider.ts` (类似 ADR)

**问题：**
- 后端接口已实现
- 但前端没有添加 FAQ 的表单 UI
- 用户无法通过界面添加

**当前数据：**
```json
[
  {
    "id": "faq-001",
    "question": "如何调试 VSCode 扩展？",
    "answer": "按 F5 启动调试，在 Extension Development Host 中测试。",
    "category": "development",
    "tags": ["debug", "vscode", "extension"],
    "useCount": 1
  }
]
```

这些是**测试数据**，不是真实的项目问题。

---

## 2. 初始化流程

### 2.1 扩展激活时

```
VS Code 激活扩展
    ↓
WebViewProvider.constructor()
    ↓
initializeProjectKnowledgeBase()
    ↓
new ProjectKnowledgeBase({ projectRoot })
    ↓
projectKnowledgeBase.initialize()
    ↓
├─ ensureStorageDir()           # 创建 .multicli/knowledge/
├─ loadCodeIndex()              # 加载已有索引
├─ loadADRs()                   # 加载已有 ADR
├─ loadFAQs()                   # 加载已有 FAQ
└─ 如果没有索引 → indexProject() # 首次索引
```

**代码位置：**
- `src/ui/webview-provider.ts:255-272`
- `src/knowledge/project-knowledge-base.ts:171-192`

### 2.2 用户切换到知识库 Tab

```
用户点击"知识库" Tab
    ↓
event-handlers.js: handleTopTabClick('knowledge')
    ↓
knowledge-handler.js: loadProjectKnowledge()
    ↓
发送消息: { type: 'getProjectKnowledge' }
    ↓
webview-provider.ts: handleGetProjectKnowledge()
    ↓
返回数据: { codeIndex, adrs, faqs }
    ↓
前端渲染 UI
```

### 2.3 用户点击刷新按钮

```
用户点击刷新按钮
    ↓
isKnowledgeLoaded = false
    ↓
loadProjectKnowledge()
    ↓
重新加载所有数据
```

---

## 3. 当前实际状态

### 3.1 已有数据文件

```bash
$ ls -la .multicli/knowledge/
total 488
-rw-r--r--  1 xie  staff    3028 Jan 22 19:20 adrs.json
-rw-r--r--  1 xie  staff  239247 Jan 22 18:48 code-index.json
-rw-r--r--  1 xie  staff    2909 Jan 22 19:20 faqs.json
```

### 3.2 数据质量分析

| 数据类型 | 文件大小 | 数据来源 | 质量评估 |
|---------|---------|---------|---------|
| 代码索引 | 239KB | ✅ 自动扫描 | ✅ 真实有效 |
| ADR | 3KB | ⚠️ 测试数据 | ❌ 非真实项目决策 |
| FAQ | 3KB | ⚠️ 测试数据 | ❌ 非真实项目问题 |

### 3.3 问题总结

1. **代码索引**：✅ 完全正常，自动生成真实数据
2. **ADR**：❌ 只有测试数据，缺少真实项目决策
3. **FAQ**：❌ 只有测试数据，缺少真实项目问题

---

## 4. 缺失的功能

### 4.1 自动提取 ADR/FAQ 的触发机制

**已实现的方法：**
- ✅ `extractADRFromSession(messages)`
- ✅ `extractFAQFromSession(messages)`

**缺失的触发点：**
- ❌ 会话结束时自动提取
- ❌ 用户手动触发提取的按钮
- ❌ 定期分析会话历史

**建议实现：**

#### 方案 1：会话结束时自动提取
```typescript
// 在会话管理器中
async onSessionEnd(sessionId: string) {
  const messages = this.getSessionMessages(sessionId);
  
  // 提取 ADR
  const adrs = await this.knowledgeBase.extractADRFromSession(messages);
  if (adrs.length > 0) {
    // 提示用户确认
    this.showADRConfirmationDialog(adrs);
  }
  
  // 提取 FAQ
  const faqs = await this.knowledgeBase.extractFAQFromSession(messages);
  if (faqs.length > 0) {
    // 提示用户确认
    this.showFAQConfirmationDialog(faqs);
  }
}
```

#### 方案 2：添加手动提取按钮
```html
<!-- 在知识库 Tab 中 -->
<button id="extract-knowledge-btn">从当前会话提取知识</button>
```

### 4.2 手动添加 ADR/FAQ 的 UI

**缺失的 UI 组件：**
- ❌ 添加 ADR 的表单对话框
- ❌ 编辑 ADR 的表单对话框
- ❌ 添加 FAQ 的表单对话框
- ❌ 编辑 FAQ 的表单对话框
- ❌ 删除确认对话框

**建议实现：**

#### ADR 添加表单
```html
<div class="add-adr-dialog">
  <h3>添加架构决策记录</h3>
  <form>
    <input type="text" placeholder="决策标题" required />
    <select name="status">
      <option value="proposed">提议中</option>
      <option value="accepted">已接受</option>
      <option value="deprecated">已废弃</option>
      <option value="superseded">已替代</option>
    </select>
    <textarea placeholder="决策背景" required></textarea>
    <textarea placeholder="决策内容" required></textarea>
    <textarea placeholder="影响和后果" required></textarea>
    <textarea placeholder="替代方案（每行一个）"></textarea>
    <button type="submit">保存</button>
  </form>
</div>
```

#### FAQ 添加表单
```html
<div class="add-faq-dialog">
  <h3>添加常见问题</h3>
  <form>
    <input type="text" placeholder="问题" required />
    <textarea placeholder="答案" required></textarea>
    <input type="text" placeholder="分类" />
    <input type="text" placeholder="标签（逗号分隔）" />
    <button type="submit">保存</button>
  </form>
</div>
```

---

## 5. 用户操作指南

### 5.1 当前可用功能

#### ✅ 查看代码索引
1. 打开 MultiCLI 扩展
2. 切换到"知识库" Tab
3. 查看顶部统计栏：文件数、代码行数
4. 点击刷新按钮重新索引

#### ✅ 查看 ADR 列表
1. 切换到"知识库" Tab
2. 左侧显示 ADR 列表
3. 点击过滤按钮筛选状态
4. 点击卡片展开查看详情

#### ✅ 搜索 FAQ
1. 切换到"知识库" Tab
2. 右侧显示 FAQ 列表
3. 在搜索框输入关键词
4. 点击卡片展开查看答案

### 5.2 当前不可用功能

#### ❌ 添加 ADR
**原因：** 缺少 UI 表单

**临时方案：** 手动编辑 `.multicli/knowledge/adrs.json`

```json
{
  "id": "adr-custom-001",
  "title": "你的决策标题",
  "date": 1737542400000,
  "status": "accepted",
  "context": "决策背景",
  "decision": "决策内容",
  "consequences": "影响和后果",
  "alternatives": ["方案1", "方案2"]
}
```

#### ❌ 添加 FAQ
**原因：** 缺少 UI 表单

**临时方案：** 手动编辑 `.multicli/knowledge/faqs.json`

```json
{
  "id": "faq-custom-001",
  "question": "你的问题",
  "answer": "你的答案",
  "category": "分类",
  "tags": ["标签1", "标签2"],
  "createdAt": 1737542400000,
  "updatedAt": 1737542400000,
  "useCount": 0
}
```

#### ❌ 从会话自动提取
**原因：** 缺少触发机制

**临时方案：** 无，需要开发实现

---

## 6. 开发建议

### 6.1 优先级 1：添加手动添加 UI

**工作量：** 4-6 小时

**任务：**
1. 创建 ADR 添加/编辑对话框
2. 创建 FAQ 添加/编辑对话框
3. 添加删除确认对话框
4. 连接前后端消息处理

**收益：**
- 用户可以手动添加真实的项目知识
- 替换当前的测试数据
- 提升知识库的实用价值

### 6.2 优先级 2：实现自动提取触发

**工作量：** 3-4 小时

**任务：**
1. 在会话结束时触发提取
2. 添加"从会话提取"按钮
3. 实现确认对话框
4. 自动关联相关文件

**收益：**
- 自动积累项目知识
- 减少手动输入工作
- 提升知识库的智能化

### 6.3 优先级 3：优化提取算法

**工作量：** 2-3 小时

**任务：**
1. 优化关键词识别
2. 改进 LLM 提示词
3. 添加去重逻辑
4. 提高提取准确率

**收益：**
- 提高自动提取质量
- 减少误报和漏报
- 提升用户体验

---

## 7. 总结

### 当前状态

| 功能 | 实现状态 | 数据质量 | 用户可用性 |
|------|---------|---------|-----------|
| 代码索引 | ✅ 完整 | ✅ 真实有效 | ✅ 完全可用 |
| ADR 查看 | ✅ 完整 | ❌ 测试数据 | ⚠️ 可查看但无真实数据 |
| FAQ 查看 | ✅ 完整 | ❌ 测试数据 | ⚠️ 可查看但无真实数据 |
| ADR 添加 | ⚠️ 后端完整 | - | ❌ 缺少 UI |
| FAQ 添加 | ⚠️ 后端完整 | - | ❌ 缺少 UI |
| 自动提取 | ⚠️ 方法已实现 | - | ❌ 缺少触发机制 |

### 关键问题

1. **ADR/FAQ 只有测试数据**
   - 不是真实的项目决策和问题
   - 需要用户手动添加或自动提取

2. **缺少添加 UI**
   - 后端接口已实现
   - 但用户无法通过界面添加

3. **自动提取未启用**
   - 提取方法已实现
   - 但没有触发机制

### 建议行动

**短期（1-2 天）：**
- 实现 ADR/FAQ 添加表单 UI
- 让用户可以手动添加真实数据

**中期（1 周）：**
- 实现自动提取触发机制
- 添加确认对话框
- 清理测试数据

**长期（持续）：**
- 优化提取算法
- 添加批量操作
- 实现协作功能

---

## 8. 快速参考

### 数据文件位置
```
.multicli/knowledge/
├── code-index.json    # 自动生成，真实有效
├── adrs.json          # 测试数据，需要替换
└── faqs.json          # 测试数据，需要替换
```

### 关键代码位置
```
后端：
- src/knowledge/project-knowledge-base.ts    # 核心业务逻辑
- src/ui/webview-provider.ts                 # 消息处理

前端：
- src/ui/webview/js/ui/knowledge-handler.js  # UI 逻辑
- src/ui/webview/index.html                  # UI 结构
- src/ui/webview/styles/components.css       # UI 样式
```

### 消息类型
```
前端 → 后端：
- getProjectKnowledge  # 获取所有知识
- getADRs             # 获取 ADR 列表
- getFAQs             # 获取 FAQ 列表
- searchFAQs          # 搜索 FAQ
- addADR              # 添加 ADR
- addFAQ              # 添加 FAQ

后端 → 前端：
- projectKnowledgeLoaded  # 知识加载完成
- adrsLoaded             # ADR 列表
- faqsLoaded             # FAQ 列表
- faqSearchResults       # 搜索结果
```

