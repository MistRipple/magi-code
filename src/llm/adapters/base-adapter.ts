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
import {
  LLMClient,
  LLMMessageParams,
  LLMRetryRuntimeEvent,
  ToolCall,
  ToolResult,
  StandardizedToolResult,
  DecisionHookEvent,
} from '../types';
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

interface ToolResultRenderParams {
  streamId: string;
  toolCalls: ToolCall[];
  toolResults: ToolResult[];
  preAnnouncedToolCallIds: Set<string>;
  executionContext: ToolExecutionContext;
  signal?: AbortSignal;
}

interface TerminalPreviewEntry {
  streamId: string;
  ownStream: boolean;
  result: ToolResult;
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

  /**
   * 当前请求的显式标识（实例级，非全局）。
   * 由 AdapterFactory 在每次 sendMessage 前通过 setCurrentRequestId 注入，
   * 用于将流式输出绑定到 UI 占位消息。取代已废弃的全局 requestContext。
   */
  protected currentRequestId: string | undefined;
  /**
   * 当前请求的工具执行上下文覆盖（实例级，非全局）。
   * ⚠️ 工程约束：新增写工具或执行入口必须复用该链路透传 worktreePath，禁止旁路。
   */
  protected currentToolExecutionContext: Partial<ToolExecutionContext> | undefined;

  protected decisionHook?: (event: DecisionHookEvent) => string[];

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
      || toolName === 'process_launch'
      || toolName === 'process_read'
      || toolName === 'process_write'
      || toolName === 'process_kill'
      || toolName === 'process_list';
  }

  protected isHardToolFailure(result: { isError?: boolean; standardized?: { status?: string } }): boolean {
    const status = result.standardized?.status;
    if (status === 'success') {
      return false;
    }
    if (status === 'error' || status === 'timeout' || status === 'killed') {
      return true;
    }
    if (status === 'blocked' || status === 'rejected') {
      return true;
    }
    return Boolean(result.isError);
  }

  protected truncateToolResultContent(toolCall: ToolCall, rawResult: ToolResult, maxChars: number): boolean {
    if (typeof rawResult.content !== 'string' || rawResult.content.length <= maxChars) {
      return false;
    }

    // 终端工具结果要求保持 JSON 可解析，且已通过增量预览持续输出，不在此处做破坏性截断
    if (this.isTerminalProcessTool(toolCall.name)) {
      return false;
    }

    const truncated = rawResult.content.slice(0, maxChars);
    rawResult.content = `${truncated}\n...[truncated ${rawResult.content.length - maxChars} chars]`;
    return true;
  }

  protected preAnnounceToolCalls(streamId: string, toolCalls: ToolCall[]): Set<string> {
    const preAnnouncedToolCallIds = new Set<string>();
    for (const toolCall of toolCalls) {
      const requiresAuthorization = this.toolManager.requiresUserAuthorization(toolCall.name);
      // shell/process 工具即使需要授权也要先渲染卡片，避免执行期无可视反馈
      if (requiresAuthorization && !this.isTerminalProcessTool(toolCall.name)) {
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
    return preAnnouncedToolCallIds;
  }

  protected buildAssistantToolUseBlocks(toolCalls: ToolCall[]): Array<{
    type: 'tool_use';
    id: string;
    name: string;
    input: Record<string, any>;
  }> {
    return toolCalls.map((toolCall) => ({
      type: 'tool_use',
      id: toolCall.id,
      name: toolCall.name,
      input: toolCall.arguments,
    }));
  }

  private startDetachedToolStream(): string {
    const traceId = this.currentTraceId || this.syncTraceFromMessageHub();
    return this.normalizer.startStream(traceId);
  }

  protected async renderToolResultsWithTerminalStreaming({
    streamId,
    toolCalls,
    toolResults,
    preAnnouncedToolCallIds,
    executionContext,
    signal,
  }: ToolResultRenderParams): Promise<void> {
    const terminalPreviewPromises: Promise<void>[] = [];
    const terminalPreviewEntries: TerminalPreviewEntry[] = [];
    const toolCallMap = new Map(toolCalls.map((toolCall) => [toolCall.id, toolCall] as const));

    for (const result of toolResults) {
      const toolCall = toolCallMap.get(result.toolCallId);
      if (!toolCall) {
        continue;
      }
      const hardError = this.isHardToolFailure(result);
      const isTerminalTool = this.isTerminalProcessTool(toolCall.name);
      const isPreAnnounced = preAnnouncedToolCallIds.has(result.toolCallId);

      if (isTerminalTool) {
        const targetStreamId = isPreAnnounced ? streamId : this.startDetachedToolStream();
        const initialOutput = typeof result.content === 'string' ? result.content : String(result.content ?? '');

        // 终端工具统一进入“running + 自动预览”路径，避免因预告缺失退化成完成态一次性渲染
        this.normalizer.addToolCall(targetStreamId, {
          type: 'tool_call',
          toolName: toolCall.name,
          toolId: result.toolCallId,
          status: 'running',
          input: JSON.stringify(toolCall.arguments, null, 2),
          output: initialOutput,
        });

        terminalPreviewEntries.push({
          streamId: targetStreamId,
          ownStream: !isPreAnnounced,
          result,
        });

        const previewPromise = this.autoPreviewProcessOutput(
          targetStreamId,
          toolCall,
          result,
          executionContext,
          signal,
        );
        terminalPreviewPromises.push(previewPromise);
        continue;
      }

      if (isPreAnnounced) {
        this.normalizer.finishToolCall(
          streamId,
          result.toolCallId,
          hardError ? undefined : result.content,
          hardError ? result.content : undefined,
          result.fileChange,
          result.standardized
        );
        continue;
      }

      const deferredToolStreamId = this.startDetachedToolStream();
      this.normalizer.addToolCall(deferredToolStreamId, {
        type: 'tool_call',
        toolName: toolCall.name,
        toolId: result.toolCallId,
        status: hardError ? 'failed' : 'completed',
        input: JSON.stringify(toolCall.arguments, null, 2),
        output: hardError ? undefined : result.content,
        error: hardError ? result.content : undefined,
        standardized: result.standardized,
      });

      this.normalizer.finishToolCall(
        deferredToolStreamId,
        result.toolCallId,
        hardError ? undefined : result.content,
        hardError ? result.content : undefined,
        result.fileChange,
        result.standardized
      );
      this.normalizer.endStream(deferredToolStreamId);
    }

    if (terminalPreviewPromises.length === 0) {
      return;
    }

    await Promise.all(terminalPreviewPromises);
    for (const entry of terminalPreviewEntries) {
      const hardError = this.isHardToolFailure(entry.result);
      this.normalizer.finishToolCall(
        entry.streamId,
        entry.result.toolCallId,
        hardError ? undefined : entry.result.content,
        hardError ? entry.result.content : undefined,
        entry.result.fileChange,
        entry.result.standardized
      );
      if (entry.ownStream) {
        this.normalizer.endStream(entry.streamId);
      }
    }
  }

  protected async autoPreviewProcessOutput(
    streamId: string,
    toolCall: ToolCall,
    result: ToolResult,
    executionContext: ToolExecutionContext,
    signal?: AbortSignal,
  ): Promise<void> {
    if (result.isError || (toolCall.name !== 'process_launch' && toolCall.name !== 'shell')) {
      return;
    }

    const launchResult = this.parseToolResultJson(result);
    const terminalId = typeof launchResult?.terminal_id === 'number' ? launchResult.terminal_id : undefined;
    const status = typeof launchResult?.status === 'string' ? launchResult.status : '';
    const runMode = launchResult?.run_mode;
    const isTaskMode = runMode === 'task';
    const shouldStream = Boolean(
      terminalId
      && (runMode === 'service' || runMode === 'task')
      && status !== 'completed'
      && status !== 'failed'
      && status !== 'killed'
      && status !== 'timeout'
    );
    if (!shouldStream || !terminalId) {
      return;
    }

    const maxPreviewMs = isTaskMode ? 600_000 : 10_000; // task 最大 10 分钟，service 最大 10 秒
    const sessionId = this.syncTraceFromMessageHub();
    let frameSeq = 0;

    // 发送 terminalStreamStarted
    this.messageHub.data('terminalStreamStarted', {
      sessionId,
      traceId: sessionId,
      toolCallId: result.toolCallId,
      toolName: toolCall.name,
      workerId: executionContext.workerId,
      workerRole: executionContext.role,
      terminalId,
      frameSeq,
      status,
      runMode,
      phase: typeof launchResult?.phase === 'string' ? launchResult.phase : undefined,
      cwd: typeof launchResult?.cwd === 'string' ? launchResult.cwd : undefined,
      command: typeof toolCall.arguments?.command === 'string' ? toolCall.arguments.command : undefined,
      terminalName: typeof launchResult?.terminal_name === 'string' ? launchResult.terminal_name : undefined,
      output: typeof launchResult?.output === 'string' ? launchResult.output : '',
      outputCursor: typeof launchResult?.output_cursor === 'number' ? launchResult.output_cursor : 0,
      outputStartCursor: typeof launchResult?.output_start_cursor === 'number' ? launchResult.output_start_cursor : 0,
      nextCursor: typeof launchResult?.output_cursor === 'number' ? launchResult.output_cursor : 0,
      delta: false,
      timestamp: Date.now(),
    });

    // 事件驱动流式推送
    const executor = this.toolManager.getShellExecutor();
    const startAt = Date.now();
    const launchCursor = typeof launchResult?.output_cursor === 'number' ? launchResult.output_cursor : 0;

    return new Promise<void>((resolve) => {
      let resolved = false;
      let timeoutId: ReturnType<typeof setTimeout> | undefined;
      let abortHandler: (() => void) | undefined;

      const finalizeTaskResult = async (state: string): Promise<void> => {
        if (!isTaskMode) {
          return;
        }
        try {
          const readResult = await this.toolManager.executeInternalTool({
            id: `${toolCall.id}::final-read`,
            name: 'process_read',
            arguments: { terminal_id: terminalId, wait: false, max_wait_seconds: 1 },
          }, undefined, executionContext);
          if (typeof readResult.content === 'string' && readResult.content.trim()) {
            result.content = readResult.content;
            result.isError = state === 'failed' || state === 'killed' || state === 'timeout';
          }
        } catch {
          // 最终读取失败不影响主流程
        }
      };

      const cleanup = () => {
        if (resolved) return;
        resolved = true;
        if (timeoutId) {
          clearTimeout(timeoutId);
        }
        if (signal && abortHandler) {
          signal.removeEventListener('abort', abortHandler);
        }
        executor.off('processOutput', onOutput);
        executor.off('processCompleted', onCompleted);
        resolve();
      };

      const onOutput = (event: { processId: number; output: string; fromCursor: number; cursor: number }) => {
        if (event.processId !== terminalId) return;
        if (signal?.aborted) { cleanup(); return; }
        if (Date.now() - startAt >= maxPreviewMs) { cleanup(); return; }

        frameSeq += 1;
        this.messageHub.data('terminalStreamFrame', {
          sessionId,
          traceId: sessionId,
          toolCallId: result.toolCallId,
          toolName: toolCall.name,
          workerId: executionContext.workerId,
          workerRole: executionContext.role,
          terminalId,
          frameSeq,
          status: 'running',
          runMode,
          command: typeof toolCall.arguments?.command === 'string' ? toolCall.arguments.command : undefined,
          output: event.output,
          fromCursor: event.fromCursor,
          outputCursor: event.cursor,
          nextCursor: event.cursor,
          delta: true,
          timestamp: Date.now(),
        });
      };

      const onCompleted = (event: {
        processId: number;
        state: string;
        exitCode: number | null;
        output: string;
        cursor: number;
      }) => {
        if (event.processId !== terminalId) return;

        frameSeq += 1;
        this.messageHub.data('terminalStreamCompleted', {
          sessionId,
          traceId: sessionId,
          toolCallId: result.toolCallId,
          toolName: toolCall.name,
          workerId: executionContext.workerId,
          workerRole: executionContext.role,
          terminalId,
          frameSeq,
          status: event.state,
          runMode,
          command: typeof toolCall.arguments?.command === 'string' ? toolCall.arguments.command : undefined,
          returnCode: event.exitCode,
          output: event.output,
          outputCursor: event.cursor,
          nextCursor: event.cursor,
          delta: false,
          timestamp: Date.now(),
        });

        // task 终态：用最终输出替换 launch 快返内容
        if (isTaskMode) {
          void finalizeTaskResult(event.state).finally(cleanup);
          return;
        }

        cleanup();
      };

      executor.on('processOutput', onOutput);
      executor.on('processCompleted', onCompleted);

      void executor.readProcess(terminalId, false, 1, launchCursor, signal).then(async (snapshot) => {
        if (resolved) return;
        if (signal?.aborted) { cleanup(); return; }

        if (typeof snapshot.output === 'string' && snapshot.output.length > 0) {
          frameSeq += 1;
          this.messageHub.data('terminalStreamFrame', {
            sessionId,
            traceId: sessionId,
            toolCallId: result.toolCallId,
            toolName: toolCall.name,
            workerId: executionContext.workerId,
            workerRole: executionContext.role,
            terminalId,
            frameSeq,
            status: snapshot.status,
            runMode,
            command: typeof toolCall.arguments?.command === 'string' ? toolCall.arguments.command : undefined,
            output: snapshot.output,
            fromCursor: snapshot.from_cursor,
            outputCursor: snapshot.output_cursor,
            outputStartCursor: snapshot.output_start_cursor,
            nextCursor: snapshot.next_cursor,
            delta: true,
            truncated: snapshot.truncated,
            timestamp: Date.now(),
          });
        }

        if (snapshot.status === 'completed' || snapshot.status === 'failed' || snapshot.status === 'killed' || snapshot.status === 'timeout') {
          frameSeq += 1;
          this.messageHub.data('terminalStreamCompleted', {
            sessionId,
            traceId: sessionId,
            toolCallId: result.toolCallId,
            toolName: toolCall.name,
            workerId: executionContext.workerId,
            workerRole: executionContext.role,
            terminalId,
            frameSeq,
            status: snapshot.status,
            runMode,
            command: typeof toolCall.arguments?.command === 'string' ? toolCall.arguments.command : undefined,
            returnCode: snapshot.return_code,
            output: snapshot.output,
            outputCursor: snapshot.output_cursor,
            outputStartCursor: snapshot.output_start_cursor,
            nextCursor: snapshot.next_cursor,
            delta: false,
            truncated: snapshot.truncated,
            timestamp: Date.now(),
          });
          await finalizeTaskResult(snapshot.status);
          cleanup();
        }
      }).catch(() => {
        // 初始同步失败不影响后续实时事件
      });

      // 超时兜底
      timeoutId = setTimeout(() => {
        if (!resolved) cleanup();
      }, maxPreviewMs + 1000);

      // abort 信号监听
      if (signal) {
        abortHandler = () => {
          cleanup();
        };
        signal.addEventListener('abort', abortHandler, { once: true });
      }
    });
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
  setDecisionHook(hook?: (event: DecisionHookEvent) => string[]): void {
    this.decisionHook = hook;
  }

  /**
   * 设置当前请求标识（由 AdapterFactory 在每次 sendMessage 调用前注入）。
   * 实例级变量，不存在多 Worker 并发竞态。
   */
  setCurrentRequestId(requestId: string | undefined): void {
    this.currentRequestId = requestId;
  }

  /**
   * 设置当前请求的工具执行上下文覆盖（由 AdapterFactory 注入）。
   */
  setCurrentToolExecutionContext(context: Partial<ToolExecutionContext> | undefined): void {
    this.currentToolExecutionContext = context;
  }

  /**
   * 合并当前请求的工具执行上下文覆盖。
   */
  protected resolveToolExecutionContext(base: ToolExecutionContext): ToolExecutionContext {
    if (!this.currentToolExecutionContext) {
      return base;
    }
    return {
      workerId: this.currentToolExecutionContext.workerId ?? base.workerId,
      role: this.currentToolExecutionContext.role ?? base.role,
      worktreePath: this.currentToolExecutionContext.worktreePath ?? base.worktreePath,
    };
  }

  /**
   * 使用当前请求上下文启动流式消息
   * 优先复用占位消息 ID，确保 UI 端流式更新命中同一条消息。
   * 从实例级 currentRequestId 获取标识（取代已废弃的全局 requestContext）。
   */
  protected startStreamWithContext(visibility?: 'user' | 'system' | 'debug'): string {
    const traceId = this.syncTraceFromMessageHub();
    const requestId = this.currentRequestId;
    const boundMessageId = requestId ? this.messageHub.getRequestMessageId(requestId) : undefined;

    return this.normalizer.startStream(traceId, undefined, boundMessageId, visibility);
  }

  protected createRetryRuntimeHook(messageId?: string): LLMMessageParams['retryRuntimeHook'] | undefined {
    if (!messageId) {
      return undefined;
    }

    return (event) => {
      this.emitRetryRuntime(messageId, event);
    };
  }

  protected emitRetryRuntime(messageId: string, event: LLMRetryRuntimeEvent): void {
    if (!messageId) {
      return;
    }

    const traceId = this.syncTraceFromMessageHub();
    this.messageHub.data('llmRetryRuntime', {
      traceId,
      messageId,
      agent: this.agent,
      role: this.role,
      provider: this.config.provider,
      model: this.config.model,
      ...event,
    });
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
    const sendWithCurrentRequestContext = (message: import('../../protocol').StandardMessage): void => {
      this.messageHub.sendMessage(message, {
        explicitRequestId: this.currentRequestId,
      });
    };

    // 消息开始/流式：直接发送到 MessageHub
    this.normalizer.on(MESSAGE_EVENTS.MESSAGE, (message) => {
      // 续跑轮 / 工具派生流不能丢失当前 requestId，
      // 否则主线只会看到任务卡，思考过程与工具卡片可能漂成“裸消息”。
      sendWithCurrentRequestContext(message);
    });

    // 消息完成：直接发送到 MessageHub
    this.normalizer.on(MESSAGE_EVENTS.COMPLETE, (_messageId, message) => {
      sendWithCurrentRequestContext(message);
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
