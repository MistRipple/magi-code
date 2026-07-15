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
