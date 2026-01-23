# Phase 7 最终验收清单

## ✅ 所有代码修复已完成

### 修复 1: Webview 路径 ✅
- [x] 更新 `webview-provider.ts` 处理 6 个 CSS 文件
- [x] 更新 JavaScript 主入口路径
- [x] 实现 Import Map 支持
- [x] 编译成功

### 修复 2: 重复函数定义 ✅
- [x] 删除 `message-renderer.js` 中 58 行重复代码
- [x] 保留从 `utils.js` 的导入
- [x] 语法验证通过

### 修复 3: 模板字符串语法 ✅
- [x] 修复第 101 行: `top-tab-${tabName}`
- [x] 修复第 128 行: `settings-tab-${tabName}`
- [x] 修复第 377 行: 删除会话确认
- [x] 使用 `od -c` 验证字节正确

### 修复 4: 导入/导出不匹配 ✅
- [x] 修复 `event-handlers.js` 导入 `answerQuestions`
- [x] 导入/导出冲突检查通过
- [x] 编译成功

---

## 🧪 现在开始 Phase 8 运行时测试

### 步骤 1: 启动扩展开发主机

```bash
# 在 VSCode 中按 F5
# 或者使用命令面板: Debug: Start Debugging
```

**确认**: 新窗口标题显示 "[Extension Development Host]"

---

### 步骤 2: 打开 MultiCLI 面板

- 方法 1: 点击侧边栏的 MultiCLI 图标
- 方法 2: Ctrl+Shift+P → "MultiCLI: Open Main View"

---

### 步骤 3: 强制刷新清除缓存 ⚠️ 重要

**必须执行此步骤！**

按 **Ctrl+Shift+R** (Mac: **Cmd+Shift+R**)

这会强制浏览器重新加载所有资源，清除旧的缓存文件。

---

### 步骤 4: 打开开发者工具

1. 在 Webview 中右键
2. 选择 "检查元素" 或 "Inspect"

---

### 步骤 5: 检查 Network 面板

切换到 **Network** 标签，查看资源加载：

#### CSS 文件（应该全部 200）
- [ ] base.css - 200
- [ ] layout.css - 200
- [ ] components.css - 200
- [ ] messages.css - 200
- [ ] settings.css - 200
- [ ] modals.css - 200

#### JavaScript 文件（应该全部 200）
- [ ] main.js - 200
- [ ] state.js - 200
- [ ] utils.js - 200
- [ ] vscode-api.js - 200
- [ ] message-renderer.js - 200
- [ ] message-handler.js - 200
- [ ] event-handlers.js - 200

**注意**: URL 应该是 `vscode-webview://` 开头，不是 `file://`

---

### 步骤 6: 检查 Console 面板

切换到 **Console** 标签：

#### 应该看到 ✅
- [ ] 无红色错误信息
- [ ] 无 "CORS policy" 错误
- [ ] 无 "Module not found" 错误
- [ ] 无 "SyntaxError" 错误
- [ ] 无 "does not provide an export named" 错误

#### 可能看到（正常）
- 一些 info 或 log 信息
- 初始化消息

---

### 步骤 7: 验证 Import Map

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
    "./core/vscode-api.js": "vscode-webview://xxx/core/vscode-api.js",
    "./ui/message-renderer.js": "vscode-webview://xxx/ui/message-renderer.js",
    "./ui/message-handler.js": "vscode-webview://xxx/ui/message-handler.js",
    "./ui/event-handlers.js": "vscode-webview://xxx/ui/event-handlers.js"
  }
}
```

---

### 步骤 8: 视觉检查

#### 应该看到 ✅
- [ ] 深色背景（不是白色）
- [ ] 顶部 4 个 Tab（对话/任务/变更/输出）
- [ ] 底部 6 个 Tab（统计/画像/编排者/MCP/技能/配置）
- [ ] 输入框有圆角和边框
- [ ] 按钮有样式和 hover 效果
- [ ] 消息卡片有圆角和阴影

#### 不应该看到 ❌
- [ ] 纯白色背景
- [ ] 无样式的 HTML 文本
- [ ] 没有圆角的元素
- [ ] 黑色文字在白色背景上

---

### 步骤 9: 功能测试

#### Tab 切换
- [ ] 点击顶部 Tab - 应该切换视图
- [ ] 点击底部 Tab - 应该切换设置面板
- [ ] 选中的 Tab 应该有高亮效果

#### 输入测试
- [ ] 在输入框中输入文字 - 应该正常显示
- [ ] 按 Enter - 应该发送消息（或显示错误提示）
- [ ] 按 Shift+Enter - 应该换行

#### 样式测试
- [ ] 消息卡片有圆角和阴影
- [ ] 按钮有 hover 效果
- [ ] 输入框有 focus 效果

---

## 🎯 验收标准

### 必须通过 ✅
- [ ] 所有 6 个 CSS 文件加载成功（状态码 200）
- [ ] 所有 7 个 JavaScript 文件加载成功（状态码 200）
- [ ] Import Map 正确生成
- [ ] Console 无加载错误
- [ ] 页面有样式（不是白色背景）
- [ ] Tab 切换功能正常

### 应该通过 ✅
- [ ] 输入和发送消息功能正常
- [ ] 消息渲染样式正常
- [ ] 刷新后样式和功能正常

---

## 🐛 如果遇到问题

### 问题 1: 仍然看到旧的错误

**原因**: 浏览器缓存未清除

**解决**:
1. 按 **Ctrl+Shift+R** 强制刷新
2. 或关闭扩展开发主机窗口，重新按 F5 启动

### 问题 2: 样式未加载

**检查**:
1. Network 面板 - 哪些 CSS 文件加载失败？
2. Console - 是否有加载错误？
3. 文件路径 - `src/ui/webview/styles/` 下是否有所有 CSS 文件？

### 问题 3: JavaScript 错误

**检查**:
1. Console - 具体错误信息是什么？
2. Import Map - 是否正确生成？
3. Network - 哪些 JS 文件加载失败？

### 问题 4: 功能异常

**检查**:
1. Console - 是否有 JavaScript 错误？
2. 全局变量 - vscode, state 等是否正确初始化？
3. 事件监听器 - 是否正确绑定？

---

## 📞 需要帮助？

如果测试后仍有问题，请提供：

1. **截图**: Network 面板和 Console 面板
2. **错误信息**: 具体的错误消息
3. **确认**: 是否在扩展开发主机中测试（不是浏览器）
4. **确认**: 是否执行了强制刷新（Ctrl+Shift+R）

---

## 📚 相关文档

- **快速测试**: `QUICK_TEST_GUIDE.md` (5分钟)
- **完整测试**: `test-webview-runtime.md` (15分钟)
- **测试说明**: `HOW_TO_TEST_WEBVIEW.md` (必读)
- **所有修复**: `PHASE7_ALL_FIXES_SUMMARY.md`
- **缓存问题**: `CACHE_ISSUE_FIX.md`

---

## 🎉 成功标准

如果以上所有检查都通过，说明 **Phase 7 修复成功，Phase 8 测试通过**！

**恭喜！UI 重构的核心工作已完成！** 🎊

---

**最后更新**: 2024-01-22
**预计测试时间**: 10-15 分钟
