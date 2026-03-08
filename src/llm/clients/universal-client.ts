/**
 * 通用 LLM 客户端
 * 职责：客户端初始化 + 协议适配分发 + 重试治理 + 协议一致性校验
 */

import Anthropic from '@anthropic-ai/sdk';
import OpenAI from 'openai';
import { BaseLLMClient } from './base-client';
import { LLMConfig } from '../../types/agent-types';
import {
  LLMMessageParams,
  LLMResponse,
  LLMStreamChunk,
} from '../types';
import { logger, LogCategory } from '../../logging';
import { fetchWithRetry, isRetryableNetworkError, toErrorMessage } from '../../tools/network-utils';
import { ProviderProtocolAdapter } from '../protocol/provider-adapter';
import { ProtocolConformanceValidator } from '../protocol/conformance-validator';
import { OpenAIResponsesProtocolAdapter } from '../protocol/adapters/openai-responses-adapter';
import { AnthropicMessagesProtocolAdapter } from '../protocol/adapters/anthropic-messages-adapter';

class NonRetryableError extends Error {
  constructor(message: string, public originalError?: unknown) {
    super(message);
    this.name = 'NonRetryableError';
  }
}

export class UniversalLLMClient extends BaseLLMClient {
  private anthropicClient?: Anthropic;
  private openaiClient?: OpenAI;
  private protocolAdapter?: ProviderProtocolAdapter;
  private readonly conformanceValidator = new ProtocolConformanceValidator();

  constructor(config: LLMConfig) {
    super(config);
    this.validateConfig();
    this.initializeClient();
  }

  private normalizeOpenAIBaseUrl(baseUrl: string): string {
    const trimmed = (baseUrl || '').trim();
    if (!trimmed) return trimmed;
    const noTrailingSlash = trimmed.replace(/\/+$/, '');
    if (/\/v1$/i.test(noTrailingSlash)) {
      return noTrailingSlash;
    }
    return `${noTrailingSlash}/v1`;
  }

  /**
   * Anthropic SDK 会自行拼接 /v1/* 路径。
   * 为避免用户配置已带 /v1 时出现 /v1/v1，SDK 入参需要去掉末尾 /v1。
   */
  private normalizeAnthropicSdkBaseUrl(baseUrl: string): string {
    const trimmed = (baseUrl || '').trim();
    if (!trimmed) return trimmed;
    const noTrailingSlash = trimmed.replace(/\/+$/, '');
    if (/\/v1$/i.test(noTrailingSlash)) {
      return noTrailingSlash.replace(/\/v1$/i, '');
    }
    return noTrailingSlash;
  }

  /**
   * 获取 Anthropic Models API 的基础地址（以 /v1 结尾）
   */
  private normalizeAnthropicModelsBaseUrl(baseUrl: string): string {
    const sdkBase = this.normalizeAnthropicSdkBaseUrl(baseUrl);
    if (!sdkBase) return sdkBase;
    return `${sdkBase}/v1`;
  }

  private initializeClient(): void {
    if (this.config.provider === 'anthropic') {
      const baseURL = this.normalizeAnthropicSdkBaseUrl(this.config.baseUrl);
      this.anthropicClient = new Anthropic({
        apiKey: this.config.apiKey,
        baseURL,
      });
      logger.info('Anthropic client initialized', {
        originalBaseUrl: this.config.baseUrl,
        finalBaseUrl: baseURL,
        model: this.config.model,
      }, LogCategory.LLM);
    } else if (this.config.provider === 'openai') {
      const baseURL = this.normalizeOpenAIBaseUrl(this.config.baseUrl);
      this.openaiClient = new OpenAI({
        apiKey: this.config.apiKey,
        baseURL,
      });

      logger.info('OpenAI client initialized', {
        originalBaseUrl: this.config.baseUrl,
        finalBaseUrl: baseURL,
        model: this.config.model,
      }, LogCategory.LLM);
    } else {
      throw new Error(`Unsupported provider: ${this.config.provider}`);
    }

    this.protocolAdapter = this.createProtocolAdapter();
    logger.info('LLM protocol adapter initialized', {
      provider: this.protocolAdapter.provider,
      protocol: this.protocolAdapter.protocol,
      capabilities: this.protocolAdapter.capabilities,
      model: this.config.model,
    }, LogCategory.LLM);
  }

  private getProtocolAdapter(): ProviderProtocolAdapter {
    if (!this.protocolAdapter) {
      this.protocolAdapter = this.createProtocolAdapter();
    }
    return this.protocolAdapter;
  }

  public getProtocolProfile() {
    const adapter = this.getProtocolAdapter();
    return {
      provider: adapter.provider,
      protocol: adapter.protocol,
      capabilities: { ...adapter.capabilities },
    };
  }

  private createProtocolAdapter(): ProviderProtocolAdapter {
    if (this.config.provider === 'anthropic') {
      if (!this.anthropicClient) {
        throw new Error('Anthropic client not initialized');
      }
      return new AnthropicMessagesProtocolAdapter(this.config, this.anthropicClient);
    }

    if (!this.openaiClient) {
      throw new Error('OpenAI client not initialized');
    }
    return new OpenAIResponsesProtocolAdapter(this.config, this.openaiClient);
  }

  async testConnectionFast(): Promise<{
    success: boolean;
    modelExists?: boolean;
    error?: string;
  }> {
    try {
      const isAnthropic = this.config.provider === 'anthropic';
      const baseV1 = isAnthropic
        ? this.normalizeAnthropicModelsBaseUrl(this.config.baseUrl)
        : this.normalizeOpenAIBaseUrl(this.config.baseUrl);
      const modelsUrl = `${baseV1}/models`;

      const headers: Record<string, string> = {
        'Content-Type': 'application/json',
      };
      if (isAnthropic) {
        headers['x-api-key'] = this.config.apiKey;
        headers['anthropic-version'] = '2023-06-01';
      } else {
        headers.Authorization = `Bearer ${this.config.apiKey}`;
      }

      const response = await fetchWithRetry(modelsUrl, {
        method: 'GET',
        headers,
      }, {
        timeoutMs: 5000,
        attempts: 2,
        retryOnStatuses: [429, 500, 502, 503, 504],
      });

      if (!response.ok) {
        const status = response.status;
        if (status === 401 || status === 403) {
          return { success: false, error: 'API Key 无效' };
        }
        if (status === 404) {
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
      const message = toErrorMessage(error);
      const lowerMessage = message.toLowerCase();
      if (lowerMessage.includes('timeout') || lowerMessage.includes('timed out')) {
        return { success: false, error: '连接超时' };
      }
      if (isRetryableNetworkError(message)) {
        return { success: false, error: '网络连接失败' };
      }
      logger.error('Fast connection test failed', { error: message }, LogCategory.LLM);
      return { success: false, error: message };
    }
  }

  async sendMessage(params: LLMMessageParams): Promise<LLMResponse> {
    this.logRequest(params);

    return this.withRetry(async () => {
      try {
        const adapter = this.getProtocolAdapter();
        const response = await adapter.send(params);
        this.conformanceValidator.validateResponse(response, adapter.protocol);
        this.logResponse(response);
        return response;
      } catch (error) {
        this.logError(error, 'sendMessage');
        throw error;
      }
    }, 'sendMessage');
  }

  async streamMessage(
    params: LLMMessageParams,
    onChunk: (chunk: LLMStreamChunk) => void,
  ): Promise<LLMResponse> {
    this.logRequest({ ...params, stream: true });

    let hasReceivedData = false;
    const adapter = this.getProtocolAdapter();
    const wrappedOnChunk = (chunk: LLMStreamChunk) => {
      this.conformanceValidator.validateStreamChunk(chunk, adapter.protocol);
      hasReceivedData = true;
      onChunk(chunk);
    };

    return this.withRetry(async () => {
      try {
        const response = await adapter.stream(params, wrappedOnChunk);
        this.conformanceValidator.validateResponse(response, adapter.protocol);
        this.logResponse(response);
        return response;
      } catch (error) {
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
    return /timeout|timed out|connection|network|fetch failed|socket hang up|ECONNRESET|ENOTFOUND|EAI_AGAIN|ECONNREFUSED|request ended without sending|stream ended|overloaded/i.test(message);
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }
}
