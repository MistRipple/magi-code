import { t } from '../../../i18n';
import type { WaitForWorkersResult } from '../../../tools/orchestration-executor';
import type { MessageHub } from '../message/message-hub';
import { DispatchCompletionQueue } from './dispatch-completion-queue';
import { DispatchBatch, type DispatchEntry, type DispatchStatus } from './dispatch-batch';

export interface DispatchReactiveWaitCoordinatorDeps {
  messageHub: MessageHub;
  getIdleTimeoutMs: () => number;
}

export class DispatchReactiveWaitCoordinator {
  private readonly completionQueue = new DispatchCompletionQueue();
  private reactiveMode = false;
  private readonly batchesAwaitingSummary = new Set<string>();

  constructor(
    private readonly deps: DispatchReactiveWaitCoordinatorDeps,
  ) {}

  resetForNewExecutionCycle(activeBatch?: DispatchBatch | null): void {
    if (activeBatch) {
      this.batchesAwaitingSummary.delete(activeBatch.id);
    }
    this.reactiveMode = false;
    this.completionQueue.reset();
  }

  resetForNextBatch(): void {
    this.completionQueue.reset();
  }

  pushCompletionEntry(entry: DispatchEntry): void {
    this.completionQueue.push(entry);
  }

  isReactiveMode(): boolean {
    return this.reactiveMode;
  }

  markBatchAwaitingSummary(batchId: string): void {
    this.batchesAwaitingSummary.add(batchId);
  }

  clearBatchAwaitingSummary(batchId: string): void {
    this.batchesAwaitingSummary.delete(batchId);
  }

  isBatchAwaitingSummary(batchId: string): boolean {
    return this.batchesAwaitingSummary.has(batchId);
  }

  markBatchSummarized(batchId: string): void {
    this.batchesAwaitingSummary.delete(batchId);
  }

  async waitForWorkers(
    batch: DispatchBatch | null,
    taskIds?: string[],
  ): Promise<WaitForWorkersResult> {
    this.reactiveMode = true;
    const normalizedTaskIds = Array.isArray(taskIds)
      ? Array.from(new Set(taskIds.map((item) => item.trim()).filter(Boolean)))
      : [];
    if (!batch || batch.size === 0) {
      throw new Error(t('dispatch.errors.workerWaitWithoutActiveBatch'));
    }

    if (normalizedTaskIds.length > 0) {
      const missingTaskIds = normalizedTaskIds.filter((taskId) => !batch.getEntry(taskId));
      if (missingTaskIds.length > 0) {
        throw new Error(t('dispatch.errors.workerWaitUnknownTasks', {
          taskIds: missingTaskIds.join(', '),
        }));
      }
    }

    const waitResult = await this.completionQueue.waitFor(
      batch,
      normalizedTaskIds.length > 0 ? normalizedTaskIds : undefined,
      {
      idleTimeoutMs: this.deps.getIdleTimeoutMs(),
      wakeupTimeoutMs: 30_000,
      onTimeout: (pendingTaskIds, elapsedMs) => {
        this.deps.messageHub.notify(
          t('dispatch.waitForWorkers.timeout', {
            seconds: Math.round(elapsedMs / 1000),
            pendingCount: pendingTaskIds.length,
          }),
          'warning',
        );
      },
      },
    );

    if (!waitResult.timed_out && waitResult.pending_task_ids.length === 0) {
      const auditOutcome = batch.getAuditOutcome();
      if (auditOutcome) {
        return {
          ...waitResult,
          audit: {
            level: auditOutcome.level,
            summary: auditOutcome.summary,
            issues: auditOutcome.issues.map(issue => ({
              task_id: issue.taskId,
              level: issue.level,
              dimension: issue.dimension,
              detail: issue.detail,
            })),
          },
        };
      }
    }

    return waitResult;
  }

  buildFallbackSummary(batch: DispatchBatch): string {
    const entries = batch.getEntries();
    const summary = batch.getSummary();
    const modifiedFiles = Array.from(new Set(entries.flatMap(entry => entry.result?.modifiedFiles || [])));

    const statusLabel = (status: DispatchStatus): string => {
      switch (status) {
        case 'completed': return t('dispatch.reactiveSummary.status.completed');
        case 'failed': return t('dispatch.reactiveSummary.status.failed');
        case 'skipped': return t('dispatch.reactiveSummary.status.skipped');
        case 'cancelled': return t('dispatch.reactiveSummary.status.cancelled');
        case 'running': return t('dispatch.reactiveSummary.status.running');
        case 'pending':
        case 'waiting_deps':
          return t('dispatch.reactiveSummary.status.waiting');
        default:
          return status;
      }
    };

    const taskLines = entries.map((entry, index) =>
      t('dispatch.reactiveSummary.taskLine', {
        index: index + 1,
        worker: entry.worker,
        task: entry.taskContract.taskTitle,
        status: statusLabel(entry.status),
        summary: entry.result?.summary || t('dispatch.reactiveSummary.noResultSummary'),
      })
    );

    const lines = [
      t('dispatch.reactiveSummary.header', {
        total: summary.total,
        completed: summary.completed,
        failed: summary.failed,
        skipped: summary.skipped,
        cancelled: summary.cancelled,
      }),
      ...taskLines,
      modifiedFiles.length > 0
        ? t('dispatch.reactiveSummary.modifiedFiles', { files: modifiedFiles.join('，') })
        : t('dispatch.reactiveSummary.modifiedFilesNone'),
    ];

    return lines.join('\n');
  }

  dispose(): void {
    this.completionQueue.reset();
    this.reactiveMode = false;
    this.batchesAwaitingSummary.clear();
  }
}
