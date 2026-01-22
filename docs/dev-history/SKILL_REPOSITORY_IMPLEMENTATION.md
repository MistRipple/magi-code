# Skill 仓库支持实现方案

## 目标

支持用户配置自定义 Skill 仓库，从多个来源安装 Skills。

## 当前状态

- ✅ Skills 安装功能已实现
- ✅ Skills 显示在列表中
- ❌ 仅支持硬编码的 4 个 Claude Skills
- ❌ 不支持自定义仓库

## 实现方案

### 1. 配置结构扩展

**文件**: `~/.multicli/skills.json`

```json
{
  "builtInTools": {
    "web_search_20250305": {
      "enabled": true,
      "description": "搜索网络以获取最新信息"
    }
  },
  "customTools": [],
  "repositories": [
    {
      "id": "official",
      "name": "官方仓库",
      "url": "https://raw.githubusercontent.com/anthropics/claude-skills/main/registry.json",
      "enabled": true,
      "type": "json"
    },
    {
      "id": "custom-1",
      "name": "自定义仓库",
      "url": "https://example.com/skills.json",
      "enabled": true,
      "type": "json"
    }
  ]
}
```

### 2. 仓库格式定义

**JSON 仓库格式** (`registry.json`):

```json
{
  "version": "1.0",
  "skills": [
    {
      "id": "web_search",
      "name": "Web Search",
      "fullName": "web_search_20250305",
      "description": "搜索网络以获取最新信息",
      "author": "Anthropic",
      "version": "1.0.0",
      "category": "search",
      "type": "server-side",
      "icon": "🔍"
    },
    {
      "id": "custom_skill",
      "name": "Custom Skill",
      "fullName": "custom_skill_v1",
      "description": "自定义技能示例",
      "author": "Community",
      "version": "1.0.0",
      "category": "custom",
      "type": "client-side",
      "icon": "⚙️"
    }
  ]
}
```

### 3. 后端实现

#### 3.1 扩展 LLMConfigLoader

**文件**: `src/llm/config.ts`

```typescript
/**
 * 加载仓库配置
 */
static loadRepositories(): RepositoryConfig[] {
  const config = this.loadSkillsConfig();
  return config?.repositories || this.getDefaultRepositories();
}

/**
 * 保存仓库配置
 */
static saveRepositories(repositories: RepositoryConfig[]): void {
  const config = this.loadSkillsConfig() || {
    builtInTools: {},
    customTools: [],
    repositories: []
  };
  config.repositories = repositories;
  this.saveSkillsConfig(config);
}

/**
 * 添加仓库
 */
static addRepository(repository: RepositoryConfig): void {
  const repositories = this.loadRepositories();
  repositories.push(repository);
  this.saveRepositories(repositories);
}

/**
 * 更新仓库
 */
static updateRepository(id: string, updates: Partial<RepositoryConfig>): void {
  const repositories = this.loadRepositories();
  const index = repositories.findIndex(r => r.id === id);
  if (index >= 0) {
    repositories[index] = { ...repositories[index], ...updates };
    this.saveRepositories(repositories);
  }
}

/**
 * 删除仓库
 */
static deleteRepository(id: string): void {
  const repositories = this.loadRepositories();
  const filtered = repositories.filter(r => r.id !== id);
  this.saveRepositories(filtered);
}

/**
 * 获取默认仓库
 */
private static getDefaultRepositories(): RepositoryConfig[] {
  return [
    {
      id: 'builtin',
      name: '内置 Skills',
      url: 'builtin',
      enabled: true,
      type: 'builtin'
    }
  ];
}
```

#### 3.2 创建 SkillRepositoryManager

**文件**: `src/tools/skill-repository-manager.ts`

```typescript
import axios from 'axios';
import { logger, LogCategory } from '../logging';

export interface RepositoryConfig {
  id: string;
  name: string;
  url: string;
  enabled: boolean;
  type: 'builtin' | 'json' | 'api';
}

export interface SkillInfo {
  id: string;
  name: string;
  fullName: string;
  description: string;
  author: string;
  version: string;
  category: string;
  type: 'server-side' | 'client-side';
  icon?: string;
  repositoryId: string;
}

export class SkillRepositoryManager {
  private cache: Map<string, SkillInfo[]> = new Map();
  private cacheExpiry: Map<string, number> = new Map();
  private readonly CACHE_TTL = 5 * 60 * 1000; // 5 分钟

  /**
   * 获取内置 Skills
   */
  private getBuiltInSkills(): SkillInfo[] {
    return [
      {
        id: 'web_search',
        name: 'Web Search',
        fullName: 'web_search_20250305',
        description: '搜索网络以获取最新信息',
        author: 'Anthropic',
        version: '1.0.0',
        category: 'search',
        type: 'server-side',
        icon: '🔍',
        repositoryId: 'builtin'
      },
      {
        id: 'web_fetch',
        name: 'Web Fetch',
        fullName: 'web_fetch_20250305',
        description: '获取并分析网页内容',
        author: 'Anthropic',
        version: '1.0.0',
        category: 'web',
        type: 'server-side',
        icon: '🌐',
        repositoryId: 'builtin'
      },
      {
        id: 'text_editor',
        name: 'Text Editor',
        fullName: 'text_editor_20250124',
        description: '编辑文本文件',
        author: 'Anthropic',
        version: '1.0.0',
        category: 'development',
        type: 'client-side',
        icon: '📝',
        repositoryId: 'builtin'
      },
      {
        id: 'computer_use',
        name: 'Computer Use',
        fullName: 'computer_use_20241022',
        description: '控制计算机（需要额外权限）',
        author: 'Anthropic',
        version: '1.0.0',
        category: 'system',
        type: 'client-side',
        icon: '💻',
        repositoryId: 'builtin'
      }
    ];
  }

  /**
   * 从 JSON 仓库获取 Skills
   */
  private async fetchJSONRepository(url: string, repositoryId: string): Promise<SkillInfo[]> {
    try {
      const response = await axios.get(url, {
        timeout: 10000,
        headers: {
          'Accept': 'application/json'
        }
      });

      const data = response.data;
      if (!data || !Array.isArray(data.skills)) {
        throw new Error('Invalid repository format');
      }

      return data.skills.map((skill: any) => ({
        ...skill,
        repositoryId
      }));
    } catch (error: any) {
      logger.error('Failed to fetch JSON repository', {
        url,
        error: error.message
      }, LogCategory.TOOLS);
      throw error;
    }
  }

  /**
   * 从仓库获取 Skills（带缓存）
   */
  async fetchRepository(repository: RepositoryConfig): Promise<SkillInfo[]> {
    // 检查缓存
    const cached = this.cache.get(repository.id);
    const expiry = this.cacheExpiry.get(repository.id);
    if (cached && expiry && Date.now() < expiry) {
      return cached;
    }

    let skills: SkillInfo[];

    if (repository.type === 'builtin') {
      skills = this.getBuiltInSkills();
    } else if (repository.type === 'json') {
      skills = await this.fetchJSONRepository(repository.url, repository.id);
    } else {
      throw new Error(`Unsupported repository type: ${repository.type}`);
    }

    // 更新缓存
    this.cache.set(repository.id, skills);
    this.cacheExpiry.set(repository.id, Date.now() + this.CACHE_TTL);

    return skills;
  }

  /**
   * 获取所有启用仓库的 Skills
   */
  async getAllSkills(repositories: RepositoryConfig[]): Promise<SkillInfo[]> {
    const enabledRepos = repositories.filter(r => r.enabled);
    const results = await Promise.allSettled(
      enabledRepos.map(repo => this.fetchRepository(repo))
    );

    const allSkills: SkillInfo[] = [];
    results.forEach((result, index) => {
      if (result.status === 'fulfilled') {
        allSkills.push(...result.value);
      } else {
        logger.warn('Failed to fetch repository', {
          repository: enabledRepos[index].name,
          error: result.reason
        }, LogCategory.TOOLS);
      }
    });

    return allSkills;
  }

  /**
   * 清除缓存
   */
  clearCache(repositoryId?: string): void {
    if (repositoryId) {
      this.cache.delete(repositoryId);
      this.cacheExpiry.delete(repositoryId);
    } else {
      this.cache.clear();
      this.cacheExpiry.clear();
    }
  }
}
```

#### 3.3 扩展 WebviewProvider

**文件**: `src/ui/webview-provider.ts`

```typescript
// 新增消息处理
case 'loadRepositories':
  await this.handleLoadRepositories();
  break;

case 'addRepository':
  await this.handleAddRepository(message.repository);
  break;

case 'updateRepository':
  await this.handleUpdateRepository(message.repositoryId, message.updates);
  break;

case 'deleteRepository':
  await this.handleDeleteRepository(message.repositoryId);
  break;

case 'refreshRepository':
  await this.handleRefreshRepository(message.repositoryId);
  break;

case 'loadSkillLibrary':
  await this.handleLoadSkillLibrary();
  break;

// 实现方法
private async handleLoadRepositories(): Promise<void> {
  try {
    const { LLMConfigLoader } = await import('../llm/config');
    const repositories = LLMConfigLoader.loadRepositories();

    this.postMessage({
      type: 'repositoriesLoaded',
      repositories
    });
  } catch (error: any) {
    logger.error('Failed to load repositories', { error: error.message }, LogCategory.TOOLS);
    this.postMessage({
      type: 'toast',
      message: '加载仓库失败: ' + error.message,
      toastType: 'error'
    });
  }
}

private async handleAddRepository(repository: any): Promise<void> {
  try {
    const { LLMConfigLoader } = await import('../llm/config');
    LLMConfigLoader.addRepository(repository);

    this.postMessage({
      type: 'repositoryAdded',
      repository
    });

    this.postMessage({
      type: 'toast',
      message: `仓库 "${repository.name}" 已添加`,
      toastType: 'success'
    });

    await this.handleLoadRepositories();
  } catch (error: any) {
    logger.error('Failed to add repository', { error: error.message }, LogCategory.TOOLS);
    this.postMessage({
      type: 'toast',
      message: '添加仓库失败: ' + error.message,
      toastType: 'error'
    });
  }
}

private async handleUpdateRepository(repositoryId: string, updates: any): Promise<void> {
  try {
    const { LLMConfigLoader } = await import('../llm/config');
    LLMConfigLoader.updateRepository(repositoryId, updates);

    this.postMessage({
      type: 'repositoryUpdated',
      repositoryId
    });

    this.postMessage({
      type: 'toast',
      message: '仓库已更新',
      toastType: 'success'
    });

    await this.handleLoadRepositories();
  } catch (error: any) {
    logger.error('Failed to update repository', { error: error.message }, LogCategory.TOOLS);
    this.postMessage({
      type: 'toast',
      message: '更新仓库失败: ' + error.message,
      toastType: 'error'
    });
  }
}

private async handleDeleteRepository(repositoryId: string): Promise<void> {
  try {
    const { LLMConfigLoader } = await import('../llm/config');
    LLMConfigLoader.deleteRepository(repositoryId);

    this.postMessage({
      type: 'repositoryDeleted',
      repositoryId
    });

    this.postMessage({
      type: 'toast',
      message: '仓库已删除',
      toastType: 'success'
    });

    await this.handleLoadRepositories();
  } catch (error: any) {
    logger.error('Failed to delete repository', { error: error.message }, LogCategory.TOOLS);
    this.postMessage({
      type: 'toast',
      message: '删除仓库失败: ' + error.message,
      toastType: 'error'
    });
  }
}

private async handleRefreshRepository(repositoryId: string): Promise<void> {
  try {
    const { SkillRepositoryManager } = await import('../tools/skill-repository-manager');
    const manager = new SkillRepositoryManager();
    manager.clearCache(repositoryId);

    this.postMessage({
      type: 'repositoryRefreshed',
      repositoryId
    });

    this.postMessage({
      type: 'toast',
      message: '仓库缓存已清除',
      toastType: 'success'
    });
  } catch (error: any) {
    logger.error('Failed to refresh repository', { error: error.message }, LogCategory.TOOLS);
    this.postMessage({
      type: 'toast',
      message: '刷新仓库失败: ' + error.message,
      toastType: 'error'
    });
  }
}

private async handleLoadSkillLibrary(): Promise<void> {
  try {
    const { LLMConfigLoader } = await import('../llm/config');
    const { SkillRepositoryManager } = await import('../tools/skill-repository-manager');

    const repositories = LLMConfigLoader.loadRepositories();
    const manager = new SkillRepositoryManager();
    const skills = await manager.getAllSkills(repositories);

    // 检查哪些已安装
    const skillsConfig = LLMConfigLoader.loadSkillsConfig();
    const installedSkills = new Set<string>();
    if (skillsConfig && skillsConfig.builtInTools) {
      Object.keys(skillsConfig.builtInTools).forEach(name => {
        if (skillsConfig.builtInTools[name].enabled) {
          installedSkills.add(name);
        }
      });
    }

    const skillsWithStatus = skills.map(skill => ({
      ...skill,
      installed: installedSkills.has(skill.fullName)
    }));

    this.postMessage({
      type: 'skillLibraryLoaded',
      skills: skillsWithStatus
    });
  } catch (error: any) {
    logger.error('Failed to load skill library', { error: error.message }, LogCategory.TOOLS);
    this.postMessage({
      type: 'toast',
      message: '加载 Skill 库失败: ' + error.message,
      toastType: 'error'
    });
  }
}
```

### 4. 前端实现

#### 4.1 仓库管理 UI

在 Skills Tab 中添加仓库管理区域：

```html
<!-- 仓库管理 -->
<div class="settings-section">
  <div class="settings-section-header">
    <div class="settings-section-title">Skill 仓库</div>
    <button class="settings-btn primary" id="repo-add-btn">+ 添加仓库</button>
  </div>
  <div class="repo-list" id="repo-list">
    <!-- 仓库列表 -->
  </div>
</div>
```

#### 4.2 JavaScript 实现

```javascript
let repositories = [];

// 初始化仓库配置
function initRepositories() {
  vscode.postMessage({ type: 'loadRepositories' });
}

// 渲染仓库列表
function renderRepositoryList() {
  const listEl = document.getElementById('repo-list');
  if (!listEl) return;

  if (repositories.length === 0) {
    listEl.innerHTML = '<div class="empty-state">暂无仓库</div>';
    return;
  }

  listEl.innerHTML = repositories.map(repo => `
    <div class="repo-item">
      <div class="repo-info">
        <div class="repo-name">${repo.name}</div>
        <div class="repo-url">${repo.url}</div>
      </div>
      <div class="repo-actions">
        <label class="repo-toggle">
          <input type="checkbox" ${repo.enabled ? 'checked' : ''}
            onchange="toggleRepository('${repo.id}', this.checked)">
          <span>启用</span>
        </label>
        <button class="repo-action-btn" onclick="refreshRepository('${repo.id}')">刷新</button>
        <button class="repo-action-btn" onclick="editRepository('${repo.id}')">编辑</button>
        <button class="repo-action-btn danger" onclick="deleteRepository('${repo.id}')">删除</button>
      </div>
    </div>
  `).join('');
}

// 添加仓库
function showAddRepositoryDialog() {
  const dialogHTML = `
    <div class="modal-overlay" id="repo-dialog-overlay">
      <div class="modal-dialog">
        <div class="modal-header">
          <h3>添加 Skill 仓库</h3>
          <button class="modal-close" id="repo-dialog-close">×</button>
        </div>
        <div class="modal-body">
          <div class="form-field">
            <label>仓库名称</label>
            <input type="text" id="repo-name" placeholder="例如：社区仓库">
          </div>
          <div class="form-field">
            <label>仓库 URL</label>
            <input type="text" id="repo-url" placeholder="https://example.com/skills.json">
          </div>
          <div class="form-field">
            <label>仓库类型</label>
            <select id="repo-type">
              <option value="json">JSON</option>
            </select>
          </div>
        </div>
        <div class="modal-footer">
          <button class="settings-btn" id="repo-dialog-cancel">取消</button>
          <button class="settings-btn primary" id="repo-dialog-save">保存</button>
        </div>
      </div>
    </div>
  `;

  document.body.insertAdjacentHTML('beforeend', dialogHTML);

  document.getElementById('repo-dialog-close').addEventListener('click', closeRepositoryDialog);
  document.getElementById('repo-dialog-cancel').addEventListener('click', closeRepositoryDialog);
  document.getElementById('repo-dialog-save').addEventListener('click', saveRepository);
}

function saveRepository() {
  const name = document.getElementById('repo-name').value.trim();
  const url = document.getElementById('repo-url').value.trim();
  const type = document.getElementById('repo-type').value;

  if (!name || !url) {
    alert('请填写完整信息');
    return;
  }

  const repository = {
    id: 'repo-' + Date.now(),
    name,
    url,
    type,
    enabled: true
  };

  vscode.postMessage({
    type: 'addRepository',
    repository
  });

  closeRepositoryDialog();
}

function closeRepositoryDialog() {
  const dialog = document.getElementById('repo-dialog-overlay');
  if (dialog) dialog.remove();
}

// 切换仓库启用状态
function toggleRepository(id, enabled) {
  vscode.postMessage({
    type: 'updateRepository',
    repositoryId: id,
    updates: { enabled }
  });
}

// 刷新仓库
function refreshRepository(id) {
  vscode.postMessage({
    type: 'refreshRepository',
    repositoryId: id
  });
}

// 删除仓库
function deleteRepository(id) {
  if (confirm('确定要删除此仓库吗？')) {
    vscode.postMessage({
      type: 'deleteRepository',
      repositoryId: id
    });
  }
}

// 修改 loadSkillLibrary 函数
function loadSkillLibrary() {
  // 从后端加载所有仓库的 Skills
  vscode.postMessage({ type: 'loadSkillLibrary' });
}

// 处理后端消息
window.addEventListener('message', event => {
  const message = event.data;

  switch (message.type) {
    case 'repositoriesLoaded':
      repositories = message.repositories;
      renderRepositoryList();
      break;

    case 'skillLibraryLoaded':
      renderSkillLibrary(message.skills);
      break;

    // ... 其他消息处理
  }
});

// 渲染 Skill 库（支持多仓库）
function renderSkillLibrary(skills) {
  const listEl = document.getElementById('skill-library-list');
  if (!listEl) return;

  // 按仓库分组
  const groupedSkills = {};
  skills.forEach(skill => {
    if (!groupedSkills[skill.repositoryId]) {
      groupedSkills[skill.repositoryId] = [];
    }
    groupedSkills[skill.repositoryId].push(skill);
  });

  let html = '';
  Object.entries(groupedSkills).forEach(([repoId, repoSkills]) => {
    const repo = repositories.find(r => r.id === repoId);
    const repoName = repo ? repo.name : repoId;

    html += `
      <div class="skill-repo-group">
        <div class="skill-repo-header">${repoName}</div>
        ${repoSkills.map(skill => `
          <div class="skill-library-item">
            <div class="skill-library-icon">${skill.icon || '⚙️'}</div>
            <div class="skill-library-info">
              <div class="skill-name">${skill.name}</div>
              <div class="skill-desc">${skill.description}</div>
              <div class="skill-meta">
                作者: ${skill.author} | 版本: ${skill.version} | 分类: ${skill.category}
              </div>
            </div>
            <button class="settings-btn ${skill.installed ? '' : 'primary'}"
              onclick="installSkill('${skill.id}')" ${skill.installed ? 'disabled' : ''}>
              ${skill.installed ? '已安装' : '安装'}
            </button>
          </div>
        `).join('')}
      </div>
    `;
  });

  listEl.innerHTML = html;
}

// 初始化
initRepositories();
```

### 5. 类型定义

**文件**: `src/types.ts`

```typescript
// WebviewToExtensionMessage
| { type: 'loadRepositories' }
| { type: 'addRepository'; repository: any }
| { type: 'updateRepository'; repositoryId: string; updates: any }
| { type: 'deleteRepository'; repositoryId: string }
| { type: 'refreshRepository'; repositoryId: string }
| { type: 'loadSkillLibrary' }

// ExtensionToWebviewMessage
| { type: 'repositoriesLoaded'; repositories: any[] }
| { type: 'repositoryAdded'; repository: any }
| { type: 'repositoryUpdated'; repositoryId: string }
| { type: 'repositoryDeleted'; repositoryId: string }
| { type: 'repositoryRefreshed'; repositoryId: string }
| { type: 'skillLibraryLoaded'; skills: any[] }
```

## 验收标准

- [ ] 用户可以添加自定义仓库
- [ ] 用户可以启用/禁用仓库
- [ ] 用户可以刷新仓库缓存
- [ ] Skill 库显示所有启用仓库的 Skills
- [ ] Skills 按仓库分组显示
- [ ] 安装功能正常工作
- [ ] 配置持久化到 ~/.multicli/skills.json
- [ ] 错误处理完善（网络错误、格式错误等）

## 实施步骤

1. ✅ 创建实现方案文档
2. ✅ 扩展配置结构
3. ✅ 实现 SkillRepositoryManager
4. ✅ 扩展 LLMConfigLoader
5. ✅ 扩展 WebviewProvider
6. ⏳ 实现前端 UI
7. ✅ 添加类型定义
8. ⏳ 测试和验证

## 后端实现状态（已完成）

### 完成时间
2024年（当前会话）

### 已完成内容

#### 1. SkillRepositoryManager (src/tools/skill-repository-manager.ts)
- ✅ 内置 Skills 定义（4个 Claude 官方 Skills）
- ✅ JSON 仓库获取（HTTP + 验证）
- ✅ 缓存机制（5分钟 TTL）
- ✅ 多仓库聚合（getAllSkills）
- ✅ 错误处理（网络错误、格式错误）

#### 2. LLMConfigLoader 扩展 (src/llm/config.ts)
- ✅ `loadRepositories()` - 加载仓库配置
- ✅ `saveRepositories()` - 保存仓库配置
- ✅ `addRepository()` - 添加仓库（带重复检查）
- ✅ `updateRepository()` - 更新仓库
- ✅ `deleteRepository()` - 删除仓库
- ✅ `getDefaultRepositories()` - 默认仓库（内置）

#### 3. WebviewProvider 消息处理 (src/ui/webview-provider.ts)
- ✅ `handleLoadRepositories()` - 加载仓库列表
- ✅ `handleAddRepository()` - 添加仓库
- ✅ `handleUpdateRepository()` - 更新仓库
- ✅ `handleDeleteRepository()` - 删除仓库
- ✅ `handleRefreshRepository()` - 刷新缓存
- ✅ `handleLoadSkillLibrary()` - 加载 Skill 库（从所有仓库）

#### 4. 类型定义 (src/types.ts)
- ✅ WebviewToExtensionMessage 扩展（6个新消息类型）
- ✅ ExtensionToWebviewMessage 扩展（6个新消息类型）

#### 5. 依赖
- ✅ axios (^1.7.9) - HTTP 客户端已安装

#### 6. 编译状态
- ✅ 编译成功，0 错误

### 详细文档
参见 `SKILL_REPOSITORY_BACKEND_COMPLETE.md` 了解完整的后端实现细节。
