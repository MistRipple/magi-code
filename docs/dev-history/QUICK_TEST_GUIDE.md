# 🚀 快速测试指南

## ✅ 已完成的修复

**问题**：添加 `https://github.com/anthropics/claude-code` 时显示错误

**解决方案**：实现了自动检测和转换 Claude Code 插件格式

**状态**：
- ✅ 代码已修改
- ✅ 编译成功（0 错误）
- ✅ 自动检测测试通过
- ✅ 准备好测试

## 🧪 立即测试

### 步骤 1: 重新加载插件

1. 在 VS Code 中按 `Cmd+Shift+P`（Mac）或 `Ctrl+Shift+P`（Windows/Linux）
2. 输入：`Developer: Reload Window`
3. 回车（重新加载 VS Code 窗口）

### 步骤 2: 添加 Claude Code 仓库

1. 打开 MultiCLI
2. 点击"管理技能仓库"按钮
3. 在输入框中输入：
   ```
   https://github.com/anthropics/claude-code
   ```
4. 点击"添加"按钮

### 步骤 3: 验证结果

**预期结果**：
- ✅ 显示成功提示："仓库 \"claude-code (Claude Code Plugins)\" 已添加（13 个技能）"
- ✅ 仓库出现在列表中
- ✅ 仓库名称：`claude-code (Claude Code Plugins)`

### 步骤 4: 查看技能

1. 点击"安装 Skill"按钮
2. 查看技能列表

**预期结果**：
- ✅ 看到"claude-code (Claude Code Plugins)"分组
- ✅ 包含 13 个技能：
  1. Agent SDK Development Plugin
  2. Claude Opus 4.5 Migration Plugin
  3. Code Review Plugin
  4. Commit Commands Plugin
  5. Explanatory Output Style Plugin
  6. Feature Development Plugin
  7. Frontend Design Plugin
  8. Hookify Plugin
  9. Learning Style Plugin
  10. Plugin Development Toolkit
  11. PR Review Toolkit
  12. Ralph Wiggum Plugin
  13. Security Guidance

### 步骤 5: 检查日志（可选）

**浏览器控制台**：
1. 按 `Cmd+Shift+P` → "Developer: Toggle Developer Tools"
2. 查看 Console 标签

**输出面板**：
1. 按 `Cmd+Shift+U`
2. 选择 "MultiCLI" 通道

**预期日志**：
```
[TOOLS] Fetching GitHub repository
[TOOLS] No skills.json found, trying Claude Code plugins format
[TOOLS] Trying to fetch Claude Code plugins
[TOOLS] Found Claude Code plugins directory { pluginCount: 13 }
[TOOLS] Claude Code plugins converted { pluginCount: 13 }
```

## ❌ 如果测试失败

### 问题 1: 仍然显示错误

**可能原因**：VS Code 没有重新加载插件

**解决方案**：
1. 完全关闭 VS Code
2. 重新打开 VS Code
3. 再次测试

### 问题 2: 显示其他错误

**解决方案**：
1. 查看浏览器控制台的完整错误消息
2. 查看输出面板的日志
3. 将错误消息发送给我

### 问题 3: 编译错误

**解决方案**：
```bash
cd /Users/xie/code/MultiCLI
npm run compile
```

如果有错误，将错误消息发送给我。

## 📊 测试清单

- [ ] VS Code 已重新加载
- [ ] 打开 MultiCLI
- [ ] 点击"管理技能仓库"
- [ ] 输入 Claude Code URL
- [ ] 点击"添加"
- [ ] 看到成功提示
- [ ] 仓库出现在列表中
- [ ] 点击"安装 Skill"
- [ ] 看到 13 个 Claude Code 插件
- [ ] 可以安装技能

## 🎯 成功标准

如果以下所有条件都满足，说明修复成功：

1. ✅ 添加 Claude Code 仓库时**不再显示错误**
2. ✅ 显示成功提示
3. ✅ 仓库出现在列表中
4. ✅ 可以看到 13 个插件
5. ✅ 可以安装插件

## 📝 测试后反馈

请告诉我：

1. **测试是否成功？**
   - ✅ 成功：所有步骤都正常
   - ❌ 失败：哪一步失败了？

2. **如果失败，错误消息是什么？**
   - 浏览器控制台的错误
   - 输出面板的日志

3. **其他问题或建议**

---

## 🔧 技术细节

### 实现原理

1. **检测 skills.json**
   - 先尝试标准格式（main/master 分支）

2. **自动回退到 Claude Code 格式**
   - 如果没有 skills.json，检查 `/plugins` 目录
   - 如果存在，自动转换插件

3. **转换插件**
   - 读取每个插件的 README.md
   - 提取标题和描述
   - 生成技能格式

4. **返回结果**
   - 仓库名称：`{repo} (Claude Code Plugins)`
   - 技能列表：所有插件

### 支持的仓库类型

1. **标准技能仓库**（优先）
   - 包含 `skills.json` 文件
   - 示例：自定义技能仓库

2. **Claude Code 插件仓库**（自动检测）
   - 包含 `/plugins` 目录
   - 示例：`https://github.com/anthropics/claude-code`

3. **Raw JSON URL**
   - 直接指向 JSON 文件
   - 示例：Gist Raw URL

---

**准备好了吗？开始测试吧！** 🚀
