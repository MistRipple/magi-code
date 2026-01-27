# VSCode 终端功能测试指南

## ⚠️ 重要说明

**我必须诚实地告诉你：我还没有进行实际测试！**

虽然我已经完成了代码实现和编译，但作为 AI 助手，我无法直接运行 VSCode 扩展来验证终端是否真的能打开。

**你需要亲自测试来确认功能是否正常工作。**

---

## 🧪 测试方法

### 方法 1: 使用测试命令（推荐）

我已经创建了一个专门的测试命令，可以快速验证终端功能。

#### 步骤：

1. **启动扩展调试**
   ```
   按 F5 启动扩展开发主机
   ```

2. **打开命令面板**
   ```
   按 Cmd+Shift+P (Mac) 或 Ctrl+Shift+P (Windows/Linux)
   ```

3. **执行测试命令**
   ```
   输入: MultiCLI: Test Terminal
   或直接输入: multicli.testTerminal
   ```

4. **观察结果**
   - ✅ 应该会打开一个输出面板显示测试结果
   - ✅ 应该会看到 VSCode 终端窗口被创建
   - ✅ 应该会看到命令在终端中执行

#### 预期输出：

```
=== VSCode Terminal Executor 测试 ===

测试 1: 执行简单命令（后台）
✅ 命令执行成功
   退出码: 0
   输出: Hello from MultiCLI
   耗时: XXXms

测试 2: 显示终端窗口执行命令
✅ 终端窗口已打开
   退出码: 0
   输出: This should appear in a terminal window
   耗时: XXXms

测试 3: 列出当前目录文件
✅ 命令执行成功
   退出码: 0
   输出长度: XXX 字符
   耗时: XXXms

测试 4: 检查进程管理
✅ 当前进程数: X
   进程 1: ... (状态: completed)

=== 所有测试完成 ===
✅ VSCode Terminal Executor 工作正常！
```

---

### 方法 2: 通过 MultiCLI 对话测试

1. **启动扩展调试** (F5)

2. **打开 MultiCLI 面板**
   - 点击侧边栏的 MultiCLI 图标
   - 或按 Cmd+Shift+P 执行 "MultiCLI: Open Panel"

3. **发送测试消息**
   ```
   请使用终端执行命令: echo "测试终端功能"
   ```

4. **观察 AI 的响应**
   - AI 应该会调用 `execute_shell` 工具
   - 如果 AI 使用了 `showTerminal: true` 参数，应该会看到终端窗口

---

### 方法 3: 手动代码测试

如果上述方法都不行，可以创建一个简单的测试脚本：

#### 创建测试文件 `test-terminal-manual.ts`：

```typescript
import * as vscode from 'vscode';
import { VSCodeTerminalExecutor } from './tools/vscode-terminal-executor';

// 在扩展激活后的某个地方调用
async function quickTest() {
  const executor = new VSCodeTerminalExecutor();
  
  // 测试：显示终端执行命令
  const result = await executor.execute({
    command: 'echo "Hello from VSCode Terminal!"',
    showTerminal: true,
    keepTerminalOpen: true,
    name: 'Quick Test',
  });
  
  console.log('Result:', result);
}
```

---

## 🔍 可能遇到的问题

### 问题 1: 终端没有打开

**可能原因：**
- `showTerminal` 参数没有传递
- VSCode API 调用失败
- 权限问题

**调试方法：**
```typescript
// 在 vscode-terminal-executor.ts 中添加日志
console.log('Creating terminal with options:', options);
console.log('Terminal created:', terminal);
console.log('Showing terminal...');
terminal.show(true);
```

### 问题 2: 命令没有执行

**可能原因：**
- Shell Integration 不可用
- 命令语法错误
- 超时设置太短

**调试方法：**
```typescript
// 检查 Shell Integration
if (terminal.shellIntegration) {
  console.log('✅ Shell Integration available');
} else {
  console.log('❌ Shell Integration not available, using fallback');
}
```

### 问题 3: 输出捕获失败

**可能原因：**
- `for await` 循环问题
- 流读取错误
- VSCode API 版本不兼容

**调试方法：**
```typescript
// 在 executeWithShellIntegration 中添加日志
for await (const data of stream) {
  console.log('Received data:', data);
  output += data;
}
console.log('Final output:', output);
```

---

## 📋 测试检查清单

请按照以下清单逐项测试：

### 基础功能测试

- [ ] **编译成功** - `npm run compile` 无错误
- [ ] **扩展启动** - F5 启动扩展开发主机成功
- [ ] **测试命令可用** - 命令面板中能找到 "MultiCLI: Test Terminal"

### 终端创建测试

- [ ] **创建终端** - 能看到新的终端标签页被创建
- [ ] **终端命名** - 终端名称显示正确（如 "MultiCLI Test Terminal"）
- [ ] **终端显示** - 终端窗口自动显示（当 `showTerminal: true`）

### 命令执行测试

- [ ] **简单命令** - `echo "test"` 能正常执行
- [ ] **输出捕获** - 能捕获命令的标准输出
- [ ] **退出码** - 能正确获取命令退出码
- [ ] **工作目录** - `cwd` 参数生效

### Shell Integration 测试

- [ ] **检测可用性** - 能检测 Shell Integration 是否可用
- [ ] **流式读取** - `for await` 循环能正常读取输出
- [ ] **降级方案** - 无 Shell Integration 时能使用 `sendText` 降级

### 进程管理测试

- [ ] **进程列表** - `listProcesses()` 能返回正确的进程列表
- [ ] **进程状态** - 能正确跟踪进程状态（running/completed/killed）
- [ ] **终端清理** - 命令完成后终端能正确清理（当 `keepTerminalOpen: false`）

### 集成测试

- [ ] **ToolManager 集成** - ToolManager 能正确选择执行器
- [ ] **参数传递** - 终端参数能正确传递到执行器
- [ ] **错误处理** - 错误能被正确捕获和报告

---

## 🐛 如果测试失败

### 1. 检查 VSCode 版本

VSCode Shell Integration 需要 **VSCode 1.93+**

```bash
# 检查 VSCode 版本
code --version
```

### 2. 检查日志

打开 VSCode 开发者工具：
```
帮助 > 切换开发人员工具
```

查看 Console 中的错误信息。

### 3. 检查 package.json

确认 `engines.vscode` 版本要求：
```json
{
  "engines": {
    "vscode": "^1.93.0"
  }
}
```

### 4. 降级测试

如果 Shell Integration 不可用，测试降级方案：
```typescript
// 强制使用 sendText 方案
const result = await executor.execute({
  command: 'echo "test"',
  showTerminal: true,
  // Shell Integration 会自动降级
});
```

---

## 📊 测试报告模板

测试完成后，请填写以下报告：

```markdown
## VSCode 终端功能测试报告

**测试日期：** YYYY-MM-DD
**VSCode 版本：** X.XX.X
**操作系统：** macOS / Windows / Linux

### 测试结果

| 测试项 | 结果 | 备注 |
|--------|------|------|
| 编译成功 | ✅ / ❌ | |
| 扩展启动 | ✅ / ❌ | |
| 终端创建 | ✅ / ❌ | |
| 命令执行 | ✅ / ❌ | |
| 输出捕获 | ✅ / ❌ | |
| Shell Integration | ✅ / ❌ / N/A | |
| 进程管理 | ✅ / ❌ | |

### 遇到的问题

1. 问题描述...
2. 错误信息...

### 截图

（如果可能，请提供截图）

### 结论

- [ ] ✅ 功能完全正常
- [ ] ⚠️ 部分功能正常
- [ ] ❌ 功能不正常

### 建议

（如果有改进建议，请在此说明）
```

---

## 🎯 下一步

1. **立即测试**
   - 按 F5 启动扩展
   - 执行 `multicli.testTerminal` 命令
   - 观察结果

2. **报告结果**
   - 如果成功：太好了！功能正常工作
   - 如果失败：请提供错误信息，我会帮你修复

3. **实际使用**
   - 在 MultiCLI 对话中测试
   - 让 AI 执行需要终端的命令
   - 验证用户体验

---

## ⚠️ 我的承诺

**我承认：我还没有实际测试过这个功能。**

虽然我：
- ✅ 参考了 Augment 插件的实现
- ✅ 使用了正确的 VSCode API
- ✅ 编写了完整的代码
- ✅ 编译通过了

但是：
- ❌ 我没有实际运行过扩展
- ❌ 我没有看到终端真的打开
- ❌ 我不能100%保证功能正常

**所以，你的测试非常重要！**

如果测试失败，请告诉我：
1. 具体的错误信息
2. Console 中的日志
3. 预期行为 vs 实际行为

我会立即帮你修复问题！

---

**🙏 感谢你的理解和测试！**

