import Anthropic from '@anthropic-ai/sdk';
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
  isChunkParseError,
  normalizeAnthropicUsage,
  normalizeStreamDelta,
  normalizeToolResultBlock,
  parseToolArguments,
  sanitizeSchema,
} from './protocol-utils';

const PROFILE = resolveProviderProtocolProfile('anthropic');

export class AnthropicMessagesProtocolAdapter implements ProviderProtocolAdapter {
  readonly provider = PROFILE.provider;
  readonly protocol = PROFILE.protocol;
  readonly capabilities = PROFILE.capabilities;

  constructor(
    private readonly config: LLMConfig,
    private readonly anthropicClient: Anthropic,
  ) {}

  async send(request: LLMMessageParams): Promise<LLMResponse> {
    const { messages, systemPrompt } = this.convertToAnthropicFormat(request);
    const sanitizedTools = this.sanitizeToolsForAnthropic(request.tools);
    const supportsThinking = this.shouldEnableThinking();

    const requestParams: any = {
      model: this.config.model,
      max_tokens: supportsThinking
        ? Math.max(request.maxTokens || 16000, 16000)
        : (request.maxTokens || 4096),
      temperature: request.temperature,
      system: systemPrompt,
      messages,
      tools: sanitizedTools as any,
    };

    const anthropicToolChoice = this.mapToolChoiceForAnthropic(request.toolChoice);
    if (anthropicToolChoice) {
      requestParams.tool_choice = anthropicToolChoice;
    }

    if (supportsThinking) {
      requestParams.thinking = {
        type: 'enabled',
        budget_tokens: 10000,
      };
      delete requestParams.temperature;
    }

    const response = await this.anthropicClient.messages.create(requestParams);
    return this.parseAnthropicResponse(response as Anthropic.Message);
  }

  async stream(
    request: LLMMessageParams,
    onEvent: (event: LLMStreamChunk) => void,
  ): Promise<LLMResponse> {
    const { messages, systemPrompt } = this.convertToAnthropicFormat(request);
    const sanitizedTools = this.sanitizeToolsForAnthropic(request.tools);
    const supportsThinking = this.shouldEnableThinking();

    const requestParams: any = {
      model: this.config.model,
      max_tokens: supportsThinking
        ? Math.max(request.maxTokens || 16000, 16000)
        : (request.maxTokens || 4096),
      temperature: request.temperature,
      system: systemPrompt,
      messages,
      tools: sanitizedTools as any,
      stream: true as const,
    };

    const anthropicToolChoice = this.mapToolChoiceForAnthropic(request.toolChoice);
    if (anthropicToolChoice) {
      requestParams.tool_choice = anthropicToolChoice;
    }

    if (supportsThinking) {
      requestParams.thinking = {
        type: 'enabled',
        budget_tokens: 10000,
      };
      delete requestParams.temperature;
      logger.debug('Anthropic thinking enabled', {
        model: this.config.model,
        budgetTokens: 10000,
      }, LogCategory.LLM);
    }

    const stream = await this.anthropicClient.messages.create(
      {
        ...requestParams,
        stream: true,
      } as any,
      {
        signal: request.signal,
      },
    ) as unknown as AsyncIterable<any>;

    let fullContent = '';
    const toolCallBuffers = new Map<string, {
      id: string;
      name?: string;
      argumentsText: string;
      argumentsDeltaMode: 'unknown' | 'delta' | 'cumulative';
    }>();
    const contentBlockTypes = new Map<number, string>();
    const toolIndexToId = new Map<number, string>();

    let usage: {
      inputTokens: number;
      outputTokens: number;
      cacheReadTokens?: number;
      cacheWriteTokens?: number;
    } = { inputTokens: 0, outputTokens: 0 };
    let stopReason: LLMResponse['stopReason'] = 'end_turn';

    const iterator = (stream as any)[Symbol.asyncIterator]();
    while (true) {
      let event: any;
      try {
        const result = await iterator.next();
        if (result.done) break;
        event = result.value;
      } catch (iterError: any) {
        if (isChunkParseError(iterError)) {
          logger.warn('Anthropic stream chunk 底层解析失败，跳过此残片', {
            model: this.config.model,
            provider: this.config.provider,
            error: iterError?.message?.substring(0, 200),
          }, LogCategory.LLM);
          continue;
        }
        throw iterError;
      }

      if (event.type === 'content_block_start') {
        if (typeof event.index === 'number' && event.content_block?.type) {
          contentBlockTypes.set(event.index, event.content_block.type);
        }

        if (event.content_block.type === 'text') {
          onEvent({ type: 'content_start' });
        } else if (event.content_block.type === 'thinking') {
          onEvent({ type: 'thinking', thinking: '' });
        } else if (event.content_block.type === 'tool_use') {
          const toolId = event.content_block.id || '';
          if (toolId) {
            toolCallBuffers.set(toolId, {
              id: toolId,
              name: event.content_block.name,
              argumentsText: '',
              argumentsDeltaMode: 'unknown',
            });
            if (typeof event.index === 'number') {
              toolIndexToId.set(event.index, toolId);
            }
          }
          onEvent({
            type: 'tool_call_start',
            toolCall: {
              id: event.content_block.id,
              name: event.content_block.name,
              arguments: {},
            },
          });
        }
      } else if (event.type === 'content_block_delta') {
        if (event.delta.type === 'text_delta') {
          fullContent += event.delta.text;
          onEvent({ type: 'content_delta', content: event.delta.text });
        } else if (event.delta.type === 'thinking_delta') {
          const thinkingContent = (event.delta as any).thinking || '';
          if (thinkingContent) {
            onEvent({ type: 'thinking', thinking: thinkingContent });
          }
        } else if (event.delta.type === 'input_json_delta') {
          let tool: { id: string; name?: string; argumentsText: string; argumentsDeltaMode: 'unknown' | 'delta' | 'cumulative' } | undefined;
          if (typeof event.index === 'number') {
            const indexedToolId = toolIndexToId.get(event.index);
            if (indexedToolId) {
              tool = toolCallBuffers.get(indexedToolId);
            }
          }
          if (!tool) {
            tool = [...toolCallBuffers.values()].slice(-1)[0];
          }

          if (tool) {
            const incomingArgs = event.delta.partial_json || '';
            const normalizedArgs = normalizeStreamDelta(
              incomingArgs,
              tool.argumentsText,
              tool.argumentsDeltaMode,
            );
            tool.argumentsDeltaMode = normalizedArgs.mode;
            tool.argumentsText += normalizedArgs.delta;
          }

          let partialParsedArgs: Record<string, any> | undefined;
          if (tool?.argumentsText) {
            try {
              partialParsedArgs = JSON.parse(tool.argumentsText);
            } catch {
              // 增量解析失败是正常的
            }
          }

          onEvent({
            type: 'tool_call_delta',
            toolCall: {
              id: tool?.id,
              name: tool?.name,
              arguments: partialParsedArgs,
            },
          });
        }
      } else if (event.type === 'content_block_stop') {
        if (typeof event.index === 'number' && contentBlockTypes.get(event.index) === 'text') {
          onEvent({ type: 'content_end' });
        }
      } else if (event.type === 'message_delta') {
        if (event.usage) {
          const normalizedUsage = normalizeAnthropicUsage(event.usage);
          usage.outputTokens = normalizedUsage.outputTokens;
          usage.cacheReadTokens = normalizedUsage.cacheReadTokens;
          usage.cacheWriteTokens = normalizedUsage.cacheWriteTokens;
          onEvent({
            type: 'usage',
            usage: {
              outputTokens: normalizedUsage.outputTokens,
              cacheReadTokens: normalizedUsage.cacheReadTokens,
              cacheWriteTokens: normalizedUsage.cacheWriteTokens,
            },
          });
        }
        if (event.delta.stop_reason) {
          stopReason = this.mapAnthropicStopReason(event.delta.stop_reason);
        }
      } else if (event.type === 'message_start') {
        if (event.message.usage) {
          const normalizedUsage = normalizeAnthropicUsage(event.message.usage);
          usage.inputTokens = normalizedUsage.inputTokens;
          usage.cacheReadTokens = normalizedUsage.cacheReadTokens;
          usage.cacheWriteTokens = normalizedUsage.cacheWriteTokens;
          onEvent({
            type: 'usage',
            usage: {
              inputTokens: normalizedUsage.inputTokens,
              cacheReadTokens: normalizedUsage.cacheReadTokens,
              cacheWriteTokens: normalizedUsage.cacheWriteTokens,
            },
          });
        }
      }
    }

    const toolCalls: ToolCall[] = [];
    for (const tool of toolCallBuffers.values()) {
      if (!tool.id) continue;

      const parsedArgs = parseToolArguments(
        tool.argumentsText || '',
        `stream:${tool.name || tool.id}`,
        this.config.provider,
        this.config.model,
      );

      toolCalls.push({
        id: tool.id,
        name: tool.name || '',
        arguments: parsedArgs.value,
        argumentParseError: parsedArgs.error,
        rawArguments: parsedArgs.rawText,
      });
    }

    return {
      content: fullContent,
      toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
      usage,
      stopReason,
    };
  }

  private sanitizeToolsForAnthropic(tools?: ToolDefinition[]): any[] | undefined {
    if (!tools || tools.length === 0) {
      return undefined;
    }

    return tools.map(tool => ({
      name: tool.name,
      description: tool.description || 'No description available',
      input_schema: sanitizeSchema(tool.input_schema),
    }));
  }

  private mapToolChoiceForAnthropic(choice?: LLMMessageParams['toolChoice']): any | undefined {
    if (!choice) return undefined;
    if (typeof choice === 'string') {
      if (choice === 'required') return { type: 'any' };
      return undefined;
    }
    if (choice.type === 'any') {
      return { type: 'any' };
    }
    if (choice.type === 'tool' && choice.name) {
      return { type: 'tool', name: choice.name };
    }
    return undefined;
  }

  private shouldEnableThinking(): boolean {
    return this.config.enableThinking === true;
  }

  private convertToAnthropicFormat(params: LLMMessageParams): {
    messages: Anthropic.MessageParam[];
    systemPrompt?: string;
  } {
    let systemPrompt: string | undefined;
    const messages: Anthropic.MessageParam[] = [];

    const sanitizedMessages = sanitizeToolOrder(params.messages);

    for (const msg of sanitizedMessages) {
      if (msg.role === 'system') {
        systemPrompt = typeof msg.content === 'string' ? msg.content : '';
      } else {
        const content = typeof msg.content === 'string'
          ? msg.content
          : msg.content
            .map((block) => {
              if (block.type !== 'tool_result') {
                return block as any;
              }
              const normalized = normalizeToolResultBlock(
                block as any,
                `anthropic:${msg.role}`,
                this.config.provider,
                this.config.model,
              );
              if (!normalized) {
                return null;
              }
              return {
                type: 'tool_result',
                tool_use_id: normalized.toolUseId,
                content: normalized.content,
                is_error: normalized.isError,
              } as any;
            })
            .filter((block): block is any => block !== null) as any;
        messages.push({
          role: msg.role,
          content,
        });
      }
    }

    if (params.systemPrompt) {
      systemPrompt = params.systemPrompt;
    }

    return { messages, systemPrompt };
  }

  private parseAnthropicResponse(response: Anthropic.Message): LLMResponse {
    let content = '';
    const toolCalls: ToolCall[] = [];

    for (const block of response.content) {
      if (block.type === 'text') {
        content += block.text;
      } else if (block.type === 'tool_use') {
        toolCalls.push({
          id: block.id,
          name: block.name,
          arguments: block.input as Record<string, any>,
        });
      }
    }

    const normalizedUsage = normalizeAnthropicUsage(response.usage);

    return {
      content,
      toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
      usage: normalizedUsage,
      stopReason: this.mapAnthropicStopReason(response.stop_reason),
    };
  }

  private mapAnthropicStopReason(reason: string | null): LLMResponse['stopReason'] {
    switch (reason) {
      case 'end_turn':
        return 'end_turn';
      case 'max_tokens':
        return 'max_tokens';
      case 'tool_use':
        return 'tool_use';
      case 'stop_sequence':
        return 'stop_sequence';
      default:
        return 'end_turn';
    }
  }
}
