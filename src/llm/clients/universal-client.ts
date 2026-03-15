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
  LLMRetryRuntimeEvent,
  LLMResponse,
  LLMStreamChunk,
} from '../types';
import { logger, LogCategory } from '../../logging';
import { fetchWithRetry, isRetryableNetworkError, toErrorMessage } from '../../tools/network-utils';
import { ProviderProtocolAdapter } from '../protocol/provider-adapter';
import { ProtocolConformanceValidator } from '../protocol/conformance-validator';
import { OpenAIResponsesProtocolAdapter } from '../protocol/adapters/openai-responses-adapter';
import { OpenAIChatCompletionsProtocolAdapter } from '../protocol/adapters/openai-chat-completions-adapter';
import { AnthropicMessagesProtocolAdapter } from '../protocol/adapters/anthropic-messages-adapter';
import { resolveProtocolId } from '../protocol/capability-registry';
import { resolveModelsBaseUrl, resolveSdkBaseUrl } from '../url-mode';

class NonRetryableError extends Error {
  constructor(message: string, public originalError?: unknown) {
    super(message);
    this.name = 'NonRetryableError';
  }
}

interface EffectiveRetryPolicy {
  maxRetries: number;
  baseDelayMs: number;
  retryDelaysMs: number[];
  retryOnTimeout: boolean;
  retryOnAllErrors: boolean;
  maxRetryDurationMs: number;
  deterministicErrorStreakLimit: number;
  circuitBreaker: {
    enabled: boolean;
    windowMs: number;
    failureThreshold: number;
    cooldownMs: number;
  };
}

interface RetryResilienceState {
  recentFailureTimestamps: number[];
  circuitOpenUntil: number;
}

export class UniversalLLMClient extends BaseLLMClient {
  private anthropicClient?: Anthropic;
  private openaiClient?: OpenAI;
  private protocolAdapter?: ProviderProtocolAdapter;
  private readonly conformanceValidator = new ProtocolConformanceValidator();
  private readonly retryResilienceState: RetryResilienceState = {
    recentFailureTimestamps: [],
    circuitOpenUntil: 0,
  };

  constructor(config: LLMConfig) {
    super(config);
    this.validateConfig();
    this.initializeClient();
  }

  private initializeClient(): void {
    if (this.config.provider === 'anthropic') {
      const baseURL = resolveSdkBaseUrl(this.config.provider, this.config.baseUrl, this.config.urlMode);
      this.anthropicClient = new Anthropic({
        apiKey: this.config.apiKey,
        baseURL,
      });
      logger.info('Anthropic client initialized', {
        originalBaseUrl: this.config.baseUrl,
        finalBaseUrl: baseURL,
        urlMode: this.config.urlMode,
        model: this.config.model,
      }, LogCategory.LLM);
    } else if (this.config.provider === 'openai') {
      const baseURL = resolveSdkBaseUrl(this.config.provider, this.config.baseUrl, this.config.urlMode);
      this.openaiClient = new OpenAI({
        apiKey: this.config.apiKey,
        baseURL,
      });

      logger.info('OpenAI client initialized', {
        originalBaseUrl: this.config.baseUrl,
        finalBaseUrl: baseURL,
        urlMode: this.config.urlMode,
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

    const protocolId = resolveProtocolId(this.config.provider, this.config.openaiProtocol);
    if (protocolId === 'openai.chat-completions') {
      return new OpenAIChatCompletionsProtocolAdapter(this.config, this.openaiClient);
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
      const baseV1 = resolveModelsBaseUrl(this.config.provider, this.config.baseUrl, this.config.urlMode);
      if (!baseV1) {
        logger.info('Fast connection test skipped model listing in full URL mode', {
          provider: this.config.provider,
          model: this.config.model,
          baseUrl: this.config.baseUrl,
        }, LogCategory.LLM);
        return { success: true, modelExists: undefined };
      }
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
        const response = await this.withRequestTimeout(params, (effectiveParams) => adapter.send(effectiveParams));
        this.conformanceValidator.validateResponse(response, adapter.protocol);
        this.logResponse(response);
        return response;
      } catch (error) {
        this.logError(error, 'sendMessage');
        throw error;
      }
    }, 'sendMessage', params.retryPolicy, params.retryRuntimeHook);
  }

  async streamMessage(
    params: LLMMessageParams,
    onChunk: (chunk: LLMStreamChunk) => void,
  ): Promise<LLMResponse> {
    this.logRequest({ ...params, stream: true });

    let hasReceivedData = false;
    const adapter = this.getProtocolAdapter();

    return this.withRetry(async () => {
      try {
        const response = await this.withStreamTimeout(
          params,
          (effectiveParams, notifyActivity) => adapter.stream(effectiveParams, (chunk) => {
            notifyActivity();
            this.conformanceValidator.validateStreamChunk(chunk, adapter.protocol);
            hasReceivedData = true;
            onChunk(chunk);
          }),
        );
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
    }, 'streamMessage', params.retryPolicy, params.retryRuntimeHook);
  }

  private async withRetry<T>(
    fn: () => Promise<T>,
    context: string,
    retryPolicy?: LLMMessageParams['retryPolicy'],
    retryRuntimeHook?: LLMMessageParams['retryRuntimeHook'],
  ): Promise<T> {
    const policy = this.resolveRetryPolicy(retryPolicy);
    let deterministicSignature = '';
    let deterministicStreak = 0;
    const startedAt = Date.now();
    let hasActiveRetryRuntime = false;

    const emitSettled = (outcome: 'success' | 'failed') => {
      if (!hasActiveRetryRuntime) {
        return;
      }
      this.emitRetryRuntimeEvent(retryRuntimeHook, { phase: 'settled', outcome }, context);
      hasActiveRetryRuntime = false;
    };

    for (let attempt = 0; attempt < policy.maxRetries; attempt++) {
      try {
        this.assertCircuitReady(context, policy);
      } catch (error) {
        emitSettled('failed');
        throw error;
      }

      if (attempt > 0) {
        this.emitRetryRuntimeEvent(retryRuntimeHook, {
          phase: 'attempt_started',
          attempt: attempt + 1,
          maxAttempts: policy.maxRetries,
        }, context);
        hasActiveRetryRuntime = true;
      }

      try {
        const result = await fn();
        this.recordRetrySuccess();
        emitSettled('success');
        return result;
      } catch (error: any) {
        if (error instanceof NonRetryableError) {
          emitSettled('failed');
          throw error.originalError || error;
        }
        if (this.isAbortLikeError(error)) {
          emitSettled('failed');
          throw error;
        }
        const timeoutBudgetExceeded = Date.now() - startedAt >= policy.maxRetryDurationMs;
        const signature = this.getDeterministicErrorSignature(error);
        if (signature && signature === deterministicSignature) {
          deterministicStreak += 1;
        } else if (signature) {
          deterministicSignature = signature;
          deterministicStreak = 1;
        } else {
          deterministicSignature = '';
          deterministicStreak = 0;
        }

        this.recordRetryFailure(policy);
        if (this.maybeOpenCircuit(policy, context)) {
          const circuitError = new Error(
            `上游模型通道短时熔断（${policy.circuitBreaker.cooldownMs}ms），请稍后重试`,
          );
          (circuitError as any).code = 'EUPSTREAM_CIRCUIT_OPEN';
          emitSettled('failed');
          throw circuitError;
        }

        const canRetry = this.shouldRetryError(error, {
          ...retryPolicy,
          retryOnAllErrors: policy.retryOnAllErrors,
          retryOnTimeout: policy.retryOnTimeout,
        });

        const deterministicBudgetExceeded = deterministicStreak >= policy.deterministicErrorStreakLimit;
        if (!canRetry || attempt === policy.maxRetries - 1 || timeoutBudgetExceeded || deterministicBudgetExceeded) {
          emitSettled('failed');
          throw error;
        }

        const scheduleDelay = policy.retryDelaysMs[attempt];
        const delay = typeof scheduleDelay === 'number' && scheduleDelay > 0
          ? scheduleDelay
          : policy.baseDelayMs > 0
            ? policy.baseDelayMs * Math.pow(2, attempt) + Math.floor(Math.random() * 200)
            : 0;
        this.logError(error, `${context}.retry_${attempt + 1}`);
        this.emitRetryRuntimeEvent(retryRuntimeHook, {
          phase: 'scheduled',
          attempt: attempt + 2,
          maxAttempts: policy.maxRetries,
          delayMs: delay,
          nextRetryAt: Date.now() + delay,
        }, context);
        hasActiveRetryRuntime = true;
        if (delay > 0) {
          await this.sleep(delay);
        }
      }
    }

    emitSettled('failed');
    throw new Error(`Retry failed: ${context}`);
  }

  private emitRetryRuntimeEvent(
    retryRuntimeHook: LLMMessageParams['retryRuntimeHook'] | undefined,
    event: LLMRetryRuntimeEvent,
    context: string,
  ): void {
    if (!retryRuntimeHook) {
      return;
    }

    try {
      retryRuntimeHook(event);
    } catch (error) {
      logger.warn('LLM retry runtime hook failed', {
        context,
        phase: event.phase,
        error: toErrorMessage(error),
      }, LogCategory.LLM);
    }
  }

  private async withRequestTimeout<T>(
    params: LLMMessageParams,
    run: (effectiveParams: LLMMessageParams) => Promise<T>,
  ): Promise<T> {
    const timeoutMs = params.timeoutMs;
    if (!timeoutMs || timeoutMs <= 0) {
      return run(params);
    }

    const controller = new AbortController();
    const abortFromParent = () => {
      const reason = (params.signal as any)?.reason;
      controller.abort(reason || new Error('Request aborted'));
    };

    if (params.signal?.aborted) {
      abortFromParent();
    } else if (params.signal) {
      params.signal.addEventListener('abort', abortFromParent, { once: true });
    }

    const timer = setTimeout(() => {
      const timeoutError = new Error(`Request timed out after ${timeoutMs}ms`);
      (timeoutError as any).code = 'ETIMEDOUT';
      controller.abort(timeoutError);
    }, timeoutMs);

    try {
      return await run({
        ...params,
        signal: controller.signal,
      });
    } catch (error: any) {
      if (controller.signal.aborted && !params.signal?.aborted) {
        const timeoutError = new Error(`Request timed out after ${timeoutMs}ms`);
        (timeoutError as any).code = 'ETIMEDOUT';
        throw timeoutError;
      }
      throw error;
    } finally {
      clearTimeout(timer);
      if (params.signal) {
        params.signal.removeEventListener('abort', abortFromParent);
      }
    }
  }

  private async withStreamTimeout<T>(
    params: LLMMessageParams,
    run: (effectiveParams: LLMMessageParams, notifyActivity: () => void) => Promise<T>,
  ): Promise<T> {
    const idleTimeoutMs = params.streamIdleTimeoutMs ?? params.timeoutMs;
    const hardTimeoutMs = params.streamHardTimeoutMs;
    const hasIdleTimeout = !!idleTimeoutMs && idleTimeoutMs > 0;
    const hasHardTimeout = !!hardTimeoutMs && hardTimeoutMs > 0;
    const effectiveIdleTimeoutMs = hasIdleTimeout ? idleTimeoutMs as number : 0;
    const effectiveHardTimeoutMs = hasHardTimeout ? hardTimeoutMs as number : 0;

    if (!hasIdleTimeout && !hasHardTimeout) {
      return run(params, () => {});
    }

    const controller = new AbortController();
    const abortFromParent = () => {
      const reason = (params.signal as any)?.reason;
      controller.abort(reason || new Error('Request aborted'));
    };

    if (params.signal?.aborted) {
      abortFromParent();
    } else if (params.signal) {
      params.signal.addEventListener('abort', abortFromParent, { once: true });
    }

    let timeoutKind: 'idle' | 'hard' | null = null;
    let idleTimer: ReturnType<typeof setTimeout> | null = null;
    let hardTimer: ReturnType<typeof setTimeout> | null = null;

    const abortByTimeout = (kind: 'idle' | 'hard') => {
      if (controller.signal.aborted) {
        return;
      }
      timeoutKind = kind;
      const timeoutValue = kind === 'idle' ? effectiveIdleTimeoutMs : effectiveHardTimeoutMs;
      const timeoutError = new Error(
        kind === 'idle'
          ? `Stream idle timed out after ${timeoutValue}ms`
          : `Stream timed out after ${timeoutValue}ms`,
      );
      (timeoutError as any).code = 'ETIMEDOUT';
      controller.abort(timeoutError);
    };

    const armIdleTimer = () => {
      if (!hasIdleTimeout) {
        return;
      }
      if (idleTimer) {
        clearTimeout(idleTimer);
      }
      idleTimer = setTimeout(() => {
        abortByTimeout('idle');
      }, effectiveIdleTimeoutMs);
    };

    if (hasHardTimeout) {
      hardTimer = setTimeout(() => {
        abortByTimeout('hard');
      }, effectiveHardTimeoutMs);
    }
    armIdleTimer();

    try {
      return await run(
        {
          ...params,
          signal: controller.signal,
        },
        () => {
          armIdleTimer();
        },
      );
    } catch (error) {
      if (controller.signal.aborted && !params.signal?.aborted && timeoutKind) {
        const timeoutValue = timeoutKind === 'idle' ? effectiveIdleTimeoutMs : effectiveHardTimeoutMs;
        const timeoutError = new Error(
          timeoutKind === 'idle'
            ? `Stream idle timed out after ${timeoutValue}ms`
            : `Stream timed out after ${timeoutValue}ms`,
        );
        (timeoutError as any).code = 'ETIMEDOUT';
        throw timeoutError;
      }
      throw error;
    } finally {
      if (idleTimer) {
        clearTimeout(idleTimer);
      }
      if (hardTimer) {
        clearTimeout(hardTimer);
      }
      if (params.signal) {
        params.signal.removeEventListener('abort', abortFromParent);
      }
    }
  }

  private isRetryableError(error: any): boolean {
    const status = error?.status || error?.response?.status;
    if (typeof status === 'number') {
      if ([408, 429].includes(status)) return true;
      if (status >= 500 && status <= 599) return true;
    }

    const code = error?.code;
    if (typeof code === 'string') {
      return ['ETIMEDOUT', 'ECONNRESET', 'ENOTFOUND', 'EAI_AGAIN', 'ECONNREFUSED'].includes(code);
    }

    const message = String(error?.message || '');
    return /timeout|timed out|connection|network|fetch failed|socket hang up|ECONNRESET|ENOTFOUND|EAI_AGAIN|ECONNREFUSED|request ended without sending|stream ended|overloaded|error occurred while processing your request|help\.openai\.com/i.test(message);
  }

  private isTimeoutError(error: any): boolean {
    const code = error?.code;
    if (typeof code === 'string' && code.toUpperCase() === 'ETIMEDOUT') {
      return true;
    }
    const message = String(error?.message || '').toLowerCase();
    return message.includes('timeout') || message.includes('timed out');
  }

  private isAbortLikeError(error: any): boolean {
    if (!error) {
      return false;
    }
    const name = String(error?.name || '');
    if (name === 'AbortError') {
      return true;
    }
    const code = String(error?.code || '').toUpperCase();
    if (code === 'ABORT_ERR') {
      return true;
    }
    const message = String(error?.message || '').toLowerCase();
    return message.includes('aborted') || message.includes('aborterror');
  }

  private shouldRetryError(error: any, retryPolicy?: LLMMessageParams['retryPolicy']): boolean {
    if (this.isAbortLikeError(error)) {
      return false;
    }

    const retryOnAllErrors = retryPolicy?.retryOnAllErrors ?? true;
    if (!retryOnAllErrors && !this.isRetryableError(error)) {
      return false;
    }

    if (retryPolicy?.retryOnTimeout === false && this.isTimeoutError(error)) {
      return false;
    }

    return true;
  }

  private resolveRetryPolicy(retryPolicy?: LLMMessageParams['retryPolicy']): EffectiveRetryPolicy {
    const circuitBreaker = retryPolicy?.circuitBreaker || {};
    return {
      maxRetries: Math.max(1, retryPolicy?.maxRetries ?? 3),
      baseDelayMs: Math.max(0, retryPolicy?.baseDelayMs ?? 500),
      retryDelaysMs: Array.isArray(retryPolicy?.retryDelaysMs)
        ? retryPolicy!.retryDelaysMs!.filter((value) => Number.isFinite(value) && value > 0).map((value) => Math.floor(value))
        : [],
      retryOnTimeout: retryPolicy?.retryOnTimeout ?? true,
      retryOnAllErrors: retryPolicy?.retryOnAllErrors ?? true,
      maxRetryDurationMs: Math.max(1000, retryPolicy?.maxRetryDurationMs ?? 30_000),
      deterministicErrorStreakLimit: Math.max(1, retryPolicy?.deterministicErrorStreakLimit ?? 3),
      circuitBreaker: {
        enabled: circuitBreaker.enabled ?? true,
        windowMs: Math.max(1000, circuitBreaker.windowMs ?? 20_000),
        failureThreshold: Math.max(1, circuitBreaker.failureThreshold ?? 8),
        cooldownMs: Math.max(1000, circuitBreaker.cooldownMs ?? 15_000),
      },
    };
  }

  private getDeterministicErrorSignature(error: any): string | null {
    const status = error?.status || error?.response?.status;
    if (typeof status === 'number') {
      if (status >= 500 || status === 408 || status === 429) {
        return null;
      }
      return `status:${status}`;
    }

    const code = String(error?.code || '').toUpperCase();
    if (['ETIMEDOUT', 'ECONNRESET', 'ENOTFOUND', 'EAI_AGAIN', 'ECONNREFUSED'].includes(code)) {
      return null;
    }

    const message = String(error?.message || '').toLowerCase();
    if (
      message.includes('unauthorized')
      || message.includes('forbidden')
      || message.includes('invalid api key')
      || message.includes('api key invalid')
      || message.includes('permission')
      || message.includes('insufficient_quota')
      || message.includes('quota')
      || message.includes('billing')
      || message.includes('not found')
      || message.includes('unsupported')
      || message.includes('invalid model')
      || message.includes('invalid_request')
    ) {
      return `msg:${message.slice(0, 120)}`;
    }
    return null;
  }

  private assertCircuitReady(context: string, policy: EffectiveRetryPolicy): void {
    if (!policy.circuitBreaker.enabled) {
      return;
    }
    const now = Date.now();
    if (this.retryResilienceState.circuitOpenUntil > now) {
      const remainMs = this.retryResilienceState.circuitOpenUntil - now;
      const circuitError = new Error(
        `上游模型通道熔断中，剩余冷却 ${remainMs}ms`,
      );
      (circuitError as any).code = 'EUPSTREAM_CIRCUIT_OPEN';
      logger.warn('LLM 通道熔断拦截请求', {
        provider: this.config.provider,
        model: this.config.model,
        context,
        remainMs,
      }, LogCategory.LLM);
      throw circuitError;
    }
  }

  private recordRetryFailure(policy: EffectiveRetryPolicy): void {
    if (!policy.circuitBreaker.enabled) {
      return;
    }
    const now = Date.now();
    const cutoff = now - policy.circuitBreaker.windowMs;
    this.retryResilienceState.recentFailureTimestamps = this.retryResilienceState.recentFailureTimestamps
      .filter((item) => item >= cutoff);
    this.retryResilienceState.recentFailureTimestamps.push(now);
  }

  private maybeOpenCircuit(policy: EffectiveRetryPolicy, context: string): boolean {
    if (!policy.circuitBreaker.enabled) {
      return false;
    }
    const now = Date.now();
    const failures = this.retryResilienceState.recentFailureTimestamps.length;
    if (failures < policy.circuitBreaker.failureThreshold) {
      return false;
    }
    const openUntil = now + policy.circuitBreaker.cooldownMs;
    if (openUntil <= this.retryResilienceState.circuitOpenUntil) {
      return false;
    }
    this.retryResilienceState.circuitOpenUntil = openUntil;
    logger.warn('LLM 通道触发短时熔断', {
      provider: this.config.provider,
      model: this.config.model,
      context,
      failuresInWindow: failures,
      windowMs: policy.circuitBreaker.windowMs,
      cooldownMs: policy.circuitBreaker.cooldownMs,
    }, LogCategory.LLM);
    return true;
  }

  private recordRetrySuccess(): void {
    this.retryResilienceState.recentFailureTimestamps = [];
  }

  private sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }
}
