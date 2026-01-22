# Webview Runtime Test Plan

## 目标
验证 UI 重构后的 Webview 资源加载是否正常工作

## 测试环境
- VSCode 扩展开发模式
- MultiCLI 插件已编译

## 测试步骤

### 1. 启动测试环境
1. 在 VSCode 中打开 MultiCLI 项目
2. 按 F5 启动扩展开发主机
3. 在新窗口中打开任意工作区

### 2. 打开 MultiCLI 面板
1. 按 Ctrl+Shift+P (Mac: Cmd+Shift+P)
2. 输入 "MultiCLI: Open Main View"
3. 或点击侧边栏的 MultiCLI 图标

### 3. 检查样式加载

#### 3.1 视觉检查
- [ ] 页面有正确的背景色（深色主题）
- [ ] 顶部 Tab 栏样式正常（对话/任务/变更/输出）
- [ ] 底部 Tab 栏样式正常（统计/画像/编排者/MCP/技能/配置）
- [ ] 输入框样式正常（圆角、边框、占位符）
- [ ] 按钮样式正常（执行按钮、设置按钮等）
- [ ] 消息卡片样式正常（圆角、阴影、间距）

#### 3.2 开发者工具检查
1. 在 Webview 中右键 → "检查元素"
2. 打开 Network 面板
3. 刷新页面
4. 检查 CSS 文件加载：
   - [ ] base.css - 状态码 200
   - [ ] layout.css - 状态码 200
   - [ ] components.css - 状态码 200
   - [ ] messages.css - 状态码 200
   - [ ] settings.css - 状态码 200
   - [ ] modals.css - 状态码 200

#### 3.3 Console 检查
1. 打开 Console 面板
2. 检查是否有错误：
   - [ ] 无 CSS 加载失败错误
   - [ ] 无 JavaScript 模块加载失败错误
   - [ ] 无 Import Map 相关错误

### 4. 检查 JavaScript 模块加载

#### 4.1 模块加载检查
在 Console 中执行：
```javascript
// 检查全局状态是否初始化
console.log('vscode:', typeof vscode);
console.log('state:', typeof state);
console.log('renderMainContent:', typeof renderMainContent);
console.log('initializeEventListeners:', typeof initializeEventListeners);
```

预期输出：
- [ ] vscode: object
- [ ] state: object
- [ ] renderMainContent: function
- [ ] initializeEventListeners: function

#### 4.2 Network 面板检查
检查 JavaScript 文件加载：
- [ ] main.js - 状态码 200
- [ ] core/state.js - 状态码 200
- [ ] core/utils.js - 状态码 200
- [ ] core/vscode-api.js - 状态码 200
- [ ] ui/message-renderer.js - 状态码 200
- [ ] ui/message-handler.js - 状态码 200
- [ ] ui/event-handlers.js - 状态码 200

### 5. 功能测试

#### 5.1 Tab 切换
- [ ] 点击顶部 Tab（对话/任务/变更/输出）- 切换正常
- [ ] 点击底部 Tab（统计/画像/编排者/MCP/技能/配置）- 切换正常
- [ ] Tab 高亮状态正确

#### 5.2 输入功能
- [ ] 在输入框中输入文字 - 正常显示
- [ ] 按 Enter 发送消息 - 正常发送
- [ ] 按 Shift+Enter 换行 - 正常换行

#### 5.3 消息渲染
- [ ] 发送一条简单消息
- [ ] 检查消息卡片样式是否正常
- [ ] 检查 Markdown 渲染是否正常
- [ ] 检查代码块高亮是否正常

### 6. Import Map 验证

在 Console 中执行：
```javascript
// 检查 Import Map 是否生效
const importMap = document.querySelector('script[type="importmap"]');
console.log('Import Map:', importMap ? JSON.parse(importMap.textContent) : 'Not found');
```

预期输出：
- [ ] Import Map 存在
- [ ] 包含 6 个模块映射：
  - ./core/state.js
  - ./core/utils.js
  - ./core/vscode-api.js
  - ./ui/message-renderer.js
  - ./ui/message-handler.js
  - ./ui/event-handlers.js
- [ ] 每个映射的值是 vscode-webview:// 开头的 URI

### 7. 错误场景测试

#### 7.1 刷新测试
- [ ] 刷新 Webview - 样式和功能正常
- [ ] 关闭并重新打开 Webview - 样式和功能正常

#### 7.2 多窗口测试
- [ ] 打开多个工作区窗口
- [ ] 每个窗口的 MultiCLI 面板样式都正常

## 问题排查

### 如果样式未加载
1. 检查 Console 是否有 CSS 加载错误
2. 检查 Network 面板中 CSS 文件的 URL 是否正确
3. 检查 webview-provider.ts 中的 cssFiles 数组是否完整
4. 检查 index.html 中的 CSS 引用路径是否正确

### 如果 JavaScript 模块未加载
1. 检查 Console 是否有模块加载错误
2. 检查 Import Map 是否正确生成
3. 检查 Network 面板中 JS 文件的 URL 是否正确
4. 检查浏览器是否支持 Import Map（VSCode 内置 Chromium 应该支持）

### 如果功能异常
1. 检查 Console 是否有 JavaScript 错误
2. 检查全局变量是否正确初始化
3. 检查事件监听器是否正确绑定

## 验收标准

### 必须通过
- [x] 编译成功，无 TypeScript 错误
- [ ] 所有 6 个 CSS 文件加载成功（状态码 200）
- [ ] 所有 7 个 JavaScript 文件加载成功（状态码 200）
- [ ] Import Map 正确生成并包含所有模块映射
- [ ] 页面样式完整显示（无白屏或无样式 HTML）
- [ ] Tab 切换功能正常
- [ ] 输入和发送消息功能正常
- [ ] Console 无加载错误

### 应该通过
- [ ] 消息渲染样式正常
- [ ] Markdown 和代码高亮正常
- [ ] 刷新后样式和功能正常
- [ ] 多窗口测试正常

## 测试结果

**测试日期**: ___________
**测试人**: ___________
**VSCode 版本**: ___________
**MultiCLI 版本**: ___________

### 通过的测试项
-

### 失败的测试项
-

### 发现的问题
-

### 备注
-

---

**下一步**: 如果所有测试通过，更新 UI_REFACTOR_EXECUTION.md 标记 Phase 7 完成。
