import OpenAI from 'openai';
import { LLMConfig } from '../../../types/agent-types';
import {
  LLMMessage,
  LLMMessageParams,
  LLMResponse,
  LLMStreamChunk,
  ToolCall,
  ToolDefinition,
  sanitizeToolOrder,
} from '../../types';
import { logger, LogCategory } from '../../../logging';
import { ProviderProtocolAdapter } from '../provider-adapter';
import { resolveProviderProtocolProfile } from '../capability-registry';
import {
  is400ToolSchemaError,
  isChunkParseError,
  normalizeOpenAIUsage,
  normalizeStreamDelta,
  normalizeToolResultBlock,
  parseToolArguments,
  sanitizeSchema,
  toOpenAIToolMessageContent,
} from './protocol-utils';

const PROFILE = resolveProviderProtocolProfile('openai');

export class OpenAIResponsesProtocolAdapter implements ProviderProtocolAdapter {
  readonly provider = PROFILE.provider;
  readonly protocol = PROFILE.protocol;
  readonly capabilities = PROFILE.capabilities;

  constructor(
    private readonly config: LLMConfig,
    private readonly openaiClient: OpenAI,
  ) {}

  async send(request: LLMMessageParams): Promise<LLMResponse> {
    const { input, instructions } = this.convertToOpenAIResponsesInput(request);
    const openAiTools = this.mapToolsForOpenAI(request.tools);
    const requestParams: any = {
      model: this.config.model,
      input,
      max_output_tokens: request.maxTokens,
      temperature: request.temperature,
      tools: openAiTools,
    };
    if (instructions) {
      requestParams.instructions = instructions;
    }

    const openAiToolChoice = this.mapToolChoiceForOpenAI(request.toolChoice);
    if (openAiToolChoice && openAiTools && openAiTools.length > 0) {
      requestParams.tool_choice = openAiToolChoice;
    }

    if (this.shouldEnableReasoning()) {
      requestParams.reasoning = { effort: this.config.reasoningEffort };
      delete requestParams.temperature;
    }

    let response;
    try {
      response = await this.openaiClient.responses.create(requestParams);
    } catch (error: any) {
      if (is400ToolSchemaError(error) && requestParams.tools?.length > 0) {
        response = await this.retryWithToolElimination(requestParams, error);
      } else {
        throw error;
      }
    }

    logger.info('OpenAI Responses API response received', {
      model: this.config.model,
      responseId: response?.id,
      status: response?.status,
      outputCount: Array.isArray(response?.output) ? response.output.length : 0,
    }, LogCategory.LLM);

    return this.parseOpenAIResponse(response);
  }

  async stream(
    request: LLMMessageParams,
    onEvent: (event: LLMStreamChunk) => void,
  ): Promise<LLMResponse> {
    const { input, instructions } = this.convertToOpenAIResponsesInput(request);
    const openAiTools = this.mapToolsForOpenAI(request.tools);
    const requestParams: any = {
      model: this.config.model,
      input,
      max_output_tokens: request.maxTokens,
      temperature: request.temperature,
      tools: openAiTools,
      stream: true,
    };
    if (instructions) {
      requestParams.instructions = instructions;
    }

    const openAiToolChoice = this.mapToolChoiceForOpenAI(request.toolChoice);
    if (openAiToolChoice && openAiTools && openAiTools.length > 0) {
      requestParams.tool_choice = openAiToolChoice;
    }

    if (this.shouldEnableReasoning()) {
      requestParams.reasoning = { effort: this.config.reasoningEffort };
      delete requestParams.temperature;
    }

    let stream;
    try {
      stream = await this.openaiClient.responses.create(
        requestParams as Parameters<typeof this.openaiClient.responses.create>[0] & { stream: true },
        { signal: request.signal },
      );
    } catch (error: any) {
      if (is400ToolSchemaError(error) && requestParams.tools?.length > 0) {
        stream = await this.retryStreamWithToolElimination(requestParams, request.signal, error);
      } else {
        throw error;
      }
    }

    let fullContent = '';
    let contentDeltaMode: 'unknown' | 'delta' | 'cumulative' = 'unknown';
    const toolCallBuffers = new Map<string, {
      id: string;
      name?: string;
      argumentsText: string;
      argumentsDeltaMode: 'unknown' | 'delta' | 'cumulative';
    }>();
    const toolOutputIndexToKey = new Map<number, string>();
    const startedToolCallKeys = new Set<string>();
    const endedToolCallKeys = new Set<string>();
    const toolCallFallbackPrefix = `magi_call_${Date.now().toString(36)}`;
    let toolCallFallbackSeq = 0;
    const createFallbackToolCallId = () => `${toolCallFallbackPrefix}_${toolCallFallbackSeq++}`;
    const ensureToolCallBuffer = (key: string, callId?: string, name?: string) => {
      if (!toolCallBuffers.has(key)) {
        toolCallBuffers.set(key, {
          id: callId || createFallbackToolCallId(),
          name,
          argumentsText: '',
          argumentsDeltaMode: 'unknown',
        });
      }
      const buffer = toolCallBuffers.get(key)!;
      if (callId) buffer.id = callId;
      if (name) buffer.name = name;
      return buffer;
    };

    const parsePartialToolArgs = (text: string): Record<string, any> | undefined => {
      if (!text) return undefined;
      try {
        const parsed = JSON.parse(text);
        if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
          return parsed as Record<string, any>;
        }
      } catch {
        // 增量解析失败是正常的
      }
      return undefined;
    };

    let usage: { inputTokens: number; outputTokens: number } = { inputTokens: 0, outputTokens: 0 };
    let stopReason: LLMResponse['stopReason'] = 'end_turn';
    let emittedContentStart = false;
    let finalResponsePayload: any | undefined;

    const iterator = (stream as any)[Symbol.asyncIterator]();
    while (true) {
      let chunk: any;
      try {
        const result = await iterator.next();
        if (result.done) break;
        chunk = result.value as any;
      } catch (iterError: any) {
        if (isChunkParseError(iterError)) {
          logger.warn('OpenAI Responses stream chunk 底层解析失败，跳过此残片', {
            model: this.config.model,
            provider: this.config.provider,
            error: iterError?.message?.substring(0, 200),
          }, LogCategory.LLM);
          continue;
        }
        throw iterError;
      }

      switch (chunk.type) {
        case 'response.output_text.delta': {
          const incoming = typeof chunk.delta === 'string' ? chunk.delta : '';
          const normalized = normalizeStreamDelta(incoming, fullContent, contentDeltaMode);
          contentDeltaMode = normalized.mode;
          if (normalized.delta) {
            if (!emittedContentStart) {
              emittedContentStart = true;
              onEvent({ type: 'content_start' });
            }
            fullContent += normalized.delta;
            onEvent({ type: 'content_delta', content: normalized.delta });
          }
          break;
        }
        case 'response.output_text.done': {
          if (!fullContent && typeof chunk.text === 'string' && chunk.text) {
            fullContent = chunk.text;
          }
          if (emittedContentStart) {
            onEvent({ type: 'content_end' });
          }
          break;
        }
        case 'response.refusal.delta': {
          const refusalDelta = typeof chunk.delta === 'string' ? chunk.delta : '';
          if (refusalDelta) {
            if (!emittedContentStart) {
              emittedContentStart = true;
              onEvent({ type: 'content_start' });
            }
            fullContent += refusalDelta;
            onEvent({ type: 'content_delta', content: refusalDelta });
          }
          break;
        }
        case 'response.refusal.done': {
          if (!fullContent && typeof chunk.refusal === 'string' && chunk.refusal) {
            fullContent = chunk.refusal;
          }
          if (emittedContentStart) {
            onEvent({ type: 'content_end' });
          }
          break;
        }
        case 'response.reasoning_text.delta':
        case 'response.reasoning_summary_text.delta': {
          if (this.config.enableThinking === true) {
            const thinkingText = typeof chunk.delta === 'string' ? chunk.delta : '';
            if (thinkingText) {
              onEvent({ type: 'thinking', thinking: thinkingText });
            }
          }
          break;
        }
        case 'response.output_item.added':
        case 'response.output_item.done': {
          const item = chunk.item;
          if (!item || item.type !== 'function_call') {
            break;
          }
          const outputIndex = typeof chunk.output_index === 'number' ? chunk.output_index : undefined;
          const bufferKey = String(item.id || item.call_id || (outputIndex != null ? `idx_${outputIndex}` : createFallbackToolCallId()));
          const buffer = ensureToolCallBuffer(bufferKey, item.call_id || item.id, item.name);
          if (outputIndex != null) {
            toolOutputIndexToKey.set(outputIndex, bufferKey);
          }

          if (!startedToolCallKeys.has(bufferKey)) {
            startedToolCallKeys.add(bufferKey);
            onEvent({
              type: 'tool_call_start',
              toolCall: {
                id: buffer.id,
                name: buffer.name || '',
                arguments: {},
              },
            });
          }

          if (typeof item.arguments === 'string') {
            const normalizedArgs = normalizeStreamDelta(
              item.arguments,
              buffer.argumentsText,
              buffer.argumentsDeltaMode,
            );
            buffer.argumentsDeltaMode = normalizedArgs.mode;
            buffer.argumentsText += normalizedArgs.delta;
          }

          if (chunk.type === 'response.output_item.done' && !endedToolCallKeys.has(bufferKey)) {
            const parsedArgs = parseToolArguments(
              buffer.argumentsText,
              `stream:${buffer.name || buffer.id}`,
              this.config.provider,
              this.config.model,
            );
            onEvent({
              type: 'tool_call_end',
              toolCall: {
                id: buffer.id,
                name: buffer.name || '',
                arguments: parsedArgs.value,
              },
            });
            endedToolCallKeys.add(bufferKey);
          }
          break;
        }
        case 'response.function_call_arguments.delta': {
          const outputIndex = typeof chunk.output_index === 'number' ? chunk.output_index : undefined;
          const existingKey = typeof chunk.item_id === 'string' && chunk.item_id
            ? chunk.item_id
            : (outputIndex != null ? toolOutputIndexToKey.get(outputIndex) : undefined);
          const bufferKey = existingKey || (outputIndex != null ? `idx_${outputIndex}` : createFallbackToolCallId());
          const buffer = ensureToolCallBuffer(bufferKey);
          if (outputIndex != null) {
            toolOutputIndexToKey.set(outputIndex, bufferKey);
          }

          const deltaArgs = typeof chunk.delta === 'string' ? chunk.delta : '';
          if (deltaArgs) {
            const normalizedArgs = normalizeStreamDelta(
              deltaArgs,
              buffer.argumentsText,
              buffer.argumentsDeltaMode,
            );
            buffer.argumentsDeltaMode = normalizedArgs.mode;
            buffer.argumentsText += normalizedArgs.delta;
          }

          onEvent({
            type: 'tool_call_delta',
            toolCall: {
              id: buffer.id,
              name: buffer.name,
              arguments: parsePartialToolArgs(buffer.argumentsText),
            },
          });
          break;
        }
        case 'response.function_call_arguments.done': {
          const outputIndex = typeof chunk.output_index === 'number' ? chunk.output_index : undefined;
          const existingKey = typeof chunk.item_id === 'string' && chunk.item_id
            ? chunk.item_id
            : (outputIndex != null ? toolOutputIndexToKey.get(outputIndex) : undefined);
          const bufferKey = existingKey || (outputIndex != null ? `idx_${outputIndex}` : createFallbackToolCallId());
          const buffer = ensureToolCallBuffer(bufferKey, undefined, chunk.name);
          if (outputIndex != null) {
            toolOutputIndexToKey.set(outputIndex, bufferKey);
          }

          if (typeof chunk.arguments === 'string') {
            buffer.argumentsText = chunk.arguments;
            buffer.argumentsDeltaMode = 'delta';
            onEvent({
              type: 'tool_call_delta',
              toolCall: {
                id: buffer.id,
                name: buffer.name,
                arguments: parsePartialToolArgs(chunk.arguments),
              },
            });
          }

          if (!endedToolCallKeys.has(bufferKey)) {
            const parsedArgs = parseToolArguments(
              buffer.argumentsText,
              `stream:${buffer.name || buffer.id}`,
              this.config.provider,
              this.config.model,
            );
            onEvent({
              type: 'tool_call_end',
              toolCall: {
                id: buffer.id,
                name: buffer.name || '',
                arguments: parsedArgs.value,
              },
            });
            endedToolCallKeys.add(bufferKey);
          }
          break;
        }
        case 'response.completed':
        case 'response.incomplete': {
          finalResponsePayload = chunk.response;
          const normalizedUsage = normalizeOpenAIUsage(finalResponsePayload?.usage);
          if (normalizedUsage.inputTokens > 0 || normalizedUsage.outputTokens > 0) {
            usage = {
              inputTokens: normalizedUsage.inputTokens,
              outputTokens: normalizedUsage.outputTokens,
            };
            onEvent({ type: 'usage', usage: normalizedUsage });
          }
          stopReason = this.mapOpenAIStopReasonFromResponse(
            finalResponsePayload,
            toolCallBuffers.size > 0,
          );
          break;
        }
        case 'response.failed': {
          const errorMessage = chunk?.response?.error?.message || 'OpenAI Responses stream failed';
          throw new Error(errorMessage);
        }
        case 'error': {
          const errorMessage = chunk?.message || 'OpenAI Responses stream error';
          throw new Error(errorMessage);
        }
        default:
          break;
      }
    }

    if (!fullContent && finalResponsePayload) {
      fullContent = this.extractOpenAIResponseText(finalResponsePayload);
    }

    const toolCalls: ToolCall[] = [];
    for (const [bufferKey, tool] of toolCallBuffers.entries()) {
      const toolId = tool.id || bufferKey;
      if (!toolId) continue;
      const parsedArgs = parseToolArguments(
        tool.argumentsText,
        `stream:${tool.name || toolId}`,
        this.config.provider,
        this.config.model,
      );
      toolCalls.push({
        id: toolId,
        name: tool.name || '',
        arguments: parsedArgs.value,
        argumentParseError: parsedArgs.error,
        rawArguments: parsedArgs.rawText,
      });
    }

    if (finalResponsePayload?.output && Array.isArray(finalResponsePayload.output)) {
      for (const item of finalResponsePayload.output) {
        if (item?.type !== 'function_call') continue;
        const toolCallId = item.call_id || item.id || createFallbackToolCallId();
        if (toolCalls.some(tc => tc.id === toolCallId)) continue;
        const parsedArgs = parseToolArguments(
          item.arguments,
          `stream:${item.name || toolCallId}`,
          this.config.provider,
          this.config.model,
        );
        toolCalls.push({
          id: toolCallId,
          name: item.name || '',
          arguments: parsedArgs.value,
          argumentParseError: parsedArgs.error,
          rawArguments: parsedArgs.rawText,
        });
      }
    }

    if (toolCalls.length > 0) {
      stopReason = 'tool_use';
    }

    return {
      content: fullContent,
      toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
      usage,
      stopReason,
    };
  }

  private shouldEnableReasoning(): boolean {
    return !!this.config.reasoningEffort;
  }

  private mapToolsForOpenAI(tools?: ToolDefinition[]): any[] | undefined {
    if (!tools || tools.length === 0) {
      return undefined;
    }

    return tools.map(tool => ({
      type: 'function',
      name: tool.name,
      description: tool.description || 'No description available',
      parameters: sanitizeSchema(tool.input_schema),
      strict: false,
    }));
  }

  private mapToolChoiceForOpenAI(choice?: LLMMessageParams['toolChoice']): any | undefined {
    if (!choice) return undefined;
    if (typeof choice === 'string') {
      if (choice === 'auto' || choice === 'none' || choice === 'required') return choice;
      return undefined;
    }
    if (choice.type === 'any') {
      return 'required';
    }
    if (choice.type === 'tool' && choice.name) {
      return { type: 'function', name: choice.name };
    }
    return undefined;
  }

  private async retryWithToolElimination(requestParams: any, originalError: any): Promise<any> {
    const allTools: any[] = requestParams.tools;
    logger.warn('400 工具不兼容，启动渐进式排除', {
      model: this.config.model,
      toolCount: allTools.length,
      error: originalError?.message?.substring(0, 200),
    }, LogCategory.LLM);

    const compatibleTools = await this.findCompatibleTools(
      allTools,
      (tools) => {
        requestParams.tools = tools.length > 0 ? tools : undefined;
        if (!requestParams.tools) delete requestParams.tool_choice;
        return this.openaiClient.responses.create(requestParams);
      },
    );

    requestParams.tools = compatibleTools.length > 0 ? compatibleTools : undefined;
    if (!requestParams.tools) delete requestParams.tool_choice;
    return this.openaiClient.responses.create(requestParams);
  }

  private async retryStreamWithToolElimination(requestParams: any, signal?: AbortSignal, originalError?: any): Promise<any> {
    const allTools: any[] = requestParams.tools;
    logger.warn('400(stream) 工具不兼容，启动渐进式排除', {
      model: this.config.model,
      toolCount: allTools.length,
      error: originalError?.message?.substring(0, 200),
    }, LogCategory.LLM);

    const createStream = (tools: any[]) => {
      requestParams.tools = tools.length > 0 ? tools : undefined;
      if (!requestParams.tools) delete requestParams.tool_choice;
      return (this.openaiClient.responses.create as any)(
        { ...requestParams, stream: true },
        { signal },
      );
    };

    const compatibleTools = await this.findCompatibleTools(allTools, createStream);

    requestParams.tools = compatibleTools.length > 0 ? compatibleTools : undefined;
    if (!requestParams.tools) delete requestParams.tool_choice;
    return (this.openaiClient.responses.create as any)(
      { ...requestParams, stream: true },
      { signal },
    );
  }

  private async findCompatibleTools(
    tools: any[],
    tryRequest: (tools: any[]) => Promise<any>,
  ): Promise<any[]> {
    if (tools.length <= 1) {
      if (tools.length === 0) return [];
      try {
        await tryRequest(tools);
        return tools;
      } catch (error: any) {
        if (is400ToolSchemaError(error)) {
          logger.warn('排除不兼容工具', {
            toolName: tools[0]?.name || tools[0]?.function?.name || 'unknown',
          }, LogCategory.LLM);
          return [];
        }
        throw error;
      }
    }

    try {
      await tryRequest(tools);
      return tools;
    } catch (error: any) {
      if (!is400ToolSchemaError(error)) throw error;
    }

    const mid = Math.ceil(tools.length / 2);
    const firstHalf = tools.slice(0, mid);
    const secondHalf = tools.slice(mid);

    const [compatible1, compatible2] = await Promise.all([
      this.findCompatibleTools(firstHalf, tryRequest),
      this.findCompatibleTools(secondHalf, tryRequest),
    ]);

    const merged = [...compatible1, ...compatible2];

    logger.info('工具兼容性排除完成', {
      original: tools.length,
      retained: merged.length,
      removed: tools.length - merged.length,
    }, LogCategory.LLM);

    return merged;
  }

  private convertToOpenAIResponsesInput(params: LLMMessageParams): {
    input: any[];
    instructions?: string;
  } {
    const sanitizedMessages = sanitizeToolOrder(params.messages);
    const input: any[] = [];
    const declaredIds: string[] = [];
    const resultIds: string[] = [];

    for (const msg of sanitizedMessages) {
      const role = this.mapOpenAIRoleForResponses(msg.role);
      if (typeof msg.content === 'string') {
        input.push({ type: 'message', role, content: msg.content });
        continue;
      }

      const messageContent: any[] = [];
      const flushMessageContent = () => {
        if (messageContent.length === 0) return;
        const assistantPlainText = role === 'assistant'
          && messageContent.every(part => part?.type === 'input_text');
        input.push({
          type: 'message',
          role,
          content: assistantPlainText
            ? messageContent.map(part => part.text || '').join('\n')
            : [...messageContent],
        });
        messageContent.length = 0;
      };

      for (const block of msg.content) {
        const b = block as any;
        switch (b.type) {
          case 'text':
            messageContent.push({ type: 'input_text', text: b.text || '' });
            break;
          case 'image':
            messageContent.push({
              type: 'input_image',
              detail: 'auto',
              image_url: `data:${b.source?.media_type || 'image/png'};base64,${b.source?.data || ''}`,
            });
            break;
          case 'tool_use': {
            flushMessageContent();
            const toolCallId = typeof b.id === 'string' && b.id.trim()
              ? b.id.trim()
              : `magi_call_input_${Date.now().toString(36)}_${declaredIds.length}`;
            declaredIds.push(toolCallId);
            input.push({
              type: 'function_call',
              call_id: toolCallId,
              name: b.name || '',
              arguments: typeof b.input === 'string' ? b.input : JSON.stringify(b.input ?? {}),
            });
            break;
          }
          case 'tool_result': {
            flushMessageContent();
            const normalized = normalizeToolResultBlock(
              b,
              `openai-responses:${msg.role}`,
              this.config.provider,
              this.config.model,
            );
            if (!normalized) {
              break;
            }
            resultIds.push(normalized.toolUseId);
            input.push({
              type: 'function_call_output',
              call_id: normalized.toolUseId,
              output: toOpenAIToolMessageContent({
                content: normalized.content,
                isError: normalized.isError,
              }),
            });
            break;
          }
        }
      }

      flushMessageContent();
    }

    const countBy = (ids: string[]): Map<string, number> => {
      const counter = new Map<string, number>();
      for (const id of ids) {
        counter.set(id, (counter.get(id) || 0) + 1);
      }
      return counter;
    };
    const declaredCounter = countBy(declaredIds);
    const resultCounter = countBy(resultIds);

    const missingResults: string[] = [];
    const orphanResults: string[] = [];

    for (const [id, declaredCount] of declaredCounter.entries()) {
      const resultCount = resultCounter.get(id) || 0;
      const gap = declaredCount - resultCount;
      for (let i = 0; i < gap; i++) missingResults.push(id);
    }
    for (const [id, resultCount] of resultCounter.entries()) {
      const declaredCount = declaredCounter.get(id) || 0;
      const gap = resultCount - declaredCount;
      for (let i = 0; i < gap; i++) orphanResults.push(id);
    }
    if (missingResults.length > 0 || orphanResults.length > 0) {
      logger.warn('convertToOpenAIResponsesInput: call_id 匹配异常', {
        missingResults,
        orphanResults,
        declaredCount: declaredIds.length,
        resultCount: resultIds.length,
      }, LogCategory.LLM);
    }

    logger.debug('convertToOpenAIResponsesInput 转换完成', {
      inputCount: params.messages.length,
      outputCount: input.length,
      roles: input.filter(item => item?.type === 'message').map(item => item.role),
      toolCallIds: declaredIds,
      toolResultIds: resultIds,
      matched: missingResults.length === 0 && orphanResults.length === 0,
    }, LogCategory.LLM);

    const instructions = typeof params.systemPrompt === 'string' && params.systemPrompt.trim()
      ? params.systemPrompt
      : undefined;

    return { input, instructions };
  }

  private mapOpenAIRoleForResponses(role: LLMMessage['role']): 'user' | 'assistant' | 'system' {
    if (role === 'assistant') return 'assistant';
    if (role === 'system') return 'system';
    return 'user';
  }

  private parseOpenAIResponse(response: any): LLMResponse {
    const toolCalls: ToolCall[] = [];
    const outputItems = Array.isArray(response?.output) ? response.output : [];
    const fallbackPrefix = `magi_call_sync_${Date.now().toString(36)}_`;
    let fallbackSeq = 0;
    for (const item of outputItems) {
      if (!item || item.type !== 'function_call') continue;
      const toolCallId = item.call_id || item.id || `${fallbackPrefix}${fallbackSeq++}`;
      const parsedArgs = parseToolArguments(
        item.arguments,
        `sync:${item.name || toolCallId}`,
        this.config.provider,
        this.config.model,
      );
      toolCalls.push({
        id: toolCallId,
        name: item.name || '',
        arguments: parsedArgs.value,
        argumentParseError: parsedArgs.error,
        rawArguments: parsedArgs.rawText,
      });
    }

    return {
      content: this.extractOpenAIResponseText(response),
      toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
      usage: normalizeOpenAIUsage(response?.usage),
      stopReason: this.mapOpenAIStopReasonFromResponse(response, toolCalls.length > 0),
    };
  }

  private extractOpenAIResponseText(response: any): string {
    if (typeof response?.output_text === 'string' && response.output_text.trim()) {
      return response.output_text;
    }
    const fragments: string[] = [];
    const outputItems = Array.isArray(response?.output) ? response.output : [];
    for (const item of outputItems) {
      if (!item || item.type !== 'message' || !Array.isArray(item.content)) continue;
      for (const part of item.content) {
        if (part?.type === 'output_text' && typeof part.text === 'string') {
          fragments.push(part.text);
        } else if (part?.type === 'refusal' && typeof part.refusal === 'string') {
          fragments.push(part.refusal);
        }
      }
    }
    return fragments.join('\n');
  }

  private mapOpenAIStopReasonFromResponse(response: any, hasToolCalls: boolean): LLMResponse['stopReason'] {
    if (hasToolCalls) {
      return 'tool_use';
    }
    const incompleteReason = String(response?.incomplete_details?.reason || '');
    switch (incompleteReason) {
      case 'max_output_tokens':
        return 'max_tokens';
      default:
        return 'end_turn';
    }
  }
}
