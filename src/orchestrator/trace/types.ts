import type { WorkerSlot } from '../../types';

export interface OrchestrationTraceLinks {
  sessionId?: string;
  turnId?: string;
  planId?: string;
  missionId?: string;
  requestId?: string;
  batchId?: string;
  assignmentId?: string;
  todoId?: string;
  verificationId?: string;
  workerId?: WorkerSlot | 'orchestrator';
}

function normalizeString(value: unknown): string | undefined {
  if (typeof value !== 'string') {
    return undefined;
  }
  const normalized = value.trim();
  return normalized || undefined;
}

function normalizeWorkerId(value: unknown): WorkerSlot | 'orchestrator' | undefined {
  const normalized = normalizeString(value);
  if (!normalized) {
    return undefined;
  }
  if (normalized === 'orchestrator' || normalized === 'claude' || normalized === 'codex' || normalized === 'gemini') {
    return normalized;
  }
  return undefined;
}

export function normalizeOrchestrationTraceLinks(
  input?: Partial<OrchestrationTraceLinks> | null,
): OrchestrationTraceLinks | undefined {
  if (!input) {
    return undefined;
  }
  const normalized: OrchestrationTraceLinks = {
    sessionId: normalizeString(input.sessionId),
    turnId: normalizeString(input.turnId),
    planId: normalizeString(input.planId),
    missionId: normalizeString(input.missionId),
    requestId: normalizeString(input.requestId),
    batchId: normalizeString(input.batchId),
    assignmentId: normalizeString(input.assignmentId),
    todoId: normalizeString(input.todoId),
    verificationId: normalizeString(input.verificationId),
    workerId: normalizeWorkerId(input.workerId),
  };
  return Object.values(normalized).some(Boolean) ? normalized : undefined;
}

export function mergeOrchestrationTraceLinks(
  base?: Partial<OrchestrationTraceLinks> | null,
  overrides?: Partial<OrchestrationTraceLinks> | null,
): OrchestrationTraceLinks | undefined {
  return normalizeOrchestrationTraceLinks({
    ...(base || {}),
    ...(overrides || {}),
  });
}

export function buildVerificationId(batchId: string): string {
  const normalizedBatchId = normalizeString(batchId) || 'unknown-batch';
  return `verification:${normalizedBatchId}`;
}
