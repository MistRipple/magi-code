/**
 * Knowledge Extraction Client Factory - 知识提取 LLM 客户端工厂
 *
 * 从 WebviewProvider 提取的业务逻辑（P1-1 修复）。
 * 职责：创建带执行统计的 LLM 客户端，用于知识库自动提取。
 */

import { logger, LogCategory } from '../logging';
import type { LLMClient, LLMMessageParams, LLMStreamChunk } from '../llm/types';
import type { ExecutionStats } from '../orchestrator/execution-stats';

/**
 * 创建带执行统计的知识提取 LLM 客户端
 *
 * @param executionStats 执行统计实例（可选，用于记录调用指标）
 * @returns 包装后的 LLMClient，在 sendMessage/streamMessage 时自动记录统计
 */
export async function createKnowledgeExtractionClient(
  executionStats?: ExecutionStats | null,
): Promise<LLMClient> {
  const { LLMConfigLoader } = await import('../llm/config');
  const { UniversalLLMClient } = await import('../llm/clients/universal-client');

  // 加载配置：优先 auxiliary，回退 orchestrator
  const auxiliaryConfig = LLMConfigLoader.loadAuxiliaryConfig();
  const orchestratorConfig = LLMConfigLoader.loadOrchestratorConfig();

  const useAuxiliary = auxiliaryConfig.enabled
    && Boolean(auxiliaryConfig.baseUrl && auxiliaryConfig.model);
  const activeConfig = useAuxiliary ? auxiliaryConfig : orchestratorConfig;
  const activeLabel = useAuxiliary ? 'auxiliary' : 'orchestrator';

  // 创建基础客户端
  const baseClient = new UniversalLLMClient({
    baseUrl: activeConfig.baseUrl,
    apiKey: activeConfig.apiKey,
    model: activeConfig.model,
    provider: activeConfig.provider,
    enabled: true,
  });

  // 包装带执行统计的客户端
  const client: LLMClient = {
    config: baseClient.config,
    sendMessage: async (params: LLMMessageParams) => {
      const startedAt = Date.now();
      try {
        const response = await baseClient.sendMessage(params);
        const duration = Date.now() - startedAt;
        if (executionStats) {
          executionStats.recordExecution({
            worker: activeLabel,
            taskId: 'knowledge',
            subTaskId: 'extract',
            success: true,
            duration,
            inputTokens: response.usage?.inputTokens,
            outputTokens: response.usage?.outputTokens,
            phase: 'integration',
          });
        }
        return response;
      } catch (error: unknown) {
        const duration = Date.now() - startedAt;
        if (executionStats) {
          executionStats.recordExecution({
            worker: activeLabel,
            taskId: 'knowledge',
            subTaskId: 'extract',
            success: false,
            duration,
            error: error instanceof Error ? error.message : String(error),
            phase: 'integration',
          });
        }
        throw error;
      }
    },
    streamMessage: async (params: LLMMessageParams, onChunk: (chunk: LLMStreamChunk) => void) => {
      const startedAt = Date.now();
      try {
        const response = await baseClient.streamMessage(params, onChunk);
        const duration = Date.now() - startedAt;
        if (executionStats) {
          executionStats.recordExecution({
            worker: activeLabel,
            taskId: 'knowledge',
            subTaskId: 'extract',
            success: true,
            duration,
            inputTokens: response.usage?.inputTokens,
            outputTokens: response.usage?.outputTokens,
            phase: 'integration',
          });
        }
        return response;
      } catch (error: unknown) {
        const duration = Date.now() - startedAt;
        if (executionStats) {
          executionStats.recordExecution({
            worker: activeLabel,
            taskId: 'knowledge',
            subTaskId: 'extract',
            success: false,
            duration,
            error: error instanceof Error ? error.message : String(error),
            phase: 'integration',
          });
        }
        throw error;
      }
    },
    testConnection: baseClient.testConnection.bind(baseClient),
    testConnectionFast: baseClient.testConnectionFast.bind(baseClient),
  };

  logger.info('知识提取客户端.已创建', {
    model: activeConfig.model,
    provider: activeConfig.provider,
    fallbackToOrchestrator: !useAuxiliary,
  }, LogCategory.SESSION);

  return client;
}
