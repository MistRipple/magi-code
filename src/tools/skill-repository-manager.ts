/**
 * Skill 仓库管理器
 *
 * 负责从多个来源获取 Skill 信息：
 * - 内置 Skills（Claude 官方）
 * - JSON 仓库（自定义 URL）
 * - GitHub 仓库（GitHub 项目）
 */

import axios from 'axios';
import { logger, LogCategory } from '../logging';

/**
 * 仓库配置
 */
export interface RepositoryConfig {
  id: string;
  url: string;
  type?: 'json' | 'github';  // 仓库类型：json（直接 JSON 文件）或 github（GitHub 仓库）
}

/**
 * Skill 信息
 */
export interface SkillInfo {
  id: string;
  name: string;
  fullName: string;
  description: string;
  author?: string;
  version?: string;
  category?: string;
  type?: 'server-side' | 'client-side';
  icon?: string;
  repositoryId: string;
  repositoryName?: string;
}

/**
 * Skill 仓库管理器
 */
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
        repositoryId: 'builtin',
        repositoryName: 'Claude 官方技能'
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
        repositoryId: 'builtin',
        repositoryName: 'Claude 官方技能'
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
        repositoryId: 'builtin',
        repositoryName: 'Claude 官方技能'
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
        repositoryId: 'builtin',
        repositoryName: 'Claude 官方技能'
      }
    ];
  }

  /**
   * 从 JSON 仓库获取 Skills（同时获取仓库名称）
   */
  private async fetchJSONRepository(url: string, repositoryId: string): Promise<{ name: string; skills: SkillInfo[] }> {
    try {
      logger.info('Fetching JSON repository', { url, repositoryId }, LogCategory.TOOLS);

      const response = await axios.get(url, {
        timeout: 10000,
        headers: {
          'Accept': 'application/json',
          'User-Agent': 'MultiCLI-SkillManager/1.0'
        }
      });

      const data = response.data;

      // 验证数据格式
      if (!data || typeof data !== 'object') {
        throw new Error('Invalid repository format: not an object');
      }

      if (!data.name || typeof data.name !== 'string') {
        throw new Error('Invalid repository format: missing name field');
      }

      if (!Array.isArray(data.skills)) {
        throw new Error('Invalid repository format: missing skills array');
      }

      // 转换并验证每个 Skill
      const skills: SkillInfo[] = [];
      for (const skill of data.skills) {
        if (!skill.id || !skill.name || !skill.fullName) {
          logger.warn('Skipping invalid skill', { skill }, LogCategory.TOOLS);
          continue;
        }

        skills.push({
          id: skill.id,
          name: skill.name,
          fullName: skill.fullName,
          description: skill.description || '',
          author: skill.author,
          version: skill.version,
          category: skill.category,
          type: skill.type,
          icon: skill.icon,
          repositoryId,
          repositoryName: data.name
        });
      }

      logger.info('JSON repository fetched', {
        url,
        repositoryId,
        name: data.name,
        skillCount: skills.length
      }, LogCategory.TOOLS);

      return { name: data.name, skills };
    } catch (error: any) {
      logger.error('Failed to fetch JSON repository', {
        url,
        repositoryId,
        error: error.message
      }, LogCategory.TOOLS);
      throw error;
    }
  }

  /**
   * 从 Claude Code 插件仓库获取 Skills
   * 检测 plugins 目录并转换为技能格式
   */
  private async fetchClaudeCodePlugins(owner: string, repo: string, repositoryId: string): Promise<{ name: string; skills: SkillInfo[] } | null> {
    try {
      logger.info('Trying to fetch Claude Code plugins', { owner, repo }, LogCategory.TOOLS);

      // 检查是否有 plugins 目录
      const pluginsUrl = `https://api.github.com/repos/${owner}/${repo}/contents/plugins`;
      const pluginsResponse = await axios.get(pluginsUrl, {
        timeout: 10000,
        headers: {
          'Accept': 'application/vnd.github.v3+json',
          'User-Agent': 'MultiCLI-SkillManager/1.0'
        }
      });

      const plugins = pluginsResponse.data.filter((item: any) => item.type === 'dir');
      if (plugins.length === 0) {
        return null;
      }

      logger.info('Found Claude Code plugins directory', { pluginCount: plugins.length }, LogCategory.TOOLS);

      // 转换每个插件
      const skills: SkillInfo[] = [];
      for (const plugin of plugins) {
        const pluginName = plugin.name;

        try {
          // 尝试读取插件的 README.md
          const readmeUrl = `https://raw.githubusercontent.com/${owner}/${repo}/main/plugins/${pluginName}/README.md`;
          const readmeResponse = await axios.get(readmeUrl, {
            timeout: 5000,
            headers: {
              'User-Agent': 'MultiCLI-SkillManager/1.0'
            }
          });

          const readme = readmeResponse.data;
          const lines = readme.split('\n').filter((line: string) => line.trim());
          const title = lines[0]?.replace(/^#\s*/, '') || pluginName;
          const description = lines[1] || `Claude Code plugin: ${pluginName}`;

          skills.push({
            id: pluginName.replace(/-/g, '_'),
            name: title,
            fullName: `${pluginName.replace(/-/g, '_')}_v1`,
            description: description,
            author: owner,
            version: '1.0.0',
            category: 'claude-code',
            type: 'client-side',
            icon: '🔌',
            repositoryId,
            repositoryName: `${repo} (Claude Code Plugins)`
          });

          logger.debug('Converted Claude Code plugin', { pluginName, title }, LogCategory.TOOLS);
        } catch (readmeError) {
          // 如果读取 README 失败，使用默认信息
          skills.push({
            id: pluginName.replace(/-/g, '_'),
            name: pluginName,
            fullName: `${pluginName.replace(/-/g, '_')}_v1`,
            description: `Claude Code plugin: ${pluginName}`,
            author: owner,
            version: '1.0.0',
            category: 'claude-code',
            type: 'client-side',
            icon: '🔌',
            repositoryId,
            repositoryName: `${repo} (Claude Code Plugins)`
          });

          logger.debug('Converted Claude Code plugin (no README)', { pluginName }, LogCategory.TOOLS);
        }
      }

      logger.info('Claude Code plugins converted', {
        owner,
        repo,
        pluginCount: skills.length
      }, LogCategory.TOOLS);

      return {
        name: `${repo} (Claude Code Plugins)`,
        skills
      };
    } catch (error: any) {
      logger.debug('Not a Claude Code plugin repository', { error: error.message }, LogCategory.TOOLS);
      return null;
    }
  }

  /**
   * 从 GitHub 仓库获取 Skills
   * 支持格式：https://github.com/owner/repo
   */
  private async fetchGitHubRepository(url: string, repositoryId: string): Promise<{ name: string; skills: SkillInfo[] }> {
    try {
      logger.info('Fetching GitHub repository', { url, repositoryId }, LogCategory.TOOLS);

      // 解析 GitHub URL
      const match = url.match(/github\.com\/([^\/]+)\/([^\/]+)/);
      if (!match) {
        throw new Error('Invalid GitHub URL format');
      }

      const owner = match[1];
      const repo = match[2].replace(/\.git$/, '');

      // 获取仓库信息
      const repoInfoUrl = `https://api.github.com/repos/${owner}/${repo}`;
      const repoInfoResponse = await axios.get(repoInfoUrl, {
        timeout: 10000,
        headers: {
          'Accept': 'application/vnd.github.v3+json',
          'User-Agent': 'MultiCLI-SkillManager/1.0'
        }
      });

      const repoInfo = repoInfoResponse.data;
      const repoName = repoInfo.name || repo;
      const repoDescription = repoInfo.description || '';

      // 尝试获取 skills.json 文件
      const skillsJsonUrl = `https://raw.githubusercontent.com/${owner}/${repo}/main/skills.json`;
      let skillsData: any;

      try {
        const skillsResponse = await axios.get(skillsJsonUrl, {
          timeout: 10000,
          headers: {
            'Accept': 'application/json',
            'User-Agent': 'MultiCLI-SkillManager/1.0'
          }
        });
        skillsData = skillsResponse.data;
      } catch (mainError: any) {
        // 如果 main 分支没有，尝试 master 分支
        const skillsJsonUrlMaster = `https://raw.githubusercontent.com/${owner}/${repo}/master/skills.json`;
        try {
          const skillsResponse = await axios.get(skillsJsonUrlMaster, {
            timeout: 10000,
            headers: {
              'Accept': 'application/json',
              'User-Agent': 'MultiCLI-SkillManager/1.0'
            }
          });
          skillsData = skillsResponse.data;
        } catch (masterError: any) {
          // 两个分支都没有 skills.json，尝试检测 Claude Code 插件格式
          logger.info('No skills.json found, trying Claude Code plugins format', { owner, repo }, LogCategory.TOOLS);

          try {
            const pluginsData = await this.fetchClaudeCodePlugins(owner, repo, repositoryId);
            if (pluginsData) {
              return pluginsData;
            }
          } catch (pluginError: any) {
            logger.warn('Failed to fetch Claude Code plugins', { error: pluginError.message }, LogCategory.TOOLS);
          }

          // 如果也不是 Claude Code 格式，抛出错误
          throw new Error(
            `GitHub 仓库 ${owner}/${repo} 中没有找到 skills.json 文件。\n` +
            `请确保仓库根目录包含 skills.json 文件（main 或 master 分支）。\n` +
            `参考格式请查看 example-skills-repository.json 文件。`
          );
        }
      }

      // 验证 skills.json 格式
      if (!skillsData || typeof skillsData !== 'object') {
        throw new Error('Invalid skills.json format: not an object');
      }

      if (!Array.isArray(skillsData.skills)) {
        throw new Error('Invalid skills.json format: missing skills array');
      }

      // 转换并验证每个 Skill
      const skills: SkillInfo[] = [];
      for (const skill of skillsData.skills) {
        if (!skill.id || !skill.name || !skill.fullName) {
          logger.warn('Skipping invalid skill', { skill }, LogCategory.TOOLS);
          continue;
        }

        skills.push({
          id: skill.id,
          name: skill.name,
          fullName: skill.fullName,
          description: skill.description || '',
          author: skill.author || owner,
          version: skill.version,
          category: skill.category,
          type: skill.type,
          icon: skill.icon,
          repositoryId,
          repositoryName: skillsData.name || repoName
        });
      }

      logger.info('GitHub repository fetched', {
        url,
        repositoryId,
        owner,
        repo,
        name: skillsData.name || repoName,
        skillCount: skills.length
      }, LogCategory.TOOLS);

      return { name: skillsData.name || repoName, skills };
    } catch (error: any) {
      logger.error('Failed to fetch GitHub repository', {
        url,
        repositoryId,
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
      logger.debug('Using cached repository', { repositoryId: repository.id }, LogCategory.TOOLS);
      return cached;
    }

    let skills: SkillInfo[];

    if (repository.id === 'builtin') {
      // 内置仓库
      skills = this.getBuiltInSkills();
    } else {
      // 根据类型或 URL 判断仓库类型
      const isGitHub = repository.type === 'github' || repository.url.includes('github.com');

      if (isGitHub) {
        // GitHub 仓库
        const result = await this.fetchGitHubRepository(repository.url, repository.id);
        skills = result.skills;
      } else {
        // JSON 仓库
        const result = await this.fetchJSONRepository(repository.url, repository.id);
        skills = result.skills;
      }
    }

    // 更新缓存
    this.cache.set(repository.id, skills);
    this.cacheExpiry.set(repository.id, Date.now() + this.CACHE_TTL);

    return skills;
  }

  /**
   * 获取所有仓库的 Skills
   */
  async getAllSkills(repositories: RepositoryConfig[]): Promise<SkillInfo[]> {
    logger.info('Fetching skills from repositories', {
      totalRepos: repositories.length
    }, LogCategory.TOOLS);

    const results = await Promise.allSettled(
      repositories.map(repo => this.fetchRepository(repo))
    );

    const allSkills: SkillInfo[] = [];
    results.forEach((result, index) => {
      if (result.status === 'fulfilled') {
        allSkills.push(...result.value);
        logger.debug('Repository fetched successfully', {
          repositoryId: repositories[index].id,
          skillCount: result.value.length
        }, LogCategory.TOOLS);
      } else {
        logger.warn('Failed to fetch repository', {
          repositoryId: repositories[index].id,
          error: result.reason?.message || result.reason
        }, LogCategory.TOOLS);
      }
    });

    logger.info('All skills fetched', { totalSkills: allSkills.length }, LogCategory.TOOLS);

    return allSkills;
  }

  /**
   * 验证并获取仓库信息（用于添加仓库时）
   */
  async validateRepository(url: string): Promise<{ name: string; skillCount: number; type: 'json' | 'github' }> {
    try {
      const tempId = 'temp-' + Date.now();

      // 判断是否为 GitHub 仓库
      const isGitHub = url.includes('github.com');

      if (isGitHub) {
        const result = await this.fetchGitHubRepository(url, tempId);
        return {
          name: result.name,
          skillCount: result.skills.length,
          type: 'github'
        };
      } else {
        const result = await this.fetchJSONRepository(url, tempId);
        return {
          name: result.name,
          skillCount: result.skills.length,
          type: 'json'
        };
      }
    } catch (error: any) {
      throw new Error(`无法验证仓库: ${error.message}`);
    }
  }

  /**
   * 清除缓存
   */
  clearCache(repositoryId?: string): void {
    if (repositoryId) {
      this.cache.delete(repositoryId);
      this.cacheExpiry.delete(repositoryId);
      logger.info('Repository cache cleared', { repositoryId }, LogCategory.TOOLS);
    } else {
      this.cache.clear();
      this.cacheExpiry.clear();
      logger.info('All repository caches cleared', {}, LogCategory.TOOLS);
    }
  }
}
