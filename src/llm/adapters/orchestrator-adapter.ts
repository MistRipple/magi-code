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
import { BUILTIN_TOOL_NAMES } from '../../tools/types';
import { MessageHub } from '../../orchestrator/core/message-hub';
import { BaseLLMAdapter, AdapterState } from './base-adapter';
import { logger, LogCategory } from '../../logging';

/**
 * 历史管理配置
 */
export interface OrchestratorHistoryConfig {
  /** 最大历史消息数量（默认 40） */
  maxMessages?: number;
  /** 最大历史字符数（默认 100000） */
  maxChars?: number;
  /** 保留最近 N 轮对话（默认 6） */
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
  /** 深度任务模式：解除总轮次硬上限 */
  deepTask?: boolean;
}

export interface OrchestratorRuntimeState {
  reason:
    | 'completed'
    | 'failure_limit'
    | 'round_limit'
    | 'interrupted'
    | 'unknown';
  rounds: number;
}

/**
 * Orchestrator LLM 适配器
 */
export class OrchestratorLLMAdapter extends BaseLLMAdapter {
  /** 编排者单次会话中允许直接修改的最大文件数（常规模式） */
  private static readonly MAX_ORCHESTRATOR_EDIT_FILES = 3;
  /** 滚动摘要最大长度（字符） */
  private static readonly MAX_ROLLING_SUMMARY_CHARS = 2000;

  /**
   * 深度模式下编排者可用工具白名单（强制约束）
   *
   * 设计原则：编排者专职"分析、规划、监控、汇总"，所有代码变更必须通过 Worker 执行。
   * 白名单仅包含只读/分析/编排/任务管理类工具，写入类工具全部排除。
   * MCP 工具不在白名单中但单独放行（无法自动判断读写性，且多数为只读查询）。
   */
  private static readonly DEEP_MODE_ALLOWED_TOOLS = new Set([
    // 编排工具（核心）
    'dispatch_task',
    'send_worker_message',
    'wait_for_workers',
    // 只读分析工具（辅助规划）
    'file_view',
    'grep_search',
    'codebase_retrieval',
    'web_search',
    'web_fetch',
    'list-processes',
    'read-process',
    // 任务管理工具
    'get_todos',
    'update_todo',
  ]);

  private systemPrompt: string;
  private conversationHistory: LLMMessage[] = [];
  private abortController?: AbortController;
  private historyConfig: Required<OrchestratorHistoryConfig>;
  private rollingContextSummary: string | null = null;
  /** 深度任务模式：解除总轮次硬上限 */
  private readonly deepTask: boolean;

  /** 当前会话中编排者已修改的文件路径集合（用于规模限制） */
  private editedFiles = new Set<string>();

  /**
   * 临时配置（仅对下一次请求生效）
   */
  private tempSystemPrompt?: string;
  private tempEnableToolCalls?: boolean;
  private tempVisibility?: 'user' | 'system' | 'debug';
  private lastRuntimeState: OrchestratorRuntimeState = {
    reason: 'unknown',
    rounds: 0,
  };

  constructor(adapterConfig: OrchestratorAdapterConfig) {
    super(
      adapterConfig.client,
      adapterConfig.normalizer,
      adapterConfig.toolManager,
      adapterConfig.config,
      adapterConfig.messageHub  // 🔧 统一消息通道：使用 messageHub
    );
    this.systemPrompt = adapterConfig.systemPrompt ?? '';
    this.deepTask = adapterConfig.deepTask ?? false;
    this.historyConfig = {
      maxMessages: adapterConfig.historyConfig?.maxMessages ?? 40,
      maxChars: adapterConfig.historyConfig?.maxChars ?? 100000,
      preserveRecentRounds: adapterConfig.historyConfig?.preserveRecentRounds ?? 6,
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
    this.lastRuntimeState = {
      reason: 'unknown',
      rounds: 0,
    };

    try {
      if (enableToolCalls) {
        const content = await this.sendMessageWithTools(
          message,
          images,
          effectiveSystemPrompt,
          silent ? 'system' : undefined
        );
        this.setState(AdapterState.CONNECTED);
        this.lastRuntimeState = {
          reason: 'completed',
          rounds: 1,
        };
        return content;
      }

      let messagesToSend: LLMMessage[];
      if (silent) {
        // system 可见性调用仅用于内部决策，不污染编排对话历史
        messagesToSend = [this.buildUserMessage(message, images)];
      } else {
        // 准备消息历史（自动截断以控制 token 消耗）
        this.truncateHistoryIfNeeded();

        // 添加用户消息
        const userMessage = this.buildUserMessage(message, images);
        this.conversationHistory.push(userMessage);
        messagesToSend = this.conversationHistory;
      }

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

      // 用户可见请求才会写入编排历史，内部 system 请求不写入
      if (!silent) {
        this.conversationHistory.push({
          role: 'assistant',
          content: fullResponse,
        });
      }

      this.normalizer.endStream(streamId);
      this.setState(AdapterState.CONNECTED);
      this.lastRuntimeState = {
        reason: 'completed',
        rounds: 1,
      };

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
        this.lastRuntimeState = {
          reason: 'interrupted',
          rounds: 0,
        };
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
    this.rollingContextSummary = null;
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
   * 获取最近一次运行态
   */
  getLastRuntimeState(): OrchestratorRuntimeState {
    return { ...this.lastRuntimeState };
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

    // 钉住 index 0（用户原始请求）：截断从 index 1 开始，保留迭代锚点
    const pinnedCount = 1;
    const truncatedCount = currentLength - preserveCount - pinnedCount;
    if (truncatedCount > 0) {
      const droppedMessages = this.conversationHistory.splice(pinnedCount, truncatedCount);

      this.updateRollingSummary(droppedMessages);

      // rolling summary 注入到 index 1（钉住消息之后、保留消息之前）
      if (this.rollingContextSummary) {
        const bridgeMsg = this.conversationHistory[pinnedCount];
        if (bridgeMsg && bridgeMsg.role === 'user') {
          if (typeof bridgeMsg.content === 'string') {
            bridgeMsg.content = `${this.rollingContextSummary}\n\n---\n\n${bridgeMsg.content}`;
          } else if (Array.isArray(bridgeMsg.content)) {
            (bridgeMsg.content as any[]).unshift({ type: 'text', text: this.rollingContextSummary });
          }
        } else {
          this.conversationHistory.splice(pinnedCount, 0, {
            role: 'user',
            content: this.rollingContextSummary,
          });
        }
      }

      logger.debug('Orchestrator history truncated', {
        removedMessages: truncatedCount,
        remainingMessages: this.conversationHistory.length,
        previousChars: currentChars,
        currentChars: this.getHistoryChars(),
        hasRollingSummary: !!this.rollingContextSummary,
      }, LogCategory.LLM);
    }
  }

  private updateRollingSummary(droppedMessages: LLMMessage[]): void {
    const highlights: string[] = [];

    for (const message of droppedMessages) {
      const text = this.extractMessageText(message);
      if (!text) {
        continue;
      }

      if (message.role === 'user') {
        if (/(不要|不能|禁止|必须|务必|严禁|优先|确认)/.test(text)) {
          highlights.push(`- 用户约束: ${text.substring(0, 140)}`);
        }
        continue;
      }

      if (message.role === 'assistant') {
        highlights.push(`- 编排进展: ${text.substring(0, 180)}`);
      }
    }

    if (highlights.length === 0) {
      return;
    }

    const previousLines = (this.rollingContextSummary || '')
      .split('\n')
      .map(line => line.trim())
      .filter(line => line.startsWith('- '));
    const mergedLines = Array.from(new Set([...previousLines, ...highlights]));
    const mergedText = mergedLines.join('\n');
    const cropped = mergedText.length > OrchestratorLLMAdapter.MAX_ROLLING_SUMMARY_CHARS
      ? mergedText.substring(mergedText.length - OrchestratorLLMAdapter.MAX_ROLLING_SUMMARY_CHARS)
      : mergedText;

    this.rollingContextSummary = `[System 上下文回顾] 以下为之前轮次的关键上下文（自动精简）：\n${cropped}`;
  }

  private extractMessageText(message: LLMMessage): string {
    if (typeof message.content === 'string') {
      return message.content.trim().replace(/\s+/g, ' ');
    }

    if (!Array.isArray(message.content)) {
      return '';
    }

    const parts: string[] = [];
    for (const block of message.content as any[]) {
      if (!block || typeof block !== 'object') {
        continue;
      }
      if (block.type === 'text' && typeof block.text === 'string') {
        parts.push(block.text);
      } else if (block.type === 'tool_use' && typeof block.name === 'string') {
        parts.push(`调用工具 ${block.name}`);
      }
    }
    return parts.join(' ').trim().replace(/\s+/g, ' ');
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
    const isTransientSystemCall = visibility === 'system';

    // system 可见性调用使用临时历史，避免污染编排上下文
    const history = isTransientSystemCall
      ? [...this.conversationHistory]
      : this.conversationHistory;

    // 添加用户消息到历史
    history.push(this.buildUserMessage(message, images));

    const ORCHESTRATOR_HIDDEN_TOOLS = ['split_todo'];
    const allTools = await this.toolManager.getTools();
    const toolDefinitions = allTools
      .filter(tool => {
        if (ORCHESTRATOR_HIDDEN_TOOLS.includes(tool.name)) return false;
        if (!this.deepTask) return true;
        // 深度模式：非内置工具（MCP/Skill）放行，内置工具仅白名单可见
        if (tool.metadata?.source !== 'builtin') return true;
        return OrchestratorLLMAdapter.DEEP_MODE_ALLOWED_TOOLS.has(tool.name);
      })
      .map(tool => ({
        name: tool.name,
        description: tool.description,
        input_schema: tool.input_schema,
      }));

    // 每轮 LLM 调用独立一个 stream，确保时间轴正确：
    // 当轮 stream 内包含 thinking + text + tool_call + tool_result，
    // endStream 后工具副作用产生的新消息（如 subTaskCard）自然排在后面，
    // 下一轮 stream 再开启新卡片，时间顺序天然正确。
    // 异常终止依赖失败检测止损：连续 5 次失败 → 提示换方式，累计 25 轮失败 → 终止
    const CONSECUTIVE_FAIL_THRESHOLD = 5;
    const TOTAL_FAIL_LIMIT = 25;
    // 总轮次安全网：deepTask 模式下解除硬上限，仅保留空转/失败等智能安全网
    const MAX_ORCHESTRATOR_ROUNDS = this.deepTask ? Infinity : 50;
    const WARN_BEFORE_LIMIT = 5;

    try {
      let finalText = '';
      let consecutiveFailures = 0;
      let totalFailures = 0;
      let loopRounds = 0;
      let terminationReason: Exclude<OrchestratorRuntimeState['reason'], 'unknown'> = 'completed';

      // 创建 AbortController，供 interrupt() 中断 LLM 请求
      this.abortController = new AbortController();

      let round = 0;
      while (true) {
        // 中断检查：每轮迭代入口检测 abort 信号
        if (this.abortController.signal.aborted) {
          terminationReason = 'interrupted';
          break;
        }
        loopRounds++;

        // 总轮次安全网：防止编排者陷入"dispatch → wait → 不满意 → dispatch"无限循环
        if (loopRounds > MAX_ORCHESTRATOR_ROUNDS) {
          logger.warn('Orchestrator 达到总轮次上限', { loopRounds }, LogCategory.LLM);
          finalText = finalText || `编排已执行 ${loopRounds} 轮，达到安全上限，任务终止。请检查已完成的工作。`;
          terminationReason = 'round_limit';
          break;
        }
        if (loopRounds === MAX_ORCHESTRATOR_ROUNDS - WARN_BEFORE_LIMIT) {
          history.push({
            role: 'user',
            content: `[System] 你已执行 ${loopRounds} 轮编排循环，即将达到上限（${MAX_ORCHESTRATOR_ROUNDS} 轮）。请立即完成当前阶段的工作，输出最终汇总结果。`,
          });
        }

        // 长任务 history 裁剪：每轮 LLM 调用前检查并截断，防止 context window 溢出
        if (!isTransientSystemCall) {
          this.truncateHistoryIfNeeded();
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
          tools: toolDefinitions.length > 0 ? toolDefinitions : undefined,
          stream: true,
          maxTokens: 8192,
          temperature: 0.3,
          signal: this.abortController.signal,
        };

        let accumulatedText = '';
        let hasStreamedTextDelta = false;
        let toolCalls: ToolCall[] = [];

        try {
          const response = await this.client.streamMessage(params, (chunk) => {
            if (chunk.type === 'content_delta' && chunk.content) {
              this.normalizer.processTextDelta(streamId, chunk.content);
              hasStreamedTextDelta = true;
              accumulatedText += chunk.content;
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
          if (assistantText && !hasStreamedTextDelta) {
            // 兜底：部分 provider 可能仅在最终响应体返回文本，未逐块回调 content_delta。
            this.normalizer.processTextDelta(streamId, assistantText);
          }

          // 无工具调用 → 收敛
          if (toolCalls.length === 0) {
            if (assistantText && !hasStreamedTextDelta) {
              this.emit('message', assistantText);
            }
            history.push({ role: 'assistant', content: assistantText });
            finalText = assistantText;
            this.normalizer.endStream(streamId);
            terminationReason = 'completed';
            break;
          }

          // 有工具调用 → 只对无需授权的工具即时渲染卡片
          // 需要授权的高风险工具延后到授权完成后再渲染，避免“先出现 edit 卡片后弹授权”。
          const preAnnouncedToolCallIds = new Set<string>();
          for (const toolCall of toolCalls) {
            if (this.toolManager.requiresUserAuthorization(toolCall.name)) {
              continue;
            }
            preAnnouncedToolCallIds.add(toolCall.id);
            this.normalizer.addToolCall(streamId, {
              type: 'tool_call',
              toolName: toolCall.name,
              toolId: toolCall.id,
              status: 'running',
              input: JSON.stringify(toolCall.arguments, null, 2),
            });
          }

          const assistantContent: any[] = [];
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
            terminationReason = 'interrupted';
            break;
          }

          const toolCallMap = new Map(toolCalls.map((toolCall) => [toolCall.id, toolCall] as const));
          for (const result of toolResults) {
            const toolCall = toolCallMap.get(result.toolCallId);
            if (!toolCall) {
              continue;
            }
            if (preAnnouncedToolCallIds.has(result.toolCallId)) {
              this.normalizer.finishToolCall(
                streamId,
                result.toolCallId,
                result.isError ? undefined : result.content,
                result.isError ? result.content : undefined,
                result.fileChange,
                result.standardized
              );
              continue;
            }

            // 高风险工具：单独产出一张工具卡片，确保其时序晚于授权卡片
            const deferredToolStreamId = this.normalizer.startStream(this.currentTraceId!);
            this.normalizer.addToolCall(deferredToolStreamId, {
              type: 'tool_call',
              toolName: toolCall.name,
              toolId: toolCall.id,
              status: result.isError ? 'failed' : 'completed',
              input: JSON.stringify(toolCall.arguments, null, 2),
              output: result.isError ? undefined : result.content,
              error: result.isError ? result.content : undefined,
              standardized: result.standardized,
            });

            this.normalizer.finishToolCall(
              deferredToolStreamId,
              toolCall.id,
              result.isError ? undefined : result.content,
              result.isError ? result.content : undefined,
              result.fileChange,
              result.standardized
            );
            this.normalizer.endStream(deferredToolStreamId);
          }

          history.push({
            role: 'user',
            content: toolResults.map((result) => ({
              type: 'tool_result',
              tool_use_id: result.toolCallId,
              content: result.content,
              is_error: result.isError,
              standardized: result.standardized,
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
              terminationReason = 'failure_limit';
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

          // 当轮 stream 结束，工具副作用（subTaskCard 等）已自然排在后面
          this.normalizer.endStream(streamId);
          round++;
        } catch (error: any) {
          this.normalizer.endStream(streamId, error?.message || 'Request failed');
          // abort 中断不视为异常，优雅退出循环
          if (error?.name === 'AbortError' || this.abortController?.signal.aborted) {
            terminationReason = 'interrupted';
            break;
          }
          throw error;
        }
      }

      // abort 中断时不要求必须有内容
      if (!finalText.trim() && !this.abortController?.signal.aborted) {
        throw new Error('LLM 响应为空：流式传输完成但未收到有效内容');
      }
      this.lastRuntimeState = {
        reason: terminationReason,
        rounds: loopRounds,
      };
      return finalText || '任务已中断';
    } catch (error: any) {
      // abort 中断不视为错误
      if (error?.name === 'AbortError' || this.abortController?.signal.aborted) {
        this.lastRuntimeState = {
          reason: 'interrupted',
          rounds: 0,
        };
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
    const toolSourceMap = await this.buildToolSourceMap();

    for (const toolCall of toolCalls) {
      // 中断检查：工具调用之间检测 abort 信号，避免中断后继续执行后续工具
      if (this.abortController?.signal.aborted) {
        results.push(this.createSyntheticToolResult(
          toolCall,
          '任务已中断',
          'aborted',
          toolSourceMap,
        ));
        continue;
      }

      // 参数解析失败：不执行工具，直接回传给模型修正参数
      if (toolCall.argumentParseError) {
        const raw = typeof toolCall.rawArguments === 'string'
          ? toolCall.rawArguments.substring(0, 500)
          : '';
        const errorContent = `工具参数解析失败（${toolCall.name}）：${toolCall.argumentParseError}${raw ? `\n原始参数: ${raw}` : ''}`;
        results.push(this.createSyntheticToolResult(
          toolCall,
          errorContent,
          'error',
          toolSourceMap,
        ));
        this.emit('toolResult', toolCall.name, errorContent);
        continue;
      }

      // 编排者角色约束：禁止文件写入操作
      const blocked = this.checkOrchestratorToolRestriction(toolCall);
      if (blocked) {
        results.push(this.createSyntheticToolResult(
          toolCall,
          blocked,
          'blocked',
          toolSourceMap,
        ));
        continue;
      }

      try {
        const rawResult = await this.toolManager.execute(
          toolCall,
          this.abortController?.signal,
          { workerId: 'orchestrator', role: 'orchestrator' },
        );
        if (typeof rawResult.content === 'string' && rawResult.content.length > maxToolResultChars) {
          const truncated = rawResult.content.slice(0, maxToolResultChars);
          rawResult.content = `${truncated}\n...[truncated ${rawResult.content.length - maxToolResultChars} chars]`;
        }
        const result = this.ensureStandardizedToolResult(toolCall, rawResult, toolSourceMap);
        results.push(result);
        this.emit('toolResult', toolCall.name, result.content);
      } catch (error: any) {
        results.push(this.createSyntheticToolResult(
          toolCall,
          `Error: ${error?.message || String(error)}`,
          'error',
          toolSourceMap,
        ));
      }
    }

    return results;
  }

  /**
   * 编排者工具调用限制检查（第二道防线）
   *
   * 深度模式：内置工具必须在白名单内，否则拒绝（用 BUILTIN_TOOL_NAMES 明确判断来源）。
   * 常规模式：文件写入超限时拒绝并引导 dispatch_task。
   *
   * 返回 null 表示允许，返回字符串表示拒绝原因。
   */
  private checkOrchestratorToolRestriction(toolCall: ToolCall): string | null {
    const { name, arguments: args } = toolCall;

    // 深度模式兜底：内置工具必须在白名单内（MCP/Skill 不在 BUILTIN_TOOL_NAMES 中，自动放行）
    if (this.deepTask
      && (BUILTIN_TOOL_NAMES as readonly string[]).includes(name)
      && !OrchestratorLLMAdapter.DEEP_MODE_ALLOWED_TOOLS.has(name)) {
      return `深度模式下编排者不可直接执行 ${name}，请通过 dispatch_task 委派给 Worker。`;
    }

    // 常规模式：文件写入数量限制
    if (name === 'file_edit' || name === 'file_create' || name === 'file_insert' || name === 'file_remove') {
      const filePath = (args?.path || args?.file_path || '') as string;
      this.editedFiles.add(filePath);
      if (this.editedFiles.size > OrchestratorLLMAdapter.MAX_ORCHESTRATOR_EDIT_FILES) {
        return `编排者已修改 ${this.editedFiles.size} 个文件（超过 ${OrchestratorLLMAdapter.MAX_ORCHESTRATOR_EDIT_FILES} 个），请通过 dispatch_task 委派给 Worker。`;
      }
    }

    return null;
  }
}
