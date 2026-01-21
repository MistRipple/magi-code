/**
 * Skill 仓库管理器
 *
 * 负责从多个来源获取 Skill 信息：
 * - 内置 Skills（Claude 官方）
 * - JSON 仓库（自定义 URL）
 */

import axios from 'axios';
import { logger, LogCategory } from '../logging';

/**
 * 仓库配置
 */
export interface RepositoryConfig {
  id: string;
  url: string;
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
      // JSON 仓库
      const result = await this.fetchJSONRepository(repository.url, repository.id);
      skills = result.skills;
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
  async validateRepository(url: string): Promise<{ name: string; skillCount: number }> {
    try {
      const tempId = 'temp-' + Date.now();
      const result = await this.fetchJSONRepository(url, tempId);
      return {
        name: result.name,
        skillCount: result.skills.length
      };
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
