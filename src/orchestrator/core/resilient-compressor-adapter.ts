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

// ============================================================================
// 错误分类工具
// ============================================================================

function normalizeErrorMessage(error: any): string {
  if (!error) return 'Unknown error';
  if (typeof error === 'string') return error;
  if (error instanceof Error && error.message) return error.message;
  if (error?.message) return String(error.message);
  return String(error);
}

function isAuthOrQuotaError(error: any): boolean {
  const status = error?.status || error?.response?.status;
  if (status === 401 || status === 403 || status === 429) return true;
  const message = normalizeErrorMessage(error).toLowerCase();
  return /unauthorized|forbidden|invalid api key|api key|auth|permission|quota|insufficient|billing|payment|exceeded|rate limit|limit|blocked|suspended|disabled|account/i.test(message);
}

function isConnectionError(error: any): boolean {
  const status = error?.status || error?.response?.status;
  if (status === 408 || status === 502 || status === 503 || status === 504) return true;
  const code = typeof error?.code === 'string' ? error.code : '';
  if (['ETIMEDOUT', 'ECONNRESET', 'ECONNREFUSED', 'ENOTFOUND', 'EAI_AGAIN'].includes(code)) {
    return true;
  }
  const message = normalizeErrorMessage(error).toLowerCase();
  return /timeout|timed out|network|connection|fetch failed|socket hang up|tls|certificate|econnreset|econnrefused|enotfound|eai_again/.test(message);
}

function isModelError(error: any): boolean {
  const message = normalizeErrorMessage(error).toLowerCase();
  return /model|not found|unknown model|invalid model|unsupported model|no such model/.test(message);
}

function isConfigError(error: any): boolean {
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
      usage?: { inputTokens?: number; outputTokens?: number },
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

    const sendWithClient = async (client: any, label: string, payload: string): Promise<string> => {
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
      } catch (error: any) {
        const duration = Date.now() - startAt;
        recordCompression(false, duration, undefined, error?.message);
        logger.warn('编排器.上下文.压缩模型.调用失败', {
          model: label,
          error: normalizeErrorMessage(error),
        }, LogCategory.ORCHESTRATOR);
        throw error;
      }
    };

    const sendWithRetry = async (client: any, label: string, payload: string): Promise<string> => {
      for (let attempt = 0; attempt <= retryDelays.length; attempt++) {
        try {
          return await sendWithClient(client, label, payload);
        } catch (error: any) {
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
        } catch (error: any) {
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
