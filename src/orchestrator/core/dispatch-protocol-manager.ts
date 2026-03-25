import { logger, LogCategory } from '../../logging';
import type { WorkerSlot } from '../../types';

export type DispatchAckState = 'pending' | 'acked' | 'nacked';

export interface DispatchExecutionProtocolState {
  taskId: string;
  batchId: string;
  worker: WorkerSlot;
  dispatchAttemptId: string;
  idempotencyKey: string;
  leaseId: string;
  leaseExpireAt: number;
  heartbeatAt: number;
  ackState: DispatchAckState;
  createdAt: number;
  ackAt?: number;
  nackReason?: string;
  timeoutTriggered: boolean;
}

export interface DispatchProtocolTimeoutPayload {
  state: DispatchExecutionProtocolState;
  reasonCode: string;
}

export interface DispatchProtocolManagerDeps {
  ackTimeoutMs: number;
  leaseTtlMs: number;
  leaseWatchIntervalMs: number;
  onProtocolTimeout: (payload: DispatchProtocolTimeoutPayload) => void;
  touchBatchActivity?: (batchId: string) => void;
}

export class DispatchProtocolManager {
  private readonly states = new Map<string, DispatchExecutionProtocolState>();
  private leaseWatcherTimer?: NodeJS.Timeout;

  constructor(
    private readonly deps: DispatchProtocolManagerDeps,
  ) {
    this.startLeaseWatcher();
  }

  register(
    taskId: string,
    batchId: string | undefined,
    worker: WorkerSlot,
  ): DispatchExecutionProtocolState {
    const now = Date.now();
    const dispatchAttemptId = `dispatch-attempt-${taskId}-${now}-${Math.random().toString(36).slice(2, 8)}`;
    const state: DispatchExecutionProtocolState = {
      taskId,
      batchId: batchId || 'unknown-batch',
      worker,
      dispatchAttemptId,
      idempotencyKey: dispatchAttemptId,
      leaseId: `lease-${taskId}-${now}-${Math.random().toString(36).slice(2, 8)}`,
      leaseExpireAt: now + this.deps.leaseTtlMs,
      heartbeatAt: now,
      ackState: 'pending',
      createdAt: now,
      timeoutTriggered: false,
    };
    this.states.set(taskId, state);
    return state;
  }

  markAck(taskId: string, workerId?: WorkerSlot): void {
    const normalizedTaskId = typeof taskId === 'string' ? taskId.trim() : '';
    if (!normalizedTaskId) {
      return;
    }
    const state = this.states.get(normalizedTaskId);
    if (!state) {
      return;
    }
    if (workerId && state.worker !== workerId) {
      logger.warn('Dispatch.Protocol.ACK.Worker不一致', {
        taskId: normalizedTaskId,
        expectedWorker: state.worker,
        actualWorker: workerId,
      }, LogCategory.ORCHESTRATOR);
    }
    const now = Date.now();
    state.ackState = 'acked';
    state.ackAt = now;
    state.heartbeatAt = now;
    state.leaseExpireAt = now + this.deps.leaseTtlMs;
  }

  markNack(taskId: string, reason: string): void {
    const normalizedTaskId = typeof taskId === 'string' ? taskId.trim() : '';
    if (!normalizedTaskId) {
      return;
    }
    const state = this.states.get(normalizedTaskId);
    if (!state) {
      return;
    }
    state.ackState = 'nacked';
    state.nackReason = reason;
    state.leaseExpireAt = Date.now();
  }

  updateHeartbeat(taskId: string, workerId: WorkerSlot, timestamp: number): void {
    const normalizedTaskId = typeof taskId === 'string' ? taskId.trim() : '';
    if (!normalizedTaskId) {
      return;
    }
    const state = this.states.get(normalizedTaskId);
    if (!state) {
      return;
    }
    const hbAt = Number.isFinite(timestamp) ? Math.floor(timestamp) : Date.now();
    if (state.worker !== workerId) {
      logger.warn('Dispatch.Protocol.Heartbeat.Worker不一致', {
        taskId: normalizedTaskId,
        expectedWorker: state.worker,
        actualWorker: workerId,
      }, LogCategory.ORCHESTRATOR);
      return;
    }
    if (state.ackState === 'pending') {
      state.ackState = 'acked';
      state.ackAt = hbAt;
    }
    state.heartbeatAt = hbAt;
    state.leaseExpireAt = hbAt + this.deps.leaseTtlMs;
    if (typeof this.deps.touchBatchActivity === 'function') {
      this.deps.touchBatchActivity(state.batchId);
    }
  }

  clear(taskId: string): void {
    const normalizedTaskId = typeof taskId === 'string' ? taskId.trim() : '';
    if (!normalizedTaskId) {
      return;
    }
    this.states.delete(normalizedTaskId);
  }

  clearByBatch(batchId: string): void {
    for (const [taskId, state] of this.states.entries()) {
      if (state.batchId === batchId) {
        this.states.delete(taskId);
      }
    }
  }

  clearAll(): void {
    this.states.clear();
  }

  dispose(): void {
    this.stopLeaseWatcher();
    this.clearAll();
  }

  private startLeaseWatcher(): void {
    if (this.leaseWatcherTimer) {
      return;
    }
    this.leaseWatcherTimer = setInterval(() => {
      this.checkLeases();
    }, this.deps.leaseWatchIntervalMs);
  }

  private stopLeaseWatcher(): void {
    if (!this.leaseWatcherTimer) {
      return;
    }
    clearInterval(this.leaseWatcherTimer);
    this.leaseWatcherTimer = undefined;
  }

  private checkLeases(): void {
    if (this.states.size === 0) {
      return;
    }
    const now = Date.now();
    for (const state of this.states.values()) {
      if (state.timeoutTriggered) {
        continue;
      }
      if (state.ackState === 'pending' && now - state.createdAt > this.deps.ackTimeoutMs) {
        this.triggerTimeout(state, 'ack-timeout');
        continue;
      }
      if (state.ackState === 'nacked') {
        this.triggerTimeout(state, `nack:${state.nackReason || 'unknown'}`);
        continue;
      }
      if (state.ackState !== 'acked') {
        continue;
      }
      if (state.leaseExpireAt <= now) {
        this.triggerTimeout(state, 'lease-expired');
      }
    }
  }

  private triggerTimeout(state: DispatchExecutionProtocolState, reasonCode: string): void {
    if (state.timeoutTriggered) {
      return;
    }
    state.timeoutTriggered = true;
    this.deps.onProtocolTimeout({
      state: { ...state },
      reasonCode,
    });
    this.clear(state.taskId);
  }
}
