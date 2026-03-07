/**
 * Skill 仓库管理器
 *
 * 负责从多个来源获取 Skill 信息：
 * - 内置 Skills（Claude 官方）
 * - JSON 仓库（自定义 URL）
 * - GitHub 仓库（GitHub 项目）
 */

import type { CustomToolExecutorConfig, ToolDefinition } from './skills-manager';
import { logger, LogCategory } from '../logging';
import { fetchWithRetry, isRetryableNetworkError, toErrorMessage } from './network-utils';

class HttpRequestError extends Error {
  constructor(
    message: string,
    public readonly status: number,
  ) {
    super(message);
    this.name = 'HttpRequestError';
  }
}

/**
 * 仓库配置
 */
export interface RepositoryConfig {
  id: string;
  url: string;
  type?: 'json' | 'github';  // 仓库类型：json（直接 JSON 文件）或 github（GitHub 仓库）
}

export interface RepositoryFetchFailure {
  repositoryId: string;
  url: string;
  error: string;
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
  private readonly REQUEST_TIMEOUT = 8000; // 8 秒超时
  private readonly REQUEST_RETRIES = 3;
  private readonly MAX_CONCURRENT = 3; // 最大并发仓库数

  private async fetchJSON(url: string): Promise<any> {
    const response = await this.fetchRaw(url, 'application/json');
    const contentType = response.headers.get('content-type') || '';
    const text = await response.text();
    if (!response.ok) {
      throw new HttpRequestError(`HTTP ${response.status}: ${url}`, response.status);
    }
    if (!text) {
      return {};
    }
    if (contentType.includes('application/json')) {
      return JSON.parse(text);
    }
    try {
      return JSON.parse(text);
    } catch {
      throw new Error(`Invalid JSON response from ${url}`);
    }
  }

  private async fetchText(url: string): Promise<string> {
    const response = await this.fetchRaw(url);
    const text = await response.text();
    if (!response.ok) {
      throw new HttpRequestError(`HTTP ${response.status}: ${url}`, response.status);
    }
    return text;
  }

  private async fetchRaw(url: string, accept: string = '*/*'): Promise<Response> {
    try {
      return await fetchWithRetry(url, {
        method: 'GET',
        headers: {
          'Accept': accept,
          'User-Agent': 'Magi-SkillManager/1.0',
        },
      }, {
        timeoutMs: this.REQUEST_TIMEOUT,
        attempts: this.REQUEST_RETRIES,
        retryOnStatuses: [429, 500, 502, 503, 504],
      });
    } catch (error: any) {
      const message = toErrorMessage(error);
      const lower = message.toLowerCase();
      if (error?.name === 'AbortError' || lower.includes('timeout') || lower.includes('timed out')) {
        throw new Error(`Request timeout: ${url}`);
      }
      if (isRetryableNetworkError(message)) {
        throw new Error(`Network request failed: ${url}; ${message}`);
      }
      throw error;
    }
  }

  /**
   * 从 JSON 仓库获取 Skills（同时获取仓库名称）
   */
  private async fetchJSONRepository(url: string, repositoryId: string): Promise<{ name: string; skills: SkillInfo[] }> {
    try {
      logger.info('Fetching JSON repository', { url, repositoryId }, LogCategory.TOOLS);

      const data = await this.fetchJSON(url);

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
      const pluginsData = await this.fetchJSON(pluginsUrl);
      const plugins = pluginsData.filter((item: any) => item.type === 'dir');
      if (plugins.length === 0) {
        return null;
      }

      logger.info('Found Claude Code plugins directory', { pluginCount: plugins.length }, LogCategory.TOOLS);

      // 并行获取所有插件的 README（限制并发数）
      const fetchPluginInfo = async (plugin: any): Promise<SkillInfo> => {
        const pluginName = plugin.name;
        try {
          const readmeUrl = `https://raw.githubusercontent.com/${owner}/${repo}/main/plugins/${pluginName}/README.md`;
          const readme = await this.fetchText(readmeUrl);
          const lines = readme.split('\n').filter((line: string) => line.trim());
          const title = lines[0]?.replace(/^#\s*/, '') || pluginName;
          const description = lines[1] || `Claude Code plugin: ${pluginName}`;

          return {
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
          };
        } catch {
          return {
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
          };
        }
      };

      // 分批并行处理，每批最多 5 个
      const skills: SkillInfo[] = [];
      const batchSize = 5;
      for (let i = 0; i < plugins.length; i += batchSize) {
        const batch = plugins.slice(i, i + batchSize);
        const batchResults = await Promise.all(batch.map(fetchPluginInfo));
        skills.push(...batchResults);
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
      return await this.fetchText(rawUrl);
    } catch (error: any) {
      if (error instanceof HttpRequestError && (error.status === 404 || error.status === 403 || error.status === 429)) {
        return null;
      }
      throw error;
    }
  }

  private async listRepoDir(owner: string, repo: string, dirPath: string): Promise<any[] | null> {
    const url = `https://api.github.com/repos/${owner}/${repo}/contents/${dirPath}`;
    try {
      const data = await this.fetchJSON(url);
      return Array.isArray(data) ? data : null;
    } catch (error: any) {
      if (error instanceof HttpRequestError) {
        if (error.status === 404) {
          return null;
        }
        // GitHub API 限流时降级到网页目录解析，避免仓库被静默丢失
        if (error.status === 403 || error.status === 429) {
          return await this.listRepoDirViaHtml(owner, repo, dirPath);
        }
      }
      throw error;
    }
  }

  private async listRepoDirViaHtml(owner: string, repo: string, dirPath: string): Promise<any[] | null> {
    const normalizedDirPath = dirPath.replace(/^\/+|\/+$/g, '');
    if (!normalizedDirPath) {
      return null;
    }
    const encodedDirPath = normalizedDirPath
      .split('/')
      .filter(Boolean)
      .map((segment) => encodeURIComponent(segment))
      .join('/');
    const treeUrl = `https://github.com/${owner}/${repo}/tree/HEAD/${encodedDirPath}`;

    try {
      const html = await this.fetchText(treeUrl);
      const entries = new Map<string, { name: string; type: 'dir' | 'file' }>();
      const escapeRegex = (value: string): string => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      const entryPattern = new RegExp(
        `^/${escapeRegex(owner)}/${escapeRegex(repo)}/(tree|blob)/[^/]+/${escapeRegex(encodedDirPath)}/([^/?#]+)$`
      );

      const hrefRegex = /href="([^"]+)"/g;
      let match: RegExpExecArray | null;
      while ((match = hrefRegex.exec(html)) !== null) {
        const href = match[1];
        const entryMatch = href.match(entryPattern);
        if (!entryMatch) {
          continue;
        }
        const type = entryMatch[1] === 'tree' ? 'dir' : 'file';
        const remainder = entryMatch[2];
        if (!remainder) {
          continue;
        }

        const cleanName = decodeURIComponent(remainder).trim();
        if (!cleanName) {
          continue;
        }
        if (!entries.has(cleanName)) {
          entries.set(cleanName, { name: cleanName, type });
        }
      }

      return entries.size > 0 ? Array.from(entries.values()) : null;
    } catch (error: any) {
      if (error instanceof HttpRequestError && error.status === 404) {
        return null;
      }
      logger.warn('Failed to list repo directory via HTML fallback', {
        owner,
        repo,
        dirPath,
        error: error?.message || String(error),
      }, LogCategory.TOOLS);
      return null;
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
    repositoryId: string,
    repositoryName: string,
    skillDirPath: string
  ): Promise<SkillInfo | null> {
    const skillMd = await this.fetchRawFile(owner, repo, 'HEAD', `${skillDirPath}/SKILL.md`);
    const selectedMd = skillMd ?? await this.fetchRawFile(owner, repo, 'HEAD', `${skillDirPath}/skill.md`);
    if (!selectedMd) {
      return null;
    }

    const { meta, body } = this.parseSkillMarkdown(selectedMd);
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
        const skill = await this.fetchSkillFromDirectory(owner, repo, repositoryId, pluginJson.name || repo, skillDirPath);
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
    repositoryId: string
  ): Promise<{ name: string; skills: SkillInfo[] } | null> {
    const pluginJsonPaths = ['.claude-plugin/plugin.json', 'plugin.json'];

    // 并行尝试两个路径（使用 HEAD 默认分支）
    const results = await Promise.all(
      pluginJsonPaths.map(async (p) => {
        const raw = await this.fetchRawFile(owner, repo, 'HEAD', p);
        if (!raw) return null;
        try {
          const pluginJson = JSON.parse(raw);
          const skills = await this.fetchClaudeCodePluginSkills(owner, repo, repositoryId, '', pluginJson);
          if (skills.length > 0) {
            return { name: pluginJson.name || repo, skills };
          }
        } catch {
          // ignore parse errors
        }
        return null;
      })
    );

    // 返回第一个成功的结果
    const success = results.find(r => r !== null);
    if (success) {
      return success;
    }

    // 支持 monorepo plugins/ 目录
    const pluginsDir = await this.listRepoDir(owner, repo, 'plugins');
    if (!pluginsDir) {
      return null;
    }

    // 并行处理所有插件目录
    const pluginDirs = pluginsDir.filter((item: any) => item.type === 'dir');
    const pluginResults = await Promise.all(
      pluginDirs.map(async (pluginDir: any) => {
        const basePath = `plugins/${pluginDir.name}`;
        for (const pluginJsonPath of pluginJsonPaths) {
          const raw = await this.fetchRawFile(owner, repo, 'HEAD', `${basePath}/${pluginJsonPath}`);
          if (!raw) continue;
          try {
            const pluginJson = JSON.parse(raw);
            const pluginSkills = await this.fetchClaudeCodePluginSkills(owner, repo, repositoryId, basePath, pluginJson);
            if (pluginSkills.length > 0) {
              pluginSkills.forEach((skill) => {
                skill.repositoryName = pluginJson.name || pluginDir.name || repo;
              });
              return pluginSkills;
            }
          } catch {
            // ignore
          }
        }
        return [];
      })
    );

    const skills = pluginResults.flat();
    if (skills.length === 0) {
      return null;
    }

    return { name: repo, skills };
  }

  /**
   * 尝试直接从 skills/ 目录获取 SKILL.md 文件
   * 支持没有 plugin.json 但有 skills/{skill-name}/SKILL.md 的仓库
   * 直接使用 HEAD（默认分支）
   */
  private async tryFetchSkillsDirectory(
    owner: string,
    repo: string,
    repositoryId: string
  ): Promise<{ name: string; skills: SkillInfo[] } | null> {
    // 常见的 skill 路径模式
    const commonPaths = [
      `skills/${repo}`,
      `skills/${repo.toLowerCase()}`,
      repo,
    ];

    // 先尝试常见路径
    for (const path of commonPaths) {
      const skill = await this.fetchSkillFromDirectory(
        owner,
        repo,
        repositoryId,
        repo,
        path
      );
      if (skill) {
        return { name: repo, skills: [skill] };
      }
    }

    // 如果常见路径都没找到，尝试使用 API 列出 skills/ 目录
    const skillsDir = await this.listRepoDir(owner, repo, 'skills');
    if (!skillsDir) {
      return null;
    }

    // 并行处理所有 skill 目录
    const skillDirs = skillsDir.filter((i: any) => i.type === 'dir');
    const skillResults = await Promise.all(
      skillDirs.map((item: any) =>
        this.fetchSkillFromDirectory(owner, repo, repositoryId, repo, `skills/${item.name}`)
      )
    );

    const topLevelSkills = skillResults.filter((s): s is SkillInfo => s !== null);
    if (topLevelSkills.length > 0) {
      return { name: repo, skills: topLevelSkills };
    }

    // 兼容 anthropics/skills 这类嵌套目录结构：skills/.curated/<skill>/SKILL.md
    const nestedSkillPaths: string[] = [];
    for (const topLevelDir of skillDirs) {
      const nestedDirPath = `skills/${topLevelDir.name}`;
      const nestedEntries = await this.listRepoDir(owner, repo, nestedDirPath);
      if (!nestedEntries || nestedEntries.length === 0) {
        continue;
      }
      nestedEntries
        .filter((entry: any) => entry.type === 'dir')
        .forEach((entry: any) => nestedSkillPaths.push(`${nestedDirPath}/${entry.name}`));
    }

    if (nestedSkillPaths.length === 0) {
      return null;
    }

    const nestedSkillResults = await Promise.all(
      nestedSkillPaths.map((skillPath) =>
        this.fetchSkillFromDirectory(owner, repo, repositoryId, repo, skillPath)
      )
    );

    const nestedSkills = nestedSkillResults.filter((s): s is SkillInfo => s !== null);
    if (nestedSkills.length > 0) {
      return { name: repo, skills: nestedSkills };
    }

    return null;
  }

  /**
   * 从 GitHub 仓库获取 Skills
   * 直接使用仓库默认分支（HEAD），无需探测
   */
  private async fetchGitHubRepository(url: string, repositoryId: string): Promise<{ name: string; skills: SkillInfo[] }> {
    try {
      logger.info('Fetching GitHub repository', { url, repositoryId }, LogCategory.TOOLS);

      // 解析 GitHub URL
      const repoMatch = url.match(/github\.com\/([^\/]+)\/([^\/]+)/);
      if (!repoMatch) {
        throw new Error('Invalid GitHub URL format');
      }

      const owner = repoMatch[1];
      const repo = repoMatch[2].replace(/\.git$/, '').replace(/\/.*$/, '');

      // 直接使用 HEAD（默认分支）
      const skillsJsonUrl = `https://raw.githubusercontent.com/${owner}/${repo}/HEAD/skills.json`;
      let skillsData: any;
      let repoName = repo;

      try {
        skillsData = await this.fetchJSON(skillsJsonUrl);
        logger.debug('Found skills.json', { owner, repo }, LogCategory.TOOLS);
      } catch {
        // skills.json 不存在，尝试其他格式
        logger.info('No skills.json found, trying alternative formats', { owner, repo }, LogCategory.TOOLS);

        // 尝试 Claude Code 插件格式
        try {
          const pluginSkills = await this.tryFetchClaudeCodePluginRepository(owner, repo, repositoryId);
          if (pluginSkills) {
            return pluginSkills;
          }
        } catch {
          // continue
        }

        try {
          const pluginsData = await this.fetchClaudeCodePlugins(owner, repo, repositoryId);
          if (pluginsData) {
            return pluginsData;
          }
        } catch {
          // continue
        }

        // 尝试 skills/ 目录
        try {
          const skillsDirData = await this.tryFetchSkillsDirectory(owner, repo, repositoryId);
          if (skillsDirData) {
            return skillsDirData;
          }
        } catch {
          // continue
        }

        throw new Error(
          `GitHub 仓库 ${owner}/${repo} 中没有找到 skills.json 或 Claude Code 插件技能。\n` +
          `请确保仓库根目录包含 skills.json 文件，或符合 Claude Code 插件格式。`
        );
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
   * 获取所有仓库的 Skills（受控并发，避免过多请求）
   */
  async getAllSkills(repositories: RepositoryConfig[]): Promise<SkillInfo[]> {
    const { skills } = await this.getAllSkillsWithReport(repositories);
    return skills;
  }

  async getAllSkillsWithReport(
    repositories: RepositoryConfig[]
  ): Promise<{ skills: SkillInfo[]; failedRepositories: RepositoryFetchFailure[] }> {
    logger.info('Fetching skills from repositories', {
      totalRepos: repositories.length
    }, LogCategory.TOOLS);

    const skills: SkillInfo[] = [];
    const failedRepositories: RepositoryFetchFailure[] = [];

    // 分批处理，每批最多 MAX_CONCURRENT 个
    for (let i = 0; i < repositories.length; i += this.MAX_CONCURRENT) {
      const batch = repositories.slice(i, i + this.MAX_CONCURRENT);
      const results = await Promise.allSettled(
        batch.map(repo => this.fetchRepository(repo))
      );

      results.forEach((result, index) => {
        const repoIndex = i + index;
        if (result.status === 'fulfilled') {
          skills.push(...result.value);
          logger.debug('Repository fetched successfully', {
            repositoryId: repositories[repoIndex].id,
            skillCount: result.value.length
          }, LogCategory.TOOLS);
        } else {
          const failedRepo = repositories[repoIndex];
          const errorMessage = result.reason?.message || String(result.reason);
          failedRepositories.push({
            repositoryId: failedRepo.id,
            url: failedRepo.url,
            error: errorMessage,
          });
          logger.warn('Failed to fetch repository', {
            repositoryId: repositories[repoIndex].id,
            error: errorMessage
          }, LogCategory.TOOLS);
        }
      });
    }

    logger.info('All skills fetched', {
      totalSkills: skills.length,
      failedRepos: failedRepositories.length,
    }, LogCategory.TOOLS);

    return { skills, failedRepositories };
  }

  /**
   * 验证并获取仓库信息（用于添加仓库时）
   */
  /**
   * 验证仓库是否有效（快速验证，不获取所有 skills 详情）
   */
  async validateRepository(url: string): Promise<{ name: string; skillCount: number; type: 'json' | 'github' }> {
    try {
      const isGitHub = url.includes('github.com');

      if (isGitHub) {
        // 解析 GitHub URL
        const repoMatch = url.match(/github\.com\/([^\/]+)\/([^\/]+)/);
        if (!repoMatch) {
          throw new Error('Invalid GitHub URL format');
        }
        const owner = repoMatch[1];
        const repo = repoMatch[2].replace(/\.git$/, '').replace(/\/.*$/, '');

        // 快速检查：尝试获取 skills.json
        const skillsJsonUrl = `https://raw.githubusercontent.com/${owner}/${repo}/HEAD/skills.json`;
        try {
          const data = await this.fetchJSON(skillsJsonUrl);
          return {
            name: data.name || repo,
            skillCount: Array.isArray(data.skills) ? data.skills.length : 0,
            type: 'github'
          };
        } catch {
          // 没有 skills.json，检查是否有 plugin.json
          const pluginPaths = ['.claude-plugin/plugin.json', 'plugin.json'];
          for (const p of pluginPaths) {
            const raw = await this.fetchRawFile(owner, repo, 'HEAD', p);
            if (raw) {
              try {
                const pluginJson = JSON.parse(raw);
                const skillPaths = this.normalizeSkillPaths(pluginJson.skills);
                return {
                  name: pluginJson.name || repo,
                  skillCount: skillPaths.length,
                  type: 'github'
                };
              } catch {
                // continue
              }
            }
          }

          // 检查 plugins/ 目录
          const pluginsDir = await this.listRepoDir(owner, repo, 'plugins');
          if (pluginsDir && pluginsDir.length > 0) {
            const dirs = pluginsDir.filter((i: any) => i.type === 'dir');
            return {
              name: repo,
              skillCount: dirs.length,
              type: 'github'
            };
          }

          // 检查 skills/ 目录 (Agent Skills 标准格式)
          const skillsDir = await this.listRepoDir(owner, repo, 'skills');
          if (skillsDir && skillsDir.length > 0) {
            const dirs = skillsDir.filter((i: any) => i.type === 'dir');
            if (dirs.length > 0) {
              return {
                name: repo,
                skillCount: dirs.length,
                type: 'github'
              };
            }
          }

          throw new Error('仓库中未找到有效的 skills 配置');
        }
      } else {
        // JSON 仓库
        const data = await this.fetchJSON(url);
        return {
          name: data.name || 'Unknown',
          skillCount: Array.isArray(data.skills) ? data.skills.length : 0,
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
