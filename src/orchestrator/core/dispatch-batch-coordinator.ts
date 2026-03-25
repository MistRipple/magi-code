import { logger, LogCategory } from '../../logging';
import { t } from '../../i18n';
import { MessageType } from '../../protocol/message-protocol';
import type { WorkerSlot } from '../../types';
import type { MessageHub } from './message-hub';
import {
  DispatchBatch,
  isTerminalStatus,
  type DispatchEntry,
  type DispatchStatus,
  type DispatchAuditOutcome,
} from './dispatch-batch';

export interface DispatchBatchCoordinatorDeps {
  messageHub: MessageHub;
  emitWorkerLaneInstructionCard: (
    entry: DispatchEntry,
    worker: WorkerSlot,
    batch: DispatchBatch,
    preferredTaskId?: string,
  ) => void;
  scheduleReadyTasks: (
    batch: DispatchBatch,
    options?: { immediate?: boolean; reason?: string },
  ) => void;
  clearProtocolState: (taskId: string) => void;
  clearProtocolStatesByBatch: (batchId: string) => void;
  clearActiveWorkerLanes: () => void;
  clearDispatchScheduleTimers: (batchId?: string) => void;
  clearResumeContext: () => void;
  updateDispatchStatus: (taskId: string, status: 'completed' | 'failed' | 'cancelled') => void;
  pushCompletionEntry: (entry: DispatchEntry) => void;
  isReactiveMode: () => boolean;
  markReactiveBatchAwaitingSummary: (batchId: string) => void;
  clearReactiveBatchAwaitingSummary: (batchId: string) => void;
  clearPhaseBPlusTimestamp: (batchId: string) => void;
  ensureBatchAuditOutcome: (
    batch: DispatchBatch,
    entries: DispatchEntry[],
  ) => DispatchAuditOutcome;
  buildInterventionReport: (
    auditOutcome: DispatchAuditOutcome,
    entries: DispatchEntry[],
  ) => string;
  triggerPhaseCSummary: (
    batch: DispatchBatch,
    entries: DispatchEntry[],
    auditOutcome?: DispatchAuditOutcome,
  ) => Promise<void>;
}

export class DispatchBatchCoordinator {
  constructor(
    private readonly deps: DispatchBatchCoordinatorDeps,
  ) {}

  setupBatchEventHandlers(batch: DispatchBatch): void {
    batch.on('task:ready', (taskId: string, entry: DispatchEntry) => {
      this.deps.emitWorkerLaneInstructionCard(entry, entry.worker, batch, taskId);
      this.deps.scheduleReadyTasks(batch, { reason: 'task-ready' });
    });

    batch.on('task:statusChanged', (_taskId: string, status: DispatchStatus) => {
      const entry = batch.getEntry(_taskId);
      if (entry) {
        this.deps.emitWorkerLaneInstructionCard(entry, entry.worker, batch, _taskId);
      }
      if (isTerminalStatus(status)) {
        this.deps.clearProtocolState(_taskId);
        const mappedStatus = status === 'completed'
          ? 'completed'
          : status === 'failed'
            ? 'failed'
            : 'cancelled';
        this.deps.updateDispatchStatus(_taskId, mappedStatus);
        this.deps.scheduleReadyTasks(batch, { reason: 'task-terminal' });

        const completedEntry = batch.getEntry(_taskId);
        if (completedEntry) {
          this.deps.pushCompletionEntry(completedEntry);
        }
      }
    });

    batch.on('batch:allCompleted', (batchId: string, entries: DispatchEntry[]) => {
      const summary = batch.getSummary();
      logger.info('DispatchBatch.全部完成', { batchId, ...summary, reactiveMode: this.deps.isReactiveMode() }, LogCategory.ORCHESTRATOR);
      const auditOutcome = this.deps.ensureBatchAuditOutcome(batch, entries);

      if (this.deps.isReactiveMode()) {
        this.deps.markReactiveBatchAwaitingSummary(batchId);
        if (auditOutcome.level === 'intervention') {
          const blockedReport = this.deps.buildInterventionReport(auditOutcome, entries);
          this.deps.messageHub.notify(t('dispatch.audit.reactiveInterventionBlocked'), 'error');
          this.deps.messageHub.orchestratorMessage(blockedReport, { type: MessageType.RESULT });
          logger.warn('Reactive Phase 审计阻断交付', {
            batchId,
            auditOutcome,
          }, LogCategory.ORCHESTRATOR);
        }
        batch.archive();
      } else {
        this.deps.clearReactiveBatchAwaitingSummary(batchId);
        void this.deps.triggerPhaseCSummary(batch, entries, auditOutcome);
      }
    });

    batch.on('batch:cancelled', (batchId: string, reason: string) => {
      this.deps.clearReactiveBatchAwaitingSummary(batchId);
      this.deps.clearPhaseBPlusTimestamp(batchId);
      this.deps.clearProtocolStatesByBatch(batchId);
      this.deps.clearActiveWorkerLanes();
      this.deps.clearDispatchScheduleTimers(batchId);
      logger.info('DispatchBatch.已取消', { batchId, reason }, LogCategory.ORCHESTRATOR);
      this.deps.messageHub.orchestratorMessage(t('dispatch.notify.taskCancelledWithReason', { reason }));
    });

    batch.on('phase:changed', (_batchId: string, phase) => {
      if (phase === 'archived') {
        this.deps.clearPhaseBPlusTimestamp(batch.id);
        this.deps.clearProtocolStatesByBatch(batch.id);
        this.deps.clearActiveWorkerLanes();
        this.deps.clearDispatchScheduleTimers(batch.id);
        this.deps.clearResumeContext();
      }
    });
  }
}
