
import type { ModelStatusType } from '../types/message';

export type AgentIndicatorVariant = 'brand' | 'disabled' | 'warning' | 'error';
export type WorkerRuntimeIndicatorVariant = 'brand' | 'warning' | 'error' | 'disabled' | null;

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

export function resolveWorkerRuntimeIndicatorVariant(
  status?: string | null,
): WorkerRuntimeIndicatorVariant {
  switch (status) {
    case 'pending':
    case 'running':
      return 'brand';
    case 'awaiting_approval':
    case 'review_required':
      return 'warning';
    case 'blocked':
    case 'failed':
    case 'cancelled':
      return 'error';
    case 'disabled':
      return 'disabled';
    case 'completed':
    case 'idle':
    case 'skipped':
    default:
      return null;
  }
}

