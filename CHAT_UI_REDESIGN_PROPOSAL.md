# MultiCLI 对话界面重构设计方案

## 📊 现状分析

### 当前实现的问题

1. **信息层级混乱**
   - 消息、工具调用、thinking、代码块混在一起
   - 没有清晰的视觉层级，用户难以快速定位关键信息
   - 1450行CSS + 3065行JS，代码量大但缺乏系统性

2. **设计不统一**
   - 颜色、间距、圆角使用不一致
   - 多处硬编码的样式值
   - 缺少统一的设计语言

3. **特殊内容展示不专业**
   - Thinking占据过多空间，抢夺主要内容注意力
   - 工具调用缺少清晰的输入/输出/状态展示
   - 代码块交互单一，只有复制功能

4. **交互能力不足**
   - 缺少消息级操作（复制、重新生成、编辑）
   - 代码块缺少"应用到编辑器"等VSCode集成功能
   - 没有快捷键支持

5. **技术债务**
   - 历史遗留代码多（如刚修复的空code标签问题）
   - 样式耦合严重，难以维护
   - 缺少组件化思维

---

## 🎯 优秀案例研究

### 1. Claude.ai (Web版)

**优点：**
- ✅ 消息气泡清晰，用户/AI有明显视觉区分
- ✅ Thinking用轻量级可折叠面板，默认折叠，有智能摘要
- ✅ 工具调用用独立卡片，有图标、状态指示、输入输出分离
- ✅ 代码块有语言标签、复制按钮、下载功能
- ✅ 流式输出有光标动画，视觉反馈清晰
- ✅ 引用和链接高亮明显

**核心设计原则：**
- 渐进式披露：次要信息默认隐藏
- 视觉层级：主要内容 > 辅助信息 > 元数据
- 操作就近：悬停显示操作按钮

### 2. Cursor

**优点：**
- ✅ 紧凑型设计，适合侧边栏
- ✅ 代码diff直接内联显示，一目了然
- ✅ 文件引用可点击跳转，与编辑器深度集成
- ✅ 多轮对话有清晰分割，易于追溯
- ✅ Apply按钮直接应用代码到编辑器

**核心设计原则：**
- 编辑器集成：充分利用VSCode能力
- 紧凑高效：适合窄窗口
- 操作导向：快捷操作突出

### 3. GitHub Copilot Chat

**优点：**
- ✅ 原生VSCode体验，无违和感
- ✅ 简洁的消息样式，不喧宾夺主
- ✅ 代码块可直接插入到编辑器
- ✅ 上下文显示清晰（@file、@workspace）
- ✅ 快捷键支持完善

**核心设计原则：**
- 原生感：融入VSCode
- 简洁：只展示必要信息
- 快捷：键盘优先

### 4. Continue.dev (开源)

**优点：**
- ✅ 开源可参考实现细节
- ✅ 上下文管理清晰（文件、选择、终端输出）
- ✅ 工具调用有详细的日志
- ✅ 代码块有diff模式

---

## 🏗️ 新设计方案

### 核心设计原则

1. **清晰的信息层级**
   - 主要内容：用户问题、AI回答
   - 辅助信息：Thinking、工具调用、上下文
   - 元数据：时间、状态、模型信息

2. **一致的设计语言**
   - 统一的颜色系统
   - 统一的间距系统
   - 统一的圆角和阴影
   - 统一的动画

3. **渐进式披露**
   - Thinking默认折叠，显示智能摘要
   - 工具调用可展开查看详情
   - 长代码块自动折叠

4. **快捷操作**
   - 消息级：复制、重新生成、编辑
   - 代码块：复制、应用、插入、查看diff
   - 全局：快捷键支持

5. **上下文感知**
   - 文件路径可点击跳转
   - 代码与编辑器集成
   - 显示引用的上下文

---

## 📐 设计系统

### 1. 颜色系统

```css
/* 主色调 - 角色标识 */
--color-user: #3b82f6;           /* 蓝色 - 用户 */
--color-assistant: #8b5cf6;      /* 紫色 - AI */
--color-orchestrator: #f59e0b;   /* 橙色 - 编排者 */
--color-worker: #10b981;         /* 绿色 - Worker */

/* 状态色 */
--color-success: #22c55e;
--color-error: #ef4444;
--color-warning: #f59e0b;
--color-info: #3b82f6;
--color-pending: #6b7280;

/* 语义色 */
--color-thinking: rgba(139, 92, 246, 0.1);  /* Thinking背景 */
--color-tool: rgba(16, 185, 129, 0.1);      /* 工具调用背景 */
--color-code: rgba(100, 116, 139, 0.1);     /* 代码块背景 */

/* 中性色 - 继承VSCode */
--color-bg-primary: var(--vscode-editor-background);
--color-bg-secondary: var(--vscode-sideBar-background);
--color-bg-hover: var(--vscode-list-hoverBackground);
--color-border: var(--vscode-panel-border);
--color-text-primary: var(--vscode-foreground);
--color-text-secondary: var(--vscode-descriptionForeground);
```

### 2. 间距系统（4px基础单位）

```css
--spacing-0: 0;
--spacing-1: 4px;
--spacing-2: 8px;
--spacing-3: 12px;
--spacing-4: 16px;
--spacing-5: 20px;
--spacing-6: 24px;
--spacing-8: 32px;
--spacing-10: 40px;
--spacing-12: 48px;
```

### 3. 圆角系统

```css
--radius-none: 0;
--radius-sm: 4px;
--radius-md: 6px;
--radius-lg: 8px;
--radius-xl: 12px;
--radius-2xl: 16px;
--radius-full: 9999px;
```

### 4. 阴影系统

```css
--shadow-sm: 0 1px 2px rgba(0, 0, 0, 0.05);
--shadow-md: 0 4px 6px rgba(0, 0, 0, 0.1);
--shadow-lg: 0 10px 15px rgba(0, 0, 0, 0.1);
--shadow-xl: 0 20px 25px rgba(0, 0, 0, 0.15);
```

### 5. Z-index系统

```css
--z-base: 0;
--z-message: 10;
--z-sticky-header: 50;
--z-dropdown: 100;
--z-modal: 200;
--z-tooltip: 300;
--z-notification: 400;
```

### 6. 动画系统

```css
--duration-fast: 150ms;
--duration-normal: 250ms;
--duration-slow: 350ms;

--easing-standard: cubic-bezier(0.4, 0, 0.2, 1);
--easing-decelerate: cubic-bezier(0, 0, 0.2, 1);
--easing-accelerate: cubic-bezier(0.4, 0, 1, 1);
```

---

## 🧩 组件架构

### 消息结构

```
Message
├── MessageHeader (角色、时间、状态)
│   ├── Avatar / Badge
│   ├── Metadata (时间、模型)
│   └── Actions (复制、重新生成、编辑)
├── MessageThinking (可折叠，默认折叠)
│   ├── ThinkingSummary (智能摘要)
│   └── ThinkingContent (完整内容)
├── MessageContent (主要内容)
│   ├── Markdown渲染
│   ├── CodeBlocks (独立组件)
│   └── FileReferences (可点击)
├── MessageToolCalls (工具调用列表)
│   └── ToolCallCard[]
│       ├── ToolHeader (名称、状态)
│       ├── ToolInput (可折叠)
│       └── ToolOutput (可折叠)
└── MessageFooter (元数据、反馈)
```

### 核心组件

#### 1. MessageThinking 组件

**设计要求：**
- 默认折叠，只显示一行摘要
- 摘要智能生成：提取第一句话或关键信息
- 折叠状态显示思考步数
- 流式时自动展开，完成后自动折叠
- 使用轻量级样式，不抢主内容注意力

**样式规范：**
```css
.message-thinking {
  background: var(--color-thinking);
  border-left: 2px solid var(--color-assistant);
  border-radius: var(--radius-md);
  margin: var(--spacing-2) 0;
}

.thinking-header {
  display: flex;
  align-items: center;
  gap: var(--spacing-2);
  padding: var(--spacing-2) var(--spacing-3);
  cursor: pointer;
  user-select: none;
}

.thinking-summary {
  flex: 1;
  color: var(--color-text-secondary);
  font-size: 0.9em;
  font-style: italic;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.thinking-badge {
  font-size: 0.75em;
  padding: 2px 6px;
  background: var(--color-assistant);
  color: white;
  border-radius: var(--radius-sm);
}
```

#### 2. ToolCallCard 组件

**设计要求：**
- 卡片式设计，有清晰的边界
- 显示工具图标、名称、状态
- 输入/输出分离展示
- 支持展开/折叠
- 状态用颜色标识（pending/running/success/error）

**样式规范：**
```css
.tool-call-card {
  background: var(--color-bg-secondary);
  border: 1px solid var(--color-border);
  border-left: 3px solid var(--color-tool);
  border-radius: var(--radius-md);
  margin: var(--spacing-2) 0;
  overflow: hidden;
}

.tool-call-header {
  display: flex;
  align-items: center;
  gap: var(--spacing-2);
  padding: var(--spacing-3);
  background: var(--color-bg-primary);
}

.tool-call-icon {
  width: 16px;
  height: 16px;
  flex-shrink: 0;
}

.tool-call-status {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}

.tool-call-status.running {
  background: var(--color-info);
  animation: pulse 1.5s infinite;
}

.tool-call-status.success {
  background: var(--color-success);
}

.tool-call-status.error {
  background: var(--color-error);
}
```

#### 3. CodeBlock 组件

**设计要求：**
- 语法高亮
- 顶部工具栏：语言标签、文件路径、操作按钮
- 操作按钮：复制、应用、插入、查看diff
- 超过20行自动折叠
- 折叠时显示前5行和后2行

**样式规范：**
```css
.code-block {
  background: var(--color-code);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  margin: var(--spacing-3) 0;
  overflow: hidden;
}

.code-block-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: var(--spacing-2) var(--spacing-3);
  background: var(--color-bg-secondary);
  border-bottom: 1px solid var(--color-border);
}

.code-block-info {
  display: flex;
  align-items: center;
  gap: var(--spacing-3);
}

.code-block-lang {
  font-size: 0.75em;
  font-weight: 600;
  text-transform: uppercase;
  color: var(--color-text-secondary);
}

.code-block-filepath {
  font-size: 0.85em;
  color: var(--color-info);
  cursor: pointer;
  text-decoration: underline;
  text-decoration-style: dotted;
}

.code-block-actions {
  display: flex;
  gap: var(--spacing-1);
}

.code-action-btn {
  padding: var(--spacing-1) var(--spacing-2);
  font-size: 0.75em;
  border-radius: var(--radius-sm);
  background: transparent;
  border: 1px solid var(--color-border);
  cursor: pointer;
  transition: all var(--duration-fast);
}

.code-action-btn:hover {
  background: var(--color-bg-hover);
  border-color: var(--color-info);
}
```

---

## 🎨 视觉优化建议

### 1. 消息区分

**用户消息：**
- 右对齐，蓝色气泡
- 最大宽度85%
- 圆角：左上/左下/右下为12px，右上为4px

**AI消息：**
- 左对齐，无气泡
- 全宽显示
- 浅色背景（可选）

### 2. 内容间距

```
Message
  ↕ 16px
Message
  ↕ 16px
Message
```

同一来源连续消息：
```
Message (Claude)
  ↕ 4px
Message (Claude, grouped)
  ↕ 16px
Message (User)
```

### 3. 折叠策略

**Thinking：**
- 默认折叠
- 流式时展开
- 完成后自动折叠

**工具调用：**
- 输入：默认折叠
- 输出：默认展开（如果有内容）
- 错误：自动展开

**代码块：**
- \< 20行：全部展开
- \>= 20行：折叠显示前5行+后2行
- 用户点击展开后记住状态

### 4. 交互反馈

**悬停效果：**
- 消息悬停：显示操作按钮
- 代码块悬停：高亮边框
- 链接悬停：下划线+颜色变化

**点击反馈：**
- 按钮：scale(0.95)
- 复制成功：✓ 图标 + 提示
- 应用成功：编辑器闪烁高亮

**流式效果：**
- 光标动画（闪烁）
- 内容淡入
- 滚动跟随（智能：用户滚动时暂停）

---

## 🚀 实施计划

### 阶段一：建立设计系统（1-2天）

1. **创建设计系统文件**
   - `styles/design-system.css` - 变量定义
   - `styles/tokens.css` - 语义化token

2. **重构现有变量**
   - 统一颜色变量
   - 统一间距变量
   - 删除硬编码值

### 阶段二：核心组件重构（3-4天）

1. **MessageThinking组件**
   - 重写HTML结构
   - 重写CSS样式
   - 实现折叠逻辑
   - 添加智能摘要

2. **ToolCallCard组件**
   - 设计新的卡片结构
   - 实现状态指示
   - 优化输入/输出展示
   - 添加展开/折叠

3. **CodeBlock组件**
   - 增强工具栏
   - 添加操作按钮
   - 实现智能折叠
   - 集成VSCode能力

### 阶段三：消息结构优化（2-3天）

1. **重构消息渲染逻辑**
   - 分离关注点
   - 组件化拆分
   - 优化DOM结构

2. **优化布局和间距**
   - 统一消息间距
   - 优化内容层级
   - 响应式调整

### 阶段四：交互增强（2-3天）

1. **添加消息操作**
   - 复制消息
   - 重新生成
   - 编辑消息

2. **添加代码操作**
   - 应用到编辑器
   - 插入到光标位置
   - 查看diff
   - 创建新文件

3. **添加快捷键**
   - 定义快捷键映射
   - 实现快捷键处理
   - 添加提示

### 阶段五：打磨和优化（1-2天）

1. **性能优化**
   - 虚拟滚动（如需要）
   - 图片懒加载
   - 代码高亮优化

2. **动画优化**
   - 流式动画
   - 折叠动画
   - 过渡效果

3. **无障碍支持**
   - 键盘导航
   - ARIA标签
   - 焦点管理

---

## 📝 代码组织建议

### 新的文件结构

```
src/ui/webview/
├── styles/
│   ├── design-system.css          # 设计系统变量
│   ├── tokens.css                 # 语义化token
│   ├── base.css                   # 基础样式
│   ├── layout.css                 # 布局
│   └── components/
│       ├── message.css            # 消息组件
│       ├── thinking.css           # Thinking组件
│       ├── tool-call.css          # 工具调用组件
│       ├── code-block.css         # 代码块组件
│       └── common.css             # 通用组件
├── js/
│   ├── core/
│   │   ├── design-system.js       # 设计系统工具
│   │   └── ...
│   └── components/
│       ├── MessageRenderer.js     # 消息渲染器
│       ├── ThinkingPanel.js       # Thinking面板
│       ├── ToolCallCard.js        # 工具调用卡片
│       └── CodeBlock.js           # 代码块
```

### 组件化示例

```javascript
// components/ThinkingPanel.js
export class ThinkingPanel {
  constructor(thinkingData, options = {}) {
    this.data = thinkingData;
    this.collapsed = options.collapsed ?? true;
    this.summary = this.generateSummary();
  }

  generateSummary() {
    // 智能摘要生成逻辑
    const firstSentence = this.extractFirstSentence(this.data.content);
    return firstSentence || `${this.data.steps.length} 步思考过程`;
  }

  render() {
    return `
      <div class="thinking-panel ${this.collapsed ? 'collapsed' : 'expanded'}">
        <div class="thinking-header" onclick="toggleThinking('${this.id}')">
          <span class="thinking-icon">💭</span>
          <span class="thinking-summary">${this.summary}</span>
          <span class="thinking-badge">${this.data.steps.length} 步</span>
          <span class="thinking-chevron">›</span>
        </div>
        <div class="thinking-content">
          ${this.renderContent()}
        </div>
      </div>
    `;
  }

  renderContent() {
    // 渲染思考内容
  }
}
```

---

## 🎯 成功指标

1. **视觉一致性**
   - ✅ 所有组件使用统一的设计系统
   - ✅ 间距、圆角、颜色完全统一
   - ✅ 无硬编码样式值

2. **信息层级**
   - ✅ 用户能在3秒内定位到主要信息
   - ✅ Thinking不抢主要内容注意力
   - ✅ 工具调用清晰可见但不突兀

3. **交互体验**
   - ✅ 所有常用操作可通过快捷键完成
   - ✅ 代码可一键应用到编辑器
   - ✅ 流式输出流畅无卡顿

4. **代码质量**
   - ✅ CSS行数减少30%
   - ✅ JS模块化，单一职责
   - ✅ 无重复代码

5. **性能**
   - ✅ 100条消息渲染时间 < 100ms
   - ✅ 流式更新延迟 < 50ms
   - ✅ 无内存泄漏

---

## 💡 创新点

1. **智能折叠**
   - 基于内容长度自动决定是否折叠
   - 记住用户的折叠偏好
   - 流式时智能展开/折叠

2. **上下文可视化**
   - 显示引用的文件、代码片段
   - 可点击跳转到源位置
   - 高亮相关上下文

3. **代码协作**
   - 代码diff模式
   - 一键应用更改
   - 创建新文件
   - 批量操作多个代码块

4. **个性化**
   - 保存用户偏好（折叠、间距等）
   - 主题定制
   - 布局调整

---

## 🔄 迁移策略

1. **渐进式重构**
   - 先建立设计系统
   - 逐个组件迁移
   - 保持向后兼容

2. **A/B测试**
   - 新旧UI并存
   - 用户可切换
   - 收集反馈

3. **数据迁移**
   - 旧消息数据兼容
   - 自动转换格式
   - 无缝过渡

---

## 📚 参考资料

- [Radix Design System](https://www.radix-ui.com/themes/docs/overview/getting-started)
- [Tailwind CSS](https://tailwindcss.com/docs)
- [Material Design 3](https://m3.material.io/)
- [VSCode Design Guidelines](https://code.visualstudio.com/api/extension-guides/webview#design-guidelines)
