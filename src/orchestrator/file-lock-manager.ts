/**
 * File Lock Manager
 * Manages exclusive locks for file-level task execution.
 */

import { EventEmitter } from 'events';

interface LockRequest {
  id: string;
  files: string[];
  priority: number;
  enqueuedAt: number;
  resolve: (release: () => void) => void;
  reject: (error: Error) => void;
  abortSignal?: AbortSignal;
  abortHandler?: () => void;
}

const DEFAULT_PRIORITY = 5;
const STARVATION_BOOST_MS = 15000;

export class FileLockManager extends EventEmitter {
  private locks: Map<string, string> = new Map();
  private queue: LockRequest[] = [];
  private requestCounter = 0;

  async acquire(
    files: string[],
    priority?: number,
    abortSignal?: AbortSignal
  ): Promise<() => void> {
    const normalized = this.normalizeFiles(files);

    if (normalized.length === 0) {
      return () => undefined;
    }

    if (abortSignal?.aborted) {
      throw new Error('任务已取消');
    }

    return new Promise((resolve, reject) => {
      const request: LockRequest = {
        id: String(++this.requestCounter),
        files: normalized,
        priority: priority ?? DEFAULT_PRIORITY,
        enqueuedAt: Date.now(),
        resolve,
        reject,
        abortSignal,
      };

      if (this.tryAcquire(request)) {
        resolve(() => this.release(request));
        return;
      }

      if (abortSignal) {
        const onAbort = () => {
          this.removeRequest(request.id);
          const reason = abortSignal.reason;
          reject(reason instanceof Error ? reason : new Error('任务已取消'));
        };
        request.abortHandler = onAbort;
        abortSignal.addEventListener('abort', onAbort, { once: true });
      }

      this.queue.push(request);
    });
  }

  canAcquire(files: string[]): boolean {
    const normalized = this.normalizeFiles(files);
    return normalized.every(file => !this.locks.has(file));
  }

  waitForRelease(): Promise<void> {
    return new Promise(resolve => this.once('lock_released', resolve));
  }

  private tryAcquire(request: LockRequest): boolean {
    for (const file of request.files) {
      if (this.locks.has(file)) {
        return false;
      }
    }

    for (const file of request.files) {
      this.locks.set(file, request.id);
    }

    return true;
  }

  private release(request: LockRequest): void {
    for (const file of request.files) {
      const owner = this.locks.get(file);
      if (owner === request.id) {
        this.locks.delete(file);
      }
    }

    this.emit('lock_released');
    this.processQueue();
  }

  private processQueue(): void {
    let progressed = true;

    while (progressed) {
      progressed = false;

      const index = this.findNextRequestIndex();
      if (index === -1) {
        return;
      }

      const request = this.queue.splice(index, 1)[0];

      if (request.abortSignal?.aborted) {
        const reason = request.abortSignal.reason;
        request.reject(reason instanceof Error ? reason : new Error('任务已取消'));
        progressed = true;
        continue;
      }

      if (!this.tryAcquire(request)) {
        this.queue.push(request);
        return;
      }

      if (request.abortHandler && request.abortSignal) {
        request.abortSignal.removeEventListener('abort', request.abortHandler);
      }

      request.resolve(() => this.release(request));
      progressed = true;
    }
  }

  private findNextRequestIndex(): number {
    if (this.queue.length === 0) {
      return -1;
    }

    const now = Date.now();
    let bestIndex = -1;
    let bestPriority = Number.POSITIVE_INFINITY;
    let bestEnqueuedAt = Number.POSITIVE_INFINITY;

    for (let i = 0; i < this.queue.length; i++) {
      const request = this.queue[i];
      if (!this.canAcquire(request.files)) {
        continue;
      }

      const effectivePriority = this.computeEffectivePriority(request, now);
      if (
        effectivePriority < bestPriority ||
        (effectivePriority === bestPriority && request.enqueuedAt < bestEnqueuedAt)
      ) {
        bestPriority = effectivePriority;
        bestEnqueuedAt = request.enqueuedAt;
        bestIndex = i;
      }
    }

    return bestIndex;
  }

  private computeEffectivePriority(request: LockRequest, now: number): number {
    const waitBoost = Math.floor((now - request.enqueuedAt) / STARVATION_BOOST_MS);
    return request.priority - waitBoost;
  }

  private removeRequest(requestId: string): void {
    const index = this.queue.findIndex(req => req.id === requestId);
    if (index !== -1) {
      this.queue.splice(index, 1);
    }
  }

  private normalizeFiles(files: string[]): string[] {
    return Array.from(new Set(files.filter(Boolean))).sort();
  }
}
