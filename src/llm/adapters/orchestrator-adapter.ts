/**
 * Orchestrator LLM 适配器
 * 用于编排者代理
 *
 * 🔧 统一消息通道：使用 MessageHub 替代 UnifiedMessageBus
 */

import { AgentType, AgentRole, LLMConfig } from '../../types/agent-types';
import { LLMClient, LLMMessageParams, LLMMessage } from '../types';
import { BaseNormalizer } from '../../normalizer/base-normalizer';
import { ToolManager } from '../../tools/tool-manager';
import { MessageHub } from '../../orchestrator/core/message-hub';
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
  messageHub: MessageHub;  // 🔧 统一消息通道：替代 messageBus
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

  /**
   * 临时配置（仅对下一次请求生效）
   */
  private tempSystemPrompt?: string;
  private tempIsolatedSession?: boolean;

  constructor(adapterConfig: OrchestratorAdapterConfig) {
    super(
      adapterConfig.client,
      adapterConfig.normalizer,
      adapterConfig.toolManager,
      adapterConfig.config,
      adapterConfig.messageHub  // 🔧 统一消息通道：使用 messageHub
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

    // 获取临时配置（使用后清除）
    const useIsolatedSession = this.tempIsolatedSession ?? false;
    const effectiveSystemPrompt = this.tempSystemPrompt ?? this.systemPrompt;
    this.tempIsolatedSession = undefined;
    this.tempSystemPrompt = undefined;

    try {
      // 准备消息历史
      let messagesToSend: LLMMessage[];

      if (useIsolatedSession) {
        // 独立会话模式：不使用历史记录
        messagesToSend = [];
      } else {
        // 自动截断历史以控制 token 消耗
        this.truncateHistoryIfNeeded();
        messagesToSend = this.conversationHistory;
      }

      // 添加用户消息
      const userMessage = this.buildUserMessage(message, images);

      if (useIsolatedSession) {
        // 独立会话模式：只发送当前消息
        messagesToSend = [userMessage];
      } else {
        // 正常模式：添加到历史
        this.conversationHistory.push(userMessage);
        messagesToSend = this.conversationHistory;
      }

      // Orchestrator 通常不需要工具，但可以根据需要启用
      const params: LLMMessageParams = {
        messages: messagesToSend,
        systemPrompt: effectiveSystemPrompt,
        stream: true,
        maxTokens: 8192, // Orchestrator 可能需要更多 tokens
        temperature: 0.3, // 更低的温度以获得更确定的规划
      };

      // 开始流式响应
      const streamId = this.startStreamWithContext();
      messageId = streamId;
      let fullResponse = '';

      // 流式调用 LLM
      const response = await this.client.streamMessage(params, (chunk) => {
        if (chunk.type === 'content_delta' && chunk.content) {
          fullResponse += chunk.content;
          this.normalizer.processTextDelta(streamId, chunk.content);
          this.emit('message', chunk.content);
        } else if (chunk.type === 'thinking' && chunk.thinking) {
          // 处理 thinking 内容
          this.normalizer.processThinking(streamId, chunk.thinking);
          this.emit('thinking', chunk.thinking);
        }
      });
      this.recordTokenUsage(response.usage);

      // 独立会话模式下不更新历史
      if (!useIsolatedSession) {
        this.conversationHistory.push({
          role: 'assistant',
          content: fullResponse,
        });
      }

      this.normalizer.endStream(streamId);
      this.setState(AdapterState.CONNECTED);

      // 🔧 如果流式传输完成但没有内容，抛出明确错误而非静默返回空
      if (!fullResponse.trim()) {
        throw new Error('LLM 响应为空：流式传输完成但未收到有效内容');
      }

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
   * 构建用户消息（支持图片）
   */
  private buildUserMessage(message: string, images?: string[]): LLMMessage {
    if (images && images.length > 0) {
      const contentBlocks: any[] = [];

      // 添加图片内容块
      for (const imagePath of images) {
        try {
          const fs = require('fs');
          const path = require('path');
          const imageBuffer = fs.readFileSync(imagePath);
          const base64Data = imageBuffer.toString('base64');
          const ext = path.extname(imagePath).toLowerCase().slice(1);
          const mediaType = ext === 'jpg' ? 'image/jpeg' : `image/${ext}`;

          contentBlocks.push({
            type: 'image',
            source: {
              type: 'base64',
              media_type: mediaType,
              data: base64Data,
            },
          });
        } catch (err) {
          logger.warn('Orchestrator适配器.图片读取失败', { path: imagePath, error: String(err) }, LogCategory.LLM);
        }
      }

      // 添加文本内容块
      if (message) {
        contentBlocks.push({
          type: 'text',
          text: message,
        });
      }

      return {
        role: 'user',
        content: contentBlocks,
      };
    }

    // 纯文本消息
    return {
      role: 'user',
      content: message,
    };
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
   * 设置临时系统提示（仅对下一次请求生效）
   */
  setTempSystemPrompt(prompt: string): void {
    this.tempSystemPrompt = prompt;
  }

  /**
   * 设置临时独立会话模式（仅对下一次请求生效）
   */
  setTempIsolatedSession(isolated: boolean): void {
    this.tempIsolatedSession = isolated;
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
