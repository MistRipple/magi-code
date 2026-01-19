# CLI 询问功能测试指南

**日期**: 2026-01-18
**修复提交**: e076575

---

## 测试环境

CLI 询问功能需要在 **VS Code 扩展环境** 中测试，因为它涉及：
- Webview 前端交互
- 用户输入处理
- 实时消息流

---

## 测试准备

### 1. 启动 VS Code 扩展开发环境

```bash
# 在 VS Code 中按 F5 启动扩展开发主机
# 或者使用命令面板：
# "Debug: Start Debugging"
```

### 2. 打开浏览器开发者工具

在扩展开发主机中：
1. 打开 MultiCLI Webview
2. 右键点击 Webview → "检查元素"
3. 打开 Console 标签页查看日志

---

## 测试场景

### 场景 1: 基本 CLI 询问显示 ✅

**目标**: 验证 CLI 询问卡片正确显示

**步骤**:
1. 在 MultiCLI 中输入任务：
   ```
   使用 git 命令检查当前仓库状态
   ```

2. 如果 git 需要配置用户信息，会触发 CLI 询问

**预期结果**:
- ✅ 在 CLI 面板显示黄色询问卡片
- ✅ 卡片显示"等待回答"状态
- ✅ 卡片内容完整显示询问文本
- ✅ 输入框占位符更新为"回答 xxx 的询问..."

**验证日志**:
```javascript
// 浏览器控制台应该显示：
[Webview] 收到标准消息: msg-xxx interaction streaming
[Webview] 收到交互消息: question req-xxx
```

---

### 场景 2: 回答 CLI 询问 ✅

**目标**: 验证回答功能正常工作

**步骤**:
1. 在询问卡片中输入回答
2. 点击"发送回答"按钮

**预期结果**:
- ✅ 卡片状态更新为绿色"已回答"
- ✅ 显示回答内容
- ✅ CLI 继续执行任务
- ✅ 输入框占位符恢复正常

**验证日志**:
```javascript
// 浏览器控制台应该显示：
[Webview] 发送 CLI 询问回答: claude req-xxx your-answer
```

---

### 场景 3: 询问超时 ⏱️

**目标**: 验证超时机制正常工作

**步骤**:
1. 触发 CLI 询问
2. 等待 60 秒不回答

**预期结果**:
- ✅ 卡片状态更新为红色"已超时"
- ✅ 显示自动回答内容 "n (超时自动回答)"
- ✅ CLI 继续执行（使用默认回答）

**验证日志**:
```javascript
// 后端日志应该显示：
[PrintSession] CLI 询问超时: req-xxx
```

---

### 场景 4: 去重机制 🔄

**目标**: 验证相同询问不会重复显示

**步骤**:
1. 触发相同的 CLI 询问多次
2. 观察 CLI 面板

**预期结果**:
- ✅ 只显示一个询问卡片
- ✅ 不会出现 5 个重复的卡片（之前的 bug）

**验证日志**:
```javascript
// 浏览器控制台应该显示：
[Webview] 收到交互消息: question req-xxx
// 后续相同的询问应该被去重，不会再次显示
```

---

## 调试技巧

### 1. 查看前端日志

在浏览器控制台中：
```javascript
// 查看所有 CLI 输出
console.log(cliOutputs);

// 查看当前待处理的询问
console.log(window._pendingCliQuestion);

// 查看标准消息缓存
console.log(standardMessages);
```

### 2. 查看后端日志

```bash
# 查看最新的日志文件
tail -f /Users/xie/code/MultiCLI/.multicli-logs/multicli-*.log | grep -i "question\|interaction"
```

### 3. 触发测试询问

如果难以触发自然的 CLI 询问，可以：

**方法 1**: 使用 git 命令
```
执行 git commit，但不配置用户信息
```

**方法 2**: 使用交互式命令
```
运行一个需要用户确认的命令
```

---

## 验证清单

### 前端验证 ✅

- [ ] `handleInteractionMessage` 函数被调用
- [ ] 询问卡片正确渲染
- [ ] 卡片内容完整显示
- [ ] 状态颜色正确（黄色→绿色/红色）
- [ ] 输入框占位符正确更新

### 后端验证 ✅

- [ ] `CLIQuestion` 事件正确发送
- [ ] `StandardMessage` 格式正确
- [ ] `interaction` 字段存在
- [ ] `content` 字段有值（不是 `text`）

### 类型验证 ✅

- [ ] TypeScript 编译通过
- [ ] 无类型错误
- [ ] 事件监听器类型正确

---

## 常见问题排查

### 问题 1: 询问卡片不显示

**检查**:
1. 浏览器控制台是否有错误？
2. 是否收到 `standardMessage` 事件？
3. `message.type` 是否为 `'interaction'`？
4. `message.interaction` 是否存在？

**解决**:
```javascript
// 在浏览器控制台中检查
window.addEventListener('message', (event) => {
  if (event.data.type === 'standardMessage') {
    console.log('收到标准消息:', event.data.message);
  }
});
```

### 问题 2: 询问内容为空

**检查**:
1. `message.interaction.prompt` 是否有值？
2. 前端是否正确提取 `interaction.prompt`？

**解决**:
- 查看 `handleInteractionMessage` 函数中的 `interaction.prompt`

### 问题 3: 回答无法发送

**检查**:
1. `answerCliQuestion` 函数是否存在？
2. `vscode.postMessage` 是否正常工作？

**解决**:
```javascript
// 在浏览器控制台中测试
window.answerCliQuestion('claude', 'test-question-id', 'test-answer', 'worker');
```

---

## 成功标准

### 所有场景通过 ✅

- ✅ 场景 1: CLI 询问正确显示
- ✅ 场景 2: 可以正常回答
- ✅ 场景 3: 超时机制正常
- ✅ 场景 4: 去重机制有效

### 无错误日志 ✅

- ✅ 浏览器控制台无错误
- ✅ 后端日志无错误
- ✅ TypeScript 编译无错误

### 代码质量 ✅

- ✅ 无死代码
- ✅ 类型安全
- ✅ 代码简洁

---

## 测试报告模板

```markdown
## CLI 询问功能测试报告

**测试日期**: YYYY-MM-DD
**测试人员**: [姓名]
**提交版本**: e076575

### 测试结果

| 场景 | 状态 | 备注 |
|------|------|------|
| 场景 1: 基本显示 | ✅/❌ | |
| 场景 2: 回答询问 | ✅/❌ | |
| 场景 3: 询问超时 | ✅/❌ | |
| 场景 4: 去重机制 | ✅/❌ | |

### 发现的问题

1. [问题描述]
   - 重现步骤：
   - 预期结果：
   - 实际结果：
   - 错误日志：

### 总体评价

- [ ] 所有功能正常
- [ ] 部分功能有问题
- [ ] 需要进一步修复

### 建议

[测试建议和改进意见]
```

---

**状态**: 📋 测试指南已完成
**下一步**: 在 VS Code 扩展环境中执行测试
