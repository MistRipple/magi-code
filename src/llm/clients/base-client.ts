/**
 * LLM 客户端抽象基类
 */

import { EventEmitter } from 'events';
import { LLMConfig } from '../../types/agent-types';
import {
  LLMClient,
  LLMMessageParams,
  LLMResponse,
  LLMStreamChunk,
} from '../types';
import { logger, LogCategory } from '../../logging';

/**
 * LLM 客户端基类
 */
export abstract class BaseLLMClient extends EventEmitter implements LLMClient {
  public readonly config: LLMConfig;

  constructor(config: LLMConfig) {
    super();
    this.config = config;
  }

  /**
   * 发送消息（非流式）
   */
  abstract sendMessage(params: LLMMessageParams): Promise<LLMResponse>;

  /**
   * 发送消息（流式）
   */
  abstract streamMessage(
    params: LLMMessageParams,
    onChunk: (chunk: LLMStreamChunk) => void
  ): Promise<LLMResponse>;

  /**
   * 快速测试连接（使用 Models API）
   *
   * 使用 /v1/models 端点验证 API Key 有效性，不消耗 tokens。
   * 比 testConnection() 快约 10 倍。
   *
   * @returns 包含成功/失败状态、模型是否存在的结果
   */
  abstract testConnectionFast(): Promise<{
    success: boolean;
    modelExists?: boolean;
    error?: string;
  }>;

  /**
   * 测试连接（发送真实消息）
   *
   * 注意：此方法会发送一个实际的 LLM 请求来测试连接，可能产生延迟和费用。
   * 不建议在连接阶段调用，Adapter.connect() 已不再使用此方法。
   * 保留此方法仅供需要显式验证 API 配置的场景使用（如设置界面测试）。
   */
  async testConnection(): Promise<boolean> {
    try {
      const response = await this.sendMessage({
        messages: [{ role: 'user', content: 'test' }],
        maxTokens: 10,
      });
      return !!response;
    } catch (error) {
      logger.error('Connection test failed', { error }, LogCategory.LLM);
      return false;
    }
  }

  /**
   * 验证配置
   */
  protected validateConfig(): void {
    if (!this.config.apiKey) {
      throw new Error(`API key is required for ${this.config.provider}`);
    }
    if (!this.config.model) {
      throw new Error(`Model is required for ${this.config.provider}`);
    }
    if (!this.config.baseUrl) {
      throw new Error(`Base URL is required for ${this.config.provider}`);
    }
  }

  /**
   * 记录请求
   */
  protected logRequest(params: LLMMessageParams): void {
    logger.debug('Sending LLM request', {
      provider: this.config.provider,
      model: this.config.model,
      messageCount: params.messages.length,
      hasTools: !!params.tools?.length,
      stream: params.stream,
    }, LogCategory.LLM);
  }

  /**
   * 记录响应
   */
  protected logResponse(response: LLMResponse): void {
    logger.debug('Received LLM response', {
      provider: this.config.provider,
      model: this.config.model,
      contentLength: response.content.length,
      toolCalls: response.toolCalls?.length || 0,
      usage: response.usage,
      stopReason: response.stopReason,
    }, LogCategory.LLM);
  }

  /**
   * 记录错误
   */
  protected logError(error: any, context: string): void {
    logger.error(`LLM error: ${context}`, {
      provider: this.config.provider,
      model: this.config.model,
      error: error.message || error,
      stack: error.stack,
    }, LogCategory.LLM);
  }
}
