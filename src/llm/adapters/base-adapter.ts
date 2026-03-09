/**
 * LLM 适配器抽象基类
 *
 * 🔧 统一消息通道（unified-message-channel-design.md v2.5）
 *
 * 消息流架构（3层）：
 * Layer 1: Normalizer.emit('message')
 * Layer 2: Adapter.setupNormalizerEvents() → messageHub.sendMessage() [直接调用]
 * Layer 3: MessageHub → emit('standard:message') → WebviewProvider.postMessage()
 */

import { EventEmitter } from 'events';
import { AgentType, AgentRole, LLMConfig, TokenUsage } from '../../types/agent-types';
import { LLMClient, ToolCall, ToolResult, StandardizedToolResult } from '../types';
import { BaseNormalizer } from '../../normalizer/base-normalizer';
import { ToolManager, type ToolExecutionContext } from '../../tools/tool-manager';
import { MessageHub } from '../../orchestrator/core/message-hub';
import { logger, LogCategory } from '../../logging';
import { MESSAGE_EVENTS, ADAPTER_EVENTS } from '../../protocol/event-names';

/**
 * 适配器状态
 */
export enum AdapterState {
  DISCONNECTED = 'disconnected',
  CONNECTING = 'connecting',
  CONNECTED = 'connected',
  BUSY = 'busy',
  ERROR = 'error',
}

/**
 * 适配器事件
 */
export interface AdapterEvents {
  stateChange: (state: AdapterState) => void;
  error: (error: Error) => void;
  message: (content: string) => void;
  toolCall: (toolName: string, args: any) => void;
  toolResult: (toolName: string, result: string) => void;
  thinking: (content: string) => void;
}

/**
 * LLM 适配器基类
 *
 * 🔧 统一消息通道：持有 MessageHub 引用，消息通过 MessageHub 发送到前端。
 * 不再通过 UnifiedMessageBus 事件转发链，减少层级，提高效率。
 */
export abstract class BaseLLMAdapter extends EventEmitter {
  protected state: AdapterState = AdapterState.DISCONNECTED;
  protected client: LLMClient;
  protected normalizer: BaseNormalizer;
  protected toolManager: ToolManager;
  protected config: LLMConfig;
  protected currentTraceId?: string;
  protected lastTokenUsage: TokenUsage = { inputTokens: 0, outputTokens: 0 };
  protected totalTokenUsage: TokenUsage = { inputTokens: 0, outputTokens: 0 };

  /**
   * 消息出口 - 直接发送消息到前端
   * 🔧 统一消息通道：替代 UnifiedMessageBus，成为唯一出口
   */
  protected messageHub: MessageHub;

  protected decisionHook?: (event: {
    type: 'thinking' | 'tool_call' | 'tool_result';
    toolName?: string;
    toolArgs?: any;
    toolResult?: string;
  }) => string[];

  protected async buildToolSourceMap(): Promise<Map<string, StandardizedToolResult['source']>> {
    const sourceMap = new Map<string, StandardizedToolResult['source']>();
    try {
      const tools = await this.toolManager.getTools();
      for (const tool of tools) {
        if (!tool?.name || !tool?.metadata?.source) {
          continue;
        }
        sourceMap.set(tool.name, tool.metadata.source);
      }
    } catch (error: any) {
      logger.warn('构建工具来源映射失败，将在单链路内使用 builtin 兜底来源', {
        agent: this.agent,
        error: error?.message || String(error),
      }, LogCategory.LLM);
    }
    return sourceMap;
  }

  protected resolveToolSource(
    toolName: string,
    sourceMap?: Map<string, StandardizedToolResult['source']>,
    existing?: StandardizedToolResult['source'],
  ): StandardizedToolResult['source'] {
    if (existing === 'builtin' || existing === 'mcp' || existing === 'skill') {
      return existing;
    }
    const mapped = sourceMap?.get(toolName);
    if (mapped) {
      return mapped;
    }
    // 单链路兜底：来源映射不可用时统一归类 builtin，避免分叉逻辑。
    logger.warn('工具来源未命中映射，使用 builtin 兜底', {
      agent: this.agent,
      toolName,
    }, LogCategory.LLM);
    return 'builtin';
  }

  protected createSyntheticToolResult(
    toolCall: ToolCall,
    content: string,
    status: StandardizedToolResult['status'],
    sourceMap?: Map<string, StandardizedToolResult['source']>
  ): ToolResult {
    const normalizedContent = typeof content === 'string' ? content : String(content ?? '');
    const source = this.resolveToolSource(toolCall.name, sourceMap);
    const isError = status !== 'success';
    return {
      toolCallId: toolCall.id,
      content: normalizedContent,
      isError,
      standardized: {
        schemaVersion: 'tool-result.v1',
        source,
        toolName: toolCall.name,
        toolCallId: toolCall.id,
        status,
        message: normalizedContent,
      },
    };
  }

  protected ensureStandardizedToolResult(
    toolCall: ToolCall,
    result: ToolResult,
    sourceMap?: Map<string, StandardizedToolResult['source']>,
  ): ToolResult {
    const toolCallId = result.toolCallId || toolCall.id;
    const content = typeof result.content === 'string' ? result.content : String(result.content ?? '');
    const existing = result.standardized;

    if (existing) {
      const status = existing.status;
      return {
        ...result,
        toolCallId,
        content,
        isError: status !== 'success',
        standardized: {
          ...existing,
          source: this.resolveToolSource(toolCall.name, sourceMap, existing.source),
          toolName: existing.toolName || toolCall.name,
          toolCallId,
          message: content,
        },
      };
    }

    const status: StandardizedToolResult['status'] = result.isError ? 'error' : 'success';
    logger.warn('工具结果缺少 standardized，已在适配器层补齐', {
      agent: this.agent,
      toolName: toolCall.name,
      status,
    }, LogCategory.LLM);
    return {
      ...result,
      toolCallId,
      content,
      isError: status !== 'success',
      standardized: {
        schemaVersion: 'tool-result.v1',
        source: this.resolveToolSource(toolCall.name, sourceMap),
        toolName: toolCall.name,
        toolCallId,
        status,
        message: content,
      },
    };
  }

  protected parseToolResultJson(result: ToolResult): Record<string, unknown> | undefined {
    const content = typeof result.content === 'string' ? result.content : String(result.content ?? '');
    if (!content.trim()) {
      return undefined;
    }

    // 使用提取逻辑以防首尾有额外文本干扰
    const trimmed = content.trim();
    let jsonText = trimmed;

    if (trimmed[0] === '{' || trimmed[0] === '[') {
      let depth = 0;
      let inString = false;
      let escaping = false;
      const openChar = trimmed[0];
      const closeChar = openChar === '{' ? '}' : ']';

      for (let i = 0; i < trimmed.length; i += 1) {
        const ch = trimmed[i];
        if (inString) {
          if (escaping) { escaping = false; continue; }
          if (ch === '\\') { escaping = true; continue; }
          if (ch === '"') { inString = false; }
          continue;
        }
        if (ch === '"') { inString = true; continue; }
        if (ch === openChar) { depth += 1; continue; }
        if (ch === closeChar) {
          depth -= 1;
          if (depth === 0) {
            jsonText = trimmed.slice(0, i + 1);
            break;
          }
        }
      }
    }

    try {
      const parsed = JSON.parse(jsonText);
      if (!parsed || typeof parsed !== 'object') {
        return undefined;
      }
      return parsed as Record<string, unknown>;
    } catch {
      return undefined;
    }
  }

  protected isTerminalProcessTool(toolName: string): boolean {
    return toolName === 'shell'
      || toolName === 'launch-process'
      || toolName === 'read-process'
      || toolName === 'write-process'
      || toolName === 'kill-process'
      || toolName === 'list-processes';
  }

  protected isHardToolFailure(result: { isError?: boolean; standardized?: { status?: string } }): boolean {
    const status = result.standardized?.status;
    if (status === 'success') {
      return false;
    }
    if (status === 'error' || status === 'timeout' || status === 'killed') {
      return true;
    }
    return Boolean(result.isError);
  }

  protected truncateToolResultContent(toolCall: ToolCall, rawResult: ToolResult, maxChars: number): void {
    if (typeof rawResult.content !== 'string' || rawResult.content.length <= maxChars) {
      return;
    }

    // 终端工具结果要求保持 JSON 可解析，且已通过增量预览持续输出，不在此处做破坏性截断
    if (this.isTerminalProcessTool(toolCall.name)) {
      return;
    }

    const truncated = rawResult.content.slice(0, maxChars);
    rawResult.content = `${truncated}\n...[truncated ${rawResult.content.length - maxChars} chars]`;
  }

  protected async autoPreviewProcessOutput(
    streamId: string,
    toolCall: ToolCall,
    result: ToolResult,
    executionContext: ToolExecutionContext,
    signal?: AbortSignal,
  ): Promise<void> {
    if (result.isError || (toolCall.name !== 'launch-process' && toolCall.name !== 'shell')) {
      return;
    }

    const launchResult = this.parseToolResultJson(result);
    const terminalId = typeof launchResult?.terminal_id === 'number' ? launchResult.terminal_id : undefined;
    const status = typeof launchResult?.status === 'string' ? launchResult.status : '';
    const runMode = launchResult?.run_mode;
    const nextCursor = typeof launchResult?.output_cursor === 'number' ? launchResult.output_cursor : 0;
    const shouldPoll = Boolean(
      terminalId
      && (runMode === 'service' || runMode === 'task')
      && status !== 'completed'
      && status !== 'failed'
      && status !== 'killed'
      && status !== 'timeout'
    );
    if (!shouldPoll || !terminalId) {
      return;
    }

    try {
      let cursor = nextCursor;
      let noProgressRounds = 0;
      const isTaskMode = runMode === 'task';
      const maxRounds = isTaskMode ? 600 : 6;
      // 放大 task 模式的无进展退出阈值，允许 shell 有足够的沉默/睡眠时间 (约 300 轮 * 0.5s = 150s)
      const maxTaskNoProgressRounds = 300;
      const taskPreviewStartAt = Date.now();
      const maxTaskPreviewMs = 600_000; // 最大 10 分钟预览
      // 跟踪最后一轮 read-process 的完整 JSON 结果，用于在终态时替换 launch 快返内容
      let lastReadContent: string | undefined;

      for (let round = 0; round < maxRounds; round += 1) {
        if (signal?.aborted) {
          return;
        }

        const autoRead = await this.toolManager.executeInternalTool({
          id: `${toolCall.id}::auto-read::${round}`,
          name: 'read-process',
          arguments: {
            terminal_id: terminalId,
            // 统一使用阻塞等待拉取增量，提升实时性并降低轮询频率
            wait: true,
            max_wait_seconds: isTaskMode ? 2 : 1,
            from_cursor: cursor,
          },
        }, signal);

        const parsedRead = this.parseToolResultJson(autoRead);
        const readStatus = typeof parsedRead?.status === 'string' ? parsedRead.status : '';
        const readOutput = typeof parsedRead?.output === 'string' ? parsedRead.output : '';
        const readNextCursor = typeof parsedRead?.next_cursor === 'number'
          ? parsedRead.next_cursor
          : (typeof parsedRead?.output_cursor === 'number' ? parsedRead.output_cursor : cursor);
        const cursorAdvanced = readNextCursor > cursor;
        const hasProgress = cursorAdvanced || readOutput.trim().length > 0;

        // 记录完整 read 结果用于终态替换
        if (typeof autoRead.content === 'string' && autoRead.content.trim()) {
          lastReadContent = autoRead.content;
        }

        const autoReadContent = typeof autoRead.content === 'string' ? autoRead.content : String(autoRead.content ?? '');
        if (autoReadContent.trim()) {
          this.normalizer.addToolCall(streamId, {
            type: 'tool_call',
            toolName: toolCall.name,
            toolId: result.toolCallId,
            status: 'running',
            input: JSON.stringify(toolCall.arguments, null, 2),
            output: autoReadContent,
          });
        }

        cursor = readNextCursor;

        if (hasProgress) {
          noProgressRounds = 0;
        } else {
          noProgressRounds += 1;
        }

        const isTerminal = readStatus === 'completed'
          || readStatus === 'failed'
          || readStatus === 'killed'
          || readStatus === 'timeout';

        if (isTerminal) {
          // task 终态：用最后一轮完整 read 结果替换 launch 快返内容，
          // 让模型看到最终状态（completed/failed），避免不必要的额外 read-process 调用。
          if (isTaskMode && lastReadContent) {
            result.content = lastReadContent;
            result.isError = readStatus === 'failed' || readStatus === 'killed' || readStatus === 'timeout';
          }
          break;
        }

        if (isTaskMode) {
          // task：无进展超过阈值时让控制权回到模型，避免单个工具长时间阻塞
          if (noProgressRounds >= maxTaskNoProgressRounds) {
            break;
          }
          // task：即便持续有输出，也限制自动预览总时长，避免占满编排预算
          if (Date.now() - taskPreviewStartAt >= maxTaskPreviewMs) {
            break;
          }
          // 轮询间隔：有进展时快速刷新，无进展时退避等待
          await new Promise(r => setTimeout(r, hasProgress ? 200 : 500));
        } else {
          // service：短轮询预览，避免长时间占用链路
          if (noProgressRounds >= 2) {
            break;
          }
          await new Promise(r => setTimeout(r, 1000));
        }
      }
    } catch (error: any) {
      logger.warn('process 自动读流预览失败，忽略本次刷新', {
        agent: this.agent,
        toolCallId: result.toolCallId,
        terminalId,
        error: error?.message || String(error),
      }, LogCategory.TOOLS);
    }
  }

  constructor(
    client: LLMClient,
    normalizer: BaseNormalizer,
    toolManager: ToolManager,
    config: LLMConfig,
    messageHub: MessageHub
  ) {
    super();
    this.client = client;
    this.normalizer = normalizer;
    this.toolManager = toolManager;
    this.config = config;
    this.messageHub = messageHub;

    // 设置 Normalizer 事件处理，直接发送到 MessageHub
    this.setupNormalizerEvents();
  }

  /**
   * 设置决策点回调
   */
  setDecisionHook(hook?: (event: {
    type: 'thinking' | 'tool_call' | 'tool_result';
    toolName?: string;
    toolArgs?: any;
    toolResult?: string;
  }) => string[]): void {
    this.decisionHook = hook;
  }

  /**
   * 使用当前请求上下文启动流式消息
   * 优先复用占位消息 ID，确保 UI 端流式更新命中同一条消息
   */
  protected startStreamWithContext(visibility?: 'user' | 'system' | 'debug'): string {
    const traceId = this.syncTraceFromMessageHub();
    const requestId = this.messageHub.getRequestContext();
    const boundMessageId = requestId ? this.messageHub.getRequestMessageId(requestId) : undefined;

    return this.normalizer.startStream(traceId, undefined, boundMessageId, visibility);
  }

  /**
   * 将适配器当前 trace 与 MessageHub 保持一致。
   * MessageHub trace 已与会话绑定，消息 trace 必须沿用它，避免跨会话误过滤。
   */
  protected syncTraceFromMessageHub(): string {
    const hubTraceId = this.messageHub.getTraceId();
    if (typeof hubTraceId === 'string' && hubTraceId.trim()) {
      this.currentTraceId = hubTraceId.trim();
      return this.currentTraceId;
    }

    if (!this.currentTraceId) {
      this.currentTraceId = this.generateTraceId();
    }
    return this.currentTraceId;
  }

  // 节流定时器
  private tokenUpdateTimer: NodeJS.Timeout | null = null;
  // 待发送的 Token 更新
  private pendingTokenUpdate = false;

  /**
   * 发送实时 Token 更新（节流 1000ms）
   */
  private sendRealtimeTokenUpdate(): void {
    if (this.tokenUpdateTimer) {
      this.pendingTokenUpdate = true;
      return;
    }

    this.emitTokenUpdate();

    this.tokenUpdateTimer = setTimeout(() => {
      this.tokenUpdateTimer = null;
      if (this.pendingTokenUpdate) {
        this.pendingTokenUpdate = false;
        this.emitTokenUpdate();
      }
    }, 1000);
  }

  private emitTokenUpdate(): void {
    if (!this.messageHub) return;
    
    this.messageHub.data('executionTokenRuntime', {
      worker: this.agent,
      provider: this.config.provider,
      model: this.config.model,
      usage: {
        inputTokens: this.totalTokenUsage.inputTokens,
        outputTokens: this.totalTokenUsage.outputTokens,
        cacheReadTokens: this.totalTokenUsage.cacheReadTokens,
        cacheWriteTokens: this.totalTokenUsage.cacheWriteTokens,
      }
    });
  }

  /**
   * 设置 Normalizer 事件处理
   *
   * 🔧 统一消息通道：消息直接发送到 MessageHub（Layer 2 → Layer 3）：
   * - 消息无条件发送，不再有静默丢弃逻辑
   * - 跳过 AdapterFactory 和 WebviewProvider 的中间转发层
   * - 错误事件仍通过 EventEmitter 传递（需要特殊处理）
   */
  private setupNormalizerEvents(): void {
    // 消息开始/流式：直接发送到 MessageHub
    this.normalizer.on(MESSAGE_EVENTS.MESSAGE, (message) => {
      this.messageHub.sendMessage(message);
    });

    // 消息完成：直接发送到 MessageHub
    this.normalizer.on(MESSAGE_EVENTS.COMPLETE, (_messageId, message) => {
      this.messageHub.sendMessage(message);
    });

    // 流式更新：直接发送到 MessageHub
    this.normalizer.on(MESSAGE_EVENTS.UPDATE, (update) => {
      // 实时 Token 统计
      if (update.tokenUsage) {
        this.recordTokenUsage(update.tokenUsage);
        this.sendRealtimeTokenUpdate();
      }

      this.messageHub.sendUpdate(update);
    });

    // 错误事件：通过 EventEmitter 传递（需要特殊处理）
    this.normalizer.on(MESSAGE_EVENTS.ERROR, (error) => {
      this.emit(ADAPTER_EVENTS.NORMALIZER_ERROR, error);
    });
  }

  /**
   * 获取代理类型
   */
  abstract get agent(): AgentType;

  /**
   * 获取代理角色
   */
  abstract get role(): AgentRole;

  /**
   * 连接到 LLM
   *
   * 直接标记为已连接状态，不再发送测试请求。
   * 如果配置有误（API key 错误等），sendMessage 时会抛出错误并返回给用户。
   * 这样避免了第一条消息发送两次 LLM 请求的性能问题。
   */
  async connect(): Promise<void> {
    if (this.state === AdapterState.CONNECTED) {
      return;
    }

    // 直接标记为已连接，跳过 testConnection 调用
    // 原因：testConnection 会发送一个 "test" 消息到 LLM API，
    // 这导致第一条用户消息需要等待两次 LLM 往返，延迟翻倍
    this.setState(AdapterState.CONNECTED);
    logger.info(`${this.agent} adapter connected`, undefined, LogCategory.LLM);
  }

  /**
   * 断开连接
   */
  async disconnect(): Promise<void> {
    if (this.state === AdapterState.DISCONNECTED) {
      return;
    }

    this.setState(AdapterState.DISCONNECTED);
    logger.info(`${this.agent} adapter disconnected`, undefined, LogCategory.LLM);
  }

  /**
   * 发送消息
   */
  abstract sendMessage(message: string, images?: string[]): Promise<string>;

  /**
   * 中断当前请求
   */
  abstract interrupt(): Promise<void>;

  /**
   * 获取连接状态
   */
  get isConnected(): boolean {
    return this.state === AdapterState.CONNECTED || this.state === AdapterState.BUSY;
  }

  /**
   * 获取忙碌状态
   */
  get isBusy(): boolean {
    return this.state === AdapterState.BUSY;
  }

  /**
   * 获取最近一次请求的 Token 使用
   */
  getLastTokenUsage(): TokenUsage {
    return { ...this.lastTokenUsage };
  }

  /**
   * 获取累计 Token 使用
   */
  getTotalTokenUsage(): TokenUsage {
    return { ...this.totalTokenUsage };
  }

  /**
   * 重置 Token 统计（用于“重置统计”全链路）
   */
  resetTokenUsage(): void {
    this.lastTokenUsage = { inputTokens: 0, outputTokens: 0 };
    this.totalTokenUsage = { inputTokens: 0, outputTokens: 0 };

    if (this.tokenUpdateTimer) {
      clearTimeout(this.tokenUpdateTimer);
      this.tokenUpdateTimer = null;
    }
    this.pendingTokenUpdate = false;

    this.emitTokenUpdate();
  }

  /**
   * 记录 Token 使用
   */
  protected recordTokenUsage(usage?: Partial<TokenUsage>): void {
    if (!usage) return;
    const inputTokens = usage.inputTokens || 0;
    const outputTokens = usage.outputTokens || 0;
    const cacheReadTokens = usage.cacheReadTokens || 0;
    const cacheWriteTokens = usage.cacheWriteTokens || 0;

    // 跳过全零的 usage（无意义更新）
    if (
      inputTokens === 0
      && outputTokens === 0
      && cacheReadTokens === 0
      && cacheWriteTokens === 0
    ) {
      return;
    }

    this.lastTokenUsage = {
      inputTokens,
      outputTokens,
      cacheReadTokens: cacheReadTokens || undefined,
      cacheWriteTokens: cacheWriteTokens || undefined,
    };

    this.totalTokenUsage.inputTokens += inputTokens;
    this.totalTokenUsage.outputTokens += outputTokens;
    if (cacheReadTokens) {
      this.totalTokenUsage.cacheReadTokens =
        (this.totalTokenUsage.cacheReadTokens || 0) + cacheReadTokens;
    }
    if (cacheWriteTokens) {
      this.totalTokenUsage.cacheWriteTokens =
        (this.totalTokenUsage.cacheWriteTokens || 0) + cacheWriteTokens;
    }

    // 主动推送实时 Token 更新到前端
    this.sendRealtimeTokenUpdate();
  }

  /**
   * 获取当前状态
   */
  getState(): AdapterState {
    return this.state;
  }

  /**
   * 设置状态
   */
  protected setState(state: AdapterState): void {
    if (this.state !== state) {
      this.state = state;
      this.emit('stateChange', state);
    }
  }

  /**
   * 发出错误事件
   */
  protected emitError(error: Error): void {
    this.emit('error', error);
    logger.error(`${this.agent} adapter error`, { error: error.message }, LogCategory.LLM);
  }

  /**
   * 生成 trace ID
   */
  protected generateTraceId(): string {
    return `trace-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
  }

  /**
   * 销毁适配器（清理资源）
   */
  dispose(): void {
    // 清理定时器
    if (this.tokenUpdateTimer) {
      clearTimeout(this.tokenUpdateTimer);
      this.tokenUpdateTimer = null;
    }
    this.pendingTokenUpdate = false;

    // 移除所有事件监听器
    this.removeAllListeners();

    logger.debug(`${this.agent} adapter disposed`, undefined, LogCategory.LLM);
  }
}
