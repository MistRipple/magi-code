/**
 * Worker LLM 适配器
 * 用于 Worker 代理（claude, codex, gemini）
 *
 * 🔧 统一消息通道：使用 MessageHub 替代 UnifiedMessageBus
 */

import { AgentType, AgentRole, LLMConfig, WorkerSlot } from '../../types/agent-types';
import { LLMClient, LLMMessageParams, LLMMessage, ToolCall } from '../types';
import { BaseNormalizer } from '../../normalizer/base-normalizer';
import { ToolManager } from '../../tools/tool-manager';
import { MessageHub } from '../../orchestrator/core/message-hub';
import { BaseLLMAdapter, AdapterState } from './base-adapter';
import { logger, LogCategory } from '../../logging';
import { AgentProfileLoader } from '../../orchestrator/profile/agent-profile-loader';
import { GuidanceInjector } from '../../orchestrator/profile/guidance-injector';

/**
 * 历史管理配置
 */
export interface HistoryManagementConfig {
  /** 最大历史消息数量（默认 50） */
  maxMessages?: number;
  /** 最大历史字符数（默认 100000） */
  maxChars?: number;
  /** 保留最近 N 轮对话（默认 5） */
  preserveRecentRounds?: number;
}

/**
 * Worker 适配器配置
 */
export interface WorkerAdapterConfig {
  client: LLMClient;
  normalizer: BaseNormalizer;
  toolManager: ToolManager;
  config: LLMConfig;
  messageHub: MessageHub;  // 🔧 统一消息通道：替代 messageBus
  workerSlot: WorkerSlot;
  systemPrompt?: string;
  profileLoader?: AgentProfileLoader;
  historyConfig?: HistoryManagementConfig;
}

/**
 * Worker LLM 适配器
 */
export class WorkerLLMAdapter extends BaseLLMAdapter {
  private workerSlot: WorkerSlot;
  private systemPrompt: string;
  private conversationHistory: LLMMessage[] = [];
  private abortController?: AbortController;
  private profileLoader?: AgentProfileLoader;
  private guidanceInjector: GuidanceInjector;
  private historyConfig: Required<HistoryManagementConfig>;

  constructor(adapterConfig: WorkerAdapterConfig) {
    super(
      adapterConfig.client,
      adapterConfig.normalizer,
      adapterConfig.toolManager,
      adapterConfig.config,
      adapterConfig.messageHub  // 🔧 统一消息通道：使用 messageHub
    );
    this.workerSlot = adapterConfig.workerSlot;
    this.profileLoader = adapterConfig.profileLoader;
    this.guidanceInjector = new GuidanceInjector();
    this.systemPrompt = adapterConfig.systemPrompt || this.buildSystemPrompt();
    this.historyConfig = {
      maxMessages: adapterConfig.historyConfig?.maxMessages ?? 50,
      maxChars: adapterConfig.historyConfig?.maxChars ?? 100000,
      preserveRecentRounds: adapterConfig.historyConfig?.preserveRecentRounds ?? 5,
    };
  }

  /**
   * 获取代理类型
   */
  get agent(): AgentType {
    return this.workerSlot;
  }

  /**
   * 获取代理角色
   */
  get role(): AgentRole {
    return 'worker';
  }

  /**
   * 发送消息
   */
  async sendMessage(message: string, images?: string[]): Promise<string> {
    return this.sendMessageInternal(message, images, false);
  }

  private async sendMessageInternal(
    message: string | undefined,
    images: string[] | undefined,
    skipUserMessage: boolean,
    recursionDepth: number = 0
  ): Promise<string> {
    if (!this.isConnected) {
      throw new Error('Adapter not connected');
    }

    this.setState(AdapterState.BUSY);
    this.currentTraceId = this.generateTraceId();
    let messageId: string | null = null;

    try {
      // 自动截断历史以控制 token 消耗
      this.truncateHistoryIfNeeded();

      // 清理可能破坏工具调用链路的历史片段
      this.normalizeHistoryForTools();

      // 添加用户消息到历史（支持图片）
      if (!skipUserMessage) {
        // 🔧 如果有图片，构建多模态内容块
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
              logger.warn('Worker适配器.图片读取失败', { path: imagePath, error: String(err) }, LogCategory.LLM);
            }
          }

          // 添加文本内容块
          if (message) {
            contentBlocks.push({
              type: 'text',
              text: message,
            });
          }

          this.conversationHistory.push({
            role: 'user',
            content: contentBlocks,
          });
        } else {
          // 纯文本消息
          this.conversationHistory.push({
            role: 'user',
            content: message || '',
          });
        }
      }

      // 获取工具定义
      const tools = await this.toolManager.getTools();
      const toolDefinitions = tools.map((tool) => ({
        name: tool.name,
        description: tool.description,
        input_schema: tool.input_schema,
      }));

      const extractText = (content: unknown): string => {
        if (!content) return '';
        if (typeof content === 'string') return content;
        if (Array.isArray(content)) {
          return content
            .map((block) => {
              if (!block || typeof block !== 'object') return '';
              const maybeText = (block as { text?: string }).text;
              return typeof maybeText === 'string' ? maybeText : '';
            })
            .filter(Boolean)
            .join('\n');
        }
        return '';
      };

      const combinedContent = [
        this.systemPrompt || '',
        message || '',
        ...this.conversationHistory.map((entry) => extractText(entry.content)),
      ]
        .filter(Boolean)
        .join('\n');
      const hasFileTarget = Boolean(
        combinedContent &&
        /[\w\-./]+\.(ts|js|tsx|jsx|py|java|go|rs|cpp|c|h|hpp|css|scss|html|json|md|yaml|yml|txt)/i.test(combinedContent)
      );
      const readOnlyIntent = Boolean(
        combinedContent &&
        /(不要修改|仅需读取|只需读取|只读分析)/.test(combinedContent)
      );
      const forceToolUse = Boolean(
        toolDefinitions.length > 0 &&
        !readOnlyIntent &&
        (/必须使用工具/.test(combinedContent) ||
          /必须使用\s*text_editor/.test(combinedContent) ||
          /\btext_editor\b/.test(combinedContent) ||
          /目标文件/.test(combinedContent) ||
          /目标路径/.test(combinedContent) ||
          hasFileTarget)
      );

      const preferredTool = toolDefinitions.find(tool => tool.name === 'text_editor')?.name;
      const effectiveTools = forceToolUse && preferredTool
        ? toolDefinitions.filter(tool => tool.name === preferredTool)
        : toolDefinitions;
      const toolChoice = undefined;

      // 构建请求参数
      const params: LLMMessageParams = {
        messages: this.conversationHistory,
        systemPrompt: this.systemPrompt,
        tools: effectiveTools.length > 0 ? effectiveTools : undefined,
        stream: true,
        maxTokens: 4096,
        temperature: 0.7,
        toolChoice,
      };

      // 开始流式响应
      const streamId = this.startStreamWithContext();
      messageId = streamId;
      let fullResponse = '';
      let toolCalls: ToolCall[] = [];

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
        } else if (chunk.type === 'tool_call_start' && chunk.toolCall) {
          this.emit('toolCall', chunk.toolCall.name || '', chunk.toolCall.arguments || {});
        }
      });
      this.recordTokenUsage(response.usage);

      // 处理工具调用
      if (response.toolCalls && response.toolCalls.length > 0) {
        toolCalls = response.toolCalls;
        const allowedTool = forceToolUse && preferredTool ? preferredTool : null;

        // 添加助手响应到历史（包含工具调用）
        const assistantContent: any[] = [];

        // 如果有文本内容，添加文本块
        if (response.content) {
          assistantContent.push({
            type: 'text',
            text: response.content
          });
        }

        // 添加工具使用块
        for (const toolCall of toolCalls) {
          assistantContent.push({
            type: 'tool_use',
            id: toolCall.id,
            name: toolCall.name,
            input: toolCall.arguments
          });
        }

        this.conversationHistory.push({
          role: 'assistant',
          content: assistantContent,
        });

        // 执行工具调用
        const toolResults = await (async () => {
          if (!allowedTool) {
            return await this.executeToolCalls(toolCalls);
          }
          const allowedCalls = toolCalls.filter(call => call.name === allowedTool);
          const blockedCalls = toolCalls.filter(call => call.name !== allowedTool);
          const blockedResults = blockedCalls.map(call => ({
            toolCallId: call.id,
            content: `Tool blocked: only '${allowedTool}' is permitted for this task. Use text_editor to edit target files.`,
            isError: true,
          }));
          const allowedResults = await this.executeToolCalls(allowedCalls);
          return [...blockedResults, ...allowedResults];
        })();

        // 添加工具结果到历史（使用 ContentBlock 格式）
        const toolResultContent: any[] = toolResults.map(result => ({
          type: 'tool_result',
          tool_use_id: result.toolCallId,
          content: result.content,
          is_error: result.isError
        }));

        this.conversationHistory.push({
          role: 'user',
          content: toolResultContent,
        });

        // 递归调用以获取最终响应
        this.normalizer.endStream(messageId);
        if (recursionDepth >= 3) {
          return '多次工具调用后仍未产出最终回复，已中止以避免无限循环。';
        }
        return await this.sendMessageInternal(undefined, undefined, true, recursionDepth + 1);
      }

      if (forceToolUse && preferredTool && !response.toolCalls?.length) {
        this.conversationHistory.push({
          role: 'assistant',
          content: fullResponse,
        });
        this.conversationHistory.push({
          role: 'user',
          content: `必须使用 ${preferredTool} 完成目标文件修改。请调用该工具后继续。`,
        });
        this.normalizer.endStream(messageId);
        if (recursionDepth >= 3) {
          return '多次工具调用后仍未产出最终回复，已中止以避免无限循环。';
        }
        return await this.sendMessageInternal(undefined, undefined, true, recursionDepth + 1);
      }

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
    logger.info(`${this.agent} adapter interrupted`, undefined, LogCategory.LLM);
  }

  /**
   * 清除对话历史
   */
  clearHistory(): void {
    this.conversationHistory = [];
    logger.debug(`${this.agent} conversation history cleared`, undefined, LogCategory.LLM);
  }

  /**
   * 设置系统提示
   */
  setSystemPrompt(prompt: string): void {
    this.systemPrompt = prompt;
    logger.debug(`${this.agent} system prompt updated`, undefined, LogCategory.LLM);
  }

  getSystemPrompt(): string {
    return this.systemPrompt;
  }

  /**
   * 执行工具调用
   */
  private async executeToolCalls(toolCalls: ToolCall[]) {
    const results = [];
    const maxToolResultChars = 20000;

    for (const toolCall of toolCalls) {
      try {
        logger.debug(`Executing tool: ${toolCall.name}`, { args: toolCall.arguments }, LogCategory.TOOLS);

        const result = await this.toolManager.execute(toolCall);
        if (typeof result.content === 'string' && result.content.length > maxToolResultChars) {
          const truncated = result.content.slice(0, maxToolResultChars);
          result.content = `${truncated}\n...[truncated ${result.content.length - maxToolResultChars} chars]`;
        }
        results.push(result);

        this.emit('toolResult', toolCall.name, result.content);

        logger.debug(`Tool execution completed: ${toolCall.name}`, {
          success: !result.isError,
        }, LogCategory.TOOLS);
      } catch (error: any) {
        logger.error(`Tool execution failed: ${toolCall.name}`, {
          error: error.message,
        }, LogCategory.TOOLS);

        results.push({
          toolCallId: toolCall.id,
          content: `Error: ${error.message}`,
          isError: true,
        });
      }
    }

    return results;
  }

  /**
   * 构建系统提示（使用 Agent 画像）
   */
  private buildSystemPrompt(): string {
    if (!this.profileLoader) {
      return this.getDefaultSystemPrompt();
    }

    try {
      const workerProfile = this.profileLoader.getProfileLoader().getProfile(this.workerSlot);
      const guidancePrompt = this.guidanceInjector.buildWorkerPrompt(workerProfile, {
        taskDescription: '', // 将在实际任务中填充
      });

      return guidancePrompt;
    } catch (error: any) {
      logger.warn(`Failed to build system prompt from profile: ${error.message}`, undefined, LogCategory.LLM);
      return this.getDefaultSystemPrompt();
    }
  }

  /**
   * 获取默认系统提示
   */
  private getDefaultSystemPrompt(): string {
    return `You are a helpful AI assistant specialized in software development.
You have access to various tools to help complete tasks.
Always think step by step and use tools when appropriate.`;
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
      logger.debug(`${this.agent} history truncated`, {
        removedMessages: truncatedCount,
        remainingMessages: this.conversationHistory.length,
        previousChars: currentChars,
        currentChars: this.getHistoryChars(),
      }, LogCategory.LLM);
    }
  }

  private normalizeHistoryForTools(): void {
    if (this.conversationHistory.length === 0) {
      return;
    }

    const cleaned: LLMMessage[] = [];
    for (let i = 0; i < this.conversationHistory.length; i++) {
      const msg = this.conversationHistory[i];

      if (this.hasToolUse(msg)) {
        const next = this.conversationHistory[i + 1];
        const prev = cleaned[cleaned.length - 1];
        if (!this.isToolResultUser(next) || !this.isUserOrToolResult(prev)) {
          continue;
        }
        cleaned.push(msg);
        cleaned.push(next);
        i += 1;
        continue;
      }

      if (this.isToolResultUser(msg)) {
        const prev = this.conversationHistory[i - 1];
        if (!this.hasToolUse(prev)) {
          continue;
        }
      }

      cleaned.push(msg);
    }

    this.conversationHistory = cleaned;
  }

  private isUserOrToolResult(message?: LLMMessage): boolean {
    if (!message) {
      return false;
    }
    if (message.role === 'user') {
      return true;
    }
    return this.isToolResultUser(message);
  }

  private hasToolUse(message?: LLMMessage): boolean {
    if (!message || !Array.isArray(message.content)) {
      return false;
    }
    return message.content.some((block: any) => block?.type === 'tool_use');
  }

  private isToolResultUser(message?: LLMMessage): boolean {
    if (!message || message.role !== 'user' || !Array.isArray(message.content)) {
      return false;
    }
    return message.content.some((block: any) => block?.type === 'tool_result');
  }
}
