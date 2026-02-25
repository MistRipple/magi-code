/**
 * LLM 适配器工厂
 * 创建和管理 LLM 适配器实例
 *
 * 🔧 统一消息通道（unified-message-channel-design.md v2.5）
 *
 * 消息流职责：
 * - 创建 Adapter 并注入 MessageHub
 * - Adapter 直接通过 MessageHub 发送消息
 * - 只转发错误事件
 *
 * 🔧 单一真相来源架构：
 * - 工具、MCP、Skills 统一由 ToolManager 管理
 * - 环境上下文由 EnvironmentContextProvider 统一生成
 * - Orchestrator 和 Worker 都通过同一入口获取上下文
 */

import { EventEmitter } from 'events';
import { AgentType, WorkerSlot, TokenUsage } from '../types/agent-types';
import { BaseLLMAdapter } from './adapters/base-adapter';
import { WorkerLLMAdapter, WorkerAdapterConfig, getStallDetectionPreset } from './adapters/worker-adapter';
import { OrchestratorLLMAdapter, OrchestratorAdapterConfig } from './adapters/orchestrator-adapter';
import { LLMConfigLoader } from './config';
import { createLLMClient } from './clients/client-factory';
import { createNormalizer } from '../normalizer';
import { ToolManager } from '../tools/tool-manager';
import { SkillsManager } from '../tools/skills-manager';
import { MCPToolExecutor } from '../tools/mcp-executor';
import { MessageHub } from '../orchestrator/core/message-hub';
import { logger, LogCategory } from '../logging';
import { IAdapterFactory, AdapterOutputScope, AdapterResponse } from '../adapters/adapter-factory-interface';
import { ProfileLoader } from '../orchestrator/profile/profile-loader';
import { ADAPTER_EVENTS } from '../protocol/event-names';
import { EnvironmentContextProvider } from '../context/environment-context-provider';
import { WorkspaceFolderInfo } from '../workspace/workspace-roots';

/**
 * LLM 适配器工厂
 */
export class LLMAdapterFactory extends EventEmitter implements IAdapterFactory {
  private adapters = new Map<AgentType, BaseLLMAdapter>();
  private toolManager: ToolManager;
  private skillsManager: SkillsManager | null = null;
  private mcpExecutor: MCPToolExecutor | null = null;
  private readonly mcpExecutorDefaultId = 'mcp-servers';
  private mcpExecutorBindings = new Set<string>();
  private profileLoader: ProfileLoader;
  private connectionPromises = new Map<AgentType, Promise<void>>();

  /**
   * 🔧 环境上下文提供者（单一真相来源）
   */
  private environmentContextProvider: EnvironmentContextProvider;

  /**
   * 消息出口 - 注入给 Adapter，用于直接发送消息
   * 🔧 统一消息通道：替代 UnifiedMessageBus
   * 必须在创建 Adapter 之前通过 setMessageHub() 设置
   */
  private messageHub: MessageHub | null = null;

  constructor(options: { cwd: string; workspaceFolders?: WorkspaceFolderInfo[] }) {
    super();
    this.toolManager = new ToolManager({
      workspaceRoot: options.cwd,
      workspaceFolders: options.workspaceFolders,
    });
    this.profileLoader = ProfileLoader.getInstance();

    // 创建环境上下文提供者并注入 ToolManager
    this.environmentContextProvider = new EnvironmentContextProvider({
      workspace: this.toolManager.getWorkspacePromptDisplay(),
    });
    this.environmentContextProvider.setToolManager(this.toolManager);

    logger.info('LLM Adapter Factory initialized', { cwd: options.cwd }, LogCategory.LLM);
  }

  /**
   * 设置 MessageHub（由 WebviewProvider 在初始化时调用）
   * 🔧 统一消息通道：替代 setMessageBus
   * 必须在创建任何 Adapter 之前调用
   */
  setMessageHub(messageHub: MessageHub): void {
    this.messageHub = messageHub;
    logger.info('MessageHub 已注入到 AdapterFactory', undefined, LogCategory.LLM);
  }

  /**
   * 获取 MessageHub（内部使用）
   * @throws 如果 MessageHub 未设置
   */
  private getMessageHub(): MessageHub {
    if (!this.messageHub) {
      throw new Error('MessageHub 未设置，请先调用 setMessageHub()');
    }
    return this.messageHub;
  }

  /**
   * 初始化（加载画像配置和 Skills）
   */
  async initialize(): Promise<void> {
    LLMConfigLoader.ensureDefaults();
    await this.profileLoader.load();

    // 加载并注册 Skills
    await this.loadSkills();

    // 加载并注册 MCP
    await this.loadMCP();

    // 🔧 刷新环境上下文缓存（从 ToolManager 获取最新数据）
    await this.environmentContextProvider.refresh();

    logger.info('LLM Adapter Factory initialized', { configDir: LLMConfigLoader.getConfigDir() }, LogCategory.LLM);
  }

  /**
   * 加载并注册 Skills（用于指令型 Skills 和自定义工具）
   */
  private async loadSkills(): Promise<void> {
    try {
      // 加载 Skills 配置
      const skillsConfig = LLMConfigLoader.loadSkillsConfig();

      // 创建 SkillsManager（仅管理指令型 Skills 和自定义工具）
      this.skillsManager = new SkillsManager({
        customTools: skillsConfig?.customTools || [],
        instructionSkills: skillsConfig?.instructionSkills || [],
      });

      // 注意：内置工具已由 ToolManager 直接管理，不需要注册 SkillsManager
      // SkillsManager 现在用于指令型 Skills 提示注入 + 自定义工具执行
      this.toolManager.registerSkillExecutor(this.skillsManager);

      logger.info('Skills loaded', {
        customTools: this.skillsManager.getCustomTools().length,
        instructionSkills: this.skillsManager.getInstructionSkills().length,
      }, LogCategory.TOOLS);
    } catch (error: any) {
      logger.error('Failed to load skills', { error: error.message }, LogCategory.TOOLS);
    }
  }

  /**
   * 重新加载 Skills（用于安装新 skill 后）
   */
  async reloadSkills(): Promise<void> {
    // 重新加载 Skills 配置
    await this.loadSkills();

    // 🔧 刷新环境上下文缓存
    await this.environmentContextProvider.refresh();

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

      // 注册到 ToolManager（默认别名 + 实际 serverId，避免 sourceId 不匹配触发备用路径）
      this.registerMCPExecutorBindings(this.mcpExecutor);

      const tools = await this.mcpExecutor.getTools();
      const prompts = this.mcpExecutor.getPrompts();
      logger.info('MCP loaded and registered', {
        toolCount: tools.length,
        promptCount: prompts.length,
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
      this.unregisterMCPExecutorBindings();
    }

    // 重新加载
    await this.loadMCP();

    // 🔧 刷新环境上下文缓存
    await this.environmentContextProvider.refresh();

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
   * 🔧 使用 EnvironmentContextProvider 统一注入环境上下文
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

    // 创建适配器（注入 MessageHub）
    const adapterConfig: WorkerAdapterConfig = {
      client,
      normalizer,
      toolManager: this.toolManager,
      config: workerConfig,
      messageHub: this.getMessageHub(),  // 🔧 统一消息通道：使用 messageHub
      workerSlot,
      profileLoader: this.profileLoader,
      stallConfig: getStallDetectionPreset(workerSlot),
    };

    const adapter = new WorkerLLMAdapter(adapterConfig);

    // 🔧 使用 EnvironmentContextProvider 统一注入环境上下文
    let systemPrompt = adapter.getSystemPrompt();
    const environmentPrompt = this.environmentContextProvider.getEnvironmentPrompt();
    if (environmentPrompt) {
      systemPrompt = `${systemPrompt}\n\n${environmentPrompt}`;
    }
    adapter.setSystemPrompt(systemPrompt);

    // 只设置错误事件处理（消息由 Adapter 直接发送到 MessageHub）
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
   * 🔧 使用 EnvironmentContextProvider 统一注入环境上下文
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
    const normalizer = createNormalizer('claude', 'orchestrator', false, 'orchestrator');

    // 创建适配器（注入 MessageHub）
    const adapterConfig: OrchestratorAdapterConfig = {
      client,
      normalizer,
      toolManager: this.toolManager,
      config: orchestratorConfig,
      messageHub: this.getMessageHub(),  // 🔧 统一消息通道：使用 messageHub
    };

    const adapter = new OrchestratorLLMAdapter(adapterConfig);

    // 🔧 使用 EnvironmentContextProvider 统一注入环境上下文
    // 工具、MCP、Skills 统一由 ToolManager 管理，EnvironmentContextProvider 统一格式化
    let systemPrompt = adapter.getSystemPrompt();
    const environmentPrompt = this.environmentContextProvider.getEnvironmentPrompt();
    if (environmentPrompt) {
      systemPrompt = `${systemPrompt}\n\n${environmentPrompt}`;
    }
    adapter.setSystemPrompt(systemPrompt);

    // 只设置错误事件处理（消息由 Adapter 直接发送到 MessageHub）
    this.setupAdapterEvents(adapter, 'orchestrator');

    this.adapters.set('orchestrator', adapter);

    logger.info('Created orchestrator adapter', {
      provider: orchestratorConfig.provider,
      model: orchestratorConfig.model,
    }, LogCategory.LLM);

    return adapter;
  }

  /**
   * 设置适配器错误事件处理
   *
   * 🔧 统一消息通道：消息事件不再转发，Adapter 直接通过 MessageHub 发送消息
   * 只转发错误事件，供上层统一处理
   */
  private setupAdapterEvents(adapter: BaseLLMAdapter, _agent: AgentType): void {
    // 只转发错误事件
    adapter.on(ADAPTER_EVENTS.NORMALIZER_ERROR, (error) => {
      this.emit(ADAPTER_EVENTS.ERROR, error);
    });

    adapter.on(ADAPTER_EVENTS.ERROR, (error) => {
      this.emit(ADAPTER_EVENTS.ERROR, error);
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
   * @param agent - 代理类型
   * @param message - 消息内容
   * @param images - 图片（可选）
   * @param options - 输出范围配置
   *   - source: 消息来源
   *   - adapterRole: 适配器角色
   */
  async sendMessage(
    agent: AgentType,
    message: string,
    images?: string[],
    options?: AdapterOutputScope
  ): Promise<AdapterResponse> {
    const adapter = this.getOrCreateAdapter(agent);
    const decisionHook = options?.decisionHook;

    if (typeof (adapter as any).setDecisionHook === 'function') {
      (adapter as any).setDecisionHook(decisionHook);
    }

    // 为 orchestrator 适配器应用临时配置
    if (agent === 'orchestrator' && adapter instanceof OrchestratorLLMAdapter) {
      if (options?.systemPrompt) {
        adapter.setTempSystemPrompt(options.systemPrompt);
      }
      if (typeof options?.includeToolCalls === 'boolean') {
        adapter.setTempEnableToolCalls(options.includeToolCalls);
      }
      if (options?.visibility) {
        adapter.setTempVisibility(options.visibility);
      }
    }

    try {
      await this.ensureConnected(agent, adapter);
    } catch (error: any) {
      if (typeof (adapter as any).setDecisionHook === 'function') {
        (adapter as any).setDecisionHook(undefined);
      }
      return {
        content: '',
        done: false,
        error: error instanceof Error ? error.message : String(error),
      };
    }

    const sendOnce = async (): Promise<{ content: string; tokenUsage?: TokenUsage }> => {
      // requestContext 是全局状态，只有编排器的 LLM 输出才应绑定到 placeholder。
      // Worker 或 visibility:'system' 调用必须临时清除 requestContext，
      // 否则 Worker 的流式输出会复用编排器的 placeholder messageId，导致消息归属错误。
      const shouldDetachRequest = options?.visibility === 'system' || agent !== 'orchestrator';
      let savedRequestContext: string | undefined;
      if (shouldDetachRequest && this.messageHub) {
        savedRequestContext = this.messageHub.getRequestContext();
        this.messageHub.setRequestContext(undefined);
      }

      const beforeTotals = 'getTotalTokenUsage' in adapter && typeof (adapter as any).getTotalTokenUsage === 'function'
        ? (adapter as any).getTotalTokenUsage()
        : { inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 };
      try {
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

      return { content, tokenUsage };
      } finally {
        // 恢复 requestContext（无论成功或异常）
        if (shouldDetachRequest && this.messageHub && savedRequestContext !== undefined) {
          this.messageHub.setRequestContext(savedRequestContext);
        }
      }
    };

    const isWorker = agent !== 'orchestrator';
    const retryDelays = [10000, 20000, 30000];

    try {
      for (let attempt = 0; attempt <= retryDelays.length; attempt++) {
        try {
          const { content, tokenUsage } = await sendOnce();
          return {
            content,
            done: true,
            tokenUsage,
          };
        } catch (error: any) {
          const errorMessage = this.normalizeErrorMessage(error);
          if (!isWorker) {
            return {
              content: '',
              done: false,
              error: errorMessage,
            };
          }

          if (this.isAuthOrQuotaError(error)) {
            return {
              content: '',
              done: false,
              error: errorMessage,
            };
          }

          if (!this.isConnectionError(error) || attempt === retryDelays.length) {
            return {
              content: '',
              done: false,
              error: errorMessage,
            };
          }

          const delay = retryDelays[attempt];
          logger.warn('Worker 连接失败，准备重试', {
            agent,
            attempt: attempt + 1,
            delayMs: delay,
            error: errorMessage,
          }, LogCategory.LLM);

          try {
            await adapter.disconnect();
            await this.ensureConnected(agent, adapter);
          } catch (reconnectError: any) {
            logger.warn('Worker 重连失败', {
              agent,
              error: this.normalizeErrorMessage(reconnectError),
            }, LogCategory.LLM);
          }

          await this.sleep(delay);
        }
      }
      return { content: '', done: false, error: 'Worker request failed after retries.' };
    } finally {
      // 清理决策点回调，避免跨请求泄漏
      if (typeof (adapter as any).setDecisionHook === 'function') {
        (adapter as any).setDecisionHook(undefined);
      }
    }
  }

  /**
   * 确保适配器已连接
   *
   * 由于 connect() 现在是同步标记状态（不再发送测试请求），
   * 此方法已简化，移除了重试逻辑。
   */
  private async ensureConnected(agent: AgentType, adapter: BaseLLMAdapter): Promise<void> {
    if (adapter.isConnected) {
      return;
    }

    // 防止并发连接同一个适配器
    const existing = this.connectionPromises.get(agent);
    if (existing) {
      return existing;
    }

    const connectPromise = adapter.connect();
    this.connectionPromises.set(agent, connectPromise);
    try {
      await connectPromise;
    } finally {
      this.connectionPromises.delete(agent);
    }
  }

  private isAuthOrQuotaError(error: any): boolean {
    const status = error?.status || error?.response?.status;
    if (status === 401 || status === 403 || status === 429) return true;
    const message = this.normalizeErrorMessage(error).toLowerCase();
    return /unauthorized|forbidden|invalid api key|api key|auth|permission|quota|insufficient|billing|payment|exceeded|rate limit|limit|blocked|suspended|disabled|account/i.test(message);
  }

  private isConnectionError(error: any): boolean {
    const status = error?.status || error?.response?.status;
    if (status === 408 || status === 502 || status === 503 || status === 504) return true;
    const code = typeof error?.code === 'string' ? error.code : '';
    if (['ETIMEDOUT', 'ECONNRESET', 'ECONNREFUSED', 'ENOTFOUND', 'EAI_AGAIN'].includes(code)) {
      return true;
    }
    const message = this.normalizeErrorMessage(error).toLowerCase();
    return /timeout|timed out|network|connection|fetch failed|socket hang up|tls|certificate|econnreset|econnrefused|enotfound|eai_again|request ended without sending|stream ended|overloaded/.test(message);
  }

  private normalizeErrorMessage(error: any): string {
    if (!error) return 'Unknown error';
    if (typeof error === 'string') return error;
    if (error instanceof Error && error.message) return error.message;
    if (error?.message) return String(error.message);
    return String(error);
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
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
   * 中断所有适配器的当前请求（不销毁适配器）
   */
  async interruptAll(): Promise<void> {
    for (const [agent, adapter] of this.adapters) {
      try {
        await adapter.interrupt();
      } catch (error: any) {
        logger.error(`Failed to interrupt adapter: ${agent}`, { error: error.message }, LogCategory.LLM);
      }
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
        this.unregisterMCPExecutorBindings();
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
   * 注册 MCP 执行器绑定
   * 同时绑定默认别名和真实 serverId，确保 executeMCPTool 能直接命中。
   */
  private registerMCPExecutorBindings(executor: MCPToolExecutor): void {
    this.unregisterMCPExecutorBindings();

    const bindingIds = new Set<string>([this.mcpExecutorDefaultId]);
    for (const status of executor.getMCPManager().getAllServerStatuses()) {
      if (status.id) {
        bindingIds.add(status.id);
      }
    }

    for (const serverId of bindingIds) {
      this.toolManager.registerMCPExecutor(serverId, executor);
      this.mcpExecutorBindings.add(serverId);
    }
  }

  /**
   * 注销全部 MCP 执行器绑定
   */
  private unregisterMCPExecutorBindings(): void {
    for (const serverId of this.mcpExecutorBindings) {
      this.toolManager.unregisterMCPExecutor(serverId);
    }
    this.mcpExecutorBindings.clear();
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
    for (const [, adapter] of this.adapters) {
      if ('clearHistory' in adapter && typeof adapter.clearHistory === 'function') {
        adapter.clearHistory();
      }
    }
    logger.info('Cleared all adapter histories', undefined, LogCategory.LLM);
  }

  /**
   * 刷新所有适配器的环境上下文
   * 🔧 通过 EnvironmentContextProvider 统一处理
   */
  async refreshUserRules(): Promise<void> {
    // 刷新环境上下文缓存
    await this.environmentContextProvider.refresh();

    // 清除适配器缓存，强制重新创建（以获取新的环境上下文）
    this.adapters.clear();

    logger.info('Environment context refreshed', undefined, LogCategory.LLM);
  }

  /**
   * 获取环境提示词（IDE 状态 + 工具 + 用户规则等）
   */
  getEnvironmentPrompt(): string {
    return this.environmentContextProvider.getEnvironmentPrompt();
  }

  /**
   * 获取用户规则提示词
   */
  getUserRulesPrompt(): string {
    return this.environmentContextProvider.getUserRulesPrompt();
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
