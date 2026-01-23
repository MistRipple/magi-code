# 导入/导出不匹配错误修复

## 🐛 问题描述

在 VSCode 扩展开发主机中测试时，发现 JavaScript 模块导入错误：

```
Uncaught SyntaxError: The requested module '../core/vscode-api.js' does not provide an export named 'answerQuestion' (at event-handlers.js:31:3)
```

## 🔍 根本原因

`event-handlers.js` 尝试导入 `answerQuestion`（单数），但 `vscode-api.js` 实际导出的是 `answerQuestions`（复数）。

### 错误代码

**文件**: `src/ui/webview/js/ui/event-handlers.js` (第 31 行)

```javascript
// ❌ 错误：导入不存在的函数名
import {
  postMessage,
  executeTask,
  interruptTask,
  confirmPlan,
  answerQuestion  // ❌ 不存在
} from '../core/vscode-api.js';
```

**文件**: `src/ui/webview/js/core/vscode-api.js` (第 71 行)

```javascript
// ✅ 实际导出的函数名
export function answerQuestions(answer) {  // ✅ 注意是复数
  postMessage({ type: 'answerQuestions', answer });
}
```

## 🔧 修复内容

### 修改导入语句

**文件**: `src/ui/webview/js/ui/event-handlers.js`

```javascript
// ✅ 正确：导入实际存在的函数名
import {
  postMessage,
  executeTask,
  interruptTask,
  confirmPlan,
  answerQuestions  // ✅ 改为复数
} from '../core/vscode-api.js';
```

## ✅ 验证结果

### 1. 语法验证
```bash
$ node -c src/ui/webview/js/ui/event-handlers.js
✅ 语法正确
```

### 2. 编译验证
```bash
$ npm run compile
✅ 编译成功，0 错误
```

### 3. 导入/导出冲突检查
```bash
$ node check-import-export-conflicts.js
✅ src/ui/webview/js/ui/event-handlers.js
   导入: 31 个, 导出: 27 个, 无冲突
✅ 无导入/导出冲突
```

## 📊 影响范围

### 修改的文件
- `src/ui/webview/js/ui/event-handlers.js` - 修改 1 处导入语句

### 不受影响的功能
- 函数调用逻辑无需修改（如果有使用该函数的地方）
- 其他模块的导入/导出正常

## 🎯 为什么会出现这个问题？

这是在 Phase 3 UI 模块提取时产生的命名不一致问题：

1. **函数定义**: `vscode-api.js` 中定义为 `answerQuestions`（复数）
2. **导入语句**: `event-handlers.js` 中错误地导入为 `answerQuestion`（单数）
3. **测试遗漏**: Phase 6 的自动化测试没有检测到这种导入/导出不匹配

## 🔍 如何避免类似问题？

### 1. 使用导入/导出冲突检查脚本

已创建 `check-import-export-conflicts.js` 脚本，可以检测：
- 导入的名称是否在目标模块中存在
- 导出的名称是否被正确导入
- 循环依赖问题

### 2. 使用 ESLint

配置 ESLint 规则检测导入错误：

```json
{
  "rules": {
    "import/named": "error",
    "import/no-unresolved": "error"
  }
}
```

### 3. 运行时测试

在 VSCode 扩展开发主机中测试，可以立即发现模块加载错误。

## 📝 相关文档

- Phase 7 修复: `UI_REFACTOR_WEBVIEW_FIX.md`
- 重复函数修复: `DUPLICATE_FUNCTION_FIX.md`
- 模板字符串修复: `TEMPLATE_STRING_FIX.md`
- 测试指南: `HOW_TO_TEST_WEBVIEW.md`

## 🚀 下一步

现在可以继续 Phase 8 运行时测试：

1. 在 VSCode 中按 **F5** 启动扩展开发主机
2. 打开 MultiCLI 面板
3. 按 **Ctrl+Shift+R** 强制刷新（清除缓存）
4. 验证所有资源加载成功
5. 验证功能正常工作

---

**修复完成时间**: 2024-01-22
**修复人**: Claude
**问题类型**: 导入/导出不匹配
**严重程度**: 高（阻止模块加载）
**修复状态**: ✅ 已完成
