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
import { MessageHub } from '../../orchestrator/core/message/message-hub';
import { logger, LogCategory } from '../../logging';
import { MESSAGE_EVENTS, ADAPTER_EVENTS } from '../../protocol/event-names';
import { mergeToolPolicies } from '../../tools/tool-policy';
import type {
  MessageMetadata,
  ToolCallBlock,
  StandardizedToolResultPayload,
} from '../../protocol/message-protocol';

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
  toolCall: ToolCall;
  result: ToolResult;
}

interface TerminalPreviewSnapshot {
  terminal_id: number;
  status: string;
  run_mode?: 'task' | 'service';
  phase?: string;
  terminal_name?: string;
  cwd?: string;
  command?: string;
  output: string;
  output_cursor?: number;
  output_start_cursor?: number;
  from_cursor?: number;
  next_cursor?: number;
  delta?: boolean;
  truncated?: boolean;
  startup_status?: string;
  startup_message?: string;
  locked?: boolean;
  return_code?: number | null;
  accepted?: boolean;
  killed?: boolean;
  released_lock?: boolean;
  error?: string;
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
  /**
   * 当前输出流的显式消息归属元数据。
   * 用于把 Worker 普通流式输出绑定到 assignment 生命周期卡片。
   */
  protected currentMessageMetadata: Partial<MessageMetadata> | undefined;

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

  private computeTerminalOutputOverlap(previous: string, incoming: string): number {
    const maxOverlap = Math.min(previous.length, incoming.length);
    for (let length = maxOverlap; length > 0; length -= 1) {
      if (previous.slice(previous.length - length) === incoming.slice(0, length)) {
        return length;
      }
    }
    return 0;
  }

  private reconcileTerminalOutputSnapshot(previous: string, incoming: string): string {
    if (!incoming) return previous;
    if (!previous) return incoming;
    if (incoming === previous) return previous;
    if (incoming.startsWith(previous)) return incoming;
    if (previous.startsWith(incoming)) return previous;
    if (previous.includes(incoming)) return previous;
    if (incoming.includes(previous)) return incoming;
    const overlap = this.computeTerminalOutputOverlap(previous, incoming);
    if (overlap > 0) {
      return previous + incoming.slice(overlap);
    }
    return incoming;
  }

  private mergeTerminalDeltaOutput(previous: TerminalPreviewSnapshot, patch: Partial<TerminalPreviewSnapshot>): string {
    const currentOutput = previous.output || '';
    const chunk = typeof patch.output === 'string' ? patch.output : '';
    if (!chunk) {
      return currentOutput;
    }

    const fromCursor = typeof patch.from_cursor === 'number' ? patch.from_cursor : undefined;
    const previousNextCursor = typeof previous.next_cursor === 'number'
      ? previous.next_cursor
      : (typeof previous.output_cursor === 'number' ? previous.output_cursor : undefined);

    if (fromCursor === undefined || previousNextCursor === undefined) {
      if (currentOutput.endsWith(chunk)) {
        return currentOutput;
      }
      return currentOutput + chunk;
    }

    if (fromCursor === previousNextCursor) {
      return currentOutput + chunk;
    }

    if (fromCursor < previousNextCursor) {
      const drop = previousNextCursor - fromCursor;
      if (drop >= chunk.length) {
        return currentOutput;
      }
      return currentOutput + chunk.slice(drop);
    }

    return currentOutput + chunk;
  }

  private applyTerminalPreviewPatch(
    previous: TerminalPreviewSnapshot,
    patch: Partial<TerminalPreviewSnapshot>,
  ): TerminalPreviewSnapshot {
    const nextOutput = typeof patch.output === 'string'
      ? (patch.delta
        ? this.mergeTerminalDeltaOutput(previous, patch)
        : this.reconcileTerminalOutputSnapshot(previous.output || '', patch.output))
      : previous.output;

    return {
      ...previous,
      ...patch,
      output: nextOutput,
      status: typeof patch.status === 'string' && patch.status.trim() ? patch.status : previous.status,
      run_mode: patch.run_mode ?? previous.run_mode,
      phase: patch.phase ?? previous.phase,
      terminal_name: patch.terminal_name ?? previous.terminal_name,
      cwd: patch.cwd ?? previous.cwd,
      command: patch.command ?? previous.command,
      output_cursor: patch.output_cursor ?? previous.output_cursor,
      output_start_cursor: patch.output_start_cursor ?? previous.output_start_cursor,
      from_cursor: patch.from_cursor ?? previous.from_cursor,
      next_cursor: patch.next_cursor ?? previous.next_cursor,
      delta: patch.delta ?? previous.delta,
      truncated: patch.truncated ?? previous.truncated,
      startup_status: patch.startup_status ?? previous.startup_status,
      startup_message: patch.startup_message ?? previous.startup_message,
      locked: patch.locked ?? previous.locked,
      return_code: patch.return_code ?? previous.return_code,
      accepted: patch.accepted ?? previous.accepted,
      killed: patch.killed ?? previous.killed,
      released_lock: patch.released_lock ?? previous.released_lock,
      error: patch.error ?? previous.error,
    };
  }

  protected updateTerminalPreviewStandardized(
    toolCall: ToolCall,
    result: ToolResult,
    status: StandardizedToolResult['status'],
    message?: string,
  ): void {
    const toolCallId = result.toolCallId || toolCall.id;
    const content = typeof result.content === 'string' ? result.content : String(result.content ?? '');
    const normalizedMessage = typeof message === 'string' && message.trim()
      ? message.trim()
      : content;
    const existing = result.standardized;

    result.standardized = existing
      ? {
          ...existing,
          source: this.resolveToolSource(toolCall.name, undefined, existing.source),
          toolName: existing.toolName || toolCall.name,
          toolCallId,
          status,
          message: normalizedMessage,
        }
      : {
          schemaVersion: 'tool-result.v1',
          source: this.resolveToolSource(toolCall.name),
          toolName: toolCall.name,
          toolCallId,
          status,
          message: normalizedMessage,
        };
    result.isError = status !== 'success';
  }

  protected resolveTerminalCompletionStatus(state: string): StandardizedToolResult['status'] {
    switch ((state || '').trim()) {
      case 'completed':
      case 'success':
      case 'ready':
        return 'success';
      case 'timeout':
        return 'timeout';
      case 'killed':
        return 'killed';
      default:
        return 'error';
    }
  }

  protected buildTerminalFailureMessage(snapshot: TerminalPreviewSnapshot): string {
    if (typeof snapshot.error === 'string' && snapshot.error.trim()) {
      return snapshot.error.trim();
    }
    if (typeof snapshot.status === 'string' && snapshot.status.trim()) {
      return `Terminal process ${snapshot.status.trim()}`;
    }
    return 'Terminal process failed';
  }

  protected emitTerminalToolPreview(
    streamId: string,
    toolCall: ToolCall,
    result: ToolResult,
    snapshot: TerminalPreviewSnapshot,
    status: ToolCallBlock['status'] = 'running',
    error?: string,
    standardized?: StandardizedToolResultPayload,
  ): void {
    const serialized = JSON.stringify(snapshot);
    result.content = serialized;
    this.normalizer.addToolCall(streamId, {
      type: 'tool_call',
      toolName: toolCall.name,
      toolId: result.toolCallId || toolCall.id,
      status,
      input: JSON.stringify(toolCall.arguments, null, 2),
      output: serialized,
      ...(error ? { error } : {}),
      ...(standardized ? { standardized } : {}),
    });
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
    const traceId = this.syncTraceFromMessageHub();
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
          toolCall,
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
      const output = typeof entry.result.content === 'string' ? entry.result.content : String(entry.result.content ?? '');
      const error = hardError
        ? (() => {
            const parsed = this.parseToolResultJson(entry.result);
            const snapshotError = typeof parsed?.error === 'string' ? parsed.error : undefined;
            return snapshotError || entry.result.standardized?.message || output || 'Terminal process failed';
          })()
        : undefined;
      this.normalizer.settleToolCallBlock(
        entry.streamId,
        {
          type: 'tool_call',
          toolName: entry.toolCall.name,
          toolId: entry.result.toolCallId,
          status: hardError ? 'failed' : 'completed',
          input: JSON.stringify(entry.toolCall.arguments, null, 2),
          output,
          ...(error ? { error } : {}),
          ...(entry.result.standardized ? { standardized: entry.result.standardized } : {}),
        },
        entry.result.fileChange,
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
    const normalizedRunMode = runMode === 'service' || runMode === 'task' ? runMode : undefined;
    const isTaskMode = normalizedRunMode === 'task';
    const shouldStream = Boolean(
      terminalId
      && normalizedRunMode
      && status !== 'completed'
      && status !== 'failed'
      && status !== 'killed'
      && status !== 'timeout'
    );
    if (!shouldStream || !terminalId) {
      return;
    }

    const maxPreviewMs = isTaskMode ? 600_000 : 10_000; // task 最大 10 分钟，service 最大 10 秒
    let previewSnapshot: TerminalPreviewSnapshot = {
      terminal_id: terminalId,
      status: status || 'running',
      run_mode: normalizedRunMode,
      phase: typeof launchResult?.phase === 'string' ? launchResult.phase : undefined,
      terminal_name: typeof launchResult?.terminal_name === 'string' ? launchResult.terminal_name : undefined,
      cwd: typeof launchResult?.cwd === 'string' ? launchResult.cwd : undefined,
      command: typeof toolCall.arguments?.command === 'string' ? toolCall.arguments.command : undefined,
      output: typeof launchResult?.output === 'string' ? launchResult.output : '',
      output_cursor: typeof launchResult?.output_cursor === 'number' ? launchResult.output_cursor : 0,
      output_start_cursor: typeof launchResult?.output_start_cursor === 'number' ? launchResult.output_start_cursor : 0,
      next_cursor: typeof launchResult?.output_cursor === 'number' ? launchResult.output_cursor : 0,
      delta: false,
      truncated: typeof launchResult?.truncated === 'boolean' ? launchResult.truncated : undefined,
      startup_status: typeof launchResult?.startup_status === 'string' ? launchResult.startup_status : undefined,
      startup_message: typeof launchResult?.startup_message === 'string' ? launchResult.startup_message : undefined,
      locked: typeof launchResult?.locked === 'boolean' ? launchResult.locked : undefined,
      return_code: typeof launchResult?.return_code === 'number'
        ? launchResult.return_code
        : (launchResult?.return_code === null ? null : undefined),
      accepted: typeof launchResult?.accepted === 'boolean' ? launchResult.accepted : undefined,
      killed: typeof launchResult?.killed === 'boolean' ? launchResult.killed : undefined,
      released_lock: typeof launchResult?.released_lock === 'boolean' ? launchResult.released_lock : undefined,
      error: typeof launchResult?.error === 'string' ? launchResult.error : undefined,
    };
    this.emitTerminalToolPreview(streamId, toolCall, result, previewSnapshot, 'running');

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

        previewSnapshot = this.applyTerminalPreviewPatch(previewSnapshot, {
          status: 'running',
          run_mode: normalizedRunMode,
          command: typeof toolCall.arguments?.command === 'string' ? toolCall.arguments.command : undefined,
          output: event.output,
          from_cursor: event.fromCursor,
          output_cursor: event.cursor,
          next_cursor: event.cursor,
          delta: true,
        });
        this.emitTerminalToolPreview(streamId, toolCall, result, previewSnapshot, 'running');
      };

      const onCompleted = (event: {
        processId: number;
        state: string;
        exitCode: number | null;
        output: string;
        cursor: number;
      }) => {
        if (event.processId !== terminalId) return;
        previewSnapshot = this.applyTerminalPreviewPatch(previewSnapshot, {
          status: event.state,
          run_mode: normalizedRunMode,
          command: typeof toolCall.arguments?.command === 'string' ? toolCall.arguments.command : undefined,
          output: event.output,
          output_cursor: event.cursor,
          next_cursor: event.cursor,
          delta: false,
          return_code: event.exitCode,
          killed: event.state === 'killed' ? true : previewSnapshot.killed,
        });
        const completionStatus = this.resolveTerminalCompletionStatus(event.state);
        if (completionStatus !== 'success') {
          previewSnapshot = this.applyTerminalPreviewPatch(previewSnapshot, {
            error: this.buildTerminalFailureMessage(previewSnapshot),
          });
        }
        result.content = JSON.stringify(previewSnapshot);
        this.updateTerminalPreviewStandardized(
          toolCall,
          result,
          completionStatus,
          completionStatus === 'success' ? result.content : this.buildTerminalFailureMessage(previewSnapshot),
        );

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
          previewSnapshot = this.applyTerminalPreviewPatch(previewSnapshot, {
            status: typeof snapshot.status === 'string' ? snapshot.status : previewSnapshot.status,
            run_mode: normalizedRunMode,
            command: typeof toolCall.arguments?.command === 'string' ? toolCall.arguments.command : undefined,
            output: snapshot.output,
            from_cursor: typeof snapshot.from_cursor === 'number' ? snapshot.from_cursor : undefined,
            output_cursor: typeof snapshot.output_cursor === 'number' ? snapshot.output_cursor : undefined,
            output_start_cursor: typeof snapshot.output_start_cursor === 'number' ? snapshot.output_start_cursor : undefined,
            next_cursor: typeof snapshot.next_cursor === 'number' ? snapshot.next_cursor : undefined,
            delta: true,
            truncated: typeof snapshot.truncated === 'boolean' ? snapshot.truncated : undefined,
          });
          this.emitTerminalToolPreview(streamId, toolCall, result, previewSnapshot, 'running');
        }

        if (snapshot.status === 'completed' || snapshot.status === 'failed' || snapshot.status === 'killed' || snapshot.status === 'timeout') {
          previewSnapshot = this.applyTerminalPreviewPatch(previewSnapshot, {
            status: snapshot.status,
            run_mode: normalizedRunMode,
            command: typeof toolCall.arguments?.command === 'string' ? toolCall.arguments.command : undefined,
            output: typeof snapshot.output === 'string' ? snapshot.output : previewSnapshot.output,
            output_cursor: typeof snapshot.output_cursor === 'number' ? snapshot.output_cursor : undefined,
            output_start_cursor: typeof snapshot.output_start_cursor === 'number' ? snapshot.output_start_cursor : undefined,
            next_cursor: typeof snapshot.next_cursor === 'number' ? snapshot.next_cursor : undefined,
            delta: false,
            truncated: typeof snapshot.truncated === 'boolean' ? snapshot.truncated : undefined,
            return_code: typeof snapshot.return_code === 'number'
              ? snapshot.return_code
              : (snapshot.return_code === null ? null : undefined),
            killed: snapshot.status === 'killed' ? true : previewSnapshot.killed,
          });
          const completionStatus = this.resolveTerminalCompletionStatus(snapshot.status);
          if (completionStatus !== 'success') {
            previewSnapshot = this.applyTerminalPreviewPatch(previewSnapshot, {
              error: this.buildTerminalFailureMessage(previewSnapshot),
            });
          }
          result.content = JSON.stringify(previewSnapshot);
          this.updateTerminalPreviewStandardized(
            toolCall,
            result,
            completionStatus,
            completionStatus === 'success' ? result.content : this.buildTerminalFailureMessage(previewSnapshot),
          );
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
   * 设置当前请求的消息归属元数据（实例级，非全局）。
   */
  setCurrentMessageMetadata(metadata: Partial<MessageMetadata> | undefined): void {
    this.currentMessageMetadata = metadata;
    const boundSessionId = this.resolveBoundSessionId();
    if (boundSessionId) {
      this.currentTraceId = boundSessionId;
    }
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
      toolPolicy: mergeToolPolicies([
        base.toolPolicy,
        this.currentToolExecutionContext.toolPolicy,
      ]),
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

    // 将 requestId 注入流式消息 metadata，确保前端语义锚点
    // 能通过 scope ID 将 worker_dispatch/task_card/worker_wait 关联到同一请求。
    const streamMetadata: Record<string, unknown> = {
      ...(this.currentMessageMetadata || {}),
    };
    if (requestId) {
      streamMetadata.requestId = requestId;
    }

    return this.normalizer.startStream(traceId, undefined, boundMessageId, visibility, streamMetadata);
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
    const sessionId = this.resolveMessageSessionId();
    this.messageHub.data('llmRetryRuntime', {
      sessionId,
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
   * 将适配器当前链路 trace 与 MessageHub 保持一致。
   * 会话归属始终来自 metadata.sessionId，trace 仅用于链路追踪。
   */
  protected syncTraceFromMessageHub(): string {
    const boundSessionId = this.resolveBoundSessionId();
    if (boundSessionId) {
      this.currentTraceId = boundSessionId;
      return this.currentTraceId;
    }

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

  protected resolveBoundSessionId(): string | undefined {
    const sessionId = this.currentMessageMetadata?.sessionId;
    if (typeof sessionId !== 'string') {
      return undefined;
    }
    const normalized = sessionId.trim();
    return normalized || undefined;
  }

  protected resolveMessageSessionId(): string {
    const boundSessionId = this.resolveBoundSessionId() || this.messageHub.getSessionId();
    if (typeof boundSessionId === 'string' && boundSessionId.trim()) {
      return boundSessionId.trim();
    }
    return this.currentMessageMetadata?.sessionId?.trim() || '';
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

    const traceId = this.syncTraceFromMessageHub();
    const sessionId = this.resolveMessageSessionId();
    this.messageHub.data('executionTokenRuntime', {
      sessionId,
      traceId,
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
