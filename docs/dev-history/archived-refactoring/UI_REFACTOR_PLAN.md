# UI 模块化重构计划

## 目标

将 `src/ui/webview/index.html` 中的 15000+ 行代码拆分为模块化结构，提高可维护性。

## 目录结构

```
src/ui/webview/
├── index.html                 # 主入口（精简，~200 行）
├── styles/
│   ├── base.css              # 基础样式、CSS 变量、重置
│   ├── layout.css            # 布局相关（主容器、侧边栏等）
│   ├── components.css        # 通用组件（按钮、输入框、标签等）
│   ├── messages.css          # 消息列表样式
│   ├── settings.css          # 设置面板样式
│   └── modals.css            # 所有弹窗样式
├── js/
│   ├── core/
│   │   ├── state.js          # 全局状态管理
│   │   ├── vscode-api.js     # VSCode 通信封装
│   │   └── utils.js          # 工具函数
│   ├── ui/
│   │   ├── message-renderer.js    # 消息渲染逻辑
│   │   ├── settings-panel.js      # 设置面板逻辑
│   │   ├── modal-mcp.js           # MCP 服务器弹窗
│   │   ├── modal-repository.js    # 仓库管理弹窗
│   │   └── modal-skill.js         # Skill 库弹窗
│   └── main.js               # 应用入口，初始化
└── templates/
    ├── modals.html           # 弹窗 HTML 模板
    └── settings.html         # 设置面板 HTML 模板
```

## 实施步骤

### Phase 1: 提取 CSS（1-2 小时）

1. **base.css** - 提取 CSS 变量、重置样式
2. **layout.css** - 提取布局相关样式
3. **components.css** - 提取通用组件样式
4. **messages.css** - 提取消息列表样式
5. **settings.css** - 提取设置面板样式
6. **modals.css** - 提取弹窗样式

### Phase 2: 提取 JavaScript 核心（2-3 小时）

1. **state.js** - 全局状态管理
   - sessions, currentSessionId, threadMessages
   - repositories, skillsConfig
   - cliOutputs, executionStats
   - 状态持久化（localStorage）

2. **vscode-api.js** - VSCode 通信封装
   - postMessage 封装
   - 消息监听器
   - 事件分发

3. **utils.js** - 工具函数
   - escapeHtml
   - formatTimestamp
   - generateId
   - 其他辅助函数

### Phase 3: 提取 UI 模块（3-4 小时）

1. **message-renderer.js** - 消息渲染
   - renderMessage
   - renderMessageBlock
   - renderToolCallBlock
   - renderThinkingBlock
   - 等等

2. **settings-panel.js** - 设置面板
   - renderSettingsPanel
   - handleSettingsTabSwitch
   - 各个 Tab 的渲染和逻辑

3. **modal-mcp.js** - MCP 服务器弹窗
   - showMCPServerDialog
   - renderMCPServerList
   - addMCPServer
   - deleteMCPServer

4. **modal-repository.js** - 仓库管理弹窗
   - showRepositoryManagementDialog
   - renderRepositoryManagementList
   - addRepositoryFromDialog
   - deleteRepositoryFromDialog
   - refreshRepositoryInDialog

5. **modal-skill.js** - Skill 库弹窗
   - showSkillLibraryDialog
   - renderSkillLibrary
   - installSkill
   - uninstallSkill

### Phase 4: 提取 HTML 模板（1-2 小时）

1. **modals.html** - 所有弹窗的 HTML 模板
   - MCP 服务器弹窗
   - 仓库管理弹窗
   - Skill 库弹窗
   - 其他弹窗

2. **settings.html** - 设置面板 HTML 模板
   - 各个 Tab 的内容

### Phase 5: 重构主入口（1 小时）

1. **index.html** - 精简主入口
   - 只保留基础 HTML 结构
   - 引入所有 CSS 和 JS 文件
   - 初始化应用

2. **main.js** - 应用入口
   - 初始化状态
   - 初始化 UI
   - 设置事件监听器
   - 启动应用

### Phase 6: 测试和验证（1-2 小时）

1. 功能测试
2. 性能测试
3. 兼容性测试
4. Bug 修复

## 预计总时间

**9-14 小时**

## 验收标准

- [ ] 所有功能正常工作
- [ ] 代码结构清晰，易于维护
- [ ] 每个文件职责单一
- [ ] 文件大小合理（< 500 行）
- [ ] 编译通过
- [ ] 无运行时错误

## 注意事项

1. **保持向后兼容**：确保所有现有功能正常工作
2. **渐进式重构**：每完成一个模块就测试
3. **保留备份**：重构前备份 index.html
4. **详细注释**：每个模块添加清晰的注释
5. **统一风格**：保持代码风格一致

## 开始重构？

请确认是否开始重构，我将按照以上计划逐步进行。
