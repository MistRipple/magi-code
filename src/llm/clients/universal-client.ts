/**
 * 通用 LLM 客户端
 * 根据 provider 自动选择正确的 API 格式（OpenAI 或 Anthropic）
 */

import Anthropic from '@anthropic-ai/sdk';
import OpenAI from 'openai';
import { BaseLLMClient } from './base-client';
import { LLMConfig } from '../../types/agent-types';
import {
  LLMMessageParams,
  LLMResponse,
  LLMStreamChunk,
  ToolCall,
  ToolDefinition,
  ContentBlock,
} from '../types';
import { logger, LogCategory } from '../../logging';

/**
 * 通用 LLM 客户端
 * 支持 OpenAI 和 Anthropic API
 */
export class UniversalLLMClient extends BaseLLMClient {
  private anthropicClient?: Anthropic;
  private openaiClient?: OpenAI;

  constructor(config: LLMConfig) {
    super(config);
    this.validateConfig();
    this.initializeClient();
  }

  /**
   * 初始化客户端
   */
  private initializeClient(): void {
    if (this.config.provider === 'anthropic') {
      this.anthropicClient = new Anthropic({
        apiKey: this.config.apiKey,
        baseURL: this.config.baseUrl,
      });
    } else if (this.config.provider === 'openai') {
      // OpenAI SDK 需要 baseURL 包含 /v1 路径
      let baseURL = this.config.baseUrl;
      if (baseURL && !baseURL.endsWith('/v1')) {
        baseURL = baseURL.replace(/\/$/, '') + '/v1';
      }

      this.openaiClient = new OpenAI({
        apiKey: this.config.apiKey,
        baseURL: baseURL,
      });

      logger.info('OpenAI client initialized', {
        originalBaseUrl: this.config.baseUrl,
        finalBaseUrl: baseURL,
        model: this.config.model
      }, LogCategory.LLM);
    } else {
      throw new Error(`Unsupported provider: ${this.config.provider}`);
    }
  }

  /**
   * 发送消息（非流式）
   */
  async sendMessage(params: LLMMessageParams): Promise<LLMResponse> {
    this.logRequest(params);

    try {
      if (this.config.provider === 'anthropic') {
        return await this.sendAnthropicMessage(params);
      } else {
        return await this.sendOpenAIMessage(params);
      }
    } catch (error) {
      this.logError(error, 'sendMessage');
      throw error;
    }
  }

  /**
   * 发送消息（流式）
   */
  async streamMessage(
    params: LLMMessageParams,
    onChunk: (chunk: LLMStreamChunk) => void
  ): Promise<LLMResponse> {
    this.logRequest({ ...params, stream: true });

    try {
      if (this.config.provider === 'anthropic') {
        return await this.streamAnthropicMessage(params, onChunk);
      } else {
        return await this.streamOpenAIMessage(params, onChunk);
      }
    } catch (error) {
      this.logError(error, 'streamMessage');
      throw error;
    }
  }

  // ============================================================================
  // Anthropic 实现
  // ============================================================================

  /**
   * 清理工具定义，确保符合 Anthropic API 要求
   */
  private sanitizeToolsForAnthropic(tools?: ToolDefinition[]): any[] | undefined {
    if (!tools || tools.length === 0) {
      return undefined;
    }

    return tools.map(tool => {
      const sanitized: any = {
        name: tool.name,
        description: tool.description || 'No description available',
        input_schema: this.sanitizeSchema(tool.input_schema)
      };
      return sanitized;
    });
  }

  /**
   * 清理 JSON Schema，移除不兼容的属性
   */
  private sanitizeSchema(schema: any): any {
    if (!schema || typeof schema !== 'object') {
      return { type: 'object', properties: {} };
    }

    const sanitized: any = {
      type: schema.type || 'object'
    };

    // 处理 properties
    if (schema.properties && typeof schema.properties === 'object') {
      sanitized.properties = {};
      for (const [key, value] of Object.entries(schema.properties)) {
        sanitized.properties[key] = this.sanitizeProperty(value);
      }
    } else {
      sanitized.properties = {};
    }

    // 处理 required - 只在有值时添加
    if (Array.isArray(schema.required) && schema.required.length > 0) {
      // 过滤出实际存在于 properties 中的 required 字段
      const validRequired = schema.required.filter(
        (r: string) => sanitized.properties[r] !== undefined
      );
      if (validRequired.length > 0) {
        sanitized.required = validRequired;
      }
    }

    return sanitized;
  }

  /**
   * 清理属性定义
   */
  private sanitizeProperty(prop: any): any {
    if (!prop || typeof prop !== 'object') {
      return { type: 'string' };
    }

    const sanitized: any = {};

    // 复制基本字段
    if (prop.type) {
      sanitized.type = prop.type;
    } else {
      sanitized.type = 'string';
    }

    if (prop.description) {
      sanitized.description = String(prop.description);
    }

    // 处理枚举
    if (Array.isArray(prop.enum) && prop.enum.length > 0) {
      sanitized.enum = prop.enum;
    }

    // 处理默认值
    if (prop.default !== undefined) {
      sanitized.default = prop.default;
    }

    // 处理数组类型
    if (prop.type === 'array' && prop.items) {
      sanitized.items = this.sanitizeProperty(prop.items);
    }

    // 处理对象类型
    if (prop.type === 'object' && prop.properties) {
      sanitized.properties = {};
      for (const [key, value] of Object.entries(prop.properties)) {
        sanitized.properties[key] = this.sanitizeProperty(value);
      }
      if (Array.isArray(prop.required) && prop.required.length > 0) {
        sanitized.required = prop.required;
      }
    }

    return sanitized;
  }

  private async sendAnthropicMessage(params: LLMMessageParams): Promise<LLMResponse> {
    if (!this.anthropicClient) {
      throw new Error('Anthropic client not initialized');
    }

    const { messages, systemPrompt } = this.convertToAnthropicFormat(params);
    const sanitizedTools = this.sanitizeToolsForAnthropic(params.tools);

    const response = await this.anthropicClient.messages.create({
      model: this.config.model,
      max_tokens: params.maxTokens || 4096,
      temperature: params.temperature,
      system: systemPrompt,
      messages,
      tools: sanitizedTools as any,
    });

    const result = this.parseAnthropicResponse(response);
    this.logResponse(result);
    return result;
  }

  private async streamAnthropicMessage(
    params: LLMMessageParams,
    onChunk: (chunk: LLMStreamChunk) => void
  ): Promise<LLMResponse> {
    if (!this.anthropicClient) {
      throw new Error('Anthropic client not initialized');
    }

    const { messages, systemPrompt } = this.convertToAnthropicFormat(params);
    const sanitizedTools = this.sanitizeToolsForAnthropic(params.tools);

    const stream = await this.anthropicClient.messages.create({
      model: this.config.model,
      max_tokens: params.maxTokens || 4096,
      temperature: params.temperature,
      system: systemPrompt,
      messages,
      tools: sanitizedTools as any,
      stream: true,
    });

    let fullContent = '';
    const toolCallBuffers = new Map<string, { id: string; name?: string; argumentsText: string }>();
    let usage = { inputTokens: 0, outputTokens: 0 };
    let stopReason: LLMResponse['stopReason'] = 'end_turn';

    for await (const event of stream) {
      if (event.type === 'content_block_start') {
        if (event.content_block.type === 'text') {
          onChunk({ type: 'content_start' });
        } else if (event.content_block.type === 'tool_use') {
          const toolId = event.content_block.id || '';
          if (toolId) {
            toolCallBuffers.set(toolId, {
              id: toolId,
              name: event.content_block.name,
              argumentsText: '',
            });
          }
          onChunk({
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
          onChunk({ type: 'content_delta', content: event.delta.text });
        } else if (event.delta.type === 'input_json_delta') {
          const lastTool = [...toolCallBuffers.values()].slice(-1)[0];
          if (lastTool) {
            lastTool.argumentsText += event.delta.partial_json || '';
          }
          let safeArgs: Record<string, any> = {};
          if (event.delta.partial_json) {
            try {
              safeArgs = JSON.parse(event.delta.partial_json);
            } catch {
              safeArgs = {};
            }
          }
          onChunk({
            type: 'tool_call_delta',
            toolCall: { arguments: safeArgs },
          });
        }
      } else if (event.type === 'content_block_stop') {
        onChunk({ type: 'content_end' });
      } else if (event.type === 'message_delta') {
        if (event.usage) {
          usage.outputTokens = event.usage.output_tokens;
        }
        if (event.delta.stop_reason) {
          stopReason = this.mapAnthropicStopReason(event.delta.stop_reason);
        }
      } else if (event.type === 'message_start') {
        if (event.message.usage) {
          usage.inputTokens = event.message.usage.input_tokens;
        }
      }
    }

    const toolCalls: ToolCall[] = [];
    for (const tool of toolCallBuffers.values()) {
      if (!tool.id) continue;
      let parsedArgs: Record<string, any> = {};
      if (tool.argumentsText) {
        try {
          parsedArgs = JSON.parse(tool.argumentsText);
        } catch {
          parsedArgs = {};
        }
      }
      toolCalls.push({
        id: tool.id,
        name: tool.name || '',
        arguments: parsedArgs,
      });
    }

    const result: LLMResponse = {
      content: fullContent,
      toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
      usage,
      stopReason,
    };

    this.logResponse(result);
    return result;
  }

  private convertToAnthropicFormat(params: LLMMessageParams): {
    messages: Anthropic.MessageParam[];
    systemPrompt?: string;
  } {
    let systemPrompt: string | undefined;
    const messages: Anthropic.MessageParam[] = [];

    for (const msg of params.messages) {
      if (msg.role === 'system') {
        systemPrompt = typeof msg.content === 'string' ? msg.content : '';
      } else {
        messages.push({
          role: msg.role,
          content: typeof msg.content === 'string' ? msg.content : msg.content as any,
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

    return {
      content,
      toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
      usage: {
        inputTokens: response.usage.input_tokens,
        outputTokens: response.usage.output_tokens,
      },
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

  // ============================================================================
  // OpenAI 实现
  // ============================================================================

  private async sendOpenAIMessage(params: LLMMessageParams): Promise<LLMResponse> {
    if (!this.openaiClient) {
      throw new Error('OpenAI client not initialized');
    }

    const messages = this.convertToOpenAIFormat(params);

    const response = await this.openaiClient.chat.completions.create({
      model: this.config.model,
      messages,
      max_tokens: params.maxTokens,
      temperature: params.temperature,
      tools: params.tools as any,
    });

    // 添加调试日志
    logger.info('OpenAI API response received', {
      model: this.config.model,
      hasChoices: !!response.choices,
      choicesLength: response.choices?.length || 0,
      firstChoice: response.choices?.[0] ? {
        hasMessage: !!response.choices[0].message,
        finishReason: response.choices[0].finish_reason
      } : null
    }, LogCategory.LLM);

    const result = this.parseOpenAIResponse(response);
    this.logResponse(result);
    return result;
  }

  private async streamOpenAIMessage(
    params: LLMMessageParams,
    onChunk: (chunk: LLMStreamChunk) => void
  ): Promise<LLMResponse> {
    if (!this.openaiClient) {
      throw new Error('OpenAI client not initialized');
    }

    const messages = this.convertToOpenAIFormat(params);

    const stream = await this.openaiClient.chat.completions.create({
      model: this.config.model,
      messages,
      max_tokens: params.maxTokens,
      temperature: params.temperature,
      tools: params.tools as any,
      stream: true,
      stream_options: { include_usage: true },
    });

    let fullContent = '';
    const toolCallBuffers = new Map<string, { id: string; name?: string; argumentsText: string }>();
    let usage = { inputTokens: 0, outputTokens: 0 };
    let stopReason: LLMResponse['stopReason'] = 'end_turn';

    for await (const chunk of stream) {
      const delta = chunk.choices[0]?.delta;

      if (delta?.content) {
        fullContent += delta.content;
        onChunk({ type: 'content_delta', content: delta.content });
      }

      if (delta?.tool_calls) {
        for (const toolCall of delta.tool_calls) {
          const toolId = toolCall.id || toolCall.index?.toString() || '';
          if (!toolCallBuffers.has(toolId)) {
            toolCallBuffers.set(toolId, {
              id: toolId,
              name: toolCall.function?.name,
              argumentsText: '',
            });
          }
          const buffer = toolCallBuffers.get(toolId);
          if (buffer && toolCall.function?.arguments) {
            buffer.argumentsText += toolCall.function.arguments;
          }
          if (toolCall.function?.name) {
            onChunk({
              type: 'tool_call_delta',
              toolCall: {
                id: toolId,
                name: toolCall.function.name,
                arguments: {},
              },
            });
          }
        }
      }

      if (chunk.choices[0]?.finish_reason) {
        stopReason = this.mapOpenAIStopReason(chunk.choices[0].finish_reason);
      }

      if (chunk.usage) {
        usage.inputTokens = chunk.usage.prompt_tokens || 0;
        usage.outputTokens = chunk.usage.completion_tokens || 0;
      }
    }

    const toolCalls: ToolCall[] = [];
    for (const tool of toolCallBuffers.values()) {
      if (!tool.id) continue;
      let parsedArgs: Record<string, any> = {};
      if (tool.argumentsText) {
        try {
          parsedArgs = JSON.parse(tool.argumentsText);
        } catch {
          parsedArgs = {};
        }
      }
      toolCalls.push({
        id: tool.id,
        name: tool.name || '',
        arguments: parsedArgs,
      });
    }

    const result: LLMResponse = {
      content: fullContent,
      toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
      usage,
      stopReason,
    };

    this.logResponse(result);
    return result;
  }

  private convertToOpenAIFormat(params: LLMMessageParams): OpenAI.ChatCompletionMessageParam[] {
    const messages: OpenAI.ChatCompletionMessageParam[] = [];

    for (const msg of params.messages) {
      if (typeof msg.content === 'string') {
        messages.push({
          role: msg.role,
          content: msg.content,
        } as OpenAI.ChatCompletionMessageParam);
      } else {
        // 处理复杂内容块
        messages.push({
          role: msg.role,
          content: msg.content.map((block) => {
            if (block.type === 'text') {
              return { type: 'text', text: block.text };
            }
            return block;
          }) as any,
        } as OpenAI.ChatCompletionMessageParam);
      }
    }

    if (params.systemPrompt) {
      messages.unshift({
        role: 'system',
        content: params.systemPrompt,
      });
    }

    return messages;
  }

  private parseOpenAIResponse(response: OpenAI.ChatCompletion): LLMResponse {
    // ✅ FIX: 检查 choices 数组是否存在且不为空
    if (!response.choices || response.choices.length === 0) {
      // 提供更详细的错误信息
      const errorMsg = `OpenAI API returned empty choices array. This usually means the model name is invalid or the API returned an error. Model: ${this.config.model}`;
      logger.error('OpenAI response parsing failed', {
        model: this.config.model,
        provider: this.config.provider,
        hasChoices: !!response.choices,
        choicesLength: response.choices?.length || 0,
        response: JSON.stringify(response)
      }, LogCategory.LLM);
      throw new Error(errorMsg);
    }

    const choice = response.choices[0];
    const message = choice.message;

    const toolCalls: ToolCall[] = [];
    if (message.tool_calls) {
      for (const toolCall of message.tool_calls) {
        if (toolCall.type === 'function') {
          toolCalls.push({
            id: toolCall.id,
            name: toolCall.function.name,
            arguments: JSON.parse(toolCall.function.arguments),
          });
        }
      }
    }

    return {
      content: message.content || '',
      toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
      usage: {
        inputTokens: response.usage?.prompt_tokens || 0,
        outputTokens: response.usage?.completion_tokens || 0,
      },
      stopReason: this.mapOpenAIStopReason(choice.finish_reason),
    };
  }

  private mapOpenAIStopReason(reason: string): LLMResponse['stopReason'] {
    switch (reason) {
      case 'stop':
        return 'end_turn';
      case 'length':
        return 'max_tokens';
      case 'tool_calls':
        return 'tool_use';
      default:
        return 'end_turn';
    }
  }
}
