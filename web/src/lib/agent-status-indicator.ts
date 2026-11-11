import type { ModelStatusType } from '../types/message';

export type AgentIndicatorVariant = 'brand' | 'disabled' | 'warning' | 'error';

export function resolveAgentIndicatorVariant(
  status?: ModelStatusType | string,
): AgentIndicatorVariant {
  if (
    status === 'available' ||
    status === 'connected' ||
    status === 'configured'
  ) {
    return 'brand';
  }

  if (status === 'checking' || status === 'orchestrator') {
    return 'warning';
  }

  if (status === 'disabled' || status === 'not_configured') {
    return 'disabled';
  }

  return 'error';
}

