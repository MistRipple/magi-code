# GitHub 技能仓库支持

## 功能说明

MultiCLI 现在支持两种类型的技能仓库：

1. **JSON 仓库**：直接指向 JSON 文件的 URL（如 Gist Raw URL）
2. **GitHub 仓库**：GitHub 项目仓库 URL（自动读取仓库中的 `skills.json` 文件）

## GitHub 仓库使用方法

### 1. 创建 GitHub 技能仓库

在你的 GitHub 仓库根目录创建 `skills.json` 文件：

```json
{
  "name": "我的技能仓库",
  "description": "自定义技能集合",
  "version": "1.0.0",
  "skills": [
    {
      "id": "example_skill",
      "name": "示例技能",
      "fullName": "example_skill_v1",
      "description": "这是一个示例技能",
      "author": "Your Name",
      "version": "1.0.0",
      "category": "example",
      "type": "client-side",
      "icon": "⚡"
    }
  ]
}
```

### 2. 添加到 MultiCLI

1. 打开 MultiCLI
2. 点击"管理技能仓库"
3. 输入 GitHub 仓库 URL：
   - ✅ `https://github.com/username/repo`
   - ✅ `https://github.com/username/repo.git`
4. 点击"添加"

### 3. 自动识别

系统会自动：
- 识别这是一个 GitHub 仓库
- 获取仓库信息（名称、描述）
- 读取 `skills.json` 文件（优先 `main` 分支，其次 `master` 分支）
- 解析技能列表
- 保存仓库类型为 `github`

## 技术实现

### 自动类型检测

```typescript
// 根据 URL 自动判断仓库类型
const isGitHub = repository.type === 'github' || repository.url.includes('github.com');

if (isGitHub) {
  // 使用 GitHub API 获取仓库信息
  const result = await this.fetchGitHubRepository(repository.url, repository.id);
} else {
  // 直接获取 JSON 文件
  const result = await this.fetchJSONRepository(repository.url, repository.id);
}
```

### GitHub API 调用

1. **获取仓库信息**：
   ```
   GET https://api.github.com/repos/{owner}/{repo}
   ```
   - 获取仓库名称
   - 获取仓库描述

2. **获取 skills.json**：
   ```
   GET https://raw.githubusercontent.com/{owner}/{repo}/main/skills.json
   ```
   - 优先尝试 `main` 分支
   - 失败则尝试 `master` 分支

### 分支支持

- ✅ `main` 分支（优先）
- ✅ `master` 分支（备选）
- ⏳ 未来可支持指定分支

## 示例仓库

### 示例 1: 基础仓库

**仓库结构**：
```
my-skills/
├── README.md
├── skills.json
└── docs/
    └── usage.md
```

**skills.json**：
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

**添加方式**：
```
https://github.com/username/my-skills
```

### 示例 2: 完整仓库

**仓库结构**：
```
advanced-skills/
├── README.md
├── skills.json
├── LICENSE
└── skills/
    ├── data-analyzer/
    │   └── README.md
    └── code-formatter/
        └── README.md
```

**skills.json**：
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

**添加方式**：
```
https://github.com/username/advanced-skills
```

## 对比：JSON 仓库 vs GitHub 仓库

| 特性 | JSON 仓库 | GitHub 仓库 |
|------|-----------|-------------|
| URL 格式 | Raw JSON URL | GitHub 仓库 URL |
| 示例 | `https://gist.githubusercontent.com/.../skills.json` | `https://github.com/user/repo` |
| 优点 | 简单直接 | 版本控制、协作、文档 |
| 缺点 | 无版本控制 | 需要 GitHub 账号 |
| 适用场景 | 快速测试、个人使用 | 团队协作、开源项目 |

## 配置文件格式

添加 GitHub 仓库后，`~/.multicli/skills.json` 中的配置：

```json
{
  "repositories": [
    {
      "id": "builtin",
      "url": "builtin"
    },
    {
      "id": "repo-1234567890",
      "url": "https://github.com/username/repo",
      "name": "我的技能仓库",
      "type": "github"
    }
  ]
}
```

**关键字段**：
- `type: "github"` - 标识这是 GitHub 仓库
- 系统会自动从 GitHub API 获取仓库信息
- 自动读取 `skills.json` 文件

## 错误处理

### 常见错误

1. **仓库不存在**
   ```
   ❌ 添加仓库失败: 无法验证仓库: Request failed with status code 404
   ```
   **解决**：检查仓库 URL 是否正确

2. **没有 skills.json 文件**
   ```
   ❌ 添加仓库失败: 无法验证仓库: Request failed with status code 404
   ```
   **解决**：在仓库根目录创建 `skills.json` 文件

3. **skills.json 格式错误**
   ```
   ❌ 添加仓库失败: 无法验证仓库: Invalid skills.json format: missing skills array
   ```
   **解决**：检查 JSON 格式，确保包含 `skills` 数组

4. **GitHub API 限流**
   ```
   ❌ 添加仓库失败: 无法验证仓库: API rate limit exceeded
   ```
   **解决**：等待一段时间后重试，或使用 GitHub Token（未来功能）

## 使用示例

### 添加 GitHub 仓库

1. **打开管理对话框**
   - 点击"管理技能仓库"按钮

2. **输入 GitHub URL**
   ```
   https://github.com/anthropics/claude-code
   ```

3. **点击添加**
   - 系统自动验证仓库
   - 获取仓库信息
   - 读取 skills.json
   - 显示成功消息：`仓库 "claude-code" 已添加（X 个技能）`

4. **查看技能**
   - 点击"安装 Skill"按钮
   - 在技能列表中看到新仓库的技能
   - 按仓库分组显示

### 刷新仓库

1. **打开管理对话框**
2. **找到 GitHub 仓库**
3. **点击刷新按钮**
   - 按钮显示旋转动画
   - 清除缓存
   - 重新从 GitHub 获取最新的 skills.json

## 优势

### 1. 版本控制
- 使用 Git 管理技能版本
- 可以回滚到历史版本
- 查看修改历史

### 2. 协作
- 多人协作开发技能
- Pull Request 审查
- Issue 跟踪

### 3. 文档
- README.md 说明文档
- 每个技能可以有独立文档
- Wiki 支持

### 4. 开源
- 公开分享技能
- 社区贡献
- Star 和 Fork

### 5. CI/CD
- 自动测试
- 自动发布
- 质量保证

## 未来功能

- [ ] 支持指定分支（如 `https://github.com/user/repo@dev`）
- [ ] 支持 GitHub Token（避免 API 限流）
- [ ] 支持私有仓库
- [ ] 支持 GitLab、Gitee 等其他 Git 平台
- [ ] 自动检测仓库更新
- [ ] 技能版本管理

## 总结

GitHub 仓库支持让技能管理更加专业和便捷：

✅ **自动识别**：无需手动指定类型
✅ **智能获取**：自动读取 skills.json
✅ **版本控制**：利用 Git 的强大功能
✅ **团队协作**：支持多人开发
✅ **开源分享**：方便社区贡献

现在你可以直接使用 GitHub 仓库 URL 添加技能仓库了！
