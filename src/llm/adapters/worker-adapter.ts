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
  private seenThinking = false;
  private decisionHookAppliedForThinking = false;

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

  /**
   * 迭代式工具调用模式
   *
   * 每轮 LLM 调用使用独立 streamId，首轮绑定 placeholder，后续轮次生成新 messageId。
   * 每张卡片包含当轮的 thinking + text + tool_call + tool_result。
   */
  private async sendMessageInternal(
    message: string | undefined,
    images: string[] | undefined,
    skipUserMessage: boolean,
  ): Promise<string> {
    if (!this.isConnected) {
      throw new Error('Adapter not connected');
    }

    this.setState(AdapterState.BUSY);
    this.currentTraceId = this.generateTraceId();

    // 自动截断历史以控制 token 消耗
    this.truncateHistoryIfNeeded();
    // 清理可能破坏工具调用链路的历史片段
    this.normalizeHistoryForTools();

    // 添加用户消息到历史（支持图片）
    if (!skipUserMessage) {
      if (images && images.length > 0) {
        const contentBlocks: any[] = [];
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
              source: { type: 'base64', media_type: mediaType, data: base64Data },
            });
          } catch (err) {
            logger.warn('Worker适配器.图片读取失败', { path: imagePath, error: String(err) }, LogCategory.LLM);
          }
        }
        if (message) {
          contentBlocks.push({ type: 'text', text: message });
        }
        this.conversationHistory.push({ role: 'user', content: contentBlocks });
      } else {
        this.conversationHistory.push({ role: 'user', content: message || '' });
      }
    }

    // 获取工具定义
    const tools = await this.toolManager.getTools();
    const toolDefinitions = tools.map((tool) => ({
      name: tool.name,
      description: tool.description,
      input_schema: tool.input_schema,
    }));

    // 每轮 LLM 调用独立一个 stream，确保时间轴正确：
    // 当轮 stream 内包含 thinking + text + tool_call + tool_result，
    // endStream 后再产生新消息或下一轮 stream，时间顺序天然正确。
    // 无轮次上限 — Worker 可执行任意多轮工具调用，
    // 异常终止完全依赖连续失败检测机制：连续 5 次失败 → 提示换方式，累计 25 轮失败 → 终止
    const CONSECUTIVE_FAIL_THRESHOLD = 5;
    const TOTAL_FAIL_LIMIT = 25;

    try {
      let finalText = '';
      let consecutiveFailures = 0;
      let totalFailures = 0;

      let round = 0;
      while (true) {
        this.seenThinking = false;
        this.decisionHookAppliedForThinking = false;

        // 只有首轮使用 startStreamWithContext 绑定 placeholder messageId，
        // 后续轮次生成新 messageId，避免复用同一个 ID 导致 Pipeline 重新激活覆盖前一轮内容
        const streamId = round === 0
          ? this.startStreamWithContext()
          : this.normalizer.startStream(this.currentTraceId!);

        const params: LLMMessageParams = {
          messages: this.conversationHistory,
          systemPrompt: this.systemPrompt,
          tools: toolDefinitions.length > 0 ? toolDefinitions : undefined,
          stream: true,
          maxTokens: 4096,
          temperature: 0.7,
        };

        let accumulatedText = '';
        let toolCalls: ToolCall[] = [];

        try {
          const response = await this.client.streamMessage(params, (chunk) => {
            if (chunk.type === 'content_delta' && chunk.content) {
              if (this.seenThinking && !this.decisionHookAppliedForThinking) {
                this.decisionHookAppliedForThinking = true;
                this.applyDecisionHook({ type: 'thinking' });
              }
              accumulatedText += chunk.content;
              this.normalizer.processTextDelta(streamId, chunk.content);
              this.emit('message', chunk.content);
            } else if (chunk.type === 'thinking' && chunk.thinking) {
              this.normalizer.processThinking(streamId, chunk.thinking);
              this.emit('thinking', chunk.thinking);
              this.seenThinking = true;
            } else if (chunk.type === 'tool_call_start' && chunk.toolCall) {
              this.emit('toolCall', chunk.toolCall.name || '', chunk.toolCall.arguments || {});
              this.applyDecisionHook({
                type: 'tool_call',
                toolName: chunk.toolCall.name || '',
                toolArgs: chunk.toolCall.arguments || {},
              });
            }
          });
          this.recordTokenUsage(response.usage);

          if (response.toolCalls && response.toolCalls.length > 0) {
            toolCalls = response.toolCalls;
          }

          const assistantText = accumulatedText || response.content || '';

          // 无工具调用 → 收敛
          if (toolCalls.length === 0) {
            this.conversationHistory.push({ role: 'assistant', content: assistantText });
            finalText = assistantText;
            this.normalizer.endStream(streamId);
            break;
          }

          // 有工具调用 → 同步到当轮 stream，执行工具，endStream 后进入下一轮
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
            assistantContent.push({ type: 'text', text: assistantText });
          }
          for (const toolCall of toolCalls) {
            assistantContent.push({
              type: 'tool_use',
              id: toolCall.id,
              name: toolCall.name,
              input: toolCall.arguments,
            });
          }
          this.conversationHistory.push({ role: 'assistant', content: assistantContent });

          const toolResults = await this.executeToolCalls(toolCalls);
          for (const result of toolResults) {
            this.normalizer.finishToolCall(
              streamId,
              result.toolCallId,
              result.isError ? undefined : result.content,
              result.isError ? result.content : undefined,
            );
          }

          this.conversationHistory.push({
            role: 'user',
            content: toolResults.map((result) => ({
              type: 'tool_result',
              tool_use_id: result.toolCallId,
              content: result.content,
              is_error: result.isError,
            })),
          });

          // 连续失败检测
          const allFailed = toolResults.every(r => r.isError);
          if (allFailed) {
            consecutiveFailures++;
            totalFailures++;

            if (totalFailures >= TOTAL_FAIL_LIMIT) {
              // 累计失败达到上限 → 终止
              finalText = assistantText || `工具调用累计失败 ${TOTAL_FAIL_LIMIT} 轮，判定为异常终止。`;
              this.normalizer.endStream(streamId);
              break;
            }

            if (consecutiveFailures >= CONSECUTIVE_FAIL_THRESHOLD) {
              // 连续失败达到阈值 → 注入提示让 LLM 换方式
              consecutiveFailures = 0;
              this.conversationHistory.push({
                role: 'user',
                content: `[System] 工具调用已连续失败 ${CONSECUTIVE_FAIL_THRESHOLD} 次，请换一种方式或策略继续处理任务。`,
              });
            }
          } else {
            consecutiveFailures = 0;
          }

          this.applyDecisionHook({ type: 'tool_result' });

          // 当轮 stream 结束，下一轮开启新 stream
          this.normalizer.endStream(streamId);
          round++;
        } catch (error: any) {
          this.normalizer.endStream(streamId, error?.message || 'Request failed');
          throw error;
        }
      }

      this.setState(AdapterState.CONNECTED);

      if (!finalText.trim()) {
        throw new Error('LLM 响应为空：流式传输完成但未收到有效内容');
      }

      return finalText;
    } catch (error: any) {
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
   * 决策点补充指令注入
   */
  private applyDecisionHook(event: { type: 'thinking' | 'tool_call' | 'tool_result'; toolName?: string; toolArgs?: any; toolResult?: string }): void {
    if (!this.decisionHook) {
      return;
    }
    const instructions = this.decisionHook(event) || [];
    if (instructions.length === 0) {
      return;
    }
    const content = `[System] 用户补充指令：\n${instructions.map(i => `- ${i}`).join('\n')}`;
    this.conversationHistory.push({
      role: 'user',
      content,
    });
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
    return `你是一个专业的软件开发助手。
你可以使用系统提供的工具来完成任务。
请逐步思考，在适当时使用工具。`;
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
