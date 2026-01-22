# Skill UI 显示问题修复

## 问题描述

用户反馈：点击"安装 Skill"后，弹窗关闭了，但是 Skills 列表没有显示任何已安装的 Skill。

## 根本原因

1. **`renderSkillsToolList()` 函数只显示空状态**
   - 原来的实现只是硬编码显示"暂无已安装的 Skill"
   - 没有读取 `skillsConfig` 来渲染已安装的 Skills

2. **Skill 库对话框没有检查安装状态**
   - `loadSkillLibrary()` 中所有 Skills 的 `installed` 都是 `false`
   - 没有根据 `skillsConfig` 来判断哪些 Skills 已经安装

## 修复方案

### 1. 修复 `renderSkillsToolList()` 函数

**文件**: `src/ui/webview/index.html`

**修改内容**:

```javascript
function renderSkillsToolList() {
  const listEl = document.getElementById('skills-tool-list');
  if (!listEl) return;

  // 检查是否有已安装的 Skills
  if (!skillsConfig || !skillsConfig.builtInTools) {
    // 显示空状态
    return;
  }

  // 获取已启用的 Skills
  const enabledSkills = [];
  for (const [toolName, toolConfig] of Object.entries(skillsConfig.builtInTools)) {
    if (toolConfig.enabled) {
      enabledSkills.push({
        name: toolName,
        description: toolConfig.description || '',
        enabled: true
      });
    }
  }

  // 如果没有启用的 Skills，显示空状态
  if (enabledSkills.length === 0) {
    // 显示空状态
    return;
  }

  // 渲染已安装的 Skills
  let html = '<div class="skills-tool-list">';

  for (const skill of enabledSkills) {
    // 判断是服务器端还是客户端工具
    const isServerSide = skill.name.includes('web_search') || skill.name.includes('web_fetch');
    const typeLabel = isServerSide ? 'Server' : 'Client';
    const typeColor = isServerSide ? '#0078d4' : '#e91e63';

    html += `
      <div class="skills-tool-item">
        <div class="skills-tool-icon" style="background: ${typeColor}20; color: ${typeColor};">
          <svg>...</svg>
        </div>
        <div class="skills-tool-info">
          <div class="skills-tool-header">
            <span class="skills-tool-name">${skill.name}</span>
            <span class="skills-tool-type" style="background: ${typeColor}20; color: ${typeColor};">${typeLabel}</span>
          </div>
          <div class="skills-tool-description">${skill.description}</div>
        </div>
        <label class="skills-tool-toggle">
          <input type="checkbox" checked disabled>
          <span class="toggle-slider"></span>
        </label>
      </div>
    `;
  }

  html += '</div>';
  listEl.innerHTML = html;
}
```

**改进点**:
- ✅ 读取 `skillsConfig.builtInTools` 获取已安装的 Skills
- ✅ 遍历所有启用的工具并渲染
- ✅ 显示工具名称、描述、类型（Server/Client）
- ✅ 区分服务器端和客户端工具（不同颜色）
- ✅ 显示启用状态（checkbox 已选中且禁用）

### 2. 修复 `loadSkillLibrary()` 函数

**文件**: `src/ui/webview/index.html`

**修改内容**:

```javascript
function loadSkillLibrary() {
  const listEl = document.getElementById('skill-library-list');
  if (!listEl) return;

  // Skill 库映射（skillId -> 完整的 skill name）
  const skillIdToName = {
    'web_search': 'web_search_20250305',
    'web_fetch': 'web_fetch_20250305',
    'text_editor': 'text_editor_20250124',
    'computer_use': 'computer_use_20241022'
  };

  // Skill 库（只包含 4 个真实的 Claude Skills）
  const skills = [
    {
      id: 'web_search',
      name: 'Web Search',
      description: '搜索网络以获取最新信息',
      author: 'Anthropic',
      version: '1.0.0',
      category: 'search',
      installed: false
    },
    {
      id: 'web_fetch',
      name: 'Web Fetch',
      description: '获取并分析网页内容',
      author: 'Anthropic',
      version: '1.0.0',
      category: 'web',
      installed: false
    },
    {
      id: 'text_editor',
      name: 'Text Editor',
      description: '编辑文本文件',
      author: 'Anthropic',
      version: '1.0.0',
      category: 'development',
      installed: false
    },
    {
      id: 'computer_use',
      name: 'Computer Use',
      description: '控制计算机（需要额外权限）',
      author: 'Anthropic',
      version: '1.0.0',
      category: 'system',
      installed: false
    }
  ];

  // ✅ 检查哪些 Skills 已经安装
  if (skillsConfig && skillsConfig.builtInTools) {
    for (const skill of skills) {
      const fullName = skillIdToName[skill.id];
      if (fullName && skillsConfig.builtInTools[fullName]) {
        skill.installed = skillsConfig.builtInTools[fullName].enabled === true;
      }
    }
  }

  // 渲染 Skill 列表
  listEl.innerHTML = skills.map(skill => `
    <div class="skill-library-item">
      ...
      <button class="settings-btn ${skill.installed ? '' : 'primary'}"
        data-skill-id="${skill.id}"
        onclick="installSkill('${skill.id}')"
        ${skill.installed ? 'disabled' : ''}>
        ${skill.installed ? '已安装' : '安装'}
      </button>
    </div>
  `).join('');
}
```

**改进点**:
- ✅ 添加 `skillIdToName` 映射，将简短 ID 映射到完整的工具名称
- ✅ 只显示 4 个真实的 Claude Skills（移除了假的 code_analyzer 和 data_processor）
- ✅ 检查 `skillsConfig.builtInTools` 来判断哪些 Skills 已安装
- ✅ 已安装的 Skill 按钮显示"已安装"并禁用
- ✅ 未安装的 Skill 按钮显示"安装"并可点击

## 完整的工作流程

### 安装 Skill 流程

```
1. 用户点击"安装 Skill"按钮
   ↓
2. 显示 Skill 库对话框
   ↓
3. loadSkillLibrary() 检查 skillsConfig，标记已安装的 Skills
   ↓
4. 用户点击某个 Skill 的"安装"按钮
   ↓
5. installSkill(skillId) 发送消息到后端
   ↓
6. 后端 handleInstallSkill() 处理：
   - 保存配置到 ~/.multicli/skills.json
   - 调用 adapterFactory.reloadSkills()
   - 重新加载配置到前端
   ↓
7. 前端收到 skillsConfigLoaded 消息
   ↓
8. renderSkillsToolList() 渲染已安装的 Skills
   ↓
9. 用户看到 Skills 列表中显示新安装的 Skill
```

### 显示已安装 Skills 流程

```
1. 页面加载时调用 initSkillsConfig()
   ↓
2. 发送 loadSkillsConfig 消息到后端
   ↓
3. 后端加载 ~/.multicli/skills.json
   ↓
4. 发送 skillsConfigLoaded 消息到前端
   ↓
5. 前端更新 skillsConfig 变量
   ↓
6. 调用 renderSkillsToolList()
   ↓
7. 遍历 skillsConfig.builtInTools
   ↓
8. 渲染所有 enabled: true 的 Skills
   ↓
9. 用户看到已安装的 Skills 列表
```

## UI 效果

### Skills 列表显示

```
┌─────────────────────────────────────────────────────┐
│ Claude Skills（内置工具）                            │
│                                                       │
│ ┌─────────────────────────────────────────────────┐ │
│ │ [🔧] web_search_20250305          [Server] [✓] │ │
│ │      搜索网络以获取最新信息                      │ │
│ └─────────────────────────────────────────────────┘ │
│                                                       │
│ ┌─────────────────────────────────────────────────┐ │
│ │ [🔧] web_fetch_20250305           [Server] [✓] │ │
│ │      获取网页内容                                │ │
│ └─────────────────────────────────────────────────┘ │
│                                                       │
│ [+ 安装 Skill]                                        │
└─────────────────────────────────────────────────────┘
```

### Skill 库对话框

```
┌─────────────────────────────────────────────────────┐
│ Skill 库                                        [×] │
├─────────────────────────────────────────────────────┤
│ [搜索框]                                             │
│                                                       │
│ ┌─────────────────────────────────────────────────┐ │
│ │ [🔧] Web Search                    [已安装]     │ │
│ │      搜索网络以获取最新信息                      │ │
│ │      作者: Anthropic | 版本: 1.0.0 | 分类: search│ │
│ └─────────────────────────────────────────────────┘ │
│                                                       │
│ ┌─────────────────────────────────────────────────┐ │
│ │ [🔧] Web Fetch                     [已安装]     │ │
│ │      获取并分析网页内容                          │ │
│ │      作者: Anthropic | 版本: 1.0.0 | 分类: web  │ │
│ └─────────────────────────────────────────────────┘ │
│                                                       │
│ ┌─────────────────────────────────────────────────┐ │
│ │ [🔧] Text Editor                   [安装]       │ │
│ │      编辑文本文件                                │ │
│ │      作者: Anthropic | 版本: 1.0.0 | 分类: dev  │ │
│ └─────────────────────────────────────────────────┘ │
│                                                       │
│                                          [关闭]      │
└─────────────────────────────────────────────────────┘
```

## 验证方法

### 1. 检查 Skills 列表显示

1. 打开插件设置面板
2. 切换到"工具"标签
3. 查看"Claude Skills（内置工具）"部分
4. 应该看到已安装的 Skills 列表

### 2. 检查 Skill 库对话框

1. 点击"安装 Skill"按钮
2. 查看 Skill 库对话框
3. 已安装的 Skills 应该显示"已安装"并禁用按钮
4. 未安装的 Skills 应该显示"安装"并可点击

### 3. 测试安装流程

1. 在 Skill 库中选择一个未安装的 Skill
2. 点击"安装"按钮
3. 对话框关闭
4. 查看 Skills 列表，应该显示新安装的 Skill
5. 再次打开 Skill 库，该 Skill 应该显示"已安装"

### 4. 检查配置文件

查看 `~/.multicli/skills.json`：

```json
{
  "builtInTools": {
    "web_search_20250305": {
      "enabled": true,
      "description": "搜索网络以获取最新信息"
    },
    "web_fetch_20250305": {
      "enabled": true,
      "description": "获取网页内容"
    }
  },
  "customTools": []
}
```

## 编译状态

✅ **编译成功，0 错误**

```bash
npm run compile
> multicli@0.1.0 compile
> tsc -p ./
```

## 总结

### 修复的问题

✅ **Skills 列表现在正确显示已安装的 Skills**
- 读取 `skillsConfig` 数据
- 渲染所有启用的工具
- 显示工具名称、描述、类型

✅ **Skill 库对话框正确显示安装状态**
- 检查 `skillsConfig` 判断哪些已安装
- 已安装的显示"已安装"并禁用
- 未安装的显示"安装"并可点击

✅ **安装后立即更新 UI**
- 后端保存配置后重新加载
- 前端收到更新后重新渲染
- 用户立即看到新安装的 Skill

### 改进点

1. **数据驱动渲染**：UI 完全基于 `skillsConfig` 数据渲染
2. **状态同步**：前后端状态保持一致
3. **用户体验**：安装后立即看到效果，无需刷新
4. **视觉反馈**：清晰的类型标签（Server/Client）和颜色区分
