/**
 * OpenAI Chat Completions 协议适配器
 *
 * 适用于第三方 OpenAI 兼容 API（如腾讯云 Coding Plan、DeepSeek、Moonshot 等）。
 * 这些 API 通常只支持 /chat/completions 端点，不支持 /responses 端点。
 */

import OpenAI from 'openai';
import { LLMConfig } from '../../../types/agent-types';
import {
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
  DeltaMode,
} from './protocol-utils';

const PROFILE = resolveProviderProtocolProfile('openai', 'openai.chat-completions');

export class OpenAIChatCompletionsProtocolAdapter implements ProviderProtocolAdapter {
  readonly provider = PROFILE.provider;
  readonly protocol = PROFILE.protocol;
  readonly capabilities = PROFILE.capabilities;

  constructor(
    private readonly config: LLMConfig,
    private readonly openaiClient: OpenAI,
  ) {}

  async send(request: LLMMessageParams): Promise<LLMResponse> {
    const messages = this.convertToChatMessages(request);
    const tools = this.mapToolsForChat(request.tools);
    const requestParams: any = {
      model: this.config.model,
      messages,
      max_tokens: request.maxTokens,
      temperature: request.temperature,
    };
    if (tools && tools.length > 0) {
      requestParams.tools = tools;
      const toolChoice = this.mapToolChoiceForChat(request.toolChoice);
      if (toolChoice) {
        requestParams.tool_choice = toolChoice;
      }
    }

    let response;
    try {
      response = await this.openaiClient.chat.completions.create(requestParams, { signal: request.signal });
    } catch (error: any) {
      if (is400ToolSchemaError(error) && requestParams.tools?.length > 0) {
        response = await this.retryWithToolElimination(requestParams, request.signal, error);
      } else {
        throw error;
      }
    }

    logger.info('OpenAI Chat Completions API response received', {
      model: this.config.model,
      responseId: response?.id,
      choicesCount: Array.isArray(response?.choices) ? response.choices.length : 0,
    }, LogCategory.LLM);

    return this.parseChatResponse(response);
  }

  async stream(
    request: LLMMessageParams,
    onEvent: (event: LLMStreamChunk) => void,
  ): Promise<LLMResponse> {
    const messages = this.convertToChatMessages(request);
    const tools = this.mapToolsForChat(request.tools);
    const requestParams: any = {
      model: this.config.model,
      messages,
      max_tokens: request.maxTokens,
      temperature: request.temperature,
      stream: true,
    };
    if (tools && tools.length > 0) {
      requestParams.tools = tools;
      const toolChoice = this.mapToolChoiceForChat(request.toolChoice);
      if (toolChoice) {
        requestParams.tool_choice = toolChoice;
      }
    }

    let stream;
    try {
      stream = await this.openaiClient.chat.completions.create(requestParams, { signal: request.signal });
    } catch (error: any) {
      if (is400ToolSchemaError(error) && requestParams.tools?.length > 0) {
        stream = await this.retryStreamWithToolElimination(requestParams, request.signal, error);
      } else {
        throw error;
      }
    }

    return this.processChatStream(stream as any, onEvent);
  }

  // ============================================================================
  // 流式处理
  // ============================================================================

  private async processChatStream(
    stream: AsyncIterable<any>,
    onEvent: (event: LLMStreamChunk) => void,
  ): Promise<LLMResponse> {
    let fullContent = '';
    let contentDeltaMode: DeltaMode = 'unknown';
    const toolCallBuffers = new Map<number, {
      id: string;
      name: string;
      argumentsText: string;
      argumentsDeltaMode: DeltaMode;
    }>();
    let emittedContentStart = false;
    let usage = { inputTokens: 0, outputTokens: 0 };
    let stopReason: LLMResponse['stopReason'] = 'end_turn';

    const iterator = (stream as any)[Symbol.asyncIterator]();
    while (true) {
      let chunk: any;
      try {
        const result = await iterator.next();
        if (result.done) break;
        chunk = result.value;
      } catch (iterError: any) {
        if (isChunkParseError(iterError)) {
          logger.warn('Chat Completions stream chunk 解析失败，跳过', {
            error: iterError?.message?.substring(0, 200),
          }, LogCategory.LLM);
          continue;
        }
        throw iterError;
      }

      // 处理尾部 usage（部分 API 在最后一个 chunk 返回）
      if (!chunk?.choices?.[0]?.delta && chunk?.usage) {
        const normalizedUsage = normalizeOpenAIUsage(chunk.usage);
        if (normalizedUsage.inputTokens > 0 || normalizedUsage.outputTokens > 0) {
          usage = normalizedUsage;
          onEvent({ type: 'usage', usage: normalizedUsage });
        }
        continue;
      }

      const delta = chunk?.choices?.[0]?.delta;
      if (!delta) continue;

      // 处理 reasoning_content（部分兼容 API 如 DeepSeek 支持）
      if (delta.reasoning_content) {
        onEvent({ type: 'thinking', thinking: delta.reasoning_content });
      }

      // 处理文本内容
      if (delta.content) {
        if (!emittedContentStart) {
          emittedContentStart = true;
          onEvent({ type: 'content_start' });
        }
        const { delta: normalizedDelta, mode } = normalizeStreamDelta(delta.content, fullContent, contentDeltaMode);
        contentDeltaMode = mode;
        if (normalizedDelta) {
          fullContent += normalizedDelta;
          onEvent({ type: 'content_delta', content: normalizedDelta });
        }
      }

      // 处理工具调用
      if (delta.tool_calls && Array.isArray(delta.tool_calls)) {
        for (const tc of delta.tool_calls) {
          const index = tc.index ?? 0;

          if (!toolCallBuffers.has(index)) {
            toolCallBuffers.set(index, {
              id: tc.id || `magi_chat_call_${Date.now().toString(36)}_${index}`,
              name: tc.function?.name || '',
              argumentsText: '',
              argumentsDeltaMode: 'unknown',
            });
            const buffer = toolCallBuffers.get(index)!;
            onEvent({
              type: 'tool_call_start',
              toolCall: { id: buffer.id, name: buffer.name },
            });
          }

          const buffer = toolCallBuffers.get(index)!;
          if (tc.id) buffer.id = tc.id;
          if (tc.function?.name) buffer.name = tc.function.name;

          if (tc.function?.arguments) {
            const { delta: argDelta, mode: argMode } = normalizeStreamDelta(
              tc.function.arguments,
              buffer.argumentsText,
              buffer.argumentsDeltaMode,
            );
            buffer.argumentsDeltaMode = argMode;
            if (argDelta) {
              buffer.argumentsText += argDelta;
              onEvent({
                type: 'tool_call_delta',
                toolCall: { id: buffer.id, name: buffer.name },
                content: argDelta,
              });
            }
          }
        }
      }

      // 处理 finish_reason
      const finishReason = chunk?.choices?.[0]?.finish_reason;
      if (finishReason) {
        stopReason = this.mapFinishReason(finishReason, toolCallBuffers.size > 0);
      }
    }

    // 关闭内容流
    if (emittedContentStart) {
      onEvent({ type: 'content_end' });
    }

    // 关闭所有工具调用
    const toolCalls: ToolCall[] = [];
    for (const [, buffer] of toolCallBuffers.entries()) {
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
          name: buffer.name,
          arguments: parsedArgs.value,
        },
      });
      toolCalls.push({
        id: buffer.id,
        name: buffer.name,
        arguments: parsedArgs.value,
        argumentParseError: parsedArgs.error,
        rawArguments: parsedArgs.rawText,
      });
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

  // ============================================================================
  // 消息转换
  // ============================================================================

  private convertToChatMessages(params: LLMMessageParams): any[] {
    const sanitizedMessages = sanitizeToolOrder(params.messages);
    const messages: any[] = [];

    if (params.systemPrompt) {
      messages.push({ role: 'system', content: params.systemPrompt });
    }

    for (const msg of sanitizedMessages) {
      if (msg.role === 'system') {
        messages.push({ role: 'system', content: typeof msg.content === 'string' ? msg.content : '' });
        continue;
      }

      if (typeof msg.content === 'string') {
        messages.push({ role: msg.role, content: msg.content });
        continue;
      }

      const textParts: any[] = [];
      const toolCalls: any[] = [];
      const toolResults: any[] = [];

      for (const block of msg.content) {
        const b = block as any;
        switch (b.type) {
          case 'text':
            textParts.push({ type: 'text', text: b.text || '' });
            break;
          case 'image':
            textParts.push({
              type: 'image_url',
              image_url: {
                url: `data:${b.source?.media_type || 'image/png'};base64,${b.source?.data || ''}`,
                detail: 'auto',
              },
            });
            break;
          case 'tool_use':
            toolCalls.push({
              id: b.id || `magi_chat_call_${Date.now().toString(36)}_${toolCalls.length}`,
              type: 'function',
              function: {
                name: b.name || '',
                arguments: typeof b.input === 'string' ? b.input : JSON.stringify(b.input ?? {}),
              },
            });
            break;
          case 'tool_result': {
            const normalized = normalizeToolResultBlock(
              b,
              `chat-completions:${msg.role}`,
              this.config.provider,
              this.config.model,
            );
            if (normalized) {
              toolResults.push({
                role: 'tool',
                tool_call_id: normalized.toolUseId,
                content: toOpenAIToolMessageContent({
                  content: normalized.content,
                  isError: normalized.isError,
                }),
              });
            }
            break;
          }
        }
      }

      if (msg.role === 'assistant' && toolCalls.length > 0) {
        const assistantMsg: any = { role: 'assistant' };
        const plainText = textParts
          .filter((p: any) => p.type === 'text')
          .map((p: any) => p.text)
          .join('\n')
          .trim();
        assistantMsg.content = plainText || null;
        assistantMsg.tool_calls = toolCalls;
        messages.push(assistantMsg);
      } else if (textParts.length > 0) {
        if (textParts.length === 1 && textParts[0].type === 'text') {
          messages.push({ role: msg.role, content: textParts[0].text });
        } else {
          messages.push({ role: msg.role, content: textParts });
        }
      }

      for (const toolResult of toolResults) {
        messages.push(toolResult);
      }
    }

    return messages;
  }

  // ============================================================================
  // 工具映射
  // ============================================================================

  private mapToolsForChat(tools?: ToolDefinition[]): any[] | undefined {
    if (!tools || tools.length === 0) return undefined;
    return tools.map(tool => ({
      type: 'function',
      function: {
        name: tool.name,
        description: tool.description || 'No description available',
        parameters: sanitizeSchema(tool.input_schema),
      },
    }));
  }

  private mapToolChoiceForChat(choice?: LLMMessageParams['toolChoice']): any | undefined {
    if (!choice) return undefined;
    if (typeof choice === 'string') {
      if (choice === 'auto' || choice === 'none' || choice === 'required') return choice;
      return undefined;
    }
    if (choice.type === 'any') return 'required';
    if (choice.type === 'tool' && choice.name) {
      return { type: 'function', function: { name: choice.name } };
    }
    return undefined;
  }

  // ============================================================================
  // 响应解析
  // ============================================================================

  private parseChatResponse(response: any): LLMResponse {
    const choice = response?.choices?.[0];
    const message = choice?.message;
    const content = message?.content || '';

    const toolCalls: ToolCall[] = [];
    if (message?.tool_calls && Array.isArray(message.tool_calls)) {
      for (const tc of message.tool_calls) {
        const parsedArgs = parseToolArguments(
          tc.function?.arguments,
          `sync:${tc.function?.name || tc.id}`,
          this.config.provider,
          this.config.model,
        );
        toolCalls.push({
          id: tc.id || `magi_chat_call_sync_${Date.now().toString(36)}_${toolCalls.length}`,
          name: tc.function?.name || '',
          arguments: parsedArgs.value,
          argumentParseError: parsedArgs.error,
          rawArguments: parsedArgs.rawText,
        });
      }
    }

    return {
      content,
      toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
      usage: normalizeOpenAIUsage(response?.usage),
      stopReason: this.mapFinishReason(choice?.finish_reason, toolCalls.length > 0),
    };
  }

  private mapFinishReason(reason: string | undefined, hasToolCalls: boolean): LLMResponse['stopReason'] {
    if (hasToolCalls) return 'tool_use';
    switch (reason) {
      case 'tool_calls': return 'tool_use';
      case 'length': return 'max_tokens';
      case 'stop': return 'end_turn';
      default: return 'end_turn';
    }
  }

  // ============================================================================
  // 工具兼容性降级
  // ============================================================================

  private async retryWithToolElimination(
    requestParams: any, signal: AbortSignal | undefined, originalError: any,
  ): Promise<any> {
    const allTools: any[] = requestParams.tools;
    logger.warn('Chat Completions 400 工具不兼容，启动排除', {
      model: this.config.model,
      toolCount: allTools.length,
      error: originalError?.message?.substring(0, 200),
    }, LogCategory.LLM);

    const compatibleTools = await this.findCompatibleTools(
      allTools,
      (tools) => {
        requestParams.tools = tools.length > 0 ? tools : undefined;
        if (!requestParams.tools) delete requestParams.tool_choice;
        return this.openaiClient.chat.completions.create(requestParams, { signal });
      },
    );

    requestParams.tools = compatibleTools.length > 0 ? compatibleTools : undefined;
    if (!requestParams.tools) delete requestParams.tool_choice;
    return this.openaiClient.chat.completions.create(requestParams, { signal });
  }

  private async retryStreamWithToolElimination(
    requestParams: any, signal?: AbortSignal, originalError?: any,
  ): Promise<any> {
    const allTools: any[] = requestParams.tools;
    logger.warn('Chat Completions 400(stream) 工具不兼容，启动排除', {
      model: this.config.model,
      toolCount: allTools.length,
      error: originalError?.message?.substring(0, 200),
    }, LogCategory.LLM);

    const createStream = (tools: any[]) => {
      requestParams.tools = tools.length > 0 ? tools : undefined;
      if (!requestParams.tools) delete requestParams.tool_choice;
      return this.openaiClient.chat.completions.create(
        { ...requestParams, stream: true },
        { signal },
      );
    };

    const compatibleTools = await this.findCompatibleTools(allTools, createStream);

    requestParams.tools = compatibleTools.length > 0 ? compatibleTools : undefined;
    if (!requestParams.tools) delete requestParams.tool_choice;
    return this.openaiClient.chat.completions.create(
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
            toolName: tools[0]?.function?.name || 'unknown',
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
    const [compatible1, compatible2] = await Promise.all([
      this.findCompatibleTools(tools.slice(0, mid), tryRequest),
      this.findCompatibleTools(tools.slice(mid), tryRequest),
    ]);

    return [...compatible1, ...compatible2];
  }
}

