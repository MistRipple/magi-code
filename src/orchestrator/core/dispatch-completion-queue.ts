import { logger, LogCategory } from '../../logging';
import type { WorkerCompletionResult, WaitForWorkersResult } from '../../tools/orchestration-executor';
import type { DispatchBatch, DispatchEntry, DispatchStatus } from './dispatch-batch';
import { isTerminalStatus } from './dispatch-batch';

interface WaitForWorkersOptions {
  waitTimeoutMs: number;
  wakeupTimeoutMs: number;
  onTimeout?: (pendingTaskIds: string[], elapsedMs: number) => void;
}

export class DispatchCompletionQueue {
  private completionQueue: WorkerCompletionResult[] = [];
  private completionResolvers: Array<() => void> = [];
  private pushedTaskIds: Set<string> = new Set();

  reset(): void {
    this.completionQueue = [];
    this.completionResolvers = [];
    this.pushedTaskIds.clear();
  }

  push(entry: DispatchEntry): void {
    if (this.pushedTaskIds.has(entry.taskId)) {
      return;
    }
    this.pushedTaskIds.add(entry.taskId);

    const result: WorkerCompletionResult = {
      task_id: entry.taskId,
      worker: entry.worker,
      status: entry.status as WorkerCompletionResult['status'],
      summary: entry.result?.summary || '',
      modified_files: entry.result?.modifiedFiles || [],
      errors: entry.result?.errors,
    };

    this.completionQueue.push(result);
    const resolvers = this.completionResolvers.splice(0);
    for (const resolve of resolvers) {
      resolve();
    }
  }

  async waitFor(
    batch: DispatchBatch,
    taskIds: string[] | undefined,
    options: WaitForWorkersOptions,
  ): Promise<WaitForWorkersResult> {
    const waitStartedAt = Date.now();
    const targetIds = taskIds && taskIds.length > 0 ? new Set(taskIds) : null;

    if (this.isTargetSatisfied(batch, targetIds)) {
      return {
        results: this.drainCompletionResults(targetIds, batch),
        wait_status: 'completed',
        timed_out: false,
        pending_task_ids: [],
        waited_ms: Date.now() - waitStartedAt,
      };
    }

    let timedOut = false;
    while (!this.isTargetSatisfied(batch, targetIds)) {
      const elapsed = Date.now() - waitStartedAt;
      if (elapsed > options.waitTimeoutMs) {
        timedOut = true;
        const pendingTaskIds = this.getPendingTargetTaskIds(batch, targetIds);
        logger.warn('waitForWorkers.超时', {
          elapsed,
          targetIds: targetIds ? Array.from(targetIds) : 'all',
          pendingTaskIds,
        }, LogCategory.ORCHESTRATOR);
        options.onTimeout?.(pendingTaskIds, elapsed);
        break;
      }

      await this.waitForSignal(options.wakeupTimeoutMs);
    }

    const pendingTaskIds = this.getPendingTargetTaskIds(batch, targetIds);
    return {
      results: this.drainCompletionResults(targetIds, batch),
      wait_status: timedOut ? 'timeout' : 'completed',
      timed_out: timedOut,
      pending_task_ids: pendingTaskIds,
      waited_ms: Date.now() - waitStartedAt,
    };
  }

  private waitForSignal(timeoutMs: number): Promise<void> {
    return new Promise<void>(resolve => {
      this.completionResolvers.push(resolve);
      setTimeout(resolve, timeoutMs);
    });
  }

  private isTargetSatisfied(batch: DispatchBatch, targetIds: Set<string> | null): boolean {
    if (!targetIds) {
      return batch.isAllCompleted();
    }

    return Array.from(targetIds).every(id => {
      const entry = batch.getEntry(id);
      return !!entry && isTerminalStatus(entry.status);
    });
  }

  private getPendingTargetTaskIds(batch: DispatchBatch, targetIds: Set<string> | null): string[] {
    if (!targetIds) {
      return batch.getEntries()
        .filter(entry => !isTerminalStatus(entry.status))
        .map(entry => entry.taskId);
    }

    const pending: string[] = [];
    for (const taskId of targetIds) {
      const entry = batch.getEntry(taskId);
      if (!entry || !isTerminalStatus(entry.status)) {
        pending.push(taskId);
      }
    }
    return pending;
  }

  private drainCompletionResults(targetIds: Set<string> | null, batch?: DispatchBatch): WorkerCompletionResult[] {
    if (!targetIds) {
      return this.completionQueue.splice(0);
    }

    const matched: WorkerCompletionResult[] = [];
    const remaining: WorkerCompletionResult[] = [];

    for (const result of this.completionQueue) {
      if (targetIds.has(result.task_id)) {
        matched.push(result);
      } else {
        remaining.push(result);
      }
    }

    if (batch) {
      const matchedIds = new Set(matched.map(item => item.task_id));
      for (const taskId of targetIds) {
        if (matchedIds.has(taskId)) {
          continue;
        }

        const entry = batch.getEntry(taskId);
        if (!entry || !isTerminalStatus(entry.status)) {
          continue;
        }

        matched.push({
          task_id: entry.taskId,
          worker: entry.worker,
          status: entry.status as WorkerCompletionResult['status'],
          summary: entry.result?.summary || '',
          modified_files: entry.result?.modifiedFiles || [],
          errors: entry.result?.errors,
        });
      }
    }

    this.completionQueue = remaining;
    return matched;
  }
}
