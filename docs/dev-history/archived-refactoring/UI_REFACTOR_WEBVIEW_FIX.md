# UI 重构 - Webview 路径修复

## 问题描述

UI 重构完成后，样式完全没有加载，页面显示为无样式的纯 HTML。

## 根本原因

VSCode Webview 中的资源（CSS、JS）需要使用特殊的 URI 格式，不能使用相对路径。重构后的模块化文件路径没有被正确转换为 Webview URI。

## 修复内容

### 1. 更新 `webview-provider.ts` 的 `getHtmlContent` 方法

**修复前**：
```typescript
// 只处理单个 styles.css 文件
const stylesUri = webview.asWebviewUri(
  vscode.Uri.file(path.join(this.extensionUri.fsPath, 'src', 'ui', 'webview', 'styles.css'))
);
html = html.replace('href="styles.css"', `href="${stylesUri}"`);
```

**修复后**：
```typescript
// 处理 6 个模块化 CSS 文件
const cssFiles = ['base.css', 'layout.css', 'components.css', 'messages.css', 'settings.css', 'modals.css'];
cssFiles.forEach(cssFile => {
  const cssUri = webview.asWebviewUri(
    vscode.Uri.file(path.join(this.extensionUri.fsPath, 'src', 'ui', 'webview', 'styles', cssFile))
  );
  html = html.replace(`href="styles/${cssFile}"`, `href="${cssUri}"`);
});

// 处理 JavaScript 主入口
const mainJsUri = webview.asWebviewUri(
  vscode.Uri.file(path.join(this.extensionUri.fsPath, 'src', 'ui', 'webview', 'js', 'main.js'))
);
html = html.replace('src="js/main.js"', `src="${mainJsUri}"`);
```

### 2. 添加 Import Map 支持 ES6 模块

**问题**：ES6 模块的相对导入（如 `import { ... } from './core/state.js'`）在 Webview 中无法正常工作。

**解决方案**：动态生成 Import Map，将相对路径映射到完整的 Webview URI。

```typescript
// 创建 import map 来处理 ES6 模块的相对导入
const jsModules = [
  'core/state.js',
  'core/utils.js',
  'core/vscode-api.js',
  'ui/message-renderer.js',
  'ui/message-handler.js',
  'ui/event-handlers.js'
];

const imports: Record<string, string> = {};
jsModules.forEach(modulePath => {
  const moduleUri = webview.asWebviewUri(
    vscode.Uri.file(path.join(this.extensionUri.fsPath, 'src', 'ui', 'webview', 'js', modulePath))
  );
  imports[`./${modulePath}`] = moduleUri.toString();
});

const importMap = `<script type="importmap">
{
  "imports": ${JSON.stringify(imports, null, 2)}
}
</script>`;

// 在 </head> 之前插入 import map
html = html.replace('</head>', `${importMap}\n</head>`);
```

## 修复后的效果

1. **CSS 正确加载**：所有 6 个 CSS 模块文件都能正确加载
2. **JavaScript 模块正常工作**：ES6 模块的 import/export 正常工作
3. **样式完整显示**：页面样式恢复正常

## 技术细节

### VSCode Webview URI 格式

VSCode Webview 使用特殊的 URI 格式来访问本地文件：

```
vscode-webview://[authority]/[path]
```

通过 `webview.asWebviewUri()` 方法将本地文件路径转换为 Webview URI。

### Import Map

Import Map 是一个 Web 标准，允许控制 JavaScript 模块的导入解析。在 Webview 中使用 Import Map 可以将相对路径映射到完整的 URI。

**示例**：
```html
<script type="importmap">
{
  "imports": {
    "./core/state.js": "vscode-webview://xxx/core/state.js",
    "./core/utils.js": "vscode-webview://xxx/core/utils.js"
  }
}
</script>
```

## 测试验证

1. 编译项目：`npm run compile`
2. 重新加载 VSCode 窗口
3. 打开 MultiCLI 面板
4. 验证样式正确显示
5. 验证 JavaScript 功能正常

## 相关文件

- `src/ui/webview-provider.ts` - Webview 提供者，处理资源路径转换
- `src/ui/webview/index.html` - HTML 模板
- `src/ui/webview/styles/*.css` - 6 个 CSS 模块
- `src/ui/webview/js/**/*.js` - 7 个 JavaScript 模块

## 注意事项

1. **Import Map 兼容性**：Import Map 是较新的 Web 标准，VSCode 内置的 Chromium 版本需要支持此特性
2. **路径一致性**：确保 HTML 中的路径与 `getHtmlContent` 中的替换逻辑一致
3. **模块列表维护**：如果添加新的 JavaScript 模块，需要更新 `jsModules` 数组

## 后续优化建议

1. **自动发现模块**：可以通过文件系统扫描自动发现所有 JavaScript 模块，而不是硬编码列表
2. **缓存优化**：考虑添加版本号或哈希值到文件名，利用浏览器缓存
3. **开发模式**：添加开发模式，支持热重载

---

**修复完成时间**: 2024-01-22
**修复人**: Claude
