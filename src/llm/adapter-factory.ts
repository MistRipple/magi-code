/**
 * LLM 适配器工厂
 * 创建和管理 LLM 适配器实例
 */

import { EventEmitter } from 'events';
import { AgentType, WorkerSlot } from '../types/agent-types';
import { BaseLLMAdapter } from './adapters/base-adapter';
import { WorkerLLMAdapter, WorkerAdapterConfig } from './adapters/worker-adapter';
import { OrchestratorLLMAdapter, OrchestratorAdapterConfig } from './adapters/orchestrator-adapter';
import { LLMConfigLoader } from './config';
import { createLLMClient } from './clients/client-factory';
import { createNormalizer } from '../normalizer';
import { ToolManager } from '../tools/tool-manager';
import { SkillsManager } from '../tools/skills-manager';
import { logger, LogCategory } from '../logging';
import { IAdapterFactory, AdapterOutputScope, AdapterResponse } from '../adapters/adapter-factory-interface';
import { AgentProfileLoader } from '../orchestrator/profile/agent-profile-loader';

/**
 * LLM 适配器工厂
 */
export class LLMAdapterFactory extends EventEmitter implements IAdapterFactory {
  private adapters = new Map<AgentType, BaseLLMAdapter>();
  private toolManager: ToolManager;
  private skillsManager: SkillsManager | null = null;
  private workspaceRoot: string;
  private profileLoader: AgentProfileLoader;

  constructor(options: { cwd: string }) {
    super();
    this.workspaceRoot = options.cwd;
    this.toolManager = new ToolManager();
    this.profileLoader = new AgentProfileLoader();
    logger.info('LLM Adapter Factory initialized', { cwd: options.cwd }, LogCategory.LLM);
  }

  /**
   * 初始化（加载画像配置和 Skills）
   */
  async initialize(): Promise<void> {
    LLMConfigLoader.ensureDefaults();
    await this.profileLoader.initialize();

    // 加载并注册 Skills
    await this.loadSkills();

    logger.info('LLM Adapter Factory initialized', { configDir: LLMConfigLoader.getConfigDir() }, LogCategory.LLM);
  }

  /**
   * 加载并注册 Skills
   */
  private async loadSkills(): Promise<void> {
    try {
      // 加载 Skills 配置
      const skillsConfig = LLMConfigLoader.loadSkillsConfig();

      // 创建 SkillsManager
      this.skillsManager = new SkillsManager(skillsConfig);

      // 注册到 ToolManager
      this.toolManager.registerSkillExecutor('claude-skills', this.skillsManager);

      logger.info('Skills loaded and registered', {
        enabledTools: (await this.skillsManager.getTools()).length
      }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to load skills', { error: error.message }, LogCategory.TOOLS);
    }
  }

  /**
   * 重新加载 Skills（用于安装新 skill 后）
   */
  async reloadSkills(): Promise<void> {
    // 注销旧的 SkillsManager
    if (this.skillsManager) {
      this.toolManager.unregisterSkillExecutor('claude-skills');
    }

    // 重新加载
    await this.loadSkills();

    // 清除适配器缓存，强制重新创建（以获取新的工具列表）
    this.adapters.clear();

    logger.info('Skills reloaded', {}, LogCategory.TOOLS);
  }

  /**
   * 创建 Worker 适配器
   */
  private createWorkerAdapter(workerSlot: WorkerSlot): WorkerLLMAdapter {
    // 检查缓存
    if (this.adapters.has(workerSlot)) {
      const adapter = this.adapters.get(workerSlot);
      if (adapter instanceof WorkerLLMAdapter) {
        return adapter;
      }
    }

    // 加载配置
    const config = LLMConfigLoader.loadFullConfig();
    const workerConfig = config.workers[workerSlot];

    if (!workerConfig.enabled) {
      throw new Error(`Worker ${workerSlot} is disabled in configuration`);
    }

    // 验证配置
    if (!LLMConfigLoader.validateConfig(workerConfig, workerSlot)) {
      throw new Error(`Invalid configuration for worker ${workerSlot}`);
    }

    // 创建客户端
    const client = createLLMClient(workerConfig);

    // 创建 normalizer
    const normalizer = createNormalizer(workerSlot, 'worker', false);

    // 创建适配器
    const adapterConfig: WorkerAdapterConfig = {
      client,
      normalizer,
      toolManager: this.toolManager,
      config: workerConfig,
      workerSlot,
      profileLoader: this.profileLoader,  // ✅ 传递 profileLoader
    };

    const adapter = new WorkerLLMAdapter(adapterConfig);

    // 转发适配器事件
    this.setupAdapterEvents(adapter, workerSlot);

    this.adapters.set(workerSlot, adapter);

    logger.info(`Created worker adapter: ${workerSlot}`, {
      provider: workerConfig.provider,
      model: workerConfig.model,
    }, LogCategory.LLM);

    return adapter;
  }

  /**
   * 创建 Orchestrator 适配器
   */
  private createOrchestratorAdapter(): OrchestratorLLMAdapter {
    // 检查缓存
    if (this.adapters.has('orchestrator')) {
      const adapter = this.adapters.get('orchestrator');
      if (adapter instanceof OrchestratorLLMAdapter) {
        return adapter;
      }
    }

    // 加载配置
    const config = LLMConfigLoader.loadFullConfig();
    const orchestratorConfig = config.orchestrator;

    if (!orchestratorConfig.enabled) {
      throw new Error('Orchestrator is disabled in configuration');
    }

    // 验证配置
    if (!LLMConfigLoader.validateConfig(orchestratorConfig, 'orchestrator')) {
      throw new Error('Invalid configuration for orchestrator');
    }

    // 创建客户端
    const client = createLLMClient(orchestratorConfig);

    // 创建 normalizer
    const normalizer = createNormalizer('claude', 'orchestrator', false);

    // 创建适配器
    const adapterConfig: OrchestratorAdapterConfig = {
      client,
      normalizer,
      toolManager: this.toolManager,
      config: orchestratorConfig,
    };

    const adapter = new OrchestratorLLMAdapter(adapterConfig);

    // 转发适配器事件
    this.setupAdapterEvents(adapter, 'orchestrator');

    this.adapters.set('orchestrator', adapter);

    logger.info('Created orchestrator adapter', {
      provider: orchestratorConfig.provider,
      model: orchestratorConfig.model,
    }, LogCategory.LLM);

    return adapter;
  }

  /**
   * 设置适配器事件转发
   */
  private setupAdapterEvents(adapter: BaseLLMAdapter, agent: AgentType): void {
    // 转发标准消息事件
    adapter.on('standardMessage', (message) => {
      this.emit('standardMessage', message);
    });

    adapter.on('standardComplete', (message) => {
      this.emit('standardComplete', message);
    });

    adapter.on('stream', (update) => {
      this.emit('stream', update);
    });

    adapter.on('normalizerError', (error) => {
      this.emit('error', error);
    });

    adapter.on('error', (error) => {
      this.emit('error', error);
    });
  }

  /**
   * 获取或创建适配器
   */
  private getOrCreateAdapter(agent: AgentType): BaseLLMAdapter {
    if (agent === 'orchestrator') {
      return this.createOrchestratorAdapter();
    } else {
      return this.createWorkerAdapter(agent as WorkerSlot);
    }
  }

  /**
   * 发送消息（实现 IAdapterFactory 接口）
   */
  async sendMessage(
    agent: WorkerSlot,
    message: string,
    images?: string[],
    options?: AdapterOutputScope
  ): Promise<AdapterResponse> {
    const adapter = this.getOrCreateAdapter(agent);

    if (!adapter.isConnected) {
      await adapter.connect();
    }

    try {
      const content = await adapter.sendMessage(message, images);

      return {
        content,
        done: true,
        tokenUsage: {
          inputTokens: 0, // TODO: Get from adapter
          outputTokens: 0, // TODO: Get from adapter
        },
      };
    } catch (error: any) {
      return {
        content: '',
        done: false,
        error: error.message,
      };
    }
  }

  /**
   * 中断（实现 IAdapterFactory 接口）
   */
  async interrupt(agent: AgentType): Promise<void> {
    const adapter = this.adapters.get(agent);
    if (adapter) {
      await adapter.interrupt();
    }
  }

  /**
   * 关闭所有适配器（实现 IAdapterFactory 接口）
   */
  async shutdown(): Promise<void> {
    for (const [agent, adapter] of this.adapters) {
      try {
        await adapter.disconnect();
        logger.info(`Disconnected adapter: ${agent}`, undefined, LogCategory.LLM);
      } catch (error: any) {
        logger.error(`Failed to disconnect adapter: ${agent}`, {
          error: error.message,
        }, LogCategory.LLM);
      }
    }
    this.adapters.clear();
    logger.info('All adapters shut down', undefined, LogCategory.LLM);
  }

  /**
   * 检查是否已连接（实现 IAdapterFactory 接口）
   */
  isConnected(agent: AgentType): boolean {
    const adapter = this.adapters.get(agent);
    return adapter ? adapter.isConnected : false;
  }

  /**
   * 检查是否忙碌（实现 IAdapterFactory 接口）
   */
  isBusy(agent: AgentType): boolean {
    const adapter = this.adapters.get(agent);
    return adapter ? adapter.isBusy : false;
  }

  /**
   * 获取适配器（如果存在）
   */
  getAdapter(agent: AgentType): BaseLLMAdapter | undefined {
    return this.adapters.get(agent);
  }

  /**
   * 获取所有适配器
   */
  getAllAdapters(): Map<AgentType, BaseLLMAdapter> {
    return new Map(this.adapters);
  }

  /**
   * 获取工具管理器实例
   */
  getToolManager(): ToolManager {
    return this.toolManager;
  }

  /**
   * 清除特定适配器
   */
  async clearAdapter(agent: AgentType): Promise<void> {
    const adapter = this.adapters.get(agent);
    if (adapter) {
      await adapter.disconnect();
      this.adapters.delete(agent);
      logger.info(`Cleared adapter: ${agent}`, undefined, LogCategory.LLM);
    }
  }

  /**
   * 重新加载 Worker 配置并清除缓存
   */
  async reloadWorkerConfig(worker: WorkerSlot): Promise<void> {
    await this.clearAdapter(worker);
    logger.info(`Worker config reloaded: ${worker}`, undefined, LogCategory.LLM);
  }

  /**
   * 重新加载编排者配置并清除缓存
   */
  async reloadOrchestratorConfig(): Promise<void> {
    await this.clearAdapter('orchestrator');
    logger.info('Orchestrator config reloaded', undefined, LogCategory.LLM);
  }
}
