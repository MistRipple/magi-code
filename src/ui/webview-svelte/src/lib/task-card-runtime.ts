import type { Message, WaitForWorkersResult } from '../types/message';
import { normalizeWorkerSlot } from './message-classifier';

function normalizeTaskCardKey(raw: unknown): string {
  if (typeof raw !== 'string') return '';
  const trimmed = raw.trim();
  return trimmed.length > 0 ? trimmed : '';
}

export function resolveTaskCardScopeId(meta: Record<string, unknown> | undefined): string {
  const requestId = typeof meta?.requestId === 'string' ? meta.requestId.trim() : '';
  if (requestId) return requestId;
  const missionId = typeof meta?.missionId === 'string' ? meta.missionId.trim() : '';
  return missionId || '';
}

export function buildAssignmentTaskCardKey(assignmentKey: string, scopeId?: string): string {
  const normalizedAssignment = assignmentKey.trim();
  if (!normalizedAssignment) return '';
  const normalizedScope = typeof scopeId === 'string' ? scopeId.trim() : '';
  return normalizedScope ? `assign:${normalizedAssignment}@${normalizedScope}` : `assign:${normalizedAssignment}`;
}

export function resolveTaskCardKeyFromMetadata(meta: Record<string, unknown> | undefined): string {
  const scopeId = resolveTaskCardScopeId(meta);
  const rawAssignmentId = normalizeTaskCardKey(meta?.assignmentId);
  if (rawAssignmentId) return buildAssignmentTaskCardKey(rawAssignmentId, scopeId);
  const rawSubTaskId = normalizeTaskCardKey(meta?.subTaskId);
  if (rawSubTaskId) return buildAssignmentTaskCardKey(rawSubTaskId, scopeId);
  const rawSubTaskCardId = normalizeTaskCardKey((meta?.subTaskCard as { id?: unknown } | undefined)?.id);
  if (rawSubTaskCardId) return buildAssignmentTaskCardKey(rawSubTaskCardId, scopeId);
  const rawCardId = normalizeTaskCardKey(meta?.cardId);
  if (rawCardId) return rawCardId;
  return '';
}

export function buildWaitResultFromTaskCardMessage(
  message: Pick<Message, 'id' | 'type' | 'timestamp' | 'metadata'>,
): { cardKey: string; result: WaitForWorkersResult } | null {
  if (message.type !== 'task_card') return null;
  const meta = message.metadata as Record<string, unknown> | undefined;
  const subTaskCard = (meta?.subTaskCard || {}) as Record<string, unknown>;
  const rawWorker = (subTaskCard.worker as string | undefined) || (meta?.assignedWorker as string | undefined);
  const worker = normalizeWorkerSlot(rawWorker);
  if (!worker) return null;
  const cardKey = resolveTaskCardKeyFromMetadata(meta);
  if (!cardKey) return null;

  const statusRaw = typeof subTaskCard.status === 'string' ? subTaskCard.status : '';
  const statusMap = {
    completed: 'completed',
    failed: 'failed',
    skipped: 'skipped',
    stopped: 'cancelled',
  } as const;
  const mappedStatus = statusMap[statusRaw as keyof typeof statusMap];
  if (!mappedStatus) return null;

  const summary = typeof subTaskCard.summary === 'string'
    ? subTaskCard.summary
    : (typeof subTaskCard.error === 'string' ? subTaskCard.error : '');
  const modifiedFiles = Array.isArray(subTaskCard.modifiedFiles)
    ? subTaskCard.modifiedFiles.filter((file): file is string => typeof file === 'string' && file.trim().length > 0)
    : [];
  const errors = typeof subTaskCard.error === 'string' && subTaskCard.error.trim()
    ? [subTaskCard.error.trim()]
    : undefined;

  return {
    cardKey,
    result: {
      results: [{
        task_id: String(subTaskCard.id || message.id || ''),
        worker,
        status: mappedStatus,
        summary,
        modified_files: modifiedFiles,
        ...(errors ? { errors } : {}),
      }],
      wait_status: 'completed',
      timed_out: false,
      pending_task_ids: [],
      waited_ms: 0,
      updatedAt: message.timestamp || Date.now(),
    },
  };
}
