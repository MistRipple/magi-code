/**
 * Skill 仓库管理器
 *
 * 负责从多个来源获取 Skill 信息：
 * - 内置 Skills（Claude 官方）
 * - JSON 仓库（自定义 URL）
 * - GitHub 仓库（GitHub 项目）
 */

import axios from 'axios';
import type { CustomToolExecutorConfig, ToolDefinition } from './skills-manager';
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
  toolDefinition?: ToolDefinition;
  executor?: CustomToolExecutorConfig;
  skillType?: 'tool' | 'instruction';
  instruction?: string;
  allowedTools?: string[];
  disableModelInvocation?: boolean;
  userInvocable?: boolean;
  argumentHint?: string;
}

/**
 * Skill 仓库管理器
 */
export class SkillRepositoryManager {
  private cache: Map<string, SkillInfo[]> = new Map();
  private cacheExpiry: Map<string, number> = new Map();
  private readonly CACHE_TTL = 5 * 60 * 1000; // 5 分钟

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

        const { toolDefinition, executor } = this.normalizeToolDefinition(skill);
        const skillType = this.detectSkillType(skill, toolDefinition);

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
          repositoryName: data.name,
          toolDefinition,
          executor,
          skillType,
          instruction: skill.instruction,
          allowedTools: skill.allowedTools || skill['allowed-tools'],
          disableModelInvocation: skill.disableModelInvocation ?? skill['disable-model-invocation'],
          userInvocable: skill.userInvocable ?? skill['user-invocable'],
          argumentHint: skill.argumentHint ?? skill['argument-hint']
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
            repositoryName: `${repo} (Claude Code Plugins)`,
            skillType: 'instruction',
            instruction: readme
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
            repositoryName: `${repo} (Claude Code Plugins)`,
            skillType: 'instruction',
            instruction: `Claude Code plugin: ${pluginName}`
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

  private async fetchRawFile(owner: string, repo: string, branch: string, filePath: string): Promise<string | null> {
    const rawUrl = `https://raw.githubusercontent.com/${owner}/${repo}/${branch}/${filePath}`;
    try {
      const response = await axios.get(rawUrl, {
        timeout: 10000,
        headers: {
          'User-Agent': 'MultiCLI-SkillManager/1.0'
        }
      });
      if (typeof response.data === 'string') {
        return response.data;
      }
      return JSON.stringify(response.data);
    } catch (error: any) {
      if (error.response?.status === 404 || error.response?.status === 403 || error.response?.status === 429) {
        return null;
      }
      throw error;
    }
  }

  private async listRepoDir(owner: string, repo: string, dirPath: string): Promise<any[] | null> {
    const url = `https://api.github.com/repos/${owner}/${repo}/contents/${dirPath}`;
    try {
      const response = await axios.get(url, {
        timeout: 10000,
        headers: {
          'Accept': 'application/vnd.github.v3+json',
          'User-Agent': 'MultiCLI-SkillManager/1.0'
        }
      });
      return Array.isArray(response.data) ? response.data : null;
    } catch (error: any) {
      if (error.response?.status === 404 || error.response?.status === 403 || error.response?.status === 429) {
        return null;
      }
      throw error;
    }
  }

  private parseSkillMarkdown(content: string): {
    meta: Record<string, any>;
    body: string;
  } {
    const trimmed = content.trim();
    if (!trimmed.startsWith('---')) {
      return { meta: {}, body: content.trim() };
    }

    const lines = trimmed.split('\n');
    const metaLines: string[] = [];
    let i = 1;
    for (; i < lines.length; i++) {
      const line = lines[i].trim();
      if (line === '---') {
        i++;
        break;
      }
      metaLines.push(lines[i]);
    }

    const meta: Record<string, any> = {};
    let currentKey: string | null = null;

    for (const line of metaLines) {
      const trimmedLine = line.trim();
      if (!trimmedLine) continue;

      if (trimmedLine.startsWith('- ') && currentKey) {
        if (!Array.isArray(meta[currentKey])) {
          meta[currentKey] = [];
        }
        meta[currentKey].push(trimmedLine.slice(2).trim());
        continue;
      }

      const match = trimmedLine.match(/^([A-Za-z0-9_-]+)\s*:\s*(.*)$/);
      if (match) {
        const key = match[1];
        let value: any = match[2].trim();
        if (value === '') {
          meta[key] = [];
        } else if (value === 'true' || value === 'false') {
          meta[key] = value === 'true';
        } else {
          value = value.replace(/^['"]|['"]$/g, '');
          meta[key] = value;
        }
        currentKey = key;
        continue;
      }
    }

    const body = lines.slice(i).join('\n').trim();
    return { meta, body };
  }

  private normalizeSkillName(name: string): string {
    return name.trim();
  }

  private slugify(name: string): string {
    return name
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '_')
      .replace(/^_+|_+$/g, '');
  }

  private extractAllowedTools(meta: Record<string, any>): string[] | undefined {
    const raw = meta['allowed-tools'] ?? meta['allowed_tools'];
    if (!raw) return undefined;
    if (Array.isArray(raw)) {
      return raw.map((item) => String(item).trim()).filter(Boolean);
    }
    if (typeof raw === 'string') {
      return raw.split(',').map((item) => item.trim()).filter(Boolean);
    }
    return undefined;
  }

  private async fetchSkillFromDirectory(
    owner: string,
    repo: string,
    branch: string,
    repositoryId: string,
    repositoryName: string,
    skillDirPath: string
  ): Promise<SkillInfo | null> {
    const skillMd = await this.fetchRawFile(owner, repo, branch, `${skillDirPath}/SKILL.md`);
    const fallbackMd = skillMd ?? await this.fetchRawFile(owner, repo, branch, `${skillDirPath}/skill.md`);
    if (!fallbackMd) {
      return null;
    }

    const { meta, body } = this.parseSkillMarkdown(fallbackMd);
    const dirName = skillDirPath.split('/').filter(Boolean).pop() || 'skill';
    const name = this.normalizeSkillName(meta.name || dirName);
    const description = meta.description || body.split('\n').find(line => line.trim()) || '';

    return {
      id: this.slugify(name),
      name,
      fullName: name,
      description: description || '',
      author: meta.author || owner,
      version: meta.version,
      category: meta.category,
      repositoryId,
      repositoryName,
      skillType: 'instruction',
      instruction: body,
      allowedTools: this.extractAllowedTools(meta),
      disableModelInvocation: meta['disable-model-invocation'] ?? meta.disable_model_invocation ?? false,
      userInvocable: meta['user-invocable'] ?? meta.user_invocable ?? true,
      argumentHint: meta['argument-hint'] ?? meta.argument_hint
    };
  }

  private normalizeSkillPaths(raw: any): string[] {
    if (!raw) {
      return ['skills'];
    }
    if (Array.isArray(raw)) {
      return raw.map((item) => String(item)).filter(Boolean);
    }
    if (typeof raw === 'string') {
      return [raw];
    }
    return ['skills'];
  }

  private normalizeSkillPath(pathInput: string): string {
    return pathInput
      .trim()
      .replace(/\\/g, '/')
      .replace(/^\.\/+/, '')
      .replace(/^\/+/, '')
      .replace(/\/+/g, '/');
  }

  private async fetchClaudeCodePluginSkills(
    owner: string,
    repo: string,
    repositoryId: string,
    branch: string,
    basePath: string,
    pluginJson: any
  ): Promise<SkillInfo[]> {
    const skillPaths = this.normalizeSkillPaths(pluginJson.skills);
    const skills: SkillInfo[] = [];

    for (const skillPath of skillPaths) {
      const normalized = this.normalizeSkillPath([basePath, skillPath].filter(Boolean).join('/'));

      const directSkill = await this.fetchSkillFromDirectory(
        owner,
        repo,
        branch,
        repositoryId,
        pluginJson.name || repo,
        normalized
      );
      if (directSkill) {
        skills.push(directSkill);
        continue;
      }

      const entries = await this.listRepoDir(owner, repo, normalized);
      if (!entries) continue;

      const dirs = entries.filter((item: any) => item.type === 'dir');
      for (const dir of dirs) {
        const skillDirPath = dir.path;
        const skill = await this.fetchSkillFromDirectory(owner, repo, branch, repositoryId, pluginJson.name || repo, skillDirPath);
        if (skill) {
          skills.push(skill);
        }
      }
    }

    return skills;
  }

  private async tryFetchClaudeCodePluginRepository(
    owner: string,
    repo: string,
    repositoryId: string,
    branch: string
  ): Promise<{ name: string; skills: SkillInfo[] } | null> {
    const pluginJsonPaths = ['.claude-plugin/plugin.json', 'plugin.json'];
    const candidateBranches = [branch, 'main', 'master'].filter((value, index, arr) => value && arr.indexOf(value) === index);

    for (const pluginJsonPath of pluginJsonPaths) {
      for (const candidateBranch of candidateBranches) {
        const raw = await this.fetchRawFile(owner, repo, candidateBranch, pluginJsonPath);
        if (!raw) continue;
        try {
          const pluginJson = JSON.parse(raw);
          const skills = await this.fetchClaudeCodePluginSkills(owner, repo, repositoryId, candidateBranch, '', pluginJson);
          if (skills.length > 0) {
            return { name: pluginJson.name || repo, skills };
          }
        } catch (error: any) {
          logger.warn('Failed to parse plugin.json', { error: error.message }, LogCategory.TOOLS);
        }
      }
    }

    // 支持 monorepo plugins/ 目录
    const pluginsDir = await this.listRepoDir(owner, repo, 'plugins');
    if (!pluginsDir) {
      return null;
    }

    const skills: SkillInfo[] = [];
    for (const pluginDir of pluginsDir.filter((item: any) => item.type === 'dir')) {
      const basePath = `plugins/${pluginDir.name}`;
      for (const pluginJsonPath of ['.claude-plugin/plugin.json', 'plugin.json']) {
        const raw = await this.fetchRawFile(owner, repo, branch, `${basePath}/${pluginJsonPath}`);
        if (!raw) continue;
        try {
          const pluginJson = JSON.parse(raw);
          const pluginSkills = await this.fetchClaudeCodePluginSkills(owner, repo, repositoryId, branch, basePath, pluginJson);
          if (pluginSkills.length > 0) {
            pluginSkills.forEach((skill) => {
              skill.repositoryName = pluginJson.name || pluginDir.name || repo;
            });
            skills.push(...pluginSkills);
          }
          break;
        } catch (error: any) {
          logger.warn('Failed to parse plugin.json', { error: error.message }, LogCategory.TOOLS);
        }
      }
    }

    if (skills.length === 0) {
      return null;
    }

    return { name: repo, skills };
  }

  /**
   * 尝试直接从 skills/ 目录获取 SKILL.md 文件
   * 支持没有 plugin.json 但有 skills/{skill-name}/SKILL.md 的仓库
   * 为避免 GitHub API 速率限制，直接尝试常见的路径模式
   */
  private async tryFetchSkillsDirectory(
    owner: string,
    repo: string,
    repositoryId: string
  ): Promise<{ name: string; skills: SkillInfo[] } | null> {
    const candidateBranches = ['main', 'master'];

    // 常见的 skill 路径模式：skills/{repo-name}/SKILL.md
    const commonPaths = [
      `skills/${repo}`,           // skills/planning-with-files/
      `skills/${repo.toLowerCase()}`,
      repo,                        // 根目录下直接有 SKILL.md
    ];

    for (const branch of candidateBranches) {
      const skills: SkillInfo[] = [];

      for (const path of commonPaths) {
        const skill = await this.fetchSkillFromDirectory(
          owner,
          repo,
          branch,
          repositoryId,
          repo,
          path
        );
        if (skill) {
          skills.push(skill);
          break; // 找到一个就够了
        }
      }

      if (skills.length > 0) {
        return { name: repo, skills };
      }
    }

    // 如果常见路径都没找到，尝试使用 API 列出目录（可能会被速率限制）
    for (const branch of candidateBranches) {
      const skillsDir = await this.listRepoDir(owner, repo, 'skills');
      if (!skillsDir) continue;

      const skills: SkillInfo[] = [];
      for (const item of skillsDir.filter((i: any) => i.type === 'dir')) {
        const skill = await this.fetchSkillFromDirectory(
          owner,
          repo,
          branch,
          repositoryId,
          repo,
          `skills/${item.name}`
        );
        if (skill) {
          skills.push(skill);
        }
      }

      if (skills.length > 0) {
        return { name: repo, skills };
      }
    }

    return null;
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

      // 获取仓库信息（可能被 GitHub API 速率限制阻断）
      let repoName = repo;
      let repoDescription = '';
      let defaultBranch = 'main';
      try {
        const repoInfoUrl = `https://api.github.com/repos/${owner}/${repo}`;
        const repoInfoResponse = await axios.get(repoInfoUrl, {
          timeout: 10000,
          headers: {
            'Accept': 'application/vnd.github.v3+json',
            'User-Agent': 'MultiCLI-SkillManager/1.0'
          }
        });

        const repoInfo = repoInfoResponse.data;
        repoName = repoInfo.name || repo;
        repoDescription = repoInfo.description || '';
        defaultBranch = repoInfo.default_branch || 'main';
      } catch (error: any) {
        logger.warn('Failed to fetch GitHub repo info, falling back to branch guesses', {
          url,
          error: error.message
        }, LogCategory.TOOLS);
      }

      // 尝试获取 skills.json 文件
      const skillsJsonUrl = `https://raw.githubusercontent.com/${owner}/${repo}/${defaultBranch}/skills.json`;
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
      } catch (defaultBranchError: any) {
        const fallbackBranches = ['main', 'master'].filter(branch => branch !== defaultBranch);
        let fetched = false;
        for (const branch of fallbackBranches) {
          const fallbackUrl = `https://raw.githubusercontent.com/${owner}/${repo}/${branch}/skills.json`;
          try {
            const skillsResponse = await axios.get(fallbackUrl, {
              timeout: 10000,
              headers: {
                'Accept': 'application/json',
                'User-Agent': 'MultiCLI-SkillManager/1.0'
              }
            });
            skillsData = skillsResponse.data;
            fetched = true;
            break;
          } catch {
            // continue
          }
        }

        if (!fetched) {
          // 两个分支都没有 skills.json，尝试检测 Claude Code 插件格式
          logger.info('No skills.json found, trying Claude Code plugins format', { owner, repo }, LogCategory.TOOLS);

          try {
            const pluginSkills = await this.tryFetchClaudeCodePluginRepository(owner, repo, repositoryId, defaultBranch);
            if (pluginSkills) {
              return pluginSkills;
            }
          } catch (pluginError: any) {
            logger.warn('Failed to fetch Claude Code plugins', { error: pluginError.message }, LogCategory.TOOLS);
          }

          try {
            const pluginsData = await this.fetchClaudeCodePlugins(owner, repo, repositoryId);
            if (pluginsData) {
              return pluginsData;
            }
          } catch (pluginError: any) {
            logger.warn('Failed to fetch Claude Code plugins', { error: pluginError.message }, LogCategory.TOOLS);
          }

          // 尝试直接从 skills/ 目录获取 SKILL.md
          try {
            const skillsDirData = await this.tryFetchSkillsDirectory(owner, repo, repositoryId);
            if (skillsDirData) {
              return skillsDirData;
            }
          } catch (skillsDirError: any) {
            logger.warn('Failed to fetch skills directory', { error: skillsDirError.message }, LogCategory.TOOLS);
          }

          // 如果也不是 Claude Code 格式，抛出错误
          throw new Error(
            `GitHub 仓库 ${owner}/${repo} 中没有找到 skills.json 或 Claude Code 插件技能。\n` +
            `请确保仓库根目录包含 skills.json 文件（${defaultBranch} 分支优先），或符合 Claude Code 插件格式。\n` +
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

        const { toolDefinition, executor } = this.normalizeToolDefinition(skill);
        const skillType = this.detectSkillType(skill, toolDefinition);

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
          repositoryName: skillsData.name || repoName,
          toolDefinition,
          executor,
          skillType,
          instruction: skill.instruction,
          allowedTools: skill.allowedTools || skill['allowed-tools'],
          disableModelInvocation: skill.disableModelInvocation ?? skill['disable-model-invocation'],
          userInvocable: skill.userInvocable ?? skill['user-invocable'],
          argumentHint: skill.argumentHint ?? skill['argument-hint']
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

  private normalizeToolDefinition(skill: any): {
    toolDefinition?: ToolDefinition;
    executor?: CustomToolExecutorConfig;
  } {
    const candidate = skill.toolDefinition || skill.tool;
    const inputSchema =
      candidate?.input_schema ||
      candidate?.inputSchema ||
      skill.input_schema ||
      skill.inputSchema;

    const executor: CustomToolExecutorConfig | undefined =
      candidate?.executor || skill.executor;

    if (!inputSchema) {
      return { executor };
    }

    return {
      toolDefinition: {
        name: candidate?.name || skill.fullName || skill.name,
        description: candidate?.description || skill.description || '',
        input_schema: inputSchema
      },
      executor
    };
  }

  private detectSkillType(skill: any, toolDefinition?: ToolDefinition): 'tool' | 'instruction' | undefined {
    if (skill.skillType) {
      if (skill.skillType === 'instruction' || skill.skillType === 'tool') {
        return skill.skillType;
      }
    }
    if (skill.instruction || skill['instruction']) {
      return 'instruction';
    }
    if (toolDefinition || skill.executor || skill.toolDefinition || skill.tool) {
      return 'tool';
    }
    return undefined;
  }
}
