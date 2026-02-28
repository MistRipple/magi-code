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
  sanitizeToolOrder,
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
   * 计算 text 尾部与 incoming 头部的最大重叠长度
   * 用于 OpenAI 兼容流在 cumulative 模式下提取“新增后缀”
   */
  private computeSuffixPrefixOverlap(text: string, incoming: string): number {
    const max = Math.min(text.length, incoming.length, 4096);
    for (let len = max; len > 0; len--) {
      if (text.slice(-len) === incoming.slice(0, len)) {
        return len;
      }
    }
    return 0;
  }

  /**
   * 统一流式文本增量归一化：
   * - 默认按 delta 模式处理
   * - 当检测到 provider 返回 cumulative 内容时，自动切换为“提取新增后缀”
   */
  private normalizeStreamDelta(
    incoming: string,
    emittedText: string,
    mode: 'unknown' | 'delta' | 'cumulative'
  ): { delta: string; mode: 'unknown' | 'delta' | 'cumulative' } {
    if (!incoming) {
      return { delta: '', mode };
    }
    if (!emittedText) {
      return { delta: incoming, mode };
    }

    let resolvedMode = mode;
    if (resolvedMode === 'unknown') {
      resolvedMode = incoming.length > emittedText.length && incoming.startsWith(emittedText)
        ? 'cumulative'
        : 'delta';
    }

    if (resolvedMode !== 'cumulative') {
      return { delta: incoming, mode: resolvedMode };
    }

    if (incoming.length <= emittedText.length && emittedText.endsWith(incoming)) {
      return { delta: '', mode: resolvedMode };
    }
    if (incoming.startsWith(emittedText)) {
      return { delta: incoming.slice(emittedText.length), mode: resolvedMode };
    }

    const overlap = this.computeSuffixPrefixOverlap(emittedText, incoming);
    return { delta: incoming.slice(overlap), mode: resolvedMode };
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
   *
   * 注意：不匹配 `invalid.argument` — 该模式过于宽泛，
   * 会误匹配 Google API 通用 400（如模型名错误、参数格式不合法），
   * 导致非工具问题触发 retryWithToolElimination 二分法递归，
   * 产生 ~2N 次无效 API 调用和大量 warn 日志。
   */
  private is400ToolSchemaError(error: any): boolean {
    const status = error?.status || error?.response?.status;
    if (status !== 400) return false;
    const msg = String(error?.message || error?.error?.message || '');
    return /invalid.*schema|invalid.*tool|invalid.*function/i.test(msg);
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

  /**
   * 将各类 token 字段安全转换为非负整数
   */
  private toSafeTokenNumber(value: unknown): number {
    if (typeof value === 'number' && Number.isFinite(value)) {
      return Math.max(0, Math.trunc(value));
    }
    if (typeof value === 'string') {
      const parsed = Number(value);
      if (Number.isFinite(parsed)) {
        return Math.max(0, Math.trunc(parsed));
      }
    }
    return 0;
  }

  /**
   * 取第一个有效 token 数值
   */
  private pickFirstTokenNumber(...values: unknown[]): number {
    for (const value of values) {
      const tokenNumber = this.toSafeTokenNumber(value);
      if (tokenNumber > 0) {
        return tokenNumber;
      }
    }
    return 0;
  }

  /**
   * 统一解析 Anthropic usage（含 cache 字段）
   */
  private normalizeAnthropicUsage(rawUsage: any): {
    inputTokens: number;
    outputTokens: number;
    cacheReadTokens?: number;
    cacheWriteTokens?: number;
  } {
    const inputTokens = this.pickFirstTokenNumber(rawUsage?.input_tokens, rawUsage?.inputTokens);
    const outputTokens = this.pickFirstTokenNumber(rawUsage?.output_tokens, rawUsage?.outputTokens);
    const cacheReadTokens = this.pickFirstTokenNumber(
      rawUsage?.cache_read_input_tokens,
      rawUsage?.cacheReadInputTokens,
      rawUsage?.cache_read_tokens,
      rawUsage?.cacheReadTokens,
    );
    const cacheWriteTokens = this.pickFirstTokenNumber(
      rawUsage?.cache_creation_input_tokens,
      rawUsage?.cacheCreationInputTokens,
      rawUsage?.cache_creation_tokens,
      rawUsage?.cacheWriteTokens,
    );

    return {
      inputTokens,
      outputTokens,
      cacheReadTokens: cacheReadTokens || undefined,
      cacheWriteTokens: cacheWriteTokens || undefined,
    };
  }

  /**
   * 统一解析 OpenAI 兼容 usage（兼容 prompt/completion 与 input/output 两套命名）
   */
  private normalizeOpenAIUsage(rawUsage: any): {
    inputTokens: number;
    outputTokens: number;
  } {
    return {
      inputTokens: this.pickFirstTokenNumber(
        rawUsage?.prompt_tokens,
        rawUsage?.promptTokens,
        rawUsage?.input_tokens,
        rawUsage?.inputTokens,
      ),
      outputTokens: this.pickFirstTokenNumber(
        rawUsage?.completion_tokens,
        rawUsage?.completionTokens,
        rawUsage?.output_tokens,
        rawUsage?.outputTokens,
      ),
    };
  }

  /**
   * 统一规整 tool_result 内容块，作为 OpenAI/Anthropic 转换前的单一入口。
   */
  private normalizeToolResultBlock(
    block: any,
    context: string,
  ): { toolUseId: string; content: string; isError: boolean } | null {
    const toolUseId = typeof block?.tool_use_id === 'string' ? block.tool_use_id.trim() : '';
    if (!toolUseId) {
      logger.warn('忽略缺少 tool_use_id 的 tool_result', {
        provider: this.config.provider,
        model: this.config.model,
        context,
      }, LogCategory.LLM);
      return null;
    }

    const standardizedStatus = typeof block?.standardized?.status === 'string'
      ? block.standardized.status
      : '';
    const standardizedMessage = typeof block?.standardized?.message === 'string'
      ? block.standardized.message.trim()
      : '';

    const isError = block?.is_error === true || (standardizedStatus !== '' && standardizedStatus !== 'success');
    const rawContent = block?.content;
    const stringContent = typeof rawContent === 'string'
      ? rawContent
      : (rawContent == null ? '' : JSON.stringify(rawContent));
    const content = stringContent.trim()
      ? stringContent
      : (isError ? (standardizedMessage || 'Tool execution failed') : '[empty result]');

    return {
      toolUseId,
      content,
      isError,
    };
  }

  private toOpenAIToolMessageContent(normalized: { content: string; isError: boolean }): string {
    const content = normalized.content || '[empty result]';
    if (!normalized.isError) {
      return content;
    }
    if (/^\s*(\[error\]|error[:\]])/i.test(content)) {
      return content;
    }
    return `[Error] ${content}`;
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
    let usage: {
      inputTokens: number;
      outputTokens: number;
      cacheReadTokens?: number;
      cacheWriteTokens?: number;
    } = { inputTokens: 0, outputTokens: 0 };
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
          let partialParsedArgs: Record<string, any> | undefined = undefined;
          if (lastTool?.argumentsText) {
            try {
              partialParsedArgs = JSON.parse(lastTool.argumentsText);
            } catch {
              // 增量解析失败是正常的，传递 undefined 让上层自己判断
            }
          }
          onChunk({
            type: 'tool_call_delta',
            toolCall: { arguments: partialParsedArgs },
          });
        }
      } else if (event.type === 'content_block_stop') {
        onChunk({ type: 'content_end' });
      } else if (event.type === 'message_delta') {
        if (event.usage) {
          const normalizedUsage = this.normalizeAnthropicUsage(event.usage);
          usage.outputTokens = normalizedUsage.outputTokens;
          usage.cacheReadTokens = normalizedUsage.cacheReadTokens;
          usage.cacheWriteTokens = normalizedUsage.cacheWriteTokens;
          onChunk({
            type: 'usage',
            usage: {
              outputTokens: normalizedUsage.outputTokens,
              cacheReadTokens: normalizedUsage.cacheReadTokens,
              cacheWriteTokens: normalizedUsage.cacheWriteTokens,
            }
          });
        }
        if (event.delta.stop_reason) {
          stopReason = this.mapAnthropicStopReason(event.delta.stop_reason);
        }
      } else if (event.type === 'message_start') {
        if (event.message.usage) {
          const normalizedUsage = this.normalizeAnthropicUsage(event.message.usage);
          usage.inputTokens = normalizedUsage.inputTokens;
          usage.cacheReadTokens = normalizedUsage.cacheReadTokens;
          usage.cacheWriteTokens = normalizedUsage.cacheWriteTokens;
          onChunk({
            type: 'usage',
            usage: {
              inputTokens: normalizedUsage.inputTokens,
              cacheReadTokens: normalizedUsage.cacheReadTokens,
              cacheWriteTokens: normalizedUsage.cacheWriteTokens,
            }
          });
        }
      }
    }

    const toolCalls: ToolCall[] = [];
    for (const tool of toolCallBuffers.values()) {
      if (!tool.id) continue;

      const parsedArgs = this.parseToolArguments(
        tool.argumentsText || '',
        `stream:${tool.name || tool.id}`
      );

      toolCalls.push({
        id: tool.id,
        name: tool.name || '',
        arguments: parsedArgs.value,
        argumentParseError: parsedArgs.error,
        rawArguments: parsedArgs.rawText,
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
              const normalized = this.normalizeToolResultBlock(
                block as any,
                `anthropic:${msg.role}`
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

    const normalizedUsage = this.normalizeAnthropicUsage(response.usage);

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

    // 用户显式配置了 reasoningEffort 即传递（该参数是 OpenAI API 的独立顶层参数，无需前置条件）
    // 推理模型不支持 temperature，设置 reasoning_effort 时需移除
    if (this.config.reasoningEffort) {
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

    // 用户显式配置了 reasoningEffort 即传递（该参数是 OpenAI API 的独立顶层参数，无需前置条件）
    // 推理模型不支持 temperature，设置 reasoning_effort 时需移除
    if (this.config.reasoningEffort) {
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
    let contentDeltaMode: 'unknown' | 'delta' | 'cumulative' = 'unknown';
    const toolCallBuffers = new Map<string, { id: string; name?: string; argumentsText: string }>();
    const toolCallFallbackPrefix = `magi_call_${Date.now().toString(36)}`;
    let toolCallFallbackSeq = 0;
    const createFallbackToolCallId = () => `${toolCallFallbackPrefix}_${toolCallFallbackSeq++}`;
    let usage: {
      inputTokens: number;
      outputTokens: number;
    } = { inputTokens: 0, outputTokens: 0 };
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
        const normalized = this.normalizeStreamDelta(delta.content, fullContent, contentDeltaMode);
        contentDeltaMode = normalized.mode;
        if (normalized.delta) {
          fullContent += normalized.delta;
          onChunk({ type: 'content_delta', content: normalized.delta });
        }
      }

      if (delta?.tool_calls) {
        for (const [toolPosition, toolCall] of delta.tool_calls.entries()) {
          // 以 index 为主键聚合同一次 tool call 的多段 delta；
          // 缺失 index/id 时使用当前位置作为弱主键，确保同轮多工具不串扰。
          let bufferKey = '';
          if (typeof toolCall.index === 'number') {
            bufferKey = `idx_${toolCall.index}`;
          } else if (toolCall.id) {
            bufferKey = `id_${toolCall.id}`;
          } else {
            bufferKey = `anon_pos_${toolPosition}`;
          }

          if (!toolCallBuffers.has(bufferKey)) {
            const stableToolCallId = toolCall.id || createFallbackToolCallId();
            if (!toolCall.id) {
              logger.warn('OpenAI stream 返回的 tool_call 缺少 id，已生成后备 id', {
                model: this.config.model,
                bufferKey,
                stableToolCallId,
              }, LogCategory.LLM);
            }
            toolCallBuffers.set(bufferKey, {
              id: stableToolCallId,
              name: toolCall.function?.name,
              argumentsText: '',
            });
          }
          const buffer = toolCallBuffers.get(bufferKey)!;
          if (toolCall.function?.name) {
            buffer.name = toolCall.function.name;
          }
          const deltaArgs = (toolCall.function as any)?.arguments;
          if (typeof deltaArgs === 'string') {
            buffer.argumentsText += deltaArgs;
          } else if (deltaArgs !== undefined && deltaArgs !== null) {
            buffer.argumentsText += JSON.stringify(deltaArgs);
          }
          if (toolCall.function?.name) {
            onChunk({
              type: 'tool_call_start',
              toolCall: {
                id: buffer.id,
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
        const normalizedUsage = this.normalizeOpenAIUsage(chunk.usage);
        usage.inputTokens = normalizedUsage.inputTokens;
        usage.outputTokens = normalizedUsage.outputTokens;
        onChunk({
          type: 'usage',
          usage: normalizedUsage,
        });
      }
    }

    const toolCalls: ToolCall[] = [];
    for (const [bufferKey, tool] of toolCallBuffers.entries()) {
      const toolId = tool.id || bufferKey;
      if (!toolId) continue;
      const parsedArgs = this.parseToolArguments(tool.argumentsText, `stream:${tool.name || toolId}`);
      toolCalls.push({
        id: toolId,
        name: tool.name || '',
        arguments: parsedArgs.value,
        argumentParseError: parsedArgs.error,
        rawArguments: parsedArgs.rawText,
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
    // 先清理悬空的 tool_use/tool_result 对（与 Anthropic 路径共享逻辑）
    const sanitizedMessages = sanitizeToolOrder(params.messages);
    const messages: OpenAI.ChatCompletionMessageParam[] = [];

    for (const msg of sanitizedMessages) {
      if (typeof msg.content === 'string') {
        messages.push({
          role: msg.role,
          content: msg.content,
        } as OpenAI.ChatCompletionMessageParam);
        continue;
      }

      // 按类型分拣内容块
      const textParts: string[] = [];
      const imageParts: OpenAI.ChatCompletionContentPartImage[] = [];
      const toolUseBlocks: Array<{ id: string; name: string; input: Record<string, any> }> = [];
      const toolResultBlocks: Array<{ tool_use_id: string; content: string; is_error: boolean }> = [];

      for (const block of msg.content) {
        const b = block as any;
        switch (b.type) {
          case 'text':
            textParts.push(b.text);
            break;
          case 'image':
            imageParts.push({
              type: 'image_url',
              image_url: {
                url: `data:${b.source?.media_type || 'image/png'};base64,${b.source?.data || ''}`,
              },
            });
            break;
          case 'tool_use':
            toolUseBlocks.push({ id: b.id, name: b.name, input: b.input });
            break;
          case 'tool_result': {
            const normalized = this.normalizeToolResultBlock(
              b,
              `openai:${msg.role}`
            );
            if (!normalized) {
              break;
            }
            toolResultBlocks.push({
              tool_use_id: normalized.toolUseId,
              content: normalized.content,
              is_error: normalized.isError,
            });
            break;
          }
        }
      }

      // assistant 消息：tool_use → OpenAI tool_calls 顶层属性
      if (msg.role === 'assistant') {
        const assistantText = textParts.join('\n');
        const assistantMsg: OpenAI.ChatCompletionAssistantMessageParam = {
          role: 'assistant',
          content: assistantText || (toolUseBlocks.length > 0 ? null : ''),
        };
        if (toolUseBlocks.length > 0) {
          assistantMsg.tool_calls = toolUseBlocks.map(tc => ({
            id: tc.id,
            type: 'function' as const,
            function: {
              name: tc.name,
              arguments: typeof tc.input === 'string' ? tc.input : JSON.stringify(tc.input),
            },
          }));
        }
        messages.push(assistantMsg);
        continue;
      }

      // tool_result → 独立的 role:'tool' 消息
      for (const result of toolResultBlocks) {
        const content = this.toOpenAIToolMessageContent({
          content: result.content,
          isError: result.is_error,
        });
        messages.push({
          role: 'tool',
          tool_call_id: result.tool_use_id,
          content,
        } as OpenAI.ChatCompletionToolMessageParam);
      }

      // 剩余的文本/图片块保持原始 role
      if (textParts.length > 0 || imageParts.length > 0) {
        const role = msg.role === 'system' ? 'system' : 'user';
        if (imageParts.length === 0) {
          messages.push({ role, content: textParts.join('\n') } as OpenAI.ChatCompletionMessageParam);
        } else {
          const parts: OpenAI.ChatCompletionContentPart[] = [
            ...textParts.map(t => ({ type: 'text' as const, text: t })),
            ...imageParts,
          ];
          messages.push({ role: 'user', content: parts });
        }
      }
    }

    if (params.systemPrompt) {
      messages.unshift({
        role: 'system',
        content: params.systemPrompt,
      });
    }

    // 格式校验：检查 tool_call_id 数量与顺序是否匹配（按 occurrence 计数）
    const declaredIds: string[] = [];
    const resultIds: string[] = [];
    for (const m of messages) {
      if (m.role === 'assistant' && (m as any).tool_calls) {
        for (const tc of (m as any).tool_calls) {
          declaredIds.push(String(tc.id || ''));
        }
      }
      if (m.role === 'tool') {
        resultIds.push(String((m as any).tool_call_id || ''));
      }
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
      for (let i = 0; i < gap; i++) {
        missingResults.push(id);
      }
    }
    for (const [id, resultCount] of resultCounter.entries()) {
      const declaredCount = declaredCounter.get(id) || 0;
      const gap = resultCount - declaredCount;
      for (let i = 0; i < gap; i++) {
        orphanResults.push(id);
      }
    }
    if (missingResults.length > 0 || orphanResults.length > 0) {
      logger.warn('convertToOpenAIFormat: tool_call_id 匹配异常', {
        missingResults,
        orphanResults,
        declaredCount: declaredIds.length,
        resultCount: resultIds.length,
      }, LogCategory.LLM);
    }

    logger.debug('convertToOpenAIFormat 转换完成', {
      inputCount: params.messages.length,
      outputCount: messages.length,
      roles: messages.map(m => m.role),
      toolCallIds: declaredIds,
      toolResultIds: resultIds,
      matched: missingResults.length === 0 && orphanResults.length === 0,
    }, LogCategory.LLM);

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
    const fallbackPrefix = `magi_call_sync_${Date.now().toString(36)}`;
    let fallbackSeq = 0;
    if (message.tool_calls) {
      for (const toolCall of message.tool_calls) {
        if (toolCall.type === 'function') {
          const toolCallId = toolCall.id || `${fallbackPrefix}_${fallbackSeq++}`;
          if (!toolCall.id) {
            logger.warn('OpenAI 非流式响应的 tool_call 缺少 id，已生成后备 id', {
              model: this.config.model,
              toolName: toolCall.function.name,
              toolCallId,
            }, LogCategory.LLM);
          }
          const parsedArgs = this.parseToolArguments(
            toolCall.function.arguments,
            `sync:${toolCall.function.name}`
          );
          toolCalls.push({
            id: toolCallId,
            name: toolCall.function.name,
            arguments: parsedArgs.value,
            argumentParseError: parsedArgs.error,
            rawArguments: parsedArgs.rawText,
          });
        }
      }
    }

    const normalizedUsage = this.normalizeOpenAIUsage(response.usage);

    return {
      content: message.content || '',
      toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
      usage: normalizedUsage,
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

  private parseToolArguments(
    raw: unknown,
    context: string
  ): { value: Record<string, any>; error?: string; rawText?: string } {
    if (raw === undefined || raw === null || raw === '') {
      return { value: {} };
    }

    if (typeof raw === 'object') {
      if (Array.isArray(raw)) {
        return {
          value: {},
          error: '参数解析后为数组，工具参数必须是对象',
          rawText: JSON.stringify(raw),
        };
      }
      return { value: raw as Record<string, any> };
    }

    if (typeof raw !== 'string') {
      logger.error('Tool arguments 类型异常', {
        provider: this.config.provider,
        model: this.config.model,
        context,
        argType: typeof raw,
      }, LogCategory.LLM);
      return {
        value: null as any,
        error: `参数类型异常: ${typeof raw}`,
        rawText: String(raw),
      };
    }

    const text = raw.trim();
    if (!text) {
      return { value: {} };
    }

    try {
      const parsed = JSON.parse(text);
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        return { value: parsed as Record<string, any>, rawText: text };
      }
      logger.error('Tool arguments 解析结果非对象', {
        provider: this.config.provider,
        model: this.config.model,
        context,
        parsedType: typeof parsed,
      }, LogCategory.LLM);
      return {
        value: null as any,
        error: `参数 JSON 解析后不是对象: ${typeof parsed}`,
        rawText: text,
      };
    } catch (error: any) {
      // 兜底恢复：使用字符串状态机提取首个完整 JSON 对象，避免被字符串内大括号干扰。
      const extracted = this.extractFirstJSONObject(text);
      if (extracted && extracted !== text) {
        try {
          const recovered = JSON.parse(extracted);
          if (recovered && typeof recovered === 'object' && !Array.isArray(recovered)) {
            logger.info('Tool arguments 解析失败后已成功恢复 JSON', {
              provider: this.config.provider,
              model: this.config.model,
              context,
            }, LogCategory.LLM);
            return { value: recovered as Record<string, any>, rawText: text };
          }
        } catch (recoveryError: any) {
          logger.info('工具参数尝试恢复解析失败', {
            error: recoveryError?.message,
            extractedText: extracted
          }, LogCategory.LLM);
        }
      }
      logger.error('Tool arguments JSON 解析彻底失败', {
        provider: this.config.provider,
        model: this.config.model,
        context,
        error: error?.message || String(error),
        rawSnippet: text.substring(0, 300),
      }, LogCategory.LLM);
      return {
        value: null as any,
        error: `参数 JSON 解析失败: ${error?.message || String(error)}`,
        rawText: text,
      };
    }
  }

  /**
   * 从文本中提取首个完整 JSON 对象。
   * 使用引号/转义感知状态机，避免误把字符串中的 { } 当成结构边界。
   */
  private extractFirstJSONObject(text: string): string | null {
    let depth = 0;
    let start = -1;
    let inString = false;
    let escaped = false;

    for (let i = 0; i < text.length; i++) {
      const ch = text[i];

      if (inString) {
        if (escaped) {
          escaped = false;
          continue;
        }
        if (ch === '\\') {
          escaped = true;
          continue;
        }
        if (ch === '"') {
          inString = false;
        }
        continue;
      }

      if (ch === '"') {
        inString = true;
        continue;
      }

      if (ch === '{') {
        if (depth === 0) {
          start = i;
        }
        depth++;
        continue;
      }

      if (ch === '}') {
        if (depth === 0) {
          continue;
        }
        depth--;
        if (depth === 0 && start >= 0) {
          return text.slice(start, i + 1).trim();
        }
      }
    }

    return null;
  }
}
