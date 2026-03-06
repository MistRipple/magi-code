import { logger, LogCategory } from '../logging';
import { classifyModelOriginIssue, type ModelOriginIssueKind } from './model-origin';

export type ModelOriginEventStage = 'detected' | 'recovered' | 'escalated' | 'surfaced';

interface ModelOriginMetrics {
  total: number;
  byStage: Record<ModelOriginEventStage, number>;
  byKind: Record<ModelOriginIssueKind, number>;
  byLayer: Record<string, number>;
}

const EMPTY_STAGE_COUNTER = (): Record<ModelOriginEventStage, number> => ({
  detected: 0,
  recovered: 0,
  escalated: 0,
  surfaced: 0,
});

const EMPTY_KIND_COUNTER = (): Record<ModelOriginIssueKind, number> => ({
  prefixed: 0,
  auth: 0,
  quota: 0,
  rate_limit: 0,
  context_limit: 0,
  model_unavailable: 0,
  timeout: 0,
  network: 0,
  empty_response: 0,
  tool_param_parse: 0,
  reasoning_leak: 0,
  unknown: 0,
});

const metrics: ModelOriginMetrics = {
  total: 0,
  byStage: EMPTY_STAGE_COUNTER(),
  byKind: EMPTY_KIND_COUNTER(),
  byLayer: {},
};

function safeKind(kind?: ModelOriginIssueKind): ModelOriginIssueKind {
  return kind || 'unknown';
}

/**
 * 记录模型异常治理事件（仅内存统计 + 结构化日志）
 * 作用：为后续观测/告警提供统一计数口径，避免“修了但不可见”。
 */
export function trackModelOriginEvent(
  stage: ModelOriginEventStage,
  layer: string,
  reason: string,
  details?: Record<string, unknown>,
): void {
  const classified = classifyModelOriginIssue(reason);
  if (!classified.isModelCause) {
    return;
  }

  const kind = safeKind(classified.kind);
  metrics.total += 1;
  metrics.byStage[stage] += 1;
  metrics.byKind[kind] += 1;
  metrics.byLayer[layer] = (metrics.byLayer[layer] || 0) + 1;

  const payload = {
    stage,
    layer,
    kind,
    reason: classified.normalized,
    surfaced: classified.message,
    metrics: {
      total: metrics.total,
      byStage: metrics.byStage,
      byKind: metrics.byKind,
    },
    ...details,
  };

  if (stage === 'escalated') {
    logger.warn('模型异常治理事件', payload, LogCategory.LLM);
    return;
  }
  logger.info('模型异常治理事件', payload, LogCategory.LLM);
}

export function getModelOriginMetricsSnapshot(): ModelOriginMetrics {
  return {
    total: metrics.total,
    byStage: { ...metrics.byStage },
    byKind: { ...metrics.byKind },
    byLayer: { ...metrics.byLayer },
  };
}
