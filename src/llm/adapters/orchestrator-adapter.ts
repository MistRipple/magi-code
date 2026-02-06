/**
 * Orchestrator LLM 适配器
 * 用于编排者代理
 *
 * 🔧 统一消息通道：使用 MessageHub 替代 UnifiedMessageBus
 */

import { AgentType, AgentRole, LLMConfig } from '../../types/agent-types';
import { LLMClient, LLMMessageParams, LLMMessage, ToolCall } from '../types';
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
  private tempEnableToolCalls?: boolean;

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
    const effectiveSystemPrompt = this.tempSystemPrompt ?? this.systemPrompt;
    const enableToolCalls = this.tempEnableToolCalls ?? false;
    this.tempSystemPrompt = undefined;
    this.tempEnableToolCalls = undefined;

    try {
      if (enableToolCalls) {
        const content = await this.sendMessageWithTools(
          message,
          images,
          effectiveSystemPrompt
        );
        this.setState(AdapterState.CONNECTED);
        return content;
      }

      // 准备消息历史（自动截断以控制 token 消耗）
      this.truncateHistoryIfNeeded();

      // 添加用户消息
      const userMessage = this.buildUserMessage(message, images);
      this.conversationHistory.push(userMessage);
      const messagesToSend = this.conversationHistory;

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

      // 将助手响应添加到历史
      this.conversationHistory.push({
        role: 'assistant',
        content: fullResponse,
      });

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
   * 设置临时工具调用开关（仅对下一次请求生效）
   */
  setTempEnableToolCalls(enabled: boolean): void {
    this.tempEnableToolCalls = enabled;
  }

  /**
   * 获取默认系统提示
   */
  private getDefaultSystemPrompt(): string {
    return `你是 MultiCLI 的任务编排者，负责协调多个专业 AI 协作完成开发任务。

你的职责：
1. 分析用户需求，拆解为可执行的子任务
2. 将子任务分配给合适的 Worker（Claude、Codex、Gemini）
3. 定义清晰的验收标准
4. 监控执行进度，协调各 Worker 之间的协作
5. 确保输出质量和一致性

可用的 Worker：
- Claude: 架构设计、代码重构、深度分析
- Codex: 代码生成、API 集成、测试编写
- Gemini: 前端 UI/UX、长文档分析、多模态理解

执行原则：
- 将复杂任务拆解为可管理的子任务
- 根据 Worker 特长分配任务
- 定义可测试的验收标准
- 考虑子任务之间的依赖关系
- 为 Worker 提供充分的上下文`;
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

  /**
   * 编排者工具调用模式（仅在显式启用时）
   */
  private async sendMessageWithTools(
    message: string,
    images: string[] | undefined,
    systemPrompt: string
  ): Promise<string> {
    // 自动截断历史以控制 token 消耗
    this.truncateHistoryIfNeeded();

    // 添加用户消息到历史
    this.conversationHistory.push(this.buildUserMessage(message, images));

    return await this.runToolCallingRound(this.conversationHistory, systemPrompt, 0);
  }

  /**
   * 单轮工具调用 + 递归收敛
   */
  private async runToolCallingRound(
    history: LLMMessage[],
    systemPrompt: string,
    recursionDepth: number
  ): Promise<string> {
    const tools = await this.toolManager.getTools();
    const toolDefinitions = tools.map((tool) => ({
      name: tool.name,
      description: tool.description,
      input_schema: tool.input_schema,
    }));

    const params: LLMMessageParams = {
      messages: history,
      systemPrompt,
      tools: toolDefinitions.length > 0 ? toolDefinitions : undefined,
      stream: true,
      maxTokens: 8192,
      temperature: 0.3,
    };

    const streamId = this.startStreamWithContext();
    let accumulatedText = '';
    let toolCalls: ToolCall[] = [];

    try {
      const response = await this.client.streamMessage(params, (chunk) => {
        if (chunk.type === 'content_delta' && chunk.content) {
          accumulatedText += chunk.content;
          this.normalizer.processTextDelta(streamId, chunk.content);
          this.emit('message', chunk.content);
        } else if (chunk.type === 'thinking' && chunk.thinking) {
          this.normalizer.processThinking(streamId, chunk.thinking);
          this.emit('thinking', chunk.thinking);
        } else if (chunk.type === 'tool_call_start' && chunk.toolCall) {
          this.emit('toolCall', chunk.toolCall.name || '', chunk.toolCall.arguments || {});
        }
      });
      this.recordTokenUsage(response.usage);

      if (response.toolCalls && response.toolCalls.length > 0) {
        toolCalls = response.toolCalls;
      }

      const assistantText = accumulatedText || response.content || '';
      if (toolCalls.length > 0) {
        for (const toolCall of toolCalls) {
          this.normalizer.addToolCall(streamId, {
            type: 'tool_call',
            toolName: toolCall.name,
            toolId: toolCall.id,
            status: 'running',
            input: JSON.stringify(toolCall.arguments, null, 2),
          });
        }

        const assistantContent: any[] = [];
        if (assistantText) {
          assistantContent.push({
            type: 'text',
            text: assistantText,
          });
        }
        for (const toolCall of toolCalls) {
          assistantContent.push({
            type: 'tool_use',
            id: toolCall.id,
            name: toolCall.name,
            input: toolCall.arguments,
          });
        }
        history.push({
          role: 'assistant',
          content: assistantContent,
        });

        const toolResults = await this.executeToolCalls(toolCalls);
        for (const result of toolResults) {
          this.normalizer.finishToolCall(
            streamId,
            result.toolCallId,
            result.isError ? undefined : result.content,
            result.isError ? result.content : undefined
          );
        }

        history.push({
          role: 'user',
          content: toolResults.map((result) => ({
            type: 'tool_result',
            tool_use_id: result.toolCallId,
            content: result.content,
            is_error: result.isError,
          })),
        });

        this.normalizer.endStream(streamId);

        if (recursionDepth >= 3) {
          return '多次工具调用后仍未产出最终回复，已中止以避免无限循环。';
        }
        return await this.runToolCallingRound(history, systemPrompt, recursionDepth + 1);
      }

      history.push({
        role: 'assistant',
        content: assistantText,
      });

      this.normalizer.endStream(streamId);
      if (!assistantText.trim()) {
        throw new Error('LLM 响应为空：流式传输完成但未收到有效内容');
      }
      return assistantText;
    } catch (error: any) {
      this.normalizer.endStream(streamId, error?.message || 'Request failed');
      throw error;
    }
  }

  /**
   * 执行工具调用
   */
  private async executeToolCalls(toolCalls: ToolCall[]) {
    const results = [];
    const maxToolResultChars = 20000;

    for (const toolCall of toolCalls) {
      try {
        const result = await this.toolManager.execute(toolCall);
        if (typeof result.content === 'string' && result.content.length > maxToolResultChars) {
          const truncated = result.content.slice(0, maxToolResultChars);
          result.content = `${truncated}\n...[truncated ${result.content.length - maxToolResultChars} chars]`;
        }
        results.push(result);
        this.emit('toolResult', toolCall.name, result.content);
      } catch (error: any) {
        results.push({
          toolCallId: toolCall.id,
          content: `Error: ${error?.message || String(error)}`,
          isError: true,
        });
      }
    }

    return results;
  }
}
