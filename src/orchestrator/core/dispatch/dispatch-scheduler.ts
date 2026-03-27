import { logger, LogCategory } from '../../../logging';
import type { WorkerSlot } from '../../../types';
import { DispatchBatch, type DispatchEntry } from './dispatch-batch';

export interface DispatchExecutionWorkerResolution {
  ok: boolean;
  selectedWorker?: WorkerSlot;
  degraded?: boolean;
  routingReason?: string;
  error?: string;
}

export interface DispatchSchedulerDeps {
  coalesceMs: number;
  getWorkerLaneResidentTimeoutMs: () => number;
  getWorkerLaneResidentPollIntervalMs: () => number;
  getActiveWorkerLanes: () => Iterable<WorkerSlot>;
  tryActivateWorkerLane: (worker: WorkerSlot) => boolean;
  releaseWorkerLane: (worker: WorkerSlot) => void;
  resolveExecutionWorker: (
    preferredWorker: WorkerSlot,
    options?: {
      busyWorkers?: Set<WorkerSlot>;
      excludedWorkers?: Set<WorkerSlot>;
      allowBusyFallback?: boolean;
    },
  ) => DispatchExecutionWorkerResolution;
  executeDispatchEntry: (
    entry: DispatchEntry,
    options?: { emitWorkerInstruction?: boolean },
  ) => Promise<void>;
  emitWorkerLaneInstructionCard: (
    entry: DispatchEntry,
    worker: WorkerSlot,
    batch: DispatchBatch,
    preferredTaskId?: string,
  ) => void;
  notifyWorkerRoutingAdjusted: (payload: {
    batchId: string;
    taskId: string;
    fromWorker: WorkerSlot;
    toWorker: WorkerSlot;
    routingReason: string;
  }) => void;
}

export class DispatchScheduler {
  private readonly dispatchScheduleTimers = new Map<string, NodeJS.Timeout>();

  constructor(
    private readonly deps: DispatchSchedulerDeps,
  ) {}

  scheduleReadyTasks(
    batch: DispatchBatch,
    options?: { immediate?: boolean; reason?: string },
  ): void {
    if (batch.status !== 'active') {
      return;
    }

    const existing = this.dispatchScheduleTimers.get(batch.id);
    if (existing) {
      clearTimeout(existing);
      this.dispatchScheduleTimers.delete(batch.id);
    }

    const delay = options?.immediate ? 0 : this.deps.coalesceMs;
    const timer = setTimeout(() => {
      this.dispatchScheduleTimers.delete(batch.id);
      if (batch.status !== 'active') {
        return;
      }
      this.dispatchReadyTasksWithIsolation(batch);
    }, delay);

    this.dispatchScheduleTimers.set(batch.id, timer);
  }

  clearScheduleTimers(batchId?: string): void {
    if (batchId) {
      const timer = this.dispatchScheduleTimers.get(batchId);
      if (timer) {
        clearTimeout(timer);
        this.dispatchScheduleTimers.delete(batchId);
      }
      return;
    }

    for (const timer of this.dispatchScheduleTimers.values()) {
      clearTimeout(timer);
    }
    this.dispatchScheduleTimers.clear();
  }

  dispose(): void {
    this.clearScheduleTimers();
  }

  private dispatchReadyTasksWithIsolation(batch: DispatchBatch): void {
    if (batch.status !== 'active') {
      return;
    }

    const readyTasks = batch.getReadyTasks();
    if (readyTasks.length === 0) {
      return;
    }

    const busyWorkers = new Set<WorkerSlot>(this.deps.getActiveWorkerLanes());
    const selectedWorkers = new Set<WorkerSlot>();

    for (const entry of readyTasks) {
      const routing = this.deps.resolveExecutionWorker(entry.worker, {
        busyWorkers,
        // 忙碌时默认等待 owner worker，不因并行度压力打破分工 ownership。
        allowBusyFallback: false,
      });
      if (!routing.ok || !routing.selectedWorker) {
        logger.debug('Dispatch.WorkerLane.就绪任务暂不可执行', {
          batchId: batch.id,
          taskId: entry.taskId,
          worker: entry.worker,
          reason: routing.error,
        }, LogCategory.ORCHESTRATOR);
        continue;
      }

      const selectedWorker = routing.selectedWorker;
      if (busyWorkers.has(selectedWorker) || selectedWorkers.has(selectedWorker)) {
        continue;
      }

      if (selectedWorker !== entry.worker) {
        const previousWorker = entry.worker;
        entry.worker = selectedWorker;
        this.deps.notifyWorkerRoutingAdjusted({
          batchId: batch.id,
          taskId: entry.taskId,
          fromWorker: previousWorker,
          toWorker: selectedWorker,
          routingReason: routing.routingReason || '',
        });
      }

      selectedWorkers.add(selectedWorker);
      busyWorkers.add(selectedWorker);
    }

    for (const worker of selectedWorkers) {
      this.launchWorkerLane(batch, worker);
    }
  }

  private launchWorkerLane(batch: DispatchBatch, worker: WorkerSlot): void {
    if (!this.deps.tryActivateWorkerLane(worker)) {
      return;
    }

    const residentTimeoutMs = this.deps.getWorkerLaneResidentTimeoutMs();
    logger.info('DispatchBatch.WorkerLane.启动', {
      batchId: batch.id,
      worker,
      residentTimeoutMs,
    }, LogCategory.ORCHESTRATOR);

    void (async () => {
      let executedCount = 0;
      let idlePollRounds = 0;
      let residentDeadlineAt = Date.now() + residentTimeoutMs;
      try {
        while (batch.status === 'active') {
          const nextEntry = this.getNextReadyTaskForWorker(batch, worker);
          if (nextEntry) {
            executedCount += 1;
            residentDeadlineAt = Date.now() + residentTimeoutMs;
            await this.deps.executeDispatchEntry(nextEntry, { emitWorkerInstruction: true });
            continue;
          }
          if (residentTimeoutMs <= 0) {
            break;
          }
          const keepResident = await this.waitForReadyTaskWhileResident(batch, worker, residentDeadlineAt);
          if (!keepResident) {
            break;
          }
          idlePollRounds += 1;
        }
      } finally {
        this.deps.releaseWorkerLane(worker);
        logger.info('DispatchBatch.WorkerLane.结束', {
          batchId: batch.id,
          worker,
          executedCount,
          idlePollRounds,
          residentTimeoutMs,
          batchStatus: batch.status,
        }, LogCategory.ORCHESTRATOR);

        if (batch.status === 'active') {
          this.scheduleReadyTasks(batch, { immediate: true, reason: 'lane-finished' });
        }
      }
    })();
  }

  private getNextReadyTaskForWorker(batch: DispatchBatch, worker: WorkerSlot): DispatchEntry | null {
    const ready = batch.getReadyTasks();
    for (const entry of ready) {
      if (entry.worker === worker) {
        return entry;
      }
    }
    return null;
  }

  private async waitForReadyTaskWhileResident(
    batch: DispatchBatch,
    worker: WorkerSlot,
    residentDeadlineAt: number,
  ): Promise<boolean> {
    const pollIntervalMs = this.deps.getWorkerLaneResidentPollIntervalMs();
    while (batch.status === 'active') {
      if (this.getNextReadyTaskForWorker(batch, worker)) {
        return true;
      }
      const remainingMs = residentDeadlineAt - Date.now();
      if (remainingMs <= 0) {
        return false;
      }
      await new Promise((resolve) => setTimeout(resolve, Math.min(pollIntervalMs, remainingMs)));
    }
    return false;
  }
}
