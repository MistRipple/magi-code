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
  /** 编排者单次会话中允许直接修改的最大文件数 */
  private static readonly MAX_ORCHESTRATOR_EDIT_FILES = 3;

  private systemPrompt: string;
  private conversationHistory: LLMMessage[] = [];
  private abortController?: AbortController;
  private historyConfig: Required<OrchestratorHistoryConfig>;

  /** 当前会话中编排者已修改的文件路径集合（用于规模限制） */
  private editedFiles = new Set<string>();

  /**
   * 临时配置（仅对下一次请求生效）
   */
  private tempSystemPrompt?: string;
  private tempEnableToolCalls?: boolean;
  private tempVisibility?: 'user' | 'system' | 'debug';

  constructor(adapterConfig: OrchestratorAdapterConfig) {
    super(
      adapterConfig.client,
      adapterConfig.normalizer,
      adapterConfig.toolManager,
      adapterConfig.config,
      adapterConfig.messageHub  // 🔧 统一消息通道：使用 messageHub
    );
    this.systemPrompt = adapterConfig.systemPrompt ?? '';
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
    const silent = this.tempVisibility === 'system';
    this.tempSystemPrompt = undefined;
    this.tempEnableToolCalls = undefined;
    this.tempVisibility = undefined;

    try {
      if (enableToolCalls) {
        const content = await this.sendMessageWithTools(
          message,
          images,
          effectiveSystemPrompt,
          silent ? 'system' : undefined
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

      // 创建 AbortController，供 interrupt() 中断 LLM 请求
      this.abortController = new AbortController();

      // Orchestrator 通常不需要工具，但可以根据需要启用
      const params: LLMMessageParams = {
        messages: messagesToSend,
        systemPrompt: effectiveSystemPrompt,
        stream: true,
        maxTokens: 8192, // Orchestrator 可能需要更多 tokens
        temperature: 0.3, // 更低的温度以获得更确定的规划
        signal: this.abortController.signal,
      };

      // visibility: 'system' 时不绑定 placeholder，使用独立 messageId，且标记 visibility 让前端拦截
      let streamId: string;
      if (silent) {
        if (!this.currentTraceId) {
          this.currentTraceId = this.generateTraceId();
        }
        streamId = this.normalizer.startStream(this.currentTraceId, undefined, undefined, 'system');
      } else {
        streamId = this.startStreamWithContext();
      }
      messageId = streamId;
      let fullResponse = '';

      // 流式调用 LLM
      const response = await this.client.streamMessage(params, (chunk) => {
        if (chunk.type === 'content_delta' && chunk.content) {
          fullResponse += chunk.content;
          this.normalizer.processTextDelta(streamId, chunk.content);
          this.emit('message', chunk.content);
        } else if (chunk.type === 'thinking' && chunk.thinking) {
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
      // abort 中断不视为错误
      if (error?.name === 'AbortError' || this.abortController?.signal.aborted) {
        if (messageId) {
          this.normalizer.endStream(messageId);
        }
        this.setState(AdapterState.CONNECTED);
        return '任务已中断';
      }
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
      // 不清除 abortController 引用 — 循环内的 abort 状态检查（L436/L518）
      // 依赖 abortController.signal.aborted 判断中断状态。
      // 下次 sendMessage 调用时会创建新的 AbortController 覆盖。
    }
    this.setState(AdapterState.CONNECTED);
    logger.info('Orchestrator adapter interrupted', undefined, LogCategory.LLM);
  }

  /**
   * 清除对话历史
   */
  clearHistory(): void {
    this.conversationHistory = [];
    this.editedFiles.clear();
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
   * 设置临时可见性（仅对下一次请求生效）
   * visibility: 'system' 时，LLM 调用跳过 normalizer，不产生前端消息
   */
  setTempVisibility(visibility: 'user' | 'system' | 'debug'): void {
    this.tempVisibility = visibility;
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
   *
   * 使用迭代循环（而非递归）实现工具调用链，
   * 整个循环共享一个 streamId，确保用户只看到一条流式消息。
   */
  private async sendMessageWithTools(
    message: string,
    images: string[] | undefined,
    systemPrompt: string,
    visibility?: 'user' | 'system' | 'debug'
  ): Promise<string> {
    // 自动截断历史以控制 token 消耗
    this.truncateHistoryIfNeeded();

    // 添加用户消息到历史
    const history = this.conversationHistory;
    history.push(this.buildUserMessage(message, images));

    const tools = await this.toolManager.getTools();
    const toolDefinitions = tools.map((tool) => ({
      name: tool.name,
      description: tool.description,
      input_schema: tool.input_schema,
    }));

    // 每轮 LLM 调用独立一个 stream，确保时间轴正确：
    // 当轮 stream 内包含 thinking + text + tool_call + tool_result，
    // endStream 后工具副作用产生的新消息（如 subTaskCard）自然排在后面，
    // 下一轮 stream 再开启新卡片，时间顺序天然正确。
    // 异常终止依赖三层止损机制：
    // 1. 连续失败检测：连续 5 次失败 → 提示换方式，累计 25 轮失败 → 终止
    // 2. 总轮次安全网：80 轮硬上限 → 强制总结 → 终止
    // 3. 空转检测：连续同工具重复 / 连续无编排动作 → 渐进式引导 → 强制总结
    const CONSECUTIVE_FAIL_THRESHOLD = 5;
    const TOTAL_FAIL_LIMIT = 25;
    const MAX_TOTAL_ROUNDS = 80;
    const FINAL_WARN_ROUND = MAX_TOTAL_ROUNDS - 10;
    // 连续同工具重复检测阈值
    // L1 场景下 Orchestrator 连续调用 file_view 读取不同文件是正常的分析行为，
    // 阈值需高于 Worker adapter（Worker 有 visitedPaths 等精细去重，Orchestrator 靠轮次粗检测）
    const SAME_TOOL_WARN = 6;
    const SAME_TOOL_FORCE = 10;
    // 编排者空转检测：连续多轮只用只读工具不做编排动作（dispatch_task / 写入操作）
    const STALL_WARN = 10;
    const STALL_FORCE = 15;

    try {
      let finalText = '';
      let consecutiveFailures = 0;
      let totalFailures = 0;
      // 总轮次强制总结模式
      let forceNoToolsNextRound = false;
      // 连续同工具重复检测状态
      let lastPrimaryToolName = '';
      let consecutiveSameToolRounds = 0;
      // 编排者空转检测状态：连续多轮无编排动作
      let consecutiveStallRounds = 0;

      // 创建 AbortController，供 interrupt() 中断 LLM 请求
      this.abortController = new AbortController();

      let round = 0;
      while (true) {
        // 中断检查：每轮迭代入口检测 abort 信号
        if (this.abortController.signal.aborted) {
          break;
        }

        // 总轮次安全网：防止任何场景下的无限循环
        // round == MAX_TOTAL_ROUNDS → 注入提示 + 撤掉工具，给 LLM 一轮纯文本总结机会
        // round >  MAX_TOTAL_ROUNDS → LLM 仍调用工具或未收敛，强制终止
        if (round > MAX_TOTAL_ROUNDS) {
          logger.warn('Orchestrator 超过总轮次上限，强制终止', { round }, LogCategory.LLM);
          finalText = finalText || `已执行 ${round} 轮工具调用，达到安全上限，编排终止。`;
          break;
        }
        if (round === MAX_TOTAL_ROUNDS) {
          forceNoToolsNextRound = true;
          logger.warn('Orchestrator 达到总轮次上限，触发强制总结', { round }, LogCategory.LLM);
          history.push({
            role: 'user',
            content: `[System] 你已执行 ${round} 轮工具调用，达到系统上限。工具调用能力已被收回。请立即总结当前编排进展和执行结果。`,
          });
        }
        if (round === FINAL_WARN_ROUND) {
          history.push({
            role: 'user',
            content: `[System] 你已执行 ${round} 轮工具调用，即将达到上限（${MAX_TOTAL_ROUNDS} 轮）。请尽快完成剩余编排工作并输出最终结论。`,
          });
        }

        // 只有首轮使用 startStreamWithContext 绑定 placeholder messageId，
        // 后续轮次生成新 messageId，避免复用同一个 ID 导致 Pipeline 重新激活覆盖前一轮内容
        const streamId = visibility === 'system'
          ? this.normalizer.startStream(this.currentTraceId!, undefined, undefined, 'system')
          : round === 0
            ? this.startStreamWithContext()
            : this.normalizer.startStream(this.currentTraceId!);

        const params: LLMMessageParams = {
          messages: history,
          systemPrompt,
          tools: forceNoToolsNextRound ? undefined : (toolDefinitions.length > 0 ? toolDefinitions : undefined),
          stream: true,
          maxTokens: 8192,
          temperature: 0.3,
          signal: this.abortController.signal,
        };

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

          // 无工具调用 → 收敛
          if (toolCalls.length === 0) {
            history.push({ role: 'assistant', content: assistantText });
            finalText = assistantText;
            this.normalizer.endStream(streamId);
            break;
          }

          // 有工具调用 → 同步到当轮 stream
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
          history.push({ role: 'assistant', content: assistantContent });

          const toolResults = await this.executeToolCalls(toolCalls);

          // 中断检查：工具执行完成后立即检测 abort，跳过后续处理直接退出循环
          if (this.abortController?.signal.aborted) {
            this.normalizer.endStream(streamId);
            break;
          }

          for (const result of toolResults) {
            this.normalizer.finishToolCall(
              streamId,
              result.toolCallId,
              result.isError ? undefined : result.content,
              result.isError ? result.content : undefined,
              result.fileChange,
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

          // 连续失败检测
          const allFailed = toolResults.every(r => r.isError);
          if (allFailed) {
            consecutiveFailures++;
            totalFailures++;

            if (totalFailures >= TOTAL_FAIL_LIMIT) {
              finalText = assistantText || `工具调用累计失败 ${TOTAL_FAIL_LIMIT} 轮，判定为异常终止。`;
              this.normalizer.endStream(streamId);
              break;
            }

            if (consecutiveFailures >= CONSECUTIVE_FAIL_THRESHOLD) {
              consecutiveFailures = 0;
              history.push({
                role: 'user',
                content: `[System] 工具调用已连续失败 ${CONSECUTIVE_FAIL_THRESHOLD} 次，请换一种方式或策略继续处理任务。`,
              });
            }
          } else {
            consecutiveFailures = 0;
          }

          // 空转检测（仅在工具调用未全部失败时检测）
          if (!allFailed) {
            // 编排动作 = 委派任务 / 写入文件（编排者的核心职责）
            const ORCHESTRATION_ACTIONS = ['dispatch_task', 'send_worker_message', 'wait_for_workers', 'file_edit', 'file_create', 'file_insert', 'file_remove'];
            const hasAction = toolCalls.some(tc => ORCHESTRATION_ACTIONS.includes(tc.name));

            if (hasAction) {
              // 有编排动作 → 重置空转状态
              consecutiveStallRounds = 0;
              lastPrimaryToolName = '';
              consecutiveSameToolRounds = 0;
            } else {
              consecutiveStallRounds++;

              // 连续同工具重复检测
              const primaryTool = toolCalls[0]?.name || '';
              if (primaryTool === lastPrimaryToolName) {
                consecutiveSameToolRounds++;
              } else {
                lastPrimaryToolName = primaryTool;
                consecutiveSameToolRounds = 1;
              }

              // 连续同工具重复提示
              if (consecutiveSameToolRounds >= SAME_TOOL_FORCE) {
                logger.warn('Orchestrator 同工具重复调用达到强制阈值', { tool: primaryTool, rounds: consecutiveSameToolRounds }, LogCategory.LLM);
                forceNoToolsNextRound = true;
                history.push({
                  role: 'user',
                  content: `[System] 你已连续 ${consecutiveSameToolRounds} 轮调用 ${primaryTool} 工具，这是无效的重复行为。工具调用能力已被收回，请立即总结当前进展。`,
                });
              } else if (consecutiveSameToolRounds >= SAME_TOOL_WARN) {
                logger.warn('Orchestrator 同工具重复调用', { tool: primaryTool, rounds: consecutiveSameToolRounds }, LogCategory.LLM);
                history.push({
                  role: 'user',
                  content: `[System] 你已连续 ${consecutiveSameToolRounds} 轮调用 ${primaryTool} 工具。如果已获取到所需信息，请通过 dispatch_task 委派任务给 Worker 执行，或直接输出结论。不要反复调用同一工具。`,
                });
              }

              // 编排者空转检测（连续无编排动作）
              if (consecutiveStallRounds >= STALL_FORCE && !forceNoToolsNextRound) {
                logger.warn('Orchestrator 空转达到强制阈值', { rounds: consecutiveStallRounds }, LogCategory.LLM);
                forceNoToolsNextRound = true;
                history.push({
                  role: 'user',
                  content: `[System] 你已连续 ${consecutiveStallRounds} 轮仅使用查看/搜索类工具，未执行任何编排动作（dispatch_task）。工具调用能力已被收回，请立即总结当前进展并输出结论。`,
                });
              } else if (consecutiveStallRounds === STALL_WARN) {
                logger.warn('Orchestrator 空转警告', { rounds: consecutiveStallRounds }, LogCategory.LLM);
                history.push({
                  role: 'user',
                  content: `[System] 你已连续 ${consecutiveStallRounds} 轮仅使用查看/搜索类工具，未执行编排动作。请通过 dispatch_task 委派任务给 Worker 执行，或直接输出最终结论。不要继续查看文件。`,
                });
              }
            }
          }

          // 当轮 stream 结束，工具副作用（subTaskCard 等）已自然排在后面
          this.normalizer.endStream(streamId);
          round++;
        } catch (error: any) {
          this.normalizer.endStream(streamId, error?.message || 'Request failed');
          // abort 中断不视为异常，优雅退出循环
          if (error?.name === 'AbortError' || this.abortController?.signal.aborted) {
            break;
          }
          throw error;
        }
      }

      // abort 中断时不要求必须有内容
      if (!finalText.trim() && !this.abortController?.signal.aborted) {
        throw new Error('LLM 响应为空：流式传输完成但未收到有效内容');
      }
      return finalText || '任务已中断';
    } catch (error: any) {
      // abort 中断不视为错误
      if (error?.name === 'AbortError' || this.abortController?.signal.aborted) {
        return '任务已中断';
      }
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
      // 中断检查：工具调用之间检测 abort 信号，避免中断后继续执行后续工具
      if (this.abortController?.signal.aborted) {
        results.push({
          toolCallId: toolCall.id,
          content: '任务已中断',
          isError: true,
        });
        continue;
      }

      // 参数解析失败：不执行工具，直接回传给模型修正参数
      if (toolCall.argumentParseError) {
        const raw = typeof toolCall.rawArguments === 'string'
          ? toolCall.rawArguments.substring(0, 500)
          : '';
        const errorContent = `工具参数解析失败（${toolCall.name}）：${toolCall.argumentParseError}${raw ? `\n原始参数: ${raw}` : ''}`;
        results.push({
          toolCallId: toolCall.id,
          content: errorContent,
          isError: true,
        });
        this.emit('toolResult', toolCall.name, errorContent);
        continue;
      }

      // 编排者角色约束：禁止文件写入操作
      const blocked = this.checkOrchestratorToolRestriction(toolCall);
      if (blocked) {
        results.push({
          toolCallId: toolCall.id,
          content: blocked,
          isError: true,
        });
        continue;
      }

      try {
        const result = await this.toolManager.execute(
          toolCall,
          this.abortController?.signal,
          { workerId: 'orchestrator', role: 'orchestrator' },
        );
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

  /**
   * 编排者工具调用限制检查
   * 编排者可执行简单的单文件操作（改名、typo、改配置），
   * 但多文件/复杂修改应委派给 Worker。
   * 通过累计写入文件数追踪，超过阈值时拒绝并引导使用 dispatch_task。
   * 返回 null 表示允许，返回字符串表示拒绝原因。
   */
  private checkOrchestratorToolRestriction(toolCall: ToolCall): string | null {
    const { name, arguments: args } = toolCall;

    if (name === 'file_edit' || name === 'file_create' || name === 'file_insert') {
      const filePath = (args?.path || args?.file_path || '') as string;
      this.editedFiles.add(filePath);
      if (this.editedFiles.size > OrchestratorLLMAdapter.MAX_ORCHESTRATOR_EDIT_FILES) {
        return `编排者已修改 ${this.editedFiles.size} 个文件（超过 ${OrchestratorLLMAdapter.MAX_ORCHESTRATOR_EDIT_FILES} 个），多文件修改应通过 dispatch_task 委派给 Worker。`;
      }
    }

    if (name === 'file_remove') {
      const filePath = (args?.path || args?.file_path || '') as string;
      this.editedFiles.add(filePath);
      if (this.editedFiles.size > OrchestratorLLMAdapter.MAX_ORCHESTRATOR_EDIT_FILES) {
        return `编排者已修改 ${this.editedFiles.size} 个文件（超过 ${OrchestratorLLMAdapter.MAX_ORCHESTRATOR_EDIT_FILES} 个），多文件操作应通过 dispatch_task 委派给 Worker。`;
      }
    }

    return null;
  }
}
