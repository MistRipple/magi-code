/**
 * 通用 LLM 客户端
 * 根据 provider 自动选择正确的 API 格式（OpenAI 或 Anthropic）
 */

import Anthropic from '@anthropic-ai/sdk';
import OpenAI from 'openai';
import { BaseLLMClient } from './base-client';
import { LLMConfig } from '../../types/agent-types';
import {
  LLMMessage,
  LLMMessageParams,
  LLMResponse,
  LLMStreamChunk,
  ToolCall,
  ToolDefinition,
  ContentBlock,
} from '../types';
import { logger, LogCategory } from '../../logging';

class NonRetryableError extends Error {
  constructor(message: string, public originalError?: unknown) {
    super(message);
    this.name = 'NonRetryableError';
  }
}

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
   * 快速测试连接（使用 Models API）
   *
   * 调用 /v1/models 端点验证 API Key，不消耗 tokens。
   * 同时检查配置的模型是否在列表中。
   */
  async testConnectionFast(): Promise<{
    success: boolean;
    modelExists?: boolean;
    error?: string;
  }> {
    try {
      // 构建 models API URL
      let modelsUrl = this.config.baseUrl;
      if (!modelsUrl.endsWith('/v1')) {
        modelsUrl = modelsUrl.replace(/\/$/, '') + '/v1';
      }
      modelsUrl += '/models';

      const response = await fetch(modelsUrl, {
        method: 'GET',
        headers: {
          'Authorization': `Bearer ${this.config.apiKey}`,
          'Content-Type': 'application/json',
        },
        signal: AbortSignal.timeout(5000), // 5 秒超时
      });

      if (!response.ok) {
        const status = response.status;
        if (status === 401 || status === 403) {
          return { success: false, error: 'API Key 无效' };
        }
        if (status === 404) {
          // Models API 不支持，使用简化验证
          return { success: true, modelExists: undefined };
        }
        return { success: false, error: `HTTP ${status}` };
      }

      const data = await response.json();
      const models = data?.data || [];
      const modelExists = models.some((m: any) => m.id === this.config.model);

      logger.debug('Fast connection test succeeded', {
        provider: this.config.provider,
        model: this.config.model,
        modelExists,
        modelsCount: models.length,
      }, LogCategory.LLM);

      return { success: true, modelExists };
    } catch (error: any) {
      const message = error.message || String(error);
      if (message.includes('timeout') || message.includes('TimeoutError')) {
        return { success: false, error: '连接超时' };
      }
      if (message.includes('ECONNREFUSED') || message.includes('ENOTFOUND')) {
        return { success: false, error: '网络连接失败' };
      }
      logger.error('Fast connection test failed', { error: message }, LogCategory.LLM);
      return { success: false, error: message };
    }
  }

  /**
   * 发送消息（非流式）
   */
  async sendMessage(params: LLMMessageParams): Promise<LLMResponse> {
    this.logRequest(params);

    return this.withRetry(async () => {
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
    }, 'sendMessage');
  }

  /**
   * 发送消息（流式）
   */
  async streamMessage(
    params: LLMMessageParams,
    onChunk: (chunk: LLMStreamChunk) => void
  ): Promise<LLMResponse> {
    this.logRequest({ ...params, stream: true });

    let hasReceivedData = false;
    const wrappedOnChunk = (chunk: LLMStreamChunk) => {
      hasReceivedData = true;
      onChunk(chunk);
    };

    return this.withRetry(async () => {
      try {
        if (this.config.provider === 'anthropic') {
          return await this.streamAnthropicMessage(params, wrappedOnChunk);
        } else {
          return await this.streamOpenAIMessage(params, wrappedOnChunk);
        }
      } catch (error) {
        // 如果已经收到数据后发生错误，禁止重试，避免内容重复
        if (hasReceivedData) {
          throw new NonRetryableError('Stream interrupted after data received', error);
        }
        this.logError(error, 'streamMessage');
        throw error;
      }
    }, 'streamMessage');
  }

  private async withRetry<T>(fn: () => Promise<T>, context: string): Promise<T> {
    const maxRetries = 3;
    const baseDelayMs = 500;
    for (let attempt = 0; attempt < maxRetries; attempt++) {
      try {
        return await fn();
      } catch (error: any) {
        if (error instanceof NonRetryableError) {
          throw error.originalError || error;
        }
        if (!this.isRetryableError(error) || attempt === maxRetries - 1) {
          throw error;
        }
        const delay = baseDelayMs * Math.pow(2, attempt) + Math.floor(Math.random() * 200);
        this.logError(error, `${context}.retry_${attempt + 1}`);
        await this.sleep(delay);
      }
    }
    throw new Error(`Retry failed: ${context}`);
  }

  private isRetryableError(error: any): boolean {
    const status = error?.status || error?.response?.status;
    if (typeof status === 'number') {
      if (status === 408 || status === 429) return true;
      if (status >= 500 && status <= 599) return true;
    }
    const code = error?.code;
    if (typeof code === 'string') {
      return ['ETIMEDOUT', 'ECONNRESET', 'ENOTFOUND', 'EAI_AGAIN', 'ECONNREFUSED'].includes(code);
    }
    const message = String(error?.message || '');
    return /timeout|timed out|ECONNRESET|ENOTFOUND|EAI_AGAIN|ECONNREFUSED|request ended without sending|stream ended|overloaded/i.test(message);
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  /**
   * 检测是否为 400 状态码错误
   */
  private is400Error(error: any): boolean {
    const status = error?.status || error?.response?.status;
    return status === 400;
  }

  /**
   * 检测是否为 400 工具 schema 不兼容错误
   * Gemini OpenAI 兼容 API 对 JSON Schema 严格校验，
   * MCP 工具的 schema 可能包含不支持的属性导致 400。
   */
  private is400ToolSchemaError(error: any): boolean {
    const status = error?.status || error?.response?.status;
    if (status !== 400) return false;
    const msg = String(error?.message || error?.error?.message || '');
    return /invalid.argument|invalid.*schema|invalid.*tool|invalid.*function/i.test(msg);
  }

  /**
   * 400 工具不兼容容错（非流式）
   *
   * 渐进式降级策略：
   * 1. 二分法排除不兼容工具，保留兼容工具重试
   * 2. 多次失败后才去掉全部工具
   */
  private async retryWithToolElimination(requestParams: any, originalError: any): Promise<any> {
    const allTools: any[] = requestParams.tools;
    logger.warn('400 工具不兼容，启动渐进式排除', {
      model: this.config.model,
      toolCount: allTools.length,
      error: originalError?.message?.substring(0, 200),
    }, LogCategory.LLM);

    // 二分法找出可用工具子集
    const compatibleTools = await this.findCompatibleTools(
      allTools,
      (tools) => {
        requestParams.tools = tools.length > 0 ? tools : undefined;
        if (!requestParams.tools) delete requestParams.tool_choice;
        return this.openaiClient!.chat.completions.create(requestParams);
      },
    );

    requestParams.tools = compatibleTools.length > 0 ? compatibleTools : undefined;
    if (!requestParams.tools) delete requestParams.tool_choice;
    return this.openaiClient!.chat.completions.create(requestParams);
  }

  /**
   * 400 工具不兼容容错（流式）
   */
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
      return (this.openaiClient!.chat.completions.create as any)(
        { ...requestParams, stream: true },
        { signal },
      );
    };

    const compatibleTools = await this.findCompatibleTools(allTools, createStream);

    requestParams.tools = compatibleTools.length > 0 ? compatibleTools : undefined;
    if (!requestParams.tools) delete requestParams.tool_choice;
    return (this.openaiClient!.chat.completions.create as any)(
      { ...requestParams, stream: true },
      { signal },
    );
  }

  /**
   * 二分法查找兼容工具子集
   *
   * 策略：将工具列表对半分，分别尝试，保留不触发 400 的那半。
   * 如果两半都失败则继续递归，直到找到可用子集或全部排除。
   * 最多 log2(N) 轮 API 调用。
   */
  private async findCompatibleTools(
    tools: any[],
    tryRequest: (tools: any[]) => Promise<any>,
  ): Promise<any[]> {
    if (tools.length <= 1) {
      // 单个工具：直接尝试，失败则排除
      if (tools.length === 0) return [];
      try {
        await tryRequest(tools);
        return tools;
      } catch (error: any) {
        if (this.is400ToolSchemaError(error)) {
          logger.warn('排除不兼容工具', {
            toolName: tools[0]?.function?.name || 'unknown',
          }, LogCategory.LLM);
          return [];
        }
        throw error;
      }
    }

    // 先整体尝试
    try {
      await tryRequest(tools);
      return tools;
    } catch (error: any) {
      if (!this.is400ToolSchemaError(error)) throw error;
    }

    // 整体失败 → 对半分
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

  private mapToolsForOpenAI(tools?: ToolDefinition[]): any[] | undefined {
    if (!tools || tools.length === 0) {
      return undefined;
    }

    return tools.map(tool => ({
      type: 'function',
      function: {
        name: tool.name,
        description: tool.description || 'No description available',
        parameters: this.sanitizeSchema(tool.input_schema),
      },
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
      return { type: 'function', function: { name: choice.name } };
    }
    return undefined;
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

  /**
   * 清理 JSON Schema，移除某些 API 不支持的属性
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

    // 注意：不传递 default 属性（Gemini OpenAI 兼容 API 不支持）

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

  /**
   * 检测是否启用 extended thinking
   * 仅在配置中明确启用 enableThinking: true 时才开启
   */
  private shouldEnableThinking(): boolean {
    return this.config.enableThinking === true;
  }

  private async sendAnthropicMessage(params: LLMMessageParams): Promise<LLMResponse> {
    if (!this.anthropicClient) {
      throw new Error('Anthropic client not initialized');
    }

    const { messages, systemPrompt } = this.convertToAnthropicFormat(params);
    const sanitizedTools = this.sanitizeToolsForAnthropic(params.tools);

    // 检测是否启用 extended thinking
    const supportsThinking = this.shouldEnableThinking();

    // 构建请求参数
    const requestParams: any = {
      model: this.config.model,
      max_tokens: supportsThinking ? Math.max(params.maxTokens || 16000, 16000) : (params.maxTokens || 4096),
      temperature: params.temperature,
      system: systemPrompt,
      messages,
      tools: sanitizedTools as any,
    };

    const anthropicToolChoice = this.mapToolChoiceForAnthropic(params.toolChoice);
    if (anthropicToolChoice) {
      requestParams.tool_choice = anthropicToolChoice;
    }

    // 为支持 thinking 的模型添加 thinking 参数
    if (supportsThinking) {
      requestParams.thinking = {
        type: 'enabled',
        budget_tokens: 10000,
      };
      // 注意：启用 thinking 时不能设置 temperature
      delete requestParams.temperature;
    }

    const response = await this.anthropicClient.messages.create(requestParams);

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

    // 检测是否启用 extended thinking
    const supportsThinking = this.shouldEnableThinking();

    // 构建请求参数
    const requestParams: any = {
      model: this.config.model,
      max_tokens: supportsThinking ? Math.max(params.maxTokens || 16000, 16000) : (params.maxTokens || 4096),
      temperature: params.temperature,
      system: systemPrompt,
      messages,
      tools: sanitizedTools as any,
      stream: true as const,
    };

    const anthropicToolChoice = this.mapToolChoiceForAnthropic(params.toolChoice);
    if (anthropicToolChoice) {
      requestParams.tool_choice = anthropicToolChoice;
    }

    // 为支持 thinking 的模型添加 thinking 参数
    if (supportsThinking) {
      requestParams.thinking = {
        type: 'enabled',
        budget_tokens: 10000,
      };
      // 注意：启用 thinking 时不能设置 temperature
      delete requestParams.temperature;
      logger.debug('Anthropic thinking enabled', {
        model: this.config.model,
        budgetTokens: 10000,
      }, LogCategory.LLM);
    }

    const stream = this.anthropicClient.messages.stream(requestParams, {
      signal: params.signal,
    });

    let fullContent = '';
    const toolCallBuffers = new Map<string, { id: string; name?: string; argumentsText: string }>();
    let usage = { inputTokens: 0, outputTokens: 0 };
    let stopReason: LLMResponse['stopReason'] = 'end_turn';

    for await (const event of stream) {
      if (event.type === 'content_block_start') {
        if (event.content_block.type === 'text') {
          onChunk({ type: 'content_start' });
        } else if (event.content_block.type === 'thinking') {
          // Thinking block 开始
          onChunk({ type: 'thinking', thinking: '' });
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
        } else if (event.delta.type === 'thinking_delta') {
          // Thinking delta - 发送 thinking 内容
          const thinkingContent = (event.delta as any).thinking || '';
          if (thinkingContent) {
            onChunk({ type: 'thinking', thinking: thinkingContent });
          }
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
          onChunk({
            type: 'usage',
            usage: { outputTokens: event.usage.output_tokens }
          });
        }
        if (event.delta.stop_reason) {
          stopReason = this.mapAnthropicStopReason(event.delta.stop_reason);
        }
      } else if (event.type === 'message_start') {
        if (event.message.usage) {
          usage.inputTokens = event.message.usage.input_tokens;
          onChunk({
            type: 'usage',
            usage: { inputTokens: event.message.usage.input_tokens }
          });
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

    const hasToolUse = (message: LLMMessage): boolean => {
      if (!Array.isArray(message.content)) {
        return false;
      }
      return message.content.some((block: any) => block?.type === 'tool_use');
    };

    const isToolResultUser = (message: LLMMessage): boolean => {
      if (message.role !== 'user' || !Array.isArray(message.content)) {
        return false;
      }
      return message.content.some((block: any) => block?.type === 'tool_result');
    };

    const isUserOrToolResult = (message?: LLMMessage): boolean => {
      if (!message) {
        return false;
      }
      if (message.role === 'user') {
        return true;
      }
      return isToolResultUser(message);
    };

    const sanitizeToolOrder = (inputMessages: LLMMessage[]): LLMMessage[] => {
      const cleaned: LLMMessage[] = [];
      for (let i = 0; i < inputMessages.length; i++) {
        const msg = inputMessages[i];
        if (msg.role === 'assistant' && hasToolUse(msg)) {
          const next = inputMessages[i + 1];
          const prev = cleaned[cleaned.length - 1];
          if (!next || !isToolResultUser(next) || !isUserOrToolResult(prev)) {
            continue;
          }
          cleaned.push(msg);
          cleaned.push(next);
          i += 1;
          continue;
        }

        if (isToolResultUser(msg)) {
          const prev = cleaned[cleaned.length - 1];
          if (!prev || !hasToolUse(prev)) {
            continue;
          }
        }

        cleaned.push(msg);
      }
      return cleaned;
    };

    const sanitizedMessages = sanitizeToolOrder(params.messages);

    for (const msg of sanitizedMessages) {
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

    // 构建请求参数
    const openAiTools = this.mapToolsForOpenAI(params.tools);
    const requestParams: any = {
      model: this.config.model,
      messages,
      max_tokens: params.maxTokens,
      temperature: params.temperature,
      tools: openAiTools,
    };

    const openAiToolChoice = this.mapToolChoiceForOpenAI(params.toolChoice);
    if (openAiToolChoice && openAiTools && openAiTools.length > 0) {
      requestParams.tool_choice = openAiToolChoice;
    }

    // 仅在启用 thinking 且配置了 reasoningEffort 时才添加（该参数仅部分模型支持，盲目添加会导致 400）
    if (this.shouldEnableThinking() && this.config.reasoningEffort) {
      requestParams.reasoning_effort = this.config.reasoningEffort;
      delete requestParams.temperature;
    }

    let response;
    try {
      response = await this.openaiClient.chat.completions.create(requestParams);
    } catch (error: any) {
      if (this.is400ToolSchemaError(error) && requestParams.tools?.length > 0) {
        const result = await this.retryWithToolElimination(requestParams, error);
        response = result;
      } else {
        throw error;
      }
    }
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

    // 构建请求参数
    const openAiTools = this.mapToolsForOpenAI(params.tools);
    const requestParams: any = {
      model: this.config.model,
      messages,
      max_tokens: params.maxTokens,
      temperature: params.temperature,
      tools: openAiTools,
      stream: true,
      stream_options: { include_usage: true },
    };

    const openAiToolChoice = this.mapToolChoiceForOpenAI(params.toolChoice);
    if (openAiToolChoice && openAiTools && openAiTools.length > 0) {
      requestParams.tool_choice = openAiToolChoice;
    }

    // 仅在启用 thinking 且配置了 reasoningEffort 时才添加（该参数仅部分模型支持，盲目添加会导致 400）
    if (this.shouldEnableThinking() && this.config.reasoningEffort) {
      requestParams.reasoning_effort = this.config.reasoningEffort;
      delete requestParams.temperature;
    }

    let stream;
    try {
      stream = await this.openaiClient.chat.completions.create(
        requestParams as Parameters<typeof this.openaiClient.chat.completions.create>[0] & { stream: true },
        { signal: params.signal },
      );
    } catch (error: any) {
      if (this.is400Error(error) && requestParams.stream_options) {
        // 渐进式降级：先去掉 stream_options（Gemini 等 OpenAI 兼容 API 不支持）
        logger.warn('400 stream_options 不兼容，降级重试', {
          model: this.config.model,
          error: error?.message?.substring(0, 200),
        }, LogCategory.LLM);
        delete requestParams.stream_options;
        try {
          stream = await this.openaiClient.chat.completions.create(
            requestParams as Parameters<typeof this.openaiClient.chat.completions.create>[0] & { stream: true },
            { signal: params.signal },
          );
        } catch (retryError: any) {
          if (this.is400ToolSchemaError(retryError) && requestParams.tools?.length > 0) {
            stream = await this.retryStreamWithToolElimination(requestParams, params.signal, retryError);
          } else {
            throw retryError;
          }
        }
      } else if (this.is400ToolSchemaError(error) && requestParams.tools?.length > 0) {
        stream = await this.retryStreamWithToolElimination(requestParams, params.signal, error);
      } else {
        throw error;
      }
    }

    let fullContent = '';
    const toolCallBuffers = new Map<string, { id: string; name?: string; argumentsText: string }>();
    let usage = { inputTokens: 0, outputTokens: 0 };
    let stopReason: LLMResponse['stopReason'] = 'end_turn';

    for await (const chunk of stream) {
      const delta = chunk.choices[0]?.delta;

      // 处理推理模型的思考内容
      // 不同 OpenAI 兼容 API 可能使用不同的字段名返回推理内容
      // 仅在启用 thinking 时才转发，否则忽略模型自带的推理内容
      if (this.shouldEnableThinking()) {
        const d = delta as any;
        const reasoningContent = d?.reasoning_content || d?.reasoning || d?.thinking_content || d?.thinking;
        if (reasoningContent) {
          onChunk({ type: 'thinking', thinking: reasoningContent });
        }
      }

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
        onChunk({
          type: 'usage',
          usage: {
            inputTokens: chunk.usage.prompt_tokens || 0,
            outputTokens: chunk.usage.completion_tokens || 0,
          },
        });
      }
    }

    // Fallback：如果 API 未返回 usage（第三方 OpenAI 兼容 API 常见），使用本地估算
    if (usage.inputTokens === 0 && usage.outputTokens === 0) {
      const inputText = messages.map(m => {
        if (typeof m.content === 'string') return m.content;
        if (Array.isArray(m.content)) return m.content.map((b: any) => b.text || b.content || '').join('');
        return '';
      }).join('');
      const outputText = fullContent;
      // 估算：约 4 字符 ≈ 1 token（通用近似）
      usage.inputTokens = Math.ceil(inputText.length / 4);
      usage.outputTokens = Math.ceil(outputText.length / 4);
      logger.debug('OpenAI 流式未返回 usage，使用本地估算', {
        estimatedInput: usage.inputTokens,
        estimatedOutput: usage.outputTokens,
        model: this.config.model,
      }, LogCategory.LLM);
      onChunk({
        type: 'usage',
        usage: {
          inputTokens: usage.inputTokens,
          outputTokens: usage.outputTokens,
        },
      });
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
            if (block.type === 'image') {
              const mediaType = block.source?.media_type || 'image/png';
              const base64Data = block.source?.data || '';
              return {
                type: 'image_url',
                image_url: {
                  url: `data:${mediaType};base64,${base64Data}`,
                },
              };
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
