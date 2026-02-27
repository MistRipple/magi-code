/**
 * ResilientCompressorAdapter - 弹性上下文压缩适配器
 *
 * 职责：
 * - 配置 ContextManager 的压缩适配器
 * - 压缩模型不可用时自动切换到编排模型
 * - 连接失败时重试（瞬态故障容错）
 * - 认证/配额错误时立即失败（不重试）
 */

import { logger, LogCategory } from '../../logging';
import type { ContextManager } from '../../context/context-manager';
import type { ExecutionStats } from '../execution-stats';
import type { LLMClient } from '../../llm/types';

// ============================================================================
// 错误分类工具
// ============================================================================

/** 从 unknown error 中安全提取 HTTP 状态码 */
function getErrorStatus(error: unknown): number | undefined {
  if (!error || typeof error !== 'object') return undefined;
  const e = error as { status?: unknown; response?: { status?: unknown } };
  if (typeof e.status === 'number') return e.status;
  if (typeof e.response?.status === 'number') return e.response.status;
  return undefined;
}

/** 从 unknown error 中安全提取错误码 */
function getErrorCode(error: unknown): string {
  if (!error || typeof error !== 'object') return '';
  const code = (error as { code?: unknown }).code;
  return typeof code === 'string' ? code : '';
}

function normalizeErrorMessage(error: unknown): string {
  if (!error) return 'Unknown error';
  if (typeof error === 'string') return error;
  if (error instanceof Error && error.message) return error.message;
  if (typeof error === 'object' && 'message' in error && typeof (error as { message: unknown }).message === 'string') {
    return (error as { message: string }).message;
  }
  return String(error);
}

function isAuthOrQuotaError(error: unknown): boolean {
  const status = getErrorStatus(error);
  if (status === 401 || status === 403 || status === 429) return true;
  const message = normalizeErrorMessage(error).toLowerCase();
  return /unauthorized|forbidden|invalid api key|api key|auth|permission|quota|insufficient|billing|payment|exceeded|rate limit|limit|blocked|suspended|disabled|account/i.test(message);
}

function isConnectionError(error: unknown): boolean {
  const status = getErrorStatus(error);
  if (status === 408 || status === 502 || status === 503 || status === 504) return true;
  const code = getErrorCode(error);
  if (['ETIMEDOUT', 'ECONNRESET', 'ECONNREFUSED', 'ENOTFOUND', 'EAI_AGAIN'].includes(code)) {
    return true;
  }
  const message = normalizeErrorMessage(error).toLowerCase();
  return /timeout|timed out|network|connection|fetch failed|socket hang up|tls|certificate|econnreset|econnrefused|enotfound|eai_again/.test(message);
}

function isModelError(error: unknown): boolean {
  const message = normalizeErrorMessage(error).toLowerCase();
  return /model|not found|unknown model|invalid model|unsupported model|no such model/.test(message);
}

function isConfigError(error: unknown): boolean {
  const message = normalizeErrorMessage(error).toLowerCase();
  return /disabled in config|invalid configuration|missing|not configured|config/.test(message);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ============================================================================
// 核心配置函数
// ============================================================================

/**
 * 为 ContextManager 配置弹性压缩适配器
 *
 * 策略：
 * 1. 优先使用专用压缩模型
 * 2. 压缩模型不可用/失败时，自动切换编排模型
 * 3. 连接失败时最多重试 3 次（10s/20s/30s 间隔）
 * 4. 认证/配额错误立即抛出（不重试）
 */
export async function configureResilientCompressor(
  contextManager: ContextManager,
  executionStats: ExecutionStats,
): Promise<void> {
  try {
    const { LLMConfigLoader } = await import('../../llm/config');
    const { createLLMClient } = await import('../../llm/clients/client-factory');
    const compressorConfig = LLMConfigLoader.loadCompressorConfig();
    const orchestratorConfig = LLMConfigLoader.loadOrchestratorConfig();

    const compressorReady = compressorConfig.enabled
      && Boolean(compressorConfig.baseUrl && compressorConfig.model)
      && LLMConfigLoader.validateConfig(compressorConfig, 'compressor');

    if (!compressorReady) {
      logger.warn('编排器.上下文.压缩模型.不可用_切换编排模型', {
        enabled: compressorConfig.enabled,
        hasBaseUrl: Boolean(compressorConfig.baseUrl),
        hasModel: Boolean(compressorConfig.model),
      }, LogCategory.ORCHESTRATOR);
    }

    const retryDelays = [10000, 20000, 30000];

    const recordCompression = (
      success: boolean,
      duration: number,
      usage?: {
        inputTokens?: number;
        outputTokens?: number;
      },
      error?: string
    ) => {
      executionStats.recordExecution({
        worker: 'compressor',
        taskId: 'memory',
        subTaskId: 'compress',
        success,
        duration,
        error,
        inputTokens: usage?.inputTokens,
        outputTokens: usage?.outputTokens,
        phase: 'integration',
      });
    };

    const sendWithClient = async (client: LLMClient, label: string, payload: string): Promise<string> => {
      const startAt = Date.now();
      try {
        const response = await client.sendMessage({
          messages: [{ role: 'user', content: payload }],
          maxTokens: 2000,
          temperature: 0.3,
        });
        const duration = Date.now() - startAt;
        recordCompression(true, duration, {
          inputTokens: response.usage?.inputTokens,
          outputTokens: response.usage?.outputTokens,
        });
        return response.content || '';
      } catch (error: unknown) {
        const duration = Date.now() - startAt;
        recordCompression(false, duration, undefined, normalizeErrorMessage(error));
        logger.warn('编排器.上下文.压缩模型.调用失败', {
          model: label,
          error: normalizeErrorMessage(error),
        }, LogCategory.ORCHESTRATOR);
        throw error;
      }
    };

    const sendWithRetry = async (client: LLMClient, label: string, payload: string): Promise<string> => {
      for (let attempt = 0; attempt <= retryDelays.length; attempt++) {
        try {
          return await sendWithClient(client, label, payload);
        } catch (error: unknown) {
          if (isAuthOrQuotaError(error)) {
            throw error;
          }
          if (!isConnectionError(error) || attempt === retryDelays.length) {
            throw error;
          }
          const delay = retryDelays[attempt];
          logger.warn('编排器.上下文.压缩模型.连接失败_重试', {
            attempt: attempt + 1,
            delayMs: delay,
            error: normalizeErrorMessage(error),
            model: label,
          }, LogCategory.ORCHESTRATOR);
          await sleep(delay);
        }
      }
      throw new Error('Compression retry failed.');
    };

    const adapter = {
      sendMessage: async (message: string) => {
        try {
          if (!compressorReady) {
            throw new Error('compressor_unavailable');
          }
          const client = createLLMClient(compressorConfig);
          return await sendWithRetry(client, 'compressor', message);
        } catch (error: unknown) {
          const shouldSwitchToOrchestrator = !compressorReady
            || isAuthOrQuotaError(error)
            || isConnectionError(error)
            || isModelError(error)
            || isConfigError(error);
          if (!shouldSwitchToOrchestrator) {
            throw error;
          }
          logger.warn('编排器.上下文.压缩模型.切换_使用编排模型', {
            reason: !compressorReady ? 'not_available'
              : isAuthOrQuotaError(error) ? 'auth_or_quota'
              : isConnectionError(error) ? 'connection'
              : isModelError(error) ? 'model'
              : 'config',
            error: normalizeErrorMessage(error),
          }, LogCategory.ORCHESTRATOR);
          const orchestratorClient = createLLMClient(orchestratorConfig);
          return await sendWithRetry(orchestratorClient, 'orchestrator', message);
        }
      },
    };

    contextManager.setCompressorAdapter(adapter);
    const activeConfig = compressorReady ? compressorConfig : orchestratorConfig;
    logger.info('编排器.上下文.压缩模型.已设置', {
      model: activeConfig.model,
      provider: activeConfig.provider,
      useOrchestratorModel: !compressorReady,
    }, LogCategory.ORCHESTRATOR);
  } catch (error) {
    logger.error('编排器.上下文.压缩模型.设置失败', error, LogCategory.ORCHESTRATOR);
  }
}
