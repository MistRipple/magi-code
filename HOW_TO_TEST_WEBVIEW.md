# ⚠️ 重要：如何正确测试 Webview 修复

## 🚫 错误的测试方法

**不要**直接在浏览器中打开 `index.html` 文件！

如果你看到这个错误：
```
Access to script at 'file:///Users/xie/code/MultiCLI/src/ui/webview/js/main.js'
from origin 'null' has been blocked by CORS policy
```

这说明你是在浏览器中直接打开了 HTML 文件，这是**无法工作的**，因为：

1. **CORS 限制**：浏览器的 `file://` 协议不允许加载 ES6 模块
2. **缺少 Webview 环境**：没有 VSCode 的 `webview.asWebviewUri()` 路径转换
3. **协议不匹配**：我们的修复使用 `vscode-webview://` 协议，不是 `file://`

---

## ✅ 正确的测试方法

### 步骤 1: 启动 VSCode 扩展开发主机

在 VSCode 中：
1. 打开 MultiCLI 项目
2. 按 **F5** 键
3. 或者：按 **Ctrl+Shift+P** → 输入 "Debug: Start Debugging"

这会打开一个新的 VSCode 窗口（标题显示 "[Extension Development Host]"）

### 步骤 2: 在新窗口中打开 MultiCLI 面板

在新打开的 VSCode 窗口中：

**方法 1**：点击侧边栏的 MultiCLI 图标

**方法 2**：
1. 按 **Ctrl+Shift+P** (Mac: **Cmd+Shift+P**)
2. 输入 "MultiCLI: Open Main View"
3. 按 Enter

### 步骤 3: 验证样式加载

现在你应该看到：
- ✅ 深色背景（不是白色）
- ✅ 顶部有 4 个 Tab（对话/任务/变更/输出）
- ✅ 底部有 6 个 Tab（统计/画像/编排者/MCP/技能/配置）
- ✅ 输入框有圆角和边框
- ✅ 按钮有样式

**不应该看到**：
- ❌ 纯白色背景
- ❌ 无样式的 HTML 文本
- ❌ CORS 错误

### 步骤 4: 使用开发者工具检查

1. 在 Webview 中**右键**
2. 选择 "**检查元素**" 或 "**Inspect**"
3. 切换到 **Network** 标签
4. 刷新页面（Ctrl+R）

**应该看到**（全部状态码 200）：
```
✅ base.css          - 200 - vscode-webview://xxx/base.css
✅ layout.css        - 200 - vscode-webview://xxx/layout.css
✅ components.css    - 200 - vscode-webview://xxx/components.css
✅ messages.css      - 200 - vscode-webview://xxx/messages.css
✅ settings.css      - 200 - vscode-webview://xxx/settings.css
✅ modals.css        - 200 - vscode-webview://xxx/modals.css
✅ main.js           - 200 - vscode-webview://xxx/main.js
✅ state.js          - 200 - vscode-webview://xxx/state.js
✅ utils.js          - 200 - vscode-webview://xxx/utils.js
✅ vscode-api.js     - 200 - vscode-webview://xxx/vscode-api.js
✅ message-renderer.js - 200 - vscode-webview://xxx/message-renderer.js
✅ message-handler.js  - 200 - vscode-webview://xxx/message-handler.js
✅ event-handlers.js   - 200 - vscode-webview://xxx/event-handlers.js
```

注意：URL 应该是 `vscode-webview://` 开头，**不是** `file://`

### 步骤 5: 检查 Console

切换到 **Console** 标签，应该：
- ✅ 无红色错误信息
- ✅ 无 "CORS policy" 错误
- ✅ 无 "Module not found" 错误

---

## 🔍 为什么必须在 VSCode 中测试？

### 1. Webview URI 转换
我们的修复使用 `webview.asWebviewUri()` 将路径转换为：
```
file:///path/to/file.css  →  vscode-webview://authority/path/to/file.css
```

这个转换**只在 VSCode Webview 环境中有效**。

### 2. Import Map
我们生成的 Import Map 使用 `vscode-webview://` 协议：
```html
<script type="importmap">
{
  "imports": {
    "./core/state.js": "vscode-webview://xxx/core/state.js"
  }
}
</script>
```

浏览器的 `file://` 协议无法处理这种映射。

### 3. Content Security Policy
VSCode Webview 有特殊的 CSP 配置，允许加载 `vscode-webview://` 资源。

---

## 📋 快速检查清单

在 VSCode 扩展开发主机中测试时：

- [ ] 按 F5 启动扩展开发主机
- [ ] 新窗口标题显示 "[Extension Development Host]"
- [ ] 打开 MultiCLI 面板
- [ ] 页面有深色背景和样式
- [ ] Network 面板显示所有资源加载成功（状态码 200）
- [ ] URL 是 `vscode-webview://` 开头
- [ ] Console 无 CORS 错误
- [ ] Tab 切换功能正常

---

## 🆘 如果仍然有问题

### 问题 1: 扩展开发主机无法启动
- 确保已运行 `npm run compile`
- 检查是否有编译错误
- 重启 VSCode

### 问题 2: MultiCLI 面板无法打开
- 检查侧边栏是否有 MultiCLI 图标
- 尝试命令面板：Ctrl+Shift+P → "MultiCLI: Open Main View"
- 查看 VSCode 输出面板的错误信息

### 问题 3: 样式仍然未加载
- 打开开发者工具检查 Network 面板
- 查看哪些资源加载失败
- 检查 Console 的具体错误信息
- 截图并报告具体错误

---

## 📞 需要帮助？

如果在 VSCode 扩展开发主机中测试后仍有问题：

1. **截图**：Network 面板和 Console 面板
2. **记录**：具体的错误信息
3. **确认**：是否在扩展开发主机中测试（不是浏览器）

---

**重要提醒**：
- ✅ 在 VSCode 扩展开发主机中测试（按 F5）
- ❌ 不要在浏览器中直接打开 HTML 文件

**测试时间**: 5-10 分钟
**最后更新**: 2024-01-22
