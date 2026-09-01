export const RUNTIME_TASK_STATUSES = [
  'pending',
  'running',
  'completed',
  'failed',
  'killed',
] as const;

export type RuntimeTaskStatus = typeof RUNTIME_TASK_STATUSES[number];

export function parseRuntimeTaskStatus(value: unknown): RuntimeTaskStatus | null {
  if (typeof value !== 'string') {
    return null;
  }
  const normalized = value.trim().toLowerCase();
  switch (normalized) {
    case 'pending':
    case 'running':
    case 'completed':
    case 'failed':
    case 'killed':
      return normalized;
    default:
      return null;
  }
}

export function isTerminalRuntimeTaskStatus(value: unknown): boolean {
  const status = parseRuntimeTaskStatus(value);
  return status === 'completed' || status === 'failed' || status === 'killed';
}
