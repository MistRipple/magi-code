import type { Message, WaitForWorkersResult } from '../types/message';
import {
  buildTimelineAssignmentTaskKey,
  resolveTimelineTaskCardScopeId,
  resolveTimelineWorkerLifecycleKey,
} from '../../../../shared/timeline-worker-lifecycle';
import { normalizeWorkerSlot } from './message-classifier';

export function resolveTaskCardScopeId(meta: Record<string, unknown> | undefined): string {
  return resolveTimelineTaskCardScopeId(meta);
}

export function buildAssignmentTaskCardKey(assignmentKey: string, scopeId?: string): string {
  return buildTimelineAssignmentTaskKey(assignmentKey, scopeId);
}

export function resolveTaskCardKeyFromMetadata(meta: Record<string, unknown> | undefined): string {
  return resolveTimelineWorkerLifecycleKey(meta);
}

export function buildWaitResultFromTaskCardMessage(
  message: Pick<Message, 'id' | 'type' | 'timestamp' | 'metadata'>,
): { cardKey: string; result: WaitForWorkersResult } | null {
  if (message.type !== 'task_card' && message.type !== 'instruction') return null;
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
    cancelled: 'cancelled',
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
