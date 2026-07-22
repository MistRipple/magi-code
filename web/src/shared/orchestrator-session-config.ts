export type OrchestratorReasoningEffort = 'low' | 'medium' | 'high' | 'xhigh';

export const DEFAULT_ORCHESTRATOR_REASONING_EFFORT: OrchestratorReasoningEffort = 'medium';

export function normalizeOrchestratorReasoningEffort(
  value: unknown,
): OrchestratorReasoningEffort | null {
  return value === 'low' || value === 'medium' || value === 'high' || value === 'xhigh'
    ? value
    : null;
}

export function resolveOrchestratorReasoningEffort(
  sessionConfig: Record<string, unknown> | null | undefined,
  effectiveConfig: Record<string, unknown> | null | undefined,
): OrchestratorReasoningEffort {
  return normalizeOrchestratorReasoningEffort(sessionConfig?.reasoningEffort)
    ?? normalizeOrchestratorReasoningEffort(effectiveConfig?.reasoningEffort)
    ?? DEFAULT_ORCHESTRATOR_REASONING_EFFORT;
}

export function resolveOrchestratorModel(
  sessionConfig: Record<string, unknown> | null | undefined,
  effectiveConfig: Record<string, unknown> | null | undefined,
  availableModels: readonly string[],
): string {
  const candidates = [
    sessionConfig?.model,
    effectiveConfig?.model,
    ...availableModels,
  ];
  for (const candidate of candidates) {
    if (typeof candidate === 'string' && candidate.trim()) {
      return candidate.trim();
    }
  }
  return '';
}

export function withOrchestratorReasoningEffort(
  currentConfig: Record<string, unknown>,
  reasoningEffort: OrchestratorReasoningEffort,
  patch: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    ...currentConfig,
    ...patch,
    reasoningEffort,
  };
}

/**
 * 提取新建会话需要继承的会话级主模型配置。
 * 只复制模型和思考强度，不携带连接地址、密钥等全局配置。
 */
export function copyOrchestratorSessionConfig(
  sessionConfig: Record<string, unknown> | null | undefined,
  effectiveConfig: Record<string, unknown> | null | undefined,
): Record<string, unknown> {
  const model = resolveOrchestratorModel(sessionConfig, effectiveConfig, []);
  const reasoningEffort = resolveOrchestratorReasoningEffort(sessionConfig, effectiveConfig);
  return {
    ...(model ? { model } : {}),
    reasoningEffort,
  };
}
