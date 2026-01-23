# 前端代码 CLI 清理计划

## 📊 清理范围

根据 grep 统计，前端代码中共有 **568 处** `cli` 相关内容需要清理。

### 文件分布
| 文件 | 出现次数 | 优先级 |
|------|---------|--------|
| index.html.backup | 284 | 🟢 低（备份文件，可删除） |
| index.html | 59 | 🔴 高 |
| js/ui/message-renderer.js | 44 | 🔴 高 |
| js/ui/message-handler.js | 48 | 🔴 高 |
| styles/components.css | 32 | 🟡 中 |
| styles/messages.css | 34 | 🟡 中 |
| styles/settings.css | 21 | 🟡 中 |
| js/ui/settings-handler.js | 14 | 🟡 中 |
| js/main.js | 12 | 🔴 高 |
| js/core/state.js | 10 | 🔴 高 |
| styles/layout.css | 5 | 🟢 低 |
| js/ui/event-handlers.js | 3 | 🟡 中 |
| js/core/vscode-api.js | 2 | 🟡 中 |

---

## 🎯 清理策略

### 阶段 1: 删除备份文件
- 删除 `index.html.backup`（284 处，无需清理）

### 阶段 2: JavaScript 代码清理（高优先级）
需要清理的核心 JS 文件：
1. `js/core/state.js` - 状态管理
2. `js/main.js` - 主逻辑
3. `js/ui/message-handler.js` - 消息处理
4. `js/ui/message-renderer.js` - 消息渲染
5. `js/ui/event-handlers.js` - 事件处理
6. `js/ui/settings-handler.js` - 设置处理
7. `js/core/vscode-api.js` - VSCode API

### 阶段 3: HTML 清理
- `index.html` - 主 HTML 文件

### 阶段 4: CSS 清理（低优先级）
- `styles/components.css`
- `styles/messages.css`
- `styles/settings.css`
- `styles/layout.css`

---

## 📝 清理规则

### JavaScript 变量和字段
```javascript
// ❌ 旧代码
cli: 'claude'
cliType: 'claude'
data.cli
message.cli
state.cli
processingActor.cli

// ✅ 新代码
agent: 'claude'
data.agent
message.agent
state.agent
processingActor.agent
```

### HTML 属性和 ID
```html
<!-- ❌ 旧代码 -->
<div id="cli-panel">
<button class="cli-button">
data-cli="claude"

<!-- ✅ 新代码 -->
<div id="agent-panel">
<button class="agent-button">
data-agent="claude"
```

### CSS 类名和选择器
```css
/* ❌ 旧代码 */
.cli-panel { }
#cli-output { }
.cli-message { }

/* ✅ 新代码 */
.agent-panel { }
#agent-output { }
.agent-message { }
```

---

## ⚠️ 注意事项

1. **保持功能一致**: 只改名称，不改逻辑
2. **同步更新**: 前后端字段名必须一致
3. **测试验证**: 每个文件修改后都要测试
4. **备份重要**: 修改前确保有备份

---

## 🚀 执行顺序

### 第一步: 删除备份文件
```bash
rm src/ui/webview/index.html.backup
```

### 第二步: 核心 JS 文件（按依赖顺序）
1. `js/core/state.js` - 状态定义
2. `js/core/vscode-api.js` - API 封装
3. `js/ui/event-handlers.js` - 事件处理
4. `js/ui/settings-handler.js` - 设置
5. `js/ui/message-renderer.js` - 渲染
6. `js/ui/message-handler.js` - 消息处理
7. `js/main.js` - 主逻辑

### 第三步: HTML
8. `index.html`

### 第四步: CSS（可选）
9. CSS 文件（如果时间允许）

---

**创建日期**: 2025-01-22
**状态**: 待执行
