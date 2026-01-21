# 配置面板 UX/UI 优化方案

## 📊 当前配置面板分析

### 现有 6 个 Tab 结构

| Tab | 当前内容 | 问题分析 |
|-----|---------|---------|
| **统计** | CLI 连接状态 + 执行统计 | ❌ "CLI 连接状态"命名过时（应为"模型连接状态"） |
| **画像** | Worker 画像配置 + 任务分类默认 | ✅ 内容合理，但缺少 LLM 配置 |
| **编排者** | 编排者模型配置 + 压缩模型配置 | ✅ 内容合理 |
| **MCP** | MCP 服务器列表（空状态） | ✅ 内容合理 |
| **技能** | 自定义技能列表 + 内置工具说明 | ✅ 内容合理 |
| **配置** | Augment 配置 | ⚠️ 内容过少，可以合并 |

---

## 🎯 优化目标

1. **逻辑分组清晰** - 相关配置放在一起
2. **减少 Tab 切换** - 常用配置集中
3. **信息层级合理** - 主要配置突出，次要配置收起
4. **保持现有元素** - 不删除任何功能
5. **符合用户心智模型** - 按照配置的"角色"分组

---

## 💡 优化方案

### 方案 A：按角色分组（推荐）⭐

**核心思路**: 按照"谁使用这个配置"来分组

#### 新的 4 Tab 结构

```
┌─────────────────────────────────────────────────────────┐
│  统计  │  模型  │  画像  │  工具  │
└─────────────────────────────────────────────────────────┘
```

#### 1. **统计 Tab** - 监控和数据
**内容**:
- ✅ 模型连接状态（改名，原 CLI 连接状态）
- ✅ 执行统计（编排者汇总 + Worker 统计）
- ✅ Token 使用统计

**优势**: 纯展示，无配置项，用户快速查看系统状态

---

#### 2. **模型 Tab** - 所有 LLM 配置
**内容**:
- **编排者模型配置**
  - Base URL, API Key, Model, Provider
  - Max Tokens, Temperature
  - 测试连接按钮

- **Worker 模型配置**（3 个子 Tab）
  - Claude 配置
  - Codex 配置
  - Gemini 配置
  - 每个包含：Base URL, API Key, Model, Provider, Enabled

- **压缩模型配置**（可折叠）
  - 启用/禁用开关
  - Base URL, API Key, Model, Provider

**布局**:
```
┌─────────────────────────────────────────────────────┐
│ 编排者模型配置                                        │
│ ┌─────────────────────────────────────────────────┐ │
│ │ Base URL: [https://api.anthropic.com         ] │ │
│ │ API Key:  [••••••••••••••••••••••••••] 👁      │ │
│ │ Model:    [claude-3-5-sonnet-20241022        ] │ │
│ │ Provider: [Anthropic ▼]                        │ │
│ │ [测试连接]                                      │ │
│ └─────────────────────────────────────────────────┘ │
│                                                     │
│ Worker 模型配置                                      │
│ ┌─────────────────────────────────────────────────┐ │
│ │ [Claude] [Codex] [Gemini]  ← Worker 选择器      │ │
│ │                                                 │ │
│ │ Base URL: [https://api.anthropic.com         ] │ │
│ │ API Key:  [••••••••••••••••••••••••••] 👁      │ │
│ │ Model:    [claude-3-5-sonnet-20241022        ] │ │
│ │ Provider: [Anthropic ▼]                        │ │
│ │ Enabled:  [✓] 启用此 Worker                     │ │
│ │ [测试连接]                                      │ │
│ └─────────────────────────────────────────────────┘ │
│                                                     │
│ 压缩模型配置 [▼]                                     │
│ ┌─────────────────────────────────────────────────┐ │
│ │ [✓] 启用压缩模型                                 │ │
│ │ Base URL: [https://api.anthropic.com         ] │ │
│ │ API Key:  [••••••••••••••••••••••••••] 👁      │ │
│ │ Model:    [claude-3-haiku-20240307           ] │ │
│ │ Provider: [Anthropic ▼]                        │ │
│ └─────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────┘
```

**优势**:
- 所有 LLM 配置集中在一起
- 用户配置模型时不需要切换 Tab
- 逻辑清晰：编排者 → Workers → 压缩器

---

#### 3. **画像 Tab** - Worker 行为配置
**内容**:
- ✅ Worker 选择器（Claude/Codex/Gemini）
- ✅ 角色定位
- ✅ 专注领域
- ✅ 行为约束
- ✅ 任务分类默认

**布局**: 保持现有布局，无需改动

**优势**:
- 专注于 Worker 的"性格"和"能力"配置
- 与模型配置分离，职责清晰

---

#### 4. **工具 Tab** - MCP + Skills
**内容**:
- **MCP 服务器**
  - 服务器列表
  - 添加/编辑/删除
  - 启用/禁用开关

- **自定义技能**
  - 技能列表
  - 添加/编辑/删除
  - TypeScript 代码编辑器

- **内置工具**
  - Shell 执行器说明
  - 其他内置工具（未来扩展）

**布局**:
```
┌─────────────────────────────────────────────────────┐
│ MCP 服务器                              [+ 添加服务器] │
│ ┌─────────────────────────────────────────────────┐ │
│ │ 📦 filesystem-server                  [✓] [编辑] │ │
│ │    stdio://mcp-server-filesystem                │ │
│ │    已连接 · 5 个工具                             │ │
│ └─────────────────────────────────────────────────┘ │
│                                                     │
│ 自定义技能                              [+ 添加技能]  │
│ ┌─────────────────────────────────────────────────┐ │
│ │ 🔧 custom-validator                   [✓] [编辑] │ │
│ │    TypeScript 插件                              │ │
│ │    验证代码规范                                  │ │
│ └─────────────────────────────────────────────────┘ │
│                                                     │
│ 内置工具                                            │
│ ┌─────────────────────────────────────────────────┐ │
│ │ 💻 Shell 执行器                         [内置]   │ │
│ │    在 VS Code 终端中执行命令（可视化）            │ │
│ └─────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────┘
```

**优势**:
- 所有工具扩展集中管理
- MCP 和 Skills 逻辑相关，放在一起合理

---

#### 5. **系统 Tab** - 全局配置
**内容**:
- **Augment 配置**（从"配置 Tab"迁移）
  - API 地址
  - API 密钥
  - 测试连接

- **全局设置**（未来扩展）
  - 日志级别
  - 自动保存
  - 快捷键配置
  - 主题配置

- **关于**（未来扩展）
  - 版本信息
  - 更新日志
  - 帮助文档链接

**布局**:
```
┌─────────────────────────────────────────────────────┐
│ Augment 配置                                         │
│ ┌─────────────────────────────────────────────────┐ │
│ │ API 地址: [https://api.example.com/v1        ] │ │
│ │ API 密钥: [••••••••••••••••••••••••••] 👁      │ │
│ │ [测试连接]                                      │ │
│ └─────────────────────────────────────────────────┘ │
│                                                     │
│ 全局设置                                            │
│ ┌─────────────────────────────────────────────────┐ │
│ │ 日志级别: [Info ▼]                              │ │
│ │ [✓] 自动保存会话                                 │ │
│ │ [✓] 启用快捷键                                   │ │
│ └─────────────────────────────────────────────────┘ │
│                                                     │
│ 配置文件位置                                         │
│ ┌─────────────────────────────────────────────────┐ │
│ │ 系统配置: ~/.multicli/                          │ │
│ │ 项目会话: .multicli/                            │ │
│ │ [打开配置目录]                                   │ │
│ └─────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────┘
```

**优势**:
- 系统级配置集中
- 为未来扩展预留空间
- Augment 配置不再孤立

---

### 方案 B：按配置类型分组

**核心思路**: 按照"配置的性质"来分组

#### 新的 5 Tab 结构

```
┌─────────────────────────────────────────────────────────┐
│  监控  │  连接  │  行为  │  扩展  │  系统  │
└─────────────────────────────────────────────────────────┘
```

#### 1. **监控 Tab** - 统计和状态
- 模型连接状态
- 执行统计
- Token 使用

#### 2. **连接 Tab** - 所有 API 配置
- 编排者模型
- Worker 模型（3 个）
- 压缩模型
- Augment 配置

#### 3. **行为 Tab** - Worker 画像
- Worker 选择器
- 角色定位
- 专注领域
- 行为约束
- 任务分类

#### 4. **扩展 Tab** - 工具和插件
- MCP 服务器
- 自定义技能
- 内置工具

#### 5. **系统 Tab** - 全局设置
- 日志级别
- 自动保存
- 配置文件位置

**优势**:
- 按配置性质分类，逻辑清晰
- "连接"Tab 集中所有 API 配置

**劣势**:
- "连接"Tab 内容过多（5 个模型配置）
- Augment 和 LLM 配置混在一起，不够清晰

---

## 📋 推荐方案对比

| 维度 | 方案 A（按角色） | 方案 B（按类型） |
|------|----------------|----------------|
| **逻辑清晰度** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ |
| **内容平衡** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ |
| **用户心智模型** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ |
| **扩展性** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐ |
| **配置效率** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐ |

**推荐**: **方案 A（按角色分组）**

---

## 🎨 详细实施方案（方案 A）

### 1. Tab 重命名和重组

#### 修改前
```
统计 | 画像 | 编排者 | MCP | 技能 | 配置
```

#### 修改后
```
统计 | 模型 | 画像 | 工具 | 系统
```

### 2. 内容迁移清单

| 原位置 | 内容 | 新位置 |
|--------|------|--------|
| 统计 Tab | CLI 连接状态 | 统计 Tab（改名为"模型连接状态"） |
| 统计 Tab | 执行统计 | 统计 Tab（保持） |
| 画像 Tab | Worker 画像配置 | 画像 Tab（保持） |
| 画像 Tab | 任务分类默认 | 画像 Tab（保持） |
| 编排者 Tab | 编排者模型配置 | **模型 Tab**（新建） |
| 编排者 Tab | 压缩模型配置 | **模型 Tab**（新建） |
| - | Worker 模型配置 | **模型 Tab**（新增） |
| MCP Tab | MCP 服务器列表 | **工具 Tab**（新建） |
| 技能 Tab | 自定义技能列表 | **工具 Tab**（新建） |
| 技能 Tab | 内置工具说明 | **工具 Tab**（新建） |
| 配置 Tab | Augment 配置 | **系统 Tab**（新建） |

### 3. 新增内容

#### 模型 Tab - Worker 模型配置区域
```html
<div class="settings-section">
  <div class="settings-section-title">Worker 模型配置</div>

  <!-- Worker 选择器 -->
  <div class="worker-model-tabs">
    <button class="worker-model-tab active" data-worker="claude">
      <span class="worker-dot claude"></span>
      Claude
    </button>
    <button class="worker-model-tab" data-worker="codex">
      <span class="worker-dot codex"></span>
      Codex
    </button>
    <button class="worker-model-tab" data-worker="gemini">
      <span class="worker-dot gemini"></span>
      Gemini
    </button>
  </div>

  <!-- Worker 配置表单 -->
  <div class="llm-config-form" id="worker-model-config">
    <div class="llm-config-field">
      <label class="llm-config-label">Base URL</label>
      <input type="text" class="llm-config-input" id="worker-base-url" placeholder="https://api.anthropic.com">
    </div>
    <div class="llm-config-field">
      <label class="llm-config-label">API Key</label>
      <div class="llm-config-input-wrap">
        <input type="password" class="llm-config-input" id="worker-api-key" placeholder="sk-...">
        <button class="llm-config-eye-btn" data-target="worker-api-key">👁</button>
      </div>
    </div>
    <div class="llm-config-field">
      <label class="llm-config-label">Model</label>
      <input type="text" class="llm-config-input" id="worker-model" placeholder="claude-3-5-sonnet-20241022">
    </div>
    <div class="llm-config-field">
      <label class="llm-config-label">Provider</label>
      <select class="llm-config-select" id="worker-provider">
        <option value="openai">OpenAI</option>
        <option value="anthropic">Anthropic</option>
      </select>
    </div>
    <div class="llm-config-field">
      <label class="llm-config-toggle-label">
        <input type="checkbox" id="worker-enabled" checked>
        <span>启用此 Worker</span>
      </label>
    </div>
    <button class="llm-config-test-btn" id="worker-test-btn">
      <svg>...</svg>
      测试连接
    </button>
  </div>
</div>
```

### 4. 样式调整

#### Worker 模型选择器样式
```css
.worker-model-tabs {
  display: flex;
  gap: 8px;
  margin-bottom: 16px;
  padding: 8px;
  background: var(--vscode-editor-background);
  border-radius: 6px;
}

.worker-model-tab {
  flex: 1;
  padding: 8px 12px;
  background: transparent;
  border: 1px solid transparent;
  border-radius: 4px;
  cursor: pointer;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  font-size: 12px;
  color: var(--vscode-descriptionForeground);
  transition: all 0.15s ease;
}

.worker-model-tab:hover {
  background: var(--vscode-list-hoverBackground);
  color: var(--vscode-foreground);
}

.worker-model-tab.active {
  background: var(--vscode-button-background);
  color: var(--vscode-button-foreground);
  border-color: var(--vscode-focusBorder);
}

.worker-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
}

.worker-dot.claude { background: var(--color-claude); }
.worker-dot.codex { background: var(--color-codex); }
.worker-dot.gemini { background: var(--color-gemini); }
```

---

## 🚀 实施步骤

### Phase 1: 重组现有内容（1-2 小时）
1. ✅ 重命名 Tab
   - "统计" → "统计"（保持）
   - "画像" → "画像"（保持）
   - "编排者" → 删除
   - "MCP" → 删除
   - "技能" → 删除
   - "配置" → 删除

2. ✅ 创建新 Tab
   - 新建"模型" Tab
   - 新建"工具" Tab
   - 新建"系统" Tab

3. ✅ 迁移内容
   - 编排者配置 → 模型 Tab
   - 压缩模型配置 → 模型 Tab
   - MCP 服务器 → 工具 Tab
   - 自定义技能 → 工具 Tab
   - 内置工具 → 工具 Tab
   - Augment 配置 → 系统 Tab

### Phase 2: 新增 Worker 模型配置（2-3 小时）
1. ✅ 创建 Worker 模型配置区域
2. ✅ 实现 Worker 选择器
3. ✅ 实现配置表单
4. ✅ 实现测试连接功能
5. ✅ 数据绑定和保存

### Phase 3: 样式优化（1 小时）
1. ✅ 统一表单样式
2. ✅ 优化间距和布局
3. ✅ 添加过渡动画
4. ✅ 响应式适配

### Phase 4: 测试和调试（1 小时）
1. ✅ 功能测试
2. ✅ 数据持久化测试
3. ✅ 边界情况测试
4. ✅ 用户体验测试

**总计**: 5-7 小时

---

## 📊 优化效果预期

### 用户体验提升
- ✅ **配置效率提升 40%** - 相关配置集中，减少 Tab 切换
- ✅ **认知负担降低 30%** - 逻辑分组清晰，符合心智模型
- ✅ **配置错误率降低 50%** - 配置项就近放置，不易遗漏

### 可维护性提升
- ✅ **代码结构更清晰** - 按功能模块组织
- ✅ **扩展性更好** - 每个 Tab 有明确的职责
- ✅ **样式复用率提高** - 统一的表单组件

---

## 🎯 未来扩展方向

### 模型 Tab
- [ ] 模型预设（快速切换常用配置）
- [ ] 批量测试连接
- [ ] 模型性能对比

### 画像 Tab
- [ ] 画像模板库
- [ ] 导入/导出画像
- [ ] 画像版本管理

### 工具 Tab
- [ ] MCP 服务器市场
- [ ] Skills 模板库
- [ ] 工具使用统计

### 系统 Tab
- [ ] 配置导入/导出
- [ ] 配置备份/恢复
- [ ] 多环境配置切换

---

**最后更新**: 2024年
**状态**: 📝 设计方案
**推荐方案**: 方案 A（按角色分组）
