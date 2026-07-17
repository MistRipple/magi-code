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
