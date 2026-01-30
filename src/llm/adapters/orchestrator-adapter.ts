/**
 * Orchestrator LLM 适配器
 * 用于编排者代理
 */

import { AgentType, AgentRole, LLMConfig } from '../../types/agent-types';
import { LLMClient, LLMMessageParams, LLMMessage } from '../types';
import { BaseNormalizer } from '../../normalizer/base-normalizer';
import { ToolManager } from '../../tools/tool-manager';
import { UnifiedMessageBus } from '../../normalizer/unified-message-bus';
import { BaseLLMAdapter, AdapterState } from './base-adapter';
import { logger, LogCategory } from '../../logging';

/**
 * 历史管理配置
 */
export interface OrchestratorHistoryConfig {
  /** 最大历史消息数量（默认 30） */
  maxMessages?: number;
  /** 最大历史字符数（默认 80000） */
  maxChars?: number;
  /** 保留最近 N 轮对话（默认 3） */
  preserveRecentRounds?: number;
}

/**
 * Orchestrator 适配器配置
 */
export interface OrchestratorAdapterConfig {
  client: LLMClient;
  normalizer: BaseNormalizer;
  toolManager: ToolManager;
  config: LLMConfig;
  messageBus: UnifiedMessageBus;  // 必选：消息总线
  systemPrompt?: string;
  historyConfig?: OrchestratorHistoryConfig;
}

/**
 * Orchestrator LLM 适配器
 */
export class OrchestratorLLMAdapter extends BaseLLMAdapter {
  private systemPrompt: string;
  private conversationHistory: LLMMessage[] = [];
  private abortController?: AbortController;
  private historyConfig: Required<OrchestratorHistoryConfig>;

  constructor(adapterConfig: OrchestratorAdapterConfig) {
    super(
      adapterConfig.client,
      adapterConfig.normalizer,
      adapterConfig.toolManager,
      adapterConfig.config,
      adapterConfig.messageBus
    );
    this.systemPrompt = adapterConfig.systemPrompt || this.getDefaultSystemPrompt();
    this.historyConfig = {
      maxMessages: adapterConfig.historyConfig?.maxMessages ?? 30,
      maxChars: adapterConfig.historyConfig?.maxChars ?? 80000,
      preserveRecentRounds: adapterConfig.historyConfig?.preserveRecentRounds ?? 3,
    };
  }

  /**
   * 获取代理类型
   */
  get agent(): AgentType {
    return 'orchestrator';
  }

  /**
   * 获取代理角色
   */
  get role(): AgentRole {
    return 'orchestrator';
  }

  /**
   * 发送消息
   */
  async sendMessage(message: string, images?: string[]): Promise<string> {
    if (!this.isConnected) {
      throw new Error('Adapter not connected');
    }

    this.setState(AdapterState.BUSY);
    this.currentTraceId = this.generateTraceId();
    let messageId: string | null = null;

    try {
      // 自动截断历史以控制 token 消耗
      this.truncateHistoryIfNeeded();

      // 添加用户消息到历史
      this.conversationHistory.push({
        role: 'user',
        content: message,
      });

      // Orchestrator 通常不需要工具，但可以根据需要启用
      const params: LLMMessageParams = {
        messages: this.conversationHistory,
        systemPrompt: this.systemPrompt,
        stream: true,
        maxTokens: 8192, // Orchestrator 可能需要更多 tokens
        temperature: 0.3, // 更低的温度以获得更确定的规划
      };

      // 开始流式响应
      const streamId = this.normalizer.startStream(this.currentTraceId);
      messageId = streamId;
      let fullResponse = '';

      // 流式调用 LLM
      const response = await this.client.streamMessage(params, (chunk) => {
        if (chunk.type === 'content_delta' && chunk.content) {
          fullResponse += chunk.content;
          this.normalizer.processChunk(streamId, chunk.content);
          this.emit('message', chunk.content);
        }
      });
      this.recordTokenUsage(response.usage);

      // 添加助手响应到历史
      this.conversationHistory.push({
        role: 'assistant',
        content: fullResponse,
      });

      this.normalizer.endStream(streamId);
      this.setState(AdapterState.CONNECTED);

      return fullResponse;
    } catch (error: any) {
      if (messageId) {
        this.normalizer.endStream(messageId, error?.message || 'Request failed');
      }
      this.setState(AdapterState.ERROR);
      this.emitError(error);
      throw error;
    }
  }

  /**
   * 中断当前请求
   */
  async interrupt(): Promise<void> {
    if (this.abortController) {
      this.abortController.abort();
      this.abortController = undefined;
    }
    this.setState(AdapterState.CONNECTED);
    logger.info('Orchestrator adapter interrupted', undefined, LogCategory.LLM);
  }

  /**
   * 清除对话历史
   */
  clearHistory(): void {
    this.conversationHistory = [];
    logger.debug('Orchestrator conversation history cleared', undefined, LogCategory.LLM);
  }

  /**
   * 设置系统提示
   */
  setSystemPrompt(prompt: string): void {
    this.systemPrompt = prompt;
    logger.debug('Orchestrator system prompt updated', undefined, LogCategory.LLM);
  }

  getSystemPrompt(): string {
    return this.systemPrompt;
  }

  /**
   * 获取默认系统提示
   */
  private getDefaultSystemPrompt(): string {
    return `You are an intelligent task orchestrator for a multi-agent development system.

Your responsibilities:
1. Analyze user requirements and break them down into subtasks
2. Assign subtasks to appropriate worker agents (claude, codex, gemini)
3. Define clear acceptance criteria for each subtask
4. Monitor progress and coordinate between workers
5. Ensure quality and consistency across all work

Available workers:
- claude: General-purpose coding, refactoring, documentation
- codex: Code generation, API integration, testing
- gemini: UI/UX, frontend development, design

Guidelines:
- Break complex tasks into manageable subtasks
- Assign tasks based on worker strengths
- Define clear, testable acceptance criteria
- Consider dependencies between subtasks
- Provide context and guidance to workers`;
  }

  /**
   * 获取对话历史
   */
  getHistory(): LLMMessage[] {
    return [...this.conversationHistory];
  }

  /**
   * 获取历史消息数量
   */
  getHistoryLength(): number {
    return this.conversationHistory.length;
  }

  /**
   * 获取历史总字符数
   */
  getHistoryChars(): number {
    return this.conversationHistory.reduce((total, msg) => {
      if (typeof msg.content === 'string') {
        return total + msg.content.length;
      } else if (Array.isArray(msg.content)) {
        return total + JSON.stringify(msg.content).length;
      }
      return total;
    }, 0);
  }

  /**
   * 截断历史（如果超过限制）
   * 保留最近的 N 轮对话
   */
  private truncateHistoryIfNeeded(): void {
    const { maxMessages, maxChars, preserveRecentRounds } = this.historyConfig;

    // 检查是否需要截断
    const currentLength = this.conversationHistory.length;
    const currentChars = this.getHistoryChars();

    if (currentLength <= maxMessages && currentChars <= maxChars) {
      return; // 无需截断
    }

    // 计算需要保留的消息数量（每轮对话约 2 条消息：user + assistant）
    const preserveCount = Math.min(preserveRecentRounds * 2, currentLength);

    // 截断旧消息，保留最近的
    const truncatedCount = currentLength - preserveCount;
    if (truncatedCount > 0) {
      this.conversationHistory = this.conversationHistory.slice(-preserveCount);
      logger.debug('Orchestrator history truncated', {
        removedMessages: truncatedCount,
        remainingMessages: this.conversationHistory.length,
        previousChars: currentChars,
        currentChars: this.getHistoryChars(),
      }, LogCategory.LLM);
    }
  }

  /**
   * 添加系统消息
   */
  addSystemMessage(content: string): void {
    this.conversationHistory.push({
      role: 'system',
      content,
    });
  }

  /**
   * 添加助手消息（用于注入上下文）
   */
  addAssistantMessage(content: string): void {
    this.conversationHistory.push({
      role: 'assistant',
      content,
    });
  }
}
