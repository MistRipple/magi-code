# Skill Library Loading Debug

## Issue
用户报告：点击"安装 Skill"按钮后，自定义仓库的技能列表没有加载显示。

## 调试步骤

### 1. 后端日志增强

**文件**: `src/ui/webview-provider.ts`

在 `handleLoadSkillLibrary()` 方法中添加了详细的日志：

```typescript
// 加载仓库配置
const repositories = LLMConfigLoader.loadRepositories();
logger.info('Loaded repositories for skill library', {
  count: repositories.length,
  repositories: repositories.map((r: any) => ({ id: r.id, url: r.url }))
}, LogCategory.TOOLS);

// 获取所有 Skills
const manager = new SkillRepositoryManager();
const skills = await manager.getAllSkills(repositories);
logger.info('Fetched skills from all repositories', {
  totalSkills: skills.length,
  byRepository: skills.reduce((acc: any, skill) => {
    acc[skill.repositoryId] = (acc[skill.repositoryId] || 0) + 1;
    return acc;
  }, {})
}, LogCategory.TOOLS);
```

**日志输出内容**：
- 加载了多少个仓库
- 每个仓库的 ID 和 URL
- 总共获取了多少个技能
- 每个仓库分别有多少个技能

### 2. 前端日志增强

**文件**: `src/ui/webview/index.html`

#### 2.1 消息接收日志

```javascript
else if (msg.type === 'skillLibraryLoaded') {
  console.log('[Skill Library] Received skills from backend:', msg.skills);
  showSkillLibraryDialog(msg.skills);
}
```

#### 2.2 对话框打开日志

```javascript
function showSkillLibraryDialog(skills) {
  console.log('[Skill Library] Opening dialog with skills:', skills);
  // ...

  if (skills) {
    console.log('[Skill Library] Rendering provided skills');
    renderSkillLibrary(skills);
  } else {
    console.log('[Skill Library] Requesting skills from backend');
    vscode.postMessage({ type: 'loadSkillLibrary' });
  }
}
```

#### 2.3 渲染日志

```javascript
function renderSkillLibrary(skills) {
  console.log('[Skill Library] Rendering skills:', skills);
  const listEl = document.getElementById('skill-library-list');
  if (!listEl) {
    console.error('[Skill Library] List element not found');
    return;
  }

  if (!skills || skills.length === 0) {
    console.warn('[Skill Library] No skills to display');
    // ...
    return;
  }

  // 按仓库分组
  const skillsByRepo = {};
  // ...

  console.log('[Skill Library] Skills grouped by repository:', skillsByRepo);

  // 渲染
  // ...

  console.log('[Skill Library] Rendered successfully');
}
```

### 3. UI 改进

#### 3.1 添加技能数量显示

在仓库分组标题中显示技能数量：

```javascript
<div style="font-size: 13px; font-weight: 600; margin-bottom: 12px; color: var(--vscode-descriptionForeground);">
  ${repoData.name} (${repoData.skills.length} 个技能)
</div>
```

#### 3.2 添加搜索数据属性

为每个技能项添加 `data-skill-name` 和 `data-skill-desc` 属性，支持搜索功能：

```javascript
<div class="skill-library-item"
  data-skill-name="${skill.name}"
  data-skill-desc="${skill.description}"
  style="...">
```

## 预期日志输出

### 正常流程

1. **用户点击"安装 Skill"按钮**
   ```
   [Skill Library] Opening dialog with skills: undefined
   [Skill Library] Requesting skills from backend
   ```

2. **后端加载仓库**
   ```
   [LLM] Loaded repositories for skill library { count: 2, repositories: [{ id: 'builtin', url: 'builtin' }, { id: 'repo-xxx', url: 'https://...' }] }
   ```

3. **后端获取技能**
   ```
   [TOOLS] Fetching skills from repositories { totalRepos: 2 }
   [TOOLS] Fetching JSON repository { url: 'https://...', repositoryId: 'repo-xxx' }
   [TOOLS] JSON repository fetched { url: 'https://...', repositoryId: 'repo-xxx', name: '社区仓库', skillCount: 5 }
   [LLM] Fetched skills from all repositories { totalSkills: 9, byRepository: { builtin: 4, 'repo-xxx': 5 } }
   [LLM] Skill library loaded { totalSkills: 9, installedCount: 2 }
   ```

4. **前端接收并渲染**
   ```
   [Skill Library] Received skills from backend: [{ id: 'web_search', ... }, ...]
   [Skill Library] Opening dialog with skills: [{ id: 'web_search', ... }, ...]
   [Skill Library] Rendering provided skills
   [Skill Library] Rendering skills: [{ id: 'web_search', ... }, ...]
   [Skill Library] Skills grouped by repository: { builtin: { name: 'Claude 官方技能', skills: [...] }, 'repo-xxx': { name: '社区仓库', skills: [...] } }
   [Skill Library] Rendered successfully
   ```

### 异常情况

#### 情况 1: 仓库配置为空
```
[LLM] Loaded repositories for skill library { count: 1, repositories: [{ id: 'builtin', url: 'builtin' }] }
[LLM] Fetched skills from all repositories { totalSkills: 4, byRepository: { builtin: 4 } }
```
**原因**: 自定义仓库没有保存到配置文件

#### 情况 2: 网络请求失败
```
[TOOLS] Failed to fetch JSON repository { url: 'https://...', repositoryId: 'repo-xxx', error: 'timeout of 10000ms exceeded' }
[TOOLS] Failed to fetch repository { repositoryId: 'repo-xxx', error: 'timeout of 10000ms exceeded' }
[LLM] Fetched skills from all repositories { totalSkills: 4, byRepository: { builtin: 4 } }
```
**原因**: 网络超时或 URL 无效

#### 情况 3: JSON 格式错误
```
[TOOLS] Failed to fetch JSON repository { url: 'https://...', repositoryId: 'repo-xxx', error: 'Invalid repository format: missing name field' }
```
**原因**: 仓库 JSON 格式不符合要求

#### 情况 4: 前端没有收到消息
```
[Skill Library] Opening dialog with skills: undefined
[Skill Library] Requesting skills from backend
(没有后续日志)
```
**原因**: 后端消息处理失败或消息没有发送

## 排查步骤

1. **打开 VS Code 开发者工具**
   - 按 `Cmd+Shift+P` (Mac) 或 `Ctrl+Shift+P` (Windows/Linux)
   - 输入 "Developer: Toggle Developer Tools"
   - 打开 Console 标签

2. **打开输出面板**
   - 按 `Cmd+Shift+U` (Mac) 或 `Ctrl+Shift+U` (Windows/Linux)
   - 选择 "MultiCLI" 输出通道

3. **点击"安装 Skill"按钮**
   - 观察 Console 中的前端日志
   - 观察输出面板中的后端日志

4. **根据日志判断问题**
   - 如果没有前端日志：按钮事件绑定失败
   - 如果没有后端日志：消息没有发送或处理失败
   - 如果仓库数量为 1：自定义仓库没有保存
   - 如果网络错误：检查 URL 和网络连接
   - 如果格式错误：检查 JSON 仓库格式

## 验证自定义仓库配置

检查 `~/.multicli/skills.json` 文件：

```json
{
  "builtInTools": { ... },
  "customTools": [],
  "repositories": [
    {
      "id": "builtin",
      "url": "builtin"
    },
    {
      "id": "repo-1234567890",
      "url": "https://example.com/skills.json",
      "name": "社区仓库"
    }
  ]
}
```

**检查点**：
- `repositories` 数组中是否包含自定义仓库
- 自定义仓库的 `url` 是否正确
- 自定义仓库的 `id` 是否唯一

## 验证 JSON 仓库格式

自定义仓库的 JSON 格式必须符合以下要求：

```json
{
  "name": "仓库名称",
  "description": "仓库描述（可选）",
  "version": "1.0（可选）",
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

**必需字段**：
- `name` (仓库名称)
- `skills` (技能数组)
- 每个技能的 `id`, `name`, `fullName`

## 编译状态

✅ 编译成功，0 错误

```bash
npm run compile
```

## 下一步

1. 用户测试并提供日志输出
2. 根据日志定位具体问题
3. 修复问题并验证
