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
import { SkillsManager, InstructionSkillDefinition } from '../tools/skills-manager';
import { MCPToolExecutor } from '../tools/mcp-executor';
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
  private mcpExecutor: MCPToolExecutor | null = null;
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

    // 加载并注册 MCP
    await this.loadMCP();

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
      this.skillsManager = new SkillsManager(skillsConfig, {
        workspaceRoot: this.workspaceRoot,
      });

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
   * 加载并注册 MCP 执行器
   */
  private async loadMCP(): Promise<void> {
    try {
      // 创建 MCP 执行器
      this.mcpExecutor = new MCPToolExecutor();

      // 初始化（连接所有配置的 MCP 服务器）
      await this.mcpExecutor.initialize();

      // 注册到 ToolManager
      this.toolManager.registerMCPExecutor('mcp-servers', this.mcpExecutor);

      const tools = await this.mcpExecutor.getTools();
      logger.info('MCP loaded and registered', {
        toolCount: tools.length
      }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to load MCP', { error: error.message }, LogCategory.TOOLS);
    }
  }

  /**
   * 重新加载 MCP（用于添加/删除 MCP 服务器后）
   */
  async reloadMCP(): Promise<void> {
    // 注销旧的 MCP 执行器
    if (this.mcpExecutor) {
      await this.mcpExecutor.shutdown();
      this.toolManager.unregisterMCPExecutor('mcp-servers');
    }

    // 重新加载
    await this.loadMCP();

    // 清除适配器缓存，强制重新创建（以获取新的工具列表）
    this.adapters.clear();

    logger.info('MCP reloaded', {}, LogCategory.TOOLS);
  }

  /**
   * 获取 MCP 执行器（用于 UI 交互）
   */
  getMCPExecutor(): MCPToolExecutor | null {
    return this.mcpExecutor;
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

    const skillPrompt = this.buildSkillPromptAppendix();
    if (skillPrompt) {
      adapter.setSystemPrompt(`${adapter.getSystemPrompt()}\n\n${skillPrompt}`);
    }

    // 转发适配器事件
    this.setupAdapterEvents(adapter, workerSlot);

    this.adapters.set(workerSlot, adapter);

    logger.info(`Created worker adapter: ${workerSlot}`, {
      provider: workerConfig.provider,
      model: workerConfig.model,
    }, LogCategory.LLM);

    return adapter;
  }

  private buildSkillPromptAppendix(): string {
    const skillsConfig = LLMConfigLoader.loadSkillsConfig();
    const instructionSkills: InstructionSkillDefinition[] = Array.isArray(skillsConfig?.instructionSkills)
      ? skillsConfig.instructionSkills
      : [];

    if (instructionSkills.length === 0) {
      return '';
    }

    const autoSkills = instructionSkills.filter(skill => !skill.disableModelInvocation);
    const manualSkills = instructionSkills.filter(skill => skill.disableModelInvocation);
    const maxChars = 6000;
    let usedChars = 0;
    const blocks: string[] = [];

    const header = [
      '## 可用 Skills（兼容 Claude Code）',
      '- 你可以在合适的任务中主动使用 Skill。',
      '- 当用户输入 /skill-name 时，必须应用对应 Skill 指令。',
    ].join('\n');

    blocks.push(header);

    if (instructionSkills.length > 0) {
      blocks.push('\n### Skill 列表');
      instructionSkills.forEach((skill) => {
        const flag = skill.disableModelInvocation ? '（仅手动 /skill）' : '';
        blocks.push(`- ${skill.name}${flag}: ${skill.description || ''}`);
      });
    }

    blocks.push('\n### Skill 指令（可自动调用）');
    for (const skill of autoSkills) {
      const contentBlock = this.formatSkillInstruction(skill);
      if (usedChars + contentBlock.length > maxChars) {
        blocks.push(`- ${skill.name}: 指令内容过长，需在 /${skill.name} 调用时加载`);
        continue;
      }
      blocks.push(contentBlock);
      usedChars += contentBlock.length;
    }

    if (manualSkills.length > 0) {
      blocks.push('\n### 仅在 /skill 调用时启用的 Skills');
      manualSkills.forEach((skill) => {
        blocks.push(`- ${skill.name}: ${skill.description || ''}`);
      });
    }

    return blocks.join('\n');
  }

  private formatSkillInstruction(skill: InstructionSkillDefinition): string {
    const toolHint = Array.isArray(skill.allowedTools) && skill.allowedTools.length > 0
      ? `允许使用的工具: ${skill.allowedTools.join(', ')}`
      : '';
    const argHint = skill.argumentHint ? `参数提示: ${skill.argumentHint}` : '';
    const hints = [toolHint, argHint].filter(Boolean).join(' | ');

    return [
      `\n[Skill: ${skill.name}]`,
      skill.description ? `描述: ${skill.description}` : '',
      hints ? `提示: ${hints}` : '',
      skill.content ? `指令:\n${skill.content}` : '',
    ].filter(Boolean).join('\n');
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
    agent: AgentType,
    message: string,
    images?: string[],
    options?: AdapterOutputScope
  ): Promise<AdapterResponse> {
    const adapter = this.getOrCreateAdapter(agent);

    if (!adapter.isConnected) {
      await adapter.connect();
    }

    try {
      const beforeTotals = 'getTotalTokenUsage' in adapter && typeof (adapter as any).getTotalTokenUsage === 'function'
        ? (adapter as any).getTotalTokenUsage()
        : { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 };
      const content = await adapter.sendMessage(message, images);
      const afterTotals = 'getTotalTokenUsage' in adapter && typeof (adapter as any).getTotalTokenUsage === 'function'
        ? (adapter as any).getTotalTokenUsage()
        : { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 };

      const tokenUsage = {
        inputTokens: Math.max(0, (afterTotals.inputTokens || 0) - (beforeTotals.inputTokens || 0)),
        outputTokens: Math.max(0, (afterTotals.outputTokens || 0) - (beforeTotals.outputTokens || 0)),
        cacheReadTokens: (afterTotals.cacheReadTokens || 0) - (beforeTotals.cacheReadTokens || 0) || undefined,
        cacheWriteTokens: (afterTotals.cacheWriteTokens || 0) - (beforeTotals.cacheWriteTokens || 0) || undefined,
      };

      return {
        content,
        done: true,
        tokenUsage,
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
    // 关闭 LLM 适配器
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

    // 关闭 MCP 连接
    if (this.mcpExecutor) {
      try {
        await this.mcpExecutor.shutdown();
        logger.info('MCP executor shut down', undefined, LogCategory.TOOLS);
      } catch (error: any) {
        logger.error('Failed to shut down MCP executor', {
          error: error.message,
        }, LogCategory.TOOLS);
      }
    }

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

  /**
   * 清除特定适配器的对话历史（不断开连接）
   */
  clearAdapterHistory(agent: AgentType): void {
    const adapter = this.adapters.get(agent);
    if (adapter) {
      if ('clearHistory' in adapter && typeof adapter.clearHistory === 'function') {
        adapter.clearHistory();
        logger.info(`Cleared adapter history: ${agent}`, undefined, LogCategory.LLM);
      }
    }
  }

  /**
   * 清除所有适配器的对话历史（不断开连接）
   */
  clearAllAdapterHistories(): void {
    for (const [agent, adapter] of this.adapters) {
      if ('clearHistory' in adapter && typeof adapter.clearHistory === 'function') {
        adapter.clearHistory();
      }
    }
    logger.info('Cleared all adapter histories', undefined, LogCategory.LLM);
  }

  /**
   * 获取适配器历史信息（用于监控 token 消耗）
   */
  getAdapterHistoryInfo(agent: AgentType): { messages: number; chars: number } | null {
    const adapter = this.adapters.get(agent);
    if (!adapter) {
      return null;
    }

    if ('getHistoryLength' in adapter && 'getHistoryChars' in adapter) {
      return {
        messages: (adapter as any).getHistoryLength(),
        chars: (adapter as any).getHistoryChars(),
      };
    }

    return null;
  }

  /**
   * 获取所有适配器的历史信息
   */
  getAllAdapterHistoryInfo(): Map<AgentType, { messages: number; chars: number }> {
    const result = new Map<AgentType, { messages: number; chars: number }>();

    for (const [agent] of this.adapters) {
      const info = this.getAdapterHistoryInfo(agent);
      if (info) {
        result.set(agent, info);
      }
    }

    return result;
  }
}
