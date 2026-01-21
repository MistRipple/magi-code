/**
 * LLM 配置加载器
 *
 * 配置存储位置：~/.multicli/
 * - llm.json              - 所有 LLM 配置（augment, orchestrator, workers, compressor）
 * - claude.json           - Claude Worker 画像
 * - codex.json            - Codex Worker 画像
 * - gemini.json           - Gemini Worker 画像
 * - categories.json       - 任务分类配置
 * - mcp.json              - MCP 服务器配置
 * - skills.json           - 自定义技能配置
 * - config.json           - 全局配置
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import { LLMConfig, WorkerSlot } from '../types/agent-types';
import { FullLLMConfig, WorkerLLMConfig } from './types';
import { logger, LogCategory } from '../logging';

/**
 * LLM 配置加载器（从 ~/.multicli/ 加载）
 */
export class LLMConfigLoader {
  private static readonly CONFIG_DIR = path.join(os.homedir(), '.multicli');
  private static readonly LLM_CONFIG_FILE = path.join(this.CONFIG_DIR, 'llm.json');

  /**
   * 加载完整配置
   */
  static loadFullConfig(): FullLLMConfig {
    const config = this.loadLLMConfigFile();

    return {
      orchestrator: this.extractOrchestratorConfig(config),
      workers: this.extractWorkersConfig(config),
      compressor: this.loadCompressorConfig(),
    };
  }

  /**
   * 从文件加载 LLM 配置
   */
  private static loadLLMConfigFile(): any {
    if (!fs.existsSync(this.LLM_CONFIG_FILE)) {
      // 创建默认配置
      const defaultConfig = this.getDefaultLLMConfig();
      this.saveLLMConfigFile(defaultConfig);
      return defaultConfig;
    }

    try {
      const content = fs.readFileSync(this.LLM_CONFIG_FILE, 'utf-8');
      return JSON.parse(content);
    } catch (error) {
      logger.warn(`Failed to load LLM config from ${this.LLM_CONFIG_FILE}, using defaults`, { error }, LogCategory.LLM);
      return this.getDefaultLLMConfig();
    }
  }

  /**
   * 保存 LLM 配置到文件（私有方法）
   */
  private static saveLLMConfigFile(config: any): void {
    this.ensureConfigDir();

    try {
      fs.writeFileSync(this.LLM_CONFIG_FILE, JSON.stringify(config, null, 2), 'utf-8');
      logger.info('LLM config saved', { path: this.LLM_CONFIG_FILE }, LogCategory.LLM);
    } catch (error) {
      logger.error('Failed to save LLM config', { error, path: this.LLM_CONFIG_FILE }, LogCategory.LLM);
      throw error;
    }
  }

  /**
   * 保存完整配置到文件（公共方法）
   */
  static saveFullConfig(config: any): void {
    this.saveLLMConfigFile(config);
  }

  /**
   * 获取默认 LLM 配置
   */
  private static getDefaultLLMConfig(): any {
    return {
      augment: {
        email: '',
        apiKey: '',
      },
      orchestrator: {
        baseUrl: 'https://api.anthropic.com',
        apiKey: process.env.ANTHROPIC_API_KEY || '',
        model: 'claude-3-5-sonnet-20241022',
        provider: 'anthropic',
        enabled: true,
      },
      workers: {
        claude: {
          baseUrl: 'https://api.anthropic.com',
          apiKey: process.env.ANTHROPIC_API_KEY || '',
          model: 'claude-3-5-sonnet-20241022',
          provider: 'anthropic',
          enabled: true,
        },
        codex: {
          baseUrl: 'https://api.openai.com/v1',
          apiKey: process.env.OPENAI_API_KEY || '',
          model: 'gpt-4-turbo-preview',
          provider: 'openai',
          enabled: true,
        },
        gemini: {
          baseUrl: 'https://api.openai.com/v1',
          apiKey: process.env.OPENAI_API_KEY || '',
          model: 'gpt-4-turbo-preview',
          provider: 'openai',
          enabled: true,
        },
      },
      compressor: {
        enabled: false,
        baseUrl: 'https://api.anthropic.com',
        apiKey: process.env.ANTHROPIC_API_KEY || '',
        model: 'claude-3-haiku-20240307',
        provider: 'anthropic',
      },
    };
  }

  /**
   * 提取 Orchestrator 配置
   */
  private static extractOrchestratorConfig(config: any): LLMConfig {
    const orchestratorConfig = config.orchestrator || {};
    const defaults = this.getDefaultLLMConfig().orchestrator;

    return {
      baseUrl: orchestratorConfig.baseUrl || defaults.baseUrl,
      apiKey: orchestratorConfig.apiKey || defaults.apiKey,
      model: orchestratorConfig.model || defaults.model,
      provider: orchestratorConfig.provider || defaults.provider,
      enabled: orchestratorConfig.enabled !== false,
    };
  }

  /**
   * 提取 Workers 配置
   */
  private static extractWorkersConfig(config: any): WorkerLLMConfig {
    const workersConfig = config.workers || {};
    const defaults = this.getDefaultLLMConfig().workers;

    return {
      claude: this.extractWorkerConfig(workersConfig.claude, defaults.claude),
      codex: this.extractWorkerConfig(workersConfig.codex, defaults.codex),
      gemini: this.extractWorkerConfig(workersConfig.gemini, defaults.gemini),
    };
  }

  /**
   * 提取单个 Worker 配置
   */
  private static extractWorkerConfig(workerConfig: any, defaults: any): LLMConfig {
    if (!workerConfig) {
      return defaults;
    }

    return {
      baseUrl: workerConfig.baseUrl || defaults.baseUrl,
      apiKey: workerConfig.apiKey || defaults.apiKey,
      model: workerConfig.model || defaults.model,
      provider: workerConfig.provider || defaults.provider,
      enabled: workerConfig.enabled !== false,
    };
  }

  /**
   * 加载 Orchestrator 配置
   */
  static loadOrchestratorConfig(): LLMConfig {
    const config = this.loadLLMConfigFile();
    return this.extractOrchestratorConfig(config);
  }

  /**
   * 加载 Workers 配置
   */
  static loadWorkersConfig(): WorkerLLMConfig {
    const config = this.loadLLMConfigFile();
    return this.extractWorkersConfig(config);
  }

  /**
   * 确保配置目录存在
   */
  private static ensureConfigDir(): void {
    if (!fs.existsSync(this.CONFIG_DIR)) {
      fs.mkdirSync(this.CONFIG_DIR, { recursive: true });
      logger.info('Created config directory', { dir: this.CONFIG_DIR }, LogCategory.LLM);
    }
  }

  /**
   * 初始化默认配置
   */
  static ensureDefaults(): void {
    this.ensureConfigDir();

    if (!fs.existsSync(this.LLM_CONFIG_FILE)) {
      const defaultConfig = this.getDefaultLLMConfig();
      this.saveLLMConfigFile(defaultConfig);
      logger.info('Created default LLM config', { path: this.LLM_CONFIG_FILE }, LogCategory.LLM);
    }
  }

  /**
   * 获取配置目录路径
   */
  static getConfigDir(): string {
    return this.CONFIG_DIR;
  }

  /**
   * 验证配置
   */
  static validateConfig(config: LLMConfig, name: string): boolean {
    const errors: string[] = [];

    if (!config.apiKey) {
      errors.push(`${name}: API key is missing`);
    }

    if (!config.model) {
      errors.push(`${name}: Model is missing`);
    }

    if (!config.baseUrl) {
      errors.push(`${name}: Base URL is missing`);
    }

    if (!['openai', 'anthropic'].includes(config.provider)) {
      errors.push(`${name}: Invalid provider '${config.provider}'`);
    }

    if (errors.length > 0) {
      logger.error(`Configuration validation failed for ${name}`, {
        errors,
      }, LogCategory.LLM);
      return false;
    }

    return true;
  }

  /**
   * 验证完整配置
   */
  static validateFullConfig(config: FullLLMConfig): {
    valid: boolean;
    errors: string[];
  } {
    const errors: string[] = [];

    if (!this.validateConfig(config.orchestrator, 'orchestrator')) {
      errors.push('Orchestrator configuration is invalid');
    }

    for (const worker of ['claude', 'codex', 'gemini'] as WorkerSlot[]) {
      const workerConfig = config.workers[worker];
      if (workerConfig.enabled && !this.validateConfig(workerConfig, worker)) {
        errors.push(`Worker '${worker}' configuration is invalid`);
      }
    }

    return {
      valid: errors.length === 0,
      errors,
    };
  }

  /**
   * 更新单个 Worker 配置
   */
  static updateWorkerConfig(worker: WorkerSlot, config: any): void {
    const fullConfig = this.loadLLMConfigFile();

    if (!fullConfig.workers) {
      fullConfig.workers = {};
    }

    fullConfig.workers[worker] = {
      baseUrl: config.baseUrl,
      apiKey: config.apiKey,
      model: config.model,
      provider: config.provider,
      enabled: config.enabled !== false,
    };

    this.saveFullConfig(fullConfig);
    logger.info('Worker config updated', { worker }, LogCategory.LLM);
  }

  /**
   * 更新编排者配置
   */
  static updateOrchestratorConfig(config: any): void {
    const fullConfig = this.loadLLMConfigFile();

    fullConfig.orchestrator = {
      baseUrl: config.baseUrl,
      apiKey: config.apiKey,
      model: config.model,
      provider: config.provider,
      enabled: config.enabled !== false,
    };

    this.saveFullConfig(fullConfig);
    logger.info('Orchestrator config updated', undefined, LogCategory.LLM);
  }

  /**
   * 更新压缩器配置
   */
  static updateCompressorConfig(config: any): void {
    const fullConfig = this.loadLLMConfigFile();

    fullConfig.compressor = {
      enabled: config.enabled === true,
      baseUrl: config.baseUrl,
      apiKey: config.apiKey,
      model: config.model,
      provider: config.provider,
    };

    this.saveFullConfig(fullConfig);
    logger.info('Compressor config updated', undefined, LogCategory.LLM);
  }

  /**
   * 加载压缩器配置
   */
  static loadCompressorConfig(): any {
    const config = this.loadLLMConfigFile();
    const compressorConfig = config.compressor || {};
    const defaults = this.getDefaultLLMConfig().compressor;

    return {
      enabled: compressorConfig.enabled === true,
      baseUrl: compressorConfig.baseUrl || defaults.baseUrl,
      apiKey: compressorConfig.apiKey || defaults.apiKey,
      model: compressorConfig.model || defaults.model,
      provider: compressorConfig.provider || defaults.provider,
    };
  }

  /**
   * 加载 Augment 配置
   */
  static loadAugmentConfig(): any {
    const config = this.loadLLMConfigFile();
    return config.augment || { email: '', apiKey: '' };
  }

  /**
   * 更新 Augment 配置
   */
  static updateAugmentConfig(config: any): void {
    const fullConfig = this.loadLLMConfigFile();

    fullConfig.augment = {
      email: config.email || '',
      apiKey: config.apiKey || '',
    };

    this.saveFullConfig(fullConfig);
    logger.info('Augment config updated', undefined, LogCategory.LLM);
  }

  // ============================================================================
  // MCP 配置管理
  // ============================================================================

  private static readonly MCP_CONFIG_FILE = path.join(this.CONFIG_DIR, 'mcp.json');

  /**
   * 加载 MCP 配置
   */
  static loadMCPConfig(): any[] {
    if (!fs.existsSync(this.MCP_CONFIG_FILE)) {
      return [];
    }

    try {
      const content = fs.readFileSync(this.MCP_CONFIG_FILE, 'utf-8');
      const config = JSON.parse(content);
      return config.servers || [];
    } catch (error) {
      logger.warn(`Failed to load MCP config from ${this.MCP_CONFIG_FILE}`, { error }, LogCategory.LLM);
      return [];
    }
  }

  /**
   * 保存 MCP 配置
   */
  static saveMCPConfig(servers: any[]): void {
    this.ensureConfigDir();

    try {
      const config = { servers };
      fs.writeFileSync(this.MCP_CONFIG_FILE, JSON.stringify(config, null, 2), 'utf-8');
      logger.info('MCP config saved', { count: servers.length }, LogCategory.LLM);
    } catch (error) {
      logger.error('Failed to save MCP config', { error }, LogCategory.LLM);
      throw error;
    }
  }

  /**
   * 添加 MCP 服务器
   */
  static addMCPServer(server: any): void {
    const servers = this.loadMCPConfig();
    servers.push(server);
    this.saveMCPConfig(servers);
    logger.info('MCP server added', { id: server.id, name: server.name }, LogCategory.LLM);
  }

  /**
   * 更新 MCP 服务器
   */
  static updateMCPServer(serverId: string, updates: any): void {
    const servers = this.loadMCPConfig();
    const index = servers.findIndex((s: any) => s.id === serverId);

    if (index === -1) {
      throw new Error(`MCP server not found: ${serverId}`);
    }

    servers[index] = { ...servers[index], ...updates };
    this.saveMCPConfig(servers);
    logger.info('MCP server updated', { id: serverId }, LogCategory.LLM);
  }

  /**
   * 删除 MCP 服务器
   */
  static deleteMCPServer(serverId: string): void {
    const servers = this.loadMCPConfig();
    const filtered = servers.filter((s: any) => s.id !== serverId);

    if (filtered.length === servers.length) {
      throw new Error(`MCP server not found: ${serverId}`);
    }

    this.saveMCPConfig(filtered);
    logger.info('MCP server deleted', { id: serverId }, LogCategory.LLM);
  }

  // ============================================================================
  // Skills 配置管理
  // ============================================================================

  private static readonly SKILLS_CONFIG_FILE = path.join(this.CONFIG_DIR, 'skills.json');

  /**
   * 加载 Skills 配置
   */
  static loadSkillsConfig(): any {
    if (!fs.existsSync(this.SKILLS_CONFIG_FILE)) {
      return null;
    }
    try {
      const content = fs.readFileSync(this.SKILLS_CONFIG_FILE, 'utf-8');
      const config = JSON.parse(content);
      return config;
    } catch (error) {
      logger.warn(`Failed to load Skills config from ${this.SKILLS_CONFIG_FILE}`, { error }, LogCategory.LLM);
      return null;
    }
  }

  /**
   * 保存 Skills 配置
   */
  static saveSkillsConfig(config: any): void {
    this.ensureConfigDir();
    try {
      fs.writeFileSync(this.SKILLS_CONFIG_FILE, JSON.stringify(config, null, 2), 'utf-8');
      logger.info('Skills config saved', {}, LogCategory.LLM);
    } catch (error) {
      logger.error('Failed to save Skills config', { error }, LogCategory.LLM);
      throw error;
    }
  }

  // ============================================================================
  // Skills 仓库管理
  // ============================================================================

  /**
   * 加载仓库配置
   */
  static loadRepositories(): any[] {
    const config = this.loadSkillsConfig();
    return config?.repositories || this.getDefaultRepositories();
  }

  /**
   * 保存仓库配置
   */
  static saveRepositories(repositories: any[]): void {
    const config = this.loadSkillsConfig() || {
      builtInTools: {},
      customTools: [],
      repositories: []
    };
    config.repositories = repositories;
    this.saveSkillsConfig(config);
    logger.info('Repositories saved', { count: repositories.length }, LogCategory.LLM);
  }

  /**
   * 添加仓库（简化版：只需要 URL）
   */
  static async addRepository(url: string): Promise<{ id: string; name: string }> {
    const repositories = this.loadRepositories();

    // 检查是否已存在相同 URL
    const existing = repositories.find((r: any) => r.url === url);
    if (existing) {
      throw new Error(`仓库已存在`);
    }

    // 生成新 ID
    const id = 'repo-' + Date.now();

    // 创建仓库配置（暂时不包含 name，等验证后再更新）
    const repository = {
      id,
      url
    };

    repositories.push(repository);
    this.saveRepositories(repositories);
    logger.info('Repository added', { id, url }, LogCategory.LLM);

    return { id, name: '' };
  }

  /**
   * 更新仓库名称（验证后调用）
   */
  static updateRepositoryName(id: string, name: string): void {
    const repositories = this.loadRepositories();
    const repo = repositories.find((r: any) => r.id === id);
    if (repo) {
      repo.name = name;
      this.saveRepositories(repositories);
      logger.info('Repository name updated', { id, name }, LogCategory.LLM);
    }
  }

  /**
   * 更新仓库
   */
  static updateRepository(id: string, updates: any): void {
    const repositories = this.loadRepositories();
    const index = repositories.findIndex((r: any) => r.id === id);

    if (index === -1) {
      throw new Error(`Repository not found: ${id}`);
    }

    repositories[index] = { ...repositories[index], ...updates };
    this.saveRepositories(repositories);
    logger.info('Repository updated', { id }, LogCategory.LLM);
  }

  /**
   * 删除仓库（内置仓库不可删除）
   */
  static deleteRepository(id: string): void {
    if (id === 'builtin') {
      throw new Error('内置仓库不可删除');
    }

    const repositories = this.loadRepositories();
    const filtered = repositories.filter((r: any) => r.id !== id);

    if (filtered.length === repositories.length) {
      throw new Error(`Repository not found: ${id}`);
    }

    this.saveRepositories(filtered);
    logger.info('Repository deleted', { id }, LogCategory.LLM);
  }

  /**
   * 获取默认仓库
   */
  private static getDefaultRepositories(): any[] {
    return [
      {
        id: 'builtin',
        url: 'builtin'
      }
    ];
  }
}
