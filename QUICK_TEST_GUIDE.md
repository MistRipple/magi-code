# 快速测试指南

## ⚠️ 重要提醒

**不要在浏览器中直接打开 HTML 文件！**

如果你看到 CORS 错误（`Access to script ... has been blocked by CORS policy`），说明你在浏览器中打开了文件。这是**错误的测试方法**。

**正确方法**：必须在 VSCode 扩展开发主机中测试（按 F5）。

详细说明请参考：`HOW_TO_TEST_WEBVIEW.md`

---

## 🚀 立即开始测试

### 1. 启动扩展开发主机
```bash
# 在 VSCode 中按 F5
# 或者在命令面板中运行: Debug: Start Debugging
```

**确认**：新窗口标题应该显示 "[Extension Development Host]"

### 2. 打开 MultiCLI 面板
- 方法 1: 点击侧边栏的 MultiCLI 图标
- 方法 2: Ctrl+Shift+P → "MultiCLI: Open Main View"

### 3. 快速视觉检查（30秒）

**✅ 应该看到**:
- 深色背景（不是白色）
- 顶部有 4 个 Tab（对话/任务/变更/输出）
- 底部有 6 个 Tab（统计/画像/编排者/MCP/技能/配置）
- 输入框有圆角和边框
- 执行按钮有样式

**❌ 不应该看到**:
- 纯白色背景
- 无样式的 HTML 文本
- 没有圆角和边框的元素

### 4. 开发者工具检查（2分钟）

#### 打开开发者工具
1. 在 Webview 中右键
2. 选择 "检查元素" 或 "Inspect"

#### 检查 Network 面板
1. 切换到 Network 标签
2. 刷新页面（Ctrl+R）
3. 查看资源加载：

**应该看到（全部状态码 200）**:
```
✅ base.css          - 200
✅ layout.css        - 200
✅ components.css    - 200
✅ messages.css      - 200
✅ settings.css      - 200
✅ modals.css        - 200
✅ main.js           - 200
✅ state.js          - 200
✅ utils.js          - 200
✅ vscode-api.js     - 200
✅ message-renderer.js - 200
✅ message-handler.js  - 200
✅ event-handlers.js   - 200
```

#### 检查 Console 面板
1. 切换到 Console 标签
2. 查看是否有错误

**应该看到**:
```javascript
// 无红色错误信息
// 可能有一些 info 或 log 信息（正常）
```

**不应该看到**:
```
❌ Failed to load resource
❌ Module not found
❌ Uncaught SyntaxError
❌ Import Map error
```

#### 验证 Import Map
在 Console 中执行：
```javascript
const importMap = document.querySelector('script[type="importmap"]');
console.log('Import Map:', importMap ? JSON.parse(importMap.textContent) : 'Not found');
```

**应该看到**:
```javascript
{
  "imports": {
    "./core/state.js": "vscode-webview://xxx/core/state.js",
    "./core/utils.js": "vscode-webview://xxx/core/utils.js",
    // ... 其他 4 个模块
  }
}
```

#### 验证全局函数
在 Console 中执行：
```javascript
console.log('vscode:', typeof vscode);
console.log('state:', typeof state);
console.log('renderMainContent:', typeof renderMainContent);
console.log('initializeEventListeners:', typeof initializeEventListeners);
```

**应该看到**:
```
vscode: object
state: object
renderMainContent: function
initializeEventListeners: function
```

### 5. 功能测试（3分钟）

#### Tab 切换
- [ ] 点击顶部 Tab（对话/任务/变更/输出）- 应该切换视图
- [ ] 点击底部 Tab（统计/画像/编排者/MCP/技能/配置）- 应该切换设置面板
- [ ] 选中的 Tab 应该有高亮效果

#### 输入测试
- [ ] 在输入框中输入文字 - 应该正常显示
- [ ] 按 Enter - 应该发送消息（或显示错误提示）
- [ ] 按 Shift+Enter - 应该换行

#### 样式测试
- [ ] 消息卡片有圆角和阴影
- [ ] 按钮有 hover 效果
- [ ] 输入框有 focus 效果

### 6. 问题排查

#### 如果样式未加载
1. 检查 Network 面板 - 哪些 CSS 文件加载失败？
2. 检查 Console - 是否有加载错误？
3. 检查文件路径 - `src/ui/webview/styles/` 下是否有所有 CSS 文件？

#### 如果 JavaScript 错误
1. 检查 Console - 具体错误信息是什么？
2. 检查 Import Map - 是否正确生成？
3. 检查 Network - 哪些 JS 文件加载失败？

#### 如果功能异常
1. 检查 Console - 是否有 JavaScript 错误？
2. 检查全局变量 - vscode, state 等是否正确初始化？
3. 检查事件监听器 - 是否正确绑定？

---

## 📋 测试检查清单

### 必须通过 ✅
- [ ] 编译成功（npm run compile）
- [ ] 页面有样式（不是白色背景）
- [ ] 所有 6 个 CSS 文件加载成功（状态码 200）
- [ ] 所有 7 个 JavaScript 文件加载成功（状态码 200）
- [ ] Import Map 正确生成
- [ ] Console 无加载错误
- [ ] Tab 切换功能正常

### 应该通过 ✅
- [ ] 输入和发送消息功能正常
- [ ] 消息渲染样式正常
- [ ] 刷新后样式和功能正常

---

## 🎯 成功标准

如果以上所有检查都通过，说明 **Phase 7 修复成功**！

如果有任何问题，请：
1. 记录具体的错误信息
2. 截图保存
3. 查看详细测试文档：`test-webview-runtime.md`

---

## 📞 需要帮助？

- 详细测试计划: `test-webview-runtime.md`
- 修复说明: `UI_REFACTOR_WEBVIEW_FIX.md`
- Phase 7 报告: `UI_REFACTOR_PHASE7_REPORT.md`
- 执行计划: `UI_REFACTOR_EXECUTION.md`

---

**预计测试时间**: 5-10 分钟
**最后更新**: 2024-01-22
