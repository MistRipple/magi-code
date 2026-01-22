# 技能仓库使用指南

## 问题诊断

### 当前问题

用户添加的仓库 URL `https://github.com/anthropics/claude-code` 不是一个有效的 JSON 技能仓库。

**错误原因**：
- 这个 URL 指向的是 GitHub 仓库页面（HTML），不是 JSON 文件
- 技能仓库必须是一个可以通过 HTTP GET 请求访问的 JSON 文件

### 测试结果

```
测试仓库: repo-1769008007266
  发送请求: https://github.com/anthropics/claude-code
  ✅ 请求成功
  状态码: 200
  ❌ 格式错误: 缺少 name 字段
```

**解释**：虽然请求成功（返回 200），但返回的是 HTML 页面，不是 JSON 格式的技能仓库。

---

## 正确的技能仓库格式

### JSON 文件结构

技能仓库必须是一个 JSON 文件，包含以下字段：

```json
{
  "name": "仓库名称",
  "description": "仓库描述（可选）",
  "version": "1.0.0（可选）",
  "skills": [
    {
      "id": "skill_id",
      "name": "Skill Name",
      "fullName": "skill_full_name_v1",
      "description": "技能描述",
      "author": "作者（可选）",
      "version": "1.0.0（可选）",
      "category": "分类（可选）",
      "type": "server-side 或 client-side（可选）",
      "icon": "图标（可选）"
    }
  ]
}
```

### 必需字段

**仓库级别**：
- `name` (string) - 仓库名称，会显示在 UI 中
- `skills` (array) - 技能数组

**技能级别**：
- `id` (string) - 技能唯一标识符
- `name` (string) - 技能显示名称
- `fullName` (string) - 技能完整名称（用于安装）
- `description` (string) - 技能描述

### 可选字段

**仓库级别**：
- `description` (string) - 仓库描述
- `version` (string) - 仓库版本

**技能级别**：
- `author` (string) - 作者名称
- `version` (string) - 技能版本
- `category` (string) - 技能分类
- `type` (string) - 技能类型（"server-side" 或 "client-side"）
- `icon` (string) - 技能图标（emoji 或 SVG）

---

## 如何创建技能仓库

### 方法 1: 使用 GitHub Gist

1. **创建 Gist**
   - 访问 https://gist.github.com/
   - 创建一个新的 Gist
   - 文件名：`skills.json`
   - 内容：参考上面的 JSON 格式

2. **获取 Raw URL**
   - 点击 "Raw" 按钮
   - 复制 URL（格式：`https://gist.githubusercontent.com/username/xxx/raw/xxx/skills.json`）

3. **添加到 MultiCLI**
   - 在 MultiCLI 中点击"管理技能仓库"
   - 粘贴 Raw URL
   - 点击"添加"

### 方法 2: 使用 GitHub 仓库

1. **创建仓库**
   - 在 GitHub 创建一个新仓库
   - 添加 `skills.json` 文件到仓库根目录

2. **获取 Raw URL**
   - 打开 `skills.json` 文件
   - 点击 "Raw" 按钮
   - 复制 URL（格式：`https://raw.githubusercontent.com/username/repo/main/skills.json`）

3. **添加到 MultiCLI**
   - 在 MultiCLI 中点击"管理技能仓库"
   - 粘贴 Raw URL
   - 点击"添加"

### 方法 3: 使用自己的服务器

1. **上传 JSON 文件**
   - 将 `skills.json` 上传到你的服务器
   - 确保文件可以通过 HTTP GET 访问

2. **配置 CORS（如果需要）**
   - 如果服务器有 CORS 限制，需要配置允许跨域访问

3. **添加到 MultiCLI**
   - 在 MultiCLI 中点击"管理技能仓库"
   - 输入完整的 URL（如 `https://example.com/skills.json`）
   - 点击"添加"

---

## 示例仓库

### 示例 1: 基础仓库

```json
{
  "name": "我的技能仓库",
  "skills": [
    {
      "id": "hello_world",
      "name": "Hello World",
      "fullName": "hello_world_v1",
      "description": "一个简单的示例技能"
    }
  ]
}
```

### 示例 2: 完整仓库

```json
{
  "name": "高级技能仓库",
  "description": "包含多个高级技能的仓库",
  "version": "2.0.0",
  "skills": [
    {
      "id": "data_analyzer",
      "name": "数据分析器",
      "fullName": "data_analyzer_v2",
      "description": "分析和可视化数据",
      "author": "John Doe",
      "version": "2.0.0",
      "category": "data",
      "type": "server-side",
      "icon": "📊"
    },
    {
      "id": "code_formatter",
      "name": "代码格式化",
      "fullName": "code_formatter_v1",
      "description": "自动格式化代码",
      "author": "Jane Smith",
      "version": "1.5.0",
      "category": "development",
      "type": "client-side",
      "icon": "✨"
    }
  ]
}
```

---

## 常见错误

### 错误 1: URL 指向 HTML 页面

**错误信息**：
```
❌ 格式错误: 缺少 name 字段
```

**原因**：URL 返回的是 HTML 页面，不是 JSON 文件

**解决方法**：
- 如果是 GitHub 仓库，使用 Raw URL
- 如果是 Gist，使用 Raw URL
- 确保 URL 直接返回 JSON 内容

**示例**：
- ❌ 错误：`https://github.com/username/repo`
- ❌ 错误：`https://github.com/username/repo/blob/main/skills.json`
- ✅ 正确：`https://raw.githubusercontent.com/username/repo/main/skills.json`

### 错误 2: JSON 格式错误

**错误信息**：
```
❌ 格式错误: 不是对象
```

**原因**：JSON 格式不正确（语法错误、缺少引号等）

**解决方法**：
- 使用 JSON 验证工具（如 https://jsonlint.com/）
- 检查是否有多余的逗号
- 检查是否缺少引号或括号

### 错误 3: 缺少必需字段

**错误信息**：
```
❌ 格式错误: 缺少 name 字段
❌ 格式错误: 缺少 skills 数组
```

**原因**：JSON 文件缺少必需的字段

**解决方法**：
- 确保包含 `name` 字段（仓库名称）
- 确保包含 `skills` 数组
- 确保每个技能包含 `id`, `name`, `fullName`, `description`

### 错误 4: 网络错误

**错误信息**：
```
❌ 请求失败: timeout of 10000ms exceeded
❌ 请求失败: getaddrinfo ENOTFOUND
```

**原因**：网络连接问题或 URL 无效

**解决方法**：
- 检查网络连接
- 确认 URL 是否正确
- 尝试在浏览器中打开 URL，看是否能访问

---

## 修复当前问题

### 步骤 1: 删除无效仓库

1. 打开 MultiCLI
2. 点击"管理技能仓库"
3. 找到 `https://github.com/anthropics/claude-code` 仓库
4. 点击"删除"按钮

### 步骤 2: 创建测试仓库

使用项目中的 `example-skill-repository.json` 文件：

1. **上传到 GitHub Gist**
   - 访问 https://gist.github.com/
   - 创建新 Gist
   - 文件名：`skills.json`
   - 内容：复制 `example-skill-repository.json` 的内容
   - 点击 "Create public gist"
   - 点击 "Raw" 按钮获取 URL

2. **添加到 MultiCLI**
   - 在 MultiCLI 中点击"管理技能仓库"
   - 粘贴 Raw URL
   - 点击"添加"

### 步骤 3: 验证

1. 点击"刷新"按钮（应该看到旋转动画）
2. 点击"安装 Skill"按钮
3. 应该能看到两个仓库：
   - Claude 官方技能（4 个技能）
   - 示例技能仓库（2 个技能）

---

## 调试技巧

### 1. 使用测试脚本

运行项目中的测试脚本：

```bash
node test-skill-loading.js
```

这会检查：
- 配置文件是否存在
- 仓库配置是否正确
- 每个仓库的 URL 是否可访问
- JSON 格式是否正确

### 2. 检查浏览器控制台

1. 打开 VS Code 开发者工具
   - 按 `Cmd+Shift+P` (Mac) 或 `Ctrl+Shift+P` (Windows/Linux)
   - 输入 "Developer: Toggle Developer Tools"

2. 打开 Console 标签

3. 点击"安装 Skill"按钮

4. 查看日志：
   ```
   [Skill Library] Opening dialog with skills: undefined
   [Skill Library] Requesting skills from backend
   [Skill Library] Received skills from backend: [...]
   [Skill Library] Skills grouped by repository: {...}
   ```

### 3. 检查输出面板

1. 打开输出面板
   - 按 `Cmd+Shift+U` (Mac) 或 `Ctrl+Shift+U` (Windows/Linux)

2. 选择 "MultiCLI" 输出通道

3. 查看后端日志：
   ```
   [LLM] Loaded repositories for skill library { count: 2, ... }
   [TOOLS] Fetching skills from repositories { totalRepos: 2 }
   [TOOLS] JSON repository fetched { name: '...', skillCount: 2 }
   ```

---

## 总结

### 问题根源

用户添加的 URL 不是有效的 JSON 技能仓库，而是 GitHub 仓库页面。

### 解决方案

1. **删除无效仓库**：删除 `https://github.com/anthropics/claude-code`
2. **使用正确的 URL**：使用 Raw URL 或 Gist URL
3. **验证格式**：确保 JSON 文件包含必需的字段

### 功能改进

1. ✅ **刷新按钮动画**：已添加旋转动画和禁用状态
2. ✅ **详细日志**：已添加前后端详细日志
3. ✅ **测试脚本**：已创建测试脚本用于诊断
4. ✅ **示例文件**：已创建示例 JSON 文件

### 下一步

1. 用户删除无效仓库
2. 用户创建正确的 JSON 仓库（使用 Gist 或 Raw URL）
3. 用户添加新仓库并测试
4. 如果还有问题，运行测试脚本并提供日志
